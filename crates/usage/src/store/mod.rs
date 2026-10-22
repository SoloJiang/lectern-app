//! This crate's own SQLite database (docs/design/usage.md「数据库」, ADR 0027):
//! `<data_dir>/usage.db`, never in `cache`'s directory and never deleted to
//! rebuild. Two connections, a writer and a reader, both used only inside
//! executor tasks; the writer is behind a lock.

pub(crate) mod cold;
mod dedup;
mod ingest;
mod query;
mod recover;
pub(crate) mod schema;
mod segment;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lectern_logging::{Value, error, info, warn};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};

pub(crate) use dedup::FactDelta;
pub(crate) use ingest::{FileStamp, Ingest, Ingested, Place, SessionInfo, agent_of, path_bytes};
pub(crate) use query::{FolderRoot, ProjectRow, Totals, under};
pub(crate) use recover::Recovery;
#[cfg(test)]
pub(crate) use recover::{SALVAGE_FILE, Step, fail_at};

use schema::{APPLICATION_ID, CURRENT, DERIVED_VERSION, Schema};

/// The database file in the data directory.
pub(crate) const DB_FILE: &str = "usage.db";

/// Prepared statements kept per connection.
const STATEMENT_CACHE: usize = 48;

/// `PRAGMA cache_size` of each connection: 1 MiB, two connections ≤ 2 MiB
/// (ADR 0027).
pub(crate) const CACHE_KIB: i64 = 1024;

/// What the database accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Writable,
    /// Written by a newer version: read only.
    Newer,
    /// The backup before a migration could not be written, or a write
    /// failed: read only until a write succeeds again (the latter only).
    Unwritable(Arc<str>),
}

/// Why a store operation failed.
#[derive(Debug)]
pub(crate) enum StoreError {
    Sqlite(rusqlite::Error),
    /// The database is read only (`Mode::Newer`, or a failed backup).
    ReadOnly,
    /// The connections are being replaced (recovery).
    Closed,
}

impl StoreError {
    pub(crate) fn is_corrupt(&self) -> bool {
        matches!(self, StoreError::Sqlite(e) if is_corrupt(e))
    }

    /// The SQLite extended code, for logs.
    pub(crate) fn code(&self) -> i64 {
        match self {
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(f, _)) => i64::from(f.extended_code),
            StoreError::Sqlite(_) => -1,
            StoreError::ReadOnly => -2,
            StoreError::Closed => -3,
        }
    }

    /// A short reason for `UsageDegraded::Unwritable`: the SQLite code name
    /// only, never a message (messages can quote values).
    pub(crate) fn reason(&self) -> Arc<str> {
        match self {
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(f, _)) => {
                format!("sqlite {:?} ({})", f.code, f.extended_code).into()
            }
            StoreError::Sqlite(_) => "sqlite".into(),
            StoreError::ReadOnly => "read only".into(),
            StoreError::Closed => "closed".into(),
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

/// `SQLITE_CORRUPT` or `SQLITE_NOTADB`.
pub(crate) fn is_corrupt(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == ErrorCode::DatabaseCorrupt || f.code == ErrorCode::NotADatabase
    )
}

fn not_a_database() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
        None,
    )
}

/// Why opening failed for good.
#[derive(Debug)]
pub(crate) enum OpenFailure {
    /// The data directory could not be created.
    Dir(io::Error),
    Sqlite(rusqlite::Error),
}

impl OpenFailure {
    pub(crate) fn reason(&self) -> Arc<str> {
        match self {
            OpenFailure::Dir(e) => format!("data directory: {:?}", e.kind()).into(),
            OpenFailure::Sqlite(e) => StoreError::Sqlite(clone_error(e)).reason(),
        }
    }
}

fn clone_error(e: &rusqlite::Error) -> rusqlite::Error {
    match e {
        rusqlite::Error::SqliteFailure(f, _) => {
            rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(f.extended_code), None)
        }
        _ => rusqlite::Error::InvalidQuery,
    }
}

/// The result of opening.
pub(crate) struct Opened {
    pub(crate) store: Store,
    /// The last run did not close the database: run [`Store::check`] once
    /// idle.
    pub(crate) needs_check: bool,
    /// The file was corrupt and has been salvaged.
    pub(crate) recovered: Option<Recovery>,
    /// The version migrated from.
    pub(crate) migrated_from: Option<i32>,
}

/// What [`Store::check`] found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Checked {
    Ok,
    /// Only an index was damaged; `REINDEX` fixed it.
    Reindexed,
    Recovered(Recovery),
}

pub(crate) struct Store {
    dir: PathBuf,
    schema: &'static Schema,
    writer: Mutex<Option<Connection>>,
    reader: Mutex<Option<Connection>>,
    mode: Mutex<Mode>,
    /// Calls that reached SQLite or the data directory; tests check the UI
    /// thread never adds to it.
    io: AtomicU64,
    /// Cold segments decoded so far (`cold_part`); the window snapshots never
    /// add to it.
    cold_reads: AtomicU64,
    /// The largest uncompressed segment the archive writes.
    segment_bytes: AtomicUsize,
    #[cfg(test)]
    pub(crate) faults: Mutex<Faults>,
}

/// Failures injected by tests.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Faults {
    /// Fail the next ingest after its records, before the cursor.
    pub(crate) before_cursor: bool,
    /// Fail the next archive step at this point of its transaction.
    pub(crate) archive: Option<cold::Fault>,
    /// The next archive step's check finds the decoded segment different.
    pub(crate) mismatch: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Store {
    /// Opens (creating, migrating or salvaging) `<dir>/usage.db`. Blocking;
    /// executor only.
    pub(crate) fn open(dir: &Path, now_ms: i64) -> Result<Opened, OpenFailure> {
        Store::open_with(dir, &CURRENT, now_ms)
    }

    pub(crate) fn open_with(
        dir: &Path,
        schema: &'static Schema,
        now_ms: i64,
    ) -> Result<Opened, OpenFailure> {
        fs::create_dir_all(dir).map_err(OpenFailure::Dir)?;
        let file = dir.join(DB_FILE);
        // A salvage was cut short: `usage.db` is still the corrupt original.
        let interrupted = recover::discard_leftover(dir).map_err(OpenFailure::Dir)?;
        let salvage = || recover::salvage(dir, schema, now_ms).map_err(OpenFailure::Sqlite);
        let (files, recovered) = if interrupted && file.exists() {
            warn!("salvage was interrupted; salvaging again"; path = Value::path(&file));
            let (files, recovery) = salvage()?;
            (files, Some(recovery))
        } else {
            match open_files(dir, &file, schema) {
                Ok(files) => (files, None),
                Err(e) if is_corrupt(&e) => {
                    error!("database corrupt at open"; path = Value::path(&file), sqlite = code_of(&e));
                    let (files, recovery) = salvage()?;
                    (files, Some(recovery))
                }
                Err(e) => return Err(OpenFailure::Sqlite(e)),
            }
        };
        info!(
            "database opened";
            path = Value::path(&file),
            version = schema.version,
            migrated_from = files.migrated_from,
            writable = files.mode == Mode::Writable
        );
        let needs_check = files.needs_check && recovered.is_none();
        let migrated_from = files.migrated_from;
        let store = Store {
            dir: dir.to_owned(),
            schema,
            writer: Mutex::new(Some(files.writer)),
            reader: Mutex::new(Some(files.reader)),
            mode: Mutex::new(files.mode),
            io: AtomicU64::new(1),
            cold_reads: AtomicU64::new(0),
            segment_bytes: AtomicUsize::new(cold::SEGMENT_BYTES),
            #[cfg(test)]
            faults: Mutex::new(Faults::default()),
        };
        if recovered.is_some() {
            // A salvaged file was not closed by anyone yet.
            let _ = store.write(|c| set_meta(c, "clean", 0));
        }
        Ok(Opened {
            store,
            needs_check,
            recovered,
            migrated_from,
        })
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn mode(&self) -> Mode {
        lock(&self.mode).clone()
    }

    /// A write failed (not corruption): read only until the next write
    /// succeeds.
    pub(crate) fn set_unwritable(&self, reason: Arc<str>) {
        let mut mode = lock(&self.mode);
        if *mode == Mode::Writable {
            *mode = Mode::Unwritable(reason);
        }
    }

    /// Calls that touched SQLite or the data directory so far.
    #[cfg(test)]
    pub(crate) fn io_count(&self) -> u64 {
        self.io.load(Ordering::Relaxed)
    }

    /// Runs `f` on the writer. Refused while read only for a newer version
    /// or after a failed backup; after a failed write it is tried again.
    pub(crate) fn write<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        match self.mode() {
            Mode::Newer => return Err(StoreError::ReadOnly),
            Mode::Unwritable(_) if self.backup_failed() => return Err(StoreError::ReadOnly),
            _ => {}
        }
        self.io.fetch_add(1, Ordering::Relaxed);
        let mut writer = lock(&self.writer);
        let conn = writer.as_mut().ok_or(StoreError::Closed)?;
        let out = f(conn).map_err(StoreError::Sqlite)?;
        let mut mode = lock(&self.mode);
        if matches!(*mode, Mode::Unwritable(_)) {
            *mode = Mode::Writable;
        }
        Ok(out)
    }

    /// Runs `f` on the reader.
    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        self.io.fetch_add(1, Ordering::Relaxed);
        let reader = lock(&self.reader);
        let conn = reader.as_ref().ok_or(StoreError::Closed)?;
        f(conn).map_err(StoreError::Sqlite)
    }

    fn backup_failed(&self) -> bool {
        matches!(self.mode(), Mode::Unwritable(r) if r.starts_with(BACKUP_FAILED))
    }

    /// Marks the database closed normally. Called when the plugin stops;
    /// skipped when the writer is busy (the next run then checks it).
    pub(crate) fn close(&self) {
        if self.mode() != Mode::Writable {
            return;
        }
        if let Ok(mut writer) = self.writer.try_lock()
            && let Some(conn) = writer.as_mut()
        {
            self.io.fetch_add(1, Ordering::Relaxed);
            let _ = set_meta(conn, "clean", 1);
        }
    }

    /// `PRAGMA quick_check` after an unclean shutdown; on failure `REINDEX`,
    /// and when that does not help, a salvage into a new file. Blocking.
    pub(crate) fn check(&self, now_ms: i64) -> Result<Checked, StoreError> {
        self.io.fetch_add(1, Ordering::Relaxed);
        if self.mode() == Mode::Newer {
            return Ok(Checked::Ok);
        }
        let quick = {
            let writer = lock(&self.writer);
            let conn = writer.as_ref().ok_or(StoreError::Closed)?;
            quick_check(conn)
        };
        if quick {
            return Ok(Checked::Ok);
        }
        warn!("quick_check failed"; path = Value::path(&self.dir.join(DB_FILE)));
        self.repair(now_ms)
    }

    /// `REINDEX`, then a salvage if that is not enough. Called by `check`
    /// and when an operation reports corruption.
    pub(crate) fn repair(&self, now_ms: i64) -> Result<Checked, StoreError> {
        self.io.fetch_add(1, Ordering::Relaxed);
        // A newer version's file is never rewritten, not even to salvage it.
        if self.mode() == Mode::Newer {
            return Err(StoreError::ReadOnly);
        }
        let mut writer = lock(&self.writer);
        let mut reader = lock(&self.reader);
        if let Some(conn) = writer.as_ref()
            && conn.execute_batch("REINDEX;").is_ok()
            && quick_check(conn)
        {
            info!("reindex repaired the database"; path = Value::path(&self.dir.join(DB_FILE)));
            return Ok(Checked::Reindexed);
        }
        // Close both connections before the files are renamed.
        *reader = None;
        *writer = None;
        let (files, recovery) = recover::salvage(&self.dir, self.schema, now_ms)?;
        let _ = set_meta(&files.writer, "clean", 0);
        *writer = Some(files.writer);
        *reader = Some(files.reader);
        *lock(&self.mode) = files.mode;
        drop((writer, reader));
        Ok(Checked::Recovered(recovery))
    }

    /// Whether any source still waits for its record to be read again after
    /// a salvage. Ingest asks the same inside its own transaction.
    #[cfg(test)]
    pub(crate) fn repairing(&self) -> bool {
        self.read(any_repaired).unwrap()
    }

    /// Cold segments decoded so far.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn cold_reads(&self) -> u64 {
        self.cold_reads.load(Ordering::Relaxed)
    }

    /// The largest uncompressed segment of later archive steps (tests use
    /// small ones to get several segments a month).
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn set_segment_bytes(&self, bytes: usize) {
        self.segment_bytes.store(bytes.max(1), Ordering::Relaxed);
    }

    /// The settings of both connections, one line each, for the page cache
    /// test.
    #[cfg(test)]
    pub(crate) fn pragmas(&self) -> String {
        let read = |conn: &Connection| -> String {
            let get = |name: &str| -> String {
                conn.query_row(&format!("PRAGMA {name}"), [], |r| {
                    r.get::<_, rusqlite::types::Value>(0)
                })
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|e| format!("error {e}"))
            };
            [
                "cache_size",
                "mmap_size",
                "journal_mode",
                "synchronous",
                "foreign_keys",
                "query_only",
                "auto_vacuum",
                "application_id",
                "user_version",
            ]
            .iter()
            .map(|n| format!("{n} = {}", get(n)))
            .collect::<Vec<_>>()
            .join("\n")
        };
        let writer = lock(&self.writer);
        let reader = lock(&self.reader);
        format!(
            "writer:\n{}\nreader:\n{}\n",
            read(writer.as_ref().unwrap()),
            read(reader.as_ref().unwrap())
        )
    }

    /// Caps the writer's file size (pages), to make writes fail with
    /// `SQLITE_FULL`.
    #[cfg(test)]
    pub(crate) fn set_max_page_count(&self, pages: u64) {
        let writer = lock(&self.writer);
        let _: i64 = writer
            .as_ref()
            .unwrap()
            .query_row(&format!("PRAGMA max_page_count = {pages}"), [], |r| {
                r.get(0)
            })
            .unwrap();
    }

    /// Runs `f` inside an immediate transaction on the writer and commits.
    pub(crate) fn transaction<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        self.write(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let out = f(&tx)?;
            tx.commit()?;
            Ok(out)
        })
    }
}

/// The reason prefix of a failed backup: the file stays read only.
pub(crate) const BACKUP_FAILED: &str = "backup before migration failed";

/// Both connections of an opened file, with what opening found.
pub(crate) struct Files {
    pub(crate) writer: Connection,
    pub(crate) reader: Connection,
    pub(crate) mode: Mode,
    pub(crate) needs_check: bool,
    pub(crate) migrated_from: Option<i32>,
}

fn code_of(e: &rusqlite::Error) -> i64 {
    StoreError::Sqlite(clone_error(e)).code()
}

/// Opens `file` with both connections, creating or migrating the tables.
fn open_files(dir: &Path, file: &Path, schema: &Schema) -> rusqlite::Result<Files> {
    let existed = file.exists();
    let mut writer = open_connection(file, true)?;
    let application_id: i32 = writer.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let version: i32 = writer.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let mut mode = Mode::Writable;
    let mut migrated_from = None;
    let mut needs_check = false;
    if application_id == 0 && version == 0 {
        let tables: i64 =
            writer.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
        if tables != 0 {
            return Err(not_a_database());
        }
        create(&mut writer, schema)?;
    } else if application_id != APPLICATION_ID {
        return Err(not_a_database());
    } else {
        if version > schema.version {
            warn!("database written by a newer version"; version = version, known = schema.version);
            mode = Mode::Newer;
        } else if version < schema.version {
            match migrate(&mut writer, dir, schema, version) {
                Ok(()) => migrated_from = Some(version),
                Err(Migrate::Backup(e)) => {
                    error!("backup before migration failed"; from = version, to = schema.version, io = Value::io(&e));
                    mode = Mode::Unwritable(format!("{BACKUP_FAILED}: {:?}", e.kind()).into());
                }
                Err(Migrate::Sqlite(e)) => {
                    error!("migration failed"; from = version, to = schema.version, sqlite = code_of(&e));
                    return Err(e);
                }
            }
        }
        if existed {
            let clean: Option<i64> = writer
                .query_row("SELECT value FROM meta WHERE name = 'clean'", [], |r| {
                    r.get(0)
                })
                .optional()?;
            needs_check = clean != Some(1);
        }
    }
    if mode == Mode::Writable {
        set_meta(&writer, "clean", 0)?;
        let derived: Option<i64> = writer
            .query_row("SELECT value FROM meta WHERE name = 'derived'", [], |r| {
                r.get(0)
            })
            .optional()?;
        if derived != Some(DERIVED_VERSION) {
            info!("rebuilding derived tables"; from = derived, to = DERIVED_VERSION);
            dedup::rebuild(&mut writer)?;
            set_meta(&writer, "derived", DERIVED_VERSION)?;
        }
    }
    let reader = open_connection(file, false)?;
    reader.execute_batch("PRAGMA query_only = ON;")?;
    Ok(Files {
        writer,
        reader,
        mode,
        needs_check,
        migrated_from,
    })
}

/// A connection with this crate's settings. `journal_mode` is persistent and
/// set when the file is created.
pub(crate) fn open_connection(file: &Path, create: bool) -> rusqlite::Result<Connection> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = Connection::open_with_flags(file, flags)?;
    configure(&conn)?;
    Ok(conn)
}

/// The settings of both connections (docs/design/usage.md「数据库」): WAL,
/// `synchronous = FULL` (metering commits before a request is forwarded),
/// 1 MiB of page cache each, no memory-mapped IO, no shared cache, foreign
/// keys on.
pub(crate) fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    conn.busy_timeout(Duration::from_millis(1000))?;
    conn.execute_batch(&format!(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = FULL;
         PRAGMA cache_size = -{CACHE_KIB};
         PRAGMA mmap_size = 0;
         PRAGMA trusted_schema = OFF;
         PRAGMA cell_size_check = ON;"
    ))
}

/// Creates the tables of an empty file.
pub(crate) fn create(conn: &mut Connection, schema: &Schema) -> rusqlite::Result<()> {
    // Before the first table, and before WAL writes the header.
    conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")?;
    let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(schema.create)?;
    tx.execute_batch(&format!(
        "PRAGMA application_id = {APPLICATION_ID};
         PRAGMA user_version = {};",
        schema.version
    ))?;
    set_meta(&tx, "derived", DERIVED_VERSION)?;
    set_meta(&tx, "clean", 0)?;
    tx.commit()
}

pub(crate) fn set_meta(conn: &Connection, name: &str, value: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO meta (name, value) VALUES (?1, ?2)
         ON CONFLICT (name) DO UPDATE SET value = excluded.value",
    )?
    .execute(params![name, value])?;
    Ok(())
}

/// Some source still carries a salvage's `repaired` mark (partial index).
pub(crate) fn any_repaired(conn: &Connection) -> rusqlite::Result<bool> {
    conn.prepare_cached("SELECT EXISTS (SELECT 1 FROM usage_source WHERE repaired = 1)")?
        .query_row([], |r| r.get(0))
}

pub(crate) fn quick_check(conn: &Connection) -> bool {
    let result: rusqlite::Result<String> =
        conn.query_row("PRAGMA quick_check(1)", [], |r| r.get(0));
    matches!(result.as_deref(), Ok("ok"))
}

enum Migrate {
    Backup(io::Error),
    Sqlite(rusqlite::Error),
}

/// Writes the backup, then applies each step in one transaction.
fn migrate(conn: &mut Connection, dir: &Path, schema: &Schema, from: i32) -> Result<(), Migrate> {
    backup(conn, dir, from).map_err(Migrate::Backup)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(Migrate::Sqlite)?;
    let mut at = from;
    while at < schema.version {
        let Some(step) = schema.migrations.iter().find(|m| m.from == at) else {
            return Err(Migrate::Sqlite(rusqlite::Error::InvalidQuery));
        };
        tx.execute_batch(step.sql).map_err(Migrate::Sqlite)?;
        at += 1;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {};", schema.version))
        .map_err(Migrate::Sqlite)?;
    tx.commit().map_err(Migrate::Sqlite)?;
    info!("database migrated"; from = from, to = schema.version);
    Ok(())
}

/// The prefix of the copies written before a migration.
pub(crate) const BACKUP_PREFIX: &str = "usage.db.pre-v";

/// `VACUUM INTO '<dir>/usage.db.pre-v<from>'`, synced to disk, keeping the
/// two most recent copies.
fn backup(conn: &Connection, dir: &Path, from: i32) -> io::Result<()> {
    let path = dir.join(format!("{BACKUP_PREFIX}{from}"));
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let target = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path"))?;
    conn.execute("VACUUM INTO ?1", params![target])
        .map_err(|_| io::Error::other("vacuum into"))?;
    fs::File::open(&path)?.sync_all()?;
    sync_dir(dir);
    prune_backups(dir);
    Ok(())
}

/// `fsync` of a directory, best effort as SQLite does it: some file systems
/// refuse it, and that must not stop a migration or a salvage.
pub(crate) fn sync_dir(dir: &Path) {
    if let Err(e) = fs::File::open(dir).and_then(|f| f.sync_all()) {
        warn!("directory sync failed"; path = Value::path(dir), io = Value::io(&e));
    }
}

fn prune_backups(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut versions: Vec<(i32, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let v: i32 = name.to_str()?.strip_prefix(BACKUP_PREFIX)?.parse().ok()?;
            Some((v, e.path()))
        })
        .collect();
    versions.sort_by_key(|(v, _)| std::cmp::Reverse(*v));
    for (_, path) in versions.into_iter().skip(2) {
        let _ = fs::remove_file(path);
    }
}
