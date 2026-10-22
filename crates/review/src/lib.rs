//! Change attribution, hunk alignment, reviewed state and review notes
//! (docs/design/review.md).
//!
//! Step 1 of the design: the noise list and rule grouping ([`noise`]), the
//! annotation log (`log`, ADR 0044) and the three seen states (`seen`).
//! Step 2: change chains, line provenance and attribution, the turn-history
//! digest and the `review.attribution` cache value (`chain`). Step 3: the
//! review engine (`engine`) with its claims (`claims`), path cutoffs
//! (`cutoff`), snapshots, commands, rows and the version LRU (`lru`),
//! driven without the plugin by `testing::Driven`. The plugin comes with
//! step 4.

// Without `test-support` nothing calls the engine until the plugin (step 4).
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
mod chain;
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
mod claims;
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
mod cutoff;
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
mod engine;
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
mod group;
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
mod log;
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
mod lru;
pub mod noise;
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
mod seen;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;
