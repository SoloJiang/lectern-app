# bench 设计

状态：实现中。第 1 步（运行器、预算表与迁移，[#83](https://github.com/SoloJiang/lectern-app/pull/83)）和第 3 步（插件集合的闭包与裁剪，[#98](https://github.com/SoloJiang/lectern-app/pull/98)）已实现；第 2 步（`platform` 的进程、主机信息与线程 QoS，[platform 设计「验收」](platform.md#验收)的 A 部分）已实现；第 4 步（探针、进程调度与空窗口基线）已实现，依赖 [ADR 0051](../adr/0051.md)，首帧的时刻按 [ADR 0052](../adr/0052.md)（drawable 没有 `presentedTime` 时取显示回调运行的时刻，结果注明来源），空窗口基线的实测值由 macOS CI 给出（见「实际验证」）；第 5 步尚未实现（见「验收」）；第 4、5 步要的 `platform` 接口已在 [platform 设计](platform.md)里补充（[ADR 0048](../adr/0048.md)）。按总时长封顶的采样、共享夹具和每个基准的耗时（[ADR 0036](../adr/0036.md)）已实现，见「平台与噪声」「实际验证」。时间盒截断的轮次计为不合格的样本（[ADR 0043](../adr/0043.md)）已实现，见「时间盒截断的轮次」「实际验证」。

对应 issue [#7](https://github.com/SoloJiang/lectern-app/issues/7)；安装包体积检查 [#38](https://github.com/SoloJiang/lectern-app/issues/38) 复用本 crate 的运行器和预算表。`bench` 测量[性能预算](tech-research.md#性能预算)、[ADR 0017](../adr/0017.md)、[ADR 0018](../adr/0018.md) 和各 crate 设计文档「性能」一节里的每一项，打印实测值与预算，在 Apple 芯片的 macOS 上超标即以非 0 退出码让 CI 失败。

## 范围

- 做：
  - 基准运行器：预热、采样、统计、与预算比较、重试、文本报告和 JSON 报告、退出码。
  - 唯一的预算表：每个基准 ID 对应一条预算，写明统计量、上限和出处。
  - 进程内基准：直接调用领域、基础设施和 kernel 层 crate 的公开接口与 `testing` 模块。
  - 进程级基准：启动 `lectern`（或空 GPUI 窗口的基线程序）的子进程，测冷启动、空闲内存、每个插件的激活耗时与常驻内存、打开与切换的端到端耗时、帧时间。
  - 进程级基准的探针（`probe` feature）：`app` 在测量构建里调用它，向运行器报告首帧时刻、静止信号和插件状态。
  - 迁移现有的 `perf_smoke` 测试（`crates/agents/tests/perf_smoke.rs`、`crates/platform/src/safe_read/tests.rs` 里的 `perf_smoke`、`crates/git` 里的 `perf_smoke_git_open` 和 `perf_smoke_git_diff_50`、`crates/cache` 里的 `perf_smoke_cache`）。
- 不做：
  - 取进程创建时间、`phys_footprint`、帧的 `presentedTime`、线程 QoS、机器信息：macOS 专属，在 `platform`（架构规则 6），接口见「需要其他 crate 提供的接口」。帧的时刻靠方法替换，只在测量构建里有（[ADR 0048](../adr/0048.md)）。
  - 生成合成仓库、合成会话等夹具：放在被测 crate 的 `testing` 模块，供它自己的测试和 bench 共用。
  - 在视图里判断「内容已画出」：由 `workspace` 报告里程碑，`app` 转给探针。视图 crate 不依赖 `bench`。
  - 打包 `.app` 与 `.dmg`：由 [#38](https://github.com/SoloJiang/lectern-app/issues/38) 的打包步骤负责，bench 只测量产物。
- 本 issue 之外：
  - 主体还不存在的基准（`git`、`project` 的分项，打开项目、打开文件、打开 Diff、切换 worktree、滚动、输入、动画帧时间、monorepo 空闲内存、进程插件合计内存）在预算表里登记为「计划中」，由实现主体的 PR 同时加上基准（见「预算表」）。
  - 安装包体积：[#38](https://github.com/SoloJiang/lectern-app/issues/38)。

[#7](https://github.com/SoloJiang/lectern-app/issues/7) 正文写的冷启动判定是「p50 < 20 ms、p95 < 30 ms」，与已接受的 ADR 0017（20 次里最多 1 次超过 20 ms，任何一次不超过 30 ms）不一致。以 ADR 0017 为准。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| 标准库 | 计时（`Instant`）、计数分配器、子进程、Unix 域 socket | 运行器自己写，见下表 |
| `serde_json`（工作区已有） | 写 JSON 报告 | 已在依赖图里，不新增包 |
| `lectern-kernel`（可选，`probe` feature） | 探针插件、裁剪插件集合 | kernel 层 |
| `lectern-platform`（`probe` feature 同时打开它的 `bench-hooks`） | `main` 设置基准线程的 QoS，报告的 `host`，进程级基准读进程的创建时间、`phys_footprint` 和子进程；`probe` 注册首帧回调。`bench-hooks` 是 `platform` 里方法替换的 feature，只有 `probe` 打开它，所以只在测量构建里（[ADR 0048](../adr/0048.md)） | 基础设施层 |
| `tempfile`（工作区已有） | 进程级基准每次启动的临时目录：socket 和临时 `HOME` | 在系统临时目录下，socket 路径不超过 macOS `sun_path` 的 104 字节 |
| dev：`lectern-agents`、`lectern-cache`、`lectern-git`、`lectern-kernel`、`lectern-i18n`、`lectern-project`（各自打开 `test-support`），`lectern-executor`、`lectern-services`（只用公开接口，不需要 `test-support`），`insta`；`libc`、`futures`（工作区已有） | 进程内基准的被测代码与夹具；单元测试的快照；`benches/process.rs` 向进程组发 `SIGKILL`（POSIX 的 `kill`，不是 macOS 专属）；`empty_window` 示例里探针线程请主线程退出的通道 | 只编译进基准目标、示例和测试；`futures` 不只为 macOS 声明，因为只为 Apple 平台声明的外部依赖算作系统绑定（架构规则 6） |
| dev（只在 macOS）：`gpui`（`test-support`）、`lectern-ui` | `ui/*` 基准（[ui 设计「性能」](ui.md#性能)）：`Transition::tick` 要在一个视图的 render 里跑，所以用 GPUI 的测试平台开一个窗口 | 只在 macOS 声明，Linux 上本 crate 仍不依赖 GPUI（架构检查第 4 条只把外部的、非 GPUI 的 Apple 专属依赖算作绑定）；对应的预算标 `macos_only`，Linux 上记为 `skipped` |
| dev（只在 macOS）：`gpui_platform` | `empty_window` 示例程序 | 同上 |
| 以后的 dev：其他插件 crate | 随这些 crate 的实现加入；`lectern-project` 随它的第 5 步加入 | tool 层可以依赖任何层，包括插件 crate（[ADR 0025](../adr/0025.md)） |

本 crate 不新增任何外部 crate，`just deny` 与 `Cargo.lock` 的外部部分不变。

### 运行器的选择

| 方案 | 新增的包 | 按绝对预算判定 | 进程级基准 | 冒烟运行 |
|---|---|---|---|---|
| criterion | criterion 和它的 plotters、ciborium、clap、tinytemplate 等十几个传递依赖（`Cargo.lock` 里一个都没有） | 没有。只和上一次存在 `target/criterion` 里的基线比较，CI 上没有稳定的基线 | 不适用：迭代模型是在进程内反复调用闭包 | `cargo test --benches` 下每项跑一次 |
| divan | divan、divan-macros、clap 系列、condtype、regex-lite | 没有，只打印统计量 | 同上 | 同上 |
| 自写，`harness = false` | 无 | 读预算表，超标返回非 0 | 同一个运行器调度子进程 | 同上 |

选自写的运行器。三者的许可证都是 MIT / Apache-2.0，`cargo-deny` 不是区分点。区别在于：预算是固定的上限，判定只需要中位数、p95、最大值和计数，几十行代码；criterion 的自助法和离群点分析服务于「与上次相比有没有变化」，CI 用不上；冷启动和内存基准要启动子进程、等事件，两个框架都没有对应的模型，仍要另写一套。自写也省掉十几个包的编译时间，两个 CI 任务都受益。

## 接口

### 运行器（`lectern_bench`，默认编译）

```rust
/// 一项基准。`run` 在基准进程的主线程上调用，可以阻塞。
/// 一项基准可以有多个场景，返回多条结果；每条结果的 ID 必须在预算表里。
pub struct Bench {
    /// 结果 ID 的前缀，用于过滤，如 "agents/parse_throughput"、"project/resolve_package"。
    pub id: &'static str,
    pub run: fn(&mut Ctx) -> Result<Vec<Measured>, BenchError>,
}

pub struct Measured {
    pub id: String,          // "project/resolve_package/hit"
    pub value: Value,
    /// 报告和注释里写在值旁边的说明，如被时间盒截断的轮次的诊断（`with_note` 设置）。
    pub note: Option<String>,
}

pub enum Value {
    /// 每次操作的耗时（已除以批量次数）。
    Time(Samples),
    /// 每次操作处理 `bytes` 字节；统计量是 bytes / 耗时。
    Throughput { bytes: u64, time: Samples },
    /// 被测与对照交替采样；比值按同一统计量计算，被测的耗时也可以另有绝对上限。
    Ratio { subject: Samples, baseline: Samples },
    /// 字节数：计数分配器的存活字节，或 `phys_footprint`。
    Bytes(Vec<u64>),
    /// 观察一次的计数，如一次操作读过的文件数。只记录，没有适用的检查。
    Count(u64),
    /// 一段滚动、输入或动画：每帧的工作时间（`completed - requested`）和相邻两帧 `started` 的间隔。
    Frames { work: Samples, intervals: Samples, refresh: Duration },
}

pub struct Samples(pub Vec<Duration>);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `cargo bench`：按「平台与噪声」采样；在判定平台上与预算比较。
    Measure,
    /// `cargo test --benches`：每项跑一次、用小规模输入，只检查能运行，不比较预算。
    Smoke,
}

pub struct Ctx { /* 模式、采样配置、临时目录 */ }

impl Ctx {
    pub fn mode(&self) -> Mode;
    /// 按模式选输入规模：Measure 用 `measure`，Smoke 用 `smoke`。
    pub fn scale<T>(&self, measure: T, smoke: T) -> T;
    /// 本基准独占的空目录，在 `CARGO_TARGET_TMPDIR/lectern-bench/<id>/` 下；基准结束后删除。
    pub fn scratch(&self) -> &Path;
    /// 预热后采样，`f` 每次执行一次被测操作并返回它的结果。单次短于 1 ms 的操作按批量计时（见「平台与噪声」）。
    pub fn time<R>(&mut self, f: impl FnMut() -> R) -> Samples;
    /// 每次调用 `f` 之前调用一次 `setup`，不计时（追加 4 KB、改一个文件）；`setup` 的返回值经 `black_box` 后交给 `f`。
    /// 从不批量：每个样本是一次 `setup` 加一次 `f`（k = 1），有状态的操作每次都面对新准备好的输入。
    pub fn time_with_setup<S, R>(&mut self, setup: impl FnMut() -> S, f: impl FnMut(S) -> R) -> Samples;
    /// 被测与对照交替采样，抵消采样期间的频率和缓存漂移。
    pub fn compare<R1, R2>(&mut self, subject: impl FnMut() -> R1, baseline: impl FnMut() -> R2) -> (Samples, Samples);
    /// 覆盖本基准的采样配置（慢操作减少样本数等）。
    pub fn config(&mut self, config: SampleConfig);
    /// 只计时一次调用，不预热：只记录的诊断项（见「平台与噪声」）。
    pub fn once<R>(&mut self, f: impl FnMut() -> R) -> Samples;
    /// 本目标进程里按 `key` 共享的只读夹具：第一次调用 `build`，之后的基准和重跑拿到同一份（见「夹具与 `test-support`」）。
    pub fn shared<T: 'static>(&self, key: &str, build: impl FnOnce() -> Result<T, BenchError>) -> Result<Rc<T>, BenchError>;
}

/// `memory` 基准目标的全局分配器：包装 `System`，用两个 `AtomicUsize` 记录存活字节与峰值。
/// 只在 `memory` 目标里注册，`time` 目标的计时不受它影响。
pub struct CountingAlloc;
impl CountingAlloc {
    pub fn live() -> usize;
    pub fn peak() -> usize;
    pub fn reset_peak();
}

/// 基准二进制的入口。解析参数（cargo bench 传入的 `--bench` 决定模式，其余是 ID 的子串过滤，
/// `--list` 只列出 ID 与预算），检查与预算表的对应，按 ID 顺序运行，写报告，返回退出码。
/// `tmp_dir` 是基准目标的 `env!("CARGO_TARGET_TMPDIR")`（库代码编译时没有这个变量）：
/// `scratch()` 在它下面，报告写在它的上一级（target 目录）的 `lectern-bench/` 里。
pub fn main(target: Target, tmp_dir: &str, benches: &[Bench]) -> ExitCode;

/// 基准出错（夹具生成失败、被测操作返回错误）。实现了 `std::error::Error` 的错误可以直接用 `?`；
/// 只实现 `Debug` 的（如 `SessionError`）用 `BenchError::debug(上下文, 错误)`。
pub struct BenchError { /* 原因 */ }
```

被测操作不能被优化掉，约定如下：

- 被测闭包返回操作的结果（解析出的记录、`ChangeSet`、`bool`），不在闭包里丢弃。运行器把每次的返回值传给 `std::hint::black_box`；批量计时时，返回值先放进计时开始前按批量次数预分配好的缓冲区，计时结束后再 drop，drop 的耗时不计入样本。
- 闭包读取的输入（路径、字节、夹具句柄）在使用前经过 `std::hint::black_box`，如 `ctx.time(|| parse_file(agent, black_box(&path)))`，编译器不能把输入当作常量提前算出结果。
- 返回 `()` 的操作只用于本身有副作用的调用（写文件、提交任务后等待完成）。`runner_*` 里有一个对常量输入做计算并返回结果的假基准，确认 `time` 测到的耗时不为 0。

退出码：0 表示没有超标、没有出错；1 表示有判定为超标的结果；2 表示基准出错、夹具生成失败，或基准与预算表对不上。出错在任何模式和平台上都返回 2。

### 预算表（`lectern_bench::budgets`）

```rust
pub static BUDGETS: &[Budget] = &[ /* 见「预算表」 */ ];

pub struct Budget {
    /// 与 `Measured::id` 相同。一段写成 `*` 时匹配任意一段（插件 ID），更具体的条目优先。
    pub id: &'static str,
    /// 全部满足才算通过；为空表示只记录。
    pub checks: &'static [Check],
    pub tier: Tier,
    /// 出处：仓库根起的路径加锚点，如 "docs/adr/0017.md#决策"、"docs/design/agents.md#性能"。
    pub source: &'static str,
    pub state: State,
    /// 产出这条预算的基准目标。每条预算只属于一个目标。
    pub target: Target,
    /// 基准只在 macOS 上编译（要 GPUI）：其他平台上这一条记为 `skipped`（只在 macOS 运行），
    /// 不算「预算没有对应的基准」。
    pub macos_only: bool,
}

/// `Package` 不是基准目标：由打包流程调用 `lectern-bench-report package` 产出，`just bench` 不产出它（见「报告」）。
pub enum Target { Time, Memory, Process, Package }

pub struct Check { pub stat: Stat, pub limit: Limit }

pub enum Stat {
    Median,
    P95,
    Max,
    /// 超过给定时长的样本个数，与 `Limit::CountAtMost` 配合（冷启动「20 次里最多 1 次超过 20 ms」）。
    CountAbove(Duration),
}

pub enum Limit {
    TimeBelow(Duration),      // <
    TimeAtMost(Duration),     // ≤
    ThroughputAtLeast(u64),   // 字节/秒，≥
    BytesBelow(u64),          // <
    BytesAtMost(u64),         // ≤
    RatioAtMost(f64),         // ≤，只用于 Value::Ratio
    CountAtMost(u32),
    /// 只用于 Value::Frames 的 `intervals`：间隔 ≤ 给定倍数的刷新周期。
    RefreshMultipleAtMost(f64),
}

pub enum Tier {
    /// 技术调研「性能预算」或 ADR 里的数字。出处必须是 tech-research.md 或 docs/adr/ 下的文件；改动先写 ADR。
    Top,
    /// 某个 crate 设计文档「性能」里分到的份额。改动在同一个 PR 里修改那份设计文档，
    /// 并说明同一顶层预算下各份额之和仍在预算内；超出时就是改顶层预算，要写 ADR。
    Share,
}

pub enum State {
    Active,
    /// 主体还没有实现。报告里列为「计划中」，不要求有对应的基准。参数是负责加入的 crate 或 issue。
    Planned(&'static str),
}
```

预算表里的 MB 一律按 2^20 字节计，与活动监视器显示的内存和 `du -k` 的换算一致。

数字的权威是出处：出处改了，`budgets.rs` 和本文「预算表」一节在同一个 PR 里跟着改。

### 报告协议（`lectern_bench::wire`，默认编译）

运行器和探针之间用一条 Unix 域 socket 通信，一行一条消息，UTF-8，形如 `first-frame pid=812 presented_unix_ns=… source=presented`：第一个词是消息名，其余是 `key=value`，值用百分号编码（空格、`%`、换行和非 UTF-8 字节）。

```rust
/// 探针 → 运行器。
pub enum Report {
    /// 首帧的时刻和来源（ADR 0052）：`source` 为 `presented`（`presentedTime`）或 `handler`（显示回调运行的时刻），
    /// 缺少或不认识时整行无效。`TimeSource` 就是 `platform::frame::TimeSource`，`wire` 重新导出。
    FirstFrame { pid: u32, presented: SystemTime, source: TimeSource },
    Busy { epoch: u64 },
    Quiescent { epoch: u64 },
    /// 所选插件都已进入 Active（裁剪了插件集合时，或应答 `Request::ActivateAll` 时发出）。
    Ready,
    /// 对 `Request::Epoch` 的应答：`Kernel::quiescent_epoch()`。
    Epoch(Option<u64>),
    /// 对 `Request::Plugins` 的应答，每个插件一条，以 `PluginsEnd` 结束。
    Plugin(PluginReport),
    PluginsEnd,
    /// 所选插件之一进入 `Failed`（`attach` 等待 `Ready` 期间和之后都检查），立即发出。
    PluginFailed { id: String, message: String },
    /// 还不带来源：随它的使用方（`frame::record`，即 platform 的 B2，和第 5 步）一起加上（ADR 0052）。
    Milestone { milestone: Milestone, presented: SystemTime },
    Frames(FrameStats),
    /// Lectern 启动的子进程，内存基准按类别分别统计（ADR 0018）。
    Child { pid: u32, kind: ChildKind, id: String },
    Error { message: String },
}

/// 运行器 → 探针。
pub enum Request {
    Epoch, Plugins, Open(OpenTarget), Quit,
    /// 激活 profile 里的全部插件：`Command` 插件各调用一次 `Kernel::command`，`Manual` 插件调用 `Kernel::activate`，
    /// 还未激活的 `Workspace` 插件由 `bench-probe` 插件提交一份对任何 pattern 都返回 true 的 `WorkspaceFacts`；
    /// 全部进入 Active 后发出 `Ready`，期间有插件进入 `Failed` 时发出 `PluginFailed`。
    ActivateAll,
}

pub struct PluginReport {
    pub id: String,
    pub state: String,                 // PluginState 的名字
    pub activation: Vec<ActivationKind>, // 清单里的全部激活条件（AfterFirstFrame / Workspace / Command / Manual），与顺序无关
    pub manifest: Option<Duration>,
    pub activate: Option<Duration>,
    pub commit: Option<Duration>,
    pub started: Option<Duration>,
}

pub enum Milestone { ProjectHome, FileHighlighted, DiffShown, WorktreeSwitched }
pub enum OpenTarget {
    Project(PathBuf), File(PathBuf), Diff { session: String }, Worktree(PathBuf), Scroll, Animate,
    /// 以固定间隔向当前焦点注入按键，走与真实键盘输入相同的分发路径。
    Input { text: String, interval: Duration },
}
/// 一段滚动或动画的逐帧时刻，按帧顺序。只有 `platform` 的 `FrameProblems` 干净的记录才会发出（见 `Probe::frames`）；
/// `without_tick` 是不是 display link 发起的帧数（`requested` 取 `started`），只报告。
pub struct FrameStats { pub scenario: String, pub refresh: Duration, pub frames: Vec<FrameTimes>, pub without_tick: u32 }
/// `presented` 为 None：这一帧没有显示时刻。来源与 `Milestone` 一样，随 B2 和第 5 步加上（ADR 0052）。只报告，不判定。
pub struct FrameTimes { pub requested: SystemTime, pub started: SystemTime, pub completed: SystemTime, pub presented: Option<SystemTime> }
pub enum ChildKind { ProcessPlugin, LanguageServer, Other }

impl Report  { pub fn encode(&self) -> String; pub fn decode(line: &str) -> Result<Report, WireError>; }
impl Request { pub fn encode(&self) -> String; pub fn decode(line: &str) -> Result<Request, WireError>; }
```

### 探针（`lectern_bench::probe`，`probe` feature）

`app` 的 `bench-probe` feature（不在 `default` 里）打开 `lectern-bench/probe` 并调用下面的函数。发布构建不打开它，bench 不进入发布构建（架构规则检查第 2 条）。

```rust
pub struct Probe { /* socket 路径、插件选择、首帧状态 */ }

impl Probe {
    /// 首帧之前、`main` 开头调用。只读环境变量 `LECTERN_BENCH_SOCKET`、`LECTERN_BENCH_PLUGINS`，
    /// 不做 IO、不建线程。没有设置 `LECTERN_BENCH_SOCKET` 时返回 None，应用照常运行。
    /// 没有严格 archive 模式：发布构建不带 binary archive（[ADR 0049](../adr/0049.md)）。
    pub fn from_env() -> Option<Probe>;

    /// 首帧之前、创建窗口之前调用：通过 `platform::frame::on_first_presented` 注册回调。
    /// 回调只记下时刻和来源（`PresentedAt`，ADR 0052），然后在一个新线程上连接 socket、发出 `FirstFrame`，之后由这个线程应答请求。
    /// 注册出错（`HookError`，如 GPUI 升级后方法的类型编码变了）时立即连接 socket，发出 `Error { message: "hook <HookError::as_str()>" }`，
    /// 然后以退出码 3 退出：不能让运行器等到期限，也不能测出错的数字（ADR 0048）。
    pub fn watch_first_frame(&self);

    /// 首帧之后，代替 `Kernel::start` 调用：加入 `bench-probe` 插件并启动 kernel。
    /// 总是先 `PluginSet::prepare` 取得清单：每个插件的 `manifest()` 在整个启动里只调用一次，就是那次被计时的第一次调用，
    /// 之后 `Kernel::start_prepared` 直接用这些清单和耗时，不再调用 `manifest()`，`plugin/*/activate` 里的 manifest 一段因此与发布构建相同。
    /// 没有设置 `LECTERN_BENCH_PLUGINS` 时不裁剪：`Plugins` 的应答和 `ActivateAll` 要用清单里的激活条件，`PluginStatus` 不带它们
    /// （见「与设计的差异」第 4 步）。设置了时按它裁剪：`closure:<id>` 只留 `<id>` 的依赖闭包，`closure+self:<id>` 留依赖闭包加 `<id>`。
    /// 变量的值无效或裁剪不了时发出 `Error`，整个集合照常启动。
    pub fn start_kernel(&self, set: PluginSet) -> Arc<Kernel>;

    /// `start_kernel` 之后调用。裁剪过插件集合时，逐个满足所选插件的激活条件：
    /// `Command` 调用一次 `Kernel::command`，`Manual` 调用 `Kernel::activate`，
    /// `Workspace` 由 `bench-probe` 插件在 `started` 里提交一份对任何 pattern 都返回 true 的 `WorkspaceFacts`；
    /// 全部进入 Active 后发出 `Ready`。
    pub fn attach(&self, kernel: Arc<Kernel>, driver: Arc<dyn Driver>);

    /// 没有 kernel 的程序（`empty_window`）代替 `attach` 调用，只交出 `Driver`：
    /// `Epoch` 应答 None，`Plugins` 只应答 `PluginsEnd`，`ActivateAll` 应答 `Ready`。
    /// `attach` 或 `drive` 之前到达的请求排队，之后按顺序处理。
    pub fn drive(&self, driver: Arc<dyn Driver>);

    /// `app` 把 `workspace` 报告的里程碑转给探针，`presented` 是画出这一状态的帧显示的时刻。任意线程。
    pub fn milestone(&self, milestone: Milestone, presented: SystemTime);

    /// `app` 把 `workspace` 记录的一段滚动或动画的逐帧时刻（取自 `platform::frame::record`）转给探针。任意线程。
    /// 记录出错（`HookError`）或 `FrameProblems::is_clean()` 为 false 时，`app` 不调用它，改为调用 `error`，
    /// 消息是 `frames <HookError::as_str()>` 或 `frames unmatched=<n> incomplete=<n> overflowed=<n>`；运行器把这个基准记为 `error`。
    pub fn frames(&self, stats: FrameStats);

    /// 发出 `Error { message }`。任意线程。
    pub fn error(&self, message: &str);

    /// `app` 的进程插件宿主和语言服务器启动子进程后报告，供内存基准分类统计。
    /// `pid` 是这个插件（或语言服务器）进程组的根进程，即宿主直接启动的那个进程；它再启动的进程不必单独报告。
    pub fn child(&self, pid: u32, kind: ChildKind, id: &str);
}

/// `app` 实现：按与用户操作相同的路径执行运行器的请求。在探针线程上调用，实现自己转到主线程，不阻塞。
pub trait Driver: Send + Sync {
    fn open(&self, target: OpenTarget);
    fn quit(&self);
}
```

`bench-probe` 插件（`AfterFirstFrame`）监听 `Busy` / `Quiescent`，按 [kernel「静止」](kernel.md#静止)的规则丢弃序号过时的事件，记下序号转发给运行器，监听里不做别的工作，所以不会让 kernel 重新变忙。只在裁剪过插件集合时，它在 `started` 里提交那份 facts；整个 profile 的启动（`app/idle_empty`）不提交，`Workspace` 插件保持未激活，与用户打开空窗口时一致。kernel 设计里的合成 `bench-facts` 插件就是这一部分：只用 kernel 的公开接口（`PluginHandle::control().workspace_changed`），放在本 crate，kernel 不需要另外提供。

### 进程调度（`lectern_bench::process`，默认编译）

`benches/process.rs` 用 `std::process`（每个程序是新进程组的组长，`process_group(0)`）和 `platform::process` 实现两个 trait，`process_*` 用脚本化的假实现。

```rust
pub trait Launcher: Sync {
    /// 以新进程组的组长启动 `launch.program`（组 ID 等于 pid），环境在运行器的之上按 `launch.env` 设置或删除。
    fn launch(&self, launch: &Launch) -> io::Result<Box<dyn Launched>>;
}
pub trait Launched: Send {
    fn pid(&self) -> u32;
    fn try_wait(&mut self) -> io::Result<Option<Exit>>;
    fn kill_group(&mut self);        // 向整个进程组发 SIGKILL
    fn kill(&mut self, pid: u32);    // 离开了进程组的后代
}
pub trait ProcessInfo: Sync {
    fn start_time(&self, pid: u32) -> io::Result<SystemTime>;
    fn phys_footprint(&self, pid: u32) -> io::Result<u64>;
    fn children(&self, pid: u32) -> io::Result<Vec<u32>>;
}
pub struct Launch { pub program: PathBuf, pub env: Vec<(OsString, Option<OsString>)> }
pub struct Exit { pub code: Option<i32> }   // None：被信号结束
/// 「进程级基准」表里的期限和静止后的等待；默认值就是那张表（10 s、30 s、30 s、10 s、10 s、5 s），测试里缩短。
pub struct Limits { pub first_frame, pub first_quiescent, pub again, pub reply, pub quit, pub settle, pub poll: Duration }

pub struct Scheduler<'a> { /* Launcher、ProcessInfo、Limits */ }
impl Scheduler<'_> {
    /// 一次启动：新的 socket 和临时 `HOME`（都在一个临时目录里，会话结束后删除），设置 `LECTERN_BENCH_SOCKET`、
    /// 可选的 `LECTERN_BENCH_PLUGINS`，删除 `LECTERN_LOG`、`CLAUDE_CONFIG_DIR`、`CODEX_HOME`；等到 `FirstFrame`，pid 必须是启动的那个。
    pub fn session(&self, program: &Path, plugins: Option<&str>) -> Result<Session<'_>, BenchError>;
    /// 冷启动：`warmups` 次不计入，再 `samples` 次，逐个启动；样本是 `presented - start_time(pid)`，同时记下每个样本的来源；
    /// 每次都发 `Quit` 并等它退出。
    pub fn cold_start(&self, program: &Path, warmups: usize, samples: usize) -> Result<ColdStart, BenchError>;
    /// 内存类：最多 `MEMORY_PARALLEL`（3）个同时运行；第一个出错的结果返回，之后不再开始新的。
    pub fn memory<T: Send>(&self, launches: usize, run: impl Fn(&Scheduler) -> Result<T, BenchError> + Sync) -> Result<Vec<T>, BenchError>;
}
/// 冷启动的样本，和按同样顺序的每个样本的首帧来源（ADR 0052）。
pub struct ColdStart { pub samples: Samples, pub sources: Vec<TimeSource> }
impl Session<'_> {
    pub fn pid(&self) -> u32;
    pub fn presented(&self) -> SystemTime;
    pub fn source(&self) -> TimeSource;          // `presented` 的来源（ADR 0052）
    pub fn children(&self) -> &[Spawned];       // 探针报告的 `Child`
    pub fn footprint(&self) -> Result<u64, BenchError>;   // 主进程的 phys_footprint
    pub fn settle_after_first_frame(&mut self) -> Result<(), BenchError>;   // 没有 kernel：首帧后 `settle`
    /// kernel「静止」的采样规则；首帧后 `first_quiescent` 内没有 `Quiescent` 时返回 `Idle::NotQuiescent`。
    pub fn idle<T>(&mut self, sample: impl FnMut(&mut Session) -> Result<T, BenchError>) -> Result<Idle<T>, BenchError>;
    pub fn epoch(&mut self) -> Result<Option<u64>, BenchError>;
    pub fn plugins(&mut self) -> Result<Vec<PluginReport>, BenchError>;
    pub fn ready(&mut self) -> Result<(), BenchError>;
    pub fn activate_all(&mut self) -> Result<(), BenchError>;
    pub fn open(&mut self, target: OpenTarget, milestone: Milestone, limit: Duration) -> Result<Duration, BenchError>;
    pub fn frames(&mut self, target: OpenTarget, limit: Duration) -> Result<FrameStats, BenchError>;
    pub fn quit(self) -> Result<(), BenchError>;   // 退出码不是 0 也是错误
}
pub enum Idle<T> { Sample(T), NotQuiescent }
/// 根进程及其全部后代：广度优先、按 pid 去重，展开期间已经退出的进程跳过。
pub fn expand(info: &dyn ProcessInfo, roots: &[u32]) -> Vec<u32>;
/// `sudo -n purge`；sudo 不存在、要密码或 purge 失败时为 false（`skipped`）。`path` 只替换这条命令的 `PATH`。
pub fn purge(path: Option<&OsStr>) -> io::Result<bool>;
```

会话的每个等待都经过同一个循环：收到探针的 `Error`、`PluginFailed` 或无效的行，或者进程退出，立即出错；到期时出错，原因写明等的消息。出错时（会话被丢弃时进程还在运行）先读出进程树，再向整个进程组发 `SIGKILL`，然后结束离开了进程组的后代（含探针报告的 `Child` 根进程），最后才关闭 socket。进程退出时，它在退出前写下的报告仍然先读完，所以探针的 `hook <名字>` 是原因，而不只是「退出码 3」。

### 需要其他 crate 提供的接口

这些接口由对应 crate 在自己的设计文档里补充、在各自的 PR 里实现（见「验收」）。下面是 bench 需要的最小形状。

`platform`（macOS 实现；其他平台返回 `ErrorKind::Unsupported`，进程级基准在那里不运行）。签名、线程、错误和实现以 [platform 设计](platform.md)为准：「进程」「主机信息与线程 QoS」「帧的时刻」，方法替换的规则在「测量构建的方法替换」（[ADR 0048](../adr/0048.md)）。下面是 bench 用到的形状：

```rust
pub mod process {
    /// 内核记录的进程创建时间（ADR 0017 的测量起点）。
    pub fn start_time(pid: u32) -> io::Result<SystemTime>;
    /// 活动监视器里的「内存」。
    pub fn phys_footprint(pid: u32) -> io::Result<u64>;
    /// 父进程是 `pid` 的进程（直接子进程），调用时的快照。
    pub fn children(pid: u32) -> io::Result<Vec<u32>>;
}

pub mod host {
    pub struct HostInfo { pub model: String, pub chip: String, pub os_version: String, pub cpus: u32 }
    /// 报告的 `host` 用它填；出错时 `model`、`chip`、`os_version` 写成 null（与第 1 步相同）。
    pub fn describe() -> io::Result<HostInfo>;
}

pub mod thread {
    /// 把调用线程的 QoS 设为 user-interactive，让基准线程运行在性能核上。
    pub fn set_user_interactive() -> io::Result<()>;
}

/// 以下只在 `bench-hooks` feature 里有效（`probe` 打开它）；没有时返回 `HookError::Unsupported`。
pub mod frame {
    /// 首帧的时刻，换算到系统时钟，带来源（ADR 0052）：最先运行的显示回调读到的 `presentedTime` 大于 0 时是它（`TimeSource::Presented`），
    /// 否则是这个回调运行的时刻（`TimeSource::Handler`）。回调在任意线程上调用一次。
    /// 首帧之前、创建窗口之前调用；注册本身不做 IO、不建线程。
    pub fn on_first_presented(f: impl FnOnce(PresentedAt) + Send + 'static) -> Result<(), HookError>;

    /// 开始记录 `window` 的逐帧时刻。主线程，首帧之后。`workspace` 调用它，并在每一帧开头调用 `FrameRecorder::frame_started`；
    /// `stop` 之后在后台线程上 `PendingFrames::wait` 得到 `FrameRecording { refresh, frames, problems }`。
    /// `FrameTimes` 的四个时刻：`requested`（这一次刷新的垂直同步，即帧源要求这一帧的时刻）、`started`（主线程开始处理、布局之前）、
    /// `completed`（命令缓冲的 `GPUEndTime`）、`presented`（`PresentedAt`，规则与首帧相同，只报告）。`app` 把它转换成 `wire::FrameTimes`。
    pub fn record(window: &impl HasWindowHandle) -> Result<FrameRecorder, HookError>;
}
```

与原来的形状相比：`host::describe`、`thread::set_user_interactive` 返回 `io::Result`；`frame` 的函数返回 `Result<_, HookError>`；`record` 要窗口句柄，刷新周期放进 `FrameRecording::refresh`，不再单独提供 `refresh_period()`；`FrameTimes::presented` 是 `Option`；首帧和 `presented` 的时刻带来源（`PresentedAt`，ADR 0052）；没有 `metal` 模块（ADR 0049）。

其中 `process::phys_footprint` 已随 `cache` 第 2 步加入（`cache/memory` 用它，见「进程内基准」，[platform 设计「进程」](platform.md#进程)）。`process` 的另两个、`host`、`thread` 随第 2 步，已实现；`frame::on_first_presented` 在第 4 步之前，`frame::record` 在第一个生效的帧时间预算之前（platform 设计「验收」的 B1、B2）。

`kernel`（普通的公开接口，不放在 `test-support` 下：发布构建里没有调用方，测量用的构建因此不需要打开 kernel 的 `test-support`；已随第 3 步实现，细节以 [kernel 设计「插件集合的闭包与裁剪」](kernel.md#插件集合的闭包与裁剪)为准）：

```rust
impl PluginSet {
    /// 对每个插件调用且只调用一次 `manifest()`，按 `Kernel::start` 的方式计时、处理 panic（记为 `Failed`），
    /// 交出带清单和耗时的集合。在调用线程上执行，`manifest()` 是纯内存计算。
    pub fn prepare(self) -> PreparedSet;
}

pub struct PreparedSet { /* Vec<{ Name, Arc<dyn Plugin>, Result<Manifest, String>, Duration }> */ }

impl PreparedSet {
    /// 已取得的清单，不再调用 `manifest()`；探针用它找出激活条件。清单 panic 的插件不出现。
    pub fn manifests(&self) -> impl Iterator<Item = (&Name, &Manifest)>;
    /// `id` 依赖的服务的提供者，传递闭包，不含 `id` 自己。只读已取得的清单。
    pub fn dependency_closure(&self, id: &str) -> Result<Vec<Name>, Vec<GraphError>>;
    /// 只保留 `keep` 返回 true 的插件。
    pub fn retain(self, keep: impl FnMut(&str) -> bool) -> Self;
    /// 加入一个插件，它的 `manifest()` 同样在这里调用一次并计时。
    pub fn with(self, id: impl Into<Name>, plugin: impl Plugin) -> Self;
    /// `PluginSet::validate` 的检查，只读已取得的清单。
    pub fn validate(&self) -> Result<(), Vec<GraphError>>;
}

impl Kernel {
    /// 与 `start` 相同，但用 `PreparedSet` 里的清单与耗时，不再调用 `manifest()`；
    /// `status()` 的 `manifest_time` 是 `prepare` / `with` 里那一次调用的耗时。
    pub fn start_prepared(set: PreparedSet) -> Arc<Kernel>;
}
```

`app`：`bench-probe` feature，打开 `lectern-bench/probe`；在上面写明的位置调用探针；实现 `Driver`；提供把应用数据目录指到临时目录的方式（运行器每次启动都用新的临时目录，不读写用户的真实数据）。具体写进 `app` 的设计文档。

`workspace`：画出某个里程碑状态的帧显示后、一段滚动或动画结束后，通过一个回调交给 `app`。里程碑的判定条件（「项目主页可用」指侧栏和会话列表画出真实内容而不是骨架）写进 `workspace` 和各视图的设计文档。

被测的领域、基础设施和 kernel 层 crate：见「夹具与 `test-support`」。

## 数据结构

### 基准目标

```
crates/bench/
  src/lib.rs          公开类型与 main；运行器在 runner.rs，采样在 ctx.rs，统计在 stats.rs，
                      报告在 report.rs，合并在 merge.rs，计数分配器在 alloc.rs
  src/budgets.rs      预算表
  src/bin/lectern-bench-report.rs  合并报告（见下）
  src/wire.rs         报告协议
  src/probe.rs        探针（probe feature）
  src/process.rs      进程级基准的调度（通过 Launcher / ProcessInfo 两个 trait 访问子进程，测试里换成假的）
  benches/time.rs     进程内计时基准，按 crate 分模块（benches/time/agents.rs 等）
  benches/memory.rs   进程内内存基准，注册 CountingAlloc
  benches/process.rs  进程级基准：app/*、plugin/*、baseline/*
  examples/empty_window.rs  空 GPUI 窗口（required-features = ["probe"]），只在 macOS 有内容，其他平台的 main 直接返回
  tests/alloc.rs      注册 CountingAlloc 的单独测试二进制（alloc_*）
  tests/fixtures/     本 crate 自己的小样本（目前没有）
```

三个基准目标都是 `harness = false`，入口都是 `lectern_bench::main`，各自把自己是哪个 `Target` 传给它。`time` 与 `memory` 分开，是为了计时不经过计数分配器。预算表里的 `Budget::target` 规定每条预算由哪个目标产出：`*_bytes`、`*/resident`（及其场景，如 `fs_watch/resident_100k`）、`*_peak` 这类进程内的字节数属于 `Memory`，`plugin/code-intel/resident_*` 也是（进程级基准实现之前，`code_intel` 的常驻份额在进程内用计数分配器测量，见 [code_intel 设计「与设计的差异」](code_intel.md#与设计的差异)），`log/idle_memory` 及其场景也是；`app/bundle_size`、`app/dmg_size` 属于 `Package`；`plugin/usage/activate`、`plugin/review/activate` 在进程级插件基准实现之前属于 `Time`（`TestKernel` 下进程内取 kernel 的四段耗时）；其余的 `app/*` 和 `plugin/*`、`lsp/*`、`baseline/*` 属于 `Process`；剩下的属于 `Time`。

另有一个 `src/bin/lectern-bench-report.rs`，有两个子命令：`merge` 合并各目标的报告，`package --app <.app> --dmg <.dmg>` 测量安装包、写 `report-package.json`（见「报告」）。它只用本 crate 的普通依赖。

### 报告

每个基准目标运行后打印自己的表（ID、实测统计量、预算、状态、出处），并写 `target/lectern-bench/report-<target>.json`（`report-time.json`、`report-memory.json`、`report-process.json`），三个目标不写同一个文件。每份报告列出属于这个目标的全部预算，各一条：被参数过滤掉的记为 `filtered`，`Planned` 的记为 `planned`。

`just bench` 在三个目标都运行完之后调用 `lectern-bench-report merge target/lectern-bench`：

- 读 `report-time.json`、`report-memory.json`、`report-process.json`，以及存在时的 `report-package.json`。检查每一条预算（`Active` 与 `Planned`）在这些报告里恰好出现一次，且出现在 `Budget::target` 指定的那一份里；缺少前三份之一（目标没有编译成功或中途崩溃）、缺少或重复的条目都按出错处理。
- `report-package.json` 是可选的：不存在时，合并工具自己为 `Package` 的每条预算加一条 `skipped`（原因「未打包」），不算缺失，所以普通的 `just bench` 不需要打包。打包流程（[#38](https://github.com/SoloJiang/lectern-app/issues/38) 的 macOS 任务）在构建 `.app` 与 `.dmg` 之后运行 `lectern-bench-report package --app <路径> --dmg <路径> target/lectern-bench`，再运行 `lectern-bench-report merge --require package target/lectern-bench`；带 `--require package` 时缺少这份报告按出错处理。
- `lsp/*/resident` 按 `report-process.json` 记录的语言服务器列表（这次运行里 `Child { kind: LanguageServer }` 报告过的 ID）展开，规则与下面的插件列表相同；没有语言服务器运行时列表为空，`lsp/*/resident` 本身出现一次，状态是 `skipped`（原因「没有运行的语言服务器」）、`planned` 或 `filtered`。
- 带 `*` 的预算先展开再检查：`report-process.json` 记录这次运行的插件列表（`app/idle_empty` 的 `Plugins` 应答，不含 `bench-probe`），合并工具把 `plugin/*/activate` 展开成列表里每个插件的 `plugin/<id>/activate`（已有具体条目的插件用具体条目），每个展开后的 ID 恰好出现一次。插件列表为空时（Linux、`app` 尚未接入、进程级基准被过滤或跳过），带 `*` 的条目本身出现一次，状态是 `planned`、`skipped` 或 `filtered`；插件列表不为空时，带 `*` 的条目本身不应出现。进程级目标自己的对应检查用同一套展开规则。
- 合并成 `target/lectern-bench/report.json`；设置了 `GITHUB_STEP_SUMMARY` 时追加一张 Markdown 表。在 GitHub Actions 里（`GITHUB_ACTIONS=true`）另为每条 `over` / `error` 的结果和每条报告级错误输出一条 `::error` 注释（每个步骤最多 10 条，超出的合成最后一条），不下载任务日志也能从 check run 的注释看到是哪条预算没过。同一环境下，状态为 `record` 的结果输出为 `::notice` 注释，按 ID 的第一段分组，每组一条：标题是 `<组> record`（如 `git record`），正文每行一条结果 `<ID>: <实测值>`，结果带 `note` 时接在后面（`<ID>: <实测值>; <说明>`）；`::error` 注释同样在原因之后接上 `note`。GitHub 每个步骤对每一级注释（error、warning、notice）各只保留 10 条，所以组数超过 10 时，第 10 条合并剩下的所有组，标题列出这些组名，正文照样逐行列出，不丢结果。CI 容器里取不到任务日志、产物和步骤摘要，只能经 API 读 check run 的注释，`git/changes_clean_100k/stat_all` 这类只记录的分项靠这些 notice 读出。冒烟模式的结果状态是 `smoke`，不输出 notice。
- 每份报告另有 `benches`：每个运行过的基准一项 `{id, seconds, attempts}`，`seconds` 是这个基准全部尝试的墙钟时间，从建临时目录到删除，夹具生成和重跑都算在内（[ADR 0036](../adr/0036.md)）。合并后的 `report.json` 给每项加上 `target`，按耗时从长到短排序；文本表和步骤摘要之后列出总数、合计耗时和最慢的 10 个。在 GitHub Actions 里这份列表是第一条 notice（标题 `slowest benchmarks`），`record` 的分组 notice 共用剩下的 9 条，超出的照样合进最后一条。
- 退出码按合并后的结果计算，规则与单个目标相同（0 / 1 / 2），`just bench` 以它为最终结果。

合并后的格式：

```json
{
  "version": 1,
  "mode": "measure",
  "gated": true,
  "host": { "model": "Mac14,2", "chip": "Apple M2", "os_version": "15.6", "cpus": 8 },
  "commit": "<GITHUB_SHA，没有时为 null>",
  "results": [
    {
      "id": "agents/parse_throughput",
      "status": "ok",
      "budget": "throughput ≥ 200 MB/s (median)",
      "source": "docs/design/agents.md#性能",
      "attempts": [ { "median": 0.17, "p95": 0.19, "max": 0.2, "mad": 0.004, "samples": 12, "unit": "s", "bytes": 52428800 } ],
      "noisy": false
    }
  ]
}
```

结果的 `note`（`Measured::note`，最后一次尝试的）存在时写在结果对象里，没有时省略。

状态取以下之一：`ok`、`over`、`record`（只记录）、`planned`、`filtered`、`smoke`、`skipped`（附原因，如「只在 macOS 运行」「sudo 不可用」）、`error`（附原因）。

## 数据流与线程

### 进程内基准

1. `main` 只对本目标检查对应关系：每条结果 ID 都在预算表里且 `target` 是本目标，本目标的每条 `Active` 预算都有基准产出它；别的目标的预算不参与。不对应时直接以 2 退出。冒烟模式同样检查，所以 Linux 任务就能发现漏登记的预算；跨目标的「恰好一次」由 `lectern-bench-report` 检查。
2. 在 macOS 上调用 `platform::thread::set_user_interactive()`；失败时以 2 退出（基准线程可能不在性能核上，数字不可信）。
3. 按 ID 顺序逐个运行，同一时间只运行一个基准。每个基准先在 `scratch()` 下生成夹具（不计时），再采样。
4. 统计、与预算比较。超标时用新的夹具把这个基准重跑一次，两次都超标才记为 `over`，两次的数据都写进报告。
5. 打印报告，写 JSON，返回退出码。

`memory` 目标的计数分配器只看到经 Rust 全局分配器的内存。被测 crate 用 C 的分配器（如 SQLite 的页缓存和连接）时，它的内存基准改为在子进程里测常驻增量：目标的 `main` 先检查一个只属于这个基准的环境变量，设置了就作为子进程运行，不进入 `lectern_bench::main`；基准在 `scratch()` 下生成夹具，用 `std::env::current_exe()` 启动子进程，子进程在开始前和做完被测操作之后各取一次本进程的常驻大小，打印在标准输出的一行里，基准取两者之差作为一个样本（`Value::Bytes`），Measure 模式启动 5 次，冒烟模式 1 次、用小规模的夹具。常驻大小在 macOS 上是 `platform::process::phys_footprint(std::process::id())`，与 `app/idle_*` 的口径相同；Linux 上读 `/proc/self/status` 的 `VmRSS`（只用于冒烟和 `--measure` 的记录，不判定）；其他平台为基准出错。用新的子进程，是因为生成夹具的进程里，被测操作会复用写夹具时释放的堆，增量偏小。目前用这种方式的只有 `cache/memory`：子进程打开应用级分库和 2 个项目分库、各读 50 MiB，同一个子进程里计数分配器的存活字节只增加约 5 KB，常驻增量约 4 MB。

### 进程级基准

运行器从环境变量 `LECTERN_BENCH_APP`（`lectern` 的路径）和 `LECTERN_BENCH_BASELINE`（`empty_window` 的路径）取得被测程序，由 `just bench` 设置。每次启动：建一个新的 socket 和临时数据目录，带上 `LECTERN_BENCH_SOCKET` 等环境变量启动子进程，等它连上来。

- 冷启动（`app/cold_start`、`baseline/empty_window_cold_start`）：先启动 2 次不计入（预热系统文件缓存），再启动 20 次。发布构建不带 binary archive（[ADR 0049](../adr/0049.md)），2 次预热同时填满系统的着色器缓存，测量的启动不含管线编译。探针注册首帧回调出错时发出 `Error { message: "hook <名字>" }` 并以退出码 3 退出，同样记为 `error`。每次收到 `FirstFrame` 后取 `presented - platform::process::start_time(pid)`，发 `Quit`，等进程退出（10 秒不退出就结束它并记为出错）。`presented` 是首帧的 `presentedTime`；机器不给时（托管 runner、锁屏），是显示回调运行的时刻（[ADR 0052](../adr/0052.md)）。两种来源的样本都照常判定。结果的 `note` 写明超过 20 ms 的个数、最大值、两种来源各多少个，以及按启动顺序的每个样本（毫秒）。
- 冷文件缓存（`app/cold_start_purged`）：`sudo -n purge` 之后启动 1 次，只记录（ADR 0017）。`sudo -n` 失败时记为 `skipped`。
- 空闲内存（`app/idle_empty`）：启动 3 次，每次按 [kernel「静止」](kernel.md#静止)采样：收到 `Quiescent` 后等 5 秒，期间收到 `Busy` 就重新等，第一次 `Epoch` 的应答到达之前收到的 `Busy` 也算在这段等待里；然后发 `Epoch`、读 `phys_footprint(pid)`、再发 `Epoch`，两次都是同一个 `Some(n)` 时样本有效，否则重新等静止，连续 3 次无效记为出错。首帧后 30 秒内没有静止记为 `over`（ADR 0017）。取 3 次的中位数。可暂停的后台工作（[ADR 0029](../adr/0029.md)，打开项目后的建索引）：运行器在静止后一律请求暂停，不看当时有没有后台工作，等 kernel 报告后台工作为「没有」或「全部已暂停」再开始 5 秒的等待，暂停一直保持到采样结束；`app/idle_monorepo` 因此测的是后台索引暂停时的内存。`Epoch` 的应答同时带上 kernel 的后台代数和后台工作状态：5 秒等待开始时和采样之后各取一次，两次的 `quiescent_epoch()` 和后台代数都相同、后台工作一直是「没有」或「全部已暂停」时样本才有效，否则按上面的规则重新等待。暂停请求和后台工作的字段随第 5 步加入报告协议。`app/idle_empty` 只计主进程。子进程按 ADR 0018 以进程组为单位统计：对 `Child` 报告的每个根进程，运行器用 `ProcessInfo::children` 逐层展开，得到根进程及其全部后代（广度优先，按 pid 去重；展开期间已经退出的进程跳过），这一组的 `phys_footprint` 之和是这个插件的内存。`app/idle_empty` 不计算 `plugin/process_total`：空窗口里按 `Workspace` / `Command` / `Manual` 激活的进程插件都没有启动。主进程的后代中不属于任何已报告进程组的（含 `ChildKind::Other`），合计记为 `app/unclassified_children`（只记录），用来发现没有报告的子进程。展开与读取在同一次采样里进行，前后两次 `Epoch` 检查同样覆盖它们。同一次启动里发 `Plugins`，激活条件里含 `AfterFirstFrame` 的插件（不论它排在第几项、是否还有别的条件）四段耗时之和给出 `plugin/after_first_frame`。`baseline/empty_window_idle` 没有 kernel，在首帧的时刻之后 5 秒采样，结果的 `note` 写明首帧的来源和 3 个值。
- 后台工作完成后的内存（`app/idle_monorepo_indexed`，ADR 0029）：打开 cloudprint 形状的夹具，不暂停后台工作，等 kernel 报告后台工作计数归零并静止后，按上面的方式等 5 秒、采样；5 秒等待开始时和采样之后的两次 `Epoch` 应答里，`quiescent_epoch()` 和后台代数都相同、后台工作计数都为零时样本才有效，否则重新等待，连续 3 次无效记为出错。取 3 次启动的中位数。
- 进程插件合计（`plugin/process_total`）与语言服务器（`lsp/*`）：单独启动一次完整的 profile（进程插件宿主从安装目录加载的插件都在内；macOS CI 在这一步之前安装索引里的全部官方进程插件，方式由进程插件宿主的实现决定），先 `Open(Project(<cloudprint 夹具>))` 并等静止，再发 `ActivateAll`，等到 `Ready`（30 秒）和有效的静止样本后采样。所有 `ProcessPlugin` 进程组之和是 `plugin/process_total`，这是 ADR 0018 要求的「多个插件同时运行时的合计」。这次启动里报告过的每个 `LanguageServer` 组记为 `lsp/<id>/resident`，合计记为 `lsp/total`，只记录；没有语言服务器运行时 `lsp/total` 为 0。
- 每个插件（`plugin/<id>/*`）：插件列表取自 `app/idle_empty` 的 `Plugins` 应答（不含 `bench-probe`）。对每个插件启动两次，`LECTERN_BENCH_PLUGINS` 分别为 `closure:<id>` 和 `closure+self:<id>`，都等到 `Ready` 和有效的静止样本。每次启动的内存是主进程的 `phys_footprint` 加上这次启动里所有 `ProcessPlugin` 进程组（按上面的展开规则取根进程及其全部后代）的合计，语言服务器组不计入、单独报告；两次之差是 `plugin/<id>/resident`，所以进程插件自己的进程组（闭包那一次不存在，记为 0）计入它的常驻内存。第二次里这个插件的四段耗时之和是 `plugin/<id>/activate`（[kernel「性能」](kernel.md#性能)）。超标时再测两对，取三对的中位数。预算按项目形状给出的插件（如 `project`），两次启动都先 `Open(Project(<夹具>))`。
- 端到端（`app/open_project` 等）：每个样本是一次新的启动。等空窗口静止后（打开文件、Diff、切换 worktree 的，先打开 `cloudprint` 形状的夹具项目并再次静止）发 `Open(...)`，从发出请求（运行器取 `SystemTime::now()`）到对应 `Milestone` 的 `presented` 为一个样本，两者是同一台机器的系统时钟；每项 5 次启动，取中位数。帧时间按两个条件判定，在任何刷新率的显示器上都成立：
  - 每帧工作时间 `completed - requested` 的最大值 ≤ 8.3 ms。从帧源回调算起，所以主线程在布局之前的阻塞、布局、绘制和 GPU 都计入。
  - 不掉帧：相邻两帧 `started` 的间隔都 ≤ 1.5 个刷新周期。主线程阻塞超过一个周期时会错过一次 display link 回调，间隔变成两个周期以上。

  GPUI 按显示器的 display link 发出帧，关不掉也不改（ADR 0016），60 Hz 的 CI 虚拟显示器上间隔本身约 16.7 ms，所以不直接拿间隔和 8.3 ms 比。在 120 Hz 的显示器上，两个条件合起来与「最大帧间隔 ≤ 8.3 ms」等价。报告写明 `refresh`。`app/input_frames`：打开夹具项目、切到 Conversation 的 Terminal 页（终端是核心插件）后发 `Open(Input { text: <400 个字符的固定文本>, interval: 8 ms })`；`app` 的 `Driver` 把每个字符作为一次按键送进与真实键盘相同的分发路径，`workspace` 在注入期间持续请求帧并用 `platform::frame::record` 记录，结束后经 `Probe::frames` 交回（场景名 `input`），按上面的两个条件判定。按键处理阻塞主线程时，这一帧的工作时间变长、或者错过一次帧源回调，任一条件不满足即超标。
- 安装包（`app/bundle_size`、`app/dmg_size`）：不在 `process` 目标里。`lectern-bench-report package` 读参数给出的产物，`.app` 按目录下全部文件实际占用的块数求和（与 `du -sk` 相同），`.dmg` 取文件大小，写 `report-package.json`。目标 50 MB 只在报告里显示。

计时类的进程级基准逐个运行，期间不运行别的基准；内存类最多同时运行 3 个，只影响等待时间，不影响 `phys_footprint`。

每个等待都有期限，到期时运行器结束子进程（子进程在自己的进程组里启动，向整个进程组发 `SIGKILL`，再按上面的展开规则结束仍然存活的后代），把基准记为 `error`，原因写明在等哪一条消息：

| 等待 | 期限 |
|---|---|
| 启动后连上 socket 并收到 `FirstFrame` | 10 秒 |
| 首帧后的第一次 `Quiescent` | 首帧后 30 秒（ADR 0017；超时记为 `over`，不是 `error`） |
| 发 `Open(...)`、`ActivateAll` 或满足激活条件后的再次 `Quiescent`、`Ready` | 30 秒 |
| 请求暂停后 kernel 报告后台工作「全部已暂停」 | 10 秒 |
| `app/idle_monorepo_indexed` 打开夹具后后台工作全部完成 | 120 秒 |
| `Epoch`、`Plugins` … `PluginsEnd` 的应答 | 10 秒 |
| `Open(...)` 之后的 `Milestone` | 30 秒与这条预算上限的 1.5 倍中较长的一个（`app/index_100k` 的 120 秒预算为 180 秒） |
| `Open(Scroll/Animate/Input)` 之后的 `Frames` | 场景时长加 10 秒（输入场景 400 × 8 ms，约 13 秒） |
| `Quit` 之后进程退出 | 10 秒 |

规则：一个等待的期限不得短于它所测预算的上限，并留出余量（取上限的 1.5 倍），否则慢于期限、但仍在预算内的样本会被记为 `error`，超出预算的样本也拿不到实测值。新增预算时按这条规则检查对应等待的期限。

插件相关的基准（`plugin/*`）期间收到 `PluginFailed`，或 `Plugins` 应答里所选插件的状态是 `Failed`，立即按上面的方式结束子进程并记为 `error`，不等期限。

运行器只通过 `Launcher`（启动、取 pid、等待、结束整个进程组）和 `ProcessInfo`（`start_time`、`phys_footprint`、`children`）两个 trait 访问子进程；基准目标里用 `std::process` 和 `platform::process` 实现，单元测试里用脚本化的假实现。

## 预算表

初始条目如下，数字以出处为准。表中「加入」一列写的是哪一步或哪个 crate 的 PR 把它从「计划中」改为生效。

顶层预算（`Tier::Top`，改动先写 ADR）：

| ID | 统计量与预算 | 出处 | 加入 |
|---|---|---|---|
| `app/cold_start` | 20 次里 > 20 ms 的不超过 1 次，且最大值 ≤ 30 ms | [ADR 0017](../adr/0017.md#决策) | 第 5 步 |
| `app/cold_start_purged` | 只记录 | [ADR 0017](../adr/0017.md#影响) | 第 5 步 |
| `app/idle_empty` | 中位数 < 60 MB；首帧后 30 秒内静止 | [ADR 0017](../adr/0017.md#决策) | 第 5 步 |
| `app/idle_monorepo` | 中位数 < 100 MB（后台工作已暂停，[ADR 0029](../adr/0029.md)） | [ADR 0017](../adr/0017.md#决策) | `project_home` |
| `app/idle_monorepo_indexed` | 中位数 < 100 MB（后台工作全部完成后） | [ADR 0029](../adr/0029.md#决策) | `code_intel`（[#17](https://github.com/SoloJiang/lectern-app/issues/17)） |
| `app/idle_100k`、`app/idle_100k_indexed` | 中位数 < 120 MB（[project 设计「性能」](project.md#性能)的 10 万文件形状、17 个 worktree；采样同 `app/idle_monorepo` 与 `app/idle_monorepo_indexed`，后者在 `app/index_100k` 的启动里、索引全部完成并静止后采样；份额的分配见 ADR 0035） | [ADR 0035](../adr/0035.md#决策) | `project_home` / `code_intel`（#17） |
| `app/open_project` | 中位数 < 100 ms | [性能预算](tech-research.md#性能预算) | `project_home` |
| `app/open_file` | 中位数 < 16 ms；场景：cloudprint 形状的普通文件，另有 4 MiB 与 64 MiB 的合成源码、压缩成一行的 4 MiB JS、从 diff 打开 4 MiB `package-lock.json` 中部的一个 hunk（按字节偏移定位）、从 diff 的旧一侧打开 4 MiB blob 中部的一个 hunk（对象库内容已由审阅视图登记），预算相同（[ADR 0034](../adr/0034.md)、[ADR 0042](../adr/0042.md)） | [性能预算](tech-research.md#性能预算) | `file_reader` |
| `app/enter_edit` | 中位数 ≤ 100 ms（1 MiB、没有超过 4 KiB 的行的文件从 ⌘E 到可以输入；超标时降低编辑上限，[ADR 0034](../adr/0034.md)） | [交互规范「状态与异常」](interaction.md#状态与异常)（100 ms 内不显示加载态） | `file_reader` |
| `app/open_diff` | 中位数 < 50 ms | [性能预算](tech-research.md#性能预算) | `review_view` |
| `app/switch_worktree` | 中位数 < 100 ms | [性能预算](tech-research.md#性能预算) | `worktrees` |
| `app/palette_open` | 中位数 < 16 ms（面板和空查询列表） | [性能预算](tech-research.md#性能预算)（[ADR 0030](../adr/0030.md)，下同） | ⌘K（[#102](https://github.com/SoloJiang/lectern-app/issues/102)） |
| `app/palette_key_200k`、`app/palette_key_1m` | 每次按键到结果更新，中位数 ≤ 16 ms / ≤ 50 ms（20 万条和 100 万条路径的合成仓库：分档预算在档位上界测量） | [性能预算](tech-research.md#性能预算) | ⌘K（#102） |
| `app/files_open`、`app/files_expand` | 中位数 < 16 ms（目录表已建好；展开 2,000 个子项以内的目录） | [性能预算](tech-research.md#性能预算) | Files（[#10](https://github.com/SoloJiang/lectern-app/issues/10)） |
| `app/nav_back` | 中位数 < 16 ms（回到最近访问的位置） | [性能预算](tech-research.md#性能预算) | Files（#10） |
| `app/goto_candidates`、`app/goto_single` | 中位数 ≤ 50 ms（候选列表，96 KiB 以内的当前文本；96–128 KiB 的在解析保留之后，[ADR 0037](../adr/0037.md)）/ ≤ 100 ms（唯一候选的目标文件首屏高亮） | [性能预算](tech-research.md#性能预算) | `code_intel`（#17） |
| `app/goto_unindexed` | 第一批结果中位数 ≤ 300 ms | [性能预算](tech-research.md#性能预算) | `code_intel`（#17） |
| `app/goto_unindexed_done` | 从请求到限时搜索结束（找完或到时停止）、最后一批结果画出，最大值 ≤ 1.1 s（1 s 限时加 0.1 s 取消与绘制的余量）；夹具里未索引的文件要多到 1 s 内搜不完 | [性能预算](tech-research.md#性能预算) | `code_intel`（#17） |
| `app/references_first`、`app/references_100k` | 中位数 ≤ 200 ms（第一批）/ ≤ 2 s（10 万文件的全部结果） | [性能预算](tech-research.md#性能预算) | 上下文抽屉（[#20](https://github.com/SoloJiang/lectern-app/issues/20)） |
| `app/index_review_set`、`app/index_subproject`、`app/index_100k` | 打开项目到这一层全部入索引，中位数 ≤ 2 s / ≤ 10 s / ≤ 120 s（接电源）；`app/index_100k` 的夹具含一个达到依赖层上限（64 MiB 源码）的 `node_modules`，依赖层排在第 4 层之后，不推迟里程碑（[ADR 0033](../adr/0033.md)） | [性能预算](tech-research.md#性能预算) | `code_intel`（#17） |
| `app/index_100k_interaction` | 只记录（与 `app/index_100k` 相同，建索引期间按脚本持续滚动，记下第 3、4 层的完成时间） | [ADR 0033](../adr/0033.md) | `code_intel`（#17） |
| `app/scroll_frames` | 每帧工作时间最大值 ≤ 8.3 ms，且相邻帧间隔都 ≤ 1.5 个刷新周期；场景另有 64 MiB 的合成源码、压缩成一行的文件和编辑模式下 1 MiB 的文件（[ADR 0034](../adr/0034.md)） | [性能预算](tech-research.md#性能预算)「滚动与输入」 | `file_reader` |
| `app/scroll_frames_indexing` | 同 `app/scroll_frames`，在批量层有工作（打开 10 万文件的夹具后立即滚动）时测量；不让路就超标时启用 kernel 的让路开关，并以这个基准校准它（[ADR 0033](../adr/0033.md)） | [性能预算](tech-research.md#性能预算)「滚动与输入」 | `file_reader`、`code_intel`（#17） |
| `app/input_frames` | 每帧工作时间最大值 ≤ 8.3 ms，且相邻帧间隔都 ≤ 1.5 个刷新周期 | [性能预算](tech-research.md#性能预算)「滚动与输入」 | `terminal_view` |
| `app/animation_frames` | 每帧工作时间最大值 ≤ 8.3 ms，且相邻帧间隔都 ≤ 1.5 个刷新周期 | [ADR 0015](../adr/0015.md) | `ui` |
| `app/bundle_size` | ≤ 80 MB（目标 50 MB 只显示）；`Package` | [ADR 0018](../adr/0018.md#决策) | [#38](https://github.com/SoloJiang/lectern-app/issues/38) |
| `app/dmg_size` | ≤ 40 MB；`Package` | [ADR 0018](../adr/0018.md#决策) | [#38](https://github.com/SoloJiang/lectern-app/issues/38) |
| `plugin/process_total` | 全部进程插件同时激活时，各进程组（根进程及其全部后代）合计 ≤ 50 MB | [ADR 0018](../adr/0018.md#影响) | `kernel` 的进程插件宿主 |
| `lsp/*/resident`、`lsp/total` | 只记录（ADR 0018：语言服务器不计入总预算，单独报告） | [ADR 0018](../adr/0018.md#影响) | `code_intel` |
| `app/unclassified_children` | 只记录 | [ADR 0018](../adr/0018.md#影响) | 第 5 步 |
| `baseline/empty_window_cold_start` | 只记录 | [ADR 0017](../adr/0017.md#影响) | 第 4 步 |
| `baseline/empty_window_idle` | 只记录 | [ADR 0017](../adr/0017.md#影响) | 第 4 步 |

分项预算（`Tier::Share`，随出处的设计文档修改）：

| ID | 统计量与预算 | 出处 | 加入 |
|---|---|---|---|
| `plugin/*/activate` | 四段之和 < 1 ms | [kernel](kernel.md#性能) | 第 5 步 |
| `plugin/after_first_frame` | 激活条件含 `AfterFirstFrame` 的插件四段之和合计 < 5 ms | [kernel](kernel.md#性能) | 第 5 步 |
| `plugin/*/resident` | 只记录；插件的设计文档给出数字时另加具体条目 | [kernel](kernel.md#性能) | 第 5 步 |
| `plugin/project/resident_cloudprint`、`plugin/project/resident_100k` | < 2 MB / < 16 MB | [project](project.md#性能) | 第 5 步（进程级，两次启动的 `phys_footprint`；在此之前进程内的同一测量见 `project/resident_*_bytes`） |
| `agents/parse_throughput` | 中位数吞吐 ≥ 200 MB/s | [agents](agents.md#性能) | 第 1 步 |
| `agents/append_4k` | 中位数 < 1 ms | [agents](agents.md#性能) | 第 1 步 |
| `agents/discover_1000` | 中位数 < 500 ms | [agents](agents.md#性能) | `agents` 第 4 步 |
| `agents/turns_cached_50mb` | 中位数 < 10 ms | [agents](agents.md#性能) | `agents` 第 4 步 |
| `agents/discover_codex_1000` | 中位数 < 500 ms | [agents](agents.md#性能) | `agents` 第 5 步（[#97](https://github.com/SoloJiang/lectern-app/issues/97)） |
| `agents/zst_throughput` | 中位数吞吐 ≥ 100 MB/s | [agents](agents.md#性能) | 第 1 步（`agents` 第 3 步先以 `perf_smoke` 测量） |
| `agents/zst_load_decoded` | 中位数 < 1 ms | [agents](agents.md#性能) | 第 1 步（同上） |
| `git/open` | 中位数 < 15 ms | [git](git.md#性能) | 第 1 步 |
| `git/open_handle` | 中位数 < 1 ms | [git](git.md#性能) | 第 1 步 |
| `git/changes_clean`、`git/changes_50` | 中位数 < 15 ms | [git](git.md#性能) | `git` 第 3 步 |
| `git/refresh_one`、`git/refresh_full` | 中位数 < 15 ms（`refresh(Full)` 的份额，一个路径的刷新不会更慢） | [git](git.md#性能) | `git` 第 3 步 |
| `git/diff_50_cold` / `git/diff_50_warm` | 中位数 < 20 ms / < 2 ms | [git](git.md#性能) | 第 1 步（`git` 第 2 步先以 `perf_smoke` 测量） |
| `git/changes_capped` | 中位数 < 1 s | [git](git.md#性能) | `git` 第 3 步 |
| `git/changes_capped_bytes` | 存活字节 ≤ `Limits` 默认上限之和（数字由 `git` 的实现写入 git 设计） | [git](git.md#性能) | `git` 第 3 步 |
| `git/changes_count/total_bytes` | 只记录（1000 个改了的跟踪文件、1000 个未跟踪文件，各 10–50 KB，加一个 16 MiB 的未跟踪文件，按项目主页的计数比较取 `retained_bytes = 0`、`total_patch_bytes = 0`；改动集连同 `Worktree` 持有的存活字节；stat 缓存不留住文件内容，比较用的 index 在调用返回时释放；`Memory`） | [git](git.md#性能) | 随 [ADR 0035](../adr/0035.md) 加入 |
| `git/changes_count/per_path_bytes` | 存活字节 ≤ 1 KB（同上除以改动路径数；ADR 0035 按它给首页的改动集定上限；`Memory`） | [git](git.md#性能) | `git` 第 4 步（释放比较用的 index；另一个前提 stat 缓存不持有内容） |
| `git/resident` | 存活字节 ≤ 48 MB（hunk LRU 16 MB + 一个 `ChangeSet` 32 MB，空闲时不持有句柄） | [git](git.md#性能) | `git` 第 3 步 |
| `git/open_100k`、`git/filter_100k`、`git/changes_clean_100k` | 中位数 < 40 ms / < 400 ms / < 2 s（10 万个跟踪文件、`core.ignoreCase`：`git/open` 的读取，index 不在缓存里；只有 index 变化后 fs_watch 重建过滤器，给出在用的过滤器、复用它的未跟踪遍历，用自己的一份仓库，因为要改 index；干净工作区的 `changes`，每个样本用新的 `Worktree`，index 每次都解析） | [git](git.md#性能) | `git/open_100k` 随 [ADR 0035](../adr/0035.md) 加入；`git/changes_clean_100k` 在 macOS CI 上是 9.05–9.54 s，cache-tree 跳过基准树与并行 stat（[ADR 0038](../adr/0038.md)）之后改回生效；`git/filter_100k` 原来测完整构造，在 macOS CI 上是 678–766 ms（目录遍历单独 473 ms），复用遍历之后改测这次重建并生效（git 设计「index」） |
| `git/filter_100k/cold`、`git/filter_100k/walk`、`git/changes_clean_100k/stat_all` | 只记录（共享仓库：没有在用过滤器时的第一次构造，含完整的未跟踪遍历；经 `SafeRoot::list` 列出工作区的每个目录一次；对每个跟踪路径 `SafeRoot::stat` 一次。用来定位上一行在 macOS 上的开销；原来的 `git/changes_clean_100k/read_all` 已去掉，见「实际验证」） | [git](git.md#性能) | 随 ADR 0035 加入；`…/cold` 随复用遍历加入 |
| `git/changes_clean_100k/reads`、`…/stat_racy`、`…/stat_unmatched`、`…/stat_mtime`、`…/stat_size`、`…/stat_ctime`、`…/stat_ino`、`…/stat_owner` | 只记录，计数（`Value::Count`；同一仓库：新 `Worktree` 上第一次比较读过的文件数，读取之前 index 的 stat 与 `SafeRoot::stat` 逐项不一致的条目数。分辨 macOS 上的开销是比较去读了内容，还是 stat 与遍历本身慢，见 [git 设计「与设计的差异」](git.md#与设计的差异)） | [git](git.md#性能) | `git` 的 stat 缓存修正 |
| `git/changes_clean_100k/phase_open`、`…/phase_index`、`…/phase_base_tree`、`…/phase_setup`、`…/phase_stat`、`…/phase_fold`、`…/phase_untracked`、`…/phase_attributes`、`…/phase_classify`、`…/phase_assemble`、`…/phase_finish`、`…/phase_rest` | 只记录（同一仓库上一次冷的比较按阶段的墙钟时间，各 1 个样本；`…/phase_rest` 是整个调用减去各阶段之和。ID 用 `phase_<名字>`，不用 `phase/<名字>`：ID 最多三段） | [git](git.md#性能) | `git` 的比较分阶段计时 |
| `git/compare_100k/peak_bytes` | 存活字节 ≤ 72 MB（同一仓库上新 `Worktree` 的一次 `changes` 期间的峰值：比较自己解析的 index、折叠副本、未跟踪遍历、改动集；临时内存，不是空闲份额；`Memory`） | [git](git.md#性能) | 随 ADR 0035 加入 |
| `git/compare_100k/retained_bytes` | 存活字节 < 1 MB（同一次比较返回、丢掉改动集之后 `Worktree` 比打开时多持有的字节：比较用的 index 不在调用之间保留；`Memory`） | [git](git.md#性能) | `git` 第 4 步 |
| `git/refresh_one_100k` | 只记录（同一仓库，干净的改动集上对一个没有改动的普通跟踪文件做 `refresh(Paths)`：一次 fs_watch 事件的开销，主要是每次重新解析 index；`git/refresh_one` 的 < 15 ms 是 cloudprint 量级的） | [git](git.md#性能) | `git` 第 4 步 |
| `git/canonical_oid_100k`、`git/canonical_oid_100k/batch_100` | 只记录（同一仓库，每个样本新开 `Worktree`：一次 `canonical_oid`，即 `code_intel` 的 git 适配每次调用的开销；100 个路径一次 `canonical_oids`。`code_intel` 的设计没有给它单独的份额，计入调用方的预算） | [git](git.md#性能) | `git` 第 4 步 |
| `git/filter_cloudprint_bytes`、`git/filter_100k_bytes` | 存活字节 < 1 MB / ≤ 16 MB（`cloudprint` 形状 / 10 万文件的仓库，`core.ignoreCase`；fs_watch 过滤器连同它的 `Worktree` 句柄：与 `project` 的句柄共用的解析后 index、折叠顺序、忽略规则；`git` 的份额，`fs_watch/resident*` 和 `project/resident_*` 不含它；`Memory`） | [git](git.md#性能) | 随 ADR 0035 加入 |
| `cache/get_4k`、`cache/get_many_50`、`cache/get_1m` | 中位数 < 50 µs / < 1 ms / < 2 ms（分库已打开） | [cache](cache.md#性能) | 第 1 步（`cache` 第 1 步先以 `perf_smoke` 测量） |
| `cache/open_existing`、`cache/open_new` | 中位数 < 5 ms / < 10 ms | [cache](cache.md#性能) | 第 1 步（同上） |
| `cache/put_4k`、`cache/batch_1000` | 中位数 < 1 ms / < 15 ms | [cache](cache.md#性能) | 第 1 步（同上） |
| `cache/zstd_1m/put`、`cache/zstd_1m/get` | 只记录（命名空间选 `Zstd` 之前对照使用方的预算） | [cache](cache.md#性能) | 第 1 步（同上） |
| `cache/evict` | 中位数 < 30 ms（超出预算的分库里一次小写入，淘汰 1 MiB；原为 `cache/evict_16m`，见 [cache 设计「与设计的差异」](cache.md#与设计的差异)） | [cache](cache.md#性能) | `cache` 第 2 步 |
| `cache/memory` | 子进程的常驻增量中位数 ≤ 8 MB（见「进程内基准」）；`Memory` | [cache](cache.md#性能) | `cache` 第 2 步 |
| `cache/tables_write`、`cache/tables_evict` | 中位数 < 15 ms / < 30 ms（自定义表：一次写入 64 个单元及其行；同样一批写进超出配额的命名空间） | [cache](cache.md#性能) | `cache` 第 3 步 |
| `cache/tables_read`、`cache/tables_touch` | 中位数 < 50 µs / < 20 ms（一次 `read` 按索引取 1 行；`touch` 5000 个过期单元，调用里写回 4096 个） | [cache](cache.md#性能) | `cache` 第 3 步 |
| `project/open_cloudprint` | 中位数 < 20 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/packages_cloudprint` | 中位数 < 15 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/open_100k/files`、`project/open_100k/packages` | 中位数 < 80 ms / < 150 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/switch_worktree` | 中位数 < 30 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/resolve_package/hit`、`…/miss_cloudprint`、`…/miss_100k` | 中位数 < 1 ms / < 20 ms / < 150 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/refresh_index/cloudprint`、`…/100k` | 中位数 < 15 ms / < 60 ms（只计模型那一轮；新 index 的解析是 fs_watch 重建过滤器时 `git` 的工作，在批次交付之前完成，不计入） | [project](project.md#性能) | `project` 第 5 步 |
| `project/facts_contains/name`、`…/path` | 中位数 < 1 µs / < 5 ms（未缓存：模式的结果还没有缓存，每组新 facts 上每个模式各问一次） | [project](project.md#性能) | `project` 第 5 步 |
| `project/facts_contains/name_first` | 只记录（每组新 facts 上的第一次调用） | [project](project.md#性能) | `project` 第 5 步 |
| `project/resident_cloudprint_bytes`、`project/resident_100k_bytes` | 中位数 < 2 MB / < 16 MB（`project` 插件静止后的存活字节：只有依赖闭包、但 arm 同样监听的 kernel，与加上 `project` 并打开夹具、填满非当前 worktree 的子项目缓存之后的 kernel 之差，同一进程里按 kernel「性能」测；fs_watch 的监听和过滤器（连同过滤器持有的解析后 index）两边都有，分别归 `fs_watch/resident*` 和 `git/filter_*_bytes`；与 `plugin/project/resident_*` 同一数字；`Memory`） | [project](project.md#性能) | `project` 第 5 步 |
| `fs_watch/classify_burst` | 中位数 < 100 ms（`FakeBackend` 注入 10 万个 `node_modules/**` 事件和 1000 个受跟踪路径事件，728 个跟踪文件、20 条忽略规则） | [fs_watch](fs_watch.md#性能) | `fs_watch` 第 1 步（基准由 [#92](https://github.com/SoloJiang/lectern-app/pull/92) 加入） |
| `fs_watch/reflog_append` | 中位数 < 1 ms（追加一行 reflog 到记录进入批次，不含后端延迟） | [fs_watch](fs_watch.md#性能) | `fs_watch` 第 1 步（同上） |
| `fs_watch/flush_idle`、`fs_watch/write_to_deliver` | 中位数 < 5 ms / < 120 ms（macOS 真实后端） | [fs_watch](fs_watch.md#性能) | `fs_watch` 第 4 步 |
| `fs_watch/resident`、`fs_watch/resident_100k` | 常驻增量 < 1 MB / < 1 MB（11 个 worktree、728 个文件的仓库 / 10 万文件、17 个 worktree 的仓库）；只计本 crate 自己的状态，过滤器和它持有的解析后 index 归 `git`（ADR 0035）；`Memory` | [fs_watch](fs_watch.md#性能) | `fs_watch` 第 4 步 |
| `code_intel/extract_throughput` | 中位数吞吐 ≥ 4 MB/s（单线程，首发的每种语言约 1 MiB，切成约 8 KiB 的文件，看门狗在运行；核心语言用一次遍历抽取；测量方式和预算的推导见 [ADR 0031](../adr/0031.md)） | [code_intel](code_intel.md#性能) | `code_intel` 第 1 步 |
| `code_intel/extract_1mib` | 只记录（每种语言一个约 1 MiB 的文件，对照单文件 250 ms 的截止时间） | [code_intel](code_intel.md#性能) | `code_intel` 第 1 步 |
| `code_intel/warm_100k` | 中位数 < 500 ms（热启动的存在检查：缓存里已有 10 万个文件的单元，200 次 500 个 OID 的查询，只读、不标记访问） | [code_intel](code_intel.md#性能) | `code_intel` 第 2 步 |
| `code_intel/warm_100k_touch` | 只记录（存在检查之后推迟进行的访问标记：10 万个命中的单元每次 1024 个，访问时间都已过 1 小时，全部要写回） | [code_intel](code_intel.md#性能) | `code_intel` 第 2 步 |
| `code_intel/map_100k`、`code_intel/map_100k_bytes` | 中位数 ≤ 20 ms / ≤ 3 MB（10 万文件的「blob → 路径」映射重建：基础部分和按路径沿用的提示，合成的 OID；字节数不含 `project` 的文件列表；`Memory`） | [code_intel](code_intel.md#性能) | `code_intel` 第 3 步 |
| `code_intel/batch_peak` | 最大值 ≤ 40 MB（批量层为合成仓库建索引期间的活跃字节峰值：8 KiB 的文件和接近 1 MiB 的文件，语法树经 tree-sitter 的分配器计入；`Memory`） | [code_intel](code_intel.md#性能) | `code_intel` 第 3 步 |
| `code_intel/disk_100k` | 只记录（10 万文件的合成仓库全部写入之后分库文件的大小，含 `-wal`；字节数，由 `time` 目标产出） | [code_intel](code_intel.md#性能) | `code_intel` 第 3 步 |
| `code_intel/goto_indexed`、`code_intel/goto_indexed/done` | 中位数 ≤ 20 ms / 只记录（索引命中的跳转：10 万个已入索引的文件、每 1000 个一个子项目，约 100 KB 的当前文本，它是当前文本、解析已保留（[ADR 0037](../adr/0037.md)），核算的遍历已缓存；从 `query` 到第一批有候选的事件 / 到 `Done`） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/goto_uncached`、`code_intel/goto_popular`、`code_intel/goto_full` 和各自的 `/done` | 中位数 ≤ 20 ms / 只记录，`goto_full/done` 最大值 ≤ 1.02 s（同上，分别在首次跳转会遇到的状态下：每次跳转前忘掉核算缓存的遍历；约 3000 个文件定义的常见名字（每 17 个文件一个，跳转的语言组约占一半）；命名空间满额，核算核对存储） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/goto_cold_parse`、`code_intel/goto_cold_parse/at_128k` | 中位数 ≤ 32 ms / 只记录（同上的跳转，解析没有保留、为光标处解析的冷路径：当前文本略小于 96 KiB，候选 50 ms 减去抽屉的 18 ms；略小于 128 KiB（保留解析的上限），那里的 50 ms 只靠保留的解析成立，冷路径是已知限制，[ADR 0037](../adr/0037.md)） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/goto_same_file`、`code_intel/goto_object` | 只记录（同上的跳转：跳到当前文本里 locals 绑定的 `File` 定义；当前文本是对象库里的 blob（Diff 的旧一侧）；对照候选 50 ms） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/parse_cache_bytes` | 最大值 ≤ 12 MiB（保留的上限处：两个略小于 128 KiB 的 TypeScript 文本先后成为当前文本，两份解析都保留时它们的活跃字节，tree-sitter 的分配计入；`Memory`，[ADR 0037](../adr/0037.md)） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/goto_first` | 只记录（新索引的第一次跳转：cloudprint 形状、打开并静止之后，每个样本一个新的语言表；对照候选 50 ms） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/named_50k`、`code_intel/named_50k/done`、`code_intel/named_50k/candidates`、`code_intel/named_50k/unsearched` | 只记录（`Named` 查共享的 10 万文件虚拟仓库里约 4.5 万个文件都定义的名字 `id`：从 `query` 到第一批，第一批必须到达且有候选；到 `Done` 的时间，`Done` 时的候选数和未搜的文件数。其余的查找要用 1 s 时间盒的大半，慢的机器上结果是 `Partial`，只记录、不算失败） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/search_stop` | 最大值 ≤ 1.02 s（6000 个未入索引、每个都命中的文件，从 `query` 到最后一批的事件） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/references_100k/all`、`code_intel/references_100k/first` | 中位数 ≤ 2 s / ≤ 200 ms（10 万个已入索引的文件里 300 个用到这个名字：到最后一批 / 到第一批带引用的事件） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/references_full/all`、`code_intel/references_full/first` | 中位数 ≤ 2 s / ≤ 200 ms（同上，命名空间满额；每轮都要 `Complete`，查引用有自己的 1.9 s 时间盒；被时间盒截断的一轮是两项都不合格的样本，不是错误，见「时间盒截断的轮次」，[ADR 0043](../adr/0043.md)） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/references_full/partial_rounds`、`code_intel/references_full/outside` | 只记录（被时间盒截断的轮数，含预热；一轮留在范围外的最多文件数；有截断时带最后一轮的诊断，[ADR 0043](../adr/0043.md)） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/names_100k`、`code_intel/names_100k_bytes` | 中位数 ≤ 1 s / ≤ 8 MB（10 万文件的合成抽取结果，约 32 万个名字；名字表的构建与它的活跃字节；`Memory`） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `code_intel/reread_crlf_1000` | 只记录（`git` 的 `TestRepo` 上 1 万个跟踪文件、`core.autocrlf = true`，1000 个文件进入重新读集合，到覆盖部分装上；整批的 `canonical_oids`） | [code_intel](code_intel.md#性能) | `code_intel` 第 6 步 |
| `code_intel/yield_stop` | 最大值 ≤ 10 ms（1 MiB 的 TypeScript 在解析中途打开让路开关，到这次抽取停在分片边界；[ADR 0033](../adr/0033.md)） | [code_intel](code_intel.md#性能) | `code_intel` 第 7 步 |
| `code_intel/goto_large`、`code_intel/goto_large/cold`、`code_intel/goto_large/object` | 中位数 ≤ 20 ms / 只记录 / 只记录（4 MiB 的 TypeScript 当前文本、位置在中部：身份已记下时只读窗口、解析窗口；身份没记下时流式读一遍求 OID；对象库一侧、位置不在保留的窗口里，整份取出 blob；[ADR 0034](../adr/0034.md)） | [code_intel](code_intel.md#性能) | `code_intel` 第 8 步 |
| `code_intel/window_chain_16mib`、`code_intel/window_peak` | 只记录 / 最大值 ≤ 8 MiB（16 MiB 的 TypeScript 被打开，从打开到窗口链写完；期间的活跃字节峰值：流式哈希的 1 MiB 区间或一个 64 KiB 窗口的树，`Memory`） | [code_intel](code_intel.md#性能) | `code_intel` 第 8 步 |
| `code_intel/deps_ts_project`、`code_intel/deps_identity`、`code_intel/deps_map_bytes` | 只记录 / 中位数 ≤ 50 ms / ≤ 1 MB（合成的 TS 项目：21 个直接依赖、约 5 MiB 声明闭包、4 MiB 的 `package-lock.json`：从空缓存到依赖层完成；路径表和单元都在缓存里时，重新打开项目的身份任务自身的耗时（每次从开始到结束，求和，不含项目本身的打开）；上限处的依赖映射（给出 2 万个文件，包内路径约 48 字节、SHA-256）的活跃字节，`Memory`；[ADR 0033](../adr/0033.md)） | [code_intel](code_intel.md#性能) | `code_intel` 第 9 步 |
| `syntax/first_screen`、`syntax/first_screen/minified` | 中位数 ≤ 8 ms / 只记录（磁盘上合成的 TypeScript、Rust、JSON 文件 1–64 MiB，各 40 个随机偏移：打开、一次 `read_ranges`、`window::plan` 加 8 ms 截止时间的窗口解析与查询；压缩成一行的 4 MiB JavaScript。第 3 步起含首屏读取） | [file_reader](file_reader.md#性能) | file_reader 第 2、3 步 |
| `syntax/first_screen/object_shared`、`syntax/first_screen/object_1mib`、`syntax/first_screen/object_cold` | 中位数 ≤ 8 ms / ≤ 8 ms / 只记录（对象库内容的首屏，位置在中部：已经 `share_object` 的 4 MiB blob；没有人持有、首屏路径上从对象库取出的 1 MiB blob；没有人持有的 4 MiB blob，[ADR 0042](../adr/0042.md) 的例外） | [file_reader](file_reader.md#性能) | file_reader 第 3 步 |
| `syntax/lru_bytes` | 最大值 ≤ 8 MiB（先后打开、关闭 24 个约 600 KiB 的档位 A 文件之后，drop `Engine` 释放的活跃字节：装满的高亮与文本 LRU 加上引擎的几张小表；`Memory`） | [file_reader](file_reader.md#性能) | file_reader 第 3 步 |
| `syntax/scan_64mib`、`syntax/find_16mib` | 只记录（64 MiB 的 TypeScript 从 `Engine::open` 到流式扫描给出 OID 和精确行数；16 MiB 的 `Text` 上从 `find` 到最后一批结果） | [file_reader](file_reader.md#性能) | file_reader 第 3 步 |
| `syntax/window_peak` | 最大值 ≤ 8 MiB（窗口链里一个 64 KiB 窗口的解析与高亮期间的活跃字节峰值，TypeScript、Rust、JSON、压缩 JavaScript 各几个窗口，tree-sitter 的分配计入；`Memory`） | [file_reader](file_reader.md#性能) | file_reader 第 2 步 |
| `syntax/full_parse_1mib`、`syntax/chain_16mib` | 只记录（1 MiB 的 TypeScript 按 8 ms 一片的整文件解析；16 MiB 的 TypeScript 的整条窗口链，每片 50 ms） | [file_reader](file_reader.md#性能) | file_reader 第 2 步 |
| `syntax/edit_tree_bytes` | 最大值 ≤ 41 MiB（编辑会话对档位 A 上限处的文件（1 MiB 减 4 KiB 的 TypeScript、Rust）完成第一次解析之后持有的活跃字节：语法树为主，加缓冲区的文本和区间，tree-sitter 的分配计入；结束会话后释放；`Memory`，[ADR 0034](../adr/0034.md)） | [file_reader](file_reader.md#性能) | file_reader 第 5 步 |
| `syntax/prefetch_wait` | p95 ≤ 500 ms，最大值只记录（真实的 `code_intel` 在 10 万个 8 KiB 文件的夹具上建索引、同一个 executor 上打开的 16 MiB 文件的窗口链在运行时，预取从请求到预取任务开始做它的那一轮；每个批量线程的一批约 350 ms 加一个 50 ms 的窗口任务，留 25% 余量） | [file_reader](file_reader.md#性能) | file_reader 第 4 步 |
| `plugin/code-intel/resident_cloudprint`、`plugin/code-intel/resident_100k` | 中位数 ≤ 4 MB / ≤ 10 MB（一个项目建完索引、做过一次跳转之后本 crate 的活跃字节，测量前丢掉那次跳转保留的解析（不在 [ADR 0035](../adr/0035.md) 的采样状态里，见 [ADR 0037](../adr/0037.md)），计数分配器，tree-sitter 的分配计入，不含文件列表；`Memory`，进程级基准实现之前在进程内测量） | [code_intel](code_intel.md#性能) | `code_intel` 第 4 步 |
| `review/log_load_16k` | 中位数 < 30 ms（16 000 条记录、约 5.3 MiB 的标注日志整份读入并建索引，后台；夹具是 `lectern_review::testing::write_log`） | [review](review.md#性能) | `review` 第 1 步 |
| `review/chain_3000_lines` | 只记录（一个 3000 行的文件、40 次 `Replace`，分在 8 轮里：改动链的计算，含每一步的 `line_diff` 和版本 OID，不含 `load`；夹具是 `lectern_review::testing::chain_fixture`） | [review](review.md#性能) | `review` 第 2 步 |
| `review/open_session_50`、`review/open_session_50_warm` | 中位数 < 35 ms / < 20 ms（`testing::Driven` 驱动审阅引擎，不经插件；夹具 `testing::SessionFixture`：2000 个提交、300 条 HEAD reflog 记录（10 次分支切换）、50 个会话，审阅对象的会话 20 轮、120 次改动调用，50 个 300 行的改动文件。从 `open` 到第一份 `Ready` 快照：新的缓存目录、`review.*` 全空 / 同一缓存目录上的新引擎、`review.*` 都已命中；缓存的数据库文件在计时前已打开） | [review](review.md#性能) | `review` 第 3 步 |
| `review/open_session_settled`、`review/cutoffs_cold`、`review/attribute_session_50` | 只记录（同一夹具：缓存全空时从 `open` 到最后一份快照；300 条记录的 `tree_changes` 全部未命中时建出截止时刻表；其余 49 个文件的改动链与归因，每一步都 `load`，不含缓存的读写） | [review](review.md#性能) | `review` 第 3 步 |
| `review_view/items_5000` | 中位数 < 4 ms（`items::build` 对 `testing::snapshot_fixture(50, 20, 100)` 的全部 5000 行；进程内，不开窗口，只在 macOS 编译） | [review_view](review_view.md#性能) | `review_view` 第 1 步 |
| `review/rows_first`、`review/mark_seen_5000` | 中位数 < 3 ms / < 0.2 ms（同一夹具、审阅已完成时第一个文件的 `Prepare` 到 `Prepared`；`testing::MarkSeenFixture` 的 5000 个文件的会话外改动上 `command(MarkSeen)` 到快照替换） | [review](review.md#性能) | `review` 第 3 步 |
| `review/refresh_one` | 中位数 < 20 ms（同一夹具、审阅已完成时，agent 追加一轮并改写第一个改动文件：从 worktree 的路径批次和来源的 `SessionsChanged` 到第一份同时显示新一轮和新内容的快照；`testing::Driven` 不经插件和 fs_watch，批次与事件直接交给引擎） | [review](review.md#性能) | `review` 第 4 步 |
| `plugin/review/activate` | 中位数 < 1 ms（kernel 记下的四段耗时之和，`TestKernel` 下进程内测量；激活没有提交任务也没有写文件；第 5 步的进程级插件基准实现后改为 `Process`） | [review](review.md#性能) | `review` 第 4 步 |
| `usage/ingest_append_4k`、`usage/backfill_batch` | 中位数 < 5 ms / < 150 ms（`lectern_usage::testing::Scenario` 不经插件驱动存储；夹具 `testing::heavy_days`：30 个重度日、20 个仓库、约 18 万条事实行。活跃会话追加约 4 KiB（一个轮次开始、两条用量记录）从读取到游标提交；一批 8 MiB 的合成会话（约 390 轮、1170 条用量记录）的读取与入库） | [usage](usage.md#性能) | `usage` 第 1 步 |
| `usage/backfill_large_session` | 每批最大值 < 300 ms、中位数 < 150 ms（同一夹具上一个 200 MB 的合成会话按 8 MiB 分批补读，每批一个样本；来源交出的字节合计超过日志的 1.2 倍时基准出错。定稿在 `usage` 第 4 步加入） | [usage](usage.md#性能) | `usage` 第 1 步 |
| `usage/project_30d`、`usage/all_projects_30d` | 中位数 < 50 ms / < 80 ms（同一夹具：一个项目 / 所有项目的 30 天快照从事实行整体计算，即快照任务的工作） | [usage](usage.md#性能) | `usage` 第 1 步 |
| `usage/snapshot_increment` | 中位数 < 2 ms（同一夹具上关注着第一个项目的 30 天快照，一批入库之后：事实行增量应用到快照的累加器、替换快照。这一批的入库本身由 `usage/ingest_append_4k` 计时；定稿在 `usage` 第 4 步加入） | [usage](usage.md#性能) | `usage` 第 1 步 |
| `usage/archive_month` | 每段中位数 < 150 ms（同一夹具在时钟推后 400 天之后归档，每写一段（读热行、编码、压缩、解码核对、插入、删热行、提交）一个样本；段数、行数、压缩比和归档前的数据库大小写在备注里） | [usage](usage.md#性能) | `usage` 第 1 步 |
| `usage/cold_read_month` | 中位数 < 1 s（归档之后一个项目一个月的 `History` 按日明细：逐段解压这个月的 `fact` 段并按本地日求和） | [usage](usage.md#性能) | `usage` 第 1 步 |
| `usage/disk_day_heavy` | 只记录（空数据库写入一个重度日之后数据库文件与 `-wal` 的增长；字节数，由 `time` 目标产出） | [usage](usage.md#性能) | `usage` 第 1 步 |
| `usage/resident_bytes` | 存活字节中位数 ≤ 2 MB（插件在 `TestKernel` 下打开同一夹具，会话来源列出夹具的全部 1500 个会话、每个从保存的游标读一次、kernel 静止之后；与只依赖同样服务的空插件之差；SQLite 的内存（含页缓存）不经计数分配器，不计入；`Memory`） | [usage](usage.md#性能) | `usage` 第 1 步 |
| `plugin/usage/activate` | 中位数 < 1 ms（kernel 记下的四段耗时之和，`TestKernel` 下进程内测量；第 5 步的进程级插件基准实现后改为 `Process`，与 `plugin/*/activate` 的展开一致） | [kernel](kernel.md#性能) | `usage` 第 1 步 |
| `platform/safe_stat` | 中位数 ≤ 同一路径 `symlink_metadata` 的 3 倍，且 < 10 µs | [platform](platform.md#性能) | 第 1 步 |
| `platform/safe_read_4k` | 中位数 ≤ `std::fs::read` 的 1.5 倍 | [platform](platform.md#性能) | 第 1 步 |
| `platform/safe_read_range` | 中位数 ≤ 同一路径上 `File::open`、`metadata`、两次 `read_at`、`metadata` 的 1.5 倍，且 < 200 µs（64 MiB 的文件，首屏的文件头 8 KiB 加中部目标附近 132 KiB） | [platform](platform.md#性能) | `platform` 范围读取（[#103](https://github.com/SoloJiang/lectern-app/issues/103)） |
| `platform/safe_read_page` | 中位数 ≤ `std::fs::File` 上 `read_at` 加 `metadata` 的 1.5 倍，且 < 50 µs（持有的 `OpenFile` 上依次读 64 KiB 的页） | [platform](platform.md#性能) | `platform` 范围读取（#103） |
| `executor/spawn_10k`、`executor/wake_1000x100` | 只记录 | [executor](executor.md#性能) | 第 1 步 |
| `kernel/service_get`、`kernel/service_for_each/{1,4,16}`、`kernel/service_current/{1,4,16}`、`kernel/emit`、`kernel/emit_nested` | 只记录 | [kernel](kernel.md#性能) | 第 1 步 |
| `i18n/frame_strings` | 只记录（en 资源里每条消息各取一次、带变量的用样例参数格式化） | [i18n](i18n.md#性能) | 第 1 步 |
| `log/disabled_call`、`log/filtered_call`、`log/enabled_call` | 中位数 ≤ 2 ns / ≤ 5 ns / ≤ 1 µs（调用线程上：低于全局级别的调用；全局级别放行、按 target 拒绝的调用；写出的 info 记录，4 个字段，每个样本是写盘线程清空缓冲后的 256 次调用） | [logging](logging.md#性能) | `logging` 第 1 步 |
| `log/contended_call` | 10 000 次里超过 5 µs 的不超过 100 次，即 p99 ≤ 5 µs（`min(4, 可用核数 − 1)` 个线程同时写，至少 1 个，个数和调用线程慢路径的计数（遇到缓冲锁被占着、自旋后仍阻塞、唤醒写盘线程的次数）写在报告的备注里；线程只建一次、先各写一条不计时的记录，每轮在写盘线程清空缓冲后同时开始；每次调用单独计时，每轮约 400 条小于缓冲，不丢记录） | [logging](logging.md#性能) | `logging` 第 1 步 |
| `log/third_party_limited`、`log/install_early` | 中位数 ≤ 30 ns / ≤ 20 µs（被限流的第三方记录；`install_early` 的全部工作，经 `testing::install_early_again`） | [logging](logging.md#性能) | `logging` 第 1 步 |
| `log/idle_memory`、`log/idle_memory/verbose` | ≤ 512 KiB / 只记录（写出 1 万条记录、静止 5 s 后日志器的活跃字节，默认配置 / 详细日志开启；`Memory`） | [logging](logging.md#性能) | `logging` 第 1 步 |

基准 ID 一律写成 `<crate 或类别>/<名字>[/<场景>]`。git 与 platform 设计文档里原来的名字（`git_open`、`safe_stat` 等）已在本 PR 中改成这种写法。

新增有预算的功能时，实现它的 PR 同时加上基准，并把对应条目改为 `Active`；没有对应条目的，在预算表里新增，出处指向那个 crate 设计文档的「性能」一节。

## 平台与噪声

预算定义在 M 系列 Mac 上。

| 平台 | `just bench` 做什么 | 是否判定 |
|---|---|---|
| macOS | 构建测量用的 `lectern` 和 `empty_window`，`cargo bench`：全部基准按下面的规则采样 | 只在 Apple 芯片（`aarch64-apple-darwin`）上判定，超标即失败；Intel Mac 只报告 |
| Linux | `cargo test --benches`：冒烟，每个进程内基准用小规模输入跑一次，进程级基准记为 `skipped` | 不判定，只在出错时失败 |
| Linux，`just bench --measure` | `cargo bench`，进程内基准照常采样 | 不判定，只报告（供 Linux 上的实际验证记录数字） |

在共享的、虚拟化的 x86 Linux runner 上放宽阈值没有意义：硬件和预算定义的机器不同，判定结果只会带来误报。Linux 任务的作用是在几十秒内发现基准编不过、夹具坏了、预算漏登记。

采样规则（Measure 模式）：

- 预热：至少调用 3 次且至少 100 ms，或者至少调用 1 次且至少 300 ms（慢操作一次调用之后就是热的，[ADR 0036](../adr/0036.md)）。`time` 和 `compare` 在预热时确定批量次数 k，使一个样本不短于 1 ms（`facts_contains/name` 这类 1 µs 以下的操作）；样本值是批量耗时除以 k。`time_with_setup` 不批量，k 固定为 1（`agents/append_4k`、`git/refresh_one` 单次在 10 µs 以上，`Instant` 在 M 系列上的分辨率约 42 ns，足够单次计时）。
- 采样（进程内基准）按以下顺序判断是否停止，前一条优先：
  1. 少于 5 个样本时不停止。
  2. 达到 10 000 个样本，或从预热开始的墙钟时间（含预热和 `setup`）达到 3 秒时停止。
  3. 已有至少 30 个样本，且样本耗时之和（不含 `setup`）至少 1 秒时停止。

  批量后约 1 ms 的样本在约 1 秒、1 000 个样本时停在第 3 条；10 µs 的单次操作（`time_with_setup`）停在第 2 条的 10 000 个样本，约 0.1 秒；0.2 秒的操作（`agents/parse_throughput`）预热 2 次，停在第 2 条，约 13 个样本；0.5 秒的操作（`agents/discover_1000`）预热 1 次，约 5 个样本；1 秒以上的操作取最少的 5 个样本。`Ctx::config` 可以提高最少样本数和时长，不能把第 1 条的 5 个降低。

  最少 5 个样本的理由（ADR 0036）：真实中位数落在 5 个样本的最小值与最大值之间的概率约 94%，与分布无关；超标要两次独立采样的中位数都超标才算。这一版之前第 2 条从采样开始计 5 秒、预热至少 3 次，1 秒的操作要 8 秒；判定用的统计量、上限和重跑规则都没有变。
- 只记录、只用来定位开销的诊断项用 `Ctx::once` 取 1 个样本，不预热，由基准自己先调用一次（如 `git/filter_100k/walk`、`git/changes_clean_100k/stat_all`）。`Ctx::once(f) -> Samples` 在两种模式下都只计时一次调用。
- 进程级基准不用这些规则，次数由各自的流程固定（冷启动 2 次预热加 20 次、空闲内存 3 次、端到端 5 次、插件一对、超标时再两对，见「进程级基准」）。
- 冒烟模式不用这些规则：不预热、k = 1，每个进程内基准只调用一次。
- 统计量默认取中位数；冷启动按 ADR 0017 用计数和最大值；帧时间用每帧工作时间的最大值和以刷新周期为单位的最大间隔（见「进程级基准」）。
- 吞吐是 `bytes / 中位数耗时`；比值是被测与对照交替采样后两个中位数之比。
- 不设容差：预算就是表里的数字。抗噪声靠预热、中位数和一次重试：超标的基准重跑一次，两次都超标才算超标；重跑用新的夹具，`Ctx::shared` 的共享夹具除外（见下文「夹具与 `test-support`」）。
- 离散度：中位数绝对偏差超过中位数 10% 的结果在报告里标为 `noisy`，不影响判定，用来发现不稳定的基准。
- 确定性：夹具由固定种子生成，不取时钟和随机数；基准按 ID 顺序运行；运行器在 macOS 上把基准线程设为 user-interactive QoS。
- 页缓存：进程内基准测的是热缓存（预热之后）；冷文件缓存只在 `app/cold_start_purged` 里测量。

判定用的机器是 CI 的 `macos-15` runner（Apple M1 虚拟机）。它与实体 M 系列 Mac 的差距由第 4 步的基线测出，见「未决问题」。

### 时间盒截断的轮次

预算含完整性、查询又有时间盒的基准（现在只有 `code_intel/references_full`），每一轮按 `lectern_bench::rounds` 判定（[ADR 0043](../adr/0043.md)）：

- `Complete` 的一轮：样本是实测的时间（`/first` 到第一批带引用的事件，`/all` 到最后一批）。
- 被时间盒截断的一轮（`Verdict::Cut`）：两个 ID 的样本都记为这一轮的耗时加一个时间盒（`Round::charge`，≥ 3.8 s），超过两条预算；预热轮同样不报错，只计入记录项。中位数照常判定，11 个样本里截断的超过 5 轮时两项都超标，超标重跑一次的规则不变。
- 截断的依据只来自查询的公开结果和 `diagnose`：`unsearched` 为 0、`outside` 大于 0、从查询到最后一批不短于时间盒（`query::time_box`）、存储里当前版本的单元数不少于夹具写入的（模板的单元加上定义所在文件和 300 个调用方）、淘汰次数为 0。任何一条不成立或不知道，这一轮仍是错误，消息带上 `diagnose` 的输出。`unsearched` 大于 0、没有 `Done`、没有第一批、引用少于调用方数也仍是错误。
- 截断的轮数和一轮留在范围外的最多文件数是只记录的 `…/partial_rounds`、`…/outside`；有截断时这两项和两条判定项都带 `note`（截断的轮数、最后一轮的 `outside` 和诊断），CI 的 notice 和 error 注释里看得到。
- 只在命名空间满额时适用（`references_full`，或手动设置 `CODE_INTEL_BENCH_FULL` 的 `references_100k`）：没满时不核对存储，时间盒不会产生 `outside`。`goto_full` 的 `/done` 判定最大值，按「每一轮」判定，不用这条规则；其余要求 `Complete` 的跳转基准离时间盒很远，不完整说明夹具或查询坏了（ADR 0043「决策」）。

## `just bench` 与 CI

`justfile` 的 `bench` 改为：

```just
# Performance budget benchmarks: gated on Apple silicon macOS, a smoke run elsewhere.
bench *args:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -f target/lectern-bench/report-*.json target/lectern-bench/report.json
    if [[ "$(uname -s)" == Darwin ]]; then
        if [[ "${1-}" == --measure ]]; then shift; fi
        cargo build --release --locked -p lectern-bench --all-features --example empty_window   # 第 4 步加入
        cargo build --release --locked -p lectern-app --features bench-probe                     # 第 5 步加入
        export LECTERN_BENCH_BASELINE="$PWD/target/release/examples/empty_window"
        export LECTERN_BENCH_APP="$PWD/target/release/lectern"
        cargo bench --locked --no-fail-fast -p lectern-bench --all-features -- "$@" || true
    elif [[ "${1-}" == --measure ]]; then
        shift
        cargo bench --locked --no-fail-fast -p lectern-bench --all-features -- "$@" || true
    else
        cargo test --locked --no-fail-fast -p lectern-bench --all-features --benches -- "$@" || true
    fi
    # The merged report decides the result: a target that failed to build or crashed leaves no report.
    cargo run -q --locked -p lectern-bench --bin lectern-bench-report -- merge target/lectern-bench
```

- `--no-fail-fast` 让一个目标超标后其余目标照常运行。cargo 对失败的目标统一返回 101，所以退出码不取 cargo 的，而取 `lectern-bench-report` 的。

- 其余参数是 ID 过滤，如 `just bench agents/`。
- `cargo bench` 使用 `bench` profile（继承 `release`：thin LTO、`codegen-units = 1`），与预算要求的发布构建一致。测量用的 `lectern` 与发布构建的差别只有 `bench-probe`；安装包体积测量用不带它的构建。
- 冒烟用 `test` profile，和 `just test` 共用依赖的编译产物。nextest 不运行 `harness = false` 的目标，所以冒烟另由 `just bench` 运行。
- `just ci` 在两个平台上都调用 `just bench`（Linux 上是冒烟）。

CI：

- Linux 任务在 Doctests 之后增加一步 `just bench`。目标耗时 2 分钟以内（大部分编译产物与测试步骤共用）。
- macOS 上 `just bench` 在单独的 `Benchmarks (macOS arm64)` 任务里运行，与 `Build and test (macOS arm64)` 并行（[ADR 0036](../adr/0036.md)，[CI 设计「任务」](ci.md#任务)），之后用 `actions/upload-artifact` 上传 `target/lectern-bench/report.json`（`if: always()`，超标时也能看到数据）。报告表同时写进任务摘要，最慢的 10 个基准是一条 notice。目标耗时 15 分钟以内，其中运行约 8 分钟；超过时先把端到端基准和 `app/idle_empty` 的启动次数降到 3 次和 1 次，不减少 ADR 规定的次数和时长（冷启动 20 次、静止后等 5 秒）。

## 夹具与 `test-support`

- 被测 crate 用 `test-support` feature 导出 `pub mod testing`（`#[cfg(any(test, feature = "test-support"))]`）：夹具生成器，以及公开接口之外 bench 需要的入口（如 `agents::testing::parse_file`）。`git` 的 `TestRepo`、`project` 的合成仓库形状、`kernel` 的 `TestKernel`、`executor` 的手动执行器都按这个约定放置。
- bench 只调用公开接口和 `testing`，不复制被测 crate 的内部逻辑，也不按相对路径读别的 crate 的目录；需要的样本由 `testing` 用 `include_bytes!` 嵌入或在运行时生成。
- 生成器是确定的（固定种子，不取时钟），接受规模参数，冒烟用小规模。生成的文件放在 `Ctx::scratch()` 下，每个基准、每次重试都重新生成，会修改夹具的基准（`append_4k`、`refresh_one`）不影响别的基准。
- 例外是共享夹具（[ADR 0036](../adr/0036.md)）：生成要几秒以上、只读的大型夹具用 `Ctx::shared(key, build)` 在一个基准目标进程里只生成一次，之后的基准和重跑拿到同一份（`Rc`），目标运行结束时释放。键写明生成器和规模，如 `git_worktree::large(100000, 2000)`；同一个键换了类型时返回错误。共享夹具不放在 `scratch()` 下（每个基准结束时删除）。用它的基准只读：不写工作区和 git 目录，句柄和缓存各自新建。现在共享的有 10 万文件的 git 工作区（`git_worktree::large_shared`：`time` 目标的 `git/open_100k`、`git/filter_100k/cold`、`git/filter_100k/walk`、`git/changes_clean_100k`，`memory` 目标的 `git/compare_100k`、`git/filter_100k_bytes`）和 `code_intel` 的 10 万文件虚拟仓库（见下一条）；`git/filter_100k` 要改 index，用 `git_worktree::shaped_with_entries` 自己生成一份。git 工作区不跨运行保留：index 记着每个文件的 ctime 和 inode，复制或还原都会改变它们。
- 跨运行保留的夹具（`Ctx::persistent()`，即 `<target 目录>/lectern-bench/fixtures/`，[ADR 0039](../adr/0039.md)）：写一次要很久、之后只被复制的夹具放在这里，一次 `just bench` 的几个目标共用，之后的运行也接着用。每个夹具在一个按键命名的子目录里；用之前对照当前代码核对内容，核对不过就删掉整个目录重写；同一时刻只有一次基准运行。现在只有 `code_intel` 的 10 万文件虚拟仓库（`code_intel_virtual::shared` → `code_intel::testing::VirtualTemplate::cached`，在 `fixtures/code_intel/<键>/`）：
  - 内容：写好单元的缓存目录（`cache/`）和最后写入的 `manifest`（键、文件数、单元数）。键是声明的输入的哈希，前面加文件数（同一文件数的旧模板被替换，冒烟模式的 3000 文件模板与 10 万文件的并存）：模板自己的版本号、文件数和公共名字（`fetchShared` 每 17 个文件一个、`id` 每 2 个文件一个）、缓存的预算、`code_intel.tags` 的命名空间版本和表结构、每种核心语言当前的（语言、grammar 版本、查询版本）、合成生成器对 128 个文件的输出（路径、OID、抽取结果），以及所在目录的绝对路径（`FakeGit` 的公共目录在它下面，分库按这个路径命名，挪到别处就打不开）。
  - 核对（在复制出来的一份上做）：`manifest` 的键相同；当前版本的单元数等于当前语言映射下的源文件数，也等于 `manifest` 记下的数；对抽样的文件（均匀分布的 96 个，加上每个公共名字各 32 个定义它的文件），用当前的写入逻辑（`testing::virtual_unit`，`virtual_repo_sharing` 也只经它决定写什么）重新生成单元，与读回的定义和引用逐个比较。任何一项不符就重写。能保证的是：写入逻辑或语言映射的改动，只要影响到抽样文件或单元数，缓存格式变了（`lectern-cache` 打开时会丢掉旧格式的分库）、写到一半中断、单元被删，都不会让旧模板被用上，不需要手动加版本号（`template_*` 测试，其中 `template_changed_writer_is_written_again` 是键不变、写入逻辑不同的情况）。不能保证的是：只改变样本之外的文件、又不改变单元数的改动，这时要把 `template.rs` 的 `TEMPLATE_VERSION` 加一。
  - 写入每 1024 个单元一个事务（夹具，不是批量层的 64 个；`code_intel/disk_100k` 仍按 64 个写），10 万文件约为原来的三分之一时间。
  - 使用：`time` 目标的 `code_intel/goto_indexed`、`goto_uncached`、`goto_popular`、`goto_full`、`goto_cold_parse`、`goto_same_file`、`goto_object`、`named_50k`、`references_100k`、`references_full`、`names_100k`，`memory` 目标的 `code_intel/names_100k_bytes`、`plugin/code-intel/resident_100k`。虚拟仓库的基准会写缓存（当前文本的单元、访问时间、淘汰），所以给出的只是写好的文件：每个基准（`resident_100k` 是每个样本）复制一份缓存目录（经缓冲区逐字节写出，不用 `std::fs::copy`：它在 APFS 上可能是 `clonefile`，克隆与模板共用数据块、却不共用页缓存，查询的随机读要到磁盘上去，限时分页的 `goto_popular`、`named_50k` 在另一次运行或另一个目标写的模板上会提前停下；写出的副本像刚建好索引的分库一样在页缓存里），在同一个公共目录路径上新建自己的 `FakeGit`，再建自己的 rig；写好之后没有任何东西再打开保留的那份。要冷状态的基准照旧在自己的 rig 上每次清掉（`goto_cold_parse` 的保留解析、`goto_uncached` 的遍历缓存），`names_*` 每个样本重建名字表。
  - CI 的 `Benchmarks (macOS arm64)` 用 `actions/cache` 在运行之间还原这个目录，键是生成它的源码（`code_intel`、`cache` 的源码、bench 的夹具代码和 `Cargo.lock`）的哈希；这个键只决定命中率：还原的模板仍按上面的规则核对，源码哈希是另一层防护，覆盖了样本之外的改动（[CI 设计「任务」](ci.md#任务)）。
- 只有 bench 的 dev-dependencies 打开被测 crate 的 `test-support`，没有哪个 crate 的普通依赖打开它，`app` 的 `bench-probe` 也不打开。发布构建里既没有 `lectern-bench` 也没有 `test-support`（见「验收」里的 `cargo tree` 检查）。
- 大型样例项目不提交进仓库：`cloudprint` 形状（728 个文件、11 个 worktree、约 25 个清单）和 10 万文件的仓库都由 `project` 的形状描述加 `git` 的 `TestRepo` 生成。`crates/bench/tests/fixtures/` 只放本 crate 自己的小样本。

## 迁移

第 1 步把被忽略的 `perf_smoke` 测试移进 bench，同一个 PR 里删除原测试：

- `crates/agents/tests/perf_smoke.rs` → `benches/time/agents.rs` 的 `agents/parse_throughput`、`agents/append_4k`，以及 `agents` 第 3 步加进同一个文件的 `agents/zst_throughput`、`agents/zst_load_decoded`。拼接 50 MB 会话和取约 4 KB 完整行的代码移进 `lectern_agents::testing`（`synthetic_session(len) -> Vec<u8>`、`append_chunk() -> &'static [u8]`，fixtures 用 `include_bytes!` 嵌入）；Codex 归档用 `synthetic_rollout(len) -> Vec<u8>` 和 `compress_zst(&[u8]) -> Vec<u8>`（`ruzstd` 最快档）。删除 `crates/agents/Cargo.toml` 里的 `[[test]] perf_smoke`，更新 `crates/agents/AGENTS.md` 里对它的说明。
- `crates/git` 第 2 步加入的 `perf_smoke_git_diff_50` → `benches/time/git.rs` 的 `git/diff_50_cold`、`git/diff_50_warm`，测量内容不变（整个 `diff_trees`，728 个文件里 50 个约 300 行的文件各改 10 行）。冷的一项用 `Ctx::time_with_setup`：每个样本之前生成一对内容从未比较过的树（hunk 缓存是进程内唯一的，以 blob OID 为键，重试也换新内容）。
- `crates/git` 的 `perf_smoke_git_open` 测了两项，两项都移进 `benches/time/git.rs` 之后才删除原测试：
  - `git/open`：`discover` + `worktrees` + `tracked`，用 `lectern_git::testing::TestRepo` 生成同样形状的仓库（728 个跟踪文件、11 个 worktree）。
  - `git/open_handle`：在同一仓库的公共目录上打开再 drop 一次 gix 句柄。`Handle` 是 crate 内部类型，`lectern_git::testing` 增加 `open_handle(common_dir: &Path) -> Result<(), GitError>`，按原测试的方式（`ConfigScope::RepositoryOnly`）打开并 drop。
  - 更新 git 设计「与设计的差异」里的对应一条。
- `crates/cache` 第 1 步加入的 `perf_smoke_cache`（`src/tests/perf.rs`）→ `benches/time/cache.rs` 的 `cache/get_4k`、`cache/get_many_50`、`cache/get_1m`、`cache/open_existing`、`cache/open_new`、`cache/put_4k`、`cache/batch_1000` 和只记录的 `cache/zstd_1m/put`、`cache/zstd_1m/get`。条目类型和数据由 `lectern_cache::testing` 提供（`Raw`、`Zstd`、`noise(seed, len)`、`text(len)`）。打开分库的两项用 `time_with_setup`，每个样本新建一个 `Cache`（`open_new` 还换一个新分库）；写入的两项限制样本数，写入的总量远低于分库的 512 MiB 预算，样本不含淘汰（淘汰由第 2 步的 `cache/evict_16m` 测量）。
- `crates/platform/src/safe_read/tests.rs` 的 `perf_smoke` 和 `perf_tree` → `benches/time/platform.rs` 的 `platform/safe_stat`、`platform/safe_read_4k`。bench 自己生成同样的 728 个 4 KiB 文件、深度 4 的目录树。原测试比较每个后端，`open_with` 是 crate 内部接口；bench 只测 `SafeRoot::open` 选中的生产后端（macOS 上是 `NofollowAny`，Linux 上是 `Openat2`），预算也只对生产后端成立。`Walk` 只用于少见的路径，不设基准。更新 platform 设计「性能」里的「基准」一条。
- `executor` 设计里推迟的两项、`kernel` 与 `i18n` 设计里的基准，同一步加入（只记录）。

## 性能

- 本 crate 不进入发布构建，不影响冷启动、内存和安装包。
- 测量用的构建里，探针在首帧之前只读两个环境变量，并通过 `platform` 注册一个首帧回调（一次方法替换，≤ 100 µs，[platform 设计「帧的时刻」](platform.md#帧的时刻)），不读文件、不建线程、不连 socket；socket 和应答线程都在首帧显示之后才建立。逐帧记录的钩子在首帧之后、只在帧时间的场景里安装。`bench-probe` 插件在所有内存基准里都存在（插件的两次启动里也都存在），差值不受它影响。

## 状态与异常

- 夹具生成失败、基准返回错误或 panic：记为 `error`，退出码 2，在任何模式下都如此。
- 子进程在 `FirstFrame` 之前退出，或任何一个等待超过「进程级基准」表里的期限：结束子进程的整个进程组，记为 `error`，原因写明超时的消息。
- 所选插件进入 `Failed`：立即结束子进程，记为 `error`。
- 首帧后 30 秒内没有静止：对应的内存基准记为 `over`，原因写「没有静止」。
- 采样期间静止代数变化：丢弃样本重新等待，连续 3 次无效记为 `error`。
- 判定平台上缺少 `LECTERN_BENCH_APP` 或 `LECTERN_BENCH_BASELINE`：对应的生效条目记为 `error`；冒烟模式下记为 `skipped`。
- `sudo -n purge` 不可用：`app/cold_start_purged` 记为 `skipped`。
- 基准与预算表对不上：开始运行前以 2 退出，列出多出和缺少的 ID。

## 测试

`just test -p lectern-bench`（Linux 和 macOS 都运行，不需要 GPUI）：

- 统计：`stats_*` 用固定的样本验证中位数、p95、最大值、中位数绝对偏差、批量换算；冷启动规则的边界：20 个样本里 1 个 25 ms 通过、2 个超过 20 ms 失败、1 个 31 ms 失败、恰好 30 ms 通过。
- 预算表：`budgets_*` 检查 ID 不重复；带 `*` 的条目与具体条目的优先级；每条 `Check` 的统计量与上限能用于它的 `Value` 类型；`Tier::Top` 的出处都在 `docs/design/tech-research.md` 或 `docs/adr/` 下；每个出处文件存在、锚点对应文件里的一个标题。
- 运行器：`runner_*` 用假基准验证：超标时重跑一次，第二次通过记为 `ok` 且两次都写进报告；两次都超标退出码 1；冒烟模式和非判定平台从不因预算失败；基准出错退出码 2；缺少或多出预算时开始前退出；`--bench` 决定模式；ID 过滤；`--list`。
- 报告：`report_*` 用 insta 快照固定一组假结果的文本表、JSON 和 Markdown 摘要；每个目标只写自己的 `report-<target>.json`。
- 合并：`report_merge_*` 验证三份报告（有 `report-package.json` 时四份）合并后每条预算恰好一次；没有 `report-package.json` 时 `Package` 的条目记为 `skipped`，带 `--require package` 时退出码 2；`lsp/*/resident` 按语言服务器列表展开；带 `*` 的条目按插件列表展开，列表为空时条目本身出现一次，不为空时某个插件缺少展开项即出错；缺一份报告、某条预算缺失、重复或出现在错误的目标里时退出码 2；合并后的退出码取超标与出错的最严重者；被过滤的条目记为 `filtered` 不算缺失；`::error` 注释最多 10 条；`record` 结果按组输出 `::notice`，`smoke`、`planned`、`ok` 不输出，组数超过 10 时第 10 条合并剩下的组、不丢结果。
- 计数分配器：`alloc_*` 分配并保留 1 MiB 后 `live` 增加 1 MiB，释放后回到原值，`peak` 记录峰值（在单独的测试二进制里注册分配器）。
- 报告协议：`wire_*` 对每种消息编码后解码相等，含带空格、`%`、换行和非 UTF-8 字节的路径；缺字段、未知消息名、坏的百分号编码返回错误。
- 进程调度：`process_*` 用脚本化的 `Launcher` / `ProcessInfo`：冷启动 2 次预热不计入、20 次样本；`Busy` 让 5 秒等待重新开始；采样前后代数不同时重采；30 秒不静止记为 `over`；不连 socket、提前退出、不响应 `Quit` 记为出错；`process_deadline_*`：假探针对 `Open` 不回 `Milestone`、对 `Scroll` 不回 `Frames`、不发 `Ready`、不应答 `Epoch` 时，运行器在各自的期限（测试里缩短）后结束进程组并记为 `error`，原因写明等的消息；插件基准里假探针发 `PluginFailed` 时立即记为 `error`、不等期限；插件两次启动的差值与激活耗时的取法（进程插件的进程组计入差值）；激活条件为 `[Workspace(..), AfterFirstFrame]` 的插件计入 `plugin/after_first_frame`；预热启动以 3 退出时记为出错；探针报告 `hook <名字>`、`frames <…>` 时记为出错；`time_with_setup` 每个样本调用一次 `setup`；进程组展开到孙进程、中途退出的后代被跳过、同一 pid 不重复计入、`plugin/process_total` 取自 `ActivateAll` 那次启动、是全部进程插件组之和（`Command` / `Manual` / `Workspace` 激活的进程插件都计入），`app/idle_empty` 不产出它；语言服务器组记为 `lsp/<id>/resident` 并计入 `lsp/total`，没有语言服务器时 `lsp/*/resident` 为 `skipped`、`lsp/total` 为 0；帧判定：60 Hz 下间隔 16.7 ms、工作时间 5 ms 时通过；一帧工作时间 9 ms 时超标；`requested` 之后主线程阻塞 20 ms、`started` 间隔达到两个周期时超标；120 Hz 下与「最大间隔 ≤ 8.3 ms」的结论一致；内存类并发不超过 3、计时类不并发。
- `purge`：集成测试把一个假的 `sudo` 放在 PATH 最前面，记录参数，按夹具回放成功与失败（AGENTS.md「工作方式」）。
- 探针：`probe_*` 在 Linux 上用真实的 `Kernel` 和测试插件验证 `start_kernel` 的两种裁剪（用计数的测试插件确认每个插件的 `manifest()` 在整个启动里只被调用一次）、`attach` 满足 `Command` / `Manual` / `Workspace` 三种激活条件后发出 `Ready`、`bench-probe` 插件丢弃过时的 `Busy` 且不让 kernel 重新变忙。首帧回调只能在 macOS 上验证，由第 4 步的基线运行覆盖。

只能在 macOS 上运行的：全部判定、进程级基准、`empty_window`。

## 未决问题

没有需要产品决定的问题。下面一条已于 2026-10-18 决定，产品负责人交给负责推进的 agent，按默认值：

1. **判定用哪台机器。** 选项 A：用 GitHub 托管的 `macos-15` 判定，不额外花钱。预算定义在 M 系列 Mac 上，`macos-15` runner 是 3 核的 Apple M1 虚拟机，与用户的实体机不同，也有邻居干扰。
   - 不采用选项 B：增加一台自托管的实体 M 系列 Mac 作为判定机器，托管 runner 只做冒烟。要付硬件和维护成本，并且要隔离在它上面运行的 PR 代码。
   - 第 4 步测出空 GPUI 窗口在托管 runner 上的基线后，如果冷启动在实体机上低于 20 ms、在托管 runner 上不低于 20 ms，需要产品负责人在 A、B 之间重新选择；按 ADR 0017，不能为此放宽预算。
   - 2026-10-26：托管 runner 不给 drawable 的 `presentedTime`（[ADR 0052](../adr/0052.md)「背景」），首帧回调一直不到。产品负责人决定首帧的时刻在这时取显示回调运行的时刻，每个结果注明来源，并再次确认选项 A；预算和判定规则不变。

## 验收

分五步，每步一个 PR。前四步写 `Refs #7`，第 5 步写 `Closes #7`。之后各 crate 的基准由实现它们的 PR 加入（见「预算表」），[#38](https://github.com/SoloJiang/lectern-app/issues/38) 另行关闭：它的 PR 加入打包任务，在任务里运行 `lectern-bench-report package` 和 `merge --require package`，把 `app/bundle_size`、`app/dmg_size` 改为 `Active`；普通的 `just bench` 里它们仍是 `skipped`。

1. **运行器、预算表与迁移**（`bench`，同时删除 `agents`、`platform` 里的 `perf_smoke`）
   - `just test -p lectern-bench` 里 `stats_*`、`budgets_*`、`runner_*`、`report_*`、`report_merge_*`、`alloc_*` 通过。
   - `just bench` 之后 `target/lectern-bench/` 下有 `report-time.json`、`report-memory.json`、`report-process.json` 和合并后的 `report.json`，`app/bundle_size`、`app/dmg_size` 的状态是 `skipped`（未打包），退出码不因它们非 0；`lectern-bench-report merge --require package` 在没有 `report-package.json` 时退出码 2；删掉 `budgets.rs` 里任意一条 `Active` 预算对应的基准时，`just bench` 退出码 2。
   - Linux 上 `just bench` 退出码 0：`time` 与 `memory` 目标的生效条目状态都是 `smoke`（`macos_only` 的条目是 `skipped`），`process` 目标的生效条目都是 `skipped`（只在 macOS 运行），其余是 `planned`；`just bench agents/` 只运行 `agents/*`，其余条目为 `filtered`。
   - macOS CI 的 `just bench` 判定 `agents/parse_throughput`、`agents/append_4k`、`agents/zst_throughput`、`agents/zst_load_decoded`、`git/open`、`git/open_handle`、`git/diff_50_cold`、`git/diff_50_warm`、`cache/*` 的 7 项读写与打开、`platform/safe_stat`、`platform/safe_read_4k`，记录 `cache/zstd_1m/*`、`executor/*`、`kernel/*`、`i18n/*`，上传 `report.json`。
   - Linux CI 任务有 `just bench` 一步；`just ci` 在 Linux 上运行它。
   - `rg -l perf_smoke crates` 没有结果（含 `crates/git/src/worktree/tests.rs`）；`rg -n 'perf_smoke' crates/agents/Cargo.toml` 没有结果。
   - `rg -n '^name = "(criterion|divan)' Cargo.lock` 没有结果；`just deny` 通过。
   - `python3 scripts/check_architecture.py` 通过；`python3 scripts/check_architecture.py --gpui-free-packages` 的输出包含 `lectern-bench`。
2. **进程、主机信息与线程 QoS**（`platform`，接口已在 [platform 设计](platform.md)里补充，验收是它「验收」的 A 部分）
   - macOS 上 `just test -p lectern-platform` 的 `process_*` 通过：子进程的 `start_time` 在启动它的前后两次读数之间，本进程的 `start_time` 早于测试开始；分配并写满 64 MiB 后 `phys_footprint` 至少增加 60 MiB；`children` 列出刚启动的子进程。`host_*` 给出非空的机型、芯片与系统版本，`thread_*` 读回 user-interactive 的 QoS。
   - Linux 上这些函数返回 `ErrorKind::Unsupported`，有测试覆盖。
   - 首帧与逐帧的时刻不在这一步：`frame::on_first_presented` 和 `bench-hooks` feature（platform 设计「验收」的 B1）在第 4 步之前合并，`frame::record`（B2）在第一个生效的帧时间预算之前合并。没有 `metal`（[ADR 0049](../adr/0049.md)）。
3. **插件集合的闭包与裁剪**（`kernel`，同一 PR 里更新 kernel 设计）
   - `just test -p lectern-kernel` 的 `set_closure_*` 通过：多层依赖、`MULTIPLE` 服务的多个提供者、缺提供者时返回 `GraphError`；`retain` 之后 `validate` 通过。
   - `set_prepare_*` 通过：`prepare`、`dependency_closure`、`retain`、`with`、`start_prepared` 走完之后，每个插件的 `manifest()` 恰好被调用一次；`status()` 的 `manifest_time` 等于 `prepare` 记下的耗时；`manifest()` panic 的插件进入 `Failed`，与 `Kernel::start` 相同。
4. **探针、进程调度与空窗口基线**（`bench`；前提是 platform 设计「验收」的 B1 已合并。`probe` 加上 `lectern-platform/bench-hooks`）
   - `just test -p lectern-bench` 的 `wire_*`、`process_*`、`probe_*` 和 `purge` 集成测试通过。
   - macOS CI 上 `just bench baseline/` 报告 `baseline/empty_window_cold_start` 的 20 个样本和 `baseline/empty_window_idle`。
   - 实测值和机器信息写进 ADR 0017（ADR 0017「影响」要求的基线）；不满足 `app/cold_start` 的判定规则（20 次里超过 20 ms 的多于 1 次，或有一次超过 30 ms）时告诉产品负责人，并按「未决问题」处理。
5. **`app` 的测量构建与应用级基准**（`app`，`Closes #7`；先合并 `app` 的设计文档。发布构建不带 binary archive，见 [ADR 0049](../adr/0049.md)）
   - `cargo build --release --locked -p lectern-app --features bench-probe` 成功；`cargo tree --locked -p lectern-app -e normal,build,features --target aarch64-apple-darwin` 的输出里没有 `lectern-bench`，也没有 `"test-support"` 和 `"bench-hooks"`（发布构建不含方法替换，ADR 0048）。
   - macOS CI 的 `just bench` 判定 `app/cold_start`、`app/idle_empty`、`plugin/*/activate`、`plugin/after_first_frame`，记录 `app/cold_start_purged` 和 `plugin/*/resident`。
   - 本文状态行改为「已实现」并列出五个 PR，填写「实际验证」。

## 实际验证

第 1 步，在 Linux（x86_64，4 核 Xeon 2.1 GHz，内核 6.18）上运行。判定、进程级基准和 macOS 上的 `just bench` 由 macOS CI 给出，本机没有运行。

- `just check`、`just test`（整个 Linux 工作区 679 个测试通过，跳过 1 个 `#[ignore]`；含合并进来的 `cache` 第 1 步）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）、`python3 scripts/check_architecture.py` 全部通过；`python3 scripts/check_architecture.py --gpui-free-packages` 的输出包含 `lectern-bench`。`cargo clippy --target aarch64-apple-darwin -p lectern-bench -p lectern-platform -p lectern-agents -p lectern-git -p lectern-i18n --all-targets --all-features -- -D warnings` 在 Linux 上交叉检查通过（没有运行）。
- `just test -p lectern-bench`：60 个测试通过。按前缀：`stats_*` 19（含冷启动规则的四个边界和帧判定）、`budgets_*` 9（含每个出处文件存在、锚点对应文件里的一个标题）、`runner_*` 14、`report_*` 5（3 个 insta 快照：文本表、JSON、Markdown，逐个看过）、`report_merge_*` 9、`alloc_*` 4（`tests/alloc.rs`，注册了 `CountingAlloc` 的单独测试二进制）。运行器的测试不检测本机平台：判定平台、是否 macOS、采样配置和 `host` 都由测试传给运行器，快照里的 `host` 是固定值。
- `alloc_*` 的计数是整个进程的：libtest 在新线程里运行测试，主线程在启动测试线程之后还会分配（登记正在运行的测试、等待通道）。在 Linux 上把测试二进制和 4 个忙循环一起绑在一个 CPU 上运行，测试开始后 20 ms 内看到别的线程分配了 144 或 900 字节（300 次里 16 次）；第一版 `alloc_*` 要求读数完全相等，同样条件下 150 次里失败 2 次。现在每项检查最多重试 5 次，以第一次读数完全符合的为准，计数错误时每次都不符合，仍然失败；`alloc_checks_retry_past_another_threads_allocation` 让另一个线程在第一次读数之间分配 4 KiB，确定地复现这种交错，要求第二次通过。同样条件下新的 `alloc_*` 150 次都通过，`lectern-bench` 的其他测试各 10 次都通过。
- macOS CI 上 `report_json` 的快照失败：macOS 构建里 GPUI 打开了 `serde_json` 的 `preserve_order`，JSON 的键按插入顺序输出，与 Linux 上的字母顺序不同。报告和合并后的 `report.json` 写出前统一按键名排序；`cargo nextest run -p lectern-bench --features serde_json/preserve_order` 在 Linux 上复现了这个失败，修复后 60 个测试都通过。失败的测试名来自 [#84](https://github.com/SoloJiang/lectern-app/pull/84) 的失败注释。
- `just bench`（冒烟）退出码 0：`target/lectern-bench/` 下有 `report-time.json`、`report-memory.json`、`report-process.json` 和 `report.json`；合并后的 78 条里 `smoke` 31 条（`time` 目标的全部生效条目），`planned` 45 条，`skipped` 2 条（`app/bundle_size`、`app/dmg_size`，原因「未打包」）。`memory`、`process` 目标在这一步没有生效条目。冒烟运行（含编译）在这台机器上约 80 秒，增量编译之后约 10 秒。
- `just bench agents/` 退出码 0：`agents/*` 的 4 条生效条目是 `smoke`、2 条计划中的是 `planned`，未打包的 2 条是 `skipped`，其余 70 条都是 `filtered`。`just bench cache/` 同样退出码 0：`cache/*` 的 9 条生效条目是 `smoke`、2 条计划中的是 `planned`，未打包的 2 条是 `skipped`，其余 65 条是 `filtered`。
- `lectern-bench-report merge --require package target/lectern-bench` 在没有 `report-package.json` 时退出码 2。
- 临时从 `benches/time.rs` 里去掉 `agents::APPEND_4K` 时，`time` 目标在运行前以 2 退出（「预算 agents/append_4k 没有对应的基准」），`just bench` 退出码 2。
- `rg -l perf_smoke crates` 没有结果（含 `crates/git/src/worktree/tests.rs`、`crates/git/src/changes/tests.rs`、`crates/cache/src/tests/`）；`rg -n 'perf_smoke' crates/agents/Cargo.toml` 没有结果；`rg -n '^name = "(criterion|divan)' Cargo.lock` 没有结果。`Cargo.lock` 只多了 `lectern-bench` 的依赖列表，没有新增包。
- `just bench --measure`（release、thin LTO，不判定）的中位数。不是预算规定的 M 系列 Mac，只说明基准在采样规则下能跑完、量级与原来的 `perf_smoke` 一致：

  | ID | 中位数 | 样本数 | 预算（M 系列） |
  |---|---|---|---|
  | `agents/parse_throughput` | 290 MB/s | 25 | ≥ 200 MB/s |
  | `agents/append_4k` | 14.7 µs | 10 000 | < 1 ms |
  | `agents/zst_throughput` | 166 MB/s | 15 | ≥ 100 MB/s |
  | `agents/zst_load_decoded` | 10.2 µs | 766 | < 1 ms |
  | `git/open` | 0.53 ms | 883 | < 15 ms |
  | `git/open_handle` | 52 µs | 849 | < 1 ms |
  | `git/diff_50_cold` | 4.76 ms | 168 | < 20 ms |
  | `git/diff_50_warm` | 1.39 ms | 666 | < 2 ms |
  | `platform/safe_stat` | 1.48 µs，`symlink_metadata` 的 1.19 倍 | 384 | ≤ 3 倍且 < 10 µs |
  | `platform/safe_read_4k` | 2.83 µs，`std::fs::read` 的 0.87 倍 | 418 | ≤ 1.5 倍 |
  | `executor/spawn_10k` | 50.7 ms（noisy） | 30 | 只记录 |
  | `executor/wake_1000x100` | 109 ms | 30 | 只记录 |
  | `kernel/service_get` | 0.80 µs（noisy） | 279 | 只记录 |
  | `kernel/service_for_each/{1,4,16}` | 0.86 / 3.32 / 12.6 µs | | 只记录 |
  | `kernel/service_current/{1,4,16}` | 0.95 / 1.22 / 3.26 µs | | 只记录 |
  | `kernel/emit`、`kernel/emit_nested` | 1.17 / 1.55 µs | | 只记录 |
  | `i18n/frame_strings` | 1.22 µs | 1 276 | 只记录 |
  | `cache/get_4k` | 4.2 µs | 727 | < 50 µs |
  | `cache/get_many_50` | 85 µs | 1 035 | < 1 ms |
  | `cache/get_1m` | 0.50–1.39 ms（三次运行） | 730 | < 2 ms |
  | `cache/open_existing` | 0.34 ms（noisy） | 2 315 | < 5 ms |
  | `cache/open_new` | 5.1 ms（noisy） | 185 | < 10 ms |
  | `cache/put_4k` | 54 µs（noisy） | 200 | < 1 ms |
  | `cache/batch_1000` | 6.9 ms（noisy，p95 36 ms） | 60 | < 15 ms |
  | `cache/zstd_1m/put`、`cache/zstd_1m/get` | 5.3 ms / 0.49 ms | 184 / 1 003 | 只记录 |

- `cache/*` 的中位数与 `perf_smoke_cache` 在同一台机器上的结果（[cache 设计「实际验证」](cache.md#实际验证)）同一量级，写入的长尾仍来自提交时的自动检查点。`cache/get_1m` 在完整运行里一次是 1.39 ms，单独运行两次是 0.50 和 0.57 ms，与 `perf_smoke_cache` 的 0.48–0.53 ms 一致。
- 没有验证的：macOS 上的判定和重试（只由 `runner_*` 用假基准覆盖）、`GITHUB_STEP_SUMMARY` 和 `report.json` 的上传（由 CI 运行）。

第 3 步（kernel 的插件集合闭包与裁剪），在同一类 Linux 虚拟机（x86_64，4 核）上运行：

- `just test -p lectern-kernel` 的 `set_closure_*` 7 个、`set_prepare_*` 3 个通过（kernel 共 86 个测试），对应验收的四项和三项：多层依赖、`MULTIPLE` 服务的多个提供者、缺提供者返回 `GraphError`（另有清单 panic、单实现服务多个提供者、未知 ID）、`retain` 之后 `validate` 通过；`prepare` → `dependency_closure` → `retain` → `with` → `validate` → `start_prepared` → `shutdown` 之后每个插件的 `manifest()` 恰好调用一次（含被 `retain` 去掉的插件），`status()` 的 `manifest_time` 与 `prepare` / `with` 记下的耗时相等，`manifest()` panic 的插件进入 `Failed`、状态与 `Kernel::start` 相同。详见 [kernel 设计「实际验证」](kernel.md#实际验证)。
- `just check`、`just test`、`just doctest`、`just deny` 通过。
- kernel 在冷启动路径上，`Kernel::start` 不能变慢。`kernel/*` 基准测的是服务查找和事件分发，不经过本步改动的代码。把 `origin/main` 导出到 `target/` 下，与本分支交替各运行三次 `just bench --measure kernel/`（中位数，三次依次列出），前后的差别在同一侧三次之间的波动以内：

  | ID | 改动前 | 改动后 |
  |---|---|---|
  | `kernel/service_get` | 510 / 703 / 703 ns | 331 / 633 / 709 ns |
  | `kernel/service_for_each/1` | 234 / 633 / 724 ns | 717 / 654 / 601 ns |
  | `kernel/service_for_each/4` | 0.97 / 2.08 / 3.02 µs | 2.81 / 2.11 / 2.19 µs |
  | `kernel/service_for_each/16` | 4.55 / 9.4 / 10.8 µs | 5.5 / 11.2 / 10.7 µs |
  | `kernel/service_current/1` | 299 / 818 / 647 ns | 697 / 684 / 661 ns |
  | `kernel/service_current/4` | 0.61 / 1.21 / 1.2 µs | 1.21 / 1.26 / 1.19 µs |
  | `kernel/service_current/16` | 2.3 / 3.03 / 3.93 µs | 3.85 / 2.95 / 3.69 µs |
  | `kernel/emit` | 448 / 632 / 630 ns | 440 / 668 / 728 ns |
  | `kernel/emit_nested` | 0.81 / 1.49 / 1.48 µs | 1.4 / 1.45 / 1.44 µs |

  `Kernel::start` 本身另用一个临时测试（不提交）测量，见 [kernel 设计「实际验证」](kernel.md#实际验证)：改动前后中位数都在 80–100 µs（20 个插件，含 `barrier`）。
- 没有验证的：探针怎样用这组接口（第 4 步），macOS 上的运行（kernel 不依赖 GPUI，行为与 Linux 相同）。

10 万文件的预算与内存归属（[ADR 0035](../adr/0035.md)，`Refs #7`），同一类 Linux 虚拟机：

- 预算表加入 `app/idle_100k`、`app/idle_100k_indexed`（计划中）、`git/open_100k`、`git/filter_100k/walk`、`git/changes_clean_100k/stat_all`、`git/changes_clean_100k/read_all`、`git/filter_cloudprint_bytes`、`git/filter_100k_bytes`、`git/compare_100k/peak_bytes`、`git/changes_count/total_bytes`（生效）、`git/compare_100k/retained_bytes`（计划中，`git` 第 4 步）、`git/changes_count/per_path_bytes`（计划中，stat 缓存的修正与 `git` 第 4 步中后完成的那个）、`git/filter_100k` 和 `git/changes_clean_100k`（计划中，macOS 上超标，见下）和 `fs_watch/resident_100k`（计划中）；`fs_watch/resident` 改为不含过滤器。新的夹具 `benches/fixtures/git_worktree.rs` 的 `shaped` 按 project 的 `Shape` 写出真实文件和带 stat 的 index（`core.ignoreCase = true`），只写树对象。实测值见 [git 设计「实际验证」](git.md#实际验证)。
- `budgets_belong_to_the_documented_targets` 把 `fs_watch/resident_100k` 归入 `Memory`。
- macOS CI（PR [#129](https://github.com/SoloJiang/lectern-app/pull/129)）上 `git/changes_clean_100k` 中位数 9.05、9.54 s（< 2 s），`git/filter_100k` 766、678 ms（< 400 ms），Linux 上约 1.0 s 和 214–276 ms；其余条目通过。预算不放宽：两行改为计划中，由定位并修正这项开销的 `git` PR 改为生效（git 设计「与设计的差异」）。基准在条目生效时才产出它们（`active`），另外总是记录 `git/filter_100k/walk`、`git/changes_clean_100k/stat_all`、`git/changes_clean_100k/read_all`，它们的值出现在 CI 的 Markdown 摘要里，下一个 PR 据此分辨开销在 stat、读取还是目录遍历。Linux 上依次是 163 ms、279 ms、393 ms。

只记录的结果输出为 `::notice` 注释（`Refs #7`），同一类 Linux 虚拟机：

- 只能经 API 读 check run 注释的环境里，上一段的 Markdown 摘要读不到。`lectern-bench-report merge` 在 `GITHUB_ACTIONS=true` 时为 `record` 结果按 ID 的第一段分组输出 `::notice`（见「报告」）。`report_merge_notices_group_record_results`、`report_merge_notices_keep_every_record_within_ten` 通过：分组与逐行格式、只取 `record`、12 个组合成 10 条且第 10 条带上剩下 3 组的全部结果；`report_merge_annotations_*` 不变。`just test -p lectern-bench` 64 个测试通过。
- 没有验证的：GitHub 实际保留和显示这些 notice（由下一次 macOS CI 运行确认）。目前只记录的结果分属 `app`、`baseline`、`cache`、`code_intel`、`executor`、`git`、`i18n`、`kernel`、`lsp`、`plugin`、`project` 11 组，macOS CI 上生效、实际产出 `record` 的组少于 10 个，每组各一条。

`git` 的 stat 缓存修正（`Refs #14`），同一类 Linux 虚拟机：

- `Value` 增加 `Count(u64)`：观察一次的计数，报告里是 `{"count": n, "samples": 1, "unit": "count"}`，文字是数字本身，没有适用的检查（`stats_counts_are_record_only`）。预算表加入只记录的 `git/changes_clean_100k/reads` 和 7 个 `…/stat_*`（「预算表」），`git/changes_count/per_path_bytes` 的「加入」改为只差 `git` 第 4 步。
- `just bench --measure git/`：`git/changes_count/total_bytes` 从 76.7 MB 降到 2.12 MB；计数在 Linux 上是 `reads` 1（根目录的 `.gitignore`）、`stat_*` 全为 0。数字见 [git 设计「实际验证」](git.md#实际验证)。
- macOS CI（[#130](https://github.com/SoloJiang/lectern-app/pull/130) 的 notice，修正之前的代码）：`git/filter_100k/walk` 502 ms、`git/changes_clean_100k/stat_all` 1.03 s、`git/changes_clean_100k/read_all` 14.6 s、`git/changes_count/total_bytes` 76.7 MB。`changes_clean_100k` 约 9 s，介于 stat 与读取全部文件之间，说明 macOS 上比较读了大部分文件。`read_all` 每个样本 14.6 s，按采样规则要跑十几个样本，CI 时间过长，所以去掉了它，连同依赖它的 `…/reads_after_read_all`：是否读、为什么读由 `…/reads` 和 `…/stat_*` 回答，读取的代价已经知道。rebase 到 #130 之后 `just test -p lectern-bench` 65 个测试通过。

CI 提速（[ADR 0036](../adr/0036.md)，`Refs #7`），同一类 Linux 虚拟机（4 核）：

- 新的测试：`runner_shares_fixtures_across_benches_and_retries`（两个基准和一次重跑只生成一次共享夹具，目标运行结束时释放；同一个键换类型时返回错误；`benches` 记下两个基准，超标重跑的那个 `attempts` 为 2）、`report_merge_lists_the_slowest_benchmarks_first`（合并后按耗时排序、带上目标，文本列出最慢的几个，notice 的格式）、`report_merge_notices_leave_room_for_the_slowest`（有耗时列表时 `record` 的分组只占 9 条）、`stats_warm_up_ends_after_three_calls_or_one_slow_call`、`stats_slow_operation_takes_the_minimum`（1 s 的操作是 1 次预热加 5 个样本），`stats_stop_rules_in_order` 改为 3 秒。rebase 到 #131 之后 `just test -p lectern-bench` 70 个测试通过，`just check`、`just test`（1328 个通过，跳过 1 个）、`just doctest`、`just deny` 和冒烟的 `just bench` 都通过。
- `just bench --measure`（release，不判定），改动前后各运行一次。这台虚拟机上同时有另一个任务在编译和测试（空闲时负载约 4），同一个基准两次之间可以差 4 倍（没有改动的 `project/refresh_index` 前后是 7.9 s 和 30.1 s），所以只看结构性的变化：
  - 进程内基准合计 424.3 s → 364.6 s（69 个）；改动前编译 1 分 26 秒，整个命令约 8.5 分钟。
  - 10 万文件的 git 基准（`time` 目标 3 个、`memory` 目标 2 个）：改动前 121.7 s（`git/changes_clean_100k` 32.6 s、`git/filter_100k` 28.5 s、`git/open_100k` 24.6 s、`git/filter_100k_bytes` 19.7 s、`git/compare_100k` 16.3 s，每个都生成一次夹具），改动后 67.9 s：每个目标只在第一个用到的基准里生成一次（`git/changes_clean_100k` 9.7 s、`git/compare_100k` 55.7 s，后者在负载最高时），其余三个是 0.3–1.1 s。
  - `code_intel/extract_1mib` 21.5 s → 12.2 s、`code_intel/extract_throughput` 17.5 s → 12.0 s、`project/facts_contains` 15.4 s → 9.1 s：单次 0.1–1 s 的操作按 3 秒封顶采样。
  - 最慢的仍是 `code_intel/disk_100k`（76.7 s、80.5 s）：只记录、只运行一次，时间是 9 万个文件的抽取和写入，本次没有改它。
- 这两次测量在 #131 合并之前（`git/changes_clean_100k/read_all` 还在，改动后只取 1 个样本）；rebase 到 #131 之后它已不存在，`…/stat_all` 取 1 个样本。
- 没有验证的：macOS 上的耗时，由下一次 CI 的 `slowest benchmarks` notice 给出。

`git` 第 4 步里比较用的 index 不跨调用保留（`Refs #14`），同一类 Linux 虚拟机：

- `git/compare_100k/retained_bytes` 和 `git/changes_count/per_path_bytes` 改为生效：前者由 `git/compare_100k` 一并产出（丢掉改动集之后的存活字节减去打开 `Worktree` 之后的），后者由 `git/changes_count` 产出（`total_bytes` 的每个样本除以改动路径数）。加入只记录的 `git/refresh_one_100k`、`git/canonical_oid_100k` 和 `git/canonical_oid_100k/batch_100`（`time` 目标，共享的大仓库，路径不改动，保持只读）。`just test -p lectern-bench` 70 个测试通过。
- `just bench --measure git/`：`retained_bytes` 64 B（< 1 MB），`per_path_bytes` 980 B（≤ 1 KB），`git/refresh_one_100k` 32.5 ms，`git/canonical_oid_100k` 21.7 ms、`…/batch_100` 22.2 ms。数字和取舍见 [git 设计「实际验证」](git.md#实际验证)。

时间盒截断的轮次（[ADR 0043](../adr/0043.md)，`Refs #17`），基于 main b9e0fb8，同一类 Linux 虚拟机（4 核）：

- 原因：macOS CI 上 `code_intel/references_full` 间歇报 `not complete: Done(Partial { unsearched: 0, outside: … })`（ADR 0043「背景」）。在查询里加临时计时复现：无负载时每轮 1.08–1.27 s（`ci_ref` 一轮 0.85–1.02 s，核算的遍历 0.23–0.31 s，52 301 个提示 108 条语句，0 缺失、0 次淘汰）；4 个忙循环占满 4 核时第 11 轮 1.917 s 结束，范围外 7 497 个提示没核对，`Partial { unsearched: 0, outside: 7430 }`。临时计时没有提交。
- 新的测试：`rounds_complete_rounds_keep_their_times`、`rounds_a_time_boxed_round_is_charged_and_recorded`、`rounds_other_incomplete_rounds_are_errors`（`unsearched`、时间盒之前结束、单元缺失或不知道、有淘汰或不知道、没有留下文件，各自仍是错误）、`rounds_the_median_fails_once_more_than_half_are_cut`（按预算表里的两条预算：11 个样本里 5 轮截断通过、6 轮超标）、`rounds_cuts_give_their_record_results`、`runner_notes_go_into_the_report`（重跑时取后一次的说明）、`report_merge_notes_reach_the_annotations`。`just check` 通过，`just test -p lectern-bench` 77 个通过（跳过 6 个），`just test -p lectern-code-intel` 348 个通过。
- `just bench --measure code_intel/references_full`，无负载：`/all` 1.19 s、`/first` 10.2 ms，`partial_rounds` 0、`outside` 0。
- 同样的命令，4 个忙循环：第一次没有轮次被截断（`/all` 1.55 s、`/first` 12.4 ms）；第二次 5 轮被截断，不报错，作为 3.85 s 左右的样本计入（样本最大值 3.85 s），中位数 `/all` 1.93 s、`/first` 24.3 ms，仍在预算内；`partial_rounds` 5、`outside` 33 179。`GITHUB_ACTIONS=true` 合并时 `code_intel record` 的 notice 带上说明：「5 rounds cut by the time box, the last with 33179 outside; done after 1.942528946s of a 1.9s time box; Some(90301) units stored, Some(0) evictions」。
- 没有验证的：macOS 上截断的频率，由之后的 CI 运行的 notice 给出。

第 4 步（探针、进程调度与空窗口基线，[ADR 0051](../adr/0051.md)），在 macOS 上运行：Apple M5 Pro（Mac17,9，15 核），macOS 27.0.1。这台机器的屏幕处于锁定状态（`IOConsoleLocked = Yes`，`CGSSessionScreenIsLocked` 为 1），每个 drawable 的 `presentedTime` 都是 0，首帧的来源都是 `handler`（[ADR 0052](../adr/0052.md)），所以本机的数字不是 ADR 0017 要的基线。空窗口基线的实测值由 macOS CI 给出，本节在 CI 运行之后补上。

- `just test -p lectern-bench`：130 个测试通过，跳过 6 个。本步新增 `wire_*` 6 个、`probe_*` 15 个、`process_*` 27 个、`purge` 集成测试 3 个（`tests/purge.rs`）和 `runner_skipping_benchmark_is_skipped_not_an_error`。`just test -p lectern-platform`：197 个通过（合并 ADR 0052 的 `platform` 改动之后）。`wire_*`、`probe_*`、`process_*` 覆盖首帧的来源：`first-frame` 两种来源的往返、缺少 `source` 和不认识的来源都是错误；探针把 `PresentedAt` 的来源原样发出；冷启动的每个样本带自己那次启动的来源，会话的 `source()` 是 `FirstFrame` 里的来源。
- `probe_*` 用真实的 `Kernel` 和计数的测试插件，运行器一侧是临时目录里的 Unix 域 socket：两种裁剪（`closure:`、`closure+self:`）之后每个插件的 `manifest()` 在整个启动里只调用一次；`attach` 满足 `Command`、`Manual`、`Workspace` 三种激活条件后发出 `Ready`，空闭包立即 `Ready`；`bench-probe` 丢弃序号过时的 `Busy` / `Quiescent`，kernel 保持静止；所选插件进入 `Failed` 时发出 `PluginFailed`，`ActivateAll` 报告失败的插件；`FirstFrame` 先发出，之前积压的报告随后按顺序发出；`drive` 之前到达的请求排队，之后按顺序应答；无效的请求行、无效的 `LECTERN_BENCH_PLUGINS` 和裁剪不了的选择发出 `Error`。
- `process_*` 用脚本化的 `Launcher` / `ProcessInfo` 和假探针（一个线程按脚本连接 socket、发报告、应答请求），期限缩短到几十到几百毫秒：冷启动 2 次预热、20 个样本，同一时间只有 1 个进程；探针报告 `hook <名字>`、预热以 3 退出都是出错；不连 socket、等待期间退出、`Quit` 之后不退出或退出码不是 0、`FirstFrame` 的 pid 不是启动的那个，都是出错；基线在首帧后等 `settle`；`Busy` 让等待重新开始，在第一次 `Epoch` 的应答之前到达的 `Busy` 也是；采样前后代数不同时重采，连续 3 次无效出错；首帧后不静止返回 `Idle::NotQuiescent`；`process_deadline_*` 对 `Epoch`、`Plugins`、`Ready`、`Milestone`、`Frames` 各一个，原因写明等的消息；`PluginFailed` 和 `frames <…>` 立即出错；进程组展开到孙进程、按 pid 去重、跳过已退出的；结束会话时向进程组和探针报告的 `Child` 根进程发 `SIGKILL`；内存类最多 3 个同时运行，第一个出错之后不再开始新的启动。`process_*` 连续运行 25 次都通过，同时运行 16 个 `yes` 忙循环时 20 次、45 个时 15 次都通过。
- macOS CI 的 `Build and test (macOS arm64)` 上 `process_busy_restarts_the_settle` 失败过一次：负载高时假探针在 `Quiescent` 之后的 30 ms 睡眠超过了 100 ms 的等待，`Busy` 和下一个 `Quiescent` 在等第一次 `Epoch` 的应答时才到，样本离最后一个 `Quiescent` 不到一个等待时长。这是 `Session::idle` 的缺陷，不只是测试的问题：现在第一次应答之前收到 `Busy` 时重新等待。新的 `process_busy_before_the_first_epoch_answer_restarts_the_settle` 确定地复现这种顺序（假探针收到第一个 `Epoch` 时才发 `Busy` 和 `Quiescent`，然后应答），修复之前失败（样本在最后一个 `Quiescent` 之后 40 µs），修复之后通过。
- `purge`：假的 `sudo` 只放在这条命令的 `PATH` 最前面，记录参数 `-n purge`；回放成功时为 true，失败和没有 `sudo` 时为 false。
- `just check` 通过（含 ADR 0051 修改后的架构检查和文档链接）；`python3 scripts/test_check_architecture.py` 26 个测试通过；`python3 scripts/check_architecture.py --gpui-free-packages` 的输出仍包含 `lectern-bench`。`Cargo.lock` 只在 `lectern-bench` 的依赖列表里多了 `futures`、`gpui-pre-platform` 和 `libc`，没有新的包；`cargo metadata --format-version 1 --locked --all-features` 在不加和加 `--filter-platform x86_64-unknown-linux-gnu` 时都通过。
- `just bench baseline/`（这台机器，屏幕锁定），ADR 0052 之前：退出码 2，两条都是 `error`，原因「10s 内没有等到 FirstFrame」，各用 10.0 s。同样条件下 `platform` 的 `frame_hooks` 示例注册首帧回调用了 13.2–45.2 µs（11 个冷启动的进程，中位数 15.0 µs），之后 300 帧都没有首帧回调，以 101 退出。到期之后运行器结束了进程组：没有留下 `empty_window` 进程，系统临时目录下没有留下 `lectern-bench-*` 目录。
- `just bench baseline/`（同一台机器，屏幕锁定），ADR 0052 之后：退出码 0，两条都是 `record`。冷启动的 20 个样本 78.38–86.36 ms，中位数 82.1 ms，20 个都超过 20 ms，来源都是 `handler`；空闲内存 23.8、23.4、23.5 MB，中位数 23.5 MB，3 次的首帧来源都是 `handler`。屏幕锁定，来源 `handler`，不是 ADR 0017 要的基线。
- 第一次 CI 运行（[run 40204026597](https://github.com/SoloJiang/lectern-app/actions/runs/40204026597/job/121201015126)）的两条基线也都是 `error`「10s 内没有等到 FirstFrame」。之后两个临时的诊断提交（54b48d0、40c5468）只写探针的 stderr 和一个不经方法替换的示例，测出托管 runner 从不给 `presentedTime`，这台机器的屏幕也没有锁定（ADR 0052「背景」）。两个诊断提交已由 revert 提交去掉（af784a3、29e7931），本 PR 相对 `main` 的 diff 里没有它们。
- 冒烟模式（`cargo test --benches`，没有设置 `LECTERN_BENCH_BASELINE`）：两条基线都是 `skipped`，原因「没有设置 LECTERN_BENCH_BASELINE」。
- 没有验证的：空窗口基线的实测值（20 个冷启动样本和 3 次空闲内存）和真实的首帧回调路径，由 macOS CI 的 `Benchmarks (macOS arm64)` 给出，之后写进本节和 ADR 0017（「验收」第 4 步第 3 条）；注册首帧回调出错的真实路径（只由 `process_cold_start_stops_at_a_hook_error` 用假探针覆盖）；Linux 上的编译和测试（这台机器只装了 `aarch64-apple-darwin` 目标，由 Linux CI 给出）。

## 与设计的差异

第 1 步：

- 迁移多了两组：`agents/zst_throughput`、`agents/zst_load_decoded`（`agents` 第 3 步写进了同一个 `tests/perf_smoke.rs`）和 `git/diff_50_cold`、`git/diff_50_warm`（`git` 第 2 步的 `perf_smoke_git_diff_50`）。本文写于这两步合并之前；验收要求 `rg -l perf_smoke crates` 没有结果，只删不迁会丢掉这两组测量。预算表里这四条改为生效，「迁移」「范围」和第 1 步的验收已改。
- 同样的原因迁移了 `cache` 第 1 步（[#82](https://github.com/SoloJiang/lectern-app/pull/82)，在本文之后合并）加入的 `perf_smoke_cache`：`cache.md`「性能」的条目加进预算表，第 1 步已测量的 7 项读写与打开生效，`cache/evict_16m` 和 `cache/memory` 属于 `cache` 第 2 步，登记为计划中。`zstd_1m` 拆成 `cache/zstd_1m/put` 和 `cache/zstd_1m/get` 两条只记录的结果：原测试也分别打印写和读，一个 ID 只能放一组样本。`cache/memory` 暂定属于 `Memory` 目标；SQLite 用自己的 `malloc`，不经过 Rust 的全局分配器，计数分配器看不到页缓存，第 2 步实现这项基准时可能要改为进程级的 `phys_footprint`，那时一并修改目标。
- `main` 多了 `target` 和 `tmp_dir` 两个参数：`Target` 就是「各自把自己是哪个 `Target` 传给它」；`CARGO_TARGET_TMPDIR` 只在编译基准目标和集成测试时设置，库代码取不到，由基准目标用 `env!` 传入。已改「接口」，并补上 `BenchError` 的形状。
- 过滤优先于计划中：`just bench agents/` 之后，`project/*` 这类计划中的条目也是 `filtered`，与验收「其余条目为 filtered」一致。计划中条目的原因写「由 <负责方> 加入」。
- 非判定平台上的 measure 模式照常与预算比较，报告里是 `ok` / `over`，`gated` 为 false，退出码不因 `over` 非 0，也不重试。本文只写了「不判定，只报告」。
- 基准产出了计划中预算的结果，或者基准的前缀只对应计划中的预算时，按「基准与预算表对不上」处理（退出码 2，提示先改为 `Active`）。本文没有规定这种情况。
- 报告的结果对象多了 `measured`（实测统计量的文字）：文本表和 Markdown 表都从 JSON 渲染，合并工具不必重新计算；每份报告多了顶层的 `errors`，放不属于任何预算的问题（多出的基准或结果、重复的结果），合并时汇总，非空即退出码 2。JSON 的键按字母顺序输出：macOS 构建里 GPUI 打开了 `serde_json` 的 `preserve_order`（Linux 上没有），写出前统一排序，两个平台的报告逐字节相同。
- `host` 的 `model`、`chip`、`os_version` 暂为 null，只填 `cpus`：前三项来自第 2 步的 `platform::host`。
- `lectern-bench-report package` 没有实现：第 1 步的验收只要求 `merge`（含 `--require package`），体积测量随 [#38](https://github.com/SoloJiang/lectern-app/issues/38) 加入；现在只认 `merge`，其他子命令打印用法并以 2 退出。
- `platform`、`executor` 不打开 `test-support`：bench 只用它们的公开接口，`platform` 也没有这个 feature。`i18n` 新增 `test-support` 下的 `testing::frame_strings`（遍历全部消息要用到生成的表格，见 [i18n 设计「性能」](i18n.md#性能)），`git` 新增 `testing::open_handle`（[git 设计](git.md#第-1-步补全的类型)）。依赖表已改，`lectern-git` 从「以后的 dev」移到 dev，另加 `lectern-services`（会话类型）和 `insta`。
- `git` 的夹具由 `TestRepo` 生成，它在系统临时目录里建自己的 `TempDir`（drop 时删除），不在 `Ctx::scratch()` 下：`TestRepo` 不接受目录参数，只为 bench 改它的接口不值得。
- `benches/time/*.rs` 用 `#[path]` 引入：基准目标的根是 `benches/time.rs`，按 Rust 的模块规则子模块默认在 `benches/` 下。`[lib]` 和 `[[bin]]` 设了 `bench = false`，否则 `cargo test --benches` 会把库的单元测试再跑一遍，`cargo bench` 也会把库当作 libtest 基准运行。
- `process` 和 `memory` 目标在这一步没有基准：它们的预算都还是计划中的。`process` 目标的报告只有 `planned` 条目和空的 `plugins`、`language_servers`；「`process` 目标的生效条目都是 `skipped`」这条规则由 `runner_process_target_is_skipped_off_macos` 用假预算表验证。`just bench` 在 macOS 上构建 `empty_window` 和测量用 `lectern` 的两行、两个 `LECTERN_BENCH_*` 环境变量，随第 4、5 步加入。
- 帧判定的测试（60 Hz、9 ms、阻塞两个周期、120 Hz）是 `stats_frames_*`：判定在统计模块里，不依赖进程调度；`time_with_setup` 每个样本调用一次 `setup` 的测试是 `runner_time_with_setup_calls_setup_once_per_sample`。本文把这两组列在 `process_*` 下。`budgets_*` 另检查了 ID 的写法、`Tier::Share` 的出处是某份设计文档的「性能」、每条预算属于「基准目标」一节规定的目标、`CountAbove` 只与 `CountAtMost` 配合。

第 3 步（kernel，详见 [kernel 设计「与设计的差异」](kernel.md#与设计的差异)）：

- `PreparedSet::manifests` 返回 `impl Iterator<Item = (&Name, &Manifest)>`，不是切片：集合里每一项还带插件对象、耗时和 panic 消息，返回切片要另存一份清单。探针只遍历它。「需要其他 crate 提供的接口」已改。
- 新增 `PreparedSet::validate`：第 3 步的验收要求 `retain` 之后 `validate` 通过，而 `PluginSet::validate` 会再调用一次 `manifest()`。新增 `GraphError::UnknownPlugin`，`dependency_closure` 收到集合里没有的 ID 时返回它。
- 只留闭包（`closure:<id>`）的集合在一种情况下不能激活：`MULTIPLE` 服务的某个提供者反过来依赖 `<id>`。这时闭包里有这个提供者，却没有它依赖的 `<id>`，探针等不到 `Ready`、基准记为出错。记为已知限制（[kernel 设计「插件集合的闭包与裁剪」](kernel.md#插件集合的闭包与裁剪)第 3 条）。

第 4 步：

- 探针插件 `bench-probe` 放在本 crate（「探针」），而 ADR 0025 的架构检查要求实现了 `Plugin` 的 crate 都标 `plugin = true`；本 crate 以 dev-dependency 依赖插件 crate，不能是插件 crate。[ADR 0051](../adr/0051.md) 让架构检查在这一条里跳过 tool 层 crate，并限定 tool 层的 `Plugin` 实现只用于测量和测试、不提供也不要求服务。
- `Probe::start_kernel` 不裁剪时也先 `PluginSet::prepare`，再 `Kernel::start_prepared`，不调用 `Kernel::start`：`Plugins` 的应答要带每个插件清单里的激活条件（`plugin/after_first_frame` 按它选插件），`ActivateAll` 要按激活条件逐个满足，而 `PluginStatus` 不带激活条件，`Kernel::start` 又消耗了插件集合。两条路径进入同一个 kernel 线程函数，`start_prepared` 用 `prepare` 记下的清单和耗时，每个 `manifest()` 仍只调用一次，`plugin/*/activate` 的四段耗时口径不变。区别是测量构建里清单在调用 `start_kernel` 的线程上读取（`app` 里是首帧之后的主线程），不在 kernel 线程上；第 3 步测得 20 个插件的 `Kernel::start` 共 80–100 µs，清单只是其中一部分。
- 新增 `Probe::drive`：`empty_window` 没有 kernel，设计只有 `attach(kernel, driver)`。`drive` 只交出 `Driver`：`Epoch` 应答 None，`Plugins` 只应答 `PluginsEnd`，`ActivateAll` 立即应答 `Ready`。`attach` 或 `drive` 之前到达的请求排队。
- `LECTERN_BENCH_PLUGINS` 的值无效或裁剪不了（ID 不在集合里、闭包出错）时，探针发出 `Error`，整个集合照常启动：运行器立即把这个基准记为 `error`，不等 `Ready` 的期限。设计没有规定这种情况。
- 环境变量名 `LECTERN_BENCH_SOCKET`、`LECTERN_BENCH_PLUGINS` 是 `wire` 的常量 `SOCKET_VAR`、`PLUGINS_VAR`，`probe` 重新导出：运行器默认编译，不能用只在 `probe` feature 下才有的模块。
- 依赖：`lectern-platform` 从 dev 和可选依赖改为普通依赖，`main` 用它设置基准线程的 QoS（「数据流与线程」第 2 条，本步补上），报告的 `host` 用 `platform::host::describe` 填上第 1 步暂为 null 的三项，进程级基准用 `platform::process`；`probe` 只另外打开它的 `bench-hooks`。`tempfile` 从 dev 改为普通依赖：进程调度在默认编译的 `lectern_bench::process` 里，每次启动的临时目录由它建。新增 dev 依赖 `libc`（`benches/process.rs` 向进程组发 `SIGKILL`）和 `futures`（`empty_window` 的通道），都已在工作区的依赖图里，「不新增外部 crate」仍成立。「依赖」表已改。
- 新增 `BenchError::skip`：「状态与异常」要求冒烟模式下缺少被测程序、`sudo -n purge` 不可用时记为 `skipped`，第 1 步的 `BenchError` 只能表示 `error`。基准返回 `BenchError::skip(原因)` 时，它的条目记为 `skipped` 并带上原因，不影响退出码。
- 进程调度的接口写进了新的「进程调度」一节，设计原来只写了两个 trait 的职责：`Launcher` 拆成 `Launcher`（启动）和 `Launched`（一个已启动的进程：pid、`try_wait`、`kill_group`、`kill`），`Limits` 让测试缩短期限，`Session` 的方法供第 5 步的基准使用。
- 结束进程的顺序：设计写的是先向进程组发 `SIGKILL`，再按展开规则结束仍然存活的后代。实现先展开进程树（从主进程和探针报告的 `Child` 根进程开始），再向进程组发 `SIGKILL`，最后结束展开得到的后代：主进程被结束后，它的子进程改由 launchd 收养，`children(pid)` 就找不到它们了。之后才关闭 socket，假探针因此不会在被结束之前自己正常退出。
- 每次启动的环境除了两个 `LECTERN_BENCH_*` 变量，还把 `HOME` 设为这次启动的临时目录，删除 `LECTERN_LOG`、`CLAUDE_CONFIG_DIR` 和 `CODEX_HOME`：被测程序不读写用户的数据、日志设置和 agent 会话。设计只写了「临时数据目录」。socket 和 `HOME` 在系统临时目录下同一个 `lectern-bench-*` 目录里，socket 路径短于 macOS `sun_path` 的 104 字节（`CARGO_TARGET_TMPDIR` 下可能更长）。
- `purge` 多了 `path` 参数：测试把假的 `sudo` 只放在这一条命令的 `PATH` 最前面，不改测试进程自己的环境（`std::env::set_var` 在多线程的进程里不安全）。基准传 None。
- `process_*` 只覆盖本步的调度。插件两次启动的差值与激活耗时、`plugin/after_first_frame`、`plugin/process_total` 和 `lsp/*` 的取法属于第 5 步的基准，测试随第 5 步加入；帧判定是 `stats_frames_*`，`time_with_setup` 是 `runner_*`（第 1 步「与设计的差异」）。首帧后 30 秒内不静止时会话返回 `Idle::NotQuiescent`，由第 5 步的 `app/idle_*` 记为 `over`。
- `baseline/empty_window_cold_start` 的结果带一条说明：20 个样本里超过 20 ms 的个数和最大值，即 `app/cold_start` 按 ADR 0017 判定时会看到的数，供「未决问题」里重新选择 A、B 时用；还有两种首帧来源各多少个和按启动顺序的每个样本（ADR 0052，报告的统计量里没有单个样本，ADR 0017 要记下它们）。`baseline/empty_window_idle` 的说明是 3 次的首帧来源和 3 个值。条目仍然只记录。
- 预算表里两条 `baseline/*` 从「计划中」改为生效，仍然只记录、没有上限：`budgets.rs` 只改了状态，没有改任何上限。
- `empty_window` 的窗口与 `app` 的 Welcome 窗口一样大（820 × 520），画的像素与 `app/cold_start` 相同；设计没有规定大小。它的 `Driver` 把 `Quit` 经通道转到主线程调用 `cx.quit()`，`Open` 什么也不做。
- 「验收」第 4 步第 3 条（实测值写进 ADR 0017）等 macOS CI 的数字。这台机器的屏幕锁定，首帧的来源是 `handler`，不是基线（「实际验证」）。
- 首帧的时刻按 [ADR 0052](../adr/0052.md)：托管 runner 从不给 `presentedTime`，原设计的首帧回调在那里永远不到。`FirstFrame` 多了 `source`（`presented` 或 `handler`，缺少或不认识时整行无效），`Scheduler::cold_start` 返回 `ColdStart`（样本和每个样本的来源），`Session` 多了 `source()`。`wire::Milestone` 和 `wire::FrameTimes` 的形状不变，来源随它们的使用方（B2 和第 5 步）加上。「报告协议」「探针」「进程调度」「进程级基准」已改。
- 静止后的等待覆盖到第一次 `Epoch` 的应答：应答之前收到的 `Busy` 也让等待重新开始，不计为无效样本。原来的文字只说 5 秒内收到 `Busy` 就重新等，`Busy` 和新的 `Quiescent` 恰好在 5 秒之后、第一次应答之前到达时，两次应答是同一个新的代数，样本会在新的静止之后不到 5 秒取得。「进程级基准」已改。
