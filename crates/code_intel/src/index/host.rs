//! Where the indexer's work runs (「线程与守卫」): every task is submitted
//! through `PluginHandle::own_with`, so it is dropped when the activation
//! dies, and holds the guard its kind requires.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use lectern_executor::{Executor, Priority};
use lectern_kernel::{BackgroundGuard, BusyGuard, PluginHandle};
use lectern_services::code_intel::CodeIntelEvent;

/// Which guard a task held, for the `schedule_*` tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Guard {
    Busy,
    Background,
}

pub(crate) struct Host {
    handle: PluginHandle,
    executor: Executor,
    #[cfg(test)]
    pub(crate) log: std::sync::Mutex<Vec<(&'static str, Priority, Guard)>>,
    /// Every event emitted, in order (tests).
    #[cfg(test)]
    pub(crate) events: std::sync::Mutex<Vec<CodeIntelEvent>>,
}

impl Host {
    pub(crate) fn new(handle: PluginHandle, executor: Executor) -> Host {
        Host {
            handle,
            executor,
            #[cfg(test)]
            log: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            events: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Emits through this activation's emitter: on the calling (executor)
    /// thread, after the snapshot it announces is in place.
    pub(crate) fn emit(&self, event: CodeIntelEvent) {
        #[cfg(test)]
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
        self.handle.emitter().emit(event);
    }

    pub(crate) fn busy(&self) -> Option<BusyGuard> {
        self.handle.busy()
    }

    /// Runs `work` as an `Interactive` or `Background` task holding a
    /// `BusyGuard` from now until it ends (the guard is taken here, so the
    /// kernel is busy from this call). False when the activation is gone;
    /// `work` is then dropped unrun.
    pub(crate) fn spawn_busy(
        &self,
        what: &'static str,
        priority: Priority,
        work: impl FnOnce() + Send + 'static,
    ) -> bool {
        debug_assert!(priority != Priority::Batch);
        let Some(guard) = self.handle.busy() else {
            return false;
        };
        self.record(what, priority, Guard::Busy);
        let executor = self.executor.clone();
        self.handle.own_with(move |gate| {
            executor.spawn_gated(&gate, priority, async move {
                let _guard = guard;
                work();
            })
        })
    }

    /// Runs a pausable batch task (`Priority::Batch`) holding a
    /// `BackgroundGuard` (ADR 0029). False when the activation is gone.
    pub(crate) fn spawn_batch<F>(
        &self,
        what: &'static str,
        work: impl FnOnce(Arc<BackgroundGuard>) -> F + Send + 'static,
    ) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Some(guard) = self.handle.background() else {
            return false;
        };
        self.record(what, Priority::Batch, Guard::Background);
        let executor = self.executor.clone();
        self.handle.own_with(move |gate| {
            executor.spawn_gated(&gate, Priority::Batch, work(Arc::new(guard)))
        })
    }

    #[cfg(test)]
    fn record(&self, what: &'static str, priority: Priority, guard: Guard) {
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((what, priority, guard));
    }

    #[cfg(not(test))]
    fn record(&self, _what: &'static str, _priority: Priority, _guard: Guard) {}
}

/// Gives the executor back once: other tasks (and cancellation) get a turn
/// between batches.
pub(crate) fn yield_now() -> impl Future<Output = ()> {
    struct Yield(bool);
    impl Future for Yield {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
    Yield(false)
}
