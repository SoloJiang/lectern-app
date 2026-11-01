//! Segments: a branch's (or a detached HEAD's) stretch of existence
//! (docs/design/usage.md「分支观察」). A segment has a fixed ID, written into
//! the observations; renaming keeps it. It begins when the branch is created
//! (a reflog record), when it is first seen, or when a name that no longer
//! exists first appears in a log; it ends when its reflog file is deleted
//! or when a branch of its name is created again.
//!
//! What ends a segment because commits were lost (a reset, `branch -f`,
//! `checkout -B`), the boundaries and the uses they make come with step 6.

use rusqlite::{Connection, OptionalExtension, params};

use crate::gitread::BranchLog;

/// `segment.kind`.
pub(crate) const BRANCH: i64 = 0;
pub(crate) const DETACHED: i64 = 1;
/// Only a name: the branch is gone, or was made after the turn.
pub(crate) const NAME_ONLY: i64 = 2;

/// `segment.begin_reason`.
pub(crate) const BEGIN_CREATED: i64 = 1;
pub(crate) const BEGIN_FIRST_SEEN: i64 = 2;
pub(crate) const BEGIN_RECREATED: i64 = 3;
pub(crate) const BEGIN_DETACHED: i64 = 4;
pub(crate) const BEGIN_NAME: i64 = 5;

/// `segment.end_reason`.
pub(crate) const END_REMOVED: i64 = 1;
pub(crate) const END_RECREATED: i64 = 2;
pub(crate) const END_REATTACHED: i64 = 3;

/// A segment found by its name at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) id: i64,
    pub(crate) kind: i64,
}

fn create(
    conn: &Connection,
    repo: i64,
    kind: i64,
    wt: Option<i64>,
    prev: Option<i64>,
    reason: i64,
    at: Option<i64>,
) -> rusqlite::Result<i64> {
    conn.prepare_cached(
        "INSERT INTO segment (sid, repo, kind, prev, wt, begin_reason, begin_at)
         VALUES (randomblob(16), ?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![repo, kind, prev, wt, reason, at])?;
    Ok(conn.last_insert_rowid())
}

/// A new segment with a name from `at` on (NULL: from the beginning).
pub(crate) fn create_named(
    conn: &Connection,
    repo: i64,
    kind: i64,
    name: &str,
    prev: Option<i64>,
    reason: i64,
    at: Option<i64>,
) -> rusqlite::Result<i64> {
    let id = create(conn, repo, kind, None, prev, reason, at)?;
    add_name(conn, id, name, at)?;
    Ok(id)
}

pub(crate) fn add_name(
    conn: &Connection,
    segment: i64,
    name: &str,
    at: Option<i64>,
) -> rusqlite::Result<()> {
    conn.prepare_cached("INSERT INTO segment_name (segment, name, at) VALUES (?1, ?2, ?3)")?
        .execute(params![segment, name, at])?;
    Ok(())
}

/// The newest name of a segment.
pub(crate) fn current_name(conn: &Connection, segment: i64) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(
        "SELECT name FROM segment_name WHERE segment = ?1
         ORDER BY coalesce(at, -9223372036854775808) DESC, id DESC LIMIT 1",
    )?
    .query_row(params![segment], |r| r.get(0))
    .optional()
}

/// The segment that is open (not ended) and has `name` now.
pub(crate) fn open_named(
    conn: &Connection,
    repo: i64,
    name: &str,
) -> rusqlite::Result<Option<(i64, Option<i64>, i64)>> {
    // Names are few per segment: the candidates by name, then their newest name.
    let candidates: Vec<(i64, Option<i64>, i64)> = conn
        .prepare_cached(
            "SELECT s.id, s.begin_at, s.begin_reason FROM segment_name n
             JOIN segment s ON s.id = n.segment
             WHERE n.name = ?1 AND s.repo = ?2 AND s.kind = 0 AND s.end_at IS NULL
               AND s.end_reason IS NULL",
        )?
        .query_map(params![name, repo], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for c in candidates.into_iter().rev() {
        if current_name(conn, c.0)?.as_deref() == Some(name) {
            return Ok(Some(c));
        }
    }
    Ok(None)
}

/// The open segment of a branch; made when there is none: seen first now,
/// its beginning not known.
pub(crate) fn ensure_open(conn: &Connection, repo: i64, name: &str) -> rusqlite::Result<i64> {
    match open_named(conn, repo, name)? {
        Some((id, _, _)) => Ok(id),
        None => create_named(conn, repo, BRANCH, name, None, BEGIN_FIRST_SEEN, None),
    }
}

/// A branch was created at `at` (a branch reflog's first line, or a
/// `Created from` record): a branch first seen just before is the same one.
pub(crate) fn created(conn: &Connection, repo: i64, name: &str, at: i64) -> rusqlite::Result<i64> {
    match open_named(conn, repo, name)? {
        Some((id, None, BEGIN_FIRST_SEEN)) => {
            conn.prepare_cached(
                "UPDATE segment SET begin_at = ?2, begin_reason = ?3 WHERE id = ?1",
            )?
            .execute(params![id, at, BEGIN_CREATED])?;
            Ok(id)
        }
        // The same record again, or a record of the same second.
        Some((id, Some(begin), _)) if begin >= at - 2_000 => Ok(id),
        Some((id, _, _)) => {
            end(conn, id, END_RECREATED, at)?;
            create_named(
                conn,
                repo,
                BRANCH,
                name,
                Some(id),
                BEGIN_RECREATED,
                Some(at),
            )
        }
        None => create_named(conn, repo, BRANCH, name, None, BEGIN_CREATED, Some(at)),
    }
}

pub(crate) fn end(conn: &Connection, id: i64, reason: i64, at: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "UPDATE segment SET end_reason = ?2, end_at = ?3 WHERE id = ?1 AND end_reason IS NULL",
    )?
    .execute(params![id, reason, at])?;
    Ok(())
}

/// The branch `from` was renamed to `to` at `at`: the same segment.
pub(crate) fn renamed(
    conn: &Connection,
    repo: i64,
    from: &str,
    to: &str,
    at: i64,
) -> rusqlite::Result<i64> {
    let id = match open_named(conn, repo, to)? {
        Some((id, _, _)) => id,
        None => match open_named(conn, repo, from)? {
            Some((id, _, _)) => id,
            None => create_named(conn, repo, BRANCH, from, None, BEGIN_FIRST_SEEN, None)?,
        },
    };
    if current_name(conn, id)?.as_deref() != Some(to) {
        add_name(conn, id, to, Some(at))?;
    }
    Ok(id)
}

/// The detached segment of a worktree that is open, if any.
fn open_detached(conn: &Connection, repo: i64, wt: i64) -> rusqlite::Result<Option<i64>> {
    conn.prepare_cached(
        "SELECT id FROM segment WHERE repo = ?1 AND kind = 1 AND wt = ?2 AND end_reason IS NULL
         ORDER BY id DESC LIMIT 1",
    )?
    .query_row(params![repo, wt], |r| r.get(0))
    .optional()
}

/// HEAD of a worktree went detached at `at`.
pub(crate) fn detached(conn: &Connection, repo: i64, wt: i64, at: i64) -> rusqlite::Result<i64> {
    match open_detached(conn, repo, wt)? {
        Some(id) => Ok(id),
        None => create(
            conn,
            repo,
            DETACHED,
            Some(wt),
            None,
            BEGIN_DETACHED,
            Some(at),
        ),
    }
}

/// HEAD of a worktree is on a branch again: its detached segment ends.
pub(crate) fn reattached(conn: &Connection, repo: i64, wt: i64, at: i64) -> rusqlite::Result<()> {
    if let Some(id) = open_detached(conn, repo, wt)? {
        end(conn, id, END_REATTACHED, at)?;
    }
    Ok(())
}

/// The open detached segment of a worktree, or a new one with its beginning
/// not known.
pub(crate) fn detached_now(conn: &Connection, repo: i64, wt: i64) -> rusqlite::Result<i64> {
    match open_detached(conn, repo, wt)? {
        Some(id) => Ok(id),
        None => create(conn, repo, DETACHED, Some(wt), None, BEGIN_DETACHED, None),
    }
}

/// The segment named `name` at `at` (milliseconds): the branch's own, else
/// the one that is only a name.
pub(crate) fn lookup(
    conn: &Connection,
    repo: i64,
    name: &str,
    at: i64,
) -> rusqlite::Result<Option<Found>> {
    let candidates: Vec<(i64, i64, Option<i64>, Option<i64>)> = conn
        .prepare_cached(
            "SELECT DISTINCT s.id, s.kind, s.begin_at, s.end_at FROM segment_name n
             JOIN segment s ON s.id = n.segment
             WHERE n.name = ?1 AND s.repo = ?2 AND s.kind IN (0, 2)",
        )?
        .query_map(params![name, repo], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut best: Option<Found> = None;
    for (id, kind, begin, end) in candidates {
        if begin.is_some_and(|b| at < b) || end.is_some_and(|e| at >= e) {
            continue;
        }
        // The name in effect at `at`.
        let name_then: Option<String> = conn
            .prepare_cached(
                "SELECT name FROM segment_name WHERE segment = ?1
                   AND (at IS NULL OR at <= ?2)
                 ORDER BY coalesce(at, -9223372036854775808) DESC, id DESC LIMIT 1",
            )?
            .query_row(params![id, at], |r| r.get(0))
            .optional()?;
        if name_then.as_deref() != Some(name) {
            continue;
        }
        let found = Found { id, kind };
        if best.is_none_or(|b| {
            (found.kind, std::cmp::Reverse(found.id)) < (b.kind, std::cmp::Reverse(b.id))
        }) {
            best = Some(found);
        }
    }
    Ok(best)
}

/// A segment that had the name at some time and was open at `at`: a branch
/// that was renamed since, found by its old name.
pub(crate) fn lookup_ever(
    conn: &Connection,
    repo: i64,
    name: &str,
    at: i64,
) -> rusqlite::Result<Option<Found>> {
    conn.prepare_cached(
        "SELECT s.id, s.kind FROM segment_name n JOIN segment s ON s.id = n.segment
         WHERE n.name = ?1 AND s.repo = ?2 AND s.kind = 0
           AND (s.begin_at IS NULL OR s.begin_at <= ?3)
           AND (s.end_at IS NULL OR s.end_at > ?3)
         ORDER BY s.id DESC LIMIT 1",
    )?
    .query_row(params![name, repo, at], |r| {
        Ok(Found {
            id: r.get(0)?,
            kind: r.get(1)?,
        })
    })
    .optional()
}

/// The segment of a branch name at `at`, from what is known; a log read
/// (`log`, when the branch has one) makes the segments of the branch as
/// the reflog tells, and a name nothing explains becomes a segment that is
/// only a name.
pub(crate) fn resolve(
    conn: &Connection,
    repo: i64,
    name: &str,
    at: i64,
    log: Option<&BranchLog>,
) -> rusqlite::Result<i64> {
    if let Some(f) = lookup(conn, repo, name, at)? {
        return Ok(f.id);
    }
    if let Some(log) = log
        && log.exists
    {
        import(conn, repo, name, log)?;
        if let Some(f) = lookup(conn, repo, name, at)? {
            return Ok(f.id);
        }
    }
    // The branch is gone, or was made after `at`: only the name is known.
    // One such segment serves every turn that names it.
    let existing: Option<i64> = conn
        .prepare_cached(
            "SELECT s.id FROM segment s JOIN segment_name n ON n.segment = s.id
             WHERE s.repo = ?1 AND s.kind = 2 AND n.name = ?2 LIMIT 1",
        )?
        .query_row(params![repo, name], |r| r.get(0))
        .optional()?;
    match existing {
        Some(id) => Ok(id),
        None => create_named(conn, repo, NAME_ONLY, name, None, BEGIN_NAME, None),
    }
}

/// Makes the segment of an existing branch from its reflog: it began when
/// the log says it was created (a log cut short, or one without a creation
/// line, leaves the beginning open to the past), and carries the names the
/// renames in the log tell.
pub(crate) fn import(
    conn: &Connection,
    repo: i64,
    name: &str,
    log: &BranchLog,
) -> rusqlite::Result<i64> {
    let id = match open_named(conn, repo, name)? {
        Some((id, None, BEGIN_FIRST_SEEN)) => {
            if let Some(at) = log.created {
                conn.prepare_cached(
                    "UPDATE segment SET begin_at = ?2, begin_reason = ?3 WHERE id = ?1",
                )?
                .execute(params![id, at, BEGIN_CREATED])?;
            }
            id
        }
        Some((id, _, _)) => id,
        None => match log.created {
            Some(at) => create_named(conn, repo, BRANCH, name, None, BEGIN_CREATED, Some(at))?,
            None => create_named(conn, repo, BRANCH, name, None, BEGIN_FIRST_SEEN, None)?,
        },
    };
    // Names before the last rename, when the segment knows only the last.
    let names: i64 = conn
        .prepare_cached("SELECT count(*) FROM segment_name WHERE segment = ?1")?
        .query_row(params![id], |r| r.get(0))?;
    if names == 1 && !log.renames.is_empty() {
        let first = &log.renames[0].1;
        let last = log.renames.last().map_or(0, |r| r.0);
        conn.prepare_cached("UPDATE segment_name SET at = ?2 WHERE segment = ?1")?
            .execute(params![id, last])?;
        add_name(conn, id, first, None)?;
        for (at, _, to) in &log.renames[..log.renames.len() - 1] {
            add_name(conn, id, to, Some(*at))?;
        }
    }
    Ok(id)
}

/// An observation found the branch of a segment to be the default branch.
pub(crate) fn mark_default(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.prepare_cached("UPDATE segment SET is_default = 1 WHERE id = ?1 AND is_default = 0")?
        .execute(params![id])?;
    Ok(())
}
