//! Syntax highlighting and open documents for full-file reading, the
//! review's diffs and editing (docs/design/file_reader.md「`syntax` 的公开项」,
//! docs/adr/0041.md): the language table and its highlight queries,
//! highlight tokens, the first screen's window, the sliced whole-file parse,
//! the window chain, the stuck-grammar check, text with its line index and
//! line checkpoints, the highlight and text LRU, and the open documents.
//!
//! A domain library, not a plugin: no GPUI. Its IO goes through `platform`
//! (`SafeRoot`), `git` (objects) and `cache` (`syntax.lines`), always on
//! executor threads; nothing here blocks the UI thread.

mod cells;
mod change;
mod cut;
mod doc;
pub mod find;
mod git_access;
pub mod health;
pub mod highlight;
mod lang;
pub mod limits;
mod lru;
mod objects;
mod spans;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod text;
mod util;
pub mod window;

pub use change::TextEdit;
pub use doc::{
    DocHandle, DocSnapshot, DocState, EditSession, EditState, Editable, Engine, EngineDeps,
    Highlighting, LineCount, LineNo, MergeOutcome, NotEditable, ReadError, Row, RowPos, SaveResult,
    Tier, Viewport,
};
#[cfg(any(test, feature = "test-support"))]
pub use doc::{StepEnd, Trace};
pub use find::{FindQuery, FindResults};
pub use lang::{Lang, Languages, Rules, SPANS_VERSION};
pub use spans::{Token, TokenKind, WindowSpans};

#[cfg(test)]
mod tests;
