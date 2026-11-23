//! From the probe's [`FrameStats`] to a [`Value::Frames`]
//! (docs/design/bench.md「进程级基准」帧时间): the two quantities the frame
//! budgets judge, as pure functions.
//!
//! - The work of a frame is `completed - requested`: from the frame source's
//!   callback to the end of the GPU work, so the main thread's blocking
//!   before layout, layout, painting and the GPU all count.
//! - The interval is the time between the `started` of two consecutive
//!   frames of the recording; the judge divides it by the refresh period.
//!
//! A scenario is recorded in one launch; a benchmark with several scenarios
//! and launches judges the worst frame of all of them, so the samples are
//! put together and [`describe`] gives each recording's own maximum for the
//! result's note.

use std::time::Duration;

use crate::wire::{FrameStats, FrameTimes, SCROLL_EVENTS, TimeSource};
use crate::{BenchError, Samples, Value};

/// The work of each frame, in order: `completed - requested`. A frame whose
/// `completed` is before its `requested` (clock steps between two readings
/// of the system clock) counts as 0.
pub fn work(stats: &FrameStats) -> Vec<Duration> {
    stats
        .frames
        .iter()
        .map(|frame| gap(frame.completed, frame.requested))
        .collect()
}

/// The time between the `started` of each frame and the next one's: one
/// fewer than the frames.
pub fn intervals(stats: &FrameStats) -> Vec<Duration> {
    stats
        .frames
        .windows(2)
        .map(|pair| gap(pair[1].started, pair[0].started))
        .collect()
}

fn gap(later: std::time::SystemTime, earlier: std::time::SystemTime) -> Duration {
    later.duration_since(earlier).unwrap_or_default()
}

/// The fewest frames of a scroll recording that count as a scroll: each of
/// the [`SCROLL_EVENTS`] wheel events draws a frame when it moves the
/// content, so a recording well short of that scrolled nothing (a view the
/// events did not reach, a file shorter than the scroll).
pub const MIN_SCROLL_FRAMES: usize = SCROLL_EVENTS as usize * 3 / 4;

/// The recording can be judged: at least `min_frames` frames (a scroll that
/// reached nothing draws nothing, and its few frames would pass) and a
/// refresh period above 0.
pub fn check(stats: &FrameStats, min_frames: usize) -> Result<(), BenchError> {
    if stats.refresh.is_zero() {
        return Err(BenchError::new(format!(
            "帧记录 {}：刷新周期为 0",
            stats.scenario
        )));
    }
    // Two frames give one interval.
    let min = min_frames.max(2);
    if stats.frames.len() < min {
        return Err(BenchError::new(format!(
            "帧记录 {}：只有 {} 帧，少于 {min} 帧：场景没有画出东西",
            stats.scenario,
            stats.frames.len()
        )));
    }
    Ok(())
}

/// The frames of several recordings (scenarios and launches) as one value:
/// each frame's work and each interval inside a recording, so no interval
/// spans two launches. Every recording must pass [`check`] and have the
/// same refresh period.
pub fn value(recordings: &[FrameStats], min_frames: usize) -> Result<Value, BenchError> {
    let first = recordings
        .first()
        .ok_or_else(|| BenchError::new("没有帧记录"))?;
    let mut work_samples = Vec::new();
    let mut interval_samples = Vec::new();
    for stats in recordings {
        check(stats, min_frames)?;
        if stats.refresh != first.refresh {
            return Err(BenchError::new(format!(
                "帧记录 {} 的刷新周期 {:?} 与 {} 的 {:?} 不同",
                stats.scenario, stats.refresh, first.scenario, first.refresh
            )));
        }
        work_samples.extend(work(stats));
        interval_samples.extend(intervals(stats));
    }
    Ok(Value::Frames {
        work: Samples(work_samples),
        intervals: Samples(interval_samples),
        refresh: first.refresh,
    })
}

/// The frames of the recordings (one scenario's launches, say), for a note:
/// the count, the worst work and interval, the frames the display link did
/// not start, and where the `presented` times came from (reported, not
/// judged; ADR 0052).
pub fn describe(recordings: &[FrameStats]) -> String {
    let worst_work = recordings.iter().flat_map(work).max().unwrap_or_default();
    let (worst_interval, refresh) = recordings
        .iter()
        .flat_map(|stats| intervals(stats).into_iter().map(|i| (i, stats.refresh)))
        .max_by(|a, b| {
            // Compared in refresh periods, as the budget judges them.
            let periods = |(interval, refresh): &(Duration, Duration)| {
                interval.as_secs_f64() / refresh.as_secs_f64().max(f64::MIN_POSITIVE)
            };
            periods(a).total_cmp(&periods(b))
        })
        .unwrap_or_default();
    let periods = worst_interval.as_secs_f64() / refresh.as_secs_f64().max(f64::MIN_POSITIVE);
    let frames: Vec<FrameTimes> = recordings
        .iter()
        .flat_map(|stats| stats.frames.iter().copied())
        .collect();
    format!(
        "{} frames in {} recording(s), max work {:.2} ms, max interval {periods:.2} refresh periods, without tick {}, presented {}, {}",
        frames.len(),
        recordings.len(),
        worst_work.as_secs_f64() * 1e3,
        recordings
            .iter()
            .map(|stats| stats.without_tick)
            .sum::<u32>(),
        sources(&frames),
        stages(&frames)
    )
}

/// The work split at its moments, for a note: the wait from the vsync tick
/// to the frame's start (the main thread busy with something else), then
/// the frame's CPU work to the `commit`, the wait from the `commit` to the
/// GPU's start, and the GPU's run, each as median / p95 / max in ms. A stage
/// counts only the frames that have both of its moments (a missing `commit`
/// or GPU start time leaves the frame out of that stage, not out of the
/// others); a stage no frame has is `-`. A diagnostic only: the budgets
/// judge `completed - requested`.
fn stages(frames: &[FrameTimes]) -> String {
    let spread = |mut values: Vec<Duration>| {
        if values.is_empty() {
            return "-".to_owned();
        }
        values.sort_unstable();
        let at = |q: f64| {
            values
                .get(((values.len() as f64 - 1.0) * q).round() as usize)
                .map_or(0.0, |d| d.as_secs_f64() * 1e3)
        };
        format!("{:.2}/{:.2}/{:.2}", at(0.5), at(0.95), at(1.0))
    };
    let queue = frames.iter().map(|f| gap(f.started, f.requested)).collect();
    let to_commit = frames
        .iter()
        .filter_map(|f| Some(gap(f.committed?, f.started)))
        .collect();
    let to_gpu = frames
        .iter()
        .filter_map(|f| Some(gap(f.gpu_started?, f.committed?)))
        .collect();
    let on_gpu = frames
        .iter()
        .filter_map(|f| Some(gap(f.completed, f.gpu_started?)))
        .collect();
    format!(
        "tick to start {} ms, start to commit {} ms, commit to GPU start {} ms, GPU start to end {} ms (median/p95/max)",
        spread(queue),
        spread(to_commit),
        spread(to_gpu),
        spread(on_gpu)
    )
}

/// `presented`, `handler` and no time, as counts.
fn sources(frames: &[FrameTimes]) -> String {
    let count = |source: TimeSource| {
        frames
            .iter()
            .filter(|f| f.presented.is_some_and(|p| p.source == source))
            .count()
    };
    format!(
        "{}/{}/{} (presented/handler/none)",
        count(TimeSource::Presented),
        count(TimeSource::Handler),
        frames.iter().filter(|f| f.presented.is_none()).count()
    )
}

#[cfg(test)]
mod tests;
