//! Fakes and the rig: a `dyn Projects`, session sources, a `dyn Review` and a
//! `dyn Contributions` written for these tests, and the three plugins on
//! `TestKernel`s sharing one model and one manual executor. Every path here
//! is synthetic; nothing reads a real session directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use lectern_executor::Executor;
use lectern_kernel::testing::TestKernel;
use lectern_kernel::{ActivationToken, Event};
use lectern_services::project::{
    ActiveTree, Location, OpenError, OpenRequest, PackageMatch, ProjectEvent, ProjectKey,
    ProjectSnapshot, Projects, ResolveError, Resolved, Scan, ScanError, WorktreeKey,
};
use lectern_services::review::{
    BriefRules, BriefRulesRegistration, FileRows, NoteRoute, NoteRouteRegistration, OutsideCount,
    Overview, OverviewInterest, Progress, Review, ReviewCommand, ReviewEvent, ReviewId,
    ReviewSnapshot, ReviewTarget, RowsKey, SessionOverview, TextKey,
};
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, Format, Parent, Place, ResumeCommand, SessionError,
    SessionKey, SessionList, SessionSource, SessionSummary, SourceId, Timestamp, TurnIndex,
};
use lectern_services::workspace::{
    CommandHandler, Contribution, ContributionKind, Contributions, LeaveGuard, Milestone, Payload,
    Registration, Route, SidebarEntry, WindowId, WorkspaceCommand,
};
use lectern_ui::contrib::{KeyBindings, Screen};

use crate::model::WelcomeModel;
use crate::plugin::{WelcomePlugin, from_model};
use crate::prefs::{Done, LanguagePrefs};
use crate::recent::store::testing as files;
use lectern_kernel::Name;

pub(crate) const NOW: i64 = 1_780_000_000_000;
pub(crate) const MINUTE: i64 = 60_000;

pub(crate) fn folder_key(root: &str) -> ProjectKey {
    ProjectKey::Folder {
        root: Arc::from(Path::new(root)),
    }
}

pub(crate) fn git_key(common_dir: &str) -> ProjectKey {
    ProjectKey::Git {
        common_dir: Arc::from(Path::new(common_dir)),
    }
}

/// A snapshot of a project opened at `opened_at` below `root`.
pub(crate) fn snapshot(
    key: &ProjectKey,
    name: &str,
    root: &str,
    opened_at: &str,
) -> Arc<ProjectSnapshot> {
    Arc::new(ProjectSnapshot {
        key: key.clone(),
        generation: 1,
        name: Arc::from(name),
        worktrees: Arc::from(Vec::new()),
        active: Some(ActiveTree {
            root: Arc::from(Path::new(root)),
            worktree: None,
            opened_at: Arc::from(opened_at),
            files: Scan::Unavailable(ScanError::RootMissing),
            packages: Scan::Unavailable(ScanError::RootMissing),
            dirs: Scan::Unavailable(ScanError::RootMissing),
        }),
    })
}

/// A snapshot with no current worktree: a bare repository.
pub(crate) fn bare_snapshot(key: &ProjectKey, name: &str) -> Arc<ProjectSnapshot> {
    Arc::new(ProjectSnapshot {
        key: key.clone(),
        generation: 1,
        name: Arc::from(name),
        worktrees: Arc::from(Vec::new()),
        active: None,
    })
}

/// A `dyn Projects` that keeps a table, and records the paths it was asked
/// to open (the screen must never ask).
#[derive(Default)]
pub(crate) struct FakeProjects {
    open: Mutex<Vec<ProjectKey>>,
    snapshots: Mutex<HashMap<ProjectKey, Arc<ProjectSnapshot>>>,
    pub(crate) asked_to_open: Mutex<Vec<PathBuf>>,
}

impl FakeProjects {
    /// The project is open from now on.
    pub(crate) fn add(&self, snapshot: Arc<ProjectSnapshot>) {
        self.open.lock().unwrap().push(snapshot.key.clone());
        self.snapshots
            .lock()
            .unwrap()
            .insert(snapshot.key.clone(), snapshot);
    }

    /// A snapshot `snapshot` can answer for a project that is not in
    /// `open_projects`.
    pub(crate) fn remember(&self, snapshot: Arc<ProjectSnapshot>) {
        self.snapshots
            .lock()
            .unwrap()
            .insert(snapshot.key.clone(), snapshot);
    }
}

impl Projects for FakeProjects {
    fn open(&self, path: &Path) -> OpenRequest {
        self.asked_to_open.lock().unwrap().push(path.to_path_buf());
        OpenRequest(0)
    }

    fn close(&self, _: &ProjectKey) {}

    fn set_active_worktree(&self, _: &ProjectKey, _: &WorktreeKey) {}

    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        self.snapshots.lock().unwrap().get(project).cloned()
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        self.open.lock().unwrap().clone()
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

/// A session source with a list the test replaces.
pub(crate) struct FakeSource {
    id: SourceId,
    agent: Agent,
    list: Mutex<Arc<SessionList>>,
}

impl FakeSource {
    pub(crate) fn new(id: &str, agent: Agent) -> Arc<FakeSource> {
        Arc::new(FakeSource {
            id: SourceId(Arc::from(id)),
            agent,
            list: Mutex::new(Arc::default()),
        })
    }

    pub(crate) fn set(&self, sessions: Vec<SessionSummary>) {
        let mut list = self.list.lock().unwrap();
        *list = Arc::new(SessionList {
            generation: list.generation + 1,
            sessions: sessions.into(),
            ..SessionList::default()
        });
    }

    pub(crate) fn changed(&self) -> lectern_services::session::SessionsChanged {
        lectern_services::session::SessionsChanged {
            source: self.id.clone(),
            generation: self.list.lock().unwrap().generation,
            changed: Arc::from(Vec::new()),
            removed: Arc::from(Vec::new()),
        }
    }
}

impl SessionSource for FakeSource {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn agent(&self) -> Agent {
        self.agent.clone()
    }

    fn sessions(&self) -> Arc<SessionList> {
        self.list.lock().unwrap().clone()
    }

    fn read(&self, _: &SessionKey, _: &Cursor, _: u64) -> Result<Batch, SessionError> {
        unreachable!("the welcome screen never reads a session")
    }

    fn turns(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        unreachable!("the welcome screen never reads a session")
    }

    fn turns_unretained(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        unreachable!("the welcome screen never reads a session")
    }

    fn load(&self, _: &SessionKey, _: &ContentRef) -> Result<Content, SessionError> {
        unreachable!("the welcome screen never reads a session")
    }

    fn resume(&self, _: &SessionKey) -> Option<ResumeCommand> {
        None
    }

    fn complete(&self, _: &[SessionKey]) {}
}

/// A synthetic session: `cwd` below the project root, placed in `place`.
pub(crate) struct Sess {
    pub(crate) id: String,
    pub(crate) source: &'static str,
    pub(crate) agent: Agent,
    pub(crate) cwd: String,
    pub(crate) place: Place,
    pub(crate) last_activity: i64,
    pub(crate) open_turn: bool,
    pub(crate) parent: Option<String>,
}

impl Sess {
    pub(crate) fn new(id: impl Into<String>, cwd: impl Into<String>, place: Place) -> Sess {
        Sess {
            id: id.into(),
            source: "session-claude-code",
            agent: Agent::ClaudeCode,
            cwd: cwd.into(),
            place,
            last_activity: NOW - MINUTE,
            open_turn: false,
            parent: None,
        }
    }

    pub(crate) fn at(mut self, last_activity: i64) -> Sess {
        self.last_activity = last_activity;
        self
    }

    pub(crate) fn running(mut self) -> Sess {
        self.open_turn = true;
        self
    }

    pub(crate) fn agent(mut self, source: &'static str, agent: Agent) -> Sess {
        self.source = source;
        self.agent = agent;
        self
    }

    pub(crate) fn child_of(mut self, parent: &str) -> Sess {
        self.parent = Some(parent.to_owned());
        self
    }

    pub(crate) fn build(self) -> SessionSummary {
        let source = SourceId(Arc::from(self.source));
        SessionSummary {
            key: SessionKey {
                source: source.clone(),
                id: Arc::from(self.id.as_str()),
            },
            agent: self.agent,
            parent: self.parent.map(|p| Parent {
                session: SessionKey {
                    source,
                    id: Arc::from(p.as_str()),
                },
                tool_call: None,
            }),
            file: Arc::from(Path::new("/synthetic/session.jsonl")),
            cwd: Arc::from(Path::new(&self.cwd)),
            place: self.place,
            started_at: Timestamp(self.last_activity - MINUTE),
            last_activity: Timestamp(self.last_activity),
            title: None,
            agent_version: None,
            logged_branch: None,
            open_turn: self.open_turn,
            tail_pending: false,
            archived: false,
            inheritance_unknown: false,
            format: Format::Known,
        }
    }
}

/// A `Place::Resolved` in project `key` at `rel` below the root.
pub(crate) fn resolved(key: &ProjectKey, rel: &str) -> Place {
    Place::Resolved(Resolved {
        project: key.clone(),
        worktree: None,
        rel: Arc::from(rel),
    })
}

/// A `Place::Located` in project `key`.
pub(crate) fn located(key: &ProjectKey, rel: &str) -> Place {
    Place::Located(Location {
        project: key.clone(),
        worktree: None,
        rel: Arc::from(rel),
        package: PackageMatch::None,
    })
}

/// A `dyn Review` that answers `overview` from a table the test fills, and
/// counts the calls to `watch_overview` (the screen must never make one).
#[derive(Default)]
pub(crate) struct FakeReview {
    overviews: Mutex<HashMap<ProjectKey, Arc<Overview>>>,
    pub(crate) watched: Mutex<Vec<ProjectKey>>,
}

impl FakeReview {
    /// The overview of `project` from now on: `(session id, needs review)`,
    /// all in the Claude Code source.
    pub(crate) fn set(&self, project: &ProjectKey, sessions: &[(&str, bool)]) {
        let sessions: Vec<SessionOverview> = sessions
            .iter()
            .map(|(id, needs_review)| SessionOverview {
                session: session_key(id),
                progress: Some(if *needs_review {
                    Progress {
                        total: 4,
                        seen: 1,
                        changed_since_seen: 0,
                    }
                } else {
                    Progress {
                        total: 4,
                        seen: 4,
                        changed_since_seen: 0,
                    }
                }),
                drifted: None,
                critical: 0,
            })
            .collect();
        let mut table = self.overviews.lock().unwrap();
        let generation = table.get(project).map_or(0, |o| o.generation) + 1;
        table.insert(
            project.clone(),
            Arc::new(Overview {
                project: project.clone(),
                generation,
                sessions: sessions.into(),
                outside: OutsideCount::Files(0),
                packages: None,
            }),
        );
    }

    /// Nobody watches the overview any more.
    pub(crate) fn clear(&self, project: &ProjectKey) {
        self.overviews.lock().unwrap().remove(project);
    }

    /// The event the review plugin sends after it replaced an overview.
    pub(crate) fn event(&self, project: &ProjectKey) -> ReviewEvent {
        ReviewEvent::Overview {
            project: project.clone(),
            generation: self
                .overviews
                .lock()
                .unwrap()
                .get(project)
                .map_or(0, |o| o.generation),
        }
    }
}

impl Review for FakeReview {
    fn open(&self, _: ReviewTarget) -> ReviewId {
        unreachable!("the welcome screen never opens a review")
    }
    fn snapshot(&self, _: ReviewId) -> Option<Arc<ReviewSnapshot>> {
        unreachable!("the welcome screen never reads a review")
    }
    fn command(&self, _: ReviewId, _: ReviewCommand) {
        unreachable!("the welcome screen never commands a review")
    }
    fn rows(&self, _: ReviewId, _: &RowsKey) -> Option<Arc<FileRows>> {
        unreachable!("the welcome screen never reads rows")
    }
    fn text(&self, _: ReviewId, _: &TextKey) -> Option<Arc<str>> {
        unreachable!("the welcome screen never reads text")
    }
    fn close(&self, _: ReviewId) {
        unreachable!("the welcome screen never closes a review")
    }
    fn overview(&self, project: &ProjectKey) -> Option<Arc<Overview>> {
        self.overviews.lock().unwrap().get(project).cloned()
    }
    fn watch_overview(&self, project: &ProjectKey) -> OverviewInterest {
        self.watched.lock().unwrap().push(project.clone());
        OverviewInterest::new(|| {})
    }
    fn register_brief_rules(
        &self,
        _: Arc<dyn BriefRules>,
        _: ActivationToken,
    ) -> BriefRulesRegistration {
        unreachable!("the welcome screen registers no brief rules")
    }

    fn register_note_route(
        &self,
        _: Arc<dyn NoteRoute>,
        _: ActivationToken,
    ) -> NoteRouteRegistration {
        unreachable!("the welcome screen registers no note routes")
    }
}

/// The key of a synthetic session of the Claude Code source.
pub(crate) fn session_key(id: &str) -> SessionKey {
    SessionKey {
        source: SourceId(Arc::from("session-claude-code")),
        id: Arc::from(id),
    }
}

/// A `dyn Contributions` that remembers what was registered (the payloads
/// too, so a test can build the screen) and the commands it was sent.
#[derive(Default)]
pub(crate) struct FakeContributions {
    pub(crate) registered: Mutex<Vec<(ContributionKind, Name)>>,
    pub(crate) tokens: Mutex<Vec<bool>>,
    payloads: Mutex<Vec<Payload>>,
    pub(crate) dispatched: Mutex<Vec<WorkspaceCommand>>,
}

impl FakeContributions {
    /// The screen's payload, as `workspace` would take it back.
    pub(crate) fn screen(&self) -> Screen {
        self.payloads
            .lock()
            .unwrap()
            .iter()
            .find_map(|payload| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                any.downcast_ref::<Screen>().cloned()
            })
            .expect("the screen is registered")
    }

    /// The key bindings, as `(keystrokes, action name, predicate)`.
    pub(crate) fn bindings(&self) -> Vec<(&'static str, String, Option<&'static str>)> {
        self.payloads
            .lock()
            .unwrap()
            .iter()
            .find_map(|payload| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                any.downcast_ref::<KeyBindings>().map(|bindings| {
                    bindings
                        .0
                        .iter()
                        .map(|(keys, action, context)| (*keys, action.name().to_owned(), *context))
                        .collect()
                })
            })
            .expect("the key bindings are registered")
    }

    /// The key bindings as GPUI loads them.
    pub(crate) fn key_bindings(&self, cx: &gpui::App) -> Vec<gpui::KeyBinding> {
        let payloads = self.payloads.lock().unwrap();
        let bindings = payloads
            .iter()
            .find_map(|payload| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                any.downcast_ref::<KeyBindings>()
            })
            .expect("the key bindings are registered");
        bindings
            .0
            .iter()
            .map(|(keys, action, context)| {
                let predicate = context.map(|context| {
                    std::rc::Rc::new(
                        gpui::KeyBindingContextPredicate::parse(context)
                            .expect("the predicate parses"),
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
                .expect("the keystrokes parse")
            })
            .collect()
    }
}

impl Contributions for FakeContributions {
    fn register(&self, contribution: Contribution, token: Option<ActivationToken>) -> Registration {
        self.tokens.lock().unwrap().push(token.is_some());
        self.registered
            .lock()
            .unwrap()
            .push((contribution.kind, contribution.id));
        self.payloads.lock().unwrap().push(contribution.payload);
        Registration::new(|_| {}, || {})
    }

    fn handle_command(
        &self,
        _: Name,
        _: CommandHandler,
        _: Option<ActivationToken>,
    ) -> Registration {
        Registration::new(|_| {}, || {})
    }

    fn register_leave_guard(
        &self,
        _: Arc<dyn LeaveGuard>,
        _: Option<ActivationToken>,
    ) -> Registration {
        Registration::new(|_| {}, || {})
    }

    fn dispatch(&self, command: WorkspaceCommand) {
        self.dispatched.lock().unwrap().push(command);
    }

    fn sidebar_entries(&self) -> Arc<[SidebarEntry]> {
        Arc::from(Vec::new())
    }

    fn current_route(&self, _window: WindowId) -> Option<Route> {
        None
    }

    fn report_milestone(&self, _: Milestone) {}
}

/// A language preference in memory: what the system prefers, the saved
/// override, and every write it was asked for. The tests never touch the
/// real user defaults.
#[derive(Default)]
pub(crate) struct FakePrefs {
    pub(crate) system: Mutex<Vec<String>>,
    pub(crate) saved: Mutex<Option<String>>,
    /// Every `set_language_override` call, in order: the tag.
    pub(crate) writes: Mutex<Vec<Option<String>>>,
    /// The result handed to `done` (default: `Ok`).
    pub(crate) outcome: Mutex<Option<Result<(), lectern_platform::system::PrefError>>>,
}

impl LanguagePrefs for FakePrefs {
    fn system_languages(&self) -> Vec<String> {
        self.system.lock().unwrap().clone()
    }

    fn language_override(&self) -> Option<String> {
        self.saved.lock().unwrap().clone()
    }

    fn set_language_override(&self, tag: Option<&str>, done: Done) {
        self.writes.lock().unwrap().push(tag.map(str::to_owned));
        *self.saved.lock().unwrap() = tag.map(str::to_owned);
        let outcome = self.outcome.lock().unwrap().unwrap_or(Ok(()));
        done(outcome);
    }
}

/// The three plugins on test kernels, one manual executor, one clock and one
/// data directory.
pub(crate) struct Rig {
    pub(crate) model: Arc<WelcomeModel>,
    pub(crate) executor: Executor,
    pub(crate) projects: Arc<FakeProjects>,
    pub(crate) contributions: Arc<FakeContributions>,
    pub(crate) prefs: Arc<FakePrefs>,
    pub(crate) review: Arc<FakeReview>,
    pub(crate) claude: Arc<FakeSource>,
    pub(crate) codex: Arc<FakeSource>,
    pub(crate) dir: tempfile::TempDir,
    pub(crate) clock: Arc<AtomicI64>,
    screen: TestKernel,
    discovery: Option<TestKernel>,
    discovery_plugin: Option<WelcomePlugin>,
    review_kernel: Option<TestKernel>,
    review_plugin: Option<WelcomePlugin>,
}

impl Rig {
    /// A rig whose app version is "0.1".
    pub(crate) fn new() -> Rig {
        Rig::with_version(Some("0.1"))
    }

    /// A rig with the given app version ("major.minor"), or none.
    pub(crate) fn with_version(version: Option<&'static str>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let executor = Executor::manual();
        let clock = Arc::new(AtomicI64::new(NOW));
        let now = clock.clone();
        let model = WelcomeModel::with_clock(
            executor.clone(),
            dir.path().join("welcome"),
            Arc::new(move || now.load(Ordering::SeqCst)),
        );
        let projects = Arc::new(FakeProjects::default());
        let contributions = Arc::new(FakeContributions::default());
        let claude = FakeSource::new("session-claude-code", Agent::ClaudeCode);
        let codex = FakeSource::new("session-codex", Agent::Codex);
        let prefs = Arc::new(FakePrefs::default());
        let [(_, screen), (_, discovery), (_, review_plugin)] = from_model(
            model.clone(),
            prefs.clone(),
            Some(PathBuf::from("/Users/example")),
            version.map(gpui::SharedString::from),
        );
        let screen = TestKernel::new(screen)
            .mock::<dyn Contributions>(contributions.clone())
            .mock::<dyn Projects>(projects.clone());
        Rig {
            model,
            executor,
            projects,
            contributions,
            prefs,
            review: Arc::new(FakeReview::default()),
            claude,
            codex,
            dir,
            clock,
            screen,
            discovery: None,
            discovery_plugin: Some(discovery),
            review_kernel: None,
            review_plugin: Some(review_plugin),
        }
    }

    /// The data directory the model reads and writes.
    pub(crate) fn data_dir(&self) -> PathBuf {
        self.dir.path().join("welcome")
    }

    /// Puts `bytes` in place as `recent.json`.
    pub(crate) fn seed(&self, bytes: &[u8]) {
        files::seed(&self.data_dir(), bytes);
    }

    /// The file as it is now.
    pub(crate) fn file(&self) -> Option<Vec<u8>> {
        files::contents(&self.data_dir())
    }

    /// Activates "welcome" (its `started` queues the read).
    pub(crate) fn start_screen(&self) {
        self.screen.activate().unwrap();
    }

    /// Activates "welcome-discovery" with both sources.
    pub(crate) fn start_discovery(&mut self) {
        let plugin = self.discovery_plugin.take().expect("started once");
        let kernel = TestKernel::new(plugin)
            .mock::<dyn Projects>(self.projects.clone())
            .mock::<dyn SessionSource>(self.claude.clone())
            .mock::<dyn SessionSource>(self.codex.clone());
        kernel.activate().unwrap();
        self.discovery = Some(kernel);
    }

    /// Deactivates "welcome-discovery".
    pub(crate) fn stop_discovery(&mut self) {
        if let Some(kernel) = self.discovery.take() {
            kernel.deactivate();
        }
    }

    /// Activates "welcome-review" with the fake `dyn Review`.
    pub(crate) fn start_review(&mut self) {
        let plugin = self.review_plugin.take().expect("started once");
        let kernel = TestKernel::new(plugin).mock::<dyn Review>(self.review.clone());
        kernel.activate().unwrap();
        self.review_kernel = Some(kernel);
    }

    /// Deactivates "welcome-review".
    pub(crate) fn stop_review(&mut self) {
        if let Some(kernel) = self.review_kernel.take() {
            kernel.deactivate();
        }
    }

    /// An event for the plugins that are listening.
    pub(crate) fn emit<E: Event + Clone>(&self, event: E) {
        self.screen.inner().unwrap().emitter().emit(event.clone());
        if let Some(discovery) = &self.discovery {
            discovery.inner().unwrap().emitter().emit(event.clone());
        }
        if let Some(review) = &self.review_kernel {
            review.inner().unwrap().emitter().emit(event);
        }
    }

    pub(crate) fn emit_opened(&self, key: &ProjectKey) {
        self.emit(ProjectEvent::Opened {
            request: OpenRequest(1),
            result: Ok(key.clone()),
        });
    }

    pub(crate) fn emit_open_failed(&self, error: OpenError) {
        self.emit(ProjectEvent::Opened {
            request: OpenRequest(1),
            result: Err(error),
        });
    }

    /// Runs the executor until no task is left, letting the kernels deliver
    /// what the tasks emitted in between.
    pub(crate) fn settle(&self) {
        for _ in 0..100 {
            self.screen.inner().unwrap().barrier();
            if let Some(discovery) = &self.discovery {
                discovery.inner().unwrap().barrier();
            }
            if let Some(review) = &self.review_kernel {
                review.inner().unwrap().barrier();
            }
            if self.executor.run_until_idle() == 0 {
                return;
            }
        }
        panic!("never settled");
    }

    pub(crate) fn rebuilds(&self) -> usize {
        self.model.stats.rebuilds.load(Ordering::SeqCst)
    }

    pub(crate) fn writes(&self) -> usize {
        self.model.stats.writes.load(Ordering::SeqCst)
    }

    pub(crate) fn wakes(&self) -> usize {
        self.model.stats.wakes.load(Ordering::SeqCst)
    }

    /// The paths in the recent list now.
    pub(crate) fn recent_paths(&self) -> Vec<PathBuf> {
        match &self.model.snapshot().recent {
            crate::RecentState::Ready(entries) => entries.iter().map(|e| e.path.clone()).collect(),
            crate::RecentState::Loading => Vec::new(),
        }
    }

    /// The paths in the discovered list now.
    pub(crate) fn discovered_paths(&self) -> Vec<PathBuf> {
        match &self.model.snapshot().discovered {
            crate::DiscoveredState::Ready(folders) => {
                folders.iter().map(|f| f.path.to_path_buf()).collect()
            }
            _ => Vec::new(),
        }
    }
}

/// The paths as `PathBuf`s, for comparisons.
pub(crate) fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

pub(crate) const FIXTURE_V1: &[u8] = include_bytes!("../../tests/fixtures/recent_v1.json");
pub(crate) const FIXTURE_BAD_VERSION: &[u8] =
    include_bytes!("../../tests/fixtures/recent_bad_version.json");
pub(crate) const FIXTURE_GARBAGE: &[u8] =
    include_bytes!("../../tests/fixtures/recent_garbage.json");
