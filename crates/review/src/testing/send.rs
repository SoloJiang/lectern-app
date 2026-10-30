//! `FakeClipboard` and `FakeRoute` (docs/design/review.md「测试」): the
//! clipboard route's write and a session route for the tests of sending
//! review notes. No test writes the real system clipboard: the engine's
//! write is a `Writer`, and these are the ones the tests give it.

use std::sync::{Arc, Mutex, PoisonError};

use lectern_services::review::{
    DeliveryOutcome, DeliveryReply, NoteDelivery, NoteRoute, ReviewTarget, RouteBlock, SentVia,
};

use crate::clipboard_route::{Done, WriteError, Writer};

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a [`FakeClipboard`] does with a write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClipMode {
    /// The clipboard takes the text; `done` is called at once.
    Accept,
    /// The clipboard refuses the text.
    Reject,
    /// There is no clipboard.
    Unsupported,
    /// `done` is kept until [`FakeClipboard::release`].
    Hold,
}

struct ClipState {
    mode: ClipMode,
    writes: Vec<Arc<str>>,
    held: Vec<Done>,
}

/// A clipboard that keeps what was written to it.
#[derive(Clone)]
pub struct FakeClipboard {
    state: Arc<Mutex<ClipState>>,
}

impl Default for FakeClipboard {
    fn default() -> Self {
        FakeClipboard::new()
    }
}

impl FakeClipboard {
    pub fn new() -> FakeClipboard {
        FakeClipboard {
            state: Arc::new(Mutex::new(ClipState {
                mode: ClipMode::Accept,
                writes: Vec::new(),
                held: Vec::new(),
            })),
        }
    }

    /// The write to give the engine (`ReviewPlugin::with_clipboard_writer`).
    pub fn writer(&self) -> Writer {
        let state = Arc::clone(&self.state);
        Arc::new(move |text, done| {
            let mode = {
                let mut s = locked(&state);
                s.writes.push(text);
                if s.mode == ClipMode::Hold {
                    s.held.push(done);
                    return;
                }
                s.mode
            };
            done(match mode {
                ClipMode::Accept => Ok(()),
                ClipMode::Reject => Err(WriteError::Rejected),
                _ => Err(WriteError::Unsupported),
            });
        })
    }

    pub fn set_mode(&self, mode: ClipMode) {
        locked(&self.state).mode = mode;
    }

    /// The texts written so far, oldest first.
    pub fn writes(&self) -> Vec<Arc<str>> {
        locked(&self.state).writes.clone()
    }

    /// Answers the writes held with `result`.
    pub fn release(&self, result: Result<(), WriteError>) {
        let held = std::mem::take(&mut locked(&self.state).held);
        for done in held {
            done(result);
        }
    }
}

/// What a [`FakeRoute`] answers to a delivery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RouteMode {
    Deliver(SentVia),
    NotNow(RouteBlock),
    Fail,
    /// The reply is kept until [`FakeRoute::release`]; dropped with the
    /// route's state it is answered as `Failed`.
    Hold,
}

struct RouteState {
    applies: bool,
    mode: RouteMode,
    deliveries: Vec<NoteDelivery>,
    held: Vec<DeliveryReply>,
}

/// A session route: it applies to session targets, keeps what it was
/// given and answers as told.
#[derive(Clone)]
pub struct FakeRoute {
    id: Arc<str>,
    state: Arc<Mutex<RouteState>>,
}

impl FakeRoute {
    pub fn new(id: &str, mode: RouteMode) -> FakeRoute {
        FakeRoute {
            id: Arc::from(id),
            state: Arc::new(Mutex::new(RouteState {
                applies: true,
                mode,
                deliveries: Vec::new(),
                held: Vec::new(),
            })),
        }
    }

    pub fn set_mode(&self, mode: RouteMode) {
        locked(&self.state).mode = mode;
    }

    /// Whether the route applies to session targets (it never applies to
    /// the others).
    pub fn set_applies(&self, applies: bool) {
        locked(&self.state).applies = applies;
    }

    /// What it was given, oldest first.
    pub fn deliveries(&self) -> Vec<NoteDelivery> {
        locked(&self.state).deliveries.clone()
    }

    /// Answers the deliveries held with `outcome`.
    pub fn release(&self, outcome: DeliveryOutcome) {
        let held = std::mem::take(&mut locked(&self.state).held);
        for reply in held {
            reply.send(outcome);
        }
    }

    /// Drops the replies held without answering them.
    pub fn drop_held(&self) {
        let held = std::mem::take(&mut locked(&self.state).held);
        drop(held);
    }
}

impl NoteRoute for FakeRoute {
    fn id(&self) -> Arc<str> {
        Arc::clone(&self.id)
    }

    fn applies(&self, target: &ReviewTarget) -> bool {
        matches!(target, ReviewTarget::Session(_)) && locked(&self.state).applies
    }

    fn deliver(&self, delivery: NoteDelivery, reply: DeliveryReply) {
        let mode = {
            let mut s = locked(&self.state);
            s.deliveries.push(delivery);
            if s.mode == RouteMode::Hold {
                s.held.push(reply);
                return;
            }
            s.mode
        };
        reply.send(match mode {
            RouteMode::Deliver(via) => DeliveryOutcome::Delivered(via),
            RouteMode::NotNow(block) => DeliveryOutcome::NotNow(block),
            RouteMode::Fail | RouteMode::Hold => DeliveryOutcome::Failed,
        });
    }
}
