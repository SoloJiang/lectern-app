//! `run_dedicated` and `run_dedicated_gated`.

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use lectern_kernel::{PluginError, PluginState, SubmitGate};

use super::support::*;
use crate::{Config, Executor, Task, TaskError, ThreadKind};

/// An executor whose thread start hook reports dedicated threads.
fn hooked() -> (Executor, mpsc::Receiver<String>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let exec = Executor::new(Config {
        workers: 1,
        on_thread_start: Some(Arc::new(move |kind| {
            if kind == ThreadKind::Dedicated {
                let name = thread::current().name().unwrap_or("").to_owned();
                let _ = tx.lock().unwrap().send(name);
            }
        })),
    });
    (exec, rx)
}

#[test]
fn run_dedicated_runs_the_closure_on_its_own_named_thread() {
    let exec = executor(1);
    let task = exec.run_dedicated("pipe-reader", || {
        thread::current().name().map(str::to_owned)
    });
    assert_eq!(block_on(task), Ok(Some("pipe-reader".to_owned())));
}

#[test]
fn a_gated_dedicated_closure_waits_for_the_gate_then_runs() {
    let plugin = TestPlugin::start();
    let (exec, started) = hooked();
    let ran = Arc::new(AtomicBool::new(false));
    let ran_in_start = Arc::new(Mutex::new(None));
    let (done_tx, done) = mpsc::channel();
    let (r, seen) = (ran.clone(), ran_in_start.clone());
    assert!(plugin.handle.own_with(move |gate| {
        let r2 = r.clone();
        let task = exec.run_dedicated_gated(&gate, "gated", move || {
            r2.store(true, SeqCst);
            let _ = done_tx.send(thread::current().name().map(str::to_owned));
        });
        // The thread is up and checking the gate, which stays closed until
        // `start` returns.
        assert_eq!(recv(&started, "dedicated thread"), "gated");
        *seen.lock().unwrap() = Some(r.load(SeqCst));
        task
    }));
    assert_eq!(*ran_in_start.lock().unwrap(), Some(false));
    assert_eq!(recv(&done, "closure ran"), Some("gated".to_owned()));
    assert!(ran.load(SeqCst));
    assert_eq!(plugin.state(), PluginState::Active);
}

/// Submits a dedicated task inside `start`, keeps its handle out of the
/// kernel's hands, then runs `then` (still inside `start`).
fn submit_and(
    plugin: &TestPlugin,
    exec: &Executor,
    log: &Log,
    then: impl FnOnce() + Send + 'static,
) -> (bool, Task<()>, Arc<AtomicBool>) {
    let ran = Arc::new(AtomicBool::new(false));
    let stash = Arc::new(Mutex::new(None));
    let probe = DropProbe::new("closure", log);
    let (e, r, s) = (exec.clone(), ran.clone(), stash.clone());
    let accepted = plugin.handle.own_with(move |gate| {
        let task = e.run_dedicated_gated(&gate, "dropped-unrun", move || {
            let _probe = &probe;
            r.store(true, SeqCst);
        });
        *s.lock().unwrap() = Some(task);
        then();
    });
    let task = stash.lock().unwrap().take().unwrap();
    (accepted, task, ran)
}

#[test]
fn a_gated_dedicated_closure_is_dropped_unrun_when_the_gate_closes() {
    let plugin = TestPlugin::start();
    let exec = executor(1);
    let log = Log::default();
    let (accepted, task, ran) = submit_and(&plugin, &exec, &log, || panic!("start panicked"));
    assert!(!accepted);
    assert_eq!(block_on(task), Err(TaskError::Cancelled));
    assert!(!ran.load(SeqCst));
    assert_eq!(log.snapshot(), vec!["dropped closure on dropped-unrun"]);
    plugin.kernel.barrier();
    assert!(matches!(plugin.state(), PluginState::Failed(_)));
}

#[test]
fn a_gated_dedicated_closure_is_dropped_unrun_when_the_activation_dies() {
    let plugin = TestPlugin::start();
    let exec = executor(1);
    let log = Log::default();
    let handle = plugin.handle.clone();
    let (accepted, task, ran) = submit_and(&plugin, &exec, &log, move || {
        handle.fail(PluginError::new("died while starting"));
    });
    assert!(!accepted);
    assert_eq!(block_on(task), Err(TaskError::Cancelled));
    assert!(!ran.load(SeqCst));
    assert_eq!(log.snapshot(), vec!["dropped closure on dropped-unrun"]);
}

#[test]
fn a_panicking_gated_dedicated_closure_fails_the_plugin() {
    let plugin = TestPlugin::start();
    let exec = executor(1);
    let stash = Arc::new(Mutex::new(None));
    let s = stash.clone();
    assert!(plugin.handle.own_with(move |gate| {
        let task = exec.run_dedicated_gated(&gate, "panics", || panic!("dedicated closure"));
        *s.lock().unwrap() = Some(task);
    }));
    let task: Task<()> = stash.lock().unwrap().take().unwrap();
    assert_eq!(
        block_on(task),
        Err(TaskError::Panicked("dedicated closure".into()))
    );
    plugin.wait_failed("dedicated closure");
}

#[test]
fn run_dedicated_gated_on_a_sealed_gate_starts_no_thread() {
    let plugin = TestPlugin::start();
    let stash = Arc::new(Mutex::new(None::<SubmitGate>));
    let s = stash.clone();
    assert!(plugin.handle.own_with(move |gate| {
        *s.lock().unwrap() = Some(gate);
    }));
    let gate = stash.lock().unwrap().take().unwrap();
    let (exec, started) = hooked();
    let log = Log::default();
    let probe = DropProbe::new("closure", &log);
    let task = exec.run_dedicated_gated(&gate, "never", move || {
        let _probe = &probe;
    });
    assert!(task.is_finished());
    assert_eq!(block_on(task), Err(TaskError::Cancelled));
    let here = thread::current();
    assert_eq!(
        log.snapshot(),
        vec![format!(
            "dropped closure on {}",
            here.name().unwrap_or("unnamed")
        )]
    );
    // The hook would have reported a dedicated thread.
    drop(exec);
    assert!(started.recv().is_err());
}

#[test]
fn dropping_the_handle_of_a_waiting_dedicated_task_drops_its_closure() {
    let plugin = TestPlugin::start();
    let exec = executor(1);
    let log = Log::default();
    let ran = Arc::new(AtomicBool::new(false));
    let probe = DropProbe::new("closure", &log);
    let (e, r, l) = (exec.clone(), ran.clone(), log.clone());
    let (dropped_tx, dropped) = mpsc::channel();
    // The handle is dropped inside `start`, while the gate is still closed.
    assert!(plugin.handle.own_with(move |gate| {
        let task = e.run_dedicated_gated(&gate, "cancelled", move || {
            let _probe = &probe;
            r.store(true, SeqCst);
        });
        drop(task);
        wait_until("closure dropped", || !l.snapshot().is_empty());
        let _ = dropped_tx.send(());
    }));
    recv(&dropped, "closure dropped");
    assert!(!ran.load(SeqCst));
    assert_eq!(log.snapshot(), vec!["dropped closure on cancelled"]);
}
