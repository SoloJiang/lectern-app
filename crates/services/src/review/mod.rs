//! 审阅服务：审阅快照、命令、展示行、项目主页的概览与简报规则的扩展点
//! （docs/design/review.md「接口」）。
//!
//! `review` 插件实现 [`Review`]；`review_view`、`project_home`、`worktrees` 和
//! 简报规则的提供者（[`BriefRules`]）只依赖本模块（架构规则 7）。这里只有数据，
//! 没有行为，`BriefReply`、`BriefRulesRegistration`、`DeliveryReply`、
//! `NoteRouteRegistration` 与 `OverviewInterest` 的回调除外。

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_kernel::{ActivationToken, Event, Service};

use super::changes::BaseSpec;
use super::project::{ProjectKey, WorktreeKey};
use super::session::SessionKey;

mod brief;
mod notes;
mod overview;
mod rows;
mod rules;
mod snapshot;
mod worktrees;

#[cfg(test)]
mod tests;

pub use brief::{
    AnalysisState, Brief, BriefItem, BriefRule, DiffSide, Group, GroupKind, IncompleteKind,
    LineAnchor, NoiseKind, SensitiveKind,
};
pub use notes::{
    BatchId, DeliveryOutcome, DeliveryReply, Excerpt, Note, NoteAnchor, NoteDelivery, NoteId,
    NotePlace, NoteRoute, NoteRouteRegistration, NoteRows, NoteState, NotesState, RouteBlock,
    RouteKind, RouteOffer, SendFailure, SendOutcome, SentBatch, SentVia, limits,
};
pub use overview::{OutsideCount, Overview, SessionOverview};
pub use rows::{DiffRow, FileRows, HunkAttribution, HunkInfo, InlineSpans, RowsKey, RowsSide};
pub use rules::{
    BriefFile, BriefReply, BriefRequest, BriefRules, BriefRulesRegistration, ProvidedItem,
};
pub use snapshot::{
    Alignment, AttributionState, BaseHow, BaseInfo, CallRef, Degraded, FileAttribution, FileEntry,
    FileKey, LoadState, NarrativeItem, Progress, ReviewError, ReviewSnapshot, SeenState,
    SessionRef, StepEntry, StepId, Survival, TextKey, TurnEntry, TurnRef,
};
pub use worktrees::{ChangeCount, Overlap, WorktreeChanges, WorktreesOverview};

/// 单实现服务，"review" 插件提供。
pub trait Review: Send + Sync {
    /// 打开一个审阅。立即返回，任何线程可调用。同一审阅对象已经打开时返回同一个 ID，
    /// 引用数加一。第一份快照是 `LoadState::Loading`，之后随 `ReviewEvent::Changed`
    /// 更新。
    fn open(&self, target: ReviewTarget) -> ReviewId;

    /// 当前快照。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程可调用。已关闭的 ID
    /// 返回 None。
    fn snapshot(&self, id: ReviewId) -> Option<Arc<ReviewSnapshot>>;

    /// 发一个命令。立即返回，任何线程可调用。改已看状态和审阅意见的命令在返回之前已
    /// 换出新快照（只改内存），写标注日志在后台进行；其余命令的结果随事件送达
    /// （`SendNotes` 先在返回前把意见标为发送中，结果随事件送达；`CopyBatch` 不改快照，
    /// 结果随事件送达）。
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

    /// 并行 worktree 总览（worktrees 设计「对其他 crate 的要求」A）。不阻塞，取锁复制
    /// `Arc`；没有关注者、也没有保留的结果时为 None。
    fn worktrees(&self, project: &ProjectKey) -> Option<Arc<WorktreesOverview>> {
        let _ = project;
        None
    }

    /// 关注一个项目全部 worktree 的改动计数。立即返回；持有期间实现方计算并保持它，
    /// drop 后停止计算。
    fn watch_worktrees(&self, project: &ProjectKey) -> OverviewInterest {
        let _ = project;
        OverviewInterest::new(|| {})
    }

    /// 用户要求刷新（⌘R）：有关注者时把这个项目的全部 worktree 标为失效，按节流规则
    /// 重算。立即返回。
    fn refresh_worktrees(&self, project: &ProjectKey) {
        let _ = project;
    }

    /// 登记一个简报规则的提供者。立即返回，任何线程。实现方每次调用提供者之前
    /// `token.enter()`，取不到（提供者已停用）就当作没有登记。返回值 drop 时注销。
    fn register_brief_rules(
        &self,
        provider: Arc<dyn BriefRules>,
        token: ActivationToken,
    ) -> BriefRulesRegistration;

    /// #22：登记一个审阅意见的会话途径。立即返回，任何线程。规则与
    /// `register_brief_rules` 相同：每次调用途径之前 `token.enter()`，取不到就当作没有
    /// 登记；返回值 drop 时注销。打开着的审阅随即换出新快照（`NotesState::routes`）。
    fn register_note_route(
        &self,
        route: Arc<dyn NoteRoute>,
        token: ActivationToken,
    ) -> NoteRouteRegistration;
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
    /// `Review::worktrees(project)` 换了一份新的。
    Worktrees {
        project: ProjectKey,
        generation: u64,
    },
}

impl Event for ReviewEvent {}

/// 一个打开的审阅，`Review::open` 给出。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ReviewId(pub u64);

/// `watch_overview` 与 `watch_worktrees` 的关注，drop 时注销（任何线程，不阻塞）。`review` 用 `new` 构造，
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
    /// #22：加一条待发送的意见。`rows` 是调用方画这一行用的展示行的键：实现方从它取
    /// 锚点前后的几行作摘录（`excerpt` 为 None 时）。调用方已经有这几行的文字时
    /// （上下文面板）直接给出 `excerpt`。
    AddNote {
        rows: Option<RowsKey>,
        anchor: NoteAnchor,
        text: Arc<str>,
        excerpt: Option<Excerpt>,
    },
    /// #22：改一条待发送的意见的文字。发送中、已发送的意见不能改。
    EditNote {
        note: NoteId,
        text: Arc<str>,
    },
    /// #22：删一条意见（待发送或已发送；发送中的不能删）。
    RemoveNote(NoteId),
    /// #22：删掉全部待发送的意见（视图先请用户确认）。
    DiscardNotes,
    /// #22：把待发送的意见作为一批（至多 `limits::PENDING_NOTES` 条），经 `route`
    /// （`NotesState::routes` 里一项的 `id`）送出。
    SendNotes {
        route: Arc<str>,
    },
    /// #22：按一批已发出的意见重新拼出 prompt（同一个批次 ID），经剪贴板途径再写一次。
    /// 不改意见的状态和 `batches`，不写日志，结果只放进 `NotesState::last`。
    CopyBatch(BatchId),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Need {
    Rows(RowsKey),
    Text(TextKey),
}
