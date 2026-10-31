//! 测试夹具：假的 `SessionSource` 与 `Projects`、会话和轮次的构造、记录事件的引擎。全部是合成数据，
//! 不读真实的会话目录。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lectern_services::conversation::ConversationEvent;
use lectern_services::project::{
    Location, OpenRequest, PackageMatch, ProjectKey, ProjectSnapshot, Projects, ResolveError,
    Resolved, ScanError, WorktreeKey,
};
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, Format, Place, ResumeCommand, SessionError,
    SessionKey, SessionList, SessionSource, SessionSummary, SessionsChanged, SourceId, Timestamp,
    ToolCall, ToolCallId, ToolKind, ToolStatus, Turn, TurnId, TurnIndex, TurnItem,
};

use crate::acp::engine::{Engine, Open, Setup, Sources};
use crate::acp::program::Env;
use crate::{claude, codex};

pub(crate) const CLAUDE_SESSION: &str = "c1a0de00-1111-4111-8111-0000000000a1";
pub(crate) const CODEX_SESSION: &str = "c0de0000-2222-4222-8222-0000000000b2";

pub(crate) fn claude_source() -> SourceId {
    SourceId(claude::SOURCE_ID.into())
}

pub(crate) fn codex_source() -> SourceId {
    SourceId(codex::SOURCE_ID.into())
}

pub(crate) fn key(source: &SourceId, id: &str) -> SessionKey {
    SessionKey {
        source: source.clone(),
        id: id.into(),
    }
}

pub(crate) fn folder(root: &str) -> ProjectKey {
    ProjectKey::Folder {
        root: Path::new(root).into(),
    }
}

/// 一个会话在假来源里的描述。
#[derive(Clone)]
pub(crate) struct Spec {
    pub(crate) parent: bool,
    pub(crate) place: Place,
    pub(crate) resume: Option<ResumeCommand>,
    pub(crate) cwd: PathBuf,
}

impl Spec {
    /// 放在 `project` 里、可以恢复的会话。
    pub(crate) fn located(key: &SessionKey, project: ProjectKey, agent: &Agent) -> Spec {
        let resume = match agent {
            Agent::ClaudeCode => ResumeCommand::ClaudeCode {
                session_id: key.id.clone(),
            },
            _ => ResumeCommand::Codex {
                session_id: key.id.clone(),
            },
        };
        Spec {
            parent: false,
            place: Place::Located(Location {
                project,
                worktree: None::<WorktreeKey>,
                rel: "".into(),
                package: PackageMatch::NotLoaded,
            }),
            resume: Some(resume),
            cwd: PathBuf::from("/work/project"),
        }
    }
}

#[derive(Default)]
struct SourceState {
    specs: HashMap<SessionKey, Spec>,
    turns: HashMap<SessionKey, Result<Arc<TurnIndex>, SessionError>>,
    contents: HashMap<u64, Result<Content, SessionError>>,
    next_pos: u64,
    generation: u64,
    turns_calls: usize,
}

/// 假的 `SessionSource`：会话列表、轮次索引和正文都由测试设置。
pub(crate) struct FakeSource {
    id: SourceId,
    agent: Agent,
    state: Mutex<SourceState>,
}

impl FakeSource {
    pub(crate) fn new(id: SourceId, agent: Agent) -> Arc<FakeSource> {
        Arc::new(FakeSource {
            id,
            agent,
            state: Mutex::new(SourceState::default()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SourceState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn add(&self, key: &SessionKey, spec: Spec) {
        let mut state = self.state();
        state.specs.insert(key.clone(), spec);
        state.generation += 1;
    }

    pub(crate) fn set_spec(&self, key: &SessionKey, edit: impl FnOnce(&mut Spec)) {
        let mut state = self.state();
        edit(state.specs.get_mut(key).expect("known session"));
        state.generation += 1;
    }

    /// 一段文字的引用。
    pub(crate) fn text(&self, text: &str) -> ContentRef {
        self.content(Ok(Content::Text(text.into())))
    }

    pub(crate) fn content(&self, content: Result<Content, SessionError>) -> ContentRef {
        let mut state = self.state();
        let pos = state.next_pos;
        state.next_pos += 1;
        state.contents.insert(pos, content);
        ContentRef::new(pos, 0, 0)
    }

    pub(crate) fn set_turns(&self, key: &SessionKey, turns: Vec<Turn>) {
        let index = TurnIndex {
            session: key.clone(),
            cursor: Cursor::START,
            turns: turns.into(),
            format: Format::Known,
        };
        self.state().turns.insert(key.clone(), Ok(Arc::new(index)));
    }

    pub(crate) fn fail_turns(&self, key: &SessionKey, error: SessionError) {
        self.state().turns.insert(key.clone(), Err(error));
    }

    pub(crate) fn turns_calls(&self) -> usize {
        self.state().turns_calls
    }

    /// 一轮：prompt 和若干项。
    pub(crate) fn turn(&self, ordinal: u32, prompt: &str, items: Vec<TurnItem>) -> Turn {
        Turn {
            id: TurnId(format!("turn-{ordinal}").into()),
            ordinal,
            started_at: Timestamp(i64::from(ordinal) * 1000),
            ended_at: Some(Timestamp(i64::from(ordinal) * 1000 + 500)),
            prompt: Some(self.text(prompt)),
            prompt_preview: prompt.chars().take(120).collect::<String>().into(),
            items: items.into(),
        }
    }

    pub(crate) fn agent_text(&self, text: &str) -> TurnItem {
        TurnItem::AgentText(self.text(text))
    }

    pub(crate) fn tool(
        &self,
        id: &str,
        name: &str,
        kind: ToolKind,
        finished: Option<ToolStatus>,
    ) -> TurnItem {
        TurnItem::Tool(ToolCall {
            id: ToolCallId(id.into()),
            name: name.into(),
            kind,
            started_at: Timestamp(1),
            finished: finished.map(|status| (Timestamp(2), status)),
            input: self.text("{}"),
            result: None,
        })
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
        let state = self.state();
        let sessions: Vec<SessionSummary> = state
            .specs
            .iter()
            .map(|(key, spec)| SessionSummary {
                key: key.clone(),
                agent: self.agent.clone(),
                parent: spec.parent.then(|| lectern_services::session::Parent {
                    session: key.clone(),
                    tool_call: None,
                }),
                file: Path::new("/sessions/file.jsonl").into(),
                cwd: spec.cwd.as_path().into(),
                place: spec.place.clone(),
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
            })
            .collect();
        Arc::new(SessionList {
            generation: state.generation,
            sessions: sessions.into(),
            ..SessionList::default()
        })
    }

    fn read(&self, _: &SessionKey, _: &Cursor, _: u64) -> Result<Batch, SessionError> {
        Err(SessionError::NotFound)
    }

    fn turns(&self, session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        let mut state = self.state();
        state.turns_calls += 1;
        state
            .turns
            .get(session)
            .cloned()
            .unwrap_or(Err(SessionError::NotFound))
    }

    fn turns_unretained(&self, session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        self.turns(session)
    }

    fn load(&self, _: &SessionKey, at: &ContentRef) -> Result<Content, SessionError> {
        self.state()
            .contents
            .get(&at.pos())
            .cloned()
            .unwrap_or(Err(SessionError::NotFound))
    }

    fn resume(&self, session: &SessionKey) -> Option<ResumeCommand> {
        self.state()
            .specs
            .get(session)
            .and_then(|spec| spec.resume.clone())
    }

    fn complete(&self, _: &[SessionKey]) {}
}

/// 假的 `Projects`：只有已打开的项目有意义。
#[derive(Default)]
pub(crate) struct FakeProjects {
    open: Mutex<Vec<ProjectKey>>,
}

impl FakeProjects {
    pub(crate) fn set_open(&self, open: Vec<ProjectKey>) {
        *self.open.lock().unwrap() = open;
    }
}

impl Projects for FakeProjects {
    fn open(&self, _: &Path) -> OpenRequest {
        OpenRequest(0)
    }

    fn close(&self, project: &ProjectKey) {
        self.open.lock().unwrap().retain(|p| p != project);
    }

    fn set_active_worktree(&self, _: &ProjectKey, _: &WorktreeKey) {}

    fn snapshot(&self, _: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        None
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        self.open.lock().unwrap().clone()
    }

    fn locate(&self, _: &Path) -> Option<Location> {
        None
    }

    fn resolve(&self, _: &Path) -> Result<Resolved, ResolveError> {
        Err(lectern_services::project::OpenError::NotFound)
    }

    fn resolve_package(
        &self,
        _: &ProjectKey,
        _: &WorktreeKey,
        _: &str,
    ) -> Result<PackageMatch, ScanError> {
        Ok(PackageMatch::None)
    }
}

/// 一个引擎和它的假依赖，同步运行（没有 executor），事件记在 `events` 里。
pub(crate) struct Rig {
    pub(crate) engine: Arc<Engine>,
    pub(crate) claude: Arc<FakeSource>,
    pub(crate) codex: Arc<FakeSource>,
    pub(crate) projects: Arc<FakeProjects>,
    pub(crate) events: Arc<Mutex<Vec<ConversationEvent>>>,
}

impl Rig {
    /// `path` 是查找 ACP 程序用的 `PATH`。
    pub(crate) fn new(path: &Path) -> Rig {
        Rig::with_vars(vec![("PATH".into(), path.into())])
    }

    /// 查找用的环境变量由测试给出。
    pub(crate) fn with_vars(vars: Vec<(std::ffi::OsString, std::ffi::OsString)>) -> Rig {
        let claude = FakeSource::new(claude_source(), Agent::ClaudeCode);
        let codex = FakeSource::new(codex_source(), Agent::Codex);
        let projects = Arc::new(FakeProjects::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = events.clone();
        let engine = Engine::new(Setup {
            executor: None,
            sources: Sources::Fixed(vec![claude.clone(), codex.clone()]),
            projects: Open::Fixed(projects.clone()),
            env: Env::Fixed(vars.into()),
            sink: Some(Arc::new(move |event| {
                sink_events.lock().unwrap().push(event);
            })),
        });
        Rig {
            engine,
            claude,
            codex,
            projects,
            events,
        }
    }

    /// 一个放在已打开项目里、可以恢复的 Claude Code 会话，带两轮。
    pub(crate) fn claude_session(&self) -> SessionKey {
        let session = key(&claude_source(), CLAUDE_SESSION);
        let project = folder("/work/project");
        self.projects.set_open(vec![project.clone()]);
        self.claude.add(
            &session,
            Spec::located(&session, project, &Agent::ClaudeCode),
        );
        let turns = vec![
            self.claude.turn(1, "first prompt", vec![]),
            self.claude
                .turn(2, "second prompt", vec![self.claude.agent_text("a reply")]),
        ];
        self.claude.set_turns(&session, turns);
        session
    }

    pub(crate) fn take_events(&self) -> Vec<ConversationEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

/// 另一组来源的引擎（同步运行，没有事件记录）。
pub(crate) fn engine_with(
    sources: Vec<Arc<FakeSource>>,
    projects: Arc<FakeProjects>,
    path: &Path,
) -> Arc<Engine> {
    Engine::new(Setup {
        executor: None,
        sources: Sources::Fixed(
            sources
                .into_iter()
                .map(|s| s as Arc<dyn SessionSource>)
                .collect(),
        ),
        projects: Open::Fixed(projects),
        env: Env::Fixed(vec![("PATH".into(), path.into())].into()),
        sink: None,
    })
}

/// 在 `dir` 里放一个可执行文件。
pub(crate) fn install(dir: &Path, name: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// 来源 `source` 报告 `key` 变了。
pub(crate) fn changed(source: &SourceId, key: &SessionKey) -> SessionsChanged {
    SessionsChanged {
        source: source.clone(),
        generation: 1,
        changed: vec![key.clone()].into(),
        removed: Arc::from([]),
    }
}
