//! One batch of a session's records and its cursor, in one transaction
//! (docs/design/usage.md「数据流与线程」「规范化与去重」).
//!
//! A `Batch::reset` starts a new generation of the session: the replayed
//! records go into it while the old generation still counts, and the batch
//! that reaches `at_end` deletes the old generation and makes the new one
//! current in the same transaction. A reset during a replay drops the
//! unfinished generation and starts again. Keys deduplicate across
//! generations, so a replay never counts a usage twice.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lectern_logging::{debug, warn};
use lectern_services::session::{Agent, Batch, Cursor, Record, SessionKey, UsageRecord};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::schema::{LOCATION_DIR, LOCATION_GIT, PROJECT_GIT};
use super::{Store, StoreError, dedup};
use crate::key;
use crate::round::MICRO;
use crate::tokens::normalize;

/// Where a path belongs: a repository's git common directory, a directory
/// that is in no repository, or nowhere known.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Place {
    Git(PathBuf),
    Dir(PathBuf),
    Unknown,
}

/// The session a batch belongs to.
pub(crate) struct SessionInfo<'a> {
    pub(crate) key: &'a SessionKey,
    pub(crate) agent: &'a Agent,
    pub(crate) parent: Option<&'a SessionKey>,
    pub(crate) inheritance_unknown: bool,
    /// The place of the session's starting cwd: the cwd until a `Context`
    /// record says otherwise.
    pub(crate) start: &'a Place,
}

/// What [`Store::ingest`] writes.
pub(crate) struct Ingest<'a> {
    pub(crate) session: SessionInfo<'a>,
    pub(crate) batch: &'a Batch,
    /// The place of every cwd of the batch's `Context` records.
    pub(crate) places: &'a HashMap<PathBuf, Place>,
    /// The log file as it was just before the read; kept when the batch
    /// reaches the end, so the next run skips an unchanged file.
    pub(crate) stamp: Option<FileStamp>,
}

/// A log file's size, modification time and inode: equal stamps mean the
/// file has not changed since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileStamp {
    pub(crate) size: u64,
    pub(crate) mtime_ns: i64,
    pub(crate) inode: u64,
}

impl FileStamp {
    /// The stamp of `path` now; None when it cannot be read.
    pub(crate) fn of(path: &Path) -> Option<FileStamp> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        let mtime_ns = m
            .mtime()
            .saturating_mul(1_000_000_000)
            .saturating_add(m.mtime_nsec());
        Some(FileStamp {
            size: m.len(),
            mtime_ns,
            inode: m.ino(),
        })
    }

    pub(crate) fn to_bytes(self) -> [u8; 24] {
        let mut out = [0u8; 24];
        out[..8].copy_from_slice(&self.size.to_le_bytes());
        out[8..16].copy_from_slice(&self.mtime_ns.to_le_bytes());
        out[16..].copy_from_slice(&self.inode.to_le_bytes());
        out
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<FileStamp> {
        let bytes: &[u8; 24] = bytes.try_into().ok()?;
        let word = |i: usize| -> [u8; 8] { bytes[i..i + 8].try_into().unwrap_or([0; 8]) };
        Some(FileStamp {
            size: u64::from_le_bytes(word(0)),
            mtime_ns: i64::from_le_bytes(word(8)),
            inode: u64::from_le_bytes(word(16)),
        })
    }
}

/// What a batch did, for logs and the reading cadence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ingested {
    pub(crate) records: usize,
    pub(crate) sources: usize,
    /// Keys whose counting source was chosen again.
    pub(crate) keys: usize,
    pub(crate) replay_started: bool,
    pub(crate) replay_done: bool,
    pub(crate) at_end: bool,
    /// Cursor offsets before and after.
    pub(crate) from: u64,
    pub(crate) to: u64,
}

/// Most tokens of one kind a record may carry: their millionths must fit in
/// an SQLite integer.
const MAX_TOKENS: u64 = (i64::MAX as u64) / MICRO;

fn agent_label(agent: &Agent) -> String {
    match agent {
        Agent::ClaudeCode => "agent:claude-code".to_owned(),
        Agent::Codex => "agent:codex".to_owned(),
        Agent::Other(name) => format!("agent:other:{name}"),
    }
}

/// The agent of an `agent_label`.
pub(crate) fn agent_of(label: &[u8]) -> Agent {
    match label {
        b"agent:claude-code" => Agent::ClaudeCode,
        b"agent:codex" => Agent::Codex,
        other => Agent::Other(
            String::from_utf8_lossy(other.strip_prefix(b"agent:other:").unwrap_or(other))
                .as_ref()
                .into(),
        ),
    }
}

pub(crate) fn path_bytes(path: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes()
}

/// IDs looked up within one transaction. Dropped with it, so a rollback
/// never leaves a stale ID behind.
#[derive(Default)]
struct Ids {
    text: HashMap<Vec<u8>, i64>,
    place: HashMap<Place, i64>,
}

impl Ids {
    fn text(&mut self, conn: &Connection, text: &[u8]) -> rusqlite::Result<i64> {
        if let Some(&id) = self.text.get(text) {
            return Ok(id);
        }
        let id = intern(conn, text)?;
        self.text.insert(text.to_vec(), id);
        Ok(id)
    }

    fn place(&mut self, conn: &Connection, place: &Place) -> rusqlite::Result<i64> {
        if let Some(&id) = self.place.get(place) {
            return Ok(id);
        }
        let id = location(conn, place)?;
        self.place.insert(place.clone(), id);
        Ok(id)
    }
}

pub(crate) fn intern(conn: &Connection, text: &[u8]) -> rusqlite::Result<i64> {
    conn.prepare_cached("INSERT OR IGNORE INTO intern (text) VALUES (?1)")?
        .execute(params![text])?;
    conn.prepare_cached("SELECT id FROM intern WHERE text = ?1")?
        .query_row(params![text], |r| r.get(0))
}

/// The location of a place, created on first sight; a repository's
/// location gets its project at the same time (docs/design/usage.md
/// 「项目标识」: never opened until a `ProjectEvent` says otherwise).
pub(crate) fn location(conn: &Connection, place: &Place) -> rusqlite::Result<i64> {
    let (kind, path): (i64, &[u8]) = match place {
        Place::Git(p) => (LOCATION_GIT, path_bytes(p)),
        Place::Dir(p) => (LOCATION_DIR, path_bytes(p)),
        Place::Unknown => (LOCATION_DIR, b""),
    };
    conn.prepare_cached("INSERT OR IGNORE INTO location (kind, path) VALUES (?1, ?2)")?
        .execute(params![kind, path])?;
    let id: i64 = conn
        .prepare_cached("SELECT id FROM location WHERE kind = ?1 AND path = ?2")?
        .query_row(params![kind, path], |r| r.get(0))?;
    if kind == LOCATION_GIT {
        conn.prepare_cached(
            "INSERT OR IGNORE INTO project (pid, kind, location) VALUES (randomblob(16), ?1, ?2)",
        )?
        .execute(params![PROJECT_GIT, id])?;
    }
    Ok(id)
}

/// The reading state of one generation, kept between batches.
struct GenState {
    cwd: i64,
    model: Option<i64>,
    turn: Option<i64>,
}

struct SessionRow {
    id: i64,
    generation: i64,
    replay: Option<i64>,
    fresh: bool,
    from: u64,
}

impl Store {
    /// The saved cursor of a session; `START` for one never read.
    pub(crate) fn cursor(&self, key: &SessionKey) -> Result<Cursor, StoreError> {
        self.read(|conn| {
            let bytes: Option<Vec<u8>> = conn
                .prepare_cached(
                    "SELECT s.cursor FROM session s
                     JOIN intern a ON a.id = s.source JOIN intern b ON b.id = s.sid
                     WHERE a.text = ?1 AND b.text = ?2",
                )?
                .query_row(params![key.source.0.as_bytes(), key.id.as_bytes()], |r| {
                    r.get(0)
                })
                .optional()?;
            Ok(bytes
                .and_then(|b| Cursor::from_bytes(&b))
                .unwrap_or(Cursor::START))
        })
    }

    /// Writes a batch, its cursor and the re-chosen keys in one transaction.
    pub(crate) fn ingest(&self, input: &Ingest<'_>) -> Result<Ingested, StoreError> {
        #[cfg(test)]
        let fail_before_cursor = std::mem::take(&mut super::lock(&self.faults).before_cursor);
        #[cfg(not(test))]
        let fail_before_cursor = false;
        self.transaction(|tx| ingest(tx, input, fail_before_cursor))
    }
}

fn ingest(
    tx: &Transaction<'_>,
    input: &Ingest<'_>,
    fail_before_cursor: bool,
) -> rusqlite::Result<Ingested> {
    let mut ids = Ids::default();
    let info = &input.session;
    let batch = input.batch;
    let mut row = session_row(tx, &mut ids, info)?;
    let mut keys: HashSet<Vec<u8>> = HashSet::new();
    let mut out = Ingested {
        records: batch.records.len(),
        at_end: batch.at_end,
        from: row.from,
        to: batch.cursor.offset(),
        ..Ingested::default()
    };

    if batch.reset && !row.fresh {
        if let Some(unfinished) = row.replay {
            drop_generation(tx, row.id, unfinished, &mut keys)?;
        }
        let next = row.generation.max(row.replay.unwrap_or(row.generation)) + 1;
        row.replay = Some(next);
        out.replay_started = true;
        warn!("session log replayed from the start"; generation = next, records = batch.records.len());
    }
    let target = row.replay.unwrap_or(row.generation);
    let mut state = gen_state(tx, &mut ids, row.id, target, info.start)?;
    let mut next_seq: Option<i64> = None;
    let mut turns: HashMap<&str, (i64, i64)> = HashMap::new();

    for rec in &batch.records {
        match &rec.value {
            Record::Context { cwd, model, .. } => {
                let place = input
                    .places
                    .get(cwd.as_ref())
                    .cloned()
                    .unwrap_or_else(|| Place::Dir(cwd.to_path_buf()));
                state.cwd = ids.place(tx, &place)?;
                if let Some(m) = model {
                    state.model = Some(ids.text(tx, m.as_bytes())?);
                }
            }
            Record::TurnStarted { turn, at } => {
                let seq = match next_seq {
                    Some(s) => s,
                    None => tx
                        .prepare_cached(
                            "SELECT coalesce(max(seq), 0) + 1 FROM turn WHERE session = ?1 AND gen = ?2",
                        )?
                        .query_row(params![row.id, target], |r| r.get(0))?,
                };
                next_seq = Some(seq + 1);
                let tid = ids.text(tx, turn.0.as_bytes())?;
                tx.prepare_cached(
                    "INSERT OR IGNORE INTO turn (session, gen, tid, seq, started_at, location)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?
                .execute(params![row.id, target, tid, seq, at.0, state.cwd])?;
                let (turn_id, location) =
                    turn_of(tx, row.id, target, tid)?.unwrap_or((0, state.cwd));
                turns.insert(&turn.0, (turn_id, location));
                state.turn = Some(turn_id);
            }
            Record::TurnEnded { turn, at, .. } => {
                let tid = ids.text(tx, turn.0.as_bytes())?;
                tx.prepare_cached(
                    "UPDATE turn SET ended_at = ?4 WHERE session = ?1 AND gen = ?2 AND tid = ?3",
                )?
                .execute(params![row.id, target, tid, at.0])?;
            }
            Record::Usage(usage) => {
                let (turn, location) = match &usage.turn {
                    Some(t) => match turns.get(&*t.0) {
                        Some(&found) => (Some(found.0), found.1),
                        None => {
                            let tid = ids.text(tx, t.0.as_bytes())?;
                            match turn_of(tx, row.id, target, tid)? {
                                Some((id, loc)) => {
                                    turns.insert(&t.0, (id, loc));
                                    (Some(id), loc)
                                }
                                None => (None, state.cwd),
                            }
                        }
                    },
                    None => (state.turn, state.cwd),
                };
                let model = match &usage.model {
                    Some(m) => ids.text(tx, m.as_bytes())?,
                    None => match state.model {
                        Some(m) => m,
                        None => ids.text(tx, b"")?,
                    },
                };
                let source = Source {
                    session: row.id,
                    generation: target,
                    pos: rec.pos,
                    seq: rec.seq,
                    inherited: rec.inherited,
                    turn,
                    location,
                };
                let main = key::encode(&usage.key);
                insert_source(tx, &source, 0, &main, model, usage, &usage.tokens)?;
                out.sources += 1;
                if let Some(advisor) = &usage.advisor {
                    let advisor_key = key::advisor(main.clone());
                    let advisor_model = ids.text(tx, advisor.model.as_bytes())?;
                    insert_source(
                        tx,
                        &source,
                        1,
                        &advisor_key,
                        advisor_model,
                        usage,
                        &advisor.tokens,
                    )?;
                    out.sources += 1;
                    keys.insert(advisor_key);
                }
                keys.insert(main);
            }
            Record::ToolStarted(_) | Record::ToolFinished(_) | Record::RateLimits(_) => {}
        }
    }

    tx.prepare_cached(
        "INSERT INTO session_gen (session, gen, cwd, model, turn) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (session, gen) DO UPDATE SET
           cwd = excluded.cwd, model = excluded.model, turn = excluded.turn",
    )?
    .execute(params![row.id, target, state.cwd, state.model, state.turn])?;

    if batch.at_end
        && let Some(replay) = row.replay
    {
        drop_generation(tx, row.id, row.generation, &mut keys)?;
        row.generation = replay;
        row.replay = None;
        out.replay_done = true;
    }

    if fail_before_cursor {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            None,
        ));
    }
    // The stamp only stands for a read that reached the end; any other
    // batch clears it, so the next run reads the session again.
    let stamp = input
        .stamp
        .filter(|_| batch.at_end)
        .map(|s| s.to_bytes().to_vec());
    tx.prepare_cached(
        "UPDATE session SET cursor = ?2, gen = ?3, replay = ?4, reached_end = reached_end OR ?5,
           end_stamp = ?6
         WHERE id = ?1",
    )?
    .execute(params![
        row.id,
        batch.cursor.to_bytes(),
        row.generation,
        row.replay,
        batch.at_end,
        stamp
    ])?;

    out.keys = keys.len();
    for key in &keys {
        dedup::reselect(tx, key)?;
    }
    debug!(
        "batch stored";
        from = out.from,
        to = out.to,
        records = out.records,
        sources = out.sources,
        keys = out.keys,
        at_end = out.at_end
    );
    Ok(out)
}

/// The session's row, created on first sight.
fn session_row(
    tx: &Connection,
    ids: &mut Ids,
    info: &SessionInfo<'_>,
) -> rusqlite::Result<SessionRow> {
    let source = ids.text(tx, info.key.source.0.as_bytes())?;
    let sid = ids.text(tx, info.key.id.as_bytes())?;
    let agent = ids.text(tx, agent_label(info.agent).as_bytes())?;
    let parent = match info.parent {
        Some(p) => Some((
            ids.text(tx, p.source.0.as_bytes())?,
            ids.text(tx, p.id.as_bytes())?,
        )),
        None => None,
    };
    let found: Option<(i64, i64, Option<i64>, Vec<u8>)> = tx
        .prepare_cached(
            "SELECT id, gen, replay, cursor FROM session WHERE source = ?1 AND sid = ?2",
        )?
        .query_row(params![source, sid], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    match found {
        Some((id, generation, replay, cursor)) => {
            tx.prepare_cached(
                "UPDATE session SET agent = ?2, parent_source = ?3, parent_sid = ?4,
                   inheritance_unknown = ?5 WHERE id = ?1",
            )?
            .execute(params![
                id,
                agent,
                parent.map(|p| p.0),
                parent.map(|p| p.1),
                info.inheritance_unknown
            ])?;
            let cursor = Cursor::from_bytes(&cursor).unwrap_or(Cursor::START);
            Ok(SessionRow {
                id,
                generation,
                replay,
                fresh: cursor.is_start() && replay.is_none() && !has_rows(tx, id)?,
                from: cursor.offset(),
            })
        }
        None => {
            tx.prepare_cached(
                "INSERT INTO session (source, sid, agent, parent_source, parent_sid, cursor,
                                      inheritance_unknown)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![
                source,
                sid,
                agent,
                parent.map(|p| p.0),
                parent.map(|p| p.1),
                Cursor::START.to_bytes(),
                info.inheritance_unknown
            ])?;
            Ok(SessionRow {
                id: tx.last_insert_rowid(),
                generation: 1,
                replay: None,
                fresh: true,
                from: 0,
            })
        }
    }
}

fn has_rows(tx: &Connection, session: i64) -> rusqlite::Result<bool> {
    tx.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM usage_source WHERE session = ?1)
             OR EXISTS (SELECT 1 FROM turn WHERE session = ?1)",
    )?
    .query_row(params![session], |r| r.get(0))
}

fn gen_state(
    tx: &Connection,
    ids: &mut Ids,
    session: i64,
    generation: i64,
    start: &Place,
) -> rusqlite::Result<GenState> {
    let found: Option<(Option<i64>, Option<i64>, Option<i64>)> = tx
        .prepare_cached("SELECT cwd, model, turn FROM session_gen WHERE session = ?1 AND gen = ?2")?
        .query_row(params![session, generation], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    let (cwd, model, turn) = found.unwrap_or((None, None, None));
    let cwd = match cwd {
        Some(c) => c,
        None => ids.place(tx, start)?,
    };
    Ok(GenState { cwd, model, turn })
}

/// A turn's row and location in a generation.
fn turn_of(
    tx: &Connection,
    session: i64,
    generation: i64,
    tid: i64,
) -> rusqlite::Result<Option<(i64, i64)>> {
    tx.prepare_cached(
        "SELECT id, coalesce(location, 0) FROM turn WHERE session = ?1 AND gen = ?2 AND tid = ?3",
    )?
    .query_row(params![session, generation, tid], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .optional()
}

/// Deletes a generation's rows, collecting the keys to choose again.
fn drop_generation(
    tx: &Connection,
    session: i64,
    generation: i64,
    keys: &mut HashSet<Vec<u8>>,
) -> rusqlite::Result<()> {
    {
        let mut stmt = tx.prepare_cached(
            "SELECT DISTINCT key FROM usage_source WHERE session = ?1 AND gen = ?2",
        )?;
        let mut rows = stmt.query(params![session, generation])?;
        while let Some(r) = rows.next()? {
            keys.insert(r.get(0)?);
        }
    }
    for table in ["usage_source", "turn", "session_gen"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE session = ?1 AND gen = ?2"),
            params![session, generation],
        )?;
    }
    Ok(())
}

struct Source {
    session: i64,
    generation: i64,
    pos: u64,
    seq: u16,
    inherited: bool,
    turn: Option<i64>,
    location: i64,
}

fn clamp(n: u64) -> i64 {
    n.min(MAX_TOKENS) as i64
}

fn insert_source(
    tx: &Connection,
    s: &Source,
    part: i64,
    key: &[u8],
    model: i64,
    usage: &UsageRecord,
    raw: &lectern_services::session::RawTokens,
) -> rusqlite::Result<()> {
    let t = normalize(raw);
    tx.prepare_cached(
        "INSERT OR IGNORE INTO usage_source
           (key, session, gen, pos, seq, part, at, turn, location, model,
            input, output, cache_read, cache_write, sidechain, superseded, inherited)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
    )?
    .execute(params![
        key,
        s.session,
        s.generation,
        i64::try_from(s.pos).unwrap_or(i64::MAX),
        s.seq,
        part,
        usage.at.0,
        s.turn,
        s.location,
        model,
        clamp(t.input),
        clamp(t.output),
        clamp(t.cache_read),
        clamp(t.cache_write),
        usage.sidechain,
        usage.superseded,
        s.inherited
    ])?;
    Ok(())
}
