//! The probe's node IDs (docs/design/project_home.md「界面」的语义节点表).
//!
//! Names are `[a-z0-9-]`; the business keys (session IDs, worktree and
//! sub-project directories) are written between brackets as `NodeId::item`
//! does. A key comes from files and paths, so it is made safe first: a
//! newline or a closing bracket followed by a dot would trip `NodeId`'s
//! format check in debug builds.

use lectern_ui::semantic::NodeId;

/// `project-home.<area>`.
pub(crate) fn area(name: &'static str) -> NodeId {
    NodeId::new("project-home", name, None)
}

/// A business key that `NodeId::item` accepts.
pub(crate) fn safe(key: &str) -> String {
    key.replace('\n', " ").replace("].", "]_")
}

pub(crate) fn item(parent: &NodeId, key: &str) -> NodeId {
    parent.item(&safe(key))
}

// ----------------------------------------------------------------- sidebar

pub(crate) fn sidebar() -> NodeId {
    area("sidebar")
}

pub(crate) fn sidebar_overview() -> NodeId {
    sidebar().child("overview")
}

pub(crate) fn sidebar_list() -> NodeId {
    sidebar().child("list")
}

pub(crate) fn sidebar_sessions() -> NodeId {
    sidebar().child("sessions")
}

/// A worktree's small heading; the key is the worktree directory, or
/// `other` / `folder` for a group without one.
pub(crate) fn sidebar_worktree(key: &str) -> NodeId {
    item(&sidebar_sessions().child("worktree"), key)
}

pub(crate) fn sidebar_session(id: &str) -> NodeId {
    item(&sidebar_sessions(), id)
}

pub(crate) fn sidebar_packages() -> NodeId {
    sidebar().child("packages")
}

pub(crate) fn sidebar_package(dir: &str) -> NodeId {
    item(&sidebar_packages(), dir)
}

/// A contributed entry of the `Project`, `Views` or `Bottom` section.
pub(crate) fn sidebar_entry(section: &'static str, id: &str) -> NodeId {
    item(&sidebar().child(section), id)
}

pub(crate) fn sidebar_usage() -> NodeId {
    sidebar().child("usage")
}

// -------------------------------------------------------------------- main

pub(crate) fn title() -> NodeId {
    area("title")
}

pub(crate) fn empty() -> NodeId {
    area("empty")
}

pub(crate) fn needs_review() -> NodeId {
    area("needs-review")
}

pub(crate) fn needs_review_card(id: &str) -> NodeId {
    item(&needs_review(), id)
}

pub(crate) fn outside() -> NodeId {
    area("outside")
}

pub(crate) fn done() -> NodeId {
    area("done")
}

pub(crate) fn done_card(id: &str) -> NodeId {
    item(&done(), id)
}
