//! Integration with the kernel: `PluginHandle::own_with` + `spawn_gated`.

use std::sync::mpsc;

use lectern_kernel::PluginState;

use super::support::*;
use crate::{Priority, Shutdown};

#[test]
fn tasks_submitted_through_own_with_run_after_start_returns() {
    let plugin = TestPlugin::start();
    let exec = executor(2);
    let log = Log::default();
    let (done_tx, done) = mpsc::channel();
    let (e, l) = (exec.clone(), log.clone());
    let busy = plugin.handle.busy().unwrap();
    assert!(plugin.handle.own_with(move |gate| {
        let l2 = l.clone();
        let task = e.spawn_gated(&gate, Priority::Interactive, async move {
            let _busy = busy;
            l2.push("task ran");
            let _ = done_tx.send(());
        });
        l.push("start returns");
        task
    }));
    recv(&done, "task ran");
    assert_eq!(log.snapshot(), vec!["start returns", "task ran"]);
    // The busy guard went with the task; the kernel settles.
    wait_until("quiescent", || plugin.kernel.is_quiescent());
    assert_eq!(plugin.state(), PluginState::Active);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn deactivating_the_plugin_drops_its_pending_tasks_on_a_worker() {
    let plugin = TestPlugin::start();
    let exec = executor(2);
    let log = Log::default();
    let (polled_tx, polled) = mpsc::channel();
    let probe = DropProbe::new("task", &log);
    let e = exec.clone();
    let busy = plugin.handle.busy().unwrap();
    assert!(plugin.handle.own_with(move |gate| {
        e.spawn_gated(&gate, Priority::Background, async move {
            let _probe = probe;
            let _busy = busy;
            let _ = polled_tx.send(());
            std::future::pending::<()>().await;
        })
    }));
    recv(&polled, "task polled");
    assert!(!plugin.kernel.is_quiescent());

    // Revocation drops the kernel's task handle, which cancels the task.
    plugin.kernel.set_enabled("p", false);
    plugin.kernel.barrier();
    wait_until("task dropped", || !log.snapshot().is_empty());
    let entry = &log.snapshot()[0];
    assert!(entry.starts_with("dropped task on executor-"), "{entry}");
    wait_until("quiescent", || plugin.kernel.is_quiescent());
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}
