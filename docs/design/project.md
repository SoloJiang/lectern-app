# project 设计

状态：已接受，尚未实现。

对应 issue [#9](https://github.com/SoloJiang/lectern-app/issues/9)。`project` 把用户打开的路径解析成项目模型：仓库 → worktree → 子项目（[ADR 0006](../adr/0006.md)、[技术调研「Monorepo 与多 worktree 支持」](tech-research.md#monorepo-与多-worktree-支持)），以不可变快照交给视图和其他插件，并通过 `KernelControl::workspace_changed` 提交 `WorkspaceFacts`，驱动 `Activation::Workspace` 的插件（[kernel「依赖与自动启停」](kernel.md#依赖与自动启停)）。

## 范围

- 做：
  - 打开路径：解析成 git 项目（按 git 公共目录识别）或普通文件夹项目；同一仓库的所有 worktree 是同一个项目。
  - worktree 列表：主 worktree 与 linked worktree 的根目录、HEAD、locked、prunable。
  - 当前 worktree 的文件列表：读 git index，不遍历目录；普通文件夹做有上限的遍历。
  - 子项目识别：读清单（npm / yarn / pnpm workspaces、Cargo workspace、Maven modules、Gradle settings、go.work），不在声明里但有自己清单的目录也算子项目。不运行任何构建工具。
  - 路径归位：把会话 cwd、改动路径映射到「项目 + worktree + 子项目」。
  - `WorkspaceFacts`：当前打开项目的文件，供 kernel 判断激活条件。
  - 监听 index、HEAD、清单和 worktree 增减，增量刷新快照。
  - `project` 插件：提供 `dyn Projects` 服务。
- 不做：
  - 读 git 对象、index 和 worktree 元数据的实现：归 `git`，本文只列需要的接口（见「依赖 git 的接口」）。
  - 改动量、diff：归 `git` 与 `review`。
  - 项目的长期标识（Lectern 分配的固定 ID、移动后的别名、临时项目、手动合并）：归 `usage`（[ADR 0020](../adr/0020.md)，见「项目标识」）。
  - 会话发现：归 `agents`，它调用本 crate 的 `locate` / `resolve` 归位。
  - 按文件类型与清单匹配插件市场的激活条件（[ADR 0018](../adr/0018.md)）：归 `marketplace`，它读同一份 `WorkspaceFacts`。
  - 依赖坐标、Gradle 版本目录、tsconfig `paths`、跨包跳转：归 `code_intel`（P2，[ADR 0007](../adr/0007.md) 的第 2、3 步）。
  - 最近项目列表的保存：归 `welcome` / `app`。
- 本 issue 之外：
  - 插件提供的子项目识别（`ProjectDetector`）：P1 只有内置解析；`services` 定义这个接口后（[#8](https://github.com/SoloJiang/lectern-app/issues/8)），本 crate 合并它的结果。
  - 进程插件读取项目模型的协议（[#39](https://github.com/SoloJiang/lectern-app/issues/39)）。
  - 普通文件夹的实时刷新：P1 只在打开时扫描一次。
  - 多窗口按项目分别判断激活条件：kernel 届时修订，P1 提交所有打开项目的并集（见「WorkspaceFacts」）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-kernel` | `Plugin`、`KernelControl::workspace_changed`、事件、`BusyGuard` | 插件机制 |
| `lectern-services` | `Projects` 接口、快照与事件类型定义在这里（见「接口」） | 消费方（`project_home`、`worktrees`、`agents`、`usage`、`review`）只依赖接口（架构规则 7） |
| `lectern-git` | 发现仓库、列 worktree、读 index 路径、读工作区文件、算 blob OID | gix 只在 `git` 里用；接口见下文 |
| `lectern-fs-watch` | worktree 与 git 元数据的变化通知 | 接口见下文；实例由 `app` 创建，经 `ProjectDeps` 传入 |
| `lectern-executor` | 后台任务 | UI 线程不做 IO；实例由 `app` 创建，经 `ProjectDeps` 传入 |
| `serde` 1 + `serde_json` 1 | package.json | MIT / Apache-2.0；只反序列化需要的字段 |
| `toml` 0.9（`parse`、`serde`） | Cargo.toml | MIT / Apache-2.0 |
| `quick-xml` 0.38 | pom.xml | MIT；流式读取，只依赖 `memchr` |
| `glob` 0.3 | 工作区成员与 `WorkspaceFacts` 的模式匹配 | MIT / Apache-2.0，无传递依赖；Cargo 用它解析 `members` |

选型与放弃的方案见 [ADR 0023](../adr/0023.md)。pnpm-workspace.yaml、settings.gradle(.kts)、go.work、go.mod 用手写的子集解析。版本取实现时 crates.io 上的最新兼容版本，由 Cargo.lock 锁定。

- 不用 `cache`：解析一个清单是微秒级，比 SQLite 查询还快，持久化没有收益。解析结果在内存里以 blob OID 为键缓存（见「数据流与线程」）。
- 本 crate 是插件 crate（`Cargo.toml` 的 `[package.metadata.lectern]` 写 `plugin = true`，[ADR 0025](../adr/0025.md)）：可以依赖 kernel 层的 crate 和不是插件的库 crate（`lectern-git`、`lectern-executor`、`lectern-fs-watch`，以及以后需要时的 `lectern-cache`），不依赖任何其他插件 crate；只有 `app` 依赖本 crate。其他插件读项目模型只经 `services` 里的 `Projects`。

## 接口

### 服务接口（`lectern-services` 的 `project` 模块）

接口和数据类型放在 `services`，由本 crate 实现。类型都是不含行为的数据，能通过 JSON-RPC 序列化（serde 派生方式随 `services` 设计）；`services` 的设计文档引用本节，不重复定义。

```rust
/// 单实现服务，`project` 插件提供。
/// impl Service for dyn Projects { const ID = "projects"; const MULTIPLE = false; }
pub trait Projects: Send + Sync {
    /// 打开一个路径。立即返回，任何线程可调用；结果以 `ProjectEvent::Opened` 送达。
    /// 路径所在仓库已经打开时返回同一个 `ProjectKey`，并把当前 worktree 切到路径所在的 worktree。
    fn open(&self, path: &Path) -> OpenRequest;

    /// 关闭项目：取消它的后台任务、停止监听、丢弃快照，送出 `ProjectEvent::Closed`。立即返回。
    fn close(&self, project: &ProjectKey);

    /// 切换当前 worktree。立即返回；新 worktree 的文件和子项目就绪后送出 `Changed`。
    /// worktree 不属于这个项目或已 prunable 时忽略。
    fn set_active_worktree(&self, project: &ProjectKey, worktree: &WorktreeKey);

    /// 当前快照。只取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程上可以调用。
    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>>;

    /// 打开中的项目，按打开顺序。不阻塞。
    fn open_projects(&self) -> Vec<ProjectKey>;

    /// 在打开中的项目里归位一个路径：只做前缀比较，不做 IO，任何线程可调用。
    /// 几个打开的项目都包含这个路径时（父仓库与其中单独打开的子模块或嵌套仓库），返回匹配的 worktree 根或文件夹根最深的那个。
    /// `path` 必须已是规范路径（`resolve` 的结果或 `std::fs::canonicalize`）；不在任何打开项目里时返回 None。
    fn locate(&self, path: &Path) -> Option<Location>;

    /// 解析任意路径属于哪个项目，不要求项目已打开：规范化路径、向上发现仓库。
    /// 阻塞（几次 stat 与读取 `.git` 文件），只在 executor 上调用。调用方（`agents`、`usage`）对相同 cwd 去重。
    fn resolve(&self, path: &Path) -> Result<Resolved, ResolveError>;

    /// 在指定 git worktree 里找包含 `rel` 的最深子项目，用于 `locate` 对非当前 worktree 返回 `NotLoaded` 的情况。
    /// 阻塞，只在 executor 上调用：读这个 worktree 的 index 与清单（复用以 blob OID 为键的解析缓存），
    /// 算出它的子项目列表后丢弃文件列表，只缓存子项目列表（见「数据流与线程」）。返回值只有 `Found` 或 `None`，
    /// 只有一种例外：是当前 worktree 时直接用快照里的结果、不做 IO，子项目还是 `Pending` 就返回 `NotLoaded`，
    /// 调用方等下一次 `ProjectEvent::Changed` 后重新调 `locate`。
    /// 文件夹项目只有一棵树、没有 `WorktreeKey`，只用 `locate`：子项目还是 `Pending` 时同样等 `Changed`。
    /// 项目未打开或 worktree 不属于它时返回 `Err(ScanError::RootMissing)`。
    fn resolve_package(&self, project: &ProjectKey, worktree: &WorktreeKey, rel: &str) -> Result<PackageMatch, ScanError>;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct OpenRequest(pub u64);

pub enum OpenError {
    NotFound,
    NotADirectory,
    PermissionDenied,
    /// 有 `.git` 但读不出来（gitdir 指向不存在的目录、公共目录损坏）。附 `git` 给出的原因，日志用。
    RepoUnreadable(Arc<str>),
    Io(Arc<str>),
}

pub type ResolveError = OpenError;
```

调用方：`welcome` 和 `workspace` 调 `open`（⌘O、最近项目、恢复窗口快照，都在首帧之后）；`project_home`、`worktrees` 在渲染时调 `snapshot`；`agents` 在 executor 上调 `resolve`，对已打开项目调 `locate`，`locate` 给出 `PackageMatch::NotLoaded` 时再调 `resolve_package`；`usage` 写观察记录时用 `ProjectKey`。

### 事件

```rust
/// 在 executor 线程上、快照替换之后发出。监听只更新状态或安排重绘，不做 IO（kernel「事件分发」）。
pub enum ProjectEvent {
    Opened { request: OpenRequest, result: Result<ProjectKey, OpenError> },
    Changed { project: ProjectKey, generation: u64, parts: ChangedParts },
    Closed { project: ProjectKey },
}
impl Event for ProjectEvent {}

#[derive(Clone, Copy, Default, Debug)]
pub struct ChangedParts {
    pub worktrees: bool, // 增减、HEAD、locked、prunable
    pub active: bool,    // 当前 worktree 变了
    pub files: bool,
    pub packages: bool,
}
```

成功的 `Opened` 之后至少还有一次 `Changed`（文件列表就绪或不可用）。视图收到 `Changed` 后调 `snapshot` 取最新快照；事件里的 `generation` 只用来丢弃过时的通知。

### 本 crate 的公开项

```rust
/// `app` 装配时构造，放进 `PluginSet`（id "project"）。
pub struct ProjectPlugin { /* deps */ }

pub struct ProjectDeps {
    /// 由 app 创建并传入，类型随 executor 设计。
    pub executor: /* executor 句柄 */,
    /// 由 app 创建并传入（FSEvents 后端由 platform 实现），类型随 fs_watch 设计。
    pub watcher: /* fs_watch 的监听接口 */,
    /// 用户主目录，判断「主目录被当作 dotfiles 仓库」（见「项目标识」）。由 app 从 platform 取得。
    pub home: PathBuf,
}

impl ProjectPlugin {
    pub fn new(deps: ProjectDeps) -> Self;
}

// Manifest { provides: [service::<dyn Projects>()], requires: [], activation: [AfterFirstFrame] }
// activate：建空的项目表，provide 服务；不做 IO。started：不提交任何任务，等第一次 open。

/// 纯函数，供测试和 bench 直接调用；不做 IO，任何线程。
pub mod manifest {
    /// 一个清单的解析结果：名字、成员声明（glob、排除项或显式路径，按「清单格式」表）、是否 partial。
    pub struct Parsed { /* name, members: Vec<Member>, excludes, partial */ }
    pub fn parse(kind: ManifestKind, bytes: &[u8]) -> Result<Parsed, ManifestError>;
}
/// `manifest(path)`：None 表示工作区里没有这个文件（已删除，不算清单）；Some(Err) 是读取或解析失败，
/// 子项目照常列出并把错误填进 `Manifest.error`。
/// `root_name`：项目显示名（`ProjectSnapshot::name`），根目录的子项目没有清单名字时（如 Cargo 虚拟清单）用它。
pub fn resolve_packages(
    files: &FileList,
    root_name: &str,
    manifest: &dyn Fn(&str) -> Option<Result<Arc<Parsed>, ManifestError>>,
) -> Vec<Package>;
pub fn workspace_facts(trees: Vec<Arc<FileList>>) -> Arc<dyn WorkspaceFacts>;

/// `test-support` feature：内存里的假 git（见「测试」）和合成仓库的形状。
#[cfg(feature = "test-support")]
pub mod testing;
```

### 依赖 git 的接口

本 crate 只通过一个内部 trait 访问 git，生产实现转调 `lectern-git`，测试用内存实现。具体签名以 git 的设计文档为准；下表是 project 需要的最小集合，都是阻塞调用，只在 executor 上执行：

```rust
pub(crate) trait GitAccess: Send + Sync {
    /// 从 path 向上发现仓库，不跨越 GIT_CEILING_DIRECTORIES。不是仓库时返回 Ok(None)。
    fn discover(&self, path: &Path) -> Result<Option<Discovered>, GitError>;
    /// 公共目录下的全部 worktree：主 worktree（裸仓库没有）与 `<common>/worktrees/*`。
    fn worktrees(&self, common_dir: &Path) -> Result<Vec<WorktreeInfo>, GitError>;
    /// 一个 worktree 的 index 条目：路径（字节）与文件模式，按 index 顺序；附 index 的校验和，未变时跳过重建。
    fn index_paths(&self, git_dir: &Path) -> Result<IndexListing, GitError>;
    /// 只读 index 的校验和（文件末尾），判断 `resolve_package` 的缓存是否过期。
    fn index_checksum(&self, git_dir: &Path) -> Result<ObjectId, GitError>;
    /// 从 worktree 根目录安全地读一个工作区文件（不跟随符号链接、不越出 worktree，git 的同一套读取接口）；
    /// 不存在时 Ok(None)，超过 limit 时 Err(TooLarge)。
    fn read_worktree_file(&self, root: &Path, rel: &str, limit: u64) -> Result<Option<Vec<u8>>, GitError>;
    /// 按仓库的哈希算法计算 blob OID（与 `git hash-object` 相同），作为解析缓存的键。
    fn hash_blob(&self, kind: HashKind, bytes: &[u8]) -> ObjectId;
}

pub(crate) struct Discovered { worktree_root: Option<PathBuf>, git_dir: PathBuf, common_dir: PathBuf, bare: bool, hash: HashKind }
pub(crate) struct WorktreeInfo { root: PathBuf, git_dir: PathBuf, is_main: bool, head: Head, locked: bool, prunable: bool }
```

另需 `git` 的 `test-support` 提供不调用 git 命令行的仓库构造器：写对象、index 和 linked worktree 的管理目录，用于集成测试和 bench。

### 依赖 fs_watch 的接口

每个打开的 git 项目一份订阅，具体签名以 fs_watch 的设计文档为准：

- 当前 worktree：按 index 过滤后的变更路径批次，以及这一批里 `index`、`HEAD` 是否变化（fs_watch 的 worktree 修订号已覆盖这两个文件）。
- 其他 worktree：只要 `HEAD` 变化的通知，不监听工作区文件（非活动 worktree 只留元数据，[ADR 0017](../adr/0017.md)）。
- `<公共目录>/worktrees/` 下条目的增减（`git worktree add / remove / prune`）。

## 数据结构

### 项目标识

```rust
/// 项目在本次运行中的键，也是 `usage` 观察记录里的仓库位置、`cache` 的分库依据。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum ProjectKey {
    /// git 公共目录的规范绝对路径。同一仓库的所有 worktree 得到同一个键。
    Git { common_dir: Arc<Path> },
    /// 普通文件夹：打开的根目录的规范绝对路径。
    Folder { root: Arc<Path> },
}

/// worktree 的 git 目录的规范绝对路径：主 worktree 是公共目录，linked worktree 是 `<公共目录>/worktrees/<id>`。
/// 不用根目录做键：prunable 的 worktree 根目录已不存在，无法规范化，而管理目录还在。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct WorktreeKey(pub Arc<Path>);
```

规则：

- 规范路径用 `std::fs::canonicalize`（解析符号链接，如 `/tmp` → `/private/tmp`）。
- 打开路径在某个 worktree 内部（包括子目录、`.git` 目录本身）时，项目是整个仓库，当前 worktree 是路径所在的那个，`opened_at` 记下路径相对 worktree 根的位置。
- 打开的是裸仓库或公共目录本身时，项目是这个仓库，当前 worktree 取第一个未 prunable 的 linked worktree；没有时没有当前 worktree，文件和子项目为空。
- 主目录被当作仓库的情况（dotfiles 仓库的主 worktree 根就是 `$HOME`）：发现的仓库主 worktree 根等于 `home`、而打开的路径不是 `home` 本身时，按普通文件夹打开这个路径。否则打开 `~/code/x` 这类没有 `.git` 的目录会变成打开整个主目录仓库。
- 子模块是独立的仓库：在子模块里打开得到子模块自己的项目；在父仓库里，子模块是一条 gitlink 文件条目，不展开、不算子项目。

只到 `ProjectKey` 为止。ADR 0020 要求的固定标识、打开已知路径时重新比对根提交与远端、仓库移动后的别名、临时项目和在 Usage 页手动合并，都由 `usage` 实现并持久化（`crates/usage/AGENTS.md`）：它用 `ProjectKey` 作为位置，需要的根提交与远端直接向 `git` 读取。P1 的 `project` 不持久化任何东西；仓库被移动后，下次打开得到新的 `ProjectKey`，与旧记录的关联由 `usage` 判断（P2 的 Usage 页提供合并界面）。

### 快照

```rust
/// 不可变。每次变化整体替换为新的 `Arc`，视图持有旧快照不受影响。
pub struct ProjectSnapshot {
    pub key: ProjectKey,
    /// 每次替换加一，严格递增；与 `ProjectEvent::Changed` 的 generation 一致。
    pub generation: u64,
    /// 显示名：主 worktree 根的目录名；裸仓库为公共目录名去掉 `.git` 后缀；文件夹为目录名。
    pub name: Arc<str>,
    /// git 项目的全部 worktree：主 worktree 在前，其余按根路径排序。文件夹项目为空。
    pub worktrees: Arc<[Worktree]>,
    pub active: Option<ActiveTree>,
}

pub struct Worktree {
    /// 即 git 目录，见 `WorktreeKey`。
    pub key: WorktreeKey,
    /// 根目录。存在时是规范路径；prunable 时取管理目录里 `gitdir` 文件记录的路径去掉末尾的 `/.git`，只做字面归并。
    pub root: Arc<Path>,
    pub is_main: bool,
    pub head: Head,
    pub locked: bool,
    /// 管理目录还在、根目录已不存在。列出但不扫描，不能设为当前 worktree。
    pub prunable: bool,
}

pub enum Head {
    Branch(Arc<str>),   // 完整引用名，如 "refs/heads/main"
    Detached(Arc<str>), // 提交 OID 的十六进制，长度随哈希算法
    Unborn(Arc<str>),   // 还没有提交的分支
    Unknown,            // HEAD 读不出来
}

/// 当前 worktree（文件夹项目为根目录）的文件与子项目。只有它保留文件列表。
pub struct ActiveTree {
    pub root: Arc<Path>,
    pub worktree: Option<WorktreeKey>, // 文件夹项目为 None
    pub opened_at: RelPath,            // 打开的路径相对 root；直接打开根时为 ""
    pub files: Scan<Arc<FileList>>,
    pub packages: Scan<Arc<[Package]>>,
}

pub enum Scan<T> {
    Pending,
    Ready(T),
    Unavailable(ScanError),
}

pub enum ScanError {
    IndexUnreadable(Arc<str>), // 附 git 给出的原因
    RootMissing,
    Io(Arc<str>),
}

/// worktree 内的相对路径，`/` 分隔，不以 `/` 开头或结尾；根为 ""。
pub type RelPath = Arc<str>;

pub struct Location {
    pub project: ProjectKey,
    pub worktree: Option<WorktreeKey>,
    pub rel: RelPath,
    pub package: PackageMatch,
}

/// 包含这个路径的最深子项目。`locate` 只用当前 worktree（文件夹项目为根目录）快照里的子项目列表，
/// 它由监听保持最新；不拿它去套其他 worktree（各分支可能增删或移动了子项目），也不读非当前 worktree 的缓存
///（那些 worktree 只监听 HEAD，未暂存的清单修改不会让缓存失效）。
pub enum PackageMatch {
    /// 匹配到的子项目目录。
    Found(RelPath),
    /// 当前 worktree 的子项目已就绪，路径不在任何子项目里。
    None,
    /// 两种情况，调用方按 `Location.worktree` 区分：
    /// - 路径在非当前 worktree（总是这一项）：需要时在 executor 上调 `resolve_package`，由它检查缓存是否过期；
    /// - 路径在当前 worktree 或文件夹项目里，子项目还是 `Pending`：等下一次 `ProjectEvent::Changed` 后重新调 `locate`。
    NotLoaded,
}

pub struct Resolved {
    pub project: ProjectKey,
    pub worktree: Option<WorktreeKey>,
    pub rel: RelPath,
}
```

不变量：

- `worktrees` 里 `key` 和 `root` 都不重复，最多一个 `is_main`；`active.worktree` 是其中一个未 prunable 的 worktree。
- `files` 先于 `packages` 就绪：`packages` 为 `Ready` 时 `files` 一定是 `Ready`，且子项目来自同一份文件列表。
- 一次替换内的各部分来自同一轮扫描，不会出现「新的 worktree 列表配旧的当前 worktree」。

### 文件列表

```rust
/// 一个 worktree 的受跟踪文件。连续的路径缓冲加结束偏移，按字节序排序（即 index 顺序），
/// 所以一个目录下的文件是连续的一段。
pub struct FileList { /* buf: Box<str>, ends: Box<[u32]>, kinds: Box<[FileKind]>, skipped: u32 */ }

impl FileList {
    pub fn len(&self) -> usize;
    pub fn get(&self, i: usize) -> Option<(&str, FileKind)>;
    pub fn iter(&self) -> impl Iterator<Item = (&str, FileKind)> + '_;
    /// `dir` 下全部文件的下标范围，二分查找。`dir` 为 "" 时是全部。
    pub fn range_under(&self, dir: &str) -> Range<usize>;
    /// 因不是 UTF-8 等原因没有收入的条目数。
    pub fn skipped(&self) -> u32;
    /// 文件夹项目的遍历达到上限时为 true。
    pub fn truncated(&self) -> bool;
}

pub enum FileKind { File, Executable, Symlink, Submodule }
```

- git 项目：来自 index，同一路径的多个合并阶段（冲突中）只收一条。index 里没有的文件（未跟踪的新文件）不在列表里。
- 文件夹项目：从根目录遍历，不跟随符号链接；跳过以 `.` 开头的目录和 `node_modules`、`target`、`build`、`dist`、`out`、`vendor`、`venv`、`__pycache__`；最多 20,000 个条目，超过时停止并标记 `truncated`。
- 不带 blob OID：P1 的消费者（⌘K、Files、`WorkspaceFacts`、子项目）只要路径。需要时（`code_intel`，P2）另加，不改现有字段。

### 子项目与清单

```rust
/// 一个目录一个子项目；同一目录里有几种清单（如 package.json 与 pom.xml）时合在一起。
/// 被声明、但目录里没有自己清单的成员（Gradle 子项目不要求构建文件；Maven、Gradle、go.work 的显式路径只要求有受跟踪文件）`manifests` 为空。
pub struct Package {
    pub dir: RelPath,
    /// 显示名：按 package.json `name`、Cargo `package.name`、go.mod `module`、Maven `artifactId`、
    /// Gradle 项目名的顺序取第一个有的，都没有时用目录名（根目录用项目名）。
    pub name: Arc<str>,
    pub manifests: Arc<[Manifest]>,
    /// 被某个工作区清单声明为成员。
    pub declared: bool,
    /// 本目录有声明成员的清单（workspaces、[workspace]、modules、settings.gradle、go.work）。
    pub workspace_root: bool,
    /// 最近的上级子项目在列表里的下标。
    pub parent: Option<u32>,
    /// 目录下的受跟踪文件数（含下级子项目的文件）。
    pub files: u32,
}

pub struct Manifest {
    pub kind: ManifestKind,
    pub path: RelPath,
    pub name: Option<Arc<str>>,
    /// 有不支持的写法，结果可能不全（Gradle 的变量与循环等）。
    pub partial: bool,
    pub error: Option<ManifestError>,
}

pub enum ManifestKind { PackageJson, PnpmWorkspace, CargoToml, PomXml, GradleSettings, GradleBuild, GoMod, GoWork }

pub enum Ecosystem { Npm, Cargo, Maven, Gradle, Go }

impl ManifestKind {
    pub fn ecosystem(self) -> Ecosystem;
}

pub enum ManifestError {
    TooLarge,             // 超过 1 MB，不解析
    Syntax { line: u32 }, // 解析失败的位置
    Unreadable,           // 读取失败，或是符号链接等非普通文件
}
```

不变量：`dir` 不重复，列表按 `dir` 的字节序排序；`parent` 指向的子项目的 `dir` 是本 `dir` 的真前缀（按 `/` 分段）；`manifests` 为空时 `declared` 一定为 true（未声明的子项目来自自己目录里的清单）。

#### 清单格式

| 文件 | 格式来源与版本 | 读取的内容 | 成员如何声明 |
|---|---|---|---|
| `package.json` | npm 7+ [workspaces](https://docs.npmjs.com/cli/using-npm/workspaces)；yarn 1 的对象写法 `{ "packages": [...] }`；yarn 2+、bun 与 npm 相同 | `name`、`workspaces` | glob，相对本文件所在目录，只匹配有 `package.json` 的目录 |
| `pnpm-workspace.yaml` | [pnpm 7–10](https://pnpm.io/pnpm-workspace_yaml) | `packages` | glob，`!` 开头的为排除；同目录的 package.json 是根子项目 |
| `Cargo.toml` | [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html)（与 edition 无关） | `package.name`、`workspace.members`、`workspace.exclude` | `members` 为 glob，`exclude` 为路径，只匹配有 `Cargo.toml` 的目录 |
| `pom.xml` | POM 4.0.0（Maven 3）与 4.1.0（Maven 4，[`subprojects`](https://maven.apache.org/whatsnewinmaven4.html)） | `artifactId`、`groupId`（缺省时取 `parent`）、`modules/module`、`subprojects/subproject`，含 `profiles` 里的 | 相对路径，指向目录或另一个 pom 文件；无法判断哪个 profile 生效，全部计入 |
| `settings.gradle`、`settings.gradle.kts` | Gradle 7–9 的 [settings 脚本](https://docs.gradle.org/current/userguide/multi_project_builds.html) | `rootProject.name`、`include`、`includeFlat`、`includeBuild` 的字面量参数，`project(":x").projectDir = file("...")` | `":a:b"` 对应 `a/b`，`includeFlat "x"` 对应 `../x`，`projectDir` 覆盖默认目录；子项目目录不要求有构建文件 |
| `build.gradle`、`build.gradle.kts` | 同上 | 不读内容，只说明这个目录是 Gradle 项目 | — |
| `go.work` | Go 1.18+ [workspaces](https://go.dev/ref/mod#workspaces) | `use`（单行与块） | 目录路径 |
| `go.mod` | [Go modules](https://go.dev/ref/mod#go-mod-file) | `module` | — |

fixtures：每行至少一份真实写法的样本（脱敏），格式随版本变化的每个版本一份（yarn 1 对象写法、POM 4.0.0 与 4.1.0、Groovy 与 Kotlin DSL）。

#### 成员解析

`resolve_packages` 是纯函数，输入文件列表和每个清单的解析结果：

1. 候选：文件列表里每个清单文件（`FileKind::File` 或 `Executable`，符号链接不算）所在的目录。`manifest(path)` 返回 None 的不算；返回 Err 的照常是候选，只是它声明的成员无法展开。
2. 声明：每个工作区清单按上表的规则把成员模式展开成目录。glob 相对声明它的清单所在目录，`*` 不跨 `/`，`**` 匹配任意层；去掉 `./` 前缀和末尾的 `/`；`..` 按字面路径归并，越出 worktree 的丢弃。npm、pnpm、Cargo 的 glob 只匹配同生态有清单的候选目录；Maven、Gradle、go.work 的显式路径只要目录下有受跟踪文件就成立。
3. 合并：每个目录一个 `Package`。被声明的标 `declared`；没有自己清单的被声明目录也收入，`manifests` 为空，名字取声明里的名字（Gradle 的 `:a:b` 取最后一段）或目录名。未被声明的候选目录也收入，除非路径里有 `node_modules`、`vendor`、`fixtures`、`__fixtures__`、`testdata` 这几段之一（测试样本里的清单不是子项目）；被声明的目录不受这条排除。
4. 层级：按 `dir` 排序后，用栈算出每项的 `parent`；用 `FileList::range_under` 算出 `files`。

`cloudprint-miniapps` 的形状（yarn workspaces + Maven 多模块，`apps/miniapp-console/frontend` 有 package.json 却不在 workspaces 里）作为快照测试的样例：后者是 `declared: false` 的子项目。

### WorkspaceFacts

```rust
/// 所有打开项目的当前 worktree（文件夹项目为遍历结果）的文件列表的并集。
struct ProjectFacts { /* trees: Vec<Arc<FileList>>, 文件名与扩展名索引, 模式结果的缓存 */ }

impl WorkspaceFacts for ProjectFacts {
    fn contains(&self, pattern: &str) -> bool;
}
```

模式的语义（kernel 的 `Activation::Workspace` 和插件市场的激活条件共用）：

- 用 `/` 分隔的相对路径做 glob 匹配，`*` 不跨 `/`，`**` 匹配任意层。
- 不含 `/` 的模式匹配任意深度的文件名：`pom.xml` 匹配 `a/b/pom.xml`，`build.gradle*` 匹配 `x/build.gradle.kts`。含 `/` 的模式从 worktree 根开始匹配。`**/*.java` 与 `*.java` 等价。
- 只匹配文件（不含 `Submodule` 条目），不匹配目录。
- 不合法的模式返回 false，不 panic（kernel 会把 `contains` 的 panic 记为 `project` 失败）。

实现：提交前在 executor 上建好文件名集合与扩展名集合。纯文件名和 `*.ext` 形式的模式查一次哈希表；其他文件名模式遍历去重后的文件名；含 `/` 的模式遍历路径。每个模式的结果缓存在这份 facts 里（`Mutex<HashMap>`），同一份 facts 不重复计算。

提交时机：打开、关闭项目，切换当前 worktree，以及当前 worktree 的文件列表变化之后，在 executor 上构造新的 facts，调用 `control.workspace_changed`。kernel 的激活条件一旦满足就保持，所以关闭项目或删除文件不会停用已激活的插件。

## 数据流与线程

```
open(path) ─▶ [executor] discover ─▶ worktrees ─▶ 当前 worktree 的 index ─▶ FileList
                                                         │                      │
                         快照 v1（worktrees、files）◀─────┘                      ├─▶ facts ─▶ workspace_changed
                                                                                │
                                   读清单 ─▶ 解析（按 blob OID 缓存）─▶ resolve_packages ─▶ 快照 v2（packages）
fs_watch 事件 ─▶ 标记脏 ─▶ [executor] 增量刷新 ─▶ 新快照 ─▶ ProjectEvent::Changed
```

- 所有 IO（discover、读 index、读清单、遍历文件夹）都在 executor 上，经 `PluginHandle::own_with` 提交，每一轮扫描持有一个 `BusyGuard`，结束、快照替换并发出事件后才释放（kernel「静止」）。
- UI 线程只调 `snapshot`、`locate`、`open_projects`（取锁复制 `Arc` 或在内存里比较），以及立即返回的 `open`、`close`、`set_active_worktree`。
- 快照存在每个项目的 `Mutex<Arc<ProjectSnapshot>>` 里，只有这个项目的扫描任务替换它。每个项目同时最多一个扫描任务；运行期间来的事件只标记脏，结束后再跑一轮。一批事件到达后等 50 ms 再开始，合并 agent 连续写文件产生的多批事件。
- 增量刷新按变化的内容决定做什么：
  - 只有普通文件的内容变了：什么都不做（文件列表来自 index，内容变化不改变它）。
  - `index` 变了：重读 index 路径；校验和未变时跳过；变了就重建 `FileList` 和 facts，并重做成员解析。普通文件的增减也会改变 `Package.files` 和「显式路径的目录下有受跟踪文件」这类成员条件，所以只要 `FileList` 变了就重算。重算只读新出现的清单；已有清单沿用以 blob OID 为键的解析结果，只有 fs_watch 报告变化的才重读；成员解析本身是内存里的计算。
  - 清单文件变了：重读这个文件，重做成员解析。
  - 任一 worktree 的 `HEAD` 变了：只更新那个 worktree 的 `head`；当前 worktree 的 `index` 通常会一起变，按上一条处理。
  - `worktrees/` 下有增减：重新列 worktree；当前 worktree 被删除时切到主 worktree（没有时第一个可用的），`ChangedParts.active` 为 true。
- 清单内容从工作区读（用户和 agent 看到的版本，未暂存的修改也算），缓存键是这份字节的 blob OID（`hash_blob`，按仓库的哈希算法；文件夹项目用 SHA-1），值是 `Result<Arc<Parsed>, ManifestError>`：语法错误同样按 OID 缓存，`TooLarge` 和 `Unreadable` 没有可哈希的内容，不缓存。缓存在进程内、所有项目和 worktree 共享，按字节 LRU，上限 2 MB（[ADR 0017](../adr/0017.md)「内存的具体约束」）。切换 worktree 时，各分支内容相同的清单直接命中。
- 非当前 worktree 的子项目（`resolve_package`）：按需计算，缓存按 worktree 的 git 目录存放，内容是子项目列表、算出它时的 index 校验和，以及参与计算的每个清单的 stat 指纹（路径、大小、修改时间、inode）。缓存不存文件列表，最多 16 个 worktree，按最近使用淘汰；只有 `resolve_package` 读它，`locate` 不读。每次调用都校验缓存：
  - 先读 index 校验和（`GitAccess::index_checksum`，只读文件末尾）。变了就重读 index 并重算整份列表（文件列表变了，成员条件和 `Package.files` 都可能变）。
  - 没变就对指纹里的每个清单做一次 `lstat`（`std::fs::symlink_metadata`，不跟随符号链接）。有任何不一致（未暂存的修改、删除）时按未命中处理：重读 index 并重算整份列表，因为成员解析要用文件列表判断没有清单的声明目录下有没有受跟踪文件，而缓存不存文件列表。重算时内容 blob OID 没变的清单不重新解析。
  - 都一致才直接返回缓存的结果。
  - 同一个 worktree 的并发请求合并成一次计算。
  - 不在缓存检查范围内的变化：index 里没有的新清单在 `git add` 之前本来就不算（与当前 worktree 相同）。修改时间精度内、大小不变的连续写入可能漏掉一次，下次 stat 变化时纠正。
- 切换当前 worktree：先发布 `files: Pending` 的新快照（视图立即显示骨架），旧 worktree 的 `FileList` 随旧快照释放，只保留它的元数据。
- 插件停用时，kernel 丢弃扫描任务；视图手里的快照仍然有效，只是不再更新。

## 性能

| 场景 | 预算 | 基准（`crates/bench`） |
|---|---|---|
| 打开项目到文件列表就绪（discover、列 worktree、读 index、建 facts），`cloudprint-miniapps` 形状：728 个文件、11 个 worktree | < 20 ms | `project/open_cloudprint` |
| 同上，再到子项目就绪（约 25 个清单） | 再加 < 15 ms | `project/packages_cloudprint` |
| 大仓库：10 万个文件、2,000 个清单，到文件列表就绪 / 子项目就绪 | < 80 ms / 再加 < 150 ms | `project/open_100k` |
| 切换当前 worktree（清单缓存已热），`cloudprint` 形状 | < 30 ms | `project/switch_worktree` |
| `resolve_package`，非当前 worktree：缓存命中（读 index 校验和，加上约 25 个清单各一次 `lstat`）/ 未命中、清单缓存已热，`cloudprint` 形状 / 未命中，10 万文件 | < 1 ms / < 20 ms / < 150 ms | `project/resolve_package` |
| `index` 变化后的刷新（重建文件列表与 facts、重算成员解析，清单缓存已热），`cloudprint` 形状 / 10 万文件 | < 15 ms / < 60 ms | `project/refresh_index` |
| `contains`：文件名与 `*.ext` 模式 / 含 `/` 的模式（10 万文件，未缓存） | < 1 µs / < 5 ms | `project/facts_contains` |
| 常驻内存：`cloudprint` 形状 / 10 万文件（含 16 个非当前 worktree 的子项目列表缓存） | < 2 MB / < 12 MB | 静止后按 kernel「性能」一节测插件内存 |

- 对应[性能预算](tech-research.md#性能预算)的「打开项目到项目主页可用 < 100 ms」「切换 worktree / 分支 < 100 ms」和 ADR 0017 的「打开一个 monorepo 后空闲 < 100 MB」。主页在文件列表就绪后即可用，子项目稍后填入侧栏的 Packages。
- 首帧之前什么都不做：插件 `AfterFirstFrame` 激活，`activate` 只建空表；第一次扫描由首帧之后的 `open` 触发。
- 预算是初始值，实现时用真实规模的合成仓库实测后校准，写进 bench 的预算表，超标即 CI 失败。合成仓库由 `testing` 的形状描述加 git 的 `test-support` 构造器生成，不依赖 git 命令行和网络。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)，本 crate 只给出数据，表现由视图决定：

| 情况 | 快照 / 事件 |
|---|---|
| 打开中 | `Opened` 之前没有快照；之后 `files`、`packages` 为 `Pending`，视图按 100 ms 规则显示骨架 |
| 路径不存在、不是目录、没有权限 | `Opened` 带 `OpenError`，不创建项目 |
| `.git` 存在但读不出 | `OpenError::RepoUnreadable` |
| 不是 git 仓库 | 文件夹项目，没有 worktree 和分支 |
| index 读不出（损坏、不支持的扩展） | `files: Unavailable(IndexUnreadable)`，worktree 列表照常 |
| 清单读不出或语法错误 | 子项目照常列出（名字用目录名），`Manifest.error` 有值；不弹错误 |
| Gradle 用了不支持的写法 | `Manifest.partial`，已识别的部分照常 |
| worktree 根目录被删除 | 列为 prunable；是当前 worktree 时按「数据流与线程」切换 |
| 文件夹太大 | `FileList::truncated`，facts 与子项目只覆盖已遍历的部分 |

## 测试

全部在 Linux 上运行（`just test -p lectern-project`），不调用 git 命令行、不访问网络：

- 清单解析（`manifest_*`）：`tests/fixtures/manifests/<格式>/` 下每个样本的解析结果用 insta 快照；含 yarn 1 对象写法、pnpm 的 `!` 排除与注释、Cargo 虚拟清单（根子项目的名字取 `root_name`）与 `exclude`、POM 4.0.0 / 4.1.0 与 profile 里的 modules、Groovy / Kotlin 的 settings（`include` 多参数跨行、`projectDir`、`includeBuild`、变量写法标为 partial）、go.work 块写法；超过 1 MB、语法错误各一例。
- 成员解析（`packages_*`）：内存里构造文件列表，快照验收。场景：`cloudprint` 形状、嵌套工作区、同一目录多种清单、没有构建文件的 Gradle 子项目（`manifests` 为空）、语法错误与超大清单的错误保留在 `Manifest.error`、Maven 模块指向 `../`、越出 worktree 的路径、样本目录排除、未声明但有清单的目录。
- 模型（`open_*`、`worktrees_*`、`locate_*`、`resolve_*`）：用 `testing::FakeGit`（内存里的仓库、worktree、index 与工作区文件，记录每次调用）。场景：在子目录打开、同一仓库从另一个 worktree 再打开得到同一个键、裸仓库、主目录 dotfiles 仓库、prunable（根目录已删除，键仍来自管理目录）/ locked / detached / unborn、文件夹项目的上限与跳过规则、`locate` 取最深的子项目，以及父仓库和其中单独打开的子模块都包含路径时 `locate` 返回子模块的项目；路径在非当前 worktree 时 `locate` 返回 `NotLoaded`，`resolve_package` 按那个 worktree 自己的子项目给出结果（它比当前 worktree 多一个、少一个、移动了一个子项目各一例），index 与清单都未变时第二次调用命中缓存、不读清单；未暂存地修改或删除那个 worktree 的一个清单后，下一次 `resolve_package` 按未命中重算并给出新结果，其余清单不重新解析；已经填过缓存之后 `locate` 对非当前 worktree 仍返回 `NotLoaded`；当前 worktree 的子项目还是 `Pending` 时 `resolve_package` 返回 `NotLoaded`、不做 IO。
- 刷新（`refresh_*`）：用 fs_watch 的模拟后端驱动事件；连续事件合并为一轮、扫描中到达的事件触发下一轮、只改普通文件内容不重读 index、新增普通受跟踪文件后 `Package.files` 更新而已有清单不重新解析、清单 OID 未变不重新解析、删除当前 worktree 后的切换、generation 严格递增。
- facts（`facts_*`）：模式语义逐条覆盖；多个项目的并集；不合法的模式返回 false。
- 插件（`plugin_*`）：用 kernel 的 `TestKernel`。`activate` 之后、`open` 之前 `FakeGit` 没有收到任何调用；`open` 之后提交的 facts 让一个 `Workspace(["pom.xml"])` 的测试插件激活；事件顺序为 `Opened` → `Changed`。
- 真实仓库（`real_repo_*`）：用 git 的 `test-support` 构造器写出的仓库（含 linked worktree、SHA-256 仓库各一个）跑一遍 open 与刷新，确认 `GitAccess` 的生产实现与 `FakeGit` 行为一致。

没有只能在 macOS 上运行的部分。

## 未决问题

需要产品决定：

1. 在仓库的子目录里打开（如 ⌘O 选了 `apps/console`）：
   - 打开整个仓库，当前子项目定位到这个目录（默认）；
   - 只把这个子目录当作项目。
   默认选项符合 ADR 0020「同一仓库的所有 worktree 属于同一个项目」，用量也只计一次。
2. 同一仓库已经在一个窗口里打开，再从另一个 worktree 打开它：
   - 切到已有窗口，把当前 worktree 换成新打开的那个（默认）；
   - 再开一个窗口，两个窗口各有自己的当前 worktree。
   第二种要求快照按窗口区分当前 worktree，`Projects` 的接口要改，涉及的实现等答复后再做。

技术问题已在文中决定。

## 验收

实现分五步，每步一个 PR，只有最后一步写 `Closes #9`，其余写 `Refs #9`。

1. 清单解析与成员解析（`project`）：
   - `just test -p lectern-project` 里 `manifest_*`、`packages_*` 通过，insta 快照覆盖「清单格式」表的每一行。
   - `just deny` 通过；`cargo tree -p lectern-project -e normal --depth 1` 只列出「依赖」表里已用到的 crate。
   - `rg -n 'std::process|Command::new' crates/project/src` 没有结果（不运行构建工具）。
2. 服务接口（`services`）：
   - `cargo check -p lectern-services` 通过；`rg -n 'pub trait Projects' crates/services/src` 有一处。
   - `just test -p lectern-services` 里检查 `dyn Projects` 的 `ID == "projects"`、`MULTIPLE == false` 的测试通过。
   - `python3 scripts/check_architecture.py` 通过（`services` 仍只依赖 kernel 层）。
3. 项目模型与插件（`project`，依赖 `git` 的实现）：
   - `just test -p lectern-project` 里 `open_*`、`worktrees_*`、`locate_*`、`resolve_*`、`facts_*`、`plugin_*`、`real_repo_*` 通过。
   - `cargo tree -p lectern-project -e normal | rg gpui` 没有结果；`python3 scripts/check_architecture.py` 通过。
   - `rg -n 'gix' crates/project/Cargo.toml` 没有结果（git 只经 `lectern-git`）。
4. 增量刷新（`project`，依赖 `fs_watch` 的实现）：
   - `just test -p lectern-project` 里 `refresh_*` 通过。
5. 基准（`bench`，`Closes #9`）：
   - `just bench` 里 `project/*` 全部在「性能」表的预算内，预算写进 bench 的预算表。
   - 本文状态行改为「已实现」并列出五个 PR，填写「实际验证」。
