//! Background tasks for IO and parsing off the UI thread.
//!
//! A fixed pool of worker threads runs futures and blocking closures from two
//! priority queues; a smaller batch pool runs pausable background work
//! (`Priority::Batch`, ADR 0029) from a queue of its own; long-blocking work
//! gets a thread of its own. Plugins
//! submit through the kernel's [`SubmitGate`], so their tasks are dropped when
//! the activation dies and a panic fails the plugin. The design is in
//! `docs/design/executor.md`.

mod dedicated;
mod diag;
mod hooks;
mod output;
mod sched;
mod task;
mod util;

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use lectern_kernel::{GateSlot, SubmitGate};
use lectern_logging::{Value, error};

use crate::dedicated::DedicatedCell;
use crate::diag::{Phase, contain_task, next_task, task_ctx};
use crate::sched::{Inner, Job};
use crate::task::TaskCell;

pub use crate::hooks::ShutdownHook;
pub use crate::task::{Task, TaskError};

type Hook = Arc<dyn Fn(ThreadKind) + Send + Sync>;

/// The background executor. `Clone + Send + Sync`; `app` creates one after the
/// startup barrier is released and hands it to the services that need it.
///
/// Creating it starts no thread: the workers start on the first
/// `Interactive` or `Background` submission and the batch threads on the
/// first `Batch` submission (ADR 0017: nothing is submitted before the first
/// frame; ADR 0029).
#[derive(Clone)]
pub struct Executor {
    inner: Arc<Inner>,
}

/// Executor settings.
pub struct Config {
    /// Number of worker threads. Defaults to
    /// `clamp(available_parallelism - 1, 2, 6)`. 0 counts as 1; with a single
    /// worker no thread is reserved for `Interactive` tasks.
    pub workers: usize,
    /// Number of batch threads, which run only `Priority::Batch` tasks.
    /// Defaults to `clamp(available_parallelism / 4, 1, 3)`; 0 counts as 1
    /// (ADR 0029).
    pub batch_workers: usize,
    /// Called on every worker, batch and dedicated thread when it starts, on
    /// that thread. `app` uses it to set the QoS class from `platform`:
    /// utility for `ThreadKind::Batch`, unchanged for the others. A panic is
    /// logged and the thread works as usual.
    pub on_thread_start: Option<Arc<dyn Fn(ThreadKind) + Send + Sync>>,
}

impl Default for Config {
    fn default() -> Self {
        Config::for_parallelism(thread::available_parallelism().map_or(1, |n| n.get()))
    }
}

impl Config {
    /// The default settings for a machine with `parallelism` hardware
    /// threads.
    fn for_parallelism(parallelism: usize) -> Config {
        Config {
            workers: parallelism.saturating_sub(1).clamp(2, 6),
            batch_workers: (parallelism / 4).clamp(1, 3),
            on_thread_start: None,
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("workers", &self.workers)
            .field("batch_workers", &self.batch_workers)
            .field("on_thread_start", &self.on_thread_start.is_some())
            .finish()
    }
}

/// Which kind of thread [`Config::on_thread_start`] is called on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadKind {
    /// A worker thread (`executor-N`): `Interactive` and `Background` tasks.
    Worker,
    /// A batch thread (`executor-batch-N`): `Batch` tasks only.
    Batch,
    /// A dedicated thread, named by the caller.
    Dedicated,
}

/// Task priority.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    /// A result the user is waiting for: the diff of the current review
    /// session, an opened file. One worker only takes these.
    Interactive,
    /// Scans, refreshes, and indexing that holds a `BusyGuard`.
    Background,
    /// Pausable long-running batch work (ADR 0029). Runs only on the batch
    /// threads, which start on the first `Batch` submission; never queued
    /// with `Interactive` or `Background` and never on a worker.
    Batch,
}

/// Result of [`Executor::shutdown`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shutdown {
    /// Every worker and batch thread exited.
    Clean,
    /// The timeout passed with this many worker and batch threads still
    /// running (a task did not finish).
    TimedOut { busy_workers: usize },
}

impl Executor {
    pub fn new(config: Config) -> Executor {
        Executor {
            inner: Inner::new(
                config.workers,
                config.batch_workers,
                config.on_thread_start,
                false,
            ),
        }
    }

    /// Submits a future. Callable from any thread; never blocks.
    pub fn spawn<T: Send + 'static>(
        &self,
        priority: Priority,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        self.submit(priority, Box::pin(future), None, None)
    }

    /// For plugins submitting through `PluginHandle::own_with`. Takes a
    /// `GateSlot` inside this call; when the gate is sealed the future is not
    /// submitted but dropped here under `contain` (a panic is reported with
    /// `SubmitGate::panicked`) and the handle is already cancelled.
    pub fn spawn_gated<T: Send + 'static>(
        &self,
        gate: &SubmitGate,
        priority: Priority,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        match gate.slot() {
            Some(slot) => self.submit(priority, Box::pin(future), Some(slot), Some(gate)),
            None => {
                reject(gate, future);
                Task::cancelled()
            }
        }
    }

    /// Runs a blocking closure as a task; same as
    /// `spawn(priority, async move { f() })`.
    pub fn run<T: Send + 'static>(
        &self,
        priority: Priority,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Task<T> {
        self.spawn(priority, async move { f() })
    }

    /// [`run`](Self::run) through a submit gate, as [`spawn_gated`](Self::spawn_gated).
    pub fn run_gated<T: Send + 'static>(
        &self,
        gate: &SubmitGate,
        priority: Priority,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Task<T> {
        self.spawn_gated(gate, priority, async move { f() })
    }

    /// Runs a closure that may block for a long time (waiting for a child
    /// process, reading a pipe) on a new thread named `name`. The thread exits
    /// when the closure returns.
    pub fn run_dedicated<T: Send + 'static>(
        &self,
        name: &str,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Task<T> {
        self.dedicated(name, Box::new(f), None, None)
    }

    /// For plugins submitting long-blocking work through `own_with` (the
    /// jdtls pipe reader, a file watcher). Takes a `GateSlot` inside this
    /// call; when the gate is sealed no thread is started, the closure is
    /// dropped here under `contain` and the handle is already cancelled. The
    /// thread waits for the gate to open before running the closure; if the
    /// gate closes, the activation dies or the task is cancelled first, the
    /// closure is dropped unrun.
    pub fn run_dedicated_gated<T: Send + 'static>(
        &self,
        gate: &SubmitGate,
        name: &str,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Task<T> {
        match gate.slot() {
            Some(slot) => self.dedicated(name, Box::new(f), Some(slot), Some(gate)),
            None => {
                reject(gate, f);
                Task::cancelled()
            }
        }
    }

    /// Stops accepting tasks (later submissions return cancelled handles) and
    /// cancels every unfinished task in the live registry, suspended ones
    /// included; their futures are dropped by the worker and batch threads
    /// under `contain`, not on this thread. Waits up to `timeout` for those
    /// threads to exit.
    /// Dedicated threads are not waited for: one still waiting for its gate
    /// is woken and drops its closure; a running closure exits its thread
    /// when it returns.
    ///
    /// The first thing it does is run the callbacks registered with
    /// [`on_shutdown`](Self::on_shutdown).
    pub fn shutdown(&self, timeout: Duration) -> Shutdown {
        self.inner.shutdown(timeout)
    }

    /// Registers `f` to run when [`shutdown`](Self::shutdown) starts: on the
    /// thread calling `shutdown`, before any task is cancelled or thread
    /// joined, in registration order, each once and under `contain` (a panic
    /// is logged; the other callbacks and the shutdown go on). Callbacks
    /// must not block: they only signal work that does not belong to the
    /// pools, such as a dedicated thread's loop, to stop.
    ///
    /// Dropping the returned hook unregisters `f`, which is then dropped
    /// unrun. If shutdown has already started, `f` runs right here on the
    /// calling thread and the hook is inert, so `f` runs exactly once either
    /// way; the caller must not hold a lock `f` takes.
    pub fn on_shutdown(&self, f: Box<dyn FnOnce() + Send>) -> ShutdownHook {
        self.inner.shutdown_hooks.register(f)
    }

    /// `gate` is the gate `slot` came from: its `plugin` and `act` go into
    /// the task's log scope.
    fn submit<T: Send + 'static>(
        &self,
        priority: Priority,
        future: std::pin::Pin<Box<dyn Future<Output = T> + Send>>,
        slot: Option<GateSlot>,
        gate: Option<&SubmitGate>,
    ) -> Task<T> {
        let id = next_task();
        let ctx = task_ctx(id, gate);
        let cell = TaskCell::new(&self.inner, id, ctx, priority, future, slot);
        if self.inner.submit(id, priority, cell.clone()) {
            Task::new(cell)
        } else {
            Task::cancelled()
        }
    }

    fn dedicated<T: Send + 'static>(
        &self,
        name: &str,
        f: Box<dyn FnOnce() -> T + Send>,
        slot: Option<GateSlot>,
        gate: Option<&SubmitGate>,
    ) -> Task<T> {
        let id = next_task();
        let ctx = task_ctx(id, gate);
        let cell = DedicatedCell::new(&self.inner, id, ctx, f, slot);
        if !self.inner.register_dedicated(id, cell.clone()) {
            return Task::cancelled();
        }
        let body = cell.clone();
        let spawned = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || body.run());
        if let Err(error) = spawned {
            {
                // The thread name is the caller's string: not logged.
                let _scope = lectern_logging::scope(ctx);
                error!("could not start a dedicated thread"; error = Value::io(&error));
            }
            cell.clone().discard();
        }
        Task::new(cell)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Executor {
    /// An executor that starts no thread: tasks run only inside
    /// [`run_until_idle`](Self::run_until_idle), on the calling thread, in a
    /// fixed order. `shutdown` drops the cancelled tasks on its calling
    /// thread. Dedicated tasks still get their own threads.
    pub fn manual() -> Executor {
        Executor {
            inner: Inner::new(1, 1, None, true),
        }
    }

    /// Runs queued tasks until the queues are empty, `Interactive` before
    /// `Background` before `Batch` and each queue in submission order.
    /// Returns the number of steps run.
    pub fn run_until_idle(&self) -> usize {
        self.inner.run_until_idle()
    }
}

impl fmt::Debug for Executor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Executor").finish_non_exhaustive()
    }
}

/// Drops work refused by a sealed gate on the calling thread, under `catch`
/// and the gate's `plugin` / `act` scope (the work never became a task, so
/// there is no `task`); a panic is logged, then reported to the gate.
fn reject(gate: &SubmitGate, work: impl Send + 'static) {
    let _scope = lectern_logging::scope(gate.log_ctx());
    if let Err(message) = contain_task(Phase::Drop, move || drop(work)) {
        gate.panicked(message);
    }
}

#[cfg(test)]
mod tests;
