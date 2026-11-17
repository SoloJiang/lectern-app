//! Full-file reading and editing (third tier): the "file-reader" plugin and
//! `dyn Reader` (docs/design/file_reader.md「接口」「本 crate 的公开项」).
//!
//! This part of the crate has no view yet (steps 7 and 8 of the design add
//! the reading view, Files and the edit mode, and with them GPUI). What is
//! here is the plugin, the service and [`Session`], the window's reading
//! state that the view will render: the queue of open requests, the current
//! document, the back and forward history and the focus handed to
//! `code_intel`. It does no IO and no parsing: documents are `syntax`'s
//! (`Engine`), read through snapshots.

mod diag;
mod focus;
mod history;
mod plugin;
mod session;

#[cfg(test)]
mod tests;

pub use history::{HISTORY_MAX, History};
pub use plugin::{FileReaderDeps, FileReaderPlugin};
pub use session::{Host, Request, Session, Wake};
