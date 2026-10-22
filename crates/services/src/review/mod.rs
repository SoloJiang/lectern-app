//! 审阅服务：审阅快照、命令、展示行、项目主页的概览与简报规则的扩展点
//! （docs/design/review.md「接口」）。
//!
//! `review` 插件实现 [`Review`]；`review_view`、`project_home`、`worktrees` 和
//! 简报规则的提供者（[`BriefRules`]）只依赖本模块（架构规则 7）。这里只有数据，
//! 没有行为，`BriefReply`、`BriefRulesRegistration` 与 `OverviewInterest` 的回调
//! 除外。

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_kernel::{ActivationToken, Event, Service};

use super::changes::BaseSpec;
use super::project::{ProjectKey, WorktreeKey};
use super::session::SessionKey;

mod brief;
mod overview;
mod rows;
mod rules;
mod snapshot;

#[cfg(test)]
mod tests;

pub use brief::{
    AnalysisState, Brief, BriefItem, BriefRule, DiffSide, Group, GroupKind, IncompleteKind,
    LineAnchor, NoiseKind, SensitiveKind,
};
pub use overview::{OutsideCount, Overview, SessionOverview};
pub use rows::{DiffRow, FileRows, HunkAttribution, HunkInfo, InlineSpans, RowsKey, RowsSide};
pub use rules::{
    BriefFile, BriefReply, BriefRequest, BriefRules, BriefRulesRegistration, ProvidedItem,
};
pub use snapshot::{
    Alignment, AttributionState, BaseHow, BaseInfo, CallRef, Degraded, FileAttribution, FileEntry,
    FileKey, LoadState, NarrativeItem, Note, Progress, ReviewError, ReviewSnapshot, SeenState,
    SessionRef, StepEntry, StepId, Survival, TextKey, TurnEntry, TurnRef,
};

/// 单实现服务，"review" 插件提供。
pub trait Review: Send + Sync {
    /// 打开一个审阅。立即返回，任何线程可调用。同一审阅对象已经打开时返回同一个 ID，
    /// 引用数加一。第一份快照是 `LoadState::Loading`，之后随 `ReviewEvent::Changed`
    /// 更新。
    fn open(&self, target: ReviewTarget) -> ReviewId;

    /// 当前快照。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程可调用。已关闭的 ID
    /// 返回 None。
    fn snapshot(&self, id: ReviewId) -> Option<Arc<ReviewSnapshot>>;

    /// 发一个命令。立即返回，任何线程可调用。改已看状态的命令在返回之前已换出新快照
    /// （只改内存），写标注日志在后台进行；其余命令的结果随事件送达。
    fn command(&self, id: ReviewId, command: ReviewCommand);

    /// 准备好的展示行。不阻塞；还没有准备时为 None，调用方先发
    /// `ReviewCommand::Prepare`。
    fn rows(&self, id: ReviewId, key: &RowsKey) -> Option<Arc<FileRows>>;

    /// 准备好的正文（prompt、agent 说明）。不阻塞；规则同 `rows`。
    fn text(&self, id: ReviewId, key: &TextKey) -> Option<Arc<str>>;

    /// 引用数减一。归零后保留 30 秒（返回项目主页再进入不重算），之后释放快照、行与
    /// 正文、worktree 句柄和监听。
    fn close(&self, id: ReviewId);

    /// 项目主页的概览。不阻塞；没有关注者时为 None。
    fn overview(&self, project: &ProjectKey) -> Option<Arc<Overview>>;

    /// 关注一个项目的概览：返回值持有期间实现方计算并保持它（含 ADR 0035 的计数比较），
    /// drop 后释放。立即返回。
    fn watch_overview(&self, project: &ProjectKey) -> OverviewInterest;

    /// 登记一个简报规则的提供者。立即返回，任何线程。实现方每次调用提供者之前
    /// `token.enter()`，取不到（提供者已停用）就当作没有登记。返回值 drop 时注销。
    fn register_brief_rules(
        &self,
        provider: Arc<dyn BriefRules>,
        token: ActivationToken,
    ) -> BriefRulesRegistration;
}

impl Service for dyn Review {
    const ID: &'static str = "review";
    const MULTIPLE: bool = false;
}

/// 在 executor 线程上、快照替换之后发出。监听只更新状态或安排重绘，不做 IO。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ReviewEvent {
    Changed {
        id: ReviewId,
        generation: u64,
    },
    /// `Prepare` 请求的行或正文有一部分已就绪。
    Prepared {
        id: ReviewId,
        generation: u64,
    },
    Overview {
        project: ProjectKey,
        generation: u64,
    },
}

impl Event for ReviewEvent {}

/// 一个打开的审阅，`Review::open` 给出。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ReviewId(pub u64);

/// `watch_overview` 的关注，drop 时注销（任何线程，不阻塞）。`review` 用 `new` 构造，
/// 传入的回调持有关注表的 `Weak` 和关注序号；实现方已停用时回调不做任何事。回调最多
/// 调用一次，不得 panic。不序列化：进程插件的代理在宿主进程里持有它。
pub struct OverviewInterest {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl OverviewInterest {
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        OverviewInterest {
            release: Some(Box::new(release)),
        }
    }
}

impl Drop for OverviewInterest {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for OverviewInterest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OverviewInterest").finish_non_exhaustive()
    }
}

/// 一次审阅看的范围。已看标记按审阅对象分开记录。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ReviewTarget {
    /// 一个会话，含它在同一 worktree 里派出的子 agent 会话。
    Session(SessionKey),
    /// worktree 相对当前 HEAD 的改动里没有任何会话声明的部分（项目主页的 Outside Any
    /// Session）。
    Outside {
        project: ProjectKey,
        worktree: WorktreeKey,
    },
    /// worktree 相对一个基准的全部改动（Worktrees 检查器的「相对 main」）。
    Range {
        project: ProjectKey,
        worktree: WorktreeKey,
        base: BaseSpec,
    },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ReviewCommand {
    MarkSeen(FileKey),
    MarkUnseen(FileKey),
    /// 把当前快照 `files` 里的全部文件标为已看（会话审阅只含它声明的文件）。
    LooksGood,
    /// 视图即将显示的行与正文。替换这个审阅上一次的请求：不在新请求里、也不在保留范围
    /// 内的会被丢弃。
    Prepare(Arc<[Need]>),
    /// 用户要求重新比较（`RefreshHint::Full`）。
    Refresh,
    /// #66 实现之前，快照的 `brief.analysis` 是 `Unavailable`，这两个命令被忽略。
    Analyze,
    CancelAnalyze,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Need {
    Rows(RowsKey),
    Text(TextKey),
}
