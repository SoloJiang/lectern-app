//! Service registry and listener tables.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::{Arc, RwLock};
use std::thread;

use lectern_logging::warn;

use crate::diag::Phase;
use crate::handle::PluginHandle;
use crate::panic::contain_plugin;
use crate::service::ServiceKey;
use crate::token::{ActivationToken, Admission, TokenState};
use crate::util::{read, write};

pub(crate) type ListenerFn = Arc<dyn Fn(&dyn Any, &PluginHandle) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct Listener {
    pub(crate) id: u64,
    pub(crate) handle: PluginHandle,
    pub(crate) f: ListenerFn,
}

#[derive(Clone)]
pub(crate) struct ErasedProvider {
    pub(crate) token: ActivationToken,
    /// An `Arc<S>` behind `dyn Any`.
    pub(crate) obj: Arc<dyn Any + Send + Sync>,
}

/// The shared provider snapshot of one service, replaced as a whole whenever
/// providers change so `ServiceSet` handles can be kept long term.
pub(crate) struct ProviderCell {
    snapshot: RwLock<Arc<[ErasedProvider]>>,
}

impl ProviderCell {
    fn new() -> Arc<Self> {
        Arc::new(ProviderCell {
            snapshot: RwLock::new(Arc::from(Vec::new())),
        })
    }

    fn snapshot(&self) -> Arc<[ErasedProvider]> {
        read(&self.snapshot).clone()
    }
}

pub(crate) struct ServiceEntry {
    pub(crate) providers: Vec<ErasedProvider>,
    pub(crate) cell: Arc<ProviderCell>,
}

impl ServiceEntry {
    /// Publishes the provider list to the shared snapshot. Returns the old
    /// snapshot so the caller can drop it outside the registry lock.
    fn publish(&self) -> Arc<[ErasedProvider]> {
        let next: Arc<[ErasedProvider]> = Arc::from(self.providers.clone());
        std::mem::replace(&mut *write(&self.cell.snapshot), next)
    }
}

#[derive(Default)]
pub(crate) struct Registry {
    pub(crate) services: HashMap<TypeId, ServiceEntry>,
    pub(crate) listeners: HashMap<TypeId, Arc<[Listener]>>,
}

/// Values replaced under the registry lock, dropped after it is released.
#[derive(Default)]
pub(crate) struct Garbage {
    snapshots: Vec<Arc<[ErasedProvider]>>,
    listeners: Vec<Arc<[Listener]>>,
    providers: Vec<ErasedProvider>,
}

impl Garbage {
    pub(crate) fn is_empty(&self) -> bool {
        self.snapshots.is_empty() && self.listeners.is_empty() && self.providers.is_empty()
    }
}

impl Registry {
    pub(crate) fn cell(&mut self, key: ServiceKey) -> Arc<ProviderCell> {
        self.services
            .entry(key.type_id())
            .or_insert_with(|| ServiceEntry {
                providers: Vec::new(),
                cell: ProviderCell::new(),
            })
            .cell
            .clone()
    }

    pub(crate) fn add_provider(
        &mut self,
        key: ServiceKey,
        provider: ErasedProvider,
        garbage: &mut Garbage,
    ) {
        self.cell(key);
        if let Some(entry) = self.services.get_mut(&key.type_id()) {
            entry.providers.push(provider);
            garbage.snapshots.push(entry.publish());
        }
    }

    pub(crate) fn remove_provider(
        &mut self,
        key: ServiceKey,
        token: &ActivationToken,
        garbage: &mut Garbage,
    ) {
        if let Some(entry) = self.services.get_mut(&key.type_id()) {
            let (removed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut entry.providers)
                .into_iter()
                .partition(|p| p.token.same(token));
            entry.providers = kept;
            garbage.providers.extend(removed);
            garbage.snapshots.push(entry.publish());
        }
    }

    /// The first provider of `key` whose token is `Live`.
    pub(crate) fn live_provider(&self, key: ServiceKey) -> Option<ErasedProvider> {
        self.services
            .get(&key.type_id())?
            .providers
            .iter()
            .find(|p| p.token.state() == TokenState::Live)
            .cloned()
    }

    pub(crate) fn add_listener(
        &mut self,
        type_id: TypeId,
        listener: Listener,
        garbage: &mut Garbage,
    ) {
        let slot = self
            .listeners
            .entry(type_id)
            .or_insert_with(|| Arc::from(Vec::new()));
        let mut next: Vec<Listener> = slot.to_vec();
        next.push(listener);
        garbage
            .listeners
            .push(std::mem::replace(slot, Arc::from(next)));
    }

    pub(crate) fn remove_listener(&mut self, type_id: TypeId, id: u64, garbage: &mut Garbage) {
        if let Some(slot) = self.listeners.get_mut(&type_id)
            && slot.iter().any(|l| l.id == id)
        {
            let next: Vec<Listener> = slot.iter().filter(|l| l.id != id).cloned().collect();
            garbage
                .listeners
                .push(std::mem::replace(slot, Arc::from(next)));
        }
    }

    /// Removes every listener registered by this activation.
    pub(crate) fn remove_listeners_of(&mut self, token: &ActivationToken, garbage: &mut Garbage) {
        for slot in self.listeners.values_mut() {
            if slot.iter().any(|l| l.handle.act.same(token)) {
                let next: Vec<Listener> = slot
                    .iter()
                    .filter(|l| !l.handle.act.same(token))
                    .cloned()
                    .collect();
                garbage
                    .listeners
                    .push(std::mem::replace(slot, Arc::from(next)));
            }
        }
    }

    pub(crate) fn listeners(&self, type_id: TypeId) -> Option<Arc<[Listener]>> {
        self.listeners.get(&type_id).cloned()
    }
}

/// Drops replaced registry values outside the lock, isolating panics in
/// closures and services the kernel releases (logged as `plugin failed` in
/// the `drop` phase, under the caller's scope).
pub(crate) fn drop_garbage(garbage: Garbage) {
    if garbage.is_empty() {
        return;
    }
    let _ = contain_plugin(Phase::Drop, move || drop(garbage));
}

/// Handle to a single-provider service, kept in the `Active` state. Each call
/// takes the provider's admission through [`get`](Self::get).
pub struct ServiceRef<S: ?Sized> {
    pub(crate) arc: Arc<S>,
    pub(crate) token: ActivationToken,
}

impl<S: ?Sized> Clone for ServiceRef<S> {
    fn clone(&self) -> Self {
        ServiceRef {
            arc: self.arc.clone(),
            token: self.token.clone(),
        }
    }
}

impl<S: ?Sized> ServiceRef<S> {
    /// The service with the provider's admission held until the returned value
    /// is dropped, or `None` once the provider is no longer live.
    pub fn get(&self) -> Option<Admitted<S>> {
        let admission = self.token.enter()?;
        Some(Admitted {
            arc: self.arc.clone(),
            admission,
        })
    }
}

/// A service reference holding one provider's admission. Use it within one
/// call; do not store it.
pub struct Admitted<S: ?Sized> {
    arc: Arc<S>,
    admission: Admission,
}

impl<S: ?Sized> Deref for Admitted<S> {
    type Target = S;

    fn deref(&self) -> &S {
        &self.arc
    }
}

impl<S: ?Sized> Drop for Admitted<S> {
    fn drop(&mut self) {
        if thread::panicking() {
            warn!(
                "unwinding while holding a provider's admission";
                provider_act = self.admission.act_id(),
            );
        }
    }
}

/// The current providers of a multi-provider service. The kernel replaces the
/// snapshot as providers come and go, so the handle can be taken once in
/// `activate` and kept.
pub struct ServiceSet<S: ?Sized> {
    pub(crate) cell: Arc<ProviderCell>,
    pub(crate) _marker: PhantomData<fn() -> Box<S>>,
}

impl<S: ?Sized> Clone for ServiceSet<S> {
    fn clone(&self) -> Self {
        ServiceSet {
            cell: self.cell.clone(),
            _marker: PhantomData,
        }
    }
}

impl<S: ?Sized + 'static> ServiceSet<S> {
    /// Every live provider, each holding its admission until the `Vec` is
    /// dropped. Allocates; hot paths use [`for_each`](Self::for_each).
    pub fn current(&self) -> Vec<Admitted<S>> {
        self.cell
            .snapshot()
            .iter()
            .filter_map(|provider| {
                let admission = provider.token.enter()?;
                let arc = provider.obj.downcast_ref::<Arc<S>>()?.clone();
                Some(Admitted { arc, admission })
            })
            .collect()
    }

    /// Calls `f` for each live provider, holding one admission at a time.
    /// Does not allocate.
    pub fn for_each(&self, mut f: impl FnMut(&S)) {
        let snapshot = self.cell.snapshot();
        for provider in snapshot.iter() {
            let Some(admission) = provider.token.enter() else {
                continue;
            };
            if let Some(arc) = provider.obj.downcast_ref::<Arc<S>>() {
                f(arc);
            }
            drop(admission);
        }
    }
}
