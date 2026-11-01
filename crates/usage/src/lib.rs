//! Token usage: dedupe and normalization of the records `agents` hands over,
//! this crate's own SQLite database, attribution to projects and windows of
//! local days, and the `dyn Usage` service (docs/design/usage.md). Tokens
//! only, no amounts of money: ADR 0022.
//!
//! Implemented so far: steps 1 to 4 of the design's 「验收」. Step 1: the
//! database, reading and cursors, dedupe, per-cwd places and project
//! totals, windows, rounding, the hot / cold split and the `History`
//! query, the `Project` and `AllProjects` queries kept up to date by fact
//! deltas, the plugin with its reading and archive cadence,
//! `testing::Scenario`. Step 2: limit snapshots. Step 3: project identity
//! (`identity`, aliases, the Usage page's commands, folder claims, 「其他」).
//! Step 4: tool calls, changed files and shares (a turn's tokens go where
//! its files changed), settling turns, per-turn tokens of a session.

mod engine;
mod facts;
mod finalize;
mod key;
mod limits;
mod paths;
mod plugin;
mod snapshot;
mod store;
mod time;

pub mod identity;
pub mod round;
pub mod tokens;
pub mod window;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use plugin::{PLUGIN_ID, UsageDeps, UsagePlugin, plugin};

/// The local time zone. `app` implements it with `platform::time::utc_offset`
/// (usage design prerequisite B); tests use `testing::TableZone`.
pub trait LocalZone: Send + Sync {
    /// The offset from UTC (seconds, east positive) at `unix_ms`. The first
    /// call may read the zone file: only on background threads.
    fn offset_at(&self, unix_ms: i64) -> i32;
}
