//! Tool calls as the store keeps them (docs/design/usage.md「数据库」`call`,
//! `call_fact`): the kind and status codes, the call's key, and the
//! statements that write a call when its start and its result are read. What
//! the commands say (`cd`, `git -C`, stash and push facts) joins them with
//! step 5.
//!
//! A call is named by a 64-bit hash of its call ID, not by an `intern` row:
//! `intern` is never pruned, and a heavy day has 20 000 calls. Only the
//! calls a rule needs have a row: those that spawn a sub-agent (the child
//! session finds its turn by the call). An `Edit` call is a flag on its turn
//! and a `call_fact` row per file; the other calls are counted only in their
//! turn's last moment.

use std::path::PathBuf;

use lectern_services::session::{ToolKind, ToolStatus};
use rusqlite::{Connection, OptionalExtension, params};

use super::ingest::Place;

/// Where an edited file lies, as `Projects` said when the call was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathPlace {
    /// The repository's common directory, or the directory the file is in.
    pub(crate) place: Place,
    /// The worktree (or folder root) the file is in, when it is in one.
    pub(crate) worktree: Option<PathBuf>,
}

/// `call.kind`.
pub(crate) const KIND_SPAWN: i64 = 4;

/// Whether a kind of call has a row.
pub(crate) fn stored(kind: &ToolKind) -> Option<i64> {
    match kind {
        ToolKind::SpawnAgent => Some(KIND_SPAWN),
        ToolKind::Edit { .. }
        | ToolKind::Execute { .. }
        | ToolKind::Read { .. }
        | ToolKind::Search { .. }
        | ToolKind::Other => None,
    }
}

/// `call.status`. `STATUS_AMBIGUOUS` marks a row two different call IDs of
/// one session map to (see [`hash`]); it is never overwritten.
pub(crate) const STATUS_COMPLETED: i64 = 0;
pub(crate) const STATUS_AMBIGUOUS: i64 = 9;

pub(crate) fn status_code(status: ToolStatus) -> i64 {
    match status {
        ToolStatus::Completed => STATUS_COMPLETED,
        ToolStatus::Failed => 1,
        ToolStatus::Denied => 2,
        ToolStatus::Interrupted => 3,
    }
}

#[cfg(test)]
thread_local! {
    /// Tests: every call ID that starts with `collide-` has the same key.
    pub(crate) static COLLIDE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The key of a call ID within a session: FNV-1a over its bytes, 64 bits. It
/// does not change between versions, so a key stored by one version is found
/// by the next.
///
/// Two call IDs of one session with the same key (about one in 10^10 for a
/// session of 100 000 calls) are told apart only by their start times: the
/// second start finds the row of the first with another `started_at`, marks
/// it `STATUS_AMBIGUOUS` and is not stored. No sub-agent session
/// finds its turn by an ambiguous row; it is found by the time it began
/// instead.
pub(crate) fn hash(id: &str) -> i64 {
    #[cfg(test)]
    if COLLIDE.with(std::cell::Cell::get) && id.starts_with("collide-") {
        return 7;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h as i64
}

/// A call's start. Read again (a re-read after a migration, or a replay
/// that meets a call already stored) it is left as it is; a start with the
/// key of another call marks the row ambiguous. Returns whether the key was
/// taken by another call.
pub(crate) fn start(
    conn: &Connection,
    session: i64,
    generation: i64,
    cid: i64,
    turn: Option<i64>,
    at: i64,
    kind: i64,
) -> rusqlite::Result<bool> {
    let added = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO call (session, gen, cid, turn, started_at, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![session, generation, cid, turn, at, kind])?;
    if added == 1 {
        return Ok(false);
    }
    let marked = conn
        .prepare_cached(
            "UPDATE call SET status = ?4
             WHERE session = ?1 AND gen = ?2 AND cid = ?3 AND started_at != ?5",
        )?
        .execute(params![session, generation, cid, STATUS_AMBIGUOUS, at])?;
    Ok(marked > 0)
}

/// A call's result. Returns the call's turn: `Some(None)` for a call
/// without one, None when the call has no row (a call of another kind, a
/// start in an archived month).
pub(crate) fn finish(
    conn: &Connection,
    session: i64,
    generation: i64,
    cid: i64,
    at: i64,
    status: i64,
) -> rusqlite::Result<Option<Option<i64>>> {
    conn.prepare_cached(
        "UPDATE call SET finished_at = ?4,
           status = CASE WHEN status = ?6 THEN status ELSE ?5 END
         WHERE session = ?1 AND gen = ?2 AND cid = ?3
         RETURNING turn",
    )?
    .query_row(
        params![session, generation, cid, at, status, STATUS_AMBIGUOUS],
        |r| r.get(0),
    )
    .optional()
}

/// What the world looked like for one file of a call; written once.
pub(crate) fn fact(
    conn: &Connection,
    session: i64,
    cid: i64,
    path: i64,
    place: (i64, Option<i64>),
    at: i64,
) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT OR IGNORE INTO call_fact (session, cid, path, location, worktree, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![session, cid, path, place.0, place.1, at])?;
    Ok(())
}
