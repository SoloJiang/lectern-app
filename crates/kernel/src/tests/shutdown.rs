//! Shutdown.

use std::sync::mpsc;
use std::time::Duration;

use super::support::*;
use crate::{PluginSet, service};

#[test]
fn shutdown_revokes_in_reverse_dependency_order_then_calls_done() {
    let log = Log::default();
    let kernel = start(
        PluginSet::new()
            .with(
                "a",
                chain_plugin(
                    "a",
                    &log,
                    vec![],
                    vec![service::<dyn SvcB>()],
                    after_first_frame(),
                ),
            )
            .with(
                "b",
                chain_plugin(
                    "b",
                    &log,
                    vec![service::<dyn SvcB>()],
                    vec![service::<dyn SvcC>()],
                    after_first_frame(),
                ),
            )
            .with(
                "c",
                chain_plugin(
                    "c",
                    &log,
                    vec![service::<dyn SvcC>()],
                    vec![],
                    after_first_frame(),
                ),
            ),
    );
    log.take();
    let l = log.clone();
    let (done, wait) = mpsc::channel();
    kernel.shutdown(move || {
        l.push("done");
        let _ = done.send(());
    });
    wait.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(log.snapshot(), vec!["drop a", "drop b", "drop c", "done"]);
}

#[test]
fn after_shutdown_pending_is_closed_and_rejected_guards_drop_on_the_caller() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    let handle = slot.get();
    let token = handle.token();
    let pending = token.pending().unwrap();
    let (done, wait) = mpsc::channel();
    kernel.shutdown(move || {
        let _ = done.send(());
    });
    // Shutdown waits for queued main-thread work before closing pending().
    std::thread::sleep(Duration::from_millis(50));
    drop(pending);
    wait.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(token.pending().is_none());

    struct Where(Log);
    impl Drop for Where {
        fn drop(&mut self) {
            self.0
                .push(std::thread::current().name().unwrap_or("?").to_owned());
        }
    }
    let caller = std::thread::Builder::new()
        .name("caller".into())
        .spawn(move || {
            assert!(!handle.own(Where(log.clone())));
            log.snapshot()
        })
        .unwrap();
    assert_eq!(caller.join().unwrap(), vec!["caller"]);
    // Requests after shutdown are answered on the calling thread.
    let (done, wait) = mpsc::channel();
    kernel.command("anything", move |result| {
        let _ = done.send(result);
    });
    assert_eq!(
        wait.recv().unwrap().err(),
        Some(crate::PluginError::ShuttingDown)
    );
}

#[test]
fn dropping_the_kernel_shuts_it_down() {
    let log = Log::default();
    let kernel = start(PluginSet::new().with(
        "p",
        chain_plugin("p", &log, vec![], vec![], after_first_frame()),
    ));
    drop(kernel);
    wait_until("revoked", || log.snapshot().contains(&"drop p".to_owned()));
}
