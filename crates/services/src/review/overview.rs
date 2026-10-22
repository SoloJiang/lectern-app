//! 项目主页的概览（`Review::overview`，docs/design/review.md「接口」「概览」）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::Progress;
use crate::project::ProjectKey;
use crate::session::SessionKey;

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Overview {
    pub project: ProjectKey,
    pub generation: u64,
    /// 当前 worktree 里在声明窗口内（最近 14 天）、至少声明了一个改动文件的会话，
    /// 按 `last_activity` 从新到旧，最多 50 个。
    pub sessions: Arc<[SessionOverview]>,
    /// 当前 worktree 不属于任何会话的改动。
    pub outside: OutsideCount,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SessionOverview {
    pub session: SessionKey,
    /// 已看进度，文件集合与这个会话的审阅快照的 `files` 相同；还没算出（会话的基准不是
    /// 当前 HEAD，且超出了每轮的计算数量）时为 None。
    pub progress: Option<Progress>,
    /// 有 Drifted 的文件：只在归因缓存命中时知道，否则为 None。
    pub drifted: Option<bool>,
    /// 规则简报里 critical 的条数（不含需要逐行来源的规则）。
    pub critical: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum OutsideCount {
    Pending,
    Files(u32),
    /// 改动路径超过 4000 个（ADR 0035）：不保留改动集，只给出全部改动路径数，不区分
    /// 是否属于会话。
    Capped(u32),
}
