//! The "usage" plugin (docs/design/usage.md「依赖」): provides `dyn Usage`,
//! requires `dyn Projects` and `dyn SessionSource`, activates after the
//! first frame.

use std::path::PathBuf;
use std::sync::Arc;

use lectern_executor::Executor;
use lectern_kernel::{
    Activation, Active, Busy, Context, Manifest, Name, Plugin, PluginError, PluginHandle,
    Quiescent, service,
};
use lectern_services::project::{ProjectEvent, Projects};
use lectern_services::session::{SessionSource, SessionsChanged};
use lectern_services::usage::Usage;

use crate::LocalZone;
use crate::engine::{Deps, Engine};
use crate::time::Time;

/// The plugin ID.
pub const PLUGIN_ID: &str = "usage";

/// What `app` hands the plugin. Building it does no IO.
pub struct UsageDeps {
    pub executor: Executor,
    /// `app` passes `platform::dirs::app_data()` joined with `usage/`
    /// (ADR 0044). The database is `usage.db` in it; the directory is
    /// created by the first open, after the first frame.
    pub data_dir: PathBuf,
    /// The local time zone; `app` wraps `platform::time::utc_offset`.
    pub zone: Arc<dyn LocalZone>,
}

/// The plugin and its ID, for `PluginSet::with`.
pub fn plugin(deps: UsageDeps) -> (Name, UsagePlugin) {
    (
        Name::Borrowed(PLUGIN_ID),
        UsagePlugin {
            deps: Arc::new(deps),
            time: Time::system(),
            #[cfg(test)]
            engine: Arc::default(),
        },
    )
}

pub struct UsagePlugin {
    deps: Arc<UsageDeps>,
    time: Time,
    /// The engine of the latest activation, for this crate's tests.
    #[cfg(test)]
    pub(crate) engine: Arc<std::sync::Mutex<Option<Arc<Engine>>>>,
}

#[cfg(any(test, feature = "test-support"))]
impl UsagePlugin {
    /// Replaces the clocks (tests and bench).
    pub fn with_time(mut self, time: &crate::testing::ManualTime) -> UsagePlugin {
        self.time = time.time();
        self
    }
}

impl Plugin for UsagePlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn Usage>()],
            requires: vec![service::<dyn Projects>(), service::<dyn SessionSource>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// Builds the empty in-memory state, provides the service and
    /// subscribes; no IO, no task.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let engine = Engine::new(
            Deps {
                executor: self.deps.executor.clone(),
                data_dir: self.deps.data_dir.clone(),
                zone: self.deps.zone.clone(),
                time: self.time.clone(),
            },
            ctx.service_set::<dyn SessionSource>(),
            ctx.service::<dyn Projects>(),
        );
        #[cfg(test)]
        {
            *self.engine.lock().unwrap() = Some(engine.clone());
        }
        ctx.provide::<dyn Usage>(engine.clone())?;
        let e = engine.clone();
        ctx.on::<SessionsChanged>(move |event, _| e.on_sessions(event));
        let e = engine.clone();
        ctx.on::<ProjectEvent>(move |event, _| e.on_project(event));
        let e = engine.clone();
        ctx.on::<Quiescent>(move |event, _| e.on_quiescent(event.seq));
        let e = engine.clone();
        ctx.on::<Busy>(move |event, _| e.on_busy(event.seq));
        Ok(Box::new(UsageActive { engine }))
    }
}

struct UsageActive {
    engine: Arc<Engine>,
}

impl Active for UsageActive {
    /// Submits the task that opens the database.
    fn started(&mut self, handle: PluginHandle) {
        self.engine.start(handle);
    }
}

impl Drop for UsageActive {
    fn drop(&mut self) {
        self.engine.stop();
    }
}
