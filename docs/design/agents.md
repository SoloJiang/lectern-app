# agents 设计

状态：已接受，尚未实现。

对应 issue [#13](https://github.com/SoloJiang/lectern-app/issues/13)。agents 发现并解析 Claude Code 与 Codex 写在本机的会话日志，把每个会话按 cwd 归位到项目和 worktree，交出轮次结构、工具调用事件和原始用量记录（[技术调研「Agent 项目发现」](tech-research.md#1-agent-项目发现)、[ADR 0014](../adr/0014.md)、[ADR 0020](../adr/0020.md)）。读取方式与缓存键见 [ADR 0026](../adr/0026.md)。

## 范围

- 做：
  - 会话发现：枚举两种 agent 的会话目录，读每个文件的开头和末尾得到会话摘要，监听目录变化。
  - 解析：Claude Code 与 Codex 的 JSONL（Codex 另有 `.jsonl.zst`），按版本适配，不认识的记录跳过并计数。
  - 轮次结构：prompt、agent 说明、工具调用（含文件改动）按轮次组织，正文按引用延迟读取。
  - 子 agent 会话：与父会话、派出它的工具调用关联；Codex 子 agent 与分叉会话标出继承自父会话的前缀。
  - 按 cwd 归位：经 `services` 的 `Projects::resolve` 得到项目与 worktree。
  - 增量读取：游标之后的新行，供多个消费方各自按游标拉取。
  - 交给 `usage` 的三类记录：原始用量、工具调用的开始与结果事件（ADR 0020）、Codex 的额度快照。
  - 恢复会话的命令（`claude --resume <id>`、`codex resume <id>`），供 `terminal` 使用。
- 不做：
  - 用量去重、规范化、归属与聚合：`usage`。本 crate 交出日志里的原始数值和去重用的键。
  - 命令文本里 `cd`、`pushd`、`git -C` 的解析：`usage`（`crates/usage/AGENTS.md`）。
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
| `lectern-services` | `SessionSource` 接口与数据类型（本文定义，放在 `services::session`）；调用 `Projects::resolve` | 消费方只依赖接口（架构规则 7） |
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
    /// 会话目录：默认取 `$CLAUDE_CONFIG_DIR`（逗号分隔）或 `~/.claude` 与 `~/.config/claude`，
    /// `$CODEX_HOME` 或 `~/.codex`。测试传临时目录。
    pub roots: AgentRoots,
}
/// 两个插件：ID "session-claude-code" 与 "session-codex"，各提供一个 `dyn SessionSource`，激活条件 `AfterFirstFrame`，依赖 `dyn Projects`。
pub fn plugins(deps: AgentsDeps) -> [(Name, Box<dyn Plugin>); 2];
```

### 对其他 crate 的要求

- `fs_watch`：提供不过滤的递归目录监听，监视 git 仓库之外的目录（会话目录），事件只带路径，合并后交付。已写进 `crates/fs_watch/AGENTS.md`。
- `cache`：提供不属于任何仓库的应用级分库，存本 crate 的轮次索引（键见 ADR 0026），按字节 LRU，与其他条目一样可以随时删除重建。已写进 `crates/cache/AGENTS.md`。
- `services`：采纳本文「接口」一节定义的 `session` 模块，`services` 的设计文档引用本节，不重复定义。

## 接口

### 服务接口（`lectern-services` 的 `session` 模块）

类型都是不含行为的数据，能通过 JSON-RPC 序列化（serde 派生方式随 `services` 设计），以后的进程插件可以实现同一个接口。

```rust
/// 多实现服务：每种 agent 一个提供者，ACP（P3）和第三方会话来源再各加一个。
/// impl Service for dyn SessionSource { const ID = "session-source"; const MULTIPLE = true; }
pub trait SessionSource: Send + Sync {
    fn agent(&self) -> Agent;

    /// 当前会话列表。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程上可以调用。
    fn sessions(&self) -> Arc<SessionList>;

    /// 从 `from` 读到文件当前末尾的最后一个换行，最多读 `max_bytes` 字节（调用方循环读完，循环之间可以放弃）。
    /// 阻塞（读文件、解压、解析），只在 executor 上调用。
    /// 游标与文件对不上（被截断、被替换）时从头读，`Batch::reset` 为 true。
    fn read(&self, session: &SessionKey, from: &Cursor, max_bytes: u64) -> Result<Batch, SessionError>;

    /// 轮次索引。阻塞，只在 executor 上调用：内存命中时只复制 `Arc`；否则读 `cache` 里的索引、
    /// 再解析索引之后追加的部分；都没有时解析整个文件。会话还在写入时，返回截至当前末尾的索引。
    fn turns(&self, session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError>;

    /// 读一段正文（prompt、agent 说明、工具输入与结果、文件改动）。阻塞，只在 executor 上调用；
    /// 读一行并解析，与文件大小无关。
    fn load(&self, session: &SessionKey, at: &ContentRef) -> Result<Content, SessionError>;

    /// 恢复这个会话的命令。不阻塞。子 agent 会话和格式无法识别的会话返回 None。
    fn resume(&self, session: &SessionKey) -> Option<ResumeSpec>;
}

pub enum SessionError {
    /// 文件已删除（Claude Code 默认删除 30 天前的会话）。调用方从列表里去掉它；`usage` 已落盘的数据不受影响。
    NotFound,
    /// 读文件或解压失败。附原因，日志用。调用方在下一次 `SessionsChanged` 后重试。
    Io(Arc<str>),
    /// 整个文件认不出格式（见「状态与异常」）。`turns` 返回这个错误，`read` 返回空的 `Batch`。
    Unrecognized,
}

pub struct ResumeSpec {
    pub program: Arc<str>,      // "claude" / "codex"，由 terminal 在用户的 shell 里解析
    pub args: Arc<[Arc<str>]>,  // ["--resume", id] / ["resume", id]
    pub cwd: Arc<Path>,         // 会话开始时的 cwd
}
```

调用方：

- `welcome`、`project_home`、`worktrees` 在渲染时调 `sessions()`；收到 `SessionsChanged` 后安排重绘。
- `usage` 收到 `SessionsChanged` 后在 executor 上对变化的会话调 `read`，把记录和新游标在同一个 SQLite 事务里写入，所以重启后从自己的游标继续，不重复也不遗漏。
- `review` 在 executor 上调 `turns` 和 `load`，`review_view` 只读 `review` 的快照。
- `terminal` 调 `resume`。

### 事件

```rust
/// 在 executor 线程上、`SessionList` 替换之后发出。监听只更新状态或安排后台任务，不做 IO。
pub struct SessionsChanged {
    pub agent: Agent,
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
}
```

## 数据结构

### 会话与列表

```rust
pub enum Agent { ClaudeCode, Codex }

/// Claude Code：`sessionId`（文件名去掉 `.jsonl`）；子 agent 为 `agent-<agentId>`。Codex：`session_meta.id`（线程 ID）。
pub struct SessionKey { pub agent: Agent, pub id: Arc<str> }

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
    pub fn get(&self, key: &SessionKey) -> Option<&SessionSummary>;
    /// 归位到这个项目的会话（含子 agent 会话），按 `last_activity` 从新到旧。
    pub fn in_project<'a>(&'a self, project: &'a ProjectKey) -> impl Iterator<Item = &'a SessionSummary> + 'a;
}

pub struct SessionSummary {
    pub key: SessionKey,
    pub parent: Option<Parent>,
    pub file: Arc<Path>,
    /// 会话开始时的 cwd，原样取自日志。
    pub cwd: Arc<Path>,
    pub place: Place,
    pub started_at: Timestamp,
    /// 最后一条记录的时间戳；没有时取文件的修改时间。
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
    /// Codex `archived_sessions/` 下的会话。
    pub archived: bool,
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
    Resolved(Resolved),             // services::project 的类型：项目、worktree、相对路径
    /// cwd 已不存在。会话照常列出，不属于任何项目。
    Missing,
    Unresolvable(ResolveError),
}

pub enum Format {
    Known,
    /// 有记录被跳过（不认识的类型、坏行、超长行），数量见 `Diagnostics`。
    Partial,
    /// 开头 256 KB 里找不到会话头（Claude Code：带 `sessionId` 和 `cwd` 的记录；Codex：首行 `session_meta`）。
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

- 一个会话一个 `SessionKey`。Codex 的同一个会话同时有 `.jsonl` 和 `.jsonl.zst` 时读 `.jsonl`；`sessions/` 和 `archived_sessions/` 都有时读 `sessions/`。
- `parent` 指向的会话可能不在列表里（父会话文件已删除）。
- 被 `SessionFilter` 隐藏的会话不在列表里。

### 游标与批量读取

```rust
/// 不透明，可序列化。消费方原样保存，不解读。
pub struct Cursor { /* 见下 */ }

impl Cursor {
    pub const START: Cursor;
    pub fn to_bytes(&self) -> Vec<u8>;
    /// 版本不认识时返回 None，调用方从 `START` 读。
    pub fn from_bytes(bytes: &[u8]) -> Option<Cursor>;
}

pub struct Batch {
    /// 为 true 时记录从文件开头重新给出。消费方按记录里的稳定键（`UsageKey`、`ToolCallId`、`TurnId`）覆盖已有数据。
    pub reset: bool,
    pub records: Vec<Positioned<Record>>,
    pub cursor: Cursor,
    /// 已读到文件当前末尾。
    pub at_end: bool,
}

pub struct Positioned<T> {
    /// 这一行在（解压后的）文件里的字节偏移。同一会话内严格递增，是记录的顺序。
    pub pos: u64,
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
}

pub enum ToolKind {
    /// 写文件、改文件、应用补丁（含新建、删除、移动）。路径是绝对路径（相对路径按调用时的 cwd 在词法上拼接，不访问文件系统）。
    Edit { paths: Arc<[PathBuf]> },
    /// 执行命令。`command` 是交给 shell 的完整文本；`workdir` 是工具参数里的工作目录，没有时为调用时的 cwd。
    Execute { command: Arc<str>, workdir: Arc<Path> },
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
| `Execute` | `Bash`（`workdir` 为该记录的 `cwd`） | `function_call` 的 `shell`（`command` 数组；形如 `["bash", "-lc", 脚本]` 时取脚本）、`exec_command`（`cmd`、`workdir`）、`write_stdin`（写入的字符）、`local_shell_call` |
| `Read` | `Read`、`NotebookRead` | `view_image`（读文件靠 shell，按 `Execute` 交出） |
| `Search` | `Grep`、`Glob`（`path`）、`LS` | 无（同上） |
| `SpawnAgent` | `Task`、`Agent` | 派出子 agent 的工具（`spawn_agent` 一类，按 fixtures 补全） |

结果状态：Claude Code 的 `tool_result.is_error` 为 true 是 `Failed`，`toolUseResult.interrupted` 为 true 是 `Interrupted`，权限被拒是 `Denied`；Codex 的 `function_call_output.output.success == false` 是 `Failed`。

用量与额度（ADR 0014、ADR 0022）：

```rust
pub struct UsageRecord {
    pub at: Timestamp,
    pub turn: Option<TurnId>,
    pub model: Option<Arc<str>>,
    pub tokens: RawTokens,
    pub key: UsageKey,
    /// Claude Code 的 `isSidechain`。
    pub sidechain: bool,
    /// 会话日志。Analyze 的计量记录（后续 issue）用另一个变体，`usage` 按它区分「编写」与「审阅」。
    pub origin: UsageOrigin,
}

pub enum UsageOrigin { SessionLog }

/// 日志里的原始数值，不规范化：两家的口径不同，规范化成互不重叠的类别在 `usage` 做。
pub enum RawTokens {
    /// `message.usage`：`input_tokens` 不含缓存。
    Anthropic { input: u64, output: u64, cache_read: u64, cache_write: u64, cache_write_5m: u64, cache_write_1h: u64 },
    /// Codex `TokenUsage`：`input` 含 `cached_input`，`output` 含 `reasoning_output`。
    OpenAi { input: u64, cached_input: u64, cache_write: u64, output: u64, reasoning_output: u64 },
}

/// 去重用的键，去重规则在 `usage`（ADR 0014）。
pub enum UsageKey {
    /// 每个内容块一行、重复同一份 usage：同一个键可能出现多次，也可能跨文件出现。
    /// `advisor` 是 `usage.iterations[]` 里 `type: "advisor_message"` 的那一份，模型取 `advisorModel`。
    Claude { message_id: Arc<str>, request_id: Option<Arc<str>>, advisor: Option<(Arc<str>, RawTokens)> },
    /// `event_msg` / `token_count`：`tokens` 是 `last_token_usage`，`total` 是 `total_token_usage`。`info` 为 null 的事件不给出用量记录。
    CodexTokenCount { total: RawTokens },
    /// 顶层 `token_usage_record`（压缩上下文的用量），按 `response_id` 去重。
    CodexResponse { response_id: Arc<str> },
}

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

/// 指向文件里的一行和行内的一个部分。不透明。
pub struct ContentRef { /* pos: u64, len: u32, part: u16 */ }

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
- 继承前缀：`sessionId` 与文件所属会话不同的记录（分叉、续接时复制进来的历史），标为 `inherited`，不进入轮次索引。

**Codex**（来源：[openai/codex](https://github.com/openai/codex) 提交 `3b8f43a`（2026-10-17）的 `codex-rs/history/src/lib.rs`（`RolloutLine`、`RolloutItem`）、`codex-rs/protocol/src/protocol.rs`（`SessionMeta`、`EventMsg`、`TokenCountEvent`）、`codex-rs/protocol/src/models.rs`（`ResponseItem`）、`codex-rs/protocol/src/items.rs`（`TurnItem`）、`codex-rs/rollout/src/policy.rs`（哪些条目写入文件）、`codex-rs/rollout/src/compression.rs`（`.zst`））

- 位置：`<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-<时间>-<id>.jsonl`，冷文件被压缩为 `.jsonl.zst`；归档的在 `<CODEX_HOME>/archived_sessions/`。
- 每行 `{timestamp, ordinal?, type, payload}`。`type`：`session_meta`、`turn_context`、`response_item`、`event_msg`、`compacted`、`token_usage_record`、`inter_agent_communication_metadata`；其余（`world_state`、`retained_context` 等）已知忽略。
- `session_meta`（首行）：`id`、`cwd`、`cli_version`、`source`（含 `subagent`）、`parent_thread_id`、`forked_from_id`、`history_mode`（`legacy` / `paginated`）、`subagent_history_start_ordinal`、`git.branch`。
- `turn_context`：`turn_id`、`cwd`、`model`。
- 轮次：`event_msg` 的 `task_started`（别名 `turn_started`）到同一 `turn_id` 的 `task_complete`（`turn_complete`）或 `turn_aborted`。prompt 和 agent 说明：`legacy` 模式取 `event_msg` 的 `user_message` / `agent_message`；`paginated` 模式取 `item_completed` 里的 `UserMessage` / `AgentMessage`。`response_item` 里 `role: "user"` 的消息含环境说明和指令文件，不作为 prompt。
- 工具：两种模式都从 `response_item` 取 `function_call{name, arguments, call_id}`、`custom_tool_call{name, input, call_id}`、`local_shell_call`，结果取 `function_call_output` / `custom_tool_call_output`。`exec_command_begin/end`、`patch_apply_begin` 不写入文件；改动另取 `legacy` 的 `patch_apply_end.changes` 或 `paginated` 的 `item_completed` / `FileChange`。
- 用量：`event_msg` / `token_count` 的 `info.{last_token_usage, total_token_usage}`，模型取最近一条 `turn_context`；额度取同一事件的 `rate_limits.{primary, secondary}`。
- 继承前缀：子 agent 与分叉会话先重放来源的历史。有 `subagent_history_start_ordinal` 时，`ordinal` 小于它的行是继承前缀；否则到第一条自己的 `task_started`（或 `trigger_turn: true` 的 `inter_agent_communication_metadata`）为止（ADR 0014）。

兼容策略：适配器按记录形状判断，不按版本号分支；字段缺失时按上表的旧写法再试一次（如 `resets_in_seconds`、没有 `origin`），仍不行就跳过这一行并计数。上游格式变化时，加 fixtures、改适配器、在本节记下来源提交或版本。

fixtures（`crates/agents/tests/fixtures/`）：每个上游格式版本一份脱敏的真实样本，目录名写 agent 与版本（如 `claude-code/2.1.302/`、`codex/0.14x-legacy/`、`codex/<版本>-paginated/`），旁边的 `SOURCE.md` 记下版本、日期和覆盖了哪些记录。至少包括：Claude Code 的主会话加两个子 agent（一个带 worktree）、一次 `Edit` 失败、一次中断的 `Bash`、一个分叉会话；Codex 的 `legacy` 与 `paginated` 各一个、一个 `.jsonl.zst`、一个子 agent rollout、一个只有额度没有用量的 `token_count`。脱敏用测试辅助工具按字段白名单处理：保留结构、类型、ID 的相同关系、时间戳和 token 数，文本和路径替换为等长的占位内容，路径统一到 `/Users/dev/fixture/` 下。

### 子 agent 与继承前缀

- 子 agent 会话是独立的 `SessionSummary`，用 `parent` 指回父会话和派出它的工具调用，按自己的 cwd 归位（Claude Code 的子 agent 常在 `.claude/worktrees/` 下的另一个 worktree 里）。合到父会话的合计、在列表里折叠到父会话下，由 `usage` 和视图决定。
- 继承前缀里的记录照样交出、标为 `inherited`，由 `usage` 按 ADR 0014 只计自己的部分；轮次索引不含它们。

## 数据流与线程

1. 插件在首帧之后激活（`AfterFirstFrame`），`started` 里通过 `own_with` 提交发现任务（`Background`）。首帧之前不做任何事。
2. 发现：枚举会话目录（只列目录项，不读 `.prefix.json` 等无关文件），每个会话文件读开头最多 256 KB（找会话头、cwd、第一个 prompt）和末尾最多 64 KB（最后活动时间、`open_turn`），得到摘要；`.zst` 只解压开头。不读整个文件。
3. 归位：对去重后的 cwd 调 `Projects::resolve`（阻塞，在 executor 上），结果按 cwd 缓存到下一次目录变化；cwd 不存在时为 `Missing`。
4. 发布：整体替换 `Arc<SessionList>`，发出 `SessionsChanged`。
5. 监听：`fs_watch` 递归监听每个会话目录，事件合并 250 ms 后处理：新文件按第 2 步读摘要；已有文件只读游标之后追加的部分更新摘要；删除的文件从列表移除。处理完再按第 4 步发布。
6. 消费：`usage` 按自己的游标 `read`；`review` 调 `turns` / `load`。
7. 轮次索引：内存里按字节 LRU 保留（上限 8 MB）；索引更新后写入 `cache` 的应用级分库，键见 ADR 0026。`turns` 命中缓存时只解析缓存之后追加的部分。

线程：

- `sessions()`、`resume()`、`SessionFilter::hide` 不阻塞，任何线程可调用。
- `read`、`turns`、`load` 阻塞，只在 executor 上调用；调用方选优先级（`review` 打开会话时用 `Interactive`，`usage` 的补录用 `Background`）。
- 同一个会话的 `read` 可以被多个消费方同时调用，互不影响：解析状态在游标里，本 crate 不为消费方保存进度。
- 读取不持有任何锁；列表替换和索引 LRU 各用一把只在内存操作时短暂持有的锁。

## 性能

- 冷启动（ADR 0017）：首帧之前不调用本 crate；插件在首帧之后激活，发现在 `Background` 优先级运行。
- 空闲内存（ADR 0017）：`SessionList` 每个会话约 1 KB（1000 个会话约 1 MB）；轮次索引只为打开过的会话保留，LRU 上限 8 MB；读取用 64 KB 缓冲逐行处理，单行上限 16 MB，任何时候只持有一行。
- 打开一个会话的 Diff < 50 ms（[性能预算](tech-research.md#性能预算)）：本 crate 的份额是 `turns` 命中缓存加解析追加部分 < 10 ms，`load` 一次 < 1 ms。
- 预期耗时（M 系列，单线程，release）：发现 1000 个会话文件 < 500 ms；整文件解析 ≥ 200 MB/s；追加 4 KB 后 `read` < 1 ms；`.zst` 解压后解析 ≥ 100 MB/s。实现时用 bench 实测后校准，写进 bench 的预算表。
- `bench`（[#7](https://github.com/SoloJiang/lectern-app/issues/7)）新增：`agents/discover_1000`（合成 1000 个会话目录）、`agents/parse_throughput`（fixtures 拼成 50 MB）、`agents/append_4k`、`agents/turns_cached_50mb`、`agents/zst_throughput`。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)：

- 没有会话目录或目录为空：列表为空，不报错；Welcome 和项目主页按「还没有会话」显示。
- 目录不可读：记进 `Diagnostics::unreadable_dirs` 和日志，其余目录照常发现。
- 整个文件认不出格式：`Format::Unrecognized`；能读到 cwd 时照常列出、归位，`turns` 返回 `Unrecognized`，界面降级为按文件显示、隐藏轮次和说明，不弹错误。读不到 cwd 的不列出，只计数。
- 个别行不认识、不是合法 JSON、超过 16 MB：跳过并计数，`Format::Partial`；超长行里的改动拿不到，`review` 按 Drifted 规则处理。
- 文件末尾不完整的行（agent 正在写）：不读，等下一次变化。
- 文件被截断或改写：游标的前缀副本对不上，`Batch::reset` 为 true，从头读。
- 文件被删除：从列表移除，`SessionsChanged::removed` 带上它；`read` / `turns` 返回 `NotFound`。

## 测试

全部测试在 Linux 上运行（`just test -p lectern-agents`），不依赖真实的 agent、会话目录、网络和登录状态：

- 解析快照（insta）：每份 fixture 解析出的 `Record` 序列、`TurnIndex`、`SessionSummary` 各一个快照，`claude_*` / `codex_*`。
- 增量等价：每份 fixture 按随机切分（包括行中间）逐段追加到临时文件、每段后 `read` 一次，拼起来的记录与一次读完相同；游标经 `to_bytes` / `from_bytes` 往返后继续读，结果相同（`incremental_*`）。
- 改写与截断：截断、替换为另一个会话、改写游标之前的内容，`reset` 为 true 且记录从头给出（`reset_*`）。
- 压缩：同一份 Codex fixture 的 `.jsonl` 与 `.jsonl.zst` 记录相同；压缩前取的游标在压缩后继续有效（`zst_*`）。
- 继承前缀：Codex 子 agent 与分叉会话、Claude Code 分叉会话的 `inherited` 标记，轮次索引不含继承的轮次（`inherited_*`）。
- 子 agent：父会话、`toolUseId` 关联，`meta.json` 缺失时 `tool_call` 为 None（`subagent_*`）。
- 去重键：Claude Code 同一响应的多行给出相同的 `UsageKey`；`info` 为 null 的 `token_count` 只给出 `RateLimits`（`usage_keys_*`）。
- 工具分类：分类表每一行一个用例，表外工具名归 `Other` 并计数（`tool_kind_*`）。
- 异常：不认识的类型、坏行、超长行、末尾不完整的行、认不出格式的文件（`format_*`）。
- 发现与监听：临时目录加 `fs_watch` 的内存模拟后端，新增、追加、删除、`.jsonl` 与 `.zst` 并存、`sessions/` 与 `archived_sessions/` 并存、`SessionFilter` 隐藏（`discover_*`、`watch_*`）。
- 归位：模拟的 `Projects`，相同 cwd 只 `resolve` 一次，cwd 不存在为 `Missing`（`place_*`）。
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

1. **`services` 的会话接口**（改 `lectern-services`）
   - `SessionSource`、`SessionsChanged` 和「数据结构」里的类型在 `services::session` 里，`cargo check -p lectern-services` 通过。
   - 每个类型的 JSON 往返测试通过（`just test -p lectern-services` 的 `session_*`）。
2. **Claude Code 适配器与增量读取**
   - `just test -p lectern-agents` 里 `claude_*`、`incremental_*`、`reset_*`、`subagent_*`、`usage_keys_*`、`tool_kind_*`、`format_*` 通过，快照已逐个看过。
   - `crates/agents/tests/fixtures/claude-code/` 下每个版本目录都有 `SOURCE.md`。
   - `rg 'serde_json::Value' crates/agents/src` 只出现在测试代码里。
   - `just bench` 的 `agents/parse_throughput`、`agents/append_4k` 达到「性能」里的目标。
3. **Codex 适配器**
   - `codex_*`、`zst_*`、`inherited_*` 通过；`just deny` 通过（`ruzstd`）。
   - `agents/zst_throughput` 达标。
4. **发现、监听、归位与插件**（依赖 `fs_watch` 的目录监听和 `cache` 的应用级分库）
   - `discover_*`、`watch_*`、`place_*`、`index_cache_*`、`plugin_*` 通过。
   - `python3 scripts/check_architecture.py` 通过；`rg gpui crates/agents/Cargo.toml` 没有结果。
   - `agents/discover_1000`、`agents/turns_cached_50mb` 达标；macOS 上冷启动基准（#7）不因本插件变化。
