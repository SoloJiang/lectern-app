use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::wire::PresentedAt;

fn at(us: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_micros(us)
}

/// A frame requested, started and completed at the given microseconds.
fn frame(requested: u64, started: u64, completed: u64) -> FrameTimes {
    FrameTimes {
        requested: at(requested),
        started: at(started),
        completed: at(completed),
        presented: None,
    }
}

fn stats(scenario: &str, frames: Vec<FrameTimes>) -> FrameStats {
    FrameStats {
        scenario: scenario.into(),
        refresh: Duration::from_micros(8_333),
        frames,
        without_tick: 0,
    }
}

#[test]
fn frames_work_is_completed_minus_requested() {
    let s = stats(
        "scroll",
        vec![frame(0, 100, 3_000), frame(8_333, 8_500, 20_000)],
    );
    assert_eq!(
        work(&s),
        [Duration::from_micros(3_000), Duration::from_micros(11_667)]
    );
}

#[test]
fn frames_intervals_are_between_consecutive_starts() {
    let s = stats(
        "scroll",
        vec![
            frame(0, 100, 3_000),
            frame(8_333, 8_433, 11_000),
            frame(25_000, 25_100, 27_000),
        ],
    );
    assert_eq!(
        intervals(&s),
        [Duration::from_micros(8_333), Duration::from_micros(16_667)]
    );
    // One frame gives none.
    assert!(intervals(&stats("scroll", vec![frame(0, 1, 2)])).is_empty());
}

#[test]
fn frames_a_completion_before_the_request_counts_as_zero() {
    let s = stats("scroll", vec![frame(500, 600, 400), frame(900, 500, 1_000)]);
    assert_eq!(work(&s)[0], Duration::ZERO);
    assert_eq!(intervals(&s), [Duration::ZERO]);
}

#[test]
fn frames_value_puts_the_recordings_samples_together_without_a_joint_interval() {
    let a = stats("a", vec![frame(0, 10, 100), frame(1_000, 1_010, 1_100)]);
    let b = stats(
        "b",
        vec![frame(50_000, 50_010, 50_200), frame(60_000, 60_010, 60_100)],
    );
    let Value::Frames {
        work,
        intervals,
        refresh,
    } = value(&[a, b], 2).unwrap()
    else {
        panic!("frames");
    };
    assert_eq!(work.0.len(), 4);
    // 2 recordings of 2 frames: one interval each, none from `a` to `b`.
    assert_eq!(
        intervals.0,
        [Duration::from_micros(1_000), Duration::from_micros(10_000)]
    );
    assert_eq!(refresh, Duration::from_micros(8_333));
}

#[test]
fn frames_a_recording_with_too_few_frames_is_an_error() {
    let few = stats("scroll", vec![frame(0, 1, 2), frame(3, 4, 5)]);
    let error = value(&[few], 10).unwrap_err();
    assert!(error.message().contains("2 帧"), "{}", error.message());
    // An empty recording is an error even with no minimum, and so is one
    // frame: it has no interval to judge.
    assert!(value(&[stats("scroll", Vec::new())], 0).is_err());
    assert!(value(&[stats("scroll", vec![frame(0, 1, 2)])], 0).is_err());
    assert!(value(&[], 0).is_err());
}

#[test]
fn frames_recordings_with_different_refresh_periods_are_an_error() {
    let a = stats("a", vec![frame(0, 1, 2), frame(3, 4, 5)]);
    let mut b = stats("b", vec![frame(0, 1, 2), frame(3, 4, 5)]);
    b.refresh = Duration::from_micros(16_667);
    assert!(value(&[a, b], 2).is_err());
}

#[test]
fn frames_a_zero_refresh_period_is_an_error() {
    let mut s = stats("scroll", vec![frame(0, 1, 2), frame(3, 4, 5)]);
    s.refresh = Duration::ZERO;
    assert!(check(&s, 2).is_err());
}

#[test]
fn frames_describe_gives_the_worst_frame_and_the_sources() {
    let mut s = stats(
        "scroll",
        vec![
            frame(0, 100, 3_000),
            frame(8_333, 8_433, 11_000),
            frame(40_000, 40_100, 41_000),
        ],
    );
    s.frames[0].presented = Some(PresentedAt {
        time: at(5_000),
        source: TimeSource::Presented,
    });
    s.frames[1].presented = Some(PresentedAt {
        time: at(12_000),
        source: TimeSource::Handler,
    });
    s.without_tick = 1;
    let text = describe(std::slice::from_ref(&s));
    assert!(
        text.starts_with("3 frames in 1 recording(s), max work 3.00 ms"),
        "{text}"
    );
    // 31667 µs between the last two starts, over an 8333 µs period.
    assert!(text.contains("max interval 3.80 refresh periods"), "{text}");
    assert!(text.contains("without tick 1"), "{text}");
    assert!(text.contains("1/1/1 (presented/handler/none)"), "{text}");
    // 100 µs from each tick to the start; 2900, 2567 and 900 µs after it.
    assert!(
        text.contains("tick to start 0.10/0.10/0.10 ms, start to GPU end 2.57/2.90/2.90 ms"),
        "{text}"
    );
}

#[test]
fn frames_describe_several_recordings_takes_the_worst_of_them() {
    let a = stats("a", vec![frame(0, 100, 3_000), frame(8_333, 8_433, 9_000)]);
    // A slower display: its interval is fewer refresh periods.
    let mut b = stats(
        "b",
        vec![frame(0, 100, 1_000), frame(30_000, 30_100, 31_000)],
    );
    b.refresh = Duration::from_micros(16_667);
    let text = describe(&[a, b]);
    assert!(
        text.starts_with("4 frames in 2 recording(s), max work 3.00 ms"),
        "{text}"
    );
    // 8333 µs of 8333 µs is 1.00 period; 30000 µs of 16667 µs is 1.80.
    assert!(text.contains("max interval 1.80 refresh periods"), "{text}");
}

#[test]
fn frames_a_scroll_needs_three_quarters_of_its_events_drawn() {
    assert_eq!(MIN_SCROLL_FRAMES, 180);
}
