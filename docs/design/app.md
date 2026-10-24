# app 设计

状态：已实现（第 1 步，[#203](https://github.com/SoloJiang/lectern-app/pull/203)）。前置条件 A（[#201](https://github.com/SoloJiang/lectern-app/pull/201)）、B（[#202](https://github.com/SoloJiang/lectern-app/pull/202)）已实现。第 2 步（插件装配，[#205](https://github.com/SoloJiang/lectern-app/issues/205)，见「装配」）已实现（[#210](https://github.com/SoloJiang/lectern-app/pull/210)）。实现与原设计的差别见「与设计的差异」。

对应 issue [#199](https://github.com/SoloJiang/lectern-app/issues/199)。`app` 是二进制 `lectern`：它决定进程启动时先做什么、后做什么，在首帧之后创建进程级的共享资源（[ADR 0025](../adr/0025.md)「进程级的共享资源」），并按固定的顺序退出。第 1 步是最小的启动外壳：启动顺序、启动屏障、退出顺序和运行日志的接入（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 3 项）。第 2 步（[#205](https://github.com/SoloJiang/lectern-app/issues/205)）在首帧之后创建 `Cache`、`fs_watch` 并装配已经实现的插件（「装配」）。首帧的规则见 [ADR 0017](../adr/0017.md)，profile 的概念见[技术调研「插件架构」](tech-research.md#插件架构)。

## 范围

- 做：
  - `main` 的第一行装日志（`lectern_logging::install_early`），解析命令行的 `--log`。
  - 首帧之前读系统的首选语言，选界面语言，建窗口，画中性的骨架。
  - desktop profile 的启动屏障：首帧画出之后放开，然后依次启动日志的写盘线程、创建 `Executor`、`Kernel::start`。
  - 应用菜单和 `Quit` Action（⌘Q）：退出时依次关闭 kernel、executor 和日志，然后 `cx.quit()`。
  - 由系统发起的退出（注销、关机、Dock 菜单）也写日志的结束记录。
  - 把 `Logging`、`Executor`、`Arc<Kernel>` 放进一个 GPUI 全局对象，供以后的设置页、导出命令和插件装配取用。
- 不做：
  - 窗口骨架以外的界面：Welcome（[#29](https://github.com/SoloJiang/lectern-app/issues/29)）、Overview 与项目主页（[#30](https://github.com/SoloJiang/lectern-app/issues/30)），由各自的视图 crate 负责。
  - 控件、颜色和动效原语：归 `ui`。
  - 读取系统首选语言、线程 QoS：macOS 专属，归 `platform`（架构规则 6）。
- 本 issue 之外：
  - 按 profile 装配具体的插件，创建 `Cache` 和 `fs_watch` 并传给插件：第 2 步（[#205](https://github.com/SoloJiang/lectern-app/issues/205)，「装配」）。`usage`、`code_intel` 等其余插件在各自的前置条件完成后加入。
  - 窗口快照的读取与恢复、窗口位置的 frame autosave：随 [#30](https://github.com/SoloJiang/lectern-app/issues/30)。
  - headless profile、`bench-probe` feature、`Driver`、把应用数据目录指到临时目录：随 [bench 设计](bench.md)第 5 步（[#7](https://github.com/SoloJiang/lectern-app/issues/7)）。probe profile（`ui-probe` feature，UI 探针）：随 [ui_probe 设计「对其他 crate 的要求」](ui_probe.md#对其他-crate-的要求)第 3 项（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）。
  - 写盘线程和批量线程的 utility QoS：`platform` 提供设置 QoS 的接口之后，`app` 经 `Config::on_writer_start` 和 `executor::Config::on_thread_start` 传入。在此之前两者都是 `None`，线程用默认 QoS（logging 设计已允许）。
  - 详细日志的开关和导出界面：logging 第 12 项。
  - 离开守卫（有未保存的编辑时先询问）：随 [file_reader 设计「编辑」](file_reader.md#编辑)接入。本 issue 的 `Quit` 不询问。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui`、`gpui_platform` | 应用、窗口、菜单、Action | 已锁定（[ADR 0016](../adr/0016.md)） |
| `lectern-logging`（`sink` feature） | `install_early`、`Early::start`、`Logging::shutdown`、日志宏 | 只有 `app` 和 tool 层可以开 `sink`（架构检查） |
| `lectern-platform` | `dirs::logs`、`system::os_version`、`system::preferred_languages`（前置条件 B） | 架构规则 6 |
| `lectern-i18n` | `Locale::negotiate`、`Strings::of`、菜单文字 | 界面字符串不硬编码 |
| `lectern-kernel` | `PluginSet`、`Kernel::start`、`Kernel::shutdown` | |
| `lectern-executor` | `Executor::new`、`Executor::shutdown` | |
| `lectern-cache`、`lectern-fs-watch` | 第 2 步：`Cache::new`、`Cache::close`、`Watcher::new`、`Watcher::shutdown` | 进程级资源由 `app` 创建一次（ADR 0025） |
| `lectern-project`、`lectern-agents`、`lectern-review` | 第 2 步：构造插件 | 只有 `app` 和 tool 层依赖插件 crate（架构规则 7） |
| `lectern-fs-watch`（`test-support`，dev） | 第 2 步：profile 测试的 `FakeBackend` | |
| `lectern-logging`（`test-support`，dev） | 金丝雀测试的 `testing::capture()` | |
| `tempfile`（dev） | 启动测试的临时 `HOME` | 已在工作区里 |

不新增外部依赖。命令行只有一个选项，不引入命令行解析库：它会增加二进制体积和首帧前的初始化，换来的功能用不上。

## 接口

`app` 分成库 `lectern_app`（`src/lib.rs`，`pub fn run()`）和二进制 `lectern`（`src/main.rs` 只调用 `lectern_app::run()`），不供其他 crate 依赖。下面是模块之间的接口，测试经它们验证启动和退出的顺序。

```rust
// src/args.rs
pub struct Args {
    /// `--log <过滤>` 或 `--log=<过滤>`。出现多次时取最后一次。值不是 UTF-8 时当作没有给出。
    pub log: Option<String>,
    /// 只在调试构建里识别：启动完成后自动退出，供 `tests/startup.rs` 使用（见「测试」）。
    #[cfg(debug_assertions)]
    pub quit_after_start: Option<QuitPath>,
    /// 认不出的参数的下标（从 1 起，不含程序名）。不保存参数本身。
    pub unknown: Vec<usize>,
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitPath {
    /// 经 `Quit` Action 退出：`--quit-after-start`。
    Action,
    /// 直接调用 `cx.quit()`，走系统发起的退出那条路径：`--quit-after-start=system`。
    System,
}

/// 纯内存操作，在 `main` 的第一行、`install_early` 之前调用。
pub fn parse(args: impl Iterator<Item = OsString>) -> Args;
```

```rust
// src/profile.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// 发布构建运行的 profile。
    Desktop,
    /// `#[gpui::test]` 用：没有真实的帧，屏障在窗口建好后立即放开。
    Test,
}

impl Profile {
    /// 这个 profile 装的插件。本 issue 里两个 profile 都返回空集合。
    pub fn plugins(self) -> PluginSet;
}
```

```rust
// src/startup.rs
/// 首帧前的状态。主线程上创建，只放开一次。
pub struct Startup {
    early: Option<lectern_logging::Early>,
    profile: Profile,
}

/// 屏障放开后创建的进程级资源，作为 GPUI 全局对象保存（`impl Global`）。
/// 视图只读它，不经它做 IO：`Logging::set_verbose`、`status` 不阻塞，`export` 在 executor 的 Background 任务里调用。
pub struct Started {
    pub logging: Logging,
    pub executor: Executor,
    pub kernel: Arc<Kernel>,
}

impl Startup {
    /// 主线程，在 `run` 里 `install_early` 之后调用；结果设为 GPUI 全局对象。
    pub fn new(early: Early, profile: Profile) -> Startup;
    pub fn profile(&self) -> Profile;
}

/// 放开启动屏障。主线程，不阻塞。第一次调用从全局的 `Startup` 里取出 `Early`，依次：
/// `Early::start`（建写盘线程后返回）、`Executor::new`（不建线程）、`Kernel::start(profile.plugins())`
/// （建 kernel 线程后返回），记一条 info，把 `Started` 设为全局对象。之后的调用、以及退出开始后的调用什么也不做。
pub fn release(cx: &mut App);

/// desktop：根视图第一次 `render` 时调用，登记「启动屏障」一节的两次帧回调。
pub fn arm(window: &mut Window);
```

```rust
// src/quit.rs
actions!(lectern, [Quit]);

/// 退出顺序里的三步。发布构建里由 `Started` 实现；测试换成记录调用顺序、可以卡住的假实现。
pub trait QuitSteps: Send + 'static {
    /// 调用 `Kernel::shutdown`，kernel 线程退出后调用 `done`。
    fn kernel(&self, done: Box<dyn FnOnce() + Send>);
    fn executor(&self, timeout: Duration) -> executor::Shutdown;
    fn logging(&self, timeout: Duration) -> lectern_logging::Flushed;
}

pub struct Timeouts {
    /// kernel 与 executor 合计，从开始退出算起。发布构建 2 s（kernel 设计 `Kernel::shutdown`）。
    pub services: Duration,
    /// `Logging::shutdown`。发布构建 500 ms（logging 设计）。
    pub logging: Duration,
}

/// 在名为 `lectern-quit` 的线程上执行退出顺序，返回时三步都已完成或超时。阻塞，不在主线程上调用。
pub fn run(steps: &dyn QuitSteps, timeouts: &Timeouts) -> Outcome;

pub struct Outcome {
    pub kernel_timed_out: bool,
    pub executor: executor::Shutdown,
    pub logging: lectern_logging::Flushed,
}
```

`quit::init(cx)` 在 `run` 里登记 `Quit` 的处理函数和 `on_app_quit` 回调；`quit::quit(cx)` 是处理函数本身，`quit::is_quitting(cx)` 读退出是否已经开始。`Quit` Action 的处理函数在主线程上：

1. 已经在退出中时直接返回。
2. 屏障还没有放开（没有 `Started`）时直接 `cx.quit()`。
3. 否则建 `lectern-quit` 线程执行 `run`，主线程用一个前台任务等它结束（不阻塞主线程，kernel 撤销时需要在主线程上执行的守卫 drop 照常进行），然后 `cx.quit()`。线程建不起来时在主线程上直接 `cx.quit()`，结束记录由下面的 `on_app_quit` 写。

`cx.quit()` 之后 GPUI 在 `applicationWillTerminate:` 里调用 `on_app_quit` 登记的回调，最多等 200 ms（`gpui::SHUTDOWN_TIMEOUT`）。`app` 在这里调用 `Logging::shutdown(150 ms)`。经 `Quit` 退出时日志已经关闭，这次调用立即返回；由系统发起的退出不经 `Quit`，这次调用写出缓冲和结束记录。

## 数据流与线程

### 启动顺序

| 步骤 | 线程 | 做什么 | IO 或建线程 |
|---|---|---|---|
| 1 | 主线程 | `args::parse(std::env::args_os())` | 否 |
| 2 | 主线程 | `install_early(EarlyConfig { dir: platform::dirs::logs(), cli_filter: args.log })` | 否：只读环境变量，分配首帧前缓冲，装 panic 钩子 |
| 3 | 主线程 | 认不出的参数各记一条 warn（进首帧前缓冲） | 否 |
| 4 | 主线程 | `Locale::negotiate(platform::system::preferred_languages())`、`Strings::of` | 否：系统偏好在内存里，资源已编译进二进制（[i18n 设计「数据流与线程」](i18n.md#数据流与线程)） |
| 5 | 主线程 | `Application::new().run(…)`：`lectern_ui::init(cx, strings)`（在 `set_menus` 之前：内置的两份 `Palette`、减弱动态效果、登记 `platform` 的变化通知，不读系统色，[ui 设计「初始化」](ui.md#初始化)）、设置应用菜单、绑定 ⌘Q、建骨架窗口（`open_window` 的回调里 `lectern_ui::attach(window, cx)`，`Subscription` 放在根视图 `Skeleton` 里） | GPUI 和 AppKit 自己的初始化；`init` 只做内存操作 |
| 6 | 主线程 | 首帧画出；根视图第一次 `render` 时登记启动屏障（下节） | 否 |
| 7 | 主线程 | `startup::release`：`Early::start`、`lectern_ui::ready`（`gpui_component::init`、读两套系统色、投影到 gpui-component 的主题；在 `Kernel::start` 之前，插件挂载视图时 `ui` 已就绪）、`Executor::new`、`Kernel::start` | 建 `lectern-log` 线程和 kernel 线程，立即返回；`ready` 约 2–3 ms 的主线程内存操作，不做 IO |
| 8 | `lectern-log` | 建日志目录、清理、写文件头和首帧前缓冲 | 是 |
| 9 | kernel 线程 | 读清单、激活 `AfterFirstFrame` 插件（本 issue 里没有插件） | 由插件决定 |

第 4 步的 `platform::system::os_version()` 不在首帧前调用：它在第 7 步填 `Header` 时调用，是一次 `sysctl`，不读文件。

### 启动屏障

kernel 设计规定 desktop profile 在第一帧真正显示时放开屏障（[kernel 设计「启动」](kernel.md#启动)）。精确的显示时刻要在 Metal drawable 上登记 `addPresentedHandler`，锁定的 GPUI 不暴露 drawable，`platform::frame::on_first_presented` 还没有实现（[bench 设计「需要其他 crate 提供的接口」](bench.md#需要其他-crate-提供的接口)）。`app` 因此用 GPUI 的帧回调：

- desktop：根视图第一次 `render` 时调用 `window.on_next_frame`，回调里再调用一次 `window.on_next_frame`，第二个回调里调用 `startup::release`。GPUI 在一次帧请求（display link 的回调）的开头执行上一帧登记的回调，所以屏障在首帧画出之后的第二次帧请求时放开。首帧的命令缓冲在第一次帧请求的末尾提交，在下一个垂直同步显示；第二次帧请求时它已经显示。比精确的显示时刻最多晚约 2 个刷新周期（60 Hz 下约 33 ms；窗口不在前台时 GPUI 按 30 fps 请求帧，更晚，见「已知限制」），只推迟后台初始化，不计入冷启动（冷启动的终点是首帧的显示时刻，由 bench 单独测量）。
- test：窗口建好后立即放开。
- `platform::frame::on_first_presented` 实现以后，desktop 改为在它的回调里放开（回调在任意线程上，转到主线程后调用 `release`），并删掉本节的帧回调。

`startup::release` 只生效一次。窗口关闭后重新打开（见「界面」）时根视图再次登记，不会再次启动。

### 退出顺序

```
Quit（主线程）
  └─ lectern-quit 线程：
       Kernel::shutdown(done) ── 等 done，最多到 services 截止时间
       Executor::shutdown(剩余时间)
       Logging::shutdown(500 ms)
  └─ 主线程：cx.quit()
       └─ applicationWillTerminate: → on_app_quit → Logging::shutdown(150 ms)（已关闭时立即返回）
```

- kernel 超过截止时间还没调用 `done` 时，记一条 warn，继续关闭 executor。剩余时间为零时 `Executor::shutdown(0)` 只取消任务、不等线程。
- executor 返回 `TimedOut` 时记一条 warn（executor 自己也记 `shutdown timed out`）。
- `Logging::shutdown` 之后的记录只计数，所以 `Outcome` 里的日志结果不再记日志。
- 按 ⌘Q 到进程退出最多约 2.5 s，之后 GPUI 再给 `on_app_quit` 200 ms。等待期间窗口照常显示，主线程照常处理事件。

## 装配

[#205](https://github.com/SoloJiang/lectern-app/issues/205)。desktop profile 在启动屏障放开后创建进程级资源，装配已经实现的插件。test profile 仍是空集合，`#[gpui::test]` 不受影响。

### 插件与资源

| 插件 ID | 构造 | 提供 | 依赖 |
|---|---|---|---|
| `project` | `ProjectPlugin::new(ProjectDeps { executor, watcher, home })` | `dyn Projects` | 无 |
| `session-claude-code`、`session-codex` | `lectern_agents::plugins(AgentsDeps { executor, cache, watch: watcher, decoded_dir, roots: AgentRoots::from_env(&home), filter: SessionFilter::new() })` | `dyn SessionSource` | `dyn Projects` |
| `review` | `lectern_review::plugin(ReviewDeps { executor, cache, watcher, data_dir, git_ceilings })` | `dyn Review` | `dyn Projects`、`dyn SessionSource` |

`usage`、`code_intel` 不在本 issue：它们的前置条件（usage 前置条件 C、code_intel 第 5 步）完成后各自加进这张表。

目录与环境：

- `home`：环境变量 `HOME`，必须是绝对路径。
- `cache::Config::root`：`platform::dirs::app_cache()` 下的 `cache/`；`ui_thread` 为主线程的 `ThreadId`；`budgets` 取默认值。
- `AgentsDeps::decoded_dir`：`platform::dirs::app_cache()` 下的 `agents/decoded/`。
- `ReviewDeps::data_dir`：`platform::dirs::app_data()` 下的 `review/`。
- `ReviewDeps::git_ceilings`：`GIT_CEILING_DIRECTORIES` 用 `std::env::split_paths` 分开，只保留绝对路径，读一次（与 `project` 的 `LecternGit::from_env` 相同）。
- 三个目录都只拼路径；不存在时由使用它的 crate 在第一次写入时创建。

`home`、`app_cache()`、`app_data()` 任一个为 None 时不装配插件：kernel 以空集合启动，记一条 warn（见「日志与诊断」）。窗口照常显示，只是没有项目和会话。

### 创建顺序

全部在 `startup::release` 里、主线程上，紧接 `Early::start` 之后：

1. `Executor::new(Config::default())`（已有）。
2. `Cache::new(…)`。
3. `Watcher::new(platform::fs_events::backend(executor.clone()), executor.clone(), fs_watch::Config::default())`。
4. 按上表构造插件，放进 `PluginSet`。
5. `Kernel::start(set)`。

这几步都不做 IO、不建线程，只分配内存，所以 `release` 仍不阻塞：

- `Cache::new` 只建内存里的表；分库文件在第一次读写时才打开（[cache 设计「生命周期」](cache.md#生命周期)）。
- `Watcher::new` 不启动 fs_watch 线程（第一次 `watch_*` 时才启动）；`fs_events::backend` 在第一次 arm 时才建 dispatch queue 和 kqueue 线程。
- `ProjectPlugin::new` 只读 `GIT_CEILING_DIRECTORIES`；`AgentRoots::from_env` 只读环境变量；`agents::plugins`、`review::plugin` 只保存依赖。
- `Kernel::start` 建 kernel 线程后返回（已有）。插件都是 `AfterFirstFrame`，在 kernel 线程上激活。

### 接口的变化

```rust
// src/profile.rs
/// `release` 里创建、交给 profile 的资源。
pub struct Resources {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: Watcher,
    pub home: PathBuf,
    pub app_cache: PathBuf,
    pub app_data: PathBuf,
    pub git_ceilings: Arc<[PathBuf]>,
}

impl Profile {
    /// desktop：上表的四个插件；test：空集合。不做 IO。
    pub fn plugins(self, resources: &Resources) -> PluginSet;
}

// src/startup.rs
pub struct Started {
    pub logging: Logging,
    pub executor: Executor,
    pub kernel: Arc<Kernel>,
    /// 目录缺失、没有装配插件时为 None。
    pub cache: Option<Cache>,
    pub watcher: Option<Watcher>,
}
```

### 退出顺序的变化

`lectern-quit` 线程依次：

```
Kernel::shutdown(done) ── 等 done，最多到 services 截止时间；按依赖的相反顺序停用插件（review → agents → project），插件 drop 自己的监听
Watcher::shutdown()    ── 停 fs_watch 线程，drop 后端的监视；最多等到 services 截止时间
Executor::shutdown(剩余时间)
Cache::close()         ── 写回访问时间、关闭连接；最多等到 services 截止时间，剩余时间为零时不调用（drop 同样安全，只丢访问时间）
Logging::shutdown(500 ms)
```

- kernel、watcher、executor、cache 共用 2 s 的截止时间（`Timeouts::services`），所以按 ⌘Q 到进程退出仍最多约 2.5 s（「退出顺序」）。
- `Watcher::shutdown` 是阻塞的：fs_watch 线程正在处理一项（sink 的投递、reflog 的读取）时，它最多等 `fs_watch::Config::flush_timeout`（默认 2 s），见 [fs_watch 设计](fs_watch.md)。`flush_timeout` 同时限制 `flush_revision`，不为退出缩短它。`quit::run` 在短命线程 `lectern-quit-step` 上调用 `QuitSteps::watcher`，最多等到截止时间；到时记 warn `watcher shutdown timed out`，继续下一步，短命线程留到进程退出。插件停用之后它们的 sink 已经释放，正常情况下几毫秒内返回。
- `Cache::close` 也是阻塞的：它可能等一个超时的 executor 任务持有的分库锁。`quit::run` 用同样的方式调用 `QuitSteps::cache`，到截止时间记 warn `cache close timed out`。没有关闭只丢访问时间。
- `QuitSteps` 加两个方法：`fn watcher(&self)`、`fn cache(&self)`，都阻塞；没有装配时立即返回。等待与超时在 `quit::run` 里，所以假的 `QuitSteps` 能测超时。`Outcome` 加 `watcher_timed_out`、`cache_timed_out`。
- 由系统发起的退出不经这条路径：只有 `on_app_quit` 里的 `Logging::shutdown(150 ms)`（已有）。缓存没有关闭不会丢失已提交的事务（[cache 设计「生命周期」](cache.md#生命周期)第 4 步）。

### 性能

- 冷启动：首帧之前没有新增的工作。
- 首帧之后：四个插件激活。`project`、`review` 的 `activate` 只建空表；`agents` 在 `started` 里提交发现任务（`Background`），枚举会话目录（[agents 设计「数据流与线程」](agents.md#数据流与线程)第 1、2 步）。
- 空闲内存（ADR 0017：60 MB）：在 `app/idle_empty`（bench 第 5 步）实现之前手动测量：临时 `HOME`、去掉 `CLAUDE_CONFIG_DIR` 和 `CODEX_HOME`、没有会话、不打开项目，启动 10 s 后读 `phys_footprint`，写进「实际验证」。超出 60 MB 时交给产品负责人决定（ADR 0017），不放宽。有真实会话时的内存由 agents 的预算覆盖，不在这里测量。
- 退出：kernel、watcher、executor、cache 合计要在 2 s 内完成。超时会记 warn；不加长截止时间，先找慢的原因。

### 日志与诊断

- info `plugins assembled`：`profile`、`plugins`（插件个数）。在 `startup barrier released` 之前。
- warn `plugins not assembled`：`missing`（`home`、`app_cache` 或 `app_data`）。不记路径。test profile 不装配插件，不检查目录，不记这条 warn。
- warn `watcher shutdown timed out`、`cache close timed out`：`timeout_us`（「退出顺序的变化」）。
- debug `quit finished` 加 `watcher_us`、`cache_us`。
- 插件激活、失败和 kernel 的静止由 kernel 自己记录（[kernel 设计「日志与诊断」](kernel.md#日志与诊断)）。
- 金丝雀：`HOME` 设为含金丝雀字符串的临时目录启动，日志里没有这个字符串（`tests/startup.rs` 的 `home_path_stays_out_of_the_log`）。

### 测试

- `every_profile_graph_validates`：用临时目录建 `Resources`（`Cache::new` 指向临时目录，`Watcher` 用 `fs_watch::testing::FakeBackend`），对每个 profile 调用 `plugins(&resources).validate()`；之后临时目录仍是空的（构造不做 IO）。
- `quit`：`steps_run_in_order` 的顺序改为 kernel、kernel 完成、watcher、executor、cache、logging；`a_hung_kernel_does_not_block_quit` 里 watcher 照常调用、cache 因剩余时间为零跳过；`a_hung_watcher_does_not_block_quit`（`watcher` 不返回时，到截止时间后照常关闭 executor 和日志，`watcher_timed_out` 为 true，cache 跳过）。
- `tests/startup.rs`（这个测试二进制的测试一次只运行一个，见「与设计的差异」第 2 步）：
  - `startup_writes_a_header_and_an_end_record` 加检查：`plugins assembled` 在 `first frame drawn` 之后、`plugins` 为 4。
  - `quit_shuts_the_services_down_in_time`：`--quit-after-start` 时日志里没有 `kernel shutdown timed out`、`executor shutdown timed out`。
  - `home_path_stays_out_of_the_log`（上一节）。
  - 每个启动测试的 `HOME` 都是临时目录，并去掉 `CLAUDE_CONFIG_DIR`、`CODEX_HOME`（`AgentRoots::from_env` 优先读它们），不读本机的会话目录。
- 手动：空闲内存（「性能」）。

## 性能

- 冷启动：首帧之前只有「启动顺序」的第 1–6 步。新增的工作是参数解析（几个字符串比较）、`install_early`（预算 ≤ 20 µs，`log/install_early`）、语言协商（常数级的内存操作）和 `lectern_ui::init`（预算 ≤ 300 µs，`ui/init`：一次减弱动态效果的读取、两份内置表、登记通知；不读系统色，[ui 设计「性能」](ui.md#性能)），加上骨架窗口的 `lectern_ui::attach`（登记一个外观观察者），合计远小于 20 ms 预算的 1%。首帧之前不建线程、不读写 Lectern 自己的文件、不调用 kernel 和 executor、不运行 `gpui_component::init`（ADR 0017）。
- 空闲内存：首帧之后多出日志的缓冲（logging 设计「性能」：≤ 512 KiB）、kernel 线程和它的栈，以及 `ui` 的 `UiState`（< 8 KiB）和 `gpui_component::init` 建的主题全局（预期 < 1 MB，`ui/idle_memory` 实测，[ui 设计「性能」](ui.md#性能)）。executor 在第一次提交任务之前不建线程。空集合的 kernel 静止后不再唤醒。
- 基准：`app/cold_start`、`app/idle_empty` 已在预算表里，由 bench 第 5 步的进程级基准测量；本 issue 不新增基准。在那之前用「测试」里的 `startup_*` 测试验证顺序：首帧之前没有日志文件。

## 界面

- 窗口：没有打开项目时的独立窗口，820 × 520 pt，居中（[交互规范「窗口结构」](interaction.md#窗口结构)的 Welcome 宽度）。标题栏透明，窗口背景用 `WindowBackgroundAppearance::Blurred`（系统的毛玻璃材质，自动适配浅色和深色模式，不需要自定义颜色），根视图不画任何内容，这就是 ADR 0017 的中性骨架。`ui` 提供骨架控件以后，由 Welcome 和 Overview 的视图替换这个根视图。
- 关闭窗口不退出应用（产品负责人 2026-10-23 决定，见「未决问题」）。点 Dock 图标而且没有窗口时，重新打开骨架窗口（`Application::on_reopen`，在 `run` 之前登记）。
- 应用菜单，Fluent 键在 `crates/i18n/locales/{en,zh-CN}/app.ftl`（前置条件 A）：

  | 菜单项 | 键 | en | zh-CN | 快捷键 | 动作 |
  |---|---|---|---|---|---|
  | 隐藏 | `app-menu-hide` | Hide { -app-name } | 隐藏 { -app-name } | ⌘H | `cx.hide()` |
  | 隐藏其他 | `app-menu-hide-others` | Hide Others | 隐藏其他 | ⌥⌘H | `cx.hide_other_apps()` |
  | 全部显示 | `app-menu-show-all` | Show All | 全部显示 | — | `cx.unhide_other_apps()` |
  | 退出 | `app-menu-quit` | Quit { -app-name } | 退出 { -app-name } | ⌘Q | `Quit` |

  菜单的文字在启动时按第 4 步选出的语言生成。设置里切换语言后重建菜单，随语言设置的实现（[i18n 设计「数据流与线程」](i18n.md#数据流与线程)）一起做。
- UI 探针的语义节点：本 issue 的根视图没有可交互元素，不登记节点。
- 动效：没有。

## 状态与异常

| 情况 | 处理 |
|---|---|
| `platform::dirs::logs()` 为 None（没有 `HOME`） | 日志只留在内存里（logging 设计「状态与异常」），启动照常 |
| 认不出的命令行参数 | 忽略，每个记一条 warn（只有下标） |
| `--log` 的过滤里有认不出的指令 | 由 logging 处理：跳过，写盘线程启动后记 warn |
| 系统首选语言里没有支持的语言 | `Locale::negotiate` 返回 `En` |
| 屏障放开之前按 ⌘Q | 直接 `cx.quit()`。写盘线程没有启动，首帧前缓冲里的记录丢失（见「已知限制」） |
| 退出过程中再按 ⌘Q | 忽略 |
| kernel 在 2 s 内没有关闭完 | 记 warn，继续关闭 executor 和日志 |
| fs_watch 线程或缓存的关闭到截止时间还没返回 | 记 warn，继续下一步（「装配」「退出顺序的变化」） |
| `lectern-quit` 线程建不起来 | 主线程直接 `cx.quit()`，`on_app_quit` 写结束记录 |
| 主线程 panic | logging 的 panic 钩子写记录（写盘线程还没启动时由钩子自己打开文件），然后调用原来的钩子 |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `app`。

- 默认记录：
  - info `startup barrier released`：`profile`（`desktop` 或 `test`）。记录自带的 `up` 是从 `install_early` 起的微秒数，用来对照启动耗时。
  - info `first frame drawn`：根视图第一次 `render` 之后的那次帧回调里记，同样只靠 `up` 和 `t`。启动测试用它的 `t` 检查日志文件在首帧之后才创建。
  - `ui` 自己记（target 为 `ui`，[ui 设计「日志与诊断」](ui.md#日志与诊断)）：info `ui initialised`（`accent` 总是 `builtin`）在 `first frame drawn` 之前，进首帧前缓冲；info `ui ready`（`accent` 为 `system` 或 `builtin`）在 `first frame drawn` 之后、`plugins assembled` 和 `startup barrier released` 之前。启动测试检查这个顺序。
  - info `quit requested`：`path` 为 `action`（`Quit` Action）或 `system`（不经 `Quit`，只在 `on_app_quit` 里看到日志还没关闭时记）。
  - warn `unrecognized argument`：`index`（从 1 起）。
  - warn `kernel shutdown timed out`：`waited_us`。
  - warn `executor shutdown timed out`：`busy_workers`。
  - warn `could not start the quit thread`：`error` 为 `Value::io`。
- 详细级别：debug `quit finished`：`kernel_us`、`executor_us`（各步实际用时）。`quit::run` 在调用 `Logging::shutdown` 之前记这一条，之后的记录只计数。
- 关联 ID：没有。`app` 的代码不在任何插件的作用域里。
- 不记录：命令行参数的内容（认不出的参数只记下标；参数里可能有路径和令牌）；`--log` 的过滤字符串只由 logging 写进文件头；环境变量的值；系统首选语言列表。
- 金丝雀测试：`canary_stays_out_of_app_logs`（`src/tests/logs.rs`）：用 `testing::capture()`，把金丝雀放进一个认不出的参数和 `--log` 的值，经 `parse` 和记录 warn 的同一个函数，再跑一遍退出顺序，`assert_absent(金丝雀)`。

## 测试

`app` 依赖 GPUI，只在 macOS 上编译和测试（`just test -p lectern-app`）。

- 单元测试（`src/tests/`）：
  - `args`：`log_value_forms`（`--log x`、`--log=x`、多次出现取最后一次）、`log_without_a_value_is_unknown`、`non_utf8_log_is_ignored`、`unknown_arguments_keep_only_indexes`、`quit_after_start_only_in_debug_builds`。
  - `profile`：`every_profile_graph_validates`（对每个 `Profile` 调用 `plugins().validate()`，kernel 设计要求的 profile 测试）。
  - `quit`（用假的 `QuitSteps`、毫秒级的 `Timeouts`）：`steps_run_in_order`（kernel 的 `done` 之后才关 executor，executor 之后才关日志）、`a_hung_kernel_does_not_block_quit`（`done` 从不调用时，到截止时间后照常关闭 executor 和日志，`kernel_timed_out` 为 true）、`a_dropped_done_is_not_a_timeout`（kernel 线程没有调用 `done` 就退出时，不记为超时）、`executor_gets_the_remaining_time`、`timeouts_are_logged`。
  - `logs`：`canary_stays_out_of_app_logs`。
  - `#[gpui::test]`：`test_profile_releases_once`（窗口建好后 `Started` 已设为全局对象；再次调用 `release` 不再启动 kernel）、`quit_before_release_starts_nothing`（屏障放开之前退出：不设 `Started`，之后的 `release` 也不启动任何东西）。test profile 在开窗口之前先 `lectern_ui::init_fixed`（骨架窗口的 `attach` 要 `ui` 的全局；`release` 里的 `ready` 随后是空操作）。
  - `#[gpui::test]`，`ui` 的接入（`src/tests/ui.rs`，[ui 设计「验收」](ui.md#验收)第 1 步的 `app` 项；外观变化经 `lectern_ui::testing::simulate_appearance_change`，GPUI 的测试平台不能改窗口外观）：`appearance_change_refreshes_the_skeleton_window`（test profile：`init_fixed`、`window::open`（含 `attach` 与 `release`）之后模拟外观变深，骨架窗口重画一次，`colors` 给出深色的 `Palette`）、`appearance_change_before_ready_does_not_touch_the_theme`（直接 `lectern_ui::init`、`window::open`，不设 `Startup` 全局，所以不 `release`；`ready` 之前模拟一次外观变化：不 panic、窗口重画一次、`is_ready` 仍为 false、gpui-component 的 `Theme` 全局不存在，即没有被读写）。
- 启动测试（`tests/startup.rs`，启动真实的 `lectern` 进程，需要 macOS 的图形会话；macOS CI 任务满足）：每个测试用新的临时目录作 `HOME`，日志落在 `$HOME/Library/Logs/Lectern/`，带调试构建才有的 `--quit-after-start`，等进程退出（30 s 后结束进程并判为失败，只防卡住，不判定耗时）。`--quit-after-start` 等屏障放开、本次运行的日志文件出现之后才退出。
  - `startup_writes_a_header_and_an_end_record`：只有一个 `lectern-*.log`；第一行是文件头（`msg` 为 `file`）；`ui initialised`、`first frame drawn`、`ui ready`、`plugins assembled`、`startup barrier released` 依次出现；`ui initialised` 的 `accent` 为 `builtin`，`ui ready` 的为 `system`（真实的 macOS 图形会话）；最后一行是 `run end`。
  - `log_file_is_created_after_the_first_frame`：日志文件的创建时间（`Metadata::created`）不早于 `first frame drawn` 的 `t`（毫秒精度）。
  - `cli_and_env_filters_reach_the_header`：`LECTERN_LOG=debug` 启动时文件头的 `filter` 是 `debug`；`--log cache=trace` 启动时是 `cache=trace`（同时设了 `LECTERN_LOG` 时仍是 `cache=trace`）。
  - `system_quit_writes_the_end_record`：`--quit-after-start=system`，最后一行是 `run end`，文件里有 `quit requested`、`path` 为 `system`。
  - `second_run_sees_a_clean_previous_run`：同一个 `HOME` 连续启动两次，第二次的文件里没有 `previous run ended unexpectedly`。

## 对其他 crate 的要求

每一项是一个 PR，在 `app` 的实现 PR 之前合并：

- **A. `i18n`**：新增 `locales/en/app.ftl`、`locales/zh-CN/app.ftl`，内容是「界面」一节的四个菜单键。
- **B. `platform`**：[i18n 设计「数据流与线程」](i18n.md#数据流与线程)里启动时用的 `preferred_languages()`，放在 `system` 模块：`system::preferred_languages() -> Vec<String>`。macOS 上用 `CFLocaleCopyPreferredLanguages`（本进程生效的首选语言，包含应用域里 `AppleLanguages` 的覆盖；内存里的系统偏好，不读 Lectern 自己的文件），返回 BCP 47 标签，按系统的顺序；其他平台返回空列表。i18n 设计里的 `system_languages()` 和 `language_override()` 只供设置页使用，随语言设置实现，不在本 issue。测试：macOS 上返回的列表不为空、每一项都能被 `Locale::negotiate` 接受；其他平台为空。在 platform 设计里加一节，写「日志与诊断」（读不到时返回空列表，不记日志）。

## 未决问题

没有。原来的问题 1（关闭最后一个窗口时是否退出应用）已由产品负责人在 2026-10-23 决定：选 (a)，不退出，点 Dock 图标重新打开窗口（Xcode 欢迎窗口和大多数文档类 macOS 应用的做法）；不选 (b)（退出，等同于 ⌘Q）。

## 已知限制

- 屏障放开之前（首帧之后约 2 个刷新周期内）退出时，首帧前缓冲里的记录丢失，下一次运行也不会报告上一次异常结束：这时还没有创建本次运行的日志文件。
- 窗口被完全遮挡、收不到帧请求时，屏障不放开，后台初始化一直推迟到窗口重新可见；这期间 ⌘Q 按「屏障放开之前」处理。屏幕锁定的图形会话（`CGSessionCopyCurrentDictionary` 的 `CGSSessionScreenIsLocked` 为 1）同样：首帧画出，之后不再有帧请求，屏障不放开；启动测试在锁屏期间全部 30 s 后判为失败（`ui` 接入时实测；接入前一天的本机调试构建同样如此），解锁后再跑。
- 由系统发起的退出不经 `Quit`：kernel 和 executor 不按顺序关闭（kernel 设计「已知限制」），只有日志的结束记录在 `on_app_quit` 的 200 ms 里写出。
- 屏障比首帧的实际显示最多晚约 2 个刷新周期，直到 `platform::frame::on_first_presented` 实现（「启动屏障」）。窗口不是前台窗口时 GPUI 按 30 fps 请求帧（`inactive_frame_interval` 33 ms），间隔按 33 ms 计；从终端启动时实测首帧画出到屏障放开约 90–100 ms（「实际验证」）。

## 验收

分两步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-app`、`just deny` 通过。

**前置条件 A、B**：各自的 PR，`just test -p lectern-i18n`、`just test -p lectern-platform` 通过。

**第 1 步：启动外壳与日志接入（本 issue 的实现 PR，`Closes #199`）。**

- 「测试」一节的单元测试和启动测试全部通过：`just test -p lectern-app`。
- `rg 'eprintln!|println!|log::' crates/app/src` 没有结果（记录只经 `lectern_logging` 的宏）。
- `cargo tree -p lectern-app -e features -i lectern-logging` 里有 `sink`。
- 手动在 macOS 上运行 `cargo run -p lectern-app`：窗口出现，按 ⌘Q 后进程在 3 s 内退出，`~/Library/Logs/Lectern/` 里本次运行的文件最后一行是 `run end`。
- logging 设计的状态行和「实际验证」写明第 3 项已实现，第 3 步验收里依赖进程级基准的一条（`app/cold_start` 不变慢）随 bench 第 5 步验证。

**第 2 步：装配（[#205](https://github.com/SoloJiang/lectern-app/issues/205) 的实现 PR，`Closes #205`）。** 在 `platform` 的 `dirs::app_cache`（[#206](https://github.com/SoloJiang/lectern-app/pull/206)）和 review 第 4 步（插件）合并之后。

- 「装配」一节「测试」里的单元测试和启动测试通过：`just test -p lectern-app`。
- `python3 scripts/check_architecture.py` 通过：`app` 是唯一依赖插件 crate 的非 tool crate。
- 手动：临时 `HOME` 启动发布构建，日志里有 `plugins assembled`（`plugins` 为 4）；按 ⌘Q 后 3 s 内退出，没有 `shutdown timed out` 的 warn；空闲内存的实测值写进「实际验证」。
- review 设计的状态行写明前置条件 D 的 `app` 部分已实现；fs_watch 设计的状态行写明第 4 步的装配部分已实现（实测部分另列）。

## 实际验证

第 1 步，macOS 27.0.1（Apple M5 Pro，15 核，Rust 1.97.0）：

- `just check`、`just deny` 通过。`just test -p lectern-app` 在系统自带的 bash 3.2 上因空数组报 `selection[@]: unbound variable` 而没有运行，改为直接运行它展开后的命令 `cargo nextest run --locked -p lectern-app --all-features --no-tests=pass`：19 个测试通过（`src/tests/` 的单元测试 14 个，`tests/startup.rs` 的启动测试 5 个）。
- `rg 'eprintln!|println!|log::' crates/app/src` 没有结果；`cargo tree -p lectern-app -e features -i lectern-logging` 里有 `lectern-logging feature "sink"`。
- 手动运行发布构建（临时 `HOME`）：菜单是 Hide Lectern、Hide Others、Show All、Quit Lectern（经 System Events 读出）；按 ⌘Q 后进程在 1 s 内以退出码 0 退出。日志文件依次是文件头、`first frame drawn`、`startup barrier released`（`profile` 为 `desktop`）、`quit requested`（`path` 为 `action`）、`run end`。文件头的 `up` 是 257 ms，即启动后 1 s 内出现本次运行的文件（logging 设计第 3 步）。
- 同一构建连续 3 次（从终端启动，窗口不在前台）：`first frame drawn` 的 `up` 是 120、141、137 ms，`startup barrier released` 比它晚 88、100、89 ms。`up` 从 `install_early` 算起，`first frame drawn` 是首帧画出的时刻，不是显示时刻，这些数字不是 ADR 0017 定义的冷启动；冷启动由 bench 第 4 步（空 GPUI 窗口的基线）和第 5 步（`app/cold_start`）测量。屏障晚于首帧的时间见「已知限制」。

第 2 步（装配），同一台机器：

- `cargo clippy --locked -p lectern-app --all-targets --all-features -- -D warnings`、`cargo fmt --check` 通过；`python3 scripts/check_architecture.py` 通过（32 个 crate）；`rg 'eprintln!|println!|log::' crates/app/src` 没有结果。
- `cargo nextest run -p lectern-app`：22 个测试通过（单元测试 15 个，启动测试 7 个），连续 11 次。新增或改过的：`every_profile_graph_validates`（desktop 4 个插件、test 0 个，临时目录之后仍是空的）、`steps_run_in_order`（6 步的顺序）、`a_hung_kernel_does_not_block_quit`、`a_hung_watcher_does_not_block_quit`、`startup_writes_a_header_and_an_end_record`（`plugins assembled` 在首帧之后、屏障之前，`plugins` 为 4）、`quit_shuts_the_services_down_in_time`、`home_path_stays_out_of_the_log`（`HOME` 的目录名含金丝雀，trace 级别）。
- 手动运行发布构建，临时 `HOME`，去掉 `CLAUDE_CONFIG_DIR`、`CODEX_HOME`，没有会话，不打开项目：
  - 空闲内存：启动 10 s 后 `footprint -p` 的 `phys_footprint` 3 次都是 40 MB（ADR 0017：60 MB）。
  - 日志依次是 `first frame drawn`、`plugins assembled`（`plugins` 为 4）、`startup barrier released`、4 条 `plugin activated`、2 条 `sessions discovered`。
  - 按 ⌘Q 后 0.08 s 退出，退出码 0，没有 warn。详细日志（`verbose/`）的 `quit finished`：`kernel_us` 157、`watcher_us` 149、`executor_us` 189、`cache_us` 62。

`ui` 接入（PR_NUMBER_PLACEHOLDER，同一台机器，[ui 设计「验收」](ui.md#验收)第 1 步的 `app` 项）：

- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过（32 个 crate）；`rg 'eprintln!|println!|log::' crates/app/src` 没有结果。
- `cargo nextest run --locked -p lectern-app --all-features -E 'not binary(startup)'`：17 个单元测试通过（原有 15 个加新增的 `appearance_change_refreshes_the_skeleton_window`、`appearance_change_before_ready_does_not_touch_the_theme`）。`tests/startup.rs` 的 7 个启动测试本机没有通过：图形会话处于锁屏（`CGSSessionScreenIsLocked` 为 1，用户不在），窗口画出首帧后收不到帧请求，屏障不放开，7 个都在 30 s 后判为失败；接入前一天的本机调试构建同样卡住（「已知限制」）。它们由 macOS CI 运行；`startup_writes_a_header_and_an_end_record` 多检查 `ui initialised` → `first frame drawn` → `ui ready` → `plugins assembled` → `startup barrier released` 的顺序和两条记录的 `accent`，结果以 CI 为准。
- 发布构建（`cargo build --release --locked -p lectern-app`）成功；临时 `HOME` 下的手动运行（空闲内存、`ui initialised` / `ui ready` 的字段与耗时）因为同一个锁屏原因没有做：屏障不放开，日志文件不出现，内存读数没有意义。解锁后按「装配」「性能」的方法补测，写进这里。

## 与设计的差异

- **库与二进制分开。** 原文写 `app` 是二进制 crate。记录的 target 取 `module_path!` 的第一段：二进制名为 `lectern` 时 target 是 `lectern`，不是 logging 设计规定的 `app`。现在逻辑在库 `lectern_app` 里，二进制只调用 `lectern_app::run()`，target 是 `app`（「接口」已改）。
- **`release` 是自由函数。** 原文是 `Startup::release(&mut self, cx)`。`Startup` 是 GPUI 全局对象，帧回调里只有 `cx`，在借用全局对象时不能再设 `Started`，所以改为 `startup::release(cx)` 从全局对象里取出 `Early`。退出开始之后它什么也不做：屏障在 ⌘Q 之后才到时不再启动 kernel（「接口」已改）。
- **`--quit-after-start` 等日志文件出现。** 屏障放开后写盘线程异步建文件；立即退出时测试与建文件竞争。现在调试入口在后台任务里查到 `lectern-*.log` 之后才退出（「测试」已改）。
- **测试名与超时。** `quit_before_release_quits_at_once` 改为 `quit_before_release_starts_nothing`：TestPlatform 的 `quit()` 不做任何事，测试只能检查没有启动任何东西。新增 `a_dropped_done_is_not_a_timeout`。启动测试的超时从 10 s 改为 30 s：调试构建在 CI 上第一次启动较慢，超时只防卡住（「测试」已改）。
- **窗口不在前台时屏障更晚。** 原文按 60 Hz 估计屏障最多晚 33 ms；GPUI 对非前台窗口按 30 fps 请求帧，实测晚约 90–100 ms（「已知限制」已改）。只推迟后台初始化，不影响首帧。

第 2 步：

- **`Profile::plugins` 同时返回插件个数。** 原文返回 `PluginSet`。`PluginSet` 不公开个数，`plugins assembled` 的 `plugins` 字段要它，所以改为返回 `(PluginSet, usize)`，不为此改 kernel（「接口的变化」的签名照此理解）。
- **test profile 在 `startup` 里直接跳过装配。** `startup::assemble` 对 test profile 不建 `Resources`、不读环境变量，返回空集合；`Profile::Test.plugins` 只在 `every_profile_graph_validates` 里调用。
- **`QuitSteps` 要 `Sync`，`quit::run` 接收 `Arc<dyn QuitSteps>`。** 短命线程 `lectern-quit-step` 要持有这些步骤。线程建不起来时记 warn `could not start a quit step` 并跳过这一步：watcher 的线程和缓存的连接随进程退出结束，只丢访问时间。
- **启动测试一次只运行一个。** 7 个启动测试并行时，本机 10 次里有 2 次有测试 30 s 没有退出。main 上的二进制在同样的并行启动下也会卡住（混合启动 120 次，两种二进制各 3 次），所以不是装配引起的：窗口都开在屏幕中央同一位置，被完全遮住的窗口 macOS 不给它画帧，首帧画不出，启动屏障不放开。测试从 5 个增加到 7 个之后更容易出现。`.config/nextest.toml` 把 `package(lectern-app) & binary(startup)` 放进 `max-threads = 1` 的测试组，之后连续 11 次通过，耗时约 2.6 s。

`ui` 接入：

- **test profile 的 `ui` 用 `init_fixed`，由测试自己调用。** `run` 只有 desktop profile，`lectern_ui::init` 在它里面；test profile 没有入口函数，`#[gpui::test]` 在 `window::open` 之前调用 `init_fixed`（ui 设计「对其他 crate 的要求」：test profile 用 `init_fixed`），`release` 里的 `ready` 随后是空操作。
- **`appearance_change_before_ready_does_not_touch_the_theme` 不检查 `Palette` 的切换**，原因见 [ui 设计「与设计的差异」](ui.md#与设计的差异)。
