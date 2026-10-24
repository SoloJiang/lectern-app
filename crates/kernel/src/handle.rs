//! The activation context, the long-lived plugin handle and the control handle.

use std::any::{Any, TypeId};
use std::marker::PhantomData;
use std::sync::Arc;

use crate::background::BackgroundGuard;
use crate::diag::Phase;
use crate::event::{Emitter, Event, ServiceAdded, ServiceRemoved, Subscription, dispatch};
use crate::gate::{GateInner, SubmitGate};
use crate::kernel::{Request, Shared, WorkspaceFacts, invalidate};
use crate::panic::catch;
use crate::plugin::{Manifest, Name, PluginError, Staged};
use crate::registry::{Garbage, Listener, ListenerFn, ServiceRef, ServiceSet, drop_garbage};
use crate::service::{Service, ServiceKey, service};
use crate::token::{ActivationToken, BusyGuard, Entry, StagedItem, TokenState, drop_rejected};
use crate::util::{lock, read, write};

pub(crate) type ServiceEventFn = fn(&Arc<Shared>, &Name);

/// A service registered during `activate`, published at commit.
pub(crate) struct StagedService {
    pub(crate) key: ServiceKey,
    pub(crate) obj: Arc<dyn Any + Send + Sync>,
    pub(crate) added: ServiceEventFn,
    pub(crate) removed: ServiceEventFn,
}

fn emit_added<S: Service + ?Sized>(shared: &Arc<Shared>, provider: &Name) {
    dispatch(shared, None, ServiceAdded::<S>::new(provider.clone()));
}

fn emit_removed<S: Service + ?Sized>(shared: &Arc<Shared>, provider: &Name) {
    dispatch(shared, None, ServiceRemoved::<S>::new(provider.clone()));
}

/// The context of one `activate` call. Everything registered here is held in
/// this activation and published in one step only after `activate` returns
/// `Ok` (`docs/design/kernel.md`, 激活的提交).
pub struct Context<'a> {
    pub(crate) shared: &'a Arc<Shared>,
    pub(crate) manifest: &'a Manifest,
    pub(crate) token: ActivationToken,
    pub(crate) services: Vec<StagedService>,
    pub(crate) listeners: Vec<(TypeId, Listener)>,
    pub(crate) guards: Vec<Box<dyn Send>>,
    pub(crate) staged: Vec<Box<dyn Staged>>,
}

impl<'a> Context<'a> {
    pub(crate) fn new(
        shared: &'a Arc<Shared>,
        manifest: &'a Manifest,
        token: ActivationToken,
    ) -> Self {
        Context {
            shared,
            manifest,
            token,
            services: Vec::new(),
            listeners: Vec::new(),
            guards: Vec::new(),
            staged: Vec::new(),
        }
    }

    /// Provides a service. Its key must be in the manifest's `provides`.
    pub fn provide<S: Service + ?Sized>(
        &mut self,
        service_object: Arc<S>,
    ) -> Result<(), PluginError> {
        let key = service::<S>();
        if !self.manifest.provides.contains(&key) {
            return Err(PluginError::UndeclaredService(S::ID));
        }
        if self.services.iter().any(|s| s.key == key) {
            return Err(PluginError::DuplicateService(S::ID));
        }
        self.services.push(StagedService {
            key,
            obj: Arc::new(service_object),
            added: emit_added::<S>,
            removed: emit_removed::<S>,
        });
        Ok(())
    }

    /// A required single-provider service. Its key must be in the manifest's
    /// `requires`; using it for a `MULTIPLE` service fails to compile.
    ///
    /// # Panics
    ///
    /// If the key is not in `requires`. The panic is caught at the `activate`
    /// boundary and fails the plugin.
    pub fn service<S: Service + ?Sized>(&self) -> ServiceRef<S> {
        const { assert!(!S::MULTIPLE, "use service_set() for MULTIPLE services") };
        let key = self.required::<S>();
        let provider = read(&self.shared.registry).live_provider(key);
        let Some(provider) = provider else {
            panic!("required service {} has no live provider", S::ID);
        };
        let Some(arc) = provider.obj.downcast_ref::<Arc<S>>().cloned() else {
            panic!("service {} was registered with another type", S::ID);
        };
        ServiceRef {
            arc,
            token: provider.token,
        }
    }

    /// The provider set of a required multi-provider service. Its key must be
    /// in the manifest's `requires`; using it for a single-provider service
    /// fails to compile.
    ///
    /// # Panics
    ///
    /// If the key is not in `requires`.
    pub fn service_set<S: Service + ?Sized>(&self) -> ServiceSet<S> {
        const { assert!(S::MULTIPLE, "use service() for single-provider services") };
        let key = self.required::<S>();
        let cell = write(&self.shared.registry).cell(key);
        ServiceSet {
            cell,
            _marker: PhantomData,
        }
    }

    fn required<S: Service + ?Sized>(&self) -> ServiceKey {
        let key = service::<S>();
        assert!(
            self.manifest.requires.contains(&key),
            "service {} is not in the manifest's requires",
            S::ID
        );
        key
    }

    /// Subscribes to an event. The listener is published at commit and skipped
    /// until this activation is live.
    pub fn on<E: Event>(&mut self, handler: impl Fn(&E, &PluginHandle) + Send + Sync + 'static) {
        let listener = Listener {
            id: self.shared.next_listener_id(),
            handle: PluginHandle {
                act: self.token.clone(),
            },
            f: erase(handler),
        };
        self.listeners.push((TypeId::of::<E>(), listener));
    }

    /// A guard without side effects on creation, dropped when the plugin is
    /// deactivated.
    pub fn own(&mut self, guard: impl Send + 'static) {
        self.guards.push(Box::new(guard));
    }

    /// A guard that takes effect at commit, such as a `dyn Contributions`
    /// registration. Dropped without `commit` if the activation fails first.
    pub fn own_staged(&mut self, guard: impl Staged) {
        self.staged.push(Box::new(guard));
    }
}

fn erase<E: Event>(handler: impl Fn(&E, &PluginHandle) + Send + Sync + 'static) -> ListenerFn {
    Arc::new(move |event: &dyn Any, handle: &PluginHandle| {
        if let Some(event) = event.downcast_ref::<E>() {
            handler(event, handle);
        }
    })
}

/// The long-lived handle of one activation, valid from `started` until the
/// activation is deactivated. After that (and after a re-activation) every
/// operation on it is a no-op.
#[derive(Clone)]
pub struct PluginHandle {
    pub(crate) act: ActivationToken,
}

impl PluginHandle {
    /// Hands a guard to the kernel, dropped when this activation is revoked.
    /// Returns false once the activation is deactivated; the guard is then
    /// dropped on this thread, under `contain`.
    pub fn own(&self, guard: impl Send + 'static) -> bool {
        let _scope = self.act.scope();
        let mut table = lock(&self.act.0.guards);
        if table.closed || self.act.state() == TokenState::Dead {
            drop(table);
            drop_rejected(guard);
            return false;
        }
        table.push(Entry::Guard(Box::new(guard)));
        true
    }

    /// Submits work through `start` and keeps the handle it returns.
    ///
    /// `start` receives a [`SubmitGate`] to pass to the executor: tasks are not
    /// polled until the handle is registered and the gate opens. If `start`
    /// panics or the activation dies first, the gate closes and the tasks are
    /// dropped without running. Inside `started` the call is held until the
    /// activation goes live. Returns whether the work was accepted.
    pub fn own_with<G: Send + 'static>(
        &self,
        start: impl FnOnce(SubmitGate) -> G + Send + 'static,
    ) -> bool {
        if self.act.state() == TokenState::Pending {
            let mut staging = lock(&self.act.0.staging);
            if self.act.state() == TokenState::Pending && !staging.sealed {
                staging
                    .items
                    .push(StagedItem::Start(Box::new(move |handle: &PluginHandle| {
                        handle.own_with_live(start);
                    })));
                return true;
            }
        }
        self.own_with_live(start)
    }

    pub(crate) fn own_with_live<G: Send + 'static>(
        &self,
        start: impl FnOnce(SubmitGate) -> G + Send + 'static,
    ) -> bool {
        let _scope = self.act.scope();
        let Some(admission) = self.act.enter() else {
            drop_rejected(start);
            return false;
        };
        let gate = GateInner::new(self.act.clone());
        // Step 1: a provisional registration, so a failure while `start` runs
        // finds the gate.
        let seq = {
            let mut table = lock(&self.act.0.guards);
            if table.closed || self.act.state() == TokenState::Dead {
                drop(table);
                drop_rejected(start);
                drop(admission);
                return false;
            }
            table.push(Entry::Task {
                gate: gate.clone(),
                handle: None,
            })
        };
        gate.set_seq(seq);

        // Step 2: `start` runs without the lock.
        let submitted = catch(|| {
            start(SubmitGate {
                inner: gate.clone(),
            })
        });
        let handle = match submitted {
            Ok(handle) => handle,
            Err(payload) => {
                gate.close();
                let removed = lock(&self.act.0.guards).take(seq);
                drop_rejected(removed);
                payload.report(Phase::OwnWith);
                let message = payload.dispose();
                invalidate(
                    &self.act,
                    Some(PluginError::Panicked(format!(
                        "own_with start panicked: {message}"
                    ))),
                );
                drop(admission);
                return false;
            }
        };

        // Step 3: register the handle, then open the gate outside the lock.
        let mut table = lock(&self.act.0.guards);
        if !table.closed && self.act.state() != TokenState::Dead {
            if let Some((_, Entry::Task { handle: slot, .. })) =
                table.entries.iter_mut().find(|(s, _)| *s == seq)
            {
                *slot = Some(Box::new(handle));
            }
            let reclaim_now = gate.seal_registered();
            drop(table);
            gate.open();
            if reclaim_now {
                gate.request_reclaim(seq);
            }
            drop(admission);
            true
        } else {
            let removed = table.take(seq);
            drop(table);
            gate.close();
            drop_rejected(removed);
            drop_rejected(handle);
            drop(admission);
            false
        }
    }

    /// Subscribes to an event until the returned `Subscription` is dropped (or
    /// kept until deactivation). Returns `None` once the activation is
    /// deactivated.
    pub fn on<E: Event>(
        &self,
        handler: impl Fn(&E, &PluginHandle) + Send + Sync + 'static,
    ) -> Option<Subscription> {
        let _scope = self.act.scope();
        let f = erase(handler);
        let Some(shared) = self.act.0.kernel.upgrade() else {
            drop_rejected(f);
            return None;
        };
        let mut table = lock(&self.act.0.guards);
        if table.closed || self.act.state() == TokenState::Dead {
            drop(table);
            drop_rejected(f);
            return None;
        }
        let id = shared.next_listener_id();
        let type_id = TypeId::of::<E>();
        let mut garbage = Garbage::default();
        write(&shared.registry).add_listener(
            type_id,
            Listener {
                id,
                handle: self.clone(),
                f,
            },
            &mut garbage,
        );
        table.push(Entry::Listener);
        drop(table);
        drop_garbage(garbage);
        Some(Subscription {
            kernel: Arc::downgrade(&shared),
            type_id,
            id,
            keep: false,
        })
    }

    /// Declares outstanding work. Works inside `started` too. Returns `None`
    /// once the activation is deactivated.
    pub fn busy(&self) -> Option<BusyGuard> {
        let admission = self.act.enter_busy()?;
        let guard = self.act.0.counters.busy();
        drop(admission);
        Some(guard)
    }

    /// Declares pausable background work (ADR 0029), taken by the same rules
    /// as [`busy`](PluginHandle::busy) but counted apart: it does not hold off
    /// quiescence. Move it into the task and check
    /// [`BackgroundGuard::proceed`] before every batch. Returns `None` once the
    /// activation is deactivated. The work does not yield to interaction
    /// (`code_intel`'s tier 3); see
    /// [`background_yielding`](PluginHandle::background_yielding).
    pub fn background(&self) -> Option<BackgroundGuard> {
        self.background_guard(false)
    }

    /// Declares pausable background work that also yields to interaction
    /// (ADR 0033): `code_intel`'s tier 4, dependency tier and tier 5, and the
    /// memory-only batch work. Taken by the same rules as
    /// [`background`](PluginHandle::background); while
    /// [`Kernel::set_yield`](crate::Kernel::set_yield) is on the work moves
    /// only in keepalive slots, see [`BackgroundGuard::proceed_item`].
    pub fn background_yielding(&self) -> Option<BackgroundGuard> {
        self.background_guard(true)
    }

    fn background_guard(&self, yields: bool) -> Option<BackgroundGuard> {
        let shared = self.act.0.kernel.upgrade()?;
        let admission = self.act.enter_background()?;
        let guard = shared.background.guard(yields);
        drop(admission);
        Some(guard)
    }

    /// An emitter bound to this activation.
    pub fn emitter(&self) -> Emitter {
        Emitter {
            kernel: self.act.0.kernel.clone(),
            token: Some(self.act.clone()),
        }
    }

    pub fn is_active(&self) -> bool {
        self.act.state() == TokenState::Live
    }

    /// The token of this activation, the same one `Staged::commit` receives.
    pub fn token(&self) -> ActivationToken {
        self.act.clone()
    }

    /// Reports that this activation failed in the background. Same as
    /// [`ActivationToken::fail`] on this activation's token: before returning,
    /// this activation and the plugins that lose their last provider are marked
    /// dead on this thread; the deactivation is queued.
    pub fn fail(&self, error: PluginError) {
        self.act.fail(error);
    }

    /// A restricted kernel control handle bound to this activation.
    pub fn control(&self) -> KernelControl {
        KernelControl {
            act: self.act.clone(),
        }
    }
}

/// Kernel control for plugins that drive activation (`project`,
/// `workspace`). Requests sent after the activation is deactivated are
/// ignored.
#[derive(Clone)]
pub struct KernelControl {
    pub(crate) act: ActivationToken,
}

impl KernelControl {
    fn shared(&self) -> Option<Arc<Shared>> {
        if self.act.state() == TokenState::Dead {
            return None;
        }
        self.act.0.kernel.upgrade()
    }

    /// Submits the facts of the open project. They belong to this activation
    /// and are cleared when it is deactivated.
    pub fn workspace_changed(&self, facts: Arc<dyn WorkspaceFacts>) {
        match self.shared() {
            Some(shared) => shared.enqueue(Request::Workspace {
                facts,
                from: self.act.clone(),
            }),
            None => {
                let _scope = self.act.scope();
                drop_rejected(facts);
            }
        }
    }

    /// Activates a plugin with a `Manual` condition.
    pub fn activate(&self, id: &str) {
        if let Some(shared) = self.shared() {
            shared.enqueue(Request::Activate {
                id: id.to_owned(),
                from: Some(self.act.clone()),
            });
        }
    }

    /// Every `Command` name in every manifest.
    pub fn lazy_commands(&self) -> Vec<Name> {
        match self.act.0.kernel.upgrade() {
            Some(shared) => shared.lazy_commands(),
            None => Vec::new(),
        }
    }

    /// Activates the plugin declaring `name`. `done` runs inside this
    /// activation's admission; if this activation is no longer live by then,
    /// `done` is dropped without being called.
    pub fn command(
        &self,
        name: &str,
        done: impl FnOnce(Result<ActivationToken, PluginError>) + Send + 'static,
    ) {
        match self.shared() {
            Some(shared) => shared.enqueue(Request::Command {
                name: name.to_owned(),
                done: Box::new(done),
                requester: Some(self.act.clone()),
            }),
            None => {
                let _scope = self.act.scope();
                drop_rejected(done);
            }
        }
    }
}
