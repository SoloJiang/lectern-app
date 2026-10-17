//! Shared helpers: a configurable plugin, an event log, test services.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{
    Activation, Active, Context, Event, Kernel, Manifest, Name, Plugin, PluginError, PluginHandle,
    PluginSet, PluginState, Service, ServiceKey, service,
};

/// An ordered log shared between a test and its plugins.
#[derive(Clone, Default)]
pub(crate) struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    pub(crate) fn push(&self, entry: impl Into<String>) {
        self.0.lock().unwrap().push(entry.into());
    }

    pub(crate) fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    pub(crate) fn snapshot(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    pub(crate) fn text(&self) -> String {
        self.snapshot().join("\n")
    }
}

/// A single-provider test service.
pub(crate) trait Greeter: Send + Sync {
    fn hello(&self) -> String;
}

impl Service for dyn Greeter {
    const ID: &'static str = "greeter";
    const MULTIPLE: bool = false;
}

pub(crate) struct Hello(pub(crate) &'static str);

impl Greeter for Hello {
    fn hello(&self) -> String {
        self.0.to_owned()
    }
}

/// A multi-provider test service.
pub(crate) trait Source: Send + Sync {
    fn name(&self) -> String;
}

impl Service for dyn Source {
    const ID: &'static str = "source";
    const MULTIPLE: bool = true;
}

pub(crate) struct Named(pub(crate) String);

impl Source for Named {
    fn name(&self) -> String {
        self.0.clone()
    }
}

/// Distinct single-provider services for dependency chains.
macro_rules! marker_service {
    ($trait:ident, $id:literal) => {
        pub(crate) trait $trait: Send + Sync {}
        impl Service for dyn $trait {
            const ID: &'static str = $id;
            const MULTIPLE: bool = false;
        }
        impl $trait for () {}
    };
}

marker_service!(SvcA, "svc-a");
marker_service!(SvcB, "svc-b");
marker_service!(SvcC, "svc-c");

/// A test event.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ping(pub(crate) u32);

impl Event for Ping {}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pong(pub(crate) u32);

impl Event for Pong {}

type ActivateFn = dyn Fn(&mut Context<'_>) -> Result<Box<dyn Active>, PluginError> + Send + Sync;

/// A plugin built from a manifest and an `activate` closure.
pub(crate) struct TestPlugin {
    manifest: Manifest,
    activate: Box<ActivateFn>,
}

impl TestPlugin {
    pub(crate) fn new(manifest: Manifest) -> Self {
        TestPlugin {
            manifest,
            activate: Box::new(|_| Ok(Box::new(()))),
        }
    }

    pub(crate) fn activate(
        mut self,
        f: impl Fn(&mut Context<'_>) -> Result<Box<dyn Active>, PluginError> + Send + Sync + 'static,
    ) -> Self {
        self.activate = Box::new(f);
        self
    }
}

impl Plugin for TestPlugin {
    fn manifest(&self) -> Manifest {
        self.manifest.clone()
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        (self.activate)(ctx)
    }
}

pub(crate) fn manifest(
    provides: Vec<ServiceKey>,
    requires: Vec<ServiceKey>,
    activation: Vec<Activation>,
) -> Manifest {
    Manifest {
        provides,
        requires,
        activation,
    }
}

pub(crate) fn after_first_frame() -> Vec<Activation> {
    vec![Activation::AfterFirstFrame]
}

/// An `Active` that runs a closure in `started` and logs its own drop.
pub(crate) struct Probe {
    pub(crate) name: &'static str,
    pub(crate) log: Log,
    pub(crate) on_started: Option<Box<dyn FnOnce(PluginHandle) + Send>>,
}

impl Probe {
    pub(crate) fn new(name: &'static str, log: &Log) -> Self {
        Probe {
            name,
            log: log.clone(),
            on_started: None,
        }
    }

    pub(crate) fn started(mut self, f: impl FnOnce(PluginHandle) + Send + 'static) -> Self {
        self.on_started = Some(Box::new(f));
        self
    }
}

impl Active for Probe {
    fn started(&mut self, handle: PluginHandle) {
        self.log.push(format!("started {}", self.name));
        if let Some(f) = self.on_started.take() {
            f(handle);
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.log.push(format!("drop {}", self.name));
    }
}

/// A guard that logs its drop.
pub(crate) struct Guard(pub(crate) String, pub(crate) Log);

impl Drop for Guard {
    fn drop(&mut self) {
        self.1.push(format!("drop guard {}", self.0));
    }
}

/// A plugin providing `provides` and requiring `requires`, logging activate,
/// started and drop under `name`.
pub(crate) fn chain_plugin(
    name: &'static str,
    log: &Log,
    provides: Vec<ServiceKey>,
    requires: Vec<ServiceKey>,
    activation: Vec<Activation>,
) -> TestPlugin {
    let log = log.clone();
    let provided = provides.clone();
    TestPlugin::new(manifest(provides, requires, activation)).activate(move |ctx| {
        log.push(format!("activate {name}"));
        for key in &provided {
            provide_marker(ctx, *key)?;
        }
        Ok(Box::new(Probe::new(name, &log)))
    })
}

/// Provides one of the marker services by key.
pub(crate) fn provide_marker(ctx: &mut Context<'_>, key: ServiceKey) -> Result<(), PluginError> {
    if key == service::<dyn SvcA>() {
        ctx.provide::<dyn SvcA>(Arc::new(()))
    } else if key == service::<dyn SvcB>() {
        ctx.provide::<dyn SvcB>(Arc::new(()))
    } else if key == service::<dyn SvcC>() {
        ctx.provide::<dyn SvcC>(Arc::new(()))
    } else if key == service::<dyn Greeter>() {
        ctx.provide::<dyn Greeter>(Arc::new(Hello("hello")))
    } else {
        Err(PluginError::new(format!("unknown marker {key}")))
    }
}

pub(crate) fn start(set: PluginSet) -> Arc<Kernel> {
    let kernel = Kernel::start(set);
    kernel.barrier();
    kernel
}

pub(crate) fn state(kernel: &Kernel, id: &str) -> PluginState {
    kernel
        .status()
        .into_iter()
        .find(|status| status.id == id)
        .map(|status| status.state)
        .unwrap_or_else(|| panic!("no plugin {id}"))
}

pub(crate) fn states(kernel: &Kernel) -> Vec<(Name, PluginState)> {
    kernel
        .status()
        .into_iter()
        .map(|s| (s.id, s.state))
        .collect()
}

/// Polls `condition` for up to five seconds.
pub(crate) fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Shuts the kernel down and waits for `done`.
pub(crate) fn shutdown(kernel: &Kernel) {
    let (done, wait) = std::sync::mpsc::channel();
    kernel.shutdown(move || {
        let _ = done.send(());
    });
    wait.recv_timeout(Duration::from_secs(10))
        .expect("shutdown finished");
}

/// Captures the `PluginHandle` an activation receives in `started`.
#[derive(Clone, Default)]
pub(crate) struct HandleSlot(Arc<Mutex<Option<PluginHandle>>>);

impl HandleSlot {
    pub(crate) fn set(&self, handle: PluginHandle) {
        *self.0.lock().unwrap() = Some(handle);
    }

    pub(crate) fn get(&self) -> PluginHandle {
        self.0.lock().unwrap().clone().expect("handle captured")
    }
}

/// A plugin with no services that stores its handle in `slot` from `started`.
pub(crate) fn handle_plugin(slot: &HandleSlot, activation: Vec<Activation>) -> TestPlugin {
    let slot = slot.clone();
    TestPlugin::new(manifest(vec![], vec![], activation)).activate(move |_| {
        let slot = slot.clone();
        Ok(Box::new(OnStarted(Some(Box::new(move |handle| {
            slot.set(handle)
        })))))
    })
}

/// An `Active` that only runs a closure in `started`.
pub(crate) struct OnStarted(pub(crate) Option<Box<dyn FnOnce(PluginHandle) + Send>>);

impl Active for OnStarted {
    fn started(&mut self, handle: PluginHandle) {
        if let Some(f) = self.0.take() {
            f(handle);
        }
    }
}
