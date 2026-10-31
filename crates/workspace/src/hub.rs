//! The `dyn Contributions` implementation and the main-thread queue's
//! sending side (docs/design/workspace.md「数据流与线程」). The GPUI-free
//! part of `WorkspaceHost`: the root view wraps it with the window.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lectern_kernel::{
    ActivationToken, Emitter, KernelControl, Name, PendingGuard, PluginHandle, ServiceRef,
};
use lectern_logging::debug;
use lectern_services::project::{ProjectEvent, ProjectSnapshot, Projects};
use lectern_services::workspace::{
    CommandHandler, Contribution, Contributions, LeaveGuard, LeaveScope, Milestone, Registration,
    Route, SidebarEntry, WindowId, WorkspaceCommand, WorkspaceEvent,
};

use crate::nav::{AskId, Report};
use crate::registry::{Payloads, Registry, Serial};
use crate::state::WindowState;

/// When a frame recording ends (`WorkspaceHost::record_frames`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordStop {
    /// After a fixed time.
    After(Duration),
    /// Once every transition in progress has ended
    /// (`lectern_ui::motion::active_transitions`).
    AnimationsSettled,
    /// When the caller calls `stop_recording`.
    Manual,
}

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
    /// `WorkspacePlugin::started`: the kernel control for the proxied
    /// commands and the workspace's own token.
    Started {
        control: KernelControl,
        token: ActivationToken,
    },
    /// A proxied command's action fired (`workspace::Command { name }`).
    Proxy(Name),
    /// `KernelControl::command` activated the plugin declaring `name`:
    /// replay once its registrations have applied.
    Replay { name: Name, token: ActivationToken },
    /// `KernelControl::command` failed; `error` is the error's kind.
    CommandFailed { name: Name, error: &'static str },
    /// `WorkspaceHost::ask_leave`.
    AskLeave { scope: LeaveScope, done: Report },
    /// `WorkspaceHost::restore`.
    Restore(WindowState),
    /// `WorkspaceHost::record_frames`; the root view handles it.
    RecordFrames { scenario: Name, stop: RecordStop },
    /// `WorkspaceHost::stop_recording`; the root view handles it.
    StopRecording,
    /// Nothing to do: retry what was deferred (a pending token, a command
    /// waiting for `dyn Projects`, a replay waiting for its registrations).
    /// The root view sends one per frame while something is deferred.
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
            Item::Started { .. } => f.write_str("Started"),
            Item::Proxy(name) => f.debug_tuple("Proxy").field(name).finish(),
            Item::Replay { name, .. } => f.debug_tuple("Replay").field(name).finish(),
            Item::CommandFailed { name, error } => f
                .debug_struct("CommandFailed")
                .field("name", name)
                .field("error", error)
                .finish(),
            Item::AskLeave { scope, .. } => f.debug_tuple("AskLeave").field(scope).finish(),
            Item::Restore(state) => f.debug_tuple("Restore").field(state).finish(),
            Item::RecordFrames { scenario, stop } => f
                .debug_struct("RecordFrames")
                .field("scenario", scenario)
                .field("stop", stop)
                .finish(),
            Item::StopRecording => f.write_str("StopRecording"),
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

/// The queue's sending side: wakes the main thread (`install` makes it a
/// `futures::channel::mpsc` sender whose receiver is a GPUI foreground task).
pub type Sender = Arc<dyn Fn(Queued) + Send + Sync>;

/// What `WorkspacePlugin::started` leaves with the hub.
#[derive(Default)]
struct Plugin {
    /// The workspace plugin's own activation token: its `pending()` guards
    /// commands and milestones in the queue.
    token: Option<ActivationToken>,
    /// Emits `WorkspaceEvent`s.
    emitter: Option<Emitter>,
    /// `dyn Projects`, kept for `open_snapshots`.
    projects: Option<ServiceRef<dyn Projects>>,
}

/// The process-wide, thread-safe part of the workspace: the registry and the
/// queue. `WorkspacePlugin` provides it as `dyn Contributions`.
pub struct Hub {
    registry: Arc<Registry>,
    sender: Sender,
    plugin: Mutex<Plugin>,
    /// The current screen of each window, as of the last `ScreenChanged`
    /// that went out (`Contributions::current_route`).
    routes: Mutex<BTreeMap<WindowId, Route>>,
}

impl Hub {
    pub fn new(payloads: Box<dyn Payloads>, sender: Sender) -> Arc<Hub> {
        Arc::new(Hub {
            registry: Registry::new(payloads, sender.clone()),
            sender,
            plugin: Mutex::new(Plugin::default()),
            routes: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    pub fn sender(&self) -> &Sender {
        &self.sender
    }

    /// `WorkspacePlugin::started`: keeps the activation's token and emitter
    /// and sends the kernel control to the main thread.
    pub(crate) fn started(&self, handle: &PluginHandle) {
        {
            let mut plugin = self.plugin.lock().unwrap_or_else(|e| e.into_inner());
            plugin.token = Some(handle.token());
            plugin.emitter = Some(handle.emitter());
        }
        self.enqueue(Item::Started {
            control: handle.control(),
            token: handle.token(),
        });
    }

    /// Keeps `dyn Projects` for `open_snapshots`; `WorkspacePlugin::started`.
    pub(crate) fn keep_projects(&self, projects: ServiceRef<dyn Projects>) {
        self.plugin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .projects = Some(projects);
    }

    /// The snapshots of the open projects, in the order they were opened.
    /// Empty before `started` and once `dyn Projects` is gone. Only copies
    /// `Arc`s under locks (`Projects::snapshot`): no IO, fine on the main
    /// thread. For `app`'s measurement build, which finds a worktree by its
    /// root (docs/design/worktrees.md「对其他 crate 的要求」D).
    pub fn open_snapshots(&self) -> Vec<Arc<ProjectSnapshot>> {
        let projects = self
            .plugin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .projects
            .clone();
        let Some(projects) = projects.as_ref().and_then(ServiceRef::get) else {
            return Vec::new();
        };
        projects
            .open_projects()
            .iter()
            .filter_map(|key| projects.snapshot(key))
            .collect()
    }

    /// The workspace plugin's own token, from `started`.
    pub fn token(&self) -> Option<ActivationToken> {
        self.plugin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .token
            .clone()
    }

    /// Records the current screen of `window` for `current_route`.
    pub(crate) fn note_route(&self, window: WindowId, route: Route) {
        self.routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(window, route);
    }

    /// Emits a `WorkspaceEvent` through the plugin's emitter; nothing before
    /// `started` (no listener can exist yet). `ScreenChanged` and
    /// `WindowClosed` also update the routes `current_route` reads, before
    /// the event goes out, so a plugin that reads `current_route` in its
    /// `started` never sees an older screen than the events it receives.
    pub fn emit(&self, event: WorkspaceEvent) {
        match &event {
            WorkspaceEvent::ScreenChanged { window, to, .. } => {
                self.note_route(*window, to.clone());
            }
            WorkspaceEvent::WindowClosed { window } => {
                self.routes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(window);
            }
            _ => {}
        }
        let emitter = self
            .plugin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .emitter
            .clone();
        if let Some(emitter) = emitter {
            emitter.emit(event);
        }
    }

    /// The workspace's own pending guard, or None before `started` (nothing to
    /// guard with yet) or after `shutdown` (the queue no longer runs: the
    /// item is dropped).
    fn pending(&self) -> Result<Option<PendingGuard>, ()> {
        match self.token() {
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

    fn current_route(&self, window: WindowId) -> Option<Route> {
        self.routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&window)
            .cloned()
    }

    fn report_milestone(&self, milestone: Milestone) {
        self.enqueue(Item::Milestone(milestone));
    }
}
