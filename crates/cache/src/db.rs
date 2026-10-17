//! The SQLite file: connections, schema, identity check, and the statements
//! of reads and write transactions. Error handling is in `slot`.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::Namespace;
use crate::codec::Raw;

/// `PRAGMA application_id`: 0x4C435443, "LCTC".
pub(crate) const APPLICATION_ID: i32 = 0x4C43_5443;
/// `PRAGMA user_version`. Bumped when the tables change; older and newer
/// files are rebuilt, never migrated.
pub(crate) const SCHEMA_VERSION: i32 = 1;

/// Prepared statements kept per connection (rusqlite's `cache` feature).
const STATEMENT_CACHE: usize = 16;
/// Bytes counted for an entry beyond its key and value.
pub(crate) const ENTRY_OVERHEAD: u64 = 64;
/// Most bytes one write transaction deletes to get under the budget.
pub(crate) const EVICT_MAX: u64 = 16 << 20;
/// Most entries of retired namespace versions one write transaction deletes.
pub(crate) const RETIRED_MAX: i64 = 256;
/// Eviction stops at this share of the budget, in tenths.
const EVICT_TARGET_TENTHS: u64 = 9;

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

pub(crate) struct WriteParams<'a> {
    pub(crate) ops: &'a [Op<'a>],
    /// Namespace IDs already known, one per op.
    pub(crate) known: &'a [Option<i64>],
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
    conn.execute_batch(&format!(
        "PRAGMA synchronous = NORMAL;
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

/// Reads entries of one namespace; one read transaction when there are
/// several keys.
pub(crate) fn read(
    conn: &Connection,
    ns: i64,
    keys: &[&[u8]],
) -> rusqlite::Result<Vec<Option<(i64, Raw)>>> {
    // The connection's default behavior: deferred.
    let tx = if keys.len() > 1 {
        Some(conn.unchecked_transaction()?)
    } else {
        None
    };
    let mut stmt =
        conn.prepare_cached("SELECT id, codec, value FROM entry WHERE ns = ?1 AND key = ?2")?;
    let mut found = Vec::with_capacity(keys.len());
    for key in keys {
        let row = stmt
            .query_row(params![ns, key], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    Raw {
                        codec: r.get(1)?,
                        bytes: r.get(2)?,
                    },
                ))
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

/// One write transaction: delete entries that failed to decode, apply the
/// writes, delete entries of retired namespaces and evict to the budget,
/// update `meta.bytes`, commit. `hook` runs just before the commit.
pub(crate) fn write(
    conn: &mut Connection,
    p: &WriteParams<'_>,
    hook: impl FnOnce(),
) -> rusqlite::Result<WriteOutcome> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut bytes: i64 = tx
        .prepare_cached("SELECT value FROM meta WHERE name = 'bytes'")?
        .query_row([], |r| r.get(0))?;
    let mut evicted = 0i64;

    {
        let mut delete = tx.prepare_cached("DELETE FROM entry WHERE id = ?1 RETURNING size")?;
        for id in p.bad {
            if let Some(size) = delete
                .query_row(params![id], |r| r.get::<_, i64>(0))
                .optional()?
            {
                bytes -= size;
            }
        }
    }

    let mut namespaces: Vec<(Namespace, i64)> = Vec::new();
    for (op, known) in p.ops.iter().zip(p.known) {
        let id = match known.or_else(|| {
            namespaces
                .iter()
                .find(|(ns, _)| ns.same_version(&op.namespace))
                .map(|(_, id)| *id)
        }) {
            Some(id) => Some(id),
            None => {
                let id = if op.value.is_some() {
                    Some(assign_namespace(&tx, &op.namespace)?)
                } else {
                    namespace_id(&tx, &op.namespace)?
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
                .execute(params![ns, op.key, codec, size, p.atime, value])?;
                bytes += size - old.unwrap_or(0);
            }
            None => {
                if let Some(size) = tx
                    .prepare_cached("DELETE FROM entry WHERE ns = ?1 AND key = ?2 RETURNING size")?
                    .query_row(params![ns, op.key], |r| r.get::<_, i64>(0))
                    .optional()?
                {
                    bytes -= size;
                }
            }
        }
    }

    {
        let mut retired = tx.prepare_cached(
            "DELETE FROM entry WHERE id IN (
               SELECT id FROM entry
               WHERE ns IN (SELECT id FROM namespace WHERE retired = 1)
               LIMIT ?1
             ) RETURNING size",
        )?;
        let mut rows = retired.query(params![RETIRED_MAX])?;
        while let Some(row) = rows.next()? {
            let size: i64 = row.get(0)?;
            bytes -= size;
            evicted += size;
        }
    }

    let budget = i64::try_from(p.budget).unwrap_or(i64::MAX);
    if bytes > budget {
        let target = i64::try_from(p.budget / 10 * EVICT_TARGET_TENTHS).unwrap_or(i64::MAX);
        let mut freed = 0i64;
        let mut oldest =
            tx.prepare_cached("SELECT id, size FROM entry ORDER BY atime, id LIMIT 64")?;
        let mut delete = tx.prepare_cached("DELETE FROM entry WHERE id = ?1")?;
        'evict: while bytes > target && freed < EVICT_MAX as i64 {
            let victims: Vec<(i64, i64)> = oldest
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            if victims.is_empty() {
                break;
            }
            for (id, size) in victims {
                delete.execute(params![id])?;
                bytes -= size;
                freed += size;
                if bytes <= target || freed >= EVICT_MAX as i64 {
                    break 'evict;
                }
            }
        }
        evicted += freed;
    }

    let bytes = bytes.max(0);
    tx.prepare_cached("UPDATE meta SET value = ?1 WHERE name = 'bytes'")?
        .execute(params![bytes])?;
    hook();
    tx.commit()?;
    Ok(WriteOutcome {
        bytes: bytes as u64,
        evicted: evicted as u64,
        namespaces,
    })
}

/// The ID of a namespace version, inserting it on first use. A new version
/// retires the lower ones; a version lower than one already seen is retired
/// from the start.
fn assign_namespace(conn: &Connection, ns: &Namespace) -> rusqlite::Result<i64> {
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
