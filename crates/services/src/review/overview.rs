//! 项目主页的概览（`Review::overview`，docs/design/review.md「接口」「概览」）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::Progress;
use crate::project::{ProjectKey, RelPath};
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
    /// 当前 worktree 里今天改动的文件按子项目的计数：计数改动集（ADR 0035）里工作区一侧
    /// stat 的 mtime 落在本地自然日（含今天，与 usage 设计「取整与时间窗口」的日界相同）内的
    /// 路径，按 `ProjectSnapshot::active.packages` 的 `dir` 最长前缀归到一个子项目，不在任何
    /// 子项目里的不计。`OutsideCount::Capped` 时为 None（没有改动集）；`packages` 还是
    /// `Pending`（或 `Unavailable`）时为 None。只列计数不为 0 的子项目，按 `dir` 排序；
    /// 没有列出的子项目计 0。
    pub packages: Option<Arc<[(RelPath, u32)]>>,
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

impl SessionOverview {
    /// 项目主页的 Needs Review 与 Welcome 的待审数共用的判断：进度还没算出、
    /// 有没看过的文件，或看过之后又改了（docs/design/review.md「接口」）。
    pub fn needs_review(&self) -> bool {
        match self.progress {
            None => true,
            Some(p) => p.seen < p.total || p.changed_since_seen > 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum OutsideCount {
    Pending,
    Files(u32),
    /// 改动路径超过 4000 个（ADR 0035）：不保留改动集，只给出全部改动路径数，不区分
    /// 是否属于会话。
    Capped(u32),
}
