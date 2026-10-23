# app 设计

状态：已接受，尚未实现。

对应 issue [#199](https://github.com/SoloJiang/lectern-app/issues/199)。`app` 是二进制 `lectern`：它决定进程启动时先做什么、后做什么，在首帧之后创建进程级的共享资源（[ADR 0025](../adr/0025.md)「进程级的共享资源」），并按固定的顺序退出。本文只写最小的启动外壳：启动顺序、启动屏障、退出顺序和运行日志的接入（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 3 项）。首帧的规则见 [ADR 0017](../adr/0017.md)，profile 的概念见[技术调研「插件架构」](tech-research.md#插件架构)。

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
  - 按 profile 装配具体的插件，创建 `Cache` 和 `fs_watch` 并传给插件：接入第一个插件时另开 issue。在此之前 desktop profile 的插件集合是空的。
  - 窗口快照的读取与恢复、窗口位置的 frame autosave：随 [#30](https://github.com/SoloJiang/lectern-app/issues/30)。
  - headless 与 probe profile、`bench-probe` feature、`Driver`、把应用数据目录指到临时目录：随 [bench 设计](bench.md)第 5 步（[#7](https://github.com/SoloJiang/lectern-app/issues/7)）。
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
| `lectern-logging`（`test-support`，dev） | 金丝雀测试的 `testing::capture()` | |
| `tempfile`（dev） | 启动测试的临时 `HOME` | 已在工作区里 |

不新增外部依赖。命令行只有一个选项，不引入命令行解析库：它会增加二进制体积和首帧前的初始化，换来的功能用不上。

## 接口

`app` 是二进制 crate，没有对外的接口。下面是模块之间的接口，测试经它们验证启动和退出的顺序。

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
    /// 主线程，在 `main` 里 `install_early` 之后调用。
    pub fn new(early: Early, profile: Profile) -> Startup;

    /// 放开启动屏障。主线程，不阻塞。第一次调用依次：`Early::start`（建写盘线程后返回）、
    /// `Executor::new`（不建线程）、`Kernel::start(profile.plugins())`（建 kernel 线程后返回），
    /// 记一条 info，把 `Started` 设为全局对象。之后的调用什么也不做。
    pub fn release(&mut self, cx: &mut App);
}
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

`Quit` Action 的处理函数在主线程上：

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
| 5 | 主线程 | `Application::new().run(…)`：设置应用菜单、绑定 ⌘Q、建骨架窗口 | GPUI 和 AppKit 自己的初始化 |
| 6 | 主线程 | 首帧画出；根视图第一次 `render` 时登记启动屏障（下节） | 否 |
| 7 | 主线程 | `Startup::release`：`Early::start`、`Executor::new`、`Kernel::start` | 建 `lectern-log` 线程和 kernel 线程，立即返回 |
| 8 | `lectern-log` | 建日志目录、清理、写文件头和首帧前缓冲 | 是 |
| 9 | kernel 线程 | 读清单、激活 `AfterFirstFrame` 插件（本 issue 里没有插件） | 由插件决定 |

第 4 步的 `platform::system::os_version()` 不在首帧前调用：它在第 7 步填 `Header` 时调用，是一次 `sysctl`，不读文件。

### 启动屏障

kernel 设计规定 desktop profile 在第一帧真正显示时放开屏障（[kernel 设计「启动」](kernel.md#启动)）。精确的显示时刻要在 Metal drawable 上登记 `addPresentedHandler`，锁定的 GPUI 不暴露 drawable，`platform::frame::on_first_presented` 还没有实现（[bench 设计「需要其他 crate 提供的接口」](bench.md#需要其他-crate-提供的接口)）。`app` 因此用 GPUI 的帧回调：

- desktop：根视图第一次 `render` 时调用 `window.on_next_frame`，回调里再调用一次 `window.on_next_frame`，第二个回调里调用 `Startup::release`。GPUI 在一次帧请求（display link 的回调）的开头执行上一帧登记的回调，所以屏障在首帧画出之后的第二次帧请求时放开。首帧的命令缓冲在第一次帧请求的末尾提交，在下一个垂直同步显示；第二次帧请求时它已经显示。比精确的显示时刻最多晚约 2 个刷新周期（60 Hz 下约 33 ms），只推迟后台初始化，不计入冷启动（冷启动的终点是首帧的显示时刻，由 bench 单独测量）。
- test：窗口建好后立即放开。
- `platform::frame::on_first_presented` 实现以后，desktop 改为在它的回调里放开（回调在任意线程上，转到主线程后调用 `release`），并删掉本节的帧回调。

`Startup::release` 只生效一次。窗口关闭后重新打开（见「界面」）时根视图再次登记，不会再次启动。

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

## 性能

- 冷启动：首帧之前只有「启动顺序」的第 1–6 步。新增的工作是参数解析（几个字符串比较）、`install_early`（预算 ≤ 20 µs，`log/install_early`）和语言协商（常数级的内存操作），合计远小于 20 ms 预算的 1%。首帧之前不建线程、不读写 Lectern 自己的文件、不调用 kernel 和 executor（ADR 0017）。
- 空闲内存：首帧之后多出日志的缓冲（logging 设计「性能」：≤ 512 KiB）、kernel 线程和它的栈。executor 在第一次提交任务之前不建线程。空集合的 kernel 静止后不再唤醒。
- 基准：`app/cold_start`、`app/idle_empty` 已在预算表里，由 bench 第 5 步的进程级基准测量；本 issue 不新增基准。在那之前用「测试」里的 `startup_*` 测试验证顺序：首帧之前没有日志文件。

## 界面

- 窗口：没有打开项目时的独立窗口，820 × 520 pt，居中（[交互规范「窗口结构」](interaction.md#窗口结构)的 Welcome 宽度）。标题栏透明，窗口背景用 `WindowBackgroundAppearance::Blurred`（系统的毛玻璃材质，自动适配浅色和深色模式，不需要自定义颜色），根视图不画任何内容，这就是 ADR 0017 的中性骨架。`ui` 提供骨架控件以后，由 Welcome 和 Overview 的视图替换这个根视图。
- 关闭窗口不退出应用（见「未决问题」1）。点 Dock 图标（`cx.on_reopen`）而且没有窗口时，重新打开骨架窗口。
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
| `lectern-quit` 线程建不起来 | 主线程直接 `cx.quit()`，`on_app_quit` 写结束记录 |
| 主线程 panic | logging 的 panic 钩子写记录（写盘线程还没启动时由钩子自己打开文件），然后调用原来的钩子 |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `app`。

- 默认记录：
  - info `startup barrier released`：`profile`（`desktop` 或 `test`）。记录自带的 `up` 是从 `install_early` 起的微秒数，用来对照启动耗时。
  - info `first frame drawn`：根视图第一次 `render` 之后的那次帧回调里记，同样只靠 `up` 和 `t`。启动测试用它的 `t` 检查日志文件在首帧之后才创建。
  - info `quit requested`：`path` 为 `action`（`Quit` Action）或 `system`（不经 `Quit`，只在 `on_app_quit` 里看到日志还没关闭时记）。
  - warn `unrecognized argument`：`index`（从 1 起）。
  - warn `kernel shutdown timed out`：`waited_us`。
  - warn `executor shutdown timed out`：`busy_workers`。
  - warn `could not start the quit thread`：`error` 为 `Value::io`。
- 详细级别：debug `quit finished`：`kernel_us`、`executor_us`（各步实际用时）。
- 关联 ID：没有。`app` 的代码不在任何插件的作用域里。
- 不记录：命令行参数的内容（认不出的参数只记下标；参数里可能有路径和令牌）；`--log` 的过滤字符串只由 logging 写进文件头；环境变量的值；系统首选语言列表。
- 金丝雀测试：`canary_stays_out_of_app_logs`（`src/tests/logs.rs`）：用 `testing::capture()`，把金丝雀放进一个认不出的参数和 `--log` 的值，经 `parse` 和记录 warn 的同一个函数，再跑一遍退出顺序，`assert_absent(金丝雀)`。

## 测试

`app` 依赖 GPUI，只在 macOS 上编译和测试（`just test -p lectern-app`）。

- 单元测试（`src/tests/`）：
  - `args`：`log_value_forms`（`--log x`、`--log=x`、多次出现取最后一次）、`log_without_a_value_is_unknown`、`non_utf8_log_is_ignored`、`unknown_arguments_keep_only_indexes`、`quit_after_start_only_in_debug_builds`。
  - `profile`：`every_profile_graph_validates`（对每个 `Profile` 调用 `plugins().validate()`，kernel 设计要求的 profile 测试）。
  - `quit`（用假的 `QuitSteps`、毫秒级的 `Timeouts`）：`steps_run_in_order`（kernel 的 `done` 之后才关 executor，executor 之后才关日志）、`a_hung_kernel_does_not_block_quit`（`done` 从不调用时，到截止时间后照常关闭 executor 和日志，`kernel_timed_out` 为 true）、`executor_gets_the_remaining_time`、`timeouts_are_logged`。
  - `logs`：`canary_stays_out_of_app_logs`。
  - `#[gpui::test]`：`test_profile_releases_once`（窗口建好后 `Started` 已设为全局对象；再次调用 `release` 不再启动 kernel）、`quit_before_release_quits_at_once`。
- 启动测试（`tests/startup.rs`，启动真实的 `lectern` 进程，需要 macOS 的图形会话；macOS CI 任务满足）：每个测试用新的临时目录作 `HOME`，日志落在 `$HOME/Library/Logs/Lectern/`，带调试构建才有的 `--quit-after-start`，等进程退出（10 s 超时）。
  - `startup_writes_a_header_and_an_end_record`：只有一个 `lectern-*.log`；第一行是文件头（`msg` 为 `file`）；含 `first frame drawn` 和 `startup barrier released`，前者在先；最后一行是 `run end`。
  - `log_file_is_created_after_the_first_frame`：日志文件的创建时间（`Metadata::created`）不早于 `first frame drawn` 的 `t`（毫秒精度）。
  - `cli_and_env_filters_reach_the_header`：`LECTERN_LOG=debug` 启动时文件头的 `filter` 是 `debug`；`--log cache=trace` 启动时是 `cache=trace`（同时设了 `LECTERN_LOG` 时仍是 `cache=trace`）。
  - `system_quit_writes_the_end_record`：`--quit-after-start=system`，最后一行是 `run end`，文件里有 `quit requested`、`path` 为 `system`。
  - `second_run_sees_a_clean_previous_run`：同一个 `HOME` 连续启动两次，第二次的文件里没有 `previous run ended unexpectedly`。

## 对其他 crate 的要求

每一项是一个 PR，在 `app` 的实现 PR 之前合并：

- **A. `i18n`**：新增 `locales/en/app.ftl`、`locales/zh-CN/app.ftl`，内容是「界面」一节的四个菜单键。
- **B. `platform`**：`system::preferred_languages() -> Vec<String>`：macOS 上用 `CFLocaleCopyPreferredLanguages`（内存里的系统偏好，不读 Lectern 自己的文件），返回 BCP 47 标签，按系统的顺序；其他平台返回空列表。测试：macOS 上返回的列表不为空、每一项都能被 `Locale::negotiate` 接受；其他平台为空。在 platform 设计里加一节，写「日志与诊断」（读不到时返回空列表，不记日志）。

## 未决问题

1. **关闭最后一个窗口时是否退出应用。** 选项：(a) 不退出，点 Dock 图标重新打开窗口（Xcode 欢迎窗口和大多数文档类 macOS 应用的做法）；(b) 退出，等同于 ⌘Q。默认 (a)。

## 已知限制

- 屏障放开之前（首帧之后约 2 个刷新周期内）退出时，首帧前缓冲里的记录丢失，下一次运行也不会报告上一次异常结束：这时还没有创建本次运行的日志文件。
- 由系统发起的退出不经 `Quit`：kernel 和 executor 不按顺序关闭（kernel 设计「已知限制」），只有日志的结束记录在 `on_app_quit` 的 200 ms 里写出。
- 屏障比首帧的实际显示最多晚约 2 个刷新周期，直到 `platform::frame::on_first_presented` 实现（「启动屏障」）。

## 验收

分两步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-app`、`just deny` 通过。

**前置条件 A、B**：各自的 PR，`just test -p lectern-i18n`、`just test -p lectern-platform` 通过。

**第 1 步：启动外壳与日志接入（本 issue 的实现 PR，`Closes #199`）。**

- 「测试」一节的单元测试和启动测试全部通过：`just test -p lectern-app`。
- `rg 'eprintln!|println!|log::' crates/app/src` 没有结果（记录只经 `lectern_logging` 的宏）。
- `cargo tree -p lectern-app -e features -i lectern-logging` 里有 `sink`。
- 手动在 macOS 上运行 `cargo run -p lectern-app`：窗口出现，按 ⌘Q 后进程在 3 s 内退出，`~/Library/Logs/Lectern/` 里本次运行的文件最后一行是 `run end`。
- logging 设计的状态行和「实际验证」写明第 3 项已实现，第 3 步验收里依赖进程级基准的一条（`app/cold_start` 不变慢）随 bench 第 5 步验证。
