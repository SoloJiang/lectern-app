//! The reads behind the `Session` query (docs/design/usage.md「接口」
//! `SessionUsage`): a session's tokens with its sub-agents', and the tokens
//! of each of its own turns.

use lectern_services::session::SessionKey;
use rusqlite::{OptionalExtension, params};

use super::cold::TOTAL_SESSION;
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
                Ok(Some(out))
            })
        })
    }
}
