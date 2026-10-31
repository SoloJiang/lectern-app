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
    ActiveTree, ChangedParts, FileList, Head, Location, OpenRequest, PackageMatch, ProjectEvent,
    ProjectKey, ProjectSnapshot, Projects, RelPath, ResolveError, Resolved, Scan, Worktree,
    WorktreeKey,
};
use lectern_services::review::{
    BriefRules, BriefRulesRegistration, ChangeCount, FileRows, NoteRoute, NoteRouteRegistration,
    Overlap, Overview, OverviewInterest, Review, ReviewCommand, ReviewEvent, ReviewId,
    ReviewSnapshot, ReviewTarget, RowsKey, TextKey, WorktreeChanges, WorktreesOverview,
};
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, Format, Parent, Place, ResumeCommand, SessionError,
    SessionKey, SessionList, SessionSource, SessionSummary, SessionsChanged, SourceId, Timestamp,
    TurnIndex,
};
use lectern_services::usage::{
    AnalysisId, AnalysisStart, ForgeRepo, Limits, MeterEvent, MrLabel, MrRef, MrSource,
    MrSourceRegistration, MrState, MrUsage, SnapshotHeader, Tokens, Usage, UsageCommand,
    UsageError, UsageEvent, UsageInterest, UsageQuery, UsageSnapshot, UsageState, WorktreeUsage,
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

fn active_tree(root: Arc<Path>, worktree: Option<WorktreeKey>) -> ActiveTree {
    ActiveTree {
        root,
        worktree,
        opened_at: Arc::from(""),
        files: Scan::Ready(Arc::new(FileList::builder().build())),
        packages: Scan::Ready(Arc::from(Vec::new())),
        dirs: Scan::Pending,
    }
}

impl ProjectFx {
    /// A git project with its main worktree on `main`, active, with an empty
    /// ready file list.
    pub fn git(name: &str) -> ProjectFx {
        let main = worktree(name, "main", branch("main"));
        ProjectFx {
            name: name.to_owned(),
            key: git_key(name),
            active: Some(active_tree(main.root.clone(), Some(main.key.clone()))),
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
            active: Some(active_tree(
                Arc::from(Path::new(&format!("/fixture/{name}"))),
                None,
            )),
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

    /// Marks the worktree `name` as locked.
    pub fn locked(mut self, name: &str) -> ProjectFx {
        let key = wt_key(&self.name, name);
        if let Some(w) = self.worktrees.iter_mut().find(|w| w.key == key) {
            w.locked = true;
        }
        self
    }

    /// Makes the worktree `name` the active one.
    pub fn active_in(mut self, name: &str) -> ProjectFx {
        let key = wt_key(&self.name, name);
        let w = self.worktrees.iter().find(|w| w.key == key);
        if let (Some(w), Some(tree)) = (w, self.active.as_mut()) {
            tree.root = w.root.clone();
            tree.worktree = Some(w.key.clone());
        }
        self
    }

    /// The active tree's file list is still being scanned.
    pub fn files_pending(mut self) -> ProjectFx {
        if let Some(tree) = self.active.as_mut() {
            tree.files = Scan::Pending;
        }
        self
    }

    pub fn generation(mut self, generation: u64) -> ProjectFx {
        self.generation = generation;
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
    pub fn agent(mut self, agent: Agent) -> Sess {
        self.0.agent = agent;
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

pub(crate) fn list(generation: u64, sessions: Vec<SessionSummary>) -> Arc<SessionList> {
    Arc::new(SessionList {
        generation,
        sessions: Arc::from(sessions),
        diagnostics: Default::default(),
    })
}

// ----------------------------------------------------------------- changes

/// The counts of one worktree, with the overlaps as they are given.
pub(crate) fn changes_of(
    worktree: &WorktreeKey,
    uncommitted: ChangeCount,
    committed: ChangeCount,
    vs_base: ChangeCount,
    overlaps: Option<Vec<Overlap>>,
) -> WorktreeChanges {
    WorktreeChanges {
        worktree: worktree.clone(),
        head: None,
        uncommitted,
        committed,
        vs_base,
        overlaps: overlaps.map(Arc::from),
        counted_at: Some(Timestamp(NOW - 2 * MINUTE)),
    }
}

/// A worktree whose counts are all `Files`.
pub(crate) fn counted(
    worktree: &WorktreeKey,
    uncommitted: u32,
    committed: u32,
    vs_base: u32,
    overlaps: Vec<Overlap>,
) -> WorktreeChanges {
    changes_of(
        worktree,
        ChangeCount::Files(uncommitted),
        ChangeCount::Files(committed),
        ChangeCount::Files(vs_base),
        Some(overlaps),
    )
}

pub(crate) fn overlap(other: &WorktreeKey, files: u32, paths: &[&str]) -> Overlap {
    let paths: Vec<RelPath> = paths.iter().map(|p| Arc::from(*p)).collect();
    Overlap {
        other: other.clone(),
        files,
        paths: Arc::from(paths),
    }
}

pub(crate) fn overview_of(
    project: &ProjectKey,
    base: Option<&str>,
    worktrees: Vec<WorktreeChanges>,
) -> WorktreesOverview {
    WorktreesOverview {
        project: project.clone(),
        generation: 5,
        base: base.map(Arc::from),
        worktrees: Arc::from(worktrees),
    }
}

// ------------------------------------------------------------------- usage

pub(crate) fn tokens(input: u64, output: u64) -> Tokens {
    Tokens {
        input,
        output,
        ..Tokens::default()
    }
}

pub(crate) fn header(state: UsageState) -> SnapshotHeader {
    SnapshotHeader {
        generation: 3,
        state,
        may_include_inherited: false,
    }
}

pub(crate) fn mr_ref(number: u64) -> MrRef {
    MrRef {
        repo: ForgeRepo {
            host: Arc::from("forge.example"),
            id: Arc::from("1"),
        },
        number,
    }
}

pub(crate) fn mr_label(number: u64, title: &str) -> MrLabel {
    MrLabel {
        mr: mr_ref(number),
        title: Arc::from(title),
        state: MrState::Open,
    }
}

/// The usage of a worktree's branch.
pub(crate) fn branch_usage(
    state: UsageState,
    cumulative: Option<Tokens>,
    mrs: Vec<MrLabel>,
) -> WorktreeUsage {
    WorktreeUsage {
        header: header(state),
        branch: None,
        mrs: Arc::from(mrs),
        cumulative,
        review: tokens(10, 5),
        since: Some(Timestamp(NOW - 3 * DAY)),
    }
}

pub(crate) fn worktree_snapshot(usage: WorktreeUsage) -> UsageSnapshot {
    UsageSnapshot::Worktree(usage)
}

pub(crate) fn mr_snapshot(
    number: u64,
    state: UsageState,
    write: Tokens,
    shared_with: Vec<u64>,
) -> UsageSnapshot {
    UsageSnapshot::Mr(MrUsage {
        header: header(state),
        mr: mr_label(number, "title"),
        start: Timestamp(NOW - 3 * DAY),
        end: None,
        write,
        review: Tokens::default(),
        shared_with: shared_with.into_iter().map(mr_ref).collect(),
        sessions: Arc::from(Vec::new()),
    })
}

pub(crate) fn worktree_query(project: &ProjectKey, worktree: &WorktreeKey) -> UsageQuery {
    UsageQuery::Worktree {
        project: project.clone(),
        worktree: worktree.clone(),
    }
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
