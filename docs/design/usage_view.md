# usage_view 设计

状态：已接受，尚未实现。

对应 issue [#32](https://github.com/SoloJiang/lectern-app/issues/32)。`usage_view` 是 Usage 屏幕（[交互规范「屏幕」](interaction.md#屏幕)的 05 画板，按需层）：额度卡片、按 agent 堆叠的每日 token 柱状图、当前项目的合计与按分支（P3 起按 MR）、按模型的明细、所有项目的合计与占比、项目的合并与确认，以及 Claude Code statusline 的默认接入（[ADR 0014](../adr/0014.md)、[ADR 0020](../adr/0020.md)、[ADR 0022](../adr/0022.md)、[交互规范「用量」](interaction.md#用量)）。数据全部来自 `usage` 的快照（[usage 设计「接口」](usage.md#接口)），本 crate 不读会话日志、不查 SQLite，只显示快照、发 `UsageCommand`。

## 范围

- 做：
  - Usage 屏幕：`Screen(screens::USAGE)` 与侧栏底部的 `SidebarEntry("usage")` 的登记，工具栏的范围与时间窗口切换。
  - 额度卡片：每个 `LimitWindow` 一行，带使用百分比、进度条、重置时间、采样时间和过期状态；快到上限时变色并加一行文字（额度提醒，「数据结构」的「额度的级别」）。
  - 每日柱状图：所选窗口的每一天按 agent 堆叠。
  - 合计：四类 token 与缓存命中率。
  - 当前项目：按分支（P2 标为「分支用量」）或按 MR（P3）的行，「不属于任何 MR」一行（P2 标为「默认分支与无法归属」），按模型的行；普通文件夹没有分支和 MR 这一级。
  - 所有项目：各项目合计与占比，「其他」一行；点一个项目行切到它的明细（ADR 0020「界面位置」）。
  - 项目标识的用户操作：「路径已不存在」的行「合并到…」，临时项目的「是同一个项目」「是新项目」，以及这三者的撤销。视图只发 `UsageCommand`，别名由 `usage` 记录。
  - Claude Code statusline 的默认接入：首帧之后检查并安装（默认开启）、首次生效时提示、Usage 页上的关闭与重新开启、本 crate 自己的策略文件。安装和恢复本身由 `platform` 完成（ADR 0014「影响」）。
- 不做：
  - 统计、去重、归属、额度快照的生成、statusline JSON 的解析：`usage`。
  - 编辑 Claude Code 的设置文件、写 statusline 脚本、恢复原配置：`platform`（「对其他 crate 的要求」B）。
  - 项目侧栏底部的 Usage 行（7 天合计与最紧张的额度条）：`project_home` 画，本 crate 只登记条目（[project_home 设计「界面」](project_home.md#界面)）。
  - 会话简报、每轮标题里的 token：`review_view`。Worktrees 列表的分支用量：`worktrees`。Conversation 的上下文窗口占用：Conversation 面板所在的 crate。
  - 按会话的明细：ADR 0020 把统计单位改为项目和 MR，Usage 页不按会话列出（issue 正文的「按会话」以 ADR 0020 为准）。
  - 费用：ADR 0022 取消，只显示 token 和额度百分比。
- 本 issue 之外：
  - 设置页里的 statusline 开关：设置页做出来时加入，接口不变（「未决问题」2）。
  - MR 视图（P3，#25）。超过热窗口的历史（`UsageQuery::History`）：现在没有界面用它。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-kernel`、`lectern-services` | `Plugin`、`Context`、`PluginHandle`、`ActivationToken`；`dyn Usage`、`dyn Contributions` 与它们的数据类型和事件 | 架构规则 7（[ADR 0025](../adr/0025.md)）：插件只经 `services` 往来 |
| `lectern-executor` | statusline 与策略文件的 `Background` 任务（经 `PluginHandle::own_with`） | 后台线程只由 `executor` 管理（[ADR 0024](../adr/0024.md)） |
| `lectern-platform` | `claude_statusline::{status, enable, disable}`（「对其他 crate 的要求」B） | ADR 0014：statusline 的安装与恢复在 `platform` |
| `lectern-ui` | 控件、`Palette`、`Text`、`motion`、`semantic`、`contrib` 的载荷类型 | [ADR 0015](../adr/0015.md)：颜色、字体、动画只来自 `ui` |
| `lectern-i18n` | `keys::usage::*`、`keys::common::*`、`Args`、`count_compact`、`Strings::relative_time` | [ADR 0013](../adr/0013.md) |
| `lectern-logging` | 「日志与诊断」的宏；测试用 `test-support` 的 `testing::capture()` | [ADR 0046](../adr/0046.md) |
| `gpui` | 视图、Action、`Window::prompt`（系统的确认框） | 已锁定（[ADR 0016](../adr/0016.md)） |
| `serde`、`serde_json` | 策略文件 `state.json` 的序列化 | 已在工作区依赖里 |
| `futures`（`channel` feature） | 唤醒视图的 `mpsc::unbounded` | 已在 `[workspace.dependencies]` 里，不新增 crate |
| `lectern-kernel`（`test-support`，dev）、`insta`（dev）、`tempfile`（dev） | `TestKernel`、展示模型与语义树的快照、策略文件的临时目录 | 已在 Cargo.lock |

本 crate 是插件 crate（实现 PR 在 `Cargo.toml` 的 `[package.metadata.lectern]` 加 `plugin = true`，[ADR 0025](../adr/0025.md)），含两个插件（见「插件」），都是 `AfterFirstFrame`，都不提供服务。它不依赖 `usage`、`workspace`、`project_home` 这些插件 crate（dev-dependency 也不行，`scripts/check_architecture.py`），只依赖 kernel 层、`ui`、`platform` 和非插件的库 crate；只有 `app` 和 tool 层依赖它。不引入新的外部依赖。所在层与依赖方向见 [crates/usage_view/AGENTS.md](../../crates/usage_view/AGENTS.md)。

## 接口

### 本 crate 的公开项

```rust
pub struct UsageViewDeps {
    /// 由 `app` 创建并传入。
    pub executor: lectern_executor::Executor,
    /// 本 crate 的数据目录：`app` 取 `platform::dirs::app_data()` 下的 `usage_view/`。
    /// 里面是策略文件 `state.json` 和 `platform` 的 `statusline/` 目录（脚本与安装记录，只有 `platform` 写）。
    pub data_dir: PathBuf,
    /// Claude Code 的用户设置文件：`app` 取 `$CLAUDE_CONFIG_DIR` 的第一项（设置时）或 `~/.claude`，
    /// 加 `settings.json`。目录不存在时 `platform` 报告 `NoConfig`，本 crate 不安装。
    pub claude_settings: PathBuf,
    /// 脚本每次写出 JSON 的位置：`app_data()` 下的 `usage/statusline/claude-code.json`
    /// （[usage 设计「对其他 crate 的要求」](usage.md#对其他-crate-的要求)的 C）。
    pub statusline_sample: PathBuf,
}

/// 两个插件："usage-view" 与 "usage-view-data"，共用一个 `Arc<Shared>`。`app` 逐个放进 `PluginSet::with`。
/// 只保存依赖、建空的 `Shared`，不做 IO。
pub fn plugins(deps: UsageViewDeps) -> [(Name, UsageViewPlugin); 2];
pub struct UsageViewPlugin { /* 角色（Screen / Data）, Arc<Shared> */ }
pub const PLUGIN_ID: &str = "usage-view";
pub const DATA_PLUGIN_ID: &str = "usage-view-data";
```

### 插件

kernel 没有可选依赖，所以与 [project_home](project_home.md#本-crate-的公开项) 的 `project-home-usage`、[review_view](review_view.md) 的 `review-view-handoff` 相同，拆成两个插件：`usage` 插件失败或停用时，侧栏底部的 Usage 标签照常显示，点进来是「现在无法显示用量」，statusline 的开关照常可用。

| 插件 | 提供 | 依赖 | 激活 | 职责 |
|---|---|---|---|---|
| `usage-view` | — | `dyn Contributions` | `AfterFirstFrame` | 登记屏幕、侧栏条目和快捷键；statusline 的检查、安装、关闭与提示 |
| `usage-view-data` | — | `dyn Usage` | `AfterFirstFrame` | 把 `dyn Usage` 放进 `Shared`；`UsageEvent` 的监听，只转发 |

```rust
// "usage-view"：Manifest { provides: [], requires: [service::<dyn Contributions>()], activation: [AfterFirstFrame] }
//   activate（kernel 线程）：取 ServiceRef<dyn Contributions>；`register(Screen, None)`、`register(SidebarEntry, None)`、
//     `register(KeyBindings, None)` 的守卫交给 `ctx.own_staged`（`activate` 里没有 token）。不做 IO，不改 `Shared`。
//   started（kernel 线程）：`Shared::set_handle(handle, contributions)` 只记录；`own_with` 提交 statusline 的启动任务
//     （「数据流与线程」第 4 步）。
//   `Active` drop（停用）：`Shared::clear_handle()`；正在运行的 `platform` 调用是阻塞调用，跑完为止，结果丢弃（只记日志）。
// "usage-view-data"：Manifest { provides: [], requires: [service::<dyn Usage>()], activation: [AfterFirstFrame] }
//   activate：取 ServiceRef<dyn Usage>；`ctx.on::<UsageEvent>`（监听调 `Shared::changed`）。不写 `Shared`。
//   started：`Shared::set_usage(service, token)` 只记录；`own_with` 提交一个只调 `Shared::wake_all()` 的任务，
//     已经打开的 Usage 屏幕在主线程上取关注（token 这时已 `Live`）。
//   `Active` drop：`Shared::clear_usage()`，唤醒所有屏幕；屏幕放下关注，显示「现在无法显示用量」。
```

登记的贡献：

| kind | id | order | 载荷 |
|---|---|---|---|
| `Screen(ScreenLayout { sidebar: Shown, back_button: true, reading_column: true, inspector: false, floating_bar: false })` | `usage`（`screens::USAGE`） | 0 | `ui::contrib::Screen { build, toolbar }`：`build` 对 `Target::Project(key)` 建「当前项目」范围的页面，对 `Target::None` 建「所有项目」范围的页面，其他 `Target` 记 warn、按 `Target::None` 处理；`toolbar` 每帧给标题、副标题和两个分段控件 |
| `SidebarEntry { section: Bottom }` | `usage` | 0 | `SidebarItem { label: keys::usage::SIDEBAR, icon: None, on_select: Push(Route { screen: USAGE, target: Target::None }) }`。`project_home` 不用这个 `on_select`，它自己发 `Push(USAGE, Target::Project(key))`（[project_home 设计「界面」](project_home.md#界面)）；其他宿主用 `on_select` 时进入「所有项目」 |
| `KeyBindings` | `usage` | 0 | `usage::Back`：Esc 与 ⌘[，上下文 `Content > usage`（「界面」的 Action 表） |

### 共享状态与视图（本 crate 内部）

```rust
/// 两个插件与屏幕共用。`Send + Sync`，各插件的 `Active` 和屏幕工厂各持有一份 `Arc`。
struct Shared {
    deps: UsageViewDeps,
    /// usage-view-data 的 started 记下，Active drop 时清掉。
    usage: Mutex<Option<(ServiceRef<dyn Usage>, ActivationToken)>>,
    /// usage-view 的句柄与 `dyn Contributions`（statusline 任务和一次性提示用）。
    handle: Mutex<Option<(PluginHandle, ServiceRef<dyn Contributions>)>>,
    /// 本次运行记住的时间窗口，之后打开的屏幕从它开始。默认 `Window::D7`。不落盘。
    window: Mutex<Window>,
    /// 打开着的屏幕：每个一条，带它当前的查询和唤醒发送端。
    pages: Mutex<Vec<PageSlot>>,
    /// statusline 的当前状态，整体替换（「数据结构」）。
    statusline: Mutex<Arc<StatuslineState>>,
    /// 测试换成假的；正式实现调 `lectern_platform::claude_statusline`。
    host: Arc<dyn StatuslineHost>,
}
struct PageSlot { id: u64, query: Option<UsageQuery>, wake: futures::channel::mpsc::UnboundedSender<()> }

impl Shared {
    /// 取服务：`token.enter()`，取不到（插件已停用）为 None，按没有 usage 处理。任何线程，不阻塞。
    fn usage(&self) -> Option<(ServiceRef<dyn Usage>, Admission)>;
    /// `UsageEvent` 的监听调用（executor 线程）：`Changed` 只唤醒查询相同的屏幕，`Limits` 唤醒全部。只取锁、发送，不做别的。
    fn changed(&self, event: &UsageEvent);
    fn wake_all(&self);
    /// 视图发起的 statusline 操作：排一个 `Background` 任务（见「数据流与线程」第 5 步）。主线程调用，立即返回。
    fn statusline_command(&self, command: StatuslineCommand);
}

/// `platform::claude_statusline` 的三个函数。都阻塞（读写几个小文件），只在 `Background` 任务里调用。
trait StatuslineHost: Send + Sync {
    fn status(&self) -> io::Result<claude_statusline::Status>;
    fn enable(&self) -> io::Result<claude_statusline::Status>;
    fn disable(&self) -> io::Result<claude_statusline::Status>;
}
```

屏幕（`UsagePage`，GPUI 实体）由工厂在主线程上建，弹出时 drop。它持有当前查询的 `UsageInterest`、上一次的 `Arc<UsageSnapshot>` 和由它算出的 `Rc<PageModel>`，以及分段控件的两个 `Transition`。工具栏闭包只有 `&mut App`，所以与 review_view 相同，屏幕建立时把自己的 `WeakEntity` 放进一个 GPUI `Global`（`Current`），工具栏从它读状态。

## 数据结构

### 范围与查询

```rust
enum Scope {
    /// `Target::Project(key)` 打开的项目：`UsageQuery::Project { project: ProjectRef::Open(key), window }`。
    Current(ProjectKey),
    /// 所有项目：`UsageQuery::AllProjects { window }`。
    All,
    /// 在「所有项目」里点进的一个项目：`ProjectRef::Id(id)`。返回（`usage::Back`、标题上方的返回按钮）回到 `All`。
    Drilled(UsageProjectId),
}
```

- 从 `Target::None` 打开时没有 `Current`，工具栏不显示范围控件。
- 点进的项目就是当前打开的项目（`ProjectInfo::key == Some(当前 key)`）时切到 `Current`，不进 `Drilled`。
- 切换范围或窗口：先取新查询的关注，再放下旧关注；新快照到达之前继续显示旧内容，加载态按 `loading_phase(切换后的时间)`（「状态与异常」）。窗口记进 `Shared::window`。

### 展示模型

`present::page(snapshot, limits, statusline, now) -> PageModel` 是纯函数，在主线程上每次唤醒时运行一次（「性能」）。渲染只读 `PageModel`，把枚举映射到 Fluent 键。

```rust
struct PageModel {
    generation: Option<u64>,
    notices: Vec<Notice>,
    limits: Vec<LimitCard>,
    /// 设置文件的目录不存在（`NoConfig`）时为 None：不显示 Claude Code 的接入行。
    statusline: Option<StatuslineRow>,
    /// 快照还没有时为 None。
    chart: Option<Chart>,
    totals: Option<Totals>,
    body: Body,
}
enum Notice {
    Degraded(UsageDegradedKind),             // Recovered / NewerDatabase / Unwritable，不带原因文字
    Backfilling { percent: u8 },             // done / total，total 为 0 时不显示
    StatuslineInstalled { chained: bool },   // 首次生效的横幅（「statusline 的协调」）
    Temporary(TemporaryReason),              // 当前项目自己是临时项目
    Inherited,                               // header.may_include_inherited，放在页面末尾
}
struct LimitCard {
    key: String,                    // `<agent>-<kind>[-<limit_id 的序号>]`，语义节点的业务键
    agent: Agent,
    window: WindowLabel,            // FiveHour / Weekly / Hours(n) / Days(n) / Unknown
    percent: u16,                   // used_percent 四舍五入，可以超过 100
    level: LimitLevel,              // 「额度的级别」
    line: LimitLine,                // ResetsIn(Duration) / Expired / NoReset
    updated: Duration,              // now - sampled_at，负数按 0
}
struct Chart { series: Vec<Agent>, days: Vec<ChartDay>, max: u64 }
struct ChartDay { date: LocalDate, parts: Vec<u64> /* 与 series 对应 */, total: u64 }
struct Totals { tokens: Tokens, total: u64, cache_hit: Option<u8> }
enum Body {
    /// 还没有快照，或 `usage` 不在。
    Loading,
    Unavailable,
    Project(ProjectBody),
    All(AllBody),
}
struct ProjectBody {
    info: ProjectInfo,
    /// 有任何 `RowKey::Mrs` 时为 true：标题与「不属于任何 MR」用 P3 的文字。
    mr_mode: bool,
    rows: Vec<DetailRow>,           // 普通文件夹为空，不显示这一组
    models: Vec<ModelRow>,
}
struct DetailRow { key: DetailKey, tokens: u64, percent: u8, review: u64, unattributable: u64 }
enum DetailKey {
    Branch { name: Arc<str> /* 去掉 refs/heads/ */, worktree: bool },
    Mrs { labels: Vec<(u64 /* number */, Arc<str> /* title */, MrState)> },
    Unassigned,
}
struct ModelRow { agent: Agent, model: Arc<str> /* 空串显示为「未知模型」 */, tokens: u64, percent: u8 }
struct AllBody {
    rows: Vec<ProjectRow>,          // 至多 100 行，其余计入 hidden
    hidden: usize,
    other: Option<(u64, u8)>,       // 「其他」的 token 与占比，为 0 时 None
    merged: Vec<MergedRow>,
    /// 「合并到…」的候选：状态为 `Active` 的项目（含窗口内为 0 的），按名字排序。
    candidates: Vec<(UsageProjectId, Arc<str>)>,
}
struct ProjectRow { info: ProjectInfo, tokens: u64, percent: u8, action: RowAction }
enum RowAction {
    None,
    /// `PathMissing`：「合并到…」。
    MergeInto,
    /// `Temporary { LocalRewrite | PrimaryRemoteChanged }`：「是同一个项目」「是新项目」。
    Confirm { candidate: Option<UsageProjectId> },
    /// `Temporary { ForgeIdPending | Unborn }`：只显示原因，`usage` 会自动判定。
    Waiting(TemporaryReason),
    /// 用户确认过「是新项目」：撤销（`ProjectInfo::confirmed`，「对其他 crate 的要求」A）。
    ConfirmedNew(AliasId),
    /// 本屏幕发了命令、还没有收到更新的快照。
    Pending,
}
struct MergedRow { info: ProjectInfo, into: Option<Arc<str>> /* 在 rows 里找不到时 None */, alias: AliasId, pending: bool }
```

### 计算规则

- 占比：一组行（含「其他」）按 `Tokens::total()` 用最大余数法取整，相加等于 100；合计为 0 时不显示占比。余数相同时按行的顺序。
- 缓存命中率：`cache_read / (input + cache_read + cache_write)`，四舍五入到整数百分比；分母为 0 时不显示。`usage` 已把各服务商的输入规范化成不含缓存的部分（usage 设计「规范化与去重」），所以两种 agent 用同一个公式。
- 所有项目的行：`AllUsage::projects` 的顺序（合计从大到小）。窗口内为 0 的 `Active` 行不显示，只作候选；`PathMissing` 和临时项目即使为 0 也显示，否则用户找不到操作。显示的行超过 100 时只画前 100 行和「再显示 N 个」按钮，点了画全部（这次屏幕内有效）。
- 分支行：`ProjectUsage::rows` 的顺序；分支名去掉 `refs/heads/` 前缀，其他引用原样显示；`worktree` 有值时在次要文字里显示。`review` 不为 0 时次要文字写审阅的部分，`Unassigned` 的 `unattributable` 不为 0 时写无法归属的部分。
- 按模型：`by_model` 的顺序（usage 已按 agent、再按模型 id 排序）。模型 id 原样显示（`Text::verbatim`，来自会话日志），空串显示 `usage-model-unknown`。
- 柱状图：系列是 `daily` 里出现过的 agent，顺序与 `by_agent` 相同（Claude Code、Codex、其他）。柱高 = 当天合计 / `max`；`max` 为 0 时所有柱高为 0。颜色：Claude Code 用 `palette.agent_claude`，Codex 用 `palette.agent_codex`，其他 agent 都用 `palette.system(SystemColor::Gray)`。
- 额度卡片的顺序：Claude Code、Codex、其他，同一 agent 内按窗口从短到长（`window_minutes`，没有时 `FiveHour` < `SevenDay` < `Primary` < `Secondary`）。窗口文字：`FiveHour` 或 300 分钟 → 「5 小时」；`SevenDay` 或 10080 分钟 → 「每周」；其余能被 1440 整除的按天，否则按小时（向上取整）；都没有时 `usage-limit-other`。
- 重置时间：`resets_at - now`。不到 1 小时写分钟（向上取整），不到 1 天写小时与分钟，否则写天数（向上取整）。`now >= resets_at` 而 `usage` 还没有标 `expired` 时，按 `expired` 显示。

### 额度的级别

额度提醒在应用内表达：卡片和侧栏的进度条变色，并加一行文字（「不只靠颜色区分」，[交互规范「视觉」](interaction.md#视觉)）。阈值由 `services::usage` 定义（「对其他 crate 的要求」A），`project_home` 用同一个函数，两处一致：

| `used_percent` | 级别 | 进度条 | 文字 |
|---|---|---|---|
| < 80 | `Normal` | 强调色 | 无 |
| 80 – < 100 | `Warning` | 系统橙 | `usage-limit-warning` |
| ≥ 100 | `Critical` | 系统红 | `usage-limit-critical` |
| `expired` | `Normal` | 进度条不画 | `usage-limit-expired` |

阈值与形式按「未决问题」3 的决定：只在应用内，不加系统通知。过期的文案按 [usage 设计「未决问题」](usage.md#未决问题) 1 的 (a)，在这里确认为「已重置，等待新数据」。

### statusline 的策略文件

`<data_dir>/state.json`，本 crate 自己读写，只在 `Background` 任务里访问：

```rust
#[derive(Serialize, Deserialize)]
struct PolicyFile { version: u32 /* 1 */, statusline: Policy }
#[derive(Serialize, Deserialize)]
struct Policy {
    /// 用户在 Lectern 里关掉了。之后启动不再安装，直到用户在 Lectern 里重新开启。
    opted_out: bool,
    /// 首次生效的提示已经被用户关掉（横幅点了「好」或「关闭」）。
    notice_shown: bool,
}
```

- 文件不存在：第一次运行，`opted_out = false`、`notice_shown = false`（ADR 0014：默认开启）。
- 文件存在但读不了、认不出、版本更高：按 `opted_out = true` 处理（不安装、不改动），记 warn。用户的「关闭」不能因为文件坏了被推翻；这时卡片显示关闭状态，用户点「开启」会重写文件。
- 写：同目录临时文件写完后 `rename`；先 `create_dir_all(data_dir)`。两次写不重叠（任务串行，见下）。

### statusline 的协调

`platform` 的状态（「对其他 crate 的要求」B）与策略合起来决定启动时做什么：

| `Status` | `opted_out = false`（意图：开） | `opted_out = true`（意图：关） |
|---|---|---|
| `NoConfig`（没有用过 Claude Code） | 不做；不显示接入行 | 同左 |
| `Off { existing }` | `enable()`；成功变成 `On` 时这是「首次生效」 | 不做 |
| `On { current: false, .. }`（脚本是旧版本写的，或路径变了） | `enable()` 更新；不算首次生效 | `disable()` |
| `On { current: true, .. }` | 不做 | `disable()`（上次关闭时恢复失败，按用户最后的意图补做） |
| `Replaced`（别的程序或用户改了 `statusLine`） | 不做：尊重外部的改动，卡片提示，用户可以点「开启」重新接入 | 不做 |
| `Unsupported(_)` | 不做；卡片说明无法修改 | 不做 |

用户的操作先写策略文件，再调 `platform`，所以意图总是先落盘：

- 关闭：`opted_out = true`、`notice_shown = true` 写入成功后调 `disable()`。写入失败时不调 `disable()`，卡片显示失败。
- 开启：`opted_out = false`、`notice_shown = true` 写入成功后调 `enable()`。用户自己开启的不弹提示。

首次生效（启动任务里 `Off` → `On`，且 `notice_shown = false`）时：

1. 经 `dyn Contributions` 的一次性提示接口（「对其他 crate 的要求」D）在窗口里显示 `usage-statusline-hint`。
2. Usage 页顶部显示横幅（`Notice::StatuslineInstalled`），带「好」和「关闭」，直到用户点其中一个（「好」只写 `notice_shown = true`）。

```rust
/// 屏幕读取的 statusline 状态。每次变化整体替换。
enum StatuslineState {
    /// 启动任务还没有完成。
    Checking,
    Known { status: claude_statusline::Status, policy: Policy },
    /// 正在开启或关闭（按钮不可用）。
    Working { enabling: bool, policy: Policy },
    /// 上一次的检查或修改失败：`step` 是 `policy-read` / `policy-write` / `status` / `enable` / `disable`。
    Failed { step: &'static str, policy: Policy, status: Option<claude_statusline::Status> },
}
enum StatuslineCommand { Enable, Disable, DismissNotice }
```

statusline 的任务串行：`Shared` 里有一个「进行中」标记和至多一个排队的命令，后到的命令替换排队的那个；任务结束时取出排队的命令接着做。所以开启和关闭不会交错修改设置文件。

## 数据流与线程

```
主线程：Push(USAGE, Target) ─▶ Screen::build ─▶ UsagePage::new：登记 PageSlot、设 Current、
          Shared::usage() 有值时 Usage::watch(query)（立即返回）；没有时 Body::Unavailable
executor：UsageEvent::Changed { query } ─▶ usage-view-data 的监听 ─▶ Shared::changed ─▶ 同查询的 PageSlot.wake
executor：UsageEvent::Limits ─▶ 同上，唤醒全部屏幕
主线程：唤醒的接收循环（cx.spawn）─▶ Usage::snapshot(&query)、Usage::limits()（都是取锁复制 Arc）
          ─▶ present::page ─▶ Rc<PageModel> ─▶ cx.notify()
主线程：分段控件、点行 ─▶ 换查询：新 watch、旧关注 drop、PageSlot.query 更新 ─▶ 等唤醒
主线程：合并、确认 ─▶ Window::prompt（系统确认框，异步）─▶ 选「合并」─▶ Usage::command(…)（立即返回）
          ─▶ 行标为 Pending ─▶ 下一个 generation 更大的快照到达时清掉
kernel 线程：usage-view 的 started ─▶ own_with ─▶ executor [Background]：读 state.json ─▶ host.status()
          ─▶ 按「statusline 的协调」调 enable / disable ─▶ 写 Shared::statusline ─▶ wake_all
          ─▶ 首次生效时经 dyn Contributions 显示一次性提示
主线程：开启 / 关闭 / 「好」─▶ Shared::statusline_command ─▶ own_with ─▶ [Background]：写 state.json ─▶ host.enable / disable
          ─▶ 写 Shared::statusline ─▶ wake_all
主线程：Pop ─▶ UsagePage drop：UsageInterest drop（usage 释放快照）、PageSlot 注销、Current 清掉（是自己时）
```

1. 视图只读不可变快照、只发命令（架构规则 4）：`snapshot`、`limits` 不阻塞、不做 IO，`usage` 设计允许在 UI 线程上调用；`watch`、`command` 立即返回。
2. 页面的计算（`present::page`）只是遍历快照，至多几百行，在主线程上做（「性能」）。没有解析、git 操作和文件 IO。
3. 唤醒通道合并：同一帧里多次唤醒只算一次（接收循环每次取空通道），回填期间的频繁 `Changed` 不会让页面每个事件重算一次。
4. statusline 的启动任务在 `usage-view` 的 `started` 之后、首帧之后运行，不在冷启动路径上。所有文件 IO（`state.json`、`platform` 的调用）都在 `Background` 任务里；视图代码没有 `std::fs`（「验收」）。这与 welcome 读写 `recent.json` 的做法相同（[welcome 设计「数据流与线程」](welcome.md#数据流与线程)）。
5. 页面每 60 秒重算一次（只在屏幕显示时），更新「x 分钟后重置」「x 分钟前更新」。这是页面自己的计时器，屏幕 drop 时取消。
6. 没有派生数据缓存：快照由 `usage` 持有，本 crate 每次从快照重算，不以 OID 为键缓存任何东西（架构规则 5 不适用）。

## 性能

| 预算 | 本 crate 的份额 |
|---|---|
| 冷启动 20 ms（[ADR 0017](../adr/0017.md)） | 首帧之前没有代码运行：两个插件都是 `AfterFirstFrame`；`plugin/usage-view/activate`、`plugin/usage-view-data/activate` 各 < 1 ms（只登记）。statusline 的启动任务在首帧之后的 `Background` 任务里，读两个小文件，至多写两次 |
| 空闲内存 60 MB（ADR 0017）与打开项目时的预算 | 屏幕没有打开时 < 16 KiB（`Shared`、登记的工厂、`StatuslineState`），从 [ADR 0035](../adr/0035.md) 的余量里分配（project_home 之后剩 5.5 MB）。屏幕在 `Pop` 时 drop，关注随之释放，`usage` 不再保留它的快照 |
| 屏幕打开时的内存 | `PageModel` 至多 100 + 个项目行、300 个分支行、50 个模型行、30 天 × 4 个系列，名字是快照里 `Arc` 的克隆：< 256 KiB。快照本身由 `usage` 持有（计入 `usage/resident_bytes`） |
| 帧时间 120 fps（每帧工作 ≤ 8.3 ms） | 页面不虚拟化：项目行至多 100 行（「计算规则」），柱状图至多 30 × 4 个色块，没有逐帧动画（分段控件的滑块用 `ui` 的 `SEGMENT`，进度条宽度不做动画）。`present::page` < 1 ms，只在唤醒时运行，不在每帧 |
| 打开 Usage 屏幕 | `build` 只建实体和关注（< 1 ms）；第一份快照的计算在 `usage` 的后台（`usage/project_30d`、`usage/all_projects_30d`），到达前按加载态显示 |

基准（`bench`，macOS 判定）：

| ID | 统计量与预算 |
|---|---|
| `usage_view/present_heavy` | 中位数 < 1 ms（`present::page`：`AllUsage` 200 个项目、30 天、3 个 agent；`ProjectUsage` 300 个分支行、50 个模型行；4 个额度窗口。进程内，不开窗口） |
| `usage_view/scroll_all_100` | 每帧工作时间 p95 ≤ 8.3 ms：100 个项目行的「所有项目」从顶部滚到底（`WorkspaceHost::record_frames`，与 `review_view/scroll_5000` 同一套记录方式） |
| `plugin/usage-view/activate`、`plugin/usage-view-data/activate`、`plugin/*/resident` | 随 bench 的插件基准自动覆盖 |

## 界面

对应交互规范的 05 画板（按需层）：从项目侧栏底部的 Usage 行进入，Esc 回到原处（[交互规范「三层结构」](interaction.md#三层结构)）。内容在 760pt 阅读栏里。

- 工具栏（`ui::contrib::Screen::toolbar`）：标题 `usage-title`；副标题是 `usage-subtitle-project`（项目名与天数）或 `usage-subtitle-all`；右侧两个 `Segmented`：范围（本项目 / 所有项目，`Target::None` 时不画）与时间窗口（7D / 14D / 30D，交互规范「屏幕」05）。`Drilled` 时范围控件选中「所有项目」，副标题是点进的项目名。
- 内容，从上到下：
  1. 横幅（`notices` 里除 `Inherited` 外的项）：一行次要文字；statusline 的横幅带「好」（`common-ok`）与「关闭」两个 `Button`（`Small`）。`Drilled` 时最上面是返回按钮（`Button`，`Plain`，`Icon::Back`，`usage-back-to-all`）。
  2. 额度（`GroupedList`，标题 `usage-limits`）：每张卡片一行：`AgentDot`、`usage-limit-title`、右侧 `usage-limit-used`；第二行 `Progress::Bar`（`fraction = percent / 100`，上限 1，级别给颜色，「对其他 crate 的要求」C）；第三行次要文字：重置时间 · `usage-limit-updated`，`Warning` / `Critical` 时前面加级别的文字，过期时只有 `usage-limit-expired`。最后一行是 Claude Code 的接入行（`statusline` 有值时）：状态文字与一个 `Button`（「关闭」或「开启」，`Working` 时不可用）。没有任何卡片时第一行是 `usage-limits-empty`。
  3. 每日 token（标题 `usage-daily`）：柱状图，高 120pt，每天一根柱、按系列从下往上堆叠，柱间 2pt；左上角是 `count_compact(max)`，下方只标第一天和今天（`usage-day`）。悬停一根柱时，图上方的一行次要文字换成那一天的明细（`usage-day-detail` 加每个系列的 `usage-day-agent`），不弹浮层；离开时恢复为窗口合计。柱和色块是 `div`，颜色来自 `Palette`。
  4. 合计：`usage-total`（`count_compact`），下面一行四类 token 与 `usage-cache-hit`。
  5. 当前项目（`Current` / `Drilled`）：分支组（标题 `usage-branches`，`mr_mode` 时 `usage-mrs`；普通文件夹没有这一组），每行：名字、右侧 token 与占比、次要文字（worktree、审阅部分、无法归属部分；MR 行是编号与状态）；`Unassigned` 行的名字是 `usage-unassigned-branches`（`mr_mode` 时 `usage-unassigned-mrs`），排在最后。按模型组（`usage-models`）：`AgentDot`（Claude Code、Codex；其他 agent 不画圆点）、模型 id、token 与占比。
  6. 所有项目（`All`）：项目组（`usage-projects`），每行：项目名、右侧 token 与占比；次要文字是状态（`usage-path-missing`、临时项目的原因、`usage-confirmed-new`、`usage-pending`）；行尾按钮按 `RowAction`：「合并到…」（弹出菜单，列出候选，「对其他 crate 的要求」C），「是同一个项目」（`candidate` 有值时直接确认，没有时弹出同样的候选菜单）与「是新项目」，「撤销」。右键这一行弹出同样内容的菜单。「其他」行在最后（`usage-other`，次要文字 `usage-other-detail`），不可点。点其他项目行进入 `Drilled`。已合并组（`usage-merged`，没有时不画）：每行项目名、`usage-merged-into` 与「撤销」。
  7. 页脚：`Inherited` 时一行次要文字 `usage-inherited`。
- 确认：「合并」与「是同一个项目」先弹系统确认框（`Window::prompt`，`PromptLevel::Info`，文字 `usage-merge-confirm` / `usage-same-confirm` 与对应的 `-detail`，按钮 `usage-merge` / `usage-same-project` 与 `common-cancel`），选第一个按钮才发命令。「是新项目」和「撤销」可以撤回，不弹确认。
- 用到的 `ui` 控件与原语：`Segmented`（`SEGMENT`）、`GroupedList`、`Row`（悬停 `HOVER_IN` / `HOVER_OUT`）、`Button`、`AgentDot`、`Progress`、`Skeleton`、`loading_phase`、`show_menu` 与 `Row::context_menu`；文字只用 `TextRole`。不在视图里定义颜色、字体和动画参数；柱状图的尺寸（120pt、2pt）是布局常量，不是动画。

Fluent 键在 `crates/i18n/locales/{en,zh-CN}/usage.ftl`（文件名取屏幕名 `screens::USAGE`，与 review_view 用 `review.ftl` 相同；键 `usage-*`，生成 `keys::usage::*`，前置条件 E）。项目名、分支名、MR 标题、模型 id 原样显示，作为 `Args::str` 或 `Text::verbatim`：

| 键 | en | zh-CN |
|---|---|---|
| `usage-title` | Usage | 用量 |
| `usage-sidebar` | Usage | 用量 |
| `usage-scope-project` | This Project | 本项目 |
| `usage-scope-all` | All Projects | 所有项目 |
| `usage-window-7d` / `-14d` / `-30d` | 7D / 14D / 30D | 7 天 / 14 天 / 30 天 |
| `usage-subtitle-project` | { $name } · Last { $days } days | { $name } · 最近 { $days } 天 |
| `usage-subtitle-all` | All projects · Last { $days } days | 所有项目 · 最近 { $days } 天 |
| `usage-back-to-all` | All Projects | 所有项目 |
| `usage-unavailable` | Usage isn't available right now. | 现在无法显示用量。 |
| `usage-loading` | Loading usage… | 正在读取用量… |
| `usage-backfilling` | Reading session history… { $percent }% | 正在读取会话历史… { $percent }% |
| `usage-degraded-recovered` | The usage database was damaged and has been rebuilt. Usage from deleted session logs is missing. | 用量数据库损坏，已经重建。已删除的会话日志里的用量无法恢复。 |
| `usage-degraded-newer` | A newer version of Lectern wrote the usage database. New usage isn't recorded. | 用量数据库由更新版本的 Lectern 写入，新的用量不会记录。 |
| `usage-degraded-unwritable` | Lectern can't save usage data. | Lectern 无法保存用量数据。 |
| `usage-inherited` | Some totals may include context that a subagent inherited from its parent session. | 部分合计可能含子 agent 从父会话继承的上下文。 |
| `usage-empty` | No usage in this period. | 这段时间没有用量。 |
| `usage-limits` | Limits | 额度 |
| `usage-agent-claude-code` / `usage-agent-codex` | Claude Code / Codex | Claude Code / Codex |
| `usage-limit-title` | { $agent } · { $window } | { $agent } · { $window } |
| `usage-limit-five-hour` | 5-hour | 5 小时 |
| `usage-limit-weekly` | Weekly | 每周 |
| `usage-limit-hours` | { $count }-hour | { $count } 小时 |
| `usage-limit-days` | { $count }-day | { $count } 天 |
| `usage-limit-other` | Limit | 额度 |
| `usage-limit-used` | { $percent }% used | 已用 { $percent }% |
| `usage-limit-resets-minutes` | Resets in { $count } min | { $count } 分钟后重置 |
| `usage-limit-resets-hours` | Resets in { $hours } h { $minutes } min | { $hours } 小时 { $minutes } 分钟后重置 |
| `usage-limit-resets-days` | Resets in { $count } days（`[one]` 为 1 day） | { $count } 天后重置 |
| `usage-limit-updated` | Updated { $ago } | { $ago }更新 |
| `usage-limit-expired` | Reset · waiting for new data | 已重置，等待新数据 |
| `usage-limit-warning` | Close to the limit | 接近上限 |
| `usage-limit-critical` | Limit reached | 已达上限 |
| `usage-limits-empty` | No limit data yet | 还没有额度数据 |
| `usage-statusline-checking` | Checking Claude Code settings… | 正在检查 Claude Code 的设置… |
| `usage-statusline-on` | Reading limits from the Claude Code status line | 从 Claude Code 的 statusline 读取额度 |
| `usage-statusline-on-chained` | Reading limits from the Claude Code status line. Your status line works as before. | 从 Claude Code 的 statusline 读取额度，原来的 statusline 照常显示。 |
| `usage-statusline-waiting` | Claude Code hasn't reported limits yet | Claude Code 还没有报告额度 |
| `usage-statusline-off` | Claude Code limits are off | Claude Code 额度已关闭 |
| `usage-statusline-replaced` | The Claude Code status line was changed outside Lectern | Claude Code 的 statusline 已在 Lectern 之外改动 |
| `usage-statusline-unsupported` | Lectern can't edit the Claude Code settings file | Lectern 无法修改 Claude Code 的设置文件 |
| `usage-statusline-failed` | Couldn't update the Claude Code settings | 无法更新 Claude Code 的设置 |
| `usage-statusline-turn-on` / `-turn-off` | Turn On / Turn Off | 开启 / 关闭 |
| `usage-statusline-notice` | Lectern added a status line command to Claude Code to read your limits. | Lectern 在 Claude Code 里加了一条 statusline 命令，用来读取额度。 |
| `usage-statusline-notice-chained` | Lectern added a status line command to Claude Code to read your limits. Your status line runs as before. | Lectern 在 Claude Code 里加了一条 statusline 命令，用来读取额度；你原来的 statusline 照常运行。 |
| `usage-statusline-hint` | Lectern now reads Claude Code limits. You can turn this off in Usage. | Lectern 开始读取 Claude Code 额度，可以在用量页关闭。 |
| `usage-daily` | Daily Tokens | 每日 token |
| `usage-day` | { $month }/{ $day } | { $month }月{ $day }日 |
| `usage-day-detail` | { $date } · { $tokens } tokens | { $date } · { $tokens } token |
| `usage-day-agent` | { $agent } { $tokens } | { $agent } { $tokens } |
| `usage-total` | { $tokens } tokens | { $tokens } token |
| `usage-input` / `usage-output` | Input / Output | 输入 / 输出 |
| `usage-cache-read` / `usage-cache-write` | Cache read / Cache write | 缓存读取 / 缓存写入 |
| `usage-cache-hit` | Cache hit rate { $percent }% | 缓存命中率 { $percent }% |
| `usage-percent` | { $percent }% | { $percent }% |
| `usage-branches` | Branch Usage | 分支用量 |
| `usage-mrs` | MR Usage | MR 用量 |
| `usage-unassigned-branches` | Default branch & unattributable | 默认分支与无法归属 |
| `usage-unassigned-mrs` | Not in any MR | 不属于任何 MR |
| `usage-unattributable` | { $tokens } unattributable | 其中 { $tokens } 无法归属 |
| `usage-review-part` | { $tokens } from review | 其中审阅 { $tokens } |
| `usage-mr` | #{ $number } { $title } | #{ $number } { $title } |
| `usage-mr-open` / `-merged` / `-closed` | Open / Merged / Closed | 开启中 / 已合并 / 已关闭 |
| `usage-models` | By Model | 按模型 |
| `usage-model-unknown` | Unknown model | 未知模型 |
| `usage-projects` | Projects | 项目 |
| `usage-other` | Other | 其他 |
| `usage-other-detail` | Projects not opened yet and sessions outside any project | 还没有打开的项目和不在任何项目里的会话 |
| `usage-more` | Show { $count } More | 再显示 { $count } 个 |
| `usage-path-missing` | Path no longer exists | 路径已不存在 |
| `usage-merge-into` | Merge Into… | 合并到… |
| `usage-merge-menu` | Merge Into | 合并到 |
| `usage-merge-none` | No other projects | 没有其他项目 |
| `usage-merge-confirm` | Merge “{ $from }” into “{ $into }”? | 把「{ $from }」合并到「{ $into }」？ |
| `usage-merge-confirm-detail` | Usage recorded for “{ $from }” will count toward “{ $into }”. You can undo this. | 「{ $from }」的用量将计入「{ $into }」，之后可以撤销。 |
| `usage-merge` | Merge | 合并 |
| `usage-temporary-local-rewrite` | Root commit changed | 根提交变了 |
| `usage-temporary-remote-changed` | Main remote changed | 主远端地址变了 |
| `usage-temporary-forge-pending` | Checking the repository on the code host | 正在向代码平台确认仓库 |
| `usage-temporary-unborn` | No commits yet | 还没有提交 |
| `usage-same-project` | Same Project | 是同一个项目 |
| `usage-new-project` | New Project | 是新项目 |
| `usage-same-confirm` | Treat “{ $name }” as “{ $into }”? | 把「{ $name }」当作「{ $into }」？ |
| `usage-same-confirm-detail` | Usage recorded for “{ $name }” will count toward “{ $into }”. You can undo this. | 「{ $name }」的用量将计入「{ $into }」，之后可以撤销。 |
| `usage-confirmed-new` | Confirmed as a new project | 已确认是新项目 |
| `usage-merged` | Merged Projects | 已合并的项目 |
| `usage-merged-into` | Merged into “{ $into }” | 已合并到「{ $into }」 |
| `usage-merged-into-unknown` | Merged into another project | 已合并到另一个项目 |
| `usage-undo` | Undo | 撤销 |
| `usage-pending` | Updating… | 正在更新… |

通用的「好」「取消」用 `common-ok`、`common-cancel`；「x 分钟前」由 `Strings::relative_time` 给出（`$ago`）。

UI 探针的语义节点（`ui::semantic::NodeId::new("usage", 区域, 业务键)`）：

| ID | 角色 | 说明 |
|---|---|---|
| `usage.toolbar.scope`、`usage.toolbar.scope[project]` / `[all]` | `TabBar`、`Tab` | `selected`；`Target::None` 时不出现 |
| `usage.toolbar.window`、`usage.toolbar.window[7d]` / `[14d]` / `[30d]` | `TabBar`、`Tab` | `selected` |
| `usage.back` | `Button` | 只在 `Drilled` |
| `usage.notice[degraded]` / `[backfilling]` / `[statusline]` / `[temporary]` / `[inherited]` | `Text` | `Label::key` 是消息键；`backfilling` 的 `attrs` 有 `percent` |
| `usage.notice.statusline.ok`、`usage.notice.statusline.off` | `Button` | 横幅的两个按钮 |
| `usage.limits` | `List` | |
| `usage.limits.row[<card key>]` | `ListItem` | `attrs`：`agent`、`percent`、`level`（`normal` / `warning` / `critical`）、`expired` |
| `usage.limits.row[<card key>].bar` | `Progress` | `fraction`；过期时不出现 |
| `usage.limits.statusline` | `ListItem` | `attrs`：`state`（`checking` / `on` / `on-chained` / `off` / `replaced` / `unsupported` / `failed` / `working`） |
| `usage.limits.statusline.toggle` | `Button` | `disabled` 在 `Working` 时为 true |
| `usage.chart` | `List` | `attrs`：`days`、`max` |
| `usage.chart.day[<YYYY-MM-DD>]` | `ListItem` | `attrs`：`total`，以及每个系列的 token（键 `claude-code`、`codex`、`other`） |
| `usage.totals` | `Group` | `attrs`：`total`、`input`、`output`、`cache-read`、`cache-write`、`cache-hit` |
| `usage.detail` | `List` | `attrs`：`mode`（`branch` / `mr`）；普通文件夹不出现 |
| `usage.detail.row[<use id 的十六进制> / mr-<编号> / unassigned]` | `ListItem` | `attrs`：`tokens`、`percent`、`review`、`unattributable` |
| `usage.models`、`usage.models.row[<agent>-<序号>]` | `List`、`ListItem` | `attrs`：`tokens`、`percent`；业务键不用模型 id |
| `usage.projects`、`usage.projects.row[<项目 ID 的十六进制>]` | `List`、`ListItem` | `attrs`：`tokens`、`percent`、`status`（`active` / `path-missing` / `temporary` / `confirmed-new` / `pending`） |
| `usage.projects.row[<id>].merge` / `.same` / `.new` / `.undo` | `Button` | 按 `RowAction` 出现 |
| `usage.projects.other` | `ListItem` | `attrs`：`tokens`、`percent` |
| `usage.projects.more` | `Button` | 行数超过 100 时 |
| `usage.merged`、`usage.merged.row[<id>]`、`usage.merged.row[<id>].undo` | `List`、`ListItem`、`Button` | 没有已合并的项目时不出现 |
| `usage.unavailable`、`usage.loading` | `Text` | |

Action 与快捷键（探针可按名字触发）：

| Action | 键 | 上下文 | 处理 |
|---|---|---|---|
| `usage::Back` | Esc、⌘[ | `Content > usage` | `Drilled` 时回到「所有项目」；否则 `cx.propagate()`，交给 `workspace`（Esc → `Pop`，⌘[ → `PopToBase`，[workspace 设计「界面」](workspace.md#界面)） |

菜单打开时 Esc 先关闭菜单（系统菜单自己处理）。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)与 [usage 设计「状态与异常」](usage.md#状态与异常)：

| 情况 | 表现 |
|---|---|
| `usage-view-data` 没有装、激活失败或停用 | `Body::Unavailable`：只显示 `usage-unavailable`；额度、图表、合计都不画；statusline 的接入行照常显示，开关可用。侧栏底部由 `project_home` 只画 Usage 标签 |
| 快照还没有（刚打开、刚切换） | 100 ms 内不显示加载态（切换时继续显示旧内容）；100 ms – 1 s 内容区（额度以外）显示 `Skeleton`；超过 1 s 显示 `usage-loading`。没有取消按钮：离开屏幕（Esc）即放下关注 |
| `Loading` | 同上 |
| `Backfilling { done, total }` | 已入库的部分照常显示；顶部横幅 `usage-backfilling`（`done * 100 / total`） |
| `Degraded(Recovered)` / `NewerDatabase` / `Unwritable` | 数据照常显示，顶部横幅对应的 `usage-degraded-*`；`Unwritable` 附带的原因文字不显示（`usage` 已记日志） |
| 窗口内没有任何用量 | 合计为 0；图表的柱全是 0 高；明细组显示 `usage-empty`；所有项目里只剩需要操作的行 |
| 没有额度数据 | 额度组第一行 `usage-limits-empty`；statusline 已开但还没有 Claude Code 的窗口时，接入行的次要文字是 `usage-statusline-waiting` |
| 额度过期 | 「额度的级别」表的最后一行 |
| 当前项目是临时项目 | 顶部横幅说明原因（`usage-temporary-*`），带「是同一个项目」「是新项目」（只对 `LocalRewrite`、`PrimaryRemoteChanged`） |
| 合并的目标在列表里找不到（又被合并了） | `usage-merged-into-unknown` |
| 命令发出后数据库写不进 | `usage` 不改快照，下一个快照到达时 `Pending` 清掉，行回到原状态；横幅显示 `Unwritable` |
| 设置文件不存在的目录（没有用过 Claude Code） | 不安装，不显示接入行 |
| 设置文件不是 JSON、`statusLine` 的形状认不出 | `Unsupported`：不修改，接入行显示 `usage-statusline-unsupported` |
| 别的程序改了 `statusLine` | `Replaced`：不重装、不恢复，接入行显示 `usage-statusline-replaced` 与「开启」 |
| `platform` 调用或策略文件读写失败 | `Failed`：接入行显示 `usage-statusline-failed` 与原来的按钮，可以重试 |
| 策略文件坏了 | 按关闭处理（「statusline 的策略文件」），接入行显示 `usage-statusline-off` 与「开启」 |
| 项目名、分支名不是有效 UTF-8 | `usage` 已替换无效字节，原样显示 |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `usage_view`，不启用 `sink`。

- 默认记录：
  - info `statusline installed`：`chained`（bool）、`first`（是否首次生效）。info `statusline restored`。info `statusline updated`（`current` 为 false 时的更新）。
  - info `statusline left alone`：`reason`（`replaced` / `unsupported-json` / `unsupported-shape` / `too-large` / `no-config`），每次启动至多一条。
  - warn `statusline step failed`：`step`（`status` / `enable` / `disable`）、`io`（`Value::io`）。
  - warn `usage policy unreadable`：`reason`（`io` / `json` / `version`）、`io`（只在 `io` 时）。warn `usage policy not written`：`step`（`create_dir` / `create` / `write` / `rename`）、`io`。
  - info `usage command sent`：`kind`（`merge` / `confirm-same` / `confirm-new` / `undo`）。`usage` 另记命令的结果和项目 ID（usage 设计「日志与诊断」）。
  - warn `usage screen target`：`target`（收到的 `Target` 变体名），`build` 收到不认识的 `Target` 时。
- 详细级别：debug `usage page query`（`scope`：`current` / `all` / `drilled`，`window`：`7d` / `14d` / `30d`）；debug `usage page presented`（`generation`、`rows`、`took_us`）；debug `statusline status`（`state`：与语义节点的 `state` 相同的取值）；debug `usage hint shown`。
- 关联 ID：`plugin`（`usage-view` 或 `usage-view-data`）、`act` 由 kernel 在监听和 `started` 外设置，`task` 由 executor 设置；视图代码在 `workspace` 调用工厂时恢复的作用域里运行，Action 处理函数只带 target。不设 `session`、`review`、`req`、`analysis`：本 crate 不处理单个会话。
- 不记录：项目名与路径、分支名、MR 标题与编号、模型 id、token 数（用 `rows` 计数代替）、`UsageProjectId`（`usage` 已记）、`Unwritable` 的原因文字、Claude Code 设置文件的路径与内容、原来的 `statusLine` 命令（可能含令牌；本 crate 也拿不到它，只有 `platform` 读）、statusline JSON、策略文件的路径。
- 金丝雀测试：`canary_stays_out_of_usage_view_logs`（`src/tests/logs.rs`）：`lectern_logging::testing::capture()` 在 trace 级别，假 `Usage` 的快照里项目名、分支名、MR 标题、模型 id 都带金丝雀，`Unwritable` 的原因带金丝雀，假 `StatuslineHost` 返回的 io 错误文字之外不带内容；跑完打开屏幕、切换范围与窗口、点进项目、发四种命令、开启与关闭、写到不可写目录触发 `usage policy not written`，`assert_absent(金丝雀)`。

## 测试

全部在 macOS 上运行（`just test -p lectern-usage-view`）：本 crate 依赖 GPUI，Linux 上的 `just` 命令不覆盖它。`present` 和 statusline 的协调不开窗口，也是普通的 `#[test]`。

- 不依赖真实的 `usage`、Claude Code、设置文件和网络。`usage`、`workspace` 的服务用本 crate `testing` 模块里的假实现（`FakeUsage`：直接构造 `UsageSnapshot`、`Limits` 交出，记下收到的 `watch`、`command`；`begin_analysis`、`meter`、`register_mr_source` 一律 `unreachable!`。`FakeContributions`：记下登记和一次性提示）。不借用 `usage` 等插件 crate 的 `test-support`。
- statusline 用假的 `StatuslineHost`（按脚本返回状态与错误，记下调用顺序），策略文件写在 `tempfile` 的临时目录里。测试不读真实的 `HOME`、`~/.claude` 和 `CLAUDE_CONFIG_DIR`。本 crate 不调用外部命令行，不需要 PATH 上的假命令；设置文件的修改与脚本的测试在 `platform`（前置条件 B）。
- 快照里的模型 id 只用合成值（`model-a`、`model-b`），项目名与分支名也是合成的。
- `present_*`（insta 快照，`PageModel` 渲染成文本）：`present_project_branches`、`present_project_mrs`（`mr_mode` 与文字切换）、`present_folder_has_no_detail`、`present_models_unknown_id`、`present_all_projects`（零用量的 `Active` 行隐藏、`PathMissing` 与临时项目保留、「其他」、超过 100 行折叠）、`present_merged_and_confirmed`、`present_percent_sums_to_100`（含余数相同、合计为 0）、`present_cache_hit_rate`、`present_chart_series`（缺一天的 agent、其他 agent 灰色）、`present_limits_order_and_labels`、`present_limit_levels`（79.9 / 80 / 99.9 / 100 / 120 与 `expired`）、`present_resets_in`（分钟、小时、天的边界，已过 `resets_at`）、`present_notices`（四种 `UsageState`、`may_include_inherited`、临时的当前项目）。
- `statusline_*`：「statusline 的协调」表每一格一个用例；策略文件不存在、坏了、版本更高；关闭时策略写入失败不调 `disable`；开启与关闭排队时后到的替换排队的；首次生效时一次性提示只发一次、横幅出现，「好」之后不再出现；`Replaced` 不重装。
- `plugin_*`（`TestKernel`）：两个插件的 Manifest；`activate` 不做 IO、不写 `Shared`（策略目录的访问计数为 0）；`usage-view-data` 不在时屏幕是 `Unavailable`、statusline 照常；屏幕先打开、`usage-view-data` 后激活时取到关注；`usage-view-data` 停用时关注放下；`UsageEvent::Changed` 只唤醒同查询的屏幕，`Limits` 唤醒全部。
- 视图（`#[gpui::test]`）：`view_window_switch_rewatches`（`FakeUsage` 记下新查询、旧关注 drop、`Shared::window` 记住）；`view_scope_switch_and_drill`（点项目行进入 `Drilled`，`usage::Back` 回到所有项目，非 `Drilled` 时 Esc 冒泡）；`view_merge_confirms_then_commands`（菜单选候选、`simulate_prompt_answer` 选「合并」后才有 `UsageCommand::Merge`，选「取消」没有；行变 `Pending`，下一个快照清掉）；`view_confirm_same_and_new`、`view_undo`；`view_statusline_buttons`；`view_loading_phases`（假时间 50 ms / 300 ms / 1.5 s）；`view_refreshes_every_minute`；`semantic_tree_*`（项目范围、所有项目、`Unavailable` 三份语义树的 insta 快照）。
- `canary_stays_out_of_usage_view_logs`（「日志与诊断」）。

## 对其他 crate 的要求

以下改动各由对应 crate 的 PR 完成，写 `Refs #32`，在「验收」里列为前置条件。

- **A. `services`（`usage` 模块）与 `usage`**：三处接口补充，都是新增，不改已有字段的含义：
  1. `ProjectInfo` 加 `confirmed: Option<AliasId>`：用户确认「是新项目」（`UsageCommand::ConfirmNew`）之后为 Some，撤销后为 None。「是新项目」之后项目是 `Active`，没有这个字段就拿不到撤销要的别名（交互规范「屏幕」05：两个确认操作「同样可以撤销」）。
  2. `AllUsage` 加 `merged: Arc<[ProjectInfo]>`：经用户命令（`Merge`、`ConfirmSame`）并入别的项目、还没有撤销的项目，状态都是 `Merged { into, alias }`，按合并时间从新到旧。它们不在 `projects` 里、不带 token（用量已计入 `into`），`projects` 加 `other` 等于 `total` 的不变量不变（usage 设计「接口」的「并入别的项目的不单列」照旧成立）。自动的别名（移动、改写、`ForgeIdPending` 的自动并入）不列出。没有这个字段，「合并过的行显示已合并并可以撤销」做不出来。
  3. 额度的级别，供本 crate 与 `project_home` 共用：

     ```rust
     pub enum LimitLevel { Normal, Warning, Critical }
     impl LimitWindow {
         /// `expired` 为 true 时 Normal；否则 used_percent < 80 为 Normal，< 100 为 Warning，其余 Critical。纯函数。
         pub fn level(&self) -> LimitLevel;
     }
     ```

  另外，本 crate 依赖 `AllUsage::projects` 列出每个已打开过的项目，窗口内为 0 的也列出（第 1 步的实现已如此），「合并到…」的候选从这里取。测试：JSON 往返覆盖新字段；`identity_*` 加确认与撤销后 `confirmed`、`merged` 的变化；`limit_level_thresholds`。1、2 随 usage 第 3 步（项目标识）实现（已实现，见 usage 设计「与设计的差异」第 3 步），3 可以先做。
- **B. `platform`**：新模块 `claude_statusline`，在 platform 设计里另加一节（现在列在「本文不覆盖」里）。本 crate 只用下面三个函数；文件格式、脚本内容、记录的位置由 platform 设计决定，但必须满足其后的约束。

  ```rust
  pub mod claude_statusline {
      pub struct Paths {
          /// Claude Code 的用户设置文件（`UsageViewDeps::claude_settings`）。
          pub settings: PathBuf,
          /// Lectern 的脚本与安装记录所在目录：`<data_dir>/statusline/`。只有本模块写。
          pub own: PathBuf,
          /// 脚本写 JSON 的位置（`UsageViewDeps::statusline_sample`）。
          pub sample: PathBuf,
      }
      pub enum Status {
          /// 设置文件所在的目录不存在。
          NoConfig,
          /// 配置里没有 Lectern 的命令。`existing`：用户有自己的 statusline。
          Off { existing: bool },
          /// 配置里是 Lectern 的命令。`chained`：原命令照常运行。`current`：命令与脚本是这个版本写的，为 false 时 `enable` 更新它们。
          On { chained: bool, current: bool },
          /// 有安装记录，但配置里的 statusLine 已不是 Lectern 写的值。
          Replaced,
          Unsupported(Unsupported),
      }
      pub enum Unsupported { NotJson, UnknownShape, TooLarge }
      /// 只读。阻塞（读几个小文件），只在 executor 的 Background 任务里调用。
      pub fn status(paths: &Paths) -> io::Result<Status>;
      /// 安装或更新，返回之后的状态。NoConfig、Unsupported 时不写任何文件。阻塞。
      pub fn enable(paths: &Paths) -> io::Result<Status>;
      /// 把 statusLine 恢复成安装前的值（安装前没有就删掉这个键），删除脚本与记录。不是 On 时不写设置文件。阻塞。
      pub fn disable(paths: &Paths) -> io::Result<Status>;
  }
  ```

  约束：
  1. 只改 `statusLine`：其余键、值和顺序不变；原 `statusLine` 对象里 `command` 以外的字段保留。文件不是 JSON 对象时不写（`NotJson`），`statusLine` 不是命令类型时不写（`UnknownShape`）。
  2. 不丢失别人的写入：写同目录的临时文件后 `rename`，保留原文件的权限；设置文件是符号链接时写到它最终指向的文件所在的目录，链接本身不变；`rename` 之前核对原文件自读取之后没有变，变了就重读重算（有次数上限，超过时返回错误、不写）。
  3. 原 `statusLine` 的值存进 `own` 下的安装记录，`disable` 原样写回。目录存在而设置文件不存在时，`enable` 新建只含 `statusLine` 的文件；`disable` 时这个文件除了 `statusLine` 没有别的内容就删掉它，有别的内容就只删这个键。不复制设置文件的其他内容（其中可能有环境变量和凭据）。`Replaced` 时安装记录里的原值已经过时：`enable` 先把设置文件里现在的 `statusLine`（用户或别的程序改成的值）存为新的原值，替换旧记录，再接入它；`disable` 不改设置文件，只删掉过时的记录和脚本。所以「原值」总是 Lectern 写入之前设置文件里的那个值，不会把用户后来改的值丢掉，也不会恢复一个用户已经换掉的值。`own` 目录 0700，安装记录与 `sample` 0600。
  4. 写进配置的命令在 Lectern 的脚本不存在时（Lectern 被删除）照常运行原命令，没有原命令时什么也不输出，所以删掉 Lectern 不会弄坏用户的 statusline。命令和脚本不依赖 Lectern 进程在运行，只用 `/bin/sh`，不引入 Node 等运行时（架构规则 3）。
  5. 脚本：把 stdin 原样写到 `sample`（同目录临时文件后 `rename`，超过 1 MiB 不写，usage 设计前置条件 C）；有原命令时把同样的字节交给它，原样传出它的 stdout 和退出码；没有原命令时输出为空。写 `sample` 失败不影响原命令的输出。不联网、不等待别的进程。
  6. 测试在临时目录里用假的设置文件，不读真实的 `~/.claude`；脚本的测试用 `/bin/sh` 运行，原命令是放在 PATH 最前面的假命令，记录收到的 stdin。
  7. 设置文件的位置、`statusLine` 的格式和空输出时 Claude Code 的表现按 Claude Code 的公开文档核对，写进 platform 设计（本文没有核对，见「已知限制」）。
- **C. `ui`**：
  1. `Progress` 加颜色：`pub enum ProgressTone { Accent, Warning, Critical }`、`impl Progress { pub fn tone(self, tone: ProgressTone) -> Progress; }`。`Warning` 用系统橙、`Critical` 用系统红，来自 `Palette`；探针的 `attrs` 加 `tone`。
  2. 菜单：[ui 设计「第二批控件」](ui.md#第二批控件283)的 `show_menu` 与 `Row::context_menu`（系统菜单）。按钮弹出（pull-down）和右键弹出共用同一组 `MenuItem`；选中经带项目 ID 的 `Action` 派发（如 `usage::MergeInto { project, into }`，在本文的 Action 表里随实现补上），没有选中回调；菜单项不是探针节点，探针按名字触发这个 `Action`；标题项用一个禁用的项。
- **D. `workspace`**：让插件在窗口里显示一次性提示（`ui::Hint`，文字是调用方的 Fluent 键）。例如 `Contributions::show_hint(payload: Payload /* ui::contrib 的提示载荷 */, token: Option<ActivationToken>)`，任何线程调用，排到主线程显示，与 `workspace-command-failed` 用同一个位置；具体形状由 workspace 设计决定。本 crate 用它满足 ADR 0014 的「首次生效时提示一次」（「未决问题」1）。其余用到的接口都已存在：`ContributionKind::Screen`、`SidebarEntry { section: Bottom }`、`KeyBindings`、`screens::USAGE`、`Target::{None, Project}`、`WorkspaceCommand::Push`。
- **E. `i18n`**：新增 `locales/en/usage.ftl`、`locales/zh-CN/usage.ftl`，内容是「界面」的键表。
- **F. `app`**：`Profile::Desktop` 加两行：`lectern_usage_view::plugins(UsageViewDeps { executor, data_dir: app_data.join("usage_view"), claude_settings, statusline_sample: app_data.join("usage/statusline/claude-code.json") })` 的两个插件，在 `workspace`、`usage` 之后逐个放进 `PluginSet::with`。`claude_settings`：`CLAUDE_CONFIG_DIR` 有值时取逗号分隔的第一项，否则 `home/.claude`，加 `settings.json`；不检查目录是否存在（这是 IO，由 `platform::claude_statusline::status` 在后台判断）。启动测试的临时 `HOME` 已去掉 `CLAUDE_CONFIG_DIR`（app 设计「测试」），所以测试不会碰真实的设置文件。
- **G. `project_home`**：侧栏底部的额度条按 `LimitWindow::level()` 设 `Progress::tone`（ADR 0014「快到上限时变色」）。只改颜色，文字不变。
- **H. `bench`**：`usage_view/present_heavy`、`usage_view/scroll_all_100` 两个条目（「性能」），随本 crate 的对应步骤加进 `budgets.rs`。

## 未决问题

没有需要产品决定的问题。下面三条已于 2026-10-25 决定，人确认按默认选项：

1. **首次接入的提示放在哪里。** 已决定：选 (a)。ADR 0014 写「首次生效时提示一次」，没有写位置。安装时在窗口里显示一次性提示（前置条件 D），Usage 页顶部另有横幅，直到用户点「好」或「关闭」。D 完成之前不做 statusline 的自动安装（第 3 步的前置条件）。不选：(b) 只在 Usage 页显示横幅；(c) 安装之前先问（与 ADR 0014 的「默认开启」冲突）。
2. **关闭开关放在哪里。** 已决定：选 (a)。ADR 0014 写「设置里可关闭」，现在没有设置页。先放在 Usage 页额度组的 Claude Code 接入行；设置页做出来时设置页也提供同一个开关，两处读写同一份策略。不选：(b) 等设置页，在那之前不自动安装。
3. **额度提醒的阈值与形式。** 已决定：选 (a)。只在应用内：80% 起变橙并写「接近上限」，100% 起变红并写「已达上限」。系统橙与 Claude Code 的 agent 圆点颜色相近；卡片和侧栏都另有文字，所以不只靠颜色区分。不选：(b) 加系统通知；(c) 阈值改为 90%。

技术问题都已在上文决定。

## 已知限制

- Claude Code 的额度只在 Claude Code 运行、刷新 statusline 时更新；它不运行时卡片显示最后一个样本和「x 分钟前更新」，过了重置时刻显示「已重置，等待新数据」。
- 本 crate 只改用户级的设置文件。项目级的设置（`.claude/settings.json`、`.claude/settings.local.json`）或托管的设置覆盖了 `statusLine` 时，在那些项目里拿不到额度。
- 从 Dock 启动的应用读不到只在 shell 里设置的 `CLAUDE_CONFIG_DIR`，与 agents 设计「未决问题」1 相同；这时改的是 `~/.claude/settings.json`。
- 已经在运行的 Claude Code 会话是否立即读到新的 statusline 配置没有核对；最晚在新会话里生效。
- 设置文件的「核对后 rename」之间仍有极短的窗口，别的程序恰好在这时写入，它的写入会被覆盖。
- 「是新项目」的撤销一直可用，没有期限；已合并的项目一直列在「已合并的项目」里。两者都很少见。
- 所有项目超过 100 行时折叠，点「再显示」后不虚拟化。
- 柱状图只标第一天和今天，其余日期靠悬停查看。

## 验收

前置条件（其他 crate 的 PR，写 `Refs #32`）：

- A. `services` 与 `usage`：`rg -n 'pub confirmed: Option<AliasId>|pub merged: Arc<\[ProjectInfo\]>|pub fn level' crates/services/src/usage` 各有一处；`limit_level_thresholds` 与新字段的 JSON 往返通过（第 1 步之前）；`confirmed`、`merged` 的取值随 usage 第 3 步实现（第 2 步之前）。
- B. `platform`：`claude_statusline` 的三个函数；platform 设计加对应一节；它的测试在临时目录里覆盖「对其他 crate 的要求」B 的七条约束（第 3 步之前）。
- C. `ui`：`Progress::tone`（第 1 步之前）、`show_menu` 与 `Row::context_menu`（第 2 步之前，[ui 设计「第二批控件」](ui.md#第二批控件283)）；各自的语义树快照通过。
- D. `workspace`：一次性提示的接口（第 3 步之前）。
- E. `i18n`：`usage.ftl` 两种语言（第 1 步之前）。
- F. `app`：装配两个插件（第 1 步之后；第 3 步之前 `claude_settings` 可以先传，statusline 任务在第 3 步才有）。
- G. `project_home`：额度条的颜色（A、C 之后）。

#32 的视图部分分三步，每步一个 PR；前两步写 `Refs #32`，第 3 步写 `Closes #32`（其余 crate 的 PR 都写 `Refs #32`，[AGENTS.md「工作方式」](../../AGENTS.md#工作方式)）。每一步都要 `just check`、`just test -p lectern-usage-view` 在 macOS 上通过。

1. **只读的页面**（前置 A 的 3、C 的 1、E）：两个插件、`Shared`、`present`、屏幕与工具栏、额度卡片、柱状图、合计、当前项目与所有项目的显示、点进项目、加载态与异常态；`Cargo.toml` 加 `plugin = true`。
   - `present_*`、`plugin_*`、`view_window_switch_rewatches`、`view_scope_switch_and_drill`、`view_loading_phases`、`view_refreshes_every_minute`、`semantic_tree_*`、`canary_stays_out_of_usage_view_logs` 通过，快照已逐个看过。
   - `usage_view/present_heavy` 在预算内；`plugin/usage-view/activate`、`plugin/usage-view-data/activate` < 1 ms。
   - `python3 scripts/check_architecture.py` 通过：本 crate 不依赖其他插件 crate。
   - `rg 'std::fs|std::process|std::thread::spawn' crates/usage_view/src --glob '!**/statusline/policy.rs'` 没有结果。
   - `rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|\.font_family\(|gpui_component::' crates/usage_view/src` 没有结果；`rg 'Duration::from_secs' crates/usage_view/src` 只有每分钟重算的一处（`src/page.rs`）。
   - `rg '"[A-Z][a-z]+ [a-z]' crates/usage_view/src --glob '!**/tests*' --glob '!**/testing*'` 没有界面字符串字面量；`rg 'eprintln!|println!|log::' crates/usage_view/src` 没有结果。
2. **项目操作**（前置 A 的 1、2，C 的 2）：「合并到…」的按钮与右键菜单、确认框、「是同一个项目」「是新项目」、撤销、`Pending`、已合并组。
   - `view_merge_confirms_then_commands`、`view_confirm_same_and_new`、`view_undo` 与更新后的 `present_merged_and_confirmed`、`semantic_tree_*` 通过。
   - `usage_view/scroll_all_100` 在预算内（随 bench 的帧记录接入）。
3. **statusline 接入**（前置 B、D、F）：`StatuslineHost` 的 `platform` 实现、策略文件、启动任务、接入行、横幅、一次性提示、开启与关闭。
   - `statusline_*`、`view_statusline_buttons` 通过。
   - `rg 'std::fs' crates/usage_view/src/statusline/policy.rs` 的调用只出现在由 `Background` 任务调用的函数里（审阅时核对）；`rg 'claude_statusline::' crates/usage_view/src` 只出现在 `src/statusline/host.rs`。
   - 手动在 macOS 上运行（临时 `HOME`，其中有一个合成的 `.claude/settings.json`）：首次启动后设置文件的 `statusLine` 换成 Lectern 的命令，其余内容不变，窗口里出现一次性提示；用 `/bin/sh` 跑写进去的命令并从 stdin 给一份合成的 statusline JSON，`<app_data>/usage/statusline/claude-code.json` 出现；Usage 页点「关闭」后设置文件恢复原样（`diff` 没有输出）；再次启动不重装。结果写进「实际验证」。
