//! Panics in gated tasks fail the plugin and never reach the worker.

use std::future::poll_fn;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::task::Poll;
use std::time::Instant;

use lectern_kernel::{PluginError, PluginState, TokenState};

use super::support::*;
use crate::{Executor, Priority, Shutdown, TaskError};

#[test]
fn a_panicking_gated_task_fails_the_plugin_and_the_worker_keeps_running() {
    let plugin = TestPlugin::start();
    let exec = executor(1);
    let e = exec.clone();
    assert!(plugin.handle.own_with(move |gate| {
        e.spawn_gated(&gate, Priority::Background, async {
            panic!("gated task");
        })
    }));
    plugin.wait_failed("gated task");

    // The single worker survived the panic.
    assert_eq!(block_on(exec.run(Priority::Background, || 7)), Ok(7));
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

/// A resource held by the future, which the panic payload's `Drop` waits for.
#[derive(Clone, Default)]
struct Resource(Arc<(Mutex<bool>, Condvar)>);

impl Resource {
    fn acquire(&self) -> Held {
        *self.0.0.lock().unwrap() = true;
        Held(self.clone())
    }

    /// Waits until the resource is released; false after `LIMIT`.
    fn wait_free(&self) -> bool {
        let deadline = Instant::now() + LIMIT;
        let mut held = self.0.0.lock().unwrap();
        while *held {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            held = self.0.1.wait_timeout(held, deadline - now).unwrap().0;
        }
        true
    }
}

struct Held(Resource);

impl Drop for Held {
    fn drop(&mut self) {
        *self.0.0.0.lock().unwrap() = false;
        self.0.0.1.notify_all();
    }
}

/// A `panic_any` payload whose `Drop` waits for the future's resource.
struct Payload {
    resource: Resource,
    log: Log,
    done: mpsc::Sender<()>,
}

impl Drop for Payload {
    fn drop(&mut self) {
        let freed = self.resource.wait_free();
        self.log
            .push(format!("payload dropped, resource free: {freed}"));
        let _ = self.done.send(());
    }
}

/// Logs whether the activation was already dead when the future was dropped.
struct FutureProbe {
    log: Log,
    token: lectern_kernel::ActivationToken,
}

impl Drop for FutureProbe {
    fn drop(&mut self) {
        let dead = self.token.state() == TokenState::Dead;
        self.log
            .push(format!("future dropped, activation dead: {dead}"));
    }
}

#[test]
fn a_panicking_task_drops_its_future_before_the_payload_and_reports_last() {
    let plugin = TestPlugin::start();
    let exec = executor(1);
    let log = Log::default();
    let resource = Resource::default();
    let (done_tx, done) = mpsc::channel();
    let (e, l, r) = (exec.clone(), log.clone(), resource.clone());
    let token = plugin.handle.token();
    assert!(plugin.handle.own_with(move |gate| {
        let held = r.acquire();
        let probe = FutureProbe {
            log: l.clone(),
            token,
        };
        e.spawn_gated(
            &gate,
            Priority::Background,
            poll_fn(move |_| -> Poll<()> {
                // Owned by the future, not the stack: unwinding the poll does
                // not release it.
                let _held = &held;
                let _probe = &probe;
                std::panic::panic_any(Payload {
                    resource: r.clone(),
                    log: l.clone(),
                    done: done_tx.clone(),
                })
            }),
        )
    }));
    recv(&done, "payload dropped");
    plugin.wait_failed("non-string payload");
    assert_eq!(
        log.snapshot(),
        vec![
            "future dropped, activation dead: false",
            "payload dropped, resource free: true",
        ]
    );
    // The worker was not held up by the payload.
    assert_eq!(block_on(exec.run(Priority::Background, || 8)), Ok(8));
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn a_panic_dropping_a_finished_future_fails_the_plugin() {
    let plugin = TestPlugin::start();
    let exec = Executor::manual();
    let e = exec.clone();
    let probe = PanicOnDrop;
    assert!(plugin.handle.own_with(move |gate| {
        // The closure keeps `probe` until the future itself is dropped.
        e.spawn_gated(
            &gate,
            Priority::Interactive,
            poll_fn(move |_| {
                let _probe = &probe;
                Poll::Ready(1)
            }),
        )
    }));
    assert_eq!(exec.run_until_idle(), 1);
    plugin.kernel.barrier();
    assert!(matches!(
        plugin.state(),
        PluginState::Failed(PluginError::Panicked(message)) if message.contains("drop panicked")
    ));
}

#[test]
fn a_panic_dropping_a_cancelled_future_fails_the_plugin() {
    let plugin = TestPlugin::start();
    let exec = Executor::manual();
    let e = exec.clone();
    let probe = PanicOnDrop;
    let stash = Arc::new(Mutex::new(None));
    let s = stash.clone();
    assert!(plugin.handle.own_with(move |gate| {
        let task = e.spawn_gated(&gate, Priority::Interactive, async move {
            let _probe = probe;
            std::future::pending::<()>().await;
        });
        *s.lock().unwrap() = Some(task);
    }));
    assert_eq!(exec.run_until_idle(), 1);
    let task = stash.lock().unwrap().take().unwrap();
    drop(task);
    assert_eq!(exec.run_until_idle(), 1);
    plugin.kernel.barrier();
    assert!(matches!(
        plugin.state(),
        PluginState::Failed(PluginError::Panicked(message)) if message.contains("drop panicked")
    ));
}

#[test]
fn a_gated_task_that_panics_reports_panicked_to_an_awaiting_task() {
    let plugin = TestPlugin::start();
    let exec = Executor::manual();
    let stash = Arc::new(Mutex::new(None));
    let (e, s) = (exec.clone(), stash.clone());
    assert!(plugin.handle.own_with(move |gate| {
        let task = e.spawn_gated(&gate, Priority::Interactive, async {
            panic!("awaited");
        });
        *s.lock().unwrap() = Some(task);
    }));
    let task: crate::Task<()> = stash.lock().unwrap().take().unwrap();
    exec.run_until_idle();
    assert_eq!(block_on(task), Err(TaskError::Panicked("awaited".into())));
    plugin.kernel.barrier();
    assert!(matches!(plugin.state(), PluginState::Failed(_)));
}
