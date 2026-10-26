//! The pairing and the clock conversion (「测试」 in docs/design/platform.md,
//! `frame_pairing_*` and `frame_clock_*`): pure, on every platform. The
//! sequences stand in for the hooks: `frame_started`, `drawable` and
//! `commit` on the recording thread, `tick`, `completed` and `presented`
//! from the system's threads. The clock has a timebase of 1/1, so ticks
//! are nanoseconds and media clock seconds are ticks / 1e9.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::super::{FrameProblems, FrameTimes, PresentedAt, TimeSource};
use super::{Clock, SLOTS, Store};

const REFRESH: Duration = Duration::from_micros(8333);
/// The tick clock's "now" when the recording is collected: 100 s.
const NOW_TICKS: u64 = 100_000_000_000;

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000)
}

fn clock() -> Clock {
    Clock::new(1, 1, NOW_TICKS, now())
}

/// The system time of a moment in ticks under `clock()`.
fn at(ticks: u64) -> SystemTime {
    now() - Duration::from_nanos(NOW_TICKS - ticks)
}

fn seconds(ticks: u64) -> f64 {
    ticks as f64 / 1e9
}

/// A store with a recording begun at tick 1 ms.
fn recording() -> (Store, u32) {
    let store = Store::new();
    assert!(store.claim());
    let generation = store.begin(1_000_000);
    (store, generation)
}

/// One frame handling that draws: started at `started`, completed at
/// `completed`, shown at `shown`. Returns its slot.
fn frame(store: &Store, generation: u32, started: u64, completed: u64, shown: u64) -> u32 {
    store.frame_started(started);
    let index = store.drawable().expect("a slot");
    assert_eq!(store.commit(), Some(index));
    store.completed(generation, index, seconds(completed));
    store.presented(generation, index, seconds(shown), seconds(shown + 500));
    index
}

/// The f64 seconds round trip loses well under a microsecond at 100 s.
fn close(a: SystemTime, b: SystemTime) -> bool {
    let gap = a
        .duration_since(b)
        .or_else(|_| b.duration_since(a))
        .unwrap();
    gap < Duration::from_micros(1)
}

fn assert_times(found: &FrameTimes, requested: u64, started: u64, completed: u64) {
    assert_eq!(found.requested, at(requested), "requested {found:?}");
    assert_eq!(found.started, at(started), "started {found:?}");
    assert!(close(found.completed, at(completed)), "completed {found:?}");
}

#[test]
fn frame_pairing_consecutive_frames() {
    let (store, generation) = recording();
    for i in 0..3u64 {
        let tick = 10_000_000 + i * 8_333_333;
        store.tick(tick);
        frame(
            &store,
            generation,
            tick + 200_000,
            tick + 3_000_000,
            tick + 8_000_000,
        );
    }
    let taken = store.stop();
    assert!(store.settled(taken));
    let recording = store.collect(taken, REFRESH, &clock());
    assert_eq!(recording.refresh, REFRESH);
    assert_eq!(recording.problems, FrameProblems::default());
    assert_eq!(recording.frames.len(), 3);
    for (i, found) in recording.frames.iter().enumerate() {
        let tick = 10_000_000 + i as u64 * 8_333_333;
        assert_times(found, tick, tick + 200_000, tick + 3_000_000);
        let presented = found.presented.expect("presented");
        assert_eq!(presented.source, TimeSource::Presented);
        assert!(close(presented.time, at(tick + 8_000_000)), "{presented:?}");
    }
}

#[test]
fn frame_pairing_main_thread_blocked_across_two_ticks() {
    let (store, generation) = recording();
    store.tick(10_000_000);
    frame(&store, generation, 10_100_000, 12_000_000, 18_000_000);
    // The main thread is busy through two ticks; the next frame handling
    // answers the first of them, and its work time includes the block.
    store.tick(18_333_333);
    store.tick(26_666_666);
    frame(&store, generation, 27_000_000, 29_000_000, 35_000_000);
    let taken = store.stop();
    let recording = store.collect(taken, REFRESH, &clock());
    assert!(recording.problems.is_clean());
    assert_eq!(recording.problems.without_tick, 0);
    assert_times(&recording.frames[1], 18_333_333, 27_000_000, 29_000_000);
}

#[test]
fn frame_pairing_a_frame_handling_that_draws_nothing() {
    let (store, generation) = recording();
    store.tick(10_000_000);
    frame(&store, generation, 10_100_000, 12_000_000, 18_000_000);
    store.tick(18_333_333);
    // GPUI decides the window needs no redraw: no drawable.
    store.frame_started(18_400_000);
    store.tick(26_666_666);
    frame(&store, generation, 26_700_000, 28_000_000, 35_000_000);
    let taken = store.stop();
    let recording = store.collect(taken, REFRESH, &clock());
    assert!(recording.problems.is_clean());
    assert_eq!(recording.frames.len(), 2);
    // The next frame's "previous" start is the one that drew nothing.
    assert_times(&recording.frames[1], 26_666_666, 26_700_000, 28_000_000);
}

#[test]
fn frame_pairing_a_frame_without_a_tick() {
    let (store, generation) = recording();
    // `displayLayer:`: no display link tick between the two starts.
    frame(&store, generation, 5_000_000, 7_000_000, 9_000_000);
    // A tick after the start is not the one the frame answers.
    store.tick(5_500_000);
    let taken = store.stop();
    let recording = store.collect(taken, REFRESH, &clock());
    assert!(recording.problems.is_clean());
    assert_eq!(recording.problems.without_tick, 1);
    assert_times(&recording.frames[0], 5_000_000, 5_000_000, 7_000_000);
}

#[test]
fn frame_pairing_unmatched_drawables() {
    let (store, generation) = recording();
    // A drawable before any frame handling started.
    assert_eq!(store.drawable(), None);
    frame(&store, generation, 5_000_000, 7_000_000, 9_000_000);
    // A second drawable in the same frame handling.
    assert_eq!(store.drawable(), None);
    let taken = store.stop();
    let recording = store.collect(taken, REFRESH, &clock());
    assert_eq!(recording.problems.unmatched, 2);
    assert!(!recording.problems.is_clean());
    assert_eq!(recording.frames.len(), 1);
}

#[test]
fn frame_pairing_only_the_first_commit_after_a_drawable() {
    let (store, _) = recording();
    assert_eq!(store.commit(), None);
    store.frame_started(5_000_000);
    let index = store.drawable();
    assert!(index.is_some());
    assert_eq!(store.commit(), index);
    assert_eq!(store.commit(), None);
}

#[test]
fn frame_pairing_incomplete_frames_are_left_out() {
    let (store, generation) = recording();
    store.frame_started(5_000_000);
    let first = store.drawable().unwrap();
    store.commit();
    // A failed command buffer gives no time.
    store.completed(generation, first, 0.0);
    store.presented(generation, first, seconds(9_000_000), seconds(9_000_000));
    frame(&store, generation, 15_000_000, 17_000_000, 19_000_000);
    let taken = store.stop();
    assert!(!store.settled(taken));
    let recording = store.collect(taken, REFRESH, &clock());
    assert_eq!(recording.problems.incomplete, 1);
    assert!(!recording.problems.is_clean());
    assert_eq!(recording.frames.len(), 1);
    assert_times(&recording.frames[0], 15_000_000, 15_000_000, 17_000_000);
}

#[test]
fn frame_pairing_past_the_capacity_overflows() {
    let (store, generation) = recording();
    for i in 0..SLOTS as u64 + 2 {
        store.frame_started(2_000_000 + i * 1000);
        if let Some(index) = store.drawable() {
            store.commit();
            store.completed(generation, index, seconds(3_000_000 + i * 1000));
            store.presented(generation, index, 0.0, seconds(4_000_000));
        }
    }
    let taken = store.stop();
    let recording = store.collect(taken, REFRESH, &clock());
    assert_eq!(recording.problems.overflowed, 2);
    assert!(!recording.problems.is_clean());
    assert_eq!(recording.frames.len(), SLOTS);
}

#[test]
fn frame_pairing_presented_time_zero_is_the_handler_time() {
    let (store, generation) = recording();
    store.frame_started(5_000_000);
    let index = store.drawable().unwrap();
    store.commit();
    store.completed(generation, index, seconds(7_000_000));
    store.presented(generation, index, 0.0, seconds(9_000_000));
    let taken = store.stop();
    let recording = store.collect(taken, REFRESH, &clock());
    let PresentedAt { time, source } = recording.frames[0].presented.unwrap();
    assert_eq!(source, TimeSource::Handler);
    assert!(close(time, at(9_000_000)));
}

#[test]
fn frame_pairing_no_presented_handler_by_the_deadline_is_none() {
    let (store, generation) = recording();
    store.frame_started(5_000_000);
    let index = store.drawable().unwrap();
    store.commit();
    store.completed(generation, index, seconds(7_000_000));
    let taken = store.stop();
    assert!(!store.settled(taken));
    let recording = store.collect(taken, REFRESH, &clock());
    assert!(recording.problems.is_clean());
    assert_eq!(recording.frames[0].presented, None);
}

#[test]
fn frame_pairing_a_stale_generation_writes_nothing() {
    let (store, old) = recording();
    store.frame_started(5_000_000);
    let index = store.drawable().unwrap();
    store.stop();
    // The next recording takes the same slot; the old handlers arrive late.
    assert!(store.claim());
    let new = store.begin(20_000_000);
    assert_ne!(new, old);
    assert_eq!(store.generation(), new);
    store.frame_started(25_000_000);
    assert_eq!(store.drawable(), Some(index));
    store.completed(old, index, seconds(26_000_000));
    store.presented(old, index, seconds(27_000_000), seconds(27_000_000));
    let taken = store.stop();
    let recording = store.collect(taken, REFRESH, &clock());
    assert_eq!(recording.problems.incomplete, 1);
    assert!(recording.frames.is_empty());
}

#[test]
fn frame_pairing_one_recorder_at_a_time_and_no_ticks_when_stopped() {
    let store = Store::new();
    assert!(store.claim());
    assert!(!store.claim());
    store.release();
    assert!(store.claim());
    let generation = store.begin(1_000_000);
    frame(&store, generation, 5_000_000, 7_000_000, 9_000_000);
    let taken = store.stop();
    // A tick after the stop is not kept.
    store.tick(4_000_000);
    let recording = store.collect(taken, REFRESH, &clock());
    assert_eq!(recording.problems.without_tick, 1);
    assert!(store.claim());
}

#[test]
fn frame_clock_ticks_to_nanoseconds() {
    let one = Clock::new(1, 1, 0, now());
    assert_eq!(one.nanos(123_456), 123_456);
    // Intel Macs and some Apple silicon report 125/3: 24 MHz ticks.
    let apple = Clock::new(125, 3, 0, now());
    assert_eq!(apple.nanos(24_000_000), 1_000_000_000);
    assert_eq!(apple.nanos(3), 125);
    // A denominator of 0 is taken as 1, never a division by zero.
    assert_eq!(Clock::new(2, 0, 0, now()).nanos(5), 10);
    // No overflow at the top of the tick range.
    assert_eq!(apple.nanos(u64::MAX), u128::from(u64::MAX) * 125 / 3);
}

#[test]
fn frame_clock_to_the_system_clock() {
    let apple = Clock::new(125, 3, 24_000_000 * 10, now());
    // One second of ticks before "now" is one second before `now`.
    assert_eq!(
        apple.time_of_ticks(24_000_000 * 9),
        now() - Duration::from_secs(1)
    );
    // A moment slightly ahead (a tick read after the clock pair).
    assert_eq!(
        apple.time_of_ticks(24_000_000 * 10 + 24),
        now() + Duration::from_micros(1)
    );
    // Media clock seconds are the same clock: 9.5 s is 0.5 s before.
    let time = apple.time_of_seconds(9.5);
    assert!(close(time, now() - Duration::from_millis(500)), "{time:?}");
    // Spans at the ends of the tick range do not panic.
    let far = Clock::new(1, 1, u64::MAX, UNIX_EPOCH);
    assert!(far.time_of_ticks(0) < UNIX_EPOCH);
    assert!(Clock::new(1, 1, 0, now()).time_of_ticks(u64::MAX) > now());
}
