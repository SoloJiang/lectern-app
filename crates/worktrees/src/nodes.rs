//! The probe's node IDs (docs/design/worktrees.md「界面」的语义节点表): the
//! ones the snapshot carries (the row of a worktree, a session row and an
//! overlap row of the inspector) and the ones the views build.
//!
//! Names are `[a-z0-9-]`; the business keys (a worktree key is a git
//! directory, a session ID comes from a file) are written between brackets as
//! `NodeId::item` does. A key comes from files and paths, so it is made safe
//! first: a newline or a closing bracket followed by a dot would trip
//! `NodeId`'s format check in debug builds.

use std::path::Path;

use lectern_ui::semantic::NodeId;

/// A business key that `NodeId::item` accepts.
pub(crate) fn safe(key: &str) -> String {
    key.replace('\n', " ").replace("].", "]_")
}

/// A row of the list: `worktrees.list.item[<WorktreeKey>]`. The design's table
/// writes `worktrees.item[<WorktreeKey>]`, which `NodeId`'s constructors cannot
/// build (an item always follows `view.area`); the row is an item of the list
/// (docs/design/worktrees.md「与设计的差异」).
pub(crate) fn row(worktree: &Path) -> NodeId {
    NodeId::new("worktrees", "list", None).item(&safe(&worktree.to_string_lossy()))
}

/// `worktrees.<area>`.
pub(crate) fn area(name: &'static str) -> NodeId {
    NodeId::new("worktrees", name, None)
}

/// The list of both groups: `worktrees.list`. A row is an item of it.
pub(crate) fn list() -> NodeId {
    area("list")
}

pub(crate) fn active() -> NodeId {
    area("active")
}

pub(crate) fn idle() -> NodeId {
    area("idle")
}

/// The refresh button of the toolbar.
pub(crate) fn refresh() -> NodeId {
    area("toolbar").child("refresh")
}

/// What the probe reads off a row (`Row` has no attributes of its own).
pub(crate) fn facts(row: &NodeId) -> NodeId {
    row.child("facts")
}

/// The tokens at the end of a row: `<row>.tokens`. The facts node holds the
/// other eight attributes, which is all a node may hold.
pub(crate) fn tokens(row: &NodeId) -> NodeId {
    row.child("tokens")
}

/// A mark after the branch label: `<row>.badge.item[current]`. The design's
/// `badge[current]` cannot be built either (docs/design/worktrees.md「与设计的差异」).
pub(crate) fn badge(row: &NodeId, which: &str) -> NodeId {
    row.child("badge").item(which)
}

pub(crate) fn detail() -> NodeId {
    area("detail")
}

pub(crate) fn detail_usage() -> NodeId {
    detail().child("usage")
}

/// `worktrees.detail.usage.mr.item[<MR number>]`.
pub(crate) fn detail_mr(number: u64) -> NodeId {
    detail_usage().child("mr").item(&number.to_string())
}

pub(crate) fn detail_compare() -> NodeId {
    detail().child("compare")
}

pub(crate) fn detail_open() -> NodeId {
    detail_compare().child("open")
}

pub(crate) fn detail_overlaps() -> NodeId {
    detail().child("overlaps")
}

pub(crate) fn detail_sessions() -> NodeId {
    detail().child("sessions")
}

pub(crate) fn detail_switch() -> NodeId {
    detail().child("switch")
}

/// `worktrees.detail.sessions.item[<session ID>]`.
pub(crate) fn session(id: &str) -> NodeId {
    detail().child("sessions").item(&safe(id))
}

/// `worktrees.detail.overlaps.item[<WorktreeKey>]`.
pub(crate) fn overlap(other: &Path) -> NodeId {
    detail()
        .child("overlaps")
        .item(&safe(&other.to_string_lossy()))
}
