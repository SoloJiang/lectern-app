//! 并行 worktree 总览（`Review::worktrees`，docs/design/worktrees.md「对其他 crate 的要求」A）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::project::{BlobId, ProjectKey, RelPath, WorktreeKey};
use crate::session::Timestamp;

/// 一个项目全部 worktree 的改动计数与文件重叠。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct WorktreesOverview {
    pub project: ProjectKey,
    pub generation: u64,
    /// 主干：`Repo::default_branch()` 的完整引用名。没有时为 None，`committed`、
    /// `vs_base` 为 `ChangeCount::Unavailable`。
    pub base: Option<Arc<str>>,
    /// `ProjectSnapshot::worktrees` 里不是 prunable 的 worktree，顺序相同。
    pub worktrees: Arc<[WorktreeChanges]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct WorktreeChanges {
    pub worktree: WorktreeKey,
    /// 这份计数依据的 HEAD 提交；unborn 或读不出时为 None。
    pub head: Option<BlobId>,
    /// 相对 HEAD 未提交的改动（含未跟踪文件）。
    pub uncommitted: ChangeCount,
    /// 相对主干已提交的改动：merge-base 到 HEAD 之间改变的路径。
    pub committed: ChangeCount,
    /// 相对主干的改动文件：`uncommitted` 与 `committed` 路径的并集。
    pub vs_base: ChangeCount,
    /// 与其他 worktree 共同改动的文件，按 `files` 从多到少，至多 8 项。None：这个
    /// worktree 的路径集合未知（`Pending`、`Capped`、`Unavailable`）；路径集合未知的
    /// worktree 也不出现在别人的列表里。
    pub overlaps: Option<Arc<[Overlap]>>,
    /// 最近一次完整计算的时刻（kernel 的 `Clock`）；还没有算过时为 None。
    pub counted_at: Option<Timestamp>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ChangeCount {
    Pending,
    Files(u32),
    /// 超过 4000 个改动路径（ADR 0035）：不保留路径集合。
    Capped,
    /// prunable、HEAD 读不出、没有主干（`committed`、`vs_base`）或比较失败。
    Unavailable,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Overlap {
    pub other: WorktreeKey,
    pub files: u32,
    /// 共同路径按字节序的前 10 个，相对 worktree 根。两个方向的项共用同一个 `Arc`。
    pub paths: Arc<[RelPath]>,
}
