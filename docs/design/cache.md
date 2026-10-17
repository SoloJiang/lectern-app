# cache 设计

状态：实现中。第 1 步已实现（存储、键值接口与生命周期，[#82](https://github.com/SoloJiang/lectern-app/pull/82)），第 2 步尚未实现（见「验收」）。

对应 issue [#17](https://github.com/SoloJiang/lectern-app/issues/17)、[#31](https://github.com/SoloJiang/lectern-app/issues/31)（`crates/cache/AGENTS.md` 列出的两个 issue；本 crate 是它们的前置，不单独关闭其中任何一个）。`cache` 是派生数据的本地持久缓存：解析、diff、索引这类可以从 git 对象或会话日志重新算出的结果，以内容的身份为键存进 SQLite，下次启动、切换 worktree 时直接命中（架构规则 5，[技术调研「性能要点」](tech-research.md#性能要点)）。存储选型见 [ADR 0027](../adr/0027.md)。

## 范围

- 做：
  - 两类分库：项目分库，每个仓库（按 git 公共目录区分，同一仓库的 worktree 共用）或文件夹项目一个；应用级分库，只有一个，不属于任何仓库。
  - 键值接口：按命名空间和键读写字节值，值的编码由使用方的类型实现；多条写入在一个事务里提交。
  - 按字节的 LRU 淘汰：每个分库一个预算，所有分库的文件另有一个总预算。
  - 生命周期：首帧之后第一次使用时才打开；版本不符、文件损坏时删除重建；磁盘写满、被其他进程占用时放弃写入，不让调用方失败。
  - 内存上限：连接数和每个连接的页缓存（[ADR 0017](../adr/0017.md)）。
- 不做：
  - 进程内的内存缓存（hunk 的 16 MiB LRU、清单解析缓存、轮次索引的 8 MB LRU）：各使用方自己维护，本 crate 只管磁盘上的一层。
  - `usage` 的用量记录和游标：会话日志删除后只有它们还在，不是派生数据，不能删除重建。`usage` 用自己的数据库（应用数据目录下，不在本 crate 的目录里），同样经 rusqlite（ADR 0027）；记录和游标在同一个事务里落盘由 usage 的设计文档规定。
  - `project` 的清单解析结果：解析一个清单是微秒级，比一次 SQLite 查询还快，只在内存里按 blob OID 缓存（[project 设计](project.md)「依赖」）。
  - 压缩会话的解压副本：普通文件，由 `agents` 管理目录和它的 1 GB 字节 LRU（[agents 设计](agents.md)「压缩会话的随机读取」）。
  - 缓存键的含义和有效性判断：使用方负责。例如轮次索引是否仍有效由 `agents` 按值里的游标判断（[ADR 0026](../adr/0026.md)）。
  - 应用缓存目录的位置：`platform` 提供，`app` 传入（见「接口」）。
- 本 issue 之外：
  - `code_intel` 的符号索引（[#17](https://github.com/SoloJiang/lectern-app/issues/17)）：按名字查询需要自己的表和索引，键值接口不够。code_intel 的设计文档确定查询之后，在本文增加「自定义表」一节，表仍在仓库分库里、计入同一个字节预算、可以删除重建。
  - Analyze 的模型结果和基准快照（[ADR 0019](../adr/0019.md)、[#66](https://github.com/SoloJiang/lectern-app/issues/66)）：结果按完整分析请求的哈希存进仓库分库的 `analyze.result` 命名空间（本文预留名字）；快照是磁盘映像里的目录树，带引用计数，它的字节 LRU 随 #66 设计，不进 SQLite。

本 crate 在基础设施层，是普通的库 crate，不是插件：不实现 `Plugin`，不注册服务。`app` 创建一个 `Cache`，在构造插件时传入（[ADR 0025](../adr/0025.md)「进程级的共享资源」）；插件 crate 和领域 crate 直接依赖本 crate。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `rusqlite` =0.40.2（`default-features = false`，`bundled`、`cache`） | SQLite 连接、事务、预编译语句缓存 | MIT；静态链接自带的 SQLite 3.53.2，约 2 MB；理由与放弃的方案见 [ADR 0027](../adr/0027.md) |
| `sha2` 0.11 | 由分库对应的路径算出文件名 | 已在 Cargo.lock（gix-hash 依赖同一版本），不新增 crate |
| `ruzstd` 0.9 | 命名空间选择压缩时的编码与解码 | 已是工作区依赖（[ADR 0026](../adr/0026.md)），纯 Rust |
| `tempfile`（dev） | 测试用的缓存目录 | 已在 Cargo.lock |

不依赖 `executor`、`kernel` 和 `platform`：所有接口都是阻塞的普通函数，由调用方放进自己的后台任务；本 crate 不创建线程、不提交任务、没有定时器。应用缓存目录由 `app` 从 `platform` 取得后传入。

## 接口

```rust
/// 进程里唯一的缓存。`Clone + Send + Sync`（`Arc<Inner>`）。`app` 在启动屏障放开后创建，交给需要它的 crate。
#[derive(Clone)]
pub struct Cache { /* Arc<Inner> */ }

pub struct Config {
    /// 本 crate 独占的目录：`app` 传 `platform` 提供的应用缓存目录下的 `cache/`。不存在时第一次打开分库才创建。
    pub root: PathBuf,
    pub budgets: Budgets,
    /// UI 线程的 ID。设置后，在这个线程上调用任何阻塞方法：debug 构建 panic；release 构建不做 IO，
    /// 读取返回 None，写入返回 `Written::Dropped(Dropped::WrongThread)`，并计入 `Stats`。
    pub ui_thread: Option<ThreadId>,
}

pub struct Budgets {
    /// 应用级分库的字节预算。默认 256 MiB。
    pub app: u64,
    /// 每个仓库或文件夹分库的字节预算。默认 512 MiB。
    pub project: u64,
    /// `root` 下全部分库文件（含 `-wal`）的总预算，超出时删除最久没打开的项目分库。默认 2 GiB。
    pub total: u64,
    /// 同时打开的项目分库数。默认 2。应用级分库另算，不受这个限制。
    pub open_projects: usize,
    /// 每个连接的页缓存。默认 1 MiB。连接数上限是 2 ×（1 + open_projects），默认 6 个、合计 6 MiB（ADR 0027）。
    pub page_cache: u64,
}

impl Default for Budgets { /* 上面的默认值 */ }

impl Cache {
    /// 只分配结构，不做 IO、不创建目录，任何线程可调用（ADR 0017：首帧之前可以构造，但没有人会在首帧之前调用读写）。
    pub fn new(config: Config) -> Cache;

    /// 应用级分库的句柄。不做 IO，任何线程可调用；第一次读写时才打开数据库。
    pub fn app(&self) -> Partition;

    /// 仓库分库的句柄。`common_dir` 是 git 公共目录的绝对规范路径（`git::Location::common_dir`），
    /// 同一仓库的所有 worktree 传同一个值，得到同一个分库。本 crate 不再规范化路径。不做 IO。
    pub fn repo(&self, common_dir: &Path) -> Partition;

    /// 文件夹项目（不是 git 仓库）的分库句柄。`root` 是打开的根目录的规范路径。不做 IO。
    pub fn folder(&self, root: &Path) -> Partition;

    /// 计数与各分库的状态（见「数据结构」）。只读内存，不阻塞。测试和以后的诊断页读取。
    pub fn stats(&self) -> Stats;

    /// 把内存里待写的访问时间写回，关闭全部连接。阻塞，只在后台线程调用。
    /// 可选：`app` 在 kernel 的 `shutdown` 完成后、退出前在后台线程调用；不调用也安全（见「生命周期」）。
    /// 之后的读写会重新打开分库。
    pub fn close(&self);
}

/// 一个分库的句柄。`Clone + Send + Sync`，不持有连接：连接由 `Cache` 按「连接与内存」一节统一管理，
/// 所以 drop 句柄不做 IO，可以在任何线程上 drop。
#[derive(Clone)]
pub struct Partition { /* Arc<Slot> */ }

impl Partition {
    /// 读一条。阻塞（SQLite 查询，第一次调用时打开数据库），只在 executor 的任务里调用。
    /// 没有、命名空间版本不同、解码失败、分库不可用时都返回 None：调用方按未命中重算，不区分原因。
    /// 解码失败的条目在下一次写事务里删除。
    pub fn get<E: Entry>(&self, key: &Key) -> Option<E>;

    /// 在一个读事务里读多条，结果与 `keys` 一一对应。阻塞，只在 executor 的任务里调用。
    /// 用于一次要读几十条的场景（`git` 一个改动集的全部 hunk）。
    pub fn get_many<E: Entry>(&self, keys: &[Key]) -> Vec<Option<E>>;

    /// 写一条，已有同键的条目时替换。阻塞（一个写事务，含这次的淘汰），只在 executor 的任务里调用。
    /// 写不进去时返回 `Dropped`，不返回错误：缓存写失败不影响调用方，调用方通常忽略返回值。
    pub fn put<E: Entry>(&self, key: &Key, value: &E) -> Written;

    /// 删一条。阻塞，只在 executor 的任务里调用。
    pub fn remove<E: Entry>(&self, key: &Key) -> Written;

    /// 开始一批写入。不做 IO。
    pub fn batch(&self) -> Batch;
}

/// 一批写入，`commit` 时在一个事务里全部生效或全部不生效。`Send`，可以在一个任务里攒好再提交。
pub struct Batch { /* 分库句柄 + 已编码的写入 */ }

impl Batch {
    /// 编码并记下这条写入，不做 IO。同一批里同一个键写多次时，以最后一次为准。
    pub fn put<E: Entry>(&mut self, key: &Key, value: &E) -> &mut Batch;
    pub fn remove<E: Entry>(&mut self, key: &Key) -> &mut Batch;
    /// 已编码的字节数（键加值），调用方据此分批，不超过 `MAX_BATCH`。
    pub fn bytes(&self) -> u64;
    /// 在一个写事务里提交。阻塞，只在 executor 的任务里调用。
    /// 任何一条写不进去（包括单条超过 `MAX_VALUE`、整批超过 `MAX_BATCH`）时整批回滚，返回 `Dropped`。
    pub fn commit(self) -> Written;
}

pub const MAX_KEY: usize = 512;            // 编码后的键
pub const MAX_VALUE: usize = 8 << 20;      // 编码（压缩）后的单个值
pub const MAX_BATCH: u64 = 16 << 20;       // 一批的键加值

#[must_use]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Written {
    Committed,
    Dropped(Dropped),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dropped {
    /// 键超过 `MAX_KEY`，值超过 `MAX_VALUE`，或整批超过 `MAX_BATCH`。
    TooLarge,
    /// 磁盘写满（`SQLITE_FULL`）。之后 60 秒内的写入直接放弃，见「状态与异常」。
    DiskFull,
    /// 另一个进程持有写锁超过 100 ms（`SQLITE_BUSY`）。
    Busy,
    /// 分库已停用（多次损坏、目录不可写）。
    Unavailable,
    /// 其他 IO 错误。
    Io,
    /// 在 `Config::ui_thread` 上调用（只在 release 构建出现）。
    WrongThread,
}
```

使用方为每种缓存的数据实现 `Entry`：

```rust
pub trait Entry: Sized {
    const NAMESPACE: Namespace;
    /// 编码成字节。在调用 `put` / `Batch::put` 的线程上运行，不做 IO。
    fn encode(&self, out: &mut Vec<u8>);
    /// 解码。返回 None 时按未命中处理，并删除这一条。在调用 `get` 的线程上运行。
    fn decode(bytes: &[u8]) -> Option<Self>;
}

pub struct Namespace {
    /// 全局唯一的名字，`<crate>.<数据>`，登记在下面的「命名空间」表里。只用 ASCII 小写字母、数字、`.` 和 `_`。
    pub name: &'static str,
    /// 生产算法或编码格式变化时加一。不同版本的条目互不可见，旧版本的条目逐步删除（见「淘汰」）。
    pub version: u32,
    pub compression: Compression,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compression {
    None,
    /// 编码后 ≥ 4 KiB 的值用 zstd（`ruzstd`，Fastest 级别）压缩，压缩后不到原大小的 90% 才存压缩结果，否则存原样。
    Zstd,
}

/// 编码后的键。由若干段组成，每段带类型标记和长度，所以 ("ab", "c") 与 ("a", "bc") 不同。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key { /* Vec<u8>，第一段时预留 64 字节 */ }

impl Key {
    pub fn new() -> Key;
    pub fn bytes(self, b: &[u8]) -> Key;     // 对象 ID、哈希等原始字节
    pub fn str(self, s: &str) -> Key;
    pub fn u32(self, v: u32) -> Key;
    pub fn u64(self, v: u64) -> Key;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
```

键的编码：每段一个类型字节（1 字节串、2 字符串、3 `u32`、4 `u64`），字节串和字符串接着是 LEB128 编码的长度和内容，整数是 4 或 8 字节大端序。编码是单射，改动编码要升 `SCHEMA_VERSION`。

键的规则：结果取决于的每一项（内容身份、生产算法、编码格式、外部工具或适配器的版本）都要出现在命名空间版本或键里，缺一项就可能读到过期的结果。内容来自 git 的，键以 blob OID 为主（架构规则 5）；来自会话日志的，按 ADR 0026 以会话和适配器版本为键、值里带游标。键超过 `MAX_KEY` 的（Analyze 的完整请求）由使用方先求哈希。

### 命名空间

新增命名空间时在这张表里加一行，名字不得重复。

| 命名空间 | 分库 | 使用方 | 键 | 值 | 压缩 |
|---|---|---|---|---|---|
| `git.diff` | 仓库 | `git` | `HashKind`、旧 OID、新 OID、算法、空白处理（hunk 的格式版本是命名空间版本） | hunk 的行号区间（[git 设计「缓存」](git.md#缓存)） | 不压缩 |
| `agents.turns` | 应用 | `agents` | 来源 ID、会话 ID、适配器版本（索引格式版本是命名空间版本） | 轮次索引和生成它时的游标；Codex 来源会话的轮次 ID 集合由 `agents` 决定放进同一个值，或在同一批写入里另写一条 | `agents` 按基准决定 |
| `agents.summary` | 应用 | `agents` | 来源 ID、会话 ID、压缩文件的设备号、inode、大小、修改时间、适配器版本（摘要格式版本是命名空间版本） | 补齐后的会话摘要 | 不压缩 |
| `analyze.result` | 仓库 | Analyze（#66） | 完整分析请求的哈希（ADR 0019，构成见 `crates/cache/AGENTS.md`） | 校验后的分析结果 | 随 #66 决定 |

规则生成的简报不缓存（ADR 0019）。`agents` 的两个命名空间不在 git 里、没有 blob OID，键按 ADR 0026。

## 数据结构

### 文件布局

```
<root>/                               # Config::root，应用缓存目录下的 cache/
  app.sqlite   app.sqlite-wal   app.sqlite-shm
  repo/<h>.sqlite   (-wal, -shm)      # h = SHA-256("repo\0" + common_dir 的字节) 的前 16 字节，32 位十六进制
  folder/<h>.sqlite (-wal, -shm)      # h = SHA-256("folder\0" + root 的字节) 的前 16 字节
```

- 路径的字节取 `OsStr::as_bytes`，不做大小写或 Unicode 规范化（`common_dir` 已是 git 给出的规范路径）。
- 每个分库在 `meta` 表里记下自己的种类和完整路径。打开时与句柄的种类和路径比较，不同（文件被复制或改名过来，或哈希碰撞）就当作别的分库的文件：删除重建。
- 不在用户的 `.git` 或项目目录里写任何文件（`crates/cache/AGENTS.md`）。

### 表结构

```sql
PRAGMA application_id = 1279480899;   -- 0x4C435443，"LCTC"
PRAGMA user_version = 1;              -- SCHEMA_VERSION，表结构变化时加一
PRAGMA auto_vacuum = INCREMENTAL;     -- 建表之前设置

CREATE TABLE meta (
  name  TEXT PRIMARY KEY NOT NULL,    -- 'kind'、'path'、'bytes'、'opened'
  value BLOB NOT NULL
) WITHOUT ROWID;

CREATE TABLE namespace (
  id      INTEGER PRIMARY KEY,
  name    TEXT NOT NULL,
  version INTEGER NOT NULL,
  retired INTEGER NOT NULL DEFAULT 0, -- 同名的更高版本出现过之后为 1
  UNIQUE (name, version)
);

CREATE TABLE entry (
  id    INTEGER PRIMARY KEY,
  ns    INTEGER NOT NULL,             -- namespace.id
  key   BLOB NOT NULL,
  codec INTEGER NOT NULL,             -- 0 原样，1 zstd
  size  INTEGER NOT NULL,             -- 计入预算的字节数：length(key) + length(value) + 64
  atime INTEGER NOT NULL,             -- 最后访问时间，Unix 纪元起的分钟数
  value BLOB NOT NULL,                -- 放在最后一列：只读前几列（淘汰、计数）时不读值的溢出页
  UNIQUE (ns, key)
);
CREATE INDEX entry_atime ON entry (atime);
```

- `entry` 用有 rowid 的表：值可能有几 MB，SQLite 文档说明 `WITHOUT ROWID` 只适合单行不超过页大小约 1/20 的表。
- `meta.bytes` 是 `entry.size` 之和，在每个写事务里与条目一起更新，淘汰据此判断，不扫表。
- `meta.opened` 是最近一次打开的时刻，每次打开写一次；它也让文件的修改时间反映最近使用，目录清理按修改时间判断（见「淘汰」）。
- 命名空间的 ID 在这个分库第一次用到 `(name, version)` 时分配（一次插入）；插入时把同名、版本更低的行标为 `retired`。旧版本的程序打开新版本写过的分库时，同样只看见自己版本的条目。
- 页大小用默认的 4096 字节。

每个连接打开后执行：

```sql
PRAGMA journal_mode = WAL;            -- 只需在建库时设置一次，之后持久
PRAGMA synchronous = NORMAL;          -- WAL 下提交不 fsync，检查点才 fsync；进程崩溃不丢已提交的事务
PRAGMA cache_size = -1024;            -- Budgets::page_cache，单位 KiB
PRAGMA busy_timeout = 100;
PRAGMA journal_size_limit = 4194304;  -- 检查点之后 WAL 截到 4 MiB
PRAGMA mmap_size = 0;
PRAGMA trusted_schema = OFF;
PRAGMA cell_size_check = ON;          -- 读页时多做一次检查，损坏更早以 SQLITE_CORRUPT 报出
PRAGMA query_only = ON;               -- 只在读连接上
```

预编译语句缓存每个连接 16 条（rusqlite 的 `cache` feature）。

### 统计

```rust
pub struct Stats {
    pub hits: u64,
    pub misses: u64,                        // 含解码失败、超长的键、UI 线程上的读
    pub decode_failures: u64,
    pub written: u64,                       // 提交成功的写入调用（put、remove、commit）
    pub dropped: Vec<(Dropped, u64)>,       // 按原因计数，每种原因一项，六项都在
    pub evicted_bytes: u64,
    pub rebuilt: u64,                       // 删除重建的次数
    pub connections: usize,                 // 当前打开的连接数
    pub partitions: Vec<PartitionStats>,
}

pub struct PartitionStats {
    pub kind: PartitionKind,                // App / Repo / Folder
    pub path: Option<PathBuf>,
    pub state: PartitionState,              // Closed / Open / WritesSuspended { until } / Disabled { reason }
    pub bytes: u64,                         // 最近一次读到的 meta.bytes
}

impl Stats {
    pub fn dropped(&self, reason: Dropped) -> u64;
}

pub enum DisableReason {
    Corrupt,                                // 一次运行里第二次损坏
    NotWritable,                            // SQLITE_CANTOPEN、SQLITE_READONLY、SQLITE_PERM，或目录建不出来
    Io,                                     // 连续 3 次其他 IO 错误
}
```

## 数据流与线程

- 调用方：`git` 在计算 hunk 的后台任务里先 `get_many`，未命中的算完后 `put`；`agents` 在 `read` / `turns` 的任务里读写轮次索引，在补齐摘要的任务里写摘要。视图不调用本 crate（架构规则 4）。
- 本 crate 的每个阻塞方法都在调用方的线程上同步完成：打开数据库、查询、写事务、淘汰都在这一次调用里做完。所以：
  - 不影响 kernel 的静止判断：本 crate 没有自己的任务和线程，工作只发生在调用方的任务里，由调用方按 kernel 的规则持有 `BusyGuard`（[kernel 设计「静止」](kernel.md#静止)）。
  - 不需要取消：每次调用的工作量有上限（一个事务最多写 `MAX_BATCH`、淘汰最多删 16 MiB），调用方在两次调用之间检查自己的取消标志。补读历史、建索引这类长时间的工作由调用方分批，每批一次 `commit`。
  - 用户等待的结果不排在缓存写入后面：在 `Interactive` 任务里算出结果的调用方，先把结果交出去，再写缓存（或把写入交给一个 `Background` 任务）。
- 读写分开两个连接（WAL 下读不等写）：每个打开的分库有一个写连接和一个读连接，各由一把互斥锁保护（rusqlite 的 `Connection` 是 `Send`、不是 `Sync`）。写事务互相排队；读与写并行；读与读排队，单次读的耗时在百微秒级，`get_many` 把一个改动集的读取放进一次加锁。
- 写事务用 `BEGIN IMMEDIATE`，依次：写回待写的访问时间、删除上次解码失败的条目、本次写入、更新 `meta.bytes`、淘汰（见下节），然后提交。
- 自动检查点由 SQLite 在提交时做（默认每 1000 页），发生在当次写入的调用里。

### 连接与内存

- 连接数上限 2 ×（1 + `open_projects`），默认 6 个，每个页缓存 1 MiB，合计 6 MiB；`usage` 自己的数据库 ≤ 2 MiB，进程里 SQLite 的页缓存合计 ≤ 8 MB（ADR 0017、ADR 0027）。
- 读连接在第一次读时才打开。应用级分库的连接打开后一直保留到 `close`。
- 打开第 `open_projects + 1` 个项目分库时，关闭最久没用过的项目分库的两个连接（等它正在进行的调用结束），在这次调用里完成；之后对它的读写重新打开。
- 连接只在本 crate 的阻塞方法里关闭（换出项目分库、损坏重建、`close`），或在最后一个 `Cache` 克隆 drop 时关闭；`Partition` 句柄不持有连接，drop 句柄不做 IO。`app` 持有的 `Cache` 在退出时 drop，关闭连接会做一次检查点，发生在退出过程中。最后一个 `Cache` drop 之后，仍在的 `Partition` 句柄不再做 IO：读返回 None，写返回 `Dropped::Unavailable`。

## 淘汰

- 预算：应用级分库 256 MiB，每个项目分库 512 MiB（`Budgets`），按 `entry.size` 计。
- 每个写事务在提交前检查 `meta.bytes`：超过预算时按 `atime` 从旧到新删除，删到预算的 90% 为止，每个事务最多删 16 MiB；没删够的留给下一个写事务。单次写入最多 `MAX_BATCH`，所以分库最多短暂超出预算 16 MiB。
- 旧版本命名空间（`retired = 1`）的条目，每个写事务另外删最多 256 条，不论是否超出预算。命名空间升级后不需要一次性清掉旧数据。
- 空闲页：`freelist_count × page_size` 超过 32 MiB 时，在同一个事务里执行 `PRAGMA incremental_vacuum(2048)`，每次最多把 8 MiB 还给文件系统。
- 访问时间：读命中时，若条目的 `atime` 比现在早 60 分钟以上，把它的 `id` 记进这个分库在内存里的待写集合（最多 4096 个，满了不再记）；待写集合在下一个写事务开头写回，`close` 和换出分库时也写回。所以读命中通常不产生写入，LRU 的精度是 1 小时；进程崩溃时丢掉没写回的访问时间，只影响淘汰顺序。
- 目录清理：每次运行第一次打开任何分库时，列出 `repo/` 和 `folder/` 下的文件：修改时间早于 90 天的项目分库删除；全部分库文件（含 `-wal`）合计超过 `Budgets::total` 时，按修改时间从旧到新删除项目分库，直到不超过。当前打开的分库和应用级分库不删除。列目录和 `stat` 只涉及几十个文件，在这次调用里完成。

## 生命周期

1. `Cache::new`、`app()`、`repo()`、`folder()` 不做 IO。`app` 在首帧之后创建 `Cache`；插件在 `AfterFirstFrame` 之后才激活，第一次读写发生在它们的后台任务里，所以首帧之前不会打开 SQLite（ADR 0017）。
2. 第一次读写一个分库时打开它：创建目录，打开写连接，读 `application_id`、`user_version` 和 `meta`：
   - 两者都是 0 并且没有表：新文件，建表、写 `meta`。
   - `application_id` 不是 Lectern 的、`user_version` 不等于 `SCHEMA_VERSION`（更旧或更新）、`meta` 的种类或路径不符：关闭连接，删除 `.sqlite`、`-wal`、`-shm` 三个文件，重新建库。
3. 迁移：不做。表结构变化时 `SCHEMA_VERSION` 加一，旧文件在打开时整份重建；使用方的数据格式变化时命名空间版本加一，旧条目逐步删除。缓存里只有能重算的数据，重建只损失命中率。
4. 退出：不需要任何操作。WAL 加 `synchronous = NORMAL` 下，进程崩溃或被杀不会丢失已提交的事务、不会损坏文件，下次打开时 SQLite 自动恢复 WAL；断电可能丢失最后几个事务，可以接受。`Cache::close` 只多写回访问时间。

## 状态与异常

缓存的失败不传给调用方：读失败等于未命中，写失败等于没写。错误按 SQLite 的主错误码处理：

| 情况 | 处理 | 调用方看到的 |
|---|---|---|
| 文件损坏（`SQLITE_CORRUPT`、`SQLITE_NOTADB`，任何一次调用中出现） | 等这个分库上其他正在进行的调用结束，关闭它的连接，删除三个文件，重新建库；这次调用按未命中或 `Dropped::Io` 返回。同一分库在一次运行里第二次损坏时停用 | 未命中，之后照常 |
| 版本或身份不符 | 删除重建（见「生命周期」第 2 步） | 未命中 |
| 磁盘写满（`SQLITE_FULL`） | 事务回滚；这个分库之后 60 秒内的写入不再尝试；这次运行的有效预算降为当时 `meta.bytes` 的 75%，下一次写入成功时按新预算淘汰 | `Dropped::DiskFull`；读照常 |
| 另一个进程持有锁超过 100 ms（`SQLITE_BUSY`） | 放弃这次操作 | 读：未命中；写：`Dropped::Busy` |
| 目录不可创建或不可写（`SQLITE_CANTOPEN`、`SQLITE_READONLY`、`SQLITE_PERM`） | 停用这个分库，这次运行不再尝试 | 未命中、`Dropped::Unavailable` |
| 其他 IO 错误 | 放弃这次操作；同一分库连续 3 次时停用 | 未命中、`Dropped::Io` |
| 解码失败（`Entry::decode` 返回 None） | 记下 `id`，下一个写事务删除 | 未命中 |
| 在 UI 线程上调用 | debug 构建 panic；release 构建不做 IO | 未命中、`Dropped::WrongThread` |

- 停用的分库不再做任何 IO，读写立即返回；原因记进 `Stats`，向 stderr 写一行（`lectern-cache:` 前缀，与 kernel、executor 相同）。
- 文件在运行中被删除（用户清理缓存目录、系统清理空间）：已打开的连接继续使用已删除的文件，换出或重启后重新建库。
- 本 crate 没有界面；缓存不可用时界面只是变慢，不显示错误。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)，M 系列 Mac，系统文件缓存已热）：

| 预算 | 本 crate 的份额 | 基准 |
|---|---|---|
| 冷启动 20 ms | 首帧之前不做任何事：`Cache::new` 只分配结构 | 冷启动基准不变（#7） |
| 打开一个会话的 Diff < 50 ms | 分库已打开时：`get_many` 50 条 1 KiB 的 hunk < 1 ms（git 的份额是命中 < 2 ms）；`get` 一条 1 MiB 的轮次索引 < 2 ms（agents 的份额是命中加解析追加部分 < 10 ms） | `cache/get_many_50`、`cache/get_1m` |
| 打开项目 < 100 ms | 第一次打开已有的分库（含目录清理，100 个以内的文件）< 5 ms；新建分库 < 10 ms | `cache/open_existing`、`cache/open_new` |
| 写入（后台） | `put` 一条 4 KiB < 1 ms；1000 条 1 KiB 一批 < 15 ms；带 16 MiB 淘汰的写事务 < 30 ms | `cache/put_4k`、`cache/batch_1000`、`cache/evict_16m` |
| 空闲内存（空窗口 < 60 MB；monorepo < 100 MB） | 页缓存合计 ≤ 6 MiB（6 个连接 × 1 MiB），只开应用级分库时 ≤ 2 MiB；另有每个连接的预编译语句和 SQLite 自身结构，约 0.2 MiB；待写访问时间 ≤ 32 KiB | `cache/memory`：打开应用级分库和 2 个项目分库、各读 50 MiB 之后，常驻增量 ≤ 8 MiB |
| 安装包 ≤ 80 MB | SQLite 与 rusqlite 约 2 MB（ADR 0027） | #38 |

- `get` 一条 4 KiB < 50 µs（`cache/get_4k`）。
- 命名空间选 `Zstd` 之前，先用 `cache/zstd_1m`（1 MiB 值的压缩与解压）确认读取仍在使用方的预算内。
- bench 的设计文档（#7）合并之前不写基准代码；实现 PR 先用 `#[ignore]` 的 `perf_smoke_cache` 在 release 下测量上表各项，结果写进「实际验证」（与 git 的做法相同）。

## 测试

全部在 Linux 上运行（`just test -p lectern-cache`），用 `tempfile` 的临时目录，不依赖 GPUI 和 macOS。`test-support` feature 提供可手动推进的时钟（访问时间、60 秒的写入暂停、目录清理）和故障注入（对写连接设置 `max_page_count` 制造 `SQLITE_FULL`；在写事务中途暂停的钩子）：`testing::ManualClock`、`Cache::with_clock`、`Cache::set_ui_thread_panics`、`Partition::file`、`Partition::set_max_page_count`、`Partition::set_write_hook`。

- 构造（`new_*`）：`Cache::new` 加 `app()`、`repo()`、`folder()` 之后 `root` 不存在；第一次 `get` 之后才出现文件。
- 线程（`ui_thread_*`）：在 `ui_thread` 上调用，debug 构建 panic；`test-support` 可以关掉这个 panic，用来检查 release 的行为：不打开文件，读返回 None，写返回 `WrongThread`。
- 读写（`roundtrip_*`）：`put` 后 `get`、覆盖、`remove`、空值、8 MiB 的值、超过 `MAX_VALUE` 和 `MAX_KEY` 时 `TooLarge`、`get_many` 的结果顺序；键的编码是单射（分段不同的键编码不同，insta 快照记录编码）。
- 命名空间（`namespace_*`）：同一个键在不同命名空间里互不影响；版本加一后旧条目不命中，之后的写事务逐步删除旧条目、`meta.bytes` 随之减少；旧版本程序（低版本）读不到高版本写的条目。
- 批量（`batch_*`）：一批多条一起可见；中途注入 `SQLITE_FULL` 时整批都不可见；超过 `MAX_BATCH` 整批 `TooLarge`；同一批里同键多次写取最后一次。
- 淘汰（`evict_*`）：超出预算后删掉 `atime` 最旧的条目、保留刚被读过的；每个事务最多删 16 MiB；`retired` 的条目先于预算淘汰被删；空闲页超过 32 MiB 后文件变小。
- 访问时间（`touch_*`）：60 分钟内的命中不产生写入（读连接之外没有写事务）；时钟推进 61 分钟后命中，下一个写事务写回 `atime`；待写集合满 4096 后不再增长。
- 打开（`open_*`）：`user_version` 更旧、更新，`application_id` 不同，`meta` 路径不同（复制别的分库的文件过来），文件是随机字节，文件被截断，文件中间的页被改写（读到该页时 `SQLITE_CORRUPT`）：都删除重建，之后读写正常；同一次运行里第二次损坏后分库停用、不再做 IO；`root` 不可写时停用、不 panic。
- 磁盘满（`disk_full_*`）：`put` 返回 `DiskFull`，读照常；60 秒内的写入不尝试；时钟推进后去掉限制，下一次写入成功并按降低后的预算淘汰。
- 占用（`busy_*`）：另一个连接（模拟另一个进程）持有 `BEGIN IMMEDIATE` 时，`put` 在 100 ms 加余量内返回 `Busy`，`get` 照常命中。
- 并发（`concurrency_*`）：4 个线程读、1 个线程写，结果都是某个已提交的版本；写事务被钩子暂停时，另一个线程的 `get` 照常返回。
- 分库（`partitions_*`）：同一个 `common_dir` 得到同一个文件；`repo` 和 `folder` 对同一路径得到不同文件；打开第 3 个项目分库时最久没用的那个的连接被关闭，`Stats::connections` 不超过 6，应用级分库不被换出；换出后再读照常命中。
- 目录清理（`sweep_*`）：修改时间（`File::set_modified`）早于 90 天的项目分库被删除；超过总预算时从最旧的删起；打开中的分库和应用级分库不删。
- 内存（`memory_*`）：每个连接的 `PRAGMA cache_size` 是 `-1024`，读连接的 `query_only` 为 1。
- 崩溃（`crash_*`）：子进程写入并提交后 `std::process::abort()`，父进程重新打开，已提交的条目都在、`PRAGMA integrity_check` 为 `ok`。
- 压缩（`compression_*`）：`Zstd` 命名空间的大值压缩存储、读回一致；小于 4 KiB 或压不下来的值以原样存储（`codec = 0`）。

## 未决问题

1. **设置页是否显示缓存占用并提供「清除缓存」。** 选项：
   - (a) P1 不做（倾向）：缓存可以随时删除，需要时用户删除应用缓存目录下的 `cache/`；预算用上文的默认值，不可配置。
   - (b) P1 在设置页显示各分库的占用、提供清除按钮和总预算的选项：要加界面字符串和设置项，数据来自 `Cache::stats`。

   这是界面上的增项，由产品负责人决定。默认按 (a) 实现；选 (b) 时只增加接口（按分库删除），不改本文的存储设计。

技术问题都已在上文决定，没有其他未决项。

## 验收

分两步实现，每步一个 PR，都写 `Refs #17` 和 `Refs #31`（本 crate 不关闭这两个 issue）。每一步都要 `just check`、`just test -p lectern-cache`、`just deny` 通过。

1. **存储、键值接口与生命周期**（[#82](https://github.com/SoloJiang/lectern-app/pull/82)）：根 `Cargo.toml` 按 ADR 0027 加入 `rusqlite`。
   - `just test -p lectern-cache` 里 `new_*`、`ui_thread_*`、`roundtrip_*`、`namespace_*`、`batch_*`、`open_*`、`disk_full_*`、`busy_*`、`concurrency_*`、`crash_*`、`compression_*`、`memory_*` 通过。
   - `cargo tree -i rusqlite -e normal --depth 1` 只列出 `lectern-cache`。
   - `python3 scripts/check_architecture.py` 通过：本 crate 不依赖 GPUI、`platform`、`executor`。
   - `rg 'load_extension|thread::spawn' crates/cache/src --glob '!**/tests*'` 没有结果：不加载扩展，不自建线程。
   - `perf_smoke_cache`（`cargo test --release -p lectern-cache -- --ignored perf_smoke`）的读写各项在「性能」一节的预算内。
2. **淘汰、访问时间与分库管理**：
   - `evict_*`、`touch_*`、`partitions_*`、`sweep_*` 通过。
   - `perf_smoke_cache` 的 `evict_16m` 与 `memory` 两项在预算内。

接入由使用方各自的 PR 完成：`agents` 设计的第 4 步（`index_cache_*`、`complete_*`），`git` 的 hunk 缓存（`hunk_cache_*`）。`bench` 的 `cache/*` 基准随 [#7](https://github.com/SoloJiang/lectern-app/issues/7) 加入。

## 实际验证

第 1 步，在 Linux（x86_64，4 核 Xeon 2.1 GHz，内核 6.18）上运行。本 crate 不依赖 GPUI，也没有 macOS 专属代码；macOS 上的结果由 macOS CI 给出。

- `just check`、`just test`（整个工作区 619 个测试通过，跳过 6 个 `#[ignore]`）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。
- `just test -p lectern-cache`：47 个测试通过，跳过 `perf_smoke_cache`。按前缀：`new_*` 3、`ui_thread_*` 3（其中 2 个检查 debug 构建 panic 的测试只在 debug 下编译）、`roundtrip_*` 9、`namespace_*` 3、`batch_*` 5、`open_*` 11、`disk_full_*` 2、`busy_*` 1、`concurrency_*` 2、`crash_*` 2（其中一个是子进程的入口，没有环境变量时直接返回）、`compression_*` 3、`memory_*` 3。insta 快照 5 个（文件布局、`meta` 的内容、键的编码、写连接和读连接的 PRAGMA），逐个看过。
- `open_*` 的每种文件都确认走到了预期的分支（看 stderr 的 `lectern-cache:` 行）：`user_version` 更旧和更新、`application_id` 不同、复制别的分库的文件、普通的 SQLite 文件都是「版本或身份不符」；随机字节在打开时报 `SQLITE_NOTADB`；截断到一半、中间三分之一的页被改写都在读取时报 `SQLITE_CORRUPT`。`open_read_only_directory_disables` 在以 root 运行的容器里不生效（权限位被忽略，测试检测到后直接返回），`root` 是普通文件的 `open_unwritable_root_disables_without_panicking` 覆盖同一个分支。
- `crash_committed_entries_survive_an_abort`：子进程（同一个测试二进制）提交 150 次 `put` 和一批 150 条后 `abort()`；父进程打开时 `-wal` 还在（没有做过检查点），300 条都在，`PRAGMA integrity_check` 为 `ok`，`meta.bytes` 等于 `entry.size` 之和。
- `cargo tree -i rusqlite -e normal --depth 1` 只列出 `lectern-cache`；`python3 scripts/check_architecture.py` 通过；`rg 'load_extension|thread::spawn' crates/cache/src --glob '!**/tests*'` 没有结果。Cargo.lock 新增 `rusqlite`、`libsqlite3-sys`、`hashlink`、`fallible-iterator`、`fallible-streaming-iterator`、`vcpkg`（只在 Windows 上编译），与 ADR 0027 的实测一致；`sha2` 0.11 原来就在 Cargo.lock，已有依赖的版本没有变化。
- `perf_smoke_cache`（`cargo test --release -p lectern-cache -- --ignored perf_smoke`，运行三次，分库已打开，系统文件缓存已热）：

  | 项 | 预算（M 系列 Mac） | 中位数 | p95 | 最慢 |
  |---|---|---|---|---|
  | `get` 一条 4 KiB | < 50 µs | 3.7–3.9 µs | 6.0–7.2 µs | 0.36 ms |
  | `get_many` 50 条 1 KiB | < 1 ms | 77–83 µs | 113–124 µs | 0.18 ms |
  | `get` 一条 1 MiB | < 2 ms | 0.48–0.53 ms | 0.64–0.93 ms | 2.8 ms |
  | `put` 一条 4 KiB | < 1 ms | 46 µs | 90–104 µs | 15 ms |
  | 1000 条 1 KiB 一批 | < 15 ms | 6.5–7.1 ms | 28–33 ms | 36 ms |
  | 打开已有的分库 | < 5 ms | 1.3–1.6 ms | 1.9–2.3 ms | 2.9 ms |
  | 新建分库 | < 10 ms | 4.3–6.0 ms | 6.8–10.1 ms | 34 ms |
  | `zstd_1m`（参考，1 MiB 文本） | 无 | 写 4.7–5.8 ms，读 0.51–0.65 ms | | |

  中位数都在预算内。写入的长尾来自提交时的自动检查点（每 1000 页一次，检查点要 fsync）：同样的测量关掉自动检查点后，1000 条一批的 p95 是 6.5 ms。设计规定检查点发生在当次写入的调用里，写入都在后台任务里，不影响读取的预算。新建分库的长尾是建目录和建 WAL 时的 fsync。打开已有的分库还不含目录清理（第 2 步）。没有在 macOS 上测量。
- 没有验证的：`bench` 的 `cache/*` 基准没有写（bench 的设计文档 #7 合并之前不写，由上面的 `perf_smoke_cache` 代替）；`evict_16m` 和 `memory` 两项属于第 2 步；macOS 上没有运行。

## 与设计的差异

第 1 步：

- 写事务里的淘汰在第 1 步就实现了：超出预算时按 `atime` 删到 90%、每个事务最多 16 MiB，以及每个事务删最多 256 条 `retired` 命名空间的条目。`disk_full_*`（按降低后的预算淘汰）和 `namespace_*`（旧条目逐步删除、`meta.bytes` 随之减少）要用到它们。第 2 步还剩：空闲页的 `incremental_vacuum`；访问时间的待写集合（读命中目前不更新 `atime`，`atime` 是写入的时刻）；项目分库的换出（目前每个打开过的项目分库都保留两个连接直到 `close`，连接数还没有 6 个的上限）；目录清理；以及 `evict_*`、`touch_*`、`partitions_*`、`sweep_*` 测试。第 2 步合并之前没有使用方接入本 crate，连接数暂时没有上限不影响内存预算。
- `Key` 的内部表示用 `Vec<u8>`，第一段时预留 64 字节，不用 `SmallVec<[u8; 64]>`：依赖表里没有 `smallvec`，键的分配不在热路径上。接口不变，另加 `is_empty`（clippy 要求有 `len` 的类型也有 `is_empty`）。设计没有写键的编码格式，已补在「接口」里。
- 设计没有定义的类型和方法：`DisableReason`（`PartitionState::Disabled` 的原因，已补在「统计」里）、`Stats::dropped(reason)`；`Budgets` 派生 `Clone, Debug`，`Dropped` 派生 `Hash`，`PartitionKind` 派生 `Ord, Hash`（`Stats` 里的项目分库按种类和路径排序）。`test-support` 的公开接口已列在「测试」里。
- 计数的口径：`written` 是提交成功的写入调用数，不是条目数；UI 线程上的读计入 `misses`，不计入 `dropped`（`Dropped` 是写入的结果）；`evicted_bytes` 含 `retired` 条目的删除。
- 打开时发现的损坏（随机字节报 `SQLITE_NOTADB`）与版本不符一样，在这次调用里删除重建，然后照常完成这次调用；它也计入「同一分库第二次损坏时停用」。读写中途发现的损坏按设计关闭连接、删除文件，这次调用按未命中或 `Dropped::Io` 返回，下一次调用建新库。
- 第二次损坏、分库停用时也删除三个文件：留下的文件下次运行打开时反正要重建，删除不多做事，也让「停用后不做 IO」可以从文件不再出现来检查。
- 新文件以外的识别：`application_id` 和 `user_version` 都是 0 但有表（别的程序的 SQLite 文件）、`meta` 表缺失或缺项、值的类型不对，都当作版本或身份不符，删除重建。
- 打开已有的分库时写 `meta.opened` 遇到 `SQLITE_BUSY` 或 `SQLITE_FULL`，不让打开失败：它只给目录清理提供修改时间。
- 读连接打开时报 `SQLITE_CANTOPEN`（写连接打开之后文件被删除）只关闭这个分库的连接，不停用；下一次调用重新打开写连接、建新库。设计的「文件在运行中被删除」只写了已打开的连接。
- `SQLITE_LOCKED` 按 `SQLITE_BUSY` 处理，`SQLITE_TOOBIG` 返回 `Dropped::TooLarge`；超过 `MAX_KEY` 的 `remove` 返回 `TooLarge`；空的批不打开分库，直接返回 `Committed`。
- 解码失败的待删集合每个会话最多 4096 个 ID，不重复记录：同一个坏条目在下一次写事务之前被读多次，只记一次。
- 读连接也设置 `synchronous` 和 `journal_size_limit`：设计列出的 PRAGMA 除 `query_only` 外每个连接都执行；`journal_mode` 只在建库时设置（持久）。
- `bench` 的 `cache/*` 没有加入，理由与 git 第 1 步的 `git/open` 相同；由 `#[ignore]` 的 `perf_smoke_cache` 在 release 下测量，结果见「实际验证」。
