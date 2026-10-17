//! 会话目录（docs/design/agents.md「依赖」里的 `AgentsDeps::roots`）。

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// 两种 agent 的配置目录。只是路径，不保证存在；会话根目录在它们之下（[`AgentRoots::claude_sessions`]、
/// [`AgentRoots::codex_sessions`]），还不存在时由 `fs_watch` 监视最近的已存在上级目录。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AgentRoots {
    /// Claude Code 的配置目录，会话在每个目录的 `projects/` 下。
    pub claude: Vec<PathBuf>,
    /// Codex 的主目录（`CODEX_HOME`），会话在 `sessions/` 与 `archived_sessions/` 下。
    pub codex: Vec<PathBuf>,
}

impl AgentRoots {
    /// 按进程环境取默认目录：`$CLAUDE_CONFIG_DIR`（逗号分隔）或 `~/.claude` 与 `~/.config/claude`，
    /// `$CODEX_HOME` 或 `~/.codex`。只读环境变量，不访问文件系统（「未决问题」1：只在 shell 里设置的
    /// 变量读不到，默认按 (a) 处理）。
    pub fn from_env(home: &Path) -> AgentRoots {
        AgentRoots::from_vars(
            home,
            std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
            std::env::var_os("CODEX_HOME").as_deref(),
        )
    }

    fn from_vars(home: &Path, claude: Option<&OsStr>, codex: Option<&OsStr>) -> AgentRoots {
        let listed: Vec<PathBuf> = claude
            .and_then(OsStr::to_str)
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        let claude = if listed.is_empty() {
            vec![home.join(".claude"), home.join(".config/claude")]
        } else {
            listed
        };
        let codex = match codex.filter(|v| !v.is_empty()) {
            Some(dir) => vec![PathBuf::from(dir)],
            None => vec![home.join(".codex")],
        };
        AgentRoots { claude, codex }
    }

    /// Claude Code 的会话根目录：每个配置目录下的 `projects/`。
    pub fn claude_sessions(&self) -> Vec<PathBuf> {
        dedup(self.claude.iter().map(|d| d.join("projects")).collect())
    }

    /// Codex 的会话根目录：每个主目录下的 `sessions/` 与 `archived_sessions/`。
    pub fn codex_sessions(&self) -> Vec<PathBuf> {
        dedup(
            self.codex
                .iter()
                .flat_map(|d| [d.join("sessions"), d.join("archived_sessions")])
                .collect(),
        )
    }
}

fn dedup(mut v: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    v.retain(|p| seen.insert(p.clone()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_roots_follow_the_environment() {
        let home = Path::new("/Users/dev");
        let default = AgentRoots::from_vars(home, None, None);
        assert_eq!(
            default.claude_sessions(),
            [
                PathBuf::from("/Users/dev/.claude/projects"),
                PathBuf::from("/Users/dev/.config/claude/projects")
            ]
        );
        assert_eq!(
            default.codex_sessions(),
            [
                PathBuf::from("/Users/dev/.codex/sessions"),
                PathBuf::from("/Users/dev/.codex/archived_sessions")
            ]
        );
        let set =
            AgentRoots::from_vars(home, Some(OsStr::new("/a, /b,,/a")), Some(OsStr::new("/c")));
        assert_eq!(
            set.claude_sessions(),
            [PathBuf::from("/a/projects"), PathBuf::from("/b/projects")]
        );
        assert_eq!(set.codex, [PathBuf::from("/c")]);
        let empty = AgentRoots::from_vars(home, Some(OsStr::new(" , ")), Some(OsStr::new("")));
        assert_eq!(empty, default);
    }
}
