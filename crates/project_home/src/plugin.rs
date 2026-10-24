//! The two plugins (docs/design/project_home.md「本 crate 的公开项」):
//! "project-home" and "project-home-usage", both `AfterFirstFrame`. `activate`
//! takes the services, registers the contributions (`ctx.own_staged`: they
//! take effect at the commit) and subscribes; nothing is read or submitted
//! before `started`.

use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use lectern_executor::{Executor, Priority};
use lectern_kernel::{
    Activation, Active, Context, Manifest, Plugin, PluginError, PluginHandle, ServiceRef, service,
};
use lectern_services::project::{ProjectEvent, Projects};
use lectern_services::review::{Review, ReviewEvent};
use lectern_services::session::{SessionSource, SessionsChanged, Timestamp};
use lectern_services::usage::{Usage, UsageEvent};
use lectern_services::workspace::{Contributions, WorkspaceEvent};

use crate::contribute;
use crate::home::{Home, Parts, Wall};
use crate::usage_slot::UsageSlot;

/// The ID of "project-home" in the `PluginSet`.
pub const PLUGIN_ID: &str = "project-home";
/// The ID of "project-home-usage" in the `PluginSet`.
pub const USAGE_PLUGIN_ID: &str = "project-home-usage";

/// What `app` hands "project-home". Building it does no IO.
pub struct ProjectHomeDeps {
    pub executor: Executor,
    /// The cell shared with [`ProjectHomeUsagePlugin`]: `app` builds one and
    /// gives a clone to each.
    pub usage: UsageSlot,
}

/// "project-home": requires `dyn Projects`, `dyn SessionSource`, `dyn Review`
/// and `dyn Contributions`.
pub struct ProjectHomePlugin {
    deps: ProjectHomeDeps,
    /// The time of day the 14 days and the 10 minutes count from.
    wall: Wall,
    /// The state of the latest activation, for this crate's tests.
    #[cfg(test)]
    pub(crate) home: Arc<Mutex<Option<Arc<Home>>>>,
}

impl ProjectHomePlugin {
    /// Does no IO.
    pub fn new(deps: ProjectHomeDeps) -> ProjectHomePlugin {
        ProjectHomePlugin {
            deps,
            wall: Arc::new(wall_clock),
            #[cfg(test)]
            home: Arc::default(),
        }
    }

    /// Replaces the time of day (tests).
    #[cfg(test)]
    pub(crate) fn with_wall_clock(
        mut self,
        wall: impl Fn() -> Timestamp + Send + Sync + 'static,
    ) -> ProjectHomePlugin {
        self.wall = Arc::new(wall);
        self
    }
}

fn wall_clock() -> Timestamp {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp(i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

impl Plugin for ProjectHomePlugin {
    /// `dyn Contributions` takes the screen, the sidebar panel, the Overview
    /// entry and the key bindings, and the views' commands.
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![
                service::<dyn Projects>(),
                service::<dyn SessionSource>(),
                service::<dyn Review>(),
                service::<dyn Contributions>(),
            ],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// Takes the service references, builds the empty state, registers the
    /// contributions and subscribes: no IO, no task. The registrations go to
    /// `ctx.own_staged` (there is no token yet); the views are built later,
    /// by `workspace`, on the main thread.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let contributions = ctx.service::<dyn Contributions>();
        let home = Home::new(Parts {
            executor: self.deps.executor.clone(),
            projects: ctx.service::<dyn Projects>(),
            sessions: ctx.service_set::<dyn SessionSource>(),
            review: ctx.service::<dyn Review>(),
            contributions: contributions.clone(),
            usage: self.deps.usage.clone(),
            wall: self.wall.clone(),
        });
        {
            let registry = contributions
                .get()
                .ok_or(PluginError::DependencyUnavailable)?;
            for registration in contribute::register(&*registry, &home) {
                ctx.own_staged(registration);
            }
        }
        #[cfg(test)]
        {
            *self.home.lock().unwrap() = Some(home.clone());
        }
        let listener = home.clone();
        ctx.on::<ProjectEvent>(move |event, handle| listener.on_project(event, handle));
        let listener = home.clone();
        ctx.on::<SessionsChanged>(move |event, handle| listener.on_sessions(event, handle));
        let listener = home.clone();
        ctx.on::<ReviewEvent>(move |event, handle| listener.on_review(event, handle));
        let listener = home.clone();
        ctx.on::<WorkspaceEvent>(move |event, _| listener.on_workspace(event));
        Ok(Box::new(HomeActive { home }))
    }
}

struct HomeActive {
    home: Arc<Home>,
}

impl Active for HomeActive {
    /// Keeps the handle and takes up a project that is open already.
    fn started(&mut self, handle: PluginHandle) {
        self.home.attach(handle);
    }
}

impl Drop for HomeActive {
    fn drop(&mut self) {
        self.home.detach();
    }
}

/// "project-home-usage": only puts `dyn Usage` into the shared cell. When the
/// `usage` plugin fails or is off, the project home works without the Usage
/// row (the kernel has no optional dependencies).
pub struct ProjectHomeUsagePlugin {
    executor: Executor,
    slot: UsageSlot,
}

impl ProjectHomeUsagePlugin {
    /// Does no IO. `executor` runs the one reconcile task of `started`; the
    /// design's `new(slot)` has no way to submit it.
    pub fn new(executor: Executor, slot: UsageSlot) -> ProjectHomeUsagePlugin {
        ProjectHomeUsagePlugin { executor, slot }
    }
}

impl Plugin for ProjectHomeUsagePlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Usage>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// Takes the service reference and subscribes. The slot is written in
    /// `started`: `activate` has no side effects.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let usage = ctx.service::<dyn Usage>();
        let slot = self.slot.clone();
        ctx.on::<UsageEvent>(move |_, _| slot.changed());
        Ok(Box::new(UsageActive {
            executor: self.executor.clone(),
            slot: self.slot.clone(),
            usage: Some(usage),
        }))
    }
}

struct UsageActive {
    executor: Executor,
    slot: UsageSlot,
    usage: Option<ServiceRef<dyn Usage>>,
}

impl Active for UsageActive {
    /// Records the service and the token, then submits the task that reconciles:
    /// the token is `Pending` here and does not admit, and the gate opens only
    /// once it is `Live`.
    fn started(&mut self, handle: PluginHandle) {
        let Some(usage) = self.usage.take() else {
            return;
        };
        self.slot.set_usage(usage, handle.token());
        let Some(busy) = handle.busy() else { return };
        let slot = self.slot.clone();
        let executor = self.executor.clone();
        handle.own_with(move |gate| {
            executor.run_gated(&gate, Priority::Background, move || {
                let _busy = busy;
                slot.reconcile();
            })
        });
    }
}

impl Drop for UsageActive {
    /// Drops the usage interest and has "project-home" recompose once.
    fn drop(&mut self) {
        self.slot.clear_usage();
    }
}
