//! One partition: its connections, its state, and what each SQLite error
//! does to them ("状态与异常" in the design).
//!
//! Locks, always taken in this order: `session` (read for calls, write to
//! open, close or rebuild), then the session's connection mutexes, then
//! `state`. `state` is never held across IO.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};

use rusqlite::{Connection, ErrorCode};

use crate::codec::{self, Raw};
use crate::db::{self, Check, Identity, Op, WriteParams};
use crate::stats::{DisableReason, PartitionKind, PartitionState, PartitionStats};
use crate::util::{lock, log, read, write};
use crate::{Dropped, Entry, Key, MAX_KEY, Namespace, Shared, Written, clock};

/// Writes are not tried for this long after `SQLITE_FULL`.
const FULL_PAUSE_MS: u64 = 60_000;
/// Other IO errors in a row that disable a partition.
const IO_ERRORS_MAX: u32 = 3;
/// Corruptions in one run that disable a partition.
const CORRUPTIONS_MAX: u32 = 2;
/// Undecodable entries remembered for deletion; more are found again later.
const BAD_MAX: usize = 4096;
/// `max_page_count` when no limit is set.
#[cfg(any(test, feature = "test-support"))]
const NO_PAGE_LIMIT: u64 = 4_294_967_294;

pub(crate) type Hook = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct Slot {
    pub(crate) shared: Arc<Shared>,
    kind: PartitionKind,
    path: Option<PathBuf>,
    pub(crate) file: PathBuf,
    session: RwLock<Option<Session>>,
    state: Mutex<State>,
    io_errors: AtomicU32,
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
}

impl Session {
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

impl Slot {
    pub(crate) fn new(
        shared: Arc<Shared>,
        kind: PartitionKind,
        path: Option<PathBuf>,
        file: PathBuf,
    ) -> Slot {
        let budget = match kind {
            PartitionKind::App => shared.budgets.app,
            PartitionKind::Repo | PartitionKind::Folder => shared.budgets.project,
        };
        Slot {
            shared,
            kind,
            path,
            file,
            session: RwLock::new(None),
            state: Mutex::new(State {
                open: false,
                disabled: None,
                suspended_until: None,
                corruptions: 0,
                bytes: 0,
                budget,
            }),
            io_errors: AtomicU32::new(0),
            #[cfg(any(test, feature = "test-support"))]
            hooks: Mutex::new(Hooks::default()),
        }
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

    /// Closes the connections; waits for calls in progress.
    pub(crate) fn close(&self) {
        let mut session = write(&self.session);
        *session = None;
        lock(&self.state).open = false;
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
        let mut hits = 0;
        for (i, row) in wanted.iter().zip(found) {
            let Some((id, raw)) = row else {
                continue;
            };
            match codec::decode::<E>(raw) {
                Some(value) => {
                    out[*i] = Some(value);
                    hits += 1;
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
    ) -> Result<Vec<Option<(i64, Raw)>>, ReadError> {
        let mut reader = lock(&session.reader);
        if reader.is_none() {
            let conn = db::open_reader(&self.file, self.shared.budgets.page_cache)
                .map_err(ReadError::Reader)?;
            *reader = Some(conn);
            self.shared.connections.fetch_add(1, Ordering::Relaxed);
        }
        let Some(conn) = reader.as_ref() else {
            unreachable!("the reader was just opened");
        };
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
    pub(crate) fn write(&self, ops: &[Op<'_>]) -> Written {
        debug_assert!(
            ops.iter().all(|op| op.namespace.is_valid()),
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
        let guard = match self.session() {
            Ok(guard) => guard,
            Err(reason) => return self.dropped(reason),
        };
        let Some(session) = guard.as_ref() else {
            unreachable!("session() returns an open session");
        };
        let known: Vec<Option<i64>> = ops
            .iter()
            .map(|op| session.namespace(&op.namespace))
            .collect();
        let bad: Vec<i64> = lock(&session.bad).clone();
        let result = {
            let mut conn = lock(&session.writer);
            self.prepare_writer(&conn).and_then(|hook| {
                let params = WriteParams {
                    ops,
                    known: &known,
                    bad: &bad,
                    budget,
                    atime: (now / 60_000) as i64,
                };
                db::write(&mut conn, &params, || {
                    if let Some(hook) = &hook {
                        hook();
                    }
                })
            })
        };
        match result {
            Ok(outcome) => {
                if !bad.is_empty() {
                    lock(&session.bad).retain(|id| !bad.contains(id));
                }
                for (ns, id) in outcome.namespaces {
                    session.remember(ns, id);
                }
                drop(guard);
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

    /// Opens the write connection and checks the file, rebuilding it when it
    /// belongs to something else or is corrupt. Runs under the session's
    /// write lock; failures are handled here.
    fn open(&self) -> Result<Session, Dropped> {
        if lock(&self.state).disabled.is_some() {
            return Err(Dropped::Unavailable);
        }
        // At most: a mismatch rebuilt, then a corrupt new file rebuilt.
        for _ in 0..3 {
            let failure = match self.try_open() {
                Ok((writer, bytes)) => {
                    {
                        let mut st = lock(&self.state);
                        st.open = true;
                        st.bytes = bytes;
                    }
                    self.succeeded();
                    let shared = self.shared.clone();
                    shared.connections.fetch_add(1, Ordering::Relaxed);
                    return Ok(Session {
                        id: shared.sessions.fetch_add(1, Ordering::Relaxed),
                        shared,
                        writer: Mutex::new(writer),
                        reader: Mutex::new(None),
                        namespaces: Mutex::new(Vec::new()),
                        bad: Mutex::new(Vec::new()),
                    });
                }
                Err(OpenError::Mismatch) => {
                    self.rebuild("is from another version or partition")?;
                    continue;
                }
                Err(OpenError::Dir(e)) => {
                    return Err(self.disable(
                        DisableReason::NotWritable,
                        format_args!("cannot create the directory of {}: {e}", self.describe()),
                    ));
                }
                Err(OpenError::Sqlite(e)) => classify_open(&e, self),
            };
            if let Some(reason) = failure {
                return Err(reason);
            }
        }
        Err(Dropped::Io)
    }

    fn try_open(&self) -> Result<(Connection, u64), OpenError> {
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
                Ok((conn, 0))
            }
            Check::Ready { bytes } => {
                db::enable_wal(&conn)?;
                if let Err(e) = db::record_opened(&conn, now) {
                    // Only a hint for the directory sweep.
                    if !matches!(classify(&e), Failure::Busy | Failure::Full) {
                        return Err(e.into());
                    }
                }
                Ok((conn, bytes))
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
                    if let Err(reason) = self.corrupt(e) {
                        return reason;
                    }
                }
                Dropped::Io
            }
            Failure::NotWritable => {
                let mut guard = write(&self.session);
                *guard = None;
                self.disable(
                    DisableReason::NotWritable,
                    format_args!("{} is not writable: {e}", self.describe()),
                )
            }
            Failure::Io => {
                if self.io_errors.fetch_add(1, Ordering::Relaxed) + 1 >= IO_ERRORS_MAX {
                    let mut guard = write(&self.session);
                    *guard = None;
                    self.disable(
                        DisableReason::Io,
                        format_args!("{} failed {IO_ERRORS_MAX} times: {e}", self.describe()),
                    )
                } else {
                    Dropped::Io
                }
            }
        }
    }

    /// Records a corruption with the session closed, deletes the files, and
    /// disables the partition the second time.
    fn corrupt(&self, e: &rusqlite::Error) -> Result<(), Dropped> {
        let corruptions = {
            let mut st = lock(&self.state);
            st.corruptions += 1;
            st.corruptions
        };
        if corruptions >= CORRUPTIONS_MAX {
            let _ = self.remove_files();
            return Err(self.disable(
                DisableReason::Corrupt,
                format_args!("{} is corrupt again: {e}", self.describe()),
            ));
        }
        self.rebuild(&format!("is corrupt ({e})"))
    }

    /// Deletes the three files with the session closed.
    fn rebuild(&self, why: &str) -> Result<(), Dropped> {
        log(format_args!("{} {why}; rebuilding", self.describe()));
        self.shared.counters.rebuilt();
        if let Err(e) = self.remove_files() {
            return Err(self.disable(
                DisableReason::NotWritable,
                format_args!("cannot delete {}: {e}", self.file.display()),
            ));
        }
        Ok(())
    }

    fn remove_files(&self) -> io::Result<()> {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = self.file.clone().into_os_string();
            name.push(suffix);
            match fs::remove_file(&name) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Pauses writes and lowers this run's budget to 75% of the current size.
    fn disk_full(&self) {
        let now = self.shared.clock.now_ms();
        let mut st = lock(&self.state);
        st.suspended_until = Some(now + FULL_PAUSE_MS);
        st.budget = st.budget.min(st.bytes / 4 * 3);
    }

    /// Disables the partition for this run. The session must be closed.
    fn disable(&self, reason: DisableReason, why: std::fmt::Arguments<'_>) -> Dropped {
        {
            let mut st = lock(&self.state);
            st.open = false;
            if st.disabled.is_none() {
                st.disabled = Some(reason);
            }
        }
        log(format_args!("{why}; disabled for this run"));
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

    fn describe(&self) -> String {
        match &self.path {
            Some(path) => format!("{} partition of {}", self.kind.name(), path.display()),
            None => "app partition".to_owned(),
        }
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
}

/// What a failed open does. `None`: try again (the file was deleted).
fn classify_open(e: &rusqlite::Error, slot: &Slot) -> Option<Dropped> {
    match classify(e) {
        Failure::Corrupt => slot.corrupt(e).err(),
        Failure::Full => {
            slot.disk_full();
            Some(Dropped::DiskFull)
        }
        Failure::Busy => Some(Dropped::Busy),
        Failure::NotWritable => Some(slot.disable(
            DisableReason::NotWritable,
            format_args!("cannot open {}: {e}", slot.describe()),
        )),
        Failure::TooBig | Failure::Io => {
            if slot.io_errors.fetch_add(1, Ordering::Relaxed) + 1 >= IO_ERRORS_MAX {
                Some(slot.disable(
                    DisableReason::Io,
                    format_args!("cannot open {}: {e}", slot.describe()),
                ))
            } else {
                Some(Dropped::Io)
            }
        }
    }
}
