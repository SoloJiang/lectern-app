//! Change attribution, hunk alignment, reviewed state and review notes
//! (docs/design/review.md).
//!
//! Step 1 of the design: the noise list and rule grouping ([`noise`]), the
//! annotation log (`log`, ADR 0044) and the three seen states (`seen`). The
//! review engine that uses the last two comes with step 3.

#[allow(dead_code, reason = "the review engine (step 3) arranges groups")]
mod group;
#[allow(dead_code, reason = "the review engine (step 3) uses the log")]
mod log;
pub mod noise;
#[allow(dead_code, reason = "the review engine (step 3) uses seen states")]
mod seen;
pub mod types;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;
