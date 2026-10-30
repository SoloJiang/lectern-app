//! 审阅意见与它的发送途径（docs/design/review.md「接口」的审阅意见、「审阅意见的发送
//! 途径」，#22）。
//!
//! 会话途径（终端粘贴、ACP）要 `terminal`、`conversation` 的服务，`review` 不依赖
//! 它们，所以与 `BriefRules` 一样反过来登记：途径的插件 `requires` `dyn Review`，在
//! `started` 里调用 `Review::register_note_route`，把返回的 [`NoteRouteRegistration`]
//! 交给 `PluginHandle::own`。[`NoteRoute`] 不实现 `Service`。剪贴板途径由 `review`
//! 内置，不经登记。

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{DiffSide, FileKey, ReviewTarget, StepId, TurnRef};
use crate::project::{BlobId, RelPath};
use crate::session::Timestamp;

/// 一条审阅意见。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: NoteId,
    pub anchor: NoteAnchor,
    /// 用户写的文字，原样，UTF-8，至多 `limits::NOTE_BYTES` 字节，不全是空白。
    pub text: Arc<str>,
    pub created_at: Timestamp,
    /// 最近一次改文字的时刻；没改过为 None。
    pub edited_at: Option<Timestamp>,
    pub state: NoteState,
    /// 每份快照重新算。
    pub place: NotePlace,
    /// 写下意见时锚点所在那一侧的内容里，锚点前后各 2 行。后台取出之前、取不到时为
    /// None。
    pub excerpt: Option<Excerpt>,
}

/// 写下的时刻（Unix 毫秒）乘 1024 加进程内的序号（同一毫秒至多 1024 个），跨运行
/// 不重复；日志里写成 16 位十六进制。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct NoteId(pub u64);

/// 一批意见的 ID，构成与 [`NoteId`] 相同。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct BatchId(pub u64);

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct NoteAnchor {
    /// 写下意见时这个文件在快照里的标识；上下文面板里不在改动集里的文件是
    /// (路径, 路径)。
    pub file: FileKey,
    pub side: DiffSide,
    /// 这一侧内容里的行号，0 起（与 `DiffRow` 相同）。
    pub line: u32,
    /// 这一侧内容的 OID（`RowsSide::oid`；改动链里的中间版本按 git 的 blob 格式计算）。
    pub oid: BlobId,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum NoteState {
    Pending,
    Sending(BatchId),
    Sent(BatchId),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum NotePlace {
    /// 锚点的内容还在：这些展示行（最终改动或叙事里的一步）`anchor.side` 那一侧的
    /// OID 等于 `anchor.oid`。视图把意见画在 `anchor.line` 那一行之后。
    Rows(Arc<[NoteRows]>),
    /// 文件还在 `files` 里，但没有哪份展示行的那一侧是锚点的内容：代码在写下意见之后
    /// 变了（Drifted）。`file` 是文件现在的标识。
    Drifted { file: FileKey },
    /// 文件不在 `files` 里：不再有改动、属于别的审阅对象，或本来就在改动集之外
    /// （上下文面板里写下的意见）。
    Elsewhere,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum NoteRows {
    Final,
    Step(StepId),
}

/// `first` 是 `lines[0]` 在那一侧内容里的行号（0 起）。每行至多 160 字节，在字符
/// 边界截断，不含行尾。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Excerpt {
    pub first: u32,
    pub lines: Arc<[Arc<str>]>,
}

/// `Default` 是没有意见、没有途径的空状态；`review` 总放进剪贴板途径。
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct NotesState {
    /// 适用于这个审阅对象的发送途径：登记的会话途径按登记顺序，最后是剪贴板（总有）。
    /// 第一项是 ⌘↩ 的默认途径。
    pub routes: Arc<[RouteOffer]>,
    /// `notes` 里待发送的条数。
    pub pending: u32,
    /// `notes` 里还没发出的条数：待发送加发送中。`AddNote` 按它与
    /// `limits::PENDING_NOTES` 比较。
    pub unsent: u32,
    pub sending: Option<BatchId>,
    /// 上一次发送或再次复制的结果；下一次 `SendNotes`、`AddNote`、`DiscardNotes` 时
    /// 清除，`CopyBatch` 的结果到达时替换。
    pub last: Option<SendOutcome>,
    /// 这个审阅对象 14 天内发出的批次，从旧到新。
    pub batches: Arc<[SentBatch]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct RouteOffer {
    pub id: Arc<str>,
    pub kind: RouteKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RouteKind {
    Session,
    Clipboard,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SendOutcome {
    Sent {
        batch: BatchId,
        via: SentVia,
        count: u32,
    },
    Failed {
        route: RouteKind,
        reason: SendFailure,
    },
    /// `CopyBatch` 的结果：`count` 是这次复制的条数；`failed` 为 None 时已写进剪贴板。
    /// 意见的状态不随它变。
    CopiedAgain {
        batch: BatchId,
        count: u32,
        failed: Option<SendFailure>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SentVia {
    Clipboard,
    TerminalPaste,
    Acp,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SendFailure {
    /// 途径答复现在送不了。
    NotNow(RouteBlock),
    /// 10 秒没有答复。
    TimedOut,
    /// 途径失败（剪贴板拒绝写入、终端已退出等），或途径在发送期间注销。
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SentBatch {
    pub id: BatchId,
    pub at: Timestamp,
    pub via: SentVia,
    pub count: u32,
    /// 回应这批意见的那一轮（只在会话审阅里找）；还没找到时为 None。
    pub answered_by: Option<TurnRef>,
}

pub mod limits {
    /// 一条意见的文字上限（字节）。
    pub const NOTE_BYTES: usize = 4096;
    /// 一个审阅对象还没发出的意见（待发送加发送中，`NotesState::unsent`）的上限，
    /// 也是一批的上限。
    pub const PENDING_NOTES: u32 = 40;
}

/// 审阅意见的会话途径。不实现 `Service`。
pub trait NoteRoute: Send + Sync {
    /// 稳定 ID（登记它的插件 ID），即 `RouteOffer::id` 和 `SendNotes::route`。不阻塞。
    fn id(&self) -> Arc<str>;

    /// 这个途径是否适用于一个审阅对象。只看参数，不阻塞、不做 IO；`review` 在打开审阅
    /// 和登记变化时调用，结果进 `NotesState::routes`。会话途径只对
    /// `ReviewTarget::Session` 返回 true。现在能不能送到，在 `deliver` 里答复。
    fn applies(&self, target: &ReviewTarget) -> bool;

    /// 送出一批。立即返回，任何线程（`review` 在 `Background` 任务里调用）。结果经
    /// `reply` 交回一次。
    fn deliver(&self, delivery: NoteDelivery, reply: DeliveryReply);
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct NoteDelivery {
    pub batch: BatchId,
    pub target: ReviewTarget,
    /// 审阅对象的 worktree 根（绝对路径）；`locations` 的路径相对它。
    pub root: Arc<Path>,
    /// 拼好的 prompt（「审阅意见」的模板），至多 256 KiB。
    pub text: Arc<str>,
    pub count: u32,
    /// 前 8 条意见的位置：相对路径和 1 起的行号（锚点不在当前内容上时为 None）。ACP
    /// 途径用它填 `PromptOrigin::ReviewNotes`。
    pub locations: Arc<[(RelPath, Option<u32>)]>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum DeliveryOutcome {
    Delivered(SentVia),
    NotNow(RouteBlock),
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RouteBlock {
    /// 驱动这个会话的终端还在启动：`Starting` 期间发给程序的输入被丢弃。
    Starting,
    /// agent 正在工作，现在不能发 prompt。
    Busy,
    /// 没有能接收的会话（会话不能恢复、agent 不支持）。
    NoSession,
}

/// 途径交回结果的通道。`review` 用 `new` 构造，传入的回调只把结果放进队列、不阻塞。
/// `send` 在任何线程上调用一次，不阻塞；drop 而没有 `send` 时按
/// `DeliveryOutcome::Failed` 调用回调。不序列化：进程插件的代理在宿主进程里持有它。
pub struct DeliveryReply {
    send: Option<Box<dyn FnOnce(DeliveryOutcome) + Send>>,
}

impl DeliveryReply {
    pub fn new(send: impl FnOnce(DeliveryOutcome) + Send + 'static) -> Self {
        DeliveryReply {
            send: Some(Box::new(send)),
        }
    }

    /// 交回结果。
    pub fn send(mut self, outcome: DeliveryOutcome) {
        if let Some(send) = self.send.take() {
            send(outcome);
        }
    }
}

impl Drop for DeliveryReply {
    fn drop(&mut self) {
        if let Some(send) = self.send.take() {
            send(DeliveryOutcome::Failed);
        }
    }
}

impl fmt::Debug for DeliveryReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeliveryReply").finish_non_exhaustive()
    }
}

/// `register_note_route` 的登记，drop 时注销（任何线程，不阻塞）。`Send`，可以交给
/// `PluginHandle::own`。`review` 用 `new` 构造，传入的回调持有登记表的 `Weak` 和登记
/// 序号；`review` 已停用时回调不做任何事。回调最多调用一次，不得 panic。不序列化。
pub struct NoteRouteRegistration {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl NoteRouteRegistration {
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        NoteRouteRegistration {
            release: Some(Box::new(release)),
        }
    }
}

impl Drop for NoteRouteRegistration {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for NoteRouteRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoteRouteRegistration")
            .finish_non_exhaustive()
    }
}
