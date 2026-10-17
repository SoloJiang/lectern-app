//! Background tasks for IO and parsing off the UI thread.
//!
//! A fixed pool of worker threads runs futures and blocking closures from two
//! priority queues; long-blocking work gets a thread of its own. Plugins
//! submit through the kernel's [`SubmitGate`], so their tasks are dropped when
//! the activation dies and a panic fails the plugin. The design is in
//! `docs/design/executor.md`.

mod dedicated;
mod output;
mod sched;
mod task;
mod util;

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use lectern_kernel::{GateSlot, SubmitGate, contain};

use crate::dedicated::DedicatedCell;
use crate::sched::{Inner, Job};
use crate::task::TaskCell;
use crate::util::log;

pub use crate::task::{Task, TaskError};

type Hook = Arc<dyn Fn(ThreadKind) + Send + Sync>;

/// The background executor. `Clone + Send + Sync`; `app` creates one after the
/// startup barrier is released and hands it to the services that need it.
///
/// Creating it starts no thread: the workers start on the first submission
/// (ADR 0017: nothing is submitted before the first frame).
#[derive(Clone)]
pub struct Executor {
    inner: Arc<Inner>,
}

/// Executor settings.
pub struct Config {
    /// Number of worker threads. Defaults to
    /// `clamp(available_parallelism - 1, 2, 6)`.
    pub workers: usize,
    /// Called on every worker and dedicated thread when it starts, on that
    /// thread. `app` uses it to set the QoS class from `platform`. A panic is
    /// logged and the thread works as usual.
    pub on_thread_start: Option<Arc<dyn Fn(ThreadKind) + Send + Sync>>,
}

impl Default for Config {
    fn default() -> Self {
        let parallelism = thread::available_parallelism().map_or(1, |n| n.get());
        Config {
            workers: parallelism.saturating_sub(1).clamp(2, 6),
            on_thread_start: None,
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("workers", &self.workers)
            .field("on_thread_start", &self.on_thread_start.is_some())
            .finish()
    }
}

/// Which kind of thread [`Config::on_thread_start`] is called on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadKind {
    Worker,
    Dedicated,
}

/// Task priority.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    /// A result the user is waiting for: the diff of the current review
    /// session, an opened file. One worker only takes these.
    Interactive,
    /// Scans, indexing, refreshes.
    Background,
}

/// Result of [`Executor::shutdown`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shutdown {
    /// Every worker exited.
    Clean,
    /// The timeout passed with this many workers still running (a task did
    /// not finish).
    TimedOut { busy_workers: usize },
}

impl Executor {
    pub fn new(config: Config) -> Executor {
        Executor {
            inner: Inner::new(config.workers, config.on_thread_start, false),
        }
    }

    /// Submits a future. Callable from any thread; never blocks.
    pub fn spawn<T: Send + 'static>(
        &self,
        priority: Priority,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        self.submit(priority, Box::pin(future), None)
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
            Some(slot) => self.submit(priority, Box::pin(future), Some(slot)),
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
        self.dedicated(name, Box::new(f), None)
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
            Some(slot) => self.dedicated(name, Box::new(f), Some(slot)),
            None => {
                reject(gate, f);
                Task::cancelled()
            }
        }
    }

    /// Stops accepting tasks (later submissions return cancelled handles) and
    /// cancels every unfinished task in the live registry, suspended ones
    /// included; their futures are dropped by the workers under `contain`,
    /// not on this thread. Waits up to `timeout` for the workers to exit.
    /// Dedicated threads are not waited for: one still waiting for its gate
    /// is woken and drops its closure; a running closure exits its thread
    /// when it returns.
    pub fn shutdown(&self, timeout: Duration) -> Shutdown {
        self.inner.shutdown(timeout)
    }

    fn submit<T: Send + 'static>(
        &self,
        priority: Priority,
        future: std::pin::Pin<Box<dyn Future<Output = T> + Send>>,
        slot: Option<GateSlot>,
    ) -> Task<T> {
        let cell = TaskCell::new(&self.inner, priority, future, slot);
        if self.inner.submit(cell.id(), priority, cell.clone()) {
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
    ) -> Task<T> {
        let cell = DedicatedCell::new(&self.inner, f, slot);
        if !self.inner.register_dedicated(cell.id(), cell.clone()) {
            return Task::cancelled();
        }
        let body = cell.clone();
        let spawned = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || body.run());
        if let Err(error) = spawned {
            log(format_args!(
                "could not start dedicated thread {name:?}: {error}"
            ));
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
            inner: Inner::new(1, None, true),
        }
    }

    /// Runs queued tasks until the queues are empty, `Interactive` before
    /// `Background` and each queue in submission order. Returns the number of
    /// steps run.
    pub fn run_until_idle(&self) -> usize {
        self.inner.run_until_idle()
    }
}

impl fmt::Debug for Executor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Executor").finish_non_exhaustive()
    }
}

/// Drops work refused by a sealed gate on the calling thread, under `contain`.
fn reject(gate: &SubmitGate, work: impl Send + 'static) {
    if let Err(message) = contain(move || drop(work)) {
        gate.panicked(message);
    }
}

#[cfg(test)]
mod tests;
