//! Test support: a kernel with one plugin under test and mocked services,
//! and a manual clock for the keepalive slots.
//!
//! Enabled with the `test-support` feature. Every plugin crate tests itself
//! with [`TestKernel`] instead of starting the whole app.

use std::sync::{Arc, Mutex};
use std::task::Waker;
use std::time::{Duration, Instant};

use crate::clock::Clock;

use crate::event::Event;
use crate::handle::Context;
use crate::kernel::Kernel;
use crate::plugin::{Activation, Active, Manifest, Name, Plugin, PluginError, PluginState};
use crate::service::{Service, ServiceKey, service};
use crate::set::PluginSet;
use crate::util::lock;

const TARGET: &str = "plugin-under-test";

type Provide = Box<dyn Fn(&mut Context<'_>) -> Result<(), PluginError> + Send + Sync>;

struct Mock {
    key: ServiceKey,
    provide: Provide,
}

impl Plugin for Mock {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![self.key],
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        (self.provide)(ctx)?;
        Ok(Box::new(()))
    }
}

/// The plugin under test, activated only through `TestKernel::activate`.
struct Target(Box<dyn Plugin>);

impl Plugin for Target {
    fn manifest(&self) -> Manifest {
        let mut manifest = self.0.manifest();
        manifest.activation = vec![Activation::Manual];
        manifest
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        self.0.activate(ctx)
    }
}

enum State {
    Building {
        plugin: Box<dyn Plugin>,
        mocks: Vec<Mock>,
    },
    Running(Arc<Kernel>),
    Empty,
}

/// A kernel holding only the plugin under test; the services it requires are
/// injected as mocks.
pub struct TestKernel {
    state: Mutex<State>,
}

impl TestKernel {
    pub fn new(plugin: impl Plugin) -> Self {
        TestKernel {
            state: Mutex::new(State::Building {
                plugin: Box::new(plugin),
                mocks: Vec::new(),
            }),
        }
    }

    /// Injects a mock implementation of a service the plugin requires.
    pub fn mock<S: Service + ?Sized>(self, service_object: Arc<S>) -> Self {
        if let State::Building { mocks, .. } = &mut *lock(&self.state) {
            mocks.push(Mock {
                key: service::<S>(),
                provide: Box::new(move |ctx| ctx.provide::<S>(service_object.clone())),
            });
        }
        self
    }

    fn kernel(&self) -> Option<Arc<Kernel>> {
        let mut state = lock(&self.state);
        if let State::Building { .. } = &*state
            && let State::Building { plugin, mocks } = std::mem::replace(&mut *state, State::Empty)
        {
            let mut set = PluginSet::new();
            for (index, mock) in mocks.into_iter().enumerate() {
                set = set.with(Name::Owned(format!("mock-{index}-{}", mock.key.id())), mock);
            }
            set = set.with(TARGET, Target(plugin));
            *state = State::Running(Kernel::start_with_tap(
                crate::set::Plugins::Set(set),
                Some(Name::Borrowed(TARGET)),
            ));
        }
        match &*state {
            State::Running(kernel) => Some(kernel.clone()),
            _ => None,
        }
    }

    fn target_state(kernel: &Kernel) -> Option<PluginState> {
        kernel
            .status()
            .into_iter()
            .find(|status| status.id == TARGET)
            .map(|status| status.state)
    }

    /// Activates the plugin under test and waits until the kernel has settled.
    pub fn activate(&self) -> Result<(), PluginError> {
        let Some(kernel) = self.kernel() else {
            return Err(PluginError::ShuttingDown);
        };
        kernel.barrier();
        kernel.set_enabled(TARGET, true);
        kernel.activate(TARGET);
        kernel.barrier();
        match TestKernel::target_state(&kernel) {
            Some(PluginState::Active) => Ok(()),
            Some(PluginState::Failed(error)) => Err(error),
            Some(PluginState::Disabled) => Err(PluginError::Disabled),
            _ => Err(PluginError::DependencyUnavailable),
        }
    }

    /// Deactivates the plugin under test and waits until it is revoked.
    pub fn deactivate(&self) {
        if let Some(kernel) = self.kernel() {
            kernel.set_enabled(TARGET, false);
            kernel.barrier();
        }
    }

    /// Events the plugin under test emitted, of type `E`, in order.
    pub fn emitted<E: Event + Clone>(&self) -> Vec<E> {
        let Some(kernel) = self.kernel() else {
            return Vec::new();
        };
        kernel
            .tapped()
            .into_iter()
            .filter_map(|event| event.downcast_ref::<E>().cloned())
            .collect()
    }

    /// The kernel behind this test kernel.
    pub fn inner(&self) -> Option<Arc<Kernel>> {
        self.kernel()
    }
}

impl Drop for TestKernel {
    fn drop(&mut self) {
        if let State::Running(kernel) = &*lock(&self.state) {
            let (done, wait) = std::sync::mpsc::channel();
            kernel.shutdown(move || {
                let _ = done.send(());
            });
            let _ = wait.recv_timeout(std::time::Duration::from_secs(10));
        }
    }
}

/// A clock that moves only when told to, for
/// [`Kernel::set_clock`](crate::Kernel::set_clock). `advance` wakes the timers
/// it passes, in order, on the calling thread, after releasing its lock.
#[derive(Clone)]
pub struct ManualClock {
    inner: Arc<Mutex<ManualInner>>,
}

struct ManualInner {
    now: Instant,
    timers: Vec<(Instant, Waker)>,
}

impl ManualClock {
    pub fn new() -> ManualClock {
        ManualClock {
            inner: Arc::new(Mutex::new(ManualInner {
                now: Instant::now(),
                timers: Vec::new(),
            })),
        }
    }

    pub fn advance(&self, by: Duration) {
        let mut due = {
            let mut inner = lock(&self.inner);
            inner.now += by;
            let now = inner.now;
            let (due, keep) = std::mem::take(&mut inner.timers)
                .into_iter()
                .partition(|(at, _)| *at <= now);
            inner.timers = keep;
            due
        };
        due.sort_by_key(|(at, _)| *at);
        for (_, waker) in due {
            waker.wake();
        }
    }

    /// The timers not yet reached.
    pub fn pending_timers(&self) -> usize {
        lock(&self.inner).timers.len()
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        ManualClock::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        lock(&self.inner).now
    }

    fn wake_at(&self, at: Instant, waker: Waker) {
        let mut inner = lock(&self.inner);
        if at <= inner.now {
            drop(inner);
            waker.wake();
        } else {
            inner.timers.push((at, waker));
        }
    }
}
