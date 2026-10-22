//! `spawn_gated` against the real kernel's `SubmitGate`.

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, mpsc};

use lectern_kernel::{PluginError, PluginState, SubmitGate};

use super::support::*;
use crate::{Executor, Priority, Shutdown, TaskError};

#[test]
fn a_gated_task_does_not_run_before_the_gate_opens() {
    let plugin = TestPlugin::start();
    let exec = Executor::manual();
    let ran = Arc::new(AtomicBool::new(false));
    let seen_in_start = Arc::new(Mutex::new(None));
    let (e, r, seen) = (exec.clone(), ran.clone(), seen_in_start.clone());
    assert!(plugin.handle.own_with(move |gate| {
        let r2 = r.clone();
        let task = e.spawn_gated(&gate, Priority::Interactive, async move {
            r2.store(true, SeqCst);
        });
        // The gate opens only after `start` returns: the step only waits.
        let steps = e.run_until_idle();
        *seen.lock().unwrap() = Some((steps, r.load(SeqCst)));
        task
    }));
    assert_eq!(*seen_in_start.lock().unwrap(), Some((1, false)));
    // Opening the gate woke the task.
    assert_eq!(exec.run_until_idle(), 1);
    assert!(ran.load(SeqCst));
}

#[test]
fn a_gate_that_opens_while_the_task_checks_it_still_schedules_the_task() {
    let plugin = TestPlugin::start();
    let exec = executor(4);
    let (ran_tx, ran) = mpsc::channel();
    // The worker's first check races with the gate opening right after
    // `start` returns; whichever comes first, every task runs.
    for index in 0..300 {
        let (e, tx) = (exec.clone(), ran_tx.clone());
        assert!(plugin.handle.own_with(move |gate| {
            e.spawn_gated(&gate, Priority::Background, async move {
                let _ = tx.send(index);
            })
        }));
    }
    let mut seen: Vec<_> = (0..300).map(|_| recv(&ran, "gated task")).collect();
    seen.sort_unstable();
    assert_eq!(seen, (0..300).collect::<Vec<_>>());
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn suspended_gated_tasks_are_dropped_when_the_activation_dies() {
    let plugin = TestPlugin::start();
    let exec = Executor::manual();
    let log = Log::default();
    let probe = DropProbe::new("future", &log);
    let e = exec.clone();
    assert!(plugin.handle.own_with(move |gate| {
        e.spawn_gated(&gate, Priority::Background, async move {
            let _probe = probe;
            std::future::pending::<()>().await;
        })
    }));
    assert_eq!(exec.run_until_idle(), 1);
    assert!(log.snapshot().is_empty());

    // The kernel wakes the slot; the next step finds the activation dead.
    plugin.handle.fail(PluginError::new("stop"));
    assert_eq!(exec.run_until_idle(), 1);
    let main = std::thread::current();
    assert_eq!(
        log.snapshot(),
        vec![format!(
            "dropped future on {}",
            main.name().unwrap_or("unnamed")
        )]
    );
    plugin.kernel.barrier();
    assert_eq!(
        plugin.state(),
        PluginState::Failed(PluginError::new("stop"))
    );
}

/// Calls `own_with` with a `start` that hands its gate out and returns, so
/// the gate is sealed.
pub(super) fn sealed_gate(plugin: &TestPlugin) -> SubmitGate {
    let stash = Arc::new(Mutex::new(None));
    let s = stash.clone();
    assert!(plugin.handle.own_with(move |gate| {
        *s.lock().unwrap() = Some(gate);
    }));
    stash.lock().unwrap().take().unwrap()
}

#[test]
fn spawn_gated_on_a_sealed_gate_returns_a_cancelled_handle_without_polling() {
    let plugin = TestPlugin::start();
    let gate = sealed_gate(&plugin);
    let exec = Executor::manual();
    let polled = Arc::new(AtomicBool::new(false));
    let log = Log::default();
    let probe = DropProbe::new("future", &log);
    let p = polled.clone();
    let task = exec.spawn_gated(&gate, Priority::Interactive, async move {
        let _probe = probe;
        p.store(true, SeqCst);
    });
    assert!(task.is_finished());
    // Dropped right away on the calling thread.
    assert_eq!(log.snapshot().len(), 1);
    assert_eq!(block_on(task), Err(TaskError::Cancelled));
    assert_eq!(exec.run_until_idle(), 0);
    assert!(!polled.load(SeqCst));
    assert_eq!(plugin.state(), PluginState::Active);
}

#[test]
fn a_panic_dropping_a_future_refused_by_a_sealed_gate_fails_the_plugin() {
    let plugin = TestPlugin::start();
    let gate = sealed_gate(&plugin);
    let exec = Executor::manual();
    let probe = PanicOnDrop;
    let task = exec.spawn_gated(&gate, Priority::Interactive, async move {
        let _probe = probe;
    });
    assert!(task.is_finished());
    plugin.kernel.barrier();
    assert!(matches!(
        plugin.state(),
        PluginState::Failed(PluginError::Panicked(message)) if message.contains("drop panicked")
    ));
}
