//! Logging helpers (`docs/design/executor.md`, 日志与诊断): task numbers,
//! the task scope and the panic records.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use lectern_kernel::{PanicPayload, SubmitGate, catch};
use lectern_logging::{Ctx, Field, Value, error};

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
