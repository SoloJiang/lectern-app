//! The SQLite file: connections, schema, identity check, and the statements
//! of reads and write transactions. Error handling is in `slot`.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::Namespace;
use crate::codec::Raw;
use crate::custom;
use crate::tables::TableOps;

/// `PRAGMA application_id`: 0x4C435443, "LCTC".
pub(crate) const APPLICATION_ID: i32 = 0x4C43_5443;
/// `PRAGMA user_version`. Bumped when the tables change; older and newer
/// files are rebuilt, never migrated. 2: the `custom` table and foreign keys
/// (custom tables).
pub(crate) const SCHEMA_VERSION: i32 = 2;

/// Prepared statements kept per connection (rusqlite's `cache` feature):
/// this crate's own and those of the custom tables' users.
pub(crate) const STATEMENT_CACHE: usize = 32;
/// A hit or touch on an entry whose access time is older than this many
/// minutes has its access time written back; younger ones are left alone.
pub(crate) const TOUCH_AFTER_MIN: i64 = 60;
/// Bytes counted for an entry beyond its key and value.
pub(crate) const ENTRY_OVERHEAD: u64 = 64;
/// Most bytes one write transaction deletes to get under the budget: what
/// it wrote, but at least `EVICT_MIN` and at most `EVICT_MAX`. While over
/// the budget a partition never grows, and the cost of eviction follows the
/// cost of the write.
pub(crate) const EVICT_MIN: u64 = 1 << 20;
pub(crate) const EVICT_MAX: u64 = 16 << 20;
/// The same for a custom namespace over its quota, at least this much: its
/// units carry rows, so a byte costs more to delete than a byte of entries.
pub(crate) const QUOTA_EVICT_MIN: u64 = 256 << 10;
/// Most entries of retired namespace versions one write transaction deletes.
pub(crate) const RETIRED_MAX: i64 = 256;
/// Eviction stops at this share of the budget, in tenths.
const EVICT_TARGET_TENTHS: u64 = 9;
/// Free pages beyond these bytes are returned to the file system.
pub(crate) const VACUUM_FREE: u64 = 32 << 20;
/// Most pages one write transaction returns (2 MiB of 4 KiB pages).
pub(crate) const VACUUM_PAGES: u32 = 512;

const SCHEMA: &str = "
CREATE TABLE meta (
  name  TEXT PRIMARY KEY NOT NULL,
  value BLOB NOT NULL
) WITHOUT ROWID;

CREATE TABLE namespace (
  id      INTEGER PRIMARY KEY,
  name    TEXT NOT NULL,
  version INTEGER NOT NULL,
  retired INTEGER NOT NULL DEFAULT 0,
  UNIQUE (name, version)
);

CREATE TABLE entry (
  id    INTEGER PRIMARY KEY,
  ns    INTEGER NOT NULL,
  key   BLOB NOT NULL,
  codec INTEGER NOT NULL,
  size  INTEGER NOT NULL,
  atime INTEGER NOT NULL,
  value BLOB NOT NULL,
  UNIQUE (ns, key)
);
CREATE INDEX entry_atime ON entry (atime);

CREATE TABLE custom (
  ns      INTEGER PRIMARY KEY,
  name    TEXT NOT NULL UNIQUE,
  version INTEGER NOT NULL,
  bytes   INTEGER NOT NULL,
  tables  TEXT NOT NULL
);
";

/// What a partition file must say about itself in `meta`.
pub(crate) struct Identity<'a> {
    pub(crate) kind: &'static str,
    /// Path bytes; empty for the app partition.
    pub(crate) path: &'a [u8],
}

/// The result of checking an opened file.
pub(crate) enum Check {
    /// An empty file: no header, no tables.
    New,
    /// A file of this schema and this partition, with its `meta.bytes`.
    Ready { bytes: u64 },
    /// Another application, another schema version, another partition, or
    /// missing tables: delete and rebuild.
    Mismatch,
}

/// One write of a write transaction.
pub(crate) struct Op<'a> {
    pub(crate) namespace: Namespace,
    pub(crate) key: &'a [u8],
    /// `(codec, bytes)`; `None` deletes.
    pub(crate) value: Option<(u8, &'a [u8])>,
}

/// What a write transaction writes.
pub(crate) enum Work<'a> {
    /// Entries, with the namespace IDs already known, one per op.
    Entries {
        ops: &'a [Op<'a>],
        known: &'a [Option<i64>],
    },
    /// The units and statements of a `TableWrite` on namespace `ns`.
    Tables {
        ns: i64,
        quota: u64,
        ops: &'a TableOps,
    },
}

pub(crate) struct WriteParams<'a> {
    pub(crate) work: Work<'a>,
    /// Entries read since their access time was last written; set to `atime`.
    pub(crate) touched: &'a [i64],
    /// Entries that failed to decode.
    pub(crate) bad: &'a [i64],
    pub(crate) budget: u64,
    /// Minutes since the Unix epoch.
    pub(crate) atime: i64,
}

pub(crate) struct WriteOutcome {
    /// `meta.bytes` after the transaction.
    pub(crate) bytes: u64,
    /// Bytes deleted for the budget and for retired namespaces.
    pub(crate) evicted: u64,
    /// Namespace IDs this transaction looked up or assigned.
    pub(crate) namespaces: Vec<(Namespace, i64)>,
    /// Custom table namespaces this transaction deleted units of.
    pub(crate) deleted_tables: Vec<String>,
}

/// The byte accounting of one write transaction: `meta.bytes`, and the
/// change of each namespace for `custom.bytes`.
#[derive(Default)]
pub(crate) struct Tally {
    /// `meta.bytes` so far.
    bytes: i64,
    /// Bytes of the entries and units written.
    written: i64,
    /// Bytes deleted for the budget, a quota and retired namespaces.
    evicted: i64,
    /// Per namespace ID: the change in bytes, and whether any was deleted.
    namespaces: Vec<(i64, i64, bool)>,
}

impl Tally {
    fn namespace(&mut self, ns: i64) -> &mut (i64, i64, bool) {
        let at = match self.namespaces.iter().position(|(id, _, _)| *id == ns) {
            Some(at) => at,
            None => {
                self.namespaces.push((ns, 0, false));
                self.namespaces.len() - 1
            }
        };
        &mut self.namespaces[at]
    }

    /// A new or replacing entry or unit of `size` bytes.
    pub(crate) fn write(&mut self, ns: i64, size: i64) {
        self.bytes += size;
        self.written += size;
        self.namespace(ns).1 += size;
    }

    /// An entry overwritten in place: its old size no longer counts.
    fn overwrite(&mut self, ns: i64, old: i64) {
        self.bytes -= old;
        self.namespace(ns).1 -= old;
    }

    /// An entry or unit deleted.
    pub(crate) fn delete(&mut self, ns: i64, size: i64) {
        self.bytes -= size;
        let n = self.namespace(ns);
        n.1 -= size;
        n.2 = true;
    }

    /// Deleted by eviction.
    pub(crate) fn evict(&mut self, ns: i64, size: i64) {
        self.delete(ns, size);
        self.evicted += size;
    }

    /// The change in bytes of namespace `ns` so far.
    fn delta(&self, ns: i64) -> i64 {
        self.namespaces
            .iter()
            .find(|(id, _, _)| *id == ns)
            .map_or(0, |n| n.1)
    }
}

/// Opens the write connection of a partition, creating the file.
pub(crate) fn open_writer(file: &Path, page_cache: u64) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        file,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    configure(&conn, page_cache)?;
    Ok(conn)
}

/// Opens the read connection of a partition whose file exists.
pub(crate) fn open_reader(file: &Path, page_cache: u64) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        file,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    configure(&conn, page_cache)?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    Ok(conn)
}

/// Settings of both connections. None of them touches the file;
/// `journal_mode` is persistent and set when the file is created.
fn configure(conn: &Connection, page_cache: u64) -> rusqlite::Result<()> {
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    conn.busy_timeout(Duration::from_millis(100))?;
    let kib = (page_cache / 1024).max(1);
    // Foreign keys: deleting a unit's entry deletes its rows in the custom
    // tables in the same statement (ON DELETE CASCADE).
    conn.execute_batch(&format!(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -{kib};
         PRAGMA journal_size_limit = 4194304;
         PRAGMA mmap_size = 0;
         PRAGMA trusted_schema = OFF;
         PRAGMA cell_size_check = ON;"
    ))
}

/// Reads the header and `meta`. Errors only for SQLite failures that are not
/// about the content (busy, IO, corruption).
pub(crate) fn check(conn: &Connection, identity: &Identity<'_>) -> rusqlite::Result<Check> {
    let application_id: i32 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let user_version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if application_id == 0 && user_version == 0 {
        let tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
        return Ok(if tables == 0 {
            Check::New
        } else {
            Check::Mismatch
        });
    }
    if application_id != APPLICATION_ID || user_version != SCHEMA_VERSION {
        return Ok(Check::Mismatch);
    }
    let meta = match read_meta(conn) {
        Ok(meta) => meta,
        Err(e) if is_content_error(&e) => return Ok(Check::Mismatch),
        Err(e) => return Err(e),
    };
    let (Some(kind), Some(path), Some(bytes)) = meta else {
        return Ok(Check::Mismatch);
    };
    if kind != identity.kind || path != identity.path || bytes < 0 {
        return Ok(Check::Mismatch);
    }
    Ok(Check::Ready {
        bytes: bytes as u64,
    })
}

type Meta = (Option<String>, Option<Vec<u8>>, Option<i64>);

fn read_meta(conn: &Connection) -> rusqlite::Result<Meta> {
    let mut stmt = conn.prepare("SELECT name, value FROM meta")?;
    let mut rows = stmt.query([])?;
    let (mut kind, mut path, mut bytes) = (None, None, None);
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        match name.as_str() {
            "kind" => kind = Some(row.get(1)?),
            "path" => path = Some(row.get(1)?),
            "bytes" => bytes = Some(row.get(1)?),
            _ => {}
        }
    }
    Ok((kind, path, bytes))
}

/// Errors that mean the tables are not what this version writes: a missing
/// table or column (`SQLITE_ERROR`), a value of the wrong type.
fn is_content_error(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(f, _) => f.code == ErrorCode::Unknown,
        rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::IntegralValueOutOfRange(..) => true,
        _ => false,
    }
}

/// Creates the tables of an empty file.
pub(crate) fn create(
    conn: &mut Connection,
    identity: &Identity<'_>,
    now_ms: u64,
) -> rusqlite::Result<()> {
    // Before the first table, and before WAL writes the header.
    conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")?;
    enable_wal(conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA)?;
    tx.execute_batch(&format!(
        "PRAGMA application_id = {APPLICATION_ID};
         PRAGMA user_version = {SCHEMA_VERSION};"
    ))?;
    {
        let mut insert = tx.prepare("INSERT INTO meta (name, value) VALUES (?1, ?2)")?;
        insert.execute(params!["kind", identity.kind])?;
        insert.execute(params!["path", identity.path])?;
        insert.execute(params!["bytes", 0i64])?;
        insert.execute(params!["opened", (now_ms / 1000) as i64])?;
    }
    tx.commit()
}

/// `journal_mode = WAL` is persistent; setting it on an existing WAL file is
/// a no-op.
pub(crate) fn enable_wal(conn: &Connection) -> rusqlite::Result<()> {
    let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    Ok(())
}

/// Records the time of this open in `meta.opened`, which also moves the
/// file's modification time.
pub(crate) fn record_opened(conn: &Connection, now_ms: u64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE meta SET value = ?1 WHERE name = 'opened'",
        params![(now_ms / 1000) as i64],
    )?;
    Ok(())
}

/// The ID of a namespace version, if this file has seen it.
pub(crate) fn namespace_id(conn: &Connection, ns: &Namespace) -> rusqlite::Result<Option<i64>> {
    conn.prepare_cached("SELECT id FROM namespace WHERE name = ?1 AND version = ?2")?
        .query_row(params![ns.name, ns.version], |r| r.get(0))
        .optional()
}

/// A stored entry as read.
pub(crate) struct Found {
    pub(crate) id: i64,
    /// Minutes since the Unix epoch.
    pub(crate) atime: i64,
    pub(crate) raw: Raw,
}

/// Reads entries of one namespace; one read transaction when there are
/// several keys.
pub(crate) fn read(
    conn: &Connection,
    ns: i64,
    keys: &[&[u8]],
) -> rusqlite::Result<Vec<Option<Found>>> {
    // The connection's default behavior: deferred.
    let tx = if keys.len() > 1 {
        Some(conn.unchecked_transaction()?)
    } else {
        None
    };
    let mut stmt = conn
        .prepare_cached("SELECT id, atime, codec, value FROM entry WHERE ns = ?1 AND key = ?2")?;
    let mut found = Vec::with_capacity(keys.len());
    for key in keys {
        let row = stmt
            .query_row(params![ns, key], |r| {
                Ok(Found {
                    id: r.get(0)?,
                    atime: r.get(1)?,
                    raw: Raw {
                        codec: r.get(2)?,
                        bytes: r.get(3)?,
                    },
                })
            })
            .optional()?;
        found.push(row);
    }
    drop(stmt);
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(found)
}

/// Writes access times back in a transaction of their own, when a partition
/// is closed or swapped out, or the pending set is full.
pub(crate) fn touch(conn: &mut Connection, ids: &[i64], atime: i64) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    set_atimes(&tx, ids, atime)?;
    tx.commit()
}

/// Entries deleted since they were read are skipped, and so are entries
/// written or touched within `TOUCH_AFTER_MIN` (units are touched without
/// reading their access time). The time of the write keeps the LRU order:
/// nothing was written between the read and now.
fn set_atimes(conn: &Connection, ids: &[i64], atime: i64) -> rusqlite::Result<()> {
    let mut update =
        conn.prepare_cached("UPDATE entry SET atime = ?1 WHERE id = ?2 AND atime < ?3")?;
    for id in ids {
        update.execute(params![atime, id, atime - TOUCH_AFTER_MIN])?;
    }
    Ok(())
}

/// One write transaction: write access times back, delete entries that
/// failed to decode, apply the writes, delete entries of retired namespaces,
/// evict to a custom namespace's quota and to the budget, return free pages,
/// update `meta.bytes` and `custom.bytes`, commit. `hook` runs just before
/// the commit. A failing statement of a custom table's user sets
/// `statement_failed`.
pub(crate) fn write(
    conn: &mut Connection,
    p: &WriteParams<'_>,
    statement_failed: &mut bool,
    hook: impl FnOnce(),
) -> rusqlite::Result<WriteOutcome> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut tally = Tally {
        bytes: tx
            .prepare_cached("SELECT value FROM meta WHERE name = 'bytes'")?
            .query_row([], |r| r.get(0))?,
        ..Tally::default()
    };
    set_atimes(&tx, p.touched, p.atime)?;

    {
        let mut delete = tx.prepare_cached("DELETE FROM entry WHERE id = ?1 RETURNING ns, size")?;
        for id in p.bad {
            if let Some((ns, size)) = delete
                .query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
            {
                tally.delete(ns, size);
            }
        }
    }

    let mut namespaces: Vec<(Namespace, i64)> = Vec::new();
    match p.work {
        Work::Entries { ops, known } => {
            write_entries(&tx, ops, known, p.atime, &mut tally, &mut namespaces)?
        }
        Work::Tables { ns, ops, .. } => {
            custom::apply(&tx, ns, ops, p.atime, &mut tally, statement_failed)?
        }
    }

    {
        let mut retired = tx.prepare_cached(
            "DELETE FROM entry WHERE id IN (
               SELECT id FROM entry
               WHERE ns IN (SELECT id FROM namespace WHERE retired = 1)
               LIMIT ?1
             ) RETURNING ns, size",
        )?;
        let mut rows = retired.query(params![RETIRED_MAX])?;
        while let Some(row) = rows.next()? {
            tally.evict(row.get(0)?, row.get(1)?);
        }
    }

    // Quota and budget eviction share one limit per transaction; quota
    // eviction alone stops at a lower one.
    let most = tally.written.clamp(EVICT_MIN as i64, EVICT_MAX as i64);
    let mut freed = 0i64;
    if let Work::Tables { ns, quota, .. } = p.work {
        let ns_bytes = custom::bytes(&tx, ns)? + tally.delta(ns);
        if ns_bytes > i64::try_from(quota).unwrap_or(i64::MAX) {
            let target = i64::try_from(quota / 10 * EVICT_TARGET_TENTHS).unwrap_or(i64::MAX);
            let quota_most = tally
                .written
                .clamp(QUOTA_EVICT_MIN as i64, EVICT_MAX as i64);
            custom::evict_quota(
                &tx, ns, ns_bytes, target, quota_most, &mut freed, &mut tally,
            )?;
        }
    }

    let budget = i64::try_from(p.budget).unwrap_or(i64::MAX);
    if tally.bytes > budget {
        let target = i64::try_from(p.budget / 10 * EVICT_TARGET_TENTHS).unwrap_or(i64::MAX);
        let mut oldest =
            tx.prepare_cached("SELECT id, ns, size FROM entry ORDER BY atime, id LIMIT 64")?;
        let mut delete = tx.prepare_cached("DELETE FROM entry WHERE id = ?1")?;
        'evict: while tally.bytes > target && freed < most {
            let victims: Vec<(i64, i64, i64)> = oldest
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            if victims.is_empty() {
                break;
            }
            for (id, ns, size) in victims {
                delete.execute(params![id])?;
                tally.evict(ns, size);
                freed += size;
                if tally.bytes <= target || freed >= most {
                    break 'evict;
                }
            }
        }
    }

    vacuum(&tx)?;

    let bytes = tally.bytes.max(0);
    tx.prepare_cached("UPDATE meta SET value = ?1 WHERE name = 'bytes'")?
        .execute(params![bytes])?;
    let deleted_tables = custom_bytes(&tx, &tally)?;
    hook();
    tx.commit()?;
    Ok(WriteOutcome {
        bytes: bytes as u64,
        evicted: tally.evicted as u64,
        namespaces,
        deleted_tables,
    })
}

/// Adds each namespace's change to its `custom.bytes`; returns the custom
/// namespaces that lost units.
fn custom_bytes(conn: &Connection, tally: &Tally) -> rusqlite::Result<Vec<String>> {
    let mut deleted = Vec::new();
    let mut update =
        conn.prepare_cached("UPDATE custom SET bytes = bytes + ?1 WHERE ns = ?2 RETURNING name")?;
    for (ns, delta, lost) in &tally.namespaces {
        if *delta == 0 && !lost {
            continue;
        }
        let name: Option<String> = update
            .query_row(params![delta, ns], |r| r.get(0))
            .optional()?;
        if let Some(name) = name
            && *lost
        {
            deleted.push(name);
        }
    }
    Ok(deleted)
}

/// The writes of `Partition::put`, `remove` and `Batch::commit`.
fn write_entries(
    tx: &Connection,
    ops: &[Op<'_>],
    known: &[Option<i64>],
    atime: i64,
    tally: &mut Tally,
    namespaces: &mut Vec<(Namespace, i64)>,
) -> rusqlite::Result<()> {
    for (op, known) in ops.iter().zip(known) {
        let id = match known.or_else(|| {
            namespaces
                .iter()
                .find(|(ns, _)| ns.same_version(&op.namespace))
                .map(|(_, id)| *id)
        }) {
            Some(id) => Some(id),
            None => {
                let id = if op.value.is_some() {
                    Some(assign_namespace(tx, &op.namespace)?)
                } else {
                    namespace_id(tx, &op.namespace)?
                };
                if let Some(id) = id {
                    namespaces.push((op.namespace, id));
                }
                id
            }
        };
        let Some(ns) = id else {
            // Removing from a namespace this file has never seen.
            continue;
        };
        match op.value {
            Some((codec, value)) => {
                let old: Option<i64> = tx
                    .prepare_cached("SELECT size FROM entry WHERE ns = ?1 AND key = ?2")?
                    .query_row(params![ns, op.key], |r| r.get(0))
                    .optional()?;
                let size = (op.key.len() + value.len()) as i64 + ENTRY_OVERHEAD as i64;
                tx.prepare_cached(
                    "INSERT INTO entry (ns, key, codec, size, atime, value)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT (ns, key) DO UPDATE SET
                       codec = excluded.codec, size = excluded.size,
                       atime = excluded.atime, value = excluded.value",
                )?
                .execute(params![ns, op.key, codec, size, atime, value])?;
                if let Some(old) = old {
                    tally.overwrite(ns, old);
                }
                tally.write(ns, size);
            }
            None => {
                if let Some(size) = tx
                    .prepare_cached("DELETE FROM entry WHERE ns = ?1 AND key = ?2 RETURNING size")?
                    .query_row(params![ns, op.key], |r| r.get::<_, i64>(0))
                    .optional()?
                {
                    tally.delete(ns, size);
                }
            }
        }
    }
    Ok(())
}

/// Returns up to `VACUUM_PAGES` free pages to the file system when the free
/// pages exceed `VACUUM_FREE`. The file shrinks at the next checkpoint.
fn vacuum(conn: &Connection) -> rusqlite::Result<()> {
    let pragma = |sql: &str| -> rusqlite::Result<i64> {
        conn.prepare_cached(sql)?.query_row([], |r| r.get(0))
    };
    if pragma("PRAGMA freelist_count")? * pragma("PRAGMA page_size")? <= VACUUM_FREE as i64 {
        return Ok(());
    }
    // One result row per page freed: run the statement to the end.
    let mut stmt = conn.prepare_cached(&format!("PRAGMA incremental_vacuum({VACUUM_PAGES})"))?;
    let mut rows = stmt.query([])?;
    while rows.next()?.is_some() {}
    Ok(())
}

/// The ID of a namespace version, inserting it on first use. A new version
/// retires the lower ones; a version lower than one already seen is retired
/// from the start.
pub(crate) fn assign_namespace(conn: &Connection, ns: &Namespace) -> rusqlite::Result<i64> {
    if let Some(id) = namespace_id(conn, ns)? {
        return Ok(id);
    }
    conn.prepare_cached(
        "INSERT INTO namespace (name, version, retired)
         VALUES (?1, ?2, EXISTS (SELECT 1 FROM namespace WHERE name = ?1 AND version > ?2))",
    )?
    .execute(params![ns.name, ns.version])?;
    let id = conn.last_insert_rowid();
    conn.prepare_cached("UPDATE namespace SET retired = 1 WHERE name = ?1 AND version < ?2")?
        .execute(params![ns.name, ns.version])?;
    Ok(id)
}
