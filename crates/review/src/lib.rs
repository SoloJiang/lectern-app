//! Change attribution, hunk alignment, reviewed state and review notes
//! (docs/design/review.md).
//!
//! Step 1 of the design: the noise list and rule grouping ([`noise`]), the
//! annotation log (`log`, ADR 0044) and the three seen states (`seen`).
//! Step 2: change chains, line provenance and attribution, the turn-history
//! digest and the `review.attribution` cache value (`chain`). Step 3: the
//! review engine (`engine`) with its claims (`claims`), path cutoffs
//! (`cutoff`), snapshots, commands, rows and the version LRU (`lru`),
//! driven without the plugin by `testing::Driven`. Step 4: the "review"
//! plugin (`plugin`), the watches that refresh open reviews and the
//! 30-second retention after the last `close`. Runtime logging goes through `lectern-logging`; `diag` holds
//! what a record keeps of this crate's values (docs/design/review.md
//! 「日志与诊断」).

mod brief;
mod chain;
mod claims;
mod cutoff;
mod diag;
mod engine;
mod group;
mod log;
mod lru;
pub mod noise;
mod plugin;
mod seen;

pub use plugin::{PLUGIN_ID, ReviewDeps, ReviewPlugin, plugin};

#[cfg(any(test, feature = "test-support"))]
pub mod testing;
