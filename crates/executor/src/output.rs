//! The result slot shared by a task and its [`Task`](crate::Task) handle.

use std::sync::{Mutex, Weak};
use std::task::{Context, Poll, Waker};

use lectern_logging::Ctx;

use crate::diag::{Phase, contain_task};
use crate::sched::Inner;
use crate::util::lock;
use crate::{Priority, TaskError};

pub(crate) struct Output<T> {
    state: Mutex<State<T>>,
}

struct State<T> {
    result: Option<Result<T, TaskError>>,
    finished: bool,
    /// Set when the handle is dropped or detached: nobody reads the result,
    /// so it is dropped where the task finishes.
    abandoned: bool,
    waker: Option<Waker>,
}

impl<T> Output<T> {
    pub(crate) fn new() -> Self {
        Output {
            state: Mutex::new(State {
                result: None,
                finished: false,
                abandoned: false,
                waker: None,
            }),
        }
    }

    /// Stores the result and wakes the task awaiting it. Called under the
    /// task's scope.
    pub(crate) fn complete(&self, result: Result<T, TaskError>) {
        let (unread, waker) = {
            let mut state = lock(&self.state);
            if state.finished {
                return;
            }
            state.finished = true;
            let unread = if state.abandoned {
                Some(result)
            } else {
                state.result = Some(result);
                None
            };
            (unread, state.waker.take())
        };
        if let Some(unread) = unread {
            let _ = contain_task(Phase::Result, move || drop(unread));
        }
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Called when the handle goes away (dropped or detached). A value that
    /// was already stored is handed to `exec` to drop on a thread of
    /// `priority`'s pool (after shutdown, on the reclaim thread) under
    /// `contain`, so the handle's thread never runs its `Drop`. Errors carry
    /// only a message, and values without drop glue run no code, so both are
    /// dropped here. `ctx` is the task's scope, set wherever the value is
    /// dropped.
    pub(crate) fn abandon(&self, exec: &Weak<Inner>, priority: Priority, ctx: &Ctx)
    where
        T: Send + 'static,
    {
        let (stored, waker) = {
            let mut state = lock(&self.state);
            state.abandoned = true;
            (state.result.take(), state.waker.take())
        };
        drop(waker);
        let Some(Ok(value)) = stored else { return };
        if !std::mem::needs_drop::<T>() {
            return;
        }
        match exec.upgrade() {
            Some(exec) => exec.drop_later(Box::new(value), priority, *ctx),
            // The executor is gone (a dropped `manual()` executor): nowhere
            // else to drop it.
            None => {
                let _scope = lectern_logging::scope(*ctx);
                let _ = contain_task(Phase::Result, move || drop(value));
            }
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        lock(&self.state).finished
    }

    pub(crate) fn poll(&self, cx: &mut Context<'_>) -> Poll<Result<T, TaskError>> {
        let mut state = lock(&self.state);
        if let Some(result) = state.result.take() {
            return Poll::Ready(result);
        }
        if state.finished {
            // Polled again after the result was taken.
            return Poll::Ready(Err(TaskError::Cancelled));
        }
        match &state.waker {
            Some(waker) if waker.will_wake(cx.waker()) => {}
            _ => state.waker = Some(cx.waker().clone()),
        }
        Poll::Pending
    }
}
