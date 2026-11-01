//! Salvage of a corrupt database (docs/design/usage.md「数据库」, steps 2–4
//! of 损坏的处理). Every row that can still be read is copied by rowid ranges
//! into `usage.db.salvage`, with the original attached read only and left
//! where it is; rows that point at a lost row are kept and their references
//! made whole (placeholders under the lost IDs), the derived
//! tables are rebuilt, and sessions that may have lost rows read their logs
//! again from the start (keys deduplicate, nothing counts twice). Only the
//! finished, synced copy replaces `usage.db`; the original is kept as
//! `usage.db.corrupt-<ms>`. A salvage cut short leaves `usage.db` as it was
//! and the half-built copy behind, and the next open salvages again.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lectern_logging::{Value, error, info, warn};
use lectern_services::session::Cursor;
use rusqlite::{Connection, OpenFlags, params};

use super::schema::{OBSERVATION_TABLES, Schema};
use super::{
    DB_FILE, Files, Mode, cold, configure, create, dedup, ingest, open_connection, segment,
    set_meta, sync_dir,
};

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
    /// Copied rows whose reference to a lost row was made whole, and
    /// placeholder rows put back under lost IDs.
    pub(crate) repaired: u64,
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
    let mut repaired = 0;
    let mut sessions_reread = 0;
    // A file whose header is gone cannot even be attached: nothing to copy,
    // every session reads its log again (they have no rows here).
    if out
        .execute("ATTACH DATABASE ?1 AS old", params![read_only_uri(&file)])
        .is_ok()
    {
        for &table in OBSERVATION_TABLES {
            let (c, l) = copy_table(&out, table, "1");
            info!("salvaged table"; table = table, copied = c, lost = l);
            copied += c;
            lost += l;
        }
        let (bad, cold_top) = drop_bad_segments(&out)?;
        if bad > 0 {
            error!("cold segments that do not decode stay in the kept file"; segments = bad);
            lost += bad;
        }
        copy_meta(&out);
        let (c, l) = copy_frozen_derived(&out)?;
        copied += c;
        lost += l;
        raise_sequences(&out, &cold_top)?;
        out.execute_batch("DETACH DATABASE old;")?;
        if lost > 0 {
            repaired = repair_references(&out)?;
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
    sync_dir(dir);
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
    sync_dir(dir);

    let writer = open_connection(&file, false)?;
    set_meta(&writer, "clean", 0)?;
    let reader = open_connection(&file, false)?;
    reader.execute_batch("PRAGMA query_only = ON;")?;
    error!(
        "database salvaged";
        kept = Value::path(&kept),
        copied = copied,
        lost = lost,
        repaired = repaired,
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
            repaired,
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

/// `fsync` of a file.
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

/// Segments copied from the old file that do not decode: deleted from the
/// copy (the kept file still has them) and counted. Also gives the highest
/// row ID per table in the segments that decode.
fn drop_bad_segments(conn: &Connection) -> rusqlite::Result<(u64, HashMap<String, i64>)> {
    let mut bad = Vec::new();
    let mut top: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, data FROM main.cold_part")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let data: Vec<u8> = r.get(1)?;
            match segment::decompress(&data).and_then(|raw| segment::decode(&raw)) {
                None => bad.push(r.get::<_, i64>(0)?),
                Some(seg) => {
                    let Some(i) = seg.column("id") else { continue };
                    let max = seg.rows.iter().filter_map(|row| match row[i] {
                        rusqlite::types::Value::Integer(id) => Some(id),
                        _ => None,
                    });
                    if let Some(max) = max.max() {
                        let slot = top.entry(seg.table.clone()).or_insert(0);
                        *slot = (*slot).max(max);
                    }
                }
            }
        }
    }
    for id in &bad {
        conn.execute("DELETE FROM main.cold_part WHERE id = ?1", params![id])?;
    }
    Ok((bad.len() as u64, top))
}

/// Source and turn IDs are never reused (`AUTOINCREMENT`), but the copy
/// does not carry `sqlite_sequence`: when the highest rows were archived
/// (or lost), new rows would take IDs that cold rows already have, and a
/// source at or below a frozen month's `archive_mark` would never be
/// counted. Each sequence starts above the surviving rows, the old file's
/// sequence when it is readable, the IDs in the cold segments and, for
/// the sources, every `archive_mark`.
fn raise_sequences(conn: &Connection, cold_top: &HashMap<String, i64>) -> rusqlite::Result<()> {
    let old: HashMap<String, i64> = conn
        .prepare("SELECT name, seq FROM old.sqlite_sequence")
        .and_then(|mut stmt| {
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect()
        })
        .unwrap_or_default();
    let marks: i64 = conn.query_row(
        "SELECT coalesce(max(value), 0) FROM main.meta
         WHERE name >= 'archive_mark:' AND name < 'archive_mark;'",
        [],
        |r| r.get(0),
    )?;
    for table in ["usage_source", "turn"] {
        let hot: i64 = conn.query_row(
            &format!("SELECT coalesce(max(id), 0) FROM main.{table}"),
            [],
            |r| r.get(0),
        )?;
        let mut floor = hot
            .max(old.get(table).copied().unwrap_or(0))
            .max(cold_top.get(table).copied().unwrap_or(0));
        if table == "usage_source" {
            floor = floor.max(marks);
        }
        let updated = conn.execute(
            "UPDATE main.sqlite_sequence SET seq = max(seq, ?2) WHERE name = ?1",
            params![table, floor],
        )?;
        if updated == 0 && floor > 0 {
            conn.execute(
                "INSERT INTO main.sqlite_sequence (name, seq) VALUES (?1, ?2)",
                params![table, floor],
            )?;
        }
    }
    Ok(())
}

/// The derived rows of frozen months (docs/design/usage.md「冷热分层」): a
/// rebuild leaves those months alone, and their sources may already be
/// cold, so their facts and counted sources are copied rather than
/// rebuilt. Returns (rows copied, rows lost).
fn copy_frozen_derived(conn: &Connection) -> rusqlite::Result<(u64, u64)> {
    let frozen = cold::frozen(conn)?;
    if frozen.is_empty() {
        return Ok((0, 0));
    }
    let (mut copied, mut lost) = copy_table(conn, "fact", &frozen.sql("at"));
    match conn.execute(
        "INSERT OR IGNORE INTO main.usage_value
         SELECT * FROM old.usage_value WHERE source IN (SELECT source FROM main.fact)",
        [],
    ) {
        Ok(n) => copied += n as u64,
        Err(_) => lost += 1,
    }
    info!("salvaged frozen derived rows"; copied = copied, lost = lost);
    Ok((copied, lost))
}

/// Copies `old.<table>` into `main.<table>` by rowid ranges, the rows where
/// `filter` holds; a range that fails is halved until single rowids, which
/// are skipped. Returns (rows copied, rowids lost).
fn copy_table(conn: &Connection, table: &'static str, filter: &str) -> (u64, u64) {
    // An older file may not have the table at all (the cold tables of
    // version 2): nothing to copy, nothing lost.
    let exists = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM old.sqlite_schema WHERE type = 'table' AND name = ?1)",
        params![table],
        |r| r.get::<_, bool>(0),
    );
    if matches!(exists, Ok(false)) {
        return (0, 0);
    }
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
    // Only the columns both tables have: a file of an older table version
    // (the `project` table of version 3 has fewer columns) is copied into
    // the current tables, the new columns taking their defaults.
    let columns = match shared_columns(conn, table) {
        Ok(columns) if !columns.is_empty() => columns.join(", "),
        // The old table's columns cannot even be read: nothing of it is
        // copied.
        _ => return (0, 1),
    };
    let sql = format!(
        "INSERT INTO main.{table} ({columns}) SELECT {columns} FROM old.{table}
         WHERE rowid BETWEEN ?1 AND ?2 AND {filter}"
    );
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

/// The quoted names of the columns `main.<table>` and `old.<table>` both
/// have, in the order of the current table.
fn shared_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let names = |schema: &str| -> rusqlite::Result<Vec<String>> {
        conn.prepare(&format!(
            "SELECT name FROM pragma_table_info('{table}', '{schema}') ORDER BY cid"
        ))?
        .query_map([], |r| r.get(0))?
        .collect()
    };
    let old = names("old")?;
    Ok(names("main")?
        .into_iter()
        .filter(|name| old.contains(name))
        .map(|name| format!("\"{name}\""))
        .collect())
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

/// The prefix of placeholder strings and paths. No UTF-8 text contains the
/// byte 0xFF, so a placeholder never equals a real ID or path, and it sorts
/// after every real source and session ID: a source of a placeholder session
/// loses the dedupe tie against the same usage read again from its log.
const LOST: &[u8] = b"\xFFlost";

fn lost_text(kind: &str, id: i64) -> Vec<u8> {
    let mut out = LOST.to_vec();
    out.extend_from_slice(format!(":{kind}:{id}").as_bytes());
    out
}

/// The agent label of a session whose agent string was lost.
const LOST_AGENT: &[u8] = b"agent:other:lost";

/// IDs that rows refer to and `table` no longer has.
fn missing(conn: &Connection, table: &str, refs: &[(&str, &str)]) -> rusqlite::Result<Vec<i64>> {
    let union = refs
        .iter()
        .map(|(t, c)| format!("SELECT {c} AS id FROM {t} WHERE {c} IS NOT NULL"))
        .collect::<Vec<_>>()
        .join(" UNION ");
    let sql = format!(
        "SELECT DISTINCT id FROM ({union}) WHERE id > 0 AND id NOT IN (SELECT id FROM {table})"
    );
    conn.prepare(&sql)?.query_map([], |r| r.get(0))?.collect()
}

/// Rows that point at a lost row would point at whatever row later takes the
/// lost ID (a new ID is the largest plus one). They are kept, so the usage
/// they hold still counts when its log is gone, and their references are made
/// whole (docs/design/usage.md「数据库」, 损坏的处理 step 3):
///
/// - A lost string a session or turn needs (source ID, session ID, parent,
///   `TurnId`) and a lost session come back under their old IDs as
///   placeholders. A session read again from its log gets a new row, and
///   its sources win the dedupe tie against the placeholder's.
/// - A lost model becomes the empty string, a lost agent `agent:other:lost`,
///   a lost location of a source the unknown place; such sources are marked
///   `repaired` and take the turn, location and model of the same record
///   when its log is read again. A lost location or turn of a turn or a
///   reading state is cleared; a project without its location goes.
///
/// Returns the rows changed.
fn repair_references(conn: &Connection) -> rusqlite::Result<u64> {
    let mut n = 0u64;
    let mut run = |sql: &str| -> rusqlite::Result<()> {
        n += conn.execute(sql, [])? as u64;
        Ok(())
    };
    // Marked first: the replacements are new rows and may take a lost ID.
    // (`NULL NOT IN` an empty table is true: nullable columns check for
    // NULL first.)
    run("UPDATE usage_source SET model = -1, repaired = 1
         WHERE model NOT IN (SELECT id FROM intern)")?;
    run("UPDATE session_gen SET model = NULL
         WHERE model IS NOT NULL AND model NOT IN (SELECT id FROM intern)")?;
    run("UPDATE session SET agent = -1 WHERE agent NOT IN (SELECT id FROM intern)")?;
    run("DELETE FROM project WHERE location NOT IN (SELECT id FROM location)")?;
    // Identity rows that point at a project or location that is gone would
    // point at whatever row takes the ID next.
    run("DELETE FROM project_fact
         WHERE project NOT IN (SELECT id FROM project)
            OR location NOT IN (SELECT id FROM location)")?;
    run("DELETE FROM alias
         WHERE src NOT IN (SELECT id FROM project)
            OR (dst IS NOT NULL AND dst NOT IN (SELECT id FROM project))")?;
    run("UPDATE project SET candidate = NULL
         WHERE candidate IS NOT NULL AND candidate NOT IN (SELECT id FROM project)")?;
    run("UPDATE turn SET location = NULL
         WHERE location IS NOT NULL AND location NOT IN (SELECT id FROM location)")?;
    run("UPDATE session_gen SET cwd = NULL
         WHERE cwd IS NOT NULL AND cwd NOT IN (SELECT id FROM location)")?;
    run("UPDATE usage_source SET location = -1, repaired = 1
         WHERE location IS NOT NULL AND location NOT IN (SELECT id FROM location)")?;
    run("UPDATE usage_source SET turn = NULL, repaired = 1
         WHERE turn IS NOT NULL AND turn NOT IN (SELECT id FROM turn)")?;
    run("UPDATE session_gen SET turn = NULL
         WHERE turn IS NOT NULL AND turn NOT IN (SELECT id FROM turn)")?;
    // Calls, the places of the files they changed and the changed files
    // (step 4): a row that points at a string, session, turn or place that
    // is gone is dropped rather than left to point at whatever takes the ID
    // next. Every session is read again, and the calls and places with it;
    // the turns are settled again from the items that are left.
    run("DELETE FROM call
         WHERE cid NOT IN (SELECT id FROM intern)
            OR session NOT IN (SELECT id FROM session)")?;
    run("UPDATE call SET turn = NULL
         WHERE turn IS NOT NULL AND turn NOT IN (SELECT id FROM turn)")?;
    run("DELETE FROM call_fact
         WHERE cid NOT IN (SELECT id FROM intern) OR path NOT IN (SELECT id FROM intern)
            OR location NOT IN (SELECT id FROM location)
            OR session NOT IN (SELECT id FROM session)")?;
    run("UPDATE call_fact SET worktree = NULL
         WHERE worktree IS NOT NULL AND worktree NOT IN (SELECT id FROM intern)")?;
    run("UPDATE call_fact SET rel = NULL
         WHERE rel IS NOT NULL AND rel NOT IN (SELECT id FROM intern)")?;
    run("DELETE FROM item
         WHERE cid NOT IN (SELECT id FROM intern) OR path NOT IN (SELECT id FROM intern)
            OR location NOT IN (SELECT id FROM location)
            OR turn NOT IN (SELECT id FROM turn)
            OR session NOT IN (SELECT id FROM session)")?;
    run("UPDATE session SET parent_call = NULL
         WHERE parent_call IS NOT NULL AND parent_call NOT IN (SELECT id FROM intern)")?;

    let strings = missing(
        conn,
        "intern",
        &[
            ("session", "source"),
            ("session", "sid"),
            ("session", "parent_source"),
            ("session", "parent_sid"),
            ("turn", "tid"),
        ],
    )?;
    for &id in &strings {
        conn.execute(
            "INSERT INTO intern (id, text) VALUES (?1, ?2)",
            params![id, lost_text("string", id)],
        )?;
    }
    n += strings.len() as u64;

    let sessions = missing(
        conn,
        "session",
        &[
            ("usage_source", "session"),
            ("turn", "session"),
            ("session_gen", "session"),
        ],
    )?;
    if !sessions.is_empty() {
        let source = ingest::intern(conn, LOST)?;
        for &id in &sessions {
            let sid = ingest::intern(conn, &lost_text("session", id))?;
            conn.execute(
                "INSERT INTO session (id, source, sid, agent, gen, cursor)
                 VALUES (?1, ?2, ?3, -1, coalesce((
                   SELECT max(gen) FROM (
                     SELECT gen FROM usage_source WHERE session = ?1
                     UNION ALL SELECT gen FROM turn WHERE session = ?1
                     UNION ALL SELECT gen FROM session_gen WHERE session = ?1)), 1), ?4)",
                params![id, source, sid, Cursor::START.to_bytes()],
            )?;
        }
        n += sessions.len() as u64;
    }

    let empty = ingest::intern(conn, b"")?;
    conn.execute(
        "UPDATE usage_source SET model = ?1 WHERE model = -1",
        params![empty],
    )?;
    let agent = ingest::intern(conn, LOST_AGENT)?;
    conn.execute(
        "UPDATE session SET agent = ?1 WHERE agent = -1",
        params![agent],
    )?;
    let unknown = ingest::location(conn, &ingest::Place::Unknown)?;
    conn.execute(
        "UPDATE usage_source SET location = ?1 WHERE location = -1",
        params![unknown],
    )?;
    // A placeholder session (or one whose source or session ID is a
    // placeholder) is never read again: no log has that ID, so its repaired
    // sources would wait for a replacement forever. They keep what the
    // salvage gave them.
    conn.execute(
        "UPDATE usage_source SET repaired = 0 WHERE repaired = 1 AND session IN (
           SELECT s.id FROM session s
           JOIN intern a ON a.id = s.source JOIN intern b ON b.id = s.sid
           WHERE substr(a.text, 1, 1) = x'FF' OR substr(b.text, 1, 1) = x'FF')",
        [],
    )?;
    Ok(n)
}

/// After lost rows nobody knows which sessions they belonged to: every
/// session reads its log again from the start, unfinished replays are
/// dropped. Returns the number of sessions.
fn restart_sessions(conn: &Connection) -> rusqlite::Result<u64> {
    let start = Cursor::START.to_bytes();
    for table in ["usage_source", "turn", "session_gen", "call", "item"] {
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
