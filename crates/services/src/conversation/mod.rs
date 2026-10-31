//! 对话服务：ACP 对话的快照、命令和事件（docs/design/conversation.md「服务接口」）。
//!
//! `agents` 的 "conversations" 插件实现 [`Conversations`]；`conversation`、`project_home`
//! 和 #22 的审阅意见发回只依赖本模块（架构规则 7）。这里只有数据，没有行为，
//! `AcpProgram::name` 与 `ConversationInterest` 的回调除外。

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_kernel::{Event, Service};

use super::session::SessionKey;

mod permission;
mod snapshot;

#[cfg(test)]
mod tests;

pub use permission::{
    EditPreview, OptionId, OptionKind, PermissionDetail, PermissionId, PermissionOption,
    PermissionRequest, PreviewRow, RowKind,
};
pub use snapshot::{
    AcpProgram, CallKind, CallState, ContextUsage, ConversationSnapshot, EndReason, Entry,
    EntryBody, EntryKey, HistoryError, Link, LinkError, Load, Passage, PlanStep, StepState,
    ToolEntry, Unavailable,
};

/// 单实现服务，`agents` 的 "conversations" 插件提供。
pub trait Conversations: Send + Sync {
    /// 关注一个会话的对话：返回值持有期间提供者为它维护快照（在 executor 上读历史窗口），
    /// drop 后释放。立即返回，任何线程，不阻塞。不启动 agent 进程。同一个会话可以有多个关注，
    /// 共用一份快照。
    fn watch(&self, session: &SessionKey) -> ConversationInterest;

    /// 当前快照。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程可调用。没有关注、也没有连接时
    /// 为 None；有连接时即使没有关注也保留（权限请求在检查器关着时仍要等待回答）。
    fn snapshot(&self, session: &SessionKey) -> Option<Arc<ConversationSnapshot>>;

    /// 发命令。立即返回，任何线程。`Prompt`、`Cancel`、`Answer`、`Disconnect` 在返回之前已在锁内
    /// 换出新快照（`link`、`permissions` 已是新值），调用方随后 `snapshot()` 读到的就是新状态；
    /// 进程的启动和消息的收发在后台进行，结果随 `ConversationEvent::Changed` 送达。当前状态下
    /// 不允许的命令（`can_prompt` 为 `Err` 时的 `Prompt`、已不在等待的请求的 `Answer`）忽略，
    /// 提供者记 debug。
    fn command(&self, session: &SessionKey, command: ConversationCommand);

    /// 有待确认的权限请求的会话，按最早的请求排序。取锁复制一个 `Arc`，不阻塞，UI 线程可调用。
    fn waiting(&self) -> Arc<[SessionKey]>;
}

impl Service for dyn Conversations {
    const ID: &'static str = "conversations";
    const MULTIPLE: bool = false;
}

/// 在提供者的线程（executor、读 agent 输出的专用线程，或调用 `command` 的线程）上发出，
/// 快照替换之后。监听只转发，不做 IO。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ConversationEvent {
    /// `session` 的快照换了新值；`generation` 与 `ConversationSnapshot::generation` 一致。
    Changed {
        session: SessionKey,
        generation: u64,
    },
    /// `waiting()` 的集合变了。
    WaitingChanged,
}

impl Event for ConversationEvent {}

/// `watch` 的关注，drop 时注销（任何线程，不阻塞）。`agents` 用 `new` 构造，传入的回调持有
/// 关注表的 `Weak`；实现方已停用时回调不做任何事。回调最多调用一次，不得 panic。不序列化：
/// 进程插件的代理在宿主进程里持有它。
pub struct ConversationInterest {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl ConversationInterest {
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        ConversationInterest {
            release: Some(Box::new(release)),
        }
    }
}

impl Drop for ConversationInterest {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for ConversationInterest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConversationInterest")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ConversationCommand {
    /// 发一条消息。没有连接时这是启动 agent 进程的唯一入口（ADR 0050）。`text` 不超过
    /// 256 KiB，超过时忽略。
    Prompt {
        text: Arc<str>,
        origin: PromptOrigin,
    },
    /// `session/cancel`；待确认的请求按协议回答 `cancelled`。`link` 先变为 `Stopping`，
    /// 这一轮结束后为 `Idle`。
    Cancel,
    /// 回答一个权限请求。`option` 必须是这个请求的 `options` 之一。
    Answer {
        request: PermissionId,
        option: OptionId,
    },
    /// 历史窗口向前多读 20 轮（最多 200 轮）。
    LoadEarlier,
    /// 关闭连接（Terminal 页恢复之前）：`Idle` 或连接仍在的 `Failed` 时关闭，`link` 变为
    /// `None`；没有连接时只把 `Failed` 清为 `None`。`Connecting`、`Working`、`Stopping` 时忽略。
    Disconnect,
    /// 清除 `Link::Failed`：连接仍在时回到 `Idle`，否则回到 `None`。
    Dismiss,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PromptOrigin {
    /// 用户在回复框里写的，或历史里的 prompt。
    User,
    /// #22 发回的审阅意见：对话里显示为一行「n 条审阅意见 · 位置」，不显示拼好的 prompt 全文。
    ReviewNotes {
        count: u32,
        locations: Arc<[NoteLocation]>,
    },
}

/// 至多 8 个；`path` 是绝对路径，界面按会话的 cwd 显示相对路径。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NoteLocation {
    pub path: Arc<Path>,
    pub line: u32,
}
