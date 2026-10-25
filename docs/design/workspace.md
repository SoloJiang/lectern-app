# workspace 设计

状态：实现中。前置条件 A（`services` 的 `workspace` 模块，[#221](https://github.com/SoloJiang/lectern-app/pull/221)）、「验收」第 1 步（本 crate 不依赖 GPUI 的部分：导航状态机、贡献注册表、窗口状态、插件骨架，[#229](https://github.com/SoloJiang/lectern-app/pull/229)）、登记接口带 token（[#232](https://github.com/SoloJiang/lectern-app/pull/232)）与第 2 步（根视图、区域、代理 Action、离开守卫与窗口的联动、`WorkspaceHost`，[#235](https://github.com/SoloJiang/lectern-app/pull/235)）已实现，见「实际验证」「与设计的差异」。前置条件 D（`platform` 的 frame autosave）已实现（见 [platform 设计](platform.md#窗口-frame-的-autosave)）。第 3 步的代码（前置条件 E：`app` 装配、窗口状态文件、frame autosave 名、`Quit` 问离开守卫）已实现；这一步的手动验收（发布构建的运行、冷启动、空闲内存）、「项目」窗口第一次使用时的 1180 × 760 居中、里程碑与帧记录转交给 bench 的探针（前置条件 F）还没有做，原因见「与设计的差异」第 3 步，所以 issue 仍是 `Refs #215`。另有 `WorkspaceCommand::SetReadingColumn`（review_view 设计的前置条件 A，`Refs #15`）已实现，见「实际验证」「与设计的差异」。

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
| `lectern-ui` | 控件、颜色、动效原语、贡献载荷的类型（`ui::contrib`）、当前语言的 `Strings`；容器由本 crate 画 | 视图层的公共库，非插件 |
| `lectern-i18n` | 本 crate 自己的几条字符串（`workspace.ftl`） | [ADR 0013](../adr/0013.md) |
| `lectern-kernel`、`lectern-services` | `Plugin`、`Staged`、`ActivationToken`、`KernelControl`；提供 `dyn Contributions`，依赖 `dyn Projects` | 架构规则 7（[ADR 0025](../adr/0025.md)） |
| `lectern-logging` | 日志宏、`current()` / `scope()`（主线程上恢复贡献方的作用域） | [ADR 0046](../adr/0046.md) |
| `lectern-platform` | `frame::record`、`frame::FrameTimes`（帧记录；实现之前只有类型） | 架构规则 6：帧时刻只能在 `platform` 取 |
| `serde` | `WindowState` 的序列化 | 已在工作区依赖里 |
| `futures`（`channel` feature） | 主线程队列的 `mpsc::unbounded`：发送方在任何线程，接收方是 GPUI 的前台任务 | 已在 Cargo.lock（GPUI 依赖它），不新增 crate；加进 `[workspace.dependencies]`，`just deny` 通过 |
| `lectern-kernel`（`test-support`，dev）、`insta`（dev） | `TestKernel`、语义树快照 | |

本 crate 是插件 crate（[ADR 0025](../adr/0025.md)）：插件 `workspace`，提供 `dyn Contributions`，依赖 `dyn Projects`，`AfterFirstFrame`。它只依赖 kernel 层和非插件的库 crate，不依赖 `project_home`、`review_view`、`file_reader` 等贡献方；只有 `app` 和 tool 层依赖它。

## 接口

### 服务接口（`lectern-services` 的 `workspace` 模块）

`services` 在 kernel 层，不能依赖 `gpui` 和 `lectern-i18n`，所以这里只有稳定 ID、顺序和布局这类 GPUI 无关的数据；视图工厂、`Action`、Fluent 键放在类型擦除的 `Payload` 里，具体类型由 `ui::contrib` 定义（「`ui::contrib` 的载荷类型」），本 crate 用 `downcast` 取回。数据类型派生 `Clone`、`PartialEq`、`Eq`、`Hash`、`Debug`、`Serialize`、`Deserialize`（与 `services` 的其他模块相同），`Payload`、trait 和包着回调的类型除外；含载荷的 `Contribution` 只实现 `Debug`，`SidebarEntry` 只派生 `Clone`、`Debug`。

```rust
/// 单实现服务，`workspace` 插件提供。
/// impl Service for dyn Contributions { const ID = "contributions"; const MULTIPLE = false; }
pub trait Contributions: Send + Sync {
    /// 登记一项贡献。立即返回，任何线程可调用。`token` 是登记方这次激活的 token：`activate` 里还没有 token，
    /// 传 `None` 并把返回的守卫交给 `ctx.own_staged`，提交时才生效；`started` 之后（`started`、监听、任务里）
    /// 传 `Some(handle.token())`，登记立即按 [kernel 设计「撤销」](kernel.md#撤销)排到主线程应用——token 还是
    /// `Pending` 时留到变为 `Live` 再应用，已是 `Dead` 时拒绝（记 warn `token-dead`）。`activate` 之外传 `None`
    /// 的登记没有人提交，永远不显示（「与设计的差异」第 1 步）。
    /// 同一个 `(kind, id)` 已有登记时后来的被拒绝（记 warn，见「日志与诊断」），守卫 drop 时什么也不做。
    /// 守卫 drop 时注销，注销排到主线程，带所属激活的 `token.pending()` 守卫。
    fn register(&self, contribution: Contribution, token: Option<ActivationToken>) -> Registration;

    /// 登记一个由 `Command` 激活的命令的处理函数（kernel 设计「按命令激活」第 1 步）。规则同 `register`。
    fn handle_command(&self, name: Name, handler: CommandHandler, token: Option<ActivationToken>) -> Registration;

    /// 登记离开守卫（[file_reader 设计「编辑」](file_reader.md#编辑)）。按登记顺序询问。规则同 `register`。
    fn register_leave_guard(&self, guard: Arc<dyn LeaveGuard>, token: Option<ActivationToken>) -> Registration;

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
    /// 项目侧栏里的一个条目（Overview、Sessions、Worktrees、Files、Usage……）。`payload` 是 `ui::contrib::SidebarItem`。
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

/// 已应用的、可以被多个视图共享的载荷：本 crate 应用 `SidebarEntry` 登记时把 `Payload` 里的
/// `ui::contrib::SidebarItem` 换成 `Arc`，`project_home` 用 `Arc::downcast` 取回。
pub struct SharedPayload(pub Arc<dyn Any + Send + Sync>);

/// 一条已登记的侧栏条目（`sidebar_entries` 的元素）。
pub struct SidebarEntry { pub id: Name, pub section: SidebarSection, pub order: u16, pub payload: SharedPayload }

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
    /// 当前屏幕（栈顶；栈空时是基底屏幕）的内容是否限制在阅读栏里，覆盖它 `ScreenLayout::reading_column`
    /// 的值；这个屏幕被弹出时恢复。审阅的双栏视图要整个内容区时用它（[review_view 设计](review_view.md)「对其他 crate 的要求」A）。
    /// 立即返回，主线程应用，不发事件（根视图自己重画）。没有项目时忽略。
    SetReadingColumn(bool),
    /// 侧栏换成某个面板（Files）；`None` 回到项目侧栏。
    ShowSidebarPanel(Option<Name>),
    /// 显示或隐藏检查器（⌥⌘I）；`SelectInspectorTab` 顺带显示（⌘\ 切到 Conversation）。
    /// 当前屏幕的 `layout.inspector` 为 false（Welcome、项目主页）时两者都忽略，记 debug。
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
/// 视图工厂：每次需要这个视图时调用一次（同一个屏幕可以被压栈多次、面板可以反复打开），所以是 `Fn`，
/// 可以克隆、在任何线程持有，只在主线程调用。
pub type ViewFactory<Arg> = Arc<dyn Fn(&Arg, &mut Window, &mut App) -> AnyView + Send + Sync>;
pub struct Screen {
    pub build: ViewFactory<Target>,
    /// 工具栏的标题、副标题和右侧至多 3 个控件；每帧由本 crate 在主线程调用，返回当前值。
    pub toolbar: Arc<dyn Fn(&mut App) -> ToolbarItems + Send + Sync>,
}
pub struct ToolbarItems { pub title: Text, pub subtitle: Option<Text>, pub controls: Vec<AnyElement> /* ≤ 3 */ }
pub struct SidebarPanel { pub build: ViewFactory<()> };
pub struct SidebarItem { pub label: i18n::Key, pub icon: Option<ui::Icon>, pub on_select: WorkspaceCommand };
pub struct InspectorTab { pub label: i18n::Key, pub build: ViewFactory<()> };
pub struct Panel { pub build: ViewFactory<()> };
pub struct Overlay { pub build: ViewFactory<()> };
/// 每条：按键、Action、可选的 KeyContext 谓词（「Action 与快捷键」的上下文名）。
pub struct KeyBindings(pub Vec<(&'static str, Box<dyn gpui::Action>, Option<&'static str>)>);
impl Screen { pub fn into_payload(self) -> Payload; }   // 其余类型相同
```

视图在需要时建、不需要时 drop：屏幕在压栈时 `build`、弹出时 drop；检查器页在选中时 `build`、检查器隐藏或换页时 drop；面板和覆盖层在打开时 `build`、关闭时 drop；侧栏面板在显示时 `build`、换走时 drop。工厂本身留在注册表里直到注销。Action 的处理函数不经本 crate：贡献方在自己建的视图上 `on_action`（GPUI 的全局 `on_action` 没有注销，不用）。

### 本 crate 的公开项

```rust
/// `app` 在启动屏障放开后、主线程上调用一次：用 `Workspace` 替换窗口的根视图，启动主线程队列，
/// 返回给插件和 `app` 用的句柄。不做 IO、不建线程。
pub fn install(window: AnyWindowHandle, deps: WorkspaceDeps, cx: &mut App) -> WorkspaceHost;

pub struct WorkspaceDeps {
    /// 窗口状态变化时调用（去抖由 `app` 做）。在主线程上调用（状态机在主线程上），`app` 可以直接在这里切换 frame autosave 名
    /// （`project` 有值时 `"project"`，否则 `"welcome"`，见「对其他 crate 的要求」D）。
    pub on_state_changed: Arc<dyn Fn(WindowState) + Send + Sync>,
    /// 里程碑画出后调用，`presented` 是下一帧回调的时刻（见「已知限制」）。任何线程。
    pub on_milestone: Arc<dyn Fn(Milestone, SystemTime) + Send + Sync>,
    /// 一段帧记录结束后调用（bench 设计「需要其他 crate 提供的接口」：滚动、动画、输入的逐帧时刻）。任何线程。
    pub on_frames: Arc<dyn Fn(FrameRecord) + Send + Sync>,
}

/// 一段记录的逐帧时刻，`frames` 是 `platform::frame::FrameRecorder::stop` 的结果；`app` 转成 bench 的 `FrameStats`。
pub struct FrameRecord { pub scenario: Name, pub frames: Vec<lectern_platform::frame::FrameTimes> }

/// `Send + Sync`。`app` 持有一份，`WorkspacePlugin` 持有一份。
#[derive(Clone)]
pub struct WorkspaceHost { /* Arc<{ 主线程队列的发送端, Mutex<Option<AnyWindowHandle>>, 贡献注册表 }> */ }

impl WorkspaceHost {
    /// 点 Dock 图标重新打开窗口时（app 设计「界面」），`app` 开好新的骨架窗口后调用：替换它的根视图，
    /// 沿用注册表和导航状态。主线程。
    pub fn attach(&self, window: AnyWindowHandle, cx: &mut App);

    /// `app` 读到窗口状态文件后调用：有项目路径时 `dispatch(OpenProject)`，检查器按 `inspector` 恢复。任何线程。
    /// `WorkspacePlugin::started` 还没有拿到 `dyn Projects` 时（读文件比激活快），命令留在主线程队列里，`started` 之后执行。
    pub fn restore(&self, state: WindowState);

    /// 开始记录当前窗口的逐帧时刻（`platform::frame::record`），期间每帧都请求下一帧，
    /// 到 `stop` 的条件满足时结束，经 `on_frames` 交回。bench 的 `Driver`（`app` 实现）在滚动、动画和输入场景里调用。任何线程。
    pub fn record_frames(&self, scenario: Name, stop: RecordStop);
}

pub enum RecordStop {
    /// 固定时长之后。
    After(Duration),
    /// 当前进行中的动画全部结束之后：`ui` 的 `Transition` / `Spring` 有一个「还有进行中的过渡」的计数（「对其他 crate 的要求」B）。
    AnimationsSettled,
    /// 调用方再次调用 `stop_recording` 时。
    Manual,
}
impl WorkspaceHost {
    pub fn stop_recording(&self);

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
| Toolbar | 52pt；标题 + 副标题；左侧返回按钮（有 `back_button` 的屏幕）；右侧至多 3 个控件 | 常驻 | 本 crate 画容器和返回按钮；标题、副标题、右侧控件由当前屏幕经 `ui::contrib::Screen::toolbar` 提供（下节） |
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
    reading_column: Vec<(usize, bool)>, // `SetReadingColumn` 的覆盖：(栈深度, 值)，深度 0 是基底屏幕
}
```

当前屏幕是栈顶，栈空时是基底（`Welcome` → `screens::WELCOME`，`Project` → `screens::PROJECT_HOME`）。各区域的显示由当前屏幕的 `ScreenLayout` 和 `Nav` 决定：侧栏 = `layout.sidebar == Shown`；检查器 = `layout.inspector && nav.inspector`；面板、覆盖层只看 `nav`；浮条 = `layout.floating_bar`。内容区是否限制在阅读栏里 = 当前屏幕在 `nav.reading_column` 里的值，没有时是 `layout.reading_column`。

| 当前状态 | 输入 | 守卫 | 结果 |
|---|---|---|---|
| 任意 | `ProjectEvent::Opened { Ok(key) }`，`key` 不是当前基底 | — | `base = Project(key)`，`stack` 清空（先问 `View` 守卫，被拒时不换，见「状态与异常」），`sidebar_panel = None`；`ScreenChanged`；`on_state_changed`。原来的基底是 `Project(old)` 时随后调 `Projects::close(old)`：一个窗口同一时刻只显示一个项目，`Projects` 自己不限制同时打开的数目，不关的话旧项目的监听、任务和快照一直留着 |
| `Project(k)` | `ProjectEvent::Opened { Ok(k) }`（同一个项目再次打开：切换 worktree 或刷新） | — | 不换基底、不清栈 |
| `Project(k)` | `ProjectEvent::Closed { k }` | — | `base = Welcome`，`stack` 清空，面板、覆盖层、检查器关闭；`ScreenChanged`；`on_state_changed` |
| 任意 | `ProjectEvent::Closed { other }`（不是当前基底，如上一行关掉的旧项目） | — | 忽略 |
| `Welcome` | `OpenProject(p)` / `PromptOpenProject` | — | `Projects::open(p)`；等 `Opened` |
| `Project(k)` | `OpenProject(p)` | `Project(k)` | 通过后 `Projects::open(p)`（第一版在同一个窗口里换项目，见「未决问题」1）；`p` 属于 `k` 时 `Opened` 带同一个 key，按上面的「同一个项目再次打开」处理，否则 `Opened` 换基底并关闭 `k` |
| `Project(k)` | `CloseProject` | `Project(k)` | 通过后 `Projects::close(k)` |
| `Project(k)` | `SetActiveWorktree(w)` | `Worktree(k)` | 通过后 `Projects::set_active_worktree(k, w)` |
| `Project` | `Push(r)` | — | `r` 的屏幕已注册：压栈，`panel`、`overlay` 关闭；没注册：记 warn，不变 |
| `Project`，栈非空 | `Pop` | `View` | 弹出栈顶，`panel`、`overlay` 关闭 |
| `Project`，栈非空 | `PopToBase` | 每层一次 `View` | 逐层弹出，任一层被拒即停在那一层 |
| 任意 | `Escape` | 见下 | 关闭最上面的一层 |
| 任意 | `ShowSidebarPanel(p)` | — | 面板已注册才换 |
| `layout.inspector` | `SetInspector` / `SelectInspectorTab` | — | 改 `inspector`、`inspector_tab`；`LayerChanged`；`on_state_changed` |
| `Project` | `SetReadingColumn(v)` | — | 当前屏幕（栈深度）的覆盖设为 `v`；屏幕被弹出、项目关闭或切换、屏幕的贡献被撤销时清掉 |
| 任意 | `OpenPanel` / `OpenOverlay` | — | 已注册才开；同一时刻各至多一个，新的替换旧的 |
| 任意 | `CloseWindow` | `Window` | 通过后 `remove_window`，`WindowClosed` |
| `Welcome` | `Pop`、`PopToBase`、`Push` | — | 忽略，记 debug |

`Escape` 的顺序：覆盖层 → 面板 → 替换侧栏的面板（回到项目侧栏）→ 栈顶屏幕（`Pop`，经 `View` 守卫）→ 都没有时不做任何事。当前屏幕的视图先收到 Esc（它可能关闭自己的查找栏），没有处理时才冒泡到本 crate。⌘[ 同样先给当前屏幕，没有处理时是 `PopToBase`（「Esc · ⌘[：关闭面板 · 返回 Overview」）。阅读视图是例外：它自己处理 ⌘[（后退），退到历史起点之后 `dispatch(Pop)` 回到打开它的那个屏幕（审阅或项目主页，[file_reader 设计「Action 与快捷键」](file_reader.md#action-与快捷键)），不是 `PopToBase`。

守卫的询问：对一次命令，按登记顺序逐个 `ask(scope, done)`，前一个 `done(true)` 之后才问下一个；任一个 `done(false)` 就作废这次命令。询问期间到达的同范围命令排队，不并发询问。守卫的 `ask` 在它的激活准入内调用；`enter()` 失败的守卫跳过。

### 贡献注册表

一个进程一份（第一版一个窗口，注册表不按窗口分），在 `install` 里建，`Arc` 由 `WorkspaceHost` 和 `dyn Contributions` 共享：

```rust
struct Registry {
    entries: Mutex<HashMap<(ContributionKind 的判别, Name), Entry>>,
    sidebar_entries: Mutex<Arc<[SidebarEntry]>>, // `sidebar_entries()` 取锁复制 Arc，与 `Projects::snapshot` 相同
    commands: Mutex<HashMap<Name, CommandEntry>>,
    leave_guards: Mutex<Vec<GuardEntry>>,
    key_bindings: Mutex<Vec<(ActivationToken, Vec<Binding>)>>,
}
struct Entry { token: ActivationToken, ctx: lectern_logging::Ctx, payload: Payload /* 应用时 downcast 成 ui::contrib 的类型 */, order: u16, … }
```

- 登记（任何线程）：写进表、生成一个序号，返回 `Registration`。`activate` 期间的登记（`token` 为 `None`）只进表、不发事件；`commit(token)` 把 token 填进条目并排一次「应用」到主线程（`token.pending()` 守卫随队列项，取不到就丢弃）。`started` 之后的登记带 `Some(token)`，直接排「应用」；token 已经 `Dead` 的在登记时拒绝（warn `token-dead`），不进表。
- 应用（主线程）：检查 `token.state() == Live`（否则丢弃）；按 kind 把条目加进相应的集合：`SidebarEntry` 重建 `sidebar_entries` 快照并发 `ContributionsChanged`；`KeyBindings` 用 `bind_keys` 追加到 keymap，注销时不删：GPUI 只有 `bind_keys` 和 `clear_key_bindings`，后者会清掉 `app` 的 ⌘Q、gpui-component 输入框的绑定等不归本 crate 的绑定，所以不用。GPUI 分发一次按键时按优先级逐个尝试匹配的绑定，一个 Action 没有被处理就继续下一个（gpui-pre 0.3.9 `window.rs` 的 `dispatch_key_event`）；贡献方停用后它的处理函数随视图一起 drop，留下的绑定落空、不挡住其他绑定（「已知限制」）。`key_bindings` 表只记下哪些绑定属于哪次激活，供探针和调试。`Screen` 等只登记工厂，视图在需要时才建。应用完给这次激活的「已应用」标记置位（「按命令激活」第 3 步）。
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
kernel 线程：welcome / project_home / … 的 activate：Contributions::register(…, None) ─▶ ctx.own_staged ─▶ commit(token) ─▶ [主线程] 应用 ─▶ 重绘
kernel / executor 线程：started、监听、任务里：Contributions::register(…, Some(handle.token())) ─▶ [主线程] 应用（token 还是 Pending 时等到 Live）─▶ 重绘
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
- `ui` 的控件与动效原语（名字以 `ui` 设计为准，[#214](https://github.com/SoloJiang/lectern-app/issues/214)）：颜色经 `lectern_ui::colors(cx)`，文字经 `Text` 与 `TextRole`，分段控件 `Segmented`，按钮 `Button`；动效用 `motion::{PANEL_OPEN, PANEL_CLOSE, SEGMENT, HOVER_IN, HOVER_OUT}` 和可打断的 `Transition`（面板偏移、透明度），减弱动态效果由 `motion::reduced(cx)` 给出淡入淡出、时长减半的版本。侧栏、工具栏、检查器、面板、覆盖层、浮条的容器本身由本 crate 画（`ui` 只给数值和控件）：侧栏用窗口的 `Blurred` 材质（app 设计「界面」），不自定义材质。本 crate 不写任何颜色值、字体和动画参数（验收用 `rg` 检查）。
- Fluent 键（`crates/i18n/locales/{en,zh-CN}/workspace.ftl`）：

  | 键 | en | zh-CN | 用途 |
  |---|---|---|---|
  | `workspace-back` | Back | 返回 | 工具栏返回按钮的辅助名 |
  | `workspace-inspector-toggle` | Inspector | 检查器 | 工具栏右侧控件的辅助名 |
  | `workspace-command-failed` | This command isn't available right now. | 这个命令现在不可用。 | 代理 Action 激活失败或没有处理函数时的一次性提示 |
  | `workspace-screen-missing` | Nothing to show here yet. | 这里还没有内容。 | 基底屏幕没有注册时内容区的一行次要文字 |

  工具栏标题、副标题、侧栏条目的文字都来自贡献方的 Fluent 键，本 crate 不翻译、不拼接。

- UI 探针的语义节点（[#6](https://github.com/SoloJiang/lectern-app/issues/6) 的契约：`lectern_ui::semantic` 的 `semantic(spec, child)` 包装元素，角色取它的 `Role`，ID 是 `segment('.' segment)*`，`name[key]` 的 key 用业务键；窗口根节点 `window[<id>]` 由探针合成，本 crate 不声明）：

  | ID | `Role` | `attrs` |
  |---|---|---|
  | `workspace.sidebar` | `Sidebar` | `panel`（当前面板 ID）；隐藏时不出现 |
  | `workspace.sidebar.panel[<id>]` | `Group` | — |
  | `workspace.toolbar` | `Toolbar` | `screen`（当前屏幕 ID）、`depth`（栈深度） |
  | `workspace.toolbar.back` | `Button` | — |
  | `workspace.toolbar.inspector` | `Toggle` | `State::checked` = 检查器是否显示 |
  | `workspace.content` | `Group` | `base`（`welcome` / `project`）、`column`（`reading` / `full`） |
  | `workspace.content.screen[<id>]` | `Group` | `index`（栈里的位置，基底为 0） |
  | `workspace.inspector` | `Panel` | `tab`（当前页 ID）；隐藏时不出现 |
  | `workspace.inspector.tabs` | `TabBar` | — |
  | `workspace.inspector.tab[<id>]` | `Tab` | `State::selected` |
  | `workspace.panel[<id>]`、`workspace.overlay[<id>]` | `Panel` | 关闭时不出现 |
  | `workspace.bar` | `Group` | 只在审阅出现 |
  | `workspace.notice` | `Text` | `Label::key` 是消息键 |

  贡献的视图在各自的节点下导出自己的子树（`project_home.*`、`review.*`、`file_reader.*`）。本 crate 的 Action 名稳定、可枚举（探针按名字触发）。

- Action 与快捷键（本 crate 的；都能被探针触发）：

  | Action | 键 | 上下文 | 处理 |
  |---|---|---|---|
  | `workspace::OpenFolder` | ⌘O | 全局 | `PromptOpenProject` |
  | `workspace::CloseWindow` | ⌘W | 全局 | `CloseWindow` |
  | `workspace::Back` | ⌘[ | `Workspace`（屏幕没处理时冒泡到这里） | `PopToBase` |
  | `workspace::Escape` | Esc | `Workspace` | `Escape` |
  | `workspace::ToggleInspector` | ⌥⌘I | 全局（原定 `Workspace > project`，见「与设计的差异」第 2 步） | `SetInspector(!inspector)`；没有检查器的屏幕上状态机忽略 |
  | `workspace::ShowConversation` | ⌘\ | 全局（同上） | `SelectInspectorTab("conversation")`；同上 |
  | `workspace::Command { name }` | 由贡献的 `KeyBindings` 绑定 | 任意 | 代理（「数据流与线程」） |

  KeyContext 的名字：根视图 `Workspace`，有项目时同一个节点上加 `project`；侧栏 `Sidebar`，内容区 `Content`，检查器 `Inspector`，面板 `Panel`，覆盖层 `Overlay`。贡献的屏幕在自己的根元素上设自己的上下文（如 `reader`、`review`），`KeyBindings` 的谓词写成 `Content > reader` 这类；⌘K 等由 `Command` 激活的命令只经 `workspace::Command` 绑定。⌘[ 在编辑时归 `InputState`（file_reader 设计「编辑」），它在 `Content > reader > editing` 上先处理，不会冒泡到这里。

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
  - error `leave guard panicked`：`activation`（守卫所属的激活序号）；error `contribution drop panicked`：`kind`、`id`、`activation`；error `contribution closure panicked`（视图工厂、工具栏闭包）：`kind`、`id`、`activation`；error `command handler panicked`：`command`、`activation`。四者都在 `contain` 捕获之后记录，随后用贡献方的 token 调 `ActivationToken::fail`，不展开到主线程。warn `key binding rejected`：`reason`（`context` 或 `keystrokes`：贡献的 `KeyBindings` 里解析不了的一条，其余照常绑定）。 warn `contribution released without a commit`：`kind`、`id`、`activation`（没有 commit 就释放的登记：激活在提交前失败（kernel 已记录），或在 `activate` 之外传了 `None` 登记的，没有人提交它）。
- 详细级别：debug `command`（`WorkspaceCommand` 的判别名、是否经过守卫、结果 `done` / `refused` / `ignored`）；debug `layer changed`（层、开关）；debug `milestone`（名字、`presented` 的微秒数）；debug `leave guard skipped`（守卫已注销或激活不再 `Live`，按通过处理）；debug `stale guard answer`（答复属于已结束的一轮询问）；debug `leave guard dropped its answer`（守卫没有调用 `done` 就把它 drop，按通过处理）；debug `contribution dropped before it applied`；trace `job`（队列项的来源插件与耗时）。
- 关联 ID：调用贡献方闭包时恢复登记时取的 `Ctx`（`plugin`、`act`），所以贡献方的记录和本 crate 在那段代码里的记录都带它们；本 crate 自己的状态机代码在 kernel 为 `workspace` 设的作用域之外运行（主线程），记录带 `window`，不带 `plugin`。
- 不记录：项目路径（`OpenProject` 的参数、窗口状态里的路径）、`prompt_for_paths` 返回的路径、工具栏的标题与副标题、侧栏条目的文字、提示的文字、`Target` 里的会话标识之外的内容（`ReviewTarget`、`ReadTarget` 只记判别名）。用屏幕 ID、贡献 ID、计数和错误种类代替。
- 金丝雀测试：`canary_stays_out_of_workspace_logs`（`src/tests/logs.rs`）：用 `lectern_logging::testing::capture()`，在 trace 级别 `dispatch(OpenProject(含金丝雀的路径))`、注册一个标题含金丝雀的屏幕并切换到它、`restore(WindowState { project: 含金丝雀 })`，`assert_absent(金丝雀)`。

## 测试

第 2 步起依赖 GPUI，只在 macOS 上编译和测试（第 1 步不依赖 GPUI，Linux 上也跑）：`just test -p lectern-workspace`。

- 状态机（纯函数，`#[test]`，不依赖 GPUI；`src/tests/nav.rs`，用假的 `nav::Env`）：「导航状态机」表的每一行一个测试；`escape_closes_the_topmost_layer`（覆盖层 → 面板 → 侧栏面板 → 栈顶）；`guards_are_asked_in_order_and_a_refusal_stops`；`commands_during_a_pending_ask_queue_up`；`pop_to_base_stops_at_a_refusal`；`switching_projects_closes_the_old_one`（`Opened` 带新 key 后假 `Projects` 收到旧 key 的 `close`）；`opened_with_a_refusing_view_keeps_the_screen`；`window_state_follows_the_open_path`。
- 注册表（`#[test]`，`src/tests/registry.rs`，用 `TestKernel` 跑真实激活取 token，`driver::Driver` 代替主线程）：`duplicate_ids_are_rejected`、`wrong_payload_is_rejected`、`invalid_ids_are_rejected`、`staged_registration_applies_on_commit_only`、`dead_token_is_not_applied`、`pending_token_is_deferred`（`started` 还没返回时 token 是 `Pending`，应用推迟到下一个 `Tick`）、`late_registration_applies_with_its_token`、`late_registration_needs_no_earlier_commit`（`activate` 里什么也没登记的激活在 `started` 里带 token 登记屏幕、守卫和命令处理函数，都应用）、`late_registration_inside_started_is_deferred_until_live`、`late_registration_with_a_dead_token_is_rejected`（停用后用旧 token 登记：不排队、不占 ID，记 `token-dead`）、`removal_drops_the_shown_screen`、`release_frees_the_id_before_the_main_thread_removes_it`、`screens_can_be_pushed_twice`（第 1 步只检查注册表那一半：同一个屏幕压栈两次、载荷取两次；工厂真的被调用两次在第 2 步）、`a_guard_that_drops_its_answer_allows`、`a_registration_without_a_token_outside_activate_is_reported_on_release`、`sidebar_entries_are_sorted_and_shared`、`command_handlers_guards_and_key_bindings_apply`、`leave_guards_are_asked_through_the_queue`、`commands_wait_for_projects`、`project_events_reach_the_state_machine`、`dispatch_after_shutdown_is_dropped`、`plugin_manifest_matches_the_design`。`stale_bindings_fall_through`（一个插件停用后，它绑定的键落到另一个插件在同一个键上的处理函数；`app` 的 ⌘Q 仍然有效）要 GPUI 的 keymap，归第 2 步。
- `#[gpui::test]`（`TestAppContext`，`TestKernel` 注入假的 `dyn Projects`）：`install_replaces_the_root_and_shows_nothing_before_a_screen_registers`、`project_home_shows_after_opened`（假 `Projects` 发 `Opened`，侧栏与内容区出现）、`review_hides_the_sidebar_and_shows_the_bar`、`escape_and_back_return_to_the_home`、`inspector_toggles_with_alt_cmd_i`、`close_window_asks_the_guard_first`、`proxy_action_activates_the_command_plugin_and_replays_once`（`Command` 激活的假插件：第一次触发激活并执行一次，失败时提示一次）、`contribution_closures_run_in_the_plugins_scope`（`logging::testing::capture()` 里看 `plugin`、`act`）、`restore_opens_the_last_project`、`state_changes_are_reported`、`inspector_commands_are_ignored_without_an_inspector`（项目主页上 ⌥⌘I、⌘\ 不改状态）、`record_frames_reports_once_per_scenario`（`After`、`Manual` 两种停止方式各一次 `on_frames`，`platform::frame::record` 用 `test-support` 的假记录器）。
- 语义树：`workspace_tree_welcome`、`workspace_tree_project_home`、`workspace_tree_review_with_inspector` 的 insta 快照（UI 探针完成后接入；之前用本 crate 自己的 `Inspect` 输出）。
- 日志：`canary_stays_out_of_workspace_logs`。
- 不用真实的 git、会话扫描和文件 IO；`Projects` 用 `services` 的 trait 对象的假实现，只发事件、返回构造好的快照。

## 对其他 crate 的要求

每一项是一个 PR，写 `Refs #215`，在「验收」里列为前置条件。

- **A. `services`**：`workspace` 模块（「服务接口」）。`Registration`、`Payload`、`LeaveGuard` 的回调由本 crate 定义内容；`ReviewTarget`、`ReadTarget`、`ProjectKey`、`WorktreeKey` 复用已有的模块。
- **B. `ui`**（[#214](https://github.com/SoloJiang/lectern-app/issues/214) 的设计，另一个会话并行编写）：`ui::contrib` 的载荷类型与 `into_payload`（「`ui::contrib` 的载荷类型」，`SidebarItem.icon` 是 `Option<ui::Icon>`，`ui` 内嵌的 Lucide 图标）已在它的设计（[#218](https://github.com/SoloJiang/lectern-app/pull/218)）里；本 crate 用到而 `ui` 设计还没有列出的只有一次性提示控件，以及 `motion::active_transitions(cx) -> usize`（进行中的 `Transition` 与 `Spring` 的个数，`RecordStop::AnimationsSettled` 用）；`strings(cx)`、`colors(cx)`、`Text`、`Segmented`、`Button`、`motion::*`、`Transition`、`semantic` 模块已在它的设计里。`lectern_ui::ready(cx)` 之后视图才能挂载，所以 `app` 先调它、再调 `workspace::install`。
- **C. `i18n`**：`locales/{en,zh-CN}/workspace.ftl`，内容是「界面」的四个键。
- **D. `platform`**：窗口 frame 的 autosave（ADR 0017：「窗口位置与大小用 AppKit 的 frame autosave」）。锁定的 GPUI 不暴露这个设置，`gpui-pre-macos` 实现了 `raw_window_handle`：

  ```rust
  /// 给窗口设 AppKit 的 frame autosave 名（`NSWindow.setFrameAutosaveName:`）：AppKit 立即从应用偏好设置
  /// （内存里的 user defaults，不是 Lectern 自己的文件）恢复这个名字下保存的 frame，之后每次移动或缩放自动保存。
  /// 只在主线程调用；其他平台不做任何事。
  pub fn set_frame_autosave_name(window: &impl raw_window_handle::HasWindowHandle, name: &str) -> Result<(), WindowError>;
  ```

  `app` 在 `open_window` 之后、首帧之前对 Welcome 窗口设 `"welcome"`；之后在 `on_state_changed` 里按 `WindowState::project` 是否有值设 `"project"` 或 `"welcome"`（主线程）。`"project"` 第一次使用、还没有保存过 frame 时，`app` 先把窗口设为 1180 × 760 pt 居中。这是 ADR 0017 允许的首帧前唯一的偏好读取。`frame::record` 与 `FrameTimes` 是 bench 设计「需要其他 crate 提供的接口」已经列出的要求，本 crate 只调用。
- **E. `app`**：
  - `startup::release` 里 `lectern_ui::ready(cx)` 之后、`Kernel::start` 之前：`workspace::install(window, deps, cx)`，`WorkspacePlugin::new(host)` 放进 `PluginSet`（`Profile::Desktop` 的表加一行；test profile 不装）。`release` 在骨架窗口的帧回调里运行、窗口正被 GPUI 借着，`install` 自己把 `replace_root` 推到这次更新结束时做（「与设计的差异」第 2 步），`app` 不必换调用位置。
  - `on_reopen` 开好骨架窗口后调 `host.attach`。
  - 屏障放开后在 executor 的 `Background` 任务里读 `app_data()/workspace/window.json`，解析成 `WindowState`（认不出或没有文件时为默认值），调 `host.restore`；`app_data()` 为 None 时不读不写。`on_state_changed` 去抖 500 ms 后在 `Background` 任务里写同一个文件（写临时文件再 `rename`，不建目录以外的任何东西）。读写失败只记 warn（不记路径）。
  - `Quit` 的第 1 步：`host.ask_leave(LeaveScope::App, done)`，`done(false)` 时什么也不做。
  - 把 `Milestone` 转给探针的 `milestone`，把 `FrameRecord` 转成 `FrameStats` 交给 `frames`（`bench-probe` feature）；`Driver` 的滚动、动画和输入场景调 `host.record_frames(scenario, stop)`，输入场景用 `RecordStop::Manual`，注入结束后 `stop_recording`。
- **F. `bench`**：`Milestone` 的来源改为 `workspace::Milestone` 的映射（bench 设计「需要其他 crate 提供的接口」已写明由 `workspace` 报告）。

## 未决问题

产品负责人在 2026-10-23 决定：三个问题都按默认选项实现（一个窗口；启动时恢复上次的项目；上下文面板贴右侧滑入、占检查器的位置）。原文保留如下。

1. **一个窗口还是每个项目一个窗口。** [交互规范「快捷键」](interaction.md#快捷键)写 ⌘O「打开文件夹（新窗口）」，「窗口结构」写 Welcome 是「独立窗口」。`Projects` 已支持同时打开多个项目，但多窗口要把 `ProjectEvent` 按窗口路由、探针的语义树按窗口导出、frame autosave 按窗口命名。默认（本文按它写）：第一版一个窗口；没有项目时它是 Welcome，打开项目后同一个窗口换成项目窗口（frame autosave 名随之切换，所以两种状态各有自己的大小和位置）；已有项目时 ⌘O 先问离开守卫，通过后在同一个窗口里换项目；多窗口留给后续 issue，导航状态已按窗口保存。另一个选项：从第一版就每个项目一个窗口，Welcome 单独一个窗口，⌘O 总是开新窗口。
2. **启动时恢复上次的项目，还是总是先显示 Welcome。** 默认：恢复（Xcode、大多数文档类应用的做法；`Projects::open` 立即返回，Welcome 只闪现到 `Opened` 到达，项目主页 < 100 ms 可用）；上次停在 Welcome 时显示 Welcome。另一个选项：总是 Welcome，由用户选最近项目。
3. **上下文面板在哪里打开。** 交互规范写「浮在检查器上方」；检查器隐藏时面板贴右侧滑入，占检查器的位置（300pt）。默认按这个写；另一个选项是面板打开时自动显示检查器。

## 已知限制

- 里程碑的 `presented` 用下一帧回调的时刻，不是 `MTLDrawable.presentedTime`：与 app 设计「启动屏障」相同，`platform::frame::on_first_presented` 实现后改用它。
- 注册表不按窗口分：所有窗口共享同一套贡献（多窗口时也应如此，视图实例按窗口建）。
- 由系统发起的退出不问离开守卫（app 设计「已知限制」）。
- 进程插件不能贡献界面（「范围」）。
- GPUI 没有单条删除键绑定的接口，贡献方停用后它的绑定留在 keymap 里直到进程退出（落空，不影响其他绑定）；同一个插件停用又启用时绑定重复追加，只在用户反复切换设置时发生，几十条以内没有可感知的开销。
- `platform::frame::record` 实现之前，`record_frames` 交回的记录为空，帧时间的基准报告 `skipped`。

## 验收

分三步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-workspace`、`just deny` 通过。前置条件 A、C 各自的 PR 先合并；B（`ui` 设计与实现）在第 2 步之前；D、E、F 随第 3 步。

**第 1 步：`services::workspace` 与状态机、注册表（不依赖 GPUI 的部分）。** 两个 PR：前置条件 A（`services`），然后本 crate。

- `cargo check -p lectern-services`、`just test -p lectern-services` 通过；`services` 的 AGENTS.md 加上 `workspace` 模块一行。
- `rg 'gpui' crates/services/src` 没有结果。
- `cargo check -p lectern-workspace`、`just test -p lectern-workspace` 通过：「测试」里状态机、注册表的测试和 `canary_stays_out_of_workspace_logs` 通过。
- `rg 'gpui' crates/workspace/src` 没有结果：这一步不加 `gpui`、`lectern-ui`、`lectern-i18n`、`lectern-platform` 依赖。`rg 'eprintln!|println!|log::' crates/workspace/src` 没有结果。
- `python3 scripts/check_architecture.py` 通过：`workspace` 带 `plugin = true`，只依赖 kernel 层（`lectern-kernel`、`lectern-services`、`lectern-logging`）和 `serde`。

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

## 实际验证

### `SetReadingColumn`（review_view 前置条件 A）

`services` 加 `WorkspaceCommand::SetReadingColumn(bool)`，`workspace` 的状态机、日志的命令名与根视图的 `layout` 跟着它，在 macOS（arm64）上运行：

- `just test -p lectern-workspace -p lectern-services`：175 个测试通过（`services` 99 个，`workspace` 76 个，新增 4 个）。`workspace_commands_round_trip` 加 `SetReadingColumn(true / false)`；`nav.rs` 的 `set_reading_column_overrides_the_top_screen_until_it_pops`（覆盖只对当前深度生效，上面压了屏幕时不继承、弹回后恢复，弹出时清掉）、`set_reading_column_is_ignored_without_a_project_and_cleared_with_it`、`set_reading_column_is_cleared_when_the_screen_is_removed`；`view.rs` 的 `#[gpui::test]` `set_reading_column_widens_the_content`（语义树的 `column=reading` → `column=full` → 弹出后回到 `reading`）。
- 没有验证：真实窗口里内容区变宽的外观与动画（`#[gpui::test]` 不画真实的帧），随 review_view 第 3 步手动检查。

### 前置条件 A

`services` 的 `workspace` 模块，在 macOS（arm64）上运行：

- `rg -n 'pub trait Contributions\b|pub trait LeaveGuard' crates/services/src` 各有一处（`workspace/mod.rs`）；`rg 'gpui' crates/services/src` 没有结果。
- `cargo check -p lectern-services`、`just check`、`just deny` 通过。
- `just test -p lectern-services`：99 个测试通过，新增 15 个，都以 `workspace_` 开头。
  - JSON 往返：`workspace_contribution_kinds_round_trip`（七种 `ContributionKind`、两种 `Visibility`、三种 `SidebarSection`）、`workspace_routes_and_targets_round_trip`（四种 `Target`，`Route` 的 `Name` 反序列化后是 `Owned`）、`workspace_commands_round_trip`（十六种 `WorkspaceCommand` 的 18 个值，含 `ShowSidebarPanel(None)` 与 `SetInspector(false)`）、`workspace_events_round_trip`（四种 `WorkspaceEvent`、四种 `Layer`）、`workspace_scopes_and_milestones_round_trip`（五种 `LeaveScope`、四种 `Milestone`、`WindowId`，并检查枚举是 serde 默认的外部标记）、`workspace_command_handler_receives_the_call`（`CommandCall`）。
  - 服务 ID：`workspace_contributions_service_id_and_multiple` 检查 `ID == "contributions"`、`MULTIPLE == false`。
  - 不是服务：`workspace_leave_guard_is_not_a_service` 检查 `dyn LeaveGuard` 没有实现 `Service`，同一个探针对 `dyn Contributions` 给出 true。
  - 守卫：`workspace_staged_registration_commits_with_the_activation_token` 用 `TestKernel` 跑一次真实激活：`activate` 里 `ctx.own_staged(Registration)`，`commit` 在提交时收到 token 一次（那一刻 `Pending`，激活完成后 `Live`），`release` 在停用时调用一次（token 已 `Dead`）；`workspace_registration_releases_once_on_drop` 没有提交就在另一个线程上 drop，只调用 `release`。
  - 载荷与条目：`workspace_payload_downcasts_to_its_type`（`Payload` 的 `downcast`、`SharedPayload` 的 `Arc::downcast` 与共享）、`workspace_sidebar_sections_order_top_to_bottom`、`workspace_screen_ids_are_valid_names`（六个屏幕 ID 都符合 `[a-z0-9_-]`、1 到 64 字节）。
  - trait 对象：`workspace_contributions_is_a_shared_trait_object`（`Arc<dyn Contributions>` 调用每个方法；`Send` / `Sync` 的静态检查）、`workspace_leave_guard_answers_through_done`。

### 第 1 步

本 crate 不依赖 GPUI 的部分（[#229](https://github.com/SoloJiang/lectern-app/pull/229)），在 macOS（arm64，Rust 1.97.0）上运行：

- `cargo check -p lectern-workspace`、`just check`（含 `cargo clippy --workspace --all-targets -- -D warnings` 和架构检查：32 个 crate，`workspace` 带 `plugin = true`）、`just deny` 通过。
- `just test -p lectern-workspace`：48 个测试通过，都是普通 `#[test]`。
  - `src/tests/nav.rs`（28 个）：「导航状态机」表的每一行一个（`opened_switches_the_base_and_closes_the_old_project`、`reopening_the_same_project_keeps_the_stack`、`closed_returns_to_welcome`、`closed_for_another_project_is_ignored`、`open_project_from_welcome_needs_no_guard`、`open_project_with_a_project_asks_the_project_guard`、`close_project_asks_the_project_guard`、`set_active_worktree_asks_the_worktree_guard`、`push_needs_a_registered_screen`、`pop_asks_the_view_guard`、`pop_to_base_asks_once_per_level`、`escape_closes_the_topmost_layer`、`show_sidebar_panel_needs_a_registered_panel`、`inspector_commands_follow_the_layout`、`panel_and_overlay_replace_each_other`、`close_window_asks_the_window_guard`、`welcome_ignores_stack_commands`），守卫（`guards_are_asked_in_order_and_a_refusal_stops`、`commands_during_a_pending_ask_queue_up`、`pop_to_base_stops_at_a_refusal`、`stale_guard_answers_are_ignored`），状态与异常（`switching_projects_closes_the_old_one`、`opened_with_a_refusing_view_keeps_the_screen`、`open_error_leaves_the_screen`、`screen_removed_pops_without_a_guard`），窗口状态（`window_state_follows_the_open_path`、`restore_sets_the_inspector_without_an_event`、`window_state_round_trips_as_json`）。
  - `src/tests/registry.rs`（19 个）：「测试」列出的注册表测试，经 `TestKernel` 的真实激活（`activate` 里 `own_staged`，`started` 里的晚登记，停用后的注销）和 `driver::Driver` 应用。
  - `src/tests/logs.rs`：`canary_stays_out_of_workspace_logs`：金丝雀放在项目路径（`OpenProject`、`Opened` 的 key、第二次 `OpenProject`）、屏幕载荷的标题和侧栏条目的文字里，trace 级别跑完打开、压栈、检查器、弹出、未注册屏幕、关闭项目、插件停用；没有记录含金丝雀，`screen changed` 的 `to` 是 `review`，`contribution applied` 的 `id` 是 `overview`。
- `rg 'gpui' crates/workspace/src`、`rg 'eprintln!|println!|log::' crates/workspace/src` 都没有结果。
- 没有验证：`stale_bindings_fall_through` 和「`Screen` 工厂被调用两次」要 GPUI，随第 2 步；`restore`、代理 Action、离开守卫与窗口关闭的联动也在第 2 步。

### 第 2 步前置：登记接口带 token

`Contributions::register`、`handle_command`、`register_leave_guard` 加 `token: Option<ActivationToken>` 参数（[#232](https://github.com/SoloJiang/lectern-app/pull/232)），在 macOS（arm64）上运行：

- `cargo check -p lectern-services -p lectern-workspace`、`just check`、`just deny`、`python3 scripts/check_architecture.py` 通过。
- `just test -p lectern-services -p lectern-workspace`：150 个测试通过（`services` 99 个不变；`workspace` 51 个，新增 3 个）。`src/tests/registry.rs` 现在 22 个：`late_registration_applies_without_a_commit` 改为 `late_registration_applies_with_its_token`（`started` 里带 token 登记，与 `activate` 里的登记一起应用；`activate` 之外传 `None` 的登记不显示）；`a_late_registration_without_any_commit_is_reported_on_release` 改为 `a_registration_without_a_token_outside_activate_is_reported_on_release`（释放时记 warn）；新增 `late_registration_needs_no_earlier_commit`、`late_registration_inside_started_is_deferred_until_live`（`started` 里登记后阻塞，主线程看到的 token 还是 `Pending`，推迟到 `Tick`）、`late_registration_with_a_dead_token_is_rejected`。

### 第 2 步

根视图、区域、代理 Action、离开守卫与窗口的联动、`WorkspaceHost`（[#235](https://github.com/SoloJiang/lectern-app/pull/235)），在 macOS（arm64，Rust 1.97.0）上运行，叠在 [#232](https://github.com/SoloJiang/lectern-app/pull/232) 与 `ui` 第 2 步（[#233](https://github.com/SoloJiang/lectern-app/pull/233)，`ui::motion`）之上：

- `just check`（含 clippy 与架构检查：32 个 crate，`workspace` 带 `plugin = true`，新增依赖 `gpui`、`futures`、`lectern-i18n`、`lectern-ui`，都不是插件 crate）、`just deny`、`python3 scripts/check_architecture.py` 通过。`git diff origin/main -- Cargo.lock` 只给 `lectern-workspace` 加依赖行，没有新的外部 crate（`futures` 0.3.34 已在锁文件里）。
- `just test -p lectern-workspace`：72 个测试通过（第 1 步的 51 个不变，新增 21 个）。
  - `src/tests/view.rs`（18 个 `#[gpui::test]`）：「测试」列出的 `install_replaces_the_root_and_shows_nothing_before_a_screen_registers`、`install_inside_the_windows_update_mounts_after_it`（从窗口的更新内部调 `install`，根视图在更新结束后被替换：`app` 的 `release` 正是这样调用的）、`project_home_shows_after_opened`、`review_hides_the_sidebar_and_shows_the_bar`、`escape_and_back_return_to_the_home`（Esc、`Back` Action、⌘[ 三种触发）、`inspector_toggles_with_alt_cmd_i`（⌥⌘I 两次、⌘\ 切到 conversation）、`inspector_commands_are_ignored_without_an_inspector`、`close_window_asks_the_guard_first`（`simulate_close` 返回 false，拒绝的守卫留住窗口，停用它后 ⌘W 关掉窗口）、`proxy_action_activates_the_command_plugin_and_replays_once`（真实 `Kernel::start`，一个 `Activation::Command` 插件：第一次触发激活并执行一次，第二次直接执行；未知命令和漏了 `handle_command` 的插件各提示一次并记 `command activation failed` / `command without handler`）、`contribution_closures_run_in_the_plugins_scope`（工厂里的记录带 `plugin`、`act`）、`restore_opens_the_last_project`（`started` 之前的 `restore` 等到 `dyn Projects` 到达才 `open`；认不出的 `version` 被忽略）、`state_changes_are_reported`、`record_frames_reports_once_per_scenario`（`After`、`Manual`，第二次 `stop_recording` 不重复报告）、`stale_bindings_fall_through`（第 1 步欠下的：停用的插件绑定的键落到另一个插件在同一个键上的处理函数，测试里全局绑定的 ⌘Q 仍然有效）、`screens_can_be_pushed_twice`（第 1 步欠下的另一半：同一个屏幕压栈两次，工厂被调用两次，弹出一个后不重建），另有 `open_folder_prompts_and_opens_the_chosen_path`（⌘O 打开系统面板，选中的路径变成 `OpenProject`）、`attach_moves_the_workspace_to_a_new_window`（第二个窗口的根视图被替换，导航状态保留）、`ask_leave_answers_through_the_guards`（没有守卫立即 `done(true)`，拒绝的守卫给 `done(false)`）。
  - `src/tests/semantic.rs`：`workspace_tree_welcome`、`workspace_tree_project_home`、`workspace_tree_review_with_inspector` 三份 insta 快照（`src/tests/snapshots/`），节点 ID 与角色按「UI 探针的语义节点」表。
  - `src/tests/logs.rs`：`canary_stays_out_of_workspace_logs` 加上 `restore(WindowState { project: 含金丝雀 })`。
- 「验收」第 2 步的 `rg` 检查都没有结果：`std::fs|std::process|git2|gix`、`rgb\(|hsla\(|Duration::from_millis`、界面字符串字面量（`'"[A-Z][a-z]+ [a-z]'`，`--glob '!tests*'`）、`eprintln!|println!|log::`；`ui` 设计「视图 crate 接入后的持续检查」的 `rg`（含 `\.font_family\(`、`gpui_component::Button`）也没有结果。
- 没有验证（`#[gpui::test]` 不画真实的帧：`TestWindow` 的帧请求回调只能从 GPUI 内部触发）：`on_next_frame` 驱动的每帧 `Tick`（测试直接发 `Item::Tick`）、里程碑的 `presented` 时刻、`RecordStop::AnimationsSettled`、面板滑入的视觉效果（`Transition` 的值在测试里不随帧推进）；窗口关闭后点 Dock 图标重开的真实流程（`attach` 只在测试窗口上验证）。这些随第 3 步在 macOS 上手动运行发布构建时检查。

### 第 3 步

`app` 的装配（前置条件 E）：`startup::release` 里 `install`、`WorkspacePlugin` 进 desktop 的 `PluginSet`、窗口状态文件、frame autosave 名、Dock 重开的 `attach`、`Quit` 问离开守卫、`Application::with_assets(IconAssets)`，在 macOS（arm64，Rust 1.97.0）上运行，叠在 `main`（含 `welcome` 第 1 步 #240、`project_home` 第 1 步 #241）之上。本机图形会话锁屏，下面标明的没有做。

- `just check`（含 clippy 与架构检查：32 个 crate）、`just deny`、`python3 scripts/check_architecture.py` 通过。`git diff origin/main -- Cargo.lock` 只给 `lectern-app` 加依赖行（`futures`、`lectern-services`、`lectern-workspace`），没有新的外部 crate。`rg 'eprintln!|println!|log::|log_err|warn_on_err' crates/app/src crates/app/tests` 没有结果。
- `just test -p lectern-app -E 'not binary(startup)'`：31 个测试通过（原有 17 个，新增 14 个）。新增：`src/tests/window_state.rs` 9 个（文件函数 5 个；`#[gpui::test]` 4 个：`changes_are_written_once_after_500_ms_of_stillness`、`the_name_for_a_new_window_follows_the_last_state`、`a_failed_write_is_logged_without_the_path`、`restore_ignores_what_it_cannot_use`）、`src/tests/leave.rs` 5 个（`Quit` 问离开守卫：通过、拒绝后再按一次再问、别的线程上的答复、丢掉答复、守卫还没回答）；`every_profile_graph_validates` 改成 `#[gpui::test]`，desktop 的插件数为 5。
- `just test -p lectern-workspace`：72 个测试通过，与第 2 步相同（本步没有改这个 crate 的代码）。
- 没有运行：`tests/startup.rs`（8 个，其中新增 `window_state_stays_out_of_the_log`，`startup_writes_a_header_and_an_end_record` 的 `plugins` 改为 5，`home_path_stays_out_of_the_log` 加检查 `window.json`）：锁屏时屏障不放开，全部 30 s 超时（[app 设计「已知限制」](app.md#已知限制)），结果以 macOS CI 为准。
- 没有做（「验收」第 3 步的手动项）：发布构建（临时 `HOME`）首次启动是 820 × 520 的 Welcome、没有 `workspace` 的记录早于 `first frame drawn`、移动窗口后重启位置保留、打开文件夹后重启回到项目主页（`screen changed` 的 `to` 为 `project-home`）；这一条还要 `welcome` 和 `project_home` 的屏幕装配（见「与设计的差异」第 3 步）。`app/cold_start`（bench 第 5 步之后判定）和空闲内存（与 40 MB 比较增加不超过 1 MB）也没有测。第 2 步留下的 `on_next_frame` 驱动的 `Tick`、里程碑的 `presented`、`RecordStop::AnimationsSettled`、面板滑入、窗口关闭后点 Dock 图标重开，同样要在真实窗口里手动检查。

## 与设计的差异

### `SetReadingColumn`（review_view 前置条件 A）

- **覆盖按栈深度记，不是一个 `Option<bool>`**：review_view 设计写 `Nav` 加一个 `reading_column: Option<bool>`、`Pop` 时清掉。审阅屏幕在双栏时会在它之上压入阅读视图（⇧⌘O），一个全局的 `Option` 会让阅读视图继承审阅的「整个内容区」，弹回审阅时又丢掉。所以覆盖是 `Vec<(usize, bool)>`（深度，值），只对深度等于当前栈深度的屏幕生效；弹出时去掉深于新栈的项，项目关闭或切换、屏幕的贡献被撤销（栈被改写）时全部清掉。
- **没有项目时忽略**：与 `Push`、`Pop` 一致；基底屏幕（项目主页）有项目时可以被覆盖，用不到但不特殊处理。

### 前置条件 A

- **`SidebarEntry` 与 `SharedPayload`**（「服务接口」已补上）：设计原来只写了 `sidebar_entries() -> Arc<[SidebarEntry]>`，没有定义 `services` 里的 `SidebarEntry`；`ui::contrib` 的条目载荷在视图层，`services` 不能引用。现在 `services` 定义 `SidebarEntry { id, section, order, payload: SharedPayload }`，`SharedPayload` 是 `Arc<dyn Any + Send + Sync>`：`Arc<[SidebarEntry]>` 要跨线程共享，元素必须 `Send + Sync`，而 `Payload` 的 `Box<dyn Any + Send>` 不是 `Sync`，所以本 crate 应用登记时把 `ui::contrib::SidebarItem` 换成 `Arc` 放进条目，`project_home` 用 `Arc::downcast` 取回。
- `ui::contrib` 的条目载荷由 `SidebarEntry` 改名为 `SidebarItem`（「`ui::contrib` 的载荷类型」已改）：`services` 的 `SidebarEntry` 用了这个名字，`project_home` 两个都要引用。`ui` 设计（#218）定义 `contrib` 的载荷类型，`SidebarItem.icon` 是 `Option<ui::Icon>`（`ui` 内嵌的 Lucide 图标），不是 `ui::Symbol`；「`ui::contrib` 的载荷类型」与「对其他 crate 的要求」B 已照改。
- 设计没有列出的派生（「服务接口」已补上）：数据类型都派生 `Hash`、`Serialize`、`Deserialize`，与 `services` 的其他模块相同（进程插件不贡献界面，序列化只为与其他模块一致和测试往返）；不含 `Arc` 的小类型派生 `Copy`；`SidebarSection` 派生 `Ord`，顺序即侧栏从上到下的顺序；`WindowId` 派生 `Ord`。`Contribution` 含 `Payload`，只实现 `Debug`（不打印载荷）。
- `Registration` 的两个回调都是 `FnOnce + Send`（不要求 `Sync`，`Staged` 只要求 `Send`）；没有提交就 drop 时只调用 `release`，本 crate 的注销回调要能处理「从未应用」的登记。
- `WorkspaceEvent::ScreenChanged` 按值带两个 `Route`（约 400 字节），clippy 的 `large_enum_variant` 用 `#[allow]` 并写明理由：每次导航一个事件、每个监听克隆一次，装箱只会让每个监听多一次间接访问。

### 第 1 步

- **模块划分**：设计把状态机、注册表和主线程队列都写在根视图旁边；第 1 步把不依赖 GPUI 的部分拆成四个模块，第 2 步的根视图只包一层。`nav::Navigator` 是状态机加守卫轮询，输入是 `WorkspaceCommand`、`ProjectEvent` 和守卫答复，环境（已注册的屏幕与面板、守卫列表、`Projects` 的三个调用）经 `nav::Env` 注入，输出是 `Effect` 列表（`Ask`、`Event`、`StateChanged`、`PromptOpenProject`、`CloseWindow`、`Notice`、`BindKeys`、`Milestone`）；`registry::Registry` 是注册表；`driver::Driver` 是主线程循环：取队列项（`hub::Item`：`Apply`、`Remove`、`Command`、`GuardAnswer`、`Project`、`Milestone`、`Projects`、`Tick`），调注册表和状态机，自己解决 `Ask`（取守卫的准入和日志作用域、`contain` 包住 `ask`，`done` 只把答复排回队列）；`hub::Hub` 是 `WorkspaceHost` 里不依赖 GPUI 的那份（注册表、队列发送端、本插件的 token），实现 `dyn Contributions`，`WorkspacePlugin::new(Arc<Hub>)`。第 2 步的 `WorkspaceHost` 持有 `Hub` 加窗口句柄，`install` 建 `Driver` 并把 `Effect` 变成视图、`bind_keys`、事件和回调。
- **队列的传输**：第 1 步没有加 `futures` 依赖，发送端是 `Sender = Arc<dyn Fn(Queued) + Send + Sync>`；第 2 步决定接收端用 `futures::channel::mpsc` 还是 GPUI 自己的通道，发送端照旧。队列项带 `Option<PendingGuard>`；`Hub::dispatch`、`report_milestone` 用本插件 `started` 时记下的 token 取守卫，`started` 之前的项没有守卫（还没有激活可守），`shutdown` 之后取不到就丢弃并记 debug。
- **登记接口带 token**（「服务接口」已改）：`started` 之后没有人再调用 `Staged::commit`，所以 `register`、`handle_command`、`register_leave_guard` 都加一个 `token: Option<ActivationToken>` 参数，三个方法规则相同。`activate` 里没有 token（`Context` 不给出，kernel 设计「激活时的上下文」里这正是 `Staged` 存在的原因），传 `None`，守卫经 `ctx.own_staged` 交出，`commit(token)` 时排「应用」；`started`、监听、任务里传 `Some(handle.token())`，登记时就排「应用」，`Pending` 的 token 由主线程推迟到 `Live`（见下一条），`Dead` 的在登记时拒绝（warn `token-dead`，不占 ID、不排队）。所以 `Option` 而不是像 `Review::register_brief_rules(provider, token)` 那样必带 token：那个接口只在 `started` 之后调用。第 1 步曾用 `lectern_logging::current().act` 认出已经 commit 过的激活，要求一个激活在 `activate` 里至少登记过一项，否则之后的登记要调用方自己 `commit`；这个条件和注册表里的 token 表已经去掉。在 `activate` 之外传 `None` 的登记没有人提交，留在暂存态、不显示，守卫 drop 时记 warn `contribution released without a commit`（激活在提交前失败时同样记，kernel 也记了）。
- **token 为 `Pending` 时推迟，不丢弃**：kernel 在 `started` 之前调用 `commit`，队列项到主线程时 token 可能还是 `Pending`；设计写「检查 `token.state() == Live`（否则丢弃）」会把这类登记丢掉。注册表的 `apply` 对 `Pending` 返回 `Deferred`，`Driver` 把项留到下一个 `Tick`（第 2 步每帧发一个，直到没有推迟的项）；`Dead` 才丢弃并记 `token-dead`。「已应用」标记按激活计数：这次激活排进队列的应用全部应用或丢弃后 `activation_applied(act)` 为 true。
- **注销先释放 ID**：守卫 drop 时在调用线程上立即从索引里去掉 `(kind, id)`，同一个 ID 可以马上再登记（插件停用后重新启用不必等主线程），载荷仍排到主线程在 `contain` 里 drop；没有 commit 过的登记没有主线程工作，直接在调用线程上 drop。
- **守卫 panic**：kernel 没有公开用 `ActivationToken` 让别人的激活失效的接口（`PluginHandle::fail` 只对自己的激活），所以 `ask` panic 时只记 error `leave guard panicked`，这个守卫按通过处理，不让用户困在当前视图；视图工厂和命令处理函数的 panic 同样。第 2 步前要在 kernel 加这个接口，kernel 设计「撤销」要求 panic 让贡献方的激活失效。
- **命令名允许点**：kernel 的 `Activation::Command` 用 `terminal.open` 这样的名字，`handle_command` 的名字按 `[a-z0-9_.-]`、1 到 64 字节检查，不是贡献 ID 的 `[a-z0-9_-]`；日志里运行时构造的贡献 ID 和命令名都经本 crate 的一张固定上限（1024）的驻留表变成 `&'static str`，不占 `PluginTag` 表的槽位（那是进程插件的）。`register_leave_guard` 没有 ID，条目用序号作 ID，不查重。
- **`Opened` 时的 `View` 守卫只问一轮**：栈非空时按登记顺序问一轮 `View` 守卫，通过后整栈清空；`PopToBase` 仍是每层一轮。
- **询问期间所有输入都排队**：设计写「同范围命令排队」；实现里一轮询问期间到达的全部命令和项目事件都排在后面，按顺序执行，简单且不会交错两个守卫范围。
- **`restore` 的检查器**：`Navigator::restore_inspector(bool)` 直接设 `nav.inspector`，不查当前屏幕的 `layout.inspector`、不发事件：恢复发生在 Welcome 上，那里按命令处理会被忽略；之后第一个带检查器的屏幕按这个值显示。
- **`ScreenChanged.from`**：状态机总是知道上一个屏幕，事件的 `from` 总是 `Some`；第一次显示（`None`）由根视图在第 2 步发。
- **依赖**：第 1 步只加 `lectern-kernel`、`lectern-services`、`lectern-logging`、`serde`（dev：两者的 `test-support`、`serde_json`）。`gpui`、`lectern-ui`、`lectern-i18n`、`lectern-platform`、`futures`、`insta` 随第 2、3 步。因此本 crate 目前在 Linux 上也编译和测试（`just test` 的默认包集按依赖图算），第 2 步加 `gpui` 后自动退出 Linux 的集合。
- **载荷类型的检查**：`ui::contrib` 还不存在（`ui` 设计 #218 在写），注册表经构造时注入的 `registry::Payloads`（`accepts(kind, &Payload)`、`share(Payload) -> Option<SharedPayload>`）检查载荷类型并把侧栏条目的载荷换成 `Arc`；第 2 步用 `ui::contrib` 的类型实现它，测试用假类型。
- **`WorkspaceCommand::SelectInspectorTab` 不查页是否已注册**：页可以晚于命令注册（Conversation 按命令激活），检查器显示空页直到它到达；`ShowSidebarPanel`、`OpenPanel`、`OpenOverlay` 仍要求已注册。

### 第 2 步

- **⌥⌘I、⌘\ 的绑定不带 KeyContext**（「Action 与快捷键」表已改）：设计写 `Workspace > project`。锁定的 GPUI 里 `A > B` 要求 `B` 出现在分发路径最深的那个 KeyContext 上（`KeyBindingContextPredicate::eval_inner` 对右侧只看 `contexts.last()`），而最深的是内容区（`Content`）或屏幕自己的上下文，所以这个谓词在根视图之下永远不成立；而且 GPUI 按谓词匹配到的深度选绑定，没有上下文的绑定按最深算，gpui-component 的调试 inspector（`debug_assertions` 下自动开启）正是这样绑定的 ⌥⌘I，带 `Workspace` 上下文的绑定会被它盖住。两条都改为不带上下文（同为无上下文时后绑定的优先，所以本 crate 的赢）；在 Welcome 和项目主页上状态机本来就忽略这两个命令（`inspector_commands_are_ignored_without_an_inspector`）。根视图的 KeyContext 仍是 `Workspace`，有项目时同一个节点加 `project`，供贡献方的谓词用。
- **`WorkspaceHost::ask_leave` 经状态机的一轮询问**：`Navigator::ask_leave(scope, done)` 开一轮只报告结果的询问（`Then::Report`），通过时 `done(true)`，任一守卫拒绝时 `done(false)`，没有守卫时立即 `done(true)`；询问期间到达的输入与其他命令一样排队。`Input::AskLeave` 加进状态机的输入。
- **队列项的新增**（`hub::Item`）：`Started { control, token }`（`started` 把 `KernelControl` 与本插件的 token 交给驱动，驱动用前者激活命令插件、用后者的 `pending()` 守住 `done` 的回复）、`Proxy(name)`（代理 Action 触发）、`Replay { name, token }`（激活成功后的重放，等 `activation_applied` 为 true，token 已 `Dead` 时提示）、`CommandFailed { name, error }`（激活失败的回复）、`AskLeave`、`Restore(WindowState)`（驱动直接设 `restore_inspector`，有路径时按 `OpenProject` 处理，`dyn Projects` 未到时推迟）、`RecordFrames`、`StopRecording`（根视图处理，不进驱动）。`RecordStop` 定义在 `hub` 里，不依赖 GPUI。
- **`activation_applied` 对没有排过任何登记的激活为 true**：第 1 步写「没有排队的激活为 false」，这会让漏了 `handle_command` 的插件（激活成功、什么也没登记）的重放永远推迟，与「按命令激活」第 4 步「提示一次、不一直等待」冲突。一次激活的 `commit` 都在 `started` 返回之前排队，`done` 在 `started` 返回之后才调用，所以 token 为 `Live` 时 `outstanding` 里没有它的条目就是「全部应用完」。
- **贡献方闭包 panic 时调 `ActivationToken::fail`**（kernel [#231](https://github.com/SoloJiang/lectern-app/pull/231) 提供）：守卫的 `ask`、命令处理函数、视图工厂与工具栏闭包、载荷的 `Drop` 四处都在 `contain` 捕获后、仍持有准入时调用 `token.fail(PluginError::Panicked(…))`，第 1 步「只记日志、按通过处理」的写法去掉；守卫仍按通过处理（不让用户困在当前视图）。
- **提示的键**：`Notice::ProjectOpenedWhileAViewRefusedToLeave` 用 `workspace-command-failed` 显示（「界面」只定义了四个键，打开项目也是一个命令）；代理失败的三种情况（激活失败、没有处理函数、重放时 token 已 `Dead`）用同一个键。一次性提示没有计时：`ui` 的 `Hint` 控件在 `ui` 第 3 步，现在提示留到下一个命令或屏幕切换时清掉。
- **语义节点是数据，不是元素**：`ui::semantic`（`ui` 第 4 步）还没有，本 crate 的 `semantic` 模块按 `ui` 设计的名字和形状定义 `NodeId`、`Role`、`Label`、`State`、`Semantic`，另加 `NodeId::keyed(name, key)`（`workspace.content.screen[review]` 这种形状）和 `Tree` / `Node` / `render`；`Workspace::inspect(cx)` 从导航状态和注册表算出这一帧的节点表（「UI 探针的语义节点」的表），快照测试用它。`ui::semantic` 到位后换成它的类型并用 `semantic(spec, child)` 包住各区域的元素，`inspect` 保留给测试。
- **帧记录**：`platform::frame` 还没有，`FrameTimes` 暂定义在本 crate（`host` 模块，只有 `presented`），`FrameRecord::frames` 总是空（「已知限制」）。`RecordStop::After` 用 GPUI 的 `background_executor().timer`，`Manual` 等 `stop_recording`，`AnimationsSettled` 每帧看 `motion::active_transitions`。新的记录开始前先把进行中的那次报告出去。本 crate 因此没有加 `lectern-platform` 依赖。
- **根视图在一个 GPUI 全局里**：`install` 建 `Entity<Workspace>` 放进全局 `Shell`，窗口的根是只渲染它的 `WindowRoot`；`attach` 在新窗口上重做 `replace_root`、`lectern_ui::attach`、`on_window_should_close` 和焦点，已建的贡献视图丢弃、按需重建，注册表与导航状态不动。`WorkspaceHost` 只持有 `Arc<Hub>` 和窗口句柄（`Entity` 不是 `Send`）。`app` 的 `startup::release` 在骨架窗口的下一帧回调里运行，那时 GPUI 正借着这个窗口，`install` 里的 `replace_root` 拿不到第二次借用；所以 `install`、`attach` 先试一次，拿不到就经 `App::defer` 推到这次更新结束时再做（在下一次绘制之前），第 3 步按设计原样在 `release` 里调用即可（`install_inside_the_windows_update_mounts_after_it`）。
- **`workspace.content.missing` 节点**：基底屏幕没有注册时内容区的那行次要文字（`workspace-screen-missing`）也有一个 `Text` 节点，`Label::key` 是消息键；「UI 探针的语义节点」表没有列它，探针按 `workspace.notice` 的方式读。
- **队列的传输**：`futures::channel::mpsc::unbounded`，接收端是 `install` 里 `cx.spawn` 的前台任务，设计原文如此；`futures` 加进 `[workspace.dependencies]`（`default-features = false, features = ["std"]`）。`#[gpui::test]` 里 kernel 线程唤醒前台任务，测试调度器只在 `allow_parking` 后接受别的线程的唤醒，所以 `UiRig` 先调 `cx.executor().allow_parking()`。
- **每帧的 `Tick`**：有推迟的项时根视图用 `window.on_next_frame` 排一个 `Tick`（`request_animation_frame` 只能在绘制阶段调用，不用它；`on_next_frame` 本身就让 GPUI 安排下一帧）。
- **工具栏的返回按钮和检查器开关**：`ui` 的 `Button`、`Icon` 资源在 `ui` 第 3 步，现在是两个带 `workspace-back` / `workspace-inspector-toggle` 文字的可点击元素；第 3 步后换成 `Button` 加 `Icon::Back` / `Icon::Inspector`。检查器的分段控件同理，现在是一排可点击的页名。
- **`WorkspacePlugin::new(host: WorkspaceHost)`**：按设计；第 1 步的 `new(Arc<Hub>)` 改为 `cfg(test)` 的 `from_hub`，只给不依赖 GPUI 的测试用。

### 第 3 步

- **`on_state_changed` 经通道到前台任务**：回调是 `Fn(WindowState) + Send + Sync`，没有 `cx` 和窗口，而 `set_frame_autosave_name` 要 `&Window`。所以回调（`WindowStateLink::state_changed`）只往 `futures` 的通道发消息，`app` 在主线程上开的前台任务收到后换 frame autosave 名（`window.update`）、去抖和提交写文件的任务。名字的切换因此晚一个调度周期，不在 `Workspace` 的更新里；窗口已关闭时什么也不做。「窗口状态」写的「`app` 可以直接在这里切换」在这个意义上成立（主线程，同一次事件循环里）。
- **没有文件时不调 `host.restore`**：设计写「认不出或没有文件时为默认值」再调 `restore`。默认值（Welcome、检查器隐藏）恢复不出任何东西，却会在用户已经打开项目时把检查器重置；`version` 不认得、读不出、不是 JSON 的文件同样不调，记 warn（`window state ignored` / `window state unreadable`）后由下一次状态变化覆盖。
- **`install` 只在目录都齐时调用**：`home`、`app_cache()`、`app_data()` 任一个为 None 时 `app` 不装配任何插件，这时也不调用 `install`，窗口保持中性的骨架（没有 `workspace` 插件，装了也只有一个空的 `Workspace`）。`install` 在 `Cache`、`Watcher` 建好之后、构造插件之前，仍然在 `ready` 之后、`Kernel::start` 之前。窗口句柄由启动屏障的第二个帧回调传入（`startup::release_in`）。**过渡状态**：正常启动时 `install` 总会运行，而 `welcome` 还没有装配、`project_home` 还没有登记屏幕（见下一条），没有任何 `Screen` 注册，所以在它们的视图合并之前，窗口显示空的 `Workspace`（工具栏加内容区的 `workspace-screen-missing` 文字），恢复出来的项目也停在这个状态；这是这一步有意接受的，随那两个视图的装配消失。
- **Dock 重开按当前状态取 frame autosave 名**：重开的窗口一开始就带 `"project"`（有项目）或 `"welcome"`，不是先 `"welcome"` 再换，避免窗口先以 Welcome 的大小出现再跳。
- **`Quit` 先设退出标记再问守卫**：询问期间再按 ⌘Q 被忽略，守卫拒绝时去掉标记。`done` 被丢弃（没有回答）按通过处理，不让应用退不出去；设计只写了 `done(false)` 时什么也不做。
- **「项目」窗口第一次使用时的 1180 × 760 pt 居中没有做**：GPUI 的 `Window` 没有移动接口，`resize` 只改内容尺寸并保持左上角，Welcome 居中的窗口放大到 1180 × 760 在常见屏幕上会越出右边和下边；居中要调 AppKit，按架构规则 6 属于 `platform`。设计把这件事放在 `app`，但 `app` 做不到。现在项目窗口沿用 Welcome 的大小，用户调整一次，AppKit 就把它保存在 `"project"` 名下。后续：`platform` 设计加一个「设内容尺寸并居中」的接口，`app` 在第一次改名 `"project"` 之前、`bounds()` 在设名前后不变时调用（设名会同步套用保存的 frame，没有保存过时窗口不变）。
- **里程碑与帧记录转交没有接**：`bench-probe` feature、`lectern-bench` 的探针（`Milestone`、`FrameStats`、`Driver`）是 bench 第 5 步，现在不存在，所以 `on_milestone`、`on_frames` 是空函数（`startup::assemble`），前置条件 F 随探针做。`platform::frame` 也没有，`FrameRecord::frames` 照旧总是空。
- **`welcome`、`project_home` 不装配**：两个设计都规定装配在它们的视图之后，`welcome` 现在登记的 `Screen` 载荷是占位类型，`UiPayloads` 会拒绝它（窗口没有 Welcome 屏幕）；`project-home-usage` 要 `dyn Usage`，当时没有插件提供，图校验失败；`usage` 与 `project_home` 随后装配（[#247](https://github.com/SoloJiang/lectern-app/pull/247)，[app 设计「usage 与 project_home 的装配」](app.md#usage-与-project_home-的装配)），`welcome` 在它的视图合并后装配（[app 设计「welcome 的装配」](app.md#welcome-的装配)，Welcome 的部分已可验收）。所以「首次启动显示 Welcome（820 × 520）」在 `welcome` 装配之后可以验收，「打开文件夹后重启回到项目主页」要等 `project_home` 的视图合并后。
- **500 ms 去抖不在退出时落盘**：最后一次变化之后 500 ms 内退出，这次变化不写出（`app` 设计「已知限制」）。设计的「`Quit` 的第 1 步」没有要求落盘；要做就得在 `lectern-quit` 线程里同步写，另议。
