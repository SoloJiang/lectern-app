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
fn deadline_one_mib_returns_soon_after_the_deadline() {
    // A 1 MiB file with a 100 ms deadline returns soon after it on both
    // extraction paths and in `ParsedFile`: no phase after the parse (the
    // binding model, the rules) is long or runs unchecked. Unchecked work
    // used to take seconds here; the margin is wide enough for a debug build
    // on a loaded CI runner (179 ms seen with a 150 ms limit). Release-build
    // timings are in the design doc's 「实际验证」.
    let executor = Executor::manual();
    let (watchdog, _) = watchdog(&executor, Arc::new(SystemClock));
    let table = LanguageTable::core();
    let cancel = Arc::new(AtomicBool::new(false));
    let limit = Duration::from_millis(600);
    for lang in ["js", "python", "ts"] {
        let l = table.get(lang).unwrap();
        assert!(l.prepare());
        let source = crate::testing::corpus(lang, 1 << 20);
        let started = Instant::now();
        let deadline = Deadline::at(&watchdog, started + Duration::from_millis(100), 2 << 20);
        let walked = extract(l, &source, &deadline, &cancel);
        let walked_took = started.elapsed();
        let started = Instant::now();
        let deadline = Deadline::at(&watchdog, started + Duration::from_millis(100), 2 << 20);
        let tagged = crate::extract::extract_tags(l, &source, &deadline, &cancel);
        let tagged_took = started.elapsed();
        let started = Instant::now();
        let parsed = crate::site::ParsedFile::parse(
            l,
            Arc::from(source.as_slice()),
            &SystemClock,
            started + Duration::from_millis(100),
            &AtomicBool::new(false),
        );
        let parsed_took = started.elapsed();
        for (path, took, done) in [
            ("walk", walked_took, walked.status == ExtractStatus::Done),
            ("tags", tagged_took, tagged.status == ExtractStatus::Done),
            ("ParsedFile", parsed_took, parsed.is_ok()),
        ] {
            assert!(
                took < limit || (done && took < Duration::from_millis(100)),
                "{lang} {path}: {took:?}"
            );
        }
    }
    watchdog.stop();
}

#[test]
fn deadline_checked_at_the_start() {
    // A deadline already past or a pause already requested stops a small
    // file at once, on both extraction paths and in `ParsedFile`, even when
    // the watchdog's thread never gets to check (its loop never runs here).
    let clock = ManualClock::new();
    let watchdog = crate::Watchdog::new(Arc::new(clock.clone()), Box::new(|_| {}));
    let table = LanguageTable::core();
    let cancel = Arc::new(AtomicBool::new(false));
    let source = b"function a() { return 1; }\n";
    let expired = Deadline::at(&watchdog, clock.now(), 1 << 20);
    let paused =
        Deadline::at(&watchdog, clock.now() + MINUTE, 1 << 20).with_pause(Arc::new(|| true));
    let open = Deadline::at(&watchdog, clock.now() + MINUTE, 1 << 20);
    for lang in ["js", "python", "go"] {
        let l = table.get(lang).unwrap();
        assert!(l.prepare());
        assert_eq!(
            extract(l, source, &open, &cancel).status,
            ExtractStatus::Done,
            "{lang}"
        );
        assert_eq!(
            extract(l, source, &expired, &cancel).status,
            ExtractStatus::TimedOut,
            "{lang}"
        );
        assert_eq!(
            extract(l, source, &paused, &cancel).status,
            ExtractStatus::Cancelled,
            "{lang}"
        );
        for (deadline, status) in [
            (&expired, ExtractStatus::TimedOut),
            (&paused, ExtractStatus::Cancelled),
        ] {
            let tagged = crate::extract::extract_tags(l, source, deadline, &cancel);
            assert_eq!(tagged.status, status, "{lang} tags path");
        }
        let parsed = crate::site::ParsedFile::parse(
            l,
            Arc::from(&source[..]),
            &clock,
            clock.now(),
            &AtomicBool::new(false),
        );
        assert_eq!(
            parsed.err(),
            Some(crate::site::ParseError::TimedOut),
            "{lang}"
        );
    }
    assert_eq!(watchdog.in_flight(), 0);
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
    // While the registration is in the table the thread only waits with a
    // timeout, so this is the untimed count before it can park.
    let (_, parked) = watchdog.waits();
    drop(registration);
    // The round that notices the table is empty parks the thread in the
    // untimed wait: wait for that, not for a quiet stretch of time.
    assert!(eventually(|| watchdog.waits().1 > parked));
    let (timed, untimed) = watchdog.waits();
    let rounds = watchdog.rounds();
    thread::sleep(Duration::from_millis(100));
    // Parked: no timed wait since, and every round since (a spurious
    // wake-up) went straight back into the untimed wait.
    let (timed_after, untimed_after) = watchdog.waits();
    assert_eq!(timed_after, timed, "a timed wait with nothing in flight");
    assert_eq!(watchdog.rounds() - rounds, untimed_after - untimed);
    assert!(!task.is_finished());
    watchdog.stop();
    assert!(eventually(|| task.is_finished()));
}

/// With the system's clock and only a due entry a minute away (a kept
/// parse's expiry, ADR 0037), the thread sleeps until it instead of waking
/// every check interval; an earlier entry scheduled meanwhile wakes it and
/// fires on time.
#[test]
fn deadline_watchdog_sleeps_until_a_far_due_entry() {
    let executor = Executor::manual();
    let (watchdog, task) = watchdog(&executor, Arc::new(SystemClock));
    assert!(watchdog.schedule(Instant::now() + Duration::from_secs(60), Box::new(|| {})));
    assert!(eventually(|| watchdog.waits().0 >= 1));
    let (timed, _) = watchdog.waits();
    thread::sleep(Duration::from_millis(100));
    // At most a spurious wake-up, not one every 10 ms.
    assert!(watchdog.waits().0 <= timed + 1, "{:?}", watchdog.waits());
    let fired = watchdog.fired();
    assert!(watchdog.schedule(Instant::now() + Duration::from_millis(20), Box::new(|| {})));
    assert!(eventually(|| watchdog.fired() > fired));
    assert_eq!(watchdog.scheduled(), 1, "the far one is still due");
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
fn deadline_executor_shutdown_stops_the_watchdog() {
    let executor = Executor::manual();
    let (watchdog, task) = watchdog(&executor, Arc::new(SystemClock));
    let registration = watchdog.register(Instant::now() + MINUTE, None, None);
    assert!(task.started());
    // The executor does not wait for dedicated threads; its shutdown hook
    // stops the watchdog, which cancels what is in flight and exits.
    executor.shutdown(Duration::from_secs(1));
    assert!(watchdog.is_stopped());
    assert_eq!(registration.slot().tripped(), Some(Trip::Stopped));
    assert!(eventually(|| task.is_finished()));
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

#[test]
fn deadline_walk_checks_stop_while_walking() {
    // The walk asks its stop predicate every so many nodes, not only at the
    // end, and gives up as soon as it says so.
    let table = LanguageTable::core();
    let js = table.get("js").unwrap();
    let source = crate::testing::corpus("js", 64 << 10);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(js.language()).unwrap();
    let tree = parser.parse(&source, None).unwrap();
    let calls = std::cell::Cell::new(0u32);
    let never = || {
        calls.set(calls.get() + 1);
        false
    };
    assert!(crate::walk::walk_tree(js.dialect(), js.language(), &tree, &source, &never).is_some());
    let checks = calls.get();
    assert!(checks > 2, "{checks} checks for 64 KiB");
    calls.set(0);
    let second = || {
        calls.set(calls.get() + 1);
        calls.get() >= 2
    };
    assert!(crate::walk::walk_tree(js.dialect(), js.language(), &tree, &source, &second).is_none());
    assert_eq!(calls.get(), 2, "stopped at the second check");
}

/// A clock that counts its readings and, from reading `expire_at` on, is
/// past every deadline.
struct CountingClock {
    start: Instant,
    reads: std::sync::atomic::AtomicU32,
    expire_at: u32,
}

impl Clock for CountingClock {
    fn now(&self) -> Instant {
        let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= self.expire_at {
            self.start + MINUTE * 10
        } else {
            self.start
        }
    }
}

#[test]
fn deadline_parsed_file_checks_every_phase() {
    // `ParsedFile::parse` reads its clock in the parse, the query run, the
    // walk and once more before returning: a deadline that passes at any of
    // those readings, the last one included, gives `TimedOut`.
    let table = LanguageTable::core();
    let js = table.get("js").unwrap();
    let source: Arc<[u8]> = Arc::from(crate::testing::corpus("js", 64 << 10));
    let parse = |expire_at: u32| {
        let clock = CountingClock {
            start: Instant::now(),
            reads: std::sync::atomic::AtomicU32::new(0),
            expire_at,
        };
        let result = crate::site::ParsedFile::parse(
            js,
            source.clone(),
            &clock,
            clock.start + MINUTE,
            &AtomicBool::new(false),
        );
        (result.err(), clock.reads.load(Ordering::SeqCst))
    };
    let (error, reads) = parse(u32::MAX);
    assert_eq!(error, None);
    for expire_at in [1, reads / 2, reads - 1, reads] {
        assert_eq!(
            parse(expire_at).0,
            Some(crate::site::ParseError::TimedOut),
            "expiring at reading {expire_at} of {reads}"
        );
    }
}

#[test]
fn deadline_parsed_file_walk_reads_the_deadline() {
    // The same Java source as core `java` (walked) and as a language pack
    // with Java's grammar and queries (not walked), both with the full
    // query (core languages otherwise run fewer patterns,
    // `Lang::site_query`): parsing, the query run and the check before
    // returning read the clock equally often, so the difference is the
    // walk's checks.
    let source: Arc<[u8]> = Arc::from(crate::testing::corpus("java", 64 << 10));
    let table = LanguageTable::core().with_pack(
        crate::lang_name("kotlin"),
        crate::testing::java_pack(9, Arc::new(())),
    );
    let readings = |name: &str| {
        let lang = table.get(name).unwrap();
        let clock = CountingClock {
            start: Instant::now(),
            reads: std::sync::atomic::AtomicU32::new(0),
            expire_at: u32::MAX,
        };
        crate::site::ParsedFile::parse_with(
            lang,
            source.clone(),
            &clock,
            clock.start + MINUTE,
            &AtomicBool::new(false),
            true,
        )
        .unwrap();
        clock.reads.load(Ordering::SeqCst)
    };
    let java = table.get("java").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(java.language()).unwrap();
    let tree = parser.parse(&*source, None).unwrap();
    let walk_checks = std::cell::Cell::new(0u32);
    let count = || {
        walk_checks.set(walk_checks.get() + 1);
        false
    };
    crate::walk::walk_tree(java.dialect(), java.language(), &tree, &source, &count).unwrap();
    assert_eq!(
        readings("java"),
        readings("kotlin") + walk_checks.get(),
        "the walk checks {} times",
        walk_checks.get()
    );
}
