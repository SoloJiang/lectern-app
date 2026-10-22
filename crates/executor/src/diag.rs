//! Logging helpers (`docs/design/executor.md`, 日志与诊断): task numbers,
//! the task scope and the panic records.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use lectern_kernel::{PanicPayload, SubmitGate, catch};
use lectern_logging::{Ctx, Field, PluginTag, Value, error};

use crate::ThreadKind;

/// The next task number, process-wide from 1. Pool tasks, dedicated tasks
/// and the jobs that drop unread results all take one, so the numbers a log
/// shows for tasks increase but are not consecutive.
static NEXT_TASK: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_task() -> u64 {
    NEXT_TASK.fetch_add(1, Relaxed)
}

/// The log scope of a new task: the submitter's scope (`logging::current()`),
/// the gate's `plugin` and `act` when there is one, and the task number.
/// Computed once at submission; each poll and the final drop only set it.
pub(crate) fn task_ctx(task: u64, gate: Option<&SubmitGate>) -> Ctx {
    let mut ctx = lectern_logging::current();
    if let Some(gate) = gate {
        // The gate names the activation the task belongs to, also when the
        // submitter's scope is another plugin's (or the gate's plugin has no
        // tag: then the record has no `plugin` rather than a wrong one).
        let owner = gate.log_ctx();
        ctx.plugin = owner.plugin;
        ctx.act = owner.act;
    }
    ctx.task = Some(task);
    ctx
}

/// A task's log scope as the task keeps it: 32 bytes instead of a 120-byte
/// `Ctx`, since every byte of the task's allocation is paid on each
/// submission. Usually only `plugin` and `act` are set (with `task`, the
/// cell's own number); a context with any other field (`session`, `review`,
/// `req`, `analysis`) is boxed once at submission.
pub(crate) struct TaskScope {
    plugin: Option<PluginTag>,
    /// `act`, or 0 for none (activation numbers start at 1).
    act: u64,
    /// The whole context, when it has more than `plugin`, `act` and `task`.
    full: Option<Box<Ctx>>,
}

impl TaskScope {
    /// See [`task_ctx`].
    pub(crate) fn new(task: u64, gate: Option<&SubmitGate>) -> TaskScope {
        TaskScope::from_ctx(task_ctx(task, gate))
    }

    /// `ctx` as a task keeps it (`ctx.task` is the task's number).
    pub(crate) fn from_ctx(ctx: Ctx) -> TaskScope {
        // Field by field: comparing whole `Ctx` values compiles to a long
        // vectorized routine.
        let light = ctx.session.is_none()
            && ctx.review.is_none()
            && ctx.req.is_none()
            && ctx.analysis.is_none()
            && ctx.act != Some(0);
        if light {
            TaskScope {
                plugin: ctx.plugin,
                act: ctx.act.unwrap_or(0),
                full: None,
            }
        } else {
            TaskScope {
                plugin: None,
                act: 0,
                full: Some(Box::new(ctx)),
            }
        }
    }

    /// The task's whole context; `task` is the number it was made with.
    pub(crate) fn ctx(&self, task: u64) -> Ctx {
        match &self.full {
            Some(ctx) => **ctx,
            None => Ctx {
                plugin: self.plugin,
                act: (self.act != 0).then_some(self.act),
                task: Some(task),
                ..Ctx::default()
            },
        }
    }

    /// Runs `f` with the task's context entered (`logging::within`): a
    /// pointer to the boxed context, or to one built on the stack.
    #[inline]
    pub(crate) fn enter<R>(&self, task: u64, f: impl FnOnce() -> R) -> R {
        match &self.full {
            Some(ctx) => lectern_logging::within(ctx, f),
            None => lectern_logging::within(&self.ctx(task), f),
        }
    }
}

/// What was running when a task's panic was caught: the `phase` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// A poll of a pool task's future.
    Poll,
    /// A dedicated task's closure, or a shutdown callback.
    Run,
    /// The final drop of a future or of an unrun closure or callback.
    Drop,
    /// The drop of a result nobody reads.
    Result,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Poll => "poll",
            Phase::Run => "run",
            Phase::Drop => "drop",
            Phase::Result => "result",
        }
    }
}

impl Field for Phase {
    fn value(&self) -> Value<'_> {
        Value::text(self.name())
    }
}

impl Field for ThreadKind {
    fn value(&self) -> Value<'_> {
        Value::text(match self {
            ThreadKind::Worker => "worker",
            ThreadKind::Batch => "batch",
            ThreadKind::Dedicated => "dedicated",
        })
    }
}

/// Logs error `task panicked` under the caller's scope. The `PanicText`
/// variant follows the payload's type, as in the kernel.
pub(crate) fn task_panicked(phase: Phase, payload: &PanicPayload) {
    error!(
        "task panicked";
        phase = phase,
        message = Value::panic_message(payload.text()),
    );
}

/// `kernel::contain` that logs `task panicked` with `phase` before the
/// payload is released.
pub(crate) fn contain_task<T>(phase: Phase, f: impl FnOnce() -> T) -> Result<T, String> {
    catch(f).map_err(|payload| {
        task_panicked(phase, &payload);
        payload.dispose()
    })
}
