//! 终端服务：打开、列出、关闭终端，发输入，取视口的帧（docs/design/terminal.md「接口」）。
//!
//! `terminal` 插件实现 [`Terminals`]；`conversation` 的 Terminal 页、`terminal_view`
//! 和 #22 的审阅意见发回只依赖本模块（架构规则 7）。这里只有数据，没有行为，
//! `AgentCommand::program` 与 `FrameInterest` 的回调除外。

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_kernel::{ActivationToken, Event, Service};

use super::project::ProjectKey;
use super::session::{SessionKey, Timestamp};

mod frame;
mod input;

#[cfg(test)]
mod tests;

pub use frame::{
    CellStyle, Cluster, Cursor, CursorShape, Frame, FrameColors, GridRow, Modes, ScrollState,
    SelectionSpan, StyleFlags, StyleRun, Underline,
};
pub use input::{
    ColorScheme, GridSize, Input, Key, KeyAction, KeyInput, Mods, MouseButton, MouseInput,
    MouseKind, Rgb, ScrollInput, ScrollTarget, Theme,
};

/// 单实现服务，"terminal" 插件提供。不暴露给进程插件（暴露要另写 ADR）。
pub trait Terminals: Send + Sync {
    /// 打开一个终端。任何线程可调用；不阻塞、不做 IO，只读内存里的会话列表和项目快照
    /// （校验见设计「安全边界」）。返回之前终端已登记（`for_session`、`info` 查得到，状态
    /// `Starting`）；登录环境、PTY 和子进程在终端自己的线程上准备，结果随
    /// `TerminalEvent::Listed` 送达（`RunState::Running` 或 `Failed`）。
    /// 恢复时：这个会话已有 `Starting` / `Running` 的终端时返回 `Err(AlreadyRunning(id))`，
    /// 不启动第二个进程；已有的终端是 `Exited` / `Failed` 时先关闭它（`reason = replaced`），
    /// 再开新的。所以每个会话最多一个终端。
    fn open(&self, spec: OpenSpec) -> Result<TerminalId, LaunchError>;

    /// 全部终端，按打开的先后。取锁复制一个 `Arc`，不阻塞，UI 线程可调用。
    fn list(&self) -> Arc<TerminalList>;

    /// 一个终端的当前信息（含 `RunState`）。不阻塞；已关闭的 ID 返回 None。
    fn info(&self, id: TerminalId) -> Option<TerminalInfo>;

    /// 这个会话还没有关闭的终端，任何状态都算（已退出的终端保留最后的画面）。
    /// 它是否在驱动会话由调用方看 `RunState`：只有 `Starting` / `Running` 算。不阻塞。
    fn for_session(&self, session: &SessionKey) -> Option<TerminalId>;

    /// 最新的一帧。不阻塞，复制一个 `Arc`。没有关注者期间不更新（见 `watch`）；
    /// 已关闭的 ID 返回 None。
    fn frame(&self, id: TerminalId) -> Option<Arc<Frame>>;

    /// 关注这个终端的画面：至少有一个关注时，终端线程生成帧并发出 `TerminalEvent::Frame`；
    /// 关注全部 drop 后只解析输出、不生成帧。新关注出现时立即生成一帧完整的。
    /// 立即返回，任何线程；已关闭的 ID 返回 None。
    fn watch(&self, id: TerminalId) -> Option<FrameInterest>;

    /// 发一个输入。立即返回，任何线程可调用，不阻塞（放进队列，向终端线程的唤醒 socket
    /// 写一个字节）。同一终端的输入按调用顺序处理；已关闭的 ID 忽略。
    /// 进程不在运行（`Starting`、`Exited`、`Failed`）时，发给程序的输入（`Key`、`Text`、
    /// `Paste`、`Focus`，以及会编码给程序的鼠标和滚轮）丢弃；作用于视口和选区的输入
    /// （`Scroll`、`ScrollTo`、选区用的 `Mouse`、`SelectAll`、`ClearSelection`、
    /// `ClearScrollback`、`Resize`、`SetTheme`）照常处理到 `close`，所以退出后仍能滚动、
    /// 选择、复制。`Starting` 期间发给程序的输入不排队、直接丢弃：要送给程序的内容（如 #22
    /// 发回的审阅意见）由调用方等到 `RunState::Running`（随 `Listed` 送达）再发。
    fn input(&self, id: TerminalId, input: Input);

    /// 取当前选区的纯文本（复制用）。立即返回。终端线程在 `token.enter()` 取得的准入内
    /// 调用一次 `reply`；取不到准入（调用方已停用）时丢弃 `reply`，不调用。
    /// 没有选区、终端已关闭时 `reply` 收到 None。`reply` 只把结果转到主线程，不做别的工作。
    fn copy_selection(
        &self,
        id: TerminalId,
        token: ActivationToken,
        reply: Box<dyn FnOnce(Option<String>) + Send>,
    );

    /// 关闭：立即从列表里去掉，进程的结束在终端线程上进行（设计「数据流与线程」的「关闭」）。
    /// 立即返回，任何线程；已关闭的 ID 忽略。
    fn close(&self, id: TerminalId);
}

impl Service for dyn Terminals {
    const ID: &'static str = "terminal";
    const MULTIPLE: bool = false;
}

/// 进程内从 1 递增，不复用。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct TerminalId(pub u64);

pub struct OpenSpec {
    pub launch: Launch,
    /// 初始网格。None 时用 80×24、单元 8×16 像素；视图第一次布局后发 `Input::Resize`。
    pub size: Option<GridSize>,
    /// 第一帧之前就要有颜色，所以随打开一起给出；之后用 `Input::SetTheme` 更新。
    pub theme: Theme,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Launch {
    /// 恢复会话：程序和参数按 `ResumeCommand` 映射，工作目录取会话列表里的 `cwd`。
    Resume(SessionKey),
    /// 在 `cwd` 里开始新的 agent 会话：`claude` 或 `codex`，不带参数。
    NewSession { agent: AgentCommand, cwd: Arc<Path> },
    /// 用户的登录 shell。
    Shell { cwd: Arc<Path> },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum AgentCommand {
    ClaudeCode,
    Codex,
}

impl AgentCommand {
    /// 程序名（"claude"、"codex"），界面显示用。
    pub fn program(self) -> &'static str {
        match self {
            AgentCommand::ClaudeCode => "claude",
            AgentCommand::Codex => "codex",
        }
    }
}

/// `open` 在调用线程上能判断的错误。调用方按种类显示一行提示，不重试。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum LaunchError {
    /// 这个会话已有启动中或运行中的终端，附它的 ID；调用方切到它。
    AlreadyRunning(TerminalId),
    /// 会话不在来源的列表里，或这个来源当前没有提供者。
    UnknownSession,
    /// 来源不是内置的（进程插件的代理）：不接受它的恢复命令。
    UntrustedSource,
    /// 来源对这个会话不给恢复命令（子 agent 会话、认不出格式的会话）。
    NotResumable,
    /// 会话 ID 不是 UUID 的规范写法，或与恢复命令对不上。
    InvalidSessionId,
    /// 工作目录不在已打开项目的 worktree 根或文件夹项目根之内。
    OutsideProject,
    /// 已有 8 个终端（任何状态）。
    TooMany,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TerminalList {
    /// 每次替换加一，与 `TerminalEvent::Listed::generation` 一致。
    pub generation: u64,
    pub terminals: Arc<[TerminalInfo]>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TerminalInfo {
    pub id: TerminalId,
    pub launch: LaunchKind,
    pub project: ProjectKey,
    /// 子进程的工作目录：恢复时是日志里的 cwd 原样，其余是调用方给出的路径。
    pub cwd: Arc<Path>,
    pub state: RunState,
    /// 程序用 OSC 0 / 2 设置的标题。是程序输出，只用于显示，不进日志。
    pub title: Option<Arc<str>>,
    pub opened_at: Timestamp,
    /// 响铃次数。
    pub bells: u32,
    /// 因输入队列已满（4 MiB）而丢弃的输入次数（设计「状态与异常」）。
    pub dropped_inputs: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LaunchKind {
    Resume(SessionKey),
    NewSession(AgentCommand),
    Shell,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RunState {
    /// 终端线程还在校验目录、准备环境、启动进程。
    Starting,
    /// `pid` 是子进程的进程号，也是它的进程组号（`setsid`）。
    Running { pid: u32 },
    /// 进程已退出；最后的画面和滚动缓冲保留到 `close`，可以滚动、选择、复制。
    /// `signaled` 为 true 时 `code` 无意义。
    Exited { code: u32, signaled: bool },
    /// 没有启动起来，或终端线程出了内部错误。画面保留到 `close`。
    Failed(FailReason),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FailReason {
    /// 工作目录已不存在或不是目录。
    CwdMissing,
    /// 规范化之后的工作目录不在同一个已打开的项目里（符号链接指向项目之外，或项目已关闭）。
    OutsideProject,
    /// 登录环境的 `PATH` 里没有这个 agent 的程序；界面用 `AgentCommand::program` 提示安装。
    NotInstalled(AgentCommand),
    /// 找不到可执行的 shell（只在 `Launch::Shell`）。
    NoShell,
    /// 打开 PTY 或启动进程失败，附系统错误码。
    Spawn { errno: Option<i32> },
    /// 终端线程 panic（设计「数据流与线程」）。只影响这一个终端。
    Internal,
}

/// `watch` 的关注，drop 时注销（任何线程，不阻塞）。`terminal` 用 `new` 构造，传入的回调
/// 持有终端表的 `Weak`；实现方已停用时回调不做任何事。回调最多调用一次，不得 panic。
/// 不序列化。
pub struct FrameInterest {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl FrameInterest {
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        FrameInterest {
            release: Some(Box::new(release)),
        }
    }
}

impl Drop for FrameInterest {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for FrameInterest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameInterest").finish_non_exhaustive()
    }
}

/// 只在终端线程上发出，不在调用 `open` / `close` 的线程上同步分发（kernel 的 `emit` 在调用
/// 线程上同步调用监听，调用方可能正在 GPUI 的更新里）。`open` / `close` 改的表在返回之前
/// 已经生效，事件稍后由那个终端的线程发出。监听只更新状态或安排重绘，不做 IO。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TerminalEvent {
    /// `list()` 换了新值：打开、关闭、状态变化、标题变化（同一终端的标题变化最多每 250 ms
    /// 一次）。
    Listed { generation: u64 },
    /// 有关注者的终端有了新的一帧。同一终端最多每 8 ms 一次（设计「帧的节奏」）。
    Frame { id: TerminalId, generation: u64 },
    /// 程序发出响铃（BEL）。同一终端最多每 500 ms 一次。
    Bell { id: TerminalId },
}

impl Event for TerminalEvent {}
