//! The "workspace" plugin (docs/design/workspace.md「本 crate 的公开项」):
//! provides `dyn Contributions`, requires `dyn Projects`, activates after
//! the first frame. It does no IO and submits no task.

use std::sync::Arc;

use lectern_kernel::{
    Activation, Active, Context, Manifest, Plugin, PluginError, PluginHandle, ServiceRef, service,
};
use lectern_logging::debug;
use lectern_services::project::{ProjectEvent, Projects};
use lectern_services::workspace::Contributions;

use crate::host::WorkspaceHost;
use crate::hub::{Hub, Item, Queued};

/// Built by `app` and put in the `PluginSet` as "workspace".
pub struct WorkspacePlugin {
    hub: Arc<Hub>,
}

impl WorkspacePlugin {
    /// Does no IO. `host` is what `install` returned.
    pub fn new(host: WorkspaceHost) -> WorkspacePlugin {
        WorkspacePlugin {
            hub: host.hub().clone(),
        }
    }

    /// The GPUI-free tests: a plugin over a hub without a window.
    #[cfg(test)]
    pub(crate) fn from_hub(hub: Arc<Hub>) -> WorkspacePlugin {
        WorkspacePlugin { hub }
    }
}

impl Plugin for WorkspacePlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn Contributions>()],
            requires: vec![service::<dyn Projects>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// Hands out the hub (the registry exists already) and subscribes to
    /// `ProjectEvent`: the events only go into the queue.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let contributions: Arc<dyn Contributions> = self.hub.clone();
        ctx.provide::<dyn Contributions>(contributions)?;
        let projects = ctx.service::<dyn Projects>();
        let sender = self.hub.sender().clone();
        ctx.on::<ProjectEvent>(move |event, handle| {
            if matches!(event, ProjectEvent::Changed { .. }) {
                return;
            }
            match handle.token().pending() {
                Some(guard) => sender(Queued {
                    item: Item::Project(event.clone()),
                    guard: Some(guard),
                }),
                None => debug!("job dropped after shutdown"; jobs = 1u32),
            }
        });
        Ok(Box::new(WorkspaceActive {
            hub: self.hub.clone(),
            projects: Some(projects),
        }))
    }
}

struct WorkspaceActive {
    hub: Arc<Hub>,
    projects: Option<ServiceRef<dyn Projects>>,
}

impl Active for WorkspaceActive {
    /// Keeps the token and sends `dyn Projects` to the main thread; the
    /// commands that waited for it run then.
    fn started(&mut self, handle: PluginHandle) {
        self.hub.started(&handle);
        if let Some(projects) = self.projects.take() {
            self.hub.enqueue(Item::Projects(projects));
        }
    }
}
