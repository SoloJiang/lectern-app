//! Syntax highlighting for full-file reading, the review's diffs and editing
//! (docs/design/file_reader.md「`syntax` 的公开项」, docs/adr/0041.md): the
//! language table and its highlight queries, highlight tokens, whole-file
//! highlighting and the stuck-grammar check.
//!
//! A domain library, not a plugin: no GPUI, no IO. Parsing runs on the
//! caller's background thread; nothing here blocks the UI thread.

pub mod health;
pub mod highlight;
mod lang;
pub mod limits;
mod spans;
mod util;

pub use lang::{Lang, Languages, Rules, SPANS_VERSION};
pub use spans::{Token, TokenKind, WindowSpans};

#[cfg(test)]
mod tests;
