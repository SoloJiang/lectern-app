# welcome 设计

状态：已接受，尚未实现。

对应 issue [#29](https://github.com/SoloJiang/lectern-app/issues/29)。`welcome` 是没有打开项目时的屏幕（[交互规范「屏幕」](interaction.md#屏幕)画板 01，[ADR 0012](../adr/0012.md)）：左侧是应用名、Open Folder（⌘O）和语言选择，右侧是最近项目，以及从 Claude Code 与 Codex 的会话历史里发现、还没有打开过的文件夹。它作为一个屏幕贡献给 [workspace](workspace.md)（`Screen(WELCOME)`），由 `workspace` 在没有项目时显示；打开项目、读会话目录、画窗口容器都不在这里。

## 范围

- 做：
  - `Screen(WELCOME)` 的视图：标题、Open Folder 按钮、语言选择、最近项目列表、发现的文件夹列表、打开失败的提示。
  - 最近项目列表：存在本 crate 自己的文件里（`platform::dirs::app_data()` 下的 `welcome/recent.json`），首帧之后在 executor 的后台任务里读写；项目打开成功时更新；用户可以移除一条。
  - 发现的文件夹：从 `dyn SessionSource::sessions()` 的会话列表按 `Place` 归并出还没有打开过的项目根目录，带 agent、会话数、运行中数和最近活动时间；`SessionsChanged`、`ProjectEvent` 和最近项目变化时重算。
  - 打开：最近项目、发现的文件夹、Open Folder 都经 `Contributions::dispatch` 交给 `workspace`，本 crate 只监听 `ProjectEvent::Opened` 的结果。
  - 语言选择：跟随系统 / English / 简体中文，选择立即生效，并经 `platform` 写入应用的语言偏好。
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
  - Clone：需要 `git clone` 的进度、目标目录选择和 forge 登录，另开 issue；`welcome-clone` 的 Fluent 键保留给它（见「未决问题」2）。
  - 最近项目行的待审数：需要为没有打开的项目算审阅概览（`Review::overview` 只对关注中的已打开项目计算，[review 设计](review.md)），本 issue 只显示会话数和运行中数（见「未决问题」1）。
  - 每个项目一个窗口时的 Welcome 窗口（[workspace 设计「未决问题」](workspace.md#未决问题)1）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui` | 视图、`FocusHandle`、Action、`cx.spawn` 的前台任务 | 已锁定（[ADR 0016](../adr/0016.md)） |
| `lectern-ui` | `Text`、`strings`、`set_locale`、`colors`、`TextRole`、`Button`、`Segmented`、`GroupedList`、`Row`、`AgentDot`、`Icon`、`semantic`、`ui::contrib` 的载荷类型 | 视图层的公共库，非插件；颜色和动效只从这里取 |
| `lectern-i18n` | `keys::welcome::*`、`keys::common::*`、`Locale`、`Args` | [ADR 0013](../adr/0013.md) |
| `lectern-kernel`、`lectern-services` | `Plugin`、`PluginHandle`、`ServiceRef<dyn Contributions>`、`ServiceRef<dyn Projects>`、`ServiceSet<dyn SessionSource>`；`WorkspaceCommand`、`ProjectEvent`、`SessionsChanged`、`Place`、`ProjectKey` | 架构规则 7（[ADR 0025](../adr/0025.md)）：插件只经 `services` 的接口往来 |
| `lectern-executor` | 读写最近项目文件、重算发现列表的 `Background` 任务 | UI 线程不做 IO；实例由 `app` 创建，经 `WelcomeDeps` 传入 |
| `lectern-platform` | `system::language_override`、`system::set_language_override`（前置条件 B） | macOS 专属能力只在 `platform`（架构规则 6） |
| `lectern-logging` | 「日志与诊断」的宏；测试用 `test-support` 的 `testing::capture()` | [ADR 0046](../adr/0046.md) |
| `serde`、`serde_json` | `recent.json` 的序列化 | 已在工作区依赖里 |
| `futures`（`channel` feature） | 唤醒视图的 `mpsc::unbounded`（见「数据流与线程」） | `workspace` 已把它加进 `[workspace.dependencies]`，不新增 crate |
| `lectern-kernel`（`test-support`，dev）、`insta`（dev） | `TestKernel`、语义树快照 | |

本 crate 是插件 crate（`[package.metadata.lectern]` 加 `plugin = true`，[ADR 0025](../adr/0025.md)），含两个插件，都不提供服务，都是 `AfterFirstFrame`：`welcome` 依赖 `dyn Contributions`、`dyn Projects`；`welcome-discovery` 依赖 `dyn Projects`、`dyn SessionSource`。两者共用一个 `Arc<WelcomeModel>`（`app` 用 `plugins(deps)` 一次构造）。本 crate 不依赖 `workspace`、`project`、`agents` 这些插件 crate；只有 `app` 和 tool 层依赖它。

## 接口

### 本 crate 的公开项

```rust
pub struct WelcomeDeps {
    /// 由 `app` 创建并传入。
    pub executor: lectern_executor::Executor,
    /// 最近项目文件所在目录：`app` 取 `platform::dirs::app_data()` 下的 `welcome/`
    /// （`Resources::app_data` 没有值时 `app` 不装配任何插件，[app 设计「装配」](app.md#装配)）。
    pub data_dir: PathBuf,
}

/// 两个插件：ID "welcome" 与 "welcome-discovery"，共用一个 `Arc<WelcomeModel>`。`app` 逐个放进 `PluginSet::with`。
/// 只保存依赖、建空的 `WelcomeModel`，不做 IO。
pub fn plugins(deps: WelcomeDeps) -> [(Name, WelcomePlugin); 2];
pub struct WelcomePlugin { /* 角色（Screen / Discovery）, deps, model: Arc<WelcomeModel> */ }

// "welcome"：Manifest { provides: [], requires: [service::<dyn Contributions>(), service::<dyn Projects>()], activation: [AfterFirstFrame] }
//   activate（kernel 线程）：取 ServiceRef<dyn Contributions>、ServiceRef<dyn Projects>；`ctx.on::<ProjectEvent>`；
//     `Contributions::register(Screen(WELCOME), None)`、`register(KeyBindings, None)`（`activate` 里没有 token）的守卫交给 `ctx.own_staged`。不做 IO。
//   started（kernel 线程）：`own_with` 提交读 `recent.json` 的 Background 任务（「数据流与线程」第 1 步）。
// "welcome-discovery"：Manifest { provides: [], requires: [service::<dyn Projects>(), service::<dyn SessionSource>()], activation: [AfterFirstFrame] }
//   activate：取 ServiceRef<dyn Projects>、ServiceSet<dyn SessionSource>；`ctx.on::<SessionsChanged>`、`ctx.on::<ProjectEvent>`；
//     把模型的 `discovered` 置为 `Pending`。不做 IO。
//   started：`own_with` 提交第一次重算（第 3 步）。
//   `Active` drop（停用：会话来源的最后一个提供者停用、本插件失败）：`discovered` 置为 `Unavailable`，换快照并唤醒。

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
}
```

不变量：

- `path` 不重复。项目打开成功时，同一路径的旧条目移到最前并更新 `last_opened`、`name`、`key`；超过 10 条时去掉最旧的。同一个 `key` 可以有多条（在不同 worktree 或子目录里打开的同一仓库），各自回到自己的位置。
- 启动时不 `stat` 任何条目（不做遍历）：路径是否还存在只在用户打开时才知道。
- 单一写入者：模型里的 `RecentList` 带 `loaded` 标记，第 1 步读完文件才置位。置位之前到达的改动（`Opened`、`remove_recent`）只记进一个待应用的变更队列，不改列表、不写文件；第 1 步读完后先按顺序应用它们，再写一次。写文件用与发现列表相同的「排队中 + 脏标记」：任何时刻最多一个写任务，它在运行时序列化模型里当时最新的列表，结束时发现脏标记就再写一次。所以不会出现空列表盖掉文件、两个任务同时写 `recent.json.tmp`、旧列表写在新列表之后这三种情况。
- 文件格式：JSON，一个对象；字段不认识时忽略；`path` 用 `PathBuf` 的 serde 表示（非 UTF-8 的路径写不进去，这样的条目不写，记 warn）。样本在 `crates/welcome/tests/fixtures/recent_v1.json`。

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

归并规则（`discover::build(sessions, open_projects, recent, now)`，纯函数）：

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
2. 记录打开成功（`welcome` 对 `ProjectEvent::Opened { Ok(key) }` 的监听安排，`Background`）：`Projects::snapshot(key)` 在 `Opened` 时已有第 1 代（[project 设计「事件」](project.md#事件)），从它取 `path`、`key`、`name`；清掉 `opening`，更新内存里的 `RecentList`（`loaded` 还没置位时只进变更队列），先换快照、再置写入的脏标记：写任务（同一时刻最多一个）在 `data_dir` 下写 `recent.json.tmp`，`rename` 成 `recent.json`；`data_dir` 不存在时先 `create_dir_all`（只建这一级）。写失败记 warn，内存里的列表照常使用。这一步与 `Opened` 来自 `restore`、⌘O 还是本屏幕无关：任何成功打开的项目都进入最近项目，所以恢复上次项目时它也在列表最前。最后置第 3 步的脏标记（`welcome-discovery` 没有激活时标记无人消费，无害）。
3. 重算发现列表（`welcome-discovery`，`Background`，持有 `busy()`）：对 `ServiceSet<dyn SessionSource>::for_each` 的每个来源取 `sessions()`（取锁复制 `Arc`），取 `open_projects()` 和模型里当前的 `RecentList`，调 `discover::build`，把发现列表换进快照，并把算出的计数按 `key` 并进模型里当时的 `RecentList`（不用任务开始时的那份副本替换列表，否则刚移除的条目会短暂回来）。触发：`started`、每次 `SessionsChanged`、`ProjectEvent::Opened` / `Closed`、最近项目变化（第 2、6 步置的脏标记）。合并：模型里有一个「排队中」标记，已经排了一个任务时后续触发只置脏标记，任务结束时发现脏标记就再跑一次，所以发现阶段的连续事件不会堆积任务。
4. 唤醒视图：`WelcomeModel` 持有 `futures::channel::mpsc::unbounded` 的发送端（`subscribe` 登记）；视图建立时 `cx.spawn` 一个前台任务循环 `await` 接收端，每收到一项就 `this.update(cx, |_, cx| cx.notify())`。发送端的 `wake` 经 GPUI 的平台分发器唤醒主线程（与 [workspace 设计「数据流与线程」](workspace.md#数据流与线程)的主线程队列同一机制），任何线程都能发，不轮询。视图 drop 时守卫注销，之后的 `wake` 不做任何事。
5. 打开：最近项目和发现的文件夹：视图在主线程上先 `model.begin_open(path, source)`（清 `notice`、记 `opening`），再 `dispatch(WorkspaceCommand::OpenProject(path))`；`opening` 有值期间，视图对同一路径的再次 ↵ 或点击不发出命令，对其他路径照常（`workspace` 自己排队）。Open Folder 按钮：`model.clear_notice()` 后 `dispatch(PromptOpenProject)`（⌘O 是 `workspace` 的全局绑定，不经本视图，只有 `workspace` 的流程）。`workspace` 问守卫、调 `Projects::open`，`Opened { Ok }` 到达后换基底屏幕、drop 本视图，本 crate 的监听同时清 `opening`（第 2 步）。`Opened { Err(e) }` 时 `workspace` 停在 Welcome 并记 warn，本 crate 的监听把 `Notice::OpenFailed(e 的种类)` 放进快照、清 `opening`；`opening` 有值并且 `e` 是 `NotFound` / `NotADirectory` 时，`path` 等于 `opening.path` 的最近项目标 `missing`。`Opened` 不带路径（[project 设计「事件」](project.md#事件)只有 `OpenRequest`），所以本屏幕发起的打开同一时刻只能有一个在进行，`opening` 就是那一个；不是本屏幕发起的失败（`restore` 的路径已不存在、⌘O 选的目录不可读）只显示提示，不标 `missing`。
6. 移除最近项目：视图调 `model.remove_recent(path)`：在调用线程（主线程）上只改内存里的列表并换快照（常数时间，不做 IO；`loaded` 还没置位时只进变更队列），然后置写入的脏标记：没有写任务在排队时用 `started` 存下的 `PluginHandle::own_with` 排一个 `Background` 写任务（同第 2 步；`welcome` 已停用时 `own_with` 返回 false，不写，视图这时也已经被 `workspace` 撤下），再置第 3 步的脏标记（被遮住的发现项可能重新出现）。
7. 语言选择：视图在主线程上调 `lectern_ui::set_locale(locale, cx)`（立即换全局 `Strings`、刷新窗口），再调 `platform::system::set_language_override(tag, done)`（立即返回，写入在 `platform` 的串行队列里，[i18n 设计「数据流与线程」](i18n.md#数据流与线程)）；`done` 收到 `Err(WriteFailed)` 时调 `model.language_not_saved()`（`Superseded` 是被更新的选择顶掉，不提示），视图显示 `welcome-language-not-saved`（i18n 设计「状态与异常」要求的一次提示）。选「跟随系统」时 `locale = Locale::negotiate(platform::system::system_languages())`，`tag` 为 None。分段控件的初始选项来自 `platform::system::language_override()`（一次内存里的偏好读取，主线程，视图建立时读一次），有值时用 `Locale::negotiate([tag])` 转成 `Locale`。
8. UI 线程上不做 IO、解析和 git 操作：视图只调 `snapshot()`（取锁复制）、`dispatch`、`begin_open` / `clear_notice` / `remove_recent`（改内存）、`set_locale`、`language_override`（内存里的偏好）。所有 `std::fs` 调用都在 `src/recent/store.rs`，只从 `Background` 任务调用（验收用 `rg` 检查）。
9. 派生数据：没有以内容为键的派生数据（架构规则 5 不适用）。发现列表以会话列表的代数为准：任务开始时记下每个来源的 `generation`，结束时有更新的来源就按第 3 步的脏标记再跑一次。
10. `welcome-discovery` 停用（会话来源的最后一个提供者停用、本插件失败）：kernel 先撤销监听、关闭闸门（排队中的重算丢弃），最后 drop `Active`，它把 `discovered` 置为 `Unavailable`、换快照并唤醒；列表显示空状态。重新激活时回到 `Pending`。

## 性能

- 冷启动（[ADR 0017](../adr/0017.md)）：首帧之前本 crate 没有代码运行。首帧是 `app` 的中性骨架；`workspace::install` 在屏障放开后替换根视图，本 crate 的插件在 kernel 线程上激活，`Screen` 贡献应用后的下一帧才画出 Welcome。`plugin/welcome/activate`、`plugin/welcome-discovery/activate` 各 < 1 ms（取服务引用、登记贡献或监听，不做 IO）。
- Welcome 的第一次渲染（屏障放开后的第 2–3 帧）：只有标题、一个按钮、一个分段控件、两个标题行和两条空状态文字，从视图自己的状态画出，不等任何后台任务：排版十几段文字，< 1 ms，远在一帧 8.3 ms 之内。列表在后台任务完成后填入：最近项目在第 1 步后（读文件 < 1 ms，加任务调度，用户可感知为与首帧同时出现）；发现的文件夹在 `agents` 第一次发布会话列表之后（[agents 设计「性能」](agents.md#性能)：1000 个会话 < 500 ms）。按[交互规范「状态与异常」](interaction.md#状态与异常)，100 ms 内不显示加载态，所以两个列表在到达之前只显示标题、不显示骨架；超过 100 ms 还没到时显示 2 行骨架（`ui::Skeleton`），由 `ui::loading_phase` 判断。
- 重算发现列表：`discover::build` 对 1000 个会话约 0.3 ms，10 000 个约 3 ms（一次哈希分组加一次排序），在 `Background` 线程上，持有 `busy()` 守卫；发现阶段最多再跑一次（脏标记合并），不让 kernel 长时间忙碌。
- 空闲内存：快照最多 10 条最近项目加 50 条发现项（每条约 200 字节），模型和视图合计 < 64 KiB；本 crate 不保留 `SessionList` 的 `Arc`（任务结束即放下），不保留 `ProjectSnapshot`。
- 帧时间：`render` 只克隆一个 `Arc` 和构造至多 60 行的元素；悬停和选中用 `ui::Row` 的动画（`HOVER_IN` / `HOVER_OUT`），不自定义动画参数。
- 基准：两个插件的 `plugin/*/activate`、`plugin/*/resident` 随 bench 的插件基准自动覆盖；不新增进程内基准。`app/cold_start` 不受影响（首帧前没有本 crate 的代码）。

## 界面

- 屏幕与层：[交互规范「屏幕」](interaction.md#屏幕)画板 01，常驻层；没有侧栏、工具栏、检查器、浮条（`ScreenLayout { sidebar: Hidden, back_button: false, reading_column: false, inspector: false, floating_bar: false }`）。窗口 820 × 520 pt（[app 设计「界面」](app.md#界面)）。布局：左栏 300 pt（应用名 `TextRole::LargeTitle`、一行副标题、Open Folder 主按钮、底部的语言分段控件），右栏为剩余宽度，两个分组列表（`ui::GroupedList`）上下排列，各带标题行；右栏整体可滚动。
- `ui` 的控件与动效原语（名字以 `ui` 设计「控件」为准，PR [#218](https://github.com/SoloJiang/lectern-app/pull/218)）：`Text`（全部文字）、`Button`（Open Folder：`Primary`、`Regular`、`Icon::Folder`；移除：`Plain`、`Small`、`Icon::Close`）、`Segmented`（语言）、`GroupedList` + `Row`（两个列表）、`AgentDot`（发现项的 agent 圆点）、`Skeleton`（加载态）；颜色经 `colors(cx)`，字号经 `TextRole`。本 crate 不写任何颜色值、字体名和动画时长（验收用 `rg` 检查）。
- 每一行的内容：
  - 最近项目：第一行 `name`（`TextRole::Body`），第二行次要文字：`path`（主目录写成 `~`，`Text::verbatim`）；`sessions > 0` 时在第二行末尾加 `welcome-row-sessions` 和（`running > 0` 时）`welcome-row-running`；`missing` 时第二行换成 `welcome-recent-missing`。行尾悬停或选中时显示移除按钮。会话数和运行中数来自与发现列表同一次 `discover::build`（按 `key` 匹配，见「发现的文件夹」）；`welcome-discovery` 没有激活时为 0，不显示。
  - 发现的文件夹：行首 `AgentDot`（每种 agent 一个，最多 4 个），第一行 `name`，第二行 `path`、`welcome-row-sessions`、`welcome-row-running`、`Strings::relative_time(now - newest_activity)`。
- Fluent 键（`crates/i18n/locales/{en,zh-CN}/welcome.ftl`；已有的 6 个键保留，新增的在前置条件 A 里加）：

  | 键 | en | zh-CN | 用途 |
  |---|---|---|---|
  | `welcome-title` | Welcome to { -app-name } | 欢迎使用 { -app-name } | 左栏标题（已有） |
  | `welcome-subtitle` | Open a folder to see what your agents changed. | 打开一个文件夹，看 agent 改了什么。 | 标题下的一行次要文字 |
  | `welcome-open-folder`（`.tooltip`） | Open Folder… | 打开文件夹… | 主按钮（已有） |
  | `welcome-clone` | Clone… | 克隆… | 保留给 Clone 的 issue（已有，本 issue 不显示） |
  | `welcome-recent` | Recent Projects | 最近的项目 | 列表标题（已有） |
  | `welcome-recent-empty` | No recent projects | 没有最近的项目 | 空状态（已有） |
  | `welcome-recent-missing` | Folder no longer exists | 文件夹已不存在 | `missing` 行的第二行 |
  | `welcome-recent-remove` | Remove from Recents | 从最近的项目移除 | 移除按钮的辅助名与 `.tooltip` |
  | `welcome-discovered`（`.tooltip`） | From Claude Code and Codex | 来自 Claude Code 与 Codex | 列表标题（已有） |
  | `welcome-discovered-empty` | No agent sessions found yet | 还没有发现 agent 会话 | 空状态 |
  | `welcome-row-sessions` | { $count -> [one] { $count } session *[other] { $count } sessions } | { $count } 个会话 | 行的次要文字 |
  | `welcome-row-running` | { $count } running | { $count } 个运行中 | 行的次要文字 |
  | `welcome-language` | Language | 语言 | 分段控件的辅助名 |
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
  | `welcome` | `Group` | `recent`（`loading` / `ready`）、`discovered`（`unavailable` / `pending` / `ready`）、`opening`（`true` 时才有） |
  | `welcome.title` | `Text` | `Label::key = welcome-title` |
  | `welcome.open-folder` | `Button` | `Label::key = welcome-open-folder` |
  | `welcome.language` | `TabBar` | `Label::key = welcome-language` |
  | `welcome.language.item[system]`、`[en]`、`[zh-CN]` | `Tab` | `State::selected`；`Label::key` 是对应的 `common-language-*` |
  | `welcome.notice` | `Text` | `Label::key` 是 `welcome-open-failed-*` 或 `welcome-language-not-saved`；没有提示时不出现 |
  | `welcome.recent` | `List` | — |
  | `welcome.recent.empty` | `Text` | `Label::key = welcome-recent-empty`；列表非空时不出现 |
  | `welcome.recent.item[<path>]` | `ListItem` | `Label` 是 `name`（`key` None）；`State::selected`；`attrs`：`kind`（`git` / `folder`，取自 `key` 的变体）、`sessions`、`running`、`missing`（`true` 时才有） |
  | `welcome.recent.item[<path>].remove` | `Button` | `Label::key = welcome-recent-remove` |
  | `welcome.discovered` | `List` | — |
  | `welcome.discovered.empty` | `Text` | `Label::key = welcome-discovered-empty` |
  | `welcome.discovered.item[<path>]` | `ListItem` | `Label` 是 `name`；`State::selected`；`attrs`：`agents`（`claude-code,codex`）、`sessions`、`running` |

  `<path>` 是条目的 `path`（业务键，不是下标）。骨架块不声明节点。

- Action 与快捷键（都能被探针按名字触发；经 `KeyBindings` 贡献登记，谓词 `Content > welcome`，视图的根元素设 KeyContext `welcome` 并持有焦点）：

  | Action | 键 | 处理 |
  |---|---|---|
  | `welcome::SelectNext` | ↓ | 选择线性序列里的下一项；没有选中时选第一项；已是最后一项时不动 |
  | `welcome::SelectPrevious` | ↑ | 上一项；在第一项时不动 |
  | `welcome::OpenSelected` | ↵ | 对选中项 `begin_open` 后 `dispatch(OpenProject(path))`；没有选中、或 `opening.path` 就是它时不做任何事 |
  | `welcome::RemoveSelected` | ⌫ | 选中的是最近项目时 `remove_recent(path)`，选择移到同位置的下一项（没有则上一项）；选中的是发现项时不做任何事 |
  | `welcome::SelectLanguage { locale: Option<Locale> }` | — | 分段控件的点击；探针用它切换语言 |

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
- 详细级别：debug `snapshot rebuilt`（`generation`、`recent`、`discovered`、`sessions`（归并前的会话总数）、`skipped`（`Pending` 等跳过的数量）、`took_us`）；debug `recent list written`（`entries`、`bytes`）；debug `recent entry removed`；debug `selection`（`list`、`index`）；debug `locale selected`（`locale`：`system` / `en` / `zh-cn`）。
- 关联 ID：`plugin`（`welcome` 或 `welcome-discovery`）、`act` 由 kernel 在监听和 `started` 外设置，`task` 由 executor 设置；主线程上的视图代码在 `workspace` 调用视图工厂时恢复的作用域里运行（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 11 项），Action 处理函数不在作用域里，只带 target。不设 `session`：本 crate 不记任何与单个会话有关的记录。
- 不记录：最近项目和发现项的路径与名字（`recent.json` 的内容、`RecentEntry::path`、`DiscoveredFolder::path`、`ProjectSnapshot::name`）、会话的 cwd 与标题、`OpenError::RepoUnreadable` / `Io` 附带的文字、`data_dir` 的路径、语言偏好之外的任何系统偏好。用条数、种类、代数和耗时代替。`workspace` 同样不记项目路径，所以日志里找不到用户打开过哪些目录。
- 金丝雀测试：`canary_stays_out_of_welcome_logs`（`src/tests/logs.rs`）：`lectern_logging::testing::capture()` 在 trace 级别，用含金丝雀的路径与项目名构造 `recent.json`（含一条坏行触发 warn）、假 `Projects` 的快照、假 `SessionSource` 的会话（cwd、标题、`rel` 里都有金丝雀），跑完加载、`Opened`、重算、移除、写文件（写到一个不可写的目录触发 `recent list not written`）、`Opened { Err(RepoUnreadable(金丝雀)) }`，`assert_absent(金丝雀)`。

## 测试

依赖 GPUI，只在 macOS 上编译和测试：`just test -p lectern-welcome`。

- 纯函数（`#[test]`，不依赖 GPUI）：
  - `recent::push_front_dedupes_by_path_and_caps_at_10`、`recent::remove_keeps_order`、`recent::decode_rejects_unknown_version`、`recent::decode_ignores_unknown_fields`、`recent::encode_roundtrips`（用 `fixtures/recent_v1.json`）、`recent::entry_from_snapshot_joins_root_and_opened_at`（子目录打开、linked worktree、裸仓库三种）。
  - `discover::groups_by_project_and_takes_the_newest_root`、`discover::root_strips_rel_from_cwd`、`discover::skips_open_projects_and_recent_keys`（同一仓库的其他 worktree 里的会话不再出现）、`discover::skips_pending_missing_and_unresolvable`、`discover::running_needs_open_turn_within_10_min`、`discover::subagents_count_as_sessions_not_running`、`discover::caps_at_50`、`discover::recent_rows_get_counts_by_key_from_located_and_resolved`。
- 模型（`#[test]`，`TestKernel` 注入假的 `dyn Projects`、`dyn SessionSource`、`dyn Contributions`，executor 用测试队列）：`welcome_started_loads_the_file`、`welcome_started_records_projects_opened_before_it`（假 `Projects` 在 `started` 之前已有一个打开的项目，`Opened` 没有送达，列表最前仍是它）、`discovery_started_builds_the_list`、`recent_loaded_after_discovery_triggers_a_rebuild`（先发现后读文件，最近项目不留在发现列表里）、`welcome_activates_without_any_session_source`（只装 `welcome`：屏幕已注册，`discovered` 是 `Unavailable`）、`discovery_drop_sets_unavailable`、`opened_records_a_recent_entry_with_its_key_and_writes_the_file`、`opened_without_a_snapshot_records_nothing`、`opened_before_the_file_is_loaded_does_not_overwrite_it`（先 `Opened` 后读完文件：文件里原有的条目都还在，新条目在最前）、`writes_are_serialized_and_coalesced`（三次连续变化只写两次，最后一次写出的是最新列表）、`sessions_changed_rebuilds_once_when_events_pile_up`（三次事件只跑两次）、`remove_recent_updates_the_snapshot_before_writing`、`rebuild_merges_counts_without_restoring_a_removed_entry`、`begin_open_then_error_marks_the_entry_missing_and_clears_opening`、`error_without_opening_only_shows_the_notice`、`wake_is_called_once_per_snapshot`。
- `#[gpui::test]`（`TestAppContext`，`lectern_ui::init_fixed`）：`first_render_shows_title_and_open_folder_before_any_snapshot`（模型还是 `Loading` / `Pending` 时的语义树只有标题、按钮、分段控件和两个列表标题）、`skeleton_appears_after_100ms_and_rows_replace_it`、`click_and_enter_dispatch_open_project_once`（`opening` 期间对同一行再按 ↵ 不重复，失败后可以再试）、`open_folder_dispatches_prompt`、`arrows_move_selection_across_both_lists`、`backspace_removes_the_selected_recent_and_moves_selection`、`open_error_shows_a_notice_and_clears_on_the_next_open`、`discovered_rows_update_on_wake`、`unavailable_discovery_shows_the_empty_state_without_a_skeleton`、`language_picker_switches_strings_and_calls_platform`（假的 `platform` 写入函数经 `test-support` 注入；失败时显示 `welcome-language-not-saved`）、`actions_are_triggerable_by_name`（探针契约）。
- 语义树：`welcome_tree_empty`、`welcome_tree_with_lists`、`welcome_tree_with_error` 的 insta 快照（UI 探针完成后接入；之前用 `semantic::Recorder` 的记录序列化）。
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
- **C. `app`**：`Profile::Desktop` 的表加两行：`lectern_welcome::plugins(WelcomeDeps { executor, data_dir: app_data.join("welcome") })` 的两个插件，在 `workspace` 之后逐个放进 `PluginSet::with`。`Resources::app_data` 已是非可选的 `PathBuf`（[app 设计「装配」](app.md#装配)：`app_data()` 为 None 时 `app` 不装配任何插件），本 crate 不处理没有目录的情况。
- **D. `ui`**：本 crate 只用 `ui` 设计已列出的控件和 `ui::contrib` 的载荷类型，没有新增要求。`Segmented` 的探针角色按 `ui` 的实现（本文按 `TabBar` / `Tab` 写，以 `ui` 为准）。

## 未决问题

1. **最近项目行是否显示待审数。** issue 写「最近项目（待审数、运行中数）」。待审数要对没有打开的项目算审阅概览：读 worktree 的改动、按会话归因（[review 设计](review.md)），每个项目几十到几百毫秒的 git 操作，Welcome 显示 10 个项目就是最多几秒的后台工作，并且会在用户还没选项目之前打开 10 个仓库。默认（本文按它写）：Welcome 只显示会话数和运行中数（都来自已经在内存里的会话列表，没有额外 IO），待审数在打开项目后的项目主页显示；以后 `review` 有廉价的按项目计数时再加一列，行的结构不变。另一个选项：Welcome 对最近项目逐个 `Review::watch_overview`，显示概览里的会话数，接受上述开销。
2. **Clone 按钮在这个 issue 里是否出现。** 交互规范写「Open Folder（⌘O）/ Clone」，但克隆流程（目标目录、进度、forge 登录）没有设计。默认：本 issue 不显示 Clone 按钮，`welcome-clone` 的键保留，Clone 的 issue 把按钮加回左栏。另一个选项：显示一个禁用的 Clone 按钮并在悬停时说明「即将推出」。

## 已知限制

- 运行中的计数只在 `SessionsChanged` 时刷新：一个会话停下来之后，最长要到下一次任何来源的事件才从「运行中」变回普通会话；没有新事件时它一直显示运行中。项目主页实现时如果加了定时刷新，本 crate 复用同一机制。
- 最近项目的路径在启动时不检查是否存在：失效的条目在用户打开失败时才标出。
- `missing` 不写进文件：重启后标记消失，再次打开失败时再标。
- 发现的文件夹按 `ProjectKey` 分组，显示最新会话所在的 worktree 根；同一仓库其他 worktree 里的会话算进同一条，打开后在项目主页里按 worktree 查看。
- 「尚未打开过」只对照最近项目列表（最多 10 条）的 `key`：更早打开过、已经被挤出列表的项目会重新出现在发现列表里。仓库被移动后 `ProjectKey` 变了（[project 设计「项目标识」](project.md#项目标识)），旧条目打开失败标 `missing`，新位置的会话作为新的发现项出现。
- 本屏幕同一时刻只跟踪一个进行中的打开（`opening`）：在 `Opened` 到达之前点第二行，第二次的失败不会标 `missing`。
- 非 UTF-8 的项目路径不写进 `recent.json`（本次运行里仍在列表中）。

## 验收

分两步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-welcome`、`just deny` 通过。前置条件 A、B 各自的 PR 先合并；C 随第 2 步；`workspace` 的第 2 步（根视图与贡献应用）和 `ui` 的实现在第 2 步之前。

**第 1 步：插件、模型、最近项目的存储、发现列表的归并（不依赖 GPUI 的部分，`Refs #29`）。**

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
