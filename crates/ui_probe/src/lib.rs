//! UI probe for debug and test builds only (docs/design/ui_probe.md): the
//! semantic tree of a window as JSON (`ui_tree`), the window as a PNG with
//! the nodes' borders and numbers (`screenshot`), over a Unix socket in a
//! private directory, with the command line client `lectern-probe`.
//!
//! The same functions serve the running application and `#[gpui::test]`:
//! `tree::snapshot` records one frame without the frame loop.

pub mod annotate;
pub mod client;
pub mod protocol;
pub mod server;
pub mod tree;

#[cfg(test)]
mod tests;
