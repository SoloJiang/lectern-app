//! Quiescence signals for `bench` and the UI probe.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use super::support::*;
use crate::{Busy, PluginSet, Quiescent, service};

/// A plugin recording `Busy` / `Quiescent` sequence numbers, as `bench` does.
fn recorder(log: &Log) -> TestPlugin {
    let log = log.clone();
    TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
        let l = log.clone();
        ctx.on(move |busy: &Busy, _| l.push(format!("busy {}", busy.seq)));
        let l = log.clone();
        ctx.on(move |quiet: &Quiescent, _| l.push(format!("quiescent {}", quiet.seq)));
        Ok(Box::new(()))
    })
}

fn settle(kernel: &crate::Kernel) {
    wait_until("quiescent", || kernel.is_quiescent());
    // Let the transition event reach its listeners.
    kernel.barrier();
    wait_until("quiescent", || kernel.is_quiescent());
}

#[test]
fn busy_guards_hold_off_quiescence_and_transitions_are_announced_once() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(
        PluginSet::new()
            .with("recorder", recorder(&log))
            .with("p", handle_plugin(&slot, after_first_frame())),
    );
    settle(&kernel);
    let first = kernel.quiescent_epoch().unwrap();
    assert_eq!(kernel.quiescent_epoch(), Some(first));
    wait_until("last transition recorded", || {
        log.snapshot().last() == Some(&format!("quiescent {first}"))
    });
    log.take();

    let guard = slot.get().busy().unwrap();
    assert!(!kernel.is_quiescent());
    drop(guard);
    wait_until("quiescent", || kernel.is_quiescent());
    let second = kernel.quiescent_epoch().unwrap();
    assert_eq!(second, first + 2);
    wait_until("transitions recorded", || log.snapshot().len() == 2);
    thread::sleep(Duration::from_millis(50));
    // One busy and one quiescent, and recording them caused no new transition.
    assert_eq!(
        log.snapshot(),
        vec![format!("busy {}", first + 1), format!("quiescent {second}")]
    );
    assert_eq!(kernel.quiescent_epoch(), Some(second));
    shutdown(&kernel);
}

#[test]
fn an_admitted_call_or_an_emit_in_progress_is_not_quiescent() {
    let refs: Arc<Mutex<Option<crate::ServiceRef<dyn Greeter>>>> = Arc::default();
    let r = refs.clone();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (e, rel) = (entered.clone(), release.clone());
    let kernel = start(
        PluginSet::new()
            .with(
                "provider",
                chain_plugin(
                    "provider",
                    &Log::default(),
                    vec![service::<dyn Greeter>()],
                    vec![],
                    after_first_frame(),
                ),
            )
            .with(
                "user",
                TestPlugin::new(manifest(
                    vec![],
                    vec![service::<dyn Greeter>()],
                    after_first_frame(),
                ))
                .activate(move |ctx| {
                    *r.lock().unwrap() = Some(ctx.service::<dyn Greeter>());
                    let (e, rel) = (e.clone(), rel.clone());
                    ctx.on(move |_: &Ping, _| {
                        e.wait();
                        rel.wait();
                    });
                    Ok(Box::new(()))
                }),
            ),
    );
    settle(&kernel);
    let call = refs.lock().unwrap().clone().unwrap().get().unwrap();
    assert!(!kernel.is_quiescent());
    drop(call);
    settle(&kernel);

    let emitter = kernel.emitter();
    let worker = thread::spawn(move || emitter.emit(Ping(1)));
    entered.wait();
    assert!(!kernel.is_quiescent());
    release.wait();
    worker.join().unwrap();
    settle(&kernel);
    shutdown(&kernel);
}

/// Barriers sent per run of the stress test below.
const BARRIER_ROUNDS: usize = 20_000;

/// Counts the barriers after which the kernel still reads busy, while one
/// spinning thread per core competes with the kernel thread for the CPU, so
/// it is often preempted right after it wakes the waiter.
fn busy_after_barrier(kernel: &crate::Kernel, rounds: usize) -> usize {
    let stop = Arc::new(AtomicBool::new(false));
    let cores = thread::available_parallelism().map_or(2, |n| n.get());
    let spinners: Vec<_> = (0..cores)
        .map(|_| {
            let stop = stop.clone();
            thread::spawn(move || {
                while !stop.load(SeqCst) {
                    std::hint::spin_loop();
                }
            })
        })
        .collect();
    let busy = (0..rounds)
        .filter(|_| {
            kernel.barrier();
            kernel.quiescent_epoch().is_none()
        })
        .count();
    stop.store(true, SeqCst);
    for spinner in spinners {
        spinner.join().unwrap();
    }
    busy
}

/// `barrier` answers only after its own request has left the busy count
/// (#96): with nothing else busy, the kernel is quiescent the moment
/// `barrier` returns, without polling.
#[test]
fn barrier_returns_after_its_own_request_leaves_the_busy_count() {
    let kernel = start(PluginSet::new().with(
        "p",
        handle_plugin(&HandleSlot::default(), after_first_frame()),
    ));
    wait_until("quiescent", || kernel.is_quiescent());
    let busy = busy_after_barrier(&kernel, BARRIER_ROUNDS);
    assert_eq!(
        busy, 0,
        "busy right after {busy} of {BARRIER_ROUNDS} barriers"
    );
    shutdown(&kernel);
}

/// The same holds for a barrier the kernel thread rejects because it was
/// queued behind `shutdown`.
#[test]
fn a_barrier_rejected_after_shutdown_also_answers_after_its_own_count() {
    let mut busy = 0;
    for _ in 0..200 {
        let kernel = start(PluginSet::new());
        wait_until("quiescent", || kernel.is_quiescent());
        let (done, finished) = std::sync::mpsc::channel();
        kernel.shutdown(move || {
            let _ = done.send(());
        });
        kernel.barrier();
        if kernel.quiescent_epoch().is_none() {
            busy += 1;
        }
        finished
            .recv_timeout(Duration::from_secs(10))
            .expect("shutdown finished");
    }
    assert_eq!(busy, 0, "busy right after {busy} of 200 rejected barriers");
}

#[test]
fn a_cancelled_task_still_counts_until_it_ends() {
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    settle(&kernel);
    let guard = slot.get().busy().unwrap();
    kernel.set_enabled("p", false);
    kernel.barrier();
    assert!(!kernel.is_quiescent(), "the task's guard still counts");
    drop(guard);
    settle(&kernel);
    shutdown(&kernel);
}

#[test]
fn transition_listeners_may_take_the_plugins_lock_without_deadlock() {
    let lock = Arc::new(Mutex::new(0u32));
    let l = lock.clone();
    let slot = HandleSlot::default();
    let s = slot.clone();
    let kernel = start(PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
            let l1 = l.clone();
            ctx.on(move |_: &Busy, _| *l1.lock().unwrap() += 1);
            let l2 = l.clone();
            ctx.on(move |_: &Quiescent, _| *l2.lock().unwrap() += 1);
            let s = s.clone();
            Ok(Box::new(OnStarted(Some(Box::new(move |h| s.set(h))))))
        }),
    ));
    settle(&kernel);
    {
        // Take and drop a guard while holding the lock the listeners need.
        let _held = lock.lock().unwrap();
        let guard = slot.get().busy().unwrap();
        drop(guard);
    }
    settle(&kernel);
    shutdown(&kernel);
}

#[test]
fn a_late_busy_event_is_recognisable_by_its_sequence_number() {
    let log = Log::default();
    let largest = Arc::new(AtomicU64::new(0));
    let l = log.clone();
    let (b, q) = (largest.clone(), largest.clone());
    let kernel = start(PluginSet::new().with(
        "bench",
        TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
            let (b, l1) = (b.clone(), l.clone());
            ctx.on(move |busy: &Busy, _| {
                if busy.seq > b.fetch_max(busy.seq, SeqCst) {
                    l1.push("busy");
                } else {
                    l1.push("stale busy ignored");
                }
            });
            let (q, l2) = (q.clone(), l.clone());
            ctx.on(move |quiet: &Quiescent, _| {
                q.fetch_max(quiet.seq, SeqCst);
                l2.push("quiescent");
            });
            Ok(Box::new(()))
        }),
    ));
    settle(&kernel);
    // Deliver an out-of-date Busy by hand: its seq is below the latest.
    let stale = largest.load(SeqCst).saturating_sub(1);
    kernel.emitter().emit(Busy { seq: stale });
    assert!(log.snapshot().contains(&"stale busy ignored".to_owned()));
    shutdown(&kernel);
}
