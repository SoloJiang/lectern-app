//! Claude Code 工具名到 `ToolKind` 的分类表（docs/design/agents.md「记录」）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::session::ToolKind;
use serde_json::value::RawValue;

use super::format::ToolInput;
pub(crate) use crate::path::absolute;
use crate::stats::ParseStats;

/// 工具名属于哪一类。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Class {
    /// 路径取 `file_path`。
    EditFile,
    /// 路径取 `notebook_path`。
    EditNotebook,
    Execute,
    ReadFile,
    ReadNotebook,
    /// 路径取 `path`，没有时为 cwd。
    Search,
    SpawnAgent,
    Other,
}

pub(super) fn class(name: &str) -> Class {
    match name {
        "Write" | "Edit" | "MultiEdit" => Class::EditFile,
        "NotebookEdit" => Class::EditNotebook,
        "Bash" => Class::Execute,
        "Read" => Class::ReadFile,
        "NotebookRead" => Class::ReadNotebook,
        "Grep" | "Glob" | "LS" => Class::Search,
        "Task" | "Agent" => Class::SpawnAgent,
        _ => Class::Other,
    }
}

/// 一次 `tool_use` 的分类。`cwd` 是这条记录的 cwd。表外的工具名归 `Other` 并计数。
pub(super) fn kind(
    name: &str,
    input: Option<&RawValue>,
    cwd: &Path,
    stats: &mut ParseStats,
) -> ToolKind {
    let class = class(name);
    let input = match class {
        Class::SpawnAgent => return ToolKind::SpawnAgent,
        Class::Other => {
            stats.unknown_tool(name);
            return ToolKind::Other;
        }
        _ => input
            .and_then(|raw| serde_json::from_str::<ToolInput>(raw.get()).ok())
            .unwrap_or_default(),
    };
    let one = |p: Option<&str>| -> Arc<[PathBuf]> {
        p.map(|p| vec![absolute(cwd, p)]).unwrap_or_default().into()
    };
    match class {
        Class::EditFile => ToolKind::Edit {
            paths: one(input.file_path.0.as_deref()),
        },
        Class::EditNotebook => ToolKind::Edit {
            paths: one(input.notebook_path.0.as_deref()),
        },
        Class::Execute => ToolKind::Execute {
            command: input.command.0.as_deref().unwrap_or_default().into(),
            workdir: cwd.into(),
            exec_session: None,
            workdir_exact: true,
        },
        Class::ReadFile => ToolKind::Read {
            paths: one(input.file_path.0.as_deref()),
        },
        Class::ReadNotebook => ToolKind::Read {
            paths: one(input.notebook_path.0.as_deref()),
        },
        Class::Search => ToolKind::Search {
            paths: one(Some(input.path.0.as_deref().unwrap_or(""))),
        },
        Class::SpawnAgent | Class::Other => unreachable!("returned above"),
    }
}
