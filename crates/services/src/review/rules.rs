//! 简报规则的扩展点（docs/design/review.md「简报规则的扩展点」）。
//!
//! 需要别的插件知识的简报规则由提供者登记进来。[`BriefRules`] 不是 kernel 服务：
//! kernel 没有可选依赖，所以方向反过来，与 `NavigationProvider` 相同。提供者的插件
//! `requires` `dyn Review`，在 `started` 里调用 `Review::register_brief_rules`，把
//! 返回的 [`BriefRulesRegistration`] 交给 `PluginHandle::own`。

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::{FileKey, LineAnchor};
use crate::changes::DiffHunk;
use crate::project::{BlobId, ProjectKey, RelPath, WorktreeKey};

/// 简报规则的提供者。不实现 `Service`。
pub trait BriefRules: Send + Sync {
    /// 提供者的稳定 ID（插件 ID），也是 `BriefRule::Provided::provider`。不阻塞。
    fn id(&self) -> Arc<str>;

    /// 立即返回，任何线程。结果经 `reply` 交回，可以分几次，最后一次 `done` 为 true。
    /// 同一审阅的简报重算时，`review` 丢弃上一次的 `BriefReply`（`cancelled()` 变为
    /// true），提供者应停止。
    fn evaluate(&self, request: BriefRequest, reply: BriefReply);
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct BriefRequest {
    pub project: ProjectKey,
    pub worktree: Option<WorktreeKey>,
    /// 噪声文件、路径不是 UTF-8 的文件不在其中。
    pub files: Arc<[BriefFile]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct BriefFile {
    pub key: FileKey,
    pub path: RelPath,
    pub old_oid: Option<BlobId>,
    /// 工作区一侧的 OID：内容按 `TextOrigin::Worktree` 读、按它核对。
    pub new_oid: Option<BlobId>,
    pub hunks: Arc<[DiffHunk]>,
}

/// 提供者给出的一条简报。经过与模型备注相同的对账：`file` 不在快照里的丢弃；`line`
/// 不是最终改动里显示的行时降为文件备注；每个提供者最多 8 条。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ProvidedItem {
    pub rule: Arc<str>,
    pub critical: bool,
    pub file: Option<FileKey>,
    pub line: Option<LineAnchor>,
    pub args: Arc<[(Arc<str>, Arc<str>)]>,
}

/// 提供者交回结果的通道。`review` 用 `new` 构造，传入的回调只把结果放进队列、
/// 不阻塞；`cancelled` 由 `review` 在丢弃这次请求时置位。提供者只调用 `send` 和
/// `cancelled`。`send` 在任何线程上调用，不阻塞。不序列化：进程插件的代理在宿主
/// 进程里持有它。
pub struct BriefReply {
    send: Box<dyn Fn(Vec<ProvidedItem>, bool) + Send + Sync>,
    cancelled: Arc<AtomicBool>,
}

impl BriefReply {
    pub fn new(
        send: impl Fn(Vec<ProvidedItem>, bool) + Send + Sync + 'static,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        BriefReply {
            send: Box::new(send),
            cancelled,
        }
    }

    /// 交回一批条目；`done` 为 true 的是最后一批。
    pub fn send(&self, items: Vec<ProvidedItem>, done: bool) {
        (self.send)(items, done);
    }

    /// `review` 已丢弃这次请求，提供者应停止。
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl fmt::Debug for BriefReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BriefReply")
            .field("cancelled", &self.cancelled())
            .finish_non_exhaustive()
    }
}

/// `register_brief_rules` 的登记，drop 时注销（任何线程，不阻塞）。`Send`，可以交给
/// `PluginHandle::own`。`review` 用 `new` 构造，传入的回调持有登记表的 `Weak` 和登记
/// 序号；`review` 已停用时回调不做任何事。回调最多调用一次，不得 panic。不序列化。
pub struct BriefRulesRegistration {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl BriefRulesRegistration {
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        BriefRulesRegistration {
            release: Some(Box::new(release)),
        }
    }
}

impl Drop for BriefRulesRegistration {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for BriefRulesRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BriefRulesRegistration")
            .finish_non_exhaustive()
    }
}
