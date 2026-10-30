//! PTY and libghostty-vt terminal state on a dedicated thread
//! (docs/design/terminal.md).
//!
//! Today: the libghostty-vt engine and its viewport frames (「验收」 step 2).
//! The PTY, the dedicated threads and the plugin come with step 3.

// Used by `testing` until the terminal thread (step 3) uses it.
#[cfg_attr(
    not(feature = "test-support"),
    allow(dead_code, reason = "the terminal thread arrives in step 3")
)]
mod engine;
mod grid;
mod keys;
mod pacing;
mod palette;

#[cfg(feature = "test-support")]
pub mod testing;
