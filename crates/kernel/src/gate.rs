//! The submit gate between `PluginHandle::own_with` and the executor.

use std::sync::Arc;
use std::task::{Context as TaskContext, Waker};

use lectern_logging::Ctx;

use crate::kernel::{Request, invalidate};
use crate::plugin::PluginError;
use crate::token::{ActivationToken, TokenState};
use crate::util::lock;

/// The gate of one `own_with` submission. `executor` asks it for a
/// [`GateSlot`] per task and calls [`GateSlot::poll`] before every poll of the
/// task's future.
///
/// The executor's contract with the kernel: `Waker::wake` only puts the task
/// back on the run queue; it never polls or drops the future on the waking
/// thread.
pub struct SubmitGate {
    pub(crate) inner: Arc<GateInner>,
}

/// Result of [`GateSlot::poll`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatePoll {
    /// Poll the future.
    Run,
    /// The gate is not open yet; return `Pending`.
    Wait,
    /// The gate closed or the activation is dead; drop the future.
    Drop,
}

/// One task's place on a gate. Holds the task's latest waker and leaves the
/// gate when dropped.
pub struct GateSlot {
    inner: Arc<GateInner>,
    index: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    NotOpen,
    Open,
    Closed,
}

struct SlotState {
    waker: Option<Waker>,
    detached: bool,
}

struct GateState {
    phase: Phase,
    /// Set when `start` returns: no more slots are handed out.
    sealed: bool,
    /// Set once the task handle is registered in the guard table.
    registered: bool,
    reclaim_sent: bool,
    seq: u64,
    slots: Vec<Option<SlotState>>,
    live: usize,
}

pub(crate) struct GateInner {
    state: std::sync::Mutex<GateState>,
    token: ActivationToken,
}

impl GateInner {
    pub(crate) fn new(token: ActivationToken) -> Arc<Self> {
        Arc::new(GateInner {
            state: std::sync::Mutex::new(GateState {
                phase: Phase::NotOpen,
                sealed: false,
                registered: false,
                reclaim_sent: false,
                seq: 0,
                slots: Vec::new(),
                live: 0,
            }),
            token,
        })
    }

    pub(crate) fn set_seq(&self, seq: u64) {
        lock(&self.state).seq = seq;
    }

    /// Seals the gate and marks the handle registered. Returns true if every
    /// task already finished, so the registration can be reclaimed now.
    pub(crate) fn seal_registered(&self) -> bool {
        let mut state = lock(&self.state);
        state.sealed = true;
        state.registered = true;
        if state.live == 0 && !state.reclaim_sent {
            state.reclaim_sent = true;
            true
        } else {
            false
        }
    }

    /// `NotOpen → Open`. A gate that was closed in the meantime stays closed.
    pub(crate) fn open(&self) {
        let wakers = {
            let mut state = lock(&self.state);
            if state.phase == Phase::NotOpen {
                state.phase = Phase::Open;
            }
            collect_wakers(&state)
        };
        wakers.into_iter().for_each(Waker::wake);
    }

    pub(crate) fn close(&self) {
        let wakers = {
            let mut state = lock(&self.state);
            state.phase = Phase::Closed;
            state.sealed = true;
            collect_wakers(&state)
        };
        wakers.into_iter().for_each(Waker::wake);
    }

    /// Wakes every slot. Wakers are cloned under the lock and called after it
    /// is released.
    pub(crate) fn wake_all(&self) {
        let wakers = collect_wakers(&lock(&self.state));
        wakers.into_iter().for_each(Waker::wake);
    }

    pub(crate) fn request_reclaim(&self, seq: u64) {
        if let Some(shared) = self.token.0.kernel.upgrade() {
            shared.enqueue(Request::Reclaim {
                token: self.token.clone(),
                seq,
            });
        }
    }
}

fn collect_wakers(state: &GateState) -> Vec<Waker> {
    state
        .slots
        .iter()
        .flatten()
        .filter(|slot| !slot.detached)
        .filter_map(|slot| slot.waker.clone())
        .collect()
}

impl SubmitGate {
    /// A slot for one task. The executor asks for it inside the submitting
    /// call, before `spawn` returns. Returns `None` once `start` has returned
    /// (the gate is sealed); the executor then drops the task without polling
    /// it, under `contain`, and reports a panic in that drop with
    /// [`SubmitGate::panicked`].
    pub fn slot(&self) -> Option<GateSlot> {
        let mut state = lock(&self.inner.state);
        if state.sealed {
            return None;
        }
        let slot = SlotState {
            waker: None,
            detached: false,
        };
        let index = match state.slots.iter().position(Option::is_none) {
            Some(index) => {
                state.slots[index] = Some(slot);
                index
            }
            None => {
                state.slots.push(Some(slot));
                state.slots.len() - 1
            }
        };
        state.live += 1;
        Some(GateSlot {
            inner: self.inner.clone(),
            index,
        })
    }

    /// The `plugin` and `act` of the activation this gate submits for (the
    /// other fields None). The executor puts them in the log scope of every
    /// task it takes a slot for.
    pub fn log_ctx(&self) -> Ctx {
        self.inner.token.log_ctx()
    }

    /// A task without a slot panicked while being dropped; same effect as
    /// [`GateSlot::panicked`].
    pub fn panicked(&self, message: String) {
        invalidate(
            &self.inner.token,
            Some(PluginError::Panicked(format!("task panicked: {message}"))),
        );
    }
}

impl GateSlot {
    /// Stores `cx.waker()` and reads the gate and token state under one lock,
    /// so a concurrent state change is either seen here or wakes this waker.
    pub fn poll(&self, cx: &mut TaskContext<'_>) -> GatePoll {
        let mut state = lock(&self.inner.state);
        if let Some(Some(slot)) = state.slots.get_mut(self.index)
            && !slot.detached
        {
            match &slot.waker {
                Some(waker) if waker.will_wake(cx.waker()) => {}
                _ => slot.waker = Some(cx.waker().clone()),
            }
        }
        let phase = state.phase;
        let dead = self.inner.token.state() == TokenState::Dead;
        drop(state);
        match phase {
            Phase::Closed => GatePoll::Drop,
            _ if dead => GatePoll::Drop,
            Phase::NotOpen => GatePoll::Wait,
            Phase::Open => GatePoll::Run,
        }
    }

    /// The task panicked while being polled or dropped. The executor has
    /// already marked the task finished, dropped its future and disposed of the
    /// payload. This slot leaves the wake list first, then the activation
    /// fails as with `PluginHandle::fail`.
    pub fn panicked(&self, message: String) {
        {
            let mut state = lock(&self.inner.state);
            if let Some(Some(slot)) = state.slots.get_mut(self.index) {
                slot.detached = true;
                slot.waker = None;
            }
        }
        invalidate(
            &self.inner.token,
            Some(PluginError::Panicked(format!("task panicked: {message}"))),
        );
    }
}

impl Drop for GateSlot {
    fn drop(&mut self) {
        let reclaim = {
            let mut state = lock(&self.inner.state);
            if let Some(slot) = state.slots.get_mut(self.index) {
                *slot = None;
            }
            state.live = state.live.saturating_sub(1);
            if state.sealed && state.registered && state.live == 0 && !state.reclaim_sent {
                state.reclaim_sent = true;
                Some(state.seq)
            } else {
                None
            }
        };
        if let Some(seq) = reclaim {
            self.inner.request_reclaim(seq);
        }
    }
}
