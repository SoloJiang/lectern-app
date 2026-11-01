//! Finishing turns (docs/design/usage.md「轮次的观察与改动项」第 5 步): which
//! turns are ready, and writing a batch of them in one transaction.
//!
//! A turn is ready when its end was read (`turn.closed`) and the clock has
//! passed the later of its last result and its last moment by 2 seconds, so
//! that the observations the next step takes from the repository
//! (`call_obs`, `turn_obs`) have been read by then. A session is finished
//! only after its first read to the end (`session.reached_end`) and while
//! no replay runs.

use std::path::PathBuf;

use lectern_services::session::{SessionKey, SourceId};
use rusqlite::{OptionalExtension, params};

use super::calls::{self, PathPlace};
use super::dedup::Deltas;
use super::ingest::{Ingested, intern, location, path_bytes};
use super::{Store, StoreError, attrib, cold};

/// Milliseconds a turn waits after its last moment (docs/design/usage.md
/// 「轮次的观察与改动项」第 5 步).
pub(crate) const SETTLE_WAIT_MS: i64 = 2_000;

/// A turn that can be settled now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReadyTurn {
    pub(crate) id: i64,
    /// The `TurnId` text.
    pub(crate) turn: String,
    /// It has `Edit` calls (`turn.edits`): its changed files are in the
    /// session's turn index.
    pub(crate) edits: bool,
}

/// The turns of a session waiting to be settled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pending {
    /// Ready now, in order, at most the limit asked for.
    pub(crate) ready: Vec<ReadyTurn>,
    /// More turns are ready than `ready` holds.
    pub(crate) more: bool,
    /// The earliest moment a turn that is not ready yet will be.
    pub(crate) wait_until: Option<i64>,
}

/// A changed file of a turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ItemWork {
    /// The key of the call that changed it (`calls::hash`).
    pub(crate) call: i64,
    pub(crate) path: PathBuf,
    pub(crate) lines: u64,
    /// Where it lies, used when the call's own record does not say.
    pub(crate) place: PathPlace,
}

/// A turn to settle with its changed files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnWork {
    pub(crate) id: i64,
    pub(crate) items: Vec<ItemWork>,
}

impl Store {
    /// The sessions read to their end that have turns not settled: what a
    /// restart finds left over (a turn that has not ended is found too, and
    /// found not ready; the last turn of a session ends when the session is
    /// looked at, [`Store::pending`]).
    pub(crate) fn unsettled(&self) -> Result<Vec<SessionKey>, StoreError> {
        self.read(|conn| {
            let rows: Vec<(Vec<u8>, Vec<u8>)> = conn
                .prepare_cached(
                    "SELECT a.text, b.text FROM session s
                     JOIN intern a ON a.id = s.source JOIN intern b ON b.id = s.sid
                     WHERE s.reached_end = 1 AND s.replay IS NULL
                       AND EXISTS (SELECT 1 FROM turn t
                                   WHERE t.session = s.id AND t.gen = s.gen
                                     AND t.done = 0)",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows
                .into_iter()
                .map(|(source, id)| SessionKey {
                    source: SourceId(String::from_utf8_lossy(&source).as_ref().into()),
                    id: String::from_utf8_lossy(&id).as_ref().into(),
                })
                .collect())
        })
    }

    /// The turns of a session to settle. `end_open` says the session's
    /// last turn is over although no later turn began (Claude Code's last
    /// turn when the session has no open turn). None while the session has
    /// not been read to its end or is being replayed.
    pub(crate) fn pending(
        &self,
        key: &SessionKey,
        now_ms: i64,
        end_open: bool,
        limit: usize,
    ) -> Result<Option<Pending>, StoreError> {
        self.write(|conn| {
            let session: Option<(i64, i64, bool, Option<i64>)> = conn
                .prepare_cached(
                    "SELECT s.id, s.gen, s.reached_end, s.replay FROM session s
                     JOIN intern a ON a.id = s.source JOIN intern b ON b.id = s.sid
                     WHERE a.text = ?1 AND b.text = ?2",
                )?
                .query_row(params![key.source.0.as_bytes(), key.id.as_bytes()], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?;
            let Some((session, generation, reached, replay)) = session else {
                return Ok(None);
            };
            if !reached || replay.is_some() {
                return Ok(None);
            }
            if end_open {
                conn.prepare_cached(
                    "UPDATE turn SET closed = 1
                     WHERE session = ?1 AND gen = ?2 AND closed = 0
                       AND seq = (SELECT max(seq) FROM turn WHERE session = ?1 AND gen = ?2)",
                )?
                .execute(params![session, generation])?;
            }
            let frozen = cold::frozen(conn)?;
            let sql = format!(
                "SELECT t.id, i.text, coalesce(t.last_at, t.started_at), t.edits FROM turn t
                 JOIN intern i ON i.id = t.tid
                 WHERE t.session = ?1 AND t.gen = ?2 AND t.done = 0 AND t.closed = 1
                   AND NOT {}
                 ORDER BY t.seq",
                frozen.sql("t.started_at")
            );
            let mut pending = Pending::default();
            let rows: Vec<(i64, Vec<u8>, i64, bool)> = conn
                .prepare_cached(&sql)?
                .query_map(params![session, generation], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            for (id, turn, last, edits) in rows {
                let ready_at = last.saturating_add(SETTLE_WAIT_MS);
                if ready_at > now_ms {
                    pending.wait_until =
                        Some(pending.wait_until.map_or(ready_at, |w| w.min(ready_at)));
                    continue;
                }
                if pending.ready.len() >= limit {
                    pending.more = true;
                    break;
                }
                pending.ready.push(ReadyTurn {
                    id,
                    turn: String::from_utf8_lossy(&turn).into_owned(),
                    edits,
                });
            }
            Ok(Some(pending))
        })
    }

    /// Settles turns: their changed files are written, then their shares
    /// and the fact rows that follow, and so are the turns that depend on
    /// them (docs/design/usage.md「分摊」), in one transaction. The facts
    /// changed are in the result, like a batch's.
    pub(crate) fn finalize(
        &self,
        key: &SessionKey,
        turns: &[TurnWork],
    ) -> Result<Ingested, StoreError> {
        self.transaction(|tx| {
            let session: i64 = tx
                .prepare_cached(
                    "SELECT s.id FROM session s
                     JOIN intern a ON a.id = s.source JOIN intern b ON b.id = s.sid
                     WHERE a.text = ?1 AND b.text = ?2",
                )?
                .query_row(params![key.source.0.as_bytes(), key.id.as_bytes()], |r| {
                    r.get(0)
                })?;
            let frozen = cold::frozen(tx)?;
            let mut work = Vec::new();
            for turn in turns {
                let started: Option<(i64, i64)> = tx
                    .prepare_cached(
                        "SELECT started_at, gen FROM turn WHERE id = ?1 AND session = ?2",
                    )?
                    .query_row(params![turn.id, session], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                let Some((at, generation)) = started else {
                    continue;
                };
                tx.prepare_cached("DELETE FROM item WHERE turn = ?1")?
                    .execute(params![turn.id])?;
                for item in &turn.items {
                    let cid = item.call;
                    let path = intern(tx, path_bytes(&item.path))?;
                    let known: Option<i64> = tx
                        .prepare_cached(
                            "SELECT location FROM call_fact
                             WHERE session = ?1 AND cid = ?2 AND path = ?3",
                        )?
                        .query_row(params![session, cid, path], |r| r.get(0))
                        .optional()?;
                    let place = match known {
                        Some(place) => place,
                        None => {
                            let place = location(tx, &item.place.place)?;
                            let worktree = match &item.place.worktree {
                                Some(w) => Some(intern(tx, path_bytes(w))?),
                                None => None,
                            };
                            calls::fact(tx, session, cid, path, (place, worktree), at)?;
                            place
                        }
                    };
                    tx.prepare_cached(
                        "INSERT INTO item (session, gen, turn, path, location, lines, at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    )?
                    .execute(params![
                        session,
                        generation,
                        turn.id,
                        path,
                        place,
                        i64::try_from(item.lines).unwrap_or(i64::MAX),
                        at
                    ])?;
                }
                work.push(turn.id);
            }
            let mut deltas = Deltas::collect();
            let settled = attrib::settle_all(tx, work, &frozen, &mut deltas)?;
            let commit: i64 = tx
                .prepare_cached(
                    "INSERT INTO meta (name, value) VALUES ('commits', 1)
                     ON CONFLICT (name) DO UPDATE SET value = value + 1
                     RETURNING value",
                )?
                .query_row([], |r| r.get(0))?;
            Ok(Ingested {
                records: settled,
                facts: std::mem::take(&mut deltas.facts),
                facts_unknown: deltas.unknown,
                commit,
                ..Ingested::default()
            })
        })
    }
}
