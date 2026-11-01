//! Test support (`test-support` feature, docs/design/usage.md「本 crate 的公开项」):
//! a session source replaying `services::session` records, a projects
//! service without IO, a time zone from a table and manual clocks. Every
//! fixture is synthetic; nothing here reads an agent's real session
//! directories.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::Deserialize;

use lectern_kernel::Clock;
use lectern_kernel::testing::ManualClock;
use lectern_services::project::{
    Head, Location, OpenError, OpenRequest, PackageMatch, ProjectKey, ProjectSnapshot, Projects,
    RelPath, ResolveError, Resolved, ScanError, Worktree, WorktreeKey,
};
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, Diagnostics, FileEdit, Format, Parent, Place,
    Positioned, Record, ResumeCommand, SessionError, SessionKey, SessionList, SessionSource,
    SessionSummary, SessionsChanged, SourceId, Timestamp, ToolCall, ToolCallId, ToolKind,
    ToolStatus, Turn, TurnId, TurnIndex, TurnItem,
};

use crate::LocalZone;
use crate::time::Time;

mod scenario;

pub use scenario::{
    ArchiveStep, ArchiveTotals, Finished, HEAVY_NOW, HEAVY_REPOS, HEAVY_SESSIONS, HEAVY_TURNS,
    HEAVY_USAGES, HeavySession, LINE_BYTES, Scenario, Stored, TURN_BYTES, TURN_LINES, heavy_days,
    heavy_fill, heavy_open, heavy_roots, heavy_sessions, synthetic_turns,
};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A time zone given as (from Unix milliseconds, offset in seconds) rows,
/// in order; before the first row the first offset applies.
pub struct TableZone {
    rows: Vec<(i64, i32)>,
}

impl TableZone {
    pub fn new(rows: &[(i64, i32)]) -> TableZone {
        let mut rows = rows.to_vec();
        rows.sort_by_key(|r| r.0);
        TableZone { rows }
    }

    /// One offset at all times.
    pub fn fixed(offset_secs: i32) -> TableZone {
        TableZone::new(&[(i64::MIN, offset_secs)])
    }
}

impl LocalZone for TableZone {
    fn offset_at(&self, unix_ms: i64) -> i32 {
        let i = self.rows.partition_point(|r| r.0 <= unix_ms);
        self.rows[i.saturating_sub(1)].1
    }
}

/// A monotonic clock and a wall clock that only move when told to, in step
/// unless [`ManualTime::jump_wall`] moves the wall clock alone.
#[derive(Clone)]
pub struct ManualTime {
    clock: ManualClock,
    start: Instant,
    wall_start: i64,
    jumped: Arc<AtomicI64>,
}

impl ManualTime {
    /// Starting at Unix milliseconds `wall_ms`.
    pub fn new(wall_ms: i64) -> ManualTime {
        let clock = ManualClock::new();
        let start = clock.now();
        ManualTime {
            clock,
            start,
            wall_start: wall_ms,
            jumped: Arc::new(AtomicI64::new(0)),
        }
    }

    /// Moves both clocks; timers that come due are woken on this thread.
    pub fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }

    /// Moves the wall clock only, as a sleep of the Mac does (the monotonic
    /// clock stops) or setting the clock; no timer is woken.
    pub fn jump_wall(&self, by_ms: i64) {
        self.jumped.fetch_add(by_ms, Ordering::Relaxed);
    }

    pub fn now_ms(&self) -> i64 {
        let since = self.clock.now() - self.start;
        let ran = i64::try_from(since.as_millis()).unwrap_or(i64::MAX);
        self.wall_start
            .saturating_add(ran)
            .saturating_add(self.jumped.load(Ordering::Relaxed))
    }

    /// Timers not yet due.
    pub fn pending_timers(&self) -> usize {
        self.clock.pending_timers()
    }

    pub(crate) fn time(&self) -> Time {
        let me = self.clone();
        Time {
            clock: Arc::new(self.clock.clone()),
            wall: Arc::new(move || me.now_ms()),
        }
    }
}

/// The bytes one record line takes in a [`FakeSessions`] file: positions are
/// given by the records, the last line is this long.
pub const LAST_LINE: u64 = 100;

/// A session script: summaries and the records of their logs, in the serde
/// form of `services::session`.
///
/// ```text
/// { "source": "session-claude-code", "agent": "ClaudeCode",
///   "sessions": [ { "id", "cwd", "started_at"?, "last_activity"?,
///                   "open_turn"?, "parent"?, "inheritance_unknown"?,
///                   "records": [Positioned<Record>…] } ] }
/// ```
#[derive(Deserialize)]
struct Script {
    source: String,
    agent: Agent,
    sessions: Vec<ScriptSession>,
}

#[derive(Deserialize)]
struct ScriptSession {
    id: String,
    cwd: PathBuf,
    #[serde(default)]
    started_at: i64,
    #[serde(default)]
    last_activity: Option<i64>,
    #[serde(default)]
    open_turn: bool,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    inheritance_unknown: bool,
    #[serde(default)]
    records: Vec<Positioned<Record>>,
}

struct FakeSession {
    summary: SessionSummary,
    records: Vec<Positioned<Record>>,
    /// The next `read` gives `reset` (an adapter upgrade).
    reset: bool,
}

impl FakeSession {
    fn len(&self) -> u64 {
        self.records.last().map_or(0, |r| r.pos + LAST_LINE)
    }
}

struct Inner {
    adapter: u32,
    generation: u64,
    sessions: Vec<FakeSession>,
    reads: HashMap<Arc<str>, usize>,
    /// The turns each session's index holds (`FakeSessions::edit_call`).
    index: HashMap<Arc<str>, Vec<Turn>>,
    /// What `load` gives, by session and the `pos` of the reference.
    contents: HashMap<(Arc<str>, u64), Content>,
    /// `turns_unretained` calls, and `load` calls, per session.
    index_reads: HashMap<Arc<str>, usize>,
    loads: HashMap<Arc<str>, usize>,
    /// The next `pos` of a content reference.
    next_ref: u64,
    /// `turns_unretained` fails with an IO error (`FakeSessions::fail_index`).
    index_fails: bool,
}

/// A `SessionSource` replaying record scripts; counts its reads.
pub struct FakeSessions {
    source: SourceId,
    agent: Agent,
    /// Where the stand-in log files go, if anywhere.
    files: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl FakeSessions {
    /// An empty source; its sessions' files do not exist.
    pub fn new(source: &str, agent: Agent) -> FakeSessions {
        FakeSessions {
            source: SourceId(source.into()),
            agent,
            files: None,
            inner: Mutex::new(Inner {
                adapter: 1,
                generation: 1,
                sessions: Vec::new(),
                reads: HashMap::new(),
                index: HashMap::new(),
                contents: HashMap::new(),
                index_reads: HashMap::new(),
                loads: HashMap::new(),
                next_ref: 0,
                index_fails: false,
            }),
        }
    }

    /// An empty source whose sessions have files in `dir`: sparse files as
    /// long as the records say, rewritten with a new modification time on
    /// every change, so a reader can tell a changed log from an unchanged one.
    pub fn with_files(source: &str, agent: Agent, dir: &Path) -> FakeSessions {
        FakeSessions {
            files: Some(dir.to_owned()),
            ..FakeSessions::new(source, agent)
        }
    }

    /// Changes the adapter version: a read from a cursor of another version
    /// gives `reset` and starts from the beginning, as `agents` does.
    pub fn set_adapter(&self, version: u32) {
        let mut inner = lock(&self.inner);
        inner.adapter = version;
        inner.generation += 1;
    }

    fn write_file(&self, s: &FakeSession, generation: u64) {
        if self.files.is_none() {
            return;
        }
        let file = std::fs::File::create(&*s.summary.file).expect("fake session file");
        file.set_len(s.len()).expect("fake session file length");
        let mtime = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000 + generation);
        file.set_modified(mtime).expect("fake session file time");
    }

    /// A source from a JSON script (see [`Script`]). Panics on a bad script:
    /// test code.
    pub fn from_json(json: &str) -> FakeSessions {
        let script: Script = serde_json::from_str(json).expect("session script");
        let fake = FakeSessions::new(&script.source, script.agent);
        for s in script.sessions {
            let parent = s.parent.as_deref();
            fake.add(&s.id, &s.cwd, s.started_at, parent);
            fake.edit(&s.id, |summary| {
                summary.open_turn = s.open_turn;
                summary.inheritance_unknown = s.inheritance_unknown;
                if let Some(at) = s.last_activity {
                    summary.last_activity = Timestamp(at);
                }
            });
            fake.push(&s.id, s.records);
        }
        fake
    }

    pub fn key(&self, id: &str) -> SessionKey {
        SessionKey {
            source: self.source.clone(),
            id: id.into(),
        }
    }

    /// Adds a session with no records.
    pub fn add(&self, id: &str, cwd: &Path, started_at: i64, parent: Option<&str>) {
        let key = self.key(id);
        let parent = parent.map(|p| Parent {
            session: self.key(p),
            tool_call: None,
        });
        let summary = SessionSummary {
            key,
            agent: self.agent.clone(),
            parent,
            file: match &self.files {
                Some(dir) => Arc::from(dir.join(format!("{id}.jsonl"))),
                None => Arc::from(Path::new(&format!("/fake/{id}.jsonl"))),
            },
            cwd: Arc::from(cwd),
            place: Place::Pending,
            started_at: Timestamp(started_at),
            last_activity: Timestamp(started_at),
            title: None,
            agent_version: None,
            logged_branch: None,
            open_turn: false,
            tail_pending: false,
            archived: false,
            inheritance_unknown: false,
            format: Format::Known,
        };
        let session = FakeSession {
            summary,
            records: Vec::new(),
            reset: false,
        };
        let mut inner = lock(&self.inner);
        inner.generation += 1;
        self.write_file(&session, inner.generation);
        inner.sessions.push(session);
    }

    /// Changes a session's summary.
    pub fn edit(&self, id: &str, f: impl FnOnce(&mut SessionSummary)) {
        let mut inner = lock(&self.inner);
        inner.generation += 1;
        if let Some(s) = inner.sessions.iter_mut().find(|s| &*s.summary.key.id == id) {
            f(&mut s.summary);
        }
    }

    /// Appends records; their positions must follow the existing ones.
    pub fn push(&self, id: &str, records: Vec<Positioned<Record>>) {
        let mut inner = lock(&self.inner);
        inner.generation += 1;
        let generation = inner.generation;
        let s = inner
            .sessions
            .iter_mut()
            .find(|s| &*s.summary.key.id == id)
            .expect("session in the source");
        if let Some(at) = records.iter().filter_map(|r| record_at(&r.value)).max() {
            s.summary.last_activity = Timestamp(s.summary.last_activity.0.max(at));
        }
        s.records.extend(records);
        self.write_file(s, generation);
    }

    /// Replaces a session's records; the next read gives `reset` (the
    /// adapter was upgraded and parses the file again).
    pub fn replace(&self, id: &str, records: Vec<Positioned<Record>>) {
        let mut inner = lock(&self.inner);
        inner.generation += 1;
        let generation = inner.generation;
        if let Some(s) = inner.sessions.iter_mut().find(|s| &*s.summary.key.id == id) {
            s.records = records;
            s.reset = true;
            self.write_file(s, generation);
        }
    }

    /// Removes a session (its log was deleted).
    pub fn remove(&self, id: &str) {
        let mut inner = lock(&self.inner);
        inner.generation += 1;
        inner.sessions.retain(|s| &*s.summary.key.id != id);
    }

    /// The event announcing new records in `ids`.
    pub fn changed(&self, ids: &[&str]) -> SessionsChanged {
        let generation = lock(&self.inner).generation;
        SessionsChanged {
            source: self.source.clone(),
            generation,
            changed: ids.iter().map(|id| self.key(id)).collect::<Vec<_>>().into(),
            removed: Arc::from(Vec::new()),
        }
    }

    /// `read` calls for a session so far.
    pub fn reads(&self, id: &str) -> usize {
        lock(&self.inner).reads.get(id).copied().unwrap_or(0)
    }

    /// `read` calls for every session so far.
    pub fn total_reads(&self) -> usize {
        lock(&self.inner).reads.values().sum()
    }

    /// An `Edit` call of a turn as the session's turn index and `load` give
    /// it: the call (completed, naming `paths`) is added to the turn, which
    /// is created when the index has none of this ID yet, and `load` of its
    /// input gives `edits`. The records of the log (`ToolStarted`,
    /// `ToolFinished`) are the test's own.
    pub fn edit_call(
        &self,
        session: &str,
        turn: &str,
        call: &str,
        paths: &[PathBuf],
        edits: Vec<FileEdit>,
    ) {
        self.edit_call_with(session, turn, call, paths, edits, ToolStatus::Completed);
    }

    /// The same with the status the index gives the call.
    pub fn edit_call_with(
        &self,
        session: &str,
        turn: &str,
        call: &str,
        paths: &[PathBuf],
        edits: Vec<FileEdit>,
        status: ToolStatus,
    ) {
        let mut inner = lock(&self.inner);
        let id: Arc<str> = session.into();
        let at = ContentRef::new(inner.next_ref, 0, 0);
        inner.next_ref += 1;
        inner
            .contents
            .insert((id.clone(), at.pos()), Content::Edits(edits.into()));
        let turns = inner.index.entry(id).or_default();
        let position = match turns.iter().position(|t| &*t.id.0 == turn) {
            Some(p) => p,
            None => {
                turns.push(Turn {
                    id: TurnId(turn.into()),
                    ordinal: turns.len() as u32 + 1,
                    started_at: Timestamp(0),
                    ended_at: None,
                    prompt: None,
                    prompt_preview: "".into(),
                    items: Arc::from(Vec::new()),
                });
                turns.len() - 1
            }
        };
        let t = &mut turns[position];
        let mut items: Vec<TurnItem> = t.items.iter().cloned().collect();
        items.push(TurnItem::Tool(ToolCall {
            id: ToolCallId(call.into()),
            name: "Edit".into(),
            kind: ToolKind::Edit {
                paths: paths.to_vec().into(),
            },
            started_at: Timestamp(0),
            finished: Some((Timestamp(0), status)),
            input: at,
            result: None,
        }));
        t.items = items.into();
        inner.generation += 1;
    }

    /// Makes `turns_unretained` fail with an IO error (or work again).
    pub fn fail_index(&self, fails: bool) {
        lock(&self.inner).index_fails = fails;
    }

    /// `turns_unretained` calls for a session so far.
    pub fn index_reads(&self, id: &str) -> usize {
        lock(&self.inner).index_reads.get(id).copied().unwrap_or(0)
    }

    /// `load` calls for a session so far.
    pub fn loads(&self, id: &str) -> usize {
        lock(&self.inner).loads.get(id).copied().unwrap_or(0)
    }
}

fn record_at(r: &Record) -> Option<i64> {
    match r {
        Record::Context { at, .. }
        | Record::TurnStarted { at, .. }
        | Record::TurnEnded { at, .. } => Some(at.0),
        Record::ToolStarted(t) => Some(t.at.0),
        Record::ToolFinished(t) => Some(t.at.0),
        Record::Usage(u) => Some(u.at.0),
        Record::RateLimits(r) => Some(r.at.0),
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
        let inner = lock(&self.inner);
        let mut sessions: Vec<SessionSummary> =
            inner.sessions.iter().map(|s| s.summary.clone()).collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity));
        Arc::new(SessionList {
            generation: inner.generation,
            sessions: sessions.into(),
            diagnostics: Diagnostics::default(),
        })
    }

    /// Records from the cursor's offset, whole lines (one `pos`) at a time,
    /// until about `max_bytes`; at least one line.
    fn read(&self, key: &SessionKey, from: &Cursor, max_bytes: u64) -> Result<Batch, SessionError> {
        let mut inner = lock(&self.inner);
        *inner.reads.entry(key.id.clone()).or_default() += 1;
        let adapter = inner.adapter;
        let s = inner
            .sessions
            .iter_mut()
            .find(|s| s.summary.key == *key)
            .ok_or(SessionError::NotFound)?;
        let upgraded = from.adapter_version().is_some_and(|v| v != adapter);
        let reset = (std::mem::take(&mut s.reset) || upgraded) && !from.is_start();
        let offset = if reset { 0 } else { from.offset() };
        let mut records = Vec::new();
        let mut next = s.len();
        let mut line_start: Option<u64> = None;
        for r in s.records.iter().filter(|r| r.pos >= offset) {
            if line_start != Some(r.pos) {
                if let Some(first) = records.first().map(|f: &Positioned<Record>| f.pos)
                    && r.pos - first >= max_bytes
                {
                    next = r.pos;
                    break;
                }
                line_start = Some(r.pos);
            }
            records.push(r.clone());
        }
        let len = s.len();
        Ok(Batch {
            reset,
            records,
            cursor: Cursor::new(self.source.clone(), adapter, next, Vec::new()),
            at_end: next >= len,
        })
    }

    fn turns(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        Err(SessionError::Unrecognized)
    }

    /// The turns `edit_call` registered; `Unrecognized` for a session
    /// without any.
    fn turns_unretained(&self, key: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        let mut inner = lock(&self.inner);
        *inner.index_reads.entry(key.id.clone()).or_default() += 1;
        if inner.index_fails {
            return Err(SessionError::Io("the index cannot be read".into()));
        }
        match inner.index.get(&key.id) {
            Some(turns) => Ok(Arc::new(TurnIndex {
                session: key.clone(),
                cursor: Cursor::START,
                turns: turns.clone().into(),
                format: Format::Known,
            })),
            None => Err(SessionError::Unrecognized),
        }
    }

    fn load(&self, key: &SessionKey, at: &ContentRef) -> Result<Content, SessionError> {
        let mut inner = lock(&self.inner);
        *inner.loads.entry(key.id.clone()).or_default() += 1;
        inner
            .contents
            .get(&(key.id.clone(), at.pos()))
            .cloned()
            .ok_or(SessionError::NotFound)
    }

    fn resume(&self, _: &SessionKey) -> Option<ResumeCommand> {
        None
    }

    fn complete(&self, _: &[SessionKey]) {}
}

/// A `Projects` without IO: repositories (root → common directory) and
/// folders the test declares, which of them are open, and `resolve` by
/// prefix. Unknown paths resolve to themselves as folders, as `project`
/// does for a directory outside any repository; `missing` paths fail.
#[derive(Default)]
pub struct FakeProjects {
    repos: Mutex<Vec<(PathBuf, PathBuf)>>,
    folders: Mutex<Vec<PathBuf>>,
    missing: Mutex<Vec<PathBuf>>,
    open: Mutex<Vec<ProjectKey>>,
    resolves: AtomicUsize,
}

impl FakeProjects {
    pub fn new() -> FakeProjects {
        FakeProjects::default()
    }

    /// A repository with its main worktree at `root`; called again with the
    /// same common directory it adds a linked worktree (its own worktree key).
    pub fn repo(&self, root: &Path, common_dir: &Path) -> ProjectKey {
        lock(&self.repos).push((root.to_owned(), common_dir.to_owned()));
        ProjectKey::Git {
            common_dir: common_dir.into(),
        }
    }

    /// A folder project's root (not yet open).
    pub fn folder(&self, root: &Path) -> ProjectKey {
        lock(&self.folders).push(root.to_owned());
        ProjectKey::Folder { root: root.into() }
    }

    /// `resolve` fails for this path (the directory no longer exists).
    pub fn missing(&self, path: &Path) {
        lock(&self.missing).push(path.to_owned());
    }

    pub fn set_open(&self, key: &ProjectKey, open: bool) {
        let mut list = lock(&self.open);
        list.retain(|k| k != key);
        if open {
            list.push(key.clone());
        }
    }

    /// `resolve` calls so far.
    pub fn resolves(&self) -> usize {
        self.resolves.load(Ordering::Relaxed)
    }

    fn root_of(&self, key: &ProjectKey) -> Option<PathBuf> {
        match key {
            ProjectKey::Git { common_dir } => lock(&self.repos)
                .iter()
                .find(|(_, c)| c.as_path() == &**common_dir)
                .map(|(r, _)| r.clone()),
            ProjectKey::Folder { root } => Some(root.to_path_buf()),
        }
    }
}

fn rel(path: &Path, root: &Path) -> RelPath {
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
        .into()
}

impl Projects for FakeProjects {
    fn open(&self, _: &Path) -> OpenRequest {
        OpenRequest(0)
    }

    fn close(&self, _: &ProjectKey) {}

    fn set_active_worktree(&self, _: &ProjectKey, _: &WorktreeKey) {}

    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        let root = self.root_of(project)?;
        let worktrees: Vec<Worktree> = match project {
            ProjectKey::Git { common_dir } => vec![Worktree {
                key: WorktreeKey(common_dir.clone()),
                root: root.as_path().into(),
                is_main: true,
                head: Head::Unknown,
                locked: false,
                prunable: false,
            }],
            ProjectKey::Folder { .. } => Vec::new(),
        };
        Some(Arc::new(ProjectSnapshot {
            key: project.clone(),
            generation: 1,
            name: root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
            worktrees: worktrees.into(),
            active: None,
        }))
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        lock(&self.open).clone()
    }

    fn locate(&self, path: &Path) -> Option<Location> {
        let open = lock(&self.open).clone();
        let resolved = self.resolve_inner(path).ok()?;
        if !open.contains(&resolved.project) {
            return None;
        }
        Some(Location {
            project: resolved.project,
            worktree: resolved.worktree,
            rel: resolved.rel,
            package: PackageMatch::None,
        })
    }

    fn resolve(&self, path: &Path) -> Result<Resolved, ResolveError> {
        self.resolves.fetch_add(1, Ordering::Relaxed);
        self.resolve_inner(path)
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

impl FakeProjects {
    fn resolve_inner(&self, path: &Path) -> Result<Resolved, ResolveError> {
        if lock(&self.missing).iter().any(|m| path.starts_with(m)) {
            return Err(OpenError::NotFound);
        }
        if let Some((root, common)) = lock(&self.repos)
            .iter()
            .filter(|(r, _)| path.starts_with(r))
            .max_by_key(|(r, _)| r.as_os_str().len())
        {
            return Ok(Resolved {
                project: ProjectKey::Git {
                    common_dir: common.as_path().into(),
                },
                worktree: Some(WorktreeKey(root.as_path().into())),
                rel: rel(path, root),
            });
        }
        let open = lock(&self.open).clone();
        if let Some(root) = lock(&self.folders)
            .iter()
            .filter(|r| path.starts_with(r))
            .filter(|r| {
                open.contains(&ProjectKey::Folder {
                    root: r.as_path().into(),
                })
            })
            .max_by_key(|r| r.as_os_str().len())
        {
            return Ok(Resolved {
                project: ProjectKey::Folder {
                    root: root.as_path().into(),
                },
                worktree: None,
                rel: rel(path, root),
            });
        }
        Ok(Resolved {
            project: ProjectKey::Folder { root: path.into() },
            worktree: None,
            rel: "".into(),
        })
    }
}
