//! The probe's node IDs of this crate (docs/design/review_view.md「界面」,
//! UI 探针的语义节点). `lectern_ui::semantic::NodeId` writes a keyed node as
//! `view.area.item[key]`; the keys are business keys (paths, step IDs, line
//! numbers), never indices of the list.

use lectern_services::review::{FileKey, RowsKey, StepId, TurnRef};
use lectern_ui::semantic::NodeId;

use crate::brief::file_text;

const VIEW: &str = "review";

pub(crate) fn plain(area: &'static str) -> NodeId {
    NodeId::new(VIEW, area, None)
}

pub(crate) fn keyed(area: &'static str, key: &str) -> NodeId {
    NodeId::new(VIEW, area, Some(key))
}

/// `<session>:<ordinal>`
pub(crate) fn turn_key(at: TurnRef) -> String {
    format!("{}:{}", at.session, at.ordinal)
}

pub(crate) fn step_key(id: StepId) -> String {
    id.0.to_string()
}

/// `final:<path>` or `step:<id>`.
pub(crate) fn rows_key_text(rows: &RowsKey) -> String {
    match rows {
        RowsKey::Final { file, .. } => format!("final:{}", file_text(file)),
        RowsKey::Step { step, .. } => format!("step:{}", step.0),
    }
}

pub(crate) fn file_key(file: &FileKey) -> String {
    file_text(file)
}

/// `<old>:<new>` with `-` for a side that has no line.
pub(crate) fn lines_key(old: Option<u32>, new: Option<u32>) -> String {
    let side = |line: Option<u32>| line.map_or_else(|| "-".to_owned(), |line| line.to_string());
    format!("{}:{}", side(old), side(new))
}
