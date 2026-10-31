//! The probe's node IDs the snapshot carries (docs/design/worktrees.md「界面」
//! 的语义节点表): the row of a worktree, a session row and an overlap row of the
//! inspector. The other nodes belong to the views of step 2.
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

fn detail() -> NodeId {
    NodeId::new("worktrees", "detail", None)
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
