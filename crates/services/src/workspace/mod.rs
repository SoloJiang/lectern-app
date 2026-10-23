//! 窗口骨架的服务接口：界面贡献的登记、导航与布局命令、离开守卫与窗口事件
//! （docs/design/workspace.md「接口」「服务接口」）。
//!
//! `workspace` 插件实现 [`Contributions`]；`welcome`、`project_home`、`review_view`、
//! `file_reader` 等贡献方只依赖本模块（架构规则 7）。本 crate 在 kernel 层，所以这里
//! 只有稳定 ID、顺序和布局这类与界面框架无关的数据；视图工厂、Action 和 Fluent 键放在
//! 类型擦除的 [`Payload`] 里，具体类型由 `ui::contrib` 定义，`workspace` 用
//! `downcast` 取回。这里没有行为，[`Registration`]、[`Payload`] 和 [`LeaveGuard`] 的
//! 回调除外。

use std::any::Any;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_kernel::{ActivationToken, Event, Name, Service, Staged};

use super::project::{ProjectKey, WorktreeKey};
use super::reader::ReadTarget;
use super::review::ReviewTarget;

#[cfg(test)]
mod tests;

/// 单实现服务，`workspace` 插件提供。
pub trait Contributions: Send + Sync {
    /// 登记一项贡献。立即返回，任何线程可调用。`activate` 里调用时把返回的守卫交给
    /// `ctx.own_staged`，提交时才生效；`started` 之后调用时按 kernel 设计「撤销」排到
    /// 主线程应用。同一个 `(kind, id)` 已有登记时后来的被拒绝（记 warn），守卫 drop 时
    /// 什么也不做。守卫 drop 时注销，注销排到主线程，带所属激活的 `token.pending()`
    /// 守卫。
    fn register(&self, contribution: Contribution) -> Registration;

    /// 登记一个由 `Command` 激活的命令的处理函数（kernel 设计「按命令激活」第 1 步）。
    /// 规则同 `register`。
    fn handle_command(&self, name: Name, handler: CommandHandler) -> Registration;

    /// 登记离开守卫（file_reader 设计「编辑」）。按登记顺序询问。规则同 `register`。
    fn register_leave_guard(&self, guard: Arc<dyn LeaveGuard>) -> Registration;

    /// 向窗口发一个导航或布局命令。立即返回，任何线程可调用；排到主线程执行，需要
    /// 离开守卫的命令先问守卫（workspace 设计「数据结构」的状态机）。
    fn dispatch(&self, command: WorkspaceCommand);

    /// 当前的侧栏条目（按 `section`、`order`、`id` 排序），供 `project_home` 渲染项目
    /// 侧栏。取锁复制一个 `Arc`，不阻塞；条目增减时换一份新的，
    /// `WorkspaceEvent::ContributionsChanged` 通知。
    fn sidebar_entries(&self) -> Arc<[SidebarEntry]>;

    /// 报告一个里程碑状态已经画出（bench 设计「需要其他 crate 提供的接口」）：
    /// `workspace` 在下一帧回调里记下时刻并转给 `app`。任何线程。
    fn report_milestone(&self, milestone: Milestone);
}

impl Service for dyn Contributions {
    const ID: &'static str = "contributions";
    const MULTIPLE: bool = false;
}

/// 贡献：`kind` 决定 `payload` 必须是 `ui::contrib` 的哪个类型。
pub struct Contribution {
    pub kind: ContributionKind,
    /// 稳定 ID，`[a-z0-9_-]`，1 到 64 字节：探针节点、日志和窗口状态都用它。
    pub id: Name,
    /// 同类贡献的显示顺序，小的在前，相同时按 `id`。
    pub order: u16,
    pub payload: Payload,
}

impl fmt::Debug for Contribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Contribution")
            .field("kind", &self.kind)
            .field("id", &self.id)
            .field("order", &self.order)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ContributionKind {
    /// 内容区的一个屏幕。`payload` 是 `ui::contrib::Screen`；`id` 用 [`screens`] 里的
    /// 名字。
    Screen(ScreenLayout),
    /// 替换侧栏内容的面板（项目侧栏本身、Files）。`payload` 是 `ui::contrib::SidebarPanel`。
    SidebarPanel,
    /// 项目侧栏里的一个条目（Overview、Sessions、Worktrees、Files、Usage……）。
    /// `payload` 是 `ui::contrib::SidebarItem`。
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

/// 屏幕对窗口布局的要求，由贡献方声明，状态机据此决定各区域的显示。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Visibility {
    Shown,
    Hidden,
}

/// 项目侧栏的分区，按从上到下的顺序（`Ord`）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum SidebarSection {
    Project,
    Views,
    Bottom,
}

/// 一条已登记的侧栏条目（`Contributions::sidebar_entries`）。`payload` 是登记时的
/// `ui::contrib::SidebarItem`，`workspace` 应用登记时从 [`Payload`] 里取出、换成
/// 共享的 `Arc`，`project_home` 用 `Arc::downcast` 取回。
#[derive(Clone, Debug)]
pub struct SidebarEntry {
    pub id: Name,
    pub section: SidebarSection,
    pub order: u16,
    pub payload: SharedPayload,
}

/// 类型擦除的载荷。`ui::contrib` 的类型实现 `into_payload`，`workspace` 用 `downcast`
/// 取回；类型不对时拒绝登记（warn）。进程插件不能跨进程传它。
pub struct Payload(pub Box<dyn Any + Send>);

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Payload").finish_non_exhaustive()
    }
}

/// 已应用的、可以被多个视图共享的载荷（[`SidebarEntry::payload`]）。
#[derive(Clone)]
pub struct SharedPayload(pub Arc<dyn Any + Send + Sync>);

impl fmt::Debug for SharedPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedPayload").finish_non_exhaustive()
    }
}

/// 登记的守卫。drop 时注销；实现 [`Staged`]，`activate` 里经 `ctx.own_staged` 交出。
/// `workspace` 用 `new` 构造：`commit` 在提交时收到这次激活的 token，把登记排到主线程
/// 应用；`release` 在 drop 时把注销排到主线程。两个回调各最多调用一次，不得 panic；
/// 没有提交就 drop（激活失败）时只调用 `release`。不序列化。
pub struct Registration {
    commit: Option<Box<dyn FnOnce(ActivationToken) + Send>>,
    release: Option<Box<dyn FnOnce() + Send>>,
}

impl Registration {
    pub fn new(
        commit: impl FnOnce(ActivationToken) + Send + 'static,
        release: impl FnOnce() + Send + 'static,
    ) -> Self {
        Registration {
            commit: Some(Box::new(commit)),
            release: Some(Box::new(release)),
        }
    }
}

impl Staged for Registration {
    fn commit(&mut self, token: ActivationToken) {
        if let Some(commit) = self.commit.take() {
            commit(token);
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for Registration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registration")
            .field("committed", &self.commit.is_none())
            .finish_non_exhaustive()
    }
}

/// 命令处理函数：主线程调用，运行在登记方激活的准入和日志作用域里。
pub type CommandHandler = Box<dyn Fn(&CommandCall) + Send + Sync>;

/// 处理函数收到的调用。`window` 是发起命令的窗口（第一版只有一个）。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct CommandCall {
    pub window: WindowId,
    pub name: Name,
}

/// 离开守卫（file_reader 设计「编辑」的接口）。`ask` 在主线程上调用，不阻塞，结果经
/// `done` 交回。不实现 `Service`：经 `Contributions::register_leave_guard` 登记。
pub trait LeaveGuard: Send + Sync {
    fn ask(&self, scope: LeaveScope, done: Box<dyn FnOnce(bool) + Send>);
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum LeaveScope {
    View,
    Project(ProjectKey),
    Worktree(ProjectKey),
    Window,
    App,
}

/// 窗口的标识。第一版总是 `WindowId(0)`。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
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
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Target {
    None,
    Project(ProjectKey),
    Review(ReviewTarget),
    Read(ReadTarget),
}

/// 一次导航的目标。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Route {
    pub screen: Name,
    pub target: Target,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum WorkspaceCommand {
    /// 打开一个路径：先问 `LeaveScope::Project` 的守卫（已有项目时），通过后调
    /// `Projects::open`。
    OpenProject(PathBuf),
    /// 用系统的打开面板选文件夹（⌘O），然后同 `OpenProject`。
    PromptOpenProject,
    /// 关闭当前项目：问 `LeaveScope::Project`，通过后调 `Projects::close`。
    CloseProject,
    /// 切换当前 worktree：问 `LeaveScope::Worktree`，通过后调
    /// `Projects::set_active_worktree`。
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
    /// 当前屏幕的 `layout.inspector` 为 false（Welcome、项目主页）时两者都忽略，记 debug。
    SetInspector(bool),
    SelectInspectorTab(Name),
    /// 打开或关闭浮动面板（Space 打开上下文面板，Esc 关闭）。
    OpenPanel(Name),
    ClosePanel,
    /// 打开或关闭覆盖层（⌘K）。
    OpenOverlay(Name),
    CloseOverlay,
    /// Esc：按 workspace 设计「数据结构」的顺序关闭最上面的一层。
    Escape,
    /// 关闭窗口：问 `LeaveScope::Window`，通过后关闭。
    CloseWindow,
}

/// 由 `workspace` 插件在主线程上发出（`PluginHandle::emitter()`）。监听只更新状态或
/// 安排重绘。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "ScreenChanged carries two routes by value (about 400 bytes): one event per \
              navigation, cloned once per listener; boxing would put an indirection in \
              every listener for nothing"
)]
pub enum WorkspaceEvent {
    /// 当前屏幕变了（压入、弹出、项目打开或关闭）。`from` 在第一次显示时为 None。
    ScreenChanged {
        window: WindowId,
        from: Option<Route>,
        to: Route,
    },
    /// 一个按需层打开或关闭（检查器、面板、覆盖层、侧栏面板）。
    LayerChanged {
        window: WindowId,
        layer: Layer,
        open: bool,
    },
    /// 侧栏条目、检查器页等贡献增减。
    ContributionsChanged,
    /// 窗口已关闭。
    WindowClosed { window: WindowId },
}

impl Event for WorkspaceEvent {}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Layer {
    SidebarPanel(Name),
    Inspector,
    Panel(Name),
    Overlay(Name),
}

/// bench 的里程碑（bench 设计 `Milestone` 的同名变体；`app` 做映射，`workspace` 不依赖
/// tool 层）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Milestone {
    ProjectHome,
    FileHighlighted,
    DiffShown,
    WorktreeSwitched,
}
