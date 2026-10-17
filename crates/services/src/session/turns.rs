//! 轮次索引与按引用读取的正文（docs/design/agents.md「轮次索引」）。

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Cursor, Format, SessionKey, Timestamp, ToolCallId, ToolKind, ToolStatus, TurnId};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TurnIndex {
    pub session: SessionKey,
    /// 索引解析到的位置；`turns` 下次从这里继续。
    pub cursor: Cursor,
    pub turns: Arc<[Turn]>,
    pub format: Format,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    /// 从 1 开始，按出现顺序；不含继承前缀里的轮次。
    pub ordinal: u32,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub prompt: Option<ContentRef>,
    /// prompt 的第一行，最多 120 个字符，列表直接显示，不用 `load`。
    pub prompt_preview: Arc<str>,
    pub items: Arc<[TurnItem]>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TurnItem {
    /// agent 的说明文字。思考内容不进入索引。
    AgentText(ContentRef),
    /// 不是人写的输入：后台任务通知、其他 agent 发来的消息、hook 注入的内容。
    Injected(ContentRef),
    Tool(ToolCall),
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: Arc<str>,
    pub kind: ToolKind,
    pub started_at: Timestamp,
    pub finished: Option<(Timestamp, ToolStatus)>,
    pub input: ContentRef,
    pub result: Option<ContentRef>,
}

/// 指向文件里的一行和行内的一个部分。不透明：消费方只把它交回 `SessionSource::load`。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ContentRef {
    pos: u64,
    len: u32,
    part: u16,
}

impl ContentRef {
    /// 提供者用：从 `pos` 开始、`len` 字节的一行里的第 `part` 个部分。
    pub fn new(pos: u64, len: u32, part: u16) -> ContentRef {
        ContentRef { pos, len, part }
    }

    /// 行的字节偏移。
    pub fn pos(&self) -> u64 {
        self.pos
    }

    /// 行的字节数。
    pub fn line_len(&self) -> u32 {
        self.len
    }

    /// 行内的部分序号。
    pub fn part(&self) -> u16 {
        self.part
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Content {
    Text(Arc<str>),
    /// `Edit` 类工具的改动，`review` 据此归因。
    Edits(Arc<[FileEdit]>),
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileEdit {
    pub path: PathBuf,
    pub op: EditOp,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum EditOp {
    /// Claude Code `Write`：结果里 `originalFile` 为 null 时是新建。
    Write {
        content: Arc<str>,
        created: bool,
    },
    /// Claude Code `Edit` / `MultiEdit` 的输入。
    Replace {
        old: Arc<str>,
        new: Arc<str>,
        all: bool,
    },
    /// 统一 diff：Claude Code 的 `structuredPatch`、Codex 的 `apply_patch` 补丁或
    /// `FileChange::Update`。
    Patch {
        hunks: Arc<str>,
        move_to: Option<PathBuf>,
    },
    Add {
        content: Arc<str>,
    },
    Delete,
}
