//! 历史窗口：会话的最近几轮，从轮次索引和正文读成对话项
//! （docs/design/agents.md「ACP 客户端」的「历史窗口」）。

use std::path::PathBuf;
use std::sync::Arc;

use lectern_services::conversation::{
    CallKind, CallState, Entry, EntryBody, EntryKey, HistoryError, Passage, PromptOrigin, ToolEntry,
};
use lectern_services::session::{
    Content, ContentRef, SessionError, SessionKey, SessionSource, ToolCall, ToolKind, ToolStatus,
    Turn, TurnItem,
};

/// 单段正文的上限。
const PASSAGE_MAX: usize = 64 << 10;
/// 窗口里正文的合计上限。
const WINDOW_MAX: usize = 1 << 20;
/// 工具行 `summary` 的字符数上限。
const SUMMARY_CHARS: usize = 160;
/// `Edit`、`Read` 的 `summary` 里最多列出的文件数。
const SUMMARY_FILES: usize = 3;

/// 一次读出的窗口。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct Window {
    pub(crate) entries: Arc<[Entry]>,
    /// 窗口之前还有多少轮。
    pub(crate) earlier: u32,
}

/// 读最近 `wanted` 轮。阻塞（读会话文件），只在 executor 上调用。
///
/// 从最新的一轮往前读，合计正文超过 1 MiB 时停下，剩下的轮数计入 `earlier`。最新的一轮自己超过上限时
/// 去掉它开头的项。
pub(crate) fn read_window(
    source: &dyn SessionSource,
    key: &SessionKey,
    wanted: u32,
) -> Result<Window, HistoryError> {
    let index = source.turns(key).map_err(history_error)?;
    let turns = &index.turns;
    let take = usize::try_from(wanted)
        .unwrap_or(usize::MAX)
        .min(turns.len());
    let first = turns.len() - take;
    let mut earlier = first;
    // 每一轮的项，从最新的一轮起。
    let mut kept: Vec<Vec<Entry>> = Vec::new();
    let mut bytes = 0usize;
    for (at, turn) in turns[first..].iter().enumerate().rev() {
        let (entries, size) = turn_entries(source, key, turn)?;
        if !kept.is_empty() && bytes + size > WINDOW_MAX {
            earlier += at + 1;
            break;
        }
        bytes += size;
        kept.push(entries);
    }
    kept.reverse();
    let mut entries: Vec<Entry> = kept.into_iter().flatten().collect();
    if bytes > WINDOW_MAX {
        trim_front(&mut entries, bytes);
    }
    Ok(Window {
        entries: entries.into(),
        earlier: u32::try_from(earlier).unwrap_or(u32::MAX),
    })
}

/// 去掉开头的项，直到正文合计不超过上限。
fn trim_front(entries: &mut Vec<Entry>, mut bytes: usize) {
    let mut drop = 0;
    while bytes > WINDOW_MAX && drop + 1 < entries.len() {
        bytes -= text_len(&entries[drop]);
        drop += 1;
    }
    entries.drain(..drop);
}

fn text_len(entry: &Entry) -> usize {
    match &entry.body {
        EntryBody::Prompt { text, .. }
        | EntryBody::AgentText { text, .. }
        | EntryBody::Injected { text } => text.text.len(),
        EntryBody::Tool(_) | EntryBody::Ended(_) => 0,
    }
}

/// 一轮的项和它们的正文字节数。
fn turn_entries(
    source: &dyn SessionSource,
    key: &SessionKey,
    turn: &Turn,
) -> Result<(Vec<Entry>, usize), HistoryError> {
    let mut entries = Vec::with_capacity(turn.items.len() + 1);
    let mut bytes = 0usize;
    let mut push = |n: usize, body: EntryBody| {
        entries.push(Entry {
            key: EntryKey(format!("h:{}:{n}", turn.ordinal).into()),
            body,
        });
    };
    if let Some(prompt) = &turn.prompt
        && let Some(text) = load_text(source, key, prompt)?
    {
        bytes += text.text.len();
        push(
            0,
            EntryBody::Prompt {
                text,
                origin: PromptOrigin::User,
            },
        );
    }
    for (n, item) in turn.items.iter().enumerate() {
        let n = n + 1;
        match item {
            TurnItem::AgentText(at) => {
                if let Some(text) = load_text(source, key, at)? {
                    bytes += text.text.len();
                    push(
                        n,
                        EntryBody::AgentText {
                            text,
                            streaming: false,
                        },
                    );
                }
            }
            TurnItem::Injected(at) => {
                if let Some(text) = load_text(source, key, at)? {
                    bytes += text.text.len();
                    push(n, EntryBody::Injected { text });
                }
            }
            TurnItem::Tool(call) => push(n, EntryBody::Tool(tool_entry(call, turn))),
        }
    }
    Ok((entries, bytes))
}

/// 正文。不是文字（`Edits`）时为 None。
fn load_text(
    source: &dyn SessionSource,
    key: &SessionKey,
    at: &ContentRef,
) -> Result<Option<Passage>, HistoryError> {
    match source.load(key, at).map_err(history_error)? {
        Content::Text(text) => Ok(Some(passage(&text))),
        Content::Edits(_) => Ok(None),
    }
}

/// 超过 64 KiB 的截断到 64 KiB 以内最后一个完整的字符。
fn passage(text: &str) -> Passage {
    if text.len() <= PASSAGE_MAX {
        return Passage {
            text: text.into(),
            truncated: false,
        };
    }
    let mut end = PASSAGE_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Passage {
        text: text[..end].into(),
        truncated: true,
    }
}

fn tool_entry(call: &ToolCall, turn: &Turn) -> ToolEntry {
    let (kind, summary) = match &call.kind {
        ToolKind::Edit { paths } => (CallKind::Edit, file_names(paths)),
        ToolKind::Read { paths } => (CallKind::Read, file_names(paths)),
        ToolKind::Execute { command, .. } => (
            CallKind::Execute,
            clip(command.lines().next().unwrap_or_default()),
        ),
        ToolKind::Search { .. } => (CallKind::Search, clip(&call.name)),
        ToolKind::SpawnAgent => (CallKind::SpawnAgent, clip(&call.name)),
        ToolKind::Other => (CallKind::Other, clip(&call.name)),
    };
    let state = match call.finished {
        Some((_, ToolStatus::Completed)) => CallState::Completed,
        Some((_, ToolStatus::Failed)) => CallState::Failed,
        Some((_, ToolStatus::Denied)) => CallState::Denied,
        Some((_, ToolStatus::Interrupted)) => CallState::Cancelled,
        None if turn.ended_at.is_none() => CallState::Running,
        None => CallState::Cancelled,
    };
    ToolEntry {
        kind,
        state,
        summary,
    }
}

/// 至多 3 个文件名（不含目录），用「, 」连接。
fn file_names(paths: &[PathBuf]) -> Arc<str> {
    let names: Vec<_> = paths
        .iter()
        .take(SUMMARY_FILES)
        .map(|path| {
            path.file_name()
                .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
        })
        .collect();
    clip(&names.join(", "))
}

/// 至多 160 个字符。
fn clip(text: &str) -> Arc<str> {
    match text.char_indices().nth(SUMMARY_CHARS) {
        Some((end, _)) => text[..end].into(),
        None => text.into(),
    }
}

fn history_error(error: SessionError) -> HistoryError {
    match error {
        SessionError::NotFound => HistoryError::NotFound,
        SessionError::Unrecognized => HistoryError::Unrecognized,
        SessionError::Io(_) | SessionError::TooLarge(_) => HistoryError::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_clipped_by_characters() {
        let long = "é".repeat(400);
        assert_eq!(clip(&long).chars().count(), SUMMARY_CHARS);
        assert_eq!(&*clip("short"), "short");
    }

    #[test]
    fn passages_cut_at_a_character_boundary() {
        // 3-byte characters: 64 KiB is not a multiple of 3.
        let text = "界".repeat(PASSAGE_MAX / 3 + 10);
        let cut = passage(&text);
        assert!(cut.truncated);
        assert!(cut.text.len() <= PASSAGE_MAX);
        assert!(cut.text.len() > PASSAGE_MAX - 3);
        assert!(text.starts_with(&*cut.text));
        assert!(!passage("small").truncated);
    }

    #[test]
    fn file_names_drop_directories_and_stop_at_three() {
        let paths: Vec<PathBuf> = ["/a/b/one.rs", "/a/two.rs", "/three.rs", "/x/four.rs"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(&*file_names(&paths), "one.rs, two.rs, three.rs");
        assert_eq!(&*file_names(&[]), "");
    }
}
