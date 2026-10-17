# agents 设计

状态：实现中。第 1–3 步已实现（第 1 步 `services` 的会话接口 [#71](https://github.com/SoloJiang/lectern-app/pull/71)，第 2 步 Claude Code 适配器与增量读取 [#75](https://github.com/SoloJiang/lectern-app/pull/75)，第 3 步 Codex 适配器 [#78](https://github.com/SoloJiang/lectern-app/pull/78)），第 4 步尚未实现（见「验收」）。

对应 issue [#13](https://github.com/SoloJiang/lectern-app/issues/13)。agents 发现并解析 Claude Code 与 Codex 写在本机的会话日志，把每个会话按 cwd 归位到项目和 worktree，交出轮次结构、工具调用事件和原始用量记录（[技术调研「Agent 项目发现」](tech-research.md#1-agent-项目发现)、[ADR 0014](../adr/0014.md)、[ADR 0020](../adr/0020.md)）。读取方式与缓存键见 [ADR 0026](../adr/0026.md)。

## 范围

- 做：
  - 会话发现：枚举两种 agent 的会话目录，读每个文件的开头和末尾得到会话摘要，监听目录变化。
  - 解析：Claude Code 与 Codex 的 JSONL（Codex 另有 `.jsonl.zst`），按版本适配，不认识的记录跳过并计数。
  - 轮次结构：prompt、agent 说明、工具调用（含文件改动）按轮次组织，正文按引用延迟读取。
  - 子 agent 会话：与父会话、派出它的工具调用关联；Codex 子 agent 与分叉会话标出继承自父会话的前缀。
  - 按 cwd 归位：经 `services` 的 `Projects::locate`（已打开的项目）或 `Projects::resolve` 得到项目与 worktree。
  - 增量读取：游标之后的新行，供多个消费方各自按游标拉取。
  - 交给 `usage` 的三类记录：原始用量、工具调用的开始与结果事件（ADR 0020）、Codex 的额度快照。
  - 恢复会话用哪个已登记的 agent 命令（`ResumeCommand`），供 `terminal` 映射成 `claude --resume <id>`、`codex resume <id>`。
- 不做：
  - 用量去重、规范化、归属与聚合：`usage`。本 crate 交出日志里的原始数值和去重用的键。
  - 命令文本的解析：`usage`（`crates/usage/AGENTS.md`）。包括 `cd`、`pushd`、`git -C` 改变目录的写法，也包括常见只读命令（`cat`、`sed -n`、`head`、`tail`、`rg`、`grep`、`ls`、`find`）的目标路径：Codex 读文件和搜索都走 shell，ADR 0020 要求这些读取 / 搜索目标参与归属。本 crate 交出完整命令文本和工作目录，不解释命令。
  - 改动归因、片段对齐、Drifted：`review`。本 crate 只交出每次工具调用的改动内容。
  - 路径到项目、worktree、子项目的映射：`project`，本 crate 只调用 `Projects` 接口。
  - 运行 `claude --resume`：`terminal`。
  - Claude Code 的额度（statusline）：`platform` 安装、`usage` 解析。
- 本 issue 之外：
  - Analyze（[ADR 0019](../adr/0019.md)）：查找并经监护进程启动命令行、凭据转发端点与计量记录、按沙箱跟踪和终止进程、过滤辅助会话。机制大，不属于 P1 的会话发现，另开 issue，设计写进本文的后续修订；约束仍以 `crates/agents/AGENTS.md` 为准。本文为它预留：`SessionFilter`（隐藏辅助会话）、`UsageRecord` 的来源字段（见「数据结构」），以及测试启动命令行的假命令方式（见「测试」）。
  - ACP 结构化对话（[#23](https://github.com/SoloJiang/lectern-app/issues/23)，P3）：ACP 会话作为另一个 `SessionSource` 实现接入；`ToolKind` 的分类与 ACP 的 tool call kind（read、edit、execute、search 等）一一对应，Conversation 面板读同一套轮次结构。本文不定义 ACP 客户端。

## 依赖

本 crate 是插件 crate（ADR 0025「插件 crate」规则，与本文同期合入）：实现 `Plugin`，`[package.metadata.lectern]` 标 `plugin = true`。只依赖 kernel 层和非插件的库 crate；`project`、`usage`、`review` 都是插件，彼此只通过 `services` 的接口往来。

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-kernel` | `Plugin`、事件、`PluginHandle::own_with` | 插件机制 |
| `lectern-services` | `SessionSource` 接口与数据类型（本文定义，放在 `services::session`）；调用 `Projects::locate` / `Projects::resolve` | 消费方只依赖接口（架构规则 7） |
| `lectern-executor` | 后台任务（发现、解析、归位都在 `Background` 优先级） | UI 线程不做 IO |
| `lectern-fs-watch` | 会话目录的递归监听（不按 git index 过滤，见「对其他 crate 的要求」） | macOS 后端在 `platform` |
| `lectern-cache` | 轮次索引的持久化（ADR 0026） | 打开大会话时不重新解析 |
| `serde` 1（`derive`）+ `serde_json` 1 | 每行反序列化到只声明用到字段、字符串借用原行的结构 | MIT / Apache-2.0；ADR 0014 规定用 serde 自行解析；不先转成 `serde_json::Value`（ADR 0026） |
| `ruzstd` | 解压 Codex 的 `.jsonl.zst` | MIT，纯 Rust 流式解码器，不需要 C 工具链（ADR 0026） |

不引入 `chrono` / `time`：日志里的时间戳都是 RFC 3339 UTC（`2026-10-17T02:46:01.123Z`），手写解析到毫秒。不直接用 `notify`：目录监听走 `fs_watch`。版本取实现时 crates.io 上的最新兼容版本，由 Cargo.lock 锁定；新增依赖先跑 `just deny`。

`executor`、`cache`、`fs_watch` 由 `app` 创建一次、在构造插件时传入：

```rust
pub struct AgentsDeps {
    pub executor: Executor,
    pub cache: Cache,
    pub watch: Arc<dyn Watcher>,
    /// 压缩会话的解压副本目录，`app` 取 `platform` 提供的应用缓存目录下的 `agents/decoded/`（见「压缩会话的随机读取」）。
    pub decoded_dir: PathBuf,
    /// 会话目录：默认取 `$CLAUDE_CONFIG_DIR`（逗号分隔）或 `~/.claude` 与 `~/.config/claude`，
    /// `$CODEX_HOME` 或 `~/.codex`。测试传临时目录。
    pub roots: AgentRoots,
}
/// 两个插件：ID "session-claude-code" 与 "session-codex"，各提供一个 `dyn SessionSource`，激活条件 `AfterFirstFrame`，依赖 `dyn Projects`。
pub fn plugins(deps: AgentsDeps) -> [(Name, Box<dyn Plugin>); 2];
```

### 对其他 crate 的要求

- `fs_watch`：提供不过滤的递归目录监听，监视 git 仓库之外的目录（会话目录），事件只带路径，合并后交付。已写进 `crates/fs_watch/AGENTS.md`。
- `cache`：提供不属于任何仓库的应用级分库，存本 crate 的轮次索引（键见 ADR 0026）和压缩会话的摘要（键是压缩文件的身份加适配器版本和摘要格式版本），按字节 LRU，与其他条目一样可以随时删除重建。已写进 `crates/cache/AGENTS.md`。
- `services`：采纳本文「接口」一节定义的 `session` 模块，`services` 的设计文档引用本节，不重复定义。

## 接口

### 服务接口（`lectern-services` 的 `session` 模块）

类型都是不含行为的数据，能通过 JSON-RPC 序列化（与 `services::project` 一样派生 `Serialize`、`Deserialize`，外部标记的枚举；都派生 `Clone`、`PartialEq`、`Debug`，不含浮点数的另派生 `Eq`，`RateWindow` 的 `used_percent` 是 `f32`，所以含它的 `RateLimitSample`、`Record`、`Batch` 只有 `PartialEq`），以后的进程插件可以实现同一个接口。进程插件的单条消息上限是 4 MB（[ADR 0018](../adr/0018.md)）：经进程插件实现时，超过这个上限的单条正文由 `load` 返回 `SessionError::TooLarge(len)`，`read` 跳过该记录并计入 `oversized_lines`、游标越过它；分块传输留给进程插件宿主的设计。内置来源在进程内，不受此限。

```rust
/// 多实现服务：每种 agent 一个提供者，ACP（P3）和第三方会话来源再各加一个。
/// impl Service for dyn SessionSource { const ID = "session-source"; const MULTIPLE = true; }
pub trait SessionSource: Send + Sync {
    /// 这个提供者的稳定 ID，等于提供它的插件 ID（"session-claude-code"、"session-codex"；
    /// 以后如 "acp-claude-code"）。插件 ID 全局唯一（`PluginSet::validate`），所以来源 ID 也唯一；
    /// 跨版本不变，消费方把它和会话 ID 一起落盘。不阻塞。
    fn id(&self) -> SourceId;

    /// 会话属于哪个 agent 产品（界面的 agent 圆点、`usage` 的按 agent 拆分）。不阻塞。
    fn agent(&self) -> Agent;

    /// 当前会话列表。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程上可以调用。
    fn sessions(&self) -> Arc<SessionList>;

    /// 从 `from` 往后读，读到累计约 `max_bytes` 字节后在下一个完整记录之后停下。阻塞（读文件、解压、解析），只在 executor 上调用。
    /// `max_bytes` 是软上限：只要游标之后还有一个完整的行，这次调用至少消费一行（哪怕它比 `max_bytes` 大），
    /// 所以返回时要么 `at_end` 为 true，要么新游标严格大于 `from`，调用方反复调用不会原地打转。调用方怎样分批、何时停下见「调用方」里对消费方的约束。
    /// 超过 16 MB 的行不缓存进内存，边读边丢弃到下一个换行，计入 `Diagnostics::oversized_lines`，游标越过它（也算消费了一行）。
    /// 游标与文件对不上（被截断、被替换），或游标里记的来源 ID、适配器版本与当前提供者不同时，从头读，`Batch::reset` 为 true。
    /// 版本检查在这里做：`Cursor::from_bytes` 与来源无关，只检查序列化格式。
    fn read(&self, session: &SessionKey, from: &Cursor, max_bytes: u64) -> Result<Batch, SessionError>;

    /// 轮次索引。阻塞，只在 executor 上调用：内存命中时只复制 `Arc`；否则读 `cache` 里的索引、
    /// 再解析索引之后追加的部分；都没有时解析整个文件（大会话超出打开预算，见「性能」）。会话还在写入时，返回截至当前末尾的索引。
    fn turns(&self, session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError>;

    /// 读一段正文（prompt、agent 说明、工具输入与结果、文件改动）。阻塞，只在 executor 上调用；
    /// 按偏移定位、读一行并解析，与文件大小无关。`.jsonl.zst` 会话从它的解压副本里定位（见「压缩会话的随机读取」）。
    fn load(&self, session: &SessionKey, at: &ContentRef) -> Result<Content, SessionError>;

    /// 恢复这个会话用哪个已登记的 agent 命令。不阻塞。子 agent 会话和格式无法识别的会话返回 None。
    /// 只返回数据，不返回可执行的命令行（见下面的 `ResumeCommand`）。
    fn resume(&self, session: &SessionKey) -> Option<ResumeCommand>;

    /// 请求补齐这些会话的待定字段（`SessionSummary::tail_pending`）。立即返回，任何线程可调用，视图在会话行
    /// 显示出来或被打开时调用；补齐在后台进行，完成后随 `SessionsChanged` 送达。已补齐或不待定的会话忽略。
    fn complete(&self, sessions: &[SessionKey]);
}

pub enum SessionError {
    /// 文件已删除（Claude Code 默认删除 30 天前的会话）。调用方从列表里去掉它；`usage` 已落盘的数据不受影响。
    NotFound,
    /// 读文件或解压失败。附原因，日志用。调用方在下一次 `SessionsChanged` 后重试。
    Io(Arc<str>),
    /// 整个文件认不出格式（见「状态与异常」）。`turns` 返回这个错误，`read` 返回空的 `Batch`。
    Unrecognized,
    /// 经进程插件实现的来源：这条正文超过进程插件的单条消息上限（4 MB）。附原始字节数。
    TooLarge(u64),
}

/// 恢复会话的命令只能是宿主登记的几种，来源只给出会话 ID，不给程序名和参数。
pub enum ResumeCommand {
    /// `terminal` 映射为 `claude --resume <session_id>`。
    ClaudeCode { session_id: Arc<str> },
    /// `terminal` 映射为 `codex resume <session_id>`。
    Codex { session_id: Arc<str> },
}
```

恢复命令是安全边界（[ADR 0018](../adr/0018.md)「能力声明」）：`terminal` 在插件的 Seatbelt 之外、以用户身份运行它，所以：

- 程序名和固定参数由 `terminal` 按枚举映射，来源不能给出任意程序、参数或环境变量。
- `session_id` 必须是 UUID 的规范写法（两种 agent 的会话 ID 都是 UUID），不是时 `terminal` 拒绝，所以它不会被解析成选项或注入别的参数。
- 工作目录不取自来源：`terminal` 用这个会话在 `SessionList` 里的 `cwd`，并要求它在一个已打开项目的 worktree 根或已打开的文件夹项目根之内，否则不恢复。
- `terminal` 只接受内置来源（本 crate 的 "session-claude-code"、"session-codex"，以后内置的 ACP 来源）的 `ResumeCommand`；第三方进程插件来源的 `resume` 结果一律忽略，直到它们有对应的能力声明（另写 ADR）。
- 内置来源的身份由宿主保证，不依赖提供者自报的 `id()`：进程插件的 `SessionSource` 由进程插件宿主在 app 进程里的代理实现，代理的 `id()` 是宿主生成的 `ext.<插件 ID>.<插件报告的 ID>`，所以不会等于内置来源的 ID；代理的 `resume` 不转发给插件，总是返回 None。内置 ID 只能来自编译进 app 的插件。

调用方：

- 路由：消费方通过 `ServiceSet<dyn SessionSource>` 拿到全部提供者，按 `SessionKey::source` 找 `id()` 相同的那一个调用 `read` / `turns` / `load` / `resume`；找不到（提供者已停用）就当作 `NotFound`，不改调用别的提供者。同一个上游会话可以同时出现在两个来源里（如本地日志和 P3 的 ACP），它们是两个不同的 `SessionKey`；`usage` 按与来源无关的 `UsageKey` 去重，不会重复计数。
- `welcome`、`project_home`、`worktrees` 在渲染时调 `sessions()`；收到 `SessionsChanged` 后安排重绘。
- `usage` 收到 `SessionsChanged` 后在 executor 上对变化的会话调 `read`，把记录和新游标在同一个 SQLite 事务里写入，所以重启后从自己的游标继续，不重复也不遗漏。
- 对消费方的约束：第一次发现时（或游标丢失后）要补读的历史可能有几 GB，消费方不得一次读完。补读分批进行，每批之后提交游标、可以中断并在下次从存下的游标继续；在 `Background` 优先级运行；不得让 kernel 无法静止（[ADR 0017](../adr/0017.md)：首帧后 30 秒内静止），即正在运行的一批持有 `BusyGuard`，这一批的结果写入、游标提交之后才释放；批与批之间不持有守卫，剩余的历史不预先排队，等 kernel 发出 `Quiescent` 事件、并且静止持续 10 秒（长于 [kernel 设计](kernel.md)「静止」里内存采样要求的 5 秒连续静止）后才调度下一批；其间出现忙碌就重新计时。所以每一批都计入忙碌，批与批之间留出完整的采样窗口。每批的大小和节奏由 `usage` 的设计文档规定。活跃会话（`open_turn` 为 true，或 `last_activity` 在 10 分钟之内）不受这条节奏限制：它的历史一批接一批连续读完，每批仍持有 `BusyGuard`，读完之后追加的内容收到 `SessionsChanged` 就读；游标只有一个，所以必须先读完历史才能读到新追加的内容，活跃会话的数量和大小有限（单个会话的完整解析在 1 s 以内），不会让 kernel 长时间忙碌。
- `review` 在 executor 上调 `turns` 和 `load`，`review_view` 只读 `review` 的快照。
- `terminal` 调 `resume`，按上面的规则映射成命令。
- `project_home`、`welcome` 在显示或打开 `tail_pending` 的会话行时调 `complete`。

### 事件

```rust
/// 在 executor 线程上、`SessionList` 替换之后发出。监听只更新状态或安排后台任务，不做 IO。
pub struct SessionsChanged {
    /// 发出这个事件的提供者；`changed`、`removed` 里的键的 `source` 都等于它。
    pub source: SourceId,
    pub generation: u64,
    /// 新增或有新内容的会话。
    pub changed: Arc<[SessionKey]>,
    pub removed: Arc<[SessionKey]>,
}
```

### 本 crate 的公开项

```rust
/// 不出现在会话列表里、也不交出记录的会话。Analyze 的后续 issue 用它隐藏辅助会话
/// （它们本来写在临时的配置目录里，这是兜底）。任何线程可调用，不阻塞；下一次发现时生效。
pub struct SessionFilter { /* Arc<RwLock<HashSet<SessionKey>>> */ }
impl SessionFilter {
    pub fn hide(&self, session: SessionKey);
}

/// 测试与 bench 用：不经过插件，直接解析一个文件（`test-support` feature）。
pub mod testing {
    pub fn parse_file(agent: Agent, path: &Path) -> Result<(Vec<Positioned<Record>>, TurnIndex), SessionError>;
    /// bench 的夹具（[bench 设计「迁移」](bench.md#迁移)）：fixtures 用 `include_bytes!` 嵌入，重复拼接到不少于 `len` 字节。
    pub fn synthetic_session(len: usize) -> Vec<u8>;   // Claude Code 的全部会话文件
    pub fn append_chunk() -> &'static [u8];            // 开头到第一个不早于 4096 字节的行尾
    pub fn synthetic_rollout(len: usize) -> Vec<u8>;   // Codex 的两个主会话
    pub fn compress_zst(data: &[u8]) -> Vec<u8>;       // ruzstd 最快档，一个帧
}
```

适配器的库层（第 2 步）：按会话文件工作，不持有会话列表；第 4 步的 `SessionSource` 实现按 `SessionKey` 找到文件后调用它。Codex 适配器（第 3 步）用同样的形状。

```rust
pub mod claude {
    pub const SOURCE_ID: &str = "session-claude-code";
    /// 解析规则或游标状态布局变化时加一：旧游标从头读（`Batch::reset`），旧索引不再命中。
    pub const ADAPTER_VERSION: u32;

    /// 文件归属（「外部格式」的「继承前缀」）。
    pub enum Owner { Main { session_id: Arc<str> }, Subagent { parent_session_id: Arc<str>, agent_id: Arc<str> } }
    pub struct SessionFile { pub key: SessionKey, pub path: Arc<Path>, pub owner: Owner }
    /// `read` 的结果：`Batch` 加上要并进会话与全局 `Diagnostics` 的计数。
    pub struct Parsed { pub batch: Batch, pub stats: ParseStats }
    /// 游标里的解析器状态记下的末尾字段（「数据流与线程」第 2 步：增量读取中的文件不再读末尾）。
    pub struct Activity { pub last_activity: Option<Timestamp>, pub open_turn: bool }

    pub struct ClaudeCode { /* SourceId */ }
    impl ClaudeCode {
        pub fn new(source: SourceId) -> Self;
        pub fn source(&self) -> &SourceId;
        /// `<sessionId>.jsonl` 或某个 `subagents/` 之下（可再嵌套）的 `agent-<agentId>.jsonl`；其余为 None。
        pub fn session_file(&self, path: &Path) -> Option<SessionFile>;
        /// 以下都阻塞，只在 executor 上调用。
        pub fn read(&self, file: &SessionFile, from: &Cursor, max_bytes: u64) -> Result<Parsed, SessionError>;
        pub fn turns(&self, file: &SessionFile) -> Result<TurnIndex, SessionError>;
        /// 从已有索引的游标继续，游标失效时从头重建（第 4 步的索引缓存用它）。
        pub fn turns_after(&self, file: &SessionFile, prev: Option<&TurnIndex>) -> Result<TurnIndex, SessionError>;
        /// 一遍解析得到全部记录和索引（`testing::parse_file`）。
        pub fn parse_all(&self, file: &SessionFile) -> Result<(Parsed, TurnIndex), SessionError>;
        pub fn load(&self, file: &SessionFile, at: &ContentRef) -> Result<Content, SessionError>;
        /// 读开头、末尾和 `meta.json`；`place` 为 `Pending`。读不到任何 cwd 时返回 `Unrecognized`（不列出，只计数）。
        pub fn summarize(&self, file: &SessionFile) -> Result<(SessionSummary, ParseStats), SessionError>;
        /// 不阻塞。
        pub fn resume(&self, summary: &SessionSummary) -> Option<ResumeCommand>;
        pub fn activity(&self, cursor: &Cursor) -> Option<Activity>;
    }
}

pub mod codex {
    pub const SOURCE_ID: &str = "session-codex";
    pub const ADAPTER_VERSION: u32;

    /// `rollout-<时间>-<线程 ID>[_<rollout ID>].jsonl` 或 `.jsonl.zst`；会话 ID 是文件名里的线程 ID。
    pub struct SessionFile { pub key: SessionKey, pub path: Arc<Path>, pub compressed: bool, pub archived: bool }
    /// 按线程 ID 找来源会话的文件（分叉的 `forked_from_id`、子 agent 的父线程）；第 4 步按 `SessionList` 实现，不在本机时为 None。
    pub trait Lineage: Send + Sync { fn locate(&self, thread_id: &str) -> Option<SessionFile>; }
    pub use crate::{Activity, Parsed}; // 与 `claude` 共用

    pub struct Codex { /* SourceId、解压副本、Lineage */ }
    impl Codex {
        /// `decoded_dir` 即 `AgentsDeps::decoded_dir`，副本合计上限 1 GB。
        pub fn new(source: SourceId, decoded_dir: PathBuf) -> Self;
        /// 不建副本，压缩会话每次从头流式解码（`testing::parse_file`、bench）。
        pub fn without_decoded_copies(source: SourceId) -> Self;
        pub fn with_decoded_budget(self, bytes: u64) -> Self;
        pub fn with_lineage(self, lineage: Arc<dyn Lineage>) -> Self;
        pub fn source(&self) -> &SourceId;
        pub fn session_file(&self, path: &Path) -> Option<SessionFile>;
        /// 以下与 `ClaudeCode` 的同名方法相同，都阻塞。
        pub fn read(&self, file: &SessionFile, from: &Cursor, max_bytes: u64) -> Result<Parsed, SessionError>;
        pub fn turns(&self, file: &SessionFile) -> Result<TurnIndex, SessionError>;
        pub fn turns_after(&self, file: &SessionFile, prev: Option<&TurnIndex>) -> Result<TurnIndex, SessionError>;
        pub fn parse_all(&self, file: &SessionFile) -> Result<(Parsed, TurnIndex), SessionError>;
        pub fn load(&self, file: &SessionFile, at: &ContentRef) -> Result<Content, SessionError>;
        /// `.jsonl.zst` 只流式解压开头，`tail_pending` 为 true。
        pub fn summarize(&self, file: &SessionFile) -> Result<(SessionSummary, ParseStats), SessionError>;
        /// 补齐末尾字段：整份流式解码一次，不建副本（`SessionSource::complete`）。
        pub fn complete(&self, file: &SessionFile) -> Result<(SessionSummary, ParseStats), SessionError>;
        /// 压缩文件消失时删掉它的副本（第 4 步的发现调用）。
        pub fn remove_decoded(&self, key: &SessionKey);
        pub fn resume(&self, summary: &SessionSummary) -> Option<ResumeCommand>;
        pub fn activity(&self, cursor: &Cursor) -> Option<Activity>;
    }
}

/// 被跳过的行与归入 `ToolKind::Other` 的工具名。`unknown_tools` 只进日志（`Diagnostics` 没有这一项）。
pub struct ParseStats {
    pub unknown_types: Vec<(Arc<str>, u32)>, // 最多 32 种
    pub bad_lines: u32,
    pub oversized_lines: u32,
    pub unknown_tools: Vec<(Arc<str>, u32)>, // 最多 32 种
}
```

`ContentRef::part` 在 Claude Code 里是 `message.content` 内容块的下标，`u16::MAX` 指整条消息的文本（字符串内容，或全部 `text` 块用换行拼接）：prompt 与 `Injected` 用后者，`AgentText` 和工具调用的输入、结果用前者。Codex 一行只有一个条目，`part` 说的是取这一行的哪一部分：`u16::MAX` 是文本（prompt、agent 说明、`compacted` 的摘要），0 是工具调用的输入（`arguments`、`input`、`local_shell_call.action`），1 是工具输出，2 是改动（`patch_apply_end.changes`、`FileChange.changes`）。

## 数据结构

### 会话与列表

```rust
/// 第三方来源报告的其他 agent 用 `Other`，附它自己的名字。
pub enum Agent { ClaudeCode, Codex, Other(Arc<str>) }

/// 提供者的稳定 ID，见 `SessionSource::id`。
pub struct SourceId(pub Arc<str>);

/// 会话的全局键：来源加来源内的会话 ID。`id` 只在同一来源内唯一。
/// 本 crate 的 `id`：Claude Code 为 `sessionId`（文件名去掉 `.jsonl`），子 agent 为 `<父会话 sessionId>/agent-<agentId>`（不同父会话里的同名 `agentId` 是不同的会话，与「子 agent 与继承前缀」里按（父会话目录，agentId）认定归属一致）；Codex 为 `session_meta.id`（线程 ID）。
pub struct SessionKey { pub source: SourceId, pub id: Arc<str> }

/// UTC 毫秒。
pub struct Timestamp(pub i64);

pub struct SessionList {
    /// 每次替换加一，与 `SessionsChanged::generation` 一致。
    pub generation: u64,
    /// 按 `last_activity` 从新到旧。
    pub sessions: Arc<[SessionSummary]>,
    pub diagnostics: Diagnostics,
}

impl SessionList {
    /// 线性查找。
    pub fn get(&self, key: &SessionKey) -> Option<&SessionSummary>;
    /// 归位到这个项目的会话（`Place::Located` 或 `Place::Resolved`，含子 agent 会话），按 `last_activity` 从新到旧。
    pub fn in_project<'a>(&'a self, project: &'a ProjectKey) -> impl Iterator<Item = &'a SessionSummary> + 'a;
}

pub struct SessionSummary {
    pub key: SessionKey,
    pub agent: Agent,
    pub parent: Option<Parent>,
    pub file: Arc<Path>,
    /// 会话开始时的 cwd，原样取自日志。
    pub cwd: Arc<Path>,
    pub place: Place,
    pub started_at: Timestamp,
    /// 最后一条记录的时间戳；没有时、或 `tail_pending` 时取文件的修改时间。
    pub last_activity: Timestamp,
    /// 第一个 prompt 的第一行，最多 120 个字符；开头 256 KB 里没有 prompt 时为 None。
    pub title: Option<Arc<str>>,
    /// Claude Code 的 `version`，Codex 的 `cli_version`。
    pub agent_version: Option<Arc<str>>,
    /// 日志记录的开始时的分支（Claude Code `gitBranch`，Codex `session_meta.git.branch`），只用于导入的旧会话（ADR 0020）。
    pub logged_branch: Option<Arc<str>>,
    /// 最后一轮还没有结束：Codex 没有 `task_complete` / `turn_aborted`；Claude Code 最后一条 assistant 记录的
    /// `stop_reason` 不是 `end_turn`，或有工具调用没有结果。界面据此和 `last_activity` 判断是否在运行。
    pub open_turn: bool,
    /// 取自文件末尾的字段（`last_activity`、`open_turn`）还没有算出：只发生在第一次见到、摘要缓存里也没有的 `.jsonl.zst`
    /// （见「数据流与线程」第 2 步）。此时 `last_activity` 暂取文件修改时间，`open_turn` 为 false；只主动补齐最近的几个，其余在 `SessionSource::complete` 之后补齐，随下一次 `SessionsChanged` 更新。
    pub tail_pending: bool,
    /// Codex `archived_sessions/` 下的会话。
    pub archived: bool,
    /// 继承前缀的边界无法判断（没有 `subagent_history_start_ordinal` 的 Codex 分叉，来源会话不在本机），全部记录按自己的计。
    pub inheritance_unknown: bool,
    pub format: Format,
}

pub struct Parent {
    pub session: SessionKey,
    /// 派出它的工具调用。Claude Code 取 `agent-<id>.meta.json` 的 `toolUseId`，没有这个文件时为 None。
    pub tool_call: Option<ToolCallId>,
}

pub enum Place {
    /// 还没有归位（首次发现之后、`resolve` 完成之前）。
    Pending,
    /// cwd 在已打开的项目里：`Projects::locate` 的结果，含子项目。`package` 为 `NotLoaded` 时按「归位」补齐：
    /// 非当前 worktree 用 `resolve_package` 的结果替换，当前 worktree 或加载中的文件夹项目等下一次 `Changed` 后重新 `locate`。
    Located(Location),
    /// 项目没有打开：`Projects::resolve` 的结果（项目、worktree、相对路径），没有子项目信息。
    /// 项目打开后，收到 `ProjectEvent` 时重新归位为 `Located`。
    Resolved(Resolved),
    /// cwd 已不存在。会话照常列出，不属于任何项目。
    Missing,
    Unresolvable(ResolveError),
}

pub enum Format {
    Known,
    /// 有记录被跳过（不认识的类型、坏行、超长行），数量见 `Diagnostics`。
    Partial,
    /// 开头 256 KB 里开始的行里找不到会话头（Claude Code：带 `sessionId` 和 `cwd` 的记录；Codex：首行 `session_meta`）。
    Unrecognized,
}

/// 每个会话与全局的计数，进日志，bench 读取。
pub struct Diagnostics {
    pub unknown_types: Arc<[(Arc<str>, u32)]>, // 不认识的 `type`（Codex 为 `type` 加 `payload.type`），最多记 32 种
    pub bad_lines: u32,                        // 不是合法 JSON，或缺少必需字段
    pub oversized_lines: u32,                  // 超过 16 MB 的行
    pub unreadable_dirs: Arc<[Arc<Path>]>,
}
```

不变量：

- 一个会话一个 `SessionKey`。Codex 的同一个会话同时有 `.jsonl` 和 `.jsonl.zst` 时读 `.jsonl`；`sessions/` 和 `archived_sessions/` 都有时读 `sessions/`。每次文件新增或删除后，按 `SessionKey` 重新核对它的所有候选路径：选中的路径消失而另一个候选仍在（压缩或归档后原文件被删）时改用剩下的那个，只有所有候选都不在时才从列表移除。
- `parent` 指向的会话可能不在列表里（父会话文件已删除）。
- 被 `SessionFilter` 隐藏的会话不在列表里。

### 游标与批量读取

```rust
/// 不透明，可序列化（serde 形式是 `to_bytes` 的字节）。消费方原样保存，不解读。
pub struct Cursor { /* 见下 */ }

impl Cursor {
    pub const START: Cursor;
    /// 以下供提供者使用：外层的来源 ID、适配器版本和偏移由 `services` 编码，
    /// `state`（偏移之前的 256 字节副本、解析器状态）是提供者自己的字节，`services` 不解读。
    pub fn new(source: SourceId, adapter_version: u32, offset: u64, state: impl Into<Box<[u8]>>) -> Cursor;
    pub fn is_start(&self) -> bool;
    pub fn source(&self) -> Option<&SourceId>;      // `START` 为 None
    pub fn adapter_version(&self) -> Option<u32>;   // `START` 为 None
    pub fn offset(&self) -> u64;                    // `START` 为 0
    pub fn state(&self) -> &[u8];                   // `START` 为空
    /// 版本字节（1）；`START` 到此为止，否则依次是来源 ID（u16 LE 长度加 UTF-8）、
    /// 适配器版本（u32 LE）、偏移（u64 LE），其余为 `state`。
    pub fn to_bytes(&self) -> Vec<u8>;
    /// 序列化格式不认识时返回 None，调用方从 `START` 读。游标里还记着生成它的来源 ID 和适配器版本，
    /// 但这里不检查：版本是否与当前提供者一致由 `SessionSource::read` 判断，不一致时从头读并在 `Batch::reset` 里告诉消费方
    /// （适配器升级后能解析更多记录或修正了映射），消费方按 `Batch::reset` 的规则替换已有数据。
    pub fn from_bytes(bytes: &[u8]) -> Option<Cursor>;
}

pub struct Batch {
    /// 为 true 时记录从文件开头重新给出，回放可能跨多批。消费方把回放的记录写进这个 `SessionKey` 的新一代数据，
    /// 旧数据在回放期间照常可见；每批的写入和游标提交在同一个事务里，重启后从游标继续回放。读到 `at_end` 为 true 的一批时，
    /// 在同一个事务里删除旧一代（用量、工具事件、限额等）并把新一代设为当前，所以截断、替换或适配器不再给出的旧记录不会留下，
    /// 回放中途也不会只剩一部分历史。回放期间再次 `reset` 时丢弃未完成的新一代，重新开始。
    /// 同一条用量也由另一个会话交来时（按 `UsageKey` 去重），只去掉这个会话的那份来源，其他会话的仍然计数，细节由 `usage` 的设计规定。
    pub reset: bool,
    pub records: Vec<Positioned<Record>>,
    pub cursor: Cursor,
    /// 游标之后已没有完整的行（包括只剩一行还没写完的情况）。为 false 时 `cursor` 一定严格大于调用时的 `from`；
    /// `reset` 的一批例外（从头回放，游标可以小于 `from`），之后的批照常前进。
    pub at_end: bool,
}

pub struct Positioned<T> {
    /// 这一行在（解压后的）文件里的字节偏移。不同行的 pos 严格递增；同一行产生的多条记录 pos 相同，
    /// 用 `seq` 区分。记录的顺序是 (pos, seq)，与 `Batch` 里的顺序一致。
    pub pos: u64,
    /// 同一行内的序号，从 0 开始。
    pub seq: u16,
    /// 继承自父会话或分叉来源的前缀（见「子 agent 与继承前缀」）。
    pub inherited: bool,
    pub value: T,
}
```

游标的内容：格式版本、解压后的字节偏移（只停在换行之后）、偏移之前的 256 字节原样副本、解析器的状态（当前轮次、当前模型与 cwd、是否仍在继承前缀里）。读取时先比较这 256 字节，不同就当作文件被改写，从头读（ADR 0026）。偏移按解压后的内容计，Codex 把冷文件压缩成 `.zst` 之后游标照样有效。

### 记录

```rust
pub enum Record {
    /// cwd、日志记录的分支或模型变化时给出一次；会话的第一条记录之前总有一条。
    Context { at: Timestamp, cwd: Arc<Path>, logged_branch: Option<Arc<str>>, model: Option<Arc<str>> },
    TurnStarted { turn: TurnId, at: Timestamp },
    /// 只有 Codex 有明确的轮次结束；Claude Code 的一轮在下一个 prompt 开始时结束，不给出这条记录。
    TurnEnded { turn: TurnId, at: Timestamp, aborted: bool },
    ToolStarted(ToolStarted),
    ToolFinished(ToolFinished),
    Usage(UsageRecord),
    RateLimits(RateLimitSample),
}

/// Claude Code：prompt 记录的 `uuid`；Codex：`turn_id`。
pub struct TurnId(pub Arc<str>);
/// Claude Code：`tool_use.id`；Codex：`call_id`。
pub struct ToolCallId(pub Arc<str>);
```

工具调用事件（ADR 0020）：

```rust
pub struct ToolStarted {
    pub call: ToolCallId,
    /// 没有 prompt 就开始的调用（极少见）为 None。
    pub turn: Option<TurnId>,
    pub at: Timestamp,
    /// 日志里的工具名，原样保留。
    pub name: Arc<str>,
    pub kind: ToolKind,
}

pub struct ToolFinished {
    pub call: ToolCallId,
    pub at: Timestamp,
    pub status: ToolStatus,
    /// 这次调用打开的交互式 shell 会话（Codex `exec_command` 在 `function_call_output` 里返回的 `session_id`），
    /// 调用结束时 shell 仍在运行才有；其余为 None。开始时还不知道这个 ID，所以放在结束事件上。
    pub exec_session: Option<Arc<str>>,
}

pub enum ToolKind {
    /// 写文件、改文件、应用补丁（含新建、删除、移动）。路径是绝对路径（相对路径按调用时的 cwd 在词法上拼接，不访问文件系统）。
    Edit { paths: Arc<[PathBuf]> },
    /// 执行命令。`command` 是交给 shell 的完整文本；`workdir` 是工具参数里的工作目录，没有时为调用时的 cwd。
    /// `exec_session` 是这次调用写入的交互式 shell 会话 ID（`write_stdin` 参数里的 `session_id`），其余调用为 None；
    /// 打开会话的 `exec_command` 在开始时还不知道 ID，它的 ID 在 `ToolFinished::exec_session` 上，消费方按 ID 关联两者。同一会话里的 `cd` 会延续到后面的写入，所以 `write_stdin` 的 `workdir` 只是会话启动时的目录、
    /// `workdir_exact` 为 false；按目录归属的消费方对这类事件改用命令结束后观察到的分支 / HEAD。
    Execute { command: Arc<str>, workdir: Arc<Path>, exec_session: Option<Arc<str>>, workdir_exact: bool },
    Read { paths: Arc<[PathBuf]> },
    /// 搜索的目标目录或文件；工具没给路径时为调用时的 cwd。
    Search { paths: Arc<[PathBuf]> },
    /// 派出子 agent。子会话通过 `SessionSummary::parent` 指回这次调用。
    SpawnAgent,
    /// 其余工具（网页、MCP、计划、等待后台任务等）。可能有副作用的 MCP 工具无法判断，是已知限制。
    Other,
}

pub enum ToolStatus { Completed, Failed, Denied, Interrupted }
```

`ToolStarted` 的 `at` 是发出调用的那条记录的时间戳，`ToolFinished` 的 `at` 是带结果的那条记录的时间戳；两者都是日志写入时刻，不是命令真正开始和结束的时刻（是已知限制，`usage` 按「结果时刻之前」判断 reflog 顺序）。没有结果的调用不给出 `ToolFinished`。

工具的分类（适配器按版本维护这张表，表外的工具名归 `Other` 并计数）：

| | Claude Code（`tool_use.name`） | Codex |
|---|---|---|
| `Edit` | `Write`、`Edit`、`MultiEdit`、`NotebookEdit`（路径取 `file_path` / `notebook_path`） | `apply_patch`（`custom_tool_call` 或 `function_call`；路径取补丁里的 `*** Add File:`、`*** Update File:`、`*** Delete File:`、`*** Move to:`） |
| `Execute` | `Bash`（`workdir` 为该记录的 `cwd`） | `function_call` 的 `shell`（`command` 数组；形如 `["bash", "-lc", 脚本]` 时取脚本，其余按 shell 的写法加引号拼接）、`shell_command`（`command` 字符串）、`exec_command`（`cmd`、`workdir`）、`write_stdin`（写入的字符）、`local_shell_call`（`action.command`、`action.working_directory`） |
| `Read` | `Read`、`NotebookRead` | `view_image`（读文件靠 shell，按 `Execute` 交出） |
| `Search` | `Grep`、`Glob`（`path`）、`LS` | 无（同上） |
| `SpawnAgent` | `Task`、`Agent` | `spawn_agent` |

结果状态：Claude Code 的 `tool_result.is_error` 为 true 是 `Failed`，`toolUseResult.interrupted` 为 true 是 `Interrupted`，权限被拒是 `Denied`；Codex 的结果按退出码判断：`function_call_output` 里有退出码（`exec_command` / `shell` 的输出元数据：JSON 输出的 `exit_code`，或输出文本开头 `Output:` 之前的 `Exit code: N`、`Process exited with code N`）时，非 0 为 `Failed`；`apply_patch` 先到的 `patch_apply_end` / `FileChange` 的 `status` 优先（`completed`、`failed`、`declined` 分别为 `Completed`、`Failed`、`Denied`）；没有退出码时 `output.success == false` 为 `Failed`（`exec_command` 失败时 `success` 仍可能为 true，不能只看它）；`local_shell_call` 没有对应的 output 记录，结束事件取它自带的 `status`：同一 `call_id` 出现 `completed` 时发 `ToolFinished`（`Completed`），`incomplete` 时发 `ToolFinished`（`Interrupted`），`in_progress` 只发开始。

用量与额度（ADR 0014、ADR 0022）：

```rust
pub struct UsageRecord {
    pub at: Timestamp,
    pub turn: Option<TurnId>,
    pub model: Option<Arc<str>>,
    pub tokens: RawTokens,
    pub key: UsageKey,
    /// Claude Code `usage.iterations[]` 里 `type: "advisor_message"` 的那一份，模型取 `advisorModel`；按自己的模型计，
    /// 不与 `tokens` 相加。是同一条记录的附带数据，不影响去重。
    pub advisor: Option<AdvisorUsage>,
    /// 只对 `CodexTokenCount` 有意义：这条之前这个线程已出现过 `token_usage_record`（见下面的「只用一路用量」）。
    /// 是记录的可变数据，不进键：适配器升级后重读时同一条记录按键覆盖，这个标记随之更新，不会留下旧的可计数副本。
    pub superseded: bool,
    /// Claude Code 的 `isSidechain`。
    pub sidechain: bool,
    /// 会话日志。Analyze 的计量记录（后续 issue）用另一个变体，`usage` 按它区分「编写」与「审阅」。
    pub origin: UsageOrigin,
}

pub enum UsageOrigin { SessionLog }

pub struct AdvisorUsage { pub model: Arc<str>, pub tokens: RawTokens }

/// 日志里的原始数值，不规范化：两家的口径不同，规范化成互不重叠的类别在 `usage` 做。
pub enum RawTokens {
    /// `message.usage`：`input_tokens` 不含缓存。
    Anthropic { input: u64, output: u64, cache_read: u64, cache_write: u64, cache_write_5m: u64, cache_write_1h: u64 },
    /// Codex `TokenUsage`：`input` 含 `cached_input`，`output` 含 `reasoning_output`。
    OpenAi { input: u64, cached_input: u64, cache_write: u64, output: u64, reasoning_output: u64 },
}

/// 去重用的键，去重规则在 `usage`（ADR 0014）。
/// 键只由上游的标识组成，不含 `SourceId`：同一个上游会话经本地日志和 ACP 两个来源交来时，键相同、只计一次；
/// 不同的上游会话即使 token 数相同，键也不同。
pub enum UsageKey {
    /// 每个内容块一行、重复同一份 usage：同一个键可能出现多次，也可能跨文件出现（分叉、续接复制的记录），按 ADR 0014 只计一次。
    /// `message.id`（`msg_…`）和 `requestId`（`req_…`）由模型服务端生成、全局唯一，不需要再加会话 ID。
    /// 键只有这两个上游 ID：同一响应的多行逐步写出更完整的 usage，键相同，`usage` 按 ADR 0014 保留 token 合计最大的一条
    /// （连同它的 `advisor`）。advisor 的数据不进键，见 `UsageRecord::advisor`。
    Claude { message_id: Arc<str>, request_id: Option<Arc<str>> },
    /// `event_msg` / `token_count`：`tokens` 是 `last_token_usage`，`total` 是 `total_token_usage`。`info` 为 null 的事件不给出用量记录。
    /// `total` 是会话内的累计值，不是唯一标识，所以键带上游线程 ID（`session_meta.id`）：同一线程里重复写出的同一个累计值
    /// 合为一条，不同线程（重复的工作流、分叉）各自计数。分叉和子 agent 重放的父会话 `token_count` 带的是新线程的 ID，
    /// 靠 `inherited` 标记排除（ADR 0014），不靠键。
    /// 是否已被逐响应记录取代不在键里，见 `UsageRecord::superseded`。
    CodexTokenCount { thread_id: Arc<str>, total: RawTokens },
    /// 顶层 `token_usage_record`：每个完成的响应一条，`tokens` 取它的 `usage`。`response_id` 由模型服务端生成、全局唯一；同样带上 `thread_id`，与上一项一致。
    CodexResponse { thread_id: Arc<str>, response_id: Arc<str> },
}
```

只用一路用量：Codex 按线程决定。在 `3b8f43a`，每个完成的响应先写一条 `token_usage_record`，同一个响应的用量又累加进随后的 `token_count`（`codex-rs/core/src/session/mod.rs` 的 `record_response_token_usage` 与 `update_token_usage_info`），两路都计会重复。规则：

- 在一个线程（`thread_id`）里按文件顺序切换：第一条 `token_usage_record` 之前，用 `token_count` 的 `last_token_usage`；从它开始，只用 `token_usage_record`。会话由只写 `token_count` 的旧版本开始、升级后续接时，`token_usage_record` 会在同一个文件中途出现，切换之前的用量照样计入。没有任何 `token_usage_record` 的线程（旧版本的日志）全程用 `token_count`。
- 本 crate 两路都交出，在游标的解析器状态里记下这个线程是否已出现过 `token_usage_record`：只有在它之后交出的 `CodexTokenCount` 记录的 `superseded` 为 true，之前的不标、也不回头重标。`usage` 计入全部 `CodexResponse` 和 `superseded` 为 false 的 `CodexTokenCount` 记录。
- `token_count` 无论哪种情况都照常交出 `RateLimits`。

```rust

pub struct RateLimitSample {
    pub at: Timestamp,
    pub limit_id: Option<Arc<str>>,
    pub plan_type: Option<Arc<str>>,
    pub primary: Option<RateWindow>,
    pub secondary: Option<RateWindow>,
}

pub struct RateWindow {
    pub used_percent: f32,
    pub window_minutes: Option<u32>,
    /// `resets_at`（Unix 秒）；旧版本只有 `resets_in_seconds` 时按记录时间换算。
    pub resets_at: Option<Timestamp>,
}
```

### 轮次索引

```rust
pub struct TurnIndex {
    pub session: SessionKey,
    /// 索引解析到的位置；`turns` 下次从这里继续。
    pub cursor: Cursor,
    pub turns: Arc<[Turn]>,
    pub format: Format,
}

pub struct Turn {
    pub id: TurnId,
    /// 从 1 开始，按出现顺序；不含继承前缀里的轮次。
    pub ordinal: u32,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub prompt: Option<ContentRef>,
    /// prompt 的第一行，最多 120 个字符，列表直接显示，不用 `load`。
    pub prompt_preview: Arc<str>,
    pub items: Arc<[TurnItem]>,
}

pub enum TurnItem {
    /// agent 的说明文字（Claude Code 的 `text` 块，Codex 的 agent message）。思考内容不进入索引。
    AgentText(ContentRef),
    /// 不是人写的输入：后台任务通知、其他 agent 发来的消息、hook 注入的内容。
    Injected(ContentRef),
    Tool(ToolCall),
}

pub struct ToolCall {
    pub id: ToolCallId,
    pub name: Arc<str>,
    pub kind: ToolKind,
    pub started_at: Timestamp,
    pub finished: Option<(Timestamp, ToolStatus)>,
    pub input: ContentRef,
    pub result: Option<ContentRef>,
}

/// 指向文件里的一行和行内的一个部分。不透明：消费方只把它交回 `load`。
pub struct ContentRef { /* pos: u64, len: u32, part: u16 */ }

impl ContentRef {
    /// 以下供提供者使用。
    pub fn new(pos: u64, len: u32, part: u16) -> ContentRef;
    pub fn pos(&self) -> u64;
    pub fn line_len(&self) -> u32;
    pub fn part(&self) -> u16;
}

pub enum Content {
    Text(Arc<str>),
    /// `Edit` 类工具的改动，`review` 据此归因。
    Edits(Arc<[FileEdit]>),
}

pub struct FileEdit {
    pub path: PathBuf,
    pub op: EditOp,
}

pub enum EditOp {
    /// Claude Code `Write`：结果里 `originalFile` 为 null 时是新建。
    Write { content: Arc<str>, created: bool },
    /// Claude Code `Edit` / `MultiEdit` 的输入。
    Replace { old: Arc<str>, new: Arc<str>, all: bool },
    /// 统一 diff：Claude Code 结果里的 `structuredPatch`、Codex 的 `apply_patch` 补丁或 `FileChange::Update`。
    Patch { hunks: Arc<str>, move_to: Option<PathBuf> },
    Add { content: Arc<str> },
    Delete,
}
```

`load(tool_call.input)` 给出输入里的改动（`Replace`、`Write`、补丁），`load(tool_call.result)` 给出结果里的改动（`structuredPatch`，Codex 的 `PatchApplyEnd.changes` 或 `FileChange`）；两者都有时 `review` 优先用结果。索引只存引用和预览，正文按需读取，所以一个 50 MB 的会话索引也只有几百 KB。

### 外部格式

两种日志都没有官方契约（[技术调研「风险」](tech-research.md#风险)）。适配器只声明用到的字段，其余忽略；认识的、不需要的记录类型列在适配器的「已知忽略」表里，不计入 `unknown_types`。

**Claude Code**（来源：本机 Claude Code 2.1.30x 的会话文件，2026-10-17；[ccusage](https://github.com/ryoppippi/ccusage) 的 `rust/adapters/claude/src/README.md`，提交 `34cda48`）

- 位置：`<配置目录>/projects/<编码后的路径>/<sessionId>.jsonl`；子 agent 在 `<sessionId>/subagents/` 下（可能再嵌套一层，如 `workflows/<id>/`）：`agent-<agentId>.jsonl` 加 `agent-<agentId>.meta.json`（`toolUseId`、`agentType`、`worktreePath`），`*.prefix.json` 忽略。目录名的路径编码有歧义，以记录里的 `cwd` 为准。
- 每行一条记录，按 `type` 区分。用到的：`user`、`assistant`；已知忽略：`attachment`、`system`、`queue-operation`、`last-prompt`、`summary`、`file-history-snapshot`、`atis-latch`、`frame-link`、`pr-link`、`mode`、`cost-state`。
- 公共字段：`uuid`、`parentUuid`、`sessionId`、`timestamp`、`cwd`、`gitBranch`、`version`、`isSidechain`、`isMeta`、`agentId`（子 agent）。
- `assistant`：`requestId`，`message.{id, model, stop_reason, usage, content[]}`；内容块 `text`、`tool_use{id, name, input}`、`thinking`（不读）。一次响应的每个内容块各写一行，重复同一份 `usage`。
- `user`：`message.content` 是字符串或内容块数组；`tool_result{tool_use_id, is_error}` 块配合顶层 `toolUseResult`（`structuredPatch`、`originalFile`、`interrupted` 等）；`origin.kind`（`human`、`task-notification`、`peer` 等，旧版本没有）。
- 轮次：一轮从一条人写的 prompt 开始：`type: "user"`，`isMeta` 不为 true，内容是文本且不含 `tool_result`，`origin.kind` 为 `human` 或没有 `origin`；`isCompactSummary` 的摘要和本地命令输出的包装（`<local-command-stdout>` 等）不开新轮次。其他 `user` 文本是 `Injected`。子 agent 会话的第一条 `user` 记录是父 agent 给的任务，作为第 1 轮的 prompt。
- 继承前缀：先确定文件的归属，再比较每条记录。主会话文件归属于文件名里的 `sessionId`，没有 `agentId`；子 agent 文件（`subagents/` 下的 `agent-<agentId>.jsonl`）里的记录带的是**父会话**的 `sessionId`（本机 2.1.30x 的全部子 agent 文件逐行核对过），归属于（父会话目录名、文件名里的 `agentId`）。记录的 `sessionId` 与归属的会话不同、或在子 agent 文件里 `agentId` 与文件的不同（分叉、续接时复制进来的历史），标为 `inherited`，不进入轮次索引；子 agent 文件里带父会话 `sessionId` 和自己 `agentId` 的记录是它自己的，照常计入。

**Codex**（来源：[openai/codex](https://github.com/openai/codex) 提交 `3b8f43a`（2026-10-17）的 `codex-rs/history/src/lib.rs`（`RolloutLine`、`RolloutItem`）、`codex-rs/protocol/src/protocol.rs`（`SessionMeta`、`EventMsg`、`TokenCountEvent`）、`codex-rs/protocol/src/models.rs`（`ResponseItem`）、`codex-rs/protocol/src/items.rs`（`TurnItem`）、`codex-rs/rollout/src/policy.rs`（哪些条目写入文件）、`codex-rs/rollout/src/compression.rs`（`.zst`））

- 位置：`<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-<时间>-<id>.jsonl`，冷文件被压缩为 `.jsonl.zst`；归档的在 `<CODEX_HOME>/archived_sessions/`。
- 每行 `{timestamp, ordinal?, type, payload}`。`type`：`session_meta`、`turn_context`、`response_item`、`event_msg`、`compacted`、`token_usage_record`、`inter_agent_communication_metadata`；其余（`world_state`、`retained_context` 等）已知忽略。
- `session_meta`（首行）：`id`、`cwd`、`cli_version`、`source`（含 `subagent`）、`parent_thread_id`、`forked_from_id`、`history_mode`（`legacy` / `paginated`）、`subagent_history_start_ordinal`、`git.branch`。
- `turn_context`：`turn_id`、`cwd`、`model`。
- 轮次：`event_msg` 的 `task_started`（别名 `turn_started`）到同一 `turn_id` 的 `task_complete`（`turn_complete`）或 `turn_aborted`。prompt 和 agent 说明：`legacy` 模式取 `event_msg` 的 `user_message` / `agent_message`；`paginated` 模式取 `item_completed` 里的 `UserMessage` / `AgentMessage`。`response_item` 里 `role: "user"` 的消息含环境说明和指令文件，不作为 prompt。
- 工具：两种模式都从 `response_item` 取 `function_call{name, arguments, call_id}`、`custom_tool_call{name, input, call_id}`、`local_shell_call`，结果取 `function_call_output` / `custom_tool_call_output`。`exec_command_begin/end`、`patch_apply_begin` 不写入文件；改动另取 `legacy` 的 `patch_apply_end.changes` 或 `paginated` 的 `item_completed` / `FileChange`。
- 用量：顶层 `token_usage_record{thread_id, turn_id, response_id, usage}`（每个响应一条），以及 `event_msg` / `token_count` 的 `info.{last_token_usage, total_token_usage}`；两路只用一路，规则见「记录」里的「只用一路用量」。模型取最近一条 `turn_context`；额度取 `token_count` 的 `rate_limits.{primary, secondary}`。
- 继承前缀：子 agent 与分叉会话先重放来源的历史。有 `subagent_history_start_ordinal` 时，`ordinal` 小于它的行是继承前缀。没有这个字段时（`legacy` 分叉），复制进来的前缀里本身就有来源的 `task_started`，所以按轮次 ID 判断：取来源会话（`forked_from_id` / `parent_thread_id`）文件里全部 `task_started` 的 `turn_id`，包括来源自己继承的轮次（轮次索引不含这些，所以不用轮次索引），多级分叉因此同样成立；这份集合在解析来源时顺带记下，与轮次索引一起缓存。`read` 读到这样的分叉、来源在 `SessionList` 里但集合还没有建立时，先在同一次调用里解析来源建立集合，再判断边界，所以结果与发现和补读的顺序无关。`turn_id` 出现在这份集合里的轮次是继承的，第一条 `turn_id` 不在来源里的 `task_started`（或 `trigger_turn: true` 的 `inter_agent_communication_metadata`）开始自己的部分（ADR 0014）。找不到来源会话时（已删除或不在本机）无法判断边界：全部记录不标 `inherited`，`SessionSummary::inheritance_unknown` 为 true，`usage` 据此提示这个会话的用量可能含继承部分；游标里记下边界未定，来源之后出现时，下一次 `read` 返回 `reset` 并按边界重新回放。

兼容策略：适配器按记录形状判断，不按版本号分支；字段缺失时按上表的旧写法再试一次（如 `resets_in_seconds`、没有 `origin`），仍不行就跳过这一行并计数。上游格式变化时，加 fixtures、改适配器、在本节记下来源提交或版本。

fixtures（`crates/agents/tests/fixtures/`）：每个上游格式版本一份脱敏的真实样本，目录名写 agent 与版本（如 `claude-code/2.1.302/`、`codex/0.14x-legacy/`、`codex/<版本>-paginated/`），旁边的 `SOURCE.md` 记下版本、日期和覆盖了哪些记录。至少包括：Claude Code 的主会话加两个子 agent（一个带 worktree）、一次 `Edit` 失败、一次中断的 `Bash`、一个分叉会话；Codex 的 `legacy` 与 `paginated` 各一个、一个 `.jsonl.zst`、一个子 agent rollout、一个只有额度没有用量的 `token_count`。脱敏用测试辅助工具按字段白名单处理：保留结构、类型、ID 的相同关系、时间戳和 token 数，文本和路径替换为等长的占位内容，路径统一到 `/Users/dev/fixture/` 下。

### 子 agent 与继承前缀

- 子 agent 会话是独立的 `SessionSummary`，用 `parent` 指回父会话和派出它的工具调用，按自己的 cwd 归位（Claude Code 的子 agent 常在 `.claude/worktrees/` 下的另一个 worktree 里）。合到父会话的合计、在列表里折叠到父会话下，由 `usage` 和视图决定。
- 继承前缀里的记录照样交出、标为 `inherited`，由 `usage` 按 ADR 0014 只计自己的部分；轮次索引不含它们。

### 压缩会话的随机读取

Codex 把整个冷文件压成一个 zstd 帧（`codex-rs/rollout/src/compression.rs` 在 `3b8f43a` 用 `zstd::stream::write::Encoder` 一次写完），没有可以定位的帧边界，流式解码只能从头开始。所以凡是要从某个偏移开始读的操作都用解压副本，不在每次调用时重新解码：

- 第一次对某个 `.jsonl.zst` 会话调 `read`、`turns` 或 `load` 时，把它完整解压到 `decoded_dir` 下的一个文件（先写临时文件、完整后 `rename`，之后只读），同时建好轮次索引；之后 `read` 从游标的偏移开始读副本，`load` 在副本上按偏移 `pread` 一行，都与未压缩的会话相同。分批 `read` 一个归档的总代价是一次解压加一次顺序读，不随批数增加。
- 只有发现（只解压开头 256 KB）和补齐末尾字段（整份流式解码一次）不建副本。
- 副本的键：来源 ID、会话 ID，以及压缩文件的身份（设备号、inode、大小、修改时间）。冷文件压缩后不再改动，身份变了（被重新压缩、被替换）就删掉旧副本重建；压缩文件消失时，下一次发现删掉副本。
- 代价：磁盘占用等于解压后的大小，全部副本合计上限 1 GB，按最后访问时间做字节 LRU，正在读取的副本（有打开的描述符）不淘汰；内存只有解码用的 64 KB 缓冲，副本不读入内存。副本和 `cache` 一样可以随时删除重建。
- 第一次打开一个压缩会话要解压整个文件（≥ 100 MB/s，40 MB 约 0.4 s），超出 50 ms 预算，界面按[交互规范「状态与异常」](interaction.md#状态与异常)显示骨架或进度；之后的打开和 `load` 在预算内。这是已知限制（ADR 0026）。
- 写副本失败（磁盘满、目录不可写）时，`read` 和 `load` 退回从头解码到偏移，结果正确但慢，记日志；这时 `read` 忽略 `max_bytes`，一次读到末尾，避免分批时反复从头解码。
- 发现阶段不为压缩文件建副本，也不在发布列表之前整份解压（见「数据流与线程」第 2 步）。压缩文件不变，补齐后的完整摘要存进 `cache` 的应用级分库，键是同一个文件身份加适配器版本和摘要格式版本：升级后适配器或摘要字段变了，旧摘要不再命中、重新补齐；下次启动直接读，不再解压。

放弃的方案：记录 zstd 帧边界（Codex 只写一个帧）；把压缩会话里被引用的每一行正文存进轮次索引（索引的大小接近原文件，全部进 SQLite、加载时进内存，比一个可淘汰的副本文件更贵）。

## 数据流与线程

1. 插件在首帧之后激活（`AfterFirstFrame`），`started` 里通过 `own_with` 提交发现任务（`Background`）。首帧之前不做任何事。发现、监听触发的刷新、`complete` 的补齐都在提交前用 `PluginHandle::busy()` 取一个 `BusyGuard`，随任务一起移进去，结果发布（第 4 步）之后才 drop（[kernel 设计](kernel.md)「静止」），所以这些工作进行中 kernel 不报告静止。
2. 发现：先注册第 5 步的目录监听，再枚举会话目录；枚举期间收到的事件先排队，枚举结束后与扫描结果按路径合并、按第 5 步处理，然后才发布，所以扫描与监听之间新建或追加的会话不会漏掉。枚举只列目录项，不读 `.prefix.json` 等无关文件），每个会话文件读开头 256 KB 里开始的各行（找会话头、cwd、第一个 prompt），在 256 KB 之内开始的行总是读完整（最长 16 MB，与 `read` 的单行上限相同，所以带大段指令的 `session_meta` 也能识别）；以及末尾的完整记录（最后活动时间、`open_turn`），得到摘要。不读整个文件。
   - 末尾：从文件末尾按 64 KB 的块向前读，按换行切出完整的行，从最后一行往前逐行解析，直到找到决定 `open_turn` 的记录（Claude Code：最后一条 `user` 或 `assistant` 记录；Codex：最后一条 `task_started` / `task_complete` / `turn_aborted`）。单行最长 16 MB，所以一行比 64 KB 大时继续向前读到它前面的换行；向前合计最多读 16 MB 或 64 行。`last_activity` 取读到的最后一条带时间戳的记录。
   - 回退：最后一行超过 16 MB，或在上限内没找到决定 `open_turn` 的记录时，`last_activity` 取文件修改时间，`open_turn` 为 true（末尾是超长的工具结果时，这一轮通常还没结束），计入 `oversized_lines`。
   - 已经在增量读取的文件（第 5 步）不再读末尾，这两个字段从解析器状态里直接取。
   - `.zst` 没有办法直接读末尾：先查摘要缓存（见「压缩会话的随机读取」），命中就用；没命中时只流式解压开头 256 KB，用开头和文件的 `stat` 生成摘要、标 `tail_pending`，照常发布。补齐末尾字段要整份解压，按需进行：列表发布后只主动补齐修改时间最近的 8 个压缩会话（M 系列上约 3 s，之后不再有后台工作，kernel 能在首帧后 30 秒内静止，[ADR 0017](../adr/0017.md)），其余等视图调 `complete`（会话行显示出来或被打开）时才补齐；同时最多 2 个补齐任务，补齐后按第 4 步再发布，并写入摘要缓存。冷的归档历史不会让应用一直忙碌。
3. 归位：先在 executor 上把去重后的 cwd 规范化（`std::fs::canonicalize`，解开 `/tmp` 一类的符号链接），失败（目录已不存在）为 `Missing`；`SessionSummary::cwd` 仍保留日志里的原样路径，规范路径只用于归位。对规范路径先调 `Projects::locate`（只查内存，匹配已打开的项目，取最深的根），匹配到就记为 `Place::Located`，所以已打开的文件夹项目的子目录也归到该项目；没有匹配时再调 `Projects::resolve`（阻塞，在 executor 上），记为 `Place::Resolved`。结果按 cwd 缓存，目录变化或收到 `ProjectEvent`（项目打开、关闭、worktree 变化）时对受影响的 cwd 重新归位；`locate` 返回 `NotLoaded` 时按项目设计的规则处理：路径在非当前 worktree 时调 `Projects::resolve_package`（阻塞，在 executor 上）取这个 worktree 自己的子项目；在当前 worktree 或加载中的文件夹项目时，先记下项目与 worktree，等下一次 `Changed` 再补子项目。cwd 不存在时为 `Missing`。
4. 发布：整体替换 `Arc<SessionList>`，发出 `SessionsChanged`。
5. 监听：`fs_watch` 递归监听每个会话目录；会话根目录（`<CLAUDE_CONFIG_DIR>/projects`、`<CODEX_HOME>/sessions`、`archived_sessions`）还不存在时（第一次使用），监听它最近的已存在的上级目录，根目录出现后改为递归监听它并按第 2 步扫描一次。事件合并 250 ms 后处理：新文件按第 2 步读摘要；已有文件只读游标之后追加的部分更新摘要；删除的文件按「不变量」里的规则核对同一 `SessionKey` 的所有候选路径：另一个候选仍在就改用它，所有候选都不在时才从列表移除。处理完再按第 4 步发布。
6. 消费：`usage` 按自己的游标 `read`；`review` 调 `turns` / `load`。
7. 轮次索引：`read` 解析记录时顺带更新这个会话的轮次索引（同一遍解析，不另读文件）：`from` 等于已有索引的 `cursor` 时在它之后追加，`reset` 时从头重建，其余情况不动索引。所以 `usage` 补读完历史之后，每个会话都已有缓存的索引。内存里按字节 LRU 保留（上限 8 MB）；索引更新后写入 `cache` 的应用级分库，键见 ADR 0026。`turns` 命中缓存时只解析缓存之后追加的部分。

线程：

- `sessions()`、`resume()`、`complete()`、`SessionFilter::hide` 不阻塞，任何线程可调用。
- `read`、`turns`、`load` 阻塞，只在 executor 上调用；调用方选优先级（`review` 打开会话时用 `Interactive`，`usage` 的补录用 `Background`）。
- 同一个会话的 `read` 可以被多个消费方同时调用，互不影响：解析状态在游标里，本 crate 不为消费方保存进度。
- 读取不持有任何锁；列表替换和索引 LRU 各用一把只在内存操作时短暂持有的锁。

## 性能

- 冷启动（ADR 0017）：首帧之前不调用本 crate；插件在首帧之后激活，发现在 `Background` 优先级运行。
- 空闲内存（ADR 0017）：`SessionList` 每个会话约 1 KB（1000 个会话约 1 MB）；轮次索引只为打开过的会话保留，LRU 上限 8 MB；读取用 64 KB 缓冲逐行处理，单行上限 16 MB，任何时候只持有一行；超长行边读边丢弃，不缓存。
- 磁盘：压缩会话的解压副本合计上限 1 GB，字节 LRU（见「压缩会话的随机读取」）。
- 打开一个会话的 Diff < 50 ms（[性能预算](tech-research.md#性能预算)）：本 crate 的份额是 `turns` 命中缓存加解析追加部分 < 10 ms，`load` 一次 < 1 ms（压缩会话从解压副本读，同样适用；第一次打开压缩会话除外，见上节）。
- 没有缓存索引时打开（首次启动、`usage` 的补读还没读到这个会话，或缓存被淘汰）：`turns` 解析整个文件，56 MB 的会话约 0.3 s，超出预算；界面与第一次打开压缩会话一样显示骨架或进度。`usage` 补读完成后，只有缓存被淘汰的会话会再遇到这种情况。这是已知限制（ADR 0026）。
- 预期耗时（M 系列，单线程，release）：发现 1000 个会话文件 < 500 ms；整文件解析 ≥ 200 MB/s；追加 4 KB 后 `read` < 1 ms；`.zst` 解压后解析 ≥ 100 MB/s。实现时用 bench 实测后校准，写进 bench 的预算表。
- `bench`（[#7](https://github.com/SoloJiang/lectern-app/issues/7)）新增：`agents/discover_1000`（合成 1000 个会话目录）、`agents/parse_throughput`（fixtures 拼成 50 MB）、`agents/append_4k`、`agents/turns_cached_50mb`、`agents/zst_throughput`、`agents/zst_load_decoded`（副本建好后 `load` 一次）。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)：

- 没有会话目录或目录为空：列表为空，不报错；Welcome 和项目主页按「还没有会话」显示。
- 目录不可读：记进 `Diagnostics::unreadable_dirs` 和日志，其余目录照常发现。
- 整个文件认不出格式：`Format::Unrecognized`；能读到 cwd 时照常列出、归位，`turns` 返回 `Unrecognized`，界面降级为按文件显示、隐藏轮次和说明，不弹错误。读不到 cwd 的不列出，只计数。
- 个别行不认识、不是合法 JSON、超过 16 MB：跳过并计数，游标越过它，`Format::Partial`；超长行里的改动拿不到，`review` 按 Drifted 规则处理。
- 文件末尾不完整的行（agent 正在写）：不读，等下一次变化。
- 文件被截断或改写：游标的前缀副本对不上，`Batch::reset` 为 true，从头读。
- 文件被删除：从列表移除，`SessionsChanged::removed` 带上它；`read` / `turns` 返回 `NotFound`。

## 测试

全部测试在 Linux 上运行（`just test -p lectern-agents`），不依赖真实的 agent、会话目录、网络和登录状态：

- 解析快照（insta）：每份 fixture 解析出的 `Record` 序列、`TurnIndex`、`SessionSummary` 各一个快照，`claude_*` / `codex_*`。
- 增量等价：每份 fixture 按随机切分（包括行中间）逐段追加到临时文件、每段后 `read` 一次，拼起来的记录与一次读完相同；游标经 `to_bytes` / `from_bytes` 往返后继续读，结果相同（`incremental_*`）。
- 改写与截断：截断、替换为另一个会话、改写游标之前的内容，`reset` 为 true 且记录从头给出；替换后旧文件里有、新文件里没有的记录不再出现在回放里（`reset_*`）。
- 读取进度：`max_bytes` 为 1 时每次调用仍消费一行；下一行比 `max_bytes` 大、但不到 16 MB 时整行返回；超过 16 MB 的行被跳过、计数、游标越过它；任何返回 `at_end == false` 的调用，新游标都大于旧游标（`progress_*`）。
- 压缩：同一份 Codex fixture 的 `.jsonl` 与 `.jsonl.zst` 记录相同；压缩前取的游标在压缩后继续有效；第一次 `read` / `turns` / `load` 建好解压副本，之后分批 `read` 和 `load` 都不再解码（用计数的解码器检查整个归档只解压一次）；压缩文件身份变化后副本重建；副本合计超过上限时按 LRU 删除、不删正在读取的副本；副本目录不可写时 `load` 结果不变（`zst_*`、`decoded_*`）。
- 路由：两个来源报告相同的会话 ID 时得到两个不同的 `SessionKey`；`SessionsChanged::source` 与键的 `source` 一致（`source_*`）。
- 继承前缀：Codex 子 agent 与分叉会话、Claude Code 分叉会话的 `inherited` 标记，轮次索引不含继承的轮次；没有 `subagent_history_start_ordinal` 的 `legacy` 分叉按来源文件里的全部轮次 ID 判断边界（含分叉的分叉），先于来源读到分叉时结果相同，找不到来源时 `inheritance_unknown` 为 true、来源之后出现时下一次 `read` 返回 `reset`；Claude Code 子 agent 文件里带父会话 `sessionId` 和自己 `agentId` 的记录不标 `inherited`，它的轮次和用量完整交出（`inherited_*`）。
- Codex 用量只取一路：第一条 `token_usage_record` 之后的 `CodexTokenCount` 标 `superseded`，之前的不标；没有 `token_usage_record` 的线程都不标；旧版本开始、升级后续接的 fixture 里，切换前后的用量合计等于逐响应的真实合计（`codex_usage_stream_*`）。
- Claude Code 同一响应的多行 usage 逐步变完整时键相同，带 advisor 的行与不带的行键也相同（`usage_keys_*`）。
- 恢复命令：`resume` 只给出 `ResumeCommand`；非 UUID 的会话 ID 被 `terminal` 拒绝的规则在 `terminal` 测试，本 crate 测试子 agent 和认不出格式的会话返回 None（`resume_*`）。
- 末尾字段：最后一条记录大于 64 KB（至 16 MB）时仍能取到它的时间戳和 `open_turn`；末尾跟着非消息记录时向前找到最后一条消息记录；最后一条记录超过 16 MB 时按「数据流与线程」第 2 步的回退规则；已在增量读取的文件不再读末尾（`tail_*`）。
- 压缩会话的补齐：冷缓存下发现 100 个 `.zst` 之后，只补齐最近的 8 个，kernel 在补齐完成后静止；对其余会话调 `complete` 才补齐（`complete_*`）。
- 子 agent：父会话、`toolUseId` 关联，`meta.json` 缺失时 `tool_call` 为 None（`subagent_*`）。
- 去重键：Claude Code 同一响应的多行给出相同的 `UsageKey`；两个 Codex 线程写出相同的 `total_token_usage` 时键不同，同一线程重复写出的相同累计值键相同；键里没有 `SourceId`；`info` 为 null 的 `token_count` 只给出 `RateLimits`（`usage_keys_*`）。
- 工具分类：分类表每一行一个用例，表外工具名归 `Other` 并计数（`tool_kind_*`）。
- 异常：不认识的类型、坏行、超长行、末尾不完整的行、认不出格式的文件（`format_*`）。
- 发现与监听：临时目录加 `fs_watch` 的内存模拟后端，新增、追加、删除、`.jsonl` 与 `.zst` 并存、`sessions/` 与 `archived_sessions/` 并存、`SessionFilter` 隐藏；没有缓存摘要的 `.zst` 先以 `tail_pending` 发布、整份解压之前列表已可用，后台补齐后再发布一次；摘要缓存在适配器版本或摘要格式版本变化后不命中；会话根目录在启动时不存在、之后被创建并写入第一个会话时，会话出现在列表里（`discover_*`、`watch_*`）。
- 归位：模拟的 `Projects`，相同 cwd 只 `resolve` 一次，cwd 不存在为 `Missing`；已打开项目里的 cwd 为 `Located` 并带子项目，非当前 worktree 的 `NotLoaded` 由 `resolve_package` 补齐；经符号链接记录的 cwd 归到已打开的项目，`cwd` 字段保留原样（`place_*`）。
- 交互式 shell：`exec_command` 的 `session_id` 出现在 `ToolFinished::exec_session` 上，之后的 `write_stdin` 的 `ToolKind::Execute::exec_session` 与它相同（`tool_kind_*`）。
- 轮次索引缓存：命中后只解析追加部分；适配器版本变化后不命中（`index_cache_*`）。
- 插件：`TestKernel` 注入模拟的 `Projects`，激活后提供两个 `SessionSource`，首次发现后发出 `SessionsChanged`（`plugin_*`）。

本 issue 的代码不调用外部命令行。Analyze 的后续 issue 按根目录 AGENTS.md 的规定测试：在 PATH 最前面放一个假命令，记录参数、标准输入和工作目录，按夹具回放 JSON 行输出。

## 未决问题

1. **只在 shell 里设置的会话目录。** 从 Dock 启动的应用读不到 `.zshrc` 里设置的 `CLAUDE_CONFIG_DIR`、`CODEX_HOME`，这些用户的会话发现不了。选项：
   - (a) P1 只用进程环境和默认目录（倾向）；Analyze 的后续 issue 会实现「经监护进程查询一次登录 shell 环境」，届时会话发现一并使用。
   - (b) 在设置里加「额外的会话目录」。

   默认按 (a) 实现。
2. **Codex 已归档的会话。** 选项：
   - (a) 列出并标 `archived`，项目主页默认不显示，用量照常统计（倾向）。
   - (b) 与普通会话一样显示。

   本 crate 两种都只需交出 `archived`，默认按 (a)，由 `project_home` 的设计确认。

## 验收

分四步，每步一个 PR。前三步写 `Refs #13`，最后一步写 `Closes #13`。

1. **`services` 的会话接口**（改 `lectern-services`，[#71](https://github.com/SoloJiang/lectern-app/pull/71)）
   - `SessionSource`、`SessionsChanged` 和「数据结构」里的类型在 `services::session` 里，`cargo check -p lectern-services` 通过。
   - 每个类型的 JSON 往返测试通过（`just test -p lectern-services` 的 `session_*`）。
2. **Claude Code 适配器与增量读取**
   - `just test -p lectern-agents` 里 `claude_*`、`incremental_*`、`reset_*`、`progress_*`、`source_*`、`subagent_*`、`usage_keys_*`、`tool_kind_*`、`format_*`、`resume_*`、`tail_*` 通过，快照已逐个看过；Claude Code 部分的 `inherited_*` 通过。
   - `crates/agents/tests/fixtures/claude-code/` 下每个版本目录都有 `SOURCE.md`。
   - `rg 'serde_json::Value' crates/agents/src` 只出现在测试代码里。
   - `agents/parse_throughput`、`agents/append_4k` 达到「性能」里的目标（`bench` 的设计文档合并之前，以 `crates/agents/tests/perf_smoke.rs` 里被忽略的测试运行，见「与设计的差异」）。
3. **Codex 适配器**
   - `codex_*`、`codex_usage_stream_*`、`zst_*`、`decoded_*`、`inherited_*` 通过；`just deny` 通过（`ruzstd`）。
   - `agents/zst_throughput`、`agents/zst_load_decoded` 达标。
4. **发现、监听、归位与插件**（依赖 `fs_watch` 的目录监听和 `cache` 的应用级分库）
   - `discover_*`、`watch_*`、`place_*`、`index_cache_*`、`complete_*`、`plugin_*` 通过。
   - `python3 scripts/check_architecture.py` 通过；`rg gpui crates/agents/Cargo.toml` 没有结果。
   - `agents/discover_1000`、`agents/turns_cached_50mb` 达标；macOS 上冷启动基准（#7）不因本插件变化。

## 实际验证

第 1 步（`services` 的会话接口），在 Linux 上运行（`services` 不依赖 GPUI，macOS 上是同一份代码；没有在 macOS 上实测）：

- `just check`（格式、架构规则、文档链接、clippy `-D warnings`）、`just test`（Linux 上不依赖 GPUI 的全部 crate，301 个测试）、`just doctest`、`just deny` 全部通过；`cargo check -p lectern-services` 没有警告。没有新增依赖，`services` 仍只依赖 `lectern-kernel` 和 `serde`。
- `just test -p lectern-services`（21 个测试，其中 11 个 `session_*`）：
  - `session_source_service_id_and_multiple`：`<dyn SessionSource as Service>::ID == "session-source"`、`MULTIPLE == true`（编译期断言），以及 `service::<dyn SessionSource>()` 的键。
  - `session_source_is_a_shared_trait_object`：一个空实现确认 `Arc<dyn SessionSource>` 的每个方法可调用，`SessionList`、`Batch`、`TurnIndex`、`Content` 是 `Send + Sync`。
  - `session_events_round_trip`：`SessionsChanged` 实现 `Event` 与 `Clone`，经 serde_json 往返后相等。
  - `session_errors_and_resume_commands_round_trip`：`SessionError`、`ResumeCommand` 的每个变体。
  - `session_list_round_trips`：含 `Place` 每个变体、`Agent` 每个变体、`Format` 每个变体、带与不带 `tool_call` 的 `Parent`、非空 `Diagnostics` 的 `SessionList`，以及空列表、`Timestamp`、`SourceId`、`SessionKey`。
  - `session_list_get_and_in_project`：`get` 按来源和 ID 查找；`in_project` 收 `Located` 与 `Resolved`、含子 agent 会话、保持从新到旧的顺序。
  - `session_cursor_bytes_round_trip`：`START` 与带状态的游标经 `to_bytes` / `from_bytes` 和 serde 往返；空输入、其他版本、固定字段的每一种截断、非 UTF-8 的来源 ID 返回 None，serde 反序列化报错。
  - `session_batch_round_trips`：`Record` 每个变体（`ToolKind`、`ToolStatus`、`UsageKey`、`RawTokens` 各变体，带与不带 `advisor`、`RateLimitSample` 带与不带窗口）包在 `Positioned` 里的 `Batch`。
  - `session_tool_and_usage_types_round_trip`、`session_turn_index_round_trips`（`TurnItem` 每个变体、已结束与未结束的 `ToolCall` 和 `Turn`、`ContentRef` 的极值）、`session_content_round_trips`（`Content` 与 `EditOp` 每个变体）。
- `services` 没有用 insta：类型都是手写的样本，用相等断言验收。

第 2 步（Claude Code 适配器与增量读取），在 Linux 上运行（`agents` 不依赖 GPUI；没有在 macOS 上实测）：

- `just check`、`just test`（Linux 上不依赖 GPUI 的全部 crate，395 个测试通过，2 个忽略）、`just doctest`、`just deny`、`python3 scripts/check_architecture.py` 全部通过；`rg gpui crates/agents/Cargo.toml` 没有结果；`rg 'serde_json::Value' crates/agents/src` 只出现在 `#[cfg(test)]` 的 `redact.rs`。
- `just test -p lectern-agents`：64 个通过，1 个忽略（手动运行的 `redact_file_from_env`）。验收要求的前缀：`claude_*` 6 个（含 12 个快照：4 份 fixture 各自的 `Record` 序列、`TurnIndex`、`SessionSummary`，逐个看过）、`incremental_*` 2 个（每份 fixture 8 个种子的随机切分，含行中间，每段之后用随机的 `max_bytes` 读到末尾，游标经 `to_bytes` / `from_bytes` 和 serde 往返；记录、游标和用 `turns_after` 逐段追加的索引都与一次读完相同）、`reset_*` 4 个、`progress_*` 4 个（含现场生成的 16 MB 以上的行）、`source_*` 1 个、`subagent_*` 2 个、`usage_keys_*` 2 个、`tool_kind_*` 8 个（Claude Code 分类表每一行一个，表外工具名计数，结果状态）、`format_*` 8 个、`resume_*` 1 个、`tail_*` 10 个、`inherited_*` 3 个（Claude Code 的分叉与子 agent）。另有 `redact_*` 5 个（白名单脱敏工具）和行读取、游标状态编码、时间戳的单元测试。
- `crates/agents/tests/fixtures/claude-code/2.1.302/SOURCE.md` 写明 fixtures 是合成的、模拟的版本和覆盖的记录。
- `cargo test -p lectern-agents --features test-support --release --test perf_smoke -- --ignored`（Linux 容器，x86_64，release，单线程；不是预算规定的 M 系列）：`agents/parse_throughput` 268–296 MB/s（预算 ≥ 200 MB/s），`agents/append_4k` 0.014 ms（追加 4250 字节，预算 < 1 ms），都达标。fixtures 的行短（几百字节），比带大段工具结果的真实会话解析得慢，这个吞吐是偏保守的估计。

第 3 步（Codex 适配器），在 Linux 上运行（`agents` 不依赖 GPUI；没有在 macOS 上实测）：

- `just check`、`just test`（Linux 上不依赖 GPUI 的全部 crate，449 个测试通过，3 个忽略）、`just doctest`、`just deny`（新增 `ruzstd` 0.9.1 与它的 `twox-hash` 2.1.5，都是 MIT）、`python3 scripts/check_architecture.py` 全部通过；`rg gpui crates/agents/Cargo.toml` 没有结果；`rg 'serde_json::Value' crates/agents/src` 仍只出现在 `#[cfg(test)]` 的 `redact.rs`。
- `just test -p lectern-agents`：118 个通过，2 个忽略（`redact_file_from_env` 和 release 下手动运行的 `perf_smoke`）。验收要求的前缀：`codex_*` 12 个（含 15 个快照：5 份 fixture 各自的 `Record` 序列、`TurnIndex`、`SessionSummary`，逐个看过）、`codex_usage_stream_*` 3 个（切换前后的合计等于最后的 `total_token_usage`；没有 `token_usage_record` 的线程都不标；子 agent 重放的父线程记录不让自己的被取代）、`zst_*` 8 个（含多帧解码的单元测试；`.jsonl` 与 `.jsonl.zst` 的记录、索引和每个引用的 `load` 相同；压缩前的游标压缩后续读不重置；分批 `read` 和 `load` 只解压一次；身份变化后重建、旧副本删除；摘要标 `tail_pending`、`complete` 补齐；损坏与缺失的归档）、`decoded_*` 3 个（含副本文件名；超过预算时删最旧的、不删正在使用的；目录不可写时退回从头解码、一次读到末尾、`load` 结果不变）、`inherited_*` 的 Codex 部分 7 个（摘要不为来源解压、不扫描来源；按 `ordinal` 的子 agent；按来源文件全部轮次 ID 的 `legacy` 分叉与分叉的分叉；先于来源读到分叉；找不到来源时 `inheritance_unknown`、来源出现后下一次 `read` 返回 `reset`；回放中途来源消失）。另有 Codex 部分的 `incremental_*`、`reset_*`、`progress_*`、`tool_kind_*`、`usage_keys_*`、`resume_*`（含没有父线程 ID 的子 agent）、`tail_*` 和 `redact_codex_*`（脱敏前后的 Codex fixtures 解析出同样的记录形状）。
- `crates/agents/tests/fixtures/codex/0.140.0-legacy/` 和 `0.140.0-paginated/` 各有 `SOURCE.md`，写明 fixtures 是合成的。
- `cargo test -p lectern-agents --features test-support --release --test perf_smoke -- --ignored`（Linux 容器，x86_64，release，单线程；不是预算规定的 M 系列；运行 3 次）：`agents/zst_throughput` 172–177 MB/s（预算 ≥ 100 MB/s，按解压后的字节计，含解压与解析），`agents/zst_load_decoded` 0.006–0.009 ms（预算 < 1 ms，副本建好之后 680 次 `load` 的中位数），都达标；同一次运行里 `agents/parse_throughput` 261–284 MB/s、`agents/append_4k` 0.015–0.019 ms。归档用 `ruzstd` 的最快档压缩（Codex 用 C 的 zstd，压缩级别不同，解压速度会有差别），内容是重复拼接到 50 MB 的合成 fixtures，行短，这个吞吐是偏保守的估计。

没有验证到的：第 4 步的验收条件；bench 没有在 macOS 上运行；没有拿真实的 Claude Code 和 Codex 日志验证（fixtures 是合成的，见下面「与设计的差异」）；没有用 C 的 zstd 压缩的真实归档测试解码（`ruzstd` 实现的是标准格式，测试只用它自己的编码器）。

## 与设计的差异

第 1 步：

- `Cursor` 的编码定在 `services`：`from_bytes` 只检查序列化格式、`read` 检查来源 ID 和适配器版本（上文「游标与批量读取」），所以外层格式只能由 `services` 规定。外层是版本字节、来源 ID、适配器版本和偏移；偏移之前的 256 字节副本和解析器状态是适配器自己的 `state` 字节。为此加了供提供者使用的 `new` 与访问方法，已写进上文。`offset` 也让测试能检查「新游标大于旧游标」。serde 形式是 `to_bytes` 的字节（JSON 里是数字数组），不认识的格式反序列化报错。
- `ContentRef` 加了供提供者使用的 `new` 与访问方法，已写进上文。行长度的访问方法叫 `line_len`，不叫 `len`（clippy 的 `len_without_is_empty`）。
- 标准派生：设计没有写，按 `services::project` 的做法定下，已写进上文「服务接口」。`Diagnostics`、`SessionList` 另派生 `Default`（提供者启动时的空列表）。
- `SessionList::get` 是线性查找，`in_project` 收 `Place::Located` 与 `Place::Resolved`，已写进上文。它们与 `Cursor` 的编码是 `services::session` 里仅有的方法实现，`crates/services/AGENTS.md`「边界」的例外里补上了。
- `SessionFilter` 和 `testing::parse_file` 不在 `services`：它们是上文「本 crate 的公开项」，属于 `agents`（`SessionFilter` 带锁和行为），在第 4 步、第 2 步实现。
- 模块划分：`services::session` 分为 `list`（键、`SessionList`、`SessionSummary`、`Place` 等）、`read`（`Cursor`、`Batch`、`Positioned`）、`record`（`Record` 与工具、用量、额度类型）、`turns`（轮次索引、`ContentRef`、`Content`、`FileEdit`），全部从 `lectern_services::session` 导出；`SessionSource`、`SessionError`、`ResumeCommand`、`SessionsChanged` 在模块根。只是文件划分，接口不变。

第 2 步：

- **fixtures 是合成的，脱敏的真实样本推迟。** 设计要求每个版本一份脱敏的真实样本；这一步的 `claude-code/2.1.302/` 全部按「外部格式」记下的记录形状和公开的 Claude Code JSONL 格式手工构造（占位文本、假 ID、假模型名，路径在 `/Users/dev/fixture/` 下），`SOURCE.md` 写明了这一点。人提供真实日志之后再补。字段白名单脱敏工具已经按设计实现为测试辅助（`src/redact.rs`，`#[cfg(test)]`）：保留结构、取值为枚举的字段、数字、时间戳和 token 数，ID 换成形状相同、同值同换的假 ID，路径移到 `/Users/dev/fixture/` 下、每级换成等长的占位名，文本换成等长的占位内容并保留解析器依赖的开头标记；在合成输入上做了单元测试，并验证脱敏前后的 fixtures 解析出同样的记录形状和计数。用被忽略的测试 `redact_file_from_env` 手动生成。脱敏只按结构字段保留取值：工具输入、`toolUseResult` 和 `tool_result` 的内容是任意 JSON，里面的 `name`、`type` 一类键也照常替换，工具名只在 `tool_use` 块上保留。
- **未用真实日志验证的假设。** 适配器假定同一个会话文件里每条 `user` / `assistant` 记录的 `uuid` 只出现一次（压缩、续接复制进来的历史由「继承前缀」处理，`sessionId` 不同）。拿到真实样本后要核对这一点；如果上游会在同一文件里重写同一 `uuid` 的记录，再按 `uuid` 去重（解析器状态要随游标保存已见的 `uuid`）。
- **库层，不实现 `SessionSource`。** 第 2 步交出「本 crate 的公开项」里的 `claude::ClaudeCode`：按会话文件读取、建索引、读正文、生成摘要；按 `SessionKey` 找文件、会话列表、`SessionsChanged` 和插件都在第 4 步。为此 `read` 返回 `Parsed`（`Batch` 加 `ParseStats`），`summarize` 返回摘要和计数，由第 4 步并进 `Diagnostics`。`source_*` 里「`SessionsChanged::source` 与键的 `source` 一致」随事件一起移到第 4 步；这一步测了键带提供者的来源 ID、另一个来源的游标从头读、用量键与来源无关。`tail_*` 里「已在增量读取的文件不再读末尾」是第 4 步发现时的选择；这一步提供 `activity(cursor)`，并测了它与读末尾的结果相同。「数据流与线程」第 7 步（`read` 顺带更新内存与 `cache` 里的索引）同样在第 4 步，这一步提供 `turns_after` 从已有索引的游标继续。
- **`reset` 的一批游标可以小于 `from`。** 「游标与批量读取」原来写 `at_end` 为 false 时游标一定大于 `from`；从头回放的第一批做不到，已在上文改为 reset 的一批例外。`services` 里 `Batch::at_end` 的文档注释还是旧说法，留给下一个改 `services` 的 PR 一并改。
- **表外工具名的计数在 `ParseStats::unknown_tools`。** `Diagnostics` 没有这一项，`services` 不在这一步改动；它只进日志，不使格式变成 `Partial`（没有记录被跳过）。所有归入 `Other` 的名字都计数，最多 32 种。
- **轮次划分的补充。** `[Request interrupted by user` 开头的 `user` 文本（中断说明）不开新轮次，按 `Injected` 处理。子 agent 文件里自己的第一条 `user` 记录总是第 1 轮的 prompt。没有 `uuid` 的 prompt 当作 `Injected`。自己的第一个 prompt 之前的内容不进索引，记录的 `turn` 为 None。
- **记录引用的轮次。** 继承前缀里的记录引用继承前缀里的轮次，自己的记录引用自己的轮次，两边分开记，所以 `usage` 排除继承记录后不会把自己的用量归到继承的轮次。
- **继承前缀之后补一条 `Context`。** 上一条 `Context` 是在继承前缀里给出的，而自己的第一条记录上下文没变时，也给出一条不标 `inherited` 的 `Context`：只看自己记录的消费方同样有 cwd 和模型。
- **结果状态的判断顺序与识别方式。** 设计没写怎样识别「权限被拒」。实现：`toolUseResult.interrupted` 为 true 或结果文本以 `[Request interrupted by user for tool use]` 开头是 `Interrupted`；否则结果文本以 `The user doesn't want to proceed with this tool use` 开头或 `toolUseResult` 是 `User rejected tool use` 是 `Denied`；否则 `is_error` 为 true 是 `Failed`；其余 `Completed`。
- **`open_turn` 的具体规则。** 取最后一条 `user` / `assistant` 记录：`assistant` 的 `stop_reason` 不是 `end_turn` 时为 true（有工具调用没结果时 `stop_reason` 是 `tool_use`，已包含在内）；`user` 记录（工具结果、prompt、通知）为 true，`isMeta`、本地命令输出的包装和中断说明为 false（`/exit` 之后的会话不显示为进行中）。读取时的解析器状态和读末尾用同一条规则。读末尾时一直读到文件开头也没找到消息，同样按回退规则处理。
- **Claude Code 轮次的 `ended_at`。** 设计只说 Claude Code 没有明确的轮次结束。实现：`stop_reason` 为 `end_turn` 的 `assistant` 记录、且这一轮没有还在等结果的工具调用时，记为它的时间戳；之后这一轮又出现工具调用时清空。被中断的轮次为 None。
- **摘要读开头时找齐就停。** 发现 1000 个会话要在 500 ms 内，所以开头只读到找齐会话头、自己的第一条记录和第一个 prompt 为止（仍不超过 256 KB），摘要的 `format` 和计数只反映读过的行；末尾的超长记录计入 `oversized_lines`，使格式为 `Partial`。`cwd`、`started_at`、`agent_version`、`logged_branch` 取自己的第一条 `user` / `assistant` 记录，没有时取会话头；标题取自己的第一个 prompt，没有时取第一个 prompt（分叉会话的标题是它自己的 prompt）。
- **认不出格式时的读取。** 会话头之前的 `user` / `assistant` 记录计为坏行（没有 cwd 和归属就无法交出）。在 256 KB 之后开始的行仍没见过会话头时，`read` 交出空的一批、游标不动（`reset` 时为 `START`）、`at_end` 为 true；读到末尾还没有会话头时（含空文件），`turns` 返回 `Unrecognized`。
- **`load` 的细节。** 引用对不上文件（越界、不在行首、没有这一部分）返回 `SessionError::Io`。`Write` 的输入看不出是否新建，`created` 为 false；结果里 `toolUseResult` 的 `type` 是 `create` / `update` 时给出 `Write`（`originalFile` 为 null 是新建），否则有 `structuredPatch` 时给出 `Patch`。`NotebookEdit` 的输入和其他工具的输入给出原样的 JSON 文本，失败的调用给出结果文本。
- **advisor 的模型。** 设计写「模型取 `advisorModel`」，没写在哪一层：依次取 `iterations[]` 那一项的 `advisorModel`、记录顶层的 `advisorModel`、那一项的 `model`；都没有时不给出 `advisor`。
- **路径在词法上规范化。** 相对路径按 cwd 拼接之后，再在词法上去掉 `.` 和 `..`（不访问文件系统）。
- **`serde_json` 打开 `raw_value` feature。** 不是新依赖：工具输入以借用的原样 JSON 保留，只有分类表里的工具才解析，`load` 原样交出。没有新增依赖，`just deny` 通过。
- **`testing::parse_file`。** Codex 在第 3 步之前返回 `Unrecognized`；单元测试里不打开 `test-support` 也能用。
- **插件标记。** ADR 0025 规定在第一个实现 `Plugin` 的 PR 里加 `plugin = true`，所以这一步不加，第 4 步加。
- **bench 的形式。** `bench` crate 的设计文档（[#7](https://github.com/SoloJiang/lectern-app/issues/7)）还没有写，按 `crates/bench/AGENTS.md` 不能先加基准代码。这一步把 `agents/parse_throughput`、`agents/append_4k` 写成 `crates/agents/tests/perf_smoke.rs` 里被忽略的测试（`required-features = ["test-support"]`），打印实测值和预算，超出时失败；`bench` 的设计文档合并后再移进 `lectern-bench`。`agents/parse_throughput` 用的是重复拼接到 50 MB 的合成 fixtures。bench 第 1 步已把它们移进 `crates/bench`（[bench 设计「迁移」](bench.md#迁移)），拼接会话和取约 4 KB 完整行的代码在 `testing::synthetic_session`、`testing::append_chunk`，`tests/perf_smoke.rs` 随之删除。

第 3 步：

- **fixtures 是合成的。** 与第 2 步相同：`codex/0.140.0-legacy/`、`codex/0.140.0-paginated/` 全部按「外部格式」记下的记录形状和公开的 Codex 源码（提交 `3b8f43a`）里的序列化形状手工构造，没有读取本机的任何会话日志，`SOURCE.md` 写明了这一点；真实样本等人提供后再补。`.jsonl.zst` 不提交，测试在临时目录里用 `ruzstd` 的编码器从合成的 `.jsonl` 生成。`src/redact.rs` 为 Codex 补了白名单：ID（`call_id`、`turn_id`、`thread_id`、`response_id`、`forked_from_id`、`parent_thread_id` 等）、路径（`workdir`、`working_directory`、`move_path`，以及 `changes` 的键）、分支（`git.branch`）、取值为枚举的键（`history_mode`、`cli_version`、`originator`、`plan_type`、`limit_id`），工具名在 `function_call` / `custom_tool_call` 上保留；`arguments` 是装着 JSON 的字符串，按任意内容处理里面的值、键保留；工具输出按任意内容处理，只保留 `Output:` 之前的退出码与 shell 会话 ID 两类元数据行，以及 JSON 文本输出里的数字。KEEP 仍只用于结构字段，不进入工具参数和输出。测试检查脱敏前后的 Codex fixtures 解析出同样的记录形状。
- **库层，不实现 `SessionSource`；来源会话经 `Lineage` 查找。** 第 3 步交出上文「本 crate 的公开项」里的 `codex::Codex`，形状与 `claude::ClaudeCode` 相同，`Parsed`、`Activity` 两个适配器共用。库层没有 `SessionList`，所以分叉会话的来源经 `Lineage`（线程 ID 到文件）查找，第 4 步按 `SessionList` 实现。来源文件里全部 `task_started` 的 `turn_id` 由扫描来源文件得到（只看这一种记录，不做来源自己的继承判断），按来源文件在内存里缓存（最多 64 个），来源追加后从上次的位置接着扫；「与轮次索引一起缓存」进 `cache` 是第 4 步。
- **会话 ID 取文件名。** Codex 用 `session_meta.id`（线程 ID）给文件命名（`rollout_file_name.rs`），所以 `session_file` 只看路径：`rollout-<时间>-<线程 ID>[_<rollout ID>].jsonl` 或 `.jsonl.zst`，线程 ID 必须是 UUID 的规范写法，`archived_sessions/` 之下为 `archived`。同一线程有多个 rollout 文件（带 rollout ID）时怎样选，是第 4 步「不变量」里的候选路径规则。用量键里的线程 ID 取 `session_meta.id` 和 `token_usage_record.thread_id`。
- **解压副本与它的预算在这一步。** 副本放在 `decoded_dir/<来源 ID>/`，文件名带会话 ID 和压缩文件的设备号、inode、大小、修改时间；身份变了就删掉这个会话的旧副本重建。字节 LRU 的「最后访问时间」是副本的修改时间，每次使用时更新（访问时间常被挂载选项关掉）；「正在读取」是本进程登记的正在使用的副本，不知道别的进程。「同时建好轮次索引」随第 4 步的索引缓存一起做。副本写不出来时退回从头解码，但本 crate 还没有日志设施，原因不记录。解码的内存不只是 64 KB 的读缓冲：`ruzstd` 按帧头声明的窗口分配解码窗口（几 MB，上限是它的默认值 128 MB），不随文件大小增长。流里有多个 zstd 帧时依次解码（Codex 只写一个帧）。
- **`ContentRef::part` 在 Codex 里的含义** 已写进上文「本 crate 的公开项」之后。`load` 给出补丁输入里的改动时，相对路径要拼上调用时的 cwd，而一行里没有 cwd：向前最多 64 行找最近的 `turn_context`，找不到时取 `session_meta` 的 cwd。cwd 中途变过、调用又离 `turn_context` 超过 64 行时，输入里的相对路径会拼错，是已知限制；`ToolKind::Edit` 的路径在解析时按准确的 cwd 拼接，结果里 `patch_apply_end` / `FileChange` 的路径本来就是绝对路径，`review` 优先用结果。从头解码时（副本写不出来）一路记下 cwd，是准确的。`changes` 按路径排序交出；补丁的 `Update` 交出文件头之后的 hunk 行（去掉 `*** End of File`），`Add` 交出去掉 `+` 的内容。
- **工具分类的补充。** `shell_command`（`command` 是字符串）归 `Execute`；派出子 agent 的工具是 `spawn_agent`；`shell` 的 `command` 不是 `-c` / `-lc` 脚本的形式时按 POSIX shell 的写法加引号拼接。`local_shell_call` 在日志里没有工具名，`ToolStarted::name` 记为 `local_shell_call`；它没有结果记录，轮次索引里的 `result` 为 None。`write_stdin` 的 `workdir` 取打开这个 shell 会话的 `exec_command` 的工作目录（在它的输出给出会话 ID 时记下），不知道时取当前 cwd。还在等结果的 `exec_command`、已知的 shell 会话、开始了的 `local_shell_call`、先到的补丁结果在游标状态里各最多记 64 个。
- **结果状态的补充。** 退出码除了 `Exit code: N`，还认 `exec_command` 在 `3b8f43a` 的输出头 `Process exited with code N`，只看 `Output:` 之前的行；JSON 文本的输出取 `exit_code`（旧版本 `shell` 的 `metadata.exit_code`、code mode 的结果）。会话 ID 取 `Process running with session ID N` 或 JSON 的 `session_id`，只放在 `exec_command` 的结束事件上。`3b8f43a` 不再写 `output.success`，失败的补丁的输出也没有退出码，所以 `apply_patch` 的状态取先于输出到达的 `patch_apply_end` / `FileChange` 的 `status`；轮次索引里补丁调用的 `result` 指向这条改动记录。
- **轮次与 prompt 的补充。** 一轮的 prompt 是这一轮里自己的第一条 `user_message`（`paginated` 为 `UserMessage`）；`task_started` 之前写出的先记下，接到下一个 `task_started` 上；同一轮里之后的用户消息（中途补充的输入）记为 `Injected`，`compacted` 的摘要也记为 `Injected`。`legacy` 不读 `UserMessage` / `AgentMessage` 条目，`paginated` 不读 `user_message` / `agent_message` 事件（按 `policy.rs` 本来也不会出现）。`Turn::ended_at` 取 `task_complete` / `turn_aborted` 的时间戳；没有 `turn_id` 的 `turn_aborted` 结束当前轮次。
- **继承前缀的补充。** 来源依次取 `forked_from_id`、`parent_thread_id`、`source.subagent.thread_spawn.parent_thread_id`。按 `ordinal` 判断时，没有 `ordinal` 的行算自己的。第一条 `session_meta` 是会话头，复制进来的来源 `session_meta` 不改变它。回放中途（继承前缀跨批读）来源消失时，下一次 `read` 返回 `reset`、按边界未定处理。「来源之后出现」以能读到来源为准（找到文件但读不了不算），不然每次 `read` 都会重置。没有 `ordinal` 的分叉或子 agent 找不到来源时一律标 `inheritance_unknown`，不论它是否真有继承前缀；摘要里来源的轮次集合还没有缓存时同样如此（见下面「摘要的补充」）。
- **只用一路用量的补充。** 切换只看 `thread_id` 等于 `session_meta.id` 的 `token_usage_record`（没有 `thread_id` 的算自己的），所以子 agent 重放的父线程记录不会让自己的 `token_count` 被取代。`CodexResponse` 的轮次取记录自己的 `turn_id`。负的 token 数按 0 交出。
- **摘要的补充。** 与 Claude Code 相同，开头只读到会话头和自己的第一个 prompt 为止。子 agent（`source.subagent` 或有 `parent_thread_id`）的 `Parent::tool_call` 为 None：子 rollout 里没有派出它的调用 ID（父会话 `spawn_agent` 的输出里有子线程 ID，要关联需要读父会话，留给第 4 步）。`{"subagent": "review"}`、`"compact"` 一类子 agent 没有父线程 ID，`SessionSummary` 没有「父会话未知的子 agent」这种写法；最小的改动是不改 `services` 的类型，照样给出 `parent`，`Parent::session.id` 为空字符串：它不指向任何会话，与「不变量」里父会话不在列表里的情况一样处理，`resume` 对所有带 `parent` 的会话（父会话已知或未知）都返回 None。摘要在发现阶段运行，不为分叉的来源会话解压或扫描：继承边界只用内存里已缓存的来源轮次集合（之前对这个来源 `read` / `turns` 时建立），没有缓存时按边界未定处理（`inheritance_unknown` 为 true，标题取第一个 prompt），第一次 `read` / `turns` 这个分叉时才扫描来源；第 4 步在那之后重新生成摘要（或从缓存的集合生成）。没有 `session_meta`、但有带 cwd 的 `turn_context` 时照常列出，`Format::Unrecognized`。
- **键的顺序。** 读取路径按 Codex 的写法先读 `type` 再按它选结构，不缓存；`type` 不在第一个键时（文件被别的工具重新序列化过，如脱敏工具）先留下其余键值的原样 JSON 再读，结果相同。
- **`ruzstd` 的版本与 feature。** `Cargo.lock` 里原来没有 `ruzstd`，取实现时 crates.io 上的最新版本 0.9.1，默认 feature（`std`，以及 `hash` 用 `twox-hash` 2.1.5 校验帧的校验和），都是 MIT，`just deny` 通过。测试还用它的编码器生成归档。
- **共用的代码。** `Parsed`、`Activity` 移到 `read` 模块（`claude`、`codex` 和 crate 根各自导出，`claude::Parsed` 的路径不变），词法路径拼接移到 `path` 模块；`IndexBuilder` 加了 Codex 用的「后补 prompt」「结果改指向改动记录」「明确结束轮次」，`finish_tool` 的结果引用改为可选。Claude Code 的行为不变（第 2 步的测试和快照不变）。
- **bench 的形式。** 与第 2 步相同，`agents/zst_throughput`、`agents/zst_load_decoded` 写在 `tests/perf_smoke.rs` 里；归档是合成的 Codex fixtures 重复拼接到 50 MB 后用 `ruzstd` 最快档压缩的。`agents/zst_throughput` 用不建副本的适配器（`testing::parse_file`）从头解压并解析一遍。bench 第 1 步把这两项一并移进 `crates/bench`，归档由 `testing::synthetic_rollout` 和 `testing::compress_zst` 生成。
