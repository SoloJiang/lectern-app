//! The Files tree (docs/design/file_reader.md「界面」的 Files): the sidebar
//! panel that replaces the project sidebar and lists the active worktree's
//! tracked files from its directory table.
//!
//! - `tree.rs`: the visible rows of a `DirTable` under an expansion; pure.
//! - `view.rs`: the panel's state: the snapshot, the selection, the keys,
//!   opening a file through `Reader::open`.
//! - `render.rs`: the header, the `uniform_list` of rows and the states
//!   without rows.
//!
//! The panel is built by `workspace` each time the sidebar shows it and
//! dropped when the sidebar is hidden; `Shared` keeps what must outlive a
//! view (`shared::FilesMemory`).

mod render;
mod tree;
mod view;

use std::sync::Arc;

use gpui::{AnyView, App, AppContext, Window};

use crate::shared::Shared;

pub use view::FilesView;

/// The ID of the sidebar panel, and of the Files entry in the project
/// sidebar.
pub const PANEL_ID: &str = "files";

#[cfg(test)]
pub(crate) use tree::{Node, Tree};
#[cfg(test)]
pub(crate) use view::Content;

/// The factory of `SidebarPanel("files")` (main thread): the view takes
/// the focus, so the arrows and ↵ reach it.
pub(crate) fn build(shared: &Arc<Shared>, window: &mut Window, cx: &mut App) -> AnyView {
    let view = cx.new(|cx| FilesView::new(shared.clone(), cx));
    let focus = view.read(cx).focus.clone();
    window.focus(&focus, cx);
    view.into()
}
