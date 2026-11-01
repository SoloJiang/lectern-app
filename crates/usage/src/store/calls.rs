//! Tool calls as the store keeps them (docs/design/usage.md「数据库」`call`,
//! `call_fact`): the kind and status codes, and the statements that write a
//! call when its start and its result are read. What the commands say
//! (`cd`, `git -C`, stash and push facts) joins them with step 5.

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
    /// The file's path below that root; the whole path when there is none.
    pub(crate) rel: PathBuf,
}

/// `call.kind`.
pub(crate) const KIND_EDIT: i64 = 0;
pub(crate) const KIND_EXECUTE: i64 = 1;
pub(crate) const KIND_READ: i64 = 2;
pub(crate) const KIND_SEARCH: i64 = 3;
pub(crate) const KIND_SPAWN: i64 = 4;
pub(crate) const KIND_OTHER: i64 = 5;

pub(crate) fn kind_code(kind: &ToolKind) -> i64 {
    match kind {
        ToolKind::Edit { .. } => KIND_EDIT,
        ToolKind::Execute { .. } => KIND_EXECUTE,
        ToolKind::Read { .. } => KIND_READ,
        ToolKind::Search { .. } => KIND_SEARCH,
        ToolKind::SpawnAgent => KIND_SPAWN,
        ToolKind::Other => KIND_OTHER,
    }
}

/// `call.status`.
pub(crate) const STATUS_COMPLETED: i64 = 0;

pub(crate) fn status_code(status: ToolStatus) -> i64 {
    match status {
        ToolStatus::Completed => STATUS_COMPLETED,
        ToolStatus::Failed => 1,
        ToolStatus::Denied => 2,
        ToolStatus::Interrupted => 3,
    }
}

/// A call's start. Read again (a re-read after a migration, or a replay
/// that meets a call already stored) it is left as it is.
pub(crate) fn start(
    conn: &Connection,
    session: i64,
    generation: i64,
    cid: i64,
    turn: Option<i64>,
    at: i64,
    kind: i64,
) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT OR IGNORE INTO call (session, gen, cid, turn, started_at, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![session, generation, cid, turn, at, kind])?;
    Ok(())
}

/// A call's result. Returns the call's turn: `Some(None)` for a call
/// without one, None when the call is not stored (its start is in an
/// archived month, or was lost).
pub(crate) fn finish(
    conn: &Connection,
    session: i64,
    generation: i64,
    cid: i64,
    at: i64,
    status: i64,
) -> rusqlite::Result<Option<Option<i64>>> {
    conn.prepare_cached(
        "UPDATE call SET finished_at = ?4, status = ?5
         WHERE session = ?1 AND gen = ?2 AND cid = ?3
         RETURNING turn",
    )?
    .query_row(params![session, generation, cid, at, status], |r| r.get(0))
    .optional()
}

/// What the world looked like for one file of a call; written once.
pub(crate) fn fact(
    conn: &Connection,
    session: i64,
    cid: i64,
    path: i64,
    place: (i64, Option<i64>, Option<i64>),
    at: i64,
) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT OR IGNORE INTO call_fact (session, cid, path, location, worktree, rel, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![session, cid, path, place.0, place.1, place.2, at])?;
    Ok(())
}
