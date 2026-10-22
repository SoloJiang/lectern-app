//! Salvage of a corrupt database (docs/design/usage.md「数据库」, steps 2–4
//! of 损坏的处理): the original files are renamed and kept, every row that
//! can still be read is copied into a new file by rowid ranges, the derived
//! tables are rebuilt, and sessions that may have lost rows read their logs
//! again from the start (keys deduplicate, nothing counts twice).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lectern_logging::{Value, error, info};
use lectern_services::session::Cursor;
use rusqlite::{Connection, OpenFlags, params};

use super::schema::{OBSERVATION_TABLES, Schema};
use super::{DB_FILE, Files, Mode, configure, create, dedup, open_connection, set_meta};

/// What a salvage did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recovery {
    /// Unix milliseconds; part of the kept file's name.
    pub(crate) at: i64,
    /// The corrupt file under its new name.
    pub(crate) kept: PathBuf,
    /// Rows copied, and rowids that could not be read (counted one by one
    /// once the bisection narrows a failing range down to them).
    pub(crate) copied: u64,
    pub(crate) lost: u64,
    /// Sessions whose cursor went back to the start.
    pub(crate) sessions_reread: u64,
}

/// Most failing statements one table's bisection may run before the rest of
/// that table counts as lost.
const FAILURE_BUDGET: u32 = 20_000;

/// Rowids tried when the largest one cannot be read.
const ROWID_CEILING: i64 = 1 << 40;

/// Renames `usage.db` (with `-wal` and `-shm`) to
/// `usage.db.corrupt-<at>`, creates a new file and copies what it can.
pub(crate) fn salvage(dir: &Path, schema: &Schema, at: i64) -> rusqlite::Result<(Files, Recovery)> {
    let file = dir.join(DB_FILE);
    // Fold the WAL into the main file first when SQLite still can, so the
    // read-only copy below sees every committed row.
    if let Ok(conn) = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_WRITE) {
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
    let kept = dir.join(format!("{DB_FILE}.corrupt-{at}"));
    rename(&file, &kept).map_err(io_error)?;
    for suffix in ["-wal", "-shm"] {
        let from = with_suffix(&file, suffix);
        if from.exists() {
            rename(&from, &with_suffix(&kept, suffix)).map_err(io_error)?;
        }
    }

    let mut writer = open_connection(&file, true)?;
    create(&mut writer, schema)?;
    drop(writer);
    let writer = Connection::open_with_flags(
        &file,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    configure(&writer)?;
    writer.execute_batch("PRAGMA synchronous = OFF;")?;
    let mut copied = 0;
    let mut lost = 0;
    let mut sessions_reread = 0;
    // A file whose header is gone cannot even be attached: nothing to copy,
    // every session reads its log again (they have no rows here).
    if writer
        .execute("ATTACH DATABASE ?1 AS old", params![read_only_uri(&kept)])
        .is_ok()
    {
        let mut lost_rows = false;
        for &table in OBSERVATION_TABLES {
            let (c, l) = copy_table(&writer, table);
            info!("salvaged table"; table = table, copied = c, lost = l);
            copied += c;
            lost += l;
            lost_rows |= l > 0;
        }
        copy_meta(&writer);
        if lost_rows {
            sessions_reread = restart_sessions(&writer)?;
        }
        writer.execute_batch("DETACH DATABASE old;")?;
    } else {
        lost = 1;
    }
    writer.execute_batch("PRAGMA synchronous = FULL;")?;
    let mut writer = writer;
    dedup::rebuild(&mut writer)?;
    let _ = writer.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    drop(writer);

    let writer = open_connection(&file, false)?;
    set_meta(&writer, "clean", 0)?;
    let reader = open_connection(&file, false)?;
    reader.execute_batch("PRAGMA query_only = ON;")?;
    error!(
        "database salvaged";
        kept = Value::path(&kept),
        copied = copied,
        lost = lost,
        sessions_reread = sessions_reread
    );
    Ok((
        Files {
            writer,
            reader,
            mode: Mode::Writable,
            needs_check: false,
            migrated_from: None,
        },
        Recovery {
            at,
            kept,
            copied,
            lost,
            sessions_reread,
        },
    ))
}

fn rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn io_error(e: io::Error) -> rusqlite::Error {
    let _ = e;
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
        None,
    )
}

/// `file:<percent-encoded path>?mode=ro`.
fn read_only_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file:");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out.push_str("?mode=ro");
    out
}

/// Copies `old.<table>` into `main.<table>` by rowid ranges; a range that
/// fails is halved until single rowids, which are skipped. Returns (rows
/// copied, rowids lost).
fn copy_table(conn: &Connection, table: &'static str) -> (u64, u64) {
    let bound = |f: &str| -> Result<Option<i64>, ()> {
        conn.query_row(&format!("SELECT {f}(rowid) FROM old.{table}"), [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .map_err(|_| ())
    };
    let (lo, hi) = match (bound("min"), bound("max")) {
        (Ok(None), _) | (_, Ok(None)) => return (0, 0),
        // Neither end can be read: the root itself is damaged and no row
        // can be reached. The count of lost rows is unknown.
        (Err(()), Err(())) => return (0, 1),
        (lo, hi) => (
            lo.ok().flatten().unwrap_or(1),
            hi.ok().flatten().unwrap_or(ROWID_CEILING),
        ),
    };
    let sql =
        format!("INSERT INTO main.{table} SELECT * FROM old.{table} WHERE rowid BETWEEN ?1 AND ?2");
    let Ok(mut insert) = conn.prepare(&sql) else {
        return (0, 0);
    };
    let mut copied = 0u64;
    let mut lost = 0u64;
    let mut failures = 0u32;
    let mut stack = vec![(lo, hi)];
    while let Some((a, b)) = stack.pop() {
        if failures >= FAILURE_BUDGET {
            lost += 1;
            continue;
        }
        match insert.execute(params![a, b]) {
            Ok(n) => copied += n as u64,
            Err(_) if a == b => {
                failures += 1;
                lost += 1;
            }
            Err(_) => {
                failures += 1;
                let mid = a + (b - a) / 2;
                stack.push((mid + 1, b));
                stack.push((a, mid));
            }
        }
    }
    (copied, lost)
}

/// `meta` is a small table without rowid: the readable rows, minus the
/// close marker and the derived version (rebuilt here).
fn copy_meta(conn: &Connection) {
    let _ = conn.execute(
        "INSERT OR IGNORE INTO main.meta (name, value)
         SELECT name, value FROM old.meta WHERE name NOT IN ('clean', 'derived')",
        [],
    );
}

/// After lost rows nobody knows which sessions they belonged to: every
/// session reads its log again from the start, unfinished replays are
/// dropped. Returns the number of sessions.
fn restart_sessions(conn: &Connection) -> rusqlite::Result<u64> {
    let start = Cursor::START.to_bytes();
    for table in ["usage_source", "turn", "session_gen"] {
        conn.execute(
            &format!(
                "DELETE FROM {table} WHERE gen > (SELECT s.gen FROM session s WHERE s.id = {table}.session)"
            ),
            [],
        )?;
    }
    let n = conn.execute(
        "UPDATE session SET cursor = ?1, replay = NULL, reached_end = 0",
        params![start],
    )?;
    Ok(n as u64)
}
