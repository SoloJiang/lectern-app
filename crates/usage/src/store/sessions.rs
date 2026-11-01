//! The reads behind the `Session` query (docs/design/usage.md「接口」
//! `SessionUsage`): a session's tokens with its sub-agents', and the tokens
//! of each of its own turns.

use std::path::{Path, PathBuf};

use lectern_services::session::SessionKey;
use rusqlite::{Connection, OptionalExtension, params};

use super::cold::{TOTAL_SEGMENT, TOTAL_SESSION};
use super::ingest::path_bytes;
use super::segments;
use super::{Store, StoreError, read_transaction};

/// What a `Session` query shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionTotals {
    /// The session's and its sub-agents' tokens in millionths: input,
    /// output, cache read, cache write.
    pub(crate) micro: [u64; 4],
    /// The session's own turns that have usage, in order: the `TurnId` and
    /// the whole tokens of the usage counted in the turn, not divided
    /// between targets.
    pub(crate) turns: Vec<(String, [u64; 4])>,
    /// Some session of the tree may include inherited usage.
    pub(crate) inherited: bool,
    /// The branches the session's tokens went to, and the tokens of the
    /// branches in all.
    pub(crate) branches: Vec<BranchTotals>,
}

/// What a segment holds, for the rows of a `Session` query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BranchTotals {
    /// The segment; 0 for tokens that went to no segment.
    pub(crate) segment: i64,
    /// The segment's ID bytes (all zero for none).
    pub(crate) sid: [u8; 16],
    /// The repository's common directory.
    pub(crate) repo: Option<PathBuf>,
    /// The branch's newest name; None for none and for a detached HEAD.
    pub(crate) name: Option<String>,
    /// The branch is the default branch, or the HEAD is detached.
    pub(crate) unassigned: bool,
    /// The session's tokens on it (millionths), and its tokens in all.
    pub(crate) mine: [u64; 4],
    pub(crate) all: [u64; 4],
    /// The part of `mine` that could not be given a branch.
    pub(crate) unattributable: [u64; 4],
}

/// What a `Worktree` query shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorktreeTotals {
    pub(crate) default: bool,
    /// The tokens on the branch in all, and the review part (millionths).
    pub(crate) all: [u64; 4],
    pub(crate) review: [u64; 4],
    /// When the branch began, when it is known (milliseconds).
    pub(crate) since: Option<i64>,
}

fn micro(row: &rusqlite::Row<'_>, from: usize) -> rusqlite::Result<[u64; 4]> {
    let mut out = [0u64; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u64::try_from(row.get::<_, i64>(from + i)?).unwrap_or(0);
    }
    Ok(out)
}

fn add(a: &mut [u64; 4], b: [u64; 4]) {
    for (x, y) in a.iter_mut().zip(b) {
        *x = x.saturating_add(y);
    }
}

/// The tokens of a segment in all and the review part: the hot facts and
/// what the archive summed (docs/design/usage.md「冷热分层」).
fn segment_totals(conn: &Connection, segment: i64) -> rusqlite::Result<([u64; 4], [u64; 4])> {
    let mut all = [0u64; 4];
    let mut review = [0u64; 4];
    let mut hot = conn.prepare_cached(
        "SELECT review, coalesce(sum(input), 0), coalesce(sum(output), 0),
                coalesce(sum(cache_read), 0), coalesce(sum(cache_write), 0)
         FROM fact WHERE segment = ?1 GROUP BY review",
    )?;
    let mut rows = hot.query(params![segment])?;
    while let Some(r) = rows.next()? {
        let m = micro(r, 1)?;
        add(&mut all, m);
        if r.get::<_, i64>(0)? != 0 {
            add(&mut review, m);
        }
    }
    let mut cold = conn.prepare_cached(
        "SELECT review, input, output, cache_read, cache_write FROM cold_total
         WHERE kind = ?1 AND target = ?2",
    )?;
    let mut rows = cold.query(params![TOTAL_SEGMENT, segment])?;
    while let Some(r) = rows.next()? {
        let m = micro(r, 1)?;
        add(&mut all, m);
        if r.get::<_, i64>(0)? != 0 {
            add(&mut review, m);
        }
    }
    Ok((all, review))
}

impl Store {
    /// None when the database does not know the session.
    pub(crate) fn session_totals(
        &self,
        key: &SessionKey,
    ) -> Result<Option<SessionTotals>, StoreError> {
        self.read(|conn| {
            read_transaction(conn, |conn| {
                let found: Option<(i64, i64)> = conn
                    .prepare_cached(
                        "SELECT s.id, s.gen FROM session s
                         JOIN intern a ON a.id = s.source JOIN intern b ON b.id = s.sid
                         WHERE a.text = ?1 AND b.text = ?2",
                    )?
                    .query_row(params![key.source.0.as_bytes(), key.id.as_bytes()], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .optional()?;
                let Some((id, generation)) = found else {
                    return Ok(None);
                };
                let mut out = SessionTotals::default();
                // The session and everything below it.
                let tree: Vec<(i64, bool)> = conn
                    .prepare_cached(
                        "WITH RECURSIVE tree(id) AS (
                           SELECT ?1
                           UNION
                           SELECT c.id FROM session c
                             JOIN session p ON p.source = c.parent_source AND p.sid = c.parent_sid
                             JOIN tree ON tree.id = p.id)
                         SELECT s.id, s.inheritance_unknown FROM session s JOIN tree ON tree.id = s.id",
                    )?
                    .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                out.inherited = tree.iter().any(|(_, unknown)| *unknown);
                let sessions = tree
                    .iter()
                    .map(|(s, _)| s.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                let hot: [i64; 4] = conn
                    .prepare_cached(&format!(
                        "SELECT coalesce(sum(input), 0), coalesce(sum(output), 0),
                                coalesce(sum(cache_read), 0), coalesce(sum(cache_write), 0)
                         FROM fact WHERE session IN ({sessions})"
                    ))?
                    .query_row([], |r| {
                        Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?])
                    })?;
                let cold: [i64; 4] = conn
                    .prepare_cached(&format!(
                        "SELECT coalesce(sum(input), 0), coalesce(sum(output), 0),
                                coalesce(sum(cache_read), 0), coalesce(sum(cache_write), 0)
                         FROM cold_total WHERE kind = {TOTAL_SESSION} AND target IN ({sessions})"
                    ))?
                    .query_row([], |r| {
                        Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?])
                    })?;
                for i in 0..4 {
                    out.micro[i] = u64::try_from(hot[i]).unwrap_or(0)
                        + u64::try_from(cold[i]).unwrap_or(0);
                }
                let mut stmt = conn.prepare_cached(
                    "SELECT i.text, sum(s.input), sum(s.output), sum(s.cache_read), sum(s.cache_write)
                     FROM usage_source s
                     JOIN usage_value v ON v.key = s.key AND v.source = s.id
                     JOIN turn t ON t.id = s.turn
                     JOIN intern i ON i.id = t.tid
                     WHERE s.session = ?1 AND s.gen = ?2
                     GROUP BY s.turn ORDER BY t.seq",
                )?;
                let rows = stmt.query_map(params![id, generation], |r| {
                    let text: Vec<u8> = r.get(0)?;
                    let mut t = [0u64; 4];
                    for (i, slot) in t.iter_mut().enumerate() {
                        *slot = u64::try_from(r.get::<_, i64>(i + 1)?).unwrap_or(0);
                    }
                    Ok((String::from_utf8_lossy(&text).into_owned(), t))
                })?;
                out.turns = rows.collect::<rusqlite::Result<_>>()?;
                out.branches = branches(conn, &sessions)?;
                Ok(Some(out))
            })
        })
    }

    /// The branch `name` (a full reference name) of the repository at
    /// `common_dir` as it is now: its tokens and when it began; None when no
    /// segment of the repository has the name.
    pub(crate) fn worktree_totals(
        &self,
        common_dir: &Path,
        name: &str,
        now_ms: i64,
    ) -> Result<Option<WorktreeTotals>, StoreError> {
        self.read(|conn| {
            read_transaction(conn, |conn| {
                let repo: Option<i64> = conn
                    .prepare_cached("SELECT id FROM location WHERE kind = 0 AND path = ?1")?
                    .query_row(params![path_bytes(common_dir)], |r| r.get(0))
                    .optional()?;
                let Some(repo) = repo else {
                    return Ok(None);
                };
                let Some(found) = segments::lookup(conn, repo, name, now_ms)? else {
                    return Ok(None);
                };
                let (default, since): (bool, Option<i64>) = conn
                    .prepare_cached("SELECT is_default, begin_at FROM segment WHERE id = ?1")?
                    .query_row(params![found.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let (all, review) = segment_totals(conn, found.id)?;
                Ok(Some(WorktreeTotals {
                    default,
                    all,
                    review,
                    since,
                }))
            })
        })
    }
}

/// The branches a set of sessions put tokens on, each with the tokens of
/// the branch in all. The tokens that went to no segment, the default branch
/// and detached heads are one "unassigned" entry.
fn branches(conn: &Connection, sessions: &str) -> rusqlite::Result<Vec<BranchTotals>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT coalesce(segment, 0), kind, coalesce(sum(input), 0), coalesce(sum(output), 0),
                coalesce(sum(cache_read), 0), coalesce(sum(cache_write), 0)
         FROM fact WHERE session IN ({sessions}) GROUP BY segment, kind"
    ))?;
    let mut mine: Vec<(i64, i64, [u64; 4])> = Vec::new();
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        mine.push((r.get(0)?, r.get(1)?, micro(r, 2)?));
    }
    let mut out: Vec<BranchTotals> = Vec::new();
    let mut unassigned = BranchTotals {
        segment: 0,
        sid: [0; 16],
        repo: None,
        name: None,
        unassigned: true,
        mine: [0; 4],
        all: [0; 4],
        unattributable: [0; 4],
    };
    for (segment, kind, tokens) in mine {
        if segment == 0 {
            add(&mut unassigned.mine, tokens);
            add(&mut unassigned.all, tokens);
            if kind != 2 {
                add(&mut unassigned.unattributable, tokens);
            }
            continue;
        }
        type Row = (Vec<u8>, i64, bool, i64, Option<Vec<u8>>);
        let found: Option<Row> = conn
            .prepare_cached(
                "SELECT s.sid, s.kind, s.is_default, s.repo, l.path FROM segment s
                 LEFT JOIN location l ON l.id = s.repo WHERE s.id = ?1",
            )?
            .query_row(params![segment], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .optional()?;
        let Some((sid, kind, default, _, repo)) = found else {
            // The segment is lost (a salvage): its tokens have no branch.
            add(&mut unassigned.mine, tokens);
            add(&mut unassigned.all, tokens);
            add(&mut unassigned.unattributable, tokens);
            continue;
        };
        if default || kind == segments::DETACHED {
            add(&mut unassigned.mine, tokens);
            add(&mut unassigned.all, tokens);
            continue;
        }
        let (all, _) = segment_totals(conn, segment)?;
        let mut id = [0u8; 16];
        let n = sid.len().min(16);
        id[..n].copy_from_slice(&sid[..n]);
        out.push(BranchTotals {
            segment,
            sid: id,
            repo: repo.map(|p| {
                use std::os::unix::ffi::OsStringExt;
                PathBuf::from(std::ffi::OsString::from_vec(p))
            }),
            name: segments::current_name(conn, segment)?,
            unassigned: false,
            mine: tokens,
            all,
            unattributable: [0; 4],
        });
    }
    // The branches the session used most first.
    out.sort_by(|a, b| {
        let total = |t: &[u64; 4]| t.iter().fold(0u64, |s, v| s.saturating_add(*v));
        total(&b.mine).cmp(&total(&a.mine)).then(a.sid.cmp(&b.sid))
    });
    if unassigned.mine.iter().any(|v| *v > 0) {
        out.push(unassigned);
    }
    Ok(out)
}
