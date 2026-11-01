//! Record builders and a harness that drives the store directly (no
//! plugin, no executor): read a fake session to its end, open projects,
//! compute snapshots.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::project::{ProjectKey, Projects};
use lectern_services::session::{
    Positioned, RawTokens, Record, SessionSummary, Timestamp, TurnId, UsageKey, UsageOrigin,
    UsageRecord,
};
use lectern_services::usage::{
    AllUsage, LocalDate, ProjectRef, ProjectUsage, Tokens, UsageQuery, UsageSnapshot, UsageState,
    Window,
};

use crate::engine::{Context, READ_BYTES, compute, place_of_key};
use crate::store::{FileStamp, Ingest, Ingested, Place, SessionInfo, Store};
use crate::testing::{FakeProjects, FakeSessions, TableZone};
use crate::window;

/// 2026-10-20T00:00:00Z.
pub(crate) const OCT_20: i64 = 1_792_454_400_000;
pub(crate) const HOUR: i64 = 3_600_000;
pub(crate) const DAY: i64 = 24 * HOUR;

pub(crate) fn rec(pos: u64, value: Record) -> Positioned<Record> {
    Positioned {
        pos,
        seq: 0,
        inherited: false,
        value,
    }
}

pub(crate) fn inherited(mut r: Positioned<Record>) -> Positioned<Record> {
    r.inherited = true;
    r
}

pub(crate) fn seq(mut r: Positioned<Record>, seq: u16) -> Positioned<Record> {
    r.seq = seq;
    r
}

pub(crate) fn context(pos: u64, at: i64, cwd: &str) -> Positioned<Record> {
    rec(
        pos,
        Record::Context {
            at: Timestamp(at),
            cwd: Arc::from(Path::new(cwd)),
            logged_branch: None,
            model: None,
        },
    )
}

pub(crate) fn turn(pos: u64, id: &str, at: i64) -> Positioned<Record> {
    rec(
        pos,
        Record::TurnStarted {
            turn: TurnId(id.into()),
            at: Timestamp(at),
        },
    )
}

/// Anthropic tokens: input, output, cache read, cache write.
pub(crate) fn anthropic(t: [u64; 4]) -> RawTokens {
    RawTokens::Anthropic {
        input: t[0],
        output: t[1],
        cache_read: t[2],
        cache_write: t[3],
        cache_write_5m: 0,
        cache_write_1h: 0,
    }
}

/// OpenAI tokens: input (with cached), cached input, output.
pub(crate) fn openai(input: u64, cached: u64, output: u64) -> RawTokens {
    RawTokens::OpenAi {
        input,
        cached_input: cached,
        cache_write: 0,
        output,
        reasoning_output: 0,
    }
}

pub(crate) fn usage(
    at: i64,
    turn: Option<&str>,
    model: &str,
    key: UsageKey,
    tokens: RawTokens,
) -> UsageRecord {
    UsageRecord {
        at: Timestamp(at),
        turn: turn.map(|t| TurnId(t.into())),
        model: Some(model.into()),
        tokens,
        key,
        advisor: None,
        superseded: false,
        sidechain: false,
        origin: UsageOrigin::SessionLog,
    }
}

pub(crate) fn claude_key(message: &str, request: &str) -> UsageKey {
    UsageKey::Claude {
        message_id: message.into(),
        request_id: Some(request.into()),
    }
}

/// A Claude Code usage record at `pos`.
pub(crate) fn claude(
    pos: u64,
    at: i64,
    turn: Option<&str>,
    message: &str,
    t: [u64; 4],
) -> Positioned<Record> {
    rec(
        pos,
        Record::Usage(usage(
            at,
            turn,
            "model-a",
            claude_key(message, &format!("req-{message}")),
            anthropic(t),
        )),
    )
}

pub(crate) fn tokens(t: [u64; 4]) -> Tokens {
    Tokens {
        input: t[0],
        output: t[1],
        cache_read: t[2],
        cache_write: t[3],
    }
}

/// The store, a projects service and a zone; sessions are read with the
/// same steps as the engine's `read_batch`.
pub(crate) struct Harness {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) store: Option<Store>,
    pub(crate) projects: FakeProjects,
    pub(crate) zone: TableZone,
    pub(crate) now: i64,
    pub(crate) open: Vec<ProjectKey>,
    /// Paths that no longer exist, for the snapshots' `PathMissing`.
    pub(crate) missing: std::cell::RefCell<Vec<PathBuf>>,
}

impl Harness {
    /// UTC, now 2026-10-22T12:00Z.
    pub(crate) fn new() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), OCT_20).unwrap().store;
        Harness {
            dir,
            store: Some(store),
            projects: FakeProjects::new(),
            zone: TableZone::fixed(0),
            now: OCT_20 + 2 * DAY + 12 * HOUR,
            open: Vec::new(),
            missing: std::cell::RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn store(&self) -> &Store {
        self.store.as_ref().unwrap()
    }

    /// Closes the store (cleanly when `clean`) and opens it again.
    pub(crate) fn reopen(&mut self, clean: bool) -> Reopened {
        self.close(clean);
        self.open_with(&crate::store::schema::CURRENT)
    }

    /// Closes the store (cleanly when `clean`).
    pub(crate) fn close(&mut self, clean: bool) {
        if let Some(store) = self.store.take() {
            if clean {
                store.close();
            }
            drop(store);
        }
    }

    /// Opens the closed store with these tables.
    pub(crate) fn open_with(&mut self, schema: &'static crate::store::schema::Schema) -> Reopened {
        let opened = Store::open_with(self.dir.path(), schema, self.now).unwrap();
        self.store = Some(opened.store);
        Reopened {
            needs_check: opened.needs_check,
            recovered: opened.recovered,
            migrated_from: opened.migrated_from,
        }
    }

    pub(crate) fn db_path(&self) -> PathBuf {
        self.dir.path().join(crate::store::DB_FILE)
    }

    fn place(&self, summary: &SessionSummary, cwd: &Path) -> Place {
        let _ = summary;
        match self.projects.resolve(cwd) {
            Ok(r) => place_of_key(&r.project, &r.rel),
            Err(_) => Place::Dir(cwd.to_path_buf()),
        }
    }

    /// One `read` of at most `max` bytes, stored.
    pub(crate) fn read_once(&self, fake: &FakeSessions, id: &str, max: u64) -> Ingested {
        self.try_read_once(fake, id, max).unwrap()
    }

    pub(crate) fn try_read_once(
        &self,
        fake: &FakeSessions,
        id: &str,
        max: u64,
    ) -> Result<Ingested, crate::store::StoreError> {
        let read = self.read_batch(fake, id, max);
        self.commit(&read)
    }

    /// The first half of a read, as the engine does it outside any
    /// transaction: the saved cursor, then the batch from it.
    pub(crate) fn read_batch(&self, fake: &FakeSessions, id: &str, max: u64) -> PendingRead {
        let key = fake.key(id);
        let summary = lectern_services::session::SessionSource::sessions(fake)
            .get(&key)
            .cloned()
            .expect("session");
        let cursor = self.store().cursor(&key).unwrap();
        let stamp = FileStamp::of(&summary.file);
        let batch =
            lectern_services::session::SessionSource::read(fake, &key, &cursor, max).unwrap();
        let start = self.place(&summary, &summary.cwd);
        let mut places = HashMap::new();
        for r in &batch.records {
            if let Record::Context { cwd, .. } = &r.value {
                places.insert(cwd.to_path_buf(), self.place(&summary, cwd));
            }
        }
        PendingRead {
            summary,
            cursor,
            batch,
            stamp,
            places,
            start,
        }
    }

    /// The second half: stores the batch (the cursor it was read from may
    /// be stale by now).
    pub(crate) fn commit(&self, read: &PendingRead) -> Result<Ingested, crate::store::StoreError> {
        let summary = &read.summary;
        self.store().ingest(&Ingest {
            session: SessionInfo {
                key: &summary.key,
                agent: &summary.agent,
                parent: summary.parent.as_ref().map(|p| &p.session),
                inheritance_unknown: summary.inheritance_unknown,
                start: &read.start,
            },
            from: &read.cursor,
            batch: &read.batch,
            places: &read.places,
            stamp: read.stamp,
            now_ms: self.now,
        })
    }

    /// Reads a session to its end.
    pub(crate) fn read_all(&self, fake: &FakeSessions, id: &str) {
        for _ in 0..10_000 {
            if self.read_once(fake, id, READ_BYTES).at_end {
                return;
            }
        }
        panic!("session {id} never reached its end");
    }

    /// Reads every session of `fake` to its end.
    pub(crate) fn read_every(&self, fake: &FakeSessions) {
        let list = lectern_services::session::SessionSource::sessions(fake);
        for s in list.sessions.iter() {
            self.read_all(fake, &s.key.id);
        }
    }

    /// A repository at `root` (common directory `root/.git`), opened.
    pub(crate) fn open_repo(&mut self, root: &str) -> ProjectKey {
        let root = Path::new(root);
        let key = self.projects.repo(root, &root.join(".git"));
        self.open_project(&key);
        key
    }

    pub(crate) fn open_project(&mut self, key: &ProjectKey) {
        self.projects.set_open(key, true);
        let name = self
            .projects
            .snapshot(key)
            .map(|s| s.name.to_string())
            .unwrap_or_default();
        self.store().project_opened(key, &name).unwrap();
        if !self.open.contains(key) {
            self.open.push(key.clone());
        }
    }

    pub(crate) fn snapshot(&self, query: &UsageQuery) -> Option<UsageSnapshot> {
        let cx = Context {
            store: self.store(),
            projects: Some(&self.projects),
            zone: &self.zone,
            now_ms: self.now,
            open: &self.open,
            state: UsageState::Ready,
            exists: &|p| !self.missing.borrow().iter().any(|m| m == p),
        };
        compute(&cx, query).unwrap()
    }

    /// A window query's accumulators, computed in full.
    pub(crate) fn watched(&self, query: &UsageQuery) -> crate::snapshot::Watched {
        let cx = Context {
            store: self.store(),
            projects: Some(&self.projects),
            zone: &self.zone,
            now_ms: self.now,
            open: &self.open,
            state: UsageState::Ready,
            exists: &|p| !self.missing.borrow().iter().any(|m| m == p),
        };
        match crate::engine::compute_full(&cx, query).unwrap() {
            Some(crate::engine::Computed::Watched(w)) => w,
            _ => panic!("not a window query"),
        }
    }

    /// Applies a stored batch's fact deltas, as the engine does.
    pub(crate) fn apply(
        &self,
        watched: &mut crate::snapshot::Watched,
        out: &Ingested,
    ) -> crate::snapshot::Applied {
        if out.facts_unknown {
            return crate::snapshot::Applied::Recompute;
        }
        let texts = self.store().texts(&watched.missing(&out.facts)).unwrap();
        watched.add_texts(&texts);
        watched.apply(out.commit, &out.facts, out.inheritance_unknown)
    }

    pub(crate) fn project(&self, key: &ProjectKey, window: Window) -> ProjectUsage {
        match self.snapshot(&UsageQuery::Project {
            project: ProjectRef::Open(key.clone()),
            window,
        }) {
            Some(UsageSnapshot::Project(p)) => p,
            other => panic!("not a project snapshot: {other:?}"),
        }
    }

    pub(crate) fn all(&self, window: Window) -> AllUsage {
        match self.snapshot(&UsageQuery::AllProjects { window }) {
            Some(UsageSnapshot::All(a)) => a,
            other => panic!("not an all-projects snapshot: {other:?}"),
        }
    }

    /// Whole tokens per local day over the last `n` days, every kind.
    pub(crate) fn days(&self, n: u32) -> Vec<(LocalDate, Tokens)> {
        let days = window::last_days(n, self.now, &self.zone);
        let range = days[0].1.start..days[days.len() - 1].1.end;
        let totals = self.store().sums_all(range).unwrap();
        let mut micro = vec![[0u64; 4]; days.len()];
        for s in &totals.sums {
            let at = s.bucket * window::BUCKET_MS;
            if let Some(i) = days.iter().position(|(_, r)| r.contains(&at)) {
                for (m, add) in micro[i].iter_mut().zip(s.micro) {
                    *m += add;
                }
            }
        }
        days.iter()
            .zip(micro)
            .map(|((d, _), m)| {
                (
                    *d,
                    Tokens {
                        input: m[0] / 1_000_000,
                        output: m[1] / 1_000_000,
                        cache_read: m[2] / 1_000_000,
                        cache_write: m[3] / 1_000_000,
                    },
                )
            })
            .collect()
    }

    /// The whole of the last 30 days.
    pub(crate) fn total(&self) -> Tokens {
        self.all(Window::D30).total
    }
}

/// A batch read but not stored yet.
pub(crate) struct PendingRead {
    pub(crate) summary: SessionSummary,
    pub(crate) cursor: lectern_services::session::Cursor,
    pub(crate) batch: lectern_services::session::Batch,
    pub(crate) stamp: Option<FileStamp>,
    pub(crate) places: HashMap<PathBuf, Place>,
    pub(crate) start: Place,
}

/// What reopening found.
pub(crate) struct Reopened {
    pub(crate) needs_check: bool,
    pub(crate) recovered: Option<crate::store::Recovery>,
    pub(crate) migrated_from: Option<i32>,
}

pub(crate) fn date(d: LocalDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

pub(crate) fn fmt_tokens(t: &Tokens) -> String {
    format!(
        "in {} out {} read {} write {} (= {})",
        t.input,
        t.output,
        t.cache_read,
        t.cache_write,
        t.total()
    )
}

/// A project snapshot as text, for insta.
pub(crate) fn render_project(p: &ProjectUsage) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "project: {} folder={} open={} state={:?} inherited={}",
        p.project.name,
        p.project.folder,
        p.project.key.is_some(),
        p.header.state,
        p.header.may_include_inherited
    );
    let _ = writeln!(
        out,
        "window: {:?} {} .. {} ({} days)",
        p.window,
        date(p.days[0]),
        date(p.days[p.days.len() - 1]),
        p.days.len()
    );
    let _ = writeln!(out, "total: {}", fmt_tokens(&p.total));
    let _ = writeln!(out, "rows: {}", p.rows.len());
    for (agent, t) in p.by_agent.iter() {
        let _ = writeln!(out, "agent {agent:?}: {}", fmt_tokens(t));
    }
    for (agent, model, t) in p.by_model.iter() {
        let _ = writeln!(out, "model {agent:?} {model:?}: {}", fmt_tokens(t));
    }
    for (d, day) in p.days.iter().zip(p.daily.iter()) {
        if day.iter().any(|(_, n)| *n > 0) {
            let _ = writeln!(out, "day {}: {:?}", date(*d), day);
        }
    }
    out
}

/// An all-projects snapshot as text, for insta.
pub(crate) fn render_all(a: &AllUsage) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "state={:?} inherited={}",
        a.header.state, a.header.may_include_inherited
    );
    let _ = writeln!(out, "total: {}", fmt_tokens(&a.total));
    for (info, t) in a.projects.iter() {
        let _ = writeln!(
            out,
            "project {} folder={} open={}: {}",
            info.name,
            info.folder,
            info.key.is_some(),
            fmt_tokens(t)
        );
    }
    let _ = writeln!(out, "other: {}", fmt_tokens(&a.other));
    for (d, day) in a.days.iter().zip(a.daily.iter()) {
        if day.iter().any(|(_, n)| *n > 0) {
            let _ = writeln!(out, "day {}: {:?}", date(*d), day);
        }
    }
    out
}

/// Checks that `fact_bucket` is the facts summed per bucket (a settled
/// database: no month half archived).
pub(crate) fn assert_buckets(h: &Harness) {
    let conn = rusqlite::Connection::open(h.db_path()).unwrap();
    let rows = |sql: &str| -> Vec<Vec<i64>> {
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], |r| (0..11).map(|i| r.get(i)).collect())
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let facts = rows(
        "SELECT bucket, location, coalesce(project, 0), agent, model, review, sum(input),
                sum(output), sum(cache_read), sum(cache_write), count(*)
         FROM fact GROUP BY bucket, location, coalesce(project, 0), agent, model, review
         ORDER BY bucket, location, coalesce(project, 0), agent, model, review",
    );
    let buckets =
        rows("SELECT * FROM fact_bucket ORDER BY bucket, location, project, agent, model, review");
    assert_eq!(buckets, facts, "fact_bucket is not the sum of the facts");
}

/// Checks that rows add up to the total kind by kind.
pub(crate) fn assert_adds_up(total: &Tokens, rows: impl IntoIterator<Item = Tokens>) {
    let sum = rows.into_iter().fold(Tokens::default(), |s, t| Tokens {
        input: s.input + t.input,
        output: s.output + t.output,
        cache_read: s.cache_read + t.cache_read,
        cache_write: s.cache_write + t.cache_write,
    });
    assert_eq!(&sum, total);
}
