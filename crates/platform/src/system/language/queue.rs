//! The write queue behind `set_language_override`: compiled where it is used
//! (macOS) and in the tests, which give it a fake writer.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{Done, PrefError};

struct Request {
    tag: Option<String>,
    done: Done,
}

#[derive(Default)]
struct State {
    /// The request that waits for the thread; a newer one replaces it.
    pending: Option<Request>,
    /// Replaced requests whose `done` has not run yet.
    superseded: VecDeque<Done>,
    /// A thread is draining the queue (started and not yet gone).
    running: bool,
}

pub(super) type Writer = dyn Fn(Option<&str>) -> Result<(), PrefError> + Send + Sync;

/// The write queue: at most one request waits, one thread drains it and
/// ends when nothing is left, so an idle process holds no thread. The
/// writer is the system call, replaced in tests.
pub(super) struct Queue {
    state: Mutex<State>,
    writer: Box<Writer>,
}

impl Queue {
    pub(super) fn new(writer: Box<Writer>) -> Arc<Queue> {
        Arc::new(Queue {
            state: Mutex::new(State::default()),
            writer,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // The state is only changed in short sections that cannot panic; a
        // poisoned lock still holds a consistent state.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a thread is draining (tests wait for it to end).
    #[cfg(test)]
    pub(super) fn is_running(&self) -> bool {
        self.lock().running
    }

    /// Queues the request and starts the thread when none is draining.
    pub(super) fn submit(self: &Arc<Self>, tag: Option<&str>, done: Done) {
        let request = Request {
            tag: tag.map(str::to_owned),
            done,
        };
        let spawn = {
            let mut state = self.lock();
            if let Some(old) = state.pending.replace(request) {
                state.superseded.push_back(old.done);
            }
            !std::mem::replace(&mut state.running, true)
        };
        if !spawn {
            return;
        }
        let queue = Arc::clone(self);
        let started = std::thread::Builder::new()
            .name("lectern-language-pref".to_owned())
            .spawn(move || queue.drain());
        if started.is_err() {
            // No thread: nothing will ever write. Fail what is queued.
            let (pending, superseded) = {
                let mut state = self.lock();
                state.running = false;
                (state.pending.take(), std::mem::take(&mut state.superseded))
            };
            for done in superseded {
                finish(done, Err(PrefError::Superseded));
            }
            if let Some(request) = pending {
                finish(request.done, Err(PrefError::WriteFailed));
            }
        }
    }

    /// The thread's loop: replaced requests first, then the pending one;
    /// ends, under the lock, when both are empty.
    fn drain(&self) {
        loop {
            let (superseded, pending) = {
                let mut state = self.lock();
                let superseded = std::mem::take(&mut state.superseded);
                let pending = state.pending.take();
                if superseded.is_empty() && pending.is_none() {
                    state.running = false;
                    return;
                }
                (superseded, pending)
            };
            for done in superseded {
                finish(done, Err(PrefError::Superseded));
            }
            if let Some(request) = pending {
                let result =
                    catch_unwind(AssertUnwindSafe(|| (self.writer)(request.tag.as_deref())))
                        .unwrap_or(Err(PrefError::WriteFailed));
                finish(request.done, result);
            }
        }
    }
}

/// Runs a `done`, containing its panic: a panic must not end the thread
/// with `running` still set.
fn finish(done: Done, result: Result<(), PrefError>) {
    let _ = catch_unwind(AssertUnwindSafe(|| done(result)));
}
