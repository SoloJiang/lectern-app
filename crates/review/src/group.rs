//! Rule groups of a file list (docs/design/review.md「噪声文件与规则分组」):
//! the view shows them when the session log is unrecognized, for changes
//! outside any session and for a range.

use std::sync::Arc;

use crate::types::review::{Group, GroupKind, NoiseKind};

/// One file of the list, as the snapshot's `FileEntry` has it.
pub(crate) struct GroupInput<'a> {
    pub kind: GroupKind,
    /// The subproject the file is in (`FileEntry::package`).
    pub package: Option<&'a str>,
    pub path: &'a [u8],
}

/// The non-empty groups, in `GroupKind` declaration order (code, tests,
/// config, deps, docs, then each kind of noise); within a group by
/// subproject (files outside any subproject first), then path. `files` are
/// indices into `files`.
pub(crate) fn arrange(files: &[GroupInput<'_>]) -> Arc<[Group]> {
    let mut order: Vec<u32> = (0..files.len() as u32).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&files[a as usize], &files[b as usize]);
        (rank(x.kind), x.package, x.path).cmp(&(rank(y.kind), y.package, y.path))
    });
    let mut groups: Vec<Group> = Vec::new();
    let mut start = 0;
    while start < order.len() {
        let kind = files[order[start] as usize].kind;
        let end = order[start..]
            .iter()
            .position(|&i| files[i as usize].kind != kind)
            .map_or(order.len(), |n| start + n);
        groups.push(Group {
            kind,
            files: order[start..end].into(),
        });
        start = end;
    }
    groups.into()
}

/// The display position of a group.
fn rank(kind: GroupKind) -> u8 {
    match kind {
        GroupKind::Code => 0,
        GroupKind::Tests => 1,
        GroupKind::Config => 2,
        GroupKind::Deps => 3,
        GroupKind::Docs => 4,
        GroupKind::Noise(NoiseKind::Lockfile) => 5,
        GroupKind::Noise(NoiseKind::Generated) => 6,
        GroupKind::Noise(NoiseKind::Snapshot) => 7,
    }
}
