# review 设计

状态：实现中。前置条件 A（`services` 的 `changes`、`review` 模块，[#161](https://github.com/SoloJiang/lectern-app/pull/161)）、B（`git` 的 `line_diff`、`head_reflog`、`mtimes`、`tree_changes`，[#162](https://github.com/SoloJiang/lectern-app/pull/162)）、C（`git` 的 `LocalChanges`，[#165](https://github.com/SoloJiang/lectern-app/pull/165)）、G（`SessionSource::turns_unretained`，[#164](https://github.com/SoloJiang/lectern-app/pull/164)）已实现；其余前置条件与第 1–5 步尚未实现。

对应 issue [#15](https://github.com/SoloJiang/lectern-app/issues/15)（审阅视图的领域部分），审阅意见 [#22](https://github.com/SoloJiang/lectern-app/issues/22) 只预留数据模型。`review` 把一个审阅对象（会话、会话外的改动、worktree 相对某个基准的比较）整理成审阅视图读取的不可变快照：哪些文件改了、每一处改动来自哪个会话的哪一轮、按轮次的改动叙事、已看进度、Drifted 标记和规则生成的会话简报（[交互规范「核心闭环」](interaction.md#核心闭环)、[ADR 0012](../adr/0012.md)、[ADR 0019](../adr/0019.md)）。用户的已看标记写进按仓库分开的标注日志（[ADR 0044](../adr/0044.md)）。

## 范围

本 issue 涉及的 crate：

| crate | 层 | 内容 |
|---|---|---|
| `review`（本 crate） | 领域，插件 | 审阅对象与基准、改动链与逐行来源、归因与 Drifted、改动叙事、已看状态与标注日志、噪声文件与规则分组、规则简报、项目主页的概览、`dyn Review` 服务 |
| `services` | kernel | `changes` 模块（改动来源接口，ADR 0019）、`review` 模块（审阅服务、快照、命令、事件、简报规则的扩展点） |
| `git` | 领域，库 | 为本地 worktree 实现 `ChangeSource`（[git 设计「验收」](git.md#验收)第 4 步）；`line_diff`、`Worktree::head_reflog`、`Worktree::mtimes`、`Repo::tree_changes` |
| `platform`、`app` | 基础设施、入口 | 应用数据目录；构造本 crate 的插件 |

- 做：
  - 审阅对象：会话（含它在同一 worktree 里派出的子 agent 会话）、某个 worktree 里不属于任何会话的改动、worktree 相对一个基准的全部改动。
  - 会话的基准：会话开始时 HEAD 指向的提交，从 worktree 的 HEAD reflog 得到，第一次标记已看时写进标注日志固定下来。
  - 归因：把基准到工作区的每个改动文件、每个 hunk 对到会话和轮次。会话日志里的每次改动按时间顺序作用在基准内容上，得到一串版本（改动链），逐行记下每行来自哪一步；对不上的行不归因（Drifted）。
  - 改动叙事：每一轮的 prompt、agent 说明、这一轮对每个文件的改动（同一轮对同一文件的多次调用合成一步）、命令期间改动的文件、派出的子 agent。
  - 已看状态：三种状态（未看、已看、审过之后又被改过），键按 [pulls.review 调研「对 Lectern 的建议」](../research/pulls-review.md#对-lectern-的建议)；Looks good 一次标记全部。
  - 标注日志：已看标记和会话基准的持久化（ADR 0044）。
  - 噪声文件（lockfile、生成文件、快照）的模式列表，同时决定默认折叠、规则分组和模型输入里的占位；会话日志认不出或没有会话时按规则分组。
  - 规则简报：只用本地已有的信息生成「值得先看」（ADR 0019），并接收其他插件经 `Review::register_brief_rules` 登记的 `BriefRules` 提供者补充的条目。
  - 项目主页的概览：每个会话的已看进度和信号、每个 worktree 不属于任何会话的改动数（[ADR 0035](../adr/0035.md) 的计数比较）。
  - 展示行与正文：视图滚到哪个文件、哪一轮，本 crate 在后台准备好展示行、两侧文本和 prompt / 说明的正文。
- 不做：
  - 绘制、Fluent 文案、UI 探针节点、快捷键：`review_view`。V 的「跳到下一个未看文件」由视图按快照计算。
  - 调用 `CodeIntel::set_focus`：`review_view` 按快照里的 `focus_review`、`focus_touched` 调用（[code_intel 设计「接口」](code_intel.md#接口)的调用方一节留给 #15 决定）。
  - 简报里的分支用量：`review_view` 直接读 `usage` 的服务。
  - 按两侧窗口的语法高亮：`review_view` 调用 `syntax` 的纯函数（[file_reader 设计「范围」](file_reader.md#范围)）。
  - 读取、比较 git 对象和工作区：`git`。会话日志的解析：`agents`。
  - Analyze 的运行（查找并启动命令行、沙箱、凭据转发、快照）：[#66](https://github.com/SoloJiang/lectern-app/issues/66)，见「Analyze 的边界」。
  - MR 的改动来源：`forge`（[#25](https://github.com/SoloJiang/lectern-app/issues/25)），实现本文的 `ChangeSource`。
  - 提交与开 MR：[git 设计「未决问题」](git.md#未决问题)1，P1 不做。
- 本 issue 之外：
  - 审阅意见（#22）：行内写意见、攒批、发回会话。本文预留标注日志的记录类型、快照字段和锚点（见「#22 的预留」）。
  - Analyze 的校验与对账函数（#66）：接口在本文「Analyze 的边界」，实现与 #66 一起，写 `Refs #66`。
  - 「调用方未同步修改」「改到的公开接口」两条简报规则：要语法和符号信息，由 `code_intel` 实现 `BriefRules`（后续 PR，前置条件 F）。
  - 结构化 diff（[#21](https://github.com/SoloJiang/lectern-app/issues/21)）：建立在 `git` 的扩展点上，本 crate 的归因按行进行，不受影响。

## 依赖

本 crate 是插件 crate（`plugin = true`，[ADR 0025](../adr/0025.md)）：插件 "review"，只依赖 kernel 层和非插件的库 crate，只有 `app` 和 tool 层依赖它。其他插件和视图只经 `services::review` 读它的状态。

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-kernel` | `Plugin`、事件、`PluginHandle`、`BusyGuard` | 插件机制 |
| `lectern-services` | 使用 `Projects`、`SessionSource`；提供 `Review`（含 `BriefRules` 提供者的登记）；`ChangeSource` 与数据类型 | 插件之间只经 `services`（架构规则 7） |
| `lectern-git` | `Repo`、`Worktree`、`LocalChanges`（`ChangeSource` 的本地实现）、`line_diff`、`rows`、`inline_changes`、`head_reflog`、`tree_changes`、`mtimes`、`Oid::for_blob` | gix 只在 `git` 里使用 |
| `lectern-cache` | `review.attribution` 命名空间 | 派生数据以 blob OID 加轮次历史的摘要为键（架构规则 5） |
| `lectern-executor` | 后台任务 | UI 线程不做 IO |
| `lectern-fs-watch` | 被审阅 worktree 的 `WorktreeBatch`、仓库的 `RepoBatch` | 与 `project` 共用同一份 worktree 状态 |
| `serde`、`serde_json` | 标注日志的记录 | 已是工作区依赖（ADR 0044） |
| `sha2` | 轮次历史的摘要、标注日志的文件名 | 已在 Cargo.lock（`cache` 用同一版本） |
| `insta`、`tempfile`（dev）；`lectern-git` 的 `test-support`（dev） | 快照、临时目录、`TestRepo` | 工作区统一依赖 |

不依赖 `agents`、`project`、`code_intel`（插件之间不得依赖，dev-dependency 也算）：测试用本 crate `testing` 里的假会话来源和假项目服务。不依赖 `platform`：工作区只经 `git` 读取，应用数据目录由 `app` 传入。不新增外部依赖。

`executor`、`cache`、`fs_watch` 由 `app` 创建，构造插件时传入：

```rust
pub struct ReviewDeps {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: lectern_fs_watch::Watcher,
    /// 标注日志的目录：`app` 取 `platform` 提供的应用数据目录下的 `review/`（ADR 0044）。不存在时第一次写入才创建。
    pub data_dir: PathBuf,
    /// `GIT_CEILING_DIRECTORIES`，`app` 启动后从进程环境读一次（与 `project` 相同）。
    pub git_ceilings: Arc<[PathBuf]>,
}
/// 插件 ID "review"：provides `dyn Review`，requires `dyn Projects`、`dyn SessionSource`，激活条件 `AfterFirstFrame`。
/// 不 requires 简报规则的提供者：kernel 没有可选依赖，提供者反过来 requires `dyn Review` 并登记（「简报规则的扩展点」）。
pub fn plugin(deps: ReviewDeps) -> (Name, ReviewPlugin);
```

### 对其他 crate 的要求

以下改动各由对应 crate 的 PR 完成，写 `Refs #15`，在「验收」里列为前置条件。

- **A. `services`**：`changes` 模块和 `review` 模块，按本文「接口」。只有 trait、数据类型、服务 ID；例外是 `ChangeList::handle`（实现方放进去的不透明句柄，不序列化）和 `BriefReply`、`BriefRulesRegistration`、`OverviewInterest` 包着的回调，与 `ObjectShare`、`SemanticReply` 的做法相同。`crates/services/AGENTS.md` 的模块列表随本文加上这两个模块。
- **B. `git`**：四个函数（`ReflogEntry` 与 `parse_reflog_line` 已有），不新增依赖。

  ```rust
  /// 按行 diff 两段内容，算法与 hunk 缓存相同（Histogram），hunk 带两侧第一行的字节偏移。纯计算，任何线程；
  /// 不读对象库、不查缓存。任一侧超过 `max_bytes` 时返回 None。
  pub fn line_diff(old: &[u8], new: &[u8], max_bytes: u64) -> Option<FileDiff>;

  impl Worktree {
      /// 这个 worktree 的 HEAD reflog（`logs/HEAD`，git 目录里的文件，不经工作区）的末尾部分，从新到旧：
      /// 从文件末尾向前逐行解析（`parse_reflog_line`），读到第一条时间早于 `stop`（Unix 秒）的记录为止（含这一条），
      /// 至多 `max_entries` 条、4 MiB。阻塞。
      pub fn head_reflog(&self, stop: i64, max_entries: usize) -> Result<HeadReflog, GitError>;
      /// 每个路径在工作区里的修改时间（纳秒），经 `SafeRoot::stat`、不跟随符号链接；不存在或不是普通文件、符号链接时为 None。阻塞。
      pub fn mtimes(&self, paths: &[RepoPath]) -> Result<Vec<Option<i64>>, GitError>;
  }

  pub struct HeadReflog {
      /// 没有 reflog 文件（`core.logAllRefUpdates = false`）时为 false，`entries` 为空。
      pub exists: bool,
      /// 从新到旧。
      pub entries: Vec<ReflogEntry>,
      /// 读到了文件开头（没有因为 `max_entries` 或 4 MiB 停下，也没有更早的记录）。
      pub complete: bool,
  }

  impl Repo {
      /// 两个提交的树之间内容或模式不同的路径，按路径排序。子树 OID 相同时整棵跳过，不读 blob、不做重命名检测；
      /// `old` 为 None（reflog 里全 0 的旧值）时是 `new` 树里的全部路径。超过 `max_paths` 时返回 `All`。阻塞，检查 `cancel`。
      pub fn tree_changes(&self, old: Option<Oid>, new: Oid, max_paths: usize, cancel: &AtomicBool) -> Result<TreeChanges, GitError>;
  }

  pub enum TreeChanges { Paths(Arc<[RepoPath]>), All }
  ```

  测试 `line_diff_*`（与 `diff_trees` 对同一对内容给出的 hunk 和偏移相同）、`head_reflog_*`（归档夹具里的 reflog：`stop` 正好等于某条记录的时间、全部记录都晚于 `stop`、全 0 的旧值、没有 reflog、`max_entries` 与 4 MiB 的上限、末尾不完整的一行）、`tree_changes_*`（只改一个深层文件时其余子树不读；模式变化算改动；`max_paths` 时为 `All`；与 `diff_trees` 给出的路径集合相同，不含重命名配对）、`mtimes_*`（中间一级是符号链接时为 None）。[git 设计「范围」](git.md#范围)的「本 issue 之外」有一条指向这里。
- **C. `git` 第 4 步**：为本地 worktree 实现 `ChangeSource`，按本文「改动来源」的对照表转换 `ChangeSet`，`Cargo.toml` 加 `lectern-services`。这一步的接口与验收以本文为准，[git 设计「验收」](git.md#验收)第 4 步指向这里。
- **D. `platform` 与 `app`**：`platform` 提供应用数据目录（macOS 为 `~/Library/Application Support/` 下的应用目录，Linux 为 `$XDG_DATA_HOME` 或 `~/.local/share` 下的 `lectern/`，供 CI 测试），不创建目录、不做其他 IO；`app` 在启动屏障放开后用它构造 `ReviewDeps::data_dir`，放进 `PluginSet`。`usage` 的数据库以后用同一个目录（ADR 0044）。
- **E. `cache`**：[cache 设计「命名空间」](cache.md#命名空间)加 `review.attribution`、`review.tree_changes`、`review.claims` 三行（本 PR 已加）。不改代码。
- **F. `code_intel`（后续，不阻塞本 issue）**：实现 `BriefRules`，给出「调用方未同步修改」和「改到的公开接口」两条规则（见「简报规则」）。登记放在 `code_intel` crate 里另一个小插件（如 "code-intel-brief"）：它 requires `dyn Review` 和 `dyn CodeIntel`，在 `started` 里调 `Review::register_brief_rules`，把返回的登记交给 `PluginHandle::own`。不放进 "code-intel" 插件本身，否则 `review` 停用或失败时导航也随之停用。规则的判定、查询上限和预算由 code_intel 设计的修订规定。
- **G. `agents` 与 `services` 的 `session` 模块**：`SessionSource` 加一个不进内存 LRU 的读取，供概览派生声明（「概览」第 2 步）。接口按 agents 设计的修订，本文要求的行为：

  ```rust
  /// 与 `turns` 相同，但结果不放进 8 MB 的轮次索引 LRU：已在 LRU 里时照样复制 `Arc`；从 `cache` 读出或解析出的索引
  /// 照常写回 `cache`，返回后由调用方放下。阻塞，只在 executor 上调用。
  fn turns_unretained(&self, session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError>;
  ```

  ADR 0035 的采样状态不含这个 LRU；概览在采样状态里运行，用 `turns` 会把最多 50 个会话的索引留在 LRU 里（[agents 设计「性能」](agents.md#性能)：`turns` 保留索引）。测试 `turns_unretained_*`：调用前后 LRU 的字节数不变，结果与 `turns` 相同，`cache` 里写入了索引。

`agents` 其余不改：本 crate 用 `turns`（打开的审阅）、`load`、`SessionsChanged` 和 `Parent::tool_call`。`fs_watch` 不改：本 crate 是 [fs_watch 设计「对其他 crate 的要求」](fs_watch.md#对其他-crate-的要求)里写好的 `review` 一方。

## 概念

| 概念 | 说明 |
|---|---|
| 审阅对象（`ReviewTarget`） | 一次审阅看的范围：一个会话、一个 worktree 里不属于任何会话的改动，或一个 worktree 相对某个基准的全部改动。已看标记按审阅对象分开记录。 |
| 基准 | 改动集的旧一侧，总是一棵树。会话的基准是会话开始时 HEAD 指向的提交；会话外的改动用当前 HEAD。 |
| 路径的截止时刻 | HEAD reflog 里（到基准那条记录为止）最近一次让这个路径在旧、新两个提交之间不同的记录的时间。这之后的会话改动才参与这个路径的归因（「改动步与声明」）。 |
| 改动来源（`ChangeSource`） | 给出改动集和两侧内容的对象：本地 worktree（`git`）或 MR（`forge`，P3）。 |
| 改动步 | 一次改文件的工具调用（`ToolKind::Edit`）对一个路径的改动，来自 `load` 给出的 `FileEdit`。 |
| 改动链 | 一个路径上，基准内容依次应用各会话的改动步得到的一串版本。 |
| 行来源 | 版本里每一行来自哪里：基准、某一步，或「无来源」（链里没有，只出现在工作区里）。 |
| 声明 | 会话对一个路径的认领：有改动步改过它，或在会话运行命令期间被修改（文件级）。没有任何声明的改动是「会话外的改动」。 |
| Drifted | 工作区里没有来源的行，以及应用不上的改动步。不显示归因（[交互规范「状态与异常」](interaction.md#状态与异常)）。 |
| 已看标记 | 用户对一个审阅对象里一个文件的改动标为已看，键包含改动的两侧身份。 |
| 标注日志 | 已看标记和会话基准的追加日志（ADR 0044）。 |
| 噪声文件 | lockfile、生成文件、快照，按「噪声文件与规则分组」的模式列表识别。 |

## 接口

### 改动来源（`lectern-services` 的 `changes` 模块）

ADR 0019 要求 `services` 有按能力划分的改动来源接口，`git` 和 `forge` 实现它。`ChangeSource` 不注册为 kernel 服务：本地来源由本 crate 用 `git::LocalChanges::new` 构造（`git` 是库，[#47](https://github.com/SoloJiang/lectern-app/issues/47) 的结论见 ADR 0025）；MR 来源由 `forge` 的服务交出（#25）。本 crate 只经这个 trait 读改动集，P3 的 MR 不需要另一套归因和已看逻辑。

类型都派生 `Clone`、`PartialEq`、`Eq`、`Debug`、`Serialize`、`Deserialize`（外部标记的枚举），trait 和 `ListHandle` 除外。

```rust
pub trait ChangeSource: Send + Sync {
    /// 来源的种类与能力。不阻塞。
    fn info(&self) -> &SourceInfo;

    /// 必需：取改动集。阻塞（读对象库、index、工作区或代码平台），只在 executor 上调用。
    /// `cancel` 置位时尽快返回 `ChangeError::Cancelled`，不给出部分结果。
    fn fetch(&self, cancel: &AtomicBool) -> Result<Arc<ChangeList>, ChangeError>;

    /// 能力 `refresh`：在上一份改动集上按提示刷新，没有变化的条目与 `prev` 共享。没有这项能力的来源用默认实现。
    fn refresh(&self, prev: &Arc<ChangeList>, hint: &RefreshHint, cancel: &AtomicBool)
        -> Result<Arc<ChangeList>, ChangeError> { let _ = (prev, hint); self.fetch(cancel) }

    /// 必需：一个文件两侧的内容。阻塞。新一侧是这份改动集比较时读到的内容，不按路径重读；
    /// 超过 `max_bytes` 的一侧为 `TooLarge`。`file` 是 `ChangeList::files` 的下标。
    fn texts(&self, list: &ChangeList, file: u32, max_bytes: u64) -> Result<SideTexts, ChangeError>;

    /// 能力 `fingerprint`：审阅对象的指纹（ADR 0019 约束 9）。不阻塞。
    fn fingerprint(&self, list: &ChangeList) -> Option<[u8; 32]> { let _ = list; None }
}

pub struct SourceInfo { pub kind: SourceKind, pub caps: Capabilities }

pub enum SourceKind {
    /// 本地 worktree 的工作区（含未跟踪文件）相对一个基准。
    Worktree { project: ProjectKey, worktree: WorktreeKey, base: BaseSpec },
    // MR 由 forge 的设计加入（#25）。
}

/// 读写审阅评论、读取共享分析结果两项能力随 forge 的设计加入，方法有默认实现，不影响已有实现。
pub struct Capabilities { pub refresh: bool, pub fingerprint: bool }

pub enum BaseSpec {
    Head,
    /// 某个提交的树，如会话开始时的 HEAD。
    Commit(BlobId),
    /// `merge_base(HEAD, 分支)` 的树；完整引用名，如 "refs/heads/main"。
    MergeBase(Arc<str>),
}

pub enum RefreshHint {
    /// 只重新比较这些路径（目录表示其下全部路径）。
    Paths(Arc<[ChangePath]>),
    /// HEAD、index、忽略规则、属性或配置变了，或事件丢失。
    Full,
}

/// 不可变。
pub struct ChangeList {
    /// 同一个来源上每份新的改动集加一。
    pub generation: u64,
    /// 基准树。
    pub base: BlobId,
    /// 按（新路径的字节序，`untracked`）排序，与 `git::ChangeSet::files` 的顺序和不变量相同。
    pub files: Arc<[FileChange]>,
    pub totals: ChangeTotals,
    /// 实现方的句柄（本地来源放这份改动集对应的 `git::ChangeSet`，`texts` 和 `refresh` 用它）。不序列化，消费方不解读。
    #[serde(skip)]
    pub handle: ListHandle,
}

/// `Clone`、`Default`（None，反序列化得到的改动集就是它）。不是改动集的值的一部分：两个句柄比较时总是相等，
/// `Debug` 不打印内容，所以 `ChangeList` 的比较与 JSON 往返只看其余字段。
pub struct ListHandle(pub Option<Arc<dyn Any + Send + Sync>>);

/// 相对 worktree 根、`/` 分隔的原始字节（git 允许非 UTF-8 的名字）。不变量与 `git::RepoPath` 相同。
pub struct ChangePath(pub Arc<[u8]>);
impl ChangePath { pub fn as_str(&self) -> Option<&str>; }   // UTF-8 时为 Some

pub struct FileChange {
    pub status: ChangeStatus,
    /// 新路径；删除时为旧路径。
    pub path: ChangePath,
    pub old: Option<FileSide>,
    pub new: Option<FileSide>,
    pub detail: ChangeDetail,
    /// `.gitattributes` 的 `linguist-generated`。
    pub generated: bool,
    pub untracked: bool,
    /// index 里这个路径的条目与工作区一侧不同（`git::Change::staged`）。
    pub staged: Option<FileSide>,
}

pub enum ChangeStatus { Added, Deleted, Modified, TypeChanged, Renamed { from: ChangePath, similarity: u8 }, Conflicted }

pub struct FileSide { pub mode: FileMode, pub content: SideIdentity }
pub enum FileMode { File, Executable, Symlink, Gitlink, Special }

/// 一侧内容的身份。已看标记和缓存键按它比较。
pub enum SideIdentity {
    Blob(BlobId),
    /// 比较时算过 OID，但字节超过保留上限没有留下。
    NotRetained(BlobId),
    /// 超过工作区单文件读取上限，没有读内容。
    Unhashed { size: u64, mtime_ns: i64, inode: u64 },
    /// 使用了 clean / process 过滤器：原始字节的 SHA-256，或超过上限时的 stat。
    Filtered(RawIdentity),
    Unavailable(UnavailableKind),
}
pub enum RawIdentity { Sha256([u8; 32]), Stat { size: u64, mtime_ns: i64, inode: u64 } }
pub enum UnavailableKind { UnsupportedType, PathThroughSymlink, NestedRepository, Missing, Unreadable }

pub enum ChangeDetail {
    Text { hunks: Arc<[DiffHunk]>, added: u32, removed: u32 },
    Truncated { added: u32, removed: u32 },
    Binary { old_size: Option<u64>, new_size: Option<u64> },
    TooLarge, Listed, ModeOnly, StagedOnly, NoDiff,
}

/// 行号从 0 起，区间覆盖两侧完整文件；字节偏移是这一侧规范内容里第一行的偏移（git 设计「字节偏移」）。
pub struct DiffHunk { pub old: Range<u32>, pub new: Range<u32>, pub old_byte: u64, pub new_byte: u64 }

pub struct ChangeTotals {
    pub files: u32, pub added: u64, pub removed: u64,
    pub truncated: u32, pub too_large: u32, pub listed: u32, pub not_retained: u32,
    pub untracked_capped: bool,
    /// 不满足路径不变量、没有读取的路径（原始字节）。
    pub invalid_paths: Arc<[Arc<[u8]>]>,
}

pub struct SideTexts { pub old: SideText, pub new: SideText }
pub enum SideText {
    Absent,
    /// 规范内容。本地来源给出的就是 `git::Text::shared()` 那一份 `Arc`，`review_view` 用它登记 `Reader::share_object`（[ADR 0042](../adr/0042.md)）。
    Text(Arc<[u8]>),
    TooLarge { size: u64 },
    NotRetained,
    Unavailable,
}

pub enum ChangeError {
    /// 仓库格式不支持（`git::GitError::Unsupported`），附原因。
    Unsupported(Arc<str>),
    NoMergeBase,
    TooManyEntries,
    Io(Arc<str>),
    Cancelled,
}
```

本地实现（前置条件 C，在 `git` 里）：

```rust
impl LocalChanges {
    /// 不做 IO。`limits` 是 `git::Limits`，`project`、`worktree` 只用来填 `SourceInfo`。
    pub fn new(repo: Repo, worktree: Worktree, project: ProjectKey, key: WorktreeKey, base: BaseSpec, limits: Limits) -> LocalChanges;
}
impl ChangeSource for LocalChanges { /* caps: refresh、fingerprint 都为 true */ }
```

| `git` | `services::changes` |
|---|---|
| `Oid` | `BlobId`（同样的字节，SHA-1 / SHA-256） |
| `RepoPath` | `ChangePath`，共用同一个 `Arc<[u8]>`，不复制 |
| `Base::{Head, Commit, MergeBase}` | `BaseSpec` 的同名变体（引用名原样） |
| `Change`、`Status`、`Side`、`Mode`、`Detail`、`Hunk`、`Totals` | 同名的 `FileChange` 等，逐字段对应；`Content::{Blob, Unhashed, Unavailable}` 对应 `SideIdentity`，`Unavailable::{Filtered, NotRetained}` 提为 `SideIdentity::{Filtered, NotRetained}`；`mtime_ns` 从 `i128` 收窄为 `i64`（到 2262 年） |
| `Refresh::{Paths, Full}` | `RefreshHint` 的同名变体 |
| `ChangeSet::texts` 的 `SideText` | `SideText`；`Text` 取 `Text::shared()` |
| `ChangeSet::fingerprint` | `fingerprint` |
| `GitError` | `Unsupported`、`NoMergeBase`、`TooManyEntries`、`Cancelled` 同名；`ObjectMissing`、`Corrupt`、`Io`、`TooLarge` 为 `Io(原因)` |

`ChangeList::handle` 持有对应的 `Arc<ChangeSet>`，所以改动集保留的工作区内容随 `ChangeList` 释放；`refresh` 从 `prev.handle` 取上一份 `ChangeSet`，没有句柄时（不是本实现给出的）按 `Full` 重新比较。git 第 4 步的测试：`change_source_*` 对全部工作区夹具检查转换前后逐字段相同（insta）；`refresh` 后没变的 `FileChange` 与上一份共享 `Arc<[DiffHunk]>`；`texts` 的 `Text` 与 `ChangeSet::texts` 给出的 `Text::shared()` 是同一份（`Arc::ptr_eq`）；`fingerprint` 等于 `ChangeSet::fingerprint`；`cargo tree -i lectern-services -e normal` 里有 `lectern-git`。

### 审阅服务（`lectern-services` 的 `review` 模块）

数据类型与 `changes` 模块相同，都派生 `Clone`、`PartialEq`、`Eq`、`Debug`、`Serialize`、`Deserialize`（外部标记的枚举）；trait 和包着回调的 `BriefReply`、`BriefRulesRegistration`、`OverviewInterest` 除外。

```rust
/// 单实现服务，"review" 插件提供。
/// impl Service for dyn Review { const ID = "review"; const MULTIPLE = false; }
pub trait Review: Send + Sync {
    /// 打开一个审阅。立即返回，任何线程可调用。同一审阅对象已经打开时返回同一个 ID，引用数加一。
    /// 第一份快照是 `LoadState::Loading`，之后随 `ReviewEvent::Changed` 更新。
    fn open(&self, target: ReviewTarget) -> ReviewId;

    /// 当前快照。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程可调用。已关闭的 ID 返回 None。
    fn snapshot(&self, id: ReviewId) -> Option<Arc<ReviewSnapshot>>;

    /// 发一个命令。立即返回，任何线程可调用。改已看状态的命令在返回之前已换出新快照（只改内存），
    /// 写标注日志在后台进行；其余命令的结果随事件送达。
    fn command(&self, id: ReviewId, command: ReviewCommand);

    /// 准备好的展示行。不阻塞；还没有准备时为 None，调用方先发 `ReviewCommand::Prepare`。
    fn rows(&self, id: ReviewId, key: &RowsKey) -> Option<Arc<FileRows>>;

    /// 准备好的正文（prompt、agent 说明）。不阻塞；规则同 `rows`。
    fn text(&self, id: ReviewId, key: &TextKey) -> Option<Arc<str>>;

    /// 引用数减一。归零后保留 30 秒（返回项目主页再进入不重算），之后释放快照、行与正文、worktree 句柄和监听。
    fn close(&self, id: ReviewId);

    /// 项目主页的概览。不阻塞；没有关注者时为 None。
    fn overview(&self, project: &ProjectKey) -> Option<Arc<Overview>>;

    /// 关注一个项目的概览：返回值持有期间本 crate 计算并保持它（含 ADR 0035 的计数比较），drop 后释放。立即返回。
    fn watch_overview(&self, project: &ProjectKey) -> OverviewInterest;

    /// 登记一个简报规则的提供者（「简报规则的扩展点」）。立即返回，任何线程。本 crate 每次调用提供者之前
    /// `token.enter()`，取不到（提供者已停用）就当作没有登记。返回值 drop 时注销。
    fn register_brief_rules(&self, provider: Arc<dyn BriefRules>, token: ActivationToken) -> BriefRulesRegistration;
}

/// 在 executor 线程上、快照替换之后发出。监听只更新状态或安排重绘，不做 IO。
pub enum ReviewEvent {
    Changed { id: ReviewId, generation: u64 },
    /// `Prepare` 请求的行或正文有一部分已就绪。
    Prepared { id: ReviewId, generation: u64 },
    Overview { project: ProjectKey, generation: u64 },
}

pub struct ReviewId(pub u64);

/// `watch_overview` 的关注，drop 时注销（任何线程，不阻塞）。不序列化。
pub struct OverviewInterest { /* 实现方传入的注销回调 */ }
impl OverviewInterest { pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self; }

pub enum ReviewTarget {
    /// 一个会话，含它在同一 worktree 里派出的子 agent 会话。
    Session(SessionKey),
    /// worktree 相对当前 HEAD 的改动里没有任何会话声明的部分（项目主页的 Outside Any Session）。
    Outside { project: ProjectKey, worktree: WorktreeKey },
    /// worktree 相对一个基准的全部改动（Worktrees 检查器的「相对 main」）。
    Range { project: ProjectKey, worktree: WorktreeKey, base: BaseSpec },
}

pub enum ReviewCommand {
    MarkSeen(FileKey),
    MarkUnseen(FileKey),
    /// 把当前快照 `files` 里的全部文件标为已看（会话审阅只含它声明的文件，见「审阅对象与基准」）。
    LooksGood,
    /// 视图即将显示的行与正文。替换这个审阅上一次的请求：不在新请求里、也不在保留范围内的会被丢弃。
    Prepare(Arc<[Need]>),
    /// 用户要求重新比较（`RefreshHint::Full`）。
    Refresh,
    /// #66 实现之前，快照的 `brief.analysis` 是 `Unavailable`，这两个命令被忽略。
    Analyze,
    CancelAnalyze,
}

pub enum Need { Rows(RowsKey), Text(TextKey) }
```

快照：

```rust
pub struct ReviewSnapshot {
    pub id: ReviewId,
    pub target: ReviewTarget,
    /// 每次替换加一，与 `ReviewEvent::Changed` 一致。
    pub generation: u64,
    pub state: LoadState,
    /// 会话与比较范围的基准；`Loading` 时为 None。
    pub base: Option<BaseInfo>,
    /// 下标 0 是审阅对象的会话，其后是它在同一 worktree 里的子 agent 会话；其余审阅对象为空。
    pub sessions: Arc<[SessionRef]>,
    /// 进度列表。会话审阅：改动集里本审阅对象声明的文件（有它的改动步或命令声明，「审阅对象与基准」的文件集合），
    /// 按叙事里第一次出现的顺序，噪声文件在最后；会话外的改动：没有声明的文件；比较范围：全部文件。后两者按分组、路径。
    pub files: Arc<[FileEntry]>,
    /// 改动集里不在 `files` 里的文件数（会话审阅：别的会话或会话外的改动）。
    pub unclaimed: u32,
    /// 与 `files` 一一对应。单独一个数组，勾选时只换这一份。
    pub seen: Arc<[SeenState]>,
    pub progress: Progress,
    /// 按轮次的改动叙事。会话日志认不出、或审阅对象不是会话时为 None，视图按 `groups` 显示。
    pub turns: Option<Arc<[TurnEntry]>>,
    pub groups: Arc<[Group]>,
    pub brief: Arc<Brief>,
    /// 给 `CodeIntel::set_focus` 的两类焦点（UTF-8 的路径，相对 worktree 根）：改到的文件按叙事顺序；
    /// 会话写过、读过的文件。由 `review_view` 调用。
    pub focus_review: Arc<[RelPath]>,
    pub focus_touched: Arc<[RelPath]>,
    pub totals: ChangeTotals,
    pub degraded: Option<Degraded>,
    /// #22 加入；本 issue 里总是空的。
    pub notes: Arc<[Note]>,
}

pub enum LoadState { Loading, Ready, Failed(ReviewError) }

pub enum ReviewError {
    /// 会话不在列表里（已删除），或所在 worktree 已不存在。
    Missing,
    /// 会话没有归位到已打开的项目。
    NotInOpenProject,
    Changes(ChangeError),
}

pub enum Degraded {
    /// 会话日志认不出格式（`SessionError::Unrecognized`）：没有轮次，按文件显示。
    UnrecognizedSession,
    /// 文件夹项目没有基准：只显示轮次和调用记录里的改动，没有进度和 Drifted（见「未决问题」2）。
    FolderProject,
}

pub struct BaseInfo {
    pub commit: Option<BlobId>,      // None：还没有提交（空树）
    pub how: BaseHow,
}
pub enum BaseHow {
    /// 标注日志里固定下来的。
    Recorded,
    /// reflog 里会话开始时的记录。
    Reflog,
    /// reflog 的记录都晚于会话开始，取最早一条的旧值。
    BeforeLog,
    /// 没有 reflog，取当前 HEAD：会话里的提交不在改动集里，视图提示「基准可能不准」。
    CurrentHead,
    /// 会话外的改动、比较范围：按 `BaseSpec`。
    Spec,
}

pub struct SessionRef {
    pub key: SessionKey,
    pub agent: Agent,
    /// 子 agent：父会话里派出它的那一轮和调用；父会话的轮次里找不到这次调用时为 None，按时间归到轮次。
    pub spawned_by: Option<(TurnRef, ToolCallId)>,
}

/// 一个文件在快照里的稳定标识，跨代不变，视图按它保留滚动位置与选中项。
/// 新增 (None, 新路径)；删除 (旧路径, None)；修改、类型变化、冲突 (路径, 路径)；重命名 (来源, 目标)。
pub struct FileKey { pub old: Option<ChangePath>, pub new: Option<ChangePath> }

pub struct FileEntry {
    pub key: FileKey,
    pub change: FileChange,
    pub noise: Option<NoiseKind>,
    pub group: GroupKind,
    /// 所在的子项目（`PackageMatch::Found`）；没有子项目或路径不是 UTF-8 时为 None。
    pub package: Option<RelPath>,
    pub attribution: FileAttribution,
}

pub enum SeenState {
    Unseen,
    Seen,
    /// 审过之后又被改过：同一审阅对象里同一个改动的已看标记，键里的值有了变化。`since` 是那次标记的时刻。
    ChangedSinceSeen { since: Timestamp },
}

pub struct Progress { pub total: u32, pub seen: u32, pub changed_since_seen: u32 }

pub struct FileAttribution {
    pub state: AttributionState,
    /// 改过这个文件的轮次（审阅对象内），按叙事顺序。
    pub turns: Arc<[TurnRef]>,
    /// 命令期间改动（文件级）：运行那条命令的调用。
    pub commands: Arc<[CallRef]>,
    /// 同一路径上其他会话的改动（行来源指向它们的行）。
    pub other_sessions: Arc<[SessionKey]>,
    /// 至少有一行没有来源的 hunk 数。
    pub drifted_hunks: u32,
}

pub enum AttributionState {
    /// 还在计算（后台按叙事顺序逐个文件进行）。
    Pending,
    /// 逐行来源已算出，hunk 的归因见 `FileRows::hunks`。
    Lines,
    /// 只有文件级的归因：二进制、超过改动链的上限、工作区一侧没有保留内容等（见「改动链」）。
    FileOnly,
    /// 没有任何会话的改动步或命令声明这个文件。
    None,
}

/// `session` 是 `ReviewSnapshot::sessions` 的下标，`ordinal` 是那个会话的 `Turn::ordinal`。
pub struct TurnRef { pub session: u16, pub ordinal: u32 }
pub struct CallRef { pub turn: TurnRef, pub call: ToolCallId }

pub struct TurnEntry {
    pub at: TurnRef,
    pub id: TurnId,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub prompt: Option<TextKey>,
    pub prompt_preview: Arc<str>,
    pub items: Arc<[NarrativeItem]>,
    /// 这一轮在这个审阅对象最近一次已看标记之后开始（「回到第 3 步只看新轮次」）。
    pub after_last_seen: bool,
}

pub enum NarrativeItem {
    AgentText(TextKey),
    Injected(TextKey),
    /// 这一轮对一个文件的改动；同一轮对同一文件的多次调用合成一步。
    Step(StepEntry),
    /// 命令期间被修改的文件（文件级，见「声明」）。
    Command { call: ToolCallId, name: Arc<str>, files: Arc<[FileKey]> },
    /// 在这里派出的子 agent；它的轮次紧跟在这一轮之后，`TurnRef::session` 是它的下标。
    Subagent { session: u16, call: ToolCallId },
    /// 其余工具调用，一行摘要。
    Tool { call: ToolCallId, name: Arc<str>, status: Option<ToolStatus> },
}

pub struct StepEntry {
    pub step: StepId,
    pub file: FileKey,
    pub calls: Arc<[ToolCallId]>,
    pub added: u32,
    pub removed: u32,
    pub alignment: Alignment,
    /// 这一步加入的行在工作区里还剩多少。
    pub survival: Survival,
}

pub struct StepId(pub u32);

pub enum Alignment {
    Aligned,
    /// 其中有调用应用不上（Drifted），这些调用的改动按日志原文显示。
    Partial { failed: Arc<[ToolCallId]> },
    /// 全部应用不上，或这个文件超过改动链的上限：只能按日志原文显示。
    Unaligned,
}

pub enum Survival { All, Partly, None }

/// prompt 与说明的正文：哪个会话的哪一段。
pub struct TextKey { pub session: u16, pub at: ContentRef }

pub enum RowsKey {
    /// 基准到工作区的最终改动。
    Final { file: FileKey, context: u32 },
    /// 改动叙事里的一步（这一轮之前的版本到这一轮之后的版本）。
    Step { step: StepId, context: u32 },
}

pub struct FileRows {
    pub key: RowsKey,
    pub old: RowsSide,
    pub new: RowsSide,
    pub rows: Arc<[DiffRow]>,
    /// `DiffRow` 里的 `hunk` 是这里的下标。
    pub hunks: Arc<[HunkInfo]>,
    /// 配对行的词级差异：（`rows` 的下标，两侧的字节区间）；任一行超过 1000 字节的不给出。
    pub inline: Arc<[(u32, InlineSpans)]>,
}

pub struct RowsSide {
    /// 这一侧内容的 OID；改动链里的中间版本按 git 的 blob 格式计算（不在对象库里）。
    pub oid: Option<BlobId>,
    pub text: SideText,
}

/// 与 `git::Row` 一一对应，另带所在 hunk 的下标（`Folded` 没有）。
pub enum DiffRow {
    Context { old: u32, new: u32, old_byte: Option<u64>, new_byte: Option<u64>, hunk: Option<u32> },
    Removed { old: u32, pair: Option<u32>, old_byte: Option<u64>, hunk: u32 },
    Added { new: u32, pair: Option<u32>, new_byte: Option<u64>, hunk: u32 },
    Folded { old: Range<u32>, new: Range<u32>, old_byte: Option<u64>, new_byte: Option<u64> },
}

pub struct InlineSpans { pub old: Arc<[Range<u32>]>, pub new: Arc<[Range<u32>]> }

pub struct HunkInfo {
    pub old: Range<u32>,
    pub new: Range<u32>,
    pub attribution: HunkAttribution,
}

/// 视图的规则：`drifted > 0` 且 `turns`、`others` 都为空时是 Drifted，不显示归因；
/// 都不为空时显示归因，并标出其中 Drifted 的行数。`Step` 的 hunk 总是归到这一步，`turns` 只有它的轮次。
pub struct HunkAttribution {
    /// false：这个文件只有文件级归因，下面三项为空。
    pub lines: bool,
    pub turns: Arc<[TurnRef]>,
    pub others: Arc<[SessionKey]>,
    /// 没有来源的行数（新一侧加入的行，加上基准里被链外改动删掉的行）。
    pub drifted: u32,
}
```

规则分组与简报：

```rust
pub enum GroupKind { Code, Tests, Config, Deps, Docs, Noise(NoiseKind) }
pub enum NoiseKind { Lockfile, Generated, Snapshot }

/// 按 `GroupKind` 的声明顺序；组内按子项目、路径。
pub struct Group { pub kind: GroupKind, pub files: Arc<[u32]> }   // `files` 的下标

pub struct Brief {
    /// critical 在前，其后按「简报规则」表的顺序，同一规则按文件的进度顺序。最多 12 条。
    pub items: Arc<[BriefItem]>,
    /// 超出 12 条没有列出的数量。
    pub omitted: u32,
    /// 已登记、请求过、还没有答复的 `BriefRules` 提供者数（视图可以显示「还在补充」）。
    pub pending_providers: u32,
    pub analysis: AnalysisState,
}

pub struct BriefItem {
    pub rule: BriefRule,
    pub critical: bool,
    pub file: Option<FileKey>,
    /// 行备注：只挂在最终改动里显示的行上，否则为 None（降为文件备注）。
    pub line: Option<LineAnchor>,
    /// 合并成一条时的数量（如三个噪声文件）。
    pub count: u32,
}

pub enum BriefRule {
    ChangedSinceSeen,
    Drifted,
    EditFailed,
    CommandOnly,
    Sensitive(SensitiveKind),
    Deleted,
    Incomplete(IncompleteKind),
    Noise(NoiseKind),
    /// `BriefRules` 提供者给出的规则；视图按 `brief-<provider>-<rule>` 取 Fluent 文案，`args` 是它的参数。
    Provided { provider: Arc<str>, rule: Arc<str>, args: Arc<[(Arc<str>, Arc<str>)]> },
}

pub enum SensitiveKind { AgentInstructions, Ci, Hooks, GitMetadata, Symlink, Executable, Submodule }
pub enum IncompleteKind { Truncated, TooLarge, Binary, NotRetained, Filtered, Unavailable, Listed }

pub struct LineAnchor { pub side: DiffSide, pub line: u32 }
pub enum DiffSide { Old, New }

pub enum AnalysisState {
    /// #66 之前总是这一项；之后用于找不到命令行、系统不满足 ADR 0019 的约束等，附原因。
    Unavailable(Arc<str>),
    Idle,
    Running { started_at: Timestamp },
    Done { stale: bool, partial: bool },
    Failed(Arc<str>),
}
```

概览：

```rust
pub struct Overview {
    pub project: ProjectKey,
    pub generation: u64,
    /// 当前 worktree 里在声明窗口内（最近 14 天，「改动步与声明」）、至少声明了一个改动文件的会话，按 `last_activity` 从新到旧，最多 50 个。
    pub sessions: Arc<[SessionOverview]>,
    /// 当前 worktree 不属于任何会话的改动。
    pub outside: OutsideCount,
}

pub struct SessionOverview {
    pub session: SessionKey,
    /// 已看进度，文件集合与这个会话的审阅快照的 `files` 相同；还没算出（会话的基准不是当前 HEAD，且超出了每轮的计算数量）时为 None。
    pub progress: Option<Progress>,
    /// 有 Drifted 的文件：只在归因缓存命中时知道，否则为 None。
    pub drifted: Option<bool>,
    /// 规则简报里 critical 的条数（不含需要逐行来源的规则）。
    pub critical: u32,
}

pub enum OutsideCount {
    Pending,
    Files(u32),
    /// 改动路径超过 4000 个（ADR 0035）：不保留改动集，只给出全部改动路径数，不区分是否属于会话。
    Capped(u32),
}
```

调用方：

- `review_view`：进入审阅时 `open`，收到 `Changed` 后 `snapshot`；滚动时发 `Prepare`（可见的文件与轮次，加上前后各一屏），收到 `Prepared` 后读 `rows` / `text`；勾选、V、Looks good 发对应命令；离开时 `close`。显示一个文件的 diff 时为 `FileRows` 两侧的 `SideText::Text` 各调一次 `Reader::share_object`（[file_reader 设计「对其他 crate 的要求」](file_reader.md#对其他-crate-的要求)），与这份 `FileRows` 一起放下。
- `project_home`（[#30](https://github.com/SoloJiang/lectern-app/issues/30)）：显示主页时持有 `watch_overview` 的返回值，收到 `Overview` 后读 `overview`。
- `worktrees`：「相对 main」打开 `ReviewTarget::Range`。
- `conversation`（#22）：读快照里的审阅意见。

### 简报规则的扩展点（`BriefRules`）

需要别的插件知识的简报规则由提供者登记进来，本 crate 不依赖提供者；没有提供者时简报照常生成。`BriefRules` 不是 kernel 服务：kernel 没有可选依赖，本 crate 若 requires 一个 `MULTIPLE` 服务，就要等到至少一个提供者激活才能激活；查找不在 requires 里的服务则让插件失败（[kernel 设计「激活时的上下文」](kernel.md#激活时的上下文)）。所以方向反过来，与 code_intel 的 `NavigationProvider`（[code_intel 设计「语义导航的注册」](code_intel.md#语义导航的注册26-的升级钩子)）相同：提供者的插件 requires `dyn Review`，在 `started` 里调 `Review::register_brief_rules`，把返回的登记交给 `PluginHandle::own`。登记带着提供者这次激活的 `ActivationToken`，本 crate 每次调用提供者之前 `token.enter()`，持有准入直到调用返回；提供者停用时登记随撤销 drop，本 crate 丢弃它在途的答复。

```rust
pub trait BriefRules: Send + Sync {
    /// 提供者的稳定 ID（插件 ID），也是 `BriefRule::Provided::provider`。不阻塞。
    fn id(&self) -> Arc<str>;
    /// 立即返回，任何线程。结果经 `reply` 交回，可以分几次，最后一次 `done` 为 true。
    /// 同一审阅的简报重算时，本 crate 丢弃上一次的 `BriefReply`（`cancelled()` 变为 true），提供者应停止。
    fn evaluate(&self, request: BriefRequest, reply: BriefReply);
}

pub struct BriefRequest {
    pub project: ProjectKey,
    pub worktree: Option<WorktreeKey>,
    /// 噪声文件、路径不是 UTF-8 的文件不在其中。
    pub files: Arc<[BriefFile]>,
}

pub struct BriefFile {
    pub key: FileKey,
    pub path: RelPath,
    pub old_oid: Option<BlobId>,
    /// 工作区一侧的 OID：内容按 `TextOrigin::Worktree` 读、按它核对。
    pub new_oid: Option<BlobId>,
    pub hunks: Arc<[DiffHunk]>,
}

pub struct ProvidedItem {
    pub rule: Arc<str>,
    pub critical: bool,
    pub file: Option<FileKey>,
    pub line: Option<LineAnchor>,
    pub args: Arc<[(Arc<str>, Arc<str>)]>,
}

/// 包着本 crate 传入的回调（与 `SemanticReply` 相同），不序列化。`send` 在任何线程上调用，不阻塞。
pub struct BriefReply { /* … */ }
impl BriefReply {
    pub fn new(send: impl Fn(Vec<ProvidedItem>, bool) + Send + Sync + 'static, cancelled: Arc<AtomicBool>) -> Self;
    pub fn send(&self, items: Vec<ProvidedItem>, done: bool);
    pub fn cancelled(&self) -> bool;
}

/// `register_brief_rules` 的登记，drop 时注销（任何线程，不阻塞）。`Send`，可以交给 `PluginHandle::own`。
/// 包着本 crate 传入的注销回调（与 `ProviderRegistration` 相同），不序列化。
pub struct BriefRulesRegistration { /* … */ }
impl BriefRulesRegistration { pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self; }
```

提供者的条目经过与模型备注相同的对账：`file` 不在快照里的丢弃；`line` 不是最终改动里显示的行时降为文件备注；每个提供者最多 8 条。超过 10 秒没有 `done` 的提供者按完成处理，`pending_providers` 不再计它。

### 本 crate 的公开项

```rust
/// 噪声文件的识别（「噪声文件与规则分组」）。纯函数，任何线程。`generated` 是 `FileChange::generated`。
pub mod noise {
    pub const RULES_VERSION: u32 = 1;
    pub fn classify(path: &[u8], generated: bool) -> Option<NoiseKind>;
    pub fn group(path: &[u8], generated: bool) -> GroupKind;
    pub fn sensitive(change: &FileChange) -> Option<SensitiveKind>;
}

/// 测试与 bench 用（`test-support` feature）。
pub mod testing {
    /// 按脚本构造会话（轮次、说明、改动步、命令、子 agent），实现 `SessionSource`；脚本的格式见「测试」。
    pub struct FakeSessions;
    impl FakeSessions {
        pub fn from_script(source: &str, script: &str) -> FakeSessions;
        /// 追加一轮并发出 `SessionsChanged`（`refresh_*` 用）。
        pub fn append(&self, session: &str, script: &str);
    }
    /// 只有快照的 `Projects`：一个仓库项目，worktree 来自 `TestRepo`。
    pub struct FakeProjects;
    /// 不经插件，在调用线程上驱动审阅引擎（`Executor::manual()`）。bench 的 `review/*` 用它。
    pub struct Driven;
}
```

### Analyze 的边界（#66）

ADR 0019 要求模型输出经过结构校验、覆盖检查、一次纠错重试和对账后才交给视图，任何文件不能因为模型漏掉而消失。本 crate 负责输入清单、校验和对账；运行命令行、纠错重试的那一轮对话、缓存键的哈希和用量由 #66 的 `agents` 一方负责。函数都是纯函数，随 #66 实现（`Refs #66`），本 issue 只定下签名：

```rust
pub mod analyze {
    /// 输出 schema 的版本，进分析请求的哈希（ADR 0019 约束 11）。
    pub const SCHEMA_VERSION: u32 = 1;

    /// 模型的输入清单，只含快照的 `files`（会话审阅是它声明的文件，ADR 0019 约束 2）：每个文件一行（路径、状态、增删行数、`[generated]` / `[lockfile]` / `[snapshot]` / `[binary]` / `[truncated]` 标签）；
    /// 噪声文件只留这一行，不附 patch。`patches` 是其余文件规范化后的统一 diff（来自同一份 `ChangeList`）。
    pub struct Manifest { pub files: Arc<[ManifestFile]>, pub patches: Arc<[(FileKey, Arc<[u8]>)]>, pub noise_rules: u32 }
    /// 阻塞（读两侧文本），在 executor 上调用。
    pub fn manifest(source: &dyn ChangeSource, list: &ChangeList, snapshot: &ReviewSnapshot) -> Result<Manifest, ChangeError>;

    /// 模型提交的结果：分组，每组有标题、摘要、`critical`、文件、文件备注和行备注（[pulls.review 调研「分组」](../research/pulls-review.md#分组)）。
    pub struct Output { pub groups: Vec<OutputGroup> }
    /// 结构校验。
    pub fn parse(bytes: &[u8]) -> Result<Output, SchemaError>;
    /// 覆盖检查：缺失、重复、不存在的路径。不为空时 #66 在同一个辅助会话里纠错重试一次，第二次仍不为空时照常对账。
    pub fn coverage(output: &Output, manifest: &Manifest) -> Vec<CoverageIssue>;
    /// 对账：删除不存在和重复的路径（第一个分组保留）、指向组外文件的备注、空分组；没被分到的文件进入「未分类」。
    pub fn reconcile(output: Output, manifest: &Manifest) -> Reconciled;
    /// 显示时的对账：按旧路径匹配重命名，已消失的文件标为 missing；行备注只挂在最终改动里显示的行上，否则降为文件备注。
    pub fn place(reconciled: &Reconciled, snapshot: &ReviewSnapshot) -> Arc<AnalysisView>;
}
```

`AnalysisView` 进快照的 `brief`（#66 加字段），规则简报保留，两者并列显示。结果过期的判断（ADR 0019 约束 9）用 `ChangeSource::fingerprint` 和 `fs_watch` 的修订号，由 #66 设计。

## 数据结构

### 审阅对象与基准

- **会话**：`SessionSummary::place` 为 `Located` 且有 worktree 的会话（未归位、项目没有打开时 `ReviewError::NotInOpenProject`）。审阅对象包括它的子 agent 会话中归位到同一 worktree 的（`SessionSummary::parent` 指向它，递归）；子 agent 在别的 worktree 里时是单独的审阅对象，叙事里只有一行 `Subagent`（见「未决问题」1）。基准按顺序取：
  1. 标注日志里这个会话的 `base` 记录（`BaseHow::Recorded`）。
  2. `Worktree::head_reflog`（读到早于会话开始时刻的那条为止）里最后一条时间不晚于会话开始的记录：它的新值，`BaseHow::Reflog`；这条记录是「基准记录」。读到的记录都晚于会话开始（reflog 从那之后才有）时取最早一条的旧值（全 0 为空树），`BaseHow::BeforeLog`，没有基准记录。
  3. 没有 reflog：当前 HEAD（`CurrentHead`）。

  unborn 时基准是空树。会话第一次被标记已看时，把当时用的基准和基准记录的时间写进标注日志（`base` 记录），之后 reflog 过期、HEAD 移动都不改变它，已看标记的键保持有效。会话外的改动的基准记录是 reflog 的最新一条。

  **文件集合**：会话审阅的 `files` 是改动集（基准到工作区）里这个审阅对象（会话和它在同一 worktree 的子 agent）声明的文件，即有它的改动步，或有它的命令声明（「改动步与声明」）。改动集里其余的文件不进 `files`，只计入 `ReviewSnapshot::unclaimed`：它们属于别的会话的审阅或会话外的改动。声明的文件里来自其他会话的行照常显示在 hunk 的 `others` 里。会话日志认不出格式时没有改动步，声明改为按时间：工作区修改时间落在 `[started_at - 1 s, last_activity + 2 s]` 里、且没有被别的会话声明的文件。进度、`LooksGood`、简报规则、项目主页的进度（「概览」第 4 步）和模型的输入清单（`analyze::manifest`，ADR 0019 约束 2「快照只含属于审阅对象的改动」）都只用这个集合。
- **会话外的改动**：基准是当前 HEAD（`BaseSpec::Head`）。文件是改动集里没有任何声明的那些。
- **比较范围**：按 `BaseSpec` 比较整个工作区，`files` 是改动集的全部文件；文件级归因只列出声明它的会话（不建改动链：基准不是某个会话开始时的状态，链的起点对不上）。

### 改动步与声明

一个 worktree 的声明来自这个 worktree 里、`last_activity` 在最近 14 天之内的会话（含子 agent，最多 100 个，按 `last_activity` 从新到旧），从各自的轮次索引得到，不需要 `load`。每个会话派生出的声明（每个改动步的路径、时刻、轮次 ID、调用 ID 与状态、输入与结果的 `ContentRef`，命令调用的时间窗口）存进 `review.claims`（「归因的缓存」），会话没有新活动时不再读它的轮次索引。

- `review.claims` 的条目按轮次索引的身份核对：值里的 `last_activity` 与当前 `SessionSummary` 相同、`tail_pending` 为 false，并且值里的适配器版本等于这个来源当前的适配器版本，才能用。当前的适配器版本取本次运行里从这个来源读到的任一轮次索引的游标（`TurnIndex::cursor` 的 `adapter_version`）；打开会话总会读审阅对象自己的索引，概览在未命中时读。本次运行还没有读过这个来源时，第一份快照先按条目使用，后台为这个来源读一次 `turns_unretained` 核对版本，不同就丢弃这个来源的全部条目、重算受影响的文件。
- 条目里的 `ContentRef` 是会话文件里的位置。会话只追加写，`last_activity` 不变时位置有效；`load` 失败或给出的不是改动时丢弃这个会话的条目，按未命中读它的索引。每个路径另有一个截止时刻，早于它的改动步和修改时间不参与这个路径：

- **路径的截止时刻**（关键约束）：取这个 worktree 的 HEAD reflog，从基准记录（含）往前、时间在 14 天的窗口之内、旧值与新值不同的记录，最多 256 条。截止时刻是其中最近一条让这个路径在旧、新两个提交的树之间不同的记录（`Repo::tree_changes`，前置条件 B）的时间。
  - 这几类 HEAD 移动会改变这个路径的基准内容，截止时刻因此后移，之前的会话改动不再参与：提交了这个路径、切换到这个路径内容不同的分支（`git checkout feature`）、`reset`、`rebase`、`pull`。
  - `git checkout -b`（旧值等于新值）、提交别的文件（树在这个路径上相同）不移动截止时刻，所以还没提交的会话改动留在链里。
  - 窗口里没有任何记录改变这个路径时没有截止时刻，只受 14 天的窗口限制。窗口里的记录超过 256 条时，其余路径按第 256 条的时间截止（保守：只会少算会话的改动，多出的行显示为 Drifted 或会话外的改动）。
  - 没有 reflog（`BaseHow::CurrentHead`）时，全部路径按基准提交的提交时间截止（已知限制：这时提交无关的文件也会让之前未提交的会话改动离开链）。
  - 基准记录之后的 reflog 记录不参与：会话审阅的基准是会话开始时的提交，会话里 agent 自己的提交在基准到工作区的改动里，它之前的改动步要留在链里。
- 理由：按「HEAD 最后一次移动」一刀截止时，`checkout -b` 或一次无关的提交就让之前未提交的会话改动从链里消失，后来的会话的 `Replace` 应用不上，正常的改动被标成 Drifted 或整份变成会话外的改动；只看基准提交历史里路径最后一次被提交的时间时，切换到另一个分支后旧分支上的会话改动又会被重放到新分支的基准上。reflog 记录了这个 worktree 的基准内容在什么时刻、对哪些路径发生了变化，两种情况都按实际的变化处理。
- 已经提交进基准的改动（截止时刻晚于它）不再参与，不会在基准上再应用一次；rebase、amend、cherry-pick 也是一条改变路径的 reflog 记录。截止之后仍有少数链外改动（时钟偏差、只提交了一部分）时，`Write` / `Add` 让链重新对齐（「改动链」）。
- 代价的上限：`tree_changes` 跳过 OID 相同的子树，结果按（旧提交，新提交）缓存在 `review.tree_changes`（「归因的缓存」），同一对提交只算一次，跨重启有效；每个 worktree 在内存里只保留改动集里那些路径的截止时刻（≤ 4000 个）。打开审阅时在 `Interactive` 上最多计算 5 ms 的未命中，其余记录在 `Background` 上计算，之前先按「改变了全部路径」处理（保守），算完后重算受影响的文件并换出快照。
- 改动步：`ToolKind::Edit { paths }` 的每个路径。路径是绝对路径：先去掉 worktree 根的前缀；不匹配时用会话日志里的 cwd 与归位用的规范 cwd 之间的前缀替换（`/tmp` 与 `/private/tmp` 这类）再试；都不在 worktree 里的忽略。时间取 `ToolCall::finished` 的时刻；没有结果的调用（还在运行，或日志里没有结果）不参与，下一次 `SessionsChanged` 再看。状态不是 `Completed` 的调用不应用，记为失败的改动（简报的 `EditFailed`）。
- 命令声明（文件级）：改动集里有、没有任何改动步的文件，工作区修改时间（`Worktree::mtimes`）落在某个会话的 `ToolKind::Execute` 调用的 `[started_at - 1 s, finished + 2 s]` 里（日志时间与命令真正运行的时间有偏差，见 agents 设计）。只落在一个会话的窗口里时归给那次调用；落在几个会话的窗口里时都不归，算会话外的改动。早于路径的截止时刻的修改时间不算。
- 改动步和命令声明的顺序：按时刻，同一时刻按（会话在列表里的顺序，`Positioned` 的位置）。
- 已知限制：14 天之前开始、之后再没有活动、改动一直没有提交的会话，它的文件算会话外的改动。

### 改动链

每个有改动步的文件单独建链，在后台逐个文件计算：

1. 起点 v0：基准一侧的内容（`ChangeSource::texts` 的旧一侧，重命名取来源路径）；基准里没有这个路径时为空。
2. 按顺序应用这个路径的每个改动步（`load(result)`，没有结果的改动时用 `load(input)`，agents 设计「轮次索引」）：
   - `Write`、`Add`：版本整个换成给出的内容。这一步总能对齐，也是之后的重新对齐点。
   - `Replace { old, new, all }`：在当前版本里找 `old` 的字节串。`all` 为 false 时必须恰好出现一次，替换它；为 true 时替换全部出现。找不到或出现多次（工具当时只会成功替换唯一的一处，出现多次说明版本已经和 agent 看到的不同）时这一步应用不上。
   - `Patch { hunks, move_to }`：每个 hunk 的旧一侧（上下文加删除行）必须逐字节相同；先在 hunk 头给出的行号处比较，不同时在全文找最近的匹配位置（与 `git apply` 的偏移相同，不做模糊匹配）。有一个 hunk 应用不上，整步应用不上。`move_to` 让后续步骤跟到新路径。
   - `Delete`：版本为空（文件不存在）。
   - 应用不上的一步不改变版本，记为 `Alignment::Partial` 的失败调用；它想加入的行之后会作为没有来源的行出现在工作区里。不做模糊匹配、不猜位置（[交互规范「状态与异常」](interaction.md#状态与异常)）。
   - 换行：工作区一侧是规范内容（`core.autocrlf` 时 CRLF 已转成 LF），agent 的改动是原始字节。`old` 找不到、当前版本没有 `\r` 而 `old` 有时，把 `old`、`new` 里的 CRLF 换成 LF 再试一次；`Write` 的内容同样处理。这是 git 的同一种转换，不是猜测；`working-tree-encoding` 的文件只做文件级归因。
3. 逐行来源：每应用一步，用 `git::line_diff(前一版本, 后一版本)` 更新行来源：未改的行沿用，新加的行记为这一步；基准里的行被删掉时记下删掉它的那一步。同一轮里同一文件的多步合成叙事里的一步（`StepEntry`），它的 hunk 是这一轮之前与之后两个版本的 `line_diff`。
4. 对齐工作区：最后一个版本 vn 与工作区一侧比较（`line_diff(vn, W)`）。两边相同（OID 相等）时全部对齐；否则 W 里新出现的行没有来源，vn 里有、W 里没有的行记为被链外的改动删掉。
5. 最终 hunk（`ChangeList` 里基准到工作区的 hunk）的归因：新一侧每行按 W 的行来源，旧一侧每行按基准行的去向，汇总成 `HunkAttribution`：来自本审阅对象的步归到它们的轮次，来自其他会话的列在 `others`，没有来源的计入 `drifted`。
6. 存活：每一步加入的行，在 W 的行来源里还剩多少：全部为 `All`，一部分为 `Partly`，没有为 `None`。

上限（超过时这个文件只做文件级归因，`AttributionState::FileOnly`）：任一版本超过 4 MiB、改动步超过 200 个、基准或工作区一侧不是 `SideText::Text`（二进制、`TooLarge`、`NotRetained`、过滤器）。文件级归因：最后一个 `Write` / `Add` 的内容的 OID 等于工作区一侧的 OID 时归给那一步的轮次，否则列出改过它的全部轮次，`drifted_hunks` 为 0（不知道）。叙事里这些步骤按日志原文显示（`Alignment::Unaligned`）。

中间版本不在对象库里。叙事里一步的展示行要两侧文本时重新回放（基准内容加前面的步骤，`load` 每次 < 1 ms）；回放出的版本按 `Oid::for_blob` 的 OID 放进内存 LRU（字节上限 8 MiB），同一个 OID 不重复回放。

### 归因的缓存

| 数据 | 键 | 存放 |
|---|---|---|
| 一个文件的链的结果：每一步的版本 OID、对齐结果与失败的调用、叙事步的 hunk、存活；W 每行来源的游程编码；基准每行去向的游程编码 | 命名空间版本（算法版本）、哈希算法、基准一侧的 OID（没有时为空）、工作区一侧的 OID（`Blob` 或 `NotRetained`；其余身份不缓存）、轮次历史的摘要 | `cache` 的仓库分库，命名空间 `review.attribution`，Zstd；一个文件通常几 KB |
| 两个提交之间改变的路径（`tree_changes` 的结果，截止时刻用） | 哈希算法、旧提交的 OID（全 0 时为空）、新提交的 OID、`max_paths`（20 000） | `cache` 的仓库分库，命名空间 `review.tree_changes`，Zstd；排好序的路径列表或 `All`。只由两个提交 OID 决定（架构规则 5） |
| 一个会话派生的声明：每个改动步的路径、时刻、轮次 ID、调用 ID 与状态、输入与结果的 `ContentRef`；命令调用的时间窗口。足够算轮次历史的摘要、按 `load` 回放这个会话的改动步，不必读它的轮次索引 | 来源 ID、会话 ID。值里带派生时轮次索引的身份：`last_activity` 和索引游标的适配器版本（与 ADR 0026 的做法相同，值带位置时按身份核对） | `cache` 的应用级分库，命名空间 `review.claims`，不压缩；一个会话通常几 KB |
| worktree 的截止时刻表 | 路径（只为改动集里的路径保留）；值是窗口里改变了这个路径的 reflog 记录的时间，从新到旧（≤ 256 个） | 内存，概览的关注者或打开的审阅持有时存在，随 reflog 的新记录（fs_watch 的 `RepoBatch::reflog`、`meta.head`）增量更新，不每轮重算。截止时刻随基准记录而定，表本身不含基准：查询时给出基准记录的时间，取不晚于它的最新一个。会话外的改动和概览里基准是当前 HEAD 的会话用最新的记录；基准不是当前 HEAD 的会话，概览和它自己的审阅都用它自己的基准记录，所以两边的声明相同 |
| 回放出的中间版本 | 版本的 OID | 内存 LRU，8 MiB，只在有打开的审阅时存在 |
| 规则简报 | 不缓存 | 每次重算（ADR 0019） |
| 已看状态 | 不缓存 | 由标注日志和当前改动集算出 |

轮次历史的摘要：SHA-256，按顺序覆盖这个路径的每个改动步的来源 ID、会话 ID、会话轮次索引的适配器版本（`TurnIndex::cursor` 的 `adapter_version`）、轮次 ID、调用 ID、调用状态、输入与结果的 `ContentRef`（序列化后的字节），以及重命名链上的路径。只用轮次索引里的数据，不 `load`：命中时不读任何正文。两个会话把文件改成同样的内容时，摘要不同，叙事也不同（`crates/review/AGENTS.md`「边界」）；同一会话在两个 worktree 里内容相同时命中同一条。

### 改动叙事

- 轮次来自 `turns()`，按 `Turn::ordinal`；子 agent 的轮次紧跟在派出它的那一轮之后（`Parent::tool_call` 在父会话的轮次里找到时；找不到时按子会话第一轮的开始时刻插到父会话对应的轮次之后）。
- 每轮的条目按 `Turn::items` 的顺序：说明、注入内容、工具调用。`Edit` 调用合成 `Step`：同一轮对同一文件的多个调用合成一步，位置在这一轮第一次改它的地方。`Execute` 调用有命令声明的文件时为 `Command`，否则为 `Tool`。`SpawnAgent` 有对应的子会话时为 `Subagent`。
- 叙事只含本审阅对象的会话；其他会话在同一文件上的步骤不进叙事，只体现在最终 hunk 的 `others` 里。
- `after_last_seen`：这一轮的开始时刻晚于本审阅对象最近一条已看记录的时刻。

### 已看状态

已看标记的键：

```rust
pub(crate) struct SeenKey {
    pub target: TargetKey,
    pub old_path: Option<ChangePath>,
    pub new_path: Option<ChangePath>,
    pub old: Option<(FileMode, SideIdentity)>,
    pub new: Option<(FileMode, SideIdentity)>,
    /// `ChangeStatus` 去掉相似度。
    pub status: StatusKind,
}

/// 标注日志里的审阅对象：worktree 写成相对公共目录的 git 目录（主 worktree 为 ""，其余为 "worktrees/<名字>"），
/// 仓库移动后仍然有效（日志本身按仓库分开）。
pub(crate) enum TargetKey {
    Session { source: Arc<str>, id: Arc<str> },
    Outside { worktree: Arc<str> },
    Range { worktree: Arc<str>, base: BaseSpec },
}
```

一个文件的状态按本审阅对象的记录判断：

- 有与当前改动的键完全相同、且之后没有被 `unseen` 取消的 `seen` 记录：`Seen`。
- 否则，有一条「同一个改动」的 `seen` 记录（取最近的一条）：`ChangedSinceSeen`。记录 R 与当前改动 C 是同一个改动，当且仅当 `C.new_path` 不为空且等于 `R.new_path`，或 `C.old_path` 不为空且等于 `R.new_path`（审过之后又被改名或删除）。所以：
  - 重命名按（旧路径，新路径）成对匹配；审过 `a → b` 之后 `a` 上出现一个新文件（`C = (None, a)`），与那条记录不是同一个改动，为 `Unseen`。
  - 审过删除 `a` 之后又新建 `a`，同样为 `Unseen`。
  - 审过修改 `a` 之后 `a` 被改名为 `b` 或被删除，为 `ChangedSinceSeen`。
- 都没有：`Unseen`。

`MarkSeen` 写一条带当前键的 `seen` 记录；`MarkUnseen` 写一条 `unseen` 记录（之后的判断忽略它之前的 `seen`）；`LooksGood` 为快照里每个不是 `Seen` 的文件各写一条 `seen`。三者都先在内存里换出新的 `seen` 数组和 `progress`，再在后台追加日志。

### 标注日志

格式按 ADR 0044：`<data_dir>/<h>.jsonl`，`h` 是 `SHA-256("repo\0" + 公共目录的字节)` 的前 16 字节的十六进制（与 `cache` 的仓库分库相同）。第一行是文件头，之后每行一条记录：

```text
{"lectern":"review-log","v":1,"repo":"/abs/common/dir"}
{"t":"seen","at":1760000000000,"target":{"session":{"source":"session-claude-code","id":"…"}},"key":{…}}
{"t":"unseen","at":…,"target":…,"file":{"old":null,"new":"src/a.rs"}}
{"t":"base","at":…,"session":{"source":"…","id":"…"},"commit":"<40 或 64 位十六进制>","entry_at":<基准记录的时间，Unix 秒；没有时为 null>}
```

- 路径是 UTF-8 时写成字符串，否则写成字节数组。OID 写成十六进制。`at` 是 Unix 毫秒。
- 文件头的 `repo` 与当前仓库不同（文件被复制、哈希碰撞）时把文件改名为 `<h>.jsonl.foreign-<时刻>` 后新建，不读它的记录。
- 读取：打开项目的概览或审阅时，在 `Background` 任务里整份读入（上限 64 MiB，超过时只读最后 64 MiB 并从第一个完整行开始）。坏行跳过并计数；认不出的 `t` 原样保留；末尾没有换行的一行丢弃，下一次追加之前截掉它。
- 内存里的索引：每个审阅对象、每个文件标识（`new_path`，没有时 `old_path`）只保留最近一条 `seen` 或 `unseen`，加上每个会话的 `base`。项目关闭后释放。
- 写入：命令在内存里生效后把记录放进待写队列；队列在 100 ms 内没有新记录、或累计 64 条时，由一个 `Background` 任务（持有 `BusyGuard`）追加并 `fdatasync`。写失败时保留队列，下一次再试，记日志；进程退出前没写完的标记丢失（已知限制）。
- 压缩：文件超过 4 MiB 时，在下一次写入的任务里写一份只含文件头、内存索引里的记录和认不出的记录的新文件，`fsync` 后改名替换并 `fsync` 目录。超过一年的 `seen` / `unseen` 记录在压缩时丢弃。
- 压缩的前提（关键约束，ADR 0044）：内存索引来自对这个文件的一次完整、成功的读取（读到文件末尾，没有 IO 错误，没有因 64 MiB 的上限只读一部分，文件头的仓库相符），加上本进程在那之后自己的追加；并且文件的身份（设备号、inode）和长度与「读取的结束位置加本进程追加的字节」相同，说明期间没有别的写入者。任何一条不满足时只追加、不压缩：读取失败或只读了一部分时，内存索引缺记录，用它重写文件会丢掉用户的标记。下一次完整读取成功后恢复压缩。
- 文件夹项目不写日志（见「未决问题」2）。

### 噪声文件与规则分组

噪声文件的模式列表（`noise::RULES_VERSION` 1）。路径按 `/` 分段，大小写敏感；先判断 `generated`（`.gitattributes` 的 `linguist-generated`），再按下表：

| 类别 | 文件名（最后一段）完全相同 | 文件名后缀 | 路径中有这一段目录 |
|---|---|---|---|
| `Lockfile` | `package-lock.json`、`npm-shrinkwrap.json`、`yarn.lock`、`pnpm-lock.yaml`、`bun.lock`、`bun.lockb`、`deno.lock`、`Cargo.lock`、`go.sum`、`go.work.sum`、`poetry.lock`、`Pipfile.lock`、`pdm.lock`、`uv.lock`、`composer.lock`、`Gemfile.lock`、`Podfile.lock`、`Package.resolved`、`pubspec.lock`、`mix.lock`、`flake.lock`、`gradle.lockfile`、`packages.lock.json` | — | — |
| `Generated` | — | `.min.js`、`.min.css`、`.js.map`、`.css.map`、`.d.ts.map`、`.pb.go`、`.pb.h`、`.pb.cc`、`_pb2.py`、`_pb2.pyi`、`_pb2_grpc.py`、`.g.dart`、`.freezed.dart`、`.designer.cs`；文件名含 `.generated.` | `__generated__` |
| `Snapshot` | — | `.snap`、`.snap.new`、`.pending-snap`、`.ambr` | `__snapshots__` |

同一份列表决定三件事：叙事和进度列表里默认折叠（视图按 `FileEntry::noise`）；规则分组的 `Noise` 组；模型输入清单里只留一行占位（`analyze::manifest`）。列表变化时 `RULES_VERSION` 加一，它进分析请求的哈希。

规则分组：会话日志认不出、会话外的改动和比较范围按分组显示（[pulls.review 调研「对 Lectern 的建议」](../research/pulls-review.md#对-lectern-的建议)）。按下面的顺序匹配，第一个命中的生效；显示顺序是 `GroupKind` 的声明顺序（代码、测试、配置、依赖、文档、噪声）：

1. 噪声（上表）。
2. `Tests`：目录段 `test`、`tests`、`__tests__`、`spec`、`testdata`；文件名含 `.test.`、`.spec.`，或以 `_test.go`、`_test.py`、`Test.java`、`Tests.java`、`_spec.rb` 结尾，或以 `test_` 开头且以 `.py` 结尾。
3. `Docs`：后缀 `.md`、`.mdx`、`.rst`、`.adoc`；目录段 `docs`、`doc`；文件名以 `LICENSE`、`CHANGELOG`、`README` 开头。
4. `Deps`：文件名 `package.json`、`Cargo.toml`、`go.mod`、`go.work`、`pom.xml`、`build.gradle`、`build.gradle.kts`、`settings.gradle`、`settings.gradle.kts`、`pyproject.toml`、`Gemfile`、`composer.json`、`pnpm-workspace.yaml`，或以 `requirements` 开头、以 `.txt` 结尾。
5. `Config`：目录段 `.github`、`.circleci`、`.husky`；文件名以 `.` 开头；后缀 `.yml`、`.yaml`、`.toml`、`.ini`、`.cfg`、`.conf`；文件名以 `Dockerfile` 开头，或是 `Makefile`、`justfile`、`Jenkinsfile`；文件名含 `.config.`，或以 `tsconfig` 开头、以 `.json` 结尾。
6. `Code`：其余。

组内按子项目（`FileEntry::package`）再按路径排序，monorepo 里同一子项目的文件排在一起。

### 简报规则

规则只用本地已有的信息（ADR 0019），在后台随快照一起重算，不缓存。规则只看快照的 `files`：会话审阅里没有声明的文件（别的会话、手工改动）不触发任何规则。按下表顺序，critical 的条目排在最前：

| 规则 | 条件 | critical | 条目 |
|---|---|---|---|
| `ChangedSinceSeen` | 文件为 `ChangedSinceSeen` | 否 | 每个文件一条 |
| `Drifted` | `drifted_hunks > 0`，或有 `Alignment::Partial` / `Unaligned` 的步 | 否 | 每个文件一条，行备注在第一个 Drifted hunk 的第一行 |
| `EditFailed` | 本审阅对象有状态为 `Failed`、`Denied`、`Interrupted` 的改动调用 | 否 | 每个文件一条（agent 可能以为改上了） |
| `CommandOnly` | 文件只有命令声明，没有改动步 | 否 | 每个调用一条，带文件数 |
| `Sensitive(AgentInstructions)` | 路径是 `CLAUDE.md`、`AGENTS.md`、`GEMINI.md`、`.mcp.json`、`.github/copilot-instructions.md`，或在 `.claude/`、`.codex/`、`.cursor/` 下 | 是 | 每个文件一条（这些文件会被 agent 当作指令加载，ADR 0019 约束 3） |
| `Sensitive(Ci)` | 在 `.github/workflows/`、`.circleci/` 下，或是 `.gitlab-ci.yml`、`Jenkinsfile`、`azure-pipelines.yml` | 是 | 每个文件一条 |
| `Sensitive(Hooks)` | 在 `.husky/`、`.githooks/` 下，或是 `.pre-commit-config.yaml`、`lefthook.yml` | 是 | 每个文件一条（提交时会运行） |
| `Sensitive(GitMetadata)` | `.gitattributes`、`.gitmodules` | 是 | 每个文件一条 |
| `Sensitive(Symlink)` | 新一侧是新增或改了目标的符号链接 | 是 | 每个文件一条 |
| `Sensitive(Executable)` | 新一侧是 `Executable` 而旧一侧不是 | 否 | 每个文件一条 |
| `Sensitive(Submodule)` | 子模块指向的提交变了 | 否 | 每个文件一条 |
| `Deleted` | 删除了不是噪声的跟踪文件 | 是 | 合成一条，带数量 |
| `Incomplete(…)` | `Detail` 为 `Truncated`、`TooLarge`、`Binary`、`Listed`，或一侧是 `NotRetained`、`Filtered`、`Unavailable` | 否 | 每类合成一条，带数量 |
| `Noise(…)` | 噪声文件 | 否 | 每类合成一条，带数量（「已折叠」） |
| `Provided` | `BriefRules` 提供者的条目 | 由提供者给出 | 见上文 |

前置条件 F 的两条规则由 `code_intel` 作为 `Provided` 给出：改动的行落在一个导出的定义（公开的函数、类型、方法）上时为「改到的公开接口」；这个定义的调用方所在的文件不在本次改动里时为「调用方未同步修改」。判定、查询的上限和预算写在 code_intel 设计的修订里。

### #22 的预留

- 标注日志的记录类型 `note`、`note_removed`、`notes_sent` 由 #22 定义；本 issue 的读取把它们当作认不出的记录保留。
- 审阅意见的锚点与模型行备注相同：（文件标识，`DiffSide`，行号，这一侧的 OID）；行不在最终改动显示的行里时降为文件意见。
- `ReviewSnapshot::notes` 和 `Note` 类型由 #22 定义字段，本 issue 里 `Note` 是空结构，数组总为空。
- 发回会话（P1 经终端，P3 经 ACP）不在本 crate。

## 数据流与线程

```
open(target) ─▶ [Interactive] 定位会话与 worktree ─▶ 基准（日志 / head_reflog）─▶ fetch 改动集
               ─▶ 审阅对象自己的会话的 turns ─▶ 截止时刻（tree_changes 缓存，未命中最多算 5 ms）
               ─▶ 其他会话的声明（只读 review.claims，一次 get_many）
               ─▶ 快照 v1：文件、轮次骨架、规则分组、叙事顺序第一个文件的归因
fs_watch / 后台 ─▶ [Background] review.claims 没有命中的会话：turns_unretained ─▶ 派生声明并写回缓存
               ─▶ 其余 tree_changes ─▶ 声明或截止时刻有变化的文件重算（含第一个文件）─▶ 新快照
               ─▶ 其余文件的归因，每完成 8 个文件或 50 ms 换一次快照 ─▶ 简报
fs_watch WorktreeBatch ─▶ refresh(Paths / Full) ─▶ 只重算变了的文件 ─▶ 新快照
SessionsChanged ─▶ turns ─▶ 改动步变了的路径重算 ─▶ 新快照
command(MarkSeen) ─▶ [调用线程] 换出 seen 数组 ─▶ 事件 ─▶ [Background] 追加日志
Prepare ─▶ [Interactive] texts / 回放 ─▶ rows、inline_changes ─▶ Prepared
```

- 第一份快照只等审阅对象自己的数据：它自己的会话（叙事要用）、改动集、截止时刻的缓存和已缓存的声明。其他会话没有缓存的声明时不在 `Interactive` 上读它们的轮次索引，而是先按已知的声明给出归因，后台读到之后改动了哪个文件就重算哪个文件，换出新快照（可能让第一个文件里原来标为 Drifted 的行改为 `others`）。所以打开的耗时与 worktree 里的会话数、reflog 的长度无关。
- 首帧之前什么都不做：插件 `AfterFirstFrame` 激活，`activate` 只建空表、取 `Projects` 和 `SessionSource` 的引用；没有打开的审阅、也没有概览的关注者时没有任何任务。
- 每个打开的审阅一个状态对象，放在 `Mutex` 里，只在内存操作时持锁，IO 和计算都在锁外。快照每次整体替换为新的 `Arc`。
- 任务都经 `PluginHandle::own_with` 提交，持有 `BusyGuard`，结果发布后才释放（[kernel 设计「静止」](kernel.md#静止)）。用户等着的工作（打开、刷新、`Prepare`）在 `Interactive`，其余文件的归因、概览、写日志在 `Background`。没有周期性任务。
- 同一个审阅同时最多一个计算任务；运行期间来的事件只记下（路径并集、`Full`、会话变化），结束后再跑一轮。新一轮开始时置位上一轮的取消标志，结果按代数丢弃。
- 监听：打开审阅时用自己的 `RepoWatch` 监听这个仓库，`watch_worktree` 监听被审阅的 worktree，来源是本 crate 的 `Worktree::watch_source()`；同一个 worktree 已被 `project` 监听时 fs_watch 共用那一份状态（[fs_watch 设计「监听器与句柄」](fs_watch.md#监听器与句柄)）。批次按 fs_watch 设计的约定处理：`paths` → `RefreshHint::Paths`；`rescan`、`meta` 的任一标志 → `Full`。`meta.head` 变化时，会话外的改动重新确定基准；会话的基准不变。
- 释放：审阅关闭 30 秒后释放快照、`ChangeList`（连同 git 保留的工作区内容）、行与正文、`Worktree` 句柄和监听；中间版本的 LRU 在没有打开的审阅时清空。
- UI 线程上只调用 `snapshot`、`rows`、`text`、`overview`（复制 `Arc`）和立即返回的 `open`、`command`、`close`、`watch_overview`。`command` 改已看状态时只做内存里的数组替换（O(文件数)，5000 个文件约 80 KB 的复制），不做 IO。

### 概览

- 有关注者时，对项目的当前 worktree：
  1. 计数比较：`Worktree::changes(Base::Head)`，`Limits` 取 `retained_bytes = 0`、`total_patch_bytes = 0`（[ADR 0035](../adr/0035.md)，与 `crates/review/AGENTS.md`「边界」相同）。改动路径不超过 4000 个时保留这份改动集，之后按 fs_watch 的批次 `refresh`；超过时只保留计数（`OutsideCount::Capped`），之后的事件触发全量比较。这份改动集直接用 `git`，不转换成 `ChangeList`（只有路径、两侧 OID 与 stat）。
  2. 声明：声明窗口内的会话（「改动步与声明」，最多 100 个）先查 `review.claims`；没有命中（新会话、有新活动）的才读轮次索引，用 `SessionSource::turns_unretained`（前置条件 G）在 `Background` 上逐个读取，不用 `turns`：概览在 ADR 0035 的采样状态里运行，`turns` 会把这些索引留在 agents 的 8 MB 轮次索引 LRU 里，而那个 LRU 不在采样状态的份额里。每读一个会话，只留下派生的声明（同「改动步与声明」）并写进 `review.claims`，索引随即放下；声明计入本 crate 的 < 1 MB。截止时刻用这个 worktree 在内存里的截止时刻表（「归因的缓存」）：第一轮从 `review.tree_changes` 建出，之后只为 reflog 的新记录和计数改动集里新出现的路径补算，不每轮重算；每个会话按它自己的基准记录查表（基准是当前 HEAD 时用最新的记录），与它的审阅相同；命令声明对改动路径调一次 `Worktree::mtimes`（≤ 4000 个）。
  3. 会话外的改动数 = 计数改动集里没有声明的文件。
  4. 每个会话的进度：文件集合与审阅快照的 `files` 同一定义（「审阅对象与基准」的文件集合）。基准等于当前 HEAD 的会话，文件是计数改动集与它的声明的交集，已看状态按标注日志算出；基准不同的会话，每轮最多为 8 个（按 `last_activity`）另做一次同样 `Limits` 的比较，算完即丢弃改动集，其余为 None。
  5. Drifted 信号只读 `review.attribution` 的缓存（键能从声明和计数改动集的 OID 得到），未命中为 None；不为概览建链。
- 重算的时机：fs_watch 的批次、`SessionsChanged`、标注日志的写入、`ProjectEvent` 的 worktree 变化，合并 2 秒后在 `Background` 上跑一轮。关注者都放下后释放计数改动集和声明。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)、[ADR 0017](../adr/0017.md)、ADR 0035）：

| 预算 | 本 crate 的份额 |
|---|---|
| 冷启动 20 ms | 首帧之前不做任何事；插件激活（`plugin/review/activate`）< 1 ms，只建空表 |
| 打开一个会话的 Diff < 50 ms（`app/open_diff`，`review_view`） | 从 `open` 到第一份快照（`Ready`，含叙事顺序第一个文件的归因）：缓存全空时 < 35 ms，缓存已热时 < 20 ms。分项见下表；第一个文件的展示行另 < 3 ms。后台的其余工作（其他会话的声明、其余 reflog 记录的 `tree_changes`、其余文件的归因）只记录 |
| 每天的仓库规模 | 夹具：约 2000 个提交的历史，14 天里 300 条 HEAD reflog 记录（含 10 次分支切换），worktree 里 50 个会话（5 个改到改动集里的文件），审阅对象的会话 20 轮、120 次改动调用，50 个约 300 行的改动文件 |
| 刷新 | agent 追加一轮、改一个文件后，从 `SessionsChanged` 和 fs_watch 的批次到新快照 < 20 ms |
| 勾选 | `command(MarkSeen)` 到新快照替换 < 0.2 ms（5000 个文件） |
| 空闲内存（采样状态：首页加载完成，没有打开审阅；ADR 0035） | 计数改动集按 ADR 0035 是余量里的 ≤ 4 MB，测量归 `git/changes_count/per_path_bytes`；本 crate 自己的常驻（概览、派生的声明、标注日志的索引、插件状态）cloudprint 形状 < 1 MB、10 万文件的形状 < 1 MB，从 ADR 0035 的 12 MB 余量里分配（合计 ≤ 5 MB，余量剩 7 MB 给 `project_home`、`worktrees`）。概览不经 `turns` 读轮次索引（前置条件 G），agents 的轮次索引 LRU 在采样状态里不因概览而增长 |
| 审阅打开时的内存（不在采样状态，按 ADR 0035「关键约束」各自设上限） | `ChangeList` 和它持有的 `ChangeSet`（git 的 32 MiB 保留上限）；中间版本 LRU 8 MiB；`FileRows` 只为 `Prepare` 的范围保留，合计 ≤ 8 MiB，超出时丢弃离可见范围最远的；正文 ≤ 2 MiB |
| 磁盘 | 标注日志每个仓库 ≤ 4 MiB（之后压缩）；`review.attribution` 在仓库分库的 512 MiB 预算里，按 LRU 淘汰 |

打开会话的分项（上面的夹具，M 系列；缓存全空 / 缓存已热）：

| 步骤 | 缓存全空 | 缓存已热 |
|---|---|---|
| 定位、打开句柄、读 reflog 尾部（`head_reflog`，14 天、≤ 300 条） | < 2 ms | < 2 ms |
| `ChangeSource::fetch`（git 的份额） | < 15 ms | < 15 ms |
| 审阅对象自己的会话（含子 agent）的 `turns`：agents 的 `cache` 命中加解析追加部分 / 内存命中 | < 8 ms | < 1 ms |
| 截止时刻：`review.tree_changes` 的 `get_many`，加上最多 5 ms 的未命中计算 | < 6 ms | < 1 ms |
| 其他会话的声明：`review.claims` 的一次 `get_many`（≤ 100 个键），不读轮次索引 | < 1 ms | < 1 ms |
| 第一个文件的归因：链的计算（几次 `load` 和 `line_diff`）/ `review.attribution` 命中 | < 3 ms | < 0.5 ms |
| 规则简报、组装并发布快照 | < 0.5 ms | < 0.5 ms |
| 合计 | < 35 ms | < 20 ms |

缓存全空时第一份快照可能先按保守的截止时刻和不完整的声明给出归因，后台在 `review/open_session_settled` 之内换成准确的结果。

一次链的计算：每步一次 `line_diff`（300 行的文件 < 0.1 ms）加一次 `load`（< 1 ms），一个文件 5 步约 3 ms，其中 `load` 占大头。所以缓存键只用轮次索引，命中时不 `load`。

`bench`（[#7](https://github.com/SoloJiang/lectern-app/issues/7)）新增，夹具由 `TestRepo` 和 `testing::FakeSessions` 生成，`Driven` 驱动；进程级的 `plugin/review/*` 随 bench 第 5 步：

| ID | 统计量与预算 |
|---|---|
| `review/open_session_50` | 中位数 < 35 ms（「每天的仓库规模」的夹具：2000 个提交、300 条 reflog 记录、50 个会话；新的缓存目录，`review.*` 的命名空间全空；agents 的轮次索引在 `cache` 里、不在内存里） |
| `review/open_session_50_warm` | 中位数 < 20 ms（同一夹具，`review.attribution`、`review.tree_changes`、`review.claims` 都已命中，审阅对象的轮次索引在内存里） |
| `review/open_session_settled` | 只记录（同一夹具、缓存全空：从 `open` 到后台全部完成、最后一份快照发布） |
| `review/cutoffs_cold` | 只记录（同一夹具：300 条 reflog 记录的 `tree_changes` 全部未命中时建出截止时刻表） |
| `review/overview_round_warm` | 中位数 < 20 ms（同一夹具，第二轮概览：声明和截止时刻都来自缓存，不调用 `turns_unretained` 和 `tree_changes`；后台） |
| `review/attribute_session_50` | 只记录（其余 49 个文件的归因合计） |
| `review/rows_first` | 中位数 < 3 ms（第一个文件的 `Prepare` 到 `Prepared`，hunk 缓存已热） |
| `review/refresh_one` | 中位数 < 20 ms |
| `review/mark_seen_5000` | 中位数 < 0.2 ms |
| `review/chain_3000_lines` | 只记录（3000 行的文件、40 次 `Replace`，含 `line_diff`，不含 `load`） |
| `review/log_load_16k` | 中位数 < 30 ms（16 000 条记录、约 4 MiB 的日志整份读入并建索引，后台） |
| `review/overview_cloudprint` | 只记录（cloudprint 形状，20 个会话，计数比较之后的一轮概览） |
| `review/resident_cloudprint_bytes`、`review/resident_100k_bytes` | 存活字节 < 1 MB / < 1 MB（概览就绪后本 crate 的常驻，不含计数改动集；`Memory`） |
| `review/brief_5000` | 中位数 < 5 ms（5000 个文件的规则简报） |

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)：

| 情况 | 本 crate 的结果 | 视图的处理 |
|---|---|---|
| 打开中 | `LoadState::Loading`，文件和轮次骨架先到，归因 `Pending` | 100 ms 内不显示加载态，之后显示骨架 |
| 会话日志认不出格式 | `turns` 为 None，`Degraded::UnrecognizedSession`，按会话的改动集和规则分组；归因只有命令声明 | 隐藏轮次和说明，按文件显示，不弹错误 |
| 部分记录被跳过（`Format::Partial`） | 照常；超长行里的改动拿不到，对应的行没有来源（Drifted） | 照常 |
| Drifted | hunk 的 `drifted` 与 `turns` / `others` 为空；步的 `Alignment` | 不显示归因，不做猜测 |
| 会话被删除、worktree 被删除 | `Failed(Missing)` | 显示原因，返回 |
| 会话没有归位到已打开的项目 | `Failed(NotInOpenProject)` | 同上 |
| 文件夹项目 | `Degraded::FolderProject`：只有轮次和调用记录里的改动（按日志原文），没有进度、Drifted 和最终改动 | 隐藏进度 |
| 没有 reflog | `BaseHow::CurrentHead` | 简报上方一行提示基准可能不准 |
| 改动集失败（不支持的仓库格式、没有 merge-base） | `Failed(Changes(…))`；`NoMergeBase` 时比较范围改为相对 HEAD 并在快照里注明 | 显示原因 |
| 标注日志读不出、写不进 | 已看状态全部为 `Unseen`；新的标记照常追加（写不进的在下一次重试），这期间不压缩（「标注日志」的压缩前提），下一次完整读取成功后之前的标记都在；记日志 | 照常，不弹错误 |
| 归因缓存写不进 | 忽略，下次重算 | 照常 |
| 路径不是 UTF-8 | 照常归因和标记（`ChangePath` 是字节）；不进 `focus_*` 和 `BriefRequest` | 显示时替换无效字节 |
| 不满足路径不变量的路径 | 在 `totals.invalid_paths` 里，不进文件列表 | 列出并说明「路径无效，未读取」 |

## 测试

全部在 Linux 上运行（`just test -p lectern-review`），不依赖真实的 agent、会话目录、网络和登录状态。本 crate 不调用外部命令行，不需要 PATH 上的假命令。

- 夹具：
  - 仓库：`lectern-git` 的 `TestRepo`（`test-support`），在临时目录里写基准提交、工作区文件和 reflog。
  - 会话：`testing::FakeSessions` 读本 crate 的脚本（`tests/fixtures/sessions/*.json`，合成数据）：每个会话一组轮次，每轮有 prompt、说明和调用；调用写明种类、路径、状态、时刻和改动内容（`Write`、`Replace`、`Patch`、`Add`、`Delete`），按 services 的类型实现 `turns` 和 `load`。不依赖 `agents`（插件之间不得有 dev-dependency）；两种 agent 日志到这些类型的映射已由 agents 的测试覆盖。
  - 项目：`testing::FakeProjects`。
  - 插件测试用 kernel 的 `TestKernel`、fs_watch 的 `FakeBackend`、`Executor::manual()`。
- 场景（前缀即测试名的前缀）：
  - `noise_*`：模式表每一行一个用例；`linguist-generated` 优先；`group_*`：匹配顺序（`package-lock.json` 是噪声，不是依赖）、显示顺序、按子项目排序；`sensitive_*`：表里每一种。
  - `log_*`：写入后重新读出；末尾不完整的一行被丢弃并在下次追加前截掉；坏行跳过并计数；认不出的记录在压缩后仍在；超过 4 MiB 时压缩，压缩中途失败（写临时文件出错）原文件不变；`log_no_compaction_after_failed_read`：读取失败（注入的 IO 错误）之后继续标记、追加写到超过 4 MiB，文件只追加、不压缩，之后成功读取时原有的记录都在；读取成功之后的压缩只在索引来自这次完整读取加本进程之后的追加时进行；文件头的仓库不同时改名另建；非 UTF-8 路径往返不变。
  - `seen_*`：三种状态；重命名成对匹配；旧路径被无关的新文件占用时为 `Unseen`；删除后重建为 `Unseen`；审过之后改名、删除为 `ChangedSinceSeen`；`unseen` 取消之前的 `seen`；两侧模式、身份、状态任一变化时为 `ChangedSinceSeen`；不同审阅对象的标记互不影响；`LooksGood` 之后进度为全部已看。
  - `chain_*`：五种 `EditOp` 的应用；`Replace` 找不到、出现多次时应用不上，后续步照常；`Patch` 在偏移处匹配，上下文不同时应用不上；`Write` 之后重新对齐；`move_to` 跟到新路径；CRLF 归一化重试；超过上限时为 `FileOnly`。结果（每一步的 hunk、对齐、存活）用 insta 快照。
  - `provenance_*`、`attribute_*`：最终 hunk 归到正确的轮次；两个会话先后改同一文件，`others` 指向另一个会话；会话结束后手工改动的行为 Drifted；手工删除基准里的行计入 `drifted`；只有命令声明的文件为文件级；命令窗口落在两个会话里时不归。快照覆盖 `HunkAttribution`。
  - `history_digest_*`：两个会话改成相同内容时摘要不同；同一会话在两个 worktree 里摘要相同；调用状态、适配器版本变化时摘要不同；缓存命中时不调用 `load`（`FakeSessions` 计数）。
  - `base_*`：reflog 里有不晚于会话开始的记录、全部记录都晚于会话开始、没有 reflog 三种情况；`base` 记录优先；第一次 `MarkSeen` 写入 `base` 记录（含基准记录的时间）；HEAD 之后移动，会话的基准不变。
  - `claims_*`：路径的截止时刻。
    - `claims_survive_unrelated_commit`：会话 B 改 f（不提交），用户提交一个无关的文件 g，会话 A 再改 f。B 的改动步仍在 f 的链里，A 的 `Replace` 都能应用，B 的行在 A 的审阅里是 `others`，没有 Drifted。
    - `claims_survive_checkout_b`：几个会话改了文件都没提交，之后 `git checkout -b` 新分支。这些文件仍归各自的会话，会话外的改动数不变，概览里这些会话仍在。
    - `claims_after_branch_switch`：会话 B 在 `main` 上改 f 并提交，用户在同一个 worktree 里 `git checkout feature`（f 在 `feature` 上三周前改过），再手工改 f。B 的改动步不进 f 的链，f 不在 B 的审阅和进度里，是会话外的改动。
    - 会话的改动已经提交进基准后不再参与（同样的 `Replace` 不在基准上再应用一次）；amend、rebase 之后同样；基准记录之后 agent 自己的提交不移动截止时刻，它之前的改动步仍在链里；窗口里超过 256 条记录时按第 256 条截止；没有 reflog 时按基准提交的提交时间截止；14 天之前的会话不参与。
    - 缓存：同一对提交的 `tree_changes` 只算一次（`review.tree_changes` 命中时不调用它）；`review.claims` 在会话没有新活动时命中、`last_activity` 变了不用；`Interactive` 上的未命中计算超过 5 ms 时其余记录在后台算完，快照随后更新，结果与一次算完相同。
    - 打开的顺序：其他会话的声明没有缓存时，第一份快照不等它们（`FakeSessions` 记下打开期间对它们的 `turns` / `turns_unretained` 调用数为 0），后台读到一个改了第一个文件的会话后，第一个文件的归因被重算、原来的 Drifted 行变为 `others`。
    - `claims_warm_open_uses_cached_steps`：两个会话都改了第一个文件，`review.claims` 已有另一个会话的条目。热打开期间对另一个会话没有 `turns` / `turns_unretained` 调用（只有对它的改动步的 `load`），第一份快照里它的行已经是 `others`，轮次历史的摘要与读索引算出的相同；`last_activity` 或适配器版本变了时条目不用；本次运行没读过这个来源时先用、后台核对版本不同后重算。
    - `overview_progress_matches_review_after_agent_commit`：会话在运行中提交了它改过的文件（HEAD 越过它的基准记录）。概览按它的基准记录截止，这些文件仍是它声明的，`progress.total` 等于它的审阅快照的 `files` 数；会话外的改动按最新的记录截止，不含这些已提交的文件。
  - `files_*`：会话审阅的文件集合。共用的主 worktree 里两个会话和一个手工改动的文件：每个会话的 `files` 只含它声明的文件，`unclaimed` 等于其余的数量；`LooksGood` 只标记这些文件；`Sensitive`、`Deleted` 规则不对没有声明的文件触发；概览里这个会话的 `progress.total` 等于它的审阅快照的 `files` 数；认不出格式的会话按修改时间声明。
  - `narrative_*`：同一轮多次调用合成一步；子 agent 的轮次跟在派出它的那一轮之后，`tool_call` 为 None 时按时间插入；`after_last_seen`；认不出格式时 `turns` 为 None。insta 快照。
  - `rows_*`：最终改动与叙事步的展示行，偏移与 git 的规则一致；中间版本回放后 OID 与缓存的版本 OID 相同；LRU 不超过 8 MiB。
  - `brief_*`：每条规则一个用例；排序与 12 条上限；`BriefRules` 的假提供者经 `register_brief_rules` 登记：分批答复、10 秒超时、登记 drop 或 token 失效后不再调用它且丢弃在途的答复、条目对账（不在快照里的文件丢弃、行不在显示范围内降为文件备注）。
  - `overview_*`：计数比较用 `retained_bytes = 0`、`total_patch_bytes = 0`；4001 个改动路径时只保留计数；会话外的改动数；基准不同的会话每轮最多算 8 个；放下关注后释放。`overview_never_calls_turns`：一轮概览（20 个会话）之后，`FakeSessions` 记下的 `turns` 调用数为 0，只有 `turns_unretained`；本 crate 只保留派生的声明（`review/resident_*_bytes` 覆盖它的大小）。
  - `plugin_*`：`TestKernel` 激活后提供 `dyn Review`；`open` 立即返回，快照经 `Loading` 到 `Ready`；fs_watch 批次和 `SessionsChanged` 触发刷新；kernel 在结果发布后静止；关闭 30 秒后释放；`command` 在 UI 线程上调用时不做 IO（`cache` 的 `ui_thread` 检查和 `data_dir` 的访问计数都为 0）。
  - `services` 的 `changes`、`review` 模块（前置条件 A）：类型的 JSON 往返；`dyn Review` 的 `ID == "review"`、`MULTIPLE == false`；`BriefRules` 没有实现 `Service`；`BriefRulesRegistration`、`OverviewInterest` drop 时各调用一次注销回调。
- 结构化输出（链、归因、叙事、简报、分组）都用 insta 快照验收。

## 未决问题

1. **子 agent 在另一个 worktree 里的改动。** Claude Code 的子 agent 常在 `.claude/worktrees/` 下的另一个 worktree 里工作。选项：
   - (a) 单独的审阅对象，父会话的叙事在派出它的那一轮里显示一行，点开进入它的审阅（倾向）。改动集、基准、已看进度都按那个 worktree 计算，与现在的模型一致。
   - (b) 并入父会话的审阅：一个审阅里出现两个 worktree 的文件，进度列表和基准要按 worktree 分组。

   默认按 (a) 实现。
2. **普通文件夹项目里的会话。** 文件夹没有 git，也就没有基准和最终改动。选项：
   - (a) 只显示轮次和每次调用记录的改动（按日志原文），没有已看进度和 Drifted（倾向）。
   - (b) 第一次打开会话时把它碰过的文件的当前内容存一份作为基准，之后按改动集审阅；要在应用数据目录里保存用户文件的副本。

   默认按 (a) 实现。
3. **命令期间被修改的文件是否算会话的改动。** agent 用 shell 改的文件（格式化、`npm install` 改的 lockfile、`sed -i`）在日志里没有改动内容。本文按修改时间落在命令的运行窗口里把它们归给那次调用，文件级，标为「命令期间改动」。选项：
   - (a) 按上文归给会话，不进会话外的改动（倾向）。
   - (b) 只认改动工具的改动，其余都算会话外的改动。

   默认按 (a) 实现。

技术问题都已在上文决定。

## 验收

前置条件（其他 crate 的 PR，写 `Refs #15`）：

- A. `services`：`changes`、`review` 模块；`rg -n 'pub trait ChangeSource|pub trait Review\b|pub trait BriefRules' crates/services/src` 各有一处；JSON 往返和服务 ID 的测试通过；`crates/services/AGENTS.md` 列出两个模块。
- B. `git`：`line_diff`、`Worktree::head_reflog`、`Worktree::mtimes`、`Repo::tree_changes`；`line_diff_*`、`head_reflog_*`、`mtimes_*`、`tree_changes_*` 测试通过。
- C. `git` 第 4 步：`LocalChanges` 实现 `ChangeSource`；`change_source_*` 测试通过（见「改动来源」的对照表之后一段）。
- D. `platform` 与 `app`：应用数据目录；`app` 构造 `ReviewDeps` 并把插件放进 `PluginSet`（第 4 步之前）。
- E. `cache`：命名空间表的 `review.attribution` 一行（本 PR 已加）。
- F. `code_intel`：实现 `BriefRules`，由 requires `dyn Review` 的另一个小插件登记（后续，不阻塞下面任何一步）。
- G. `agents` 与 `services`：`SessionSource::turns_unretained`；`turns_unretained_*` 测试通过（第 5 步之前）。

#15 的领域部分分五步，每步一个 PR，都写 `Refs #15`（#15 由 `review_view` 最后完成的 PR 关闭）。每一步都要 `just check`、`just test -p lectern-review`、`just deny` 通过；新增 bench 条目在这一步加进 `budgets.rs`、改为生效。

1. **噪声列表、规则分组、标注日志与已看状态**（不需要前置条件）：`noise`、`group`、`sensitive`、标注日志的读写与压缩、`SeenKey` 与三种状态。
   - `noise_*`、`group_*`、`sensitive_*`、`log_*`、`seen_*` 测试通过。
   - `review/log_load_16k` 在预算内。
   - `rg -n 'rusqlite' crates/review/Cargo.toml` 没有结果。
2. **改动链、逐行来源与归因**（前置 B 的 `line_diff`）：链的应用规则、行来源、最终 hunk 的归因、存活、轮次历史的摘要、`review.attribution` 的编码与解码。
   - `chain_*`、`provenance_*`、`attribute_*`、`history_digest_*` 测试通过，带 insta 快照。
   - `review/chain_3000_lines` 有结果。
3. **审阅引擎与快照**（前置 A、B、C）：审阅对象与基准、声明、改动叙事、快照、命令、`Prepare` 与展示行、中间版本 LRU、标注日志接入；用 `testing::Driven` 驱动，不经插件。
   - `base_*`、`claims_*`、`files_*`、`narrative_*`、`rows_*` 测试通过，其中有 `claims_survive_unrelated_commit`、`claims_survive_checkout_b`。
   - `review/open_session_50`、`review/open_session_50_warm`、`review/rows_first`、`review/mark_seen_5000` 在预算内（夹具有 2000 个提交、300 条 reflog 记录、50 个会话），`review/attribute_session_50`、`review/open_session_settled`、`review/cutoffs_cold` 有结果；[cache 设计「命名空间」](cache.md#命名空间)的 `review.attribution`、`review.tree_changes`、`review.claims` 三行与实现一致。
   - `rg -n 'std::fs' crates/review/src --glob '!**/log*' --glob '!**/tests*' --glob '!**/testing*'` 没有结果：本 crate 只在标注日志里直接访问文件系统，工作区经 `git` 读取。
4. **插件、监听与刷新**（前置 D）：`plugin = true`、`ReviewPlugin`、`dyn Review`、fs_watch 与 `SessionsChanged` 的刷新、关闭后释放。
   - `plugin_*` 测试通过；`python3 scripts/check_architecture.py` 通过：本 crate 不依赖其他插件 crate、不依赖 GPUI，只有 `app` 和 tool 层依赖它。
   - `review/refresh_one` 在预算内；`plugin/review/activate` < 1 ms。
5. **规则简报与项目主页的概览**（前置 G）：简报规则、`register_brief_rules` 与提供者的请求和对账、概览（计数比较、声明、会话外的改动数、进度）。
   - `brief_*`、`overview_*` 测试通过；`overview_capped` 检查 4001 个改动路径时只保留计数。
   - `review/brief_5000`、`review/overview_round_warm`、`review/resident_cloudprint_bytes`、`review/resident_100k_bytes` 在预算内，`review/overview_cloudprint` 有结果。

## 实际验证

前置条件 A（`services` 的 `changes`、`review` 模块），在 Linux（x86_64 容器）上运行：

- `rg -n 'pub trait ChangeSource|pub trait Review\b|pub trait BriefRules' crates/services/src` 各有一处（`changes/mod.rs`、`review/mod.rs`、`review/rules.rs`）。
- `just test -p lectern-services`：70 个测试通过，新增 17 个。
  - JSON 往返：`change_list_round_trips`、`source_types_round_trip`、`review_event_round_trips`、`targets_and_commands_round_trip`、`snapshot_round_trips`、`rows_round_trip`、`overview_and_brief_requests_round_trip`。覆盖每个枚举的每个变体，包括非 UTF-8 的 `ChangePath`、SHA-1 与 SHA-256 的 `BlobId`、`u64::MAX` 等边界值。
  - 服务 ID：`review_service_id_and_multiple` 检查 `ID == "review"`、`MULTIPLE == false`。
  - 不是服务：`brief_rules_is_not_a_service` 检查 `dyn BriefRules` 没有实现 `Service`，同一个探针对 `dyn Review` 给出 true；`change_source_is_not_a_service` 对 `dyn ChangeSource` 做同样的检查。
  - 注销回调：`brief_rules_registration_releases_once_on_drop`、`overview_interest_releases_once_on_drop` 在另一个线程上 drop，回调各调用一次。
  - 其余：`brief_reply_delivers_batches_until_cancelled` 检查分批答复与 `cancelled()`；`list_handle_is_opaque` 检查句柄不进 JSON、反序列化后为 None、比较时忽略；`change_source_is_a_shared_trait_object` 检查 `refresh`、`fingerprint` 的默认实现。
- `just check`、`just doctest`、`just deny` 通过。全量 `just test`（不依赖 GPUI 的全部 crate）第一次运行时 `lectern-syntax` 的 `object_content_nobody_holds_is_taken_out_on_the_first_screen` 失败一次（断言首屏高亮为 `On`，实际是 `Pending`）。这个 crate 不使用新模块；单独重跑三次都通过，`--no-fail-fast` 重跑全量时 1838 个测试全部通过，判断为并行负载下的时序抖动。
- 没有在 macOS 上运行：`services` 不依赖 GPUI，Linux 与 macOS 编译的代码相同。本步没有 bench 条目。

## 与设计的差异

前置条件 A：

- `ListHandle` 的比较（「接口」已补上）：`ChangeList` 派生 `PartialEq`、`Eq`、`Debug`，`#[serde(skip)]` 要求 `Default`，所以 `ListHandle` 手写这几项。两个句柄比较时总是相等，`Debug` 不打印内容。句柄只是实现方的缓存，不属于改动集的值；这样 `ChangeList` 的比较与 JSON 往返结果一致，git 第 4 步按字段核对转换结果时也不受句柄影响。
- `review` 模块的数据类型同样派生 `Clone`、`PartialEq`、`Eq`、`Debug`、`Serialize`、`Deserialize`（「接口」已补上）。设计只对 `changes` 写明了这一点，但 `crates/services/AGENTS.md`「边界」要求类型能经 JSON-RPC 传给进程插件。
- 设计没有列出的派生：除 `ChangeList`、`ReviewSnapshot` 外的数据类型都派生 `Hash`。`Review::open` 按 `ReviewTarget` 找已打开的审阅，`FileKey`、`RowsKey`、`TextKey`、`StepId` 用作缓存与视图状态的键。不含 `Arc` 的小类型派生 `Copy`。`ChangePath`、`FileKey`、`TurnRef`、`StepId`、`ReviewId` 派生 `Ord`，路径按字节序；`GroupKind`、`NoiseKind` 派生 `Ord`，按声明顺序即显示顺序。`Capabilities`、`ChangeTotals`、`Progress`、`Note` 派生 `Default`。
- `ChangePath` 用派生的序列化，JSON 里是字节数组。标注日志里「UTF-8 时写成字符串」的规则属于 `review` 自己的日志格式，不在这里实现。
- `BriefReply::send` 只调用回调，不检查 `cancelled`：丢弃过期答复由 `review` 传入的回调负责。`cancelled()` 以 `Acquire` 读取。`Debug` 输出 `cancelled` 的当前值。
