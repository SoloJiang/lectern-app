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

/// The main thread's part of each frame's work, in order: from the start
/// to the call of `nextDrawable`, plus from the drawable to the `commit`.
/// It leaves out the wait from the tick to the start, the wait for the
/// drawable and the GPU. None for a frame without one of the four times.
pub fn cpu(stats: &FrameStats) -> Vec<Option<Duration>> {
    stats
        .frames
        .iter()
        .map(|f| Some(gap(f.drawable_asked?, f.started) + gap(f.committed?, f.drawable_got?)))
        .collect()
}

/// The [`cpu`] parts of the recordings (one scenario's launches) as time
/// samples: what a runner that is not the judging machine judges, by the
/// p95 (ADR 0067). A frame without one of the four moments is an error: a
/// record with a moment missing cannot be judged.
pub fn main_thread(recordings: &[FrameStats]) -> Result<Value, BenchError> {
    let mut samples = Vec::new();
    for stats in recordings {
        for (index, part) in cpu(stats).into_iter().enumerate() {
            samples.push(part.ok_or_else(|| {
                BenchError::new(format!(
                    "帧记录 {}：第 {index} 帧缺少 drawable 或 commit 的时刻，主线程部分算不出来",
                    stats.scenario
                ))
            })?);
        }
    }
    if samples.is_empty() {
        return Err(BenchError::new("没有帧记录"));
    }
    Ok(Value::Time(Samples(samples)))
}

/// The frame budget's time (ADR 0017 via tech-research.md): 8.3 ms.
const FRAME_BUDGET: Duration = Duration::from_micros(8300);

/// The [`cpu`] parts for a note: median / p95 / p99 / max over all the
/// recordings, then per recording the count of frames above 8.3 ms with
/// each one as `ms@index` (the first [`LISTED`]; a cost of the product
/// comes back at the same index in each recording, a busy machine does
/// not); `-` for a recording without any CPU part.
fn cpu_note(recordings: &[FrameStats]) -> String {
    /// The frames above the budget one recording lists.
    const LISTED: usize = 8;
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let mut all: Vec<Duration> = recordings.iter().flat_map(cpu).flatten().collect();
    all.sort_unstable();
    let at = |q: f64| {
        all.get(((all.len() as f64 - 1.0) * q).round() as usize)
            .map_or(0.0, |d| ms(*d))
    };
    let spread = if all.is_empty() {
        "-".to_owned()
    } else {
        format!(
            "{:.2}/{:.2}/{:.2}/{:.2}",
            at(0.5),
            at(0.95),
            at(0.99),
            at(1.0)
        )
    };
    let per = recordings
        .iter()
        .map(|stats| {
            let parts = cpu(stats);
            if parts.iter().all(Option::is_none) {
                return "-".to_owned();
            }
            let over: Vec<String> = parts
                .iter()
                .enumerate()
                .filter_map(|(i, d)| Some((i, (*d)?)))
                .filter(|(_, d)| *d > FRAME_BUDGET)
                .map(|(i, d)| format!("{:.2}@{i}", ms(d)))
                .collect();
            let more = if over.len() > LISTED { ", ..." } else { "" };
            match over.len() {
                0 => "0".to_owned(),
                n => format!(
                    "{n} ({}{more})",
                    over.iter()
                        .take(LISTED)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "CPU part {spread} ms (median/p95/p99/max), above 8.3 ms per recording (ms@frame) {per}"
    )
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
        "{} frames in {} recording(s), max work {:.2} ms, max interval {periods:.2} refresh periods, without tick {}, presented {}, {}, {}",
        frames.len(),
        recordings.len(),
        worst_work.as_secs_f64() * 1e3,
        recordings
            .iter()
            .map(|stats| stats.without_tick)
            .sum::<u32>(),
        sources(&frames),
        stages(&frames),
        cpu_note(recordings)
    )
}

/// The work split at its moments, for a note: the wait from the vsync tick
/// to the frame's start (the main thread busy with something else), the CPU
/// work from the start to the call of `nextDrawable`, the wait for the
/// drawable, the encoding from the drawable to the `commit`, the wait from
/// the `commit` to the GPU's start, and the GPU's run, each as median / p95
/// / max in ms. A stage counts only the frames that have both of its moments
/// (a missing time leaves the frame out of the stages that need it, not out
/// of the others); a stage no frame has is `-`. A diagnostic only: the
/// budgets judge `completed - requested`.
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
    let to_asked = frames
        .iter()
        .filter_map(|f| Some(gap(f.drawable_asked?, f.started)))
        .collect();
    let drawable_wait = frames
        .iter()
        .filter_map(|f| Some(gap(f.drawable_got?, f.drawable_asked?)))
        .collect();
    let to_commit = frames
        .iter()
        .filter_map(|f| Some(gap(f.committed?, f.drawable_got?)))
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
        "tick to start {} ms, start to drawable asked {} ms, drawable wait {} ms, drawable to commit {} ms, commit to GPU start {} ms, GPU start to end {} ms (median/p95/max)",
        spread(queue),
        spread(to_asked),
        spread(drawable_wait),
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
