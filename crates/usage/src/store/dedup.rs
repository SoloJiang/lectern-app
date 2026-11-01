//! Which occurrence of a key counts, and its fact rows
//! (docs/design/usage.md「规范化与去重」).
//!
//! Every occurrence of a `UsageKey` is a source row. Inherited sources
//! (a replayed parent or fork prefix) and superseded `CodexTokenCount`
//! sources never count. Of the rest, one counts: not `sidechain` first, then
//! the larger total (ADR 0014 「保留合计最大的一条」), then the smallest
//! (timestamp, source ID, session ID, `pos`, `seq`), then the row ID. The
//! chosen source decides the session, turn and place of the usage.
//!
//! A counted source has one fact row per target of its turn's share
//! (`attrib`): the turn's cwd until the turn is settled and nothing better is
//! known.

use std::cmp::Ordering;

use rusqlite::{Connection, OptionalExtension, params};

use super::cold::{self, Frozen};
use crate::round;
use crate::window::BUCKET_MS;

struct Candidate {
    id: i64,
    sidechain: bool,
    total: u128,
    at: i64,
    source: Vec<u8>,
    session: Vec<u8>,
    pos: i64,
    seq: i64,
}

fn order(a: &Candidate, b: &Candidate) -> Ordering {
    a.sidechain
        .cmp(&b.sidechain)
        .then(b.total.cmp(&a.total))
        .then(a.at.cmp(&b.at))
        .then_with(|| a.source.cmp(&b.source))
        .then_with(|| a.session.cmp(&b.session))
        .then(a.pos.cmp(&b.pos))
        .then(a.seq.cmp(&b.seq))
        .then(a.id.cmp(&b.id))
}

const CANDIDATES: &str = "SELECT s.id, s.sidechain, s.input, s.output, s.cache_read, s.cache_write,
                s.at, src.text, sid.text, s.pos, s.seq
         FROM usage_source s
         JOIN session se ON se.id = s.session
         JOIN intern src ON src.id = se.source
         JOIN intern sid ON sid.id = se.sid
         WHERE s.key = ?1 AND s.inherited = 0 AND s.superseded = 0";

/// Chooses the counting source of `key` again and replaces its fact rows
/// when the choice changed. Runs inside the caller's transaction.
///
/// Frozen months (docs/design/usage.md「冷热分层」, archiving or archived)
/// are left alone: a key whose counted source lies in one keeps it, and
/// sources in one are not candidates. Their facts are written by the
/// archive itself ([`derive_month`]).
pub(crate) fn reselect(
    conn: &Connection,
    key: &[u8],
    frozen: &Frozen,
    deltas: &mut Deltas,
) -> rusqlite::Result<()> {
    let current: Option<i64> = conn
        .prepare_cached("SELECT source FROM usage_value WHERE key = ?1")?
        .query_row(params![key], |r| r.get(0))
        .optional()?;
    if let Some(source) = current
        && !frozen.is_empty()
        && counts_in(conn, source, frozen)?
    {
        return Ok(());
    }
    let best = if frozen.is_empty() {
        choose(conn, key, CANDIDATES)?
    } else {
        let sql = format!("{CANDIDATES} AND NOT {}", frozen.sql("s.at"));
        choose(conn, key, &sql)?
    };
    replace(conn, key, current, best, deltas)
}

/// Whether a counted source's usage lies in a frozen month: by its own
/// timestamp, or by its facts once the source row itself is archived.
fn counts_in(conn: &Connection, source: i64, frozen: &Frozen) -> rusqlite::Result<bool> {
    let at: Option<i64> = conn
        .prepare_cached("SELECT at FROM usage_source WHERE id = ?1")?
        .query_row(params![source], |r| r.get(0))
        .optional()?;
    let at = match at {
        Some(at) => Some(at),
        None => conn
            .prepare_cached("SELECT at FROM fact WHERE source = ?1 LIMIT 1")?
            .query_row(params![source], |r| r.get(0))
            .optional()?,
    };
    Ok(at.is_some_and(|at| frozen.contains(at)))
}

/// The facts of a frozen month's sources that do not count yet: records
/// that arrived after the month was frozen (a first read of an old log),
/// the sources with IDs above `after` (IDs only grow). Only keys without a
/// hot counted source are chosen, among these sources: the month's earlier
/// sources all went through the hot path, and one whose key's counted
/// source is already cold must not be counted again. Runs inside the
/// archive step's transaction.
pub(crate) fn derive_month(
    conn: &Connection,
    range: &std::ops::Range<i64>,
    after: i64,
) -> rusqlite::Result<usize> {
    let keys: Vec<Vec<u8>> = conn
        .prepare_cached(
            "SELECT DISTINCT s.key FROM usage_source s
             WHERE s.id > ?3 AND +s.at >= ?1 AND +s.at < ?2
               AND s.inherited = 0 AND s.superseded = 0
               AND NOT EXISTS (SELECT 1 FROM usage_value v WHERE v.key = s.key)",
        )?
        // A range of row IDs (the late ones), not of the month's timestamps.
        .query_map(params![range.start, range.end, after], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let sql = format!(
        "{CANDIDATES} AND s.id > {after} AND s.at >= {} AND s.at < {}",
        range.start, range.end
    );
    let mut n = 0;
    for key in &keys {
        if let Some(best) = choose(conn, key, &sql)? {
            replace(conn, key, None, Some(best), &mut Deltas::ignore())?;
            n += 1;
        }
    }
    Ok(n)
}

/// The best candidate the query (`CANDIDATES` with more conditions) gives.
fn choose(conn: &Connection, key: &[u8], sql: &str) -> rusqlite::Result<Option<i64>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let mut best: Option<Candidate> = None;
    let mut rows = stmt.query(params![key])?;
    while let Some(r) = rows.next()? {
        let total: u128 = (2..=5)
            .map(|i| r.get::<_, i64>(i).map(|v| v.max(0) as u128))
            .sum::<rusqlite::Result<u128>>()?;
        let c = Candidate {
            id: r.get(0)?,
            sidechain: r.get(1)?,
            total,
            at: r.get(6)?,
            source: r.get(7)?,
            session: r.get(8)?,
            pos: r.get(9)?,
            seq: r.get(10)?,
        };
        if best.as_ref().is_none_or(|b| order(&c, b) == Ordering::Less) {
            best = Some(c);
        }
    }
    Ok(best.map(|c| c.id))
}

/// Makes `best` the counted source of `key` in place of `current`.
fn replace(
    conn: &Connection,
    key: &[u8],
    current: Option<i64>,
    best: Option<i64>,
    deltas: &mut Deltas,
) -> rusqlite::Result<()> {
    if current == best {
        return Ok(());
    }
    if let Some(old) = current {
        delete_facts(conn, old, deltas)?;
    }
    match best {
        Some(id) => {
            insert_facts(conn, id, deltas)?;
            conn.prepare_cached(
                "INSERT INTO usage_value (key, source) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET source = excluded.source",
            )?
            .execute(params![key, id])?;
        }
        None => {
            conn.prepare_cached("DELETE FROM usage_value WHERE key = ?1")?
                .execute(params![key])?;
        }
    }
    Ok(())
}

/// The fact rows of a counted source: one per target of its turn
/// (`attrib::source_targets`), each token kind split by the targets' weights
/// into millionths of a token (`round::split`), so that the rows add up to
/// the source exactly. A source without a turn, or whose turn is not
/// settled and has no turn with changes before it, has one row at its
/// turn's cwd.
pub(crate) fn insert_facts(
    conn: &Connection,
    source: i64,
    deltas: &mut Deltas,
) -> rusqlite::Result<()> {
    const INSERT: &str = "INSERT INTO fact (at, bucket, source, location, project, agent, model,
                           session, turn, input, output, cache_read, cache_write, segment, kind)
         SELECT s.at, (s.at - (((s.at % ?2) + ?2) % ?2)) / ?2, s.id, ?3, p.id,
                se.agent, s.model, s.session, s.turn, ?4, ?5, ?6, ?7, nullif(?8, 0), ?9
         FROM usage_source s
         JOIN session se ON se.id = s.session
         LEFT JOIN project p ON p.id = (
             SELECT q.id FROM project q WHERE q.location = ?3 AND q.since <= s.at
             ORDER BY q.since DESC, q.id DESC LIMIT 1)
         WHERE s.id = ?1";
    type Own = (Option<i64>, i64, [i64; 4]);
    let (turn, location, tokens): Own = conn
        .prepare_cached(
            "SELECT turn, location, input, output, cache_read, cache_write
             FROM usage_source WHERE id = ?1",
        )?
        .query_row(params![source], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                [r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?],
            ))
        })?;
    let targets = super::attrib::source_targets(conn, turn, location)?;
    let weights: Vec<u64> = targets.iter().map(|t| t.weight).collect();
    let parts: Vec<Vec<u64>> = tokens
        .iter()
        .map(|&t| round::split(u64::try_from(t).unwrap_or(0), &weights))
        .collect();
    for (i, target) in targets.iter().enumerate() {
        let micro = |k: usize| i64::try_from(parts[k][i]).unwrap_or(i64::MAX);
        let values = params![
            source,
            BUCKET_MS,
            target.location,
            micro(0),
            micro(1),
            micro(2),
            micro(3),
            target.segment,
            target.kind
        ];
        if !deltas.on {
            conn.prepare_cached(INSERT)?.execute(values)?;
        } else {
            let sql = format!("{INSERT} RETURNING {DELTA_COLUMNS}");
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt.query_map(values, |r| delta(r, 1))?;
            for row in rows {
                deltas.facts.push(row?);
            }
        }
    }
    if deltas.buckets {
        add_buckets(conn, source, 1)?;
    }
    Ok(())
}

/// Deletes a source's fact rows.
pub(crate) fn delete_facts(
    conn: &Connection,
    source: i64,
    deltas: &mut Deltas,
) -> rusqlite::Result<()> {
    const DELETE: &str = "DELETE FROM fact WHERE source = ?1";
    if deltas.buckets {
        add_buckets(conn, source, -1)?;
    }
    if !deltas.on {
        conn.prepare_cached(DELETE)?.execute(params![source])?;
        return Ok(());
    }
    let sql = format!("{DELETE} RETURNING {DELTA_COLUMNS}");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![source], |r| delta(r, -1))?;
    for row in rows {
        deltas.facts.push(row?);
    }
    Ok(())
}

/// Adds (`sign` 1) or takes back (-1) a source's fact rows in
/// `fact_bucket`; a bucket row left without facts goes.
fn add_buckets(conn: &Connection, source: i64, sign: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO fact_bucket (bucket, location, project, agent, model, review,
                                  input, output, cache_read, cache_write, rows)
         SELECT bucket, location, coalesce(project, 0), agent, model, review,
                ?2 * input, ?2 * output, ?2 * cache_read, ?2 * cache_write, ?2
         FROM fact WHERE source = ?1
         ON CONFLICT (bucket, location, project, agent, model, review) DO UPDATE SET
           input = input + excluded.input, output = output + excluded.output,
           cache_read = cache_read + excluded.cache_read,
           cache_write = cache_write + excluded.cache_write, rows = rows + excluded.rows",
    )?
    .execute(params![source, sign])?;
    if sign < 0 {
        conn.prepare_cached(
            "DELETE FROM fact_bucket WHERE rows <= 0
               AND (bucket, location, project, agent, model, review) IN (
                 SELECT bucket, location, coalesce(project, 0), agent, model, review
                 FROM fact WHERE source = ?1)",
        )?
        .execute(params![source])?;
    }
    Ok(())
}

/// Sums `fact_bucket` again from every fact row (after a rebuild, or a
/// change the per-source upkeep does not cover).
pub(crate) fn rebuild_buckets(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DELETE FROM fact_bucket;
         INSERT INTO fact_bucket
           SELECT bucket, location, coalesce(project, 0), agent, model, review, sum(input),
                  sum(output), sum(cache_read), sum(cache_write), count(*)
           FROM fact GROUP BY bucket, location, coalesce(project, 0), agent, model, review;",
    )
}

const DELTA_COLUMNS: &str =
    "location, coalesce(project, 0), bucket, agent, model, input, output, cache_read, cache_write";

fn delta(r: &rusqlite::Row<'_>, sign: i64) -> rusqlite::Result<FactDelta> {
    let mut micro = [0i64; 4];
    for (i, slot) in micro.iter_mut().enumerate() {
        *slot = r.get::<_, i64>(5 + i)?.saturating_mul(sign);
    }
    Ok(FactDelta {
        location: r.get(0)?,
        project: r.get(1)?,
        bucket: r.get(2)?,
        agent: r.get(3)?,
        model: r.get(4)?,
        micro,
        rows: sign,
    })
}

/// A fact row added (`rows` 1, positive tokens) or removed (-1, negative
/// tokens) by a transaction: what the watched snapshots apply without
/// summing the facts again (docs/design/usage.md「数据流与线程」).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FactDelta {
    pub(crate) location: i64,
    /// The project the fact belongs to; 0 for none.
    pub(crate) project: i64,
    pub(crate) bucket: i64,
    pub(crate) agent: i64,
    pub(crate) model: i64,
    pub(crate) micro: [i64; 4],
    pub(crate) rows: i64,
}

/// The fact changes of one transaction, when collected (`on`), and whether
/// `fact_bucket` follows them row by row (`buckets`).
#[derive(Debug, Default)]
pub(crate) struct Deltas {
    on: bool,
    buckets: bool,
    pub(crate) facts: Vec<FactDelta>,
    /// Facts changed in a way the deltas do not say (their agent): the
    /// snapshots are computed again.
    pub(crate) unknown: bool,
}

impl Deltas {
    /// An ingest: deltas collected, buckets kept in step.
    pub(crate) fn collect() -> Deltas {
        Deltas {
            on: true,
            buckets: true,
            ..Deltas::default()
        }
    }

    /// Changes nobody watches: a rebuild (which sums the buckets again at
    /// its end), the archive's own facts of a frozen month (whose buckets go
    /// when the month is archived).
    pub(crate) fn ignore() -> Deltas {
        Deltas::default()
    }
}

/// Rebuilds the hot window's `usage_value` and `fact` from the sources. Its
/// own transaction. Frozen months keep their derived rows: their sources
/// may already be cold, and the archive owns them (docs/design/usage.md
/// 「冷热分层」「重建」).
pub(crate) fn rebuild(conn: &mut Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    rebuild_in(&tx)?;
    tx.commit()
}

pub(crate) fn rebuild_in(tx: &Connection) -> rusqlite::Result<()> {
    let frozen = cold::frozen(tx)?;
    // The turns that were settled are settled again from their items once
    // the facts are back, in the provisional state.
    let settled: Vec<i64> = tx
        .prepare(&format!(
            "SELECT id FROM turn WHERE done = 1 AND NOT {}",
            frozen.sql("started_at")
        ))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    tx.execute(
        &format!("DELETE FROM share WHERE NOT {}", frozen.sql("at")),
        [],
    )?;
    tx.execute(
        &format!(
            "UPDATE turn SET done = 0, changed = 0 WHERE done = 1 AND NOT {}",
            frozen.sql("started_at")
        ),
        [],
    )?;
    tx.execute(
        &format!("DELETE FROM fact WHERE NOT {}", frozen.sql("at")),
        [],
    )?;
    tx.execute(
        "DELETE FROM usage_value WHERE source NOT IN (SELECT source FROM fact)",
        [],
    )?;
    let keys: Vec<Vec<u8>> = tx
        .prepare(&format!(
            "SELECT DISTINCT key FROM usage_source WHERE NOT {}",
            frozen.sql("at")
        ))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut deltas = Deltas::ignore();
    for key in &keys {
        reselect(tx, key, &frozen, &mut deltas)?;
    }
    super::attrib::settle_all(tx, settled, &frozen, &mut deltas)?;
    rebuild_buckets(tx)
}
