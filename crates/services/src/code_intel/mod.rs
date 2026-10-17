//! 代码导航服务：跳转到定义、查找引用与调用方、⌘K 的符号名字表
//! （docs/design/code_intel.md「接口」）。
//!
//! `code-intel` 插件实现 [`CodeIntel`]；消费方（`context_drawer`、⌘K、
//! `file_reader`、审阅视图）和语义导航的提供者（#26）只依赖本模块。这里只有
//! 数据，没有行为，`NameTable` 的构造与查找、`SemanticReply` 和
//! `ProviderRegistration` 的回调除外。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_kernel::{ActivationToken, Event, Service};

use super::project::{ProjectKey, RelPath, WorktreeKey};

mod names;
mod query;
mod semantic;
mod symbol;

#[cfg(test)]
mod tests;

pub use names::{NameTable, NameTableBuilder, Names};
pub use query::{
    Completeness, Query, QueryError, QueryId, QueryKind, QueryResults, QueryState, SemanticState,
};
pub use semantic::{
    NavigationProvider, ProviderRegistration, SemanticError, SemanticLocation, SemanticReply,
    SemanticRequest, SemanticRequestId,
};
pub use symbol::{
    Binding, Candidate, CandidateSource, CandidateTier, DefKind, DefinitionRef, Evidence, LangName,
    Location, Position, RefSyntax, Reference, Symbol, TextOrigin, TextRef,
};

/// 单实现服务，`code-intel` 插件提供。
pub trait CodeIntel: Send + Sync {
    /// 更新一类焦点，替换这一类上一次的值。立即返回，任何线程可调用（含 UI 线程）。
    /// 焦点决定第 1、2 层建索引的内容，也决定候选排序里的「本次审阅改到的文件」。
    /// `worktree` 是焦点里路径所在的 worktree（文件夹项目为 None）：只有它是当前
    /// worktree 时焦点才参与建索引；切换当前 worktree 后，各类焦点保留在原 worktree
    /// 名下，调用方对新的 worktree 重新设置。
    fn set_focus(&self, project: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus);

    /// 开始一次查询。立即返回，任何线程可调用。结果分批写进这次查询的
    /// `QueryResults`，每写一批发出一次 `CodeIntelEvent::Results`；项目没有打开时
    /// 结果直接是 `Failed(NotOpen)`。
    fn query(&self, query: Query) -> QueryId;

    /// 查询的当前结果。只取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程可调用。
    /// 查询已关闭或被挤出（同时保留的查询最多 16 个，超出时关闭最早的）时返回 None，
    /// 调用方重新查询。
    fn results(&self, id: QueryId) -> Option<Arc<QueryResults>>;

    /// 不再需要这次查询：取消未完成的部分，丢弃结果。立即返回。
    fn close(&self, id: QueryId);

    /// 光标或指针在符号上停留约 150 ms 后预取：只做跳转的前三步，结果不发事件。
    /// 2 秒内同样的 `query` 沿用它，前提是映射的代数、淘汰代数和语言包代数都没有变；
    /// 变了就重新查询。立即返回。
    fn prefetch(&self, query: Query);

    /// ⌘K 的符号名字表。立即返回：仍有持有者的表返回 `Ready`，它的代数落后时同时
    /// 开始重建；没有时开始构建并返回 `Building`，已在构建时不另起一次。建好的表随
    /// `CodeIntelEvent::Names` 交出；表有持有者期间代数落后时，实现方在后台重建并
    /// 再发一次 `Names`。电池供电时，这次调用让第 4、5 层恢复。
    fn names(&self, project: &ProjectKey) -> Names;

    /// 注册一个语义导航提供者（#26）。立即返回。实现方每次调用提供者之前
    /// `token.enter()`，取不到准入就当作没有登记。返回值 drop 时注销。
    fn register_provider(
        &self,
        provider: Arc<dyn NavigationProvider>,
        token: ActivationToken,
    ) -> ProviderRegistration;

    /// 各层的覆盖情况。只读内存，不阻塞。只给测试和 bench 的探针用，界面不显示
    /// （ADR 0012）。
    fn coverage(&self, project: &ProjectKey) -> Option<Coverage>;
}

impl Service for dyn CodeIntel {
    const ID: &'static str = "code-intel";
    const MULTIPLE: bool = false;
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Focus {
    /// 当前打开的文本：完整文件阅读里的文件，或 Diff 里正在看的那一侧。第 1 层。
    Current(Option<TextRef>),
    /// 抽屉里正在看的候选文件。第 1 层。
    Drawer(Arc<[RelPath]>),
    /// 本次审阅改到的文件，按改动叙事的顺序。第 2 层；同层候选里排在前面。
    Review(Arc<[RelPath]>),
    /// agent 本会话写过、读过的文件（ADR 0020 的工具调用事件）。第 2 层。
    Touched(Arc<[RelPath]>),
}

/// 在 executor 线程上、快照替换之后发出。监听只更新状态或安排重绘，不做 IO。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CodeIntelEvent {
    Results {
        id: QueryId,
        generation: u32,
    },
    /// 带着建好的表：实现方自己只保留 `Weak`，表的所有权在这里交给监听方。
    Names {
        project: ProjectKey,
        generation: u64,
        table: Arc<NameTable>,
    },
}

impl Event for CodeIntelEvent {}

/// 各层的覆盖情况（`CodeIntel::coverage`）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Coverage {
    pub generation: u64,
    /// 第 1、2 层。
    pub focus: TierState,
    /// 第 3 层。
    pub subproject: TierState,
    /// 第 4 层。
    pub worktree: TierState,
    /// 第 5 层。
    pub other_worktrees: TierState,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum TierState {
    Pending,
    Done,
    /// 电池供电时暂缓。
    Deferred,
}
