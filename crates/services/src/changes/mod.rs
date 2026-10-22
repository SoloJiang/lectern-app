//! 改动来源：按能力划分的改动集接口（ADR 0019，docs/design/review.md「改动来源」）。
//!
//! 本地 worktree 由 `git` 的 `LocalChanges` 实现 [`ChangeSource`]，MR 由 `forge`
//! 实现（#25）。`review` 只经这个 trait 读改动集，归因和已看逻辑不区分来源。
//! `ChangeSource` 不是 kernel 服务：本地来源由 `review` 用 `git` 构造，MR 来源由
//! `forge` 的服务交出。这里只有数据，没有行为，`ChangePath::as_str` 和
//! `ChangeList::handle` 这个不透明句柄除外。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use super::project::{BlobId, ProjectKey, WorktreeKey};

mod list;

#[cfg(test)]
mod tests;

pub use list::{
    ChangeDetail, ChangeList, ChangePath, ChangeStatus, ChangeTotals, DiffHunk, FileChange,
    FileMode, FileSide, ListHandle, RawIdentity, SideIdentity, UnavailableKind,
};

/// 一个改动集的来源。不注册为 kernel 服务。
pub trait ChangeSource: Send + Sync {
    /// 来源的种类与能力。不阻塞。
    fn info(&self) -> &SourceInfo;

    /// 必需：取改动集。阻塞（读对象库、index、工作区或代码平台），只在 executor 上调用。
    /// `cancel` 置位时尽快返回 `ChangeError::Cancelled`，不给出部分结果。
    fn fetch(&self, cancel: &AtomicBool) -> Result<Arc<ChangeList>, ChangeError>;

    /// 能力 `refresh`：在上一份改动集上按提示刷新，没有变化的条目与 `prev` 共享。
    /// 没有这项能力的来源用默认实现。
    fn refresh(
        &self,
        prev: &Arc<ChangeList>,
        hint: &RefreshHint,
        cancel: &AtomicBool,
    ) -> Result<Arc<ChangeList>, ChangeError> {
        let _ = (prev, hint);
        self.fetch(cancel)
    }

    /// 必需：一个文件两侧的内容。阻塞。新一侧是这份改动集比较时读到的内容，不按路径
    /// 重读；超过 `max_bytes` 的一侧为 `TooLarge`。`file` 是 `ChangeList::files` 的下标。
    fn texts(&self, list: &ChangeList, file: u32, max_bytes: u64)
    -> Result<SideTexts, ChangeError>;

    /// 能力 `fingerprint`：审阅对象的指纹（ADR 0019 约束 9）。不阻塞。
    fn fingerprint(&self, list: &ChangeList) -> Option<[u8; 32]> {
        let _ = list;
        None
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SourceInfo {
    pub kind: SourceKind,
    pub caps: Capabilities,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SourceKind {
    /// 本地 worktree 的工作区（含未跟踪文件）相对一个基准。
    Worktree {
        project: ProjectKey,
        worktree: WorktreeKey,
        base: BaseSpec,
    },
    // MR 由 forge 的设计加入（#25）。
}

/// 读写审阅评论、读取共享分析结果两项能力随 forge 的设计加入，方法有默认实现，
/// 不影响已有实现。
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    pub refresh: bool,
    pub fingerprint: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum BaseSpec {
    Head,
    /// 某个提交的树，如会话开始时的 HEAD。
    Commit(BlobId),
    /// `merge_base(HEAD, 分支)` 的树；完整引用名，如 "refs/heads/main"。
    MergeBase(Arc<str>),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RefreshHint {
    /// 只重新比较这些路径（目录表示其下全部路径）。
    Paths(Arc<[ChangePath]>),
    /// HEAD、index、忽略规则、属性或配置变了，或事件丢失。
    Full,
}

/// 一个文件两侧的内容（`ChangeSource::texts`）。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SideTexts {
    pub old: SideText,
    pub new: SideText,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SideText {
    Absent,
    /// 规范内容。本地来源给出的就是 `git::Text::shared()` 那一份 `Arc`，
    /// `review_view` 用它登记 `Reader::share_object`（ADR 0042）。
    Text(Arc<[u8]>),
    TooLarge {
        size: u64,
    },
    NotRetained,
    Unavailable,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ChangeError {
    /// 仓库格式不支持（`git::GitError::Unsupported`），附原因。
    Unsupported(Arc<str>),
    NoMergeBase,
    TooManyEntries,
    /// `git` 的 `ObjectMissing`、`Corrupt`、`Io`、`TooLarge`，或代码平台的错误，附原因。
    Io(Arc<str>),
    Cancelled,
}
