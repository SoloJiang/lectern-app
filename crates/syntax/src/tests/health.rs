//! `health_*`: the stuck-grammar check, on test clocks in place of
//! `platform::ThreadCpuClock`.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::health::{self, Clocks, Health, ParseGuard, ThreadClock};
use crate::limits::{STUCK_GRACE, STUCK_GRACE_WALL};

/// Test clocks: a wall clock and per-thread CPU clocks the test advances.
/// "Threads" are simulated: [`ManualClocks::run_on`] says which one the
/// next `enter` runs on.
pub(crate) struct ManualClocks {
    wall: Mutex<Duration>,
    /// None: the current thread has no CPU clock.
    current: Mutex<Option<Arc<ManualThread>>>,
}

pub(crate) struct ManualThread {
    cpu: Mutex<Duration>,
}

impl ManualThread {
    pub(crate) fn add_cpu(&self, by: Duration) {
        *self.cpu.lock().unwrap() += by;
    }
}

impl ThreadClock for ManualThread {
    fn cpu_time(&self) -> io::Result<Duration> {
        Ok(*self.cpu.lock().unwrap())
    }
}

impl ManualClocks {
    /// Starts on a thread of its own.
    pub(crate) fn new() -> Arc<ManualClocks> {
        let clocks = Arc::new(ManualClocks {
            wall: Mutex::new(Duration::ZERO),
            current: Mutex::new(None),
        });
        clocks.run_on(&clocks.new_thread());
        clocks
    }

    pub(crate) fn health(self: &Arc<ManualClocks>) -> Health {
        Health::with_clocks(Box::new(self.clone()))
    }

    /// A new simulated thread with 0 CPU time.
    pub(crate) fn new_thread(&self) -> Arc<ManualThread> {
        Arc::new(ManualThread {
            cpu: Mutex::new(Duration::ZERO),
        })
    }

    pub(crate) fn run_on(&self, thread: &Arc<ManualThread>) {
        *self.current.lock().unwrap() = Some(thread.clone());
    }

    /// The next `enter` runs on a thread whose CPU clock cannot be read.
    pub(crate) fn run_without_cpu_clock(&self) {
        *self.current.lock().unwrap() = None;
    }

    pub(crate) fn advance_wall(&self, by: Duration) {
        *self.wall.lock().unwrap() += by;
    }
}

impl Clocks for Arc<ManualClocks> {
    fn thread(&self) -> Option<Arc<dyn ThreadClock>> {
        self.current
            .lock()
            .unwrap()
            .clone()
            .map(|t| t as Arc<dyn ThreadClock>)
    }

    fn wall(&self) -> Duration {
        *self.wall.lock().unwrap()
    }
}

const BUDGET: Duration = Duration::from_millis(8);
const TINY: Duration = Duration::from_nanos(1);

/// Registers a call of `lang` (grammar version 1) on `thread`.
fn enter_on(
    clocks: &ManualClocks,
    health: &Health,
    thread: &Arc<ManualThread>,
    lang: &str,
    budget: Duration,
) -> Option<ParseGuard> {
    clocks.run_on(thread);
    health.enter(lang, 1, budget)
}

#[test]
fn health_stuck_call_disables_its_grammar_version() {
    let clocks = ManualClocks::new();
    let health = clocks.health();
    let (a, b) = (clocks.new_thread(), clocks.new_thread());
    let running = enter_on(&clocks, &health, &a, "yaml", BUDGET).unwrap();
    // A call of another language runs alongside.
    let other = enter_on(&clocks, &health, &b, "toml", BUDGET).unwrap();
    drop(other);

    // Exactly the budget plus the grace is not yet stuck.
    a.add_cpu(BUDGET + STUCK_GRACE);
    let probe = enter_on(&clocks, &health, &b, "yaml", BUDGET).unwrap();
    drop(probe);
    assert!(health.allows("yaml", 1));

    a.add_cpu(TINY);
    assert!(enter_on(&clocks, &health, &b, "yaml", BUDGET).is_none());
    assert!(!health.allows("yaml", 1));
    // Disabled for the run: also after the stuck call returns.
    drop(running);
    assert!(enter_on(&clocks, &health, &b, "yaml", BUDGET).is_none());
    assert!(!health.allows("yaml", 1));

    // Other languages and other versions of the same language are not
    // affected.
    assert!(health.allows("toml", 1));
    assert!(health.allows("yaml", 2));
    assert!(enter_on(&clocks, &health, &b, "toml", BUDGET).is_some());
    clocks.run_on(&b);
    assert!(health.enter("yaml", 2, BUDGET).is_some());
}

/// A call that returned (`Yielded`, `TimedOut`) is not in the table, so it
/// never counts as stuck, however long the wall and its thread's CPU time
/// run on afterwards.
#[test]
fn health_returned_calls_never_count() {
    let clocks = ManualClocks::new();
    let health = clocks.health();
    let (a, b) = (clocks.new_thread(), clocks.new_thread());
    let guard = enter_on(&clocks, &health, &a, "md", BUDGET).unwrap();
    assert_eq!(health.running(), 1);
    drop(guard);
    assert_eq!(health.running(), 0);
    a.add_cpu(Duration::from_secs(3600));
    clocks.advance_wall(Duration::from_secs(3600));
    assert!(enter_on(&clocks, &health, &b, "md", BUDGET).is_some());
    assert!(health.allows("md", 1));
}

/// A thread pushed aside (the wall clock runs, its CPU time does not) is
/// not stuck.
#[test]
fn health_waiting_thread_is_not_stuck() {
    let clocks = ManualClocks::new();
    let health = clocks.health();
    let (a, b) = (clocks.new_thread(), clocks.new_thread());
    let _running = enter_on(&clocks, &health, &a, "json", BUDGET).unwrap();
    a.add_cpu(BUDGET);
    clocks.advance_wall(Duration::from_secs(3600));
    assert!(enter_on(&clocks, &health, &b, "json", BUDGET).is_some());
    assert!(health.allows("json", 1));
}

/// A resumed call registers again with what is left of its budget, counted
/// from the new registration.
#[test]
fn health_resumed_call_counts_from_its_new_registration() {
    let clocks = ManualClocks::new();
    let health = clocks.health();
    let (a, b) = (clocks.new_thread(), clocks.new_thread());
    let first = enter_on(&clocks, &health, &a, "ts", BUDGET).unwrap();
    a.add_cpu(Duration::from_millis(5));
    drop(first); // the slice ran out

    let left = Duration::from_millis(3);
    let resumed = enter_on(&clocks, &health, &a, "ts", left).unwrap();
    // Counted from the second registration, against the 3 ms left: the
    // 5 ms of the first slice do not count.
    a.add_cpu(left + STUCK_GRACE);
    assert!(enter_on(&clocks, &health, &b, "ts", BUDGET).is_some());
    a.add_cpu(TINY);
    assert!(enter_on(&clocks, &health, &b, "ts", BUDGET).is_none());
    drop(resumed);
}

/// Without a CPU clock the call is timed by the wall clock, with the larger
/// grace.
#[test]
fn health_wall_time_fallback() {
    let clocks = ManualClocks::new();
    let health = clocks.health();
    let b = clocks.new_thread();
    clocks.run_without_cpu_clock();
    let _running = health.enter("go", 1, BUDGET).unwrap();
    clocks.advance_wall(BUDGET + STUCK_GRACE + TINY);
    assert!(enter_on(&clocks, &health, &b, "go", BUDGET).is_some());
    clocks.advance_wall(STUCK_GRACE_WALL - STUCK_GRACE);
    assert!(enter_on(&clocks, &health, &b, "go", BUDGET).is_none());
    assert!(!health.allows("go", 1));
}

/// The process-wide table on `platform`'s real clocks.
#[test]
fn health_global_on_real_clocks() {
    let health = health::global();
    let guard = health
        .enter("health-test", 7, Duration::from_millis(8))
        .unwrap();
    let again = health.enter("health-test", 7, Duration::from_millis(8));
    assert!(again.is_some());
    drop(again);
    drop(guard);
    assert!(health.allows("health-test", 7));
    assert!(std::ptr::eq(health, health::global()));
}

/// `ParseGuard` is not `Send`: this does not compile if it becomes `Send`
/// (the doctest on `ParseGuard` checks the same with `compile_fail`).
#[test]
fn health_parse_guard_is_not_send() {
    trait AmbiguousIfSend<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
    // Ambiguous, so a compile error, exactly when ParseGuard is Send.
    <ParseGuard as AmbiguousIfSend<_>>::check();
}
