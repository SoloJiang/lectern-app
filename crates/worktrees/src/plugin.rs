//! The two plugins (docs/design/worktrees.md「本 crate 的公开项」): "worktrees"
//! and "worktrees-usage", both `AfterFirstFrame`. `activate` takes the
//! services and subscribes; nothing is read or submitted before `started`.
//! The screen, the inspector page and the key bindings are registered here
//! (`ctx.own_staged`: they take effect at the activation's commit).

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
use crate::hub::{Hub, Parts, Wall};
use crate::usage_link::UsageLink;

/// The ID of "worktrees" in the `PluginSet`.
pub const PLUGIN_ID: &str = "worktrees";
/// The ID of "worktrees-usage" in the `PluginSet`.
pub const USAGE_PLUGIN_ID: &str = "worktrees-usage";

/// What `app` hands "worktrees". Building it does no IO.
pub struct WorktreesDeps {
    pub executor: Executor,
    /// The cell shared with [`WorktreesUsagePlugin`]: `app` builds one and
    /// gives a clone to each.
    pub usage: UsageLink,
}

/// "worktrees": requires `dyn Projects`, `dyn SessionSource`, `dyn Review` and
/// `dyn Contributions`.
pub struct WorktreesPlugin {
    deps: WorktreesDeps,
    /// The time of day the 14 days and the 10 minutes count from.
    wall: Wall,
    /// The state of the latest activation, for this crate's tests.
    #[cfg(test)]
    pub(crate) hub: Arc<Mutex<Option<Arc<Hub>>>>,
}

impl WorktreesPlugin {
    /// Does no IO.
    pub fn new(deps: WorktreesDeps) -> WorktreesPlugin {
        WorktreesPlugin {
            deps,
            wall: Arc::new(wall_clock),
            #[cfg(test)]
            hub: Arc::default(),
        }
    }

    /// Replaces the time of day (tests).
    #[cfg(test)]
    pub(crate) fn with_wall_clock(
        mut self,
        wall: impl Fn() -> Timestamp + Send + Sync + 'static,
    ) -> WorktreesPlugin {
        self.wall = Arc::new(wall);
        self
    }
}

/// The time of day in the unit of the sessions' timestamps (UTC milliseconds).
/// The kernel's `Clock` is monotonic and has no date.
fn wall_clock() -> Timestamp {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp(i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

impl Plugin for WorktreesPlugin {
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
    /// screen, the inspector page and the key bindings, and subscribes: no IO,
    /// no task. The registrations go to `ctx.own_staged` (there is no token
    /// yet); the views are built later, by `workspace`, on the main thread.
    /// The sidebar entry exists only while a git project is open (`Hub`).
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let contributions = ctx.service::<dyn Contributions>();
        let hub = Hub::new(Parts {
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
            for registration in contribute::register(&*registry, &hub) {
                ctx.own_staged(registration);
            }
        }
        #[cfg(test)]
        {
            *self.hub.lock().unwrap() = Some(hub.clone());
        }
        let listener = hub.clone();
        ctx.on::<ProjectEvent>(move |event, handle| listener.on_project(event, handle));
        let listener = hub.clone();
        ctx.on::<SessionsChanged>(move |event, handle| listener.on_sessions(event, handle));
        let listener = hub.clone();
        ctx.on::<ReviewEvent>(move |event, handle| listener.on_review(event, handle));
        let listener = hub.clone();
        ctx.on::<WorkspaceEvent>(move |event, handle| listener.on_workspace(event, handle));
        Ok(Box::new(HubActive { hub }))
    }
}

struct HubActive {
    hub: Arc<Hub>,
}

impl Active for HubActive {
    /// Keeps the handle and takes up a project that is open already.
    fn started(&mut self, handle: PluginHandle) {
        self.hub.attach(handle);
    }
}

impl Drop for HubActive {
    fn drop(&mut self) {
        self.hub.detach();
    }
}

/// "worktrees-usage": only puts `dyn Usage` into the shared cell. When the
/// `usage` plugin fails or is off, the screen works without the tokens (the
/// kernel has no optional dependencies).
pub struct WorktreesUsagePlugin {
    executor: Executor,
    link: UsageLink,
}

impl WorktreesUsagePlugin {
    /// Does no IO. `executor` runs the one reconcile task of `started`.
    pub fn new(executor: Executor, link: UsageLink) -> WorktreesUsagePlugin {
        WorktreesUsagePlugin { executor, link }
    }
}

impl Plugin for WorktreesUsagePlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Usage>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// Takes the service reference and subscribes. The link is written in
    /// `started`: `activate` has no side effects.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let usage = ctx.service::<dyn Usage>();
        let link = self.link.clone();
        ctx.on::<UsageEvent>(move |event, _| {
            if let UsageEvent::Changed { query, .. } = event {
                link.changed(query);
            }
        });
        Ok(Box::new(UsageActive {
            executor: self.executor.clone(),
            link: self.link.clone(),
            usage: Some(usage),
        }))
    }
}

struct UsageActive {
    executor: Executor,
    link: UsageLink,
    usage: Option<ServiceRef<dyn Usage>>,
}

impl Active for UsageActive {
    /// Records the service and the token, then submits the task that
    /// reconciles: the token is `Pending` here and does not admit, and the
    /// gate opens only once it is `Live`.
    fn started(&mut self, handle: PluginHandle) {
        let Some(usage) = self.usage.take() else {
            return;
        };
        self.link.set_usage(usage, handle.token());
        let Some(busy) = handle.busy() else { return };
        let link = self.link.clone();
        let executor = self.executor.clone();
        handle.own_with(move |gate| {
            executor.run_gated(&gate, Priority::Background, move || {
                let _busy = busy;
                link.reconcile();
            })
        });
    }
}

impl Drop for UsageActive {
    /// Drops the usage interests and has "worktrees" compose once more.
    fn drop(&mut self) {
        self.link.clear_usage();
    }
}
