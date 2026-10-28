# project_home 设计

状态：实现中。前置条件 B（`project-home.ftl`，[#237](https://github.com/SoloJiang/lectern-app/pull/237)）与「验收」第 1 步（不开窗口的部分：`snapshot::build`、两个插件、`UsageSlot`，[#241](https://github.com/SoloJiang/lectern-app/pull/241)）、第 2 步（视图与贡献登记：`HomeScreen`、`HomeSidebar`、Action 与快捷键、语义节点）已实现（[#248](https://github.com/SoloJiang/lectern-app/pull/248)）；`app` 的装配（前置条件 D 的装配部分，[#247](https://github.com/SoloJiang/lectern-app/pull/247)）已实现，见「实际验证」「与设计的差异」；第 3 步（`app` 装配、里程碑的基准与 `Overview.packages` 的接入）尚未实现。「界面」已为 [#283](https://github.com/SoloJiang/lectern-app/issues/283) 按设计画布修订，已实现的部分要随之修改，见「与设计的差异」。

对应 issue [#30](https://github.com/SoloJiang/lectern-app/issues/30)。`project_home` 是打开项目后首先看到的屏幕（[交互规范「屏幕」](interaction.md#屏幕)的 02 画板，[ADR 0012](../adr/0012.md)）：项目侧栏（Overview、最近的 Sessions、带今天改动数的 Packages、其他 crate 贡献的 Worktrees / Files / MR 入口、底部的 Usage 卡片）和主区（Needs Review 的会话卡片、Outside Any Session、Recently Done）。它只显示领域层算好的快照（`project`、`agents`、`review`、`usage`），自己不做归位、比较和统计；它由 [workspace 设计](workspace.md)的根视图承载，作为 `Screen(PROJECT_HOME)` 和 `SidebarPanel("project")` 贡献进去。

## 范围

- 做：
  - 项目侧栏面板：Overview 条目；Sessions 的会话行（所有 worktree 的会话按时间混排，agent 圆点、标题、运行中圆点；只列最近的几个，其余折成「N more」）；Packages 的子项目行与今天的改动数；`workspace` 收到的其他贡献条目（`Views` 区，画在「Project」小标题下面：Worktrees [#16](https://github.com/SoloJiang/lectern-app/issues/16)、Files [#10](https://github.com/SoloJiang/lectern-app/issues/10)、Merge Requests [#25](https://github.com/SoloJiang/lectern-app/issues/25)）；底部 Usage 卡片（本项目最近 7 天的 token 与最紧张的额度条）。
  - 主区屏幕：大标题一句话说明待审数量，下面一句说明运行中的 agent 数和会话外的改动数；Needs Review、Outside Any Session、Recently Done 三个分组列表；卡片的状态与信号文字、已看进度环。
  - 工具栏：标题、「分支 · N worktrees」副标题，右边的 ⌘K 搜索框和 New session 按钮的位置（按钮本身归别的 crate，见「界面」）。
  - 快照 `HomeSnapshot`：由本 crate 的插件在后台任务里从四个服务的快照合成，视图只读它。
  - 概览的关注（`Review::watch_overview`）与用量的关注（`Usage::watch`）的持有时机；收到 `ReviewEvent::Overview`、`ProjectEvent::Changed`、`SessionsChanged`、`UsageEvent` 后的刷新。
  - 导航：↑ ↓ 选会话、↵ 进入审阅、点 Outside 进入会话外改动的审阅，都经 `workspace` 的状态机；`Milestone::ProjectHome` 的报告。
  - Fluent 文案、UI 探针的语义节点、Action。
- 不做：
  - 会话归位、改动统计、已看进度、Drifted、critical 的计算：`agents`、`review`（[review 设计「概览」](review.md#概览)）。本 crate 不读会话文件、不比较工作区。
  - worktree 列表、文件列表、子项目列表：`project`（[project 设计「快照」](project.md#快照)）。
  - token 的统计与额度：`usage`（[usage 设计](usage.md)）。
  - 窗口骨架、侧栏容器、工具栏、导航状态机、贡献注册表：`workspace`。
  - 颜色、字体、控件与动效原语：`ui`（[#218](https://github.com/SoloJiang/lectern-app/pull/218) 的设计；[ADR 0015](../adr/0015.md)）。本 crate 不写任何颜色值、字体名和时长。
  - Worktrees 屏幕、Files 面板、MR 屏幕、Usage 屏幕的内容：各自的 crate（`worktrees`、`file_reader`、`forge` 的视图、`usage_view`）。本 crate 只画它们贡献的侧栏条目。
- 本 issue 之外：
  - 会话卡片的「公共包改动」「API 变化」信号：要 `code_intel` 实现 `BriefRules` 提供者（[review 设计「简报规则的扩展点」](review.md#简报规则的扩展点briefrules)，review 的前置条件 F），并且 `SessionOverview` 要带上提供者条目的规则 ID（现在只有 `critical` 的计数）。两者都合并之前，这两条信号不显示。
  - 「测试结果」信号：来自 MR 的流水线状态，随 [#25](https://github.com/SoloJiang/lectern-app/issues/25) 加入。
  - 空状态里的「最近提交」和「New session」（[交互规范「状态与异常」](interaction.md#状态与异常)）：最近提交要 `project` 或 `git` 提供一个提交列表的接口，New session 归 `terminal`。本 issue 只显示空状态的一行文字。
  - 「Agent 请求权限」的等待标记（交互规范「状态与异常」）：`SessionSummary` 没有这个字段，随 Conversation（[#22](https://github.com/SoloJiang/lectern-app/issues/22)）的设计加入。
  - 进入会话时列表行标题平移放大成审阅页标题的共享元素动画（[交互规范「动效」](interaction.md#动效)）：跨越本 crate 与 `review_view` 两个视图，第一版只做行的选中态与屏幕切换，共享元素动画在 `review_view` 实现后另开一步。
  - Packages 行的点击（过滤会话或打开 Files 到该目录）：等 Files（#10）与 `SessionOverview` 带子项目信息之后。
  - 按 worktree 的 Needs Review：`Overview.sessions` 只含当前 worktree，其他 worktree 的会话在侧栏里可见（与当前 worktree 的会话混排）、可进入审阅，但不出现在主区（见「未决问题」1）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui` | 视图、`uniform_list`、Action、`#[gpui::test]` | 已锁定（[ADR 0016](../adr/0016.md)） |
| `lectern-ui` | `colors`、`strings`、`Text`、`TextRole`、`GroupedList`、`Row`、`AgentDot`、`Progress`、`Button`、`Skeleton`、`loading_phase`、`motion`、`semantic`、`contrib` 的载荷类型 | 视图层的公共库，非插件 |
| `lectern-i18n` | `keys::project_home::*`、`Args`、`count_compact`、`Strings::relative_time` | [ADR 0013](../adr/0013.md) |
| `lectern-kernel`、`lectern-services` | `Plugin`、`Context`、`PluginHandle`、`ServiceSet`；`dyn Projects`、`dyn SessionSource`、`dyn Review`、`dyn Usage`、`dyn Contributions` 与它们的数据类型和事件 | 架构规则 7（[ADR 0025](../adr/0025.md)）：插件只经 `services` 往来 |
| `lectern-executor` | 合成快照的 `Background` 任务 | 后台任务只经 executor |
| `lectern-logging` | 日志宏、`Value::uuid` | [ADR 0046](../adr/0046.md) |
| `futures`（`std` feature） | 视图的唤醒通道 `mpsc::unbounded` | 第 2 步加入（`workspace` 设计第 2 步已把它放进工作区依赖，`workspace` 同样用它做主线程队列） |
| `lectern-kernel`（`test-support`，dev）、`insta`（dev） | `TestKernel`、语义树与快照的 insta | |

本 crate 是插件 crate（`[package.metadata.lectern] plugin = true`），含两个插件（见「本 crate 的公开项」）：`project-home`（requires `dyn Projects`、`dyn SessionSource`、`dyn Review`、`dyn Contributions`）和 `project-home-usage`（requires `dyn Usage`）。它不依赖 `project`、`agents`、`review`、`usage`、`workspace` 这些插件 crate（dev-dependency 也不行，`scripts/check_architecture.py` 第 6 条），只依赖 kernel 层和非插件的库 crate；只有 `app` 依赖它。测试用的假服务都在本 crate 自己的 `testing` 模块里（「测试」），不借用 `review` 的 `test-support`。所在层与依赖方向见 [crates/project_home/AGENTS.md](../../crates/project_home/AGENTS.md)。

## 接口

### 本 crate 的公开项

```rust
/// `app` 装配时构造，放进 `PluginSet`（id "project-home"）。
pub struct ProjectHomePlugin { /* deps, usage slot */ }

pub struct ProjectHomeDeps {
    pub executor: Executor,
    /// 与 `ProjectHomeUsagePlugin` 共用的一格（见下）。`app` 构造一个，交给两个插件。
    pub usage: UsageSlot,
}

impl ProjectHomePlugin {
    /// 不做 IO。
    pub fn new(deps: ProjectHomeDeps) -> Self;
}

// Manifest { provides: [],
//            requires: [service::<dyn Projects>(), service::<dyn SessionSource>(), service::<dyn Review>(),
//                       service::<dyn Contributions>()],
//            activation: [AfterFirstFrame] }
// activate：取服务引用（`service`、`service_set`），建空的 `Home` 状态；经 `dyn Contributions` 登记
//   `Screen(PROJECT_HOME)`、`SidebarPanel("project")`、`SidebarEntry("overview")`、`KeyBindings`（`register(…, None)`，
//   `activate` 里没有 token），守卫交给 `ctx.own_staged`；`ctx.on::<ProjectEvent>`、`ctx.on::<SessionsChanged>`、`ctx.on::<ReviewEvent>`、`ctx.on::<WorkspaceEvent>`（只转给视图，见「数据结构」）。
//   不做 IO，不提交任务。
// started：保存 `PluginHandle`；把「标记 dirty 并安排合成」的回调写进 `UsageSlot::on_change`；读一次
//   `Projects::open_projects()`，已有打开的项目时按 `Opened` 处理（监听在 token 变为 `Live` 之前收不到事件，
//   kernel 设计「激活时的上下文」）。`started` 与 token 变为 `Live` 之间到达的 `Opened` 仍可能漏掉，所以监听里
//   收到不认识的项目的 `ProjectEvent::Changed`、且 `Projects::snapshot` 里还有它时同样按 `Opened` 处理（project 设计：成功的 `Opened` 之后至少还有一次 `Changed`；
//   `Closed` 之后到达的 `Changed` 找不到快照，忽略）。

/// `app` 装配时构造，放进 `PluginSet`（id "project-home-usage"）。只负责把 `dyn Usage` 放进共用的一格：
/// `usage` 插件失败或停用时项目主页照常显示，只是没有 Usage 行（kernel 没有可选依赖，
/// 做法与 review 设计前置条件 F 的 "code-intel-brief" 相同）。
pub struct ProjectHomeUsagePlugin { /* usage slot */ }
impl ProjectHomeUsagePlugin {
    /// `executor` 跑 `started` 里提交的那一个对账任务。
    pub fn new(executor: Executor, slot: UsageSlot) -> Self;
}

// Manifest { provides: [], requires: [service::<dyn Usage>()], activation: [AfterFirstFrame] }
// activate：取 `ServiceRef<dyn Usage>`，`ctx.on::<UsageEvent>`（监听调 `UsageSlot::changed`）。不写槽位：`activate` 不得有副作用。
// started：`UsageSlot::set_usage(service, token)` 只记下服务与 token（`started` 里 token 还是 `Pending`，`enter()` 取不到准入；
//   `started` 也不得直接改外部状态），然后经 `handle.own_with` 提交一个只调 `UsageSlot::reconcile()` 的任务：闸门在
//   token 变为 `Live` 之后才放行，所以对账在 `Live` 之后进行（kernel 设计「激活时的上下文」的 `SubmitGate`）。
// 停用：`Active` drop 时 `UsageSlot::clear_usage()`，放下关注并让主插件重建一次快照（Usage 行消失）。

/// 两个插件共用的一格。两个插件都是 `AfterFirstFrame`、彼此独立，激活先后不定；`Opened` 也可能早于 usage 插件的
/// `started`。所以关注由槽位对账（`reconcile`），谁后到谁触发，不依赖顺序：
/// `usage` 与 `project` 都有、还没有关注 → `token.enter()` 内调 `Usage::watch(Project { Open(project), D7 })`；
/// 任一缺失（或 `enter()` 取不到准入）→ drop 关注；之后调 `on_change`（主插件 `started` 里登记的回调：标记 dirty、安排合成）。
/// `reconcile` 由 `set_project`、`clear_usage` 直接调用（都在 `Live` 的插件代码里），由 `set_usage` 之后的任务调用（见上）。
/// 主插件每次调用 `dyn Usage` 之前 `token.enter()`，取不到就当作没有 usage。
#[derive(Clone, Default)]
pub struct UsageSlot { /* Arc<Mutex<{ usage: Option<(ServiceRef<dyn Usage>, ActivationToken)>, project: Option<ProjectKey>,
                            interest: Option<UsageInterest>, on_change: Option<Arc<dyn Fn() + Send + Sync>> }>> */ }
impl UsageSlot {
    pub fn set_usage(&self, usage: ServiceRef<dyn Usage>, token: ActivationToken); // usage 插件 started：只记录
    pub fn clear_usage(&self);                                                      // usage 插件停用；之后 reconcile
    pub fn set_project(&self, project: Option<ProjectKey>);                         // 主插件 Opened / Closed；之后 reconcile
    pub fn set_on_change(&self, f: Arc<dyn Fn() + Send + Sync>);                    // 主插件 started
    pub fn changed(&self);                                                          // `UsageEvent` 的监听：只调 on_change
    /// 对账（见上）。任何线程，不阻塞；usage 插件 `started` 之后的任务、`set_project`、`clear_usage` 调用。
    pub fn reconcile(&self);
    /// 合成任务用：`token.enter()` 成功时返回服务引用与准入，否则 None。
    pub fn usage(&self) -> Option<(ServiceRef<dyn Usage>, Admission)>;
}
```

`reconcile` 在槽位的锁外调用 `Usage::watch` 和 drop 旧关注（两者都立即返回、不阻塞）；`on_change` 在任何线程上调用，只改标记。

- `activate`：kernel 线程，只登记；`started`：kernel 线程，不阻塞。
- 事件监听：executor 线程，只改标记、安排一次合成任务（「数据流与线程」）。
- 视图工厂（`Screen::build`、`SidebarPanel::build`）：主线程，由 `workspace` 在需要时调用（屏幕压栈、侧栏面板显示），返回 `HomeScreen` / `HomeSidebar` 的 `AnyView`；不做 IO。

### 视图读取的快照

```rust
/// 不可变，整体替换。由 `project-home` 插件在 executor 的 `Background` 任务里合成（「数据流与线程」），
/// 视图只持有 `Arc`。不含 GPUI 的元素、视图和上下文，不含格式化好的文字：文字在渲染时由 `strings(cx)` 生成；
/// `NodeId`（`ui::semantic` 的字符串新类型，`Send + Sync`）随快照建一次，每帧只克隆（ui 设计「探针」）。
pub struct HomeSnapshot {
    pub project: ProjectKey,
    /// 合成时各来源的代数，用来丢弃过时的任务结果。
    pub sources: SourceGenerations,
    /// `ProjectSnapshot::name`，工具栏标题，原样显示。
    pub name: Arc<str>,
    /// 合成时刻。相对时间（「3 min ago」）从它往回数，所以一帧只取决于快照，时间只随下一份快照前进（「已知限制」）。
    pub now: Timestamp,
    /// 当前 worktree 的 HEAD 的短名（`refs/heads/main` → `main`；detached 取 OID 前 8 位），文件夹项目为 None。
    pub branch: Option<Arc<str>>,
    pub active_worktree: Option<WorktreeKey>,
    /// `ProjectSnapshot::worktrees` 的长度，工具栏副标题的「N worktrees」；文件夹项目为 0。
    pub worktree_count: u32,
    /// `ProjectSnapshot::active.files` 已不是 `Pending`：里程碑的条件（「性能」）。
    pub files_settled: bool,
    pub sidebar: SidebarModel,
    pub main: MainModel,
    /// `usage` 插件不在时为 None。
    pub usage: Option<UsageModel>,
}

pub struct SourceGenerations { pub project: u64, pub sessions: Arc<[(SourceId, u64)]>, pub overview: Option<u64>, pub usage: Option<u64> }

pub struct SidebarModel {
    /// 按 `ProjectSnapshot::worktrees` 的顺序（主 worktree 在前）；文件夹项目只有一组、`worktree` 为 None；
    /// 归位到不在列表里的 worktree（已 prunable）的会话在最后一组 `Other`。没有会话的 worktree 不出现。
    pub groups: Arc<[WorktreeGroup]>,
    /// 侧栏里会话行的总数（探针属性与日志用）。
    pub session_rows: u32,
    pub packages: Arc<[PackageRow]>,
    /// 今天的改动数是否可用：`Overview.packages` 为 None（概览未就绪、`Capped`、或 review 还没有实现这一项）时为 false，只显示名字。
    pub package_counts: bool,
    /// 侧栏滚动区里来自快照的行，拍平、等高：Sessions 标题、会话行（所有组的会话按 `last_activity` 从新到旧混排；
    /// 只显示最近的几个、其余折成「N more」是视图的状态，快照给全部）、Packages 标题、子项目行，
    /// 按显示顺序。贡献的条目（`Project` 区在前、`Views` 区在后）不在快照里：视图从 `sidebar_entries()` 取，
    /// 在渲染时拼成完整的行表（`Project` 条目 + `rows` + `Views` 条目），`ContributionsChanged` 时重取。
    /// 整个滚动区是一个 `uniform_list`，长度是三段之和，每帧只建可见的行（「界面」「性能」）。
    pub rows: Arc<[SidebarRow]>,
}

/// 侧栏滚动区里来自快照的一行；下标指向 `groups` / `packages`。
pub enum SidebarRow {
    SessionsHeader,
    Session { group: u32, index: u32 },
    SessionsEmpty,
    PackagesHeader,
    Package(u32),                       // `packages` 的下标
    PackagesSkeleton,                   // 子项目 `Pending` 时的一行骨架
}

pub struct WorktreeGroup {
    pub worktree: Option<WorktreeKey>,
    pub kind: GroupKind,          // Worktree { branch: Arc<str>, is_active: bool } | Folder | Other
    pub sessions: Arc<[SessionRow]>,
    pub node: NodeId,
}

/// 侧栏的一行和主区的一张卡片共用。
pub struct SessionRow {
    pub key: SessionKey,
    pub agent: Agent,
    /// None 时显示 `project-home-untitled-session`。
    pub title: Option<Arc<str>>,
    pub last_activity: Timestamp,
    /// 这一行所在位置的探针节点：侧栏行的，或 Needs Review / Recently Done 里卡片的（同一个会话在两处各有各的 ID）。
    pub node: NodeId,
    /// `open_turn && last_activity` 在合成时刻的 10 分钟之内。比 agents 设计的「活跃」（两者满足其一）更严：
    /// 最后一轮没有结束、但几天没有动静的会话是中断的会话，不是运行中。
    pub running: bool,
    pub tail_pending: bool,
    pub node: NodeId,
}

pub struct PackageRow {
    pub dir: RelPath,
    pub name: Arc<str>,
    /// 嵌套层数（沿 `Package::parent` 数到根），侧栏按它缩进。
    pub depth: u8,
    /// 今天改动的文件数；`package_counts` 为 false 时为 0。
    pub changed_today: u32,
    pub node: NodeId,
}

pub enum SessionStatus { Running, Waiting, Ended }

pub struct MainModel {
    /// 项目里正在运行的会话数（`SessionList::in_project(key)` 里 `running` 为 true 的，不限 worktree），大标题下面那句说明用。
    pub running: u32,
    /// 概览还没有第一份结果（`Review::overview` 为 None）：主区按 `loading_phase` 显示骨架。
    pub overview_ready: bool,
    /// 大标题的数字，等于 `needs_review.len()`。
    pub needs_review: Arc<[Card]>,
    pub outside: OutsideRow,
    pub done: Arc<[Card]>,
}

pub struct Card {
    pub session: SessionRow,
    /// 会话所在 worktree 的分支短名（来自它所在的 `WorktreeGroup`）；文件夹项目、`Other` 组为 None。卡片第一行显示。
    pub branch: Option<Arc<str>>,
    /// 卡片第一行的状态词。现在只有 `Running`（来自 `running`）；`Waiting`、`Ended` 要 `agents` 给出会话的结束状态
    /// （「对其他 crate 的要求」F），在那之前为 None，第一行不显示状态。
    pub status: Option<SessionStatus>,
    /// `SessionOverview::progress`；None 时进度环不显示。
    pub progress: Option<Progress>,
    pub signals: Signals,
}

/// 第二行灰字的来源（「界面」的信号表）。
pub struct Signals {
    pub drifted: bool,            // `SessionOverview::drifted == Some(true)`
    pub critical: u32,            // `SessionOverview::critical`
    pub changed_since_seen: u32,  // `Progress::changed_since_seen`
}

pub enum OutsideRow {
    /// 文件夹项目没有基准，不显示这一行。
    Hidden,
    Pending,
    /// `names` 是前 3 个改动路径（卡片第二行）；`review` 的 `OutsideCount` 现在只有计数（「对其他 crate 的要求」F），
    /// 在它给出路径之前为空，卡片只有第一行。
    Files { count: u32, names: Arc<[RelPath]> },
    /// `OutsideCount::Capped(n)`：显示全部改动路径数并说明没有区分。
    Capped(u32),
}

pub struct UsageModel {
    /// 最近 7 天的合计（`ProjectUsage::total.total()`），快照还没算出时为 None。
    pub tokens_7d: Option<u64>,
    /// `Limits::tightest` 指向的窗口：agent、`used_percent`（0..=100 取整）、`window_minutes`（额度窗口的长度，卡片上的「5-hour」）、`resets_at`。
    pub tightest: Option<TightestLimit>,
}
pub struct TightestLimit { pub agent: Agent, pub used_percent: u8, pub window_minutes: Option<u32>, pub resets_at: Option<Timestamp> }
```

分类规则（`snapshot::build` 的纯函数，`#[test]` 覆盖，见「测试」）：

| 结果 | 条件 |
|---|---|
| 侧栏 Sessions 的行 | `SessionList::in_project(key)` 里 `parent` 为 None（子 agent 会话在父会话的审阅里，不单独成行）、`last_activity` 在最近 14 天内（与概览的声明窗口相同）的会话；各组的会话合在一起按 `last_activity` 从新到旧（侧栏混排，组只用来给卡片取分支） |
| 会话所在的组 | `Place::Located(l)` 取 `l.worktree`；`Place::Resolved(r)` 取 `r.worktree`（项目刚打开、还没重新归位时先按它，归位后随下一次 `SessionsChanged` 更新）；在 `ProjectSnapshot::worktrees` 里找不到的进 `Other` |
| Needs Review 的卡片 | `Overview.sessions` 里 `progress` 为 None，或 `seen < total`，或 `changed_since_seen > 0` 的；保持概览的顺序（`last_activity` 从新到旧） |
| Recently Done 的卡片 | `Overview.sessions` 里 `total > 0 && seen == total && changed_since_seen == 0` 的 |
| `Overview.sessions` 里的会话不在 `SessionList` 里 | 丢弃（会话文件刚被删除，下一轮概览会去掉它） |
| Packages 的行 | `ProjectSnapshot::active.packages` 为 `Ready` 时的全部子项目，按 `dir`；`changed_today` 取 `Overview.packages` 里同一 `dir` 的计数（「对其他 crate 的要求」A） |
| `running` | `open_turn && now - last_activity ≤ 10 min`；`now` 是合成时刻 |

### 命令与事件

本 crate 不提供服务、不发自己的事件。视图发出的只有 `WorkspaceCommand`（经 `dyn Contributions::dispatch`，[workspace 设计「服务接口」](workspace.md#服务接口lectern-services-的-workspace-模块)）和 `SessionSource::complete`：

| 用户动作 | 命令 |
|---|---|
| 侧栏 Overview 条目、⌘[ | `PopToBase`（`workspace` 处理） |
| 侧栏会话行或主区卡片的点击、↵ | `Push(Route { screen: REVIEW, target: Target::Review(ReviewTarget::Session(key)) })` |
| Outside Any Session 的点击、↵ | `Push(Route { screen: REVIEW, target: Target::Review(ReviewTarget::Outside { project, worktree: active }) })` |
| `Views` 区的贡献条目 | 条目自己的 `on_select`（如 `ShowSidebarPanel(Some("files"))`、`Push(WORKTREES)`） |
| 底部 Usage 行 | `Push(Route { screen: USAGE, target: Target::Project(key) })` |
| 会话行显示出来且 `tail_pending` | 合成快照的任务对这些键调一次 `SessionSource::complete`（按键去重，补齐后随 `SessionsChanged` 更新） |

## 数据结构

- `Home`（插件状态，`Mutex`）：当前项目的 `ProjectKey`、`OverviewInterest`、`Option<Arc<HomeSnapshot>>`、合成任务的标记（`dirty`、`building`）、已请求 `complete` 的会话键集合（随项目关闭清空）、视图的唤醒发送端列表 `Vec<mpsc::UnboundedSender<()>>`（消息不带内容，视图被唤醒后自己从 `Home` 读快照，几次唤醒合成一次刷新）、`Opened` 的时刻与 `OpenRequest`、里程碑是否已报告、当前屏幕是不是主页（侧栏的 Overview 条目据此选中）。
- `HomeScreen`（GPUI `Entity`，主区）：`Arc<HomeSnapshot>`、选中项（`Pick::Session(SessionKey)` 或 `Pick::Outside`，按身份而不按位置保存：新快照把卡片重新排序时选中项跟着会话走，会话消失时清空）、`FocusHandle`、滚动句柄、每张卡片进度环的 `motion::Transition`（按 `SessionKey` 保存，快照换代时按键沿用，让进度环从旧值过渡到新值）、等待概览时的计时（`loading_phase` 的起点与两个阈值的一次性定时器）、上一帧各项的位置（滚动到可见用）。
- `HomeSidebar`（GPUI `Entity`，侧栏面板）：`Arc<HomeSnapshot>`、滚动区的 `UniformListScrollHandle`、最近一次 `sidebar_entries()` 的 `Arc`（`ContributionsChanged` 时重取，视图订阅这个事件；贡献条目的行由视图从它拼出，不经合成任务）、展开状态不保存（分组总是展开）。
- 两个视图在 `workspace` 的工厂里建：建好时 `Home::attach_view` 登记自己的发送端、再从 `Home` 读当前快照（先登记后读，两者之间到达的变化会再唤醒一次，不会漏）；视图 drop 时发送端随之失效，`Home` 在下一次唤醒发送出错时清掉它。视图经 `Home` 调 `Contributions`（`dispatch`、`sidebar_entries`、`report_milestone`）：`ServiceRef::get` 给的准入只在一次调用里持有，视图不存 `Arc<dyn Contributions>`。`workspace` 的 `ContributionsChanged` 与 `ScreenChanged` 由本插件的监听转给视图（视图不能直接订阅 kernel 事件）：前者让侧栏重取条目，后者让 Overview 条目的选中跟着当前屏幕。侧栏面板被 Files 替换再切回时重新建一个视图，所以它必须从 `Home` 读到最新快照，不能等下一次事件。
- 快照不序列化、不落盘；本 crate 没有派生数据，也没有以 blob OID 为键的缓存（架构规则 5 不涉及）。

## 数据流与线程

```
kernel 线程：activate（登记贡献、订阅）─▶ started（已打开的项目按 Opened 处理）
executor：ProjectEvent::Opened { Ok(key) }（新基底）─▶ 监听 ─▶ Home：project = key，watch_overview(key)，
          UsageSlot::set_project(Some(key))（有 usage 时在槽位里取 Usage::watch），dirty ─▶ 安排合成
kernel 线程：project-home-usage 的 started ─▶ UsageSlot::set_usage（只记录）─▶ own_with 提交对账任务
executor [Background，token 已 Live]：UsageSlot::reconcile ─▶ 已有项目时取 Usage::watch ─▶ on_change ─▶ 安排合成
executor：ProjectEvent::Changed / SessionsChanged / ReviewEvent::Overview { project } ─▶ 监听：dirty ─▶ 安排合成
executor：UsageEvent ─▶ usage 插件的监听 ─▶ UsageSlot::changed ─▶ on_change ─▶ 安排合成
executor [Background + BusyGuard]：合成任务 ─▶ Projects::snapshot、每个 SessionSource::sessions()、Review::overview、
          Usage::snapshot / limits（都是取锁复制 Arc）─▶ snapshot::build ─▶ Arc<HomeSnapshot> ─▶ 写进 Home ─▶
          发给每个视图的发送端 ─▶ 需要 complete 的会话键 ─▶ SessionSource::complete
主线程：视图的前台任务收到快照 ─▶ 换 Arc、cx.notify() ─▶ 下一帧画出 ─▶ 第一次 files_settled 为 true 的帧：report_milestone(ProjectHome)
主线程：按键 / 点击 ─▶ on_action ─▶ Contributions::dispatch(WorkspaceCommand)
executor：ProjectEvent::Closed { key } ─▶ 监听：drop OverviewInterest，UsageSlot::set_project(None)（放下 UsageInterest），
          清空快照与标记，发一份空快照让视图清空
```

- 监听（executor 线程）只改 `Home` 的标记并安排合成任务，不读服务的快照、不做 IO（[kernel 设计「事件分发」](kernel.md#事件分发)）。合成任务经 `PluginHandle::own_with` 提交，`Background` 优先级，持有 `BusyGuard` 到快照发布为止，所以 bench 的静止采样看得到它。
- 合并：同一时刻最多一个合成任务在跑；跑的期间来的事件只把 `dirty` 置位，任务结束时发现 `dirty` 就再跑一轮。没有周期性任务、没有定时器：相对时间（「3 min ago」）只在下一次快照时刷新（「已知限制」）。
- `OverviewInterest` 的持有：从这个项目成为基底（`Opened`）到它关闭（`Closed`），不随屏幕切换放下。[review 设计「接口」](review.md#接口)写的是「显示主页时持有」；本 crate 持有得更久，原因：`watch_overview` 没有 `Review::close` 那样的 30 秒保留，进入审阅时放下、返回时再取就要重做一次计数比较（10 万文件约 1 s，在后台）和一轮概览；计数改动集本来就在 [ADR 0035](../adr/0035.md) 的采样状态里（≤ 4 MB 的余量），一直持有不增加采样状态的内存。换项目时先放下旧项目的关注再取新的。
- `UsageInterest`（`Usage::watch(UsageQuery::Project { project: ProjectRef::Open(key), window: Window::D7 })`）同样随项目持有，放在 `UsageSlot` 里，由槽位的 `reconcile` 取和放（「本 crate 的公开项」）：主插件的 `Opened` 在 usage 插件 `started` 之前到达时，`started` 之后的对账任务（token 已 `Live`）补取；反过来 `Opened` 补取；`usage` 插件停用时随 `clear_usage` 放下。
- 合成任务经 `UsageSlot::usage()` 取服务（里面 `token.enter()`），取不到就按没有 usage 合成。
- 主线程上只做：读 `Home` 里的快照（取锁复制 `Arc`）、`Contributions::dispatch`、`sidebar_entries()`（取锁复制 `Arc`）、`report_milestone`。不调用任何阻塞的服务方法（`SessionSource::read` / `turns` / `load`、`Projects::resolve` 都不碰）。
- 首帧之前什么都不做：两个插件都是 `AfterFirstFrame`，`activate` 只登记，第一次合成在 `Opened` 之后。

## 性能

项目主页就是 [ADR 0017](../adr/0017.md) 和 [ADR 0035](../adr/0035.md) 定义的采样状态（「首页加载完成、没有打开文件、Diff 或会话」），所以本 crate 的常驻就是采样时屏幕上的东西。

| 预算 | 本 crate 的份额 |
|---|---|
| 冷启动 20 ms（`app/cold_start`） | 首帧之前没有代码运行；`plugin/project-home/activate`、`plugin/project-home-usage/activate` 各 < 1 ms，只登记 |
| 空闲内存（空窗口，`app/idle_empty` < 60 MB） | 没有项目时 `Home` 是空的：两个插件的状态、注册的贡献、空的发送端列表合计 < 16 KiB（`plugin/project-home/resident`） |
| 空闲内存（打开 monorepo < 100 MB、10 万文件 < 120 MB） | 从 ADR 0035 的余量分配（review 5 MB、usage 0.5 MB 之后剩 6.5 MB）：`HomeSnapshot` 加两个视图的状态 **< 1 MB**（cloudprint 形状和 10 万文件形状都是），余量剩 5.5 MB 给 `worktrees` 等。估算：侧栏 1000 行 × 约 200 B（键与标题是 `SessionList` 里 `Arc` 的克隆，不复制字节；`NodeId` 约 80 B）≈ 200 KB，50 张卡片 ≈ 20 KB，子项目 2000 个 × 约 120 B ≈ 240 KB。`project_home/resident_bytes`（`Memory`，1000 个会话、50 个概览条目、2000 个子项目）判定 |
| 打开项目到主页可用 < 100 ms（`app/open_project`） | 终点 `Milestone::ProjectHome`：`Opened` 之后第一帧画出 `files_settled` 为 true 的快照（文件列表已 `Ready` 或 `Unavailable`，侧栏与主区的行已按会话列表画出）。不等概览：它的第一轮含计数比较，10 万文件约 1 s，在此之前 Outside 行与进度环按 `loading_phase` 显示骨架。本 crate 自己的部分（一次合成 + 一帧）< 5 ms（下表） |
| 帧时间 120 fps（每帧工作 ≤ 8.3 ms） | 侧栏的滚动区（`Project` 条目、Sessions、Packages、`Views`）整个是一个 `uniform_list`（`SidebarModel::rows`），每帧只建可见的行（约 30 行），所以 1000 个会话、2000 个子项目都不增加每帧的工作；底部 Usage 行固定在滚动区之外。主区最多 50 张卡片 + 两个分组，在 760pt 阅读栏里用 `GroupedList`，不虚拟化；文字只用 `TextRole`，不排版多行正文；动画只有进度环的 `Transition`（透明度与裁剪） |

新增基准（`bench`，macOS 判定）：

| ID | 统计量与预算 |
|---|---|
| `project_home/build_1000` | 中位数 < 2 ms（`snapshot::build`：1000 个会话、50 个概览条目、17 个 worktree、2000 个子项目） |
| `project_home/resident_bytes` | 存活字节 < 1 MB（同一夹具的 `HomeSnapshot` 与两个视图的状态；`Memory`） |
| `app/home_frames` | 每帧工作时间最大值 ≤ 8.3 ms、相邻帧间隔 ≤ 1.5 个刷新周期（与 `app/scroll_frames` 的两个条件相同）：夹具项目有 1000 个会话（50 个在概览里、50 张卡片）、17 个 worktree、2000 个子项目；打开后经 `WorkspaceHost::record_frames("home", AnimationsSettled)` 记录一次概览到达（50 个进度环同时过渡），再记录一次侧栏从顶部滚到 Packages 末尾（场景 `home-scroll`，`RecordStop::Manual`），两段都要通过 |

本 crate 的实现把 bench 预算表里「加入」一列写着 `project_home` 的 `app/open_project`、`app/idle_monorepo`、`app/idle_100k` 改为生效（[bench 设计「预算表」](bench.md#预算表)）。

## 界面

- 屏幕与层：[交互规范「屏幕」](interaction.md#屏幕)的 02 Overview；项目侧栏与项目主页都在常驻层（[「三层结构」](interaction.md#三层结构)）。`ScreenLayout { sidebar: Shown, back_button: false, reading_column: true, inspector: false, floating_bar: false }`。界面以设计画布的 Overview 画板为准（[#283](https://github.com/SoloJiang/lectern-app/issues/283)，取舍见[交互规范「视觉」](interaction.md#视觉)末尾）；文字用的层级见同处的文字样式表，本文只写层级名。
- 工具栏（`ui::contrib::Screen::toolbar`，容器由 `workspace` 画，在侧栏右边横跨内容区）：标题是 `HomeSnapshot::name`（原样，Headline 层级）；副标题（Subheadline 层级，次要色）是 `project-home-subtitle`，「{分支} · {N} worktrees」，分支取 `HomeSnapshot::branch`，N 取 `worktree_count`；文件夹项目没有分支和 worktree，不显示副标题。原来的「N to review」去掉，大标题已经说了。控件：
  - 左边的侧栏开关是 `workspace` 的图标按钮（[workspace 设计「界面」](workspace.md#界面)）；它的命令还没有设计，由 `workspace` 补上之前不画。
  - 右边的搜索框（约 220pt 宽，占位文字「Search」，右端 ⌘K 提示）：点击或聚焦时 `dispatch(OpenOverlay("finder"))`，在覆盖层里输入，不在工具栏里输入。覆盖层由 [#102](https://github.com/SoloJiang/lectern-app/issues/102) 登记，`Overlay("finder")` 没有登记时不画搜索框。
  - 右边的 New session 图标按钮（`Icon::Plus`）：新建会话的命令还没有归属（[conversation 设计「范围」](conversation.md)把它留给另一个 issue），命令存在之前不画。
- 侧栏面板（248pt，容器由 `workspace` 画，顶部一行留给窗口控件，列表从它下面开始）：第 1 到第 4 节在一个 `uniform_list` 里（`Project` 区的条目 + `SidebarModel::rows` + `Views` 区的条目；条目来自 `sidebar_entries()`，由视图拼入；行高、内缩和选中的样式由 `ui` 的侧栏行给出，[交互规范「窗口结构」](interaction.md#窗口结构)写了数值，本 crate 不写；分区小标题与条目同高，用 Subheadline 层级、半粗、次要色，上面与前一区隔开），整个滚动区一起滚动，每帧只建可见的行；第 5 节固定在面板底部，不随滚动。条目和 Packages 行的图标是强调色的 `ui::Icon`。从上到下：
  1. Overview：`overview` 条目（本 crate 登记的 `SidebarEntry`，`on_select: PopToBase`，格子图标，没有小标题，当前屏幕是主页时选中）。`workspace` 设计的调用方一节把 Sessions、Packages 也写成 `SidebarEntry`；本文只登记 `overview`：Sessions 与 Packages 是带行的分组，`SidebarEntry` 只有一个标签，放不下行。`Project` 区里其他 crate 登记的条目画成普通行，排在 `overview` 之后。
  2. Sessions：小标题 `project-home-sessions`；每行：8pt agent 圆点（`AgentDot`，运行中时 `blink` 一次：新一轮到达 = `running` 从 false 变 true，或 `last_activity` 变化；`Agent::Other(name)` 的会话没有圆点，`name` 原样显示在标题前）、标题一行（超出截断）、`running` 的会话行尾一个绿色小圆点（`project-home-running` 是它的无障碍名称）。没有次要文字，也没有 worktree 小标题：所有 worktree 的会话混排，按 `last_activity` 从新到旧；会话在哪个 worktree 不在侧栏里画（设计画布没有）。默认只列最近的 3 个，其余折成一行 `project-home-sessions-more`（「6 more」，次要色，与标题对齐）；点击这一行就地展开 14 天内的全部会话，这一行变成 `project-home-sessions-less`，再点收起。展开与否是视图的状态，不进快照。14 天内没有会话时显示一行 `project-home-sessions-empty`。
  3. Packages：小标题 `project-home-packages`；每行：文件夹图标、子项目名、行尾次要色的数字（今天的改动数，只画数字；0 时不显示；`package_counts` 为 false 时整列不显示；数字的含义由 `project-home-changed-today` 作无障碍名称和提示，「未决问题」2 不变）；嵌套的子项目按 `depth` 缩进。子项目 `Pending` 时显示一行 `Skeleton`（`PackagesSkeleton`），`Unavailable` 或没有子项目时不显示这一节。行不可点击（「本 issue 之外」）。子项目可以有几千个（10 万文件的形状是 2000 个），它们和会话行一样在同一个 `uniform_list` 里，不单独封顶。
  4. `Views` 区：小标题 `project-home-views`（「Project」）；`sidebar_entries()` 里 `section == Views` 的条目，按 `workspace` 给的顺序；每行图标（Worktrees、Files、Merge Requests 各有自己的图标）、标签（`Text::key(item.label)`）、行尾次要色的数量（画布上 Worktrees 是 11、Merge Requests 是 2），点击 `dispatch(item.on_select)`。数量要 `ui::contrib::SidebarItem` 带一个字段（[workspace 设计「`ui::contrib` 的载荷类型」](workspace.md#uicontrib-的载荷类型)里现在只有 `label`、`icon`、`on_select`），在加上之前不画数量，见「对其他 crate 的要求」C。没有条目时整节（含小标题）不显示。
  5. `Bottom` 区：`usage` 条目（`usage_view` 登记的 `SidebarEntry`，`on_select: Push(USAGE)`）由本 crate 画成一张卡片（`fill` 底、圆角、四周留白，整张可点）：第一行标签 `project-home-usage`（Callout 层级，半粗）加右对齐的次要文字 `project-home-usage-7d`；第二行撑满卡片宽度的细额度条（`Progress::Bar`，`used_percent / 100`，警示橙色填充）；第三行 Subheadline 层级的次要文字 `project-home-limit`（如「Codex 5-hour limit · 64%」）。`UsageModel` 为 None（`usage` 插件不在）时只画标签行；`tokens_7d` 为 None 时数字处是 `Skeleton`；`tightest` 为 None 时没有第二、三行。画布只画了一个 64% 的橙色条，是否随百分比换色没有画；本文先用一种颜色。`Bottom` 区的其他条目画成普通行。
- 主区（居中 760pt 阅读栏，画布的 Overview 画板画了 820pt，以 760pt 为准；背景是页面底色，分组是它上面带描边的白色卡片，见[交互规范「视觉」](interaction.md#视觉)）从上到下：
  - 大标题（Large Title 层级，粗体，`project-home-title`，数字是 `needs_review.len()`）；下面一句说明（Title 3 层级，次要色）：`project-home-lede-running`（`MainModel::running` 个 agent 正在运行）和 `project-home-lede-outside`（`OutsideRow::Files` 的个数，`Capped` 和 `Pending` 时没有这一句），某一句的数为 0 就省略，两句都没有时不画这一行，两句之间用空格连接。
  - 三个分组列表，小标题分别是 `project-home-needs-review`、`project-home-outside`、`project-home-done`（Headline 层级，次要色，左缩进与卡片圆角对齐）。
  - Needs Review 的卡片一行：8pt agent 圆点、标题（Headline 层级）、两行次要文字（Callout 层级）、行尾的进度。第一行用 ` · ` 连接：agent 名（`project-home-agent-*`；`Agent::Other(name)` 原样）、分支（`Card::branch`，没有则省略）、`Strings::relative_time`、状态词（`Card::status`：`project-home-status-running`、`-waiting` 用系统绿色，`-ended` 用次要色；`None` 时省略）。第二行是信号，用 ` · ` 连接。行尾：`Progress::Ring`（`seen / total`）、次要色的 `project-home-progress-short`（如「2/6」）和 `Icon::ChevronRight`；`progress` 为 None 时三个都不画。圆点画 8pt，画布在这里画了 10pt，见交互规范「视觉」。
  - Outside Any Session 是一张卡片，不是列表：左边第一行 `project-home-outside-files`（含当前分支），`Capped` 时用 `project-home-outside-capped`，`Pending` 时数字处是 `Skeleton`；第二行是前 3 个改动路径（`Code` 层级的等宽字，次要色，用 ` · ` 连接，不止 3 个时末尾加 `…`），没有路径（`names` 为空）时不画这一行；右边一个次要样式的 `project-home-outside-review` 按钮。整张卡片和按钮的点击、↵ 都是 `Push(REVIEW, Outside)`。
  - Recently Done 是普通行的列表，没有圆点、进度环和信号：左边标题（Body 层级），右边次要文字（Callout 层级）是结果（「Committed」「MR #86 merged」）。结果要 git 提交或 MR 的数据（[#25](https://github.com/SoloJiang/lectern-app/issues/25)），现在没有来源，右边显示 `Strings::relative_time`；有来源之后的键是 `project-home-done-committed`、`project-home-done-merged`。
  - Recently Done 为空时不显示这一组；Needs Review 为空而概览已就绪时显示 `project-home-all-reviewed`。
- 信号（第二行灰字；只显示成立的项，都不成立时第二行省略）：

  | 信号 | 键 | 来源 | 现在 / 以后 |
  |---|---|---|---|
  | 已漂移 | `project-home-signal-drifted` | `SessionOverview::drifted == Some(true)` | 现在 |
  | critical | `project-home-signal-critical` | `SessionOverview::critical > 0` | 现在 |
  | 审过之后又改过 | `project-home-signal-changed` | `Progress::changed_since_seen > 0` | 现在 |
  | 公共包改动、API 变化 | `project-home-signal-public-package`、`project-home-signal-api` | `code_intel` 的 `BriefRules` 提供者 + `SessionOverview` 带规则 ID | 以后（「本 issue 之外」）；键先登记，实现前不显示 |
  | 测试结果 | `project-home-signal-tests` | MR 流水线（#25） | 以后 |

- `ui` 的控件与动效原语：`GroupedList`、`Row`、`AgentDot`、`Progress`（Ring / Bar）、`Button`、`Icon`、`Skeleton`、`loading_phase`，侧栏行、工具栏搜索框和 Outside 卡片按 `ui` 设计补上的形态（见「对其他 crate 的要求」C）；进度环用 `motion::PROGRESS` 的 `Transition`，悬停由 `Row` 自带（`HOVER_IN` / `HOVER_OUT`），圆点轻闪用 `ARRIVE`；减弱动态效果由 `ui` 处理。本 crate 不写颜色值、字体和时长（「验收」的 `rg` 检查）。
- Fluent 键（`crates/i18n/locales/{en,zh-CN}/project-home.ftl`，`keys::project_home::*`；会话标题、分支名、子项目名、agent 名经 `Text::verbatim` 或 `Args::str` 原样显示）：

  | 键 | en | zh-CN |
  |---|---|---|
  | `project-home-overview` | Overview | 概览 |
  | `project-home-sessions` | Sessions | 会话 |
  | `project-home-sessions-empty` | No sessions in the last 14 days | 最近 14 天没有会话 |
  | `project-home-sessions-more` | { $count } more | 还有 { $count } 个 |
  | `project-home-sessions-less` | Show less | 收起 |
  | `project-home-views` | Project | 项目 |
  | `project-home-untitled-session` | Untitled session | 未命名会话 |
  | `project-home-running` | Running | 运行中 |
  | `project-home-packages` | Packages | 包 |
  | `project-home-changed-today` | { $count } today | 今天 { $count } |
  | `project-home-usage` | Usage | 用量 |
  | `project-home-usage-7d` | { $tokens } tokens · 7 days | { $tokens } token · 7 天 |
  | `project-home-limit` | { $agent } { $window } limit · { $percent }% | { $agent } { $window }额度 · { $percent }% |
  | `project-home-limit-no-window` | { $agent } limit · { $percent }% | { $agent } 额度 · { $percent }% |
  | `project-home-window-hours` | { $count }-hour | { $count } 小时 |
  | `project-home-window-days` | { $count }-day | { $count } 天 |
  | `project-home-title` | { $count -> [0] Nothing needs review [one] { $count } session needs review *[other] { $count } sessions need review } | { $count -> [0] 没有待审的会话 *[other] { $count } 个会话待审 } |
  | `project-home-subtitle` | { $branch } · { $count -> [one] { $count } worktree *[other] { $count } worktrees } | { $branch } · { $count } 个 worktree |
  | `project-home-lede-running` | { $count -> [one] { $count } agent is running. *[other] { $count } agents are running. } | { $count } 个 agent 正在运行。 |
  | `project-home-lede-outside` | { $count -> [one] { $count } file changed outside any session. *[other] { $count } files changed outside any session. } | { $count } 个文件在会话之外被改动。 |
  | `project-home-needs-review` | Needs Review | 待审阅 |
  | `project-home-outside` | Outside Any Session | 会话外的改动 |
  | `project-home-outside-files` | { $count -> [one] { $count } file changed on { $branch } *[other] { $count } files changed on { $branch } } | { $branch } 上有 { $count } 个文件改动 |
  | `project-home-outside-review` | Review | 审阅 |
  | `project-home-outside-capped` | { $count } changed paths (not attributed) | { $count } 个改动路径（未归属） |
  | `project-home-done` | Recently Done | 最近完成 |
  | `project-home-all-reviewed` | Every session is reviewed. | 所有会话都已审阅。 |
  | `project-home-empty` | No agent sessions in this project yet. | 这个项目还没有 agent 会话。 |
  | `project-home-progress` | { $seen }/{ $total } reviewed | 已看 { $seen }/{ $total } |
  | `project-home-progress-short` | { $seen }/{ $total } | { $seen }/{ $total } |
  | `project-home-status-running` | running | 运行中 |
  | `project-home-status-waiting` | waiting for you | 等你处理 |
  | `project-home-status-ended` | ended | 已结束 |
  | `project-home-done-committed` | Committed | 已提交 |
  | `project-home-done-merged` | MR #{ $number } merged | MR #{ $number } 已合并 |
  | `project-home-signal-drifted` | Drifted | 已漂移 |
  | `project-home-signal-critical` | { $count -> [one] { $count } critical *[other] { $count } critical } | { $count } 处需注意 |
  | `project-home-signal-changed` | { $count -> [one] { $count } file changed since seen *[other] { $count } files changed since seen } | 已看后又改了 { $count } 个文件 |
  | `project-home-signal-public-package` | Public package changed | 改到公共包 |
  | `project-home-signal-api` | API changed | API 有变化 |
  | `project-home-signal-tests` | Tests { $result } | 测试 { $result } |
  | `project-home-agent-claude-code`、`project-home-agent-codex` | Claude Code / Codex | Claude Code / Codex |

  相对时间用 `Strings::relative_time`（common.ftl），复数按 Fluent 的选择器（[交互规范「语言」](interaction.md#语言)）。

- UI 探针的语义节点（`ui` 设计的 `semantic` 模块：`NodeId::new("project-home", …)`，名字段只用 `[a-z0-9-]`；窗口根节点由探针合成）：

  | ID | `Role` | 状态 / `attrs` |
  |---|---|---|
  | `project-home.sidebar` | `Group` | `sessions`（会话总数）、`shown`（折叠时列出的个数） |
  | `project-home.sidebar.overview` | `ListItem` | `selected` |
  | `project-home.sidebar.list` | `List` | `rows`（滚动区的总行数）、`visible`（上一帧建出的行数：行是在布局之后才建的，这一帧的声明里还不知道）；下面的节点都是它这一帧可见的行，层级来自元素嵌套，ID 只按区命名 |
  | `project-home.sidebar.sessions` | `Text` | Sessions 标题行 |
  | `project-home.sidebar.sessions.more` | `Button` | `Label::key` 是 `project-home-sessions-more` 或 `-less`；`hidden`（折叠起来的个数）、`expanded` |
  | `project-home.sidebar.sessions.item[<会话 ID>]` | `ListItem` | `selected`；`agent`、`running`、`pending`（`tail_pending`） |
  | `project-home.sidebar.packages` | `Text` | Packages 标题行；`counts`（是否有今天的改动数） |
  | `project-home.sidebar.sessions.empty` | `Text` | 14 天内没有会话时的一行字（`project-home-sessions-empty`） |
  | `project-home.sidebar.packages.item[<dir>]` | `ListItem` | `changed`、`depth` |
  | `project-home.sidebar.project.item[<贡献 ID>]`、`project-home.sidebar.bottom.item[<贡献 ID>]` | `ListItem` | 其他 crate 登记在 `Project` / `Bottom` 区的条目，`Label::key` 是标签键 |
  | `project-home.sidebar.views.item[<贡献 ID>]` | `ListItem` | `Label::key` 是贡献的标签键 |
  | `project-home.sidebar.usage` | `ListItem` | `tokens`、`percent`；没有 usage 时不出现 |
  | `project-home.sidebar.usage.limit` | `Progress` | `fraction` |
  | `project-home.search` | `SearchField` | 工具栏的 ⌘K 搜索框；没有 `Overlay("finder")` 时不出现 |
  | `project-home.new-session` | `Button` | 工具栏的 New session；命令存在之前不出现 |
  | `project-home.title` | `Text` | `Label::key` = `project-home-title`，`count`，`ready`（概览是否就绪） |
  | `project-home.lede` | `Text` | 大标题下的说明；`running`、`outside`；两句都没有时不出现 |
  | `project-home.needs-review` | `List` | `GroupedList` 的容器，`Label::key` = `project-home-needs-review` |
  | `project-home.needs-review.item[<会话 ID>]` | `ListItem` | `selected`（`Row` 自带） |
  | `project-home.needs-review.item[<会话 ID>].facts` | `Group` | 卡片内容的容器，`Label` 是标题；`agent`、`seen`、`total`（没有进度时没有这两项）、`drifted`、`critical`、`changed`。`ui::Row` 不带 `attrs`，`GroupedList::row` 又只收 `Row`，没法在行本身上声明这些事实，所以放在行内容的容器上 |
  | `project-home.needs-review.item[<会话 ID>].progress` | `Progress` | `fraction` |
  | `project-home.outside.group` | `List` | Outside 的 `GroupedList` 容器 |
  | `project-home.outside` | `ListItem` | `selected` |
  | `project-home.outside.facts` | `Group` | `Label::key` = `project-home-outside-files` / `-capped`；`files` / `capped` / `pending` |
  | `project-home.outside.review` | `Button` | `Label::key = project-home-outside-review` |
  | `project-home.done` | `List` | `GroupedList` 的容器 |
  | `project-home.done.item[<会话 ID>]`、`….facts`、`….progress` | 同 Needs Review | |
  | `project-home.loading` | `Group` | 概览还没有结果、且已过 `LOADING_SKELETON_AT` 时 Needs Review 位置上的占位；`phase`（`skeleton` / `progress`） |
  | `project-home.empty` | `Text` | `Label::key` 是空状态的键（`project-home-empty` 或 `project-home-all-reviewed`） |

  `ui` 的控件自带角色和标签，本 crate 只给 `NodeId`；`GroupedList` 的容器、侧栏的 `uniform_list` 和它里面的标题行用 `semantic(...)` 包装。

- Action 与快捷键（经 `KeyBindings` 贡献登记，谓词 `Content > project-home`；都能被探针按名字触发）：

  | Action | 键 | 处理 |
  |---|---|---|
  | `project_home::SelectNext`、`SelectPrevious` | ↓、↑ | 在 Needs Review → Outside → Recently Done 的顺序里移动选中项，跨组连续；到头不回绕；滚动到可见 |
  | `project_home::Open` | ↵ | 选中项是会话时 `Push(REVIEW, Session)`，是 Outside 时 `Push(REVIEW, Outside)`；没有选中项时选第一项 |
  | `project_home::SelectFirst`、`SelectLast` | ⌥↑、⌥↓ | — |
  | `project_home::OpenSession { key }`、`OpenOutside` | — | 鼠标点击行与探针用，同 `Open` |

  侧栏面板的 KeyContext 是 `Sidebar > project-home`：侧栏里会话行只响应点击（侧栏没有键盘焦点，与 Finder 的侧栏相同）。Esc、⌘[、⌥⌘I、⌘\ 归 `workspace`（项目主页没有检查器，后两者被忽略）。

## 状态与异常

| 情况 | 表现（[交互规范「状态与异常」](interaction.md#状态与异常)） |
|---|---|
| 项目里还没有会话（14 天内 `in_project` 为空且概览的 `sessions` 为空） | 主区大标题换成 `project-home-empty`，不显示三个分组；侧栏 Sessions 区显示 `project-home-sessions-empty`。「最近提交」和 New session 在本 issue 之外 |
| 刚打开项目：`files`、`packages` 为 `Pending` | 侧栏按会话列表立即画出会话行（里程碑不等它们）；Packages 显示骨架；概览未就绪时 Outside 行和进度环显示骨架；按 `loading_phase`，100 ms 内不显示骨架 |
| 概览第一轮超过 1 s（10 万文件的计数比较） | 骨架换成 `LoadingPhase::Progress` 的不确定进度；不提供取消（概览在后台，不挡任何操作） |
| 文件列表 `Unavailable`（index 读不出） | `files_settled` 为 true，里程碑照常；Packages 不显示；概览由 `review` 按它自己的规则给出（`Outside` 可能一直 `Pending`） |
| 文件夹项目 | 工具栏没有副标题（没有分支和 worktree）；卡片没有分支；Outside 行不显示（没有基准）；卡片没有进度环与 Drifted（`review` 的 `Degraded::FolderProject`） |
| `OutsideCount::Capped(n)` | Outside 行显示 `project-home-outside-capped`，Packages 的今天改动数不显示 |
| 会话格式无法识别（`Format::Unrecognized`） | 侧栏照常列出（标题可能为 None → 「Untitled session」）；进入审阅由 `review` 降级 |
| `tail_pending` 的会话 | 相对时间按文件修改时间显示；合成任务对显示出来的行调 `complete`，补齐后更新 |
| `Overview.sessions` 里的会话在其他 worktree 被切换为当前 | 概览随 `review` 的下一轮更新；切换期间卡片按旧概览显示（`sources.overview` 不变） |
| `usage` 插件没有装、激活失败或停用 | 侧栏底部只画 Usage 标签（没有 token 和额度条），其余不受影响 |
| `review` 插件失败 | `project-home` 依赖 `dyn Review`，随之停用：`workspace` 显示 `workspace-screen-missing`（workspace 设计「状态与异常」）。审阅闭环没有 review 本来就不成立，不另做降级 |
| `Push(REVIEW)` 时审阅屏幕没有注册 | `workspace` 记 warn、不变；本 crate 不提示 |
| 快照里的会话在用户按 ↵ 之前被删除 | `Review::open` 返回的快照是 `Failed(Missing)`，由 `review_view` 显示；本 crate 不检查 |
| 项目关闭 | 视图收到空快照，清空；`workspace` 随 `Closed` 切回 Welcome，视图随屏幕弹出而 drop |
| 加载态 | 没有自己的加载态：命令立即发出（「界面先更新」），结果随事件到达 |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `project_home`。监听和合成任务里的记录带 kernel / executor 设的 `plugin`、`act`、`task`；视图在主线程上的记录带 `workspace` 恢复的 `plugin`、`act`（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 11 项）。

- 默认记录：
  - info `home snapshot built`，每个项目第一次合成时一条（之后降为 debug）：`sessions`（侧栏行数）、`worktrees`（组数）、`cards`（Needs Review 数）、`done`、`packages`、`overview`（bool，概览是否就绪）、`took_us`。
  - info `home milestone reported`：`request`（`OpenRequest` 的序号）、`since_opened_us`（`Opened` 到报告的微秒数）。
  - warn `overview session not in list`：`sid`（`Value::uuid`；会话 ID 不是 UUID 时传它的 128 位哈希；字段名不能叫 `session`，那是作用域的键）；同一会话只记一次。
  - warn `usage call refused`：`reason`（`token-dead`）；`usage` 插件停用期间最多记一次。
  - warn `home build dropped`：`reason`（`project-closed` / `generation-stale`）、计数。
  - error 没有：本 crate 没有会失败的操作（服务方法都不返回错误，命令由 `workspace` 处理）。
- 详细级别：debug `home snapshot built`（每次合成，字段同上，另带 `sources` 的代数）；debug `home command`：`command`（`WorkspaceCommand` 的判别名）、`target`（`session` / `outside` / `entry`）；debug `home complete requested`：`sessions`（计数）；debug `home view attached` / `detached`：`view`（`screen` / `sidebar`）；trace `home event`：事件类型名与代数。
- 关联 ID：不另设作用域。`warn` 里的 `session` 是记录字段，不是 `Ctx::session`（本 crate 不处理单个会话的流程）。
- 不记录：会话标题、项目名、worktree 根路径、分支名、子项目名与目录、`SessionKey::id` 之外的会话内容、token 数之外的用量明细、贡献条目的标签文字、`NodeId` 的业务键。用计数、代数、`Value::uuid`、判别名代替。
- 金丝雀测试：`canary_stays_out_of_home_logs`（`src/tests/logs.rs`）：`lectern_logging::testing::capture()` 在 trace 级别下，用 `FakeSessions`、`FakeProjects` 构造一个会话标题、项目名、分支名、子项目名、worktree 根路径都含金丝雀的项目，跑一次合成、画一帧、按 ↵、关闭项目，`assert_absent(金丝雀)`。

## 测试

第 1 步的测试（`snapshot::build`、插件、金丝雀）不依赖 GPUI，Linux 和 macOS 上都跑；第 2 步加 `gpui` 与 `lectern-ui` 后只在 macOS 上编译和测试：`just test -p lectern-project-home`。会话、项目、概览、用量、贡献都用 `services` trait 的假实现，放在本 crate 的 `testing` 模块（`FakeSessions`、`FakeProjects`、`FakeReview`、`FakeUsage`、`FakeContributions`）：直接构造 `SessionList`、`ProjectSnapshot`、`Overview`、`UsageSnapshot` / `Limits` 交出去，记下收到的 `complete`、`watch_overview`、`watch`、`dispatch`，不解析脚本；`read` / `turns` / `load` / `resolve` 一律 `unreachable!`（本 crate 不得调用它们）。不用真实的 git、会话扫描和文件 IO，也不依赖 `review` 等插件 crate 的 `test-support`。

- `snapshot::build`（`#[test]`，不开窗口）：`build_groups_sessions_by_worktree`（主 worktree 在前、`Resolved` 与 `Located` 都归位、prunable 的进 `Other`、子 agent 不成行、14 天之外的不出现）；`build_splits_needs_review_and_done`（`progress` None / 部分 / 全部 / `changed_since_seen` 四种）；`build_drops_overview_sessions_missing_from_list`；`build_marks_running_within_ten_minutes`；`build_counts_packages_today`（有 `Overview.packages` 与没有、`Capped`）；`build_folder_project_hides_outside`；`build_without_usage`；`build_flattens_sidebar_rows`（`rows` 的顺序与下标，2000 个子项目时长度正确；不含贡献条目）；每个用例的 `HomeSnapshot` 渲染成文本做 insta 快照（`snapshot_*`）；`project_home/build_1000` 的夹具也在这里构造。
- 插件（`#[test]`，`TestKernel`）：`activate_registers_contributions_only`（登记了 `Screen`、`SidebarPanel`、`SidebarEntry`、`KeyBindings`，没有任务）；`opened_takes_the_overview_interest_and_closed_drops_it`；`events_coalesce_into_one_build`（跑着的时候来 5 个事件，结束后只再跑一轮）；`complete_is_requested_once_per_pending_session`；`usage_slot_empty_builds_without_usage`、`usage_plugin_deactivation_rebuilds`、`usage_activating_after_opened_takes_the_watch`（`Opened` 先到、usage 插件后 `started`：`started` 返回时 `FakeUsage` 还没有收到 `watch`，token 变为 `Live`、对账任务跑过之后收到一次，快照重建一次并带 `UsageModel`）、`opened_after_usage_takes_the_watch`（相反的顺序，结果相同）；`started_handles_an_already_open_project`；`changed_for_an_unknown_project_counts_as_opened`。
- `#[gpui::test]`（`TestAppContext`，`init_fixed`）：`sidebar_lists_recent_sessions_and_folds_the_rest`；`sidebar_builds_only_visible_rows`（2000 个子项目、1000 个会话时一帧里建出的行数不超过可见行数加一屏）；`home_shows_cards_outside_and_done`；`arrow_keys_move_across_groups`（↓ 从最后一张卡片到 Outside 再到 Done，↑ 反向，到头不回绕）；`enter_pushes_the_review_route`（`FakeContributions` 收到 `Push(REVIEW, Session(key))`）；`outside_opens_the_outside_review`；`clicking_a_sidebar_session_pushes_review`；`views_entries_dispatch_their_command`；`late_entries_appear_without_a_new_snapshot`（`FakeContributions` 在快照建好之后加一个 `Project` 区和一个 `Views` 区的条目并发 `ContributionsChanged`，下一帧两行都出现）；`usage_row_shows_tokens_and_the_tightest_limit`；`new_sidebar_view_reads_the_current_snapshot`（面板视图 drop 再建，不等事件就有内容）；`progress_ring_transitions_between_snapshots`（`Transition` 从旧值出发）；`milestone_is_reported_on_the_first_settled_frame`（`files` 为 `Pending` 的快照不报告，`Ready` 的那一帧报告一次，之后不再报告）；`skeleton_appears_after_100ms_only`；`closed_project_clears_the_views`。
- 语义树：`home_tree_with_cards`、`home_tree_empty`、`sidebar_tree_with_views_and_usage` 的 insta 快照（UI 探针完成后接入；之前用 `ui::semantic::Recorder` 的记录去掉 `bounds` 和 `declared_at`）。
- 日志：`canary_stays_out_of_home_logs`。
- fixtures：没有外部格式；测试数据是源码里构造的快照。
- 只能在 macOS 上跑的部分：全部。

## 对其他 crate 的要求

每一项是一个 PR，写 `Refs #30`，在「验收」里列为前置条件。

- **A. `review`：`Overview.packages`。** 现在的 `Overview` 没有按子项目的改动数，`ProjectSnapshot` 也没有。在 [review 设计「接口」](review.md#接口)的 `Overview` 里加一项：

  ```rust
  pub struct Overview {
      // …
      /// 当前 worktree 里今天改动的文件按子项目的计数：计数改动集（ADR 0035）里工作区一侧 stat 的 mtime
      /// 落在本地自然日（含今天，与 usage 设计「取整与时间窗口」的日界相同）内的路径，按 `ProjectSnapshot::active.packages`
      /// 的 `dir` 最长前缀归到一个子项目，不在任何子项目里的不计。`OutsideCount::Capped` 时为 None（没有改动集）；
      /// `packages` 还是 `Pending` 时为 None。
      pub packages: Option<Arc<[(RelPath, u32)]>>,
  }
  ```

  「今天」因此是：worktree 相对 HEAD 的改动文件里，最后修改时间在今天的那些；已提交的改动不算，昨天改、今天还没提交的也不算。计数随 `review` 的下一轮概览刷新（fs_watch 批次、`SessionsChanged` 等触发，合并 2 秒），不在午夜自动归零：日界之后第一次变化才更新。这一项合并之前，侧栏的 Packages 只显示名字（`package_counts` 为 false）。已实现：[#239](https://github.com/SoloJiang/lectern-app/pull/239)；只列计数不为 0 的子项目，`Unavailable` 时也是 None（review 设计「与设计的差异」）。
- **B. `i18n`：`locales/{en,zh-CN}/project-home.ftl`**（[#237](https://github.com/SoloJiang/lectern-app/pull/237)），内容是「界面」的键表。
- **C. `services`、`ui`、`workspace`**：本 crate 用到的大部分已是 workspace 设计或 `ui` 设计的内容。为 [#283](https://github.com/SoloJiang/lectern-app/issues/283) 新增三项：`ui` 的 `Icon` 要加格子（Overview）、Worktrees、文件（Files）、Merge Request、加号、侧栏开关；`ui` 的侧栏行、工具栏搜索框、次要样式的卡片内按钮要有对应形态；`ui::contrib::SidebarItem` 要带一个行尾的数量字段（Worktrees 的 11、Merge Requests 的 2），`ui` 设计还没有这个字段；加上之前不画数量。此外有两项原有的，由 workspace 设计的前置条件定义、还没有实现：`services::workspace` 模块（前置条件 A，[#221](https://github.com/SoloJiang/lectern-app/pull/221)）和 `ui::contrib` 的载荷类型与 `into_payload`（前置条件 B，[workspace 设计「`ui::contrib` 的载荷类型」](workspace.md#uicontrib-的载荷类型)；`ui` 设计 [#218](https://github.com/SoloJiang/lectern-app/pull/218) 的 `contrib` 模块也列出了它们，侧栏条目的类型叫 `SidebarItem`，带 `icon: Option<ui::Icon>`）。本 crate 的 `Screen`、`SidebarPanel`、`SidebarEntry`、`KeyBindings` 登记和 `Views` 区的图标都靠它们，所以它们在「验收」里列为前置条件。`GroupedList`、`Row`、`AgentDot`、`Progress`、`Skeleton`、`semantic` 在 `ui` 设计的第 3、4 步；`sidebar_entries()`、`report_milestone`、`dispatch` 在 workspace 设计里。`usage_view` 登记 `SidebarEntry("usage")`（`section: Bottom`、`on_select: Push(USAGE)`）时，本 crate 为它画 token 与额度条；它的设计文档写明这一点。
- **D. `app`**：装配的部分已实现（[#247](https://github.com/SoloJiang/lectern-app/pull/247)，早于第 3 步，见「与设计的差异」）；`Driver` 的 `home` 场景随探针。装配时构造 `UsageSlot`，分别交给 `ProjectHomePlugin::new(ProjectHomeDeps { executor, usage })` 和 `ProjectHomeUsagePlugin::new(executor, slot)`，两个插件都放进 `Profile::Desktop` 的 `PluginSet`；bench 的 `Driver` 加 `home` 场景（`app/home_frames`）。
- **E. `bench`**：预算表加 `project_home/build_1000`、`project_home/resident_bytes`、`app/home_frames`；`app/open_project`、`app/idle_monorepo`、`app/idle_100k` 随本 crate 的最后一步改为生效。
- **F. `agents`、`review`**（为 [#283](https://github.com/SoloJiang/lectern-app/issues/283) 新增，两项都不阻塞本 crate：没有时对应位置不画）：`agents` 给会话一个结束状态（等你处理 / 已结束），卡片第一行的 `Waiting`、`Ended` 才能显示；`review` 的 `OutsideCount` 带前 3 个改动路径，Outside 卡片的文件名才能显示。

## 未决问题

1. **主区只显示当前 worktree 的会话，还是所有 worktree 的。** `Overview.sessions` 按 review 的设计只含当前 worktree（计数比较只对当前 worktree 做，ADR 0035）。默认（本文按它写）：主区只有当前 worktree 的会话；其他 worktree 的会话在侧栏里与当前 worktree 的会话按时间混排（设计画布没有按 worktree 分组），可以点进审阅，但没有进度环和信号。另一个选项：主区按 worktree 分段列出全部会话，要 `review` 对每个 worktree 各做一份概览，内存按 worktree 数增长，需要另写 ADR 调整 ADR 0035 的份额。
2. **Packages 的「今天改动数」的含义。** 默认（「对其他 crate 的要求」A）：相对 HEAD 未提交、且最后修改在今天的文件数，按子项目归类。另一个选项：不看日期，显示相对 HEAD 的全部改动文件数（名字改成「changed」）；它更稳定（不随午夜变化），但与交互设计画布上的「今天」不一致。
3. **侧栏 Sessions 的时间范围。** 默认：最近 14 天（与概览的声明窗口相同），更早的会话以后经 ⌘K 搜索。另一个选项：全部列出（1000 个会话时侧栏很长，虚拟列表能画，但找不到东西）。

## 验收

分三步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-project-home`、`just deny` 通过。前置条件的顺序：B（`project-home.ftl`）和 workspace 设计的前置条件 A（`services::workspace`）在第 1 步之前；workspace 设计的前置条件 B（`ui::contrib` 的载荷类型、侧栏图标）、`ui` 设计的第 3、4 步（控件、探针节点）和 workspace 的第 2 步（根视图）在第 2 步之前；本文的 A（`Overview.packages`）、D、E 随第 3 步（A 可以更早）。

**第 1 步：快照与插件（不开窗口的部分，[#241](https://github.com/SoloJiang/lectern-app/pull/241)）。**

- `snapshot::build` 与「测试」里 `build_*`、`snapshot_*` 的 insta 快照通过。
- 插件的 `#[test]` 通过（`activate_registers_contributions_only` 随第 2 步，见「与设计的差异」）：`opened_takes_the_overview_interest_and_closed_drops_it`、`events_coalesce_into_one_build`、`complete_is_requested_once_per_pending_session`、`usage_slot_empty_builds_without_usage`、`usage_plugin_deactivation_rebuilds`、`started_handles_an_already_open_project`。
- `python3 scripts/check_architecture.py` 通过：`lectern-project-home` 带 `plugin = true`，不依赖其他插件 crate；只有 `app` 依赖它。
- `rg 'std::fs|std::process|git2|gix|std::thread::spawn' crates/project_home/src` 没有结果；`rg '^lectern-(review|project|agents|usage|workspace)(\s|\.|=)' crates/project_home/Cargo.toml` 没有结果（不依赖插件 crate，dev-dependency 也不行；行首锚定，所以不会匹配到本 crate 的 `name = "lectern-project-home"`）。
- `canary_stays_out_of_home_logs` 通过（合成与插件部分）；`rg 'eprintln!|println!|log::' crates/project_home/src` 没有结果。

**第 2 步：视图（`#[gpui::test]`）。**

- 「测试」里的 `#[gpui::test]` 和语义树快照通过；`activate` 登记 `Screen`、`SidebarPanel`、`SidebarEntry`、`KeyBindings`，`activate_registers_contributions_only` 通过（第 1 步推迟到这里）。
- `rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|\.font_family\(|gpui_component::Button' crates/project_home/src` 没有结果（`ui` 设计的持续检查）。
- `rg '"[A-Z][a-z]+ [a-z]' crates/project_home/src --glob '!tests*'` 没有界面字符串字面量（都走 `keys::project_home::*`）。
- `rg 'NodeId::new\("project-home"' crates/project_home/src` 有结果，且 `rg 'NodeId::new\("project_home"' crates/project_home/src` 没有结果（探针 ID 的名字段只用 `[a-z0-9-]`）。
- `canary_stays_out_of_home_logs` 覆盖渲染与按键。

**第 3 步：装配、里程碑与基准（`Closes #30`，前置条件 A、D、E 随这一步）。**

- `just test -p lectern-app`：`every_profile_graph_validates` 里 desktop 的插件数加二。
- 手动在 macOS 上运行发布构建（临时 `HOME`、有会话的仓库）：打开文件夹后侧栏列出最近的会话（其余折成「N more」）、Packages 带今天的改动数，主区显示 Needs Review、Outside Any Session、Recently Done；↑ ↓ ↵ 进入审阅、⌘[ 返回；日志里 `home milestone reported` 的 `since_opened_us` < 100 000。
- `just bench app/`：`app/open_project` 中位数 < 100 ms；`app/idle_monorepo` < 100 MB、`app/idle_100k` < 120 MB；`app/home_frames` 通过。`just bench project_home/`：`project_home/build_1000` < 2 ms、`project_home/resident_bytes` < 1 MB。
- `app/idle_empty` 与改动前相比增加不超过 64 KiB。

## 已知限制

- 相对时间只在下一次快照时刷新：没有事件时「3 min ago」不会自己变成「4 min ago」。采样状态里不设定时器（ADR 0017 的静止）。
- Packages 的今天改动数不在午夜自动归零（「对其他 crate 的要求」A）。
- 侧栏只列最近 14 天的会话（「未决问题」3）。
- 主区没有其他 worktree 的会话（「未决问题」1）。
- `project-home` 依赖 `dyn Review`：`review` 失败时整个项目主页随之不可用。

## 实际验证

### 前置条件 B

`project-home.ftl`（[#237](https://github.com/SoloJiang/lectern-app/pull/237)，同一个 PR 还补了 `welcome.ftl` 与 `review.ftl`），在 macOS（arm64，Rust 1.97.0）上运行：`just check`、`just deny`、`just test -p lectern-i18n`（57 个测试通过）。「界面」键表的 28 个键在 en 与 zh-CN 里都有，变量一致（编译期校验）；`project-home-subtitle` 与 `project-home-signal-critical` 没有写与默认分支相同的 `[one]`。

### 第 1 步

本 crate 不开窗口、不依赖 GPUI 的部分（[#241](https://github.com/SoloJiang/lectern-app/pull/241)），在 macOS（arm64，Rust 1.97.0）上运行：

- `cargo check -p lectern-project-home`、`just check`（含 `cargo clippy --workspace --all-targets -- -D warnings` 和架构检查：32 个 crate，`lectern-project-home` 带 `plugin = true`）、`just deny` 通过。`python3 scripts/check_architecture.py --gpui-free-packages` 现在包含本 crate，Linux CI 也会跑它。
- `just test -p lectern-project-home`：31 个测试通过，都是普通 `#[test]`，连续跑 8 次稳定；`cargo test -p lectern-project-home`（同一进程）也通过。
  - `src/tests/build.rs`（15 个）：`build_groups_sessions_by_worktree`（两个来源交错、主 worktree 在前、`Located` 与 `Resolved` 都归位、prunable 与不在列表里的进 `Other`、子 agent、14 天之外、别的项目的会话、还没归位的会话都不成行）、`build_splits_needs_review_and_done`（`progress` 为 None / 部分 / 全部 / `changed_since_seen` / 没有文件五种）、`build_without_overview_has_a_pending_outside_row`、`build_drops_overview_sessions_missing_from_list`、`build_marks_running_within_ten_minutes`（10 分钟整算运行中，多 1 ms 不算；最后一轮没结束但几天没动静的不算）、`build_counts_packages_today`（有 / 没有计数、`Capped`；按路径段排序，`web/ui` 在 `web` 之后、`web-extra` 之前）、`build_shows_packages_by_scan_state`、`build_folder_project_hides_outside`、`build_without_usage`、`build_with_usage`（`Limits::tightest` 是下标；百分比取整并夹在 0 到 100；`Loading` 的快照没有总数；只有额度时 `sources.usage` 取额度的代数）、`build_flattens_sidebar_rows`（2000 个子项目、300 个会话，行表的顺序与每个下标，不含贡献条目）、`build_asks_to_complete_each_pending_session_once`、`build_names_the_branch`、`build_settles_the_files_once_they_are_not_pending`、`build_has_no_key_clash_between_sources`。
  - `snapshot_groups`、`snapshot_review`、`snapshot_empty`、`snapshot_packages`、`snapshot_folder`、`snapshot_usage`：六份 insta 快照（`src/tests/snapshots/`），把整个 `HomeSnapshot` 渲染成文本（`snapshot/render.rs`）；逐份看过。
  - `src/tests/plugin.rs`（15 个，真实的 kernel 加假服务，`Executor::manual()`）：验收要求的 `opened_takes_the_overview_interest_and_closed_drops_it`、`events_coalesce_into_one_build`（合成之前来 12 个事件只有一个任务；合成进行中来 5 个事件，之后只再跑一轮，共 3 次 `Projects::snapshot`，再跑就没有事了）、`complete_is_requested_once_per_pending_session`（项目关闭再打开后重新请求）、`usage_slot_empty_builds_without_usage`、`usage_plugin_deactivation_rebuilds`（关注随插件放下，重建一次，Usage 行消失）、`started_handles_an_already_open_project`；设计「测试」里列出的 `usage_activating_after_opened_takes_the_watch`（`started` 返回时 `FakeUsage` 没有收到 `watch`，对账任务跑过之后收到一次，快照重建一次并带 `UsageModel`）、`opened_after_usage_takes_the_watch`、`changed_for_an_unknown_project_counts_as_opened`；另加 `activate_does_no_work`、`opening_another_project_drops_the_old_interest_first`、`usage_events_recompose`、`a_project_without_a_snapshot_is_skipped_and_recovers`、`a_result_for_a_closed_project_is_dropped`、`deactivation_drops_the_interests`。
  - `src/tests/logs.rs`：`canary_stays_out_of_home_logs`，金丝雀放在会话标题、项目名（也是路径）、分支名、子项目名与目录里，trace 级别跑完打开、合成、`SessionsChanged`、`Changed`、概览事件、usage 插件停用、关闭；没有记录含金丝雀或 `fixture`（路径），`home snapshot built` 带 `cards` 计数，`overview session not in list` 按会话的 UUID 标签记录。
- 验收的 `rg`：`rg 'std::fs|std::process|git2|gix|std::thread::spawn' crates/project_home/src`、`rg '^lectern-(review|project|agents|usage|workspace)(\s|\.|=)' crates/project_home/Cargo.toml`、`rg 'eprintln!|println!|log::' crates/project_home/src` 都没有结果；`rg 'gpui' crates/project_home/Cargo.toml crates/project_home/src` 也没有结果。
- 没有验证：`activate_registers_contributions_only` 和所有 `#[gpui::test]`、语义树快照、`project_home/build_1000` 与 `project_home/resident_bytes` 基准随第 2、3 步（本步没有视图，也没有 `bench` 的插件夹具）；`canary_stays_out_of_home_logs` 的「画一帧、按 ↵」随第 2 步；`Overview.packages`（前置条件 A）还没有合并，`package_counts` 在 `Home` 里总是 false，`build` 的计数路径只由 `Inputs::changed_today` 的测试覆盖；`app` 的装配（`every_profile_graph_validates`）随第 3 步。

### 第 2 步

视图与贡献登记（`HomeScreen`、`HomeSidebar`、Action 与快捷键、语义节点），在 macOS（arm64，Rust 1.97.0）上运行，屏幕锁着，没有启动应用，全部是 headless 的 `#[gpui::test]`：

- `just check`（含 `cargo clippy --workspace --all-targets --all-features -- -D warnings`、架构检查 32 个 crate、文档链接）、`just deny` 通过。`cargo fmt --all --check` 通过。
- `just test -p lectern-project-home`：60 个测试通过（第 1 步的 31 个仍通过，新增 29 个），连续跑 3 次稳定。
  - `src/tests/plugin.rs`：`activate_does_no_work` 改名为设计里的 `activate_registers_contributions_only`：`activate` 登记了且只登记了 `Screen("project-home")`、`SidebarPanel("project")`、`SidebarEntry("overview")`（`Project` 区）、`KeyBindings` 四项，没有提交任务、没有读服务、没有取关注。
  - `src/tests/views.rs`（23 个，真实的 kernel、真实的插件、`Executor::manual()`、假服务，两个视图由插件登记的工厂建出，主区在 760pt 栏里）：`sidebar_lists_sessions_under_worktrees`（顺序、`Untitled`、Overview 选中）、`sidebar_builds_only_visible_rows`（1000 个会话加 2000 个子项目共 3004 行，一帧建 45 行、滚动之后 46 行，窗口一屏 45 行）、`home_shows_cards_outside_and_done`、`arrow_keys_move_across_groups`（↓ 从第一张卡片经 Outside 到 Done，↑ 反向，到头不回绕，⌥↑ ⌥↓）、`enter_pushes_the_review_route`（没有选中项时 ↵ 只选第一项）、`outside_opens_the_outside_review`（点击与按键）、`clicking_a_card_and_a_sidebar_session_pushes_review`（含另一个 worktree 的会话）、`views_entries_dispatch_their_command`（含 Overview 的 `PopToBase`）、`late_entries_appear_without_a_new_snapshot`（`ContributionsChanged` 之后两行出现，快照没有重建、没有再调 `Projects::snapshot`）、`the_overview_entry_follows_the_current_screen`、`usage_row_shows_tokens_and_the_tightest_limit`（没有 usage 插件时只有标签；插件后到时补上数字与额度条；点击 `Push(USAGE, Project(key))`）、`new_sidebar_view_reads_the_current_snapshot`（面板视图 drop 再建，不等事件就有内容；被 drop 的视图的发送端在下一次合成之后被清掉）、`progress_ring_transitions_between_snapshots`（环从旧值 0.4 出发、目标 1.0，卡片移到 Recently Done 后沿用；离开快照的卡片丢掉环）、`milestone_is_reported_on_the_first_settled_frame`、`skeleton_appears_after_100ms_only`（`advance_clock` 到 100 ms 出现 `phase=skeleton`，1 s 变 `phase=progress`，概览到达后消失）、`closed_project_clears_the_views`、`selection_scrolls_into_view`（400pt 高的窗口里 ⌥↓ 选到最后一项，滚动偏移变负，该行可见）、`a_new_round_makes_the_dot_blink`（`running` 变 true 或运行中 `last_activity` 变化才算新一轮）、`packages_follow_the_scan_state`（`Pending` 一行占位、`Ready`、`Unavailable` 不显示这一节）、`a_folder_project_has_no_worktrees_and_no_outside_row`、`other_worktrees_and_a_capped_overview`、`the_toolbar_shows_the_name_and_the_count`、`the_screen_takes_the_focus_back_from_the_root_only`（`workspace` 把焦点收回根视图之后，↓ 仍然有效；焦点在覆盖层上时屏幕不抢）。
  - `src/tests/tree.rs`：`home_tree_with_cards`、`home_tree_empty`、`sidebar_tree_with_views_and_usage` 三份 insta 快照（`ui::semantic::Recorder` 的记录，去掉 `bounds` 和 `declared_at`），逐份看过。
  - `src/tests/present.rs`（3 个）：相对时间（含时钟比文件时间慢的情形）、`Running`、第三方 agent 的名字在前、信号的连接，en 与 zh-CN 各一份期望值；这些文字不在语义树里。
  - `src/tests/logs.rs`：`canary_stays_out_of_home_logs` 改成 `#[gpui::test]`：画一帧两个视图、按 ↓ ↵（`home command` 的字段是 `push` / `session`）、`SessionsChanged`、`Changed`、概览事件、usage 插件停用、关闭项目，每步之后画帧；没有记录含金丝雀或 `fixture`，且 `home command`、`home milestone reported`、`home view attached` 都写了。
- 验收的 `rg`（第 2 步）：`rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|\.font_family\(|gpui_component::Button' crates/project_home/src`、`rg '"[A-Z][a-z]+ [a-z]' crates/project_home/src --glob '!tests*'`、`rg 'NodeId::new\("project_home"' crates/project_home/src` 都没有结果；`rg 'NodeId::new\("project-home"' crates/project_home/src` 有结果（`nodes.rs`）。第 1 步的三条 `rg`（`std::fs|std::process|…`、插件 crate 依赖、`eprintln!|println!|log::`）仍没有结果。
- 本 crate 的测试只用 nextest 跑（`just test`）：`ui::semantic::Recorder` 的开关是进程级的，一个进程里并发的测试会互相记到对方的节点，所以第 1 步写的「`cargo test` 同一进程也通过」不再成立。
- `GPUI` 的 `KeyBindingContextPredicate` 接受 `project-home` 里的连字符（`is_identifier_char` 含 `-`），测试台架里键绑定经 `KeyBinding::load` 以登记时的谓词 `Content > project-home` 装入，按键的测试（↓ ↑ ⌥↑ ⌥↓ ↵）走的就是它。
- 没有验证：真实的 `workspace` 根视图承载这两个视图（台架自己放 `Sidebar` / `Content` 上下文，`Content > project-home` 在真实窗口里是否命中，要等 `app` 装配后手动试）；真实屏幕上的观感（悬停淡入、选中行里次要文字的对比度、进度环在选中行的底色上、`svg` 图标是否画出、骨架块）；`app` 的装配、`every_profile_graph_validates`、`app/home_frames`、`project_home/build_1000`、`project_home/resident_bytes` 基准（第 3 步）；`Overview.packages` 的接入（第 3 步，侧栏的计数列由 `build` 测试和 `packages` 夹具覆盖，视图用 `Overview.packages` 为 Some 的夹具验证了显示）。
- 文档补回（本提交）：#248 合并后，晚合并的 #247 与 #251 带着旧版的本文档和 `AGENTS.md` 覆盖了第 2 步的状态行、「实际验证」「与设计的差异」和模块表，代码没有受影响。本提交把 #248 的这几处补回，并保留 #247 的装配记录；在 `9991a3d` 上重跑：`just test -p lectern-project-home` 60 个测试通过，`just check`、`just deny`、`cargo metadata --format-version 1 --locked --all-features` 通过，第 2 步的各条 `rg` 与 `python3 scripts/check_architecture.py`（32 个 crate）通过。

## 与设计的差异

### 第 1 步

- **不登记贡献**：`activate` 不登记 `Screen`、`SidebarPanel`、`SidebarEntry`、`KeyBindings`，`activate_registers_contributions_only` 随第 2 步。它们的载荷是 `ui::contrib` 的类型（视图工厂返回 `AnyView`，`KeyBindings` 持有 `Box<dyn Action>`），要依赖 `gpui` 和 `lectern-ui`，而第 1 步的验收列表（和 workspace 设计第 1 步的做法）是不依赖 GPUI 的部分；`SidebarEntry("overview")` 虽然只是数据，也要 `lectern-ui` 的 `SidebarItem`，所以一并推到第 2 步。清单照设计依赖 `dyn Contributions`（激活顺序与成品一致），`activate` 不取它，测试用 `FakeContributions` 只数登记的次数（0）。因此本 crate 目前在 Linux 上也编译和测试，第 2 步加 `gpui` 后自动退出 Linux 的集合。
- **快照里没有 `node`**：`WorktreeGroup`、`SessionRow`、`PackageRow` 的 `node: NodeId` 先不放。`ui::semantic` 在 `ui` 设计的第 3、4 步，现在不存在；`NodeId` 随快照建一次的做法不变，第 2 步加回这些字段。
- **没有视图的唤醒通道**：`Home` 里没有 `Vec<mpsc::UnboundedSender<…>>`，「依赖」表里的 `futures` 也没有加：工作区依赖里并没有 `futures`（设计写「已在」有误），第 1 步没有视图。第 2 步决定用 `futures::channel::mpsc` 还是 GPUI 自己的通道（workspace 设计第 1 步同样没有加）。`Home::snapshot()` 取当前快照；项目关闭时快照置为 None，不发空快照（没有接收方）。
- **`ProjectHomeUsagePlugin::new(executor, slot)`**：设计写 `new(slot)`，但 `started` 里那个对账任务要经 `Executor::run_gated` 提交，需要 executor（「本 crate 的公开项」已改，「对其他 crate 的要求」D 随之改）。
- **`Changed` 当作 `Opened` 要求项目还开着**：设计写「收到不认识的项目的 `Changed` 时按 `Opened` 处理」；`Closed` 之后到达的 `Changed` 会让一个已经关闭的项目重新成为基底并取一次概览关注。现在只在 `Projects::snapshot` 里还有这个项目时才这样处理，已关闭的忽略（`opened_takes_the_overview_interest_and_closed_drops_it` 检查）。
- **日志字段 `sid`**：`overview session not in list` 的字段不能叫 `session`（`lectern-logging` 把它保留为作用域的键，宏会编译失败），改叫 `sid`，值仍是 `Value::uuid(会话 ID 的 UUID 或 128 位哈希)`，推导与 `review`、`usage` 相同。
- **`Overview.packages` 经 `snapshot::Inputs::changed_today: Option<&[(RelPath, u32)]>` 进入 `build`**：`Home` 传 `Overview.packages`（[#239](https://github.com/SoloJiang/lectern-app/pull/239)，在本 PR 之前合并）；它为 None 时 `package_counts` 为 false、`changed_today` 为 0，侧栏只显示名字。`OutsideCount::Capped` 时 `build` 无论入口给什么都当作没有计数。
- **`build` 的签名**：设计只写「`snapshot::build` 的纯函数」。现在是 `build(&Inputs) -> Built`，`Inputs` 有项目快照、各来源的会话列表、概览、今天的改动数、`usage`（`UsageInputs`）和合成时刻；`Built` 除快照外还给出不在会话列表里的概览会话（插件据此记 warn）和需要 `complete` 的会话键（按显示顺序、去重）。
- **`SourceGenerations::usage`**：用量快照的代数；还没有用量快照时取额度的代数（设计没有规定）。
- **一个未规定的情形**：概览里 `progress` 为 `Some` 但 `total == 0` 的会话（没有文件）不进 Needs Review，也不进 Recently Done（设计的两个条件都不满足），`build_splits_needs_review_and_done` 的 `empty` 一条检查这一点。`archived` 的会话没有特别处理，照常列出。
- **`Home` 公开**：`pub struct Home`（`snapshot()`）从 lib 导出，第 2 步的视图工厂持有它的 `Arc`；`ProjectHomePlugin` 只在测试里暴露它。`attach` 在 `started` 里对 `Projects::open_projects()` 取最后一个（最近打开的）作为基底。

### 第 2 步

- **唤醒通道不带快照**：`Home` 的发送端是 `UnboundedSender<()>`，不是 `Arc<HomeSnapshot>`。`HomeSnapshot` 没有「空」的形式，项目关闭时要让视图清空，所以消息只是唤醒：视图先 `Home::attach_view(view)` 登记发送端，再读 `snapshot()`、`sidebar_entries()` 和当前屏幕，之后每次唤醒（合并成一次）重读，项目关闭时读到 None 就清空。先登记后读，所以两者之间的变化不会漏。`futures` 已在工作区依赖里（`workspace` 第 2 步加入），「依赖」表的说明随之改了。
- **插件转发 `WorkspaceEvent`**：设计写「视图订阅 `ContributionsChanged`」，视图是主线程上的实体，订阅不了 kernel 事件。插件用 `ctx.on::<WorkspaceEvent>` 在 `Home::on_workspace` 里转：`ContributionsChanged` 唤醒视图重取 `sidebar_entries()`；`ScreenChanged` 记下当前屏幕是不是主页，侧栏的 Overview 条目据此选中（「界面」：当前屏幕是主页时选中）。
- **视图经 `Home` 用 `Contributions`**：`ServiceRef::get` 给的 `Admitted` 只能在一次调用里持有，视图不能存 `Arc<dyn Contributions>`，所以 `dispatch`、`sidebar_entries`、`report_milestone` 在 `Home` 上（`Parts` 多了 `contributions`），并在这里写 `home command` 的 debug 日志。
- **选中项按身份保存**：`HomeScreen` 存 `Pick::Session(SessionKey)` 或 `Pick::Outside`，不是 `Selection { list, index }`：新快照把卡片重新排序（或一张卡片从 Needs Review 移到 Recently Done）时，选中项跟着会话走；会话消失时清空。↑ ↓ 的顺序不变。
- **快照里的 `now` 与 `node`**：`HomeSnapshot::now`（合成时刻）是新增的，相对时间从它往回数，使一帧只取决于快照（「已知限制」第一条因此是设计上的结果，而不是视图的疏忽）。`WorktreeGroup`、`SessionRow`、`PackageRow` 加了 `node: NodeId`（第 1 步的差异里说好第 2 步加回）；`SessionRow::node` 是这一行所在位置的节点（卡片用卡片的，侧栏用侧栏的），所以没有 `Card::node`。`NodeId` 里的业务键来自会话 ID、路径、子项目目录，换行或 `].` 会让 `NodeId` 在调试构建里 panic，先过 `nodes::safe`。
- **语义节点与设计表的差异**：
  - worktree 小标题是 `project-home.sidebar.sessions.worktree.item[<key>]`：`NodeId` 只有 `item(key)` 一种方括号写法，不能写 `worktree[<key>]`；没有 worktree 的组用键 `other` / `folder`。
  - 卡片的事实（`agent`、`seen`、`total`、`drifted`、`critical`、`changed`）在行内容的 `….facts`（`Group`）节点上，Outside 的 `files` / `capped` / `pending` 在 `project-home.outside.facts` 上：`ui::Row` 不带 `attrs`，`GroupedList::row` 只收 `Row`，没法给行本身声明。`Row` 自己的 `ListItem` 节点只有 `selected`。`….facts` 的 `Label` 是标题（原样）或 Outside 的消息键。
  - `project-home.needs-review` 是 `GroupedList` 的容器，没有 `ready` 属性（`GroupedList` 不收 `attrs`）：`ready` 放在 `project-home.title` 上；Outside 的容器是 `project-home.outside.group`，`project-home.outside` 是它的行。
  - 新增：`project-home.sidebar.sessions.empty`、`project-home.sidebar.project.item[…]` / `project-home.sidebar.bottom.item[…]`（其他 crate 登记在这两区的条目）、`project-home.loading`（概览没有结果时 Needs Review 位置上的占位，属性 `phase`）。
  - `project-home.sidebar.list` 的 `visible` 是上一帧建出的行数：行是布局之后在 `uniform_list` 里建的，这一帧声明节点时还不知道。
- **侧栏的行不是 `ui::Row`**：`Row` 是 36pt、带分隔线、选中是强调色，侧栏要 24pt 的等高行，所以侧栏行是自己的 `div` + `semantic_interactive`，悬停的 6% 淡入淡出用与 `Row` 相同的做法（`window.use_keyed_state` 里的 `Transition`，`HOVER_IN` / `HOVER_OUT`）。Overview 条目选中时用 `unemphasized_selected_content_background`（macOS 侧栏的灰色选中，文字保持 `label`）；侧栏的会话行不会处于选中态（审阅屏幕隐藏侧栏，侧栏也没有键盘焦点）。图标用 `gpui::svg()` 画 `Icon::path()`（`ui` 没有图标元素）。
- **`ui` 的一处改动**：`lectern_ui::on_accent` 公开（原来是 `pub(crate)`）。主区卡片放在 `Row` 里，选中时 `Row` 把文字设成 `on_accent`，但卡片里次要文字用 `TextRole::Secondary`（灰色）会盖掉它，在强调色上看不清；选中的卡片次要行因此改用 `on_accent` 的 75%。改动是 `ui` 的两处可见性加一行 `pub use`。
- **等待概览**：`LoadingPhase::Progress` 要显示「不确定进度」，`ui` 没有这个控件（`Progress` 只有 `Bar` / `Ring` 两种确定的）。第 2 步在 `Progress` 阶段继续显示骨架，节点的 `phase=progress` 记下阶段，不确定进度等 `ui` 有了控件再换（待办，写在 PR 里）。起点用 `background_executor().now()`（测试里可以 `advance_clock`），两个阈值各一个一次性定时器，概览到达时丢掉；没有周期性的定时器。
- **里程碑**：在 `HomeScreen::render` 里经 `Home::frame_drawn` 报告（`workspace` 自己在下一帧回调里记下时刻）；`request` 是 `Opened` 的 `OpenRequest` 序号，`started` 时项目已经打开的情形没有序号，记 0。
- **键**：↓ 没有选中项时选第一项、↑ 没有选中项时选最后一项（macOS 列表的习惯，设计没有写）；`Open` 没有选中项时选第一项（设计）。谓词 `Content > project-home` 靠的是屏幕根视图是焦点路径上最深的上下文（`workspace` 设计第 2 步的差异里写过 `>` 的这个要求）。屏幕在工厂里建完就取得焦点；审阅屏幕弹出之后，被它压在下面的主页没有焦点（`workspace` 把焦点收回自己的根视图），所以主页在 `render` 里「焦点在根视图上（或没有焦点）而不在自己里面」时取回焦点，焦点在检查器、面板、覆盖层或侧栏上时不动（用 `Window::context_stack` 的最内层上下文判断）。这是 `workspace` 测试里 `Blank` 视图的做法的收窄版；更好的做法是 `workspace` 在弹出屏幕之后把焦点交给新的栈顶屏幕，等 `app` 装配后手动试过再定（PR 里写明）。
- **`Home` 的 `Parts`**：`contributions: ServiceRef<dyn Contributions>`；`activate` 里 `contributions.get()` 取不到时返回 `DependencyUnavailable`，与 `welcome` 相同。

### `app` 的装配（D）

- **装配早于第 3 步**（[#247](https://github.com/SoloJiang/lectern-app/pull/247)）：`dyn Usage` 有了提供者（`usage` 插件），第 2 步（[#248](https://github.com/SoloJiang/lectern-app/pull/248)）已让 `project-home` 登记贡献，所以 `app` 的 `Profile::Desktop` 现在装配 `project-home` 与 `project-home-usage`（共用一个 `UsageSlot`），desktop 的插件数为 8。`app/home_frames`、里程碑与基准仍随第 3 步。

### 为 #283 修订「界面」

「界面」按设计画布的 Overview 画板重写（[#283](https://github.com/SoloJiang/lectern-app/issues/283)，`Refs`），实现随后的 PR 跟随本文。下面是改了的决定（旧 → 新）：

- **大标题**：40pt → Large Title 层级（26pt）；标题下加一句说明（运行中的 agent 数、会话外的改动数）。
- **工具栏**：副标题「N to review」→「分支 · N worktrees」；右侧没有控件 → ⌘K 搜索框和 New session 按钮（各自的归属还没有实现，实现之前不画）；左侧加侧栏开关（`workspace` 画，命令还没有）。
- **卡片**：第一行从「相对时间 / Running」改成「agent · 分支 · 时间 · 状态」；标题半粗；行尾从只有进度环改成进度环 + 「2/6」+ ›；`project-home-progress` 退为无障碍名称。状态词 `Waiting`、`Ended` 要 `agents` 提供，现在只有 `Running`。
- **Outside Any Session**：一行文字 → 卡片（所在分支、前 3 个路径、Review 按钮）；路径要 `review` 提供。
- **Recently Done**：沿用会话卡片 → 普通行（标题 + 结果）；结果要 git / MR 的来源，现在显示相对时间。
- **侧栏**：Sessions 从按 worktree 分组、显示相对时间 → 所有 worktree 混排、只列最近 3 个、其余「N more」、运行中是行尾绿点。这是设计文档当初自己的选择，不是产品负责人的决定，所以按画布改；代价是侧栏不再显示会话在哪个 worktree（需要产品确认）。行高 24pt → 画布的 28pt（数值在交互规范里，本文不写）。Packages 的数量从「N today」→ 只画数字。Views 区加「Project」小标题和图标，数量等 `SidebarItem` 带上数量字段（现在没有，之前不画）。Usage 从一条细带 → 卡片，额度条撑满宽度，文字点名额度窗口。
- **阅读栏**：仍然是 760pt（画布的 Overview 画板写 820pt，与 Flows 画板矛盾，取 Flows 与交互规范）。
- **圆点**：仍然是 8pt（画布的卡片画了 10pt）。
- **快照的数据结构也变了**（第 1 步已实现的 `snapshot::build` 要跟着改）：`HomeSnapshot` 加 `worktree_count`；`Card` 加 `branch`、`status`（新的 `SessionStatus`）；`MainModel` 加 `running`；`OutsideRow::Files(u32)` 改成 `Files { count, names }`；`TightestLimit` 加 `window_minutes`；`SidebarRow` 去掉 `WorktreeHeader`，会话行不再按组排，按 `last_activity` 混排。
- **键**：删 `project-home-current-worktree`、`project-home-other-worktrees`；改 `project-home-subtitle`、`project-home-outside-files`、`project-home-limit`；加 `project-home-sessions-more` 等（见键表）。.ftl 文件由实现 PR 改。

