//! 改动集与它的条目（docs/design/review.md「改动来源」）。字段与 `git::ChangeSet`
//! 逐个对应，见那一节的对照表。

use std::any::Any;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::project::BlobId;

/// 不可变。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ChangeList {
    /// 同一个来源上每份新的改动集加一。
    pub generation: u64,
    /// 基准树。
    pub base: BlobId,
    /// 按（新路径的字节序，`untracked`）排序，与 `git::ChangeSet::files` 的顺序和不变量
    /// 相同。
    pub files: Arc<[FileChange]>,
    pub totals: ChangeTotals,
    /// 实现方的句柄（本地来源放这份改动集对应的 `git::ChangeSet`，`texts` 和 `refresh`
    /// 用它）。不序列化，消费方不解读。
    #[serde(skip)]
    pub handle: ListHandle,
}

/// `ChangeList::handle`：实现方放进去的不透明句柄，只有给出这份改动集的实现自己
/// 向下转型。反序列化得到的改动集没有句柄（None）。
///
/// 不是改动集的值的一部分：两个句柄总是相等，`Debug` 不打印内容，所以
/// `ChangeList` 的比较与 JSON 往返只看其余字段。
#[derive(Clone, Default)]
pub struct ListHandle(pub Option<Arc<dyn Any + Send + Sync>>);

impl PartialEq for ListHandle {
    fn eq(&self, _other: &ListHandle) -> bool {
        true
    }
}

impl Eq for ListHandle {}

impl fmt::Debug for ListHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ListHandle").finish_non_exhaustive()
    }
}

/// 相对 worktree 根、`/` 分隔的原始字节（git 允许非 UTF-8 的名字）。不变量与
/// `git::RepoPath` 相同；本地来源与 `RepoPath` 共用同一个 `Arc<[u8]>`，不复制。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ChangePath(pub Arc<[u8]>);

impl ChangePath {
    /// UTF-8 时为 Some。
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FileChange {
    pub status: ChangeStatus,
    /// 新路径；删除时为旧路径。
    pub path: ChangePath,
    pub old: Option<FileSide>,
    pub new: Option<FileSide>,
    pub detail: ChangeDetail,
    /// `.gitattributes` 的 `linguist-generated`。
    pub generated: bool,
    pub untracked: bool,
    /// index 里这个路径的条目与工作区一侧不同（`git::Change::staged`）。
    pub staged: Option<FileSide>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ChangeStatus {
    Added,
    Deleted,
    Modified,
    TypeChanged,
    Renamed { from: ChangePath, similarity: u8 },
    Conflicted,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FileSide {
    pub mode: FileMode,
    pub content: SideIdentity,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum FileMode {
    File,
    Executable,
    Symlink,
    Gitlink,
    Special,
}

/// 一侧内容的身份。已看标记和缓存键按它比较。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SideIdentity {
    Blob(BlobId),
    /// 比较时算过 OID，但字节超过保留上限没有留下。
    NotRetained(BlobId),
    /// 超过工作区单文件读取上限，没有读内容。
    Unhashed {
        size: u64,
        mtime_ns: i64,
        inode: u64,
    },
    /// 使用了 clean / process 过滤器：原始字节的 SHA-256，或超过上限时的 stat。
    Filtered(RawIdentity),
    Unavailable(UnavailableKind),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RawIdentity {
    Sha256([u8; 32]),
    Stat {
        size: u64,
        mtime_ns: i64,
        inode: u64,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UnavailableKind {
    UnsupportedType,
    PathThroughSymlink,
    NestedRepository,
    Missing,
    Unreadable,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ChangeDetail {
    Text {
        hunks: Arc<[DiffHunk]>,
        added: u32,
        removed: u32,
    },
    Truncated {
        added: u32,
        removed: u32,
    },
    Binary {
        old_size: Option<u64>,
        new_size: Option<u64>,
    },
    TooLarge,
    Listed,
    ModeOnly,
    StagedOnly,
    NoDiff,
}

/// 行号从 0 起，区间覆盖两侧完整文件；字节偏移是这一侧规范内容里第一行的偏移
/// （git 设计「字节偏移」）。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct DiffHunk {
    pub old: Range<u32>,
    pub new: Range<u32>,
    pub old_byte: u64,
    pub new_byte: u64,
}

#[derive(Clone, Default, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ChangeTotals {
    pub files: u32,
    pub added: u64,
    pub removed: u64,
    pub truncated: u32,
    pub too_large: u32,
    pub listed: u32,
    pub not_retained: u32,
    pub untracked_capped: bool,
    /// 不满足路径不变量、没有读取的路径（原始字节）。
    pub invalid_paths: Arc<[Arc<[u8]>]>,
}
