//! `deadline_*`: the per-file deadline, cancellation, pause, and the
//! watchdog's thread and stop path.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use lectern_executor::Executor;

use crate::extract::{Deadline, ExtractStatus, LanguageTable, extract};
use crate::testing::{ManualClock, pathological_js, watchdog};
use crate::watchdog::Trip;
use crate::{Clock, SystemClock};

const MINUTE: Duration = Duration::from_secs(60);

/// Polls `f` for up to two seconds.
fn eventually(mut f: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(1));
    }
    f()
}

#[test]
fn deadline_pathological_input_times_out() {
    let executor = Executor::manual();
    let (watchdog, _) = watchdog(&executor, Arc::new(SystemClock));
    let table = LanguageTable::core();
    let js = table.get("js").unwrap();
    assert!(js.prepare());
    let source = pathological_js(1 << 20);
    let started = Instant::now();
    let at = SystemClock.now() + Duration::from_millis(1);
    let e = extract(
        js,
        &source,
        &Deadline::at(&watchdog, at, source.len()),
        &Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(e.status, ExtractStatus::TimedOut);
    assert!(e.defs.is_empty() && e.refs.is_empty());
    // Interrupted within a few checks, not after the whole parse.
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(watchdog.in_flight(), 0);
    watchdog.stop();
}

#[test]
fn deadline_cancel_flag_cancels() {
    let executor = Executor::manual();
    let (watchdog, _) = watchdog(&executor, Arc::new(SystemClock));
    let table = LanguageTable::core();
    let js = table.get("js").unwrap();
    assert!(js.prepare());

    // Set before the start: nothing is parsed.
    let cancel = Arc::new(AtomicBool::new(true));
    let e = extract(
        js,
        b"function a() {}",
        &Deadline::background(&watchdog),
        &cancel,
    );
    assert_eq!(e.status, ExtractStatus::Cancelled);

    // Set while parsing: the watchdog passes it on.
    let cancel = Arc::new(AtomicBool::new(false));
    let source = pathological_js(4 << 20);
    let deadline = Deadline::at(&watchdog, Instant::now() + MINUTE, source.len());
    let e = thread::scope(|s| {
        let worker = s.spawn(|| extract(js, &source, &deadline, &cancel));
        assert!(eventually(|| watchdog.in_flight() == 1));
        cancel.store(true, Ordering::Release);
        worker.join().expect("worker")
    });
    assert_eq!(e.status, ExtractStatus::Cancelled);
    watchdog.stop();
}

#[test]
fn deadline_pause_probe_cancels() {
    let executor = Executor::manual();
    let (watchdog, _) = watchdog(&executor, Arc::new(SystemClock));
    let table = LanguageTable::core();
    let js = table.get("js").unwrap();
    assert!(js.prepare());
    let paused = Arc::new(AtomicBool::new(false));
    let source = pathological_js(4 << 20);
    let probe = paused.clone();
    let deadline = Deadline::at(&watchdog, Instant::now() + MINUTE, source.len())
        .with_pause(Arc::new(move || probe.load(Ordering::Acquire)));
    let cancel = Arc::new(AtomicBool::new(false));
    let e = thread::scope(|s| {
        let worker = s.spawn(|| extract(js, &source, &deadline, &cancel));
        assert!(eventually(|| watchdog.in_flight() == 1));
        paused.store(true, Ordering::Release);
        worker.join().expect("worker")
    });
    assert_eq!(e.status, ExtractStatus::Cancelled);
    watchdog.stop();
}

#[test]
fn deadline_watchdog_trips_at_the_deadline() {
    let executor = Executor::manual();
    let clock = ManualClock::new();
    let (watchdog, task) = watchdog(&executor, Arc::new(clock.clone()));
    let at = clock.now() + Duration::from_millis(250);
    let registration = watchdog.register(at, None, None);
    assert!(task.started());
    // Before the deadline: checked, not tripped.
    let rounds = watchdog.rounds();
    assert!(eventually(|| watchdog.rounds() >= rounds + 3));
    assert_eq!(registration.slot().tripped(), None);
    assert_eq!(registration.slot().flag().load(Ordering::SeqCst), 0);

    clock.advance(Duration::from_millis(250));
    assert!(eventually(|| registration.slot().tripped().is_some()));
    assert_eq!(registration.slot().tripped(), Some(Trip::Deadline));
    assert_eq!(registration.slot().flag().load(Ordering::SeqCst), 1);
    drop(registration);
    watchdog.stop();
    assert!(eventually(|| task.is_finished()));
}

#[test]
fn deadline_watchdog_idle_does_not_wake() {
    let executor = Executor::manual();
    let (watchdog, task) = watchdog(&executor, Arc::new(SystemClock));
    // No registration yet: no thread.
    assert!(!task.started());
    let registration = watchdog.register(Instant::now() + Duration::from_secs(60), None, None);
    assert!(eventually(|| watchdog.rounds() >= 2));
    drop(registration);
    // One more round notices the table is empty; then it waits untimed.
    thread::sleep(Duration::from_millis(30));
    let rounds = watchdog.rounds();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(watchdog.rounds(), rounds);
    assert!(!task.is_finished());
    watchdog.stop();
    assert!(eventually(|| task.is_finished()));
}

#[test]
fn deadline_stop_cancels_in_flight_and_exits() {
    let executor = Executor::manual();
    let (watchdog, task) = watchdog(&executor, Arc::new(SystemClock));
    let table = LanguageTable::core();
    let js = table.get("js").unwrap();
    assert!(js.prepare());
    let source = pathological_js(4 << 20);
    let deadline = Deadline::at(&watchdog, Instant::now() + MINUTE, source.len());
    let cancel = Arc::new(AtomicBool::new(false));
    let e = thread::scope(|s| {
        let worker = s.spawn(|| extract(js, &source, &deadline, &cancel));
        assert!(eventually(|| watchdog.in_flight() == 1));
        // The guard the plugin hands to `PluginHandle::own`.
        drop(watchdog.stop_guard());
        assert!(watchdog.is_stopped());
        worker.join().expect("worker")
    });
    assert_eq!(e.status, ExtractStatus::Cancelled);
    assert!(eventually(|| task.is_finished()));

    // After the stop every extraction is cancelled at once, and no thread
    // starts again.
    let e = extract(
        table.get("js").unwrap(),
        b"function a() {}",
        &Deadline::background(&watchdog),
        &Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(e.status, ExtractStatus::Cancelled);
    assert_eq!(watchdog.in_flight(), 0);
    // Stopping twice is harmless.
    watchdog.stop();
}

#[test]
fn deadline_stop_before_start_starts_no_thread() {
    let executor = Executor::manual();
    let (watchdog, task) = watchdog(&executor, Arc::new(SystemClock));
    watchdog.stop();
    let registration = watchdog.register(Instant::now() + Duration::from_secs(1), None, None);
    assert_eq!(registration.slot().tripped(), Some(Trip::Stopped));
    assert!(!task.started());
}
