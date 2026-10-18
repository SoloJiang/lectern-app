//! `own_with`, submit gates and the executor contract.

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use super::executor::{Executor, Latch, TaskHandle};
use super::support::*;
use crate::{PluginError, PluginSet, PluginState};

fn started_plugin() -> (Arc<crate::Kernel>, HandleSlot) {
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    (kernel, slot)
}

#[test]
fn tasks_run_after_the_handle_is_registered() {
    let (kernel, slot) = started_plugin();
    let executor = Executor::new();
    let log = Log::default();
    let l = log.clone();
    let accepted = slot.get().own_with(move |gate| {
        let l2 = l.clone();
        let handle = executor.spawn(&gate, async move { l2.push("task ran") });
        // The gate is not open yet, so the task has not run.
        thread::sleep(Duration::from_millis(20));
        l.push("start returns");
        handle
    });
    assert!(accepted);
    wait_until("task ran", || log.snapshot().len() == 2);
    assert_eq!(log.snapshot(), vec!["start returns", "task ran"]);
    shutdown(&kernel);
}

#[test]
fn a_panicking_start_drops_its_tasks_unrun_and_fails_the_plugin() {
    let (kernel, slot) = started_plugin();
    let executor = Executor::new();
    let polls = Arc::new(Mutex::new(None));
    let p = polls.clone();
    let accepted = slot.get().own_with(move |gate| {
        let handle = executor.spawn(&gate, async {});
        *p.lock().unwrap() = handle.task();
        std::mem::forget(handle);
        panic!("start");
    });
    assert!(!accepted);
    kernel.barrier();
    assert!(matches!(
        state(&kernel, "p"),
        PluginState::Failed(PluginError::Panicked(_))
    ));
    let task = polls.lock().unwrap().clone().unwrap();
    thread::sleep(Duration::from_millis(20));
    assert_eq!(task.polls.load(SeqCst), 0);
    shutdown(&kernel);
}

#[test]
fn a_task_that_panics_fails_the_plugin_and_the_worker_keeps_running() {
    let (kernel, slot) = started_plugin();
    let executor = Executor::new();
    let e = executor.clone();
    assert!(
        slot.get()
            .own_with(move |gate| e.spawn(&gate, async { panic!("task") }))
    );
    wait_until("plugin failed", || {
        matches!(state(&kernel, "p"), PluginState::Failed(_))
    });

    // The executor still runs other work.
    let (kernel2, slot2) = started_plugin();
    let log = Log::default();
    let l = log.clone();
    assert!(
        slot2
            .get()
            .own_with(move |gate| executor.spawn(&gate, async move { l.push("ok") }))
    );
    wait_until("second task", || log.snapshot() == vec!["ok"]);
    shutdown(&kernel);
    shutdown(&kernel2);
}

#[test]
fn suspended_tasks_are_dropped_when_the_activation_dies() {
    let (kernel, slot) = started_plugin();
    let executor = Executor::new();
    let latch = Latch::default();
    let wait = latch.wait();
    let busy = slot.get().busy().unwrap();
    let dropped = Log::default();
    let d = dropped.clone();
    assert!(slot.get().own_with(move |gate| {
        executor.spawn(&gate, async move {
            let _busy = busy;
            let _d = Guard("task state".into(), d);
            wait.await;
        })
    }));
    thread::sleep(Duration::from_millis(20));
    assert!(!kernel.is_quiescent());
    slot.get().fail(PluginError::new("stop"));
    wait_until("task dropped", || {
        dropped.snapshot() == vec!["drop guard task state"]
    });
    wait_until("quiescent", || kernel.is_quiescent());
    drop(latch);
    shutdown(&kernel);
}

#[test]
fn a_failure_while_start_runs_closes_the_gate_and_drops_the_handle_before_the_active_state() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let s = slot.clone();
    let l = log.clone();
    let kernel = start(PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |_| {
            let s = s.clone();
            Ok(Box::new(Probe::new("p", &l).started(move |h| s.set(h))))
        }),
    ));
    let executor = Executor::new();
    let in_start = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let (i, r) = (in_start.clone(), resume.clone());
    let handle = slot.get();
    let l = log.clone();
    let worker = thread::spawn(move || {
        handle.own_with(move |gate| {
            let l2 = l.clone();
            let task = executor.spawn(&gate, async move { l2.push("task ran") });
            i.wait();
            r.wait();
            (task, Guard("task handle".into(), l.clone()))
        })
    });
    in_start.wait();
    slot.get().fail(PluginError::new("concurrent failure"));
    resume.wait();
    let accepted = worker.join().unwrap();
    assert!(!accepted);
    kernel.barrier();
    let entries = log.snapshot();
    assert!(!entries.contains(&"task ran".to_owned()), "{entries:?}");
    let handle_at = entries
        .iter()
        .position(|e| e == "drop guard task handle")
        .unwrap();
    let active_at = entries.iter().position(|e| e == "drop p").unwrap();
    assert!(handle_at < active_at, "{entries:?}");
    shutdown(&kernel);
}

#[test]
fn own_with_inside_start_and_inside_a_task_does_not_deadlock() {
    let (kernel, slot) = started_plugin();
    let executor = Executor::new();
    let log = Log::default();
    let handle = slot.get();
    let (h, e, l) = (handle.clone(), executor.clone(), log.clone());
    assert!(handle.own_with(move |gate| {
        assert!(h.own(Guard("inner own".into(), l.clone())));
        let (h2, e2, l2) = (h.clone(), e.clone(), l.clone());
        assert!(h.own_with(move |_| l2.push("nested start")));
        e.spawn(&gate, async move {
            let l3 = l.clone();
            assert!(h2.own_with(move |_| l3.push("start from task")));
            drop(e2);
        })
    }));
    wait_until("task ran", || {
        log.snapshot().contains(&"start from task".to_owned())
    });
    assert!(log.snapshot().contains(&"nested start".to_owned()));
    shutdown(&kernel);
}

#[test]
fn finished_submissions_are_reclaimed() {
    let (kernel, slot) = started_plugin();
    let executor = Executor::new();
    let done = Arc::new(AtomicUsize::new(0));
    for _ in 0..2_000 {
        let e = executor.clone();
        let d = done.clone();
        assert!(slot.get().own_with(move |gate| e.spawn(&gate, async move {
            d.fetch_add(1, SeqCst);
        })));
    }
    // A submission that starts no task is reclaimed right away.
    assert!(slot.get().own_with(|_| ()));
    wait_until("tasks done", || done.load(SeqCst) == 2_000);
    kernel.barrier();
    wait_until("table empty", || {
        crate::util::lock(&slot.get().token().0.guards).len() == 0
    });
    shutdown(&kernel);
}

#[test]
fn a_slot_requested_after_start_returns_is_refused() {
    let (kernel, slot) = started_plugin();
    let executor = Executor::new();
    let gates = Arc::new(Mutex::new(None));
    let g = gates.clone();
    assert!(slot.get().own_with(move |gate| {
        *g.lock().unwrap() = Some(gate);
    }));
    let gate = gates.lock().unwrap().take().unwrap();
    let ran = Log::default();
    let r = ran.clone();
    let handle: TaskHandle = executor.spawn(&gate, async move { r.push("ran") });
    assert!(handle.task().is_none());
    thread::sleep(Duration::from_millis(20));
    assert!(ran.snapshot().is_empty());
    shutdown(&kernel);
}

#[test]
fn a_panicking_task_releases_its_payload_and_siblings_are_dropped() {
    use std::sync::atomic::AtomicBool;
    struct Permit(Arc<AtomicBool>);
    impl Drop for Permit {
        fn drop(&mut self) {
            self.0.store(false, SeqCst);
        }
    }
    let (kernel, slot) = started_plugin();
    let executor = Executor::new();
    let taken = Arc::new(AtomicBool::new(true));
    let permit = Permit(taken.clone());
    let dropped = Log::default();
    let d = dropped.clone();
    let latch = Latch::default();
    let wait = latch.wait();
    assert!(slot.get().own_with(move |gate| {
        // Moved into the future rather than created in its body: the
        // panicking task may kill the activation before the sibling is first
        // polled, and the sibling's future must be dropped either way.
        let held = Guard("sibling".into(), d);
        let sibling = executor.spawn(&gate, async move {
            let _held = held;
            wait.await;
        });
        let panicking = executor.spawn(&gate, async move {
            std::panic::panic_any(permit);
        });
        (sibling, panicking)
    }));
    wait_until("plugin failed", || {
        matches!(state(&kernel, "p"), PluginState::Failed(_))
    });
    wait_until("payload released", || !taken.load(SeqCst));
    wait_until("sibling dropped", || {
        dropped.snapshot() == vec!["drop guard sibling"]
    });
    drop(latch);
    shutdown(&kernel);
}
