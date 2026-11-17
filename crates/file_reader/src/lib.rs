//! Full-file reading and editing (third tier): the "file-reader" plugin,
//! `dyn Reader` and the reading view (docs/design/file_reader.md「接口」
//! 「本 crate 的公开项」「界面」).
//!
//! [`Session`] is the window's reading state: the queue of open requests,
//! the current document, the back and forward history and the focus handed
//! to `code_intel`. [`ReadingView`] is the third-tier screen that draws it:
//! a self-drawn fixed-row-height list of display rows, the line number
//! column, the find bar and the history's buttons. Step 8 adds the edit
//! mode and Files (step 7b) the sidebar tree. This crate does no IO and no
//! parsing: documents are `syntax`'s (`Engine`), read through snapshots.

pub mod actions;
mod diag;
mod focus;
mod history;
mod nodes;
mod plugin;
mod render;
mod screen;
mod session;
mod shared;
mod text;
mod view;

#[cfg(test)]
mod tests;

pub use history::{HISTORY_MAX, History};
pub use plugin::{FileReaderDeps, FileReaderPlugin, PLUGIN_ID};
pub use screen::LAYOUT;
pub use session::{Host, Request, Session, Wake};
pub use view::ReadingView;
