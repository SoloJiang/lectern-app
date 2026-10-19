//! Interaction yield (`docs/design/kernel.md`, 静止; ADR 0033).
//!
//! A second global switch beside the pause switch. While it is on, background
//! work that declared itself yielding stops at its next slice boundary, keeps
//! its state and then moves only inside keepalive slots: one slot at a time
//! across all yielding work, each at most [`SLOT_LENGTH`], slot starts at
//! least [`SLOT_INTERVAL`] apart, one item per slot, items over
//! [`SLOT_MAX_BYTES`] wait until the switch turns off.
//!
//! None of this touches the background registry's state, count or epoch, or
//! quiescence: waiting for a slot is not a stop.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Wake, Waker};
use std::time::{Duration, Instant};

use crate::clock::{Clock, SystemClock};
use crate::util::lock;

/// The longest a keepalive slot lasts.
pub const SLOT_LENGTH: Duration = Duration::from_millis(50);
/// The shortest time between the starts of two keepalive slots.
pub const SLOT_INTERVAL: Duration = Duration::from_millis(500);
/// Items larger than this never run in a keepalive slot; they wait until the
/// yield switch turns off.
pub const SLOT_MAX_BYTES: u64 = 256 * 1024;

/// What a batch boundary allows, from
/// [`BackgroundGuard::proceed_item`](crate::BackgroundGuard::proceed_item).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
    /// Run as usual: the yield switch is off, or this work does not yield.
    Batch,
    /// The yield switch is on and this work holds the keepalive slot: run
    /// only the item asked for, until `deadline` on the kernel's clock.
    /// [`yield_requested`](crate::BackgroundGuard::yield_requested) turns
    /// true at the deadline; the slot ends at the next batch boundary check
    /// or when the guard is dropped.
    Slot { deadline: Instant },
}

/// What a yielding guard asks for at a batch boundary.
#[derive(Clone, Copy)]
pub(crate) enum Want {
    /// A normal batch (`proceed`): waits while the switch is on.
    Batch,
    /// One item of this many bytes (`proceed_item`).
    Item(u64),
}

/// The outcome of one poll.
pub(crate) enum Step {
    Grant(Grant),
    Pending,
    /// The pause switch is on: go back to the pause check, which wins.
    Paused,
}

struct Waiter {
    id: u64,
    /// Whether the item may enter a slot; otherwise it waits for the switch
    /// to turn off.
    fits: bool,
    waker: Waker,
}

struct Holder {
    id: u64,
    deadline: Instant,
}

struct Inner {
    on: bool,
    clock: Arc<dyn Clock>,
    holder: Option<Holder>,
    /// The waiter picked for the next slot, woken and not yet polled.
    reserved: Option<u64>,
    last_start: Option<Instant>,
    /// Yielding guards waiting at a boundary, first come first served.
    waiting: VecDeque<Waiter>,
    /// The instant the clock was last asked to wake the registry at.
    armed: Option<Instant>,
}

impl Inner {
    fn remove(&mut self, id: u64) -> Option<Waker> {
        if self.reserved == Some(id) {
            self.reserved = None;
        }
        let index = self.waiting.iter().position(|waiter| waiter.id == id)?;
        self.waiting.remove(index).map(|waiter| waiter.waker)
    }

    /// Starts a slot now: the clock is read here, after `dispatch` checked
    /// the interval, since another thread may move it in between.
    fn take_slot(&mut self, id: u64, holder: &AtomicU64) -> Grant {
        let now = self.clock.now();
        let deadline = now + SLOT_LENGTH;
        self.last_start = Some(now);
        self.holder = Some(Holder { id, deadline });
        holder.store(id + 1, SeqCst);
        Grant::Slot { deadline }
    }
}

/// What to do once the lock is released.
#[derive(Default)]
struct After {
    wake: Vec<Waker>,
    /// Wakers replaced or removed, dropped outside the lock like they are
    /// woken.
    dropped: Vec<Waker>,
    arm: Option<(Arc<dyn Clock>, Instant)>,
}

/// The kernel's yield registry.
pub(crate) struct Yield {
    /// Mirrors `Inner::on`, written under the lock.
    on: AtomicBool,
    /// Mirrors `Inner::holder`: the holding guard's id plus one, or zero.
    holder: AtomicU64,
    inner: Mutex<Inner>,
    this: Weak<Yield>,
}

/// Wakes the registry when the clock reaches the armed instant.
struct Alarm(Weak<Yield>);

impl Wake for Alarm {
    fn wake(self: Arc<Self>) {
        if let Some(registry) = self.0.upgrade() {
            registry.alarm();
        }
    }
}

impl Yield {
    pub(crate) fn new() -> Arc<Yield> {
        Arc::new_cyclic(|this| Yield {
            on: AtomicBool::new(false),
            holder: AtomicU64::new(0),
            inner: Mutex::new(Inner {
                on: false,
                clock: Arc::new(SystemClock),
                holder: None,
                reserved: None,
                last_start: None,
                waiting: VecDeque::new(),
                armed: None,
            }),
            this: this.clone(),
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn set_clock(&self, clock: Arc<dyn Clock>) {
        let mut inner = lock(&self.inner);
        inner.clock = clock;
        inner.armed = None;
    }

    fn finish(&self, after: After) {
        if let Some((clock, at)) = after.arm {
            clock.wake_at(at, Waker::from(Arc::new(Alarm(self.this.clone()))));
        }
        for waker in after.wake {
            waker.wake();
        }
        drop(after.dropped);
    }

    /// Picks the next slot holder when the slot is free and may start: the
    /// first waiter whose item fits. Returns its id if it is `polling` (it
    /// takes the slot in this poll); any other pick is reserved and woken.
    /// When the slot may not start yet, arms the clock.
    fn dispatch(&self, inner: &mut Inner, after: &mut After, polling: Option<u64>) -> bool {
        if !inner.on || inner.holder.is_some() || inner.reserved.is_some() {
            return false;
        }
        let Some(index) = inner.waiting.iter().position(|waiter| waiter.fits) else {
            return false;
        };
        let now = inner.clock.now();
        if let Some(ready) = inner.last_start.map(|start| start + SLOT_INTERVAL)
            && ready > now
        {
            if inner.armed != Some(ready) {
                inner.armed = Some(ready);
                after.arm = Some((inner.clock.clone(), ready));
            }
            return false;
        }
        let waiter = inner.waiting.remove(index).expect("index from position");
        if Some(waiter.id) == polling {
            return true;
        }
        inner.reserved = Some(waiter.id);
        after.wake.push(waiter.waker);
        false
    }

    fn alarm(&self) {
        let mut after = After::default();
        {
            let mut inner = lock(&self.inner);
            inner.armed = None;
            self.dispatch(&mut inner, &mut after, None);
        }
        self.finish(after);
    }

    /// The batch-boundary check of a yielding guard, after the pause check
    /// let it through. A poll ends the guard's own slot.
    pub(crate) fn poll(&self, id: u64, want: Want, cx: &Context<'_>, paused: &AtomicBool) -> Step {
        if !self.on.load(SeqCst) && self.holder.load(SeqCst) != id + 1 {
            return Step::Grant(Grant::Batch);
        }
        let mut after = After::default();
        let step = {
            let mut inner = lock(&self.inner);
            if inner.holder.as_ref().is_some_and(|holder| holder.id == id) {
                inner.holder = None;
                self.holder.store(0, SeqCst);
            }
            if !inner.on {
                Step::Grant(Grant::Batch)
            } else if paused.load(SeqCst) {
                // `pause` set the flag before it took this lock, so it either
                // sees this guard waiting or this poll sees the flag.
                after.dropped.extend(inner.remove(id));
                self.dispatch(&mut inner, &mut after, None);
                Step::Paused
            } else {
                let fits = matches!(want, Want::Item(bytes) if bytes <= SLOT_MAX_BYTES);
                let reserved = inner.reserved == Some(id);
                if reserved {
                    inner.reserved = None;
                }
                if reserved && fits {
                    Step::Grant(inner.take_slot(id, &self.holder))
                } else {
                    let waker = cx.waker().clone();
                    match inner.waiting.iter_mut().find(|waiter| waiter.id == id) {
                        Some(waiter) => {
                            waiter.fits = fits;
                            after
                                .dropped
                                .push(std::mem::replace(&mut waiter.waker, waker));
                        }
                        None => inner.waiting.push_back(Waiter { id, fits, waker }),
                    }
                    if self.dispatch(&mut inner, &mut after, Some(id)) {
                        Step::Grant(inner.take_slot(id, &self.holder))
                    } else {
                        Step::Pending
                    }
                }
            }
        };
        self.finish(after);
        step
    }

    /// Whether a yielding guard should stop at its next slice boundary.
    pub(crate) fn requested(&self, id: u64) -> bool {
        if !self.on.load(SeqCst) {
            return false;
        }
        if self.holder.load(SeqCst) != id + 1 {
            return true;
        }
        let inner = lock(&self.inner);
        match &inner.holder {
            Some(holder) if holder.id == id => inner.clock.now() >= holder.deadline,
            _ => true,
        }
    }

    /// The guard ended (dropped) or stopped for the pause: it leaves the
    /// queue and gives up its reservation or slot.
    pub(crate) fn forget(&self, id: u64) {
        let mut after = After::default();
        {
            let mut inner = lock(&self.inner);
            if inner.holder.as_ref().is_some_and(|holder| holder.id == id) {
                inner.holder = None;
                self.holder.store(0, SeqCst);
            }
            after.dropped.extend(inner.remove(id));
            self.dispatch(&mut inner, &mut after, None);
        }
        self.finish(after);
    }

    /// Wakes every waiter so it sees the pause switch, which `pause` has
    /// already turned on.
    pub(crate) fn wake_for_pause(&self) {
        let wake: Vec<Waker> = {
            let mut inner = lock(&self.inner);
            inner.reserved = None;
            inner.waiting.drain(..).map(|waiter| waiter.waker).collect()
        };
        for waker in wake {
            waker.wake();
        }
    }

    pub(crate) fn set(&self, on: bool) {
        let wake: Vec<Waker> = {
            let mut inner = lock(&self.inner);
            inner.on = on;
            self.on.store(on, SeqCst);
            if on {
                Vec::new()
            } else {
                // Everything waiting runs as usual now. A slot in progress
                // ends at its holder's next boundary check.
                inner.reserved = None;
                inner.waiting.drain(..).map(|waiter| waiter.waker).collect()
            }
        };
        for waker in wake {
            waker.wake();
        }
    }
}
