//! Claude Code 工具名到 `ToolKind` 的分类表（docs/design/agents.md「记录」）。

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use lectern_services::session::ToolKind;
use serde_json::value::RawValue;

use super::format::ToolInput;
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

/// 相对路径按 `cwd` 在词法上拼接并去掉 `.`、`..`，不访问文件系统。空路径是 `cwd` 本身。
pub(crate) fn absolute(cwd: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // `/..` 就是 `/`。
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_kind_paths_are_lexically_absolute() {
        let cwd = Path::new("/Users/dev/fixture/repo");
        assert_eq!(
            absolute(cwd, "src/../lib/./a.rs"),
            Path::new("/Users/dev/fixture/repo/lib/a.rs")
        );
        assert_eq!(absolute(cwd, "/etc/hosts"), Path::new("/etc/hosts"));
        assert_eq!(absolute(cwd, ""), cwd);
        assert_eq!(absolute(Path::new("/"), "../x"), Path::new("/x"));
    }
}
