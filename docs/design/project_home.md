# project_home 设计

状态：已接受，尚未实现。

对应 issue [#30](https://github.com/SoloJiang/lectern-app/issues/30)。`project_home` 是打开项目后首先看到的屏幕（[交互规范「屏幕」](interaction.md#屏幕)的 02 画板，[ADR 0012](../adr/0012.md)）：项目侧栏（Overview、按 worktree 分组的 Sessions、带今天改动数的 Packages、其他 crate 贡献的 Worktrees / Files / MR 入口、底部的 Usage 行）和主区（Needs Review 的会话卡片、Outside Any Session、Recently Done）。它只显示领域层算好的快照（`project`、`agents`、`review`、`usage`），自己不做归位、比较和统计；它由 [workspace 设计](workspace.md)的根视图承载，作为 `Screen(PROJECT_HOME)` 和 `SidebarPanel("project")` 贡献进去。

## 范围

- 做：
  - 项目侧栏面板：Overview 条目；Sessions 按 worktree 分组的会话行（agent 圆点、标题、运行中标记）；Packages 的子项目行与今天的改动数；`workspace` 收到的其他贡献条目（`Views` 区：Worktrees [#16](https://github.com/SoloJiang/lectern-app/issues/16)、Files [#10](https://github.com/SoloJiang/lectern-app/issues/10)、Merge Requests [#25](https://github.com/SoloJiang/lectern-app/issues/25)）；底部 Usage 行（本项目最近 7 天的 token 与最紧张的额度条）。
  - 主区屏幕：大标题一句话说明待审数量；Needs Review、Outside Any Session、Recently Done 三个分组列表；每行的状态与信号文字、已看进度环。
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
  - 按 worktree 的 Needs Review：`Overview.sessions` 只含当前 worktree，其他 worktree 的会话在侧栏里可见、可进入审阅，但不出现在主区（见「未决问题」1）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui` | 视图、`uniform_list`、Action、`#[gpui::test]` | 已锁定（[ADR 0016](../adr/0016.md)） |
| `lectern-ui` | `colors`、`strings`、`Text`、`TextRole`、`GroupedList`、`Row`、`AgentDot`、`Progress`、`Button`、`Skeleton`、`loading_phase`、`motion`、`semantic`、`contrib` 的载荷类型 | 视图层的公共库，非插件 |
| `lectern-i18n` | `keys::project_home::*`、`Args`、`count_compact`、`Strings::relative_time` | [ADR 0013](../adr/0013.md) |
| `lectern-kernel`、`lectern-services` | `Plugin`、`Context`、`PluginHandle`、`ServiceSet`；`dyn Projects`、`dyn SessionSource`、`dyn Review`、`dyn Usage`、`dyn Contributions` 与它们的数据类型和事件 | 架构规则 7（[ADR 0025](../adr/0025.md)）：插件只经 `services` 往来 |
| `lectern-executor` | 合成快照的 `Background` 任务 | 后台任务只经 executor |
| `lectern-logging` | 日志宏、`Value::uuid` | [ADR 0046](../adr/0046.md) |
| `futures`（`channel` feature） | 视图的唤醒通道 `mpsc::unbounded` | 已在工作区依赖里（workspace 设计同样用它） |
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
//   `activate` 里没有 token），守卫交给 `ctx.own_staged`；`ctx.on::<ProjectEvent>`、`ctx.on::<SessionsChanged>`、`ctx.on::<ReviewEvent>`。
//   不做 IO，不提交任务。
// started：保存 `PluginHandle`；把「标记 dirty 并安排合成」的回调写进 `UsageSlot::on_change`；读一次
//   `Projects::open_projects()`，已有打开的项目时按 `Opened` 处理（监听在 token 变为 `Live` 之前收不到事件，
//   kernel 设计「激活时的上下文」）。`started` 与 token 变为 `Live` 之间到达的 `Opened` 仍可能漏掉，所以监听里
//   收到不认识的项目的 `ProjectEvent::Changed` 时同样按 `Opened` 处理（project 设计：成功的 `Opened` 之后至少还有一次 `Changed`）。

/// `app` 装配时构造，放进 `PluginSet`（id "project-home-usage"）。只负责把 `dyn Usage` 放进共用的一格：
/// `usage` 插件失败或停用时项目主页照常显示，只是没有 Usage 行（kernel 没有可选依赖，
/// 做法与 review 设计前置条件 F 的 "code-intel-brief" 相同）。
pub struct ProjectHomeUsagePlugin { /* usage slot */ }
impl ProjectHomeUsagePlugin { pub fn new(slot: UsageSlot) -> Self; }

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
    /// 当前 worktree 的 HEAD 的短名（`refs/heads/main` → `main`；detached 取 OID 前 8 位），文件夹项目为 None。
    pub branch: Option<Arc<str>>,
    pub active_worktree: Option<WorktreeKey>,
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
    /// 侧栏滚动区里来自快照的行，拍平、等高（24pt）：Sessions 标题、worktree 小标题、会话行、Packages 标题、子项目行，
    /// 按显示顺序。贡献的条目（`Project` 区在前、`Views` 区在后）不在快照里：视图从 `sidebar_entries()` 取，
    /// 在渲染时拼成完整的行表（`Project` 条目 + `rows` + `Views` 条目），`ContributionsChanged` 时重取。
    /// 整个滚动区是一个 `uniform_list`，长度是三段之和，每帧只建可见的行（「界面」「性能」）。
    pub rows: Arc<[SidebarRow]>,
}

/// 侧栏滚动区里来自快照的一行；下标指向 `groups` / `packages`。
pub enum SidebarRow {
    SessionsHeader,
    WorktreeHeader(u32),                // `groups` 的下标
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

pub struct MainModel {
    /// 概览还没有第一份结果（`Review::overview` 为 None）：主区按 `loading_phase` 显示骨架。
    pub overview_ready: bool,
    /// 大标题的数字，等于 `needs_review.len()`。
    pub needs_review: Arc<[Card]>,
    pub outside: OutsideRow,
    pub done: Arc<[Card]>,
}

pub struct Card {
    pub session: SessionRow,
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
    Files(u32),
    /// `OutsideCount::Capped(n)`：显示全部改动路径数并说明没有区分。
    Capped(u32),
}

pub struct UsageModel {
    /// 最近 7 天的合计（`ProjectUsage::total.total()`），快照还没算出时为 None。
    pub tokens_7d: Option<u64>,
    /// `Limits::tightest` 指向的窗口：agent、`used_percent`（0..=100 取整）、`resets_at`。
    pub tightest: Option<TightestLimit>,
}
pub struct TightestLimit { pub agent: Agent, pub used_percent: u8, pub resets_at: Option<Timestamp> }
```

分类规则（`snapshot::build` 的纯函数，`#[test]` 覆盖，见「测试」）：

| 结果 | 条件 |
|---|---|
| 侧栏 Sessions 的行 | `SessionList::in_project(key)` 里 `parent` 为 None（子 agent 会话在父会话的审阅里，不单独成行）、`last_activity` 在最近 14 天内（与概览的声明窗口相同）的会话；组内按 `last_activity` 从新到旧 |
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

- `Home`（插件状态，`Mutex`）：当前项目的 `ProjectKey`、`OverviewInterest`、`Option<Arc<HomeSnapshot>>`、合成任务的标记（`dirty`、`building`）、已请求 `complete` 的会话键集合（随项目关闭清空）、视图的唤醒发送端列表 `Vec<mpsc::UnboundedSender<Arc<HomeSnapshot>>>`、已报告里程碑的 `OpenRequest`。
- `HomeScreen`（GPUI `Entity`，主区）：`Arc<HomeSnapshot>`、选中项（`Selection { list: NeedsReview | Outside | Done, index }`）、`FocusHandle`、每张卡片进度环的 `motion::Transition`（按 `SessionKey` 保存，快照换代时按键沿用，让进度环从旧值过渡到新值）。
- `HomeSidebar`（GPUI `Entity`，侧栏面板）：`Arc<HomeSnapshot>`、滚动区的 `UniformListScrollHandle`、最近一次 `sidebar_entries()` 的 `Arc`（`ContributionsChanged` 时重取，视图订阅这个事件；贡献条目的行由视图从它拼出，不经合成任务）、展开状态不保存（分组总是展开）。
- 两个视图在 `workspace` 的工厂里建：建好时从 `Home` 读当前快照并登记自己的发送端；视图 drop 时发送端随之失效，插件在下一次发送出错时清掉它。侧栏面板被 Files 替换再切回时重新建一个视图，所以它必须从 `Home` 读到最新快照，不能等下一次事件。
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

- 屏幕与层：[交互规范「屏幕」](interaction.md#屏幕)的 02 Overview；项目侧栏与项目主页都在常驻层（[「三层结构」](interaction.md#三层结构)）。`ScreenLayout { sidebar: Shown, back_button: false, reading_column: true, inspector: false, floating_bar: false }`。工具栏：标题是 `HomeSnapshot::name`（原样），副标题是 `project-home-subtitle`（待审数量），右侧没有控件。
- 侧栏面板（248pt，容器由 `workspace` 画）：第 1 到第 4 节在一个 `uniform_list` 里（`Project` 区的条目 + `SidebarModel::rows` + `Views` 区的条目；条目来自 `sidebar_entries()`，由视图拼入；行高都是 24pt，小标题与条目同高），整个滚动区一起滚动，每帧只建可见的行；第 5 节固定在面板底部，不随滚动。从上到下：
  1. `Project` 区：`overview` 条目（本 crate 登记的 `SidebarEntry`，`on_select: PopToBase`，当前屏幕是主页时选中）。`workspace` 设计的调用方一节把 Sessions、Packages 也写成 `SidebarEntry`；本文只登记 `overview`：Sessions 与 Packages 是带行的分组，`SidebarEntry` 只有一个标签，放不下行。`Project` 区里其他 crate 登记的条目画成普通行，排在 `overview` 之后。
  2. Sessions：标题行 `project-home-sessions`；每个 worktree 一个小标题（分支短名；当前 worktree 加 `project-home-current-worktree` 的次要文字；文件夹项目不显示小标题；`Other` 组用 `project-home-other-worktrees`），下面每行：8pt agent 圆点（`AgentDot`，运行中时 `blink` 一次：新一轮到达 = `running` 从 false 变 true，或 `last_activity` 变化；`Agent::Other(name)` 的会话没有圆点，`name` 原样显示在次要文字的开头）、标题一行（超出截断）、次要文字（相对时间；运行中时为 `project-home-running`）。14 天内没有会话时显示一行 `project-home-sessions-empty`。
  3. Packages：标题行 `project-home-packages`；每行子项目名，行尾次要文字是今天的改动数（`project-home-changed-today`，0 时不显示；`package_counts` 为 false 时整列不显示）；嵌套的子项目按 `depth` 缩进。子项目 `Pending` 时显示一行 `Skeleton`（`PackagesSkeleton`），`Unavailable` 或没有子项目时不显示这一节。行不可点击（「本 issue 之外」）。子项目可以有几千个（10 万文件的形状是 2000 个），它们和会话行一样在同一个 `uniform_list` 里，不单独封顶。
  4. `Views` 区：`sidebar_entries()` 里 `section == Views` 的条目，按 `workspace` 给的顺序；每行标签（`Text::key(item.label)`）和条目给出的图标（`ui::contrib::SidebarItem::icon: Option<ui::Icon>`，有时画），点击 `dispatch(item.on_select)`。没有条目时整节不显示。
  5. `Bottom` 区：`usage` 条目（`usage_view` 登记的 `SidebarEntry`，`on_select: Push(USAGE)`）由本 crate 画成两行：第一行标签 + `count_compact(tokens_7d)`（`project-home-usage-7d`），第二行最紧张的额度条（`Progress::Bar`，`used_percent / 100`）加次要文字（`project-home-limit`：agent 名与百分比）。`UsageModel` 为 None（`usage` 插件不在）时只画标签；`tokens_7d` 为 None 时数字处是 `Skeleton`；`tightest` 为 None 时没有第二行。`Bottom` 区的其他条目画成普通行。
- 主区（居中 760pt 阅读栏）从上到下：大标题（`TextRole::LargeTitle`，`project-home-title`，数字是 `needs_review.len()`）；三个 `GroupedList`，标题分别是 `project-home-needs-review`、`project-home-outside`、`project-home-done`。卡片一行：agent 圆点、标题、两行次要文字（第一行：相对时间，运行中时 `project-home-running`；第二行：信号，用 ` · ` 连接）、行尾进度环（`Progress::Ring`，`seen / total`；`progress` 为 None 时不画）。Outside 行：标题 `project-home-outside-files`，`Capped` 时用 `project-home-outside-capped`，`Pending` 时数字处是 `Skeleton`。Recently Done 为空时不显示这一组；Needs Review 为空而概览已就绪时显示 `project-home-all-reviewed`。
- 信号（第二行灰字；只显示成立的项，都不成立时第二行省略）：

  | 信号 | 键 | 来源 | 现在 / 以后 |
  |---|---|---|---|
  | 已漂移 | `project-home-signal-drifted` | `SessionOverview::drifted == Some(true)` | 现在 |
  | critical | `project-home-signal-critical` | `SessionOverview::critical > 0` | 现在 |
  | 审过之后又改过 | `project-home-signal-changed` | `Progress::changed_since_seen > 0` | 现在 |
  | 公共包改动、API 变化 | `project-home-signal-public-package`、`project-home-signal-api` | `code_intel` 的 `BriefRules` 提供者 + `SessionOverview` 带规则 ID | 以后（「本 issue 之外」）；键先登记，实现前不显示 |
  | 测试结果 | `project-home-signal-tests` | MR 流水线（#25） | 以后 |

- `ui` 的控件与动效原语：`GroupedList`、`Row`、`AgentDot`、`Progress`（Ring / Bar）、`Button`、`Skeleton`、`loading_phase`；进度环用 `motion::PROGRESS` 的 `Transition`，悬停由 `Row` 自带（`HOVER_IN` / `HOVER_OUT`），圆点轻闪用 `ARRIVE`；减弱动态效果由 `ui` 处理。本 crate 不写颜色值、字体和时长（「验收」的 `rg` 检查）。
- Fluent 键（`crates/i18n/locales/{en,zh-CN}/project-home.ftl`，`keys::project_home::*`；会话标题、分支名、子项目名、agent 名经 `Text::verbatim` 或 `Args::str` 原样显示）：

  | 键 | en | zh-CN |
  |---|---|---|
  | `project-home-overview` | Overview | 概览 |
  | `project-home-sessions` | Sessions | 会话 |
  | `project-home-sessions-empty` | No sessions in the last 14 days | 最近 14 天没有会话 |
  | `project-home-current-worktree` | current | 当前 |
  | `project-home-other-worktrees` | Other worktrees | 其他 worktree |
  | `project-home-untitled-session` | Untitled session | 未命名会话 |
  | `project-home-running` | Running | 运行中 |
  | `project-home-packages` | Packages | 包 |
  | `project-home-changed-today` | { $count } today | 今天 { $count } |
  | `project-home-usage-7d` | { $tokens } tokens · 7 days | { $tokens } token · 7 天 |
  | `project-home-limit` | { $agent } { $percent }% used | { $agent } 已用 { $percent }% |
  | `project-home-title` | { $count -> [0] Nothing needs review [one] { $count } session needs review *[other] { $count } sessions need review } | { $count -> [0] 没有待审的会话 *[other] { $count } 个会话待审 } |
  | `project-home-subtitle` | { $count -> [0] All caught up [one] { $count } to review *[other] { $count } to review } | { $count -> [0] 已全部审阅 *[other] 待审 { $count } 个 } |
  | `project-home-needs-review` | Needs Review | 待审阅 |
  | `project-home-outside` | Outside Any Session | 会话外的改动 |
  | `project-home-outside-files` | { $count -> [one] { $count } changed file [other] { $count } changed files } | { $count } 个改动的文件 |
  | `project-home-outside-capped` | { $count } changed paths (not attributed) | { $count } 个改动路径（未归属） |
  | `project-home-done` | Recently Done | 最近完成 |
  | `project-home-all-reviewed` | Every session is reviewed. | 所有会话都已审阅。 |
  | `project-home-empty` | No agent sessions in this project yet. | 这个项目还没有 agent 会话。 |
  | `project-home-progress` | { $seen }/{ $total } reviewed | 已看 { $seen }/{ $total } |
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
  | `project-home.sidebar` | `Group` | `worktrees`（组数）、`sessions`（行数） |
  | `project-home.sidebar.overview` | `ListItem` | `selected` |
  | `project-home.sidebar.list` | `List` | `rows`（滚动区的总行数）、`visible`（这一帧建出的行数）；下面的节点都是它这一帧可见的行，层级来自元素嵌套，ID 只按区命名 |
  | `project-home.sidebar.sessions` | `Text` | Sessions 标题行 |
  | `project-home.sidebar.sessions.worktree[<WorktreeKey>]` | `Text` | worktree 小标题；`branch`、`active` |
  | `project-home.sidebar.sessions.item[<会话 ID>]` | `ListItem` | `selected`；`agent`、`running`、`pending`（`tail_pending`） |
  | `project-home.sidebar.packages` | `Text` | Packages 标题行；`counts`（是否有今天的改动数） |
  | `project-home.sidebar.packages.item[<dir>]` | `ListItem` | `changed`、`depth` |
  | `project-home.sidebar.views.item[<贡献 ID>]` | `ListItem` | `Label::key` 是贡献的标签键 |
  | `project-home.sidebar.usage` | `ListItem` | `tokens`、`percent`；没有 usage 时不出现 |
  | `project-home.sidebar.usage.limit` | `Progress` | `fraction` |
  | `project-home.title` | `Text` | `Label::key` = `project-home-title`，`count` |
  | `project-home.needs-review` | `List` | `ready`（概览是否就绪） |
  | `project-home.needs-review.item[<会话 ID>]` | `ListItem` | `selected`；`agent`、`seen`、`total`、`drifted`、`critical`、`changed` |
  | `project-home.needs-review.item[<会话 ID>].progress` | `Progress` | `fraction` |
  | `project-home.outside` | `ListItem` | `selected`；`files` / `capped` / `pending` |
  | `project-home.done` | `List` | — |
  | `project-home.done.item[<会话 ID>]` | `ListItem` | `selected`；同上 |
  | `project-home.empty` | `Text` | `Label::key` 是空状态的键 |

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
| 文件夹项目 | 没有 worktree 分组标题；Outside 行不显示（没有基准）；卡片没有进度环与 Drifted（`review` 的 `Degraded::FolderProject`） |
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
  - warn `overview session not in list`：`session`（`Value::uuid`；会话 ID 不是 UUID 时传它的 128 位哈希）；同一会话只记一次。
  - warn `usage call refused`：`reason`（`token-dead`）；`usage` 插件停用期间最多记一次。
  - warn `home build dropped`：`reason`（`project-closed` / `generation-stale`）、计数。
  - error 没有：本 crate 没有会失败的操作（服务方法都不返回错误，命令由 `workspace` 处理）。
- 详细级别：debug `home snapshot built`（每次合成，字段同上，另带 `sources` 的代数）；debug `home command`：`command`（`WorkspaceCommand` 的判别名）、`target`（`session` / `outside` / `entry`）；debug `home complete requested`：`sessions`（计数）；debug `home view attached` / `detached`：`view`（`screen` / `sidebar`）；trace `home event`：事件类型名与代数。
- 关联 ID：不另设作用域。`warn` 里的 `session` 是记录字段，不是 `Ctx::session`（本 crate 不处理单个会话的流程）。
- 不记录：会话标题、项目名、worktree 根路径、分支名、子项目名与目录、`SessionKey::id` 之外的会话内容、token 数之外的用量明细、贡献条目的标签文字、`NodeId` 的业务键。用计数、代数、`Value::uuid`、判别名代替。
- 金丝雀测试：`canary_stays_out_of_home_logs`（`src/tests/logs.rs`）：`lectern_logging::testing::capture()` 在 trace 级别下，用 `FakeSessions`、`FakeProjects` 构造一个会话标题、项目名、分支名、子项目名、worktree 根路径都含金丝雀的项目，跑一次合成、画一帧、按 ↵、关闭项目，`assert_absent(金丝雀)`。

## 测试

依赖 GPUI，只在 macOS 上编译和测试：`just test -p lectern-project-home`。会话、项目、概览、用量、贡献都用 `services` trait 的假实现，放在本 crate 的 `testing` 模块（`FakeSessions`、`FakeProjects`、`FakeReview`、`FakeUsage`、`FakeContributions`）：直接构造 `SessionList`、`ProjectSnapshot`、`Overview`、`UsageSnapshot` / `Limits` 交出去，记下收到的 `complete`、`watch_overview`、`watch`、`dispatch`，不解析脚本；`read` / `turns` / `load` / `resolve` 一律 `unreachable!`（本 crate 不得调用它们）。不用真实的 git、会话扫描和文件 IO，也不依赖 `review` 等插件 crate 的 `test-support`。

- `snapshot::build`（`#[test]`，不开窗口）：`build_groups_sessions_by_worktree`（主 worktree 在前、`Resolved` 与 `Located` 都归位、prunable 的进 `Other`、子 agent 不成行、14 天之外的不出现）；`build_splits_needs_review_and_done`（`progress` None / 部分 / 全部 / `changed_since_seen` 四种）；`build_drops_overview_sessions_missing_from_list`；`build_marks_running_within_ten_minutes`；`build_counts_packages_today`（有 `Overview.packages` 与没有、`Capped`）；`build_folder_project_hides_outside`；`build_without_usage`；`build_flattens_sidebar_rows`（`rows` 的顺序与下标，2000 个子项目时长度正确；不含贡献条目）；每个用例的 `HomeSnapshot` 渲染成文本做 insta 快照（`snapshot_*`）；`project_home/build_1000` 的夹具也在这里构造。
- 插件（`#[test]`，`TestKernel`）：`activate_registers_contributions_only`（登记了 `Screen`、`SidebarPanel`、`SidebarEntry`、`KeyBindings`，没有任务）；`opened_takes_the_overview_interest_and_closed_drops_it`；`events_coalesce_into_one_build`（跑着的时候来 5 个事件，结束后只再跑一轮）；`complete_is_requested_once_per_pending_session`；`usage_slot_empty_builds_without_usage`、`usage_plugin_deactivation_rebuilds`、`usage_activating_after_opened_takes_the_watch`（`Opened` 先到、usage 插件后 `started`：`started` 返回时 `FakeUsage` 还没有收到 `watch`，token 变为 `Live`、对账任务跑过之后收到一次，快照重建一次并带 `UsageModel`）、`opened_after_usage_takes_the_watch`（相反的顺序，结果相同）；`started_handles_an_already_open_project`；`changed_for_an_unknown_project_counts_as_opened`。
- `#[gpui::test]`（`TestAppContext`，`init_fixed`）：`sidebar_lists_sessions_under_worktrees`；`sidebar_builds_only_visible_rows`（2000 个子项目、1000 个会话时一帧里建出的行数不超过可见行数加一屏）；`home_shows_cards_outside_and_done`；`arrow_keys_move_across_groups`（↓ 从最后一张卡片到 Outside 再到 Done，↑ 反向，到头不回绕）；`enter_pushes_the_review_route`（`FakeContributions` 收到 `Push(REVIEW, Session(key))`）；`outside_opens_the_outside_review`；`clicking_a_sidebar_session_pushes_review`；`views_entries_dispatch_their_command`；`late_entries_appear_without_a_new_snapshot`（`FakeContributions` 在快照建好之后加一个 `Project` 区和一个 `Views` 区的条目并发 `ContributionsChanged`，下一帧两行都出现）；`usage_row_shows_tokens_and_the_tightest_limit`；`new_sidebar_view_reads_the_current_snapshot`（面板视图 drop 再建，不等事件就有内容）；`progress_ring_transitions_between_snapshots`（`Transition` 从旧值出发）；`milestone_is_reported_on_the_first_settled_frame`（`files` 为 `Pending` 的快照不报告，`Ready` 的那一帧报告一次，之后不再报告）；`skeleton_appears_after_100ms_only`；`closed_project_clears_the_views`。
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
- **B. `i18n`：`locales/{en,zh-CN}/project-home.ftl`**，内容是「界面」的键表。
- **C. `services`、`ui`、`workspace`**：本 crate 用到的都已是 workspace 设计或 `ui` 设计的内容，不新增要求。其中两项由 workspace 设计的前置条件定义、还没有实现：`services::workspace` 模块（前置条件 A，[#221](https://github.com/SoloJiang/lectern-app/pull/221)）和 `ui::contrib` 的载荷类型与 `into_payload`（前置条件 B，[workspace 设计「`ui::contrib` 的载荷类型」](workspace.md#uicontrib-的载荷类型)；`ui` 设计 [#218](https://github.com/SoloJiang/lectern-app/pull/218) 的 `contrib` 模块也列出了它们，侧栏条目的类型叫 `SidebarItem`，带 `icon: Option<ui::Icon>`）。本 crate 的 `Screen`、`SidebarPanel`、`SidebarEntry`、`KeyBindings` 登记和 `Views` 区的图标都靠它们，所以它们在「验收」里列为前置条件。`GroupedList`、`Row`、`AgentDot`、`Progress`、`Skeleton`、`semantic` 在 `ui` 设计的第 3、4 步；`sidebar_entries()`、`report_milestone`、`dispatch` 在 workspace 设计里。`usage_view` 登记 `SidebarEntry("usage")`（`section: Bottom`、`on_select: Push(USAGE)`）时，本 crate 为它画 token 与额度条；它的设计文档写明这一点。
- **D. `app`**：装配时构造 `UsageSlot`，分别交给 `ProjectHomePlugin::new` 和 `ProjectHomeUsagePlugin::new`，两个插件都放进 `Profile::Desktop` 的 `PluginSet`；bench 的 `Driver` 加 `home` 场景（`app/home_frames`）。
- **E. `bench`**：预算表加 `project_home/build_1000`、`project_home/resident_bytes`、`app/home_frames`；`app/open_project`、`app/idle_monorepo`、`app/idle_100k` 随本 crate 的最后一步改为生效。

## 未决问题

1. **主区只显示当前 worktree 的会话，还是所有 worktree 的。** `Overview.sessions` 按 review 的设计只含当前 worktree（计数比较只对当前 worktree 做，ADR 0035）。默认（本文按它写）：主区只有当前 worktree 的会话；其他 worktree 的会话在侧栏里按 worktree 分组，可以点进审阅，但没有进度环和信号。另一个选项：主区按 worktree 分段列出全部会话，要 `review` 对每个 worktree 各做一份概览，内存按 worktree 数增长，需要另写 ADR 调整 ADR 0035 的份额。
2. **Packages 的「今天改动数」的含义。** 默认（「对其他 crate 的要求」A）：相对 HEAD 未提交、且最后修改在今天的文件数，按子项目归类。另一个选项：不看日期，显示相对 HEAD 的全部改动文件数（名字改成「changed」）；它更稳定（不随午夜变化），但与交互设计画布上的「今天」不一致。
3. **侧栏 Sessions 的时间范围。** 默认：最近 14 天（与概览的声明窗口相同），更早的会话以后经 ⌘K 搜索。另一个选项：全部列出（1000 个会话时侧栏很长，虚拟列表能画，但找不到东西）。

## 验收

分三步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-project-home`、`just deny` 通过。前置条件的顺序：B（`project-home.ftl`）和 workspace 设计的前置条件 A（`services::workspace`）在第 1 步之前；workspace 设计的前置条件 B（`ui::contrib` 的载荷类型、侧栏图标）、`ui` 设计的第 3、4 步（控件、探针节点）和 workspace 的第 2 步（根视图）在第 2 步之前；本文的 A（`Overview.packages`）、D、E 随第 3 步（A 可以更早）。

**第 1 步：快照与插件（不开窗口的部分）。**

- `snapshot::build` 与「测试」里 `build_*`、`snapshot_*` 的 insta 快照通过。
- 插件的 `#[test]` 通过：`activate_registers_contributions_only`、`opened_takes_the_overview_interest_and_closed_drops_it`、`events_coalesce_into_one_build`、`complete_is_requested_once_per_pending_session`、`usage_slot_empty_builds_without_usage`、`usage_plugin_deactivation_rebuilds`、`started_handles_an_already_open_project`。
- `python3 scripts/check_architecture.py` 通过：`lectern-project-home` 带 `plugin = true`，不依赖其他插件 crate；只有 `app` 依赖它。
- `rg 'std::fs|std::process|git2|gix|std::thread::spawn' crates/project_home/src` 没有结果；`rg '^lectern-(review|project|agents|usage|workspace)(\s|\.|=)' crates/project_home/Cargo.toml` 没有结果（不依赖插件 crate，dev-dependency 也不行；行首锚定，所以不会匹配到本 crate 的 `name = "lectern-project-home"`）。
- `canary_stays_out_of_home_logs` 通过（合成与插件部分）；`rg 'eprintln!|println!|log::' crates/project_home/src` 没有结果。

**第 2 步：视图（`#[gpui::test]`）。**

- 「测试」里的 `#[gpui::test]` 和语义树快照通过。
- `rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|\.font_family\(|gpui_component::Button' crates/project_home/src` 没有结果（`ui` 设计的持续检查）。
- `rg '"[A-Z][a-z]+ [a-z]' crates/project_home/src --glob '!tests*'` 没有界面字符串字面量（都走 `keys::project_home::*`）。
- `rg 'NodeId::new\("project-home"' crates/project_home/src` 有结果，且 `rg 'NodeId::new\("project_home"' crates/project_home/src` 没有结果（探针 ID 的名字段只用 `[a-z0-9-]`）。
- `canary_stays_out_of_home_logs` 覆盖渲染与按键。

**第 3 步：装配、里程碑与基准（`Closes #30`，前置条件 A、D、E 随这一步）。**

- `just test -p lectern-app`：`every_profile_graph_validates` 里 desktop 的插件数加二。
- 手动在 macOS 上运行发布构建（临时 `HOME`、有会话的仓库）：打开文件夹后侧栏列出按 worktree 分组的会话、Packages 带今天的改动数，主区显示 Needs Review、Outside Any Session、Recently Done；↑ ↓ ↵ 进入审阅、⌘[ 返回；日志里 `home milestone reported` 的 `since_opened_us` < 100 000。
- `just bench app/`：`app/open_project` 中位数 < 100 ms；`app/idle_monorepo` < 100 MB、`app/idle_100k` < 120 MB；`app/home_frames` 通过。`just bench project_home/`：`project_home/build_1000` < 2 ms、`project_home/resident_bytes` < 1 MB。
- `app/idle_empty` 与改动前相比增加不超过 64 KiB。

## 已知限制

- 相对时间只在下一次快照时刷新：没有事件时「3 min ago」不会自己变成「4 min ago」。采样状态里不设定时器（ADR 0017 的静止）。
- Packages 的今天改动数不在午夜自动归零（「对其他 crate 的要求」A）。
- 侧栏只列最近 14 天的会话（「未决问题」3）。
- 主区没有其他 worktree 的会话（「未决问题」1）。
- `project-home` 依赖 `dyn Review`：`review` 失败时整个项目主页随之不可用。
