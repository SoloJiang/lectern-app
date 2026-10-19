//! Pausable background work (`docs/design/kernel.md`, 静止; ADR 0029).
//!
//! Background guards are counted apart from busy units: they never touch
//! `Counters`, so `is_quiescent`, `quiescent_epoch` and `Busy` / `Quiescent`
//! do not see them. One mutex holds the switch, the guard count, the stopped
//! guards with their wakers and the background epoch, so every start, end,
//! stop and continuation changes the state and the epoch in one step.
//!
//! The interaction yield (ADR 0033) lives in `yielding` beside it and never
//! touches that state: a guard declares at acquisition whether it yields.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};

use crate::token::Transition;
use crate::util::lock;
use crate::yielding::{Grant, Step, Want, Yield};

/// What the pausable background work is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundState {
    /// No [`BackgroundGuard`] is held.
    Idle,
    /// At least one guard is held and not stopped.
    Running,
    /// Guards are held and every one of them is stopped by the pause switch.
    Paused,
}

/// A consistent snapshot of the background work, read in one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundStatus {
    pub state: BackgroundState,
    /// The number of [`BackgroundGuard`]s held.
    pub count: usize,
    /// The background epoch: it grows by one whenever a guard is taken or
    /// dropped, a guard stops at a batch boundary, or `resume` lets a stopped
    /// guard continue (one step per guard). Two snapshots with the same epoch mean none of that
    /// happened in between. Separate from `quiescent_epoch`.
    pub epoch: u64,
}

struct Inner {
    paused: bool,
    count: usize,
    epoch: u64,
    /// Stopped guards by id, with the waker of their last `poll_proceed`
    /// (`None` for a guard taken during a pause that has not polled yet).
    stopped: Vec<(u64, Option<Waker>)>,
    /// The state last announced as `BackgroundChanged`.
    announced: BackgroundState,
}

impl Inner {
    fn state(&self) -> BackgroundState {
        if self.count == 0 {
            BackgroundState::Idle
        } else if self.stopped.len() == self.count {
            BackgroundState::Paused
        } else {
            BackgroundState::Running
        }
    }

    /// Advances the epoch by `steps` changes made in one locked step and
    /// returns the transition to announce if the state changed.
    fn changed_by(&mut self, steps: u64) -> Option<Transition> {
        self.epoch += steps;
        let state = self.state();
        if state == self.announced {
            return None;
        }
        self.announced = state;
        Some(Transition::Background(state, self.epoch))
    }

    /// One change: a start, an end or a stop.
    fn changed(&mut self) -> Option<Transition> {
        self.changed_by(1)
    }

    fn status(&self) -> BackgroundStatus {
        BackgroundStatus {
            state: self.state(),
            count: self.count,
            epoch: self.epoch,
        }
    }
}

/// The kernel's background-work registry.
pub(crate) struct Background {
    /// Mirrors `Inner::paused`, written under the lock, so a batch boundary
    /// outside a pause does not take the lock.
    paused: AtomicBool,
    inner: Mutex<Inner>,
    next_id: AtomicU64,
    notify: OnceLock<Box<dyn Fn(Transition) + Send + Sync>>,
    pub(crate) yielding: Arc<Yield>,
}

impl Background {
    pub(crate) fn new() -> Self {
        Background {
            paused: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                paused: false,
                count: 0,
                epoch: 0,
                stopped: Vec::new(),
                announced: BackgroundState::Idle,
            }),
            next_id: AtomicU64::new(0),
            notify: OnceLock::new(),
            yielding: Yield::new(),
        }
    }

    pub(crate) fn set_notify(&self, notify: Box<dyn Fn(Transition) + Send + Sync>) {
        let _ = self.notify.set(notify);
    }

    /// Announces outside the lock, as `Counters` does: the kernel thread
    /// dispatches the event and listeners order events by `seq`.
    fn announce(&self, transition: Option<Transition>) {
        if let (Some(transition), Some(notify)) = (transition, self.notify.get()) {
            notify(transition);
        }
    }

    pub(crate) fn guard(self: &Arc<Self>, yields: bool) -> BackgroundGuard {
        let id = self.next_id.fetch_add(1, SeqCst);
        let transition = {
            let mut inner = lock(&self.inner);
            inner.count += 1;
            if inner.paused {
                // Taken during a pause: stopped before its first batch.
                inner.stopped.push((id, None));
            }
            inner.changed()
        };
        self.announce(transition);
        BackgroundGuard {
            background: self.clone(),
            id,
            yields,
        }
    }

    pub(crate) fn pause(&self) {
        {
            let mut inner = lock(&self.inner);
            inner.paused = true;
            self.paused.store(true, SeqCst);
        }
        // Work waiting for a keepalive slot is not stopped yet: wake it so it
        // stops for the pause, which wins over the yield.
        self.yielding.wake_for_pause();
    }

    pub(crate) fn resume(&self) {
        let (wakers, transition) = {
            let mut inner = lock(&self.inner);
            inner.paused = false;
            self.paused.store(false, SeqCst);
            let stopped = std::mem::take(&mut inner.stopped);
            // Each stopped guard continues: one epoch step per guard, one
            // event carrying the final epoch.
            let transition = if stopped.is_empty() {
                None
            } else {
                inner.changed_by(stopped.len() as u64)
            };
            (stopped, transition)
        };
        self.announce(transition);
        // Wake outside the lock: a waker may run executor code.
        for waker in wakers.into_iter().filter_map(|(_, waker)| waker) {
            waker.wake();
        }
    }

    pub(crate) fn status(&self) -> BackgroundStatus {
        lock(&self.inner).status()
    }
}

/// Declares pausable background work (ADR 0029). Unlike a
/// [`BusyGuard`](crate::BusyGuard) it does not hold off quiescence; the
/// kernel counts it apart and reports it through
/// [`Kernel::background`](crate::Kernel::background).
///
/// The work checks [`proceed`](BackgroundGuard::proceed) (or
/// [`poll_proceed`](BackgroundGuard::poll_proceed)) before every batch,
/// including the first: while the kernel's pause switch is on, the work
/// stops there until `resume`. Dropping the guard ends the work.
///
/// A guard taken with
/// [`PluginHandle::background_yielding`](crate::PluginHandle::background_yielding)
/// also yields to interaction (ADR 0033): while
/// [`Kernel::set_yield`](crate::Kernel::set_yield) is on, the work stops at
/// its next slice boundary ([`yield_requested`](BackgroundGuard::yield_requested)),
/// keeps its state and moves on only in keepalive slots handed out by
/// [`proceed_item`](BackgroundGuard::proceed_item).
pub struct BackgroundGuard {
    background: Arc<Background>,
    id: u64,
    yields: bool,
}

impl BackgroundGuard {
    /// Checks the switches at a batch boundary. `Ready` means run the next
    /// batch. While paused, registers `cx`'s waker, counts this work as
    /// stopped and returns `Pending`; `Kernel::resume` wakes it. A yielding
    /// guard also waits while the yield switch is on, without counting as
    /// stopped; to move inside keepalive slots it uses
    /// [`proceed_item`](BackgroundGuard::proceed_item) instead.
    pub fn poll_proceed(&self, cx: &mut Context<'_>) -> Poll<()> {
        self.poll_switches(cx, Want::Batch).map(|_| ())
    }

    /// The batch-boundary check of yielding work that knows its next item:
    /// `bytes` is the item's size. Waits while paused, as
    /// [`poll_proceed`](BackgroundGuard::poll_proceed) does. While the yield
    /// switch is on, returns [`Grant::Slot`] once this work gets the
    /// keepalive slot (never for an item over
    /// [`SLOT_MAX_BYTES`](crate::SLOT_MAX_BYTES), which waits for the switch
    /// to turn off); otherwise [`Grant::Batch`]. A guard that does not yield
    /// always gets `Batch` once the pause allows it.
    pub fn poll_proceed_item(&self, cx: &mut Context<'_>, bytes: u64) -> Poll<Grant> {
        self.poll_switches(cx, Want::Item(bytes))
    }

    /// The future form of
    /// [`poll_proceed_item`](BackgroundGuard::poll_proceed_item).
    pub fn proceed_item(&self, bytes: u64) -> ProceedItem<'_> {
        ProceedItem { guard: self, bytes }
    }

    /// Whether this guard was taken as yielding work.
    pub fn yields(&self) -> bool {
        self.yields
    }

    /// Whether yielding work should stop at its next slice boundary, without
    /// registering anything: the yield switch is on and this guard does not
    /// hold the keepalive slot, or its slot has reached the deadline. Always
    /// false for a guard that does not yield. For progress callbacks; the
    /// pause is checked apart with
    /// [`pause_requested`](BackgroundGuard::pause_requested) and wins.
    pub fn yield_requested(&self) -> bool {
        self.yields && self.background.yielding.requested(self.id)
    }

    fn poll_switches(&self, cx: &mut Context<'_>, want: Want) -> Poll<Grant> {
        loop {
            if self.poll_pause(cx).is_pending() {
                if self.yields {
                    // Stopped for the pause: no slot, no place in the queue.
                    self.background.yielding.forget(self.id);
                }
                return Poll::Pending;
            }
            if !self.yields {
                return Poll::Ready(Grant::Batch);
            }
            let yielding = &self.background.yielding;
            match yielding.poll(self.id, want, cx, &self.background.paused) {
                Step::Grant(grant) => return Poll::Ready(grant),
                Step::Pending => return Poll::Pending,
                Step::Paused => continue,
            }
        }
    }

    fn poll_pause(&self, cx: &mut Context<'_>) -> Poll<()> {
        if !self.background.paused.load(SeqCst) {
            return Poll::Ready(());
        }
        let waker = cx.waker().clone();
        // Wakers are dropped outside the lock, like they are woken.
        let (old, transition) = {
            let mut inner = lock(&self.background.inner);
            if !inner.paused {
                return Poll::Ready(());
            }
            match inner.stopped.iter_mut().find(|(id, _)| *id == self.id) {
                // Already stopped (or taken during this pause): keep the
                // latest waker, nothing changes.
                Some((_, slot)) => (slot.replace(waker), None),
                None => {
                    inner.stopped.push((self.id, Some(waker)));
                    (None, inner.changed())
                }
            }
        };
        drop(old);
        self.background.announce(transition);
        Poll::Pending
    }

    /// The future form of [`poll_proceed`](BackgroundGuard::poll_proceed):
    /// `guard.proceed().await` before each batch.
    pub fn proceed(&self) -> Proceed<'_> {
        Proceed { guard: self }
    }

    /// Whether the pause switch is on, without registering anything. For
    /// checks inside a batch (such as a parser's progress callback) that
    /// abandon the current item and return to the batch boundary.
    pub fn pause_requested(&self) -> bool {
        self.background.paused.load(SeqCst)
    }
}

impl Drop for BackgroundGuard {
    fn drop(&mut self) {
        if self.yields {
            self.background.yielding.forget(self.id);
        }
        let (stopped, transition) = {
            let mut inner = lock(&self.background.inner);
            let stopped = inner
                .stopped
                .iter()
                .position(|(id, _)| *id == self.id)
                .map(|index| inner.stopped.swap_remove(index));
            inner.count -= 1;
            (stopped, inner.changed())
        };
        drop(stopped);
        self.background.announce(transition);
    }
}

/// Resolves when the batch may run; see [`BackgroundGuard::proceed`].
pub struct Proceed<'a> {
    guard: &'a BackgroundGuard,
}

impl Future for Proceed<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.guard.poll_proceed(cx)
    }
}

/// Resolves when the next item may run; see
/// [`BackgroundGuard::proceed_item`].
pub struct ProceedItem<'a> {
    guard: &'a BackgroundGuard,
    bytes: u64,
}

impl Future for ProceedItem<'_> {
    type Output = Grant;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Grant> {
        self.guard.poll_proceed_item(cx, self.bytes)
    }
}
