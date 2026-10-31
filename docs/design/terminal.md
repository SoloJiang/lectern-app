# terminal 设计

状态：已接受；「验收」第 1 步（`services::terminal`，[#318](https://github.com/SoloJiang/lectern-app/pull/318)）、第 2 步（libghostty-vt 接入与网格快照）和第 3 步（PTY、启动、插件与服务，#338）和第 4 步（`app` 装配，`Refs #12`）已实现。`terminal_view`、`conversation` 的 Terminal 页和 `app/input_frames` 还没有实现，所以 issue #12 仍开着。

对应 issue [#12](https://github.com/SoloJiang/lectern-app/issues/12)（终端：portable-pty + libghostty-vt，会话 resume）与 [#5](https://github.com/SoloJiang/lectern-app/issues/5) 的内核部分（libghostty-vt 接入）。`terminal` 启动 PTY 和其中的程序（`claude --resume`、`codex resume`、新会话、登录 shell），把输出交给 libghostty-vt 解析，生成视口的网格快照交给视图；视图发来的按键、粘贴、鼠标和尺寸变化经 libghostty-vt 编码后写回 PTY（[技术调研「终端：接入 libghostty-vt 内核」](tech-research.md#终端接入-libghostty-vt-内核)、[ADR 0002](../adr/0002.md)）。终端在界面的「收起」一层，是 Conversation 的 Terminal 页（[交互规范「三层结构」](interaction.md#三层结构)、[ADR 0012](../adr/0012.md)）；网格的绘制在 `terminal_view`，页面在 `conversation`。

## 范围

- 做：
  - 插件 "terminal"：提供单实现服务 `dyn Terminals`（接口和数据类型放在 `services::terminal`，本文定义）。
  - 启动：恢复会话（`Launch::Resume`）、在某个目录里开始新的 agent 会话（`Launch::NewSession`）、登录 shell（`Launch::Shell`）。恢复命令按 [agents 设计](agents.md)「服务接口」里 `ResumeCommand` 的安全规则执行（见「安全边界」）。agent 用登录 shell 的环境启动（`platform::login_env`，与 `agents` 的 ACP 客户端共用）。
  - PTY：经 portable-pty 打开、设置尺寸、读写；子进程的等待、挂断与强制结束。
  - 终端状态：libghostty-vt 的 `Terminal`，滚动缓冲上限 10,000 行；按键、粘贴、鼠标、焦点按程序开启的模式编码（`key::Encoder`、`mouse::Encoder`、`paste`、`focus`）；选区与复制的纯文本。
  - 帧：视口的行、样式段、光标、选区、滚动位置、模式与运行状态，作为不可变快照（`Arc<Frame>`）交给视图；内容没变的行在相邻两帧之间共用。
  - 颜色：视图给出默认前景、背景、光标色和 16 个 ANSI 色（`Theme`），本 crate 据此解析每个格子的颜色，并应答程序的颜色查询。
  - 生命周期：项目关闭时关闭它的终端；插件停用时关闭全部终端。
- 不做：
  - 网格的绘制、字形缓存、IME、快捷键、右键菜单：`terminal_view`。
  - Terminal 页、Resume 按钮、「在别处运行」的提示、退出前的确认、「哪一处在驱动会话」的判断：`conversation`。
  - 剪贴板的读写：视图经 GPUI 读写，本 crate 只给出选区的文字、只接收要粘贴的文字。
  - 会话发现与恢复命令的来源：`agents`（`SessionSource::resume`）。
  - 登录 shell 环境的获取与缓存：`platform`（「对其他 crate 的要求」第 2 项）。
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
| `lectern-kernel` | `Plugin`、`PluginHandle::own_with`、`BusyGuard`、事件、`catch` | 插件机制 |
| `lectern-services` | `Terminals` 接口与数据类型（本文定义）；调用 `Projects::locate` / `open_projects`、`SessionSource::sessions` / `resume` | 消费方只依赖接口（架构规则 7） |
| `lectern-executor` | 每个终端的两个专属线程（`run_dedicated_gated`） | 阻塞的 `poll` 与 `wait` 不占工作线程 |
| `lectern-platform` | `login_env`：用户的登录 shell、登录 shell 的环境、在 `PATH` 里找程序 | 与 `agents` 共用一份实现和缓存；非插件的基础设施库 |
| `lectern-logging` | 运行日志（「日志与诊断」）；测试用 `test-support` 捕获记录 | [ADR 0046](../adr/0046.md) |
| `libghostty-vt = "=0.2.2"`，`default-features = false`；`libghostty-vt-sys = "=0.2.2"` | 终端状态机（ADR 0002） | 见下文「libghostty-vt」 |
| `portable-pty = "=0.9.0"` | 打开 PTY、启动子进程、设置尺寸、挂断 | 技术调研选定；MIT |
| `rustix`（工作区已有，加 `event`、`process` feature） | 主端 fd 的非阻塞读写与 `poll`；向进程组发 `SIGKILL` | 已在工作区，不引入新 crate |

### libghostty-vt

- 版本锁 `=0.2.2`（crates.io，MIT OR Apache-2.0，`rust-version` 1.90，工具链 1.97 满足）。它依赖 `libghostty-vt-sys`（写的是 `0.2.2`，即 `^0.2.2`）、`bitflags`、`int-enum`。`libghostty-vt-sys` 0.2.2 的构建脚本固定 Ghostty 的提交 `a887df42c56f6de86c0fe6da9c4eeca37931e083`，就是 ADR 0002 说的「libghostty-rs 的 vendored 构建」：首次构建时把这个提交克隆进 `OUT_DIR`，用 Zig 编出 `libghostty-vt.a` 并静态链接。
- Ghostty 的源码由 `libghostty-vt-sys` 的版本决定，所以本 crate 直接依赖 `libghostty-vt-sys = "=0.2.2"`（不调用它，只为锁版本）：否则一次 `cargo update` 就能把它升到 0.2.x，换掉 Ghostty 的提交，绕过 AGENTS.md 的「升级先确认」。升级 libghostty 就是同时改这两个版本号。
- 关掉默认 feature：不要 `kitty-graphics`（图片不在 P1）、`log` 与 `tracing`（libghostty 的日志不经 `lectern-logging` 的宏，ADR 0046），也不调用 `set_logger`，libghostty 的日志被丢弃。不开 `link-dynamic`，安装包里不多一个 dylib。
- 构建链需要 Zig 0.15.2 和 `git`。Zig 版本取自那个 Ghostty 提交的 `build.zig.zon`（`minimum_zig_version`）；Zig 的次版本之间不兼容，固定这一个版本，实现 PR 用这个版本实际构建一次确认。首次构建要访问 `github.com`（克隆 Ghostty）和 `deps.files.ghostty.org`（Zig 依赖，`build.zig.zon` 按哈希固定），之后按 `OUT_DIR` 复用。`Cargo.lock` 与 `--locked` 不覆盖构建脚本取的源码；源码由上面的提交号和哈希固定，所以构建结果仍然确定。需要离线或共用一份源码时，可设 `GHOSTTY_SOURCE_DIR` 指向同一提交的检出、`GHOSTTY_ZIG_SYSTEM_DIR` 指向预取的 Zig 依赖（libghostty-vt-sys 支持的两个变量），只影响构建速度，不改结果。
- `cargo-deny` 只检查 Rust crate。Ghostty 编进静态库的 C / C++ 依赖（如 simdutf、highway）不在它的范围内：实现 PR 从 `build.zig.zon` 列出它们和许可证，写进本文「实际验证」，确认都在 `deny.toml` 的许可证白名单里。
- 本 crate 不依赖 GPUI，Linux 上的 `--gpui-free-packages` 会包含它，所以 Linux 与 macOS 的 CI 任务、每台开发机都要装 Zig 0.15.2（「对其他 crate 的要求」第 3 项）。
- libghostty-vt 的类型都是 `!Send + !Sync`，只在终端自己的线程上创建和使用（「数据流与线程」）。

### portable-pty

- 版本锁 `=0.9.0`。新引入的传递依赖：`nix` 0.28、`filedescriptor` 0.8、`serial2`、`shell-words`、`downcast-rs`、`log`（`anyhow`、`libc` 已在 `Cargo.lock`）。都是宽松许可，`just deny` 在实现 PR 里检查。
- 只用 `native_pty_system().openpty`、`SlavePty::spawn_command`、`MasterPty::{resize, as_raw_fd, process_group_leader}`、`Child::{wait, process_id, clone_killer}`。不用 `try_clone_reader` / `take_writer`：它们给的是阻塞的 fd 副本，读线程在 `read` 里无法被打断，终端关闭时线程会留下。本 crate 把主端 fd 设为非阻塞，在自己的线程上 `poll`。也不用 `Child::kill`（它先睡眠再发 `SIGKILL`），挂断用 `clone_killer` 给出的 `ChildKiller`（只发 `SIGHUP`）。
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

    /// 测试与基准：`Launch::Shell` 固定启动这个程序，不经 `platform::login_env::user_shell`。
    pub fn with_shell(self, program: impl Into<PathBuf>) -> TerminalPlugin;

    /// 测试与基准：agent 启动用这份环境，不经 `platform::login_env::login_env`（不运行用户的 shell）。
    /// `Launch::Shell` 的环境也从这里继承。
    pub fn with_login_env(self, vars: Vec<(OsString, OsString)>) -> TerminalPlugin;
}

impl Plugin for TerminalPlugin {
    /// provides: [dyn Terminals]
    /// requires: [dyn Projects, dyn SessionSource]
    /// activation: [AfterFirstFrame]
    fn manifest(&self) -> Manifest;

    /// 只建空的终端表、取服务引用：不建线程、不做 IO、不打开 PTY、不取登录环境（< 1 ms）。
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError>;
}
```

插件 ID 是 "terminal"。激活条件用 `AfterFirstFrame`，不用 kernel 示例里的 `Command("terminal.open")`：`conversation` 的终端插件在 `requires` 里依赖 `dyn Terminals`，而命令不会替依赖去满足激活条件（[kernel 设计「依赖与自动启停」](kernel.md#依赖与自动启停)），按命令激活的提供者会让依赖者一直等待。`activate` 只建内存里的空表，代价与按命令激活相同；线程、PTY 和登录环境都在第一次 `open` 时才有，第一次 `open` 之前本插件什么也不做。

依赖 `dyn SessionSource` 的后果：两个内置会话来源都不可用时，本插件不激活，登录 shell 也打不开。这是已知限制（「已知限制」第 1 条）。

### 服务（`lectern_services::terminal`）

```rust
/// 单实现服务，"terminal" 插件提供。不暴露给进程插件（暴露要另写 ADR）。
pub trait Terminals: Send + Sync {
    /// 打开一个终端。任何线程可调用；不阻塞、不做 IO，只读内存里的会话列表和项目快照
    /// （校验见「安全边界」）。返回之前终端已登记（`for_session`、`info` 查得到，状态 `Starting`）；
    /// 登录环境、PTY 和子进程在终端自己的线程上准备，结果随 `TerminalEvent::Listed` 送达
    /// （`RunState::Running` 或 `Failed`）。
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
    /// `ClearScrollback`、`Resize`、`SetTheme`）照常处理到 `close`，所以退出后仍能滚动、选择、复制。
    /// `Starting` 期间发给程序的输入不排队、直接丢弃：要送给程序的内容（如 #22 发回的审阅意见）
    /// 由调用方等到 `RunState::Running`（随 `Listed` 送达）再发。
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

impl AgentCommand {
    /// 程序名（"claude"、"codex"），界面显示用。
    pub fn program(self) -> &'static str;
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
    /// 终端线程 panic（「数据流与线程」）。只影响这一个终端。
    Internal,
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
    /// 粘贴（⌘V，以及审阅意见发回会话 #22）。终端线程先去掉 C0 控制字符（保留 `\t`、`\n`；
    /// ESC 也在其中）和 C1 控制字符（U+0080–U+009F），再交给 libghostty-vt 的 `paste::encode`：
    /// 程序开了 bracketed paste（mode 2004，`claude`、`codex` 都开）时包上 `ESC[200~` / `ESC[201~`，
    /// 没开时把 `\n` 换成 `\r`。不追加回车：是否提交由用户在终端里按 ↵ 决定。
    /// 是否需要用户确认（多行、未开 bracketed paste）由视图按 `Frame::modes` 决定，本 crate 收到就写。
    Paste(Arc<str>),
    /// 鼠标。程序开了鼠标跟踪、且没按 Shift 时编码给程序；否则用于选区
    /// （`clicks` 为 2 选词、3 选行）。
    Mouse(MouseInput),
    /// 滚轮或触控板。开了鼠标跟踪时编码为滚轮事件；在备用屏幕且没开鼠标跟踪时
    /// 换成上下方向键（alternate scroll）；否则滚动视口。
    Scroll(ScrollInput),
    ScrollTo(ScrollTarget),
    /// 视口的格子数与单元的像素尺寸。连续的多个只处理最后一个；进程在运行时同时调整
    /// PTY 的窗口大小（子进程收到 `SIGWINCH`），libghostty-vt 按新宽度重排滚动缓冲。
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

impl StyleFlags {
    /// 依次是第 0 到第 5 位。
    pub const BOLD: u8; pub const ITALIC: u8; pub const FAINT: u8;
    pub const STRIKETHROUGH: u8; pub const OVERLINE: u8; pub const INVISIBLE: u8;
}

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

- 终端表在一把 `Mutex` 里：`TerminalId` 到条目（`TerminalInfo`、最新的 `Arc<Frame>`、关注数、命令队列的发送端、唤醒 socket 的写端）。`list()`、`info()`、`frame()`、`input()` 都只持锁复制一个 `Arc` 或放进队列，不在锁内做别的工作。
- `TerminalId` 进程内从 1 递增，不复用。
- 终端最多 8 个，含已退出、启动失败的；`close` 之后不再计入。同一会话最多一个终端（`open` 的规则）。
- 列表的替换与 `Listed` 事件一一对应，`generation` 单调递增。

### 启动命令

agent 的程序直接启动，不经 shell：程序名和参数是固定的 argv，会话 ID 是独立的一项，不经任何解析。

| 启动 | 程序 | argv |
|---|---|---|
| `Resume`，`ResumeCommand::ClaudeCode` | 在登录环境的 `PATH` 里找到的 `claude` | `claude --resume <uuid>` |
| `Resume`，`ResumeCommand::Codex` | 同上，`codex` | `codex resume <uuid>` |
| `NewSession` | `claude` 或 `codex` | 只有程序名 |
| `Shell` | 用户的登录 shell（`with_shell` 覆盖） | argv[0] 是 `-<文件名>`（以 `-` 开头即登录 shell），不带参数 |

- 程序的查找：`platform::login_env::find_program(name, PATH)`，`PATH` 取自登录环境。找不到时 `FailReason::NotInstalled`。
- 登录环境：`platform::login_env::login_env()`，用户登录 shell（`.zprofile`、`.zshrc` 等）设置之后的全部环境变量。从 Finder 启动的应用只有 launchd 给的最小环境，`claude`、`codex` 常装在 Homebrew、nvm 一类的目录里，代理和 API key 也常在这些文件里设置。第一次需要时在终端线程上取（阻塞，最多 5 秒），整个进程只取一次，与 `agents` 的 ACP 客户端共用，所以 Terminal 页和 Conversation 页里同一个 agent 的环境相同。取不到时退回 Lectern 自己的环境，warn 日志。
- 用户的登录 shell：`platform::login_env::user_shell()`（`$SHELL`，否则用户数据库，否则 `/bin/zsh`）。都不可用时 `FailReason::NoShell`。
- agent 退出即终端退出，`RunState::Exited` 的 `code` 就是 agent 的退出码。

### 子进程的环境

- agent 启动：登录环境；`Shell` 启动：Lectern 自己的环境（登录 shell 自己会读配置）。两者都去掉 `LECTERN_` 开头的变量。
- 设置 `TERM=xterm-256color`（Ghostty 的 terminfo 多数机器上没有装）、`COLORTERM=truecolor`、`TERM_PROGRAM=Lectern`、`TERM_PROGRAM_VERSION=<应用版本>`；`PWD` 设为工作目录（保留日志里的写法，不换成规范路径）。
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
  - 终端线程 `terminal-<id>`：持有 libghostty-vt 的 `Terminal`、`RenderState`、两个编码器和 `MasterPty`。`poll` 两个 fd：主端（可读；有待写数据时也等可写）和唤醒 socket（`UnixStream::pair`，非阻塞）。只有在等帧的节奏、同步输出、空闲压缩或关闭的计时时才带超时，其余时候无限期阻塞在 `poll` 上，不定时唤醒。
  - 等待线程 `terminal-<id>-wait`：阻塞在 `Child::wait`，返回后把退出状态放进命令队列、写唤醒字节，线程结束。
- panic：两个线程的主体都包在 `lectern_kernel::catch` 里，panic 不交给 executor。executor 会把专属任务的 panic 报给闸门，使整个插件进入 `Failed`、关闭全部终端并停用依赖它的插件（[kernel 设计「失败」](kernel.md#失败)）；一个终端出错不应影响其他终端。捕获后先 drop libghostty-vt 的状态，再丢弃 payload（不记它的消息，`Value::panic_message` 只有 kernel、executor 和 logging 能用），结束这个终端的进程（同「关闭」），状态改为 `Failed(Internal)`，记 error（只带 `term`）。libghostty-vt 内部（Zig 代码）的错误不会展开到 Rust，发布构建里 Zig 的安全检查关闭；Zig 自己的 panic 会结束整个进程（「已知限制」第 4 条）。
- 进程退出或启动失败之后，终端线程留下，只 `poll` 唤醒 socket，保留最后的画面和滚动缓冲供滚动、选择、复制，`close` 之后结束。
- 线程数：没有终端时为 0；每个运行中的终端 2 个，已退出或失败的 1 个。
- 启动（终端线程上，IO 都在这里）：规范化工作目录、检查是目录、`Projects::locate` 的项目与 `open` 时相同；agent 启动时取登录环境、在它的 `PATH` 里找程序；`openpty`、`spawn_command`，随后 drop `SlavePty`（父进程不留从端，否则读不到挂断）；把主端设为非阻塞；状态改为 `Running`。任一步失败时状态改为 `Failed`，线程留下（同已退出）。
- 读：每次 `read` 最多 64 KiB，读到 `EAGAIN` 或本轮累计 1 MiB。读到 0 或 `EIO`（从端都已关闭）后不再 `poll` 主端；状态等等待线程的结果再改。
- 写：输入编码后追加到待写缓冲（上限 4 MiB，超出的输入整条丢弃并计入 `dropped_inputs`），主端可写时非阻塞写出。程序不读输入时缓冲停在上限，不阻塞任何线程。
- 帧：有关注者且有变化时，`RenderState::update` 之后只为脏行重建 `GridRow`，其余行沿用上一帧的 `Arc`；新帧放进终端表，再发 `TerminalEvent::Frame`。没有关注者时只解析，不建帧。
- 空闲压缩：输出停止 2 秒、`compression_activity` 没变时，在 `poll` 的间隙调用 `compress(Incremental)`，每次一步，直到不再 `Pending`；期间有输入或输出就停下。
- 静止（[kernel「静止」](kernel.md#静止)）：终端线程每一轮实际工作（处理输入、读输出、生成帧、压缩一步）开始时取一个 `BusyGuard`，这一轮结束（帧已发布）时放下。视图在主线程上画这一帧不计入：程序可以一直输出，如果要等视图画完才放下，隐藏的 Terminal 页会让应用永远不静止。终端在运行、但没有输出时不持有守卫，不妨碍静止。登录环境的获取（最多 5 秒）在这一轮里，持有守卫。
- 关闭（`close`、项目关闭、插件停用、`open` 替换已退出的终端）：在调用线程上从终端表去掉条目、向终端线程发关闭命令；终端线程收到后发 `Listed`，进程还在运行时先 `ChildKiller::kill`（向进程发 `SIGHUP`），再 drop `MasterPty`（关闭主端，前台进程组收到挂断）；2 秒内等待线程没有报告退出时，向进程组发 `SIGKILL`，再等 1 秒；然后释放 `Terminal` 和滚动缓冲，线程结束。等待线程在进程退出时结束。
- 项目关闭：监听 `ProjectEvent::Closed`，关闭 `project` 相同的终端。之前是否询问用户由 `conversation` 的离开守卫决定（「未决问题」1）。
- 插件停用：`Active` 的 drop 关闭全部终端（非阻塞，只发关闭命令）。应用退出时进程随后结束，主端随之关闭，子进程收到挂断。
- 主线程上只有 `open` 的校验（读会话列表、项目列表，无 IO）、`input` 的入队、`frame` / `list` / `info` 的复制和 `watch` / `close` 的表操作，都不阻塞（AGENTS.md「禁止事项」：UI 线程上不做 IO）。
- 帧是 libghostty-vt 的实时状态，不是从 git 对象派生的数据，不按 blob OID 缓存（架构规则 5 不适用）；行之间的复用见「帧」。
- 终端线程的 QoS 沿用 executor 对专属线程的设置（`on_thread_start(ThreadKind::Dedicated)`，`app` 经 `platform` 设置），本 crate 不另设。

## 安全边界

恢复命令是安全边界（[agents 设计](agents.md)「服务接口」里 `ResumeCommand` 下的规则，[ADR 0018](../adr/0018.md)「能力声明」）：本 crate 在插件沙箱之外、以用户身份运行它。`open(Launch::Resume(key))` 在调用线程上按顺序检查，任一条不满足就返回对应的 `LaunchError`，不启动进程：

1. 来源是内置的：`key.source` 是 "session-claude-code" 或 "session-codex"，否则 `UntrustedSource`。进程插件代理的 `id()` 是宿主生成的 `ext.<插件 ID>.<ID>`，不会等于内置 ID；代理的 `resume` 总是返回 None。以后内置的 ACP 来源在这里加一项。
2. 在 `ServiceSet<dyn SessionSource>` 里找 `id()` 等于 `key.source` 的提供者；找不到，或它的 `sessions().get(key)` 为 None 时 `UnknownSession`。
3. 它的 `resume(key)` 为 None 时 `NotResumable`。
4. `ResumeCommand` 的变体与来源一致（"session-claude-code" 只给 `ClaudeCode`，"session-codex" 只给 `Codex`），`session_id` 等于 `key.id`（两个内置来源都这样给出），且是 UUID 的规范写法（36 个字符，8-4-4-4-12，小写十六进制），否则 `InvalidSessionId`。
5. 会话摘要的 `place` 是 `Place::Located(l)`，且 `l.project` 仍在 `Projects::open_projects()` 里，否则 `OutsideProject`。`Located` 是 `agents` 用规范化之后的 cwd 经 `locate` 得到的，所以调用线程上不再规范化（那是 IO）。工作目录用摘要里的 `cwd` 原样（agent 按 cwd 找会话），不取自来源的其他字段。
6. 终端线程启动进程之前规范化 `cwd`、确认是目录，再 `Projects::locate`：结果的项目必须与第 5 条相同，否则 `FailReason::OutsideProject` 或 `CwdMissing`（会话扫描到启动之间目录被删除、被换成指向项目之外的符号链接，或项目已关闭）。

其余启动的规则：

- `NewSession` 与 `Shell` 的 `cwd` 必须是规范路径，由调用方从项目快照取得（worktree 根、文件夹项目根）；调用线程上 `Projects::locate(cwd)` 为 None 时 `OutsideProject`，终端线程上同样做第 6 条检查。
- 程序名和固定参数只来自「启动命令」一节的表；来源只给出会话 ID，调用方只给出枚举，都不能给出程序、参数或环境变量。agent 不经 shell 启动，argv 不经任何解析。
- 粘贴先去掉控制字符（`Input::Paste`），被粘贴的文字不能借 `ESC[201~` 提前结束 bracketed paste、再注入按键。
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
| 空闲内存 60 MB（空窗口）、100 MB（monorepo） | 没有终端时不建线程、不打开 PTY、不取登录环境 | `plugin/terminal/resident` < 1 MB。空闲预算的采样里没有打开的终端 |
| 滚动与输入 120 fps | 回显与输出不占主线程 | 解析、编码、建帧都在终端线程；主线程上的 `input` 目标 ≤ 6 µs（[ADR 0058](../adr/0058.md)）、`frame` 只复制 `Arc`。整条路径由 `app/input_frames` 判定（负责的 crate 是 `terminal_view`） |
| 安装包 ≤ 80 MB（ADR 0018） | 静态链接的 `libghostty-vt.a` | 实现 PR 记录 `app/bundle_size` 的增量（预计几 MB），写进「实际验证」 |

打开的终端不属于任何空闲预算，它的内存是新的份额（下表）。8 个终端都写满滚动缓冲时约 8 × 24 MB，与 Terminal.app 一类终端相当；份额以单个终端计，便于发现回退。第一次启动 agent 时取登录环境要运行一次用户的 shell（通常几百毫秒，上限 5 秒），只在用户点了恢复之后、在终端线程上，期间 Terminal 页显示 `Starting`。

新增基准（`bench` 的预算表，实现 PR 加入；`Time` 目标在 release 构建里测，footprint 用 [bench 设计「进程内基准」](bench.md#进程内基准)里子进程测常驻增量的方式，因为 libghostty-vt 的页面不经 Rust 的分配器）：

| ID | 预算 | 测量 |
|---|---|---|
| `terminal/open` | 中位数 ≤ 150 µs（[ADR 0058](../adr/0058.md)，原来 ≤ 100 µs） | 调用线程上的 `open`：校验、建条目、提交两个专属线程 |
| `terminal/input` | 只记录，目标中位数 ≤ 6 µs（[ADR 0058](../adr/0058.md)，原来 ≤ 2 µs） | 主线程上的一次 `input`：入队与唤醒字节；终端线程睡在 `poll` 上时，写唤醒字节本身就要 3 µs 以上 |
| `terminal/parse_plain` | 中位数 ≤ 14 ms（[ADR 0055](../adr/0055.md)） | 1 MiB 纯 ASCII、每行 120 列，120×40，`vt_write` 到解析完成 |
| `terminal/parse_sgr` | 中位数 ≤ 25 ms | 1 MiB、每 8 个字符换一次 24 位色的输出 |
| `terminal/frame_full` | 中位数 ≤ 300 µs | 120×40 全部行脏，生成一帧 |
| `terminal/frame_echo` | 中位数 ≤ 30 µs | 只有一行脏（输入回显），生成一帧 |
| `terminal/echo_latency` | 中位数 ≤ 1 ms，p95 ≤ 3 ms | 真实 PTY：`Input::Text("x")` 到回显程序的输出出现在新帧里（假程序见「测试」）；只在 macOS 判定 |
| `terminal/idle_footprint` | ≤ 4 MB | 子进程：一个终端，120×40，输出 4 KiB 后生成一帧，常驻增量 |
| `terminal/scrollback_footprint` | ≤ 24 MB | 子进程：输出 10,000 行、每行 120 列带颜色，生成一帧，常驻增量 |
| `plugin/terminal/resident` | < 1 MB | 进程级，两次启动之差（[bench 设计「进程级基准」](bench.md#进程级基准)），没有打开的终端 |
| `plugin/terminal/activate` | kernel 的默认条目（< 1 ms） | 由进程级的 `plugin/*/activate` 测量（bench 第 5 步；之前 `plugin/review/activate` 在 `TestKernel` 里取四段耗时） |

滚动缓冲的上限：libghostty-vt 0.2.2 的 `max_scrollback` 实测是字节，不是行（「与设计的差异」第 1 条）。本 crate 设为 12 MiB，120 列带颜色的输出保留 10,000 行以上，`terminal/scrollback_footprint` 的份额不变。

## 状态与异常

| 情况 | 表现 |
|---|---|
| 会话不能恢复（`LaunchError` 的各种） | `open` 返回错误，不创建终端；调用方显示一行提示（`conversation` 的 Fluent 键） |
| 会话已有启动中或运行中的终端 | `LaunchError::AlreadyRunning(id)`；调用方切到那个终端 |
| 已有 8 个终端 | `LaunchError::TooMany`；调用方提示先关闭一个 |
| 工作目录已不存在 / 指向项目之外 | `RunState::Failed(CwdMissing / OutsideProject)`，帧为空白，视图显示原因 |
| `claude` / `codex` 不在登录环境的 `PATH` 上 | `Failed(NotInstalled(agent))`，视图提示安装 `AgentCommand::program` |
| 取不到登录环境（shell 超时、失败） | 退回 Lectern 自己的环境继续启动，warn 日志；程序找不到时同上 |
| 找不到 shell / PTY 打开失败 | `Failed(NoShell / Spawn)`，warn 日志带错误码 |
| 程序退出 | `Exited`，最后的画面和滚动缓冲保留，可以滚动、选择、复制；同一会话再次 `open` 时关闭它、开新的 |
| 程序不读输入、输入堆积 | 待写缓冲满 4 MiB 后丢弃新的输入，`dropped_inputs` 加一，warn 日志（每个终端只记第一次） |
| 程序忽略 `SIGHUP` | 关闭后 2 秒 `SIGKILL` 进程组 |
| 应用退出时程序仍在运行 | 插件停用时发 `SIGHUP`、关闭主端；忽略挂断的后台进程会留下（「已知限制」第 3 条） |
| 大量输出 | 每 8 ms 最多一帧，解析不落后于读；程序在 PTY 缓冲满时自然阻塞 |
| 读写主端出错 | warn 日志（`Value::io`），停止读写，状态等等待线程的结果 |
| 终端线程 panic | 本终端 `Failed(Internal)`、进程被结束，error 日志；插件和其他终端不受影响 |

## 日志与诊断

规则见 [ADR 0046](../adr/0046.md) 与 [logging 设计「隐私」](logging.md#隐私)。字段值的写法集中在 `src/diag.rs`；`LaunchKind`、`AgentCommand`、`LaunchError`、`FailReason` 和程序种类各自实现 `Field`（写成固定的小写名）。

- 默认记录：
  - info `terminal started`：`term`（`TerminalId`）、`launch`（`resume` / `new_session` / `shell`）、`program`（不含目录的程序名，取自固定表：agent 启动为 `claude` / `codex`；`Shell` 启动按 shell 的文件名记 `zsh`、`bash`、`fish`、`ksh`、`sh`、`dash`，表外的记 `other`）、`argc`（argv 的个数）、`cwd`（`Value::path`）、`pid`、`cols`、`rows`、`login_env`（`captured` / `fallback` / `not_needed`）。这就是 logging「隐私」对子进程要求的程序名（不含目录）、参数个数、工作目录、pid；程序的完整路径（来自 `$SHELL` 或 `PATH` 的值）不记。
  - info `terminal exited`：`term`、`pid`、`code`、`signaled`、`ran_ms`。
  - info `terminal closed`：`term`、`reason`（`user` / `project_closed` / `deactivated` / `replaced`）、`killed`（是否用了 `SIGKILL`）。
  - warn `terminal launch refused`：`reason`（`LaunchError`，`AlreadyRunning` 不记）、`launch`。
  - warn `terminal start failed`：`term`、`reason`（`FailReason`）、`error`（`Value::io`，只在 `Spawn`）。
  - warn `login environment unavailable`：`term`；原因由 `platform` 自己记。
  - warn `pty read failed` / `pty write failed`：`term`、`error`（`Value::io`）。
  - warn `terminal input dropped`：`term`、`bytes`（这次丢弃的字节数）；每个终端只记第一次。
  - error `terminal thread panicked`：`term`、`thread`（`engine` / `wait`）。
- 详细级别：
  - debug `terminal throughput`：每个终端每秒最多一条，`term`、`read_bytes`、`frames`、`max_frame_us`、`dirty_rows`。
  - debug `terminal resized`：`term`、`cols`、`rows`。
  - debug `terminal compressed`：`term`、`steps`。
  - trace `terminal input`：`term`、`kind`（输入的变体名）、`bytes`（编码后的字节数）。
- 关联 ID：终端线程和等待线程上的记录由 executor 带上 `plugin`、`act`、`task`；恢复的终端在线程开始时用 `scope` 设 `session`（会话 UUID 的字节，与 `agents`、`review` 相同）。会话 ID 只作为关联 ID 出现，不作为参数记录。
- 不记录：终端输出的任何字节、标题、OSC 7 的目录、选区与复制的文字、粘贴与输入的文字（包括按键产生的字符）、argv 的内容（程序名之后的参数）、环境变量的名字和值（包括 `$SHELL`、`PATH`，以及由它们得到的程序路径）、`Theme` 的颜色值。只记长度、计数和种类。
- 金丝雀测试 `logs_hold_no_terminal_content`：`testing::capture()` 在 trace 级别下，假程序输出含金丝雀的文字、OSC 0 标题、OSC 7 目录；测试粘贴、输入、复制含金丝雀的文字；登录环境里有一个值为金丝雀的变量，`PATH` 和 `with_shell` 的路径里也有金丝雀；然后 `assert_absent`。

## 测试

- 用假程序，不用真实的 `claude`、`codex`，不依赖网络和登录状态。假程序是本 crate 的一个 bin 目标 `lectern-terminal-fake`（`required-features = ["test-support"]`），集成测试从 `CARGO_BIN_EXE_lectern-terminal-fake` 取路径。它有三种用法：
  - 作为 agent：测试建一个临时目录，放名为 `claude`、`codex` 的符号链接指向它，用 `with_login_env` 给出以这个目录开头的 `PATH`；它把收到的 argv、工作目录和环境变量的名字写进测试给的记录文件，再按夹具回放输出。用来逐项核对启动命令表和环境。
  - 作为 shell（`with_shell(fake)`）：核对 `Shell` 启动的 argv[0] 与环境。
  - 回显模式：把输入原样写回输出（`terminal/echo_latency` 和输入测试）。
- 测试都用 `with_login_env`，不运行用户的 shell；登录环境本身的获取在 `platform` 测试。
- 夹具是手写的字节序列（`tests/fixtures/*.vt`），不取自真实会话或真实程序的输出：ANSI 16 色、256 色、24 位色、粗体斜体下划线（含波浪线与颜色）、反显、CJK 宽字符、组合字符、ZWJ 表情、制表符、软换行、备用屏幕、光标移动与形状、清屏、OSC 0 / 2 / 7 / 10 / 11、mode 2026。
- 帧的 insta 快照（`frame_*`）：每份夹具一份，写成每行的文字、列位置与样式段，逐个看过。
- 帧的复用（`row_reuse_*`）：只改一行时其余行的 `Arc` 与上一帧相同（`Arc::ptr_eq`），`key` 不变；选区变化不改变任何行的 `key`。
- 输入编码（`input_*`）：方向键在普通模式与 DECCKM 下的序列；kitty 键盘协议开启后的编码；鼠标跟踪开启时编码、按 Shift 时改为选区；备用屏幕下滚轮换成方向键；焦点事件只在 mode 1004 开启时写出；进程退出后滚动、选区、全选照常，按键与粘贴被丢弃。
- 粘贴（`paste_*`）：开了 bracketed paste 时包上首尾序列；没开时 `\n` 换成 `\r`；C0（`\t`、`\n` 除外）、ESC、C1 都被去掉，含 `ESC[201~` 的文字不能提前结束粘贴；末尾不追加回车。
- 复制（`copy_*`）：单行、跨软换行（不插入换行）、跨硬换行、含宽字符的选区的纯文本；没有选区时 `reply` 收到 None；调用方的 token 已失效时 `reply` 不被调用。
- 颜色（`theme_*`）：OSC 10 / 11 / 4 的查询应答取 `Theme`；mode 2031 的明暗报告；`SetTheme` 之后全部行的颜色更新。
- 帧的节奏（`pacing_*`）：持续输出时相邻帧间隔 ≥ 8 ms；空闲后的第一次输出立即发布；mode 2026 期间不发布、200 ms 后发布。
- 启动（`launch_*`）：启动命令表的每一行（argv 与程序路径）；环境里有 `TERM`、`COLORTERM`、`TERM_PROGRAM`，没有 `LECTERN_` 开头的变量；`PATH` 里没有 `claude` 时 `Failed(NotInstalled(ClaudeCode))`，假程序没有被启动。
- 安全（`resume_refuses_*`）：非 UUID、大写 UUID、带空格的 ID、ID 与 `key.id` 不同、变体与来源不一致、来源 ID 为 `ext.x.y`、来源不在集合里、`resume` 为 None、`Place` 不是 `Located`、`cwd` 是指向项目之外的符号链接（第 6 条），各返回对应的错误或 `Failed`，假程序都没有被启动（记录文件不存在）；`no_echo_of_program_text`：程序请求标题报告等查询后，输入端没有出现程序可控的文字。
- 生命周期：`no_threads_until_open`（激活后没有 `terminal-*` 线程，也没有取登录环境）；`resume_refuses_while_running`（`AlreadyRunning` 带已有的 ID）；`exited_terminal_is_replaced`、`failed_terminal_is_replaced`；`close_*`（`SIGHUP` 后退出；忽略 `SIGHUP` 的假程序 2 秒后被 `SIGKILL`；两个线程都结束）；`project_closed_closes_its_terminals`；`deactivate_closes_all`；`exit_codes_*`（正常退出、非零退出、被信号结束）；`input_overflow_*`；`panic_fails_only_that_terminal`（测试钩子让一个终端线程 panic：它变为 `Failed(Internal)`，另一个终端照常运行，插件仍是 `Active`）；`quiescent_after_output_stops`（输出停止后 kernel 报告静止）；`concurrent_spawn`（8 个终端在其他线程大量分配内存时同时启动，都能运行）。
- PTY 与真实进程的测试在 Linux 与 macOS 都跑（本 crate 不依赖 GPUI）；`terminal/echo_latency` 只在 macOS 判定。

## 对其他 crate 的要求

1. `services`：新增 `terminal` 模块，内容是「接口」的服务、事件、输入、帧几节（本文的第 1 步）。只用标准库和 serde，不加依赖。
2. `platform`：新增 `login_env` 模块（[conversation 设计](https://github.com/SoloJiang/lectern-app/pull/262)「对其他 crate 的要求」C 说的、与 `agents` 共用的登录 shell 环境）。它不依赖 GPUI，非插件，`agents` 和本 crate 都能依赖；在 platform 的设计文档里加一节（`Refs #12`），本文第 3 步之前实现：

   ```rust
   // lectern_platform::login_env
   /// 用户的登录 shell：`$SHELL`（绝对路径、可执行）；否则用户数据库（`getpwuid_r`）里的登录 shell；
   /// 否则 `/bin/zsh`；都不可用时 None。读环境变量、用户数据库和 stat，阻塞很短；不在 UI 线程上调用。
   pub fn user_shell() -> Option<PathBuf>;

   /// 登录 shell 设置之后的环境变量。第一次调用时运行一次 `<user_shell> -l -i -c <打印环境的命令>`
   /// （stdin 为 /dev/null，没有控制终端，自己的进程组），用标记把环境和配置文件的其他输出分开，
   /// 值里的换行不被误拆；最多等 5 秒，超时向进程组发 SIGKILL。结果（成功或失败）在进程内缓存，
   /// 之后的调用直接返回；并发的第一次调用只运行一次 shell。阻塞，只在 executor 或专属线程上调用，
   /// 首帧之前不调用。
   pub fn login_env() -> Result<Arc<LoginEnv>, LoginEnvError>;

   pub struct LoginEnv { pub vars: Box<[(OsString, OsString)]> }
   impl LoginEnv {
       pub fn get(&self, name: &str) -> Option<&OsStr>;
   }

   /// 在 `path`（冒号分隔）的各个目录里找名为 `name`（不含 `/`）的可执行普通文件，返回第一个的绝对路径。
   /// 阻塞（stat）。
   pub fn find_program(name: &str, path: &OsStr) -> Option<PathBuf>;

   #[derive(Clone, Copy, PartialEq, Eq, Debug)]
   pub enum LoginEnvError { NoShell, Spawn, Timeout, Unparsable }
   ```

   日志：失败记 warn（`LoginEnvError` 的种类、`Value::io`），成功记 debug（变量个数、耗时）；不记 shell 的路径、变量的名字和值、shell 的输出。测试用假 shell，不运行用户的 shell。

   [agents 设计「未决问题」](agents.md#未决问题)1 的 (a) 说以后「经监护进程查询一次登录 shell 环境」来找只在 shell 里设置的会话目录；有了这个模块，会话发现是否直接用它，由 `agents` 的设计决定。
3. 工具链与 CI（`ci.md`、`justfile`、根 AGENTS.md「命令」、`cargo-workspace.md`）：
   - Linux 与 macOS 的 CI 任务在构建之前安装 Zig 0.15.2（官方发布包，校验 SHA-256），`git` 已有。`OUT_DIR` 在 `target/` 里，随现有的 `Swatinem/rust-cache` 缓存；缓存未命中时多出克隆 Ghostty 和 Zig 构建的时间（未测，预计 1–3 分钟），实现 PR 把实测写进 ci 设计的「实际验证」，并核对 Linux 任务约 2 分钟、macOS 任务 15 分钟的目标是否仍满足。
   - `justfile`：`check`、`test`、`bench` 之前检查 `zig version` 是 0.15.2，不是时报错并给出安装方式。
   - 根 AGENTS.md「命令」一节的所需工具加上 Zig 0.15.2。
   - `cargo-workspace.md` 里 libghostty-vt 那一行改为本文的两个版本号与 Ghostty 提交。
   - `just deny` 通过；Ghostty 编进静态库的 C / C++ 依赖的许可证由实现 PR 人工核对（「依赖」）。
4. `conversation`（[conversation 设计](https://github.com/SoloJiang/lectern-app/pull/262)「对其他 crate 的要求」D）。那一节写明接口的名字以本文为准，对应关系如下：
   - `resume(session, size)` 是 `open(OpenSpec { launch: Launch::Resume(key), size: Some(size), theme })`：多了 `theme`（第一帧就要有颜色，由 `terminal_view` 给出）。`AlreadyRunning`、替换已退出或失败的终端、返回前已登记，与 D 相同。
   - `ResumeRefused::NotResumable` / `OutsideProject` / `AlreadyRunning` 对应 `LaunchError` 的同名变体（`LaunchError` 另有 `UnknownSession`、`UntrustedSource`、`InvalidSessionId`、`TooMany`，界面可以都显示为「不能恢复」）。`NotInstalled` 不在 `open` 里同步返回：查 `PATH` 是 IO，不在调用线程上做，它随 `Listed` 以 `RunState::Failed(NotInstalled(agent))` 送达，界面用 `AgentCommand::program` 显示程序名。
   - `status(id)` 是 `info(id)` 的 `state`；「是否在驱动会话」按 D 的规则看 `Starting` / `Running`。
   - `paste(id, text)` 是 `input(id, Input::Paste(text))`，去掉控制字符、bracketed paste、不追加回车的规则与 D 相同。`Starting` 期间的粘贴被丢弃（`input`），调用方等到 `Running` 再发。
   - `TerminalEvent::Output { id }` 是 `TerminalEvent::Frame { id, .. }`（只在有 `FrameInterest` 时发出）；`Status { id }` 是 `Listed`（读 `list()` 或 `info(id)`）。另有 `Bell`。
   - 服务 ID 是 "terminal"（与插件 ID 相同，同 "review"），不是 D 里写的 "terminals"。
   - Terminal 页的插件在 `requires` 里依赖 `dyn Terminals`，与不依赖它的部分分成两个插件；页可见时持有 `FrameInterest`，隐藏时 drop；关闭按钮调用 `close`。
   - 运行中的终端在离开时是否确认，按「未决问题」1 注册离开守卫（`LeaveScope::Project`、`Worktree`、`App`）。
   - `LaunchError`、`FailReason` 的提示文字（Fluent 键，en 与 zh-CN）。
5. `terminal_view`：非插件的视图库。把 GPUI 的按键、IME 文字、粘贴、鼠标、滚动、焦点、尺寸转成 `Input`；从 `ui` 的 `Palette` 推出 `Theme`，外观变化时发 `SetTheme`；按 `Frame` 绘制；拥有 `app/input_frames`（另写设计文档，按 conversation 设计「对其他 crate 的要求」E）。
6. `app`：装配 `TerminalPlugin::new(TerminalDeps { executor })`，排在 `project` 与 `agents` 之后（第 4 步）；`app/bundle_size` 记录静态库的增量。
7. `bench`：预算表加入「性能」里的条目。`app/input_frames` 在 Conversation 的 Terminal 页里恢复夹具会话（`Launch::Resume`），所以它的 profile 用 `with_login_env` 给出一个 `PATH`，其中的 `claude`、`codex` 是回显程序（本 crate 的假程序，或 bench 自带的同类程序，由 bench 的实现决定）；不运行用户的 shell 和它的配置，也不运行真实的 agent。
8. `conversation` 的审阅意见会话途径（#22 发回会话，实现 `review` 的 `NoteRoute`；`review_view` 不直接调用本 crate）：驱动会话的终端是 `Running` 时经 `Input::Paste` 写入，不追加回车，用户在终端里按 ↵ 提交（conversation 设计「对其他 crate 的要求」I）。终端还是 `Starting` 时不发送：这期间的输入和粘贴被丢弃（`input`），所以途径答复「现在送不了（终端正在启动）」，审阅屏幕提示用户稍后再发或改用剪贴板（[review 设计「审阅意见的发送途径」](review.md#审阅意见的发送途径noteroute)）。
9. `project_home`：空状态的 New session 调用 `Launch::NewSession`（本 issue 之外）。

## 未决问题

没有需要产品决定的问题。下面三条已于 2026-10-25 决定，人确认按默认选项：

1. **还有运行中的终端时关闭项目或退出应用，是否先确认。** 已决定：选 (a)，确认，提示里写出运行中的终端数，选「关闭」后才继续。确认由 `conversation` 的离开守卫实现，本 crate 提供 `list()` 里的状态。不选：(b) 不确认，直接挂断。
2. **是否允许程序经 OSC 52 写剪贴板。** 已决定：选 (a)，不允许，理由见「安全边界」。一些程序（tmux、Neovim 的远程复制）用它复制文字。不选：(b) 每个终端第一次写时询问；(c) 允许。
3. **离开会话的 Conversation 页时终端是否继续运行。** 已决定：选 (a)，继续运行，回到页面时接着显示（agent 可能还在工作）。本 crate 按此设计：只在 `close`、项目关闭、插件停用时结束进程。不选：(b) 离开即关闭。

## 已知限制

1. 本插件依赖 `dyn SessionSource`：两个内置会话来源都不可用（被用户停用）时，终端插件不激活，登录 shell 也打不开。kernel 没有可选依赖；要解除，需把登录 shell 拆成不依赖会话来源的第二个插件，等有这个需求时再做。
2. 登录环境在一次运行里只取一次：用户改了 `.zshrc`（如新装了 `claude`）之后，要重启 Lectern 才生效。
3. 应用退出时忽略 `SIGHUP`、又不在前台进程组的后台进程会留下，与关闭 Terminal.app 窗口的行为相同。
4. libghostty-vt 内部（Zig 代码）的 panic 会结束整个 Lectern 进程，不能按终端隔离；发布构建（ReleaseFast）里 Zig 的运行时安全检查关闭，出现的可能很小。
5. 闪烁文字按普通文字显示；kitty 图形协议、Sixel 不支持。
6. 视图在主线程上画帧不计入静止（「数据流与线程」）：UI 探针在静止之后读语义树时，终端的最后一帧可能还没画出，探针要另等一帧。
7. Zig 0.15.2 不能链接 macOS 27 的 SDK（它的 `libSystem.tbd` 只列 `arm64e`），本机用 macOS 27 的 Xcode 或命令行工具时构建失败。临时办法：`DEVELOPER_DIR` 指向带旧 SDK 的命令行工具（如 SDK 14.4 的 `/Library/Developer/CommandLineTools`）再运行 Zig。CI 的 `macos-15` runner 不受影响；判定机器升级到 macOS 27 之前要先确认。
8. 进程里同一时刻有别的线程在 `fork`、或在第一次使用 libSystem 的通知库时，子进程可能在 `exec` 之前就死掉。macOS 的 `fork` 在子进程里运行 libSystem 的 atfork 处理（`_notify_fork_child`），它用 `os_once` 初始化通知库的全局状态；父进程里另一个线程当时正持有同一个 once 门（第一次初始化还没做完）时，子进程发现门被一个不存在的线程占着，报「os_once_t is corrupt」并以 `SIGKILL` 结束（崩溃报告写「crashed on child side of fork pre-exec」，调用栈 `fork` → `libSystem_atfork_child` → `_notify_fork_child` → `_os_alloc_once` → `_os_once_gate_corruption_abort`）。表现是刚启动的程序在写出第一个字节之前被杀，状态 `Exited { signaled: true }`。只在进程里最初的几次 `fork` 同时进行时出现；通知库初始化之后（应用启动时 AppKit / GPUI 早已用过它）不会再出现。本 crate 的处理：所有启动经 `pty::spawn` 里一把进程内的锁，同一时刻只有一个线程 `fork`；回归测试 `concurrent_first_forks_from_several_plugins_start_every_program`（8 个插件实例、48 个线程在新进程里同时 `open`）不加锁时约 13–20% 的运行失败，加锁后 30 次都通过。仍然留下的：本 crate 之外的线程同时 `fork`（`platform::login_env` 取登录环境的 shell、其他插件启动的进程），或同时第一次初始化通知库，仍可能碰上这个窗口；彻底的办法是把这把锁放进 `platform`，所有 `fork` 的调用方共用，另开 issue。

## 验收

**第 1 步：`services::terminal`（`services` 的 PR，`Refs #12`）。**

- `crates/services/src/terminal/` 有「接口」里服务、事件、输入、帧的全部类型，`lib.rs` 的模块说明加上这一项。
- `just test -p lectern-services` 里 `terminal_events_round_trip`（`TerminalEvent` 的每个变体经 serde 往返）、`frame_interest_releases_once` 通过。
- `cargo tree -p lectern-services --edges normal` 没有新增依赖。

**第 2 步：libghostty-vt 接入与网格快照（本 crate 的 PR，`Refs #5`）。**

- 工作区依赖加上 `libghostty-vt = { version = "=0.2.2", default-features = false }` 与 `libghostty-vt-sys = "=0.2.2"`，本 crate 两个都直接依赖；`cargo tree -p lectern-terminal -e features` 里没有 `kitty-graphics`、`log`、`tracing`、`link-dynamic`；`cargo tree -i libghostty-vt-sys` 只有 0.2.2。
- `rg 'set_logger' crates/terminal/src` 没有结果。
- 「对其他 crate 的要求」第 3 项的 CI、`justfile`、AGENTS.md 与 `cargo-workspace.md` 改动在同一个 PR 里；Linux 与 macOS 的 CI 任务通过；`just deny` 通过。
- 不经 PTY 的引擎（`test-support` 的 `testing::Engine`：喂字节、发输入、取帧）完成；`just test -p lectern-terminal` 里 `frame_*`（快照逐个看过）、`row_reuse_*`、`input_*`（不含进程退出的部分）、`paste_*`、`theme_*`、`pacing_*`、`copy_*` 通过。
- `just bench terminal/` 里 `terminal/parse_plain`、`terminal/parse_sgr`、`terminal/frame_full`、`terminal/frame_echo`、`terminal/idle_footprint`、`terminal/scrollback_footprint` 在预算内。

**第 3 步：PTY、启动、插件与服务（本 crate 的 PR，`Refs #12`；前置：第 2 项 `platform::login_env`）。**

- `Cargo.toml` 加 `plugin = true`，`python3 scripts/check_architecture.py` 通过；`portable-pty = "=0.9.0"` 与 rustix 的 `event`、`process` feature 加入工作区依赖，`just deny` 通过。
- `just test -p lectern-terminal` 里 `launch_*`、`resume_refuses_*`、`no_echo_of_program_text`、`no_threads_until_open`、`resume_refuses_while_running`、`exited_terminal_is_replaced`、`failed_terminal_is_replaced`、`close_*`、`project_closed_closes_its_terminals`、`deactivate_closes_all`、`exit_codes_*`、`input_*`（进程退出的部分）、`input_overflow_*`、`panic_fails_only_that_terminal`、`quiescent_after_output_stops`、`concurrent_spawn`、`logs_hold_no_terminal_content` 通过。
- `rg 'try_clone_reader|take_writer|\.kill\(\)' crates/terminal/src` 只有 `ChildKiller` 的 `kill`；`rg 'Command::new|"sh"|"-c"' crates/terminal/src --glob '!**/tests*' --glob '!**/testing*'` 没有结果（agent 不经 shell 启动）。
- `just bench terminal/` 里 `terminal/open`、`terminal/echo_latency`（macOS）、`plugin/terminal/activate` 在预算内，`terminal/input` 有记录（[ADR 0058](../adr/0058.md)）。

**第 4 步：`app` 装配（`app` 的 PR，`Refs #12`）。**

- `app` 的 profile 里有 "terminal"，`app/idle_empty` 的 `Plugins` 应答列出它。
- `plugin/terminal/resident` < 1 MB；`app/idle_empty`、`app/cold_start` 在预算内；`app/bundle_size` ≤ 80 MB，增量写进「实际验证」。
- 启动后、没有打开终端时，`ps -M <pid>` 里没有 `terminal-` 开头的线程。

## 实际验证

第 2 步（libghostty-vt 接入与网格快照，`Refs #5`），本机 Apple M5 Pro、macOS 27.0.1：

- 构建：Zig 0.15.2（官方发布包，SHA-256 与 `scripts/install_zig.sh` 里的一致）编出 Ghostty 提交 `a887df42c56f6de86c0fe6da9c4eeca37931e083` 的 `libghostty-vt.a`；那个提交的 `build.zig.zon` 写的 `minimum_zig_version` 是 `0.15.2`。macOS 27 的 SDK 上链接失败，见「已知限制」第 7 条。
- 依赖：`cargo tree -p lectern-terminal -e features` 里没有 `kitty-graphics`、`log`、`tracing`、`link-dynamic`；`cargo tree -i libghostty-vt-sys` 只有 0.2.2；`rg 'set_logger' crates/terminal/src` 没有结果；`just deny` 通过。
- 编进静态库的 C / C++ 代码（`ar t libghostty-vt.a` 列出的目标文件）：Ghostty 自己的 Zig 与 C++ SIMD 代码（MIT），simdutf（MIT OR Apache-2.0，Ghostty 的 `pkg/simdutf/vendor` 里的单文件版，不需要 libc++），Highway（Apache-2.0 OR BSD-3-Clause，`deps.files.ghostty.org` 上按哈希固定的源码包），Zig 的 `compiler_rt`（MIT）。都在 `deny.toml` 的许可证白名单里。`build.zig.zon` 里的其他包（FreeType、HarfBuzz、Oniguruma 等）是 `lazy`，libghostty-vt 的目标不取。
- 测试：`cargo test -p lectern-terminal --all-features` 里单元测试 5 个（`palette_*`、`pacing_*`）、`tests/frames.rs` 32 个（`frame_*` 15 个快照逐个看过，`row_reuse_*`、`copy_*`、`theme_*`）、`tests/input.rs` 16 个（`input_*`、`paste_*`、`no_echo_of_program_text_in_replies`）通过。
- 基准（release，`cargo bench -p lectern-bench --bench time --bench memory -- terminal/`）：

  | 基准 | 实测中位数 | 预算 |
  |---|---|---|
  | `terminal/parse_plain` | 1.06 ms | ≤ 10 ms（当时的预算；现在是 14 ms，ADR 0055） |
  | `terminal/parse_sgr` | 5.33 ms | ≤ 25 ms |
  | `terminal/frame_full` | 133 µs | ≤ 300 µs |
  | `terminal/frame_echo` | 2.38 µs | ≤ 30 µs |
  | `terminal/idle_footprint` | 672 KB | ≤ 4 MB |
  | `terminal/scrollback_footprint` | 10.7 MB（写入 10,000 行，保留 10,001 行） | ≤ 24 MB |

- 没有验证的：CI 上缓存未命中时多出的构建时间见 [ci 设计「实际验证」](ci.md#实际验证)。

第 3 步（PTY、启动、插件与服务，`Refs #12`），同一台机器：

- 依赖与架构：`python3 scripts/check_architecture.py` 通过（`plugin = true`）；`portable-pty = "=0.9.0"` 与 rustix 的 `event`、`process` feature 在工作区依赖里；`just deny` 通过（新增 `nix` 0.28、`filedescriptor`、`serial2`、`shell-words`、`downcast-rs`、`shared_library`、`lazy_static` 等传递依赖，都是宽松许可）。`rg 'try_clone_reader|take_writer|\.kill\(\)' crates/terminal/src` 只有 `src/pty.rs` 里 `ChildKiller` 的那一处；`rg 'Command::new|"sh"|"-c"' crates/terminal/src --glob '!**/tests*' --glob '!**/testing*'` 没有结果；`rg 'set_logger' crates/terminal/src` 没有结果。
- 测试：`just test -p lectern-terminal` 110 个测试，连续 10 次都通过（nextest，一个测试一个进程；单元测试 `diag`、`launch`、`pacing`、`palette`，`tests/frames.rs`、`tests/input.rs` 是第 2 步的；第 3 步新增 `tests/launch.rs` 25 个（`launch_*`、`resume_refuses_*`、`no_echo_of_program_text`）、`tests/lifecycle.rs` 16 个、`tests/pty_input.rs` 7 个（`input_*` 经真实 PTY）、`tests/logs.rs` 1 个（`logs_hold_no_terminal_content`））。`just check`、`just test -p lectern-terminal -p lectern-bench -p lectern-services` 通过。程序是假程序 `lectern-terminal-fake`，没有运行真实的 `claude`、`codex` 或用户的 shell。把 `tests/lifecycle.rs` 的测试放在同一个进程里并行跑（直接运行 libtest 的测试二进制）时，加锁之前约三分之一的运行里有刚启动的程序被 `SIGKILL`，原因和处理见「已知限制」第 8 条；加锁之后连续 30 次运行没有一次出现，崩溃报告也没有新增。
- 基准（release，`cargo bench -p lectern-bench --bench time -- terminal/`）：

  | 基准 | 实测中位数 | 预算 |
  |---|---|---|
  | `terminal/open` | 55–69 µs（6 次运行；CI 上 77–108 µs） | ≤ 150 µs（原来 ≤ 100 µs，见 ADR 0058） |
  | `terminal/input` | 3.6–3.9 µs（另有两次安静时 1.5、1.7 µs） | 只记录，目标 ≤ 6 µs（原来 ≤ 2 µs，见 ADR 0058；CI 上记录到 8.29–21.2 µs） |
  | `terminal/echo_latency` | 中位数 81–158 µs，p95 156–247 µs（另有一次 p95 1.24 ms） | 中位数 ≤ 1 ms，p95 ≤ 3 ms |

  `terminal/input` 的 3.6–3.9 µs 是调用线程在终端线程睡在 `poll` 上时写唤醒字节的代价，不是本 crate 的其他工作：同机器上，一个线程睡在 `poll` 上时向 socketpair 写一个字节，中位数 3.1–3.8 µs，向 pipe 写 2.3–2.8 µs，用 `kqueue` 的 `EVFILT_USER` 触发 1.9–2.0 µs；没有线程在等时同一次写只要 0.33 µs。入队、查表和 `Arc` 克隆合起来不到 1 µs。所以设计原来的 2 µs 在「终端线程睡着」时用 socket 和 `poll`（设计的做法）做不到。这个 PR 引入了被测的代码，所以不是 [ADR 0055](../adr/0055.md) 的放宽，而是 [ADR 0058](../adr/0058.md) 改为只记录，目标 ≤ 6 µs，仍用 `poll`。终端代码相同的两个提交在 CI 上分别是 9.67、8.29 µs 和 21.2、18 µs，托管 runner 不能判定这个数（托管 runner 是虚拟机，唤醒可能更慢）。
- `plugin/terminal/activate`：进程级基准要等第 4 步。`TestKernel` 里 kernel 记下的四段耗时（`manifest`、`activate`、提交、`started`）之和，30 次激活的中位数 3.7 µs，最大 47 µs，≪ 1 ms。
- 没有验证的：`plugin/terminal/resident`、`app/bundle_size` 的增量、「启动后没有 `terminal-` 线程」（`ps -M`）都要等第 4 步；CI 的 Linux 任务没有在本机跑（`portable-pty`、`rustix` 的 `event` / `process` 和假程序都不含 macOS 专属调用，但没有验证）；`terminal/echo_latency` 在 Linux 上只做冒烟；`input_overflow_*` 里 PTY 输入队列的大小因系统而异，测试只断言「发出的输入多于 4 MiB 缓冲时有输入被丢弃」。


第 4 步（`app` 装配，`Refs #12`），同一台机器，没有打开真实窗口：

- 装配：`profile::desktop` 的最后（`worktrees-usage` 之后）放进 `TerminalPlugin::new(TerminalDeps { executor })`，desktop 与 probe profile 共 15 个插件（[app 设计「terminal 的装配」](app.md#terminal-的装配)）。`cargo tree --locked -p lectern-app -e features --target aarch64-apple-darwin` 里 `lectern-terminal` 只有 `default`，没有 `test-support`。
- 检查与测试：`just check`（退出码 0）、`just deny`（`advisories ok, bans ok, licenses ok, sources ok`）、`python3 scripts/check_architecture.py`（32 个 crate）、`cargo metadata --format-version 1 --locked --all-features` 通过。`just test -p lectern-terminal -p lectern-bench`：292 个测试通过（6 个因本机默认跳过）；`just test -p lectern-app`：56 个测试通过，`every_profile_graph_validates` 的 desktop 与 probe 插件数为 15，图校验通过。`logs_hold_no_terminal_content` 改为等到三条 `terminal closed` 之后才扫描金丝雀，连续 10 次都通过。
- 安装包：`cargo build --release -p lectern-app --bin lectern` 的 `lectern` 是 43 364 544 字节；在同一棵树里临时去掉 `terminal` 的装配（已还原）是 41 619 216 字节，所以多 1 745 328 字节（1.66 MiB，4.2%）。静态库 `libghostty-vt.a`、`portable-pty`、`rustix` 的 `event` / `process` 和本 crate 都在里面，库在链接时只留下被引用的目标文件。整个 `.app` 没有测（`app/bundle_size` 由安装包的任务判定，#38），ADR 0018 的 80 MB 预算远未触及。发布构建的 `lectern` 没有 `lectern-terminal-fake`（它在 `test-support` 下）。
- 没有验证的：`plugin/terminal/activate`、`plugin/terminal/resident`（新增的预算条目，< 1 MB）、`app/idle_empty`、`app/cold_start` 都要启动真实窗口的测量构建，本机不运行，以 macOS CI 为准；启动后 `ps -M <pid>` 里没有 `terminal-` 线程只有 `no_threads_until_open`（本 crate 的测试）和 `tests/startup.rs` 的「首帧之前没有 target 为 `terminal` 的记录」（需要图形会话，没有在本机运行）间接保证，没有对着真实进程查过；`tests/startup.rs`、`tests/bench_probe.rs`（`Plugins` 应答改为 15 个）、`tests/probe.rs` 没有运行。

## 与设计的差异

第 2 步：

1. 滚动缓冲的上限是字节：libghostty-vt 0.2.2 的 `max_scrollback` 按字节计（120 列一行约 1 KiB：10 MiB 保留 9,469 行）。按「性能」那一段换算为 12 MiB，`terminal/scrollback_footprint` 实测保留全部 10,000 行。
2. 反显由本 crate 换算：libghostty-vt 的单元格颜色不含 SGR 7，`grid` 交换前景与背景色，帧里的颜色仍是「已解析」的。
3. 默认打开 mode 2027（字素簇）：不打开时 ZWJ 表情被拆成几个单元格，`frame_emoji` 的快照每个表情一个簇。
4. 去掉标题与图标的报告：libghostty-vt 无条件应答 `CSI 21 t` / `CSI 20 t`，应答里是程序写过的标题（`ESC ] l <标题> ESC \`），等于把程序的文字回显给它，违反「安全边界」。它没有开关，所以写回 PTY 之前丢掉 `ESC ] l`、`ESC ] L` 开头的应答（`no_echo_of_program_text_in_replies`）。
5. `ClearScrollback` 经 `vt_write("\x1b[3J")`（ED 3）实现，libghostty-vt 0.2.2 没有单独的接口。
6. 工具链：`scripts/install_zig.sh` 安装并校验 Zig，CI 的三类任务调用它；`justfile` 的版本检查也在 `doctest` 之前（它同样编译本 crate）。

第 3 步：

1. agent 的 argv[0] 是登录环境的 `PATH` 里找到的完整路径，不是 `claude` / `codex`：portable-pty 的 `CommandBuilder` 用同一个 `args[0]` 既查找程序、又作 argv[0]，而它自己的查找会把 `PATH` 里的相对项接在工作目录后面（项目里的 `bin/claude` 会被执行）。所以先用 `platform::login_env::find_program`（只认绝对路径的项）找到程序，再把绝对路径交给它。参数仍是「启动命令」表里的 `--resume <uuid>` / `resume <uuid>` / 无。`Shell` 用 `CommandBuilder::new_default_prog()`，它按 `SHELL` 环境变量（本 crate 设为选定的 shell）启动，argv[0] 是 `-<文件名>`。
2. agent 环境里的 `SHELL` 不是可执行文件时去掉它：portable-pty 会把这样的值写进 `log` 门面（第三方记录，带路径），子进程改用用户数据库里的 shell。
3. `LaunchKind`、`AgentCommand`、`LaunchError`、`FailReason` 不实现 `Field`：它们在 `services`，那里不依赖 `logging`（孤儿规则也不允许在本 crate 实现），所以 `src/diag.rs` 里写成返回固定小写名的函数。
4. `with_shell`、`with_login_env` 只在测试和 `test-support` 下存在（设计的接口没有写条件编译）；另有只给测试用的 `with_thread_hook`（每轮开始时在终端线程上调用，`panic_fails_only_that_terminal` 用它）、`provided`、`token`、`threads`（正在运行的终端线程数）。`testing::Harness`、`FakeProjects`、`FakeSessions` 供测试和基准把插件放进 `TestKernel`。
5. 假程序用到 `libc`（设置原始模式、`SIGHUP` 忽略、`ioctl`）：加为可选依赖，只在 `test-support` 下编译；设计的依赖表只有 rustix。
6. 状态变化（`Starting` → `Running` / `Failed` → `Exited`）时终端线程总是发布一帧，没有关注者也一样，所以 `frame(id)` 在终端登记之后很快就有值，`Frame::state` 与 `info().state` 一致；`Frame` 事件仍只在有关注者时发出。
7. `Terminals::open` 在两种设计没有写的情况下返回错误：插件已停用或线程提交被拒绝时 `UnknownSession`（消费方经 `ServiceRef` 取不到已停用的提供者，所以实际不会遇到）；唤醒 socket 建不起来（文件描述符用完）时 `TooMany`。
8. `terminal/echo_latency` 用 `/bin/cat` 作 shell，不用假程序：`lectern-bench` 里没有 `CARGO_BIN_EXE_lectern-terminal-fake`。`cat` 在行模式下读终端，回显是内核行规程的，本 crate 这一侧的路径（唤醒、编码写、读、解析、建帧）都在里面。
9. `plugin/terminal/activate` 在这个 PR 里没有基准条目：它由进程级的 `plugin/*/activate` 测量，要等第 4 步 `app` 装配。数字见「实际验证」（`TestKernel` 里 kernel 记下的四段耗时）。
10. 终端表和唤醒 socket 的细节：`Slot` 是命令队列加唤醒 socket 的一对，只在队列由空变非空时写唤醒字节（`woken` 标志），所以连续的 `input` 只写一次。
11. `terminal/input` 的预算由中位数 ≤ 2 µs 改为只记录，目标 ≤ 6 µs（[ADR 0058](../adr/0058.md)）：本机实测 3.6–3.9 µs，CI 上 9.67、8.29 µs，原因是唤醒一个睡在 `poll` 上的线程的系统调用本身要 3 µs 以上，对照数字见「实际验证」。保持 `poll` 循环；`kqueue` 只到约 1.9 µs，且要改「数据流与线程」。终端代码相同的两个提交在 CI 上分别是 9.67、8.29 µs 和 21.2、18 µs，托管 runner 不能判定这个数。
12. 所有启动经一把进程内的锁（`src/pty.rs` 的 `SPAWN`），同一时刻只有一个线程 `fork`，设计没有写：原因见「已知限制」第 8 条。

第 4 步：

1. 预算表里 `plugin/terminal/resident` 单列一条（中位数 < 1 MB，`Process`）：设计的「性能」表有它，但 bench 的预算表只有记录用的 `plugin/*/resident`；具体 ID 比通配符优先，运行器展开时用它判定。`plugin/terminal/activate` 用默认的 `plugin/*/activate`，没有单列。
2. `crates/terminal/src/plugin.rs` 里 `ActivationToken` 的导入改为只在测试和 `test-support` 下编译：`app` 不带 `test-support` 地依赖本 crate 时，它是未使用的导入（警告）。行为不变。
