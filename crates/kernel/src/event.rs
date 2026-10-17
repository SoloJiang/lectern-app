//! Typed events and synchronous dispatch.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::{Arc, Weak};

use crate::kernel::{Shared, invalidate};
use crate::panic::contain;
use crate::plugin::{Name, PluginError};
use crate::registry::{Garbage, drop_garbage};
use crate::token::{ActivationToken, StagedItem, TokenState, drop_rejected};
use crate::util::{lock, read, write};

/// An event type, defined by whoever emits it (usually `services`). The kernel
/// only dispatches. Event values are dropped on the emitting thread, so their
/// `Drop` must not panic.
pub trait Event: Send + Sync + 'static {}

/// A provider of `S` became live. Emitted on the kernel thread.
pub struct ServiceAdded<S: ?Sized> {
    pub provider: Name,
    _marker: PhantomData<fn() -> Box<S>>,
}

impl<S: ?Sized> ServiceAdded<S> {
    pub(crate) fn new(provider: Name) -> Self {
        ServiceAdded {
            provider,
            _marker: PhantomData,
        }
    }
}

impl<S: ?Sized + 'static> Event for ServiceAdded<S> {}

/// A provider of `S` was removed. Emitted on the kernel thread.
pub struct ServiceRemoved<S: ?Sized> {
    pub provider: Name,
    _marker: PhantomData<fn() -> Box<S>>,
}

impl<S: ?Sized> ServiceRemoved<S> {
    pub(crate) fn new(provider: Name) -> Self {
        ServiceRemoved {
            provider,
            _marker: PhantomData,
        }
    }
}

impl<S: ?Sized + 'static> Event for ServiceRemoved<S> {}

/// The kernel went from quiescent to busy. `seq` shares one counter with
/// [`Kernel::quiescent_epoch`](crate::Kernel::quiescent_epoch); ignore events
/// whose `seq` is not greater than the largest already seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Busy {
    pub seq: u64,
}

impl Event for Busy {}

/// The kernel went from busy to quiescent. See [`Busy`] for `seq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quiescent {
    pub seq: u64,
}

impl Event for Quiescent {}

/// Sends events. An emitter from `PluginHandle::emitter()` belongs to one
/// activation; the app's emitter belongs to none.
#[derive(Clone)]
pub struct Emitter {
    pub(crate) kernel: Weak<Shared>,
    pub(crate) token: Option<ActivationToken>,
}

impl Emitter {
    /// Dispatches `event` synchronously on this thread, in subscription order.
    ///
    /// An activation's emitter holds the emitter's admission for the whole
    /// dispatch and drops the event if it cannot get one. While the activation
    /// is still `Pending` (inside `started`) the event is held and sent once
    /// it goes live, or dropped if it fails.
    pub fn emit<E: Event>(&self, event: E) {
        let Some(shared) = self.kernel.upgrade() else {
            drop_rejected(event);
            return;
        };
        let Some(token) = &self.token else {
            dispatch(&shared, None, event);
            return;
        };
        match token.state() {
            TokenState::Live => dispatch(&shared, Some(token), event),
            TokenState::Dead => drop_rejected(event),
            TokenState::Pending => {
                let mut staging = lock(&token.0.staging);
                if token.state() == TokenState::Pending && !staging.sealed {
                    let emitter = self.clone();
                    staging
                        .items
                        .push(StagedItem::Emit(Box::new(move || emitter.emit(event))));
                    return;
                }
                drop(staging);
                // The activation went live or died while we took the lock.
                self.emit(event);
            }
        }
    }
}

/// An event queued behind the dispatch already running on this thread.
struct Queued {
    token: Option<ActivationToken>,
    run: Box<dyn FnOnce()>,
}

#[derive(Default)]
struct DispatchState {
    active: bool,
    queue: VecDeque<Queued>,
}

thread_local! {
    static DISPATCH: RefCell<DispatchState> = RefCell::new(DispatchState::default());
}

/// Resets the thread's dispatch state even if something unwinds.
struct ActiveDispatch;

impl Drop for ActiveDispatch {
    fn drop(&mut self) {
        DISPATCH.with(|state| state.borrow_mut().active = false);
    }
}

pub(crate) fn dispatch<E: Event>(shared: &Arc<Shared>, token: Option<&ActivationToken>, event: E) {
    let event = shared.tap(token, event);
    let nested = DISPATCH.with(|state| state.borrow().active);
    if nested {
        // Events emitted inside a listener run after the current dispatch, on
        // this thread, each with its own emitter's admission.
        let weak = Arc::downgrade(shared);
        let run: Box<dyn FnOnce()> = Box::new(move || {
            if let Some(shared) = weak.upgrade() {
                deliver(&shared, event);
            } else {
                drop_rejected(event);
            }
        });
        DISPATCH.with(|state| {
            state.borrow_mut().queue.push_back(Queued {
                token: token.cloned(),
                run,
            })
        });
        return;
    }

    let _busy = shared.counters.busy();
    DISPATCH.with(|state| state.borrow_mut().active = true);
    let _active = ActiveDispatch;
    run_admitted(token, Box::new(|| deliver(shared, event)));
    while let Some(queued) = DISPATCH.with(|state| state.borrow_mut().queue.pop_front()) {
        run_admitted(queued.token.as_ref(), queued.run);
    }
}

fn run_admitted<'a>(token: Option<&ActivationToken>, run: Box<dyn FnOnce() + 'a>) {
    match token {
        None => run(),
        Some(token) => match token.enter() {
            Some(admission) => {
                run();
                drop(admission);
            }
            None => drop_rejected(run),
        },
    }
}

/// Calls every listener of `E` with the event, then drops it.
fn deliver<E: Event>(shared: &Arc<Shared>, event: Tapped<E>) {
    let snapshot = read(&shared.registry).listeners(TypeId::of::<E>());
    if let Some(listeners) = &snapshot {
        let value: &E = &event;
        for listener in listeners.iter() {
            let Some(admission) = listener.handle.act.enter() else {
                continue;
            };
            if let Err(message) = contain(|| (listener.f)(value as &dyn Any, &listener.handle)) {
                invalidate(
                    &listener.handle.act,
                    Some(PluginError::Panicked(format!(
                        "listener for {} panicked: {message}",
                        std::any::type_name::<E>()
                    ))),
                );
            }
            drop(admission);
        }
    }
    drop_rejected(event);
    if let Some(listeners) = snapshot {
        drop_rejected(listeners);
    }
}

/// Dispatches a `Busy` or `Quiescent` event on the kernel thread. Neither the
/// dispatch nor the admissions taken for its listeners count towards
/// quiescence, so recording the event does not cause another transition.
pub(crate) fn dispatch_transition<E: Event>(shared: &Arc<Shared>, event: E) {
    let snapshot = read(&shared.registry).listeners(TypeId::of::<E>());
    let Some(listeners) = snapshot else {
        return;
    };
    for listener in listeners.iter() {
        let Some(admission) = listener.handle.act.enter_uncounted() else {
            continue;
        };
        if let Err(message) = contain(|| (listener.f)(&event as &dyn Any, &listener.handle)) {
            invalidate(
                &listener.handle.act,
                Some(PluginError::Panicked(format!(
                    "listener for {} panicked: {message}",
                    std::any::type_name::<E>()
                ))),
            );
        }
        drop(admission);
    }
    drop_rejected(listeners);
}

/// An event value, possibly also recorded by `TestKernel`.
pub(crate) enum Tapped<E> {
    Owned(E),
    Shared(Arc<E>),
}

impl<E> std::ops::Deref for Tapped<E> {
    type Target = E;

    fn deref(&self) -> &E {
        match self {
            Tapped::Owned(event) => event,
            Tapped::Shared(event) => event,
        }
    }
}

/// Events recorded for `TestKernel::emitted`.
pub(crate) struct Tap {
    pub(crate) plugin: Name,
    pub(crate) log: Vec<Arc<dyn Any + Send + Sync>>,
}

impl Shared {
    fn tap<E: Event>(&self, token: Option<&ActivationToken>, event: E) -> Tapped<E> {
        let Some(token) = token else {
            return Tapped::Owned(event);
        };
        let mut tap = lock(&self.tap);
        match tap.as_mut() {
            Some(tap) if tap.plugin == token.0.id => {
                let event = Arc::new(event);
                tap.log.push(event.clone());
                Tapped::Shared(event)
            }
            _ => Tapped::Owned(event),
        }
    }
}

/// An event subscription; dropping it unsubscribes from the next `emit` on.
pub struct Subscription {
    pub(crate) kernel: Weak<Shared>,
    pub(crate) type_id: TypeId,
    pub(crate) id: u64,
    pub(crate) keep: bool,
}

impl Subscription {
    /// Keeps the listener until the activation is deactivated.
    pub fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        if let Some(shared) = self.kernel.upgrade() {
            let mut garbage = Garbage::default();
            write(&shared.registry).remove_listener(self.type_id, self.id, &mut garbage);
            drop_garbage(garbage);
        }
    }
}
