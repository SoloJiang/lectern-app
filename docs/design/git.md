# git 设计

状态：实现中。第 1–3 步已实现：第 1 步（仓库、worktree、引用与 index，[#76](https://github.com/SoloJiang/lectern-app/pull/76)），第 2 步（树之间的改动集、hunk 与展示行，[#79](https://github.com/SoloJiang/lectern-app/pull/79)），第 3 步（工作区比较、只读刷新与指纹，[#87](https://github.com/SoloJiang/lectern-app/pull/87)）；第 4 步尚未实现。`Worktree::watch_source` 随 fs_watch 的第 2 步实现（[#95](https://github.com/SoloJiang/lectern-app/pull/95)）。`Worktree::canonical_oid`（[code_intel 设计「对其他 crate 的要求」](code_intel.md#对其他-crate-的要求)，前置条件 E）在 [#110](https://github.com/SoloJiang/lectern-app/pull/110) 实现。hunk 与展示行的字节偏移（[ADR 0034](../adr/0034.md)）在 [#122](https://github.com/SoloJiang/lectern-app/pull/122) 实现。fs_watch 过滤器的内存归属与 10 万文件的份额（[ADR 0035](../adr/0035.md)）随 bench 的基准加入，见「性能」「实际验证」。

对应 issue [#14](https://github.com/SoloJiang/lectern-app/issues/14)（P1）；结构化 diff [#21](https://github.com/SoloJiang/lectern-app/issues/21) 在 P2，本文只留扩展点。`git` 只读地读取仓库：列出 worktree、引用和历史，计算树之间和工作区相对某棵树的改动，给出 hunk 和统一 / 双栏视图共用的展示行（[技术调研「代码与 Diff 查看」](tech-research.md#3-代码与-diff-查看)、[「性能要点」](tech-research.md#性能要点)）。依赖选型见 [ADR 0024](../adr/0024.md)。

## 范围

- 做：
  - 发现和打开仓库：worktree 根、git 公共目录、对象格式（SHA-1 / SHA-256）。
  - 列出主 worktree 和 linked worktree：HEAD、分支、锁定、prunable。
  - 引用与历史：解析引用、提交信息、最近提交、merge-base、祖先判断、分支的上游和推送目标、远端、根提交、stash 的结构、reflog 行的解析（供 `usage` 按 [ADR 0020](../adr/0020.md) 归属）。
  - index 里的路径列表，供 `project` 的文件列表和 `fs_watch` 的过滤。
  - 改动集：树 ↔ 树（提交、分支相对 merge-base），树 ↔ 工作区（含未跟踪文件）；重命名检测、二进制判断、行 diff、单文件和整体的截断。
  - 展示行：统一视图与双栏视图共用的行模型、上下文折叠、增删配对、词级差异。
  - 以两端 blob OID 为键的 hunk 缓存；工作区内容按 git 的 blob 格式计算 OID，不写对象库。
- 不做：
  - 绘制 diff、Fluent 文案、UI 探针节点：`review_view`。本 crate 只返回枚举，不产生界面字符串。
  - 归因、Drifted、已看状态、噪声文件列表：`review`。本 crate 只给出 `.gitattributes` 里的 `linguist-generated`。
  - 文件监听、worktree 修订号、reflog 新增行的时刻：`fs_watch`。分支的「一次使用」与用量归属：`usage`。
  - 安全的文件读取（`O_NOFOLLOW_ANY`）：`platform`（架构规则 6），本 crate 只调用它的平台无关接口。
  - 缓存的存储与淘汰：`cache`。代码高亮：`code_intel` / `review_view`。
  - 任何写操作：提交、暂存、切换分支、写 index 或对象库。见「未决问题」1。
- 本 issue 之外：
  - 结构化 diff（[#21](https://github.com/SoloJiang/lectern-app/issues/21)），扩展点见「结构化 diff 的扩展点」。
  - MR 的 diff：`forge`（[#25](https://github.com/SoloJiang/lectern-app/issues/25)）。
  - Analyze 快照的输入与审阅对象的指纹：随 [#66](https://github.com/SoloJiang/lectern-app/issues/66) 设计，见「Analyze 快照」。
  - 实现 `services` 的改动来源接口：等 `services` 的设计文档确定，见「验收」第 4 步。

本 crate 在领域层，是普通的库 crate，不是插件：不实现 kernel 的 `Plugin`，不注册服务。`project`、`review`、`usage` 等领域 crate 直接依赖并调用它（[#47](https://github.com/SoloJiang/lectern-app/issues/47) 以后若决定把插件做成包装领域库的薄 crate，本 crate 仍是被包装的库）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gix` =0.88.0 | 对象库、引用、index、配置、属性与忽略规则、内置转换（`text` / `eol` / `ident` / `working-tree-encoding`）、树 diff 与重命名检测；工作区的目录遍历不用它，`dirwalk` feature 已去掉（[ADR 0028](../adr/0028.md)） | 纯 Rust，MIT OR Apache-2.0；feature 与使用边界见 [ADR 0024](../adr/0024.md)，`dirwalk` 除外；release 约 3 MB，`just deny` 通过 |
| `gix::diff::blob`（`gix-imara-diff` 0.3） | 行 diff 与词级 diff，Histogram 算法 | gix 维护的 imara-diff 副本，Apache-2.0；随 gix 一起升级，不再引入上游 `imara-diff`（ADR 0024） |
| `lectern-platform` | 安全读取和列出工作区文件（见「安全读取」） | 架构规则 6 |
| `lectern-cache` | hunk 的持久缓存 | 架构规则 5；`cache` 实现之前只用进程内的 LRU |
| `lectern-fs-watch` | 实现它的 `WorktreeSource` / `PathFilter`（`Worktree::watch_source`） | 领域层依赖基础设施层；见 [fs_watch 设计](fs_watch.md)「对其他 crate 的要求」 |
| `insta`（dev） | 改动集、hunk、展示行的快照 | 工作区统一依赖 |
| `rustix`（dev，只在 Linux） | FIFO 测试里的 `mknodat` | 已在 Cargo.lock（`platform` 依赖它），工作区统一依赖；标准库没有建 FIFO 的接口 |
| `tempfile`（可选，`test-support` 打开；dev） | `TestRepo` 的临时目录（见「测试」） | 已在 Cargo.lock；gix 0.88 在上面的 feature 下已能写对象、引用文件和 index，`TestRepo` 不需要额外的 gix feature，发布构建的 feature 不变 |

不依赖 `executor`：所有函数都是阻塞的普通函数，由调用方放进后台任务；取消用 `&AtomicBool`（与 gix 的中断约定相同）。本 crate 不自己创建线程。

### 安全读取

本 crate 用 `platform` 提供的平台无关接口读工作区，接口、数据结构和两个平台的实现以 [platform 设计](platform.md)为准；它先于本文的第 1 步实现，因为 `Worktree::read_file` 用到它，PR 写 `Refs #14`。本 crate 用到的部分：

```rust
// lectern-platform
pub struct SafeRoot { /* fd */ }
pub struct RelPath<'a>(&'a [u8]);          // 不变量与本 crate 的 `RepoPath` 相同
pub enum Read {
    File { meta: Meta, bytes: Vec<u8> },
    Symlink { meta: Meta, target: Vec<u8> },
    TooLarge { meta: Meta },                // 超过调用方给的上限，没读内容
    Unsupported { meta: Meta },             // 目录、FIFO、socket、设备文件，不读内容
}
pub enum SafeError { NotFound, PathThroughSymlink, Io(io::Error) }

impl SafeRoot {
    pub fn open(root: &Path) -> io::Result<SafeRoot>;
    pub fn stat(&self, rel: RelPath<'_>) -> Result<Meta, SafeError>;
    pub fn read(&self, rel: RelPath<'_>, max: u64) -> Result<Read, SafeError>;
}
```

结果的处理：`NotFound` 时 W 为没有；`PathThroughSymlink` 为 `Unavailable::PathThroughSymlink`；`Io` 为 `Unavailable::Unreadable`；`Unsupported` 的 `meta.kind` 是目录时按 index 条目区分（gitlink 按子模块，其余是路径被换成目录），其余为 `Mode::Special` + `UnsupportedType`。stat 比较用的 `gix_index::entry::Stat` 按 platform 设计「数据结构」的对照表从 `Meta` 构造。

两个平台都把解析限制在根目录之下，不只靠「不跟随符号链接」（它不阻止 `..` 走到根目录之上）：Linux 用 `openat2` 加 `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`（不可用时逐级 `openat`），macOS 用 `O_NOFOLLOW_ANY`，出错时逐级 `openat(…, O_NOFOLLOW | O_DIRECTORY)` 判定原因。Linux 的实现不是 macOS 专属能力，也放在 `platform`，供 CI 测试。`RelPath` 的不变量本身已经排除 `..` 和绝对路径，平台实现的限制是第二道防线。

## 概念

| 概念 | 说明 |
|---|---|
| 仓库（`Repo`） | 一个 git 公共目录。同一仓库的所有 worktree 共用一个 `Repo`；gix 句柄只在一次调用内存在（见「数据流与线程」）。 |
| worktree（`Worktree`） | 主 worktree 或 linked worktree：自己的 HEAD、index 和工作区。 |
| 基准 | 比较的旧一侧，总是一棵树：HEAD、某个提交（如会话开始时的 HEAD）或 merge-base。 |
| 改动集（`ChangeSet`） | 基准与目标（另一棵树或工作区）之间每个改动文件的状态、两侧的模式和内容标识、diff。不可变，`Arc` 共享。 |
| 规范内容 | 工作区文件经过 git 内置转换（to-git 方向）之后的字节，与 `git add` 会存进对象库的内容相同。工作区一侧的 blob OID 按规范内容计算。 |

## 接口

所有函数都阻塞（读对象库、index 或工作区），只在后台任务里调用，UI 线程上不调用（AGENTS.md「禁止事项」）。`Repo`、`Worktree`、`ChangeSet` 都是 `Send + Sync`，克隆是 `Arc` 克隆；它们不持有 gix 句柄（gix 的句柄不是 `Send`，见「数据流与线程」）。带 `cancel` 参数的函数在文件之间检查它，置位后返回 `GitError::Cancelled`，调用方丢弃结果。

### 对象 ID 与路径

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HashKind { Sha1, Sha256 }

/// 完整长度的对象 ID：SHA-1 20 字节、SHA-256 32 字节，不截短，不假定长度。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Oid { /* kind + [u8; 32] */ }

impl Oid {
    pub fn kind(&self) -> HashKind;
    pub fn as_bytes(&self) -> &[u8];                       // 20 或 32 字节
    pub fn to_hex(&self) -> String;                        // 40 或 64 位
    pub fn from_hex(kind: HashKind, hex: &str) -> Option<Oid>;
    /// 按 git 的 blob 格式（`blob <长度>\0` + 内容）计算 OID，不写对象库。纯计算。
    pub fn for_blob(kind: HashKind, content: &[u8]) -> Oid;
}

/// 相对 worktree 根、以 `/` 分隔的路径，原始字节（git 允许非 UTF-8 的名字）。
/// 不变量与 `platform::RelPath` 相同：非空、各级名字非空、没有 `.` 和 `..`、没有 NUL、不以 `/` 开头；
/// 只能经 `RepoPath::new` 构造（不满足时返回 None）。index 或树里不满足的路径（git 自己会拒绝，但 index 可能被篡改）
/// 不构造 `RepoPath`、不读工作区，也不进入 `ChangeSet::files`：原始字节记在 `Totals::invalid_paths` 里，
/// 视图列出并说明「路径无效，未读取」。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct RepoPath(Arc<[u8]>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    File, Executable, Symlink, Gitlink,
    /// 只出现在工作区一侧：git 不能记录的条目（FIFO、socket、设备文件、未跟踪的嵌套仓库目录）。
    /// 这时 `content` 一定是 `Unavailable`（`UnsupportedType` 或 `NestedRepository`）。
    Special,
}

/// 完整的引用名，如 `refs/heads/main`。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RefName(Arc<str>);
```

gix 的类型不出现在公开接口里（ADR 0024）。

### 仓库与 worktree

```rust
pub struct Location {
    /// git 公共目录的绝对规范路径。`cache` 按它区分仓库，同一仓库的 worktree 相同。
    pub common_dir: PathBuf,
    pub hash: HashKind,
    /// 裸仓库：没有主 worktree（仍可能有 linked worktree）。
    pub bare: bool,
}

pub struct Discovered {
    pub repo: Repo,
    /// `path` 所在的 worktree（id、工作区根、git 目录）；`path` 在裸仓库或 git 目录里时为 None。
    pub worktree: Option<WorktreeInfo>,
}

impl Repo {
    /// 从 `path` 向上查找仓库，不越过 `ceilings` 里的目录（与 `GIT_CEILING_DIRECTORIES` 的语义相同）。
    /// 不是仓库时返回 `Ok(None)`：普通文件夹也是项目（ADR 0020）。本 crate 不读环境变量：
    /// `project` / `app` 启动后从进程环境读一次 `GIT_CEILING_DIRECTORIES`、解析成路径列表后传进来（[project 设计](project.md)）。
    pub fn discover(path: &Path, ceilings: &[PathBuf]) -> Result<Option<Discovered>, GitError>;
    pub fn location(&self) -> &Location;
    /// 主 worktree 与全部 linked worktree，按 id 排序。读管理文件和各自的 HEAD，不读工作区。
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>, GitError>;
    /// 打开一个 worktree 的句柄：读它的 HEAD 和 worktree 级配置，index 用到时才读。
    pub fn worktree(&self, id: &WorktreeId) -> Result<Worktree, GitError>;
}

/// 带标签的 ID：主 worktree 与名字恰好是 `main` 的 linked worktree（`<公共目录>/worktrees/main`）不会相同。
/// 排序时 `Main` 在前，linked worktree 按名字的字节序。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum WorktreeId {
    Main,
    /// `<公共目录>/worktrees/<名字>` 的目录名，原样保存。
    Linked(Arc<str>),
}

pub struct WorktreeInfo {
    pub id: WorktreeId,
    pub is_main: bool,                 // 与 `id == WorktreeId::Main` 等价，方便调用方
    /// 工作区根。存在时是规范路径；prunable（根目录已不存在）时取管理目录里 `gitdir` 文件记录的路径去掉末尾的 `/.git`，
    /// 只做字面归并，不访问文件系统（与 [project 设计](project.md) 相同）。
    pub root: PathBuf,
    pub git_dir: PathBuf,              // 主 worktree 为公共目录，linked worktree 为 `<公共目录>/worktrees/<名字>`
    pub head: HeadState,
    pub locked: Option<String>,        // `locked` 文件里的原因，空文件为 Some("")
    pub prunable: bool,                // 工作区根已不存在
}

pub enum HeadState {
    /// `commit` 为 None：分支还没有提交（unborn）。
    Branch { name: RefName, commit: Option<Oid> },
    Detached { commit: Oid },
}
```

### 引用与历史

```rust
impl Repo {
    pub fn resolve(&self, name: &RefName) -> Result<Option<Oid>, GitError>;
    pub fn commit(&self, id: Oid) -> Result<CommitInfo, GitError>;   // 标题、作者、提交时间、父提交、树
    /// 从 `tip` 起按提交时间倒序（`git log` 的默认顺序），最多 `limit` 个。项目主页「最近提交」用。
    pub fn log(&self, tip: Oid, limit: usize, cancel: &AtomicBool) -> Result<Vec<CommitInfo>, GitError>;
    pub fn merge_base(&self, a: Oid, b: Oid, cancel: &AtomicBool) -> Result<Option<Oid>, GitError>;
    pub fn is_ancestor(&self, ancestor: Oid, of: Oid, cancel: &AtomicBool) -> Result<bool, GitError>;
    /// 本地分支，带上游和推送目标：按 `branch.<名>.remote` / `merge` / `pushRemote`、
    /// `remote.pushDefault`、`push.default` 解析出远端名、远端 URL 和远端分支（ADR 0020 的 MR 对应）。
    pub fn branches(&self) -> Result<Vec<BranchInfo>, GitError>;
    pub fn remotes(&self) -> Result<Vec<RemoteInfo>, GitError>;      // 名字、fetch URL、push URL
    /// `refs/remotes/<主远端>/HEAD` 指向的分支；没有时依次取存在的本地 `main`、`master`；都没有为 None。
    /// 主远端：`origin`，没有时取唯一的远端。
    pub fn default_branch(&self) -> Result<Option<RefName>, GitError>;
    /// 从 `tip` 可达的根提交，最多 `limit` 个。项目识别用（ADR 0020）。
    pub fn root_commits(&self, tip: Oid, limit: usize, cancel: &AtomicBool) -> Result<Vec<Oid>, GitError>;
    /// stash 提交的三部分：工作区提交、index 提交、未跟踪文件提交（`-u` / `-a` 时才有）。
    pub fn stash(&self, id: Oid) -> Result<StashInfo, GitError>;
    /// 一棵树里的全部路径和模式，不读内容（如 stash 的未跟踪文件提交，供 `fs_watch` 的不过滤清单）。
    pub fn tree_paths(&self, tree: Oid, limits: &EntryLimits, cancel: &AtomicBool)
        -> Result<Vec<(RepoPath, Mode)>, GitError>;
    /// 一个 blob 的完整内容；超过 `max` 字节时返回 `GitError::TooLarge`，不读入。
    pub fn blob(&self, id: Oid, max: u64) -> Result<Arc<[u8]>, GitError>;
}

/// 解析 reflog 的一行（`<旧> <新> <身份> <时间> <时区>\t<消息>`）。纯函数，任何线程。
/// `fs_watch` 交付带时刻的新增行，`usage` 用它解析（ADR 0020）。
pub fn parse_reflog_line(hash: HashKind, line: &[u8]) -> Result<ReflogEntry, ReflogParseError>;

pub struct ReflogEntry {
    pub old: Option<Oid>,              // 全 0 为 None
    pub new: Option<Oid>,
    pub time: GitTime,                 // 秒和时区偏移，原样
    pub action: ReflogAction,
    pub message: Arc<[u8]>,
}

/// 按 git 写入的消息前缀分类（git 不翻译 reflog 消息）。认不出的是 `Other`，不猜测。
pub enum ReflogAction {
    Commit { amend: bool, initial: bool },
    Checkout { from: Arc<[u8]>, to: Arc<[u8]> },   // `checkout: moving from A to B`，`git switch` 也写这一种
    Reset, Merge, Rebase, CherryPick, Pull,
    BranchCreated,
    BranchRenamed { from: RefName, to: RefName },
    Other,
}
```

### index

```rust
impl Worktree {
    pub fn id(&self) -> &WorktreeId;
    pub fn root(&self) -> &Path;
    /// 重新读 HEAD（HEAD 文件加一次引用解析）。`agents` / `usage` 在工具调用后观察分支时调用（ADR 0020）。
    pub fn head(&self) -> Result<HeadState, GitError>;
    /// index 里的全部路径，按 index 的顺序（路径字节序）。index 文件没变时返回同一个 Arc。没有 index 文件时为空。
    /// 展开 sparse 目录条目时受 `limits` 约束、检查 `cancel`；超过上限时返回 `GitError::TooManyEntries`。
    pub fn tracked(&self, limits: &EntryLimits, cancel: &AtomicBool) -> Result<Arc<TrackedPaths>, GitError>;
    /// 从工作区根出发、经 `SafeRoot` 读一个文件的原始字节（不做转换），最多 `max` 字节。
    /// 供 `project` 读清单文件等不需要 git 语义的读取，与本 crate 自己的读取遵守同样的安全规则。
    pub fn read_file(&self, path: &RepoPath, max: u64) -> Result<WorktreeFile, GitError>;   // 见下文 `WorktreeFile`
    /// 只读 index 文件末尾的校验和，不解析条目、不展开 sparse 目录；与 `TrackedPaths::checksum` 相同。
    /// 供调用方在不重建文件列表的情况下检查缓存是否过期（`project` 对非当前 worktree 的缓存检查）。没有 index 文件时为 None。
    pub fn index_checksum(&self) -> Result<Option<Oid>, GitError>;
    /// 把 `bytes` 当作 `path` 上的普通文件，按这个路径的属性和配置做 to-git 方向的内置转换，返回规范内容的 blob OID
    /// （与 `git add` 记录的相同），不写对象库。设置了 clean / process 过滤器的路径返回 None。见下文「规范内容的 OID」。
    pub fn canonical_oid(&self, path: &RepoPath, bytes: &[u8]) -> Result<Option<Oid>, GitError>;
}

impl TrackedPaths {
    /// index 文件末尾的校验和（index 用的哈希算法），作为文件列表派生数据的缓存键；没有 index 文件时为 None。
    pub fn checksum(&self) -> Option<Oid>;
    pub fn len(&self) -> usize;
    /// 按路径字节序；冲突路径的各个 stage 只出现一次。sparse index 的目录条目已展开为其中的文件（见下）。
    pub fn iter(&self) -> impl Iterator<Item = &TrackedEntry>;
    pub fn contains(&self, path: &[u8]) -> bool;            // 二分查找
    pub fn contains_dir(&self, dir: &[u8]) -> bool;         // 有路径在这个目录下
}

pub struct TrackedEntry {
    pub path: RepoPath,
    pub mode: Mode,
    pub oid: Oid,
    /// skip-worktree 条目，以及从 sparse 目录条目展开出来的条目：不在工作区里，按未改动处理，不报告为删除。
    pub skip_worktree: bool,
}
```

sparse index（`index.sparse`）里，sparse checkout 之外的目录只有一个模式为 `040000` 的目录条目，记录的是树 OID。gix 0.88 读 index 时不展开它们，本 crate 自己在内存里展开：按目录条目的树 OID 从对象库读出其中的全部文件，作为 `skip_worktree` 的条目放进 `TrackedPaths`，与完整 index 里的 skip-worktree 条目相同。展开只在内存里进行，不写回 index，index 文件的 sparse 状态不变；展开的条目数计入调用方传入的 `EntryLimits`，读树时检查 `cancel`；`changes` / `refresh` 内部读 index 时用 `Limits::entries` 和它们自己的 `cancel`。`Repo::worktree` 只读 HEAD 和配置，不读 index，所以不需要这两个参数。所以「基准树 ∪ index」的比较（见「工作区比较」）看到的是逐个文件的条目，sparse 目录下的文件不会被当成删除。

`fs_watch` 是基础设施层，不依赖本 crate：本 crate 用 `TrackedPaths` 和忽略规则实现 `fs_watch` 定义的过滤与配置来源接口（已实现，`src/watch.rs`），`project`、`review` 把它传进去：

```rust
impl Worktree {
    /// 持有这个句柄。过滤器的文件列表就是 `tracked()` 返回的那个 Arc（index 没变时与 `project` 共用），
    /// 忽略规则在构造时一次读完，查询只在内存里匹配；配置清单从 gix 已加载配置的来源和 `include` 值得出。
    pub fn watch_source(&self) -> Arc<dyn lectern_fs_watch::WorktreeSource>;
}
impl From<WorktreeId> for lectern_fs_watch::WorktreeName { /* … */ }
impl From<lectern_fs_watch::WorktreeName> for WorktreeId { /* … */ }
```

- `filter`：未跟踪的 `.gitignore` 由「工作区比较」第 4 步的同一个遍历找出（经 `SafeRoot`，不进入被忽略的目录、子模块和嵌套仓库，受 `Limits::untracked` 的 20 000 个条目上限，[ADR 0028](../adr/0028.md)），遍历读到的每个 `.gitignore`（跟踪的和未跟踪的）都用上；遍历没有走到的、index 里的 `.gitignore` 和事件给出的 `gitignores` 之后再读，上级目录是子模块、嵌套仓库或被忽略的不读。工作区里只读普通文件；skip-worktree 条目在工作区里读不到时用 index 的 blob（与 git 相同）。`info/exclude`、`core.excludesFile`（没有设置时是 XDG 的默认位置）由 gix-ignore 读。`ignored` 从路径所在目录起逐级向上查 `.gitignore`（最深的有匹配的那个决定，同一文件里最后一条匹配的规则生效），都没有匹配时查 `info/exclude`，再查 `core.excludesFile`；`core.ignoreCase` 时大小写折叠（index 另存一份按折叠后排序的下标，每个条目 4 字节），`core.precomposeUnicode` 时先把查询的路径规范化为 NFC。
- `config_files`：仓库的 `config`、这个 worktree 的 `config.worktree`、`info/exclude`、`info/attributes`；gix 读系统和用户配置时（生产路径），系统、XDG、`~/.gitconfig` 三个位置和系统属性文件（`/etc/gitattributes`）本身；gix 配置里每一节的来源文件；每个 `include` / `includeIf` 节里的每个 `path`（按 gix 的规则展开 `~`、`%(prefix)`，相对路径相对所在文件的目录，条件和文件是否存在都不看）；`core.excludesFile`、`core.attributesFile` 的值，没有设置（或是不存在的 `:(optional)`）时的 XDG 默认位置。只给出绝对路径，排序去重。

### 规范内容的 OID

`canonical_oid` 供 `code_intel` 核对它自己读到的工作区字节是不是某个 OID 的那份内容（[code_intel 设计「对其他 crate 的要求」](code_intel.md#对其他-crate-的要求)）。转换与「工作区比较」第 3 步相同，只是单独公开：

- 转换：`working-tree-encoding`、`text` / `eol` / `crlf` 与 `core.autocrlf`、`core.eol` 的换行转换、`ident`，由 gix 的过滤管道完成，管道里没有任何过滤器驱动。`text=auto` 判断 index 里这个路径的 blob 是否已有 CR（git 的 `has_crlf_in_index`），index 里没有这个路径时按没有 CR 处理。转换失败（字节不符合声明的编码）时用原始字节，与工作区比较相同。
- 过滤器：属性 `filter=<名>` 指向配置了 `filter.<名>.clean` 或 `.process` 的驱动时返回 None，不执行任何程序。只有 `smudge` 的驱动和没有配置的驱动不影响 to-git 方向，与 git 相同。
- 属性：`info/attributes`、全局和系统属性文件按句柄的权限读取（与工作区比较相同）；`.gitattributes` 只取 `path` 所在的各级目录（根目录到 `path` 的父目录）里的那一个。每一级的取法与工作区比较取生效的 `.gitattributes` 相同（「与设计的差异」第 3 步）：index 里有这个 `.gitattributes`（stage 0 的普通文件）时，经 `SafeRoot` 读工作区里的那份，工作区里没有或不是普通文件时用 index 的 blob，skip-worktree 的条目直接用 index 的 blob；有未解决冲突的不用；index 里没有时，工作区里有普通文件就用它。属性栈只用 id-mapping 来源，不按路径读工作区（[ADR 0028](../adr/0028.md)）。
- 与工作区比较的差别：未跟踪的 `.gitattributes` 是否被忽略、是否在被忽略的目录里都会用上，与 git 的 `git add` 相同；工作区比较只用第 4 步遍历列出的那些（「与设计的差异」第 3 步的已知限制）。两者只在被忽略的 `.gitattributes` 上不同。
- 读取：`bytes` 由调用方给出，本函数不读 `path` 本身；读 index（与 `changes` 共用同一份按校验和缓存的解析结果）和各级 `.gitattributes`（每级最多一次 `SafeRoot::read`，上限 `Limits::default().read_bytes`）。不写 index 和对象库，不执行外部程序。
- 代价：每次调用打开一个 gix 句柄。新的 `Worktree` 句柄第一次调用时解析 index；同一个句柄在 index 不变时不再解析。调用方只在原始字节的 OID 与期望的不同时调用。

### 改动集

```rust
pub enum Base {
    /// 当前 HEAD 的树；unborn 分支为空树。审阅「agent 改了什么」的默认基准。
    Head,
    /// 某个提交的树，如会话开始时的 HEAD。
    Commit(Oid),
    /// `merge_base(HEAD, 分支)` 的树。Worktrees 检查器的「相对 main」用默认分支。
    MergeBase(RefName),
}

pub enum Refresh<'a> {
    /// 只重新比较这些路径（`fs_watch` 报告的路径，目录表示其下全部路径）。
    Paths(&'a [RepoPath]),
    /// HEAD、index、`.gitignore`、`.gitattributes` 或配置变了：重新比较全部，仍用 stat 跳过未改的文件。
    Full,
}

impl Worktree {
    /// 工作区相对 `base` 的改动集，含未跟踪文件。见「工作区比较」。
    pub fn changes(&self, base: Base, limits: &Limits, cancel: &AtomicBool) -> Result<Arc<ChangeSet>, GitError>;
    /// 返回新的改动集，没有变化的条目与 `prev` 共享。
    pub fn refresh(&self, prev: &Arc<ChangeSet>, what: Refresh<'_>, cancel: &AtomicBool)
        -> Result<Arc<ChangeSet>, GitError>;
}

impl Repo {
    /// 两棵树（或两个提交的树）之间的改动集：提交相对第一个父提交、分支相对 merge-base。
    pub fn diff_trees(&self, old: Oid, new: Oid, limits: &Limits, cancel: &AtomicBool)
        -> Result<Arc<ChangeSet>, GitError>;
}

impl ChangeSet {
    pub fn base_tree(&self) -> Oid;
    pub fn target(&self) -> &Target;               // Tree(Oid) | Worktree(WorktreeId)
    pub fn files(&self) -> &[Change];              // 按路径排序，含被截断的文件
    pub fn totals(&self) -> &Totals;               // 文件数、已知的增删行数、各类截断的数量
    /// 一个文件两侧的文本。基准一侧按 OID 读对象库；工作区一侧是比较时读到的那份内容，不重新读工作区。
    /// 新一侧是 `NotRetained` 时，`FileTexts` 的这一侧为 `NotRetained`，视图显示「内容过多，未保留」，
    /// 不按路径重读（重读的内容可能已经不是比较时的那份）；要看当前内容走完整文件阅读（⇧⌘O）。
    pub fn texts(&self, repo: &Repo, file: usize) -> Result<FileTexts, GitError>;
}
```

### 展示行

```rust
/// 统一视图与双栏视图共用的展示行。纯计算，调用方在后台任务里为可见的文件算好，交给视图。
pub fn rows(diff: &FileDiff, texts: &FileTexts, context: u32) -> Vec<Row>;

/// `*_byte`：所在行（`Folded` 为区间第一行）在这一侧规范内容里的字节偏移，见下文「字节偏移」。
/// 这一侧是 `SideText::Text` 或 `Absent`（偏移为 0）时为 Some，其余为 None。
pub enum Row {
    Context { old: u32, new: u32, old_byte: Option<u64>, new_byte: Option<u64> },
    /// `pair`：同一个 hunk 里与它配对的新行，双栏视图把两者放在同一行。
    Removed { old: u32, pair: Option<u32>, old_byte: Option<u64> },
    Added { new: u32, pair: Option<u32>, new_byte: Option<u64> },
    /// 折叠的未改动行；视图展开时用同样的行号区间取文本。
    Folded { old: Range<u32>, new: Range<u32>, old_byte: Option<u64>, new_byte: Option<u64> },
}

/// 一对配对行的词级差异：两侧改动部分的字节区间。任一行超过 1000 字节时返回 None，视图整行高亮。纯计算。
pub fn inline_changes(old: &[u8], new: &[u8]) -> Option<InlineChanges>;
```

行号是两侧完整文件里的行号（从 0 起）。高亮按 blob 的窗口计算再按行切片（[ADR 0034](../adr/0034.md)：大文件不做整文件解析，窗口覆盖展示的 hunk 和上下文），缓存键是 blob OID、语言和 grammar 与查询的版本（[pulls.review 调研](../research/pulls-review.md#对-lectern-的建议)、[#42](https://github.com/SoloJiang/lectern-app/issues/42)），由做高亮的 crate 实现。

字节偏移（ADR 0034）：`Hunk` 有两侧第一行的字节偏移（`old_byte`、`new_byte: u64`；区间为空时是插入点之后那一行的偏移，在文件末尾时等于内容的长度）。展示行的 `Context`、`Removed`、`Added` 带所在行的字节偏移，`Folded` 带区间第一行的偏移。行 diff 时两侧文本都在内存里，偏移在切行时顺带得到：hunk 在两侧都按顺序排列，每侧的行只向前走一遍。展示行的偏移取自 `Text` 的行首表（`Text::line_start`）。hunk 缓存的值每个 hunk 多两个 `u64`，缓存格式版本（`git.diff` 命名空间的版本）从 2 改为 3，旧版本的条目不会被当作新格式读出。视图从 hunk 打开完整文件、高亮按窗口定位时用偏移，不从文件开头扫描换行。

偏移的口径是规范内容，即这一侧 blob 的字节，也就是 `ChangeSet::texts` 返回的 `Text::bytes()`，与行号的口径相同。工作区一侧是经过「工作区比较」第 3 步内置转换（`working-tree-encoding`、换行、`ident`）的内容，所以 `core.autocrlf = true` 时 CRLF 的工作区文件按 LF 计偏移，UTF-16 的文件按转换后的 UTF-8 计。理由：hunk 以两侧 blob OID 为键缓存（架构规则 5），同一个 OID 在换行、编码配置不同的 worktree 里对应不同的原始字节，只有规范内容由 OID 唯一确定；`texts` 和展示行的文本也是规范内容。使用方在 `texts` 的文本或按 OID 取出的内容（`Repo::blob`）里用偏移；直接读工作区原始字节时，只有原始字节的 OID 等于这一侧的 OID（转换没有改变字节）才按偏移定位，否则按行号和行检查点定位。

其余情形：CRLF 行的 `\r` 计入这一行，最后一行没有 `\n` 也算一行；二进制、`TooLarge`、`Truncated`、`Listed` 没有 hunk，也就没有偏移；重命名的旧一侧按来源路径的 blob 计。词级差异的区间相对所在行，加上展示行的偏移就是在文本里的位置。一侧的 `SideText` 不是 `Text` 也不是 `Absent` 时，这一侧的展示行偏移为 None（见「与设计的差异」）。

### Analyze 快照

Analyze 快照的输入（[ADR 0019](../adr/0019.md)）：基准树条目、叠加的工作区改动、截断文件改动前的版本、审阅对象的指纹，随 [#66](https://github.com/SoloJiang/lectern-app/issues/66) 设计，不在本文。那份设计要满足 ADR 0019 的约束：快照只含属于审阅对象的改动；省略的内容、使用了过滤器的文件、子模块、超过上限的内容都让结果标为「部分内容未提供」；模型读到的内容、显示的 patch 和指纹来自同一份状态。它建立在本文的 `ChangeSet`、`Content`（含 `Unavailable::Filtered { raw }`、`NotRetained`）、`texts` 和 `Repo::blob` 之上。

### 错误

```rust
pub enum GitError {
    /// reftable 引用存储、未知的 `extensions.*`、`core.repositoryformatversion` 大于 1。
    Unsupported(UnsupportedRepo),
    NoMergeBase,
    ObjectMissing(Oid),
    TooLarge { size: u64 },
    TooManyEntries,
    Corrupt(String),
    Io(io::Error),
    Cancelled,
}
```

单个文件读不到不让整个改动集失败，标在这个文件的 `Content::Unavailable` 上。

### 第 1 步补全的类型

上文的签名用到、但没有写出定义的类型，第 1 步按下面实现（原因见「与设计的差异」）：

```rust
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UnsupportedRepo {
    Reftable,                                        // extensions.refStorage = reftable
    FormatVersion(i64),                              // core.repositoryformatversion > 1
    /// git 不会忽略、本 crate 不认识的扩展，或认识的扩展取了读不了的值（如 objectFormat = sha512）。名字小写。
    Extension { name: Arc<str>, value: Option<Arc<str>> },
}

pub struct EntryLimits { pub entries: usize }       // 结果的条目总数（index 自己的条目 + sparse 展开的条目），默认 1 000 000

pub enum WorktreeFile {
    File { bytes: Vec<u8> },
    Symlink { target: Vec<u8> },                     // 不跟随
    TooLarge { size: u64 },
    NotFound,                                        // 不存在，或中间一级不是目录
    Unavailable(Unavailable),                        // UnsupportedType（含目录）、PathThroughSymlink、Unreadable
}

impl TrackedPaths {
    pub fn is_empty(&self) -> bool;
    /// 不满足 `RepoPath` 不变量的 index 路径：不在 `iter` 里，也不被打开。
    pub fn invalid_paths(&self) -> &[Arc<[u8]>];
}

impl Worktree {
    pub fn git_dir(&self) -> &Path;                  // `WorktreeInfo::git_dir`
}

impl HashKind {
    pub fn byte_len(self) -> usize;                  // 20 或 32
    pub fn hex_len(self) -> usize;                   // 40 或 64
}

pub struct GitTime { pub seconds: i64, pub offset: i32 }          // 时区偏移以秒计，东正
pub struct Signature { pub name: Arc<[u8]>, pub email: Arc<[u8]>, pub time: GitTime }

pub struct CommitInfo {
    pub id: Oid,
    pub tree: Oid,
    pub parents: Arc<[Oid]>,
    pub summary: Arc<[u8]>,                          // `git log --format=%s`：消息第一段合成一行，原始字节
    pub author: Signature,
    pub committed: GitTime,                          // 提交者时间，`log` 按它排序
}

pub struct BranchInfo {
    pub name: RefName,                               // refs/heads/…
    pub commit: Oid,
    pub upstream: Option<RemoteBranch>,              // branch.<名>.remote + merge
    /// 不带参数的 `git push` 推到哪里；`git push` 会拒绝时（nothing，或 simple / upstream 没有匹配的上游）为 None。
    pub push: Option<RemoteBranch>,
}

pub struct RemoteBranch {
    pub remote: Arc<str>,                            // 远端名；本地仓库为 `.`；配置直接写 URL 时为 URL 本身
    pub url: Option<Arc<str>>,                       // 上游取 fetch URL，推送目标取 push URL（已应用 insteadOf / pushInsteadOf）；`.` 为 None
    pub branch: RefName,                             // 远端上的分支名，如 refs/heads/main
}

pub struct RemoteInfo { pub name: Arc<str>, pub fetch_url: Option<Arc<str>>, pub push_url: Option<Arc<str>> }

pub struct StashInfo {
    pub worktree: Oid,                               // stash 提交本身
    pub base: Oid,                                   // 第一个父提交：stash 时的 HEAD
    pub index: Oid,                                  // 第二个父提交
    pub untracked: Option<Oid>,                      // 第三个父提交，`-u` / `-a` 时才有
}

pub enum ReflogParseError { Oid, Identity, Time }

/// `test-support`：只读仓库自己的配置（不读系统、XDG、`~/.gitconfig`）的 `Repo::discover`，测试结果不受机器配置影响。
pub mod testing { pub fn discover(path: &Path, ceilings: &[PathBuf]) -> Result<Option<Discovered>, GitError>; pub struct TestRepo; /* … */ }

/// `test-support`：bench 的 `git/open_handle`（[bench 设计「迁移」](bench.md#迁移)）。在公共目录上按只读仓库自身配置打开一个 gix 句柄，随即 drop。
pub mod testing { pub fn open_handle(common_dir: &Path) -> Result<(), GitError>; }
```

`Unavailable` 与 `RawIdentity` 按「数据结构」一节的定义在第 1 步加入（`WorktreeFile` 用到前者）。

### 第 2 步补全的类型

改动集和展示行的签名用到、但没有写出定义的类型，第 2 步按下面实现（原因见「与设计的差异」）：

```rust
/// 「上限与截断」表里的上限，默认值与表相同。树之间的比较用前五项和 `entries`，其余由工作区比较使用。
pub struct Limits {
    pub diff_bytes: u64,          // 行 diff：任一侧内容，16 MiB → TooLarge
    pub patch_bytes: u64,         // 单文件 patch 的字节数（统一格式、3 行上下文），1 MiB → Truncated
    pub patch_lines: u32,         // 单文件改动行数（增 + 删），20 000 → Truncated
    pub total_files: usize,       // 整体文件数，5000 → 之后的文件 Listed
    pub total_patch_bytes: u64,   // 整体 patch 字节数，50 MB → 之后的文件 Listed
    pub read_bytes: u64,          // 工作区单文件读取，64 MiB → Unhashed
    pub untracked: usize,         // 未跟踪文件枚举，20 000
    pub retained_bytes: u64,      // 改动集保留的工作区内容，32 MiB → NotRetained
    pub entries: usize,           // 改动路径数（及 index 条目数），1 000 000 → GitError::TooManyEntries
}

pub enum Target { Tree(Oid), Worktree(WorktreeId) }

pub struct Totals {
    pub files: usize,
    pub added: u64, pub removed: u64,          // Text 与 Truncated 的增删行数之和
    pub truncated: usize, pub too_large: usize, pub listed: usize,   // 各种 Detail 的数量
    pub not_retained: usize,                   // 新一侧为 NotRetained 的文件数
    pub untracked_capped: bool,
    pub invalid_paths: Arc<[Arc<[u8]>]>,
}

pub struct FileTexts { pub old: SideText, pub new: SideText }
pub enum SideText {
    Absent,                        // 这一侧不存在（新增的旧一侧、删除的新一侧）
    Text(Text),
    TooLarge { size: u64 },        // 超过 `Limits::diff_bytes`，没有读
    NotRetained,                   // 「内容过多，未保留」
    Unavailable,                   // 子模块、`Unavailable`、`Unhashed`
}
/// 一侧的字节和行边界；行以 `\n` 结束，最后一行没有 `\n` 也算一行，与 hunk 的行号一致。
pub struct Text { /* Arc<[u8]> + 行首偏移 */ }
impl Text {
    pub fn new(bytes: Arc<[u8]>) -> Text;
    pub fn bytes(&self) -> &[u8];
    pub fn line_count(&self) -> u32;
    pub fn line(&self, n: u32) -> &[u8];       // 不含 `\n`
    pub fn ends_with_newline(&self) -> bool;
    pub fn line_start(&self, n: u32) -> u64;   // 第 n 行在 `bytes()` 里的偏移；从 `line_count()` 起为长度
}

/// 两侧改动部分在各自行内的字节区间。
pub struct InlineChanges { pub old: Vec<Range<u32>>, pub new: Vec<Range<u32>> }
pub const INLINE_MAX_LINE_BYTES: usize = 1000;

impl Oid { pub fn empty_tree(kind: HashKind) -> Oid; }   // 根提交和 unborn 分支的基准，不需要对象
impl Content { pub fn oid(&self) -> Option<Oid>; }        // `Blob` 和 `NotRetained` 的 OID

/// `test-support`：导入 `tests/fixtures/` 里的目录夹具，返回旧树和新树。
impl TestRepo { pub fn import_fixture(&self, dir: &Path) -> (Oid, Oid); }
```

### 第 3 步补全的类型

工作区比较按「改动集」一节的 `Base`、`Refresh`、`changes`、`refresh` 实现，另外加入下面这些（原因见「与设计的差异」）：

```rust
/// crates/git/AGENTS.md「边界」的工作区指纹：基准树 OID，每个改动文件的路径、状态（含重命名来源）、
/// `untracked`、两侧模式和内容身份（`Blob` 与 `NotRetained` 按 OID，`Unhashed` 和 `Filtered` 的 `Stat`
/// 按大小、mtime、inode，`Filtered` 的 `Sha256` 按原始字节的摘要），加上 `untracked_capped` 和无效路径。
/// 只依赖完整 OID，不受截断影响。SHA-256。
pub struct Fingerprint([u8; 32]);
impl Fingerprint { pub fn as_bytes(&self) -> &[u8; 32]; pub fn to_hex(&self) -> String; }
impl ChangeSet { pub fn fingerprint(&self) -> Fingerprint; }

/// `test-support`
pub struct IndexStat(/* gix_index::entry::Stat */);
impl IndexStat { pub fn of(path: &Path) -> IndexStat; }               // lstat，与 `git add` 记录的相同
pub struct IndexEntry { /* … */ pub stat: Option<IndexStat>, pub intent_to_add: bool }
impl TestRepo {
    pub fn add(&self, rel: &str) -> IndexEntry;                       // blob、模式和 stat，不做转换
    pub fn set_mtime(&self, rel: &str, time: SystemTime);
    pub fn backdate(&self, rel: &str);                                // mtime 提前一小时，条目不是 racy
    pub fn remove(&self, rel: &str);
    pub fn import_worktree_fixture(&self, dir: &Path) -> Vec<String>; // 工作区夹具，见夹具说明
}
```

## 数据结构

```rust
pub struct Change {
    pub status: Status,
    pub path: RepoPath,              // 新路径；删除时为旧路径
    pub old: Option<Side>,           // 基准一侧，新增时为 None
    pub new: Option<Side>,           // 目标一侧，删除时为 None
    pub detail: Detail,
    pub generated: bool,             // `.gitattributes` 的 `linguist-generated`
    /// 新一侧是 index 里没有的工作区文件。只出现在 `Added` 上。
    pub untracked: bool,
    /// index 里这个路径的条目，只在它的 OID 或模式与工作区一侧不同时为 Some（工作区一侧是 `Blob` 或已不存在时才比较）：
    /// 提交会记录暂存的版本，与磁盘上的不一样（包括只暂存了可执行位的变化）。视图据此提示，P1 不单独显示暂存区的 diff。
    pub staged: Option<Staged>,
}

pub struct Staged { pub oid: Oid, pub mode: Mode }

pub enum Status {
    Added, Deleted, Modified, TypeChanged,
    Renamed { from: RepoPath, similarity: u8 },   // 百分比，精确重命名为 100
    Conflicted,                                    // index 里有未解决的冲突条目
}

pub struct Side { pub mode: Mode, pub content: Content }

pub enum Content {
    /// 规范内容的 blob OID；符号链接是目标路径作为 blob 的 OID，子模块是提交 OID。
    Blob(Oid),
    /// 超过工作区单文件读取上限（默认 64 MB）：没有读内容。
    Unhashed { size: u64, mtime_ns: i128, inode: u64 },
    Unavailable(Unavailable),
}

pub enum Unavailable {
    /// 设置了 clean / process 过滤器（如 Git LFS）：「使用了过滤器，暂不支持比较」。
    /// `raw` 是工作区原始字节的身份，用来判断内容是否变了（基准一侧来自对象库，照常是 `Blob`）。
    Filtered { raw: RawIdentity },
    UnsupportedType,     // FIFO、socket、设备文件；模式为 `Mode::Special`
    PathThroughSymlink,  // 路径中间一级是符号链接
    NestedRepository,    // 未跟踪的目录里有自己的 `.git`；模式为 `Mode::Special`
    Missing,             // 对象库里没有（如部分克隆）
    Unreadable,          // 读取失败
    /// 超过改动集保留工作区内容的上限（32 MiB）：比较时读过、算过 OID，但字节没有保留。
    /// `oid` 是当时规范内容的 blob OID，已看状态等按 OID 的键照常可用；`Detail` 保留超过上限之前已算出的那一种（`Binary`、`TooLarge`，或有增删行数时的 `Truncated`）。
    NotRetained { oid: Oid },
}

pub enum RawIdentity {
    Sha256([u8; 32]),                                    // 不超过 64 MB 时，原始字节的 SHA-256
    Stat { size: u64, mtime_ns: i128, inode: u64 },      // 超过 64 MB 时不读内容
}

pub enum Detail {
    Text(FileDiff),
    /// 超过单文件 patch 上限：只有文件头和增删行数。
    Truncated { added: u32, removed: u32 },
    Binary { old_size: Option<u64>, new_size: Option<u64> },
    /// 某一侧超过行 diff 上限，没有比较，增删行数未知。
    TooLarge,
    /// 超过整体上限：只有路径和改动类型。
    Listed,
    /// 内容相同，只改了模式。
    ModeOnly,
    /// 工作区与基准相同，只有 index 不同（`staged` 为 Some）。
    StagedOnly,
    /// 至少一侧是 `Unavailable` 或子模块；`NotRetained` 除外（见不变量）。
    NoDiff,
}

pub struct FileDiff {
    pub hunks: Arc<[Hunk]>,          // 不含上下文；行号区间覆盖两侧完整文件
    pub added: u32,
    pub removed: u32,
}

pub struct Hunk {
    pub old: Range<u32>,
    pub new: Range<u32>,
    pub old_byte: u64,               // `old.start` 在旧一侧规范内容里的字节偏移（「展示行」的「字节偏移」）
    pub new_byte: u64,
}
```

不变量：

- `files` 按（`path` 的字节序，`untracked`）排序。同一路径最多一项，唯一的例外是「工作区比较」表里 B = A、I 没有、W 有并且路径没有被忽略的那一行：同一路径上一项是 `Deleted`，另一项是 `untracked` 的 `Added`。重命名只出现在新路径上，所以重命名的旧路径上重新出现的文件只有一项未跟踪的 `Added`，`Renamed` 本身不受影响。
- `Status` 与两侧一致：`Added` 只有 `new`，`Deleted` 只有 `old`，`Modified`、`TypeChanged`、`Renamed` 两侧都有。`Conflicted` 的两侧按实际状态：基准里没有这个路径时没有 `old`（如 HEAD 删除、对方修改的冲突），工作区里没有文件时没有 `new`，基准和工作区都没有这个路径时（如 add/add 冲突后删掉了工作区文件）仍然报告为 `Conflicted`，两侧都为 None，与 git 的 `AA` 一致。`TypeChanged` 是普通文件、符号链接、子模块、`Mode::Special` 之间的变化（跟踪的文件被换成 FIFO、socket、设备文件时新一侧是 `Mode::Special`）。
- 截断只影响 `Detail`，不影响改动的判定、`Status` 和两侧的 `Content`：被截断、只列出的文件也有完整的 OID。
- 有一侧是 `Unavailable` 的改动，`Detail` 为 `NoDiff`（被整体上限截到时为 `Listed`），唯一的例外是 `NotRetained`：它保留超过保留上限之前算出的 `Detail`（`Binary`、`TooLarge`，或带增删行数的 `Truncated`）。
- `Content::Blob` 的 OID 与 `git add` 之后对象库里会有的 OID 相同，所以只是被 touch 过、内容未变的文件不算改动，内容相同的文件在不同 worktree 之间命中同一份缓存。
- `ChangeSet` 创建后不再修改。`refresh` 生成新的 `ChangeSet`，没变的 `Change` 与旧的共享。

### 工作区比较

目标是工作区时，`changes` 对「基准树的路径 ∪ index 的路径」逐个判定，再加上未跟踪文件。每个路径有三个状态：

- 基准 B：没有，或 A。
- index I：没有、等于基准、或与基准不同（记为 S；基准没有时 index 里的任何条目都算 S）。
- 工作区 W：没有（含路径上是目录、而这个条目不是子模块：`p` 被换成了目录 `p/`）、等于 index、等于基准、其他内容（记为 C）、或不是文件（FIFO、socket、设备文件）。

「等于」指 OID 和模式都相同。W 由下面的第 1–3 步确定。判定规则：基准与 index 不同、或基准与工作区不同时报告；报告的改动在 index 的 OID 或模式与工作区一侧不同（工作区没有也算不同）时带 `staged = Some(index 的 OID 和模式)`。基准与工作区相同、只有 index 不同时，报告的是基准 → index 的改动，`Detail::StagedOnly`，新一侧取 index 的条目。完整的表：

| B | I | W | 结果 | `staged` |
|---|---|---|---|---|
| 没有 | 没有 | 有 | 不是跟踪的路径，由第 4 步作为未跟踪的 `Added` 列出 | — |
| 没有 | S | 没有 | `Added`，新一侧取 index，`StagedOnly`（`git add p; rm p`） | S |
| 没有 | S | = I | `Added` | None |
| 没有 | S | C | `Added`，新一侧为 C | S |
| 没有 | S | 不是文件 | `Added`，新一侧为 `Mode::Special` + `UnsupportedType` | S |
| A | 没有 | 没有 | `Deleted` | None |
| A | 没有 | 有 | `Deleted`；路径没有被忽略时，另由第 4 步列出一项未跟踪的 `Added`（`git rm --cached p` 后文件还在），两项独立；被忽略时只有 `Deleted`，与 git 相同 | None |
| A | = A | 没有 | `Deleted`（未暂存的 `rm`） | A |
| A | = A | = A | 不报告 | — |
| A | = A | C | `Modified` | A |
| A | = A | 不是文件 | `TypeChanged`，新一侧为 `Mode::Special` + `UnsupportedType` | A |
| A | S | 没有 | `Deleted` | S |
| A | S | = I | `Modified`，新一侧为 S | None |
| A | S | = A | `Modified`，新一侧取 index（S），`StagedOnly` | S |
| A | S | C | `Modified`，新一侧为 C | S |
| A | S | 不是文件 | `TypeChanged`，新一侧为 `Mode::Special` + `UnsupportedType` | S |

`Modified` 与 `TypeChanged` 按两侧的模式区分（普通文件、符号链接、子模块、`Mode::Special` 之间的变化是 `TypeChanged`）。W 是 `Filtered` 或 `Unhashed`、没有 OID 时，按 C 处理，`staged` 为 None（无法比较）。index 里有未解决的冲突条目的路径不查这张表，报告为 `Conflicted`（见「数据结构」的不变量）。路径被换成目录时，这个条目不设 `UPTODATE`，目录下的文件由第 4 步作为未跟踪的 `Added` 列出。

确定 W 的步骤与其余处理：

1. **过滤器先判定**：先用 `SafeRoot::stat` 判断路径是否存在和类型：不存在时 W 为没有；符号链接按下文「模式」读目标；目录按 index 条目区分（gitlink 按下文子模块的规则，其余是路径被换成目录）；FIFO、socket、设备文件为 `Mode::Special` + `UnsupportedType`。这些都不进入过滤器判定。普通文件才按属性判断这个路径是否设置了 clean / process 过滤器（属性 `filter=<名>`，配置里有 `filter.<名>.clean` 或 `.process`）。设置了的：stat 与 index 一致、不是 racy-clean、index 的 OID 与基准相同，并且这个路径的转换规则在这一次比较里没有变化（见下文 stat 缓存）时 W = I（未改动）；其余情况（包括刚暂存了更新、index 与基准不同）W 是 `Unavailable::Filtered`，不把 index 的 blob 当作工作区的完整内容；`raw` 按第 3 步的读取上限计算。
2. **stat 跳过**：`SafeRoot::stat` 报告路径不存在时 W 为没有。index 条目的 stat 与 `SafeRoot::stat` 的结果按 gix-index 的规则比较（`core.trustctime`、`core.checkStat`），一致且不是 racy-clean 时 W = I，不读文件。racy-clean 与 git 的规则相同：条目记录的 mtime 不早于 index 文件自己的 mtime（等于或晚于）；记录的 mtime 早于 index 文件的绝大多数条目只按 stat 判断，不读内容。
3. **比较内容**：stat 不一致或 racy-clean 时，用 `SafeRoot::read` 读取（上限 64 MB），按属性应用内置转换得到规范内容，计算 blob OID。`Unavailable::Missing` 只用于对象库里缺对象，不用于工作区里没有文件。
4. **未跟踪文件**：从工作区根出发，逐个目录用 `SafeRoot::list` 列出条目（不跟随符号链接，名字总在工作区之下，[ADR 0028](../adr/0028.md)），按 `.gitignore`、`info/exclude`、`core.excludesFile` 判断忽略，不进入被忽略的目录，逐个列出 index 里没有的文件（相当于 `-uall`）。每个目录里的 `.gitignore` 只在它是普通文件时经 `SafeRoot::read` 读出，用 gix-ignore 在内存里解析和匹配（更深的目录优先，同一文件里后面的规则优先）；没有匹配时再查 gix 的排除栈里的 `info/exclude` 和 `core.excludesFile`，这个栈只用 id-mapping 来源，不读工作区。子模块（index 里的 gitlink）的目录不进入；跟踪的文件被换成目录时进入，其中的文件都是未跟踪的。是否跟踪只按 index 判断。列出的每一项都是 `Added` 且 `untracked = true`，内容按第 1、3 步读取；FIFO、socket、设备文件的新一侧是 `Mode::Special` + `UnsupportedType`，未跟踪的目录里有 `.git` 时只列出这个目录，新一侧是 `Mode::Special` + `NestedRepository`。
5. **重命名**：与 git 的 `diffcore-rename` 相同（树之间的比较也用这一步）。删除与新增之间先按 OID 找精确重命名，精确重命名不受任何上限限制，总是检测。剩下的删除（来源）与新增（目标）里，文件名（最后一级）在两边都唯一的一对先按相似度不低于 75% 配对；再按相似度（不低于 50%）找重命名，上限规则与 git 的 `too_many_rename_candidates` 相同：剩下的来源数 × 目标数超过 `diff.renameLimit` 的平方时跳过这一轮（默认 1000，即 100 万对；40 × 40 的重构照常检测；0 或负数不限）。相似度是 git 的估算（按换行或 64 字节分块、哈希、计数），分数与 git 相同。同一路径上的删除和未跟踪新增不互相配对。`staged` 为 Some 的删除（index 里还留着来源路径，如未暂存的 `mv a b`）不作为来源，否则配对后 `staged` 只能描述新路径、丢掉来源路径在 index 里的状态；这时与 `git status` 一样，报告为 `Deleted` 和未跟踪的 `Added`。工作区一侧的内容只放进 gix 的内存对象存储参与计算（ADR 0024）。

其余规则：

- 模式：`core.fileMode=false` 时可执行位取 index 里的值；符号链接的内容是 `readlinkat` 读出的目标。
- 子模块：工作区一侧的提交是子模块自己的 HEAD（只读地读它的 git 目录：工作区里的 `.git` 目录经 `SafeRoot` 读，`gitdir:` 只接受本仓库 `modules/` 之下的目录，也经以公共目录为根的 `SafeRoot` 读，ADR 0028）；子模块内部的未提交改动不计算。子模块没有初始化（克隆时没带 `--recurse-submodules`，或 `deinit` 之后：路径是空目录，没有仓库和 HEAD）时，W = I（index 里的 gitlink OID），不标为 `Unavailable`。
- `skip_worktree` 的条目（含从 sparse 目录展开的条目）不 stat、不读工作区，W = I（按上面的表：index 与基准相同时不报告，不同时是 `Modified`、`staged` 为 None）。这与 `git status` 相同：git 不看 skip-worktree 条目的工作区文件，sparse 范围之外被重新写出、修改的文件在 git 下一次更新 index（如 `git sparse-checkout reapply`）清掉这个标志之前也不报告。这是已知限制，本 crate 不写 index，也就不替 git 清标志。`git add -N` 的条目按新增处理。
- 路径的大小写与 Unicode 规范化按仓库的 `core.ignoreCase`、`core.precomposeUnicode`（gix 的文件系统能力）。
- 不写 index（不更新 stat、不刷新 racy 条目），不写对象库、引用和配置；不执行任何外部程序（ADR 0024）。
- 一次比较读到的工作区内容（新一侧的文本）保存在 `ChangeSet` 里，`texts` 用它，不再按路径重读，所以展示的文本和 OID 一致。超过保留上限的文件没有字节可用，明确标为 `NotRetained`，不落盘另存。

`Worktree` 内部为未跟踪文件和已改动文件保留一份 stat 缓存（路径 → stat、OID、计算时刻），stat 没变时不重新读取和计算；文件的 mtime 不早于缓存项的计算时刻时（与 racy-clean 同理，同一时刻内的两次同长度写入 stat 相同）不命中缓存，照常读取内容；只在内存里，`Worktree` 句柄释放时丢弃。规范内容取决于转换规则，所以缓存属于一份「转换状态」：生效的 `.gitattributes`、`info/attributes`、`core.attributesFile` 和影响转换的配置（`core.autocrlf`、`core.eol`、`filter.*`）。`refresh(Full)` 时重新计算这份状态的指纹，与缓存记录的不同就清空整个缓存；并且这一次比较里，转换规则（按新旧两份属性和配置分别求出的 `eol`、`text`、`ident`、`working-tree-encoding`、`filter`）有变化的跟踪路径跳过第 1 步的过滤器快路径和第 2 步的 stat 跳过：新规则有过滤器的按第 1 步是 `Filtered`，其余按第 3 步读取并重新计算 OID。规则没变的路径照常走 stat 跳过。

### 上限与截断

| 上限 | 默认 | 超过时 |
|---|---|---|
| 行 diff：任一侧内容 | 16 MiB | `TooLarge` |
| 单文件 patch：统一格式、3 行上下文的字节数，或改动行数 | 1 MiB 或 20 000 行 | `Truncated { added, removed }` |
| 整体：按路径顺序，全部改动文件（含二进制、只改模式、`TooLarge`、`NoDiff`）计入文件数，生成的 patch 计入字节数 | 5000 个文件、50 MB | 达到任一上限之后的文件为 `Listed`，`Totals` 记数量 |
| 工作区单文件读取 | 64 MiB | `Content::Unhashed` |
| 未跟踪文件枚举 | 20 000 个 | `Totals::untracked_capped = true` |
| 改动集保留的工作区内容 | 32 MiB | 之后的文件不保留内容，新一侧为 `Unavailable::NotRetained`（见下） |
| 词级 diff 的单行长度 | 1000 字节 | 整行高亮 |

二进制判断与 git 相同：属性 `binary` 或 `-diff`，或任一侧前 8000 字节里有 NUL。属性先判断，不读内容；没有这两个属性而某一侧超过行 diff 上限时是 `TooLarge`，不为找 NUL 读入。树之间的比较按新一侧那棵树里的 `.gitattributes`（加上 `info/attributes` 和全局、系统的属性文件）判断属性和 `linguist-generated`。上限都在 `Limits` 里，调用方可以调整；默认值写进 `bench` 的预算表。

### 缓存

| 数据 | 键 | 存放 |
|---|---|---|
| hunk（`FileDiff`） | 格式版本、`HashKind`、旧 OID、新 OID、算法（Histogram）、空白处理（P1 只有「不忽略」） | 进程内字节 LRU 16 MiB；`cache` 实现后写入它的 SQLite（命名空间 `git.diff`），首帧之后才打开 |
| 工作区文件的 OID | 路径 + stat | `Worktree` 内的内存缓存，不持久化 |
| 工作区比较自己解析的 index（gix 的 `State`、逐条的 stat 与标志、`core.ignoreCase` 的折叠副本） | index 校验和 | 只在一次 `changes` / `refresh` 调用内持有，调用返回时释放，下一次比较重新解析（[ADR 0035](../adr/0035.md)）。现在的实现仍随 `Worktree` 保留，10 万个跟踪文件时调用返回后 `Worktree` 还持有 39 MB（`git/compare_100k/retained_bytes`，几乎都是它），第 4 步改正 |
| 改动集、展示行、词级差异 | 不缓存 | 由缓存的 hunk 重算，耗时与可见行数成正比 |

hunk 只存行号区间和两侧第一行的字节偏移，不存文本：文本随时可以按 OID 从对象库取出。条目里另存 patch 正文的字节数和两侧内容的字节数（都只由两个 OID 决定），命中时按这次的上限判断 `TooLarge` 和 `Truncated`，只检查两侧对象是否存在（缺失时仍是 `Unavailable::Missing`），不读也不解压对象头。两侧都是 `Blob` 且是文本时才进缓存；有 `binary` / `-diff` 属性的文件不查缓存。上下文行数不在键里，因为展示行在 hunk 之上计算。

## 数据流与线程

1. 首帧之后，`project` 在后台任务里调用 `Repo::discover`、`worktrees`、`tracked`、`read_file`，建立仓库 → worktree → 子项目的模型，派生数据以 `TrackedPaths::checksum` 和 `Oid::for_blob` 算出的 OID 为键；检查已有缓存是否过期时调 `Worktree::index_checksum`，不重建文件列表。
2. `review` 在后台任务里调用 `changes(Base::Head)` 或 `changes(Base::Commit(会话开始时的 HEAD))`，得到 `Arc<ChangeSet>`，加上归因后作为不可变快照交给 `review_view`（架构规则 4）。
3. `fs_watch` 报告路径变化时，`review` 调用 `refresh(Paths)`；HEAD、index、忽略规则、属性或配置变化时调用 `refresh(Full)`。
4. 视图滚动到某个文件时发出命令，后台任务调用 `texts` 和 `rows`，把展示行交给视图；视图不调用本 crate 的任何函数。
5. Analyze 快照的数据流随 [#66](https://github.com/SoloJiang/lectern-app/issues/66) 设计。

线程：本 crate 的函数在调用方的后台任务线程上运行，不建线程，不持有跨调用的锁。gix 的 `parallel` 关闭（ADR 0024），它的仓库句柄不是 `Send`，所以 `Repo`、`Worktree` 只保存位置、打开参数和本 crate 自己的数据（`tracked` 的 index 解析结果、stat 缓存；工作区比较自己解析的 index 只在一次调用内持有，见「缓存」），不持有 gix 句柄。gix 句柄不在调用之间保留：每次顶层调用（`changes`、`refresh`、`texts`、`diff_trees`、引用与历史的查询等）在当前线程上打开一个，调用内部复用，返回时释放。worktree 级的调用按这个 worktree 的私有 git 目录（`WorktreeInfo::git_dir`）打开，仓库级的调用按公共目录打开，打开参数（权限、对象格式）固定在 `Repo` 里。所以空闲时内存里没有 gix 句柄和它的 pack 缓存。非当前 worktree 只保留 `WorktreeInfo`，`Worktree` 句柄（index、stat 缓存）由调用方在离开后释放（[技术调研「性能要点」](tech-research.md#性能要点)）。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)、[ADR 0017](../adr/0017.md)）：

| 预算 | 本 crate 的份额（M 系列 Mac，`cloudprint-miniapps` 量级：728 个跟踪文件、11 个 worktree） |
|---|---|
| 每次顶层调用 | 打开 gix 句柄 < 1 ms（读配置和引用，不读 pack），计入下面各项 |
| 打开项目到项目主页可用 < 100 ms | `discover` + `worktrees` + `tracked` < 15 ms；10 万个跟踪文件时 < 40 ms，计入 `project/open_100k/files`（< 80 ms） |
| 打开一个会话的 Diff < 50 ms | 50 个改动文件的 `changes`（stat 跳过其余文件）< 15 ms；hunk 未命中时计算 < 20 ms，命中时 < 2 ms |
| 切换 worktree / 分支 < 100 ms | 内容相同的文件命中同一份 hunk 缓存；`refresh(Full)` < 15 ms |
| 空闲内存：打开 monorepo < 100 MB | 空闲时不持有 gix 句柄；调用期间每个句柄的 pack 与对象缓存 ≤ 4 MiB；hunk LRU 16 MiB；每个 `ChangeSet` 保留的工作区内容 ≤ 32 MiB（通常只有当前审阅的一个）；fs_watch 的过滤器（解析后的 index、`core.ignoreCase` 的折叠顺序、忽略规则）是本 crate 的份额，index 与 `project` 保留的句柄共用一份（[ADR 0035](../adr/0035.md)），连同句柄 < 1 MB |
| 空闲内存：10 万文件的仓库 < 120 MB（ADR 0035） | 过滤器连同它的 `Worktree` 句柄 ≤ 16 MB；工作区比较解析的 index 不在空闲时保留（「缓存」），调用返回后 `Worktree` 为比较留下的 < 1 MB（`git/compare_100k/retained_bytes`，第 4 步生效）。采样状态里唯一保留的改动集是项目主页的计数用改动集（`retained_bytes = 0`、`total_patch_bytes = 0`，没有工作区内容和 hunk）：它连同 stat 缓存随改动路径数增长、不随跟踪文件数增长，每个改动路径约 0.95 KB（`git/changes_count_bytes`：1 万个 9.32 MB）；主页只在 4000 个以内保留它（≤ 4 MB），在 ADR 0035 的 12 MB 余量里。上一行 cloudprint 量级的 32 MiB 保留内容是打开 Diff 后的改动集 |
| 工作区比较期间的临时内存（10 万个跟踪文件，不设顶层预算，同 ADR 0029 的临时内存） | 一次 `changes` 的存活字节峰值 ≤ 72 MB（`git/compare_100k/peak_bytes`）：gix 的 `State`、逐条的 stat 与标志、折叠副本、未跟踪遍历、改动集；比较与 `tracked` 各解析一次 index，合成一次解析是降低它的办法（ADR 0035） |
| index 变化后的刷新（10 万个跟踪文件，后台） | 重建 fs_watch 过滤器（解析 index、排序折叠顺序、未跟踪遍历、读 `.gitignore`）< 400 ms，fs_watch 等它完成后才交付这一批；`changes` 的全量比较（首次比较和每次 `refresh(Full)`，每次都重新解析 index）< 2 s，项目主页先显示文件列表，改动数随后填入 |
| 冷启动 20 ms | 首帧之前不调用本 crate 的任何函数；10 万文件仓库的读取、过滤器和全量比较都在打开项目之后的后台任务里 |
| 安装包 ≤ 80 MB | gix 约 3 MB（ADR 0024） |

刷新是增量的：一次 `refresh(Paths)` 只 stat 和读取报告的路径。大仓库（10 万个跟踪文件）的首次全量比较按 stat 数量线性增长，在后台进行，项目主页先显示会话列表，改动数随后填入。

`bench` 增加的基准（[#7](https://github.com/SoloJiang/lectern-app/issues/7)），夹具由本 crate 的测试仓库构造器生成：

- `git/open`：`discover` + `worktrees`（11 个 worktree）+ `tracked`。
- `git/open_handle`：打开一次 gix 句柄的耗时（每次顶层调用都要付出）。
- `git/changes_clean` / `git/changes_50`：没有改动、50 个约 300 行的文件各改 10 行时的 `changes`（同一个 `Worktree`，不持有上一个改动集，50 个文件每次都读取）。
- `git/refresh_one`：改动一个文件后，持有上一个改动集的 `refresh(Paths)`。
- `git/refresh_full`：持有 50 个文件的改动集、改动早于一秒时的 `refresh(Full)`（改动的文件命中 stat 缓存）。
- `git/diff_50_cold` / `git/diff_50_warm`：728 个文件的两棵树，其中 50 个约 300 行的文件各改 10 行，整个 `diff_trees`（读树、属性、重命名检测、读 blob、hunk），hunk 缓存未命中与命中。比只算 hunk 多出读树、属性和对象头的时间，结果偏保守。
- `git/changes_capped`：10 000 个未跟踪文件、5000 个之后为 `Listed` 时的 `changes`，耗时 < 1 s；`git/changes_capped_bytes`（`memory` 目标，只记录）是调用后改动集、stat 缓存和解析过的 index 仍占的字节数，不超过 `Limits` 默认上限之和（保留内容 32 MiB 加上每个改动文件的条目）。
- `git/resident`（`memory` 目标，≤ 48 MB）：打开 11 个 worktree 的仓库、当前 worktree 的句柄并持有一个 50 个文件的改动集时，本 crate 的存活字节（含这次比较写进 hunk 缓存的部分）。
- `git/filter_cloudprint_bytes`（`memory` 目标，< 1 MB）：project 设计的 `cloudprint` 形状（728 个文件，每个路径都是工作区里的文件，`core.ignoreCase = true`）上，`watch_source().filter(&[], ..)` 构造的过滤器连同 `Repo`、`Worktree` 句柄和 `WorktreeSource` 的存活字节。

10 万个跟踪文件的仓库（[project 设计「性能」](project.md#性能)的 `Shape::monorepo(100_000, 2_000)`，一个 worktree，每个路径都是工作区里的文件，index 带 stat，`core.ignoreCase = true` 与 macOS 默认相同；只写树对象，干净的比较不读 blob）：

- `git/open_100k`（< 40 ms）：同 `git/open`，每次新开 `Worktree`，index 不在缓存里。
- `git/filter_100k`（< 400 ms）：在 index 还没解析的 `Worktree` 上 `watch_source().filter(&[], ..)`，即 index 变化后 fs_watch 重建过滤器的工作。
- `git/changes_clean_100k`（< 2 s）：同 `git/changes_clean`，但每个样本用新的 `Worktree`（不计时），每次比较都解析 index，即第 4 步之后每次 `changes` / `refresh(Full)` 的做法。`refresh(Full)` 对没有改动的文件做同样的工作。
- `git/changes_count_bytes`（`memory` 目标，只记录）：同 `git/changes_capped_bytes` 的 1 万个未跟踪文件，按项目主页的计数比较取 `retained_bytes = 0`、`total_patch_bytes = 0`；改动集连同 `Worktree` 的 stat 缓存和解析过的 index 的存活字节。
- `git/compare_100k/peak_bytes`（`memory` 目标，≤ 72 MB）和 `git/compare_100k/retained_bytes`（`memory` 目标，< 1 MB，计划中，第 4 步）：新 `Worktree` 上一次 `changes(Base::Head)` 期间的存活字节峰值，和丢掉改动集之后 `Worktree` 仍持有的字节。
- `git/filter_100k_bytes`（`memory` 目标，≤ 16 MB）：同 `git/filter_cloudprint_bytes`，在这个仓库上。

## 状态与异常

| 情况 | 本 crate 的结果 | 调用方的处理 |
|---|---|---|
| 不是 git 仓库 | `discover` 返回 `Ok(None)` | 按普通文件夹打开，没有分支和改动集 |
| 不支持的仓库格式（reftable、未知扩展） | `GitError::Unsupported` | 同普通文件夹，另显示一行原因 |
| 没有提交（unborn） | 基准为空树，全部文件是新增 | 照常显示 |
| 浅克隆找不到 merge-base | `GitError::NoMergeBase` | Worktrees 检查器改为显示相对 HEAD 的改动，并注明原因 |
| 单个文件读不到、对象缺失 | 该文件 `Content::Unavailable`，其余照常 | 文件头显示原因 |
| 冲突未解决 | `Status::Conflicted`，工作区内容与基准比较 | 显示冲突标记的原文 |
| 比较期间工作区又变了 | 改动集是比较时读到的内容 | `fs_watch` 的事件触发下一次 `refresh` |
| 取消 | `GitError::Cancelled` | 丢弃，不交给视图 |

加载态的显示规则在视图层（[交互规范「状态与异常」](interaction.md#状态与异常)）。

## 测试

全部在 Linux 上运行（`just test -p lectern-git`）。工作区比较依赖 `platform` 的 Linux 实现；只有 `O_NOFOLLOW_ANY` 本身在 macOS CI 上验证（`platform` 的测试）。

测试仓库有两种来源：

- **构造器**：`TestRepo`，不调用 git 命令行。用 gix 的写入接口在临时目录里建仓库（普通、裸、SHA-1 / SHA-256）、写 blob / 树 / 提交 / 引用 / index，直接写 linked worktree 的管理文件（`<公共目录>/worktrees/<名字>/{HEAD,gitdir,commondir,locked}` 和工作区里的 `.git` 文件）来增加、锁定或制造 prunable 的 worktree，用标准库写工作区文件，提供符号链接、可执行位、FIFO、改 mtime（制造 racy-clean）等操作。放在 `test-support` feature 下导出，供 `project`、`review` 和 `bench` 的测试使用。
- **归档夹具**：需要 git 自己生成的格式放在 `tests/fixtures/<名字>/`（目录格式，见 [夹具说明](../../crates/git/tests/fixtures/README.md)），由同名的 `tests/fixtures/scripts/<名字>.sh` 用真实的 git 生成，脚本头部记录 git 版本；同时保存 `git diff --no-ext-diff --no-textconv -M --histogram --numstat` 等命令的输出作为对照。CI 只导入，不运行 git；git 升级导致格式变化时新增一份样本，不覆盖旧的。包括：linked worktree（锁定、prunable）、SHA-256 仓库、packed-refs、index v4 与 split index、sparse index（`index.sparse` 加 cone 模式的 sparse checkout）、`core.autocrlf` 与 `eol`、UTF-16 的 `working-tree-encoding`、`ident`、Git LFS 属性与过滤器配置、子模块、未解决的冲突、reftable 仓库、各种 reflog 消息。

场景：

- 仓库与引用：`discover` 在子目录、linked worktree、git 目录、普通文件夹里的结果；`worktrees` 的锁定和 prunable；名为 `main` 的 linked worktree 与主 worktree 的 ID 不同，`Repo::worktree` 各自打开正确的那一个；SHA-256 仓库的 OID 是 64 位；reftable 仓库返回 `Unsupported`；`default_branch`、上游与推送目标的解析表；reflog 消息分类逐条覆盖，认不出的是 `Other`。
- 树 diff：新增、删除、修改、模式变化、类型变化、精确与相似重命名、二进制、子模块；`--numstat` 与归档里 git 的输出一致。
- 工作区：已暂存与未暂存的改动、未跟踪文件与忽略规则、racy-clean 的条目比较内容、touch 但内容未变的文件不算改动、换行与编码转换后与 git 的判定一致、LFS 文件为 `Filtered`、FIFO 为 `UnsupportedType` 且不阻塞、中间一级换成符号链接时为 `PathThroughSymlink`、嵌套仓库、超过 64 MB 的文件为 `Unhashed`；sparse index 里目录条目下的文件出现在 `TrackedPaths` 里且为 `skip_worktree`，不报告为删除；`git rm --cached p` 后 `p` 仍在工作区时得到 `Deleted` 和未跟踪的 `Added` 两项，`p` 被 `.gitignore` 忽略时只有 `Deleted`，`git mv a b` 后重建 `a` 时得到 `Renamed` 和未跟踪的 `Added`；未暂存的 `rm` 得到 `Deleted`；HEAD 删除、对方修改的冲突得到没有 `old` 的 `Conflicted`；只改 `.gitattributes`、文件 stat 不变时 `refresh(Full)` 按新规则重新计算 OID；没有改动的跟踪文件不会作为未跟踪文件出现；二进制和只改模式的文件也计入 5000 个文件的上限。
- 截断：单文件、整体、保留内容三种上限的边界值；`NotRetained` 保留原来的 `Detail`：文本文件是带增删行数的 `Truncated`，二进制文件是 `Binary`，行级 diff 过大的是 `TooLarge`。
- 过滤器与目录：LFS 文件暂存了更新、stat 与 index 一致时仍是 `Filtered`；从工作区删除的 LFS 文件是 `Deleted`；没有改动的符号链接和已初始化的子模块不是改动；add/add 冲突后删掉工作区文件仍是两侧都为 None 的 `Conflicted`；跟踪的文件被换成同名目录时得到 `Deleted` 和目录下各文件的未跟踪 `Added`；未初始化的子模块不是改动。
- 暂存：「工作区比较」表的每一行各有一个测试（用 `TestRepo` 分别构造 B、I、W），检查 `Status`、两侧、`Detail` 和 `staged`；其中包括 `git add p; rm p` 得到 `StagedOnly` 的 `Added`，只暂存了可执行位、工作区又改了内容时 `staged` 带 index 的模式。
- 模式：未跟踪的 FIFO 和嵌套仓库的新一侧是 `Mode::Special`；跟踪的文件被换成 FIFO 时是 `TypeChanged`。
- sparse 展开：超过 `EntryLimits` 时 `tracked` 返回 `TooManyEntries`，`cancel` 置位时返回 `Cancelled`；`index_checksum` 与 `TrackedPaths::checksum` 相同，并且不展开条目（sparse index 夹具上用计数钩子检查）。
- prunable 的 worktree：根目录不存在时 `root` 是 `gitdir` 记录的路径去掉 `/.git` 后字面归并的结果。
- 句柄：一次顶层调用返回后，当前线程上没有留下 gix 句柄（测试用计数钩子检查）。
- 路径：index 里含 `..`、`.`、空名字、NUL 或以 `/` 开头的条目不被打开，记在 `Totals::invalid_paths` 里；`RelPath::new` 拒绝这些写法；Linux 上 `SafeRoot` 对 `..` 和绝对路径的解析被 `RESOLVE_BENEATH` 拒绝（`platform` 的测试）。
- 过滤器的快路径：未改动的 LFS 文件（stat 一致、index 与基准 OID 相同）不是改动；暂存了更新的是 `Filtered`。
- 重命名：40 个文件同时改名并小改时都识别为相似重命名；未暂存的 `mv a b` 得到 `Deleted(a)`（`staged` 为 a 的 index 条目）和未跟踪的 `Added(b)`，不配对；来源 × 目标超过上限的平方时只有精确重命名。
- `discover`：`ceilings` 里的目录之上的仓库不被找到。
- 只读：每个工作区测试前后对 `.git` 下全部文件的路径、大小和内容哈希做快照，必须相同。
- 规范内容的 OID（`canonical_*`，在 `compare/tests.rs`）：工作区转换夹具在 `core.autocrlf` 为 false、true、input 时，每个文件的原始字节得到 git 记录的 OID；`text=auto`、`eol=lf`、`text eol=lf` 下 LF 和 CRLF 的字节都得到 index 条目的 OID，index 的 blob 已有 CRLF 时 `text=auto` 不转换；`ident` 展开的关键字收回；clean / process 过滤器返回 None，只有 smudge 的和没有配置的驱动不影响结果；各级目录的 `.gitattributes`（未跟踪的、从工作区删掉而用 index blob 的）都生效；SHA-256 仓库得到 64 位的 OID。每次调用前后 git 目录不变。
- 不执行外部程序：夹具仓库配置 `diff.external`、`diff.<名>.textconv`、`filter.<名>.clean` / `process`、`core.fsmonitor`，指向会写标记文件的脚本，PATH 最前面放一个记录调用的假 `git`（AGENTS.md「工作方式」）；跑完全部工作区和树 diff 场景后，标记文件不存在、假 `git` 没有被调用。
- 展示行与词级差异：配对、折叠、hunk 相邻时合并上下文，用 insta 快照（带每行的字节偏移）。
- 字节偏移：全部树夹具和工作区转换夹具（三种 `core.autocrlf`）里每个文本文件的 hunk 偏移等于这一行在 `texts` 文本里的行首；上下文为 0、3 和全部显示时，每个展示行的偏移处恰好是这一行的文本，之后是 `\n` 或文本末尾；词级差异的区间加上行的偏移得到同样的字节。覆盖 CRLF、末尾没有换行、重命名、新增和删除；`core.autocrlf = true` 下 CRLF 的工作区文件按规范内容（LF）计偏移。
- 缓存：同一对 OID 第二次计算命中，命中的 hunk 带同样的偏移；格式版本 2 的条目不被版本 3 读出；键里的 `HashKind` 不同时不命中；命中时按这次的 `diff_bytes` 判断 `TooLarge`，另一个仓库里缺失的一侧仍是 `Missing`。
- 取消：`cancel` 置位后返回 `Cancelled`，没有部分结果。

改动集、hunk 和展示行的结构化输出都用 insta 快照验收。

## 结构化 diff 的扩展点（#21）

P2 的结构化 diff（忽略纯格式变化、识别移动的代码）在本 crate 的行 diff 之上做，P1 预留：

- `Detail::Text` 的 `FileDiff` 是行级结果；结构化结果作为同一文件的另一种 `Detail` 变体加入，不改 `Change` 的其余字段。
- hunk 缓存键里有「算法」和「空白处理」两项，结构化 diff 与「忽略空白」各用自己的取值，与行 diff 的结果分开缓存。结构化 diff 需要语法树时，键里再加 grammar 与查询的版本（#42）。
- `Row` 以后可以增加「移动来源 / 去向」的标注，统一视图与双栏视图仍然共用一个模型。
- 语法解析由 `code_intel` 提供，本 crate 不直接依赖 tree-sitter。

## 未决问题

1. **P1 是否在 Lectern 里提交。** 交互规范的核心闭环第 4 步是 Looks good 之后「进入提交或开 MR」。选项：
   - (a) P1 不做提交界面（倾向）：Looks good 只标记已看，提交在 Conversation 的终端里由用户执行，或发回会话让 agent 提交。本 crate 保持只读。
   - (b) P1 做提交界面：要执行用户的 pre-commit 等钩子（外部程序），需要另写设计，并决定由 `git` 命令行还是 gix 完成。

   由产品负责人决定。默认按 (a) 实现；改成 (b) 时另开 issue，不改本文的只读约束。

技术问题都已在上文决定，没有其他未决项。

## 验收

分四步实现，每步一个 PR，都写 `Refs #14`；#14 剩下的统一 / 双栏视图由 `review_view` 完成，最后完成的那个 PR 写 `Closes #14`。每一步都要 `just check`、`just test -p lectern-git`、`just deny` 通过。

1. **仓库、worktree、引用与 index**：根 `Cargo.toml` 按 ADR 0024 加入 `gix`。
   - 前置：`platform` 的安全读取（Linux 与 macOS 实现）已合并。
   - `just test -p lectern-git` 里 `repo_*`、`worktrees_*`、`refs_*`、`reflog_*`、`tracked_*`、`read_file_*` 测试通过，其中有 SHA-256 夹具和 reftable 夹具；`TestRepo` 在 `test-support` feature 下可用，`rg 'Command::new' crates/git/src` 没有结果。
   - `cargo tree -i gix -e normal --depth 1` 只列出 `lectern-git`。
   - `bench` 的 `git/open` 在预算内。bench 的设计文档（#7）合并之前不写基准代码，先由 `#[ignore]` 的 `perf_smoke_git_open` 在 release 下测量（见「与设计的差异」）。bench 第 1 步之后由 `just bench git/` 测量，macOS CI 判定。
2. **树之间的改动集、hunk 与展示行**：
   - `diff_trees_*`、`rename_*`、`binary_*`、`truncate_*`、`rows_*`、`inline_*`、`hunk_cache_*` 测试通过，结构化输出有 insta 快照；`numstat_matches_git` 对全部归档夹具通过。
   - `git/diff_50_cold` / `git/diff_50_warm` 在预算内。
3. **工作区比较**：
   - `worktree_*`、`read_only_*`、`no_external_programs` 测试通过。
   - `rg 'std::fs::(File::open|read|metadata|symlink_metadata)' crates/git/src --glob '!gitdir.rs' --glob '!testing.rs' --glob '!**/tests.rs'` 没有结果：工作区只经 `SafeRoot` 读取。git 目录里的管理文件（`HEAD`、`commondir`、`gitdir`、`locked`、`config`、index 末尾的校验和）都在 `gitdir.rs` 里读；`testing.rs` 和测试代码构造夹具。
   - `git/changes_clean`、`git/changes_50`、`git/refresh_one`、`git/changes_capped` 和内存基准在预算内。
4. **改动来源接口**：`services` 的设计文档合并后，为本地仓库实现它的改动来源接口（取 diff、加载完整文件；指纹随 #66），按那份文档的验收条件测试。由哪个插件把它注册为服务按 #47 的结论处理，不在本 crate。
   - 同一步：工作区比较解析的 index 只在一次 `changes` / `refresh` 调用内持有（「缓存」，[ADR 0035](../adr/0035.md)）。`compare_releases_its_index_*` 测试检查调用返回后 `Worktree` 不再持有它、下一次 `refresh(Paths)` / `refresh(Full)` 重新解析且结果不变；bench 的 `git/compare_100k/retained_bytes` 改为 `Active`（< 1 MB），`git/changes_clean_100k`、`git/refresh_one`、`git/refresh_full` 仍在预算内。

## 实际验证

第 1 步，在 Linux（x86_64，4 核 Xeon 2.1 GHz，内核 6.18）上运行。本 crate 不依赖 GPUI，也没有 macOS 专属代码；macOS 上的结果由 macOS CI 给出。

- `just check`、`just test`（整个工作区 399 个测试通过，跳过 2 个 `#[ignore]`）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）、`python3 scripts/check_architecture.py` 全部通过。
- `just test -p lectern-git`：68 个测试通过，跳过 `perf_smoke_git_open`。按前缀：`repo_*` 21、`worktrees_*` 9、`refs_*` 13、`reflog_*` 4、`tracked_*` 14、`read_file_*` 7。SHA-256 夹具（`TestRepo::sha256`）和 reftable 夹具（`TestRepo::reftable`）都在其中。
- `cargo tree -i gix -e normal --depth 1` 只列出 `lectern-git`；`cargo tree -p lectern-git -e features -i gix-features` 里没有 `parallel`。`rg 'Command::new' crates/git/src` 没有结果。
- Cargo.lock 只新增 gix 及其传递依赖，已有依赖的版本没有变化。
- 测试不依赖 git 命令行和用户配置：测试二进制在 `env -i PATH=/nonexistent`（没有 `PATH` 里的 git，没有 `HOME`、`XDG_CONFIG_HOME`）下运行，68 个测试全部通过。
- 第 3 步的检查（「验收」第 3 步，排除 `gitdir.rs`、`testing.rs` 和测试）现在就没有结果。
- 计数钩子：`repo_handles_never_outlive_a_call` 在每次顶层调用后检查当前线程上没有存活的 gix 句柄；`tracked_index_checksum_reads_only_the_trailer` 在 sparse index 夹具上检查 `index_checksum` 不解析条目、不展开目录、不打开句柄，且与 `TrackedPaths::checksum` 相同。
- ceiling 的语义用真实的 git 2.43 手工对照过（不进入测试）：`GIT_CEILING_DIRECTORIES=<worktree 根>` 时在子目录里找不到仓库，在根目录本身里能找到；`repo_discover_stops_at_ceilings` 断言相同的结果。
- `perf_smoke_git_open`（`cargo test --release -p lectern-git -- --ignored perf_smoke`，728 个跟踪文件、11 个 worktree、每个 worktree 都有自己的 index，200 次）：`discover` + `worktrees` + `worktree(Main)` + `tracked` 中位数 0.46 ms，p95 0.56–0.70 ms，最慢 0.8 ms；打开一次 gix 句柄中位数 48 µs，最慢 0.18 ms。预算（M 系列 Mac）是 < 15 ms 和 < 1 ms。测量用的是只读仓库自身配置的 `testing::discover`；生产路径另外读系统和用户的配置文件。没有在 macOS 上测量。
- 没有验证的：`bench` 的 `git/open` 没有写（见「与设计的差异」）；macOS 上没有运行。

第 2 步，同一台机器：

- `just check`、`just test`（整个工作区 519 个测试通过，跳过 5 个 `#[ignore]`）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）、`python3 scripts/check_architecture.py` 全部通过；`rg 'Command::new' crates/git/src` 没有结果。没有新增依赖，只把工作区已有的 `insta` 加为 dev 依赖，Cargo.lock 里已有依赖的版本没有变化。
- `just test -p lectern-git`：123 个测试通过，跳过 `perf_smoke_git_open` 和 `perf_smoke_git_diff_50`。第 2 步新增 55 个，按前缀：`diff_trees_*` 12、`rename_*` 12、`binary_*` 3、`truncate_*` 8、`rows_*` 8、`inline_*` 5、`hunk_cache_*` 6、`numstat_matches_git` 1。结构化输出有 14 个 insta 快照，逐个看过。
- `numstat_matches_git` 对 4 个目录夹具的全部 7 个变体（`basic`、`real`、`renames` 与 `renames/renameLimit=1`、`rename_many` 与 `renameLimit=39`、`renameLimit=40`）通过：改动集换算成 `--numstat -z` 和 `--raw --no-abbrev -z` 后与 git 2.43.0 的输出逐字节相同，包括每个重命名的相似度分数（`R059`、`R079`、`R088`、`R092`、`R094` 等）、git 的同名优先、basename 一轮在上限之前、`renameLimit=39` 时只剩 5 个精确重命名。`real` 里 6 个本仓库历史文件和一个高度重复的文件，Histogram 的增删行数与 git 相同。
- 测试不依赖 git 命令行和用户配置：测试二进制在 `env -i PATH=/nonexistent CI=1 INSTA_WORKSPACE_ROOT=<仓库根>`（insta 没有 `cargo` 时要靠它找快照目录）下运行，123 个测试全部通过。夹具里的 git 输出是在容器里用 git 2.43.0 生成的一次性数据（`tests/fixtures/scripts/`）。
- 计数钩子：`diff_trees_opens_one_handle_and_keeps_none` 检查一次 `diff_trees` 只打开一个 gix 句柄、`diff_trees` 和 `texts` 返回后没有存活的句柄；`hunk_cache_second_computation_hits` 检查同一对 OID 第二次命中（另一个仓库里内容相同的文件也命中），`hunk_cache_misses_across_hash_kinds` 检查 SHA-1 与 SHA-256 不共用条目，`hunk_cache_skips_additions_and_type_changes` 检查新增和类型变化不查缓存。
- `perf_smoke_git_diff_50`（`cargo test --release -p lectern-git --all-features -- --ignored perf_smoke`；728 个文件的树，其中 50 个约 300 行的文件各改 10 行，松散对象）：整个 `diff_trees`（读树、属性、重命名检测、读 blob、hunk）缓存未命中 30 次，中位数 4.4 ms，最慢 4.8 ms；缓存命中 200 次，中位数 1.26 ms，p95 1.44 ms，最慢 2.3 ms；一个可见文件的 `texts` + `rows` 0.27 ms。预算（M 系列 Mac）是未命中 < 20 ms、命中 < 2 ms；命中时的耗时主要是读树和每个 blob 的对象头（判断 `TooLarge` 和对象缺失），200 次里有一次超过 2 ms。没有在 macOS 上测量。
- 没有验证的：`bench` 的 `git_diff_50_cold` / `git_diff_50_warm` 没有写（bench 设计文档 #7 合并之前不写，由上面的 `perf_smoke_git_diff_50` 代替）；保留内容上限（`NotRetained`）在树之间的比较里不会出现，只用内存里的新一侧在组装这一步上测过，工作区一侧的真实路径随第 3 步验证；macOS 上没有运行。

第 3 步，同一台机器：

- `just check`、`just test`（整个工作区 731 个测试通过，跳过 8 个 `#[ignore]`）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过；`cargo clippy -p lectern-git --all-targets -- -D warnings`（不开 `test-support`）也通过。没有新增依赖：只把工作区已有的 `rustix` 加为 Linux 上的 dev 依赖，Cargo.lock 里已有依赖的版本没有变化。
- `just test -p lectern-git`：183 个测试通过，跳过 4 个 `perf_smoke_*`。第 3 步新增 60 个：`worktree_*` 57（其中「工作区比较」表的每一行各一个 `worktree_table_*`，共 16 个，另有全表一个快照）、`read_only_*` 2、`no_external_programs` 1。结构化输出新增 3 个 insta 快照（全表、未跟踪与忽略规则、保留内容上限），逐个看过。
- 验收里的两条检查：`rg 'std::fs::(File::open|read|metadata|symlink_metadata)' crates/git/src --glob '!gitdir.rs' --glob '!testing.rs' --glob '!**/tests.rs'` 和 `rg 'Command::new' crates/git/src` 都没有结果。
- 只读：每个工作区测试的比较都包在快照检查里（git 目录下全部文件的路径、大小和内容哈希，以及 index 的 mtime，前后相同）；`read_only_racy_entries_are_not_refreshed` 用与 index 同一时刻的 racy 条目跑 `changes`、`refresh(Full)`、`refresh(Paths)`，index 逐字节不变；`read_only_worktree_contents_stay_out_of_the_object_database` 检查工作区一侧的 OID 不在对象库里。
- 不执行外部程序：`no_external_programs` 的仓库配置 `core.fsmonitor`、`core.hooksPath`（目录里放了各种钩子）、`diff.external`、`diff.x.textconv`、`filter.x.clean` / `smudge` / `process`（`required = true`）、`gpg.program`，都指向写标记文件的脚本；在 nextest 下 PATH 最前面另有记录调用的假 `git` 和 `sh`。跑完 `changes`、`refresh(Full)`、`refresh(Paths)`、每个文件的 `texts` 和 `rows`、`diff_trees` 和它的 `texts` 之后，标记文件不存在。
- 与 git 对照：`worktree_conversions_match_git` 对 git 2.43.0 生成的工作区夹具（`tests/fixtures/scripts/worktree_conversions.sh`）在 `core.autocrlf` 为 `false`、`true`、`input` 时，15 个文件的规范内容 OID 都与 `git add -A` 之后的 `git ls-files -s` 相同。
- 测试不依赖 git 命令行和用户配置：测试二进制在 `env -i PATH=/nonexistent CI=1 INSTA_WORKSPACE_ROOT=<仓库根>` 下运行，183 个测试全部通过。
- 计数钩子：`worktree_clean_reads_nothing`、`worktree_matching_stat_skips_the_file` 检查 stat 一致的文件不读；`worktree_refresh_paths_reads_only_the_reported_paths` 检查 `refresh(Paths)` 只读报告的路径、没变的内容与上一个改动集共享同一份字节，结果与重新 `changes` 相同；`worktree_stat_cache_reuses_contents_a_change_set_still_holds` 检查 stat 缓存只在改动集还持有内容时命中；`worktree_handles_never_outlive_a_call` 检查返回后没有存活的 gix 句柄。
- 性能（`cargo test --release -p lectern-git --all-features -- --ignored perf_smoke --test-threads=1`；728 个跟踪文件、11 个 worktree，50 个约 300 行的文件各改 10 行，松散对象）：
  - `git/changes_clean`：同一个 `Worktree` 200 次中位数 3.0 ms，p95 4.7 ms，最慢 9.1 ms；每次新开 `Worktree`（要解析 index）100 次中位数 3.3 ms，最慢 7.6 ms。预算 < 15 ms。
  - `git/changes_50`：第一次（hunk 未命中）9.8 ms，之后 29 次中位数 6.1 ms，最慢 9.1 ms（50 个文件每次都经 `SafeRoot` 读取、转换、算 OID）。预算 < 15 ms。
  - `git/refresh_one`：改一个文件后 `refresh(Paths)` 200 次中位数 1.5 ms，p95 2.8 ms，最慢 5.8 ms；`refresh(Full)`（50 个改动文件走 stat 缓存）100 次中位数 4.2 ms，最慢 8.9 ms。预算（`refresh(Full)`）< 15 ms。
  - `git/changes_capped`：10 000 个未跟踪文件、5000 个之后为 `Listed`，5 次中位数 113 ms，最慢 125 ms；调用后改动集、stat 缓存和解析过的 index 共占 5.0 MiB（保留内容上限 32 MiB）。预算 < 1 s。
  - 内存：打开仓库、列出 11 个 worktree、打开当前 worktree 并持有一个 50 个文件的改动集，增量 0.79 MiB（含进程内的 hunk 缓存），释放改动集后 0.77 MiB。
  - 同一次运行里第 2 步的 `perf_smoke_git_diff_50` 未命中中位数 7.4 ms、命中 1.5 ms，比第 2 步记录的慢（机器负载不同），只作参考。没有在 macOS 上测量。
- 没有验证的：`bench` 的 git 基准没有写（见「与设计的差异」）；超过 64 MB 的文件用调小的 `Limits::read_bytes` 测 `Unhashed`，没有真的写 64 MB；macOS 上没有运行（`O_NOFOLLOW_ANY` 由 `platform` 的测试覆盖）。

第 3 步的审阅修正（PR #87，[ADR 0028](../adr/0028.md)），同一台机器：

- 未跟踪文件改为在 `SafeRoot::list` 上遍历后，`just test -p lectern-git` 188 个测试通过（新增 5 个 `worktree_*`，共 62 个），原有测试的结果（含三个快照）都没有变；`just test -p lectern-platform` 新增 `list_root_and_subdirectories_with_kinds`、`list_never_follows_symlinks`、`race_list_swap_never_escapes`，对 `Openat2` 和 `Walk` 都通过。`cargo clippy -p lectern-platform -p lectern-git --all-targets --all-features --target aarch64-apple-darwin -- -D warnings` 交叉检查通过，没有在 macOS 上运行。
- `worktree_symlinked_gitignore_outside_the_worktree_is_not_read`：未跟踪的 `.gitignore` 是指向工作区之外文件的符号链接（gix 的遍历会跟随它），那个文件里的规则不生效，`secret.txt` 和 `sub/x` 照常列出。`worktree_directory_through_a_symlink_is_not_walked`：指向外部目录的符号链接只作为一项列出，外部目录里的 `.gitignore` 和文件都不出现。`worktree_race_directory_swapped_for_a_symlink_never_escapes`：另一个线程反复把 `w` 在目录和指向外部目录的符号链接之间交换，300 次 `changes` 里从不出现外部目录的文件或内容。`worktree_submodule_gitdir_outside_modules_is_not_read`：`gitdir:` 指向 `modules/` 之外时为 `Unreadable`，指向 `modules/ok` 时读出它的 HEAD。`worktree_staged_mv_then_removed_is_not_a_rename`：`git mv a b; rm b` 得到 `Deleted(a)` 和 `StagedOnly` 的 `Added(b)`。
- 性能（同一命令）：`git/changes_clean` 中位数 3.0 ms（新开 `Worktree` 3.6 ms），`git/changes_50` 首次 7.1 ms、之后中位数 4.6 ms，`git/refresh_one` 中位数 1.2 ms，`refresh(Full)` 中位数 4.4 ms、最慢 14.0 ms，`git/changes_capped` 中位数 128 ms，内存增量 0.78 MiB；都在预算之内，与改之前同一量级。
- 第二轮审阅的修正：`worktree_absorbed_submodule_symlinks_under_modules_are_not_followed`（`modules/<名字>/HEAD` 和 `modules/<名字>` 本身是指向外部的符号链接时都为 `Unreadable`，外部 HEAD 里的提交不出现）、`worktree_ignore_case_matches_index_paths_without_case`（`core.ignoreCase` 为 true 时大小写不同的 `README.md`、`docs/a.md` 不是未跟踪的新增，为 false 时是）、`worktree_refresh_paths_of_a_directory_reloads_its_attributes`（报告的目录下改了已加载的 `.gitattributes`、或新出现未跟踪的 `.gitattributes` 时，`a.txt` 按新规则重新计算）。gix 的 `dirwalk` feature 去掉后 `cargo tree -e features -i gix` 里没有它，`gix-dir` 从 Cargo.lock 移除。
- 移进 bench 后（`just bench --measure git/`，Linux 上只报告、不判定）：`git/changes_clean` 3.03 ms、`git/changes_50` 4.46 ms、`git/refresh_one` 1.21 ms、`git/refresh_full` 4.05 ms、`git/changes_capped` 99 ms，`memory` 目标的 `git/resident` 865 KB（≤ 48 MB）、`git/changes_capped_bytes` 9.47 MB（只记录；`Limits` 默认的保留内容上限是 32 MiB）。同一次运行里 `git/diff_50_warm` 为 2.18 ms，超过 2 ms 的预算（第 2 步的 hunk 缓存命中路径，与本步无关，由 #89 处理）。没有在 macOS 上测量。

命中缓存时不读对象头，同一台机器：

- 起因：macOS CI（3 核 M1 虚拟机）上 bench 的 `git/diff_50_warm` 中位数 2.61–2.78 ms，超过 < 2 ms 的预算。在 Linux 上分段计时（300 次的中位数，整个 `diff_trees` 1.58 ms）：打开句柄 87 µs、剥离两个提交 36 µs、遍历树 69 µs、建属性栈 6 µs、查属性 29 µs、查 hunk 缓存 9 µs、释放 8 µs，100 个 blob 的对象头 1.16 ms（占 73%；松散对象每个都要打开文件、读取并解压开头）。
- `perf_smoke_git_diff_50`（同上）：命中中位数从 1.45–1.60 ms 降到 0.34–0.35 ms（−77%），p95 从 1.78–1.85 ms 降到 0.45–0.48 ms；未命中中位数改动前 5.2–5.5 ms、改动后 5.1–5.4 ms，没有变化。bench 分支（#83）的 `git/diff_50_warm` 同一台机器上 1.51–1.52 ms → 0.34–0.36 ms，`git/diff_50_cold` 5.03–5.21 ms → 5.02–5.13 ms。没有在 macOS 上测量。
- `just test -p lectern-git` 全部通过，insta 快照和 `numstat_matches_git` 没有变化；新增 `hunk_cache_hit_checks_limits_attributes_and_missing_objects`：命中时按这次更小的 `diff_bytes` 得到 `TooLarge`，`-diff` 的文件不查缓存且 `Binary` 带两侧大小，另一个缺少新一侧 blob 的仓库里命中后仍是 `NoDiff` 加 `Unavailable::Missing`。

`Worktree::watch_source`（[fs_watch 设计](fs_watch.md)验收第 2 步，[#95](https://github.com/SoloJiang/lectern-app/pull/95)），同一台机器：

- `just check`、`just test`（Linux 上的全部 crate，950 个测试通过，跳过 1 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）、`just bench`（Linux 上是冒烟运行）全部通过；`cargo clippy -p lectern-git --all-targets -- -D warnings`（不开 `test-support`）、`python3 scripts/check_architecture.py` 也通过。Cargo.lock 只多了 `lectern-git` 对 `lectern-fs-watch` 的一条工作区内部依赖，没有新增外部依赖。
- `just test -p lectern-git`：206 个测试通过，新增 `watch_source_*` 15 个（`src/watch/tests.rs`），对应 fs_watch 设计「测试」里 `git` 的一段：`.gitignore`、`info/exclude`、`core.excludesFile` 的忽略结果在 `core.ignoreCase` 为 false 和 true 时都与 git 的判定一致（`ignores_match_git`，按 fs_watch「路径分类」第 6、8 步逐级组合 `ignored` 的结果，含根目录和未跟踪目录里未跟踪的 `.gitignore`、更深的 `.gitignore` 里的否定规则、被忽略目录里的 `.gitignore` 没有读）；`ignored` 只看路径自身；`core.ignoreCase` 下大小写不同的路径判为受跟踪（含目录和子模块），过滤器与 `Worktree::tracked` 共用同一个 `Arc`；跟踪的和 skip-worktree 的 `.gitignore`（后者从 index 的 blob 读）；遍历达到上限时显式给出的 `.gitignore` 仍然生效、上级目录被忽略的不读、子模块和嵌套仓库里的不读；目录改名后 `rule_files` 给出新位置；符号链接的 `.gitignore` 不读；`config_files` 列出 `include.path`（绝对、相对、`~/`、不存在的目标）、`includeIf "onbranch:"` 满足与不满足（只有满足的那个里再嵌套的引入被列出，切换分支后反过来）、系统、XDG 和 `~/.gitconfig` 的位置、XDG 默认的 `ignore` / `attributes`、linked worktree 的 `config.worktree`，测试用的仓库范围不列出仓库之外的标准位置；`runs_no_git` 在仓库配置 `core.fsmonitor` 指向写标记文件的脚本、nextest 下 PATH 最前面另有记录调用的假 `git` 时，构造过滤器和配置清单后标记文件不存在；取消时两者都返回错误。
- 验收的检查：`rg 'Command::new' crates/git/src` 没有结果；第 3 步的 `rg 'std::fs::(File::open|read|metadata|symlink_metadata)' …` 检查也仍然没有结果。
- 测试二进制在 `env -i PATH=/nonexistent CI=1 INSTA_WORKSPACE_ROOT=<仓库根>` 下运行，206 个测试全部通过。
- 与 git 的对照：`watch_source_ignores_match_git` 的期望值（git 判为忽略的路径，即 `git check-ignore --no-index` 会列出的）是按 gitignore(5) 的规则手工推出的，没有用真实的 git 生成夹具：这次的环境不允许在 worktree 之外运行 git（生成脚本要在临时目录里 `git init`、`git check-ignore`）。gix-ignore 的匹配结果与推导完全一致。以后可以按「测试」里归档夹具的做法补一份由 `git check-ignore` 生成的目录夹具。
- 没有验证的：构造过滤器的耗时和忽略规则的内存（fs_watch「性能」里没有对应的基准条目，没有测量；遍历与「工作区比较」第 4 步相同，代价同 `git/changes_clean` 里的未跟踪文件枚举）；`core.precomposeUnicode` 的规范化没有测试；macOS 上没有运行。

`Worktree::canonical_oid`（[code_intel 设计](code_intel.md#对其他-crate-的要求)前置条件 E，[#110](https://github.com/SoloJiang/lectern-app/pull/110)），同一台机器：

- `just check`、`just test`、`just doctest`、`just deny` 全部通过。`just test -p lectern-git` 共 213 个测试，新增 `canonical_*` 6 个；`no_external_programs` 增加两次 `canonical_oid` 调用（配置了 clean / process 的 `b.f` 返回 None，`a.t` 返回原始字节的 OID），之后标记文件仍不存在。
- `canonical_matches_git_for_the_conversion_fixture` 用的期望值是 `tests/fixtures/worktree_conversions` 里 git 2.43 `git add` 记录的 OID（已有夹具，测试不运行 git）：三个 `core.autocrlf` 变体下 15 个文件全部相同，包括 UTF-16LE、带 BOM 的 UTF-16、`ident`、混合换行、单独的 CR 和带 NUL 的 `text=auto`。
- `rg 'Command::new' crates/git/src` 没有结果；「验收」第 3 步的 `rg 'std::fs::(File::open|read|metadata|symlink_metadata)' …` 也仍然没有结果。
- `code_intel` 用到的其余接口不需要改动：`GitAccess::head` 取 `Worktree::head` 返回的 `HeadState` 里的提交（`Branch { commit }` 的 `commit`、`Detached { commit }`，unborn 为 None），`GitAccess::index_checksum` 直接转调 `Worktree::index_checksum`；两者返回 `Oid`，转成 `services` 的 `BlobId` 是 `code_intel` 自己的事。
- 没有验证的：没有基准（code_intel 的设计没有为它列基准条目，调用频率由原始 OID 不等的文件数决定）；macOS 上没有运行。

hunk 与展示行的字节偏移（[ADR 0034](../adr/0034.md)，[#122](https://github.com/SoloJiang/lectern-app/pull/122)），同一台机器：

- `just check`、`just test`（Linux 上的全部 crate，1271 个测试通过，跳过 1 个）、`just doctest`（7 个）、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。`just test -p lectern-git` 共 221 个测试，新增 8 个：`hunk_offsets_count_every_byte_of_the_lines_before`、`hunk_cache_keeps_the_byte_offsets`、`hunk_cache_entry_of_an_older_format_is_not_read`、`hunk_cache_hit_has_the_offsets_of_its_own_contents`、`rows_offsets_with_crlf_and_a_missing_final_newline`、`rows_offsets_of_every_row_start_its_line`、`rows_offsets_start_their_lines_in_every_fixture`、`worktree_byte_offsets_index_the_canonical_content`；`rows_*` 的 5 个快照加上每行两侧的偏移，新增 1 个 CRLF 与末尾没有换行的快照，逐个手算核对过。
- 偏移的检查（`rows::tests::assert_offsets`）：每个 hunk 的偏移等于这一行在 `texts` 文本里的行首；上下文为 0、3 和全部显示时，每个展示行的偏移在行首（为 0 或前一个字节是 `\n`），从偏移起恰好是 `Text::line` 的字节，之后是 `\n` 或文本末尾；配对行的词级区间加上行的偏移得到同样的字节。4 个树夹具（`basic` 的 `crlf.txt`、`noeol.txt`，`renames` 的相似重命名，`real`，`rename_many`）的全部文本文件，以及 `worktree_conversions` 在 `core.autocrlf` 为 false、true、input 时的全部文本文件（`-text` 保留的 CRLF、混合换行、单独的 CR、UTF-16 转成 UTF-8）都通过。
- 规范内容的口径：`core.autocrlf = true`、HEAD 为 `one\ntwo\nthree\n`、工作区为 `one\r\nTWO\r\nthree\r\n` 时，hunk 的两侧偏移都是 4（原始字节里是 5），`texts` 的新一侧是转换后的 `one\nTWO\nthree\n`。
- 缓存：`CACHE_FORMAT` 为 3；用同样两个 OID、格式 2 的键写入的条目，格式 3 的键查不到；命中的 hunk 与计算的相同（偏移在内），另一个仓库里路径不同、内容相同的文件命中同一条目，偏移对它的文本照样成立。`Hunk` 从 16 字节变为 32 字节，LRU 的开销按 `size_of::<Hunk>()` 计，16 MiB 的预算不变。
- 计时（`just bench --measure git/`，Linux 上只记录、不判定）：`git/diff_50_cold` 中位数 4.53 ms（改动前同一台机器 4.84 ms，预算 < 20 ms），`git/diff_50_warm` 312 µs（改动前 301 µs，预算 < 2 ms），差别在噪声以内；`git/resident` 874 KB（≤ 48 MB），其余 `git/` 基准都在预算内。
- 没有验证的：macOS 上没有运行；视图按偏移打开完整文件、按窗口高亮的使用方还没有实现（`file_reader`、`review_view`）。

fs_watch 过滤器的内存归属与 10 万文件的份额（[ADR 0035](../adr/0035.md)，bench 的基准，本 crate 的代码没有改动），同一类 Linux 虚拟机（x86_64，4 核）：

- `just bench --measure git/`（release，不判定；运行期间同一台机器上另有构建，负载约 9）：`git/open_100k` 中位数 17.2 ms（< 40 ms），`git/filter_100k` 276 ms（< 400 ms），`git/filter_100k_bytes` 13.9 MB（≤ 16 MB）；另一次单独运行这些基准、机器空闲时是 16.4 ms、214 ms 和 13.9 MB。`git/filter_cloudprint_bytes` 111 KB（< 1 MB）。原有的 `git/*` 都在预算内，与上一次记录同一量级。
- 工作区比较自己的 index（同一仓库，`memory` 目标）：新 `Worktree` 上一次 `changes` 期间的峰值 65.8 MB（`git/compare_100k/peak_bytes`，≤ 72 MB）；返回、丢掉改动集之后 `Worktree` 还持有 39 MB，几乎都是比较解析的 index（gix 的 `State`、每条一个 `IndexItem`、折叠副本），这就是「缓存」一节要求第 4 步在调用返回时释放的部分（`git/compare_100k/retained_bytes` < 1 MB，计划中）。
- `git/changes_count_bytes`：1 万个改动路径 9.32 MB，约 0.95 KB 一个（同一次运行的 `git/changes_capped_bytes` 9.54 MB：这些文件只有三行，保留内容本来就少，差别主要在大文件）。
- `git/changes_clean_100k`：每个样本新开 `Worktree`、index 每次都解析时中位数 1.0 s（负载约 9，`noisy`，5 个样本）；第一版复用同一个 `Worktree`、index 已解析时是 823 ms（空闲）和 881 ms（负载约 9）。预算 < 2 s 按重新解析的做法给出。
- `git/filter_100k_bytes` 的 13.9 MB 比 project 第 5 步测得的 13.5 MB 多出折叠顺序（`core.ignoreCase`，每条 4 字节，约 0.4 MB）；夹具的忽略规则只有三条，占用很小。
- 预算按 Linux 上的中位数留出约 2 倍余量：macOS CI 的 3 核 M1 虚拟机上 `git/diff_50_warm` 曾比这里慢 1.7 倍（见上文）；`changes` 每个文件一次 `SafeRoot::stat`，macOS 上的 `platform/safe_stat` 预算是 < 10 µs。
- 没有验证的：macOS 上没有运行，三项计时和三项内存由 macOS CI 判定；比较用的 index 在调用返回时释放还没有实现（第 4 步）。

## 与设计的差异

第 1 步：

- `bench` 的 `git/open` 没有加入：`crates/bench/AGENTS.md` 要求 bench 的设计文档（#7）合并之后才写基准代码。在那之前用 `#[ignore]` 的 `perf_smoke_git_open` 在 release 下测量，结果见「实际验证」。bench 第 1 步（[bench 设计「迁移」](bench.md#迁移)）已把它测的两项移进 `crates/bench` 的 `git/open` 和 `git/open_handle`，后者用新增的 `testing::open_handle`，`perf_smoke_git_open` 随之删除。
- 夹具在测试里用 `TestRepo` 构造，没有用归档。容器里的 git 是 2.43，还不支持 reftable（2.45 起），工作区也没有解包 tar 的依赖。reftable 夹具按 `git init --ref-format=reftable` 留下的布局逐项写出：版本 1 加 `extensions.refStorage = reftable`、HEAD 指向 `refs/heads/.invalid`、`refs/heads` 是普通文件、`reftable/tables.list` 列出一张表（表里只有 reftable 文件头，本 crate 不读它）。sparse index 由 gix 写出：先写含目录条目的 index，再读回、重写一次，gix 就加上 `sdir` 扩展，与 `index.sparse = true` 时 git 写出的结构相同。需要和 git 输出对照的归档夹具（`numstat_matches_git` 等）随第 2 步加入。
- 依赖表里的「`gix`（dev，额外开写入用的 feature）」不需要：gix 0.88 在 ADR 0024 的 feature 下已能写对象、引用文件和 index。`TestRepo` 只多用一个可选的 `tempfile`（`test-support` 打开），已改依赖表。
- `discover` 的 ceiling：gix 0.88 会检查 ceiling 目录本身，git 不会进入 ceiling（只检查起点本身就是 ceiling 的情况）。gix 找到的仓库所在目录恰好是起点之上的某个 ceiling 时，本 crate 返回 `Ok(None)`，与 git 一致。
- `format` 检查在 gix 打开仓库之前读公共目录的 `config`：gix 0.88 不拒绝未知的扩展，reftable 仓库它也能打开。版本 0 接受 git 在 v0 下也认的 `noop`、`preciousObjects`、`partialClone`、`worktreeConfig`，忽略不认识的扩展，`objectFormat` 等只属于 v1 的扩展视为不支持（git 在这种情况下拒绝打开）；版本 1 另外接受 `noop-v1`、`objectFormat`（sha1 / sha256）、`compatObjectFormat`、`refStorage = files`、`relativeWorktrees`，其余都是 `Unsupported`。`refStorage = reftable` 在任何版本下都是 `Unsupported(Reftable)`。设计只写了「未知的 extensions.*」，这里列出了具体的规则。
- 「第 1 步补全的类型」一节列出的类型都是设计没有写定义的：`UnsupportedRepo`、`EntryLimits`、`CommitInfo`、`Signature`、`GitTime`、`BranchInfo`、`RemoteBranch`、`RemoteInfo`、`StashInfo`、`ReflogParseError`、`WorktreeFile`。`Unavailable`、`RawIdentity` 按「数据结构」的定义提前加入，因为 `WorktreeFile` 要用。
- `WorktreeFile` 多了 `NotFound`：设计写的是 `File | Symlink | TooLarge | Unavailable`，可是 `Unavailable::Missing` 只指对象库里缺对象，「工作区里没有这个文件」没有对应的变体；`project` 要据此把清单当作不存在。目录读出来是 `Unavailable(UnsupportedType)`（`read_file` 没有 index 条目可以区分）。
- `EntryLimits::entries` 限制结果的条目总数（index 自己的条目加 sparse 展开的条目），不只限制展开的部分：这样 `TrackedPaths` 的大小总有上限。默认 100 万条。`tracked` 命中缓存时也按这次传入的上限检查。
- `TrackedPaths` 增加 `is_empty` 和 `invalid_paths`：不满足 `RepoPath` 不变量的 index 路径不能进入 `iter`，与 `Totals::invalid_paths` 同样单独列出，不静默丢弃。`Repo::tree_paths` 的返回类型没有地方放它们，直接略过。
- 冲突路径在 `TrackedPaths` 里只出现一次，取 stage 0，没有时依次取 2（ours）、3（theirs）、1（base）的条目：设计没有规定取哪一个，ours 最接近 HEAD。
- `index.skipHash = true`（`feature.manyFiles` 会打开）时 git 写的校验和全为 0，以它为缓存键会让文件列表永不过期。这时 `index_checksum` 和 `TrackedPaths::checksum` 都取文件内容（去掉末尾）的哈希，也就是 git 本来会写的那个值；`index_checksum` 因此要读整个文件，仍不解析条目。`tracked` 在解析前后各算一次，不一致（文件在解析期间被替换）时重读，最多 3 次。
- `Repo::resolve` 跟随符号引用并剥离附注标签，得到最终指向的对象；`commit` 也剥离标签。`worktrees` 和 `Worktree::head` 解析 HEAD 时只跟随引用、不读对象。
- `BranchInfo::push` 按不带参数的 `git push` 实际推到哪里计算，不按 `@{push}`：`simple` 在三角工作流（推送远端不是 `branch.<名>.remote`，没有时是 `origin`）下推到同名分支，否则要求上游同名；`upstream` 在三角工作流下为 None；`current`、`matching` 推到同名分支；推送远端配置了 `remote.<名>.push` 时按这个 refspec。ADR 0020 要的是「分支会推到哪个远端分支」，`@{push}` 在三角工作流的 `simple` 下会报错。
- 配置的读取范围：生产路径读系统（`/etc/gitconfig`）、XDG 和 `~/.gitconfig`，只为找到这两个文件允许 gix 读 `HOME` 和 `XDG_CONFIG_HOME`；`GIT_*` 环境变量、`GIT_CONFIG_*` 都不生效，也不为找安装配置运行 git。测试用 `testing::discover`（只读仓库自己的配置），结果不受机器配置影响；这是 `test-support` 下新增的公开函数。
- git 目录里的管理文件集中在 `gitdir.rs` 里读，第 3 步验收里的 `rg` 检查相应地排除它、`testing.rs` 和测试代码：原来的写法会匹配这些读取，而检查的本意只针对工作区文件。
- 没有 `gitdir` 或 `HEAD` 文件的 `worktrees/<名>` 目录、名字不是 UTF-8 的目录不列出（git 也跳过前者；后者无法构成 `WorktreeId`）。`Repo::worktree` 对不存在或 prunable 的 worktree 返回 `GitError::Io(NotFound)`，打开时用这个 worktree 的 git 目录打开一次 gix 句柄来验证它，随即释放。
- `Oid::for_blob` 遇到被 sha1dc 判定为碰撞攻击的内容（git 拒绝存储它，没有正确的 OID）时，返回 SHA-256 blob OID 的前 20 字节作为替代：签名保持不报错，这个值不会等于仓库里的任何 OID，只会让缓存不命中。
- `HashKind` 增加 `byte_len`、`hex_len`（`TrackedPaths`、reflog 解析按长度取 OID）；`Worktree` 增加 `git_dir()`，与 `WorktreeInfo::git_dir` 相同，供调用方定位 worktree 自己的 git 目录。

第 2 步：

- 重命名检测完整照 git 2.43 的 `diffcore-rename` 实现，「工作区比较」第 5 步已按此改写：精确重命名之后、相似度矩阵之前，还有文件名在两边都唯一的一轮（相似度不低于 75%），它和精确重命名一样不受 `diff.renameLimit` 限制（用 git 2.43 实测：`renameLimit=1` 时同名的相似文件仍然配对）。所以「来源 × 目标超过上限的平方时只有精确重命名」只在文件名不同时成立，`rename_over_the_limit_squared_finds_only_exact_renames` 用改了文件名的 40 个文件验证。相似度用 git 自己的估算（`diffcore-delta.c` 的分块哈希），不用 imara-diff 的行 diff：分数和配对都与 git 相同，`numstat_matches_git` 逐字节对照 `--raw` 里的分数。git 的其余细节一并照做：空文件也参与精确重命名（`git diff` 默认的 `rename_empty`）；符号链接和子模块只按精确匹配，且模式必须相同；同样内容的多个来源里优先同名的、再按路径顺序；矩阵每个目标保留 4 个最好的候选。`diff.renameLimit` 从仓库配置读（默认 1000，0 或负数不限，与 git 相同），不放进 `Limits`。超过 `Limits::diff_bytes` 的文件不读，不参与相似度（git 会读）。
- 树之间的比较按新一侧那棵树里的 `.gitattributes` 判断 `binary` / `-diff` 和 `linguist-generated`，删除的路径也用它；git 在非裸仓库里用工作区的 `.gitattributes`。比较的是两个提交时，属性应当跟着被比较的版本，而不是当前检出的工作区；夹具生成时 git 的工作区正是新一侧，两者一致。只映射比较时访问到的目录里的 `.gitattributes`（改动的路径都在这些目录下），gix 从对象库读它们，不读工作区。
- 二进制判断的顺序：属性 → 大小 → NUL。没有 `binary` / `-diff` 属性、某一侧超过 16 MiB 的二进制文件是 `TooLarge`，不是 `Binary`：判断 NUL 要读内容，而对象库不能只读一个 blob 的开头。设计只写了判断条件，没有写顺序。
- `TypeChanged` 的 `Detail` 按「旧内容全部删除、新内容全部新增」计算，不做行匹配：git 把文件 ↔ 符号链接的变化显示成一次删除加一次新建，`--numstat` 也这样计数（实测）。这种 hunk 不进缓存，因为同一对 OID 作为 `Modified` 时是普通的行 diff。
- 内容相同的改动：模式也相同（精确重命名）时 `Detail` 是没有 hunk 的 `Text`，模式不同时是 `ModeOnly`；空文件的新增、删除也是没有 hunk 的 `Text`。设计没有规定精确重命名的 `Detail`。
- 树里引用的 blob 不在对象库里（部分克隆）时，这一侧的 `Content` 改为 `Unavailable(Missing)`、`Detail` 为 `NoDiff`；已经是 `Listed` 的文件不查对象，保持 `Blob`。
- patch 字节数的口径：单文件上限按 patch 正文（`@@` 行、3 行上下文、增删行、「No newline」标记）计算，`@@` 行总是按 `-a,b +c,d` 的写法计长度（git 在行数为 1 时省略 `,1`，差几个字节）；整体上限另加一个文件头的估算（两个路径各两次加 100 字节）。`Truncated` 只计文件头，`Binary`、`ModeOnly`、`TooLarge`、`NoDiff` 不计字节，但都计入文件数。达到整体上限的那个文件照常计算，之后的文件为 `Listed`。
- `Limits::entries` 在 `diff_trees` 里也限制改动路径的数量（超过时 `GitError::TooManyEntries`）：即使之后的文件都是 `Listed`，`ChangeSet` 仍保存每个改动的两侧 OID，路径数需要一个上限。
- hunk 缓存是进程内唯一的一个（16 MiB），不属于某个 `Repo`：键是内容的 OID，内容相同的文件在不同 worktree 和不同仓库之间都命中，内存预算也只有一份。键里的 `HashKind` 由 `Oid` 自带。值除了 hunk 和增删行数，还有 patch 正文的字节数，所以命中时不读 blob 内容；两侧内容的字节数后来也放进了值里，命中时不再读对象头，只检查对象是否存在（见下文「命中缓存时不读对象头」）。只缓存两侧都存在、内容都是文本的 `Modified` / `Renamed`；新增、删除和 `TypeChanged` 不查缓存（按整体增删计算，本来就不需要行 diff）。依赖表里的 `lectern-cache` 没有加入：它还没有实现，设计本来就写了「`cache` 实现之前只用进程内的 LRU」。
- 保留内容上限（`NotRetained`）在树之间的比较里不会出现：两侧都按 OID 在对象库里，改动集不保留字节。逻辑在组装这一步实现，供第 3 步的工作区内容使用：按路径顺序保留，第一个放不下的文件之后都不再保留（「之后的文件」）；与旧一侧 OID 相同的内容不需要保留（对象库里有）；`Listed` 的文件也参与。测试用内存里的新一侧直接驱动组装这一步。
- 展示行：增删配对按同一个 hunk 里的顺序一一配对（第 i 个删除行配第 i 个新增行）；两个 hunk 之间不超过 `2 × context` 行时合并上下文（与 git 合并 hunk 的规则相同），所以超过时可能折叠只有一行的区间。一侧不是 `SideText::Text`（如 `NotRetained`）时，它的行数按 hunk 和另一侧推出。没有 hunk 的 diff 是一个覆盖全文的 `Folded`。
- 词级差异：词是 ASCII 字母、数字和 `_` 的连续串，空白的连续串，其余每个字符单独一个（中文按字），按 ADR 0024 用 imara-diff 的 Histogram 比较。单行 1000 字节的上限是公开常量 `INLINE_MAX_LINE_BYTES`，不在 `Limits` 里：`inline_changes` 的签名没有 `Limits` 参数。
- `ChangeSet::texts` 的 `file` 越界时返回 `GitError::Io(InvalidInput)`，不 panic。基准一侧和对象库里的新一侧都按 `Limits::diff_bytes` 读，超过时是 `SideText::TooLarge`。
- 「第 2 步补全的类型」一节列出的类型设计没有写定义：`Limits`、`Target`、`Totals`、`FileTexts`、`SideText`、`Text`、`InlineChanges`；另外新增 `Oid::empty_tree`（根提交的基准，gix 不需要这个对象）、`Content::oid` 和 `testing::TestRepo::import_fixture`。
- 归档夹具是目录格式，不是 tar：工作区没有解包 tar 的依赖，只为测试引入一个不值得；目录里的文件也能在 PR 里直接审阅。格式和生成方法见 [夹具说明](../../crates/git/tests/fixtures/README.md)，「测试」一节已改。git 的输出用 `-z` 保存（重命名的路径不经 `{a => b}` 压缩），并另存 `--raw --no-abbrev`，把模式、OID、状态和相似度也拿来对照。`numstat_matches_git` 把子模块按 git 的方式计数（每一侧一行「Subproject commit」）。生成脚本不改 git 的配置环境变量，而是用 `-c` 显式给出影响输出的设置。
- `bench` 的 `git_diff_50_cold` / `git_diff_50_warm` 没有加入，理由与第 1 步的 `git_open` 相同；由 `#[ignore]` 的 `perf_smoke_git_diff_50` 在 release 下测量，结果见「实际验证」。它测的是整个 `diff_trees`，比只算 hunk 的基准多出读树、属性和对象头（命中时只检查对象是否存在）的时间。bench 第 1 步把它移进 `crates/bench` 的 `git/diff_50_cold`、`git/diff_50_warm`，测量的内容不变（「性能」已按此改写），`perf_smoke_git_diff_50` 随之删除。

第 3 步：

- 第 3 步最初没有 bench 的 runner（bench 第 1 步还没合并），`git/changes_*`、`git/refresh_one` 和内存先由 `#[ignore]` 的 `perf_smoke_git_changes`、`perf_smoke_git_changes_capped` 测量（结果见「实际验证」）。bench 第 1 步（[#83](https://github.com/SoloJiang/lectern-app/pull/83)）合并后，同一个 PR 把它们移进 `crates/bench`：`benches/time/git.rs` 的 `git/changes_clean`、`git/changes_50`、`git/refresh_one`、`git/refresh_full`、`git/changes_capped` 和 `benches/memory.rs` 的 `git/changes_capped_bytes`、`git/resident`，预算表里这七项改为 `Active`，`perf_smoke` 版本删除；两个目标共用的夹具在 `benches/fixtures/git_worktree.rs`。`git/refresh_full` 是设计的基准清单里没有、预算表里有的一项，一并加入。
- 重命名的目标可以是未跟踪的新增（这时 `Renamed` 的 `untracked` 为 false，`untracked` 只出现在 `Added` 上），但同一路径上有删除的未跟踪新增（`git rm --cached p`）不作为目标，不只是不与同一路径的删除配对：否则它可能与别的路径的删除配对，同一路径上就会出现 `Renamed` 和 `Deleted` 两项，破坏「同一路径最多一项」的不变量（`worktree_table_snapshot` 里 `06-removed` 与 `07-rm-cached` 原本会这样配对）。
- 「转换规则有变化」比较的是这个 `Worktree` 句柄第一次比较时的转换状态（基线），不是上一次 `refresh`：index 不会被重写，相对上一次比较就会在第二次刷新时又按 stat 跳过、改动消失。规则相对基线变了的跟踪路径每次都读取比较，规则改回来后恢复 stat 跳过；`changes` 和 `refresh(Full)` 都这样做。只有 `.gitattributes` 变化时逐个路径比较新旧规则（旧规则用基线保存的属性文件内容重建）；`info/attributes` 或影响转换的配置（`core.autocrlf`、`core.eol`、`core.safecrlf`、`core.attributesFile`、`core.fileMode`、`core.symlinks`、`filter.*`）变了时，全部跟踪路径都按规则变化处理。已知限制：`core.attributesFile`、XDG 和系统属性文件的内容变化不计入转换状态（只有 `core.attributesFile` 的值计入）；新句柄以当时的规则为基线，与 git 相同地信任 stat。
- 生效的 `.gitattributes`：跟踪的经 `SafeRoot` 读工作区里的那份，工作区里没有或不是普通文件时用 index 的 blob（skip-worktree 的条目直接用 index 的 blob），再加上目录遍历列出的未跟踪 `.gitattributes`。被忽略的目录或被忽略的未跟踪 `.gitattributes` 不生效（git 会读，已知限制）。属性栈按 OID 从内存里的叠加层取这些内容，不写对象库，也不按路径读工作区。
- stat 缓存的命中条件：stat（大小、mtime、ctime、inode、设备、模式位）、转换状态和 index 的 blob OID 都相同，并且 mtime 比开始比较的时刻早一秒以上（按整秒比较，多出的一秒覆盖内核文件时钟比系统时钟慢的那一点）。缓存项只弱引用内容：内容还被某个改动集持有时直接共享，否则重新读取，所以缓存本身不增加常驻内存；`Unhashed`、`Filtered` 不需要内容，命中时不读。
- `refresh(Paths)` 在三种情况下按 `Refresh::Full` 处理：index 的校验和变了、`Base::Head` 的 HEAD 树变了、报告的路径里有 `.gitignore` 或 `.gitattributes`。没有报告的路径直接取上一个改动集的结果（重命名配对之前的记录），然后对全部记录重新做重命名检测和组装，整体上限和保留内容按路径顺序重新计算；上一次没有保留内容、内容也不在对象库里的路径（`NotRetained`）一并重读。属性取上一次比较的转换状态。
- index 由工作区比较自己解析（要 stat、标志和各个 stage，`TrackedPaths` 不带这些），按 index 校验和缓存在 `Worktree` 里，与 `tracked` 的缓存分开。（[ADR 0035](../adr/0035.md) 之后改为只在一次调用内持有，见「缓存」，第 4 步实现。）`tracked.rs` 拆出 `parse`，两处共用校验和与重读的逻辑。
- 内置转换失败（如字节不符合声明的 `working-tree-encoding`）时用原始字节，与 git 的 diff 相同；`core.safecrlf` 的往返检查不做（git 的 diff 也只警告）。`worktree_conversions_match_git` 在 `autocrlf=false`、`true`、`input` 三个变体下与 git 2.43 `git add` 的 OID 逐个相同，包括 UTF-16LE、带 BOM 的 UTF-16、`ident`、混合换行和带 NUL 的 `text=auto`。
- 工作区一侧是 `PathThroughSymlink` 或 `Unreadable` 时模式取 index 的模式，按「无法比较」处理（`staged` 为 None），状态按两侧模式是 `Modified`。冲突路径的工作区一侧总是读取比较（没有 stage 0 的 stat），`staged` 为 None。`git add -N` 的条目：工作区有文件时是 `untracked = false` 的 `Added`（基准有这个路径时为 `Modified`），没有文件时不报告。
- 子模块：先经 `SafeRoot` 读 `<路径>/.git`。是目录时，`HEAD`、松散引用和 `packed-refs` 都经 `SafeRoot` 读；是 `gitdir:` 文件时，只接受规范化后在本仓库公共目录 `modules/` 之下的 git 目录，指向别处的为 `Unavailable(Unreadable)`，不按工作区里的文件给出的路径去读；接受的目录经以公共目录为根的 `SafeRoot` 读（`modules/<名字>/HEAD` 等），`modules/<名字>` 或其中的 `HEAD`、引用文件是符号链接时为 `Unavailable(Unreadable)`，不跟随（PR #87 第二轮审阅：`gitdir::read` 按路径读会跟随它们）。`.git` 是符号链接或读不到时为 `Unavailable(Unreadable)`，没有 `.git` 或 HEAD 是 unborn 时 W = I。子模块的哈希算法按超级项目的算。
- `core.ignoreCase` 为 true 时（macOS 的默认），未跟踪文件的遍历按 ASCII 大小写折叠查 index（与 git 的 `index_file_exists(…, icase)` 相同）：index 里是 `Readme.md`、磁盘上是 `README.md` 时不报告未跟踪的新增，目录下有没有跟踪的路径也同样折叠判断（PR #87 第二轮审阅）。折叠只用于判断是否跟踪，路径仍按 index 和磁盘上的字节。
- `Refresh::Paths` 里的目录代表它下面的全部路径：上一次比较加载过的 `.gitattributes` 在报告的目录之下，或这次在报告的目录下遍历到新的未跟踪 `.gitattributes` 时，按 `Refresh::Full` 重新比较，不沿用上一次的转换状态（PR #87 第二轮审阅）。
- 未跟踪文件的遍历（PR #87 审阅后，[ADR 0028](../adr/0028.md)）：不用 gix 的 `dirwalk`。gix 按路径遍历、按路径读工作区里的 `.gitignore`（未跟踪的 `.gitignore` 是符号链接时还跟随它），被审阅的进程在遍历期间把目录换成指向工作区之外的符号链接，忽略规则和列出的名字就会来自工作区之外。改为在 `SafeRoot::list`（platform 新增的接口）上自己遍历，`.gitignore` 只读普通文件（与 git 2.32 起相同），在内存里匹配；「工作区比较」第 4 步已改。与 gix 遍历的差别：嵌套仓库按「目录里有 `.git` 目录或文件」判定，不检查它是不是有效的仓库；被忽略的目录、被忽略的未跟踪 `.gitignore` 里的规则不生效（与 git 相同）；`core.precomposeUnicode` 开启时名字按 NFC 规范化（gix-utils）。剩余的竞态：每次 `list`、`read` 的结果对应调用期间某一时刻，两次调用之间被替换的目录在下一次调用里报告为不存在或 `PathThroughSymlink`，不会读到工作区之外（`worktree_race_directory_swapped_for_a_symlink_never_escapes`）。
- 未跟踪文件的上限按遍历顺序数，达到上限时停止遍历（`untracked_capped`），列出的再按路径排序。
- 只在 index 里的新增（`Detail::StagedOnly` 的 `Added`，如 `git mv a b; rm b`）不作为重命名的目标：配对后会成为 `StagedOnly` 的 `Renamed`，丢掉工作区里已经没有这个文件的信息。这时报告 `Deleted(a)` 和 `StagedOnly` 的 `Added(b)`（PR #87 审阅后）。
- `Base::MergeBase` 的引用不存在时返回 `GitError::Io(NotFound)`；HEAD 是 unborn 时返回 `GitError::NoMergeBase`。
- 新增 `ChangeSet::fingerprint` 和 `Fingerprint`：crates/git/AGENTS.md 规定了工作区指纹的组成，改动集里已经有它需要的全部身份（截断只改 `Detail`），在这里算最直接；Analyze 怎样使用它随 #66 设计。对象库里缺失的一侧（`Unavailable::Missing`）在组装时丢掉了 OID，指纹里只记「缺失」。
- `testing` 增加 `IndexStat`、`IndexEntry::stat`、`IndexEntry::intent_to_add` 和 `TestRepo::add`、`set_mtime`、`backdate`、`remove`、`import_worktree_fixture`（见「第 3 步补全的类型」）。工作区夹具（`tests/fixtures/worktree_conversions`）格式见 [夹具说明](../../crates/git/tests/fixtures/README.md)；它没有 `trees` 文件，`numstat_matches_git` 改为只取有 `trees` 的目录。FIFO 测试只在 Linux 上运行（用 `rustix` 建 FIFO），socket 的测试在所有平台上运行。`no_external_programs` 在 nextest 的一测试一进程模式下才把记录调用的假 `git`、`sh` 放到 PATH 最前面：`cargo test` 在一个进程里多线程运行测试，那时改环境变量不安全；仓库配置里的各种外部程序在两种方式下都指向写标记文件的脚本。
- `ConfigScope::RepositoryOnly` 只在 `test-support` 下构造，不开这个 feature 时按 `expect(dead_code)` 处理，`cargo clippy -p lectern-git --all-targets -- -D warnings` 不再失败。

命中缓存时不读对象头：

- hunk 缓存的值增加两侧内容的字节数（`Computed::sizes`），缓存格式版本改为 2，每个条目的固定开销按 208 字节计（原来 192），16 MiB 的预算不变。blob 的大小只由它的 OID 决定，所以这仍是以 blob OID 为键的派生数据（架构规则 5）。命中时 `TooLarge` 用缓存的大小按这次的 `Limits::diff_bytes` 判断，对象是否缺失用 gix 的 `has_object` 判断（松散对象查文件是否存在，pack 查索引，不解压），结果与读对象头相同；未命中时照旧读对象头。原因见「实际验证」：命中时 73% 的时间花在解压 100 个对象头上。
- 属性在查缓存之前取：有 `binary` / `-diff` 属性的文件不查缓存（原来查之前就返回 `Binary`，查缓存的次数不变），它的 `Binary` 大小照旧来自对象头。判断顺序「属性 → 大小 → NUL」不变。
- 没有缓存树之间的改动列表（按两棵树的 OID 为键）：读树、属性和打开句柄合计约 0.25 ms，在预算内，不值得再增加一份缓存和它的内存预算。

`Worktree::watch_source`（fs_watch 第 2 步）：

- 未跟踪的 `.gitignore` 由 ADR 0028 之后的 `SafeRoot` 遍历找出，不是 fs_watch 设计原文写的「同一种 gix 遍历」；`untracked::walk` 多了一个可选的输出参数，记下遍历读到的每个 `.gitignore`（路径和内容），过滤器直接用这些内容，不再读一遍。fs_watch 设计「对其他 crate 的要求」已改。
- index 里的 `.gitignore` 不是每个都读：遍历已经读过的不重复读；上级目录被忽略、是子模块或嵌套仓库的不读（git 不读，fs_watch「路径分类」第 6 步也用不到它们），事件给出的 `gitignores` 同样如此；skip-worktree 的条目在工作区里读不到时用 index 的 blob，与 git 的 `read_skip_worktree_file_from_index` 相同。设计只写了「index 里每个名为 `.gitignore` 的路径在工作区里的文件」。
- 配置清单里的来源文件按 gix 记录的路径给出，可能含 `..`（linked worktree 读到的公共 `config` 记为 `worktrees/<名字>/../../config`）：fs_watch 展开清单时逐级处理 `..`，这里不做字面归并，字面归并在符号链接之下会得到错误的位置。
- 相对路径的 `core.excludesFile`、`core.attributesFile` 不列出：gix 按进程的当前目录解析它们（不是工作区），fs_watch 也只接受绝对路径。`:(optional)` 的值指向不存在的文件时 gix 改读 XDG 的默认位置，所以两者都列出。
- `include` / `includeIf` 的节名按不分大小写匹配（git 不分）；gix 只跟随小写的写法，大写写法的目标会被列出而 gix 不读，只多一个监视点。
- 求 `~` 和 XDG 的默认位置要知道 `HOME`、`XDG_CONFIG_HOME`：经 gix-path 读，与 gix 打开仓库时（`ConfigScope::User` 的权限）读的是同两个变量，`GIT_*` 仍然不读。`ConfigScope::RepositoryOnly` 不读环境变量，也不列出仓库之外的标准位置，与 gix 在这个范围里实际读的文件一致。
- `ConfigScope::RepositoryOnly`（`testing::discover`、`testing::open_handle`）改为跟随仓库配置里的 `include.path` / `includeIf`：配置清单的测试要 gix 按条件加载引入的文件（`onbranch:`）。它仍然不读系统、XDG、`~/.gitconfig` 和环境变量，测试结果不受机器上的配置影响；引入的文件由测试自己写在临时目录里。
- `WorktreeName` 与 `WorktreeId` 的转换是两个 `From` 实现；`TrackedPaths` 增加 crate 内部的 `entries()`（切片，供过滤器二分查找）。

`Worktree::canonical_oid`（前置条件 E）：

- 签名按 code_intel 设计，路径参数是 `&RepoPath`，返回本 crate 的 `Oid`。
- 未跟踪的 `.gitattributes` 不看忽略规则，与 git 相同、与工作区比较不同（见「规范内容的 OID」）：单个路径只查各级目录，没有遍历可用；为它另建忽略栈只为了复现工作区比较的已知限制，不值得。
- 实现放在 `compare.rs`（`compare::canonical_oid`），与工作区比较共用 index 快照、`read_attr_file` 和 `WorktreeAttrs`，`conversion.rs` 不变。

字节偏移（ADR 0034）：

- 展示行的偏移是 `Option<u64>`，`Hunk` 的是 `u64`。一侧不是 `SideText::Text` 时（`TooLarge`、`NotRetained`、`Unavailable`），这一侧的 hunk 内后续行和最后一个 hunk 之后的行无法从 hunk 和另一侧推出（需要这一侧每行的长度），所以这一侧的偏移一律为 None；`Absent` 一侧是空内容，偏移为 0。本 crate 自己的改动集里 `Detail::Text` 的两侧总是 `Text` 或 `Absent`（`NotRetained` 的文件改为 `Truncated`），None 只出现在对象在 `texts` 之前消失、或调用方用别的文本计算展示行时。
- 新增 `Text::line_start`：展示行的偏移和词级区间在文本里的位置都由它给出，视图也用它把词级区间换算到文本里。
- 设计写的「缓存的命名空间版本加一」：`cache` 的 `git.diff` 命名空间还没有接入，现在的版本是键里的格式版本 `CACHE_FORMAT`，从 2 改为 3；接入 `cache` 时它就是命名空间的版本。
- 偏移的口径（规范内容）设计没有写，按「展示行」的「字节偏移」一段决定：与 hunk 缓存的键（规范内容的 blob OID）和 `texts` 的文本一致。
