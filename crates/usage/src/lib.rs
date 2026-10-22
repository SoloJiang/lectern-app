//! Token usage: dedupe and normalization of the records `agents` hands over,
//! this crate's own SQLite database, attribution to projects and windows of
//! local days, and the `dyn Usage` service (docs/design/usage.md). Tokens
//! only, no amounts of money: ADR 0022.
//!
//! Implemented so far: step 1 of the design's 「验收」 up to the hot / cold
//! split (the database, reading and cursors, dedupe, per-cwd places and
//! project totals, windows, rounding, the `Project` and `AllProjects`
//! queries, the plugin and its reading cadence).

mod engine;
mod key;
mod plugin;
mod snapshot;
mod store;
mod time;

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
