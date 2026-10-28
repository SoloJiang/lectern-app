# welcome 设计

状态：实现中。「验收」第 1 步（插件、模型、最近项目的存储、发现列表的归并，不依赖 GPUI 的部分，[#240](https://github.com/SoloJiang/lectern-app/pull/240)）和第 2 步（视图、Action、语言选择、语义树，`Closes #29`）已实现，见「实际验证」「与设计的差异」；前置条件 A（[#237](https://github.com/SoloJiang/lectern-app/pull/237)）、B（[#243](https://github.com/SoloJiang/lectern-app/pull/243)）已合并，C（`app` 装配，[app 设计「welcome 的装配」](app.md#welcome-的装配)）在 `app` 的 PR 里实现，该 PR 合并后第 2 步的「验收」项除手动运行外都已满足。「界面」已为 [#283](https://github.com/SoloJiang/lectern-app/issues/283) 按设计画布修订，并且已按修订实现（`Refs #283`）：两栏、应用图标、操作卡片、语言弹出菜单、平铺的侧栏行、⌫ 与右键菜单；见「实际验证」的「#283 的重绘」和「与设计的差异」。「未决问题」1、2 已由产品负责人在 2026-10-29 按设计画布决定（最近项目行显示待审数；显示 Clone 卡片，克隆流程没有实现之前卡片禁用），设计已改，尚未实现（「验收」第 4 步）。

对应 issue [#29](https://github.com/SoloJiang/lectern-app/issues/29)。`welcome` 是没有打开项目时的屏幕（[交互规范「屏幕」](interaction.md#屏幕)画板 01，[ADR 0012](../adr/0012.md)）：左侧是应用图标、应用名、Open Folder（⌘O）和语言选择，右侧是最近项目，以及从 Claude Code 与 Codex 的会话历史里发现、还没有打开过的文件夹。它作为一个屏幕贡献给 [workspace](workspace.md)（`Screen(WELCOME)`），由 `workspace` 在没有项目时显示；打开项目、读会话目录、画窗口容器都不在这里。

## 范围

- 做：
  - `Screen(WELCOME)` 的视图：应用图标、标题、Open Folder 操作卡片、语言选择、最近项目列表、发现的文件夹列表、打开失败的提示。
  - 最近项目列表：存在本 crate 自己的文件里（`platform::dirs::app_data()` 下的 `welcome/recent.json`），首帧之后在 executor 的后台任务里读写；项目打开成功时更新；用户可以移除一条。
  - 发现的文件夹：从 `dyn SessionSource::sessions()` 的会话列表按 `Place` 归并出还没有打开过的项目根目录，带 agent、会话数、运行中数和最近活动时间；`SessionsChanged`、`ProjectEvent` 和最近项目变化时重算。
  - 打开：最近项目、发现的文件夹、Open Folder 都经 `Contributions::dispatch` 交给 `workspace`，本 crate 只监听 `ProjectEvent::Opened` 的结果。
  - 语言选择：跟随系统 / English / 简体中文，选择立即生效，并经 `platform` 写入应用的语言偏好。
  - 最近项目行的待审数（[#283](https://github.com/SoloJiang/lectern-app/issues/283) 的画布决定）：项目打开期间从 `Review::overview` 记下需要审阅的会话，存进 `recent.json`；Welcome 显示这些会话加上之后又有活动的会话的个数（「待审数」）。不为没有打开的项目做 git 操作。
  - Clone Repository… 卡片：按画布画在 Open Folder 下面，禁用，说明行写「即将推出」。
  - 键盘：↑ ↓ 选择、↵ 打开、⌫ 移除最近项目。
  - 两个插件：`welcome`（注册 `Screen` 和 `KeyBindings` 贡献，维护最近项目，持有快照）和 `welcome-discovery`（依赖 `dyn SessionSource`，算发现列表）。分成两个是因为 kernel 没有可选依赖（[kernel 设计「依赖与自动启停」](kernel.md#依赖与自动启停)、[usage 设计](usage.md)的同一做法）：会话来源的插件没有装或失败时，Welcome 屏幕照常存在，只是发现列表为空。
- 不做：
  - 系统打开面板与 `Projects::open` 的调用：`workspace`（`PromptOpenProject`、`OpenProject`，[workspace 设计「接口」](workspace.md#接口)）。本 crate 不调用 `Projects::open`。
  - 窗口、工具栏、区域容器：`workspace`。Welcome 的 `ScreenLayout` 全部关闭（没有侧栏、工具栏、检查器、浮条）。
  - 会话目录的扫描、归位：`agents`（[agents 设计「数据流与线程」](agents.md#数据流与线程)）。本 crate 不读会话目录，只读 `sessions()` 的快照。
  - 路径解析成项目、worktree：`project`。本 crate 只用 `Place` 里已经算好的结果。
  - 上次打开的项目的恢复：`app` 读 `window.json` 后调 `WorkspaceHost::restore`（[workspace 设计「窗口状态」](workspace.md#窗口状态)）。最近项目列表与它是两份数据：`WindowState::project` 只有一条、决定启动时显示什么，最近项目列表有多条、只供用户选择。
  - 颜色、字体、控件、动效原语：`ui`（`ui` 设计，[#214](https://github.com/SoloJiang/lectern-app/issues/214) 的 PR [#218](https://github.com/SoloJiang/lectern-app/pull/218)；[ADR 0015](../adr/0015.md)）。
  - 语言偏好的读写实现：`platform`（见「对其他 crate 的要求」）。设置页、切换语言后重建应用菜单：[i18n 设计「数据流与线程」](i18n.md#数据流与线程)的语言设置 issue。
- 本 issue 之外：
  - Clone 的流程：需要 `git clone` 的进度、目标目录选择和 forge 登录，另开 issue；那个 issue 让本屏幕的 Clone 卡片可用（见「未决问题」2）。
  - 为没有打开的项目实时计算审阅概览：`Review::overview` 只对关注中的已打开项目计算（[review 设计](review.md)）；本 crate 的待审数是上次打开时记下的结果加上之后的会话活动（「待审数」）。
  - 每个项目一个窗口时的 Welcome 窗口（[workspace 设计「未决问题」](workspace.md#未决问题)1）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui` | 视图、`FocusHandle`、Action、`cx.spawn` 的前台任务 | 已锁定（[ADR 0016](../adr/0016.md)） |
| `lectern-ui` | `Text`、`strings`、`set_locale`、`colors`、`TextRole`、操作卡片、弹出菜单、侧栏列表的行、`AgentDot`、`Icon`、`semantic`、`ui::contrib` 的载荷类型 | 视图层的公共库，非插件；颜色和动效只从这里取 |
| `lectern-i18n` | `keys::welcome::*`、`keys::common::*`、`Locale`、`Args` | [ADR 0013](../adr/0013.md) |
| `lectern-kernel`、`lectern-services` | `Plugin`、`PluginHandle`、`ServiceRef<dyn Contributions>`、`ServiceRef<dyn Projects>`、`ServiceSet<dyn SessionSource>`、`ServiceRef<dyn Review>`、`ReviewEvent`、`SessionOverview::needs_review`；`WorkspaceCommand`、`ProjectEvent`、`SessionsChanged`、`Place`、`ProjectKey` | 架构规则 7（[ADR 0025](../adr/0025.md)）：插件只经 `services` 的接口往来 |
| `lectern-executor` | 读写最近项目文件、重算发现列表的 `Background` 任务 | UI 线程不做 IO；实例由 `app` 创建，经 `WelcomeDeps` 传入 |
| `lectern-platform` | `system::language_override`、`system::set_language_override`（前置条件 B） | macOS 专属能力只在 `platform`（架构规则 6） |
| `lectern-logging` | 「日志与诊断」的宏；测试用 `test-support` 的 `testing::capture()` | [ADR 0046](../adr/0046.md) |
| `serde`、`serde_json` | `recent.json` 的序列化 | 已在工作区依赖里 |
| `futures`（`channel` feature） | 唤醒视图的 `mpsc::unbounded`（见「数据流与线程」） | `workspace` 已把它加进 `[workspace.dependencies]`，不新增 crate |
| `lectern-kernel`（`test-support`，dev）、`insta`（dev） | `TestKernel`、语义树快照 | |

本 crate 是插件 crate（`[package.metadata.lectern]` 加 `plugin = true`，[ADR 0025](../adr/0025.md)），含三个插件，都不提供服务，都是 `AfterFirstFrame`：`welcome` 依赖 `dyn Contributions`、`dyn Projects`；`welcome-discovery` 依赖 `dyn Projects`、`dyn SessionSource`；`welcome-review` 依赖 `dyn Review`（#283 的待审数；分成单独的插件，理由与 `welcome-discovery` 相同：`review` 没有装或失败时 Welcome 照常存在，行上只是没有待审数）。三者共用一个 `Arc<WelcomeModel>`（`app` 用 `plugins(deps)` 一次构造）。本 crate 不依赖 `workspace`、`project`、`agents` 这些插件 crate；只有 `app` 和 tool 层依赖它。

## 接口

### 本 crate 的公开项

```rust
pub struct WelcomeDeps {
    /// 由 `app` 创建并传入。
    pub executor: lectern_executor::Executor,
    /// 最近项目文件所在目录：`app` 取 `platform::dirs::app_data()` 下的 `welcome/`
    /// （`Resources::app_data` 没有值时 `app` 不装配任何插件，[app 设计「装配」](app.md#装配)）。
    pub data_dir: PathBuf,
    /// 应用版本的「主版本.次版本」，副标题 `welcome-subtitle` 的 `$version`；`app` 从包版本截取。
    /// None 时副标题用 `welcome-subtitle-plain`，其余布局不变。
    pub version: Option<SharedString>,
}

/// 三个插件：ID "welcome"、"welcome-discovery" 与 "welcome-review"，共用一个 `Arc<WelcomeModel>`。`app` 逐个放进 `PluginSet::with`。
/// 只保存依赖、建空的 `WelcomeModel`，不做 IO。
pub fn plugins(deps: WelcomeDeps) -> [(Name, WelcomePlugin); 3];
pub struct WelcomePlugin { /* 角色（Screen / Discovery）, deps, model: Arc<WelcomeModel> */ }

// "welcome"：Manifest { provides: [], requires: [service::<dyn Contributions>(), service::<dyn Projects>()], activation: [AfterFirstFrame] }
//   activate（kernel 线程）：取 ServiceRef<dyn Contributions>、ServiceRef<dyn Projects>；`ctx.on::<ProjectEvent>`；
//     `Contributions::register(Screen(WELCOME), None)`、`register(KeyBindings, None)`（`activate` 里没有 token）的守卫交给 `ctx.own_staged`。不做 IO。
//   started（kernel 线程）：`own_with` 提交读 `recent.json` 的 Background 任务（「数据流与线程」第 1 步）。
// "welcome-discovery"：Manifest { provides: [], requires: [service::<dyn Projects>(), service::<dyn SessionSource>()], activation: [AfterFirstFrame] }
//   activate：取 ServiceRef<dyn Projects>、ServiceSet<dyn SessionSource>；`ctx.on::<SessionsChanged>`、`ctx.on::<ProjectEvent>`。不做 IO，不改模型。
//   started：把模型的 `discovered` 置为 `Pending`（激活失败时模型不留下 `Pending`），`own_with` 提交第一次重算（第 3 步）。
//   `Active` drop（停用：会话来源的最后一个提供者停用、本插件失败）：`discovered` 置为 `Unavailable`，换快照并唤醒。
// "welcome-review"：Manifest { provides: [], requires: [service::<dyn Review>()], activation: [AfterFirstFrame] }
//   activate：取 ServiceRef<dyn Review>；`ctx.on::<ReviewEvent>`（只处理 `Overview`）。不做 IO，不持有 `watch_overview`：
//   概览由打开的项目主页关注，本插件只在它更新时读一次（「待审数」）。

/// 视图读取的不可变快照。每次变化整体替换为新的 `Arc`。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WelcomeSnapshot {
    /// 每次替换加一。
    pub generation: u64,
    pub recent: RecentState,
    pub discovered: DiscoveredState,
    /// 本屏幕发起、还没有收到 `Opened` 的打开：`begin_open` 设置，`Opened`（成功或失败）清除。
    /// 视图据此不重复发出同一路径的 `OpenProject`。
    pub opening: Option<Opening>,
    /// 显示在按钮下方的一行提示；下一次 `begin_open` 时清空。
    pub notice: Option<Notice>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Opening { pub path: PathBuf, pub source: OpenSource }
pub enum OpenSource { Recent, Discovered, Prompt }

pub enum Notice {
    /// `ProjectEvent::Opened { Err }` 的种类（`OpenError` 的判别，不带附加文字）。
    OpenFailed(OpenFailure),
    /// `platform::system::set_language_override` 的写入失败（[i18n 设计「状态与异常」](i18n.md#状态与异常)）。
    LanguageNotSaved,
}

/// `OpenError` 去掉附加文字后的种类，对应五个 `welcome-open-failed-*` 键。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpenFailure { NotFound, NotADirectory, PermissionDenied, RepoUnreadable, Io }

pub enum RecentState {
    /// 文件还没有读完（`started` 到第 1 步结束之间）。
    Loading,
    /// 按 `last_opened` 从新到旧，最多 10 条。
    Ready(Arc<[RecentEntry]>),
}

pub enum DiscoveredState {
    /// `welcome-discovery` 没有激活：模型的初始值，也是它停用（会话来源的最后一个提供者失败或停用）后的值。显示空状态，不显示骨架。
    /// 没有会话来源的 profile 必须同时不装 `welcome-discovery`（`PluginSet::validate` 拒绝没有提供者的 `requires`）。
    Unavailable,
    /// `welcome-discovery` 已激活、第一次重算还没完成。
    Pending,
    /// 按 `newest_activity` 从新到旧，最多 50 条。
    Ready(Arc<[DiscoveredFolder]>),
}

/// 快照的持有者。`Send + Sync`；两个插件的 `Active` 各持有一份 `Arc`，视图工厂持有一份。
pub struct WelcomeModel { /* Mutex<Arc<WelcomeSnapshot>>, Mutex<RecentList>, 唤醒发送端, 重算的脏标记与排队标记 */ }

impl WelcomeModel {
    /// 当前快照。取锁复制一个 `Arc`，不阻塞、不做 IO，渲染时在主线程上调用。
    pub fn snapshot(&self) -> Arc<WelcomeSnapshot>;

    /// 登记唤醒：快照替换后调用一次 `wake`。视图在建立时登记，返回的守卫 drop 时注销。任何线程。
    pub fn subscribe(&self, wake: Box<dyn Fn() + Send + Sync>) -> WakeGuard;

    /// 视图在 `dispatch(OpenProject(path))` 之前调用：清空 `notice`，记下 `opening`，换快照。
    /// Open Folder 按钮和 ⌘O 不经过它（路径由系统面板给出，`workspace` 持有），只清空 `notice`：`clear_notice()`。
    /// 主线程，只改内存，立即返回。
    pub fn begin_open(&self, path: PathBuf, source: OpenSource);
    pub fn clear_notice(&self);

    /// 移除一条最近项目：在调用线程上改内存里的列表并换快照，然后排一个 Background 任务写文件。主线程调用，立即返回。
    pub fn remove_recent(&self, path: &Path);

    /// 语言偏好写入失败时由视图传给 `platform` 的回调调用：`notice = LanguageNotSaved`，换快照并唤醒。任何线程。
    pub fn language_not_saved(&self);
}
```

- `plugins`、`activate`：不做 IO，不建线程。
- `WelcomeModel` 的全部方法：任何线程，不阻塞、不做 IO。
- 视图发出的命令只有：`Contributions::dispatch(WorkspaceCommand)`（打开）、`WelcomeModel::{begin_open, clear_notice, remove_recent}`（只改内存）、`lectern_ui::set_locale` 和 `platform::system::set_language_override`（语言）。视图不调用 `Projects`、不读文件。

### 视图（本 crate 内部）

```rust
/// `Screen(WELCOME)` 的 `ViewFactory`：每次 `workspace` 需要这个屏幕时建一个。`Target` 总是 `None`。
/// 主线程；建立时登记唤醒，取焦点，不做 IO。
struct WelcomeView {
    model: Arc<WelcomeModel>,
    contributions: ServiceRef<dyn Contributions>,
    focus: FocusHandle,
    /// 键盘选择：右侧两个列表合成一条线性序列（先最近项目，后发现的文件夹）。None 表示没有选中。
    selection: Option<Selection>,
    _wake: WakeGuard,
}

enum Selection { Recent(usize), Discovered(usize) }
```

`toolbar` 闭包返回空标题：Welcome 的 `ScreenLayout` 没有工具栏，`workspace` 不画它。

## 数据结构

### 最近项目

```rust
/// `welcome/recent.json` 的内容。认不出的 `version` 当作没有文件（记 warn，启动时为空列表，下次写入覆盖）。
#[derive(Serialize, Deserialize)]
struct RecentList {
    version: u32,            // = 1
    entries: Vec<RecentEntry>, // 从新到旧，最多 10 条
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RecentEntry {
    /// 再次打开时交给 `WorkspaceCommand::OpenProject` 的路径：当前 worktree 的根加上 `opened_at`
    /// （`ProjectSnapshot::active`），所以在子目录或 linked worktree 里打开的项目回到同一个位置。
    /// 没有当前 worktree（裸仓库）时是 `ProjectKey::Git` 的公共目录。规范路径。
    pub path: PathBuf,
    /// `ProjectSnapshot::key`（`services::project::ProjectKey`，派生 serde）。发现列表的过滤和行上的会话计数都按它比较：
    /// 同一仓库的其他 worktree 里的会话算在这一条上，不会再作为「尚未打开过」出现。
    pub key: ProjectKey,
    /// `ProjectSnapshot::name`。
    pub name: Arc<str>,
    /// UTC 毫秒。
    pub last_opened: i64,
    /// 上次打开失败（`OpenError::NotFound` / `NotADirectory`）后置为 true，只在内存里，不写进文件：
    /// 行上显示 `welcome-recent-missing`，再次打开成功后清除。
    #[serde(skip)]
    pub missing: bool,
    /// 行上显示的会话数和运行中数：`discover::build` 按 `key` 算出（「发现的文件夹」），随第 3 步换进快照；
    /// 只在内存里，不写进文件；`welcome-discovery` 没有激活时为 0。
    #[serde(skip)]
    pub sessions: u32,
    #[serde(skip)]
    pub running: u32,
    /// 上次打开时记下的待审会话（「待审数」）。旧文件没有这个字段，读成 None（`#[serde(default)]`），
    /// `version` 仍是 1：旧版本读新文件时忽略这个字段。
    #[serde(default)]
    pub review: Option<ReviewMark>,
    /// 行上显示的待审数：`discover::build` 由 `review` 和会话列表算出（「待审数」），只在内存里；
    /// `review` 为 None 或 `welcome-discovery` 没有激活时为 None，行上改显示会话数。
    #[serde(skip)]
    pub to_review: Option<u32>,
}

/// 一个项目最近一次概览里需要审阅的会话。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ReviewMark {
    /// `SessionOverview::needs_review()` 为 true 的会话，最多 50 个（概览本身最多 50 个会话）。
    pub sessions: Vec<SessionKey>,
    /// 记下的时刻，UTC 毫秒。
    pub at: i64,
}
```

不变量：

- `path` 不重复。项目打开成功时，同一路径的旧条目移到最前并更新 `last_opened`、`name`、`key`；超过 10 条时去掉最旧的。同一个 `key` 可以有多条（在不同 worktree 或子目录里打开的同一仓库），各自回到自己的位置。
- 启动时不 `stat` 任何条目（不做遍历）：路径是否还存在只在用户打开时才知道。
- 单一写入者：模型里的 `RecentList` 带 `loaded` 标记，第 1 步读完文件才置位。置位之前到达的改动（`Opened`、`remove_recent`）只记进一个待应用的变更队列，不改列表、不写文件；第 1 步读完后先按顺序应用它们，再写一次。写文件用与发现列表相同的「排队中 + 脏标记」：任何时刻最多一个写任务，它在运行时序列化模型里当时最新的列表，结束时发现脏标记就再写一次。所以不会出现空列表盖掉文件、两个任务同时写 `recent.json.tmp`、旧列表写在新列表之后这三种情况。
- 文件格式：JSON，一个对象；字段不认识时忽略；`path` 用 `PathBuf` 的 serde 表示（非 UTF-8 的路径写不进去，这样的条目不写，记 warn）。样本在 `crates/welcome/tests/fixtures/recent_v1.json`。

### 待审数

产品负责人 2026-10-29 按设计画布决定最近项目行显示待审数（「未决问题」1）。本 crate 不为没有打开的项目做 git 操作，待审数由两部分合成，都不需要 IO：

1. 记下（`welcome-review`）：收到 `ReviewEvent::Overview { project }` 时，在监听里排一个 `Background` 任务：读 `Review::overview(project)`（复制 `Arc`），取 `needs_review()` 为 true 的会话（规则与项目主页的 Needs Review 相同：`progress` 为 None、`seen < total` 或 `changed_since_seen > 0`；这个判断移到 `services` 的 `SessionOverview::needs_review`，两个 crate 共用，见「对其他 crate 的要求」F），写进 `key` 相同的每条最近项目的 `review`，`at` 是当前时刻。内容有变化（会话集合不同）时才置写文件的脏标记，所以概览每 2 秒重算一次时不会每次都写盘。`overview` 为 None（关注者已放下）时不改。
2. 合成（`discover::build`，第 3 步）：待审数 = `review.sessions` 里仍在会话列表中的会话，加上这个项目（按 `key`）里不在 `review.sessions` 中、`last_activity > review.at` 的会话（子 agent 的会话不计，同 `sessions` 计数）。前一部分是上次打开时还没审完的，后一部分是之后 agent 又改动过的。`review` 为 None 时 `to_review` 为 None。

这是一个估计：之后的活动可能没有改文件，或者用户在别处（终端里）审过了；项目打开后，项目主页给出准确的数。行上的数在下一次打开并收到概览后更正。

### 发现的文件夹

```rust
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DiscoveredFolder {
    /// 项目根：最新那个会话的 cwd 去掉 `Resolved::rel` 尾缀（纯字符串操作，不做 IO）。
    /// 文件夹项目是 `ProjectKey::Folder::root`。
    pub path: Arc<Path>,
    /// `path` 的最后一级。
    pub name: Arc<str>,
    pub key: ProjectKey,
    /// 这个项目里出现过的 agent，按首次出现的顺序，最多 4 种。
    pub agents: Arc<[Agent]>,
    pub sessions: u32,
    /// `open_turn && last_activity` 在 10 分钟之内（与 agents 设计「调用方」对活跃会话的判断相同）。
    pub running: u32,
    /// 全部会话里最大的 `last_activity`。
    pub newest_activity: Timestamp,
}
```

归并规则（`discover::build(lists, open_projects, recent_keys, now) -> Built`，纯函数；`lists` 是每个来源的 `Arc<SessionList>`，`recent_keys` 是最近项目的 `ProjectKey`，`Built` 带发现列表、按 `key` 的会话计数、归并前的会话总数和跳过的数量）：

- 只看 `Place::Resolved`（项目没有打开）；`Located`（已打开）、`Pending`、`Missing`、`Unresolvable` 跳过。
- 按 `Resolved::project` 分组：`ProjectKey` 在 `Projects::open_projects()` 里的不列（正在打开中）；`ProjectKey` 等于某条最近项目的 `key` 的不列（「尚未打开过」；任何 worktree 里的会话都按仓库算）。
- 最近项目行的 `sessions` / `running`：`Place::Resolved` 或 `Located` 的 `project` 等于该条 `key` 的会话，按同样的规则计数（`Located` 出现在同一个窗口里换项目时的旧项目）。
- 子 agent 会话（`parent` 为 Some）计入 `sessions`，不单独算 `running`。
- 按 `newest_activity` 从新到旧，取前 50 条。

### 运行中的判断

`running = open_turn && now - last_activity <= 10 min`。`now` 由重算任务传入（`SystemTime::now()` 取一次），所以快照里的数字是重算时刻的；列表不按时间自动刷新，下一次 `SessionsChanged` 才更新（活跃会话每次追加都发事件，所以运行中的会话几秒内就刷新；停下来的会话最长在 10 分钟后的下一次事件时消失，见「已知限制」）。

## 数据流与线程

```
kernel 线程：welcome.activate ─▶ register(Screen, KeyBindings) ─▶ commit ─▶ [主线程] workspace 应用 ─▶ 下一帧画出 Welcome（空列表）
kernel 线程：welcome.started ─▶ own_with ─▶ executor Background：
   (1) 读 recent.json ─▶ RecentState::Ready ─▶ 换快照 ─▶ wake ─▶ [主线程] cx.notify ─▶ 重绘
kernel 线程：welcome-discovery.activate（discovered = Pending）─▶ started ─▶ own_with ─▶ executor Background：
   (3) 读 sessions()、open_projects()、RecentList ─▶ discover::build ─▶ DiscoveredState::Ready ─▶ 换快照 ─▶ wake
executor 线程：SessionsChanged / ProjectEvent ─▶ 监听（welcome-discovery）─▶ 安排 (3)（合并：已有一个在排队时只置脏标记）
executor 线程：ProjectEvent::Opened ─▶ 监听（welcome）─▶ Ok：安排 (2)；Err：notice、missing、清 opening ─▶ 换快照 ─▶ wake
   (2) Projects::snapshot(key) ─▶ 更新 RecentList ─▶ 换快照 ─▶ wake ─▶ 写 recent.json（临时文件 + rename）─▶ 置脏标记 (3)
主线程：点击 / ↵ ─▶ WelcomeView ─▶ model.begin_open(path) ─▶ Contributions::dispatch(OpenProject(path)) ─▶ workspace 守卫 ─▶ Projects::open
主线程：⌫ ─▶ WelcomeModel::remove_recent(path) ─▶ 立即换快照 ─▶ 安排写文件
```

1. 读最近项目（`welcome` 的 `started` 提交，`Background`，持有 `busy()` 守卫）：读 `data_dir/recent.json`；文件不存在时 `Ready(空)`，不记日志；解析失败或版本不认识时记 warn、`Ready(空)`。文件通常只有几 KB，读与解析 < 1 ms。读完后置 `loaded`，应用读取期间排队的变更（「最近项目」的单一写入者规则），再对 `Projects::open_projects()` 里的每个项目按第 2 步记录一次：`restore` 的 `OpenProject` 可能在本插件的 token 变为 `Live` 之前就发出了 `Opened`（[kernel 设计「激活时的上下文」](kernel.md#激活时的上下文)：这段时间的事件收不到，「需要初始状态的插件在 `started` 里主动读取」），不补这一步，恢复的项目就不进最近项目。最后置第 3 步的脏标记：`welcome-discovery` 的第一次重算可能先于本步完成，那时最近项目会短暂出现在发现列表里，脏标记让它再算一次。`started` 同时把 `PluginHandle` 存进模型，供第 6 步提交写文件的任务。
2. 记录打开成功（`welcome` 对 `ProjectEvent::Opened { Ok(key) }` 的监听安排，`Background`）：`Projects::snapshot(key)` 在 `Opened` 时已有第 1 代（[project 设计「事件」](project.md#事件)），从它取 `path`、`key`、`name`；清掉 `opening`，更新内存里的 `RecentList`（`loaded` 还没置位时只进变更队列），先换快照、再置写入的脏标记：写任务（同一时刻最多一个）在 `data_dir` 下写 `recent.json.tmp`，`rename` 成 `recent.json`；`data_dir` 不存在时先 `create_dir_all`。写失败记 warn，内存里的列表照常使用。这一步与 `Opened` 来自 `restore`、⌘O 还是本屏幕无关：任何成功打开的项目都进入最近项目，所以恢复上次项目时它也在列表最前。最后置第 3 步的脏标记（`welcome-discovery` 没有激活时标记无人消费，无害）。
3. 重算发现列表（`welcome-discovery`，`Background`，持有 `busy()`）：对 `ServiceSet<dyn SessionSource>::for_each` 的每个来源取 `sessions()`（取锁复制 `Arc`），取 `open_projects()` 和模型里当前的 `RecentList`，调 `discover::build`，把发现列表换进快照，并把算出的计数按 `key` 并进模型里当时的 `RecentList`（不用任务开始时的那份副本替换列表，否则刚移除的条目会短暂回来）。触发：`started`、每次 `SessionsChanged`、`ProjectEvent::Opened` / `Closed`、最近项目变化（第 2、6 步置的脏标记）。合并：模型里有一个「排队中」标记，已经排了一个任务时后续触发只置脏标记；任务每一轮在取输入时清掉脏标记，结束时发现脏标记（说明取输入之后又有触发）就再跑一轮，所以任务开始之前到达的连续事件只跑一轮，运行中到达的再多跑一轮，事件不会堆积任务。
4. 唤醒视图：`WelcomeModel` 持有 `futures::channel::mpsc::unbounded` 的发送端（`subscribe` 登记）；视图建立时 `cx.spawn` 一个前台任务循环 `await` 接收端，每收到一项就 `this.update(cx, |_, cx| cx.notify())`。发送端的 `wake` 经 GPUI 的平台分发器唤醒主线程（与 [workspace 设计「数据流与线程」](workspace.md#数据流与线程)的主线程队列同一机制），任何线程都能发，不轮询。视图 drop 时守卫注销，之后的 `wake` 不做任何事。
5. 打开：最近项目和发现的文件夹：视图在主线程上先 `model.begin_open(path, source)`（清 `notice`、记 `opening`），再 `dispatch(WorkspaceCommand::OpenProject(path))`；`opening` 有值期间，视图对同一路径的再次 ↵ 或点击不发出命令，对其他路径照常（`workspace` 自己排队）。Open Folder 按钮：`model.clear_notice()` 后 `dispatch(PromptOpenProject)`（⌘O 是 `workspace` 的全局绑定，不经本视图，只有 `workspace` 的流程）。`workspace` 问守卫、调 `Projects::open`，`Opened { Ok }` 到达后换基底屏幕、drop 本视图，本 crate 的监听同时清 `opening`（第 2 步）。`Opened { Err(e) }` 时 `workspace` 停在 Welcome 并记 warn，本 crate 的监听把 `Notice::OpenFailed(e 的种类)` 放进快照、清 `opening`；`opening` 有值并且 `e` 是 `NotFound` / `NotADirectory` 时，`path` 等于 `opening.path` 的最近项目标 `missing`。`Opened` 不带路径（[project 设计「事件」](project.md#事件)只有 `OpenRequest`），所以本屏幕发起的打开同一时刻只能有一个在进行，`opening` 就是那一个；不是本屏幕发起的失败（`restore` 的路径已不存在、⌘O 选的目录不可读）只显示提示，不标 `missing`。
6. 移除最近项目：视图调 `model.remove_recent(path)`：在调用线程（主线程）上只改内存里的列表并换快照（常数时间，不做 IO；`loaded` 还没置位时只进变更队列），然后置写入的脏标记：没有写任务在排队时用 `started` 存下的 `PluginHandle::own_with` 排一个 `Background` 写任务（同第 2 步；`welcome` 已停用时 `own_with` 返回 false，不写，视图这时也已经被 `workspace` 撤下），再置第 3 步的脏标记（被遮住的发现项可能重新出现）。
7. 语言选择：视图在主线程上调 `lectern_ui::set_locale(locale, cx)`（立即换全局 `Strings`、刷新窗口），再调 `platform::system::set_language_override(tag, done)`（立即返回，写入在 `platform` 的串行队列里，[i18n 设计「数据流与线程」](i18n.md#数据流与线程)）；`done` 收到 `Err(WriteFailed)` 时调 `model.language_not_saved()`（`Superseded` 是被更新的选择顶掉，不提示），视图显示 `welcome-language-not-saved`（i18n 设计「状态与异常」要求的一次提示）。选「跟随系统」时 `locale = Locale::negotiate(platform::system::system_languages())`，`tag` 为 None。弹出菜单的初始选项来自 `platform::system::language_override()`（一次内存里的偏好读取，主线程，视图建立时读一次），有值时用 `Locale::negotiate([tag])` 转成 `Locale`。
8. UI 线程上不做 IO、解析和 git 操作：视图只调 `snapshot()`（取锁复制）、`dispatch`、`begin_open` / `clear_notice` / `remove_recent`（改内存）、`set_locale`、`language_override`（内存里的偏好）。所有 `std::fs` 调用都在 `src/recent/store.rs`，只从 `Background` 任务调用（验收用 `rg` 检查）。
9. 派生数据：没有以内容为键的派生数据（架构规则 5 不适用）。发现列表以会话列表的代数为准：任务开始时记下每个来源的 `generation`，结束时有更新的来源就按第 3 步的脏标记再跑一次。
10. `welcome-discovery` 停用（会话来源的最后一个提供者停用、本插件失败）：kernel 先撤销监听、关闭闸门（排队中的重算丢弃），最后 drop `Active`，它把 `discovered` 置为 `Unavailable`、换快照并唤醒；列表显示空状态。重新激活时回到 `Pending`。
11. 待审数（`welcome-review`，见「待审数」）：executor 线程上的 `ReviewEvent::Overview { project }` 监听排一个 `Background` 任务，读 `overview`、更新内存里 `key` 相同的条目的 `review`，集合变化时置写文件的脏标记（同第 2 步的写任务）和第 3 步的脏标记，由第 3 步把 `to_review` 换进快照。`welcome-review` 停用时已记下的标记保留，`to_review` 照常由第 3 步算出。

## 性能

- 冷启动（[ADR 0017](../adr/0017.md)）：首帧之前本 crate 没有代码运行。首帧是 `app` 的中性骨架；`workspace::install` 在屏障放开后替换根视图，本 crate 的插件在 kernel 线程上激活，`Screen` 贡献应用后的下一帧才画出 Welcome。`plugin/welcome/activate`、`plugin/welcome-discovery/activate` 各 < 1 ms（取服务引用、登记贡献或监听，不做 IO）。
- Welcome 的第一次渲染（屏障放开后的第 2–3 帧）：只有图标、标题、一张操作卡片、一个弹出菜单、两个小标题和两条空状态文字，从视图自己的状态画出，不等任何后台任务：排版十几段文字，< 1 ms，远在一帧 8.3 ms 之内。列表在后台任务完成后填入：最近项目在第 1 步后（读文件 < 1 ms，加任务调度，用户可感知为与首帧同时出现）；发现的文件夹在 `agents` 第一次发布会话列表之后（[agents 设计「性能」](agents.md#性能)：1000 个会话 < 500 ms）。按[交互规范「状态与异常」](interaction.md#状态与异常)，100 ms 内不显示加载态，所以两个列表在到达之前只显示标题、不显示骨架；超过 100 ms 还没到时显示 2 行骨架（`ui::Skeleton`），由 `ui::loading_phase` 判断。
- 重算发现列表：`discover::build` 对 1000 个会话约 0.3 ms，10 000 个约 3 ms（一次哈希分组加一次排序），在 `Background` 线程上，持有 `busy()` 守卫；发现阶段最多再跑一次（脏标记合并），不让 kernel 长时间忙碌。
- 空闲内存：快照最多 10 条最近项目加 50 条发现项（每条约 200 字节），模型和视图合计 < 64 KiB；本 crate 不保留 `SessionList` 的 `Arc`（任务结束即放下），不保留 `ProjectSnapshot`。
- 帧时间：`render` 只克隆一个 `Arc` 和构造至多 60 行的元素；悬停和选中用 `ui::SidebarRow` 的动画（`HOVER_IN` / `HOVER_OUT`），不自定义动画参数。
- 待审数（「待审数」）：`welcome-review` 不做 git 操作、不持有 `watch_overview`，只在已打开项目的概览更新时复制一个 `Arc` 并取出最多 50 个会话键（< 0.1 ms，`Background`）；写文件只在集合变化时发生。`discover::build` 多一次按 `key` 的会话计数，与已有的会话计数同一次遍历。`recent.json` 每条多最多 50 个会话键（约 4 KB），10 条约 40 KB，读与解析仍 < 1 ms；内存同样增加约 40 KB，仍在 < 64 KiB 的估计之外再加 40 KiB，合计 < 128 KiB。冷启动不受影响（三个插件都是 `AfterFirstFrame`）。
- 基准：三个插件的 `plugin/*/activate`、`plugin/*/resident` 随 bench 的插件基准自动覆盖；不新增进程内基准。`app/cold_start` 不受影响（首帧前没有本 crate 的代码）。

## 界面

- 屏幕与层：[交互规范「屏幕」](interaction.md#屏幕)画板 01，常驻层；没有侧栏、工具栏、检查器、浮条（`ScreenLayout { sidebar: Hidden, back_button: false, reading_column: false, inspector: false, floating_bar: false }`）。窗口 820 × 520 pt（[app 设计「界面」](app.md#界面)）。布局以设计画布的 Welcome 画板为准（[#283](https://github.com/SoloJiang/lectern-app/issues/283)）：窗口分左右两栏，宽度按 360 : 320 的比例伸缩。左栏的内容水平居中，从上到下：应用图标（`ui::AppIcon`，96pt 的圆角方块，深色底，画布里是一个讲台图形；图形是 `ui` 里的 SVG 资源，不需要 `app` 提供，见「对其他 crate 的要求」E）、应用名（Large Title 层级，粗体）、一行次要文字的版本说明、两张操作卡片（Open Folder…、Clone Repository…，宽度撑满、最宽 300pt；Clone 卡片禁用（`ActionCard::disabled`，见「对其他 crate 的要求」G）：右边快捷键的位置是 `welcome-clone-soon`，点击和 ↵ 都不做任何事，不进入键盘选择；克隆流程的 issue 去掉禁用，见「未决问题」2）、最下面一行语言选择（`welcome-language` 的小标签加一个弹出菜单）。右栏铺在侧栏材质上（不是页面底色；不透明，`under_page_background`，见 [ui 设计「第二批控件」](ui.md#第二批控件283)），左边一条 0.5pt 分隔线，里面是两组平铺的行：没有卡片、没有行间分隔线，每组上面一行小标题（Subheadline 层级，半粗，次要色）；右栏整体可滚动。
- `ui` 的控件与动效原语（名字以 `ui` 设计「控件」为准，PR [#218](https://github.com/SoloJiang/lectern-app/pull/218)）：`Text`（全部文字）、操作卡片（`ui::ActionCard`；Open Folder、Clone：填充 `fill` 色、圆角 10pt、左边 20pt 的强调色图标 `Icon::Folder` / `Icon::Clone`、中间标题加一行次要说明、右边快捷键提示，见「对其他 crate 的要求」D）、弹出菜单（`ui::PopUpButton`，语言）、侧栏列表的行（`ui::SidebarRow::accent()`，右栏的行：圆角 8pt，两侧内缩；悬停是淡填充，选中是强调色填充）、`AgentDot`（发现项的 agent 圆点）、`Skeleton`（加载态）；颜色经 `colors(cx)`，文字层级经 `TextRole`（见[交互规范「视觉」](interaction.md#视觉)的文字样式）。本 crate 不写任何颜色值、字体名和动画时长（验收用 `rg` 检查）。
- 每一行的内容：
  - 最近项目：第一行 `name`（Headline 层级，半粗）；第二行次要文字（Callout 层级），各项用 ` · ` 连接，按这个顺序，不成立的项省略：`welcome-row-folder`（`key` 是文件夹而不是 git 仓库时）、计数（`to_review` 为 `Some(n)` 时是 `welcome-row-to-review`，`n` 为 0 也显示，画布的「3 to review」；为 None 时是 `welcome-row-sessions`（`sessions > 0`），即还没有记下过概览的项目照旧显示会话数）、`welcome-row-running`（`running > 0`）、所在目录（`path` 去掉最后一段，即项目的上一级；主目录写成 `~`，`Text::verbatim`，如 `~/work`）；`missing` 时第二行换成 `welcome-recent-missing`。待审数的来源见「待审数」。待审数、会话数和运行中数来自与发现列表同一次 `discover::build`（按 `key` 匹配，见「发现的文件夹」）；`welcome-discovery` 没有激活时为 0，不显示。行上没有移除按钮：移除用 ⌫ 和行的右键菜单（一项，`welcome-recent-remove` → `welcome::RemoveSelected`；`SidebarRow::context_menu` 的 `select` 回调把选择移到这一行、不打开它，菜单项再作用于选中的行）。
  - 发现的文件夹：行首 `AgentDot`（每种 agent 一个，最多 4 个），第一行 `name`（Headline 层级，半粗）；第二行次要文字（Callout 层级）：只有一种 agent 时是 `welcome-row-agent-sessions`（点名 agent，如「2 Claude Code sessions」），有几种时是 `welcome-row-sessions`（圆点已经表示各个 agent），然后（`running > 0` 时）`welcome-row-running`，最后 `Strings::relative_time(now - newest_activity)`，用 ` · ` 连接。路径不画在行里（画布没有）；完整路径仍是行的业务键和探针节点的 `NodeId`。
  - 选中的行（键盘选择、点击或右键）：整行用强调色（`accent`）填充，文字用强调色上的字色（`on_accent`），第二行用同一字色的 85% 不透明度；悬停的行用淡填充。选中状态由键盘和鼠标共用。画布画的是第一个最近项目已被选中；本屏幕没有选中项时 ↵ 什么也不做（见「Action 与快捷键」），所以进入屏幕时不预选。
- Fluent 键（`crates/i18n/locales/{en,zh-CN}/welcome.ftl`；已有的 6 个键保留，新增的在前置条件 A 里加）：

  | 键 | en | zh-CN | 用途 |
  |---|---|---|---|
  | `welcome-title` | { -app-name } | { -app-name } | 左栏标题，应用名（已有，值随 #283 改） |
  | `welcome-subtitle` | Version { $version } · Read what your agents wrote. | 版本 { $version } · 看 agent 写了什么。 | 标题下的一行次要文字；`$version` 是应用版本的「主版本.次版本」（`Args::str`），由 `app` 装配时经 `WelcomeDeps` 传入（「对其他 crate 的要求」E）；没有版本时用 `welcome-subtitle-plain` |
  | `welcome-subtitle-plain` | Read what your agents wrote. | 看 agent 写了什么。 | 没有版本号时的副标题 |
  | `welcome-open-folder` | Open Folder… | 打开文件夹… | Open Folder 卡片的标题（已有） |
  | `welcome-open-folder-hint` | A repository or any folder | 仓库或任意文件夹 | Open Folder 卡片的说明 |
  | `welcome-open-folder-shortcut` | ⌘O | ⌘O | Open Folder 卡片右边的快捷键提示（实现时加，见「与设计的差异」） |
  | `welcome-clone` | Clone Repository… | 克隆仓库… | Clone 卡片的标题（已有） |
  | `welcome-clone-hint` | From GitHub or GitLab | 来自 GitHub 或 GitLab | Clone 卡片的说明（已有） |
  | `welcome-clone-soon` | Coming Soon | 即将推出 | 禁用的 Clone 卡片右边的说明（#283 的画布决定新增） |
  | `welcome-recent` | Recent | 最近 | 列表小标题（已有，值随 #283 改） |
  | `welcome-recent-empty` | No recent projects | 没有最近的项目 | 空状态（已有） |
  | `welcome-recent-missing` | Folder no longer exists | 文件夹已不存在 | `missing` 行的第二行 |
  | `welcome-recent-remove` | Remove from Recents | 从最近的项目移除 | 行的右键菜单项 |
  | `welcome-discovered` | Found in Agent History | 在 agent 历史中发现 | 列表小标题（已有，值随 #283 改） |
  | `welcome-discovered-empty` | No agent sessions found yet | 还没有发现 agent 会话 | 空状态 |
  | `welcome-row-sessions` | { $count -> [one] { $count } session *[other] { $count } sessions } | { $count } 个会话 | 行的次要文字 |
  | `welcome-row-to-review` | { $count } to review | { $count } 个待审 | 最近项目行的待审数（#283 的画布决定新增） |
  | `welcome-row-agent-sessions` | { $count -> [one] { $count } { $agent } session *[other] { $count } { $agent } sessions } | { $count } 个 { $agent } 会话 | 发现项的次要文字（只有一种 agent；`$agent` 是产品名，`Args::str` 原样：Claude Code、Codex 的名字取 `project-home-agent-*`，别的 agent 用它自己的名字） |
  | `welcome-row-folder` | Folder | 文件夹 | 最近项目的次要文字：不是 git 仓库 |
  | `welcome-row-running` | { $count } running | { $count } 个运行中 | 行的次要文字 |
  | `welcome-language` | Language | 语言 | 语言弹出菜单旁的小标签 |
  | `welcome-language-not-saved` | The language setting couldn't be saved. The next launch uses the previous setting. | 语言设置未能保存，下次启动仍使用上一次保存的设置。 | `Notice::LanguageNotSaved` |
  | `welcome-open-failed-not-found` | That folder no longer exists. | 这个文件夹已不存在。 | `OpenError::NotFound` |
  | `welcome-open-failed-not-a-directory` | That path isn't a folder. | 这个路径不是文件夹。 | `OpenError::NotADirectory` |
  | `welcome-open-failed-permission` | { -app-name } can't read that folder. | { -app-name } 没有权限读取这个文件夹。 | `OpenError::PermissionDenied` |
  | `welcome-open-failed-repo` | The repository in that folder can't be read. | 这个文件夹里的仓库无法读取。 | `OpenError::RepoUnreadable` |
  | `welcome-open-failed-io` | That folder couldn't be opened. | 这个文件夹无法打开。 | `OpenError::Io` |

  语言名用 `common-language-en`、`common-language-zh-cn`、`common-language-system`；相对时间用 `Strings::relative_time`。路径、项目名原样显示（`Text::verbatim`），不翻译。

- UI 探针的语义节点（`ui` 设计「探针」（PR [#218](https://github.com/SoloJiang/lectern-app/pull/218)）的 `NodeId`、`Role`、`Label`；控件自带角色和文字，视图只给 ID；窗口根与 `workspace.content.screen[welcome]` 由 `workspace` 和探针提供）：

  | ID | `Role` | `Label` / `attrs` |
  |---|---|---|
  | `welcome.root` | `Group` | `recent`（`loading` / `ready`）、`discovered`（`unavailable` / `pending` / `ready`）、`opening`（`true` 时才有） |
  | `welcome.title` | `Text` | `Label::key = welcome-title` |
  | `welcome.open-folder` | `Button` | `Label::key = welcome-open-folder`（操作卡片） |
  | `welcome.language` | `Button` | `Label::key = welcome-language`；`attrs`：`value`（当前选择：`system` / `en` / `zh-CN`）。`ui::PopUpButton`；菜单是系统菜单，菜单项不是探针节点，探针经 `welcome::SelectLanguage` 切换、读 `value` 确认 |
  | `welcome.notice` | `Text` | `Label::key` 是 `welcome-open-failed-*` 或 `welcome-language-not-saved`；没有提示时不出现 |
  | `welcome.recent` | `List` | — |
  | `welcome.recent.empty` | `Text` | `Label::key = welcome-recent-empty`；列表非空时不出现 |
  | `welcome.recent.item[<path>]` | `ListItem` | `State::selected`（`ui::SidebarRow` 声明，没有 `Label` 和 `attrs`） |
  | `welcome.recent.item[<path>].name` | `Text` | `Label` 是 `name`（`key` None）；`attrs`：`kind`（`git` / `folder`，取自 `key` 的变体）、`sessions`、`running`、`missing`（`true` 时才有） |
  | `welcome.discovered` | `List` | — |
  | `welcome.discovered.empty` | `Text` | `Label::key = welcome-discovered-empty` |
  | `welcome.discovered.item[<path>]` | `ListItem` | `State::selected` |
  | `welcome.discovered.item[<path>].name` | `Text` | `Label` 是 `name`；`attrs`：`agents`（按首次出现的顺序，如 `codex,claude-code`）、`sessions`、`running` |

  `<path>` 是条目的 `path`（业务键，不是下标；含换行或 `].` 的路径把这两处换成空格和 `]·`，`NodeId` 不接受它们）。骨架块不声明节点。

- Action 与快捷键（都能被探针按名字触发；经 `KeyBindings` 贡献登记，谓词 `Content > welcome`，视图的根元素设 KeyContext `welcome` 并持有焦点）：

  | Action | 键 | 处理 |
  |---|---|---|
  | `welcome::SelectNext` | ↓ | 选择线性序列里的下一项；没有选中时选第一项；已是最后一项时不动 |
  | `welcome::SelectPrevious` | ↑ | 上一项；在第一项时不动；没有选中时选最后一项 |
  | `welcome::OpenSelected` | ↵ | 对选中项 `begin_open` 后 `dispatch(OpenProject(path))`；没有选中、或 `opening.path` 就是它时不做任何事 |
  | `welcome::RemoveSelected` | ⌫（也是行的右键菜单项） | 选中的是最近项目时 `remove_recent(path)`，选择移到同位置的下一项（没有则上一项）；选中的是发现项时不做任何事 |
  | `welcome::SelectLanguage { locale: Option<Locale> }` | — | 弹出菜单的选择；探针用它切换语言，构造时的 JSON 是 `{"locale": "en"}`、`{"locale": "zh-CN"}`、`{"locale": null}`（`Locale` 没有 serde，所以这个 `Action` 手写 `build`，见「与设计的差异」第 2 步） |

  ⌘O 不在这里绑定：它是 `workspace::OpenFolder` 的全局绑定，Open Folder 按钮和它都走 `PromptOpenProject`。Esc、⌘[ 在 Welcome 上由 `workspace` 忽略（[workspace 设计「导航状态机」](workspace.md#导航状态机)）。鼠标点击一行同时把选择移到这一行，键盘和鼠标触发同一个 `Row` 的选中样式。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)：

| 情况 | 处理 |
|---|---|
| 最近项目文件不存在（第一次运行） | `Ready(空)`，显示 `welcome-recent-empty`，不记日志 |
| 最近项目文件解析失败或版本不认识 | 记 warn，`Ready(空)`；下一次打开项目时覆盖写入 |
| 写文件失败（目录不可写、磁盘满） | 记 warn，内存里的列表照常用；下一次变化再试 |
| 列表还没到达（`Loading` / `Pending`，< 100 ms） | 只显示标题行；≥ 100 ms 显示骨架（2 行）；不显示进度（读文件和发现都不可取消，且远小于 1 s） |
| `welcome-discovery` 没有激活（profile 里没有装它，或会话来源的提供者失败、停用后它被停用；`Unavailable`） | 显示 `welcome-discovered-empty`，不显示骨架；最近项目行不显示会话数 |
| 会话目录为空、所有会话都在已打开或最近项目里 | `Ready(空)`，显示 `welcome-discovered-empty` |
| 会话的 `Place::Pending`、`Missing`、`Unresolvable` | 跳过；归位完成后随下一次 `SessionsChanged` 出现 |
| 打开失败（`Opened { Err }`） | 停在 Welcome（`workspace`），显示一行 `welcome-open-failed-*`，清 `opening`（同一行可以再试）；本屏幕发起、且是 `NotFound` / `NotADirectory` 时对应的最近项目标 `missing`，行上显示 `welcome-recent-missing`，可以移除；下一次 `begin_open` / `clear_notice` 时清空提示 |
| 打开成功但 `Projects::snapshot(key)` 为 None（打开后立刻被关闭） | 不记录最近项目，记 debug |
| 打开一个已经打开的项目（同一窗口里换项目时 `Opened` 带同一个 key） | 照常更新最近项目（移到最前） |
| 选中的行在重算后消失（发现项被打开或被最近项目遮住） | 选择按位置保留在同一列表的同位置，越界时取最后一项，列表空时为 None |
| `remove_recent` 的路径已不在列表里 | 忽略 |
| `welcome` 插件停用时视图正在显示 | `workspace` 按基底屏幕被移除处理（显示 `workspace-screen-missing`）；本 crate 的任务随闸门关闭丢弃 |
| `welcome-discovery` 停用 | 发现列表回到 `Unavailable`（「数据流与线程」第 10 步），Welcome 屏幕不受影响 |
| 语言偏好写入失败 | `platform` 记 warn；界面已切换，本次运行有效；显示一次 `welcome-language-not-saved`，下次启动用最后一次成功保存的值 |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `welcome`，不启用 `sink`。

- 默认记录：
  - info `recent list loaded`：`entries`（条数）、`took_us`。
  - warn `recent list unreadable`：`reason`（`io` / `json` / `version`）、`io`（`Value::io`，只在 `io` 时）、`version`（读到的版本号，只在 `version` 时）。
  - warn `recent list not written`：`step`（`create_dir` / `create` / `write` / `rename`）、`io`。
  - warn `recent entry skipped`：`reason`（`non-utf8-path`）。
  - info `project opened from welcome`：`source`（`recent` / `discovered` / `prompt`，取自 `opening`）。`restore` 和 ⌘O 打开的不经本屏幕的视图，不记这一条。
  - warn `project open failed`：`error`（`OpenError` 的种类：`not-found` 等）、`from_welcome`（`opening` 是否有值）。`workspace` 也记同一事件（它的 target 是 `workspace`），两条对照用。
  - warn `language override not saved`：`error`（`platform` 给出的种类）。
  - info `discovery unavailable`：`welcome-discovery` 的 `Active` drop 时记一次（停用原因由 kernel 记）。
  - 待审数不新增默认记录（记下失败的情形只有 `overview` 为 None，不是错误）。
- 详细级别：debug `snapshot rebuilt`（`generation`、`recent`、`discovered`、`sessions`（归并前的会话总数）、`skipped`（`Pending` 等跳过的数量）、`took_us`）；debug `recent list written`（`entries`、`bytes`）；debug `recent entry removed`；debug `selection`（`list`、`index`）；debug `locale selected`（`locale`：`system` / `en` / `zh-cn`）；debug `review marked`（`entries`（更新的最近项目条数）、`sessions`（待审会话数）、`changed`（集合是否变化））。
- 关联 ID：`plugin`（`welcome`、`welcome-discovery` 或 `welcome-review`）、`act` 由 kernel 在监听和 `started` 外设置，`task` 由 executor 设置；主线程上的视图代码在 `workspace` 调用视图工厂时恢复的作用域里运行（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 11 项），Action 处理函数不在作用域里，只带 target。不设 `session`：本 crate 不记任何与单个会话有关的记录。
- 不记录：会话键（`ReviewMark::sessions` 只记个数）、最近项目和发现项的路径与名字（`recent.json` 的内容、`RecentEntry::path`、`DiscoveredFolder::path`、`ProjectSnapshot::name`）、会话的 cwd 与标题、`OpenError::RepoUnreadable` / `Io` 附带的文字、`data_dir` 的路径、语言偏好之外的任何系统偏好。用条数、种类、代数和耗时代替。`workspace` 同样不记项目路径，所以日志里找不到用户打开过哪些目录。
- 金丝雀测试：`canary_stays_out_of_welcome_logs`（`src/tests/logs.rs`）：`lectern_logging::testing::capture()` 在 trace 级别，用含金丝雀的路径与项目名构造 `recent.json`（含一条坏行触发 warn）、假 `Projects` 的快照、假 `SessionSource` 的会话（cwd、标题、`rel` 里都有金丝雀），跑完加载、`Opened`、重算、移除、写文件（写到一个不可写的目录触发 `recent list not written`）、`Opened { Err(RepoUnreadable(金丝雀)) }`，`assert_absent(金丝雀)`。

## 测试

依赖 GPUI，只在 macOS 上编译和测试：`just test -p lectern-welcome`。

- 纯函数（`#[test]`，不依赖 GPUI）：
  - `recent::push_front_dedupes_by_path_and_caps_at_10`、`recent::remove_keeps_order`、`recent::decode_rejects_unknown_version`、`recent::decode_ignores_unknown_fields`、`recent::encode_roundtrips`（用 `fixtures/recent_v1.json`）、`recent::entry_from_snapshot_joins_root_and_opened_at`（子目录打开、linked worktree、裸仓库三种）。
  - `discover::groups_by_project_and_takes_the_newest_root`、`discover::root_strips_rel_from_cwd`、`discover::skips_open_projects_and_recent_keys`（同一仓库的其他 worktree 里的会话不再出现）、`discover::skips_pending_missing_and_unresolvable`、`discover::running_needs_open_turn_within_10_min`、`discover::subagents_count_as_sessions_not_running`、`discover::caps_at_50`、`discover::recent_rows_get_counts_by_key_from_located_and_resolved`、`discover::to_review_counts_marked_sessions_still_listed_and_later_activity`（标记里不在会话列表的不计；`last_activity` 等于 `at` 的不计，多 1 ms 的计；子 agent 不计；没有标记时为 None）、`recent::decode_reads_a_file_without_marks`（旧的 `recent_v1.json` 读成 `review: None`）。
- 模型（`#[test]`，`TestKernel` 注入假的 `dyn Projects`、`dyn SessionSource`、`dyn Contributions`，executor 用测试队列）：`welcome_started_loads_the_file`、`welcome_started_records_projects_opened_before_it`（假 `Projects` 在 `started` 之前已有一个打开的项目，`Opened` 没有送达，列表最前仍是它）、`discovery_started_builds_the_list`、`recent_loaded_after_discovery_triggers_a_rebuild`（先发现后读文件，最近项目不留在发现列表里）、`welcome_activates_without_any_session_source`（只装 `welcome`：屏幕已注册，`discovered` 是 `Unavailable`）、`discovery_drop_sets_unavailable`、`opened_records_a_recent_entry_with_its_key_and_writes_the_file`、`opened_without_a_snapshot_records_nothing`、`opened_before_the_file_is_loaded_does_not_overwrite_it`（先 `Opened` 后读完文件：文件里原有的条目都还在，新条目在最前）、`writes_are_serialized_and_coalesced`（写任务开始之前的两次变化只写一次；写的过程中又来一次变化，再写一次，最后写出的是最新列表）、`sessions_changed_rebuilds_once_when_events_pile_up`（任务开始之前的三次事件只跑一轮；运行中到达的再多跑一轮）、`remove_recent_updates_the_snapshot_before_writing`、`rebuild_merges_counts_without_restoring_a_removed_entry`、`begin_open_then_error_marks_the_entry_missing_and_clears_opening`、`error_without_opening_only_shows_the_notice`、`wake_is_called_once_per_snapshot`、`overview_event_marks_the_entries_of_that_key_and_writes_once`（同一集合的第二次概览不写文件）、`overview_gone_leaves_the_mark`、`welcome_activates_without_review`（只装 `welcome` 和 `welcome-discovery`：行上显示会话数）。
- `#[gpui::test]`（`TestAppContext`，`lectern_ui::init_fixed`）：`first_render_shows_title_and_open_folder_before_any_snapshot`（模型还是 `Loading` / `Pending` 时的语义树只有标题、Open Folder、语言弹出按钮和两个空的列表）、`skeleton_appears_after_100ms_and_rows_replace_it`、`click_and_enter_dispatch_open_project_once`（`opening` 期间对同一行再按 ↵ 不重复，失败后可以再试）、`open_folder_dispatches_prompt`、`arrows_move_selection_across_both_lists`、`backspace_removes_the_selected_recent_and_moves_selection`、`open_error_shows_a_notice_and_clears_on_the_next_open`、`discovered_rows_update_on_wake`、`unavailable_discovery_shows_the_empty_state_without_a_skeleton`、`language_picker_switches_strings_and_calls_platform`（假的 `platform` 写入函数经 `test-support` 注入；失败时显示 `welcome-language-not-saved`）、`actions_are_triggerable_by_name`（探针契约：五个 `Action` 都经 `App::build_action` 按名字构造，`SelectLanguage` 从 JSON 构造）。实现里另加：`enter_opens_a_discovered_folder`、`up_without_a_selection_selects_the_last_item`、`the_context_menu_selects_its_row_and_does_not_open_it`（右键先选中该行、不打开它；菜单项 `RemoveSelected` 再移除选中的行；发现项没有菜单）、`the_columns_follow_the_canvas`（卡片 300pt 且居中、语言行在左栏底部、行在右栏内、行距 4pt）、`a_click_on_a_row_that_is_gone_does_nothing`、`the_selection_stays_in_place_when_the_lists_change`、`the_picker_starts_on_the_saved_choice`、`a_failed_language_write_shows_the_notice_once`、`the_selected_row_is_scrolled_into_view`、`the_bindings_are_the_designs_and_below_content`（普通 `#[test]`）；#283 的画布决定：`the_clone_card_is_disabled_and_does_nothing`（语义节点 `disabled`，点击不发命令，↓ 不选中它）、`a_recent_row_shows_to_review_when_known`（纯函数，`src/tests/selection.rs`：`Some(3)`、`Some(0)`、None 三种）。视图的测试窗口 820 × 520，外面套一个 `Content` 区域的 `KeyContext`（`workspace` 的做法），按键走真实的 `KeyBindings` 载荷；屏幕由登记到假 `dyn Contributions` 里的 `ui::contrib::Screen` 载荷的工厂建出。语言偏好经 `prefs::LanguagePrefs`（`src/prefs.rs`）的假实现，测试从不碰真实的用户默认值。纯函数：`src/tests/selection.rs`（`Selection::next` / `previous` / `clamped`、`display_path`、`node_key`，以及第二行与副标题的文字：`a_recent_row_reads_folder_sessions_running_and_the_parent_directory`、`a_missing_recent_row_says_so_and_nothing_else`、`a_discovered_row_names_a_single_agent_and_not_several`、`the_subtitle_carries_the_version_or_goes_without`；这些文字不是探针节点，所以测纯函数）。语言弹出菜单的菜单项是系统菜单，不在语义树里：测试经 `welcome::SelectLanguage`（按名字从 JSON 构造）切换，读 `welcome.language` 的 `value` 确认。
- 语义树：`welcome_tree_empty`、`welcome_tree_with_lists`、`welcome_tree_with_error`、`welcome_tree_loading` 的 insta 快照（`semantic::Recorder` 的记录去掉 `bounds` 和 `declared_at` 之后的文本，`src/tests/snapshots/`）。
- 日志：`canary_stays_out_of_welcome_logs`。
- fixtures（`crates/welcome/tests/fixtures/`）：`recent_v1.json`（3 条）、`recent_bad_version.json`、`recent_garbage.json`；会话列表在测试里直接构造 `SessionSummary`，不用真实的会话文件、git 和文件 IO（写文件的测试用临时目录）。

## 对其他 crate 的要求

每一项是一个 PR，写 `Refs #29`，在「验收」里列为前置条件。

- **A. `i18n`**（[#237](https://github.com/SoloJiang/lectern-app/pull/237)）：`locales/{en,zh-CN}/welcome.ftl` 加上「界面」表里新增的 14 个键（`welcome-subtitle`、`welcome-recent-missing`、`welcome-recent-remove`（含 `.tooltip`）、`welcome-discovered-empty`、`welcome-row-sessions`、`welcome-row-running`、`welcome-language`、`welcome-language-not-saved`、5 个 `welcome-open-failed-*`）。编译期校验保证两种语言一致。
- **B. `platform`**：语言偏好（[i18n 设计「数据流与线程」](i18n.md#数据流与线程)已写明由 `platform` 读写，这里是它的第一个调用方；已实现，[#243](https://github.com/SoloJiang/lectern-app/pull/243)，签名与下面一致，实现的补充见 platform 设计「系统首选语言」的「语言偏好」）：

  ```rust
  pub mod system {
      /// 全局域（`NSGlobalDomain`）里的系统首选语言列表，不含应用自己的覆盖。规则同 `preferred_languages`。主线程。
      pub fn system_languages() -> Vec<String>;
      /// 应用域里是否保存了 `AppleLanguages` 覆盖：覆盖的第一个标签，没有覆盖时 None。内存里的偏好，主线程，不读文件。
      pub fn language_override() -> Option<String>;
      /// 写应用域的 `AppleLanguages`：`Some(tag)` 写成单元素数组，None 删除这个键。立即返回；写入进 `platform` 的串行队列，
      /// 队列里尚未执行的旧请求丢弃（它们的 `done` 收到 `Err(Superseded)`）；执行后在队列线程上调一次 `done`，失败另记 warn。
      /// 只在 macOS 上有效，其他平台立即 `done(Ok(()))`。
      pub fn set_language_override(tag: Option<&str>, done: Box<dyn FnOnce(Result<(), PrefError>) + Send>);
      pub enum PrefError { Superseded, WriteFailed }
  }
  ```

  在 [platform 设计](platform.md)「系统首选语言」一节下补上这三个函数。与 [i18n 设计「数据流与线程」](i18n.md#数据流与线程)的差异：那里写 `language_override() -> Option<Locale>`；`platform` 在基础设施层、不依赖 `i18n`，所以返回 BCP 47 标签，调用方用 `Locale::negotiate([tag])` 转换。前置条件 B 的 PR 同时把 i18n 设计里的那一行改成了标签。
- **C. `app`**（已实现，见 [app 设计「welcome 的装配」](app.md#welcome-的装配)）：`Profile::Desktop` 的表加两行：`lectern_welcome::plugins(WelcomeDeps { executor, data_dir: app_data.join("welcome"), version })`（`version` 随 #283 加上，见 E） 的两个插件，在 `workspace` 之后逐个放进 `PluginSet::with`。`Resources::app_data` 已是非可选的 `PathBuf`（[app 设计「装配」](app.md#装配)：`app_data()` 为 None 时 `app` 不装配任何插件），本 crate 不处理没有目录的情况。
- **D. `ui`**（为 [#283](https://github.com/SoloJiang/lectern-app/issues/283) 新增；已实现，[ui 设计「第二批控件」](ui.md#第二批控件283)，PR [#289](https://github.com/SoloJiang/lectern-app/pull/289)）：`ActionCard`（Open Folder、Clone）、`PopUpButton` 与 `MenuItem`（语言）、`SidebarRow`（`accent()`、`context_menu(items, select)`；右栏的行）、`AppIcon`、`Palette::fill`。
- **F. `services`**（#283 的画布决定，`Refs #283`；已实现，测试 `needs_review_is_unknown_unseen_or_changed_since_seen`）：`SessionOverview::needs_review(&self) -> bool`（`progress` 为 None、`seen < total` 或 `changed_since_seen > 0`），`project_home` 的 `main_model` 改用它，判断只写一处。
- **G. `ui`**（#283 的画布决定，`Refs #283`；已实现，`semantic_snapshot_action_card` 含一张禁用的卡片）：`ActionCard::disabled(bool)`：图标和标题改用次要色（`secondary_label`），说明和右边的提示用 `tertiary_label`，没有悬停变化，不调 `on_click`，探针节点带 `disabled`。见 [ui 设计「第二批控件」](ui.md#第二批控件283)。
- **H. `app`**（#283 的画布决定，随第 4 步）：`plugins(deps)` 返回三个插件，`Profile::Desktop` 多放进一个 `welcome-review`；`every_profile_graph_validates` 里 desktop 的插件数加一。
- **E. `app`**（已实现）：应用版本（主.次）经 `WelcomeDeps::version` 传入，`app` 从包版本的主、次版本号拼成（[app 设计「welcome 的装配」](app.md#welcome-的装配)）。应用图标不再需要 `app` 提供：`ui::AppIcon` 画的是 `ui` 自带的 SVG（`IconAssets`），`app` 早已把 `IconAssets` 交给 `Application::with_assets`。`version` 为 None 时副标题不带版本，其余布局不变。

## 未决问题

产品负责人在 2026-10-29 决定：两个问题都按设计画布（最近项目行显示待审数，做法见「待审数」，不为没有打开的项目算概览；显示 Clone 卡片，克隆流程实现之前禁用）。原文保留如下。

1. **最近项目行是否显示待审数。** issue 写「最近项目（待审数、运行中数）」。待审数要对没有打开的项目算审阅概览：读 worktree 的改动、按会话归因（[review 设计](review.md)），每个项目几十到几百毫秒的 git 操作，Welcome 显示 10 个项目就是最多几秒的后台工作，并且会在用户还没选项目之前打开 10 个仓库。默认（本文按它写）：Welcome 只显示会话数和运行中数（都来自已经在内存里的会话列表，没有额外 IO），待审数在打开项目后的项目主页显示；以后 `review` 有廉价的按项目计数时再加一列，行的结构不变。另一个选项：Welcome 对最近项目逐个 `Review::watch_overview`，显示概览里的会话数，接受上述开销。设计画布（[#283](https://github.com/SoloJiang/lectern-app/issues/283)）的最近项目行显示「3 to review」，即选了后一个选项；这个问题仍未决定，默认不变。
2. **Clone 按钮在这个 issue 里是否出现。** 交互规范写「Open Folder（⌘O）/ Clone」，但克隆流程（目标目录、进度、forge 登录）没有设计。默认：本 issue 不显示 Clone 按钮，`welcome-clone` 的键保留，Clone 的 issue 把按钮加回左栏。另一个选项：显示一个禁用的 Clone 按钮并在悬停时说明「即将推出」。设计画布（#283）在 Open Folder 下面画了一张 Clone Repository… 卡片（说明「From GitHub or GitLab」）；这个问题仍未决定，默认不变，卡片的样式和文字（`welcome-clone`、`welcome-clone-hint`）已经按画布写好，Clone 的 issue 把卡片加回左栏。

## 已知限制

- 运行中的计数只在 `SessionsChanged` 时刷新：一个会话停下来之后，最长要到下一次任何来源的事件才从「运行中」变回普通会话；没有新事件时它一直显示运行中。项目主页实现时如果加了定时刷新，本 crate 复用同一机制。
- 最近项目的路径在启动时不检查是否存在：失效的条目在用户打开失败时才标出。
- `missing` 不写进文件：重启后标记消失，再次打开失败时再标。
- 发现的文件夹按 `ProjectKey` 分组，显示最新会话所在的 worktree 根；同一仓库其他 worktree 里的会话算进同一条，打开后在项目主页里按 worktree 查看。
- 「尚未打开过」只对照最近项目列表（最多 10 条）的 `key`：更早打开过、已经被挤出列表的项目会重新出现在发现列表里。仓库被移动后 `ProjectKey` 变了（[project 设计「项目标识」](project.md#项目标识)），旧条目打开失败标 `missing`，新位置的会话作为新的发现项出现。
- 本屏幕同一时刻只跟踪一个进行中的打开（`opening`）：在 `Opened` 到达之前点第二行，第二次的失败不会标 `missing`。
- 非 UTF-8 的项目路径不写进 `recent.json`（本次运行里仍在列表中）。

## 验收

分三步（第 3 步是 [#283](https://github.com/SoloJiang/lectern-app/issues/283) 的重绘），每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-welcome`、`just deny` 通过。前置条件 A、B 各自的 PR 先合并；C 随第 2 步；`workspace` 的第 2 步（根视图与贡献应用）和 `ui` 的实现在第 2 步之前。

**第 1 步：插件、模型、最近项目的存储、发现列表的归并（不依赖 GPUI 的部分，`Refs #29`，[#240](https://github.com/SoloJiang/lectern-app/pull/240)）。**

- `cargo check -p lectern-welcome` 通过；`Cargo.toml` 带 `plugin = true`，`python3 scripts/check_architecture.py` 通过：`welcome` 不依赖其他插件 crate，只有 `app` 依赖它。
- `welcome_activates_without_any_session_source` 通过：没有 `dyn SessionSource` 的提供者时 `welcome` 照常激活并注册屏幕。
- 「测试」里 `recent::*`、`discover::*` 和模型的测试通过。
- `rg 'std::fs' crates/welcome/src --glob '!src/recent/store.rs'` 没有结果；`rg 'Projects::open\b|\.open\(' crates/welcome/src` 没有结果（本 crate 不调 `Projects::open`）。
- `canary_stays_out_of_welcome_logs` 通过；`rg 'eprintln!|println!|log::' crates/welcome/src` 没有结果。
- `rg 'std::fs' crates/welcome/src/recent/store.rs` 的调用只出现在由 `Background` 任务调用的函数里（审阅时核对）。

**第 2 步：视图、Action、语言选择、语义树（`Closes #29`）。**

- 「测试」里的 `#[gpui::test]` 和语义树快照通过。
- `rg 'rgb\(|hsla\(|Duration::from_millis|from_secs_f32' crates/welcome/src` 没有结果（颜色和动画参数只来自 `ui`）。
- `rg '"[A-Z][a-z]+ [a-z]' crates/welcome/src --glob '!tests*'` 没有界面字符串字面量（都走 `keys::welcome::*`、`keys::common::*`）；`rg 'Text::verbatim\(Arc::from\("' crates/welcome/src` 没有结果（原样显示的只有快照里的路径和名字）。
- `just test -p lectern-app`：`every_profile_graph_validates` 里 desktop 的插件数加二。
- 手动在 macOS 上运行发布构建（临时 `HOME`）：首次启动显示 Welcome，两个列表为空状态；打开一个文件夹再重启后回到项目主页，⌘W 关闭、点 Dock 重开后 Welcome 的最近项目里有它；移除它后 `~/Library/Application Support/Lectern/welcome/recent.json` 里没有它；日志里没有这个文件夹的路径；在临时 `HOME` 下放一个 Claude Code 会话目录（`projects/<编码的 cwd>/<uuid>.jsonl`，cwd 指向一个未打开的仓库），启动后它出现在发现列表里，↓ ↵ 打开它。
- 空闲内存：Welcome 静止后的 `phys_footprint` 与 `workspace` 第 3 步「验收」的数值相比增加不超过 1 MB。

**第 3 步：界面按设计画布重绘（`Refs #283`，叠在 `ui` 第 5 步之上）。**

- 「测试」里的 `#[gpui::test]`、纯函数和语义树快照通过；`just check`、`just test -p lectern-welcome -p lectern-app -p lectern-i18n -p lectern-workspace`、`just doctest` 通过。
- 第 2 步的三条 `rg`（颜色与动画参数、界面字符串字面量、`Text::verbatim(Arc::from("`）仍没有结果。
- 手动在 macOS 上运行（临时 `HOME`），对照画布：两栏 360 : 320、应用图标、居中的 Open Folder 卡片、语言弹出菜单的勾、右键菜单、右栏的选中行。

**第 4 步：#283 的画布决定（待审数、禁用的 Clone 卡片，`Refs #283`）。** 前置条件 F、G 先合并；H 随本步。

- 「测试」里为 #283 的画布决定新增的测试通过；`just check`、`just test -p lectern-welcome -p lectern-app -p lectern-i18n`、`just doctest` 通过；第 2 步的三条 `rg` 仍没有结果。
- `rg 'lectern_git|watch_overview' crates/welcome/src` 没有结果（本 crate 不做 git 操作、不关注概览）。
- `canary_stays_out_of_welcome_logs` 加上含金丝雀的会话键（经 `ReviewMark::sessions`）后仍通过。

## 实际验证

### 第 1 步

本 crate 不依赖 GPUI 的部分（[#240](https://github.com/SoloJiang/lectern-app/pull/240)），在 macOS（arm64，Rust 1.97.0）上运行；当时本 crate 没有 GPUI 依赖，所以也在 Linux CI 的 GPUI-free 任务里运行（第 2 步加了 `gpui` 之后不再，见第 2 步）：

- `cargo check -p lectern-welcome`、`just check`（含 `cargo clippy --workspace --all-targets -- -D warnings` 和架构检查：32 个 crate，`welcome` 带 `plugin = true`，没有依赖其他插件 crate）、`just deny` 通过。
- `just test -p lectern-welcome`：47 个测试通过，都是普通 `#[test]`，连续 15 次运行结果相同。
  - `src/tests/recent.rs`（13 个）：`push_front_dedupes_by_path_and_caps_at_10`、`push_front_keeps_one_entry_per_path_not_per_key`、`remove_keeps_order`、`decode_rejects_unknown_version`、`decode_ignores_unknown_fields`、`decode_dedupes_and_caps_a_hand_edited_file`、`encode_roundtrips`（用 `fixtures/recent_v1.json`，并有 insta 快照）、`encode_leaves_out_memory_only_fields`、`encode_skips_entries_whose_path_is_not_utf8`（Unix）、`entry_from_snapshot_joins_root_and_opened_at`（根、子目录、linked worktree、裸仓库、文件夹项目）、`store_reads_what_it_wrote`、`store_reports_unreadable_files_by_reason`、`store_write_names_the_failed_step`。
  - `src/tests/discover.rs`（10 个）：「测试」列出的 8 个 `discover::*`，加 `at_most_four_agents_in_order_of_first_appearance`、`sources_are_merged_newest_first`；`groups_by_project_and_takes_the_newest_root` 带 insta 快照。
  - `src/tests/model.rs`（23 个）：「测试」列出的模型测试（`welcome_activates_without_any_session_source` 断言 `Screen(WELCOME)` 带「接口」的 `ScreenLayout` 登记一次、没有 token），加 `opening_a_listed_project_moves_its_entry_to_the_front`、`an_unreadable_file_gives_an_empty_list_that_the_next_open_replaces`（坏版本和坏 JSON 两种文件）、`a_failed_write_keeps_the_list_in_memory`、`project_events_other_than_changed_rebuild`、`an_open_project_leaves_the_discovered_list`、`remove_recent_of_an_unknown_path_is_ignored`、`language_not_saved_sets_the_notice`。
  - `src/tests/logs.rs`：`canary_stays_out_of_welcome_logs`：金丝雀放在最近项目文件的路径与项目名、会话的 cwd 与 `rel` 与 ID、打开的项目的路径与名字、`OpenError` 的附带文字里，另有一个写不出的数据目录和一个坏文件；trace 级别跑完加载、打开、重算、移除、失败、停用；没有记录含金丝雀，且 `recent list loaded`、`recent list unreadable`、`recent list not written`、`project opened from welcome`、`project open failed`、`language override not saved`、`discovery unavailable` 都出现过。
  - 夹具：假的 `dyn Projects`（记录收到的 open 请求，所有测试断言它为空）、`dyn SessionSource`（两个来源）、`dyn Contributions` 都写在 `src/tests/support.rs`，会话是合成的 `SessionSummary`，路径都是虚构的；没有测试读取这台机器上的会话目录或任何用户目录，写文件的测试用临时目录。
- `rg 'std::fs' crates/welcome/src --glob '!src/recent/store.rs'`、`rg 'Projects::open\b|\.open\(' crates/welcome/src`、`rg 'eprintln!|println!|log::' crates/welcome/src` 都没有结果；`std::fs` 的调用在 `store.rs` 里：`read`、`write` 只从 `Background` 任务调用（`load`、`write_task`），其余是 `#[cfg(test)]` 的 `store::testing`。
- 没有验证的：`Screen(WELCOME)` 的载荷是占位类型，`workspace` 的真实应用（对 `ui::contrib::Screen` 的 `downcast`）和 `app` 装配要等第 2 步；`plugin/welcome/*` 的激活耗时和空闲内存没有测（`app` 还没有装配这两个插件，bench 的插件基准不覆盖它们）。

### 第 2 步

视图、Action、语言选择、语义树（`Closes #29`），在 macOS（arm64，Rust 1.97.0）上运行。基于 `ui` 第 4 步（[#242](https://github.com/SoloJiang/lectern-app/pull/242)，`semantic(...)`、`Recorder`）（已合并）。`app` 没有装配本 crate（另一个 PR 在做），所以没有运行过应用，下面都是 `#[gpui::test]` 与普通测试：

- `just check`（格式、架构规则、文档链接、`cargo clippy --workspace --all-targets -- -D warnings`）、`just deny` 通过；`cargo check -p lectern-welcome`（不带 `--tests`，发布构建能编译的那一份依赖特性）通过。`Cargo.lock` 只多了 `lectern-welcome` 的依赖行，没有新 crate。
- `just test -p lectern-welcome`：76 个测试通过（第 1 步的 47 个不变，新增 29 个），连续 12 次运行结果相同；`cargo test --doc -p lectern-welcome` 没有文档测试。
  - `src/tests/view.rs`（23 个，除 `the_bindings_are_the_designs_and_below_content` 外都是 `#[gpui::test]`）：「测试」列出的 `first_render_shows_title_and_open_folder_before_any_snapshot`、`skeleton_appears_after_100ms_and_rows_replace_it`（骨架不声明节点，用它们加在发现列表之上的高度判断，时间用 GPUI 测试时钟的 `advance_clock`）、`click_and_enter_dispatch_open_project_once`、`open_folder_dispatches_prompt`、`arrows_move_selection_across_both_lists`、`backspace_removes_the_selected_recent_and_moves_selection`、`open_error_shows_a_notice_and_clears_on_the_next_open`、`discovered_rows_update_on_wake`、`unavailable_discovery_shows_the_empty_state_without_a_skeleton`、`language_picker_switches_strings_and_calls_platform`、`actions_are_triggerable_by_name`，加「测试」里写的另外 9 个，以及四个语义树快照（`welcome_tree_empty`、`welcome_tree_with_lists`、`welcome_tree_with_error`，`welcome_tree_loading` 在第一个测试里）。
  - `src/tests/selection.rs`（6 个）：`Selection` 的移动与夹紧、`display_path`、`node_key`。
  - 夹具：`src/tests/support.rs` 的 `FakeContributions` 多保存了登记的载荷并记录 `dispatch` 收到的命令，`FakePrefs` 是内存里的语言偏好。路径都是虚构的，没有测试读取用户目录、真实的会话目录或用户默认值。
- 「验收」第 2 步的 `rg`：`rg 'rgb\(|hsla\(|Duration::from_millis|from_secs_f32' crates/welcome/src`、`rg '"[A-Z][a-z]+ [a-z]' crates/welcome/src --glob '!tests*'`、`rg 'Text::verbatim\(Arc::from\("' crates/welcome/src`、第 1 步的 `std::fs` / `Projects::open` / `eprintln!` 三条、`rg 'Recorder::begin' crates/welcome`（只有 `src/tests/view.rs`）都没有结果；`ui` 设计里的 `rg '\.font_family\(|gpui_component::Button'` 也没有。
- 手动检查 820 × 520 下节点的边界（临时输出，没有入库）：左栏 300 pt 加 24 pt 内边距，右栏的列表 472 pt 宽，行高 46 pt，语言分段控件在左栏底部，移除按钮在选中行的行尾。

没有验证的：

- 在 macOS 上运行发布构建（首次启动、打开文件夹、重启、⌘W、移除、发现会话、↓ ↵）、视觉效果（`headless` 的 `#[gpui::test]` 不画像素）、空闲内存 `phys_footprint`，因为 `app` 还没有装配本 crate，也不能运行应用。
- `just test -p lectern-app` 里 `every_profile_graph_validates` 的插件数加二：前置条件 C，在 `app` 的 PR。
- 用屏幕阅读器读 `semantic_interactive` 写进去的无障碍属性（`#[gpui::test]` 拿不到 AccessKit 树）。
- 真实的 `platform::system::set_language_override`：测试只用假实现（它在 `platform` 里有自己的测试）。
- Linux CI 的 GPUI-free 任务不再覆盖本 crate（有了 `gpui` 依赖），结果以 macOS CI 为准。

### #283 的重绘

[#283](https://github.com/SoloJiang/lectern-app/issues/283)（`Refs`），在 macOS（arm64，Rust 1.97.0）上运行，叠在 `ui` 第 5 步（[#289](https://github.com/SoloJiang/lectern-app/pull/289)）之上。没有运行应用，下面都是 `#[gpui::test]` 与普通测试：

- `just fmt`、`just check`、`just test -p lectern-welcome -p lectern-app -p lectern-i18n -p lectern-workspace`、`just doctest`、`cargo metadata --format-version 1 --locked --all-features` 通过：`just test -p lectern-welcome -p lectern-app -p lectern-i18n -p lectern-workspace` 共 269 个测试全部通过（含 `i18n` 的 `reference_tests`，它对照 fluent-bundle 检查新键）。第一次运行里 `lectern-app::startup` 的 `home_path_stays_out_of_the_log` 超时（启动的进程 30 s 内没有退出，与本 PR 无关的图形会话测试），重跑两次都通过。
- `just test -p lectern-welcome`：81 个测试（原 76 个；`the_remove_button_removes_its_row_and_does_not_open_it` 换成 `the_context_menu_selects_its_row_and_does_not_open_it`，新增 `the_columns_follow_the_canvas` 与 `src/tests/selection.rs` 的 4 个文字测试）。
- 四个语义树快照逐个看过，只有预期的变化：标题从「Welcome to Lectern」变成「Lectern」，`welcome.language` 的 `TabBar` 和三个 `Tab` 变成一个带 `value` 的 `Button`，每个最近项目行的 `….remove` 按钮消失，两个 `List` 节点没有 `Label`。
- `the_columns_follow_the_canvas` 量了 820 × 520 下节点的边界：Open Folder 卡片 300pt 宽且在左栏（434.1pt）内居中，语言行在左栏底部，行在右栏内且右边距 10pt，行距 4pt。
- 第 2 步的几条 `rg`（颜色与动画参数、界面字符串字面量、`Text::verbatim(Arc::from("`）仍没有结果。

没有验证的：

- 视觉效果：应用图标的渐变与阴影、字重与字号、强调色选中行、半透明度、`under_page_background` 与分隔线、与画布的逐像素对照；`headless` 的测试不画像素。
- 系统菜单：测试平台上 `show_menu` 不弹出 `NSMenu`，语言菜单的勾和右键菜单的弹出位置没有看到；菜单项只经 `Window::dispatch_action` 触发。
- 在 macOS 上运行发布构建和 `just probe`；空闲内存、冷启动（首帧之前本 crate 仍没有代码运行）。
- 屏幕阅读器读到的无障碍属性。

## 与设计的差异

### 第 1 步

- **屏幕的载荷是占位类型。** `ui::contrib::Screen` 里是 GPUI 的视图工厂，第 1 步没有 GPUI。`welcome` 在 `activate` 里照常经 `dyn Contributions` 登记 `Screen(WELCOME)`（`ScreenLayout` 按「界面」），载荷是本 crate 的空类型 `ScreenPlaceholder`，第 2 步换成 `ui::contrib::Screen`；在那之前 `app` 不装配这两个插件；第 2 步已换成 `ui::contrib::Screen`，`app` 随后装配（前置条件 C）。`KeyBindings` 的载荷是 GPUI 的 `Action`，第 1 步不登记。
- **`welcome-discovery` 在 `started` 里置 `Pending`，不在 `activate` 里。** `activate` 不改模型，激活失败时模型不会留下一个永远不结束的 `Pending`；`activate` 到 `started` 之间的几毫秒仍是 `Unavailable`。
- **合并的计数。** 「排队中 + 脏标记」每一轮在取输入时清掉脏标记，所以任务开始之前到达的三次事件只跑一轮，运行中到达的再多跑一轮（设计原来写「三次事件只跑两次」）。写文件同理。两者的保证不变：任一时刻最多一个任务，取输入之后到达的变化一定有下一轮。「数据流与线程」第 3 步和「测试」已改。
- **`discover::build` 的参数。** 取 `&[Arc<SessionList>]`（每个来源一个）、`&[ProjectKey]`（已打开）、`&[ProjectKey]`（最近项目的 key）和 `Timestamp`，返回 `Built`（发现列表、按 `key` 的计数、会话总数、跳过数）；设计写的是 `(sessions, open_projects, recent, now)`。多个来源的会话先按 `last_activity` 从新到旧稳定排序再分组。
- **`create_dir_all`。** 写文件前建 `data_dir` 用 `create_dir_all`，不是只建一级：`app` 传入的是 `app_data()` 下的 `welcome/`，`app_data()` 本身可能还不存在。
- **不依赖 `futures`、`lectern-ui`、`lectern-i18n`、`lectern-platform`、`gpui`。** 模型的 `subscribe` 收一个回调，不需要 channel（第 2 步的视图自己把回调接到 `mpsc`）；其余几个 crate 的用处都在视图和语言选择里，随第 2 步加入。`Cargo.toml` 的 `description` 随之改成不提窗口。
- **`recent entry skipped` 每个跳过的条目记一条**（设计只写了字段 `reason`），每次写文件重复记，由日志的限流兜住。
- `RecentList` 在设计里既是文件内容又是模型里的列表；实现里文件内容是私有的 `RecentFile { version, entries }`，模型里是 `Vec<RecentEntry>` 加 `loaded` 标记和变更队列。
- 测试用的钩子：`Stats` 的 `during_write`、`during_rebuild` 只在 `#[cfg(test)]` 下编译，让测试在任务进行到一半时插入一次变更。

### 第 2 步

- **根节点是 `welcome.root`，不是 `welcome`。** `ui::semantic::NodeId::new(view, area, key)` 总是产生 `view.area`，没有只有一段的构造函数；不为此改 `ui`。
- **行的名字和计数放在子节点 `….name`（`Role::Text`）上。** `ui::SidebarRow`（第 2 步时是 `ui::Row`，#283 的重绘换了）自己声明一个只有 `selected` 的 `ListItem`，没有 `Label` 和 `attrs` 的接口，同一个 ID 不能声明两次，所以 `Label` 和 `kind` / `sessions` / `running` / `missing` / `agents` 在行里的名字文字上。「界面」的节点表已按这个写。若 `ui` 以后给 `SidebarRow` 加上 `label` / `attr`，这两个子节点可以并回去。
- **`welcome.language` 没有 `Label`**（第 2 步，已取代）：当时的 `ui::Segmented` 声明的 `TabBar` 不带 `Label`。#283 的重绘把语言选择换成 `ui::PopUpButton`，它是带 `Label`（`welcome-language`）和 `value` 的 `Button`，这条差异不再存在。
- **`SelectLanguage` 手写 `Action` 的 `build`。** `#[derive(Action)]` 给带数据的 `Action` 从 `Deserialize` 和 `JsonSchema` 构造，`Locale` 两个都没有（`no_json` 的话 `App::build_action` 返回错误，探针按名字触发不了它），`schemars` 也不是工作区依赖。所以手写 `Action` 的实现并 `register_action!`，`build` 认 `{"locale": "en" | "zh-CN" | null}`（缺省或 `null` 是跟随系统）。若以后 `Locale` 有了 serde，可以换回 derive。
- **`↑` 没有选中时选最后一项**（设计只写了 `↓` 的情形）。产品上没有别的要求，是默认值。
- **移除按钮带文字**（第 2 步，已取代）：#283 的重绘去掉了移除按钮，移除只有 ⌫ 和行的右键菜单；`Button` 的只有图标形态、隐藏与悬停显示的写法都不再用。Open Folder 的 `.tooltip` 键仍没有用（操作卡片没有提示的接口），`welcome-recent-remove` 的 `.tooltip` 也一样；两个键留在资源里，`i18n` 的检查不要求删掉。
- **点击、右键菜单的 `select` 和 ⌫ 按路径找回行，不按下标。** 行里的闭包带着 `path`，点击时在当前快照里重新查找：在画出这一帧之后、点击之前行已经被移除或被遮住时，什么也不打开（`a_click_on_a_row_that_is_gone_does_nothing`），右键的 `select` 也什么都不选。
- **选中的行滚动到可见。** 设计没有写：键盘移动选择后，选中行的元素里放一个 `canvas`，在 prepaint 里把右栏的 `ScrollHandle` 滚到这一行可见（留 `REVEAL_MARGIN`，#283 起为 18 pt），只在移动选择或移除之后做一次。
- **空状态和骨架画在列表标题的下面。** 第 2 步时因为 `GroupedList` 没有行时是一个高度为零的卡片；#283 的重绘没有卡片了，仍是标题、列表节点、空状态文字（或骨架）三层，间距见「#283 的重绘（实现）」。
- **`LanguagePrefs`（`src/prefs.rs`）代替 `platform` 的 `test-support`。** 设计写「假的 `platform` 写入函数经 `test-support` 注入」，`platform` 没有这样的接口（也不应为此加）；视图经一个 crate 内的 trait 调 `platform::system`，`plugins()` 用真实的实现，测试用内存里的假实现。
- **「现在」与主目录经模型和插件传入。** 发现行的「几分钟前」用 `WelcomeModel::now_millis`（模型的时钟，测试里固定），主目录（路径里的 `~`）由 `plugins()` 读一次环境变量 `HOME`，测试传固定值；视图本身不读系统时间和环境。
- **加载阈值用执行器的时钟。** 视图建立时记 `cx.background_executor().now()`，并 `spawn` 一个 `timer(LOADING_SKELETON_AT)` 的任务到点重绘；`ui::loading_phase` 判断阶段，`Skeleton` 与 `Progress` 阶段都显示骨架（设计不显示进度）。
- **`KeyBindings` 贡献的 ID 是 `welcome`**（与 `Screen` 的 ID 相同，种类不同，不冲突）；谓词 `Content > welcome`，视图根元素的 `KeyContext` 是 `welcome`，渲染时只在整个窗口没有焦点时取回焦点（有别的层在上面时不抢）。
- **`futures` 用了**（第 1 步的差异说不依赖它）：视图把模型的唤醒回调接到 `mpsc::unbounded`，前台任务 `await` 接收端。
- **`app` 的装配不在本 PR。** 前置条件 C（`Profile::Desktop` 加这两个插件、`every_profile_graph_validates` 的插件数加二）由另一个 PR 做，见 PR 描述。
- **给 `ui` 的建议**（不在本 PR 做）：`SidebarRow` 的 `label` / `attr`、只有一段的 `NodeId`。第 2 步时提的 `Segmented` 的 `Label`、`Button` 的只有图标形态与提示，因为重绘不再用它们而撤回。

### 为 #283 修订「界面」

- **「界面」按设计画布重写**（[#283](https://github.com/SoloJiang/lectern-app/issues/283)，Refs；实现见下面的「#283 的重绘（实现）」）：应用名从「Welcome to Lectern」改成「Lectern」，加应用图标和版本说明；Open Folder 从主按钮改成操作卡片（图标、标题、说明、⌘O）；右栏从两个分组列表改成侧栏材质上的平铺行（选中用强调色填充，名字半粗）；小标题从「Recent Projects」「From Claude Code and Codex」改成「Recent」「Found in Agent History」；语言选择从三段分段控件改成带「Language」标签的弹出菜单；最近项目行的第二行从「完整路径 · 会话 · 运行中」改成「文件夹 · 会话 · 运行中 · 上级目录」；发现项的第二行去掉路径、点名 agent；移除按钮去掉，改成 ⌫ 和右键菜单。
- **当时仍按已记录的默认**：「未决问题」1（不显示待审数）和 2（不显示 Clone）在这次修订时没有变。2026-10-29 产品负责人改为按画布（显示待审数；显示禁用的 Clone 卡片），见「待审数」「界面」和「验收」第 4 步。
- **「界面」之外的几处**（范围、依赖、性能、数据流、对其他 crate 的要求）只改了与新控件一致的措辞。第 2 步里关于移除按钮、`Segmented`、`GroupedList` 的差异描述的是第 2 步时的实现，实现 PR 已经更新（见上面各条）。
- **语言菜单保留三项**（跟随系统、English、简体中文）：画布的菜单只画了两项，「跟随系统」是行为（[交互规范「语言」](interaction.md#语言)），不是视觉，所以保留。

### #283 的重绘（实现）

[#283](https://github.com/SoloJiang/lectern-app/issues/283)，`Refs`。叠在 `ui` 第 5 步之上。「界面」写的都已实现，下面是实现与文字不同或文字没有写到的地方。

- **Fluent 的方向隔离符。** `$version`、`$agent` 这类字符串参数在 Fluent 的结果里被 U+2068 / U+2069 包住（双向文字的隔离，fluent-bundle 的默认行为），屏幕上的文字带着它们，显示没有差别；测试比较文字前先去掉。
- **多一个 Fluent 键 `welcome-open-folder-shortcut`（`⌘O`，en 与 zh-CN 相同）。** 「界面」的表没有它。快捷键提示放进 Fluent 而不是 `Text::verbatim`，是因为界面字符串只走 `Text` / Fluent，`review-open-file` 的 `⇧⌘O` 也在资源里。它与 `workspace::OpenFolder` 的 ⌘O 绑定要人工保持一致。
- **发现项的 agent 名取 `project-home-agent-claude-code`、`project-home-agent-codex`，不新增键。** `welcome-row-agent-sessions` 的 `$agent` 对 Claude Code 和 Codex 用这两个已有的产品名消息，别的 `Agent::Other(name)` 用它自己的名字。代价是 `welcome` 的界面字符串依赖 `project-home` 的两个键名（都在 `i18n`，不是 crate 依赖）；以后若两处要不同的写法，再各自加键。
- **版本由 `app` 在编译期截取。** `profile.rs` 的常量 `APP_VERSION` 是 `concat!(env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"))`，不是对 `CARGO_PKG_VERSION` 做字符串解析；结果相同。工作区版本现在是 `0.0.0`，所以副标题现在显示「Version 0.0 · …」。
- **探针节点。** `welcome.recent`、`welcome.discovered` 的 `List` 由视图自己声明（原来是 `GroupedList` 声明的），没有 `Label`（原来的标题文字作 `Label`；「界面」的表本来写「—」）；列表标题是普通文字，不是节点。`welcome.language` 是 `PopUpButton` 的 `Button`，`Label` 是 `welcome-language`，`attrs.value` 是 `system` / `en` / `zh-CN`；`welcome.language.item[...]` 不再存在。副标题和行的第二行不是节点（表里没有），所以它们的文字由纯函数 `subtitle`、`recent_detail`、`discovered_detail` 的测试覆盖。
- **布局的数值取画布，表里没有写的：** 左栏宽度是窗口宽度的 360/680（右栏取余下的，两栏伸缩比 360 : 320，`relative` 宽度而不是 flex 比例）；左栏内边距上 28、两侧 32、下 32pt，块间距 22pt，图标上方再空 24pt（画布里那里是红绿灯那一行，真实窗口的标题栏不画它），标题与副标题间 4pt，卡片上方再空 8pt；右栏内边距上下 16、两侧 10pt，行间距 4pt，标题两侧内缩 8pt，上方 4pt（第一组）和 14pt（第二组）。应用名用 `TextRole::LargeTitle`（26pt，画布是 30px）。
- **标题的半粗用 `font_weight(FontWeight::SEMIBOLD)` 叠在 `TextRole::Caption` 上。** `TextRole` 没有「11pt 半粗」；字重不是字体名，验收的 `rg` 不受影响。选中行的名字和第二行的颜色在 `text_role` 之后用 `on_accent()`（第二行是它的 85% 不透明度，`Hsla::opacity`），因为 `text_role` 自己设了 `label` / `secondary_label`。
- **右键先选中。** 最近项目行的 `context_menu` 的 `select` 回调只移动选择（`select_row`），不打开；菜单项是 `RemoveSelected`，作用于选中的行。发现项没有菜单。行已不在列表里时 `select_row` 什么也不做。
- **没有改 `ui`。** 重绘用的都是 `ui` 第 5 步已有的控件。
- **`i18n` 的 `terms_are_expanded` 测试换了键。** `welcome-title` 现在就是 `{ -app-name }`，不再是「Welcome to …」，测试改用 `welcome-open-failed-permission`（也含 `-app-name`）。没有删任何键；`welcome-open-folder` 与 `welcome-recent-remove` 的 `.tooltip` 不再有调用方，留着。
- **Clone 卡片没有画**（「未决问题」2 的默认）；`welcome-clone` 的值按表改成「Clone Repository…」，`welcome-clone-hint` 已加，两者仍没有调用方。
