//! 规则分组与简报（docs/design/review.md「接口」「噪声文件与规则分组」「简报规则」）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::FileKey;
use crate::session::Timestamp;

/// 声明顺序即显示顺序。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum GroupKind {
    Code,
    Tests,
    Config,
    Deps,
    Docs,
    Noise(NoiseKind),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum NoiseKind {
    Lockfile,
    Generated,
    Snapshot,
}

/// 按 `GroupKind` 的声明顺序；组内按子项目、路径。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Group {
    pub kind: GroupKind,
    /// `ReviewSnapshot::files` 的下标。
    pub files: Arc<[u32]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Brief {
    /// critical 在前，其后按「简报规则」表的顺序，同一规则按文件的进度顺序。最多 12 条。
    pub items: Arc<[BriefItem]>,
    /// 超出 12 条没有列出的数量。
    pub omitted: u32,
    /// 已登记、请求过、还没有答复的 `BriefRules` 提供者数（视图可以显示「还在补充」）。
    pub pending_providers: u32,
    pub analysis: AnalysisState,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct BriefItem {
    pub rule: BriefRule,
    pub critical: bool,
    pub file: Option<FileKey>,
    /// 行备注：只挂在最终改动里显示的行上，否则为 None（降为文件备注）。
    pub line: Option<LineAnchor>,
    /// 合并成一条时的数量（如三个噪声文件）。
    pub count: u32,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum BriefRule {
    ChangedSinceSeen,
    Drifted,
    EditFailed,
    CommandOnly,
    Sensitive(SensitiveKind),
    Deleted,
    Incomplete(IncompleteKind),
    Noise(NoiseKind),
    /// `BriefRules` 提供者给出的规则；视图按 `brief-<provider>-<rule>` 取 Fluent 文案，
    /// `args` 是它的参数。
    Provided {
        provider: Arc<str>,
        rule: Arc<str>,
        args: Arc<[(Arc<str>, Arc<str>)]>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SensitiveKind {
    AgentInstructions,
    Ci,
    Hooks,
    GitMetadata,
    Symlink,
    Executable,
    Submodule,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum IncompleteKind {
    Truncated,
    TooLarge,
    Binary,
    NotRetained,
    Filtered,
    Unavailable,
    Listed,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct LineAnchor {
    pub side: DiffSide,
    pub line: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum DiffSide {
    Old,
    New,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum AnalysisState {
    /// #66 之前总是这一项；之后用于找不到命令行、系统不满足 ADR 0019 的约束等，附原因。
    Unavailable(Arc<str>),
    Idle,
    Running {
        started_at: Timestamp,
    },
    Done {
        stale: bool,
        partial: bool,
    },
    Failed(Arc<str>),
}
