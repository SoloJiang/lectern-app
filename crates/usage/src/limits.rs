//! Limit snapshots (docs/design/usage.md「额度快照」, 「外部格式」): reading
//! the statusline file Claude Code's statusline script saves, and building
//! the `Limits` snapshot from the stored samples.
//!
//! The statusline input is the JSON of Claude Code's public statusline
//! documentation (`tests/fixtures/statusline/SOURCE.md`): the top-level
//! `rate_limits` object has the windows `five_hour` and `seven_day`; each
//! has `used_percentage` (0 to 100) and `resets_at` (Unix epoch seconds). A
//! window may be absent on its own, and Claude Code drops a window after its
//! `resets_at` has passed. Every other field is ignored. The file's
//! modification time is the sampling time.
//!
//! What a file says is never logged, and neither is the parser's own error
//! text (it can quote values): a rejection is one of the typed reasons of
//! [`Rejected`].

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use lectern_services::session::{Agent, Timestamp};
use lectern_services::usage::{LimitKind, LimitSource, LimitWindow, Limits};

use crate::store::limits::Sample;

/// The file the statusline script writes, in `<data_dir>/statusline/`
/// (prerequisite C).
pub(crate) const STATUSLINE_FILE: &str = "claude-code.json";

/// The largest file read; a larger one is ignored.
pub(crate) const MAX_BYTES: u64 = 1 << 20;

/// Minutes of the two statusline windows, from their names.
const FIVE_HOURS: u32 = 5 * 60;
const SEVEN_DAYS: u32 = 7 * 24 * 60;

/// Why the statusline file did not update the samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rejected {
    /// There is no file (the script is not installed, or has not run).
    Missing,
    /// The file could not be read.
    Unreadable,
    /// More than [`MAX_BYTES`].
    TooLarge,
    /// Not JSON, or not an object.
    NotJson,
    /// Neither window is there with a usable percentage.
    NoWindows,
}

impl Rejected {
    /// A fixed name for the log.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Rejected::Missing => "missing",
            Rejected::Unreadable => "unreadable",
            Rejected::TooLarge => "too_large",
            Rejected::NotJson => "not_json",
            Rejected::NoWindows => "no_windows",
        }
    }
}

/// Reads the file at `path`. `fallback` is the sampling time when the file
/// has no modification time.
pub(crate) fn read_statusline(path: &Path, fallback: Timestamp) -> Result<Vec<Sample>, Rejected> {
    let file = std::fs::File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Rejected::Missing
        } else {
            Rejected::Unreadable
        }
    })?;
    let meta = file.metadata().map_err(|_| Rejected::Unreadable)?;
    if meta.len() > MAX_BYTES {
        return Err(Rejected::TooLarge);
    }
    let sampled_at = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .map_or(fallback, Timestamp);
    // The size may have grown since `metadata`: read one byte more than the
    // limit and refuse what does not fit.
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Rejected::Unreadable)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Rejected::TooLarge);
    }
    parse_statusline(&bytes, sampled_at)
}

/// The samples of a statusline JSON. A window with a percentage that is
/// not a number from 0 to 100 is left out; no usable window at all is
/// [`Rejected::NoWindows`].
pub(crate) fn parse_statusline(
    bytes: &[u8],
    sampled_at: Timestamp,
) -> Result<Vec<Sample>, Rejected> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| Rejected::NotJson)?;
    let object = value.as_object().ok_or(Rejected::NotJson)?;
    let mut samples = Vec::new();
    for (name, kind, minutes) in [
        ("five_hour", LimitKind::FiveHour, FIVE_HOURS),
        ("seven_day", LimitKind::SevenDay, SEVEN_DAYS),
    ] {
        let Some(window) = object.get("rate_limits").and_then(|r| r.get(name)) else {
            continue;
        };
        let Some(percent) = window.get("used_percentage").and_then(|p| p.as_f64()) else {
            continue;
        };
        if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
            continue;
        }
        let resets_at = window
            .get("resets_at")
            .and_then(|t| t.as_f64())
            .filter(|t| t.is_finite() && *t > 0.0 && *t < 1e15)
            .map(|secs| Timestamp((secs * 1000.0) as i64));
        samples.push(Sample {
            agent: Agent::ClaudeCode,
            source: LimitSource::Statusline,
            limit_id: None,
            kind,
            used_percent: percent as f32,
            window_minutes: Some(minutes),
            resets_at,
            sampled_at,
        });
    }
    if samples.is_empty() {
        return Err(Rejected::NoWindows);
    }
    Ok(samples)
}

/// The snapshot of `samples` (in the store's order) at `now_ms`: a window
/// is `expired` once its `resets_at` has passed, and `tightest` is the
/// index of the window with the highest percentage among those that have
/// not (the first of equals).
pub(crate) fn build(samples: &[Sample], now_ms: i64, generation: u64) -> Limits {
    let windows: Vec<LimitWindow> = samples
        .iter()
        .map(|s| LimitWindow {
            agent: s.agent.clone(),
            source: s.source,
            limit_id: s.limit_id.clone(),
            kind: s.kind,
            window_minutes: s.window_minutes,
            used_percent: s.used_percent,
            resets_at: s.resets_at,
            sampled_at: s.sampled_at,
            expired: s.resets_at.is_some_and(|t| t.0 <= now_ms),
        })
        .collect();
    let mut tightest: Option<usize> = None;
    for (i, w) in windows.iter().enumerate() {
        if w.expired {
            continue;
        }
        if tightest.is_none_or(|t| w.used_percent.total_cmp(&windows[t].used_percent).is_gt()) {
            tightest = Some(i);
        }
    }
    Limits {
        generation,
        windows: Arc::from(windows),
        tightest: tightest.and_then(|i| u16::try_from(i).ok()),
    }
}

/// The earliest `resets_at` after `now_ms`: when the snapshot changes
/// next without a new sample.
pub(crate) fn next_reset(samples: &[Sample], now_ms: i64) -> Option<i64> {
    samples
        .iter()
        .filter_map(|s| s.resets_at.map(|t| t.0))
        .filter(|t| *t > now_ms)
        .min()
}
