//! Where a turn's tokens go (docs/design/usage.md「分摊」), and the fact rows
//! that follow from it.
//!
//! A turn is settled once it has ended and its changed files are known
//! (`item`): its targets, weighted, are written to `share` and its counted
//! sources get their fact rows again. Until then its usage is attributed
//! provisionally (see [`source_targets`]).
//!
//! The rules, in order (the first that applies decides):
//!
//! 1. the turn's own changes, and its sub-agents' (the weights of the files
//!    changed, per place and segment);
//! 2. the turn went into worktrees (a command, a file read or searched, a
//!    file written without a change): their segments, equally;
//! 3. the worktree the session ran in changed branch during the turn, and
//!    the nearest changes of the session are in it: the segments it was on;
//! 4. the nearest turn of the session that has changes, before it, else after;
//! 5. a session without changes of its own follows the turn of its parent
//!    that spawned it;
//! 6. the segment the turn was observed in (or, for a turn nobody watched,
//!    the branch its log named);
//! 7. the place of the turn's cwd, with no segment.
//!
//! `turn.changed` says which: 0 the cwd or an observation, 1 changes of its
//! own, 2 followed. A target is a place with a segment (`segment` 0 for
//! none) and a kind: 0 a segment, 1 a branch that could not be told, 2 no
//! branch at all (a file or a cwd outside any worktree).

use std::collections::{HashMap, HashSet};

use lectern_logging::debug;
use rusqlite::{Connection, OptionalExtension, params};

use super::cold::Frozen;
use super::dedup::{self, Deltas};
use super::observe::{OBS_SWITCHED, OBS_TRACKED, get_ids, get_pairs};

/// Where a turn's attribution comes from (`turn.changed`).
pub(crate) const FROM_CWD: i64 = 0;
pub(crate) const FROM_CHANGES: i64 = 1;
pub(crate) const FROM_FOLLOWED: i64 = 2;

/// `share.kind` / `fact.kind`.
pub(crate) const KIND_SEGMENT: i64 = 0;
pub(crate) const KIND_UNATTRIBUTABLE: i64 = 1;
pub(crate) const KIND_NONE: i64 = 2;

/// A place and segment with its weight in a turn's share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) location: i64,
    /// The segment; 0 for none.
    pub(crate) segment: i64,
    pub(crate) kind: i64,
    pub(crate) weight: u64,
}

impl Target {
    fn seg(location: i64, segment: i64) -> Target {
        Target {
            location,
            segment,
            kind: KIND_SEGMENT,
            weight: 1,
        }
    }
}

/// The rows of `share` of a turn.
pub(crate) fn shares(conn: &Connection, turn: i64) -> rusqlite::Result<Vec<Target>> {
    let mut stmt = conn.prepare_cached(
        "SELECT location, segment, kind, weight FROM share WHERE turn = ?1
         ORDER BY location, segment, kind",
    )?;
    let rows = stmt.query_map(params![turn], |r| {
        Ok(Target {
            location: r.get(0)?,
            segment: r.get(1)?,
            kind: r.get(2)?,
            weight: u64::try_from(r.get::<_, i64>(3)?).unwrap_or(0),
        })
    })?;
    rows.collect()
}

/// The first byte of a share as `cold_anchor.shares` keeps it now.
const ANCHOR_V2: u8 = 2;

/// A share as `cold_anchor.shares` keeps it: a version byte, then for each
/// target the place, the segment, the kind and the weight, 8 little-endian
/// bytes each. (The first version had the place and the weight only.)
pub(crate) fn encode(targets: &[Target]) -> Vec<u8> {
    if targets.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(1 + targets.len() * 32);
    out.push(ANCHOR_V2);
    for t in targets {
        out.extend_from_slice(&t.location.to_le_bytes());
        out.extend_from_slice(&t.segment.to_le_bytes());
        out.extend_from_slice(&t.kind.to_le_bytes());
        out.extend_from_slice(&t.weight.to_le_bytes());
    }
    out
}

fn decode(bytes: &[u8]) -> Vec<Target> {
    let word = |c: &[u8]| i64::from_le_bytes(c.try_into().unwrap_or([0; 8]));
    if bytes.first() == Some(&ANCHOR_V2) && (bytes.len() - 1).is_multiple_of(32) {
        return bytes[1..]
            .chunks_exact(32)
            .map(|c| Target {
                location: word(&c[..8]),
                segment: word(&c[8..16]),
                kind: word(&c[16..24]),
                weight: u64::try_from(word(&c[24..])).unwrap_or(0),
            })
            .collect();
    }
    bytes
        .chunks_exact(16)
        .map(|c| Target {
            location: word(&c[..8]),
            segment: 0,
            kind: KIND_UNATTRIBUTABLE,
            weight: u64::try_from(word(&c[8..])).unwrap_or(0),
        })
        .collect()
}

/// The share of the latest archived turn with changes of a session: what
/// its hot turns follow when no hot turn before them has changes.
fn anchor(conn: &Connection, session: i64) -> rusqlite::Result<Option<Vec<Target>>> {
    let found: Option<Vec<u8>> = conn
        .prepare_cached("SELECT shares FROM cold_anchor WHERE session = ?1 AND kind = 0")?
        .query_row(params![session], |r| r.get(0))
        .optional()?;
    Ok(found.map(|b| decode(&b)).filter(|d| !d.is_empty()))
}

struct TurnRow {
    id: i64,
    session: i64,
    generation: i64,
    seq: i64,
    started_at: i64,
    location: Option<i64>,
    worktree: Option<i64>,
    done: bool,
    changed: i64,
}

fn turn_row(conn: &Connection, id: i64) -> rusqlite::Result<Option<TurnRow>> {
    conn.prepare_cached(
        "SELECT id, session, gen, seq, started_at, location, done, changed, worktree
         FROM turn WHERE id = ?1",
    )?
    .query_row(params![id], |r| {
        Ok(TurnRow {
            id: r.get(0)?,
            session: r.get(1)?,
            generation: r.get(2)?,
            seq: r.get(3)?,
            started_at: r.get(4)?,
            location: r.get(5)?,
            done: r.get(6)?,
            changed: r.get(7)?,
            worktree: r.get(8)?,
        })
    })
    .optional()
}

/// The nearest settled turn with changes of its own in a session's
/// generation, before `seq` (`before`) or after it.
fn nearest(conn: &Connection, t: &TurnRow, before: bool) -> rusqlite::Result<Option<i64>> {
    let sql = if before {
        "SELECT id FROM turn WHERE session = ?1 AND gen = ?2 AND seq < ?3 AND done = 1 AND changed = 1
         ORDER BY seq DESC LIMIT 1"
    } else {
        "SELECT id FROM turn WHERE session = ?1 AND gen = ?2 AND seq > ?3 AND done = 1 AND changed = 1
         ORDER BY seq ASC LIMIT 1"
    };
    conn.prepare_cached(sql)?
        .query_row(params![t.session, t.generation, t.seq], |r| r.get(0))
        .optional()
}

/// The targets the usage of a turn is split over for now. `fallback` is
/// the place of the source itself (its turn's cwd).
///
/// A settled turn has its `share` rows. One not settled yet follows the
/// nearest settled turn of its session that has changes (docs/design/usage.md
/// 「轮次的观察与改动项」: 临时归属), else it is the cwd's place.
pub(crate) fn source_targets(
    conn: &Connection,
    turn: Option<i64>,
    fallback: i64,
) -> rusqlite::Result<Vec<Target>> {
    let Some(turn) = turn else {
        return Ok(vec![fallback_target(conn, fallback)?]);
    };
    let Some(t) = turn_row(conn, turn)? else {
        return Ok(vec![fallback_target(conn, fallback)?]);
    };
    let found = if t.done {
        shares(conn, t.id)?
    } else {
        match nearest(conn, &t, true)? {
            Some(before) => shares(conn, before)?,
            None => anchor(conn, t.session)?.unwrap_or_default(),
        }
    };
    Ok(if found.is_empty() {
        vec![fallback_target(conn, fallback)?]
    } else {
        found
    })
}

/// The place alone, with no segment: a repository whose branch is not known,
/// or a directory outside any repository.
fn fallback_target(conn: &Connection, location: i64) -> rusqlite::Result<Target> {
    let kind: Option<i64> = conn
        .prepare_cached("SELECT kind FROM location WHERE id = ?1")?
        .query_row(params![location], |r| r.get(0))
        .optional()?;
    Ok(Target {
        location,
        segment: 0,
        kind: if kind == Some(0) {
            KIND_UNATTRIBUTABLE
        } else {
            KIND_NONE
        },
        weight: 1,
    })
}

/// The changed files of a set of turns, weighted per place: the changed
/// lines plus the number of different files (ADR 0020: 改动行数 + 改动文件数).
fn weights(conn: &Connection, turn: i64, sessions: &[i64]) -> rusqlite::Result<Vec<Target>> {
    let list = sessions
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let sql = weights_sql(&list);
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![turn], |r| {
        let lines: i64 = r.get(3)?;
        let files: i64 = r.get(4)?;
        Ok(Target {
            location: r.get(0)?,
            segment: r.get(1)?,
            kind: r.get(2)?,
            weight: u64::try_from(lines.max(0) + files.max(0)).unwrap_or(0),
        })
    })?;
    rows.collect()
}

/// The statement behind [`weights`]: `?1` the turn, `list` the sessions
/// below it, as comma separated IDs. A file's target is the place and
/// segment its `call_fact` says; one without a segment is a branch not
/// known, or none (`state` 8).
pub(crate) fn weights_sql(list: &str) -> String {
    let columns = "i.location AS location, coalesce(f.segment, 0) AS segment,
                   CASE WHEN f.state & 8 != 0 THEN 2 WHEN f.segment IS NULL THEN 1 ELSE 0 END AS kind,
                   i.lines AS lines, i.path AS path";
    format!(
        "SELECT location, segment, kind, sum(lines), count(DISTINCT path) FROM (
           SELECT {columns} FROM item i LEFT JOIN call_fact f ON f.id = i.cf WHERE i.turn = ?1
           UNION ALL
           SELECT {columns} FROM turn t
             JOIN session s ON s.id = t.session AND s.gen = t.gen
             JOIN item i ON i.turn = t.id
             LEFT JOIN call_fact f ON f.id = i.cf
             WHERE t.session IN ({list}) )
         GROUP BY location, segment, kind ORDER BY location, segment, kind"
    )
}

/// What was observed about a turn (`turn_obs`), fixed when it was settled.
struct Observed {
    flags: i64,
    start_seg: Option<i64>,
    end_seg: Option<i64>,
    stays: Vec<i64>,
    enter: Vec<(i64, i64)>,
}

fn observed(conn: &Connection, turn: i64) -> rusqlite::Result<Option<Observed>> {
    conn.prepare_cached(
        "SELECT o.flags, o.start_seg, o.end_seg, o.stays, o.enter FROM turn t
         JOIN turn_obs o ON o.session = t.session AND o.tid = t.tid WHERE t.id = ?1",
    )?
    .query_row(params![turn], |r| {
        let stays: Option<Vec<u8>> = r.get(3)?;
        let enter: Option<Vec<u8>> = r.get(4)?;
        Ok(Observed {
            flags: r.get(0)?,
            start_seg: r.get(1)?,
            end_seg: r.get(2)?,
            stays: stays.map(|b| get_ids(&b)).unwrap_or_default(),
            enter: enter.map(|b| get_pairs(&b)).unwrap_or_default(),
        })
    })
    .optional()
}

/// Whether a turn changed a file in a worktree.
fn changed_in(conn: &Connection, turn: i64, worktree: i64) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM item i JOIN call_fact f ON f.id = i.cf
                        WHERE i.turn = ?1 AND f.worktree = ?2)",
    )?
    .query_row(params![turn, worktree], |r| r.get(0))
}

/// The turn of the parent session a child session was spawned from: the
/// turn that holds the spawning call, else (the log does not say) the turn
/// running when the child began.
fn parent_turn(conn: &Connection, child: i64) -> rusqlite::Result<Option<i64>> {
    let found: Option<(i64, Option<i64>, i64)> = conn
        .prepare_cached(
            "SELECT p.id, c.parent_call, p.gen FROM session c
             JOIN session p ON p.source = c.parent_source AND p.sid = c.parent_sid
             WHERE c.id = ?1",
        )?
        .query_row(params![child], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    let Some((parent, call, generation)) = found else {
        return Ok(None);
    };
    if let Some(cid) = call {
        let turn: Option<Option<i64>> = conn
            .prepare_cached(
                "SELECT turn FROM call
                 WHERE session = ?1 AND gen = ?2 AND cid = ?3 AND coalesce(status, 0) != 9",
            )?
            .query_row(params![parent, generation, cid], |r| r.get(0))
            .optional()?;
        if let Some(Some(turn)) = turn {
            return Ok(Some(turn));
        }
    }
    let first: Option<i64> = conn
        .prepare_cached(
            "SELECT coalesce(
               (SELECT min(t.started_at) FROM turn t JOIN session c ON c.id = t.session AND c.gen = t.gen
                WHERE c.id = ?1),
               (SELECT min(at) FROM usage_source WHERE session = ?1))",
        )?
        .query_row(params![child], |r| r.get(0))?;
    let Some(first) = first else {
        return Ok(None);
    };
    conn.prepare_cached(
        "SELECT id FROM turn WHERE session = ?1 AND gen = ?2 AND started_at <= ?3
         ORDER BY seq DESC LIMIT 1",
    )?
    .query_row(params![parent, generation, first], |r| r.get(0))
    .optional()
}

/// The sessions spawned from a turn.
fn children(conn: &Connection, t: &TurnRow) -> rusqlite::Result<Vec<i64>> {
    let all: Vec<i64> = conn
        .prepare_cached(
            "SELECT c.id FROM session c JOIN session p
               ON p.source = c.parent_source AND p.sid = c.parent_sid
             WHERE p.id = ?1",
        )?
        .query_map(params![t.session], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for child in all {
        if parent_turn(conn, child)? == Some(t.id) {
            out.push(child);
        }
    }
    Ok(out)
}

/// The sessions below `roots`, the roots included.
fn descendants(conn: &Connection, roots: &[i64]) -> rusqlite::Result<Vec<i64>> {
    let mut seen: HashSet<i64> = roots.iter().copied().collect();
    let mut order: Vec<i64> = roots.to_vec();
    let mut next = roots.to_vec();
    let mut stmt = conn.prepare_cached(
        "SELECT c.id FROM session c JOIN session p
           ON p.source = c.parent_source AND p.sid = c.parent_sid
         WHERE p.id = ?1",
    )?;
    while let Some(session) = next.pop() {
        let kids: Vec<i64> = stmt
            .query_map(params![session], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for kid in kids {
            if seen.insert(kid) {
                order.push(kid);
                next.push(kid);
            }
        }
    }
    Ok(order)
}

/// Where a turn's usage goes by the rules, and which rule decided.
fn resolve(conn: &Connection, t: &TurnRow) -> rusqlite::Result<(Vec<Target>, i64)> {
    // 1. The turn's own changes and the changes of the sub-agents it spawned.
    let kids = children(conn, t)?;
    let below = descendants(conn, &kids)?;
    let own = weights(conn, t.id, &below)?;
    if !own.is_empty() {
        return Ok((own, FROM_CHANGES));
    }
    let seen = observed(conn, t.id)?;
    // 2. The worktrees the turn went into, as they were then.
    if let Some(o) = &seen
        && !o.enter.is_empty()
    {
        let targets = o.enter.iter().map(|&(l, g)| Target::seg(l, g)).collect();
        return Ok((targets, FROM_CWD));
    }
    // 3. The worktree the session ran in changed branch during the turn, and
    // the session's nearest changes are in it.
    if let Some(o) = &seen
        && o.flags & OBS_TRACKED != 0
        && o.flags & OBS_SWITCHED != 0
        && let (Some(wt), Some(location)) = (t.worktree, t.location)
    {
        let mut near = nearest(conn, t, true)?;
        if near.is_none() {
            near = nearest(conn, t, false)?;
        }
        if let Some(near) = near
            && changed_in(conn, near, wt)?
        {
            let mut segs: Vec<i64> = Vec::new();
            for seg in o
                .start_seg
                .into_iter()
                .chain(o.end_seg)
                .chain(o.stays.iter().copied())
            {
                if !segs.contains(&seg) {
                    segs.push(seg);
                }
            }
            if !segs.is_empty() {
                let targets = segs.into_iter().map(|g| Target::seg(location, g)).collect();
                return Ok((targets, FROM_CWD));
            }
        }
    }
    // 4. The nearest turn of the session with changes: before, then after.
    for before in [true, false] {
        match nearest(conn, t, before)? {
            Some(found) => {
                let share = shares(conn, found)?;
                if !share.is_empty() {
                    return Ok((share, FROM_FOLLOWED));
                }
            }
            // The turns before the hot ones are cold.
            None if before => {
                if let Some(share) = anchor(conn, t.session)? {
                    return Ok((share, FROM_FOLLOWED));
                }
            }
            None => {}
        }
    }
    // 5. A session without changes follows the turn that spawned it.
    if let Some(parent) = parent_turn(conn, t.session)?
        && let Some(p) = turn_row(conn, parent)?
        && p.done
        && p.changed != FROM_CWD
    {
        let share = shares(conn, p.id)?;
        if !share.is_empty() {
            return Ok((share, FROM_FOLLOWED));
        }
    }
    // 6. The segment the turn was observed in, or the log's branch.
    let place = match t.location {
        Some(l) => Some(l),
        None => conn
            .prepare_cached("SELECT location FROM usage_source WHERE turn = ?1 LIMIT 1")?
            .query_row(params![t.id], |r| r.get(0))
            .optional()?,
    };
    let Some(place) = place else {
        return Ok((Vec::new(), FROM_CWD));
    };
    if let Some(o) = &seen {
        let mut segs: Vec<i64> = o.end_seg.into_iter().collect();
        for seg in &o.stays {
            if !segs.contains(seg) {
                segs.push(*seg);
            }
        }
        if !segs.is_empty() {
            let targets = segs.into_iter().map(|g| Target::seg(place, g)).collect();
            return Ok((targets, FROM_CWD));
        }
    }
    // 7. The branch is not known: the place of the cwd alone.
    Ok((vec![fallback_target(conn, place)?], FROM_CWD))
}

/// Settles turns: `work` holds the turns to settle; a turn whose
/// attribution moved adds the turns that depend on it. Fact rows follow the
/// shares. Turns in frozen months are left as they are. Returns the number
/// of turns settled.
pub(crate) fn settle_all(
    conn: &Connection,
    work: Vec<i64>,
    frozen: &Frozen,
    deltas: &mut Deltas,
) -> rusqlite::Result<usize> {
    let mut queued: HashSet<i64> = work.iter().copied().collect();
    let mut work = work;
    let mut count: HashMap<i64, u32> = HashMap::new();
    let mut settled = 0;
    while let Some(id) = work.pop() {
        queued.remove(&id);
        // A turn is looked at a few times at most: a loop of dependencies
        // cannot run for ever.
        let n = count.entry(id).or_default();
        *n += 1;
        if *n > 4 {
            continue;
        }
        let Some(t) = turn_row(conn, id)? else {
            continue;
        };
        if frozen.contains(t.started_at) {
            continue;
        }
        let (dist, origin) = resolve(conn, &t)?;
        let moved = settle(conn, &t, &dist, origin, frozen, deltas)?;
        settled += 1;
        if moved {
            for dependent in dependents(conn, &t, origin)? {
                if queued.insert(dependent) {
                    work.push(dependent);
                }
            }
        }
    }
    Ok(settled)
}

/// Writes a turn's shares, marks it settled and gives its counted sources
/// their fact rows again when the targets differ from the ones they have.
/// Returns whether the turn's attribution changed.
fn settle(
    conn: &Connection,
    t: &TurnRow,
    dist: &[Target],
    origin: i64,
    frozen: &Frozen,
    deltas: &mut Deltas,
) -> rusqlite::Result<bool> {
    // The facts of a turn not settled yet were written under whatever was
    // known then: they are written again.
    let same = t.done && shares(conn, t.id)? == dist;
    if t.done && same && t.changed == origin {
        return Ok(false);
    }
    if !t.done || !same {
        conn.prepare_cached("DELETE FROM share WHERE turn = ?1")?
            .execute(params![t.id])?;
        let total: u64 = dist.iter().map(|d| d.weight).sum();
        for target in dist {
            conn.prepare_cached(
                "INSERT INTO share (turn, location, segment, kind, weight, total, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![
                t.id,
                target.location,
                target.segment,
                target.kind,
                i64::try_from(target.weight).unwrap_or(i64::MAX),
                i64::try_from(total).unwrap_or(i64::MAX),
                t.started_at
            ])?;
        }
    }
    conn.prepare_cached("UPDATE turn SET done = 1, changed = ?2 WHERE id = ?1")?
        .execute(params![t.id, origin])?;
    debug!(
        "turn settled";
        origin = origin,
        places = dist.len(),
        weight = dist.iter().map(|d| d.weight).sum::<u64>()
    );
    if !same {
        reattribute(conn, t.id, frozen, deltas)?;
    }
    Ok(true)
}

/// The turns whose attribution depends on `t`'s (`origin` is what it has
/// now): when changes are or were its source, the turns of its session
/// without changes of their own that follow it (those between its
/// neighbours that have some) and the turn of the parent that spawned its
/// session (the parent's weights count this session's files); always the
/// sessions it spawned (they follow it).
fn dependents(conn: &Connection, t: &TurnRow, origin: i64) -> rusqlite::Result<Vec<i64>> {
    let mut out = Vec::new();
    if origin == FROM_CHANGES || t.changed == FROM_CHANGES {
        let has_before = nearest(conn, t, true)?.is_some() || anchor(conn, t.session)?.is_some();
        let after = match nearest(conn, t, false)? {
            Some(id) => turn_row(conn, id)?.map_or(i64::MAX, |n| n.seq),
            None => i64::MAX,
        };
        // Turns before the first turn with changes follow it too.
        let lower = if has_before { t.seq } else { i64::MIN };
        let ids: Vec<i64> = conn
            .prepare_cached(
                "SELECT id FROM turn WHERE session = ?1 AND gen = ?2 AND done = 1 AND changed != 1
                   AND seq > ?3 AND seq < ?4 AND id != ?5",
            )?
            .query_map(params![t.session, t.generation, lower, after, t.id], |r| {
                r.get(0)
            })?
            .collect::<rusqlite::Result<_>>()?;
        out.extend(ids);
        if let Some(parent) = parent_turn(conn, t.session)? {
            out.push(parent);
        }
    }
    for child in children(conn, t)? {
        let ids: Vec<i64> = conn
            .prepare_cached(
                "SELECT id FROM turn WHERE session = ?1 AND done = 1 AND changed != 1
                   AND gen = (SELECT gen FROM session WHERE id = ?1)",
            )?
            .query_map(params![child], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        out.extend(ids);
    }
    Ok(out)
}

/// Replaces the fact rows of a turn's counted sources with ones following
/// its current targets. Sources in frozen months stay as archived.
pub(crate) fn reattribute(
    conn: &Connection,
    turn: i64,
    frozen: &Frozen,
    deltas: &mut Deltas,
) -> rusqlite::Result<()> {
    let sources: Vec<(i64, i64)> = conn
        .prepare_cached(
            "SELECT s.id, s.at FROM usage_source s
             WHERE s.turn = ?1
               AND EXISTS (SELECT 1 FROM usage_value v WHERE v.key = s.key AND v.source = s.id)",
        )?
        .query_map(params![turn], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (source, at) in sources {
        if frozen.contains(at) {
            continue;
        }
        dedup::delete_facts(conn, source, deltas)?;
        dedup::insert_facts(conn, source, deltas)?;
    }
    Ok(())
}
