//! MR 来源的扩展点（docs/design/usage.md「接口」与前置条件 F）。
//!
//! [`MrSource`] 不是 kernel 服务：kernel 没有可选依赖，所以方向反过来，与
//! `BriefRules` 相同。`forge` 里的一个小插件 `requires` `dyn Usage` 和 forge 自己的
//! 服务，在 `started` 里调用 `Usage::register_mr_source`，把返回的
//! [`MrSourceRegistration`] 交给 `PluginHandle::own`。

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::project::BlobId;
use crate::session::Timestamp;

/// MR 数据的提供者，`forge` 实现。不实现 `Service`。三个方法都阻塞（可能请求网络），
/// 只在 executor 上调用；离线或被限流时给出缓存，没有缓存时返回 `Offline`，`usage`
/// 按分支显示、之后再试。
pub trait MrSource: Send + Sync {
    /// 远端地址对应的代码平台仓库。
    fn repo(&self, remote_url: &str) -> Result<Option<ForgeRepo>, MrSourceError>;

    /// 以这个仓库的这个分支为源分支的全部 MR，含已合并、已关闭的。
    fn mrs_for_branch(
        &self,
        repo: &ForgeRepo,
        branch: &str,
    ) -> Result<Arc<[MrInfo]>, MrSourceError>;

    /// head 包含这个提交、目标分支（合并前的提交）不包含它的 MR。
    fn mrs_containing(
        &self,
        repo: &ForgeRepo,
        commit: &BlobId,
    ) -> Result<Arc<[MrInfo]>, MrSourceError>;
}

/// 平台主机加平台给仓库的不变 ID（GitHub repository id、GitLab project id）；ID 只在
/// 同一主机上唯一。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ForgeRepo {
    pub host: Arc<str>,
    pub id: Arc<str>,
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct MrRef {
    pub repo: ForgeRepo,
    pub number: u64,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MrInfo {
    pub mr: MrRef,
    pub title: Arc<str>,
    /// 源仓库与源分支名。
    pub source: (ForgeRepo, Arc<str>),
    /// 目标仓库与目标分支名。
    pub target: (ForgeRepo, Arc<str>),
    pub head: BlobId,
    /// 代码平台记录过的 base 提交（GitHub `base.sha`、GitLab `diff_refs.base_sha`）。
    pub bases: Arc<[BlobId]>,
    /// 已合并的 MR：合并前目标分支的提交。
    pub merged_target: Option<BlobId>,
    /// 打开、关闭、重新打开、合并及其时间，按时间排序。
    pub timeline: Arc<[(Timestamp, MrEventKind)]>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum MrEventKind {
    Opened,
    Closed,
    Reopened,
    Merged,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum MrSourceError {
    Offline,
    NotFound,
    Other(Arc<str>),
}

/// `register_mr_source` 的登记，drop 时注销（任何线程，不阻塞）。`Send`，可以交给
/// `PluginHandle::own`。`usage` 用 `new` 构造，传入的回调持有登记表的 `Weak` 和登记
/// 序号；`usage` 已停用时回调不做任何事。回调最多调用一次，不得 panic。不序列化。
pub struct MrSourceRegistration {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl MrSourceRegistration {
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        MrSourceRegistration {
            release: Some(Box::new(release)),
        }
    }
}

impl Drop for MrSourceRegistration {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for MrSourceRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MrSourceRegistration")
            .finish_non_exhaustive()
    }
}
