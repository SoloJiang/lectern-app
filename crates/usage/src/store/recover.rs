//! Salvage of a corrupt database (docs/design/usage.md「数据库」, steps 2–4
//! of 损坏的处理). Every row that can still be read is copied by rowid ranges
//! into `usage.db.salvage`, with the original attached read only and left
//! where it is; rows that point at a lost row are dropped, the derived
//! tables are rebuilt, and sessions that may have lost rows read their logs
//! again from the start (keys deduplicate, nothing counts twice). Only the
//! finished, synced copy replaces `usage.db`; the original is kept as
//! `usage.db.corrupt-<ms>`. A salvage cut short leaves `usage.db` as it was
//! and the half-built copy behind, and the next open salvages again.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lectern_logging::{Value, error, info, warn};
use lectern_services::session::Cursor;
use rusqlite::{Connection, OpenFlags, params};

use super::schema::{OBSERVATION_TABLES, Schema};
use super::{DB_FILE, Files, Mode, configure, create, dedup, ingest, open_connection, set_meta};

/// The copy a salvage builds before it replaces `usage.db`.
pub(crate) const SALVAGE_FILE: &str = "usage.db.salvage";

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
    /// Copied rows dropped because they pointed at a lost row.
    pub(crate) dangling: u64,
    /// Sessions whose cursor went back to the start.
    pub(crate) sessions_reread: u64,
}

/// Most failing statements one table's bisection may run before the rest of
/// that table counts as lost.
const FAILURE_BUDGET: u32 = 20_000;

/// Rowids tried when the largest one cannot be read.
const ROWID_CEILING: i64 = 1 << 40;

/// The points where a test can cut a salvage short, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// The copy has its tables.
    Created,
    /// The rows are copied and the sessions restarted.
    Copied,
    /// The derived tables are rebuilt.
    Rebuilt,
    /// The copy is checkpointed and synced.
    Synced,
    /// The original is linked under its kept name.
    Kept,
    /// The original's WAL is gone; the rename is next.
    Unjournaled,
}

#[cfg(test)]
thread_local! {
    static FAIL_AT: std::cell::Cell<Option<Step>> = const { std::cell::Cell::new(None) };
}

/// Makes this thread's next salvage fail at `step`, as if the process had
/// ended there.
#[cfg(test)]
pub(crate) fn fail_at(step: Step) {
    FAIL_AT.with(|f| f.set(Some(step)));
}

#[cfg(test)]
fn reached(step: Step) -> rusqlite::Result<()> {
    if FAIL_AT.with(|f| f.get()) == Some(step) {
        FAIL_AT.with(|f| f.set(None));
        return Err(io_error(io::Error::other("injected")));
    }
    Ok(())
}

#[cfg(not(test))]
fn reached(step: Step) -> rusqlite::Result<()> {
    let _ = step;
    Ok(())
}

/// Removes a leftover `usage.db.salvage` (with `-wal` and `-shm`). True when
/// there was one: a salvage was cut short.
pub(crate) fn discard_leftover(dir: &Path) -> io::Result<bool> {
    let temp = dir.join(SALVAGE_FILE);
    let found = temp.exists();
    for path in [with_suffix(&temp, "-wal"), with_suffix(&temp, "-shm"), temp] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(found)
}

/// Copies what can be read from `usage.db` into a new file, then puts the
/// new file in its place and keeps the original as `usage.db.corrupt-<at>`.
/// No connection to `usage.db` may be open.
pub(crate) fn salvage(dir: &Path, schema: &Schema, at: i64) -> rusqlite::Result<(Files, Recovery)> {
    let file = dir.join(DB_FILE);
    let temp = dir.join(SALVAGE_FILE);
    if discard_leftover(dir).map_err(io_error)? {
        warn!("leftover of an interrupted salvage removed"; path = Value::path(&temp));
    }
    // Fold the WAL into the main file first when SQLite still can, so the
    // read-only copy below sees every committed row.
    if let Ok(conn) = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_WRITE) {
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }

    let mut out = Connection::open_with_flags(
        &temp,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    configure(&out)?;
    create(&mut out, schema)?;
    out.execute_batch("PRAGMA synchronous = OFF;")?;
    reached(Step::Created)?;
    let mut copied = 0;
    let mut lost = 0;
    let mut dangling = 0;
    let mut sessions_reread = 0;
    // A file whose header is gone cannot even be attached: nothing to copy,
    // every session reads its log again (they have no rows here).
    if out
        .execute("ATTACH DATABASE ?1 AS old", params![read_only_uri(&file)])
        .is_ok()
    {
        for &table in OBSERVATION_TABLES {
            let (c, l) = copy_table(&out, table);
            info!("salvaged table"; table = table, copied = c, lost = l);
            copied += c;
            lost += l;
        }
        copy_meta(&out);
        out.execute_batch("DETACH DATABASE old;")?;
        if lost > 0 {
            dangling = drop_dangling(&out)?;
            sessions_reread = restart_sessions(&out)?;
        }
    } else {
        lost = 1;
    }
    reached(Step::Copied)?;
    out.execute_batch("PRAGMA synchronous = FULL;")?;
    dedup::rebuild(&mut out)?;
    reached(Step::Rebuilt)?;
    out.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    drop(out);
    sync(&temp).map_err(io_error)?;
    reached(Step::Synced)?;

    let kept = keep(dir, &file, at).map_err(io_error)?;
    sync(dir).map_err(io_error)?;
    reached(Step::Kept)?;
    // The original's journal must not meet the new file; the kept copy has
    // its own link to it.
    for suffix in ["-wal", "-shm"] {
        match fs::remove_file(with_suffix(&file, suffix)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(e)),
        }
    }
    reached(Step::Unjournaled)?;
    fs::rename(&temp, &file).map_err(io_error)?;
    sync(dir).map_err(io_error)?;

    let writer = open_connection(&file, false)?;
    set_meta(&writer, "clean", 0)?;
    let reader = open_connection(&file, false)?;
    reader.execute_batch("PRAGMA query_only = ON;")?;
    error!(
        "database salvaged";
        kept = Value::path(&kept),
        copied = copied,
        lost = lost,
        dangling = dangling,
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
            dangling,
            sessions_reread,
        },
    ))
}

/// Links `usage.db` (and a WAL it still has) under
/// `usage.db.corrupt-<at>`, with a `-<n>` suffix when that name is taken
/// (a salvage cut short after this step). Copies when links are refused.
fn keep(dir: &Path, file: &Path, at: i64) -> io::Result<PathBuf> {
    for n in 0..100u32 {
        let name = match n {
            0 => format!("{DB_FILE}.corrupt-{at}"),
            n => format!("{DB_FILE}.corrupt-{at}-{n}"),
        };
        let kept = dir.join(name);
        match link_or_copy(file, &kept) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
        let wal = with_suffix(file, "-wal");
        if fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
            link_or_copy(&wal, &with_suffix(&kept, "-wal"))?;
        }
        return Ok(kept);
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

fn link_or_copy(from: &Path, to: &Path) -> io::Result<()> {
    match fs::hard_link(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            fs::copy(from, to)?;
            sync(to)
        }
    }
}

/// `fsync` of a file or a directory.
fn sync(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
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
    // The old table's columns cannot even be read: nothing of it is copied.
    let Ok(mut insert) = conn.prepare(&sql) else {
        return (0, 1);
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

/// Rows that point at a lost row would point at whatever row later takes the
/// lost ID (a new ID is the largest plus one), so they go: a session without
/// its source, ID or agent string, and every row of a session that is gone;
/// turns without their ID string or location; sources without their
/// location. Lighter losses are repaired in place: a lost parent, turn or
/// reading state is cleared, a lost model becomes the empty string. The
/// sessions read their logs again and put the dropped rows back. Returns the
/// rows dropped or changed.
fn drop_dangling(conn: &Connection) -> rusqlite::Result<u64> {
    const STEPS: &[&str] = &[
        "DELETE FROM project WHERE location NOT IN (SELECT id FROM location)",
        "DELETE FROM session WHERE source NOT IN (SELECT id FROM intern)
           OR sid NOT IN (SELECT id FROM intern) OR agent NOT IN (SELECT id FROM intern)",
        "UPDATE session SET parent_source = NULL, parent_sid = NULL
         WHERE parent_source NOT IN (SELECT id FROM intern)
            OR parent_sid NOT IN (SELECT id FROM intern)",
        "DELETE FROM turn WHERE session NOT IN (SELECT id FROM session)
           OR tid NOT IN (SELECT id FROM intern) OR location NOT IN (SELECT id FROM location)",
        "DELETE FROM usage_source WHERE session NOT IN (SELECT id FROM session)
           OR location NOT IN (SELECT id FROM location)",
        "UPDATE usage_source SET turn = NULL WHERE turn NOT IN (SELECT id FROM turn)",
        "DELETE FROM session_gen WHERE session NOT IN (SELECT id FROM session)",
        "UPDATE session_gen SET cwd = NULL WHERE cwd NOT IN (SELECT id FROM location)",
        "UPDATE session_gen SET model = NULL WHERE model NOT IN (SELECT id FROM intern)",
        "UPDATE session_gen SET turn = NULL WHERE turn NOT IN (SELECT id FROM turn)",
        // Marked first: the empty string may itself take a lost ID.
        "UPDATE usage_source SET model = -1 WHERE model NOT IN (SELECT id FROM intern)",
    ];
    let mut n = 0u64;
    for sql in STEPS {
        n += conn.execute(sql, [])? as u64;
    }
    let empty = ingest::intern(conn, b"")?;
    conn.execute(
        "UPDATE usage_source SET model = ?1 WHERE model = -1",
        params![empty],
    )?;
    Ok(n)
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
        "UPDATE session SET cursor = ?1, replay = NULL, reached_end = 0, end_stamp = NULL",
        params![start],
    )?;
    Ok(n as u64)
}
