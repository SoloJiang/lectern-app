# project 设计

状态：实现中。第 1 步（清单解析与成员解析）、第 2 步（`services` 里的服务接口与数据类型）、第 3 步（项目模型与插件，[#85](https://github.com/SoloJiang/lectern-app/pull/85)）、第 4 步（增量刷新，[#PR](https://github.com/SoloJiang/lectern-app/pull/PR)）已实现，第 5 步尚未实现（见「验收」）。

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
| `lectern-fs-watch` | worktree 与 git 元数据的变化通知 | 接口见下文；实例由 `app` 创建，经 `ProjectDeps::watcher` 传入。第 4 步（增量刷新）接入；`test-support` 打开它的 `test-support`（`FakeGit` 的 worktree 返回 `FakeSource`） |
| `lectern-platform` | 文件夹项目读清单用的 `SafeRoot` | 与 `git` 读工作区文件同一套安全读取（不跟随符号链接、不越出根目录）；文件夹项目没有 `Worktree` 句柄 |
| `lectern-executor` | 后台任务 | UI 线程不做 IO；实例由 `app` 创建，经 `ProjectDeps` 传入 |
| `serde` 1（`derive`）+ `serde_json` 1 | package.json、Cargo.toml 的反序列化 | MIT / Apache-2.0；只反序列化需要的字段 |
| `toml` 1.1（`std`、`parse`、`serde`，不开 `display`） | Cargo.toml | MIT / Apache-2.0 |
| `quick-xml` 0.42（不开任何 feature） | pom.xml | MIT；流式读取，只依赖 `memchr` |
| `glob` 0.3 | 工作区成员与 `WorkspaceFacts` 的模式匹配 | MIT / Apache-2.0，无传递依赖；Cargo 用它解析 `members` |

选型与放弃的方案见 [ADR 0023](../adr/0023.md)。pnpm-workspace.yaml、settings.gradle(.kts)、go.work、go.mod 用手写的子集解析。版本取实现时 crates.io 上的最新兼容版本，由 Cargo.lock 锁定。

- 不用 `cache`：解析一个清单是微秒级，比 SQLite 查询还快，持久化没有收益。解析结果在内存里以 blob OID 为键缓存（见「数据流与线程」）。
- 本 crate 是插件 crate（`Cargo.toml` 的 `[package.metadata.lectern]` 写 `plugin = true`，[ADR 0025](../adr/0025.md)）：可以依赖 kernel 层的 crate 和不是插件的库 crate（`lectern-git`、`lectern-executor`、`lectern-fs-watch`、`lectern-platform`，以及以后需要时的 `lectern-cache`），不依赖任何其他插件 crate；只有 `app` 依赖本 crate。其他插件读项目模型只经 `services` 里的 `Projects`。

## 接口

### 服务接口（`lectern-services` 的 `project` 模块）

接口和数据类型放在 `services`，由本 crate 实现。类型都是不含行为的数据（`FileList` 的查找方法除外），能通过 JSON-RPC 序列化（[ADR 0018](../adr/0018.md)）；`services` 的设计文档引用本节，不重复定义。所有数据类型派生 serde 的 `Serialize`、`Deserialize`（`Arc<str>`、`Arc<[T]>` 用 serde 的 `rc` feature），按 serde 的默认外部标记表示枚举；`FileList` 例外，见「文件列表」。`Projects` trait 本身不序列化，进程插件的调用协议见 [#39](https://github.com/SoloJiang/lectern-app/issues/39)。各类型另派生 `Clone`、`PartialEq`、`Eq`、`Debug`，键与小的值类型另派生 `Hash`。

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

- 新项目在 `Opened` 之前放入第 1 代快照（`files`、`packages` 为 `Pending`），`Opened` 宣告它；之后每次替换都发一个 `Changed`，包括切换 worktree 时 `Pending` 的那一代。没有可扫描的 worktree（裸仓库没有可用的 linked worktree）时，第 2 代快照内容不变，它的 `Changed` 就是 `Opened` 之后的那一个。
- 打开一个已经打开的项目：重新列 worktree，当前 worktree 换成路径所在的那个，重读 index 和清单。第 4 步之前这是唯一的刷新方式。
- `Closed` 之后这个项目不再有 `Changed`：替换快照、发出事件与发出 `Closed` 在同一把锁下。

### 本 crate 的公开项

```rust
/// `app` 装配时构造，放进 `PluginSet`（id "project"）。
pub struct ProjectPlugin { /* deps */ }

pub struct ProjectDeps {
    /// 由 app 创建并传入。
    pub executor: lectern_executor::Executor,
    /// 第 4 步加入：由 app 在启动屏障放开后创建并传入（FSEvents 后端由 platform 实现），见 [fs_watch 设计](fs_watch.md)。
    /// `activate` 只把它交给项目模型，第一次 `open` 之前不调用 `watch_*`。
    pub watcher: lectern_fs_watch::Watcher,
    /// 用户主目录，判断「主目录被当作 dotfiles 仓库」（见「项目标识」）。由 app 从 platform 取得。
    pub home: PathBuf,
}

impl ProjectPlugin {
    /// 从环境读一次 `GIT_CEILING_DIRECTORIES`，不做 IO。
    pub fn new(deps: ProjectDeps) -> Self;
    /// 同一个插件，git 换成 `testing::FakeGit`（`test-support`）。
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_fake_git(deps: ProjectDeps, git: Arc<testing::FakeGit>) -> Self;
}

// Manifest { provides: [service::<dyn Projects>()], requires: [], activation: [AfterFirstFrame] }
// activate：建空的项目表，provide 服务；不做 IO。started：不提交任何任务，等第一次 open。

/// 纯函数，供测试和 bench 直接调用；不做 IO，任何线程。
pub mod manifest {
    /// 超过这个大小的清单不解析（`ManifestError::TooLarge`），也是 `read_worktree_file` 的 limit。
    /// 定义在 `services`（`ManifestError` 的 `Display` 要用它），这里重新导出。
    pub use lectern_services::project::MAX_MANIFEST_BYTES; // = 1024 * 1024

    /// 一个清单的解析结果。成员与排除项相对清单所在目录、按原样保存（只去掉引号），归并在 `resolve_packages` 里做。
    pub struct Parsed {
        pub name: Option<Arc<str>>,  // 按「清单格式」表的名字字段
        pub group: Option<Arc<str>>, // Maven `groupId`，缺省时取 `parent` 的
        pub workspace: bool,         // 清单声明了工作区（即使为空），决定 `Package::workspace_root`
        pub members: Vec<Member>,
        pub excludes: Vec<Exclude>,
        pub partial: bool,
    }
    pub enum Member {
        Glob(Box<str>),                                     // npm、pnpm、Cargo `members`
        Path { path: Box<str>, name: Option<Arc<str>> },    // Maven、Gradle、go.work；name 是声明给的名字（Gradle 路径的最后一段）
    }
    pub enum Exclude {
        Glob(Box<str>), // pnpm 与 npm 的 `!pattern`
        Path(Box<str>), // Cargo `exclude`：这个目录及其下的一切
    }
    pub fn parse(kind: ManifestKind, bytes: &[u8]) -> Result<Parsed, ManifestError>;
}

// `ManifestKind::from_file_name`（按文件名识别清单）随 `ManifestKind` 定义在 `services`，见「子项目与清单」。

pub type ManifestResult = Result<Arc<Parsed>, ManifestError>;

/// `manifest(path)`：None 表示工作区里没有这个文件（已删除，不算清单）；Some(Err) 是读取或解析失败，
/// 对 `build.gradle(.kts)` 不调用（不读内容，在文件列表里即算 Gradle 项目），调用方不为它们读文件、算 OID；
/// 子项目照常列出并把错误填进 `Manifest.error`。
/// `root_name`：项目显示名（`ProjectSnapshot::name`），根目录的子项目没有清单名字时（如 Cargo 虚拟清单）用它。
pub fn resolve_packages(
    files: &FileList,
    root_name: &str,
    manifest: &dyn Fn(&str) -> Option<ManifestResult>,
) -> Vec<Package>;
pub fn workspace_facts(trees: Vec<Arc<FileList>>) -> Arc<dyn WorkspaceFacts>;

/// `test-support` feature：假 git（见「测试」）；合成仓库的形状随第 5 步（bench）加入。
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
```

### 依赖 git 的接口

本 crate 只通过一个内部 trait 访问 git，生产实现转调 `lectern-git`，测试用 `testing::FakeGit`。都是阻塞调用，只在 executor 上执行。`lectern-git` 的 index 与工作区读取都在 `Worktree` 句柄上，句柄会缓存最近一次读出的 `TrackedPaths`，所以接口按「一轮扫描打开一个句柄」组织，而不是每次按路径调用：

```rust
pub(crate) trait GitAccess: Send + Sync {
    /// 从 path 向上发现仓库，不跨越 GIT_CEILING_DIRECTORIES（`ProjectPlugin::new` 从环境读一次）。不是仓库时返回 Ok(None)。
    fn discover(&self, path: &Path) -> Result<Option<Discovered>, GitError>;
    /// 公共目录下的全部 worktree：主 worktree（裸仓库没有）与 `<common>/worktrees/*`。
    fn worktrees(&self, common_dir: &Path) -> Result<Vec<WorktreeInfo>, GitError>;
    /// 为一轮扫描打开一个 worktree（生产实现是 `Repo::worktree`）：读 index、读清单共用这个句柄，这一轮结束即释放，
    /// 两轮之间不留 worktree 句柄和 index 内容。例外是有监听的当前 worktree：句柄随它的 `WorktreeWatch` 保留，
    /// 之后的刷新和同一 worktree 的再次扫描都用它（见 `watch_source`）。根目录已不存在时失败。
    fn open_worktree(&self, common_dir: &Path, git_dir: &Path) -> Result<Box<dyn TreeAccess>, GitError>;
    /// 按仓库的哈希算法计算 blob OID（与 `git hash-object` 相同），作为解析缓存的键。
    fn hash_blob(&self, kind: HashKind, bytes: &[u8]) -> Oid;
}

/// `Send + Sync`：当前 worktree 的句柄随监听保留，在 executor 的不同任务之间共用。
pub(crate) trait TreeAccess: Send + Sync {
    /// index 的路径直接建成 `FileList`（不是 UTF-8 的路径和 git 拒绝的路径记入 `skipped`），附 index 的校验和。
    fn index_files(&self, cancel: &AtomicBool) -> Result<IndexFiles, GitError>;
    /// 只读 index 的校验和（文件末尾），判断 `resolve_package` 的缓存和刷新时的文件列表是否过期。
    fn index_checksum(&self) -> Result<Option<Oid>, GitError>;
    /// 从 worktree 根目录安全地读一个工作区文件（`Worktree::read_file`：不跟随符号链接、不越出 worktree）；
    /// 不存在时 Ok(None)，超过 limit 时 Err(TooLarge)，符号链接等非普通文件为其他错误。
    fn read_file(&self, rel: &str, limit: u64) -> Result<Option<Vec<u8>>, GitError>;
    /// 第 4 步：`fs_watch` 的过滤器与配置来源，生产实现是这个句柄的 `Worktree::watch_source`。不做 IO。
    fn watch_source(&self) -> Arc<dyn lectern_fs_watch::WorktreeSource>;
}

pub(crate) struct Discovered { worktree_root: Option<PathBuf>, git_dir: PathBuf, common_dir: PathBuf, bare: bool, hash: HashKind }
pub(crate) struct WorktreeInfo { root: PathBuf, git_dir: PathBuf, is_main: bool, head: Head, locked: bool, prunable: bool }
pub(crate) struct IndexFiles { checksum: Option<Oid>, files: FileList }
```

- `Discovered::git_dir`：路径在某个 worktree 里时是它的 git 目录；在 git 目录里（`lectern-git` 的 `Discovered::worktree` 为 None）时，路径在 `<公共目录>/worktrees/<id>` 下取这个管理目录，否则取公共目录。
- `Head` 来自 `HeadState`：有提交的分支为 `Branch`，没有提交的为 `Unborn`，分离为 `Detached`（完整 OID 的十六进制）。`lectern-git` 读不出 HEAD 时整个 `worktrees` 调用失败（主 worktree）或跳过这个 worktree（linked），所以生产实现不产生 `Head::Unknown`。
- 第 4 步加入 `TreeAccess::watch_source`：当前 worktree 交给 `fs_watch::RepoWatch::watch_worktree` 的过滤器与配置来源，生产实现从 `open_worktree` 打开的同一个 `Worktree` 句柄调 `Worktree::watch_source`；`FakeGit` 的每个 worktree 有一个 `fs_watch::testing::FakeSource`，跟踪路径随它内存里的 index 更新，测试在上面加忽略规则。`watch_source` 本身只读 HEAD 和配置，不读 index。来源持有这个句柄，本 crate 也保留它（`Worktree::tracked` 在 index 没变时返回同一个 `Arc`），所以 fs_watch 重建过滤器和本 crate 刷新文件列表共用一份 index 解析结果；`FakeGit` 的句柄因此按调用时的内存状态回答，不是打开时的副本。
- 文件夹项目没有 `Worktree` 句柄：清单经 `lectern_platform::SafeRoot` 读取，规则与 `TreeAccess::read_file` 相同；遍历用标准库，不跟随符号链接。

另需 `git` 的 `test-support` 提供不调用 git 命令行的仓库构造器：写对象、index 和 linked worktree 的管理目录，用于集成测试和 bench。

### 依赖 fs_watch 的接口

每个打开的 git 项目一份订阅，签名见 [fs_watch 设计「接口」](fs_watch.md#接口)。文件夹项目不监听（「范围」：P1 只在打开时扫描一次）。

- 仓库：第一次扫描之前 `watcher.watch_repo(RepoSpec { common_dir, coalesce: Coalesce::REPO })`，项目关闭前一直保留。`RepoBatch` 里任一 worktree 的 `head`、`worktree_list`（`<公共目录>/worktrees/` 下条目的增减，即 `git worktree add / remove / prune`，以及 `gitdir`、`locked`）、`rescan`、`health` 为 `Failed`，以及快照里有 unborn 分支时的 `refs`，都让项目重新列 worktree。其他 worktree 只经这份订阅得到 `HEAD` 的变化，不监听工作区文件（非活动 worktree 只留元数据，[ADR 0017](../adr/0017.md)）。
- 当前 worktree：扫描它时 `repo_watch.watch_worktree(WorktreeSpec { name, root, source: tree.watch_source(), coalesce: Coalesce::WORKTREE })`，`name` 由 git 目录得出（公共目录为 `Main`，否则 `Linked(<worktrees/ 下的目录名>)`）。切到另一个 worktree 时先 drop 旧的 `WorktreeWatch`，再监听新的。`WorktreeBatch` 的 `paths`（按 index 过滤后的变更路径）、`meta.index`、`meta.head`（fs_watch 的修订号已覆盖这两个文件）、`rescan`、`health` 为 `Failed` 让项目刷新；只有 `refs`、`config`、`rules` 的批次不影响文件列表和子项目，直接丢弃。
- `Sink::busy` 返回 `PluginHandle::busy()`；`deliver` 在 fs_watch 线程上只把批次并进项目的脏标记，需要时提交扫描循环，守卫留到处理这一批的那一轮结束才 drop（[kernel「静止」](kernel.md#静止)）。每个 `WorktreeWatch` 的 sink 带一个编号，项目记着当前监听的编号，旧 worktree 的迟到批次不加锁就能丢弃。
- 每个订阅的第一批带 `rescan: true`（fs_watch「监听器与句柄」），覆盖扫描与 arm 之间的变化：项目据此重新列一次 worktree、读一次 index 校验和、重读一次全部清单（解析缓存命中，不重新解析），没有变化时不替换快照。

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
    pub fn builder() -> FileListBuilder;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn get(&self, i: usize) -> Option<(&str, FileKind)>;
    pub fn iter(&self) -> impl Iterator<Item = (&str, FileKind)> + '_;
    /// `dir` 下全部文件的下标范围，二分查找。`dir` 为 "" 时是全部。
    pub fn range_under(&self, dir: &str) -> Range<usize>;
    /// 这个路径是不是列表里的文件，二分查找。
    pub fn find(&self, path: &str) -> Option<FileKind>;
    /// 因不是 UTF-8 等原因没有收入的条目数。
    pub fn skipped(&self) -> u32;
    /// 文件夹项目的遍历达到上限时为 true。
    pub fn truncated(&self) -> bool;
}

pub enum FileKind { File, Executable, Symlink, Submodule }

/// 按 index 顺序 push 时不排序；乱序时 `build` 排序。同一路径只收第一条；
/// 空路径、以 `/` 开头或结尾、含 `.` 或 `..` 段的路径记入 `skipped`。
pub struct FileListBuilder { /* … */ }

impl FileListBuilder {
    pub fn push(&mut self, path: &str, kind: FileKind);
    pub fn skip(&mut self);          // 没有收入的条目（如不是 UTF-8）
    pub fn set_truncated(&mut self); // 文件夹遍历达到上限
    pub fn build(self) -> FileList;
}
```

- git 项目：来自 index，同一路径的多个合并阶段（冲突中）只收一条。index 里没有的文件（未跟踪的新文件）不在列表里。
- 文件夹项目：从根目录遍历，不跟随符号链接；跳过以 `.` 开头的目录和 `node_modules`、`target`、`build`、`dist`、`out`、`vendor`、`venv`、`__pycache__`（以 `.` 开头的文件照常收入）；最多 20,000 个条目，超过时停止并标记 `truncated`。按名字的字节序深度优先遍历，上限截断的结果是确定的；条目指文件和符号链接，FIFO、socket、设备文件不收入，名字不是 UTF-8 的记入 `skipped`，读不出的子目录跳过。
- 不带 blob OID：P1 的消费者（⌘K、Files、`WorkspaceFacts`、子项目）只要路径。需要时（`code_intel`，P2）另加，不改现有字段。
- 序列化：`{ "entries": [[路径, 类型], …], "skipped": n, "truncated": b }`，不暴露内部的缓冲与偏移。反序列化经 `FileListBuilder`：来自其他进程的乱序、重复或不合法的路径按构造器的规则排序、去重、记入 `skipped`，偏移不会越界，查找不会 panic。

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
    /// 按文件名（路径最后一段）识别清单，不是清单时为 None。
    pub fn from_file_name(name: &str) -> Option<ManifestKind>;
}

pub enum ManifestError {
    TooLarge,             // 超过 1 MB（`MAX_MANIFEST_BYTES`），不解析
    Syntax { line: u32 }, // 解析失败的位置，从 1 开始
    Unreadable,           // 读取失败，或是符号链接等非普通文件
}
```

`Syntax` 包括：JSON、TOML、XML 语法错误；package.json 与 Cargo.toml 里读取的字段类型不对（如 `"workspaces": 3`）；JSON 与 TOML 不是 UTF-8；pnpm-workspace.yaml 的引号未闭合、流序列未闭合、`packages` 键重复（pnpm 用的 js-yaml 拒绝重复键）；Gradle 脚本的字符串或块注释未闭合；go.work / go.mod 的字符串或块未闭合、多余的 `)`、go.mod 重复的 `module`。pom.xml 与手写解析的格式按 UTF-8 宽松解码（无效字节换成 U+FFFD）：老 pom 常带 ISO-8859-1 的注释，而要读的字段都是 ASCII。开头的 UTF-8 BOM 一律跳过。

不变量：`dir` 不重复，列表按 `dir` 的字节序排序；`parent` 指向的子项目的 `dir` 是本 `dir` 的真前缀（按 `/` 分段）；`manifests` 为空时 `declared` 一定为 true（未声明的子项目来自自己目录里的清单）。

#### 清单格式

| 文件 | 格式来源与版本 | 读取的内容 | 成员如何声明 |
|---|---|---|---|
| `package.json` | npm 7+ [workspaces](https://docs.npmjs.com/cli/using-npm/workspaces)；yarn 1 的对象写法 `{ "packages": [...] }`；yarn 2+、bun 与 npm 相同 | `name`、`workspaces` | glob，相对本文件所在目录，只匹配有 `package.json` 的目录；`!` 开头的为排除 |
| `pnpm-workspace.yaml` | [pnpm 7–10](https://pnpm.io/pnpm-workspace_yaml) | 顶层 `packages`：块序列（缩进可与键相同）或流序列（可跨行），纯量、单引号、双引号，注释 | glob，`!` 开头的为排除；同目录的 package.json 是根子项目；锚点、别名、标签、块纯量、嵌套集合不展开，标为 partial |
| `Cargo.toml` | [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html)（与 edition 无关） | `package.name`、`workspace.members`、`workspace.exclude` | `members` 为 glob，`exclude` 为路径，只匹配有 `Cargo.toml` 的目录 |
| `pom.xml` | POM 4.0.0（Maven 3）与 4.1.0（Maven 4，[`subprojects`](https://maven.apache.org/whatsnewinmaven4.html)） | `artifactId`、`groupId`（缺省时取 `parent`）、`modules/module`、`subprojects/subproject`，含 `profiles` 里的 | 相对路径，指向目录或另一个 pom 文件；无法判断哪个 profile 生效，全部计入 |
| `settings.gradle`、`settings.gradle.kts` | Gradle 7–9 的 [settings 脚本](https://docs.gradle.org/current/userguide/multi_project_builds.html) | `rootProject.name`、`include`、`includeFlat`、`includeBuild` 的字面量参数，`project(":x").projectDir = file("...")` | `":a:b"` 对应 `a/b`，`includeFlat "x"` 对应 `../x`，`projectDir` 覆盖默认目录；子项目目录不要求有构建文件 |
| `build.gradle`、`build.gradle.kts` | 同上 | 不读内容，只说明这个目录是 Gradle 项目 | — |
| `go.work` | Go 1.18+ [workspaces](https://go.dev/ref/mod#workspaces) | `use`（单行与块） | 目录路径 |
| `go.mod` | [Go modules](https://go.dev/ref/mod#go-mod-file) | `module` | — |

fixtures：每行至少一份真实写法的样本（脱敏），格式随版本变化的每个版本一份（yarn 1 对象写法、POM 4.0.0 与 4.1.0、Groovy 与 Kotlin DSL）。

#### 成员解析

`resolve_packages` 是纯函数，输入文件列表和每个清单的解析结果：

1. 候选：文件列表里每个清单文件（`FileKind::File` 或 `Executable`，符号链接不算）所在的目录。`manifest(path)` 返回 None 的不算；返回 Err 的照常是候选，只是它声明的成员无法展开。`build.gradle`、`build.gradle.kts` 不调用 `manifest`，在文件列表里就是候选（`packages_gradle_build_scripts_are_never_read`）。
2. 声明：每个工作区清单按上表的规则把成员模式展开成目录。glob 相对声明它的清单所在目录，`*` 不跨 `/`，`**` 匹配任意层；去掉 `./` 前缀和末尾的 `/`；`..` 按字面路径归并，越出 worktree 的丢弃，绝对路径也丢弃。npm、pnpm、Cargo 的 glob 只匹配同生态有清单的候选目录（npm 与 pnpm 是有 `package.json` 的目录，Cargo 是有 `Cargo.toml` 的目录）；npm 与 pnpm 的通配符不匹配以 `.` 开头的目录，Cargo 的匹配（与 Cargo 相同）。含通配符的 glob 不匹配 worktree 根目录本身（`*` 能匹配空串，但工作区不会把自己的根声明为成员）。排除项只作用于同一个清单的 glob：pnpm / npm 的 `!` 排除匹配到的目录；Cargo 的 `exclude` 排除这个目录及其下的一切，但不含通配符的 `members` 项优先于 `exclude`（与 Cargo 相同）。不合法的 glob 跳过，所在清单的 `Manifest.partial` 标为 true。Maven、Gradle、go.work 的显式路径只要目录下有受跟踪文件就成立；Maven 的路径正好是列表里的一个文件时（指向另一个 pom 文件），取它所在的目录。
3. 合并：每个目录一个 `Package`。被声明的标 `declared`；没有自己清单的被声明目录也收入，`manifests` 为空，名字取声明里的名字（Gradle 的 `:a:b` 取最后一段）或目录名。未被声明的候选目录也收入，除非路径里有 `node_modules`、`vendor`、`fixtures`、`__fixtures__`、`testdata` 这几段之一（测试样本里的清单不是子项目）；被声明的目录不受这条排除。
4. 层级：按 `dir` 的字节序排序后，对每项从近到远查它的上级目录（`a/b/c` → `a/b` → `a` → 根），第一个在列表里的就是 `parent`。不用栈：字节序下 `a-b` 排在 `a` 与 `a/c` 之间，栈会把 `a` 弹出。用 `FileList::range_under` 算出 `files`。

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
- 快照存在每个项目的 `Mutex<Arc<ProjectSnapshot>>` 里，只有这个项目的扫描任务替换它。每个项目同时最多一个扫描任务；运行期间来的事件只标记脏，结束后再跑一轮。一批事件到达后等 50 ms 再开始，合并 agent 连续写文件产生的多批事件；这段等待由 `fs_watch` 的合并实现（`Coalesce::WORKTREE`），本 crate 收到批次后不再另外等待。循环开始之前到达的几批并成一轮。
- 扫描循环先处理请求（打开、再次打开、切换 worktree），再刷新：完整扫描重读当前 worktree 的 index 和全部清单，所以取出请求时清掉当前 worktree 的脏标记，只留「重新列 worktree」。刷新途中有新请求或项目被关闭时放弃这一轮的结果，脏标记并回去。
- 增量刷新按变化的内容决定做什么：
  - 只有普通文件的内容变了：什么都不做，不读 index 也不读文件（文件列表来自 index，内容变化不改变它）。
  - `index` 变了：先读 index 校验和；与上一轮相同时跳过；变了就读 index 路径，路径与当前 `FileList` 相同（`git status` 这类只刷新 stat 信息的改写）时沿用原来的 `FileList`、不替换快照；路径变了就重建 `FileList` 和 facts，并重做成员解析。普通文件的增减也会改变 `Package.files` 和「显式路径的目录下有受跟踪文件」这类成员条件，所以只要 `FileList` 变了就重算。重算只读新出现的清单；已有清单沿用上一轮的结果，只有 fs_watch 报告变化的才重读；成员解析本身是内存里的计算。当前 worktree 为此记着上一轮读到的 index 校验和与每个清单的结果（`Arc<Parsed>` 与解析缓存共用，不另存一份）。
  - 清单文件变了（批次里有这个路径，或它的某个上级目录：目录被删除、改名时 fs_watch 可能只报告目录）：重读这个文件，重做成员解析；内容的 blob OID 没变时解析缓存命中，子项目不变就不替换快照。不读 index。
  - `rescan`（事件丢失，或一批的路径超过 10 000 个）：按 `index` 变了处理，并当作全部清单都变了。
  - 任一 worktree 的 `HEAD` 变了：重新列 worktree（`lectern-git` 读单个 worktree 的 HEAD 也要先打开那个 worktree，与列一次的代价相当），快照里通常只有 worktree 列表变化；当前 worktree 的 `index` 通常会一起变，按上一条处理。unborn 分支的第一次提交只写 `refs/heads/<分支>`、不写 `HEAD`，所以快照里有 unborn 分支时 `refs` 的变化也重新列。
  - `worktrees/` 下有增减，或当前 worktree 的监听失败（根目录被删除、改名，卷被卸载）：重新列 worktree；当前 worktree 被删除或已 prunable 时切到主 worktree（没有时第一个可用的），按切换 worktree 的方式发布（先 `Pending` 的一代，`ChangedParts.active` 为 true），旧的 `WorktreeWatch` 随之 drop。列 worktree 失败时保留原来的列表。
  - 一轮刷新的结果（worktree 列表、文件列表、子项目）一次替换、发一个 `Changed`：文件列表与子项目来自同一轮，不像打开时那样先发 `packages: Pending` 的一代。内容没有变化时不替换，generation 不增加。
- 清单内容从工作区读（用户和 agent 看到的版本，未暂存的修改也算），缓存键是清单类型加这份字节的 blob OID（`hash_blob`，按仓库的哈希算法；文件夹项目用 SHA-1；同样的字节按不同格式解析结果不同），值是 `Result<Arc<Parsed>, ManifestError>`：语法错误同样按 OID 缓存，`TooLarge` 和 `Unreadable` 没有可哈希的内容，不缓存。缓存在进程内、所有项目和 worktree 共享，按字节 LRU，上限 2 MB（[ADR 0017](../adr/0017.md)「内存的具体约束」）。切换 worktree 时，各分支内容相同的清单直接命中。
- 非当前 worktree 的子项目（`resolve_package`）：按需计算，缓存按 worktree 的 git 目录存放，内容是子项目列表、算出它时的 index 校验和，以及参与计算的每个清单的 stat 指纹（路径、大小、修改时间、inode）。缓存不存文件列表，最多 16 个 worktree，按最近使用淘汰；只有 `resolve_package` 读它，`locate` 不读。每次调用都校验缓存：
  - 先读 index 校验和（`TreeAccess::index_checksum`，只读文件末尾）。变了就重读 index 并重算整份列表（文件列表变了，成员条件和 `Package.files` 都可能变）。
  - 没变就对指纹里的每个清单做一次 `lstat`（`std::fs::symlink_metadata`，不跟随符号链接）。有任何不一致（未暂存的修改、删除）时按未命中处理：重读 index 并重算整份列表，因为成员解析要用文件列表判断没有清单的声明目录下有没有受跟踪文件，而缓存不存文件列表。重算时内容 blob OID 没变的清单不重新解析。
  - 都一致才直接返回缓存的结果。
  - 同一个 worktree 的并发请求合并成一次计算。
  - 不在缓存检查范围内的变化：index 里没有的新清单在 `git add` 之前本来就不算（与当前 worktree 相同）。修改时间精度内、大小不变的连续写入可能漏掉一次，下次 stat 变化时纠正。
- 切换当前 worktree：先发布 `files: Pending` 的新快照（视图立即显示骨架），旧 worktree 的 `FileList` 随旧快照释放，只保留它的元数据。
- 插件停用时，kernel 丢弃扫描任务；视图手里的快照仍然有效，只是不再更新。
- 每个项目的请求（打开、再次打开、切换 worktree）交给它的扫描循环：循环在运行时只记下最新的请求（新的目标覆盖旧的，新的 worktree 列表保留），每一步之间检查，有新请求就放弃当前这一轮的结果。`set_active_worktree` 指向当前 worktree、而且没有排队的请求时直接忽略。
- 归位的边界：当前 worktree（或文件夹项目）的子项目为 `Unavailable` 时，`locate` 给出 `PackageMatch::None`，`resolve_package` 对当前 worktree 返回 `Err` 带同一个 `ScanError`。`resolve` 遇到不在仓库里的路径时，项目是包含它的最深的已打开文件夹项目，没有时是这个目录本身（文件取所在目录）；路径在 git 目录里时 `worktree` 为 None、`rel` 为 ""，打开这样的路径时 `opened_at` 也是 ""。

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
- 模型（`open_*`、`worktrees_*`、`locate_*`、`resolve_*`）：用 `testing::FakeGit`（仓库、worktree 列表、HEAD 与 index 在内存里，worktree 根目录与工作区文件是临时目录里的真实文件，路径能规范化、`lstat` 有效；记录每次调用），任务交给测试里的队列，由测试逐个运行，事件与 facts 记录下来。场景：在子目录打开、同一仓库从另一个 worktree 再打开得到同一个键、裸仓库、主目录 dotfiles 仓库、prunable（根目录已删除，键仍来自管理目录）/ locked / detached / unborn、文件夹项目的上限与跳过规则、`locate` 取最深的子项目，以及父仓库和其中单独打开的子模块都包含路径时 `locate` 返回子模块的项目；路径在非当前 worktree 时 `locate` 返回 `NotLoaded`，`resolve_package` 按那个 worktree 自己的子项目给出结果（它比当前 worktree 多一个、少一个、移动了一个子项目各一例），index 与清单都未变时第二次调用命中缓存、不读清单；未暂存地修改或删除那个 worktree 的一个清单后，下一次 `resolve_package` 按未命中重算并给出新结果，其余清单不重新解析；已经填过缓存之后 `locate` 对非当前 worktree 仍返回 `NotLoaded`；当前 worktree 的子项目还是 `Pending` 时 `resolve_package` 返回 `NotLoaded`、不做 IO。
- 刷新（`refresh_*`）：用 fs_watch 的模拟后端驱动事件（`testing::manual`、`FakeBackend`，`FakeGit` 每个 worktree 的 `FakeSource`），模型的任务仍交给测试里的队列；连续事件合并为一轮、扫描中到达的事件触发下一轮、只改普通文件内容不重读 index、新增普通受跟踪文件后 `Package.files` 更新而已有清单不重新解析、清单 OID 未变不重新解析、删除当前 worktree 后的切换、generation 严格递增；另有打开后第一批不替换快照、只刷新 stat 信息的 index 改写、目录事件代表其下的清单、根目录被删除后切换、其他 worktree 的 HEAD 只更新列表、unborn 分支的第一次提交、关闭的项目与文件夹项目不监听。
- facts（`facts_*`）：模式语义逐条覆盖；多个项目的并集；不合法的模式返回 false。
- 插件（`plugin_*`）：用真实的 `Kernel`（kernel 的 `test-support`）和 `Executor::manual()`，`PluginSet` 里另放一个依赖 `dyn Projects` 的测试插件（取得服务、记录事件）。`TestKernel` 只容纳被测插件和模拟服务，放不下 `Workspace` 激活的测试插件。`activate` 之后、`open` 之前 `FakeGit` 没有收到任何调用；`open` 之后提交的 facts 让一个 `Workspace(["pom.xml"])` 的测试插件激活；事件顺序为 `Opened` → `Changed`；一次刷新从后端交出事件到快照替换之前 kernel 不静止。`ProjectDeps::watcher` 用 fs_watch 的手动监听器，它有自己的 `Executor::manual()`。
- 真实仓库（`real_repo_*`）：用 git 的 `test-support` 构造器写出的仓库（含 linked worktree、SHA-256 仓库各一个）跑一遍 open、再次打开、切换 worktree 和 `resolve_package`，确认 `GitAccess` 的生产实现与 `FakeGit` 给出相同的快照；增量刷新（生产实现的 `watch_source` 加 `FakeBackend`）在一次 index 变化和一次未暂存的清单修改之后，事件与快照也与 `FakeGit` 相同。

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
   - 第 1 步在 `project` 里定义的数据类型（`FileList`、`FileListBuilder`、`FileKind`、`Package`、`Manifest`、`ManifestKind`、`Ecosystem`、`ManifestError`、`RelPath`）移到 `services` 的 `project` 模块，`lectern-project` 重新导出，原有路径不变。`manifest::Parsed`、`Member`、`Exclude` 和 `resolve_packages` 留在 `project`。
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

## 实际验证

第 1 步（清单解析与成员解析），在 Linux 上运行（本 crate 不依赖 GPUI，macOS 上是同一份代码；没有在 macOS 上实测）：

- `just check`（格式、架构规则、文档链接、clippy `-D warnings`）、`just test -p lectern-project`（48 个测试）、`just doctest`、`just deny` 全部通过。新增依赖的许可证：`serde`、`serde_json`、`toml` 及其传递依赖为 MIT / Apache-2.0，`quick-xml`、`winnow`、`zmij` 为 MIT，`memchr` 为 Unlicense / MIT，都在 `deny.toml` 的允许列表里；Cargo.lock 里新出现的只有 `quick-xml` 0.42（0.41 由 GPUI 在 Linux 上经 `xcb` 引入，不进 macOS 的发布包）。
- `cargo tree -p lectern-project -e normal --depth 1` 只列出 `glob`、`quick-xml`、`serde`、`serde_json`、`toml`。
- `rg -n 'std::process|Command::new' crates/project/src` 没有结果。
- 「清单格式」表逐行有 insta 快照（`crates/project/src/manifest/snapshots/`，样本在 `crates/project/tests/fixtures/manifests/`）：package.json（npm 数组写法含 `!` 排除、yarn 1 对象写法、普通包）、pnpm-workspace.yaml（pnpm 10 带 catalog 与注释、pnpm 7 与键同缩进的序列、单行与跨行流序列、锚点与嵌套集合标为 partial）、Cargo.toml（虚拟清单与 `exclude`、包加工作区、普通包）、pom.xml（4.0.0 含 profile 里的 modules 与指向 pom 文件的模块、4.1.0 的 subprojects 与 CDATA、从 parent 取 groupId、属性插值的模块标为 partial）、settings.gradle（Groovy：多参数跨行、`includeFlat`、`projectDir`、`includeBuild`，注释和字符串里的 `include` 不算）、settings.gradle.kts（Kotlin）、变量与插值写法标为 partial、go.work 块与单行写法、go.mod。build.gradle 不读内容：`manifest_gradle_build_is_not_read`。
- 错误：`manifest_syntax_errors_report_the_line`（每种格式一个样本，检查行号）、`manifest_syntax_errors_inline`、`manifest_too_large_is_not_parsed`（每种格式超过 1 MB 都是 `TooLarge`，正好 1 MB 照常解析）、`manifest_invalid_utf8`、`manifest_utf8_bom_is_skipped`。
- 不 panic：`manifest_garbage_input_does_not_panic` 把每个样本在每个字节处截断后解析，再做 4000 轮随机变异（替换、插入、删除语法字符）和随机字节输入，每种格式都跑；`manifest_deep_nesting_does_not_overflow` 对每种格式喂 10 万层 `[`、`{`、`(`、`<a>`、`"${`、`- `。手写解析器对输入不递归（Gradle 插值嵌套超过 32 层按语法错误处理），对恶意输入保持线性。
- 成员解析的快照（`crates/project/src/packages/snapshots/`）覆盖「测试」一节列出的场景：`packages_cloudprint_shape`（`apps/miniapp-console/frontend` 为 `declared: false`）、`packages_nested_workspaces`、`packages_one_directory_several_manifests`、`packages_gradle_projects_without_build_files`、`packages_manifest_errors_are_kept`、`packages_maven_modules_outside_their_directory`、`packages_paths_outside_the_worktree_are_dropped`、`packages_sample_directories_are_not_packages`、`packages_cargo_virtual_root_and_exclude`（根子项目的名字取 `root_name`）、`packages_only_regular_present_manifests_are_candidates`（符号链接、已删除的清单不是候选）、`packages_gradle_build_scripts_are_never_read`（回调只收到 settings 脚本，build 脚本按存在计入）；每个场景都检查「子项目与清单」的不变量。`cloudprint` 形状按本文的描述合成，没有用真实仓库的数据。

没有验证到的：「性能」一节的 `project/packages_cloudprint` 等基准属于第 5 步。

第 2 步（服务接口），在 Linux 上运行（`services` 与 `project` 不依赖 GPUI）：

- `just check`、`just test`（Linux 上不依赖 GPUI 的全部 crate）、`just doctest`、`just deny`、`cargo run -q --locked -p lectern-doc-check` 全部通过；`python3 scripts/check_architecture.py` 通过，`cargo tree -p lectern-services -e normal` 只有 `lectern-kernel` 和 `serde`（及 `serde_derive` 的过程宏依赖）；工作区的 `serde` 加了 `rc` feature，`lectern-project` 的测试与快照不受影响。
- `rg -n 'pub trait Projects' crates/services/src` 有一处（`crates/services/src/project/mod.rs`）。
- `just test -p lectern-services`（10 个测试）：`projects_service_id_and_multiple` 检查 `<dyn Projects as Service>::ID == "projects"`、`MULTIPLE == false`（编译期断言），以及 `service::<dyn Projects>()` 的键；`projects_is_a_shared_trait_object` 用一个空实现确认 `Arc<dyn Projects>` 可用、快照与查找结果是 `Send + Sync`；`project_event_shape` 构造 `ProjectEvent` 的每个变体，确认它实现 `Event` 和 `Clone`（`TestKernel::emitted` 需要）、`ChangedParts::default()` 全为 false；`project_keys_compare_by_kind_and_path`；`manifest_kinds_by_file_name_and_ecosystem`；`project_snapshot_round_trips`（有两个 worktree、文件与子项目都就绪、含清单错误的 git 快照，`Unavailable` / `Pending` 的文件夹快照，以及 `Location`、`Resolved`，经 serde_json 往返后相等）；`project_events_round_trip`（`ProjectEvent` 的每个变体，含成功与失败的 `Opened`）；`file_list_deserializes_through_the_builder`（乱序、重复、不合法的条目反序列化后满足 `FileList` 的不变量）；以及随 `FileList` 移过来的 `files_range_under_covers_only_the_directory`、`files_builder_sorts_dedups_and_skips_invalid_paths`。
- `just test -p lectern-project`：第 1 步的其余 46 个测试照常通过，insta 快照没有变化（`Debug` 输出不含模块路径）。第 1 步的 48 个测试里有 2 个是 `FileList` 的单元测试，随代码移到 `services`，合计仍是 48 个。

第 3 步（项目模型与插件），在 Linux 上运行（本 crate 不依赖 GPUI；没有在 macOS 上实测）：

- `just check`（格式、架构规则、文档链接、clippy `-D warnings`）、`just test`（Linux 上不依赖 GPUI 的全部 crate，671 个测试通过、6 个跳过）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都 ok）全部通过。没有新的外部依赖：`lectern-project` 新增的依赖都是工作区里的 crate（`lectern-kernel`、`lectern-git`、`lectern-executor`、`lectern-platform`），`tempfile` 只在 `test-support` 下。
- `Cargo.toml` 标记 `plugin = true`；`python3 scripts/check_architecture.py` 通过（30 个 crate），没有其他 crate 依赖 `lectern-project`。`cargo tree -p lectern-project -e normal | rg gpui` 没有结果；`rg -n 'gix' crates/project/Cargo.toml` 没有结果；`rg -n 'std::process|Command::new' crates/project/src` 没有结果。
- `just test -p lectern-project`：98 个测试，第 1、2 步的 46 个照常通过，新增 52 个：
  - `open_*`（17 个，含解析缓存的 LRU 单元测试）：在子目录打开得到整个仓库、`opened_at` 为子目录（insta）；事件为 `Opened` → `Changed [files]` → `Changed [packages]`；扫描前快照为第 1 代、`Pending`；从 linked worktree 再打开得到同一个键并切换（insta）；再次打开重读 index；裸仓库取第一个未 prunable 的 linked worktree、名字去掉 `.git`（insta）；没有 worktree 的裸仓库 `active` 为 None，`Opened` 后仍有一个 `Changed`；打开公共目录或 linked worktree 的管理目录得到那个 worktree；主目录 dotfiles 仓库下的目录按文件夹打开，主目录本身按仓库打开；不存在、不是目录、仓库读不出时 `Opened` 带错误且不建项目；index 读不出时 `files`、`packages` 为 `Unavailable(IndexUnreadable)`、worktree 列表照常；子模块是 `Submodule` 文件条目、不是子项目（insta）；文件夹遍历的跳过规则、点文件、可执行位、符号链接（insta），上限截断且结果确定；关闭立即移除快照、`Closed` 来自 executor；扫描排队时关闭，扫描不再读 index、只有 `Opened` 与 `Closed`。
  - `worktrees_*`（4 个）：主 worktree 在前、其余按根路径排序，locked / detached / unborn / prunable（根目录已删除，键仍是管理目录、根取字面路径）（insta）；切换时先发 `Pending` 的一代再发文件与子项目（insta）；切到 prunable、未知或当前 worktree 时没有事件也没有 git 调用；扫描结束前切回去，最新的请求生效。
  - `locate_*`（6 个）：取最深的子项目，`apps/web-old` 不算在 `apps/web` 里；不在任何子项目里为 `None`；子项目 `Pending` 时 `NotLoaded`；非当前 worktree 为 `NotLoaded`，填过 `resolve_package` 的缓存后仍是；父仓库和其中单独打开的子模块都包含路径时返回子模块的项目；文件夹项目。
  - `resolve_*`（8 个，含 16 个 worktree 的缓存淘汰单元测试）：不打开就能归位（主 worktree、linked worktree、git 目录）；仓库之外是已打开的最深文件夹项目或目录本身，不存在为 `NotFound`；`resolve_package` 按各 worktree 自己的子项目给出结果（多一个、少一个、移动了一个各一例）；index 与清单都未变时第二次调用只有 `OpenWorktree`、`IndexChecksum` 两次 git 调用、不读清单；未暂存地修改一个清单后按未命中重算、只多解析这一个，未暂存地删除一个清单后结果随之改变、不重新解析；当前 worktree 的子项目 `Pending` 时返回 `NotLoaded`、不做 git 调用；未知、prunable 的 worktree 和未打开的项目为 `RootMissing`。
  - `facts_*`（8 个）：模式语义逐条（任意深度的文件名、`*.ext`、`**/`、含 `/` 的从根匹配、`*` 不跨 `/`、大小写敏感、只匹配文件不匹配目录和子模块、多个文件列表的并集、不合法的模式为 false、结果缓存）；打开、切换、关闭后提交的 facts 随之变化；多个打开项目（含文件夹）的并集；一份文件列表只提交一次。
  - `plugin_*`（5 个）：真实 `Kernel` 加 `Executor::manual()`；激活之后、`open` 之前 executor 里没有任务、`FakeGit` 没有收到调用；`open` 之后提交的 facts 让 `Workspace(["pom.xml"])` 的测试插件激活，没有 `pom.xml` 时不激活；依赖方收到的事件为 `Opened` → `Changed` → `Changed`、generation 递增；停用 `project` 后依赖方回到等待依赖、服务引用不再准入。
  - `real_repo_*`（4 个）：`TestRepo` 写出的 SHA-1 与 SHA-256 仓库（主 worktree 在分支上、linked worktree 分离 HEAD、含可执行文件）经生产实现打开、切换 worktree、对非当前 worktree 调 `resolve_package`，渲染出的快照与同样布局的 `FakeGit` 逐字相同；再次打开后新暂存的 `go.mod` 出现在子项目里（insta）；`resolve` 与打开 linked worktree 的管理目录；裸仓库没有 worktree 时 `active` 为 None、名字为 `repo`。
- 事件顺序与 generation 严格递增由测试里的 `check_event_order` 对 open、worktrees、plugin、real_repo 的事件逐一检查。

没有验证到的（第 3 步）：

- `project/*` 基准属于第 5 步。`resolve_package` 命中缓存时生产实现先经 `Repo::worktree` 打开句柄（两次 gix 打开加一次 `SafeRoot` 打开）再读校验和，「性能」表里 < 1 ms 的预算要到第 5 步实测；超标时在本 crate 里为非当前 worktree 保留只读校验和用的句柄（不调 `tracked`，不占 index 内存）。
- 增量刷新（`refresh_*`、`watcher`、`watch_source`）属于第 4 步。

第 4 步（增量刷新），在 Linux（x86_64 容器）上运行（本 crate 不依赖 GPUI，也没有平台相关代码；没有在 macOS 上实测）：

- `just check`（格式、架构规则、文档链接、clippy `-D warnings`）、`just test`（Linux 上不依赖 GPUI 的全部 crate，968 个测试通过、1 个跳过）、`just doctest`（7 个文档测试）、`just deny`（advisories、bans、licenses、sources 都 ok）全部通过；另外 `cargo clippy -p lectern-project -- -D warnings`（不开 `test-support`）和 `--features test-support` 也通过。没有新的外部依赖：Cargo.lock 只多了 `lectern-project` 对 `lectern-fs-watch` 的一条工作区内部依赖。
- `cargo tree -p lectern-project -e normal --depth 1` 列出 `glob`、`lectern-executor`、`lectern-fs-watch`、`lectern-git`、`lectern-kernel`、`lectern-platform`、`lectern-services`、`quick-xml`、`serde`、`serde_json`、`toml`；`cargo tree -p lectern-project -e normal | rg gpui`、`rg -n 'std::process|Command::new' crates/project/src`、`rg -n 'gix' crates/project/Cargo.toml` 都没有结果；`python3 scripts/check_architecture.py` 通过（30 个 crate，`lectern-fs-watch` 是基础设施层的库 crate，符合规则 7）。
- `just test -p lectern-project`：115 个测试，第 1–3 步的 98 个照常通过（`plugin_*` 的 `ProjectDeps` 加了手动监听器，快照没有变化），新增 17 个；连续运行 20 次结果相同，`plugin_*` 另外单独连续运行 40 次。
  - `refresh_*`（15 个，`src/tests/refresh.rs` 13 个、`src/model/watch.rs` 的脏标记单元测试 2 个）：打开后每个订阅的第一批（rescan）列一次 worktree、读一次 index 校验和、重读清单但不重新解析，事件仍只有 `Opened` → `Changed [files]` → `Changed [packages]`，后端只 arm 公共目录和当前 worktree 根两个目标；20 ms 内的两批加上模型的那一轮运行之前又到的两批合成一轮（一次校验和、一次 index 读取、一个 `Changed [files, packages]`）；一轮读清单时（`FakeGit::before_read`）暂存新包并交付 index 事件，下一轮把它加进来（`Changed [packages]` 后接 `Changed [files, packages]`）；只改 `README.md` 和源文件内容时 `FakeGit` 没有收到任何调用、快照是同一个 `Arc`；新增受跟踪文件后 `Package.files` 加一、不读也不解析任何清单、facts 提交一次并包含新文件；只刷新 stat 信息的 index 改写（`FakeGit::touch_index`）读校验和与 index、不替换快照，之后校验和没变的事件只读校验和；同样字节重写清单只读不解析、没有事件，改成新内容只读并解析这一个、只有 `[packages]`，目录改名事件让其下的清单重读；`git worktree remove` 当前 worktree 后切到主 worktree（`[worktrees, active, files, packages]` → `[files]` → `[packages]`），后端改为监视主 worktree 根、不再监视被删的根；当前 worktree 的根被删除（`ROOT_CHANGED`）后列为 prunable 并切换（insta）；其他 worktree 的 HEAD 变化只有一次 `Worktrees` 调用和 `Changed [worktrees]`；unborn 分支第一次提交（只写 `refs/heads/main`）后 HEAD 更新，分支都已出生后 `refs` 变化没有任何调用；三轮 index 与 HEAD 变化加一次切换，generation 从 3 依次到 12、与快照一致；关闭项目后后端目标全部 drop，文件夹项目不 arm 任何目标；脏路径超过 10 000 个时改为 rescan，取出请求时只保留「重新列 worktree」。
  - `real_repo_refresh_matches_fake_git`：`TestRepo` 写出的仓库经生产实现（`Worktree::watch_source` 构造真实的过滤器与配置清单，后端是 `FakeBackend`）与同样布局的 `FakeGit` 并排运行，暂存 `svc/go.mod` 后的 index 事件、未暂存地改 `apps/web/package.json` 后的路径事件，两边的事件序列和渲染出的快照逐字相同。
  - `plugin_refresh_keeps_the_kernel_busy_until_applied`：真实 `Kernel`；事件进入 fs_watch 队列之后、批次交付之后、刷新任务运行之前 kernel 都不静止，任务运行完后静止，依赖方收到 `Changed [files, packages]`。
- 测试只用合成的夹具（`FakeGit`、`TestRepo`、fs_watch 的 `FakeBackend` / `FakeSource`），不调用 git 命令行、不访问网络。

没有验证到的（第 4 步）：

- 性能：「性能」表的 `project/refresh_index` 等基准属于第 5 步，这一步没有测量刷新的耗时。打开后的第一批会重读全部清单（`cloudprint` 形状约 25 个，10 万文件的形状 2,000 个），只在打开后发生一次，不在「打开到文件列表就绪 / 子项目就绪」的计时之内；第 5 步实测，超标时改为按清单的 stat 指纹（与 `resolve_package` 的缓存相同）只重读变化的。
- 常驻内存：当前 worktree 的句柄（index 的 `TrackedPaths`，与过滤器共用）和每个清单结果的表随监听保留，没有测量；第 5 步按「性能」表的常驻内存一行测。
- macOS：真实的 FSEvents 后端和 `app` 的装配（fs_watch 验收第 4 步）还没有接上，`app` 里没有构造 `ProjectDeps`；FSEvents 的延迟、根目录被删除时的 `RootChanged` 是否如 `FakeBackend` 那样到达，由 fs_watch 第 3、4 步的 macOS CI 验证。

## 与设计的差异

第 1 步：

- 数据类型暂时定义在 `project`：设计把 `Package`、`Manifest`、`FileList` 等放在 `services`，但 `services` 是第 2 步。第 2 步把它们移过去，`project` 重新导出（已写进「验收」第 2 步）。
- 版本：`toml` 用 1.1、`quick-xml` 用 0.42，不是「依赖」表原来写的 0.9 与 0.38。按「版本取实现时 crates.io 上的最新兼容版本」，这两个是实现时的最新版本；`toml` 1.1 已经在 Cargo.lock 里（GPUI 的构建依赖），不多一份。`serde` 需要 `derive`，表里补上了。
- `Parsed` 的字段写全（上文「本 crate 的公开项」）：增加 `group`（「清单格式」表要求读 `groupId`）和 `workspace`（`Package::workspace_root` 要按「清单声明了工作区」判断，空的 `[workspace]` 或 `<modules/>` 也算，只看 `members` 是否为空判断不出来）。`Member`、`Exclude` 两个枚举是新增的。
- `FileList` 增加 `builder()`、`is_empty()`、`find()` 和 `FileListBuilder`：成员解析的测试要在内存里构造文件列表，第 3 步从 index 构造也用它；`find` 用来判断 Maven 模块是不是指向 pom 文件。
- `resolve_packages` 的回调类型写成 `ManifestResult` 别名（clippy 的 `type_complexity`），类型不变。
- 层级不用栈，改为逐级查上级目录：字节序下 `a-b` 排在 `a` 和 `a/c` 之间，栈算出的 `a/c` 没有 `parent`（`packages_parent_skips_byte_order_neighbours`）。
- 设计没有写到、实现时定下的规则，都已写进上文「清单格式」与「成员解析」：package.json 的 `!` 排除（npm 的 workspaces 也支持取反模式）；pnpm 流序列可以跨行；含通配符的 glob 不匹配根目录（否则 `"workspaces": ["*"]` 会把根声明为自己的成员）；npm 与 pnpm 的通配符不匹配点目录；Cargo 不含通配符的 `members` 优先于 `exclude`；不合法的 glob 让清单标为 partial；Maven 模块指向文件时取它的目录；绝对路径丢弃；各格式算作 `Syntax` 的情形与宽松解码。
- Gradle `include ':a:b'` 只声明 `a/b`，不声明 Gradle 隐式创建的上级项目 `:a`，与「清单格式」表一致；`:a` 的目录有自己的清单时仍按未声明的候选收入。

第 2 步：

- `ManifestKind::from_file_name` 随 `ManifestKind` 移到 `services`：固有方法只能定义在类型所在的 crate。原来写在「本 crate 的公开项」里，已改到「子项目与清单」；调用路径 `ManifestKind::from_file_name` 不变。
- `MAX_MANIFEST_BYTES` 定义在 `services`，`project::manifest` 重新导出、路径不变：`ManifestError` 的 `Display` 要用它，常量只保留一份。
- 设计没有写 `OpenError`、`ScanError` 等的标准派生，实现定为 `Clone`、`PartialEq`、`Eq`、`Hash`、`Debug`，测试和视图要比较它们；已写进「服务接口」。`FileList` 的序列化形式是新定的，写进了「文件列表」。
- `services` 的 `project` 模块分为 `files`（`FileList` 及其构造器，第 1 步原样移入）、`packages`（`Package`、`Manifest`、`ManifestKind`、`Ecosystem`、`ManifestError`）、`snapshot`（键、快照、`Location`、`Resolved`），全部从 `lectern_services::project` 导出；`Projects`、`OpenRequest`、`OpenError`、`ProjectEvent`、`ChangedParts` 在模块根。只是文件划分，接口不变。
- 第 2 步没有测试用的假 `Projects`：设计没有要求，消费方需要时（`project_home`、`agents` 的测试）在 `services` 的 `test-support` 里补。

第 3 步：

- `GitAccess` 改为「一轮扫描打开一个 worktree 句柄」（`open_worktree` 返回 `TreeAccess`，上文「依赖 git 的接口」已改）：`lectern-git` 的 index 和工作区读取都在 `Worktree` 句柄上，句柄会缓存最近一次的 `TrackedPaths`。按路径调用（`index_paths(git_dir)`、`read_worktree_file(root, ...)`）要么每读一个清单就重新打开一次句柄（约 25 次 gix 打开），要么在 `GitAccess` 里长期保留句柄（每个 worktree 一份 index 内容和一个目录描述符，违反「非当前 worktree 只留元数据」）。`index_paths` 换成直接建 `FileList` 的 `index_files`，不经过一份中间的路径列表。
- `watch_source` 和 `ProjectDeps::watcher` 留到第 4 步：它们只服务于增量刷新，`lectern-fs-watch` 在 main 上还只是骨架。第 3 步不依赖 `lectern-fs-watch`，打开、再次打开、切换 worktree 时整轮扫描。
- 依赖多了 `lectern-platform`：文件夹项目没有 `Worktree` 句柄，读清单用 `SafeRoot`（与 git 同一套安全读取），已加进「依赖」表；它是基础设施层的库 crate，符合规则 7。
- 解析缓存的键是清单类型加 blob OID，不只是 OID：同样的字节（如空文件）作为不同格式解析结果不同。已改「数据流与线程」。
- `ProjectPlugin::with_fake_git`（`test-support`）是新增的，`testing` 模块在 `cfg(any(test, feature = "test-support"))` 下；合成仓库的形状随第 5 步。
- 模型测试不经 kernel：`Model` 通过一个内部的 `Host`（提交任务、发事件、提交 facts）连接 kernel 与 executor，测试用一个由测试逐个运行的队列，结果确定；`plugin_*` 用真实 `Kernel` 而不是 `TestKernel`（`TestKernel` 只容纳被测插件和模拟服务，放不下 `Workspace` 激活的测试插件）。`FakeGit` 的 worktree 根目录与工作区文件是真实文件（路径要能规范化，`resolve_package` 的缓存检查要 `lstat`），仓库元数据和 index 在内存里。已改「测试」。
- 设计没有写到、实现时定下的规则，已写进上文「事件」「文件列表」「数据流与线程」：第 1 代快照与每次替换都发 `Changed`；再次打开重扫；`Closed` 之后没有 `Changed`；请求在扫描循环里合并、最新的生效；`set_active_worktree` 对当前 worktree 的忽略条件；子项目 `Unavailable` 时 `locate`、`resolve_package` 的结果；`resolve` 对仓库之外与 git 目录里的路径；文件夹遍历的顺序、计数和不收入的条目。`lectern-git` 不产生 `Head::Unknown`，写进了「依赖 git 的接口」。

第 4 步：

- `watch_source` 放在 `TreeAccess` 上，不在 `GitAccess` 上：它必须来自 `open_worktree` 打开的同一个句柄（上文原来就这样要求），句柄方法最直接。`TreeAccess` 因此要求 `Send + Sync`。已改「依赖 git 的接口」。
- 有监听的当前 worktree 不再「一轮扫描一个句柄」：句柄随 `WorktreeWatch` 保留，刷新和同一 worktree 的再次扫描都用它。fs_watch 的来源本来就持有这个句柄（及其 `TrackedPaths`），每轮另开一个句柄只会让每次 index 变化解析两遍 index（fs_watch 重建过滤器一遍、本 crate 一遍），内存里同时有两份。没有监听器时（模型测试）仍按第 3 步每轮打开。`FakeGit` 的句柄改为按调用时的内存状态回答（原来是打开时的副本），否则保留的句柄看不到之后的暂存。已改「依赖 git 的接口」。
- HEAD 的变化改为重新列 worktree，不是只读那一个 worktree 的 HEAD：`lectern-git` 读单个 HEAD 要先 `Repo::worktree` 打开那个 worktree（要求根目录存在、打开 `SafeRoot`），与 `Repo::worktrees` 列一次的代价相当，而列一次同时得到 locked、prunable 的变化。快照里通常仍只有 `worktrees` 一项变化。已改「数据流与线程」。
- 设计没有写到、实现时补上的触发条件，已写进「依赖 fs_watch 的接口」「数据流与线程」：快照里有 unborn 分支时 `refs` 变化也重新列 worktree（第一次提交只写 `refs/heads/<分支>`，`HEAD` 不变，否则 `Head::Unborn` 一直不更新）；当前 worktree 的监听 `Failed`（根目录被删除而没有 `git worktree remove`，`worktrees/` 下没有变化）也重新列，于是它变成 prunable、切到主 worktree；`rescan` 按 index 变化且全部清单都变了处理；每个订阅的第一批（rescan）让项目做一次这样的检查，覆盖扫描与 arm 之间的变化（fs_watch「监听器与句柄」的要求）。
- 一轮刷新只替换一次快照，文件列表与子项目一起发布，不像打开、切换时先发 `packages: Pending` 的一代：刷新时旧的子项目仍然有效，中间插一代 `Pending` 只会让侧栏的 Packages 闪一下。内容没变（同样的路径、同样的子项目）时不替换。已改「数据流与线程」。
- 「已有清单沿用以 blob OID 为键的解析结果」的具体做法：当前 worktree 记着上一轮每个清单的结果和 index 校验和（随监听保存，不进快照），没有被报告变化的清单直接取上一轮的结果，不读文件、不算 OID；被报告变化的读文件后仍先查解析缓存。设计原来只写了「沿用解析结果」，没有说怎样不重读文件。
- 请求与刷新在同一个循环里：请求优先，取出请求时清掉当前 worktree 的脏标记（完整扫描会重读），刷新途中有新请求时放弃这一轮、脏标记并回去。设计只写了「运行期间来的事件只标记脏」。
- `Host` 多了 `busy()`（`Sink::busy` 用，插件里是 `PluginHandle::busy()`，测试队列返回 None）；`Model::watch_with` 在 `activate` 里设置监听器。都是 crate 内部的，公开接口只有 `ProjectDeps::watcher`。
- 测试辅助：`FakeGit` 多了 `source`（这个 worktree 的 `FakeSource`，跟踪路径随 `track` / `untrack` / `write` 更新）、`remove_worktree`（`git worktree remove`）、`touch_index`（只改校验和、不改路径）、`before_read`（下一次读某个文件之前运行一个闭包，用来在一轮扫描中途制造变化）；`test-support` 打开 `lectern-fs-watch/test-support`。测试里的 `Watch` 把 fs_watch 的手动监听器和模型的任务队列交替跑到都没有工作。已改「测试」。
- 快照不带监听的健康状态：fs_watch「状态与异常」说 arm 失败时由视图显示「不再自动刷新」，但 `ProjectSnapshot` 没有对应的字段，视图拿不到这个状态。第 4 步不改 `services` 的接口：`Failed` 只触发重新列 worktree，`Degraded` 不处理。等视图（`project_home`）需要时在 `services` 里加字段。
