//! The probe's node IDs of this crate (docs/design/file_reader.md「界面」,
//! UI 探针的语义节点). `lectern_ui::semantic::NodeId` writes a keyed node as
//! `view.area.item[key]`; the keys are business keys (paths, byte offsets),
//! never indices of the list.

use lectern_services::reader::ReadTarget;
use lectern_ui::semantic::NodeId;

const VIEW: &str = "file-reader";

pub(crate) fn plain(area: &'static str) -> NodeId {
    NodeId::new(VIEW, area, None)
}

pub(crate) fn keyed(area: &'static str, key: &str) -> NodeId {
    NodeId::new(VIEW, area, Some(key))
}

/// `<worktree>:<path>`: the worktree by its directory's name (the key is
/// a path of the machine, which the IDs are not), `-` for a folder project.
pub(crate) fn document_key(target: &ReadTarget) -> String {
    let worktree = target
        .worktree
        .as_ref()
        .and_then(|key| key.0.file_name())
        .map_or_else(
            || "-".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
    format!("{worktree}:{}", target.path)
}

/// `<line start byte>.<display row of the line>`
pub(crate) fn row_key(line_start: u64, sub_row: u32) -> String {
    format!("{line_start}.{sub_row}")
}
