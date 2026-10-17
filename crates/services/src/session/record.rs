//! 记录：上下文、轮次、工具调用、用量与额度（docs/design/agents.md「记录」）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::Timestamp;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Record {
    /// cwd、日志记录的分支或模型变化时给出一次；会话的第一条记录之前总有一条。
    Context {
        at: Timestamp,
        cwd: Arc<Path>,
        logged_branch: Option<Arc<str>>,
        model: Option<Arc<str>>,
    },
    TurnStarted {
        turn: TurnId,
        at: Timestamp,
    },
    /// 只有 Codex 有明确的轮次结束；Claude Code 的一轮在下一个 prompt 开始时结束。
    TurnEnded {
        turn: TurnId,
        at: Timestamp,
        aborted: bool,
    },
    ToolStarted(ToolStarted),
    ToolFinished(ToolFinished),
    Usage(UsageRecord),
    RateLimits(RateLimitSample),
}

/// Claude Code：prompt 记录的 `uuid`；Codex：`turn_id`。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct TurnId(pub Arc<str>);

/// Claude Code：`tool_use.id`；Codex：`call_id`。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ToolCallId(pub Arc<str>);

/// 工具调用的开始（ADR 0020）。`at` 是发出调用的那条记录的时间戳。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ToolStarted {
    pub call: ToolCallId,
    /// 没有 prompt 就开始的调用（极少见）为 None。
    pub turn: Option<TurnId>,
    pub at: Timestamp,
    /// 日志里的工具名，原样保留。
    pub name: Arc<str>,
    pub kind: ToolKind,
}

/// 工具调用的结果（ADR 0020）。`at` 是带结果的那条记录的时间戳；没有结果的调用不给出。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ToolFinished {
    pub call: ToolCallId,
    pub at: Timestamp,
    pub status: ToolStatus,
    /// 这次调用打开的交互式 shell 会话（Codex `exec_command` 返回的 `session_id`），
    /// 调用结束时 shell 仍在运行才有；其余为 None。
    pub exec_session: Option<Arc<str>>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ToolKind {
    /// 写文件、改文件、应用补丁（含新建、删除、移动）。路径是绝对路径。
    Edit {
        paths: Arc<[PathBuf]>,
    },
    /// 执行命令。`command` 是交给 shell 的完整文本；`workdir` 是工具参数里的工作目录，
    /// 没有时为调用时的 cwd。`exec_session` 是这次调用写入的交互式 shell 会话 ID
    /// （`write_stdin`），其余为 None；这类写入的 `workdir` 只是会话启动时的目录，
    /// `workdir_exact` 为 false。
    Execute {
        command: Arc<str>,
        workdir: Arc<Path>,
        exec_session: Option<Arc<str>>,
        workdir_exact: bool,
    },
    Read {
        paths: Arc<[PathBuf]>,
    },
    /// 搜索的目标目录或文件；工具没给路径时为调用时的 cwd。
    Search {
        paths: Arc<[PathBuf]>,
    },
    /// 派出子 agent。子会话通过 `SessionSummary::parent` 指回这次调用。
    SpawnAgent,
    /// 其余工具（网页、MCP、计划、等待后台任务等）。
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ToolStatus {
    Completed,
    Failed,
    Denied,
    Interrupted,
}

/// 原始用量（ADR 0014、ADR 0022）。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct UsageRecord {
    pub at: Timestamp,
    pub turn: Option<TurnId>,
    pub model: Option<Arc<str>>,
    pub tokens: RawTokens,
    pub key: UsageKey,
    /// Claude Code `usage.iterations[]` 里 `advisor_message` 的那一份，按自己的模型计，
    /// 不与 `tokens` 相加，不影响去重。
    pub advisor: Option<AdvisorUsage>,
    /// 只对 `CodexTokenCount` 有意义：这条之前这个线程已出现过 `token_usage_record`。
    /// 不进键。
    pub superseded: bool,
    /// Claude Code 的 `isSidechain`。
    pub sidechain: bool,
    /// 会话日志。Analyze 的计量记录（后续 issue）用另一个变体。
    pub origin: UsageOrigin,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UsageOrigin {
    SessionLog,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct AdvisorUsage {
    pub model: Arc<str>,
    pub tokens: RawTokens,
}

/// 日志里的原始数值，不规范化：两家的口径不同，规范化在 `usage` 做。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RawTokens {
    /// `message.usage`：`input_tokens` 不含缓存。
    Anthropic {
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        cache_write_5m: u64,
        cache_write_1h: u64,
    },
    /// Codex `TokenUsage`：`input` 含 `cached_input`，`output` 含 `reasoning_output`。
    OpenAi {
        input: u64,
        cached_input: u64,
        cache_write: u64,
        output: u64,
        reasoning_output: u64,
    },
}

/// 去重用的键，去重规则在 `usage`（ADR 0014）。只由上游的标识组成，不含 `SourceId`。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UsageKey {
    /// 同一响应的多行键相同，`usage` 保留 token 合计最大的一条。
    Claude {
        message_id: Arc<str>,
        request_id: Option<Arc<str>>,
    },
    /// `event_msg` / `token_count`：`tokens` 是 `last_token_usage`，`total` 是
    /// `total_token_usage`；键带上游线程 ID（`session_meta.id`）。
    CodexTokenCount {
        thread_id: Arc<str>,
        total: RawTokens,
    },
    /// 顶层 `token_usage_record`：每个完成的响应一条。
    CodexResponse {
        thread_id: Arc<str>,
        response_id: Arc<str>,
    },
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct RateLimitSample {
    pub at: Timestamp,
    pub limit_id: Option<Arc<str>>,
    pub plan_type: Option<Arc<str>>,
    pub primary: Option<RateWindow>,
    pub secondary: Option<RateWindow>,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct RateWindow {
    pub used_percent: f32,
    pub window_minutes: Option<u32>,
    /// `resets_at`（Unix 秒）；旧版本只有 `resets_in_seconds` 时按记录时间换算。
    pub resets_at: Option<Timestamp>,
}
