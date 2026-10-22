//! Pool tasks: the state machine, the waker and the [`Task`] handle.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::SeqCst};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Wake, Waker};

use lectern_kernel::{GatePoll, GateSlot, catch};

use crate::Priority;
use crate::diag::{Phase, TaskScope, contain_task, task_panicked};
use crate::output::Output;
use crate::sched::{Inner, Job, TaskId};
use crate::util::lock;

const IDLE: u8 = 0;
const QUEUED: u8 = 1;
const RUNNING: u8 = 2;
const RUNNING_WOKEN: u8 = 3;
const DONE: u8 = 4;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Why a task produced no value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskError {
    /// The handle was dropped, the gate closed or the activation died, or the
    /// executor shut down before the task finished.
    Cancelled,
    /// The task panicked; the panic message.
    Panicked(String),
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskError::Cancelled => f.write_str("the task was cancelled"),
            TaskError::Panicked(message) => write!(f, "the task panicked: {message}"),
        }
    }
}

impl std::error::Error for TaskError {}

/// What a [`Task`] handle needs from its task.
pub(crate) trait Handle<T>: Send + Sync {
    fn output(&self) -> &Output<T>;
    fn cancel(self: Arc<Self>);
    /// The handle is going away: a result already stored is handed to the
    /// executor to drop, never dropped on the calling thread.
    fn abandon(&self);
}

/// Handle to a task. Dropping it cancels the task: it is not polled again and
/// its future is dropped the next time it comes up.
///
/// Await it (from another task) for the result.
#[must_use = "dropping a Task cancels it; call `detach` to let it run"]
pub struct Task<T> {
    handle: Option<Arc<dyn Handle<T>>>,
}

impl<T> Task<T> {
    pub(crate) fn new(handle: Arc<dyn Handle<T>>) -> Self {
        Task {
            handle: Some(handle),
        }
    }

    /// A handle that is already cancelled.
    pub(crate) fn cancelled() -> Self {
        Task { handle: None }
    }

    /// Gives up the handle without cancelling; the task runs to completion and
    /// its result is dropped. Plugins do not use this: their task handles are
    /// kept by the kernel.
    pub fn detach(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abandon();
        }
    }

    /// Whether the task has finished (completed, panicked or been cancelled
    /// and dropped).
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(|handle| handle.output().is_finished())
    }
}

impl<T> Future for Task<T> {
    type Output = Result<T, TaskError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &self.handle {
            Some(handle) => handle.output().poll(cx),
            None => Poll::Ready(Err(TaskError::Cancelled)),
        }
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abandon();
            handle.cancel();
        }
    }
}

impl<T> fmt::Debug for Task<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task")
            .field("finished", &self.is_finished())
            .finish()
    }
}

/// One pool task. Kept alive by the live registry until it is finished, so
/// its future is only ever dropped by the thread running it.
pub(crate) struct TaskCell<T> {
    /// The task number: the registry key and `task` in log records.
    id: TaskId,
    /// `IDLE` / `QUEUED` / `RUNNING` / `RUNNING_WOKEN` / `DONE`.
    state: AtomicU8,
    priority: Priority,
    cancelled: AtomicBool,
    future: Mutex<Option<BoxFuture<T>>>,
    /// Only for `spawn_gated`. Dropped after the task is `DONE`.
    slot: Mutex<Option<GateSlot>>,
    /// The log scope entered around each step and the final drop.
    scope: TaskScope,
    output: Output<T>,
    exec: Weak<Inner>,
}

impl<T: Send + 'static> TaskCell<T> {
    /// A new task in the `QUEUED` state; the caller queues it. `id` and
    /// `scope` come from `diag::next_task` and `TaskScope::new`.
    pub(crate) fn new(
        exec: &Arc<Inner>,
        id: TaskId,
        scope: TaskScope,
        priority: Priority,
        future: BoxFuture<T>,
        slot: Option<GateSlot>,
    ) -> Arc<Self> {
        Arc::new(TaskCell {
            id,
            state: AtomicU8::new(QUEUED),
            priority,
            cancelled: AtomicBool::new(false),
            future: Mutex::new(Some(future)),
            slot: Mutex::new(slot),
            scope,
            output: Output::new(),
            exec: Arc::downgrade(exec),
        })
    }

    /// `IDLE → QUEUED` queues the task; `RUNNING → RUNNING_WOKEN` only sets a
    /// flag for the worker; any other state is left alone. So waking never
    /// polls or drops the future on the waking thread.
    fn schedule(self: &Arc<Self>) {
        loop {
            let state = self.state.load(SeqCst);
            let next = match state {
                IDLE => QUEUED,
                RUNNING => RUNNING_WOKEN,
                _ => return,
            };
            if self
                .state
                .compare_exchange(state, next, SeqCst, SeqCst)
                .is_ok()
            {
                if next == QUEUED {
                    self.enqueue();
                }
                return;
            }
        }
    }

    fn enqueue(self: &Arc<Self>) {
        if let Some(exec) = self.exec.upgrade() {
            exec.enqueue(self.priority, self.clone());
        }
    }

    /// Polls once. Returns the result when the task is finished; the state is
    /// then already `DONE` and the future dropped.
    fn step(self: &Arc<Self>) -> Option<Result<T, TaskError>> {
        let waker = Waker::from(self.clone());
        let mut cx = Context::from_waker(&waker);
        let decision = match &*lock(&self.slot) {
            Some(slot) => slot.poll(&mut cx),
            None => GatePoll::Run,
        };
        if decision == GatePoll::Drop || self.cancelled.load(SeqCst) {
            self.drop_unfinished();
            return Some(Err(TaskError::Cancelled));
        }
        if decision == GatePoll::Wait {
            return None;
        }
        let Some(mut future) = lock(&self.future).take() else {
            self.state.store(DONE, SeqCst);
            return Some(Err(TaskError::Cancelled));
        };
        match catch(|| future.as_mut().poll(&mut cx)) {
            Ok(Poll::Pending) => {
                *lock(&self.future) = Some(future);
                None
            }
            Ok(Poll::Ready(value)) => {
                self.state.store(DONE, SeqCst);
                self.drop_future(future);
                Some(Ok(value))
            }
            Err(payload) => {
                // Fixed order: finished, future dropped, panic logged (it
                // reads the payload), payload disposed, then the gate is told.
                self.state.store(DONE, SeqCst);
                self.drop_future(future);
                task_panicked(Phase::Poll, &payload);
                let message = payload.dispose();
                self.fail_gate(message.clone());
                Some(Err(TaskError::Panicked(message)))
            }
        }
    }

    /// Marks the task `DONE` and drops its future, if it still has one.
    fn drop_unfinished(&self) {
        self.state.store(DONE, SeqCst);
        let future = lock(&self.future).take();
        if let Some(future) = future {
            self.drop_future(future);
        }
    }

    fn drop_future(&self, future: BoxFuture<T>) {
        if let Err(message) = contain_task(Phase::Drop, move || drop(future)) {
            self.fail_gate(message);
        }
    }

    /// A panic in a poll or a drop, already logged: a gated task fails its
    /// activation.
    fn fail_gate(&self, message: String) {
        if let Some(slot) = &*lock(&self.slot) {
            slot.panicked(message);
        }
    }

    /// The task is `DONE`: hand over the result, drop the gate slot, leave
    /// the registry.
    fn finish(&self, result: Result<T, TaskError>) {
        self.output.complete(result);
        let slot = lock(&self.slot).take();
        drop(slot);
        if let Some(exec) = self.exec.upgrade() {
            exec.finished(self.id);
        }
    }
}

impl<T: Send + 'static> Job for TaskCell<T> {
    fn run(self: Arc<Self>) {
        if self
            .state
            .compare_exchange(QUEUED, RUNNING, SeqCst, SeqCst)
            .is_err()
        {
            return;
        }
        // The task's context around the gate check, the poll, the final drop
        // and handing over the result: a pointer swap, no copy, no
        // allocation, no lock.
        self.scope.enter(self.id, || match self.step() {
            Some(result) => self.finish(result),
            None => {
                if self
                    .state
                    .compare_exchange(RUNNING, IDLE, SeqCst, SeqCst)
                    .is_err()
                {
                    // Woken while running: poll again.
                    self.state.store(QUEUED, SeqCst);
                    self.enqueue();
                }
            }
        });
    }

    fn cancel(self: Arc<Self>) {
        self.cancelled.store(true, SeqCst);
        self.schedule();
    }

    fn discard(self: Arc<Self>) {
        self.scope.enter(self.id, || {
            self.drop_unfinished();
            self.finish(Err(TaskError::Cancelled));
        });
    }
}

impl<T: Send + 'static> Handle<T> for TaskCell<T> {
    fn output(&self) -> &Output<T> {
        &self.output
    }

    fn cancel(self: Arc<Self>) {
        Job::cancel(self);
    }

    fn abandon(&self) {
        // A batch task's result is dropped on the batch pool, which is
        // already running; any other result on a worker.
        let priority = match self.priority {
            Priority::Batch => Priority::Batch,
            Priority::Interactive | Priority::Background => Priority::Background,
        };
        self.output
            .abandon(&self.exec, priority, &self.scope, self.id);
    }
}

impl<T: Send + 'static> Wake for TaskCell<T> {
    fn wake(self: Arc<Self>) {
        self.schedule();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.schedule();
    }
}
