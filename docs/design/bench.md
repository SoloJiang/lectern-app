# bench 设计

状态：已接受，尚未实现。

对应 issue [#7](https://github.com/SoloJiang/lectern-app/issues/7)；安装包体积检查 [#38](https://github.com/SoloJiang/lectern-app/issues/38) 复用本 crate 的运行器和预算表。`bench` 测量[性能预算](tech-research.md#性能预算)、[ADR 0017](../adr/0017.md)、[ADR 0018](../adr/0018.md) 和各 crate 设计文档「性能」一节里的每一项，打印实测值与预算，在 Apple 芯片的 macOS 上超标即以非 0 退出码让 CI 失败。

## 范围

- 做：
  - 基准运行器：预热、采样、统计、与预算比较、重试、文本报告和 JSON 报告、退出码。
  - 唯一的预算表：每个基准 ID 对应一条预算，写明统计量、上限和出处。
  - 进程内基准：直接调用领域、基础设施和 kernel 层 crate 的公开接口与 `testing` 模块。
  - 进程级基准：启动 `lectern`（或空 GPUI 窗口的基线程序）的子进程，测冷启动、空闲内存、每个插件的激活耗时与常驻内存、打开与切换的端到端耗时、帧时间。
  - 进程级基准的探针（`probe` feature）：`app` 在测量构建里调用它，向运行器报告首帧时刻、静止信号和插件状态。
  - 迁移现有的 `perf_smoke` 测试（`crates/agents/tests/perf_smoke.rs`、`crates/platform/src/safe_read/tests.rs` 里的 `perf_smoke`、`crates/git` 里的 `perf_smoke_git_open`）。
- 不做：
  - 取进程创建时间、`phys_footprint`、帧的 `presentedTime`、严格的 binary archive 模式、线程 QoS：macOS 专属，在 `platform`（架构规则 6），接口见「需要其他 crate 提供的接口」。
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
| `lectern-platform`（可选，`probe` feature） | 注册首帧回调 | 基础设施层 |
| dev：`lectern-agents`、`lectern-platform`、`lectern-kernel`、`lectern-executor`、`lectern-i18n`（各自打开 `test-support`），`tempfile` | 进程内基准的被测代码与夹具 | 只编译进基准目标 |
| dev（只在 macOS）：`gpui`、`gpui_platform` | `empty_window` 示例程序 | 只在 macOS 声明，Linux 上本 crate 仍不依赖 GPUI |
| 以后的 dev：`lectern-git`、`lectern-project` 和各插件 crate | 随这些 crate 的实现加入 | tool 层可以依赖任何层，包括插件 crate（[ADR 0025](../adr/0025.md)） |

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
pub fn main(benches: &[Bench]) -> ExitCode;
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
}

pub enum Target { Time, Memory, Process }

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

运行器和探针之间用一条 Unix 域 socket 通信，一行一条消息，UTF-8，形如 `first-frame pid=812 presented_unix_ns=…`：第一个词是消息名，其余是 `key=value`，值用百分号编码（空格、`%`、换行和非 UTF-8 字节）。

```rust
/// 探针 → 运行器。
pub enum Report {
    FirstFrame { pid: u32, presented: SystemTime },
    Busy { epoch: u64 },
    Quiescent { epoch: u64 },
    /// 所选插件都已进入 Active（只在裁剪了插件集合时发出）。
    Ready,
    /// 对 `Request::Epoch` 的应答：`Kernel::quiescent_epoch()`。
    Epoch(Option<u64>),
    /// 对 `Request::Plugins` 的应答，每个插件一条，以 `PluginsEnd` 结束。
    Plugin(PluginReport),
    PluginsEnd,
    Milestone { milestone: Milestone, presented: SystemTime },
    Frames(FrameStats),
    /// Lectern 启动的子进程，内存基准按类别分别统计（ADR 0018）。
    Child { pid: u32, kind: ChildKind, id: String },
    Error { message: String },
}

/// 运行器 → 探针。
pub enum Request { Epoch, Plugins, Open(OpenTarget), Quit }

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
/// 一段滚动或动画的逐帧时刻，按帧顺序。
pub struct FrameStats { pub scenario: String, pub refresh: Duration, pub sync: bool, pub frames: Vec<FrameTimes> }
pub struct FrameTimes { pub started: SystemTime, pub presented: SystemTime }
pub enum ChildKind { ProcessPlugin, LanguageServer, Other }

impl Report  { pub fn encode(&self) -> String; pub fn decode(line: &str) -> Result<Report, WireError>; }
impl Request { pub fn encode(&self) -> String; pub fn decode(line: &str) -> Result<Request, WireError>; }
```

### 探针（`lectern_bench::probe`，`probe` feature）

`app` 的 `bench-probe` feature（不在 `default` 里）打开 `lectern-bench/probe` 并调用下面的函数。发布构建不打开它，bench 不进入发布构建（架构规则检查第 2 条）。

```rust
pub struct Probe { /* socket 路径、插件选择、首帧状态 */ }

impl Probe {
    /// 首帧之前、`main` 开头调用。只读环境变量 `LECTERN_BENCH_SOCKET`、`LECTERN_BENCH_PLUGINS`、`LECTERN_BENCH_ARCHIVE`，
    /// 不做 IO、不建线程。没有设置 `LECTERN_BENCH_SOCKET` 时返回 None，应用照常运行。
    /// `LECTERN_BENCH_ARCHIVE=strict` 时立即调用 `platform::metal::set_strict_archive(on_miss)`（仍在创建窗口和管线之前）。
    pub fn from_env() -> Option<Probe>;

    /// 首帧之前、创建窗口之前调用：通过 `platform::frame::on_first_presented` 注册回调。
    /// 回调只记下时刻，然后在一个新线程上连接 socket、发出 `FirstFrame`，之后由这个线程应答请求。
    pub fn watch_first_frame(&self);

    /// 首帧之后、`Kernel::start` 之前调用：按 `LECTERN_BENCH_PLUGINS` 裁剪插件集合，并加入 `bench-probe` 插件。
    /// `closure:<id>` 只留 `<id>` 的依赖闭包，`closure+self:<id>` 留依赖闭包加 `<id>`；没有设置时保留整个 profile。
    pub fn select(&self, set: PluginSet) -> PluginSet;

    /// `Kernel::start` 之后调用。裁剪过插件集合时，逐个满足所选插件的激活条件：
    /// `Command` 调用一次 `Kernel::command`，`Manual` 调用 `Kernel::activate`，
    /// `Workspace` 由 `bench-probe` 插件在 `started` 里提交一份对任何 pattern 都返回 true 的 `WorkspaceFacts`；
    /// 全部进入 Active 后发出 `Ready`。
    pub fn attach(&self, kernel: Arc<Kernel>, driver: Arc<dyn Driver>);

    /// `app` 把 `workspace` 报告的里程碑转给探针，`presented` 是画出这一状态的帧显示的时刻。任意线程。
    pub fn milestone(&self, milestone: Milestone, presented: SystemTime);

    /// `app` 把 `workspace` 记录的一段滚动或动画的逐帧时刻（取自 `platform::frame::record`）转给探针。任意线程。
    pub fn frames(&self, stats: FrameStats);

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

### 需要其他 crate 提供的接口

这些接口由对应 crate 在自己的设计文档里补充、在各自的 PR 里实现（见「验收」）。下面是 bench 需要的最小形状。

`platform`（macOS 实现；其他平台返回 `ErrorKind::Unsupported`，进程级基准在那里不运行）：

```rust
pub mod process {
    /// 内核记录的进程创建时间（ADR 0017 的测量起点）。
    pub fn start_time(pid: u32) -> io::Result<SystemTime>;
    /// 活动监视器里的「内存」。
    pub fn phys_footprint(pid: u32) -> io::Result<u64>;
    /// 父进程是 `pid` 的进程（直接子进程），调用时的快照。
    pub fn children(pid: u32) -> io::Result<Vec<u32>>;
}

pub mod frame {
    /// 本进程第一帧真正显示在屏幕上的时刻：第一个 drawable 的 presentedTime（addPresentedHandler），
    /// 换算到系统时钟。回调在任意线程上调用一次。注册本身不做 IO、不建线程。
    /// 怎样在不改 GPUI 的前提下拿到 drawable，由 platform 的设计决定。
    pub fn on_first_presented(f: impl FnOnce(SystemTime) + Send + 'static);

    /// 记录从现在到 `stop` 之间每一帧的两个时刻，换算到系统时钟：
    /// `started` 是主线程开始处理这一帧的时刻（收到帧回调、开始布局之前，所以帧回调之前主线程上的阻塞会推迟它）；
    /// `presented` 是这一帧的 drawable 的 presentedTime。
    pub fn record() -> FrameRecorder;
    /// `FrameRecorder::stop(self) -> Vec<FrameTimes>`；`FrameTimes { started, presented }` 是 platform 自己的类型，
    /// `app` 把它转换成 `wire::FrameTimes`。
    /// 主显示器的刷新周期。
    pub fn refresh_period() -> Duration;
    /// 测量构建用：关闭 `CAMetalLayer` 的显示同步，下一帧在上一帧提交后立即开始，帧率不受显示器刷新率限制。
    pub fn set_display_sync(enabled: bool);
}

pub mod metal {
    /// 测量构建用（ADR 0017）：之后创建的渲染管线都带 `failOnBinaryArchiveMiss`；未命中时不回退到运行时编译，
    /// 也不写本机的 archive 缓存，而是调用 `on_miss(管线名)`。必须在创建第一个管线之前调用。
    pub fn set_strict_archive(on_miss: impl Fn(&str) + Send + Sync + 'static);
}

pub mod thread {
    /// 把调用线程的 QoS 设为 user-interactive，让基准线程运行在性能核上。
    pub fn set_user_interactive();
}

pub mod host {
    pub struct HostInfo { pub model: String, pub chip: String, pub os_version: String, pub cpus: u32 }
    pub fn describe() -> HostInfo;
}
```

`kernel`（普通的公开接口，不放在 `test-support` 下：三者都只对清单做计算，发布构建里没有调用方，测量用的构建因此不需要打开 kernel 的 `test-support`）：

```rust
impl PluginSet {
    /// `id` 依赖的服务的提供者，传递闭包，不含 `id` 自己。调用每个插件的 `manifest()`。
    pub fn dependency_closure(&self, id: &str) -> Result<Vec<Name>, Vec<GraphError>>;
    /// 只保留 `keep` 返回 true 的插件。
    pub fn retain(self, keep: impl FnMut(&str) -> bool) -> Self;
    /// 每个插件的清单，探针用它找出激活条件。
    pub fn manifests(&self) -> Vec<(Name, Manifest)>;
}
```

`app`：`bench-probe` feature，打开 `lectern-bench/probe`；在上面写明的位置调用探针；实现 `Driver`；提供把应用数据目录指到临时目录的方式（运行器每次启动都用新的临时目录，不读写用户的真实数据）。具体写进 `app` 的设计文档。

`workspace`：画出某个里程碑状态的帧显示后、一段滚动或动画结束后，通过一个回调交给 `app`。里程碑的判定条件（「项目主页可用」指侧栏和会话列表画出真实内容而不是骨架）写进 `workspace` 和各视图的设计文档。

被测的领域、基础设施和 kernel 层 crate：见「夹具与 `test-support`」。

## 数据结构

### 基准目标

```
crates/bench/
  src/lib.rs          运行器、统计、报告
  src/budgets.rs      预算表
  src/wire.rs         报告协议
  src/probe.rs        探针（probe feature）
  src/process.rs      进程级基准的调度（通过 Launcher / ProcessInfo 两个 trait 访问子进程，测试里换成假的）
  benches/time.rs     进程内计时基准，按 crate 分模块（benches/time/agents.rs 等）
  benches/memory.rs   进程内内存基准，注册 CountingAlloc
  benches/process.rs  进程级基准：app/*、plugin/*、baseline/*
  examples/empty_window.rs  空 GPUI 窗口（required-features = ["probe"]），只在 macOS 有内容，其他平台的 main 直接返回
  tests/fixtures/     本 crate 自己的小样本（目前没有）
```

三个基准目标都是 `harness = false`，入口都是 `lectern_bench::main`，各自把自己是哪个 `Target` 传给它。`time` 与 `memory` 分开，是为了计时不经过计数分配器。预算表里的 `Budget::target` 规定每条预算由哪个目标产出：`*_bytes`、`*/resident` 这类进程内的字节数属于 `Memory`，`app/*`、`plugin/*`、`baseline/*` 属于 `Process`，其余属于 `Time`。

另有一个 `src/bin/lectern-bench-report.rs`，合并三个目标的报告（见「报告」）。它只用本 crate 的普通依赖。

### 报告

每个基准目标运行后打印自己的表（ID、实测统计量、预算、状态、出处），并写 `target/lectern-bench/report-<target>.json`（`report-time.json`、`report-memory.json`、`report-process.json`），三个目标不写同一个文件。每份报告列出属于这个目标的全部预算，各一条：被参数过滤掉的记为 `filtered`，`Planned` 的记为 `planned`。

`just bench` 在三个目标都运行完之后调用 `lectern-bench-report target/lectern-bench`：

- 读三份报告，检查每一条预算（`Active` 与 `Planned`）在三份报告里恰好出现一次，且出现在 `Budget::target` 指定的那一份里；缺少报告文件（目标没有编译成功或中途崩溃）、缺少或重复的条目都按出错处理。
- 带 `*` 的预算先展开再检查：`report-process.json` 记录这次运行的插件列表（`app/idle_empty` 的 `Plugins` 应答，不含 `bench-probe`），合并工具把 `plugin/*/activate` 展开成列表里每个插件的 `plugin/<id>/activate`（已有具体条目的插件用具体条目），每个展开后的 ID 恰好出现一次。插件列表为空时（Linux、`app` 尚未接入、进程级基准被过滤或跳过），带 `*` 的条目本身出现一次，状态是 `planned`、`skipped` 或 `filtered`；插件列表不为空时，带 `*` 的条目本身不应出现。进程级目标自己的对应检查用同一套展开规则。
- 合并成 `target/lectern-bench/report.json`；设置了 `GITHUB_STEP_SUMMARY` 时追加一张 Markdown 表。
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

状态取以下之一：`ok`、`over`、`record`（只记录）、`planned`、`filtered`、`smoke`、`skipped`（附原因，如「只在 macOS 运行」「sudo 不可用」）、`error`（附原因）。

## 数据流与线程

### 进程内基准

1. `main` 只对本目标检查对应关系：每条结果 ID 都在预算表里且 `target` 是本目标，本目标的每条 `Active` 预算都有基准产出它；别的目标的预算不参与。不对应时直接以 2 退出。冒烟模式同样检查，所以 Linux 任务就能发现漏登记的预算；跨目标的「恰好一次」由 `lectern-bench-report` 检查。
2. 在 macOS 上调用 `platform::thread::set_user_interactive()`。
3. 按 ID 顺序逐个运行，同一时间只运行一个基准。每个基准先在 `scratch()` 下生成夹具（不计时），再采样。
4. 统计、与预算比较。超标时用新的夹具把这个基准重跑一次，两次都超标才记为 `over`，两次的数据都写进报告。
5. 打印报告，写 JSON，返回退出码。

### 进程级基准

运行器从环境变量 `LECTERN_BENCH_APP`（`lectern` 的路径）和 `LECTERN_BENCH_BASELINE`（`empty_window` 的路径）取得被测程序，由 `just bench` 设置。每次启动：建一个新的 socket 和临时数据目录，带上 `LECTERN_BENCH_SOCKET` 等环境变量启动子进程，等它连上来。

- 冷启动（`app/cold_start`、`baseline/empty_window_cold_start`）：先启动 2 次不计入（预热系统文件缓存），再启动 20 次。`app/*` 的每一次启动，包括这 2 次预热和 `app/cold_start_purged`，都带 `LECTERN_BENCH_ARCHIVE=strict`：binary archive 缺 slice 时管线创建失败，探针的 `on_miss` 立即连接 socket 发出 `Error { message: "archive-miss <管线名>" }`，然后进程以退出码 3 退出；运行器收到这条消息或退出码 3 时，把这个基准记为 `error`（`just bench` 退出码 2）。严格模式下未命中不会写本机缓存，所以预热启动不能把缺失的 slice 补进缓存、掩盖后面的测量。`empty_window` 不用 GPUI 之外的 archive，不带这个设置。每次收到 `FirstFrame` 后取 `presented - platform::process::start_time(pid)`，发 `Quit`，等进程退出（10 秒不退出就结束它并记为出错）。
- 冷文件缓存（`app/cold_start_purged`）：`sudo -n purge` 之后启动 1 次，只记录（ADR 0017）。`sudo -n` 失败时记为 `skipped`。
- 空闲内存（`app/idle_empty`）：启动 3 次，每次按 [kernel「静止」](kernel.md#静止)采样：收到 `Quiescent` 后等 5 秒，期间收到 `Busy` 就重新等；然后发 `Epoch`、读 `phys_footprint(pid)`、再发 `Epoch`，两次都是同一个 `Some(n)` 时样本有效，否则重新等静止，连续 3 次无效记为出错。首帧后 30 秒内没有静止记为 `over`（ADR 0017）。取 3 次的中位数。`app/idle_empty` 只计主进程。子进程按 ADR 0018 以进程组为单位统计：对 `Child` 报告的每个根进程，运行器用 `ProcessInfo::children` 逐层展开，得到根进程及其全部后代（广度优先，按 pid 去重；展开期间已经退出的进程跳过），这一组的 `phys_footprint` 之和是这个插件的内存。所有 `ProcessPlugin` 组之和是 `plugin/process_total`，`LanguageServer` 组各自只报告。主进程的后代中不属于任何已报告进程组的，合计记为 `app/unclassified_children`（只记录），用来发现没有报告的子进程。展开与读取在同一次采样里进行，前后两次 `Epoch` 检查同样覆盖它们。同一次启动里发 `Plugins`，激活条件里含 `AfterFirstFrame` 的插件（不论它排在第几项、是否还有别的条件）四段耗时之和给出 `plugin/after_first_frame`。`baseline/empty_window_idle` 没有 kernel，在首帧后 5 秒采样。
- 每个插件（`plugin/<id>/*`）：插件列表取自 `app/idle_empty` 的 `Plugins` 应答（不含 `bench-probe`）。对每个插件启动两次，`LECTERN_BENCH_PLUGINS` 分别为 `closure:<id>` 和 `closure+self:<id>`，都等到 `Ready` 和有效的静止样本。每次启动的内存是主进程的 `phys_footprint` 加上这次启动里所有 `ProcessPlugin` 进程组（按上面的展开规则取根进程及其全部后代）的合计，语言服务器组不计入、单独报告；两次之差是 `plugin/<id>/resident`，所以进程插件自己的进程组（闭包那一次不存在，记为 0）计入它的常驻内存。第二次里这个插件的四段耗时之和是 `plugin/<id>/activate`（[kernel「性能」](kernel.md#性能)）。超标时再测两对，取三对的中位数。预算按项目形状给出的插件（如 `project`），两次启动都先 `Open(Project(<夹具>))`。
- 端到端（`app/open_project` 等）：每个样本是一次新的启动。等空窗口静止后（打开文件、Diff、切换 worktree 的，先打开 `cloudprint` 形状的夹具项目并再次静止）发 `Open(...)`，从发出请求（运行器取 `SystemTime::now()`）到对应 `Milestone` 的 `presented` 为一个样本，两者是同一台机器的系统时钟；每项 5 次启动，取中位数。帧时间取完整的帧间隔：对 `Frames` 里相邻两帧，`started` 之差与 `presented` 之差各算一个间隔，两个序列合在一起取最大值。间隔覆盖整个周期，主线程在布局之前的阻塞、布局、绘制、GPU 和显示都计入。显示器刷新周期长于 8.3 ms 时（CI 的虚拟显示器是 60 Hz），间隔会被刷新率卡在 16.7 ms，因此探针在这一段场景前调用 `platform::frame::set_display_sync(false)`，帧一帧接一帧地生成，间隔反映的是生成一帧的真实耗时；报告里写明 `refresh` 和 `sync`。刷新周期不超过 8.3 ms 的显示器上保持同步打开。`app/input_frames`：打开夹具项目、切到 Conversation 的 Terminal 页（终端是核心插件）后发 `Open(Input { text: <400 个字符的固定文本>, interval: 8 ms })`；`app` 的 `Driver` 把每个字符作为一次按键送进与真实键盘相同的分发路径，`workspace` 在注入期间持续请求帧并用 `platform::frame::record` 记录，结束后经 `Probe::frames` 交回（场景名 `input`），按上面的帧间隔规则计算。按键处理阻塞主线程、推迟下一帧时，间隔超过 8.3 ms 即超标。
- 安装包（`app/bundle_size`、`app/dmg_size`）：读 `LECTERN_BENCH_BUNDLE`、`LECTERN_BENCH_DMG` 指向的产物，`.app` 按目录下全部文件实际占用的块数求和（与 `du -sk` 相同），`.dmg` 取文件大小。目标 50 MB 只在报告里显示。

计时类的进程级基准逐个运行，期间不运行别的基准；内存类最多同时运行 3 个，只影响等待时间，不影响 `phys_footprint`。

运行器只通过 `Launcher`（启动、取 pid、等待、结束）和 `ProcessInfo`（`start_time`、`phys_footprint`、`children`）两个 trait 访问子进程；基准目标里用 `std::process` 和 `platform::process` 实现，单元测试里用脚本化的假实现。

## 预算表

初始条目如下，数字以出处为准。表中「加入」一列写的是哪一步或哪个 crate 的 PR 把它从「计划中」改为生效。

顶层预算（`Tier::Top`，改动先写 ADR）：

| ID | 统计量与预算 | 出处 | 加入 |
|---|---|---|---|
| `app/cold_start` | 20 次里 > 20 ms 的不超过 1 次，且最大值 ≤ 30 ms | [ADR 0017](../adr/0017.md#决策) | 第 5 步 |
| `app/cold_start_purged` | 只记录 | [ADR 0017](../adr/0017.md#影响) | 第 5 步 |
| `app/idle_empty` | 中位数 < 60 MB；首帧后 30 秒内静止 | [ADR 0017](../adr/0017.md#决策) | 第 5 步 |
| `app/idle_monorepo` | 中位数 < 100 MB | [ADR 0017](../adr/0017.md#决策) | `project_home` |
| `app/open_project` | 中位数 < 100 ms | [性能预算](tech-research.md#性能预算) | `project_home` |
| `app/open_file` | 中位数 < 16 ms | [性能预算](tech-research.md#性能预算) | `file_reader` |
| `app/open_diff` | 中位数 < 50 ms | [性能预算](tech-research.md#性能预算) | `review_view` |
| `app/switch_worktree` | 中位数 < 100 ms | [性能预算](tech-research.md#性能预算) | `worktrees` |
| `app/scroll_frames` | 最大帧间隔 ≤ 8.3 ms | [性能预算](tech-research.md#性能预算)「滚动与输入」 | `file_reader` |
| `app/input_frames` | 最大帧间隔 ≤ 8.3 ms | [性能预算](tech-research.md#性能预算)「滚动与输入」 | `terminal_view` |
| `app/animation_frames` | 最大帧间隔 ≤ 8.3 ms | [ADR 0015](../adr/0015.md) | `ui` |
| `app/bundle_size` | ≤ 80 MB（目标 50 MB 只显示） | [ADR 0018](../adr/0018.md#决策) | [#38](https://github.com/SoloJiang/lectern-app/issues/38) |
| `app/dmg_size` | ≤ 40 MB | [ADR 0018](../adr/0018.md#决策) | [#38](https://github.com/SoloJiang/lectern-app/issues/38) |
| `plugin/process_total` | 全部进程插件的进程组（根进程及其全部后代）合计 ≤ 50 MB | [ADR 0018](../adr/0018.md#影响) | `kernel` 的进程插件宿主 |
| `app/unclassified_children` | 只记录 | [ADR 0018](../adr/0018.md#影响) | 第 5 步 |
| `baseline/empty_window_cold_start` | 只记录 | [ADR 0017](../adr/0017.md#影响) | 第 4 步 |
| `baseline/empty_window_idle` | 只记录 | [ADR 0017](../adr/0017.md#影响) | 第 4 步 |

分项预算（`Tier::Share`，随出处的设计文档修改）：

| ID | 统计量与预算 | 出处 | 加入 |
|---|---|---|---|
| `plugin/*/activate` | 四段之和 < 1 ms | [kernel](kernel.md#性能) | 第 5 步 |
| `plugin/after_first_frame` | 激活条件含 `AfterFirstFrame` 的插件四段之和合计 < 5 ms | [kernel](kernel.md#性能) | 第 5 步 |
| `plugin/*/resident` | 只记录；插件的设计文档给出数字时另加具体条目 | [kernel](kernel.md#性能) | 第 5 步 |
| `plugin/project/resident_cloudprint`、`plugin/project/resident_100k` | < 2 MB / < 12 MB | [project](project.md#性能) | `project` |
| `agents/parse_throughput` | 中位数吞吐 ≥ 200 MB/s | [agents](agents.md#性能) | 第 1 步 |
| `agents/append_4k` | 中位数 < 1 ms | [agents](agents.md#性能) | 第 1 步 |
| `agents/discover_1000` | 中位数 < 500 ms | [agents](agents.md#性能) | `agents` 第 4 步 |
| `agents/turns_cached_50mb` | 中位数 < 10 ms | [agents](agents.md#性能) | `agents` 第 4 步 |
| `agents/zst_throughput` | 中位数吞吐 ≥ 100 MB/s | [agents](agents.md#性能) | `agents` 第 3 步 |
| `agents/zst_load_decoded` | 中位数 < 1 ms | [agents](agents.md#性能) | `agents` 第 3 步 |
| `git/open` | 中位数 < 15 ms | [git](git.md#性能) | `git` |
| `git/open_handle` | 中位数 < 1 ms | [git](git.md#性能) | `git` |
| `git/changes_clean`、`git/changes_50` | 中位数 < 15 ms | [git](git.md#性能) | `git` |
| `git/refresh_one`、`git/refresh_full` | 中位数 < 15 ms（`refresh(Full)` 的份额，一个路径的刷新不会更慢） | [git](git.md#性能) | `git` |
| `git/diff_50_cold` / `git/diff_50_warm` | 中位数 < 20 ms / < 2 ms | [git](git.md#性能) | `git` |
| `git/changes_capped` | 中位数 < 1 s | [git](git.md#性能) | `git` |
| `git/changes_capped_bytes` | 存活字节 ≤ `Limits` 默认上限之和（数字由 `git` 的实现写入 git 设计） | [git](git.md#性能) | `git` |
| `git/resident` | 存活字节 ≤ 48 MB（hunk LRU 16 MB + 一个 `ChangeSet` 32 MB，空闲时不持有句柄） | [git](git.md#性能) | `git` |
| `project/open_cloudprint` | 中位数 < 20 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/packages_cloudprint` | 中位数 < 15 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/open_100k/files`、`project/open_100k/packages` | 中位数 < 80 ms / < 150 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/switch_worktree` | 中位数 < 30 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/resolve_package/hit`、`…/miss_cloudprint`、`…/miss_100k` | 中位数 < 1 ms / < 20 ms / < 150 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/refresh_index/cloudprint`、`…/100k` | 中位数 < 15 ms / < 60 ms | [project](project.md#性能) | `project` 第 5 步 |
| `project/facts_contains/name`、`…/path` | 中位数 < 1 µs / < 5 ms | [project](project.md#性能) | `project` 第 5 步 |
| `platform/safe_stat` | 中位数 ≤ 同一路径 `symlink_metadata` 的 3 倍，且 < 10 µs | [platform](platform.md#性能) | 第 1 步 |
| `platform/safe_read_4k` | 中位数 ≤ `std::fs::read` 的 1.5 倍 | [platform](platform.md#性能) | 第 1 步 |
| `executor/spawn_10k`、`executor/wake_1000x100` | 只记录 | [executor](executor.md#性能) | 第 1 步 |
| `kernel/service_get`、`kernel/service_for_each/{1,4,16}`、`kernel/service_current/{1,4,16}`、`kernel/emit`、`kernel/emit_nested` | 只记录 | [kernel](kernel.md#性能) | 第 1 步 |
| `i18n/frame_strings` | 只记录（en 资源里每条消息各取一次、带变量的用样例参数格式化） | [i18n](i18n.md#性能) | 第 1 步 |

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

- 预热：至少调用 3 次且至少 100 ms。`time` 和 `compare` 在预热时确定批量次数 k，使一个样本不短于 1 ms（`facts_contains/name` 这类 1 µs 以下的操作）；样本值是批量耗时除以 k。`time_with_setup` 不批量，k 固定为 1（`agents/append_4k`、`git/refresh_one` 单次在 10 µs 以上，`Instant` 在 M 系列上的分辨率约 42 ns，足够单次计时）。
- 采样：至少 10 个样本且至少 1 秒，到 100 个样本或 10 秒为止。慢操作（`agents/discover_1000`）可以用 `Ctx::config` 降到 5 个样本，不能更少。
- 统计量默认取中位数；冷启动按 ADR 0017 用计数和最大值；帧时间按 ADR 0015 用最大帧间隔。
- 吞吐是 `bytes / 中位数耗时`；比值是被测与对照交替采样后两个中位数之比。
- 不设容差：预算就是表里的数字。抗噪声靠预热、中位数和一次重试：超标的基准用新的夹具重跑一次，两次都超标才算超标。
- 离散度：中位数绝对偏差超过中位数 10% 的结果在报告里标为 `noisy`，不影响判定，用来发现不稳定的基准。
- 确定性：夹具由固定种子生成，不取时钟和随机数；基准按 ID 顺序运行；运行器在 macOS 上把基准线程设为 user-interactive QoS。
- 页缓存：进程内基准测的是热缓存（预热之后）；冷文件缓存只在 `app/cold_start_purged` 里测量。

判定用的机器是 CI 的 `macos-15` runner（Apple M1 虚拟机）。它与实体 M 系列 Mac 的差距由第 4 步的基线测出，见「未决问题」。

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
    cargo run -q --locked -p lectern-bench --bin lectern-bench-report -- target/lectern-bench
```

- `--no-fail-fast` 让一个目标超标后其余目标照常运行。cargo 对失败的目标统一返回 101，所以退出码不取 cargo 的，而取 `lectern-bench-report` 的。

- 其余参数是 ID 过滤，如 `just bench agents/`。
- `cargo bench` 使用 `bench` profile（继承 `release`：thin LTO、`codegen-units = 1`），与预算要求的发布构建一致。测量用的 `lectern` 与发布构建的差别只有 `bench-probe`；安装包体积测量用不带它的构建。
- 冒烟用 `test` profile，和 `just test` 共用依赖的编译产物。nextest 不运行 `harness = false` 的目标，所以冒烟另由 `just bench` 运行。
- `just ci` 在两个平台上都调用 `just bench`（Linux 上是冒烟）。

CI：

- Linux 任务在 Doctests 之后增加一步 `just bench`。目标耗时 2 分钟以内（大部分编译产物与测试步骤共用）。
- macOS 任务保持现有的 `just bench` 一步，之后用 `actions/upload-artifact` 上传 `target/lectern-bench/report.json`（`if: always()`，超标时也能看到数据）。报告表同时写进任务摘要。目标耗时 15 分钟以内，其中运行约 8 分钟；超过时先把端到端基准和 `app/idle_empty` 的启动次数降到 3 次和 1 次，不减少 ADR 规定的次数和时长（冷启动 20 次、静止后等 5 秒）。

## 夹具与 `test-support`

- 被测 crate 用 `test-support` feature 导出 `pub mod testing`（`#[cfg(any(test, feature = "test-support"))]`）：夹具生成器，以及公开接口之外 bench 需要的入口（如 `agents::testing::parse_file`）。`git` 的 `TestRepo`、`project` 的合成仓库形状、`kernel` 的 `TestKernel`、`executor` 的手动执行器都按这个约定放置。
- bench 只调用公开接口和 `testing`，不复制被测 crate 的内部逻辑，也不按相对路径读别的 crate 的目录；需要的样本由 `testing` 用 `include_bytes!` 嵌入或在运行时生成。
- 生成器是确定的（固定种子，不取时钟），接受规模参数，冒烟用小规模。生成的文件放在 `Ctx::scratch()` 下，每个基准、每次重试都重新生成，会修改夹具的基准（`append_4k`、`refresh_one`）不影响别的基准。
- 只有 bench 的 dev-dependencies 打开被测 crate 的 `test-support`，没有哪个 crate 的普通依赖打开它，`app` 的 `bench-probe` 也不打开。发布构建里既没有 `lectern-bench` 也没有 `test-support`（见「验收」里的 `cargo tree` 检查）。
- 大型样例项目不提交进仓库：`cloudprint` 形状（728 个文件、11 个 worktree、约 25 个清单）和 10 万文件的仓库都由 `project` 的形状描述加 `git` 的 `TestRepo` 生成。`crates/bench/tests/fixtures/` 只放本 crate 自己的小样本。

## 迁移

第 1 步把两个被忽略的 `perf_smoke` 测试移进 bench，同一个 PR 里删除原测试：

- `crates/agents/tests/perf_smoke.rs` → `benches/time/agents.rs` 的 `agents/parse_throughput`、`agents/append_4k`。拼接 50 MB 会话和取约 4 KB 完整行的代码移进 `lectern_agents::testing`（`synthetic_session(len) -> Vec<u8>`、`append_chunk() -> &'static [u8]`，fixtures 用 `include_bytes!` 嵌入）。删除 `crates/agents/Cargo.toml` 里的 `[[test]] perf_smoke`，更新 `crates/agents/AGENTS.md` 里对它的说明。
- `crates/git` 的 `perf_smoke_git_open` → `benches/time/git.rs` 的 `git/open`，用 `lectern_git::testing::TestRepo` 生成同样形状的仓库（728 个跟踪文件、11 个 worktree）；删除原测试，更新 git 设计「与设计的差异」里的对应一条。
- `crates/platform/src/safe_read/tests.rs` 的 `perf_smoke` 和 `perf_tree` → `benches/time/platform.rs` 的 `platform/safe_stat`、`platform/safe_read_4k`。bench 自己生成同样的 728 个 4 KiB 文件、深度 4 的目录树。原测试比较每个后端，`open_with` 是 crate 内部接口；bench 只测 `SafeRoot::open` 选中的生产后端（macOS 上是 `NofollowAny`，Linux 上是 `Openat2`），预算也只对生产后端成立。`Walk` 只用于少见的路径，不设基准。更新 platform 设计「性能」里的「基准」一条。
- `executor` 设计里推迟的两项、`kernel` 与 `i18n` 设计里的基准，同一步加入（只记录）。

## 性能

- 本 crate 不进入发布构建，不影响冷启动、内存和安装包。
- 测量用的构建里，探针在首帧之前只读三个环境变量、设置严格的 archive 模式，并通过 `platform` 注册一个首帧回调，不读文件、不建线程、不连 socket；socket 和应答线程都在首帧显示之后才建立。`bench-probe` 插件在所有内存基准里都存在（插件的两次启动里也都存在），差值不受它影响。

## 状态与异常

- 夹具生成失败、基准返回错误或 panic：记为 `error`，退出码 2，在任何模式下都如此。
- 子进程 10 秒内没有连上 socket、在 `FirstFrame` 之前退出、10 秒内没有响应 `Quit`：结束子进程，记为 `error`。
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
- 合并：`report_merge_*` 验证三份报告合并后每条预算恰好一次；带 `*` 的条目按插件列表展开，列表为空时条目本身出现一次，不为空时某个插件缺少展开项即出错；缺一份报告、某条预算缺失、重复或出现在错误的目标里时退出码 2；合并后的退出码取超标与出错的最严重者；被过滤的条目记为 `filtered` 不算缺失。
- 计数分配器：`alloc_*` 分配并保留 1 MiB 后 `live` 增加 1 MiB，释放后回到原值，`peak` 记录峰值（在单独的测试二进制里注册分配器）。
- 报告协议：`wire_*` 对每种消息编码后解码相等，含带空格、`%`、换行和非 UTF-8 字节的路径；缺字段、未知消息名、坏的百分号编码返回错误。
- 进程调度：`process_*` 用脚本化的 `Launcher` / `ProcessInfo`：冷启动 2 次预热不计入、20 次样本；`Busy` 让 5 秒等待重新开始；采样前后代数不同时重采；30 秒不静止记为 `over`；不连 socket、提前退出、不响应 `Quit` 记为出错；插件两次启动的差值与激活耗时的取法（进程插件的进程组计入差值）；激活条件为 `[Workspace(..), AfterFirstFrame]` 的插件计入 `plugin/after_first_frame`；预热启动报告 `archive-miss` 或以 3 退出时记为出错；`time_with_setup` 每个样本调用一次 `setup`；进程组展开到孙进程、中途退出的后代被跳过、同一 pid 不重复计入、`plugin/process_total` 是全部进程插件组之和；帧间隔取 `started` 与 `presented` 两个相邻差序列的最大值，主线程阻塞造成的单个长间隔使结果超标；内存类并发不超过 3、计时类不并发。
- `purge`：集成测试把一个假的 `sudo` 放在 PATH 最前面，记录参数，按夹具回放成功与失败（AGENTS.md「工作方式」）。
- 探针：`probe_*` 在 Linux 上用真实的 `Kernel` 和测试插件验证 `select` 的两种裁剪、`attach` 满足 `Command` / `Manual` / `Workspace` 三种激活条件后发出 `Ready`、`bench-probe` 插件丢弃过时的 `Busy` 且不让 kernel 重新变忙。首帧回调只能在 macOS 上验证，由第 4 步的基线运行覆盖。

只能在 macOS 上运行的：全部判定、进程级基准、`empty_window`。

## 未决问题

1. **判定用哪台机器。** 预算定义在 M 系列 Mac 上，GitHub 的 `macos-15` runner 是 3 核的 Apple M1 虚拟机，与用户的实体机不同，也有邻居干扰。
   - 选项 A（默认）：用 GitHub 托管的 `macos-15` 判定，不额外花钱。
   - 选项 B：增加一台自托管的实体 M 系列 Mac 作为判定机器，托管 runner 只做冒烟。要付硬件和维护成本，并且要隔离在它上面运行的 PR 代码。
   - 第 4 步测出空 GPUI 窗口在托管 runner 上的基线后，如果冷启动在实体机上低于 20 ms、在托管 runner 上不低于 20 ms，需要产品负责人在 A、B 之间选择；按 ADR 0017，不能为此放宽预算。

## 验收

分五步，每步一个 PR。前四步写 `Refs #7`，第 5 步写 `Closes #7`。之后各 crate 的基准由实现它们的 PR 加入（见「预算表」），[#38](https://github.com/SoloJiang/lectern-app/issues/38) 另行关闭。

1. **运行器、预算表与迁移**（`bench`，同时删除 `agents`、`platform` 里的 `perf_smoke`）
   - `just test -p lectern-bench` 里 `stats_*`、`budgets_*`、`runner_*`、`report_*`、`report_merge_*`、`alloc_*` 通过。
   - `just bench` 之后 `target/lectern-bench/` 下有 `report-time.json`、`report-memory.json`、`report-process.json` 和合并后的 `report.json`；删掉 `budgets.rs` 里任意一条 `Active` 预算对应的基准时，`just bench` 退出码 2。
   - Linux 上 `just bench` 退出码 0：`time` 与 `memory` 目标的生效条目状态都是 `smoke`，`process` 目标的生效条目都是 `skipped`（只在 macOS 运行），其余是 `planned`；`just bench agents/` 只运行 `agents/*`，其余条目为 `filtered`。
   - macOS CI 的 `just bench` 判定 `agents/parse_throughput`、`agents/append_4k`、`platform/safe_stat`、`platform/safe_read_4k`，记录 `executor/*`、`kernel/*`、`i18n/*`，上传 `report.json`。
   - Linux CI 任务有 `just bench` 一步；`just ci` 在 Linux 上运行它。
   - `rg -l perf_smoke crates` 没有结果；`rg -n 'perf_smoke' crates/agents/Cargo.toml` 没有结果。
   - `rg -n '^name = "(criterion|divan)' Cargo.lock` 没有结果；`just deny` 通过。
   - `python3 scripts/check_architecture.py` 通过；`python3 scripts/check_architecture.py --gpui-free-packages` 的输出包含 `lectern-bench`。
2. **进程与首帧接口**（`platform`，先在 platform 设计里补充）
   - macOS 上 `just test -p lectern-platform` 的 `process_*` 通过：本进程的 `start_time` 早于测试开始且相差不超过进程已运行的时间；分配并写满 64 MiB 后 `phys_footprint` 至少增加 60 MiB；`children` 列出刚启动的子进程。`host_*` 给出非空的机型与系统版本。
   - Linux 上这些函数返回 `ErrorKind::Unsupported`，有测试覆盖。
3. **插件集合的闭包与裁剪**（`kernel`，同一 PR 里更新 kernel 设计）
   - `just test -p lectern-kernel` 的 `set_closure_*` 通过：多层依赖、`MULTIPLE` 服务的多个提供者、缺提供者时返回 `GraphError`；`retain` 之后 `validate` 通过。
4. **探针、进程调度与空窗口基线**（`bench`）
   - `just test -p lectern-bench` 的 `wire_*`、`process_*`、`probe_*` 和 `purge` 集成测试通过。
   - macOS CI 上 `just bench baseline/` 报告 `baseline/empty_window_cold_start` 的 20 个样本和 `baseline/empty_window_idle`。
   - 实测值和机器信息写进 ADR 0017（ADR 0017「影响」要求的基线）；不满足 `app/cold_start` 的判定规则（20 次里超过 20 ms 的多于 1 次，或有一次超过 30 ms）时告诉产品负责人，并按「未决问题」处理。
5. **`app` 的测量构建与应用级基准**（`app`，`Closes #7`；先合并 `app` 的设计文档；前提是 `app` 已按 ADR 0017 内嵌 binary archive，`platform::metal::set_strict_archive` 已实现）
   - `cargo build --release --locked -p lectern-app --features bench-probe` 成功；`cargo tree --locked -p lectern-app -e normal,build,features --target aarch64-apple-darwin` 的输出里没有 `lectern-bench`，也没有 `"test-support"`。
   - macOS CI 的 `just bench` 判定 `app/cold_start`、`app/idle_empty`、`plugin/*/activate`、`plugin/after_first_frame`，记录 `app/cold_start_purged` 和 `plugin/*/resident`。
   - 临时从 archive 里去掉一个 slice 时，`just bench app/cold_start` 在第一次预热启动就记为 `error`，退出码 2。
   - 本文状态行改为「已实现」并列出五个 PR，填写「实际验证」。
