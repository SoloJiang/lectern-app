//! Test doubles and fixtures (docs/design/project_home.md「测试」).
//!
//! The services are fakes of the `services` traits that hand out what a test
//! constructs and record what the plugin does. `read`, `turns`, `load`,
//! `resolve` and the other calls this crate must not make are
//! `unreachable!`. The other plugins' crates are not dependencies, not even
//! as dev-dependencies (architecture rule 7), so nothing here comes from
//! their `test-support`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lectern_executor::Executor;
use lectern_kernel::ActivationToken;
use lectern_kernel::{
    Activation, Active, Context, Kernel, Manifest, Name, Plugin, PluginError, PluginSet, Service,
    service,
};
use lectern_services::project::{
    ActiveTree, ChangedParts, FileList, Head, Location, OpenRequest, Package, PackageMatch,
    ProjectEvent, ProjectKey, ProjectSnapshot, Projects, ResolveError, Resolved, Scan, ScanError,
    Worktree, WorktreeKey,
};
use lectern_services::review::{
    BriefRules, BriefRulesRegistration, FileRows, NoteRoute, NoteRouteRegistration, OutsideCount,
    Overview, OverviewInterest, Progress, Review, ReviewCommand, ReviewEvent, ReviewId,
    ReviewSnapshot, ReviewTarget, RowsKey, SessionOverview, TextKey,
};
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, Format, Parent, Place, ResumeCommand, SessionError,
    SessionKey, SessionList, SessionSource, SessionSummary, SessionsChanged, SourceId, Timestamp,
    TurnIndex,
};
use lectern_services::usage::{
    AnalysisId, AnalysisStart, LimitKind, LimitSource, LimitWindow, Limits, MeterEvent, MrSource,
    MrSourceRegistration, ProjectInfo, ProjectStatus, ProjectUsage, SnapshotHeader, Tokens, Usage,
    UsageCommand, UsageError, UsageInterest, UsageQuery, UsageSnapshot, UsageState, Window,
};
use lectern_services::workspace::{
    CommandHandler, Contribution, ContributionKind, Contributions, LeaveGuard, Milestone, Payload,
    Registration, Route, SharedPayload, SidebarEntry, SidebarSection, WindowId, WorkspaceCommand,
};
use lectern_ui::contrib::SidebarItem;

use crate::home::Home;
use crate::plugin::{
    PLUGIN_ID, ProjectHomeDeps, ProjectHomePlugin, ProjectHomeUsagePlugin, USAGE_PLUGIN_ID,
};
use crate::snapshot::HomeSnapshot;
use crate::usage_slot::UsageSlot;

/// The composing time of the tests: 2027-01-15 08:00 UTC, in milliseconds.
pub(crate) const NOW: i64 = 1_800_000_000_000;
pub(crate) const MINUTE: i64 = 60_000;
pub(crate) const DAY: i64 = 24 * 60 * MINUTE;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------- projects

pub(crate) fn git_key(name: &str) -> ProjectKey {
    ProjectKey::Git {
        common_dir: Arc::from(Path::new(&format!("/fixture/{name}/.git"))),
    }
}

pub(crate) fn folder_key(name: &str) -> ProjectKey {
    ProjectKey::Folder {
        root: Arc::from(Path::new(&format!("/fixture/{name}"))),
    }
}

/// A worktree key of the project `name`; "main" is the main worktree.
pub(crate) fn wt_key(project: &str, worktree: &str) -> WorktreeKey {
    let path = if worktree == "main" {
        format!("/fixture/{project}/.git")
    } else {
        format!("/fixture/{project}/.git/worktrees/{worktree}")
    };
    WorktreeKey(Arc::from(Path::new(&path)))
}

pub(crate) fn branch(name: &str) -> Head {
    Head::Branch(Arc::from(format!("refs/heads/{name}")))
}

/// Builds a `ProjectSnapshot`.
pub(crate) struct ProjectFx {
    name: String,
    key: ProjectKey,
    worktrees: Vec<Worktree>,
    active: Option<ActiveTree>,
    generation: u64,
}

impl ProjectFx {
    /// A git project with its main worktree on `main`, active, with an empty
    /// ready file list and no sub-projects.
    pub fn git(name: &str) -> ProjectFx {
        let main = worktree(name, "main", branch("main"));
        ProjectFx {
            name: name.to_owned(),
            key: git_key(name),
            active: Some(ActiveTree {
                root: main.root.clone(),
                worktree: Some(main.key.clone()),
                opened_at: Arc::from(""),
                files: Scan::Ready(Arc::new(FileList::builder().build())),
                packages: Scan::Ready(Arc::from(Vec::new())),
                dirs: Scan::Pending,
            }),
            worktrees: vec![main],
            generation: 1,
        }
    }

    /// A plain folder: no worktrees, an empty ready file list.
    pub fn folder(name: &str) -> ProjectFx {
        ProjectFx {
            name: name.to_owned(),
            key: folder_key(name),
            worktrees: Vec::new(),
            active: Some(ActiveTree {
                root: Arc::from(Path::new(&format!("/fixture/{name}"))),
                worktree: None,
                opened_at: Arc::from(""),
                files: Scan::Ready(Arc::new(FileList::builder().build())),
                packages: Scan::Ready(Arc::from(Vec::new())),
                dirs: Scan::Pending,
            }),
            generation: 1,
        }
    }

    pub fn key(&self) -> ProjectKey {
        self.key.clone()
    }

    /// The key of this project's worktree `name` ("main" for the main one).
    pub fn wt(&self, name: &str) -> WorktreeKey {
        wt_key(&self.name, name)
    }

    /// Adds a linked worktree.
    pub fn linked(mut self, name: &str, head: Head) -> ProjectFx {
        self.worktrees.push(worktree(&self.name, name, head));
        self
    }

    /// Adds a linked worktree whose root no longer exists.
    pub fn prunable(mut self, name: &str, head: Head) -> ProjectFx {
        let mut w = worktree(&self.name, name, head);
        w.prunable = true;
        self.worktrees.push(w);
        self
    }

    /// Makes the linked worktree `name` the active one.
    pub fn active_in(mut self, name: &str) -> ProjectFx {
        let w = self
            .worktrees
            .iter()
            .find(|w| w.key == wt_key(&self.name, name));
        if let (Some(w), Some(tree)) = (w, self.active.as_mut()) {
            tree.root = w.root.clone();
            tree.worktree = Some(w.key.clone());
        }
        self
    }

    pub fn head(mut self, worktree: &str, head: Head) -> ProjectFx {
        let key = wt_key(&self.name, worktree);
        if let Some(w) = self.worktrees.iter_mut().find(|w| w.key == key) {
            w.head = head;
        }
        self
    }

    pub fn files(mut self, files: Scan<()>) -> ProjectFx {
        if let Some(tree) = self.active.as_mut() {
            tree.files = match files {
                Scan::Pending => Scan::Pending,
                Scan::Ready(()) => Scan::Ready(Arc::new(FileList::builder().build())),
                Scan::Unavailable(error) => Scan::Unavailable(error),
            };
        }
        self
    }

    pub fn packages(mut self, packages: Scan<Vec<Package>>) -> ProjectFx {
        if let Some(tree) = self.active.as_mut() {
            tree.packages = match packages {
                Scan::Pending => Scan::Pending,
                Scan::Ready(list) => Scan::Ready(Arc::from(list)),
                Scan::Unavailable(error) => Scan::Unavailable(error),
            };
        }
        self
    }

    pub fn no_active_tree(mut self) -> ProjectFx {
        self.active = None;
        self
    }

    pub fn snapshot(&self) -> ProjectSnapshot {
        ProjectSnapshot {
            key: self.key.clone(),
            generation: self.generation,
            name: Arc::from(self.name.as_str()),
            worktrees: Arc::from(self.worktrees.clone()),
            active: self.active.clone(),
        }
    }
}

fn worktree(project: &str, name: &str, head: Head) -> Worktree {
    Worktree {
        key: wt_key(project, name),
        root: Arc::from(Path::new(&format!("/fixture/{project}-{name}"))),
        is_main: name == "main",
        head,
        locked: false,
        prunable: false,
    }
}

pub(crate) fn unavailable() -> ScanError {
    ScanError::IndexUnreadable(Arc::from("fixture"))
}

/// A sub-project at `dir`; `parent` is the index of the enclosing one.
pub(crate) fn package(dir: &str, name: &str, parent: Option<u32>) -> Package {
    Package {
        dir: Arc::from(dir),
        name: Arc::from(name),
        manifests: Arc::from(Vec::new()),
        declared: true,
        workspace_root: false,
        parent,
        files: 1,
    }
}

// ---------------------------------------------------------------- sessions

/// Builds a `SessionSummary` that is placed in a worktree of a project.
pub(crate) struct Sess(SessionSummary);

/// A session `id` of source `source`, located in `worktree` of `project`.
pub(crate) fn sess(
    source: &str,
    id: &str,
    project: &ProjectKey,
    worktree: Option<&WorktreeKey>,
) -> Sess {
    Sess(SessionSummary {
        key: SessionKey {
            source: SourceId(Arc::from(source)),
            id: Arc::from(id),
        },
        agent: Agent::ClaudeCode,
        parent: None,
        file: Arc::from(Path::new(&format!("/fixture/log/{id}.jsonl"))),
        cwd: Arc::from(Path::new("/fixture/cwd")),
        place: Place::Located(Location {
            project: project.clone(),
            worktree: worktree.cloned(),
            rel: Arc::from(""),
            package: PackageMatch::None,
        }),
        started_at: Timestamp(NOW - DAY),
        last_activity: Timestamp(NOW - MINUTE),
        title: Some(Arc::from(format!("title of {id}").as_str())),
        agent_version: None,
        logged_branch: None,
        open_turn: false,
        tail_pending: false,
        archived: false,
        inheritance_unknown: false,
        format: Format::Known,
    })
}

impl Sess {
    pub fn key(&self) -> SessionKey {
        self.0.key.clone()
    }
    pub fn title(mut self, title: &str) -> Sess {
        self.0.title = Some(Arc::from(title));
        self
    }
    pub fn untitled(mut self) -> Sess {
        self.0.title = None;
        self
    }
    /// The last activity `ms` before `NOW`.
    pub fn ago(mut self, ms: i64) -> Sess {
        self.0.last_activity = Timestamp(NOW - ms);
        self
    }
    pub fn open_turn(mut self) -> Sess {
        self.0.open_turn = true;
        self
    }
    pub fn pending(mut self) -> Sess {
        self.0.tail_pending = true;
        self
    }
    pub fn agent(mut self, agent: Agent) -> Sess {
        self.0.agent = agent;
        self
    }
    pub fn child_of(mut self, parent: &SessionKey) -> Sess {
        self.0.parent = Some(Parent {
            session: parent.clone(),
            tool_call: None,
        });
        self
    }
    /// Placed by `Projects::resolve` (the project was not open when it was
    /// placed) instead of by `locate`.
    pub fn resolved(mut self) -> Sess {
        if let Place::Located(location) = &self.0.place {
            self.0.place = Place::Resolved(Resolved {
                project: location.project.clone(),
                worktree: location.worktree.clone(),
                rel: location.rel.clone(),
            });
        }
        self
    }
    pub fn unplaced(mut self) -> Sess {
        self.0.place = Place::Pending;
        self
    }
    pub fn build(self) -> SessionSummary {
        self.0
    }
}

impl From<Sess> for SessionSummary {
    fn from(sess: Sess) -> SessionSummary {
        sess.0
    }
}

pub(crate) fn list(generation: u64, sessions: Vec<SessionSummary>) -> Arc<SessionList> {
    Arc::new(SessionList {
        generation,
        sessions: Arc::from(sessions),
        diagnostics: Default::default(),
    })
}

// ---------------------------------------------------------------- overview

pub(crate) fn progress(seen: u32, total: u32, changed_since_seen: u32) -> Option<Progress> {
    Some(Progress {
        total,
        seen,
        changed_since_seen,
    })
}

pub(crate) fn entry(
    session: &SessionKey,
    progress: Option<Progress>,
    drifted: Option<bool>,
    critical: u32,
) -> SessionOverview {
    SessionOverview {
        session: session.clone(),
        progress,
        drifted,
        critical,
    }
}

pub(crate) fn overview(
    project: &ProjectKey,
    sessions: Vec<SessionOverview>,
    outside: OutsideCount,
) -> Overview {
    Overview {
        project: project.clone(),
        generation: 7,
        sessions: Arc::from(sessions),
        outside,
        packages: None,
    }
}

// ------------------------------------------------------------------- usage

pub(crate) fn usage_limits(windows: Vec<(Agent, f32)>, tightest: Option<u16>) -> Arc<Limits> {
    Arc::new(Limits {
        generation: 4,
        windows: windows
            .into_iter()
            .map(|(agent, used_percent)| LimitWindow {
                agent,
                source: LimitSource::SessionLog,
                limit_id: None,
                kind: LimitKind::FiveHour,
                window_minutes: Some(300),
                used_percent,
                resets_at: Some(Timestamp(NOW + 60 * MINUTE)),
                sampled_at: Timestamp(NOW),
                expired: false,
            })
            .collect(),
        tightest,
    })
}

pub(crate) fn project_usage(state: UsageState, total: u64) -> Arc<UsageSnapshot> {
    Arc::new(UsageSnapshot::Project(ProjectUsage {
        header: SnapshotHeader {
            generation: 9,
            state,
            may_include_inherited: false,
        },
        project: ProjectInfo {
            id: lectern_services::usage::UsageProjectId([1; 16]),
            name: Arc::from("app"),
            folder: false,
            key: None,
            status: ProjectStatus::Active,
        },
        window: Window::D7,
        days: Arc::from(Vec::new()),
        total: Tokens {
            input: total,
            ..Tokens::default()
        },
        rows: Arc::from(Vec::new()),
        by_model: Arc::from(Vec::new()),
        by_agent: Arc::from(Vec::new()),
        daily: Arc::from(Vec::new()),
    }))
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
        unreachable!("project_home never opens a project")
    }
    fn close(&self, _: &ProjectKey) {
        unreachable!("project_home never closes a project")
    }
    fn set_active_worktree(&self, _: &ProjectKey, _: &WorktreeKey) {
        unreachable!("project_home never switches the worktree")
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
        unreachable!("project_home never locates a path")
    }
    fn resolve(&self, _: &Path) -> Result<Resolved, ResolveError> {
        unreachable!("project_home never resolves a path")
    }
    fn resolve_package(
        &self,
        _: &ProjectKey,
        _: &WorktreeKey,
        _: &str,
    ) -> Result<PackageMatch, ScanError> {
        unreachable!("project_home never resolves a package")
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
        unreachable!("project_home never reads a session")
    }
    fn turns(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        unreachable!("project_home never reads turns")
    }
    fn turns_unretained(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        unreachable!("project_home never reads turns")
    }
    fn load(&self, _: &SessionKey, _: &ContentRef) -> Result<Content, SessionError> {
        unreachable!("project_home never loads content")
    }
    fn resume(&self, _: &SessionKey) -> Option<ResumeCommand> {
        unreachable!("project_home never resumes a session")
    }
    fn complete(&self, sessions: &[SessionKey]) {
        lock(&self.completed).push(sessions.to_vec());
    }
}

#[derive(Default)]
pub(crate) struct FakeReview {
    overview: Mutex<Option<Arc<Overview>>>,
    watched: Mutex<Vec<ProjectKey>>,
    released: Arc<AtomicUsize>,
}

impl FakeReview {
    pub fn set_overview(&self, overview: Option<Overview>) {
        *lock(&self.overview) = overview.map(Arc::new);
    }
    /// The projects `watch_overview` was called for, in order.
    pub fn watched(&self) -> Vec<ProjectKey> {
        lock(&self.watched).clone()
    }
    /// Interests dropped so far.
    pub fn released(&self) -> usize {
        self.released.load(Ordering::SeqCst)
    }
}

impl Review for FakeReview {
    fn open(&self, _: ReviewTarget) -> ReviewId {
        unreachable!("project_home never opens a review")
    }
    fn snapshot(&self, _: ReviewId) -> Option<Arc<ReviewSnapshot>> {
        unreachable!("project_home never reads a review")
    }
    fn command(&self, _: ReviewId, _: ReviewCommand) {
        unreachable!("project_home never commands a review")
    }
    fn rows(&self, _: ReviewId, _: &RowsKey) -> Option<Arc<FileRows>> {
        unreachable!("project_home never reads rows")
    }
    fn text(&self, _: ReviewId, _: &TextKey) -> Option<Arc<str>> {
        unreachable!("project_home never reads text")
    }
    fn close(&self, _: ReviewId) {
        unreachable!("project_home never closes a review")
    }
    fn overview(&self, _: &ProjectKey) -> Option<Arc<Overview>> {
        lock(&self.overview).clone()
    }
    fn watch_overview(&self, project: &ProjectKey) -> OverviewInterest {
        lock(&self.watched).push(project.clone());
        let released = self.released.clone();
        OverviewInterest::new(move || {
            released.fetch_add(1, Ordering::SeqCst);
        })
    }
    fn register_brief_rules(
        &self,
        _: Arc<dyn BriefRules>,
        _: ActivationToken,
    ) -> BriefRulesRegistration {
        unreachable!("project_home registers no brief rules")
    }

    fn register_note_route(
        &self,
        _: Arc<dyn NoteRoute>,
        _: ActivationToken,
    ) -> NoteRouteRegistration {
        unreachable!("project_home registers no note routes")
    }
}

#[derive(Default)]
pub(crate) struct FakeUsage {
    snapshot: Mutex<Option<Arc<UsageSnapshot>>>,
    limits: Mutex<Arc<Limits>>,
    watched: Mutex<Vec<UsageQuery>>,
    released: Arc<AtomicUsize>,
}

impl FakeUsage {
    pub fn set_snapshot(&self, snapshot: Option<UsageSnapshot>) {
        *lock(&self.snapshot) = snapshot.map(Arc::new);
    }
    pub fn set_limits(&self, limits: Limits) {
        *lock(&self.limits) = Arc::new(limits);
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
    fn snapshot(&self, _: &UsageQuery) -> Option<Arc<UsageSnapshot>> {
        lock(&self.snapshot).clone()
    }
    fn limits(&self) -> Arc<Limits> {
        lock(&self.limits).clone()
    }
    fn command(&self, _: UsageCommand) {
        unreachable!("project_home sends no usage command")
    }
    fn begin_analysis(&self, _: AnalysisStart) -> Result<AnalysisId, UsageError> {
        unreachable!("project_home never analyzes")
    }
    fn meter(&self, _: MeterEvent) -> Result<(), UsageError> {
        unreachable!("project_home never meters")
    }
    fn register_mr_source(&self, _: Arc<dyn MrSource>, _: ActivationToken) -> MrSourceRegistration {
        unreachable!("project_home registers no merge request source")
    }
}

/// The window's side of `dyn Contributions`: keeps what was registered (the
/// payloads the views are built from), lists the sidebar entries the way
/// `workspace` does (an entry is applied when registered), and records the
/// commands and the milestones the views send.
#[derive(Default)]
pub(crate) struct FakeContributions {
    registered: AtomicUsize,
    contributions: Mutex<Vec<(ContributionKind, String, Payload)>>,
    entries: Mutex<Vec<SidebarEntry>>,
    listed: Mutex<Option<Arc<[SidebarEntry]>>>,
    commands: Mutex<Vec<WorkspaceCommand>>,
    milestones: Mutex<Vec<Milestone>>,
}

impl FakeContributions {
    /// How many registrations of any kind were made.
    pub fn registered(&self) -> usize {
        self.registered.load(Ordering::SeqCst)
    }

    /// The kinds and IDs of the contributions, in registration order.
    pub fn kinds(&self) -> Vec<(ContributionKind, String)> {
        lock(&self.contributions)
            .iter()
            .map(|(kind, id, _)| (*kind, id.clone()))
            .collect()
    }

    /// A clone of the payload registered under `id`, as its `ui::contrib` type.
    pub fn payload<T: Clone + 'static>(&self, id: &str) -> Option<T> {
        lock(&self.contributions)
            .iter()
            .filter(|(_, registered, _)| registered == id)
            .find_map(|(_, _, payload)| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                any.downcast_ref::<T>().cloned()
            })
    }

    /// The key bindings registered, loaded the way `workspace` loads them.
    pub fn key_bindings(&self, cx: &gpui::App) -> Vec<gpui::KeyBinding> {
        lock(&self.contributions)
            .iter()
            .filter(|(kind, _, _)| matches!(kind, ContributionKind::KeyBindings))
            .flat_map(|(_, _, payload)| {
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

    /// Adds an entry the way another crate's registration would be applied,
    /// and lists a new `Arc` (the registry's list is replaced, not edited).
    pub fn add_entry(&self, section: SidebarSection, id: &str, order: u16, item: SidebarItem) {
        let mut entries = lock(&self.entries);
        entries.push(SidebarEntry {
            id: Name::Owned(id.to_owned()),
            section,
            order,
            payload: SharedPayload(Arc::new(item)),
        });
        entries.sort_by(|a, b| (a.section, a.order, &a.id).cmp(&(b.section, b.order, &b.id)));
        *lock(&self.listed) = Some(Arc::from(entries.clone()));
    }

    /// The commands the views dispatched, in order.
    pub fn commands(&self) -> Vec<WorkspaceCommand> {
        lock(&self.commands).clone()
    }

    /// The milestones the views reported.
    pub fn milestones(&self) -> Vec<Milestone> {
        lock(&self.milestones).clone()
    }
}

impl Contributions for FakeContributions {
    fn register(&self, contribution: Contribution, _: Option<ActivationToken>) -> Registration {
        self.registered.fetch_add(1, Ordering::SeqCst);
        let Contribution {
            kind,
            id,
            order,
            payload,
        } = contribution;
        if let ContributionKind::SidebarEntry { section } = kind {
            let any: &(dyn std::any::Any + Send) = &*payload.0;
            if let Some(item) = any.downcast_ref::<SidebarItem>() {
                self.add_entry(section, &id, order, item.clone());
            }
        }
        lock(&self.contributions).push((kind, id.to_string(), payload));
        Registration::new(|_| {}, || {})
    }
    fn handle_command(
        &self,
        _: Name,
        _: CommandHandler,
        _: Option<ActivationToken>,
    ) -> Registration {
        self.registered.fetch_add(1, Ordering::SeqCst);
        Registration::new(|_| {}, || {})
    }
    fn register_leave_guard(
        &self,
        _: Arc<dyn LeaveGuard>,
        _: Option<ActivationToken>,
    ) -> Registration {
        self.registered.fetch_add(1, Ordering::SeqCst);
        Registration::new(|_| {}, || {})
    }
    fn dispatch(&self, command: WorkspaceCommand) {
        lock(&self.commands).push(command);
    }
    fn sidebar_entries(&self) -> Arc<[SidebarEntry]> {
        lock(&self.listed)
            .get_or_insert_with(|| Arc::from(Vec::new()))
            .clone()
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

/// A kernel with fake services, the two plugins under test (not activated
/// yet) and an executor that runs tasks only when the test says so.
pub(crate) struct Rig {
    pub kernel: Arc<Kernel>,
    pub executor: Executor,
    pub projects: Arc<FakeProjects>,
    pub sessions: Arc<FakeSessions>,
    pub review: Arc<FakeReview>,
    pub usage: Arc<FakeUsage>,
    pub contributions: Arc<FakeContributions>,
    home: Arc<Mutex<Option<Arc<Home>>>>,
}

impl Rig {
    pub fn new() -> Rig {
        let executor = Executor::manual();
        let projects = Arc::new(FakeProjects::default());
        let sessions = Arc::new(FakeSessions::new("fake"));
        let review = Arc::new(FakeReview::default());
        let usage = Arc::new(FakeUsage::default());
        let contributions = Arc::new(FakeContributions::default());
        let slot = UsageSlot::default();
        let home_plugin = ProjectHomePlugin::new(ProjectHomeDeps {
            executor: executor.clone(),
            usage: slot.clone(),
        })
        .with_wall_clock(|| Timestamp(NOW));
        let home = home_plugin.home.clone();
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
            .with(PLUGIN_ID, Manual(home_plugin))
            .with(
                USAGE_PLUGIN_ID,
                Manual(ProjectHomeUsagePlugin::new(executor.clone(), slot.clone())),
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
            home,
        }
    }

    pub fn activate_home(&self) {
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

    /// The state of the activation. Panics before `activate_home`.
    pub fn home(&self) -> Arc<Home> {
        lock(&self.home).clone().expect("project-home is activated")
    }

    /// The latest snapshot.
    pub fn snapshot(&self) -> Option<Arc<HomeSnapshot>> {
        self.home().snapshot()
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
        self.kernel.emitter().emit(ProjectEvent::Changed {
            project: key.clone(),
            generation: 2,
            parts: ChangedParts::default(),
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

    /// An event of the window, as `workspace` emits it.
    pub fn workspace_event(&self, event: lectern_services::workspace::WorkspaceEvent) {
        self.kernel.emitter().emit(event);
    }

    pub fn overview_changed(&self, key: &ProjectKey) {
        self.kernel.emitter().emit(ReviewEvent::Overview {
            project: key.clone(),
            generation: 8,
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
