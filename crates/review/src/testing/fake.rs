//! `FakeSessions` and `FakeProjects` (docs/design/review.md「测试」): a
//! session source built from a script and a projects service with one
//! repository project, for the engine's tests and benches. No `agents`, no
//! `project` (plugins never depend on each other, dev-dependencies
//! included).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;

use lectern_services::project::{
    ActiveTree, Head, Location, OpenError, OpenRequest, PackageMatch, ProjectKey, ProjectSnapshot,
    Projects, RelPath, ResolveError, Resolved, Scan, ScanError, Worktree, WorktreeKey,
};
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, Diagnostics, EditOp, FileEdit, Format, Parent,
    Place, ResumeCommand, SessionError, SessionKey, SessionList, SessionSource, SessionSummary,
    SessionsChanged, SourceId, Timestamp, ToolCall, ToolCallId, ToolKind, ToolStatus, Turn, TurnId,
    TurnIndex, TurnItem,
};

/// The script of a source: its sessions (docs/design/review.md「测试」).
///
/// ```text
/// { "agent": "claude-code" | "codex", "adapter": 1,
///   "sessions": [ { "id", "cwd" (absolute), "started_at" (ms),
///     "last_activity"?, "parent"?, "spawned_by"?, "unrecognized"?,
///     "tail_pending"?, "turns": [turn…] } ] }
/// turn: { "id", "at", "end"?, "prompt"?, "items": [item…] }
/// item: { "text": "…" } | { "injected": "…" }
///     | { "edit": "<call>", "at", "done"?, "status"?, "name"?,
///         "changes": [change…], "result_text"? }
///     | { "run": "<call>", "command", "at", "done"? }
///     | { "spawn": "<call>", "at", "done"? }
///     | { "read": "<call>", "paths": […], "at", "done"? }
///     | { "tool": "<call>", "name", "at", "done"? }
/// change: { "path", "write" | "add" | "replace": [old, new] (+ "all")
///          | "patch" (+ "move_to") | "delete": true }
/// ```
///
/// Paths are relative to the session's cwd unless absolute. Without
/// `done` a call has no result. `result_text` makes the result a text, so
/// the edits come from the input.
#[derive(Deserialize)]
struct Script {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default = "one")]
    adapter: u32,
    sessions: Vec<ScriptSession>,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize)]
struct ScriptSession {
    id: String,
    cwd: String,
    started_at: i64,
    #[serde(default)]
    last_activity: Option<i64>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    spawned_by: Option<String>,
    #[serde(default)]
    unrecognized: bool,
    #[serde(default)]
    tail_pending: bool,
    #[serde(default)]
    turns: Vec<ScriptTurn>,
}

#[derive(Deserialize)]
struct ScriptTurn {
    id: String,
    at: i64,
    #[serde(default)]
    end: Option<i64>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    items: Vec<ScriptItem>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScriptItem {
    text: Option<String>,
    injected: Option<String>,
    edit: Option<String>,
    run: Option<String>,
    spawn: Option<String>,
    read: Option<String>,
    tool: Option<String>,
    name: Option<String>,
    command: Option<String>,
    paths: Vec<String>,
    at: i64,
    done: Option<i64>,
    status: Option<String>,
    changes: Vec<ScriptChange>,
    result_text: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScriptChange {
    path: String,
    write: Option<String>,
    add: Option<String>,
    replace: Option<(String, String)>,
    all: bool,
    patch: Option<String>,
    move_to: Option<String>,
    delete: bool,
}

/// What `FakeSessions` was asked, per session ID.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Calls {
    pub turns: usize,
    pub turns_unretained: usize,
    pub load: usize,
}

struct FakeSession {
    summary: SessionSummary,
    turns: Vec<Turn>,
    contents: Vec<Content>,
    unrecognized: bool,
}

struct Inner {
    generation: u64,
    adapter: u32,
    sessions: Vec<FakeSession>,
    calls: HashMap<Arc<str>, Calls>,
    events: Vec<SessionsChanged>,
}

/// A `SessionSource` from a script; counts its calls.
pub struct FakeSessions {
    source: SourceId,
    agent: Agent,
    inner: Mutex<Inner>,
}

fn status(s: Option<&str>) -> ToolStatus {
    match s {
        Some("failed") => ToolStatus::Failed,
        Some("denied") => ToolStatus::Denied,
        Some("interrupted") => ToolStatus::Interrupted,
        _ => ToolStatus::Completed,
    }
}

fn abs(cwd: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

impl FakeSession {
    fn content(&mut self, content: Content) -> ContentRef {
        let n = self.contents.len() as u64;
        self.contents.push(content);
        ContentRef::new(1000 + n * 100, 100, 0)
    }

    fn add_turn(&mut self, t: ScriptTurn) {
        let cwd = self.summary.cwd.to_path_buf();
        let prompt = t
            .prompt
            .as_ref()
            .map(|p| self.content(Content::Text(Arc::from(p.as_str()))));
        let mut items = Vec::new();
        let mut last = t.end.unwrap_or(t.at);
        for item in t.items {
            last = last.max(item.done.unwrap_or(item.at));
            if let Some(text) = item.text {
                items.push(TurnItem::AgentText(
                    self.content(Content::Text(text.into())),
                ));
                continue;
            }
            if let Some(text) = item.injected {
                items.push(TurnItem::Injected(self.content(Content::Text(text.into()))));
                continue;
            }
            let finished = item
                .done
                .map(|d| (Timestamp(d), status(item.status.as_deref())));
            let (id, name, kind, input, result) = if let Some(id) = item.edit {
                let edits: Vec<FileEdit> = item
                    .changes
                    .iter()
                    .map(|c| FileEdit {
                        path: abs(&cwd, &c.path),
                        op: if let Some(content) = &c.write {
                            EditOp::Write {
                                content: content.as_str().into(),
                                created: false,
                            }
                        } else if let Some(content) = &c.add {
                            EditOp::Add {
                                content: content.as_str().into(),
                            }
                        } else if let Some((old, new)) = &c.replace {
                            EditOp::Replace {
                                old: old.as_str().into(),
                                new: new.as_str().into(),
                                all: c.all,
                            }
                        } else if let Some(hunks) = &c.patch {
                            EditOp::Patch {
                                hunks: hunks.as_str().into(),
                                move_to: c.move_to.as_ref().map(|m| abs(&cwd, m)),
                            }
                        } else {
                            EditOp::Delete
                        },
                    })
                    .collect();
                let mut paths: Vec<PathBuf> = Vec::new();
                for e in &edits {
                    if !paths.contains(&e.path) {
                        paths.push(e.path.clone());
                    }
                    if let EditOp::Patch {
                        move_to: Some(to), ..
                    } = &e.op
                        && !paths.contains(to)
                    {
                        paths.push(to.clone());
                    }
                }
                let edits: Arc<[FileEdit]> = edits.into();
                let input = self.content(Content::Edits(Arc::clone(&edits)));
                let result = finished.map(|_| {
                    if item.result_text {
                        self.content(Content::Text("ok".into()))
                    } else {
                        self.content(Content::Edits(edits))
                    }
                });
                (
                    id,
                    item.name.unwrap_or_else(|| "Edit".into()),
                    ToolKind::Edit {
                        paths: paths.into(),
                    },
                    input,
                    result,
                )
            } else if let Some(id) = item.run {
                let command = item.command.unwrap_or_default();
                let input = self.content(Content::Text(command.as_str().into()));
                let result = finished.map(|_| self.content(Content::Text("".into())));
                (
                    id,
                    item.name.unwrap_or_else(|| "Bash".into()),
                    ToolKind::Execute {
                        command: command.into(),
                        workdir: Arc::from(cwd.as_path()),
                        exec_session: None,
                        workdir_exact: true,
                    },
                    input,
                    result,
                )
            } else if let Some(id) = item.spawn {
                let input = self.content(Content::Text("task".into()));
                (
                    id,
                    item.name.unwrap_or_else(|| "Task".into()),
                    ToolKind::SpawnAgent,
                    input,
                    None,
                )
            } else if let Some(id) = item.read {
                let input = self.content(Content::Text("read".into()));
                (
                    id,
                    item.name.unwrap_or_else(|| "Read".into()),
                    ToolKind::Read {
                        paths: item.paths.iter().map(|p| abs(&cwd, p)).collect(),
                    },
                    input,
                    None,
                )
            } else if let Some(id) = item.tool {
                let input = self.content(Content::Text("tool".into()));
                (
                    id,
                    item.name.unwrap_or_else(|| "Other".into()),
                    ToolKind::Other,
                    input,
                    None,
                )
            } else {
                continue;
            };
            items.push(TurnItem::Tool(ToolCall {
                id: ToolCallId(id.into()),
                name: name.into(),
                kind,
                started_at: Timestamp(item.at),
                finished,
                input,
                result,
            }));
        }
        let ordinal = self.turns.len() as u32 + 1;
        let preview: Arc<str> = t.prompt.as_deref().unwrap_or("").into();
        if self.summary.title.is_none() && !preview.is_empty() {
            self.summary.title = Some(Arc::clone(&preview));
        }
        self.turns.push(Turn {
            id: TurnId(t.id.into()),
            ordinal,
            started_at: Timestamp(t.at),
            ended_at: t.end.map(Timestamp),
            prompt,
            prompt_preview: preview,
            items: items.into(),
        });
        self.summary.last_activity = Timestamp(self.summary.last_activity.0.max(last));
    }
}

impl FakeSessions {
    /// The sessions of `script` from the source `source` (its ID, as
    /// "session-claude-code"). Panics on a bad script: test code.
    pub fn from_script(source: &str, script: &str) -> FakeSessions {
        let script: Script = serde_json::from_str(script).expect("session script");
        let source = SourceId(source.into());
        let agent = match script.agent.as_deref() {
            Some("codex") => Agent::Codex,
            _ => Agent::ClaudeCode,
        };
        let mut sessions = Vec::new();
        for s in script.sessions {
            let key = SessionKey {
                source: source.clone(),
                id: s.id.as_str().into(),
            };
            let parent = s.parent.map(|p| Parent {
                session: SessionKey {
                    source: source.clone(),
                    id: p.as_str().into(),
                },
                tool_call: s.spawned_by.map(|c| ToolCallId(c.into())),
            });
            let mut session = FakeSession {
                summary: SessionSummary {
                    key,
                    agent: agent.clone(),
                    parent,
                    file: Arc::from(Path::new(&format!("/fake/{}.jsonl", s.id))),
                    cwd: Arc::from(Path::new(&s.cwd)),
                    place: Place::Pending,
                    started_at: Timestamp(s.started_at),
                    last_activity: Timestamp(s.started_at),
                    title: None,
                    agent_version: None,
                    logged_branch: None,
                    open_turn: false,
                    tail_pending: s.tail_pending,
                    archived: false,
                    inheritance_unknown: false,
                    format: if s.unrecognized {
                        Format::Unrecognized
                    } else {
                        Format::Known
                    },
                },
                turns: Vec::new(),
                contents: Vec::new(),
                unrecognized: s.unrecognized,
            };
            for t in s.turns {
                session.add_turn(t);
            }
            if let Some(at) = s.last_activity {
                session.summary.last_activity = Timestamp(at);
            }
            sessions.push(session);
        }
        FakeSessions {
            source,
            agent,
            inner: Mutex::new(Inner {
                generation: 1,
                adapter: script.adapter,
                sessions,
                calls: HashMap::new(),
                events: Vec::new(),
            }),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Places every session at its cwd among `projects` (`locate`), as
    /// `agents` does once the project is open.
    pub fn locate(&self, projects: &dyn Projects) {
        for s in self.inner().sessions.iter_mut() {
            s.summary.place = match projects.locate(&s.summary.cwd) {
                Some(l) => Place::Located(l),
                None => Place::Pending,
            };
        }
    }

    /// Appends turns (a JSON turn, or an array of them) to `session` and
    /// records a `SessionsChanged` (`events`).
    pub fn append(&self, session: &str, script: &str) {
        let turns: Vec<ScriptTurn> = match serde_json::from_str::<Vec<ScriptTurn>>(script) {
            Ok(turns) => turns,
            Err(_) => vec![serde_json::from_str(script).expect("turn script")],
        };
        let mut inner = self.inner();
        inner.generation += 1;
        let generation = inner.generation;
        let s = inner
            .sessions
            .iter_mut()
            .find(|s| &*s.summary.key.id == session)
            .expect("session in the script");
        for t in turns {
            s.add_turn(t);
        }
        let key = s.summary.key.clone();
        inner.events.push(SessionsChanged {
            source: self.source.clone(),
            generation,
            changed: vec![key].into(),
            removed: Arc::from(Vec::new()),
        });
    }

    /// Sets a session's `last_activity` (a new record the index does not
    /// show).
    pub fn touch(&self, session: &str, at: Timestamp) {
        let mut inner = self.inner();
        inner.generation += 1;
        if let Some(s) = inner
            .sessions
            .iter_mut()
            .find(|s| &*s.summary.key.id == session)
        {
            s.summary.last_activity = at;
        }
    }

    /// The adapter version the indexes' cursors carry from now on.
    pub fn set_adapter_version(&self, version: u32) {
        self.inner().adapter = version;
    }

    pub fn calls(&self, session: &str) -> Calls {
        self.inner().calls.get(session).copied().unwrap_or_default()
    }

    pub fn reset_calls(&self) {
        self.inner().calls.clear();
    }

    pub fn take_events(&self) -> Vec<SessionsChanged> {
        std::mem::take(&mut self.inner().events)
    }

    fn index(
        &self,
        key: &SessionKey,
        count: impl FnOnce(&mut Calls),
    ) -> Result<Arc<TurnIndex>, SessionError> {
        let mut inner = self.inner();
        count(inner.calls.entry(key.id.clone()).or_default());
        let adapter = inner.adapter;
        let s = inner
            .sessions
            .iter()
            .find(|s| s.summary.key == *key)
            .ok_or(SessionError::NotFound)?;
        if s.unrecognized {
            return Err(SessionError::Unrecognized);
        }
        Ok(Arc::new(TurnIndex {
            session: key.clone(),
            cursor: Cursor::new(
                self.source.clone(),
                adapter,
                s.contents.len() as u64,
                Vec::new(),
            ),
            turns: s.turns.clone().into(),
            format: Format::Known,
        }))
    }
}

impl SessionSource for FakeSessions {
    fn id(&self) -> SourceId {
        self.source.clone()
    }

    fn agent(&self) -> Agent {
        self.agent.clone()
    }

    fn sessions(&self) -> Arc<SessionList> {
        let inner = self.inner();
        let mut sessions: Vec<SessionSummary> =
            inner.sessions.iter().map(|s| s.summary.clone()).collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity));
        Arc::new(SessionList {
            generation: inner.generation,
            sessions: sessions.into(),
            diagnostics: Diagnostics::default(),
        })
    }

    fn read(&self, _: &SessionKey, _: &Cursor, _: u64) -> Result<Batch, SessionError> {
        Ok(Batch {
            reset: false,
            records: Vec::new(),
            cursor: Cursor::START,
            at_end: true,
        })
    }

    fn turns(&self, session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        self.index(session, |c| c.turns += 1)
    }

    fn turns_unretained(&self, session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        self.index(session, |c| c.turns_unretained += 1)
    }

    fn load(&self, session: &SessionKey, at: &ContentRef) -> Result<Content, SessionError> {
        let mut inner = self.inner();
        inner.calls.entry(session.id.clone()).or_default().load += 1;
        let s = inner
            .sessions
            .iter()
            .find(|s| s.summary.key == *session)
            .ok_or(SessionError::NotFound)?;
        let n = at
            .pos()
            .checked_sub(1000)
            .map(|p| p / 100)
            .ok_or(SessionError::NotFound)?;
        s.contents
            .get(n as usize)
            .cloned()
            .ok_or_else(|| SessionError::Io("no such content".into()))
    }

    fn resume(&self, _: &SessionKey) -> Option<ResumeCommand> {
        None
    }

    fn complete(&self, _: &[SessionKey]) {}
}

/// A `Projects` with one repository project: its worktrees, `locate` by
/// prefix, optional sub-projects. No IO.
pub struct FakeProjects {
    key: ProjectKey,
    worktrees: Mutex<Vec<Worktree>>,
    packages: Vec<RelPath>,
    /// The active worktree of the snapshot (the overview's); the main one
    /// until [`FakeProjects::set_active`].
    active: Mutex<Option<WorktreeKey>>,
}

impl FakeProjects {
    /// The repository whose common directory is `common_dir`, with its
    /// main worktree at `root`.
    pub fn new(common_dir: &Path, root: &Path) -> FakeProjects {
        FakeProjects {
            key: ProjectKey::Git {
                common_dir: Arc::from(common_dir),
            },
            worktrees: Mutex::new(vec![Worktree {
                key: WorktreeKey(Arc::from(common_dir)),
                root: Arc::from(root),
                is_main: true,
                head: Head::Unknown,
                locked: false,
                prunable: false,
            }]),
            packages: Vec::new(),
            active: Mutex::new(Some(WorktreeKey(Arc::from(common_dir)))),
        }
    }

    /// Makes `key` the active worktree of the snapshot (None: a project
    /// without one, as a folder project).
    pub fn set_active(&self, key: Option<WorktreeKey>) {
        *self.active.lock().unwrap_or_else(PoisonError::into_inner) = key;
    }

    /// Sub-project directories `locate` matches, deepest first.
    pub fn with_packages(mut self, dirs: &[&str]) -> FakeProjects {
        self.packages = dirs.iter().map(|d| RelPath::from(*d)).collect();
        self.packages.sort_by_key(|d| std::cmp::Reverse(d.len()));
        self
    }

    /// A linked worktree: its git directory and root.
    pub fn add_worktree(&self, git_dir: &Path, root: &Path) -> WorktreeKey {
        let key = WorktreeKey(Arc::from(git_dir));
        self.worktrees
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Worktree {
                key: key.clone(),
                root: Arc::from(root),
                is_main: false,
                head: Head::Unknown,
                locked: false,
                prunable: false,
            });
        key
    }

    pub fn key(&self) -> &ProjectKey {
        &self.key
    }

    /// The main worktree's key.
    pub fn main(&self) -> WorktreeKey {
        self.worktrees
            .lock()
            .unwrap_or_else(PoisonError::into_inner)[0]
            .key
            .clone()
    }
}

impl Projects for FakeProjects {
    fn open(&self, _: &Path) -> OpenRequest {
        OpenRequest(0)
    }

    fn close(&self, _: &ProjectKey) {}

    fn set_active_worktree(&self, _: &ProjectKey, _: &WorktreeKey) {}

    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        if *project != self.key {
            return None;
        }
        let worktrees: Vec<Worktree> = self
            .worktrees
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let active = self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .map(|key| {
                let root = worktrees
                    .iter()
                    .find(|w| w.key == key)
                    .map(|w| w.root.clone())
                    .expect("an added worktree");
                ActiveTree {
                    root,
                    worktree: Some(key),
                    opened_at: RelPath::from(""),
                    files: Scan::Pending,
                    packages: Scan::Pending,
                    dirs: Scan::Pending,
                }
            });
        Some(Arc::new(ProjectSnapshot {
            key: self.key.clone(),
            generation: 1,
            name: "fixture".into(),
            worktrees: worktrees.into(),
            active,
        }))
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        vec![self.key.clone()]
    }

    fn locate(&self, path: &Path) -> Option<Location> {
        let worktrees = self
            .worktrees
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let w = worktrees
            .iter()
            .filter(|w| path.starts_with(&*w.root))
            .max_by_key(|w| w.root.as_os_str().len())?;
        let rel = path.strip_prefix(&*w.root).ok()?;
        let rel: RelPath = rel.to_string_lossy().replace('\\', "/").into();
        let package = self
            .packages
            .iter()
            .find(|d| rel.starts_with(&format!("{d}/")) || rel == **d)
            .map_or(PackageMatch::None, |d| PackageMatch::Found(d.clone()));
        Some(Location {
            project: self.key.clone(),
            worktree: Some(w.key.clone()),
            rel,
            package,
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
        Ok(PackageMatch::None)
    }
}
