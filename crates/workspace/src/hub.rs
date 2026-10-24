//! The `dyn Contributions` implementation and the main-thread queue's
//! sending side (docs/design/workspace.md「数据流与线程」). The GPUI-free
//! part of `WorkspaceHost`: the root view (step 2) wraps it with the window.

use std::sync::{Arc, Mutex};

use lectern_kernel::{ActivationToken, Name, PendingGuard, PluginHandle, ServiceRef};
use lectern_logging::debug;
use lectern_services::project::{ProjectEvent, Projects};
use lectern_services::workspace::{
    CommandHandler, Contribution, Contributions, LeaveGuard, Milestone, Registration, SidebarEntry,
    WorkspaceCommand,
};

use crate::nav::AskId;
use crate::registry::{Payloads, Registry, Serial};

/// An item of the main-thread queue.
pub enum Item {
    /// Apply a committed registration.
    Apply(Serial),
    /// Remove a released registration.
    Remove(Serial),
    /// `Contributions::dispatch`.
    Command(WorkspaceCommand),
    /// A leave guard answered.
    GuardAnswer { ask: AskId, allowed: bool },
    /// From the `project` plugin.
    Project(ProjectEvent),
    /// `Contributions::report_milestone`.
    Milestone(Milestone),
    /// `dyn Projects`, from `WorkspacePlugin::started`; the commands that
    /// waited for it run then.
    Projects(ServiceRef<dyn Projects>),
    /// Nothing to do: retry what was deferred (a pending token, a command
    /// waiting for `dyn Projects`). The root view sends one per frame while
    /// something is deferred.
    Tick,
}

impl std::fmt::Debug for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Item::Apply(serial) => f.debug_tuple("Apply").field(serial).finish(),
            Item::Remove(serial) => f.debug_tuple("Remove").field(serial).finish(),
            Item::Command(command) => f.debug_tuple("Command").field(command).finish(),
            Item::GuardAnswer { ask, allowed } => f
                .debug_struct("GuardAnswer")
                .field("ask", ask)
                .field("allowed", allowed)
                .finish(),
            Item::Project(event) => f.debug_tuple("Project").field(event).finish(),
            Item::Milestone(milestone) => f.debug_tuple("Milestone").field(milestone).finish(),
            Item::Projects(_) => f.write_str("Projects"),
            Item::Tick => f.write_str("Tick"),
        }
    }
}

/// A queued item with the pending guard that keeps the kernel from
/// reporting quiescence until the item is handled or dropped.
pub struct Queued {
    pub item: Item,
    pub guard: Option<PendingGuard>,
}

impl std::fmt::Debug for Queued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Queued")
            .field("item", &self.item)
            .field("guarded", &self.guard.is_some())
            .finish()
    }
}

/// The queue's sending side: wakes the main thread (step 2: a
/// `futures::channel::mpsc` sender whose receiver is a GPUI foreground task).
pub type Sender = Arc<dyn Fn(Queued) + Send + Sync>;

/// The process-wide, thread-safe part of the workspace: the registry and the
/// queue. `WorkspacePlugin` provides it as `dyn Contributions`.
pub struct Hub {
    registry: Arc<Registry>,
    sender: Sender,
    /// The workspace plugin's own activation token, from `started`: its
    /// `pending()` guards commands and milestones in the queue.
    own: Mutex<Option<ActivationToken>>,
}

impl Hub {
    pub fn new(payloads: Box<dyn Payloads>, sender: Sender) -> Arc<Hub> {
        Arc::new(Hub {
            registry: Registry::new(payloads, sender.clone()),
            sender,
            own: Mutex::new(None),
        })
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    pub fn sender(&self) -> &Sender {
        &self.sender
    }

    /// `WorkspacePlugin::started`: keeps the activation's token.
    pub(crate) fn started(&self, handle: &PluginHandle) {
        *self.own.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle.token());
    }

    /// The workspace's own pending guard, or None before `started` (nothing to
    /// guard with yet) or after `shutdown` (the queue no longer runs: the
    /// item is dropped).
    fn pending(&self) -> Result<Option<PendingGuard>, ()> {
        let own = self.own.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match own {
            None => Ok(None),
            Some(token) => token.pending().map(Some).ok_or(()),
        }
    }

    /// Queues an item with the workspace's own pending guard.
    pub fn enqueue(&self, item: Item) {
        match self.pending() {
            Ok(guard) => (self.sender)(Queued { item, guard }),
            Err(()) => {
                debug!("job dropped after shutdown"; jobs = 1u32);
            }
        }
    }
}

impl Contributions for Hub {
    fn register(&self, contribution: Contribution, token: Option<ActivationToken>) -> Registration {
        self.registry.register(contribution, token)
    }

    fn handle_command(
        &self,
        name: Name,
        handler: CommandHandler,
        token: Option<ActivationToken>,
    ) -> Registration {
        self.registry.handle_command(name, handler, token)
    }

    fn register_leave_guard(
        &self,
        guard: Arc<dyn LeaveGuard>,
        token: Option<ActivationToken>,
    ) -> Registration {
        self.registry.register_leave_guard(guard, token)
    }

    fn dispatch(&self, command: WorkspaceCommand) {
        self.enqueue(Item::Command(command));
    }

    fn sidebar_entries(&self) -> Arc<[SidebarEntry]> {
        self.registry.sidebar_entries()
    }

    fn report_milestone(&self, milestone: Milestone) {
        self.enqueue(Item::Milestone(milestone));
    }
}
