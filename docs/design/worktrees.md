# worktrees 设计

状态：实现中。前置条件 C（`worktrees.ftl`，#333）、A 的 `services` 部分（#334）与 `review` 部分（#337）、B（#336）已实现；「验收」的第 1 步（快照与插件）已实现（#339），第 2 步（视图）已实现（PR 号待定），第 3 步尚未实现。

对应 issue [#16](https://github.com/SoloJiang/lectern-app/issues/16)。`worktrees` 是并行 agent 总览屏幕（[交互规范「屏幕」](interaction.md#屏幕)的 04 Worktrees，[技术调研](tech-research.md)「需求拆解与 Zed 对照」里的「并行 agent 总览」）：按 Active / Idle 分组列出项目的每个 worktree，每行显示分支、会话、改动量和分支的累计 token；检查器显示所选 worktree 的分支用量、相对主干的改动，以及与其他 worktree 的文件重叠。本 crate 只合成和显示领域层算好的快照（`project`、`agents`、`review`、`usage`），不读会话文件，不调用 git；屏幕和检查器页经 `workspace` 的贡献接口显示（[workspace 设计](workspace.md)）。

## 范围

- 做：
  - Worktrees 屏幕（`Screen(WORKTREES)`）：Active、Idle 两个分组列表。每行：分支标签（detached 时为提交、unborn 时为分支名）；当前、已锁定、已缺失三种标记；会话数与运行中的会话数；未提交的文件数与相对主干的文件数；分支的累计 token。
  - 检查器的 Worktree 页（`InspectorTab("worktree")`，只在本屏幕显示）：所选 worktree 的分支用量、相对主干的改动、文件重叠、最近的会话，以及「审阅改动」「切换到这个 worktree」两个操作。
  - 项目侧栏的 Worktrees 条目（`SidebarEntry("worktrees")`，`Views` 区）：打开 git 项目时登记，项目关闭时注销。
  - 快照 `WorktreesSnapshot`：插件在后台任务里从 `Projects`、`SessionSource`、`Review`、`Usage` 的快照合成，视图只读它。
  - 关注的持有时机：屏幕是当前屏幕时持有 `Review::watch_worktrees` 和每个 worktree 的 `Usage::watch(Worktree)`，屏幕不是当前屏幕时放下。
  - 导航：↑ ↓ 选择 worktree；点击或 ↵ 进入「相对主干」的审阅；检查器里的会话进入会话审阅；切换当前 worktree；`Milestone::WorktreeSwitched` 的报告。
  - Fluent 文案、UI 探针的语义节点、Action。
- 不做：
  - worktree 列表、HEAD、当前 worktree 的切换：`project`（[project 设计「快照」](project.md#快照)）。
  - 改动计数、merge-base、文件重叠的计算：`review`（「对其他 crate 的要求」A）。本 crate 不比较工作区，不读 git 对象。
  - 会话的归位、`open_turn` 与 `last_activity`：`agents`。
  - token 统计与 MR 区间：`usage`（[ADR 0020](../adr/0020.md)）。
  - 检查器容器、分段控件、导航栈、Esc 与 ⌘[：`workspace`。
  - 「相对主干」审阅的内容：`review_view`（`ReviewTarget::Range`）。
  - 颜色、字体、控件与动效原语：`ui`（[ADR 0015](../adr/0015.md)）。
- 本 issue 之外：
  - MR 用量（P3）：`WorktreeUsage::mrs` 要到 forge（[#25](https://github.com/SoloJiang/lectern-app/issues/25)）之后才有内容。本文给出 MR 的显示规则（「界面」），数据到达之前只显示分支用量。
  - 创建、删除、prune worktree：P1 只读。
  - 在某个 worktree 里启动新会话：归 `terminal`。
  - 非当前 worktree 的文件监听：`fs_watch` 只监听当前 worktree 的工作区文件（[fs_watch 设计](fs_watch.md)），所以非当前 worktree 的计数按事件刷新（「已知限制」）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui` | 视图、Action、一次性定时（Active 到期，见「数据流与线程」）、`#[gpui::test]` | 已锁定（[ADR 0016](../adr/0016.md)） |
| `lectern-ui` | `colors`、`strings`、`Text`、`TextRole`、`GroupedList`、`Row`、`AgentDot`、`Button`、`Skeleton`、`Expander`、`loading_phase`、`motion`、`semantic`、`contrib` 的载荷类型 | 视图层的公共库，非插件 |
| `lectern-i18n` | `keys::worktrees::*`、`Args`、`count_compact`、`Strings::relative_time` | [ADR 0013](../adr/0013.md) |
| `lectern-kernel`、`lectern-services` | `Plugin`、`Context`、`PluginHandle`、`ServiceSet`、`Clock`；`dyn Projects`、`dyn SessionSource`、`dyn Review`、`dyn Usage`、`dyn Contributions` 与它们的数据类型和事件 | 架构规则 7（[ADR 0025](../adr/0025.md)）：插件只经 `services` 往来 |
| `lectern-executor` | 合成快照的 `Background` 任务 | 后台任务只经 executor |
| `lectern-logging` | 日志宏与类型化字段 | [ADR 0046](../adr/0046.md) |
| `futures`（工作区依赖，已有） | 视图的唤醒通道 `channel::mpsc::unbounded` | 与 `workspace`、`project_home` 相同 |
| `lectern-kernel`、`lectern-executor`、`lectern-logging` 的 `test-support`（dev）、`insta`（dev）、`gpui` 的 `test-support`（dev） | `TestKernel`、`ManualClock`、`Executor::manual`、`testing::capture`、快照与语义树的 insta | |

本 crate 是插件 crate（`[package.metadata.lectern] plugin = true`，第 1 步加上），含两个插件：`worktrees`（requires `dyn Projects`、`dyn SessionSource`、`dyn Review`、`dyn Contributions`）和 `worktrees-usage`（requires `dyn Usage`）。它不依赖 `project`、`agents`、`review`、`usage`、`workspace`、`project_home` 这些插件 crate（dev-dependency 也不行，`scripts/check_architecture.py`），也不依赖 `lectern-git`；只有 `app` 依赖它。测试用的假服务在本 crate 的 `testing` 模块里（「测试」）。所在层与依赖方向见 [crates/worktrees/AGENTS.md](../../crates/worktrees/AGENTS.md)。

本设计不改变架构、依赖选型和性能预算，不另写 ADR：新的计算放在 `review` 里，按已有的接口约定（关注与快照）提供；采样状态里本 crate 的常驻 < 16 KiB，`review` 保留的部分在它自己的 < 1 MB 份额里（「性能」）。

## 接口

### 本 crate 的公开项

```rust
/// `app` 装配时构造，放进 `PluginSet`（id "worktrees"）。
pub struct WorktreesPlugin { /* deps */ }

pub struct WorktreesDeps {
    pub executor: Executor,
    /// 与 `WorktreesUsagePlugin` 共用的一格（见下）。`app` 构造一个，交给两个插件。
    pub usage: UsageLink,
}

impl WorktreesPlugin {
    /// 不做 IO。
    pub fn new(deps: WorktreesDeps) -> Self;
}

// Manifest { provides: [],
//            requires: [service::<dyn Projects>(), service::<dyn SessionSource>(), service::<dyn Review>(),
//                       service::<dyn Contributions>()],
//            activation: [AfterFirstFrame] }
// activate：取服务引用（`service`、`service_set`），建空的 `Hub`；经 `dyn Contributions` 登记 `Screen(WORKTREES)`、
//   `InspectorTab("worktree")`（`screen: Some(WORKTREES)`，「对其他 crate 的要求」B）、`KeyBindings`（`register(…, None)`，
//   守卫交给 `ctx.own_staged`）；`ctx.on::<ProjectEvent>`、`ctx.on::<SessionsChanged>`、`ctx.on::<ReviewEvent>`、
//   `ctx.on::<WorkspaceEvent>`。不做 IO，不提交任务。侧栏条目不在这里登记：它只在打开 git 项目时存在。
// started：保存 `PluginHandle`；把「标记 dirty 并安排合成」的回调交给 `UsageLink::set_on_change`；读一次
//   `Projects::open_projects()`，已有打开的项目时按 `Opened` 处理。`started` 与 token 变为 `Live` 之间漏掉的 `Opened`
//   按 project_home 的规则补上：收到不认识的项目的 `ProjectEvent::Changed`、且 `Projects::snapshot` 里还有它时按 `Opened` 处理。

/// `app` 装配时构造，放进 `PluginSet`（id "worktrees-usage"）。只负责把 `dyn Usage` 放进共用的一格：
/// `usage` 插件失败或停用时屏幕照常显示，只是没有 token（kernel 没有可选依赖，与 project_home 的
/// "project-home-usage" 相同）。
pub struct WorktreesUsagePlugin { /* link */ }
impl WorktreesUsagePlugin {
    /// `executor` 运行 `started` 里提交的对账任务。
    pub fn new(executor: Executor, link: UsageLink) -> Self;
}

// Manifest { provides: [], requires: [service::<dyn Usage>()], activation: [AfterFirstFrame] }
// activate：取 `ServiceRef<dyn Usage>`，`ctx.on::<UsageEvent>`（`Changed { query, .. }` 调 `UsageLink::changed(&query)`）。
// started：`UsageLink::set_usage(service, token)` 只记录，然后经 `handle.own_with` 提交一个只调 `reconcile()` 的任务，
//   它在 token 变为 `Live` 之后运行（kernel 设计「激活时的上下文」）。
// 停用：`Active` drop 时 `UsageLink::clear_usage()`：放下全部关注，主插件重建一次快照（token 消失）。

/// 两个插件共用的一格。规则与 project_home 的 `UsageSlot` 相同（两个插件激活先后不定，谁后到谁对账），
/// 区别是这里关注一组查询：屏幕是当前屏幕时，每个不是 prunable 的 worktree 一个 `UsageQuery::Worktree`，
/// P3 起 `WorktreeUsage::mrs` 多于一项的 worktree 再为每个 MR 加一个 `UsageQuery::Mr`。
#[derive(Clone, Default)]
pub struct UsageLink { /* Arc<Mutex<{ usage: Option<(ServiceRef<dyn Usage>, ActivationToken)>,
                          wanted: Arc<[UsageQuery]>, interests: Vec<(UsageQuery, UsageInterest)>,
                          on_change: Option<Arc<dyn Fn() + Send + Sync>> }>> */ }
impl UsageLink {
    pub fn set_usage(&self, usage: ServiceRef<dyn Usage>, token: ActivationToken); // usage 插件 started：只记录
    pub fn clear_usage(&self);                                                      // usage 插件停用；之后 reconcile
    /// 主插件在每次合成之后交出这一份快照需要的查询；屏幕不是当前屏幕时交空集合。之后 reconcile。
    pub fn set_wanted(&self, queries: Arc<[UsageQuery]>);
    pub fn set_on_change(&self, f: Arc<dyn Fn() + Send + Sync>);                    // 主插件 started
    /// `UsageEvent::Changed` 的监听：`query` 在 `wanted` 里才调 `on_change`。
    pub fn changed(&self, query: &UsageQuery);
    /// 对账：`wanted` 里没有关注的查询在 `token.enter()` 内调 `Usage::watch`，不在 `wanted` 里的关注 drop。
    /// 只在关注的集合真的变了（新取了关注，或 usage 消失）时调 `on_change`，所以「合成 → set_wanted → 合成」不会循环。
    /// 任何线程，不阻塞；在锁外调 `watch` 和 drop 旧关注。
    pub fn reconcile(&self);
    /// 合成任务用：`token.enter()` 成功时返回服务引用与准入，否则 None。
    pub fn usage(&self) -> Option<(ServiceRef<dyn Usage>, Admission)>;
}
```

- `activate`、`started`：kernel 线程，只登记，不阻塞。
- 事件监听：executor 线程，只改 `Hub` 的标记、安排一次合成任务（「数据流与线程」）。
- 视图工厂（`Screen::build`、`InspectorTab::build`）：主线程，由 `workspace` 在压栈、选中检查器页时调用；不做 IO。

### 视图读取的快照

```rust
/// 不可变，整体替换。由 "worktrees" 插件在 executor 的 `Background` 任务里合成，视图只持有 `Arc`。
/// 不含 GPUI 的元素与格式化好的文字：文字在渲染时由 `strings(cx)` 生成；`NodeId` 随快照建一次。
pub struct WorktreesSnapshot {
    pub project: ProjectKey,
    /// 合成时各来源的代数，用来丢弃过时的任务结果。
    pub sources: SourceGenerations,
    /// 合成时刻（kernel 的 `Clock`，测试用 `ManualClock`）。
    pub built_at: Timestamp,
    /// `ProjectSnapshot::active.worktree`。
    pub active: Option<WorktreeKey>,
    /// `ProjectSnapshot::active.files` 已不是 `Pending`：里程碑的条件（「性能」）。
    pub files_settled: bool,
    /// 插件看到的当前 worktree 变化次数（`ProjectEvent::Changed` 的 `parts.active`）。里程碑按它判断。
    pub switches: u64,
    /// 主干。`WorktreesOverview` 还没有到达，或仓库没有主干时为 None。
    pub base: Option<BaseRef>,
    pub active_rows: Arc<[WorktreeRow]>,
    pub idle_rows: Arc<[WorktreeRow]>,
    /// 运行中的会话最早在什么时刻不再算运行中：各会话 `last_activity + 10 min` 的最小值。没有运行中的会话时为 None。
    pub next_expiry: Option<Timestamp>,
    /// 项目是文件夹项目（没有 worktree）：屏幕显示空状态。
    pub folder: bool,
}

pub struct SourceGenerations {
    pub project: u64,
    pub sessions: Arc<[(SourceId, u64)]>,
    pub changes: Option<u64>,      // `WorktreesOverview::generation`
    pub usage: u64,                // `UsageLink` 的变化计数
}

pub struct BaseRef {
    /// 完整引用名（`refs/remotes/origin/main`），原样交给 `BaseSpec::MergeBase`。
    pub full: Arc<str>,
    /// 显示用：去掉 `refs/heads/` 或 `refs/remotes/`（`origin/main`）。
    pub short: Arc<str>,
}

pub struct WorktreeRow {
    pub key: WorktreeKey,
    /// `Worktree::root`，检查器原样显示。
    pub root: Arc<Path>,
    pub head: HeadLabel,
    pub is_active: bool,
    pub is_main: bool,
    pub locked: bool,
    pub prunable: bool,
    /// 最近 14 天里计入这一行的会话数，与其中运行中的数目（规则见下表）。
    pub sessions: u32,
    pub running: u32,
    /// 最近的至多 5 个会话，按 `last_activity` 从新到旧。行显示第一个，检查器全部列出。
    pub recent: Arc<[SessionRow]>,
    pub changes: RowChanges,
    pub overlaps: Overlaps,
    pub usage: RowUsage,
    pub node: NodeId,
}

pub enum HeadLabel {
    /// 分支短名：`refs/heads/feat/x` → `feat/x`。
    Branch(Arc<str>),
    /// 提交 OID 的前 8 位。
    Detached(Arc<str>),
    /// 还没有提交的分支的短名。
    Unborn(Arc<str>),
    Unknown,
}

pub struct SessionRow {
    pub key: SessionKey,
    pub agent: Agent,
    /// None 时显示 `worktrees-untitled-session`。
    pub title: Option<Arc<str>>,
    pub last_activity: Timestamp,
    pub running: bool,
    pub node: NodeId,
}

/// `WorktreeChanges`（「对其他 crate 的要求」A）的同名字段。
pub struct RowChanges {
    pub uncommitted: ChangeCount,
    pub committed: ChangeCount,
    pub vs_base: ChangeCount,
    pub counted_at: Option<Timestamp>,
}

pub enum Overlaps {
    Pending,
    /// 这个 worktree 的路径集合未知（`WorktreeChanges::overlaps` 为 None，而计数已经到达）。
    Unknown,
    List(Arc<[OverlapRow]>),
}

pub struct OverlapRow {
    pub other: WorktreeKey,
    /// 对方的分支标签。
    pub other_head: HeadLabel,
    pub files: u32,
    /// `Overlap::paths`（至多 10 个），检查器展开时列出。
    pub paths: Arc<[RelPath]>,
    pub node: NodeId,
}

pub enum RowUsage {
    /// `usage` 插件不在：不画 token 和用量一节。
    Absent,
    /// 快照还没有算出（`snapshot` 为 None 或 `UsageState::Loading`）。
    Pending,
    /// 默认分支或 detached：`WorktreeUsage::cumulative` 为 None，usage 不统计。
    NotCounted,
    /// P1：分支用量。
    Branch(BranchUsage),
    /// P3：`WorktreeUsage::mrs` 不为空时，每个 MR 一项。
    Mrs(Arc<[MrItem]>),
}

pub struct BranchUsage {
    pub cumulative: Tokens,
    pub review: Tokens,
    pub since: Option<Timestamp>,
    /// `SnapshotHeader::may_include_inherited`。
    pub may_include_inherited: bool,
    /// `UsageState::Backfilling`：历史还在补读，数字还会增长。
    pub backfilling: bool,
}

pub struct MrItem {
    pub label: MrLabel,
    /// 这个 MR 区间的编写用量：`mrs` 只有一项时取 `WorktreeUsage::cumulative`，多于一项时取这个 MR 的 `MrUsage::write`；
    /// 还没有快照时为 None。
    pub tokens: Option<Tokens>,
    /// 与这个 MR 共用分支的其他 MR 数（`MrUsage::shared_with.len()`；只有一项时为 0）。
    pub shared_with: u32,
}
```

合成规则（`snapshot::build(&Inputs) -> Built` 是纯函数，`#[test]` 覆盖，见「测试」）：

| 结果 | 条件 |
|---|---|
| 行 | `ProjectSnapshot::worktrees` 的每一项一行；组内顺序与它相同（主 worktree 在前，linked worktree 按名字）。prunable 的行排在 Idle 的最后。组内不按活动时间排序：agent 工作时行的位置不跳动 |
| 会话所在的 worktree | `Place::Located(l)` 取 `l.worktree`；`Place::Resolved(r)` 取 `r.worktree`；其余（`Pending`、`Missing`、`Unresolvable`）不计入任何一行。与 project_home 相同 |
| 计入一行的会话 | `SessionList::in_project(key)` 里 `last_activity` 在最近 14 天内、所在 worktree 是这一行的会话，其中：`parent` 为 None 的会话；`parent` 指向的会话不在同一个 worktree（或不在列表里）的子 agent 会话。与父会话在同一个 worktree 的子 agent 会话并入父会话，不单独计数。这与 `review` 的审阅对象一致：在别的 worktree 里的子 agent 会话是单独的审阅对象 |
| `running`（会话） | `open_turn && now - last_activity ≤ 10 min`（`now` 是合成时刻），与 project_home 相同。并入父会话的子 agent 会话运行中时，父会话那一项算运行中 |
| Active 组 | `running > 0` 且不是 prunable（「未决问题」1） |
| Idle 组 | 其余的行 |
| `next_expiry` | 全部运行中的会话（含并入的子 agent 会话）的 `last_activity + 10 min` 的最小值 |
| `changes` | `WorktreesOverview::worktrees` 里同一 `WorktreeKey` 的项；还没有这一项时三个计数都是 `Pending`；prunable 的行都是 `Unavailable` |
| `overlaps` | 同一项的 `overlaps`：`Some` 为 `List`；`None` 而计数已到达为 `Unknown`；还没有这一项为 `Pending`。`other` 不在 `ProjectSnapshot::worktrees` 里的项丢弃 |
| `usage` | 这一行的 `UsageSnapshot::Worktree`：`mrs` 为空时按 `cumulative` 得到 `Branch` 或 `NotCounted`；`mrs` 不为空时为 `Mrs` |
| 需要 `complete` 的会话 | `recent` 里 `tail_pending` 的会话，按键去重，每个只请求一次（与 project_home 相同）。project_home 的侧栏不列子 agent 会话，所以本 crate 自己请求 |

### 命令与事件

本 crate 不提供服务，不发自己的事件。视图发出的是 `WorkspaceCommand`（经 `dyn Contributions::dispatch`）和 `Review::refresh_worktrees`：

| 用户动作 | 命令 |
|---|---|
| 侧栏的 Worktrees 条目 | 条目的 `on_select`：`Push(Route { screen: WORKTREES, target: Target::None })`。屏幕显示当前项目 |
| 行的点击、↵ | `Push(Route { screen: REVIEW, target: Target::Review(ReviewTarget::Range { project, worktree, base }) })`。有主干时 `base = BaseSpec::MergeBase(base.full)`，没有主干时 `BaseSpec::Head`。prunable 的行只选中，不发命令（「未决问题」2） |
| 检查器的「审阅改动」 | 同上 |
| 检查器里的会话行 | `Push(Route { screen: REVIEW, target: Target::Review(ReviewTarget::Session(key)) })` |
| 检查器的「切换到这个 worktree」 | `SetActiveWorktree(worktree)`，停在本屏幕（「未决问题」3）。当前 worktree 和 prunable 的行没有这个按钮 |
| 工具栏的 Refresh、⌘R | `Review::refresh_worktrees(project)`（立即返回），不经 `workspace` |
| Esc、⌘[ | `workspace` 处理：`Pop` 回到打开本屏幕的屏幕，`PopToBase` 回到项目主页 |

## 数据结构

- `Hub`（"worktrees" 插件的状态，`Mutex`）：当前项目的 `ProjectKey`（文件夹项目也记下，快照的 `folder` 据此为 true）；侧栏条目的 `Registration`（只为 git 项目登记）；`shown`（本屏幕是不是当前屏幕）；`Review::watch_worktrees` 的关注；`Option<Arc<WorktreesSnapshot>>`；合成任务的标记（`dirty`、`building`）；`switches` 计数；已请求 `complete` 的会话键集合（随项目关闭清空）；视图的唤醒发送端列表 `Vec<mpsc::UnboundedSender<Arc<WorktreesSnapshot>>>`。
- `WorktreesModel`（GPUI `Entity`）：屏幕和检查器页共用，一个屏幕一份，屏幕弹出时 drop。字段：`Arc<WorktreesSnapshot>`；选中的 `Option<WorktreeKey>`；检查器里展开的重叠项 `HashSet<WorktreeKey>`；`reported_switches`（建模型时取 `Hub` 的 `switches`，之后每报告一次里程碑更新）；定时 `Option<(Timestamp, Task<()>)>`；屏幕打开的 `Instant`（`loading_phase` 用）。屏幕建好时把它的 `WeakEntity` 设成 GPUI 全局 `Current`，检查器页和工具栏从它读，与 review_view 的 `Current` 做法相同。
- `WorktreesScreen`（GPUI `Entity`，内容区）：`Entity<WorktreesModel>`、`FocusHandle`、列表的滚动句柄。
- `DetailTab`（GPUI `Entity`，检查器页）：`Option<WeakEntity<WorktreesModel>>`、每个重叠项展开动画的 `Transition`（高度与透明度，交给 `Expander::go`）。检查器隐藏或换页时 drop。
- 选中规则：模型建好时选中当前 worktree 的行，没有时选第一行；快照换代时按 `WorktreeKey` 保留；选中的行消失时选同一位置的行（下标超出时取最后一行）。
- 快照不序列化、不落盘。本 crate 没有派生数据，也没有以 blob OID 为键的缓存；`committed` 由 `review` 按提交对缓存（架构规则 5，「对其他 crate 的要求」A）。

## 数据流与线程

```
kernel 线程：activate（登记屏幕、检查器页、快捷键，订阅）─▶ started（已打开的项目按 Opened 处理）
executor：ProjectEvent::Opened { Ok(key) }（新项目）─▶ 监听：Hub.project = key；key 是 git 项目时
          Contributions::register(SidebarEntry("worktrees"), Some(token))（换项目时先 drop 旧的登记）
executor：WorkspaceEvent::ScreenChanged { to } ─▶ 监听：shown = (to.screen == WORKTREES)
          shown 变为 true ─▶ Review::watch_worktrees(key)，dirty ─▶ 安排合成
          shown 变为 false ─▶ drop 关注，UsageLink::set_wanted([])，丢掉 Hub 里的快照
executor：ProjectEvent::Changed（parts.active 时 switches += 1，与 shown 无关）/ SessionsChanged /
          ReviewEvent::Worktrees { project } / UsageLink 的 on_change ─▶ 监听：shown 时 dirty ─▶ 安排合成
executor [Background + BusyGuard]：合成任务 ─▶ Projects::snapshot、每个 SessionSource::sessions()、Review::worktrees、
          UsageLink::usage() 之后的 Usage::snapshot（都是取锁复制 Arc）─▶ snapshot::build ─▶ Arc<WorktreesSnapshot>
          ─▶ 写进 Hub ─▶ 发给每个视图的发送端 ─▶ UsageLink::set_wanted(built.queries) ─▶ SessionSource::complete(新的键)
主线程：WorktreesModel 的前台任务收到快照 ─▶ 换 Arc、按键保留选中、按 next_expiry 重设定时 ─▶ cx.notify() ─▶ 下一帧画出
主线程：snapshot.switches > reported_switches 且 files_settled 的那一帧 ─▶ report_milestone(WorktreeSwitched)
主线程：定时到点 ─▶ Hub::request_build()（只置 dirty、安排合成）
主线程：按键 / 点击 ─▶ on_action ─▶ Contributions::dispatch(WorkspaceCommand) 或 Review::refresh_worktrees
executor：ProjectEvent::Closed { key } ─▶ 监听：drop 侧栏条目的登记、关注与快照，UsageLink::set_wanted([])，发一份空快照
```

- 监听只改 `Hub` 的标记、安排合成任务，不读服务的快照、不做 IO（[kernel 设计「事件分发」](kernel.md#事件分发)）。合成任务经 `PluginHandle::own_with` 提交，`Background` 优先级，持有 `BusyGuard` 到快照发布为止。同一时刻最多一个合成任务；运行期间来的事件只把 `dirty` 置位，任务结束时发现 `dirty` 就再运行一轮。
- 关注随 `shown`，不随视图的生命周期：审阅屏幕压在 Worktrees 之上时，Worktrees 的视图还在栈里，但关注已经放下，`review` 不再为它比较。`review` 在关注放下后保留计数（「对其他 crate 的要求」A），所以从审阅返回时计数立即显示。`UsageLink` 的关注同理。
- `switches` 在 `shown` 为 false 时也计数（只加一）。所以在 `Push(WORKTREES)` 时建的模型取到的是切换之前的值，之后的切换都能被看到（「性能」的里程碑）。
- 定时是与 project_home 有意的不同：Active / Idle 取决于时间，一个运行中的会话 10 分钟没有新事件时要移到 Idle，而这时没有任何事件。做法：每个模型至多一个 GPUI 的一次性定时，只在 `next_expiry` 为 Some 时设，`next_expiry` 变化时重设，模型 drop 时随之取消；到点后请插件重新合成一次。插件在 `shown` 为 false 时忽略这个请求。采样状态里本屏幕不显示，所以那里没有定时。行里的相对时间只随合成刷新。
- 主线程上只做：读模型里的快照、`Contributions::dispatch`、`report_milestone`、立即返回的 `Review::refresh_worktrees`、设定时。不调用任何阻塞的服务方法（`SessionSource::read` / `turns` / `load`、`Projects::resolve` 都不碰）。
- 首帧之前什么都不做：两个插件都是 `AfterFirstFrame`，`activate` 只登记，第一次合成在屏幕第一次成为当前屏幕之后。

## 性能

| 预算 | 本 crate 的份额 |
|---|---|
| 冷启动 20 ms（`app/cold_start`） | 首帧之前没有代码运行；`plugin/worktrees/activate`、`plugin/worktrees-usage/activate` 各 < 1 ms，只登记 |
| 空闲内存（空窗口，`app/idle_empty` < 60 MB） | 插件状态与登记的贡献 < 16 KiB（`plugin/worktrees/resident`） |
| 空闲内存（采样状态：打开 monorepo < 100 MB、10 万文件 < 120 MB，[ADR 0035](../adr/0035.md)） | 采样状态是项目主页，本屏幕不显示，本 crate 不持有快照和关注：只有插件状态和侧栏条目，< 16 KiB，不从 ADR 0035 的余量里分配。`review` 在离开屏幕后保留的计数、路径哈希与重叠计入 `review` 自己的 < 1 MB（「对其他 crate 的要求」A），也不另占余量 |
| 切换 worktree < 100 ms（`app/switch_worktree`，[性能预算](tech-research.md#性能预算)） | 终点 `Milestone::WorktreeSwitched`：`switches` 增加之后，第一帧画出 `files_settled` 为 true 的快照。路径：`SetActiveWorktree` → `Projects::set_active_worktree`（`project/switch_worktree` < 30 ms，实测约 0.8 ms，清单缓存以 blob OID 为键）→ 两次合成（`Pending` 一代、`Ready` 一代，各 < 2 ms）→ 一帧。不等 `review` 的计数和 `usage` 的快照 |
| 屏幕显示时的内存（不在采样状态） | 快照与两个视图的状态 < 256 KiB（`worktrees/resident_bytes`；估算：17 行 × 约 2 KB，含 5 个会话行和 8 个重叠项的 `NodeId` 与 `Arc` 克隆）。`review` 的部分见 A：17 个 worktree、每个 4000 个改动路径时约 650 KB；一次比较期间的临时内存由 `git/compare_100k/peak_bytes`（≤ 72 MB）约束，同一时刻只有一次比较 |
| 帧时间 120 fps（每帧工作 ≤ 8.3 ms） | 列表的行数等于 worktree 数，用 `GroupedList`，不虚拟化（与 project_home 的主区相同）；检查器至多 8 个重叠项（每项至多 10 个路径）和 5 个会话。动画只有 `Row` 的悬停、`AgentDot` 的轻闪、重叠项的 `Expander` |
| 后台 CPU | 本 crate 每批事件至多一次合成（< 2 ms）。比较由 `review` 做：cloudprint 形状每个 worktree 几毫秒；10 万文件每个约 1 s，17 个 worktree 的第一轮约 17 s，在 `Background` 上逐个进行，离开屏幕时取消（A） |

新增基准（`bench`，macOS 判定）：

| ID | 统计量与预算 |
|---|---|
| `worktrees/build_1000` | 中位数 < 2 ms：`snapshot::build`，17 个 worktree、1000 个会话、每个 worktree 8 个重叠项、每个 worktree 一份分支用量 |
| `worktrees/resident_bytes` | 存活字节 < 256 KiB：同一夹具的快照与两个视图的状态（`Memory`） |

`app/switch_worktree`（[bench 设计「预算表」](bench.md#预算表)里「加入」一列写着 `worktrees`）随本 crate 的第 3 步改为生效。

## 界面

- 屏幕与层：[交互规范「屏幕」](interaction.md#屏幕)的 04 Worktrees，在按需层（[「三层结构」](interaction.md#三层结构)：从当前视图打开，Esc 回到原处）。`ScreenLayout { sidebar: Shown, back_button: true, reading_column: true, inspector: true, floating_bar: false }`。工具栏：标题 `worktrees-title`；副标题 `worktrees-subtitle`（活动数与总数）；右侧控件只有 Refresh（`Button`，`ButtonKind::Plain`，`worktrees-refresh`），检查器开关由 `workspace` 画。
- 主区（居中 760pt 阅读栏）：两个 `GroupedList`，标题分别是 `worktrees-active`、`worktrees-idle`；空的组不显示。只有一个 worktree 时，列表下面显示一行 `worktrees-only-one`。
- 行（`Row`，两行文字加行尾）：
  - 前导：`recent[0]` 的 `AgentDot`；`running` 从 0 变为大于 0 时 `blink` 一次。没有会话或 `Agent::Other` 时不画圆点。
  - 第一行：分支标签（`Text::verbatim`；`Detached` 后加次要文字 `worktrees-detached`，`Unborn` 后加 `worktrees-unborn`，`Unknown` 为 `worktrees-head-unknown`），后面依次是标记 `worktrees-current`、`worktrees-locked`、`worktrees-missing`（次要文字样式，探针角色 `Badge`）。
  - 第二行（次要文字，用 ` · ` 连接）：会话一项，`running > 0` 时为 `worktrees-running`，否则为 `recent[0]` 的相对时间，没有会话时为 `worktrees-no-sessions`；改动两项，`worktrees-uncommitted` 与 `worktrees-vs-base`（没有主干时不显示后者）。计数为 `Pending` 时按 `loading_phase` 画 `Skeleton`，`Capped` 为 `worktrees-capped`，`Unavailable` 时不显示那一项。
  - 行尾：`Branch` 为 `count_compact(cumulative.total())` 的 `worktrees-tokens`；`Mrs` 为第一项的 token，多于一项时加 `worktrees-mr-more`；`Pending` 为 `Skeleton`；`NotCounted`、`Absent` 不画。
  - 选中用 `Row::selected`。prunable 的行第一行用 `TextRole::Secondary`。
- 检查器的 Worktree 页（300pt，`ui::contrib::InspectorTab { label: worktrees-detail-title, build, screen: Some(WORKTREES) }`），内容是选中的行，从上到下：
  1. 标题：分支标签与标记（`TextRole::Body`）；下面一行 `root`（`Text::verbatim`，`TextRole::Secondary`，超出截断）。
  2. 用量：`Branch` 时标题 `worktrees-usage-branch`，下面是累计 token（`count_compact`）、四类 token 各一行（`worktrees-tokens-input`、`-output`、`-cache-read`、`-cache-write`）、审阅用量（`worktrees-tokens-review`）、起点（`worktrees-usage-since`，`Strings::relative_time`）；`may_include_inherited` 时注 `worktrees-usage-inherited`，`backfilling` 时注 `worktrees-usage-backfilling`。`Mrs` 时标题 `worktrees-usage-mr`，每个 MR 一行：`MrLabel::title`（原样）、token、`shared_with > 0` 时注 `worktrees-mr-shared`。`NotCounted` 时只有一行 `worktrees-usage-not-counted`。`Absent` 时整节不显示。不显示费用（[ADR 0022](../adr/0022.md)）。
  3. 对比：有主干时标题 `worktrees-compare-title`（参数是 `base.short`），没有时 `worktrees-compare-head`；三行 `worktrees-committed`、`worktrees-uncommitted-files`、`worktrees-total`（没有主干时只有第二行）；不是当前 worktree 时加 `worktrees-counted-at`（`counted_at` 的相对时间）；按钮 `worktrees-review-changes`（`ButtonKind::Secondary`，命令同「行的点击」）。prunable 时整节换成 `worktrees-missing-detail`。
  4. 文件重叠：标题 `worktrees-overlaps-title`；每项一行：对方的分支标签与 `worktrees-overlap-files`，点击展开（`Expander`），展开后列出 `paths`（`Text::verbatim`，`TextRole::Code`），`files` 多于列出的数目时加一行 `worktrees-overlap-more`。空列表为 `worktrees-no-overlap`，`Unknown` 为 `worktrees-overlap-unknown`，`Pending` 为 `Skeleton`。
  5. 最近会话：标题 `worktrees-sessions-title`；`recent` 每项一行：圆点、标题（None 时 `worktrees-untitled-session`）、相对时间或 `worktrees-running`；点击进入会话审阅。
  6. 按钮 `worktrees-switch`（`ButtonKind::Primary`）：不是当前 worktree、不是 prunable 时显示。
  - 没有选中的行时（只在快照为空时出现）显示 `worktrees-detail-empty`。
- `ui` 的控件与动效原语：`GroupedList`、`Row`、`AgentDot`、`Button`、`Skeleton`、`loading_phase`、`Expander`；悬停由 `Row` 自带（`HOVER_IN` / `HOVER_OUT`），圆点轻闪用 `ARRIVE`，展开用 `Expander::go`；减弱动态效果由 `ui` 处理。本 crate 不写颜色值、字体名和时长（「验收」的 `rg` 检查）。
- Fluent 键（`crates/i18n/locales/{en,zh-CN}/worktrees.ftl`，`keys::worktrees::*`；分支名、路径、会话标题、MR 标题经 `Text::verbatim` 或 `Args::str` 原样显示；数字经 `Args` 按语言格式化）：

  | 键 | en | zh-CN |
  |---|---|---|
  | `worktrees-entry` | Worktrees | Worktree |
  | `worktrees-title` | Worktrees | Worktree |
  | `worktrees-subtitle` | { $active } active · { $total -> [one] { $total } worktree *[other] { $total } worktrees } | { $active } 个活动 · 共 { $total } 个 |
  | `worktrees-refresh` | Refresh | 刷新 |
  | `worktrees-active` | Active | 活动 |
  | `worktrees-idle` | Idle | 空闲 |
  | `worktrees-only-one` | This repository has one worktree. Agents working in other worktrees appear here. | 这个仓库只有一个 worktree。在其他 worktree 里工作的 agent 会列在这里。 |
  | `worktrees-not-git` | This folder is not a git repository. | 这个文件夹不是 git 仓库。 |
  | `worktrees-current` | Current | 当前 |
  | `worktrees-locked` | Locked | 已锁定 |
  | `worktrees-missing` | Missing | 已缺失 |
  | `worktrees-detached` | detached | 分离 HEAD |
  | `worktrees-unborn` | no commits | 没有提交 |
  | `worktrees-head-unknown` | Unknown HEAD | HEAD 无法读取 |
  | `worktrees-running` | { $count } running | { $count } 个运行中 |
  | `worktrees-no-sessions` | No sessions in the last 14 days | 最近 14 天没有会话 |
  | `worktrees-uncommitted` | { $count } uncommitted | { $count } 个未提交 |
  | `worktrees-vs-base` | { $count -> [one] { $count } file *[other] { $count } files } vs { $base } | 相对 { $base } { $count } 个文件 |
  | `worktrees-capped` | { $limit }+ files | { $limit } 个以上的文件 |
  | `worktrees-tokens` | { $tokens } tokens | { $tokens } token |
  | `worktrees-mr-more` | +{ $count } MR | 另 { $count } 个 MR |
  | `worktrees-detail-title` | Worktree | Worktree |
  | `worktrees-detail-empty` | No worktree selected | 没有选中的 worktree |
  | `worktrees-usage-branch` | Branch Usage | 分支用量 |
  | `worktrees-usage-mr` | MR Usage | MR 用量 |
  | `worktrees-tokens-input` | Input | 输入 |
  | `worktrees-tokens-output` | Output | 输出 |
  | `worktrees-tokens-cache-read` | Cache read | 缓存读取 |
  | `worktrees-tokens-cache-write` | Cache write | 缓存写入 |
  | `worktrees-tokens-review` | Review | 审阅 |
  | `worktrees-usage-since` | Since { $time } | 自 { $time } 起 |
  | `worktrees-usage-inherited` | May include context inherited from a parent session | 可能含从父会话继承的上下文 |
  | `worktrees-usage-backfilling` | Still reading earlier sessions | 仍在读取较早的会话 |
  | `worktrees-usage-not-counted` | Not counted on the default branch or a detached HEAD | 默认分支和分离 HEAD 不统计 |
  | `worktrees-mr-shared` | { $count -> [one] Shares the branch with { $count } other MR *[other] Shares the branch with { $count } other MRs } | 与另外 { $count } 个 MR 共用分支 |
  | `worktrees-compare-title` | Compared with { $base } | 与 { $base } 对比 |
  | `worktrees-compare-head` | Uncommitted Changes | 未提交的改动 |
  | `worktrees-committed` | { $count -> [one] { $count } committed file *[other] { $count } committed files } | 已提交 { $count } 个文件 |
  | `worktrees-uncommitted-files` | { $count -> [one] { $count } uncommitted file *[other] { $count } uncommitted files } | 未提交 { $count } 个文件 |
  | `worktrees-total` | { $count -> [one] { $count } file in total *[other] { $count } files in total } | 合计 { $count } 个文件 |
  | `worktrees-counted-at` | Counted { $time } | { $time }统计 |
  | `worktrees-review-changes` | Review Changes | 审阅改动 |
  | `worktrees-missing-detail` | The worktree folder no longer exists. | worktree 的目录已不存在。 |
  | `worktrees-overlaps-title` | Overlapping Files | 文件重叠 |
  | `worktrees-overlap-files` | { $count -> [one] { $count } file *[other] { $count } files } | { $count } 个文件 |
  | `worktrees-overlap-more` | { $count } more | 另有 { $count } 个 |
  | `worktrees-no-overlap` | No changed files in common with other worktrees | 与其他 worktree 没有共同改动的文件 |
  | `worktrees-overlap-unknown` | Too many changes to compare | 改动太多，没有比较 |
  | `worktrees-sessions-title` | Recent Sessions | 最近会话 |
  | `worktrees-untitled-session` | Untitled session | 未命名会话 |
  | `worktrees-switch` | Switch to This Worktree | 切换到这个 worktree |

  相对时间用 `Strings::relative_time`（common.ftl），复数按 Fluent 的选择器（[交互规范「语言」](interaction.md#语言)）。`worktrees-capped` 的 `limit` 是 4000。

- UI 探针的语义节点（`ui` 设计的 `semantic` 模块：`NodeId::new("worktrees", …)`，名字段只用 `[a-z0-9-]`）：

  | ID | `Role` | 状态 / `attrs` |
  |---|---|---|
  | `worktrees.list` | `List` | `rows`、`active`、`idle`（行数）、`ready`（计数是否已到达） |
  | `worktrees.active`、`worktrees.idle` | `Group` | — |
  | `worktrees.item[<WorktreeKey>]` | `ListItem` | `selected`；`current`、`main`、`locked`、`prunable`、`sessions`、`running`、`uncommitted`、`vs-base`（数字，或 `pending` / `capped` / `unavailable`）、`tokens` |
  | `worktrees.item[<WorktreeKey>].badge[<current / locked / missing>]` | `Badge` | — |
  | `worktrees.toolbar.refresh` | `Button` | — |
  | `worktrees.only-one`、`worktrees.empty` | `Text` | `Label::key` 是文案的键 |
  | `worktrees.detail` | `Panel` | `selected`（是否有选中的行） |
  | `worktrees.detail.usage` | `Group` | `kind`（`branch` / `mr` / `not-counted`）、`tokens` |
  | `worktrees.detail.usage.mr[<MR 引用>]` | `ListItem` | `tokens`、`shared` |
  | `worktrees.detail.compare` | `Group` | `committed`、`uncommitted`、`total` |
  | `worktrees.detail.compare.open` | `Button` | — |
  | `worktrees.detail.overlaps` | `List` | `count`，或 `pending` / `unknown` |
  | `worktrees.detail.overlaps.item[<WorktreeKey>]` | `ListItem` | `files`、`expanded` |
  | `worktrees.detail.sessions.item[<会话 ID>]` | `ListItem` | `agent`、`running` |
  | `worktrees.detail.switch` | `Button` | — |

  `ui` 的控件自带角色和标签，本 crate 只给 `NodeId`；`GroupedList` 的容器和检查器的各节用 `semantic(...)` 包装。

- Action 与快捷键（经 `KeyBindings` 贡献登记，谓词 `Content > worktrees`；都能被探针按名字触发）：

  | Action | 键 | 处理 |
  |---|---|---|
  | `worktrees::SelectNext`、`SelectPrevious` | ↓、↑ | 在 Active → Idle 的顺序里移动选中，跨组连续；到头不回绕；滚动到可见 |
  | `worktrees::SelectFirst`、`SelectLast` | ⌥↑、⌥↓ | — |
  | `worktrees::ReviewChanges` | ↵ | 选中行的「相对主干」审阅；prunable 时忽略 |
  | `worktrees::Refresh` | ⌘R | `Review::refresh_worktrees(project)` |
  | `worktrees::SwitchTo` | — | 选中行的 `SetActiveWorktree`；当前 worktree 与 prunable 时忽略 |
  | `worktrees::OpenWorktree { key }`、`OpenSession { key }`、`ToggleOverlap { key }` | — | 鼠标点击与探针用 |

  检查器页没有键盘焦点，只响应点击。Esc、⌘[、⌥⌘I、⌘\ 归 `workspace`（⌘\ 切到 Conversation 页；Conversation 不在本屏幕显示，检查器显示 Worktree 页，见「对其他 crate 的要求」B）。

## 状态与异常

| 情况 | 表现（[交互规范「状态与异常」](interaction.md#状态与异常)） |
|---|---|
| 刚进入屏幕，计数还没有到达 | 行按项目快照和会话列表立即画出；改动数、重叠和 token 按 `loading_phase` 显示骨架，100 ms 内不显示 |
| 第一轮比较超过 1 s（10 万文件） | 骨架换成 `LoadingPhase::Progress` 的不确定进度；计数逐个 worktree 到达。不提供取消：离开屏幕即取消 |
| 文件夹项目 | 不登记侧栏条目；屏幕被压入时（如经探针）显示 `worktrees-not-git` |
| 只有主 worktree | 一行，下面加 `worktrees-only-one` |
| prunable 的 worktree | 在 Idle 的最后，带 `worktrees-missing`；点击只选中，不能审阅、不能切换；检查器的对比一节为 `worktrees-missing-detail` |
| `Head::Unknown` | 标签为 `worktrees-head-unknown`；计数由 `review` 给出（通常是 `Unavailable`） |
| 没有主干（`base` 为 None） | 不显示 vs-base 一项；对比一节为「未提交的改动」，按钮打开 `Range { base: Head }` |
| `Capped`（改动路径超过 4000 个） | 显示 `worktrees-capped`；这个 worktree 的重叠为 `Unknown` |
| 计数为 `Unavailable` | 不显示那一项 |
| 切换被离开守卫拒绝 | 不切换，本 crate 不提示（守卫自己提示，如未保存的编辑） |
| 切换中（`files` 为 `Pending`） | 当前标记先移到新行（快照的 `active` 先变）；里程碑等 `files` 就绪 |
| `usage` 插件没有装、激活失败或停用 | 没有 token 列和用量一节，其余照常 |
| `review` 插件失败 | `worktrees` 依赖 `dyn Review`，随之停用：屏幕与侧栏条目随插件注销，`workspace` 按它的规则处理正在显示的屏幕。总览没有计数就没有意义，不另做降级 |
| 会话格式无法识别 | 照常计数，标题为 None 时显示 `worktrees-untitled-session` |
| 选中的 worktree 被删除 | 选中移到同一位置的行 |
| 项目关闭 | 视图收到空快照，清空；`workspace` 随 `Closed` 切回 Welcome |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `worktrees`。监听和合成任务里的记录带 kernel / executor 设的 `plugin`、`act`、`task`；视图在主线程上的记录带 `workspace` 恢复的 `plugin`、`act`（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 11 项）。

- 默认记录：
  - info `worktrees snapshot built`：屏幕每次成为当前屏幕之后的第一次合成（之后降为 debug）。字段：`worktrees`（行数）、`active`（Active 组的行数）、`sessions`（计入的会话数）、`changes`（bool，计数是否已到达）、`usage`（bool）、`took_us`。
  - info `worktree switch requested`：`index`（目标在 `ProjectSnapshot::worktrees` 里的下标）、`main`（bool）。
  - info `worktrees milestone reported`：`switches`（计数值）。
  - warn `worktrees build dropped`：`reason`（`project-closed` / `generation-stale` / `hidden`）。
  - warn `usage call refused`：`reason`（`token-dead`）；`usage` 插件停用期间至多一条。
  - error：没有。本 crate 没有会失败的操作（服务方法不返回错误，命令由 `workspace` 处理）。
- 详细级别：debug `worktrees snapshot built`（每次合成，字段同上，另带 `sources` 的代数）；debug `worktrees command`：`command`（判别名）、`target`（`range-base` / `range-head` / `session` / `switch` / `refresh`）；debug `worktrees shown` / `worktrees hidden`：`queries`（usage 关注数）；debug `worktrees timer armed`：`in_ms`；debug `worktrees view attached` / `detached`：`view`（`screen` / `detail`）；trace `worktrees event`：事件类型名与代数。
- 关联 ID：不另设作用域。本 crate 不处理单个会话的流程，不设 `session`。
- 不记录：分支名与主干的引用名、worktree 的根路径与 `WorktreeKey`（它是 git 目录的路径）、重叠的路径、会话标题、项目名、MR 标题、token 数、界面文字和 `NodeId` 的业务键。用下标、计数、代数和判别名代替。
- 金丝雀测试：`canary_stays_out_of_worktrees_logs`（`src/tests/logs.rs`）：`lectern_logging::testing::capture()` 在 trace 级别下，构造分支名、主干引用名、worktree 根路径与 git 目录、重叠路径、会话标题、项目名、MR 标题都含金丝雀的项目；跑一次合成、画一帧、↑ ↓、↵、切换、⌘R、关闭项目，然后 `assert_absent(金丝雀)`。

## 测试

第 1 步的测试（`snapshot::build`、插件、`UsageLink`、金丝雀的合成部分）不开窗口，也不用 `gpui`；但快照里的 `NodeId` 和侧栏条目的 `SidebarItem` 来自 `lectern-ui`，它依赖 GPUI，所以本 crate 从第 1 步起就只在 macOS 上编译和测试，Linux 的 `just` 命令不覆盖它（「与设计的差异」）：`just test -p lectern-worktrees`。第 2 步再加上 `gpui` 本身。项目、会话、计数、用量、贡献都用 `services` trait 的假实现，放在本 crate 的 `testing` 模块（`FakeProjects`、`FakeSessions`、`FakeReview`、`FakeUsage`、`FakeContributions`）：直接构造 `ProjectSnapshot`、`SessionList`、`WorktreesOverview`、`UsageSnapshot` 交出去，记下收到的 `watch_worktrees`、`refresh_worktrees`、`watch`、`dispatch`、`complete`、`register`；`read` / `turns` / `load` / `resolve` 一律 `unreachable!`。本 crate 不调用外部命令行工具，测试不用真实的 git、会话扫描和文件 IO，不依赖插件 crate 的 `test-support`。

- `snapshot::build`（`#[test]`）：`build_one_row_per_worktree_in_project_order`（主 worktree 在前，prunable 在 Idle 最后）；`build_counts_sessions_per_worktree`（`Located` 与 `Resolved` 都计入，14 天之外不计；同一 worktree 的子 agent 并入父会话；别的 worktree 里的子 agent 计入那一行；父会话不在列表里的子 agent 单独计数）；`build_active_means_running`（10 分钟内与 11 分钟；并入的子 agent 运行中时父会话算运行中）；`build_next_expiry_is_the_earliest`；`build_changes_follow_the_overview`（计数未到达为 `Pending`，prunable 为 `Unavailable`，`Capped`，没有主干）；`build_overlaps_drop_unknown_worktrees`；`build_usage_branch_mr_and_not_counted`（分支用量；默认分支为 `NotCounted`；一个 MR 取 `cumulative`；两个 MR 取各自的 `MrUsage` 与 `shared_with`）；`build_without_usage`；`build_head_labels`（分支短名、detached 取 8 位、unborn、unknown；`refs/remotes/origin/main` → `origin/main`）；`build_requests_complete_once`；每个用例的快照渲染成文本做 insta 快照（`snapshot_*`）。`worktrees/build_1000` 的夹具也在这里构造。
- 插件（`#[test]`，`TestKernel`）：`git_project_registers_the_sidebar_entry`、`folder_project_registers_no_entry`、`closed_drops_the_entry`；`interests_follow_the_screen`（`ScreenChanged` 到 worktrees 时 `watch_worktrees` 一次、每个不是 prunable 的 worktree 一个 `watch(Worktree)`；到 review 时都放下；回来时重新取）；`hidden_screen_ignores_events`（不显示时事件不触发合成，`switches` 照常计数）；`events_coalesce_into_one_build`；`switches_count_active_changes`；`usage_link_empty_builds_without_usage`、`usage_activating_after_shown_takes_the_watches`、`usage_plugin_deactivation_rebuilds`、`same_wanted_set_does_not_rebuild`；`started_handles_an_already_open_project`、`changed_for_an_unknown_project_counts_as_opened`。
- `#[gpui::test]`（`TestAppContext`，`init_fixed`）：`activate_registers_contributions_only`（`Screen`、`InspectorTab` 带 `screen: Some(WORKTREES)`、`KeyBindings`，没有 `SidebarEntry`，没有任务）；`list_groups_active_and_idle`；`arrow_keys_cross_groups`；`click_and_enter_push_the_range_review`（有主干时 `MergeBase(完整引用名)`，没有时 `Head`，prunable 不发）；`detail_follows_the_selection`；`detail_session_pushes_session_review`；`switch_dispatches_set_active_worktree`（当前与 prunable 的行没有按钮）；`refresh_calls_review`；`milestone_after_a_switch_on_the_settled_frame`（`Pending` 那一代不报告，`Ready` 那一帧报告一次，第二次切换再报告一次）；`no_milestone_without_a_switch`；`timer_moves_an_expired_session_to_idle`；`selection_survives_rebuilds_and_removal`；`overlap_expands`；`skeleton_appears_after_100ms_only`；`closed_project_clears_the_views`。
- 语义树：`worktrees_tree_two_groups`、`worktrees_tree_folder`、`detail_tree_with_overlaps_and_usage` 的 insta 快照（UI 探针完成后接入；之前用 `ui::semantic::Recorder` 的记录去掉 `bounds` 和 `declared_at`）。
- 日志：`canary_stays_out_of_worktrees_logs`。
- fixtures：没有外部格式；测试数据是源码里构造的快照。
- 只能在 macOS 上跑的部分：第 2 步起全部。

## 对其他 crate 的要求

每一项是一个 PR，写 `Refs #16`，在「验收」里列为前置条件。`project`、`agents`、`usage` 不新增要求：`ProjectSnapshot::worktrees`、`SessionSummary::place` / `open_turn`、`UsageQuery::Worktree` / `Mr` 都已在它们的设计里。

- **A. `review`（含 `services` 的 `review` 模块）：每个 worktree 的改动计数与文件重叠。** `services` 部分（类型、带默认实现的三个方法、事件变体、第 8 条的匹配分支）已实现（#334）；`review` 部分（第 1 至 7 条的计算，`review.md` 的「接口」「概览」「性能」和调用方一节同时更新）已实现（#337），与本文的差异见「与设计的差异」。 `Overview` 只含当前 worktree（[review 设计「概览」](review.md#概览)），`ProjectSnapshot` 对非当前 worktree 只有元数据，而技术调研要求总览列出每个 worktree 的未提交改动量。所以由 `review` 补一份按 worktree 的计数，`review` 的设计文档同时更新「接口」「概览」「性能」和调用方一节：

  ```rust
  pub trait Review: Send + Sync {
      // …已有的方法
      /// 并行 worktree 总览。不阻塞，取锁复制 Arc；没有关注者、也没有保留的结果时为 None。
      fn worktrees(&self, project: &ProjectKey) -> Option<Arc<WorktreesOverview>> { let _ = project; None }
      /// 关注一个项目全部 worktree 的改动计数。立即返回；持有期间按下面的规则计算，drop 后停止计算。
      fn watch_worktrees(&self, project: &ProjectKey) -> OverviewInterest { let _ = project; OverviewInterest::new(|| {}) }
      /// 用户要求刷新（⌘R）：有关注者时把这个项目的全部 worktree 标为失效，按节流规则重算。立即返回。
      fn refresh_worktrees(&self, project: &ProjectKey) { let _ = project; }
  }

  pub enum ReviewEvent {
      // …已有的变体
      /// `worktrees(project)` 换了一份新的。
      Worktrees { project: ProjectKey, generation: u64 },
  }

  pub struct WorktreesOverview {
      pub project: ProjectKey,
      pub generation: u64,
      /// 主干：`Repo::default_branch()` 的完整引用名。没有时为 None，`committed`、`vs_base` 为 `Unavailable`。
      pub base: Option<Arc<str>>,
      /// `ProjectSnapshot::worktrees` 里不是 prunable 的 worktree，顺序相同。
      pub worktrees: Arc<[WorktreeChanges]>,
  }

  pub struct WorktreeChanges {
      pub worktree: WorktreeKey,
      /// 这份计数依据的 HEAD 提交；unborn 或读不出时为 None。
      pub head: Option<BlobId>,
      /// 相对 HEAD 未提交的改动（含未跟踪文件）。
      pub uncommitted: ChangeCount,
      /// 相对主干已提交的改动：merge-base 到 HEAD 之间改变的路径。
      pub committed: ChangeCount,
      /// 相对主干的改动文件：上两项路径的并集。
      pub vs_base: ChangeCount,
      /// 与其他 worktree 共同改动的文件，按 `files` 从多到少，至多 8 项。
      /// None：这个 worktree 的路径集合未知（`Pending`、`Capped`、`Unavailable`）；路径集合未知的 worktree 也不出现在别人的列表里。
      pub overlaps: Option<Arc<[Overlap]>>,
      /// 最近一次完整计算的时刻（kernel 的 `Clock`）；还没有算过时为 None。
      pub counted_at: Option<Timestamp>,
  }

  pub enum ChangeCount {
      Pending,
      Files(u32),
      /// 超过 4000 个改动路径（ADR 0035）：不保留路径集合。
      Capped,
      /// prunable、HEAD 读不出、没有主干（`committed`、`vs_base`）或比较失败。
      Unavailable,
  }

  pub struct Overlap {
      pub other: WorktreeKey,
      pub files: u32,
      /// 共同路径按字节序的前 10 个，相对 worktree 根。两个方向的项共用同一个 `Arc`。
      pub paths: Arc<[RelPath]>,
  }
  ```

  关键约束：

  1. 计算的时机：只在有关注者时比较。第一次关注时计算全部 worktree；之后计算失效的 worktree（第 3 条）和 `refresh_worktrees` 标记的 worktree。关注开始时，`counted_at` 早于 5 分钟的非当前 worktree 也重算：它们没有文件监听。顺序是当前 worktree 在前，其余按 `ProjectSnapshot::worktrees`。同一时刻最多一次比较，在 `Background` 上运行，持有 `BusyGuard`；最后一个关注放下时取消正在进行的比较。
  2. 计算方法：
     - 主干：`Repo::default_branch()`，每轮开头解析一次它的 OID。
     - `uncommitted`：当前 worktree 直接取概览的计数改动集（同一份，不另做比较；没有概览关注者时按非当前 worktree 处理）。非当前 worktree 每次打开 `Repo::worktree` 的句柄，做一次计数比较 `Worktree::changes(Base::Head)`，`Limits` 取 `retained_bytes = 0`、`total_patch_bytes = 0`（ADR 0035，与概览相同），改动路径超过 4000 个时只留计数，返回后放下句柄和改动集：非当前 worktree 不保留 stat 缓存。
     - `committed`：`merge_base(主干, HEAD)`，再 `tree_changes(merge_base, HEAD, 4000)`；结果按（merge-base, HEAD）缓存在已有的 `review.tree_changes` 里（按提交对，架构规则 5），merge-base 按（主干 OID, HEAD OID）在内存里缓存。HEAD 就是 merge-base 时为 `Files(0)`；`TreeChanges::All` 为 `Capped`。
     - `vs_base`：两者路径的并集的大小；任一为 `Capped` 时为 `Capped`。它与 `ReviewTarget::Range { base: MergeBase(主干) }` 的文件集合只在一种情况下不同：已提交的改动在工作区里又改回了 merge-base 的内容（正常使用很少见，记为已知限制）。
     - 重叠：每个 worktree 保留 `vs_base` 路径的 64 位哈希（排好序的数组），两两求交；没有主干时用 `uncommitted` 的路径。样本路径取自刚算完的那一侧的路径集合。
  3. 失效（没有关注者时只记标记，不比较）：fs_watch 的 `RepoBatch::worktrees` 里这个 worktree 的 `head` 或 `index`；`RepoBatch::refs`（主干可能移动：重新解析主干，merge-base 变了的 worktree 重算 `committed`）；`RepoBatch::rescan`；`SessionsChanged` 里归位到这个 worktree 的会话（agent 在非当前 worktree 里写文件时，它的会话文件随之增长，这是那里唯一的信号）；`ProjectEvent` 的 worktree 增减。当前 worktree 的 `uncommitted` 随概览的计数改动集更新。
  4. 节流：每个 worktree 两次比较之间至少间隔 max(2 s, 上一次比较耗时的 10 倍)，期间的触发合并成一次。10 万文件的一次比较约 1 s，所以一个运行中的 agent 让它的 worktree 至多每 10 s 比较一次。
  5. 内存与释放：每个 worktree 至多 4000 个路径哈希（32 KB）、8 个重叠项（每项至多 10 个路径）。关注放下后保留计数、路径哈希和重叠，直到项目关闭；之后的事件只标记失效。10 万文件的形状（17 个 worktree，每个 4000 个改动路径）时约 650 KB（路径哈希 544 KB、重叠样本约 110 KB），计入 `review` 自己的 < 1 MB（`review/resident_100k_bytes`，现在实测 3.45 KB），不另占 ADR 0035 的余量。路径哈希合计超过 65 536 个（512 KiB）时，关注放下 30 秒后释放路径哈希，只留计数和重叠。比较期间的临时内存由 `git/compare_100k/peak_bytes` 约束。
  6. 日志按 review 的规则：一轮结束记 debug `worktrees round`（`worktrees`、`compared`、`capped`、`took_us`），失败记 warn（错误种类）；不记 worktree 的路径、分支名和改动路径。
  7. 测试与基准（review 的设计展开）：用测试里生成的合成仓库，覆盖未提交与已提交的计数、当前 worktree 复用概览的改动集、两个 worktree 的重叠、`Capped` 没有重叠、节流、没有关注者时不比较、放下关注后保留到项目关闭、没有主干。基准：`review/worktrees_cloudprint`（11 个 worktree，冷，从关注到最后一个计数，只记录）；`review/resident_100k_bytes` 的夹具加上一轮关注再放下之后保留的数据，预算不变（< 1 MB）。
  8. 兼容：新方法带默认实现，已有的实现和假实现不必修改；`ReviewEvent` 加变体时，匹配它的 crate（`project_home`、`review_view`）在同一个 PR 里补上分支或已用 `_`。

- **B. `workspace` 与 `ui::contrib`：只在一个屏幕显示的检查器页；重复压栈。**（已实现，#336）
  1. `ui::contrib::InspectorTab` 加一个字段 `pub screen: Option<Name>`。`None` 是现在的行为：页在每个 `layout.inspector` 为 true 的屏幕上显示。`Some(s)`：页只在当前屏幕是 `s` 时显示。当前屏幕有自己的页（`screen == Some(当前屏幕)`）时检查器只显示这些页，不显示 `None` 的页；没有时只显示 `None` 的页。`nav.inspector_tab` 不在可见的页里时显示第一个可见的页，`nav.inspector_tab` 本身不变。原因：Worktree 页要显示在 Worktrees 屏幕上，Files（review_view 的 Progress 列表）和 Conversation 在这里没有内容；Worktree 页也不能出现在审阅屏幕上。已有的页取 `None`，行为不变。测试 `inspector_tab_scoped_to_its_screen`。
  2. `Push(r)` 在栈顶已经是同一个 `Route` 时忽略，记 debug：Worktrees 屏幕显示侧栏，再点侧栏的 Worktrees 条目不会压入第二个同样的屏幕（Usage 条目同理）。测试 `push_of_the_top_route_is_ignored`。
- **C. `i18n`：`locales/{en,zh-CN}/worktrees.ftl`**，内容是「界面」的键表。已实现（#333）。
- **D. `app`。** 装配时构造一个 `UsageLink`，分别交给 `WorktreesPlugin::new(WorktreesDeps { executor, usage })` 和 `WorktreesUsagePlugin::new(executor, link)`；两个插件放进 desktop 与 probe profile 的 `PluginSet`，test profile 不装。bench 的 `Driver::open(OpenTarget::Worktree(path))`：在主线程上取当前项目的 `Projects::snapshot`，找 `root` 等于 `path`、不是 prunable 的 worktree（找不到时报告 `Error`）；然后 `dispatch(Push(Route { screen: WORKTREES, target: Target::None }))`（已在本屏幕时由 B.2 忽略）和 `dispatch(SetActiveWorktree(key))`。终点 `Milestone::WorktreeSwitched` 经 `workspace` 的 `on_milestone` 转给探针（[app 设计「里程碑与帧记录」](app.md#里程碑与帧记录)）。
- **E. `bench`**：预算表加 `worktrees/build_1000`、`worktrees/resident_bytes`；`app/switch_worktree` 随本 crate 的第 3 步改为生效。

## 未决问题

没有需要产品决定的问题。下面四条已于 2026-10-25 决定，人确认按默认选项：

1. **Active 的含义。** 已决定：选 A，有运行中的会话（`open_turn` 且 10 分钟内有活动，与项目主页的「运行中」相同）。agent 等待用户输入时最后一轮已结束，这个 worktree 显示在 Idle。不选：B，最近 1 小时内有会话活动；C，有运行中的会话或有未提交的改动。
2. **行的点击。** 已决定：选 A，点击选中这一行并打开「相对主干」的审阅，与技术调研「点击后显示该 worktree 相对主干的 Diff」和项目主页一致；检查器跟随 ↑ ↓ 的选中，从审阅返回后显示刚才的行。不选：B，点击只选中，检查器显示详情，↵ 或检查器的按钮打开审阅。
3. **切换 worktree 之后的去向。** 已决定：选 A，停在 Worktrees，当前标记移到新行；Esc 回到项目主页时主页已是新的 worktree。不选：B，切换后回到项目主页（`PopToBase`）。
4. **进入 Worktrees 时是否自动显示检查器。** 已决定：选 A，跟随用户的检查器开关（⌥⌘I，全局保存），不自动打开；行内已有分支、会话、改动量和 token。不选：B，进入本屏幕时 `SetInspector(true)`。

## 验收

分三步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-worktrees`、`just deny` 通过。前置条件的顺序：C（`worktrees.ftl`）与 A 的 `services` 部分（类型、带默认实现的 trait 方法、事件变体）在第 1 步之前；B、`ui` 设计的第 3、4 步（控件、探针节点）与 workspace 的根视图在第 2 步之前；A 的 `review` 实现、D、E 随第 3 步（A 可以更早）。

**第 1 步：快照与插件（不开窗口的部分）。**

- `snapshot::build` 与「测试」里 `build_*`、`snapshot_*` 的 insta 快照通过。
- 插件与 `UsageLink` 的 `#[test]` 通过：`git_project_registers_the_sidebar_entry`、`folder_project_registers_no_entry`、`closed_drops_the_entry`、`interests_follow_the_screen`、`hidden_screen_ignores_events`、`events_coalesce_into_one_build`、`switches_count_active_changes`、`usage_link_empty_builds_without_usage`、`usage_activating_after_shown_takes_the_watches`、`usage_plugin_deactivation_rebuilds`、`same_wanted_set_does_not_rebuild`、`started_handles_an_already_open_project`、`changed_for_an_unknown_project_counts_as_opened`。
- `python3 scripts/check_architecture.py` 通过：`lectern-worktrees` 带 `plugin = true`，不依赖其他插件 crate；只有 `app` 依赖它。
- `rg 'std::fs|std::process|git2|gix|std::thread::spawn' crates/worktrees/src` 没有结果；`rg '^lectern-(review|project|agents|usage|workspace|project-home|git)(\s|\.|=)' crates/worktrees/Cargo.toml` 没有结果（行首锚定，不会匹配 `name = "lectern-worktrees"`）。
- `canary_stays_out_of_worktrees_logs` 通过（合成与插件部分）；`rg 'eprintln!|println!|log::' crates/worktrees/src` 没有结果。

**第 2 步：视图（`#[gpui::test]`）。**

- 「测试」里的 `#[gpui::test]` 和语义树快照通过，含 `activate_registers_contributions_only`。
- `rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|\.font_family\(|gpui_component::Button' crates/worktrees/src` 没有结果（`ui` 设计的持续检查）。
- `rg '"[A-Z][a-z]+ [a-z]' crates/worktrees/src --glob '!tests*'` 没有界面字符串字面量（都经 `keys::worktrees::*`）。
- `rg 'NodeId::new\("worktrees"' crates/worktrees/src` 有结果。
- `canary_stays_out_of_worktrees_logs` 覆盖渲染、按键、切换与刷新。

**第 3 步：装配、里程碑与基准（`Closes #16`，前置条件 D、E 随这一步；A 的 `review` 实现已先行完成，见「实际验证」，其中 `review/resident_100k_bytes` 的检查已做，第 3 步复查一次）。**

- `just test -p lectern-app`：`every_profile_graph_validates` 里 desktop 的插件数加二。
- 手动在 macOS 上运行发布构建（临时 `HOME`、至少 3 个 worktree 且有会话的仓库）：侧栏有 Worktrees 条目；屏幕按 Active / Idle 列出 worktree，带改动数与 token；↵ 打开相对主干的审阅，Esc 返回；「切换到这个 worktree」之后当前标记移动；日志里有 `worktrees milestone reported`。
- `just bench app/switch_worktree`：中位数 < 100 ms。`just bench worktrees/`：`worktrees/build_1000` < 2 ms、`worktrees/resident_bytes` < 256 KiB。
- `just bench review/resident`：`review/resident_100k_bytes` 含保留的 worktree 数据，仍 < 1 MB。
- `app/idle_empty` 与改动前相比增加不超过 32 KiB。

## 已知限制

- 非当前 worktree 没有文件监听：只改工作区文件、不写 index、也没有会话事件的改动（如人手工编辑），要到下一次触发（HEAD 或 index 变化、会话事件、⌘R，或重新进入屏幕且计数早于 5 分钟）才计入。检查器显示统计时间。
- 「相对主干」的合计是已提交与未提交路径的并集：已提交的改动在工作区里又改回原样时，这个文件仍计入，而「相对主干」的审阅里没有它。
- 改动路径超过 4000 个的 worktree 没有重叠信息。
- 相对时间只随合成刷新；Active 到期由一次性定时触发一次合成。
- `LoadingPhase::Progress` 的不确定进度：`ui` 没有这个控件（`Progress` 只有 `Bar` / `Ring` 两种确定的），超过 1 秒仍显示骨架，节点的 `phase=progress` 记下阶段；等 `ui` 有了控件再换（与项目主页相同）。
- `review` 的第一轮概览失败而概览关注者还在时，当前 worktree 的 `uncommitted` 与 `vs_base` 停在 `Pending`（见「与设计的差异 / 前置条件 A 的遗留」）：第 2 步的骨架在这时一直显示，直到 `review` 修复。
- 列表不虚拟化：worktree 多于 100 个时每帧的排版随行数增长。
- 本屏幕依赖 `dyn Review`：`review` 失败时整个屏幕不可用。
- P1 只读：不创建、删除或 prune worktree。

## 实际验证

### 第 1 步（快照与插件）

#339，`Refs #16`，在 macOS（arm64）上运行：

- `just check`（格式、架构检查的自测与检查、文档链接、整个工作区的 clippy）通过。`just test -p lectern-worktrees`：46 个测试通过，含 `build_*` 与 4 份 `snapshot_*` 的 insta 快照、「验收」第 1 步列的 13 个插件与 `UsageLink` 用例、`usage_link.rs` 的 6 个、`canary_stays_out_of_worktrees_logs`，另有 `build_1000_fixture_shape`（夹具的形状：17 个 worktree、1000 个会话、每个 worktree 8 个重叠项和一份分支用量）。`just deny`（advisories、bans、licenses、sources 通过，有既有的许可证允许项未用的警告）；`Cargo.lock` 只多了本 crate 的依赖边，没有新的外部包。
- 「验收」第 1 步的 `rg` 检查：`std::fs|std::process|git2|gix|std::thread::spawn`、对插件 crate 和 `lectern-git` 的依赖行、`eprintln!|println!|log::` 在 `crates/worktrees` 里都没有结果；`python3 scripts/check_architecture.py` 通过。
- `snapshot::build` 的耗时：不属于第 1 步的基准（`worktrees/build_1000` 随第 3 步进 `bench`）。用同一份夹具在 `cargo test --release` 里临时量过（200 次，没有留在仓库里）：中位数约 0.38 ms，最大约 0.87 ms，预算 < 2 ms。
- 没有验证：视图（第 2 步）；真实 kernel 里的 `usage` 与 `review` 插件（假服务代替，插件之间不依赖）；`warn` `usage call refused` 的写出（要 `usage` 插件的 token 在 `started` 与停用之间死掉，假服务做不出这个时刻）；`worktrees milestone reported` 和 `worktrees command` 等视图一侧的日志（第 2 步）；内存的 `worktrees/resident_bytes`（第 3 步）。

### 第 2 步（视图）

PR 号待定，`Refs #16`，在 macOS（arm64）上运行，没有打开真实窗口（窗口是 GPUI 的测试窗口）：

- `just check`（格式、架构检查、文档链接、整个工作区的 clippy）、`just test -p lectern-worktrees`（71 个测试通过：第 1 步的 46 个，其中 `activate_registers_nothing_and_does_no_work` 换成 `activate_registers_contributions_only`；新增 `views.rs` 的 `#[gpui::test]`（「测试」列的 15 个，另有 `the_dot_blinks_when_a_worktree_starts_running`、`the_page_shows_usage_of_merge_requests`、`without_a_base_the_range_is_against_head`、`a_switch_before_the_screen_is_built_is_not_reported`、`a_screen_built_before_the_snapshot_follows_it`、`a_repository_with_one_worktree_says_so`、`a_folder_project_says_it_is_not_a_repository`、`the_page_and_the_text_are_in_the_language_of_the_interface` 等）、`tree.rs` 的 3 份语义树 insta 快照、`canary_stays_out_of_worktrees_logs` 扩到渲染、↓、↵、切换按钮、⌘R 与关闭项目）、`just deny` 通过。
- 「验收」第 2 步的 `rg`：`hsla\(|rgb\(|…|gpui_component::Button`、`"[A-Z][a-z]+ [a-z]`（非测试）在 `crates/worktrees/src` 没有结果；`NodeId::new("worktrees"` 有结果；第 1 步的几条 `rg` 仍没有结果。
- 前置条件：`ui` 设计的第 3、4 步（#236、#242，另有第 5、6 步）、B（#336）、C、A 的 `services` 与 `review` 部分都已在 `main`。
- 没有验证：窗口里的实际显示（行的间距、徽标、骨架、检查器页的外观、`Expander` 与 `Disclosure` 的动画、圆点的轻闪）；`workspace` 真实地调用 `Screen::build`、`InspectorTab::build` 与 `toolbar`（测试里的 `Host` 代替它）；`ScreenChanged` 经真实窗口到达；`worktrees milestone reported` 到 `app` 的转接和 `app/switch_worktree` 基准（第 3 步）；`worktrees/resident_bytes`（第 3 步）；`LoadingPhase::Progress` 的不确定进度（`ui` 没有控件）；`review` 第一轮失败时骨架一直显示的情形（在 `review`，见「已知限制」）。

### 前置条件 B（`workspace` 与 `ui::contrib`）

#336，`Refs #16`，在 macOS（arm64）上运行：

- `just test -p lectern-workspace` 通过，含 `push_of_the_top_route_is_ignored`（`nav.rs`）和 `inspector_tab_scoped_to_its_screen`（`view.rs`，`#[gpui::test]`，不开真实窗口）。
- 与设计的差异：B.2 的「栈顶」按当前屏幕比较（栈空时是基底屏幕）；`InspectorTab::screen` 的类型写成 `Option<Cow<'static, str>>`（与 `Name` 同一类型，`ui` 不依赖 `kernel`）。原因见 [workspace 设计「与设计的差异」](workspace.md#与设计的差异)。
- 没有验证：真实窗口里页切换时的外观；还没有 `Some(WORKTREES)` 的页（第 2 步）。

### 前置条件 A 的 `services` 部分

#334，在 macOS（arm64）上运行：`just check` 的格式、架构检查（32 个 crate）和文档链接三步通过；整个工作区的 clippy 这台机器上没有 Metal 工具链，没有运行，改跑 `cargo clippy --locked -p lectern-services --all-targets -- -D warnings`（通过），整个工作区的 clippy 以 CI 为准。`just test -p lectern-services`（112 个测试通过，含 `worktrees_overview_round_trips`、`worktrees_methods_default_to_nothing` 和 `review_event_round_trips` 里新加的 `Worktrees` 变体）。依赖 `services` 的 `lectern-review`、`lectern-review-view`、`lectern-project-home`、`lectern-welcome` 用 `cargo check --locked --all-targets`（默认特性）通过，所以第 8 条的匹配分支（`review_view` 的 `on_review` 补了 `Worktrees` 分支；`project_home` 与 `welcome` 用 `let … else`，不需要改）已通过编译。没有验证：这几个 crate 的 `--all-features` clippy 和它们的测试；`review` 对新方法的实现（A 的 `review` 部分）。

### 前置条件 C

`worktrees.ftl`（#333），在 macOS（arm64）上运行：`just check` 的格式、架构检查（32 个 crate）和文档链接三步通过；其中整个工作区的 clippy 这台机器上没有 Metal 工具链，`gpui-pre-apple` 的构建脚本失败，改跑 `cargo clippy --locked -p lectern-i18n --all-targets --all-features -- -D warnings`（通过），整个工作区的 clippy 以 CI 为准。`just deny`（advisories、bans、licenses、sources 通过，有一条 `Unicode-DFS-2016` 许可证允许项未用的既有警告）、`just test -p lectern-i18n`（57 个测试通过）。「界面」键表的 51 个键在 en 与 zh-CN 里都有，变量一致（编译期校验）。没有验证：键在界面里的实际显示（视图在第 2 步）。

### 前置条件 A 的 `review` 部分

#337，在 macOS（arm64）上运行：`just check`（含整个工作区的 clippy，这台机器已装 Metal 工具链）、`just test -p lectern-review -p lectern-services`（399 个测试）通过。依赖没有改，没有运行 `just deny`。基准只跑不开窗口的两个目标：`review/worktrees_cloudprint` 中位数 686 ms（11 个 worktree、728 个文件、冷缓存，只记录），`review/resident_cloudprint_bytes` 84.8 KB、`review/resident_100k_bytes` 8.86 KB（含关注再放下之后保留的 worktree 数据，预算 < 1 MB，第 3 步复查）。测试覆盖第 7 条列的各项：未提交与已提交的计数、当前 worktree 复用概览的改动集（含概览为 `Capped`、第一轮未发布）、两个 worktree 的重叠和每个方向共用一个 `Arc`、`Capped` 没有重叠且不在别人的列表里、节流、没有关注者时不比较、放下关注后保留到项目关闭、没有主干，另有主干移动、HEAD 变化、rescan、会话事件、新增 worktree、路径哈希的释放和日志的金丝雀。没有验证：真实 fs_watch 批次到标记的整条路径（`Driven` 没有 watcher，批次直接交给引擎）；第 5 条估算的 17 个 worktree、每个 4000 个改动路径的内存（约 650 KB）没有基准；真实仓库上的耗时（见「与设计的差异」）；屏幕上的实际显示（第 2、3 步）。

## 与设计的差异

### 第 1 步（快照与插件）

技术决定，没有需要产品决定的：

- **本 crate 从第 1 步起就只在 macOS 上编译。** 设计写第 1 步不依赖 GPUI、Linux 上也跑。但 `WorktreeRow::node` 的 `NodeId` 和侧栏条目的 `SidebarItem` 载荷来自 `lectern-ui`，它依赖 `gpui`。Cargo.toml 依赖了 `lectern-ui` 和 `lectern-i18n`（`keys::worktrees::ENTRY`），没有直接依赖 `gpui`（第 2 步加）。「测试」一节已改。
- **`activate` 在第 1 步什么也不登记。** `Screen`、`InspectorTab`、`KeyBindings` 的载荷要视图工厂和 Action，随第 2 步登记；`activate_registers_contributions_only` 本来就是第 2 步的用例。第 1 步的 `activate` 只取服务引用、建空的 `Hub`、订阅四种事件。
- **时刻用日历时间，不用 kernel 的 `Clock`。** 设计写 `built_at` 取 kernel 的 `Clock`、测试用 `ManualClock`。kernel 的 `Clock` 是单调的 `Instant`，没有日期，而会话的 `last_activity` 是 UTC 毫秒，14 天和 10 分钟要拿同一个时钟比。做法与 project_home 相同：插件内部的 `Wall`（默认 `SystemTime`），测试经 `#[cfg(test)]` 的 `with_wall_clock` 固定。`WorktreesDeps` 不变。
- **`next_expiry` 是 `last_activity + 10 min + 1 ms`。** 设计写 `+ 10 min`，而「运行中」是 `≤ 10 min`：定时恰好在 `+ 10 min` 到点、合成的 `now` 与它相同时，会话仍算运行中，Active 不会移到 Idle，也不再有事件。加 1 ms 得到「第一个不再运行的时刻」。
- **行节点是 `worktrees.list.item[<WorktreeKey>]`。** 设计的表写 `worktrees.item[<WorktreeKey>]`，`NodeId` 的构造函数造不出它（`item` 总在 `视图.区域` 之后）。行当作列表的条目。其余节点 ID（`worktrees.detail.sessions.item[…]`、`worktrees.detail.overlaps.item[…]`）与表一致。第 2 步的其他节点（`….badge[…]`）跟随这个前缀。
- **prunable 的行：`overlaps` 为 `Unknown`，`usage` 为 `Absent`。** 设计写「还没有这一项为 `Pending`」，而 `review` 不比较 prunable 的 worktree，永远不会有这一项，骨架会一直显示；`usage` 也不为 prunable 的 worktree 发查询（`UsageLink` 的 `wanted` 里没有它）。检查器的文件重叠一节对 prunable 的行第 2 步可以直接不画。
- **`overlaps` 的 `Pending`。** `WorktreeChanges::overlaps` 为 None 时，`uncommitted` 或 `vs_base` 还是 `Pending` 算计数没有到达（`Pending`），其余算已到达（`Unknown`：`Capped`、`Unavailable`）。设计只写了「计数已到达」。
- **子 agent 会话并入父会话的条件。** 设计写「与父会话在同一个 worktree」。实现要求父会话本身也是一个计入的候选（最近 14 天内、已归位到这个项目的这个 worktree）；否则（父会话超过 14 天、还没归位、在别的 worktree）子 agent 会话单独计数。多层嵌套并入链上的第一个会话；`parent` 链有环时有界地终止，仍会计数。并入的子 agent 会话只影响父会话那一项的 `running` 和 `next_expiry`，`recent` 里 `tail_pending` 的请求只针对列出的会话本身，不含并入的子 agent 会话。
- **`Inputs` 带 MR 的快照。** `MrItem::tokens` 在 `mrs` 多于一项时取 `UsageSnapshot::Mr`，所以输入是 `UsageInputs { worktrees, mrs }`；`Built` 是 `{ snapshot, pending, queries }`，`queries` 是这份快照要关注的 `UsageQuery`（每个不是 prunable 的 worktree 一个 `Worktree`，`mrs` 多于一项的 worktree 再为每个 MR 一个 `Mr`，同一个 MR 只一个）。`Inputs` 另带 `switches` 和 `usage_generation`（纯函数不读 `Hub` 和 `UsageLink`）。合成任务先读各 worktree 的分支用量，再读其中多 MR 的各个 MR 用量；`Usage::snapshot` 只在有关注之后才有结果，所以第一轮没有 MR 的快照（`tokens` 为 None），`set_wanted` 取到关注、`UsageEvent::Changed` 到达之后第二轮补上。
- **`UsageLink` 的补充。** `generation()`（`SourceGenerations::usage`，每次调用 `on_change` 加一）和 `watching()`（日志用）。`on_change` 除了「取到或放下了关注」，也在 `dyn Usage` 出现或消失时调用（设计「usage 消失」一项的推广：`wanted` 为空时消失同样要重建，让快照去掉 token）。`UsageEvent::Limits` 不处理。
- **`Hub` 在第 1 步多出的公开方法。** `snapshot()`、`switches()`、`attach_view(view)`（返回 `UnboundedReceiver<Arc<WorktreesSnapshot>>`，消息是新快照，项目关闭时是一份没有行的快照，快照里的 `project` 是关闭的项目）、`request_build()`（定时到点用，`shown` 为 false 时忽略）；它们是「数据结构」里 `Hub` 的字段（唤醒发送端、`switches`）第 2 步读取的入口。
- **侧栏条目。** `Views` 区，ID `worktrees`，`order` 0（交互规范的顺序是 Worktrees 在 Files 之前），图标 `Icon::Worktrees`，登记时带激活的 token。
- **日志。** 多一条 debug `worktrees complete requested`（`sessions`：数量），与 project_home 的同类记录对应；`worktrees build dropped` 只带 `reason`。其余字段按「日志与诊断」。
- **测试。** 夹具 `fixture_1000` 在 `src/tests/build.rs`（`pub(crate)`），形状由 `build_1000_fixture_shape` 检查；第 3 步的基准要用它时再放到 `test-support` 特性后面（`crates/bench` 在第 3 步，这里没有动）。多出的用例：`build_merges_a_child_only_into_a_parent_that_counts`、`build_cuts_a_cycle_of_parents`、`build_queries_an_mr_once`、`build_folder_project`、`build_files_settled_and_generations`、`usage_link.rs` 的 6 个、插件一侧的 `activate_registers_nothing_and_does_no_work`、`opening_another_project_replaces_the_entry`、`usage_events_of_wanted_queries_recompose`、`usage_of_a_branch_in_several_mrs_asks_for_each_mr`、`complete_is_requested_once_per_pending_session`、`a_result_for_a_hidden_screen_is_dropped`、`a_result_for_a_closed_project_is_dropped`、`a_project_without_a_snapshot_is_skipped_and_recovers`、`views_get_each_snapshot_and_an_empty_one_when_the_project_closes`、`deactivation_drops_everything`。
- **前置条件 A 的遗留（`review`，未改）。** 第一轮的概览（`watch_overview`）没有发布过快照（例如第一轮失败）而概览关注者还在时，`overview_counted` 一直是 `Counted::Pending`，当前 worktree 的 `uncommitted` 与 `vs_base` 在下一次标记之前停在 `Pending`。这是读 `overview_counted` 的代码得出的，没有用测试复现。第 1 步只把 `Pending` 原样映射，没有显示，所以没有让它可见，也没有改 `review`；第 2 步的骨架让它在这个失败情形下一直显示（见「第 2 步」的差异和「已知限制」）。修法在 `review` 的概览状态里记一个「第一轮已失败」，之后按「没有概览」处理（自己比较），要在 `Driven` 里加失败的用例，不在第 1 步的范围。

### 第 2 步（视图）

技术决定，没有需要产品决定的：

- **本屏幕的所有 Action 都在屏幕上处理，包括 `OpenSession`、`ToggleOverlap`。** 检查器页没有键盘焦点，探针触发的 Action 只会落在焦点路径上的屏幕，所以屏幕的根视图监听全部十个 Action；检查器页的点击直接调用共享的 `WorktreesModel` 的同一批方法（经弱引用），两条路径做同一件事。
- **语义节点。**
  - 行的属性放在 `<行>.facts`（`Group`，行内容的节点），不放在 `Row` 自己的 `ListItem` 上（`Row` 没有属性，与项目主页相同）；一个节点至多 8 个属性（`ui::semantic`），设计的行属性有 9 个，所以 `tokens` 单独在 `<行>.tokens`（`Text`，数字或 `pending`，默认分支和没有 `usage` 插件时没有这个节点）。
  - 徽标是 `<行>.badge.item[current / locked / missing]`，设计的 `badge[…]` 造不出（与第 1 步的 `list.item` 同一原因）。
  - `GroupedList` 自己声明节点，角色是 `List`，不是设计表里的 `Group`（`worktrees.active`、`worktrees.idle`）；`worktrees.list` 是外面包的 `List` 节点，属性 `rows`、`active`、`idle`、`ready`，另有 `phase`（`nothing` / `skeleton` / `progress`）。
  - `worktrees.detail.usage.mr` 的键是 MR 的编号；检查器里重叠项与会话行的属性在各自的 `….facts` 上，重叠项展开的路径在 `….paths`（`Expander`）。空状态节点是 `worktrees.empty`（文件夹项目）和 `worktrees.detail.empty`（检查器没有选中的行）。
  - `ready` 为 true 需要概览已到达、改动计数、重叠和用量都不在 `Pending`。
- **一个模型，一个唤醒通道。** `WorktreesModel` 在建好时向 `Hub` 登记一次（`attach_view("screen")`），检查器页不登记，它经 `cx.observe` 跟随模型；唤醒消息就是新快照（项目关闭时的空快照只能经这条通道到达，`Hub::snapshot()` 此时为 None）。日志 `worktrees view attached` / `detached` 的 `view` 为 `screen` 或 `detail`。
- **`Hub` 多出的方法。** `now()`（模型用合成所用的时钟算定时的延迟）、`dispatch(command, target)`（记 debug `worktrees command`）、`refresh(project)`、`log_switch(project, worktree)`（记 info `worktree switch requested`，下标取自 `ProjectSnapshot::worktrees`，经 `Projects::snapshot`，取锁复制 `Arc`）、`report_milestone(switches)`（记 info `worktrees milestone reported`）。
- **选中与展开。** 选中换到另一个 worktree 时，检查器里展开的重叠项收起（设计只写了选中的保持规则）。检查器里 `recent` 为空时显示 `worktrees-no-sessions`（设计没有写）。
- **`vs-base` 的显示。** 有主干时显示计数；`Pending` 时不论有没有主干都显示骨架（第一份概览到达之前主干还不知道）；没有主干且计数不是 `Pending` 时不显示。
- **时钟与定时。** 展开定时用 `Duration::from_secs_f64`（「验收」的 `rg` 不允许 `Duration::from_millis`）；定时到点后只请求一次合成，`next_expiry` 不变时不再设定时（时钟回拨时会停在 Active，下一个事件修正）。测试里的 `Rig` 的时钟可以前进（`advance`），否则到点后的合成仍看到同一个 `now`。
- **不确定进度。** 见「已知限制」：`ui` 没有控件，`LoadingPhase::Progress` 继续显示骨架（`ui` 的控件缺口，没有写在本 crate 里）。
- **测试。** `activate_registers_nothing_and_does_no_work` 换成 `activate_registers_contributions_only`；`FakeContributions` 的 `dispatch` 与 `report_milestone` 改为记录，`FakeReview` 记录 `refresh_worktrees`；第 1 步里数侧栏条目的用例改用 `entries()`，`deactivation_drops_everything` 的注销数从 1 变为 4。语义树快照的文件名由 insta 按模块与函数名生成。
- **前置条件 A 的遗留（`review`，未改）在第 2 步可见。** `review` 的第一轮概览失败而概览关注者还在时，当前 worktree 的 `uncommitted` 与 `vs_base` 停在 `Pending`，现在会让那一行的计数位置一直画骨架。它在 `review` 的概览状态里，不是小改动（要在 `Driven` 里加失败的用例），没有在本 PR 里改；记入「已知限制」。

### 前置条件 A 的 `review` 部分

细节在 [review 设计「与设计的差异」](review.md#并行-worktree-总览worktrees-设计的前置条件-a-的-review-部分)，这里列影响本 crate 的几项：

- 第一个 worktree 比较完之前 `Review::worktrees` 为 None；之后每比完一个发布一次，还没比较的 worktree 三个计数是 `Pending`、`overlaps` 为 None（与「合成规则」的 `changes` 行一致）。`snapshot::build` 要处理「`WorktreesOverview` 还没有到达」。
- 当前 worktree 在概览的第一轮发布之前，`uncommitted` 与 `vs_base` 是 `Pending`；概览发布时更新（不经 `Refresh` 或节流）。
- `committed` 与 `vs_base` 为 `Unavailable` 的情形比设计多：没有主干、HEAD 为 unborn 或读不出、没有共同历史、比较失败；这时 `overlaps` 用 `uncommitted` 的路径。`WorktreesOverview::base` 为 None 时界面按「没有主干」处理，但引用名存在而解析不到提交时也是 None。
- `vs_base` 的并集超过 4000 个路径时为 `Capped`（设计只写了任一为 `Capped`）。
- `review/worktrees_cloudprint`（只记录）的值比「性能」里「cloudprint 形状每个 worktree 几毫秒」的估算大一个数量级（约 60 ms），原因是夹具的索引没有 stat 信息；真实仓库上的耗时没有测量。

### 前置条件 A 的 `services` 部分

- 类型、三个方法的签名和默认实现、`ReviewEvent::Worktrees` 与设计一致。唯一的补充：类型派生了 `Clone`、`PartialEq`、`Eq`、`Hash`、`Debug`、`Serialize`、`Deserialize`（`ChangeCount` 另有 `Copy`），与 `Overview` 相同，进程插件的代理要序列化它们。
- A 要求 `review` 的设计文档同时更新「接口」「概览」「性能」和调用方一节；`docs/design/review.md` 留给 A 的 `review` 部分的 PR，本 PR 没有改它。

### 前置条件 C

- 键表里 `worktrees-vs-base` 在 en 里分成多行的选择器，文字与键表一致；注释里补了 `$base`、`$time`、`$tokens`、`$limit` 的含义。没有其他差异。
