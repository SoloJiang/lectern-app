//! 实时项：一个连接上收到的 `session/update` 变成快照里的对话项、计划和上下文占用
//! （docs/design/agents.md「ACP 客户端」，conversation 设计「服务接口」的快照不变量）。
//!
//! 纯数据，不做 IO、不取锁。实时项的键是 `l:<发出的序号>:<项序号>`，prompt 是 0。正文放在可增长的
//! `String` 里，只在换快照时冻结成 `Arc<str>`（`session/update` 合并后至多每 50 ms 换一次），所以一段
//! 回复的每个文字块不重新分配整段。第 8 步把实时项换成历史项。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lectern_services::conversation::{
    CallKind, CallState, ContextUsage, EndReason, Entry, EntryBody, EntryKey, Passage, PlanStep,
    PromptOrigin, StepState, ToolEntry,
};

use super::wire::{PlanItem, Stop, ToolInfo};

/// 单段正文的上限。
const PASSAGE_MAX: usize = 64 << 10;
/// 实时项的正文合计上限（与历史窗口合计 1 MiB，超过时历史窗口从最早的轮次起去掉）。
pub(crate) const TEXT_MAX: usize = 1 << 20;
/// 实时项的个数上限；再多的丢弃并计数。
const ITEMS_MAX: usize = 4096;
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
    /// 跳过的更新（不认识的种类、形状不对、回放、不在一轮之内）与因上限丢弃的项。
    skipped: u64,
    dropped: u64,
}

struct Item {
    key: EntryKey,
    body: Body,
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
    /// 新的一个 prompt：之后的更新归在它名下。
    pub(crate) fn begin_prompt(&mut self, text: &str, origin: PromptOrigin) {
        self.end_streaming();
        self.group = self.next_seq;
        self.next_seq += 1;
        self.item = 1;
        let text = passage(text);
        self.bytes += text.text.len();
        let key = self.key(0);
        self.push(Item {
            key,
            body: Body::Prompt { text, origin },
        });
    }

    fn key(&self, item: u32) -> EntryKey {
        EntryKey(format!("l:{}:{item}", self.group).into())
    }

    fn next_key(&mut self) -> EntryKey {
        let key = self.key(self.item);
        self.item += 1;
        key
    }

    fn push(&mut self, item: Item) -> Option<usize> {
        if self.items.len() >= ITEMS_MAX {
            self.dropped += 1;
            return None;
        }
        self.items.push(item);
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
        let pushed = self.push(Item {
            key,
            body: Body::Text {
                buf: take.to_owned(),
                truncated: cut,
                streaming: true,
                frozen: None,
            },
        });
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
                    *title = info.title;
                }
                if !info.paths.is_empty() {
                    *paths = info.paths;
                }
                if info.command.is_some() {
                    *command = info.command;
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
        let summary = summarize(
            kind,
            info.title.as_deref(),
            &info.paths,
            info.command.as_deref(),
        );
        let key = self.next_key();
        let id = info.id;
        if let Some(at) = self.push(Item {
            key,
            body: Body::Tool {
                kind,
                state,
                title: info.title,
                paths: info.paths,
                command: info.command,
                summary,
            },
        }) {
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
        self.push(Item {
            key,
            body: Body::Ended(reason),
        });
    }

    /// 连接在一轮中间结束：没有结果的工具调用是「已取消」。
    pub(crate) fn abort(&mut self) {
        self.end_streaming();
        self.cancel_unfinished();
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
        live.begin_prompt("question", PromptOrigin::User);
        live.agent_text("an ");
        live.agent_text("answer");
        live.tool(tool("t1", Some(CallKind::Read), Some(CallState::Pending)));
        live.agent_text("more");
        live.finish(Stop::EndTurn);
        live.begin_prompt("again", PromptOrigin::User);
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
        live.begin_prompt("q", PromptOrigin::User);
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
        live.begin_prompt("q", PromptOrigin::User);
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
        live.begin_prompt("q", PromptOrigin::User);
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
            live.begin_prompt("q", PromptOrigin::User);
            live.finish(stop);
            assert!(matches!(&live.entries()[1].body, EntryBody::Ended(r) if *r == reason));
        }
        let mut live = Live::default();
        live.begin_prompt("q", PromptOrigin::User);
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
        live.begin_prompt("q", PromptOrigin::User);
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
        live.begin_prompt("q", PromptOrigin::User);
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
        live.begin_prompt("q", PromptOrigin::User);
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
        live.begin_prompt("q", PromptOrigin::User);
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
}
