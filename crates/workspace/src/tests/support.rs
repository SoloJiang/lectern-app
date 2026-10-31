//! Fakes: the state machine's environment, the payload types, a recording
//! queue, a `dyn Projects` and the plugins that register through the hub.

use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lectern_kernel::testing::TestKernel;
use lectern_kernel::{
    Activation, ActivationToken, Active, Context, Manifest, Name, Plugin, PluginError,
    PluginHandle, service,
};
use lectern_services::project::{
    Location, OpenError, OpenRequest, PackageMatch, ProjectEvent, ProjectKey, ProjectSnapshot,
    Projects, ResolveError, Resolved, ScanError, WorktreeKey,
};
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, LeaveGuard, LeaveScope, Payload, Registration,
    ScreenLayout, SharedPayload, SidebarSection, Visibility, WindowId, WorkspaceCommand, screens,
};

use crate::WorkspacePlugin;
use crate::driver::Driver;
use crate::effect::Effect;
use crate::hub::{Hub, Queued, Sender};
use crate::nav::{Env, GuardId};
use crate::registry::Payloads;

pub const WINDOW: WindowId = WindowId(0);

pub fn name(s: &str) -> Name {
    Name::Owned(s.to_owned())
}

pub fn project_key(root: &str) -> ProjectKey {
    ProjectKey::Folder {
        root: Arc::from(Path::new(root)),
    }
}

pub fn worktree_key(dir: &str) -> WorktreeKey {
    WorktreeKey(Arc::from(Path::new(dir)))
}

pub fn opened(request: u64, key: &ProjectKey) -> ProjectEvent {
    ProjectEvent::Opened {
        request: OpenRequest(request),
        result: Ok(key.clone()),
    }
}

pub fn closed(key: &ProjectKey) -> ProjectEvent {
    ProjectEvent::Closed {
        project: key.clone(),
    }
}

/// The project home's layout.
pub fn home_layout() -> ScreenLayout {
    ScreenLayout {
        sidebar: Visibility::Shown,
        back_button: false,
        reading_column: true,
        inspector: false,
        floating_bar: false,
    }
}

/// The review screen's layout.
pub fn review_layout() -> ScreenLayout {
    ScreenLayout {
        sidebar: Visibility::Hidden,
        back_button: true,
        reading_column: true,
        inspector: true,
        floating_bar: true,
    }
}

pub fn welcome_layout() -> ScreenLayout {
    ScreenLayout {
        sidebar: Visibility::Hidden,
        back_button: false,
        reading_column: false,
        inspector: false,
        floating_bar: false,
    }
}

/// What `Projects` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectCall {
    Open(PathBuf),
    Close(ProjectKey),
    SetActiveWorktree(ProjectKey, WorktreeKey),
}

/// A state machine environment the tests fill by hand.
#[derive(Default)]
pub struct FakeEnv {
    pub screens: HashMap<String, ScreenLayout>,
    pub sidebar_panels: HashSet<String>,
    pub panels: HashSet<String>,
    pub overlays: HashSet<String>,
    pub guards: Vec<GuardId>,
    pub projects_available: bool,
    pub calls: Mutex<Vec<ProjectCall>>,
    next_request: Mutex<u64>,
}

impl FakeEnv {
    /// Welcome, the project home and the review screen registered; a
    /// `Projects` that accepts every open.
    pub fn standard() -> FakeEnv {
        let mut env = FakeEnv {
            projects_available: true,
            next_request: Mutex::new(1),
            ..FakeEnv::default()
        };
        env.screens
            .insert(screens::WELCOME.to_owned(), welcome_layout());
        env.screens
            .insert(screens::PROJECT_HOME.to_owned(), home_layout());
        env.screens
            .insert(screens::REVIEW.to_owned(), review_layout());
        env
    }

    pub fn calls(&self) -> Vec<ProjectCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Env for FakeEnv {
    fn screen_layout(&self, id: &str) -> Option<ScreenLayout> {
        self.screens.get(id).copied()
    }

    fn has_sidebar_panel(&self, id: &str) -> bool {
        self.sidebar_panels.contains(id)
    }

    fn has_panel(&self, id: &str) -> bool {
        self.panels.contains(id)
    }

    fn has_overlay(&self, id: &str) -> bool {
        self.overlays.contains(id)
    }

    fn guards(&self) -> Vec<GuardId> {
        self.guards.clone()
    }

    fn open_project(&self, path: &Path) -> Option<OpenRequest> {
        if !self.projects_available {
            return None;
        }
        self.calls
            .lock()
            .unwrap()
            .push(ProjectCall::Open(path.to_path_buf()));
        let mut next = self.next_request.lock().unwrap();
        let request = OpenRequest(*next);
        *next += 1;
        Some(request)
    }

    fn close_project(&self, project: &ProjectKey) {
        self.calls
            .lock()
            .unwrap()
            .push(ProjectCall::Close(project.clone()));
    }

    fn set_active_worktree(&self, project: &ProjectKey, worktree: &WorktreeKey) {
        self.calls
            .lock()
            .unwrap()
            .push(ProjectCall::SetActiveWorktree(
                project.clone(),
                worktree.clone(),
            ));
    }
}

/// The screen payload of the tests: a title, as a real one would carry a
/// toolbar closure.
#[derive(Debug, PartialEq, Eq)]
pub struct ScreenPayload(pub String);

/// The sidebar item payload of the tests.
#[derive(Debug, PartialEq, Eq)]
pub struct ItemPayload(pub String);

/// Every other kind's payload.
#[derive(Debug, PartialEq, Eq)]
pub struct Marker;

/// The tests' `ui::contrib`: screens carry [`ScreenPayload`], sidebar entries
/// [`ItemPayload`], everything else [`Marker`].
pub struct FakePayloads;

impl Payloads for FakePayloads {
    fn accepts(&self, kind: &ContributionKind, payload: &Payload) -> bool {
        let any: &(dyn Any + Send) = &*payload.0;
        match kind {
            ContributionKind::Screen(_) => any.is::<ScreenPayload>(),
            ContributionKind::SidebarEntry { .. } => any.is::<ItemPayload>(),
            _ => any.is::<Marker>(),
        }
    }

    fn share(&self, payload: Payload) -> Option<SharedPayload> {
        let item = payload.0.downcast::<ItemPayload>().ok()?;
        let shared: Arc<dyn Any + Send + Sync> = Arc::new(*item);
        Some(SharedPayload(shared))
    }
}

pub fn screen(id: &str, layout: ScreenLayout, title: &str) -> Contribution {
    Contribution {
        kind: ContributionKind::Screen(layout),
        id: name(id),
        order: 0,
        payload: Payload(Box::new(ScreenPayload(title.to_owned()))),
    }
}

pub fn item(id: &str, section: SidebarSection, order: u16, label: &str) -> Contribution {
    Contribution {
        kind: ContributionKind::SidebarEntry { section },
        id: name(id),
        order,
        payload: Payload(Box::new(ItemPayload(label.to_owned()))),
    }
}

pub fn plain(kind: ContributionKind, id: &str) -> Contribution {
    Contribution {
        kind,
        id: name(id),
        order: 0,
        payload: Payload(Box::new(Marker)),
    }
}

/// The queue's receiving end: items wait here until a test hands them to
/// the driver.
#[derive(Default)]
pub struct Queue(Mutex<VecDeque<Queued>>);

impl Queue {
    pub fn new() -> Arc<Queue> {
        Arc::new(Queue::default())
    }

    pub fn sender(self: &Arc<Self>) -> Sender {
        let queue = self.clone();
        Arc::new(move |queued| queue.0.lock().unwrap().push_back(queued))
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    pub fn pop(&self) -> Option<Queued> {
        self.0.lock().unwrap().pop_front()
    }

    /// The items as text, without taking them out.
    pub fn items(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|queued| format!("{:?}", queued.item))
            .collect()
    }

    /// Hands every queued item to the driver, including the ones the
    /// handling queues, and returns the effects.
    pub fn drain(&self, driver: &mut Driver) -> Vec<Effect> {
        let mut out = Vec::new();
        while let Some(queued) = self.pop() {
            driver.handle(queued, &mut out);
        }
        out
    }
}

/// A `dyn Projects` that records its calls and gives each open a new
/// request number.
#[derive(Default)]
pub struct FakeProjects {
    pub calls: Mutex<Vec<ProjectCall>>,
    /// The open projects' snapshots, in the order they were opened.
    pub open: Mutex<Vec<Arc<ProjectSnapshot>>>,
}

impl FakeProjects {
    pub fn calls(&self) -> Vec<ProjectCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Projects for FakeProjects {
    fn open(&self, path: &Path) -> OpenRequest {
        let mut calls = self.calls.lock().unwrap();
        calls.push(ProjectCall::Open(path.to_path_buf()));
        OpenRequest(calls.len() as u64)
    }

    fn close(&self, project: &ProjectKey) {
        self.calls
            .lock()
            .unwrap()
            .push(ProjectCall::Close(project.clone()));
    }

    fn set_active_worktree(&self, project: &ProjectKey, worktree: &WorktreeKey) {
        self.calls
            .lock()
            .unwrap()
            .push(ProjectCall::SetActiveWorktree(
                project.clone(),
                worktree.clone(),
            ));
    }

    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        let open = self.open.lock().unwrap();
        open.iter().find(|s| s.key == *project).cloned()
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        let open = self.open.lock().unwrap();
        open.iter().map(|s| s.key.clone()).collect()
    }

    fn locate(&self, _: &Path) -> Option<Location> {
        None
    }

    fn resolve(&self, _: &Path) -> Result<Resolved, ResolveError> {
        Err(OpenError::NotFound)
    }

    fn resolve_package(
        &self,
        _: &ProjectKey,
        _: &WorktreeKey,
        _: &str,
    ) -> Result<PackageMatch, ScanError> {
        Err(ScanError::RootMissing)
    }
}

/// A leave guard that records the scopes it was asked for and answers at
/// once with a fixed verdict.
pub struct FixedGuard {
    pub allow: bool,
    pub asked: Mutex<Vec<LeaveScope>>,
}

impl FixedGuard {
    pub fn new(allow: bool) -> Arc<FixedGuard> {
        Arc::new(FixedGuard {
            allow,
            asked: Mutex::new(Vec::new()),
        })
    }

    pub fn asked(&self) -> Vec<LeaveScope> {
        self.asked.lock().unwrap().clone()
    }
}

impl LeaveGuard for FixedGuard {
    fn ask(&self, scope: LeaveScope, done: Box<dyn FnOnce(bool) + Send>) {
        self.asked.lock().unwrap().push(scope);
        done(self.allow);
    }
}

/// One registration through `dyn Contributions`, with the token the
/// [`Contributor`] has at that point: `None` in `activate`, the handle's
/// token in `started`.
pub type Register =
    Box<dyn Fn(&dyn Contributions, Option<ActivationToken>) -> Registration + Send + Sync>;
/// Called in `started` with the activation's handle.
pub type OnStarted = Arc<dyn Fn(&PluginHandle) + Send + Sync>;
pub type Registrations = Arc<Vec<Register>>;

/// A plugin that registers through `dyn Contributions`, the way `welcome`
/// or `project_home` do: `staged` in `activate` (no token, through
/// `own_staged`), `late` in `started` (with `handle.token()`, kept in the
/// `Active`).
pub struct Contributor {
    pub staged: Registrations,
    pub late: Registrations,
    /// Called in `started`, before the late registrations.
    pub on_started: Option<OnStarted>,
}

impl Contributor {
    pub fn staged(registrations: Vec<Register>) -> Contributor {
        Contributor {
            staged: Arc::new(registrations),
            late: Arc::new(Vec::new()),
            on_started: None,
        }
    }
}

impl Plugin for Contributor {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Contributions>()],
            activation: vec![Activation::Manual],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let contributions = ctx.service::<dyn Contributions>();
        let service = contributions.get().expect("the hub is live");
        for register in self.staged.iter() {
            ctx.own_staged(register(&*service, None));
        }
        drop(service);
        Ok(Box::new(ContributorActive {
            contributions,
            late: self.late.clone(),
            on_started: self.on_started.clone(),
            guards: Vec::new(),
        }))
    }
}

struct ContributorActive {
    contributions: lectern_kernel::ServiceRef<dyn Contributions>,
    late: Registrations,
    on_started: Option<OnStarted>,
    guards: Vec<Registration>,
}

impl Active for ContributorActive {
    fn started(&mut self, handle: PluginHandle) {
        if let Some(on_started) = &self.on_started {
            on_started(&handle);
        }
        let Some(service) = self.contributions.get() else {
            return;
        };
        for register in self.late.iter() {
            self.guards.push(register(&*service, Some(handle.token())));
        }
    }
}

/// A hub with its queue, a driver on the window and the kernel holding the
/// workspace plugin with a fake `Projects`.
pub struct Rig {
    pub queue: Arc<Queue>,
    pub hub: Arc<Hub>,
    pub driver: Driver,
    pub projects: Arc<FakeProjects>,
    pub kernel: TestKernel,
}

impl Rig {
    /// Everything built, the workspace plugin not activated yet.
    pub fn prepared() -> Rig {
        let queue = Queue::new();
        let hub = Hub::new(Box::new(FakePayloads), queue.sender());
        let driver = Driver::new(WINDOW, hub.registry().clone(), queue.sender());
        let projects = Arc::new(FakeProjects::default());
        let projects_service: Arc<dyn Projects> = projects.clone();
        let kernel = TestKernel::new(WorkspacePlugin::from_hub(hub.clone()))
            .mock::<dyn Projects>(projects_service);
        Rig {
            queue,
            hub,
            driver,
            projects,
            kernel,
        }
    }

    /// The workspace plugin activated and the queue drained: the driver has
    /// `dyn Projects`.
    pub fn new() -> Rig {
        let mut rig = Rig::prepared();
        rig.kernel
            .activate()
            .expect("the workspace plugin activates");
        rig.drain();
        rig
    }

    pub fn drain(&mut self) -> Vec<Effect> {
        self.queue.drain(&mut self.driver)
    }

    pub fn dispatch(&mut self, command: WorkspaceCommand) -> Vec<Effect> {
        self.hub.dispatch(command);
        self.drain()
    }

    /// Emits a `ProjectEvent` the way the `project` plugin would.
    pub fn emit(&mut self, event: ProjectEvent) -> Vec<Effect> {
        self.kernel
            .inner()
            .expect("the kernel runs")
            .emitter()
            .emit(event);
        self.kernel.inner().expect("the kernel runs").barrier();
        self.drain()
    }

    /// Opens a project: dispatches `OpenProject`, then answers with `Opened`.
    pub fn open(&mut self, path: &str, key: &ProjectKey) -> Vec<Effect> {
        let mut effects = self.dispatch(WorkspaceCommand::OpenProject(PathBuf::from(path)));
        let request = self.projects.calls().len() as u64;
        effects.extend(self.emit(opened(request, key)));
        effects
    }
}

/// Registers a contributor plugin in its own test kernel against `hub`.
pub fn contributor(hub: &Arc<Hub>, plugin: Contributor) -> TestKernel {
    let contributions: Arc<dyn Contributions> = hub.clone();
    TestKernel::new(plugin).mock::<dyn Contributions>(contributions)
}

/// Whether `effects` holds a `ScreenChanged` to `screen`.
pub fn changed_to(effects: &[Effect], screen: &str) -> bool {
    effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::Event(lectern_services::workspace::WorkspaceEvent::ScreenChanged { to, .. })
                if to.screen == screen
        )
    })
}
