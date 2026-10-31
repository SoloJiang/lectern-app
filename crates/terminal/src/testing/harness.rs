//! The plugin in a `TestKernel` with fake `Projects` and `SessionSource`
//! providers, for the tests and `lectern-bench` (no real project, no real
//! agent sessions).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lectern_kernel::ActivationToken;
use lectern_kernel::testing::TestKernel;
use lectern_services::project::{
    Location, OpenError, OpenRequest, PackageMatch, ProjectEvent, ProjectKey, ProjectSnapshot,
    Projects, ResolveError, Resolved, ScanError, WorktreeKey,
};
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, Diagnostics, Format, Place, ResumeCommand,
    SessionError, SessionKey, SessionList, SessionSource, SessionSummary, SourceId, Timestamp,
    TurnIndex,
};
use lectern_services::terminal::{
    Frame, GridSize, Launch, LaunchError, OpenSpec, RunState, TerminalEvent, TerminalId,
    TerminalInfo, Terminals,
};

use crate::{TerminalDeps, TerminalPlugin};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Open folder projects by their root, which `locate` matches by prefix.
#[derive(Default)]
pub struct FakeProjects {
    roots: Mutex<Vec<(ProjectKey, PathBuf)>>,
}

impl FakeProjects {
    /// Opens `root` as a folder project. The root is canonicalized, as
    /// `project` does: the terminal canonicalizes its working directory
    /// before asking `locate`.
    pub fn add(&self, root: &Path) -> ProjectKey {
        let root = std::fs::canonicalize(root).expect("project root exists");
        let key = ProjectKey::Folder {
            root: Arc::from(root.as_path()),
        };
        lock(&self.roots).push((key.clone(), root));
        key
    }

    /// Closes a project in the fake. The event is `closed`.
    pub fn remove(&self, key: &ProjectKey) {
        lock(&self.roots).retain(|(k, _)| k != key);
    }

    pub fn closed(key: &ProjectKey) -> ProjectEvent {
        ProjectEvent::Closed {
            project: key.clone(),
        }
    }
}

impl Projects for FakeProjects {
    fn open(&self, _: &Path) -> OpenRequest {
        OpenRequest(0)
    }

    fn close(&self, project: &ProjectKey) {
        self.remove(project);
    }

    fn set_active_worktree(&self, _: &ProjectKey, _: &WorktreeKey) {}

    fn snapshot(&self, _: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        None
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        lock(&self.roots).iter().map(|(k, _)| k.clone()).collect()
    }

    fn locate(&self, path: &Path) -> Option<Location> {
        lock(&self.roots)
            .iter()
            .filter(|(_, root)| path.starts_with(root))
            .max_by_key(|(_, root)| root.components().count())
            .map(|(project, _)| Location {
                project: project.clone(),
                worktree: None,
                rel: Arc::from(""),
                package: PackageMatch::None,
            })
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

/// One session source with a list and the resume commands it gives.
pub struct FakeSessions {
    id: SourceId,
    agent: Agent,
    inner: Mutex<SessionsInner>,
}

#[derive(Default)]
struct SessionsInner {
    list: Vec<SessionSummary>,
    resume: HashMap<SessionKey, ResumeCommand>,
    generation: u64,
}

impl FakeSessions {
    pub fn new(id: &str, agent: Agent) -> Arc<FakeSessions> {
        Arc::new(FakeSessions {
            id: SourceId(Arc::from(id)),
            agent,
            inner: Mutex::default(),
        })
    }

    pub fn key(&self, session: &str) -> SessionKey {
        SessionKey {
            source: self.id.clone(),
            id: Arc::from(session),
        }
    }

    /// Lists a session with this `cwd` and `place`. `resume` is what the
    /// source gives for it (None: the source gives nothing).
    pub fn add(
        &self,
        session: &str,
        cwd: &Path,
        place: Place,
        resume: Option<ResumeCommand>,
    ) -> SessionKey {
        let key = self.key(session);
        let mut inner = lock(&self.inner);
        inner.list.push(SessionSummary {
            key: key.clone(),
            agent: self.agent.clone(),
            parent: None,
            file: Arc::from(Path::new("/nonexistent/session.jsonl")),
            cwd: Arc::from(cwd),
            place,
            started_at: Timestamp(0),
            last_activity: Timestamp(0),
            title: None,
            agent_version: None,
            logged_branch: None,
            open_turn: false,
            tail_pending: false,
            archived: false,
            inheritance_unknown: false,
            format: Format::Known,
        });
        if let Some(command) = resume {
            inner.resume.insert(key.clone(), command);
        }
        inner.generation += 1;
        key
    }
}

impl SessionSource for FakeSessions {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn agent(&self) -> Agent {
        self.agent.clone()
    }

    fn sessions(&self) -> Arc<SessionList> {
        let inner = lock(&self.inner);
        Arc::new(SessionList {
            generation: inner.generation,
            sessions: Arc::from(inner.list.clone()),
            diagnostics: Diagnostics::default(),
        })
    }

    fn read(&self, _: &SessionKey, _: &Cursor, _: u64) -> Result<Batch, SessionError> {
        Err(SessionError::NotFound)
    }

    fn turns(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        Err(SessionError::NotFound)
    }

    fn turns_unretained(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        Err(SessionError::NotFound)
    }

    fn load(&self, _: &SessionKey, _: &ContentRef) -> Result<Content, SessionError> {
        Err(SessionError::NotFound)
    }

    fn resume(&self, session: &SessionKey) -> Option<ResumeCommand> {
        lock(&self.inner).resume.get(session).cloned()
    }

    fn complete(&self, _: &[SessionKey]) {}
}

/// The plugin under test with its fakes.
pub struct Harness {
    pub kernel: TestKernel,
    pub terminals: Arc<dyn Terminals>,
    /// The plugin's own activation token, for `copy_selection`.
    pub token: ActivationToken,
    /// The terminal threads running now.
    pub threads: Arc<AtomicUsize>,
    pub projects: Arc<FakeProjects>,
    /// "session-claude-code"
    pub claude: Arc<FakeSessions>,
    /// "session-codex"
    pub codex: Arc<FakeSessions>,
}

impl Harness {
    /// Activates `plugin` with the fakes. `plugin` is built from the
    /// `executor` by the caller so it can carry the overrides.
    pub fn start(plugin: TerminalPlugin) -> Harness {
        let provided = plugin.provided();
        let token = plugin.token();
        let threads = plugin.threads();
        let projects = Arc::new(FakeProjects::default());
        let claude = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
        let codex = FakeSessions::new("session-codex", Agent::Codex);
        let kernel = TestKernel::new(plugin)
            .mock::<dyn Projects>(projects.clone())
            .mock::<dyn SessionSource>(claude.clone())
            .mock::<dyn SessionSource>(codex.clone());
        kernel.activate().expect("the terminal plugin activates");
        let terminals = lock(&provided)
            .clone()
            .expect("the plugin provided its service");
        let token = lock(&token).clone().expect("the plugin was started");
        Harness {
            kernel,
            terminals,
            token,
            threads,
            projects,
            claude,
            codex,
        }
    }

    /// A plugin with a real executor, for `Harness::start`.
    pub fn plugin() -> TerminalPlugin {
        TerminalPlugin::new(TerminalDeps {
            executor: lectern_executor::Executor::new(lectern_executor::Config::default()),
        })
    }

    /// Opens a terminal.
    pub fn open(&self, launch: Launch, size: GridSize) -> Result<TerminalId, LaunchError> {
        self.terminals.open(OpenSpec {
            launch,
            size: Some(size),
            theme: super::theme(),
        })
    }

    /// The events the plugin emitted, in order.
    pub fn events(&self) -> Vec<TerminalEvent> {
        self.kernel.emitted::<TerminalEvent>()
    }

    /// Emits `ProjectEvent::Closed` as `project` would.
    pub fn close_project(&self, key: &ProjectKey) {
        self.projects.remove(key);
        if let Some(kernel) = self.kernel.inner() {
            kernel.emitter().emit(FakeProjects::closed(key));
        }
    }

    /// Polls `check` every millisecond until it gives a value or `timeout`
    /// passes.
    pub fn wait_for<T>(
        &self,
        timeout: Duration,
        mut check: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = check() {
                return Some(value);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Waits until the terminal's state satisfies `check`; panics after
    /// 20 seconds.
    pub fn wait_state(&self, id: TerminalId, check: impl Fn(&RunState) -> bool) -> TerminalInfo {
        self.wait_for(Duration::from_secs(20), || {
            self.terminals.info(id).filter(|info| check(&info.state))
        })
        .unwrap_or_else(|| {
            panic!(
                "terminal {id:?} did not reach the state; it is {:?}",
                self.terminals.info(id).map(|info| info.state)
            )
        })
    }

    /// Waits until the latest frame's text satisfies `check`; panics after
    /// 20 seconds. Needs a `FrameInterest` held by the caller.
    pub fn wait_frame(&self, id: TerminalId, check: impl Fn(&str) -> bool) -> Arc<Frame> {
        self.wait_for(Duration::from_secs(20), || {
            self.terminals
                .frame(id)
                .filter(|frame| check(&frame_text(frame)))
        })
        .unwrap_or_else(|| {
            panic!(
                "terminal {id:?} did not show the text; it shows {:?}",
                self.terminals.frame(id).map(|f| frame_text(&f))
            )
        })
    }
}

/// The text of a frame's rows, one line per row, trailing blank lines cut.
pub fn frame_text(frame: &Frame) -> String {
    let mut lines: Vec<&str> = frame.rows.iter().map(|row| &*row.text).collect();
    while lines.last().is_some_and(|line| line.trim_end().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}
