//! 轮次索引的增量构建（docs/design/agents.md「轮次索引」、「数据流与线程」第 7 步）。

use std::sync::Arc;

use lectern_services::session::{
    ContentRef, Cursor, Format, SessionKey, Timestamp, ToolCall, ToolCallId, ToolStatus, Turn,
    TurnId, TurnIndex, TurnItem,
};

/// prompt 预览的字符上限。
pub(crate) const PREVIEW_CHARS: usize = 120;

/// 文本第一行（去掉开头的空白），最多 120 个字符。
pub(crate) fn preview(text: &str) -> Arc<str> {
    let line = text
        .trim_start()
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end();
    match line.char_indices().nth(PREVIEW_CHARS) {
        Some((i, _)) => line[..i].into(),
        None => line.into(),
    }
}

/// 只收自己的记录（不含继承前缀）。已经结束的轮次不再变动，只复制 `Arc`。
#[derive(Clone, Default)]
pub(crate) struct IndexBuilder {
    sealed: Vec<Turn>,
    current: Option<Building>,
    partial: bool,
}

#[derive(Clone)]
struct Building {
    id: TurnId,
    ordinal: u32,
    started_at: Timestamp,
    ended_at: Option<Timestamp>,
    prompt: Option<ContentRef>,
    preview: Arc<str>,
    items: Vec<TurnItem>,
}

impl Building {
    fn seal(&self) -> Turn {
        Turn {
            id: self.id.clone(),
            ordinal: self.ordinal,
            started_at: self.started_at,
            ended_at: self.ended_at,
            prompt: self.prompt,
            prompt_preview: self.preview.clone(),
            items: self.items.clone().into(),
        }
    }
}

impl IndexBuilder {
    /// 从已有的索引继续。
    pub(crate) fn from_index(index: &TurnIndex) -> Self {
        let mut sealed = index.turns.to_vec();
        let current = sealed.pop().map(|t| Building {
            id: t.id,
            ordinal: t.ordinal,
            started_at: t.started_at,
            ended_at: t.ended_at,
            prompt: t.prompt,
            preview: t.prompt_preview,
            items: t.items.to_vec(),
        });
        IndexBuilder {
            sealed,
            current,
            partial: index.format == Format::Partial,
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = IndexBuilder::default();
    }

    /// 有行被跳过。
    pub(crate) fn mark_partial(&mut self) {
        self.partial = true;
    }

    pub(crate) fn start_turn(
        &mut self,
        id: TurnId,
        at: Timestamp,
        prompt: Option<ContentRef>,
        preview: Arc<str>,
    ) {
        let ordinal = match self.current.take() {
            Some(prev) => {
                let ordinal = prev.ordinal + 1;
                self.sealed.push(prev.seal());
                ordinal
            }
            None => 1,
        };
        self.current = Some(Building {
            id,
            ordinal,
            started_at: at,
            ended_at: None,
            prompt,
            preview,
            items: Vec::new(),
        });
    }

    /// 加进当前轮次；还没有轮次时丢弃（没有 prompt 就出现的内容）。
    pub(crate) fn push(&mut self, item: TurnItem) {
        if let Some(turn) = &mut self.current {
            if matches!(item, TurnItem::Tool(_)) {
                turn.ended_at = None;
            }
            turn.items.push(item);
        }
    }

    /// 给工具调用填上结果。先找当前轮次，再从后往前找已结束的轮次；找不到（调用在继承前缀里）时忽略。
    /// 已经由 [`IndexBuilder::attach_result`] 填上的结果引用不被替换；没有结果记录的调用（Codex 的
    /// `local_shell_call`）`result` 为 None。
    pub(crate) fn finish_tool(
        &mut self,
        call: &ToolCallId,
        at: Timestamp,
        status: ToolStatus,
        result: Option<ContentRef>,
    ) {
        self.update_tool(call, |tool| {
            if tool.finished.is_none() {
                tool.finished = Some((at, status));
                if tool.result.is_none() {
                    tool.result = result;
                }
            }
        });
    }

    /// 工具调用的结果引用改指向另一行（Codex 的改动记录：`patch_apply_end`、`FileChange`），
    /// 不论结果是否已经到达。
    pub(crate) fn attach_result(&mut self, call: &ToolCallId, result: ContentRef) {
        self.update_tool(call, |tool| tool.result = Some(result));
    }

    fn update_tool(&mut self, call: &ToolCallId, mut f: impl FnMut(&mut ToolCall)) {
        let mut fill = |items: &mut [TurnItem]| -> bool {
            for item in items.iter_mut().rev() {
                if let TurnItem::Tool(tool) = item
                    && tool.id == *call
                {
                    f(tool);
                    return true;
                }
            }
            false
        };
        if let Some(turn) = &mut self.current
            && fill(&mut turn.items)
        {
            return;
        }
        for turn in self.sealed.iter_mut().rev() {
            let mut items = turn.items.to_vec();
            if fill(&mut items) {
                turn.items = items.into();
                return;
            }
        }
    }

    /// 当前轮次还没有 prompt 时填上（Codex 的 prompt 在 `task_started` 之后才写出）。返回是否填上。
    pub(crate) fn set_prompt(&mut self, prompt: ContentRef, preview: Arc<str>) -> bool {
        match &mut self.current {
            Some(turn) if turn.prompt.is_none() => {
                turn.prompt = Some(prompt);
                turn.preview = preview;
                true
            }
            _ => false,
        }
    }

    /// 明确的轮次结束（Codex 的 `task_complete` / `turn_aborted`）。
    pub(crate) fn close_turn(&mut self, at: Timestamp) {
        if let Some(turn) = &mut self.current {
            turn.ended_at = Some(at);
        }
    }

    /// agent 结束了这一轮的回复：当前轮次里没有还在等结果的工具调用时记下结束时间。
    pub(crate) fn end_turn(&mut self, at: Timestamp) {
        if let Some(turn) = &mut self.current {
            let pending = turn
                .items
                .iter()
                .any(|i| matches!(i, TurnItem::Tool(ToolCall { finished: None, .. })));
            turn.ended_at = if pending { None } else { Some(at) };
        }
    }

    /// 第一个有 prompt 的轮次的预览。
    pub(crate) fn first_prompt(&self) -> Option<Arc<str>> {
        self.sealed
            .iter()
            .find(|t| t.prompt.is_some())
            .map(|t| t.prompt_preview.clone())
            .or_else(|| {
                self.current
                    .as_ref()
                    .filter(|t| t.prompt.is_some())
                    .map(|t| t.preview.clone())
            })
    }

    pub(crate) fn build(&self, session: SessionKey, cursor: Cursor) -> TurnIndex {
        let mut turns = self.sealed.clone();
        turns.extend(self.current.as_ref().map(Building::seal));
        TurnIndex {
            session,
            cursor,
            turns: turns.into(),
            format: if self.partial {
                Format::Partial
            } else {
                Format::Known
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_takes_the_first_line_up_to_120_chars() {
        assert_eq!(&*preview("\n  hello\nworld"), "hello");
        assert_eq!(&*preview(""), "");
        let long: String = "é".repeat(200);
        assert_eq!(preview(&long).chars().count(), PREVIEW_CHARS);
    }
}
