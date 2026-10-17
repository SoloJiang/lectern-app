//! 语义导航的注册（#26 的升级钩子，docs/design/code_intel.md「语义导航的注册」）。
//!
//! jdtls、SCIP 导入是独立进程插件，在进程内由插件宿主的代理实现
//! [`NavigationProvider`]。提供者的插件 `requires` `dyn CodeIntel`，在 `started`
//! 里调用 `register_provider`，把返回的 [`ProviderRegistration`] 交给
//! `PluginHandle::own`。

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{LangName, Position, TextRef};
use crate::project::RelPath;

pub trait NavigationProvider: Send + Sync {
    /// 稳定 ID，日志用。
    fn id(&self) -> &str;

    fn handles(&self, lang: &LangName) -> bool;

    /// 只对内容与 HEAD 相同的文件回答（SCIP 索引按提交生成）。
    fn head_only(&self) -> bool;

    /// 立即返回，不阻塞；结果经 `reply` 在任意线程上交回。按需启动、空闲退出由
    /// 提供者自己负责（架构规则 3）。
    fn definition(&self, request: SemanticRequest, reply: SemanticReply);

    fn references(&self, request: SemanticRequest, reply: SemanticReply);

    fn cancel(&self, request: SemanticRequestId);
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SemanticRequestId(pub u64);

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SemanticRequest {
    pub id: SemanticRequestId,
    pub worktree_root: Arc<Path>,
    pub at: TextRef,
    pub position: Position,
    /// `at` 的内容与 HEAD 里这个路径的内容相同。`head_only` 的提供者只在为 true 时
    /// 被调用。
    pub head_clean: bool,
}

/// 提供者交回结果的通道。`code_intel` 用 `new` 构造，传入的回调只把结果放进队列、
/// 不阻塞，查询已关闭时丢弃。提供者只调用 `send`；没有调用就 drop 时，回调随之
/// drop，不被调用。不序列化：进程插件的代理在宿主进程里持有它。
pub struct SemanticReply {
    deliver: Deliver,
}

type Deliver = Box<dyn FnOnce(Result<Arc<[SemanticLocation]>, SemanticError>) + Send>;

impl SemanticReply {
    pub fn new(
        deliver: impl FnOnce(Result<Arc<[SemanticLocation]>, SemanticError>) + Send + 'static,
    ) -> Self {
        SemanticReply {
            deliver: Box::new(deliver),
        }
    }

    pub fn send(self, result: Result<Arc<[SemanticLocation]>, SemanticError>) {
        (self.deliver)(result);
    }
}

impl fmt::Debug for SemanticReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SemanticReply").finish_non_exhaustive()
    }
}

/// 提供者只给出工作区里的路径和位置；`code_intel` 用这次查询开始时的映射快照补上
/// OID（`origin` 为 `Worktree`），映射里没有的路径（已删除、被忽略、不在当前
/// worktree）丢弃。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SemanticLocation {
    pub path: RelPath,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SemanticError {
    Unavailable,
    Timeout,
    Failed(Arc<str>),
}

/// `register_provider` 的登记，drop 时注销。`Send`，可以交给 `PluginHandle::own`。
/// `code_intel` 用 `new` 构造，传入的回调持有登记表的 `Weak` 和登记序号；
/// `code_intel` 已停用时回调不做任何事。回调最多调用一次，不得 panic。
pub struct ProviderRegistration {
    unregister: Option<Box<dyn FnOnce() + Send>>,
}

impl ProviderRegistration {
    pub fn new(unregister: impl FnOnce() + Send + 'static) -> Self {
        ProviderRegistration {
            unregister: Some(Box::new(unregister)),
        }
    }
}

impl Drop for ProviderRegistration {
    fn drop(&mut self) {
        if let Some(unregister) = self.unregister.take() {
            unregister();
        }
    }
}

impl fmt::Debug for ProviderRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRegistration")
            .finish_non_exhaustive()
    }
}
