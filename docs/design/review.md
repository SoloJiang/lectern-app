# review 设计

状态：实现中。前置条件 A（`services` 的 `changes`、`review` 模块，[#161](https://github.com/SoloJiang/lectern-app/pull/161)）、B（`git` 的 `line_diff`、`head_reflog`、`mtimes`、`tree_changes`，[#162](https://github.com/SoloJiang/lectern-app/pull/162)）、C（`git` 的 `LocalChanges`，[#165](https://github.com/SoloJiang/lectern-app/pull/165)）、G（`SessionSource::turns_unretained`，[#164](https://github.com/SoloJiang/lectern-app/pull/164)）已实现；D 的 `platform` 部分（应用数据目录 `dirs::app_data`，[#166](https://github.com/SoloJiang/lectern-app/pull/166)）已实现；第 1 步（噪声列表、规则分组、标注日志与已看状态）已实现（[#167](https://github.com/SoloJiang/lectern-app/pull/167)）；第 2 步（改动链、逐行来源与归因）已实现（[#169](https://github.com/SoloJiang/lectern-app/pull/169)）；第 3 步（审阅引擎与快照，`testing::Driven` 驱动，不经插件）已实现（[#174](https://github.com/SoloJiang/lectern-app/pull/174)）；日志接入（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 8 项）已实现（[#185](https://github.com/SoloJiang/lectern-app/pull/185)，见「日志与诊断」）；第 4 步（插件、监听与刷新，`TestKernel` 与 fs_watch 的假后端测试）已实现（[#207](https://github.com/SoloJiang/lectern-app/pull/207)）；`app` 装配（前置条件 D 的 `app` 部分，[#205](https://github.com/SoloJiang/lectern-app/issues/205)）已实现（[#210](https://github.com/SoloJiang/lectern-app/pull/210)）；第 5 步的简报部分（规则简报、`BriefRules` 提供者的登记、请求与对账，`review/brief_5000`）已实现（[#211](https://github.com/SoloJiang/lectern-app/pull/211)）；概览部分（`watch_overview`、`overview`、`review/overview_round_warm`、`review/overview_cloudprint`、`review/resident_*_bytes`）已实现（[#213](https://github.com/SoloJiang/lectern-app/pull/213)）。第 5 步至此完成。`Overview.packages`（[project_home 设计「对其他 crate 的要求」](project_home.md#对其他-crate-的要求)的 A）已实现（[#239](https://github.com/SoloJiang/lectern-app/pull/239)）。

对应 issue [#15](https://github.com/SoloJiang/lectern-app/issues/15)（审阅视图的领域部分），审阅意见 [#22](https://github.com/SoloJiang/lectern-app/issues/22) 只预留数据模型。`review` 把一个审阅对象（会话、会话外的改动、worktree 相对某个基准的比较）整理成审阅视图读取的不可变快照：哪些文件改了、每一处改动来自哪个会话的哪一轮、按轮次的改动叙事、已看进度、Drifted 标记和规则生成的会话简报（[交互规范「核心闭环」](interaction.md#核心闭环)、[ADR 0012](../adr/0012.md)、[ADR 0019](../adr/0019.md)）。用户的已看标记写进按仓库分开的标注日志（[ADR 0044](../adr/0044.md)，压缩的条件按 [ADR 0045](../adr/0045.md)）。

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
  - 标注日志：已看标记和会话基准的持久化（ADR 0044、ADR 0045）。
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
| `lectern-kernel` | `Plugin`、事件、`PluginHandle`、`BusyGuard`、`Clock`（保留 30 秒的计时） | 插件机制 |
| `lectern-services` | 使用 `Projects`、`SessionSource`；提供 `Review`（含 `BriefRules` 提供者的登记）；`ChangeSource` 与数据类型 | 插件之间只经 `services`（架构规则 7） |
| `lectern-git` | `Repo`、`Worktree`、`LocalChanges`（`ChangeSource` 的本地实现）、`line_diff`、`rows`、`inline_changes`、`head_reflog`、`tree_changes`、`mtimes`、`Repo::is_ancestor`、`Oid::for_blob` | gix 只在 `git` 里使用 |
| `lectern-cache` | `review.attribution` 命名空间 | 派生数据以 blob OID 加轮次历史的摘要为键（架构规则 5） |
| `lectern-executor` | 后台任务 | UI 线程不做 IO |
| `lectern-fs-watch` | 被审阅 worktree 的 `WorktreeBatch`、仓库的 `RepoBatch` | 与 `project` 共用同一份 worktree 状态 |
| `lectern-platform` | `time::utc_offset`：概览按本地自然日统计今天改动的文件（`Overview::packages`） | 基础设施，`project`、`code_intel` 也直接依赖；时区只在 `platform` 里读 |
| `lectern-logging` | 运行日志（「日志与诊断」）；测试用它的 `test-support` feature 捕获记录 | kernel 层，只依赖 `log`（[ADR 0046](../adr/0046.md)） |
| `serde`、`serde_json` | 标注日志的记录 | 已是工作区依赖（ADR 0044） |
| `memchr` | 标注日志整份读入时找行尾 | 已是工作区依赖（`syntax`、`code_intel` 用它找行尾和 `memmem`），也已在本 crate 的依赖树里（`serde_json`、gix 依赖它），不增加 Cargo.lock 的条目 |
| `sha2` | 轮次历史的摘要、标注日志的文件名 | 已在 Cargo.lock（`cache` 用同一版本） |
| `insta`、`tempfile`（dev）；`lectern-git` 的 `test-support`（dev） | 快照、临时目录、`TestRepo` | 工作区统一依赖 |

不依赖 `agents`、`project`、`code_intel`（插件之间不得依赖，dev-dependency 也算）：测试用本 crate `testing` 里的假会话来源和假项目服务。对 `platform` 只用 `time::utc_offset`（本地时区的偏移，在 `Background` 的概览轮里调用；`ReviewPlugin::with_utc_offset` 在测试里换成固定偏移）：工作区只经 `git` 读取，应用数据目录由 `app` 传入，`ReviewDeps` 没有新字段。不新增外部依赖：上表里的外部 crate 都已在 Cargo.lock 里。

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
    /// 当前 worktree 里今天改动的文件按子项目的计数：计数改动集（ADR 0035）里工作区一侧 stat 的 mtime
    /// 落在本地自然日（含今天，与 usage 设计「取整与时间窗口」的日界相同）内的路径，按 `ProjectSnapshot::active.packages`
    /// 的 `dir` 最长前缀归到一个子项目，不在任何子项目里的不计。`OutsideCount::Capped` 时为 None（没有改动集）；
    /// `packages` 还是 `Pending`（或 `Unavailable`）时为 None。只列计数不为 0 的子项目，按 `dir` 排序；没有列出的子项目计 0。
    pub packages: Option<Arc<[(RelPath, u32)]>>,
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

impl SessionOverview {
    /// 项目主页的 Needs Review 与 Welcome 的待审数共用的判断：`progress` 为 None、`seen < total` 或 `changed_since_seen > 0`
    /// （[#283](https://github.com/SoloJiang/lectern-app/issues/283) 的画布决定，[welcome 设计「待审数」](welcome.md#待审数)）。
    pub fn needs_review(&self) -> bool;
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
    /// 只有快照的 `Projects`：一个仓库项目，worktree 来自 `TestRepo`。`with_packages`、`set_packages` 给快照的 `active.packages` 一组子项目目录（`Ready`），没给时为 `Pending`。
    pub struct FakeProjects;
    /// 不经插件，在调用线程上驱动审阅引擎（`Executor::manual()`）。bench 的 `review/*` 用它。`set_utc_offset` 给本地时区一个固定偏移（默认 UTC），`project_changed` 递一个 `ProjectEvent::Changed` 的 `ChangedParts`。
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

- **路径的截止时刻**（关键约束）：取这个 worktree 的 HEAD reflog，从基准记录（含）往前、时间在 14 天的窗口之内、旧值与新值不同的记录，加上基准记录之后离开了基准所在历史的记录（基准提交不是这条记录新值的祖先，`Repo::is_ancestor`；结果按（基准提交，新值）缓存在 `review.ancestry`），合计最多 256 条。截止时刻是其中最近一条让这个路径在旧、新两个提交的树之间不同的记录（`Repo::tree_changes`，前置条件 B）的时间。没有基准记录（`BaseHow::BeforeLog`，或固定下来的基准没有记录）时只有后一类；基准是空树（unborn）时后一类为空。
  - 这几类 HEAD 移动会改变这个路径的基准内容，截止时刻因此后移，之前的会话改动不再参与：提交了这个路径、切换到这个路径内容不同的分支（`git checkout feature`）、`reset`、`rebase`、`pull`。
  - `git checkout -b`（旧值等于新值）、提交别的文件（树在这个路径上相同）不移动截止时刻，所以还没提交的会话改动留在链里。
  - 窗口里没有任何记录改变这个路径时没有截止时刻，只受 14 天的窗口限制。窗口里的记录超过 256 条时，其余路径按第 256 条的时间截止（保守：只会少算会话的改动，多出的行显示为 Drifted 或会话外的改动）。
  - 没有 reflog（`BaseHow::CurrentHead`）时，全部路径按基准提交的提交时间截止（已知限制：这时提交无关的文件也会让之前未提交的会话改动离开链）。
  - 基准记录之后、新值仍以基准提交为祖先的记录不参与：agent 自己的提交、amend、`pull --rebase` 或 `rebase origin/main`（基准在上游的历史里）、`reset --soft` 之后再提交的合并提交、把 main 合并进来、快进的 `pull`。会话审阅的基准是会话开始时的提交，这些移动带来的改动都在基准到工作区的改动里，之前的改动步要留在链里。新值不以基准为祖先的记录参与，典型是切换到一个无关的分支：路径在工作区里的内容换成了另一段历史的，之前的改动步对不上（`claims_after_branch_switch`）。按动作区分（只让提交不参与）不行：`pull --rebase`、合并、`reset --soft` 也会改变 agent 改过的路径，按动作截止会把 agent 自己的改动步全部切掉。
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
| worktree 的截止时刻表 | 路径（只为改动集里的路径保留）；值是窗口里改变了这个路径的 reflog 记录的时间，从新到旧（≤ 256 个） | 内存，概览的关注者或打开的审阅持有时存在，随 reflog 的新记录（fs_watch 的 `RepoBatch::reflog`、`meta.head`）增量更新，不每轮重算。截止时刻随基准而定，表本身不含基准：查询时给出这个会话的记录列表，即基准记录（含）往前的记录，加上基准记录之后新值不以基准提交为祖先的记录（「改动步与声明」），取其中最近一条改变了这个路径的。会话外的改动和概览里基准是当前 HEAD 的会话用最新的记录往前（之后没有记录）；基准不是当前 HEAD 的会话，概览和它自己的审阅都用它自己的基准记录和基准提交选记录，所以两边的声明相同 |
| 一个提交是不是另一个的祖先（`Repo::is_ancestor`，选基准记录之后的记录用） | 哈希算法、祖先一侧的 OID、另一个提交的 OID | `cache` 的仓库分库，命名空间 `review.ancestry`，不压缩；一个字节。只由两个提交 OID 决定（架构规则 5）；本次运行内另有内存里的一份 |
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
{"t":"seen","at":1760000000000,"target":{"session":{"source":"session-claude-code","id":"…"}},"key":{"old_path":"src/a.rs","new_path":"src/a.rs","old":{"mode":"file","content":{"blob":"…"}},"new":{…},"status":"modified"}}
{"t":"unseen","at":…,"target":…,"file":{"old":null,"new":"src/a.rs"}}
{"t":"base","at":…,"session":{"source":"…","id":"…"},"commit":"<40 或 64 位十六进制；空树（unborn）为 null>","entry_at":<基准记录的时间，Unix 秒；没有时为 null>}
```

- 路径是 UTF-8 时写成字符串，否则写成字节数组。OID 写成十六进制。`at` 是 Unix 毫秒。`target` 的另两种是 `{"outside":{"worktree":"…"}}`、`{"range":{"worktree":"…","base":"head" | {"commit":"…"} | {"merge_base":"refs/heads/main"}}}`；`content` 是 `blob`、`unhashed`、`filtered`（`sha256` 或 `stat`）、`unavailable` 之一，`NotRetained` 记作 `blob`（见「已看状态」）。日志的编码由本 crate 自己定义，不随 `services` 类型的序列化变化。
- 文件头的 `repo` 与当前仓库不同（文件被复制、哈希碰撞）时把文件改名为 `<h>.jsonl.foreign-<时刻>` 后新建，不读它的记录。
- 读取：打开项目的概览或审阅时，在 `Background` 任务里整份读入（上限 64 MiB，超过时只读最后 64 MiB 并从第一个完整行开始）。坏行跳过并计数；认不出的 `t` 原样保留；末尾没有换行的一行丢弃，下一次追加之前截掉它。本进程不知道文件末尾的状态时（读取失败、别的写入者追加过）不截断，文件不以换行结尾就先补一个换行，残行成为一个坏行，新记录不受影响。文件头的 `v` 不是 1 或文件头坏了时照常读记录，但只追加、不压缩。
- 内存里的索引：每个审阅对象、每个文件标识（`new_path`，没有时 `old_path`）只保留最近一条 `seen` 或 `unseen`，加上每个会话的 `base`（同一会话有多条时取最早的一条，它是已看标记所依据的基准）。重命名（`old_path` 与 `new_path` 不同）的 `seen` / `unseen` 同时撤掉这个审阅对象在 `old_path` 下的记录：那个改动已经成了这次重命名，之后 `old_path` 上的新文件不是同一个改动，对重命名的 `unseen` 也不会被旧路径上的 `seen` 抵消。重放日志得到同样的索引，压缩不需要额外的记录。项目关闭后释放。
- 写入：命令在内存里生效后把记录放进待写队列；队列在 100 ms 内没有新记录、或累计 64 条时，由一个 `Background` 任务（持有 `BusyGuard`）追加并 `fdatasync`。写失败时保留队列，下一次再试，记日志；进程退出前没写完的标记丢失（已知限制）。
- 压缩：文件超过 4 MiB、且超过本进程上一次压缩后长度的两倍时（本进程还没压缩过时只看 4 MiB；[ADR 0045](../adr/0045.md)），在下一次写入的任务里写一份只含文件头、内存索引里的记录和认不出的记录的新文件，`fsync` 后改名替换并 `fsync` 目录。超过一年的 `seen` / `unseen` 记录在压缩时丢弃。两倍的条件让仍然有效的记录本身超过 4 MiB 时不会每次写入都重写整个文件：重写的代价摊到至少同样多的追加上；压缩的前提成立时，文件不超过 max(4 MiB, 有效记录的两倍) 加一批记录。改名之后目录的 `fsync` 失败时，本进程把文件当作状态未知（只追加，直到下一次完整读取），下一次追加时再 `fsync` 目录。
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
| `Sensitive(AgentInstructions)` | 路径是 `CLAUDE.md`、`CLAUDE.local.md`、`AGENTS.md`、`GEMINI.md`、`.mcp.json`、`.github/copilot-instructions.md`，或在 `.claude/`、`.codex/`、`.cursor/`、`.gemini/` 下 | 是 | 每个文件一条（这些文件会被 agent 当作指令加载，ADR 0019 约束 3） |
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
  2. 声明：声明窗口内的会话（「改动步与声明」，最多 100 个）先查 `review.claims`；没有命中（新会话、有新活动）的才读轮次索引，用 `SessionSource::turns_unretained`（前置条件 G）在 `Background` 上逐个读取，不用 `turns`：概览在 ADR 0035 的采样状态里运行，`turns` 会把这些索引留在 agents 的 8 MB 轮次索引 LRU 里，而那个 LRU 不在采样状态的份额里。每读一个会话，只留下派生的声明（同「改动步与声明」）并写进 `review.claims`，索引随即放下；声明计入本 crate 的 < 1 MB。截止时刻用这个 worktree 在内存里的截止时刻表（「归因的缓存」）：第一轮从 `review.tree_changes` 建出，之后只为 reflog 的新记录和计数改动集里新出现的路径补算，不每轮重算；每个会话按它自己的基准记录和基准提交选出记录再查表（基准记录往前的，加上之后新值不以基准提交为祖先的，`review.ancestry`；基准是当前 HEAD 时用最新的记录往前），与它的审阅相同（同一个 `cutoff::window`）；每轮对计数改动集的路径调一次 `Worktree::mtimes`（≤ 4000 个），命令声明和第 6 项共用这一次 stat。
  3. 会话外的改动数 = 计数改动集里没有声明的文件。
  4. 每个会话的进度：文件集合与审阅快照的 `files` 同一定义（「审阅对象与基准」的文件集合）。基准等于当前 HEAD 的会话，文件是计数改动集与它的声明的交集，已看状态按标注日志算出；基准不同的会话，每轮最多为 8 个（按 `last_activity`）另做一次同样 `Limits` 的比较，算完即丢弃改动集，其余为 None。
  5. Drifted 信号只读 `review.attribution` 的缓存（键能从声明和计数改动集的 OID 得到），未命中为 None；不为概览建链。
  6. 今天改动的文件按子项目计数（`Overview::packages`）：这一轮开头读到的 `ProjectSnapshot::active.packages` 为 `Ready` 时，计数改动集里工作区一侧的 mtime（第 2 项那一次 stat；已删除的文件没有 mtime，不计；重命名取新路径）落在本地自然日内的路径，按 `dir` 的最长前缀（按路径段比较：`crates/a` 不含 `crates/ab/x`；`dir` 为 "" 的根子项目含所有路径）归到一个子项目，不在任何子项目里的不计。本地自然日用 `platform::time::utc_offset` 算：路径的 mtime 与这一轮的「现在」（`Config::clock`）各加上自己那一刻的偏移，除以一天的毫秒数后相等，与 usage 的 `window::local_date` 同一规则（夏令时切换日是 23 或 25 小时）。`packages` 为 `Pending`、`Unavailable`，或改动路径超过 4000 个（`Capped`）时为 None；发布时只列计数不为 0 的子项目，`dir` 与快照里的是同一个 `Arc`。计数随下一轮刷新，不在午夜自动归零（project_home 设计的约定）。
- 重算的时机：fs_watch 的批次、`SessionsChanged`、标注日志的写入、`ProjectEvent` 的 worktree 变化或子项目变化（`ChangedParts::packages`，复用计数改动集），合并 2 秒后在 `Background` 上跑一轮。关注者都放下后释放计数改动集和声明。

### 日志与诊断

经 `lectern-logging` 的宏记录（[ADR 0046](../adr/0046.md)、[logging 设计](logging.md)），target 是 `review`。默认过滤记到 info；详细级别是用户在设置里开启的 debug。记录在调用线程上编码进日志的内存缓冲，不做 IO；持有本 crate 的锁时写出的记录只多一次复制（[logging 设计「数据流与线程」](logging.md#数据流与线程)）。字段值的写法集中在 `src/diag.rs`：错误只记种类，不记消息文字。

- 默认记录：

  | 事件 | 级别 | `msg` | 字段 |
  |---|---|---|---|
  | 改动集取不到（取消不记） | warn | `change set unavailable` | `error`（`ChangeError` 的种类：`unsupported`、`no_merge_base`、`too_many_entries`、`io`） |
  | 审阅对象或它的子 agent 的轮次读不出（审阅照常进行，认不出格式时为 `Degraded::UnrecognizedSession`） | warn | `session turns unavailable; reviewed without them` | `from`（会话标识）、`error`（`SessionError` 的种类：`io`、`unrecognized`、`too_large`；子 agent 的会话文件不在了时为 `not_found`）、`target`（是不是审阅对象本身） |
  | HEAD reflog 读不出，基准按当前 HEAD | warn | `head reflog unreadable; the base is the current head` | `error`（`GitError` 的种类）、`io` |
  | 改动步加载失败或给出的不是改动，这一步按应用不上处理 | warn | `change step did not load; it does not apply` | `from`、`pos`、`line_len`、`part`（输入的 `ContentRef`）、`error`（`not_edits` 或 `SessionError` 的种类） |
  | `tree_changes` 失败，这条记录按改变了全部路径处理 | warn | `tree changes failed; the record cuts every path` | `old`、`new`（两个提交的 OID）、`error`、`io` |
  | `is_ancestor` 失败，这条记录按离开了基准的历史处理 | warn | `ancestry check failed; the record cuts the paths it changed` | `ancestor`、`commit`、`error`、`io` |
  | 标注日志读取失败，读成功之前只追加 | warn | `annotation log read failed; append only until the next read` | `path`（日志文件）、`io` |
  | 文件头的仓库不符，文件被改名 | warn | `annotation log of another repository; renamed` | `path`、`renamed`（改名后的路径） |
  | 文件超过 64 MiB，只读最后 64 MiB | warn | `annotation log over the read limit; read its end only` | `path`、`bytes`（文件长度）、`read`（读取上限） |
  | 文件头的 `v` 不是 1，只追加 | warn | `annotation log of another version; append only` | `path` |
  | 读取时跳过坏行、丢弃末尾不完整的一行 | warn | `annotation log recovered` | `path`、`bad_lines`（跳过的坏行数，含坏的文件头）、`torn_bytes`（末尾不完整的一行的字节数，下一次追加之前截掉）、`unknown`（保留的认不出的记录数） |
  | 追加失败，记录留在待写队列 | warn | `annotation log append failed; kept for the next mark` | `path`、`io`、`records`（这一批的条数）、`failures`（连续失败的次数） |
  | 失败之后再次追加成功 | warn | `annotation log append retried` | `path`、`records`（含之前留下的）、`failures` |
  | 压缩失败，原文件不变 | warn | `annotation log compaction failed; the file is unchanged` | `path`、`io` |
  | 压缩改名之后目录的 `fsync` 失败，只追加 | warn | `annotation log directory sync failed; append only` | `path`（目录）、`io` |
  | 压缩完成 | info | `annotation log compacted` | `path`、`before`、`after`（压缩前后的字节数）、`records`（保留的记录数）、`took_us` |
  | 压缩前发现别的写入者改过文件，只追加 | info | `annotation log changed by another writer; append only` | `path`、`expected`、`found`（预期与实际的长度） |
  | 打开审阅到第一份 `Ready` 快照 | info | `review ready` | `id`（`ReviewId`）、`round`、`base`（`BaseHow`：`recorded`、`reflog`、`before_log`、`current_head`、`spec`）、`files`、`took_us`（从 `open` 起），分项 `locate_us`（定位、句柄与 reflog）、`turns_us`、`fetch_us`、`claims_us`、`cutoffs_us`、`first_us`（第一个文件的归因） |
  | 后台完成（全部文件归因完、没有待补齐的部分），每次打开记一次 | info | `review settled` | `id`、`round`、`files`、`reprepared`（因声明或截止时刻补齐而重新准备的轮数）、`took_us`（从 `open` 起） |
  | 一轮准备失败（快照为 `Failed`；已被 `close` 或更新的一轮取消的不记） | info | `review failed` | `id`、`round`、`kind`、`error`（`missing`、`not_in_open_project` 或改动集错误的种类） |
  | 比较范围没有 merge-base，改为相对 HEAD | info | `no merge base; the range compares with head` | — |
  | 概览的计数比较失败（取消不记），这一轮发布空概览（`Pending`） | warn | `overview comparison failed` | `error`（`GitError` 的种类）、`io` |
  | 概览读不出计数改动集里文件的修改时间（`Worktree::mtimes` 失败），这一轮没有命令声明，今天改动的计数都是 0 | warn | `modification times unreadable` | `error`（`GitError` 的种类）、`io` |
  | 简报规则的提供者 10 秒没有答复 `done`，按完成处理 | info | `brief provider timed out` | `id`、`request`（这个审阅的第几次请求）、`registration`（提供者的登记序号；插件 ID 是文本，不进日志） |

  - 会话标识（`from` 和作用域的 `session`）：会话 ID 是 UUID 时取它的 16 个字节，否则（Claude Code 子 agent 的 `<父会话>/agent-<id>`）取 128 位哈希，与 `usage` 的算法相同，两个 crate 对同一会话的记录可以对照。来源 ID 不写出。
  - `path` 都经 `Value::path`，主目录写成 `~`。默认级别里的路径只有标注日志的文件和目录（应用数据目录下本 crate 的文件）。
  - 同一个调用点每 10 秒最多写 20 条（[logging 设计「限流」](logging.md#限流)）：一个坏掉的仓库反复失败时，省略的条数记在下一条的 `n` 里。
- 详细级别（debug）：

  | 事件 | `msg` | 字段 |
  |---|---|---|
  | 每一轮准备完 | `review round` | `id`、`round`、`kind`（`fetch`、`refresh_full`、`refresh_paths`、`reuse`）、`files`、`unclaimed`、`pending`（有没有留给后台的部分）、`took_us` |
  | 其他会话的声明 | `claims` | `sessions`、`memory`（本次运行内存里的）、`cached`（`review.claims` 命中）、`stale`（条目的 `last_activity` 或适配器版本不同）、`missing`（留给后台读取）、`unverified` |
  | 后台读不出一个会话的轮次，声明这一轮仍未知 | `claims not derived` | `from`、`error` |
  | 截止时刻表 | `cutoff table built` | `records`、`paths`、`hits`（`review.tree_changes` 命中）、`computed`、`left`（超出 `Interactive` 预算或失败、留给后台的记录数）、`took_us` |
  | 后台补齐截止时刻表 | `cutoff table completed` | `records`、`unknown`（仍未知的）、`took_us` |
  | 一个文件的归因 | `file attributed` | `file`（快照里的下标）、`path`（工作区里的路径）、`cached`（`review.attribution` 命中）、`steps`、`hunks`、`drifted_hunks` |
  | `Prepare` 完成 | `prepared` | `id`、`request`（请求的序号）、`needs`、`rows`、`texts`、`row_bytes`、`text_bytes`、`versions_bytes`（中间版本 LRU）、`took_us` |
  | 监听或事件要求一轮（fs_watch 的批次、`SessionsChanged`） | `refresh requested` | `id`、`how`（`paths`、`full`、`sessions`、`fetch`）、`paths`（路径批次的路径数，其余为 0）；在审阅的作用域里写出 |
| 关闭 30 秒后释放 | `review released` | `id` |
  | 向简报规则的提供者发出请求（每一轮的工作一次） | `brief requested` | `id`、`request`、`providers`（登记的提供者数）、`files`（请求里的文件数） |
  | 提供者交回一批条目 | `brief reply` | `id`、`request`、`registration`、`items`（这个提供者累计保留的条目数）、`done` |
  | 概览的一轮结束 | `overview round` | `sessions`（窗口内有声明数据的会话数）、`shown`（列出的会话数）、`outside`、`capped`、`packages`（有今天改动的子项目数；`Overview::packages` 为 None 时为 null）、`took_us`；作用域的 `review` 是 SHA-256(仓库哈希, "overview") 的前 8 字节 |
  | 概览的触发 | `overview requested` | `how`（`resolve`、`full`、`paths`、`reuse`） |
  | 最后一个关注放下，状态释放 | `overview released` | — |
| 标注日志读完 | `annotation log read` | `path`、`records`、`unknown`、`complete`（索引是不是整个文件，决定能否压缩）、`took_us` |
  | 追加之前截掉末尾不完整的一行 | `annotation log tail cut` | `path`、`bytes` |
  | 文件状态未知、不以换行结尾，追加之前补一个换行 | `annotation log of unknown state; newline added before the records` | `path` |

- 关联 ID：本 crate 的每个 executor 任务（一轮的准备、后台补齐与归因、`Prepare`、标注日志的读取与追加）都在作用域里运行（`Engine::submit` 设置 `Ctx`）。`session` 是审阅对象会话的标识，从 `open` 起就有；`review` 是审阅对象的 64 位哈希，取 SHA-256（仓库的哈希 `h` 的 16 个字节、标注日志里 `target` 的编码）的前 8 字节，同一审阅对象跨运行相同，在一轮解析出仓库之后加上。标注日志按仓库共用，它的读取和追加任务带发起它的那个审阅的作用域。`task` 由 executor 填（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 5 项）。
- 不记录：文件内容、diff 的行、改动步的正文（`load` 的结果）、prompt、agent 说明、命令文本、提交说明（reflog 的说明文字）、标注日志的行（坏行、认不出的记录照原样留在文件里，不进日志）；来源 ID，以及不是 UUID 的会话 ID 的原文（只记哈希）；worktree 的名字、分支名、`Prepare` 的键（含路径和会话里的位置）；`ChangeError::Io`、`SessionError::Io`、`GitError::Corrupt` 的消息文字和 `io::Error` 的消息（只记种类和系统错误码）；凭据和 token。工作区里的路径只在详细级别的 `file attributed` 里出现。
- 金丝雀测试（`src/engine/tests/logs.rs`，`testing::capture()` 在 trace 级别）：`logs_hold_no_contents` 把金丝雀放进基准和工作区的文件内容、提交说明、审阅对象会话的 prompt、agent 说明和改动内容、另一个会话的 prompt 和命令、标注日志的坏行、认不出的记录和末尾不完整的一行，以及注入的追加错误的消息里；打开审阅、`Prepare` 展示行和 prompt 正文、标记已看（截掉残行、压缩）、追加失败、再次标记（重试成功），再打开会话外的改动和比较范围，确认 `annotation log recovered`、`annotation log compacted`、追加失败与重试几条记录的字段，以及它们和 `review ready` 都带 `review`、`session`，任何记录都不含金丝雀。`logs_tree_changes_failure_is_a_warn`：reflog 里一条记录的新提交不在仓库里，`tree_changes` 在交互任务和后台各失败一次，各记一条 warn，带两个 OID 和 `object_missing`。`logs_unreadable_turns_are_a_warn`：认不出格式的会话记一条 warn，`error` 为 `unrecognized`。`logs_round_closed_before_it_ran_is_not_a_failure`：打开后一轮还没运行就 `close`，这一轮被取消，不记 `review failed`。`logs_session_tag_is_the_uuid_or_a_hash`：UUID 取原字节，子 agent 的 ID 取哈希。

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
| 磁盘 | 标注日志每个仓库 ≤ max(4 MiB, 一年内仍然有效的记录的两倍) 加一批记录，在压缩的前提成立时（「标注日志」的压缩，[ADR 0045](../adr/0045.md)）；只追加时（读取失败或只读了一部分、文件头坏了或 `v` 不是 1、别的写入者改过文件、压缩改名之后目录的 `fsync` 失败）文件一直增长，到下一次完整读取恢复压缩为止；`review.attribution` 在仓库分库的 512 MiB 预算里，按 LRU 淘汰 |

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

`bench`（[#7](https://github.com/SoloJiang/lectern-app/issues/7)）新增，夹具由 `TestRepo` 和 `testing::FakeSessions` 生成，`Driven` 驱动；`plugin/review/activate` 在进程级插件基准（bench 第 5 步）之前在进程内由 `TestKernel` 测量，第 5 步起是 `Process`（进程级的 `plugin/*/activate`，`closure+self:review` 的一次启动里 kernel 记下的四段耗时之和）：

| ID | 统计量与预算 |
|---|---|
| `review/open_session_50` | 中位数 < 35 ms（「每天的仓库规模」的夹具：2000 个提交、300 条 reflog 记录、50 个会话；新的缓存目录，`review.*` 的命名空间全空；agents 的轮次索引在 `cache` 里、不在内存里） |
| `review/open_session_50_warm` | 中位数 < 20 ms（同一夹具，`review.attribution`、`review.tree_changes`、`review.claims` 都已命中，审阅对象的轮次索引在内存里） |
| `review/open_session_settled` | 只记录（同一夹具、缓存全空：从 `open` 到后台全部完成、最后一份快照发布） |
| `review/cutoffs_cold` | 只记录（同一夹具：300 条 reflog 记录的 `tree_changes` 全部未命中时建出截止时刻表） |
| `review/overview_round_warm` | 中位数 < 20 ms（同一夹具，第二轮概览：声明和截止时刻都来自缓存，不调用 `turns_unretained` 和 `tree_changes`；后台） |
| `review/attribute_session_50` | 只记录（其余 49 个文件的归因合计） |
| `review/rows_first` | 中位数 < 3 ms（第一个文件的 `Prepare` 到 `Prepared`，hunk 缓存已热） |
| `review/refresh_one` | 中位数 < 20 ms（`Driven` 上 agent 追加一轮并改写第一个改动文件：路径批次和 `SessionsChanged` 到第一份同时显示新一轮和新内容的快照；不含 fs_watch 的合并窗口） |
| `plugin/review/activate` | 中位数 < 1 ms（kernel 记下的四段耗时之和；激活没有任务也没有文件由 `plugin_provides_review_and_activation_does_nothing` 验证） |
| `review/mark_seen_5000` | 中位数 < 0.2 ms |
| `review/chain_3000_lines` | 只记录（3000 行的文件、40 次 `Replace`，含 `line_diff`，不含 `load`） |
| `review/log_load_16k` | 中位数 < 30 ms（16 000 条记录、约 5.3 MiB 的日志整份读入并建索引，后台） |
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
  - `log_*`：写入后重新读出；末尾不完整的一行被丢弃并在下次追加前截掉；坏行跳过并计数；认不出的记录在压缩后仍在；超过 4 MiB 时压缩，压缩中途失败（写临时文件出错）原文件不变；`log_no_compaction_after_failed_read`：读取失败（注入的 IO 错误）之后继续标记、追加写到超过 4 MiB，文件只追加、不压缩，之后成功读取时原有的记录都在；读取成功之后的压缩只在索引来自这次完整读取加本进程之后的追加时进行；有效记录本身超过上限时压缩一次之后，到文件超过压缩后长度的两倍之前只追加；改名之后目录的 `fsync` 失败时只追加；重放日志时重命名撤掉旧路径下的记录；文件头的仓库不同时改名另建；非 UTF-8 路径往返不变。
  - `seen_*`：三种状态；重命名成对匹配；旧路径被无关的新文件占用时为 `Unseen`；删除后重建为 `Unseen`；审过之后改名、删除为 `ChangedSinceSeen`；`unseen` 取消之前的 `seen`；审过修改、文件又被改名之后，对重命名标 `unseen` 为 `Unseen`，旧路径上之后出现的新文件为 `Unseen`；两侧模式、身份、状态任一变化时为 `ChangedSinceSeen`；不同审阅对象的标记互不影响；`LooksGood` 之后进度为全部已看。
  - `chain_*`：五种 `EditOp` 的应用；`Replace` 找不到、出现多次时应用不上，后续步照常；`Patch` 在偏移处匹配，上下文不同时应用不上；`Write` 之后重新对齐；`move_to` 跟到新路径；CRLF 归一化重试；超过上限时为 `FileOnly`。结果（每一步的 hunk、对齐、存活）用 insta 快照。
  - `provenance_*`、`attribute_*`：最终 hunk 归到正确的轮次；两个会话先后改同一文件，`others` 指向另一个会话；会话结束后手工改动的行为 Drifted；手工删除基准里的行计入 `drifted`；只有命令声明的文件为文件级；命令窗口落在两个会话里时不归。快照覆盖 `HunkAttribution`。
  - `history_digest_*`：两个会话改成相同内容时摘要不同；同一会话在两个 worktree 里摘要相同；调用状态、适配器版本变化时摘要不同；缓存命中时不调用 `load`（`FakeSessions` 计数）。
  - `base_*`：reflog 里有不晚于会话开始的记录、全部记录都晚于会话开始、没有 reflog 三种情况；`base` 记录优先；第一次 `MarkSeen` 写入 `base` 记录（含基准记录的时间）；HEAD 之后移动，会话的基准不变。
  - `claims_*`：路径的截止时刻。
    - `claims_survive_unrelated_commit`：会话 B 改 f（不提交），用户提交一个无关的文件 g，会话 A 再改 f。B 的改动步仍在 f 的链里，A 的 `Replace` 都能应用，B 的行在 A 的审阅里是 `others`，没有 Drifted。
    - `claims_survive_checkout_b`：几个会话改了文件都没提交，之后 `git checkout -b` 新分支。这些文件仍归各自的会话，会话外的改动数不变，概览里这些会话仍在。
    - `claims_after_branch_switch`：会话 B 在 `main` 上改 f 并提交，用户在同一个 worktree 里 `git checkout feature`（f 在 `feature` 上三周前改过），再手工改 f。B 的改动步不进 f 的链，f 不在 B 的审阅和进度里，是会话外的改动。
    - 会话的改动已经提交进基准后不再参与（同样的 `Replace` 不在基准上再应用一次）；amend、rebase 之后同样；基准记录之后 agent 自己的提交、`pull --rebase`、`reset --soft` 之后的合并提交、把 main 合并进来都不移动截止时刻，它们之前的改动步仍在链里（`claims_survive_pull_rebase_after_an_agent_commit`、`claims_survive_a_squash_by_reset_soft`、`claims_survive_merging_main_into_the_agent_branch`）；窗口里超过 256 条记录时按第 256 条截止；没有 reflog 时按基准提交的提交时间截止；14 天之前的会话不参与。
    - 缓存：同一对提交的 `tree_changes` 只算一次（`review.tree_changes` 命中时不调用它）；`review.claims` 在会话没有新活动时命中、`last_activity` 变了不用；`Interactive` 上的未命中计算超过 5 ms 时其余记录在后台算完，快照随后更新，结果与一次算完相同。
    - 打开的顺序：其他会话的声明没有缓存时，第一份快照不等它们（`FakeSessions` 记下打开期间对它们的 `turns` / `turns_unretained` 调用数为 0），后台读到一个改了第一个文件的会话后，第一个文件的归因被重算、原来的 Drifted 行变为 `others`。
    - `claims_warm_open_uses_cached_steps`：两个会话都改了第一个文件，`review.claims` 已有另一个会话的条目。热打开期间对另一个会话没有 `turns` / `turns_unretained` 调用（只有对它的改动步的 `load`），第一份快照里它的行已经是 `others`，轮次历史的摘要与读索引算出的相同；`last_activity` 或适配器版本变了时条目不用；本次运行没读过这个来源时先用、后台核对版本不同后重算。
    - `overview_progress_matches_review_after_agent_commit`：会话在运行中提交了它改过的文件（HEAD 越过它的基准记录）。概览按它的基准记录截止，这些文件仍是它声明的，`progress.total` 等于它的审阅快照的 `files` 数；会话外的改动按最新的记录截止，不含这些已提交的文件。
  - `files_*`：会话审阅的文件集合。共用的主 worktree 里两个会话和一个手工改动的文件：每个会话的 `files` 只含它声明的文件，`unclaimed` 等于其余的数量；`LooksGood` 只标记这些文件；`Sensitive`、`Deleted` 规则不对没有声明的文件触发；概览里这个会话的 `progress.total` 等于它的审阅快照的 `files` 数；认不出格式的会话按修改时间声明。
  - `narrative_*`：同一轮多次调用合成一步；子 agent 的轮次跟在派出它的那一轮之后，`tool_call` 为 None 时按时间插入；`after_last_seen`；认不出格式时 `turns` 为 None。insta 快照。
  - `rows_*`：最终改动与叙事步的展示行，偏移与 git 的规则一致；中间版本回放后 OID 与缓存的版本 OID 相同；LRU 不超过 8 MiB。
  - `brief_*`：规则是纯函数 `brief::build`，每条规则一个用例、排序与 12 条上限、提供者条目的对账在 `src/brief/tests.rs`（手工构造的 `FileChange`，不经仓库）；引擎侧（`src/engine/tests/brief.rs`）：规则的输入来自审阅（CI 文件与 lockfile、失败的改动调用、命令声明），`BriefRules` 的假提供者经 `register_brief_rules` 登记：一轮只请求一次、分批答复、10 秒超时（`Driven::with_timers` 的手动 `Host`）、登记 drop 或 token 失效后不再调用它且丢弃在途的答复、条目对账（不在快照里的文件丢弃、行不在显示范围内降为文件备注）、晚登记的提供者对已打开的审阅补问、从 `started` 里登记（token 还是 `Pending`）的提供者在 50 ms 的重试后被问到。提供者的 `ActivationToken` 来自 `testing::host()`（一个只交出 `PluginHandle` 的 kernel），`Pending` 的情形用一个在 `started` 里登记的测试插件。
  - `overview_*`（`src/engine/tests/overview.rs`）：`overview_lists_the_sessions_with_claims_and_counts_the_outside`（会话、进度、会话外的改动数；审阅算过归因之后 `drifted` 来自缓存）、`overview_uses_zero_retention_limits`（计数改动集的每个文件都是 `Listed`）、`overview_capped`（4001 个改动路径时只有计数、不保留改动集）、`overview_never_calls_turns`（20 个会话一轮之后 `FakeSessions::calls` 的 `turns` 为 0、`turns_unretained` 为 1；第二轮从内存取声明，不再读索引）、`overview_compares_at_most_eight_other_bases_per_round`、`overview_progress_matches_review_after_agent_commit`、`overview_is_released_with_the_last_interest`、`overview_triggers_merge_within_two_seconds`（`Driven::with_timers`，三个触发合成一轮）、`overview_mark_seen_updates_the_progress`、`overview_without_an_active_worktree_is_pending`；`plugin_overview_follows_the_interest`（插件经 `TestKernel`）。
  - `overview_packages_*`（同一个文件，合成的仓库、`FakeProjects::set_packages`、`Driven::set_utc_offset`，文件的 mtime 经 `TestRepo::set_mtime` 定为毫秒）：`overview_packages_assign_by_longest_prefix`（嵌套的子项目取最长前缀、`crates/ab` 不属于 `crates/a`、已删除的文件和不在任何子项目里的文件不计、没有文件的子项目不列出、按 `dir` 排序）、`overview_packages_the_root_package_holds_the_rest`（`dir` 为 "" 的子项目）、`overview_packages_count_only_the_local_day_of_the_modification_time`（UTC+8 与 UTC 下，本地零点前 1 ms 不计、零点计、次日零点前 1 ms 计、次日零点不计）、`overview_packages_is_empty_when_nothing_changed_today`（`Some` 的空列表）、`overview_packages_none_when_capped`（4001 个路径）、`overview_packages_none_while_packages_are_pending`、`overview_packages_follow_the_projects_packages`（`Pending` 转 `Ready` 后，只带 `packages` 的 `ChangedParts` 触发一轮并得到计数；只带 `files` 的不触发）。`SessionFixture` 给 `FakeProjects` 十个子项目（`src/m00`…`src/m09`），所以 `review/overview_round_warm` 含这一项的归类。
  - `plugin_*`（`src/plugin/tests.rs`，另有 `src/engine/watch.rs` 里批次到请求的映射和请求合并的单元测试）：`TestKernel` 激活后提供 `dyn Review`、清单符合「依赖」，激活不提交任务、不监听、不写文件（`plugin_provides_review_and_activation_does_nothing`）；`open` 立即返回，快照经 `Loading` 到 `Ready`，同一对象同一个 ID（`plugin_open_returns_at_once_…`）；fs_watch 的路径批次、`rescan` 和 `SessionsChanged` 触发刷新，批次的路径进 `RefreshHint::Paths`、`rescan` 为 `Full`（`plugin_a_watch_batch_…`、`plugin_a_path_batch_…`、`plugin_sessions_changed_…`）；运行期间来的请求合成一轮，路径取并集（`plugin_requests_during_a_round_…`、`plugin_merge_…`、`plugin_worktree_batches_…`）；kernel 在结果发布后静止，轮次排队时不静止（`plugin_kernel_is_quiescent_…`）；关闭后保留 30 秒、期间重新打开得到同一个审阅且不重算、第一次关闭的定时器不释放第二次关闭之后的审阅、到期后释放快照、监听和版本 LRU（`plugin_close_keeps_…`、`plugin_a_kept_review_…`）；停用后监听和审阅都释放（`plugin_deactivation_…`）；`command` 在 UI 线程上调用时不做 IO（`plugin_commands_on_the_ui_thread_do_no_io`：`cache` 的 `ui_thread` 设为测试线程，任何落在这个线程上的缓存调用在调试构建里 panic，任务都放到另一个线程上运行；标注日志的目录在后台任务运行之前不存在）；概览随第一个关注计算、随最后一个关注放下而消失（`plugin_overview_follows_the_interest`，第 5 步）。测试不用墙上时钟：30 秒是 kernel 的 `ManualClock`，合并窗口是 fs_watch 的假时钟，任务在测试运行 executor 时才执行。
  - `logs_*`（`src/engine/tests/logs.rs`，用 `lectern-logging` 的 `testing::capture()`）：金丝雀与 warn 路径的记录，见「日志与诊断」。标注日志的 IO 故障和尺寸上限经 `Engine::set_log_test`（`cfg(test)`）作用在引擎之后打开的日志上。
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
- D. `platform` 与 `app`：应用数据目录（`platform` 部分已实现）；`app` 构造 `ReviewDeps` 并把插件放进 `PluginSet`（[#205](https://github.com/SoloJiang/lectern-app/issues/205)）。`ReviewDeps` 和 `plugin()` 由第 4 步定义，第 4 步的测试用 `TestKernel`，所以顺序是第 4 步、`app` 装配、第 5 步：`app` 装配在第 4 步之后、第 5 步之前。
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
4. **插件、监听与刷新**（前置 A、B、C 与第 3 步；不依赖 `app`）：`plugin = true`、`ReviewPlugin`、`dyn Review`、fs_watch 与 `SessionsChanged` 的刷新、关闭后释放。
   - `plugin_*` 测试通过；`python3 scripts/check_architecture.py` 通过：本 crate 不依赖其他插件 crate、不依赖 GPUI，只有 `app` 和 tool 层依赖它。
   - `review/refresh_one` 在预算内；`plugin/review/activate` < 1 ms（原来在进程内由 `TestKernel` 测量，bench 第 5 步起是进程级的测量）。
   - `rg 'eprintln!|util::log' crates/review/src --glob '!**/tests*'` 和 `rg -n 'std::fs' crates/review/src --glob '!**/log*' --glob '!**/tests*' --glob '!**/testing*'` 仍没有结果。
5. **规则简报与项目主页的概览**（前置 G）：简报规则、`register_brief_rules` 与提供者的请求和对账、概览（计数比较、声明、会话外的改动数、进度）。
   - `brief_*`、`overview_*` 测试通过；`overview_capped` 检查 4001 个改动路径时只保留计数。
   - `review/brief_5000`、`review/overview_round_warm`、`review/resident_cloudprint_bytes`、`review/resident_100k_bytes` 在预算内，`review/overview_cloudprint` 有结果。

日志接入（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 8 项，`Refs #170`）：

- `rg 'eprintln!|util::log' crates/review/src --glob '!**/tests*'` 没有结果。
- `logs_*` 通过，其余测试照常通过。
- `just bench --measure review/` 与改动前相比不变慢。

## 实际验证

### 前置条件 A

`services` 的 `changes`、`review` 模块，在 Linux（x86_64 容器）上运行：

- `rg -n 'pub trait ChangeSource|pub trait Review\b|pub trait BriefRules' crates/services/src` 各有一处（`changes/mod.rs`、`review/mod.rs`、`review/rules.rs`）。
- `just test -p lectern-services`：70 个测试通过，新增 17 个。
  - JSON 往返：`change_list_round_trips`、`source_types_round_trip`、`review_event_round_trips`、`targets_and_commands_round_trip`、`snapshot_round_trips`、`rows_round_trip`、`overview_and_brief_requests_round_trip`。覆盖每个枚举的每个变体，包括非 UTF-8 的 `ChangePath`、SHA-1 与 SHA-256 的 `BlobId`、`u64::MAX` 等边界值。
  - 服务 ID：`review_service_id_and_multiple` 检查 `ID == "review"`、`MULTIPLE == false`。
  - 不是服务：`brief_rules_is_not_a_service` 检查 `dyn BriefRules` 没有实现 `Service`，同一个探针对 `dyn Review` 给出 true；`change_source_is_not_a_service` 对 `dyn ChangeSource` 做同样的检查。
  - 注销回调：`brief_rules_registration_releases_once_on_drop`、`overview_interest_releases_once_on_drop` 在另一个线程上 drop，回调各调用一次。
  - 其余：`brief_reply_delivers_batches_until_cancelled` 检查分批答复与 `cancelled()`；`list_handle_is_opaque` 检查句柄不进 JSON、反序列化后为 None、比较时忽略；`change_source_is_a_shared_trait_object` 检查 `refresh`、`fingerprint` 的默认实现。
- `just check`、`just doctest`、`just deny` 通过。全量 `just test`（不依赖 GPUI 的全部 crate）第一次运行时 `lectern-syntax` 的 `object_content_nobody_holds_is_taken_out_on_the_first_screen` 失败一次（断言首屏高亮为 `On`，实际是 `Pending`）。这个 crate 不使用新模块；单独重跑三次都通过，`--no-fail-fast` 重跑全量时 1838 个测试全部通过，判断为并行负载下的时序抖动。
- 没有在 macOS 上运行：`services` 不依赖 GPUI，Linux 与 macOS 编译的代码相同。本步没有 bench 条目。

### 第 1 步

在 Linux（x86_64，4 核，内核 6.18，与其他编译任务共用机器）上运行。本步不依赖 GPUI，也没有 macOS 专属代码。

- 用的是前置条件 A（#161）合并进来的 `services::changes`、`services::review` 类型，本 crate 没有自己的替身。
- `just check`、`just test`（整个工作区 1949 个测试通过，跳过 7 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。
- `just test -p lectern-review`：55 个测试通过。按前缀：`noise_*` 6、`group_*` 4、`sensitive_*` 8、`seen_*` 14、`log_*` 23（含 `log_no_compaction_after_failed_read`；重命名撤掉旧路径的记录有 `seen_unseen_of_a_rename_is_unseen`、`seen_new_file_at_the_old_path_after_a_seen_rename_is_unseen`、`log_rename_retires_the_old_path_on_replay`；压缩的两倍条件有 `log_compaction_waits_for_twice_the_live_size`；目录 `fsync` 失败有 `log_failed_directory_sync_goes_append_only`）。insta 快照 4 个（日志的行格式、压缩后保留认不出的记录的文件、噪声与分组的路径表、按子项目排序的分组），逐个看过。
- `log_no_compaction_after_failed_read`：注入的读取错误之后继续标记，追加到超过压缩上限（测试里调小为 8 KiB，写到 32 KiB），文件的 inode 不变、原有字节仍在开头；之后一次完整读取里原有的 20 个已看标记和基准都在，这次读取之后的写入恢复压缩。`log_compaction_needs_the_index_to_be_the_file` 覆盖另外几种只追加的情况：读取之后有别的写入者追加、本进程追加之后到压缩之前有别的写入者（压缩前的身份核对拦下，没有开始写临时文件）、只读了最后一部分（`READ_MAX` 调小）、文件头的 `v` 为 2。`log_compacts_above_4_mib` 用真实的 4 MiB 上限。
- `review/log_load_16k`（`cargo bench -p lectern-bench --all-features --bench time -- review/`，采样模式）：中位数 22.8 ms，p95 27.6 ms，42 个样本，预算 < 30 ms；换用 `services` 的类型之后重测，中位数 23.4 ms，p95 26.6 ms（机器负载约 7）；读取时重命名撤掉旧路径下的记录之后重测，中位数 22.6 ms。夹具 16 000 条记录、5 556 267 字节。第一版每行解析两遍（先取类型再按类型解析）时中位数是 32.5 ms，改为一遍解析、重复的字符串（来源、会话 ID、worktree）共用一份之后降到上面的数字。没有在 macOS 上测量。
- `rg -n 'rusqlite' crates/review/Cargo.toml` 没有结果。
- 没有验证的：待写队列的 100 ms / 64 条合并、`Background` 任务与 `BusyGuard`、首帧之前不读，属于第 3 步接入审阅引擎时的内容；本步只提供 `LogFile`、`Index` 和 `flush`。

### 第 2 步

在 Linux（x86_64，4 核，与其他编译任务共用机器）上运行。本步是纯计算加缓存值的编解码，不做 IO（测试里的 `cache` 在临时目录），不依赖 GPUI。

- `Cargo.toml` 加 `lectern-git`（`line_diff`、`Oid::for_blob`）和 `lectern-cache`（`review.attribution`），都是「依赖」表里已有的；gix 只经 `git` 使用。
- `just test -p lectern-review`：86 个测试通过，新增 31 个。按前缀：`chain_*` 21（五种操作；`Replace` 找不到、出现多次时应用不上而后续步照常；`Patch` 在偏移处匹配、上下文不同时应用不上、没有行号的 hunk；`@@ <上下文>` 把改动放进两个相同代码块里的第二个、连续的 `@@ <上下文>` 依次定位、上下文行不计缩进和行尾空白（逐字节相同的优先）、上下文之后的纯插入放在文件末尾；`Write` 之后重新对齐；`move_to` 跟到新路径；用命令改名之后改新路径、同一个补丁里删旧加新、先写新路径，链都跟到新路径；CRLF 归一化重试；一次调用整体应用、没有完成的调用不应用；超过 200 步、超过 4 MiB、工作区一侧不是文本时为文件级；缓存值截断、多余字节、越界的步骤下标都解码为未命中；存活的三种结果；bench 夹具）、`provenance_*` 2（会话结束后手工加的行、手工删掉的基准行都计入 `drifted`）、`attribute_*` 4（最终 hunk 归到各自的轮次；两个会话先后改同一文件，同一条链在两边的审阅里互为 `others`；叙事步的 `Aligned` / `Partial` / `Unaligned`；文件级归给内容 OID 相同的 `Write`）、`history_digest_*` 4（两个会话改成相同内容时摘要和键都不同；同一会话的历史在主 worktree 和一个链接 worktree 里（日志里是两个根下的绝对路径，转成相对路径之后）摘要和链都相同，`Blob` 与 `NotRetained` 的键相同；状态、适配器版本、轮次、调用、位置、重命名链、顺序任一变化时不同；命中时不调用计算链的闭包，文件级的 `Sides` 结果不写缓存）。insta 快照 13 个（链的每一步、叙事步的 hunk 与存活、工作区每行来源、基准每行去向，加上最终 hunk 与文件的归因），逐个看过；每个快照的链同时做一次缓存值的编码往返。
- `review/chain_3000_lines`（`just bench --measure review/`，采样模式）：中位数 19.8 ms，p95 21.4 ms，51 个样本，只记录；链跟随重命名、`@@ <上下文>` 定位改过之后重测，中位数 18.5 ms，p95 20.3 ms，54 个样本（差别在机器噪声之内）。夹具 3000 行（153 780 字节）、40 次 `Replace` 分在 8 轮里，每一步一次 `line_diff` 和一次版本的 SHA-1（`Oid::for_blob`），另有 8 个叙事步的 `line_diff` 和最后与工作区的比较，不含 `load`。同一次运行里 `review/log_load_16k` 中位数 21.9 ms（预算 < 30 ms）。没有在 macOS 上测量，也没有拆分 `line_diff` 与哈希各占多少。
- `just check`、`just test`（整个工作区 2000 个测试通过，跳过 7 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。
- 没有验证的：改动步的选取、路径的截止时刻、命令声明和 `review.attribution` 的读写时机属于第 3 步；本步的链只由测试直接构造输入驱动。「性能」表里第一个文件的链 < 3 ms（300 行、5 步）没有单独测量，按 3000 行 40 步的结果推算远在其内。

### 第 3 步

在 Linux（x86_64，4 核，与其他编译任务共用机器）上运行。审阅引擎不经插件，由 `testing::Driven` 在 `Executor::manual()` 上驱动；本步不依赖 GPUI，也没有 macOS 专属代码。

- `Cargo.toml` 加 `lectern-executor`（「依赖」表里已有）；`test-support` 打开 `lectern-executor` 与 `lectern-git` 的 `test-support`（`Executor::manual`、`TestRepo`）。没有新的外部依赖，`Cargo.lock` 只多了这条内部依赖。
- `just test -p lectern-review`：117 个测试通过，新增 31 个（其中 5 个是自审之后加的：基准记录之后的 `pull --rebase`、`reset --soft` 合并提交、合并 main 不切掉 agent 的改动步；刷新之后、补齐后的那一轮之后展示行重新准备；两个「第一份快照不等」的测试改为检查第一份快照确实是临时的结果）。按前缀：`base_*` 3（reflog 里不晚于会话开始的记录、记录都晚于会话开始、没有 reflog；第一次 `MarkSeen` 写进 `base` 记录（含基准记录的时间），HEAD 之后移动、旧记录消失，新引擎上仍是 `Recorded` 的基准和已看标记）、`claims_*` 19（`claims_survive_unrelated_commit`、`claims_survive_pull_rebase_after_an_agent_commit`、`claims_survive_a_squash_by_reset_soft`、`claims_survive_merging_main_into_the_agent_branch`、`claims_survive_checkout_b`（含会话外的改动和比较范围）、`claims_after_branch_switch`、已提交进基准的改动不再应用、没有 reflog 时按基准提交的时间截止、14 天之前的会话不参与、只有命令声明的文件为文件级、命令窗口落在两个会话里时不归、`Interactive` 预算为 0 时其余记录在后台算完且结果与一次算完相同、`review.tree_changes` 命中时不再计算、其他会话没有缓存时第一份快照不等它们（对它们只有 `turns_unretained`，没有 `turns`）、`claims_warm_open_uses_cached_steps`（热打开对另一个会话的 `turns`、`turns_unretained`、`load` 都是 0；`last_activity`、适配器版本变了条目不用）、本次运行没读过的来源先用后核对；另有 `review.claims`、`review.tree_changes` 值的编码往返，路径相对化，截止时刻的记录窗口与每路径查询）、`files_*` 2（主 worktree 里两个会话和一个手工改动：每个会话的 `files` 只含它声明的文件、`unclaimed` 为其余的数量、`LooksGood` 只标记这些文件；认不出格式的会话按修改时间声明）、`narrative_*` 2（同一轮对同一文件的两次调用合成一步、子 agent 的轮次跟在派出它的那一轮之后、`tool_call` 为 None 时按时间插入；`after_last_seen`）、`rows_*` 5（最终改动与叙事步的展示行、回放出的中间版本 OID 与链里的版本 OID 相同、新的 `Prepare` 丢掉不在请求里的行；刷新之后展示行按新内容重新准备；冷打开时临时一轮的展示行被补齐之后那一轮的替换；prompt 的正文；版本 LRU 不超过上限）。insta 快照 6 个（基准、`claims_survive_unrelated_commit`、只有命令声明、叙事、最终改动的行、叙事步的行），逐个对照场景手算核对过。
- `rg -n 'std::fs' crates/review/src --glob '!**/log*' --glob '!**/tests*' --glob '!**/testing*'` 没有结果。
- [cache 设计「命名空间」](cache.md#命名空间)的四行与实现一致（`review.ancestry` 是本步新加的，见「与设计的差异」）：`review.ancestry`（仓库分库，不压缩，键是哈希算法、祖先一侧与另一个提交的 OID，值一个字节）、`review.attribution`（仓库分库，Zstd，版本 1，键里没有工作区一侧时为空，见「与设计的差异」）、`review.tree_changes`（仓库分库，Zstd，键是哈希算法、旧提交（全 0 时为空）、新提交、`max_paths` 20 000）、`review.claims`（应用分库，不压缩，键是来源 ID、会话 ID，值带 `last_activity` 和适配器版本）。
- 基准（`just bench --measure review/`，采样模式，与其他任务共用机器）：

  | ID | 结果 | 预算 |
  |---|---|---|
  | `review/open_session_50` | 中位数 9.7 ms | < 35 ms |
  | `review/open_session_50_warm` | 中位数 4.9 ms | < 20 ms |
  | `review/open_session_settled` | 中位数 130 ms | 只记录 |
  | `review/cutoffs_cold` | 中位数 157 ms | 只记录 |
  | `review/attribute_session_50` | 中位数 15.6 ms | 只记录 |
  | `review/rows_first` | 中位数 126 µs | < 3 ms |
  | `review/mark_seen_5000` | 中位数 21 µs（报告标为 noisy） | < 0.2 ms |

  这是自审修正（祖先判断、换轮时丢掉展示行）之后在 origin/main 上重测的结果，机器负载约 6；修正之前的一次 `cutoffs_cold` 是 86.8 ms，`cutoffs_cold` 不经祖先判断，差别按机器负载看待。同一次运行里 `review/log_load_16k` 中位数 23 ms（< 30 ms），`review/chain_3000_lines` 17.4 ms。`Driven` 只在 `capture_snapshots` 之后保留每份快照：一直保留时 `mark_seen_5000` 留下几千份 5000 项的已看数组，测到 319 µs，测的是夹具而不是引擎。夹具 `testing::SessionFixture`：2000 个提交（1700 个共用一棵树，300 个各改历史文件里的一个）、300 条 HEAD reflog 记录（其中 10 次分支切换）、50 个会话（审阅对象 20 轮 120 次改动调用，另 4 个会话在它之前改过其中 40 个文件，其余 45 个会话改别的文件）、50 个 300 行的改动文件；`review/mark_seen_5000` 用 `testing::MarkSeenFixture` 的 5000 个未跟踪文件。冷打开时 300 条记录的 `tree_changes` 只有 5 ms 的预算，其余留给后台（`cutoffs_cold` 说明一次算完约 87 ms），所以第一份快照的截止时刻是保守的，`open_session_settled` 包含后台补齐后的重新准备和其余 49 个文件的归因。
- `just check`、`just test`（不依赖 GPUI 的全部 crate，2033 个测试通过，跳过 7 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。
- 没有验证的：
  - 冷打开的计时不含 agents 的轮次索引从 `cache` 读出与解析的部分（「性能」分项表 < 8 ms 那一行）：`FakeSessions` 的索引一直在内存里。缓存的数据库文件在计时之前已经打开（`Driven::touch_cache`），建库的耗时不计入。
  - `review/attribute_session_50` 不含 `review.attribution` 的读写；`review/cutoffs_cold` 含新缓存目录的建库。
  - 30 秒的保留、fs_watch 与 `SessionsChanged` 的刷新、`BusyGuard` 与插件的静止属于第 4 步；简报规则与概览属于第 5 步。文件夹项目的会话没有测试（`FakeProjects` 只有仓库项目）。
  - 没有在 macOS 上运行和测量。

### 第 4 步

在 macOS（Darwin，本机，与其他任务共用）上运行，没有在 Linux 上运行。`Cargo.toml` 加 `lectern-kernel`、`lectern-fs-watch`（「依赖」表里已有）并设 `plugin = true`，dev-dependencies 加两者的 `test-support`；`Cargo.lock` 只多了 `lectern-review` 到这两个 crate 的边，没有新的外部 crate。

- `just check`（格式、架构规则、文档链接、clippy 警告即失败）、`python3 scripts/check_architecture.py`（32 个 crate）、`just deny`（advisories、bans、licenses、sources 都通过）通过。
- `just test -p lectern-review`：138 个测试通过，新增 14 个：`plugin_*` 12 个（`src/plugin/tests.rs`：提供服务与激活不做事、`open` 立即返回与 `Loading` 到 `Ready`、路径批次刷新、批次的种类（路径 1 个、`rescan` 为 `full`）、`SessionsChanged` 刷新、运行期间的请求合成一轮、kernel 静止、关闭保留 30 秒与释放、保留期间监听仍刷新、UI 线程上的命令不做 IO、停用释放监听、概览与简报规则的空操作）和 `src/engine/watch.rs` 里 2 个（请求合并的规则、批次到 `RefreshHint` 的映射）。原有的 124 个照常通过。
- `rg 'eprintln!|util::log' crates/review/src --glob '!**/tests*'` 和 `rg -n 'std::fs' crates/review/src --glob '!**/log*' --glob '!**/tests*' --glob '!**/testing*'` 都没有结果。
- 基准（`just bench --measure review/refresh_one`，采样模式，运行七次）：`review/refresh_one` 中位数 2.62 到 3.18 ms（预算 < 20 ms）；夹具是第 3 步的 2000 个提交、300 条 reflog 记录、50 个会话，审阅已完成，agent 追加一轮并改写 `src/m00/f0.rs`（300 行）。`plugin/review/activate`（`TestKernel` 下 kernel 的四段耗时之和）中位数 1.6 到 1.7 µs（预算 < 1 ms，标为 noisy），激活之后 executor 里没有任务、数据目录不存在。同一次运行里第 3 步的基准：`open_session_50` 15.8 ms、`open_session_50_warm` 5.9 ms、`rows_first` 228 µs、`mark_seen_5000` 15.9 µs、`log_load_16k` 10.2 ms，都在预算内（本机比第 3 步的 Linux 机器快，数字不可比）。
- 没有验证的：
  - 真实的 FSEvents 和多线程 executor：测试全部用 `Executor::manual()`、`FakeBackend` 和 fs_watch 的假时钟。fs_watch 的过滤器是 `git` 对 `TestRepo` 的真实实现，不是 `FakeSource`。
  - 30 秒的定时器在测试里是 `ManualClock`；kernel 的 `SystemClock`（定时线程）只在 `plugin()` 里装配，没有测试走到。
  - `review/refresh_one` 不含 fs_watch 的合并窗口（50 ms 静默、500 ms 上限），也不含批次经 `FakeBackend` 到引擎的路径：基准把批次和事件直接交给 `Driven`。
  - 第一个监听批次（`rescan`）造成的那一次 `Full` 刷新的耗时没有单独测量。
  - kernel 静止的测试用 `barrier` 加最多 2 秒的轮询（同 `agents` 的 `plugin_*`），断言的是「排队的轮次存在时不静止、结果发布并且批次交付后静止」，不是耗时。
  - 与 `app` 的装配没有测试（`app` 部分是 [#205](https://github.com/SoloJiang/lectern-app/issues/205)）。

### 第 5 步（概览）

在 macOS（Darwin，本机）上运行。

- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过。
- `just test -p lectern-review`：170 个测试通过，新增 10 个 `overview_*`（`src/engine/tests/overview.rs`，名字见「测试」）；`plugin_overview_and_brief_rules_are_empty_until_step_5` 改为 `plugin_overview_follows_the_interest`。`lectern-bench` 的 77 个测试照常通过（预算表与文档一致，`review/resident_*_bytes` 归 `Memory`）。
- 基准（`just bench --measure review/overview`、`just bench --measure review/resident`）：`review/overview_round_warm` 中位数 7.11 ms（预算 < 20 ms；第 3 步的夹具，50 个会话，第二轮，`full` 触发到 `Overview` 事件）；`review/overview_cloudprint` 中位数 38.1 ms（只记录；cloudprint 形状 728 个文件整个写出，20 个会话各改一个文件，第二轮）；`review/resident_cloudprint_bytes` 中位数 20.2 KB、`review/resident_100k_bytes` 中位数 3.45 KB（预算各 < 1 MB；扣掉 `Driven` 空闲的字节和计数改动集自己的字节）。
- 没有验证的：真实的 fs_watch 批次和 `ProjectEvent`（测试直接调用触发；插件里的订阅与 `ctx.on::<ProjectEvent>` 只有编译检查）；`project_home` 的使用（[#30](https://github.com/SoloJiang/lectern-app/issues/30)）；多个 worktree 之间切换 `active`（`FakeProjects` 只有主 worktree 的测试）。

### 第 5 步（简报）

在 macOS（Darwin，本机）上运行。

- `just check`（含 clippy 警告即失败、架构规则、文档链接）通过。
- `just test -p lectern-review`：160 个测试通过，新增 22 个：`src/brief/tests.rs` 14 个（每条规则一个、关键在前与表的顺序、12 条上限、提供者条目的对账与每提供者 8 条）、`src/engine/tests/brief.rs` 8 个（规则来自审阅、失败的改动调用、命令声明、提供者一轮一次请求与分批答复与新一轮的取消、10 秒超时、登记 drop、token 失效、晚登记补问、从 `started` 里登记的 `Pending` token）。`lectern-bench` 的 77 个测试照常通过（预算表与文档一致）。
- 基准：`just bench --measure review/brief_5000` 中位数 270 µs（预算 < 5 ms）；夹具 5000 个文件，每条规则都有命中，保留 12 条、其余计入 `omitted`。
- 没有验证的：真实的提供者（code_intel 的前置条件 F 还没有实现）；提供者在 UI 线程上同步答复的情形（设计要求 `evaluate` 立即返回，答复在 executor 任务里重新装配快照）。

### 日志接入

在 Linux（x86_64，4 核，与其他 agent 的编译任务共用机器，Rust 1.97.0）上运行，基于合并了 #182、#183、#184 的 main。

- `just check`、`just test`（不依赖 GPUI 的全部 crate，2218 个测试通过，跳过 7 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。`cargo test -p lectern-review --all-features --lib` 在同一个进程里跑全部 122 个测试也通过（捕获器由各测试共用，`logs_*` 按自己的会话 ID 或临时目录筛选记录）。
- `just test -p lectern-review`：原有的 117 个测试照常通过，新增 `logs_hold_no_contents`、`logs_tree_changes_failure_is_a_warn`、`logs_unreadable_turns_are_a_warn`、`logs_round_closed_before_it_ran_is_not_a_failure`（第一版在检查这一轮是否仍是当前一轮之前就记 `review failed`，被 `close` 取消的一轮记成 `error` 为 `cancelled` 的失败，自审时改正，这个测试在改正之前失败）、`logs_session_tag_is_the_uuid_or_a_hash`。`logs_hold_no_contents` 里 `annotation log recovered` 的 `bad_lines` 为 1、`unknown` 为 1、`torn_bytes` 等于写进去的残行长度；压缩记录的 `after` 小于 `before`；追加失败记 `failures` 1、`records` 1，下一次标记的重试记 `records` 2；这四条和 `review ready` 都带 `review`（与 `diag::review_ctx` 算出的相同）和 `session`（会话的 UUID）。
- `rg 'eprintln!|util::log' crates/review/src --glob '!**/tests*'` 没有结果（本 crate 原来就没有 stderr 输出）。Cargo.lock 只在 `lectern-review` 的依赖里多了 `lectern-logging`。
- `just bench --measure review/`，改动前是 origin/main 的同一个提交（另一个分离的 worktree），依次运行改动前、改动后、改动前、改动后，之后对三项再各补一对：

  | 基准 | 改动前 | 改动后 |
  |---|---|---|
  | `log_load_16k` | 28.1、28.6 ms | 26.8、25.5 ms |
  | `chain_3000_lines` | 18.7、19.1、25.1、25.4 ms | 19.8、30.7、20.5、25.6 ms |
  | `open_session_50` | 10.5、9.82 ms | 10.2、10.8 ms |
  | `open_session_50_warm` | 5.65、5.81 ms | 5.71、5.85 ms |
  | `open_session_settled` | 153、171 ms | 143、165 ms |
  | `cutoffs_cold` | 111、124 ms | 106、139 ms |
  | `attribute_session_50` | 16.3、17.9、18.3、23.3 ms | 19.7、19.4、19.7、18.9 ms |
  | `rows_first` | 186、209 µs | 147、160 µs |
  | `mark_seen_5000` | 23.8、26.2、27.3、39.1 µs | 28.3、28.6、26.5、31.4 µs |

  改动前后的差别都在同一组运行里的波动之内：`chain_3000_lines` 测的代码没有改动，同一边的几次运行在 18.7 到 30.7 ms 之间。`attribute_session_50` 前三对改动后高约 1.5 ms，它计时的路径（`attribute::build` → `step_inputs` → `load`）不经过任何写出的日志调用；把 `load` 的 warn 移进 `#[cold]` 函数之后的一对是 18.9 ms 对 23.3 ms。只跑 `review/` 时 `log/` 的基准不运行、进程里没有装日志器，`log::max_level()` 为 off，每个调用点只多一次级别比较。没有在 macOS 上测量。
- 没有验证的：
  - `is_ancestor` 失败的 warn 没有测试触发：reflog 里的新提交不在仓库里时，`merge_base` 找不到共同祖先，返回「不是祖先」而不报错（`logs_tree_changes_failure_is_a_warn` 的注释）。改动步加载失败、HEAD reflog 读不出、改动集取不到的记录没有测试触发；压缩失败与目录 `fsync` 失败的路径由 `log_failed_compaction_leaves_the_file`、`log_failed_directory_sync_goes_append_only` 走到，但没有断言记录。这些记录的字段只经 `Value` 的构造函数和 `diag` 的种类，金丝雀测试覆盖了同一套写法。
  - 记录写进真实日志文件要等 `app` 接入（logging 设计第 3 步）；这里只经 `testing::capture()` 检查编码后的行。`task` 要等 executor 的第 5 项。

### 标注日志读取的余量

背景：一个没有改动 review 的 PR 上，macOS CI 的 `Benchmarks (macOS arm64)` 两次报 `review/log_load_16k` 超标（中位数 35.2 ms、32.2 ms，预算 < 30 ms），同一提交重跑通过，当时 runner 有容量告警。在 Linux（x86_64，4 核，与其他 agent 的编译任务共用机器）上运行。

- 指令分布（valgrind callgrind，origin/main 的基准程序，每次读取 238.8 M 条指令）：逐行解析占 75%，其中 `serde_json` 对每个字符串的 UTF-8 检查（`from_utf8`）占读取的 14%，字符串扫描另占 13%；逐字节找换行占 12%；建索引（`Index::apply`）占 11%，其中 SipHash 是大头；共用字符串的哈希集合约 5%；读文件和清零缓冲区约 2%。
- 改动：按行切分用 `memchr` 找换行；整行是 UTF-8 时按 `str` 解析，`serde_json` 不再逐个字符串检查 UTF-8，不是 UTF-8 的行仍按字节解析，结果与原来相同；共用字符串时先比较最近交出的 4 个，再查哈希集合。格式、坏行与残行的处理、压缩的前提都不变。
- 改动后每次读取 175.7 M 条指令（−26%）：解析 141.5 M，UTF-8 检查从 33.7 M 降到 4.4 M（整行一次），找换行（`LogFile::load` 自身加 `memchr`）从 28.6 M 降到 2.5 M。剩下的大头是 `serde_json` 本身的解析（字符串扫描、字段名匹配、OID 的十六进制）和建索引的哈希。
- `review/log_load_16k`（基准程序直接运行 `--bench --measure review/log_load_16k`，origin/main 与本改动的程序交替运行）：

  | 机器负载 | origin/main | 本改动 |
  |---|---|---|
  | 约 6 | 27.9、26.7、26.7、27.3、29.0、26.9、26.7、26.5 ms | 22.8、23.5、20.2、35.8、21.3、24.1、20.1、20.6 ms |
  | 约 60 | 41.5、38.2、40.2 ms | 29.6、34.6、38.9 ms |

  负载约 6 时 origin/main 的中位数约 26.8 ms（预算的 89%），本改动约 22 ms（73%）；35.8 ms 那一次按机器上其他任务的抖动看待：前后相邻的 origin/main 是 27.3、29.0 ms，后者是 origin/main 八次里最高的。墙钟时间降得比指令数少（约 18% 对 26%），去掉的是逐字节比较、短字符串检查这类简单循环，每条指令的耗时低于剩下的解析。负载约 60 时 origin/main 每次都超标，与 macOS CI 的失败一致：预算本身不变，问题是余量太小。夹具 16 000 条记录、5 556 267 字节。
- `just test -p lectern-review`：124 个测试通过，新增 `log_lines_not_utf8_are_bad_or_kept`（不是 UTF-8 的行：已知类型里有非 UTF-8 字符串的是坏行，未知类型的原样保留、压缩后仍在）和 `log_read_shares_strings_seen_long_before`（离开最近 4 个之后再出现的字符串仍共用一份）。原有的 `log_*` 测试（坏行、残行、认不出的记录、读取失败后只追加）不改动照常通过。
- `just check`、`just test`（不依赖 GPUI 的全部 crate，2257 个测试通过，跳过 7 个）、`just doctest`、`just deny` 通过。
- 没有验证的：没有在 macOS 上测量。指令数的比例在 macOS（arm64）上应当相近，但 CI runner 的抖动可能仍会让个别运行接近预算；进一步的余量要换掉 `serde_json` 的逐行解析或建索引用的哈希（后者是依赖选型），本次没有做。

### Overview.packages（project_home 的前置条件 A）

在 macOS（Darwin，本机，Rust 1.97.0）上运行，基于 main 的 8a7e93e；屏幕锁定，没有运行应用，也没有运行 `#[gpui::test]`。

- `just check`（格式、架构规则、文档链接、clippy 警告即失败）、`just deny` 通过。
- `just test -p lectern-review`：177 个测试通过，新增 7 个 `overview_packages_*`（名字见「测试」）；`just test -p lectern-services`：99 个通过（`overview_and_brief_requests_round_trip` 含 `packages` 为 `Some` 和 None 的往返）；`just test -p lectern-bench`：78 个通过。
- 基准（`just bench review/`，同一次运行）：`review/overview_round_warm` 中位数 12.1 ms（预算 < 20 ms），`review/resident_cloudprint_bytes` 中位数 19.8 KB，`review/resident_100k_bytes` 中位数 3.47 KB（预算各 < 1 MB），`review/overview_cloudprint` 36.9 ms（只记录）。这次运行时机器上有其他编译任务，同一次运行里 `review/refresh_one` 也从 2.3 ms 涨到 4.45 ms，所以 12.1 ms 里有机器负载。为分开负载与本改动，在同一个 target 目录里交替运行 main 和本分支（`cargo bench -p lectern-bench -- review/overview_round_warm`，各 3 次）：本分支 9.27（`noisy`）、6.63、6.67 ms，main 7.54、6.17、6.60 ms；`review/refresh_one` 本分支 3.52、2.53、2.72 ms，main 2.85、2.70、2.40 ms。差别在噪声以内：本改动每轮只多一次对 ≤ 4000 个路径的最长前缀查找和一次 `utc_offset` 调用（夹具 50 个文件、10 个子项目），stat 与原来的命令声明共用。
- 没有验证的：
  - 真实时区：测试用固定偏移（UTC 与 UTC+8），没有夏令时切换日的用例；切换日的规则与 usage 的 `window::local_date` 相同（偏移取自每个时刻自己），由 platform 的 `utc_offset` 测试覆盖偏移本身。`plugin()` 里的 `platform::time::utc_offset` 只有编译检查。
  - 4000 个路径、2000 个子项目时的轮耗时：bench 夹具只有 50 个文件、10 个子项目，没有对应的基准；stat 的耗时本来就在 4000 个路径的计数比较里（`git/changes_count/*`），额外的只有查表。
  - 真实的 `ProjectEvent::Changed`（`parts.packages`）：测试直接调用 `Driven::project_changed`，插件里的订阅只有编译检查。
  - `project_home` 的使用（[#30](https://github.com/SoloJiang/lectern-app/issues/30)）。

## 与设计的差异

### 前置条件 A

- `ListHandle` 的比较（「接口」已补上）：`ChangeList` 派生 `PartialEq`、`Eq`、`Debug`，`#[serde(skip)]` 要求 `Default`，所以 `ListHandle` 手写这几项。两个句柄比较时总是相等，`Debug` 不打印内容。句柄只是实现方的缓存，不属于改动集的值；这样 `ChangeList` 的比较与 JSON 往返结果一致，git 第 4 步按字段核对转换结果时也不受句柄影响。
- `review` 模块的数据类型同样派生 `Clone`、`PartialEq`、`Eq`、`Debug`、`Serialize`、`Deserialize`（「接口」已补上）。设计只对 `changes` 写明了这一点，但 `crates/services/AGENTS.md`「边界」要求类型能经 JSON-RPC 传给进程插件。
- 设计没有列出的派生：除 `ChangeList`、`ReviewSnapshot` 外的数据类型都派生 `Hash`。`Review::open` 按 `ReviewTarget` 找已打开的审阅，`FileKey`、`RowsKey`、`TextKey`、`StepId` 用作缓存与视图状态的键。不含 `Arc` 的小类型派生 `Copy`。`ChangePath`、`FileKey`、`TurnRef`、`StepId`、`ReviewId` 派生 `Ord`，路径按字节序；`GroupKind`、`NoiseKind` 派生 `Ord`，按声明顺序即显示顺序。`Capabilities`、`ChangeTotals`、`Progress`、`Note` 派生 `Default`。
- `ChangePath` 用派生的序列化，JSON 里是字节数组。标注日志里「UTF-8 时写成字符串」的规则属于 `review` 自己的日志格式，不在这里实现。
- `BriefReply::send` 只调用回调，不检查 `cancelled`：丢弃过期答复由 `review` 传入的回调负责。`cancelled()` 以 `Acquire` 读取。`Debug` 输出 `cancelled` 的当前值。

### 第 1 步

- **`NotRetained` 与 `Blob` 是同一个内容。** `SeenKey` 把 `SideIdentity::NotRetained(oid)` 记作 `Blob(oid)`，`Sensitive(Symlink)`、`Sensitive(Submodule)` 比较两侧时也一样：工作区内容是否保留取决于改动集的保留预算，不取决于内容，否则一次刷新就会让已看的文件变成「审过之后又被改过」。
- **`Sensitive` 的路径匹配范围。** 「简报规则」表写的是「路径是」「在……下」。`CLAUDE.md`、`CLAUDE.local.md`、`AGENTS.md`、`GEMINI.md` 和 `.gitattributes` 按文件名在任何目录里匹配（agent 也加载子目录里的指令文件，子目录的 `.gitattributes` 同样生效）；`.claude/`、`.codex/`、`.cursor/`、`.gemini/` 是任何一级目录；其余（`.mcp.json`、`.github/copilot-instructions.md`、`.github/workflows/`、`.circleci/`、`.gitlab-ci.yml`、`Jenkinsfile`、`azure-pipelines.yml`、`.husky/`、`.githooks/`、`.pre-commit-config.yaml`、`lefthook.yml`、`.gitmodules`）从 worktree 根匹配。路径规则同时看重命名的来源，删除的文件也触发路径规则。
- **标注日志的细节**（已写进「标注日志」）：`key` 的编码；空树的基准 `commit` 为 `null`；同一会话有多条 `base` 时取最早的一条；不知道文件末尾状态时补换行而不截断；文件头坏了或 `v` 不是 1 时只追加、不压缩。
- **重命名撤掉旧路径下的记录**（已写进「标注日志」的内存索引）。只按文件标识保留最近一条时，审过修改 `a`、`a` 又被改名为 `b` 之后，旧路径 `a` 下的 `seen` 一直留着：对 `a → b` 标 `unseen` 后仍显示「审过之后又被改过」，`a` 上之后出现的无关新文件也是「审过之后又被改过」，与「已看状态」的规则不符。已知限制：同一份改动集里同时有 `a → b` 的重命名和 `a` 上的新文件时，标记重命名会撤掉那个新文件在 `a` 下的标记，它回到「未看」。
- **压缩的两倍条件**（已写进「标注日志」，[ADR 0045](../adr/0045.md) 修订 [ADR 0044](../adr/0044.md) 的这一条关键约束）。原来只看「超过 4 MiB」：有效记录本身超过 4 MiB 时（一年的标记，`review/log_load_16k` 的夹具就是这样），每批写入都会重写整个文件、`fsync` 并改名。「性能」的磁盘一行改为实际的上限。
- **目录 `fsync` 失败后只追加。** 压缩改名之后目录的 `fsync` 失败时，改名不一定持久，本进程把文件当作状态未知，直到下一次完整读取；下一次追加时重试目录的 `fsync`。
- **`testing` 的日志夹具。** `testing::write_log`、`testing::load_log` 给 `review/log_load_16k` 用；「本 crate 的公开项」里的 `FakeSessions`、`FakeProjects`、`Driven` 随第 3 步加入。
- **`review/log_load_16k` 的夹具大小。** 一条已看记录约 350 字节（ADR 0044 估计 250 字节），16 000 条是 5.3 MiB，不是 4 MiB；基准按 16 000 条测量，比原来的估计更重，「性能」表已改。

### 第 2 步

- **链与审阅对象无关。** 缓存的值里步骤按它在这个路径的轮次历史里的下标出现，不含轮次和会话；hunk 的归因、文件的归因和叙事步的 `Alignment` 由调用方给出每一步属于本审阅对象的哪一轮、还是哪个其他会话，每次现算（`chain::attribute`）。所以同一路径、同一历史的一条缓存在每个会话的审阅里都能用，键里也不需要审阅对象。
- **工作区一侧不是文本的文件级结果不缓存。** 键里工作区一侧的 `Blob` 与 `NotRetained` 相同（「归因的缓存」），而 `NotRetained` 时链只能做文件级；这个结果写进缓存，之后内容保留下来时会一直命中文件级。所以因为两侧不是 `SideText::Text` 而只做文件级的结果不写缓存；代价是这类文件每次都要 `load` 改动步来比较 `Write` 的 OID，而会话改二进制文件、或工作区内容超出保留预算的情况都少。超过 200 步、超过 4 MiB 的结果与内容的保留无关，照常缓存。
- **一次调用是一个整体。** 一次调用对这个路径的几处改动（`MultiEdit`、多文件补丁）按顺序应用，其中一处应用不上，整次调用都不改变版本，与工具本身的行为相同。调用的改动里没有链当前所在路径的（`load` 给出的路径与声明对不上、只改了 `move_to` 之前的旧路径），同样记为应用不上。
- **链也跟着改动集里的重命名走，不只靠 `move_to`。** 「改动链」第 2 步只写了 `move_to` 让后续步骤跟到新路径。但 Claude Code 用命令（`mv`、`git mv`）改名之后再改新路径，Codex 在一个补丁里 `Delete File` 旧路径、`Add File` 新路径，或者先 `Write` 新路径再删掉旧路径，都没有 `move_to`；只看 `move_to` 时新路径上的每一步都应用不上，会话的行全部成了 Drifted。所以链的输入另带工作区一侧的路径（重命名的目标）：链还在来源路径上时，遇到改目标路径的改动就转到目标路径（在一次调用里逐个改动判断，所以同一个补丁里删旧、加新也能接上），内容随之带过去，`Write` / `Add` 照常重新对齐。调用整体应用不上时也不转。文件级的匹配按同样的规则跟路径。
- **状态不是 `Completed` 的调用**留在历史里（进轮次历史的摘要），不应用，也不算 `Alignment::Partial` 的失败调用：它属于简报的 `EditFailed`，不是 Drifted。
- **应用规则的细节**（「改动链」第 2 步没有写到的）：`Replace` 的 `old` 为空时只在文件不存在或为空时创建它；`Delete` 一个不存在的文件应用不上；`Patch` 作用在不存在的文件上时按空内容处理；没有行号的 hunk 按 `apply_patch` 的做法定位：`@@ <上下文>` 的上下文（`load(input)` 给出的补丁正文里保留着）是从上一个 hunk 之后找到的第一个与它相同的整行，连续几个 `@@ <上下文>`（`@@ class A` 之后 `@@ fn f`）依次往后找，hunk 取最后一个上下文行之后的第一个匹配；没有上下文的 `@@` 取上一个 hunk 之后的第一个匹配。上下文行按 `apply_patch` 的 `seek_sequence` 比较：先逐字节相同，再去掉行尾空白，再去掉两端空白，每一轮查遍所有行再进入下一轮（agent 写 `@@ def save(self):` 时不带方法在类里的缩进）；这只用于上下文行，hunk 的旧一侧照样逐字节比较。没有行号的纯插入不论有没有上下文都放在文件末尾（`apply_patch` 的 `compute_replacements` 这样放），上下文行只需要存在；找不到上下文行时整步应用不上。git 格式 `@@ -a,b +c,d @@` 之后的函数名不当作上下文；hunk 里的空行是一行空的上下文；`\ No newline at end of file` 去掉前一行的换行。`Write` 的 CRLF 在工作区一侧没有 `\r` 时换成 LF（工作区一侧有 `\r` 时说明这个文件就是按 CRLF 保存的，原样比较）。
- **存活的口径。** 一个叙事步「加入的行」是这一步最后一次调用之后、行来源属于它的行；工作区里还剩的同样按行来源数。没有加入任何行的步（只删除）为 `All`。
- **叙事步的 hunk 夹着别的会话的改动。** 叙事步的 hunk 是这一轮第一次改动之前与最后一次改动之后两个版本的 `line_diff`（「改动链」第 3 步）。同一轮的两次调用之间有别的会话改了同一文件时，那次改动也在这个 diff 里；这种交错很少见，按设计照算，记为已知限制。
- **两份 diff 对一行的看法不同时不归因。** 最终 hunk 用 `ChangeList` 的 diff（基准到工作区），行来源来自链上逐步的 diff。两者对同一行的判断不同时（链认为基准里的这一行留着，最终 diff 把它算作删除或新增），这一行既不归到任何一步，也不计入 `drifted`。
- **hunk 与文件的轮次按步骤的时间顺序。** `HunkAttribution::turns`、`FileAttribution::turns` 按第一次出现的步骤排列；子 agent 的轮次插在父会话哪一轮之后属于叙事，第 3 步组装快照时按叙事顺序重排。`FileAttribution::commands` 由第 3 步的命令声明填写，本步总是空的。
- **轮次历史的摘要里重命名链上的路径**由调用方给出：选出这些改动步所依据的路径，基准一侧的路径在前、工作区一侧的路径（链跟随重命名的目标）在后；目标路径决定同样的步骤怎样应用，所以要进摘要。摘要只用轮次索引里的数据（`HistoryStep`），状态取 `ToolCall::finished` 里的状态；没有结果的调用不参与（「改动步与声明」），所以总有状态。
- **`review.attribution` 的值**（`chain::codec`）：每一步的结果与版本 OID、每个叙事步的成员、增删行数、存活与 hunk，以及行来源（工作区每行来源、基准每行去向的游程编码）或文件级的原因与匹配到的步。解码时检查每个下标和游程长度（合计不超过 4 MiB 能有的行数），末尾有多余字节也算未命中。
- **测试的替身。** 「测试」里「缓存命中时不调用 `load`（`FakeSessions` 计数）」在本步用计数的闭包代替 `FakeSessions`（第 3 步加入）：`history_digest_cache_hit_does_not_load` 在临时目录的 `cache` 上，命中时不调用计算链的闭包（`load` 只在这里发生）。「只有命令声明的文件为文件级」「命令窗口落在两个会话里时不归」属于声明，随第 3 步的 `claims_*` 一起测。
- **`testing::chain_fixture`**：`review/chain_3000_lines` 的夹具，「本 crate 的公开项」之外的又一个 bench 夹具，与第 1 步的 `write_log` 相同。

### 第 3 步

- **基准记录之后的非提交记录参与截止**（「改动步与声明」的关键约束已改）。原文是「基准记录之后的 reflog 记录不参与」，但 `claims_after_branch_switch` 要求：会话 B 在 `main` 上改 f 并提交、用户 `git checkout feature` 之后，B 的改动步不进 f 的链，f 不在 B 的审阅里。B 的基准是会话开始时的 `main`，切换分支在基准记录之后，按原文不参与，B 的改动步会留在链里，f 进 B 的审阅并整片 Drifted。所以改为：基准记录之后的记录，新值不以基准提交为祖先时参与（离开了会话的历史，如切换到无关的分支），否则不参与；祖先关系经 `Repo::is_ancestor` 判断，按（基准提交，新值）缓存在新的命名空间 `review.ancestry`（[cache 设计「命名空间」](cache.md#命名空间)已加），判断出错时按参与处理（保守）。没有基准记录时只有这一类；基准是空树时没有。第一版按动作区分（提交不参与，其余都参与），自审时发现 `pull --rebase`、`rebase origin/main`、`reset --soft` 之后的合并提交、把 main 合并进来、快进的 `pull` 都会改变 agent 改过的路径，按动作截止会切掉 agent 自己的改动步，于是改为按祖先关系，并加了这三种情况的测试。agent 提交之后的步骤仍在链里（`base_from_the_reflog_record_before_the_session`），256 条的上限把两类合起来算。祖先的判断不受 5 ms 的 `Interactive` 预算限制：基准记录之后的记录只有会话期间的那几条，结果又有缓存。
- **补齐之后重新准备一轮。** 「数据流与线程」写的是后台读到其他会话的声明或算完其余 `tree_changes` 之后「重算受影响的文件」。实现是用同一份 `ChangeList` 重新准备一轮（`Kind::Reuse`，不重新比较工作区）：声明、截止时刻、文件集合和叙事整体重算，各文件的链经 `review.attribution` 命中，只有历史变了的文件重新建链。逐个找出受影响的文件要比较两轮的历史，省下的只是几次缓存读取。
- **待写队列不等 100 ms。** executor 没有定时器，`Background` 任务里睡眠会占住线程。改为每次标记后若没有在途的写入就提交一个 `Background` 任务，它把当时队列里的记录全部追加，期间来的记录由同一个任务的下一轮追加；写失败时记录放回队列，下一次命令再试。连续标记仍然合并成少数几次追加。压缩用写入时内存索引的副本，副本里可能已有还没追加的记录，之后再追加一次，重放结果相同。`log::flush` 只留给日志自己的测试（`#[cfg(test)]`），引擎分步写，不在持有索引的锁时做 IO。
- **叙事里没有链步骤的改动调用是 `Tool`。** 改动的文件不在这个审阅里（不在改动集里、已提交，或改动早于路径的截止时刻）时，这次调用没有 `StepEntry` 可挂（`StepEntry` 要文件标识、增删行数和对齐），显示为一行 `Tool`。子 agent 在另一个 worktree 里时同样是 `Tool`：`Subagent::session` 是 `sessions` 的下标，而那个子 agent 不在这个审阅的 `sessions` 里（「未决问题」1 的 (a)）。
- **归因还在计算的文件，叙事步先给占位值**：增删 0 行、`Aligned`、`All`，文件的归因算出后随下一份快照更新。视图按 `FileEntry::attribution.state == Pending` 判断。
- **文件夹项目的会话**只给出轮次，改动调用也是 `Tool`（没有改动集，也就没有 `FileKey`），`Degraded::FolderProject`，没有文件和进度。
- **比较范围没有 merge-base 时**改为相对 HEAD 比较（`ChangeError::NoMergeBase`），`BaseInfo` 仍是 `Spec`；快照里还没有字段注明这一点，视图要提示时在 `services` 加字段（后续）。
- **命令声明归给那个会话里窗口命中的全部调用**，不只一次：同一会话连续两条命令都包住这个修改时间时无法分辨是哪一条。
- **`Prepare` 的上限**：展示行合计 8 MiB、正文 2 MiB，按请求里的顺序保留，超出的不准备（视图把可见的排在前面）。新请求替换旧请求，不在新请求里的立即丢弃，不另留「保留范围」。
- **新一轮换掉展示行。** `RowsKey::Final` 只有路径，`StepId` 跨轮不变，所以一轮的结果（刷新、补齐后重新准备的那一轮）装上时丢掉全部展示行，并按最近一次的请求重新准备，随后发 `Prepared`；在旧一轮上开始的准备任务，完成时若当前一轮已不是它的，结果丢弃。正文按会话文件里的固定位置读取，不随轮次变化，保留。`rows_prepared_again_after_a_refresh`、`rows_of_a_provisional_round_are_replaced` 覆盖这两种情况。
- **展示行里上下文行的 hunk**：第一个 hunk 之前、以及每个折叠之后的上下文行属于它后面的 hunk，hunk 之后到折叠之前的属于前一个；两个 hunk 共用的上下文属于前一个。
- **`review.attribution` 的键里，删除的文件没有工作区一侧**，这一段为空（与没有基准一侧时相同），删除也能缓存（`codec::key` 的工作区一侧改为 `Option`）。
- **本次运行的声明留在内存里**：审阅对象自己的和后台为其他会话派生的声明按会话键保存在引擎里（`last_activity` 相同才用），`tail_pending` 的会话派生的声明只在内存里用、不写 `review.claims`。截止时刻表按 worktree 保存，记录或改动集的路径变了就从 `review.tree_changes` 重建，不按 `RepoBatch::reflog` 增量更新（随第 4 步的监听）。关闭审阅时释放没有打开的审阅用到的 worktree 的表和 git 句柄、会话的声明，没有打开的审阅时清空版本 LRU 和内存里的祖先判断。
- **`ReviewError` 的映射**：会话不在列表里、`Place::Missing`、worktree 不在项目快照里或已 prunable、`turns` 返回 `NotFound` 为 `Missing`；未归位或项目没打开为 `NotInOpenProject`；审阅对象自己的 `turns` 返回 `Unrecognized` 或 `Io` 时都按认不出格式处理（`Degraded::UnrecognizedSession`）。
- **测试支持多于「本 crate 的公开项」**：`testing` 另有 `write_reflog`（写 `logs/HEAD`）、`Driven::with_budget`（把 `Interactive` 上计算 `tree_changes` 的 5 ms 预算设为别的值，测试用 0）、`Driven::touch_cache`、`Driven::attribute_uncached`，以及 bench 的夹具 `SessionFixture`、`MarkSeenFixture`。`FakeSessions` 的脚本在测试里用 `serde_json::json!` 拼出（格式写在 `src/testing/fake.rs`），没有放进 `tests/fixtures/sessions/`：会话的 cwd 是 `TestRepo` 的临时目录，每次不同。
- **第 3 步没有日志调用**：那时 `lectern-logging` crate 还没有合并，「日志与诊断」只写了接入的计划。日志已经接入，实现与那份计划的差别见下一节「日志接入」。

### 第 4 步

- **验收的顺序改了**（上文「验收」已改）。原文写前置条件 D（`app` 构造 `ReviewDeps`）在「第 4 步之前」，但 `ReviewDeps` 和 `plugin()` 由第 4 步定义，第 4 步的测试用 `TestKernel`，不需要 `app`。实际顺序是第 4 步、`app` 装配（[#205](https://github.com/SoloJiang/lectern-app/issues/205)）、第 5 步；第 4 步的验收也不再依赖 D。
- **引擎经 `Host` 提交任务，服务经函数取。** `Engine::submit` 原来直接调 `Executor::run`。插件里要持有 `BusyGuard`、经 `PluginHandle::own_with` 提交，而 `PluginHandle` 到 `started` 才有：`engine::Host`（`spawn`、`later`、`busy`）由插件的 `KernelHost` 实现，`started` 之前要求的任务排队、`started` 时提交；`Driven` 没有 `Host`，仍直接用 executor。`Config.projects` 和 `Config.sources` 由持有的服务（`Held`，插件里是 `Admitted`）的函数代替 `Arc`：`SessionSource` 是多提供者的服务，来源会随插件的激活与停用变化，`Admitted` 在每次调用期间持有提供者的准入。
- **30 秒的计时用 kernel 的 `Clock`，不用 executor。** executor 没有定时器，`Background` 任务里睡眠会占住线程。`KernelHost::later` 把一个 `Waker` 交给 `Clock::wake_at`（生产是 `SystemClock` 的定时线程，第一次用时才启动、空闲 1 秒后退出，不是常驻线程；测试是 `ManualClock`），到时唤醒只提交一个 `Background` 的释放任务。等待的 30 秒不持有 `BusyGuard`，kernel 在这期间可以静止。`ReviewPlugin::with_clock` 只在 `test-support` 和测试里存在。定时线程停了之后（空闲超过 1 秒）的第一次 `close` 在调用线程上付一次线程创建；这是 kernel 自己的定时器，审阅在首帧之后才会打开，不影响 ADR 0017 的首帧预算。
- **保留期间的行为**（设计只写「保留 30 秒」）：最后一次 `close` 之后 `snapshot`、`rows`、`text` 对这个 ID 返回 None、`command` 被忽略（与 `Review` 的文档「已关闭的 ID 返回 None」一致），但状态和监听都留着，监听的批次照常刷新它；30 秒内同一对象 `open` 得到同一个 ID，快照原样回到调用方，不重算、不发 `Changed`；每次关闭和重新打开让 `Open::idle` 加一，定时器到期时 `idle` 变了就什么也不做，所以第一次关闭的定时器不会释放第二次关闭之后的审阅。释放时 `Open`（含两个监听）在锁外 drop。
- **监听在第一份快照之后建立。** 一轮准备成功并装上快照之后，`ensure_watch` 为这个审阅订阅仓库和被审阅的 worktree（`Worktree::watch_source()`，`Coalesce::REPO` 与 `Coalesce::WORKTREE`）；arm 由 fs_watch 的线程做，调用不阻塞。fs_watch 的第一个批次是 `rescan`，所以每次 `open` 之后有一次 `Full` 刷新，用来补上准备与 arm 之间的变化（`project` 同样依赖这一点）。仓库订阅的第一个批次被忽略，免得两个订阅各造一次 `Full`。准备失败的审阅没有监听。
- **批次到请求。** worktree 批次：`rescan`、`meta` 的任一标志（`head`、`index`、`refs`、`config`、`rules`）或 `Health::Failed` 为 `Full`，否则有路径时为 `Paths`（`WatchPath` 的字节原样成 `ChangePath`）；只带健康状态的批次不请求。仓库批次：有 reflog 行、`reflog_resets`、`rescan` 或 `Failed` 时为 `Full`（提交与分支切换改变截止时刻与会话外的基准；设计只写了 worktree 批次）。`meta.head` 为 `Full`，会话外的改动在这一轮重新确定基准，会话的基准不变（第 3 步的 `Recorded`、`Reflog` 规则）。
- **合并规则补全。** 第 3 步的 `start_round` 在运行期间只保留最后一个请求（两个 `Paths` 丢掉前一个）。改为：两个路径集取并集（排序去重）、路径集优先于 `Reuse`、`Full` 优先于路径、`Fetch` 优先于一切（`engine::watch::merge_hints`）。
- **`SessionsChanged` 对所有审阅请求 `Kind::Reuse`。** 事件的 `changed` 和 `removed` 都为空时忽略；否则不看哪个来源、哪个会话，对每个审阅（含保留期间的）请求一轮 `Reuse`：同一 worktree 里任何会话的声明都可能改变归因，而每一轮都会重新读审阅对象的 `turns`。改动集不重新比较（文件的变化由 fs_watch 的批次带来）。
- **sink 里的 `BusyGuard`。** fs_watch 交给 `deliver` 的守卫在请求提交之后立即 drop：请求若启动了一轮，这个任务在 `Engine::submit` 里自己取了守卫；若并入了运行中的一轮，那一轮在结束前提交下一轮，守卫链不断。`Sink::busy` 只返回 `PluginHandle::busy()`，不取引擎的锁；它读 `KernelHost` 里存句柄的互斥锁，这把锁只在 `started` 前后和停用时写，临界区里没有 IO，也没有别的锁。
- **第 3 步遗漏的一个 bug：** 文件夹项目的会话在 `folder_round` 结束时丢掉了 `again`；现在照常开始下一轮。
- **概览与简报规则（第 5 步）。** `overview` 返回 None，`watch_overview` 返回一个什么也不做的 `OverviewInterest`，`register_brief_rules` 返回一个什么也不做的登记，提供者不保存也不调用。
- **日志多了两条 debug 记录**（上文「日志与诊断」已加）：`refresh requested`（在审阅的作用域里写出，sink 在 fs_watch 的线程上，本来没有作用域）和 `review released`。没有新的 info 以上的记录，也没有新的字段携带内容或路径。
- **测试用的设施。** `ReviewPlugin::with_wall_clock`（引擎的时刻，14 天窗口按它算；测试的会话是固定日期的）、`with_repository_config_only`（同 `project`）；`engine::tests` 的夹具函数（`Fx`、`session`、`turn`…）改为 `pub(crate)`，`plugin_*` 复用；`Driven::refresh`、`Driven::sessions_changed` 与 `SessionFixture::agent_step` 给 `review/refresh_one`。「`data_dir` 的访问计数」没有做成计数：标注日志的目录在第一次写入才创建，所以测试检查它在后台任务运行之前不存在。「`cache` 的 `ui_thread` 检查」用 `Config::ui_thread` 设为测试线程，任务放到另一个线程运行。
- **`plugin/review/activate` 在进程内测量。** 进程级的 `plugin/*/activate` 要 `app` 装配，还没有；与 `plugin/usage/activate` 相同，`TestKernel` 下取 kernel 的四段耗时，bench 的目标规则（`budgets_belong_to_the_documented_targets`、bench 设计「基准目标」）为它加了例外，随第 5 步改为 `Process`（已改，进程内的 `TestKernel` 基准随之删除）。`review/refresh_one` 在 `budgets.rs` 里是生效的，`Time` 目标。
- 去掉了 `lib.rs` 里为「没有插件时没人调用引擎」加的 `allow(dead_code)`；`ReadReport::error` 只有测试读，字段上加了带理由的 `allow`。

### 第 5 步（概览）

- **第 5 步分两个 PR** 的后半：概览。`Review::watch_overview`、`overview` 由 `engine::overview` 实现（`state`：关注与触发；`round`：一轮；`session`：会话对计数改动集；`sink`：fs_watch 的接收器）。
- **没有当前 worktree 时发布空概览。** 文件夹项目、项目快照还没有 `active`、或 worktree 句柄取不到时，概览的 `sessions` 为空、`outside` 为 `Pending`（设计没有写这种情况）。
- **超过 4000 个改动路径时 `sessions` 为空。** 只保留计数（`Capped`），不保留改动集，会话的文件集合无从求交；下一个事件触发全量比较。
- **基准不是当前 HEAD 的会话，文件集合取它自己基准比较里被它声明的文件**（与审阅快照的 `files` 同一定义），不是计数改动集与声明的交集：会话提交过自己改的文件时，这些文件不在相对 HEAD 的计数改动集里。每轮最多 8 个这样的会话，其余 `progress` 为 None。
- **触发与合并。** 当前 worktree 的 fs_watch 批次（路径 → `paths`，`rescan`、`meta` → `full`；订阅后的第一个 `rescan` 也算一轮，补上订阅的那一轮比较之后、arm 之前的变化）、仓库批次的 reflog 行（`full`）、`SessionsChanged`（`reuse`：改动集不变，声明重取）、标注日志读完或写入（`reuse`：进度变了）、`ProjectEvent::Changed` 的 `active`（`resolve`：重新找当前 worktree）；`ProjectEvent::Closed` 释放状态，不等关注放下。合并窗口 2 秒用 kernel 的 `Clock`（`Host::later`），一轮运行期间来的触发在它结束后再等一个窗口；第一次关注立即一轮，不等窗口；没有 `Host`（`Driven`）时立即。
- **第一次关注之后通常两轮。** 标注日志第一次读完会再触发一轮（进度要它）；测试按事件数断言。
- **一轮里的 IO 不持有标注日志的锁。** 基准不是 HEAD 的会话的比较和归因缓存的读取先做，之后短暂持有日志索引的锁读已看状态（`command` 在 UI 线程上取这把锁）。
- **释放在 `Background` 任务里 drop。** 最后一个关注放下时状态从表里取出，交给任务 drop：计数改动集、声明和 fs_watch 的订阅不在调用方（可能是 UI）线程上释放。`overview` 在取出之后立即返回 None。
- **声明和截止时刻留在引擎的表里。** `release` 保留的 worktree 与会话集合并上概览持有的；概览自己只留 `(SessionKey, started_at, Arc<Claims>)`，不留 `SessionSummary`。
- **`drifted` 的摘要**按审阅的顺序排步（这个会话最前，其余按最后活动从新到旧），与审阅的 `who.order` 一致；同一时间戳的步在别的审阅对象下顺序可能不同，那时未命中、为 None（允许）。文件级的链（`FileOnly`）不算 Drifted。
- **每轮把 `Change` 转成 `FileChange`（`lectern_git::file_change_of`，[#212](https://github.com/SoloJiang/lectern-app/pull/212)），不保留 `ChangeList`**：`SeenKey`、噪声和敏感规则要 `services` 的类型；转换只在一轮里，不进常驻。
- **`critical`** 只算不需要逐行来源的规则：`Sensitive` 的关键种类，加上有删除时的一条 `Deleted`；噪声文件不计。
- **`plugin/review/activate` 原来在进程内测量**（bench 第 5 步起是进程级，见 [bench 设计「与设计的差异」](bench.md#与设计的差异)）。
- **`review/resident_100k_bytes` 的形状是 index-only 的**（与 `project/resident_100k_bytes` 相同，不写 10 万个文件）：计数比较列出全部路径为缺失、超过 4000，概览被 cap；测的是 cap 之后的常驻。cloudprint 形状整个写出，20 个会话各改一个文件。基准扣掉 `Driven` 空闲时的字节和同一比较在引擎外再做一次的字节。
- **测试设施。** `Driven::watch_overview`、`overview`、`overview_refresh`（一个 `full` 触发）、`overview_count`、`has_overview_state`、`project`、`main_worktree`；`FakeProjects::set_active`（快照的 `active`，默认主 worktree）；bench 的 `fixtures/review_repo.rs`。

### 第 5 步（简报）

- **第 5 步分两个 PR。** 简报（本节）与概览各一个 PR，都写 `Refs #15`；「验收」第 5 步的条目按两部分分别满足，状态行写明哪部分已实现。
- **提供者一轮只请求一次。** 设计写「同一审阅的简报重算时丢弃上一次的 `BriefReply`」。归因进行中每 8 个文件或 50 ms 就换一次快照、重算一次简报；每次都重新请求会让提供者反复算同一份改动。实现按工作（`Work`，每一轮一份）判断：`Changed` 事件之后，当前工作还没有问过提供者时发出请求、取消上一次；同一轮里之后的快照只重新对账已收到的条目。新登记的提供者对已打开的审阅补问一次（这一次对其他提供者也是新请求）。
- **规则只看快照的 `files`**（「简报规则」已写明）：`EditFailed` 只对快照里的文件给条目，失败的改动调用若没有改到任何文件，没有条目；`CommandOnly` 每个命令调用一条，`file` 是它声明的第一个文件（进度顺序），`count` 是文件数。噪声文件只计入 `Noise` 条目，不触发其他规则。
- **`Drifted` 的行备注**是第一个有无来源行的 hunk 的第一行（新一侧；hunk 在新一侧为空时取旧一侧），行号与 `DiffHunk` 相同从 0 起；步没有对齐（`Partial`、`Unaligned`，或链里没有它的 diff）也算 Drifted。失败的改动调用不进链，所以不因此算 Drifted。
- **从 `started` 里登记的提供者稍后再问。** 提供者的插件在 `started` 里登记时它的 token 还是 `Pending`，`enter()` 不放行（kernel 设计「激活的提交」）。请求遇到 `Pending` 的 token 时不按失效处理：50 ms 后（`PENDING_RETRY`，kernel 的 `Clock`）对这次请求里还没问到的提供者再试，直到放行或这次请求超时；`Dead` 的按完成处理。没有 `Host`（`Driven` 不带手动 `Host`）时没有定时器，按完成处理。
- **新一轮的第一份快照不显示上一轮的提供者条目。** 一轮的结果装上快照时请求还没发出（请求在 `Changed` 事件之后），这份快照里提供者的条目为空、`pending_providers` 是已登记的提供者数；请求发出后的快照按答复显示。
- **登记 drop 时已交回的条目也丢弃**（设计只写丢弃在途的答复）：提供者停用后它的条目不再有人维护，留着会过期。受影响的审阅在后台任务里重新装配快照。
- **日志里用登记序号指代提供者。** `Value::text` 只接受 `'static str`，插件 ID 是运行时的文本，按日志规则不写出；`brief requested`、`brief reply`、`brief provider timed out` 用 `registration`（上文「日志与诊断」已加）。
- **提供者的答复在 `Background` 任务里重新装配快照**，不在 `send` 的调用线程上：`send` 立即返回，与 `BriefReply` 的文档一致。
- **测试设施。** `Driven::with_timers`（手动 `Host`：`later` 排队、`advance` 到时运行；没有它时 `close` 立即释放，与第 4 步相同）、`Driven::register_brief_rules`、`testing::host()`、`testing::BriefFixture`（`review/brief_5000` 的夹具，纯函数）。

### 日志接入

第 1 到 3 步时「日志与诊断」写的是接入时的计划，现在改成了实现的规格。与那份计划相比：

- **多记的 warn 与 info。** 计划只列了改动集、标注日志的读写与压缩、改动步加载和 `tree_changes`。另外加了：审阅对象的会话轮次读不出、HEAD reflog 读不出、`is_ancestor` 失败（三者都让审阅降级而不报错，不记就查不到原因）；标注日志的恢复（坏行数、截掉的尾部字节数）、文件头版本不是 1、压缩改名之后目录 `fsync` 失败、失败之后的重试（logging 设计第 8 项要求的恢复、写失败与重试）；压缩完成和压缩前发现别的写入者（info）；一轮准备失败和比较范围没有 merge-base（info）。
- **会话键只记会话标识。** 计划写「字段另带会话键（来源 ID、会话 ID）」。宏不接受运行时字符串，来源 ID 不写出；会话 ID 写成 `Value::uuid`：UUID 取原字节，否则取 128 位哈希，算法与 `usage` 相同（`diag::session_tag`）。
- **worktree 和 `Prepare` 的键不记。** 计划写 worktree 用相对公共目录的 git 目录、详细级别记 `Prepare` 的键。前者是运行时字符串（worktree 的名字），由 `review` 作用域代替：它的哈希已经覆盖了仓库和 `target`。后者含路径和会话里的位置，改为记请求的序号和条数、字节数。
- **标注日志的路径在默认级别里。** 计划写「路径只在详细模式里记」。日志文件和目录在应用数据目录下，文件名是仓库的哈希 `h`，不是被审阅仓库里的路径；计划的 warn 一项也要求记「日志文件名」，所以默认级别的标注日志记录带完整路径。工作区里的路径仍只在详细级别。
- **作用域由 `Engine::submit` 设置。** executor 的第 5 项（任务继承提交时的作用域）还没有合并，本 crate 的每个任务显式带审阅的 `Ctx`。`review` 在一轮解析出仓库之后才有，所以一轮里定位仓库之前的记录只带 `session`。标注日志按仓库共用，读取与追加任务带发起它的审阅的作用域。
- **`LogFile` 自己记读取与压缩，引擎记追加失败与重试。** 读取报告（`ReadReport`）多了 `torn_bytes`、`partial`、`other_version`；连续失败的次数在引擎的 `LogMem::failures` 里，追加成功时清零。

### 标注日志读取的余量

- **新增依赖 `memchr`**（「依赖」表已补上）。读取时按行切分改用 `memchr::memchr` 找换行，原来逐字节比较。`memchr` 已在 Cargo.lock 和本 crate 的依赖树里，安装包不多带新的 crate。
- 读取的格式、恢复规则和索引都没有变：整行是 UTF-8 时按 `str` 解析（`serde_json` 不再逐个字符串检查 UTF-8），否则按字节解析，与原来的路径相同；共用字符串时先比较最近交出的 4 个，再查哈希集合。

### Overview.packages（project_home 的前置条件 A）

- **类型在 `services` 里，不是本 crate 里。** `Overview` 定义在 `lectern-services` 的 `review` 模块，所以字段加在 `crates/services/src/review/overview.rs`，该 crate 的往返测试随之更新；project_home 设计把这一项归在 `review`，实际是 `services` 加一个字段、`review` 填它，同一个 PR。
- **本 crate 直接依赖 `platform`**（「依赖」表已改，原文写「不依赖 `platform`」）。日界要本地时区的偏移，`usage` 的做法是自己定义 `LocalZone`、由 `app` 用 `platform::time::utc_offset` 实现后传入；`review` 不能依赖 `usage`（插件之间不得依赖），为这一个函数给 `ReviewDeps` 加字段要同时改 `app`、`bench` 和测试的构造处。`project`、`code_intel` 已直接依赖 `platform`，所以 `plugin()` 直接用 `platform::time::utc_offset`（偏移取不到时按 UTC），`ReviewDeps` 不变；测试经 `ReviewPlugin::with_utc_offset`（`cfg(test)` 或 `test-support`）和 `Driven::set_utc_offset` 换成固定偏移，不依赖机器的时区。
- **日界用 usage 的 `local_date` 规则，没有搬 `start_of`。** 路径的 mtime 与这一轮的「现在」各加自己那一刻的偏移，除以一天的毫秒数后相等就算今天；不算「今天零点的 UTC 时刻」，所以夏令时的重复或缺失的零点不需要特殊处理，也与 usage 的日界一致。未来的 mtime（时钟快的文件系统）属于另一个日子，不计。
- **只列计数不为 0 的子项目，按 `dir` 排序。** 设计只写「按子项目的计数」；2000 个子项目时多数是 0，不列出可以省内存，`project_home` 取不到时按 0 显示（它本来就在 0 时不显示）。`dir` 与 `ProjectSnapshot` 里的是同一个 `Arc<str>`，不新分配。
- **`Unavailable` 也是 None。** 设计只写 `Pending` 时为 None；`Scan::Unavailable` 没有子项目可归，同样给 None（`package_counts` 为 false）。
- **已删除的文件不计，重命名取新路径。** 设计写「工作区一侧 stat 的 mtime」；删除的文件没有工作区一侧，`Worktree::mtimes` 给 None，所以今天删掉的文件不在计数里（已知限制：「今天改动」只数还存在的文件）。重命名的文件取新路径的 mtime 和新路径所在的子项目。
- **一次 stat，两处使用。** 原来只有命令声明对「没有被改动步声明的文件」调 `Worktree::mtimes`；现在每轮对计数改动集的全部路径调一次（≤ 4000 个），命令声明（`session::Input::mtimes`）和今天改动的计数共用。`packages` 不是 `Ready` 时，没有被命令声明用到的文件也会多 stat 一次，上限仍是 4000 次。
- **子项目变化也触发一轮。** `ProjectEvent::Changed` 只带 `packages`（`ChangedParts::packages`，扫描完成）时，原来的 `project_changed` 只看 `active`，所以扫描完成后 `Overview::packages` 会一直是 None，直到下一次文件变化。改为：`active` 触发 `resolve`（同前），只有 `packages` 时触发复用计数改动集的一轮（`Trigger::default()`）；其余部分仍不触发。`Engine::project_changed` 的参数从 `bool` 改为 `ChangedParts`。
- **测试用的设施。** `FakeProjects::set_packages`（运行中改变快照的 `active.packages`，`with_packages` 现在也让它为 `Ready`，原来只影响 `locate`）；`SessionFixture` 的 `FakeProjects` 给十个子项目，使 `review/overview_round_warm` 含这一项；`Driven::set_utc_offset`、`Driven::project_changed`。
- **日志。** 多一条 warn `modification times unreadable`（原来 `unwrap_or_default` 静默吞掉了 `Worktree::mtimes` 的错误），`overview round` 多一个字段 `packages`。
