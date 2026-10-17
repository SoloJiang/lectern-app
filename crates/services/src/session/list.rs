//! 会话键与会话列表（docs/design/agents.md「会话与列表」）。

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::ToolCallId;
use crate::project::{Location, ProjectKey, ResolveError, Resolved};

/// 第三方来源报告的其他 agent 用 `Other`，附它自己的名字。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Agent {
    ClaudeCode,
    Codex,
    Other(Arc<str>),
}

/// 提供者的稳定 ID，见 `SessionSource::id`。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct SourceId(pub Arc<str>);

/// 会话的全局键：来源加来源内的会话 ID。`id` 只在同一来源内唯一。
///
/// `agents` 的 `id`：Claude Code 为 `sessionId`，子 agent 为
/// `<父会话 sessionId>/agent-<agentId>`；Codex 为 `session_meta.id`（线程 ID）。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct SessionKey {
    pub source: SourceId,
    pub id: Arc<str>,
}

/// UTC 毫秒。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Timestamp(pub i64);

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionList {
    /// 每次替换加一，与 `SessionsChanged::generation` 一致。
    pub generation: u64,
    /// 按 `last_activity` 从新到旧。
    pub sessions: Arc<[SessionSummary]>,
    pub diagnostics: Diagnostics,
}

impl SessionList {
    /// 线性查找。
    pub fn get(&self, key: &SessionKey) -> Option<&SessionSummary> {
        self.sessions.iter().find(|s| s.key == *key)
    }

    /// 归位到这个项目的会话（`Place::Located` 或 `Place::Resolved`，含子 agent 会话），
    /// 按 `last_activity` 从新到旧。
    pub fn in_project<'a>(
        &'a self,
        project: &'a ProjectKey,
    ) -> impl Iterator<Item = &'a SessionSummary> + 'a {
        self.sessions.iter().filter(move |s| match &s.place {
            Place::Located(l) => l.project == *project,
            Place::Resolved(r) => r.project == *project,
            Place::Pending | Place::Missing | Place::Unresolvable(_) => false,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    pub key: SessionKey,
    pub agent: Agent,
    pub parent: Option<Parent>,
    pub file: Arc<Path>,
    /// 会话开始时的 cwd，原样取自日志。
    pub cwd: Arc<Path>,
    pub place: Place,
    pub started_at: Timestamp,
    /// 最后一条记录的时间戳；没有时、或 `tail_pending` 时取文件的修改时间。
    pub last_activity: Timestamp,
    /// 第一个 prompt 的第一行，最多 120 个字符；开头 256 KB 里没有 prompt 时为 None。
    pub title: Option<Arc<str>>,
    /// Claude Code 的 `version`，Codex 的 `cli_version`。
    pub agent_version: Option<Arc<str>>,
    /// 日志记录的开始时的分支，只用于导入的旧会话（ADR 0020）。
    pub logged_branch: Option<Arc<str>>,
    /// 最后一轮还没有结束。界面据此和 `last_activity` 判断是否在运行。
    pub open_turn: bool,
    /// 取自文件末尾的字段（`last_activity`、`open_turn`）还没有算出：此时
    /// `last_activity` 暂取文件修改时间，`open_turn` 为 false；`SessionSource::complete`
    /// 之后补齐，随下一次 `SessionsChanged` 更新。
    pub tail_pending: bool,
    /// Codex `archived_sessions/` 下的会话。
    pub archived: bool,
    /// 继承前缀的边界无法判断（来源会话不在本机），全部记录按自己的计。
    pub inheritance_unknown: bool,
    pub format: Format,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Parent {
    pub session: SessionKey,
    /// 派出它的工具调用。Claude Code 取 `agent-<id>.meta.json` 的 `toolUseId`，
    /// 没有这个文件时为 None。
    pub tool_call: Option<ToolCallId>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Place {
    /// 还没有归位（首次发现之后、`resolve` 完成之前）。
    Pending,
    /// cwd 在已打开的项目里：`Projects::locate` 的结果，含子项目。
    Located(Location),
    /// 项目没有打开：`Projects::resolve` 的结果，没有子项目信息。项目打开后重新归位为
    /// `Located`。
    Resolved(Resolved),
    /// cwd 已不存在。会话照常列出，不属于任何项目。
    Missing,
    Unresolvable(ResolveError),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Format {
    Known,
    /// 有记录被跳过（不认识的类型、坏行、超长行），数量见 `Diagnostics`。
    Partial,
    /// 开头 256 KB 里开始的行里找不到会话头。
    Unrecognized,
}

/// 每个会话与全局的计数，进日志，bench 读取。
#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Diagnostics {
    /// 不认识的 `type`（Codex 为 `type` 加 `payload.type`），最多记 32 种。
    pub unknown_types: Arc<[(Arc<str>, u32)]>,
    /// 不是合法 JSON，或缺少必需字段。
    pub bad_lines: u32,
    /// 超过 16 MB 的行。
    pub oversized_lines: u32,
    pub unreadable_dirs: Arc<[Arc<Path>]>,
}
