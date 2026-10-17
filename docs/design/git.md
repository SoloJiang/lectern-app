# git 设计

状态：已接受，尚未实现。

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
| `gix` =0.88.0 | 对象库、引用、index、配置、属性与忽略规则、内置转换（`text` / `eol` / `ident` / `working-tree-encoding`）、目录遍历、树 diff 与重命名检测 | 纯 Rust，MIT OR Apache-2.0；feature 与使用边界见 [ADR 0024](../adr/0024.md)；release 约 3 MB，`just deny` 通过 |
| `gix::diff::blob`（`gix-imara-diff` 0.3） | 行 diff 与词级 diff，Histogram 算法 | gix 维护的 imara-diff 副本，Apache-2.0；随 gix 一起升级，不再引入上游 `imara-diff`（ADR 0024） |
| `lectern-platform` | 安全读取工作区文件（见「安全读取」） | 架构规则 6 |
| `lectern-cache` | hunk 的持久缓存 | 架构规则 5；`cache` 实现之前只用进程内的 LRU |
| `insta`（dev） | 改动集、hunk、展示行的快照 | 工作区统一依赖 |
| `gix`（dev，额外开写入用的 feature） | 测试里构造仓库（见「测试」） | 只影响测试构建，发布构建的 feature 不变 |

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
    pub fn read_file(&self, path: &RepoPath, max: u64) -> Result<WorktreeFile, GitError>;   // File | Symlink | TooLarge | Unavailable
    /// 只读 index 文件末尾的校验和，不解析条目、不展开 sparse 目录；与 `TrackedPaths::checksum` 相同。
    /// 供调用方在不重建文件列表的情况下检查缓存是否过期（`project` 对非当前 worktree 的缓存检查）。没有 index 文件时为 None。
    pub fn index_checksum(&self) -> Result<Option<Oid>, GitError>;
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

`fs_watch` 是基础设施层，不依赖本 crate：`app` 把 `TrackedPaths` 包成 `fs_watch` 定义的过滤接口传进去。

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

pub enum Row {
    Context { old: u32, new: u32 },
    /// `pair`：同一个 hunk 里与它配对的新行，双栏视图把两者放在同一行。
    Removed { old: u32, pair: Option<u32> },
    Added { new: u32, pair: Option<u32> },
    /// 折叠的未改动行；视图展开时用同样的行号区间取文本。
    Folded { old: Range<u32>, new: Range<u32> },
}

/// 一对配对行的词级差异：两侧改动部分的字节区间。任一行超过 1000 字节时返回 None，视图整行高亮。纯计算。
pub fn inline_changes(old: &[u8], new: &[u8]) -> Option<InlineChanges>;
```

行号是两侧完整文件里的行号（从 0 起）。所以高亮按完整文件计算再按行切片，缓存键是 blob OID、语言和 grammar 与查询的版本（[pulls.review 调研](../research/pulls-review.md#对-lectern-的建议)、[#42](https://github.com/SoloJiang/lectern-app/issues/42)），由做高亮的 crate 实现。

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

pub struct Hunk { pub old: Range<u32>, pub new: Range<u32> }
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
4. **未跟踪文件**：用 gix 的目录遍历按 `.gitignore`、`info/exclude`、`core.excludesFile` 枚举，不进入被忽略的目录，逐个列出 index 里没有的文件（相当于 `-uall`）。gix 的目录遍历只把带 `UPTODATE` 标志的 index 条目当作已跟踪，所以遍历之前，在内存里的 index 副本上给工作区里存在的条目设上这个标志（不写回用户的 index）；遍历结果里仍出现的 index 路径一律丢弃，是否跟踪只按 index 判断。列出的每一项都是 `Added` 且 `untracked = true`，内容按第 1、3 步读取；FIFO、socket、设备文件的新一侧是 `Mode::Special` + `UnsupportedType`，未跟踪的目录里有 `.git` 时只列出这个目录，新一侧是 `Mode::Special` + `NestedRepository`。
5. **重命名**：删除与新增之间先按 OID 找精确重命名，精确重命名不受任何上限限制，总是检测。剩下的删除（来源）与新增（目标）再按相似度（不低于 50%）找重命名，上限规则与 git 的 `too_many_rename_candidates` 相同：来源数 × 目标数超过 `diff.renameLimit` 的平方时跳过相似度检测（默认 1000，即 100 万对；40 × 40 的重构照常检测）。同一路径上的删除和未跟踪新增不互相配对。`staged` 为 Some 的删除（index 里还留着来源路径，如未暂存的 `mv a b`）不作为来源，否则配对后 `staged` 只能描述新路径、丢掉来源路径在 index 里的状态；这时与 `git status` 一样，报告为 `Deleted` 和未跟踪的 `Added`。工作区一侧的内容只放进 gix 的内存对象存储参与计算（ADR 0024）。

其余规则：

- 模式：`core.fileMode=false` 时可执行位取 index 里的值；符号链接的内容是 `readlinkat` 读出的目标。
- 子模块：工作区一侧的提交是子模块自己的 HEAD（只读地打开它的 git 目录）；子模块内部的未提交改动不计算。子模块没有初始化（克隆时没带 `--recurse-submodules`，或 `deinit` 之后：路径是空目录，没有仓库和 HEAD）时，W = I（index 里的 gitlink OID），不标为 `Unavailable`。
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

二进制判断与 git 相同：属性 `binary` 或 `-diff`，或任一侧前 8000 字节里有 NUL。上限都在 `Limits` 里，调用方可以调整；默认值写进 `bench` 的预算表。

### 缓存

| 数据 | 键 | 存放 |
|---|---|---|
| hunk（`FileDiff`） | 格式版本、`HashKind`、旧 OID、新 OID、算法（Histogram）、空白处理（P1 只有「不忽略」） | 进程内字节 LRU 16 MiB；`cache` 实现后写入它的 SQLite（命名空间 `git.diff`），首帧之后才打开 |
| 工作区文件的 OID | 路径 + stat | `Worktree` 内的内存缓存，不持久化 |
| 改动集、展示行、词级差异 | 不缓存 | 由缓存的 hunk 重算，耗时与可见行数成正比 |

hunk 只存行号区间，不存文本：文本随时可以按 OID 从对象库取出。两侧都是 `Blob` 且是文本时才进缓存。上下文行数不在键里，因为展示行在 hunk 之上计算。

## 数据流与线程

1. 首帧之后，`project` 在后台任务里调用 `Repo::discover`、`worktrees`、`tracked`、`read_file`，建立仓库 → worktree → 子项目的模型，派生数据以 `TrackedPaths::checksum` 和 `Oid::for_blob` 算出的 OID 为键；检查已有缓存是否过期时调 `Worktree::index_checksum`，不重建文件列表。
2. `review` 在后台任务里调用 `changes(Base::Head)` 或 `changes(Base::Commit(会话开始时的 HEAD))`，得到 `Arc<ChangeSet>`，加上归因后作为不可变快照交给 `review_view`（架构规则 4）。
3. `fs_watch` 报告路径变化时，`review` 调用 `refresh(Paths)`；HEAD、index、忽略规则、属性或配置变化时调用 `refresh(Full)`。
4. 视图滚动到某个文件时发出命令，后台任务调用 `texts` 和 `rows`，把展示行交给视图；视图不调用本 crate 的任何函数。
5. Analyze 快照的数据流随 [#66](https://github.com/SoloJiang/lectern-app/issues/66) 设计。

线程：本 crate 的函数在调用方的后台任务线程上运行，不建线程，不持有跨调用的锁。gix 的 `parallel` 关闭（ADR 0024），它的仓库句柄不是 `Send`，所以 `Repo`、`Worktree` 只保存位置、打开参数和本 crate 自己的数据（index 解析结果、stat 缓存），不持有 gix 句柄。gix 句柄不在调用之间保留：每次顶层调用（`changes`、`refresh`、`texts`、`diff_trees`、引用与历史的查询等）在当前线程上打开一个，调用内部复用，返回时释放。worktree 级的调用按这个 worktree 的私有 git 目录（`WorktreeInfo::git_dir`）打开，仓库级的调用按公共目录打开，打开参数（权限、对象格式）固定在 `Repo` 里。所以空闲时内存里没有 gix 句柄和它的 pack 缓存。非当前 worktree 只保留 `WorktreeInfo`，`Worktree` 句柄（index、stat 缓存）由调用方在离开后释放（[技术调研「性能要点」](tech-research.md#性能要点)）。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)、[ADR 0017](../adr/0017.md)）：

| 预算 | 本 crate 的份额（M 系列 Mac，`cloudprint-miniapps` 量级：728 个跟踪文件、11 个 worktree） |
|---|---|
| 每次顶层调用 | 打开 gix 句柄 < 1 ms（读配置和引用，不读 pack），计入下面各项 |
| 打开项目到项目主页可用 < 100 ms | `discover` + `worktrees` + `tracked` < 15 ms |
| 打开一个会话的 Diff < 50 ms | 50 个改动文件的 `changes`（stat 跳过其余文件）< 15 ms；hunk 未命中时计算 < 20 ms，命中时 < 2 ms |
| 切换 worktree / 分支 < 100 ms | 内容相同的文件命中同一份 hunk 缓存；`refresh(Full)` < 15 ms |
| 空闲内存：打开 monorepo < 100 MB | 空闲时不持有 gix 句柄；调用期间每个句柄的 pack 与对象缓存 ≤ 4 MiB；hunk LRU 16 MiB；每个 `ChangeSet` 保留的工作区内容 ≤ 32 MiB（通常只有当前审阅的一个） |
| 冷启动 20 ms | 首帧之前不调用本 crate 的任何函数 |
| 安装包 ≤ 80 MB | gix 约 3 MB（ADR 0024） |

刷新是增量的：一次 `refresh(Paths)` 只 stat 和读取报告的路径。大仓库（10 万个跟踪文件）的首次全量比较按 stat 数量线性增长，在后台进行，项目主页先显示会话列表，改动数随后填入。

`bench` 增加的基准（[#7](https://github.com/SoloJiang/lectern-app/issues/7)），夹具由本 crate 的测试仓库构造器生成：

- `git_open`：`discover` + `worktrees`（11 个 worktree）+ `tracked`。
- `git_open_handle`：打开一次 gix 句柄的耗时（每次顶层调用都要付出）。
- `git_changes_clean` / `git_changes_50`：没有改动、50 个文件改动时的 `changes`。
- `git_refresh_one`：改动一个文件后的 `refresh(Paths)`。
- `git_diff_50_cold` / `git_diff_50_warm`：50 个文件、平均 300 行的 hunk 计算，缓存未命中与命中。
- `git_changes_capped`：10 000 个改动文件时整体截断生效，耗时 < 1 s，`ChangeSet` 的内存不超过上限之和。
- 内存：打开 11 个 worktree 的仓库并对当前 worktree 做一次 `changes` 后，本 crate 的常驻增量。

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
- **归档夹具**：需要 git 自己生成的格式放在 `tests/fixtures/<名字>.tar`，由同名的 `tests/fixtures/scripts/<名字>.sh` 用真实的 git 生成，脚本头部记录 git 版本；同时保存 `git diff --no-ext-diff --no-textconv -M --histogram --numstat` 等命令的输出作为对照。CI 只解包，不运行 git；git 升级导致格式变化时新增一份样本，不覆盖旧的。包括：linked worktree（锁定、prunable）、SHA-256 仓库、packed-refs、index v4 与 split index、sparse index（`index.sparse` 加 cone 模式的 sparse checkout）、`core.autocrlf` 与 `eol`、UTF-16 的 `working-tree-encoding`、`ident`、Git LFS 属性与过滤器配置、子模块、未解决的冲突、reftable 仓库、各种 reflog 消息。

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
- 不执行外部程序：夹具仓库配置 `diff.external`、`diff.<名>.textconv`、`filter.<名>.clean` / `process`、`core.fsmonitor`，指向会写标记文件的脚本，PATH 最前面放一个记录调用的假 `git`（AGENTS.md「工作方式」）；跑完全部工作区和树 diff 场景后，标记文件不存在、假 `git` 没有被调用。
- 展示行与词级差异：配对、折叠、hunk 相邻时合并上下文，用 insta 快照。
- 缓存：同一对 OID 第二次计算命中；键里的 `HashKind` 不同时不命中。
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
   - `bench` 的 `git_open` 在预算内。
2. **树之间的改动集、hunk 与展示行**：
   - `diff_trees_*`、`rename_*`、`binary_*`、`truncate_*`、`rows_*`、`inline_*`、`hunk_cache_*` 测试通过，结构化输出有 insta 快照；`numstat_matches_git` 对全部归档夹具通过。
   - `git_diff_50_cold` / `git_diff_50_warm` 在预算内。
3. **工作区比较**：
   - `worktree_*`、`read_only_*`、`no_external_programs` 测试通过。
   - `rg 'std::fs::(File::open|read|metadata|symlink_metadata)' crates/git/src` 没有结果：工作区只经 `SafeRoot` 读取。
   - `git_changes_clean`、`git_changes_50`、`git_refresh_one`、`git_changes_capped` 和内存基准在预算内。
4. **改动来源接口**：`services` 的设计文档合并后，为本地仓库实现它的改动来源接口（取 diff、加载完整文件；指纹随 #66），按那份文档的验收条件测试。由哪个插件把它注册为服务按 #47 的结论处理，不在本 crate。
