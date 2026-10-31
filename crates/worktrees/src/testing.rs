//! Test doubles and fixtures (docs/design/worktrees.md「测试」).
//!
//! The services are fakes of the `services` traits that hand out what a test
//! constructs and record what the plugin does. `read`, `turns`, `load`,
//! `resolve` and the other calls this crate must not make are `unreachable!`.
//! The other plugins' crates are not dependencies, not even as
//! dev-dependencies (architecture rule 7), so nothing here comes from their
//! `test-support`. Every name, path and title is synthetic.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lectern_executor::Executor;
use lectern_kernel::ActivationToken;
use lectern_kernel::{
    Activation, Active, Context, Kernel, Manifest, Name, Plugin, PluginError, PluginSet, Service,
    service,
};
use lectern_services::project::{
    ChangedParts, Location, OpenRequest, PackageMatch, ProjectEvent, ProjectKey, ProjectSnapshot,
    Projects, ResolveError, Resolved, WorktreeKey,
};
use lectern_services::review::{
    BriefRules, BriefRulesRegistration, FileRows, NoteRoute, NoteRouteRegistration, Overview,
    OverviewInterest, Review, ReviewCommand, ReviewEvent, ReviewId, ReviewSnapshot, ReviewTarget,
    RowsKey, TextKey, WorktreesOverview,
};
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, ResumeCommand, SessionError, SessionKey,
    SessionList, SessionSource, SessionSummary, SessionsChanged, SourceId, Timestamp, TurnIndex,
};
use lectern_services::usage::{
    AnalysisId, AnalysisStart, Limits, MeterEvent, MrSource, MrSourceRegistration, Usage,
    UsageCommand, UsageError, UsageEvent, UsageInterest, UsageQuery, UsageSnapshot,
};
use lectern_services::workspace::{
    CommandHandler, Contribution, ContributionKind, Contributions, LeaveGuard, Milestone, Payload,
    Registration, Route, SidebarEntry, Target, WindowId, WorkspaceCommand, WorkspaceEvent, screens,
};
use lectern_ui::contrib::SidebarItem;

use crate::hub::Hub;
use crate::plugin::{
    PLUGIN_ID, USAGE_PLUGIN_ID, WorktreesDeps, WorktreesPlugin, WorktreesUsagePlugin,
};
use crate::snapshot::WorktreesSnapshot;
use crate::usage_link::UsageLink;

pub(crate) use crate::fixture::*;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------------- fakes

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
pub(crate) struct FakeProjects {
    snapshots: Mutex<HashMap<ProjectKey, Arc<ProjectSnapshot>>>,
    open: Mutex<Vec<ProjectKey>>,
    calls: AtomicUsize,
    on_snapshot: Mutex<Option<Hook>>,
}

impl FakeProjects {
    /// The project is open and has this snapshot.
    pub fn set(&self, snapshot: ProjectSnapshot) {
        let key = snapshot.key.clone();
        lock(&self.snapshots).insert(key.clone(), Arc::new(snapshot));
        let mut open = lock(&self.open);
        if !open.contains(&key) {
            open.push(key);
        }
    }

    pub fn remove(&self, key: &ProjectKey) {
        lock(&self.snapshots).remove(key);
        lock(&self.open).retain(|k| k != key);
    }

    /// `snapshot` calls so far.
    pub fn snapshot_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Runs `hook` inside the next `snapshot` call, once: a composition that
    /// is running when events arrive.
    pub fn on_next_snapshot(&self, hook: impl FnOnce() + Send + 'static) {
        *lock(&self.on_snapshot) = Some(Box::new(hook));
    }
}

impl Projects for FakeProjects {
    fn open(&self, _: &Path) -> OpenRequest {
        unreachable!("worktrees never opens a project")
    }
    fn close(&self, _: &ProjectKey) {
        unreachable!("worktrees never closes a project")
    }
    fn set_active_worktree(&self, _: &ProjectKey, _: &WorktreeKey) {
        unreachable!("worktrees switches through the window's command")
    }
    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let hook = lock(&self.on_snapshot).take();
        if let Some(hook) = hook {
            hook();
        }
        lock(&self.snapshots).get(project).cloned()
    }
    fn open_projects(&self) -> Vec<ProjectKey> {
        lock(&self.open).clone()
    }
    fn locate(&self, _: &Path) -> Option<Location> {
        unreachable!("worktrees never locates a path")
    }
    fn resolve(&self, _: &Path) -> Result<Resolved, ResolveError> {
        unreachable!("worktrees never resolves a path")
    }
    fn resolve_package(
        &self,
        _: &ProjectKey,
        _: &WorktreeKey,
        _: &str,
    ) -> Result<PackageMatch, lectern_services::project::ScanError> {
        unreachable!("worktrees never resolves a package")
    }
}

pub(crate) struct FakeSessions {
    id: SourceId,
    list: Mutex<Arc<SessionList>>,
    completed: Mutex<Vec<Vec<SessionKey>>>,
}

impl FakeSessions {
    pub fn new(id: &str) -> FakeSessions {
        FakeSessions {
            id: SourceId(Arc::from(id)),
            list: Mutex::new(list(1, Vec::new())),
            completed: Mutex::default(),
        }
    }

    /// The sessions of the list now.
    pub fn current(&self) -> Vec<SessionSummary> {
        lock(&self.list).sessions.to_vec()
    }

    /// Replaces the list; the generation goes up by one.
    pub fn set(&self, sessions: Vec<SessionSummary>) {
        let mut current = lock(&self.list);
        *current = list(current.generation + 1, sessions);
    }

    /// Each `complete` call's keys, in order.
    pub fn completed(&self) -> Vec<Vec<SessionKey>> {
        lock(&self.completed).clone()
    }
}

impl SessionSource for FakeSessions {
    fn id(&self) -> SourceId {
        self.id.clone()
    }
    fn agent(&self) -> Agent {
        Agent::ClaudeCode
    }
    fn sessions(&self) -> Arc<SessionList> {
        lock(&self.list).clone()
    }
    fn read(&self, _: &SessionKey, _: &Cursor, _: u64) -> Result<Batch, SessionError> {
        unreachable!("worktrees never reads a session")
    }
    fn turns(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        unreachable!("worktrees never reads turns")
    }
    fn turns_unretained(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        unreachable!("worktrees never reads turns")
    }
    fn load(&self, _: &SessionKey, _: &ContentRef) -> Result<Content, SessionError> {
        unreachable!("worktrees never loads content")
    }
    fn resume(&self, _: &SessionKey) -> Option<ResumeCommand> {
        unreachable!("worktrees never resumes a session")
    }
    fn complete(&self, sessions: &[SessionKey]) {
        lock(&self.completed).push(sessions.to_vec());
    }
}

#[derive(Default)]
pub(crate) struct FakeReview {
    overview: Mutex<Option<Arc<WorktreesOverview>>>,
    watched: Mutex<Vec<ProjectKey>>,
    refreshed: Mutex<Vec<ProjectKey>>,
    released: Arc<AtomicUsize>,
}

impl FakeReview {
    pub fn set_overview(&self, overview: Option<WorktreesOverview>) {
        *lock(&self.overview) = overview.map(Arc::new);
    }
    /// The projects `watch_worktrees` was called for, in order.
    pub fn watched(&self) -> Vec<ProjectKey> {
        lock(&self.watched).clone()
    }
    /// The projects `refresh_worktrees` was called for, in order.
    pub fn refreshed(&self) -> Vec<ProjectKey> {
        lock(&self.refreshed).clone()
    }
    /// Interests dropped so far.
    pub fn released(&self) -> usize {
        self.released.load(Ordering::SeqCst)
    }
}

impl Review for FakeReview {
    fn open(&self, _: ReviewTarget) -> ReviewId {
        unreachable!("worktrees never opens a review")
    }
    fn snapshot(&self, _: ReviewId) -> Option<Arc<ReviewSnapshot>> {
        unreachable!("worktrees never reads a review")
    }
    fn command(&self, _: ReviewId, _: ReviewCommand) {
        unreachable!("worktrees never commands a review")
    }
    fn rows(&self, _: ReviewId, _: &RowsKey) -> Option<Arc<FileRows>> {
        unreachable!("worktrees never reads rows")
    }
    fn text(&self, _: ReviewId, _: &TextKey) -> Option<Arc<str>> {
        unreachable!("worktrees never reads text")
    }
    fn close(&self, _: ReviewId) {
        unreachable!("worktrees never closes a review")
    }
    fn overview(&self, _: &ProjectKey) -> Option<Arc<Overview>> {
        unreachable!("worktrees never reads the project overview")
    }
    fn watch_overview(&self, _: &ProjectKey) -> OverviewInterest {
        unreachable!("worktrees never watches the project overview")
    }
    fn worktrees(&self, _: &ProjectKey) -> Option<Arc<WorktreesOverview>> {
        lock(&self.overview).clone()
    }
    fn watch_worktrees(&self, project: &ProjectKey) -> OverviewInterest {
        lock(&self.watched).push(project.clone());
        let released = self.released.clone();
        OverviewInterest::new(move || {
            released.fetch_add(1, Ordering::SeqCst);
        })
    }
    fn refresh_worktrees(&self, project: &ProjectKey) {
        lock(&self.refreshed).push(project.clone());
    }
    fn register_brief_rules(
        &self,
        _: Arc<dyn BriefRules>,
        _: ActivationToken,
    ) -> BriefRulesRegistration {
        unreachable!("worktrees registers no brief rules")
    }
    fn register_note_route(
        &self,
        _: Arc<dyn NoteRoute>,
        _: ActivationToken,
    ) -> NoteRouteRegistration {
        unreachable!("worktrees registers no note routes")
    }
}

#[derive(Default)]
pub(crate) struct FakeUsage {
    snapshots: Mutex<HashMap<UsageQuery, Arc<UsageSnapshot>>>,
    watched: Mutex<Vec<UsageQuery>>,
    released: Arc<AtomicUsize>,
}

impl FakeUsage {
    pub fn set_snapshot(&self, query: UsageQuery, snapshot: UsageSnapshot) {
        lock(&self.snapshots).insert(query, Arc::new(snapshot));
    }
    /// The queries `watch` was called with, in order.
    pub fn watched(&self) -> Vec<UsageQuery> {
        lock(&self.watched).clone()
    }
    /// Interests dropped so far.
    pub fn released(&self) -> usize {
        self.released.load(Ordering::SeqCst)
    }
}

impl Usage for FakeUsage {
    fn watch(&self, query: UsageQuery) -> UsageInterest {
        lock(&self.watched).push(query);
        let released = self.released.clone();
        UsageInterest::new(move || {
            released.fetch_add(1, Ordering::SeqCst);
        })
    }
    fn snapshot(&self, query: &UsageQuery) -> Option<Arc<UsageSnapshot>> {
        lock(&self.snapshots).get(query).cloned()
    }
    fn limits(&self) -> Arc<Limits> {
        unreachable!("worktrees never reads the limits")
    }
    fn command(&self, _: UsageCommand) {
        unreachable!("worktrees sends no usage command")
    }
    fn begin_analysis(&self, _: AnalysisStart) -> Result<AnalysisId, UsageError> {
        unreachable!("worktrees never analyzes")
    }
    fn meter(&self, _: MeterEvent) -> Result<(), UsageError> {
        unreachable!("worktrees never meters")
    }
    fn register_mr_source(&self, _: Arc<dyn MrSource>, _: ActivationToken) -> MrSourceRegistration {
        unreachable!("worktrees registers no merge request source")
    }
}

/// The window's side of `dyn Contributions`: keeps what was registered and
/// counts what was unregistered.
#[derive(Default)]
pub(crate) struct FakeContributions {
    contributions: Mutex<Vec<(ContributionKind, String, u16, Payload)>>,
    released: Arc<AtomicUsize>,
    tokens_given: Mutex<Vec<bool>>,
    commands: Mutex<Vec<WorkspaceCommand>>,
    milestones: Mutex<Vec<Milestone>>,
}

impl FakeContributions {
    /// The kinds and IDs of the contributions, in registration order.
    pub fn kinds(&self) -> Vec<(ContributionKind, String)> {
        lock(&self.contributions)
            .iter()
            .map(|(kind, id, _, _)| (*kind, id.clone()))
            .collect()
    }

    /// The kinds and IDs of the sidebar entries, in registration order.
    pub fn entries(&self) -> Vec<(ContributionKind, String)> {
        self.kinds()
            .into_iter()
            .filter(|(kind, _)| matches!(kind, ContributionKind::SidebarEntry { .. }))
            .collect()
    }

    /// How many registrations were dropped.
    pub fn released(&self) -> usize {
        self.released.load(Ordering::SeqCst)
    }

    /// Whether each registration came with the activation's token.
    pub fn tokens_given(&self) -> Vec<bool> {
        lock(&self.tokens_given).clone()
    }

    /// The sidebar entry registered under `id`.
    pub fn sidebar_item(&self, id: &str) -> Option<SidebarItem> {
        lock(&self.contributions)
            .iter()
            .filter(|(_, registered, _, _)| registered == id)
            .find_map(|(_, _, _, payload)| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                any.downcast_ref::<SidebarItem>().cloned()
            })
    }

    /// A clone of the payload registered under `id`, as its `ui::contrib` type.
    pub fn payload<T: Clone + 'static>(&self, id: &str) -> Option<T> {
        lock(&self.contributions)
            .iter()
            .filter(|(_, registered, _, _)| registered == id)
            .find_map(|(_, _, _, payload)| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                any.downcast_ref::<T>().cloned()
            })
    }

    /// The key bindings registered, loaded the way `workspace` loads them.
    pub fn key_bindings(&self, cx: &gpui::App) -> Vec<gpui::KeyBinding> {
        lock(&self.contributions)
            .iter()
            .filter(|(kind, _, _, _)| matches!(kind, ContributionKind::KeyBindings))
            .flat_map(|(_, _, _, payload)| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                let bindings = any.downcast_ref::<lectern_ui::contrib::KeyBindings>();
                bindings
                    .into_iter()
                    .flat_map(|bindings| bindings.0.iter())
                    .map(|(keys, action, context)| {
                        let predicate = context.map(|context| {
                            std::rc::Rc::new(
                                gpui::KeyBindingContextPredicate::parse(context)
                                    .expect("the binding's context parses"),
                            )
                        });
                        gpui::KeyBinding::load(
                            keys,
                            action.boxed_clone(),
                            predicate,
                            false,
                            None,
                            cx.keyboard_mapper().as_ref(),
                        )
                        .expect("the binding's keystrokes parse")
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The commands the views dispatched, in order.
    pub fn commands(&self) -> Vec<WorkspaceCommand> {
        lock(&self.commands).clone()
    }

    /// The milestones the views reported.
    pub fn milestones(&self) -> Vec<Milestone> {
        lock(&self.milestones).clone()
    }

    /// The order of the contribution registered under `id`.
    pub fn order(&self, id: &str) -> Option<u16> {
        lock(&self.contributions)
            .iter()
            .find(|(_, registered, _, _)| registered == id)
            .map(|(_, _, order, _)| *order)
    }
}

impl Contributions for FakeContributions {
    fn register(&self, contribution: Contribution, token: Option<ActivationToken>) -> Registration {
        let Contribution {
            kind,
            id,
            order,
            payload,
        } = contribution;
        lock(&self.tokens_given).push(token.is_some());
        lock(&self.contributions).push((kind, id.to_string(), order, payload));
        let released = self.released.clone();
        Registration::new(
            |_| {},
            move || {
                released.fetch_add(1, Ordering::SeqCst);
            },
        )
    }
    fn handle_command(
        &self,
        _: Name,
        _: CommandHandler,
        _: Option<ActivationToken>,
    ) -> Registration {
        unreachable!("worktrees handles no command")
    }
    fn register_leave_guard(
        &self,
        _: Arc<dyn LeaveGuard>,
        _: Option<ActivationToken>,
    ) -> Registration {
        unreachable!("worktrees registers no leave guard")
    }
    fn dispatch(&self, command: WorkspaceCommand) {
        lock(&self.commands).push(command);
    }
    fn sidebar_entries(&self) -> Arc<[SidebarEntry]> {
        unreachable!("worktrees never lists the sidebar entries")
    }
    fn current_route(&self, _window: WindowId) -> Option<Route> {
        None
    }
    fn report_milestone(&self, milestone: Milestone) {
        lock(&self.milestones).push(milestone);
    }
}

/// A `SessionsChanged` of the fake source.
pub(crate) fn sessions_event() -> SessionsChanged {
    SessionsChanged {
        source: SourceId(Arc::from("fake")),
        generation: 2,
        changed: Arc::from(Vec::new()),
        removed: Arc::from(Vec::new()),
    }
}

// --------------------------------------------------------------------- rig

/// Provides a service from a plain plugin: the kernel of the tests holds the
/// plugins under test and these.
struct Provide<S: ?Sized>(Arc<S>);

impl<S> Plugin for Provide<S>
where
    S: Service + ?Sized + 'static,
    Arc<S>: Send + Sync,
{
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<S>()],
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        ctx.provide::<S>(self.0.clone())?;
        Ok(Box::new(()))
    }
}

/// A plugin that activates only through `Kernel::activate`, so that a test
/// decides the order.
struct Manual<P>(P);

impl<P: Plugin> Plugin for Manual<P> {
    fn manifest(&self) -> Manifest {
        let mut manifest = self.0.manifest();
        manifest.activation = vec![Activation::Manual];
        manifest
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        self.0.activate(ctx)
    }
}

/// A kernel with fake services, the two plugins under test (not activated yet)
/// and an executor that runs tasks only when the test says so.
pub(crate) struct Rig {
    pub kernel: Arc<Kernel>,
    pub executor: Executor,
    pub projects: Arc<FakeProjects>,
    pub sessions: Arc<FakeSessions>,
    pub review: Arc<FakeReview>,
    pub usage: Arc<FakeUsage>,
    pub contributions: Arc<FakeContributions>,
    pub link: UsageLink,
    /// The time of day the plugin composes with; `advance` moves it.
    now: Arc<AtomicI64>,
    hub: Arc<Mutex<Option<Arc<Hub>>>>,
}

impl Rig {
    pub fn new() -> Rig {
        let executor = Executor::manual();
        let projects = Arc::new(FakeProjects::default());
        let sessions = Arc::new(FakeSessions::new("fake"));
        let review = Arc::new(FakeReview::default());
        let usage = Arc::new(FakeUsage::default());
        let contributions = Arc::new(FakeContributions::default());
        let link = UsageLink::default();
        let now = Arc::new(AtomicI64::new(NOW));
        let clock = now.clone();
        let main = WorktreesPlugin::new(WorktreesDeps {
            executor: executor.clone(),
            usage: link.clone(),
        })
        .with_wall_clock(move || Timestamp(clock.load(Ordering::SeqCst)));
        let hub = main.hub.clone();
        let set = PluginSet::new()
            .with("mock-projects", Provide::<dyn Projects>(projects.clone()))
            .with(
                "mock-sessions",
                Provide::<dyn SessionSource>(sessions.clone()),
            )
            .with("mock-review", Provide::<dyn Review>(review.clone()))
            .with("mock-usage", Provide::<dyn Usage>(usage.clone()))
            .with(
                "mock-contributions",
                Provide::<dyn Contributions>(contributions.clone()),
            )
            .with(PLUGIN_ID, Manual(main))
            .with(
                USAGE_PLUGIN_ID,
                Manual(WorktreesUsagePlugin::new(executor.clone(), link.clone())),
            );
        let kernel = Kernel::start(set);
        kernel.barrier();
        Rig {
            kernel,
            executor,
            projects,
            sessions,
            review,
            usage,
            contributions,
            link,
            now,
            hub,
        }
    }

    /// Moves the time of day the next composition counts from.
    pub fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn activate_main(&self) {
        self.kernel.activate(PLUGIN_ID);
        self.kernel.barrier();
    }

    pub fn activate_usage(&self) {
        self.kernel.activate(USAGE_PLUGIN_ID);
        self.kernel.barrier();
    }

    pub fn deactivate_usage(&self) {
        self.kernel.set_enabled(USAGE_PLUGIN_ID, false);
        self.kernel.barrier();
    }

    pub fn deactivate_main(&self) {
        self.kernel.set_enabled(PLUGIN_ID, false);
        self.kernel.barrier();
    }

    /// The state of the activation. Panics before `activate_main`.
    pub fn hub(&self) -> Arc<Hub> {
        lock(&self.hub).clone().expect("worktrees is activated")
    }

    /// The latest snapshot.
    pub fn snapshot(&self) -> Option<Arc<WorktreesSnapshot>> {
        self.hub().snapshot()
    }

    /// Runs the queued tasks; returns how many steps ran.
    pub fn run(&self) -> usize {
        self.executor.run_until_idle()
    }

    /// `Opened` for `key`, as `project` emits it.
    pub fn opened(&self, key: &ProjectKey) {
        self.kernel.emitter().emit(ProjectEvent::Opened {
            request: OpenRequest(1),
            result: Ok(key.clone()),
        });
    }

    pub fn changed(&self, key: &ProjectKey) {
        self.changed_parts(key, ChangedParts::default());
    }

    pub fn changed_parts(&self, key: &ProjectKey, parts: ChangedParts) {
        self.kernel.emitter().emit(ProjectEvent::Changed {
            project: key.clone(),
            generation: 2,
            parts,
        });
    }

    pub fn closed(&self, key: &ProjectKey) {
        self.kernel.emitter().emit(ProjectEvent::Closed {
            project: key.clone(),
        });
    }

    pub fn sessions_changed(&self) {
        self.kernel.emitter().emit(sessions_event());
    }

    /// The window shows `screen`, as `workspace` announces it.
    pub fn screen(&self, screen: &'static str) {
        self.kernel.emitter().emit(WorkspaceEvent::ScreenChanged {
            window: WindowId(0),
            from: None,
            to: Route {
                screen: Name::Borrowed(screen),
                target: Target::None,
            },
        });
    }

    pub fn show(&self) {
        self.screen(screens::WORKTREES);
    }

    pub fn hide(&self) {
        self.screen(screens::PROJECT_HOME);
    }

    pub fn worktrees_changed(&self, key: &ProjectKey) {
        self.kernel.emitter().emit(ReviewEvent::Worktrees {
            project: key.clone(),
            generation: 6,
        });
    }

    pub fn usage_changed(&self, query: UsageQuery) {
        self.kernel.emitter().emit(UsageEvent::Changed {
            query,
            generation: 4,
        });
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let (done, wait) = std::sync::mpsc::channel();
        self.kernel.shutdown(move || {
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(std::time::Duration::from_secs(10));
    }
}
