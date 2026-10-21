# fs_watch 设计

状态：验收第 1 步（本 crate）已实现（[#86](https://github.com/SoloJiang/lectern-app/pull/86)，基准 [#92](https://github.com/SoloJiang/lectern-app/pull/92)）；第 2 步（`git` 的适配）已实现（[#95](https://github.com/SoloJiang/lectern-app/pull/95)，实际验证与差异见 [git 设计](git.md)「实际验证」「与设计的差异」）；第 3 步（macOS 后端）已实现（[#90](https://github.com/SoloJiang/lectern-app/pull/90)），只在 Linux 上交叉检查过，macOS 上的运行由 macOS CI 验证；第 4 步（装配与实测）尚未实现。

对应 issue [#14](https://github.com/SoloJiang/lectern-app/issues/14)（没有单独的 issue；与 [platform 设计](platform.md)相同，随 Git 与 Diff 推进），消费方分属 [#9](https://github.com/SoloJiang/lectern-app/issues/9)（`project` 的增量刷新）、[#13](https://github.com/SoloJiang/lectern-app/issues/13)（`agents` 的会话目录）、[#31](https://github.com/SoloJiang/lectern-app/issues/31)（`usage` 的 reflog 时刻）和 [#66](https://github.com/SoloJiang/lectern-app/issues/66)（Analyze 的修订号）。`fs_watch` 是与平台无关的文件监听：把后端交来的原始事件按 git index 和忽略规则过滤、合并成批交给消费方，按 worktree 维护修订号，给 reflog 的新增记录打上监听时刻和序号。FSEvents 与 kqueue 后端在 `platform`，由 `app` 注入（[技术调研「性能要点」](tech-research.md#性能要点)、[ADR 0019](../adr/0019.md)、[ADR 0020](../adr/0020.md)、[ADR 0026](../adr/0026.md)）。

## 范围

- 做：
  - 后端接口（`Backend`）：`platform` 实现，`app` 注入；测试用的内存后端（`testing::FakeBackend`）。
  - 三种监听：仓库（git 公共目录里的元数据）、worktree（工作区文件，按 index 过滤）、普通目录（递归，不过滤，供 `agents`）。
  - 每个订阅各自的事件合并。
  - worktree 修订号，以及「先让后端交付已发生的事件、再读修订号」的接口。
  - reflog 新增记录的读取、监听时刻和序号。
  - 仓库之外参与生效配置的文件清单的监视：清单的来源接口、符号链接展开、清单的重新计算。
  - 临时的不过滤路径清单。
  - 根目录还不存在时监听最近的已存在上级目录。
- 不做：
  - FSEvents、kqueue 的系统调用：`platform`（架构规则 6），见「platform 后端」。
  - 读 index、解析忽略规则、求配置来源：`git` 实现本 crate 定义的 `WorktreeSource`（见「对其他 crate 的要求」）。本 crate 不依赖 `git`（基础设施层不依赖领域层）。
  - 解析 reflog 行：`git::parse_reflog_line`，`usage` 调用；本 crate 只按行切分。
  - 按事件刷新改动集、文件列表、会话摘要：`review`、`project`、`agents`。
  - 分支的「一次使用」与用量归属：`usage`。
- 本 issue 之外：`bench` 的运行器与预算表（[#7](https://github.com/SoloJiang/lectern-app/issues/7)，[bench 设计](bench.md)）。本 crate 的基准由各步的实现 PR 加进 `crates/bench`，见「性能」。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-kernel` | `BusyGuard`（静止计数）、`contain`（隔离消费方回调的 panic） | kernel 层，任何层可用 |
| `lectern-executor` | fs_watch 线程（`run_dedicated`）、加载过滤器和配置清单的后台任务（`run`，`Background`） | 后台线程只由 `executor` 管理（[ADR 0024](../adr/0024.md)、[executor 设计](executor.md)） |
| `insta`、`tempfile`（dev） | 批次与 reflog 记录的快照；reflog 读取和符号链接展开用真实的临时目录 | 已在工作区依赖与 Cargo.lock 里 |

不引入外部依赖：不用 `notify`（它自带后端和线程，事件模型不带 FSEvents 的 `MustScanSubDirs`、`FlushSync`），不用 `bitflags`（事件标志是手写的 `u32` 常量）。锁用 `std::sync::Mutex` 与 `Condvar`。

`platform` 依赖本 crate（实现 `Backend`），本 crate 不依赖 `platform`。`git` 依赖本 crate（实现 `WorktreeSource`），领域层依赖基础设施层，方向向下（架构规则 1）。

## 概念

| 概念 | 说明 |
|---|---|
| 目标（`Target`） | 后端监视的一个路径：`Tree` 是目录及其下全部层级，`Entries` 是一个目录的条目增减和其中几个具名条目的变化。 |
| 原始批次（`RawBatch`） | 后端一次回调交来的事件，路径相对目标。修订号按原始批次计数。 |
| 仓库状态 | 一个 git 公共目录的监听状态：公共目录的 `Tree` 目标、reflog 读取进度、不过滤清单。同一公共目录只有一份，被所有 `RepoWatch` 句柄共享。 |
| worktree 状态 | 一个 worktree 的监听状态：工作区根的 `Tree` 目标、过滤器、配置文件清单、修订号。属于所在的仓库状态，被所有 `WorktreeWatch` 句柄共享。 |
| 订阅 | 一个句柄连同它的 `Sink` 和合并参数。每个订阅自己合并、自己交付。 |
| 批次 | 一个订阅在合并窗口结束时交付的结果：`RepoBatch`、`WorktreeBatch` 或 `DirBatch`。 |
| 修订号（`Revision`） | worktree 状态的计数器：一个原始批次里有影响这个 worktree 的事件，就加一。 |
| 过滤器（`PathFilter`） | 回答一个工作区路径是否受跟踪、是否被忽略。`git` 构造，只在内存里查询。 |

## 接口

本 crate 的函数都不做 IO，可以在任何线程调用，`flush_revision` 例外（阻塞，见下）。IO（后端的 arm、reflog 读取、路径的 `lstat`）在 fs_watch 线程上，过滤器和配置清单的加载在 executor 的后台任务里，都不在 UI 线程上。

### 后端

```rust
/// 文件监听后端。`platform` 提供 macOS 实现（FSEvents 与 kqueue），测试用 `testing::FakeBackend`。
pub trait Backend: Send + Sync + 'static {
    /// 开始监视 `target`，之后的事件经 `sink` 交付。只在 fs_watch 线程上调用，可以阻塞（创建流、打开描述符），
    /// 应在几毫秒内返回。`target.path` 不存在时返回 `BackendError::NotFound`，不创建监视。
    fn arm(&self, target: &Target, sink: EventSink) -> Result<Arc<dyn Armed>, BackendError>;
}

/// 一个已开始的监视。最后一个 `Arc` drop 时停止，drop 返回之后后端不再调用这个目标的 `EventSink`。
pub trait Armed: Send + Sync {
    /// 把已经发生、还没交付的事件立即交给 `EventSink`，返回时它们已在 fs_watch 的队列里。
    /// 在调用 `flush_revision` 的线程上调用，阻塞；`EventSink::send` 不阻塞，所以等待有界。
    fn flush(&self) -> Result<(), BackendError>;
}

pub struct Target {
    /// 绝对路径。后端自己求规范路径（FSEvents 报告的是解开符号链接后的路径），交付的路径相对这个目标。
    pub path: PathBuf,
    pub scope: Scope,
}

pub enum Scope {
    /// 目录及其下全部层级。
    Tree,
    /// 目录本身的条目增减、改名，以及 `names` 里每个条目自身（不跟随符号链接）的内容和元数据变化。
    /// 具名条目被删除、改名覆盖后重新出现时，后端继续监视新的条目。`names` 为空时只报告条目增减。
    Entries { names: Vec<OsString> },
}

/// 后端交付事件的入口。`Clone + Send + Sync`；`send` 不阻塞：fs_watch 的队列满时丢弃这一批，
/// 给这个目标记一次溢出，fs_watch 按「重新扫描」处理（见「状态与异常」）。
/// `send` 在入队之前从这个目标的一个订阅取 `BusyGuard`（见「数据流与线程」的静止规则），随原始批次入队。
#[derive(Clone)]
pub struct EventSink { /* 目标 ID + Arc<队列> */ }

impl EventSink {
    pub fn send(&self, batch: RawBatch);
}

pub struct RawBatch { pub events: Vec<RawEvent> }

pub struct RawEvent {
    /// 相对目标的路径，`/` 分隔，原始字节；空表示目标本身。`Entries` 的路径是条目名。
    pub path: Box<[u8]>,
    pub flags: Flags,
}

/// 事件标志。FSEvents 在文件事件模式下会把同一路径先后发生的变化合进一个事件，
/// 所以 CREATED / REMOVED / RENAMED / MODIFIED 只是提示：fs_watch 只当作「这个路径可能变了」，
/// 不按它们判断最终状态。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Flags(pub u32);

impl Flags {
    pub const FILE: Flags;          // 条目是普通文件
    pub const DIR: Flags;           // 条目是目录
    pub const SYMLINK: Flags;
    pub const CREATED: Flags;
    pub const REMOVED: Flags;
    pub const RENAMED: Flags;
    pub const MODIFIED: Flags;      // 内容
    pub const META: Flags;          // 权限、属主、扩展属性、时间
    /// 这个路径之下有事件没能逐个报告，要重新扫描（FSEvents 的 MustScanSubDirs）。
    pub const RESCAN: Flags;
    /// 与 RESCAN 同时出现：事件在用户态或内核里被丢弃（UserDropped / KernelDropped）。
    pub const DROPPED: Flags;
    /// 目标本身被删除、改名或换成了别的条目（FSEvents 的 RootChanged，kqueue 目录描述符的 NOTE_DELETE / NOTE_RENAME）。
    pub const ROOT_CHANGED: Flags;
    /// 目标所在的卷被卸载。
    pub const UNMOUNTED: Flags;
    pub fn contains(self, other: Flags) -> bool;
}

pub enum BackendError { NotFound, PermissionDenied, Exhausted, Other(String) }
```

### 监听器与句柄

```rust
/// `Clone + Send + Sync`。`app` 在启动屏障放开后创建一个，交给需要它的插件（`project`、`agents`、`review`、`usage`）。
/// 创建时不建线程、不调用后端；第一次 `watch_*` 时才启动 fs_watch 线程（ADR 0017）。
#[derive(Clone)]
pub struct Watcher { /* Arc<Inner> */ }

pub struct Config {
    /// fs_watch 队列里最多积压的事件数，默认 16 384。超过时溢出的目标按重新扫描处理。
    pub queue_events: usize,
    /// 一个批次最多带的路径数，默认 10 000。超过时改为 `rescan`，不再列路径。
    pub max_batch_paths: usize,
    /// `flush_revision` 等 fs_watch 线程处理完的上限，默认 2 s。
    pub flush_timeout: Duration,
}

impl Default for Config { /* 上面的默认值 */ }

impl Watcher {
    pub fn new(backend: Arc<dyn Backend>, executor: Executor, config: Config) -> Watcher;

    /// 监听一个仓库的 git 元数据。同一 `common_dir` 共享一份仓库状态；每个句柄有自己的 `sink` 和合并参数。
    pub fn watch_repo(&self, spec: RepoSpec, sink: Arc<dyn Sink<RepoBatch>>) -> RepoWatch;

    /// 递归监听一个目录，不过滤（`agents` 的会话目录）。`root` 可以还不存在，见「目录监听」。
    pub fn watch_dir(&self, spec: DirSpec, sink: Arc<dyn Sink<DirBatch>>) -> DirWatch;

    /// 停止 fs_watch 线程，drop 全部后端监视，之后的 `watch_*` 返回已停止的句柄（不再交付）。`app` 退出时调用。
    pub fn shutdown(&self);
}

pub struct RepoSpec {
    /// git 公共目录的绝对规范路径（`git::Location::common_dir`）。
    pub common_dir: PathBuf,
    pub coalesce: Coalesce,
}

pub struct WorktreeSpec {
    pub name: WorktreeName,
    /// 工作区根的绝对路径（`git::WorktreeInfo::root`）。
    pub root: PathBuf,
    /// 过滤器与配置清单的来源，`git` 实现。同一 worktree 状态只用第一个句柄给的来源。
    pub source: Arc<dyn WorktreeSource>,
    pub coalesce: Coalesce,
}

pub struct DirSpec { pub root: PathBuf, pub coalesce: Coalesce }

/// 与 `git::WorktreeId` 一一对应，转换在 `git` 里。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum WorktreeName { Main, Linked(Arc<str>) }

/// 句柄 drop 时取消订阅；一份状态的最后一个句柄 drop 时，fs_watch 线程 drop 它的后端监视。都是 `Send + Sync`。
pub struct RepoWatch { /* … */ }
pub struct WorktreeWatch { /* … */ }
pub struct DirWatch { /* … */ }

impl RepoWatch {
    /// 监听这个仓库里一个 worktree 的工作区。同一 `name` 共享一份 worktree 状态。
    pub fn watch_worktree(&self, spec: WorktreeSpec, sink: Arc<dyn Sink<WorktreeBatch>>) -> WorktreeWatch;
    /// 替换这个句柄的不过滤路径清单（路径相对 worktree 根，用于这个仓库的每个被监听的 worktree）。
    /// 生效的清单是同一仓库所有句柄清单的并集。不阻塞；下一个原始批次起生效。
    pub fn set_unfiltered(&self, paths: Vec<WatchPath>);
}

impl WorktreeWatch {
    /// 当前修订号，不先让后端交付。不阻塞。
    pub fn revision(&self) -> Revision;
    /// 先让这个 worktree 的每个后端监视交付已经发生的事件（`Armed::flush`），再等 fs_watch 线程处理完
    /// 在此之前进入队列的全部原始批次，然后读修订号。只计数，不提前交付正在合并的批次。
    /// 阻塞（没有积压时 < 5 ms），只在 executor 线程上调用：不在 UI 线程上，也不在 `Sink` 回调里
    /// （在 fs_watch 线程上调用时返回 `WouldDeadlock`）。worktree 状态还在 arm 时先等 arm 完成，计入同一个超时。
    pub fn flush_revision(&self) -> Result<Revision, FlushError>;
}

/// 相对 worktree 根、`/` 分隔的路径，原始字节。不变量与 `git::RepoPath` 相同（非空、各级非空、没有 `.` 和 `..`、
/// 没有 NUL、不以 `/` 开头），只能经 `new` 构造。后端报告的路径不满足时丢弃这个事件。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct WatchPath(Arc<[u8]>);

impl WatchPath {
    pub fn new(bytes: &[u8]) -> Option<WatchPath>;
    pub fn as_bytes(&self) -> &[u8];
}

/// 两次读到的值相等，说明这两次之间这个 worktree 没有被计入的变化。`epoch` 区分 worktree 状态：
/// 全部句柄 drop 后重新监听，得到新的 epoch，所以不同状态下读到的修订号不会相等。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Revision { pub epoch: u64, pub n: u64 }

pub enum FlushError {
    Timeout,
    /// worktree 状态已失败（后端出错、根目录消失），修订号不再可信。调用方按「已变化」处理。
    NotWatching(Failure),
    WouldDeadlock,
}

/// 合并参数，见「合并」。
#[derive(Clone, Copy)]
pub struct Coalesce { pub quiet: Duration, pub max_delay: Duration }

impl Coalesce {
    pub const REPO: Coalesce;       // 20 ms / 200 ms
    pub const WORKTREE: Coalesce;   // 50 ms / 500 ms
    pub const DIR: Coalesce;        // 250 ms / 1 s
}
```

`watch_*` 和 `set_unfiltered` 只把命令放进 fs_watch 的队列就返回。arm、首次加载完成后，句柄的第一个批次带 `health` 和 `rescan: true`，`health` 是当时的状态（通常是 `Live`；首次加载失败时是 `Degraded`，arm 失败时是 `Failed`）：消费方在调用 `watch_*` 之后、收到这一批之前读到的状态可能漏掉 arm 之前的变化，收到后按全部可能变化处理一次。`agents` 的「先注册监听、再枚举」和 `project` 打开后读 index 都依赖这一点。

### 消费方回调

```rust
pub trait Sink<B>: Send + Sync + 'static {
    /// 交付一个合并好的批次。在 fs_watch 线程上、`contain` 里调用；只更新状态或提交后台任务，不阻塞、不做 IO，
    /// 不调用 `flush_revision`。`busy` 是这一批第一个事件到达时用 `busy()` 取的守卫，
    /// 消费方把它移进处理这一批的任务，任务结束才 drop（[kernel 设计](kernel.md)「静止」）。
    /// panic 时这个订阅不再交付，记日志，其余订阅照常。
    fn deliver(&self, batch: B, busy: Option<BusyGuard>);

    /// 取一个守卫，插件实现为 `PluginHandle::busy()`。在 fs_watch 线程、调用 `watch_*` 的线程或后端的回调线程
    /// （`EventSink::send`）上调用，不阻塞、不取会与这些线程竞争的锁。
    /// 返回 None（插件已停用）时照常合并和交付，只是不带守卫。
    fn busy(&self) -> Option<BusyGuard> { None }
}
```

### 批次

```rust
pub struct WorktreeBatch {
    /// 交付时这个 worktree 的修订号。
    pub revision: Revision,
    /// 按字节序、去重。目录路径（目录被删除、改名，或后端只报告了目录）表示其下全部路径。`rescan` 为 true 时为空。
    pub paths: Vec<WatchPath>,
    pub meta: MetaChanges,
    /// 这一批有事件丢失或路径过多：调用方按全部可能变化处理（`git::Refresh::Full`）。
    pub rescan: bool,
    pub health: Option<Health>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MetaChanges {
    pub head: bool,     // 这个 worktree 的 HEAD
    pub index: bool,    // 这个 worktree 的 index
    pub refs: bool,     // refs/、packed-refs、reftable/
    /// config、config.worktree、info/exclude、info/attributes，或配置清单里的任何文件
    pub config: bool,
    /// `paths` 里有名为 `.gitignore` 或 `.gitattributes` 的路径
    pub rules: bool,
}

pub struct RepoBatch {
    /// HEAD 或 index 变化的 worktree，按 `WorktreeName` 排序。
    pub worktrees: Vec<(WorktreeName, WorktreeMeta)>,
    /// `<公共目录>/worktrees/` 下有目录增减，或某个 worktree 的 `gitdir`、`locked`、`commondir` 变化。
    pub worktree_list: bool,
    pub refs: bool,
    /// 公共目录里的 config、info/exclude、info/attributes。仓库之外的配置文件只计入 worktree 批次。
    pub config: bool,
    /// 两者都按 `seq` 排序，共用一个计数，合并后即先后顺序（见 `ReflogReset`）。
    pub reflog: Vec<ReflogRecord>,
    pub reflog_resets: Vec<ReflogReset>,
    pub rescan: bool,
    pub health: Option<Health>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct WorktreeMeta { pub head: bool, pub index: bool }

pub struct DirBatch {
    /// 绝对路径（`DirSpec::root` 加相对路径，不规范化），按字节序、去重。
    pub paths: Vec<PathBuf>,
    pub rescan: bool,
    /// 根目录出现（`Appeared`，同时 `rescan` 为 true）或消失（`Vanished`）时为 Some。
    pub root: Option<RootState>,
    pub health: Option<Health>,
}

pub enum RootState { Appeared, Vanished }

pub enum Health {
    Live,
    /// 仍在监听，但有一项退化，见「状态与异常」。
    Degraded(Degraded),
    Failed(Failure),
}

pub enum Degraded {
    /// 过滤器加载失败：除 `.git` 之外的路径全部交付并计入修订号。
    Filter(String),
    /// 配置清单加载失败：只监视仓库里固定的配置文件。
    Config(String),
}

pub enum Failure { Backend(String), RootGone, Unmounted, Stopped }
```

### reflog 记录

```rust
pub enum ReflogFile {
    /// `logs/HEAD`（主 worktree）或 `worktrees/<名字>/logs/HEAD`。
    Head(WorktreeName),
    /// `logs/refs/heads/<分支>`；值是完整的引用名，如 `refs/heads/feature/x`。
    Branch(Arc<[u8]>),
    /// `logs/refs/stash`。
    Stash,
}

pub struct ReflogRecord {
    pub file: ReflogFile,
    /// 这一行在文件里的起始字节偏移。
    pub offset: u64,
    /// 一行，不含结尾的换行。由 `git::parse_reflog_line` 解析。
    pub line: Arc<[u8]>,
    /// 同一 `observed.run` 内单调递增，与 `ReflogReset::seq` 共用：读到的先后；同一文件里与行序一致。不跨运行比较。
    pub seq: u64,
    /// 读到这一行的那次读取结束时：记录写在它之前。
    pub observed: ReadStamp,
    /// 同一文件上一次读取开始时，那次没有读到这一行：记录写在它之后。新出现的文件为 None。
    pub after: Option<ReadStamp>,
    pub origin: Origin,
}

pub struct ReadStamp {
    /// 这次运行的编号：`Watcher::new` 时由墙上时间（纳秒）与标准库为每个进程随机生成的哈希密钥（`RandomState`）得出，同一 `Watcher` 的全部记录相同。
    /// `read` 和 `seq` 每次运行从 0 重新计数，`usage` 落盘后按它区分不同运行的计数；只比较相等，
    /// 运行的先后由 `usage` 分配的序号给出（见「reflog」的先后规则）。
    pub run: RunId,
    /// 读取的编号。同一运行内所有 reflog 读取在 fs_watch 线程上依次进行，编号越小越早，前一次结束后下一次才开始。
    pub read: u64,
    /// 墙上时间，纳秒精度。系统时间可能被调整，先后以 `read` 为准。
    pub at: SystemTime,
}

/// 运行编号，与 `Revision::epoch` 是同一类概念：区分计数器的来源，只比较相等。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RunId(pub u64);

pub enum Origin {
    /// 追加在已读过的位置之后。
    Appended,
    /// 文件在监听期间新出现（新建分支、改名后的分支），从头读出。改名带来的旧记录也在其中，`usage` 按内容去重。
    NewFile,
}

/// 与 `ReflogRecord` 共用同一个 `seq` 计数：按 `seq` 把 `reflog` 与 `reflog_resets` 合并，就是每个文件上记录与重置的先后，
/// 同一批里「追加、重置、再追加」也能分清。`usage` 把重置与记录一起落盘，「两者之间没有重置」才能算出来。
pub struct ReflogReset {
    pub file: ReflogFile,
    pub reason: ResetReason,
    pub seq: u64,
    /// 发现这次重置的那次读取结束时。
    pub observed: ReadStamp,
}

pub enum ResetReason {
    /// 文件变短或已读部分的末尾字节变了（`reflog expire`、`reflog delete`）。读取进度移到新的末尾，不重新交付旧记录。
    Rewritten,
    /// 文件被删除（删除分支、`stash clear`、改名的旧名字）。
    Removed,
    Unreadable(String),
}
```

### 过滤器与配置来源

```rust
/// `git` 实现（`git::Worktree::watch_source`），见「对其他 crate 的要求」。
pub trait WorktreeSource: Send + Sync + 'static {
    /// 读 index 和忽略规则（含未被忽略目录里未跟踪的 `.gitignore`），构造过滤器。阻塞；fs_watch 在 executor 的 `Background` 任务里调用，
    /// `cancel` 置位时放弃。
    fn filter(&self, request: &FilterRequest<'_>, cancel: &AtomicBool) -> Result<Arc<dyn PathFilter>, SourceError>;
    /// 这个 worktree 的生效配置、忽略规则和属性规则读取（或文件出现时会读取）的全部文件，绝对路径，可以不存在。
    /// 阻塞；调用方式同上。
    fn config_files(&self, cancel: &AtomicBool) -> Result<Vec<PathBuf>, SourceError>;
}

/// 一次过滤器加载要的东西（见「路径分类」）。
#[derive(Clone, Copy, Default)]
pub struct FilterRequest<'a> {
    /// 触发这次加载的事件里的 `.gitignore` 路径：遍历达到条目上限、没有找到它们时也读入；上级目录被忽略的照样不读。
    pub gitignores: &'a [WatchPath],
    /// 在用的过滤器，只在它的加载开始以来的重新加载都是 index 变化、其间的工作区事件都由当时有效的过滤器判断过时给出
    /// （见「路径分类」末段）：其间没有事件动过 `.gitignore`、含规则文件的目录、改名或删除的目录、`info/exclude`
    /// 和配置，也没有 rescan。来源可以沿用那次加载在工作区里读到的东西
    /// （`git` 沿用未跟踪遍历），前提是能证明结果与完整加载相同，否则完整加载。其余加载为 None。
    pub previous: Option<&'a dyn PathFilter>,
}

/// 只在内存里查询，不做 IO，任何线程。路径相对 worktree 根，满足 `WatchPath` 的不变量。
/// 大小写与 Unicode 规范化按仓库的 `core.ignoreCase`、`core.precomposeUnicode` 处理，由实现负责。
/// 以 `Any` 为父 trait，来源借此在 `FilterRequest::previous` 里认出自己的过滤器。
pub trait PathFilter: Any + Send + Sync + 'static {
    fn tracked(&self, path: &[u8]) -> Tracked;
    /// 只看 `path` 自身是否被忽略规则排除，不看上级目录；上级目录由 fs_watch 逐级检查（见「路径分类」）。
    fn ignored(&self, path: &[u8], is_dir: bool) -> bool;
    /// 构造时读入的工作区里的 `.gitignore` 路径，按字节序。fs_watch 用它判断目录事件是否影响规则。
    fn rule_files(&self) -> &[WatchPath];
}

pub enum Tracked {
    No,
    /// index 里有这个路径（含 skip-worktree 条目）。
    File,
    /// index 里有这个目录下的路径。
    Dir,
    /// index 里这个路径是子模块（gitlink）。
    Gitlink,
}

pub struct SourceError(pub String);
```

### 测试辅助（`test-support` feature）

```rust
pub mod testing {
    /// 内存里的后端：记录 arm、drop 和 flush，测试按目标注入原始批次。
    pub struct FakeBackend { /* … */ }
    impl FakeBackend {
        pub fn new() -> Arc<FakeBackend>;
        /// 当前已 arm 的目标，按路径排序。
        pub fn targets(&self) -> Vec<Target>;
        /// 向 `target` 注入一个原始批次；`target` 没有 arm 时 panic。
        pub fn emit(&self, target: &Path, events: &[(&str, Flags)]);
        /// 同 `emit`，原始批次由调用方构造：基准在计时之外构造事件，只计 fs_watch 的处理。
        pub fn emit_batch(&self, target: &Path, batch: RawBatch);
        /// 之后注入的事件先留在后端，下一次 `Armed::flush` 时才交付，模拟 FSEvents 的延迟。
        pub fn hold(&self, target: &Path);
        pub fn fail_next_arm(&self, target: &Path, error: BackendError);
        pub fn flush_count(&self, target: &Path) -> usize;
        /// 下一次 arm `target` 时，在 `arm` 返回之前交付这些事件。
        pub fn emit_on_arm(&self, target: &Path, events: &[(&str, Flags)]);
        /// 下一次 arm `target` 时，在监视建立之前运行 `hook`（模拟 reflog 第 1 段与 arm 之间的写入）。
        pub fn before_arm(&self, target: &Path, hook: impl FnOnce() + Send + 'static);
        pub fn is_armed(&self, target: &Path) -> bool;
        /// 至今的 arm 与 drop，每行 `arm <路径> <范围>` / `drop <路径> <范围>`。
        pub fn history(&self) -> Vec<String>;
    }

    /// 手动驱动的监听器：不建线程，时钟是假的。`flush_revision` 在调用线程上直接处理队列。
    pub fn manual(backend: Arc<dyn Backend>, executor: Executor, config: Config) -> (Watcher, Driver);

    pub struct Driver { /* … */ }
    impl Driver {
        /// 交替处理 fs_watch 的队列和 `executor.run_until_idle()`，直到两边都没有工作。返回处理的条目数。
        pub fn run_until_idle(&self) -> usize;
        /// 推进假时钟，触发到期的合并窗口，然后 `run_until_idle`。
        pub fn advance(&self, by: Duration);
        /// 只处理一次 fs_watch 的队列，不运行 executor：开始的加载留在途中，供测试在加载前后插入事件。
        pub fn pump(&self) -> usize;
    }

    /// 记录收到的批次和是否带守卫。
    pub struct RecordingSink<B> { /* … */ }
    impl<B> RecordingSink<B> {
        pub fn new(busy: Option<Arc<dyn Fn() -> Option<BusyGuard> + Send + Sync>>) -> Arc<RecordingSink<B>>;
        /// 取走已收到的批次，同时 drop 它们的守卫（守卫在 `take` 之前一直由 sink 持有，像消费方的任务那样）。
        pub fn take(&self) -> Vec<(B, bool)>;
        pub fn len(&self) -> usize;
        pub fn is_empty(&self) -> bool;
    }

    /// 内存里的过滤器与配置来源：按给定的跟踪路径、忽略规则（只支持前缀与 `*.后缀`）、规则文件和配置清单回答，记录调用次数。
    /// 过滤器在构造时复制当前设置，之后的修改要重新加载才生效，与真实的 index 一样。
    pub struct FakeSource { /* … */ }
    impl FakeSource {
        pub fn new() -> Arc<FakeSource>;
        pub fn track(&self, paths: &[&str]);
        pub fn untrack(&self, paths: &[&str]);
        pub fn gitlink(&self, paths: &[&str]);
        /// `*.后缀`；不含 `/` 的名字匹配任一级的同名条目（结尾 `/` 只匹配目录）；含 `/` 的匹配这个路径本身。
        pub fn ignore(&self, patterns: &[&str]);
        pub fn unignore(&self, patterns: &[&str]);
        pub fn rule_files(&self, paths: &[&str]);
        pub fn set_config_files(&self, files: &[PathBuf]);
        pub fn fail_filter(&self, error: Option<&str>);
        pub fn fail_config(&self, error: Option<&str>);
        pub fn filter_calls(&self) -> usize;
        pub fn config_calls(&self) -> usize;
        /// 每次 `filter` 调用的 `gitignores` 参数。
        pub fn gitignores(&self) -> Vec<Vec<WatchPath>>;
        /// 每次 `filter` 调用收到的 `previous` 是第几次加载（从 1 起）构造的过滤器，没有时为 None。
        pub fn previous(&self) -> Vec<Option<usize>>;
    }
}
```

## 路径分类

worktree 状态收到工作区根目标上的一个事件，路径为 `p`（相对 worktree 根）。按顺序判断，先命中的规则生效：

1. `p` 为空：目标本身。有 `ROOT_CHANGED` 或 `UNMOUNTED` 时 worktree 状态失败（`Failure::RootGone` / `Unmounted`），修订号加一；其余忽略。
2. `p` 在这个 worktree 的配置清单里（按绝对路径比较，见「配置文件清单」）：`meta.config`，修订号加一，重新计算配置清单和过滤器。再继续往下判断，它可能同时是工作区路径。
3. `p` 的任何一级名字是 `.git`：丢弃。主 worktree 的 git 目录由仓库状态处理，嵌套仓库的 git 目录不属于工作区。
4. `p` 在生效的不过滤清单里，或是清单里某个路径的上级目录（目录被删除、改名）：交付。
5. `filter.tracked(p)` 是 `File`、`Dir` 或 `Gitlink`：交付（受跟踪的路径即使匹配忽略规则也交付）。
6. 从最上一级起检查 `p` 的每个上级目录 `a`：`tracked(a)` 是 `Gitlink` 时丢弃（子模块内部）；`tracked(a)` 不是 `Gitlink` 且 `ignored(a, true)` 时丢弃，`a` 之下有受跟踪的文件也一样（git 不进入被忽略的目录，也不重新包含被排除目录之下的路径，其中的未跟踪路径和规则文件都不起作用，`node_modules/**/.gitignore` 也在这里丢弃；其中受跟踪的路径已在第 5 步交付）。
7. 最后一级名字是 `.gitignore` 或 `.gitattributes`：交付，`meta.rules`；`.gitignore` 同时触发过滤器重新加载。这两种文件自身被忽略规则排除时 git 照样应用，所以不再经过第 8 步。
8. `ignored(p, flags 含 DIR)`：丢弃。
9. 其余是未跟踪、未被忽略的路径：交付。

第 6 步的结果按目录缓存（每个 worktree 状态最多 1024 个目录，两代缓存近似最近使用淘汰：当前一代满 512 项时变为上一代，上一代里命中的项移回当前一代），过滤器替换时清空，所以 `npm install` 写出的几万个 `node_modules/**` 事件每个只需一次缓存查找。

过滤器在以下情况重新加载（executor 上调用 `WorktreeSource::filter`，同时最多一个，加载期间又触发的在结束后再加载一次）：worktree 状态 arm 时；这个 worktree 的 `index` 变化；任何 `.gitignore` 变化（第 7 步）；事件路径是 `PathFilter::rule_files` 里某个文件的上级目录（目录被改名、删除时后端可能只报告目录本身）；带 `DIR` 且带 `RENAMED` 或 `REMOVED` 的事件（保守处理：移进来的目录里的新 `.gitignore` 由重新加载的遍历找到）；`info/exclude` 变化；公共目录的 `config`、这个 worktree 的 `config.worktree` 或配置清单里的文件变化（`core.excludesFile`、`core.ignoreCase` 可能变了）；工作区根目标上的 `RESCAN`。按事件路径触发的几条（`.gitignore`、规则文件的上级目录、目录改名或删除）只看按当前过滤器不被丢弃的路径：被忽略目录里的目录改名（`npm install` 写 `node_modules/.staging/*` 后改名）不触发；第一次加载完成之前暂存的路径在第一个过滤器装上后按它判断，判为交付的再触发一次。

只由 index 变化触发的重新加载把在用的过滤器作为 `FilterRequest::previous` 交给来源，`git` 据此沿用上一次的未跟踪遍历，不再列目录（[git 设计「index」](git.md#index)）。依据：加载期间又触发的重新加载都由下一次加载完成，所以一次加载服务的是在用过滤器的加载开始以来的全部触发；会改变遍历结果的事件（`.gitignore`、规则文件的上级目录、目录改名或删除）都会触发一次不是 index 的重新加载，但只在判断它的过滤器与磁盘上的规则一致时才可靠：过滤器过期（有加载在途）、失败或还没有第一个时，一个新的规则文件可能被判为丢弃、不触发（加载开始后删掉 `.gitignore` 里的 `out/`，加载的遍历列过 `out/` 之后才新建 `out/.gitignore`，旧过滤器仍忽略 `out/`，这个事件丢弃；失败的过滤器只认 `.gitignore` 路径，移进来的目录不触发；第一次加载之前暂存的项合并成目录、或溢出成 rescan 时，规则文件的信号丢了）。所以 worktree 状态记一个「不确定」标记：这三种情况下到达的任何工作区事件（过了第 3 步的）都置位，加载失败也置位；一次不带 `previous` 的加载开始时清除，因为它在这之后才读工作区，看得到此前的一切。只有标记未置位时才给 `previous`。代价是偶尔多一次完整加载（例如 index 加载期间编辑了文件，下一次 index 变化仍完整加载一次），换来不变量：`previous` 给出时，从它的加载开始以来的重新加载都是 index 变化，其间的每个工作区事件都由当时有效的过滤器判断过，所以没有规则文件或目录变化漏掉。所以下列情况都不给 `previous`：第一次加载；服务的触发里有任何一个不是 index 变化（合并进同一次加载的也算）；在用的是失败的加载；「不确定」标记置位。index 本身的变化也可能改变遍历（子模块、嵌套仓库之下有没有跟踪的路径、未跟踪条目是否到上限），由来源自己判断能否沿用。

从触发重新加载的原始批次起，到新过滤器装上为止，过滤器是过期的：走到第 6、8、9 步的路径（旧过滤器认为不受跟踪的，无论判为丢弃还是未跟踪；第 7 步的规则文件照常交付并触发加载）先暂存，照样计入修订号；新过滤器装上后重新分类，交付的路径并入当前批次。第一次加载完成之前的路径同样暂存。这样 `git add -f build/out.js` 之后紧接着的修改不会因为旧过滤器认为它被忽略而丢失，新建 `.gitignore` 之后紧接着写出的 `node_modules/**` 也会按新规则丢弃，而不是按旧规则作为未跟踪路径交付。暂存最多 4096 项：超过时把同一目录下的项合并成这个目录（从最深的目录开始），重新分类时被忽略的目录连同其下全部路径丢弃，否则作为目录路径交付；只有合并到 worktree 根仍超过上限时才改为 `rescan`。暂存期间这个订阅的批次不交付，合并窗口顺延到过滤器装上。

## git 元数据

仓库状态收到公共目录目标上的一个事件，路径为 `q`（相对公共目录）。最后一级名字以 `.lock` 结尾的先丢弃：git 写 `index.lock`、`HEAD.lock`、`refs/heads/x.lock` 再改名，改名后的目标路径会单独报告。其余按下表处理，表外的路径（`objects/`、`FETCH_HEAD`、`ORIG_HEAD`、`hooks/` 等）丢弃。

| `q` | 批次里的标志 | 修订号加一的 worktree | 另外触发 |
|---|---|---|---|
| `HEAD` | `Main` 的 `head` | `Main` | 重新计算 `Main` 的配置清单（`includeIf "onbranch:…"`） |
| `index` | `Main` 的 `index` | `Main` | 重新加载 `Main` 的过滤器 |
| `config.worktree` | `config` | `Main` | `Main` 的配置清单与过滤器 |
| `worktrees/<名字>/HEAD` | `Linked(名字)` 的 `head` | `Linked(名字)` | 它的配置清单 |
| `worktrees/<名字>/index` | `Linked(名字)` 的 `index` | `Linked(名字)` | 它的过滤器 |
| `worktrees/<名字>/config.worktree` | `config` | `Linked(名字)` | 它的配置清单与过滤器 |
| `worktrees`、`worktrees/<名字>`（目录本身）、`worktrees/<名字>/{gitdir,locked,commondir}` | `worktree_list` | — | — |
| `refs/**`、`packed-refs`、`reftable/**` | `refs` | 全部 | — |
| `config` | `config` | 全部 | 全部 worktree 的配置清单与过滤器 |
| `info/exclude` | `config` | 全部 | 全部 worktree 的过滤器 |
| `info/attributes` | `config` | 全部 | — |
| `logs/HEAD`、`worktrees/<名字>/logs/HEAD`、`logs/refs/heads/**`、`logs/refs/stash` | — | — | 读这个 reflog 文件（见「reflog」） |
| `logs`、`logs/refs`、`logs/refs/heads/**` 的目录本身 | — | — | 重新列出这个目录下的 reflog 文件 |

引用存储按并集处理：`refs/`、`packed-refs`、`reftable/` 都计入，所以不需要先知道仓库用的是文件还是 reftable。reftable 仓库的 reflog 在表里，没有 `logs/` 文件，本 crate 不解析 reftable：arm 时公共目录下有 `reftable/tables.list` 的仓库不读 reflog，`RepoBatch::reflog` 一直为空。`git` 目前把 reftable 仓库按普通文件夹打开（[ADR 0024](../adr/0024.md)「已知限制」），所以这条路径要等 `git` 支持 reftable 后才会走到。

linked worktree 的 `<名字>` 是 `worktrees/` 下的目录名，原样作为 `WorktreeName::Linked`，与 `git::WorktreeId::Linked` 相同。

## 修订号

每个 worktree 状态一个修订号，从 0 开始。fs_watch 线程每处理一个原始批次（工作区根目标、公共目录目标或配置清单的 `Entries` 目标交来的一次回调），对每个受影响的 worktree 最多加一。一个原始批次满足以下任一条就影响这个 worktree：

1. 工作区根目标上有事件按「路径分类」被交付，或被暂存（过滤器过期期间）。
2. 工作区根目标本身被删除、改名或卸载（第 1 步）。
3. 公共目录目标上有「git 元数据」表里「修订号加一」一列包含这个 worktree 的路径。
4. 这个 worktree 的配置清单里的任何一个文件有事件（无论来自哪个目标，见「配置文件清单」）。
5. 上述任一目标的事件带 `RESCAN`，或 fs_watch 的队列在这个目标上溢出。
6. 后端的监视出错、被重新 arm（期间的事件可能丢失）。

只落在「路径分类」第 3、6、8 步丢弃的路径上（`.git` 之下、子模块内部、被忽略的未跟踪路径），或只落在 `.lock` 文件和表外 git 元数据上的原始批次，不加一。

修订号在原始批次被处理时就加一，不等合并窗口结束，所以 `flush_revision` 能看到正在合并、还没交付的变化。

Analyze 的用法（ADR 0019 约束 9）：计算指纹之前调 `flush_revision` 得到 `r1`，结果显示之前再调一次得到 `r2`；`r1 != r2` 或任一次返回错误，结果标为「基于较早的版本」。

## 合并

每个订阅独立合并。订阅收到第一个要交付的变化时开一个批次，记下首个事件的时刻 `first`，此后每个事件更新 `last`；批次在 `min(last + quiet, first + max_delay)` 交付。`max_delay` 保证 agent 连续写文件时批次也会按时交付。

| 句柄 | `quiet` | `max_delay` | 依据 |
|---|---|---|---|
| `RepoWatch` | 20 ms | 200 ms | reflog 的时刻在读取时取得，不受合并影响；引用和 HEAD 的变化尽快交付 |
| `WorktreeWatch` | 50 ms | 500 ms | [project 设计](project.md)「数据流与线程」的「一批事件到达后等 50 ms」由这里实现，`project` 不再另外等待 |
| `DirWatch` | 250 ms | 1 s | [agents 设计](agents.md)「数据流与线程」第 5 步 |

调用方可以在 `*Spec::coalesce` 里改。批次里的路径去重排序；路径数超过 `Config::max_batch_paths` 时清空，改为 `rescan`。`health` 变化不等合并窗口：先交付当前批次，再立即交付带新 `health` 的批次。fs_watch 线程只在有批次等待交付时设定时器，没有时不醒来。

## reflog

仓库状态的 reflog 进度分两段建立，都在 fs_watch 线程上：

1. **arm 之前**：列出 `logs/HEAD`、`logs/refs/heads/` 下的全部文件、`logs/refs/stash` 和每个 `worktrees/<名字>/logs/HEAD`，为每个文件记下读取进度：已读到的偏移（当时的文件长度）、偏移之前最多 64 字节的副本、这次读取的 `ReadStamp`。这时已有的记录不交付。
2. **arm 之后**：公共目录目标 arm 成功后，立即对第 1 段的每个文件按下文的步骤补读一次，并重新列出这几个目录。第 1 段与 arm 之间追加的行由补读交付（`Appended`，`after` 是第 1 段的 `ReadStamp`）；这段时间新出现的文件为 `NewFile`，消失的为 `Removed`。

补读与随后排队的事件触发的读取都从同一份进度继续，进度只前进，所以 arm 前后重叠的那段时间里写入的行不会落在已记录的偏移之内而丢失，也不会交付两次：一次运行内，同一文件的同一偏移最多交付一条记录（`usage` 也可以按 `run`、文件和偏移去重）。列出与读取都在 fs_watch 线程上依次进行，读取编号才有全序（几百个分支只需几毫秒的 `fstat` 和 64 字节读取）。

公共目录目标上有某个 reflog 文件的事件时，fs_watch 线程立即读它（不等合并窗口，也不等过滤器）：

1. 取读取编号，记下开始时刻；`fstat` 后从偏移之前 64 字节处读起。文件比偏移短、或副本对不上：`ReflogReset { Rewritten }`，进度移到新的末尾，不交付记录。文件不存在：`Removed`，删掉进度。
2. 从偏移读到最后一个换行，每次最多 1 MiB。每个 `RepoWatch` 订阅的一个批次里 reflog 行合计最多 1 MiB：一次读取达到上限时，这一批立即交付，剩下的部分从保存的进度接着读，进入下一批，依次进行，直到读到末尾（改名后的分支整份重读、大段追加都按这样分批）。最后一个换行之后不完整的部分不消费，等下一次事件（git 用一次 `write` 追加整行，读到半行很少见）。超过 64 KiB 的行跳过、计数，进度越过它。
3. 读取结束后取结束时刻作为这些记录的 `observed`；`after` 是这个文件上一次读到末尾的那次读取的 `ReadStamp`（分批接着读的部分沿用同一个 `after`，因为这些行在前一批读取时已经存在）。新出现的文件从头读，每一批都是 `origin` 为 `NewFile`、`after` 为 None。每条记录按读到的顺序取 `seq`。
4. 记录放进每个 `RepoWatch` 订阅的当前批次。

先后的判断规则（供 `usage`）。记录 A 一定早于记录 B，当且仅当下列之一成立：

- **同一运行**（`A.observed.run == B.observed.run`）：A 与 B 在同一文件里、`A.offset < B.offset`，并且这次运行里两者之间这个文件没有 `ReflogReset`；或 `B.after` 存在且 `A.observed.read < B.after.read`（读取依次进行，A 被读到的那次读取结束之后、B 所在文件的上一次读取才开始，那时 B 还没写入）。
- **不同运行**：A 的运行早于 B 的运行，且 `B.after` 存在。B 写在它这次运行的第 1 段读取之后，第 1 段在上一次运行结束之后，A 写在它被读到之前，也就在上一次运行结束之前。偏移不跨运行比较：Lectern 关闭期间 git 可能已改写文件（`reflog expire`），启动时的第 1 段直接以改写后的文件为基准，不产生 `ReflogReset`。`after` 为 None 的 `NewFile` 记录可能是改名带来的旧行，不参与跨运行的判断。

运行的先后按 `usage` 落盘的序号判断，不按墙上时间：`usage` 第一次收到某个 `RunId` 的记录时在 SQLite 里给它分配下一个序号。Lectern 是单实例应用，这次运行的第一次写入总在上一次运行结束之后，所以序号顺序就是运行顺序，系统时间被调整也不影响。其余情况判断不了，`usage` 按 [ADR 0020](../adr/0020.md) 标为「不确定」。所以监听时刻只给出上界和下界，不当作写入时刻；FSEvents 的延迟只让区间变宽，不让判断出错。

reflog 文件不经 `platform` 的安全读取：公共目录不是被审阅的内容，Analyze 的威胁模型针对的是沙箱里的 agent（ADR 0019），能改写 git 目录的进程已经在以用户身份运行。读取用 `std::fs`，只读这几个固定位置下的普通文件，符号链接按 `Unreadable` 处理。

## 配置文件清单

`WorktreeSource::config_files` 给出的清单由 `git` 从 gix 已加载的配置得出，不运行 `git config --show-origin --list`。理由：

- 生产代码不运行 git 命令行（[ADR 0024](../adr/0024.md)：每次重新计算都要启动进程，还要逐项关闭 fsmonitor 等）。
- 要监视的是会改变 Lectern 自己的 diff 和快照的文件，即 `git` crate 实际读取的文件。git 命令行读的安装配置（`$(prefix)/etc/gitconfig`）和 `GIT_CONFIG_*` 环境变量，`git` crate 都不读（[git 设计](git.md)「与设计的差异」里的配置读取范围），监视它们只会多出无效的修订号；反过来，命令行的清单不会列出还不存在、但出现时会被读取的文件。

清单的内容（`git` 负责，见「对其他 crate 的要求」）：

- gix 配置里每一节的来源文件：系统（`/etc/gitconfig`）、XDG（`$XDG_CONFIG_HOME/git/config`，未设置时 `~/.config/git/config`）、`~/.gitconfig`、仓库的 `config` 和 `config.worktree`，以及 gix 按条件求值后实际引入的 `include.path` / `includeIf.*.path` 文件；
- 已加载的每个文件里出现的全部 `include.path` 和 `includeIf.*.path` 的目标，不论条件是否满足、文件是否存在（只展开一层；条件不满足的引入里再嵌套的引入，等条件满足、清单重新计算后才列出）；
- 系统、XDG、`~/.gitconfig` 三个位置本身，即使文件不存在；
- `core.excludesFile`、`core.attributesFile` 指向的文件，没有设置时的默认位置 `$XDG_CONFIG_HOME/git/ignore`、`$XDG_CONFIG_HOME/git/attributes`（`XDG_CONFIG_HOME` 未设置时为 `~/.config`），以及 `git` 实际读取的系统级属性文件；仓库的 `info/exclude`、`info/attributes`。

fs_watch 在加载任务里（executor 上）展开清单：

1. 对每个路径 `P`，从根起逐级 `lstat` 它的每一个前缀，不只看最后一级：某一级是符号链接时，把这个链接本身加入监视（它上级目录的 `Entries`，`names` 是链接名），读出目标、相对链接所在目录解析，再从目标继续逐级处理 `P` 剩下的部分；整条路径合计最多解开 40 个链接，超过时停在这里，按不存在处理。解析到最后一级的路径（`P` 的真实位置）也加入监视。某一级不存在时，停在解析后最长的已存在前缀上，按第 2 步监视它通往下一级的名字。所以 `~/.config` 是指向 dotfiles 仓库的链接时，链接被换掉、或链接后面的目录里新建 `git/config`，都能发现。dotfiles 管理工具常把这些文件或它们的上级目录做成链接。
2. 每个路径按位置分给目标：在公共目录之下的，由公共目录目标的事件匹配；在这个 worktree 的工作区根之下的，由工作区根目标的事件在「路径分类」第 2 步匹配（在过滤之前，所以被忽略的仓库内配置文件也能发现）；其余的，按上级目录合并成 `Entries` 目标（同一目录下的几个文件共用一个，`names` 取并集；多个仓库共用同一目录时只 arm 一次）。上级目录不存在时，监视第 1 步得到的最长已存在前缀（已解开其中的链接），`names` 是通往它的下一级名字，目录出现后逐级下移（与「目录监听」相同）。这些链接和前缀目录上的事件同样算作清单里的文件有事件：修订号加一，清单重新展开。
3. 与上一份清单比较，只 arm 新增的目标、drop 不再需要的目标。

清单在以下情况重新计算：worktree 状态 arm 时；清单里的任何文件有事件；这个 worktree 的 `HEAD` 变化；公共目录的 `config` 或这个 worktree 的 `config.worktree` 变化。重新计算期间到达的清单内文件事件照常计入修订号。

## 不过滤清单

`RepoWatch::set_unfiltered` 给出的路径不按 index 和忽略规则过滤（「路径分类」第 4 步），用于候选 stash 用 `-u` / `-a` 存下的未跟踪和被忽略文件。清单由 `usage` 随候选 stash 的增减更新（路径来自 `git::Repo::tree_paths`，转换成 `WatchPath`），生效清单是这个仓库所有句柄的并集，存成排序的数组，按二分查找匹配本身与上级目录。清单变化不让修订号加一，也不触发过滤器重新加载。

## 目录监听

`watch_dir` 的根存在时，arm 一个 `Tree` 目标，事件不过滤，路径拼成绝对路径交付。

根还不存在时（`agents` 第一次使用、`~/.codex/sessions` 还没建），fs_watch 找到最近的已存在上级目录 `A`，arm `Entries { names: [通往根的下一级名字] }`。这一级出现后改为监视它、继续下移；根出现后 drop 上级目录的目标，arm 根的 `Tree` 目标，交付 `root: Some(Appeared)` 与 `rescan: true`，`agents` 按这一批扫描一次。根目标收到 `ROOT_CHANGED`（被删除或改名）时 drop 它，回到上级目录的监视，交付 `root: Some(Vanished)`。用 `Entries` 而不是 `Tree` 监视上级目录，是因为最近的已存在上级目录可能是用户主目录，递归监视主目录会收到 `~/Library` 里持续不断的事件。

## platform 后端

`platform` 用 `#[cfg(target_os = "macos")]` 实现 `Backend`，构造函数 `platform::fs_events::backend(executor: Executor) -> Arc<dyn Backend>`。系统绑定手写 `extern "C"` 声明（CoreServices 的 `FSEventStream*`、CoreFoundation 的 `CFString` / `CFArray` / `CFRelease`、libdispatch 的队列函数），不引入 `fsevent-sys`、`objc2-core-services` 等 crate：用到的符号不到二十个。CoreServices 已经被 AppKit 链接，不增加启动时 dyld 的工作（ADR 0017）。

`Scope::Tree` 用 FSEvents，每个目标一个流：

- `FSEventStreamCreate`：路径是目标的规范路径（`realpath`），`sinceWhen = kFSEventStreamEventIdSinceNow`，延迟 0.02 s（合并在 fs_watch 里做），标志 `kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer | kFSEventStreamCreateFlagWatchRoot`。
- `FSEventStreamSetDispatchQueue` 设到后端自己的一个串行队列（QoS utility），所有流共用，不用主线程的 run loop。回调在这个队列上运行：去掉规范路径前缀，转成 `RawEvent`，调用 `EventSink::send`，不阻塞。前缀不匹配的路径（只有根本身的变化会这样）按 `ROOT_CHANGED` 处理。
- 标志的对应：`ItemIsFile` / `ItemIsDir` / `ItemIsSymlink` → `FILE` / `DIR` / `SYMLINK`；`ItemCreated`、`ItemRemoved`、`ItemRenamed` → 同名标志；`ItemModified`、`ItemCloned` → `MODIFIED`；`ItemInodeMetaMod`、`ItemChangeOwner`、`ItemXattrMod`、`ItemFinderInfoMod` → `META`；`MustScanSubDirs` → `RESCAN`，同时有 `UserDropped` 或 `KernelDropped` 时加 `DROPPED`；`RootChanged` → `ROOT_CHANGED`；`Unmount` → `UNMOUNTED`；`Mount` → 挂载点所在目录的 `RESCAN`；`Unmount` 的挂载点在目标之下时同样是挂载点所在目录的 `RESCAN`，挂载点是目标本身或在它之上时是空路径上的 `UNMOUNTED`；`EventIdsWrapped`、`HistoryDone` 丢弃。`ROOT_CHANGED` 和 `UNMOUNTED` 总在空路径上（fs_watch 只在目标本身上看这两个标志）。带 `MustScanSubDirs` 的事件路径不在目标之下时（丢弃事件可能报告在上级路径上），按目标本身的 `RESCAN` 处理，不当作 `ROOT_CHANGED`。
- `Armed::flush` 调 `FSEventStreamFlushSync`：它等回调返回，而回调只入队，所以等待有界。
- drop：`FSEventStreamStop`、`FSEventStreamInvalidate`，再对队列做一次 `dispatch_sync` 等正在运行的回调结束，然后 `FSEventStreamRelease`；drop 返回之后不再调用 `EventSink`。

`Scope::Entries` 用 kqueue（`rustix` 的 `event::kqueue`，打开 `event` feature），一个 kqueue、一个经 `executor.run_dedicated` 启动的线程，第一个 `Entries` 目标 arm 时才建：

- 目录用 `O_EVTONLY | O_DIRECTORY` 打开（`O_EVTONLY` 不妨碍卷卸载），注册 `EVFILT_VNODE`、`EV_CLEAR`，关注 `NOTE_WRITE`（条目增减、改名）、`NOTE_DELETE`、`NOTE_RENAME`、`NOTE_REVOKE`；后三个报告为 `ROOT_CHANGED`。
- 每个存在的具名条目用 `O_EVTONLY | O_SYMLINK` 打开（是符号链接时监视链接本身，目标由 fs_watch 另外监视），关注 `NOTE_WRITE`、`NOTE_EXTEND`、`NOTE_ATTRIB`、`NOTE_DELETE`、`NOTE_RENAME`、`NOTE_REVOKE`、`NOTE_LINK`。
- 目录的 `NOTE_WRITE` 到达时，对每个具名条目 `fstatat(AT_SYMLINK_NOFOLLOW)`，设备号和 inode 变了（原子改名覆盖）、出现或消失的，重新打开或关闭描述符，报告这个条目。
- 目录的 `NOTE_WRITE` 不说明是哪个条目：`names` 为空时报告为目录本身（空路径）；`names` 不为空时只报告具名条目的变化，其他条目的增减不报告，否则主目录里无关文件的增删也会让配置清单的使用方修订号加一。
- 具名条目打开时另加 `O_NONBLOCK`（条目是 FIFO 时不阻塞）。rustix 没有 `O_EVTONLY` 的名字，按 `sys/fcntl.h` 的值 `0x8000` 传入。
- `Armed::flush`：向 kqueue 发一个 `EVFILT_USER` 事件，等线程处理到它为止：线程收到它之后先读当时的请求序号，再以零超时取完 kqueue 里已有的事件、交付，然后标记这个序号完成；所以 `flush` 调用之前写入引起的事件都已交付。等待上限 2 s，超时或线程已停止时返回 `Other`，fs_watch 按 `NotWatching` 处理。
- 线程在后端 drop 时退出；`kevent` 意外出错而退出时，下一次 `Entries` 的 arm 重建 kqueue 和线程，旧的监视不再交付事件，flush 返回错误。
- 描述符数：每个仓库通常 10 个以内，全部 `Entries` 目标合计上限 256；超过时 `arm` 返回 `Exhausted`，fs_watch 把这个 worktree 标为 `Degraded(Config)`。每个目标在 arm 时按目录加全部 `names`（不论是否存在）预留，具名条目以后出现时不会超出上限，目标 drop 时归还。
- 交付在线程持有状态锁时进行，`Armed` 的 drop 在同一把锁下移除目标、关闭描述符（关闭即注销 kevent），所以 drop 返回之后不再调用这个目标的 `EventSink`。

Linux 上没有真实后端：`platform` 在 Linux 上不提供 `backend`，测试全部用 `FakeBackend`。FSEvents 事件到 `RawEvent` 的转换（去掉前缀、标志的对应）是纯函数，在所有平台上编译，单元测试在 Linux CI 上也运行，所以 `platform` 在所有平台上都依赖本 crate；`lectern-executor` 和 rustix 的 `event` feature 只在 macOS 上依赖。

## 对其他 crate 的要求

- `git`：提供 `Worktree::watch_source(&self) -> Arc<dyn lectern_fs_watch::WorktreeSource>`，持有这个 `Worktree` 句柄（与 `project` 共用同一份 index 解析结果，index 没变时 `tracked` 返回同一个 `Arc`，过滤器不另占一份文件列表的内存）：
  - `filter`：`tracked()` 加上忽略规则。忽略规则在构造时一次读完：`info/exclude`、`core.excludesFile`（或默认位置）、index 里每个名为 `.gitignore` 的路径在工作区里的文件，以及未被忽略的目录里未跟踪的 `.gitignore`（都经 `SafeRoot` 读取）；`ignored` 只做内存里的模式匹配，不按需读文件。未跟踪的 `.gitignore` 由一次目录遍历找出，与工作区比较的第 4 步（[git 设计「工作区比较」](git.md)）是同一个遍历（经 `SafeRoot` 列目录、读 `.gitignore`，[ADR 0028](../adr/0028.md)）：不进入被忽略的目录，受同一个 20 000 个条目的上限，代价与一次未跟踪文件枚举相同，只在构造过滤器时付出；给出 `previous` 时，能证明结果相同就沿用那个过滤器的遍历，不再遍历（「路径分类」末段）。实现细节（遍历没有走到的 index 里的 `.gitignore`、skip-worktree 条目、配置清单的来源路径）见 [git 设计「index」](git.md#index)。agent 先新建 `.gitignore`、还没 `git add` 就生成 `node_modules/**` 时：`.gitignore` 的事件交付并触发重新加载（「路径分类」第 7 步），新的过滤器读到这份未跟踪的规则，这期间的 `node_modules/**` 事件按「路径分类」的暂存机制留住、由新过滤器重新分类后丢弃，不会溢出成 `rescan`。`gitignores` 参数只绕过遍历的条目上限，不绕过上级目录的检查：上级目录被忽略的 `.gitignore`（如 `node_modules/` 里的）任何情况下都不读，与 git 相同。按 `core.ignoreCase`、`core.precomposeUnicode` 比较路径。
  - `config_files`：按「配置文件清单」给出的内容，路径来自 gix 配置节的来源元数据和 `include` 值的解析，不运行 git 命令行、不读 `GIT_*` 环境变量。
  - `WorktreeName` 与 `WorktreeId` 的转换。
  - 这是 [git 设计](git.md)「index」一节「`app` 把 `TrackedPaths` 包成 `fs_watch` 定义的过滤接口传进去」的具体做法：适配放在 `git` 里，`project`、`review` 共用，在 Linux 上可以测试。
- `platform`：按「platform 后端」实现，[platform 设计](platform.md)「本文不覆盖」里的 FSEvents 一项由本文定义。
- `app`：启动屏障放开后创建 `Watcher::new(platform::fs_events::backend(executor.clone()), executor.clone(), Config::default())`，放进 `ProjectDeps::watcher`、`AgentsDeps::watch` 等。
- `project`：`watch_repo` 拿非当前 worktree 的 HEAD 和 `worktree_list`，当前 worktree 用 `watch_worktree`，`WorktreeSpec::source` 来自它的内部接口 `GitAccess::watch_source`（生产实现转调 `Worktree::watch_source`，测试用 `FakeSource`，见 [project 设计「依赖 git 的接口」](project.md#依赖-git-的接口)）；`WorktreeBatch::meta.index` / `head` 即它要的「这一批里 index、HEAD 是否变化」。
- `agents`：每个会话根一个 `watch_dir`，用 `Coalesce::DIR`；根目录不存在时的处理由本 crate 完成，`agents` 只处理 `root` 与 `rescan`。
- `usage`：从 `RepoBatch::reflog` 取记录，按「reflog」里的先后规则判断，把 `seq`、`observed`、`after`（含 `run`）与记录一起落盘，`ReflogReset`（含 `seq`、`observed`）也落盘，第一次见到一个 `RunId` 时为它分配递增的运行序号（ADR 0020「观察先落盘」）；`set_unfiltered` 维护候选 stash 的路径。
- `review`：`paths` → `refresh(Paths)`；`rescan`、`meta` 的任一标志 → `refresh(Full)`；Analyze 用 `flush_revision`。

## 数据流与线程

```
后端线程（FSEvents 队列 / kqueue 线程）
  └─ EventSink::send ─▶ 有界队列 ─▶ fs_watch 线程：分类、修订号、读 reflog、合并、定时
                                        │    ▲                 │
                         提交加载任务 ──┘    └── 加载结果       └─ Sink::deliver ─▶ 消费方提交自己的任务
                                        ▼    │
                         executor Background：过滤器、配置清单（含符号链接展开）
```

- fs_watch 线程只有一个，经 `executor.run_dedicated("fs-watch", …)` 在第一次 `watch_*` 时启动，处理命令（新建、取消订阅、更新不过滤清单、装上加载结果、flush 栅栏）、原始批次和定时器。`Backend::arm` 和 `Armed` 的 drop 在这个线程上；reflog 读取在这个线程上，保证读取依次进行、编号有序。
- 过滤器、配置清单（含符号链接展开）在 executor 的 `Background` 任务里运行，结果作为命令交回 fs_watch 线程；状态在任务运行期间被丢弃时，任务的 `cancel` 置位，结果丢弃。
- UI 线程上不调用 `flush_revision`。其余公开函数只入队或读原子值，UI 线程可以调用，但按架构规则 4，视图不直接调用本 crate，事件由领域层插件消费。
- 首帧之前什么都不做：`Watcher::new` 不建线程、不调用后端，`app` 在启动屏障放开后才创建它；插件在 `AfterFirstFrame` 之后才 `watch_*`（ADR 0017）。

与静止的配合（[kernel 设计](kernel.md)「静止」）：

- 等待事件时不持有守卫。空闲时 kernel 能静止。
- 入队时取守卫：`EventSink::send` 在把原始批次放进队列之前，从这个目标的一个仍然有效的订阅（目标记着订阅它的句柄，取最早登记的一个；它的 `busy()` 返回 None 时试下一个）取一个守卫，随原始批次入队。所以从后端交出事件到 fs_watch 线程分类之间，kernel 不会报告静止。分类时，原始批次开启或延续了订阅的批次，订阅批次先取自己的守卫，再 drop 入队时的守卫；原始批次全部被丢弃（只落在被忽略路径上）时，分类完直接 drop。队列满、原始批次被丢弃时守卫随之 drop，溢出标记由下一次处理时的重新扫描负责，重新扫描开批次时照常取守卫。
- 订阅开一个批次时（第一个要交付的变化到达）调用 `Sink::busy()` 取一个守卫，随批次交给 `deliver`，消费方移进自己的任务。所以从事件到达后端回调、到消费方处理完，中间没有不计入的空档。暂存的路径（过滤器过期期间）同样属于这个批次，守卫一直持有到过滤器装上、批次交付。
- 只落在被忽略路径上的事件不开批次：`node_modules` 的写入只在入队到分类这段时间让 kernel 变忙。这段时间有上界：队列最多 16 384 个事件，分类每个平均 < 1 µs，排空一整个队列 < 20 ms；写入停止后最多这么久 kernel 就回到静止。
- `watch_*` 在调用线程上用 `sink.busy()` 取一个守卫，随命令进入队列，arm 和首次加载完成后放进第一个批次（带 `Live` 的那一批）交给 `deliver`。加载任务（过滤器、配置清单）开始前，为这份状态的每个订阅各取一个守卫，加载结果装上（以及因此交付的批次交付）之后才 drop。所以项目打开后、第一个 `Live` 批次交付之前，kernel 不报告静止。
- 读 reflog 不单独取守卫：读取结果进入批次，由批次的守卫覆盖。

## 性能

| 预算 | 本 crate 的份额（M 系列 Mac） |
|---|---|
| 冷启动 20 ms（ADR 0017） | 首帧之前不调用；`Watcher::new` 只分配一个 `Arc` |
| 空闲内存：空窗口 < 60 MB | 没有打开项目时不建线程、不 arm，常驻 < 64 KB |
| 空闲内存：打开 monorepo < 100 MB | 每个仓库状态 < 256 KB（reflog 进度每个文件约 150 B，1000 个分支约 150 KB；目录缓存 1024 项）；非当前 worktree 不 arm 工作区根，只在公共目录目标里计数。过滤器由 `git` 构造，它的文件列表（与 `project` 的句柄共用的解析后 index）、折叠顺序和忽略规则都是 `git` 的份额（`git/filter_cloudprint_bytes`），不计入本 crate（[ADR 0035](../adr/0035.md)） |
| 空闲内存：10 万文件的仓库 < 120 MB（ADR 0035） | 本 crate 自己的状态不随跟踪文件数增长，17 个 worktree 时同样 < 1 MB；过滤器 ≤ 16 MB 是 `git` 的份额（`git/filter_100k_bytes`） |
| 突发时的内存 | 队列最多 16 384 个事件（约 1.6 MB），每个订阅的批次最多 10 000 个路径（约 1 MB），暂存最多 4096 项（超过时合并到上级目录），每个仓库订阅的批次里 reflog 行最多 1 MiB（超过的分到后面的批次）；都有上限，路径超过时改为 `rescan` |
| 打开项目到项目主页可用 < 100 ms | `watch_*` 只入队；arm 和加载在后台，不在打开路径上 |
| 切换 worktree / 分支 < 100 ms | 切换后的事件：分类每个事件平均 < 1 µs（目录缓存命中），10 万个事件的突发 < 100 ms 的 fs_watch 线程 CPU；交付延迟是后端延迟 20 ms 加合并窗口 |
| Analyze 的修订号 | `flush_revision` 没有积压时 < 5 ms |

空闲时 fs_watch 线程阻塞在条件变量上，没有定时器，不醒来；FSEvents 和 kqueue 没有事件时也不唤醒进程。

基准写在 `crates/bench` 里（[bench 设计](bench.md)「预算表」）：实现对应功能的 PR 在 bench 预算表的分项预算里新增以下条目（出处指向本节），加入 `benches/time/fs_watch.rs` 或内存目标的基准，并把条目改为 `Active`。标注 macOS 的条目用真实后端，在 Linux 上记为 `skipped`：

- `fs_watch/classify_burst`：`FakeBackend` 注入 10 万个 `node_modules/**` 事件和 1000 个受跟踪路径事件，过滤器是 728 个跟踪文件、20 条忽略规则；全部处理完 < 100 ms。
- `fs_watch/flush_idle`（macOS）：真实后端，没有积压时 `flush_revision` < 5 ms。
- `fs_watch/write_to_deliver`（macOS）：写一个受跟踪文件到 `WorktreeBatch` 交付，默认合并参数下 < 120 ms。
- `fs_watch/reflog_append`：追加一行 reflog 到记录进入批次 < 1 ms（不含后端延迟）。
- `fs_watch/resident`：11 个 worktree 的仓库，`watch_repo` 加当前 worktree 的 `watch_worktree`，静止后本 crate 的常驻增量 < 1 MB。只计本 crate 自己的状态：过滤器连同它持有的当前 worktree 解析后 index（`git` 的 `TrackedPaths`，与 `project` 保留的 worktree 句柄共用一份）是 `git` 的份额（ADR 0035），基准先建好句柄和过滤器、单独计数，再从增量里减去。
- `fs_watch/resident_100k`：同上，在 10 万文件、17 个 worktree 的仓库上（[project 设计「性能」](project.md#性能)的大仓库形状），< 1 MB；过滤器在这个仓库上约 14 MB，由 `git/filter_100k_bytes` 计入 `git`。

## 状态与异常

本 crate 没有界面；下表是交给消费方的结果。

| 情况 | 本 crate 的处理 | 消费方 |
|---|---|---|
| `RESCAN`（FSEvents 的 MustScanSubDirs，含 UserDropped / KernelDropped）在工作区根目标上 | 这个 worktree 的订阅 `rescan: true`、`paths` 清空，修订号加一；重新加载过滤器和配置清单（丢失的事件里可能有 `.gitignore`） | `refresh(Full)` 等全量刷新 |
| `RESCAN` 在公共目录目标上 | 仓库订阅 `rescan: true`；每个 worktree 修订号加一、`meta` 全部置位；重新加载全部过滤器和配置清单；对每个 reflog 文件按「reflog」重新 `fstat` 和读取（追加的交付，改写的 `Reset`，新的 `NewFile`，消失的 `Removed`），并重新列出 `logs/` | 重新读引用、HEAD 和 worktree 列表 |
| `RESCAN` 在 `DirWatch` 根目标上 | `rescan: true` | `agents` 重新枚举 |
| fs_watch 队列在某个目标上溢出 | 按这个目标的 `RESCAN` 处理 | 同上 |
| 一个批次的路径超过 10 000 个，或暂存合并到 worktree 根仍超过 4096 项 | `rescan: true` | 同上 |
| 工作区根被删除、改名，或卷被卸载 | `Health::Failed(RootGone / Unmounted)`，drop 工作区根目标，修订号加一；`flush_revision` 返回 `NotWatching` | `project` 由 `worktree_list` 重新列出 worktree |
| 公共目录被删除 | 仓库状态与其下全部 worktree 状态 `Failed(RootGone)` | 关闭或重新打开项目 |
| `arm` 失败 | 对应状态 `Failed(Backend)`；`Entries` 目标的失败只让 worktree `Degraded(Config)` | 显示「不再自动刷新」，由视图层按[交互规范「状态与异常」](interaction.md#状态与异常)处理 |
| `filter` 失败（index 损坏、条目超过上限） | `Degraded(Filter)`：除第 1–3 步丢弃的之外全部交付并计入修订号；下一次触发时重试 | 照常处理，只是事件更多 |
| `config_files` 失败 | `Degraded(Config)`：只按「git 元数据」表监视仓库里的配置；下一次触发时重试 | 照常处理 |
| reflog 读不了（权限、是符号链接） | `ReflogReset { Unreadable }`；下一次事件时重试 | `usage` 记下缺口 |
| `Sink::deliver` panic | `contain` 截住，这个订阅不再交付，记日志 | — |
| FSEvents 的 `EventIdsWrapped`、`HistoryDone` | 丢弃 | — |
| 系统时间被调整 | `ReadStamp::at` 可能不单调；同一运行内按 `read` 判断先后，跨运行按 `usage` 的运行序号 | — |

已知限制：

- 网络卷（SMB、NFS）上 FSEvents 不报告其他机器的写入，`arm` 照常成功，状态仍是 `Live`。
- 子模块内部的 HEAD 变化（`<公共目录>/modules/<名字>/HEAD`）不监视；子模块目录本身的增删照常交付。
- `git status` 这类只刷新 stat 信息的 index 改写同样让修订号加一，Analyze 可能把内容没变的结果标为「基于较早的版本」。
- `FSEventStreamFlushSync` 交付的是 FSEvents 服务已经收到的事件；写入在 `flush_revision` 之前几毫秒内完成、内核还没交给 FSEvents 服务的，这一次可能看不到。Analyze 同时比较指纹（ADR 0019 约束 9），内容的变化仍能发现。
- 条件不满足的 `includeIf` 引入的文件里再嵌套的引入，条件满足、清单重新计算之前不监视（「配置文件清单」）。

## 测试

全部在 Linux 上运行（`just test -p lectern-fs-watch`），用 `testing::manual`、`FakeBackend`、`FakeSource` 和 `Executor::manual()`，时钟是假的，结果确定。reflog 读取、符号链接展开用 `tempfile` 的真实目录。批次序列和 reflog 记录用 insta 快照验收。

- 路径分类（`filter_*`）：「路径分类」每一步一个用例：受跟踪文件、未跟踪未忽略文件交付；`node_modules/**` 丢弃；被忽略目录里的受跟踪文件交付；自身被忽略规则排除的 `.gitignore`、`.gitattributes` 交付并置 `rules`，被忽略目录里的（`node_modules/x/.gitignore`）丢弃、不触发重新加载；主 worktree 的 `.git/**` 和嵌套仓库的 `.git/**` 丢弃；子模块内部丢弃、子模块路径本身交付；不过滤清单里的被忽略路径和它们的上级目录交付；不满足 `WatchPath` 不变量的路径丢弃；目录缓存在过滤器替换后失效；只报告目录本身的改名或删除，目录里有 `rule_files` 中的文件、或事件带 `DIR` 与 `RENAMED` 时都触发重新加载。
- 过期的过滤器（`filter_stale_*`）：`index` 变化后、新过滤器装上之前，旧过滤器认为被忽略的路径暂存，装上后按新过滤器交付（`git add -f` 的场景）；新建未跟踪的 `.gitignore` 后紧接着写出 1 万个 `node_modules/**` 事件，重新加载时 `gitignores` 带上这个路径，这些事件被暂存（超过 4096 项时合并为 `node_modules/`）后丢弃，不变成 `rescan`；暂存期间修订号照样加一；第一次加载完成前的事件不丢；合并到 worktree 根仍超过上限时改为 `rescan`；加载失败为 `Degraded(Filter)` 并全部交付。
- 沿用的过滤器（`filter_reuse_*`）：`index` 变化的重新加载收到在用的过滤器作为 `previous`（`FakeSource::previous` 给出是第几次加载构造的），连续的也是；`.gitignore`、规则文件的上级目录、目录改名、`info/exclude`、配置、两种 `rescan` 触发的都没有；一次 index 加载期间来了 `.gitignore` 事件又来了 index 变化时，合并的那一次没有，之后只有 index 变化的又有；第一次加载之前暂存了 `.gitignore`、又排上了 index 变化时都没有；失败的加载之后没有。「不确定」标记的三种情形各一个：加载在途时旧过滤器把新的 `out/.gitignore` 判为丢弃（`filter_reuse_not_after_a_rule_file_the_old_filter_dropped`），失败的过滤器之上的加载期间移进来一个目录（`…_events_during_a_load_over_a_failed_filter`），第一次加载之前 5000 个事件合并成目录、其中有 `.gitignore`（`…_an_overflow_before_the_first_filter`）：之后的第一次 index 加载都没有 `previous`，再下一次又有；去掉标记时这三个都失败。`git` 一侧的端到端测试（`watch_source_events_*`）用这个接口和真实的过滤器：index 变化之后新建、修改未跟踪目录里的 `.gitignore`，移进来一个含 `.gitignore` 的目录，之后的事件都按新规则分类；用 `Driver::pump` 重现上面第一种竞争，之后 index 变化时 `out/a.log` 按新读到的 `out/.gitignore` 丢弃（[git 设计「index」](git.md#index)）。
- 修订号（`revision_*`）：「修订号」每一条、「git 元数据」表每一行一个用例，各自恰好加一；同一原始批次里多个事件只加一；只有被忽略路径、`.lock` 文件、`objects/` 的批次不加一；另一个 worktree 的 HEAD 不让这个 worktree 加一，`refs/` 让全部 worktree 加一；配置清单里的文件、它的符号链接目标变化时加一；重新监听后 `epoch` 不同。
- flush（`flush_*`）：`FakeBackend::hold` 留住的事件在 `flush_revision` 之后计入；`flush_revision` 不提前交付正在合并的批次；在 `Sink::deliver` 里调用返回 `WouldDeadlock`；状态失败后返回 `NotWatching`；arm 还没完成时等 arm；超时返回 `Timeout`。
- 合并（`coalesce_*`）：`quiet` 内的事件合成一批；连续事件在 `max_delay` 时交付；两个句柄用不同参数各自交付；路径超过上限改为 `rescan`；`health` 变化立即交付；没有等待中的批次时不设定时器。
- reflog（`reflog_*`）：追加一行得到一条 `Appended` 记录，`seq`、`observed`、`after` 符合「reflog」的规则；两个文件交替追加时先后规则给出的判断都成立，判断不了的不判断；不完整的行等换行；变短、改写末尾为 `Rewritten`；删除为 `Removed`；新分支和改名后的分支为 `NewFile`、`after` 为 None；`worktrees/<名字>/logs/HEAD` 为 `Head(Linked)`；`logs/refs/stash`；超过 1 MiB 的追加和 3 MiB 的改名分支分到多个批次，每批不超过 1 MiB、`seq` 连续、接着读的部分 `after` 与第一批相同；一批里追加、重置、再追加时按 `seq` 合并 `reflog` 与 `reflog_resets` 得到正确的先后；超过 64 KiB 的行跳过；arm 之前的记录不交付；第 1 段与 arm 之间追加的行由补读交付且只交付一次，`FakeBackend` 在 arm 时注入的同一文件事件不让它重复；两个 `Watcher`（不同 `run`）的记录之间不按读取编号和偏移判断先后：前一个 `Watcher` 关闭后把文件改写成更短的内容、再追加，后一个运行的记录在运行序号下晚于前一个运行的全部记录，偏移更小也不判为更早，`NewFile` 记录不参与跨运行判断；同一运行内 `ReflogReset` 前后的记录不按偏移判断；`reftable/tables.list` 存在时没有记录；公共目录 `RESCAN` 后追加的记录照常交付。
- 配置清单（`config_*`）：清单里的文件按位置分到公共目录目标、工作区根目标和 `Entries` 目标；同一目录的文件共用一个 `Entries` 目标；不存在的文件监视最近的上级目录，逐级出现后下移；符号链接的每一级和规范路径都被监视；上级目录是链接（`~/.config -> dotfiles/config`）时，换掉这个链接、在链接后面的目录里新建原本不存在的 `git/config` 都让修订号加一并重新展开；超过 40 个链接时按不存在处理；工作区里被忽略的配置文件在过滤之前被识别；清单变化时只 arm 新增、drop 多余的目标；`HEAD` 变化触发重新计算；`config_files` 失败为 `Degraded(Config)`。
- 目录监听（`dir_*`）：路径为绝对路径、按 250 ms 合并；根不存在时监视上级目录的 `Entries`，根出现后交付 `Appeared` 和 `rescan`；根被删除后交付 `Vanished` 并回到上级目录。
- 异常（`rescan_*`、`health_*`）：「状态与异常」表的每一行；队列溢出按 `RESCAN` 处理；`Sink` panic 后其他订阅照常。
- 生命周期（`lifecycle_*`）：`Watcher::new` 不建线程、不调用后端；第一次 `watch_*` 才启动；同一公共目录、同一 worktree 共享一份状态和一个后端目标；最后一个句柄 drop 后目标被 drop；`shutdown` 之后不再交付。
- 静止（`quiescence_*`）：用 kernel 的 `TestKernel` 取真实的守卫：批次合并期间不静止，交付后守卫在消费方手里、drop 后静止；`EventSink::send` 返回之后、fs_watch 线程分类之前不静止；只有被忽略路径的事件在分类完后不再让 kernel 忙碌；加载任务进行中不静止；空闲时静止。

`git` 的适配在 `git` 里测试（`watch_source_*`，用 `TestRepo`）：`.gitignore`、`info/exclude`、`core.excludesFile` 的忽略结果与 git 的判定一致；未跟踪目录里和根目录下未跟踪的 `.gitignore` 生效，被忽略目录里的 `.gitignore` 不读；遍历达到上限时显式给出的 `.gitignore` 路径仍然生效，显式给出、但上级目录被忽略的不读；目录改名后 `rule_files` 给出新的位置；`core.ignoreCase` 下大小写不同的路径判为受跟踪；`config_files` 列出 `include.path`、`includeIf "onbranch:"`（满足与不满足）、不存在的引入目标和 XDG 默认位置；不调用 git 命令行。

`platform` 的后端只能在 macOS CI 上测试（`fsevents_*`、`kqueue_*`）：真实临时目录里的新建、修改、改名、删除都报告为相对路径；`/tmp` 这类经符号链接的根去掉规范前缀；`flush` 交付延迟中的事件；drop 之后不再回调；根目录被删除报告 `ROOT_CHANGED`；kqueue 对就地写入、原子改名覆盖、具名条目是符号链接（不跟随）、目录被删除各有用例。FSEvents 事件的转换（`fsevents_convert_*`：前缀、标志、挂载、丢弃的事件）是纯函数，在所有平台上运行。

## 未决问题

没有需要产品决定的问题，技术问题都已在上文决定。

## 验收

分四步，每步一个 PR，都写 `Refs #14`（#14 由 `review_view` 最后完成时关闭，见 [git 设计](git.md)「验收」）。每一步都要 `just check` 通过。

1. **本 crate**（`lectern-fs-watch`）
   - `just test -p lectern-fs-watch` 里 `filter_*`、`filter_stale_*`、`revision_*`、`flush_*`、`coalesce_*`、`reflog_*`、`config_*`、`dir_*`、`rescan_*`、`health_*`、`lifecycle_*`、`quiescence_*` 通过，快照已逐个看过。
   - `cargo tree -p lectern-fs-watch -e normal --depth 1` 只列出 `lectern-kernel`、`lectern-executor`。
   - `rg -n 'cfg\(target_os|Command::new|std::process' crates/fs_watch/src` 没有结果；`python3 scripts/check_architecture.py` 通过。
   - bench 预算表新增「性能」一节的全部 `fs_watch/*` 条目；`fs_watch/classify_burst`、`fs_watch/reflog_append` 改为 `Active`，macOS CI 的 `just bench` 判定通过，其余条目为 `planned`。这一条和两项基准在 bench 运行器合并之后由 [#92](https://github.com/SoloJiang/lectern-app/pull/92) 加入（见「与设计的差异」）。
2. **`git` 的适配**（`lectern-git`）
   - `just test -p lectern-git` 里 `watch_source_*` 通过；`rg 'Command::new' crates/git/src` 没有结果。
   - [git 设计](git.md)的状态行与「index」一节更新为已实现。
3. **macOS 后端**（`lectern-platform`）
   - macOS CI 上 `just test -p lectern-platform` 的 `fsevents_*`、`kqueue_*` 通过；Linux 上 `cargo check -p lectern-platform` 通过。
   - `rg -l 'FSEventStream' crates` 只列出 `crates/platform/` 下的文件；`python3 scripts/check_architecture.py` 通过，`just deny` 通过（没有新增 crate）。
   - [platform 设计](platform.md)的状态行更新。
4. **装配与实测**（`app`）
   - `app` 按「对其他 crate 的要求」创建 `Watcher` 并注入；macOS 上 `cargo build --locked -p lectern-app` 通过。
   - `fs_watch/flush_idle`、`fs_watch/write_to_deliver`、`fs_watch/resident`、`fs_watch/resident_100k` 改为 `Active`，macOS CI 的 `just bench` 判定通过；`app/cold_start` 不因本 crate 变化。
   - 本文状态行改为「已实现」并列出四个 PR，填写「实际验证」。

## 实际验证

第 1 步，在 Linux 上运行（本 crate 不依赖 GPUI，也没有平台相关代码，macOS 上是同一份代码；本 PR 没有在 macOS 上实测）：

- `just test -p lectern-fs-watch`：103 个测试全部通过，连续运行 15 次结果相同。测试用 `testing::manual`、`FakeBackend`、`FakeSource`、`Executor::manual()` 和假时钟，reflog 与配置清单的符号链接展开用 `tempfile` 的真实目录；只有三个测试用真实线程：`lifecycle_new_starts_no_thread_and_calls_no_backend`（用 executor 的线程启动钩子计数）、`flush_times_out_while_the_fs_watch_thread_is_busy` 和 `quiescence_*` 用的 `TestKernel`。
- 「测试」一节的各组都有对应用例（`crates/fs_watch/src/tests/`）：`filter.rs`（`filter_*` 13 个、`filter_stale_*` 7 个）、`revision.rs`（`revision_git_metadata_table` 把「git 元数据」表的每一行连同 `.lock`、`objects/`、`FETCH_HEAD` 等表外路径做成一张快照，逐行核对加一的 worktree、重新加载的过滤器与配置清单和批次标志）、`flush.rs`、`coalesce.rs`、`reflog.rs`（先后规则按「reflog」一节写成测试里的 `earlier`，按真实写入顺序核对每一对记录的判断，并确认存在判断不了的对）、`config.rs`、`dir.rs`、`health.rs`（`rescan_*`、`health_*`）、`lifecycle.rs`、`quiescence.rs`；暂存的合并、目录缓存和 reflog 路径映射另有模块内的单元测试。insta 快照（大多是内联快照，另有 `snapshots/` 下一份）逐个看过。
- `cargo tree -p lectern-fs-watch -e normal --depth 1` 只列出 `lectern-kernel`、`lectern-executor`；`rg -n 'cfg\(target_os|Command::new|std::process' crates/fs_watch/src` 没有结果；`python3 scripts/check_architecture.py` 通过。
- `just check`、`just test`（Linux 上的全部 crate，774 个测试）、`just doctest`、`just deny` 通过。
- 性能（临时的计时测试，`--release`，Linux 容器，不是 M 系列 Mac，未提交）：`classify_burst` 的场景（728 个跟踪文件、20 条忽略规则，`FakeBackend` 注入 10 万个 `node_modules/**` 事件和 1000 个受跟踪路径事件，每 1000 个一个原始批次）全部处理完最好一次 41.5 ms，含 `FakeBackend` 构造事件的开销；追加一行 reflog 到记录进入批次（`reflog_append`）中位数 5.9 µs、p95 8.0 µs。

没有验证到的：

- bench 预算表的 `fs_watch/*` 条目和 `classify_burst`、`reflog_append` 两项基准：#86 时 bench 运行器（[#7](https://github.com/SoloJiang/lectern-app/issues/7) 第 1 步）还没有合并进 `main`，后由 [#92](https://github.com/SoloJiang/lectern-app/pull/92) 加入，结果见下（「与设计的差异」）。
- macOS 上的真实后端、`flush_idle`、`write_to_deliver`、`resident`：第 3、4 步。

第 1 步的基准（[#92](https://github.com/SoloJiang/lectern-app/pull/92)，`crates/bench/benches/time/fs_watch.rs`），在 Linux 上运行（x86_64 容器，4 核，不是 M 系列 Mac；预算由 macOS CI 的 `just bench` 判定，本 PR 没有在 macOS 上运行）：

- `just bench --measure fs_watch/`，4 次运行，退出码都是 0：

  | ID | 中位数 | p95 | 样本数 | 预算 |
  |---|---|---|---|---|
  | `fs_watch/classify_burst` | 19.1 / 19.5 / 19.7 / 23.8 ms | 30.7–36.3 ms | 40–48 | < 100 ms |
  | `fs_watch/reflog_append` | 6.8 / 7.0 / 10.6 / 10.9 µs | 10.9–13.7 µs | 10 000 | < 1 ms |

  各有一次运行标为 `noisy`（`classify_burst` 的 MAD 4.7 ms，`reflog_append` 的 MAD 1.1 µs），其余没有。
- `classify_burst`：728 个跟踪文件（`packages/pNN/src/fileNNN.ts`，与 `git/*` 相同）、20 条忽略规则（`node_modules/` 排在第 19 条，`FakeSource` 的匹配要走过大半张表）；100 个原始批次，每批 1000 个 `node_modules/**` 文件事件（2000 个包、每包 50 个文件分在 4 个目录里，每 4 个包有一个带 scope，目录数远超 1024 项的目录缓存）加 10 个受跟踪路径事件（1000 个事件依次落在 728 个文件上）。每个原始批次到达后就由 `Driver::run_until_idle` 处理（像 fs_watch 线程一样边收边处理，队列不溢出），最后推进假时钟让合并窗口结束、交付批次，这些都在计时之内；原始批次在计时之外构造，经 `FakeBackend::emit_batch` 注入，所以比第 1 步临时测量的 41.5 ms（含 `FakeBackend` 构造事件）低。每个样本都检查恰好交付一个批次、含 728 个受跟踪路径、没有 `rescan`。
- `reflog_append`：追加一行到 `logs/HEAD` 在计时之外（与 `agents/append_4k` 相同）；计时的是 `emit_batch` 注入后端事件、`run_until_idle` 处理到记录进入仓库订阅的批次（入队、读新增的字节、打时刻和序号、并入批次）。下一个样本之前推进假时钟交付批次，逐条检查记录的文件、`Origin::Appended` 和行内容，最后核对记录数等于追加的行数。与第 1 步临时测量的中位数 5.9 µs 相近。
- `just bench`（冒烟）退出码 0：`fs_watch/*` 的 2 条生效条目是 `smoke`，3 条是 `planned`。`just check`、`just test`（Linux 上的全部 crate，843 个测试）、`just deny` 通过；`Cargo.lock` 只多了 `lectern-bench` 对 `lectern-fs-watch` 的一条工作区内部依赖。

第 3 步（`lectern-platform` 的 `fs_events`），在 Linux（x86_64）上完成；本 PR 没有在 macOS 上运行过，macOS 上的行为全部由 macOS CI 验证：

- 交叉检查：`cargo check -p lectern-platform --target aarch64-apple-darwin --all-features --tests` 和 `cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 通过。macOS 的代码（手写的 15 个 CoreFoundation / CoreServices / libdispatch 符号、rustix 的 kqueue、`O_EVTONLY` / `O_SYMLINK`、`dev_t` 为 `i32`、`mode_t` 为 `u16`）和 macOS 上的测试因此通过了类型检查和 clippy（每个 `unsafe` 块都有 `// SAFETY:`），但没有链接、没有运行：符号名和结构体布局是否与系统框架一致、FSEvents 的实际行为，要到 macOS CI 才能确认。
- 在 Linux 上运行的：`just test -p lectern-platform` 36 个测试通过，其中新增的 6 个 `fsevents_convert_*` 覆盖前缀去除（`/`、尾部斜杠、共享前缀的兄弟目录、未解析的 `/tmp` 写法、非 UTF-8 字节）、每个 FSEvents 标志的对应、`RootChanged`、目标之外的路径、丢弃的事件、挂载与卸载、`EventIdsWrapped` / `HistoryDone`。`just check`、`just test`（Linux 上的全部 crate，780 个测试）、`just doctest`、`just deny`、`python3 scripts/check_architecture.py` 通过；Linux 上的 `cargo check -p lectern-platform` 通过，`fs_events` 在 Linux 的非测试构建里不存在。
- 只在 macOS CI 上运行的 20 个测试：`fsevents_*` 9 个（新建、修改、改名、删除为相对路径；`/tmp` 与自建符号链接的根；120 s 延迟的流上 `flush` 交付；drop 之后不再回调；根目录被删除、被改名为 `ROOT_CHANGED`；不存在的路径为 `NotFound`；两个流共用队列互不串扰，drop 一个另一个照常；经真实 `Watcher` 的 `watch_dir`：根不存在时经 kqueue 等到它出现、`Appeared`，FSEvents 报告根里的文件，删除根后 `Vanished`），`kqueue_*` 11 个（就地写入；原子改名覆盖后报告并改为监视新文件；具名条目是符号链接时不跟随，换掉链接才报告；目录被删除、被改名为 `ROOT_CHANGED`；具名条目出现、变化、消失；没有 `names` 时报告目录本身；不存在的目录为 `NotFound`；描述符上限与归还；drop 之后不再回调；两个目标共用线程互不串扰）。FSEvents 是异步的，测试都按最长 30 s 的期限等待（每次等待之间 `flush`，FSEvents 的流先写探针文件直到报告出现），不以固定的 sleep 作为判断依据；只有「drop 之后不再回调」在 drop 后继续写一段固定时间，机器慢只会让它检查得少，不会误报。
- 验收的检查：`rg -l 'FSEventStream' crates` 只列出 `crates/platform/src/fs_events/` 下的文件（`crates/fs_watch/AGENTS.md`、`crates/agents/AGENTS.md` 里提到 `FSEventStreamFlushSync` 的两句改为不写符号名）；`Cargo.lock` 只多了 `lectern-platform` 对 `lectern-fs-watch`、`lectern-executor`、`lectern-kernel`（dev）三条工作区内部依赖，没有新增 crate；`cargo tree -p lectern-platform -e normal --depth 1` 在 Linux 上列出 `lectern-fs-watch`、`rustix`，`--target aarch64-apple-darwin` 另有 `lectern-executor`。

macOS CI 要确认的：

- `just test -p lectern-platform` 里 `fsevents_*`、`kqueue_*` 全部通过（即验收第 3 步的第一条）；尤其是 FSEvents 在根目录被删除、被改名时确实报告 `RootChanged`，以及 FSEvents 在流刚启动时报告事件的延迟在测试的期限之内。
- 测试二进制能链接（`CoreServices`、`CoreFoundation` 两个框架和 libdispatch 的符号）。

`FilterRequest::previous`（只有 index 变化时沿用在用的过滤器，[git 设计「index」](git.md#index)，`Refs #14`），Linux：

- `just test -p lectern-fs-watch`：111 个测试通过，新加 `filter_reuse_*` 8 个（见「测试」）。原来的 `filter_*`、`filter_stale_*` 没有改动，照常通过。
- 自审发现的竞争（「路径分类」末段的「不确定」标记）：只按触发的种类决定 `previous` 时，加载在途时被旧过滤器判为丢弃的新规则文件之后一直不会被读到，直到下一次不是 index 的重新加载；改动之前下一次 index 变化的完整遍历会补上它。现在这三种情形之后的第一次 index 加载完整进行，`filter_reuse_not_after_*` 和 git 的 `watch_source_events_rule_file_missed_during_a_load_is_read_by_the_next_index_load` 在去掉标记时失败。
- 接口改为 `filter(&FilterRequest, ..)`：`git`、`code_intel` 的 `LazySource`、`FakeSource` 和 bench 的调用一并改了。`git/filter_100k` 的数字见 git 设计「实际验证」。

## 与设计的差异

- 「路径分类」第 6 步：上级目录不是子模块且被忽略规则排除时就丢弃，不再要求它不受跟踪。设计原来只丢弃「不受跟踪且被忽略」的上级目录，于是 `build/` 被忽略、`build/keep.txt` 受跟踪时，`build/out.js` 会走到第 8 步，而 `PathFilter::ignored` 只看路径自身（`out.js` 不匹配），结果被当作未跟踪路径交付；git 不重新包含被排除目录之下的路径，`git status` 不会列出它。受跟踪的路径在第 5 步已经交付，所以改动只影响未跟踪的路径。上文第 6 步已改。
- `RunId` 由墙上时间和 `RandomState` 的每进程随机密钥求哈希得出，不用进程号：取进程号要用 `std::process`，验收要求 `rg 'std::process' crates/fs_watch/src` 没有结果；随机密钥同样区分同一时刻启动的两个进程。另有一个进程内计数器区分同一纳秒创建的两个 `Watcher`。上文 `ReadStamp::run` 已改。
- 第一个批次的 `health` 是当时的状态，不总是 `Live`：首次加载失败时是 `Degraded`，arm 失败时是 `Failed`，否则消费方要先收到 `Live` 再立即收到退化的状态。上文「监听器与句柄」已改。
- 按事件路径触发的过滤器重新加载（`.gitignore`、规则文件的上级目录、目录改名或删除）只看按当前过滤器不被丢弃的路径；设计只对 `.gitignore` 写明了这一点（`node_modules/x/.gitignore` 不触发）。否则 `npm install` 在 `node_modules` 里不断改名目录，每次都触发重新加载，过滤器一直过期，批次一直不交付。第一次加载完成之前暂存的路径没有过滤器可判断，在第一个过滤器装上后按它判断，判为交付的再触发一次（第一次加载的遍历可能已经错过它们）。上文已补充。
- 工作区根目标上的 `RESCAN` 同时重新加载过滤器和配置清单：丢失的事件里可能有 `.gitignore` 或配置文件，不重新加载的话过滤器会一直是旧的。上文「状态与异常」已补充。
- 目录缓存用两代缓存近似最近使用淘汰，每次操作 O(1)，不需要链表；上限仍是 1024 项。上文已改。
- 暂存超过 4096 项时，先在最深的一层里按上级目录分组，从最大的组开始合并，够了就停；只有一个成员的组要等更大的组都合并完仍超限时才上移一层。设计只写了「从最深的目录开始」。
- 配置清单的监视点在 `Tree` 目标里也匹配监视点之下的路径：监视点是「最长的已存在前缀的下一级名字」时，后端可能只报告新出现的目录里的文件。
- 过滤器加载失败（`Degraded(Filter)`）后的重试期间，路径照样全部交付（没有过滤器可暂存），但批次与正常的重新加载一样等到加载结束再交付。
- `flush_revision` 里某个 `Armed::flush` 出错时返回 `NotWatching(Failure::Backend(..))`，调用方按「已变化」处理；设计没有写这种情况。
- 最后一个 `Watcher` 克隆 drop 时也执行 `shutdown`；`shutdown` 在 fs_watch 线程之外调用时等线程退出（最多 `flush_timeout`），所以返回之后不再交付。
- 测试辅助比设计多了几个方法（上文「测试辅助」已列出）：`FakeBackend` 的 `emit_on_arm`、`before_arm`、`is_armed`、`history`，`FakeSource` 的设置与计数方法，`RecordingSink::len`、`is_empty`；`RecordingSink` 在 `take` 之前一直持有守卫，静止测试据此检查「交付后守卫在消费方手里」。另外 `Flags` 实现了 `BitOr`、`BitOrAssign`、`Default`、`Hash`，`Target`、`Scope`、批次类型实现了 `Clone`、`PartialEq`、`Debug`，错误类型实现了 `Display` 和 `Error`。
- bench 预算表的 `fs_watch/*` 条目和 `classify_burst`、`reflog_append` 两项基准没有随第 1 步（#86）加入：当时 bench 运行器与预算表（[#7](https://github.com/SoloJiang/lectern-app/issues/7) 第 1 步）还没有合并进 `main`。运行器合并（[#83](https://github.com/SoloJiang/lectern-app/pull/83)）后由 [#92](https://github.com/SoloJiang/lectern-app/pull/92) 加入：五个条目都在预算表里，这两项为 `Active`，`flush_idle`、`write_to_deliver`、`resident` 为 `planned`（第 4 步）；`resident` 属于内存目标。为了在计时之外构造事件，`testing::FakeBackend` 多了 `emit_batch`（上文「测试辅助」已列出）。实测值见「实际验证」。
- 第 3 步：FSEvents 事件的转换（去掉前缀、标志的对应）放在 `fs_events::convert`，是纯函数，在所有平台上编译，单元测试在 Linux CI 上也运行；所以 `platform` 在所有平台上依赖 `lectern-fs-watch`（只为 `Flags`、`RawEvent`），`lectern-executor` 和 rustix 的 `event` feature 只在 macOS 上依赖。`backend` 仍然只在 macOS 上存在。上文「platform 后端」已补充。
- 第 3 步：标志对应补了设计没写的情况：`ROOT_CHANGED`、`UNMOUNTED` 总在空路径上（fs_watch 只在目标本身上读这两个标志）；挂载点在目标之下的 `Unmount` 与 `Mount` 一样是挂载点所在目录的 `RESCAN`，挂载点是目标或在它之上时才是 `UNMOUNTED`；带 `MustScanSubDirs` 的事件路径不在目标之下时按目标本身的 `RESCAN` 处理，不当作「前缀不匹配即 `ROOT_CHANGED`」，否则丢弃事件会被当成根目录消失。上文已补充。
- 第 3 步：kqueue 的目录 `NOTE_WRITE` 不说明是哪个条目，`names` 不为空时只报告具名条目的出现、替换和消失，不报告其他条目的增减；`names` 为空时报告为空路径（目录本身）。设计的 `Scope::Entries` 写的是「目录本身的条目增减、改名」都报告，但对 kqueue 来说只能报告成目录本身，fs_watch 收到空路径会通知这个目录的全部使用方，主目录里任何文件的增删都会让配置清单的使用方修订号加一。fs_watch 的 `dir_*` 测试本来就期望「上级目录里其他条目的事件什么都不做」。上文已补充。
- 第 3 步：具名条目自己的描述符收到 `NOTE_DELETE`、`NOTE_RENAME`、`NOTE_REVOKE` 时，即使 `fstatat` 得到相同的设备号和 inode 也重新打开（新文件可能拿到相同的 inode 号）；具名条目打开时加 `O_NONBLOCK`；`O_EVTONLY` 在 rustix 里没有名字，按 `sys/fcntl.h` 的值传入。
- 第 3 步：kqueue 的 `flush` 最多等 2 s（与 fs_watch 默认的 `flush_timeout` 相同），超时或线程已停止时返回 `BackendError::Other`；描述符上限按每个目标的目录加全部 `names`（不论是否存在）在 arm 时预留，drop 时归还，具名条目以后出现时不会超出上限；kqueue 线程在后端 drop 时退出，`kevent` 意外出错退出后，下一次 `Entries` 的 arm 重建 kqueue 和线程。设计没有写这几处。上文已补充。
- 第 3 步：测试比设计多了 `fsevents_convert_*`（所有平台）、`fsevents_root_renamed_reports_root_changed`、`fsevents_arm_missing_path_is_not_found`、`fsevents_streams_share_one_queue_and_stay_apart`、`fsevents_watcher_dir_appears_changes_and_vanishes`（经真实 `Watcher` 和 `EventSink`，为它在 macOS 上加了 `lectern-kernel` 的 dev 依赖，实现 `Sink` 要用 `BusyGuard`），以及 kqueue 的目录改名、具名条目出现与消失、没有 `names`、不存在的目录、描述符上限、drop、两个目标各一个用例。测试经 `MacBackend::arm_with` 把批次交给一个记录用的闭包，不经 fs_watch 的队列；`Backend::arm` 只是把 `EventSink::send` 包成同样的闭包。
