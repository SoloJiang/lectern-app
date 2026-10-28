# app 设计

状态：已实现（第 1 步，[#203](https://github.com/SoloJiang/lectern-app/pull/203)）。前置条件 A（[#201](https://github.com/SoloJiang/lectern-app/pull/201)）、B（[#202](https://github.com/SoloJiang/lectern-app/pull/202)）已实现。第 2 步（插件装配，[#205](https://github.com/SoloJiang/lectern-app/issues/205)，见「装配」）已实现（[#210](https://github.com/SoloJiang/lectern-app/pull/210)）。`workspace` 的装配（[workspace 设计](workspace.md)「验收」第 3 步，见「workspace 的装配」）已实现，`usage` 与 `project_home` 的装配（[#247](https://github.com/SoloJiang/lectern-app/pull/247)，见「usage 与 project_home 的装配」）已实现，`welcome` 的装配（[#29](https://github.com/SoloJiang/lectern-app/issues/29)，见「welcome 的装配」）和 UI 探针的 probe profile（[#6](https://github.com/SoloJiang/lectern-app/issues/6)，见「ui_probe 的装配」）已实现；`review_view` 的 `review-view` 插件已装配（[#15](https://github.com/SoloJiang/lectern-app/issues/15)，见「review_view 的装配」），`review-view-handoff` 等 `dyn Reader`、`dyn CodeIntel` 有提供者后再装。测量构建（`bench-probe`，[bench 设计](bench.md)第 5 步的 5a，[#7](https://github.com/SoloJiang/lectern-app/issues/7)，见「测量构建的装配」）已实现。实现与原设计的差别见「与设计的差异」。

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
  - `bench-probe` feature、`Driver`、测量构建的数据目录：见「测量构建的装配」（[bench 设计](bench.md)第 5 步，[#7](https://github.com/SoloJiang/lectern-app/issues/7)）。headless profile（命令行，没有窗口）不在 bench 第 5 步：进程级基准测的都是有窗口的 `lectern`，headless 随第一个命令行入口加入。probe profile（`ui-probe` feature，UI 探针）：见「ui_probe 的装配」（[ui_probe 设计「对其他 crate 的要求」](ui_probe.md#对其他-crate-的要求)第 3 项，[#6](https://github.com/SoloJiang/lectern-app/issues/6)）。
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
    // `Probe`（`#[cfg(feature = "ui-probe")]`）：插件与 `Desktop` 相同，多一个 UI 探针，见「ui_probe 的装配」。
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
| 5 | 主线程 | `Application::new().run(…)`：`lectern_ui::init(cx, strings)`（在 `set_menus` 之前：内置的两份 `Palette`、减弱动态效果、登记 `platform` 的变化通知，不读系统色，[ui 设计「初始化」](ui.md#初始化)）、设置应用菜单、绑定 ⌘Q、建骨架窗口（`open_window` 的回调里 `lectern_ui::attach(window, cx)`，`Subscription` 放在根视图 `Skeleton` 里；desktop 再对窗口设 frame autosave 名 `"welcome"`，见「workspace 的装配」）；`Application::with_assets(lectern_ui::IconAssets)` 在 `run` 之前，GPUI 的资源来源只能在这里装 | GPUI 和 AppKit 自己的初始化；`init` 只做内存操作；frame autosave 名是 AppKit 内存里 user defaults 的一次读取，不读 Lectern 的文件 |
| 6 | 主线程 | 首帧画出；根视图第一次 `render` 时登记启动屏障（下节） | 否 |
| 7 | 主线程 | `startup::release`：`Early::start`、`lectern_ui::ready`（`gpui_component::init`、读两套系统色、投影到 gpui-component 的主题；在 `Kernel::start` 之前，插件挂载视图时 `ui` 已就绪）、`Executor::new`、`Cache`、`Watcher`、`workspace::install`（把骨架窗口的根视图换成 `Workspace`，启动主线程队列）、`Kernel::start`；最后在 executor 上提交读 `window.json` 的任务（「workspace 的装配」） | 建 `lectern-log` 线程和 kernel 线程，立即返回；`ready` 约 2–3 ms 的主线程内存操作，不做 IO；读文件在 `Background` 任务里 |
| 8 | `lectern-log` | 建日志目录、清理、写文件头和首帧前缓冲 | 是 |
| 9 | kernel 线程 | 读清单、激活 `AfterFirstFrame` 插件（本 issue 里没有插件） | 由插件决定 |

第 4 步的 `platform::system::os_version()` 不在首帧前调用：它在第 7 步填 `Header` 时调用，是一次 `sysctl`，不读文件。

### 启动屏障

kernel 设计规定 desktop profile 在第一帧显示之后放开屏障（[kernel 设计「启动」](kernel.md#启动)）。精确的显示时刻要在 Metal drawable 上登记 `addPresentedHandler`，锁定的 GPUI 不暴露 drawable；`platform::frame::on_first_presented` 用方法替换取得它，只在测量构建里有（[ADR 0048](../adr/0048.md)）。`app` 因此在所有构建里都用 GPUI 的帧回调：

- desktop：根视图第一次 `render` 时调用 `window.on_next_frame`，回调里再调用一次 `window.on_next_frame`，第二个回调里调用 `startup::release`。GPUI 在一次帧请求（display link 的回调）的开头执行上一帧登记的回调，所以屏障在首帧画出之后的第二次帧请求时放开。首帧的命令缓冲在第一次帧请求的末尾提交，在下一个垂直同步显示；第二次帧请求时它已经显示。比精确的显示时刻最多晚约 2 个刷新周期（60 Hz 下约 33 ms；窗口不在前台时 GPUI 按 30 fps 请求帧，更晚，见「已知限制」），只推迟后台初始化，不计入冷启动（冷启动的终点是首帧的显示时刻，由 bench 单独测量）。
- test：窗口建好后立即放开。
- 测量构建（`bench-probe`）同样用帧回调放开。探针只用 `on_first_presented` 向运行器报告首帧的显示时刻（[bench 设计「需要其他 crate 提供的接口」](bench.md#需要其他-crate-提供的接口)），所以测量构建与发布构建的启动顺序相同。

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

`usage` 与 `project_home` 随后加入（「usage 与 project_home 的装配」）；`code_intel` 不在本 issue，它的前置条件（第 5 步）完成后加进这张表。

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
    /// desktop：上表的四个插件，`workspace` 加入后是五个（「workspace 的装配」），`usage`、`project-home`、`project-home-usage` 加入后是八个（「usage 与 project_home 的装配」）；test：空集合。不做 IO。
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
  - `startup_writes_a_header_and_an_end_record` 加检查：`plugins assembled` 在 `first frame drawn` 之后、`plugins` 为 4（`workspace` 加入后为 5，`usage`、`project_home` 加入后为 8，见「usage 与 project_home 的装配」）。
  - `quit_shuts_the_services_down_in_time`：`--quit-after-start` 时日志里没有 `kernel shutdown timed out`、`executor shutdown timed out`。
  - `home_path_stays_out_of_the_log`（上一节）。
  - 每个启动测试的 `HOME` 都是临时目录，并去掉 `CLAUDE_CONFIG_DIR`、`CODEX_HOME`（`AgentRoots::from_env` 优先读它们），不读本机的会话目录。
- 手动：空闲内存（「性能」）。

## workspace 的装配

[#215](https://github.com/SoloJiang/lectern-app/issues/215)，[workspace 设计「对其他 crate 的要求」](workspace.md#对其他-crate-的要求)的 E。`workspace` 是 desktop profile 的第 5 个插件；它的根视图在启动屏障放开时替换骨架窗口的根视图，窗口状态文件和 frame autosave 名由 `app` 管。test profile 不装，也不调用 `install`。

### 插件与资源

| 插件 ID | 构造 | 提供 | 依赖 |
|---|---|---|---|
| `workspace` | `WorkspacePlugin::new(host)`，`host` 是 `workspace::install(window, deps, cx)` 的返回值（`Resources::workspace`） | `dyn Contributions` | `dyn Projects` |

- `welcome`、`welcome-discovery`（`lectern_welcome::plugins(WelcomeDeps { executor, data_dir: app_data.join("welcome"), version })` 返回的两个插件，逐个放进 `PluginSet::with`）在 `workspace` 之后装配，见「welcome 的装配」。`project_home` 和 `usage` 的装配见「usage 与 project_home 的装配」。
- `install` 的位置：`startup::release` 里，`lectern_ui::ready(cx)` 之后、`Kernel::start` 之前，在 `Cache`、`Watcher` 建好之后、构造插件之前（`resources` 检查完 `HOME`、`app_cache()`、`app_data()` 才调用它）。`home`、`app_cache()`、`app_data()` 任一个为 None 时不装配插件，也不调用 `install`：窗口保持中性的骨架。正常启动时 `install` 总会运行，`welcome` 已装配，首次启动和没有项目时显示 Welcome；`project_home` 的视图合并之前，恢复出来的项目没有基底屏幕，窗口显示空的 `Workspace`（内容区是 `workspace-screen-missing` 的文字），这是过渡状态（workspace 设计「与设计的差异」第 3 步）。`release` 在骨架窗口的帧回调里运行，窗口正被 GPUI 借着，`install` 自己把 `replace_root` 推到这次更新结束时做。
- `startup::arm` 的第二个帧回调把窗口句柄交给 `release_in(Some(window), cx)`；`release(cx)` 是 `release_in(None, cx)`，test profile 用它。没有窗口句柄的 desktop 屏障不装配（记 warn `plugins not assembled`，`missing` 为 `window`），只在直接调用 `release` 时出现。
- `Started::shell`（`Option<Shell>`）：`Shell { host, window_state }`，`Quit` 用 `host` 问离开守卫，Dock 重开用两者。

### 窗口状态与 frame autosave 名

`src/window_state.rs`。`workspace` 在主线程上、没有 `cx` 的回调里报告 `WindowState` 的变化，所以回调只往一个通道发消息，由 `WindowStateLink::start` 在主线程上开的前台任务处理：

- frame autosave 名：`open_window` 的回调里（首帧之前）对 desktop 的骨架窗口设 `"welcome"`（`platform::window::set_frame_autosave_name`，AppKit 内存里的 user defaults，不读 Lectern 的文件，ADR 0017 允许的首帧前唯一的偏好读取；失败时 `platform` 记 warn，窗口沿用打开时的 frame）。任务收到 `WindowState` 后，`project` 有值而当前名是 `"welcome"` 时改设 `"project"`，反过来亦然；窗口已经关闭时什么也不做。test profile 不设名（GPUI 的测试窗口没有 `NSWindow`）。
- 写：任务在收到最后一次变化 500 ms 之后，把状态序列化成 JSON，在 executor 的 `Background` 任务里写 `app_data()/workspace/window.json`（只建 `workspace/` 这一级目录，写同目录的 `window.json.tmp` 再 `rename`）。两次写不重叠（任务等上一次完成）。写失败只记 warn（`step`、`io` 的种类，不记路径）。退出前 500 ms 内的最后一次变化不会写出（「已知限制」）。
- 读：屏障放开、`Kernel::start` 之后，executor 的 `Background` 任务读同一个文件，解析成 `WindowState`，`version` 认得时调 `host.restore(state)`（任何线程）。没有文件：什么也不做。读不出或不是 JSON、`version` 不认得：记 warn 后忽略，下一次变化覆盖它。
- Dock 重开：`window::reopen` 按 `Shell` 里最后一次报告的状态选名字（有项目 `"project"`，否则 `"welcome"`）开骨架窗口，通知任务换窗口，再 `host.attach(window, cx)`。屏障放开之前重开：没有 `Shell`，与第一个窗口一样等屏障。

### 里程碑与帧记录

`WorkspaceDeps::on_milestone` 和 `on_frames` 现在是空函数：bench 的探针（`bench-probe` feature、`Milestone`、`FrameStats`、`Driver`，[bench 设计](bench.md)第 5 步）还没有实现，没有可转交的对象。`platform::frame` 也还没有，`FrameRecord::frames` 总是空（workspace 设计「已知限制」）。探针实现时，`app` 在 `startup::assemble` 这一处把 `workspace::Milestone`（`services::workspace::Milestone`）映射成 bench 的 `Milestone` 交给探针的 `milestone`，把 `FrameRecord` 转成 `FrameStats` 交给 `frames`，并让 `Driver` 的滚动、动画、输入场景调 `host.record_frames`。

### 退出顺序的变化

`Quit` 在 `lectern-quit` 线程启动之前，有 `Shell` 时先 `host.ask_leave(LeaveScope::App, done)`：

```
Quit（主线程）
  ├─ 设退出标记，记 quit requested
  ├─ host.ask_leave(App, done) ── 主线程的前台任务等答复（不阻塞）
  │     done(true)：继续下面的顺序
  │     done(false)：记 info `quit refused by a leave guard`，去掉退出标记；什么也不做，⌘Q 可以再按
  └─ lectern-quit 线程：Kernel::shutdown → … → Logging::shutdown（「退出顺序的变化」）
```

- 询问期间退出标记已设，再按 ⌘Q 被忽略（守卫自己弹出了确认）。
- `done` 在没有回答就被丢弃时（队列已关闭、守卫丢掉了答复）按通过处理：不能让应用退不出去。
- 没有 `Shell`（test profile、没有装配插件）和屏障放开之前：与之前相同，不问。由系统发起的退出不经 `Quit`，也不问（「已知限制」）。

### 日志与诊断

target 为 `app`。

- info `quit refused by a leave guard`（无字段）。
- warn `window state not written`：`step`（`encode`、`create_dir`、`create`、`write`、`rename`），`io`（`Value::io`，除 `encode` 外）。
- warn `window state unreadable`：`reason`（`io` 或 `json`），`io`（`reason` 为 `io` 时）。warn `window state ignored`：`reason` 为 `version`。
- warn `plugins not assembled` 的 `missing` 多一个值 `window`。
- `platform` 自己记 frame autosave 名没有设上的 warn（`kind`）；`workspace` 的记录见 [workspace 设计「日志与诊断」](workspace.md#日志与诊断)。
- 不记录：`window.json` 的路径、项目路径、窗口标题。金丝雀：`tests/startup.rs` 的 `window_state_stays_out_of_the_log`（含金丝雀的项目路径写在 `window.json` 里，`version` 为 1 和 2 各启动一次，trace 级别，日志里没有金丝雀，也没有 `window.json`）和 `home_path_stays_out_of_the_log`（同样检查 `window.json`）；`src/tests/window_state.rs` 的 `a_failed_write_is_logged_without_the_path`、`restore_ignores_what_it_cannot_use`。

### 测试

- `profile`：`every_profile_graph_validates` 成了 `#[gpui::test]`：`WorkspaceHost` 只能由 `install` 在窗口里建，测试用 `init_fixed` 和一个空的测试窗口装它；desktop 的插件数为 5，临时目录之后仍是空的。
- `window_state`（`src/tests/window_state.rs`）：文件函数（`a_written_state_reads_back`、`a_second_write_replaces_the_first`、`a_file_that_is_not_a_state_is_unreadable`：目录里只有 `window.json`，没有临时文件，根目录下没有别的东西）；`#[gpui::test]` `changes_are_written_once_after_500_ms_of_stillness`（测试调度器的时钟，手动的 `Executor`：400 ms 后再来一次变化，再过 400 ms 还没有文件，再过 200 ms 写出最后一个状态）、`the_name_for_a_new_window_follows_the_last_state`、`a_failed_write_is_logged_without_the_path`、`restore_ignores_what_it_cannot_use`（`version` 不认得、不是 JSON、认得、没有文件四种情况的记录）。
- `leave`（`src/tests/leave.rs`，假的 `LeaveAsk`）：`every_guard_allowing_goes_on`、`a_refusal_ends_the_quit_and_the_next_one_asks_again`、`an_answer_from_another_thread_is_waited_for`、`a_dropped_answer_allows`、`the_quit_waits_while_a_guard_has_not_answered`。
- `tests/startup.rs`：`startup_writes_a_header_and_an_end_record` 的 `plugins` 为 5；`window_state_stays_out_of_the_log`；`home_path_stays_out_of_the_log` 加检查 `window.json`。
- 手动（「验收」，workspace 设计第 3 步）：见「实际验证」。

## usage 与 project_home 的装配

[#32](https://github.com/SoloJiang/lectern-app/issues/32)、[#30](https://github.com/SoloJiang/lectern-app/issues/30)，[usage 设计「对其他 crate 的要求」](usage.md#对其他-crate-的要求)的 C 与 [project_home 设计「对其他 crate 的要求」](project_home.md#对其他-crate-的要求)的 D。desktop profile 多三个插件，共八个；test profile 不变。

### 插件与资源

| 插件 ID | 构造 | 提供 | 依赖 |
|---|---|---|---|
| `usage` | `lectern_usage::plugin(UsageDeps { executor, data_dir: app_data.join("usage"), zone })` | `dyn Usage` | `dyn Projects`、`dyn SessionSource` |
| `project-home` | `ProjectHomePlugin::new(ProjectHomeDeps { executor, usage: slot.clone() })` | 无 | `dyn Projects`、`dyn SessionSource`、`dyn Review`、`dyn Contributions` |
| `project-home-usage` | `ProjectHomeUsagePlugin::new(executor, slot)` | 无 | `dyn Usage` |

- `slot` 是 `UsageSlot::default()`，`profile::desktop` 里建一个，克隆给两个 `project_home` 插件。
- `zone` 是 `profile.rs` 里的 `SystemZone`：`LocalZone::offset_at(unix_ms)` 取 `platform::time::utc_offset(unix_ms.div_euclid(1000))`（向下取整，不用截断除法，见 [platform 设计「本地时区的偏移」](platform.md#本地时区的偏移)的「规则」）。`utc_offset` 返回 None 时按 UTC（0）处理：毫秒时间戳除以 1000 后总有偏移，这一支不会走到，所以不记日志。构造 `SystemZone` 不做 IO；第一次 `localtime_r`（读时区文件）发生在 `usage` 的后台任务里，不在 UI 线程、不在首帧之前。
- `UsageDeps::data_dir` 只拼路径，不创建；`usage.db` 由 `usage` 在 `started` 提交的后台任务里打开，目录在第一次打开时创建（[usage 设计「数据流与线程」](usage.md#数据流与线程)）。
- 现在的 `UsageDeps` 只有 `executor`、`data_dir`、`zone`。usage 设计「依赖」里的 `watcher`、`git_ceilings` 要到 usage 的第 2、5 步才加；`Resources::git_ceilings` 已在，加字段时 `app` 照 `review` 的做法传入（「与设计的差异」）。
- `project-home` 现在不登记任何贡献（它的视图随 project_home 的第 2 步）。装配之后，打开（或从 `window.json` 恢复）一个项目时它会取概览的关注、取用量的关注，并在后台合成快照，而窗口里没有视图读这些快照：这是过渡状态，视图合并后消失（「已知限制」）。没有项目打开时（首次启动）这两个插件什么也不做。

### 性能

- 冷启动：首帧之前没有新增的工作。`lectern_usage::plugin`、`ProjectHomePlugin::new`、`ProjectHomeUsagePlugin::new`、`UsageSlot::default()` 只保存依赖、分配内存，在 `release` 里、`Kernel::start` 之前运行，不做 IO、不建线程；三个插件都是 `AfterFirstFrame`。
- 首帧之后：`usage` 的 `activate` 只建空表、提供服务（< 1 ms），`started` 里提交 `Background` 任务打开数据库（< 10 ms）。读取、补读的节奏由 usage 设计规定（批与批之间不持有守卫）。
- 空闲内存（ADR 0017：60 MB）：usage 设计给出的份额是两个 SQLite 连接的页缓存 ≤ 2 MiB 加常驻 ≤ 2 MB。本机的 `phys_footprint` 没有测（「实际验证」）。

### 日志与诊断

- `plugins assembled` 的 `plugins` 为 8（`welcome` 装配之后为 10，见「welcome 的装配」）。
- 其余记录由 `usage`、`project_home` 自己写（target 为 `usage`、`project_home`），见各自设计的「日志与诊断」。`SystemZone` 不写日志。

### 测试

- `every_profile_graph_validates`：desktop 的插件数为 8（现为 10，见「welcome 的装配」），图校验通过（`usage` 提供的 `dyn Usage` 满足 `project-home-usage`，`workspace` 提供的 `dyn Contributions` 满足 `project-home`）；临时目录之后仍是空的。
- `zone_rounds_milliseconds_down_to_seconds`、`zone_without_an_offset_is_utc`（`src/profile.rs`）：毫秒到秒的向下取整（负数也是）和 None 时为 0；偏移本身由 `platform` 的 `utc_offset_*` 测试覆盖。
- `tests/startup.rs`：`startup_writes_a_header_and_an_end_record` 的 `plugins` 为 8（现为 10）。

## welcome 的装配

[#29](https://github.com/SoloJiang/lectern-app/issues/29)，[welcome 设计「对其他 crate 的要求」](welcome.md#对其他-crate-的要求)的 C。`welcome` 的视图（第 2 步）已合并，屏幕载荷是 `ui::contrib::Screen`，`UiPayloads` 接受它。

### 插件与资源

| 插件 ID | 构造 | 提供 | 依赖 |
|---|---|---|---|
| `welcome` | `lectern_welcome::plugins(WelcomeDeps { executor, data_dir: app_data.join("welcome"), version })` 的第一个 | 无 | `dyn Contributions`、`dyn Projects` |
| `welcome-discovery` | 同一次 `plugins` 调用的第二个 | 无 | `dyn Projects`、`dyn SessionSource` |

- `version`（[#283](https://github.com/SoloJiang/lectern-app/issues/283)，[welcome 设计「对其他 crate 的要求」](welcome.md#对其他-crate-的要求)E）是 `Some(APP_VERSION)`：`profile.rs` 里的常量，编译期用 `concat!(env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"))` 从包版本截取「主版本.次版本」（不解析字符串，不读环境）。Welcome 的副标题用它（`welcome-subtitle` 的 `$version`）。
- 两个插件在 `workspace` 之后逐个放进 `PluginSet::with`，共用 `plugins` 建的一个 `WelcomeModel`。`WelcomeDeps::data_dir` 只拼路径，不创建；`recent.json` 由 `welcome` 在 `started` 提交的后台任务里读，第一次写时才建 `welcome/` 目录（`create_dir_all`）。
- 会话来源（`agents` 的两个插件）在同一张表里，`welcome-discovery` 的 `requires` 有提供者。没有会话来源的 profile 必须同时不装 `welcome-discovery`；`Profile::Test` 什么也不装。
- `Resources::app_data` 非可选；`app_data()` 为 None 时 `app` 不装配任何插件（「workspace 的装配」），所以这里不处理没有目录的情况。

### 性能

- 冷启动：首帧之前没有新增的工作。`lectern_welcome::plugins` 只建空的 `WelcomeModel`（保存依赖，不做 IO），在 `release` 里、屏障放开之后运行。`welcome` 的 `activate` 登记 `Screen` 和 `KeyBindings`，< 1 ms；`recent.json` 在后台任务里读。
- 空闲内存（ADR 0017：60 MB）：Welcome 屏幕的视图和两个列表的份额见 [welcome 设计「性能」](welcome.md#性能)。本机的 `phys_footprint` 没有测（「实际验证」）。

### 日志与诊断

- `plugins assembled` 的 `plugins` 为 10。
- 其余记录由 `welcome` 自己写（target 为 `welcome`），见 welcome 设计的「日志与诊断」。装配不写日志，也不记路径。

### 测试

- `every_profile_graph_validates`：desktop 的插件数为 10，图校验通过（`workspace` 提供的 `dyn Contributions` 满足 `welcome`，`agents` 的会话来源满足 `welcome-discovery`）；临时目录之后仍是空的（`plugins` 不建 `welcome/`）。
- `tests/startup.rs`：`startup_writes_a_header_and_an_end_record` 的 `plugins` 为 10。
- 手动（welcome 设计「验收」第 2 步）：首次启动显示 Welcome，见「实际验证」。

## ui_probe 的装配

[#6](https://github.com/SoloJiang/lectern-app/issues/6)，[ui_probe 设计「对其他 crate 的要求」](ui_probe.md#对其他-crate-的要求)第 3 项。只在 `ui-probe` feature 下编译。

- **feature。** `ui-probe`（不在 `default` 里）打开可选依赖 `lectern-ui-probe`。`just check`、`just test` 带 `--all-features`，所以会编译和检查这条路径；不带 feature 的 `cargo build -p lectern-app` 不含探针（`cargo tree --locked -p lectern-app -e normal,build,features --target aarch64-apple-darwin` 里没有 `lectern-ui-probe`，也没有 `"test-support"`）。
- **`Profile::Probe`。** 插件集合与 desktop 相同（10 个），启动屏障、frame autosave 名也相同（`Profile::has_real_window`）。`run` 在首帧之前只读环境变量 `LECTERN_UI_PROBE_DIR`（`std::env::var_os`，内存操作，不读文件；空值当作没有设置）：设置了就是 `Probe`，`Startup::probe(dir)` 存下目录；否则是 `Desktop`。
- **启动。** `startup::release_in` 在 `Kernel::start` 之后、`Started` 设好之前调用 `Probe::start(cx, Config::new(dir))`（主线程，不阻塞）；失败时记 warn `probe start failed`（`reason`：`dir-not-private`、`bind`、`thread`；`error` 有 IO 错误时带，没有路径），应用照常运行。首帧之前没有探针的代码运行：不检查目录、不建线程、不 bind。
- **退出。** `Started::probe: Option<Arc<Probe>>`。`lectern-quit` 线程的第一步（`QuitSteps::kernel` 的实现）先 `Probe::shutdown`（不阻塞，删除 `probe.sock`），再 `Kernel::shutdown`；`quit` 请求靠名字 `lectern::Quit` 分发 `quit.rs` 里的 `Quit` Action，与 ⌘Q 同一条路径。
- **`just probe`。** 构建带 `ui-probe` 的调试版，`mktemp -d` 建探针目录（0700），临时 `HOME` 在其下的 `home/`，去掉 `CLAUDE_CONFIG_DIR`、`CODEX_HOME`、`LECTERN_LOG`，后台启动并打印 `export LECTERN_UI_PROBE_DIR=…` 与 pid；不读真实的 `HOME`。
- **日志。** `probe start failed`（target `app`）；`probe listening`、`probe stopped` 等由 `ui_probe` 写（target `ui_probe`）。`plugins assembled` 的 `profile` 字段在 probe profile 里是 `probe`。
- **测试。** `src/tests/probe.rs`（`#[gpui::test]`）：`probe::start` 对没有目录、不存在的目录（记 `dir-not-private`，路径不在日志里）、私有目录（`probe.sock` 存在，`shutdown` 后消失）；`every_profile_graph_validates` 对 `Profile::Probe`；`tests/probe.rs`（真实进程，需要图形会话，与 `tests/startup.rs` 相同）。

## review_view 的装配

[#15](https://github.com/SoloJiang/lectern-app/issues/15)，[review_view 设计「对其他 crate 的要求」](review_view.md#对其他-crate-的要求)的 C（插件部分；bench 的 `Driver::open(OpenTarget::Session)` 与 `Milestone::DiffShown` 随 review_view 第 3 步的基准）。desktop 与 probe profile 多一个插件，共 11 个；test profile 不变。

| 插件 ID | 构造 | 提供 | 依赖 |
|---|---|---|---|
| `review-view` | `lectern_review_view::plugins(executor)` 的第一个 | 无 | `dyn Contributions`、`dyn Review`、`dyn SessionSource` |

- `plugins(executor)` 同时返回 `review-view-handoff`。它依赖 `dyn Reader`、`dyn CodeIntel`、`dyn Usage`；现在的 desktop 表里没有插件提供前两项，放进 `PluginSet` 会让图校验报 `MissingProvider`，所以 `profile::desktop` 丢掉它。审阅屏幕不依赖它：交接按钮置灰，不显示会话的用量行（review_view 设计「状态与异常」）。`file_reader` 与 `code_intel` 装配之后，同一处把它放进表里。
- 依赖都在表里：`workspace` 提供 `dyn Contributions`，`review` 提供 `dyn Review`，`agents` 的两个插件提供 `dyn SessionSource`。
- 冷启动：`plugins` 只建空的共享状态，不做 IO，在 `release` 里、屏障放开之后运行；`activate` 在首帧之后，只登记屏幕、检查器页和快捷键。
- 日志：`plugins assembled` 的 `plugins` 为 11。其余记录由 `review_view` 写（target `review_view`）。
- 测试：`every_profile_graph_validates` 的 desktop 与 probe 插件数为 11；`tests/startup.rs` 的 `plugins` 为 11，并检查 `first frame drawn` 之前没有 target 为 `review_view` 的记录。

## 测量构建的装配

[#7](https://github.com/SoloJiang/lectern-app/issues/7)，[bench 设计](bench.md)第 5 步与「需要其他 crate 提供的接口」的 `app` 一项。只在 `bench-probe` feature 下编译。目的：`lectern-bench` 的进程级基准（`app/cold_start`、`app/cold_start_purged`、`app/idle_empty`、`plugin/*`）测的是与发布构建同一条启动路径的 `lectern`。差别是探针，和有探针时不设 frame autosave 名（「数据目录」）。

### feature 与依赖

- `bench-probe = ["dep:lectern-bench", "lectern-bench/probe"]`，不在 `default` 里。`lectern-bench` 是 tool crate，`app` 只能以不在 `default` 里的可选依赖引用它（架构规则检查第 2 条），与 `lectern-ui-probe` 相同。`lectern-bench/probe` 打开 `lectern-platform/bench-hooks`，所以方法替换只在这个构建里有（[ADR 0048](../adr/0048.md)，检查第 10 条）。
- `just bench` 在 macOS 上构建 `cargo build --release --locked -p lectern-app --features bench-probe`，`LECTERN_BENCH_APP` 指向它（[bench 设计「`just bench` 与 CI」](bench.md#just-bench-与-ci)）。`just check`、`just test` 带 `--all-features`，编译这条路径。
- 不带 feature 的构建里没有 `lectern-bench`：`cargo tree --locked -p lectern-app -e normal,build,features --target aarch64-apple-darwin` 的输出里没有 `lectern-bench`、`"bench-hooks"` 和 `"test-support"`。

### 选择与启动

不新增 profile：插件集合、启动屏障、退出顺序都与 desktop 相同，探针是附加在 desktop 上的。

| 位置 | 线程 | 做什么 | 首帧之前的代价 |
|---|---|---|---|
| `run`，`install_early` 之后、`application()` 之前 | 主线程 | `Probe::from_env()`：只读 `LECTERN_BENCH_SOCKET`、`LECTERN_BENCH_PLUGINS`。为 Some 时 `watch_first_frame()`（一次方法替换，≤ 100 µs，[platform 设计「帧的时刻」](platform.md#帧的时刻)） | 两次环境变量读取；有 socket 时一次方法替换。不做 IO，不建线程 |
| `app.run` 的回调里 | 主线程 | `Startup::bench(Arc<Probe>)` 存下探针 | 内存 |
| `open_window` 的回调里 | 主线程 | 有探针时不设 frame autosave 名（下一节） | 少一次 user defaults 读取 |
| `startup::release_in`，代替 `Kernel::start(set)` | 主线程 | `probe.start_kernel(set)`：加入 `bench-probe` 插件，按 `LECTERN_BENCH_PLUGINS` 裁剪，启动 kernel | 首帧之后 |
| 同上，紧接其后 | 主线程 | `probe.attach(kernel.clone(), Arc::new(BenchDriver { … }))` | 首帧之后 |

- 没有设置 `LECTERN_BENCH_SOCKET` 时 `from_env` 为 None，测量构建与发布构建走完全相同的路径（「启动屏障」已写明测量构建同样用帧回调放开屏障）。
- 注册首帧回调出错时探针自己连 socket、发 `Error { message: "hook <名字>" }`，以退出码 3 退出（bench 设计「探针」）；`app` 不处理。
- `bench-probe` 插件不计入 `plugins assembled` 的 `plugins`：那个计数在 `start_kernel` 之前记。

```rust
// src/bench.rs（#[cfg(feature = "bench-probe")]）
/// 运行器的请求到主线程的通道。`Driver` 的两个方法在探针线程上调用，只发消息，不阻塞。
pub struct BenchDriver { requests: futures::channel::mpsc::UnboundedSender<BenchRequest>, probe: Arc<Probe> }

enum BenchRequest { Open(PathBuf), Quit }

impl Driver for BenchDriver {
    /// `Project(path)`：发 `BenchRequest::Open(path)`，主线程上 `shell.host.hub().dispatch(WorkspaceCommand::OpenProject(path))`，
    /// 与用户在 Open Folder 对话框里选了文件夹之后同一条路径（`workspace` 的 `view.rs`）。`plugin/project/resident_*`（第 5 步）要它。
    /// 没有 `Shell`（没有装配插件）时发 `probe.error("open project without a workspace")`。
    /// 其余种类发 `probe.error("open <种类> unsupported")`，运行器把这个基准记为 `error`；
    /// 端到端基准（`app/open_project` 的里程碑、`File`、`Diff`、`Worktree`、`Scroll`、`Animate`、`Input`）随自己的 PR 在这里加上。
    fn open(&self, target: OpenTarget);
    /// 发 `BenchRequest::Quit`；主线程的前台任务收到后调用 `quit::quit(cx)`，与 ⌘Q 同一条退出路径。
    fn quit(&self);
}

/// `release_in` 里调用：开收请求的前台任务，`attach`。主线程，不阻塞。
pub fn attach(probe: &Arc<Probe>, kernel: Arc<Kernel>, cx: &mut App);
```

- `Startup` 多一个 `#[cfg(feature = "bench-probe")] bench: Option<Arc<Probe>>`，`Started` 不需要它：探针线程自己持有它要的东西，退出时随进程结束（运行器在 `Quit` 之后等进程退出）。
- `Quit` 走 `quit::quit`：离开守卫、kernel、watcher、executor、cache、日志依次关闭，最后 `cx.quit()`，退出码 0。运行器把非 0 的退出码记为错误，所以退出顺序的回归在基准里也会出现。
- `ui-probe` 与 `bench-probe` 可以同时打开（`--all-features`）：两个探针互不相干，各看各的环境变量。

### 数据目录

- 运行器每次启动都给一个新的临时 `HOME`，并删除 `LECTERN_LOG`、`CLAUDE_CONFIG_DIR`、`CODEX_HOME`（bench 设计「进程调度」的 `session`）。`platform::dirs::app_data`、`app_cache`、`logs` 只读 `HOME`（和 `XDG_*`），所以缓存、`window.json`、日志、各插件的数据都落在这个临时目录里，会话结束后删除。`app` 不需要另外的开关，也不新增环境变量。
- frame autosave 名是例外：它存在 AppKit 的 user defaults 里，cfprefsd 按登录用户的真实主目录读写，不看 `HOME`。有探针时 `app` 不对窗口设 frame autosave 名，窗口总是 820 × 520 pt 居中：基准不读也不改用户的窗口位置，每次启动画的像素也相同。`window_state` 的任务在有探针时同样不改名。代价：`app/cold_start` 的样本不含这一次读取，比用户的启动少做一点。5a 的「实际验证」用临时插入的计时测出 `set_frame_autosave_name` 在首帧之前的耗时（临时 `HOME`，10 次的中位数）并写明；超过 1 ms 时告诉产品负责人，由产品负责人决定是否让基准也设名（那样会读写用户真实的窗口位置）。
- 系统首选语言（`platform::system::preferred_languages`）只读，照常读取。

### 里程碑与帧

第 5 步的基准不用里程碑和逐帧记录，`WorkspaceDeps::on_milestone`、`on_frames` 仍是空函数。转交它们要报告协议的 `Milestone`、`FrameTimes` 带上时刻的来源（[ADR 0052](../adr/0052.md)，bench 设计「报告协议」写明随它们的使用方加入），所以随第一个把里程碑或帧时间预算改为生效的 PR 一起做：那时 `app` 在 `startup::assemble` 里把 `workspace::Milestone` 映射成 `wire::Milestone` 交给 `probe.milestone`，把 `FrameRecord` 里的 `FrameRecording`（`problems.is_clean()` 时）转成 `FrameStats` 交给 `probe.frames`，否则交 `probe.error`（消息格式见 bench 设计「探针」），`Driver::open` 加上 `Scroll`、`Animate`、`Input`。

### 性能

- 发布构建：0，没有这些代码。
- 测量构建、没有 socket：首帧之前多两次环境变量读取。
- 测量构建、有 socket：首帧之前多一次方法替换（≤ 100 µs，计入 `app/cold_start` 的样本，与 `baseline/empty_window_cold_start` 相同），少一次 frame autosave 的读取。首帧之后多一个探针线程和 `bench-probe` 插件；探针线程的内存计入 `app/idle_empty`，与空窗口基线一样。

### 日志与诊断

- 探针本身不写日志：它的错误经 socket 交给运行器（bench 设计「状态与异常」）。
- `app` 新增 info `bench probe attached`（target `app`，无字段），在 `startup barrier released` 之前。
- 不记录：socket 路径、`LECTERN_BENCH_PLUGINS` 的值、临时 `HOME`。金丝雀：`tests/bench_probe.rs` 的 `bench_socket_stays_out_of_the_log`（socket 所在目录名含金丝雀，运行一次完整的会话，日志里没有它）。

### 测试

- `src/tests/bench.rs`（`bench-probe`）：`no_frame_autosave_name_with_the_bench_probe`（选名字的纯函数：有探针时骨架窗口和 `window_state` 都得到 None，没有时与 desktop 相同）；`#[gpui::test]` `a_quit_request_takes_the_quit_path`（`BenchDriver::quit` 之后主线程调用 `quit::quit`：用假的 `QuitSteps` 看到 kernel、executor、日志的顺序）；`open_project_takes_the_open_folder_path`（`Open(Project(path))` 之后 `workspace` 的 hub 收到 `OpenProject(path)`）；`open_is_reported_as_unsupported`（`Open(Scroll)` 发出 `Error`，消息以 `open ` 开头）；`open_project_without_a_workspace_is_an_error`。另有 `only_a_project_is_forwarded_and_the_rest_is_an_error`（`BenchDriver` 只转发 `Project` 和 `Quit`）。`first_frame_plugins_and_quit` 还发 `Open(Scroll)`，应答是 `Error { message: "open scroll unsupported" }`。
- `tests/bench_probe.rs`（`#![cfg(feature = "bench-probe")]`；真实进程，需要图形会话，与 `tests/startup.rs` 相同；测试自己扮演运行器：在临时目录里 bind 一个 Unix 域 socket，临时 `HOME`，带 `LECTERN_BENCH_SOCKET` 启动 `--features bench-probe` 的调试构建）：
  - `first_frame_plugins_and_quit`：收到 `FirstFrame`，`pid` 是启动的进程，`source` 是 `presented` 或 `handler`；发 `Plugins`，应答是 desktop 的插件（不含 `bench-probe`），以 `PluginsEnd` 结束；发 `Epoch`，应答 `Epoch(..)`；发 `Quit`，进程 10 s 内以 0 退出。
  - `a_trimmed_set_becomes_ready`：`LECTERN_BENCH_PLUGINS=closure+self:project`，收到 `Ready`。
  - `bench_socket_stays_out_of_the_log`（上一节）。
- 不带 feature：`cargo tree` 的检查（「验收」）。

## 性能

- 冷启动：首帧之前只有「启动顺序」的第 1–6 步。新增的工作是参数解析（几个字符串比较）、`install_early`（预算 ≤ 20 µs，`log/install_early`）、语言协商（常数级的内存操作）和 `lectern_ui::init`（预算 ≤ 300 µs，`ui/init`：一次减弱动态效果的读取、两份内置表、登记通知；不读系统色，[ui 设计「性能」](ui.md#性能)），加上骨架窗口的 `lectern_ui::attach`（登记一个外观观察者），合计远小于 20 ms 预算的 1%。首帧之前不建线程、不读写 Lectern 自己的文件、不调用 kernel 和 executor、不运行 `gpui_component::init`（ADR 0017）。
- 空闲内存：首帧之后多出日志的缓冲（logging 设计「性能」：≤ 512 KiB）、kernel 线程和它的栈，以及 `ui` 的 `UiState`（< 8 KiB）和 `gpui_component::init` 建的主题全局（预期 < 1 MB，`ui/idle_memory` 实测，[ui 设计「性能」](ui.md#性能)）。executor 在第一次提交任务之前不建线程。空集合的 kernel 静止后不再唤醒。
- 基准：`app/cold_start`、`app/idle_empty` 已在预算表里，由 bench 第 5 步的进程级基准测量；本 issue 不新增基准。在那之前用「测试」里的 `startup_*` 测试验证顺序：首帧之前没有日志文件。

## 界面

- 窗口：没有打开项目时的独立窗口，820 × 520 pt，居中（[交互规范「窗口结构」](interaction.md#窗口结构)的 Welcome 宽度）。标题栏透明，窗口背景用 `WindowBackgroundAppearance::Blurred`（系统的毛玻璃材质，自动适配浅色和深色模式，不需要自定义颜色），根视图不画任何内容，这就是 ADR 0017 的中性骨架。`ui` 提供骨架控件以后，由 Welcome 和 Overview 的视图替换这个根视图。
- 关闭窗口不退出应用（产品负责人 2026-10-23 决定，见「未决问题」）。点 Dock 图标而且没有窗口时，重新打开骨架窗口（`Application::on_reopen`，在 `run` 之前登记）；`workspace` 装好之后，新窗口按当前状态取 frame autosave 名，`WorkspaceHost::attach` 把根视图换成 `Workspace`（「workspace 的装配」）。
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
| 离开守卫拒绝 `Quit` | 不退出，去掉退出标记，记 info `quit refused by a leave guard`；守卫自己弹出确认 |
| `window.json` 读不出、不是 JSON 或 `version` 不认得 | 记 warn，按没有文件处理；下一次状态变化覆盖它 |
| `window.json` 写失败 | 记 warn，放弃这次写；下一次变化再写 |
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
- 由系统发起的退出不经 `Quit`：kernel 和 executor 不按顺序关闭（kernel 设计「已知限制」），只有日志的结束记录在 `on_app_quit` 的 200 ms 里写出。系统发起的退出也不问离开守卫。
- `window.json` 去抖 500 ms：最后一次变化之后 500 ms 内退出，这次变化不会写出，下次启动回到上一次写出的状态（多半是上一个项目）。系统发起的退出同样。
- 第一次使用 `"project"`、还没有保存过 frame 时，设计要求先把窗口设为 1180 × 760 pt 居中（workspace 设计「对其他 crate 的要求」D）。GPUI 的窗口没有移动接口，`resize` 只改内容尺寸并保持左上角，在 Welcome 居中的窗口上放大到 1180 × 760 会越出屏幕；居中要调 AppKit，属于 `platform`（架构规则 6）。所以现在窗口沿用 Welcome 的大小，用户拖一次，AppKit 就保存在 `"project"` 下。`platform` 补上居中并设尺寸的接口后，`app` 在改名为 `"project"` 之前调用（「与设计的差异」）。
- 屏障比首帧的实际显示最多晚约 2 个刷新周期：精确的显示时刻只在测量构建里有（ADR 0048，「启动屏障」）。窗口不是前台窗口时 GPUI 按 30 fps 请求帧（`inactive_frame_interval` 33 ms），间隔按 33 ms 计；从终端启动时实测首帧画出到屏障放开约 90–100 ms（「实际验证」）。
- `project_home` 的视图合并之前，装配了的 `project-home` 在项目打开（或从 `window.json` 恢复）时会取概览与用量的关注、在后台合成快照，没有视图读它们。没有项目打开时它什么也不做。随 project_home 第 2 步消失。

## 验收

分两步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-app`、`just deny` 通过。

**前置条件 A、B**：各自的 PR，`just test -p lectern-i18n`、`just test -p lectern-platform` 通过。

**第 1 步：启动外壳与日志接入（本 issue 的实现 PR，`Closes #199`）。**

- 「测试」一节的单元测试和启动测试全部通过：`just test -p lectern-app`。
- `rg 'eprintln!|println!|log::' crates/app/src` 没有结果（记录只经 `lectern_logging` 的宏）。
- `cargo tree -p lectern-app -e features -i lectern-logging` 里有 `sink`。
- 手动在 macOS 上运行 `cargo run -p lectern-app`：窗口出现，按 ⌘Q 后进程在 3 s 内退出，`~/Library/Logs/Lectern/` 里本次运行的文件最后一行是 `run end`。
- logging 设计的状态行和「实际验证」写明第 3 项已实现，第 3 步验收里依赖进程级基准的一条（`app/cold_start` 不变慢）随 bench 第 5 步验证。

**测量构建（bench 第 5 步的 5a，[#7](https://github.com/SoloJiang/lectern-app/issues/7)，`Refs #7`）。** 在 [platform 设计](platform.md)「验收」的 B2（`frame::record`）之后。5b（`lectern-bench` 的 `app/*`、`plugin/*` 进程级目标与 `just bench`，`Closes #7`）在它之后，验收见 [bench 设计「验收」](bench.md#验收)第 5 步。

- 「测量构建的装配」「测试」里的测试通过：`just test -p lectern-app`（`--all-features`）。
- `cargo build --release --locked -p lectern-app --features bench-probe` 成功。
- `cargo tree --locked -p lectern-app -e normal,build,features --target aarch64-apple-darwin` 的输出里没有 `lectern-bench`、`"bench-hooks"`、`"test-support"`。
- `python3 scripts/check_architecture.py` 通过（`lectern-bench` 是 `app` 不在 `default` 里的可选依赖）。
- 手动：用测量构建和一个临时 `HOME`、不设 `LECTERN_BENCH_SOCKET` 启动，日志与发布构建相同（有 frame autosave、没有 `bench probe attached`）。
- 本文状态行写明测量构建已实现，填写「实际验证」。

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

`ui` 接入（[#234](https://github.com/SoloJiang/lectern-app/pull/234)，同一台机器，[ui 设计「验收」](ui.md#验收)第 1 步的 `app` 项）：

- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过（32 个 crate）；`rg 'eprintln!|println!|log::' crates/app/src` 没有结果。
- `cargo nextest run --locked -p lectern-app --all-features -E 'not binary(startup)'`：17 个单元测试通过（原有 15 个加新增的 `appearance_change_refreshes_the_skeleton_window`、`appearance_change_before_ready_does_not_touch_the_theme`）。`tests/startup.rs` 的 7 个启动测试本机没有通过：图形会话处于锁屏（`CGSSessionScreenIsLocked` 为 1，用户不在），窗口画出首帧后收不到帧请求，屏障不放开，7 个都在 30 s 后判为失败；接入前一天的本机调试构建同样卡住（「已知限制」）。它们由 macOS CI 运行；`startup_writes_a_header_and_an_end_record` 多检查 `ui initialised` → `first frame drawn` → `ui ready` → `plugins assembled` → `startup barrier released` 的顺序和两条记录的 `accent`，结果以 CI 为准。
- 发布构建（`cargo build --release --locked -p lectern-app`）成功；临时 `HOME` 下的手动运行（空闲内存、`ui initialised` / `ui ready` 的字段与耗时）因为同一个锁屏原因没有做：屏障不放开，日志文件不出现，内存读数没有意义。解锁后按「装配」「性能」的方法补测，写进这里。

`workspace` 的装配（[workspace 设计](workspace.md)第 3 步，同一台机器；完整的数字和没有做的项在 [workspace 设计「实际验证」](workspace.md#实际验证)第 3 步）：

- `just check`、`just deny`、`python3 scripts/check_architecture.py` 通过；`rg 'eprintln!|println!|log::|log_err|warn_on_err' crates/app/src crates/app/tests` 没有结果。
- `just test -p lectern-app -E 'not binary(startup)'`：31 个测试通过（原有 17 个，新增 14 个：`window_state` 9 个，`leave` 5 个；`every_profile_graph_validates` 改成 `#[gpui::test]`，desktop 的插件数为 5）。
- `tests/startup.rs` 的 8 个启动测试本机没有运行（图形会话锁屏，屏障不放开，「已知限制」），结果以 macOS CI 为准。发布构建（临时 `HOME`）的手动运行、冷启动、空闲内存都没有做；解锁后按「装配」「性能」的方法补测，写进这里。

`usage` 与 `project_home` 的装配（[#247](https://github.com/SoloJiang/lectern-app/pull/247)，同一台机器）：

- `just check`、`just deny`、`python3 scripts/check_architecture.py` 通过；`rg 'eprintln!|println!|log::|log_err|warn_on_err' crates/app/src crates/app/tests` 没有结果。
- `cargo nextest run --locked -p lectern-app --all-features -E 'not binary(startup)'`：33 个测试通过（新增 `zone_rounds_milliseconds_down_to_seconds`、`zone_without_an_offset_is_utc`；`every_profile_graph_validates` 的 desktop 插件数为 8，图校验通过，临时目录之后仍是空的）。
- `tests/startup.rs` 没有在本机运行（图形会话锁屏，屏障不放开，「已知限制」）；它的 `plugins` 改为 8，结果以 macOS CI 为准。发布构建（临时 `HOME`）的手动运行、冷启动、空闲内存都没有做，解锁后按「装配」「性能」的方法补测，写进这里。

`welcome` 的装配（[#29](https://github.com/SoloJiang/lectern-app/issues/29)，叠在 [#247](https://github.com/SoloJiang/lectern-app/pull/247) 之上，同一台机器）：

- `just check`、`just deny` 通过；`rg 'eprintln!|println!|log::|log_err|warn_on_err' crates/app/src crates/app/tests` 没有结果。
- `cargo nextest run --locked -p lectern-app --all-features -E 'not binary(startup)'`：`every_profile_graph_validates` 的 desktop 插件数为 10，图校验通过，临时目录之后仍是空的。
- `tests/startup.rs` 没有在本机运行（图形会话锁屏，屏障不放开，「已知限制」）；它的 `plugins` 改为 10，结果以 macOS CI 为准。发布构建（临时 `HOME`）的手动运行（首次启动显示 Welcome）、冷启动、空闲内存都没有做，解锁后按「装配」「性能」的方法补测，写进这里。

`ui_probe` 的装配（[#6](https://github.com/SoloJiang/lectern-app/issues/6)，叠在 `welcome` 的装配之上，同一台机器；完整的数字和没有做的项在 [ui_probe 设计「实际验证」](ui_probe.md#实际验证)第 4 步）：

- `just check`、`just deny` 通过；`rg 'eprintln!|println!|log::|log_err|warn_on_err' crates/app/src crates/app/tests` 没有结果。
- `cargo nextest run --locked -p lectern-app --all-features -E 'not binary(startup) and not binary(probe)'` 通过（含 `Profile::Probe` 的图校验和 `src/tests/probe.rs`）；`cargo tree` 的发布构建检查没有 `lectern-ui-probe`。
- `tests/probe.rs` 和 `tests/startup.rs` 没有在本机运行（图形会话锁屏，屏障不放开，「已知限制」），结果以 macOS CI 为准。`just probe` 起得来，锁屏下探针目录里没有 `probe.sock`（屏障未放开，探针没有启动）。

`review_view` 的装配（[#15](https://github.com/SoloJiang/lectern-app/issues/15)，同一台机器）：

- `just check`、`just deny` 通过。
- `cargo nextest run --locked -p lectern-app --all-features -E 'not binary(startup) and not binary(probe)'` 通过：`every_profile_graph_validates` 的 desktop 与 probe 插件数为 11，图校验通过，临时目录之后仍是空的。
- `tests/startup.rs` 和 `tests/probe.rs` 没有在本机运行（图形会话锁屏，屏障不放开，「已知限制」），结果以 macOS CI 为准。发布构建的手动运行（从项目主页打开一个会话的 Diff）没有做。

测量构建（5a，[#7](https://github.com/SoloJiang/lectern-app/issues/7)，`Refs #7`，同一台机器：macOS 27.0.1，Apple M5 Pro，Rust 1.97.0，图形会话没有锁屏）：

- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过（32 个 crate）；`rg 'eprintln!|println!|log::' crates/app/src` 没有结果。`cargo metadata --format-version 1 --locked --all-features`（和 `--filter-platform x86_64-unknown-linux-gnu`）通过，`Cargo.lock` 已提交。
- `just test -p lectern-app`（`--all-features`）：55 个测试通过，含 `tests/startup.rs`、`tests/probe.rs` 和 `tests/bench_probe.rs` 的真实进程测试（`src/tests/bench.rs` 6 个，`tests/bench_probe.rs` 3 个）。`tests/bench_probe.rs` 加上 `src/tests/bench.rs` 的 9 个连续运行 5 次都通过。
- `cargo build --release --locked -p lectern-app --features bench-probe` 成功。`cargo tree --locked -p lectern-app -e normal,build,features --target aarch64-apple-darwin | grep -E 'lectern-bench|bench-hooks|test-support'` 没有输出；加 `--features bench-probe` 之后有 `lectern-bench` 和 `"bench-hooks"`。
- 手动：测量构建的发布版，临时 `HOME`，不设 `LECTERN_BENCH_SOCKET`，去掉 `CLAUDE_CONFIG_DIR`、`CODEX_HOME`、`LECTERN_LOG`：日志是 `first frame drawn`、`plugins assembled`（`plugins` 为 11）、`startup barrier released` 和 11 条 `plugin activated`，没有 `bench probe attached`，与发布构建相同。
- **`set_frame_autosave_name` 在首帧之前的耗时**（「数据目录」要求的实测）：在 `window::open_named` 里临时插入 `Instant` 计时和 `eprintln!`（补丁已还原，不在提交里），发布构建（不带 `bench-probe`，与用户的启动相同），每次启动一个新的临时 `HOME`，等 2.5 s 后结束进程；frame autosave 在 AppKit 的 user defaults 里，cfprefsd 不看 `HOME`，所以每次都读了真实用户的已保存窗口位置（`NSWindow Frame welcome`）。第一组 10 次（微秒）：4540、4856、5316、5935、6190、7324、7542、7688、8092、11847，**中位数 6.8 ms**；紧接着第二组 10 次：7044、8502、11309、11588、11749、11929、13460、14099、16132、19638，中位数 11.8 ms；20 次合计中位数 8.3 ms，最小 4.5 ms。**都远大于 1 ms**；这个耗时包含 AppKit 套用已保存窗口位置（每次启动都有一次同步的改窗口大小，见下一条），不只是 cfprefsd 的读取，所以让基准也设名会带来一次随用户已保存位置而变的改大小。这相当于 ADR 0017 的 20 ms 冷启动预算的三分之一到六成，按「数据目录」告诉产品负责人：基准不设名（本设计的选择）时 `app/cold_start` 比用户的启动少这一段，是否让基准也设名（那样基准会读写用户真实的窗口位置）由产品负责人决定。这个耗时本身（发布构建每次启动都有）是否要移出首帧之前，也由产品负责人和 workspace 设计决定，本 PR 没有改。
- 无探针的启动，日志里有一条 gpui 的 error `RefCell already borrowed`（`gpui-pre` 的 `window.rs`，时间在 `first frame drawn` 之前）。在 c880521（本 PR 的基线，没有本 PR 的改动）上建发布版，临时 `HOME`、`LECTERN_LOG=debug` 启动两次，每次也有这一条，所以不是本 PR 引入的，本 PR 没有改。可能的原因（未验证）：`set_frame_autosave_name` 让 AppKit 在 `open_window` 的回调里同步改了窗口大小（已保存的高度是 521，请求的是 520），GPUI 的 `on_resize` 处理函数此时拿不到 `App`。有探针的一次完整会话（`LECTERN_BENCH_SOCKET` 指向一个监听的 socket，`LECTERN_LOG=debug`）里没有 error 记录。
- 副作用：这些运行改了真实用户 defaults 域 `lectern` 里的 `NSWindow Frame welcome`（窗口位置），测完后已按测前读到的值写回。

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

`workspace` 的装配（详见 [workspace 设计「与设计的差异」](workspace.md#与设计的差异)第 3 步）：

- **`release_in(window, cx)`。** `release(cx)` 没有窗口句柄；desktop 的屏障在第二个帧回调里取 `window.window_handle()` 调 `release_in`，`release(cx)` 是 `release_in(None, cx)`（test profile 和直接调用的测试）。
- **`resources` 接收一个闭包。** `workspace::install` 要 `cx` 和 `app_data()`，`Resources` 要它的返回值：`resources(executor, install)` 在目录都齐、`Cache`、`Watcher` 建好之后调用闭包，所以目录缺失时不装 `workspace`。
- **`Shell` 在 `Started` 里。** `Quit` 和 Dock 重开要 `WorkspaceHost`、`WindowStateLink`，两者经 `Started::shell` 取（`Started` 仍是 `Clone`）。
- **`window::open` 拆成 `open_named` 和 `reopen`。** `open(cx)` 是 `open_named(cx, "welcome")`；`reopen` 是 `on_reopen` 的处理函数，按最后一次报告的状态取名字。
- **`quit` 拆成 `quit`、`ask_then`、`shut_down`。** `ask_then` 取 `Arc<dyn LeaveAsk>`，测试用假的实现；原来的线程与等待逻辑原样搬进 `shut_down`。
- **`serde_json` 与 `futures` 成了普通依赖。** `window.json` 与通道、`oneshot` 要用；`lectern-executor` 的 `test-support` 进了 dev 依赖（`Executor::manual`）。
- **`with_assets(IconAssets)` 已加**（[ui 设计「对其他 crate 的要求」](ui.md#对其他-crate-的要求)的 `app` 项）：`gpui_platform::application().with_assets(lectern_ui::IconAssets)`，`run` 之前。

`usage` 与 `project_home` 的装配：

- **`UsageDeps` 只有三个字段。** usage 设计「依赖」写 `watcher`、`git_ceilings` 要在 `app` 构造插件前补齐；第 1 步的代码没有这两项（它们要到第 2、5 步才用到），`app` 按现有的三个字段构造，不添加没有用处的字段。加字段的 PR 同时改 `profile.rs`，传 `r.watcher.clone()` 和 `r.git_ceilings.clone()`（usage 设计「与设计的差异」前置条件 C）。
- **`project_home` 提前装配。** project_home 设计把 D 放在第 3 步（视图之后）；它的第 1 步已有两个插件和 `UsageSlot`，`dyn Usage` 现在有了提供者，`project-home` 又不登记贡献，所以图校验通过、空 `Workspace` 的启动不受影响。代价是「已知限制」里的过渡状态。bench 的 `Driver` 的 `home` 场景（D 的另一半）仍随探针。
- **`welcome` 随它的视图装配**（见「welcome 的装配」）。
- **`LocalZone` 的 None 按 UTC。** platform 设计把 None 的处理交给 `app`；毫秒时间戳总有偏移，取 0 不记日志。

`review_view` 的装配：

- **只装 `review-view`。** review_view 设计的 C 要求两个插件都放进 `PluginSet`；`dyn Reader`、`dyn CodeIntel` 还没有提供者，`review-view-handoff` 放进去图校验就失败，所以先丢掉它（「review_view 的装配」）。

测量构建（5a）：

- **`BenchDriver` 持有 `Arc<dyn Reporter>`，不是 `Arc<Probe>`。** 探针线程只在真正的首帧回调里启动，`app` 的单元测试看不到它发出的 `Error`，所以 `Reporter`（只有 `error`）由 `Probe` 实现，测试用记录的假实现。`attach` 的签名不变。
- **主线程的处理函数取 `Option<&dyn Contributions>`。** `bench::handle(request, workspace, errors, cx)` 不自己查 `Started`；`attach` 的前台任务在每个请求到达时从 `Started::shell` 取出 hub 再调用它（`Started` 在 `attach` 之后、同一次 `release_in` 里设置，任务在之后的轮次才运行）。测试因此可以用真实的 `Hub`（假的 `Payloads`）观察到 `OpenProject`，不需要装配整个 workspace。
- **`a_quit_request_takes_the_quit_path` 不用假的 `QuitSteps`。** `quit::quit` 固定用 `Started` 作为步骤，退出顺序已由 `quit.rs` 的假步骤测试覆盖。这个测试用 test profile 的 `Started` 和临时日志目录：`Quit` 请求之后 `is_quitting` 为真，日志里依次是 `quit requested`（`path` 为 `action`）和 `run end`，即 ⌘Q 的路径。
- **frame autosave 名的选择在 `window_state::autosave_name`。** 骨架窗口（`window::open_named`）和 `window_state` 的改名共用这个纯函数；`Startup::bench_probe()` 在没有 `bench-probe` feature 时恒为 false。
- **`tests/bench_probe.rs` 在发 `Plugins` 之前等 `Quiescent`。** kernel 的状态由 kernel 线程填写，`attach` 后立刻问会得到空列表；运行器本来就在静止之后问（bench 设计「进程级基准」）。
- **nextest 的 `app-startup` 组加上 `binary(bench_probe)`。** 它和 `startup` 一样启动真实窗口，并行时窗口互相遮住，屏障不放开。
