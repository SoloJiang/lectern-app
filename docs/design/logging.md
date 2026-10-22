# logging 设计

状态：实现中。第 1 步（`logging` crate、`clippy.toml`、架构检查、`log/` 基准，[#PR](https://github.com/SoloJiang/lectern-app/pull/PR)）已实现；第 2 步（`platform` 的 `dirs::logs`、`system::os_version`）已实现（[#173](https://github.com/SoloJiang/lectern-app/pull/173)，见 [platform 设计「日志目录与系统版本」](platform.md#日志目录与系统版本)）；其余步骤尚未实现（见「验收」）。

对应 issue [#170](https://github.com/SoloJiang/lectern-app/issues/170)。需求来自用户 2026-10-22 的要求：运行日志可追溯，用户可以主动开启更详细的日志落盘。决策和关键约束见 [ADR 0046](../adr/0046.md)，本文写接口、记录格式、缓冲、滚动、过滤、隐私处理和测试。`logging` 在 kernel 层，任何 crate 都可以依赖它（[工作区与 crate 划分](cargo-workspace.md)）。

## 范围

- 做：
  - 调用点：`error!`、`warn!`、`info!`、`debug!`、`trace!` 宏，类型化字段 `Field` / `Value`，线程本地的关联作用域 `Ctx` / `Scope`。
  - 落盘（`sink` feature）：实现 `log::Log` 的日志器、首帧前的内存缓冲、运行时的双缓冲、写盘线程 `lectern-log`、文件滚动与清理、过滤与详细日志、panic 钩子、正常退出时的收尾、导出。
  - 第三方记录（GPUI、gix 等经 `log` 写出的）和 panic 消息的文本处理。
  - 测试辅助（`test-support` feature）：其他 crate 在测试里捕获记录、跑金丝雀测试。
- 不做：
  - 日志目录、崩溃报告目录、系统版本：`platform`（架构规则 6），由 `app` 传入。
  - 把导出的目录压缩成 zip：`platform`（用 macOS 自带的 `ditto`），随导出界面那一步实现。
  - 设置页和 ⌘K 里的开关与导出入口：视图层，见「未决问题」1。
  - 各 crate 记什么：各自设计文档的「日志与诊断」一节。
  - 上传日志、遥测：日志只留在本机，由用户主动导出。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `log` 0.4（`default-features = false`，`features = ["std", "kv"]`） | 门面：全局级别、`Log` trait、记录的键值 | 已在发布构建里（GPUI 依赖它），这两个 feature 没有传递依赖，MIT OR Apache-2.0；比较见 [ADR 0046](../adr/0046.md)「门面与 crate」 |
| `trybuild`（dev） | 编译失败测试：字符串字段、非字面量消息 | 工作区已有 |
| `insta`（dev） | 编码结果、过滤解析的快照 | 工作区已有 |
| `serde_json`（dev） | 测试里校验每一行都是合法的 JSON | 工作区已有，只在测试里用 |
| `tempfile`（dev） | 滚动、清理、导出的测试目录 | 工作区已有 |

只依赖 `log`：本 crate 在 kernel 层，只能依赖 kernel 层，`platform`、`executor` 都在它之上。JSON 编码、UTC 时间格式化手写（一条记录只用到字符串转义、整数和日期换算），不引入 `serde`、`chrono`。

`log` 在 `[workspace.dependencies]` 里写一次，只有本 crate 直接依赖它；其他 crate 经本 crate 的宏间接使用。

## 接口

### 调用点

```rust
// 消息必须是字符串字面量；字段写成 `名字 = 值`，值的类型实现 `Field`。
lectern_logging::warn!("partition disabled"; reason = Reason::Corrupt, path = Value::path(&file), sqlite = code);
lectern_logging::info!("plugin activated"; took_us = elapsed);
lectern_logging::debug!("index batch"; files = n, bytes = total);
```

- 五个宏 `error!`、`warn!`、`info!`、`debug!`、`trace!`，任意线程调用，不阻塞，不做 IO。
- target 固定为调用所在 crate 的短名：取 `module_path!()` 的第一段，去掉 `lectern_` 前缀。不提供自定义 target，按插件过滤用 `plugin` 字段。
- 字段名是标识符，最多 12 个。保留名（`t`、`up`、`lv`、`tg`、`th`、`at`、`msg`、`n`、`trunc`，以及 `Ctx` 的字段名）在宏里逐个匹配，写了就编译失败。
- 每个调用点展开出一个 `static Callsite`：缓存按 target 过滤的结果（带过滤的代数，过滤变化后重新计算）和限流计数。第三方记录没有这个静态变量，限流见「数据结构」的「限流」。

```rust
/// 字段值。实现它的类型只能经 `Value` 的构造函数产出值，构造函数不接受运行时字符串。
pub trait Field {
    fn value(&self) -> Value<'_>;
}

/// 已实现：bool、全部整数类型、f64、Duration（写成微秒整数）、&'static str、io::Error（种类与系统错误码）、
/// Value 本身、Option<T: Field>（None 写成 null）。
/// 各 crate 为自己的枚举、ID 类型实现 Field，在 value() 里用下面的构造函数。
#[derive(Clone, Copy)]
pub struct Value<'a> { /* 私有枚举 */ }

impl<'a> Value<'a> {
    pub const fn text(s: &'static str) -> Value<'static>;
    /// 写出时主目录前缀换成 `~`，超过 1024 字节截断；非 UTF-8 的字节写成 U+FFFD。
    pub fn path(p: &'a Path) -> Value<'a>;
    /// 小写十六进制，最多 32 字节（SHA-1 和 SHA-256 的 OID）。
    pub fn oid(bytes: &'a [u8]) -> Value<'a>;
    /// 8-4-4-4-12 的小写形式。会话 ID 不是 UUID 时，调用方传它的 128 位哈希（`Ctx::session` 同样）。
    pub const fn uuid(bytes: [u8; 16]) -> Value<'static>;
    pub const fn hex(v: u64) -> Value<'static>;
    /// 只写 `scheme://host[:port]/path`，去掉 userinfo、查询串和片段；解析不了时写 `"<url>"`。
    pub fn url(s: &'a str) -> Value<'a>;
    /// `io::Error` 的种类和系统错误码，写成 `{"kind":"NotFound","code":2}`；不写错误消息。
    pub fn io(e: &io::Error) -> Value<'static>;
    /// panic 消息，写出时按「隐私」的 panic 消息规则处理。
    /// 只有 kernel、executor 和本 crate 调用，由架构检查限制调用位置。
    pub fn panic_message(text: PanicText<'a>) -> Value<'a>;
}

/// panic payload 的类型决定处理规则：调用方从 payload 取消息时按类型构造。
#[derive(Clone, Copy)]
pub enum PanicText<'a> {
    /// payload 是 `&'static str`：`panic!("字面量")`、`Option::unwrap` 等，消息是代码里的文字。
    Static(&'static str),
    /// payload 是 `String`：格式化出的消息，可能含运行时的数据。
    Formatted(&'a str),
    /// 其他类型（`panic_any`）：只记类型未知。
    Other,
}
```

### 作用域与关联 ID

```rust
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Ctx {
    pub plugin: Option<PluginTag>,
    /// kernel 给每次激活的序号，进程内从 1 递增。
    pub act: Option<u64>,
    /// executor 给每个任务的序号，进程内从 1 递增。
    pub task: Option<u64>,
    /// 会话 UUID（`agents` 解析出的 Claude Code / Codex 会话 ID）。
    pub session: Option<[u8; 16]>,
    /// 审阅对象的 64 位哈希，由 `review` 定义。
    pub review: Option<u64>,
    /// 一次对外请求（forge、Analyze 的转发、进程插件的 JSON-RPC）的序号。
    pub req: Option<u64>,
    /// 一次 Analyze 的序号。
    pub analysis: Option<u64>,
}

/// 插件的稳定 ID。内置插件用 `new`；进程插件的名字由 kernel 在加载时用 `register` 登记一次，
/// 名字留在进程内，最多 1024 个，超出时返回 None，记录里不带 plugin。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginTag(&'static str);
impl PluginTag {
    pub const fn new(id: &'static str) -> PluginTag;
    /// 只接受 `[a-z0-9_-]`、1 到 64 字节的 ID（`@plugin=level` 能写出的范围），其他返回 None。
    pub fn register(id: &str) -> Option<PluginTag>;
}

/// 当前线程的作用域。没有设置时全为 None。
pub fn current() -> Ctx;

/// 在当前作用域上覆盖 `ctx` 里非 None 的字段，guard drop 时恢复原值。
/// `!Send`，按嵌套顺序 drop（调试构建里 `debug_assert` 检查）。
/// 不得跨 `.await` 持有：clippy 的 `await_holding_invalid_type` 配置了这个类型；
/// 任务里的作用域由 executor 在每次 poll 外设置。
#[must_use]
pub fn scope(ctx: Ctx) -> Scope;
pub struct Scope { /* 上一个 Ctx */ }
```

作用域是一个线程本地的 `Cell<Ctx>`，`scope` 和 drop 各是一次拷贝，不分配内存。只有写出的记录才读它。

### 落盘（`sink` feature）

```rust
pub struct EarlyConfig {
    /// `platform::dirs::logs()`：只读环境变量，首帧前调用不做 IO。None 时只留内存缓冲。
    pub dir: Option<PathBuf>,
    /// 命令行的 `--log <过滤>`，优先于 `LECTERN_LOG`。
    pub cli_filter: Option<String>,
}

/// `app` 在 `main` 的第一行调用，首帧之前、主线程上，只调用一次（再次调用返回 Err）。
/// 只做内存操作：`log::set_logger`、设全局级别、读 `HOME` 和 `LECTERN_LOG`、解析过滤、
/// 分配 64 KiB 的首帧前缓冲、装 panic 钩子。不建线程，不打开文件。
pub fn install_early(config: EarlyConfig) -> Result<Early, AlreadyInstalled>;

pub struct Early { /* … */ }

pub struct Header {
    pub version: &'static str,
    pub build: &'static str,
    /// `platform::system::os_version()` 读出的系统版本，如 "macOS 15.1"。
    pub os: String,
    pub arch: &'static str,
}

pub struct Config {
    pub header: Header,
    /// 在写盘线程上调用一次，`app` 用它经 `platform` 把线程设为 utility QoS。
    pub on_writer_start: Option<Box<dyn FnOnce() + Send>>,
}

impl Early {
    /// 启动屏障放开后由 `app` 调用（首帧显示之后）。建 `lectern-log` 线程后立即返回，不阻塞；
    /// 建目录、清理、读状态文件、打开文件都在那个线程上做。线程建不起来时返回的 `Logging` 只有内存缓冲。
    pub fn start(self, config: Config) -> Logging;
}

/// `Clone + Send + Sync`。`app` 持有一份，交给设置页和导出命令。
#[derive(Clone)]
pub struct Logging { /* Arc<Shared> */ }

impl Logging {
    /// 设置里开关详细日志。任意线程调用，不阻塞：过滤立即生效，状态文件由写盘线程写。
    /// `lasts` 默认 24 小时，最多 7 天（见「未决问题」2）。
    pub fn set_verbose(&self, verbose: Option<Verbose>);

    /// 当前状态的快照，任意线程调用，不阻塞。
    pub fn status(&self) -> Status;

    /// 导出到 `parent` 下新建的目录。阻塞（复制文件），在 executor 的 `Background` 任务里调用。
    /// 先让写盘线程写完缓冲里已有的记录（最多等 1 s），再复制。
    pub fn export(&self, parent: &Path, options: &ExportOptions) -> Result<Exported, ExportError>;

    /// 正常退出时 `app` 在 kernel 和 executor 关闭之后调用一次：写完缓冲、写结束记录、
    /// `fsync`、关闭文件、join 写盘线程，最多等 `timeout`（`app` 用 500 ms）。之后的记录只计数。
    pub fn shutdown(&self, timeout: Duration) -> Flushed;
}

pub struct Verbose { pub filter: Filter, pub lasts: Duration }

/// 过滤，见「数据结构」。`Display` 写回规范形式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter { /* … */ }
impl Filter {
    /// 无法识别的指令跳过，返回它们的下标。
    pub fn parse(s: &str) -> (Filter, Vec<usize>);
}

pub struct Status {
    pub filter: Filter,
    pub source: FilterSource,
    pub verbose_until: Option<SystemTime>,
    pub dir: Option<PathBuf>,
    /// 缓冲满丢弃的记录数、限流省略的记录数，进程内累计。
    pub dropped: u64,
    pub suppressed: u64,
    /// 默认文件与详细文件当前占用的字节数，写盘线程在每次滚动和清理后更新。
    pub disk_bytes: u64,
}

pub enum FilterSource { Default, Env, Cli, Setting }

pub struct ExportOptions {
    /// 最近几次运行，默认 3。
    pub runs: usize,
    /// 是否附上这几次运行的详细文件，默认是。
    pub include_verbose: bool,
    /// 额外复制的文件（崩溃报告），由 `app` 经 `platform` 列出，见「未决问题」3。
    pub extra: Vec<PathBuf>,
}

pub struct Exported { pub dir: PathBuf, pub files: usize, pub bytes: u64 }
pub enum ExportError { NoDirectory, Io(io::ErrorKind) }
pub enum Flushed { Clean, TimedOut { pending_bytes: u64 } }
pub struct AlreadyInstalled;
```

### 测试辅助（`test-support` feature）

```rust
pub mod testing {
    /// 在本进程装一个捕获日志器，过滤为 trace。nextest 每个测试一个进程，所以每个测试各装一次；
    /// 同一进程里第二次调用返回同一个捕获器。记录按写盘时的编码保存，用法与真实文件相同。
    pub fn capture() -> &'static Capture;
    pub struct Capture { /* … */ }
    impl Capture {
        pub fn lines(&self) -> Vec<String>;
        pub fn clear(&self);
        /// 金丝雀检查：任何一行包含 `needle` 时 panic，并打印那一行。
        pub fn assert_absent(&self, needle: &str);
    }

    // 以下供 lectern-bench 使用。
    pub use log::Level;
    /// 经 `log::logger()` 写一条第三方记录，与 GPUI 直接调用 `log` 时相同。
    pub fn third_party(level: Level, target: &str, file: Option<&'static str>, line: Option<u32>, args: fmt::Arguments<'_>);
    /// Lectern 调用点每个窗口的条数上限（平时 20）。基准调高它，在同一个调用点上反复测写出的路径。
    pub fn set_callsite_limit(per_window: u32);
    /// `sink` 同时开启时：让写盘线程写完缓冲并等它，最多 `timeout`。
    pub fn flush(logging: &Logging, timeout: Duration) -> bool;
    /// `sink` 同时开启时：再做一遍 `install_early` 的工作（读环境变量、解析过滤、分配首帧前缓冲和限流表、
    /// 设全局级别、`set_logger`（已装过，返回失败）、panic 钩子取出再放回），不替换已装的日志器。`log/install_early` 用它。
    pub fn install_early_again(config: EarlyConfig) -> EarlyAgain;
}
```

本 crate 自己的测试另有 `ManualClock`（滚动、清理、到期）和记录线程的文件操作钩子（「测试」里「UI 线程上没有 IO」）。

## 数据结构

### 记录

一行一个 JSON 对象，以 `\n` 结尾，每行不超过 4 KiB。键的顺序固定：

```json
{"t":"2026-10-22T03:27:01.123Z","up":1834211,"lv":"warn","tg":"cache","th":"lectern-worker-2","at":"crates/cache/src/store.rs:212","plugin":"review","act":12,"task":3401,"msg":"partition disabled","reason":"corrupt","path":"~/Library/Caches/Lectern/repo/ab12cd.db","sqlite":11}
```

| 键 | 含义 |
|---|---|
| `t` | UTC 时间，毫秒，RFC 3339 |
| `up` | `install_early` 之后的微秒数（单调时钟），排序和对照启动耗时用 |
| `lv` | `error`、`warn`、`info`、`debug`、`trace` |
| `tg` | target：Lectern crate 的短名，第三方 crate 取模块路径的第一段（`gpui`、`gix_odb`） |
| `th` | 线程名；没有名字的线程写 `#<n>`，n 是本 crate 给线程的序号 |
| `at` | `file!():line!()`；第三方记录有位置时同样写出 |
| `plugin` … `analysis` | 当前作用域里非空的关联 ID，`session` 写成 UUID，`review` 写成 16 位十六进制 |
| `msg` | 消息：Lectern 的记录是字面量；第三方记录是处理过的文本 |
| 其余 | 调用点的字段，按书写顺序 |
| `n` | 限流省略的条数，只在有省略时出现 |
| `trunc` | 超过 4 KiB 被截断时为 true：从最后一个字段开始丢，固定字段和 `msg` 保留 |

字符串按 JSON 转义，控制字符写成 `\u00XX`。读取方忽略不认识的键。

每个文件的第一行是文件头，最后一行（正常退出时）是结束记录：

```json
{"t":"…","up":0,"lv":"info","tg":"logging","msg":"file","format":1,"run":"9f2c4e0a7b31d855","seq":0,"version":"0.1.0","build":"4795932","os":"macOS 15.1","arch":"arm64","pid":4821,"filter":"info","verbose":false}
{"t":"…","up":…,"lv":"info","tg":"logging","msg":"run end","run":"9f2c4e0a7b31d855","dropped":0,"suppressed":3}
```

`run` 是每次运行的 64 位随机数（`std::collections::hash_map::RandomState` 取种子，不读 `/dev/urandom`），`seq` 是同一次运行里文件的序号。格式改变时 `format` 加一。

### 文件

```
~/Library/Logs/Lectern/                       # 0700
  lectern-20261022T032701Z-4821.log           # 一次运行的第一个文件，0600
  lectern-20261022T032701Z-4821.1.log         # 同一次运行滚动出的下一个文件
  verbose/lectern-20261022T032701Z-4821.log   # 详细文件，详细日志开启期间才有
  verbose.json                                # 设置里开启的详细日志：{"filter":"debug","until":"2026-10-23T03:27:01Z"}
```

| | 默认文件 | 详细文件 |
|---|---|---|
| 收哪些记录 | 过滤放行的 info 及以上 | 过滤放行的全部记录 |
| 单个文件滚动 | 4 MiB | 16 MiB |
| 合计上限 | 20 MiB 且 50 个 | 200 MiB |
| 保留 | 14 天 | 3 天 |

- 文件名里的时间是这次运行开始时的 UTC 时间，加进程号。滚动时关闭当前文件，新开 `.<seq>.log`，写一行文件头。
- 清理在 `start` 后的写盘线程上做一次，之后每次滚动做一次：按文件名里的时间从旧到新删，直到满足上表。只有正在写的那个文件（本次运行这一类文件里序号最大的）不删；本次运行滚动出的较早的文件和其他运行的文件一样计入上限、照样删除，所以一次运行开着一周、详细日志开着、或者几个调用点按上限刷屏时，上表的上限仍然成立。本次运行的文件名里都是运行开始的时间，按保留期限判断它们时改用文件的最后修改时间。只处理符合命名规则的文件，目录里的其他文件不动。删除失败记一条 warn，不重试。
- `start` 时读上一次运行的最后一个文件的末尾 4 KiB：没有结束记录时，在新文件里记 `previous run ended unexpectedly`，带上那次的 `run`。
- `verbose.json` 只由写盘线程读写：`start` 时读，`set_verbose` 时写（写临时文件后改名）。已经过期、坏了或认不出的当作没有，删除。

### 过滤

```
filter    := directive ("," directive)*
directive := level | target "=" level | "@" plugin "=" level
level     := off | error | warn | info | debug | trace
```

- 单独的 `level` 只作用于 Lectern 的 target；第三方 target 要写出名字（`gpui=debug`），没写时保持 warn。
- `target=level` 按 crate 短名精确匹配；`@plugin=level` 匹配作用域里 `plugin` 等于它的记录（插件 ID 为 `[a-z0-9_-]`，与 `PluginTag::register` 接受的相同），与 target 规则同时命中时取较详细的一个。
- 默认过滤：Lectern 的 target 为 info，第三方为 warn。
- 来源的优先级：`--log` > `LECTERN_LOG` > 设置（`verbose.json`）> 默认。命令行或环境变量给出时，设置里的开关在本次运行中不改变过滤，`Status::source` 反映实际来源。
- `log::max_level()` 取全部指令里最详细的级别；更细的判断在调用点的缓存和 `Log::enabled` 里。过滤变化时代数加一，各调用点下次调用时重新计算。

### 缓冲

- 首帧前：`install_early` 分配一块 64 KiB 的缓冲（一次分配，页面在写入时才驻留）。记录按「4 字节长度 + 编码后的行」追加，放不下时丢弃新记录并计数。
- 运行时：两块缓冲，各 128 KiB（详细日志开启时各 512 KiB）。调用线程在锁内把编码好的行复制进前一块，锁只覆盖这次复制（单条不超过 4 KiB）；写盘线程在锁内交换两块（常数时间），在锁外写后一块。前一块放不下时丢弃新记录并计数，调用线程不等写盘线程。
- 编码在调用线程上进行，用线程本地、复用的 4 KiB 缓冲，不分配内存。
- 丢弃和限流的计数由写盘线程在下一次写入时记成一条 warn（`records dropped`，带 `dropped`、`suppressed`）。

### 限流

- 规则：同一调用点的 error、warn、info 以 10 秒为一个窗口，每个窗口最多写 20 条；超出的不编码，只计数，这个调用点下一条写出的记录带 `n`（省略的条数）。debug、trace 不按调用点限流，只受缓冲容量限制。
- Lectern 的调用点：计数在宏展开的 `static Callsite` 里，一个 `AtomicU64` 存窗口编号和条数。
- 第三方记录：`Log::log` 在编码之前按（target、`file()`、`line()`）的哈希查一张 256 项的固定表，每项存键的哈希和同样的计数，线性探测最多 8 项。哈希取 target 和 `file()` 两个字符串的地址与长度，加上行号：调用点的这两个值是字符串字面量，不必逐字节哈希。没有文件和行的记录按 target 计。探测不到空位时，这条记录计入一个共用的溢出项，溢出项按同样的规则限流。表在 `install_early` 时一次分配（约 6 KiB），之后不增长。每项另外缓存过滤给它的 target 的级别，带过滤的代数（与 Lectern 调用点的缓存相同，溢出项不缓存），所以被限流的调用不再取 target 的第一段、不再查过滤。被省略的条数先记在表项里，这个调用点下一条写出时才并进进程的合计；`Status::suppressed`、`records dropped` 和结束记录用的合计再加上表里还没并进去的条数。
- 第三方合计：warn、info 的第三方记录每个窗口另有合计上限 50 条，超出的同样只计数，所以几个调用点同时刷屏也只占默认文件很小的一部分。error 级别不受合计上限限制，只受各自调用点每窗口 20 条的限制，刷屏的 warn 不会挤掉 error。
- 被限流的调用在调用线程上的开销是一次哈希和两三次原子操作，不编码、不复制（`log/third_party_limited`）。判断窗口不每次读单调时钟（Linux 虚拟机上一次约 20–40 ns）：每个线程记下上一次读时钟时的窗口和 CPU 计数器（x86_64 的 TSC、aarch64 的 `cntvct_el0`），计数器走过 2^20 之前沿用这个窗口（3 GHz 下 0.3 ms，Apple 芯片的 24 MHz 下 44 ms），之后再读时钟。窗口的切换在一个线程上最多晚这么久被看到；其他架构每次读时钟。Lectern 调用点的限流用同一个窗口。

## 数据流与线程

1. 首帧前，主线程：`install_early`。之后任何线程的记录都进首帧前缓冲。
2. 调用：调用点的级别低于 `log::max_level()` 时返回；否则查过滤（Lectern 调用点的缓存，或第三方记录的 `Log::enabled`）和限流（调用点的计数，或第三方的固定表）；放行的在调用线程上编码，复制进缓冲；写盘线程在睡眠时 `unpark` 它。调用线程不做 IO，UI 线程上的记录同样如此。
3. 启动屏障放开后，主线程：`Early::start` 建 `lectern-log` 线程后返回。写盘线程依次：调用 `on_writer_start`，建目录，清理，读 `verbose.json`，检查上一次运行，打开文件写文件头，写出首帧前缓冲的内容，然后让调用线程改用运行时缓冲。
4. 写盘线程的循环：有 error 记录或 panic 钩子在等时立即写；其余记录到达后最多等 1 s 或攒到运行时缓冲容量的一半（默认 64 KiB，详细日志开启时 256 KiB）再写；只有让缓冲第一次变为需要立即写的那条记录唤醒等待中的写盘线程。写盘线程从缓冲锁下同步写入的原子标志读「有没有记录」「要不要立即写」，只在交换两块缓冲时取这把锁；写完没有新记录就无限期 `park`。唯一的定时等待是设置开启的详细日志到期，到期时恢复原来的过滤、删除 `verbose.json`、记一条 info。写只到 `write(2)` 为止，不 `fsync`；`shutdown` 和 panic 钩子之后才 `fsync`。
5. panic 钩子（在 panic 的线程上）：编码一条 error 记录（`msg` 为 `panic`，字段 `message` 用 `Value::panic_message`，按 `payload()` 能否转成 `&'static str` 或 `String` 选 `PanicText` 的变体，带作用域和 `location()`；`RUST_BACKTRACE` 设置了或详细日志开启时附回溯的帧名），放进缓冲，等写盘线程写完这条，最多 100 ms，然后调用原来的钩子。写盘线程还没启动时，钩子在当前线程上打开本次运行的文件，写出首帧前缓冲和这条记录。
6. 退出：`app` 依次关闭 kernel、executor，再调用 `Logging::shutdown`。

写盘线程不经 `executor`：executor 自己的 panic 和关闭过程也要记下来，日志不能依赖它；这个线程不提交 kernel 的任务，不计入静止，也没有周期性唤醒（[ADR 0017](../adr/0017.md)、[ADR 0029](../adr/0029.md)）。

| 线程 | 做什么 | 阻塞 |
|---|---|---|
| 任意（含 UI 线程） | 编码、复制进缓冲 | 只等缓冲的锁（一次复制） |
| 主线程 | `install_early`、`start`、`set_verbose`、`status` | 否 |
| 主线程，退出时 | `shutdown`：等写盘线程，最多 `timeout` | 是，窗口已关闭 |
| executor 的 `Background` 任务 | `export` | 是（复制文件） |
| panic 的线程 | 钩子：等写盘线程最多 100 ms | 是 |
| `lectern-log` | 全部文件操作 | — |

## 性能

| 项 | 预算 | 基准 |
|---|---|---|
| 低于全局级别的调用 | ≤ 2 ns | `log/disabled_call` |
| 全局级别放行、按 target 拒绝 | ≤ 5 ns | `log/filtered_call` |
| 写出的调用（info，4 个字段），调用线程上 | p50 ≤ 1 µs，不分配内存 | `log/enabled_call` |
| 4 个线程同时写（核数不够时为可用核数减 1，至少 1 个，给写盘线程留一个核），调用线程上 | p99 ≤ 5 µs，不丢记录（总量小于缓冲） | `log/contended_call` |
| 被限流的第三方记录，调用线程上 | ≤ 30 ns，不编码 | `log/third_party_limited` |
| `install_early` | ≤ 20 µs | `log/install_early` |
| 写出 1 万条记录、静止 5 s 后的常驻内存增量 | 默认配置 ≤ 512 KiB | `log/idle_memory` |

- 冷启动：首帧前只有 `install_early`，占 20 ms 预算的 0.1%；不打开文件，不建线程。`app/cold_start` 不因接入日志变慢。
- 空闲内存：首帧前缓冲 64 KiB、运行时缓冲 256 KiB、写盘线程的 `BufWriter` 64 KiB 和栈，合计不超过 512 KiB；详细日志开启时缓冲为 1 MiB，不计入空闲内存预算，`log/idle_memory` 另报一个数。
- 磁盘：默认级别下一次普通使用（打开项目、审阅几个会话）预计每天不到 1 MiB，由默认文件的 20 MiB 上限兜底。
- 实测的门面开销见 [ADR 0046](../adr/0046.md)「背景」；本 crate 在门面之上多出调用点缓存、作用域和编码，预算按上表。
- 工作区不开启 `log` 的 `max_level_*`、`release_max_level_*` feature（「验收」里有检查）。

## 隐私

| 类别 | 规则 |
|---|---|
| 文件内容、diff、高亮结果、剪贴板 | 不记录。只记路径、OID、字节数、行数 |
| 会话 JSONL | 不记录行的内容。解析失败记会话 UUID、文件路径、字节偏移、行长度、错误种类 |
| 提示词、回复、⌘K 的查询、提交说明、PR 与评论的文字 | 不记录，只记长度 |
| token、凭据、API key、环境变量的值 | 不记录，也不记长度 |
| 子进程 | 记程序名（不含目录）、参数个数、工作目录路径、pid、退出状态；不记参数和环境变量 |
| 网络请求（forge、Analyze 的转发、插件市场） | 记方法、`Value::url` 处理后的 URL、状态码、请求与响应的字节数、耗时；不记请求头和请求体、响应体 |
| 路径 | 可以记录，主目录前缀写成 `~` |
| 第三方记录、panic 消息 | 自由文本，按下面三类规则处理 |

自由文本分三类，按消息的来源选规则。三类都先做两步：只取第一行；形似凭据的串换成 `<redacted>`（以 `sk-`、`ghp_`、`gho_`、`github_pat_`、`glpat-`、`xox` 开头的词，`Bearer ` 之后的词，以 `eyJ` 开头、含两个 `.` 的词，32 个以上连续的 `[A-Za-z0-9+/=_-]`）。截断都在 UTF-8 字符边界上，主目录前缀都写成 `~`。

1. **字面量**：`PanicText::Static`，以及第三方记录里 `fmt::Arguments::as_str()` 为 Some 的消息。它们是编译进二进制的文字，不含运行时数据。规则：截到 256 字节。
2. **格式化的 panic 消息**：`PanicText::Formatted`。`panic!("{x}")`、`expect(&format!(..))`、`assert!(c, "{s}")`、`Result::expect`（消息是 `{msg}: {err:?}`）以及 std 的运行时检查（字符串切片越界的消息形如 ``byte index 5 is not a char boundary; it is inside 'é' (bytes 4..6) of `…` ``，原文在反引号里）都属于这一类，其中可能含文件、会话或提示词的文字。规则：
   - 只写 `len`（原消息的字节数）和源码位置，位置来自 `PanicHookInfo::location()`，`contain` 的记录由 kernel 另带阶段。
   - 消息以固定表里的 std 前缀开头时，写出这个前缀。表：``called `Result::unwrap()` on an `Err` value``、``called `Result::unwrap_err()` on an `Ok` value``、`index out of bounds`、`byte index`、`start byte index`、`end byte index`、`byte range starts at`、`begin <= end`、`range start index`、`range end index`、`slice index starts at`、``assertion `left == right` failed``、``assertion `left != right` failed``。其中 `start byte index`、`end byte index`、`byte range starts at` 是 Rust 1.97 的 std 对字符串切片越界写出的形式（如 `end byte index 4 is not a char boundary; it is inside 'é' (bytes 3..5 of string)`）。
   - 前缀标为「带数字」的（`index out of bounds`、`byte index`、`start byte index`、`end byte index`、`byte range starts at`、`begin <= end`、`range start index`、`range end index`、`slice index starts at`）：先把双引号、单引号、反引号括住的片段整段去掉（没有配对的引号去掉到行尾），再从剩下的文字里按顺序取最多 6 个十进制整数，写成 `nums`。
   - ``called `Result::unwrap()` on an `Err` value`` 之后，去掉引号片段，保留 Debug 输出开头的标识符（到第一个 `{`、`(`、空格为止，只含 `[A-Za-z0-9_]`，最多 32 字节），写成 `variant`，如 `Os`。
   - 其余文字一律不写。不在表里的格式化消息只有位置和长度。
   - 写出的形式：`PanicText::Static` 是字符串；`Formatted` 是对象 `{"len":…,"prefix":"…","nums":[…],"variant":"…"}`，后三项只在有时出现；`Other` 是 `{"type":"unknown"}`。
3. **格式化的第三方记录**：`as_str()` 为 None 的第三方消息，以及 target 是 Lectern crate、但消息不是字面量的记录（不经本 crate 宏写出的，按这一类兜底）。规则：
   - 双引号、单引号括住的片段换成 `"…"`、`'…'`；反引号括住的片段换成 `` `…` ``，只有 `Option::…()`、`Result::…()` 形式的 std 方法名和 `None`、`Some`、`Ok`、`Err` 保留；没有配对的引号，从它到行尾换成 `…`。
   - 第一个 `: ` 之后的文字换成 `…`：插值的错误信息和取值多在这一段。
   - 截到 128 字节。

`Debug` 输出里的字符串总是带引号，所以 ``called `Result::unwrap()` on an `Err` value: Error { input: "…", line: 3 }`` 在第 3 类里只留下结构；在第 2 类里只留下前缀和 `variant = "Error"`。

执行方式：

- 类型：宏的消息只接受字面量，字段值必须实现 `Field`；`Value` 没有接受运行时字符串的构造函数。`Value::panic_message` 的调用位置由架构检查限制在 `kernel`、`executor` 和本 crate。
- lint：`clippy.toml` 用 `disallowed-macros` 禁止 `log::error`、`log::warn`、`log::info`、`log::debug`、`log::trace`、`log::log`，理由指向 ADR 0046；本 crate 的宏展开不经过这些宏，直接构造 `log::Record`。同一个文件用 `disallowed-methods` 禁止 GPUI 的错误日志辅助方法 `gpui_util::ResultExt` 的 `log_err`、`log_err_with_backtrace`、`warn_on_err`、`log_with_level`，`gpui_util::TryFutureExt` 的 `log_err`、`warn_on_err`，以及 `gpui::TaskExt` 的 `detach_and_log_err`、`detach_and_log_err_with_backtrace`：它们把错误格式化成第三方文本，绕过类型化字段。视图层的错误经 `lectern-logging` 的宏记录，`detach` 前自己处理 `Result`。各 crate 接入完成后，非 tool crate 开启 `clippy::print_stderr` 和 `clippy::print_stdout`。
- 落盘时再检查一次：target 是 Lectern crate、但消息不是字面量的记录（`fmt::Arguments::as_str()` 为 None），按第 3 类处理。
- `PanicText` 的变体由调用方按 payload 的实际类型构造（kernel 的 `contain`、executor 的 `catch` 和本 crate 的钩子），不由调用方判断内容是否安全。
- 详细日志只改变过滤和容量，处理规则不变。

## 状态与异常

| 情况 | 处理 |
|---|---|
| `dir` 为 None | 只有内存缓冲，满了就丢弃并计数；`Status::dir` 为 None，导出返回 `NoDirectory` |
| 目录建不起来、文件打不开 | 同上；第一次失败时把一条 warn 留在内存缓冲里，`Status` 可见；之后每次滚动的时机重试一次 |
| 磁盘满、写失败 | 丢弃这一批，计入 `dropped`；下一批照常尝试；连续失败不重试得更频繁 |
| `verbose.json` 坏了或过期 | 当作没有，删除 |
| 过滤里有认不出的指令 | 跳过；写盘线程启动后记一条 warn，带指令的下标 |
| 缓冲满 | 丢弃新记录，计数，下一次写入时记 `records dropped` |
| 写盘线程建不起来 | `start` 返回只有内存缓冲的 `Logging`，记一条 warn 在缓冲里 |
| 写盘线程 panic | 循环包在 `catch_unwind` 里：panic 后停用文件，之后只有内存缓冲，计数照常 |
| 时钟回拨 | 文件名和 `t` 用系统时间，排序以 `up` 为准；清理按文件名里的时间，回拨后最多多留一些旧文件 |
| `install_early` 被调用两次 | 第二次返回 `AlreadyInstalled`，什么也不改 |

## 日志与诊断

- 默认记录：文件头和结束记录（info）；`previous run ended unexpectedly`（info，带上一次的 `run`）；`records dropped`（warn，`dropped`、`suppressed`）；目录或文件不可用（warn，`Value::io`）；过滤里认不出的指令（warn，下标）；详细日志开启与关闭（info，来源、截止时间）；清理删除的文件数和字节数（info）。
- 详细级别：滚动的时机和文件序号（debug）。
- 关联 ID：本 crate 不设作用域。
- 不记录：过滤字符串只写进文件头（由本 crate 写，字符集限于 `[a-z0-9_=,@:-]`，其余字符替换成 `?`）；`verbose.json` 的内容不另记。

## 测试

单元测试在 `src/tests/`，用 `ManualClock` 和 `tempfile` 的目录，不靠 sleep：

- 编码：一组记录的编码结果用 insta 快照；每一行都能被 `serde_json` 解析；`trunc` 只在超过 4 KiB 时出现，固定字段和 `msg` 保留。
- 隐私：
  - `crates/logging/tests/ui/` 的 trybuild 编译失败测试：`string_field_does_not_compile`（`String` 和非 `'static` 的 `&str` 字段）、`format_message_does_not_compile`（`format!` 和带参数的消息）、`reserved_key_does_not_compile`。
  - 字面量：`static_panic_text_is_kept_up_to_256_bytes`、`free_text_keeps_the_first_line`、`free_text_masks_credential_shapes`、`free_text_is_cut_on_a_char_boundary`。
  - 格式化的 panic 消息：`formatted_panic_keeps_only_location_and_length`、`std_prefix_table_keeps_prefix_and_numbers`、`unwrap_err_keeps_only_the_variant`、`unpaired_quote_drops_to_the_end_of_line`。
  - 格式化的第三方记录：`third_party_masks_quoted_and_backtick_segments`、`third_party_keeps_std_method_names_in_backticks`、`third_party_masks_after_the_first_colon`、`third_party_is_cut_at_128_bytes`、`non_literal_lectern_message_is_treated_as_third_party`。
  - `url_drops_userinfo_query_and_fragment`、`home_is_written_as_tilde`。
  - `verbose_does_not_change_sanitizing`：同一组输入在默认过滤和 `trace` 下处理结果相同。
  - 金丝雀，全部经真实的钩子或 `Log::log` 和写盘线程落盘，之后目录里任何文件都不含金丝雀：
    - `canary_str_slicing_panic`：在含金丝雀的 `String` 上按非字符边界切片（`&s[..n]`，n 落在一个多字节字符中间）触发 std 的 panic，`catch_unwind` 接住；记录里有 `byte index` 前缀和数字，没有金丝雀。
    - `canary_formatted_panic`：`panic!("{canary}")`、`Err::<(), _>(canary).expect(&format!("ctx {canary}"))`、`assert!(false, "{canary}")`、`Option::<()>::None.expect(&canary)` 各一次。
    - `canary_third_party_record`：经 `log::logger()` 直接写入 target 为 `gpui` 的记录，金丝雀分别放在引号里、反引号里、第一个 `: ` 之后。
- 限流：
  - `callsite_limit_counts_suppressed`：一个 Lectern 调用点在一个窗口里写 1 万条，写出 20 条，下一个窗口的第一条带 `n`。
  - `third_party_call_site_stays_bounded`：同一个（target、文件、行）的第三方 warn 按 120 次每秒连续写 60 秒（`ManualClock`），文件里这个调用点不超过 6 个窗口各 20 条，省略的条数合计正确；同时另一个第三方调用点的记录照常写出。
  - `third_party_total_cap`：100 个不同的第三方调用点在一个窗口里各写 1 条 warn，写出 50 条。
  - `third_party_errors_bypass_the_total_cap`：合计上限已满的窗口里，另外 30 个调用点各写 1 条 error，全部写出；其中一个调用点再写 30 条 error，只写出 20 条，`n` 记省略的 10 条。
  - `third_party_table_overflow_shares_one_limit`：超过表容量的调用点共用溢出项，写出的条数仍有上限，表不增长。
  - `limited_calls_do_not_encode`：被限流的调用不调用编码器（测试钩子计数为零）。
- 缓冲：`early_buffer_overflow_drops_newest_and_counts`、`running_buffer_never_blocks_callers`（写盘线程被测试钩子卡住时，调用线程照常返回）、`drops_are_reported_on_the_next_write`。
- 滚动与清理：`rotates_at_the_file_cap_with_a_header`、`total_cap_removes_oldest_first`、`age_cap_removes_old_runs`、`file_count_cap`、`verbose_files_have_their_own_caps`、`open_file_is_never_removed`（本次运行较早的文件照样删除）、`current_run_files_age_by_their_last_write`、`rotation_applies_the_caps_within_a_run`、`unrelated_files_are_untouched`。
- 首帧前没有 IO：`install_early_touches_no_files`（目录为空，文件操作钩子记录零次）、`install_early_spawns_no_thread`、`panic_before_start_writes_the_early_buffer`。
- UI 线程上没有 IO：`file_operations_only_on_the_writer_thread`：测试构建里全部文件操作经一个钩子，记录发起的线程；从多个线程写记录、开关详细日志、滚动、导出之后，除 `export` 的调用线程外只出现 `lectern-log`（首帧前 panic 的路径由 `panic_before_start_writes_the_early_buffer` 单独测试）。
- 过滤：`filter_parse` 的快照（合法与非法的指令）；`filter_change_invalidates_callsite_cache`；`plugin_directive_matches_the_scope`；`cli_beats_env_beats_setting`。
- 详细日志：`setting_survives_restart_until_it_expires`、`expiry_restores_the_previous_filter`。
- panic：`panic_hook_records_scope_and_location`、`panic_hook_waits_at_most_100_ms`、`panic_hook_chains_the_previous_hook`。
- 退出：`shutdown_writes_the_end_record`、`missing_end_record_is_reported_next_run`。
- 导出：`export_copies_recent_runs_and_a_manifest`（清单里有格式版本、运行 ID、文件列表和计数）。

全部测试不依赖 GPUI，在 Linux CI 上运行。

## 对其他 crate 的要求

每一项是一个 PR，按下面的顺序；各 crate 接入时在自己的设计文档里补上「日志与诊断」一节，删掉「写到 stderr、由 `app` 重定向」的描述，并加一条金丝雀测试（`testing::capture()` 在 trace 级别跑一次带金丝雀的流程，`assert_absent`）。

1. `logging`：本文的第 1 步。
2. `platform`：`dirs::logs()`（规则同 `dirs::app_data`：只读环境变量，不建目录；macOS `$HOME/Library/Logs/Lectern`，其他平台 `$XDG_STATE_HOME/lectern/logs`，变量不可用时 `$HOME/.local/state/lectern/logs`）；系统版本字符串（`sysctl kern.osproductversion`）；写盘线程的 utility QoS 沿用 executor 批量线程的接口，`platform` 有这个接口之后再接入（`Config::on_writer_start` 是可选的，在此之前写盘线程用默认 QoS）。导出那一步再加 `dirs::crash_reports()` 和 zip 压缩。
3. `app`（macOS）：`main` 第一行 `install_early`，解析 `--log`；启动屏障放开后 `start`；退出时 `shutdown(500 ms)`；把 `Logging` 交给设置与导出命令。
4. `kernel`：`ActivationToken` 带激活序号（`fn id(&self) -> u64`，进程内从 1 递增）；每段 `contain` 包住的插件代码外设 `plugin`、`act` 作用域；`contain` 捕获 panic 后记 error（`plugin failed`，带阶段：`manifest`、`activate`、`commit`、`started`、`listener`、`command`、`drop`，以及 `Value::panic_message`，`PanicText` 的变体按 payload 是 `&'static str` 还是 `String` 构造）；插件激活记 info（`took_us`）；进程插件的名字用 `PluginTag::register` 登记；15 处 `util::log` 改为宏；kernel 设计「只用标准库」改为「标准库加 `lectern-logging`」。
5. `executor`：任务序号；`spawn` 时取 `logging::current()` 并加上任务序号、闸门的插件和激活，每次 poll 和最终 drop 外设这个作用域；`catch` 捕获的 panic 记 error（`PanicText` 同 kernel）；13 处 `util::log` 改为宏；executor 设计「依赖」一节已在 [#170](https://github.com/SoloJiang/lectern-app/issues/170) 的设计 PR 里加上 `lectern-logging`。`executor/` 的基准与改动前相比不变慢。
6. `cache`：分库停用（error，原因、SQLite 扩展错误码、路径）、版本或身份不符与损坏（warn）、清理时删除失败（warn）；9 处。
7. `agents`：会话解析失败（warn，会话 UUID、路径、字节偏移、行长度、错误种类，不记行的内容）；副本写失败退回慢路径（warn）；命令行的启动与退出（程序名、参数个数、pid、退出状态）；Analyze 的凭据转发端点按「隐私」的网络请求规则记录，带 `analysis`、`req`。金丝雀放在会话 JSONL、提示词和占位令牌、真实令牌里。
8. `review`：标注日志的恢复（warn：截掉的尾部字节数、跳过的坏行数）、写失败与重试（warn）、压缩（info：前后字节数）；作用域带 `review`、`session`。金丝雀放在文件内容和会话内容里。
9. `fs_watch`（3 处）、`platform`（kqueue 线程 1 处）、`syntax`（12 处）、`code_intel`（26 处）、`i18n`（stderr 一处）：改为宏，按各自现有的「记日志」处记录。
10. CI：各 crate 接入完成后，在非 tool crate 开启 `clippy::print_stderr`、`clippy::print_stdout`。
11. `workspace` 与视图层：主线程上执行注册内容的闭包时设 `plugin`、`act` 作用域。
12. 导出界面：等「未决问题」1、3 答复后，由对应的视图 crate 和 `platform` 实现。

`git`、`project`、`forge`、`terminal`、`usage` 在各自下一次改动时补上「日志与诊断」和接入。

## 未决问题

1. **开启详细日志和导出的界面入口。** 选项：(a) 设置里新增「诊断」一节，有「详细日志（24 小时后关闭）」开关和「导出诊断信息…」按钮，⌘K 里有同名命令；(b) 只放在 ⌘K；(c) 放进菜单栏的 Help 菜单。默认 (a)。详细日志开启期间不在常驻层显示标记（[ADR 0012](../adr/0012.md) 的三层结构），只在设置里显示截止时间。
2. **设置开启的详细日志多久后自动关闭。** 选项：下次启动时关闭、24 小时、7 天、不自动关闭。默认 24 小时：能跨重启复现问题，又不会长期写大文件。`Verbose::lasts` 的上限按答复定，在此之前是 7 天。
3. **导出是否附上 macOS 的崩溃报告。** `~/Library/Logs/DiagnosticReports/` 下 Lectern 的 `.ips` 文件含线程回溯和已加载的库，不含文件内容，但有路径。选项：默认附上最近 14 天的，导出前列出文件名；默认不附，用户勾选；不支持。默认第一项。
4. **导出时是否提供隐藏路径的选项。** 把仓库路径换成哈希会让日志难以对照问题。选项：第一版不提供；提供勾选项。默认第一版不提供，导出前提示文件里有路径。

## 验收

分步实现，每步一个 PR，都要 `just check`、`just test`、`just deny` 通过。

**第 1 步：`logging` crate。**

- `Cargo.toml` 的 `layer = "kernel"`，依赖只有 `log`（`std`、`kv`）；`cargo tree -p lectern-logging -e normal --all-features` 只列出 `log`。
- 「测试」一节的全部测试通过：`just test -p lectern-logging`。
- `clippy.toml` 加上 `disallowed-macros`（`log::*` 的六个宏）、`disallowed-methods`（「隐私」列出的 GPUI 错误日志辅助方法）和 `await-holding-invalid-types`（`lectern_logging::Scope`）；`just check` 通过。macOS 上用一个调用 `log_err()` 和 `detach_and_log_err()` 的临时文件确认 clippy 报错，确认路径写对（不提交这个文件）。
- `scripts/check_architecture.py` 加两条检查及其测试：只有 `app` 和 tool 层启用 `lectern-logging` 的 `sink` feature；`Value::panic_message` 只出现在 `crates/kernel`、`crates/executor`、`crates/logging` 的源码里。
- `cargo tree --target aarch64-apple-darwin -p lectern-app -e features -i log` 里没有 `max_level_` 或 `release_max_level_` feature。
- `bench` 加入 `log/disabled_call`、`log/filtered_call`、`log/enabled_call`、`log/contended_call`、`log/third_party_limited`、`log/install_early`、`log/idle_memory`，预算写进 `budgets.rs`，在 Linux 上 `just bench --measure log/` 报告数字。

**第 2 步：`platform`。** `dirs::logs()` 和系统版本，测试 `logs_*` 覆盖与 `app_data_*` 相同的情况；`just test -p lectern-platform`。

**第 3 步：`app`（macOS）。**

- 冷启动基准 `app/cold_start` 不变慢；启动基准在 `presentedTime` 回调里记下 `Early::start` 尚未调用，首帧之后在后台检查本次运行的文件的创建时间晚于首帧显示的时间。
- 启动后 1 s 内日志目录里出现本次运行的文件，第一行是文件头；正常退出后最后一行是结束记录。
- `LECTERN_LOG=debug` 和 `--log cache=trace` 启动时文件头的 `filter` 与之相同。

**第 4 步起：** 「对其他 crate 的要求」第 4 到 11 项各一个 PR，每个 PR 的验收是：`rg 'eprintln!|util::log' crates/<crate>/src --glob '!**/tests*'` 没有结果；这个 crate 的金丝雀测试通过；涉及 panic 的（kernel、executor）有测试检查记录里的 `plugin`、`act`、`task`。

## 实际验证

第 1 步（[#PR](https://github.com/SoloJiang/lectern-app/pull/PR)），全部在 Linux（x86_64，4 核虚拟机，Rust 1.97.0）上运行：

- `just check`、`just test`（工作区 2108 个测试，基于合并了 #173、#174 的 main）、`just doctest`、`just deny` 通过。`just test -p lectern-logging` 共 64 个测试：`src/tests/` 的 62 个单元测试（「测试」一节列出的全部，另有 `names_round_trip`、`install_twice_returns_already_installed`、`turning_verbose_off_restores_the_default`、`export_without_a_directory`、`third_party_cached_level_follows_filter_changes`、`third_party_suppressed_counts_reach_the_status`、`registered_plugin_directive_survives_verbose_json`），`tests/compile_fail.rs`（trybuild，6 个编译失败用例），`tests/no_alloc.rs`（写出的调用在调用线程上不分配内存，用按线程计数的全局分配器检查）。nextest 连续运行 8 次、`cargo test` 同一进程运行一次，全部通过。
- `cargo tree -p lectern-logging -e normal --all-features` 只列出 `log v0.4.34`。`log` 0.4.34 原本就在 Cargo.lock 里（GPUI 依赖它），Cargo.lock 只多了 `lectern-logging` 这一项；`cargo metadata --locked --offline` 通过。
- `cargo tree --offline --target aarch64-apple-darwin -p lectern-app -e features -i log` 在 Linux 上离线可以运行：`log` 的 feature 是 `alloc`、`default`、`serde`、`serde_core`、`std`，没有 `max_level_` 或 `release_max_level_`。`lectern-app` 到第 3 步才依赖本 crate，那时 `kv` 会出现在这里。
- `clippy.toml`：在一个临时 crate 里确认 `log::warn!` 报 `disallowed_macros`、跨 `.await` 持有 `lectern_logging::Scope` 报 `await_holding_invalid_type`。工作区里没有直接调用 `log` 宏或 GPUI 错误日志辅助方法的代码，不需要任何 `allow`。GPUI 辅助方法的路径（`gpui_util::ResultExt::log_err`、`gpui::TaskExt::detach_and_log_err` 等）在 Linux 上解析不到（这些 crate 不在 Linux 的构建里），条目带 `allow-invalid = true`；「验收」里在 macOS 上用临时文件确认 clippy 报错的一项**没有验证**，要在 macOS 上补做。
- 架构检查的两条新规则及其测试：`python3 scripts/test_check_architecture.py`（19 个测试）、`python3 scripts/check_architecture.py`（32 个 crate）通过。
- `just bench --measure log/`（Linux 只报告、不判定）。「最初」是第一个实现提交的两次运行，「现在」是按下文几处修改之后的两次运行：

  | 基准 | 最初 | 现在 | 预算 |
  |---|---|---|---|
  | `log/disabled_call` | < 1 ns | < 1 ns | ≤ 2 ns |
  | `log/filtered_call` | 1 ns | 1 ns | ≤ 5 ns |
  | `log/enabled_call` | 602、630 ns（noisy） | 539、527 ns | ≤ 1 µs |
  | `log/contended_call` | 10 000 次里超过 5 µs 的 330、347 次（4 个线程） | 122、107 次（3 个线程；单独运行时有一次 98） | ≤ 100 次（p99 ≤ 5 µs） |
  | `log/third_party_limited` | 49、47 ns | 31、32 ns | ≤ 30 ns |
  | `log/install_early` | 4.06、3.69 µs | 4.84、4.53 µs | ≤ 20 µs |
  | `log/idle_memory` | 325 KB | 327 KB | ≤ 512 KB |
  | `log/idle_memory/verbose` | 1.13 MB | 1.13 MB | 只记录 |

  两项在这台虚拟机上仍略超预算，macOS（CI 的 macos-15 arm64，3 个虚拟核，这时是 2 个调用线程）上的判定**没有验证**：
  - `log/third_party_limited`：每次读单调时钟时是 68–73 ns（这台机器上读一次时钟约 40 ns）；改为线程本地的窗口加 CPU 计数器后是 47–49 ns；再去掉被限流的调用对进程合计的原子加、由表项缓存过滤给它的 target 的级别（「限流」）后是 31–32 ns。剩下的是构造 `log::Record`、`Log::log` 的动态调用、取 `rdtsc`、哈希和一次原子加。
  - `log/contended_call`：尾部来自三处。写盘线程在计时的窗口里每一轮要取三次缓冲锁，现在只在交换时取，其余读锁下同步写入的原子标志；每轮新建线程使第一次调用的冷启动落进样本，现在线程只建一次、先各写一条不计时的记录；调用线程加写盘线程多于核数，现在调用线程取 `min(4, 可用核数 − 1)`。剩下的主要是每轮第一条记录唤醒空闲的写盘线程（一次系统调用）和缓冲到一半容量时的那一次唤醒，每轮两次，约占 0.5%。早先试过调用线程在锁上先自旋，没有效果，已撤回。
- `log/idle_memory` 是计数分配器看到的活跃字节：运行时的两块缓冲（各 128 KiB）、默认文件的 `BufWriter`（64 KiB）、限流表和日志器的状态；不含写盘线程的栈。详细日志开启后是 1.13 MB（两块 512 KiB 的缓冲、两个 `BufWriter`）。

## 与设计的差异

第 1 步（[#PR](https://github.com/SoloJiang/lectern-app/pull/PR)）：

- **清理不再跳过本次运行的全部文件。** 原文写「当前运行的文件不删」，一次运行开得很久时 ADR 0046 的上限就都不起作用，与 ADR 冲突。现在只有正在写的那个文件不删，本次运行较早的文件按最后修改时间计保留期限（「文件」已改）。
- **缓冲到容量的一半才算需要立即写**（默认 64 KiB，原文是 32 KiB），中途唤醒写盘线程的次数减半；写盘线程读原子标志，只在交换缓冲时取锁（「数据流与线程」已改）。
- **`log/contended_call` 的线程数是 `min(4, 可用核数 − 1)`**，给写盘线程留一个核（「性能」已改）。

- **std 前缀表加了三项。** Rust 1.97 的 std 对字符串切片越界写的是 `start byte index …`、`end byte index …`、`byte range starts at …`，不再以 `byte index` 开头，也不再带原字符串；按原表这些消息只剩长度。这三项加进表里，都带数字（「隐私」已改）。`canary_str_slicing_panic` 检查前缀以 `byte index` 结尾。
- **限流不每次读时钟。** 读单调时钟比被限流的调用的其余部分还慢，`log/third_party_limited` 达不到预算。改为每个线程缓存窗口，用 CPU 计数器限定缓存的新旧（「限流」已改）。aarch64 读计数器用一行内联汇编（`mrs cntvct_el0`），x86_64 用 `_rdtsc`；这是 CPU 架构的能力，不是 macOS 的系统接口，不经 `platform`。窗口的切换在一个线程上最多晚 44 ms 被看到。
- **第三方调用点的键按地址哈希。** target 和 `file()` 取字符串的地址与长度，不逐字节哈希（「限流」已改）。运行时拼出的 target 每次地址不同，只会多占表项，由溢出项兜住。
- **`Value::io` 写进了接口。** 「日志与诊断」已经用到它，「调用点」的接口里补上；写成 `{"kind":…,"code":…}`。panic 消息的写出形式也补进「隐私」。
- **宏在编译期多查两项。** 消息不能含 `{`、`}`（不插值，避免读者以为会插值），不超过 512 字节；字段不超过 12 个。trybuild 的用例按出错的位置拆成 6 个文件：rustc 在一个文件里遇到宏匹配失败或类型错误后，不再报后面的错误。
- **插件 ID 限于 `[a-z0-9_-]`、1 到 64 字节。** `PluginTag::register` 只接受这些字符，其他返回 None；`@plugin=level` 的名字先转小写，也只接受这些字符。这是指令能写出、`verbose.json` 能原样保存的范围：大写的 ID 永远匹配不到转小写的指令，带 `.` 的指令写进 `verbose.json` 时 `.` 变成 `?`，下次启动读不回来。测试 `registered_plugin_directive_survives_verbose_json`。
- **过滤的规范形式。** `Display` 依次写单独的级别、`target=level`、`@plugin=level`；名字转成小写，target 里的 `-` 读作 `_`；同名指令后面的覆盖前面的；空指令（如 `debug,,info` 中间那个）忽略，不算无法识别。`Filter` 在不开 `sink` 时也导出，调用点的过滤缓存用到它。
- **写盘线程启动时就打开默认文件、写文件头**，不等第一条记录，第 3 步「启动后 1 s 内出现本次运行的文件」依赖这一点。
- **首帧前 panic 写出的文件头没有 `version`、`build`、`os`、`arch`**：这些在 `start` 的 `Config` 里才有。写盘线程启动后接着写这个文件，不再写第二个文件头。
- **详细文件的开关。** 过滤来自 `--log`、`LECTERN_LOG` 或设置时，都算详细日志开启（文件头 `verbose` 为 true），另写详细文件。
- **`testing` 多了基准用的入口**：`third_party`、`set_callsite_limit`、`flush`、`install_early_again`（见「测试辅助」）。panic 记录的 `tg` 是 `logging`。
- **基准的写法。** bench 的统计量没有 p99：`log/contended_call` 在测量模式下正好采 10 000 次，预算写成「超过 5 µs 的不超过 100 次」，与 p99 ≤ 5 µs 相同。`log/enabled_call` 每个样本是写盘线程清空缓冲后的 256 次调用，期间调高调用点上限，测的是写出的路径而不是限流。`log/idle_memory/verbose` 是「性能」里「另报一个数」的那一项，只记录。
- **单元测试只在开启 `sink` 时编译**（`just test` 用 `--all-features`）；`cargo test -p lectern-logging` 不带 feature 时只跑文档测试。
