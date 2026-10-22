# usage 设计

状态：实现中。前置条件 A（`services` 的 `usage` 模块，[#175](https://github.com/SoloJiang/lectern-app/pull/175)）、B（`platform` 的 `time::utc_offset`，[#176](https://github.com/SoloJiang/lectern-app/pull/176)，见 [platform 设计「本地时区的偏移」](platform.md#本地时区的偏移)）、D（`git` 的 `Repo::ref_reflog`、`Repo::patch_id`，[#178](https://github.com/SoloJiang/lectern-app/pull/178)，见 [git 设计「usage 用的读取」](git.md#usage-用的读取)）已实现。第 1 步已实现：前一部分（数据库、读取与游标、规范化与去重、按 cwd 的位置和项目合计、时间窗口、取整、`Project` / `AllProjects` 查询、插件与读取节奏，[#183](https://github.com/SoloJiang/lectern-app/pull/183)），后一部分（冷热分层与 `History` 查询、窗口快照的增量更新、`testing::Scenario` 与 `heavy_days`、第 1 步的 bench 条目，[#187](https://github.com/SoloJiang/lectern-app/pull/187)）。其余前置条件与第 2–8 步尚未实现。

对应 issue [#31](https://github.com/SoloJiang/lectern-app/issues/31)，[#32](https://github.com/SoloJiang/lectern-app/issues/32) 的 Usage 页与额度读本文定义的快照。`usage` 把 `agents` 交来的用量记录去重、规范化后落进自己的 SQLite，把每一轮的 token 归属到项目、分支的「一次使用」和 MR，按时间窗口聚合，并保存额度快照（[ADR 0014](../adr/0014.md)、[ADR 0020](../adr/0020.md)、[ADR 0022](../adr/0022.md)、[交互规范「用量」](interaction.md#用量)）。改动权重的来源按 [ADR 0047](../adr/0047.md)，数据库按 [ADR 0027](../adr/0027.md)，位置按 [ADR 0044](../adr/0044.md) 的应用数据目录。

## 范围

- 做：
  - 读取 `agents` 交来的记录（用量、轮次、工具调用的开始与结果、额度），按 ADR 0014 规范化和去重，与游标在同一个事务里落盘。
  - 观察入库：reflog 记录与它们的监听时刻、每次工具调用之后的分支与 HEAD、轮次结束时的改动项、仓库的标识事实，以及只能在当时读到的工作区状态（stash 是否恢复、切换分支时的未提交路径、工作区是否干净）。
  - 项目标识：Lectern 分配的固定 ID、位置、重新打开时的比对、临时项目、移动后的别名、手动合并与撤销。
  - 归属：轮次到项目和分支片段的分摊、片段到「一次使用」、一次使用里的 MR 区间（P3）、工作转入新分支；Lectern 自己的分析（「审阅」）的计量（[ADR 0019](../adr/0019.md)）。
  - 聚合：本地时区自然日的时间窗口，按项目、行（MR / 分支 / 不属于任何 MR）、模型、agent、日的合计；分支或 MR 的累计、会话在各分支里的份额、每轮 token；最大余数法取整。
  - 额度快照：Codex 日志里的 `RateLimits`，Claude Code statusline 存下的 JSON。
  - `dyn Usage` 服务（快照、关注、命令、计量、MR 来源的登记）和 `usage` 插件。
- 不做：
  - 费用。issue #31 的正文写了按 LiteLLM 的价格表计算 API-equivalent 费用，ADR 0022 已取消费用，以 ADR 0022 为准：不内置价格表、不联网更新、快照里没有金额。
  - 解析会话日志：`agents`（[agents 设计「外部格式」](agents.md#外部格式)）。命令文本里 `cd`、`pushd`、`git -C` 和 git 子命令的解析在本 crate（agents 设计「范围」已写明归 `usage`）。
  - statusline 的安装、串联原命令与恢复：`platform`（#32）。本 crate 只读它存下的 JSON。
  - 改动链与逐行来源：`review`。本 crate 的改动权重按 ADR 0047 从改动工具调用算出。
  - 读写代码平台：`forge`（[#25](https://github.com/SoloJiang/lectern-app/issues/25)），本 crate 只经 `MrSource` 接收 MR 数据。
  - Analyze 的运行与凭据转发：`agents`（[#66](https://github.com/SoloJiang/lectern-app/issues/66)），本 crate 只接收计量。
  - 绘制、Fluent 文案、UI 探针：`usage_view`、`project_home`、`worktrees`、`review_view`。
- 本 issue 之外：
  - Usage 页、额度提醒、statusline 接入：#32，读本文的 `dyn Usage`。
  - MR 的关联（第 7 步）要等 `forge` 的设计和实现（#25）；之前按 ADR 0020「分阶段」显示为分支用量。
  - Analyze 的计量（第 8 步）要等 #66。
  - reftable 仓库：`git` 目前把它们当作普通文件夹打开（[ADR 0024](../adr/0024.md)「已知限制」），在本 crate 里是文件夹项目，没有分支这一级。`git` 和 `fs_watch` 支持 reftable 之后，按 `crates/usage/AGENTS.md` 监视 `reftable/` 取时刻、经 `git` 的 reflog 接口读记录（前置条件 H）。

## 依赖

本 crate 是插件 crate（`plugin = true`，[ADR 0025](../adr/0025.md)）：插件 "usage"，只依赖 kernel 层和非插件的库 crate，只有 `app` 和 tool 层依赖它。其他插件和视图只经 `services::usage` 读它的状态。

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-kernel` | `Plugin`、事件、`PluginHandle`、`BusyGuard`、`ActivationToken`、`Quiescent` / `Busy`；定时器用的 `Clock::wake_at`（「数据流与线程」） | 插件机制；定时不另开线程 |
| `lectern-services` | 提供 `Usage`（前置条件 A）；使用 `Projects`、`SessionSource`；`MrSource` 的登记 | 插件之间只经 `services`（架构规则 7） |
| `lectern-executor` | 后台任务 | UI 线程不做 IO |
| `lectern-fs-watch` | `watch_repo`（reflog 记录与时刻、HEAD 与引用的变化）、`watch_worktree`（stash 恢复和分支切换的窗口）、`watch_dir`（statusline 目录） | fs_watch 设计「对其他 crate 的要求」已写好 `usage` 一方 |
| `lectern-git` | `Repo::discover`、`worktrees`、`Worktree::head`、`branches`、`remotes`、`default_branch`、`root_commits`、`stash`、`tree_paths`、`tree_changes`、`is_ancestor`、`parse_reflog_line`、`line_diff`、计数用的 `Worktree::changes`、`Worktree::read_file`；前置条件 D 的 `ref_reflog`、`patch_id` | gix 只在 `git` 里使用 |
| `rusqlite` =0.40.2（`bundled`、`cache`） | 本 crate 自己的数据库 | ADR 0027 选定，工作区依赖 |
| `serde`、`serde_json` | statusline JSON、观察里的 JSON 列、`services` 类型 | 工作区已有 |
| `ruzstd` 0.9 | 冷数据段的压缩与解压（「冷热分层」） | 已是工作区依赖（`agents`、`cache` 在用，[ADR 0026](../adr/0026.md)），纯 Rust，不改 Cargo.lock |
| `insta`、`tempfile`（dev）；`lectern-git`、`lectern-fs-watch`、`lectern-kernel` 的 `test-support`（dev） | 快照、临时目录、`TestRepo`、`FakeBackend`、`TestKernel` | 工作区统一依赖 |

不依赖 `agents`、`project`、`review`、`forge`（插件之间不得依赖，dev-dependency 也算）：测试用本 crate `testing` 里的假会话来源、假项目服务和假 MR 来源。不依赖 `platform`：本地时区经 `LocalZone` 由 `app` 注入，数据目录由 `app` 传入。不用 `cache`：用量记录不能删除重建（cache 设计「范围」）。除 ADR 0027 已选定的 `rusqlite` 和工作区已有的 `ruzstd` 外不新增外部依赖；项目 ID 用 SQLite 的 `randomblob(16)` 生成。

```rust
pub struct UsageDeps {
    pub executor: Executor,
    pub watcher: lectern_fs_watch::Watcher,
    /// `app` 取 `platform::dirs::app_data()` 下的 `usage/`（ADR 0044）。数据库是其中的 `usage.db`，
    /// statusline 的 JSON 在其中的 `statusline/`（前置条件 C）。不存在时第一次写入才创建。
    pub data_dir: PathBuf,
    /// 本地时区，`app` 用前置条件 B 的 `platform::time::utc_offset` 实现。
    pub zone: Arc<dyn LocalZone>,
    /// `GIT_CEILING_DIRECTORIES`，`app` 启动后从进程环境读一次（与 `project`、`review` 相同）。
    pub git_ceilings: Arc<[PathBuf]>,
}

/// 插件 ID "usage"：provides `dyn Usage`，requires `dyn Projects`、`dyn SessionSource`，激活条件 `AfterFirstFrame`。
/// 不 requires `MrSource`：kernel 没有可选依赖，forge 的小插件反过来 requires `dyn Usage` 并登记（与 review 的 `BriefRules` 相同）。
pub fn plugin(deps: UsageDeps) -> (Name, UsagePlugin);
```

### 对其他 crate 的要求

以下改动各由对应 crate 的 PR 完成，写 `Refs #31`，在「验收」里列为前置条件。

- **A. `services`**：`usage` 模块，按本文「接口」。只有 trait、数据类型和服务 ID；例外是 `UsageInterest`、`MrSourceRegistration` 包着的注销回调，与 `OverviewInterest` 的做法相同。`crates/services/AGENTS.md` 的模块列表加上这个模块。测试：类型的 JSON 往返，`dyn Usage` 的 `ID == "usage"`、`MULTIPLE == false`，`MrSource` 没有实现 `Service`，两个注销包装 drop 时各调用一次回调（`usage_*`）。
- **B. `platform`**：本地时区的偏移，两个平台都用 POSIX 的 `localtime_r`（不是 macOS 专属能力，与 `ThreadCpuClock` 一样放在 `platform`，供 CI 测试）。不缓存时区规则以外的状态。第一次调用（以及 macOS 上系统时区改变之后的第一次调用）由 C 库读一次时区文件，之后只在内存里查找，所以不在 UI 线程上、首帧之前调用。

  ```rust
  pub mod time {
      /// `unix_secs` 这一刻本地时间相对 UTC 的偏移（秒，东正西负），含夏令时。任何线程；第一次调用可能读时区文件，不在 UI 线程上调用。
      /// `localtime_r` 失败（年份放不进 `struct tm`）或偏移超出 TZif 的范围时为 None；毫秒时间戳除以 1000 后总有偏移。
      pub fn utc_offset(unix_secs: i64) -> Option<i32>;
  }
  ```

  测试 `utc_offset_*`：子进程里设 `TZ=UTC`、`TZ=Asia/Shanghai`、`TZ=America/New_York`（夏令时切换前后各一刻）、`TZ=Asia/Kathmandu`（+05:45），结果与表相同。
- **C. `platform`（#32 的 statusline 接入）与 `app`**：statusline 脚本每次收到 JSON，写到 `<app_data>/usage/statusline/claude-code.json`：先写同目录的临时文件，完整后 `rename`，原样保存收到的字节，不超过 1 MiB（超过时不写）。`app` 在启动屏障放开后构造 `UsageDeps`（`zone` 包装 B 的函数），把插件放进 `PluginSet`。
- **D. `git`**：两个函数，不新增依赖。

  ```rust
  impl Repo {
      /// 一个引用的 reflog（`logs/<完整引用名>`，如 `refs/heads/feat`、`refs/stash`），从旧到新，解析规则与 `head_reflog` 相同；
      /// 只读文件末尾的 `max_bytes`（调用方给 4 MiB），超过时 `complete` 为 false。没有这个文件时 `exists` 为 false。阻塞。
      pub fn ref_reflog(&self, name: &RefName, max_bytes: u64) -> Result<RefReflog, GitError>;
      /// 与 `git patch-id --stable` 相同口径的补丁 ID：提交相对第一个父提交的改动，每个文件的 diff 先按路径排序、
      /// 去掉行号与空白差异后求哈希，各文件的结果相加。合并提交、根提交和没有改动的提交为 None。按 git 文档描述的口径自己实现，不参照 git 的代码（架构规则 8）。阻塞。
      pub fn patch_id(&self, commit: Oid) -> Result<Option<PatchId>, GitError>;
  }
  pub struct RefReflog { pub exists: bool, pub entries: Vec<ReflogEntry>, pub complete: bool }
  pub struct PatchId(pub Oid);
  ```

  `ref_reflog` 读末尾：文件超过 `max_bytes` 时缺的是最早的记录，与 git 清掉旧记录的情形相同，`complete` 为 false 的分支按「片段」的「向过去开放」处理；arm 之后的新记录由 `fs_watch` 交付，两段之间没有缺口。`patch_id` 的补充口径（二进制文件、上限）见 [git 设计「usage 用的读取」](git.md#usage-用的读取)：超过上限时返回错误，调用方按这个提交没有补丁 ID 处理，不算一一对应。

  测试 `ref_reflog_*`（分支的创建、改名、`branch -f`、`refs/stash` 的增删，与 git 读出的结果相同；夹具由脚本用 git 生成一次，测试本身不运行 git）、`patch_id_*`（与 `git patch-id --stable` 对夹具给出的值相同；只改空白的两个提交补丁 ID 相同；文件顺序不同的同一组改动补丁 ID 相同）。
- **E.（取消）** 原计划让 `fs_watch` 监听 `logs/refs/remotes/**`。不这样做：每个远端跟踪分支要常驻一份读取进度（实测约 330 B），长期使用的 monorepo 克隆有几千个，超出 [ADR 0035](../adr/0035.md) 给 `fs_watch` 自身状态的 1 MB；每次 fetch 还会把成百上千条与用量无关的记录写进本 crate 的数据库。detached 推送的转移改为推送观察：`Execute` 调用的命令解析出 `git push <远端> HEAD:<分支>`（「命令文本」）时，在这次调用的 `call_obs` 里用前置条件 D 的 `Repo::ref_reflog("refs/remotes/<远端>/<分支>", 64 KiB)` 读一次这个远端跟踪分支的 reflog，记下是否有一条行时刻在 [S − 1 秒, T + 1 秒] 内的 `update by push` 记录，其新 OID 是这次调用期间 HEAD 停留过的 detached 提交之一（S 时的 detached 提交，加上「分支观察」第 1 步回放出的、落在调用期间的 detached HEAD 记录的目标；同一次调用里先提交再推送时，推上去的是新提交）。观察里存下匹配的 OID 和记录的说明文字（「转移」）。`<分支>` 写成 `refs/heads/<b>` 时去掉这个前缀。`<远端>` 是推送地址而不是已配置的远端名、远端的 fetch refspec 不覆盖这个分支（`--single-branch` 的克隆）时没有可用的远端跟踪记录，`<源>` 不是 `HEAD` 时不按推送观察判断，这些情况只能按 `MrSource::mrs_containing` 转移（「转移」表之后的已知限制）。不需要新的前置条件。
- **F. `forge`（#25）**：实现「接口」里的 `MrSource`，由 `forge` crate 里另一个小插件（如 "forge-usage"）登记：它 requires `dyn Usage` 和 forge 自己的服务，在 `started` 里调 `Usage::register_mr_source`，把返回的登记交给 `PluginHandle::own`。数据的口径以 `crates/forge/AGENTS.md`「边界」最后一条为准：仓库按平台主机加不变 ID 标识，MR 带源仓库与源分支、目标仓库与目标分支、历史 base、合并前目标分支的提交、源分支 head、完整的状态时间线。forge 的设计可以细化签名，语义不变。
- **G. `agents`（#66）**：Analyze 的插件 requires `dyn Usage`。分析开始时调 `begin_analysis`；凭据转发端点对每个模型请求调 `meter`：转发之前 `Prepared`（带按上限估算的 token），返回 Ok 之后才转发；请求体写入上游后 `Forwarded`，收到 2xx 后 `Accepted`，收到用量时 `Usage`，非 2xx 且没有用量时 `Rejected`。辅助会话不写入用户的会话目录（ADR 0019），所以计量与日志不重复。
- **H. `git`、`fs_watch`（以后，不阻塞本 issue）**：reftable 仓库的 reflog 读取与监听时刻。在那之前 reftable 仓库是文件夹项目。

`project`、`review`、`cache`、`kernel`、`executor` 不改：路径归位用已有的 `Projects::locate` / `resolve`，定时器用已有的 `Clock::wake_at`（`SystemClock` 的共用定时线程）。

## 概念

| 概念 | 说明 |
|---|---|
| 用量记录 | `agents` 交来的一条 `Record::Usage`，规范化成四类互不重叠的 token，按 `UsageKey` 去重后计数一次 |
| 观察 | 在发生时（或轮次结束时）写入 SQLite、之后不再修改的事实：工具调用、reflog 记录与监听时刻、调用之后的分支与 HEAD、改动项、仓库标识、当时的工作区状态、计量。会话日志重放（`Batch::reset`）时，这个会话的轮次观察换成新的一代 |
| 派生数据 | 从观察算出、可以整体重建的表：去重结果、轮次的份额、聚合用的事实行。算法版本变了就从观察重建 |
| 位置 | 观察里的仓库或目录：git 公共目录的规范路径，或不在任何仓库里的 cwd、改动路径所在目录 |
| 项目 | Lectern 分配的固定 ID（`UsageProjectId`）。git 仓库的全部 worktree 是同一个项目；普通文件夹也是项目，没有分支和 MR 这一级。没有打开过的仓库也有项目记录，状态为「从未打开」，用量计入「其他」 |
| 临时项目 | 身份不能自动确定时先分配的项目，等用户在 Usage 页确认、或取到代码平台仓库 ID 后自动并入或转正 |
| 别名 | 两个项目之间的合并关系：移动、改写历史、手动合并、确认、撤销都追加为别名记录 |
| 跟踪中的轮次 | 这一轮的每次相关工具调用期间，本 crate 都在监听那个仓库的 reflog（`watch_span` 覆盖调用的开始前 1 秒到结果后 2 秒）。其余轮次是导入的轮次 |
| 改动项 | 一轮里一个文件的改动，按「文件 × 分支观察」记录：所在 worktree、片段、改动行数、是否为默认分支（ADR 0047） |
| 片段 | 一个分支（或 detached HEAD）的一段连续存在，有固定 ID，写进观察。改名沿用同一个片段；删除、重建、丢弃独有提交的重置、回到起点开始新片段 |
| 一次使用 | 汇总时把相连的片段合成的一组（「一次使用」一节）。分支用量、MR 区间都在一次使用之内 |
| MR 区间 | 一次使用里属于某个 MR 的时间段（P3，`forge` 提供状态时间线） |
| 份额 | 一轮的 token 分到一个目标（项目 + 片段，或「不属于任何 MR」的一类）的比例，按分数（权重 / 权重和）保存 |
| 事实行 | 一条用量记录分到一个目标的那一份，以百万分之一 token 为单位的整数，按类别保存；聚合只对事实行求和 |

## 接口

### 用量服务（`lectern-services` 的 `usage` 模块）

类型都派生 `Clone`、`PartialEq`、`Debug`、`Serialize`、`Deserialize`（外部标记的枚举），不含浮点数的另派生 `Eq`、`Hash`；不含 `Arc` 的小类型派生 `Copy`；`UsageProjectId`、`AliasId`、`LocalDate`、`UseId`、`AnalysisId`、`ForgeRepo`、`MrRef` 派生 `Ord`；`Tokens`、`Limits` 派生 `Default`（`Limits::default()` 是数据库还没打开时的空快照）。trait 和包着回调的 `UsageInterest`、`MrSourceRegistration` 除外。额度里的 `used_percent` 是 `f32`，所以 `Limits`、`LimitWindow` 没有 `Eq`、`Hash`。`UsageEvent` 实现 kernel 的 `Event`。

```rust
/// 单实现服务，"usage" 插件提供。
/// impl Service for dyn Usage { const ID = "usage"; const MULTIPLE = false; }
pub trait Usage: Send + Sync {
    /// 关注一个查询：返回值持有期间本 crate 计算并保持它的快照，drop 后释放（任何线程，不阻塞）。立即返回。
    fn watch(&self, query: UsageQuery) -> UsageInterest;

    /// 当前快照。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程可调用。没有关注者或还没有算出时为 None。
    fn snapshot(&self, query: &UsageQuery) -> Option<Arc<UsageSnapshot>>;

    /// 额度快照。同上，不需要关注；数据库还没打开时是空的。
    fn limits(&self) -> Arc<Limits>;

    /// 项目合并与临时项目的确认。立即返回，任何线程；落盘与重算在后台，结果随 `UsageEvent::Changed` 送达。
    fn command(&self, command: UsageCommand);

    /// Analyze 开始时登记被分析的对象（ADR 0019）：在这次调用里解析出它当时的归属（含已关闭的 MR）并落盘。
    /// 阻塞（一次事务），只在 executor 上调用。
    fn begin_analysis(&self, start: AnalysisStart) -> Result<AnalysisId, UsageError>;

    /// 一个模型请求的计量变化。阻塞，事务提交之后才返回（数据库是 `synchronous = FULL`）；
    /// 凭据转发端点在 `Prepared` 返回 Ok 之后才转发请求。只在 executor 上调用。
    fn meter(&self, event: MeterEvent) -> Result<(), UsageError>;

    /// 登记 MR 来源（前置条件 F）。立即返回，任何线程。本 crate 每次调用提供者之前 `token.enter()`，
    /// 取不到（提供者已停用）就当作没有登记。返回值 drop 时注销。
    fn register_mr_source(&self, provider: Arc<dyn MrSource>, token: ActivationToken) -> MrSourceRegistration;
}

/// 在 executor 线程上、快照替换之后发出。监听只安排重绘，不做 IO。
pub enum UsageEvent {
    Changed { query: UsageQuery, generation: u64 },
    Limits { generation: u64 },
}

/// `watch` 的关注，drop 时注销。不序列化。
pub struct UsageInterest { /* 实现方传入的注销回调 */ }
impl UsageInterest { pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self; }

pub enum UsageQuery {
    /// 一个项目在时间窗口内的合计与明细：Usage 页「当前项目」；侧栏底部的 Usage 入口用 `Window::D7`。
    Project { project: ProjectRef, window: Window },
    /// Usage 页「所有项目」。
    AllProjects { window: Window },
    /// 一个 worktree 当前分支的累计：并行 worktree 列表与检查器。
    Worktree { project: ProjectKey, worktree: WorktreeKey },
    /// 一个会话（含子 agent）：改到的每个分支（P3 起为 MR）各一行，以及每轮 token：审阅页的简报和轮次标题。
    Session { session: SessionKey },
    /// 一个 MR 的累计与按会话、按轮次的明细，「编写」与「审阅」分开（P3，MR 视图）。
    Mr { mr: MrRef },
    /// 超过热窗口的历史（「冷热分层」）。现有视图都不用，留给以后的界面：`days` 为 false 时只读一直热的按月汇总，
    /// 为 true 时另外逐段解压这个范围里的冷数据段，给出按日的明细。
    History { project: ProjectRef, from: LocalDate, to: LocalDate, days: bool },
}

pub enum ProjectRef { Open(ProjectKey), Id(UsageProjectId) }
/// 最近 7 / 14 / 30 个本地自然日，含今天（[ADR 0020](../adr/0020.md)）。
pub enum Window { D7, D14, D30 }
/// Lectern 给项目分配的固定标识：16 字节随机数，跨运行不变。
pub struct UsageProjectId(pub [u8; 16]);
pub struct AliasId(pub u64);
pub struct LocalDate { pub year: i32, pub month: u8, pub day: u8 }

/// 四类互不重叠的 token（ADR 0022）。`total()` 是四者之和（超过 `u64::MAX` 时取 `u64::MAX`）；缓存命中率由视图从这些类别算出。
pub struct Tokens { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_write: u64 }
impl Tokens { pub fn total(&self) -> u64; }

pub enum UsageSnapshot {
    Project(ProjectUsage),
    All(AllUsage),
    Worktree(WorktreeUsage),
    Session(SessionUsage),
    Mr(MrUsage),
    History(HistoryUsage),
}

/// 每份快照都带。
pub struct SnapshotHeader {
    /// 每次替换加一，与 `UsageEvent::Changed` 一致。
    pub generation: u64,
    pub state: UsageState,
    /// 合计里有 `inheritance_unknown` 的会话（agents 设计「子 agent 与继承前缀」），可能含继承的前缀。
    pub may_include_inherited: bool,
}

pub enum UsageState {
    Loading,
    Ready,
    /// 历史还在补读：已读、合计的会话日志字节数（只算本次运行开始时已知的文件）。
    Backfilling { done: u64, total: u64 },
    Degraded(UsageDegraded),
}
pub enum UsageDegraded {
    /// 数据库损坏：原文件已改名保留，读得出的行已复制进新库，其余从现存的会话日志重读（「数据库」）；
    /// 日志已被删除、行又没救出来的用量不在其中。
    Recovered { at: Timestamp },
    /// 数据库是更新的版本写的：只读，不再入库。
    NewerDatabase,
    /// 数据库写不进（磁盘满、目录不可写）。附原因。
    Unwritable(Arc<str>),
}

pub struct ProjectInfo {
    pub id: UsageProjectId,
    /// 显示名：主 worktree 根的目录名，文件夹为目录名（与 `ProjectSnapshot::name` 相同的规则）。
    pub name: Arc<str>,
    pub folder: bool,
    /// 项目在本次运行中打开着时为 Some。
    pub key: Option<ProjectKey>,
    pub status: ProjectStatus,
}
pub enum ProjectStatus {
    Active,
    /// 记下的路径已不存在或已是别的仓库：Usage 页提供「合并到…」。
    PathMissing,
    /// 临时项目：Usage 页提供「是同一个项目」（并入 `candidate`）与「是新项目」。
    Temporary { reason: TemporaryReason, candidate: Option<UsageProjectId> },
    /// 已并入另一个项目；`alias` 用于撤销。
    Merged { into: UsageProjectId, alias: AliasId },
}
pub enum TemporaryReason {
    /// 根提交变了，两边都没有远端和代码平台仓库 ID。
    LocalRewrite,
    /// 根提交相同、两边都没有仓库 ID，主远端地址变了。
    PrimaryRemoteChanged,
    /// 已知项目有仓库 ID，这次还没有取到。
    ForgeIdPending,
    /// 仓库还没有提交。
    Unborn,
}

pub struct ProjectUsage {
    pub header: SnapshotHeader,
    pub project: ProjectInfo,
    pub window: Window,
    /// 窗口内的本地日期，从早到今天。
    pub days: Arc<[LocalDate]>,
    pub total: Tokens,
    /// 按 MR / 分支 / 「不属于任何 MR」的行，各行相加等于 `total`（每个类别分别按最大余数法取整）。普通文件夹为空。
    pub rows: Arc<[UsageRow]>,
    /// 按模型、按 agent，各自相加等于 `total`。
    pub by_model: Arc<[(Agent, Arc<str>, Tokens)]>,
    pub by_agent: Arc<[(Agent, Tokens)]>,
    /// 与 `days` 一一对应：每天按 agent 的 token 合计（柱状图按 agent 堆叠），每天各项相加等于那天的合计。
    pub daily: Arc<[DayTotals]>,
}

/// 一天按 agent 的 token 合计。
pub type DayTotals = Arc<[(Agent, u64)]>;

pub struct UsageRow {
    pub key: RowKey,
    pub tokens: Tokens,
    /// `tokens` 里 Lectern 自己的分析（「审阅」）的部分；其余是「编写」。
    pub review: Tokens,
}
pub enum RowKey {
    /// 同一次使用上统计区间互相重叠的几个 MR，合为一行（P3）。
    Mrs { mrs: Arc<[MrLabel]>, branch: BranchLabel },
    /// 一次使用里不在任何 MR 区间内的部分；P1、P2 是整次使用，界面标为「分支用量」。
    Branch { use_id: UseId, branch: BranchLabel },
    /// 默认分支、detached HEAD 和无法确定分支的用量；`unattributable` 是其中「无法归属」的部分。
    Unassigned { unattributable: Tokens },
}
/// 一次使用的标识：它最早的片段的 ID。片段合并的结果变化时可能变化，视图只用来区分行。
pub struct UseId(pub [u8; 16]);
pub struct BranchLabel {
    /// 这次使用最后的分支名（改名后的名字），如 "refs/heads/feat"。
    pub name: Arc<str>,
    /// 现在检出它的 worktree。
    pub worktree: Option<WorktreeKey>,
}
pub struct MrLabel { pub mr: MrRef, pub title: Arc<str>, pub state: MrState }
pub enum MrState { Open, Merged, Closed }

pub struct AllUsage {
    pub header: SnapshotHeader,
    pub window: Window,
    pub days: Arc<[LocalDate]>,
    pub total: Tokens,
    /// 每个项目一行（并入别的项目的不单列），加上 `other`，相加等于 `total`。
    pub projects: Arc<[(ProjectInfo, Tokens)]>,
    /// 「其他」：还没有打开过的项目和不在任何项目里的用量。
    pub other: Tokens,
    pub daily: Arc<[DayTotals]>,
}

pub struct WorktreeUsage {
    pub header: SnapshotHeader,
    /// 当前分支（detached 时为 None）。
    pub branch: Option<BranchLabel>,
    /// P3 起：这个分支当前所在的 MR 区间里的 MR。
    pub mrs: Arc<[MrLabel]>,
    /// 当前这次使用（P3 起为 MR 区间）的累计；默认分支和 detached 时为 None。
    pub cumulative: Option<Tokens>,
    pub review: Tokens,
    pub since: Option<Timestamp>,
}

pub struct SessionUsage {
    pub header: SnapshotHeader,
    /// 本会话加全部子 agent 会话的合计（ADR 0014）。
    pub total: Tokens,
    /// 本会话改到的每个分支（P3 起每个 MR 一行；一个分支同时对应几个 MR 时各占一行，`shared` 为 true）。
    pub rows: Arc<[SessionRow]>,
    /// 本会话自己的每轮 token，不分摊（ADR 0020「界面位置」）。不含子 agent 会话。
    pub turns: Arc<[(TurnId, Tokens)]>,
}
pub struct SessionRow {
    pub key: RowKey,
    pub worktree: Option<WorktreeKey>,
    /// 这个分支（或 MR）的累计。
    pub cumulative: Tokens,
    /// 其中本会话（含子 agent）的那一份。
    pub session_part: Tokens,
    pub shared: bool,
}

pub struct MrUsage {
    pub header: SnapshotHeader,
    pub mr: MrLabel,
    /// 统计区间（「MR 区间」）；还开着时 `end` 为 None。
    pub start: Timestamp,
    pub end: Option<Timestamp>,
    pub write: Tokens,
    pub review: Tokens,
    /// 与这个 MR 区间重叠、共用同一次使用的其他 MR（「注明共用」）。
    pub shared_with: Arc<[MrRef]>,
    /// 按会话，每个会话带它分到这个 MR 的每轮 token（一轮被分摊到几个 MR 时是分到这里的那一份）。
    pub sessions: Arc<[MrSessionRow]>,
}
pub struct MrSessionRow { pub session: SessionKey, pub tokens: Tokens, pub turns: Arc<[(TurnId, Tokens)]> }

pub struct HistoryUsage {
    pub header: SnapshotHeader,
    pub project: ProjectInfo,
    /// 每个本地年月一行：热窗口里的月份按事实行求和，更早的取 `cold_month`。
    pub months: Arc<[((i32, u8), Tokens)]>,
    /// `days` 为 true 时的按日明细（冷的部分按记录时间戳和当前时区重新分日）。
    pub days: Option<Arc<[(LocalDate, Tokens)]>>,
}

pub struct Limits {
    pub generation: u64,
    pub windows: Arc<[LimitWindow]>,
    /// `windows` 里 `used_percent` 最高、没有过期的一个（侧栏底部的进度条）。
    pub tightest: Option<u16>,
}
pub struct LimitWindow {
    pub agent: Agent,
    pub source: LimitSource,
    /// Codex 的 `limit_id`；statusline 没有时为 None。
    pub limit_id: Option<Arc<str>>,
    pub kind: LimitKind,
    pub window_minutes: Option<u32>,
    pub used_percent: f32,
    pub resets_at: Option<Timestamp>,
    pub sampled_at: Timestamp,
    /// 已过 `resets_at`，之后没有新的样本（见「未决问题」1）。
    pub expired: bool,
}
pub enum LimitSource { SessionLog, Statusline }
pub enum LimitKind { Primary, Secondary, FiveHour, SevenDay }

pub enum UsageCommand {
    /// 「合并到…」：`from`（路径已不存在的项目）的用量并入 `into`。
    Merge { from: UsageProjectId, into: UsageProjectId },
    /// 临时项目「是同一个项目」。
    ConfirmSame { temporary: UsageProjectId, into: UsageProjectId },
    /// 临时项目「是新项目」。
    ConfirmNew { temporary: UsageProjectId },
    /// 撤销一次合并或确认，也追加为别名记录。
    Undo { alias: AliasId },
}
```

计量（ADR 0019，前置条件 G）：

```rust
pub struct AnalysisStart { pub agent: Agent, pub at: Timestamp, pub target: AnalysisTarget }
pub enum AnalysisTarget {
    /// 会话简报发起：被分析的会话与轮次（None 为全部轮次）。
    Session { session: SessionKey, turns: Option<Arc<[TurnId]>> },
    /// 会话外的改动、比较范围：worktree 当时的分支。
    Worktree { project: ProjectKey, worktree: WorktreeKey },
    /// MR 视图发起（P3）。
    Mr { mr: MrRef },
}
pub struct AnalysisId(pub u64);

pub struct MeterEvent {
    pub analysis: AnalysisId,
    /// 同一次分析里请求的序号，凭据转发端点分配。
    pub request: u32,
    pub at: Timestamp,
    pub model: Arc<str>,
    pub state: MeterState,
}
pub enum MeterState {
    /// 转发之前：按上限估算的 token（ADR 0019 的 300k 上限内由 agents 估算）。
    Prepared { upper: RawTokens },
    /// 请求体已写入上游。
    Forwarded,
    /// 上游返回 2xx。
    Accepted,
    /// 收到用量；流式响应可能多次，后一次替换前一次，`done` 为 true 时是最终值。
    Usage { tokens: RawTokens, done: bool },
    /// 非 2xx 且没有用量。
    Rejected { status: u16 },
}
pub enum UsageError {
    /// 数据库还没打开、只读或已失败。凭据转发端点收到它时不转发，Analyze 报错。
    Unavailable,
    Io(Arc<str>),
    UnknownAnalysis,
}
```

MR 来源（前置条件 F）：

```rust
/// forge 实现，经 `Usage::register_mr_source` 登记，不是 kernel 服务。三个方法都阻塞（可能请求网络），
/// 只在 executor 上调用；离线或被限流时给出缓存，没有缓存时返回 `Offline`，本 crate 按分支显示、之后再试。
pub trait MrSource: Send + Sync {
    /// 远端地址对应的代码平台仓库。
    fn repo(&self, remote_url: &str) -> Result<Option<ForgeRepo>, MrSourceError>;
    /// 以这个仓库的这个分支为源分支的全部 MR，含已合并、已关闭的。
    fn mrs_for_branch(&self, repo: &ForgeRepo, branch: &str) -> Result<Arc<[MrInfo]>, MrSourceError>;
    /// head 包含这个提交、目标分支（合并前的提交）不包含它的 MR。
    fn mrs_containing(&self, repo: &ForgeRepo, commit: &BlobId) -> Result<Arc<[MrInfo]>, MrSourceError>;
}
/// 平台主机加平台给仓库的不变 ID（GitHub repository id、GitLab project id）；ID 只在同一主机上唯一。
pub struct ForgeRepo { pub host: Arc<str>, pub id: Arc<str> }
pub struct MrRef { pub repo: ForgeRepo, pub number: u64 }
pub struct MrInfo {
    pub mr: MrRef,
    pub title: Arc<str>,
    pub source: (ForgeRepo, Arc<str>),
    pub target: (ForgeRepo, Arc<str>),
    pub head: BlobId,
    /// 代码平台记录过的 base 提交（GitHub `base.sha`、GitLab `diff_refs.base_sha`）。
    pub bases: Arc<[BlobId]>,
    /// 已合并的 MR：合并前目标分支的提交。
    pub merged_target: Option<BlobId>,
    /// 打开、关闭、重新打开、合并及其时间，按时间排序。
    pub timeline: Arc<[(Timestamp, MrEventKind)]>,
}
pub enum MrEventKind { Opened, Closed, Reopened, Merged }
pub enum MrSourceError { Offline, NotFound, Other(Arc<str>) }
pub struct MrSourceRegistration { /* 注销回调 */ }
impl MrSourceRegistration { pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self; }
```

调用方：

- `usage_view`：显示 Usage 页时持有 `watch(Project | AllProjects)`，收到 `Changed` 后读 `snapshot`；额度卡片读 `limits`；合并与确认发 `command`。
- `project_home`：侧栏底部持有 `watch(Project { window: D7 })`，读 `limits().tightest`。
- `worktrees`：每个显示的 worktree 一个 `watch(Worktree)`。
- `review_view`：简报和轮次标题持有 `watch(Session)`；当前审阅的改动所在分支排第一由视图按 `SessionRow::worktree` 决定。
- MR 视图（P3）：`watch(Mr)`。
- `agents`（#66）：`begin_analysis`、`meter`。`forge`：`register_mr_source`。

### 本 crate 的公开项

```rust
/// 本地时区。`app` 用 `platform::time::utc_offset` 实现（前置条件 B），测试用 `testing::TableZone`。
pub trait LocalZone: Send + Sync {
    /// `unix_ms` 这一刻相对 UTC 的偏移（秒，东正西负）。第一次调用可能读时区文件，只在后台线程上调用。
    fn offset_at(&self, unix_ms: i64) -> i32;
}

/// 纯函数，任何线程。测试与 bench 直接调用。
pub mod tokens {
    /// ADR 0014 的规范化，见「规范化与去重」。
    pub fn normalize(raw: &RawTokens) -> Tokens;
}
pub mod round {
    /// 最大余数法：`parts` 是以百万分之一 token 为单位的各行，和必须是 `1_000_000` 的整数倍；
    /// 返回各行的整数 token，和等于 `parts` 之和除以 `1_000_000`。余数相同时按下标从小到大。
    pub fn largest_remainder(parts: &[u64]) -> Vec<u64>;
    /// 把 `tokens`（整数）按权重分成百万分之一 token 的整数份，和恰好等于 `tokens * 1_000_000`。
    pub fn split(tokens: u64, weights: &[u64]) -> Vec<u64>;
}
pub mod window {
    /// 窗口里每个本地自然日和它的 UTC 区间（毫秒，左闭右开），从早到今天；夏令时切换的日子是 23 或 25 小时。
    pub fn days(window: Window, now_ms: i64, zone: &dyn LocalZone) -> Vec<(LocalDate, Range<i64>)>;
}
pub mod command {
    /// 命令文本里与归属有关的部分，见「命令文本」。
    pub fn parse(text: &str, workdir: &Path) -> CommandFacts;
}
pub mod identity {
    /// 项目识别的判定，见「项目标识」。
    pub fn identify(known: &[KnownProject], seen: &RepoFacts) -> Identification;
}
pub mod stash {
    /// stash 的改动块是否已恢复到工作区，见「工作转入新分支」。
    pub fn restored(stash: &StashChange, worktree: &dyn Fn(&RepoPath) -> FileState, head: &dyn Fn(&RepoPath) -> FileState) -> bool;
}

/// 测试与 bench 用（`test-support` feature）。
pub mod testing {
    /// 按 JSON 的记录序列（`services::session` 的 serde 形式）实现 `SessionSource`，可追加并发出 `SessionsChanged`。
    /// `with_files` 在给定目录里为每个会话放一个稀疏文件（长度随记录、每次变化换修改时间），用来测「文件没变就不重读」；
    /// `set_adapter` 改适配器版本，游标来自别的版本时 `read` 从头给出、`reset` 为 true。
    pub struct FakeSessions;
    /// 只有快照的 `Projects`：仓库与文件夹由测试声明，`resolve` 按路径前缀判断、不做 IO；
    /// 第 5 步起 worktree 来自 `TestRepo`。
    pub struct FakeProjects;
    /// 按脚本给出 MR 与时间线的 `MrSource`。
    pub struct FakeMrSource;
    /// 按（生效时刻，偏移）表实现的 `LocalZone`，用来测夏令时。
    pub struct TableZone;
    /// 一同推进的单调时钟（kernel 的 `testing::ManualClock`）与墙上时钟，经 `UsagePlugin::with_time`
    /// 交给插件；推进时到期的定时器回调在推进的线程上执行。`jump_wall` 只拨墙上时钟（睡眠、改系统时间）。
    pub struct ManualTime;
    /// 场景：按时间顺序注入会话记录、reflog 记录与监听时刻、工作区状态、MR 时间线，
    /// 不经插件驱动引擎（`Executor::manual()`、`fs_watch` 的 `FakeBackend`），之后读快照和归属表。
    pub struct Scenario;
    /// bench 的夹具：合成「重度一天」的会话、reflog 与改动，写成数据库。
    pub fn heavy_days(dir: &Path, days: u32) -> io::Result<()>;
}
```

## 数据结构

### 外部格式

| 输入 | 来源与版本 | 用到的部分 |
|---|---|---|
| 会话记录 | `services::session`（[agents 设计「记录」](agents.md#记录)）。日志格式由 `agents` 按版本适配，本 crate 只读这些类型 | `Context`、`TurnStarted`、`TurnEnded`、`ToolStarted`、`ToolFinished`、`Usage`、`RateLimits`；`Positioned` 的 `pos`、`seq`、`inherited`；`Batch::reset`、`at_end`；`SessionSummary` 的 `parent`、`cwd`、`place`、`logged_branch`、`open_turn`、`inheritance_unknown`；`TurnIndex` 与 `load` 给出的 `FileEdit` |
| statusline JSON | Claude Code statusline 的输入（ADR 0014 的来源 <https://code.claude.com/docs/en/statusline>），`platform` 原样存下（前置条件 C） | 顶层 `rate_limits` 的 `five_hour`、`seven_day` 两个窗口：已用百分比、重置时刻；文件的修改时间是采样时刻。其余字段忽略 |
| reflog 记录 | `fs_watch::ReflogRecord`、`ReflogReset`（[fs_watch 设计「reflog 记录」](fs_watch.md#reflog-记录)），行由 `git::parse_reflog_line` 解析 | `logs/HEAD`、各 worktree 的 `logs/HEAD`、`logs/refs/heads/**`、`logs/refs/stash`；远端跟踪分支不监听，推送观察按需经 `Repo::ref_reflog` 读（前置条件 E 的说明） |
| MR | `MrSource`（前置条件 F） | 见「接口」 |

statusline JSON 的字段名在第 2 步按公开文档核对：实现之前读 ADR 0014 来源里的 statusline 文档，把每个窗口里百分比和重置时刻的字段名、取值范围、文档日期写进本节；文档里没有 `rate_limits` 时停下，在 #32 里说明，Claude Code 的额度为空。不读本机的 Claude Code 配置和会话。解析只声明用到的字段；文件超过 1 MiB、不是 JSON、没有这两个窗口时不更新额度，计数进日志。

### 规范化与去重

规范化（ADR 0014、ADR 0022，每条记录在入库时做一次，原始数值不保存）：

| `RawTokens` | `input` | `cache_read` | `cache_write` | `output` |
|---|---|---|---|---|
| `Anthropic` | `input`（不含缓存） | `cache_read` | `cache_write`；为 0 而 `cache_write_5m + cache_write_1h` 大于 0 时取后者 | `output` |
| `OpenAi` | `input - min(cached_input, input)` | `cached_input`（不超过 `input`） | `cache_write` | `output`（已含 `reasoning_output`） |

`UsageRecord::advisor` 是同一条记录附带的另一份用量，按它自己的模型单独计一次，去重键是这条记录的键加上 advisor 标记。

去重：

- 每条用量记录按 `UsageKey` 的规范编码（变体标签加各字段的长度前缀字节）入库，每次出现是一个「来源」（会话、这个会话的代、`pos`、`seq`）。键相同的来源只计一次，计数的那一份从可计数的来源（不是 `inherited`、不是被取代的 `CodexTokenCount`，见下两条）里按下面的顺序选出：不是 `sidechain` 的优先；token 合计大的优先（ADR 0014 的「保留合计最大的一条」）；再按（时间戳、来源 ID、会话 ID、`pos`、`seq`）最小的；最后按来源行的 ID。没有可计数的来源时这个键不计数。选出的来源决定这条用量属于哪个会话、哪一轮。来源行与轮次行的 ID 用 `AUTOINCREMENT`，删掉的 ID 不再分配：回放删除一代的来源时，旧来源的事实行按来源 ID 删除，不会删到新来源的事实行。
- `inherited` 的来源不计数：子 agent 和分叉会话重放的前缀（ADR 0014「只统计 `task_started` 之后的用量」由 `agents` 标出）。`inheritance_unknown` 的会话照常计数，快照的 `may_include_inherited` 为 true。
- `CodexTokenCount` 只计 `superseded` 为 false 的，`CodexResponse` 全计（agents 设计「只用一路用量」）。
- 子 agent 会话的用量属于它自己的会话，按它自己的轮次归属；会话的合计（`SessionUsage::total`）加上全部子孙会话（ADR 0014「计入父会话」）。
- Analyze 的计量不经日志（辅助会话不写入用户的会话目录，ADR 0019），与日志里的用量没有共同的键。
- `Batch::reset`：回放的记录写进这个会话的新一代，旧一代照常计数；读到 `at_end` 的那一批，在同一个事务里删除旧一代的来源和分代的轮次观察、把新一代设为当前，受影响的键重新选出计数的那一份（agents 设计「游标与批量读取」）。不分代的观察（`call_fact`、`call_obs`、`turn_obs`、`item_state`）保留，新一代按调用 ID 沿用（「轮次的观察与改动项」）。
- ccusage 的 `--json` 只在测试里对照（`ccusage_*`）。

### 数据库

- 位置：`<data_dir>/usage.db`（加 `-wal`、`-shm`），不在 `cache` 的目录里，不能删除重建（ADR 0027、ADR 0044）。
- 连接：两个，一个写、一个读，都只在 executor 的任务里打开和调用，UI 线程上不做任何 SQLite 调用。`journal_mode = WAL`，`synchronous = FULL`（计量要求 `Prepared` 在转发之前落盘；入库按批提交，代价摊到一批上），`cache_size = -1024`（每个连接 1 MiB，合计 ≤ 2 MiB，ADR 0027），`mmap_size = 0`，不用共享缓存模式，`foreign_keys = ON`；建库时（建表之前）设 `auto_vacuum = INCREMENTAL`，空间回收见「冷热分层」。写连接由一把锁串行使用。
- 版本：`PRAGMA user_version` 是表结构版本，只向前迁移，迁移不删除观察。每次迁移之前先 `VACUUM INTO '<data_dir>/usage.db.pre-v<旧版本>'` 做一份一致的副本（含冷数据段），副本写完、`fsync` 之后才迁移；同一目录只留最近两份。副本写不出来（磁盘满）时不迁移，按旧版本只读打开，快照为 `Degraded(Unwritable)`。`meta` 表另记派生算法的版本：版本变了，派生表在后台按批从观察重建（「数据流与线程」）。数据库的版本比本 crate 认识的新时只读打开，快照为 `Degraded(NewerDatabase)`。
- 损坏的检查：打开或查询时遇到 `SQLITE_CORRUPT` / `SQLITE_NOTADB`，或上一次运行没有正常关闭（`meta` 里的关闭标记）之后第一次空闲时的 `PRAGMA quick_check` 失败。
- 损坏的处理，按顺序，任何一步都不删除原文件，观察、冷数据段、项目与别名、计量都不能重建，所以先尽量救出：
  1. `REINDEX`，再 `quick_check`；通过就照常使用（索引损坏最常见，表数据没有坏）。
  2. 不通过：在同一目录新建 `usage.db.salvage`，原文件留在原处、只读附加（`ATTACH` 的 `mode=ro`），逐表按 rowid 区间把读得出的行复制过去，某个区间读出错时二分缩小、跳过读不出的行并计数。先复制观察、`cold_part`（每段解码核对，坏的段留在旧文件里并计数）、项目与别名、计量，派生表不复制，之后从观察重建。新文件做完检查点、`fsync` 之后，才把原文件硬链接为 `usage.db.corrupt-<Unix 毫秒>`（不能硬链接时复制；还有内容的 `-wal` 一起），删掉原文件的 `-wal` / `-shm`，把新文件 `rename` 成 `usage.db`，再 `fsync` 目录（目录的 `fsync` 尽力而为，被拒绝时记 warn）。任何一步之前中断（崩溃、退出），`usage.db` 都还是原文件；下次打开时发现 `usage.db.salvage` 就删掉它，从头再抢救一次。抢救开始时先把 WAL 检查点进原文件；检查点失败、又恰好在删掉 `-wal` 与 `rename` 之间中断时，WAL 里的行只在保留的副本里，是已知限制。
  3. 有行丢失时，复制过来的行都留下（日志可能已被删除，留下的用量是唯一的记录），但指向丢失行的引用要补全：新行的 ID 是现存最大值加一，会拿到丢失行的 ID，留下的引用就会指向别的字符串、会话或位置。会话和轮次要用的字符串（来源 ID、会话 ID、父会话、`TurnId`）和丢失的会话按原 ID 补回占位行（字符串以字节 0xFF 开头，不会与真实的 ID 相同）；来源的模型丢失时改为空字符串，agent 丢失时记为 `agent:other:lost`，来源的位置丢失时改为未知位置，这些来源标为 `repaired`（会话是占位行、或它的来源 ID、会话 ID 是占位串时不标：没有日志会再给出这些记录，标记会一直留着）；轮次的位置、来源所在的轮次、分代的读取状态丢失时置空；位置丢失的项目删掉。合计和每日数字不变，按 agent、模型、项目的明细在日志重读之前可能落到「其他」或未知。然后全部会话的游标回到 `START`，从现存的日志重读：会话行还在时，同一条记录读到时把 `repaired` 来源的轮次、位置和模型换成重读的结果，agent 随会话行更新；会话行是占位时，重读建出新的会话行，同一个键的两份来源去重时占位的一份在排序上靠后（0xFF 排在任何 UTF-8 文本之后），只计重读的那一份，不重复计数。没有行丢失时会话保留游标。
  4. 快照为 `Degraded(Recovered { at })`，日志记下每张表救出与丢失的行数。日志已被删除、行又没救出来的用量找不回，是已知限制。

表（观察 O、派生 D）。列只写主要的，ID 都是整数主键，字符串和路径另放在驻留表里按 ID 引用：

| 表 | 类 | 主要列 | 写入时机 |
|---|---|---|---|
| `meta` | — | 键值：派生算法版本、关闭标记、补读进度；每个 UTC 月的归档状态（`archive:<月>`）、核对失败时的段格式版本（`archive_failed:<月>`）与冻结时来源的最大 ID（`archive_mark:<月>`，「之后到达的旧记录」）；入库事务的计数（`commits`，快照的增量按它对齐） | 版本变化、打开与正常关闭、补读、归档、每批入库 |
| `intern` | O | 驻留表：字符串与字节串（来源 ID、会话 ID、`TurnId`、模型名、agent 标签）→ ID | 第一次出现 |
| `run` | O | fs_watch 的 `RunId` → 本 crate 分配的递增序号 | 第一次收到某个 `RunId` 的记录（fs_watch 设计「reflog」的先后规则） |
| `location` | O | 种类（git 公共目录 / 目录）、规范路径（字节） | 第一次出现 |
| `project`、`project_fact`、`alias` | O | 项目 ID、种类、状态；每次识别时的标识事实（根提交、主远端、远端集合、仓库 ID、位置）；别名记录（种类、从、到、被撤销的别名） | 识别、命令（「项目标识」） |
| `session` | O | 来源 ID、会话 ID、agent、父会话、派出它的调用、当前代与回放中的代、游标（`Cursor::to_bytes`）、`inheritance_unknown`、是否读到过末尾；最后一批读到末尾时、读之前日志文件的大小、修改时间与 inode（`end_stamp`，别的批次清空） | 每批读取的事务里 |
| `session_gen` | O | 会话、代：读到这一批末尾时的 cwd 位置、模型、所在轮次（下一批从这里接着读） | 每批读取的事务里 |
| `turn` | O | 会话、代、`TurnId`、序号、开始与结束时刻、cwd 的位置与 worktree、日志记录的分支、是否已定稿、跟踪中或导入 | 读到 `TurnStarted`；定稿时更新一次 |
| `call` | O | 会话、代、调用 ID、轮次、开始与结果时刻、种类、状态、工作目录、进入的 worktree、git 子命令的事实（「命令文本」）、改动路径 | 读到 `ToolStarted` / `ToolFinished` |
| `usage_source` | O | 键（`UsageKey` 的规范编码）、会话、代、`pos`、`seq`、份（0 为记录本身、1 为 advisor）、时间戳、轮次、记录所在轮次的 cwd 位置、模型、四类 token、`sidechain`、`superseded`、`inherited`；`repaired`（抢救补全过引用，等同一条记录重读时换上重读的结果） | 每批读取的事务里 |
| `limit_sample` | O | agent、来源、`limit_id`、窗口种类、百分比、窗口分钟数、重置时刻、采样时刻；每个（agent、来源、`limit_id`、窗口种类）只留最新的一条 | `RateLimits` 记录、statusline 文件变化 |
| `watch_span` | O | 仓库、运行序号、arm 之后第一批的读取戳、结束的读取戳（句柄 drop、`Failed`） | `RepoWatch` 的批次 |
| `reflog`、`reflog_reset` | O | 仓库、文件（HEAD 与 worktree / 分支 / stash）、偏移、行、`seq`、`observed` 与 `after`（运行序号、读取编号、纳秒时刻）、`origin`；重置的原因 | `RepoBatch` 到达时 |
| `head_baseline` | O | 仓库、worktree、读取戳、`HeadState` | arm 之后读一次每个 worktree 的 HEAD |
| `call_fact` | O | 会话、调用 ID、路径（不分代）：位置、worktree、相对路径、进入的 worktree | 读到这个调用时，只写一次 |
| `call_obs` | O | 会话、调用 ID、worktree（不分代）：片段（或 detached 的提交）、HEAD 的提交、确定性（确定 / 不确定）、调用期间确定停留过的片段；`Execute` 解析出 `git push <远端> HEAD:<分支>` 时另有推送观察：远端跟踪引用名、匹配的推送记录（新 OID、行时刻、说明文字）或「没有」「读不到」；`Read`、`Search` 的连续相同观察合成一行 | 结果时刻之后 2 秒，或定稿时补算（「轮次的观察与改动项」「分支观察」） |
| `turn_obs` | O | 会话、`TurnId`（不分代）：轮次的最后时刻 L、cwd 的 worktree、片段、确定性 | L 之后 2 秒，或定稿时补算 |
| `segment`、`segment_name` | O | 片段 ID（16 字节）、仓库、开始的原因与时刻、前一个片段、结束的原因与时刻；片段的分支名与生效时刻（改名） | reflog 的创建、改名、删除、重置记录到达时 |
| `boundary_obs` | O | 前后两个片段、种类（删除重建、丢弃独有提交的重置、回到起点）、新旧位置的提交关系、改写的证据、当时有没有挂起的 stash | 边界发生时（「一次使用」） |
| `worktree_obs` | O | worktree、读取戳、种类（干净 / 切换分支时的未提交路径）、路径集合 | 分支切换记录到达、回到起点的候选出现时 |
| `stash_obs` | O | stash 提交、仓库、来源 worktree 与片段、创建时刻、改动块的摘要；恢复：目标 worktree 与片段、证据种类、是否匹配、时刻 | stash 创建与恢复的候选出现时 |
| `item` | O | 会话、代、轮次、调用、路径、改动行数；依赖状态的部分（片段、是否为默认分支、去向）放在按（会话、调用 ID、路径）不分代的 `item_state` 里，重放时沿用 | 轮次定稿时（「轮次的观察与改动项」） |
| `transfer` | O | 被转移的改动项或轮次、从哪个片段到哪个片段、规则（`switch -c`、切到已有分支、stash 恢复、cherry-pick、推送） | 转移的证据成立时 |
| `analysis`、`meter` | O | 分析 ID、agent、对象、开始时解析出的归属（片段或 MR 与权重）；请求序号、状态、模型、四类 token、是否估算 | `begin_analysis`、`meter` |
| `usage_value` | D | 每个键当前计数的那个来源 | 来源增删时 |
| `share` | D | 轮次、目标（项目或位置、片段或「不属于任何 MR」的种类）、权重与权重和 | 轮次定稿、转移、受影响的轮次重算 |
| `fact` | D | 时刻、15 分钟桶、项目或位置、片段或种类、编写 / 审阅、agent、模型、会话、轮次、四类 token（百万分之一） | 随 `share` |
| `fact_bucket` | D | 15 分钟桶 × 位置 × agent × 模型 × 编写 / 审阅：热事实行的四类 token 之和与行数（`WITHOUT ROWID`）；时间窗口的查询读它，不读 `fact` | 与 `fact` 的每次增删同一事务；月份归档完时删掉这个月的行；重建时从 `fact` 重算 |
| `cold_part` | O | UTC 年月、表、段序号、行数、未压缩字节数、压缩后的块、写入时刻 | 冷归档（「冷热分层」） |
| `cold_month` | O | 项目或位置、本地年月、agent、模型、编写 / 审阅、四类 token | 冷归档，与 `cold_part` 同一事务 |
| `cold_total` | O | 片段或会话、编写 / 审阅、四类 token（已归档部分的累计） | 同上 |
| `cold_anchor` | O | 会话、已归档的最后一个有改动轮次及其份额；派出仍有热数据的子会话的父轮次及其份额 | 同上（有改动的轮次随第 4 步出现，第 1 步不写） |
| `cold_seen` | O | 会话、UTC 月：这个会话在这个月有已归档的来源 | 来源的段归档时，与 `cold_part` 同一事务 |

表结构版本 2 在版本 1 之上加冷数据的五张表、`fact_bucket`（迁移时从 `fact` 算出）和下面两个按时刻的索引（迁移前照常写副本）。冷数据的三张汇总表和 `cold_seen` 在抢救时与观察一起复制（都是 rowid 表）。

索引：`fact` 按（时刻）、（片段）、（会话）；`usage_source` 按（键）、（会话、代）、（时刻）；`turn` 按（开始时刻）；`reflog` 按（仓库、文件、`seq`）；`call` 按（会话、代、轮次）。按时刻的索引让归档按月找到行。

### 冷热分层

人的决定（2026-10-22，「用量数据按年维度吧」「超过一年的可以压缩作为冷数据」）：最近一年是热数据，更早的压缩成冷数据保留，不提供手动清除。

- **热窗口**：截止点是今天往前第 364 个本地自然日的零点（`window::days` 的同一套规则，`LocalZone` 取当前时区），截止点之后的记录都在上面的普通表里，明细完整。7D / 14D / 30D 和全部现有快照只读热数据；交互规范里最长的窗口是 30 天，本文不加更长的界面。
- **按 UTC 月归档**：一个 UTC 日历月整个早于「截止点减 1 天」（留出 ±14 小时的时区偏移）时，它的有时间戳的行移到冷数据，所以热数据实际保留 365 到 396 天。按整月而不是按天归档：一个月只写一次、段更大、压缩比更高，截止点每天移动也不会产生一串小段。
- **哪些行归档**：带时间戳的高频表，每一行按它自己的时间戳分月：`usage_source`、`usage_value`、`turn`、`call`、`call_fact`、`call_obs`、`turn_obs`、`item`、`item_state`、`transfer`、`share`、`fact`、`reflog`、`reflog_reset`、`head_baseline`、`watch_span`、`boundary_obs`、`worktree_obs`、`stash_obs`、`meter`。小的实体表一直是热的：`meta`、`run`、`location`、`project`、`project_fact`、`alias`、`session`、`segment`、`segment_name`、`analysis`、`limit_sample` 和冷数据的三张汇总表。所以跨过截止点的会话、轮次、片段、MR 按记录的时间戳拆开，热的一部分和冷的一部分相加等于原来的合计。
- **冷数据段的格式**：放在同一个数据库的 `cold_part` 表里，不另放文件：归档和删除热数据在同一个 SQLite 事务里完成，不需要跨文件的提交协议。一段是一个表在一个 UTC 月里的一组行，未压缩不超过 1 MiB（一个月可以有多段，段序号递增；一段的耗时主要是删除热行，段大小按 `usage/archive_month` 的每段预算定）：文件头（格式版本、表、表结构版本、列名、行数），之后按列存放（整数列与前一行的差按 zigzag 变长编码，字符串和字节串带长度，字符串和路径本来就是驻留表的 ID），整段用 `ruzstd` 的 Fastest 级别压成一个带内容校验和的帧，读出时核对校验和。观察和派生的事实行都进冷数据段：前者用于完整重建，后者让冷数据的聚合不必重算归属。`fact` 的段每行多一列 `cold_ym`：这一行计入 `cold_month` 的本地年月，完整重建按它从汇总里减回去，不受之后时区变化的影响。
- **一直热的汇总**：同一事务里更新 `cold_month`（项目或位置 × 归档时的本地年月 × agent × 模型 × 编写 / 审阅的四类 token），长时间范围的合计不用解压；`cold_total`（每个片段、每个会话已归档部分的累计），所以 worktree 的累计、MR 的累计和会话的合计仍然是完整的值；`cold_anchor`（每个仍有热数据的会话里已归档的最后一个有改动轮次和它的份额，以及派出仍有热数据的子会话的父轮次），所以热数据里的轮次跟随、子 agent 跟随父轮次不需要冷数据。
- **月份的归档状态**：`meta` 里每个 UTC 月一个状态：热（默认）、归档中、已归档。开始归档一个月时先在一个事务里把它设为「归档中」。状态是「归档中」或「已归档」的月份冻结：补读与入库照常写观察，但写 `share`、`fact`、`usage_value` 的路径（定稿、受影响轮次的重算、去重的重新选出、热窗口重建）都跳过时间戳落在这些月份里的记录，热窗口重建也不碰它们。所以一个月归档到一半时退出，热的一侧既不会重算出第二份事实行，也不会少掉这个月。
- **归档的步骤与持久性**：表的顺序固定：先是观察表（按「哪些行归档」列出的顺序，`fact`、`share`、`usage_value` 除外），再是 `share`，最后是 `fact`。`usage_value` 没有时间戳、只能按键查找（按来源查找要一个与这张表一样大的索引），它的行随所指的来源所在的段归档：同一个事务里写两段（`usage_source` 和这些来源的 `usage_value`）。每一段一个事务：读出这一段的热行（按时刻索引的顺序），编码、压缩，再解压、解码一遍，与编码的字节和原始行逐一核对；核对通过后在同一个事务里插入 `cold_part`、删除这些热行，提交（`synchronous = FULL`，提交返回时已落盘）。来源的段同时记下 `cold_seen`。`fact` 的段与三张汇总表的更新在同一个事务里；让这个月不再有热行的那个事务（通常是最后一段 `fact`）同时把状态设为「已归档」。在那之前，累计查询（`Worktree`、`Session`、`Mr`）照常读这个月还在热表里的 `fact`，`cold_total` 还没有加上这个月，冷的一侧只有观察，没有事实行，所以没有重复也没有缺口。崩溃或失败时事务回滚，热行原样还在；重启后状态仍是「归档中」的月份从第一张还有热行的表接着做，所以重复执行不会重复归档。核对失败时不删除热行，记 error，`meta` 记下失败时的段格式版本，这个月停在「归档中」（仍然冻结），归档计划跳过它，直到段格式的版本变了再试。
- **之后到达的旧记录**：补读第一次见到的、落在已冻结月份里的记录（新安装时 Codex 一年多以前的会话）照常写进热的观察表，不写事实行：已冻结月份里的热行就是待归档的行，不另加标记。归档计划找到这个月还有热行，下一轮归档先把状态改回「归档中」，之后每一步在归档事务里为冻结之后才写入的来源算出份额和事实行，再照常写成新的段（段序号接着原来的）并更新汇总。冻结之后写入的来源是 ID 大于 `archive_mark:<月>` 的那些：第一次设为「归档中」的事务记下来源的最大 ID（ID 只增不减），每一步处理完再记一次；只为热表里没有计数来源的键、只在这些来源里选。不能对整月的来源这样做：先归档的段已带走了它们的 `usage_value`，后面段里同一个键的另一份来源会被当成没有计数、再计一次。一个会话在已冻结月份里已有数据时（`cold_seen` 有这个会话和月份，或回放时有更早一代的热行），它这个月的用量记录和轮次不再写入（冷数据为准，计数进日志）：会话重放（`Batch::reset`）和抢救之后从头重读都不会把同一份用量计两次。重放读完时删除旧一代，已冻结月份里旧一代的行留下（保留原来的代号），由归档带走。不同会话之间、跨过截止点的同一个键不去重；之后到达的记录与这个月已归档的来源是同一个键时也会再计一次（已知限制：日志里非继承的重复记录几乎都在同一时期，一年多以前的日志很少在归档之后才第一次读到）。
- **节奏**：归档在首帧之后、按补读的节奏运行（kernel 静止 10 秒后一段，持有 `BusyGuard`），不在 UI 线程上，只在没有会话要读时进行。归档计划在数据库打开后和每个本地零点各查一次（按时刻索引逐月找第一行，每月一次查找），之后每段静止做一步：设为「归档中」、一段、或空间回收的一批。一个重度月约 30 MB 的热行，按 1 MiB 的段约 26 段（实测一个月里 21 个重度日 18 段），约 5 分钟的静止时间。
- **空间回收**：删除的热行留下空闲页。热数据大致稳定在一年的量，新的记录会复用这些页，所以默认不回收。空闲页超过文件的 25% 且超过 64 MiB 时（比如用量明显变少之后），在同样的节奏里每批 `PRAGMA incremental_vacuum(2048)`（每批最多 8 MiB），不做整库 `VACUUM`：它要重写整个数据库、临时占用同样大的磁盘、持续几秒的写入。两者都不改变页缓存的上限（每个连接仍是 1 MiB）。
- **读冷数据**：只在请求时进行。现有的快照都不读冷数据段；`UsageQuery::History` 给以后的界面用：按月的合计只读 `cold_month`，按日的明细逐段解压这个范围里的 `fact` 段（后台，每次一段，结果不常驻）。按月的行总是整个本地月（`from`、`to` 落在月中时也是），热的一侧与 `cold_month` 口径相同；热的事实行、`cold_month` 和要解压的段的列表在一个读事务里读出，中间提交的归档一步不会让同一份用量计两次。MR 区间的起点或终点落在冷数据里时（一年多以前开出、仍开着的 MR），`Mr` 和 `Worktree` 查询为那一个月的 `fact` 段解压一次，按时间切分。
- **重建**：重建热窗口只读热表和三张汇总表，不需要冷数据；已冻结月份的派生行不删也不重算。完整重建（派生算法版本变化时可选）逐月把冷数据段里的观察放回热表：每个月一个事务，放回观察行、按 `cold_ym` 从 `cold_month`、`cold_total` 减去这个月的事实行、删除这个月的段和 `cold_seen`、状态回到「热」；之后重建热窗口（这些月份已在其中），下一轮归档再把它们写成新的段。任何一个事务之后中断，热表与汇总都是一致的。数据库恢复（抢救）时冷数据段和汇总表按观察复制，已冻结月份的 `fact` 与 `usage_value` 也复制（它们的来源可能已经是冷的，重建不会重算它们），解码不了的段留在保留的原文件里并计入丢失。冷数据里的轮次不因之后的转移、跟随而重算：它们的份额在归档时固定（已知限制），别名、`uses`、MR 区间照常在汇总时映射到冷数据的事实行和汇总上。

### 项目标识

每个观察写入时带上位置；git 仓库的位置是公共目录，项目 ID 在位置第一次出现时分配（「从未打开」状态），写进 `item`、`share`、`fact`（ADR 0020「每个项目有 Lectern 分配的固定标识」）。同一仓库的所有 worktree 是同一个位置、同一个项目。不是 git 仓库的路径只记位置，不分配项目：打开的文件夹项目在汇总时认领根目录之下的位置（最深的根优先），所以之后打开的文件夹也能拿到以前的用量。

识别时读的事实（`RepoFacts`）：根提交（`root_commits(默认分支的提交, 4)`；没有默认分支时用 HEAD）、全部远端的地址集合、主远端、代码平台仓库 ID（经 `MrSource::repo`，取不到为未知）、是否还没有提交。主远端在第一次识别时选定并记下，之后沿用：默认分支上游所在的远端；没有时唯一的远端；多个远端时取完整的地址集合。记下的远端被删掉或改名时重选，新增远端不影响。远端地址比较前统一成主机加路径（去掉协议、用户名、末尾的 `.git` 与 `/`，主机名小写）。

`identity::identify` 的判定，`seen` 是这次打开的仓库，`known` 是位置上（或别名指向的位置上）记下的项目：

| 情况 | 结果 |
|---|---|
| 位置没有记录，有路径已不存在（或已是别的仓库）的已知项目作为候选：两边的仓库 ID 相同且只有一个候选（根提交和远端变过也算）；或没有仓库 ID 时，根提交和远端地址集合都相同、集合非空、只有一个候选 | 移动或改名：记别名「旧项目 → 这个位置」，沿用旧项目 ID |
| 位置没有记录，其余情况（包括 fork、有歧义） | 新项目；路径已不存在的项目在 Usage 页提供「合并到…」 |
| 已知位置，根提交相同；两边都有仓库 ID 且相同，或都没有 ID 而主远端地址相同，或都没有远端 | 同一个项目 |
| 已知项目有仓库 ID，这次取不到 | 临时项目（`ForgeIdPending`），无论根提交是否相同；取到 ID 后按本表重新判定，相同时自动并入，不同时转为新项目 |
| 已知位置，根提交相同，仓库 ID 不同，或都有远端而主远端地址不同且没有仓库 ID | 有仓库 ID 时：新项目（路径被 fork 或无关仓库占用）。没有仓库 ID、只有主远端地址不同时：临时项目（`PrimaryRemoteChanged`），候选是原项目 |
| 已知位置，根提交不同，仓库 ID 相同（没有 ID 时主远端地址相同） | 改写了历史：同一个项目，记别名「改写」 |
| 已知位置，根提交不同，两边都没有远端和仓库 ID | 临时项目（`LocalRewrite`），候选是原项目 |
| 已知位置，根提交不同，其余情况 | 新项目 |
| 仓库还没有提交 | 临时项目（`Unborn`）；第一个提交出现后（HEAD 的 reflog 记录）按本表用完整标识重新判定，并入候选或转为新项目 |

临时项目本身就是一个新项目：用户不确认时它一直是单独的项目（`crates/usage/AGENTS.md`「对不上时为它分配新项目」），确认「是同一个项目」时追加别名并入候选，「是新项目」时转为 `Active`。`PrimaryRemoteChanged` 这一行按 `crates/usage_view/AGENTS.md` 的两种临时项目给出确认操作：远端改名和 fork 占用路径在没有仓库 ID 时分不清，不自动合并。

别名只追加：合并、确认、移动、改写、撤销各是一条记录，撤销指向被撤销的那一条。汇总时按记录的顺序重放出「项目 → 最终项目」的映射，所以撤销之后的用量回到原项目，不需要改写任何观察。

### 轮次的观察与改动项

一轮的生命周期：

1. 读到 `TurnStarted` 时写 `turn`；之后的 `Usage` 记录先按「临时归属」写事实行：跟随同一会话里最近一个已定稿轮次的份额，会话还没有已定稿的轮次时按 cwd 的位置和 cwd 所在 worktree 的当前片段，快照里立即可见。
2. 读到 `ToolStarted` / `ToolFinished` 时写 `call`。`Execute` 的命令文本当场按「命令文本」解析，只保存解析出的事实和提到 git 的命令原文（前 4 KiB），其余命令只保存进入的 worktree；`Read`、`Search` 只保存目标路径所在的 worktree。路径到 worktree 的映射用 `Projects::locate`，不在打开的项目里时用 `Projects::resolve`（按目录去重缓存），结果写进 `call_fact`。
3. 结果时刻之后 2 秒（等 fs_watch 读到这段时间的 reflog），为 `Edit`、`Execute`、`Read`、`Search` 调用在它们涉及的 worktree 上算 `call_obs`（「分支观察」）：`Edit` 是改动路径所在的 worktree，`Execute` 是工作目录和进入的 worktree，`Read`、`Search` 是目标所在的 worktree。`Read`、`Search` 在同一轮、同一个 worktree 上结果相同的连续观察合成一行（带调用数），所以频繁的读文件不让表变大。导入的轮次没有 `call_obs`。
4. 轮次的最后时刻 L：Codex 是 `TurnEnded` 的时刻；Claude Code 是属于这一轮的最后一条记录的时刻（下一个 `TurnStarted` 之前的那一条，不是下一个 `TurnStarted` 本身）。L 之后 2 秒为 cwd 所在的 worktree 算一条 `turn_obs`（与 `call_obs` 同样的回放，S = T = L）。
5. 定稿：两个条件都满足时进行：读到结束的标志（Codex 的 `TurnEnded`；Claude Code 的下一个 `TurnStarted`，或会话的 `open_turn` 为 false），并且当前时刻不早于「这一轮最后一个结果时刻与 L 中较晚者加 2 秒」。不满足第二条时由定时器在那个时刻再试，所以结束记录与最后一个 `ToolFinished` 在同一批里到达时也要等满 2 秒；定稿任务先在同一个事务里补算还缺的 `call_obs`、`turn_obs`，再写这一轮的 `item`、`share`，用它们替换临时的事实行。导入的轮次没有观察要等，读到结束的标志即可定稿（补读时见下面的「补读的定稿」）。

**固定下来的观察**（ADR 0020「观察先落盘，之后不再修改」）：依赖当时状态的事实按（会话、调用 ID、路径）或（会话、`TurnId`）保存，不属于任何一代：`call_fact`（位置、worktree、相对路径、进入的 worktree）、`call_obs`、`turn_obs`、改动项的片段、是否为默认分支和去向。会话日志重放（`Batch::reset`，如适配器升级）时，新一代里调用 ID 已有记录的调用沿用这些值，只重算与状态无关的部分（行数、份额）；只有新出现的调用才按当时的状态计算。所以合并之后被删除的 worktree、已经改变的默认分支不会改写以前的归属。ADR 0047 写明了这一点。

**补读的定稿**：任何会话（补读的会话，以及第一次安装或适配器重放后还在追赶的活跃会话）在它第一次读到 `at_end` 为 true 之前，轮次只写 `turn`、`call` 和临时事实行，不定稿；之后的追加照常按第 5 步定稿。原因是：没读到末尾的 `read` 建出的轮次索引不记文件身份（agents 设计「与设计的差异」第 4 步），每批调 `turns_unretained` 都会从游标解析到文件末尾，大会话的补读会变成平方级。读到末尾之后，这个会话的定稿分批进行，每批最多 50 个轮次或 100 ms：每批调一次 `turns_unretained`（这时 `cache` 里的索引已到末尾，命中、不再解析），再 `load` 这一批的改动。批与批之间按补读的节奏，不持有守卫和索引。不选「让 `ToolStarted` / `ToolFinished` 带上 `ContentRef`」：那要改 `services` 的类型和两个适配器，适配器版本加一会让全部游标从头回放。

改动项（ADR 0047）：这一轮每个 `Completed` 的 `Edit` 调用，经 `SessionSource::turns_unretained` 找到它的结果引用（没有时用输入），`load` 出 `FileEdit`，每个文件一项：

- 行数：`Replace` 为 `git::line_diff(old, new, 4 MiB)` 的增删行数之和（超过上限时按两段内容的行数之和）；`Patch` 为 hunk 里 `+`、`-` 行的数量；`Add` 和新建的 `Write` 为内容的行数；覆盖的 `Write` 没有补丁时为内容的行数；`Delete` 为 0。行数为 0 且不是 `Delete` 的一项不算改动（「写过文件但没有留下改动」）。
- 文件：路径去掉 worktree 根的前缀得到相对路径；不匹配时先用日志里的 cwd 与规范 cwd 之间的前缀替换再试（与 review 设计「改动步与声明」相同）。不在任何 worktree 里的路径按所在目录记位置，不属于任何分支。
- 分支：这次调用的 `call_obs` 在这个 worktree 上的片段；不确定时这一项的目标是「无法归属」。导入的轮次没有 `call_obs`：文件在 cwd 所在的 worktree 里时用日志记录的分支在当时对应的片段（没有 reflog 片段时用名称片段，见「分支观察」），否则（或日志没有记录分支）为「无法归属」。
- 去向：同一轮里之后的 `Execute` 调用在同一个 worktree 上切换了分支时，这一项跟着内容走：调用期间这个 worktree 新增的提交里改了这个文件的（`tree_changes(旧, 新)` 含这个路径，按提交所在分支的 reflog 新增记录），归那个提交所在的片段；没有提交的，归调用结束时的片段（未提交的改动随工作区带走）。去向写在 `item` 上，权重按去向的片段计。
- 是否为默认分支：定稿时 `Repo::default_branch` 与片段当时的分支名比较。

权重：一个目标的权重是落在它上面的改动项的行数之和加上不同文件的个数（ADR 0020「改动行数 + 改动文件数」）。

### 命令文本

`command::parse` 按 POSIX shell 的写法切词（单双引号、反斜杠转义；`;`、`&&`、`||`、`|`、换行分隔命令），不展开变量和命令替换，含它们的那一段按「不确定」处理（ADR 0020「已知限制」）。认出的事实：

| 写法 | 事实 |
|---|---|
| `cd <目录>`、`pushd <目录>`、`popd` | 之后各段的工作目录；目录（相对的按当前段的工作目录拼接，词法上去掉 `.` 与 `..`）所在的 worktree 记为「进入」 |
| `git -C <目录> …` | 这一段的 worktree 记为「进入」 |
| `git stash` / `git stash push` / `save`（`-u`、`-a`）、`git stash apply` / `pop [<stash>]`、`git stash drop` | stash 的创建、恢复（目标 `stash@{n}`、OID 或默认的 `stash@{0}`）、删除 |
| `git push <远端> HEAD:<分支>`、`<源>:<分支>` | 推送的远端与目标分支 |
| `git cherry-pick <提交>…` | 被拣选的提交（OID 或可解析的引用） |
| `git rebase …`、`git commit --amend` | 改写的证据 |
| `git reset …`、`git branch -f`、`git checkout -B`、`git switch -C` | 可能丢弃独有提交的重置 |
| `git switch -c`、`git checkout -b`、`git worktree add -b` | 新分支的创建 |

`stash@{n}` 按调用开始之前 `refs/stash` 的 reflog（已入库的记录）解析，只认它指向的那一条。

### 分支观察

reflog 记录 R 的写入时刻区间：下界取 `R.after.at` 与行里的时间（秒）的较大者，上界取 `R.observed.at` 与「行里的时间 + 1 秒」的较小者；没有 `after` 时只用行里的时间和 `observed`。同一运行里两条记录的先后按 fs_watch 设计「reflog」的规则判断，跨运行按 `run` 表的序号。调用的开始时刻 S、结果时刻 T 取 `ToolStarted::at`、`ToolFinished::at`（日志写入时刻，与读取戳用同一个系统时钟）。

一次调用在一个 worktree 上的 `call_obs`：

1. 起点是这个 worktree 的 `head_baseline` 或更早的 `call_obs`，之后按顺序应用这个 worktree 的 HEAD reflog 记录（`checkout: moving from A to B` 改变分支；`commit`、`reset`、`rebase` 等只改变提交）。不读解析时的当前 HEAD。
2. 记录相对 T：区间整个在 T 之前的计入；整个在 T 之后的不计；跨过 T 的让这次调用为「不确定」。
3. 计入的切换里，整个在 S 之后的是调用期间的切换；跨过 S 的切换只有一条时，它的目标照常计入（调用期间确定停留过的分支不含它的起点），连续有几条时这次调用为「不确定」。
4. 分支名按当时的 `segment_name` 映射到片段；detached 记提交。HEAD 确定停留过的片段（调用开始时的、调用期间整个在 S 之后的切换目标、结束时的）写进 `call_obs`。
5. 这个仓库的 `watch_span` 不覆盖 [S - 1 秒, T + 2 秒] 时，这一轮按导入的轮次处理，不算 `call_obs`。

片段：

- 新建：分支 reflog 的创建记录（`branch: Created from …`、`checkout -b`、`worktree add -b`），或第一次在 HEAD 或 `head_baseline` 里见到一个没有片段的分支。arm 时为每个分支读一次 `Repo::ref_reflog`（前置条件 D），按其中的创建、删除重建和重置记录补出历史上的片段，导入的轮次按日志记录的分支名和时刻找到对应的片段。现存分支的 reflog 里没有创建记录时（git 默认 90 天后清掉可达的旧记录，或分支创建时没有写 reflog，或 `complete` 为 false、只读了末尾 4 MiB），它最早的片段的起点记为「向过去开放」：早于最早一条 reflog 记录的导入轮次也落在这个片段上，与之后的历史是同一次使用。
- 名称片段：导入的轮次记录的分支名在那个时刻没有 reflog 片段时，只在两种情况下用：这个仓库已经没有这个名字的分支（合并之后删掉的分支没有 reflog 文件，补读的 30 天里这是常见情况），或现存同名分支的 reflog 有创建记录、且创建晚于这一轮（同名分支复用）。这时为（仓库、分支名）建一个只有名字的片段，同一仓库、同一名字的导入轮次共用它。它自己是一次使用，不与同名的 reflog 片段合并（同名分支复用的规则）；默认分支的名字映射到默认分支的片段。P3 起按这个名字和仓库的主远端调 `mrs_for_branch`，MR 的时间线覆盖轮次时刻时按「MR 区间」归到那个 MR；同一个名字先后有几个 MR 时按区间切分，都不覆盖时是一行未开 MR 的分支。
- 改名：`BranchRenamed` 记录让同一个片段换名，片段 ID 不变。
- 结束并开始新片段：分支 reflog 文件被删除（`ReflogReset::Removed`，不是改名的旧名字）后同名分支重建；`reset`、`branch -f`、`checkout -B` 让分支离开了旧位置的独有提交（旧位置不是新位置的祖先）；只有未提交改动的分支回到起点且工作区干净（下一节）。每个边界写一条 `boundary_obs`，前后是否属于同一次使用在汇总时判断。
- 默认分支和 detached HEAD 也有片段，`item` 另记是否为默认分支。

### 一次使用

`uses(segments, boundaries, mrs) -> 片段 → UseId` 在汇总时计算，结果按（片段集合、边界、MR 数据的代数）缓存在内存里。没有边界相连的片段各自是一次使用；有边界时按下表判断前后是否连续：

| 边界 | 连续（同一次使用）的条件 |
|---|---|
| 改名 | 总是（同一个片段） |
| rebase、amend、合并默认分支、强制推送、快进 | 不产生边界 |
| 删除后重建 | `forge` 证明边界发生时这个分支对应的 MR 开着；或边界时关闭、之后同一个 MR 在边界之后的片段上重新打开；或 `forge` 现在仍把分支对应到开着的 MR |
| `reset`、`branch -f`、`checkout -B` 丢弃了独有提交 | 新位置包含旧位置的全部独有提交（`is_ancestor`，按 `boundary_obs` 里记下的关系）；或有改写的证据：同一时刻的调用里有 rebase / amend（「命令文本」），或旧位置的每个独有提交都按 `patch_id` 在新位置的独有提交里一一对应；或上一行的 `forge` 条件。只看 reflog 的说明文字不算 |
| 只有未提交改动的分支回到起点且工作区干净 | 那一刻这个 worktree 上创建了 stash（`refs/stash` 的新记录）时只是挂起：这条 stash 之后恢复到同一个分支上时连续；被删掉且改动没有恢复时，在删除的时刻结束。或上面的 `forge` 条件 |

「回到起点」的候选：一个片段没有独有提交（分支位置等于它的创建位置），之前有改动项落在它上面，并且之后出现了一次这个 worktree 的 HEAD 记录、或这个 worktree 上的 `stash` 命令。候选出现时（跟踪中的仓库，当场）用计数的 `Worktree::changes(Base::Head)`（`retained_bytes = 0`、`total_patch_bytes = 0`）判断是否干净，写 `worktree_obs`。

工作转入新分支（`transfer` 记录，转移之后的份额按新片段计，ADR 0020 的「已经在某个 MR 分支上的工作不转移」：只转移目标是默认分支、detached 或无法归属的改动项）：

| 规则 | 证据与条件 |
|---|---|
| `switch -c` / `checkout -b` / `worktree add -b` 带走未提交的改动 | HEAD 记录从默认分支或 detached 切到一个按创建记录新建的分支，新分支的起点等于切换前的 HEAD；切换前这个 worktree 里还没有提交的改动项（去向不是某个提交）转到新片段 |
| 切到已有分支时带过去的未提交改动 | 切换记录到达时（跟踪中的仓库）读这个 worktree 的未提交路径写 `worktree_obs`；默认分支或 detached 上还没有提交、路径在其中的改动项转到目标分支的片段 |
| stash 恢复到另一个分支 | 按改动块认定（`stash::restored`）：stash 的每个改动块按恢复后的工作区文件判断，加上的行都在、删掉的行已不在，目标分支 HEAD 已有一部分也算满足，且至少有一行出现在工作区相对 HEAD 的改动里；二进制和只改模式的改动按 blob OID 与模式比较；`-u` / `-a` 存下的文件取自 stash 提交的第三个父提交（`git::Repo::stash`），这些路径经 `RepoWatch::set_unfiltered` 加进不过滤清单。恢复的证据：这个 worktree 上执行过的 `git stash apply` / `pop`，或这条 stash 从 `refs/stash` 的 reflog 消失的时刻与改动出现的时刻相差不超过 5 秒。匹配的改动至少有一处要在恢复时（命令证据含结果之后 5 秒）被文件监听记到变化：本 crate 为有候选 stash、且有跟踪中会话的 worktree 持有 `WorktreeWatch`，没有监听的 worktree 不认定恢复。命令里的 stash 按执行前的 reflog 解析，只认它指向的那一条；解析不出来时内容匹配的候选要恰好一条。产生这些改动的轮次的改动项转到目标片段 |
| cherry-pick 到新分支 | HEAD 记录是 `cherry-pick`；原提交先按命令里的提交 OID，没有命令时按 `patch_id` 在已入库、带去向的提交里找，候选要恰好一个；原提交的改动项目标是默认分支、detached 或无法归属时，改动项转到新片段 |
| detached 上的提交推到某个 MR 的源分支 | 命令是 `git push <远端> HEAD:<分支>`，这次调用的推送观察（`call_obs`）找到了远端跟踪分支在 [S − 1 秒, T + 1 秒] 内、更新到调用期间 HEAD 停留过的某个 detached 提交的推送记录；或之后 `MrSource::mrs_containing` 只给出一个 MR。detached 片段上这些提交的改动项转到那个 MR 源分支的片段（没有本地分支时新建一个只有远端名的片段） |

ADR 0020「已知限制」照旧：同一个提交被 cherry-pick 到几个分支、各自开 MR 时，改动项只转到第一个满足条件的分支；`git branch` 后 `reset` 再推送这类补救流程不转移。推送观察用不上时（推送到地址而不是远端名、远端的 fetch refspec 不覆盖这个分支，没有远端跟踪记录；`<源>` 不是 `HEAD`，不按推送观察判断），detached 的推送只按 `MrSource::mrs_containing` 转移，`forge` 不可用时不转移。

### MR 区间（P3）

本地分支到 MR：按 `Repo::branches` 解析出的上游和推送目标的远端地址，经 `MrSource::repo` 得到仓库，再 `mrs_for_branch(仓库, 远端分支名)`；不只比较分支名（ADR 0020）。`forge` 有对应 MR 时优先于默认分支的规则：默认分支上的片段有 MR 时按 MR 归属，不进「不属于任何 MR」。

`mr_intervals(use, mrs)`：每个 MR 的区间在它所在的一次使用之内。

- 起点：这个 MR 开出（第一条 `Opened`）之前，同一次使用里最近一次任何 MR 的合并或关闭事件的时刻；没有时是这次使用的开始。之后别的 MR 重新打开不改变它。开 MR 之前的工作因此计入。
- 终点：这个 MR 最后一次合并或关闭；关闭后又重新打开时，关闭期间的工作并入。还开着时没有终点。
- MR 详情显示自己区间内的用量；区间重叠的几个 MR 各自都显示那段用量并注明共用（`MrUsage::shared_with`）。共用分支时其中一个关闭后，之后的用量只计入还开着的 MR。
- Usage 页的行：同一次使用上区间互相重叠的 MR 合为一行（`RowKey::Mrs`，只含这些 MR 区间内的用量）；同一次使用里先后开出、区间不重叠的 MR 各占一行；不同分支的 MR 不合并；区间之外的同一次使用是一行 `RowKey::Branch`；默认分支、detached、无法归属合成 `RowKey::Unassigned`。项目合计每条事实行只算一次。
- 离线或 `MrSource` 不可用时按分支显示，不影响统计（ADR 0020「分阶段」）；数据到达后重新映射，不改观察。

### 分摊

一轮的份额按下面的顺序决定，第一条适用的规则生效。目标是（项目或位置，片段或「不属于任何 MR」的种类）：

1. **有改动**：这一轮自己的改动项（含去向与转移），按权重分摊；改动跨项目时一起按权重分摊，不看 cwd。父会话里只派出子 agent、自己没有改动的轮次，按它派出的子 agent 子树里的改动权重分摊（ADR 0020「子 agent」）。
2. **跟踪中的轮次，有进入 worktree 的证据**：这一轮的命令进入了某个 worktree（`cd`、`pushd`、`git -C`），或读文件、搜索工具的目标在某个 worktree 里，或写过文件但没有留下改动、写文件的目标在某个 worktree 里：按这些 worktree 当时（相应 `Execute`、`Read`、`Search`、`Edit` 调用的 `call_obs`）的片段和项目平均分摊，优先于跟随。
3. **cwd 的 worktree 换了分支**：这一轮的 `call_obs` 或 `turn_obs` 确定观察到 cwd 所在 worktree 换了分支，而同一会话里最近的改动就在这个 worktree：按这一轮在 cwd worktree 上观察到的片段平均分摊。
4. **跟随**：同一会话里最近的有改动轮次，先找之前、再找之后，项目和片段一起照搬它的份额。
5. **子 agent 会话整个没有改动**：跟随派出它的父轮次（`Parent::tool_call` 所在的轮次；为 None 时按子会话第一轮的开始时刻落到的父轮次）。整棵子树都没有改动时按下一条。
6. **会话里没有任何改动**：跟踪中的轮次，按这一轮的 `turn_obs`（轮次的最后时刻 L 的 cwd 片段）和这一轮 `Execute` 调用在 cwd worktree 上的 `call_obs`（含调用期间 HEAD 确定停留过的片段）平均分摊；项目按 cwd。导入的轮次用日志记录的分支（`Context::logged_branch`）在 cwd 的 worktree 上对应的片段。
7. 以上都得不到片段（导入的轮次没有日志记录的分支、观察为不确定）：cwd 的项目里「不属于任何 MR」的「无法归属」。cwd 和改动都不在任何项目里：按位置记，汇总时归入「其他」，之后打开对应项目时自动归入。

一个轮次定稿时，同时重算受它影响的轮次：同一会话里它之前那段按「再找之后」跟随它的轮次、派出它所在会话的父轮次（规则 1 的子树权重）、跟随父轮次的子会话（规则 5）。转移记录写入时重算被转移的改动项所在的轮次。

审阅（Analyze）：`begin_analysis` 时解析对象的归属并写进 `analysis`：`Session` 是被分析轮次的改动项权重（都没有改动时用它们的份额）；`Worktree` 是那个 worktree 当时的片段；`Mr` 是这个 MR。P3 起同时解析出这些片段当时所属的 MR（含已关闭的），之后汇总时审阅的事实行按记下的 MR 归属，不受 MR 区间限制（ADR 0020「MR 标识在发起时确定」）。计量记录按这些权重分成事实行，标为「审阅」。

计量的状态：`Prepared` 先按上限写一条估算的事实行；`Usage` 替换为实际值（`done` 之前也替换，取最近的一次）；`Rejected` 且没有用量记为 0；`Forwarded`、`Accepted` 只改状态。重启时，只有 `Accepted`、还没有 `done` 的用量的请求按 `Prepared` 的上限结算，标为「估算（上限）」；`Prepared`、`Forwarded` 状态的请求作废（`crates/usage/AGENTS.md`）。

### 取整与时间窗口

- 一轮的每条用量记录按份额拆成事实行：`round::split(tokens, 权重)`，每个类别分别拆，以百万分之一 token 为单位的整数，最大余数法让各份之和恰好等于原值（ADR 0020「分摊按分数保存、精确相加」）。份额本身以（权重、权重和）保存。
- 快照里的每一组行（项目的 `rows`、`by_model`、`by_agent`，所有项目的行加「其他」，每天按 agent）在每个类别上分别用 `round::largest_remainder` 取整，所以各行相加等于合计。MR 详情里共用区间的行各自取整，不要求与项目合计相加。
- 时间窗口按每条事实行的时刻、用户本地时区的自然日划分（ADR 0020）。SQL 按 15 分钟的 UTC 桶求和（时区偏移都是 15 分钟的整数倍），本 crate 用 `window::days` 把桶归到本地日期。时区规则在查询时取当前的 `LocalZone`，过去的日子随之重算。有窗口查询被关注时，定时器在下一个本地零点安排一次重算（睡眠与改时间的处理见「数据流与线程」）。

### 规则对照

`crates/usage/AGENTS.md`「边界」的每条规则落在哪个观察、哪个判定和哪组测试。测试前缀见「测试」，步骤见「验收」。

| 规则 | 观察 / 输入 | 判定 | 测试 | 步骤 |
|---|---|---|---|---|
| 按 `message.id` + `requestId` 去重、sidechain、跨文件只计一次、子 agent 计入父会话 | `usage_source` | 「规范化与去重」 | `dedup_claude_*` | 1 |
| Codex 子 agent 只计 `task_started` 之后、只用一路用量、`.zst` | `usage_source` 的 `inherited`、`superseded` | 同上 | `dedup_codex_*` | 1 |
| 输入、输出、缓存读取、缓存写入互不重叠 | 入库时规范化 | `tokens::normalize` | `normalize_*` | 1 |
| ccusage `--json` 只在测试里对照 | 合成记录 | 每日合计 | `ccusage_*` | 1 |
| 自己的数据库、应用数据目录、页缓存 ≤ 2 MB、记录与游标同一事务 | `usage.db` | 「数据库」 | `store_*` | 1 |
| 时间窗口按记录时间戳、本地时区自然日 | `fact` 的时刻 | `window::days` | `window_*` | 1 |
| 最近一年为热数据，更早的压缩为冷数据保留，不提供手动清除（人的决定） | `cold_part`、汇总表 | 「冷热分层」 | `archive_*` | 1 |
| 分摊按分数保存、最大余数法、各行相加等于合计 | `share`、`fact` | `round` | `round_*` | 1 |
| Codex 额度来自日志，Claude Code 额度来自 statusline 的 JSON | `limit_sample` | 「外部格式」 | `limits_*`、`statusline_*` | 2 |
| 公共目录识别、固定 ID 写进观察、同一仓库的 worktree 是一个项目 | `location`、`project` | 「项目标识」 | `identity_location_*`、`identity_worktrees_*` | 3 |
| 重新打开时比对根提交与完整标识、仓库 ID 优先、取不到 ID 时临时项目 | `project_fact` | `identity::identify` | `identity_reopen_*`、`identity_forge_pending_*` | 3 |
| 主远端的选定、沿用与重选；fork 的 `upstream` 不让集合误合并 | `project_fact` | 同上 | `identity_primary_remote_*`、`identity_fork_*` | 3 |
| 改写历史、路径被占用、本地改写待确认、还没有提交 | `project_fact` | 同上 | `identity_rewritten_*`、`identity_path_reused_*`、`identity_local_rewrite_*`、`identity_unborn_*` | 3 |
| 移动与改名的别名、手动合并与撤销 | `alias` | 「项目标识」 | `identity_moved_*`、`identity_merge_undo_*` | 3 |
| 文件夹项目以根目录为准、据 cwd 认领以前的用量 | `location` | 汇总时认领 | `identity_folder_*` | 3 |
| 「其他」与之后打开时自动归入 | `project` 的状态 | 汇总 | `other_*` | 3 |
| 有改动的轮次按改动所在项目、跨项目按权重 | `item` | 分摊规则 1 | `project_by_change_*` | 4 |
| 没有改动的轮次跟随最近的有改动轮次（项目和分支一起，先之前再之后） | `share` | 规则 4 | `follow_*` | 4 |
| 会话里没有改动时按 cwd | `turn` 的 cwd | 规则 6、7 | `project_by_cwd_*` | 4 |
| 子 agent 先按自己的改动，再计入父会话；只派出子 agent 的父轮次 | `session` 的父子关系、`item` | 规则 1、5 | `subagent_*` | 4 |
| 行数口径（ADR 0047） | `item` | 「轮次的观察与改动项」 | `items_*` | 4 |
| 工具调用之后按结果时刻从 HEAD reflog 还原分支，不读解析时的状态 | `reflog`、`call_obs` | 「分支观察」 | `observe_*` | 5 |
| 用亚秒时刻和序号排序，排不出先后为不确定，跨过开始的单条切换计入 | `reflog`、`run` | 同上 | `order_*` | 5 |
| 一次调用里多次切换：提交按分支、未提交归结束时的分支 | `item` 的去向 | 「轮次的观察与改动项」 | `multi_switch_*` | 5 |
| 有改动的轮次按改动所在 worktree 的分支，多个分支按「行数 + 文件数」，跨 worktree 改动 | `item` | 规则 1 | `branch_by_change_*`（含 `branch_by_change_cross_worktree`） | 5 |
| 进入 worktree 的例外（`cd`、`pushd`、`git -C`、读与搜索的目标、没留下改动的写），平均分摊、优先于跟随 | `call` 的进入事实 | 规则 2 | `enter_*` | 5 |
| cwd 的 worktree 换了分支、最近的改动在这个 worktree | `call_obs` | 规则 3 | `follow_switched_*` | 5 |
| 没有改动的会话按每轮观察到的 cwd 分支、调用期间停留过的分支平均分摊 | `call_obs`、`turn` | 规则 6 | `cwd_branch_*` | 5 |
| 导入的旧会话用日志记录的分支，确定不了时「无法归属」；分支已删除时用名称片段 | `turn` 的日志分支、名称片段 | 规则 6、7 | `imported_*`（含 `imported_deleted_branch`、`imported_pruned_reflog`） | 5 |
| 观察写下之后不再修改，重放时按调用 ID 沿用 | `call_fact`、`call_obs`、`turn_obs`、`item_state` | 「轮次的观察与改动项」 | `reset_keeps_observations`、`finalize_*` | 4、5 |
| 观察里写固定的片段标识，改名沿用 | `segment`、`segment_name` | 「分支观察」 | `segment_*` | 5 |
| 一轮结束后写入 cwd、改动路径、「文件 × 分支」的行数、是否为默认分支；重建以记录为准 | `item` | 定稿 | `rebuild_*` | 5 |
| 删除重建、丢弃独有提交的重置、回到起点开始新的一次使用；提交关系与改写的证据；stash 只是挂起 | `boundary_obs`、`worktree_obs`、`stash_obs` | `uses` | `use_boundary_*`、`use_stash_suspend_*` | 6 |
| 同名分支复用 | `segment` | `uses` | `use_same_name_reuse` | 6 |
| 默认分支、detached HEAD、无法确定分支归「不属于任何 MR」 | `item` 的默认分支标记 | 汇总 | `rows_unassigned_*` | 6 |
| `switch -c` 带进新分支；切到已有分支带过去的未提交改动 | `worktree_obs`、`transfer` | 「工作转入新分支」 | `transfer_switch_*` | 6 |
| stash 恢复到另一个分支：改动块、证据、5 秒、文件监听、`stash@{n}` 的解析、候选恰好一条 | `stash_obs`、`transfer` | `stash::restored` | `stash_*` | 6 |
| cherry-pick 按命令的 OID 或 `patch-id`，原提交不属于 MR 时转移 | `call`、`reflog`、`transfer` | 同上 | `transfer_cherry_pick_*` | 6 |
| detached 推到 MR 源分支（远端跟踪分支在调用期间更新） | `call_obs`（推送观察）、`transfer` | 同上 | `transfer_push_*` | 6 |
| `forge` 证明边界时开着 MR、关闭后在边界之后重新打开、仍对应开着的 MR 时合并片段 | `boundary_obs`、MR 数据 | `uses` | `use_forge_*` | 7 |
| MR 区间的起点、终点、开 MR 之前的工作、关闭期间并入 | MR 时间线 | `mr_intervals` | `mr_interval_*` | 7 |
| `forge` 有对应 MR 时优先于默认分支 | MR 数据 | 汇总 | `mr_default_branch_*` | 7 |
| 共用分支时一个关闭后只计入开着的；区间重叠的合为一行、不重叠的各占一行、区间外为未开 MR 的分支 | MR 数据 | `mr_intervals`、汇总 | `mr_shared_*`、`rows_mr_*` | 7 |
| detached 提交成为唯一一个 MR head 的一部分时转移 | `MrSource::mrs_containing` | 「工作转入新分支」 | `transfer_forge_head_*` | 7 |
| 会话和轮次是 MR 之下的明细，分摊到几个 MR 时各显示分到的那一份 | `fact` | `MrUsage` | `detail_*` | 7 |
| 计量的状态、重启时的结算、按服务商规范化 | `analysis`、`meter` | 「分摊」的计量 | `meter_*` | 8 |
| 审阅按改动权重分摊、不受 MR 区间限制、简报发起时解析 MR（含已关闭的） | `analysis` | 「分摊」的审阅 | `review_usage_*` | 8 |
| 普通文件夹没有分支和 MR 明细，按 agent 和模型的明细照常 | `fact` | 汇总 | `folder_*` | 1、4 |
| 所有项目的列表里有「其他」一行 | `fact` | 汇总 | `rows_other_*` | 1 |

## 数据流与线程

```
SessionsChanged ─▶ [Background] read(游标, 8 MiB) ─▶ 规范化、去重、call、turn、usage_source、游标：一个事务
                 ─▶ 定稿的轮次：turns_unretained + load ─▶ item、share、fact：一个事务 ─▶ 受影响的快照
RepoBatch ─▶ [Background] reflog、reflog_reset、watch_span、head_baseline、segment、boundary_obs、worktree_obs、stash_obs
定时器 ─▶ 结果后 2 秒：call_obs；L 后 2 秒：turn_obs；定稿等到最后的结果或 L 加 2 秒；补读的节奏；本地零点
ProjectEvent::Opened ─▶ [Background] RepoFacts ─▶ identify ─▶ project、alias ─▶ watch_repo
watch(query) ─▶ [Background] SQL 按桶求和 + 内存里的映射（别名、uses、MR 区间）─▶ 取整 ─▶ 快照 ─▶ UsageEvent
command ─▶ [Background] alias ─▶ 重算受影响的快照
meter / begin_analysis ─▶ [调用方的 executor 任务] 一个事务
```

- 首帧之前不做任何事：插件 `AfterFirstFrame` 激活，`activate` 只建空表、provide 服务；`started` 里提交打开数据库的任务（`BusyGuard`），之后订阅 `SessionsChanged`、`ProjectEvent`、`Quiescent` / `Busy`，为已打开的 git 项目 `watch_repo`，`watch_dir` statusline 目录。数据库打开之前 `snapshot` 为 None，`limits` 为空，`meter` 返回 `Unavailable`。数据库打不开（数据目录建不出来、文件打不开）时，窗口查询的快照没有用量、状态为 `Degraded(Unwritable)`，打开的项目的 ID 暂为全零。
- 读取的节奏（agents 设计「接口」对消费方的约束）：每次 `read` 的 `max_bytes` 为 8 MiB，一批的读取、入库和定稿持有 `BusyGuard`，结果提交后才释放。活跃会话（`open_turn` 为 true，或 `last_activity` 在 10 分钟之内）一批接一批读完。其余会话的补读按 `last_activity` 从新到旧，每个会话（活跃的也一样）第一次读到末尾之后才分批定稿之前的轮次（「补读的定稿」），批与批之间不持有守卫、不预先排队：收到 `Quiescent` 后由定时器等 10 秒，期间收到 `Busy` 就重新计时，到时最近一次收到的仍是同一个 `Quiescent`（`seq` 相同，之后没有 `Busy`；插件拿不到 `Kernel::quiescent_epoch`，按事件的 `seq` 判断，`seq` 不大于已见最大值的事件忽略）才提交下一批。到时另一批还在读、这一次用不上的，不留到之后，等下一段静止重新计时。
- 仓库监听：已打开的 git 项目各一个 `RepoWatch`；活跃会话的 cwd 或改动所在、但项目没有打开的仓库也各一个，最多 16 个，会话不再活跃 10 分钟后释放。`WorktreeWatch` 只为「有候选 stash、且有跟踪中会话」的 worktree 持有，候选消失后释放；同一 worktree 已被 `project` 监听时 fs_watch 共用那一份状态。
- 定时器：每个截止时刻是交给 kernel 的 `Clock::wake_at` 的一个回调（`SystemClock` 的定时线程按需启动、空闲 1 秒后退出，与 kernel 的交互让路共用），回调只改内存里的状态、需要时提交 `Background` 任务，自己不做 IO、不阻塞，panic 被接住、记 error。这样本 crate 不开自己的线程，测试用 `testing::ManualTime`（kernel 的 `ManualClock`）推进同一份代码，回调在推进时钟的线程上执行。
- 按墙上时间的截止时刻（本地零点）：单调时钟在 Mac 睡眠时不走，系统时间也可能被改，所以每次最多等 15 分钟（`WALL_CHECK`），醒来时比较墙上时钟，没到就再等。睡过零点时最晚 15 分钟后重算。
- 启动时要读哪些会话：会话表里记着最后一批读到末尾时、读之前日志文件的大小、修改时间与 inode（`end_stamp`）。打开数据库后和每次 `SessionsChanged` 之后，变化的会话先在一个后台任务里比较当前文件与记下的值（一次查询加每个会话一次 `stat`），相同的不读，不同的（或从没读到过末尾的）才进入待读集合；不活跃的计入补读进度。Lectern 没运行期间的变化也由这个比较发现，`agents` 第一次发现时把全部会话列为变化，同样经过比较。适配器版本只有读一次才知道（游标里的版本与当前的不同时 `read` 给出 `reset`），所以每个来源在一次运行里另读一次最新的未变会话（不按补读的节奏，通常读出 0 字节）；它回放时，这个来源的其余会话都进入补读；一个来源在一次运行里只这样排一次（之后每个旧游标的会话读到时也会回放，不再重新排队）。
- 每个关注的查询一个快照对象，放在 `Mutex` 里，只在内存操作时持锁；快照整体替换为新的 `Arc`。新的事实行到达时，受影响的快照按增量（加上新事实行、减去被替换的临时事实行）更新；别名、`uses`、MR 数据变化时整体重算。同一个查询同时最多一个计算任务，期间的变化合并到下一轮。增量的做法：窗口查询（`Project`、`AllProjects`）保留快照的累加器（按 agent、模型、天、行的百万分之一 token 和事实行数）；入库事务用 `RETURNING` 收集它插入和删除的事实行，并把 `meta.commits` 加一。整体计算在一个读事务里同时读出求和与 `commits`，之后只应用紧接着的下一个事务的增量（序号不大于已有的不变，跳过了序号的整体重算；读取任务应用完增量才放开下一批读取）；增量落在整体计算时还没有的位置（可能是打开的文件夹认领的新目录）、或改了已有事实行的 agent（抢救之后）时整体重算；计算任务进行中到达的增量不应用，这个查询再算一次。一批入库后，读取任务当场应用增量、用累加器重新取整、替换快照并发出 `Changed`，不另起计算任务。`History` 每次整体计算。
- 读连接用于快照的查询，写连接用于入库、定稿、命令和计量；两者都只在 executor 上。`meter` 在调用方（agents 的转发端点）的 executor 任务里同步提交。
- UI 线程上只调用 `snapshot`、`limits`（复制 `Arc`）和立即返回的 `watch`、`command`、`register_mr_source`，不做 IO、不调用 SQLite。
- 派生数据重建（派生算法版本变化、数据库恢复）：按会话分批，每批 ≤ 50 ms，节奏与补读相同；重建期间快照为 `Backfilling`，旧的派生数据在一批重建完成前照常可见。只重建热窗口，完整重建另外逐月处理冷数据段（「冷热分层」）。
- 冷归档：每段一个事务，节奏与补读相同，不在 UI 线程上；补读、重建和归档共用同一个后台队列，同时只有一个在运行，有会话要读时先读。归档写了段之后 `History` 的关注重算。
- 读取期间的抢救：读取任务在事务之外取游标、读一批，抢救可能在这之间把游标改回 `START`。入库事务里先核对保存的游标仍是这一批读取时的游标，不同就丢弃这一批（不写任何行），从保存的游标重读；是否还有等待重读的 `repaired` 来源也在事务里查（部分索引），不缓存在内存里。
- 架构规则 5：本 crate 没有以内容为键的派生缓存。`tree_changes`、`patch_id` 的结果只在算观察时用一次，结果写进观察。
- 补丁 ID 的版本：`Repo::patch_id` 的值取决于 `git` 的 diff 实现（算法、二进制文件的处理），不同版本的 Lectern 算出的值可能不同。写进观察的补丁 ID 带 `git::PatchId::VERSION`，比较时只比较同一版本的值；版本不同时按需重新计算两边，算不出时按没有补丁 ID 处理（不转移，不会误合并）。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)、[ADR 0017](../adr/0017.md)、[ADR 0027](../adr/0027.md)、[ADR 0035](../adr/0035.md)）：

| 预算 | 本 crate 的份额 |
|---|---|
| 冷启动 20 ms | 首帧之前不做任何事；激活（`plugin/usage/activate`）< 1 ms，只建空表 |
| 首帧后 30 秒内静止 | 启动时只有打开数据库（< 10 ms）和活跃会话的读取；补读按「数据流与线程」的节奏，批与批之间留出完整的采样窗口 |
| SQLite 页缓存 8 MB（ADR 0017、ADR 0027） | 两个连接各 1 MiB，合计 ≤ 2 MiB；`cache` 的 6 MiB 不变 |
| 空闲内存：空窗口 < 60 MB | 页缓存 ≤ 2 MiB 加本 crate 的常驻 ≤ 2 MB（驻留表、会话与片段的索引、额度、没有关注者时没有快照），`usage/resident_bytes` |
| 空闲内存：打开 monorepo / 10 万文件（ADR 0035 的余量） | 每个打开的项目 < 0.5 MB（侧栏的 7 天快照、片段表）；`RepoWatch` 的状态归 `fs_watch` 的份额。从 ADR 0035 的余量里分配，review 之后的 7 MB 余量剩 6.5 MB |
| UI 线程不做 IO | `snapshot`、`limits` 只复制 `Arc` |
| 磁盘 | 「重度一天」（50 个会话、2000 轮、6000 条用量、2 万次工具调用）约 1 MB 的热数据；只保存解析出的事实和提到 git 的命令原文，读文件和搜索只存 worktree。热窗口 365 到 396 天，重度使用约 400 MB，一般使用约为它的五分之一。冷数据是按列编码的整数和驻留 ID，按 4:1 的压缩比估算（实测由 `usage/archive_month` 记录），每年重度使用约 100 MB；汇总表每年 < 1 MB。数据全部保留，不提供手动清除（「冷热分层」）；空闲页的回收规则同上。迁移之前的副本（「数据库」，最多两份）在迁移时临时多占一到两倍数据库的大小。第 1 步实测约为这个估算的 2.4 倍，见「未决问题」 |

`bench`（[#7](https://github.com/SoloJiang/lectern-app/issues/7)）新增，夹具由 `testing::heavy_days` 和 `testing::Scenario` 生成，`git` 的 `TestRepo` 写仓库：

| ID | 统计量与预算 |
|---|---|
| `usage/ingest_append_4k` | 中位数 < 5 ms（活跃会话追加约 4 KiB：`read`、规范化、去重、落盘与提交游标） |
| `usage/backfill_batch` | 中位数 < 150 ms（一批 8 MiB 的合成会话：读取、入库；补读会话的定稿在读到末尾之后另分批，由 `usage/backfill_large_session` 覆盖） |
| `usage/backfill_large_session` | 每批最大值 < 300 ms、中位数 < 150 ms（一个 200 MB 的合成会话按 8 MiB 分批补读并定稿）；适配器解析的字节合计 ≤ 文件大小的 1.2 倍（检查没有平方级的重复解析） |
| `usage/finalize_turn_20` | 中位数 < 10 ms（一轮 20 次改动调用、3 个 worktree，`turns_unretained` 命中，`load` 20 次） |
| `usage/project_30d` | 中位数 < 50 ms（30 个重度日、约 20 万条事实行、40 个片段：一个项目的 30 天快照，后台） |
| `usage/all_projects_30d` | 中位数 < 80 ms（同一夹具，20 个项目） |
| `usage/snapshot_increment` | 中位数 < 2 ms（关注着 30 天快照时一轮定稿到快照替换） |
| `usage/rebuild_30d` | 只记录（同一夹具的派生表从观察整体重建） |
| `usage/resident_bytes` | 存活字节 ≤ 2 MB（同一夹具打开数据库、补读完成并静止后，不含 SQLite 页缓存；`Memory`） |
| `usage/disk_day_heavy` | 只记录（一个重度日写入后数据库文件的增长，含 `-wal`；乘以 396 天是热数据的上限估算，对照「磁盘」一行的 400 MB） |
| `usage/archive_month` | 每段中位数 < 150 ms（一个重度月约 30 MB 的热行归档：编码、压缩、解码核对、写段、删热行、提交）；压缩比只记录，低于 3:1 时重新评估「磁盘」一行 |
| `usage/cold_read_month` | 中位数 < 1 s（解压一个重度月的 `fact` 段并按日聚合，`History` 的按日明细） |

页缓存的上限由测试检查（`store_page_cache_is_bounded`：两个连接的 `PRAGMA cache_size` 都是 -1024，`mmap_size` 为 0）。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)，视图的处理由 `usage_view` 的设计规定：

| 情况 | 本 crate 的结果 |
|---|---|
| 数据库还没打开、第一次补读进行中 | `Loading`，之后 `Backfilling { done, total }`，已入库的部分照常显示 |
| 没有任何会话 | `Ready`，合计为 0 |
| 数据库损坏 | 先 `REINDEX`；不行时把读得出的行复制进新文件、完成后替换，原文件留作 `usage.db.corrupt-<毫秒>`，其余从现存日志重读，`Degraded(Recovered)`；抢救中断时下次打开重来 |
| 数据库打不开（数据目录建不出来） | 窗口查询的快照没有用量，`Degraded(Unwritable)` |
| 迁移之前的副本写不出来 | 不迁移，旧版本只读，`Degraded(Unwritable)` |
| 数据库是更新的版本 | 只读，`Degraded(NewerDatabase)`；`meter` 返回 `Unavailable` |
| 写不进（磁盘满、目录不可写） | 游标不前进，下一次 `SessionsChanged` 重试；`Degraded(Unwritable)`；`meter` 返回 `Io`，Analyze 不转发 |
| 会话日志被删除（`NotFound`） | 已入库的观察和用量不受影响 |
| 会话日志重放（`Batch::reset`） | 旧一代照常计数，回放读完时换成新一代 |
| 继承边界未定的会话 | 照常计数，`may_include_inherited` 为 true |
| 仓库没有 reflog、不在监听里 | 轮次按导入处理：日志记录的分支，确定不了时「无法归属」 |
| reflog 读不了（`ReflogReset::Unreadable`） | 记下缺口，缺口里的调用为「不确定」 |
| statusline JSON 缺失、坏了、超过 1 MiB | Claude Code 的额度保留上一次的样本，计数进日志 |
| `MrSource` 没有登记、离线 | 按分支显示（P2 的行为） |
| 路径不是 UTF-8 | 位置和相对路径按字节保存，显示由视图替换无效字节 |

## 日志与诊断

日志门面、级别和文件布局按 [ADR 0046](../adr/0046.md) 与 [logging 设计](logging.md)，具体的宏和字段写法以 logging 设计为准。本 crate 只规定记什么。

- 默认级别（warn、error 和关键的 info）：
  - error：数据库损坏与恢复（保留的原文件名、复制与丢失的行数、补全引用的行数、重读的会话数）、迁移失败、写不进（附原因）、`meter` 在 `Prepared` 时落盘失败。
  - warn：上次的抢救被中断（打开时发现 `usage.db.salvage`）、目录的 `fsync` 被文件系统拒绝（附 IO 错误）、statusline JSON 读不了或认不出、`Batch::reset` 回放、reflog 的 `Unreadable` 缺口、`MrSource` 返回的错误（离线只记第一次）、命令文本里无法解析的片段让一次调用为「不确定」。
  - 冷热分层：error 记冷数据段的解码核对失败（UTC 月）、解码不了的段（抢救时的段数、`History` 读到的段 ID、完整重建时的月份）；warn 记归档计划、归档一步、空间回收的失败（SQLite 扩展错误码）；info 记有月份要归档（月数）、一个月归档完、之后到达的旧记录在归档时计入（月份、键数）、回放时因已有冷数据没有写入的记录数、完整重建放回的月数。
  - info：数据库打开与版本、补读开始与完成（字节数、会话数、耗时）、适配器版本变化让一个来源的会话全部重读、派生表重建、项目识别的结果（新项目、别名、临时项目及其原因）、用户的合并与撤销、轮次因「不确定」或「无法归属」进入「不属于任何 MR」、重启时计量的结算（结算与作废的请求数）。
- 详细级别（用户打开时）：每一轮定稿时的分摊决定（用了哪条规则、目标与权重）、每次 `call_obs` 用到的 reflog 记录（文件、偏移、时刻区间）与结果、片段的新建与边界、`uses` 与 MR 区间的计算结果、每批读取的游标前后位置、读取期间遇到抢救而丢弃的一批（游标偏移）、每写一段冷数据（月份、表、行数、编码与压缩后的字节数）、空间回收的每一批（页数）。
- 关联用的标识：项目 ID（`UsageProjectId` 的十六进制）、来源 ID 与会话 ID、`TurnId`、调用 ID、片段 ID、分析 ID 与请求序号；仓库与 worktree 写规范路径。
- 不得记录：prompt、agent 说明、工具输入与输出、文件内容和改动内容、命令原文（只记解析出的事实）、statusline JSON 的原文，以及任何凭据和访问令牌。路径可以记录；token 计数是用量数据，可以记录。

## 测试

全部在 Linux 上运行（`just test -p lectern-usage`），不依赖真实的 agent、会话目录、代码平台、网络和登录状态。本 crate 不调用外部命令行，不需要 PATH 上的假命令。

- fixtures（`crates/usage/tests/fixtures/`）全部是合成数据，不取自本机的会话日志、Claude Code 或 Codex 的配置目录：
  - `records/`：`services::session` 的 serde 形式写的 `Record` 序列，按 [agents 设计「外部格式」](agents.md#外部格式)、ADR 0014 和 `crates/agents/tests/fixtures/` 里已有的合成样本描述的形状手工构造（同一响应多行同一份 usage、sidechain 重放、跨文件的同一个键、Codex 两路用量的切换、子 agent 的继承前缀、`info` 为 null 的 `token_count`）。不依赖 `agents`（插件之间不得有 dev-dependency）；日志到这些类型的映射已由 agents 的测试覆盖。
  - `statusline/`：按第 2 步核对过的公开文档写的 JSON，含缺少窗口、多余字段、超过 1 MiB 的样本；`SOURCE.md` 写明文档地址和核对日期。
  - `ccusage/`：对 `records/` 的合成记录手写的、ccusage `--json` 日汇总形状的期望值。
  - 仓库：`lectern-git` 的 `TestRepo` 在临时目录里写提交、分支、stash、worktree 和 reflog；reflog 的监听时刻由 `fs_watch` 的 `FakeBackend` 和假时钟给出，或由 `testing::Scenario` 直接注入 `RepoBatch`。
  - 场景：`testing::Scenario` 的构造在 `tests/scenarios/` 里，每条归属规则一个或几个场景，名字与「规则对照」的测试前缀相同。
- 场景（前缀即测试名的前缀；结构化输出——去重结果、改动项、份额、片段到使用的映射、MR 区间、快照——都用 insta 快照验收）：
  - `normalize_*`、`dedup_claude_*`、`dedup_codex_*`、`ccusage_*`、`reset_*`（回放期间旧一代可见、读完时替换、同一个键在另一个会话里仍计数）。
  - `store_*`：记录与游标同一事务（在两者之间注入失败，重启后不重复也不遗漏）；版本迁移不删观察，迁移之前写出副本、副本写不出来时不迁移；更新的版本只读；只坏了索引时 `REINDEX` 之后照常使用；表页损坏时读得出的行（含冷数据段、项目、别名、计量）都在新库里，丢失的会话从 `START` 重读，原文件改名保留；抢救在每一步之前中断时 `usage.db` 仍是原文件，下次打开重新抢救；驻留表的第一页或最后一页、会话表或位置表的最后一页丢失时，合计不变、没有指向不存在的行的引用，之后新建的行拿到丢失的 ID 也不会被旧行引用，日志重读后计数的来源回到原来的会话、位置、模型和 agent；`store_page_cache_is_bounded`。
  - `finalize_*`：`TurnEnded` 与最后一个 `ToolFinished` 在同一批里到达时，定稿等到结果时刻加 2 秒，改动项有片段、不是「无法归属」；Claude Code 轮次的最后时刻是这一轮最后一条记录，不是下一个 `TurnStarted`；补读的会话和还在追赶的活跃会话在第一次读到末尾之前都不定稿，读到末尾后分批定稿、`turns_unretained` 不再解析（`FakeSessions` 计数）。
  - `reset_keeps_observations`：适配器升级引起的重放之后，worktree 已删除、默认分支已改名，旧调用的位置、片段、默认分支标记和去向与重放前相同；新出现的调用按当时的状态计算。
  - `archive_*`：截止点按本地时区计算（`TableZone` 的东 14 区、西 12 区，以及截止点正好落在夏令时切换日），只有整个早于「截止点减 1 天」的 UTC 月被归档；跨过截止点的活跃会话和 MR 按记录的时间戳拆开，热的一部分加 `cold_total` 等于归档前的合计，`Worktree`、`Session`、`Mr` 快照与归档前相同，7D / 14D / 30D 的快照不读 `cold_part`（读取计数为 0）；在写段、更新汇总、删除热行之间的每一处注入崩溃，重启后热行和冷数据段合起来与归档前逐行相同，再跑一次不重复归档；解码核对失败时热行不删；冷数据段解压后重算的按月与按日合计与归档前的事实行相同；重放的会话不把已归档月份的记录再写进热表；之后到达的旧记录不写热的事实行，下一轮归档为这个月追加新段；`archive_half_month_rebuild`：一个月归档到只剩 `share`、`usage_value`、`fact` 还在热表时退出，重启后先跑热窗口重建、去重的重新选出和受影响轮次的重算，再接着归档，全程每个快照和最终的冷热合计都与归档前相同，冷数据段里没有重复的事实行；完整重建与只重建热窗口的结果都与重建前相同；空闲页超过阈值时才回收，每批不超过 2048 页。
  - `window_*`：`TableZone` 的夏令时切换日是 23 / 25 小时；+05:45 的时区；跨零点的记录归到正确的日期；`round_*`：和不变、余数相同按下标、拆分后精确相加。
  - `limits_*`、`statusline_*`：Codex 的样本只留最新；statusline 文件替换后更新；过期标记；`tightest` 的选择。
  - `identity_*`：「项目标识」判定表每一行一个用例，加上主远端的选定与重选、地址的统一、别名重放与撤销、文件夹的最深根认领。
  - `project_by_change_*`、`follow_*`、`project_by_cwd_*`、`other_*`、`subagent_*`、`items_*`（每种 `EditOp` 的行数；路径前缀替换；行数为 0 的写不算改动）。
  - `observe_*`、`order_*`、`multi_switch_*`、`branch_by_change_*`、`enter_*`、`follow_switched_*`、`cwd_branch_*`（Claude Code 的轮次按最后一条记录时刻的 `turn_obs`）、`imported_*`（含 `imported_deleted_branch`：合并后删掉的分支没有 reflog，导入轮次按日志的分支名进名称片段，P3 的场景里经 `mrs_for_branch` 归到那个 MR；`imported_pruned_reflog`：现存分支的创建记录已被清掉，早于最早一条 reflog 记录的导入轮次落在它向过去开放的第一个片段上，`Worktree` 的累计和分支行含这段历史，P3 里同一个 MR 只出现在一次使用里；现存同名分支创建晚于导入轮次时才用名称片段）、`segment_*`、`rebuild_*`（删掉全部派生表后重建，快照与之前相同）；`enter_*` 里读文件、搜索的目标 worktree 有自己的 `call_obs`。
  - `use_boundary_*`、`use_stash_suspend_*`、`use_same_name_reuse`、`rows_unassigned_*`、`transfer_*`、`stash_*`（改动块认定的每一条：目标 HEAD 已有一部分、三方合并后的结果、二进制与模式、`-u` 的第三个父提交、命令证据与 5 秒的 reflog 证据、工作区本来就有同样改动时不认定、`stash@{n}` 按执行前的 reflog 解析、候选不唯一时不认定、没有监听的 worktree 不认定）。
  - `use_forge_*`、`mr_interval_*`、`mr_default_branch_*`、`mr_shared_*`、`rows_mr_*`、`detail_*`、`transfer_forge_head_*`：`FakeMrSource`。
  - `meter_*`、`review_usage_*`：每个状态序列；重启时 `Accepted` 按上限结算、其余作废；`Rejected` 记 0；审阅的 MR 在发起时确定。
  - `folder_*`、`rows_other_*`。
  - `log_*`：场景里的 prompt、命令原文、改动内容和 statusline JSON 带一个哨兵字符串，打开详细级别跑完全部场景，捕获的日志里没有它（捕获方式按 logging 的设计）。
  - `plugin_*`：`TestKernel` 激活后提供 `dyn Usage`；首帧之前不打开数据库；补读的批与批之间 kernel 能静止，静止 10 秒后才有下一批；活跃会话不受节奏限制；重启后日志没变的会话不重读、直接 `Ready`，停止期间变长的才补读；适配器版本变化后没变的会话也回放；Mac 睡过本地零点（只拨墙上时钟）后最晚 15 分钟窗口快照重算；数据库打不开时快照为 `Degraded(Unwritable)`；`snapshot` 在 UI 线程上调用时不做 IO（数据目录的访问计数为 0）；`register_mr_source` 的登记 drop 或 token 失效后不再调用提供者。
  - `services` 的 `usage` 模块（前置条件 A）：类型的 JSON 往返，服务 ID，注销回调。
- 只能在 macOS 上运行的：无。时区的系统实现由 `platform` 的 `utc_offset_*` 在两个平台上测。

## 未决问题

1. **过了重置时刻的额度。** statusline 只在 Claude Code 运行时更新，Codex 的额度只在有会话时写入日志，最后一个样本可能早已过了 `resets_at`。选项：
   - (a) 标为 `expired`，额度卡片显示「已重置，等待新数据」，不参与 `tightest`（倾向）。
   - (b) 按 0% 显示。
   - (c) 照常显示最后一个样本的百分比和采样时间。

   默认按 (a) 实现，文案由 `usage_view` 的设计确认。

2. **热数据的磁盘占用超出估算。** 「性能」的「磁盘」一行按一个重度日约 1 MB 估算，重度使用的热数据约 400 MB。第 1 步的实测（Linux，`testing::heavy_days`）：30 个重度日的数据库（含 `-wal`）76 MB，约 2.4 MB/天，乘以 396 天约 950 MB；这时还只有 `usage_source`、`turn`、`fact` 和 `usage_value`，第 2–8 步的调用、改动项、份额和 reflog 还要再加。`usage/disk_day_heavy`（从空库写一个重度日）是 5.5 MB，其中 4.2 MB 是还没检查点的 `-wal`，不能直接乘以 396。主要的空间是用量记录的键（约 60 字节的字节串）存了三份：`usage_source` 的行、它的键索引和 `usage_value`。选项：
   - (a) 接受约 1 GB 的上限，改写「磁盘」一行（数据全部保留的决定不变）。
   - (b) 由负责推进的 agent 在之后的步骤里缩小：键只在 `usage_source` 里存原文，索引和 `usage_value` 改存 16 字节的哈希，再看能否去掉别的索引（倾向，目标回到每个重度日 1 MB 左右，实测后再定）。
   - (c) 缩短热窗口（与「用量数据的保留」的决定冲突，不倾向）。

   在人确认之前按 (b) 推进，「磁盘」一行暂不改。

已由人决定的问题：

- **用量数据的保留**（2026-10-22，「用量数据按年维度吧」「超过一年的可以压缩作为冷数据」）：最近一年为热数据，更早的压缩为冷数据保留，不提供手动清除。按「冷热分层」实现。

技术问题都已在上文决定。

## 验收

前置条件（其他 crate 的 PR，写 `Refs #31`）：

- A. `services`：`usage` 模块；`rg -n 'pub trait Usage\b|pub trait MrSource' crates/services/src` 各有一处；`usage_*` 测试通过；`crates/services/AGENTS.md` 列出这个模块（第 1 步之前）。
- B. `platform`：`time::utc_offset`；`utc_offset_*` 测试在 Linux CI 和 macOS CI 上通过（第 1 步之前）。
- C. `platform` 与 `app`：`app` 构造 `UsageDeps` 并把插件放进 `PluginSet`（第 1 步的插件部分之后）；statusline 脚本写 `<app_data>/usage/statusline/claude-code.json`（#32，第 2 步的端到端之前；第 2 步本身用 fixtures 验收）。
- D. `git`：`Repo::ref_reflog`、`Repo::patch_id`；`ref_reflog_*`、`patch_id_*` 测试通过（第 5 步之前）。已完成（[#178](https://github.com/SoloJiang/lectern-app/pull/178)）。
- E. 取消：推送观察按需读 `Repo::ref_reflog`（前置条件 D），不监听远端跟踪分支（见「对其他 crate 的要求」的 E）。
- F. `forge`：`MrSource` 的实现与登记（第 7 步之前）。
- G. `agents`：Analyze 调用 `begin_analysis`、`meter`（第 8 步之前）。

#31 的领域部分分八步，每步一个 PR，都写 `Refs #31`；#31 由最后完成的一个 PR 关闭（[AGENTS.md「工作方式」](../../AGENTS.md#工作方式)）。每一步都要 `just check`、`just test -p lectern-usage`、`just deny` 通过；新增的 bench 条目在这一步加进 `budgets.rs` 并改为生效。

1. **存储、去重与项目合计**（前置 A、B）：数据库、读取与游标、规范化与去重、按 cwd 的位置和项目合计（这一步每个轮次按 cwd 归属，`rows` 为空）、时间窗口、取整、冷热分层（这一步已有的表；之后每一步新增的带时间戳的表同时加进归档清单和 `archive_*` 用例）与 `History` 查询、`dyn Usage` 的 `Project` / `AllProjects` 查询、插件与读取节奏；`Cargo.toml` 的描述去掉 price table，加 `plugin = true`。
   - `normalize_*`、`dedup_claude_*`、`dedup_codex_*`、`ccusage_*`、`reset_*`、`store_*`、`window_*`、`round_*`、`archive_*`、`folder_*`、`rows_other_*`、`plugin_*` 通过，快照已逐个看过。
   - `usage/ingest_append_4k`、`usage/backfill_batch`、`usage/project_30d`、`usage/all_projects_30d`、`usage/snapshot_increment`、`usage/resident_bytes`、`usage/archive_month`、`usage/cold_read_month`、`usage/backfill_large_session` 在预算内，`usage/disk_day_heavy` 有结果；`plugin/usage/activate` < 1 ms。
   - `python3 scripts/check_architecture.py` 通过：本 crate 不依赖其他插件 crate、不依赖 GPUI；`rg -n 'rusqlite' crates/*/Cargo.toml` 只出现在 `cache` 和 `usage`。
   - `rg -n 'price|LiteLLM|cost' crates/usage/src crates/usage/Cargo.toml` 没有结果。
2. **额度快照**（前置 A）：Codex 的 `RateLimits`、statusline JSON（先按「外部格式」核对公开文档的字段名）、`limits()`、`UsageEvent::Limits`。
   - `limits_*`、`statusline_*` 通过；`tests/fixtures/statusline/SOURCE.md` 写明文档地址与核对日期。
3. **项目标识**：位置与项目 ID、`identity::identify`、别名与命令、文件夹认领、「其他」。
   - `identity_*`（`identity_forge_pending_*` 除外，它要第 7 步的 `MrSource`）、`other_*` 通过；判定表其余每一行都有用例。
4. **改动项与按改动的项目归属**：`call`、`turn` 的定稿、改动项（ADR 0047）、分摊规则 1、4、5、6、7 的项目部分、子 agent、`Session` 查询的每轮 token。
   - `items_*`、`project_by_change_*`、`follow_*`、`project_by_cwd_*`、`subagent_*`、`finalize_*`、`reset_keeps_observations` 通过。
   - `usage/finalize_turn_20` 在预算内。
5. **分支观察与分支归属**（前置 D）：`RepoWatch` 与 reflog 入库、`watch_span`、`call_obs`、片段、分摊规则 2、3、6 的分支部分、`Worktree` 与 `Session` 查询的分支行（P1 / P2 的「分支用量」）。
   - `observe_*`、`order_*`、`multi_switch_*`、`branch_by_change_*`、`enter_*`、`follow_switched_*`、`cwd_branch_*`、`imported_*`、`segment_*`、`rebuild_*` 通过；`usage/rebuild_30d` 有结果。
6. **一次使用与工作转入新分支**（前置 D）：边界观察、`uses`、`worktree_obs`、`stash_obs` 与 `stash::restored`、`transfer` 的五种规则（`forge` 的那一种除外）、「不属于任何 MR」的行。
   - `use_boundary_*`、`use_stash_suspend_*`、`use_same_name_reuse`、`rows_unassigned_*`、`transfer_*`（`transfer_forge_head_*` 除外）、`stash_*` 通过。
7. **MR 区间**（前置 F）：`register_mr_source`、项目标识里的仓库 ID、`uses` 的 `forge` 条件、`mr_intervals`、Usage 页的 MR 行、`Mr` 查询、detached 提交按 MR head 转移。
   - `use_forge_*`、`mr_interval_*`、`mr_default_branch_*`、`mr_shared_*`、`rows_mr_*`、`detail_*`、`transfer_forge_head_*`、`identity_forge_pending_*` 通过。
8. **审阅的计量**（前置 G）：`begin_analysis`、`meter`、重启时的结算、审阅的事实行。
   - `meter_*`、`review_usage_*` 通过。

## 实际验证

### 前置条件 A

`services` 的 `usage` 模块，在 Linux（x86_64 容器）上运行：

- `rg -n 'pub trait Usage\b|pub trait MrSource' crates/services/src` 各有一处（`usage/mod.rs`、`usage/mr.rs`）。
- `just test -p lectern-services`：84 个测试通过，新增 15 个，都以 `usage_` 开头。
  - JSON 往返：`usage_event_round_trips`、`usage_queries_and_commands_round_trip`、`usage_snapshots_round_trip`、`usage_limits_round_trip`、`usage_metering_round_trips`、`usage_mr_types_round_trip`。覆盖每个枚举的每个变体（六种快照、四种 `UsageState`、三种 `UsageDegraded`、四种 `ProjectStatus` 与 `TemporaryReason`、三种 `RowKey`、五种 `MeterState`、三种 `AnalysisTarget`、四种 `LimitKind`），以及 SHA-1 与 SHA-256 的 `BlobId`、`u64::MAX`、负的年份与时间戳、`f32` 的百分比。`usage_enums_are_externally_tagged` 检查枚举是 serde 默认的外部标记。
  - 服务 ID：`usage_service_id_and_multiple` 检查 `ID == "usage"`、`MULTIPLE == false`。
  - 不是服务：`usage_mr_source_is_not_a_service` 检查 `dyn MrSource` 没有实现 `Service`，同一个探针对 `dyn Usage` 给出 true。
  - 注销回调：`usage_interest_releases_once_on_drop`、`usage_mr_source_registration_releases_once_on_drop` 在另一个线程上 drop，回调各调用一次。
  - 其余：`usage_is_a_shared_trait_object`、`usage_mr_source_is_a_shared_trait_object` 检查两个 trait 能作为 `Arc<dyn …>` 跨线程使用，快照、事件可共享，关注与登记可 `Send`；`usage_tokens_total_sums_the_four_kinds` 检查 `total` 与溢出时的饱和。
- `just check`、`just test`（不依赖 GPUI 的全部 crate，2058 个测试通过，跳过 7 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。
- `Cargo.toml`、`Cargo.lock` 没有改动：只用已有的 `lectern-kernel`、`serde`（dev 的 `serde_json`）。
- 没有在 macOS 上运行：`services` 不依赖 GPUI，Linux 与 macOS 编译的代码相同。本步没有 bench 条目。

### 第 1 步（前一部分）

在 Linux（x86_64 容器）上运行：

- `just test -p lectern-usage`：56 个测试通过，按前缀：
  - `normalize_*` 4 个（Anthropic 的输入不含缓存；`cache_write` 为 0 时取 5m + 1h；OpenAI 减去缓存命中、推理已在输出里；缓存命中超过输入时截到输入）、`round_*` 5 个（和不变、每行偏差小于 1；余数相同按下标；余数大的先进位；`split` 精确相加并能取整回整数；全 0 权重平分、空权重、0 token、超大 token）、`window_*` 7 个（日序号与日期互换；窗口以今天结束、天与天首尾相接；纽约 2026 年夏令时切换日 23 / 25 小时；+05:45 的日界在 15 分钟桶的边界上；零点被跳过时从切换时刻开始、零点出现两次时从第一次开始；下一个本地零点；跨本地零点的两条记录分在两天、8 天前的记录在 14 天窗口里、不在 7 天窗口里）。
  - `dedup_claude_*` 4 个、`dedup_codex_*` 3 个、`ccusage_*` 1 个：`tests/fixtures/records/` 的合成记录（同一响应的多个内容块与重复行只计合计最大的一份；另一个会话文件里的同一个键只计一次；sidechain 重放让位于非 sidechain；子 agent 的用量计入它自己的会话；advisor 按自己的模型另计、重复行也只计一次；Codex 被取代的 `token_count` 不计、`token_usage_record` 全计；子 agent 继承的前缀不计；边界未定的分叉照常计、键仍只计一次、快照的 `may_include_inherited` 为 true；模型取 `Context` 的模型）。`ccusage_daily_totals_match` 与 `tests/fixtures/ccusage/claude.json` 手写的日汇总逐日相同。
  - `reset_*` 3 个：回放期间旧一代照常计数、读到末尾时换成新一代；被回放会话删掉的键由另一个会话里的同一个键接着计数；回放中途再次重置时丢弃未完成的一代。
  - `store_*` 12 个：数据库在数据目录下、旁边没有别的文件；`store_page_cache_is_bounded`（两个连接的 `cache_size` 都是 -1024、`mmap_size` 为 0、WAL、`synchronous = FULL`、外键开启，读连接 `query_only`）；在记录与游标之间注入失败，重启后不重复也不遗漏；迁移不删观察、迁移前写出副本、连续迁移只留两份副本；副本写不出来时不迁移、按旧版本只读；更新的版本只读、不改动文件，`repair` 也拒绝；只坏了索引页时 `REINDEX` 后照常使用、没有改名的文件；表的一整页损坏时读得出的行都在新库里，项目仍在，三个会话从 `START` 重读后合计与损坏前相同；`store_interrupted_salvage_runs_again`：在建表、复制、重建、同步、硬链接原文件、删掉原文件日志这六步之后各注入一次失败，`usage.db` 仍是损坏的原文件、`usage.db.salvage` 留着，下次打开删掉它重新抢救，重读后合计与损坏前相同；`store_salvage_keeps_rows_that_point_at_lost_rows`：`intern` 的第一页、`intern`、`session`、`location` 的最后一页分别损坏时，抢救后合计与损坏前相同、没有指向不存在的行的引用；100 个新会话先读（拿到丢失的 ID）、旧会话再重读，每条计数的来源的会话 ID、位置、模型和 agent 都与它的键对得上，计数的键是 400 个，合计等于损坏前加新会话；文件头损坏时在打开时抢救、会话重读后合计相同；写满（`max_page_count`）时游标不前进、空间恢复后照常写入。
  - `folder_*` 2 个、`rows_other_*` 2 个：文件夹项目没有 `rows`、按模型和 agent 的明细相加等于合计；文件夹打开之前的用量在「其他」，打开后按最深的根认领；没有打开过的仓库、不在项目里的目录、已不存在的 cwd 进「其他」，各行相加等于合计，按 ID 仍能查到没有打开过的仓库，打开后移出「其他」；不认识的项目 ID 没有快照。
  - `plugin_*` 12 个：激活后提供 `dyn Usage`，任务运行之前没有数据库文件、`snapshot` 为 None、`limits` 为空；补读等满 10 秒静止才读下一批，期间的一次忙碌让等待重新开始，旧的 `Busy` 被忽略，补读完成后状态为 `Ready`；活跃会话不受节奏限制、新记录随 `SessionsChanged` 读入；`snapshot` 和 `limits` 调用 100 次，数据库的访问计数不变，`UsageEvent::Changed` 已发出，关注 drop 后快照释放；`ProjectEvent::Opened` / `Closed` 更新所有项目的列表；本地零点到时窗口快照重算；写满时快照为 `Degraded(Unwritable)`、不反复重试，下一次 `SessionsChanged` 重试成功后为 `Ready`；停用时写入关闭标记。`plugin_restart_reads_only_changed_logs`：两个会话读完后重启，不推进时钟就是 `Ready`、合计不变，只有最新的会话被读一次（适配器版本的探测），全部会话再随 `SessionsChanged` 宣布一次、推进 30 秒也没有读取；停止期间一个日志变长，重启后为 `Backfilling`，10 秒静止后读入、回到 `Ready`。`plugin_adapter_upgrade_replays_unchanged_logs`：6 个不活跃的会话读完后，重启时适配器版本加一，探测读到 `reset` 后其余会话也回放，每个会话恰好多读一次（同一来源不重复排队；去掉这条检查时最多的一个多读 6 次），回到 `Ready`，合计不变。`plugin_midnight_is_seen_after_a_sleep`：墙上时钟越过零点、单调时钟不动时快照不变，推进 15 分钟后窗口移到新的一天。`plugin_unopenable_database_degrades_the_snapshots`：数据目录的位置是一个文件时，所有项目和项目快照都是 `Degraded(Unwritable)`、合计为 0，项目名来自 `Projects`。
  - `log_canary_never_reaches_a_record`：trace 级别跑完补读、回放和删除，捕获的记录里没有放在模型名、消息与请求 ID、`TurnId`、命令文本里的金丝雀。
- insta 快照 9 个（`src/tests/snapshots/`），逐个看过。
- `cargo nextest run -p lectern-usage --stress-count 15`（全部测试）与 `--stress-count 40`（`plugin_*`、`log_*`）都通过。压力运行发现过两次时序问题：测试推进时钟时插件可能还没登记补读的定时器，`Rig::settle` 改为等定时器登记之后才返回；收到 `Busy` 时清除补读许可会与刚到时的许可抢先后，改为许可用不上时当场作废（「与设计的差异」）。
- `just check`、`just test`（不依赖 GPUI 的全部 crate，2204 个测试通过，跳过 7 个；基于 #181 之后的 `main`）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。
- `python3 scripts/check_architecture.py` 通过（本 crate 是插件 crate，不依赖其他插件 crate、不依赖 GPUI）；`rg -n 'rusqlite' crates/*/Cargo.toml` 只出现在 `cache` 和 `usage`；`rg -n 'price|LiteLLM|cost' crates/usage/src crates/usage/Cargo.toml` 没有结果。
- `Cargo.lock` 只多了 `lectern-usage` 的依赖列表，没有新的外部 crate。
- 没有在 macOS 上运行：本 crate 不依赖 GPUI，Linux 与 macOS 编译的代码相同。`plugin/usage/activate` 和第 1 步的其余 bench 条目在后一部分加入，本部分没有 bench 数字。

### 第 1 步（后一部分）

在 Linux（x86_64 容器）上运行：

- `just test -p lectern-usage`：75 个测试通过，比前一部分多 19 个：
  - `archive_*` 13 个：截止点在东 14 区、西 12 区、UTC 与纽约夏令时切换日的位置，只有整月被归档；归档 2025 年 8、9 月之后热行没有了、冷数据段解码出的行与归档前逐行相同，10 月仍是热的，7D / 14D / 30D 与只要按月的 `History` 不读冷数据段（读取计数不变），按日的 `History` 读，窗口快照、`History` 和每个会话的热事实行加 `cold_total` 与归档前相同，`History` 的按月行有 insta 快照，范围落在月中时按月的行仍是整月（冷的 2025 年 9 月与热的 2026 年 10 月），再跑一次不归档；ID 最大的来源已归档时抢救之后 `usage_source` 的序列不低于冷数据里的 ID，之后才读到的 9 月记录照常计入（`archive_salvage_keeps_new_source_ids_above_the_cold_ones`）；1 MiB 换成 300 字节的段之后，在每一步之间（`archive_survives_an_end_between_segments`）和第一段、中间、最后一段的段已插入 / 热行已删 / 汇总已更新之后（`Store::faults` 注入失败）结束进程，重启后热行加冷数据段与归档前逐行相同，接着归档不重复；另一会话里晚 20 天的同一批键分在不同段时只计一次（`archive_counts_a_key_once_across_segments`）；解码核对失败时热行不删、这个月停在「归档中」、计划跳过它；回放跨过截止点和已归档月份的会话不把这些月份写回热表（`kept_cold`）；之后才读到的 9 月的旧记录先只写观察、不计数，下一轮归档为 9 月追加新段（来源的段从一段变成两段）并计入 9 月的合计；归档到只剩事实行时退出、重启后重建热窗口与回放，结果不变、冷数据里没有重复的事实行；完整重建（`unarchive_all`）之后与重建前相同，再归档的行数相同；`heavy_days` 的两个重度日经 `testing::Scenario` 归档、`History` 往返；空闲页超过阈值时才回收、每批不超过 2048 页。
  - `store_*` 新增 3 个：`store_migration_from_version_1_adds_the_cold_tables`（版本 1 的文件与 `CREATE_V1` 的表结构相同，迁移后有冷数据的表、`fact_bucket` 等于事实行的分组和、合计不变）、`store_salvage_does_not_mark_placeholder_sessions`（抢救后读完日志 `repairing` 为 false）、`store_batch_read_across_a_salvage_is_read_again`（抢救之前取了游标的一批在入库时被丢弃、从 `START` 重读，合计与损坏前相同）。
  - `window_deltas_give_what_a_full_compute_gives`：三个关注的窗口在每一批入库后应用增量，与整体计算的快照相同，`fact_bucket` 等于事实行的分组和；新模型、新 agent、窗口外的记录、另一会话里更大的同一个键（删一行、加一行）、重放、文件夹认领的新目录（整体重算）。`window_deltas_out_of_order_compute_again`：后一个事务的增量先到时整体重算，前一个随后到达时不变，结果与整体计算相同。
  - `plugin_archive_takes_one_step_per_quiet_stretch`：插件只在 10 秒静止之后归档一步。
- 去掉「之后到达的旧记录」的 ID 标记、改回对整月的来源选计数来源时，`archive_counts_a_key_once_across_segments` 失败（8 月的合计翻倍）；去掉增量的顺序检查时 `window_deltas_out_of_order_compute_again` 失败；抢救不抬高序列时 `archive_salvage_keeps_new_source_ids_above_the_cold_ones` 失败（序列 3，冷数据里的 ID 到 12）。
- bench（`cargo bench -p lectern-bench --bench time -- usage/` 与 `--bench memory -- usage/`，Linux 上按规则采样、只报告不判定）：

  | ID | 实测 | 预算 |
  |---|---|---|
  | `usage/ingest_append_4k` | 中位数 765 µs | < 5 ms |
  | `usage/backfill_batch` | 中位数 54.1 ms | < 150 ms |
  | `usage/backfill_large_session` | 最大值 112 ms，中位数 46.8 ms；交出 209 700 964 字节 / 文件 209 707 008 字节 | < 300 ms，< 150 ms；≤ 1.2 倍 |
  | `usage/project_30d` | 中位数 2.61 ms | < 50 ms |
  | `usage/all_projects_30d` | 中位数 19.4 ms | < 80 ms |
  | `usage/snapshot_increment` | 中位数 20.5 µs | < 2 ms |
  | `usage/resident_bytes` | 中位数 80 KB | ≤ 2 MB |
  | `usage/archive_month` | 每段中位数 125 ms；27 段、60 万行、编码 34.1 MB、压缩后 4.76 MB（7.2:1），归档前数据库 76.3 MB | < 150 ms；压缩比 ≥ 3:1 |
  | `usage/cold_read_month` | 中位数 66.8 ms（6 段 `fact`） | < 1 s |
  | `usage/disk_day_heavy` | 5.49 MB，其中 `-wal` 4.23 MB | 只记录（见「未决问题」2） |
  | `plugin/usage/activate` | 中位数 5.99 µs | < 1 ms |

  30 个重度日的夹具写入约 8 秒。改动之前的实测：`project_30d` 64 ms、`all_projects_30d` 219 ms、`archive_month` 每段 367 ms（4 MiB 的段），分别由「与设计的差异」的 `may_include_inherited` 查询、`fact_bucket`、1 MiB 的段和按键顺序删除解决，预算都没有改。
- `just check`、`just test`（不依赖 GPUI 的全部 crate，2233 个测试通过，跳过 7 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过；`rg -n 'price|LiteLLM|cost' crates/usage/src crates/usage/Cargo.toml` 没有结果。
- `Cargo.lock` 只多了 `lectern-usage` 对 `ruzstd`（工作区已有）和 `lectern-bench` 对 `lectern-usage` 的依赖，没有新的外部 crate。
- 没有在 macOS 上运行：bench 数字只是 Linux 容器上的报告，预算的判定在 CI 的 macOS 任务上。

## 与设计的差异

### 前置条件 A

- 设计没有列出的派生（「接口」已补上）：不含浮点数的类型都派生 `Hash`，`usage` 按 `UsageQuery` 找关注和快照，`RowKey`、`MrRef`、`UsageProjectId` 用作聚合和视图状态的键。不含 `Arc` 的小类型派生 `Copy`。几个标识和 `LocalDate` 派生 `Ord`，`LocalDate` 按年、月、日比较。`Tokens` 派生 `Default`（全 0）；`Limits` 派生 `Default`，作为数据库还没打开时 `limits()` 的空快照。做法与 review 设计「与设计的差异」前置条件 A 相同。
- `Tokens::total` 用饱和加法：四类之和超过 `u64::MAX` 时取 `u64::MAX`，不在调试构建里 panic。实际的 token 数远小于这个上限，只是让损坏的记录不能让视图崩溃。
- `ProjectUsage::daily`、`AllUsage::daily` 的元素类型命名为 `DayTotals`（`Arc<[(Agent, u64)]>`，「接口」已补上）：`Arc<[Arc<[(Agent, u64)]>]>` 触发 clippy 的 `type_complexity`，本仓库警告即失败。JSON 形状不变。

### 第 1 步（前一部分）

- **定时器不用专用线程。** 设计原来写 `Executor::run_dedicated("usage-timer", …)`。实现里每个截止时刻是交给 kernel `Clock::wake_at` 的一个回调（「依赖」「数据流与线程」已改）。理由：本 crate 不必开自己的线程，`SystemClock` 的定时线程按需启动、空闲即退出、与 kernel 共用；测试用 `ManualClock` 推进同一份代码，不用另写一套可推进的时钟。
- **按墙上时间的截止时刻分段等待。** 本地零点原来按单调时钟一次等到。单调时钟在 Mac 睡眠时不走、系统时间也可能被改，零点的重算会晚到。现在每次最多等 15 分钟（`time::WALL_CHECK`），醒来比较墙上时钟，没到就再等（「数据流与线程」已改）。`testing::ManualTime::jump_wall` 只拨墙上时钟，测试 `plugin_midnight_is_seen_after_a_sleep`。
- **补读节奏按事件的 `seq` 判断静止。** 插件拿不到 `Kernel::quiescent_epoch`，按 `Quiescent` / `Busy` 事件的 `seq` 判断，`seq` 不大于已见最大值的事件忽略（「数据流与线程」已改）。静止满 10 秒时如果另一批还在读（活跃会话），这次的许可当场作废，不留到之后；不在收到 `Busy` 时清除许可：`Busy` 由 kernel 的生命周期线程异步送达，会与刚到时的许可抢先后，让到时的那一批读不成（`plugin_backfill_waits_for_ten_quiet_seconds_per_batch` 的压力运行复现过）。
- **启动时只读变化的日志。** 原来打开数据库后把全部已知会话标为待读，没变的会话也要等补读的节奏各读一次，补读完之前一直是 `Backfilling`。现在 `session.end_stamp` 记下最后一批读到末尾时、读之前日志文件的大小、修改时间与 inode，打开后和每次 `SessionsChanged` 之后先比较，没变的不读（「数据流与线程」「数据库」已改）。适配器版本只有读一次才知道，每个来源每次运行另读一次最新的未变会话（不按节奏），它回放时这个来源的会话全部重读。`testing::FakeSessions::with_files` 放真实的稀疏文件，`set_adapter` 改适配器版本；测试 `plugin_restart_reads_only_changed_logs`、`plugin_adapter_upgrade_replays_unchanged_logs`。记的是 `SessionSummary::file` 一个文件；一个会话的记录来自别的文件时（目前的两个来源都没有）要另记。
- **快照整体重算。** 设计要求新的事实行到达时按增量更新快照。这一部分在每次提交、项目打开或关闭、本地零点之后，在后台整体重算被关注的查询（同时只有一个计算任务，期间的变化合并到下一轮）。增量更新与 `usage/snapshot_increment` 一起放到第 1 步后一部分，按实测决定。`app` 构造插件（前置条件 C）之前，要么按增量更新满足 `usage/snapshot_increment` 的 2 ms，要么写 ADR 修订这条预算；整体重算不能直接进入应用。（后一部分已按增量更新，见下一节的「窗口快照按事实行增量更新」。）
- **第 1 步没有 `share` 表。** 每个轮次按 cwd 归属，每个计数的来源一行事实（`round::split(tokens, &[1])`，即全部 token）。`share`、`item` 与定稿随第 4 步加入（表结构版本 2，迁移前写副本）。新增 `session_gen`，记下每一代读到这一批末尾时的 cwd 位置、模型和所在轮次，下一批从这里接着读；`intern` 是设计里的驻留表（「数据库」的表已补上）。`fact` 已带后续步骤要用的列（`segment`、`kind`、`review`），第 1 步为空或 0。
- **不计数的来源。** 选择顺序原来第一项是「不是 inherited 的优先」，同一节又写「inherited 的来源不计数」。实现以后者为准：只在不是 inherited、不是被取代的来源里选，一个键只有这两类来源时不计数；最后一级按来源行的 ID，结果确定（「规范化与去重」已改）。
- **来源与轮次的 ID 不重用。** `usage_source`、`turn` 用 `AUTOINCREMENT`（「规范化与去重」已写明）。测试 `reset_during_a_replay_starts_it_again` 发现：回放删除一代的来源后，新来源拿到同一个 rowid，旧来源的事实行按来源 ID 删除时删掉了新来源的事实行。
- **cwd 的归位。** 会话开始时的 cwd 用 `SessionSummary::place`（`agents` 已归位）；`Context` 记录里的其他 cwd 用 `Projects::resolve`，结果按路径记在内存里（最多 4096 条）。不用 `locate`：它要求规范路径，日志里的 cwd 不一定是。`resolve` 失败（目录已不存在）时按日志里的路径记为目录位置，汇总时进「其他」。用量记录的位置取它所在轮次开始时的 cwd，不在任何已读轮次里时取当时的 cwd。
- **从未打开的仓库。** 仓库位置第一次出现就分配项目（`randomblob(16)`），`opened` 为 0，用量进「其他」；`ProjectEvent::Opened` 和启动时的 `open_projects` 把它标为打开过并记下显示名。关闭之后仍在「所有项目」里单列，`ProjectInfo::key` 为 None。`ProjectRef::Open` 指向一个打开着、数据库还没有记录的项目时，计算快照前先记录它。根提交、远端和别名的比对在第 3 步。
- **数据库的几处补充。** `PRAGMA application_id` 为 `0x4C435553`（"LCUS"），不是本应用写的文件按 `SQLITE_NOTADB` 处理（改名保留、新建）。迁移前的副本写不出来时进入只读的 `Unwritable`，本次运行不再写；一般的写失败同样标为 `Unwritable`，下一次写入成功后恢复。派生算法版本变化时在打开任务里用一个事务重建（目前只有版本 1；分批重建随第 5 步的 `rebuild_*`）。抢救时无法知道读不出的行属于哪个会话，只要有行丢失，全部会话的游标回到 `START`、未完成的回放作废，丢失数按读不出的 rowid 计；文件头坏到无法 `ATTACH` 时什么也复制不了，全部会话从头重读；一张表的列定义都读不出（`prepare` 失败）时按丢失计。抢救期间写连接用 `synchronous = OFF`，完成后改回 `FULL` 并做检查点。更新版本的文件不抢救（`repair` 与 `check` 一样跳过）。
- **抢救先建新文件、最后替换。** 设计原来写先把原文件改名、再新建。那样新文件就在 `usage.db`，建表时 `meta.derived` 已写好，抢救中途崩溃或退出后，下次打开看到的是一个像是完整的新库，没复制完的行就丢了。现在在 `usage.db.salvage` 里做完复制、补全悬空引用、会话重读、重建派生表、检查点和 `fsync`，原文件只读附加、不改名；最后硬链接出 `usage.db.corrupt-<毫秒>`、删掉原文件的日志、`rename` 新文件、`fsync` 目录。打开时发现 `usage.db.salvage` 就删掉它，从头再抢救（「数据库」已改）。测试 `store_interrupted_salvage_runs_again` 在每一步之前注入失败（`cfg(test)` 的 `store::fail_at`）。
- **抢救后不留悬空引用，也不丢用量。** `intern`、`location`、`session` 的 ID 是现存最大值加一，有行丢失时新行会拿到丢失行的 ID，留下的引用就指错了行。复制之后补全指向丢失行的引用，行都留下（「数据库」损坏的处理第 3 步已写明）：来源 ID、agent 标签和模型名驻留得早、ID 小，被每个会话共用，删掉引用它们的行会在驻留表的第一页损坏时删掉全部用量，而 Claude Code 默认只保留 30 天的日志。丢失的字符串和会话按原 ID 补回占位行，模型、agent、来源的位置换成空字符串、`agent:other:lost`、未知位置；`usage_source` 加 `repaired` 列（只给它建部分索引），打开时有这样的行，入库就改用 upsert，同一条记录重读时换上重读的轮次、位置和模型，并让这个键重新选出计数的来源；会话行的 agent 变了时同步更新它的事实行。不改成 `AUTOINCREMENT`：表最后一页丢失时旧文件的最大 ID 本身也读不出。与去重不冲突：键里没有会话，重读出的来源与留下的来源是同一个键的两份，只计一份，占位会话的来源在排序上靠后。测试 `store_salvage_keeps_rows_that_point_at_lost_rows` 让驻留表的第一页和最后一页、会话表与位置表的最后一页分别损坏：抢救后合计不变、没有悬空引用；新会话先读、拿到丢失的 ID，旧会话再重读后，每条计数的来源的会话、位置、模型和 agent 都对得上，合计等于损坏前加新会话。
- **目录的 `fsync` 尽力而为。** 抢救和迁移前的副本之后对数据目录的 `fsync` 失败时只记 warn、照常继续（与 SQLite 的做法相同）：有的文件系统拒绝对目录 `fsync`，否则每次启动的抢救或迁移都会失败、数据库一直打不开。文件本身的 `fsync` 失败仍算失败。
- **适配器变化只排一次。** 每个来源在一次运行里只因适配器变化把会话排队一次：旧游标的会话读到时都会回放、都报告变化，每次都重排会让 N 个会话读约 N²/2 次。测试 `plugin_adapter_upgrade_replays_unchanged_logs` 用 6 个不活跃的会话，升级后每个会话只多读一次。
- **数据库打不开时的快照。** 原来数据库打不开时快照为 None，视图看不到 `Degraded(Unwritable)`。现在窗口查询给出没有用量的快照，状态为 `Degraded(Unwritable)`；`ProjectRef::Open` 的项目 ID 暂为全零（「数据流与线程」已改）。测试 `plugin_unopenable_database_degrades_the_snapshots`。
- **`UsageDeps` 还没有 `watcher`、`git_ceilings`。** 它们在第 2 步（statusline 目录）和第 5 步（reflog 监听）才用到，届时加入，在 `app` 构造插件（前置条件 C）之前补齐。测试与 bench 经 `UsagePlugin::with_time`（`test-support`）换时钟。
- **后续步骤的接口先给空结果。** `Worktree`、`Session`、`Mr`、`History` 查询的快照为 None（`History` 随后一部分实现）；`limits()` 是空快照；`command` 只记 debug；`begin_analysis`、`meter` 返回 `Unavailable`；`register_mr_source` 立即放下提供者，返回的登记 drop 时什么也不做，从不调用提供者。
- **快照的细节。** `daily` 的每一天列出窗口里出现过的全部 agent（那天没有用量的记 0），柱状图的系列不随天数变化；agent 按 Claude Code、Codex、其他（按名字）排序；`by_model` 按 agent 再按模型名排序，模型未知时名字是空串；`AllUsage::projects` 按合计从大到小、再按名字排序。合计每类按最大余数法取整；`round::largest_remainder` 的输入之和不是整数 token 时取最近的整数（第 1 步每条记录的事实行都是整数 token，不会出现）。
- **补读进度。** `Backfilling { done, total }` 的 `total` 是这一轮补读的会话（比较后发现变化、不活跃的会话，以及适配器变化后回放的会话）的文件大小之和，`done` 是它们的游标偏移（不超过文件大小）之和；这些会话都读完后补读结束，状态回到 `Ready`，之后再发现的变化开始新的一轮。`.jsonl.zst` 的游标是解压后的偏移，进度会偏快；适配器变化后回放的会话大小记为 0，进度只按已读的字节增长：都是已知限制。
- **日志。** 用已合并的 `lectern-logging`（#177）。读一个会话时设 `session` 作用域：会话 ID 是 UUID 时取它的 16 字节，否则取 128 位哈希。默认级别记数据库打开（版本、迁移、是否可写）、损坏与抢救（保留的原文件路径、复制与丢失的行数、补全引用的行数、重读的会话数）、中断的抢救与被拒绝的目录 `fsync`（warn）、写失败（SQLite 扩展错误码）、回放、适配器变化后的整源重读、补读开始与完成、项目打开；每批的游标前后位置在 debug 级别。`log_canary_never_reaches_a_record` 是 logging 设计「对其他 crate 的要求」里的金丝雀测试。
- **测试辅助。** `testing::FakeSessions` 的脚本是会话摘要加 `services::session` serde 形式的 `Positioned<Record>` 序列（格式写在 `src/testing.rs`），行的长度由相邻记录的 `pos` 给出；`testing::FakeProjects` 按测试声明的仓库与文件夹归位，不做 IO；新增 `testing::ManualTime`（含只拨墙上时钟的 `jump_wall`）；`FakeSessions::with_files`、`set_adapter` 见「启动时只读变化的日志」。`testing::Scenario`、`heavy_days` 放到第 1 步后一部分，`FakeMrSource` 随第 7 步。

### 第 1 步（后一部分）

- **表结构版本 2。** 冷数据的表是版本 1 之后加的：迁移加上 `cold_part`、`cold_month`、`cold_total`、`cold_anchor`、`cold_seen` 和 `usage_source`（时刻）、`turn`（开始时刻）两个索引，迁移前照常写副本（测试 `store_migration_from_version_1_adds_the_cold_tables`）。汇总表用整数主键加唯一约束，不用 `WITHOUT ROWID`：抢救按 rowid 区间复制。按时刻的两个索引是设计没有的：归档按月找行、按月找计划里的月份，没有它们每一段都要扫整张表。
- **`usage_value` 随来源归档。** 设计的顺序是观察、`share`、`usage_value`、`fact`。`usage_value` 没有时间戳，只能按键找；找出一个月的行要按来源查，而按来源的索引与这张表一样大（键是 60 字节上下的字节串，重度一年约 200 万行）。现在来源的每一段在同一个事务里带上这些来源的 `usage_value` 行，写成两段（「冷热分层」已改）。月份冻结之后没有别的写入碰这些行，提前归档不影响一致性；归档到一半时，`fact` 还在热表里，冷数据里已有观察和计数的来源，合计没有重复也没有缺口。
- **段格式的补充。** 文件头另有列名，解码按名字找列；`fact` 的段每行多一列 `cold_ym`（计入 `cold_month` 的本地年月），完整重建从汇总里减回去时用它，不重新按时区分月。解压时核对帧的内容校验和。
- **待归档不加标记，跳过规则推广。** 已冻结月份里的热行就是之后到达的旧记录，不另加「待归档」标记；归档每一步先为冻结之后才写入的来源（ID 大于 `meta` 的 `archive_mark:<月>`）里热表中没有计数来源的键选出来源（只在这些来源里选）并写事实行，再写段。最初对整月的来源这样做，先归档的段已带走计数来源的 `usage_value`，后面段里同一个键的另一份来源就被再计一次（测试 `archive_counts_a_key_once_across_segments`：另一个会话里晚 20 天的同一批键，去掉这个标记时这个月的合计翻倍）；每一步还要扫一遍整月的来源。设计只说重放时不再写入已有冷数据的记录；抢救之后全部会话从头重读也会把已归档的记录写回热表，所以改为：会话在已冻结月份里已有数据时（新加的 `cold_seen`，或重放时更早一代的热行），它这个月的用量记录和轮次都不再写入，计数进日志（`kept_cold`）。重放读完时，已冻结月份里旧一代的行不删，保留原来的代号由归档带走。
- **冻结的落点。** 去重的重新选出：计数的来源在冻结月份里时整个键不动（来源已归档时按它的事实行判断），冻结月份里的来源不是候选；热窗口重建只删除、重算不在冻结月份里的事实行，`usage_value` 只删没有事实行的；抢救补全过的来源在冻结月份里重读时只清掉标记，不换引用、不删事实行；会话的 agent 因抢救而变时只改不在冻结月份里的事实行。
- **归档计划的节奏。** 设计写每月月初之后第一次有工作的那天检查一次。现在数据库打开后和每个本地零点各查一次：按时刻索引逐月找第一行，代价是每个有数据的月份一次索引查找，比判断「本月是否已查过」简单。核对失败时 `meta` 记下段格式版本（`archive_failed:<月>`），计划跳过这个月，直到格式版本变了。
- **完整重建的做法。** 设计写逐月解压、重算、写成新段替换旧段。现在逐月把观察放回热表（一个事务：放回观察行、按 `cold_ym` 从汇总减回、删段和 `cold_seen`、状态回到「热」），再重建热窗口，下一轮归档重新写段，复用归档本身的代码；每个事务之后中断都一致。派生算法还只有版本 1，打开时不调用它，只有测试和 bench 经 `testing::Scenario::unarchive_all` 调用；派生版本变化时由那一步决定接入。
- **抢救与冷数据。** 冷数据段与汇总表、`cold_seen` 随观察复制，复制后逐段解码，解码不了的段删掉并计入丢失（原文件里还有）；已冻结月份的 `fact` 和 `usage_value` 也复制，因为重建不重算它们、它们的来源可能已经是冷的。复制不带 `sqlite_sequence`：ID 最大的来源已归档时，新来源会拿到冷数据里已有的 ID，落在某个 `archive_mark` 之下的来源永远不会计数。所以复制之后 `usage_source`、`turn` 的序列从这几个值的最大者开始：留下的行、原文件读得出的序列、冷数据段里的 ID（解码核对时顺便取得），来源另加每个 `archive_mark`（测试 `archive_salvage_keeps_new_source_ids_above_the_cold_ones`）。
- **`History` 的口径。** 每个月都是热的事实行加 `cold_month`（归档中的月份两边各有一部分，相加是全月）；按月的行包括范围里的每个月、每行是整个本地月（`cold_month` 只有整月，热的一侧也按整月求和），`days` 包括范围里的每一天，各自作为一组行取整。两边与段的列表在一个读事务里读出。`may_include_inherited` 只看热的事实行。`History` 不按增量更新：每批入库和每次归档写段之后整体重算（现有视图都不关注它）。
- **`cold_anchor` 第 1 步不写。** 它存已归档的有改动轮次和份额，第 1 步还没有改动项和份额；表已建好，第 4 步写入并加 `archive_*` 用例。
- **`archive_*` 的范围。** `Worktree`、`Session`、`Mr` 查询在第 1 步还没有快照，测试改为检查每个会话的热事实行加 `cold_total` 等于归档前的合计（`archive_moves_old_months_and_keeps_every_total`），这三个快照与归档前相同的检查随各自的步骤加入。
- **窗口快照按事实行增量更新。** 前一部分在每次提交后整体重算。现在 `Project`、`AllProjects` 的关注保留快照的累加器，入库事务经 `RETURNING` 交出插入和删除的事实行，读取任务当场应用并替换快照（「数据流与线程」已写明 `meta.commits` 的对齐、需要整体重算的情形和计算中到达的增量）。整体计算也经同一套累加器，所以增量的快照与整体重算的快照相同（测试 `window_deltas_give_what_a_full_compute_gives`：新模型、新 agent、窗口外的记录、另一会话里更大的同一个键、重放、文件夹认领的新目录）。增量只按入库事务的顺序一个接一个应用：两批读取的增量先后颠倒时，后一个事务的增量先到会整体重算，不会把前一个丢掉（测试 `window_deltas_out_of_order_compute_again`）。`usage/snapshot_increment` 在预算内，`app` 构造插件不再受这条限制。
- **`usage/snapshot_increment` 的计时范围。** 设计写「一轮定稿到快照替换」，第 1 步还没有定稿；基准从一批入库返回（增量在手）计到快照替换，这一批的入库本身由 `usage/ingest_append_4k` 计时。第 4 步加入定稿后改为从定稿开始计。
- **`testing::Scenario` 的形状。** 设计写场景经 `Executor::manual()` 与 fs_watch 的 `FakeBackend` 驱动引擎。第 1 步只有会话记录，`Scenario` 直接驱动存储和快照代码（读一批、入库、应用增量、查询、归档、`History`），步骤与引擎相同；reflog、工作区状态与 MR 时间线随第 5–7 步加入，届时需要定时器和监听的场景再接上 `Executor::manual()` 和 `FakeBackend`。
- **`heavy_days` 的合成日志。** 每个重度日 50 个会话、20 个仓库（每五个会话一个 Codex）、每会话 40 轮、每轮 3 条用量；每轮在日志里占 14 行、每行 1.5 KiB，只生成轮次开始和用量记录，工具调用的行第 1 步不入库，不生成。`heavy_open` / `heavy_fill` / `synthetic_turns` 一并公开，给需要在夹具上追加会话的基准。
- **`plugin/usage/activate` 在进程内测。** kernel「性能」的四段耗时由进程级基准取（bench 第 5 步，尚未实现）。现在在 `time` 目标里用 `TestKernel` 激活插件、读 kernel 记下的四段耗时，bench 设计「基准目标」与预算表已写明这个例外，进程级基准实现后改回 `Process`。
- **段不超过 1 MiB。** 设计写未压缩不超过 4 MiB、一个重度月约 8 段。实测（Linux）一段 4 MiB 的来源（约 4.7 万行）一步要 550–650 ms，主要是删除热行：键索引和（会话、代、`pos`…）唯一索引的页是随机的，每个连接只有 1 MiB 页缓存（ADR 0027），还有提交时的 WAL 写入。1 MiB 的段一步约 100–130 ms，满足 `usage/archive_month` 的每段 150 ms；一个重度月约 26 段，压缩比 7.2:1（「冷热分层」已改）。预算没有改。
- **`usage_value` 按键的顺序删除。** 来源段带的 `usage_value` 行按键排序、逐行按主键删除，相邻的键在同一页上，小页缓存里留得住；按来源区间的子查询删除时一段 1 MiB 要 40–80 ms，排序后 8–16 ms。来源、轮次、事实行按（时刻、ID）的区间整段删除，删除的行数必须等于段的行数，否则这一步失败回滚。
- **`fact_bucket`。** 设计没有这张表。`usage/all_projects_30d` 按 `fact` 求和（30 个重度日、18 万行）实测 219 ms，超出 80 ms。现在每个（15 分钟桶、位置、agent、模型、编写 / 审阅）一行热事实行的和与行数，与 `fact` 的每次增删在同一事务里维护（`RETURNING` 收集增量时一起加减），时间窗口的查询只读它：`all_projects_30d` 19 ms、`project_30d` 2.6 ms。月份归档完时删掉这个月的行；热窗口重建、会话的 agent 因抢救而变之后从 `fact` 整体重算；表结构版本 2 的迁移从 `fact` 算出（「数据库」的表已补上）。测试辅助 `assert_buckets` 在增量、归档、重建、迁移的测试里检查它等于 `fact` 的分组和。
- **`may_include_inherited` 的查询。** 原来对窗口里每条事实行查它的会话是否 `inheritance_unknown`，30 天的项目快照要 64 ms；现在先列出 `inheritance_unknown` 的会话（通常没有），再按会话查它们在窗口里有没有事实行。
- **前一部分留下的两个已知限制已修正。** 占位会话（会话行是占位，或来源 ID、会话 ID 是占位串）的来源在抢救时不标 `repaired`：没有日志会再给出这些记录，原来 `repairing` 因此一直为真，每次入库都走 upsert（测试 `store_salvage_does_not_mark_placeholder_sessions`，并在 `store_salvage_keeps_rows_that_point_at_lost_rows` 里检查读完之后没有标记）。读取期间的抢救：入库事务里核对保存的游标仍是这一批读取时的游标，不是就丢弃这一批、从保存的游标重读；`repaired` 是否还有也在事务里查，`Store` 不再缓存（测试 `store_batch_read_across_a_salvage_is_read_again`）。
