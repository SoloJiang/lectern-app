//! Change attribution, hunk alignment, reviewed state and review notes
//! (docs/design/review.md).
//!
//! Step 1 of the design: the noise list and rule grouping ([`noise`]), the
//! annotation log (`log`, ADR 0044) and the three seen states (`seen`).
//! Step 2: change chains, line provenance and attribution, the turn-history
//! digest and the `review.attribution` cache value (`chain`). The review
//! engine that uses them comes with step 3.

#[allow(dead_code, reason = "the review engine (step 3) builds chains")]
mod chain;
#[allow(dead_code, reason = "the review engine (step 3) arranges groups")]
mod group;
#[allow(dead_code, reason = "the review engine (step 3) uses the log")]
mod log;
pub mod noise;
#[allow(dead_code, reason = "the review engine (step 3) uses seen states")]
mod seen;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;
