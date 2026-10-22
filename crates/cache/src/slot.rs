//! One partition: its connections, its state, and what each SQLite error
//! does to them ("状态与异常" in the design).
//!
//! Locks, always taken in this order: `session` (read for calls, write to
//! open, close or rebuild), then the session's connection mutexes, then
//! `state`. `state` is never held across IO. Opening a project partition
//! takes the `session` write lock of the partition it swaps out while
//! holding its own: the swapped-out one was opened earlier, so the waits
//! never form a cycle. `Shared::swept` and the pool's lock come last.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, Weak};

use lectern_logging::{Value, error, warn};
use rusqlite::{Connection, ErrorCode};

use crate::codec;
use crate::custom::{self, Attach};
use crate::db::{self, Check, Found, Identity, Op, TOUCH_AFTER_MIN, Work, WriteParams};
use crate::stats::{DisableReason, PartitionKind, PartitionState, PartitionStats};
use crate::tables::{TableOps, TableRead, Tables};
use crate::util::{SqlError, lock, read, write};
use crate::{Dropped, Entry, Key, MAX_KEY, Namespace, Shared, Written, clock, sweep};

/// Writes are not tried for this long after `SQLITE_FULL`.
const FULL_PAUSE_MS: u64 = 60_000;
/// Other IO errors in a row that disable a partition.
const IO_ERRORS_MAX: u32 = 3;
/// Corruptions in one run that disable a partition.
const CORRUPTIONS_MAX: u32 = 2;
/// Undecodable entries remembered for deletion; more are found again later.
const BAD_MAX: usize = 4096;
/// Entries waiting for their access time to be written back (32 KiB of IDs);
/// hits beyond it are not recorded.
pub(crate) const TOUCHED_MAX: usize = 4096;
/// `max_page_count` when no limit is set.
#[cfg(any(test, feature = "test-support"))]
const NO_PAGE_LIMIT: u64 = 4_294_967_294;

pub(crate) type Hook = Arc<dyn Fn() + Send + Sync>;

/// What one write transaction writes.
#[derive(Clone, Copy)]
pub(crate) enum Writes<'a> {
    Entries(&'a [Op<'a>]),
    Tables(&'static Tables, &'a TableOps),
}

pub(crate) struct Slot {
    pub(crate) shared: Arc<Shared>,
    /// This slot, for the pool of open project partitions.
    pub(crate) me: Weak<Slot>,
    kind: PartitionKind,
    path: Option<PathBuf>,
    pub(crate) file: PathBuf,
    /// `Shared::ticks` at the latest call: the least recently used project
    /// partition is swapped out.
    pub(crate) last_used: Arc<AtomicU64>,
    session: RwLock<Option<Session>>,
    state: Mutex<State>,
    io_errors: AtomicU32,
    /// The eviction generation of each custom table namespace a `TableSet`
    /// was taken for. A leaf lock, never held across IO.
    evictions: Mutex<Vec<(&'static str, Arc<AtomicU64>)>>,
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) hooks: Mutex<Hooks>,
}

/// Fault injection for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub(crate) struct Hooks {
    /// Applied to the write connection before every write transaction once
    /// set; `Some(None)` lifts the limit.
    pub(crate) max_page_count: Option<Option<u64>>,
    /// Runs inside every write transaction, just before the commit.
    pub(crate) in_write: Option<Hook>,
}

/// The connections of an open partition. Dropping it closes them.
pub(crate) struct Session {
    /// Unique across partitions and reopenings: a failure handled for one
    /// session never closes the next.
    id: u64,
    shared: Arc<Shared>,
    pub(crate) writer: Mutex<Connection>,
    /// Opened on the first read.
    pub(crate) reader: Mutex<Option<Connection>>,
    /// Namespace IDs of this file, after they were committed or read.
    namespaces: Mutex<Vec<(Namespace, i64)>>,
    /// Entries that failed to decode, deleted by the next write transaction.
    bad: Mutex<Vec<i64>>,
    /// Entries hit with an old access time and touched units, sorted;
    /// written back by the next write transaction, `close`, swapping out,
    /// and `touch` when full.
    touched: Mutex<Vec<i64>>,
    /// Custom tables known to exist in this file, by namespace name and
    /// version, with their namespace IDs.
    tables: Mutex<Vec<(Namespace, i64)>>,
}

impl Session {
    fn tables(&self, ns: &Namespace) -> Option<i64> {
        lock(&self.tables)
            .iter()
            .find(|(known, _)| known.same_version(ns))
            .map(|(_, id)| *id)
    }

    fn namespace(&self, ns: &Namespace) -> Option<i64> {
        lock(&self.namespaces)
            .iter()
            .find(|(known, _)| known.same_version(ns))
            .map(|(_, id)| *id)
    }

    fn remember(&self, ns: Namespace, id: i64) {
        let mut namespaces = lock(&self.namespaces);
        if !namespaces.iter().any(|(known, _)| known.same_version(&ns)) {
            namespaces.push((ns, id));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let reader = lock(&self.reader).is_some();
        self.shared
            .connections
            .fetch_sub(1 + usize::from(reader), Ordering::Relaxed);
        self.shared.pool.release(self.id);
    }
}

/// Records `ids` in a sorted set of at most `TOUCHED_MAX`.
fn touch(set: &mut Vec<i64>, ids: impl IntoIterator<Item = i64>) {
    for id in ids {
        if set.len() >= TOUCHED_MAX {
            return;
        }
        if let Err(at) = set.binary_search(&id) {
            set.insert(at, id);
        }
    }
}

struct State {
    open: bool,
    disabled: Option<DisableReason>,
    /// Clock milliseconds until which writes are dropped.
    suspended_until: Option<u64>,
    corruptions: u32,
    /// `meta.bytes` as last read or written.
    bytes: u64,
    /// The budget for this run: lowered after a full disk.
    budget: u64,
    /// A session was opened in this run: a new file found by a later open
    /// means the data was deleted (swept, or removed by the user).
    opened_before: bool,
}

/// How a SQLite error is handled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Failure {
    Corrupt,
    Full,
    Busy,
    NotWritable,
    TooBig,
    Io,
}

fn classify(e: &rusqlite::Error) -> Failure {
    match e.sqlite_error_code() {
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => Failure::Corrupt,
        Some(ErrorCode::DiskFull) => Failure::Full,
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Failure::Busy,
        Some(ErrorCode::CannotOpen | ErrorCode::ReadOnly | ErrorCode::PermissionDenied) => {
            Failure::NotWritable
        }
        Some(ErrorCode::TooBig) => Failure::TooBig,
        _ => Failure::Io,
    }
}

enum OpenError {
    Dir(io::Error),
    Mismatch,
    Sqlite(rusqlite::Error),
}

impl From<rusqlite::Error> for OpenError {
    fn from(e: rusqlite::Error) -> Self {
        OpenError::Sqlite(e)
    }
}

enum ReadError {
    /// Opening the read connection failed.
    Reader(rusqlite::Error),
    Query(rusqlite::Error),
}

/// The error that disabled a partition, for the log record.
#[derive(Clone, Copy)]
enum Cause<'a> {
    Sqlite(&'a rusqlite::Error),
    Io(&'a io::Error),
}

/// What the partition was doing when it was disabled: the `op` field.
#[derive(Clone, Copy)]
enum Doing {
    /// Creating its directory.
    Mkdir,
    /// Opening or checking the file.
    Open,
    /// A read or write call on the open file.
    Call,
    /// Deleting the files to rebuild them.
    Delete,
}

impl Doing {
    fn name(self) -> &'static str {
        match self {
            Doing::Mkdir => "mkdir",
            Doing::Open => "open",
            Doing::Call => "call",
            Doing::Delete => "delete",
        }
    }
}

impl Slot {
    pub(crate) fn new(
        shared: Arc<Shared>,
        kind: PartitionKind,
        path: Option<PathBuf>,
        file: PathBuf,
    ) -> Arc<Slot> {
        let budget = match kind {
            PartitionKind::App => shared.budgets.app,
            PartitionKind::Repo | PartitionKind::Folder => shared.budgets.project,
        };
        Arc::new_cyclic(|me| Slot {
            shared,
            me: me.clone(),
            kind,
            path,
            file,
            last_used: Arc::new(AtomicU64::new(0)),
            session: RwLock::new(None),
            state: Mutex::new(State {
                open: false,
                disabled: None,
                suspended_until: None,
                corruptions: 0,
                bytes: 0,
                budget,
                opened_before: false,
            }),
            io_errors: AtomicU32::new(0),
            evictions: Mutex::new(Vec::new()),
            #[cfg(any(test, feature = "test-support"))]
            hooks: Mutex::new(Hooks::default()),
        })
    }

    pub(crate) fn kind(&self) -> PartitionKind {
        self.kind
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub(crate) fn stats(&self) -> PartitionStats {
        let now = self.shared.clock.now_ms();
        let st = lock(&self.state);
        let state = if let Some(reason) = st.disabled {
            PartitionState::Disabled { reason }
        } else if let Some(until) = st.suspended_until.filter(|until| now < *until) {
            PartitionState::WritesSuspended {
                until: clock::to_system(until),
            }
        } else if st.open {
            PartitionState::Open
        } else {
            PartitionState::Closed
        };
        PartitionStats {
            kind: self.kind,
            path: self.path.clone(),
            state,
            bytes: st.bytes,
        }
    }

    /// Counts and returns a dropped write.
    pub(crate) fn dropped(&self, reason: Dropped) -> Written {
        self.shared.counters.dropped(reason);
        Written::Dropped(reason)
    }

    /// Closes the connections; waits for calls in progress. With
    /// `write_back`, pending access times are written first (`Cache::close`;
    /// not when the last `Cache` is dropped).
    pub(crate) fn close(&self, write_back: bool) {
        let mut guard = write(&self.session);
        if write_back && let Some(session) = guard.as_ref() {
            self.write_back(session);
        }
        *guard = None;
        lock(&self.state).open = false;
    }

    /// Swaps out session `id` to make room for another project partition:
    /// writes the access times back and closes the connections, after the
    /// calls in progress. Nothing when another session has replaced it.
    fn swap_out(&self, id: u64) {
        let mut guard = write(&self.session);
        if let Some(session) = guard.as_ref().filter(|s| s.id == id) {
            self.write_back(session);
            *guard = None;
            lock(&self.state).open = false;
        }
    }

    /// Writes pending access times in a transaction of their own. Failures
    /// only lose LRU precision; the next open handles a broken file. Returns
    /// whether they were written.
    fn write_back(&self, session: &Session) -> bool {
        let ids = std::mem::take(&mut *lock(&session.touched));
        if ids.is_empty() {
            return true;
        }
        if lock(&self.state).disabled.is_some() {
            return false;
        }
        let atime = (self.shared.clock.now_ms() / 60_000) as i64;
        db::touch(&mut lock(&session.writer), &ids, atime).is_ok()
    }

    /// The eviction generation of custom table namespace `name`.
    pub(crate) fn evictions(&self, name: &'static str) -> Arc<AtomicU64> {
        let mut all = lock(&self.evictions);
        if let Some((_, n)) = all.iter().find(|(known, _)| *known == name) {
            return n.clone();
        }
        let n = Arc::new(AtomicU64::new(0));
        all.push((name, n.clone()));
        n
    }

    /// Units of custom table namespace `name` were deleted.
    fn bump(&self, name: &str) {
        if let Some((_, n)) = lock(&self.evictions)
            .iter()
            .find(|(known, _)| *known == name)
        {
            n.fetch_add(1, Ordering::Release);
        }
    }

    /// The file's data is gone: rebuilt, disabled, or found new.
    fn bump_all(&self) {
        for (_, n) in lock(&self.evictions).iter() {
            n.fetch_add(1, Ordering::Release);
        }
    }

    /// Marks units as used: adds them to the pending access times, writing
    /// those back whenever the set is full. Stops when a write-back fails.
    pub(crate) fn touch_units(&self, mut ids: Vec<i64>) {
        if ids.is_empty() || !self.shared.may_block() || lock(&self.state).disabled.is_some() {
            return;
        }
        self.used();
        let Ok(guard) = self.session() else {
            return;
        };
        let Some(session) = guard.as_ref() else {
            unreachable!("session() returns an open session");
        };
        loop {
            {
                let mut set = lock(&session.touched);
                set.append(&mut ids);
                set.sort_unstable();
                set.dedup();
                if set.len() > TOUCHED_MAX {
                    ids = set.split_off(TOUCHED_MAX);
                }
            }
            if ids.is_empty() || !self.write_back(session) {
                return;
            }
        }
    }

    /// Makes sure the tables of `tables` exist in the session's file; on
    /// the write connection, under its lock. `None` for a registration that
    /// breaks a rule.
    fn attach(
        &self,
        session: &Session,
        tables: &'static Tables,
        conn: &mut Connection,
    ) -> rusqlite::Result<Option<i64>> {
        let name = tables.namespace.name;
        if let Some(ns) = session.tables(&tables.namespace) {
            return Ok(Some(ns));
        }
        match custom::attach(conn, tables)? {
            Attach::Ready { ns, replaced } => {
                let mut known = lock(&session.tables);
                // Another version's tables are gone from the file.
                known.retain(|(n, _)| n.name != name);
                known.push((tables.namespace, ns));
                drop(known);
                if replaced {
                    self.bump(name);
                }
                Ok(Some(ns))
            }
            Attach::Invalid(why) => {
                warn!(
                    "custom tables rejected";
                    namespace = name,
                    rule = why.rule,
                    statement = why.statement,
                    sqlite = why.error.sqlite,
                    error = why.error.error,
                    kind = self.kind.name(),
                    path = Value::path(&self.file),
                    project = self.project(),
                );
                Ok(None)
            }
        }
    }

    /// One read transaction over custom tables.
    pub(crate) fn read_tables<T>(
        &self,
        tables: &'static Tables,
        f: impl FnOnce(&mut TableRead<'_>) -> T,
    ) -> Option<T> {
        if !self.shared.may_block() || lock(&self.state).disabled.is_some() {
            return None;
        }
        self.used();
        let guard = self.session().ok()?;
        let Some(session) = guard.as_ref() else {
            unreachable!("session() returns an open session");
        };
        let id = session.id;
        let attached = if session.tables(&tables.namespace).is_some() {
            Ok(Some(()))
        } else {
            let mut conn = lock(&session.writer);
            self.prepare_writer(&conn)
                .and_then(|_| self.attach(session, tables, &mut conn))
                .map(|ns| ns.map(|_| ()))
        };
        match attached {
            Ok(Some(())) => {}
            Ok(None) => return None,
            Err(e) => {
                drop(guard);
                self.fail(id, &e);
                return None;
            }
        }
        let result = self.query_tables(session, tables, f);
        drop(guard);
        match result {
            Ok(value) => {
                self.succeeded();
                Some(value)
            }
            Err(ReadError::Reader(e)) if classify(&e) == Failure::NotWritable => {
                self.close_session(id);
                None
            }
            Err(ReadError::Reader(e) | ReadError::Query(e)) => {
                if custom::is_statement_error(&e) {
                    let sql = SqlError::of(&e);
                    warn!(
                        "custom table query failed";
                        namespace = tables.namespace.name,
                        sqlite = sql.sqlite,
                        error = sql.error,
                        kind = self.kind.name(),
                        path = Value::path(&self.file),
                        project = self.project(),
                    );
                } else {
                    self.fail(id, &e);
                }
                None
            }
        }
    }

    fn query_tables<T>(
        &self,
        session: &Session,
        tables: &'static Tables,
        f: impl FnOnce(&mut TableRead<'_>) -> T,
    ) -> Result<T, ReadError> {
        let mut reader = lock(&session.reader);
        let conn = self.reader(&mut reader)?;
        let tx = conn.unchecked_transaction().map_err(ReadError::Query)?;
        let mut read = TableRead {
            conn,
            tables,
            failed: None,
        };
        let value = f(&mut read);
        if let Some(e) = read.failed {
            return Err(ReadError::Query(e));
        }
        tx.commit().map_err(ReadError::Query)?;
        Ok(value)
    }

    /// The read connection, opened on first use.
    fn reader<'a>(&self, reader: &'a mut Option<Connection>) -> Result<&'a Connection, ReadError> {
        if reader.is_none() {
            let conn = db::open_reader(&self.file, self.shared.budgets.page_cache)
                .map_err(ReadError::Reader)?;
            *reader = Some(conn);
            self.shared.connections.fetch_add(1, Ordering::Relaxed);
        }
        let Some(conn) = reader.as_ref() else {
            unreachable!("the reader was just opened");
        };
        Ok(conn)
    }

    fn used(&self) {
        let tick = self.shared.ticks.fetch_add(1, Ordering::Relaxed);
        self.last_used.store(tick, Ordering::Relaxed);
    }

    /// Reads entries of one type. Counts a hit or a miss per key.
    pub(crate) fn read<E: Entry>(&self, keys: &[Key]) -> Vec<Option<E>> {
        let counters = &self.shared.counters;
        let mut out: Vec<Option<E>> = keys.iter().map(|_| None).collect();
        let wanted: Vec<usize> = (0..keys.len())
            .filter(|i| keys[*i].len() <= MAX_KEY)
            .collect();
        if !self.shared.may_block() || wanted.is_empty() || lock(&self.state).disabled.is_some() {
            counters.miss(keys.len() as u64);
            return out;
        }
        self.used();
        let Ok(guard) = self.session() else {
            counters.miss(keys.len() as u64);
            return out;
        };
        let Some(session) = guard.as_ref() else {
            unreachable!("session() returns an open session");
        };
        let bytes: Vec<&[u8]> = wanted.iter().map(|i| keys[*i].as_bytes()).collect();
        let found = match self.query(session, &E::NAMESPACE, &bytes) {
            Ok(found) => found,
            Err(e) => {
                let id = session.id;
                drop(guard);
                match e {
                    ReadError::Reader(e) if classify(&e) == Failure::NotWritable => {
                        // The file is gone (deleted while open): open it
                        // afresh next time.
                        self.close_session(id);
                    }
                    ReadError::Reader(e) | ReadError::Query(e) => {
                        self.fail(id, &e);
                    }
                }
                counters.miss(keys.len() as u64);
                return out;
            }
        };
        let now = (self.shared.clock.now_ms() / 60_000) as i64;
        let mut hits = 0;
        let mut stale = Vec::new();
        for (i, row) in wanted.iter().zip(found) {
            let Some(Found { id, atime, raw }) = row else {
                continue;
            };
            match codec::decode::<E>(raw) {
                Some(value) => {
                    out[*i] = Some(value);
                    hits += 1;
                    if now - atime > TOUCH_AFTER_MIN {
                        stale.push(id);
                    }
                }
                None => {
                    counters.decode_failure();
                    let mut bad = lock(&session.bad);
                    if bad.len() < BAD_MAX && !bad.contains(&id) {
                        bad.push(id);
                    }
                }
            }
        }
        if !stale.is_empty() {
            touch(&mut lock(&session.touched), stale);
        }
        drop(guard);
        self.succeeded();
        counters.hit(hits as u64);
        counters.miss((keys.len() - hits) as u64);
        out
    }

    fn query(
        &self,
        session: &Session,
        ns: &Namespace,
        keys: &[&[u8]],
    ) -> Result<Vec<Option<Found>>, ReadError> {
        let mut reader = lock(&session.reader);
        let conn = self.reader(&mut reader)?;
        let id = match session.namespace(ns) {
            Some(id) => id,
            None => match db::namespace_id(conn, ns).map_err(ReadError::Query)? {
                Some(id) => {
                    session.remember(*ns, id);
                    id
                }
                None => return Ok(keys.iter().map(|_| None).collect()),
            },
        };
        db::read(conn, id, keys).map_err(ReadError::Query)
    }

    /// One write transaction.
    pub(crate) fn write(&self, writes: Writes<'_>) -> Written {
        debug_assert!(
            match writes {
                Writes::Entries(ops) => ops.iter().all(|op| op.namespace.is_valid()),
                Writes::Tables(tables, _) => tables.namespace.is_valid(),
            },
            "namespace names use ASCII lowercase letters, digits, '.' and '_'"
        );
        let now = self.shared.clock.now_ms();
        let budget = {
            let mut st = lock(&self.state);
            if st.disabled.is_some() {
                drop(st);
                return self.dropped(Dropped::Unavailable);
            }
            if let Some(until) = st.suspended_until {
                if now < until {
                    drop(st);
                    return self.dropped(Dropped::DiskFull);
                }
                st.suspended_until = None;
            }
            st.budget
        };
        self.used();
        let guard = match self.session() {
            Ok(guard) => guard,
            Err(reason) => return self.dropped(reason),
        };
        let Some(session) = guard.as_ref() else {
            unreachable!("session() returns an open session");
        };
        let known: Vec<Option<i64>> = match writes {
            Writes::Entries(ops) => ops
                .iter()
                .map(|op| session.namespace(&op.namespace))
                .collect(),
            Writes::Tables(..) => Vec::new(),
        };
        let bad: Vec<i64> = lock(&session.bad).clone();
        let touched = std::mem::take(&mut *lock(&session.touched));
        let mut statement_failed = false;
        let result = {
            let mut conn = lock(&session.writer);
            self.prepare_writer(&conn).and_then(|hook| {
                let work = match writes {
                    Writes::Entries(ops) => Work::Entries { ops, known: &known },
                    Writes::Tables(tables, ops) => match self.attach(session, tables, &mut conn)? {
                        Some(ns) => Work::Tables {
                            ns,
                            quota: tables.quota,
                            ops,
                        },
                        None => return Ok(None),
                    },
                };
                let params = WriteParams {
                    work,
                    touched: &touched,
                    bad: &bad,
                    budget,
                    atime: (now / 60_000) as i64,
                };
                db::write(&mut conn, &params, &mut statement_failed, || {
                    if let Some(hook) = &hook {
                        hook();
                    }
                })
                .map(Some)
            })
        };
        match result {
            Ok(None) => {
                // The registration breaks a rule; nothing was tried.
                touch(&mut lock(&session.touched), touched);
                drop(guard);
                self.dropped(Dropped::Io)
            }
            Err(e) if statement_failed => {
                touch(&mut lock(&session.touched), touched);
                drop(guard);
                if let Writes::Tables(tables, _) = writes {
                    let sql = SqlError::of(&e);
                    warn!(
                        "custom table statement failed";
                        namespace = tables.namespace.name,
                        sqlite = sql.sqlite,
                        error = sql.error,
                        kind = self.kind.name(),
                        path = Value::path(&self.file),
                        project = self.project(),
                    );
                }
                self.dropped(Dropped::Io)
            }
            Ok(Some(outcome)) => {
                if !bad.is_empty() {
                    lock(&session.bad).retain(|id| !bad.contains(id));
                }
                for (ns, id) in outcome.namespaces {
                    session.remember(ns, id);
                }
                drop(guard);
                for name in &outcome.deleted_tables {
                    self.bump(name);
                }
                lock(&self.state).bytes = outcome.bytes;
                self.succeeded();
                let counters = &self.shared.counters;
                counters.written();
                if outcome.evicted > 0 {
                    counters.evicted(outcome.evicted);
                }
                Written::Committed
            }
            Err(e) => {
                touch(&mut lock(&session.touched), touched);
                let id = session.id;
                drop(guard);
                let reason = self.fail(id, &e);
                self.dropped(reason)
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn prepare_writer(&self, conn: &Connection) -> rusqlite::Result<Option<Hook>> {
        let hooks = lock(&self.hooks);
        if let Some(limit) = hooks.max_page_count {
            let pages = limit.unwrap_or(NO_PAGE_LIMIT);
            let _: i64 = conn.query_row(&format!("PRAGMA max_page_count = {pages}"), [], |r| {
                r.get(0)
            })?;
        }
        Ok(hooks.in_write.clone())
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn prepare_writer(&self, _conn: &Connection) -> rusqlite::Result<Option<Hook>> {
        Ok(None)
    }

    /// The open session, opening the partition on first use.
    fn session(&self) -> Result<RwLockReadGuard<'_, Option<Session>>, Dropped> {
        loop {
            let guard = read(&self.session);
            if guard.is_some() {
                return Ok(guard);
            }
            drop(guard);
            let mut guard = write(&self.session);
            if guard.is_none() {
                *guard = Some(self.open()?);
            }
        }
    }

    /// Opens the partition under the session's write lock: swaps out the
    /// least recently used project partition when this one would exceed
    /// `Budgets::open_projects`, sweeps the directory on the first open of
    /// the run, then opens the file.
    fn open(&self) -> Result<Session, Dropped> {
        if lock(&self.state).disabled.is_some() {
            return Err(Dropped::Unavailable);
        }
        let shared = &self.shared;
        let id = shared.sessions.fetch_add(1, Ordering::Relaxed);
        if self.kind != PartitionKind::App {
            let cap = shared.budgets.open_projects;
            if let Some((victim, session)) = shared.pool.reserve(self, id, cap)
                && !Weak::ptr_eq(&victim, &self.me)
                && let Some(victim) = victim.upgrade()
            {
                victim.swap_out(session);
            }
        }
        shared.sweep_once();
        let opened = self.open_file(id);
        if opened.is_err() {
            shared.pool.release(id);
        }
        opened
    }

    /// Opens the write connection and checks the file, rebuilding it when it
    /// belongs to something else or is corrupt. Failures are handled here.
    fn open_file(&self, id: u64) -> Result<Session, Dropped> {
        // At most: a mismatch rebuilt, then a corrupt new file rebuilt.
        // A rebuild already moved the eviction generations.
        let mut rebuilt = false;
        for _ in 0..3 {
            let failure = match self.try_open() {
                Ok((writer, bytes, fresh)) => {
                    let lost = {
                        let mut st = lock(&self.state);
                        st.open = true;
                        st.bytes = bytes;
                        let lost = fresh && st.opened_before && !rebuilt;
                        st.opened_before = true;
                        lost
                    };
                    if lost {
                        self.bump_all();
                    }
                    self.succeeded();
                    let shared = self.shared.clone();
                    shared.connections.fetch_add(1, Ordering::Relaxed);
                    return Ok(Session {
                        id,
                        shared,
                        writer: Mutex::new(writer),
                        reader: Mutex::new(None),
                        namespaces: Mutex::new(Vec::new()),
                        bad: Mutex::new(Vec::new()),
                        touched: Mutex::new(Vec::new()),
                        tables: Mutex::new(Vec::new()),
                    });
                }
                Err(OpenError::Mismatch) => {
                    warn!(
                        "partition mismatch; rebuilding";
                        kind = self.kind.name(),
                        path = Value::path(&self.file),
                        project = self.project(),
                    );
                    self.rebuild()?;
                    rebuilt = true;
                    continue;
                }
                Err(OpenError::Dir(e)) => {
                    return Err(self.disable(
                        DisableReason::NotWritable,
                        Doing::Mkdir,
                        Cause::Io(&e),
                    ));
                }
                Err(OpenError::Sqlite(e)) => classify_open(&e, self),
            };
            if let Some(reason) = failure {
                return Err(reason);
            }
            // The file was corrupt and deleted.
            rebuilt = true;
        }
        Err(Dropped::Io)
    }

    /// The write connection, `meta.bytes`, and whether the file is new.
    fn try_open(&self) -> Result<(Connection, u64, bool), OpenError> {
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir).map_err(OpenError::Dir)?;
        }
        let now = self.shared.clock.now_ms();
        let mut conn = db::open_writer(&self.file, self.shared.budgets.page_cache)?;
        let path = self
            .path
            .as_deref()
            .map_or(&[][..], |p| p.as_os_str().as_encoded_bytes());
        let identity = Identity {
            kind: self.kind.name(),
            path,
        };
        match db::check(&conn, &identity)? {
            Check::New => {
                db::create(&mut conn, &identity, now)?;
                Ok((conn, 0, true))
            }
            Check::Ready { bytes } => {
                db::enable_wal(&conn)?;
                if let Err(e) = db::record_opened(&conn, now) {
                    // Only a hint for the directory sweep.
                    if !matches!(classify(&e), Failure::Busy | Failure::Full) {
                        return Err(e.into());
                    }
                }
                Ok((conn, bytes, false))
            }
            Check::Mismatch => Err(OpenError::Mismatch),
        }
    }

    /// Handles a failed call of session `id` and says what the caller sees.
    fn fail(&self, id: u64, e: &rusqlite::Error) -> Dropped {
        match classify(e) {
            Failure::Busy => Dropped::Busy,
            Failure::TooBig => Dropped::TooLarge,
            Failure::Full => {
                self.disk_full();
                Dropped::DiskFull
            }
            Failure::Corrupt => {
                let mut guard = write(&self.session);
                if guard.as_ref().map(|s| s.id) == Some(id) {
                    *guard = None;
                    lock(&self.state).open = false;
                    if let Err(reason) = self.corrupt(e, Doing::Call) {
                        return reason;
                    }
                }
                Dropped::Io
            }
            Failure::NotWritable => {
                let mut guard = write(&self.session);
                *guard = None;
                self.disable(DisableReason::NotWritable, Doing::Call, Cause::Sqlite(e))
            }
            Failure::Io => {
                if self.io_errors.fetch_add(1, Ordering::Relaxed) + 1 >= IO_ERRORS_MAX {
                    let mut guard = write(&self.session);
                    *guard = None;
                    self.disable(DisableReason::Io, Doing::Call, Cause::Sqlite(e))
                } else {
                    Dropped::Io
                }
            }
        }
    }

    /// Records a corruption found while `doing` with the session closed,
    /// deletes the files, and disables the partition the second time.
    fn corrupt(&self, e: &rusqlite::Error, doing: Doing) -> Result<(), Dropped> {
        let corruptions = {
            let mut st = lock(&self.state);
            st.corruptions += 1;
            st.corruptions
        };
        if corruptions >= CORRUPTIONS_MAX {
            let _ = self.remove_files();
            return Err(self.disable(DisableReason::Corrupt, doing, Cause::Sqlite(e)));
        }
        warn!(
            "partition corrupt; rebuilding";
            sqlite = SqlError::of(e).sqlite,
            kind = self.kind.name(),
            path = Value::path(&self.file),
            project = self.project(),
        );
        self.rebuild()
    }

    /// Deletes the three files with the session closed. The caller logs why.
    fn rebuild(&self) -> Result<(), Dropped> {
        self.shared.counters.rebuilt();
        self.bump_all();
        if let Err(e) = self.remove_files() {
            return Err(self.disable(DisableReason::NotWritable, Doing::Delete, Cause::Io(&e)));
        }
        Ok(())
    }

    fn remove_files(&self) -> io::Result<()> {
        sweep::remove(&self.file)
    }

    /// Pauses writes and lowers this run's budget to 75% of the current size.
    fn disk_full(&self) {
        let now = self.shared.clock.now_ms();
        let mut st = lock(&self.state);
        st.suspended_until = Some(now + FULL_PAUSE_MS);
        st.budget = st.budget.min(st.bytes / 4 * 3);
    }

    /// Disables the partition for this run, after `cause` while `doing`.
    /// The session must be closed.
    fn disable(&self, reason: DisableReason, doing: Doing, cause: Cause<'_>) -> Dropped {
        {
            let mut st = lock(&self.state);
            st.open = false;
            if st.disabled.is_none() {
                st.disabled = Some(reason);
            }
        }
        self.bump_all();
        let (sql, io) = match cause {
            Cause::Sqlite(e) => (SqlError::of(e), None),
            Cause::Io(e) => (SqlError::default(), Some(Value::io(e))),
        };
        error!(
            "partition disabled";
            reason = reason.name(),
            op = doing.name(),
            sqlite = sql.sqlite,
            error = sql.error,
            io = io,
            kind = self.kind.name(),
            path = Value::path(&self.file),
            project = self.project(),
        );
        Dropped::Unavailable
    }

    /// Closes session `id` if it is still the open one.
    fn close_session(&self, id: u64) {
        let mut guard = write(&self.session);
        if guard.as_ref().map(|s| s.id) == Some(id) {
            *guard = None;
            lock(&self.state).open = false;
        }
    }

    fn succeeded(&self) {
        if self.io_errors.load(Ordering::Relaxed) != 0 {
            self.io_errors.store(0, Ordering::Relaxed);
        }
    }

    /// The `project` field: the repository's common directory or the folder;
    /// null for the app partition.
    fn project(&self) -> Option<Value<'_>> {
        self.path.as_deref().map(Value::path)
    }
}

#[cfg(test)]
impl Slot {
    /// Runs `f` on the open session's write and read connections.
    pub(crate) fn with_connections<T>(
        &self,
        f: impl FnOnce(&Connection, Option<&Connection>) -> T,
    ) -> Option<T> {
        let guard = read(&self.session);
        let session = guard.as_ref()?;
        let writer = lock(&session.writer);
        let reader = lock(&session.reader);
        Some(f(&writer, reader.as_ref()))
    }

    /// Undecodable entries waiting for the next write transaction.
    pub(crate) fn pending_bad(&self) -> usize {
        read(&self.session)
            .as_ref()
            .map_or(0, |s| lock(&s.bad).len())
    }

    /// Access times waiting to be written back.
    pub(crate) fn pending_touched(&self) -> usize {
        read(&self.session)
            .as_ref()
            .map_or(0, |s| lock(&s.touched).len())
    }
}

/// What a failed open does. `None`: try again (the file was deleted).
fn classify_open(e: &rusqlite::Error, slot: &Slot) -> Option<Dropped> {
    match classify(e) {
        Failure::Corrupt => slot.corrupt(e, Doing::Open).err(),
        Failure::Full => {
            slot.disk_full();
            Some(Dropped::DiskFull)
        }
        Failure::Busy => Some(Dropped::Busy),
        Failure::NotWritable => {
            Some(slot.disable(DisableReason::NotWritable, Doing::Open, Cause::Sqlite(e)))
        }
        Failure::TooBig | Failure::Io => {
            if slot.io_errors.fetch_add(1, Ordering::Relaxed) + 1 >= IO_ERRORS_MAX {
                Some(slot.disable(DisableReason::Io, Doing::Open, Cause::Sqlite(e)))
            } else {
                Some(Dropped::Io)
            }
        }
    }
}
