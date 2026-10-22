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
//! Step 1 attributes each usage to its turn's cwd: one fact row per counted
//! source, holding all of it.

use std::cmp::Ordering;

use rusqlite::{Connection, OptionalExtension, params};

use crate::round::MICRO;
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

/// Chooses the counting source of `key` again and replaces its fact rows
/// when the choice changed. Runs inside the caller's transaction.
pub(crate) fn reselect(conn: &Connection, key: &[u8]) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.id, s.sidechain, s.input, s.output, s.cache_read, s.cache_write,
                s.at, src.text, sid.text, s.pos, s.seq
         FROM usage_source s
         JOIN session se ON se.id = s.session
         JOIN intern src ON src.id = se.source
         JOIN intern sid ON sid.id = se.sid
         WHERE s.key = ?1 AND s.inherited = 0 AND s.superseded = 0",
    )?;
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
    drop(rows);
    let best = best.map(|c| c.id);
    let current: Option<i64> = conn
        .prepare_cached("SELECT source FROM usage_value WHERE key = ?1")?
        .query_row(params![key], |r| r.get(0))
        .optional()?;
    if current == best {
        return Ok(());
    }
    if let Some(old) = current {
        conn.prepare_cached("DELETE FROM fact WHERE source = ?1")?
            .execute(params![old])?;
    }
    match best {
        Some(id) => {
            insert_facts(conn, id)?;
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

/// The fact rows of a counted source. Step 1 has one target per usage, the
/// cwd's place: one row with every token, in millionths
/// (`round::split(tokens, &[1])`).
fn insert_facts(conn: &Connection, source: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO fact (at, bucket, source, location, project, agent, model, session, turn,
                           input, output, cache_read, cache_write)
         SELECT s.at, (s.at - (((s.at % ?2) + ?2) % ?2)) / ?2, s.id, s.location, p.id,
                se.agent, s.model, s.session, s.turn,
                s.input * ?3, s.output * ?3, s.cache_read * ?3, s.cache_write * ?3
         FROM usage_source s
         JOIN session se ON se.id = s.session
         LEFT JOIN project p ON p.location = s.location
         WHERE s.id = ?1",
    )?
    .execute(params![source, BUCKET_MS, MICRO as i64])?;
    Ok(())
}

/// Rebuilds `usage_value` and `fact` from the sources. Its own transaction.
pub(crate) fn rebuild(conn: &mut Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch("DELETE FROM fact; DELETE FROM usage_value;")?;
    let keys: Vec<Vec<u8>> = tx
        .prepare("SELECT DISTINCT key FROM usage_source")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for key in &keys {
        reselect(&tx, key)?;
    }
    tx.commit()
}
