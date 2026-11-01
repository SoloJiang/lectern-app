//! 实时项：一个连接上收到的 `session/update` 变成快照里的对话项、计划和上下文占用
//! （docs/design/agents.md「ACP 客户端」，conversation 设计「服务接口」的快照不变量）。
//!
//! 纯数据，不做 IO、不取锁。实时项的键是 `l:<发出的序号>:<项序号>`，prompt 是 0。正文放在可增长的
//! `String` 里，只在换快照时冻结成 `Arc<str>`（`session/update` 合并后至多每 50 ms 换一次），所以一段
//! 回复的每个文字块不重新分配整段。
//!
//! 实时项换成历史项（第 8 步）：每个 prompt 是一个组。组的 `session/prompt` 返回了（`done`），并且历史
//! 窗口里有了它这一轮（[`Live::settle`] 按发出的时刻和 prompt 的第一行认），这一组的项就整组去掉，
//! 项数和正文字节数随之回到去掉之前的值，之后的回复不会因为前几轮占着上限而被截断。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lectern_services::conversation::{
    CallKind, CallState, ContextUsage, EndReason, Entry, EntryBody, EntryKey, Passage, PlanStep,
    PromptOrigin, StepState, ToolEntry,
};

use super::history::Mark;
use super::wire::{PlanItem, Stop, ToolInfo};
use crate::index::preview;

/// 单段正文的上限。
const PASSAGE_MAX: usize = 64 << 10;
/// 实时项的正文合计上限（与历史窗口合计 1 MiB，超过时历史窗口从最早的轮次起去掉）。
pub(crate) const TEXT_MAX: usize = 1 << 20;
/// 实时项的个数上限；再多的丢弃并计数。
const ITEMS_MAX: usize = 4096;
/// 工具行里存的标题、命令、每个路径的字节数上限（只用来写 `summary`，`summary` 自己至多 160 个字符），
/// 和存的路径个数上限（`summary` 至多列 3 个）。agent 一次更新里给得再多也不会撑大实时项。
pub(crate) const LIVE_FIELD_MAX: usize = 4 << 10;
const PATHS_MAX: usize = 16;
/// 认「这一轮已经在轮次索引里」时，日志里的开始时刻可以比我们发出 prompt 的时刻早多少（毫秒）：
/// 两边都取自本机的时钟，只留一点取整的余地。
const SENT_SLACK_MS: i64 = 2000;

/// `summary` 的字符数上限、列出的文件数。
const SUMMARY_CHARS: usize = 160;
const SUMMARY_FILES: usize = 3;
/// 计划：步数与每步文字的上限。
const PLAN_STEPS: usize = 100;
const PLAN_CHARS: usize = 512;

/// 一个连接上的实时内容。
#[derive(Default)]
pub(crate) struct Live {
    /// 下一个 prompt 的序号，只增不减。
    next_seq: u32,
    /// 当前 prompt 的序号与下一项的序号。
    group: u32,
    item: u32,
    items: Vec<Item>,
    /// 正文字节数。
    bytes: usize,
    /// `toolCallId` 在 `items` 里的位置。
    tools: HashMap<String, usize>,
    /// 用户拒绝过的工具调用：之后的 `failed` 是「已拒绝」。
    denied: HashSet<String>,
    plan: Option<Arc<[PlanStep]>>,
    context: Option<ContextUsage>,
    /// 还没有换成历史项的 prompt，按发出的先后。
    groups: Vec<Group>,
    /// 跳过的更新（不认识的种类、形状不对、回放、不在一轮之内）与因上限丢弃的项。
    skipped: u64,
    dropped: u64,
}

struct Item {
    key: EntryKey,
    /// 所在的 prompt 的序号（`Group::seq`）。
    group: u32,
    body: Body,
}

impl Item {
    fn text_len(&self) -> usize {
        match &self.body {
            Body::Prompt { text, .. } => text.text.len(),
            Body::Text { buf, .. } => buf.len(),
            Body::Tool { .. } | Body::Ended(_) => 0,
        }
    }
}

/// 一个发出的 prompt，和认它在轮次索引里的依据。
struct Group {
    seq: u32,
    /// 发出的时刻（UTC 毫秒）。
    sent_at: i64,
    /// prompt 的第一行，与轮次索引里的 `Turn::prompt_preview` 同一个函数算出。
    first_line: Arc<str>,
    /// `session/prompt` 已经返回（有结果、出错，或连接在这一轮中间结束）。
    done: bool,
}

enum Body {
    Prompt {
        text: Passage,
        origin: PromptOrigin,
    },
    Text {
        buf: String,
        truncated: bool,
        streaming: bool,
        /// 冻结的副本；追加后失效。
        frozen: Option<Arc<str>>,
    },
    Tool {
        kind: CallKind,
        state: CallState,
        title: Option<String>,
        paths: Vec<String>,
        command: Option<String>,
        summary: Arc<str>,
    },
    Ended(EndReason),
}

impl Live {
    /// 新的一个 prompt（`sent_at` 是发出的时刻，UTC 毫秒）：之后的更新归在它名下。
    pub(crate) fn begin_prompt(&mut self, text: &str, origin: PromptOrigin, sent_at: i64) {
        self.end_streaming();
        self.group = self.next_seq;
        self.next_seq += 1;
        self.item = 1;
        self.groups.push(Group {
            seq: self.group,
            sent_at,
            first_line: preview(text),
            done: false,
        });
        let text = passage(text);
        let key = self.key(0);
        let len = text.text.len();
        if self.push(key, Body::Prompt { text, origin }).is_some() {
            self.bytes += len;
        }
    }

    fn key(&self, item: u32) -> EntryKey {
        EntryKey(format!("l:{}:{item}", self.group).into())
    }

    fn next_key(&mut self) -> EntryKey {
        let key = self.key(self.item);
        self.item += 1;
        key
    }

    fn push(&mut self, key: EntryKey, body: Body) -> Option<usize> {
        if self.items.len() >= ITEMS_MAX {
            self.dropped += 1;
            return None;
        }
        self.items.push(Item {
            key,
            group: self.group,
            body,
        });
        Some(self.items.len() - 1)
    }

    /// 最后一段回复不再接收。
    fn end_streaming(&mut self) {
        if let Some(Item {
            body: Body::Text { streaming, .. },
            ..
        }) = self.items.last_mut()
        {
            *streaming = false;
        }
    }

    /// `agent_message_chunk`：接在正在接收的一段后面，否则另起一段。
    pub(crate) fn agent_text(&mut self, chunk: &str) {
        if let Some(Item {
            body:
                Body::Text {
                    buf,
                    truncated,
                    streaming: true,
                    frozen,
                },
            ..
        }) = self.items.last_mut()
        {
            let room = PASSAGE_MAX
                .saturating_sub(buf.len())
                .min(TEXT_MAX.saturating_sub(self.bytes));
            let (take, cut) = clip_bytes(chunk, room);
            if take.is_empty() && !cut {
                return;
            }
            buf.push_str(take);
            self.bytes += take.len();
            *truncated |= cut;
            *frozen = None;
            return;
        }
        let room = PASSAGE_MAX.min(TEXT_MAX.saturating_sub(self.bytes));
        let (take, cut) = clip_bytes(chunk, room);
        let key = self.next_key();
        let pushed = self.push(
            key,
            Body::Text {
                buf: take.to_owned(),
                truncated: cut,
                streaming: true,
                frozen: None,
            },
        );
        if pushed.is_some() {
            self.bytes += take.len();
        }
    }

    /// `tool_call` 和 `tool_call_update`：按 `toolCallId` 更新，没见过的新建（更新先于创建到达时也一样）。
    pub(crate) fn tool(&mut self, info: ToolInfo) {
        self.end_streaming();
        let denied = self.denied.contains(&info.id);
        if let Some(&at) = self.tools.get(&info.id) {
            if let Body::Tool {
                kind,
                state,
                title,
                paths,
                command,
                summary,
                ..
            } = &mut self.items[at].body
            {
                if let Some(new) = info.kind {
                    *kind = new;
                }
                if let Some(new) = info.state {
                    *state = if new == CallState::Failed && denied {
                        CallState::Denied
                    } else {
                        new
                    };
                }
                if info.title.is_some() {
                    *title = info.title.map(cap);
                }
                if !info.paths.is_empty() {
                    *paths = capped_paths(info.paths);
                }
                if info.command.is_some() {
                    *command = info.command.map(cap);
                }
                *summary = summarize(*kind, title.as_deref(), paths, command.as_deref());
            }
            return;
        }
        let kind = info.kind.unwrap_or(CallKind::Other);
        let state = match info.state {
            Some(CallState::Failed) if denied => CallState::Denied,
            Some(state) => state,
            None => CallState::Pending,
        };
        let title = info.title.map(cap);
        let paths = capped_paths(info.paths);
        let command = info.command.map(cap);
        let summary = summarize(kind, title.as_deref(), &paths, command.as_deref());
        let key = self.next_key();
        let id = info.id;
        if let Some(at) = self.push(
            key,
            Body::Tool {
                kind,
                state,
                title,
                paths,
                command,
                summary,
            },
        ) {
            self.tools.insert(id, at);
        }
    }

    /// 这个工具调用的权限被用户拒绝：之后的失败是「已拒绝」，现在还没有结果的也是。
    pub(crate) fn deny(&mut self, tool_call_id: &str) {
        self.denied.insert(tool_call_id.to_owned());
        if let Some(&at) = self.tools.get(tool_call_id)
            && let Body::Tool { state, .. } = &mut self.items[at].body
            && matches!(state, CallState::Pending | CallState::Running)
        {
            *state = CallState::Denied;
        }
    }

    /// 权限请求里的工具调用种类，更新还没到时用已有的项。
    pub(crate) fn tool_kind(&self, tool_call_id: &str) -> Option<CallKind> {
        match &self.items[*self.tools.get(tool_call_id)?].body {
            Body::Tool { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// `plan`：整份替换。
    pub(crate) fn set_plan(&mut self, items: &[PlanItem]) {
        let steps: Vec<PlanStep> = items
            .iter()
            .take(PLAN_STEPS)
            .map(|item| PlanStep {
                text: clip(&item.content, PLAN_CHARS),
                state: match item.status.as_str() {
                    "in_progress" => StepState::InProgress,
                    "completed" => StepState::Completed,
                    _ => StepState::Pending,
                },
            })
            .collect();
        self.plan = Some(steps.into());
    }

    /// `usage_update`：`size` 为 0 时不用。
    pub(crate) fn set_usage(&mut self, used: u64, size: u64) {
        if size > 0 {
            self.context = Some(ContextUsage { used, size });
        }
    }

    /// 计数一次跳过的更新。
    pub(crate) fn skip(&mut self) {
        self.skipped += 1;
    }

    pub(crate) fn skipped(&self) -> u64 {
        self.skipped
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }

    /// 一轮结束（`session/prompt` 回复了）。
    pub(crate) fn finish(&mut self, stop: Stop) {
        self.end_streaming();
        self.finish_group();
        if stop == Stop::Cancelled {
            self.cancel_unfinished();
        }
        let reason = match stop {
            Stop::EndTurn => return,
            Stop::MaxTokens => EndReason::MaxTokens,
            Stop::MaxTurnRequests => EndReason::MaxTurnRequests,
            Stop::Refusal => EndReason::Refusal,
            Stop::Cancelled => EndReason::Cancelled,
        };
        let key = self.next_key();
        self.push(key, Body::Ended(reason));
    }

    /// 连接在一轮中间结束，或 `session/prompt` 回复了错误：没有结果的工具调用是「已取消」。
    pub(crate) fn abort(&mut self) {
        self.end_streaming();
        self.finish_group();
        self.cancel_unfinished();
    }

    /// 最后一个 prompt 不会再有更新：它这一组可以在轮次索引里有了这一轮之后换成历史项。
    fn finish_group(&mut self) {
        if let Some(group) = self.groups.last_mut() {
            group.done = true;
        }
    }

    /// 历史窗口读新了（或实时项有变）：已返回、并且轮次索引里已有这一轮的 prompt，它的项整组去掉
    /// （在同一次换快照里，所以同一轮不会出现两次）。返回还没返回、但轮次索引里已经有了的那些轮的
    /// 序号：它们的历史项暂时不显示，实时项更全。
    ///
    /// 认法：窗口里序号比前一个认出的轮次大、开始时刻不早于发出的时刻（留 2 秒余地）、prompt 的第一行
    /// 相同、并且开始时刻早于下一个 prompt 发出的时刻的第一轮。时刻挡住别人在 CLI 里先前说过同样的话的
    /// 轮次；同一时间只有一个 prompt 在跑，所以上界挡住失败的 prompt 认走重试的那一轮；序号保证两个相同的
    /// prompt 各认各的。
    pub(crate) fn settle(&mut self, marks: &[Mark]) -> Vec<u32> {
        let mut hidden = Vec::new();
        let mut retire = Vec::new();
        let mut after = 0u32;
        for (index, group) in self.groups.iter().enumerate() {
            let next_sent = self.groups.get(index + 1).map(|next| next.sent_at);
            let found = marks.iter().find(|mark| {
                mark.ordinal > after
                    && mark.started_at.0.saturating_add(SENT_SLACK_MS) >= group.sent_at
                    && next_sent.is_none_or(|sent| mark.started_at.0 < sent)
                    && mark.preview == group.first_line
            });
            let Some(mark) = found else { continue };
            after = mark.ordinal;
            if group.done {
                retire.push(group.seq);
            } else {
                hidden.push(mark.ordinal);
            }
        }
        self.retire(&retire);
        hidden
    }

    /// 去掉这些组的全部项，重排 `tools` 里的位置，正文字节数减去它们的。
    fn retire(&mut self, seqs: &[u32]) {
        if seqs.is_empty() {
            return;
        }
        let mut remap: Vec<Option<usize>> = Vec::with_capacity(self.items.len());
        let mut kept = Vec::with_capacity(self.items.len());
        for item in std::mem::take(&mut self.items) {
            if seqs.contains(&item.group) {
                self.bytes = self.bytes.saturating_sub(item.text_len());
                remap.push(None);
            } else {
                remap.push(Some(kept.len()));
                kept.push(item);
            }
        }
        self.items = kept;
        self.tools.retain(|_, at| match remap[*at] {
            Some(now) => {
                *at = now;
                true
            }
            None => false,
        });
        self.denied.retain(|id| self.tools.contains_key(id));
        self.groups.retain(|group| !seqs.contains(&group.seq));
    }

    /// 还没有换成历史项的 prompt 数。
    #[cfg(test)]
    pub(crate) fn pending_groups(&self) -> usize {
        self.groups.len()
    }

    fn cancel_unfinished(&mut self) {
        for item in &mut self.items {
            if let Body::Tool { state, .. } = &mut item.body
                && matches!(state, CallState::Pending | CallState::Running)
            {
                *state = CallState::Cancelled;
            }
        }
    }

    /// 连接关闭：实时项、计划和上下文占用都不再有（历史窗口重读后带上这一轮）。
    pub(crate) fn clear(&mut self) {
        self.items.clear();
        self.groups.clear();
        self.bytes = 0;
        self.tools.clear();
        self.denied.clear();
        self.plan = None;
        self.context = None;
    }

    pub(crate) fn plan(&self) -> Option<Arc<[PlanStep]>> {
        self.plan.clone()
    }

    pub(crate) fn context(&self) -> Option<ContextUsage> {
        self.context
    }

    /// 正文字节数，历史窗口据此让出空间。
    pub(crate) fn text_bytes(&self) -> usize {
        self.bytes
    }

    /// 快照里的实时项。冻结正在增长的正文。
    pub(crate) fn entries(&mut self) -> Vec<Entry> {
        self.items
            .iter_mut()
            .map(|item| Entry {
                key: item.key.clone(),
                body: match &mut item.body {
                    Body::Prompt { text, origin } => EntryBody::Prompt {
                        text: text.clone(),
                        origin: origin.clone(),
                    },
                    Body::Text {
                        buf,
                        truncated,
                        streaming,
                        frozen,
                    } => EntryBody::AgentText {
                        text: Passage {
                            text: frozen.get_or_insert_with(|| buf.as_str().into()).clone(),
                            truncated: *truncated,
                        },
                        streaming: *streaming,
                    },
                    Body::Tool {
                        kind,
                        state,
                        summary,
                        ..
                    } => EntryBody::Tool(ToolEntry {
                        kind: *kind,
                        state: *state,
                        summary: summary.clone(),
                    }),
                    Body::Ended(reason) => EntryBody::Ended(*reason),
                },
            })
            .collect()
    }
}

/// 工具行的 `summary`（conversation 设计「服务接口」的 `ToolEntry`）。
fn summarize(
    kind: CallKind,
    title: Option<&str>,
    paths: &[String],
    command: Option<&str>,
) -> Arc<str> {
    let title_line = || first_line(title.unwrap_or_default());
    match kind {
        CallKind::Edit | CallKind::Read | CallKind::Delete | CallKind::Move
            if !paths.is_empty() =>
        {
            let names: Vec<&str> = paths
                .iter()
                .take(SUMMARY_FILES)
                .map(|path| {
                    path.rsplit('/')
                        .find(|name| !name.is_empty())
                        .unwrap_or(path)
                })
                .collect();
            clip(&names.join(", "), SUMMARY_CHARS)
        }
        CallKind::Execute => match command {
            Some(command) => clip(first_line(command), SUMMARY_CHARS),
            None => clip(title_line(), SUMMARY_CHARS),
        },
        _ => clip(title_line(), SUMMARY_CHARS),
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// 一个字段至多 [`LIVE_FIELD_MAX`] 字节（在字符边界上截断）。
fn cap(text: String) -> String {
    if text.len() <= LIVE_FIELD_MAX {
        return text;
    }
    clip_bytes(&text, LIVE_FIELD_MAX).0.to_owned()
}

fn capped_paths(paths: Vec<String>) -> Vec<String> {
    paths.into_iter().take(PATHS_MAX).map(cap).collect()
}

/// 至多 `chars` 个字符。
pub(crate) fn clip(text: &str, chars: usize) -> Arc<str> {
    match text.char_indices().nth(chars) {
        Some((end, _)) => text[..end].into(),
        None => text.into(),
    }
}

/// 至多 `max` 字节，在字符边界上截断；第二个值是有没有截掉。
pub(crate) fn clip_bytes(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

fn passage(text: &str) -> Passage {
    let (kept, truncated) = clip_bytes(text, PASSAGE_MAX);
    Passage {
        text: kept.into(),
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str, kind: Option<CallKind>, state: Option<CallState>) -> ToolInfo {
        ToolInfo {
            id: id.into(),
            kind,
            state,
            ..ToolInfo::default()
        }
    }

    fn keys(live: &mut Live) -> Vec<String> {
        live.entries().iter().map(|e| e.key.0.to_string()).collect()
    }

    #[test]
    fn a_prompt_and_its_reply_get_ordered_keys() {
        let mut live = Live::default();
        live.begin_prompt("question", PromptOrigin::User, 0);
        live.agent_text("an ");
        live.agent_text("answer");
        live.tool(tool("t1", Some(CallKind::Read), Some(CallState::Pending)));
        live.agent_text("more");
        live.finish(Stop::EndTurn);
        live.begin_prompt("again", PromptOrigin::User, 0);
        assert_eq!(
            keys(&mut live),
            ["l:0:0", "l:0:1", "l:0:2", "l:0:3", "l:1:0"]
        );
        let entries = live.entries();
        let EntryBody::AgentText { text, streaming } = &entries[1].body else {
            panic!("reply text");
        };
        assert_eq!(&*text.text, "an answer");
        assert!(!streaming, "a tool call ends the previous text");
        let EntryBody::AgentText { streaming, .. } = &entries[3].body else {
            panic!("reply text");
        };
        assert!(!streaming, "the turn ended");
    }

    #[test]
    fn only_the_last_text_of_a_running_turn_streams() {
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User, 0);
        live.agent_text("a");
        let entries = live.entries();
        assert!(matches!(
            entries[1].body,
            EntryBody::AgentText {
                streaming: true,
                ..
            }
        ));
        // Frozen text is rebuilt after an append.
        live.agent_text("b");
        let entries = live.entries();
        let EntryBody::AgentText { text, .. } = &entries[1].body else {
            panic!("text");
        };
        assert_eq!(&*text.text, "ab");
    }

    #[test]
    fn tool_calls_update_in_place_and_denials_stick() {
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User, 0);
        let mut call = tool("t1", Some(CallKind::Edit), Some(CallState::Pending));
        call.paths = vec!["/a/b/one.rs".into(), "/a/two.rs".into()];
        live.tool(call);
        live.tool(tool("t1", None, Some(CallState::Running)));
        live.tool(tool(
            "t2",
            Some(CallKind::Execute),
            Some(CallState::Running),
        ));
        live.deny("t2");
        live.tool(tool("t2", None, Some(CallState::Failed)));
        // An update that arrives before its creation still makes an entry.
        live.tool(tool(
            "t3",
            Some(CallKind::Fetch),
            Some(CallState::Completed),
        ));
        let entries = live.entries();
        let EntryBody::Tool(edit) = &entries[1].body else {
            panic!("edit");
        };
        assert_eq!(
            (edit.kind, edit.state, &*edit.summary),
            (CallKind::Edit, CallState::Running, "one.rs, two.rs")
        );
        let EntryBody::Tool(run) = &entries[2].body else {
            panic!("run");
        };
        assert_eq!(run.state, CallState::Denied);
        assert_eq!(entries.len(), 4);
        assert_eq!(live.tool_kind("t2"), Some(CallKind::Execute));
        assert_eq!(live.tool_kind("nope"), None);
    }

    #[test]
    fn summaries_follow_the_kind() {
        let long = "é".repeat(400);
        assert_eq!(
            summarize(CallKind::Execute, Some("title"), &[], Some("ls -l\nsecond")).as_ref(),
            "ls -l"
        );
        assert_eq!(
            summarize(CallKind::Execute, Some("Run it\nmore"), &[], None).as_ref(),
            "Run it"
        );
        assert_eq!(
            summarize(CallKind::Search, Some("find things"), &["/x".into()], None).as_ref(),
            "find things"
        );
        assert_eq!(
            summarize(CallKind::Edit, Some("Edit file"), &[], None).as_ref(),
            "Edit file"
        );
        assert_eq!(summarize(CallKind::Other, None, &[], None).as_ref(), "");
        let many: Vec<String> = (0..5).map(|n| format!("/d/f{n}.rs")).collect();
        assert_eq!(
            summarize(CallKind::Read, None, &many, None).as_ref(),
            "f0.rs, f1.rs, f2.rs"
        );
        assert_eq!(
            summarize(CallKind::Search, Some(&long), &[], None)
                .chars()
                .count(),
            SUMMARY_CHARS
        );
    }

    #[test]
    fn finishing_a_turn_cancels_unfinished_tools_and_records_why() {
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User, 0);
        live.tool(tool(
            "t1",
            Some(CallKind::Execute),
            Some(CallState::Running),
        ));
        live.tool(tool("t2", Some(CallKind::Read), Some(CallState::Completed)));
        live.agent_text("partial");
        live.finish(Stop::Cancelled);
        let entries = live.entries();
        assert_eq!(entries.len(), 5);
        let EntryBody::Tool(first) = &entries[1].body else {
            panic!("tool");
        };
        assert_eq!(first.state, CallState::Cancelled);
        let EntryBody::Tool(second) = &entries[2].body else {
            panic!("tool");
        };
        assert_eq!(second.state, CallState::Completed);
        assert!(matches!(
            entries[4].body,
            EntryBody::Ended(EndReason::Cancelled)
        ));
        for (stop, reason) in [
            (Stop::MaxTokens, EndReason::MaxTokens),
            (Stop::MaxTurnRequests, EndReason::MaxTurnRequests),
            (Stop::Refusal, EndReason::Refusal),
        ] {
            let mut live = Live::default();
            live.begin_prompt("q", PromptOrigin::User, 0);
            live.finish(stop);
            assert!(matches!(&live.entries()[1].body, EntryBody::Ended(r) if *r == reason));
        }
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User, 0);
        live.finish(Stop::EndTurn);
        assert_eq!(live.entries().len(), 1);
    }

    #[test]
    fn plans_and_usage_replace_and_clear() {
        let mut live = Live::default();
        let item = |content: &str, status: &str| PlanItem {
            content: content.into(),
            status: status.into(),
        };
        live.set_plan(&[
            item("a", "completed"),
            item("b", "in_progress"),
            item("c", "pending"),
        ]);
        live.set_plan(&[item("only", "weird")]);
        let plan = live.plan().unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].state, StepState::Pending);
        live.set_usage(5, 0);
        assert_eq!(live.context(), None);
        live.set_usage(5, 50);
        assert_eq!(live.context(), Some(ContextUsage { used: 5, size: 50 }));
        live.clear();
        assert!(live.plan().is_none() && live.context().is_none());
    }

    #[test]
    fn text_is_capped_per_passage_and_in_total() {
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User, 0);
        // 3-byte characters: the cap is not a multiple of 3.
        let chunk = "界".repeat(PASSAGE_MAX / 3 + 10);
        live.agent_text(&chunk);
        live.agent_text("more");
        let entries = live.entries();
        let EntryBody::AgentText { text, .. } = &entries[1].body else {
            panic!("text");
        };
        assert!(text.truncated);
        assert!(text.text.len() <= PASSAGE_MAX && text.text.len() > PASSAGE_MAX - 3);
        // Many passages stop at the total.
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User, 0);
        for _ in 0..40 {
            live.agent_text(&"x".repeat(PASSAGE_MAX));
            live.tool(tool("t", Some(CallKind::Think), None));
            live.tool(tool(
                &format!("t{}", live.items.len()),
                Some(CallKind::Think),
                None,
            ));
        }
        assert!(live.text_bytes() <= TEXT_MAX);
    }

    #[test]
    fn items_beyond_the_cap_are_counted() {
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User, 0);
        for n in 0..ITEMS_MAX + 10 {
            live.tool(tool(&format!("t{n}"), Some(CallKind::Think), None));
        }
        assert_eq!(live.entries().len(), ITEMS_MAX);
        assert!(live.dropped() >= 10);
        live.skip();
        assert_eq!(live.skipped(), 1);
    }

    #[test]
    fn aborting_cancels_what_was_running() {
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User, 0);
        live.tool(tool("t", Some(CallKind::Execute), Some(CallState::Running)));
        live.agent_text("x");
        live.abort();
        let entries = live.entries();
        assert!(matches!(&entries[1].body, EntryBody::Tool(t) if t.state == CallState::Cancelled));
        assert!(matches!(
            &entries[2].body,
            EntryBody::AgentText {
                streaming: false,
                ..
            }
        ));
    }

    #[test]
    fn tool_fields_are_clipped_on_creation_and_on_update() {
        let long = "é".repeat(LIVE_FIELD_MAX);
        let mut call = tool("t", Some(CallKind::Edit), Some(CallState::Pending));
        call.title = Some(long.clone());
        call.command = Some(long.clone());
        call.paths = (0..PATHS_MAX + 20).map(|n| format!("/{long}{n}")).collect();
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User, 0);
        live.tool(call.clone());
        live.tool(call);
        let Body::Tool {
            title,
            command,
            paths,
            ..
        } = &live.items[1].body
        else {
            panic!("a tool");
        };
        assert_eq!(title.as_ref().map(String::len), Some(LIVE_FIELD_MAX));
        assert_eq!(command.as_ref().map(String::len), Some(LIVE_FIELD_MAX));
        assert_eq!(paths.len(), PATHS_MAX);
        assert!(paths.iter().all(|path| path.len() <= LIVE_FIELD_MAX));
        // Short fields stay as they are.
        assert_eq!(cap("short".into()), "short");
    }

    fn mark(ordinal: u32, started_at: i64, preview: &str) -> Mark {
        Mark {
            ordinal,
            started_at: lectern_services::session::Timestamp(started_at),
            preview: preview.into(),
        }
    }

    #[test]
    fn a_finished_turn_that_is_in_the_log_is_retired_and_the_counts_reset() {
        let mut live = Live::default();
        let marks = [mark(7, 5_000, "second question")];
        live.begin_prompt("first question", PromptOrigin::User, 1_000);
        live.agent_text("an answer");
        live.tool(tool("t1", Some(CallKind::Read), Some(CallState::Running)));
        live.finish(Stop::EndTurn);
        live.begin_prompt("second question\nmore lines", PromptOrigin::User, 4_000);
        live.tool(tool("t2", Some(CallKind::Edit), Some(CallState::Running)));
        live.tool(tool("t3", Some(CallKind::Edit), Some(CallState::Running)));
        live.agent_text("working");
        // Nothing in the log yet for the first turn: it stays. The second is running and in the log:
        // its history is hidden and the live items stay.
        assert_eq!(live.settle(&marks), [7]);
        assert_eq!(live.pending_groups(), 2);
        assert_eq!(live.entries().len(), 7);

        // The second turn returns: it is retired, the first one is not in the log and stays.
        live.finish(Stop::EndTurn);
        assert_eq!(live.settle(&marks), Vec::<u32>::new());
        assert_eq!(live.pending_groups(), 1);
        let entries = live.entries();
        assert_eq!(
            entries.iter().map(|e| &*e.key.0).collect::<Vec<_>>(),
            ["l:0:0", "l:0:1", "l:0:2"]
        );
        assert_eq!(
            live.text_bytes(),
            "first question".len() + "an answer".len()
        );
        // The tool of the retired turn is forgotten; the one that stays still updates in place.
        assert_eq!(live.tool_kind("t2"), None);
        live.tool(tool("t1", None, Some(CallState::Completed)));
        let entries = live.entries();
        assert!(matches!(&entries[2].body, EntryBody::Tool(t) if t.state == CallState::Completed));
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn retired_turns_give_the_caps_back() {
        let mut live = Live::default();
        let mut marks = Vec::new();
        // Each turn is 640 KiB of text and 21 items. Without the retirement the second one would run
        // into the 1 MiB total, and the 4096 items fill up long before the last one.
        for n in 0..300u32 {
            let sent = i64::from(n) * 1_000;
            live.begin_prompt(&format!("turn {n}"), PromptOrigin::User, sent);
            for i in 0..10 {
                live.agent_text(&"x".repeat(PASSAGE_MAX));
                live.tool(tool(&format!("t{n}-{i}"), Some(CallKind::Think), None));
            }
            let entries = live.entries();
            let reply = entries
                .iter()
                .rev()
                .find_map(|e| match &e.body {
                    EntryBody::AgentText { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .expect("a reply");
            assert!(!reply.truncated, "turn {n}");
            assert_eq!(reply.text.len(), PASSAGE_MAX, "turn {n}");
            assert_eq!(live.dropped(), 0, "turn {n}");
            live.finish(Stop::EndTurn);
            marks.push(mark(n + 1, sent + 100, &format!("turn {n}")));
            assert!(live.settle(&marks).is_empty());
            assert_eq!(live.pending_groups(), 0, "turn {n}");
            assert_eq!(live.text_bytes(), 0, "turn {n}");
            assert!(live.entries().is_empty(), "turn {n}");
        }
    }

    #[test]
    fn a_turn_is_told_by_time_and_first_line_and_each_mark_belongs_to_one_prompt() {
        let mut live = Live::default();
        live.begin_prompt("go on", PromptOrigin::User, 10_000);
        live.finish(Stop::EndTurn);
        live.begin_prompt("  go on  ", PromptOrigin::User, 20_000);
        live.finish(Stop::EndTurn);
        // An older turn with the same words, a turn with other words, and a turn that began before the
        // prompt was sent (two seconds of slack): none is ours.
        let marks = [
            mark(1, 1_000, "go on"),
            mark(2, 12_000, "something else"),
            mark(3, 7_000, "go on"),
        ];
        assert!(live.settle(&marks).is_empty());
        assert_eq!(live.pending_groups(), 2);
        // The first prompt gets the first mark after its time, the second the next one.
        let marks = [
            mark(1, 1_000, "go on"),
            mark(2, 8_500, "go on"),
            mark(3, 20_100, "go on"),
        ];
        assert!(live.settle(&marks).is_empty());
        assert_eq!(live.pending_groups(), 0);

        // Two identical prompts, only the first in the log so far: the first is retired, the second
        // stays live, even though the same mark would also fit it.
        let mut live = Live::default();
        live.begin_prompt("go on", PromptOrigin::User, 10_000);
        live.finish(Stop::EndTurn);
        live.begin_prompt("go on", PromptOrigin::User, 10_100);
        live.finish(Stop::EndTurn);
        assert!(live.settle(&[mark(4, 10_050, "go on")]).is_empty());
        assert_eq!(live.pending_groups(), 1);
    }

    #[test]
    fn a_failed_prompt_does_not_take_the_turn_of_its_retry() {
        let mut live = Live::default();
        live.begin_prompt("go on", PromptOrigin::User, 10_000);
        live.agent_text("failed");
        live.abort();
        live.begin_prompt("go on", PromptOrigin::User, 11_000);
        live.agent_text("worked");
        live.finish(Stop::EndTurn);
        // Only the retry reached the log. It goes; the failed prompt, within the two seconds of slack too,
        // stays on screen.
        assert!(live.settle(&[mark(1, 11_100, "go on")]).is_empty());
        assert_eq!(live.pending_groups(), 1);
        let entries = live.entries();
        assert!(entries.iter().any(|e| matches!(
            &e.body,
            EntryBody::AgentText { text, .. } if &*text.text == "failed"
        )));
        assert!(!entries.iter().any(|e| matches!(
            &e.body,
            EntryBody::AgentText { text, .. } if &*text.text == "worked"
        )));
    }

    #[test]
    fn a_turn_cut_short_counts_as_returned() {
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User, 1_000);
        live.agent_text("half");
        // The connection died, or the prompt failed.
        live.abort();
        assert!(live.settle(&[mark(1, 1_100, "q")]).is_empty());
        assert!(live.entries().is_empty());
    }
}
