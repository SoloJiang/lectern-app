//! PTY and libghostty-vt terminal state on a dedicated thread
//! (docs/design/terminal.md).
//!
//! The plugin "terminal" provides `dyn Terminals` (`lectern_services::terminal`).
//! Each terminal has a thread that owns the libghostty-vt engine and the
//! PTY's primary end, and a thread that waits for the process. Launch,
//! environment and the checks that make a resume safe are in `launch`.

mod diag;
mod engine;
mod grid;
mod keys;
mod launch;
mod pacing;
mod palette;
mod plugin;
mod pty;
mod service;
mod slot;
mod table;
mod thread;

#[cfg(feature = "test-support")]
pub mod testing;

#[cfg(any(test, feature = "test-support"))]
pub use plugin::Provided;
pub use plugin::{PLUGIN_ID, TerminalDeps, TerminalPlugin};
