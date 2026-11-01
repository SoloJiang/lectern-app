//! `testing::Scenario` and the bench fixture `heavy_days`
//! (docs/design/usage.md「本 crate 的公开项」「性能」).
//!
//! A scenario drives the store the way the engine does, without the plugin
//! or an executor: session records go in through [`FakeSessions`], each
//! batch is read from the saved cursor and stored, the watched window
//! snapshots take the batch's fact deltas, and the queries, the archive and
//! the history are called directly. Step 1 has session records only;
//! reflog records, worktree states and MR timelines join in later steps.
//!
//! The heavy day (「磁盘」): 50 sessions in 20 repositories, 2000 turns,
//! 6000 usage records; every turn takes 14 lines of 1.5 KiB in its log, of
//! which the turn start and its three usage records are materialized (the
//! tool calls, 20 000 a day, carry nothing step 1 stores).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::project::ProjectKey;
use lectern_services::session::{
    Agent, Positioned, RawTokens, Record, SessionSource, Timestamp, TurnId, UsageKey, UsageOrigin,
    UsageRecord,
};
use lectern_services::usage::{
    HistoryUsage, LocalDate, ProjectRef, UsageQuery, UsageSnapshot, UsageState,
};

use super::{FakeProjects, FakeSessions, TableZone};
use crate::engine::{Computed, Context, READ_BYTES, compute, compute_full, place_of_key};
use crate::finalize::{self, BATCH_TIME, BATCH_TURNS, Settling};
use crate::snapshot::{Applied, Watched};
use crate::store::cold::{self, Stepped};
use crate::store::{DB_FILE, FileStamp, Ingest, Ingested, PathPlace, Place, SessionInfo, Store};

fn io_error(what: &str, e: impl std::fmt::Debug) -> io::Error {
    io::Error::other(format!("{what}: {e:?}"))
}

/// A batch stored by [`Scenario::store_batch`].
pub struct Stored {
    out: Ingested,
}

impl Stored {
    /// Records in the batch.
    pub fn records(&self) -> usize {
        self.out.records
    }

    /// Usage sources written.
    pub fn sources(&self) -> usize {
        self.out.sources
    }

    /// The batch reached the end of the log.
    pub fn at_end(&self) -> bool {
        self.out.at_end
    }

    /// Log bytes the batch covered.
    pub fn bytes(&self) -> u64 {
        self.out.to.saturating_sub(self.out.from)
    }

    /// Fact rows the batch added or removed.
    pub fn fact_changes(&self) -> usize {
        self.out.facts.len()
    }
}

/// What [`Scenario::settle`] did.
pub struct Finished {
    stored: Stored,
    turns: usize,
    more: bool,
    wait_until: Option<i64>,
}

impl Finished {
    /// Turns settled.
    pub fn turns(&self) -> usize {
        self.turns
    }

    /// More turns are ready.
    pub fn more(&self) -> bool {
        self.more
    }

    /// The moment the earliest turn that is not ready yet will be.
    pub fn wait_until(&self) -> Option<i64> {
        self.wait_until
    }

    /// Fact rows added or removed.
    pub fn fact_changes(&self) -> usize {
        self.stored.out.facts.len()
    }
}

/// One archive step: the segment written, if any, and whether the month is
/// now archived.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArchiveStep {
    pub table: Option<&'static str>,
    pub rows: usize,
    pub raw_bytes: usize,
    pub packed_bytes: usize,
    pub finished: bool,
}

/// What [`Scenario::archive_all`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArchiveTotals {
    pub months: usize,
    pub segments: usize,
    pub rows: usize,
    pub raw_bytes: usize,
    pub packed_bytes: usize,
}

/// The store, a projects service, two session sources (Claude Code and
/// Codex) and a zone, driven directly.
pub struct Scenario {
    dir: PathBuf,
    store: Store,
    projects: FakeProjects,
    zone: TableZone,
    now_ms: i64,
    open: Vec<ProjectKey>,
    claude: FakeSessions,
    codex: FakeSessions,
    watched: Vec<(UsageQuery, Watched, Arc<UsageSnapshot>)>,
}

impl Scenario {
    /// Opens (or creates) the database in `dir`; the zone is UTC.
    pub fn open(dir: &Path, now_ms: i64) -> io::Result<Scenario> {
        let opened = Store::open(dir, now_ms).map_err(|e| io_error("open", e))?;
        Ok(Scenario {
            dir: dir.to_owned(),
            store: opened.store,
            projects: FakeProjects::new(),
            zone: TableZone::fixed(0),
            now_ms,
            open: Vec::new(),
            claude: FakeSessions::new("session-claude-code", Agent::ClaudeCode),
            codex: FakeSessions::new("session-codex", Agent::Codex),
            watched: Vec::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn set_zone(&mut self, zone: TableZone) {
        self.zone = zone;
    }

    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// Moves the wall clock (the windows and the archive's cutoff).
    pub fn set_now(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
    }

    /// A repository with its main worktree at `root` (common directory
    /// `root/.git`), opened.
    pub fn repo(&mut self, root: &Path) -> io::Result<ProjectKey> {
        let key = self.projects.repo(root, &root.join(".git"));
        self.projects.set_open(&key, true);
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.store
            .project_opened(&key, &name)
            .map_err(|e| io_error("project", e))?;
        if !self.open.contains(&key) {
            self.open.push(key.clone());
        }
        Ok(key)
    }

    /// The session source of an agent (Codex, or Claude Code for the
    /// others).
    pub fn sessions(&self, agent: &Agent) -> &FakeSessions {
        match agent {
            Agent::Codex => &self.codex,
            _ => &self.claude,
        }
    }

    /// Adds a session with its records.
    pub fn session(&self, agent: &Agent, id: &str, cwd: &Path, records: Vec<Positioned<Record>>) {
        let fake = self.sessions(agent);
        let started = records.first().map_or(self.now_ms, |r| at_of(&r.value));
        fake.add(id, cwd, started, None);
        fake.push(id, records);
    }

    fn path_place(&self, path: &Path) -> PathPlace {
        let (dir, name) = crate::paths::split(path);
        crate::paths::dir_place(Some(&self.projects), dir).file(name)
    }

    fn place(&self, cwd: &Path) -> Place {
        match lectern_services::project::Projects::resolve(&self.projects, cwd) {
            Ok(r) => place_of_key(&r.project, &r.rel),
            Err(_) => Place::Dir(cwd.to_path_buf()),
        }
    }

    /// Reads and stores one batch of at most `max_bytes`, from the saved
    /// cursor, without touching the watched snapshots.
    pub fn store_batch(&self, agent: &Agent, id: &str, max_bytes: u64) -> io::Result<Stored> {
        let fake = self.sessions(agent);
        let key = fake.key(id);
        let summary = fake
            .sessions()
            .get(&key)
            .cloned()
            .ok_or_else(|| io_error("session", id))?;
        let cursor = self.store.cursor(&key).map_err(|e| io_error("cursor", e))?;
        let stamp = FileStamp::of(&summary.file);
        let batch = fake
            .read(&key, &cursor, max_bytes)
            .map_err(|e| io_error("read", e))?;
        let start = self.place(&summary.cwd);
        let mut places = std::collections::HashMap::new();
        for r in &batch.records {
            if let Record::Context { cwd, .. } = &r.value {
                places
                    .entry(cwd.to_path_buf())
                    .or_insert_with(|| self.place(cwd));
            }
        }
        let mut edit_paths = std::collections::HashMap::new();
        for r in &batch.records {
            if let Record::ToolStarted(call) = &r.value
                && let lectern_services::session::ToolKind::Edit { paths } = &call.kind
            {
                for p in paths.iter() {
                    edit_paths
                        .entry(p.clone())
                        .or_insert_with(|| self.path_place(p));
                }
            }
        }
        let out = self
            .store
            .ingest(&Ingest {
                session: SessionInfo {
                    key: &key,
                    agent: &summary.agent,
                    parent: summary.parent.as_ref().map(|p| &p.session),
                    parent_call: summary.parent.as_ref().and_then(|p| p.tool_call.as_ref()),
                    inheritance_unknown: summary.inheritance_unknown,
                    start: &start,
                },
                from: &cursor,
                batch: &batch,
                places: &places,
                paths: &edit_paths,
                stamp,
                now_ms: self.now_ms,
            })
            .map_err(|e| io_error("ingest", e))?;
        Ok(Stored { out })
    }

    /// Applies a stored batch's fact deltas to the watched snapshots and
    /// replaces those it changes, as the engine does after a batch; a
    /// window that cannot take them is computed again. Returns the
    /// snapshots replaced.
    pub fn replace_snapshots(&mut self, stored: &Stored) -> io::Result<usize> {
        let out = &stored.out;
        let mut missing: Vec<i64> = self
            .watched
            .iter()
            .flat_map(|(_, w, _)| w.missing(&out.facts))
            .collect();
        missing.sort_unstable();
        missing.dedup();
        let texts = self
            .store
            .texts(&missing)
            .map_err(|e| io_error("texts", e))?;
        let mut replaced = 0;
        let mut again = Vec::new();
        for (i, (_, w, snapshot)) in self.watched.iter_mut().enumerate() {
            w.add_texts(&texts);
            let applied = if out.facts_unknown {
                Applied::Recompute
            } else {
                w.apply(out.commit, &out.facts, out.inheritance_unknown)
            };
            match applied {
                Applied::Changed => {
                    *snapshot = Arc::new(w.snapshot(UsageState::Ready));
                    replaced += 1;
                }
                Applied::Unchanged => {}
                Applied::Recompute => again.push(i),
            }
        }
        for i in again {
            let query = self.watched[i].0.clone();
            let w = self.full(&query)?;
            let s = Arc::new(w.snapshot(UsageState::Ready));
            self.watched[i] = (query, w, s);
            replaced += 1;
        }
        Ok(replaced)
    }

    /// One batch: stored, then the watched snapshots replaced.
    pub fn read(&mut self, agent: &Agent, id: &str, max_bytes: u64) -> io::Result<Stored> {
        let stored = self.store_batch(agent, id, max_bytes)?;
        self.replace_snapshots(&stored)?;
        Ok(stored)
    }

    /// Reads a session to its end in 8 MiB batches; returns the batches.
    pub fn read_all(&mut self, agent: &Agent, id: &str) -> io::Result<Vec<Stored>> {
        let mut out = Vec::new();
        loop {
            let stored = self.read(agent, id, READ_BYTES)?;
            let done = stored.at_end();
            out.push(stored);
            if done {
                return Ok(out);
            }
        }
    }

    /// A session of one turn that makes `calls` `Edit` calls, one file each
    /// (`src/file<n>.rs` below the worktree roots in turn, six lines each),
    /// and ends it with three usage records; its log is added to the
    /// source, not read yet. The bench of settling a turn uses it.
    pub fn edit_turn(
        &self,
        agent: &Agent,
        id: &str,
        cwd: &Path,
        roots: &[PathBuf],
        calls: usize,
        at: i64,
    ) {
        use lectern_services::session::{
            ToolCallId, ToolFinished, ToolKind, ToolStarted, ToolStatus,
        };
        let fake = self.sessions(agent);
        fake.add(id, cwd, at, None);
        let tid = format!("{id}-t0");
        let record = |pos: u64, value: Record| Positioned {
            pos,
            seq: 0,
            inherited: false,
            value,
        };
        let mut records = vec![
            record(
                0,
                Record::Context {
                    at: Timestamp(at),
                    cwd: Arc::from(cwd),
                    logged_branch: None,
                    model: Some("model-a".into()),
                },
            ),
            record(
                LINE_BYTES,
                Record::TurnStarted {
                    turn: TurnId(tid.as_str().into()),
                    at: Timestamp(at),
                },
            ),
        ];
        let mut pos = 2 * LINE_BYTES;
        for k in 0..3u64 {
            records.push(record(
                pos,
                Record::Usage(UsageRecord {
                    at: Timestamp(at + 1000 * (k as i64 + 1)),
                    turn: Some(TurnId(tid.as_str().into())),
                    model: Some("model-a".into()),
                    tokens: RawTokens::Anthropic {
                        input: 10 + k,
                        output: 200,
                        cache_read: 30_000,
                        cache_write: 400,
                        cache_write_5m: 0,
                        cache_write_1h: 0,
                    },
                    key: UsageKey::Claude {
                        message_id: format!("{id}-{k}").into(),
                        request_id: Some(format!("req-{id}-{k}").into()),
                    },
                    advisor: None,
                    superseded: false,
                    sidechain: false,
                    origin: UsageOrigin::SessionLog,
                }),
            ));
            pos += LINE_BYTES;
        }
        let content: String = (0..6).map(|i| format!("line {i}\n")).collect();
        for n in 0..calls {
            let path = roots[n % roots.len()].join(format!("src/file{n}.rs"));
            let call = format!("{id}-c{n}");
            fake.edit_call(
                id,
                &tid,
                &call,
                std::slice::from_ref(&path),
                vec![lectern_services::session::FileEdit {
                    path: path.clone(),
                    op: lectern_services::session::EditOp::Write {
                        content: content.as_str().into(),
                        created: true,
                    },
                }],
            );
            let started = at + 5000 + 100 * n as i64;
            records.push(record(
                pos,
                Record::ToolStarted(ToolStarted {
                    call: ToolCallId(call.as_str().into()),
                    turn: Some(TurnId(tid.as_str().into())),
                    at: Timestamp(started),
                    name: "Edit".into(),
                    kind: ToolKind::Edit {
                        paths: vec![path].into(),
                    },
                }),
            ));
            pos += LINE_BYTES;
            records.push(record(
                pos,
                Record::ToolFinished(ToolFinished {
                    call: ToolCallId(call.as_str().into()),
                    at: Timestamp(started + 50),
                    status: ToolStatus::Completed,
                    exec_session: None,
                }),
            ));
            pos += LINE_BYTES;
        }
        fake.push(id, records);
    }

    /// A linked worktree of a repository: `root` is another working tree of
    /// the repository whose common directory is `common_dir`.
    pub fn worktree(&mut self, root: &Path, common_dir: &Path) {
        self.projects.repo(root, common_dir);
    }

    /// One batch of the turns of a session that are ready, settled as the
    /// engine does (docs/design/usage.md「轮次的观察与改动项」第 5 步); the
    /// watched snapshots take the fact changes.
    pub fn settle(&mut self, agent: &Agent, id: &str) -> io::Result<Finished> {
        let fake = self.sessions(agent);
        let key = fake.key(id);
        let summary = fake
            .sessions()
            .get(&key)
            .cloned()
            .ok_or_else(|| io_error("session", id))?;
        let settled = {
            let resolve = |p: &Path| self.path_place(p);
            finalize::settle(&Settling {
                store: &self.store,
                source: fake,
                key: &key,
                now_ms: self.now_ms,
                end_open: !summary.open_turn,
                resolve: &resolve,
                turns: BATCH_TURNS,
                time: BATCH_TIME,
            })
            .map_err(|e| io_error("settle", e))?
        };
        let stored = Stored {
            out: settled.written.unwrap_or_default(),
        };
        if stored.out.commit != 0 {
            self.replace_snapshots(&stored)?;
        }
        Ok(Finished {
            stored,
            turns: settled.turns,
            more: settled.more,
            wait_until: settled.wait_until,
        })
    }

    fn context(&self) -> Context<'_> {
        Context {
            store: &self.store,
            projects: Some(&self.projects),
            zone: &self.zone,
            now_ms: self.now_ms,
            open: &self.open,
            state: UsageState::Ready,
            exists: &|_| true,
        }
    }

    fn full(&self, query: &UsageQuery) -> io::Result<Watched> {
        match compute_full(&self.context(), query).map_err(|e| io_error("query", e))? {
            Some(Computed::Watched(w)) => Ok(w),
            _ => Err(io_error("not a window query", query)),
        }
    }

    /// Keeps a window query's snapshot up to date from now on.
    pub fn watch(&mut self, query: UsageQuery) -> io::Result<()> {
        let w = self.full(&query)?;
        let s = Arc::new(w.snapshot(UsageState::Ready));
        self.watched.retain(|(q, _, _)| *q != query);
        self.watched.push((query, w, s));
        Ok(())
    }

    /// The watched snapshot of a query.
    pub fn watched(&self, query: &UsageQuery) -> Option<Arc<UsageSnapshot>> {
        self.watched
            .iter()
            .find(|(q, _, _)| q == query)
            .map(|(_, _, s)| s.clone())
    }

    /// A query computed from the database now (the snapshot task's work).
    pub fn snapshot(&self, query: &UsageQuery) -> io::Result<Option<UsageSnapshot>> {
        compute(&self.context(), query).map_err(|e| io_error("query", e))
    }

    /// `UsageQuery::History` of an open project.
    pub fn history(
        &self,
        project: &ProjectKey,
        from: LocalDate,
        to: LocalDate,
        days: bool,
    ) -> io::Result<HistoryUsage> {
        let query = UsageQuery::History {
            project: ProjectRef::Open(project.clone()),
            from,
            to,
            days,
        };
        match self.snapshot(&query)? {
            Some(UsageSnapshot::History(h)) => Ok(h),
            other => Err(io_error("not a history", other)),
        }
    }

    /// The UTC months the archive would do now, oldest first.
    pub fn archive_plan(&self) -> io::Result<Vec<(i32, u8)>> {
        let plan = self
            .store
            .archive_plan(self.now_ms, &self.zone)
            .map_err(|e| io_error("plan", e))?;
        Ok(plan.into_iter().map(cold::year_month).collect())
    }

    /// One archive step of a UTC month (its state, or one segment).
    pub fn archive_step(&self, month: (i32, u8)) -> io::Result<ArchiveStep> {
        let m = i64::from(month.0) * 12 + i64::from(month.1) - 1;
        match self
            .store
            .archive_step(m, self.now_ms, &self.zone)
            .map_err(|e| io_error("archive", e))?
        {
            Stepped::Started => Ok(ArchiveStep::default()),
            Stepped::Segment {
                table,
                rows,
                raw_bytes,
                packed_bytes,
                finished,
            } => Ok(ArchiveStep {
                table: Some(table),
                rows,
                raw_bytes,
                packed_bytes,
                finished,
            }),
            Stepped::Finished => Ok(ArchiveStep {
                finished: true,
                ..ArchiveStep::default()
            }),
            Stepped::Mismatch => Err(io_error("archive", "segment check failed")),
        }
    }

    /// Archives every month of the plan.
    pub fn archive_all(&self) -> io::Result<ArchiveTotals> {
        let a = self
            .store
            .archive_all(self.now_ms, &self.zone)
            .map_err(|e| io_error("archive", e))?;
        Ok(ArchiveTotals {
            months: a.months,
            segments: a.segments,
            rows: a.rows,
            raw_bytes: a.raw_bytes,
            packed_bytes: a.packed_bytes,
        })
    }

    /// The largest uncompressed segment of later archive steps.
    pub fn set_segment_bytes(&self, bytes: usize) {
        self.store.set_segment_bytes(bytes);
    }

    /// The full rebuild: every cold month restored to the hot tables and
    /// the derived tables rebuilt (the next archive archives them again).
    /// Returns the months restored.
    pub fn unarchive_all(&self) -> io::Result<usize> {
        self.store
            .unarchive_all()
            .map_err(|e| io_error("unarchive", e))
    }

    /// Rebuilds the hot window's derived tables.
    pub fn rebuild_hot(&self) -> io::Result<()> {
        self.store.rebuild_hot().map_err(|e| io_error("rebuild", e))
    }

    /// Cold segments decoded so far.
    pub fn cold_reads(&self) -> u64 {
        self.store.cold_reads()
    }

    /// The database file with its `-wal`, in bytes.
    pub fn db_bytes(&self) -> u64 {
        self.file_bytes("") + self.wal_bytes()
    }

    /// The size of the database's `-wal` alone.
    pub fn wal_bytes(&self) -> u64 {
        self.file_bytes("-wal")
    }

    fn file_bytes(&self, suffix: &str) -> u64 {
        let mut name = self.dir.join(DB_FILE).into_os_string();
        name.push(suffix);
        std::fs::metadata(name).map_or(0, |m| m.len())
    }

    /// Marks the database closed normally.
    pub fn close(self) {
        self.store.close();
    }
}

fn at_of(r: &Record) -> i64 {
    match r {
        Record::Context { at, .. }
        | Record::TurnStarted { at, .. }
        | Record::TurnEnded { at, .. } => at.0,
        Record::ToolStarted(t) => t.at.0,
        Record::ToolFinished(t) => t.at.0,
        Record::Usage(u) => u.at.0,
        Record::RateLimits(r) => r.at.0,
    }
}

/// The bytes of one line of a synthetic log.
pub const LINE_BYTES: u64 = 1536;

/// The lines of one synthetic turn: its start, three usage records and ten
/// tool lines that are not materialized.
pub const TURN_LINES: u64 = 14;

/// The bytes of one synthetic turn.
pub const TURN_BYTES: u64 = LINE_BYTES * TURN_LINES;

/// The heavy fixture's now: 2026-10-22 12:00 UTC. Its days end yesterday.
pub const HEAVY_NOW: i64 = 1_792_670_400_000;

/// Sessions, repositories, turns per session and usage records per turn of
/// a heavy day.
pub const HEAVY_SESSIONS: usize = 50;
pub const HEAVY_REPOS: usize = 20;
pub const HEAVY_TURNS: usize = 40;
pub const HEAVY_USAGES: usize = 3;

/// The heavy fixture's repositories.
pub fn heavy_roots() -> Vec<PathBuf> {
    (0..HEAVY_REPOS)
        .map(|i| PathBuf::from(format!("/work/heavy-{i:02}")))
        .collect()
}

/// The records of a synthetic session: `turns` turns from `start_ms`, one
/// every 6 minutes, each with three usage records 20 seconds apart, in
/// [`TURN_BYTES`] of log each, starting at log offset `from_pos` (0 for a
/// new log, whose first line is its context). Keys are
/// `<prefix>-<turn>-<k>`; Codex sessions use OpenAI tokens and response keys.
pub fn synthetic_turns(
    agent: &Agent,
    prefix: &str,
    cwd: &Path,
    start_ms: i64,
    turns: std::ops::Range<usize>,
    model: &str,
) -> Vec<Positioned<Record>> {
    let mut out = Vec::with_capacity(turns.len() * 4 + 1);
    if turns.start == 0 {
        out.push(Positioned {
            pos: 0,
            seq: 0,
            inherited: false,
            value: Record::Context {
                at: Timestamp(start_ms),
                cwd: Arc::from(cwd),
                logged_branch: None,
                model: Some(model.into()),
            },
        });
    }
    for t in turns {
        // Line 0 of the log is the context; a turn's lines follow it.
        let pos = t as u64 * TURN_BYTES + LINE_BYTES;
        let at = start_ms + t as i64 * 6 * 60_000;
        let tid = format!("{prefix}-t{t}");
        out.push(Positioned {
            pos,
            seq: 0,
            inherited: false,
            value: Record::TurnStarted {
                turn: TurnId(tid.clone().into()),
                at: Timestamp(at),
            },
        });
        for k in 0..HEAVY_USAGES {
            let (key, tokens) = match agent {
                Agent::Codex => (
                    UsageKey::CodexResponse {
                        thread_id: prefix.into(),
                        response_id: format!("{prefix}-{t}-{k}").into(),
                    },
                    RawTokens::OpenAi {
                        input: 25_000 + k as u64,
                        cached_input: 20_000,
                        cache_write: 0,
                        output: 400,
                        reasoning_output: 100,
                    },
                ),
                _ => (
                    UsageKey::Claude {
                        message_id: format!("{prefix}-{t}-{k}").into(),
                        request_id: Some(format!("req-{prefix}-{t}-{k}").into()),
                    },
                    RawTokens::Anthropic {
                        input: 1000 + k as u64,
                        output: 300,
                        cache_read: 20_000,
                        cache_write: 500,
                        cache_write_5m: 0,
                        cache_write_1h: 0,
                    },
                ),
            };
            out.push(Positioned {
                pos: pos + LINE_BYTES * (1 + 4 * k as u64),
                seq: 0,
                inherited: false,
                value: Record::Usage(UsageRecord {
                    at: Timestamp(at + 20_000 * (k as i64 + 1)),
                    turn: Some(TurnId(tid.clone().into())),
                    model: Some(model.into()),
                    tokens,
                    key,
                    advisor: None,
                    superseded: false,
                    sidechain: false,
                    origin: UsageOrigin::SessionLog,
                }),
            });
        }
    }
    out
}

/// One heavy session: its agent, ID, cwd, model and first turn's time.
pub struct HeavySession {
    pub agent: Agent,
    pub id: String,
    pub cwd: PathBuf,
    pub model: &'static str,
    pub start_ms: i64,
}

/// The sessions of heavy day `day` (0 is the oldest of the fixture's
/// `days`, ending yesterday): every fifth one is Codex, the repositories
/// taken in turn, starting from 09:00 UTC ten minutes apart.
pub fn heavy_sessions(days: u32, day: u32) -> Vec<HeavySession> {
    let day_start = (HEAVY_NOW.div_euclid(DAY_MS) - i64::from(days - day)) * DAY_MS;
    let roots = heavy_roots();
    (0..HEAVY_SESSIONS)
        .map(|s| {
            let codex = s % 5 == 4;
            HeavySession {
                agent: if codex {
                    Agent::Codex
                } else {
                    Agent::ClaudeCode
                },
                id: format!("heavy-{day:03}-{s:02}"),
                cwd: roots[s % HEAVY_REPOS].join(format!("src/m{}", s % 3)),
                model: match (codex, s % 2) {
                    (true, _) => "model-c",
                    (false, 0) => "model-a",
                    (false, _) => "model-b",
                },
                start_ms: day_start + 9 * 3_600_000 + s as i64 * 10 * 60_000,
            }
        })
        .collect()
}

const DAY_MS: i64 = 24 * 3_600_000;

/// Opens a scenario with the heavy fixture's repositories declared.
pub fn heavy_open(dir: &Path) -> io::Result<Scenario> {
    let mut s = Scenario::open(dir, HEAVY_NOW)?;
    for root in heavy_roots() {
        s.repo(&root)?;
    }
    Ok(s)
}

/// Writes `days` heavy days into the scenario: every session read to its
/// end, day by day (its records dropped from the source afterwards).
pub fn heavy_fill(s: &mut Scenario, days: u32) -> io::Result<()> {
    for day in 0..days {
        for h in heavy_sessions(days, day) {
            let records =
                synthetic_turns(&h.agent, &h.id, &h.cwd, h.start_ms, 0..HEAVY_TURNS, h.model);
            s.session(&h.agent, &h.id, &h.cwd, records);
            s.read_all(&h.agent, &h.id)?;
            s.sessions(&h.agent).remove(&h.id);
        }
    }
    Ok(())
}

/// The bench fixture: `days` heavy days ending yesterday (relative to
/// [`HEAVY_NOW`]) written into `dir/usage.db`.
pub fn heavy_days(dir: &Path, days: u32) -> io::Result<()> {
    let mut s = heavy_open(dir)?;
    heavy_fill(&mut s, days)?;
    s.close();
    Ok(())
}
