# terminal 设计

状态：已接受，尚未实现。

对应 issue [#12](https://github.com/SoloJiang/lectern-app/issues/12)（终端：portable-pty + libghostty-vt，会话 resume）与 [#5](https://github.com/SoloJiang/lectern-app/issues/5) 的内核部分（libghostty-vt 接入）。`terminal` 启动 PTY 和其中的程序（`claude --resume`、`codex resume`、新会话、登录 shell），把输出交给 libghostty-vt 解析，生成视口的网格快照交给视图；视图发来的按键、粘贴、鼠标和尺寸变化经 libghostty-vt 编码后写回 PTY（[技术调研「终端：接入 libghostty-vt 内核」](tech-research.md#终端接入-libghostty-vt-内核)、[ADR 0002](../adr/0002.md)）。终端在界面的「收起」一层，是 Conversation 的 Terminal 页（[交互规范「三层结构」](interaction.md#三层结构)、[ADR 0012](../adr/0012.md)）；网格的绘制在 `terminal_view`，页面在 `conversation`。

## 范围

- 做：
  - 插件 "terminal"：提供单实现服务 `dyn Terminals`（接口和数据类型放在 `services::terminal`，本文定义）。
  - 启动：恢复会话（`Launch::Resume`）、在某个目录里开始新的 agent 会话（`Launch::NewSession`）、登录 shell（`Launch::Shell`）。恢复命令按 [agents 设计](agents.md)「服务接口」里 `ResumeCommand` 的安全规则执行（见「安全边界」）。
  - PTY：经 portable-pty 打开、设置尺寸、读写；子进程的等待、挂断与强制结束。
  - 终端状态：libghostty-vt 的 `Terminal`，滚动缓冲上限 10,000 行；按键、粘贴、鼠标、焦点按程序开启的模式编码（`key::Encoder`、`mouse::Encoder`、`paste`、`focus`）；选区与复制的纯文本。
  - 帧：视口的行、样式段、光标、选区、滚动位置、模式与运行状态，作为不可变快照（`Arc<Frame>`）交给视图；内容没变的行在相邻两帧之间共用。
  - 颜色：视图给出默认前景、背景、光标色和 16 个 ANSI 色（`Theme`），本 crate 据此解析每个格子的颜色，并应答程序的颜色查询。
  - 生命周期：项目关闭时关闭它的终端；插件停用时关闭全部终端。
- 不做：
  - 网格的绘制、字形缓存、IME、快捷键、右键菜单：`terminal_view`。
  - Terminal 页、Resume 按钮、「在别处运行」的提示、退出前的确认：`conversation`。
  - 剪贴板的读写：视图经 GPUI 读写，本 crate 只给出选区的文字、只接收要粘贴的文字。
  - 会话发现与恢复命令的来源：`agents`（`SessionSource::resume`）。
  - 项目空状态里的 New session 入口：`project_home`（[project_home 设计](project_home.md)「范围」），它调用本 crate 的 `Launch::NewSession`。
- 本 issue 之外：
  - 超链接（OSC 8）与 URL 识别、滚动缓冲里的搜索：libghostty-vt 0.2.2 没有搜索接口，另开 issue。
  - kitty 图形协议：本 crate 关掉 `kitty-graphics` feature，图片不显示。
  - 「在外部 Ghostty 打开」（技术调研「2. 终端」第 5 条）：另开 issue。
  - 程序经 OSC 52 写剪贴板：P1 不支持（「安全边界」、「未决问题」2）。
  - 多窗口：P0 只有一个窗口，终端属于整个应用；多窗口时按项目区分，届时修订本节。

## 依赖

本 crate 是插件 crate（[ADR 0025](../adr/0025.md)）：实现 `Plugin`，`[package.metadata.lectern]` 加 `plugin = true`，层仍是 `domain`。它只依赖 kernel 层和非插件的库 crate；`conversation`、`terminal_view` 只经 `services::terminal` 用它。

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-kernel` | `Plugin`、`PluginHandle::own_with`、`BusyGuard`、事件 | 插件机制 |
| `lectern-services` | `Terminals` 接口与数据类型（本文定义）；调用 `Projects::locate`、`SessionSource::sessions` / `resume` | 消费方只依赖接口（架构规则 7） |
| `lectern-executor` | 每个终端的两个专属线程（`run_dedicated_gated`） | 阻塞的 `poll` 与 `wait` 不占工作线程 |
| `lectern-logging` | 运行日志（「日志与诊断」）；测试用 `test-support` 捕获记录 | [ADR 0046](../adr/0046.md) |
| `libghostty-vt = "=0.2.2"`，`default-features = false` | 终端状态机（ADR 0002） | 见下文「libghostty-vt」 |
| `portable-pty = "=0.9.0"` | 打开 PTY、启动子进程、设置尺寸、挂断 | 技术调研选定；MIT |
| `rustix`（工作区已有，加 `event`、`process` feature） | 主端 fd 的非阻塞读写与 `poll`；向进程组发 `SIGKILL` | 已在工作区，不引入新 crate |
| `libc`（工作区已有） | `getpwuid_r` 取登录 shell | 只在 `$SHELL` 不可用时 |

### libghostty-vt

- 版本锁 `=0.2.2`（crates.io，MIT OR Apache-2.0，`rust-version` 1.90，工具链 1.97 满足）。它依赖 `libghostty-vt-sys =0.2.2`、`bitflags`、`int-enum`。`libghostty-vt-sys` 的构建脚本固定 Ghostty 的提交 `a887df42c56f6de86c0fe6da9c4eeca37931e083`（1.3.2-dev），就是 ADR 0002 说的「libghostty-rs 的 vendored 构建」：首次构建时把这个提交克隆进 `OUT_DIR`，用 Zig 编出 `libghostty-vt.a` 并静态链接。升级 libghostty-vt 就是换这个提交，按 AGENTS.md 先确认。
- 关掉默认 feature：不要 `kitty-graphics`（图片不在 P1）、`log` 与 `tracing`（libghostty 的日志不经 `lectern-logging` 的宏，ADR 0046），也不调用 `set_logger`，libghostty 的日志被丢弃。不开 `link-dynamic`，安装包里不多一个 dylib。
- 构建链需要 Zig 0.15.2（Ghostty 的 `build.zig.zon` 要求的最低版本；Zig 的主版本之间不兼容，固定这一个版本）和 `git`。首次构建要访问 `github.com`（克隆 Ghostty）和 `deps.files.ghostty.org`（Zig 依赖，`build.zig.zon` 按哈希固定），之后按 `OUT_DIR` 复用。`Cargo.lock` 与 `--locked` 不覆盖构建脚本取的源码；源码由上面的提交号和哈希固定，所以构建结果仍然确定。需要离线或共用一份源码时，可设 `GHOSTTY_SOURCE_DIR` 指向同一提交的检出、`GHOSTTY_ZIG_SYSTEM_DIR` 指向预取的 Zig 依赖（libghostty-vt-sys 支持的两个变量），只影响构建速度，不改结果。
- `cargo-deny` 只检查 Rust crate。Ghostty 编进静态库的 C / C++ 依赖（如 simdutf、highway）不在它的范围内：实现 PR 从 `build.zig.zon` 列出它们和许可证，写进本文「实际验证」，确认都在 `deny.toml` 的许可证白名单里。
- 本 crate 不依赖 GPUI，Linux 上的 `--gpui-free-packages` 会包含它，所以 Linux 与 macOS 的 CI 任务、每台开发机都要装 Zig 0.15.2（「对其他 crate 的要求」第 2 项）。
- libghostty-vt 的类型都是 `!Send + !Sync`，只在终端自己的线程上创建和使用（「数据流与线程」）。

### portable-pty

- 版本锁 `=0.9.0`。新引入的传递依赖：`nix` 0.28、`filedescriptor` 0.8、`serial2`、`shell-words`、`downcast-rs`、`log`（`anyhow`、`libc` 已在 `Cargo.lock`）。都是宽松许可，`just deny` 在实现 PR 里检查。
- 只用 `native_pty_system().openpty`、`SlavePty::spawn_command`、`MasterPty::{resize, as_raw_fd, process_group_leader}`、`Child::{wait, process_id, clone_killer}`。不用 `try_clone_reader` / `take_writer`：它们给的是阻塞的 fd 副本，读线程在 `read` 里无法被打断，终端关闭时线程会留下。本 crate 把主端 fd 设为非阻塞，在自己的线程上 `poll`。
- 子进程由 `std::process::Command` 加 `pre_exec` 启动（重置信号、`setsid`、`TIOCSCTTY`，关闭多余的 fd）。fork 之后它用 `read_dir` 枚举 fd，会分配内存；macOS 的 libmalloc 有 fork 处理，实践中可用，记为风险，由「测试」里的并发启动测试覆盖。

## 接口

### 插件（`lectern_terminal`）

```rust
/// app 装配用。
pub struct TerminalDeps {
    pub executor: Executor,
}

pub struct TerminalPlugin { /* deps，shell 与环境的覆盖 */ }

impl TerminalPlugin {
    pub fn new(deps: TerminalDeps) -> TerminalPlugin;

    /// 测试与基准：固定用这个程序作为 shell，按 `kind` 拼参数，不读 `$SHELL` 和用户数据库。
    pub fn with_shell(self, program: impl Into<PathBuf>, kind: ShellKind) -> TerminalPlugin;

    /// 测试：子进程环境的来源。默认是启动时的 `std::env::vars_os()`。
    pub fn with_env(self, env: Vec<(OsString, OsString)>) -> TerminalPlugin;
}

impl Plugin for TerminalPlugin {
    /// provides: [dyn Terminals]
    /// requires: [dyn Projects, dyn SessionSource]
    /// activation: [AfterFirstFrame]
    fn manifest(&self) -> Manifest;

    /// 只建空的终端表、取服务引用：不建线程、不做 IO、不打开 PTY（< 1 ms）。
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError>;
}

/// 支持的 shell。决定恢复和新会话的参数写法（「数据结构」的启动命令表）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellKind { Zsh, Bash, Fish, Ksh, Sh, Dash }
```

插件 ID 是 "terminal"。激活条件用 `AfterFirstFrame`，不用 kernel 示例里的 `Command("terminal.open")`：`conversation` 的终端插件在 `requires` 里依赖 `dyn Terminals`，而命令不会替依赖去满足激活条件（[kernel 设计「依赖与自动启停」](kernel.md#依赖与自动启停)），按命令激活的提供者会让依赖者一直等待。`activate` 只建内存里的空表，代价与按命令激活相同；线程和 PTY 都在第一次 `open` 时才有。

依赖 `dyn SessionSource` 的后果：两个内置会话来源都不可用时，本插件不激活，登录 shell 也打不开。这是已知限制（「已知限制」第 1 条）。

### 服务（`lectern_services::terminal`）

```rust
/// 单实现服务，"terminal" 插件提供。不暴露给进程插件（暴露要另写 ADR）。
pub trait Terminals: Send + Sync {
    /// 打开一个终端。任何线程可调用；不阻塞、不做 IO，只读内存里的会话列表和项目快照
    /// （校验见「安全边界」）。PTY 和子进程在终端自己的线程上启动，启动结果随
    /// `TerminalEvent::Listed` 送达（`RunState::Running` 或 `Failed`）。
    /// 同一会话已有 `Starting` / `Running` 的终端时返回它的 ID，不启动第二个进程；
    /// 已有的终端已退出时先关闭它，再开新的，所以每个会话最多一个终端。
    fn open(&self, spec: OpenSpec) -> Result<TerminalId, LaunchError>;

    /// 全部终端，按打开的先后。取锁复制一个 `Arc`，不阻塞，UI 线程可调用。
    fn list(&self) -> Arc<TerminalList>;

    /// 这个会话的终端（任何状态）。不阻塞。
    fn for_session(&self, session: &SessionKey) -> Option<TerminalId>;

    /// 最新的一帧。不阻塞，复制一个 `Arc`。没有关注者期间不更新（见 `watch`）；
    /// 已关闭的 ID 返回 None。
    fn frame(&self, id: TerminalId) -> Option<Arc<Frame>>;

    /// 关注这个终端的画面：至少有一个关注时，终端线程生成帧并发出 `TerminalEvent::Frame`；
    /// 关注全部 drop 后只解析输出、不生成帧。新关注出现时立即生成一帧完整的。
    /// 立即返回，任何线程；已关闭的 ID 返回 None。
    fn watch(&self, id: TerminalId) -> Option<FrameInterest>;

    /// 发一个输入。立即返回，任何线程可调用，不阻塞（放进队列，向终端线程的唤醒 socket
    /// 写一个字节）。同一终端的输入按调用顺序处理；已关闭或已退出的终端忽略。
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

    /// 关闭：立即从列表里去掉，进程的结束在终端线程上进行（「数据流与线程」的「关闭」）。
    /// 立即返回，任何线程；已关闭的 ID 忽略。
    fn close(&self, id: TerminalId);
}

impl Service for dyn Terminals {
    const ID: &'static str = "terminal";
    const MULTIPLE: bool = false;
}

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
pub enum AgentCommand { ClaudeCode, Codex }

/// `open` 在调用线程上能判断的错误。调用方按种类显示一行提示，不重试。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum LaunchError {
    /// 会话不在来源的列表里，或这个来源当前没有提供者。
    UnknownSession,
    /// 来源不是内置的（进程插件的代理）：不接受它的恢复命令。
    UntrustedSource,
    /// 来源对这个会话不给恢复命令（子 agent 会话、认不出格式的会话）。
    NotResumable,
    /// 会话 ID 不是 UUID 的规范写法。
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
    /// 因输入队列已满（4 MiB）而丢弃的输入次数（「状态与异常」）。
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
    /// 终端线程还在校验目录、启动进程。
    Starting,
    /// `pid` 是 shell 的进程号，也是它的进程组号（`setsid`）。
    Running { pid: u32 },
    /// 进程已退出；最后的画面和滚动缓冲保留到 `close`，可以滚动、选择、复制。
    /// `signaled` 为 true 时 `code` 无意义。shell 找不到程序时 `code` 是 127。
    Exited { code: u32, signaled: bool },
    Failed(StartError),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum StartError {
    /// 工作目录已不存在或不是目录。
    CwdMissing,
    /// 规范化之后的工作目录不在同一个已打开的项目里（符号链接指向项目之外，或项目已关闭）。
    OutsideProject,
    /// 找不到可执行的 shell（`$SHELL` 与用户数据库都不可用，`/bin/zsh` 也不存在）。
    NoShell,
    /// 打开 PTY 或启动进程失败，附系统错误码。
    Spawn { errno: Option<i32> },
}

/// `watch` 的关注，drop 时注销（任何线程，不阻塞）。写法同 `OverviewInterest`：
/// `terminal` 用 `new` 构造，回调持有终端表的 `Weak`，最多调用一次，不得 panic。
pub struct FrameInterest { /* release: Option<Box<dyn FnOnce() + Send + Sync>> */ }

impl FrameInterest {
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self;
}
```

### 事件

```rust
/// 只在终端线程上发出，不在调用 `open` / `close` 的线程上同步分发（kernel 的 `emit` 在调用线程上
/// 同步调用监听，调用方可能正在 GPUI 的更新里）。`open` / `close` 改的表在返回之前已经生效，
/// 事件稍后由那个终端的线程发出。监听只更新状态或安排重绘，不做 IO。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TerminalEvent {
    /// `list()` 换了新值：打开、关闭、状态变化、标题变化（同一终端的标题变化最多每 250 ms 一次）。
    Listed { generation: u64 },
    /// 有关注者的终端有了新的一帧。同一终端最多每 8 ms 一次（「帧的节奏」）。
    Frame { id: TerminalId, generation: u64 },
    /// 程序发出响铃（BEL）。同一终端最多每 500 ms 一次。
    Bell { id: TerminalId },
}

impl Event for TerminalEvent {}
```

### 输入

```rust
#[derive(Clone, PartialEq, Debug)]
pub enum Input {
    /// 一次按键。终端线程按程序当前的模式（DECCKM、kitty 键盘协议的标志）编码。
    Key(KeyInput),
    /// 输入法提交的文字、或不经按键编码的文字。原样以 UTF-8 写入。
    Text(Arc<str>),
    /// 粘贴。程序开了 bracketed paste 时包上 `ESC[200~` / `ESC[201~`，否则原样写入。
    /// 是否需要用户确认（多行、未开 bracketed paste）由视图按 `Frame::modes` 决定，
    /// 本 crate 收到就写。审阅意见发回会话（#22）用 `Paste` 加一次 `Key(Enter)`。
    Paste(Arc<str>),
    /// 鼠标。程序开了鼠标跟踪、且没按 Shift 时编码给程序；否则用于选区
    /// （`clicks` 为 2 选词、3 选行）。
    Mouse(MouseInput),
    /// 滚轮或触控板。开了鼠标跟踪时编码为滚轮事件；在备用屏幕且没开鼠标跟踪时
    /// 换成上下方向键（alternate scroll）；否则滚动视口。
    Scroll(ScrollInput),
    ScrollTo(ScrollTarget),
    /// 视口的格子数与单元的像素尺寸。连续的多个只处理最后一个；同时调整 PTY 的窗口大小
    /// （子进程收到 `SIGWINCH`），libghostty-vt 按新宽度重排滚动缓冲。
    Resize(GridSize),
    /// 视图获得或失去焦点。程序开了焦点事件（mode 1004）时写入 `ESC[I` / `ESC[O`。
    Focus(bool),
    SelectAll,
    ClearSelection,
    /// 清空滚动缓冲（不改视口的内容）。
    ClearScrollback,
    /// 外观变化时由视图发出：更新默认颜色，下一帧全部行重新解析颜色。
    SetTheme(Theme),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KeyInput {
    pub key: Key,
    pub mods: Mods,
    pub action: KeyAction,
    /// 这次按键产生的文字（GPUI 的 `key_char`），如 Shift+a 为 "A"。没有时为 None。
    pub text: Option<Arc<str>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    /// 不按 Shift 时这个键产生的字符，如 'a'、'1'、'/'。
    Char(char),
    Enter, Tab, Backspace, Escape, Space,
    Up, Down, Left, Right,
    Home, End, PageUp, PageDown, Insert, Delete,
    /// F1 到 F20。
    F(u8),
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mods { pub shift: bool, pub control: bool, pub option: bool, pub command: bool }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyAction { Press, Repeat, Release }

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MouseInput {
    pub kind: MouseKind,
    pub button: Option<MouseButton>,
    pub mods: Mods,
    /// 视口里的格子（列、行），从 0 开始。
    pub col: u16,
    pub row: u16,
    /// 视口里的像素位置（SGR 像素模式用）。
    pub x: f32,
    pub y: f32,
    pub clicks: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind { Down, Up, Drag, Move }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton { Left, Middle, Right }

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ScrollInput {
    /// 正数向下（看更新的内容）。视图把像素滚动量累计成整行再发。
    pub lines: i32,
    pub col: u16,
    pub row: u16,
    pub mods: Mods,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScrollTarget { Top, Bottom, PageUp, PageDown }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    /// 单元的设备像素尺寸，用于应答程序的尺寸查询（CSI 14 / 16 / 18 t）。
    pub cell_width: u16,
    pub cell_height: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// ANSI 0–15。16–255 由本 crate 按 xterm 的 6×6×6 色立方和灰阶算出。
    pub ansi: [Rgb; 16],
    /// 应答程序的明暗查询（mode 2031、`CSI ? 996 n`）。
    pub scheme: ColorScheme,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorScheme { Light, Dark }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Rgb { pub r: u8, pub g: u8, pub b: u8 }
```

`Theme` 的颜色由视图从 `ui` 的 `Palette`（系统语义色）推出，本 crate 不定义颜色值（ADR 0015）。程序自己指定的 256 色和 24 位色是程序输出的内容，原样显示，与代码配色不冲突。

### 帧

```rust
#[derive(Clone, PartialEq, Debug)]
pub struct Frame {
    /// 每生成一帧加一，与 `TerminalEvent::Frame::generation` 一致。
    pub generation: u64,
    pub size: GridSize,
    /// 视口的行，从上到下，恰好 `size.rows` 行。内容没变的行在相邻两帧里是同一个 `Arc`。
    pub rows: Arc<[Arc<GridRow>]>,
    /// None：光标隐藏，或视口滚到了历史里、光标不在视口内。
    pub cursor: Option<Cursor>,
    /// 选区在视口里的部分，按行升序。不放进行里，选区变化不改变行的 `key`。
    pub selection: Arc<[SelectionSpan]>,
    pub colors: FrameColors,
    pub scroll: ScrollState,
    pub modes: Modes,
    pub state: RunState,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GridRow {
    /// 文字、字素簇的列位置、样式段和 `wrapped` 的 64 位哈希。相同内容的行 `key` 相同，
    /// 视图按它缓存排版结果。
    pub key: u64,
    /// 每个字素簇一段：空格子写成空格，宽字符的占位格不写，行尾的空格子去掉。
    pub text: Box<str>,
    /// 每个字素簇在 `text` 里的起始字节和所在的列，按字节升序。
    pub clusters: Box<[Cluster]>,
    /// 样式段，按列升序、首尾相接，覆盖 0..cols（行尾空格子的底色也在内）。
    pub runs: Box<[StyleRun]>,
    /// 这一行是上一行软换行的延续（复制时不插入换行）。
    pub wrapped: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cluster { pub byte: u32, pub col: u16, pub wide: bool }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StyleRun { pub start: u16, pub end: u16, pub style: CellStyle }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CellStyle {
    /// 已解析的前景色：调色板序号、默认色、反显（SGR 7）都已换算。
    pub fg: Rgb,
    /// None：与 `FrameColors::background` 相同，视图不画底色。
    pub bg: Option<Rgb>,
    pub flags: StyleFlags,
    pub underline: Underline,
    /// None：与 `fg` 相同。
    pub underline_color: Option<Rgb>,
}

/// 位标志：BOLD、ITALIC、FAINT、STRIKETHROUGH、OVERLINE、INVISIBLE。闪烁不支持，按普通文字显示。
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub struct StyleFlags(pub u8);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Underline { None, Single, Double, Curly, Dotted, Dashed }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
    pub blinking: bool,
    /// 光标所在的格子是宽字符：视图画两格宽。
    pub wide: bool,
}

/// 失去焦点时的空心方块由视图画，本 crate 只给程序请求的形状（DECSCUSR）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape { Block, Bar, Underline }

/// `end` 不含。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SelectionSpan { pub row: u16, pub start: u16, pub end: u16 }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameColors { pub foreground: Rgb, pub background: Rgb, pub cursor: Rgb }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScrollState {
    /// 滚动缓冲与屏幕的总行数。
    pub total: u32,
    /// 视口顶行在全部行里的序号；`offset + size.rows == total` 时在底部。
    pub offset: u32,
}

/// 程序开启的、视图需要知道的模式。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Modes {
    pub bracketed_paste: bool,
    pub mouse_tracking: bool,
    pub alt_screen: bool,
    pub focus_events: bool,
}
```

## 数据结构

### 终端表

- 终端表在一把 `Mutex` 里：`TerminalId` 到条目（`TerminalInfo`、最新的 `Arc<Frame>`、关注数、命令队列的发送端、唤醒 socket 的写端）。`list()`、`frame()`、`input()` 都只持锁复制一个 `Arc` 或放进队列，不在锁内做别的工作。
- `TerminalId` 进程内从 1 递增，不复用。
- 终端最多 8 个，含已退出、启动失败的；`close` 之后不再计入。同一会话最多一个终端（`open` 的规则）。
- 列表的替换与 `Listed` 事件一一对应，`generation` 单调递增。

### 启动命令表

shell 的选择：`$SHELL`（必须是绝对路径、可执行）；不可用时取用户数据库（`getpwuid_r`）的登录 shell；按文件名对应 `ShellKind`；不在表里或都不可用时用 `/bin/zsh`，`/bin/zsh` 也不存在时 `StartError::NoShell`。`with_shell` 覆盖这一步。

恢复和新会话经登录 shell 启动，让用户在 `.zprofile`、`.zshrc` 里设置的 `PATH` 生效（从 Finder 启动的应用只有最小的 `PATH`，`claude`、`codex` 常装在 Homebrew、nvm 一类的目录里）。程序名和参数作为独立的 argv 传给 shell，不拼进命令字符串，所以会话 ID 不经 shell 解析：

| 启动 | argv（`<shell>` 是 shell 的路径，argv[0] 是它的文件名） |
|---|---|
| `Shell`（各 shell） | `-<文件名>`（argv[0] 以 `-` 开头即登录 shell），不带参数 |
| `Resume`，`ClaudeCode`（zsh、bash、ksh、sh、dash） | `<shell> -i -l -c 'exec "$0" "$@"' claude --resume <uuid>` |
| `Resume`，`Codex`（同上） | `<shell> -i -l -c 'exec "$0" "$@"' codex resume <uuid>` |
| `NewSession`（同上） | `<shell> -i -l -c 'exec "$0" "$@"' claude`（或 `codex`） |
| fish 的三种 agent 启动 | `<shell> -i -l -c 'exec $argv' claude --resume <uuid>`（其余同理） |

- `-c` 之后的字符串是常量，只有上表两种；`claude`、`codex`、`--resume`、`resume` 是常量；`<uuid>` 是通过「安全边界」检查的会话 ID。
- `exec` 让 agent 取代 shell，agent 退出即终端退出，`RunState::Exited` 的 `code` 就是 agent 的退出码；shell 找不到程序时是 127。
- fish 的 `-c` 之后的参数进入 `$argv`：实现时用 fish 3.x 验证（「验收」第 3 步的 `launch_argv_*` 用假 shell 校验 argv，`launch_fish_*` 在装了 fish 的机器上手动跑一次，结果写进「实际验证」）。

### 子进程的环境

- 继承启动 Lectern 时的环境，去掉 `LECTERN_` 开头的变量。
- 设置 `TERM=xterm-256color`（Ghostty 的 terminfo 多数机器上没有装）、`COLORTERM=truecolor`、`TERM_PROGRAM=Lectern`、`TERM_PROGRAM_VERSION=<应用版本>`；`PWD` 设为工作目录（shell 保留日志里的写法，不换成规范路径）。
- `LANG`、`LC_ALL`、`LC_CTYPE` 都没有设置时（从 Finder 启动的常见情况），设 `LC_CTYPE=UTF-8`，让程序按 UTF-8 输出。
- 来源和调用方都不能指定环境变量。

### 网格与颜色

- 视口之外的历史行不进帧；滚动由 `Input::Scroll` / `ScrollTo` 在终端线程上移动视口，再生成新帧。
- 行的 `text` 每个字素簇一段：组合字符和 ZWJ 表情序列在同一个格子里。视图排版之后按字形的字节偏移在 `clusters` 里查到列，把字形放在 `列 × 单元宽` 处，所以 CJK、回退字体的字形也对齐网格。
- 颜色在终端线程上解析成 `Rgb`：调色板序号查 `Theme` 推出的 256 色表；默认色取当前默认前景、背景（程序可以用 OSC 10 / 11 改，OSC 110 / 111 恢复）；SGR 7 交换前景与背景；`bg` 与默认背景相同时写 None。
- `key` 用标准库的 `DefaultHasher`（固定种子）只对脏行计算。不同行哈希相同的概率可以忽略；碰撞的后果只是视图用错一次排版缓存，下一次该行变化时恢复。

### 帧的节奏

- 有关注者时，输出之后：距上一帧 ≥ 8 ms 就立即生成并发布，否则等到满 8 ms。所以空闲后的第一个回显没有额外延迟，持续输出时每秒最多 120 帧。
- 程序开了同步输出（mode 2026）时不发布，直到它关闭这个模式或已过 200 ms。
- 每轮最多解析 1 MiB 的输出就检查一次是否该发布，大量输出（`cat` 大文件）时画面照常前进。

## 数据流与线程

```
视图（主线程） ──Input──▶ 命令队列 + 唤醒字节 ──▶ 终端线程 terminal-<id>
                                                   │ poll(主端 fd, 唤醒 socket)
         ◀── TerminalEvent::Frame ── 发布 Arc<Frame> ┤ 读输出 → vt_write → RenderState → 帧
                                                   │ 编码输入 → 待写缓冲 → 非阻塞写主端
                              等待线程 terminal-<id>-wait ── child.wait() ──▶ 退出状态
```

- 每个终端两个专属线程，都经插件的 `own_with` 拿到的闸门用 `executor.run_dedicated_gated` 启动（与 `review` 的后台任务相同的写法，句柄由 kernel 保管，停用时随之取消）：
  - 终端线程 `terminal-<id>`：持有 libghostty-vt 的 `Terminal`、`RenderState`、两个编码器和 `MasterPty`。`poll` 两个 fd：主端（可读；有待写数据时也等可写）和唤醒 socket（`UnixStream::pair`，非阻塞）。没有工作时无限期阻塞在 `poll` 上，不定时唤醒。
  - 等待线程 `terminal-<id>-wait`：阻塞在 `Child::wait`，返回后把退出状态放进命令队列、写唤醒字节，线程结束。
- 进程退出之后，终端线程留下，只 `poll` 唤醒 socket，保留最后的画面和滚动缓冲供滚动、选择、复制，`close` 之后结束。
- 线程数：没有终端时为 0；每个运行中的终端 2 个，已退出的 1 个。
- 启动（终端线程上，IO 都在这里）：规范化工作目录、检查是目录、`Projects::locate` 的项目与 `open` 时相同；选 shell；`openpty`、`spawn_command`，随后 drop `SlavePty`（父进程不留从端，否则读不到挂断）；把主端设为非阻塞；状态改为 `Running`。任一步失败时状态改为 `Failed`，线程留下（同已退出）。
- 读：每次 `read` 最多 64 KiB，读到 `EAGAIN` 或本轮累计 1 MiB。读到 0 或 `EIO`（从端都已关闭）后不再 `poll` 主端；状态等等待线程的结果再改。
- 写：输入编码后追加到待写缓冲（上限 4 MiB，超出的输入整条丢弃并计入 `dropped_inputs`），主端可写时非阻塞写出。程序不读输入时缓冲停在上限，不阻塞任何线程。
- 帧：有关注者且有变化时，`RenderState::update` 之后只为脏行重建 `GridRow`，其余行沿用上一帧的 `Arc`；新帧放进终端表，再发 `TerminalEvent::Frame`。没有关注者时只解析，不建帧。
- 空闲压缩：输出停止 2 秒、`compression_activity` 没变时，在 `poll` 的间隙调用 `compress(Incremental)`，每次一步，直到不再 `Pending`；期间有输入或输出就停下。
- 静止（[kernel「静止」](kernel.md#静止)）：终端线程每一轮实际工作（处理输入、读输出、生成帧、压缩一步）开始时取一个 `BusyGuard`，这一轮结束（帧已发布）时放下。视图在主线程上画这一帧不计入：程序可以一直输出，如果要等视图画完才放下，隐藏的 Terminal 页会让应用永远不静止。终端在运行、但没有输出时不持有守卫，不妨碍静止。
- 关闭（`close`、项目关闭、插件停用）：在调用线程上从终端表去掉条目、向终端线程发关闭命令；终端线程收到后发 `Listed`，进程还在运行时先 `ChildKiller::kill`（向 shell 发 `SIGHUP`），再 drop `MasterPty`（关闭主端，前台进程组收到挂断）；2 秒内等待线程没有报告退出时，向进程组发 `SIGKILL`，再等 1 秒；然后释放 `Terminal` 和滚动缓冲，线程结束。等待线程在进程退出时结束。
- 项目关闭：监听 `ProjectEvent::Closed`，关闭 `project` 相同的终端。之前是否询问用户由 `conversation` 的离开守卫决定（「未决问题」1）。
- 插件停用：`Active` 的 drop 关闭全部终端（非阻塞，只发关闭命令）。应用退出时进程随后结束，主端随之关闭，子进程收到挂断。
- 主线程上只有 `open` 的校验（读会话列表、`Projects::locate`，无 IO）、`input` 的入队、`frame` / `list` 的 `Arc` 复制和 `watch` / `close` 的表操作，都不阻塞（AGENTS.md「禁止事项」：UI 线程上不做 IO）。
- 帧是 libghostty-vt 的实时状态，不是从 git 对象派生的数据，不按 blob OID 缓存（架构规则 5 不适用）；行之间的复用见「帧」。
- 终端线程的 QoS 沿用 executor 对专属线程的设置（`on_thread_start(ThreadKind::Dedicated)`，`app` 经 `platform` 设置），本 crate 不另设。

## 安全边界

恢复命令是安全边界（[agents 设计](agents.md)「服务接口」里 `ResumeCommand` 下的规则，[ADR 0018](../adr/0018.md)「能力声明」）：本 crate 在插件沙箱之外、以用户身份运行它。`open(Launch::Resume(key))` 在调用线程上按顺序检查，任一条不满足就返回对应的 `LaunchError`，不启动进程：

1. 来源是内置的：`key.source` 是 "session-claude-code" 或 "session-codex"，否则 `UntrustedSource`。进程插件代理的 `id()` 是宿主生成的 `ext.<插件 ID>.<ID>`，不会等于内置 ID；代理的 `resume` 总是返回 None。以后内置的 ACP 来源在这里加一项。
2. 在 `ServiceSet<dyn SessionSource>` 里找 `id()` 等于 `key.source` 的提供者；找不到，或它的 `sessions().get(key)` 为 None 时 `UnknownSession`。
3. 它的 `resume(key)` 为 None 时 `NotResumable`。
4. `ResumeCommand` 的变体与来源一致（"session-claude-code" 只给 `ClaudeCode`，"session-codex" 只给 `Codex`），`session_id` 等于 `key.id`（两个内置来源都这样给出），且是 UUID 的规范写法（36 个字符，8-4-4-4-12，小写十六进制），否则 `InvalidSessionId`。
5. 会话摘要的 `place` 是 `Place::Located(l)`，且 `l.project` 仍在 `Projects::open_projects()` 里，否则 `OutsideProject`。`Located` 是 `agents` 用规范化之后的 cwd 经 `locate` 得到的，所以调用线程上不再规范化（那是 IO）。工作目录用摘要里的 `cwd` 原样（agent 按 cwd 找会话），不取自来源的其他字段。
6. 终端线程启动进程之前规范化 `cwd`、确认是目录，再 `Projects::locate`：结果的项目必须与第 5 条相同，否则 `StartError::OutsideProject` 或 `CwdMissing`（会话扫描到启动之间目录被删除、被换成指向项目之外的符号链接，或项目已关闭）。

其余启动的规则：

- `NewSession` 与 `Shell` 的 `cwd` 必须是规范路径，由调用方从项目快照取得（worktree 根、文件夹项目根）；调用线程上 `Projects::locate(cwd)` 为 None 时 `OutsideProject`，终端线程上同样做第 6 条检查。
- 程序名和固定参数只来自「启动命令表」；来源只给出会话 ID，调用方只给出枚举，都不能给出程序、参数或环境变量。
- 程序输出能触发的动作：
  - 剪贴板：不注册 `on_clipboard_write`，OSC 52 写剪贴板被忽略；读剪贴板 libghostty-vt 一律忽略。
  - 回显：ENQ 的应答为空；不注册任何把程序可控的文字写回输入的应答（标题报告等），实现时用测试确认（`no_echo_of_program_text`）。设备属性、XTVERSION（应答 `Lectern <版本>`）、尺寸、颜色与明暗的查询只应答固定信息和 `Theme` 的颜色。
  - 标题只显示为纯文本；OSC 7 报告的目录不使用。
  - 响铃限流（每个终端每 500 ms 最多一次事件）。

威胁模型同 AGENTS.md「Review guidelines」：防的是终端里显示的内容（被审阅的文件、agent 的输出）借转义序列做额外的事；已经能以用户身份运行任意程序的攻击者不在范围内。终端里运行的 agent 本身有用户的全部权限，这是用户的选择，与 Analyze 的沙箱无关。

## 性能

对照[性能预算](tech-research.md#性能预算)：

| 预算 | 本 crate 的影响 | 怎样满足 |
|---|---|---|
| 冷启动 20 ms（ADR 0017） | 首帧之前不做任何事 | `AfterFirstFrame` 激活；`activate` 只建空表，< 1 ms（kernel「性能」的 `plugin/*/activate`）。静态库只增加二进制的代码段，不在启动时做初始化 |
| 空闲内存 60 MB（空窗口）、100 MB（monorepo） | 没有终端时不建线程、不打开 PTY | `plugin/terminal/resident` < 1 MB。空闲预算的采样里没有打开的终端 |
| 滚动与输入 120 fps | 回显与输出不占主线程 | 解析、编码、建帧都在终端线程；主线程上的 `input` ≤ 2 µs、`frame` 只复制 `Arc`。整条路径由 `app/input_frames` 判定（负责的 crate 是 `terminal_view`） |
| 安装包 ≤ 80 MB（ADR 0018） | 静态链接的 `libghostty-vt.a` | 实现 PR 记录 `app/bundle_size` 的增量（预计几 MB），写进「实际验证」 |

打开的终端不属于任何空闲预算，它的内存是新的份额（下表）。8 个终端都写满滚动缓冲时约 8 × 24 MB，与 Terminal.app 一类终端相当；份额以单个终端计，便于发现回退。

新增基准（`bench` 的预算表，实现 PR 加入；`Time` 目标在 release 构建里测，footprint 用 [bench 设计「进程内基准」](bench.md#进程内基准)里子进程测常驻增量的方式，因为 libghostty-vt 的页面不经 Rust 的分配器）：

| ID | 预算 | 测量 |
|---|---|---|
| `terminal/open` | 中位数 ≤ 100 µs | 调用线程上的 `open`：校验、建条目、提交两个专属线程 |
| `terminal/input` | 中位数 ≤ 2 µs | 主线程上的一次 `input`：入队与唤醒字节 |
| `terminal/parse_plain` | 中位数 ≤ 10 ms | 1 MiB 纯 ASCII、每行 120 列，120×40，`vt_write` 到解析完成 |
| `terminal/parse_sgr` | 中位数 ≤ 25 ms | 1 MiB、每 8 个字符换一次 24 位色的输出 |
| `terminal/frame_full` | 中位数 ≤ 300 µs | 120×40 全部行脏，生成一帧 |
| `terminal/frame_echo` | 中位数 ≤ 30 µs | 只有一行脏（输入回显），生成一帧 |
| `terminal/echo_latency` | 中位数 ≤ 1 ms，p95 ≤ 3 ms | 真实 PTY：`Input::Text("x")` 到回显程序的输出出现在新帧里（假程序见「测试」）；只在 macOS 判定 |
| `terminal/idle_footprint` | ≤ 4 MB | 子进程：一个终端，120×40，输出 4 KiB 后生成一帧，常驻增量 |
| `terminal/scrollback_footprint` | ≤ 24 MB | 子进程：输出 10,000 行、每行 120 列带颜色，生成一帧，常驻增量 |
| `plugin/terminal/resident` | < 1 MB | 进程级，两次启动之差（[bench 设计「进程级基准」](bench.md#进程级基准)），没有打开的终端 |
| `plugin/terminal/activate` | kernel 的默认条目（< 1 ms） | 进程级插件基准实现之前在 `TestKernel` 里取四段耗时，同 `plugin/review/activate` |

滚动缓冲 10,000 行的单位按 libghostty-vt 的文档是行；实现时如果测出它是字节，按 `terminal/scrollback_footprint` 的份额换算上限，份额不变。

## 状态与异常

| 情况 | 表现 |
|---|---|
| 会话不能恢复（`LaunchError` 的各种） | `open` 返回错误，不创建终端；调用方显示一行提示（`conversation` 的 Fluent 键） |
| 已有 8 个终端 | `LaunchError::TooMany`；调用方提示先关闭一个 |
| 工作目录已不存在 / 指向项目之外 | `RunState::Failed(CwdMissing / OutsideProject)`，帧为空白，视图显示原因 |
| 找不到 shell / PTY 打开失败 | `Failed(NoShell / Spawn)`，warn 日志带错误码 |
| `claude` / `codex` 不在 `PATH` 上 | shell 输出「command not found」，`Exited { code: 127 }`；视图对 agent 启动的 127 显示一行提示 |
| 程序退出 | `Exited`，最后的画面和滚动缓冲保留，可以滚动、选择、复制；同一会话再次 `open` 时关闭它、开新的 |
| 程序不读输入、输入堆积 | 待写缓冲满 4 MiB 后丢弃新的输入，`dropped_inputs` 加一，warn 日志（每个终端只记第一次） |
| 程序忽略 `SIGHUP` | 关闭后 2 秒 `SIGKILL` 进程组 |
| 应用退出时程序仍在运行 | 插件停用时发 `SIGHUP`、关闭主端；忽略挂断的后台进程会留下（「已知限制」第 3 条） |
| 大量输出 | 每 8 ms 最多一帧，解析不落后于读；程序在 PTY 缓冲满时自然阻塞 |
| 读写主端出错 | warn 日志（`Value::io`），停止读写，状态等等待线程的结果 |
| 终端线程 panic | executor 捕获并记录；本终端的状态改为 `Failed(Spawn { errno: None })`，其他终端不受影响 |

## 日志与诊断

规则见 [ADR 0046](../adr/0046.md) 与 [logging 设计「隐私」](logging.md#隐私)。字段值的写法集中在 `src/diag.rs`；`LaunchKind`、`AgentCommand`、`ShellKind`、`LaunchError`、`StartError` 各自实现 `Field`（写成固定的小写名）。

- 默认记录：
  - info `terminal started`：`term`（`TerminalId`）、`launch`（`resume` / `new_session` / `shell`）、`agent`（`claude_code` / `codex`，只在 agent 启动时）、`shell`（`ShellKind`）、`argc`（argv 的个数）、`cwd`（`Value::path`）、`pid`、`cols`、`rows`。这就是 logging「隐私」对子进程要求的程序名（不含目录）、参数个数、工作目录、pid。
  - info `terminal exited`：`term`、`pid`、`code`、`signaled`、`ran_ms`。
  - info `terminal closed`：`term`、`reason`（`user` / `project_closed` / `deactivated` / `replaced`）、`killed`（是否用了 `SIGKILL`）。
  - warn `terminal launch refused`：`reason`（`LaunchError`）、`launch`。
  - warn `terminal start failed`：`term`、`reason`（`StartError`）、`error`（`Value::io`，只在 `Spawn`）。
  - warn `shell not supported, using /bin/zsh`：`shell`（`Value::path`，shell 的路径）。
  - warn `pty read failed` / `pty write failed`：`term`、`error`（`Value::io`）。
  - warn `terminal input dropped`：`term`、`bytes`（这次丢弃的字节数）；每个终端只记第一次。
- 详细级别：
  - debug `terminal throughput`：每个终端每秒最多一条，`term`、`read_bytes`、`frames`、`max_frame_us`、`dirty_rows`。
  - debug `terminal resized`：`term`、`cols`、`rows`。
  - debug `terminal compressed`：`term`、`steps`。
  - trace `terminal input`：`term`、`kind`（输入的变体名）、`bytes`（编码后的字节数）。
- 关联 ID：终端线程和等待线程上的记录由 executor 带上 `plugin`、`act`、`task`；恢复的终端在线程开始时用 `scope` 设 `session`（会话 UUID 的字节，与 `agents`、`review` 相同）。会话 ID 只作为关联 ID 出现，不作为参数记录。
- 不记录：终端输出的任何字节、标题、OSC 7 的目录、选区与复制的文字、粘贴与输入的文字（包括按键产生的字符）、argv 的内容（命令字符串、程序名之后的参数）、环境变量的名字和值、`Theme` 的颜色值。只记长度、计数和种类。
- 金丝雀测试 `logs_hold_no_terminal_content`：`testing::capture()` 在 trace 级别下，假程序输出含金丝雀的文字、OSC 0 标题、OSC 7 目录；测试粘贴、输入、复制含金丝雀的文字；子进程环境里有一个值为金丝雀的变量；然后 `assert_absent`。

## 测试

- 用假程序，不用真实的 `claude`、`codex`，不依赖网络和登录状态。假程序是本 crate 的一个 bin 目标 `lectern-terminal-fake`（`required-features = ["test-support"]`），集成测试从 `CARGO_BIN_EXE_lectern-terminal-fake` 取路径。它有三种用法：
  - 作为 shell（`with_shell(fake, ShellKind::Zsh)` 等）：把收到的 argv、工作目录和环境变量的名字写进测试给的记录文件，再按夹具回放输出。用来逐项核对启动命令表。
  - 作为 agent：测试建一个临时目录，放名为 `claude`、`codex` 的符号链接指向它，放在 `PATH` 最前面；记录 argv 并回放夹具。
  - 回显模式：把输入原样写回输出（`terminal/echo_latency` 和输入测试）。
- 端到端的一条：`with_shell("/bin/sh", ShellKind::Sh)`、`HOME` 设为临时目录（不读用户的配置）、`PATH` 以假 `claude` 的目录开头，确认 `exec "$0" "$@"` 这条链路把 argv 原样传到 agent。
- 夹具是手写的字节序列（`tests/fixtures/*.vt`），不取自真实会话或真实程序的输出：ANSI 16 色、256 色、24 位色、粗体斜体下划线（含波浪线与颜色）、反显、CJK 宽字符、组合字符、ZWJ 表情、制表符、软换行、备用屏幕、光标移动与形状、清屏、OSC 0 / 2 / 7 / 10 / 11、mode 2026。
- 帧的 insta 快照（`frame_*`）：每份夹具一份，写成每行的文字、列位置与样式段，逐个看过。
- 帧的复用（`row_reuse_*`）：只改一行时其余行的 `Arc` 与上一帧相同（`Arc::ptr_eq`），`key` 不变；选区变化不改变任何行的 `key`。
- 输入编码（`input_*`）：方向键在普通模式与 DECCKM 下的序列；kitty 键盘协议开启后的编码；bracketed paste 开与不开；鼠标跟踪开启时编码、按 Shift 时改为选区；备用屏幕下滚轮换成方向键；焦点事件只在 mode 1004 开启时写出。
- 颜色（`theme_*`）：OSC 10 / 11 / 4 的查询应答取 `Theme`；mode 2031 的明暗报告；`SetTheme` 之后全部行的颜色更新。
- 帧的节奏（`pacing_*`）：持续输出时相邻帧间隔 ≥ 8 ms；空闲后的第一次输出立即发布；mode 2026 期间不发布、200 ms 后发布。
- 安全（`resume_refuses_*`）：非 UUID、大写 UUID、带空格的 ID、ID 与 `key.id` 不同、来源 ID 为 `ext.x.y`、来源不在集合里、`resume` 为 None、`Place` 不是 `Located`、`cwd` 是指向项目之外的符号链接（第 6 条），各返回对应的错误或 `Failed`，假程序都没有被启动（记录文件不存在）；`no_echo_of_program_text`：程序请求标题报告等查询后，输入端没有出现程序可控的文字。
- 生命周期：`no_threads_until_open`（激活后没有 `terminal-*` 线程）；`resume_reuses_running_terminal`；`exited_terminal_is_replaced`；`close_*`（`SIGHUP` 后退出；忽略 `SIGHUP` 的假程序 2 秒后被 `SIGKILL`；两个线程都结束）；`project_closed_closes_its_terminals`；`deactivate_closes_all`；`exit_codes_*`（正常退出、127、被信号结束）；`input_overflow_*`；`quiescent_after_output_stops`（输出停止后 kernel 报告静止）；`concurrent_spawn`（8 个终端在其他线程大量分配内存时同时启动，都能运行）。
- PTY 与真实进程的测试在 Linux 与 macOS 都跑（本 crate 不依赖 GPUI）；`terminal/echo_latency` 只在 macOS 判定。

## 对其他 crate 的要求

1. `services`：新增 `terminal` 模块，内容是「接口」的服务、事件、输入、帧几节（本文的第 1 步）。只用标准库和 serde，不加依赖。
2. 工具链与 CI（`ci.md`、`justfile`、根 AGENTS.md「命令」、`cargo-workspace.md`）：
   - Linux 与 macOS 的 CI 任务在构建之前安装 Zig 0.15.2（官方发布包，校验 SHA-256），`git` 已有。`OUT_DIR` 在 `target/` 里，随现有的 `Swatinem/rust-cache` 缓存；缓存未命中时多出克隆 Ghostty 和 Zig 构建的时间（未测，预计 1–3 分钟），实现 PR 把实测写进 ci 设计的「实际验证」，并核对 Linux 任务约 2 分钟、macOS 任务 15 分钟的目标是否仍满足。
   - `justfile`：`check`、`test`、`bench` 之前检查 `zig version` 是 0.15.2，不是时报错并给出安装方式。
   - 根 AGENTS.md「命令」一节的所需工具加上 Zig 0.15.2。
   - `cargo-workspace.md` 里 libghostty-vt 那一行改为本文的版本与 Ghostty 提交。
   - `just deny` 通过；Ghostty 编进静态库的 C / C++ 依赖的许可证由实现 PR 人工核对（「依赖」）。
3. `conversation`：
   - Terminal 页的插件在 `requires` 里依赖 `dyn Terminals`，与不依赖它的部分分成两个插件，终端不可用时 Conversation 的其他页照常可用。
   - 用 `Launch::Resume(key)` 恢复会话；页可见时持有 `FrameInterest`，隐藏时 drop；监听 `TerminalEvent`，转到主线程后让视图重绘；关闭按钮调用 `close`。
   - 运行中的终端在离开时是否确认，按「未决问题」1 注册离开守卫（`LeaveScope::Project`、`Worktree`、`App`）。
   - `LaunchError`、`StartError` 与 127 退出码的提示文字（Fluent 键，en 与 zh-CN）。
4. `terminal_view`：把 GPUI 的按键、IME 文字、粘贴、鼠标、滚动、焦点、尺寸转成 `Input`；从 `ui` 的 `Palette` 推出 `Theme`，外观变化时发 `SetTheme`；按 `Frame` 绘制；拥有 `app/input_frames`（另写设计文档）。
5. `app`：装配 `TerminalPlugin::new(TerminalDeps { executor })`，排在 `project` 与 `agents` 之后（第 4 步）；`app/bundle_size` 记录静态库的增量。
6. `bench`：预算表加入「性能」里的条目；`app/input_frames` 的 profile 用 `with_shell` 指向一个回显程序（本 crate 的假程序，或 bench 自带的同类程序，由 bench 的实现决定），不运行用户的 shell 和它的配置。
7. `review_view` / `conversation`（#22 发回会话）：经 `Input::Paste` 加 `Input::Key(Enter)` 写入恢复的会话。
8. `project_home`：空状态的 New session 调用 `Launch::NewSession`（本 issue 之外）。

## 未决问题

1. **还有运行中的终端时关闭项目或退出应用，是否先确认。** 选项：(a) 确认，提示里写出运行中的终端数，选「关闭」后才继续；(b) 不确认，直接挂断。默认 (a)。确认由 `conversation` 的离开守卫实现，本 crate 提供 `list()` 里的状态。
2. **是否允许程序经 OSC 52 写剪贴板。** 一些程序（tmux、Neovim 的远程复制）用它复制文字。选项：(a) 不允许（默认）；(b) 每个终端第一次写时询问；(c) 允许。默认 (a)，理由见「安全边界」；选 (b) 时需要 `conversation` 提供询问界面，本 crate 加一个回调。
3. **离开会话的 Conversation 页时终端是否继续运行。** 选项：(a) 继续运行，回到页面时接着显示（默认，agent 可能还在工作）；(b) 离开即关闭。本 crate 按 (a) 设计：只在 `close`、项目关闭、插件停用时结束进程。

## 已知限制

1. 本插件依赖 `dyn SessionSource`：两个内置会话来源都不可用（被用户停用）时，终端插件不激活，登录 shell 也打不开。kernel 没有可选依赖；要解除，需把登录 shell 拆成不依赖会话来源的第二个插件，等有这个需求时再做。
2. 不在启动命令表里的 shell（如 nushell、xonsh）退回 `/bin/zsh`，这些用户的 `PATH` 设置不生效，`claude` 可能找不到（127）。
3. 应用退出时忽略 `SIGHUP`、又不在前台进程组的后台进程会留下，与关闭 Terminal.app 窗口的行为相同。
4. 闪烁文字按普通文字显示；kitty 图形协议、Sixel 不支持。
5. 视图在主线程上画帧不计入静止（「数据流与线程」）：UI 探针在静止之后读语义树时，终端的最后一帧可能还没画出，探针要另等一帧。

## 验收

**第 1 步：`services::terminal`（`services` 的 PR，`Refs #12`）。**

- `crates/services/src/terminal/` 有「接口」里服务、事件、输入、帧的全部类型，`lib.rs` 的模块说明加上这一项。
- `just test -p lectern-services` 里 `terminal_events_round_trip`（`TerminalEvent` 的每个变体经 serde 往返）、`frame_interest_releases_once` 通过。
- `cargo tree -p lectern-services --edges normal` 没有新增依赖。

**第 2 步：libghostty-vt 接入与网格快照（本 crate 的 PR，`Refs #5`）。**

- 工作区依赖加上 `libghostty-vt = { version = "=0.2.2", default-features = false }`；`cargo tree -p lectern-terminal -e features` 里没有 `kitty-graphics`、`log`、`tracing`、`link-dynamic`。
- `rg 'set_logger' crates/terminal/src` 没有结果。
- 「对其他 crate 的要求」第 2 项的 CI、`justfile`、AGENTS.md 与 `cargo-workspace.md` 改动在同一个 PR 里；Linux 与 macOS 的 CI 任务通过；`just deny` 通过。
- 不经 PTY 的引擎（`test-support` 的 `testing::Engine`：喂字节、发输入、取帧）完成；`just test -p lectern-terminal` 里 `frame_*`（快照逐个看过）、`row_reuse_*`、`input_*`、`theme_*`、`pacing_*`、`copy_*` 通过。
- `just bench terminal/` 里 `terminal/parse_plain`、`terminal/parse_sgr`、`terminal/frame_full`、`terminal/frame_echo`、`terminal/idle_footprint`、`terminal/scrollback_footprint` 在预算内。

**第 3 步：PTY、启动、插件与服务（本 crate 的 PR，`Refs #12`）。**

- `Cargo.toml` 加 `plugin = true`，`python3 scripts/check_architecture.py` 通过；`portable-pty = "=0.9.0"` 与 rustix 的 `event`、`process` feature 加入工作区依赖，`just deny` 通过。
- `just test -p lectern-terminal` 里 `launch_argv_*`（启动命令表的每一行、每种 shell）、`launch_env_*`（有 `TERM`、`COLORTERM`、`TERM_PROGRAM`，没有 `LECTERN_` 开头的变量）、`resume_runs_through_posix_sh`、`resume_refuses_*`、`no_echo_of_program_text`、`no_threads_until_open`、`resume_reuses_running_terminal`、`exited_terminal_is_replaced`、`close_*`、`project_closed_closes_its_terminals`、`deactivate_closes_all`、`exit_codes_*`、`input_overflow_*`、`quiescent_after_output_stops`、`concurrent_spawn`、`logs_hold_no_terminal_content` 通过。
- `rg 'try_clone_reader|take_writer' crates/terminal/src` 没有结果。
- `just bench terminal/` 里 `terminal/open`、`terminal/input`、`terminal/echo_latency`（macOS）、`plugin/terminal/activate` 在预算内。
- 装了 fish 的机器上手动跑一次 `launch_fish_*`，结果写进「实际验证」。

**第 4 步：`app` 装配（`app` 的 PR，`Refs #12`）。**

- `app` 的 profile 里有 "terminal"，`app/idle_empty` 的 `Plugins` 应答列出它。
- `plugin/terminal/resident` < 1 MB；`app/idle_empty`、`app/cold_start` 在预算内；`app/bundle_size` ≤ 80 MB，增量写进「实际验证」。
- 启动后、没有打开终端时，`ps -M <pid>` 里没有 `terminal-` 开头的线程。
