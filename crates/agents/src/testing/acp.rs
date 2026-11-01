//! ACP 客户端的测试夹具：假的 `SessionSource` 与 `Projects`、会话和轮次的构造（`test-support`）。
//! 全部是合成数据，不读真实的会话目录。`acp` 模块自己的单元测试和 `tests/acp_*.rs` 共用。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lectern_services::project::{
    Location, OpenRequest, PackageMatch, ProjectKey, ProjectSnapshot, Projects, ResolveError,
    Resolved, ScanError, WorktreeKey,
};
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, Format, Place, ResumeCommand, SessionError,
    SessionKey, SessionList, SessionSource, SessionSummary, SourceId, Timestamp, ToolCall,
    ToolCallId, ToolKind, ToolStatus, Turn, TurnId, TurnIndex, TurnItem,
};

use crate::{claude, codex};

pub const CLAUDE_SESSION: &str = "c1a0de00-1111-4111-8111-0000000000a1";
pub const CODEX_SESSION: &str = "c0de0000-2222-4222-8222-0000000000b2";

pub fn claude_source() -> SourceId {
    SourceId(claude::SOURCE_ID.into())
}

pub fn codex_source() -> SourceId {
    SourceId(codex::SOURCE_ID.into())
}

pub fn key(source: &SourceId, id: &str) -> SessionKey {
    SessionKey {
        source: source.clone(),
        id: id.into(),
    }
}

pub fn folder(root: &str) -> ProjectKey {
    ProjectKey::Folder {
        root: Path::new(root).into(),
    }
}

/// 一个会话在假来源里的描述。
#[derive(Clone)]
pub struct Spec {
    pub parent: bool,
    pub place: Place,
    pub resume: Option<ResumeCommand>,
    pub cwd: PathBuf,
}

impl Spec {
    /// 放在 `project` 里、可以恢复的会话。
    pub fn located(key: &SessionKey, project: ProjectKey, agent: &Agent) -> Spec {
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
pub struct FakeSource {
    id: SourceId,
    agent: Agent,
    state: Mutex<SourceState>,
}

impl FakeSource {
    pub fn new(id: SourceId, agent: Agent) -> Arc<FakeSource> {
        Arc::new(FakeSource {
            id,
            agent,
            state: Mutex::new(SourceState::default()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SourceState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn add(&self, key: &SessionKey, spec: Spec) {
        let mut state = self.state();
        state.specs.insert(key.clone(), spec);
        state.generation += 1;
    }

    pub fn set_spec(&self, key: &SessionKey, edit: impl FnOnce(&mut Spec)) {
        let mut state = self.state();
        edit(state.specs.get_mut(key).expect("known session"));
        state.generation += 1;
    }

    /// 一段文字的引用。
    pub fn text(&self, text: &str) -> ContentRef {
        self.content(Ok(Content::Text(text.into())))
    }

    pub fn content(&self, content: Result<Content, SessionError>) -> ContentRef {
        let mut state = self.state();
        let pos = state.next_pos;
        state.next_pos += 1;
        state.contents.insert(pos, content);
        ContentRef::new(pos, 0, 0)
    }

    pub fn set_turns(&self, key: &SessionKey, turns: Vec<Turn>) {
        let index = TurnIndex {
            session: key.clone(),
            cursor: Cursor::START,
            turns: turns.into(),
            format: Format::Known,
        };
        self.state().turns.insert(key.clone(), Ok(Arc::new(index)));
    }

    pub fn fail_turns(&self, key: &SessionKey, error: SessionError) {
        self.state().turns.insert(key.clone(), Err(error));
    }

    pub fn turns_calls(&self) -> usize {
        self.state().turns_calls
    }

    /// 一轮：prompt 和若干项。
    pub fn turn(&self, ordinal: u32, prompt: &str, items: Vec<TurnItem>) -> Turn {
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

    pub fn agent_text(&self, text: &str) -> TurnItem {
        TurnItem::AgentText(self.text(text))
    }

    pub fn tool(
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
pub struct FakeProjects {
    open: Mutex<Vec<ProjectKey>>,
    located: Mutex<Option<Location>>,
}

impl FakeProjects {
    pub fn set_open(&self, open: Vec<ProjectKey>) {
        *self.open.lock().unwrap() = open;
    }

    /// `locate` 对每个路径给出的位置（默认没有）。
    pub fn set_location(&self, location: Option<Location>) {
        *self.located.lock().unwrap() = location;
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
        self.located.lock().unwrap().clone()
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
