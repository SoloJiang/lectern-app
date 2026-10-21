//! 完整文件阅读服务（docs/design/file_reader.md「接口」）。
//!
//! `file-reader` 插件实现 [`Reader`]；审阅视图、上下文抽屉和 ⌘K 只依赖本模块，
//! 经它打开文件和请求预取（架构规则 7）。这里只有数据，没有行为，
//! `ObjectShare` 的回调除外。

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_kernel::{Event, Service};

use super::code_intel::TextOrigin;
use super::project::{BlobId, ProjectKey, RelPath, WorktreeKey};

#[cfg(test)]
mod tests;

/// 单实现服务，`file-reader` 插件提供。
pub trait Reader: Send + Sync {
    /// 请求打开完整文件阅读。立即返回，任何线程可调用；请求排到主线程，经离开守卫
    /// 之后才替换本窗口当前的文档：当前文档有未保存的编辑时先问保存 / 不保存 / 取消，
    /// 选「取消」时这次请求作废。结果以 `ReaderEvent::Open` 送达。审阅视图（⇧⌘O、
    /// 从 hunk）、抽屉的候选、⌘K 的文件结果都经它打开。
    fn open(&self, target: ReadTarget) -> OpenTicket;

    /// 交互静止时的预取（ADR 0033）：只取前 4 个，算出高亮区间放进 LRU，不打开、
    /// 不发事件。立即返回。
    fn prefetch(&self, targets: Vec<ReadTarget>);

    /// 登记一份调用方正在持有的对象库内容（审阅视图显示的 diff 两侧文本）。登记表
    /// 持有这份 `Arc` 的一个强引用，只到返回的 `ObjectShare` drop 为止；调用方在自己
    /// 持有这份内容期间保留它、放下内容时一起放下，所以登记不延长内容的生命期
    /// （ADR 0035）。打开这个 OID 的 `Object` 目标时，有登记就直接用，不再从对象库
    /// 解压。立即返回。
    fn share_object(&self, oid: BlobId, bytes: Arc<[u8]>) -> ObjectShare;
}

impl Service for dyn Reader {
    const ID: &'static str = "reader";
    const MULTIPLE: bool = false;
}

/// `share_object` 的登记，drop 时注销（任何线程，不阻塞）；同一个 OID 登记多次时，
/// 最后一个 drop 才移除。已经打开的文档自己持有它用到的那份 `Arc`，到文档关闭为止，
/// 不受注销影响。`file_reader` 用 `new` 构造，传入的回调持有登记表的 `Weak`、OID
/// 和登记序号；登记表已不在时回调不做任何事。回调最多调用一次，不得 panic。
/// 不序列化：进程插件的代理在宿主进程里持有它。
pub struct ObjectShare {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl ObjectShare {
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        ObjectShare {
            release: Some(Box::new(release)),
        }
    }
}

impl Drop for ObjectShare {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for ObjectShare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectShare").finish_non_exhaustive()
    }
}

/// 标识一次 `Reader::open`，与它的 `ReaderEvent::Open` 对应。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct OpenTicket(pub u64);

/// 在主线程上、阅读视图切换之后（或请求作废之后）发出。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ReaderEvent {
    Open {
        ticket: OpenTicket,
        outcome: OpenOutcome,
    },
}

impl Event for ReaderEvent {}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum OpenOutcome {
    Shown,
    /// 用户在离开守卫的提示里选了「取消」，或保存时发生冲突：当前文档和它的编辑
    /// 保持不变。
    Cancelled,
}

/// 要打开的文件和位置。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ReadTarget {
    pub project: ProjectKey,
    /// 文件夹项目为 None。
    pub worktree: Option<WorktreeKey>,
    pub path: RelPath,
    /// 工作区或对象库；依赖根随 `code_intel` 的依赖类型（`TextOrigin::Dependency`）
    /// 加入。
    pub origin: TextOrigin,
    /// 调用方看到的那份内容的 OID（hunk 的新旧一侧、`Location::oid`）。`Object`
    /// 必须有。
    pub expect: Option<BlobId>,
    pub at: Anchor,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Anchor {
    Start,
    /// 行和列从 0 起，列是行内的字节偏移。agent 输出里的 `file:line` 只有行号。
    Line {
        line: u32,
        column: u32,
    },
    /// `byte` 是 `expect` 那份内容里的字节偏移（`git` 的 `*_byte`、
    /// `Location::byte`），同时给出行号：读到的内容的 OID 与 `expect` 不同时按行号
    /// 定位。
    Byte {
        byte: u64,
        line: u32,
        column: u32,
    },
}
