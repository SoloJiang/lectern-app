//! The limit samples (docs/design/usage.md「额度快照」): the newest sample of
//! each (agent, source, limit ID, kind) in `limit_sample`. Codex samples
//! come from `RateLimits` records inside the ingest transaction; Claude
//! Code samples come from the statusline file (`crate::limits`).

use std::sync::Arc;

use lectern_services::session::{Agent, Timestamp};
use lectern_services::usage::{LimitKind, LimitSource};
use rusqlite::{Connection, params};

use super::ingest::{agent_label, agent_of};
use super::{Store, StoreError};

/// One stored sample.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Sample {
    pub(crate) agent: Agent,
    pub(crate) source: LimitSource,
    /// None is stored as the empty text.
    pub(crate) limit_id: Option<Arc<str>>,
    pub(crate) kind: LimitKind,
    pub(crate) used_percent: f32,
    pub(crate) window_minutes: Option<u32>,
    pub(crate) resets_at: Option<Timestamp>,
    pub(crate) sampled_at: Timestamp,
}

fn source_code(source: LimitSource) -> i64 {
    match source {
        LimitSource::SessionLog => 0,
        LimitSource::Statusline => 1,
    }
}

fn source_of(code: i64) -> LimitSource {
    if code == 1 {
        LimitSource::Statusline
    } else {
        LimitSource::SessionLog
    }
}

fn kind_code(kind: LimitKind) -> i64 {
    match kind {
        LimitKind::Primary => 0,
        LimitKind::Secondary => 1,
        LimitKind::FiveHour => 2,
        LimitKind::SevenDay => 3,
    }
}

fn kind_of(code: i64) -> Option<LimitKind> {
    Some(match code {
        0 => LimitKind::Primary,
        1 => LimitKind::Secondary,
        2 => LimitKind::FiveHour,
        3 => LimitKind::SevenDay,
        _ => return None,
    })
}

/// A window reset further away than this many window lengths after the
/// sample is not believable (a damaged file, a clock far off): the sample
/// is kept without its `resets_at`.
const MAX_RESET_WINDOWS: i64 = 2;

/// `sample` as it is stored: `sampled_at` is not later than `now_ms` (a
/// clock set ahead would otherwise make the sample newer than every later
/// one and block them), and a `resets_at` more than twice the window
/// length past `sampled_at` is dropped. A sample without a window length
/// keeps its `resets_at`.
pub(crate) fn sanitized(sample: &Sample, now_ms: i64) -> Sample {
    let mut out = sample.clone();
    out.sampled_at = Timestamp(sample.sampled_at.0.min(now_ms));
    if let (Some(minutes), Some(resets_at)) = (out.window_minutes, out.resets_at) {
        let window_ms = i64::from(minutes).saturating_mul(60_000);
        let limit = out
            .sampled_at
            .0
            .saturating_add(window_ms.saturating_mul(MAX_RESET_WINDOWS));
        if resets_at.0 > limit {
            out.resets_at = None;
        }
    }
    out
}

/// Writes `sample` (as [`sanitized`] at `now_ms`) unless the stored one for
/// its key is newer or equal in every column. Returns whether the table
/// changed.
pub(crate) fn put(conn: &Connection, sample: &Sample, now_ms: i64) -> rusqlite::Result<bool> {
    let sample = &sanitized(sample, now_ms);
    let changed = conn
        .prepare_cached(
            "INSERT INTO limit_sample
               (agent, source, limit_id, kind, used_percent, window_minutes, resets_at, sampled_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (agent, source, limit_id, kind) DO UPDATE SET
               used_percent = excluded.used_percent,
               window_minutes = excluded.window_minutes,
               resets_at = excluded.resets_at,
               sampled_at = excluded.sampled_at
             WHERE excluded.sampled_at >= limit_sample.sampled_at
               AND (limit_sample.used_percent IS NOT excluded.used_percent
                 OR limit_sample.window_minutes IS NOT excluded.window_minutes
                 OR limit_sample.resets_at IS NOT excluded.resets_at
                 OR limit_sample.sampled_at IS NOT excluded.sampled_at)",
        )?
        .execute(params![
            agent_label(&sample.agent),
            source_code(sample.source),
            sample.limit_id.as_deref().unwrap_or(""),
            kind_code(sample.kind),
            f64::from(sample.used_percent),
            sample.window_minutes,
            sample.resets_at.map(|t| t.0),
            sample.sampled_at.0,
        ])?;
    Ok(changed > 0)
}

impl Store {
    /// Writes samples in one transaction (as [`sanitized`] at `now_ms`).
    /// Returns whether any changed.
    pub(crate) fn put_samples(&self, samples: &[Sample], now_ms: i64) -> Result<bool, StoreError> {
        self.transaction(|tx| {
            let mut changed = false;
            for sample in samples {
                changed |= put(tx, sample, now_ms)?;
            }
            Ok(changed)
        })
    }

    /// Every stored sample, in a fixed order (agent, source, limit ID,
    /// kind). A row with an unknown kind (written by a newer version) is
    /// skipped.
    pub(crate) fn samples(&self) -> Result<Vec<Sample>, StoreError> {
        self.read(|conn| {
            let mut statement = conn.prepare_cached(
                "SELECT agent, source, limit_id, kind, used_percent, window_minutes, resets_at,
                        sampled_at
                 FROM limit_sample ORDER BY agent, source, limit_id, kind",
            )?;
            let rows = statement.query_map([], |r| {
                let agent: String = r.get(0)?;
                let limit_id: String = r.get(2)?;
                let window_minutes: Option<i64> = r.get(5)?;
                let resets_at: Option<i64> = r.get(6)?;
                let kind: i64 = r.get(3)?;
                Ok(kind_of(kind).map(|kind| Sample {
                    agent: agent_of(agent.as_bytes()),
                    source: source_of(r.get(1).unwrap_or(0)),
                    limit_id: (!limit_id.is_empty()).then(|| Arc::from(limit_id.as_str())),
                    kind,
                    used_percent: r.get::<_, f64>(4).unwrap_or(0.0) as f32,
                    window_minutes: window_minutes.and_then(|m| u32::try_from(m).ok()),
                    resets_at: resets_at.map(Timestamp),
                    sampled_at: Timestamp(r.get(7).unwrap_or(0)),
                }))
            })?;
            let mut out = Vec::new();
            for row in rows {
                if let Some(sample) = row? {
                    out.push(sample);
                }
            }
            Ok(out)
        })
    }
}
