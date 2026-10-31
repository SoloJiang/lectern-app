//! 对话快照及其数据类型（docs/design/conversation.md「服务接口」）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::super::session::{Agent, SessionKey};
use super::{PermissionRequest, PromptOrigin};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConversationSnapshot {
    pub session: SessionKey,
    /// 每次替换加一，与 `ConversationEvent::Changed::generation` 一致。
    pub generation: u64,
    pub agent: Agent,
    pub load: Load,
    pub link: Link,
    /// 回复框能不能发送；`Err` 时界面显示原因（「状态与异常」）。不含「终端在驱动这个会话」，
    /// 那一条由 `conversation` 判断（「一处驱动」）。
    pub can_prompt: Result<(), Unavailable>,
    /// 上下文窗口的占用：只取自 ACP 的 `usage_update`（`used`、`size`），没有连接或 agent
    /// 不发时为 None。不估算。
    pub context: Option<ContextUsage>,
    /// 历史窗口之前还有多少轮。
    pub earlier: u32,
    /// 按对话顺序。不变量见 conversation 设计「服务接口」：同一轮不出现两次；
    /// 正文合计不超过 1 MiB；只有 `link` 为 `Working` 时最后一项可以是 `streaming` 的
    /// `AgentText`；有待确认的请求时 `link` 是 `Working` 或 `Stopping`。
    pub entries: Arc<[Entry]>,
    /// agent 最近一次给出的计划（ACP `plan`，整份替换）；只在有连接时有。
    pub plan: Option<Arc<[PlanStep]>>,
    /// 待确认的权限请求，最早的在前。
    pub permissions: Arc<[PermissionRequest]>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Load {
    Loading,
    Ready,
    Failed(HistoryError),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum HistoryError {
    /// 会话文件已删除。
    NotFound,
    /// 会话格式无法识别（`SessionError::Unrecognized`）：没有轮次和说明。
    Unrecognized,
    /// 读文件失败。下一次 `SessionsChanged` 后重试。
    Io,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Link {
    /// 没有连接：只有历史。
    None,
    /// 进程已启动，正在 `initialize` 和恢复会话。
    Connecting,
    /// 已连接，没有进行中的 prompt。空闲 10 分钟后关闭，回到 `None`（ADR 0050）。
    Idle,
    /// 一轮正在进行（`session/prompt` 还没有返回）。
    Working,
    /// 已发出 `session/cancel`，等这一轮结束。
    Stopping,
    /// 上一次连接或上一轮异常结束。下一次 `Prompt` 或 `Dismiss` 清除。`AuthRequired`、
    /// `PromptFailed` 时连接可能仍在，按 `Idle` 计空闲时间，下一次 `Prompt` 直接用它。
    Failed(LinkError),
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum LinkError {
    /// 程序启动失败：执行失败，或发送时程序的查找还没完成、完成后没有找到。
    Spawn,
    /// 协商的协议版本不是 1，或 agent 不支持恢复这个会话。
    Unsupported,
    /// 恢复会话（`session/resume` 或 `session/load`）返回错误。
    LoadFailed,
    /// 收到无法解析的消息、超过 16 MiB 的行，或不合协议的回复。
    Protocol,
    /// 进程在连接期间退出。
    Exited { code: Option<i32> },
    /// agent 要求先登录（ACP 的认证错误）。Lectern 不调用 `authenticate`（ADR 0050）。
    AuthRequired,
    /// `session/prompt` 返回了其他错误。`message` 是 agent 写的，只取第一行、至多 1 KiB；
    /// 界面原样显示，不进日志。
    PromptFailed { message: Arc<str> },
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Unavailable {
    /// 子 agent 会话，或来源不是内置的（`SessionSource::resume` 为 None）。
    NotResumable,
    /// 会话的 `cwd` 不在已打开项目的 worktree 根或文件夹项目根之内。
    OutsideProject,
    /// 登录 shell 环境的 PATH 里找不到这个 agent 的 ACP 程序。
    NotInstalled(AcpProgram),
    /// 这个 agent 的适配器没有通过 agents 设计的核对，或上一次协商的协议版本不是 1。
    Unsupported,
    /// 已有 3 个连接，都不空闲（ADR 0050）。
    TooMany,
    /// 正在连接或这一轮还没有结束（`Connecting`、`Working`、`Stopping`）。
    Busy,
}

/// 固定映射的程序（ADR 0050）。界面只显示 `name()`。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum AcpProgram {
    ClaudeCode,
    Codex,
}

impl AcpProgram {
    /// 程序名，界面显示用。取自技术调研「Agent 原生面板」；`agents` 设计「对其他 crate 的要求」C
    /// 的核对如果得出别的名字，在这里改。
    pub fn name(&self) -> &'static str {
        match self {
            AcpProgram::ClaudeCode => "claude-agent-acp",
            AcpProgram::Codex => "codex-acp",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used: u64,
    pub size: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub key: EntryKey,
    pub body: EntryBody,
}

/// 项的稳定键。不透明，只由序号组成、不含内容：`[a-z0-9:_-]`，至多 64 字节，可以直接作
/// 探针节点的业务键。历史的是 `h:<轮次序号>:<项序号>`，实时的是 `l:<发出的序号>:<项序号>`。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct EntryKey(pub Arc<str>);

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum EntryBody {
    Prompt {
        text: Passage,
        origin: PromptOrigin,
    },
    /// `streaming` 为 true：这一段还在接收（这一轮的最后一段，`link` 为 `Working`）。
    AgentText {
        text: Passage,
        streaming: bool,
    },
    /// 不是人写的输入（后台任务通知、hook 注入的内容），对应 `TurnItem::Injected`。
    Injected {
        text: Passage,
    },
    Tool(ToolEntry),
    /// 一轮没有正常结束。
    Ended(EndReason),
}

/// 一段正文。超过 64 KiB 的截断到 64 KiB 以内最后一个完整的字符，`truncated` 为 true。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Passage {
    pub text: Arc<str>,
    pub truncated: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ToolEntry {
    pub kind: CallKind,
    pub state: CallState,
    /// 动词之后的一段（「Edit unit.test.ts」里的文件名）：`Edit`、`Read`、`Delete`、`Move`
    /// 是文件名（不含目录，至多 3 个，用「, 」连接），`Execute` 是命令的第一行，`Search` 是
    /// agent 写的标题，`Other` 是 agent 写的标题或工具名；都至多 160 个字符。
    pub summary: Arc<str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum CallKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Fetch,
    Think,
    SpawnAgent,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum CallState {
    Pending,
    Running,
    Completed,
    Failed,
    Denied,
    Cancelled,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum EndReason {
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PlanStep {
    pub text: Arc<str>,
    pub state: StepState,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum StepState {
    Pending,
    InProgress,
    Completed,
}
