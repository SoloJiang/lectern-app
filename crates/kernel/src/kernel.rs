//! The kernel thread: activation, revocation, failure and shutdown.

use std::mem;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use lectern_logging::{PluginTag, Value, error, info, warn};

use crate::background::{Background, BackgroundStatus};
use crate::diag::{Phase, plugin_scope, plugin_tag};
use crate::event::{BackgroundChanged, Busy, Emitter, Quiescent, Tap, dispatch_transition};
use crate::handle::{Context, PluginHandle, ServiceEventFn};
use crate::panic::{contain_plugin, run_shutdown_callback};
use crate::plugin::{
    Activation, Active, Manifest, Name, Plugin, PluginError, PluginState, PluginStatus,
};
use crate::registry::{ErasedProvider, Garbage, Registry, drop_garbage};
use crate::service::ServiceKey;
use crate::set::{PluginSet, Plugins, PreparedSet, analyze, manifest_failure, read_manifest};
use crate::token::{
    ActivationToken, Counters, Entry, StagedItem, TokenState, Transition, drop_rejected,
};
use crate::util::{lock, write};

/// An immutable snapshot of the open project's files. `contains` only reads
/// memory and may be called on any thread. The last reference may be released
/// on any thread, so `Drop` must not panic.
pub trait WorkspaceFacts: Send + Sync {
    /// Whether the project has a file matching `pattern`.
    fn contains(&self, pattern: &str) -> bool;
}

type Done = Box<dyn FnOnce(Result<ActivationToken, PluginError>) + Send>;

/// Requests for the kernel thread. Every request except `Transition` counts
/// as a busy unit from enqueue until it is handled. `Barrier` and `Shutdown`
/// answer only after that unit is released.
pub(crate) enum Request {
    Start,
    Activate {
        id: String,
        from: Option<ActivationToken>,
    },
    SetEnabled {
        id: String,
        enabled: bool,
    },
    Command {
        name: String,
        done: Done,
        requester: Option<ActivationToken>,
    },
    Workspace {
        facts: Arc<dyn WorkspaceFacts>,
        from: ActivationToken,
    },
    /// Revoke activations that were marked dead.
    Sweep,
    /// Every task of an `own_with` submission finished; drop its handle.
    Reclaim {
        token: ActivationToken,
        seq: u64,
    },
    Transition(Transition),
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        expect(
            dead_code,
            reason = "only `Kernel::barrier` (tests and `test-support`) sends this"
        )
    )]
    Barrier(Sender<()>),
    Shutdown(Box<dyn FnOnce() + Send>),
}

/// Which plugin is currently activated in each slot, with its manifest edges.
/// Guarded by the invalidation lock (`Shared::live`).
pub(crate) struct LiveTable {
    slots: Vec<LiveSlot>,
}

struct LiveSlot {
    act: Option<ActivationToken>,
    provides: Vec<ServiceKey>,
    requires: Vec<ServiceKey>,
}

/// State shared between the kernel thread and every other thread.
pub(crate) struct Shared {
    pub(crate) counters: Arc<Counters>,
    pub(crate) background: Arc<Background>,
    sender: Mutex<Option<Sender<Request>>>,
    pub(crate) registry: RwLock<Registry>,
    live: Mutex<LiveTable>,
    status: Mutex<Vec<PluginStatus>>,
    commands: Mutex<Option<Vec<Name>>>,
    commands_ready: Condvar,
    next_listener: AtomicU64,
    pub(crate) tap: Mutex<Option<Tap>>,
}

impl Shared {
    /// Queues a lifecycle request. After shutdown the request is rejected on
    /// this thread.
    pub(crate) fn enqueue(&self, request: Request) {
        self.counters.inc();
        let rejected = match lock(&self.sender).as_ref() {
            Some(sender) => sender.send(request).err().map(|error| error.0),
            None => Some(request),
        };
        if let Some(request) = rejected {
            reject(request, &self.counters);
        }
    }

    fn notify(&self, transition: Transition) {
        if let Some(sender) = lock(&self.sender).as_ref() {
            let _ = sender.send(Request::Transition(transition));
        }
    }

    pub(crate) fn next_listener_id(&self) -> u64 {
        self.next_listener.fetch_add(1, SeqCst)
    }

    pub(crate) fn lazy_commands(&self) -> Vec<Name> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut commands = lock(&self.commands);
        loop {
            if let Some(list) = commands.as_ref() {
                return list.clone();
            }
            let now = Instant::now();
            if now >= deadline {
                return Vec::new();
            }
            commands = match self.commands_ready.wait_timeout(commands, deadline - now) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }
}

/// Handles a request that arrived after shutdown, on the calling thread, and
/// releases its busy unit. A barrier or shutdown reply goes out only after the
/// release, as on the kernel thread, so whoever it wakes no longer counts this
/// request as busy. A command callback runs before the release, as it does
/// when the command is handled.
fn reject(request: Request, counters: &Counters) {
    match request {
        Request::Command {
            done, requester, ..
        } => {
            call_done(done, requester.as_ref(), Err(PluginError::ShuttingDown));
            counters.dec();
        }
        Request::Shutdown(done) => {
            counters.dec();
            run_shutdown_callback(done);
        }
        Request::Barrier(reply) => {
            counters.dec();
            let _ = reply.send(());
        }
        // Not counted: `notify` sends these directly, never through `enqueue`.
        Request::Transition(transition) => drop_rejected(transition),
        other => {
            drop_rejected(other);
            counters.dec();
        }
    }
}

/// Calls a command callback inside the requester's admission and log scope.
/// A requester that is no longer live never sees the callback run.
fn call_done(
    done: Done,
    requester: Option<&ActivationToken>,
    result: Result<ActivationToken, PluginError>,
) {
    let _scope = requester.map(ActivationToken::scope);
    let admission = match requester {
        Some(token) => match token.enter() {
            Some(admission) => Some(admission),
            None => {
                drop_rejected(done);
                return;
            }
        },
        None => None,
    };
    let _ = contain_plugin(Phase::Command, move || done(result));
    drop(admission);
}

/// Marks an activation dead on the calling thread, together with every
/// activation that loses its last live provider as a result, and queues the
/// deactivation. All invalidations take the same lock, so two concurrent ones
/// always see each other's result. Nothing inside the lock waits or runs
/// plugin code; gates are woken after it is released.
pub(crate) fn invalidate(token: &ActivationToken, failure: Option<PluginError>) {
    let Some(shared) = token.0.kernel.upgrade() else {
        if let Some(failure) = failure {
            token.record_failure(failure);
        }
        token.mark_dead();
        token.wake_gates();
        return;
    };
    let affected = {
        let live = lock(&shared.live);
        if let Some(failure) = failure {
            token.record_failure(failure);
        }
        let mut affected = Vec::new();
        if token.mark_dead() {
            affected.push(token.clone());
        }
        cascade(&live, &mut affected);
        affected
    };
    for token in &affected {
        token.wake_gates();
    }
    if !affected.is_empty() {
        shared.enqueue(Request::Sweep);
    }
}

fn cascade(live: &LiveTable, affected: &mut Vec<ActivationToken>) {
    loop {
        let mut changed = false;
        for (index, slot) in live.slots.iter().enumerate() {
            let Some(act) = &slot.act else { continue };
            if act.state() == TokenState::Dead {
                continue;
            }
            let unsatisfied = slot.requires.iter().any(|key| {
                !live.slots.iter().enumerate().any(|(other, provider)| {
                    other != index
                        && provider.provides.contains(key)
                        && provider
                            .act
                            .as_ref()
                            .is_some_and(|a| a.state() == TokenState::Live)
                })
            });
            if unsatisfied && act.mark_dead() {
                affected.push(act.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// The plugin kernel. `Send + Sync`; `app` holds an `Arc<Kernel>`.
pub struct Kernel {
    shared: Arc<Shared>,
}

impl Kernel {
    /// Starts the kernel thread and returns at once. The thread reads the
    /// manifests, checks the dependency graph and activates every
    /// `AfterFirstFrame` plugin in dependency order. Call it only after the
    /// startup barrier is released.
    pub fn start(plugins: PluginSet) -> Arc<Kernel> {
        Kernel::start_with_tap(Plugins::Set(plugins), None)
    }

    /// Like [`start`](Kernel::start), but with the manifests and timings
    /// [`PluginSet::prepare`] and [`PreparedSet::with`] already took:
    /// `manifest()` is not called again, `status()` reports those timings as
    /// `manifest_time`, and a plugin whose `manifest()` panicked is `Failed`.
    pub fn start_prepared(plugins: PreparedSet) -> Arc<Kernel> {
        Kernel::start_with_tap(Plugins::Prepared(plugins), None)
    }

    pub(crate) fn start_with_tap(plugins: Plugins, tap: Option<Name>) -> Arc<Kernel> {
        let counters = Arc::new(Counters::new());
        let background = Arc::new(Background::new());
        let (sender, receiver) = mpsc::channel();
        let shared = Arc::new(Shared {
            counters: counters.clone(),
            background: background.clone(),
            sender: Mutex::new(Some(sender)),
            registry: RwLock::new(Registry::default()),
            live: Mutex::new(LiveTable { slots: Vec::new() }),
            status: Mutex::new(Vec::new()),
            commands: Mutex::new(None),
            commands_ready: Condvar::new(),
            next_listener: AtomicU64::new(0),
            tap: Mutex::new(tap.map(|plugin| Tap {
                plugin,
                log: Vec::new(),
            })),
        });
        let weak = Arc::downgrade(&shared);
        counters.set_notify(Box::new(move |transition| {
            if let Some(shared) = weak.upgrade() {
                shared.notify(transition);
            }
        }));
        let weak = Arc::downgrade(&shared);
        background.set_notify(Box::new(move |transition| {
            if let Some(shared) = weak.upgrade() {
                shared.notify(transition);
            }
        }));
        shared.enqueue(Request::Start);
        let thread_shared = shared.clone();
        let spawned = thread::Builder::new()
            .name("kernel".to_owned())
            .spawn(move || run(thread_shared, plugins, receiver));
        if let Err(error) = spawned {
            error!("could not start the kernel thread"; error = Value::io(&error));
        }
        Arc::new(Kernel { shared })
    }

    /// Activates a plugin that declares `Manual`. If its dependencies are not
    /// ready yet, the request is kept until they are.
    pub fn activate(&self, id: &str) {
        self.shared.enqueue(Request::Activate {
            id: id.to_owned(),
            from: None,
        });
    }

    /// Every `Command` name in every manifest, for the view layer's proxy
    /// actions.
    pub fn lazy_commands(&self) -> Vec<Name> {
        self.shared.lazy_commands()
    }

    /// Activates the plugin declaring command `name` and calls `done` on the
    /// kernel thread once the activation is live (with its token) or has
    /// failed (with the error).
    pub fn command(
        &self,
        name: &str,
        done: impl FnOnce(Result<ActivationToken, PluginError>) + Send + 'static,
    ) {
        self.shared.enqueue(Request::Command {
            name: name.to_owned(),
            done: Box::new(done),
            requester: None,
        });
    }

    /// Disables or re-enables a plugin. Disabling also deactivates the plugins
    /// that depend on its services.
    pub fn set_enabled(&self, id: &str, enabled: bool) {
        self.shared.enqueue(Request::SetEnabled {
            id: id.to_owned(),
            enabled,
        });
    }

    /// Deactivates every plugin in reverse dependency order, then calls `done`
    /// after the kernel thread has finished.
    pub fn shutdown(&self, done: impl FnOnce() + Send + 'static) {
        self.shared.enqueue(Request::Shutdown(Box::new(done)));
    }

    /// An emitter that belongs to no plugin (for `app`).
    pub fn emitter(&self) -> Emitter {
        Emitter {
            kernel: Arc::downgrade(&self.shared),
            token: None,
        }
    }

    pub fn status(&self) -> Vec<PluginStatus> {
        lock(&self.shared.status).clone()
    }

    pub fn is_quiescent(&self) -> bool {
        self.shared.counters.epoch().is_some()
    }

    /// The current quiescent epoch, or `None` while busy. Two calls returning
    /// the same `Some(n)` mean the kernel stayed quiescent in between.
    pub fn quiescent_epoch(&self) -> Option<u64> {
        self.shared.counters.epoch()
    }

    /// Turns the global pause switch on. Every piece of background work stops
    /// at its next batch boundary, and work that starts during the pause
    /// stops before its first batch. Once all of it has stopped,
    /// [`background`](Kernel::background) reports
    /// [`Paused`](crate::BackgroundState::Paused). Only for measurement
    /// (ADR 0029); it does not affect quiescence.
    pub fn pause(&self) {
        self.shared.background.pause();
    }

    /// Turns the pause switch off: every stopped piece of work continues from
    /// where it stopped.
    pub fn resume(&self) {
        self.shared.background.resume();
    }

    /// The state, count and epoch of the background work, read in one step.
    /// Independent of [`quiescent_epoch`](Kernel::quiescent_epoch).
    pub fn background(&self) -> BackgroundStatus {
        self.shared.background.status()
    }

    /// The interaction yield switch (ADR 0033), separate from the pause
    /// switch and off by default. While on, work whose guard was taken with
    /// [`background_yielding`](crate::PluginHandle::background_yielding)
    /// stops at its next slice boundary, keeps its state and moves only in
    /// keepalive slots: one at a time across all yielding work, each at most
    /// [`SLOT_LENGTH`](crate::SLOT_LENGTH), starts at least
    /// [`SLOT_INTERVAL`](crate::SLOT_INTERVAL) apart, one item each, items
    /// over [`SLOT_MAX_BYTES`](crate::SLOT_MAX_BYTES) waiting for the switch
    /// to turn off. Other background work runs as usual. It changes neither
    /// `pause_requested`, the background state, count and epoch, nor
    /// quiescence; with both switches on the pause wins. Turning it off wakes
    /// every waiting piece of work. Callable on any thread.
    pub fn set_yield(&self, on: bool) {
        self.shared.background.yielding.set(on);
    }

    /// Replaces the clock of the keepalive slots, before any yielding work
    /// waits (tests use [`ManualClock`](crate::testing::ManualClock)).
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_clock(&self, clock: Arc<dyn crate::Clock>) {
        self.shared.background.yielding.set_clock(clock);
    }

    /// Waits until every request queued before this call has been handled.
    /// The barrier's own busy unit is released before it returns, so with
    /// nothing else busy `is_quiescent()` is already true. A request the
    /// kernel thread queued for itself while handling those (the `Sweep`
    /// after a failure) may still be queued behind the barrier.
    #[cfg(any(test, feature = "test-support"))]
    pub fn barrier(&self) {
        let (reply, wait) = mpsc::channel();
        self.shared.enqueue(Request::Barrier(reply));
        let _ = wait.recv_timeout(Duration::from_secs(10));
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn tapped(&self) -> Vec<Arc<dyn std::any::Any + Send + Sync>> {
        lock(&self.shared.tap)
            .as_ref()
            .map(|tap| tap.log.clone())
            .unwrap_or_default()
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        self.shared.enqueue(Request::Shutdown(Box::new(|| {})));
    }
}

struct Waiter {
    done: Done,
    requester: Option<ActivationToken>,
}

struct Current {
    token: ActivationToken,
    active: Option<Box<dyn Active>>,
    services: Vec<(ServiceKey, ServiceEventFn)>,
    /// `ServiceAdded` was announced, so removal is announced too.
    announced: bool,
}

struct Slot {
    id: Name,
    /// The log tag of `id`, registered once when the plugin is loaded.
    tag: Option<PluginTag>,
    plugin: Option<Arc<dyn Plugin>>,
    manifest: Option<Manifest>,
    enabled: bool,
    failed: Option<PluginError>,
    command_met: bool,
    manual_met: bool,
    workspace_met: bool,
    current: Option<Current>,
    generation: u64,
    waiters: Vec<Waiter>,
    manifest_time: Option<Duration>,
    activate_time: Option<Duration>,
    commit_time: Option<Duration>,
    started_time: Option<Duration>,
}

struct Lifecycle {
    shared: Arc<Shared>,
    slots: Vec<Slot>,
    order: Vec<usize>,
    started: bool,
    shutting_down: bool,
    facts: Option<(Arc<dyn WorkspaceFacts>, ActivationToken)>,
}

impl Slot {
    fn new(id: Name, plugin: Arc<dyn Plugin>) -> Slot {
        Slot {
            tag: plugin_tag(&id),
            id,
            plugin: Some(plugin),
            manifest: None,
            enabled: true,
            failed: None,
            command_met: false,
            manual_met: false,
            workspace_met: false,
            current: None,
            generation: 0,
            waiters: Vec::new(),
            manifest_time: None,
            activate_time: None,
            commit_time: None,
            started_time: None,
        }
    }

    /// Logs info `plugin activated` with the timings of the latest
    /// activation, which just went live.
    fn log_activated(&self) {
        let took: Duration = [self.activate_time, self.commit_time, self.started_time]
            .into_iter()
            .flatten()
            .sum();
        info!(
            "plugin activated";
            took_us = took,
            activate_us = self.activate_time,
            commit_us = self.commit_time,
            started_us = self.started_time,
        );
    }
}

fn run(shared: Arc<Shared>, plugins: Plugins, receiver: Receiver<Request>) {
    let slots = match plugins {
        Plugins::Set(set) => set
            .plugins
            .into_iter()
            .map(|(id, plugin)| Slot::new(id, plugin))
            .collect(),
        // Already read: `start` sees `manifest_time` and skips these.
        Plugins::Prepared(set) => set
            .plugins
            .into_iter()
            .map(|prepared| {
                let mut slot = Slot::new(prepared.id, prepared.plugin);
                slot.manifest_time = Some(prepared.time);
                match prepared.manifest {
                    Ok(manifest) => slot.manifest = Some(manifest),
                    Err(message) => slot.failed = Some(manifest_failure(&message)),
                }
                slot
            })
            .collect(),
    };
    let mut lifecycle = Lifecycle {
        shared: shared.clone(),
        slots,
        order: Vec::new(),
        started: false,
        shutting_down: false,
        facts: None,
    };

    let mut finish: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
    while let Ok(request) = receiver.recv() {
        match request {
            Request::Transition(transition) => lifecycle.transition(transition),
            Request::Shutdown(done) => {
                lifecycle.shutdown();
                finish.push(done);
                shared.counters.dec();
                break;
            }
            Request::Barrier(reply) => {
                lifecycle.settle();
                // Release this request's busy unit before waking the waiter,
                // or `is_quiescent()` right after `barrier()` reads busy (#96).
                shared.counters.dec();
                let _ = reply.send(());
            }
            request => {
                lifecycle.handle(request);
                lifecycle.settle();
                shared.counters.dec();
            }
        }
    }

    // Close the queue, then reject whatever was sent before it closed.
    let rest: Vec<Request> = {
        let mut sender = lock(&shared.sender);
        *sender = None;
        receiver.try_iter().collect()
    };
    drop(receiver);
    for request in rest {
        match request {
            Request::Shutdown(done) => {
                finish.push(done);
                shared.counters.dec();
            }
            request => reject(request, &shared.counters),
        }
    }
    drop(lifecycle);
    for done in finish {
        run_shutdown_callback(done);
    }
}

impl Lifecycle {
    fn handle(&mut self, request: Request) {
        match request {
            Request::Start => self.start(),
            Request::Activate { id, from } => {
                if from.is_some_and(|token| token.state() == TokenState::Dead) {
                    return;
                }
                match self.find(&id) {
                    Some(index) => {
                        let slot = &mut self.slots[index];
                        let manual = slot
                            .manifest
                            .as_ref()
                            .is_some_and(|m| m.activation.contains(&Activation::Manual));
                        if manual && slot.enabled {
                            slot.manual_met = true;
                        }
                    }
                    None => warn!("unknown plugin"; request = "activate"),
                }
            }
            Request::SetEnabled { id, enabled } => self.set_enabled(&id, enabled),
            Request::Command {
                name,
                done,
                requester,
            } => self.command(&name, done, requester),
            Request::Workspace { facts, from } => self.workspace(facts, from),
            Request::Sweep => {}
            Request::Reclaim { token, seq } => {
                let _scope = token.scope();
                let entry = lock(&token.0.guards).take(seq);
                drop_rejected(entry);
            }
            Request::Transition(_) | Request::Barrier(_) | Request::Shutdown(_) => {}
        }
    }

    fn transition(&self, transition: Transition) {
        match transition {
            Transition::Busy(seq) => dispatch_transition(&self.shared, Busy { seq }),
            Transition::Quiescent(seq) => dispatch_transition(&self.shared, Quiescent { seq }),
            Transition::Background(state, seq) => {
                dispatch_transition(&self.shared, BackgroundChanged { state, seq })
            }
        }
    }

    fn settle(&mut self) {
        if self.shutting_down {
            return;
        }
        self.sweep();
        self.reconcile();
        self.settle_waiters();
        self.refresh_status();
    }

    fn find(&self, id: &str) -> Option<usize> {
        self.slots.iter().position(|slot| slot.id == id)
    }

    fn start(&mut self) {
        for slot in &mut self.slots {
            // A prepared slot already has its manifest (or failure) and time.
            if slot.manifest_time.is_some() {
                continue;
            }
            let Some(plugin) = slot.plugin.clone() else {
                continue;
            };
            let (result, time) = read_manifest(plugin.as_ref(), slot.tag);
            slot.manifest_time = Some(time);
            match result {
                Ok(manifest) => slot.manifest = Some(manifest),
                Err(message) => slot.failed = Some(manifest_failure(&message)),
            }
        }

        let entries: Vec<(Name, Option<&Manifest>)> = self
            .slots
            .iter()
            .map(|slot| (slot.id.clone(), slot.manifest.as_ref()))
            .collect();
        let report = analyze(&entries);
        for (index, error) in report.blamed {
            let _scope = plugin_scope(self.slots[index].tag);
            error!(
                "plugin rejected by the dependency graph";
                error = Value::text(error.kind()),
                service = error.service(),
            );
            let slot = &mut self.slots[index];
            if slot.failed.is_none() {
                slot.failed = Some(PluginError::InvalidGraph(error.to_string()));
            }
        }
        self.order = report.order;

        {
            let mut live = lock(&self.shared.live);
            live.slots = self
                .slots
                .iter()
                .map(|slot| LiveSlot {
                    act: None,
                    provides: slot
                        .manifest
                        .as_ref()
                        .map(|m| m.provides.clone())
                        .unwrap_or_default(),
                    requires: slot
                        .manifest
                        .as_ref()
                        .map(|m| m.requires.clone())
                        .unwrap_or_default(),
                })
                .collect();
        }

        let mut commands = Vec::new();
        for slot in &self.slots {
            for activation in slot.manifest.iter().flat_map(|m| &m.activation) {
                if let Activation::Command(name) = activation
                    && !commands.contains(name)
                {
                    commands.push(name.clone());
                }
            }
        }
        *lock(&self.shared.commands) = Some(commands);
        self.shared.commands_ready.notify_all();

        self.started = true;
    }

    fn condition_met(&self, index: usize) -> bool {
        let slot = &self.slots[index];
        let Some(manifest) = &slot.manifest else {
            return false;
        };
        manifest
            .activation
            .iter()
            .any(|activation| match activation {
                Activation::AfterFirstFrame => self.started,
                Activation::Workspace(_) => slot.workspace_met,
                Activation::Command(_) => slot.command_met,
                Activation::Manual => slot.manual_met,
            })
    }

    fn is_live(&self, index: usize) -> bool {
        self.slots[index]
            .current
            .as_ref()
            .is_some_and(|current| current.token.state() == TokenState::Live)
    }

    fn provides(&self, index: usize, key: &ServiceKey) -> bool {
        self.slots[index]
            .manifest
            .as_ref()
            .is_some_and(|m| m.provides.contains(key))
    }

    /// Required services that have no live provider.
    fn missing(&self, index: usize) -> Vec<ServiceKey> {
        let Some(manifest) = &self.slots[index].manifest else {
            return Vec::new();
        };
        manifest
            .requires
            .iter()
            .filter(|key| {
                !(0..self.slots.len())
                    .any(|other| other != index && self.is_live(other) && self.provides(other, key))
            })
            .copied()
            .collect()
    }

    fn eligible(&self, index: usize) -> bool {
        let slot = &self.slots[index];
        !self.shutting_down
            && self.started
            && slot.manifest.is_some()
            && slot.enabled
            && slot.failed.is_none()
            && slot.current.is_none()
            && self.condition_met(index)
            && self.missing(index).is_empty()
    }

    /// Whether the plugin could still be activated: enabled, not failed, its
    /// condition met, and every required service has a provider that is live
    /// or could itself be activated. A command does not satisfy the
    /// conditions of the plugins it depends on.
    fn activatable(&self, index: usize, stack: &mut Vec<usize>) -> bool {
        if stack.contains(&index) {
            return false;
        }
        let slot = &self.slots[index];
        let Some(manifest) = &slot.manifest else {
            return false;
        };
        if !slot.enabled || slot.failed.is_some() {
            return false;
        }
        if self.is_live(index) {
            return true;
        }
        if !self.condition_met(index) {
            return false;
        }
        stack.push(index);
        let ok = manifest.requires.iter().all(|key| {
            (0..self.slots.len()).any(|other| {
                other != index && self.provides(other, key) && self.activatable(other, stack)
            })
        });
        stack.pop();
        ok
    }

    fn reconcile(&mut self) {
        loop {
            let mut progressed = false;
            for position in 0..self.order.len() {
                let index = self.order[position];
                if self.eligible(index) {
                    self.try_activate(index);
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
    }

    fn set_live_act(&self, index: usize, act: Option<ActivationToken>) {
        let mut live = lock(&self.shared.live);
        if let Some(slot) = live.slots.get_mut(index) {
            slot.act = act;
        }
    }

    fn clear_live_act(&self, index: usize, token: &ActivationToken) {
        let mut live = lock(&self.shared.live);
        if let Some(slot) = live.slots.get_mut(index)
            && slot.act.as_ref().is_some_and(|act| act.same(token))
        {
            slot.act = None;
        }
    }

    fn try_activate(&mut self, index: usize) {
        let Some(plugin) = self.slots[index].plugin.clone() else {
            return;
        };
        let Some(manifest) = self.slots[index].manifest.clone() else {
            return;
        };
        let id = self.slots[index].id.clone();
        self.slots[index].generation += 1;
        let token = ActivationToken::new(
            self.shared.counters.clone(),
            Arc::downgrade(&self.shared),
            id.clone(),
            self.slots[index].generation,
            self.slots[index].tag,
        );
        // Everything below runs this activation's code or drops its values.
        let _scope = token.scope();
        self.set_live_act(index, Some(token.clone()));

        // Phase 1: activate, with everything held in the context.
        let mut ctx = Context::new(&self.shared, &manifest, token.clone());
        let begin = Instant::now();
        let result = contain_plugin(Phase::Activate, || plugin.activate(&mut ctx));
        self.slots[index].activate_time = Some(begin.elapsed());
        let held = Held {
            services: mem::take(&mut ctx.services),
            listeners: mem::take(&mut ctx.listeners),
            guards: mem::take(&mut ctx.guards),
            staged: mem::take(&mut ctx.staged),
        };
        drop(ctx);

        let active = match result {
            Ok(Ok(active)) => active,
            Ok(Err(error)) => {
                held.discard(None);
                self.abort(index, &token, Some(error));
                return;
            }
            Err(message) => {
                held.discard(None);
                self.abort(
                    index,
                    &token,
                    Some(PluginError::Panicked(format!(
                        "activate panicked: {message}"
                    ))),
                );
                return;
            }
        };
        if let Some(key) = manifest
            .provides
            .iter()
            .find(|key| !held.services.iter().any(|s| s.key == **key))
        {
            let error = PluginError::MissingService(key.id());
            held.discard(Some(active));
            self.abort(index, &token, Some(error));
            return;
        }
        if token.state() == TokenState::Dead || !self.missing(index).is_empty() {
            // A provider failed while this plugin was activating: back to
            // waiting, not a failure.
            held.discard(Some(active));
            let failure = token.failure();
            self.abort(index, &token, failure);
            return;
        }

        // Commit: publish services and listeners in one step.
        let begin = Instant::now();
        let mut garbage = Garbage::default();
        {
            let mut registry = write(&self.shared.registry);
            for service in &held.services {
                registry.add_provider(
                    service.key,
                    ErasedProvider {
                        token: token.clone(),
                        obj: service.obj.clone(),
                    },
                    &mut garbage,
                );
            }
            for (type_id, listener) in &held.listeners {
                registry.add_listener(*type_id, listener.clone(), &mut garbage);
            }
        }
        drop_garbage(garbage);
        let Held {
            services,
            listeners,
            guards,
            staged,
        } = held;
        {
            let mut table = lock(&token.0.guards);
            for _ in &listeners {
                table.push(Entry::Listener);
            }
            for guard in guards {
                table.push(Entry::Guard(guard));
            }
        }
        drop_rejected(listeners);
        self.slots[index].current = Some(Current {
            token: token.clone(),
            active: Some(active),
            services: services.iter().map(|s| (s.key, s.removed)).collect(),
            announced: false,
        });
        for mut guard in staged {
            if let Err(message) = contain_plugin(Phase::Commit, || guard.commit(token.clone())) {
                invalidate(
                    &token,
                    Some(PluginError::Panicked(format!(
                        "Staged::commit panicked: {message}"
                    ))),
                );
            }
            let mut table = lock(&token.0.guards);
            table.push(Entry::Guard(Box::new(guard)));
        }
        self.slots[index].commit_time = Some(begin.elapsed());

        // Phase 2: started.
        if token.state() == TokenState::Dead {
            return;
        }
        if !self.missing(index).is_empty() {
            invalidate(&token, None);
            return;
        }
        let begin = Instant::now();
        let handle = PluginHandle { act: token.clone() };
        if let Some(active) = self.slots[index]
            .current
            .as_mut()
            .and_then(|c| c.active.as_mut())
        {
            let started_handle = handle.clone();
            if let Err(message) = contain_plugin(Phase::Started, || active.started(started_handle))
            {
                invalidate(
                    &token,
                    Some(PluginError::Panicked(format!(
                        "started panicked: {message}"
                    ))),
                );
            }
        }

        // Go live and seal the held work in one step under the staging lock.
        let (live, items) = {
            let mut staging = lock(&token.0.staging);
            let live = token.go_live();
            staging.sealed = true;
            (live, mem::take(&mut staging.items))
        };
        if !live {
            for item in items {
                drop_rejected(item);
            }
            self.slots[index].started_time = Some(begin.elapsed());
            return;
        }
        let mut items = items.into_iter();
        for item in items.by_ref() {
            if token.state() != TokenState::Live {
                drop_rejected(item);
                break;
            }
            let outcome = match item {
                StagedItem::Emit(emit) => contain_plugin(Phase::Started, emit),
                StagedItem::Start(start) => contain_plugin(Phase::Started, || start(&handle)),
            };
            if let Err(message) = outcome {
                invalidate(
                    &token,
                    Some(PluginError::Panicked(format!(
                        "held work panicked: {message}"
                    ))),
                );
            }
        }
        for item in items {
            drop_rejected(item);
        }
        if token.state() == TokenState::Live {
            if let Some(current) = self.slots[index].current.as_mut() {
                current.announced = true;
            }
            for service in &services {
                (service.added)(&self.shared, &id);
            }
        }
        drop_rejected(services);
        self.slots[index].started_time = Some(begin.elapsed());
        if token.state() == TokenState::Live {
            self.slots[index].log_activated();
        }
    }

    /// Ends an activation that never committed.
    fn abort(&mut self, index: usize, token: &ActivationToken, failure: Option<PluginError>) {
        token.mark_dead();
        self.clear_live_act(index, token);
        if let Some(error) = failure {
            token.record_failure(error.clone());
            error!(
                "plugin activation failed";
                error = Value::text(error.kind()),
                service = error.service(),
            );
            self.slots[index].failed = Some(error);
        }
    }

    /// Revokes every activation marked dead, dependents first.
    fn sweep(&mut self) {
        loop {
            let next = self.order.iter().rev().copied().find(|&index| {
                self.slots[index]
                    .current
                    .as_ref()
                    .is_some_and(|current| current.token.state() == TokenState::Dead)
            });
            match next {
                Some(index) => self.revoke(index),
                None => break,
            }
        }
    }

    /// Revokes one dead activation (`docs/design/kernel.md`, 撤销).
    fn revoke(&mut self, index: usize) {
        let Some(current) = self.slots[index].current.take() else {
            return;
        };
        let token = current.token.clone();
        let id = self.slots[index].id.clone();
        let scope = token.scope();

        // Step 2: remove the services.
        let mut garbage = Garbage::default();
        {
            let mut registry = write(&self.shared.registry);
            for (key, _) in &current.services {
                registry.remove_provider(*key, &token, &mut garbage);
            }
        }
        drop_garbage(garbage);

        // Step 3: remove the listeners and wait for admissions to drain.
        let mut garbage = Garbage::default();
        write(&self.shared.registry).remove_listeners_of(&token, &mut garbage);
        drop_garbage(garbage);
        token.wait_idle();

        // Step 4: close the guard table and drop guards in reverse order.
        let entries = {
            let mut table = lock(&token.0.guards);
            table.closed = true;
            mem::take(&mut table.entries)
        };
        let mut garbage = Garbage::default();
        write(&self.shared.registry).remove_listeners_of(&token, &mut garbage);
        drop_garbage(garbage);
        for (_, entry) in entries.into_iter().rev() {
            let outcome = match entry {
                Entry::Guard(guard) => contain_plugin(Phase::Drop, move || drop(guard)),
                Entry::Listener => Ok(()),
                Entry::Task { gate, handle } => {
                    gate.close();
                    contain_plugin(Phase::Drop, move || drop(handle))
                }
            };
            if let Err(message) = outcome {
                token.record_failure(PluginError::Panicked(format!(
                    "dropping a guard panicked: {message}"
                )));
            }
        }
        let held = {
            let mut staging = lock(&token.0.staging);
            staging.sealed = true;
            mem::take(&mut staging.items)
        };
        for item in held {
            drop_rejected(item);
        }
        if let Some(active) = current.active
            && let Err(message) = contain_plugin(Phase::Drop, move || drop(active))
        {
            token.record_failure(PluginError::Panicked(format!(
                "dropping the Active state panicked: {message}"
            )));
        }

        self.clear_live_act(index, &token);
        if self
            .facts
            .as_ref()
            .is_some_and(|(_, owner)| owner.same(&token))
        {
            let facts = self.facts.take();
            drop_rejected(facts);
        }
        if current.announced {
            for (_, removed) in &current.services {
                removed(&self.shared, &id);
            }
        }
        let failed = match token.failure() {
            Some(error) => {
                error!(
                    "plugin deactivated after a failure";
                    error = Value::text(error.kind()),
                    service = error.service(),
                );
                self.slots[index].failed = Some(error.clone());
                Some((mem::take(&mut self.slots[index].waiters), error))
            }
            None => {
                info!("plugin deactivated");
                None
            }
        };
        // The callbacks are the requesters' code, not this plugin's: each runs
        // in its requester's scope, or none for `app`'s.
        drop(scope);
        if let Some((waiters, error)) = failed {
            for waiter in waiters {
                call_done(waiter.done, waiter.requester.as_ref(), Err(error.clone()));
            }
        }
    }

    fn settle_waiters(&mut self) {
        for index in 0..self.slots.len() {
            if self.slots[index].waiters.is_empty() {
                continue;
            }
            let slot = &self.slots[index];
            let outcome = if let Some(current) = slot
                .current
                .as_ref()
                .filter(|c| c.token.state() == TokenState::Live)
            {
                Ok(current.token.clone())
            } else if !slot.enabled {
                Err(PluginError::Disabled)
            } else if let Some(error) = &slot.failed {
                Err(error.clone())
            } else if !self.activatable(index, &mut Vec::new()) {
                Err(PluginError::DependencyUnavailable)
            } else {
                continue;
            };
            for waiter in mem::take(&mut self.slots[index].waiters) {
                call_done(waiter.done, waiter.requester.as_ref(), outcome.clone());
            }
        }
    }

    fn command(&mut self, name: &str, done: Done, requester: Option<ActivationToken>) {
        if requester
            .as_ref()
            .is_some_and(|token| token.state() == TokenState::Dead)
        {
            drop_rejected(done);
            return;
        }
        let target = self.slots.iter().position(|slot| {
            slot.manifest.as_ref().is_some_and(|m| {
                m.activation
                    .iter()
                    .any(|a| matches!(a, Activation::Command(c) if c == name))
            })
        });
        let Some(index) = target else {
            call_done(
                done,
                requester.as_ref(),
                Err(PluginError::UnknownCommand(Name::Owned(name.to_owned()))),
            );
            return;
        };
        let slot = &mut self.slots[index];
        if !slot.enabled {
            call_done(done, requester.as_ref(), Err(PluginError::Disabled));
        } else if let Some(error) = &slot.failed {
            let error = error.clone();
            call_done(done, requester.as_ref(), Err(error));
        } else if let Some(current) = slot
            .current
            .as_ref()
            .filter(|c| c.token.state() == TokenState::Live)
        {
            let token = current.token.clone();
            call_done(done, requester.as_ref(), Ok(token));
        } else {
            slot.command_met = true;
            slot.waiters.push(Waiter { done, requester });
        }
    }

    fn set_enabled(&mut self, id: &str, enabled: bool) {
        let Some(index) = self.find(id) else {
            warn!("unknown plugin"; request = "set_enabled");
            return;
        };
        let slot = &mut self.slots[index];
        if enabled {
            slot.enabled = true;
            if slot.manifest.is_some() {
                slot.failed = None;
            }
            self.evaluate_workspace(Some(index));
        } else {
            slot.enabled = false;
            slot.command_met = false;
            slot.manual_met = false;
            slot.workspace_met = false;
            for waiter in mem::take(&mut slot.waiters) {
                call_done(
                    waiter.done,
                    waiter.requester.as_ref(),
                    Err(PluginError::Disabled),
                );
            }
            if let Some(current) = &slot.current {
                let token = current.token.clone();
                invalidate(&token, None);
            }
        }
    }

    fn workspace(&mut self, facts: Arc<dyn WorkspaceFacts>, from: ActivationToken) {
        if from.state() == TokenState::Dead {
            let _scope = from.scope();
            drop_rejected(facts);
            return;
        }
        if let Some((old, owner)) = self.facts.replace((facts, from)) {
            let _scope = owner.scope();
            drop_rejected(old);
        }
        self.evaluate_workspace(None);
    }

    fn evaluate_workspace(&mut self, only: Option<usize>) {
        let Some((facts, owner)) = self.facts.clone() else {
            return;
        };
        // `contains` and dropping the facts are the submitter's code.
        let _scope = owner.scope();
        for index in 0..self.slots.len() {
            if only.is_some_and(|only| only != index) {
                continue;
            }
            let slot = &self.slots[index];
            if !slot.enabled || slot.workspace_met {
                continue;
            }
            let patterns: Vec<Name> = slot
                .manifest
                .iter()
                .flat_map(|m| &m.activation)
                .filter_map(|a| match a {
                    Activation::Workspace(patterns) => Some(patterns.clone()),
                    _ => None,
                })
                .flatten()
                .collect();
            if patterns.is_empty() {
                continue;
            }
            match contain_plugin(Phase::Workspace, || {
                patterns.iter().any(|pattern| facts.contains(pattern))
            }) {
                Ok(true) => self.slots[index].workspace_met = true,
                Ok(false) => {}
                Err(message) => {
                    invalidate(
                        &owner,
                        Some(PluginError::Panicked(format!(
                            "WorkspaceFacts::contains panicked: {message}"
                        ))),
                    );
                    let dropped = self.facts.take();
                    drop_rejected(dropped);
                    break;
                }
            }
        }
        drop_rejected(facts);
    }

    fn shutdown(&mut self) {
        self.shutting_down = true;
        for slot in &mut self.slots {
            for waiter in mem::take(&mut slot.waiters) {
                call_done(
                    waiter.done,
                    waiter.requester.as_ref(),
                    Err(PluginError::ShuttingDown),
                );
            }
        }
        for position in (0..self.order.len()).rev() {
            let index = self.order[position];
            if let Some(current) = &self.slots[index].current {
                let token = current.token.clone();
                invalidate(&token, None);
            }
        }
        self.sweep();
        self.shared.counters.close_pending(Duration::from_secs(2));
        if let Some((facts, owner)) = self.facts.take() {
            let _scope = owner.scope();
            drop_rejected(facts);
        }
        for slot in &mut self.slots {
            let _scope = plugin_scope(slot.tag);
            let plugin = slot.plugin.take();
            drop_rejected(plugin);
        }
        self.refresh_status();
    }

    fn state_of(&self, index: usize) -> PluginState {
        let slot = &self.slots[index];
        if !slot.enabled {
            return PluginState::Disabled;
        }
        if let Some(error) = &slot.failed {
            return PluginState::Failed(error.clone());
        }
        if self.is_live(index) {
            return PluginState::Active;
        }
        if !self.shutting_down && self.condition_met(index) {
            let missing = self.missing(index);
            if !missing.is_empty() {
                return PluginState::Waiting { missing };
            }
        }
        PluginState::Inactive
    }

    fn refresh_status(&self) {
        let statuses = (0..self.slots.len())
            .map(|index| {
                let slot = &self.slots[index];
                PluginStatus {
                    id: slot.id.clone(),
                    state: self.state_of(index),
                    manifest_time: slot.manifest_time,
                    activate_time: slot.activate_time,
                    commit_time: slot.commit_time,
                    started_time: slot.started_time,
                }
            })
            .collect();
        *lock(&self.shared.status) = statuses;
    }
}

/// What `activate` registered, before the commit.
struct Held {
    services: Vec<crate::handle::StagedService>,
    listeners: Vec<(std::any::TypeId, crate::registry::Listener)>,
    guards: Vec<Box<dyn Send>>,
    staged: Vec<Box<dyn crate::plugin::Staged>>,
}

impl Held {
    /// Drops everything in reverse order of registration, each under
    /// `contain`, so a failed activation leaves nothing behind.
    fn discard(self, active: Option<Box<dyn Active>>) {
        let Held {
            services,
            listeners,
            guards,
            staged,
        } = self;
        for guard in staged.into_iter().rev() {
            drop_rejected(guard);
        }
        for guard in guards.into_iter().rev() {
            drop_rejected(guard);
        }
        drop_rejected(listeners);
        drop_rejected(services);
        drop_rejected(active);
    }
}
