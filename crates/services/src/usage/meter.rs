//! Analyze 的计量（`Usage::begin_analysis`、`Usage::meter`，ADR 0019，
//! docs/design/usage.md「接口」与前置条件 G）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::MrRef;
use crate::project::{ProjectKey, WorktreeKey};
use crate::session::{Agent, RawTokens, SessionKey, Timestamp, TurnId};

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct AnalysisStart {
    pub agent: Agent,
    pub at: Timestamp,
    pub target: AnalysisTarget,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum AnalysisTarget {
    /// 会话简报发起：被分析的会话与轮次（None 为全部轮次）。
    Session {
        session: SessionKey,
        turns: Option<Arc<[TurnId]>>,
    },
    /// 会话外的改动、比较范围：worktree 当时的分支。
    Worktree {
        project: ProjectKey,
        worktree: WorktreeKey,
    },
    /// MR 视图发起（P3）。
    Mr { mr: MrRef },
}

/// `begin_analysis` 给出的一次分析。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct AnalysisId(pub u64);

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MeterEvent {
    pub analysis: AnalysisId,
    /// 同一次分析里请求的序号，凭据转发端点分配。
    pub request: u32,
    pub at: Timestamp,
    pub model: Arc<str>,
    pub state: MeterState,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum MeterState {
    /// 转发之前：按上限估算的 token（ADR 0019 的 300k 上限内由 agents 估算）。
    Prepared { upper: RawTokens },
    /// 请求体已写入上游。
    Forwarded,
    /// 上游返回 2xx。
    Accepted,
    /// 收到用量；流式响应可能多次，后一次替换前一次，`done` 为 true 时是最终值。
    Usage { tokens: RawTokens, done: bool },
    /// 非 2xx 且没有用量。
    Rejected { status: u16 },
}
