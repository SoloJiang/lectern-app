# forge 设计

状态：已接受，尚未实现。

对应 issue [#25](https://github.com/SoloJiang/lectern-app/issues/25)。`forge` 读 GitHub 与 GitLab（含 GitHub Enterprise Server 和自部署的 GitLab）上的 MR：列表、详情、CI 状态、PR 评论里 pulls.review 的共享分析。它为审阅给出 MR 的改动集（`ChangeSource`），为用量统计给出分支与 MR 的对应（`MrSource`）。MR 屏幕是[交互规范「屏幕」](interaction.md#屏幕)的 06 画板，由新的视图 crate `forge_view` 绘制（「对其他 crate 的要求」H）；本 crate 只提供数据。平台选型见 [ADR 0008](../adr/0008.md)，共享分析的规则见 [ADR 0019](../adr/0019.md)，MR 级用量见 [ADR 0020](../adr/0020.md)。

## 范围

- 做：
  - 识别平台：从项目的 git 远端地址解析主机和仓库路径。github.com 与 gitlab.com 内置；其他主机由用户确认平台和命令行的主机名（ADR 0008 的「自部署实例由用户配置」）。
  - 允许表：只有内置主机和用户确认过的主机会收到请求。这是安全边界，见「主机与允许表」。
  - 调用 `gh api` / `glab api`：查找命令行、清理环境变量、固定参数、解析 `--include` 输出、限制输出大小和时长、条件请求（ETag）、限流与退避。
  - MR 列表（与项目本地分支对应的 MR）、MR 详情（标题、描述、状态、合并基点、改动文件数）、CI 状态（GitHub check run 与 commit status，GitLab 流水线与 job）。
  - MR 的改动集：提交在本地时由 `git` 计算 diff；不在本地时用代码平台的文件列表，标为「部分内容未提供」。MR 的指纹和取数一致性的检查。
  - `MrSource` 的实现（[usage 设计「对其他 crate 的要求」](usage.md#对其他-crate-的要求)前置条件 F）：仓库的不变 ID、分支对应的 MR、状态时间线、历史 base、合并前的目标分支提交。
  - 读取 pulls.review 的共享分析评论：按条数、字节、解压后大小设上限，判断作者是否受信任，判断是否过期。只读，不写入。
  - `dyn Forge` 服务和三个插件：`forge`、`forge-usage`、`forge-review`。
  - 设置的持久化：用户确认的主机、受信任的机器人账号、命令行路径、总开关。
- 不做：
  - 绘制、Fluent 文案、UI 探针节点、快捷键：`forge_view`（新 crate，「对其他 crate 的要求」H）。本 crate 没有界面字符串，只返回枚举。
  - 共享分析的结构校验、覆盖检查和对账：`review`，与本地模型输出用同一套校验（ADR 0019）。本 crate 只取出评论里的 `result` 原文。
  - 计算 diff、读 git 对象：`git`。用量的归属：`usage`。
  - macOS 的系统代理设置：`platform`（架构规则 6，「对其他 crate 的要求」E）。
  - 向 PR 写任何内容：Lectern 不写自己的分析（ADR 0019），本 issue 也不写审阅评论。
- 本 issue 之外：
  - 读写 PR 的审阅评论（`ChangeSource` 的「读写审阅评论」能力）：另开 issue。审阅意见（[#22](https://github.com/SoloJiang/lectern-app/issues/22)）发回 agent 会话，不发到 PR。
  - 由 Lectern 自己取回不在本地的 MR 提交：见「未决问题」2。
  - 其他代码平台（Gitea、Bitbucket 等）和 GitLab 自部署的扩展能力：按 [ADR 0018](../adr/0018.md) 做成进程插件，接口随进程插件宿主（[#39](https://github.com/SoloJiang/lectern-app/issues/39)）设计。
  - 项目主页会话卡片的「测试结果」信号（[project_home 设计](project_home.md)「信号」）：`project_home` 读本文的快照，在它自己的 PR 里做（「对其他 crate 的要求」I）。

## 访问方式的选择

结论：经 `gh api` / `glab api` 子进程访问代码平台的 REST 接口。Lectern 进程里不发 HTTP 请求，不持有令牌。这与 ADR 0008 的「调用 gh / glab CLI，复用其登录态」一致，不需要新 ADR。

| 方案 | 凭据 | 依赖与体积 | 每次请求的代价 | 结论 |
|---|---|---|---|---|
| `gh api` / `glab api` 子进程 | 不进入 Lectern：命令行自己从钥匙串或配置文件取 | 不新增 crate；用户要装 gh / glab | 起进程约 30 ms，加一次新的 TLS 握手 | 选用 |
| 进程内 HTTP（octocrab，或 reqwest + rustls），令牌经 `gh auth token` 取得 | 令牌进入进程内存，要修订 ADR 0008，还要防日志、崩溃报告泄露 | 新增 HTTP、TLS 和异步运行时（octocrab 依赖 tokio、hyper），计入 80 MB 包体（[ADR 0018](../adr/0018.md)）和空闲内存 | 连接复用，后续请求少一次握手 | 不选 |
| GraphQL（`gh api graphql`） | 同第一行 | 同第一行 | 一次可以取多项 | 没有 ETag 和 304，按点数计费；GitLab 自部署的 GraphQL 版本参差，两个平台要两套查询。不选 |
| `gh pr diff` / `glab mr diff` 取补丁（[技术调研](tech-research.md#3-代码与-diff-查看)原来的写法） | 同第一行 | 同第一行 | 大 PR 一次很大 | 补丁里没有两侧的 blob OID 和完整内容，造不出 `ChangeList` 的 `SideIdentity` 和字节偏移；GitHub 对超大 PR 返回 406。diff 改在本地算（「MR 的改动集」） |

选用的理由：

- 凭据边界最小。令牌不进 Lectern 的内存、日志、缓存和子进程参数。Lectern 不调用 `gh auth token`、`glab auth` 的任何子命令。
- 登录方式全部复用：钥匙串、SSO、GitHub Enterprise Server、自部署 GitLab 的 `api_host` 配置、企业 CA，都由 gh / glab 自己处理。
- 不增加依赖和包体。

代价与对策：

- 每个请求一个进程、一次新的 TLS 握手。设计时在本机测得（经 HTTP 代理访问 github.com）：`gh --version` 32 ms；`gh api -i` 一次约 0.93–1.10 s，304 与 200 相同；同一连接上的第二个请求约 0.41 s。所以：
  - 先显示缓存，后台再校验（「缓存」）。
  - 首屏只发必要的请求，其余逐步加载（「数据流与线程」）。
  - 每个主机同时最多 2 个请求（「命令行的调用」）。
- 实现之后在真机上测 MR 详情从缓存到最新的耗时，记进「实际验证」。如果常用路径不可接受，另写 ADR 评估进程内 HTTP（要同时修订 ADR 0008）。

设计时核对过的命令行行为（gh 2.97.0、glab 1.113.0，只读请求）：

- `-i` 时标准输出依次是状态行（如 `HTTP/2.0 304 Not Modified`）、响应头、空行、响应体。
- 状态码不是 2xx 时（包括 304）两者都以 1 退出，标准输出照样有状态行和响应头；标准错误是 `gh: HTTP 304`、`glab: 404 Not found (HTTP 404)` 这类文字。所以以状态行为准，不以退出码判断请求结果。
- gh 的 304 不消耗 `X-Ratelimit-Remaining`（与 [GitHub 的说明](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api)一致）。
- glab 不带 `--hostname` 时按当前目录的 git 仓库推断主机；`NO_PROMPT` 已改名为 `GLAB_NO_PROMPT`，旧名仍有效并打印弃用警告。
- gh 有遥测（`GH_TELEMETRY`、`DO_NOT_TRACK`）和每 24 小时一次的更新检查（`GH_NO_UPDATE_NOTIFIER`）；glab 有更新检查（`GLAB_CHECK_UPDATE`）和发到 GitLab 实例的遥测（`GLAB_SEND_TELEMETRY`）。这些都要关掉（「命令行的调用」）。
- gh 的 `--verbose` 和 `GH_DEBUG=api` 会把请求头（含 `Authorization`）写出。

## 依赖

本 crate 是插件 crate（`plugin = true`，[ADR 0025](../adr/0025.md)），在第一个实现 `Plugin` 的 PR（第 2 步）里加上标记。只依赖 kernel 层和非插件的库 crate，只有 `app` 和 tool 层依赖它。其他插件和视图只经 `services::forge` 读它的状态。

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-kernel` | `Plugin`、事件、`PluginHandle`、`BusyGuard`、`ActivationToken`、定时用的 `Clock::wake_at` | 插件机制；定时不另开线程（与 `usage` 相同） |
| `lectern-services` | 提供 `Forge`（前置条件 A）；使用 `Projects`、`Usage`、`Review`；实现 `MrSource`、`ChangeSource`、`MrChanges` | 插件之间只经 `services`（架构规则 7） |
| `lectern-git` | `Repo::discover`、`branches`、`remotes`、`commit`、`merge_base`、`diff_trees`、`file_change_of`；前置条件 B 的 `change_list_of`、`list_texts` | gix 只在 `git` 里使用 |
| `lectern-cache` | 应用分库的 `forge.http` 命名空间（前置条件 C） | 缓存的响应摘要可以删除重建 |
| `lectern-executor` | 后台任务、`run_dedicated_gated`（读子进程的管道） | UI 线程不做 IO；等子进程用专用线程 |
| `lectern-logging` | 运行日志（「日志与诊断」）；测试用 `test-support` 捕获记录 | kernel 层，只依赖 `log`（[ADR 0046](../adr/0046.md)） |
| `serde`、`serde_json` | 解析响应体、设置文件、缓存的值 | 工作区已有 |
| `sha2` | MR 指纹、元数据的哈希 | 已在 Cargo.lock |
| `insta`、`tempfile`（dev）；`lectern-git`、`lectern-kernel` 的 `test-support`（dev） | 快照、临时目录、`TestRepo`、`TestKernel` | 工作区统一依赖 |

不依赖 `review`、`usage`、`project`、`agents`（插件之间不得依赖，dev-dependency 也算）：测试用 `services` 的 trait 写假实现。不依赖 `platform`：系统代理由 `app` 经前置条件 E 读出后放进 `ChildEnv`，数据目录和缓存目录由 `app` 传入。

不新增外部依赖。pulls.review 的压缩格式是 lz-string 的 base64 变体，本 crate 自己写一个有输出上限的解码器（约 150 行，「共享分析评论」）：现有的 Rust 实现一次解出全部内容，无法在 2 MiB 处停止；而 lz-string 的输出可以是输入长度的平方量级。

```rust
pub struct ForgeDeps {
    pub executor: Executor,
    pub cache: Cache,
    /// `app` 取 `platform::dirs::app_data()` 下的 `forge/`。设置文件是其中的 `settings.json`。
    pub data_dir: PathBuf,
    /// `app` 取 `platform::dirs::app_cache()` 下的 `forge/`。其中的空目录 `cwd/` 是子进程的工作目录。
    pub scratch_dir: PathBuf,
    /// 子进程能拿到的环境变量（「命令行的调用」）。`app` 在首帧之后从进程环境读一次，
    /// 进程环境里没有代理变量时补上前置条件 E 读出的系统代理。
    pub env: ChildEnv,
    /// `GIT_CEILING_DIRECTORIES`，`app` 从进程环境读一次（与 `project`、`review` 相同）。
    pub git_ceilings: Arc<[PathBuf]>,
}

/// 三个插件共用一份依赖，但不共享状态：`forge-usage`、`forge-review` 经 `dyn Forge` 取得对象。
/// - "forge"：provides `dyn Forge`，requires `dyn Projects`，激活条件 `AfterFirstFrame`。
/// - "forge-usage"：requires `dyn Usage`、`dyn Forge`，`AfterFirstFrame`；`started` 里
///   `Usage::register_mr_source(forge.mr_source(), token)`，登记交给 `PluginHandle::own`。
/// - "forge-review"：requires `dyn Review`、`dyn Forge`，`AfterFirstFrame`；`started` 里
///   `Review::register_mr_changes(forge.mr_changes(), token)`，登记交给 `PluginHandle::own`。
pub fn plugins(deps: ForgeDeps) -> [(Name, Box<dyn Plugin>); 3];
```

kernel 没有可选依赖，所以 `forge` 不 requires `dyn Usage`、`dyn Review`：两个小插件反过来登记，与 `review` 的 `BriefRules`、`usage` 的 `MrSource` 相同。`forge` 停用时两个小插件随依赖一起停用，登记随之注销。

### 对其他 crate 的要求

以下改动各由对应 crate 的 PR 完成，写 `Refs #25`，在「验收」里列为前置条件。

- **A. `services`**：
  - 新的 `forge` 模块，按本文「接口」的「MR 服务」：`dyn Forge`（`ID = "forge"`，`MULTIPLE = false`）、快照和命令的数据类型、`ForgeEvent`、`ForgeInterest`。沿用 `usage` 模块已有的 `ForgeRepo`、`MrRef`，不另定义。`crates/services/AGENTS.md` 的模块列表加上这个模块。
  - `changes` 模块，按「接口」的「改动来源的扩展」：`SourceKind::Mr`、`Capabilities` 的 `remote` 与 `shared_analysis`、`ChangeList::partial`、`ChangeSource::shared_analyses` 和它的数据类型、`MrChanges` 与 `MrChangesRegistration`。`git` 的 `LocalChanges` 给出的 `partial` 总是 None，两个新能力为 false。
  - `review` 模块：`ReviewTarget::Mr { project: ProjectKey, mr: MrRef }`，`Review::register_mr_changes(provider: Arc<dyn MrChanges>, token: ActivationToken) -> MrChangesRegistration`（规则与 `register_brief_rules` 相同：每次调用提供者之前 `token.enter()`，取不到就当作没有登记）。
  - 测试：新类型的 JSON 往返；`dyn Forge` 的 `ID`、`MULTIPLE`；`MrChanges` 没有实现 `Service`；`ForgeInterest`、`MrChangesRegistration` drop 时各调用一次回调。
- **B. `git`**：把 `LocalChanges` 里 `ChangeSet` 到 `ChangeList` 的转换和 `texts` 公开为两个函数，供树与树之间的改动集使用，`LocalChanges` 也改为调用它们：
  ```rust
  /// `diff_trees` 或 `Worktree::changes` 给出的改动集转成 `services` 的 `ChangeList`；句柄持有 `set`。不做 IO。
  pub fn change_list_of(set: Arc<ChangeSet>, generation: u64) -> ChangeList;
  /// `list` 必须是 `change_list_of` 给出的；规则与 `LocalChanges::texts` 相同。阻塞（读对象库）。
  pub fn list_texts(repo: &Repo, list: &ChangeList, file: u32, max_bytes: u64) -> Result<SideTexts, ChangeError>;
  ```
  测试：`change_source_*` 照旧通过；对 `diff_trees` 的夹具，`change_list_of` 的结果与逐个 `file_change_of` 的结果相同（insta）。
- **C. `cache`**：[cache 设计「命名空间」](cache.md#命名空间)的表加一行 `forge.http`：应用分库，使用方 `forge`，键是命令行主机、账号的不变 ID、规范化的端点（「缓存」），值是 ETag、取得时刻和解析后的摘要，`Zstd` 压缩。随第 2 步的 PR 修改 cache 设计文档；`cache` 的代码不用改。
- **D. `review`**：
  - 第 4 步之前：`ReviewTarget::Mr` 经登记的 `MrChanges` 取得 `ChangeSource`；没有登记或返回 None 时快照为 `LoadState::Failed(ReviewError::Missing)`。MR 审阅按「比较范围」处理：全部文件，不归因。`Capabilities::remote` 为 true 的来源，`fetch` 和 `shared_analyses` 用 `Executor::run_dedicated` 调用，不占工作线程。`ChangeList::partial` 有值时显示「部分内容未提供」和缺少的文件数；`partial.listed_only` 时不按 `ChangeList::base` 读对象。
  - 第 5 步，在 [#66](https://github.com/SoloJiang/lectern-app/issues/66) 的校验函数实现之后：调用 `shared_analyses`，把每条的 `result` 交给与本地模型输出相同的校验和对账，结果作为分组参考放进审阅快照；`Unverified` 的条目折叠，只在用户点开后校验和显示；全部标为「可能过期」或「过期」。
- **E. `platform`**：系统代理设置。从 Dock 启动的应用拿不到 shell 里的 `HTTPS_PROXY`，而 gh、glab（Go 程序）在 macOS 上只读环境变量里的代理。
  ```rust
  pub mod net {
      /// macOS：`CFNetworkCopySystemProxySettings` 里启用的 HTTP、HTTPS、SOCKS 代理和例外列表；
      /// 只配置了 PAC 时 `pac_only` 为 true。其他平台返回 None。内存里的系统设置，任何线程。
      pub fn system_proxies() -> Option<SystemProxies>;
  }
  pub struct SystemProxies { pub http: Option<(String, u16)>, pub https: Option<(String, u16)>, pub socks: Option<(String, u16)>, pub exceptions: Vec<String>, pub pac_only: bool }
  ```
  `app` 把它换成 `HTTPS_PROXY`、`HTTP_PROXY`、`ALL_PROXY`（`socks5://`）、`NO_PROXY` 放进 `ChildEnv`。第 2 步之前完成；没完成时只用进程环境里的代理。
- **F. `usage`**：
  - `MrSource` 的三个方法可能等子进程最多 30 秒（「数据流与线程」）。`usage` 用 `Executor::run_dedicated` 调用它们，或保证同一时刻最多一个调用在等，不占满工作线程。
  - 错误的含义按本文「MR 来源」：不是代码平台的地址返回 `Ok(None)`；命令行缺失、没登录、离线、被限流且没有缓存、用户关闭了连接，都返回 `Offline`。
  - `ForgeRepo::host` 是命令行主机名（「主机与允许表」），不是远端地址里的主机。
- **G. `app`**：装配三个插件，传入 `ForgeDeps`；`ChildEnv` 按前置条件 E 补上系统代理。
- **H. `forge_view`**（新的视图 crate，#25）：先写自己的设计文档和 `crates/forge_view/AGENTS.md`，再实现。要满足：
  - 侧栏 `Views` 区贡献 Merge Requests 条目；MR 屏幕读 `Forge::snapshot` 和 `Forge::mr`，持有 `watch` / `watch_mr` 期间可见。
  - 远端主机不认识时（`HostState::UnknownHost`）显示主机名，让用户选 GitHub 或 GitLab、确认命令行主机名，确认后发 `ForgeCommand::AddHost`。确认之前不发任何请求。
  - MR 描述、评论、检查名是不受信任的文字：按纯文本或不加载远程资源的 Markdown 显示，不请求其中的图片和链接。网页链接（`web_url`、检查的 `url`）只在用户点击时经 `platform` 用系统浏览器打开，悬停时显示主机名。
  - 收到 `ForgeEvent::Mr` 且 head、合并基点或指纹变了时，对打开中的 MR 审阅发 `ReviewCommand::Refresh`。
  - 读 `usage` 的 `watch(Mr)` 显示 MR 的累计 token。
- **I. `project_home`**（可选，之后的 PR）：会话卡片的「测试结果」信号读 `ForgeSnapshot::mrs` 里与会话 worktree 对应的那一行的 `ci`。

## 概念

| 概念 | 说明 |
|---|---|
| 远端主机 | git 远端地址里的主机，如 `github.com`、`ssh.github.com`、`git.example.com`，或 `~/.ssh/config` 里的别名 |
| 命令行主机名 | 传给 `gh api --hostname` / `glab api --hostname` 的主机名，即用户用 `gh auth login` / `glab auth login` 登录过的主机。`ForgeRepo::host` 用它 |
| 允许表 | 内置的两条映射加上用户确认的映射，把远端主机映射到（平台，命令行主机名）。不在表里的远端主机不发任何请求 |
| 端点 | REST 接口的路径与查询串，如 `repos/acme/app/pulls?state=all&per_page=100`。只由本 crate 从校验过的值拼出 |
| 摘要 | 解析响应体后留下的字段，序列化后存进缓存；不存原始响应体 |
| 合并基点 | MR 的 diff 的旧一侧：GitLab 的 `diff_refs.base_sha`；GitHub 的 `merge_base(base.sha, head.sha)` |
| 降级的改动集 | 提交不在本地时，用代码平台的文件列表组成的改动集：只有路径和状态，没有内容 |

## 接口

### MR 服务（`lectern-services` 的 `forge` 模块）

类型都派生 `Clone`、`PartialEq`、`Eq`、`Debug`、`Serialize`、`Deserialize`（外部标记的枚举），不含 `Arc` 的小类型另派生 `Copy`、`Hash`；trait 和 `ForgeInterest` 除外。时间用 `services::session::Timestamp`（UTC 毫秒）。

```rust
/// 单实现服务，"forge" 插件提供。
/// impl Service for dyn Forge { const ID = "forge"; const MULTIPLE = false; }
pub trait Forge: Send + Sync {
    /// 关注一个项目的 MR 列表：持有期间本 crate 读远端、按「刷新的节奏」保持列表新鲜。
    /// 立即返回，任何线程。第一份快照先由缓存填上，再在后台校验。
    fn watch(&self, project: &ProjectKey) -> ForgeInterest;

    /// 当前快照。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程可调用。没有关注者时为 None。
    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ForgeSnapshot>>;

    /// 关注一个 MR 的详情和 CI：持有期间按 CI 的节奏刷新。立即返回，任何线程。
    fn watch_mr(&self, project: &ProjectKey, mr: &MrRef) -> ForgeInterest;

    /// 当前详情。不阻塞；没有关注者时为 None。
    fn mr(&self, mr: &MrRef) -> Option<Arc<MrDetail>>;

    /// 当前设置。不阻塞。设置文件还没读完时是默认值，读完后发 `ForgeEvent::Settings`。
    fn settings(&self) -> Arc<ForgeSettings>;

    /// 立即返回，任何线程；结果随事件送达。
    fn command(&self, command: ForgeCommand);

    /// 给 "forge-usage" 登记用。不阻塞。
    fn mr_source(&self) -> Arc<dyn MrSource>;

    /// 给 "forge-review" 登记用。不阻塞。
    fn mr_changes(&self) -> Arc<dyn MrChanges>;
}

/// 在 executor 线程上、快照替换之后发出。监听只安排重绘或转发命令，不做 IO。
pub enum ForgeEvent {
    Changed { project: ProjectKey, generation: u64 },
    Mr { mr: MrRef, generation: u64 },
    Settings { generation: u64 },
}

/// `watch` / `watch_mr` 的关注，drop 时注销（任何线程，不阻塞）。不序列化。
pub struct ForgeInterest { /* 实现方传入的注销回调 */ }
impl ForgeInterest { pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self; }

pub enum ForgeCommand {
    /// 立即校验列表（用户要求刷新）。
    Refresh(ProjectKey),
    RefreshMr(MrRef),
    /// 用户确认一个远端主机属于哪个平台、用哪个命令行主机名。写进设置文件。
    AddHost(HostMapping),
    RemoveHost { remote_host: Arc<str> },
    /// 受信任的机器人账号（ADR 0019），按平台主机加账号的不变 ID。整体替换。
    SetTrustedBots(Arc<[ForgeUserKey]>),
    /// 总开关。关闭时不启动任何子进程，快照里每个远端为 `Disabled`。
    SetEnabled(bool),
    /// 用户指定 gh / glab 的路径；None 恢复自动查找。
    SetToolPath { platform: Platform, path: Option<Arc<Path>> },
}

pub enum Platform { GitHub, GitLab }

pub struct HostMapping {
    /// 小写，带非默认端口。
    pub remote_host: Arc<str>,
    pub platform: Platform,
    pub cli_host: Arc<str>,
}

pub struct ForgeSettings {
    pub generation: u64,
    pub enabled: bool,
    /// 用户确认的映射，不含内置的两条。
    pub hosts: Arc<[HostMapping]>,
    pub trusted_bots: Arc<[ForgeUserKey]>,
    pub gh_path: Option<Arc<Path>>,
    pub glab_path: Option<Arc<Path>>,
}

/// 账号的不变 ID。GitHub 是 user id，GitLab 是 user id；只在同一命令行主机上唯一。
pub struct ForgeUserKey { pub host: Arc<str>, pub id: u64 }
pub struct ForgeUser { pub key: ForgeUserKey, /** 只用于显示 */ pub login: Arc<str> }
```

快照：

```rust
/// 不可变。每次变化整体替换。
pub struct ForgeSnapshot {
    pub project: ProjectKey,
    pub generation: u64,
    /// 项目的每个远端的识别结果，按远端名排序。
    pub remotes: Arc<[RemoteStatus]>,
    /// 「MR 列表」：与本项目本地分支对应的 MR，开着的在前，各组内按 `updated_at` 从新到旧。
    pub mrs: Arc<[MrRow]>,
    pub fetch: FetchState,
}

pub struct RemoteStatus {
    pub remote: Arc<str>,
    pub remote_host: Option<Arc<str>>,   // 本地路径、`file://` 为 None
    pub state: HostState,
}

pub enum HostState {
    /// 不是代码平台的地址（本地路径、`file://`、解析不了的地址）。
    NotAForge,
    Disabled,
    /// 不在允许表里。不发任何请求，等用户确认（「对其他 crate 的要求」H）。
    UnknownHost,
    ToolMissing(Platform),
    /// 命令行的 `-i` 输出里没有状态行：版本太旧或行为变了。
    ToolUnsupported(Platform),
    NotLoggedIn { platform: Platform, cli_host: Arc<str> },
    /// 仓库不存在或没有权限（404）。
    RepoNotFound,
    Ready { platform: Platform, repo: ForgeRepo, /** owner/name 或 group/…/name，只用于显示 */ path: Arc<str> },
}

pub enum FetchState {
    /// 还没有缓存，第一次请求进行中。
    Loading,
    Fresh { at: Timestamp },
    /// 显示的是缓存；`why` 说明为什么没能更新。
    Cached { at: Timestamp, why: StaleReason },
    /// 没有缓存，也取不到。
    Unavailable(StaleReason),
}

pub enum StaleReason {
    Refreshing,
    Offline,
    RateLimited { until: Timestamp },
    /// 请求失败的种类（不带文字，「日志与诊断」）。
    Failed(RequestErrorKind),
}

pub enum RequestErrorKind { Timeout, Unreachable, Auth, Forbidden, NotFound, Server, OutputTooLarge, BadResponse, ToolFailed }

pub struct MrRow {
    pub mr: MrRef,
    pub title: Arc<str>,
    pub state: MrState,
    pub draft: bool,
    pub author: ForgeUser,
    pub source: (ForgeRepo, Arc<str>),   // 源仓库与源分支名
    pub target: (ForgeRepo, Arc<str>),   // 目标仓库与目标分支名
    pub head: BlobId,
    /// 对应的本地分支（完整引用名）与检出它的 worktree。
    pub local: Option<LocalMatch>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// 缓存或最近一次取到的 CI 摘要；`head` 与本行的 `head` 不同时视为过期，视图不显示。
    pub ci: Option<CiSummary>,
    pub web_url: Arc<str>,
}

pub enum MrState { Open, Merged, Closed }
pub struct LocalMatch { pub branch: Arc<str>, pub worktree: Option<WorktreeKey> }

pub struct MrDetail {
    pub generation: u64,
    pub row: MrRow,
    /// 不受信任的 Markdown 原文，最多 256 KiB（超出截断，`description_truncated` 为 true）。
    pub description: Arc<str>,
    pub description_truncated: bool,
    pub merge_base: Option<BlobId>,
    /// 代码平台报告的改动文件数（GitLab 的 `changes_count` 为 "1000+" 时为 1000，`changed_files_capped` 为 true）。
    pub changed_files: Option<u32>,
    pub changed_files_capped: bool,
    pub ci: Option<CiStatus>,
    /// 「MR 的指纹」。
    pub fingerprint: [u8; 32],
    pub fetch: FetchState,
}

pub struct CiSummary { pub head: BlobId, pub state: CiState, pub counts: CiCounts }
pub struct CiCounts { pub total: u32, pub success: u32, pub failure: u32, pub running: u32, pub pending: u32, pub other: u32 }
pub enum CiState { None, Pending, Running, Success, Failure, Cancelled, ActionRequired, Neutral }

pub struct CiStatus {
    pub summary: CiSummary,
    /// 按（状态的严重程度，名字）排序，最多 300 条。
    pub checks: Arc<[Check]>,
    pub truncated: bool,
}

pub struct Check {
    /// 不受信任的文字。
    pub name: Arc<str>,
    pub kind: CheckKind,
    pub state: CiState,
    pub url: Option<Arc<str>>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
}
pub enum CheckKind { CheckRun, CommitStatus, Job }
```

调用方：

- `forge_view`：MR 屏幕持有 `watch(project)`，打开 MR 详情时持有 `watch_mr`；主机确认、刷新、设置项发 `command`。
- `project_home`（可选，I）：读 `snapshot(project).mrs` 的 `ci`。
- "forge-usage"、"forge-review"：`mr_source`、`mr_changes`。
- 设置页（另开 issue）：读 `settings`，发 `SetEnabled`、`SetTrustedBots`、`SetToolPath`、`RemoveHost`。

### 改动来源的扩展（`lectern-services` 的 `changes` 模块）

```rust
pub enum SourceKind {
    Worktree { project: ProjectKey, worktree: WorktreeKey, base: BaseSpec },
    /// 代码平台上的一个 MR，旧一侧是合并基点，新一侧是 head。
    Mr { project: ProjectKey, mr: MrRef },
}

pub struct Capabilities {
    pub refresh: bool,
    pub fingerprint: bool,
    /// `fetch`、`shared_analyses` 可能等子进程或网络，最多 30 秒；调用方用 `Executor::run_dedicated`。
    pub remote: bool,
    /// 实现了 `shared_analyses`。
    pub shared_analysis: bool,
}

pub struct ChangeList {
    // 已有字段不变。
    /// 来源只给出了一部分时有值；本地来源总是 None。
    pub partial: Option<Partial>,
}

pub struct Partial {
    /// 两侧内容都没有取到，`files` 来自代码平台的文件列表（每个文件的 detail 为 `NoDiff`）。
    /// 这时 `ChangeList::base` 是合并基点的提交 ID，不是树，消费方不按它读对象。
    pub listed_only: bool,
    /// 代码平台报告改动、但 `files` 里没有的文件数（列表被截断）。
    pub missing_files: u32,
    /// 取数期间 MR 的 head、base 或元数据变了，重取 2 次仍不一致（「MR 的改动集」第 6 步）。
    pub possibly_stale: bool,
}

pub trait ChangeSource: Send + Sync {
    // 已有方法不变。

    /// 能力 `shared_analysis`：这份改动集对应的 MR 上的共享分析（ADR 0019）。阻塞；
    /// `remote` 为 true 时可能等网络。没有这项能力的来源用默认实现。
    fn shared_analyses(&self, list: &ChangeList, cancel: &AtomicBool) -> Result<SharedAnalyses, ChangeError> {
        let _ = (list, cancel);
        Ok(SharedAnalyses { items: Arc::from([]), scan: SharedScan::NotSupported })
    }
}

pub struct SharedAnalyses {
    /// 每个作者最多一条（取 `updated_at` 最新的那条），按 `updated_at` 从新到旧。
    pub items: Arc<[SharedAnalysis]>,
    pub scan: SharedScan,
}

pub enum SharedScan {
    NotSupported,
    Complete,
    /// 读到条数或字节上限仍可能有没读到的评论：显示「共享分析可能未找到」，不当作没有。
    MayHaveMissed,
}

pub struct SharedAnalysis {
    pub comment_id: u64,
    pub author: ForgeUser,
    pub trust: Trust,
    /// 评论里的 `headSha`；解析不出时为 None。
    pub head: Option<BlobId>,
    pub freshness: Freshness,
    pub updated_at: Timestamp,
    /// 评论里 `result` 的 JSON 原文，最多 2 MiB。由 `review` 校验，本 crate 不解读。
    pub result: Arc<str>,
}

pub enum Trust {
    /// 对仓库有写权限（GitHub `admin` / `write`，GitLab access_level ≥ 30）。
    Writer,
    /// 设置里的机器人账号。
    Bot,
    Unverified(UnverifiedReason),
}
pub enum UnverifiedReason { NoWriteAccess, LookupFailed, LookupBudget }

pub enum Freshness {
    /// `head` 与这份改动集的 head 不同。
    Outdated,
    /// `head` 相同；评论没有记录 base 和元数据，仍一律可能过期。
    MaybeStale,
}

/// MR 改动来源的提供者，"forge-review" 经 `Review::register_mr_changes` 登记。不实现 `Service`。
pub trait MrChanges: Send + Sync {
    /// 构造一个来源。不做 IO、不阻塞，任何线程。MR 不属于这个项目的远端仓库、或用户关闭了连接时为 None。
    fn source(&self, project: &ProjectKey, mr: &MrRef) -> Option<Arc<dyn ChangeSource>>;
}

/// drop 时注销（任何线程，不阻塞）。`review` 用 `new` 构造。不序列化。
pub struct MrChangesRegistration { /* 注销回调 */ }
impl MrChangesRegistration { pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self; }
```

本 crate 的 MR 来源：`info()` 的 `caps` 为 `refresh: false`、`fingerprint: true`、`remote: true`、`shared_analysis: true`。`refresh` 用默认实现（重新 `fetch`）：元数据有 ETag，diff 按提交缓存在 `git` 里，重取的代价小。各方法的规则见「MR 的改动集」「共享分析评论」。

### MR 来源（`usage` 的 `MrSource`）

接口在 [usage 设计「接口」](usage.md#接口)，已在 `services::usage` 里。本 crate 的实现规则：

| 方法 | 做什么 | 错误 |
|---|---|---|
| `repo(remote_url)` | 解析地址（「远端地址」），查允许表，查缓存，没有或过期（24 小时）时请求仓库接口 | 不是代码平台的地址、不在允许表：`Ok(None)`；仓库 404：`NotFound`；命令行缺失、没登录、离线、被限流且没有缓存、总开关关闭：`Offline` |
| `mrs_for_branch(repo, branch)` | 先查本项目「MR 列表」的结果，再查按分支的缓存（10 分钟），都没有时按分支请求；对 fork 也查它的上游仓库（「MR 列表」） | 同上；有缓存时失败也返回缓存 |
| `mrs_containing(repo, commit)` | GitHub `commits/{sha}/pulls`，GitLab `repository/commits/{sha}/merge_requests`；缓存 10 分钟 | 同上 |

`MrInfo` 的字段：

- `mr`：目标仓库（MR 所在的仓库）加编号（GitHub 的 PR number，GitLab 的 iid）。
- `source`、`target`：仓库的不变 ID 加分支名。
- `head`：GitHub `head.sha`，GitLab `sha`。
- `bases`：GitHub 只有当前的 `base.sha`（平台不记录历史）；GitLab 是 `versions` 接口里每个版本的 `base_commit_sha`，去重后按版本顺序。
- `merged_target`：已合并的 MR 才有。合并提交（GitHub `merge_commit_sha`，GitLab `merge_commit_sha` 或 `squash_commit_sha`）的第一个父提交；GitLab 快进合并没有这两个字段时取 `diff_refs.start_sha`；都取不到为 None。合并提交的父提交按提交 ID 缓存，不再变。GitHub 的 rebase 合并见「已知限制」。
- `timeline`：创建时刻记为 `Opened`。GitHub 读 `issues/{n}/events` 里的 `closed`、`reopened`、`merged`；GitLab 读 `resource_state_events` 里的 `closed`、`reopened`、`merged`，没有记录时用 MR 的 `closed_at`、`merged_at` 补上。每个 MR 最多读 10 页；缓存按 MR 的 `updated_at` 失效，`updated_at` 不变时不重取。

### 本 crate 的公开项

```rust
pub fn plugins(deps: ForgeDeps) -> [(Name, Box<dyn Plugin>); 3];
pub struct ForgeDeps { /* 见「依赖」 */ }

/// 子进程的环境：白名单变量的值，`app` 读一次后传入（「命令行的调用」）。
pub struct ChildEnv { /* 私有字段 */ }
impl ChildEnv {
    /// 从一组（名字，值）里只保留白名单里的变量。`app` 传 `std::env::vars_os()` 加系统代理。
    pub fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> ChildEnv;
}

#[cfg(feature = "test-support")]
pub mod testing {
    /// 在临时目录里生成假的 gh / glab（POSIX sh 脚本），按端点回放夹具，记录每次调用的参数、
    /// 环境变量、工作目录和标准输入（「测试」）。
    pub struct FakeCli;
}
```

## 数据结构

### 主机与允许表

允许表把远端主机映射到（平台，命令行主机名）：

| 远端主机 | 平台 | 命令行主机名 | 来源 |
|---|---|---|---|
| `github.com`、`ssh.github.com` | GitHub | `github.com` | 内置 |
| `gitlab.com`、`altssh.gitlab.com` | GitLab | `gitlab.com` | 内置 |
| 其他 | 用户选择 | 默认等于远端主机，用户可以改 | `ForgeCommand::AddHost`，写进设置文件 |

规则：

- 只有允许表里的远端主机会收到请求。不在表里的主机（自部署实例、GitHub Enterprise Cloud 的 `*.ghe.com`、`~/.ssh/config` 里的别名）在用户确认之前不启动任何子进程。
- 比较前主机名转成小写，去掉末尾的 `.`；带非默认端口时端口是主机的一部分。不做后缀匹配：`github.com.example.com` 不是 GitHub。
- 允许表只在本 crate 的一个模块里查询。调用命令行的函数只接收 `AllowedHost` 类型的参数，这个类型只能由允许表构造，所以没有经过允许表的主机名到不了命令行（「验收」第 1 步的 `rg` 检查；测试 `unknown_host_spawns_nothing` 与 `FakeCli::assert_safe`）。
- 命令行主机名只决定 gh / glab 用哪个已登录的主机。gh / glab 再把它解析成 API 地址（github.com 是 `api.github.com`，GitHub Enterprise Server 是 `<主机>/api/v3`，GitLab 是 `<主机>/api/v4` 或 glab 配置里的 `api_host`），这一步属于用户自己的命令行配置。
- 响应里的任何 URL（`Link` 头、`url`、`html_url`、`web_url`、头像、附件、检查的详情链接）都不被本 crate 请求。翻页由本 crate 用页码重新拼端点（「翻页」）。
- 令牌只由 gh / glab 发往用户用 `auth login` 登录过的主机：子进程拿不到令牌类的环境变量（「命令行的调用」），所以即使用户确认了一个陌生主机，没有登录过它时请求也不带凭据，结果是 `NotLoggedIn`。
- 远端地址来自项目的 git 配置。能改它的程序已经以用户身份运行，可以运行任意程序，不在威胁模型内（AGENTS.md「Review guidelines」）。改到允许表里的主机（如 github.com 上的另一个仓库）只会读那个仓库里用户本来就能看到的数据，令牌仍只发往 github.com；读到的内容都按不受信任的文字处理。

### 远端地址

`git::Repo::remotes` 给出 fetch URL 和 push URL（已应用 `insteadOf`）。认得的形式：

- `https://[用户@]主机[:端口]/路径`、`http://…`（只用于识别，请求仍经命令行走 HTTPS）、`ssh://[用户@]主机[:端口]/路径`、`git://主机/路径`、`[用户@]主机:路径`（scp 写法，冒号前没有 `/`）。
- 本地路径和 `file://` 为 `NotAForge`。
- 路径去掉开头的 `/`、`~`，末尾的 `/` 和 `.git`。GitHub 必须恰好是 `owner/name` 两段；GitLab 至少两段（`group/sub/…/name`）。每段只允许 `[A-Za-z0-9._-]`，不允许 `.`、`..` 单独成段。不符合的为 `NotAForge`。
- 一个远端的 fetch URL 和 push URL 指向不同的仓库时两个都识别，push URL 只用于「MR 列表」的分支对应。

仓库的不变 ID 由仓库接口给出：GitHub 的 `repos/{owner}/{name}` 的 `id`，GitLab 的 `projects/{路径，按段转义后用 %2F 连接}` 的 `id`。GitHub 对改名的仓库返回重定向，gh 自动跟随，取到的 `full_name` 是新名字，之后的端点用它。fork 的上游：GitHub 的 `parent.id`、`parent.full_name`，GitLab 的 `forked_from_project.id`。

### 命令行的调用

查找命令行，按顺序取第一个存在且可执行的：

1. 设置里的 `gh_path` / `glab_path`。
2. `ChildEnv` 里 `PATH` 的每个绝对路径目录（相对路径和空项跳过）。
3. `/opt/homebrew/bin`、`/usr/local/bin`、`/opt/local/bin`（从 Dock 启动的应用 `PATH` 里没有 Homebrew 的目录）。

以后 Analyze 的 issue 实现「经监护进程查询一次登录 shell 的环境」时（[agents 设计「未决问题」](agents.md#未决问题)1），本 crate 一并使用它的 `PATH`。测试把假命令放在 `PATH` 最前面，第 1 项为空，所以测试只会找到假命令。查找结果每次运行缓存一次，`ToolMissing` 一分钟后重查。

启动规则（每次请求一个子进程）：

- 不经 shell，`std::process::Command` 直接执行找到的绝对路径。
- 环境变量先全部清空，再放入：
  - 白名单（值来自 `ChildEnv`，有才放）：`HOME`、`USER`、`LOGNAME`、`TMPDIR`、`XDG_CONFIG_HOME`、`GH_CONFIG_DIR`、`GLAB_CONFIG_DIR`、`HTTPS_PROXY`、`HTTP_PROXY`、`ALL_PROXY`、`NO_PROXY` 及其小写形式、`SSL_CERT_FILE`、`SSL_CERT_DIR`。
  - 固定值：`PATH=/usr/bin:/bin:/usr/sbin:/sbin`（gh 在 macOS 上经 `/usr/bin/security` 读钥匙串）、`LC_ALL=C`、`NO_COLOR=1`、`GH_NO_UPDATE_NOTIFIER=1`、`GH_NO_EXTENSION_UPDATE_NOTIFIER=1`、`GH_TELEMETRY=false`、`DO_NOT_TRACK=1`、`GH_PROMPT_DISABLED=1`、`GH_SPINNER_DISABLED=1`、`GH_PAGER=cat`、`PAGER=cat`、`GLAB_NO_PROMPT=true`、`NO_PROMPT=true`、`GLAB_CHECK_UPDATE=false`、`GLAB_SEND_TELEMETRY=false`、`GLAB_SHOW_WHATS_NEW=false`、`GLAB_NOTIFY_SKILL_UPDATES=false`、`GIT_CEILING_DIRECTORIES=<scratch_dir>`。
  - 其余一律不传，包括 `GH_TOKEN`、`GITHUB_TOKEN`、`GH_ENTERPRISE_TOKEN`、`GITHUB_ENTERPRISE_TOKEN`、`GITLAB_TOKEN`、`GITLAB_ACCESS_TOKEN`、`OAUTH_TOKEN`、`GH_HOST`、`GH_REPO`、`GITLAB_HOST`、`GITLAB_URI`、`GITLAB_API_HOST`、`GH_DEBUG`、`DEBUG`。`GH_ENTERPRISE_TOKEN` 会发往任何 GitHub Enterprise Server 主机，`GH_HOST`、`GH_REPO` 会改变请求的目标，所以不传。只用环境变量登录的用户要改用 `gh auth login` / `glab auth login`（「已知限制」）。
- 工作目录是 `scratch_dir/cwd/`：一个空目录，不在任何 git 仓库里（`GIT_CEILING_DIRECTORIES` 挡住上层），所以 glab 不会从当前目录推断主机。
- 标准输入为 `/dev/null`。标准输出、标准错误为管道。
- 参数固定为：`api --hostname <命令行主机名> -i -X GET [-H <头>]… -- <端点>`。头只有 `Accept`（GitHub `application/vnd.github+json`）、`X-GitHub-Api-Version: 2022-11-28`（GitHub）和 `If-None-Match: <ETag>`。不用 `-f` / `-F` / `--input`（只发 GET）、`--paginate`（翻页自己做，才能在上限处停）、`--cache`、`--jq`、`--template`、`--verbose`、`--silent`。Lectern 不调用 `api` 以外的子命令，例外是查找时的 `--version`。
- 端点只由本 crate 拼出：数字、OID 的十六进制、校验过的仓库路径段、按 RFC 3986 转义的分支名和时间。以 `-` 开头的值到不了参数（端点前有 `--`，且端点总以字母开头）。
- 每个子进程最长 20 秒（`REQUEST_TIMEOUT`）。超时、取消或输出超限时 `SIGKILL` 并回收。
- 标准输出最多读 8 MiB（`MAX_RESPONSE`，含响应头），超出时终止，结果为 `OutputTooLarge`。标准错误最多读 4 KiB，只用来区分「没有状态行」时的原因，不写进日志、快照和界面。
- 并发：每个命令行主机同时最多 2 个子进程，其中后台请求（`usage` 的对应、轮询、预取）最多 1 个；全局最多 4 个。等待的请求按优先级排队：用户正在看的（`watch_mr` 的详情、`ChangeSource::fetch`）在前。

输出的解析：

| 输出 | 结果 |
|---|---|
| 第一行是 `HTTP/<版本> <三位数> …` | 按状态码处理，不看退出码；读响应头到空行，余下是响应体 |
| 没有状态行，退出码 4（gh 的「需要登录」） | `Auth`，主机状态为 `NotLoggedIn` |
| 没有状态行，退出码其他 | 标准错误里有 `HTTP` 或 `Get "…"`、`dial`、`timeout` 这类网络错误的形式时为 `Unreachable`；否则为 `ToolFailed`。同一主机连续 3 次 `ToolFailed` 且都没有状态行时，主机状态为 `ToolUnsupported` |
| 2xx | 解析响应体（JSON） |
| 304 | 用缓存的摘要，`fetched_at` 更新 |
| 401 | `Auth`，`NotLoggedIn` |
| 403 / 429 带限流信号 | 「限流与退避」 |
| 403 其他 | `Forbidden` |
| 404 | `NotFound` |
| 5xx | `Server`，按退避重试一次 |
| 响应体不是期望的 JSON 结构 | `BadResponse` |

用到的响应头：`ETag`、`Link`（只取 `rel="next"`、`rel="last"` 的页码）、GitHub 的 `X-Ratelimit-Remaining`、`X-Ratelimit-Reset`、`Retry-After`，GitLab 的 `Ratelimit-Remaining`、`Ratelimit-Reset`、`Retry-After`、`X-Next-Page`、`X-Total-Pages`。其他头不保存。

### 用到的接口

GitHub REST，版本 `2022-11-28`（设计时 github.com 的 `X-Github-Api-Version-Selected` 就是它）；GitHub Enterprise Server 用同样的路径。GitLab REST v4，只用 v4 里长期存在的接口（ADR 0008）；`diffs` 在 GitLab 15.7 加入，更早的实例用已弃用但仍在 v4 里的 `changes`（[GitLab MR API](https://docs.gitlab.com/api/merge_requests/)）。响应体只解析表里的字段，其余忽略；字段缺失时按「状态与异常」处理，不让整个请求失败。

| 用途 | GitHub | GitLab |
|---|---|---|
| 当前账号 | `user`：`id`、`login` | `user`：`id`、`username` |
| 实例版本 | — | `version`：`version`（只用于选 `diffs` 或 `changes`） |
| 仓库 | `repos/{o}/{r}`：`id`、`full_name`、`parent.{id,full_name}` | `projects/{路径}`：`id`、`path_with_namespace`、`forked_from_project.id` |
| MR 列表 | `repos/{o}/{r}/pulls?state=all&sort=updated&direction=desc&per_page=100&page=k` | `projects/{id}/merge_requests?state=all&order_by=updated_at&sort=desc&updated_after=<30 天前>&per_page=100&page=k` |
| 按分支 | `repos/{o}/{r}/pulls?head={owner}:{branch}&state=all&per_page=100` | `projects/{id}/merge_requests?source_branch={branch}&state=all&per_page=100` |
| MR 的字段 | `number`、`title`、`state`、`draft`、`created_at`、`updated_at`、`closed_at`、`merged_at`、`user.{id,login}`、`head.{sha,ref,repo.id}`、`base.{sha,ref,repo.id}`、`html_url`；详情另有 `body`、`changed_files`、`merge_commit_sha` | `iid`、`title`、`state`、`draft`、`created_at`、`updated_at`、`closed_at`、`merged_at`、`author.{id,username}`、`source_project_id`、`target_project_id`、`source_branch`、`target_branch`、`sha`、`web_url`；详情另有 `description`、`diff_refs.{base_sha,start_sha,head_sha}`、`changes_count`、`head_pipeline.{id,sha,status}`、`merge_commit_sha`、`squash_commit_sha` |
| 详情 | `repos/{o}/{r}/pulls/{n}` | `projects/{id}/merge_requests/{iid}` |
| 合并基点 | `repos/{o}/{r}/compare/{base}...{head}?per_page=1`：`merge_base_commit.sha` | 详情的 `diff_refs.base_sha` |
| 历史 base | — | `projects/{id}/merge_requests/{iid}/versions`：`base_commit_sha` |
| 降级的文件列表 | `repos/{o}/{r}/pulls/{n}/files?per_page=100&page=k`：`filename`、`previous_filename`、`status`、`sha` | 15.7 起 `…/merge_requests/{iid}/diffs?per_page=100&page=k`，更早 `…/changes`：`old_path`、`new_path`、`new_file`、`deleted_file`、`renamed_file`、`a_mode`、`b_mode`；`changes` 的 `overflow` |
| 时间线 | `repos/{o}/{r}/issues/{n}/events?per_page=100&page=k`：`event`、`created_at` | `…/merge_requests/{iid}/resource_state_events?per_page=100&page=k`：`state`、`created_at` |
| 提交的父提交 | `repos/{o}/{r}/commits/{sha}`：`parents[0].sha` | `projects/{id}/repository/commits/{sha}`：`parent_ids[0]` |
| 提交所属的 MR | `repos/{o}/{r}/commits/{sha}/pulls?per_page=100` | `projects/{id}/repository/commits/{sha}/merge_requests` |
| CI | `repos/{o}/{r}/commits/{sha}/check-runs?per_page=100&page=k`：`name`、`status`、`conclusion`、`started_at`、`completed_at`、`details_url`；`repos/{o}/{r}/commits/{sha}/status?per_page=100`：`statuses[].{context,state,target_url,created_at,updated_at}` | 详情的 `head_pipeline`；`projects/{id}/pipelines/{pid}/jobs?per_page=100&page=k`：`name`、`status`、`started_at`、`finished_at`、`web_url` |
| 评论 | `repos/{o}/{r}/issues/{n}/comments?per_page=100&page=k`；`repos/{o}/{r}/issues/comments?sort=updated&direction=desc&since=<MR 创建时刻>&per_page=100&page=k`；`repos/{o}/{r}/issues/comments/{id}`：`id`、`user.{id,login}`、`issue_url`、`created_at`、`updated_at`、`body` | `…/merge_requests/{iid}/notes?order_by=updated_at&sort=desc&per_page=100&page=k`；`…/notes/{id}`：`id`、`author.{id,username}`、`system`、`created_at`、`updated_at`、`body` |
| 作者的权限 | `repos/{o}/{r}/collaborators/{login}/permission`：`permission`、`user.id` | `projects/{id}/members/all/{user_id}`：`access_level` |

### 翻页

- 只用页码：第 k 页的端点由本 crate 拼出。GitHub 有 `Link` 的 `rel="next"` 时还有下一页，`rel="last"` 的页码是总页数；GitLab 看 `X-Next-Page` 是否为空。`Link` 里的 URL 本身不请求。
- 每类请求有页数上限，到上限时停，结果标为截断（各节写明）。

### 缓存

- 命名空间 `forge.http`（应用分库，前置条件 C）。键：命令行主机名、当前账号的不变 ID、规范化的端点（查询参数按名字排序）。GitLab 列表的 `updated_after` 取 30 天前那一天的 UTC 零点，同一天里查询串不变，ETag 才能命中；GitHub 评论的 `since` 是 MR 的创建时刻，本来就不变。值：`ETag`、取得时刻、摘要的格式版本、摘要（serde 序列化的、表里那些字段）。不存原始响应体和响应头。
- 账号不同的条目互不可见：换了 gh 登录的账号时，旧账号的缓存不会显示给新账号。账号 ID 每次运行第一次用到这个主机时请求 `user` 取得，401 后重取。
- 摘要里的文字（标题、描述、评论里取出的 `result`）会落在缓存目录里，与 gh 自己的缓存一样属于本机数据。评论只缓存 pulls.review 评论的摘要（ID、作者、`updated_at`、`headSha`、`result` 原文），其他评论只缓存 ID 和 `updated_at`，不缓存正文。
- 不会再变的结果没有 ETag 也不过期：提交的父提交、`compare` 的合并基点（按两端提交 ID 为键）。
- `cache` 不可用（磁盘满、分库损坏）时只用内存里的结果，不影响请求（cache 设计「生命周期」）。

### 限流与退避

每个命令行主机一份状态，只在内存里：

- 从每个响应的限流头更新剩余次数和重置时刻。GitHub 剩余 ≤ 100、GitLab 剩余 ≤ 20 时，后台请求暂停到重置时刻，用户正在看的请求照常发。
- 剩余为 0、429，或 403 带 `Retry-After` 或剩余为 0 的头：这个主机全部暂停到 `Retry-After` 或重置时刻；两者都没有时暂停 60 秒，连续出现时翻倍，最长 15 分钟。期间快照为 `Cached { why: RateLimited { until } }` 或 `Unavailable(RateLimited)`。
- `Unreachable`、`Timeout`：这个主机暂停 30 秒，连续出现时翻倍，最长 10 分钟，状态为 `Offline`。用户发 `Refresh` / `RefreshMr` 时立即重试一次。
- 任何一次成功（含 304）清零退避。

### MR 列表

对一个项目：

1. 读 `Repo::branches` 和 `remotes`。每个本地分支取上游和推送目标的远端地址与远端分支名（[git 设计](git.md)的 `BranchInfo`），经「远端地址」和允许表得到（仓库，远端分支名）。检出在 worktree 里的分支从 `Projects` 快照的 `Worktree::head` 得到。
2. 对每个 `Ready` 的仓库读最近更新的 MR：最多 3 页（300 条），读到 `updated_at` 早于 30 天前即停。仓库是 fork 时，也读上游仓库的列表，只留 head 仓库是这个 fork 的。
3. MR 的（源仓库 ID，源分支名）等于某个本地分支的（上游或推送目标的仓库 ID，远端分支名）时对应上；不只比较分支名（ADR 0020）。
4. 检出在 worktree 里、第 2 步没有对应上的分支，每个再按分支查一次（每次刷新最多 20 个），覆盖更新较早的 MR。
5. `mrs` 是对应上的 MR（开着的，加上 30 天内合并或关闭的）。范围是否扩大见「未决问题」1。
6. 列表显示之后，在后台为最多 5 个开着的、对应上的 MR 取 CI 摘要（GitHub 的 check run 与 commit status 各一次，GitLab 的详情一次），按 `updated_at` 从新到旧。

刷新的节奏（只在有关注者时）：

| 对象 | 何时请求 |
|---|---|
| 仓库映射 | 缓存超过 24 小时 |
| MR 列表 | `watch` 开始、`Refresh`、之后每 120 秒；`Projects` 报告分支或 HEAD 变化后 1 秒（去抖） |
| MR 详情 | `watch_mr` 开始、`RefreshMr`、之后每 60 秒 |
| CI | MR 的 CI 还没结束时每 30 秒，10 分钟后每 60 秒，30 分钟后每 120 秒；结束后不再轮询，head 变了重新开始 |

所有校验都带 `If-None-Match`。没有关注者时不轮询；`usage` 的 `MrSource` 调用按自己的需要请求（「MR 来源」）。

### MR 的改动集

`MrChanges::source(project, mr)` 构造一个 `MrChangeSource`，不做 IO。`fetch(cancel)`：

1. 取 MR 详情。`watch_mr` 的详情在 30 秒内取到过时直接用；否则发一次条件请求。
2. 合并基点：GitLab 用 `diff_refs.base_sha`。GitHub 的 `base.sha` 和 `head.sha` 都在项目的对象库里时用 `Repo::merge_base`，否则请求 `compare`，结果按（base，head）缓存。
3. 提交是否在本地：对项目的仓库（`ProjectKey::Git` 的公共目录）调用 `Repo::commit(head)` 和 `Repo::commit(合并基点)`；两个都成功，且 OID 长度与仓库的哈希算法相同时，走本地路径，否则走降级路径。
4. 本地路径：`Repo::diff_trees(合并基点, head, limits)`，`git::change_list_of`（前置条件 B）转成 `ChangeList`，`partial` 为 None。`limits` 与本地审阅相同（[git 设计「上限与截断」](git.md#上限与截断)）。diff 只取决于这两个提交 ID，而两个 ID 来自第 1 步的同一个响应，所以本地路径不做第 6 步的一致性检查。
5. 降级路径：请求代码平台的文件列表（GitHub 最多 30 页、3000 个文件；GitLab 的 `diffs` 最多 30 页，`changes` 一次）。每个文件一条 `FileChange`：状态按平台的字段映射（`added` / `new_file` 为 `Added`，`removed` / `deleted_file` 为 `Deleted`，`renamed` / `renamed_file` 为 `Renamed`（相似度记 0），其余为 `Modified`）；GitHub 给了 `sha` 时新一侧为 `Blob(sha)`，其余一侧为 `Unavailable(Missing)`；`detail` 为 `NoDiff`；按路径的字节序排序。`partial` 为 `listed_only: true`，`missing_files` = 详情里的改动文件数减去列表里的文件数（GitLab 的 `overflow` 为 true、或 `changes_count` 为 "1000+" 时至少为 1）。路径不满足 `ChangePath` 不变量的记进 `totals.invalid_paths`。
6. 降级路径要先后读两次元数据：开始前记下（head，合并基点，标题与描述的哈希），文件列表读完后再取一次（条件请求，通常是 304）。不一致时从第 1 步重来，最多 2 次（`MAX_CONSISTENCY_RETRIES`）；仍不一致时返回最后一次的结果，`partial.possibly_stale` 为 true，这份结果不写缓存。不把不同版本的数据拼在一起。
7. `generation` 在同一个来源上递增。`cancel` 置位时在子进程之间检查，正在运行的子进程被终止，返回 `Cancelled`。

`texts(list, file, max_bytes)`：本地路径用 `git::list_texts`；降级路径两侧都是 `SideText::Unavailable`。`texts` 不访问网络。

### MR 的指纹

`fingerprint(list)` 是下面各项按顺序、带长度前缀拼接后的 SHA-256：命令行主机名、仓库 ID、MR 编号、head、合并基点、SHA-256(标题)、SHA-256(描述)。各项是 `fetch` 时记在 `ChangeList::handle` 里的值，所以 `fingerprint` 不做 IO、不阻塞。任一项变了，缓存的分析结果按过期处理（ADR 0019 约束 9）。`MrDetail::fingerprint` 用同样的算法；合并基点还不知道时这一项为空。

### CI 状态

GitHub：check run（最多 3 页）和 commit status（1 页，最多 100 条）合并。GitLab：详情的 `head_pipeline` 加它的 job（最多 3 页）。超过时 `truncated` 为 true。

| GitHub check run | GitHub commit status | GitLab | `CiState` |
|---|---|---|---|
| `queued`、`waiting`、`requested`、`pending` | `pending` | `created`、`waiting_for_resource`、`preparing`、`pending`、`scheduled` | `Pending` |
| `in_progress` | — | `running` | `Running` |
| `success` | `success` | `success` | `Success` |
| `failure`、`timed_out`、`startup_failure` | `failure`、`error` | `failed` | `Failure` |
| `cancelled` | — | `canceled` | `Cancelled` |
| `action_required` | — | `manual` | `ActionRequired` |
| `neutral`、`skipped`、`stale` | — | `skipped` | `Neutral` |

不认识的值为 `Neutral`。摘要：有 `Failure` 为 `Failure`；否则有 `Running` 为 `Running`；否则有 `Pending` 为 `Pending`；否则有 `ActionRequired` 为 `ActionRequired`；否则有 `Cancelled` 为 `Cancelled`；否则有 `Success` 为 `Success`；有检查但都是 `Neutral` 为 `Neutral`；没有检查为 `None`。「CI 还没结束」指摘要是 `Pending` 或 `Running`。必需检查（分支保护）要仓库管理员权限才能读，不区分。

### 共享分析评论

规则来自 `crates/forge/AGENTS.md`「边界」和 ADR 0019，常量如下：

| 常量 | 值 |
|---|---|
| `MAX_COMMENTS` | 1000 条（按创建顺序的读取） |
| `MAX_COMMENT_BYTES` | 累计 32 MiB（原始响应体） |
| `MAX_COMMENT_BODY` | 单条正文 2 MiB，超过的跳过 |
| `MAX_UPDATED_SCAN` | 按更新时间的读取另计 5000 条、64 MiB |
| `MAX_DECOMPRESSED` | 解压后 2 MiB，超过即停止并丢弃 |
| `MAX_NEW_AUTHOR_CHECKS` | 每次读取最多新查 20 个作者的权限 |
| `NEGATIVE_PERMISSION_TTL` | 否定的权限结果缓存 24 小时 |

读取（`shared_analyses`）：

1. 按创建顺序从最新往前读：GitHub 先读第 1 页取得总页数，再从最后一页往前；GitLab 的 notes 本身按 `updated_at` 从新到旧，这一项与第 2 项合为一次读取。到 `MAX_COMMENTS` 或 `MAX_COMMENT_BYTES` 时停止翻页。
2. 按更新时间从新到旧读（pulls.review 原地更新同一条评论，旧评论的创建时间可能很早）：GitHub 用仓库级的评论接口，`since` 为 MR 的创建时刻，只留 `issue_url` 是这个 PR 的；到 `MAX_UPDATED_SCAN` 时停。不用作者可设置的提交时间做下界。
3. 以前找到过的评论 ID 直接按 ID 重读。
4. 第 1、2 项任何一项到了上限，`scan` 为 `MayHaveMissed`。
5. 只看第一行是 `<!-- pulls.review data -->` 的评论；系统评论（GitLab `system`）跳过。正文按 [pulls.review 调研「共享分析评论」](../research/pulls-review.md#共享分析评论)的格式取出 JSON 代码块 `{ headSha, result }`；代码块是 lz-string 的 base64 时按流解码，输出到 `MAX_DECOMPRESSED` 即停止并丢弃这条。JSON 解析失败、缺字段的忽略，不报错。`result` 的 JSON 原文交给 `review`，本 crate 不解读它的结构。
6. 每个作者只留 `updated_at` 最新的一条。
7. 作者是否受信任，按顺序判断：
   - 是设置里的机器人账号（命令行主机名加账号的不变 ID 相同，不比用户名）：`Bot`。
   - 权限缓存（键：命令行主机名、仓库 ID、作者 ID）里有否定结果且未满 24 小时：`Unverified(NoWriteAccess)`；有肯定结果、且检查时刻晚于这条评论的 `updated_at`：`Writer`。
   - 否则新查一次（每次读取最多 `MAX_NEW_AUTHOR_CHECKS` 个，超出的为 `Unverified(LookupBudget)`）。GitHub 的 `permission` 为 `admin` 或 `write`、且响应的 `user.id` 等于评论作者的 ID 时为 `Writer`；GitLab 的 `access_level` ≥ 30 时为 `Writer`。其余为 `NoWriteAccess`。请求失败（含调用者自己没有权限读成员信息的 403、404）为 `LookupFailed`，不缓存。
8. `head` 与改动集的 head 不同为 `Outdated`，相同也为 `MaybeStale`。
9. 与「MR 的改动集」第 6 步相同，读评论前后各读一次元数据；不一致时重来，最多 2 次，仍不一致时这次结果不缓存。

评论正文、`result` 原文都是不受信任的外部输入：本 crate 只做有上限的取出，不执行、不跟随其中的任何链接，也不把它们当作指令。

### 设置

`data_dir/settings.json`，用户的决定，不能重建，所以不放进 `cache`：

```json
{ "version": 1, "enabled": true,
  "hosts": [{ "remote_host": "git.example.com", "platform": "gitlab", "cli_host": "git.example.com" }],
  "trusted_bots": [{ "host": "github.com", "id": 41898282 }],
  "gh_path": null, "glab_path": null }
```

- "forge" 插件的 `started` 里在后台任务读一次。文件不存在时用默认值（`enabled: true`，其余为空），不创建文件。版本不认识或解析失败时用默认值、记 warn，不覆盖原文件，直到用户改了设置。
- 写入：同目录的临时文件、`fsync`、`rename`，在后台任务里串行进行（与 `welcome` 的 `recent.json` 相同）。命令改内存里的设置后立即发 `ForgeEvent::Settings`，写盘在后台。
- 移除一个主机映射后，这个主机的缓存条目留给 `cache` 的 LRU 淘汰，不再被读取（键里有命令行主机名，允许表里没有它就不会查）。

## 数据流与线程

```
Projects 快照 ──┐
                ├─▶ [Background] 读 remotes / branches ─▶ 允许表 ─▶ 请求队列（每主机 ≤ 2）
watch / watch_mr┘                                              │
                                                               ▼
                     [专用线程] 启动子进程、读管道、解析 -i 输出 ─▶ 摘要 ─▶ cache
                                                               │
                                     换出新快照（取锁换 Arc）◀──┘ ─▶ ForgeEvent
```

- UI 线程只调用 `snapshot`、`mr`、`settings`（取锁复制 `Arc`）、`watch` / `watch_mr`（登记关注、提交任务）和 `command`（改内存、提交任务）。
- 读仓库（`git`）、读缓存、解析 JSON 在 `Background` 优先级的 executor 任务里；用户正在看的详情和 `ChangeSource::fetch` 用 `Interactive`。
- 每个子进程由 `run_dedicated_gated` 的专用线程启动和读管道，进程结束后线程退出；空闲时没有常驻线程和子进程。请求进行期间持有 `BusyGuard`，所以 kernel 不报告静止；轮询的等待不持有。
- 定时用 kernel 的 `Clock::wake_at`：回调只改内存里的状态、提交任务，自己不做 IO。
- 阻塞的接口（`MrSource` 的三个方法、`ChangeSource::fetch`、`shared_analyses`）把请求放进队列，在调用线程上等结果，每 50 ms 检查一次取消标志；一次调用最多等 30 秒（`WAIT_BUDGET`），超时时有缓存返回缓存，否则 `MrSource` 返回 `Offline`、`ChangeSource` 返回 `ChangeError::Io("timeout")`。调用方按前置条件 D、F 用专用线程调用。
- 首屏先显示缓存：`watch` 开始时在后台读 `forge.http` 里这个项目各端点的摘要，先换出一份 `Cached` 快照，再发条件请求。
- 渐进加载：MR 详情先取详情（GitHub 另取 check run 与 commit status），共享分析在详情显示之后才开始读，它的几十个请求不挡住详情和 diff。
- 派生数据：MR 的 diff 由 `git` 按两端 blob OID 缓存 hunk（架构规则 5），与本地审阅共用。forge 自己的缓存是远端数据的摘要，按端点和账号为键，不是派生数据。
- 项目关闭（`ProjectEvent::Closed`）时丢弃这个项目的快照、队列里属于它的请求和计时；正在运行的子进程被终止。

## 性能

- 首帧之前不做任何事。三个插件的激活条件都是 `AfterFirstFrame`，`activate` 只登记；`started` 只提交一个读设置文件的后台任务，不启动子进程。三个插件的激活四段之和合计 < 0.3 ms，计入 `plugin/after_first_frame` 的 5 ms（[bench 设计](bench.md)）。
- 空闲内存：没有关注者时，内存里只有每个打开项目的远端识别结果和最近一份 MR 列表（≤ 300 行，约 100 KB）。MR 详情和共享分析的 `result` 在最后一个关注者离开 60 秒后释放；详情最多保留 16 个，`result` 合计最多 8 MiB。新增 `plugin/forge/resident` < 1 MB（打开一个有 GitHub 远端的项目、没有关注者时，假命令回放）。
- 子进程：空闲时为 0。每个子进程是 gh / glab 自己的内存，存在时间以秒计，不计入空闲内存。
- 交互路径：MR 详情在 30 秒内取到过、提交在本地时，`ChangeSource::fetch` 不访问网络，forge 在 `diff_trees` 之外的开销 < 1 ms，打开 MR 的 diff 与 `app/open_diff`（< 50 ms）走同一条本地路径。网络请求的耗时取决于网络（「访问方式的选择」的实测），不设预算；首屏靠缓存。
- 新增进程内基准（`bench` 的 `time` 目标，只在本 crate 的 fixtures 上）：
  - `forge/parse_pulls_page`：解析一页 100 个 PR 的列表 JSON（约 2 MB），中位数 < 10 ms。
  - `forge/parse_include_8mib`：解析 8 MiB 的 `-i` 输出（状态行、头、体的切分），中位数 < 5 ms。
  - `forge/lz_decode_2mib`：解出 2 MiB 输出的 lz-string 输入，中位数 < 30 ms。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)，视图怎样显示由 `forge_view` 决定：

| 情况 | 本 crate 的表现 |
|---|---|
| 不是 git 仓库、没有远端、远端是本地路径 | 快照的 `remotes` 为空或 `NotAForge`，`mrs` 为空，不发请求 |
| 远端主机不在允许表里 | `UnknownHost`，不发请求 |
| 用户关闭了连接 | 每个远端为 `Disabled`，不发请求，`MrSource` 返回 `Offline` |
| 找不到 gh / glab | `ToolMissing`，一分钟后重查 |
| 命令行输出没有状态行（版本太旧） | `ToolUnsupported` |
| 没登录这个主机 | `NotLoggedIn { cli_host }`；用户在终端登录后，下一次刷新恢复 |
| 仓库 404 | `RepoNotFound` |
| 离线、超时 | 有缓存时 `Cached { why: Offline }`，没有时 `Unavailable(Offline)`；按退避重试 |
| 被限流 | `RateLimited { until }`，到时恢复 |
| 提交不在本地 | 降级的改动集，`partial.listed_only`，说明缺少的文件数 |
| 文件列表也被截断 | `partial.missing_files` > 0 |
| 取数期间 MR 变了，重取 2 次仍不一致 | `partial.possibly_stale`，不缓存 |
| 共享分析读到上限 | `SharedScan::MayHaveMissed`（「共享分析可能未找到」） |
| 作者权限查不到或超出本次查询数 | `Unverified(LookupFailed / LookupBudget)`（「部分作者未验证」由视图按条目判断） |
| 响应体缺字段或结构不对 | 这一项为 `BadResponse`；列表里单个 MR 缺字段时跳过这一行，其余照常 |
| 设置文件损坏 | 用默认值，记 warn，不覆盖 |

## 日志与诊断

规则见 [ADR 0046](../adr/0046.md) 和 [logging 设计「隐私」](logging.md#隐私)。全部经 `lectern-logging` 的宏写出，target 为 `forge`。

- 默认记录：
  - info `forge tool found`：每次运行每个命令行一条。`platform`、`path`（`Value::path`）、`major`、`minor`、`patch`（`--version` 输出第一行里的版本号，解析成三个整数；`Value` 不接受运行时字符串，所以不写原文；解析不出时三项都是 0）。
  - info `forge host state`：主机状态变化时一条，`platform`、`state`（`HostState` 的种类，不带主机名之外的文字）、`host`（命令行主机名的 `Value::url`，形如 `https://github.com/`）。
  - info `forge mr changes`：每次 `ChangeSource::fetch` 完成，`path`（`local` / `listed`）、`files`、`missing`、`retries`、`took_us`、`review`（审阅对象的哈希，由调用方的作用域带上）。
  - warn `forge request failed`：`kind`（`RequestErrorKind`）、`status`（有状态行时）、`exit`（退出码）、`url`、`took_us`、`req`。
  - warn `forge rate limited`：`host`、`retry_after_s`。
  - warn `forge inconsistent read`：重取 2 次仍不一致，`what`（`changes` / `shared`）。
  - warn `forge settings unreadable`：`reason`（`parse` / `version` / `io`），带 `io` 时用 `Value::io`。
  - warn `forge shared scan capped`：`which`（`created` / `updated`）、`comments`、`bytes`。
- 详细级别：
  - debug `forge request`：每个请求一条，按 logging 设计「隐私」的子进程和网络请求两行：`program`（`gh` / `glab`，不含目录）、`argc`、`cwd`（`Value::path`）、`pid`、`exit`、`method`、`url`、`status`、`bytes`（标准输出字节数）、`cached`（304）、`took_us`、`req`。`url` 是 `Value::url("https://<命令行主机名>/<端点的路径>")`：查询串（分支名、时间都在这里）被 `Value::url` 去掉；路径里的仓库路径与文件路径同类，可以记录；作者权限端点里的用户名段由本 crate 先换成 `:user` 再交给 `Value::url`。
  - debug `forge wait`：阻塞接口等待的时长和结果种类。
  - trace：队列长度、退避的计时、每页的条数。
- 关联 ID：每个请求一个 `req`（进程内从 1 递增），由本 crate 在请求的整个处理期间用 `scope` 设置；插件代码里的记录另带 kernel 设置的 `plugin`、`act`；`ChangeSource` 的调用带调用方（`review`）的 `review`。
- 不记录：令牌和凭据（本 crate 拿不到，也不调用 `auth` 子命令）；子进程的参数（端点里有分支名）和环境变量的值；标准错误的文字；响应头（`X-Oauth-Scopes` 这类）和响应体；MR 标题、描述、评论正文、`result`、检查名、分支名、提交说明、用户名。用什么代替：URL 只到路径（`Value::url` 去掉查询串，分支名都在查询串里）、OID、条数、字节数、状态码、种类。
- 金丝雀测试：`crates/forge/src/tests/logs.rs` 的 `canary_stays_out_of_forge_logs`：在 trace 级别的 `testing::capture()` 下，金丝雀放进假命令回放的 MR 标题、描述、评论正文、`result`、检查名、分支名、用户名、响应头、标准错误，以及父进程环境里的 `GH_TOKEN`、`GITLAB_TOKEN`；跑完列表、详情、CI、改动集、共享分析、失败与限流的流程后 `assert_absent`。同一测试检查假命令记下的环境里没有这两个变量。

## 测试

全部在 Linux 和 macOS 上运行（`just test -p lectern-forge`），不依赖真实的 gh、glab、网络和登录状态。

- 假命令（`testing::FakeCli`）：测试在临时目录里生成 POSIX sh 脚本 `gh`、`glab`，放在 `ChildEnv` 的 `PATH` 最前面（第 1 项的设置路径为空）。脚本从参数里取出 `--hostname` 和端点，按 `routes/<主机>/<端点的转义>` 找夹具，输出其中的 `-i` 文本、按其中的退出码退出；`If-None-Match` 与夹具的 ETag 相同时输出 304。每次调用把参数（NUL 分隔）、全部环境变量、工作目录和标准输入记进 `calls/<序号>/`。没有对应夹具时以 1 退出、不输出状态行。
- 每个测试结束时的检查（`FakeCli::assert_safe`）：每次调用的 `--hostname` 都在这个测试的允许表里；参数里没有 `auth`、`--verbose`、`--paginate`、`--cache`、`-f`、`-F`、`--input`；环境变量正好是白名单里有值的那些加固定值；工作目录是 `scratch_dir/cwd/`。
- 夹具（`crates/forge/tests/fixtures/`）：按「用到的接口」手写的合成 JSON，`github/` 与 `gitlab/` 各一套，响应头按设计时核对过的格式写（「访问方式的选择」）。`README.md` 写明每个文件对应的文档版本。lz-string 的测试向量由 lz-string 1.5.0 的参考实现生成一次（`pnpm dlx`，生成脚本在 `fixtures/scripts/`），输入是合成文字。不含任何真实仓库、账号和会话日志的数据。
- 单元测试（`#[cfg(test)] mod tests`，结构化结果用 insta 快照）：
  - `remote_url_*`：各种地址形式、`.git` 与 `/`、端口、别名、非法路径段。
  - `allowlist_*`：内置映射、用户映射、后缀不匹配、大小写和末尾的点；`unknown_host_spawns_nothing`：远端是 `git.example.test`、没有映射时假命令没有任何调用。
  - `runner_*`：`-i` 输出的切分、304 与 404 的退出码 1、没有状态行时退出码 4 与其他、20 秒超时被终止（假命令 `sleep`）、8 MiB 截止、取消时终止、并发上限。`AllowedHost` 只能由允许表构造，这一点由「验收」第 1 步的 `rg` 检查保证。
  - `env_*`：白名单、固定值、令牌变量不传、代理变量的大小写。
  - `rate_*`：剩余次数的阈值、`Retry-After`、翻倍与上限、304 清零。
  - `github_*`、`gitlab_*`：列表、按分支、fork 上游、详情、合并基点、降级的文件列表（含 3000 个文件的截断、GitLab 的 `overflow` 与 `changes_count` 为 "1000+"）、时间线、合并前的目标分支、CI 的状态映射与摘要（表的每一行一个用例）。
  - `cache_*`：账号隔离、`updated_after` 在同一天里不变、304 用摘要、不变的结果不过期。
  - `changes_*`：本地路径（`git::testing::TestRepo` 建两个提交）与 `change_list_of` 的结果相同；提交不在本地、哈希算法不同时走降级路径；`consistency_*`：元数据在文件列表之后变了时重取，两次都变时 `possibly_stale` 且不写缓存；`fingerprint_*`：各项变化都改变指纹。
  - `shared_*`：标记行、JSON 代码块、lz-string、解压超过 2 MiB 丢弃、单条超过 2 MiB 跳过、1000 条与 32 MiB 停止、按更新时间的 5000 条与 64 MiB、按 ID 重读、每个作者一条、`Outdated` 与 `MaybeStale`；`trust_*`：机器人按 ID 不按用户名、`admin` / `write` / `read`、`user.id` 不一致、GitLab 29 与 30、否定缓存 24 小时、肯定结果对之后更新的评论无效、每次最多新查 20 个、403 为 `LookupFailed`。
  - `lz_*`：参考向量逐个相同；截断的输入、非法字符返回错误。
  - `mr_source_*`：三个方法的缓存、错误的对照表、`MrInfo` 各字段。
  - `settings_*`：默认值、损坏不覆盖、命令后写盘。
- 插件（`lectern-kernel` 的 `TestKernel`）：`plugin_*`：三个插件激活，`forge-usage` 调用了 `register_mr_source`、`forge-review` 调用了 `register_mr_changes`（假的 `Usage`、`Review`），`forge` 停用时两个登记都被 drop；`started` 之后没有关注者时假命令没有调用。
- 运行日志：`logs_*` 和金丝雀测试（「日志与诊断」）。

## 已知限制

- 只用环境变量（`GH_TOKEN`、`GITLAB_TOKEN`）登录、没有 `gh auth login` / `glab auth login` 的用户连不上，要先在终端登录。
- 只配置了 PAC 的系统代理拿不到具体的代理地址，这类用户要在环境里设置代理变量。
- 每个请求一个子进程和一次 TLS 握手，慢网络上一个请求约 1 秒；首屏靠缓存，详情和共享分析逐步出现。
- gh / glab 以后的版本新增其他会联网的行为时，环境变量不一定能关掉；遥测、更新检查的开关按设计时的版本核对。
- GitHub 不记录 PR 的历史 base，`MrInfo::bases` 只有当前的 `base.sha`。
- GitHub 的 rebase 合并（PR 有多个提交）时，`merge_commit_sha` 是 rebase 后的最后一个提交，它的第一个父提交是同一次 rebase 产生的前一个提交，所以 `merged_target` 比合并前的目标分支晚几个提交。REST 接口不说明合并方式，这一项不区分。
- 一个 MR 的状态事件超过 10 页时，`timeline` 只含前 10 页和 MR 的 `closed_at`、`merged_at`。
- `mrs_containing` 用平台的「提交所属的 MR」接口：GitHub 对不在默认分支上的提交只返回开着的 PR。
- 降级的改动集没有内容和 hunk，只能看路径；已看标记以 `Unavailable(Missing)` 作为一侧的身份。
- 没有写权限的用户查不到其他作者的权限，共享分析都按未验证显示。
- 仓库的哈希算法与代码平台不同（SHA-256 仓库对 GitHub）时总走降级路径。

## 未决问题

需要产品决定，设计按默认选项实现：

1. **MR 列表的范围。**
   - (a) 只列与本项目本地分支对应的 MR：开着的，加上 30 天内合并或关闭的（默认）。Lectern 审的是本机 agent 写的改动，这些 MR 都来自本地分支。
   - (b) 再加上请求我审阅的 MR（GitHub `review-requested:@me`，GitLab `reviewer_id`）。
   - (c) 仓库里全部开着的 MR。大仓库要翻很多页，每页约 1 秒。
2. **MR 的提交不在本地时，Lectern 是否自己取回。**
   - (a) 不取：显示代码平台的文件列表，标「部分内容未提供」，提示用户在终端 `git fetch`；提交到了本地后自动换成完整的 diff（默认）。
   - (b) 用户点「取回提交」后，Lectern 用 git 命令行把 MR 的 head 和合并基点取进 Lectern 自己的裸仓库（放在缓存目录，`objects/info/alternates` 指向项目的对象库，不写用户的仓库；按字节 LRU 封顶），再在本地算 diff。要另外处理 git 的凭据（ssh-agent、凭据助手）和没有命令行工具时的安装对话框。
   - (c) 自动在后台取回。
3. **没有打开 MR 屏幕时访问代码平台的范围。**
   - (a) 为用量的 MR 关联（ADR 0020）在后台请求仓库映射和分支对应的 MR；CI 只在 MR 屏幕或其他显示它的地方可见时轮询；设置里可以整体关闭（默认）。
   - (b) 只在用户打开 MR 屏幕之后才访问；关闭前的用量按分支显示。

## 验收

#25 的领域部分分五步，每步一个 PR，都写 `Refs #25`；#25 由 `forge_view` 的最后一个实现 PR 关闭（AGENTS.md「工作方式」）。每一步都要 `just check`、`just test -p lectern-forge`、`just deny` 通过。

前置条件（各自的 PR）：

- A. `services`：`rg -n 'pub trait Forge\b|pub trait MrChanges' crates/services/src` 各有一处；`rg -n 'Mr \{' crates/services/src/changes` 有 `SourceKind::Mr`；`forge_*`、`changes_*` 的类型测试通过；`crates/services/AGENTS.md` 列出 `forge` 模块（第 2 步之前）。
- B. `git`：`rg -n 'pub fn change_list_of|pub fn list_texts' crates/git/src` 各有一处；`change_source_*` 通过（第 4 步之前）。
- C. `cache`：cache 设计的命名空间表有 `forge.http` 一行（随第 2 步）。
- D. `review`：`ReviewTarget::Mr` 与 `register_mr_changes`（第 4 步之前）；共享分析的校验（第 5 步之后，依赖 #66）。
- E. `platform`：`rg -n 'pub fn system_proxies' crates/platform/src` 有一处（第 2 步之前；没有时只用进程环境里的代理）。
- F. `usage`：`MrSource` 在专用线程上调用（第 3 步之前）。
- G. `app`：装配三个插件（分别随第 2、3、4 步）。
- H. `forge_view`：设计文档 PR，之后的实现 PR（第 2 步之后）。

步骤：

1. **主机、命令行与请求**：远端地址、允许表、设置文件、查找命令行、启动规则与环境变量、`-i` 输出的解析、超时与输出上限、并发、限流与退避、`FakeCli`、运行日志。不依赖 `services` 的新类型。
   - `remote_url_*`、`allowlist_*`、`unknown_host_spawns_nothing`、`runner_*`、`env_*`、`rate_*`、`settings_*`、`logs_*` 通过。
   - `rg -n 'std::process::Command' crates/forge/src` 只在 `src/runner/spawn.rs`。
   - `AllowedHost`（字段私有）和允许表的查询在同一个文件 `src/hosts/allowed.rs` 里，没有其他构造方法：`rg -n 'AllowedHost \{' crates/forge/src` 只在这个文件里，`rg -n 'fn .*-> AllowedHost' crates/forge/src` 只有允许表的查询函数。
   - `rg -n '"auth"|"--verbose"|"--paginate"|"--cache"' crates/forge/src` 只在 `FakeCli::assert_safe` 的禁用列表里。
2. **MR 列表、详情与 CI**：两个平台的仓库、账号、列表、按分支、详情、CI；`forge.http` 缓存；`dyn Forge` 与 "forge" 插件；`Cargo.toml` 加 `plugin = true`。
   - `github_*`、`gitlab_*`（时间线、文件列表除外）、`cache_*`、`plugin_*`（`forge` 部分）、金丝雀测试通过。
   - `cargo tree -p lectern-forge -e normal | rg gpui` 没有结果。
   - `plugin/forge/resident`、`forge/parse_pulls_page`、`forge/parse_include_8mib` 加进 bench 预算表并生效。
3. **MR 来源**：`MrSource` 与 "forge-usage" 插件：时间线、历史 base、合并前的目标分支、提交所属的 MR。
   - `mr_source_*`、`github_timeline_*`、`gitlab_timeline_*`、`plugin_*`（`forge-usage` 部分）通过。
4. **MR 的改动集**：`MrChangeSource`、`MrChanges` 与 "forge-review" 插件：合并基点、本地路径、降级路径、一致性检查、指纹。
   - `changes_*`、`consistency_*`、`fingerprint_*`、`github_files_*`、`gitlab_files_*`、`plugin_*`（`forge-review` 部分）通过。
5. **共享分析评论**：两种读取、按 ID 重读、上限、lz-string、作者信任、过期判断、一致性检查。
   - `shared_*`、`trust_*`、`lz_*` 通过；`forge/lz_decode_2mib` 加进 bench 预算表并生效。
