//! 用量快照与额度快照（`Usage::snapshot`、`Usage::limits`，docs/design/usage.md「接口」）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{AliasId, LocalDate, MrRef, Tokens, UsageProjectId, Window};
use crate::project::{ProjectKey, WorktreeKey};
use crate::session::{Agent, SessionKey, Timestamp, TurnId};

/// 一天按 agent 的 token 合计（`ProjectUsage::daily`、`AllUsage::daily` 的一项）。
pub type DayTotals = Arc<[(Agent, u64)]>;

/// 一个 `UsageQuery` 的结果，变体与查询一一对应。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UsageSnapshot {
    Project(ProjectUsage),
    All(AllUsage),
    Worktree(WorktreeUsage),
    Session(SessionUsage),
    Mr(MrUsage),
    History(HistoryUsage),
}

/// 每份快照都带。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SnapshotHeader {
    /// 每次替换加一，与 `UsageEvent::Changed` 一致。
    pub generation: u64,
    pub state: UsageState,
    /// 合计里有 `inheritance_unknown` 的会话（agents 设计「子 agent 与继承前缀」），
    /// 可能含继承的前缀。
    pub may_include_inherited: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UsageState {
    Loading,
    Ready,
    /// 历史还在补读：已读、合计的会话日志字节数（只算本次运行开始时已知的文件）。
    Backfilling {
        done: u64,
        total: u64,
    },
    Degraded(UsageDegraded),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UsageDegraded {
    /// 数据库损坏：原文件已改名保留，读得出的行已复制进新库，其余从现存的会话日志重读；
    /// 日志已被删除、行又没救出来的用量不在其中。
    Recovered { at: Timestamp },
    /// 数据库是更新的版本写的：只读，不再入库。
    NewerDatabase,
    /// 数据库写不进（磁盘满、目录不可写）。附原因。
    Unwritable(Arc<str>),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: UsageProjectId,
    /// 显示名：主 worktree 根的目录名，文件夹为目录名（与 `ProjectSnapshot::name` 相同的
    /// 规则）。
    pub name: Arc<str>,
    pub folder: bool,
    /// 项目在本次运行中打开着时为 Some。
    pub key: Option<ProjectKey>,
    pub status: ProjectStatus,
    /// 用户确认「是新项目」（`UsageCommand::ConfirmNew`）之后为 Some，撤销后为 None：
    /// 「是新项目」之后项目是 `Active`，撤销要的别名只能从这里拿到。
    pub confirmed: Option<AliasId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ProjectStatus {
    Active,
    /// 记下的路径已不存在或已是别的仓库：Usage 页提供「合并到…」。
    PathMissing,
    /// 临时项目：Usage 页提供「是同一个项目」（并入 `candidate`）与「是新项目」。
    Temporary {
        reason: TemporaryReason,
        candidate: Option<UsageProjectId>,
    },
    /// 已并入另一个项目；`alias` 用于撤销。
    Merged {
        into: UsageProjectId,
        alias: AliasId,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum TemporaryReason {
    /// 根提交变了，两边都没有远端和代码平台仓库 ID。
    LocalRewrite,
    /// 根提交相同、两边都没有仓库 ID，主远端地址变了。
    PrimaryRemoteChanged,
    /// 已知项目有仓库 ID，这次还没有取到。
    ForgeIdPending,
    /// 仓库还没有提交。
    Unborn,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ProjectUsage {
    pub header: SnapshotHeader,
    pub project: ProjectInfo,
    pub window: Window,
    /// 窗口内的本地日期，从早到今天。
    pub days: Arc<[LocalDate]>,
    pub total: Tokens,
    /// 按 MR / 分支 / 「不属于任何 MR」的行，各行相加等于 `total`（每个类别分别按最大
    /// 余数法取整）。普通文件夹为空。
    pub rows: Arc<[UsageRow]>,
    /// 按模型、按 agent，各自相加等于 `total`。
    pub by_model: Arc<[(Agent, Arc<str>, Tokens)]>,
    pub by_agent: Arc<[(Agent, Tokens)]>,
    /// 与 `days` 一一对应：每天按 agent 的 token 合计（柱状图按 agent 堆叠），每天各项
    /// 相加等于那天的合计。
    pub daily: Arc<[DayTotals]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct UsageRow {
    pub key: RowKey,
    pub tokens: Tokens,
    /// `tokens` 里 Lectern 自己的分析（「审阅」）的部分；其余是「编写」。
    pub review: Tokens,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RowKey {
    /// 同一次使用上统计区间互相重叠的几个 MR，合为一行（P3）。
    Mrs {
        mrs: Arc<[MrLabel]>,
        branch: BranchLabel,
    },
    /// 一次使用里不在任何 MR 区间内的部分；P1、P2 是整次使用，界面标为「分支用量」。
    Branch { use_id: UseId, branch: BranchLabel },
    /// 默认分支、detached HEAD 和无法确定分支的用量；`unattributable` 是其中「无法归属」
    /// 的部分。
    Unassigned { unattributable: Tokens },
}

/// 一次使用的标识：它最早的片段的 ID。片段合并的结果变化时可能变化，视图只用来区分行。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct UseId(pub [u8; 16]);

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct BranchLabel {
    /// 这次使用最后的分支名（改名后的名字），如 "refs/heads/feat"。
    pub name: Arc<str>,
    /// 现在检出它的 worktree。
    pub worktree: Option<WorktreeKey>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MrLabel {
    pub mr: MrRef,
    pub title: Arc<str>,
    pub state: MrState,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum MrState {
    Open,
    Merged,
    Closed,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct AllUsage {
    pub header: SnapshotHeader,
    pub window: Window,
    pub days: Arc<[LocalDate]>,
    pub total: Tokens,
    /// 每个项目一行（并入别的项目的不单列），加上 `other`，相加等于 `total`。
    pub projects: Arc<[(ProjectInfo, Tokens)]>,
    /// 经用户命令（`Merge`、`ConfirmSame`）并入别的项目、还没有撤销的项目，状态都是
    /// `Merged { into, alias }`，按合并时间从新到旧。它们不在 `projects` 里、不带 token
    /// （用量已计入 `into`），所以 `projects` 加 `other` 仍等于 `total`。自动的别名（移动、
    /// 改写）不列出。
    pub merged: Arc<[ProjectInfo]>,
    /// 「其他」：还没有打开过的项目和不在任何项目里的用量。
    pub other: Tokens,
    pub daily: Arc<[DayTotals]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct WorktreeUsage {
    pub header: SnapshotHeader,
    /// 当前分支（detached 时为 None）。
    pub branch: Option<BranchLabel>,
    /// P3 起：这个分支当前所在的 MR 区间里的 MR。
    pub mrs: Arc<[MrLabel]>,
    /// 当前这次使用（P3 起为 MR 区间）的累计；默认分支和 detached 时为 None。
    pub cumulative: Option<Tokens>,
    pub review: Tokens,
    pub since: Option<Timestamp>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SessionUsage {
    pub header: SnapshotHeader,
    /// 本会话加全部子 agent 会话的合计（ADR 0014）。
    pub total: Tokens,
    /// 本会话改到的每个分支（P3 起每个 MR 一行；一个分支同时对应几个 MR 时各占一行，
    /// `shared` 为 true）。
    pub rows: Arc<[SessionRow]>,
    /// 本会话自己的每轮 token，不分摊（ADR 0020「界面位置」）。不含子 agent 会话。
    pub turns: Arc<[(TurnId, Tokens)]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SessionRow {
    pub key: RowKey,
    pub worktree: Option<WorktreeKey>,
    /// 这个分支（或 MR）的累计。
    pub cumulative: Tokens,
    /// 其中本会话（含子 agent）的那一份。
    pub session_part: Tokens,
    pub shared: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MrUsage {
    pub header: SnapshotHeader,
    pub mr: MrLabel,
    /// 统计区间（usage 设计「MR 区间」）；还开着时 `end` 为 None。
    pub start: Timestamp,
    pub end: Option<Timestamp>,
    pub write: Tokens,
    pub review: Tokens,
    /// 与这个 MR 区间重叠、共用同一次使用的其他 MR。
    pub shared_with: Arc<[MrRef]>,
    /// 按会话，每个会话带它分到这个 MR 的每轮 token（一轮被分摊到几个 MR 时是分到这里
    /// 的那一份）。
    pub sessions: Arc<[MrSessionRow]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MrSessionRow {
    pub session: SessionKey,
    pub tokens: Tokens,
    pub turns: Arc<[(TurnId, Tokens)]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct HistoryUsage {
    pub header: SnapshotHeader,
    pub project: ProjectInfo,
    /// 每个本地年月一行：热窗口里的月份按事实行求和，更早的取冷数据的按月汇总。
    pub months: Arc<[((i32, u8), Tokens)]>,
    /// 查询的 `days` 为 true 时的按日明细（冷的部分按记录时间戳和当前时区重新分日）。
    pub days: Option<Arc<[(LocalDate, Tokens)]>>,
}

/// 额度快照。`Default` 是数据库还没打开时的空快照。
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct Limits {
    pub generation: u64,
    pub windows: Arc<[LimitWindow]>,
    /// `windows` 里 `used_percent` 最高、没有过期的一个（侧栏底部的进度条）。
    pub tightest: Option<u16>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct LimitWindow {
    pub agent: Agent,
    pub source: LimitSource,
    /// Codex 的 `limit_id`；statusline 没有时为 None。
    pub limit_id: Option<Arc<str>>,
    pub kind: LimitKind,
    pub window_minutes: Option<u32>,
    pub used_percent: f32,
    pub resets_at: Option<Timestamp>,
    pub sampled_at: Timestamp,
    /// 已过 `resets_at`，之后没有新的样本（usage 设计「未决问题」1）。
    pub expired: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum LimitSource {
    SessionLog,
    Statusline,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum LimitKind {
    Primary,
    Secondary,
    FiveHour,
    SevenDay,
}
