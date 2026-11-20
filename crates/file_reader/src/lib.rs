//! Full-file reading and editing (third tier): the "file-reader" plugin,
//! `dyn Reader` and the reading view (docs/design/file_reader.md「接口」
//! 「本 crate 的公开项」「界面」).
//!
//! [`Session`] is the window's reading state: the queue of open requests,
//! the current document, the back and forward history and the focus handed
//! to `code_intel`. [`ReadingView`] is the third-tier screen that draws it:
//! a self-drawn fixed-row-height list of display rows, the line number
//! column, the find bar and the history's buttons. [`FilesView`] is the
//! sidebar panel that lists the active worktree's tracked files from the
//! directory table. The edit mode (step 8a: the code editor over `syntax`'s
//! edit session, saving, the disk banner) is in `ReadingView` too, behind
//! `edit::EDIT_REACHABLE` until the leave guard exists (step 8b). This crate
//! does no IO and no parsing: documents are `syntax`'s (`Engine`), read
//! through snapshots.

pub mod actions;
mod diag;
mod edit;
mod files;
mod focus;
mod history;
mod nodes;
mod plugin;
mod render;
mod render_edit;
mod screen;
mod session;
mod shared;
mod text;
mod view;

#[cfg(test)]
mod tests;

pub use files::FilesView;
pub use history::{HISTORY_MAX, History};
pub use plugin::{FileReaderDeps, FileReaderPlugin, PLUGIN_ID};
pub use screen::LAYOUT;
pub use session::{Host, Request, Session, Wake};
pub use view::ReadingView;
