//! 会话来源服务：发现、增量读取与轮次索引
//! （docs/design/agents.md「接口」「数据结构」）。
//!
//! `agents` 实现 [`SessionSource`]（每种 agent 一个提供者）；消费方
//! （`usage`、`review`、`terminal`、`welcome`、`project_home`、`worktrees`）
//! 只依赖本模块。这里只有数据，没有行为，`Cursor` 的编码和
//! `SessionList` 的查找除外。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_kernel::{Event, Service};

mod list;
mod read;
mod record;
mod turns;

#[cfg(test)]
mod tests;

pub use list::{
    Agent, Diagnostics, Format, Parent, Place, SessionKey, SessionList, SessionSummary, SourceId,
    Timestamp,
};
pub use read::{Batch, Cursor, Positioned};
pub use record::{
    AdvisorUsage, RateLimitSample, RateWindow, RawTokens, Record, ToolCallId, ToolFinished,
    ToolKind, ToolStarted, ToolStatus, TurnId, UsageKey, UsageOrigin, UsageRecord,
};
pub use turns::{Content, ContentRef, EditOp, FileEdit, ToolCall, Turn, TurnIndex, TurnItem};

/// 多实现服务：每种 agent 一个提供者，ACP（P3）和第三方会话来源再各加一个。
pub trait SessionSource: Send + Sync {
    /// 这个提供者的稳定 ID，等于提供它的插件 ID（"session-claude-code"、
    /// "session-codex"）。全局唯一、跨版本不变，消费方把它和会话 ID 一起落盘。不阻塞。
    fn id(&self) -> SourceId;

    /// 会话属于哪个 agent 产品（界面的 agent 圆点、`usage` 的按 agent 拆分）。不阻塞。
    fn agent(&self) -> Agent;

    /// 当前会话列表。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程上可以调用。
    fn sessions(&self) -> Arc<SessionList>;

    /// 从 `from` 往后读，读到累计约 `max_bytes` 字节后在下一个完整记录之后停下。
    /// 阻塞（读文件、解压、解析），只在 executor 上调用。
    ///
    /// `max_bytes` 是软上限：游标之后还有完整的行时至少消费一行，所以返回时要么
    /// `at_end` 为 true，要么新游标严格大于 `from`。超过 16 MB 的行边读边丢弃，计入
    /// `Diagnostics::oversized_lines`，游标越过它。游标与文件对不上（被截断、被替换），
    /// 或游标里的来源 ID、适配器版本与当前提供者不同时，从头读，`Batch::reset` 为 true。
    fn read(
        &self,
        session: &SessionKey,
        from: &Cursor,
        max_bytes: u64,
    ) -> Result<Batch, SessionError>;

    /// 轮次索引。阻塞，只在 executor 上调用：内存命中时只复制 `Arc`；否则读 `cache`
    /// 里的索引、再解析之后追加的部分；都没有时解析整个文件。会话还在写入时，
    /// 返回截至当前末尾的索引。
    fn turns(&self, session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError>;

    /// 与 `turns` 相同，但结果不放进内存里的轮次索引 LRU（review 设计前置条件 G）：
    /// 已在 LRU 里、文件没变时照样复制 `Arc`（不算一次使用）；从 `cache` 读出或解析出的
    /// 索引照常写回 `cache`，返回后由调用方放下。LRU 里的条目不增不减。阻塞，只在
    /// executor 上调用。
    fn turns_unretained(&self, session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError>;

    /// 读一段正文（prompt、agent 说明、工具输入与结果、文件改动）。阻塞，只在
    /// executor 上调用；按偏移定位、读一行并解析，与文件大小无关。
    fn load(&self, session: &SessionKey, at: &ContentRef) -> Result<Content, SessionError>;

    /// 恢复这个会话用哪个已登记的 agent 命令。不阻塞。子 agent 会话和格式无法识别的
    /// 会话返回 None。只返回数据，不返回可执行的命令行。
    fn resume(&self, session: &SessionKey) -> Option<ResumeCommand>;

    /// 请求补齐这些会话的待定字段（`SessionSummary::tail_pending`）。立即返回，任何
    /// 线程可调用；补齐在后台进行，完成后随 `SessionsChanged` 送达。已补齐或不待定的
    /// 会话忽略。
    fn complete(&self, sessions: &[SessionKey]);
}

impl Service for dyn SessionSource {
    const ID: &'static str = "session-source";
    const MULTIPLE: bool = true;
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SessionError {
    /// 文件已删除。调用方从列表里去掉它；`usage` 已落盘的数据不受影响。
    NotFound,
    /// 读文件或解压失败。附原因，日志用。调用方在下一次 `SessionsChanged` 后重试。
    Io(Arc<str>),
    /// 整个文件认不出格式。`turns` 返回这个错误，`read` 返回空的 `Batch`。
    Unrecognized,
    /// 经进程插件实现的来源：这条正文超过进程插件的单条消息上限（4 MB）。附原始字节数。
    TooLarge(u64),
}

/// 恢复会话的命令只能是宿主登记的几种，来源只给出会话 ID，不给程序名和参数。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ResumeCommand {
    /// `terminal` 映射为 `claude --resume <session_id>`。
    ClaudeCode { session_id: Arc<str> },
    /// `terminal` 映射为 `codex resume <session_id>`。
    Codex { session_id: Arc<str> },
}

/// 在 executor 线程上、`SessionList` 替换之后发出。监听只更新状态或安排后台任务，
/// 不做 IO。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionsChanged {
    /// 发出这个事件的提供者；`changed`、`removed` 里的键的 `source` 都等于它。
    pub source: SourceId,
    pub generation: u64,
    /// 新增或有新内容的会话。
    pub changed: Arc<[SessionKey]>,
    pub removed: Arc<[SessionKey]>,
}

impl Event for SessionsChanged {}
