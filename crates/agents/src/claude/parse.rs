//! 一行 Claude Code 记录到 `Record` 与轮次索引（docs/design/agents.md「记录」「轮次索引」「外部格式」）。

use std::path::Path;
use std::sync::Arc;

use lectern_services::session::{
    AdvisorUsage, ContentRef, Positioned, RawTokens, Record, Timestamp, ToolCall, ToolCallId,
    ToolFinished, ToolStarted, ToolStatus, TurnId, TurnItem, UsageKey, UsageOrigin, UsageRecord,
};

use super::Owner;
use super::format::{
    Block, CacheCreation, Iteration, Line, Message, StrOrSeq, TextHead, UseResult,
};
use super::tools;
use crate::codec::{Reader, Writer};
use crate::diag::{FirstSkip, Skip};
use crate::index::{IndexBuilder, preview};
use crate::stats::ParseStats;
use crate::time;

/// 认识、但不需要的记录类型（「已知忽略」表），不计入 `unknown_types`。
const KNOWN_IGNORED: &[&str] = &[
    "attachment",
    "system",
    "queue-operation",
    "last-prompt",
    "summary",
    "file-history-snapshot",
    "atis-latch",
    "frame-link",
    "pr-link",
    "mode",
    "cost-state",
];

pub(super) fn is_known_ignored(kind: &str) -> bool {
    KNOWN_IGNORED.contains(&kind)
}

/// `ContentRef::part` 为这个值时指整条消息的文本（字符串内容，或全部 `text` 块依次拼接）；
/// 其余值是 `message.content` 里内容块的下标。
pub(super) const PART_TEXT: u16 = u16::MAX;

/// 本地命令输出的包装：不开新轮次，也不让轮次保持进行中。
const LOCAL_COMMAND_WRAPPERS: &[&str] = &[
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<local-command-caveat>",
];
/// 用户中断后 Claude Code 写入的说明（`[Request interrupted by user]`、
/// `[Request interrupted by user for tool use]`）：不是 prompt。
const INTERRUPT_MARKER: &str = "[Request interrupted by user";
/// 工具被中断时 `tool_result` 的内容。
const TOOL_INTERRUPTED: &str = "[Request interrupted by user for tool use]";
/// 用户拒绝工具调用时 `tool_result` 的内容与 `toolUseResult` 的字符串。
const REJECTED_RESULT: &str = "The user doesn't want to proceed with this tool use";
const REJECTED_USE_RESULT: &str = "User rejected tool use";

/// 解析器状态，跨批保存在游标里。
#[derive(Clone, Default, PartialEq, Debug)]
pub(super) struct State {
    /// 见过会话头（带 `sessionId` 和 `cwd` 的记录）。
    pub header: bool,
    pub cwd: Option<Arc<Path>>,
    pub branch: Option<Arc<str>>,
    pub model: Option<Arc<str>>,
    /// 上一次给出的 `Record::Context`，以及它是不是在继承前缀里给出的。
    pub emitted: Option<Ctx>,
    pub emitted_inherited: bool,
    /// 当前轮次：自己的与继承前缀里的分开记，记录只引用与自己同属一边的轮次。
    pub own_turn: Option<TurnId>,
    pub inherited_turn: Option<TurnId>,
    /// 子 agent 文件里已经见过自己的第一条 `user` 记录（父 agent 给的任务）。
    pub first_user_seen: bool,
    /// 最后一条带时间戳的记录（`SessionSummary::last_activity`）。
    pub last_at: Option<Timestamp>,
    /// 按最后一条 `user` / `assistant` 记录判断的 `SessionSummary::open_turn`。
    pub open_turn: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub(super) struct Ctx {
    cwd: Arc<Path>,
    branch: Option<Arc<str>>,
    model: Option<Arc<str>>,
}

impl State {
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.bool(self.header)
            .path(self.cwd.as_deref())
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
            .bool(self.first_user_seen)
            .i64(self.last_at.map(|t| t.0))
            .bool(self.open_turn);
        w.finish()
    }

    pub(super) fn decode(bytes: &[u8]) -> Option<State> {
        let mut r = Reader::new(bytes);
        let header = r.bool()?;
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
        let state = State {
            header,
            cwd,
            branch,
            model,
            emitted,
            emitted_inherited,
            own_turn: r.str()?.map(TurnId),
            inherited_turn: r.str()?.map(TurnId),
            first_user_seen: r.bool()?,
            last_at: r.i64()?.map(Timestamp),
            open_turn: r.bool()?,
        };
        r.end()?;
        Some(state)
    }
}

/// 一个 `user` 记录是哪一种。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum UserKind {
    /// 人写的 prompt，开始新的一轮。
    Prompt,
    /// 不是人写的输入（后台任务通知、其他 agent 的消息、hook 注入、摘要、本地命令输出）。
    Injected,
    /// 工具结果。
    Results,
}

/// 消息内容的文本开头：字符串内容，或第一个 `text` 块。
pub(super) fn text_head<'a>(content: Option<&'a StrOrSeq<TextHead<'_>, Block<'_>>>) -> &'a str {
    match content {
        Some(StrOrSeq::Str(t)) => &t.head,
        Some(StrOrSeq::Seq(blocks)) => blocks
            .iter()
            .find(|b| b.kind.as_deref() == Some("text"))
            .and_then(|b| b.text.as_ref())
            .map_or("", |t| &t.head),
        None => "",
    }
}

fn has_tool_result(content: Option<&StrOrSeq<TextHead<'_>, Block<'_>>>) -> bool {
    matches!(content, Some(StrOrSeq::Seq(blocks)) if blocks.iter().any(|b| b.kind.as_deref() == Some("tool_result")))
}

/// 轮次的划分（「外部格式」里的「轮次」）。`subagent_task` 为 true 时是子 agent 文件里自己的第一条 `user` 记录。
pub(super) fn user_kind(line: &Line<'_>, subagent_task: bool) -> UserKind {
    let content = line.message.as_ref().and_then(|m| m.content.as_ref());
    if has_tool_result(content) {
        return UserKind::Results;
    }
    if subagent_task {
        return UserKind::Prompt;
    }
    let head = text_head(content);
    let human = line
        .origin
        .as_ref()
        .and_then(|o| o.kind.as_deref())
        .is_none_or(|k| k == "human");
    if line.is_meta == Some(true)
        || line.is_compact_summary == Some(true)
        || !human
        || LOCAL_COMMAND_WRAPPERS.iter().any(|w| head.starts_with(w))
        || head.starts_with(INTERRUPT_MARKER)
    {
        UserKind::Injected
    } else {
        UserKind::Prompt
    }
}

/// 以这条 `user` 记录结尾时，最后一轮是否还在进行：工具结果和 prompt 在等 agent 回复；
/// `isMeta`、本地命令输出和中断说明之后 agent 不再回复。
pub(super) fn user_leaves_open(line: &Line<'_>) -> bool {
    let content = line.message.as_ref().and_then(|m| m.content.as_ref());
    if has_tool_result(content) {
        return true;
    }
    let head = text_head(content);
    !(line.is_meta == Some(true)
        || LOCAL_COMMAND_WRAPPERS.iter().any(|w| head.starts_with(w))
        || head.starts_with(INTERRUPT_MARKER))
}

/// 以这条 `assistant` 记录结尾时，最后一轮是否还在进行。
pub(super) fn assistant_leaves_open(message: &Message<'_>) -> bool {
    message.stop_reason.as_deref() != Some("end_turn")
}

fn status(block: &Block<'_>, use_result: Option<&UseResult<'_>>) -> ToolStatus {
    let text = match &block.content {
        Some(StrOrSeq::Str(t)) => &*t.head,
        Some(StrOrSeq::Seq(parts)) => parts
            .iter()
            .find(|p| p.kind.as_deref() == Some("text"))
            .and_then(|p| p.text.as_ref())
            .map_or("", |t| &t.head),
        None => "",
    };
    let interrupted = matches!(use_result, Some(UseResult::Object { interrupted: true }));
    let rejected =
        matches!(use_result, Some(UseResult::Text(t)) if t.head.starts_with(REJECTED_USE_RESULT));
    if interrupted || text.starts_with(TOOL_INTERRUPTED) {
        ToolStatus::Interrupted
    } else if rejected || text.starts_with(REJECTED_RESULT) {
        ToolStatus::Denied
    } else if block.is_error == Some(true) {
        ToolStatus::Failed
    } else {
        ToolStatus::Completed
    }
}

fn tokens(
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    split: Option<&CacheCreation>,
) -> RawTokens {
    RawTokens::Anthropic {
        input: input.unwrap_or(0),
        output: output.unwrap_or(0),
        cache_read: cache_read.unwrap_or(0),
        cache_write: cache_write.unwrap_or(0),
        cache_write_5m: split.and_then(|s| s.ephemeral_5m_input_tokens).unwrap_or(0),
        cache_write_1h: split.and_then(|s| s.ephemeral_1h_input_tokens).unwrap_or(0),
    }
}

fn advisor(iterations: Option<&[Iteration<'_>]>, line_model: Option<&str>) -> Option<AdvisorUsage> {
    let it = iterations?
        .iter()
        .find(|i| i.kind.as_deref() == Some("advisor_message"))?;
    let model = it
        .advisor_model
        .as_deref()
        .or(line_model)
        .or(it.model.as_deref())?;
    Some(AdvisorUsage {
        model: model.into(),
        tokens: tokens(
            it.input_tokens,
            it.output_tokens,
            it.cache_read_input_tokens,
            it.cache_creation_input_tokens,
            it.cache_creation.as_ref(),
        ),
    })
}

/// 一次解析：从某个解析器状态起，逐行交出记录、更新索引。
pub(super) struct Parse<'s> {
    pub owner: &'s Owner,
    pub state: State,
    pub records: Vec<Positioned<Record>>,
    pub index: Option<&'s mut IndexBuilder>,
    pub stats: ParseStats,
    /// 这一遍里第一条被跳过的行（「日志与诊断」）。
    pub skip: FirstSkip,
    /// 正在解析的行：偏移与长度。
    pub cur: (u64, u64),
}

impl Parse<'_> {
    fn bad(&mut self) {
        self.bad_as(Skip::Missing);
    }

    fn bad_as(&mut self, kind: Skip) {
        self.stats.bad_lines += 1;
        self.skip.note(Some(self.cur.0), Some(self.cur.1), kind);
    }

    fn emit(&mut self, pos: u64, seq: &mut u16, inherited: bool, value: Record) {
        self.records.push(Positioned {
            pos,
            seq: *seq,
            inherited,
            value,
        });
        *seq = seq.saturating_add(1);
    }

    /// 任何类型的记录都更新会话头、cwd、分支和最后活动时间。
    fn observe(&mut self, line: &Line<'_>, at: Option<Timestamp>) {
        if line.session_id.is_some() && line.cwd.is_some() {
            self.state.header = true;
        }
        if let Some(cwd) = &line.cwd
            && self.state.cwd.as_deref() != Some(Path::new(&**cwd))
        {
            self.state.cwd = Some(Path::new(&**cwd).into());
        }
        if let Some(branch) = &line.git_branch {
            let branch = (!branch.is_empty()).then_some(&**branch);
            if self.state.branch.as_deref() != branch {
                self.state.branch = branch.map(Arc::from);
            }
        }
        if at.is_some() {
            self.state.last_at = at;
        }
    }

    /// 解析一行。`bytes` 不含换行。
    pub(super) fn line(&mut self, pos: u64, bytes: &[u8]) {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        self.cur = (pos, bytes.len() as u64);
        let line = match serde_json::from_slice::<Line>(bytes) {
            Ok(line) => line,
            Err(e) => return self.bad_as(Skip::of_json(&e)),
        };
        let Some(kind) = line.kind.as_deref() else {
            return self.bad();
        };
        let at = line.timestamp.as_deref().and_then(time::parse);
        let is_user = match kind {
            "user" => true,
            "assistant" => false,
            _ => {
                if !is_known_ignored(kind) {
                    self.stats.unknown_type(kind);
                    self.skip
                        .note(Some(pos), Some(bytes.len() as u64), Skip::UnknownType);
                }
                self.observe(&line, at);
                return;
            }
        };
        let (Some(at), Some(message)) = (at, line.message.as_ref()) else {
            return self.bad();
        };
        let header = line.session_id.is_some() && line.cwd.is_some();
        if !self.state.header && !header {
            return self.bad_as(Skip::NoHeader);
        }
        self.observe(&line, Some(at));
        let Some(cwd) = self.state.cwd.clone() else {
            return self.bad();
        };
        let inherited = self
            .owner
            .inherits(line.session_id.as_deref(), line.agent_id.as_deref());
        if !is_user
            && let Some(model) = message.model.as_deref()
            && self.state.model.as_deref() != Some(model)
        {
            self.state.model = Some(model.into());
        }
        let mut seq = 0;
        let ctx = Ctx {
            cwd: cwd.clone(),
            branch: self.state.branch.clone(),
            model: self.state.model.clone(),
        };
        // 自己的部分的第一条记录之前也给出一条：只看自己的记录的消费方同样有上下文。
        if self.state.emitted.as_ref() != Some(&ctx) || (self.state.emitted_inherited && !inherited)
        {
            self.emit(
                pos,
                &mut seq,
                inherited,
                Record::Context {
                    at,
                    cwd: ctx.cwd.clone(),
                    logged_branch: ctx.branch.clone(),
                    model: ctx.model.clone(),
                },
            );
            self.state.emitted = Some(ctx);
            self.state.emitted_inherited = inherited;
        }
        let len = u32::try_from(bytes.len()).expect("lines are at most 16 MB");
        if is_user {
            self.user(&line, pos, len, seq, at, inherited);
        } else {
            self.assistant(&line, message, pos, len, seq, at, inherited, &cwd);
        }
    }

    fn user(
        &mut self,
        line: &Line<'_>,
        pos: u64,
        len: u32,
        mut seq: u16,
        at: Timestamp,
        inherited: bool,
    ) {
        let subagent_task = matches!(self.owner, Owner::Subagent { .. })
            && !inherited
            && !self.state.first_user_seen;
        if !inherited {
            self.state.first_user_seen = true;
        }
        self.state.open_turn = user_leaves_open(line);
        let content = line.message.as_ref().and_then(|m| m.content.as_ref());
        match user_kind(line, subagent_task) {
            UserKind::Results => {
                let Some(StrOrSeq::Seq(blocks)) = content else {
                    return;
                };
                for (i, block) in blocks.iter().enumerate() {
                    if block.kind.as_deref() != Some("tool_result") {
                        continue;
                    }
                    let Some(id) = block.tool_use_id.as_deref() else {
                        continue;
                    };
                    let call = ToolCallId(id.into());
                    let status = status(block, line.tool_use_result.as_ref());
                    if !inherited && let Some(index) = self.index.as_deref_mut() {
                        index.finish_tool(
                            &call,
                            at,
                            status,
                            Some(ContentRef::new(pos, len, part(i))),
                        );
                    }
                    self.emit(
                        pos,
                        &mut seq,
                        inherited,
                        Record::ToolFinished(ToolFinished {
                            call,
                            at,
                            status,
                            exec_session: None,
                        }),
                    );
                }
            }
            UserKind::Prompt if line.uuid.is_some() => {
                let turn = TurnId(line.uuid.as_deref().unwrap_or_default().into());
                if inherited {
                    self.state.inherited_turn = Some(turn.clone());
                } else {
                    self.state.own_turn = Some(turn.clone());
                    if let Some(index) = self.index.as_deref_mut() {
                        index.start_turn(
                            turn.clone(),
                            at,
                            Some(ContentRef::new(pos, len, PART_TEXT)),
                            preview(text_head(content)),
                        );
                    }
                }
                self.emit(pos, &mut seq, inherited, Record::TurnStarted { turn, at });
            }
            UserKind::Prompt | UserKind::Injected => {
                if !inherited && let Some(index) = self.index.as_deref_mut() {
                    index.push(TurnItem::Injected(ContentRef::new(pos, len, PART_TEXT)));
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one call site; the values are all per-line"
    )]
    fn assistant(
        &mut self,
        line: &Line<'_>,
        message: &Message<'_>,
        pos: u64,
        len: u32,
        mut seq: u16,
        at: Timestamp,
        inherited: bool,
        cwd: &Path,
    ) {
        let turn = if inherited {
            self.state.inherited_turn.clone()
        } else {
            self.state.own_turn.clone()
        };
        match &message.content {
            Some(StrOrSeq::Seq(blocks)) => {
                for (i, block) in blocks.iter().enumerate() {
                    match block.kind.as_deref() {
                        Some("text") => {
                            let blank = block.text.as_ref().is_none_or(|t| t.blank);
                            if !inherited
                                && !blank
                                && let Some(index) = self.index.as_deref_mut()
                            {
                                index.push(TurnItem::AgentText(ContentRef::new(pos, len, part(i))));
                            }
                        }
                        Some("tool_use") => {
                            let Some(id) = block.id.as_deref() else {
                                continue;
                            };
                            let name: Arc<str> = block.name.as_deref().unwrap_or_default().into();
                            let kind = tools::kind(&name, block.input, cwd, &mut self.stats);
                            let call = ToolCallId(id.into());
                            if !inherited && let Some(index) = self.index.as_deref_mut() {
                                index.push(TurnItem::Tool(ToolCall {
                                    id: call.clone(),
                                    name: name.clone(),
                                    kind: kind.clone(),
                                    started_at: at,
                                    finished: None,
                                    input: ContentRef::new(pos, len, part(i)),
                                    result: None,
                                }));
                            }
                            self.emit(
                                pos,
                                &mut seq,
                                inherited,
                                Record::ToolStarted(ToolStarted {
                                    call,
                                    turn: turn.clone(),
                                    at,
                                    name,
                                    kind,
                                }),
                            );
                        }
                        _ => {}
                    }
                }
            }
            Some(StrOrSeq::Str(text)) => {
                if !inherited
                    && !text.blank
                    && let Some(index) = self.index.as_deref_mut()
                {
                    index.push(TurnItem::AgentText(ContentRef::new(pos, len, PART_TEXT)));
                }
            }
            None => {}
        }
        if let (Some(usage), Some(id)) = (&message.usage, message.id.as_deref()) {
            let record = UsageRecord {
                at,
                turn,
                model: message.model.as_deref().map(Arc::from),
                tokens: tokens(
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_input_tokens,
                    usage.cache_creation_input_tokens,
                    usage.cache_creation.as_ref(),
                ),
                key: UsageKey::Claude {
                    message_id: id.into(),
                    request_id: line.request_id.as_deref().map(Arc::from),
                },
                advisor: advisor(usage.iterations.as_deref(), line.advisor_model.as_deref()),
                superseded: false,
                sidechain: line.is_sidechain == Some(true),
                origin: UsageOrigin::SessionLog,
            };
            self.emit(pos, &mut seq, inherited, Record::Usage(record));
        }
        self.state.open_turn = assistant_leaves_open(message);
        if !self.state.open_turn
            && !inherited
            && let Some(index) = self.index.as_deref_mut()
        {
            index.end_turn(at);
        }
    }
}

/// 内容块下标到 `ContentRef::part`。
fn part(i: usize) -> u16 {
    // 一行最多 16 MB，内容块不会超过 65535 个；超过的按最后一个处理。
    u16::try_from(i).unwrap_or(PART_TEXT - 1)
}
