# workspace 设计

状态：已接受，尚未实现。

对应 issue [#215](https://github.com/SoloJiang/lectern-app/issues/215)。`workspace` 是窗口的骨架：它把[交互规范「窗口结构」](interaction.md#窗口结构)的六个区域（侧栏、工具栏、内容区、检查器、浮动面板、底部浮条）画成一个根视图，按 [ADR 0012](../adr/0012.md) 的三层结构决定每个区域什么时候出现，维护屏幕之间的导航状态机，并通过 `services` 里的 `dyn Contributions` 接收其他视图 crate 和插件贡献的侧栏条目、屏幕、检查器页、面板、快捷键和离开守卫（[kernel 设计「撤销」「按命令激活」](kernel.md#撤销)）。它不含任何一个屏幕的内容：Welcome 归 [#29](https://github.com/SoloJiang/lectern-app/issues/29)，项目主页与项目侧栏归 [#30](https://github.com/SoloJiang/lectern-app/issues/30)，审阅视图归 [#15](https://github.com/SoloJiang/lectern-app/issues/15)，完整文件阅读与 Files 归 [file_reader 设计](file_reader.md)。

## 范围

- 做：
  - 根视图 `Workspace`：`app` 在首帧之后用它替换骨架窗口的根视图（[app 设计「界面」](app.md#界面)）；首帧之前本 crate 没有任何代码运行（[ADR 0017](../adr/0017.md)）。
  - 区域与三层：常驻层（侧栏、工具栏、内容区）、按需层（检查器、浮动面板、覆盖层、替换侧栏的面板）、收起层（阅读视图等占据内容区的屏幕）各自如何打开、关闭和回到原处；Esc、⌘[、⌥⌘I、⌘\、⌘O、⌘W 的处理。
  - 导航状态机：屏幕（Welcome、项目主页、审阅、阅读视图、Worktrees、Usage）之间的切换，命令与事件。
  - `dyn Contributions`（接口在 `services` 的 `workspace` 模块，本 crate 实现）：注册与撤销的规则按 kernel 设计；按命令激活的代理 Action 与重放；离开守卫的登记与询问。
  - 主线程队列：把其他线程上的注册、命令、事件结果排到主线程，带 `ActivationToken` 与 `PendingGuard`。
  - 跨次运行保留的窗口状态（上次打开的项目、检查器是否显示）的数据结构与恢复顺序；读写由 `app` 做（见「窗口状态」）。
  - UI 探针需要的区域语义节点，bench 需要的里程碑回调。
- 不做：
  - 各屏幕和各区域里的内容：项目侧栏的条目列表与会话列表（`project_home`）、审阅页的简报与叙事（`review_view`）、Welcome 的最近项目（`welcome`）、Files 文件树与阅读视图（`file_reader`）、Conversation 页（`conversation`）、上下文面板的内容（`context_drawer`）。
  - 颜色、字体、控件与动效原语：归 `ui`（[ADR 0015](../adr/0015.md)），本 crate 只调用，见「对其他 crate 的要求」。
  - 窗口位置与大小的保存：AppKit 的 frame autosave（ADR 0017），由 `app` 经 `platform` 设置，见「对其他 crate 的要求」。
  - 窗口状态文件的读写、最近项目列表：`app`（[project 设计「范围」](project.md#范围)把最近项目列表归 `welcome` / `app`）。
  - 应用菜单与 `Quit`：`app`。本 crate 只回答 `Quit` 之前的离开守卫询问。
- 本 issue 之外：
  - 多窗口：第一版一个窗口（见「未决问题」1）。导航状态按窗口保存，扩展到多窗口时不改状态机。
  - 进程插件（[#39](https://github.com/SoloJiang/lectern-app/issues/39)）的界面贡献：贡献的载荷是进程内的闭包，不经 JSON-RPC；进程插件要贡献界面时在 #39 的设计里另定协议。
  - 设置页、语言切换后重建菜单（[i18n 设计「数据流与线程」](i18n.md#数据流与线程)）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui` | 根视图、`replace_root`、`on_window_should_close`、`bind_keys`、`on_next_frame`、`prompt_for_paths` | 已锁定（[ADR 0016](../adr/0016.md)） |
| `lectern-ui` | 布局容器、动效原语、贡献载荷的类型（`ui::contrib`）、当前语言的 `Strings` | 视图层的公共库，非插件 |
| `lectern-i18n` | 本 crate 自己的几条字符串（`workspace.ftl`） | [ADR 0013](../adr/0013.md) |
| `lectern-kernel`、`lectern-services` | `Plugin`、`Staged`、`ActivationToken`、`KernelControl`；提供 `dyn Contributions`，依赖 `dyn Projects` | 架构规则 7（[ADR 0025](../adr/0025.md)） |
| `lectern-logging` | 日志宏、`current()` / `scope()`（主线程上恢复贡献方的作用域） | [ADR 0046](../adr/0046.md) |
| `futures`（`channel` feature） | 主线程队列的 `mpsc::unbounded`：发送方在任何线程，接收方是 GPUI 的前台任务 | 已在 Cargo.lock（GPUI 依赖它），不新增 crate；加进 `[workspace.dependencies]`，`just deny` 通过 |
| `lectern-kernel`（`test-support`，dev）、`insta`（dev） | `TestKernel`、语义树快照 | |

本 crate 是插件 crate（[ADR 0025](../adr/0025.md)）：插件 `workspace`，提供 `dyn Contributions`，依赖 `dyn Projects`，`AfterFirstFrame`。它只依赖 kernel 层和非插件的库 crate，不依赖 `project_home`、`review_view`、`file_reader` 等贡献方；只有 `app` 和 tool 层依赖它。

## 接口

### 服务接口（`lectern-services` 的 `workspace` 模块）

`services` 在 kernel 层，不能依赖 `gpui` 和 `lectern-i18n`，所以这里只有稳定 ID、顺序和布局这类 GPUI 无关的数据；视图工厂、`Action`、Fluent 键放在类型擦除的 `Payload` 里，具体类型由 `ui::contrib` 定义（「`ui::contrib` 的载荷类型」），本 crate 用 `downcast` 取回。数据类型派生 `Clone`、`PartialEq`、`Eq`、`Debug`，`Payload`、trait 和包着回调的类型除外。

```rust
/// 单实现服务，`workspace` 插件提供。
/// impl Service for dyn Contributions { const ID = "contributions"; const MULTIPLE = false; }
pub trait Contributions: Send + Sync {
    /// 登记一项贡献。立即返回，任何线程可调用。`activate` 里调用时把返回的守卫交给 `ctx.own_staged`，
    /// 提交时才生效；`started` 之后调用时按 [kernel 设计「撤销」](kernel.md#撤销)排到主线程应用。
    /// 同一个 `(kind, id)` 已有登记时后来的被拒绝（记 warn，见「日志与诊断」），守卫 drop 时什么也不做。
    /// 守卫 drop 时注销，注销排到主线程，带所属激活的 `token.pending()` 守卫。
    fn register(&self, contribution: Contribution) -> Registration;

    /// 登记一个由 `Command` 激活的命令的处理函数（kernel 设计「按命令激活」第 1 步）。规则同 `register`。
    fn handle_command(&self, name: Name, handler: CommandHandler) -> Registration;

    /// 登记离开守卫（[file_reader 设计「编辑」](file_reader.md#编辑)）。按登记顺序询问。规则同 `register`。
    fn register_leave_guard(&self, guard: Arc<dyn LeaveGuard>) -> Registration;

    /// 向窗口发一个导航或布局命令。立即返回，任何线程可调用；排到主线程执行，
    /// 需要离开守卫的命令先问守卫（见「数据结构」的状态机）。
    fn dispatch(&self, command: WorkspaceCommand);

    /// 当前的侧栏条目（按 `section`、`order`、`id` 排序），供 `project_home` 渲染项目侧栏。
    /// 取锁复制一个 `Arc`，不阻塞；条目增减时换一份新的，`WorkspaceEvent::ContributionsChanged` 通知。
    fn sidebar_entries(&self) -> Arc<[SidebarEntry]>;

    /// 报告一个里程碑状态已经画出（bench 设计「需要其他 crate 提供的接口」）：本 crate 在下一帧回调里
    /// 记下时刻并转给 `app`。任何线程。
    fn report_milestone(&self, milestone: Milestone);
}

/// 贡献：kind 决定 `payload` 必须是 `ui::contrib` 的哪个类型（见「`ui::contrib` 的载荷类型」）。
pub struct Contribution {
    pub kind: ContributionKind,
    /// 稳定 ID，`[a-z0-9_-]`，1 到 64 字节：探针节点、日志和窗口状态都用它。
    pub id: Name,
    /// 同类贡献的显示顺序，小的在前，相同时按 `id`。
    pub order: u16,
    pub payload: Payload,
}

pub enum ContributionKind {
    /// 内容区的一个屏幕。`payload` 是 `ui::contrib::Screen`；`id` 用「屏幕」表里的名字。
    Screen(ScreenLayout),
    /// 替换侧栏内容的面板（项目侧栏本身、Files）。`payload` 是 `ui::contrib::SidebarPanel`。
    SidebarPanel,
    /// 项目侧栏里的一个条目（Overview、Sessions、Worktrees、Files、Usage……）。`payload` 是 `ui::contrib::SidebarEntry`。
    SidebarEntry { section: SidebarSection },
    /// 检查器的一页（Files、Conversation）。`payload` 是 `ui::contrib::InspectorTab`。
    InspectorTab,
    /// 浮在检查器上方的面板（上下文面板）。`payload` 是 `ui::contrib::Panel`。
    Panel,
    /// 覆盖在窗口中央的层（⌘K）。`payload` 是 `ui::contrib::Overlay`。
    Overlay,
    /// 快捷键绑定。`payload` 是 `ui::contrib::KeyBindings`。
    KeyBindings,
}

/// 屏幕对窗口布局的要求，由贡献方声明，状态机据此决定各区域的显示（见「数据结构」）。
pub struct ScreenLayout {
    /// 侧栏：`Shown`（项目主页）、`Hidden`（审阅：「Overview 常驻，审阅时隐藏」）。
    pub sidebar: Visibility,
    /// 工具栏左侧是否显示返回按钮（⌘[）。
    pub back_button: bool,
    /// 内容是否限制在居中的 760pt 阅读栏里；阅读视图为 false（代码按行不折）。
    pub reading_column: bool,
    /// 检查器是否可用（⌥⌘I）。Welcome 和项目主页为 false。
    pub inspector: bool,
    /// 是否显示底部浮条。只有审阅为 true。
    pub floating_bar: bool,
}

pub enum Visibility { Shown, Hidden }

pub enum SidebarSection { Project, Views, Bottom }

/// 类型擦除的载荷。`ui::contrib` 的类型实现 `into_payload`，本 crate `downcast`；
/// 类型不对时拒绝登记（warn）。进程插件不能跨进程传它。
pub struct Payload(pub Box<dyn Any + Send>);

/// 登记的守卫。drop 时注销；实现 `kernel::Staged`，`activate` 里经 `ctx.own_staged` 交出。
pub struct Registration { /* 实现方传入的 commit 与注销回调 */ }
impl Registration {
    pub fn new(commit: impl FnOnce(ActivationToken) + Send + 'static, release: impl FnOnce() + Send + 'static) -> Self;
}

/// 命令处理函数：主线程调用，运行在登记方激活的准入和日志作用域里。
pub type CommandHandler = Box<dyn Fn(&CommandCall) + Send + Sync>;
/// 处理函数收到的调用。`window` 是发起命令的窗口（第一版只有一个）。
pub struct CommandCall { pub window: WindowId, pub name: Name }

/// 离开守卫（file_reader 设计「编辑」的接口，签名照抄）。`ask` 在主线程上调用，不阻塞，结果经 `done` 交回。
pub trait LeaveGuard: Send + Sync {
    fn ask(&self, scope: LeaveScope, done: Box<dyn FnOnce(bool) + Send>);
}
pub enum LeaveScope { View, Project(ProjectKey), Worktree(ProjectKey), Window, App }

/// 窗口的标识。第一版总是 `WindowId(0)`。
pub struct WindowId(pub u32);

/// 屏幕 ID 的约定值，贡献方按它注册，状态机按它选择。
pub mod screens {
    pub const WELCOME: &str = "welcome";
    pub const PROJECT_HOME: &str = "project-home";
    pub const REVIEW: &str = "review";
    pub const READER: &str = "reader";
    pub const WORKTREES: &str = "worktrees";
    pub const USAGE: &str = "usage";
}

/// 屏幕的参数。
pub enum Target {
    None,
    Project(ProjectKey),
    Review(ReviewTarget),
    Read(ReadTarget),
}

/// 一次导航的目标。
pub struct Route { pub screen: Name, pub target: Target }

pub enum WorkspaceCommand {
    /// 打开一个路径：先问 `LeaveScope::Project` 的守卫（已有项目时），通过后调 `Projects::open`。
    OpenProject(PathBuf),
    /// 用系统的打开面板选文件夹（⌘O），然后同 `OpenProject`。
    PromptOpenProject,
    /// 关闭当前项目：问 `LeaveScope::Project`，通过后调 `Projects::close`。
    CloseProject,
    /// 切换当前 worktree：问 `LeaveScope::Worktree`，通过后调 `Projects::set_active_worktree`。
    SetActiveWorktree(WorktreeKey),
    /// 在当前项目之上压入一个屏幕（进入审阅、打开阅读视图、Worktrees、Usage）。
    Push(Route),
    /// 弹出栈顶屏幕，回到下面那个（⌘[、阅读视图的 Esc）：先问 `LeaveScope::View`。
    Pop,
    /// 回到基底屏幕（项目主页）：逐个弹出，每个都问 `LeaveScope::View`。
    PopToBase,
    /// 侧栏换成某个面板（Files）；`None` 回到项目侧栏。
    ShowSidebarPanel(Option<Name>),
    /// 显示或隐藏检查器（⌥⌘I）；`SelectInspectorTab` 顺带显示（⌘\ 切到 Conversation）。
    SetInspector(bool),
    SelectInspectorTab(Name),
    /// 打开或关闭浮动面板（Space 打开上下文面板，Esc 关闭）。
    OpenPanel(Name),
    ClosePanel,
    /// 打开或关闭覆盖层（⌘K）。
    OpenOverlay(Name),
    CloseOverlay,
    /// Esc：按「数据结构」的顺序关闭最上面的一层。
    Escape,
    /// 关闭窗口：问 `LeaveScope::Window`，通过后关闭。
    CloseWindow,
}

/// 由 `workspace` 插件在主线程上发出（`PluginHandle::emitter()`）。监听只更新状态或安排重绘。
pub enum WorkspaceEvent {
    /// 当前屏幕变了（压入、弹出、项目打开或关闭）。`from` 在第一次显示时为 None。
    ScreenChanged { window: WindowId, from: Option<Route>, to: Route },
    /// 一个按需层打开或关闭（检查器、面板、覆盖层、侧栏面板）。
    LayerChanged { window: WindowId, layer: Layer, open: bool },
    /// 侧栏条目、检查器页等贡献增减。
    ContributionsChanged,
    /// 窗口已关闭。
    WindowClosed { window: WindowId },
}
impl Event for WorkspaceEvent {}

pub enum Layer { SidebarPanel(Name), Inspector, Panel(Name), Overlay(Name) }

/// bench 的里程碑（bench 设计 `Milestone` 的同名变体；`app` 做映射，本 crate 不依赖 tool 层）。
pub enum Milestone { ProjectHome, FileHighlighted, DiffShown, WorktreeSwitched }
```

调用方：

- `project_home` 注册 `SidebarPanel("project")`、`Screen(PROJECT_HOME)`、`SidebarEntry`（Overview、Sessions、Packages），渲染时读 `sidebar_entries()`，↵ 时 `dispatch(Push(REVIEW))`，画出真实内容后 `report_milestone(ProjectHome)`。
- `welcome` 注册 `Screen(WELCOME)`，最近项目和 Open Folder 经 `dispatch(OpenProject)` / `dispatch(PromptOpenProject)`。
- `review_view` 注册 `Screen(REVIEW)`、`InspectorTab("files")`、`KeyBindings`；Space 时 `dispatch(OpenPanel("context"))`。
- `file_reader` 注册 `SidebarPanel("files")`、`SidebarEntry("files")`、`Screen(READER)`、`KeyBindings`，登记离开守卫；`Reader::open` 时 `dispatch(Push(READER))`（[file_reader 设计「本 crate 的公开项」](file_reader.md#本-crate-的公开项)）。
- `conversation` 注册 `InspectorTab("conversation")`；`context_drawer` 注册 `Panel("context")`；⌘K（[#102](https://github.com/SoloJiang/lectern-app/issues/102)）注册 `Overlay("finder")` 并用 `Command` 激活。
- `worktrees`、`usage_view` 注册各自的 `Screen` 和 `SidebarEntry`。

### `ui::contrib` 的载荷类型

定义在 `ui`（「对其他 crate 的要求」），这里只列本 crate 依赖的形状；名字以 `ui` 设计为准。闭包都在主线程上调用，本 crate 调用前先 `token.enter()`、恢复贡献方的日志作用域，并用 `kernel::contain` 包住。

```rust
pub struct Screen { pub build: Box<dyn FnOnce(&Target, &mut Window, &mut App) -> AnyView + Send> };
pub struct SidebarPanel { pub build: Box<dyn FnOnce(&mut Window, &mut App) -> AnyView + Send> };
pub struct SidebarEntry { pub label: i18n::Key, pub icon: Option<ui::Symbol>, pub on_select: WorkspaceCommand };
pub struct InspectorTab { pub label: i18n::Key, pub build: Box<dyn FnOnce(&mut Window, &mut App) -> AnyView + Send> };
pub struct Panel { pub build: Box<dyn FnOnce(&mut Window, &mut App) -> AnyView + Send> };
pub struct Overlay { pub build: Box<dyn FnOnce(&mut Window, &mut App) -> AnyView + Send> };
/// 每条：按键、Action、可选的 KeyContext 谓词（「Action 与快捷键」的上下文名）。
pub struct KeyBindings(pub Vec<(&'static str, Box<dyn gpui::Action>, Option<&'static str>)>);
impl Screen { pub fn into_payload(self) -> Payload; }   // 其余类型相同
```

每个 `build` 只调用一次：视图在第一次需要时建，之后保留在本 crate 的区域里（屏幕随栈弹出而 drop；检查器页、面板、覆盖层在关闭时 drop，再次打开时重新 `build`——所以 `build` 是 `FnOnce` 加一个重建用的工厂，具体由 `ui` 设计定；本文按「每次打开重新建」写）。Action 的处理函数不经本 crate：贡献方在自己建的视图上 `on_action`（GPUI 的全局 `on_action` 没有注销，不用）。

### 本 crate 的公开项

```rust
/// `app` 在启动屏障放开后、主线程上调用一次：用 `Workspace` 替换窗口的根视图，启动主线程队列，
/// 返回给插件和 `app` 用的句柄。不做 IO、不建线程。
pub fn install(window: AnyWindowHandle, deps: WorkspaceDeps, cx: &mut App) -> WorkspaceHost;

pub struct WorkspaceDeps {
    /// 窗口状态变化时调用（去抖由 `app` 做）。任何线程。
    pub on_state_changed: Arc<dyn Fn(WindowState) + Send + Sync>,
    /// 里程碑画出后调用，`presented` 是下一帧回调的时刻（见「已知限制」）。任何线程。
    pub on_milestone: Arc<dyn Fn(Milestone, SystemTime) + Send + Sync>,
}

/// `Send + Sync`。`app` 持有一份，`WorkspacePlugin` 持有一份。
#[derive(Clone)]
pub struct WorkspaceHost { /* Arc<{ 主线程队列的发送端, Mutex<Option<AnyWindowHandle>>, 贡献注册表 }> */ }

impl WorkspaceHost {
    /// 点 Dock 图标重新打开窗口时（app 设计「界面」），`app` 开好新的骨架窗口后调用：替换它的根视图，
    /// 沿用注册表和导航状态。主线程。
    pub fn attach(&self, window: AnyWindowHandle, cx: &mut App);

    /// `app` 读到窗口状态文件后调用：有项目路径时 `dispatch(OpenProject)`，检查器按 `inspector` 恢复。任何线程。
    pub fn restore(&self, state: WindowState);

    /// `app` 的 `Quit` 在 `Kernel::shutdown` 之前问 `LeaveScope::App` 的守卫（file_reader 设计「编辑」退出的第 1 步）。
    /// 任何线程；`done(true)` 表示全部通过。没有窗口或还没有守卫时立即 `done(true)`。
    pub fn ask_leave(&self, scope: LeaveScope, done: Box<dyn FnOnce(bool) + Send>);
}

/// `app` 装配时构造，放进 `PluginSet`（id "workspace"）。
pub struct WorkspacePlugin { /* host */ }
impl WorkspacePlugin {
    /// 不做 IO。
    pub fn new(host: WorkspaceHost) -> Self;
}

// Manifest { provides: [service::<dyn Contributions>()], requires: [service::<dyn Projects>()], activation: [AfterFirstFrame] }
// activate：provide `dyn Contributions`（注册表已在 `install` 里建好，这里只交出 Arc）；不做 IO。
// started：保存 `PluginHandle`；`handle.control().lazy_commands()` 为每个命令注册代理（见「数据流与线程」）；
//   订阅 `ProjectEvent`（Opened / Closed 驱动状态机）。不提交任何任务。

/// 跨次运行保留的窗口状态。由 `app` 以 JSON 存在 `platform::dirs::app_data()` 下的 `workspace/window.json`，
/// 首帧之后读。序列化派生 serde；认不出的 `version` 当作没有文件。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct WindowState {
    pub version: u32,                 // = 1
    /// 上次打开的项目的路径（`Projects::open` 收的那个），None 表示上次停在 Welcome。
    pub project: Option<PathBuf>,
    pub inspector: bool,
}
```

- `install`、`attach`：主线程，不阻塞。`replace_root` 在同一次调用里完成，下一帧画出 `Workspace`。
- `WorkspaceHost::restore`、`ask_leave`、`Contributions` 的全部方法：任何线程，立即返回，工作排到主线程。
- `WorkspacePlugin::activate`、`started`：kernel 线程，只登记。

## 数据结构

### 区域与三层

| 区域 | 规格（[交互规范「窗口结构」](interaction.md#窗口结构)） | 层 | 内容来源 |
|---|---|---|---|
| Sidebar | 248pt，vibrancy；当前侧栏面板 | 常驻（项目侧栏）/ 按需（Files 等替换面板） | `SidebarPanel` 贡献；项目侧栏里的条目来自 `SidebarEntry` 贡献 |
| Toolbar | 52pt；标题 + 副标题；左侧返回按钮（有 `back_button` 的屏幕）；右侧至多 3 个控件 | 常驻 | 标题、副标题、控件由当前屏幕的视图经 `ui` 的工具栏容器提供 |
| Content | 居中 760pt 阅读栏（`reading_column`）或整个内容区 | 常驻 / 收起（阅读视图） | 栈顶 `Screen` |
| Inspector | 300pt；分段控件切换页 | 按需（⌥⌘I） | `InspectorTab` 贡献 |
| Panel | 浮在检查器上方，圆角 14pt，阴影，不加遮罩 | 按需（Space 打开，Esc 关闭） | `Panel` 贡献 |
| Overlay | 窗口中央 | 按需（⌘K） | `Overlay` 贡献 |
| Floating bar | 贴底居中 | 常驻（只在审阅） | 审阅屏幕的视图自己画，本 crate 只留出区域 |

没有项目时整个窗口是 Welcome 屏幕：没有侧栏、工具栏、检查器（`ScreenLayout` 全部关闭），窗口 820 × 520 pt（app 设计「界面」）。打开项目后窗口切到项目的 frame autosave 名（「对其他 crate 的要求」），默认 1180 × 760 pt。

### 导航状态机

每个窗口一份：

```rust
struct Nav {
    base: Base,                 // Welcome | Project(ProjectKey)
    stack: Vec<Route>,          // 压在基底之上的屏幕；空时显示基底屏幕
    sidebar_panel: Option<Name>,// None：项目侧栏
    inspector: bool,
    inspector_tab: Option<Name>,
    panel: Option<Name>,
    overlay: Option<Name>,
}
```

当前屏幕是栈顶，栈空时是基底（`Welcome` → `screens::WELCOME`，`Project` → `screens::PROJECT_HOME`）。各区域的显示由当前屏幕的 `ScreenLayout` 和 `Nav` 决定：侧栏 = `layout.sidebar == Shown`；检查器 = `layout.inspector && nav.inspector`；面板、覆盖层只看 `nav`；浮条 = `layout.floating_bar`。

| 当前状态 | 输入 | 守卫 | 结果 |
|---|---|---|---|
| 任意 | `ProjectEvent::Opened { Ok(key) }` | — | `base = Project(key)`，`stack` 清空（先问 `View` 守卫，被拒时不换，见「状态与异常」），`sidebar_panel = None`；`ScreenChanged` |
| `Project(k)` | `ProjectEvent::Closed { k }` | — | `base = Welcome`，`stack` 清空，面板、覆盖层、检查器关闭；`ScreenChanged` |
| `Welcome` | `OpenProject(p)` / `PromptOpenProject` | — | `Projects::open(p)`；等 `Opened` |
| `Project(k)` | `OpenProject(p)` | `Project(k)` | 通过后 `Projects::open(p)`（第一版在同一个窗口里换项目，见「未决问题」1） |
| `Project(k)` | `CloseProject` | `Project(k)` | 通过后 `Projects::close(k)` |
| `Project(k)` | `SetActiveWorktree(w)` | `Worktree(k)` | 通过后 `Projects::set_active_worktree(k, w)` |
| `Project` | `Push(r)` | — | `r` 的屏幕已注册：压栈，`panel`、`overlay` 关闭；没注册：记 warn，不变 |
| `Project`，栈非空 | `Pop` | `View` | 弹出栈顶，`panel`、`overlay` 关闭 |
| `Project`，栈非空 | `PopToBase` | 每层一次 `View` | 逐层弹出，任一层被拒即停在那一层 |
| 任意 | `Escape` | 见下 | 关闭最上面的一层 |
| 任意 | `ShowSidebarPanel(p)` | — | 面板已注册才换 |
| `layout.inspector` | `SetInspector` / `SelectInspectorTab` | — | 改 `inspector`、`inspector_tab`；`LayerChanged`；`on_state_changed` |
| 任意 | `OpenPanel` / `OpenOverlay` | — | 已注册才开；同一时刻各至多一个，新的替换旧的 |
| 任意 | `CloseWindow` | `Window` | 通过后 `remove_window`，`WindowClosed` |
| `Welcome` | `Pop`、`PopToBase`、`Push` | — | 忽略，记 debug |

`Escape` 的顺序：覆盖层 → 面板 → 替换侧栏的面板（回到项目侧栏）→ 栈顶屏幕（`Pop`，经 `View` 守卫）→ 都没有时不做任何事。当前屏幕的视图先收到 Esc（它可能关闭自己的查找栏），没有处理时才冒泡到本 crate。⌘[ 同样先给当前屏幕（阅读视图的后退），没有处理时是 `PopToBase`（「Esc · ⌘[：关闭面板 · 返回 Overview」）。

守卫的询问：对一次命令，按登记顺序逐个 `ask(scope, done)`，前一个 `done(true)` 之后才问下一个；任一个 `done(false)` 就作废这次命令。询问期间到达的同范围命令排队，不并发询问。守卫的 `ask` 在它的激活准入内调用；`enter()` 失败的守卫跳过。

### 贡献注册表

一个进程一份（第一版一个窗口，注册表不按窗口分），在 `install` 里建，`Arc` 由 `WorkspaceHost` 和 `dyn Contributions` 共享：

```rust
struct Registry {
    entries: Mutex<HashMap<(ContributionKind 的判别, Name), Entry>>,
    sidebar_entries: ArcSwap<[SidebarEntry]>,   // `sidebar_entries()` 直接返回
    commands: Mutex<HashMap<Name, CommandEntry>>,
    leave_guards: Mutex<Vec<GuardEntry>>,
    key_bindings: Mutex<Vec<(ActivationToken, Vec<Binding>)>>,
}
struct Entry { token: ActivationToken, ctx: lectern_logging::Ctx, payload: Option<Payload> /* 建视图时取走 */, order: u16, … }
```

- 登记（任何线程）：写进表、生成一个序号，返回 `Registration`。`activate` 期间的登记只进表、不发事件；`commit(token)` 把 token 填进条目并排一次「应用」到主线程（`token.pending()` 守卫随队列项，取不到就丢弃）。`started` 之后的登记直接排「应用」。
- 应用（主线程）：检查 `token.state() == Live`（否则丢弃）；按 kind 把条目加进相应的集合：`SidebarEntry` 重建 `sidebar_entries` 快照并发 `ContributionsChanged`；`KeyBindings` 重建整个 keymap（GPUI 只有 `bind_keys` 和 `clear_key_bindings`，没有单条删除：本 crate 保存全部有效绑定，每次增减都 `clear_key_bindings` 再 `bind_keys`，本 crate 自己的绑定也在这份表里）；`Screen` 等只登记工厂，视图在需要时才建。应用完给这次激活的「已应用」标记置位（「按命令激活」第 3 步）。
- 注销（`Registration` drop，任何线程）：取 `token.pending()`，排「移除」到主线程；主线程上从集合移除、drop 工厂和已建的视图（正在显示的屏幕被移除时按 `Pop` 处理，不问守卫；基底屏幕被移除时内容区显示空白并记 warn），每项在 `contain` 里做；守卫 drop 后 kernel 才能报告静止。
- 调用贡献方的闭包（视图工厂、命令处理函数、守卫的 `ask`）：`token.enter()` 取准入，`logging::scope(entry.ctx)`，`contain` 包住；panic 时 `fail` 这次激活（kernel 设计「撤销」）。`ctx` 是登记时在调用线程上取的 `lectern_logging::current()`，`activate` 里登记的因此带 kernel 设的 `plugin`、`act`（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 11 项）。

### 窗口状态

`WindowState` 只保存不能从别处恢复、又值得跨次运行保留的三项（侧栏 248pt、检查器 300pt 是固定宽度，不保存）。变化时（项目打开或关闭、检查器显示或隐藏）本 crate 调 `on_state_changed(state)`；`app` 去抖 500 ms 后在 executor 的 `Background` 任务里写文件（「对其他 crate 的要求」）。启动时 `app` 在屏障放开后读文件，调 `WorkspaceHost::restore`；在那之前窗口显示 Welcome。`restore` 来的 `OpenProject` 不问守卫（这时还没有项目）。

## 数据流与线程

```
首帧 ─▶ 屏障放开（app::startup::release，主线程）
   ├─ workspace::install：replace_root、起主线程队列的前台任务 ─▶ 下一帧画出空的 Workspace（Welcome 屏幕还没注册时内容区空白）
   ├─ PluginSet 加 WorkspacePlugin ─▶ Kernel::start
   └─ app 在 executor 上读 window.json ─▶ WorkspaceHost::restore ─▶ dispatch(OpenProject)
kernel 线程：workspace.activate（provide）─▶ started：lazy_commands → 代理；订阅 ProjectEvent
kernel 线程：welcome / project_home / … 的 activate：Contributions::register ─▶ ctx.own_staged ─▶ commit(token) ─▶ [主线程] 应用 ─▶ 重绘
executor 线程：ProjectEvent::Opened ─▶ 监听（workspace）─▶ [主线程，pending + BusyGuard] 状态机 ─▶ ScreenChanged、重绘、on_state_changed
主线程：按键 / 点击 ─▶ 当前视图的 on_action ─▶ 没处理的冒泡到 Workspace 的 Action ─▶ 状态机
```

- 主线程队列：`futures::channel::mpsc::unbounded::<Job>()`，`Job = Box<dyn FnOnce(&mut App) + Send>` 加可选的 `PendingGuard` / `BusyGuard`。接收端是 `install` 里 `cx.spawn` 的前台任务，`await` 下一项后在主线程上执行；发送端的 `wake` 经 GPUI 的平台分发器唤醒主线程，所以任何线程都能排队，不轮询、不占 CPU。队列在 `shutdown` 关闭准入之后取不到 `PendingGuard` 的项直接丢弃（kernel 设计「撤销」）。
- 代理 Action（kernel 设计「按命令激活」）：`started` 里 `lazy_commands()` 的每个名字绑定一个 `workspace::Command { name }` 的处理：主线程查 `commands`，有 `Live` 的处理函数就在准入内调用；没有则 `control.command(name, done)`，`done` 带本插件的 `pending()` 排到主线程，等目标激活的「已应用」标记置位后再查一次；失败、仍没有处理函数、或处理函数的 token 已不是 `Live` 时显示一次提示（`workspace-command-failed`）并记录。
- 离开守卫与窗口关闭：`install` 里 `on_window_should_close` 返回 false 并 `dispatch(CloseWindow)`；守卫都通过后 `remove_window`。`app` 的 `Quit` 调 `ask_leave(App, done)`，`done(true)` 后才 `Kernel::shutdown`（app 设计「退出顺序」在它的下一次改动里加上这一步，见「对其他 crate 的要求」）。
- UI 线程上不做 IO、解析和 git 操作：本 crate 只调 `Projects::open` / `close` / `set_active_worktree`（立即返回）、`snapshot`（取锁复制 `Arc`）、`prompt_for_paths`（系统面板，异步）。窗口状态文件的读写在 `app` 的 executor 任务里。
- 派生数据：没有。本 crate 不缓存任何按内容派生的数据（架构规则 5 不适用）。

## 性能

- 冷启动：首帧之前本 crate 没有代码运行；`install` 在屏障放开之后调用，`replace_root` 加队列任务的创建在 1 ms 内，不计入冷启动。
- 首帧之后：插件激活只交出 `Arc`（`plugin/workspace/activate` < 1 ms）；注册表的应用每项常数时间；重建 keymap 与绑定数成正比（几十条，< 1 ms）。
- 空闲内存：注册表、导航状态、队列合计 < 200 KiB；视图只在需要时建，关闭即 drop（检查器页、面板、覆盖层、弹出的屏幕）。本 crate 不持有快照：`Projects::snapshot` 只在渲染时取、不保存。
- 帧时间：`Workspace::render` 只组合已建的视图和固定尺寸的容器，不排版文本；动画只用 `ui` 的原语（位移、透明度、裁剪，ADR 0015）。
- 基准：`plugin/workspace/activate`、`plugin/workspace/resident` 随 bench 的插件基准自动覆盖；`app/open_project` 的终点 `Milestone::ProjectHome` 由 `project_home` 经 `report_milestone` 报告，本 crate 在下一帧回调里转交。不新增进程内基准。

## 界面

- 屏幕与层：见「区域与三层」。本 crate 画的只有区域的容器、工具栏的返回按钮、检查器的分段控件和一次性提示；其余都是贡献的视图。
- `ui` 控件与动效原语（名字以 `ui` 设计为准）：侧栏容器（vibrancy）、工具栏容器、检查器容器与分段控件（滑块 260 ms）、面板容器（滑入 380 ms / 滑出 280 ms，`cubic-bezier(0.32, 0.72, 0, 1)`）、覆盖层容器、浮条容器、一次性提示；减弱动态效果时位移改为淡入淡出、时长减半。本 crate 不写任何颜色值、字体和动画参数。
- Fluent 键（`crates/i18n/locales/{en,zh-CN}/workspace.ftl`）：

  | 键 | en | zh-CN | 用途 |
  |---|---|---|---|
  | `workspace-back` | Back | 返回 | 工具栏返回按钮的辅助名 |
  | `workspace-inspector-toggle` | Inspector | 检查器 | 工具栏右侧控件的辅助名 |
  | `workspace-command-failed` | This command isn't available right now. | 这个命令现在不可用。 | 代理 Action 激活失败或没有处理函数时的一次性提示 |
  | `workspace-screen-missing` | Nothing to show here yet. | 这里还没有内容。 | 基底屏幕没有注册时内容区的一行次要文字 |

  工具栏标题、副标题、侧栏条目的文字都来自贡献方的 Fluent 键，本 crate 不翻译、不拼接。

- UI 探针的语义节点（角色与稳定 ID）：

  | ID | 角色 | 状态 |
  |---|---|---|
  | `workspace.window` | 窗口 | 基底（`welcome` / `project`）、当前屏幕 ID、栈深度 |
  | `workspace.sidebar` | 侧栏 | 显示 / 隐藏、当前面板 ID |
  | `workspace.sidebar.panel[<id>]` | 侧栏面板 | — |
  | `workspace.toolbar` | 工具栏 | 标题、副标题（文字由屏幕提供） |
  | `workspace.toolbar.back` | 按钮 | 可见 |
  | `workspace.toolbar.inspector` | 按钮 | 可见、选中 |
  | `workspace.content` | 内容区 | 阅读栏 / 整区 |
  | `workspace.content.screen[<id>]` | 屏幕 | 栈里的位置 |
  | `workspace.inspector` | 检查器 | 显示 / 隐藏、当前页 |
  | `workspace.inspector.tab[<id>]` | 分段控件的一项 | 选中 |
  | `workspace.panel[<id>]`、`workspace.overlay[<id>]` | 面板、覆盖层 | 打开 |
  | `workspace.bar` | 浮条区域 | 可见 |
  | `workspace.notice` | 一次性提示 | 消息键 |

  贡献的视图在各自的节点下导出自己的子树（`project_home.*`、`review.*`、`file_reader.*`）。

- Action 与快捷键（本 crate 的；都能被探针触发）：

  | Action | 键 | 上下文 | 处理 |
  |---|---|---|---|
  | `workspace::OpenFolder` | ⌘O | 全局 | `PromptOpenProject` |
  | `workspace::CloseWindow` | ⌘W | 全局 | `CloseWindow` |
  | `workspace::Back` | ⌘[ | `Workspace`（屏幕没处理时冒泡到这里） | `PopToBase` |
  | `workspace::Escape` | Esc | `Workspace` | `Escape` |
  | `workspace::ToggleInspector` | ⌥⌘I | `Workspace > project` | `SetInspector(!inspector)` |
  | `workspace::ShowConversation` | ⌘\ | `Workspace > project` | `SelectInspectorTab("conversation")` |
  | `workspace::Command { name }` | 由贡献的 `KeyBindings` 绑定 | 任意 | 代理（「数据流与线程」） |

  KeyContext 的名字：根视图 `Workspace`，有项目时加 `project`；侧栏 `Sidebar`，内容区 `Content`，检查器 `Inspector`，面板 `Panel`，覆盖层 `Overlay`。贡献的屏幕在自己的根元素上设自己的上下文（如 `reader`、`review`），`KeyBindings` 的谓词写成 `Content > reader` 这类；⌘K 等由 `Command` 激活的命令只经 `workspace::Command` 绑定。⌘[ 在编辑时归 `InputState`（file_reader 设计「编辑」），它在 `Content > reader > editing` 上先处理，不会冒泡到这里。

## 状态与异常

| 情况 | 处理 |
|---|---|
| `Opened` 到达时栈上有被守卫拒绝的屏幕（正在编辑） | 项目已经打开（`Projects` 不等界面），但状态机不换基底：记 warn `project opened while a view refused to leave`，提示一次；用户离开编辑后再 `OpenProject` 同一路径时 `Projects::open` 返回同一个 key 并再发 `Opened`，这次正常切换 |
| `Opened { Err(e) }` | 停在当前屏幕，记 warn（错误种类），提示交给 `welcome`（它监听同一个事件） |
| 恢复窗口状态时项目路径已不存在 | 同上：留在 Welcome；`app` 下次写状态时 `project` 为 None |
| 基底屏幕（`welcome` / `project-home`）还没有注册（插件没装、激活失败或还没应用） | 内容区显示 `workspace-screen-missing`；注册到达后自动换上 |
| `Push` 的屏幕没有注册 | 忽略，记 warn |
| 贡献的闭包 panic | `contain` 捕获，让这次激活失效（kernel 设计「撤销」），区域显示为空，不崩溃 |
| 插件停用时它的屏幕正在显示 | 按 `Pop` 处理，不问守卫；是基底时显示 `workspace-screen-missing` |
| 命令激活失败、没有处理函数 | 一次性提示 `workspace-command-failed`，记录 |
| 关闭窗口被守卫拒绝 | 窗口不关，什么也不提示（守卫自己弹出了确认） |
| 由系统发起的退出 | 不经本 crate（app 设计「已知限制」） |
| 加载态 | 本 crate 没有自己的加载态：屏幕切换立即生效（「界面先更新」），内容的骨架由各屏幕按[交互规范「状态与异常」](interaction.md#状态与异常)显示 |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `workspace`。

- 默认记录：
  - info `screen changed`：`window`、`from`（屏幕 ID，`&'static str` 或登记的 `Name` 经 `PluginTag` 同样的约束，不是标题）、`to`、`depth`（栈深度）。
  - info `contribution applied`：`kind`、`id`、`act`（贡献方的激活序号，在作用域里）。info `contribution removed`：同样的字段。
  - warn `contribution rejected`：`kind`、`id`、`reason`（`duplicate`、`wrong-payload`、`invalid-id`、`token-dead`）。
  - warn `screen not registered`：`id`。warn `base screen missing`：`id`。
  - warn `command without handler`：`command`。error `command activation failed`：`command`、`error`（`PluginError` 的种类）。
  - warn `project opened while a view refused to leave`、warn `project open failed`：`error`（`OpenError` 的种类）。
  - warn `job dropped after shutdown`：`jobs`（计数，队列关闭后累计一次）。
- 详细级别：debug `command`（`WorkspaceCommand` 的判别名、是否经过守卫、结果 `done` / `refused` / `ignored`）；debug `layer changed`（层、开关）；debug `milestone`（名字、`presented` 的微秒数）；trace `job`（队列项的来源插件与耗时）。
- 关联 ID：调用贡献方闭包时恢复登记时取的 `Ctx`（`plugin`、`act`），所以贡献方的记录和本 crate 在那段代码里的记录都带它们；本 crate 自己的状态机代码在 kernel 为 `workspace` 设的作用域之外运行（主线程），记录带 `window`，不带 `plugin`。
- 不记录：项目路径（`OpenProject` 的参数、窗口状态里的路径）、`prompt_for_paths` 返回的路径、工具栏的标题与副标题、侧栏条目的文字、提示的文字、`Target` 里的会话标识之外的内容（`ReviewTarget`、`ReadTarget` 只记判别名）。用屏幕 ID、贡献 ID、计数和错误种类代替。
- 金丝雀测试：`canary_stays_out_of_workspace_logs`（`src/tests/logs.rs`）：用 `lectern_logging::testing::capture()`，在 trace 级别 `dispatch(OpenProject(含金丝雀的路径))`、注册一个标题含金丝雀的屏幕并切换到它、`restore(WindowState { project: 含金丝雀 })`，`assert_absent(金丝雀)`。

## 测试

依赖 GPUI，只在 macOS 上编译和测试：`just test -p lectern-workspace`。

- 状态机（纯函数，`#[test]`，不依赖 GPUI）：`nav::*`：「导航状态机」表的每一行一个测试；`escape_closes_the_topmost_layer`（覆盖层 → 面板 → 侧栏面板 → 栈顶）；`guards_are_asked_in_order_and_a_refusal_stops`；`commands_during_a_pending_ask_queue_up`；`pop_to_base_stops_at_a_refusal`。
- 注册表（`#[test]`，用 `TestKernel` 的 token）：`duplicate_ids_are_rejected`、`wrong_payload_is_rejected`、`staged_registration_applies_on_commit_only`、`dead_token_is_not_applied`、`removal_drops_the_shown_screen`、`keymap_is_rebuilt_on_change`（全部绑定在表里，减少一个插件的绑定后其余仍在）。
- `#[gpui::test]`（`TestAppContext`，`TestKernel` 注入假的 `dyn Projects`）：`install_replaces_the_root_and_shows_nothing_before_a_screen_registers`、`project_home_shows_after_opened`（假 `Projects` 发 `Opened`，侧栏与内容区出现）、`review_hides_the_sidebar_and_shows_the_bar`、`escape_and_back_return_to_the_home`、`inspector_toggles_with_alt_cmd_i`、`close_window_asks_the_guard_first`、`proxy_action_activates_the_command_plugin_and_replays_once`（`Command` 激活的假插件：第一次触发激活并执行一次，失败时提示一次）、`contribution_closures_run_in_the_plugins_scope`（`logging::testing::capture()` 里看 `plugin`、`act`）、`restore_opens_the_last_project`、`state_changes_are_reported`。
- 语义树：`workspace_tree_welcome`、`workspace_tree_project_home`、`workspace_tree_review_with_inspector` 的 insta 快照（UI 探针完成后接入；之前用本 crate 自己的 `Inspect` 输出）。
- 日志：`canary_stays_out_of_workspace_logs`。
- 不用真实的 git、会话扫描和文件 IO；`Projects` 用 `services` 的 trait 对象的假实现，只发事件、返回构造好的快照。

## 对其他 crate 的要求

每一项是一个 PR，写 `Refs #215`，在「验收」里列为前置条件。

- **A. `services`**：`workspace` 模块（「服务接口」）。`Registration`、`Payload`、`LeaveGuard` 的回调由本 crate 定义内容；`ReviewTarget`、`ReadTarget`、`ProjectKey`、`WorktreeKey` 复用已有的模块。
- **B. `ui`**（[#6](https://github.com/SoloJiang/lectern-app/issues/6) 的设计，另一个会话并行编写；需要的内容已写进共享笔记）：`ui::contrib` 的载荷类型与 `into_payload`；`ui::strings(cx)`；侧栏、工具栏、检查器、面板、覆盖层、浮条的容器；分段控件；滑入 / 滑出、分段滑块、悬停的动效原语；一次性提示；SF Symbols 图标。
- **C. `i18n`**：`locales/{en,zh-CN}/workspace.ftl`，内容是「界面」的四个键。
- **D. `platform`**：窗口 frame 的 autosave（ADR 0017：「窗口位置与大小用 AppKit 的 frame autosave」）。锁定的 GPUI 不暴露这个设置，`gpui-pre-macos` 实现了 `raw_window_handle`：

  ```rust
  /// 给窗口设 AppKit 的 frame autosave 名（`NSWindow.setFrameAutosaveName:`）：AppKit 立即从应用偏好设置
  /// （内存里的 user defaults，不是 Lectern 自己的文件）恢复这个名字下保存的 frame，之后每次移动或缩放自动保存。
  /// 只在主线程调用；其他平台不做任何事。
  pub fn set_frame_autosave_name(window: &impl raw_window_handle::HasWindowHandle, name: &str) -> Result<(), WindowError>;
  ```

  `app` 在 `open_window` 之后、首帧之前对 Welcome 窗口设 `"welcome"`；切到项目时设 `"project"`，回到 Welcome 时设回。这是 ADR 0017 允许的首帧前唯一的偏好读取。
- **E. `app`**：
  - `startup::release` 里 `Kernel::start` 之前：`workspace::install(window, deps, cx)`，`WorkspacePlugin::new(host)` 放进 `PluginSet`（`Profile::Desktop` 的表加一行；test profile 不装）。
  - `on_reopen` 开好骨架窗口后调 `host.attach`。
  - 屏障放开后在 executor 的 `Background` 任务里读 `app_data()/workspace/window.json`，解析成 `WindowState`（认不出或没有文件时为默认值），调 `host.restore`；`app_data()` 为 None 时不读不写。`on_state_changed` 去抖 500 ms 后在 `Background` 任务里写同一个文件（写临时文件再 `rename`，不建目录以外的任何东西）。读写失败只记 warn（不记路径）。
  - `Quit` 的第 1 步：`host.ask_leave(LeaveScope::App, done)`，`done(false)` 时什么也不做。
  - 把 `Milestone` 转给 bench 的探针（`bench-probe` feature）。
- **F. `bench`**：`Milestone` 的来源改为 `workspace::Milestone` 的映射（bench 设计「需要其他 crate 提供的接口」已写明由 `workspace` 报告）。

## 未决问题

1. **一个窗口还是每个项目一个窗口。** [交互规范「快捷键」](interaction.md#快捷键)写 ⌘O「打开文件夹（新窗口）」，「窗口结构」写 Welcome 是「独立窗口」。`Projects` 已支持同时打开多个项目，但多窗口要把 `ProjectEvent` 按窗口路由、探针的语义树按窗口导出、frame autosave 按窗口命名。默认（本文按它写）：第一版一个窗口；没有项目时它是 Welcome，打开项目后同一个窗口换成项目窗口（frame autosave 名随之切换，所以两种状态各有自己的大小和位置）；已有项目时 ⌘O 先问离开守卫，通过后在同一个窗口里换项目；多窗口留给后续 issue，导航状态已按窗口保存。另一个选项：从第一版就每个项目一个窗口，Welcome 单独一个窗口，⌘O 总是开新窗口。
2. **启动时恢复上次的项目，还是总是先显示 Welcome。** 默认：恢复（Xcode、大多数文档类应用的做法；`Projects::open` 立即返回，Welcome 只闪现到 `Opened` 到达，项目主页 < 100 ms 可用）；上次停在 Welcome 时显示 Welcome。另一个选项：总是 Welcome，由用户选最近项目。
3. **上下文面板在哪里打开。** 交互规范写「浮在检查器上方」；检查器隐藏时面板贴右侧滑入，占检查器的位置（300pt）。默认按这个写；另一个选项是面板打开时自动显示检查器。

## 已知限制

- 里程碑的 `presented` 用下一帧回调的时刻，不是 `MTLDrawable.presentedTime`：与 app 设计「启动屏障」相同，`platform::frame::on_first_presented` 实现后改用它。
- 注册表不按窗口分：所有窗口共享同一套贡献（多窗口时也应如此，视图实例按窗口建）。
- 由系统发起的退出不问离开守卫（app 设计「已知限制」）。
- 进程插件不能贡献界面（「范围」）。
- GPUI 没有单条删除键绑定的接口，每次增减重建整个 keymap；绑定数在几十条以内，没有可感知的开销。

## 验收

分三步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-workspace`、`just deny` 通过。前置条件 A、C 各自的 PR 先合并；B（`ui` 设计与实现）在第 2 步之前；D、E、F 随第 3 步。

**第 1 步：`services::workspace` 与状态机、注册表（不依赖 GPUI 的部分）。**

- `cargo check -p lectern-services`、`just test -p lectern-services` 通过；`services` 的 AGENTS.md 加上 `workspace` 模块一行。
- 「测试」里 `nav::*` 和注册表的测试通过。
- `rg 'gpui' crates/services/src` 没有结果。

**第 2 步：根视图、区域、代理 Action、离开守卫（`#[gpui::test]`）。**

- 「测试」里的 `#[gpui::test]` 和语义树快照通过。
- `rg 'std::fs|std::process|git2|gix' crates/workspace/src` 没有结果（本 crate 不做 IO）。
- `rg 'rgb\(|hsla\(|Duration::from_millis' crates/workspace/src` 没有结果（颜色和动画参数只来自 `ui`）。
- `rg '"[A-Z][a-z]+ [a-z]' crates/workspace/src --glob '!tests*'` 没有界面字符串字面量（都走 `keys::workspace::*`）。
- `python3 scripts/check_architecture.py` 通过：`workspace` 带 `plugin = true`，不依赖其他插件 crate；只有 `app` 依赖它。
- `canary_stays_out_of_workspace_logs` 通过；`rg 'eprintln!|println!|log::' crates/workspace/src` 没有结果。

**第 3 步：装配（`app`）、窗口状态、frame autosave、里程碑（`Closes #215`）。**

- `just test -p lectern-app`：`every_profile_graph_validates` 里 desktop 的插件数加一；启动测试里 `plugins assembled` 的 `plugins` 相应加一；`startup_writes_a_header_and_an_end_record` 之后 `~/Library/Logs/Lectern/` 的记录里没有 `window.json` 的路径。
- 手动在 macOS 上运行发布构建（临时 `HOME`）：首次启动显示 Welcome（820 × 520）；没有任何 `workspace` 的记录早于 `first frame drawn`；移动窗口后重启，位置保留；打开一个文件夹后重启，直接回到它的项目主页，`screen changed` 的 `to` 为 `project-home`。
- `app/cold_start` 与改动前相比不变慢（bench 第 5 步实现之后判定；之前用启动测试确认首帧之前没有本 crate 的记录）。
- 空闲内存：空窗口的 `phys_footprint` 与 app 设计「实际验证」的 40 MB 相比增加不超过 1 MB。
