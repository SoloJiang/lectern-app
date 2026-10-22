//! 审阅快照：文件、已看状态、归因与改动叙事（docs/design/review.md「接口」）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Brief, Group, GroupKind, NoiseKind, ReviewId, ReviewTarget};
use crate::changes::{ChangeError, ChangePath, ChangeTotals, FileChange};
use crate::project::{BlobId, RelPath};
use crate::session::{Agent, ContentRef, SessionKey, Timestamp, ToolCallId, ToolStatus, TurnId};

/// 一个审阅的不可变快照。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ReviewSnapshot {
    pub id: ReviewId,
    pub target: ReviewTarget,
    /// 每次替换加一，与 `ReviewEvent::Changed` 一致。
    pub generation: u64,
    pub state: LoadState,
    /// 会话与比较范围的基准；`Loading` 时为 None。
    pub base: Option<BaseInfo>,
    /// 下标 0 是审阅对象的会话，其后是它在同一 worktree 里的子 agent 会话；其余审阅
    /// 对象为空。
    pub sessions: Arc<[SessionRef]>,
    /// 进度列表。会话审阅：改动集里本审阅对象声明的文件，按叙事里第一次出现的顺序，
    /// 噪声文件在最后；会话外的改动：没有声明的文件；比较范围：全部文件。后两者按
    /// 分组、路径。
    pub files: Arc<[FileEntry]>,
    /// 改动集里不在 `files` 里的文件数（会话审阅：别的会话或会话外的改动）。
    pub unclaimed: u32,
    /// 与 `files` 一一对应。单独一个数组，勾选时只换这一份。
    pub seen: Arc<[SeenState]>,
    pub progress: Progress,
    /// 按轮次的改动叙事。会话日志认不出、或审阅对象不是会话时为 None，视图按
    /// `groups` 显示。
    pub turns: Option<Arc<[TurnEntry]>>,
    pub groups: Arc<[Group]>,
    pub brief: Arc<Brief>,
    /// 给 `CodeIntel::set_focus` 的两类焦点（UTF-8 的路径，相对 worktree 根）：改到的
    /// 文件按叙事顺序；会话写过、读过的文件。由 `review_view` 调用。
    pub focus_review: Arc<[RelPath]>,
    pub focus_touched: Arc<[RelPath]>,
    pub totals: ChangeTotals,
    pub degraded: Option<Degraded>,
    /// #22 加入；本 issue 里总是空的。
    pub notes: Arc<[Note]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum LoadState {
    Loading,
    Ready,
    Failed(ReviewError),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ReviewError {
    /// 会话不在列表里（已删除），或所在 worktree 已不存在。
    Missing,
    /// 会话没有归位到已打开的项目。
    NotInOpenProject,
    Changes(ChangeError),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Degraded {
    /// 会话日志认不出格式（`SessionError::Unrecognized`）：没有轮次，按文件显示。
    UnrecognizedSession,
    /// 文件夹项目没有基准：只显示轮次和调用记录里的改动，没有进度和 Drifted。
    FolderProject,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct BaseInfo {
    /// None：还没有提交（空树）。
    pub commit: Option<BlobId>,
    pub how: BaseHow,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum BaseHow {
    /// 标注日志里固定下来的。
    Recorded,
    /// reflog 里会话开始时的记录。
    Reflog,
    /// reflog 的记录都晚于会话开始，取最早一条的旧值。
    BeforeLog,
    /// 没有 reflog，取当前 HEAD：会话里的提交不在改动集里，视图提示「基准可能不准」。
    CurrentHead,
    /// 会话外的改动、比较范围：按 `BaseSpec`。
    Spec,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SessionRef {
    pub key: SessionKey,
    pub agent: Agent,
    /// 子 agent：父会话里派出它的那一轮和调用；父会话的轮次里找不到这次调用时为 None，
    /// 按时间归到轮次。
    pub spawned_by: Option<(TurnRef, ToolCallId)>,
}

/// 一个文件在快照里的稳定标识，跨代不变，视图按它保留滚动位置与选中项。
/// 新增 (None, 新路径)；删除 (旧路径, None)；修改、类型变化、冲突 (路径, 路径)；
/// 重命名 (来源, 目标)。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct FileKey {
    pub old: Option<ChangePath>,
    pub new: Option<ChangePath>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub key: FileKey,
    pub change: FileChange,
    pub noise: Option<NoiseKind>,
    pub group: GroupKind,
    /// 所在的子项目（`PackageMatch::Found`）；没有子项目或路径不是 UTF-8 时为 None。
    pub package: Option<RelPath>,
    pub attribution: FileAttribution,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SeenState {
    Unseen,
    Seen,
    /// 审过之后又被改过：同一审阅对象里同一个改动的已看标记，键里的值有了变化。
    /// `since` 是那次标记的时刻。
    ChangedSinceSeen {
        since: Timestamp,
    },
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Progress {
    pub total: u32,
    pub seen: u32,
    pub changed_since_seen: u32,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FileAttribution {
    pub state: AttributionState,
    /// 改过这个文件的轮次（审阅对象内），按叙事顺序。
    pub turns: Arc<[TurnRef]>,
    /// 命令期间改动（文件级）：运行那条命令的调用。
    pub commands: Arc<[CallRef]>,
    /// 同一路径上其他会话的改动（行来源指向它们的行）。
    pub other_sessions: Arc<[SessionKey]>,
    /// 至少有一行没有来源的 hunk 数。
    pub drifted_hunks: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum AttributionState {
    /// 还在计算（后台按叙事顺序逐个文件进行）。
    Pending,
    /// 逐行来源已算出，hunk 的归因见 `FileRows::hunks`。
    Lines,
    /// 只有文件级的归因：二进制、超过改动链的上限、工作区一侧没有保留内容等。
    FileOnly,
    /// 没有任何会话的改动步或命令声明这个文件。
    None,
}

/// `session` 是 `ReviewSnapshot::sessions` 的下标，`ordinal` 是那个会话的
/// `Turn::ordinal`。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct TurnRef {
    pub session: u16,
    pub ordinal: u32,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct CallRef {
    pub turn: TurnRef,
    pub call: ToolCallId,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TurnEntry {
    pub at: TurnRef,
    pub id: TurnId,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub prompt: Option<TextKey>,
    pub prompt_preview: Arc<str>,
    pub items: Arc<[NarrativeItem]>,
    /// 这一轮在这个审阅对象最近一次已看标记之后开始（「回到第 3 步只看新轮次」）。
    pub after_last_seen: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum NarrativeItem {
    AgentText(TextKey),
    Injected(TextKey),
    /// 这一轮对一个文件的改动；同一轮对同一文件的多次调用合成一步。
    Step(StepEntry),
    /// 命令期间被修改的文件（文件级，见「声明」）。
    Command {
        call: ToolCallId,
        name: Arc<str>,
        files: Arc<[FileKey]>,
    },
    /// 在这里派出的子 agent；它的轮次紧跟在这一轮之后，`TurnRef::session` 是它的下标。
    Subagent {
        session: u16,
        call: ToolCallId,
    },
    /// 其余工具调用，一行摘要。
    Tool {
        call: ToolCallId,
        name: Arc<str>,
        status: Option<ToolStatus>,
    },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct StepEntry {
    pub step: StepId,
    pub file: FileKey,
    pub calls: Arc<[ToolCallId]>,
    pub added: u32,
    pub removed: u32,
    pub alignment: Alignment,
    /// 这一步加入的行在工作区里还剩多少。
    pub survival: Survival,
}

/// 改动叙事里的一步，在一个审阅的各代快照之间稳定。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct StepId(pub u32);

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Alignment {
    Aligned,
    /// 其中有调用应用不上（Drifted），这些调用的改动按日志原文显示。
    Partial {
        failed: Arc<[ToolCallId]>,
    },
    /// 全部应用不上，或这个文件超过改动链的上限：只能按日志原文显示。
    Unaligned,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Survival {
    All,
    Partly,
    None,
}

/// prompt 与说明的正文：哪个会话的哪一段。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TextKey {
    pub session: u16,
    pub at: ContentRef,
}

/// 审阅意见。字段由 #22 定义；本 issue 里是空结构，`ReviewSnapshot::notes` 总为空。
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Note {}
