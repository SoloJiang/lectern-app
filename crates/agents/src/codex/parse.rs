//! 一行 Codex rollout 到 `Record` 与轮次索引（docs/design/agents.md「记录」「轮次索引」「外部格式」）。

use std::path::Path;
use std::sync::Arc;

use lectern_services::session::{
    ContentRef, Positioned, RateLimitSample, RateWindow, RawTokens, Record, Timestamp, ToolCall,
    ToolCallId, ToolFinished, ToolKind, ToolStarted, ToolStatus, TurnId, TurnItem, UsageKey,
    UsageOrigin, UsageRecord,
};
use serde_json::value::RawValue;

use super::SourceTurns;
use super::format::{
    Event, Item, Line, Output, Payload, RateLimits, Response, SessionMeta, ShellAction, Text,
    TokenInfo, TokenUsage, TokenUsageRecord, Window,
};
use super::tools;
use crate::codec::{Reader, Writer};
use crate::index::{IndexBuilder, preview};
use crate::stats::ParseStats;
use crate::time;

/// `ContentRef::part`：整行的文本（prompt、agent 说明、压缩摘要）。
pub(super) const PART_TEXT: u16 = u16::MAX;
/// 工具调用的输入（`function_call.arguments`、`custom_tool_call.input`、`local_shell_call.action`）。
pub(super) const PART_INPUT: u16 = 0;
/// 工具输出（`function_call_output` / `custom_tool_call_output` 的 `output`）。
pub(super) const PART_OUTPUT: u16 = 1;
/// 改动（`patch_apply_end.changes`、`FileChange.changes`）。
pub(super) const PART_CHANGES: u16 = 2;

/// 用到的顶层类型。
const USED: &[&str] = &[
    "session_meta",
    "turn_context",
    "response_item",
    "event_msg",
    "compacted",
    "token_usage_record",
    "inter_agent_communication_metadata",
];
/// 认识、但不需要的顶层类型（「已知忽略」）。
const KNOWN_IGNORED: &[&str] = &[
    "world_state",
    "retained_context",
    "security_risk_score",
    "realtime_item",
    "inter_agent_communication",
];
/// 写入文件、但不需要的 `event_msg` 类型。
const KNOWN_EVENTS: &[&str] = &[
    "thread_goal_updated",
    "thread_rolled_back",
    "thread_settings_applied",
    "agent_reasoning",
    "agent_reasoning_raw_content",
    "agent_reasoning_section_break",
    "entered_review_mode",
    "exited_review_mode",
    "context_compacted",
    "mcp_tool_call_end",
    "web_search_end",
    "image_generation_end",
    "sub_agent_activity",
    "error",
    "warning",
    "stream_error",
    "turn_diff",
    "plan_update",
];
/// 写入文件、但不需要的 `response_item` 类型。`message` 里 `role: "user"` 的是环境说明和指令文件，不作为 prompt。
const KNOWN_RESPONSES: &[&str] = &[
    "message",
    "reasoning",
    "agent_message",
    "additional_tools",
    "tool_search_call",
    "tool_search_output",
    "web_search_call",
    "image_generation_call",
    "configuration_update",
    "compaction",
    "context_compaction",
    "compaction_trigger",
    "other",
];
/// 不需要的 `item_completed` 条目（`TurnItem` 的其余变体）。
const KNOWN_ITEMS: &[&str] = &[
    "FunctionCallOutput",
    "HookPrompt",
    "Plan",
    "Reasoning",
    "CommandExecution",
    "DynamicToolCall",
    "CollabAgentToolCall",
    "SubAgentActivity",
    "WebSearch",
    "ImageView",
    "Extension",
    "ImageGeneration",
    "EnteredReviewMode",
    "ExitedReviewMode",
    "McpToolCall",
    "ContextCompaction",
];

pub(super) fn is_known_ignored(kind: &str) -> bool {
    KNOWN_IGNORED.contains(&kind)
}

/// 游标里记下的小表最多这么多项（还没有结果的调用、交互式 shell 会话），超出时丢掉最早的。
const MAX_PENDING: usize = 64;

/// 继承前缀的边界（「外部格式」的「继承前缀」）。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) enum Boundary {
    /// 不是子 agent 或分叉会话。
    #[default]
    None,
    /// `ordinal` 小于它的行是继承前缀。
    Ordinal(u64),
    /// 按来源会话文件里全部 `task_started` 的 `turn_id` 判断；`in_prefix` 为 false 后都是自己的。
    Turns { source: Arc<str>, in_prefix: bool },
    /// 来源会话不在本机：全部记录按自己的计；来源之后出现时从头重读。
    Unknown { source: Arc<str> },
}

/// 一个还在等的 prompt：`task_started` 之前写出的 `user_message`。
#[derive(Clone, PartialEq, Debug)]
pub(super) struct Pending {
    pos: u64,
    len: u32,
    preview: Arc<str>,
}

/// 解析器状态，跨批保存在游标里。
#[derive(Clone, Default, PartialEq, Debug)]
pub(super) struct State {
    /// 见过会话头（首行 `session_meta`，带 `id` 和 `cwd`）。
    pub header: bool,
    /// `session_meta.id`：这个线程的 ID（`UsageKey` 用）。
    pub thread: Option<Arc<str>>,
    pub paginated: bool,
    pub boundary: Boundary,
    pub cwd: Option<Arc<Path>>,
    pub branch: Option<Arc<str>>,
    pub model: Option<Arc<str>>,
    /// 上一次给出的 `Record::Context`，以及它是不是在继承前缀里给出的。
    emitted: Option<Ctx>,
    emitted_inherited: bool,
    /// 当前轮次：自己的与继承前缀里的分开记。
    own_turn: Option<TurnId>,
    inherited_turn: Option<TurnId>,
    /// 自己的当前轮次还没有结束、已经有 prompt。
    own_open: bool,
    own_prompted: bool,
    pending_prompt: Option<Pending>,
    /// 最后一条带时间戳的记录（`SessionSummary::last_activity`）。
    pub last_at: Option<Timestamp>,
    /// 最后一条 `task_started` / `task_complete` / `turn_aborted` 是 `task_started`。
    pub open_turn: bool,
    /// 这个线程已出现过 `token_usage_record`（「只用一路用量」）。
    responses_seen: bool,
    /// 开始了、还没有结束的 `local_shell_call`。
    shells: Vec<Arc<str>>,
    /// 还没有结果的 `exec_command` 调用及其工作目录。
    exec_calls: Vec<(Arc<str>, Arc<Path>)>,
    /// 交互式 shell 会话 ID 及其启动目录。
    exec_sessions: Vec<(Arc<str>, Arc<Path>)>,
    /// 先于输出到达的补丁结果。
    patch_status: Vec<(Arc<str>, ToolStatus)>,
}

#[derive(Clone, PartialEq, Debug)]
struct Ctx {
    cwd: Arc<Path>,
    branch: Option<Arc<str>>,
    model: Option<Arc<str>>,
}

fn push_bounded<T>(v: &mut Vec<T>, item: T) {
    if v.len() >= MAX_PENDING {
        v.remove(0);
    }
    v.push(item);
}

fn status_code(s: ToolStatus) -> i64 {
    match s {
        ToolStatus::Completed => 0,
        ToolStatus::Failed => 1,
        ToolStatus::Denied => 2,
        ToolStatus::Interrupted => 3,
    }
}

fn status_of(code: i64) -> Option<ToolStatus> {
    Some(match code {
        0 => ToolStatus::Completed,
        1 => ToolStatus::Failed,
        2 => ToolStatus::Denied,
        3 => ToolStatus::Interrupted,
        _ => return None,
    })
}

fn len_i64(n: usize) -> Option<i64> {
    Some(i64::try_from(n).unwrap_or(i64::MAX))
}

impl State {
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.bool(self.header)
            .str(self.thread.as_deref())
            .bool(self.paginated);
        match &self.boundary {
            Boundary::None => w.i64(Some(0)),
            Boundary::Ordinal(n) => w
                .i64(Some(1))
                .i64(Some(i64::try_from(*n).unwrap_or(i64::MAX))),
            Boundary::Turns { source, in_prefix } => {
                w.i64(Some(2)).str(Some(source)).bool(*in_prefix)
            }
            Boundary::Unknown { source } => w.i64(Some(3)).str(Some(source)),
        };
        w.path(self.cwd.as_deref())
            .str(self.branch.as_deref())
            .str(self.model.as_deref())
            .bool(self.emitted_inherited)
            .bool(self.emitted.is_some());
        if let Some(c) = &self.emitted {
            w.path(Some(&c.cwd))
                .str(c.branch.as_deref())
                .str(c.model.as_deref());
        }
        w.str(self.own_turn.as_ref().map(|t| &*t.0))
            .str(self.inherited_turn.as_ref().map(|t| &*t.0))
            .bool(self.own_open)
            .bool(self.own_prompted)
            .bool(self.pending_prompt.is_some());
        if let Some(p) = &self.pending_prompt {
            w.i64(Some(i64::try_from(p.pos).unwrap_or(i64::MAX)))
                .i64(Some(i64::from(p.len)))
                .str(Some(&p.preview));
        }
        w.i64(self.last_at.map(|t| t.0))
            .bool(self.open_turn)
            .bool(self.responses_seen)
            .i64(len_i64(self.shells.len()));
        for s in &self.shells {
            w.str(Some(s));
        }
        for list in [&self.exec_calls, &self.exec_sessions] {
            w.i64(len_i64(list.len()));
            for (id, dir) in list {
                w.str(Some(id)).path(Some(dir));
            }
        }
        w.i64(len_i64(self.patch_status.len()));
        for (id, status) in &self.patch_status {
            w.str(Some(id)).i64(Some(status_code(*status)));
        }
        w.finish()
    }

    pub(super) fn decode(bytes: &[u8]) -> Option<State> {
        let mut r = Reader::new(bytes);
        let count = |r: &mut Reader<'_>| -> Option<usize> {
            usize::try_from(r.i64()??)
                .ok()
                .filter(|n| *n <= MAX_PENDING)
        };
        let header = r.bool()?;
        let thread = r.str()?;
        let paginated = r.bool()?;
        let boundary = match r.i64()?? {
            0 => Boundary::None,
            1 => Boundary::Ordinal(u64::try_from(r.i64()??).ok()?),
            2 => Boundary::Turns {
                source: r.str()??,
                in_prefix: r.bool()?,
            },
            3 => Boundary::Unknown { source: r.str()?? },
            _ => return None,
        };
        let cwd = r.path()?;
        let branch = r.str()?;
        let model = r.str()?;
        let emitted_inherited = r.bool()?;
        let emitted = if r.bool()? {
            Some(Ctx {
                cwd: r.path()??,
                branch: r.str()?,
                model: r.str()?,
            })
        } else {
            None
        };
        let own_turn = r.str()?.map(TurnId);
        let inherited_turn = r.str()?.map(TurnId);
        let own_open = r.bool()?;
        let own_prompted = r.bool()?;
        let pending_prompt = if r.bool()? {
            Some(Pending {
                pos: u64::try_from(r.i64()??).ok()?,
                len: u32::try_from(r.i64()??).ok()?,
                preview: r.str()??,
            })
        } else {
            None
        };
        let last_at = r.i64()?.map(Timestamp);
        let open_turn = r.bool()?;
        let responses_seen = r.bool()?;
        let mut shells = Vec::new();
        for _ in 0..count(&mut r)? {
            shells.push(r.str()??);
        }
        let mut pairs = || -> Option<Vec<(Arc<str>, Arc<Path>)>> {
            let mut out = Vec::new();
            for _ in 0..count(&mut r)? {
                out.push((r.str()??, r.path()??));
            }
            Some(out)
        };
        let exec_calls = pairs()?;
        let exec_sessions = pairs()?;
        let mut patch_status = Vec::new();
        for _ in 0..count(&mut r)? {
            patch_status.push((r.str()??, status_of(r.i64()??)?));
        }
        r.end()?;
        Some(State {
            header,
            thread,
            paginated,
            boundary,
            cwd,
            branch,
            model,
            emitted,
            emitted_inherited,
            own_turn,
            inherited_turn,
            own_open,
            own_prompted,
            pending_prompt,
            last_at,
            open_turn,
            responses_seen,
            shells,
            exec_calls,
            exec_sessions,
            patch_status,
        })
    }

    /// 还在继承前缀里、需要来源会话的轮次 ID 集合才能判断边界。
    pub(super) fn needs_source_turns(&self) -> Option<&str> {
        match &self.boundary {
            Boundary::Turns {
                source,
                in_prefix: true,
            } => Some(source),
            _ => None,
        }
    }
}

/// `TokenUsage` 到原始数值；负数当作 0。
fn tokens(u: &TokenUsage) -> RawTokens {
    let n = |v: Option<i64>| u64::try_from(v.unwrap_or(0)).unwrap_or(0);
    RawTokens::OpenAi {
        input: n(u.input_tokens),
        cached_input: n(u.cached_input_tokens),
        cache_write: n(u.cache_write_input_tokens),
        output: n(u.output_tokens),
        reasoning_output: n(u.reasoning_output_tokens),
    }
}

fn window(w: &Window, at: Timestamp) -> Option<RateWindow> {
    let used = w.used_percent?;
    let resets_at = match (w.resets_at, w.resets_in_seconds) {
        (Some(secs), _) => Some(Timestamp(secs.saturating_mul(1000))),
        (None, Some(secs)) => Some(Timestamp(at.0.saturating_add(secs.saturating_mul(1000)))),
        (None, None) => None,
    };
    Some(RateWindow {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a percentage; f32 is what RateWindow stores"
        )]
        used_percent: used as f32,
        window_minutes: w.window_minutes.and_then(|m| u32::try_from(m).ok()),
        resets_at,
    })
}

/// 一次解析：从某个解析器状态起，逐行交出记录、更新索引。
pub(super) struct Parse<'s> {
    pub state: State,
    pub records: Vec<Positioned<Record>>,
    pub index: Option<&'s mut IndexBuilder>,
    pub stats: ParseStats,
    /// 按线程 ID 取来源会话的轮次 ID 集合（不在本机时为 None）。
    pub lineage: &'s dyn Fn(&str) -> Option<Arc<SourceTurns>>,
    /// 当前边界用到的来源集合。
    pub source_turns: Option<Arc<SourceTurns>>,
    /// 以下只供摘要：第一个 prompt（不论是否继承）的预览、会话头的其余字段、任何一条记录里的 cwd。
    pub first_prompt: Option<Arc<str>>,
    pub meta: Option<Meta>,
    pub any_cwd: Option<Arc<Path>>,
}

/// 会话头里摘要要用、解析器状态里没有的字段。
#[derive(Clone, Debug)]
pub(super) struct Meta {
    /// 会话开始时的 cwd。
    pub cwd: Arc<Path>,
    pub cli_version: Option<Arc<str>>,
    pub started_at: Option<Timestamp>,
    /// 子 agent 会话：`source.subagent` 或 `parent_thread_id`。
    pub subagent: bool,
    pub parent_thread: Option<Arc<str>>,
}

impl Parse<'_> {
    fn bad(&mut self) {
        self.stats.bad_lines += 1;
    }

    fn side_turn(&self, inherited: bool) -> Option<TurnId> {
        if inherited {
            self.state.inherited_turn.clone()
        } else {
            self.state.own_turn.clone()
        }
    }

    /// 交出一条记录；上下文变了（或进入自己的部分）时先给出一条 `Context`。
    fn emit(&mut self, pos: u64, seq: &mut u16, inherited: bool, at: Timestamp, value: Record) {
        if let Some(cwd) = self.state.cwd.clone() {
            let ctx = Ctx {
                cwd,
                branch: self.state.branch.clone(),
                model: self.state.model.clone(),
            };
            if self.state.emitted.as_ref() != Some(&ctx)
                || (self.state.emitted_inherited && !inherited)
            {
                self.records.push(Positioned {
                    pos,
                    seq: *seq,
                    inherited,
                    value: Record::Context {
                        at,
                        cwd: ctx.cwd.clone(),
                        logged_branch: ctx.branch.clone(),
                        model: ctx.model.clone(),
                    },
                });
                *seq = seq.saturating_add(1);
                self.state.emitted = Some(ctx);
                self.state.emitted_inherited = inherited;
            }
        }
        self.records.push(Positioned {
            pos,
            seq: *seq,
            inherited,
            value,
        });
        *seq = seq.saturating_add(1);
    }

    /// 会话头：线程 ID、模式、cwd、分支和继承前缀的边界。
    fn header(&mut self, meta: &SessionMeta<'_>) -> bool {
        let (Some(id), Some(cwd)) = (meta.id.as_deref(), meta.cwd.as_deref()) else {
            return false;
        };
        let parent = meta
            .parent_thread_id
            .as_deref()
            .or(meta.source.parent_thread_id.as_deref());
        self.meta = Some(Meta {
            cwd: Path::new(cwd).into(),
            cli_version: meta.cli_version.as_deref().map(Arc::from),
            started_at: meta.timestamp.as_deref().and_then(time::parse),
            subagent: meta.source.subagent || meta.parent_thread_id.is_some(),
            parent_thread: parent.map(Arc::from),
        });
        let s = &mut self.state;
        s.header = true;
        s.thread = Some(id.into());
        s.paginated = meta.history_mode.as_deref() == Some("paginated");
        s.cwd = Some(Path::new(cwd).into());
        s.branch = meta
            .git
            .as_ref()
            .and_then(|g| g.branch.as_deref())
            .filter(|b| !b.is_empty())
            .map(Arc::from);
        let source = meta
            .forked_from_id
            .as_deref()
            .or(meta.parent_thread_id.as_deref())
            .or(meta.source.parent_thread_id.as_deref());
        s.boundary = match (meta.subagent_history_start_ordinal, source) {
            (Some(start), _) => Boundary::Ordinal(start),
            (None, Some(source)) => match (self.lineage)(source) {
                Some(turns) => {
                    self.source_turns = Some(turns);
                    Boundary::Turns {
                        source: source.into(),
                        in_prefix: true,
                    }
                }
                None => Boundary::Unknown {
                    source: source.into(),
                },
            },
            (None, None) => Boundary::None,
        };
        true
    }

    /// 这一行是不是继承前缀；按轮次 ID 判断时，遇到第一条自己的轮次（或触发轮次的 agent 间消息）就结束前缀。
    fn inherited(&mut self, line: &Line<'_>) -> bool {
        match &mut self.state.boundary {
            Boundary::None | Boundary::Unknown { .. } => false,
            Boundary::Ordinal(start) => line.ordinal.is_some_and(|o| o < *start),
            Boundary::Turns { in_prefix, .. } => {
                if !*in_prefix {
                    return false;
                }
                let own = match &line.payload {
                    Payload::Event(Event::TurnStarted {
                        turn_id: Some(turn),
                    }) => !self
                        .source_turns
                        .as_ref()
                        .is_some_and(|s| s.ids.contains(&**turn)),
                    Payload::InterAgentMetadata { trigger_turn } => *trigger_turn,
                    _ => false,
                };
                if own {
                    *in_prefix = false;
                }
                !own
            }
        }
    }

    /// 解析一行。`bytes` 不含换行。
    pub(super) fn line(&mut self, pos: u64, bytes: &[u8]) {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
            return self.bad();
        };
        let Some(kind) = line.kind.as_deref() else {
            return self.bad();
        };
        let at = line.timestamp.as_deref().and_then(time::parse);
        if self.any_cwd.is_none() {
            let cwd = match &line.payload {
                Payload::SessionMeta(m) => m.cwd.as_deref(),
                Payload::TurnContext(c) => c.cwd.as_deref(),
                _ => None,
            };
            self.any_cwd = cwd.map(|c| Path::new(c).into());
        }
        let used = USED.contains(&kind);
        if !used {
            if !is_known_ignored(kind) {
                self.stats.unknown_type(kind);
            }
            if at.is_some() {
                self.state.last_at = at;
            }
            return;
        }
        if let Payload::SessionMeta(meta) = &line.payload {
            // 第一条是会话头；分叉复制进来的来源 `session_meta` 不改变它。
            if !self.state.header && !self.header(meta) {
                return self.bad();
            }
            let at = at.or_else(|| meta.timestamp.as_deref().and_then(time::parse));
            if at.is_some() {
                self.state.last_at = at;
            }
            return;
        }
        if matches!(line.payload, Payload::Skipped) || !self.state.header {
            // 没有 `payload`，或者在会话头之前（没有 cwd 和线程，无法交出）。
            return self.bad();
        }
        let Some(at) = at else {
            return self.bad();
        };
        self.state.last_at = Some(at);
        let inherited = self.inherited(&line);
        let len = u32::try_from(bytes.len()).expect("lines are at most 16 MB");
        let mut seq = 0;
        match &line.payload {
            Payload::TurnContext(ctx) => {
                if let Some(cwd) = ctx.cwd.as_deref() {
                    self.state.cwd = Some(Path::new(cwd).into());
                }
                if let Some(model) = ctx.model.as_deref() {
                    self.state.model = Some(model.into());
                }
            }
            Payload::Event(event) => self.event(event, pos, len, &mut seq, at, inherited),
            Payload::Response(item) => self.response(item, pos, len, &mut seq, at, inherited),
            Payload::Compacted => {
                if !inherited && let Some(index) = self.index.as_deref_mut() {
                    index.push(TurnItem::Injected(ContentRef::new(pos, len, PART_TEXT)));
                }
            }
            Payload::TokenUsageRecord(record) => {
                self.response_usage(record, pos, &mut seq, at, inherited);
            }
            Payload::InterAgentMetadata { .. } => {}
            Payload::SessionMeta(_) | Payload::Skipped => unreachable!("handled above"),
        }
    }

    fn event(
        &mut self,
        event: &Event<'_>,
        pos: u64,
        len: u32,
        seq: &mut u16,
        at: Timestamp,
        inherited: bool,
    ) {
        match event {
            Event::TurnStarted { turn_id } => {
                let Some(id) = turn_id.as_deref() else {
                    return self.bad();
                };
                let turn = TurnId(id.into());
                self.state.open_turn = true;
                if inherited {
                    self.state.inherited_turn = Some(turn.clone());
                } else {
                    self.state.own_turn = Some(turn.clone());
                    self.state.own_open = true;
                    let pending = self.state.pending_prompt.take();
                    self.state.own_prompted = pending.is_some();
                    if let Some(index) = self.index.as_deref_mut() {
                        let (prompt, preview) = match pending {
                            Some(p) => (Some(ContentRef::new(p.pos, p.len, PART_TEXT)), p.preview),
                            None => (None, "".into()),
                        };
                        index.start_turn(turn.clone(), at, prompt, preview);
                    }
                }
                self.emit(pos, seq, inherited, at, Record::TurnStarted { turn, at });
            }
            Event::TurnEnded { turn_id, aborted } => {
                let Some(turn) = turn_id
                    .as_deref()
                    .map(|t| TurnId(t.into()))
                    .or_else(|| self.side_turn(inherited))
                else {
                    return self.bad();
                };
                self.state.open_turn = false;
                if !inherited && self.state.own_turn.as_ref() == Some(&turn) {
                    self.state.own_open = false;
                    if let Some(index) = self.index.as_deref_mut() {
                        index.close_turn(at);
                    }
                }
                self.emit(
                    pos,
                    seq,
                    inherited,
                    at,
                    Record::TurnEnded {
                        turn,
                        at,
                        aborted: *aborted,
                    },
                );
            }
            Event::UserMessage { message } if !self.state.paginated => {
                self.first_prompt
                    .get_or_insert_with(|| preview(&message.head));
                if !inherited {
                    self.prompt(message, pos, len);
                }
            }
            Event::AgentMessage { message } if !self.state.paginated => {
                self.agent_text(message, pos, len, inherited);
            }
            Event::UserMessage { .. } | Event::AgentMessage { .. } => {}
            Event::TokenCount { info, rate_limits } => {
                self.token_count(info.as_ref(), rate_limits.as_ref(), pos, seq, at, inherited);
            }
            Event::PatchApplyEnd {
                call_id,
                status,
                success,
            } => {
                let Some(call) = call_id.as_deref() else {
                    return self.bad();
                };
                self.changes(
                    call,
                    tools::patch_status(status.as_deref(), *success),
                    pos,
                    len,
                    inherited,
                );
            }
            Event::ItemCompleted { item } => match item {
                Item::UserMessage { text } if self.state.paginated => {
                    self.first_prompt.get_or_insert_with(|| preview(&text.head));
                    if !inherited {
                        self.prompt(text, pos, len);
                    }
                }
                Item::AgentMessage { text } if self.state.paginated => {
                    self.agent_text(text, pos, len, inherited);
                }
                Item::UserMessage { .. } | Item::AgentMessage { .. } => {}
                Item::FileChange { id, status } => {
                    let Some(call) = id.as_deref() else {
                        return self.bad();
                    };
                    self.changes(
                        call,
                        tools::patch_status(status.as_deref(), None),
                        pos,
                        len,
                        inherited,
                    );
                }
                Item::Other(kind) => {
                    if !KNOWN_ITEMS.contains(&&**kind) {
                        self.stats
                            .unknown_type(&format!("event_msg/item_completed/{kind}"));
                    }
                }
                Item::Untagged => self.bad(),
            },
            Event::Other(kind) => {
                if !KNOWN_EVENTS.contains(&&**kind) {
                    self.stats.unknown_type(&format!("event_msg/{kind}"));
                }
            }
            Event::Untagged => self.bad(),
        }
    }

    /// prompt：自己的当前轮次还没有 prompt 时是它的 prompt；轮次里已经有了时（轮次中途补充的输入）是 `Injected`；
    /// 还没有开始的轮次先记下，等 `task_started`。
    fn prompt(&mut self, text: &Text<'_>, pos: u64, len: u32) {
        let at = ContentRef::new(pos, len, PART_TEXT);
        if !self.state.own_open {
            self.state.pending_prompt = Some(Pending {
                pos,
                len,
                preview: preview(&text.head),
            });
            return;
        }
        if !self.state.own_prompted {
            self.state.own_prompted = true;
            if let Some(index) = self.index.as_deref_mut() {
                index.set_prompt(at, preview(&text.head));
            }
        } else if let Some(index) = self.index.as_deref_mut() {
            index.push(TurnItem::Injected(at));
        }
    }

    fn agent_text(&mut self, text: &Text<'_>, pos: u64, len: u32, inherited: bool) {
        if !inherited
            && !text.blank
            && let Some(index) = self.index.as_deref_mut()
        {
            index.push(TurnItem::AgentText(ContentRef::new(pos, len, PART_TEXT)));
        }
    }

    /// 补丁的结果（`patch_apply_end` / `FileChange`）：记下状态等输出到达时用，轮次索引里的结果改指向这一行。
    fn changes(
        &mut self,
        call: &str,
        status: Option<ToolStatus>,
        pos: u64,
        len: u32,
        inherited: bool,
    ) {
        if let Some(status) = status {
            push_bounded(&mut self.state.patch_status, (call.into(), status));
        }
        if !inherited && let Some(index) = self.index.as_deref_mut() {
            index.attach_result(
                &ToolCallId(call.into()),
                ContentRef::new(pos, len, PART_CHANGES),
            );
        }
    }

    fn token_count(
        &mut self,
        info: Option<&TokenInfo>,
        limits: Option<&RateLimits<'_>>,
        pos: u64,
        seq: &mut u16,
        at: Timestamp,
        inherited: bool,
    ) {
        if let Some(info) = info
            && let (Some(last), Some(total)) = (&info.last_token_usage, &info.total_token_usage)
            && let Some(thread) = self.state.thread.clone()
        {
            let record = UsageRecord {
                at,
                turn: self.side_turn(inherited),
                model: self.state.model.clone(),
                tokens: tokens(last),
                key: UsageKey::CodexTokenCount {
                    thread_id: thread,
                    total: tokens(total),
                },
                advisor: None,
                superseded: self.state.responses_seen,
                sidechain: false,
                origin: UsageOrigin::SessionLog,
            };
            self.emit(pos, seq, inherited, at, Record::Usage(record));
        }
        if let Some(l) = limits {
            let sample = RateLimitSample {
                at,
                limit_id: l.limit_id.as_deref().map(Arc::from),
                plan_type: l.plan_type.as_deref().map(Arc::from),
                primary: l.primary.as_ref().and_then(|w| window(w, at)),
                secondary: l.secondary.as_ref().and_then(|w| window(w, at)),
            };
            self.emit(pos, seq, inherited, at, Record::RateLimits(sample));
        }
    }

    fn response_usage(
        &mut self,
        r: &TokenUsageRecord<'_>,
        pos: u64,
        seq: &mut u16,
        at: Timestamp,
        inherited: bool,
    ) {
        let (Some(usage), Some(response)) = (&r.usage, r.response_id.as_deref()) else {
            return self.bad();
        };
        let Some(own) = self.state.thread.clone() else {
            return self.bad();
        };
        let thread: Arc<str> = r.thread_id.as_deref().map_or(own.clone(), Arc::from);
        let turn = r
            .turn_id
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(|t| TurnId(t.into()))
            .or_else(|| self.side_turn(inherited));
        let record = UsageRecord {
            at,
            turn,
            model: self.state.model.clone(),
            tokens: tokens(usage),
            key: UsageKey::CodexResponse {
                thread_id: thread.clone(),
                response_id: response.into(),
            },
            advisor: None,
            superseded: false,
            sidechain: false,
            origin: UsageOrigin::SessionLog,
        };
        self.emit(pos, seq, inherited, at, Record::Usage(record));
        if thread == own {
            self.state.responses_seen = true;
        }
    }

    fn response(
        &mut self,
        item: &Response<'_>,
        pos: u64,
        len: u32,
        seq: &mut u16,
        at: Timestamp,
        inherited: bool,
    ) {
        match item {
            Response::FunctionCall {
                name,
                arguments,
                call_id,
            } => self.call(
                name,
                *arguments,
                false,
                call_id.as_deref(),
                pos,
                len,
                seq,
                at,
                inherited,
            ),
            Response::CustomToolCall {
                name,
                input,
                call_id,
            } => self.call(
                name,
                *input,
                true,
                call_id.as_deref(),
                pos,
                len,
                seq,
                at,
                inherited,
            ),
            Response::LocalShellCall {
                call_id,
                status,
                action,
            } => self.local_shell(
                call_id.as_deref(),
                status.as_deref(),
                action.as_ref(),
                pos,
                len,
                seq,
                at,
                inherited,
            ),
            Response::Output { call_id, output } => {
                let Some(call) = call_id.as_deref() else {
                    return self.bad();
                };
                self.output(call, output, pos, len, seq, at, inherited);
            }
            Response::Other(kind) => {
                if !KNOWN_RESPONSES.contains(&&**kind) {
                    self.stats.unknown_type(&format!("response_item/{kind}"));
                }
            }
            Response::Untagged => self.bad(),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one call site per item type; the values are all per-line"
    )]
    fn call(
        &mut self,
        name: &str,
        raw: Option<&RawValue>,
        custom: bool,
        call_id: Option<&str>,
        pos: u64,
        len: u32,
        seq: &mut u16,
        at: Timestamp,
        inherited: bool,
    ) {
        let (Some(id), Some(cwd)) = (call_id, self.state.cwd.clone()) else {
            return self.bad();
        };
        let sessions = &self.state.exec_sessions;
        let classified = tools::kind(
            name,
            raw,
            custom,
            &cwd,
            |sid| {
                sessions
                    .iter()
                    .rev()
                    .find(|(s, _)| &**s == sid)
                    .map(|(_, d)| d.clone())
            },
            &mut self.stats,
        );
        let call = ToolCallId(id.into());
        if let Some(dir) = classified.exec_workdir {
            push_bounded(&mut self.state.exec_calls, (id.into(), dir));
        }
        self.started(
            call,
            name.into(),
            classified.kind,
            pos,
            len,
            PART_INPUT,
            seq,
            at,
            inherited,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "shared by every call item type; the values are all per-line"
    )]
    fn started(
        &mut self,
        call: ToolCallId,
        name: Arc<str>,
        kind: ToolKind,
        pos: u64,
        len: u32,
        part: u16,
        seq: &mut u16,
        at: Timestamp,
        inherited: bool,
    ) {
        if !inherited && let Some(index) = self.index.as_deref_mut() {
            index.push(TurnItem::Tool(ToolCall {
                id: call.clone(),
                name: name.clone(),
                kind: kind.clone(),
                started_at: at,
                finished: None,
                input: ContentRef::new(pos, len, part),
                result: None,
            }));
        }
        let turn = self.side_turn(inherited);
        self.emit(
            pos,
            seq,
            inherited,
            at,
            Record::ToolStarted(ToolStarted {
                call,
                turn,
                at,
                name,
                kind,
            }),
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one call site; the values are all per-line"
    )]
    fn local_shell(
        &mut self,
        call_id: Option<&str>,
        status: Option<&str>,
        action: Option<&ShellAction<'_>>,
        pos: u64,
        len: u32,
        seq: &mut u16,
        at: Timestamp,
        inherited: bool,
    ) {
        let (Some(id), Some(cwd)) = (call_id, self.state.cwd.clone()) else {
            return self.bad();
        };
        let call = ToolCallId(id.into());
        let started = self.state.shells.iter().any(|s| &**s == id);
        if !started {
            let kind = tools::local_shell(
                action
                    .and_then(|a| a.command.as_deref())
                    .unwrap_or_default(),
                action.and_then(|a| a.working_directory.as_deref()),
                &cwd,
            );
            self.started(
                call.clone(),
                "local_shell_call".into(),
                kind,
                pos,
                len,
                PART_INPUT,
                seq,
                at,
                inherited,
            );
        }
        let finished = match status {
            Some("completed") => Some(ToolStatus::Completed),
            Some("incomplete") => Some(ToolStatus::Interrupted),
            _ => None,
        };
        match finished {
            Some(status) => {
                self.state.shells.retain(|s| &**s != id);
                if !inherited && let Some(index) = self.index.as_deref_mut() {
                    index.finish_tool(&call, at, status, None);
                }
                self.emit(
                    pos,
                    seq,
                    inherited,
                    at,
                    Record::ToolFinished(ToolFinished {
                        call,
                        at,
                        status,
                        exec_session: None,
                    }),
                );
            }
            None if !started => push_bounded(&mut self.state.shells, id.into()),
            None => {}
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one call site; the values are all per-line"
    )]
    fn output(
        &mut self,
        call: &str,
        output: &Output<'_>,
        pos: u64,
        len: u32,
        seq: &mut u16,
        at: Timestamp,
        inherited: bool,
    ) {
        let outcome = tools::outcome(&output.text);
        let patched = self
            .state
            .patch_status
            .iter()
            .position(|(c, _)| &**c == call)
            .map(|i| self.state.patch_status.remove(i).1);
        let status = patched.unwrap_or_else(|| tools::status(&outcome, output.success));
        // 只有 `exec_command` 打开交互式 shell 会话：记下会话从哪个目录启动，供之后的 `write_stdin`。
        let exec = self
            .state
            .exec_calls
            .iter()
            .position(|(c, _)| &**c == call)
            .map(|i| self.state.exec_calls.remove(i).1);
        let exec_session = match (exec, outcome.exec_session) {
            (Some(dir), Some(session)) => {
                push_bounded(&mut self.state.exec_sessions, (session.clone(), dir));
                Some(session)
            }
            _ => None,
        };
        let id = ToolCallId(call.into());
        if !inherited && let Some(index) = self.index.as_deref_mut() {
            index.finish_tool(
                &id,
                at,
                status,
                Some(ContentRef::new(pos, len, PART_OUTPUT)),
            );
        }
        self.emit(
            pos,
            seq,
            inherited,
            at,
            Record::ToolFinished(ToolFinished {
                call: id,
                at,
                status,
                exec_session,
            }),
        );
    }
}
