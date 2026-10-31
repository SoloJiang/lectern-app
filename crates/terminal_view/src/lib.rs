//! Terminal grid rendering for the conversation's Terminal page
//! (docs/design/terminal_view.md). `TerminalPane` draws the latest
//! `Arc<Frame>` of one terminal on a GPUI grid and turns keys, input method
//! text, the mouse, the wheel, focus, the clipboard and the size of its area
//! into `Input`s for `dyn Terminals`. The PTY, the terminal state machine and
//! the encoding of keys are `terminal`'s; the Terminal page, the status line
//! and the buttons are `conversation`'s.
//!
//! This crate is a library of the view layer, not a plugin: the host plugin
//! registers `key_bindings()` and passes its `ServiceRef` and token to
//! `TerminalPane::new`.
//!
//! Wheel convention: GPUI's positive scroll `y` shows earlier lines;
//! `ScrollInput::lines` is positive towards newer lines. The pane turns the
//! sign over.

mod actions;
mod boxes;
mod colors;
mod diag;
mod element;
mod geometry;
mod input;
mod layout;
mod paint;
mod pane;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use actions::{ClearScrollback, Copy, KEY_CONTEXT, Paste, SelectAll, key_bindings};
pub use colors::{theme, theme_of};
pub use geometry::{cells, grid_size};
pub use pane::TerminalPane;
