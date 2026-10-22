//! 用量服务：按项目、分支、MR、会话聚合的 token 快照，额度快照，项目合并的命令，
//! Analyze 的计量和 MR 来源的扩展点（docs/design/usage.md「接口」）。
//!
//! `usage` 插件实现 [`Usage`]；`usage_view`、`project_home`、`worktrees`、
//! `review_view`、Analyze（`agents`，#66）和 MR 来源的提供者（[`MrSource`]，
//! `forge`，#25）只依赖本模块（架构规则 7）。这里只有数据，没有行为，
//! `Tokens::total` 与 `UsageInterest`、`MrSourceRegistration` 的回调除外。

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_kernel::{ActivationToken, Event, Service};

use super::project::{ProjectKey, WorktreeKey};
use super::session::SessionKey;

mod meter;
mod mr;
mod snapshot;

#[cfg(test)]
mod tests;

pub use meter::{AnalysisId, AnalysisStart, AnalysisTarget, MeterEvent, MeterState};
pub use mr::{
    ForgeRepo, MrEventKind, MrInfo, MrRef, MrSource, MrSourceError, MrSourceRegistration,
};
pub use snapshot::{
    AllUsage, BranchLabel, DayTotals, HistoryUsage, LimitKind, LimitSource, LimitWindow, Limits,
    MrLabel, MrSessionRow, MrState, MrUsage, ProjectInfo, ProjectStatus, ProjectUsage, RowKey,
    SessionRow, SessionUsage, SnapshotHeader, TemporaryReason, UsageDegraded, UsageRow,
    UsageSnapshot, UsageState, UseId, WorktreeUsage,
};

/// 单实现服务，"usage" 插件提供。
pub trait Usage: Send + Sync {
    /// 关注一个查询：返回值持有期间实现方计算并保持它的快照，drop 后释放（任何线程，
    /// 不阻塞）。立即返回。
    fn watch(&self, query: UsageQuery) -> UsageInterest;

    /// 当前快照。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程可调用。没有关注者或还没有
    /// 算出时为 None。
    fn snapshot(&self, query: &UsageQuery) -> Option<Arc<UsageSnapshot>>;

    /// 额度快照。同上，不需要关注；数据库还没打开时是空的。
    fn limits(&self) -> Arc<Limits>;

    /// 项目合并与临时项目的确认。立即返回，任何线程；落盘与重算在后台，结果随
    /// `UsageEvent::Changed` 送达。
    fn command(&self, command: UsageCommand);

    /// Analyze 开始时登记被分析的对象（ADR 0019）：在这次调用里解析出它当时的归属
    /// （含已关闭的 MR）并落盘。阻塞（一次事务），只在 executor 上调用。
    fn begin_analysis(&self, start: AnalysisStart) -> Result<AnalysisId, UsageError>;

    /// 一个模型请求的计量变化。阻塞，事务提交之后才返回（数据库是
    /// `synchronous = FULL`）；凭据转发端点在 `Prepared` 返回 Ok 之后才转发请求。只在
    /// executor 上调用。
    fn meter(&self, event: MeterEvent) -> Result<(), UsageError>;

    /// 登记 MR 来源（usage 设计前置条件 F）。立即返回，任何线程。实现方每次调用提供者
    /// 之前 `token.enter()`，取不到（提供者已停用）就当作没有登记。返回值 drop 时注销。
    fn register_mr_source(
        &self,
        provider: Arc<dyn MrSource>,
        token: ActivationToken,
    ) -> MrSourceRegistration;
}

impl Service for dyn Usage {
    const ID: &'static str = "usage";
    const MULTIPLE: bool = false;
}

/// 在 executor 线程上、快照替换之后发出。监听只安排重绘，不做 IO。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UsageEvent {
    Changed { query: UsageQuery, generation: u64 },
    Limits { generation: u64 },
}

impl Event for UsageEvent {}

/// `watch` 的关注，drop 时注销（任何线程，不阻塞）。`usage` 用 `new` 构造，传入的回调
/// 持有关注表的 `Weak` 和关注序号；实现方已停用时回调不做任何事。回调最多调用一次，
/// 不得 panic。不序列化：进程插件的代理在宿主进程里持有它。
pub struct UsageInterest {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl UsageInterest {
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        UsageInterest {
            release: Some(Box::new(release)),
        }
    }
}

impl Drop for UsageInterest {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for UsageInterest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsageInterest").finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UsageQuery {
    /// 一个项目在时间窗口内的合计与明细：Usage 页「当前项目」；侧栏底部的 Usage 入口用
    /// `Window::D7`。
    Project { project: ProjectRef, window: Window },
    /// Usage 页「所有项目」。
    AllProjects { window: Window },
    /// 一个 worktree 当前分支的累计：并行 worktree 列表与检查器。
    Worktree {
        project: ProjectKey,
        worktree: WorktreeKey,
    },
    /// 一个会话（含子 agent）：改到的每个分支（P3 起为 MR）各一行，以及每轮 token：
    /// 审阅页的简报和轮次标题。
    Session { session: SessionKey },
    /// 一个 MR 的累计与按会话、按轮次的明细，「编写」与「审阅」分开（P3，MR 视图）。
    Mr { mr: MrRef },
    /// 超过热窗口的历史（usage 设计「冷热分层」）。现有视图都不用：`days` 为 false 时
    /// 只读一直热的按月汇总，为 true 时另外逐段解压这个范围里的冷数据段，给出按日的明细。
    History {
        project: ProjectRef,
        from: LocalDate,
        to: LocalDate,
        days: bool,
    },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ProjectRef {
    Open(ProjectKey),
    Id(UsageProjectId),
}

/// 最近 7 / 14 / 30 个本地自然日，含今天（ADR 0020）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Window {
    D7,
    D14,
    D30,
}

/// Lectern 给项目分配的固定标识：16 字节随机数，跨运行不变。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct UsageProjectId(pub [u8; 16]);

/// 一条别名记录（合并、确认），`UsageCommand::Undo` 用它撤销。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct AliasId(pub u64);

/// 本地日历日期。按年、月、日的顺序比较。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct LocalDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

/// 四类互不重叠的 token（ADR 0022）。缓存命中率由视图从这些类别算出。
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    /// 四者之和，超过 `u64::MAX` 时取 `u64::MAX`。
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UsageCommand {
    /// 「合并到…」：`from`（路径已不存在的项目）的用量并入 `into`。
    Merge {
        from: UsageProjectId,
        into: UsageProjectId,
    },
    /// 临时项目「是同一个项目」。
    ConfirmSame {
        temporary: UsageProjectId,
        into: UsageProjectId,
    },
    /// 临时项目「是新项目」。
    ConfirmNew { temporary: UsageProjectId },
    /// 撤销一次合并或确认，也追加为别名记录。
    Undo { alias: AliasId },
}

/// `begin_analysis` 与 `meter` 的错误。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UsageError {
    /// 数据库还没打开、只读或已失败。凭据转发端点收到它时不转发，Analyze 报错。
    Unavailable,
    Io(Arc<str>),
    UnknownAnalysis,
}
