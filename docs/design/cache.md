# cache 设计

状态：已实现。第 1 步（存储、键值接口与生命周期）[#82](https://github.com/SoloJiang/lectern-app/pull/82)，第 2 步（淘汰、访问时间与分库管理）[#91](https://github.com/SoloJiang/lectern-app/pull/91)，第 3 步（自定义表）[#111](https://github.com/SoloJiang/lectern-app/pull/111)（见「验收」）。

对应 issue [#17](https://github.com/SoloJiang/lectern-app/issues/17)、[#31](https://github.com/SoloJiang/lectern-app/issues/31)（`crates/cache/AGENTS.md` 列出的两个 issue；本 crate 是它们的前置，不单独关闭其中任何一个）。`cache` 是派生数据的本地持久缓存：解析、diff、索引这类可以从 git 对象或会话日志重新算出的结果，以内容的身份为键存进 SQLite，下次启动、切换 worktree 时直接命中（架构规则 5，[技术调研「性能要点」](tech-research.md#性能要点)）。存储选型见 [ADR 0027](../adr/0027.md)。

## 范围

- 做：
  - 两类分库：项目分库，每个仓库（按 git 公共目录区分，同一仓库的 worktree 共用）或文件夹项目一个；应用级分库，只有一个，不属于任何仓库。
  - 键值接口：按命名空间和键读写字节值，值的编码由使用方的类型实现；多条写入在一个事务里提交。
  - 自定义表：使用方在分库里建自己的表，表里的行属于一个个单元，单元与条目一起计入预算和 LRU，按命名空间另有配额（见「自定义表」）。第一个使用方是 `code_intel` 的符号索引（[#17](https://github.com/SoloJiang/lectern-app/issues/17)，[code_intel 设计「存储表」](code_intel.md#存储表)）。
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
| `code_intel.tags` | 仓库、文件夹 | `code_intel` | OID、语言、grammar 版本、查询版本（表结构和单元的键是命名空间版本） | 没有值：单元，行在 `ci_ext`、`ci_def`、`ci_ref` 三张自定义表里（「自定义表」，[code_intel 设计「存储表」](code_intel.md#存储表)）；配额 224 MiB（[ADR 0033](../adr/0033.md) 从原来的 256 MiB 里划出 32 MiB 给 `code_intel.deps`）；打开的大文件按窗口写的单元也在这里，键另加窗口起点（[ADR 0034](../adr/0034.md)） | 不使用 |
| `code_intel.deps` | 仓库、文件夹 | `code_intel` | 依赖的单元：与 `code_intel.tags` 相同；包的路径表：生态、包名、版本、身份 | 依赖的单元（同一组自定义表）；包的（相对路径、OID）表作为普通条目；配额 32 MiB，依赖的单元只淘汰依赖的单元（[ADR 0033](../adr/0033.md)） | 不使用 |
| `syntax.lines` | 仓库、文件夹 | 高亮的领域库（随 `file_reader` 的设计） | 内容 OID | 每 64 KiB 处的行号（行检查点，[ADR 0034](../adr/0034.md)），64 MiB 的文件约 4 KiB | 不压缩 |

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
PRAGMA user_version = 2;              -- SCHEMA_VERSION，表结构变化时加一（2：custom 表与外键）
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

CREATE TABLE custom (                 -- 已建的自定义表，见「自定义表」
  ns      INTEGER PRIMARY KEY,        -- 单元的 namespace.id
  name    TEXT NOT NULL UNIQUE,
  version INTEGER NOT NULL,
  bytes   INTEGER NOT NULL,           -- 这个命名空间的 entry.size 之和
  tables  TEXT NOT NULL               -- schema 建出的表名，每行一个
);
```

- `entry` 用有 rowid 的表：值可能有几 MB，SQLite 文档说明 `WITHOUT ROWID` 只适合单行不超过页大小约 1/20 的表。
- `meta.bytes` 是 `entry.size` 之和，在每个写事务里与条目一起更新，淘汰据此判断，不扫表。
- `meta.opened` 是最近一次打开的时刻，每次打开写一次；它也让文件的修改时间反映最近使用，目录清理按修改时间判断（见「淘汰」）。
- 命名空间的 ID 在这个分库第一次用到 `(name, version)` 时分配（一次插入）；插入时把同名、版本更低的行标为 `retired`。旧版本的程序打开新版本写过的分库时，同样只看见自己版本的条目。
- 页大小用默认的 4096 字节。

每个连接打开后执行：

```sql
PRAGMA journal_mode = WAL;            -- 只需在建库时设置一次，之后持久
PRAGMA foreign_keys = ON;             -- 删除单元的条目时级联删除它在自定义表里的行
PRAGMA synchronous = NORMAL;          -- WAL 下提交不 fsync，检查点才 fsync；进程崩溃不丢已提交的事务
PRAGMA cache_size = -1024;            -- Budgets::page_cache，单位 KiB
PRAGMA busy_timeout = 100;
PRAGMA journal_size_limit = 4194304;  -- 检查点之后 WAL 截到 4 MiB
PRAGMA mmap_size = 0;
PRAGMA trusted_schema = OFF;
PRAGMA cell_size_check = ON;          -- 读页时多做一次检查，损坏更早以 SQLITE_CORRUPT 报出
PRAGMA query_only = ON;               -- 只在读连接上
```

预编译语句缓存每个连接 32 条（rusqlite 的 `cache` feature）。写连接上本 crate 自己的语句约 22 条，加上自定义表使用方的写入语句（code_intel 3 条）；读连接上本 crate 2 条，加上使用方的查询（code_intel 约 9 条）。第 1、2 步是 16 条。

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
  - 不需要取消：每次调用的工作量有上限（一个事务最多写 `MAX_BATCH`、淘汰最多删本事务写入的字节数，至少 1 MiB、至多 16 MiB），调用方在两次调用之间检查自己的取消标志。补读历史、建索引这类长时间的工作由调用方分批，每批一次 `commit`。
  - 用户等待的结果不排在缓存写入后面：在 `Interactive` 任务里算出结果的调用方，先把结果交出去，再写缓存（或把写入交给一个 `Background` 任务）。
- 读写分开两个连接（WAL 下读不等写）：每个打开的分库有一个写连接和一个读连接，各由一把互斥锁保护（rusqlite 的 `Connection` 是 `Send`、不是 `Sync`）。写事务互相排队；读与写并行；读与读排队，单次读的耗时在百微秒级，`get_many` 把一个改动集的读取放进一次加锁。
- 写事务用 `BEGIN IMMEDIATE`，依次：写回待写的访问时间、删除上次解码失败的条目、本次写入、更新 `meta.bytes`、淘汰（见下节），然后提交。
- 自动检查点由 SQLite 在提交时做（默认每 1000 页），发生在当次写入的调用里。

### 连接与内存

- 连接数上限 2 ×（1 + `open_projects`），默认 6 个，每个页缓存 1 MiB，合计 6 MiB；`usage` 自己的数据库 ≤ 2 MiB，进程里 SQLite 的页缓存合计 ≤ 8 MB（ADR 0017、ADR 0027）。
- 读连接在第一次读时才打开。应用级分库的连接打开后一直保留到 `close`。
- 打开第 `open_projects + 1` 个项目分库时，关闭最久没用过的项目分库的两个连接（等它正在进行的调用结束），在这次调用里完成；之后对它的读写重新打开。「最久没用过」按最近一次读写调用的先后。正在打开的分库先占一个名额再打开文件，被换出的分库在新分库打开文件之前关闭，所以并发打开时连接数也不超过上限。
- 连接只在本 crate 的阻塞方法里关闭（换出项目分库、损坏重建、`close`），或在最后一个 `Cache` 克隆 drop 时关闭；`Partition` 句柄不持有连接，drop 句柄不做 IO。`app` 持有的 `Cache` 在退出时 drop，关闭连接会做一次检查点，发生在退出过程中。最后一个 `Cache` drop 之后，仍在的 `Partition` 句柄不再做 IO：读返回 None，写返回 `Dropped::Unavailable`。

## 淘汰

- 预算：应用级分库 256 MiB，每个项目分库 512 MiB（`Budgets`），按 `entry.size` 计。
- 每个写事务在提交前检查 `meta.bytes`：超过预算时按 `atime` 从旧到新删除，删到预算的 90% 为止；每个事务最多删本事务写入的字节数（`entry.size` 之和），但至少 1 MiB、至多 16 MiB；没删够的留给下一个写事务。超出预算时每个事务删掉的不少于它写入的（单次写入最多 `MAX_BATCH`），所以分库最多短暂超出预算 16 MiB，之后只减不增；淘汰的开销与这次写入的开销同一量级。
- 旧版本命名空间（`retired = 1`）的条目，每个写事务另外删最多 256 条，不论是否超出预算。命名空间升级后不需要一次性清掉旧数据。
- 空闲页：`freelist_count × page_size` 超过 32 MiB 时，在同一个事务里执行 `PRAGMA incremental_vacuum(512)`，每次最多把 2 MiB 还给文件系统（文件在下一次检查点时变小）。
- 访问时间：读命中时，若条目的 `atime` 比现在早 60 分钟以上，把它的 `id` 记进这个分库在内存里的待写集合（最多 4096 个，满了不再记；自定义表的 `touch` 满时写回，见「自定义表」）；待写集合在下一个写事务开头写回，`close` 和换出分库时也写回，写回的 `atime` 是写回的时刻：读和写回之间没有别的写入，所以与其他条目的先后不变。写回只更新 `atime` 早于写回时刻 60 分钟以上的条目，读命中记下的条目都满足这一条。所以读命中通常不产生写入，LRU 的精度是 1 小时；进程崩溃、最后一个 `Cache` drop 时丢掉没写回的访问时间，只影响淘汰顺序。
- 目录清理：每次运行第一次打开任何分库时，在打开它的文件之前，列出 `repo/` 和 `folder/` 下的文件：一个项目分库的修改时间取它三个文件中最新的，早于 90 天的删除；全部分库文件（含 `-wal`、`-shm` 和应用级分库的文件）合计超过 `Budgets::total` 时，按修改时间从旧到新删除项目分库，直到不超过。当前打开或正在打开的分库和应用级分库不删除；同时打开的其他分库等清理完成后再打开文件。不是分库的文件不动；删除失败写一行 stderr，不停用分库。列目录和 `stat` 只涉及几十个文件，在这次调用里完成。

## 自定义表

`code_intel` 的符号索引要按名字查询定义和引用，键值接口不够。自定义表让使用方在分库里建自己的表：表里的每一行属于一个**单元**，单元是 `entry` 表里的一个条目，有命名空间和键，没有值。单元与其他条目一样计入 `meta.bytes`、按访问时间淘汰；单元被删除时，它在自定义表里的行经外键级联在同一条语句里删除。自定义表与其他缓存数据一样可以随时删除重建。

### 接口

```rust
/// 一组自定义表的登记。使用方定义成 `static`，一组表对应一个命名空间。
pub struct Tables {
    /// 单元的命名空间，登记在「命名空间」表里。`version` 同时是表的版本：表结构、语句用到的列、
    /// 单元的键任何一项变化时加一。`compression` 不使用（单元没有值）。
    pub namespace: Namespace,
    /// 建表和建索引的语句，第一次使用时在一个写事务里按顺序执行（「登记的规则」）。
    pub schema: &'static [&'static str],
    /// `TableRead::query` 和 `TableWrite::execute` 能执行的语句。
    pub statements: &'static [&'static str],
    /// 这个命名空间的单元在一个分库里最多占的字节数（`entry.size` 之和）。
    pub quota: u64,
}

impl Partition {
    /// 这组表在这个分库里的句柄。不做 IO，任何线程可调用。
    pub fn tables(&self, tables: &'static Tables) -> TableSet;
}

/// `Clone + Send + Sync`，不持有连接。
#[derive(Clone)]
pub struct TableSet { /* 分库句柄、登记、淘汰代数 */ }

impl TableSet {
    /// 淘汰代数（「淘汰代数」）。只读一个原子变量，任何线程可调用，不阻塞。
    pub fn evictions(&self) -> u64;
    /// 在读连接上开一个读事务，把它交给 `f`。阻塞，只在 executor 的任务里调用；第一次使用时建表。
    /// 分库不可用、建表失败或 `f` 里任何一条查询失败时返回 None，`f` 看到的结果不完整。
    pub fn read<T>(&self, f: impl FnOnce(&mut TableRead<'_>) -> T) -> Option<T>;
    /// 标记单元被访问，不读 `entry`，访问时间的规则与读命中相同。通常只写内存，
    /// 待写集合满时在这次调用里写回。阻塞，只在 executor 的任务里调用。
    pub fn touch(&self, units: &[Unit]);
    /// 开始一次写入。不做 IO。
    pub fn write(&self) -> TableWrite;
}

pub struct TableRead<'a> { /* 读连接 */ }

impl TableRead<'_> {
    /// 执行一条登记过的只读语句，每一行调用一次 `row`，`row` 返回 false 时停止。
    /// 失败时返回 false，同一次 `read` 里之后的查询不再执行，`read` 返回 None。
    pub fn query(&mut self, sql: &'static str, params: &[Param<'_>],
                 row: impl FnMut(&Row<'_>) -> bool) -> bool;
}

pub struct Row<'a> { /* 当前行 */ }

impl Row<'_> {
    /// 第 `column` 列（从 0 开始）。NULL、类型不符或没有这一列时返回 None。
    pub fn int(&self, column: usize) -> Option<i64>;
    pub fn bytes(&self, column: usize) -> Option<&[u8]>;
    /// 不是合法 UTF-8 时也返回 None。
    pub fn text(&self, column: usize) -> Option<&str>;
    pub fn unit(&self, column: usize) -> Option<Unit>;
}

/// 语句的参数，依次绑定到 ?1、?2……
#[derive(Clone, Copy, Debug)]
pub enum Param<'a> {
    Null,
    Int(i64),
    Bytes(&'a [u8]),
    Text(&'a str),
    /// 同一个 `TableWrite` 里 `create` 建的单元，提交时换成它的 ID。用在 `query` 里时查询失败。
    New(NewUnit),
}

/// 已提交的单元：它的 `entry.id`。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Unit(/* i64 */);
impl Unit {
    pub fn from_id(id: i64) -> Unit;
    pub fn id(self) -> i64;
}

/// 一次写入里新建的单元。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NewUnit(/* 这次写入里的序号 */);

/// 一次写入，`commit` 时在一个写事务里全部生效或全部不生效。`Send`，可以在一个任务里攒好再提交。
pub struct TableWrite { /* 记下的操作，参数复制进一个缓冲区 */ }

impl TableWrite {
    /// 新建一个单元，计入预算的字节数是 `len(key) + bytes + 64`。这个键已有单元时先删除它和它的行。不做 IO。
    pub fn create(&mut self, key: &Key, bytes: u64) -> NewUnit;
    /// 记下一条登记过的语句和它的参数，提交时按记下的顺序执行。不做 IO。
    pub fn execute(&mut self, sql: &'static str, params: &[Param<'_>]) -> &mut TableWrite;
    /// 删除这个键的单元和它的行；没有时什么也不做。
    pub fn remove(&mut self, key: &Key) -> &mut TableWrite;
    /// 删除这个命名空间的一个单元和它的行。
    pub fn remove_unit(&mut self, unit: Unit) -> &mut TableWrite;
    /// 已记下的字节数：键加参数（整数按 8 字节），调用方据此分批，不超过 `MAX_BATCH`。
    pub fn bytes(&self) -> u64;
    /// 在一个写事务里提交。阻塞，只在 executor 的任务里调用。结果与 `Batch::commit` 相同，见「写入、配额与淘汰」。
    pub fn commit(self) -> Written;
}
```

公开接口里没有 rusqlite 的类型；`Row` 只在 `query` 的回调里有效。

### 登记的规则

- `schema` 只建表和建在这些表上的索引：不建触发器和视图，不在本 crate 的表（`meta`、`namespace`、`entry`、`custom`）上建索引。表名带使用方自己的前缀（code_intel 是 `ci_`），不与本 crate 的表和其他登记重复。
- 每张表有一列声明为 `REFERENCES entry (id) ON DELETE CASCADE`（列名由使用方定，code_intel 是 `unit`），并且这一列上有索引，或者它就是这张表的 `INTEGER PRIMARY KEY`。每个连接都打开外键（`PRAGMA foreign_keys = ON`）：删除 `entry` 的一行时，SQLite 在同一条语句里按这一列找到并删除引用它的行，没有索引就要扫整张表；插入引用不存在的单元的行违反外键，这次写入整个回滚。所以任何一条删除条目的路径（预算淘汰、`retired` 清理、解码失败、`remove`）都不会留下不属于单元的行，也不依赖进程里有没有这组登记。
- 建表时检查：新建的对象只有表和这些表上的索引；每张表有符合上一条的列（`PRAGMA foreign_key_list`），`SELECT 1 FROM <表> WHERE <列> = ?` 的查询计划不是全表扫描（`EXPLAIN QUERY PLAN`）；`statements` 的每一条都能预编译。任何一项不满足时建表的事务回滚，向 stderr 写一行，这次读返回 None、写返回 `Dropped::Io`；分库照常可用，下一次使用时再检查一遍。
- `query` 只执行只读语句（`sqlite3_stmt_readonly`），写语句在 `query` 里不执行、直接失败：读连接是 `query_only`，在它上面执行写语句会报 `SQLITE_READONLY`，按「状态与异常」会停用分库。
- 语句是静态文本，参数个数固定。可变长的列表（code_intel 的存在检查 `oid IN (…)`，最多 500 个）写成固定个数的占位符，用不到的绑定 NULL：`IN` 里的 NULL 不匹配任何值。
- 语句只读写使用方自己的表；`entry` 的行只经本节的接口建立和删除。登记过的命名空间不再用作 `Entry` 的命名空间。
- 单元 ID 是 `entry.id`（rowid），单元删除之后这个 ID 可能分配给新单元。使用方在淘汰代数变化之后，不再沿用之前读到的单元 ID。

### 建表与版本

- 一个会话里一组登记第一次经 `TableSet` 读或写时，在写连接上按名字查 `custom`：版本相同就使用；没有记录或版本不同时开一个写事务：删除旧版本的表（`DROP TABLE`）和它在 `entry` 上的部分索引，把旧版本的命名空间标为 `retired`，然后执行 `schema`、做上一节的检查、建部分索引 `entry_ns<id>`（`ON entry (atime) WHERE ns = <id>`，配额淘汰用）、写入 `custom`。读也会触发建表：表还不存在时，建表和这次读在同一次调用里完成。
- 旧版本的单元在表删除之后已经没有行，查询看不见它们；它们与其他 `retired` 命名空间的条目一样，每个写事务删最多 256 条（「淘汰」），不在建表的事务里一次删完，单个事务的工作量仍有上限。
- 一个分库只保留一个版本：旧版本程序打开新版本建过表的分库时，同样删掉新版本的表、建自己的。两个版本交替运行时每次切换都重建，只损失命中率。
- 建部分索引要扫一遍 `entry`：已有 20 万个条目的分库约 45 ms（见「实际验证」），每个分库每个版本一次，在使用方的后台任务里。
- `custom` 表和外键设置让 `SCHEMA_VERSION` 升为 2，第 2 步写的文件打开时整份重建。旧版本程序（`SCHEMA_VERSION` 为 1，不打开外键）打开新文件时同样因版本不符而重建，不会在有自定义表的文件里删除条目而留下行。

### 写入、配额与淘汰

- `TableWrite::commit` 是一个写事务，依次：写回访问时间、删除解码失败的条目（与「数据流与线程」相同）、按记下的顺序执行 `create`、`execute`、`remove`、删除 `retired` 命名空间的条目、配额淘汰、预算淘汰、归还空闲页、更新 `meta.bytes` 和 `custom.bytes`。全部生效或全部不生效。
- 单元的字节数由使用方声明（code_intel 按行估算），计入 `entry.size`、`meta.bytes` 和 `custom.bytes`；本 crate 不核对行的实际大小。
- 失败时的结果与 `Batch::commit` 相同。另外，使用方的语句出错（违反约束、引用不存在的单元、类型不符、参数个数不对、语法错误、表不存在）时整次回滚，返回 `Dropped::Io`，向 stderr 写一行，但不计入「连续 3 次其他 IO 错误」：这类错误来自使用方的代码，不说明文件有问题，不停用分库。
- `bytes()` 超过 `MAX_BATCH` 或任何一个键超过 `MAX_KEY` 时整次返回 `TooLarge`，不打开分库；没有记下任何操作的写入直接返回 `Committed`。
- 配额：提交前，这个命名空间的 `custom.bytes` 加上本事务的变化超过 `quota` 时，按 `atime` 从旧到新删除它自己的单元（经部分索引，连同它们的行），删到配额的 90% 为止；每个事务最多删本事务写入的字节数，但至少 256 KiB、至多 16 MiB。配额淘汰只在这个命名空间自己的写入里进行：只有它自己的写入会让它变大。超出配额时每次写入删掉的不少于它写入的，所以这个命名空间最多短暂超出配额 16 MiB，之后只减不增。下限比预算淘汰的 1 MiB 低：单元带着行，按 1 MiB 的下限，一次只写一个单元的写入要删约 240 个单元、1.5 万行（`testing::tags` 的形状），实测 39 ms（见「与设计的差异」第 3 步）。
- 预算淘汰的规则不变（「淘汰」）：单元与条目一起按 `atime` 排序。配额淘汰删掉的字节计入同一个事务的预算淘汰上限。配额淘汰先于预算淘汰，`code_intel.tags` 与 `code_intel.deps` 合计不超过 256 MiB（224 + 32，[ADR 0033](../adr/0033.md)），所以只有其他命名空间合计超过「分库预算 − 配额」（512 − 256 = 256 MiB）时才会触发预算淘汰：hunk 缓存在 256 MiB 以内时不会因为符号索引被淘汰。
- 任何写事务（包括 `Partition::put` 引起的预算淘汰）删除单元时，`custom.bytes` 在同一个事务里减少，并记下这个命名空间有单元被删除（「淘汰代数」）。
- 外键的代价：分库里有自定义表时，删除任何条目（包括 hunk）都要按外键列在每张自定义表里查一次。`cache/evict` 的夹具里另有 2000 个单元时，淘汰 1 MiB 的 1 KiB 条目从 6.5 ms 变为约 10 ms，仍在 30 ms 的预算内。

### 访问时间

- `touch` 把单元 ID 并进分库的待写集合（与读命中共用，最多 4096 个），不读 `entry`。集合满时在这次调用里写回（一个只更新访问时间的写事务），再接着记录余下的 ID；写回失败时丢掉这次调用余下的 ID，只影响淘汰顺序。所以标记 10 万个单元最多写回 25 次，待写集合仍不超过 32 KiB。
- 写回只更新 `atime` 早于写回时刻 60 分钟以上的条目（`… WHERE atime < ?2 AND id IN (?3, …)`），所以一小时内重复标记的单元不改变文件，精度与读命中相同。写回按集合执行：每 1024 个 ID 一条语句，`IN` 里是固定个数的占位符，最后一组用不到的绑定 NULL。
- 写回可能发生在 `touch` 的调用里。用户等待结果的路径（code_intel 的查询命中）先交出结果再调用 `touch`，或把它交给 `Background` 任务。

### 淘汰代数

- 每个分库为取过 `TableSet` 的每个命名空间保留一个 `AtomicU64`，同一分库同一命名空间的句柄共用它，`evictions()` 只读一次原子变量。
- 加一：这个进程的写事务提交时删除了这个命名空间的单元（预算淘汰、配额淘汰、`remove`、`create` 替换同键的单元）；建表时删除了这个命名空间另一个版本的表。分库删除重建（版本或身份不符、损坏）、停用，或本次运行已经打开过的分库再打开时发现是新文件（目录清理删掉了它，或用户删除了缓存目录）时，这个分库的全部代数加一。
- 不加一：读、`touch`、只新增单元的写入、换出和 `close`（文件和数据都还在）、本次运行第一次打开时新建文件（之前没有可以失效的结果）。
- 另一个进程（同时运行的另一个 Lectern）删除的单元不改变这个进程的代数：使用方据代数认为仍在的单元，读的时候可能已经不在，按未命中处理。这是已知限制。

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
| 写入（后台） | `put` 一条 4 KiB < 1 ms；1000 条 1 KiB 一批 < 15 ms；超出预算的分库里一次小写入（淘汰 1 MiB，空闲页超过 32 MiB 时归还 2 MiB）< 30 ms | `cache/put_4k`、`cache/batch_1000`、`cache/evict` |
| 空闲内存（空窗口 < 60 MB；monorepo < 100 MB） | 页缓存合计 ≤ 6 MiB（6 个连接 × 1 MiB），只开应用级分库时 ≤ 2 MiB；另有每个连接的预编译语句和 SQLite 自身结构，约 0.2 MiB；待写访问时间 ≤ 32 KiB | `cache/memory`：打开应用级分库和 2 个项目分库、各读 50 MiB 之后，常驻增量 ≤ 8 MiB |
| 安装包 ≤ 80 MB | SQLite 与 rusqlite 约 2 MB（ADR 0027） | #38 |
| 自定义表的写入（后台） | 一次写入 64 个单元及其行（每个单元 20 个定义、40 个引用，参数约 100 KB，code_intel 的一批）< 15 ms；同样一批写进超出配额的命名空间（同时淘汰同样多字节的单元和行）< 30 ms；`touch` 5000 个访问时间超过 1 小时的单元（调用里写回 4096 个）< 20 ms | `cache/tables_write`、`cache/tables_evict`、`cache/tables_touch` |
| 跳转与查找引用（code_intel 的名字查找 ≤ 2 ms、`ci_ref` 查找 ≤ 5 ms） | 分库和表已打开时，一次 `read` 执行一条按索引查找、返回 1 行的语句（1 万个单元、20 万个定义）< 50 µs | `cache/tables_read` |

- `get` 一条 4 KiB < 50 µs（`cache/get_4k`）。
- 命名空间选 `Zstd` 之前，先用 `cache/zstd_1m/put`、`cache/zstd_1m/get`（1 MiB 文本的压缩与解压，只记录）确认读取仍在使用方的预算内。
- 上表各项是 `lectern-bench` 的基准（`just bench cache/`，[bench 设计「迁移」](bench.md#迁移)），夹具用 `test-support` 下的 `testing::Raw`、`testing::Zstd`、`testing::noise`、`testing::text`，自定义表用 `testing::tags`（code_intel 形状的三张表，`TABLES` 的配额 256 MiB，`SMALL_QUOTA` 是同一组表、配额 8 MiB）。`cache/evict` 和 `cache/memory` 随第 2 步生效，`cache/tables_*` 随第 3 步生效。`cache/memory` 在 bench 的 `memory` 目标里用子进程测量：SQLite 的页缓存和连接用 C 的分配器，计数分配器看不到，所以测的是子进程打开分库、读完之后的常驻增量（Linux 的 `VmRSS`，macOS 的 `phys_footprint`，见 [bench 设计「进程内基准」](bench.md#进程内基准)）。第 1 步的实现 PR 用 `#[ignore]` 的 `perf_smoke_cache` 测量，bench 第 1 步把它移进 `crates/bench` 后删除。

## 测试

全部在 Linux 上运行（`just test -p lectern-cache`），用 `tempfile` 的临时目录，不依赖 GPUI 和 macOS。`test-support` feature 提供可手动推进的时钟（访问时间、60 秒的写入暂停、目录清理）和故障注入（对写连接设置 `max_page_count` 制造 `SQLITE_FULL`；在写事务中途暂停的钩子）：`testing::ManualClock`、`Cache::with_clock`、`Cache::set_ui_thread_panics`、`Partition::file`、`Partition::set_max_page_count`、`Partition::set_write_hook`；另有 bench 用的 `testing::Raw`、`testing::Zstd`（`test.raw`、`test.zstd` 命名空间的原样字节）和 `testing::noise`、`testing::text`（不可压缩与可压缩的确定数据），见「性能」。

- 构造（`new_*`）：`Cache::new` 加 `app()`、`repo()`、`folder()` 之后 `root` 不存在；第一次 `get` 之后才出现文件。
- 线程（`ui_thread_*`）：在 `ui_thread` 上调用，debug 构建 panic；`test-support` 可以关掉这个 panic，用来检查 release 的行为：不打开文件，读返回 None，写返回 `WrongThread`。
- 读写（`roundtrip_*`）：`put` 后 `get`、覆盖、`remove`、空值、8 MiB 的值、超过 `MAX_VALUE` 和 `MAX_KEY` 时 `TooLarge`、`get_many` 的结果顺序；键的编码是单射（分段不同的键编码不同，insta 快照记录编码）。
- 命名空间（`namespace_*`）：同一个键在不同命名空间里互不影响；版本加一后旧条目不命中，之后的写事务逐步删除旧条目、`meta.bytes` 随之减少；旧版本程序（低版本）读不到高版本写的条目。
- 批量（`batch_*`）：一批多条一起可见；中途注入 `SQLITE_FULL` 时整批都不可见；超过 `MAX_BATCH` 整批 `TooLarge`；同一批里同键多次写取最后一次。
- 淘汰（`evict_*`）：超出预算后删掉 `atime` 最旧的条目、保留刚被读过的；小写入的事务删 1 MiB，大写入删它写入的字节数、最多 16 MiB，超出预算时分库不增长；`retired` 的条目先于预算淘汰被删；空闲页超过 32 MiB 后每个写事务归还 512 页，文件变小。
- 访问时间（`touch_*`）：60 分钟内的命中不产生写入（读连接之外没有写事务）；时钟推进 61 分钟后命中，下一个写事务写回 `atime`；写事务失败时待写集合保留；`close` 写回，drop 不写回；待写集合满 4096 后不再增长。
- 打开（`open_*`）：`user_version` 更旧、更新，`application_id` 不同，`meta` 路径不同（复制别的分库的文件过来），文件是随机字节，文件被截断，文件中间的页被改写（读到该页时 `SQLITE_CORRUPT`）：都删除重建，之后读写正常；同一次运行里第二次损坏后分库停用、不再做 IO；`root` 不可写时停用、不 panic。
- 磁盘满（`disk_full_*`）：`put` 返回 `DiskFull`，读照常；60 秒内的写入不尝试；时钟推进后去掉限制，下一次写入成功并按降低后的预算淘汰。
- 占用（`busy_*`）：另一个连接（模拟另一个进程）持有 `BEGIN IMMEDIATE` 时，`put` 在 100 ms 加余量内返回 `Busy`，`get` 照常命中。
- 并发（`concurrency_*`）：4 个线程读、1 个线程写，结果都是某个已提交的版本；写事务被钩子暂停时，另一个线程的 `get` 照常返回。
- 分库（`partitions_*`）：同一个 `common_dir` 得到同一个文件；`repo` 和 `folder` 对同一路径得到不同文件；打开第 3 个项目分库时最久没用的那个的连接被关闭，`Stats::connections` 不超过 6，应用级分库不被换出；换出后再读照常命中；换出时写回访问时间、等正在进行的调用结束；多个线程同时读写 5 个项目分库时连接数不超过 6。
- 目录清理（`sweep_*`）：修改时间（`File::set_modified`）早于 90 天的项目分库被删除；超过总预算时从最旧的删起；打开中的分库和应用级分库不删；一次运行只清理一次。
- 内存（`memory_*`）：每个连接的 `PRAGMA cache_size` 是 `-1024`，读连接的 `query_only` 为 1。
- 崩溃（`crash_*`）：子进程写入并提交后 `std::process::abort()`，父进程重新打开，已提交的条目都在、`PRAGMA integrity_check` 为 `ok`。
- 压缩（`compression_*`）：`Zstd` 命名空间的大值压缩存储、读回一致；小于 4 KiB 或压不下来的值以原样存储（`codec = 0`）。
- 自定义表（`tables_*`，夹具是测试里 code_intel 形状的三张表，检查文件时用 `PRAGMA foreign_key_check` 找不属于单元的行）：
  - 登记：取句柄不做 IO；第一次读时建表、`custom` 记下表名、建部分索引；版本加一后旧表删除、旧版本的命名空间为 `retired`、它的单元在之后的写事务里删除、`meta.bytes` 随之减少、代数加一，旧版本程序再打开时同样重建；违反规则的登记（没有外键、外键不级联、外键列没有索引、建了触发器、语句或 `schema` 编译失败）什么也不建，读 None、写 `Io`，分库不停用。
  - 读写：写入后按名字读回，`Param::New` 换成单元 ID，固定占位符里的 NULL 不匹配，`Row` 的类型不符时为 None；`create` 替换同键的单元和它的行；`remove`、`remove_unit` 删除单元和它的行；使用方的语句出错（引用不存在的单元、违反 UNIQUE、参数个数不对）时整次回滚、返回 `Io`，连续 5 次之后分库仍可用；`query` 里的写语句和 `Param::New` 失败、不停用分库；超过 `MAX_KEY`、`MAX_BATCH` 时 `TooLarge` 且不打开分库；UI 线程上不做 IO。`meta.bytes` 等于 `entry.size` 之和，`custom.bytes` 等于这个命名空间的 `entry.size` 之和。
  - 访问时间：一小时内的 `touch` 被记下，但写回不改变 `atime`；超过一小时的由下一个写事务写回；5000 个单元的 `touch` 在调用里写回 4096 个，余下的由下一个写事务写回。
  - 淘汰：普通条目的写入引起的预算淘汰删除最旧的单元和它们的行，代数加一；256 KiB 配额的命名空间写入 2 MiB 之后，`custom.bytes` 始终不超过配额，留下的是最新的单元，同一分库里 512 KiB 的条目一条也没有被淘汰；配额降低后，一次小写入删 256 KiB，大写入删它写入的字节数，命名空间不增长，之后回到配额以内。
  - 代数：读、`touch`、只新增单元、`close` 之后再打开不改变代数；文件在关闭期间被删除、`user_version` 不符被重建时加一；同一分库的另一个句柄读到同样的值，下一次运行从 0 开始。
  - 回滚与崩溃：写入中途 `SQLITE_FULL` 时三张表的行数、单元数都不变，没有不属于单元的行；子进程在写事务提交之前 `abort()`，父进程重新打开后只有已提交的单元和行，`PRAGMA integrity_check` 为 `ok`，`custom.bytes`、`meta.bytes` 与条目一致。

## 未决问题

没有需要产品决定的问题。下面一条已于 2026-10-18 决定，产品负责人交给负责推进的 agent，按默认值：

1. **设置页是否显示缓存占用并提供「清除缓存」。** 选 (a)，P1 不做：缓存可以随时删除，需要时用户删除应用缓存目录下的 `cache/`；预算用上文的默认值，不可配置。
   - 不采用 (b)：P1 在设置页显示各分库的占用、提供清除按钮和总预算的选项，要加界面字符串和设置项，数据来自 `Cache::stats`。以后要做时只增加接口（按分库删除），不改本文的存储设计。

技术问题都已在上文决定，没有其他未决项。

## 验收

分三步实现，每步一个 PR，都写 `Refs #17` 和 `Refs #31`（本 crate 不关闭这两个 issue）。每一步都要 `just check`、`just test -p lectern-cache`、`just deny` 通过。第 3 步是 code_intel 的前置条件 C（[code_intel 设计「验收」](code_intel.md#验收)）。

1. **存储、键值接口与生命周期**（[#82](https://github.com/SoloJiang/lectern-app/pull/82)）：根 `Cargo.toml` 按 ADR 0027 加入 `rusqlite`。
   - `just test -p lectern-cache` 里 `new_*`、`ui_thread_*`、`roundtrip_*`、`namespace_*`、`batch_*`、`open_*`、`disk_full_*`、`busy_*`、`concurrency_*`、`crash_*`、`compression_*`、`memory_*` 通过。
   - `cargo tree -i rusqlite -e normal --depth 1` 只列出 `lectern-cache`。
   - `python3 scripts/check_architecture.py` 通过：本 crate 不依赖 GPUI、`platform`、`executor`。
   - `rg 'load_extension|thread::spawn' crates/cache/src --glob '!**/tests*'` 没有结果：不加载扩展，不自建线程。
   - `perf_smoke_cache`（`cargo test --release -p lectern-cache -- --ignored perf_smoke`）的读写各项在「性能」一节的预算内（已满足；这个测试之后移进 bench，见「性能」）。
2. **淘汰、访问时间与分库管理**：
   - `evict_*`、`touch_*`、`partitions_*`、`sweep_*` 通过。
   - 加入 bench 的 `cache/evict` 与 `cache/memory` 两项基准（`cache/evict` 原为 `cache/evict_16m`，见「与设计的差异」第 2 步），在 `crates/bench/src/budgets.rs` 里改为生效，在预算内。
3. **自定义表**（[#111](https://github.com/SoloJiang/lectern-app/pull/111)）：
   - `tables_*` 通过，第 1、2 步的测试照常通过（`memory_*` 的 PRAGMA 快照多了 `foreign_keys = 1`，`user_version` 为 2）。
   - `cargo tree -i rusqlite -e normal --depth 1` 仍只列出 `lectern-cache`；没有新增依赖。
   - `rg 'load_extension|thread::spawn' crates/cache/src --glob '!**/tests*'` 没有结果。
   - bench 的 `cache/tables_write`、`cache/tables_read`、`cache/tables_touch`、`cache/tables_evict` 加入预算表并生效，在预算内；第 1、2 步的 `cache/*` 仍在预算内。

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

第 2 步，同一台机器。

- `just check`、`just test`（合并 bench 第 1 步之后，整个工作区 858 个测试通过，跳过 1 个）、`just doctest`、`just deny`、`just bench`（冒烟）全部通过。
- `just test -p lectern-cache`：67 个测试通过。第 1 步的 47 个照常通过；新增 `evict_*` 4、`touch_*` 6、`partitions_*` 6、`sweep_*` 4。`partitions_concurrent_calls_stay_within_the_connection_limit`（6 个线程在 5 个项目分库上交替读写，期间不断换出）和 `partitions_swap_out_waits_for_calls_in_progress` 连续运行 30 次没有失败或卡住。
- `evict_returns_free_pages_over_32_mib`：删掉 40 MiB 后，同一个事务的 `page_count` 减少 512 页加上它们的指针映射页（`auto_vacuum` 下每页约覆盖 1000 页），之后每个写事务再归还 512 页，直到空闲页不超过 32 MiB；关闭最后一个连接时检查点把文件截到 `page_count × 4096`。
- `rg 'load_extension|thread::spawn' crates/cache/src --glob '!**/tests*'` 没有结果；`python3 scripts/check_architecture.py` 通过；没有新增依赖，Cargo.lock 不变。
- bench 的 `cache/*`（`just bench --measure cache/`，运行三次）：`cache/evict` 中位数 6.4–6.8 ms（p95 38–44 ms，65–71 个样本），`cache/memory` 3.86–3.99 MB（每次 5 个子进程，最大 4.20 MB），都在预算内；`just bench cache/` 的冒烟运行通过。其余各项：`get_4k` 3.8–3.9 µs、`get_many_50` 87–90 µs、`get_1m` 1.04–1.09 ms、`open_existing` 0.43–0.45 ms、`open_new` 3.5–4.2 ms、`put_4k` 59–72 µs、`batch_1000` 6.8–8.4 ms、`zstd_1m` 写 5.7–5.8 ms、读 0.86–1.08 ms，都在预算内。同一个子进程里计数分配器的存活字节只增加了 5.6 KB，确认 SQLite 的内存不经过它。
- 实现时先用 `perf_smoke_cache` 测量（运行三次；分库在临时目录，即本机的 ext4 虚拟磁盘；另在 tmpfs 上运行一次，排除磁盘），之后按 bench 的迁移移进 `crates/bench`，参考项没有移：

  | 项 | 预算（M 系列 Mac） | 中位数 | p95 | 最慢 | tmpfs 中位数 |
  |---|---|---|---|---|---|
  | `evict`：一次小写入，淘汰 1 MiB 的 1 KiB 条目 | < 30 ms | 6.1–6.2 ms | 33–38 ms | 57 ms | 4.8 ms |
  | `evict`：同上，64 KiB 条目 | < 30 ms | 1.2–1.3 ms | 24–27 ms | 37 ms | 0.33 ms |
  | `memory`：应用级分库和 2 个项目分库各读 50 MiB 之后的常驻增量（子进程，5 次的中位数） | ≤ 8 MiB | 3.77–3.84 MiB | | 3.88 MiB | 3.80 MiB |
  | `open_new`（每次运行第一次打开，含目录清理） | < 10 ms | 3.6–3.7 ms | 5.3–7.3 ms | 66 ms | 0.87 ms |
  | `open_existing`（同上） | < 5 ms | 0.54–0.99 ms | 1.3–2.0 ms | 2.2 ms | 0.51 ms |
  | `swap_out`（参考：打开第 3 个项目分库，换出刚写过 1 MiB 的分库） | 无 | 13–16 ms | 15–36 ms | 271 ms | 1.8 ms |
  | `batch_16m`（参考：1.5 万条 1 KiB 一批，预算以内） | 无 | 191–213 ms | 211–240 ms | 243 ms | 128 ms |
  | `evict_16m`（参考：同样一批写进超出预算的分库，淘汰 16 MiB） | 无 | 287–300 ms | 308–332 ms | 357 ms | 174 ms |

  `evict` 的 120 多个样本里，前十几个只淘汰，之后每个还归还 2 MiB 空闲页。p95 超出 30 ms 的样本是提交时的自动检查点（每 1000 页一次，要 fsync），tmpfs 上 p95 是 9.3 ms 和 5.2 ms。`memory` 的增量里约 3 MiB 是页缓存（6 个连接，各自没有读满 1 MiB），其余是连接和预编译语句；每次读出的值在下一次读之前释放。第 1 步的读写各项重新测过，仍在预算内，数字与第 1 步相近。没有在 macOS 上测量。
- 按第 1 步的设计（每个写事务最多淘汰 16 MiB、归还 8 MiB 空闲页）实测过一次小写入带 16 MiB 淘汰，超出 30 ms 的预算，所以改了淘汰的上限，数字与理由见「与设计的差异」第 2 步。
- `platform::process::phys_footprint` 的 macOS 实现由 `cargo clippy -p lectern-platform --all-targets --all-features --target aarch64-apple-darwin -- -D warnings` 交叉检查通过，没有运行；`process_*` 测试在 Linux 上只有返回 `Unsupported` 的一个。
- 没有验证的：macOS 上没有运行，`cache/*` 由 macOS CI 判定。

第 3 步，同一台机器。

- `just check`、`just test`（整个工作区 1070 个测试通过，跳过 1 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过；`just bench cache/` 的冒烟运行通过。
- `just test -p lectern-cache`：88 个测试通过。第 1、2 步的 67 个照常通过，`memory_connection_settings` 的写连接和读连接两个 PRAGMA 快照多了 `foreign_keys = Integer(1)`、`user_version` 从 1 变为 2，逐个看过；新增 `tables_*` 21 个（其中 `tables_crash_child_aborts_inside_a_write` 是子进程的入口，没有环境变量时直接返回），连续运行 5 次没有失败。
- `cargo tree -i rusqlite -e normal --depth 1` 只列出 `lectern-cache`；`rg 'load_extension|thread::spawn' crates/cache/src --glob '!**/tests*'` 没有结果；`python3 scripts/check_architecture.py` 通过；没有新增依赖，Cargo.lock 不变。
- bench 的 `cache/tables_*`（`just bench --measure cache/tables`，运行三次；p95 和样本数取第三次）：

  | 项 | 预算（M 系列 Mac） | 中位数 | p95 | 样本数 |
  |---|---|---|---|---|
  | `tables_write`：64 个单元及其行一次写入 | < 15 ms | 8.7–10.1 ms | 29 ms | 75 |
  | `tables_read`：一次 `read`，按索引取 1 行 | < 50 µs | 7.4–10.4 µs | 8.2 µs | 1033 |
  | `tables_touch`：5000 个过期单元，调用里写回 4096 个 | < 20 ms | 8.4–8.6 ms | 24 ms | 94 |
  | `tables_evict`：同一批写进超出配额的命名空间 | < 30 ms | 13.1–13.6 ms | 34 ms | 57 |

  p95 超出预算的样本与其他写入一样，是提交时的自动检查点（要 fsync）。`tables_evict` 比 `tables_write` 多出的约 4.5 ms 是淘汰约 64 个单元和它们的 4000 行。同一次 `just bench --measure cache/` 里第 1、2 步的各项仍在预算内，数字与第 2 步相近：`get_4k` 3.75 µs、`get_many_50` 97 µs、`get_1m` 1.04 ms、`open_existing` 0.45 ms、`open_new` 3.75 ms、`put_4k` 58 µs、`batch_1000` 5.7 ms、`evict` 6.53 ms、`memory` 4.29 MB。
- 没有进 bench 的测量：
  - 外键的代价：在 `cache/evict` 的夹具里另写 2000 个单元（`testing::tags`）之后，`cache/evict` 中位数 10.0–10.6 ms（两次运行），没有自定义表时 6.5 ms。
  - 配额淘汰的下限：按 1 MiB 的下限，一次写一个单元带 1 MiB 的配额淘汰，中位数 39.4 ms，因此改为 256 KiB（「与设计的差异」第 3 步）。
  - 第一次建表：分库里已有 20 万个 300 字节的条目时，第一次 `read`（建表、检查、建部分索引）44 ms；之后的 `read` 3.4 µs（release 构建，一次性的测试，没有提交）。
- 没有验证的：macOS 上没有运行，`cache/tables_*` 由 macOS CI 判定；code_intel 真实的表和语句由 code_intel 第 2 步接入时验证（`store_*`）。

访问时间的集合写回（[#17](https://github.com/SoloJiang/lectern-app/issues/17)，code_intel 第 2 步的 `code_intel/warm_100k` 在 macOS CI 上中位数 537、549 ms，超出 500 ms；其中大部分是 `touch` 逐行写回），同一台机器。

- `just check`、`just test`（整个工作区 1102 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过；`just bench cache/` 的冒烟运行通过。`just test -p lectern-cache`：89 个测试通过，新增 `tables_touch_write_back_spans_chunks_and_skips_recent_units`：2055 个单元跨三组写回（最后一组不满），一小时以前写入的单元换成新的访问时间，一小时以内写入的保持不变，已删除的单元跳过；写回语句的查询计划按主键逐个查找，不扫描 `entry_atime`。原有的 `touch_*` 和 `tables_touch_*` 照常通过。
- `cache/tables_touch`（`just bench --measure cache/tables_touch`，各三次）：逐行写回 8.54、8.54、8.63 ms；集合写回 7.28、7.34、7.24 ms。
- 更大的规模（一次性的测量，没有提交）：5 万个 `testing::tags` 单元，每次 `touch` 500 个、共 100 次。访问时间都已过期（全部要改）时，逐行 154 ms，每组 256 个 138 ms，每组 1024 个 127 ms；访问时间都在一小时以内（一行也不改）时，逐行 179 ms，集合 8.0 ms。用 `json_each` 一次传入全部 ID 时分别是 151 ms 和 13.7 ms，所以选了固定占位符、每组 1024 个。
- callgrind（`tables_touch`，集合写回）：要改的行占绝大部分开销，几乎都在索引维护上。`sqlite3BtreeInsert` 45%，`sqlite3BtreeIndexMoveto` 44%，页的分裂 `balance` 21%，`sqlite3BtreeDelete` 12%。每改一行，`entry_atime` 和配额淘汰的部分索引 `entry_ns<id>` 都要删掉旧键、插入新键。语句本身的开销（绑定、单步执行、重置）是逐行写回时省下的那部分。

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
- `bench` 的 `cache/*` 没有加入，理由与 git 第 1 步的 `git/open` 相同；由 `#[ignore]` 的 `perf_smoke_cache` 在 release 下测量，结果见「实际验证」。bench 第 1 步把它的各项移进 `crates/bench` 的 `cache/*`，`perf_smoke_cache` 随之删除；`zstd_1m` 拆成 `cache/zstd_1m/put` 和 `cache/zstd_1m/get` 两条结果，`testing` 加了 bench 用的条目类型和数据生成函数（「性能」已改）。

第 2 步：

- 每个写事务的淘汰上限从固定的 16 MiB 改为「本事务写入的字节数，至少 1 MiB、至多 16 MiB」（「淘汰」一节已改）。按原设计实测（Linux 虚拟机，见「实际验证」），一次小写入带 16 MiB 淘汰：1 KiB 的条目中位数 164–176 ms，放到 tmpfs 上排除磁盘仍是 94 ms，其中逐行删除 1.6 万行就占 43 ms（每行要改表和两个索引，开销按行数而不是字节数增长）；64 KiB 的条目 63–73 ms（tmpfs 26 ms）。`git.diff` 的值只有几十到几百字节，16 MiB 是十万行以上，固定的字节上限在任何机器上都到不了 30 ms。新规则保留原设计的两个性质：超出预算时每个事务删掉的不少于它写入的，所以最多短暂超出预算 16 MiB、之后只减不增；淘汰的开销与这次写入本身同一量级。代价是降低预算之后（如磁盘写满时降到 75%）回到预算以内要更多次写入，每次小写入 1 MiB。
- 每个写事务归还的空闲页从 2048 页（8 MiB）改为 512 页（2 MiB）。实测 `incremental_vacuum(2048)` 一次 11–118 ms，空闲页越多越慢：SQLite 每移动一页都要在空闲页列表里找一个更靠前的位置。512 页一次在 32 MiB 左右的空闲页下约 3 ms。
- 性能一项 `cache/evict_16m`（带 16 MiB 淘汰的写事务 < 30 ms）改为 `cache/evict`（超出预算的分库里一次小写入 < 30 ms，淘汰 1 MiB，空闲页超过 32 MiB 时另外归还 2 MiB），预算不变。实现时的 `perf_smoke_cache` 另外记下一批 16 MiB（1.5 万条 1 KiB）写入超出预算的分库、同时淘汰 16 MiB 的耗时（`evict_16m`，只作参考），与同样一批写入预算以内的分库（`batch_16m`）对比；这两项和 `swap_out` 不在预算表里，没有移进 bench。
- 实现时 `perf_smoke_cache` 的 `open_new`、`open_existing` 改为每次用新的 `Cache` 测量（bench 的 `cache/open_*` 本来就是这样），即每次运行第一次打开（含目录清理）：第 1 步在同一个 `Cache` 里依次打开 50 个分库，第 2 步加上连接上限之后，从第 3 个起每次打开都要换出一个分库。换出的耗时另记为 `swap_out`（只作参考）：主要是关闭最后一个连接时的检查点和 fsync，被换出的分库刚写过 1 MiB 时，磁盘上中位数约 15 ms，tmpfs 上约 2 ms。它发生在打开第 3 个项目的调用里，计入「打开项目 < 100 ms」，不计入「新建分库 < 10 ms」。
- 访问时间写回的值是写回的时刻，不是读的时刻：待写集合只存 `id`（4096 个 32 KiB），读与写回之间没有别的写入，淘汰顺序不变。设计没有写用哪个时刻。
- 最后一个 `Cache` drop 时不写回访问时间，只有 `Cache::close` 和换出时写回：drop 可能发生在任何线程上，设计的「生命周期」第 4 步也只说 `close` 多写回访问时间。
- 换出的实现：正在打开的项目分库在打开文件之前先占一个名额（按会话编号记录），名额满时取最近一次调用最早的分库，关闭它之后再打开自己的文件；打开失败时让出名额，会话关闭（换出、损坏重建、停用、`close`）时自动让出。`Budgets::open_projects` 为 0 时按 1 处理。设计只写了「关闭最久没用过的」，没有写并发打开时怎样不超过上限。
- 目录清理的细节：一个项目分库的修改时间取三个文件里最新的（WAL 模式下写入先到 `-wal`）；总量计入应用级分库的文件，但不删除它；`repo/`、`folder/` 下不是分库的文件不动；删除失败只写一行 stderr。清理在第一次打开的调用里、打开文件之前做，同时打开的其他分库等它完成，所以不会删掉正在打开的文件。
- 测试的 `ManualClock` 改为从真实的当前时刻开始（第 1 步是固定的 2027 年 1 月）：目录清理拿文件的修改时间与这个时钟比较，固定的起点会让测试里刚建的文件看起来已有 90 天。
- `cache/memory` 在子进程里测常驻增量（bench 的 `memory` 目标再启动自己，见 [bench 设计「进程内基准」](bench.md#进程内基准)），不用 `memory` 目标的计数分配器：SQLite 的页缓存用 C 的分配器，计数分配器看不到（同一个子进程里只增加 5.6 KB，常驻增量约 3.9 MB）；用子进程而不是本进程的常驻大小，是因为在写过夹具的进程里，读取会复用写入时释放的堆，增量偏小（同一进程里测得 0.1 MiB）。macOS 上的 `phys_footprint` 经 `platform` 新增的 `process::phys_footprint`（架构规则 6，[platform 设计「进程」](platform.md#进程)），Linux 上直接读 `/proc/self/status`。

第 3 步（「自定义表」一节在本 PR 里写成，与实现同时合并；下面是它与 [code_intel 设计「对其他 crate 的要求」](code_intel.md#对其他-crate-的要求)里 `cache` 一项的差异，以及实现时补上的细节）：

- 要求第 1 条写「版本变化时删掉旧表和它的全部单元后重建」：表在建表的事务里删除，旧单元因此没有行、查询看不见，单元本身按 `retired` 命名空间的规则每个写事务删 256 条，不在建表的事务里一次删完。理由与命名空间升级相同：单个事务的工作量有上限，10 万个单元不在一次调用里删除。
- 要求第 2 条允许「外键级联，或使用方登记的删除语句」，选了外键级联：删除条目的路径（预算淘汰、`retired` 清理、解码失败、`remove`）出现在任何写事务里，包括这次运行里没有取过 `TableSet` 的分库；登记的删除语句只在进程里，外键在文件里，任何路径都留不下行。代价是每个连接 `PRAGMA foreign_keys = ON`，以及删除条目时按外键列查各张自定义表（「写入、配额与淘汰」的实测）。code_intel 设计「存储表」的建表语句已在本 PR 里补上外键子句。
- 要求第 3 条的配额淘汰沿用预算淘汰的「每个事务最多删本事务写入的字节数」，但下限从 1 MiB 改为 256 KiB：按 1 MiB 的下限实测（`cache/tables_evict` 的早期版本，一次写一个单元），中位数 39 ms，超过 30 ms；单元带行，同样的字节数比条目多很多行。`cache/tables_evict` 改为测 code_intel 的一批（64 个单元）写进超出配额的命名空间，中位数 13 ms。
- 要求第 5 条的 `touch` 不读 `entry.atime`，所以写回语句加了「`atime` 早于写回时刻 60 分钟以上」的条件；读命中记下的条目本来满足这个条件，第 2 步的行为不变。待写集合满时 `touch` 在调用里写回，而不是像读命中那样不再记录：热启动时 10 万个单元都要标记，只记 4096 个会让其余单元按旧的访问时间被淘汰。
- 要求第 8 条只写了「被淘汰或删除、或分库被删除重建时加一」。另外在分库停用时，以及本次运行打开过的分库再打开时发现是新文件（目录清理或用户删除了文件）时加一：这两种情况下之前的单元同样不在了。本次运行第一次打开时新建文件不加一。
- 设计没有写的：单元 ID 是 rowid，删除后可能重用（测试 `tables_create_replaces_the_unit_of_a_key` 里新单元拿到了旧单元的 ID），所以「登记的规则」要求代数变化后不再沿用旧 ID；`Row::unit`、`Unit::from_id` 和 `TableWrite::remove_unit` 是为使用方从自己的表里读出单元 ID 后使用而加的；登记违规时只写 stderr、不 panic，测试可以检查这时什么也没建。`Stats` 没有新增计数：表的读不计入 `hits` / `misses`，写入计入 `written` 和 `dropped`，淘汰计入 `evicted_bytes`。
- `testing` 增加 `tags`（code_intel 形状的三张表和写入一个单元的函数），供 bench 的 `cache/tables_*` 使用；`crates/cache/src/tests/tables.rs` 的测试用自己的登记（多个前缀和配额）。

访问时间的集合写回：

- 「访问时间」原来写的是逐个 ID 执行 `… WHERE id = ? AND atime < ?`。现在每 1024 个 ID 执行一条 `UPDATE entry SET atime = ?1 WHERE atime < ?2 AND id IN (…)`（「访问时间」已改）。精度规则不变：一小时以内写入或写回过的条目不改。读命中和 `touch` 的写回都走这条路径，写在自己的事务里或其他写事务的开头，位置不变。这条语句占预编译语句缓存的一条。
- 要改的行很多时（第二天的热启动，所有单元都过期），收益只有约 15%：开销在两个以 `atime` 为键的索引上（见「实际验证」），只改写回的方式省不掉。要再快，就得减少以 `atime` 为键的索引（比如让配额淘汰改用 `entry_atime` 加 `ns` 过滤），这会改变表结构和 `SCHEMA_VERSION`，不在这次改动里。
