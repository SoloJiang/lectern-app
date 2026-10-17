//! Pausable background work (ADR 0029): `BackgroundGuard`, the pause switch,
//! the background state and epoch.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Barrier, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

use super::executor::{Executor, Latch, Task};
use super::support::*;
use crate::{
    BackgroundChanged, BackgroundGuard, BackgroundState, BackgroundStatus, Busy, Kernel, PluginSet,
    Quiescent,
};

/// A waker that counts its wakes.
#[derive(Default)]
struct Count(AtomicUsize);

impl Wake for Count {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, SeqCst);
    }
}

impl Count {
    fn wakes(&self) -> usize {
        self.0.load(SeqCst)
    }
}

fn poll(guard: &BackgroundGuard, count: &Arc<Count>) -> Poll<()> {
    let waker = Waker::from(count.clone());
    guard.poll_proceed(&mut Context::from_waker(&waker))
}

/// Records `BackgroundChanged` as `(state, seq)` and every `Busy` /
/// `Quiescent`.
#[derive(Clone, Default)]
struct Events {
    background: Arc<Mutex<Vec<(BackgroundState, u64)>>>,
    transitions: Log,
}

impl Events {
    fn background(&self) -> Vec<(BackgroundState, u64)> {
        self.background.lock().unwrap().clone()
    }

    /// The latest change by `seq`, the way a listener orders them.
    fn latest(&self) -> Option<(BackgroundState, u64)> {
        self.background().into_iter().max_by_key(|(_, seq)| *seq)
    }
}

fn recorder(events: &Events) -> TestPlugin {
    let events = events.clone();
    TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
        let b = events.background.clone();
        ctx.on(move |change: &BackgroundChanged, _| {
            b.lock().unwrap().push((change.state, change.seq));
        });
        let t = events.transitions.clone();
        ctx.on(move |busy: &Busy, _| t.push(format!("busy {}", busy.seq)));
        let t = events.transitions.clone();
        ctx.on(move |quiet: &Quiescent, _| t.push(format!("quiescent {}", quiet.seq)));
        Ok(Box::new(()))
    })
}

struct Setup {
    kernel: Arc<Kernel>,
    slot: HandleSlot,
    events: Events,
}

fn setup() -> Setup {
    let slot = HandleSlot::default();
    let events = Events::default();
    let kernel = start(
        PluginSet::new()
            .with("recorder", recorder(&events))
            .with("p", handle_plugin(&slot, after_first_frame())),
    );
    settle(&kernel);
    Setup {
        kernel,
        slot,
        events,
    }
}

fn settle(kernel: &Kernel) {
    wait_until("quiescent", || kernel.is_quiescent());
    kernel.barrier();
    wait_until("quiescent", || kernel.is_quiescent());
}

fn status(kernel: &Kernel) -> (BackgroundState, usize) {
    let BackgroundStatus { state, count, .. } = kernel.background();
    (state, count)
}

fn epoch(kernel: &Kernel) -> u64 {
    kernel.background().epoch
}

#[test]
fn background_guards_do_not_affect_quiescence_or_the_epoch() {
    let s = setup();
    let quiet = s.kernel.quiescent_epoch().expect("quiescent");
    s.events.transitions.take();
    let waker = Arc::new(Count::default());

    let first = s.slot.get().background().unwrap();
    assert_eq!(s.kernel.quiescent_epoch(), Some(quiet));
    s.kernel.pause();
    let second = s.slot.get().background().unwrap();
    assert_eq!(poll(&first, &waker), Poll::Pending);
    assert_eq!(poll(&second, &waker), Poll::Pending);
    assert_eq!(status(&s.kernel), (BackgroundState::Paused, 2));
    s.kernel.resume();
    assert_eq!(poll(&first, &waker), Poll::Ready(()));
    drop(first);
    drop(second);
    assert_eq!(status(&s.kernel), (BackgroundState::Idle, 0));

    // The kernel thread dispatched every BackgroundChanged, and neither the
    // guards nor those dispatches made the kernel busy.
    let last = epoch(&s.kernel);
    wait_until("last change dispatched", || {
        s.events.latest() == Some((BackgroundState::Idle, last))
    });
    assert_eq!(s.kernel.quiescent_epoch(), Some(quiet));
    assert!(s.kernel.is_quiescent());
    assert!(s.events.transitions.snapshot().is_empty());
    shutdown(&s.kernel);
}

#[test]
fn the_state_goes_idle_running_paused_and_back() {
    let s = setup();
    let waker = Arc::new(Count::default());
    assert_eq!(status(&s.kernel), (BackgroundState::Idle, 0));

    let first = s.slot.get().background().unwrap();
    assert_eq!(status(&s.kernel), (BackgroundState::Running, 1));
    assert_eq!(poll(&first, &waker), Poll::Ready(()));
    assert!(!first.pause_requested());

    s.kernel.pause();
    assert!(first.pause_requested());
    // Still running until it reaches its batch boundary.
    assert_eq!(status(&s.kernel), (BackgroundState::Running, 1));
    assert_eq!(poll(&first, &waker), Poll::Pending);
    assert_eq!(status(&s.kernel), (BackgroundState::Paused, 1));

    // Work taken during the pause is stopped before its first batch.
    let second = s.slot.get().background().unwrap();
    assert_eq!(status(&s.kernel), (BackgroundState::Paused, 2));
    assert_eq!(poll(&second, &waker), Poll::Pending);
    assert_eq!(waker.wakes(), 0);

    s.kernel.resume();
    assert_eq!(status(&s.kernel), (BackgroundState::Running, 2));
    assert_eq!(waker.wakes(), 2, "resume wakes both stopped guards");
    assert!(!first.pause_requested());
    assert_eq!(poll(&first, &waker), Poll::Ready(()));
    assert_eq!(poll(&second, &waker), Poll::Ready(()));

    drop(first);
    assert_eq!(status(&s.kernel), (BackgroundState::Running, 1));
    drop(second);
    assert_eq!(status(&s.kernel), (BackgroundState::Idle, 0));

    // One BackgroundChanged per state change, carrying the epoch.
    let last = epoch(&s.kernel);
    wait_until("last change dispatched", || {
        s.events.latest() == Some((BackgroundState::Idle, last))
    });
    let mut changes = s.events.background();
    changes.sort_by_key(|(_, seq)| *seq);
    let states: Vec<BackgroundState> = changes.iter().map(|(state, _)| *state).collect();
    assert_eq!(
        states,
        vec![
            BackgroundState::Running,
            BackgroundState::Paused,
            BackgroundState::Running,
            BackgroundState::Idle,
        ]
    );
    shutdown(&s.kernel);
}

#[test]
fn the_epoch_changes_on_each_start_end_stop_and_continue_and_only_then() {
    let s = setup();
    let waker = Arc::new(Count::default());
    let mut expected = epoch(&s.kernel);
    let mut check = |what: &str, changed: bool| {
        if changed {
            expected += 1;
        }
        assert_eq!(epoch(&s.kernel), expected, "after {what}");
    };

    // The switch alone changes nothing while no work is held.
    s.kernel.pause();
    check("pause with no work", false);
    s.kernel.resume();
    check("resume with no work", false);

    let first = s.slot.get().background().unwrap();
    check("start", true);
    assert_eq!(poll(&first, &waker), Poll::Ready(()));
    check("a batch boundary outside a pause", false);
    let _ = s.kernel.background();
    check("reading the status", false);

    s.kernel.pause();
    check("pause before the work reaches a boundary", false);
    assert!(first.pause_requested());
    check("pause_requested", false);
    assert_eq!(poll(&first, &waker), Poll::Pending);
    check("stop", true);
    assert_eq!(poll(&first, &waker), Poll::Pending);
    check("polling a stopped guard again", false);
    s.kernel.pause();
    check("pausing again", false);

    let second = s.slot.get().background().unwrap();
    check("start during the pause", true);
    assert_eq!(poll(&second, &waker), Poll::Pending);
    check("first poll of work started during the pause", false);

    s.kernel.resume();
    check("resume", true);
    s.kernel.resume();
    check("resuming again", false);
    assert_eq!(poll(&first, &waker), Poll::Ready(()));
    check("a boundary after resume", false);

    // A pause that no work reached yet, then resume: nothing stopped, nothing
    // continued.
    s.kernel.pause();
    s.kernel.resume();
    check("pause and resume between boundaries", false);

    drop(first);
    check("end", true);
    s.kernel.pause();
    assert_eq!(poll(&second, &waker), Poll::Pending);
    check("stop", true);
    drop(second);
    check("end of stopped work", true);
    s.kernel.resume();
    check("resume after the stopped work ended", false);
    assert_eq!(status(&s.kernel), (BackgroundState::Idle, 0));
    shutdown(&s.kernel);
}

/// Runs `body` with a background guard on the test executor, returning the
/// executor task.
fn spawn_background<F>(
    s: &Setup,
    executor: &Executor,
    body: impl FnOnce(BackgroundGuard) -> F + Send + 'static,
) -> Arc<Task>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let task: Arc<Mutex<Option<Arc<Task>>>> = Arc::default();
    let t = task.clone();
    let handle = s.slot.get();
    let h = handle.clone();
    let e = executor.clone();
    assert!(handle.own_with(move |gate| {
        let guard = h.background().expect("live");
        let spawned = e.spawn(&gate, body(guard));
        *t.lock().unwrap() = spawned.task();
        spawned
    }));
    let spawned = task.lock().unwrap().clone();
    spawned.expect("spawned")
}

/// Waits until `task` has been polled and is not running.
fn wait_parked(task: &Task, what: &str, ready: impl Fn() -> bool) {
    wait_until(what, || ready() && task.entered.load(SeqCst) == 0);
}

#[test]
fn work_started_during_a_pause_stops_before_its_first_batch_and_resume_releases_it() {
    let s = setup();
    let executor = Executor::new();
    let batches = Arc::new(AtomicUsize::new(0));
    let checked = Arc::new(AtomicBool::new(false));
    s.kernel.pause();

    let (b, c) = (batches.clone(), checked.clone());
    let task = spawn_background(&s, &executor, move |guard| async move {
        for _ in 0..3 {
            c.store(true, SeqCst);
            guard.proceed().await;
            b.fetch_add(1, SeqCst);
        }
    });
    // The task reached its first boundary and returned from that poll.
    wait_parked(&task, "first boundary", || checked.load(SeqCst));
    assert_eq!(batches.load(SeqCst), 0, "no batch ran during the pause");
    assert_eq!(status(&s.kernel), (BackgroundState::Paused, 1));
    assert!(
        s.kernel.is_quiescent(),
        "paused work does not hold off quiescence"
    );

    s.kernel.resume();
    wait_until("work finished", || {
        batches.load(SeqCst) == 3 && status(&s.kernel) == (BackgroundState::Idle, 0)
    });
    shutdown(&s.kernel);
}

#[test]
fn running_work_stops_at_its_next_batch_boundary() {
    let s = setup();
    let executor = Executor::new();
    let batches = Arc::new(AtomicUsize::new(0));
    let in_batch = Latch::default();

    let (b, latch) = (batches.clone(), in_batch.clone());
    let task = spawn_background(&s, &executor, move |guard| async move {
        for _ in 0..3 {
            guard.proceed().await;
            let done = b.fetch_add(1, SeqCst) + 1;
            if done == 1 {
                // The first batch takes a while.
                latch.wait().await;
            }
        }
    });
    wait_parked(&task, "inside the first batch", || {
        batches.load(SeqCst) == 1
    });
    s.kernel.pause();
    // Pausing does not interrupt a batch.
    assert_eq!(status(&s.kernel), (BackgroundState::Running, 1));
    in_batch.release();
    wait_until("stopped at the boundary", || {
        status(&s.kernel) == (BackgroundState::Paused, 1)
    });
    wait_parked(&task, "parked", || true);
    assert_eq!(batches.load(SeqCst), 1, "the next batch did not start");

    s.kernel.resume();
    wait_until("work finished", || {
        batches.load(SeqCst) == 3 && status(&s.kernel) == (BackgroundState::Idle, 0)
    });
    shutdown(&s.kernel);
}

#[test]
fn a_dropped_guard_ends_the_work() {
    let s = setup();
    let waker = Arc::new(Count::default());

    // A stopped guard that is dropped leaves the stopped set too.
    let running = s.slot.get().background().unwrap();
    let stopped = s.slot.get().background().unwrap();
    s.kernel.pause();
    assert_eq!(poll(&stopped, &waker), Poll::Pending);
    assert_eq!(status(&s.kernel), (BackgroundState::Running, 2));
    drop(running);
    assert_eq!(status(&s.kernel), (BackgroundState::Paused, 1));
    drop(stopped);
    assert_eq!(status(&s.kernel), (BackgroundState::Idle, 0));
    s.kernel.resume();
    assert_eq!(waker.wakes(), 0, "a dropped guard is not woken");

    // Deactivating the plugin drops its paused task and with it the guard.
    let executor = Executor::new();
    s.kernel.pause();
    let checked = Arc::new(AtomicBool::new(false));
    let c = checked.clone();
    let task = spawn_background(&s, &executor, move |guard| async move {
        loop {
            c.store(true, SeqCst);
            guard.proceed().await;
        }
    });
    wait_parked(&task, "paused", || checked.load(SeqCst));
    assert_eq!(status(&s.kernel), (BackgroundState::Paused, 1));
    let old = s.slot.get();
    s.kernel.set_enabled("p", false);
    wait_until("task dropped", || {
        status(&s.kernel) == (BackgroundState::Idle, 0)
    });
    assert!(old.background().is_none(), "a dead handle takes no guard");
    s.kernel.resume();
    shutdown(&s.kernel);
}

#[test]
fn count_state_and_epoch_stay_consistent_under_concurrent_guards() {
    const ROUNDS: usize = 300;
    const THREADS: usize = 4;
    const GUARDS: usize = 8;

    let s = setup();
    let quiet = s.kernel.quiescent_epoch().expect("quiescent");
    let handle = s.slot.get();
    for round in 0..ROUNDS {
        // Odd rounds start paused and toggle the switch while the workers run.
        let pausing = round % 2 == 1;
        if pausing {
            s.kernel.pause();
        }
        let before = epoch(&s.kernel);
        let saw_pause = AtomicBool::new(false);
        let start = Barrier::new(THREADS + 2);
        let finished = AtomicUsize::new(0);
        thread::scope(|scope| {
            for _ in 0..THREADS {
                let (handle, start, saw_pause, finished) = (&handle, &start, &saw_pause, &finished);
                scope.spawn(move || {
                    let waker = Arc::new(Count::default());
                    start.wait();
                    let guards: Vec<BackgroundGuard> =
                        (0..GUARDS).map(|_| handle.background().unwrap()).collect();
                    for guard in &guards {
                        while poll(guard, &waker).is_pending() {
                            saw_pause.store(true, SeqCst);
                            thread::yield_now();
                        }
                    }
                    drop(guards);
                    finished.fetch_add(1, SeqCst);
                });
            }
            let (kernel, start, saw_pause, finished) = (&s.kernel, &start, &saw_pause, &finished);
            scope.spawn(move || {
                start.wait();
                if !pausing {
                    return;
                }
                // Some worker is stopped before toggling begins.
                while !saw_pause.load(SeqCst) {
                    thread::yield_now();
                }
                while finished.load(SeqCst) < THREADS {
                    kernel.resume();
                    thread::yield_now();
                    kernel.pause();
                    thread::yield_now();
                }
                kernel.resume();
            });
            // Snapshots read in one step agree with each other.
            let kernel = &s.kernel;
            start.wait();
            let mut last: Option<BackgroundStatus> = None;
            while finished.load(SeqCst) < THREADS {
                let now = kernel.background();
                assert_eq!(
                    now.count == 0,
                    now.state == BackgroundState::Idle,
                    "{now:?}"
                );
                assert!(now.count <= THREADS * GUARDS, "{now:?}");
                if let Some(last) = last {
                    assert!(now.epoch >= last.epoch, "{last:?} then {now:?}");
                    if now.epoch == last.epoch {
                        assert_eq!(now, last, "same epoch, different state");
                    }
                }
                last = Some(now);
            }
        });
        let after = s.kernel.background();
        assert_eq!((after.state, after.count), (BackgroundState::Idle, 0));
        // Every start and end changed the epoch; stops and continuations did
        // only in the rounds that paused.
        let starts_and_ends = (2 * THREADS * GUARDS) as u64;
        if pausing {
            assert!(saw_pause.load(SeqCst));
            assert!(
                after.epoch - before > starts_and_ends,
                "round {round}: a continuation is counted"
            );
        } else {
            assert_eq!(after.epoch - before, starts_and_ends, "round {round}");
        }
    }

    // The latest event (by seq) matches the final state, and nothing here
    // touched quiescence.
    let last = epoch(&s.kernel);
    wait_until("last change dispatched", || {
        s.events.latest() == Some((BackgroundState::Idle, last))
    });
    assert_eq!(s.kernel.quiescent_epoch(), Some(quiet));
    shutdown(&s.kernel);
}
