//! Where a turn's tokens go (docs/design/usage.md「分摊」), and the fact rows
//! that follow from it.
//!
//! A turn is settled once it has ended and its changed files are known
//! (`item`): its targets, weighted, are written to `share` and its counted
//! sources get their fact rows again. Until then its usage is attributed
//! provisionally (see [`source_targets`]).
//!
//! The rules in this step, in order (the first that applies decides; the
//! branch parts of the design's rules come with step 5):
//!
//! 1. the turn's own changes, and its sub-agents' (the weights of the files
//!    changed, per place);
//! 4. the nearest turn of the session that has changes, before it, else after;
//! 5. a session without changes of its own follows the turn of its parent
//!    that spawned it;
//! 6. 7. the place of the turn's cwd.
//!
//! `turn.changed` says which: 0 the cwd, 1 changes of its own, 2 followed.

use std::collections::{HashMap, HashSet};

use lectern_logging::debug;
use rusqlite::{Connection, OptionalExtension, params};

use super::cold::Frozen;
use super::dedup::{self, Deltas};

/// Where a turn's attribution comes from (`turn.changed`).
pub(crate) const FROM_CWD: i64 = 0;
pub(crate) const FROM_CHANGES: i64 = 1;
pub(crate) const FROM_FOLLOWED: i64 = 2;

/// A place with its weight in a turn's share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) location: i64,
    pub(crate) weight: u64,
}

/// The rows of `share` of a turn, by place.
pub(crate) fn shares(conn: &Connection, turn: i64) -> rusqlite::Result<Vec<Target>> {
    let mut stmt = conn
        .prepare_cached("SELECT location, weight FROM share WHERE turn = ?1 ORDER BY location")?;
    let rows = stmt.query_map(params![turn], |r| {
        Ok(Target {
            location: r.get(0)?,
            weight: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
        })
    })?;
    rows.collect()
}

/// A share as `cold_anchor.shares` keeps it: the place and the weight, 8
/// little-endian bytes each.
pub(crate) fn encode(targets: &[Target]) -> Vec<u8> {
    let mut out = Vec::with_capacity(targets.len() * 16);
    for t in targets {
        out.extend_from_slice(&t.location.to_le_bytes());
        out.extend_from_slice(&t.weight.to_le_bytes());
    }
    out
}

fn decode(bytes: &[u8]) -> Vec<Target> {
    bytes
        .chunks_exact(16)
        .map(|c| Target {
            location: i64::from_le_bytes(c[..8].try_into().unwrap_or([0; 8])),
            weight: u64::from_le_bytes(c[8..].try_into().unwrap_or([0; 8])),
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
    done: bool,
    changed: i64,
}

fn turn_row(conn: &Connection, id: i64) -> rusqlite::Result<Option<TurnRow>> {
    conn.prepare_cached(
        "SELECT id, session, gen, seq, started_at, location, done, changed FROM turn WHERE id = ?1",
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
    let one = || {
        vec![Target {
            location: fallback,
            weight: 1,
        }]
    };
    let Some(turn) = turn else {
        return Ok(one());
    };
    let Some(t) = turn_row(conn, turn)? else {
        return Ok(one());
    };
    let found = if t.done {
        shares(conn, t.id)?
    } else {
        match nearest(conn, &t, true)? {
            Some(before) => shares(conn, before)?,
            None => anchor(conn, t.session)?.unwrap_or_default(),
        }
    };
    Ok(if found.is_empty() { one() } else { found })
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
        let lines: i64 = r.get(1)?;
        let files: i64 = r.get(2)?;
        Ok(Target {
            location: r.get(0)?,
            weight: u64::try_from(lines.max(0) + files.max(0)).unwrap_or(0),
        })
    })?;
    rows.collect()
}

/// The statement behind [`weights`]: `?1` the turn, `list` the sessions
/// below it, as comma separated IDs.
pub(crate) fn weights_sql(list: &str) -> String {
    format!(
        "SELECT location, sum(lines), count(DISTINCT path) FROM (
           SELECT location, lines, path FROM item WHERE turn = ?1
           UNION ALL
           SELECT i.location, i.lines, i.path FROM turn t
             JOIN session s ON s.id = t.session AND s.gen = t.gen
             JOIN item i ON i.turn = t.id
             WHERE t.session IN ({list}) )
         GROUP BY location ORDER BY location"
    )
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
    // 6. 7. The cwd's place.
    let place = match t.location {
        Some(l) => Some(l),
        None => conn
            .prepare_cached("SELECT location FROM usage_source WHERE turn = ?1 LIMIT 1")?
            .query_row(params![t.id], |r| r.get(0))
            .optional()?,
    };
    Ok((
        place
            .map(|location| Target {
                location,
                weight: 1,
            })
            .into_iter()
            .collect(),
        FROM_CWD,
    ))
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
                "INSERT INTO share (turn, location, weight, total, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                t.id,
                target.location,
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
