//! ACP 的 JSON-RPC 层：stdio 上一行一条消息，只声明 Lectern 用到的六种消息和字段
//! （docs/design/agents.md「ACP 客户端」，ADR 0060）。
//!
//! 反序列化到只含用到字段的结构，不认识的字段忽略；`params` 和 `result` 先按 `RawValue` 取出，
//! 按方法再解析，所以一条已知种类的消息形状不对（`session/update` 里的一个字段类型不符）时只是
//! 这一条被跳过并计数，不让连接失败。整条消息不是合法的 JSON-RPC（不是 JSON、既没有 `method` 也没有
//! `id`）才算 [`Malformed`]，连接以 `Protocol` 关闭。不使用 `serde_json::Value`。

use std::borrow::Cow;

use lectern_services::conversation::{CallKind, CallState, OptionKind};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// 一行至多 16 MiB（不含换行）。
pub(crate) const MAX_LINE: usize = 16 << 20;
/// Lectern 只说 ACP v1（ADR 0060）。
pub(crate) const PROTOCOL_VERSION: u64 = 1;
/// ACP 的「需要认证」错误码。
pub(crate) const AUTH_REQUIRED: i64 = -32000;
/// JSON-RPC 的「方法不存在」。
const METHOD_NOT_FOUND: i64 = -32601;

/// 整条消息不是合法的 JSON-RPC：连接以 `Protocol` 关闭。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Malformed;

/// JSON-RPC 的 `id`：数字或字符串，原样保存，回复时原样带回。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Id {
    Num(i64),
    Str(String),
}

/// 方法名，只有固定表里的几种；日志只记这张表里的字面量（表外是 `other`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Method {
    Initialize,
    SessionResume,
    SessionLoad,
    SessionPrompt,
    SessionCancel,
    SessionUpdate,
    RequestPermission,
    Other,
}

impl Method {
    fn of(name: &str) -> Method {
        match name {
            "initialize" => Method::Initialize,
            "session/resume" => Method::SessionResume,
            "session/load" => Method::SessionLoad,
            "session/prompt" => Method::SessionPrompt,
            "session/cancel" => Method::SessionCancel,
            "session/update" => Method::SessionUpdate,
            "session/request_permission" => Method::RequestPermission,
            _ => Method::Other,
        }
    }

    /// 线上的名字，同时是日志里的 `method`。
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Method::Initialize => "initialize",
            Method::SessionResume => "session/resume",
            Method::SessionLoad => "session/load",
            Method::SessionPrompt => "session/prompt",
            Method::SessionCancel => "session/cancel",
            Method::SessionUpdate => "session/update",
            Method::RequestPermission => "session/request_permission",
            Method::Other => "other",
        }
    }
}

/// 回复里的错误。`message` 是 agent 写的：只给界面（`PromptFailed`），不进日志。
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
pub(crate) struct RpcError {
    pub(crate) code: i64,
    #[serde(default)]
    pub(crate) message: String,
}

/// 收到的一条消息。
pub(crate) enum Incoming<'a> {
    /// agent 发来的请求，要回复。
    Request {
        id: Id,
        method: Method,
        params: Option<&'a RawValue>,
    },
    Notification {
        method: Method,
        params: Option<&'a RawValue>,
    },
    /// 对我们的请求的回复。
    Response {
        id: Id,
        outcome: Result<Option<&'a RawValue>, RpcError>,
    },
}

#[derive(Deserialize)]
struct Envelope<'a> {
    id: Option<Id>,
    #[serde(borrow)]
    method: Option<Cow<'a, str>>,
    #[serde(borrow)]
    params: Option<&'a RawValue>,
    #[serde(borrow)]
    result: Option<&'a RawValue>,
    error: Option<RpcError>,
}

/// 解析一行（不含换行）。
pub(crate) fn parse(line: &[u8]) -> Result<Incoming<'_>, Malformed> {
    let envelope: Envelope<'_> = serde_json::from_slice(line).map_err(|_| Malformed)?;
    match (envelope.method, envelope.id) {
        (Some(method), Some(id)) => Ok(Incoming::Request {
            id,
            method: Method::of(&method),
            params: envelope.params,
        }),
        (Some(method), None) => Ok(Incoming::Notification {
            method: Method::of(&method),
            params: envelope.params,
        }),
        (None, Some(id)) => Ok(Incoming::Response {
            id,
            outcome: match envelope.error {
                Some(error) => Err(error),
                None => Ok(envelope.result),
            },
        }),
        (None, None) => Err(Malformed),
    }
}

// ---- 发出的消息 ----

#[derive(Serialize)]
struct Outgoing<'a, P: Serialize> {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a Id>,
    method: &'static str,
    params: P,
}

#[derive(Serialize)]
struct Reply<'a, R: Serialize> {
    jsonrpc: &'static str,
    id: &'a Id,
    #[serde(flatten)]
    body: R,
}

#[derive(Serialize)]
struct ResultBody<R: Serialize> {
    result: R,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorObject<'a>,
}

#[derive(Serialize)]
struct ErrorObject<'a> {
    code: i64,
    message: &'a str,
}

fn line<T: Serialize>(message: &T) -> Vec<u8> {
    // 结构都是字符串、数字和数组，序列化不会失败。
    let mut bytes = serde_json::to_vec(message).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

fn request<P: Serialize>(id: i64, method: Method, params: P) -> Vec<u8> {
    line(&Outgoing {
        jsonrpc: "2.0",
        id: Some(&Id::Num(id)),
        method: method.name(),
        params,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams {
    protocol_version: u64,
    client_capabilities: Empty,
    client_info: ClientInfo,
}

/// 空对象：不声明 `fs` 和 `terminal`（ADR 0050）。
#[derive(Serialize)]
struct Empty {}

#[derive(Serialize)]
struct ClientInfo {
    name: &'static str,
    version: &'static str,
}

/// `initialize`：协议版本 1，不声明任何客户端能力。
pub(crate) fn initialize(id: i64) -> Vec<u8> {
    request(
        id,
        Method::Initialize,
        InitializeParams {
            protocol_version: PROTOCOL_VERSION,
            client_capabilities: Empty {},
            client_info: ClientInfo {
                name: "lectern",
                version: env!("CARGO_PKG_VERSION"),
            },
        },
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionParams<'a> {
    session_id: &'a str,
    cwd: &'a str,
    mcp_servers: [Empty; 0],
}

/// `session/resume` 或 `session/load`（`load` 为 true）：`mcpServers` 为空。
pub(crate) fn resume(id: i64, load: bool, session_id: &str, cwd: &str) -> Vec<u8> {
    let method = if load {
        Method::SessionLoad
    } else {
        Method::SessionResume
    };
    request(
        id,
        method,
        SessionParams {
            session_id,
            cwd,
            mcp_servers: [],
        },
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptParams<'a> {
    session_id: &'a str,
    prompt: [TextBlock<'a>; 1],
}

#[derive(Serialize)]
struct TextBlock<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}

/// `session/prompt`：一段文字。
pub(crate) fn prompt(id: i64, session_id: &str, text: &str) -> Vec<u8> {
    request(
        id,
        Method::SessionPrompt,
        PromptParams {
            session_id,
            prompt: [TextBlock { kind: "text", text }],
        },
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelParams<'a> {
    session_id: &'a str,
}

/// `session/cancel`（通知）。
pub(crate) fn cancel(session_id: &str) -> Vec<u8> {
    line(&Outgoing {
        jsonrpc: "2.0",
        id: None,
        method: Method::SessionCancel.name(),
        params: CancelParams { session_id },
    })
}

#[derive(Serialize)]
struct Outcome<'a> {
    outcome: OutcomeBody<'a>,
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
enum OutcomeBody<'a> {
    Cancelled,
    Selected {
        #[serde(rename = "optionId")]
        option_id: &'a str,
    },
}

/// 对 `session/request_permission` 的回复：选了一个选项。
pub(crate) fn selected(id: &Id, option_id: &str) -> Vec<u8> {
    line(&Reply {
        jsonrpc: "2.0",
        id,
        body: ResultBody {
            result: Outcome {
                outcome: OutcomeBody::Selected { option_id },
            },
        },
    })
}

/// 对 `session/request_permission` 的回复：取消。
pub(crate) fn cancelled(id: &Id) -> Vec<u8> {
    line(&Reply {
        jsonrpc: "2.0",
        id,
        body: ResultBody {
            result: Outcome {
                outcome: OutcomeBody::Cancelled,
            },
        },
    })
}

/// 对不认识的请求（我们没有声明的 `fs/*`、`terminal/*` 等）回复「方法不存在」。
pub(crate) fn method_not_found(id: &Id) -> Vec<u8> {
    line(&Reply {
        jsonrpc: "2.0",
        id,
        body: ErrorBody {
            error: ErrorObject {
                code: METHOD_NOT_FOUND,
                message: "method not found",
            },
        },
    })
}

// ---- 收到的内容 ----

/// `initialize` 的回复。`protocolVersion` 必须有。
#[derive(Deserialize)]
pub(crate) struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub(crate) protocol_version: u64,
    #[serde(rename = "agentCapabilities", default)]
    capabilities: Capabilities,
}

#[derive(Default, Deserialize)]
struct Capabilities {
    #[serde(rename = "loadSession", default)]
    load_session: bool,
    #[serde(rename = "sessionCapabilities", default)]
    session: SessionCapabilities,
}

#[derive(Default, Deserialize)]
struct SessionCapabilities {
    resume: Option<IgnoredAny>,
}

/// agent 怎样恢复会话。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Restore {
    Resume,
    Load,
}

impl InitializeResult {
    /// 优先 `session/resume`，否则 `session/load`；两者都不支持时为 None。
    pub(crate) fn restore(&self) -> Option<Restore> {
        if self.capabilities.session.resume.is_some() {
            Some(Restore::Resume)
        } else if self.capabilities.load_session {
            Some(Restore::Load)
        } else {
            None
        }
    }
}

pub(crate) fn initialize_result(raw: Option<&RawValue>) -> Result<InitializeResult, Malformed> {
    let raw = raw.ok_or(Malformed)?;
    serde_json::from_str(raw.get()).map_err(|_| Malformed)
}

/// 一轮怎样结束（`session/prompt` 的 `stopReason`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Stop {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

pub(crate) fn stop_reason(raw: Option<&RawValue>) -> Result<Stop, Malformed> {
    #[derive(Deserialize)]
    struct Result_ {
        #[serde(rename = "stopReason")]
        stop_reason: String,
    }
    let raw = raw.ok_or(Malformed)?;
    let parsed: Result_ = serde_json::from_str(raw.get()).map_err(|_| Malformed)?;
    Ok(match parsed.stop_reason.as_str() {
        "max_tokens" => Stop::MaxTokens,
        "max_turn_requests" => Stop::MaxTurnRequests,
        "refusal" => Stop::Refusal,
        "cancelled" => Stop::Cancelled,
        // `end_turn`，以及以后新增的原因：按正常结束处理。
        _ => Stop::EndTurn,
    })
}

/// 一条 `session/update` 的内容。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Update {
    /// `agent_message_chunk` 的文字。
    AgentText(String),
    /// `tool_call`（`false`）或 `tool_call_update`（`true`）。
    Tool {
        info: ToolInfo,
        update: bool,
    },
    Plan(Vec<PlanItem>),
    /// `usage_update`。
    Usage {
        used: u64,
        size: u64,
    },
    /// 不认识的种类，或形状不对的已知种类：计数后跳过。
    Skipped,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct PlanItem {
    #[serde(default)]
    pub(crate) content: String,
    #[serde(default)]
    pub(crate) status: String,
}

/// 工具调用里 Lectern 用到的部分。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct ToolInfo {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) kind: Option<CallKind>,
    pub(crate) state: Option<CallState>,
    /// 涉及的文件（`locations`，没有时取 `content` 里的 diff 路径）。
    pub(crate) paths: Vec<String>,
    /// 原始输入里的命令与工作目录（各适配器的字段，设计「核对」）。
    pub(crate) command: Option<String>,
    pub(crate) cwd: Option<String>,
}

#[derive(Deserialize)]
struct UpdateParams<'a> {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(borrow)]
    update: &'a RawValue,
}

#[derive(Deserialize)]
struct UpdateKind<'a> {
    #[serde(rename = "sessionUpdate", borrow)]
    kind: Cow<'a, str>,
}

/// 解析 `session/update` 的 `params`：会话 ID 和更新。参数不是对象，或没有 `sessionId` 与 `update`
/// 时为 `Malformed`；更新本身解析不了时是 [`Update::Skipped`]。
pub(crate) fn session_update(params: Option<&RawValue>) -> Result<(String, Update), Malformed> {
    let params = params.ok_or(Malformed)?;
    let parsed: UpdateParams<'_> = serde_json::from_str(params.get()).map_err(|_| Malformed)?;
    let kind: UpdateKind<'_> = match serde_json::from_str(parsed.update.get()) {
        Ok(kind) => kind,
        Err(_) => return Ok((parsed.session_id, Update::Skipped)),
    };
    let raw = parsed.update.get();
    let update = match kind.kind.as_ref() {
        "agent_message_chunk" => agent_text(raw),
        "tool_call" => tool(raw, false),
        "tool_call_update" => tool(raw, true),
        "plan" => plan(raw),
        "usage_update" => usage(raw),
        _ => None,
    };
    Ok((parsed.session_id, update.unwrap_or(Update::Skipped)))
}

fn agent_text(raw: &str) -> Option<Update> {
    #[derive(Deserialize)]
    struct Chunk {
        content: Block,
    }
    #[derive(Deserialize)]
    struct Block {
        #[serde(rename = "type")]
        kind: String,
        text: Option<String>,
    }
    let chunk: Chunk = serde_json::from_str(raw).ok()?;
    match (chunk.content.kind.as_str(), chunk.content.text) {
        ("text", Some(text)) => Some(Update::AgentText(text)),
        _ => None,
    }
}

fn plan(raw: &str) -> Option<Update> {
    #[derive(Deserialize)]
    struct Plan {
        entries: Vec<PlanItem>,
    }
    let plan: Plan = serde_json::from_str(raw).ok()?;
    Some(Update::Plan(plan.entries))
}

fn usage(raw: &str) -> Option<Update> {
    #[derive(Deserialize)]
    struct Usage {
        used: u64,
        size: u64,
    }
    let usage: Usage = serde_json::from_str(raw).ok()?;
    Some(Update::Usage {
        used: usage.used,
        size: usage.size,
    })
}

fn tool(raw: &str, update: bool) -> Option<Update> {
    tool_info(raw).map(|info| Update::Tool { info, update })
}

/// 工具调用（`session/update` 的 `tool_call`、`tool_call_update`，权限请求的 `toolCall`）。
/// `toolCallId` 必须有；其余字段形状不对时当作没有。
pub(crate) fn tool_info(raw: &str) -> Option<ToolInfo> {
    #[derive(Deserialize)]
    struct Call<'a> {
        #[serde(rename = "toolCallId")]
        id: String,
        title: Option<String>,
        kind: Option<String>,
        status: Option<String>,
        #[serde(borrow)]
        locations: Option<&'a RawValue>,
        #[serde(borrow)]
        content: Option<&'a RawValue>,
        #[serde(rename = "rawInput", borrow)]
        raw_input: Option<&'a RawValue>,
    }
    #[derive(Deserialize)]
    struct Located {
        path: Option<String>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(rename = "type", default)]
        kind: String,
        path: Option<String>,
    }
    #[derive(Deserialize)]
    struct Input {
        command: Option<Command>,
        cwd: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Command {
        Text(String),
        Parts(Vec<String>),
    }
    fn lenient<T: serde::de::DeserializeOwned>(raw: Option<&RawValue>) -> Option<T> {
        serde_json::from_str(raw?.get()).ok()
    }

    let call: Call<'_> = serde_json::from_str(raw).ok()?;
    let mut paths: Vec<String> = lenient::<Vec<Located>>(call.locations)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|location| location.path)
        .collect();
    if paths.is_empty() {
        paths = lenient::<Vec<Item>>(call.content)
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.kind == "diff")
            .filter_map(|item| item.path)
            .collect();
    }
    let input: Option<Input> = lenient(call.raw_input);
    let (command, cwd) = match input {
        Some(input) => (
            input.command.map(|command| match command {
                Command::Text(text) => text,
                Command::Parts(parts) => parts.join(" "),
            }),
            input.cwd,
        ),
        None => (None, None),
    };
    Some(ToolInfo {
        id: call.id,
        title: call.title,
        kind: call.kind.as_deref().map(call_kind),
        state: call.status.as_deref().and_then(call_state),
        paths,
        command,
        cwd,
    })
}

fn call_kind(kind: &str) -> CallKind {
    match kind {
        "read" => CallKind::Read,
        "edit" => CallKind::Edit,
        "delete" => CallKind::Delete,
        "move" => CallKind::Move,
        "search" => CallKind::Search,
        "execute" => CallKind::Execute,
        "think" => CallKind::Think,
        "fetch" => CallKind::Fetch,
        // `switch_mode`, `other` and kinds added later.
        _ => CallKind::Other,
    }
}

fn call_state(status: &str) -> Option<CallState> {
    match status {
        "pending" => Some(CallState::Pending),
        "in_progress" => Some(CallState::Running),
        "completed" => Some(CallState::Completed),
        "failed" => Some(CallState::Failed),
        _ => None,
    }
}

/// `session/request_permission` 的参数。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PermissionAsk {
    pub(crate) session_id: String,
    pub(crate) tool: ToolInfo,
    /// 认识的种类的选项，按 agent 给的顺序。
    pub(crate) options: Vec<(String, OptionKind)>,
}

pub(crate) fn permission(params: Option<&RawValue>) -> Result<PermissionAsk, Malformed> {
    #[derive(Deserialize)]
    struct Params<'a> {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "toolCall", borrow)]
        tool_call: &'a RawValue,
        options: Vec<RawOption>,
    }
    #[derive(Deserialize)]
    struct RawOption {
        #[serde(rename = "optionId")]
        id: String,
        kind: String,
    }
    let params = params.ok_or(Malformed)?;
    let parsed: Params<'_> = serde_json::from_str(params.get()).map_err(|_| Malformed)?;
    let tool = tool_info(parsed.tool_call.get()).ok_or(Malformed)?;
    let options = parsed
        .options
        .into_iter()
        .filter_map(|option| {
            let kind = match option.kind.as_str() {
                "allow_once" => OptionKind::AllowOnce,
                "allow_always" => OptionKind::AllowAlways,
                "reject_once" => OptionKind::RejectOnce,
                "reject_always" => OptionKind::RejectAlways,
                _ => return None,
            };
            Some((option.id, kind))
        })
        .collect();
    Ok(PermissionAsk {
        session_id: parsed.session_id,
        tool,
        options,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(text: &str) -> Box<RawValue> {
        RawValue::from_string(text.to_owned()).unwrap()
    }

    fn text(bytes: &[u8]) -> &str {
        std::str::from_utf8(bytes).unwrap().trim_end()
    }

    #[test]
    fn initialize_declares_version_one_and_no_capabilities() {
        let message = initialize(1);
        assert_eq!(
            text(&message),
            concat!(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"#,
                r#""clientCapabilities":{},"clientInfo":{"name":"lectern","version":""#,
                env!("CARGO_PKG_VERSION"),
                r#""}}}"#
            )
        );
        assert!(!text(&message).contains("fs"));
        assert!(!text(&message).contains("terminal"));
    }

    #[test]
    fn requests_carry_no_servers_and_one_text_block() {
        assert_eq!(
            text(&resume(2, false, "sid", "/work")),
            r#"{"jsonrpc":"2.0","id":2,"method":"session/resume","params":{"sessionId":"sid","cwd":"/work","mcpServers":[]}}"#
        );
        assert!(text(&resume(2, true, "sid", "/work")).contains(r#""method":"session/load""#));
        assert_eq!(
            text(&prompt(3, "sid", "hi \"there\"")),
            r#"{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"sid","prompt":[{"type":"text","text":"hi \"there\""}]}}"#
        );
        assert_eq!(
            text(&cancel("sid")),
            r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"sid"}}"#
        );
    }

    #[test]
    fn replies_echo_the_agents_id() {
        assert_eq!(
            text(&selected(&Id::Str("p-1".into()), "allow")),
            r#"{"jsonrpc":"2.0","id":"p-1","result":{"outcome":{"outcome":"selected","optionId":"allow"}}}"#
        );
        assert_eq!(
            text(&cancelled(&Id::Num(7))),
            r#"{"jsonrpc":"2.0","id":7,"result":{"outcome":{"outcome":"cancelled"}}}"#
        );
        assert!(text(&method_not_found(&Id::Num(9))).contains(r#""code":-32601"#));
    }

    #[test]
    fn incoming_messages_are_told_apart() {
        let request = parse(
            br#"{"jsonrpc":"2.0","id":"x","method":"session/request_permission","params":{}}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            Incoming::Request {
                id: Id::Str(_),
                method: Method::RequestPermission,
                ..
            }
        ));
        let notification =
            parse(br#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#).unwrap();
        assert!(matches!(
            notification,
            Incoming::Notification {
                method: Method::SessionUpdate,
                ..
            }
        ));
        let ok = parse(br#"{"jsonrpc":"2.0","id":4,"result":null}"#).unwrap();
        assert!(matches!(
            ok,
            Incoming::Response {
                id: Id::Num(4),
                outcome: Ok(None)
            }
        ));
        let error = parse(br#"{"jsonrpc":"2.0","id":4,"error":{"code":-32000,"message":"login"}}"#)
            .unwrap();
        let Incoming::Response {
            outcome: Err(error),
            ..
        } = error
        else {
            panic!("an error reply");
        };
        assert_eq!(error.code, AUTH_REQUIRED);
        // Unknown methods are `other`; the name is not kept.
        let other = parse(br#"{"id":1,"method":"fs/read_text_file"}"#).unwrap();
        assert!(matches!(
            other,
            Incoming::Request {
                method: Method::Other,
                ..
            }
        ));
    }

    #[test]
    fn malformed_lines_are_refused() {
        for bad in [
            &b"not json"[..],
            b"[]",
            b"{}",
            br#"{"id":null,"result":1}"#,
            br#"{"id":1.5,"result":1}"#,
            b"",
        ] {
            assert!(parse(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn initialize_results_pick_resume_before_load() {
        let both = initialize_result(Some(&raw(
            r#"{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"sessionCapabilities":{"resume":{}}}}"#,
        )))
        .unwrap();
        assert_eq!(both.protocol_version, 1);
        assert_eq!(both.restore(), Some(Restore::Resume));
        let load = initialize_result(Some(&raw(
            r#"{"protocolVersion":1,"agentCapabilities":{"loadSession":true},"agentInfo":{"name":"x"}}"#,
        )))
        .unwrap();
        assert_eq!(load.restore(), Some(Restore::Load));
        let neither = initialize_result(Some(&raw(r#"{"protocolVersion":2}"#))).unwrap();
        assert_eq!(neither.protocol_version, 2);
        assert_eq!(neither.restore(), None);
        assert!(initialize_result(Some(&raw(r#"{"agentCapabilities":{}}"#))).is_err());
        assert!(initialize_result(None).is_err());
    }

    #[test]
    fn stop_reasons_map_and_unknown_ones_end_the_turn() {
        for (reason, stop) in [
            ("end_turn", Stop::EndTurn),
            ("max_tokens", Stop::MaxTokens),
            ("max_turn_requests", Stop::MaxTurnRequests),
            ("refusal", Stop::Refusal),
            ("cancelled", Stop::Cancelled),
            ("something_new", Stop::EndTurn),
        ] {
            let result = raw(&format!(r#"{{"stopReason":"{reason}"}}"#));
            assert_eq!(stop_reason(Some(&result)), Ok(stop));
        }
        assert!(stop_reason(None).is_err());
    }

    fn update(body: &str) -> Update {
        let params = raw(&format!(r#"{{"sessionId":"sid","update":{body}}}"#));
        let (session, update) = session_update(Some(&params)).unwrap();
        assert_eq!(session, "sid");
        update
    }

    #[test]
    fn updates_of_the_kinds_we_use_are_typed() {
        assert_eq!(
            update(
                r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}"#
            ),
            Update::AgentText("hi".into())
        );
        assert_eq!(
            update(r#"{"sessionUpdate":"usage_update","used":10,"size":100,"cost":{"amount":1}}"#),
            Update::Usage {
                used: 10,
                size: 100
            }
        );
        let Update::Plan(items) = update(
            r#"{"sessionUpdate":"plan","entries":[{"content":"a","priority":"high","status":"in_progress"}]}"#,
        ) else {
            panic!("a plan");
        };
        assert_eq!(items[0].content, "a");
        assert_eq!(items[0].status, "in_progress");
        let Update::Tool { info, update } = update(
            r#"{"sessionUpdate":"tool_call","toolCallId":"t1","title":"Run it","kind":"execute","status":"in_progress","rawInput":{"command":"ls -l","cwd":"/work"}}"#,
        ) else {
            panic!("a tool call");
        };
        assert!(!update);
        assert_eq!(info.id, "t1");
        assert_eq!(info.kind, Some(CallKind::Execute));
        assert_eq!(info.state, Some(CallState::Running));
        assert_eq!(info.command.as_deref(), Some("ls -l"));
        assert_eq!(info.cwd.as_deref(), Some("/work"));
    }

    #[test]
    fn tool_calls_tolerate_odd_shapes() {
        // Paths from `locations`, else from the diffs in `content`.
        let info = tool_info(
            r#"{"toolCallId":"t","kind":"edit","locations":[{"path":"/a/b.rs","line":3}],"content":[{"type":"diff","path":"/x.rs","newText":"n"}]}"#,
        )
        .unwrap();
        assert_eq!(info.paths, ["/a/b.rs"]);
        let info = tool_info(
            r#"{"toolCallId":"t","kind":"edit","content":[{"type":"content","content":{}},{"type":"diff","path":"/x.rs"}]}"#,
        )
        .unwrap();
        assert_eq!(info.paths, ["/x.rs"]);
        // A command given as parts; a raw input that is not an object; an unknown kind and status.
        let info =
            tool_info(r#"{"toolCallId":"t","rawInput":{"command":["git","status"]}}"#).unwrap();
        assert_eq!(info.command.as_deref(), Some("git status"));
        let info = tool_info(
            r#"{"toolCallId":"t","rawInput":"text","locations":5,"kind":"brand_new","status":"strange"}"#,
        )
        .unwrap();
        assert_eq!(info.command, None);
        assert!(info.paths.is_empty());
        assert_eq!(info.kind, Some(CallKind::Other));
        assert_eq!(info.state, None);
        assert!(tool_info(r#"{"title":"no id"}"#).is_none());
    }

    #[test]
    fn unknown_or_odd_updates_are_skipped_not_errors() {
        for body in [
            r#"{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"x"}}"#,
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"x"}}"#,
            r#"{"sessionUpdate":"available_commands_update","availableCommands":[]}"#,
            r#"{"sessionUpdate":"brand_new_kind"}"#,
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"image","data":"x"}}"#,
            r#"{"sessionUpdate":"usage_update","used":"many","size":1}"#,
            r#"{"sessionUpdate":"plan","entries":7}"#,
            r#"{"no_kind":true}"#,
            r#"5"#,
        ] {
            assert_eq!(update(body), Update::Skipped, "{body}");
        }
        // Without a session ID or an update the message itself is malformed.
        assert!(session_update(Some(&raw(r#"{"update":{}}"#))).is_err());
        assert!(session_update(None).is_err());
    }

    #[test]
    fn permission_options_keep_known_kinds_in_order() {
        let params = raw(
            r#"{"sessionId":"sid","toolCall":{"toolCallId":"t1","title":"Run","kind":"execute","rawInput":{"command":"make"}},
                "options":[{"optionId":"a","name":"A","kind":"allow_once"},
                           {"optionId":"b","name":"B","kind":"allow_once"},
                           {"optionId":"c","name":"C","kind":"brand_new"},
                           {"optionId":"d","name":"D","kind":"reject_always"}]}"#,
        );
        let ask = permission(Some(&params)).unwrap();
        assert_eq!(ask.session_id, "sid");
        assert_eq!(ask.tool.command.as_deref(), Some("make"));
        assert_eq!(
            ask.options,
            [
                ("a".to_owned(), OptionKind::AllowOnce),
                ("b".to_owned(), OptionKind::AllowOnce),
                ("d".to_owned(), OptionKind::RejectAlways)
            ]
        );
        assert!(permission(Some(&raw(r#"{"sessionId":"s","options":[]}"#))).is_err());
    }

    /// Every message in the synthetic scripts parses as the shape its script says
    /// (`tests/fixtures/acp/`, ADR 0060's replacement for a conformance suite).
    #[test]
    fn fixture_messages_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            for (at, rule) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
                if rule.trim().is_empty() {
                    continue;
                }
                for message in fixture_messages(rule) {
                    let message = message.replace("$SESSION", "sid");
                    assert!(
                        parse(message.as_bytes()).is_ok(),
                        "{}:{}: {message}",
                        path.display(),
                        at + 1
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 10, "the scripts hold messages: {checked}");
    }

    /// The `send` and `ask` messages of one rule line, as lines of JSON-RPC.
    fn fixture_messages(rule: &str) -> Vec<String> {
        #[derive(Deserialize)]
        struct Rule<'a> {
            #[serde(rename = "do", borrow)]
            actions: Vec<&'a RawValue>,
        }
        #[derive(Deserialize)]
        struct Action<'a> {
            #[serde(borrow)]
            send: Option<&'a RawValue>,
        }
        let rule: Rule<'_> = serde_json::from_str(rule).expect("a rule line");
        rule.actions
            .into_iter()
            .filter_map(|action| {
                let action: Action<'_> = serde_json::from_str(action.get()).ok()?;
                action.send.map(|message| message.get().to_owned())
            })
            .collect()
    }
}
