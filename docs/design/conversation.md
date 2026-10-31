# conversation 设计

状态：实现中。前置条件 G（`conversation.ftl`，「界面」表的 90 个键）已实现。

对应 issue [#23](https://github.com/SoloJiang/lectern-app/issues/23)（Conversation 面板：经 ACP 的结构化对话）、[#24](https://github.com/SoloJiang/lectern-app/issues/24)（权限请求的界面）和 [#12](https://github.com/SoloJiang/lectern-app/issues/12) 的 Terminal 页。`conversation` 是检查器的 Conversation 页（⌘\）：Messages 页显示正在审阅的会话的对话（prompt、agent 说明、工具调用、发回的审阅意见），用户在这里回复 agent、批准或拒绝 agent 的权限请求；Terminal 页在内嵌终端里恢复这个会话（`claude --resume`、`codex resume`），显示原始输出（[交互规范「三层结构」](interaction.md#三层结构)、[「状态与异常」](interaction.md#状态与异常)的「Agent 请求权限」、[ADR 0012](../adr/0012.md)、[技术调研「Agent 原生面板」](tech-research.md#4-agent-原生面板第二阶段)）。ACP agent 的启动方式和信任边界见 [ADR 0050](../adr/0050.md)。

## 范围

- 做：
  - 检查器页 `InspectorTab("conversation")`：页内的分段控件切换 Messages 与 Terminal；页跟随当前审阅屏幕的会话（`Target::Review(ReviewTarget::Session(key))`）。
  - Messages 页：状态行（agent、连接状态、上下文窗口占用）、对话列表（prompt、agent 说明、连续的工具调用合成一行、发回的审阅意见、异常结束的说明）、更早的轮次按需加载、agent 的计划、权限请求卡片、回复框（⌘↩ 发送，进行中可以停止）。
  - 权限请求：按选项的种类给出按钮，执行命令的请求显示命令文本，改文件的请求显示改动预览；回答经 `Conversations` 发出；当前审阅的会话有新请求而检查器没有显示 Conversation 时，切到这一页（「未决问题」3 的决定）。
  - Terminal 页：恢复按钮、拒绝原因、「可能还在别处运行」的提示；恢复后嵌入 `terminal_view` 的终端视图；结束终端；还有运行中的终端时，关闭项目或退出之前确认（离开守卫）。
  - 一个会话只由 Lectern 里的一处驱动（[ADR 0050](../adr/0050.md)）：Terminal 页在恢复这个会话时不经 ACP 发送，有 ACP 连接时不在 Terminal 页恢复。
  - 全部界面字符串的 Fluent 键、UI 探针的语义节点、具名 Action 与快捷键、向 `workspace` 登记的贡献。
  - 本文定义 `services::conversation` 模块（「接口」），由 `services` 采纳、`agents` 实现（「对其他 crate 的要求」B、C）。
- 不做：
  - ACP 客户端、agent 进程的启动与空闲退出、历史窗口的读取、实时更新与历史的合并、改动预览的计算：`agents`（「对其他 crate 的要求」C）。本 crate 不启动进程、不读会话日志、不做 IO。
  - PTY、终端状态机、恢复命令的映射与检查、登录 shell 的环境：`terminal`（[terminal 设计](https://github.com/SoloJiang/lectern-app/pull/265)）与 `platform`；终端网格的绘制、选区和键盘输入：`terminal_view`（「对其他 crate 的要求」D、E）。
  - 检查器容器和它的分段控件（Files / Conversation）、导航：`workspace`。
  - 项目侧栏里会话行的等待标记：`project_home`（「对其他 crate 的要求」H）。
  - 审阅意见的编写、攒批和 ⌘↩ 发回：[#22](https://github.com/SoloJiang/lectern-app/issues/22)（`review_view` 与 `review`）。本文只规定发回的路线（「对其他 crate 的要求」I：本 crate 实现 `review` 的发送途径 `NoteRoute`）和发回后在对话里怎样显示。
  - 颜色、字体、动效常量、控件：`ui`。
- 本 issue 之外：
  - agent 说明按 Markdown 渲染：与 [review_view 设计「未决问题」](review_view.md#未决问题)4 相同，第一版按纯文本显示、保留换行。
  - 思考内容（ACP 的 `agent_thought_chunk`）、模式切换（`session/set_mode`）、斜杠命令（`available_commands_update`）、prompt 里的图片和附件：不显示、不提供。
  - 新建会话（项目主页的 New session）：另开 issue；本文只继续已有的会话。
  - 权限请求的系统通知：见「未决问题」3。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui` | 视图、`list` 与 `ListState`（变高的虚拟列表）、Action、`cx.spawn` 的前台任务 | 已锁定（[ADR 0016](../adr/0016.md)） |
| `lectern-ui` | 颜色、`TextRole`、`motion`、`Transition`、控件（`Segmented`、`Button`、`AgentDot`、`Expander`、`Skeleton`、`loading_phase`，以及新增的 `TextArea`）、`Text`、`semantic`、`contrib` 的载荷类型 | 视图层的公共库（[ADR 0015](../adr/0015.md)） |
| `lectern-i18n` | `keys::conversation::*`、`Args`、`relative_time` | [ADR 0013](../adr/0013.md) |
| `lectern-kernel`、`lectern-services` | `Plugin`、`PluginHandle`、`ServiceRef`；使用 `dyn Contributions`、`dyn SessionSource`、`dyn Conversations`、`dyn Terminals` 和它们的数据类型 | 架构规则 7（[ADR 0025](../adr/0025.md)）：插件之间只经 `services` |
| `lectern-terminal-view` | Terminal 页里的终端视图 `TerminalPane`；从 `ui` 的颜色推出终端的 `Theme`、按区域大小算 `GridSize` 的两个函数 | 非插件的视图库（「对其他 crate 的要求」E），插件 crate 可以依赖 |
| `lectern-logging` | 日志宏；测试用 `test-support` 的 `testing::capture()` | [ADR 0046](../adr/0046.md) |
| `futures`（`channel` feature） | 事件到主线程的 `mpsc::unbounded` | 已在工作区依赖（`review_view` 同样用它） |
| `lectern-kernel`（`test-support`，dev）、`insta`（dev） | `TestKernel`、语义树快照 | 工作区统一依赖 |

本 crate 是插件 crate（`[package.metadata.lectern] plugin = true`，实现的第 1 步加上），含三个插件（见「接口」）。它不依赖 `lectern-agents`、`lectern-terminal`、`lectern-workspace`、`lectern-review-view`、`lectern-project-home`（`agents`、`workspace` 等是插件；`terminal` 的服务经 `services` 使用），测试用本 crate 自己的假实现（「测试」）。不新增 Cargo.lock 里没有的外部 crate。所在层与依赖方向见 [crates/conversation/AGENTS.md](../../crates/conversation/AGENTS.md)。

## 接口

### 插件

kernel 没有可选依赖：`requires` 里的服务都要有活着的提供者，插件才能激活（[kernel 设计「激活时的上下文」](kernel.md#激活时的上下文)）。检查器页不该因为 ACP 客户端或终端失败而消失，所以按 [review_view 设计「插件」](review_view.md#插件)的做法分成三个插件：

| 插件 ID | provides | requires | 激活 | 内容 |
|---|---|---|---|---|
| `conversation` | — | `dyn Contributions`、`dyn SessionSource` | `AfterFirstFrame` | 检查器页与快捷键的登记；`WorkspaceEvent` 的监听（当前会话）；页本身 |
| `conversation-live` | — | `dyn Conversations` | `AfterFirstFrame` | 把 `dyn Conversations` 放进共享状态；`ConversationEvent` 的监听；不在时 Messages 页显示不可用，Terminal 页照常 |
| `conversation-terminal` | — | `dyn Terminals`、`dyn Contributions` | `AfterFirstFrame` | 把 `dyn Terminals` 和这次激活的 `PluginHandle` 放进共享状态；终端快捷键（`terminal_view::key_bindings()`）与离开守卫的登记；`TerminalEvent` 的监听（`Listed`、`Frame`）；不在时 Terminal 页显示不可用，Messages 页照常 |

```rust
/// `app` 装配时调用一次，三个插件都放进 `PluginSet`（ID 见上表）。不做 IO。
pub fn plugins() -> (ConversationPlugin, LivePlugin, TerminalPagePlugin);

pub struct ConversationPlugin { /* Arc<Shared> */ }
pub struct LivePlugin { /* Arc<Shared> */ }
pub struct TerminalPagePlugin { /* Arc<Shared> */ }
```

- `ConversationPlugin::activate`（kernel 线程）：取 `ctx.service::<dyn Contributions>()`、`service_set::<dyn SessionSource>()` 存进 `Shared`（会话列表用于 agent 圆点和「可能还在别处运行」的判断，按 `SessionKey::source` 找提供者，与 review_view 相同）；`Contributions::register(…, None)` 两项贡献（「界面」的「向 `workspace` 登记的贡献」），返回的 `Registration` 经 `ctx.own_staged` 交出；`ctx.on::<WorkspaceEvent>`（`ScreenChanged`、`WindowClosed`）订阅。不提交任务。`started`：保存 `PluginHandle`；调 `Contributions::current_route(WindowId(0))`（「对其他 crate 的要求」A）补上激活之前的屏幕，因为 `Live` 之前发出的 `ScreenChanged` 收不到（kernel 设计「激活时的上下文」）。停用：`Active` drop 时清空 `Shared` 里的引用。
- `LivePlugin::activate`：取 `ServiceRef<dyn Conversations>`，订阅 `ConversationEvent`；`started` 时放进 `Shared::conversations`，并读一次 `waiting()` 记进 `Shared::waiting`；`Active` drop 时取出。
- `TerminalPagePlugin::activate`：取 `ServiceRef<dyn Terminals>` 和 `ctx.service::<dyn Contributions>()`，订阅 `TerminalEvent`；`Contributions::register(…, None)` 一项 `KeyBindings`（`id` 是 `conversation-terminal`，内容是 `terminal_view::key_bindings()`）和 `register_leave_guard(…, None)` 一个离开守卫（「数据结构」的「离开守卫」），返回的 `Registration` 经 `ctx.own_staged` 交出。`started` 时把服务引用和这次激活的 `PluginHandle` 放进 `Shared::terminals`、`Shared::terminal_handle`；`Active` drop 时取出。这两项和终端一起出现、一起消失：没有 `dyn Terminals` 时不绑定终端的键，也不询问。
- 三个插件共用一个 `Arc<Shared>`（`plugins()` 建）。页每次用到服务时从 `Shared` 取一份（取锁复制），`ServiceRef::get()` 为 None（提供者已停用）时按没有处理。`conversation-live` 或 `conversation-terminal` 启动或停用时向活着的页投递 `Job::Services`，页重画。

```rust
pub(crate) struct Shared {
    contributions: Mutex<Option<ServiceRef<dyn Contributions>>>,
    sessions: Mutex<Option<ServiceSet<dyn SessionSource>>>,
    conversations: Mutex<Option<ServiceRef<dyn Conversations>>>,
    terminals: Mutex<Option<ServiceRef<dyn Terminals>>>,
    /// `conversation` 这次激活的句柄。
    handle: Mutex<Option<PluginHandle>>,
    /// `conversation-terminal` 这次激活的句柄：`TerminalPane::new` 要它的 `token()`（复制时交给 `Terminals::copy_selection`）。
    terminal_handle: Mutex<Option<PluginHandle>>,
    /// 当前屏幕上的审阅对象（「当前会话」）。
    current: Mutex<Current>,
    /// `waiting()` 上一次的集合，用来找出新进入等待的会话（「权限请求」）。
    waiting: Mutex<Arc<[SessionKey]>>,
    /// 每个会话的回复草稿与上一次所在的页，只在内存里（「草稿」）。
    memory: Mutex<Memory>,
    /// 活着的页的主线程通道（「数据流与线程」）。检查器只有一个，通常至多一个元素。
    tabs: Mutex<Vec<futures::channel::mpsc::UnboundedSender<Job>>>,
}

pub(crate) enum Current {
    /// 没有审阅屏幕（项目主页、Welcome、阅读视图）。
    None,
    /// 审阅对象不是会话（`ReviewTarget::Outside`、`Range`）。
    NotSession,
    Session(SessionKey),
}
```

### 视图

视图代码在 `src/view/`（`tab.rs`、`messages.rs`、`terminal.rs`），不读服务的纯函数在 `src/items.rs`、`src/drive.rs`、`src/memory.rs`。

```rust
/// 检查器的 Conversation 页。`InspectorTab("conversation")` 的工厂建，检查器隐藏或换页时 drop。主线程。
pub struct ConversationTab {
    shared: Arc<Shared>,
    current: Current,
    page: Page,
    page_slider: Transition,               // 页内分段控件的滑块（`SEGMENT`）
    messages: Option<Entity<MessagesModel>>, // `current` 是会话时建，换会话时换掉
    terminal: Option<Entity<TerminalPage>>,  // 第一次切到 Terminal 页时建
    focus: FocusHandle,
    // 事件通道的接收端由构造时 `cx.spawn` 的前台任务持有。
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page { Messages, Terminal }

/// 一个会话的 Messages 页状态。只在主线程上读写；没有 IO。
pub struct MessagesModel {
    session: SessionKey,
    agent: Agent,
    /// `Conversations::watch` 的守卫：持有期间提供者维护这个会话的快照。没有 `dyn Conversations` 时为 None。
    interest: Option<ConversationInterest>,
    opened_at: Instant,                       // 加载态按它算
    snapshot: Option<Arc<ConversationSnapshot>>,
    items: Vec<Item>,                         // 「数据结构」的「列表的项」
    index: HashMap<EntryKey, u32>,            // 项的稳定键 → 下标，每次应用快照后重建
    list: ListState,
    follow: bool,                             // 列表停在底部：新项到达时跟到底部
    arriving: HashMap<EntryKey, Transition>,  // 新项的 `ARRIVE`
    composer: Entity<ui::TextAreaState>,      // 回复框（「对其他 crate 的要求」F）
    plan_open: Transition,                    // 计划的展开（`Expander`）
    more_open: Transition,                    // 「还有 n 个请求」的展开
}

/// Terminal 页。主线程；没有 IO。
pub struct TerminalPage {
    session: SessionKey,
    terminal: Option<TerminalId>,
    /// `Terminals::info(terminal)` 上一次的 `state`；`TerminalEvent::Listed` 之后重读。
    state: Option<RunState>,
    pane: Option<Entity<lectern_terminal_view::TerminalPane>>,
    /// `Terminals::watch(terminal)` 的关注：只在 Terminal 页正在显示时持有（「数据流与线程」）。
    frames: Option<FrameInterest>,
    /// 页上终端区域上一次绘制的大小（`canvas` 的 prepaint 记下），恢复时换算 `GridSize`。
    area: Option<Size<Pixels>>,
    refused: Option<LaunchError>,             // 上一次 `open` 被拒绝的原因
}

/// 执行器线程、kernel 线程、主线程上产生，排到主线程的工作。
pub(crate) enum Job {
    Route(Current),
    Conversation { session: SessionKey, generation: u64 },
    Waiting,
    Terminal(TerminalEvent),
    /// `conversation-live` 或 `conversation-terminal` 启动或停用。
    Services,
}
```

### 服务接口（`lectern-services` 的 `conversation` 模块）

`services` 在 kernel 层。`Agent`、`SessionKey`、`Timestamp` 取自 `services::session`。类型都是不含行为的数据，与 `services::session` 一样派生 `Clone`、`PartialEq`、`Eq`、`Debug`、`Serialize`、`Deserialize`（外部标记的枚举）；trait 和包着回调的 `ConversationInterest` 除外。`ConversationEvent` 实现 kernel 的 `Event`。

```rust
/// 单实现服务，`agents` 的 "conversations" 插件提供（「对其他 crate 的要求」C）。
/// impl Service for dyn Conversations { const ID = "conversations"; const MULTIPLE = false; }
pub trait Conversations: Send + Sync {
    /// 关注一个会话的对话：返回值持有期间提供者为它维护快照（在 executor 上读历史窗口），drop 后释放。
    /// 立即返回，任何线程，不阻塞。不启动 agent 进程。同一个会话可以有多个关注，共用一份快照。
    fn watch(&self, session: &SessionKey) -> ConversationInterest;

    /// 当前快照。取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程可调用。
    /// 没有关注、也没有连接时为 None；有连接时即使没有关注也保留（权限请求在检查器关着时仍要等待回答）。
    fn snapshot(&self, session: &SessionKey) -> Option<Arc<ConversationSnapshot>>;

    /// 发命令。立即返回，任何线程。`Prompt`、`Cancel`、`Answer`、`Disconnect` 在返回之前已在锁内换出新快照
    /// （`link`、`permissions` 已是新值），调用方随后 `snapshot()` 读到的就是新状态；进程的启动和消息的收发在后台进行，
    /// 结果随 `ConversationEvent::Changed` 送达。当前状态下不允许的命令（`can_prompt` 为 `Err` 时的 `Prompt`、
    /// 已不在等待的请求的 `Answer`）忽略，提供者记 debug。
    fn command(&self, session: &SessionKey, command: ConversationCommand);

    /// 有待确认的权限请求的会话，按最早的请求排序。取锁复制一个 `Arc`，不阻塞，UI 线程可调用。
    fn waiting(&self) -> Arc<[SessionKey]>;
}

/// 在提供者的线程（executor、读 agent 输出的专用线程，或调用 `command` 的线程）上发出，快照替换之后。监听只转发，不做 IO。
pub enum ConversationEvent {
    Changed { session: SessionKey, generation: u64 },
    /// `waiting()` 的集合变了。
    WaitingChanged,
}

/// `watch` 的关注，drop 时注销。不序列化。
pub struct ConversationInterest { /* 实现方传入的注销回调 */ }
impl ConversationInterest { pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self; }

pub enum ConversationCommand {
    /// 发一条消息。没有连接时这是启动 agent 进程的唯一入口（ADR 0050）。`text` 不超过 256 KiB，超过时忽略。
    Prompt { text: Arc<str>, origin: PromptOrigin },
    /// `session/cancel`；待确认的请求按协议回答 `cancelled`。`link` 先变为 `Stopping`，这一轮结束后为 `Idle`。
    Cancel,
    /// 回答一个权限请求。`option` 必须是这个请求的 `options` 之一。
    Answer { request: PermissionId, option: OptionId },
    /// 历史窗口向前多读 20 轮（最多 200 轮）。
    LoadEarlier,
    /// 关闭连接（Terminal 页恢复之前）：`Idle` 或连接仍在的 `Failed` 时关闭，`link` 变为 `None`；没有连接时只把 `Failed` 清为 `None`。
    /// `Connecting`、`Working`、`Stopping` 时忽略。
    Disconnect,
    /// 清除 `Link::Failed`：连接仍在时回到 `Idle`，否则回到 `None`。
    Dismiss,
}

pub enum PromptOrigin {
    /// 用户在回复框里写的，或历史里的 prompt。
    User,
    /// #22 发回的审阅意见：对话里显示为一行「n 条审阅意见 · 位置」，不显示拼好的 prompt 全文。
    ReviewNotes { count: u32, locations: Arc<[NoteLocation]> },
}
/// 至多 8 个；`path` 是绝对路径，界面按会话的 cwd 显示相对路径。
pub struct NoteLocation { pub path: Arc<Path>, pub line: u32 }

pub struct ConversationSnapshot {
    pub session: SessionKey,
    /// 每次替换加一，与 `ConversationEvent::Changed::generation` 一致。
    pub generation: u64,
    pub agent: Agent,
    pub load: Load,
    pub link: Link,
    /// 回复框能不能发送；`Err` 时界面显示原因（「状态与异常」）。不含「终端在驱动这个会话」，那一条由本 crate 判断（「一处驱动」）。
    pub can_prompt: Result<(), Unavailable>,
    /// 上下文窗口的占用：只取自 ACP 的 `usage_update`（`used`、`size`），没有连接或 agent 不发时为 None。不估算。
    pub context: Option<ContextUsage>,
    /// 历史窗口之前还有多少轮。
    pub earlier: u32,
    /// 按对话顺序。见下面的不变量。
    pub entries: Arc<[Entry]>,
    /// agent 最近一次给出的计划（ACP `plan`，整份替换）；只在有连接时有。
    pub plan: Option<Arc<[PlanStep]>>,
    /// 待确认的权限请求，最早的在前。
    pub permissions: Arc<[PermissionRequest]>,
}

pub enum Load { Loading, Ready, Failed(HistoryError) }
pub enum HistoryError {
    /// 会话文件已删除。
    NotFound,
    /// 会话格式无法识别（`SessionError::Unrecognized`）：没有轮次和说明。
    Unrecognized,
    /// 读文件失败。下一次 `SessionsChanged` 后重试。
    Io,
}

pub enum Link {
    /// 没有连接：只有历史。
    None,
    /// 进程已启动，正在 `initialize` 和恢复会话。
    Connecting,
    /// 已连接，没有进行中的 prompt。空闲 10 分钟后关闭，回到 `None`（ADR 0050）。
    Idle,
    /// 一轮正在进行（`session/prompt` 还没有返回）。
    Working,
    /// 已发出 `session/cancel`，等这一轮结束。
    Stopping,
    /// 上一次连接或上一轮异常结束。下一次 `Prompt` 或 `Dismiss` 清除。`AuthRequired`、`PromptFailed` 时连接可能仍在，
    /// 按 `Idle` 计空闲时间，下一次 `Prompt` 直接用它。
    Failed(LinkError),
}

pub enum LinkError {
    /// 程序启动失败：执行失败，或发送时程序的查找还没完成、完成后没有找到。
    Spawn,
    /// 协商的协议版本不是 1，或 agent 不支持恢复这个会话。
    Unsupported,
    /// 恢复会话（`session/resume` 或 `session/load`）返回错误。
    LoadFailed,
    /// 收到无法解析的消息、超过 16 MiB 的行，或不合协议的回复。
    Protocol,
    /// 进程在连接期间退出。
    Exited { code: Option<i32> },
    /// agent 要求先登录（ACP 的认证错误）。Lectern 不调用 `authenticate`（ADR 0050）。
    AuthRequired,
    /// `session/prompt` 返回了其他错误。`message` 是 agent 写的，只取第一行、至多 1 KiB；界面原样显示，不进日志。
    PromptFailed { message: Arc<str> },
}

pub enum Unavailable {
    /// 子 agent 会话，或来源不是内置的（`SessionSource::resume` 为 None）。
    NotResumable,
    /// 会话的 `cwd` 不在已打开项目的 worktree 根或文件夹项目根之内。
    OutsideProject,
    /// 登录 shell 环境的 PATH 里找不到这个 agent 的 ACP 程序（「对其他 crate 的要求」C 的查找）。
    NotInstalled(AcpProgram),
    /// 这个 agent 的适配器没有通过「对其他 crate 的要求」C 的核对，或上一次协商的协议版本不是 1。
    Unsupported,
    /// 已有 3 个连接，都不空闲（ADR 0050）。
    TooMany,
    /// 正在连接或这一轮还没有结束（`Connecting`、`Working`、`Stopping`）。
    Busy,
}

/// 固定映射的程序（ADR 0050）。程序名在「对其他 crate 的要求」C 的核对里确定，界面只显示 `name()`。
pub enum AcpProgram { ClaudeCode, Codex }
impl AcpProgram { pub fn name(&self) -> &'static str; }

pub struct ContextUsage { pub used: u64, pub size: u64 }

pub struct Entry { pub key: EntryKey, pub body: EntryBody }

/// 项的稳定键。不透明，只由序号组成、不含内容：`[a-z0-9:_-]`，至多 64 字节，可以直接作探针节点的业务键。
pub struct EntryKey(pub Arc<str>);

pub enum EntryBody {
    Prompt { text: Passage, origin: PromptOrigin },
    /// `streaming` 为 true：这一段还在接收（这一轮的最后一段，`link` 为 `Working`）。
    AgentText { text: Passage, streaming: bool },
    /// 不是人写的输入（后台任务通知、hook 注入的内容），对应 `TurnItem::Injected`。
    Injected { text: Passage },
    Tool(ToolEntry),
    /// 一轮没有正常结束。
    Ended(EndReason),
}

/// 一段正文。超过 64 KiB 的截断到 64 KiB 以内最后一个完整的字符，`truncated` 为 true。
pub struct Passage { pub text: Arc<str>, pub truncated: bool }

pub struct ToolEntry {
    pub kind: CallKind,
    pub state: CallState,
    /// 动词之后的一段（「Edit unit.test.ts」里的文件名）：`Edit`、`Read`、`Delete`、`Move` 是文件名（不含目录，至多 3 个，用「, 」连接），
    /// `Execute` 是命令的第一行，`Search` 是 agent 写的标题，`Other` 是 agent 写的标题或工具名；都至多 160 个字符。
    pub summary: Arc<str>,
}

pub enum CallKind { Read, Edit, Delete, Move, Search, Execute, Fetch, Think, SpawnAgent, Other }
pub enum CallState { Pending, Running, Completed, Failed, Denied, Cancelled }
pub enum EndReason { MaxTokens, MaxTurnRequests, Refusal, Cancelled }

pub struct PlanStep { pub text: Arc<str>, pub state: StepState }
pub enum StepState { Pending, InProgress, Completed }

pub struct PermissionRequest {
    /// 提供者分配，本次运行内唯一。
    pub id: PermissionId,
    pub kind: CallKind,
    /// agent 写的工具调用标题，作为 agent 的内容原样显示。
    pub title: Arc<str>,
    pub detail: PermissionDetail,
    /// 每种至多一个，按 `OptionKind` 的顺序；agent 给了同一种的多个选项时只留第一个。
    pub options: Arc<[PermissionOption]>,
    pub asked_at: Timestamp,
}
pub struct PermissionId(pub u64);

pub enum PermissionDetail {
    /// 执行命令：agent 原始输入里的命令文本（ADR 0050），至多 16 KiB；`workdir` 是输入里的工作目录。
    Command { text: Arc<str>, workdir: Option<Arc<Path>> },
    /// 改文件：工具调用内容里的 diff，至多 8 个文件。
    Edits(Arc<[EditPreview]>),
    /// agent 没有给出可识别的内容，只有标题。
    None,
}

pub struct EditPreview {
    pub path: Arc<Path>,
    pub new_file: bool,
    pub added: u32,
    pub removed: u32,
    /// 至多 120 行（上下文 3 行）；其余计入 `omitted`。
    pub rows: Arc<[PreviewRow]>,
    pub omitted: u32,
}
pub struct PreviewRow { pub kind: RowKind, pub text: Arc<str> }
pub enum RowKind { Context, Added, Removed }

pub struct PermissionOption { pub id: OptionId, pub kind: OptionKind }
/// agent 给的选项 ID，原样交回。不显示，不进日志。
pub struct OptionId(pub Arc<str>);
/// 顺序即按钮的顺序。
pub enum OptionKind { AllowOnce, AllowAlways, RejectOnce, RejectAlways }
```

快照的不变量（提供者保证，本 crate 的测试用假实现构造）：

- 同一轮不出现两次。`entries` 先是历史窗口（这个 `SessionKey` 的轮次索引，每轮依次是 prompt、agent 说明、注入内容、工具调用），再是本次运行里经 ACP 发出的、还没有出现在轮次索引里的 prompt 和它的实时更新。轮次索引里出现了这一轮、这一轮的 `session/prompt` 也已返回之后，实时的项在同一代里换成历史的项。
- 历史的 `EntryKey` 是 `h:<轮次序号>:<项序号>`，实时的是 `l:<发出的序号>:<项序号>`；同一项在各代之间键不变，只有实时换成历史时键变化一次。
- 正文合计不超过 1 MiB：超过时从窗口最早的轮次去掉，`earlier` 随之增加。
- 只有 `link` 为 `Working` 时，最后一项可以是 `streaming` 的 `AgentText`。
- 有待确认的请求时 `link` 是 `Working` 或 `Stopping`。

调用方：

- `conversation`：页建好时 `watch`；收到 `Changed` 后 `snapshot`；回复、停止、回答、加载更早的轮次、断开发对应命令；`waiting()` 用来在当前会话新进入等待时切到这一页。
- `project_home`：渲染会话行时读 `waiting()`，收到 `WaitingChanged` 后重画（「对其他 crate 的要求」H）。
- #22：本 crate 的会话途径（「对其他 crate 的要求」I）在没有终端驱动这个会话时发 `Prompt { origin: ReviewNotes }`。

## 数据结构

### 当前会话

`Shared::current` 由 `ScreenChanged { to, .. }` 和 `started` 时的 `current_route` 维护：`to.screen` 是 `screens::REVIEW` 且 `to.target` 是 `Target::Review(ReviewTarget::Session(key))` 时为 `Session(key)`，是审阅但不是会话时为 `NotSession`，其余为 `None`；`WindowClosed` 时为 `None`。值变化时向活着的页投递 `Job::Route`。页按它换掉 `MessagesModel`（旧的 drop，关注随之释放）和 `TerminalPage`。检查器只出现在审阅屏幕上（[workspace 设计「接口」](workspace.md#接口)的 `ScreenLayout::inspector`），所以 `None` 时通常没有活着的页。

### 列表的项

`MessagesModel::items` 由快照算出，纯函数（`items::build(&ConversationSnapshot) -> Vec<Item>`），不读服务：

```rust
pub(crate) enum Item {
    /// 「显示之前的 n 轮」（`earlier > 0` 时在最前）。
    Earlier(u32),
    Prompt { key: EntryKey, text: Passage },
    /// `PromptOrigin::ReviewNotes` 的 prompt：一行「n 条审阅意见 · 位置」。
    Notes { key: EntryKey, count: u32, locations: Arc<[NoteLocation]> },
    Agent { key: EntryKey, text: Passage, streaming: bool },
    Injected { key: EntryKey, text: Passage },
    /// 连续的工具调用合成一行（「Edit unit.test.ts · Run yarn test unit」）。键取第一项的；至多显示 4 项，其余是「另有 n 项」。
    Tools { key: EntryKey, calls: Vec<ToolEntry> },
    Ended { key: EntryKey, reason: EndReason },
}
```

- 应用新快照时按 `EntryKey` 比较新旧项：只有变化的项 `ListState::splice`，没有变化的项不重新排版。流式更新只改最后一项，所以每次只重新排版这一项。
- 新出现的键（不是首次应用）带一个 `ARRIVE` 的 `Transition`；首次应用和 `Earlier` 加载的项没有动画。
- 跟随底部：`follow` 在列表滚到最后一项可见时为 true；为 true 时应用快照后滚到底部，否则按 `logical_scroll_top` 的键找回原来的位置（与 [review_view 设计「滚动位置与选中项」](review_view.md#滚动位置与选中项)相同的做法）。实时的项换成历史的项时键变化，找不到原键就停在底部。

### 权限请求

- 卡片固定在列表和回复框之间，不随列表滚动。只完整显示最早的一个请求；其余折叠成一行「还有 n 个请求」，点开（`Expander`）后逐个显示。
- 按钮由 `options` 生成，顺序与文字按 `OptionKind`（「界面」的 Fluent 键），不用 agent 给选项写的名字（ADR 0050）。没有给出的种类不画按钮。`AllowOnce` 是主按钮（`ButtonKind::Primary`），其余是次按钮；没有默认按钮，↵ 不触发任何选项，焦点不自动移到卡片上。
- 内容：`Command` 用等宽字体显示命令文本（至多 12 行，超出的可以在卡片里滚动），`workdir` 与会话 cwd 不同时另起一行显示；`Edits` 每个文件一个标题（相对路径、`+added −removed`）和 `rows`，增删行按 diff 的配色与 `+` / `−` 符号；`None` 只显示 `title` 和 `conversation-permission-no-detail`。
- 回答：点按钮发 `Answer { request, option }`。卡片立即从快照里消失（`command` 在返回前换出快照）。
- 切到这一页：`conversation-live` 的 `WaitingChanged` 监听比较新旧集合；新进入等待的会话等于 `Shared::current` 的会话时，`Contributions::dispatch(SelectInspectorTab("conversation"))`（检查器隐藏时这条命令同时显示它）。不移动焦点。这是「未决问题」3 的决定。

### 一处驱动

「终端在驱动这个会话」指 `Terminals::for_session` 有值，并且 `info(id)` 的 `state` 是 `RunState::Starting` 或 `Running { .. }`；`Exited`、`Failed`、页上还留着最后画面的终端不算（没有 `dyn Terminals` 时视为不在驱动）。本 crate 在主线程上检查，两个服务的状态都在调用返回前更新（`Conversations::command` 换出快照；`Terminals::open` 在返回前登记，状态 `Starting`），所以同一帧里的两次检查看到一致的状态：

| 动作 | 允许的条件 | 不允许时 |
|---|---|---|
| 回复框发送 | `can_prompt` 为 `Ok`，并且没有终端在驱动这个会话 | 回复框换成 `conversation-in-terminal` 和「显示终端」按钮；`can_prompt` 的原因见「状态与异常」 |
| Terminal 页恢复 | `link` 是 `None`（没有 `dyn Conversations` 时视为 `None`） | `Idle`、`Failed`（`Failed` 时连接可能仍在）：按钮改为「断开并恢复」，先 `Disconnect` 再恢复；`Connecting`、`Working`、`Stopping`：按钮不可用，显示 `conversation-terminal-linked` |

「可能还在别处运行」只是提示，不阻止：会话的 `SessionSummary::open_turn` 为 true、`last_activity` 在 2 分钟之内、`link` 为 `None`、也没有终端在驱动它时，回复框上方和恢复按钮上方各显示一行 `conversation-elsewhere`（「未决问题」4）。

### 离开守卫

terminal 设计「未决问题」1 的决定 (a)：还有运行中的终端时，关闭项目或退出 Lectern 之前先确认。守卫用 `workspace` 已有的接口（[workspace 设计「服务接口」](workspace.md#服务接口lectern-services-的-workspace-模块)的 `register_leave_guard`、`LeaveGuard::ask`）：

- `conversation-terminal` 插件在 `activate` 里登记（「插件」）。`ask(scope, done)` 在主线程上调用：用本插件的 `ServiceRef<dyn Terminals>` 取 `list()`，数 `state` 是 `Starting` 或 `Running` 的终端。`Project(key)` 只数 `project == key` 的；`App` 数全部；`View`、`Window`、`Worktree` 立即 `done(true)`，因为终端不随离开视图、关闭窗口、切换 worktree 结束（terminal 设计「数据流与线程」只在项目关闭和插件停用时关闭终端）。`get()` 为 None（提供者已停用）或数目为 0 时立即 `done(true)`。
- 数目大于 0 时弹出系统的确认（`conversation-leave-*`）：「结束终端」→ `done(true)`，终端由 `terminal` 在项目关闭或停用时结束；「取消」→ `done(false)`。
- 只管终端。ACP 连接在项目关闭和退出时按 ADR 0050 关闭，不询问。

### 草稿

- 回复框的文字在 `MessagesModel` drop 时（换会话、检查器隐藏、换页）存进 `Shared::memory`，下一次为同一个会话建页时放回。上一次所在的页（Messages / Terminal）同样按会话记下。
- 只在内存里，不写盘（prompt 不落盘）。至多 16 个会话，按最近使用淘汰；单个草稿超过 256 KiB 的不保存。
- 发送：文字去掉首尾空白后为空时不可发送；`command(Prompt)` 之后快照的 `link` 变为 `Connecting` 或 `Working` 才清空回复框，否则保留文字（提供者拒绝了这次发送）。

## 数据流与线程

```
workspace（主线程）─▶ WorkspaceEvent::ScreenChanged ─▶ [监听] Shared::current ─▶ Job::Route ─▶ [主线程前台任务] 换 MessagesModel / TerminalPage
conversation 的 started ─▶ Contributions::current_route(WindowId(0)) ─▶ Shared::current
页建好 ─▶ Conversations::watch(session) ─▶ agents（executor）读历史窗口 ─▶ ConversationEvent::Changed ─▶ [监听] 按会话投递 Job ─▶ [主线程] snapshot() ─▶ items::build ─▶ splice ─▶ 重画
⌘↩ ─▶ Conversations::command(Prompt)（返回前换出 Connecting 的快照）─▶ agents 启动 ACP agent，专用线程读它的输出 ─▶ session/update 合并（≤ 20 次/秒）─▶ Changed ─▶ …
session/request_permission ─▶ 快照的 permissions、WaitingChanged ─▶ [监听] 当前会话新进入等待时 dispatch(SelectInspectorTab) ；project_home 重画等待标记
Allow ─▶ command(Answer)（返回前去掉这个请求）─▶ agents 回复 JSON-RPC ─▶ Changed
恢复 ─▶ Terminals::open(OpenSpec { launch: Launch::Resume(key), size, theme })（返回前已登记，Starting）─▶ watch(id) + TerminalPane::new(terminals, token, id) ─▶ TerminalEvent::Frame / Listed ─▶ [监听] Job::Terminal ─▶ [主线程] TerminalPane::refresh、重读 info(id)
```

- 恢复的参数：`size` 是 `area` 经 E 的 `grid_size(area, window, cx)` 换算的网格（还没有记下 `area` 时为 None，`terminal` 用 80×24，`TerminalPane` 第一次布局后发 `Input::Resize`）；`theme` 取 E 的 `theme(window, cx)`（外观按窗口取）。`TerminalPane::new` 的 `token` 是 `Shared::terminal_handle` 的 `token()`；没有这个句柄时（`conversation-terminal` 不是 `Live`）不恢复，按「没有 `dyn Terminals`」处理。建好之后把焦点交给 `focus_handle()`。
- `FrameInterest` 只在 Terminal 页正在显示时持有：切到 Terminal 页、页上有终端时 `watch(id)`；切到 Messages 页、换会话、检查器隐藏（`ConversationTab` drop）时 drop。没有关注时 `terminal` 只解析输出、不生成帧（terminal 设计「接口」的 `watch`），所以隐藏的 Terminal 页不占主线程。新关注出现时 `terminal` 立即生成一帧完整的，页再次显示时画面是最新的。

- 主线程上只有：读快照（复制 `Arc`）、`items::build`、键的比较与锚点、发命令、渲染。没有 IO、解析和进程操作（架构规则 4）。
- 本 crate 不提交 executor 任务。读历史、ACP 的收发、终端的读写都在 `agents`、`terminal` 里。
- 事件的监听在发出事件的线程上运行，只做转发：取 `Shared::tabs` 的锁、`unbounded_send`。`WaitingChanged` 的监听另外比较集合、按上一节发一条 `dispatch`，不做别的。
- 合并：前台任务每次醒来把通道里的项全部取出：同一个会话的 `Conversation` 只应用最高的 `generation`；页上那个终端的 `TerminalEvent::Frame` 和任何 `Listed` 合成一次 `TerminalPane::refresh`（别的终端的 `Frame` 丢弃）；有 `Listed` 时另外重读 `info(id)`，更新页的 `state`；`Bell` 不处理；应用之后 `cx.notify()` 一次。
- 派生数据：没有需要缓存的派生数据。改动预览的行由 `agents` 计算，按两侧内容的 git blob OID 缓存（架构规则 5，「对其他 crate 的要求」C）。
- 首帧之前什么都不做：三个插件 `AfterFirstFrame` 激活，`activate` 只登记（[ADR 0017](../adr/0017.md)）。不打开 Conversation 页时，本 crate 没有通道、没有关注，`Shared` 只有几个空的 `Mutex`。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)、[bench 设计「预算表」](bench.md#预算表)）：

| 预算 | 本 crate 的份额 |
|---|---|
| 冷启动 20 ms（`app/cold_start`） | 首帧之前没有代码运行；`plugin/conversation/activate`、`plugin/conversation-live/activate`、`plugin/conversation-terminal/activate` 各 < 1 ms，只登记 |
| 空闲内存 60 MB（ADR 0017） | 没有打开 Conversation 页时只有 `Shared`（几百字节）。ACP agent 与终端进程空闲时不运行，也不计入（ADR 0050）；草稿至多 16 × 256 KiB，只在用户写过时存在 |
| 帧预算 8.3 ms（120 Hz） | 应用一次流式更新只重新排版最后一项（≤ 64 KiB）：新基准 `conversation/stream_apply` 的 p95 ≤ 2 ms；列表是虚拟的，只排版可见项 |
| 打开页 | 快照已有时下一帧画出；历史窗口还在读时按 `loading_phase`：100 ms 内空白，之后骨架，1 s 后 `conversation-loading` |
| 安装包 80 MB（ADR 0018） | 不新增外部 crate；适配器不随安装包分发（ADR 0050） |

新基准（「对其他 crate 的要求」K）：`conversation/stream_apply`：40 轮、正文合计 1 MiB 的快照，连续应用 200 个快照，每个让最后一段 agent 说明多 200 字节，测每次 `items::build` 加 `splice` 加这一项的排版，p95 ≤ 2 ms。

## 界面

- 位置：审阅屏幕的检查器（[交互规范「窗口结构」](interaction.md#窗口结构)的 Inspector，300pt），按需呼出层（⌘\）；Terminal 页是收起层的「终端原始输出」。
- 页的结构，从上到下：
  1. 页内分段控件：Messages / Terminal（`ui::Segmented`，滑块走 `SEGMENT`）。
  2. Messages 页：
     - 状态行：`AgentDot` + 连接状态（`conversation-status-*`），有 `context` 时接「· context 72%」（`TextRole::Secondary`）。
     - 对话列表（gpui `list`）：prompt 用 `unemphasized_selected_content_background` 的圆角块，正文 `TextRole::Body`；agent 说明是正文，`streaming` 的一段用 `secondary_label`；工具行与审阅意见行用 `TextRole::Caption`；`Ended` 用 `TextRole::Caption` 的一行说明；截断的正文末尾加 `conversation-truncated`。
     - 计划（有 `plan` 时，在列表之下）：一行「计划 2/5」，点开（`Expander`）显示每一步，已完成的一步用 `Icon::Check`。
     - 权限请求卡片（「数据结构」的「权限请求」）：`control_background` 底、圆角 12pt、0.5pt `separator` 边，与分组列表同一外观。
     - 回复框：`ui::TextArea`（2 到 6 行，随内容增高），占位文字是 `conversation-reply-placeholder`（如「Reply to Claude Code」）；右下角是 Send（`Working` 时换成 Stop）。不能发送时回复框不可用，下面一行写原因，有对应操作时带一个按钮（「显示终端」「关闭」）。
  3. Terminal 页：
     - 没有终端时：一行说明（`conversation-terminal-explain`，命令文本 `claude --resume` / `codex resume` 不翻译）、恢复按钮、拒绝原因或「可能还在别处运行」的提示。
     - 有终端时：一行状态（启动中、运行中、已退出（退出码，或被信号结束）、启动失败（原因））和「关闭」按钮，下面是 `TerminalPane`，占满页剩下的部分（检查器 300pt 宽时约 40 列，「未决问题」5）。终端退出或启动失败后保留最后的画面，恢复按钮重新出现。
- 用到的 `ui` 控件与动效原语：`Segmented`、`Button`、`AgentDot`（新的请求或新一轮到达时 `blink`）、`Expander`、`Skeleton`、`loading_phase`、`TextArea`（新增）、`Icon::Check`；新项与新卡片用 `ARRIVE`（从下方 8pt 淡入），悬停由控件自带。减弱动态效果由 `ui` 处理。本 crate 不写颜色值、字体名和时长。
- Fluent 键（`crates/i18n/locales/{en,zh-CN}/conversation.ftl`，「对其他 crate 的要求」G）。`$agent` 是「Claude Code」或「Codex」，`$program` 是 `AcpProgram::name()`（Messages 页）或 `AgentCommand::program()`（Terminal 页），`$command` 是恢复命令，三者都不翻译：

  | 键 | en | zh-CN |
  |---|---|---|
  | `conversation-tab` | Conversation | 对话 |
  | `conversation-page-messages` | Messages | 消息 |
  | `conversation-page-terminal` | Terminal | 终端 |
  | `conversation-status-history` | Not connected | 未连接 |
  | `conversation-status-connecting` | Connecting… | 正在连接… |
  | `conversation-status-idle` | Idle | 空闲 |
  | `conversation-status-working` | Working | 运行中 |
  | `conversation-status-stopping` | Stopping… | 正在停止… |
  | `conversation-status-waiting` | Waiting for you | 等待你确认 |
  | `conversation-status-terminal` | Running in Terminal | 在终端中运行 |
  | `conversation-status-failed` | Disconnected | 已断开 |
  | `conversation-context` | context { $percent }% | 上下文 { $percent }% |
  | `conversation-earlier` | Show { $count } earlier turn(s)（复数形式） | 显示之前的 { $count } 轮 |
  | `conversation-notes-sent` | { $count } review note(s)（复数形式） | { $count } 条审阅意见 |
  | `conversation-tool-read` | Read { $target } | 读取 { $target } |
  | `conversation-tool-edit` | Edit { $target } | 修改 { $target } |
  | `conversation-tool-delete` | Delete { $target } | 删除 { $target } |
  | `conversation-tool-move` | Move { $target } | 移动 { $target } |
  | `conversation-tool-search` | Search { $target } | 搜索 { $target } |
  | `conversation-tool-execute` | Run { $target } | 运行 { $target } |
  | `conversation-tool-fetch` | Fetch { $target } | 获取 { $target } |
  | `conversation-tool-agent` | Start a subagent | 派出子 agent |
  | `conversation-tool-more` | { $count } more | 另有 { $count } 项 |
  | `conversation-tool-failed` | failed | 失败 |
  | `conversation-tool-denied` | denied | 已拒绝 |
  | `conversation-tool-cancelled` | cancelled | 已取消 |
  | `conversation-ended-max-tokens` | The reply reached the output limit. | 回复达到了输出上限。 |
  | `conversation-ended-max-turns` | The turn reached the request limit. | 这一轮达到了请求次数上限。 |
  | `conversation-ended-refusal` | The agent declined to continue. | agent 拒绝继续。 |
  | `conversation-ended-cancelled` | Stopped. | 已停止。 |
  | `conversation-truncated` | Only the first 64 KB is shown. | 只显示前 64 KB。 |
  | `conversation-plan` | Plan { $done }/{ $total } | 计划 { $done }/{ $total } |
  | `conversation-permission-execute` | Run a command? | 运行命令？ |
  | `conversation-permission-edit` | Change { $count } file(s)?（复数形式） | 修改 { $count } 个文件？ |
  | `conversation-permission-other` | Allow this tool? | 允许这个工具？ |
  | `conversation-permission-workdir` | in { $path } | 位置：{ $path } |
  | `conversation-permission-no-detail` | The agent didn't send the details. | agent 没有给出详细内容。 |
  | `conversation-permission-omitted` | { $count } more line(s)（复数形式） | 另有 { $count } 行 |
  | `conversation-permission-allow-once` | Allow | 允许 |
  | `conversation-permission-allow-always` | Always Allow | 总是允许 |
  | `conversation-permission-reject-once` | Deny | 拒绝 |
  | `conversation-permission-reject-always` | Always Deny | 总是拒绝 |
  | `conversation-permission-more` | { $count } more request(s)（复数形式） | 还有 { $count } 个请求 |
  | `conversation-reply-label` | Reply | 回复 |
  | `conversation-reply-placeholder` | Reply to { $agent } | 回复 { $agent } |
  | `conversation-send` | Send | 发送 |
  | `conversation-stop` | Stop | 停止 |
  | `conversation-unavailable-not-resumable` | This session can't be continued here. | 这个会话不能在这里继续。 |
  | `conversation-unavailable-outside-project` | The session's folder isn't in an open project. | 会话的文件夹不在已打开的项目里。 |
  | `conversation-unavailable-not-installed` | Install { $program } to continue this session here. | 安装 { $program } 后才能在这里继续这个会话。 |
  | `conversation-unavailable-unsupported` | Lectern doesn't support this { $agent } adapter. | Lectern 不支持这个 { $agent } 适配器。 |
  | `conversation-unavailable-too-many` | Three conversations are running. Stop one, then try again. | 已有 3 个对话在运行。先停止一个，再重试。 |
  | `conversation-unavailable-service` | Conversations aren't available. | 对话功能不可用。 |
  | `conversation-in-terminal` | This session is running in the Terminal page. | 这个会话正在终端页中运行。 |
  | `conversation-show-terminal` | Show Terminal | 显示终端 |
  | `conversation-elsewhere` | This session may still be running somewhere else. | 这个会话可能还在别处运行。 |
  | `conversation-error-spawn` | Couldn't start { $program }. | 无法启动 { $program }。 |
  | `conversation-error-unsupported` | { $agent } can't continue this session through its adapter. | { $agent } 的适配器不能继续这个会话。 |
  | `conversation-error-load` | { $agent } couldn't open this session. | { $agent } 无法打开这个会话。 |
  | `conversation-error-protocol` | { $agent } sent a reply that Lectern can't read. | { $agent } 发来的回复无法识别。 |
  | `conversation-error-exited` | { $agent } stopped unexpectedly. | { $agent } 意外退出。 |
  | `conversation-error-auth` | Sign in to { $agent } from its command line, then send again. | 请先在 { $agent } 的命令行里登录，再重新发送。 |
  | `conversation-error-prompt` | { $agent } returned an error: | { $agent } 返回了错误： |
  | `conversation-dismiss` | Dismiss | 关闭 |
  | `conversation-empty-no-session` | This review isn't from an agent session. | 这个审阅不来自 agent 会话。 |
  | `conversation-empty-missing` | The session's log no longer exists. | 会话日志已不存在。 |
  | `conversation-empty-turns` | No messages yet. | 还没有消息。 |
  | `conversation-unrecognized` | Lectern can't read this session's format. | 无法识别这个会话的格式。 |
  | `conversation-error-history` | Couldn't read this session. | 无法读取这个会话。 |
  | `conversation-loading` | Loading the conversation… | 正在载入对话… |
  | `conversation-terminal-explain` | Runs { $command } on this page. | 在这一页运行 { $command }。 |
  | `conversation-terminal-resume` | Resume in Terminal | 在终端中恢复 |
  | `conversation-terminal-disconnect-resume` | Disconnect and Resume | 断开并恢复 |
  | `conversation-terminal-linked` | Stop the conversation on the Messages page, then resume here. | 先在「消息」页停止对话，再在这里恢复。 |
  | `conversation-terminal-starting` | Starting… | 正在启动… |
  | `conversation-terminal-running` | Running | 运行中 |
  | `conversation-terminal-exited` | Exited ({ $code }) | 已退出（{ $code }） |
  | `conversation-terminal-signaled` | Ended by a signal | 被信号结束 |
  | `conversation-terminal-close` | Close | 关闭 |
  | `conversation-terminal-not-resumable` | This session can't be resumed. | 这个会话不能恢复。 |
  | `conversation-terminal-outside-project` | The session's folder isn't in an open project. | 会话的文件夹不在已打开的项目里。 |
  | `conversation-terminal-cwd-missing` | The session's folder no longer exists. | 会话的文件夹已不存在。 |
  | `conversation-terminal-too-many` | Too many terminals are open. Close one, then try again. | 打开的终端太多。先关闭一个，再重试。 |
  | `conversation-terminal-not-installed` | Install { $program } to resume this session. | 安装 { $program } 后才能恢复这个会话。 |
  | `conversation-terminal-failed` | Couldn't start the terminal. | 无法启动终端。 |
  | `conversation-terminal-unavailable` | The terminal isn't available. | 终端不可用。 |
  | `conversation-leave-title` | { $count } terminal(s) still running（复数形式） | 还有 { $count } 个终端在运行 |
  | `conversation-leave-detail` | Their sessions stop when the terminals close. | 终端关闭后，其中的会话也会停止。 |
  | `conversation-leave-end` | End Terminals | 结束终端 |
  | `conversation-leave-cancel` | Cancel | 取消 |

- UI 探针的语义节点（`ui::semantic`；`NodeId` 随项建一次放在项里，每帧只克隆）：

  | NodeId | 角色 | 说明 |
  |---|---|---|
  | `conversation` | `Group` | 页的根（在 `workspace.inspector` 之下）；`attrs`：`page`（`messages` / `terminal`） |
  | `conversation.pages` | `TabBar` | 页内分段控件 |
  | `conversation.pages.tab[messages]`、`conversation.pages.tab[terminal]` | `Tab` | `State::selected` |
  | `conversation.status` | `Text` | `Label::key` 是状态的键；`attrs`：`link`（`Link` 的判别名）、`context`（百分比或 `none`）、`agent` |
  | `conversation.list` | `List` | `attrs`：`entries`、`earlier` |
  | `conversation.list.item[<entry-key>]` | `ListItem` | `attrs`：`kind`（`prompt` / `notes` / `agent` / `injected` / `tools` / `ended`）、`streaming`、`truncated` |
  | `conversation.earlier` | `Button` | |
  | `conversation.plan` | `Group` | `State::expanded`；`attrs`：`done`、`total` |
  | `conversation.permission[<id>]` | `Dialog` | `<id>` 是 `PermissionId` 的十进制；`attrs`：`kind`（`CallKind` 的判别名）、`detail`（`command` / `edits` / `none`） |
  | `conversation.permission[<id>].option[<kind>]` | `Button` | `<kind>` 是 `allow-once` / `allow-always` / `reject-once` / `reject-always` |
  | `conversation.permission.more` | `Button` | `State::expanded` |
  | `conversation.composer` | `Group` | |
  | `conversation.composer.field` | `TextField` | `State::disabled`；`Label::key` = `conversation-reply-label`（不记录输入的文字） |
  | `conversation.composer.send`、`conversation.composer.stop` | `Button` | `State::disabled` |
  | `conversation.notice` | `Text` | `Label::key` 是原因的键 |
  | `conversation.notice.action` | `Button` | 「显示终端」「关闭」等 |
  | `conversation.empty` | `Text` | `Label::key` 是空状态的键 |
  | `conversation.terminal` | `Group` | `attrs`：`state`（`none` / `starting` / `running` / `exited` / `failed`） |
  | `conversation.terminal.resume` | `Button` | `State::disabled`；`Label::key` 是按钮的键 |
  | `conversation.terminal.warning` | `Text` | `Label::key` |
  | `conversation.terminal.close` | `Button` | |

  终端网格自己的节点由 `terminal_view` 声明（「对其他 crate 的要求」E），挂在 `conversation.terminal` 之下。

- Action 与快捷键（KeyContext：页的根元素 `conversation`，Terminal 页 `conversation-terminal`；谓词只写一层，不用 `A > B`，原因见 [workspace 设计「与设计的差异」](workspace.md#与设计的差异)里 ⌥⌘I、⌘\ 的绑定一条）：

  | Action | 键 | 上下文 | 处理 |
  |---|---|---|---|
  | `conversation::Send` | ⌘↩ | 回复框（`TextArea::on_submit`，「对其他 crate 的要求」F） | 按「一处驱动」与「草稿」发 `Prompt { origin: User }` |
  | `conversation::Stop` | ⌘. | `conversation` | `link` 为 `Working` 时发 `Cancel` |
  | `conversation::ShowMessages`、`ShowTerminal` | — | `conversation` | 切页（分段控件的点击） |
  | `conversation::AllowOnce`、`AllowAlways`、`RejectOnce`、`RejectAlways` | — | `conversation` | 回答最早的请求里这一种的选项；没有这一种时不处理 |
  | `conversation::LoadEarlier` | — | `conversation` | `LoadEarlier` |
  | `conversation::TogglePlan`、`ToggleMoreRequests` | — | `conversation` | 展开 / 收起 |
  | `conversation::Dismiss` | — | `conversation` | `Dismiss` |
  | `conversation::ResumeInTerminal` | — | `conversation-terminal` | 按「一处驱动」恢复（`Idle`、`Failed` 时先 `Disconnect`） |
  | `conversation::CloseTerminal` | — | `conversation-terminal` | `Terminals::close` |

  权限选项没有默认快捷键（「数据结构」的「权限请求」）。⌘\、⌥⌘I、Esc 由 `workspace` 处理；Terminal 页里终端获得焦点时，不带 ⌘ 的键（包括 Esc）交给终端；⌘C、⌘V、⌘A 是 `terminal_view` 的 Action（上下文 `terminal`，由 `conversation-terminal` 登记，见下表）；其余带 ⌘ 的组合键照常冒泡（「对其他 crate 的要求」E）。全部 Action 名稳定、可枚举（探针按名字触发）。
- 向 `workspace` 登记的贡献（[workspace 设计「接口」](workspace.md#接口)）：

  | `Contribution` | `id` | `order` | 载荷 |
  |---|---|---|---|
  | `InspectorTab` | `conversation` | 10 | `ui::contrib::InspectorTab { label: conversation-tab, build }`：建 `ConversationTab`（Files 页的 `order` 是 0，所以排在第二） |
  | `KeyBindings` | `conversation` | 0 | 上表里有键、不经 `TextArea` 的 Action（⌘.） |
  | `KeyBindings` | `conversation-terminal` | 0 | `terminal_view::key_bindings()`：⌘C、⌘V、⌘A，以及 Esc 的 `NoAction`（挡住祖先上下文 `Workspace` 的 Esc，让它到达终端），上下文都是 `terminal`。由 `conversation-terminal` 登记 |

  `conversation-terminal` 另外登记一个离开守卫（`register_leave_guard`，「数据结构」的「离开守卫」）。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)：

| 情况 | 表现 |
|---|---|
| 审阅对象不是会话（`NotSession`） | 两页都只显示 `conversation-empty-no-session` |
| 没有 `dyn Conversations`（`conversation-live` 没有激活、失败或已停用） | Messages 页显示 `conversation-unavailable-service`，回复框不可用；Terminal 页照常 |
| 没有 `dyn Terminals` | Terminal 页显示 `conversation-terminal-unavailable`，恢复不可用；Messages 页照常 |
| `Load::Loading` | 按 `loading_phase`：100 ms 内空白，之后三块 `Skeleton`，1 s 后 `conversation-loading`；`Ready` 到达时换成内容 |
| `Load::Failed(NotFound)` / `Failed(Io)` | `conversation-empty-missing` / `conversation-error-history`；回复框按 `can_prompt` |
| `Load::Failed(Unrecognized)` | 只显示 `conversation-unrecognized`（不弹错误，与「会话格式无法识别」一致）；回复框按 `can_prompt` |
| 历史为空、也没有实时的项 | `conversation-empty-turns` |
| `can_prompt` 为 `Err(…)` | 回复框不可用，下面一行写对应的 `conversation-unavailable-*`；`Busy` 不写原因（状态行已说明），只是 Send 不可用 |
| 终端在驱动这个会话（`Starting` 或 `Running`） | 回复框换成 `conversation-in-terminal` 和「显示终端」；状态行「在终端中运行」；`TerminalEvent::Listed` 之后重读的 `state` 不再是这两种时，回复框恢复 |
| 会话可能还在别处运行 | 回复框与恢复按钮上方的 `conversation-elsewhere`（只是提示） |
| `Link::Connecting` | 状态行「正在连接…」；回复框里刚发出的 prompt 已在列表末尾 |
| `Link::Failed(e)` | 状态行「已断开」；列表末尾一行对应的 `conversation-error-*` 和「关闭」；`PromptFailed` 另起一行原样显示 agent 的错误文字（`TextRole::Secondary`） |
| 有待确认的请求 | 状态行「等待你确认」；卡片；当前会话新进入等待且检查器没显示这一页时切过来 |
| 请求被 agent 撤回、已被回答、连接关闭 | 卡片随快照消失 |
| 回答的请求已不在（快照过时） | 提供者忽略；卡片随下一个快照消失 |
| 正文超过 64 KiB | 截断，末尾 `conversation-truncated` |
| 恢复被拒绝（`open` 返回 `Err(LaunchError)`） | `AlreadyRunning(id)`：页改用这个终端，不显示原因。其余在恢复按钮下面写一行原因：`NotResumable`、`UnknownSession`、`UntrustedSource`、`InvalidSessionId` 写 `conversation-terminal-not-resumable`，`OutsideProject` 写 `conversation-terminal-outside-project`，`TooMany` 写 `conversation-terminal-too-many`。`TooMany` 时按钮仍可用（关闭一个终端后再试），其余时不可用 |
| 终端启动失败（`RunState::Failed(reason)`，`open` 成功之后随 `Listed` 到达） | 状态行写原因：`NotInstalled(agent)` 写 `conversation-terminal-not-installed`（`$program` 是 `agent.program()`），`CwdMissing` 写 `conversation-terminal-cwd-missing`，`OutsideProject` 写 `conversation-terminal-outside-project`，`NoShell`、`Spawn`、`Internal` 写 `conversation-terminal-failed`。恢复按钮可以再试（`open` 关闭这个终端，换成新的） |
| 终端退出（`RunState::Exited`） | 状态「已退出（退出码）」，`signaled` 时「被信号结束」；保留最后的画面，恢复按钮重新出现 |
| 关闭项目或退出时还有运行中的终端 | 离开守卫弹出确认（「数据结构」的「离开守卫」）；取消时留在原处 |
| 插件停用时页正在显示 | `conversation` 停用：`workspace` 注销检查器页，页 drop，关注释放；`conversation-live` / `conversation-terminal` 停用：按上面「没有 …」两行 |
| 事件的 `generation` 低于已应用的 | 丢弃 |
| 快照不一致（`streaming` 的项不在末尾、键重复，不应发生） | 重复的键只取第一项，记 warn，不 panic |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `conversation`。主线程上的记录带调用线程已有的作用域（`workspace` 调用视图工厂和 Action 处理时恢复本插件的 `plugin`、`act`，[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 11 项）。与一个会话有关的记录在 `session` 作用域里写出：会话 ID 是 UUID 时取它的字节，否则取 128 位哈希，与 `agents`、`usage`、`review` 相同，所以与 `agents` 的 ACP 记录能对应。

- 默认记录：

  | 事件 | 级别 | `msg` | 字段 |
  |---|---|---|---|
  | 页为一个会话建好 | info | `conversation shown` | `page`（`messages` / `terminal`）、`link`（判别名）、`entries`、`pending`（待确认的请求数） |
  | 发出回复 | info | `prompt sent` | `bytes`（文字的字节数）、`connects`（bool：这次发送是否启动连接） |
  | 停止 | info | `stop requested` | — |
  | 回答权限请求 | info | `permission answered` | `option`（`allow_once` / `allow_always` / `reject_once` / `reject_always`）、`kind`（`CallKind` 的判别名）、`waited_ms`（从 `asked_at` 起） |
  | 因新请求切到这一页 | info | `conversation tab selected for a request` | `pending` |
  | 恢复终端 | info | `terminal resume requested` | `outcome`（`started` / `LaunchError` 的判别名）、`disconnected`（bool：先断开了 ACP 连接） |
  | 关闭终端 | info | `terminal closed` | `state`（`starting` / `running` / `exited` / `failed`） |
  | 离开守卫弹出确认后得到回答 | info | `terminal leave answered` | `scope`（`project` / `app`）、`running`（运行中的终端数）、`answer`（`end` / `cancel`） |
  | 显示 `Link::Failed` | warn | `conversation failed shown` | `error`（`LinkError` 的判别名） |
  | 快照不一致 | warn | `snapshot inconsistent` | `generation`、`kind`（`duplicate_key` / `streaming_not_last`） |
  | `started` 时读不到当前屏幕 | warn | `route unavailable` | — |
- 详细级别（debug）：`snapshot applied`（`generation`、`entries`、`items`、`spliced`、`anchor_kept`（bool）、`took_us`）；`route changed`（`current`：`none` / `not_session` / `session`）；`page switched`（`page`）；`draft restored`（`bytes`）；`send skipped`（`reason`：`empty` / `in_terminal` / `unavailable` / `refused`）；`waiting changed`（`sessions`、`new`）。trace 不另记；渲染路径（每帧的 `render`、滚动事件）上不加记录。
- 关联 ID：`plugin`、`act` 由 `workspace` 在调用工厂和 Action 处理时恢复；`session` 由本 crate 设置。本 crate 不发对外请求，不设 `req`。
- 不记录：prompt 与草稿的文字、agent 说明、注入内容、工具行的 `summary` 与权限请求的 `title`、命令文本与 `workdir`、改动预览的路径与行、计划的文字、`PromptFailed` 的错误文字、`OptionId`、审阅意见的位置、会话标题、终端的输出与输入、`EntryKey`。用字节数、计数、判别名、`generation` 和耗时代替。
- 金丝雀测试：`logs_hold_no_contents`（`src/tests/logs.rs`，`lectern_logging::testing::capture()` 在 trace 级别）：用含金丝雀的 prompt、草稿、agent 说明、注入内容、工具 `summary`、请求的 `title`、命令文本、`workdir`、预览路径与行、计划、错误文字、`OptionId`、审阅意见位置构造快照，经假 `Conversations` 走完「测试」里第 1、2 步的流程，第 3 步再加上终端的恢复与关闭，`assert_absent(金丝雀)`，并检查默认记录的字段。

## 测试

依赖 GPUI，只在 macOS 上编译和测试：`just test -p lectern-conversation`。不用真实的会话日志、ACP agent、PTY 和文件 IO；`dyn Contributions`、`dyn SessionSource`、`dyn Conversations`、`dyn Terminals` 用本 crate 的假实现（记录调用、按测试的需要换出快照和 `RunState`、记录活着的 `FrameInterest` 数、从任意线程发事件），装在 `TestKernel` 上（`testing::harness`）。本 crate 不能依赖 `agents`、`terminal` 的 `testing`（架构规则 7）。

- 纯函数（`#[test]`）：
  - `items_*`：项的顺序；连续工具调用合成一行、至多 4 项加「另有 n 项」；`ReviewNotes` 成为 `Notes`；`earlier > 0` 时 `Earlier` 在最前；新旧快照的键比较只报变化的项（流式更新只有最后一项）。
  - `options_*`：按钮按 `OptionKind` 排序；只画给出的种类；文字来自 Fluent 键，不来自 `OptionId`。
  - `drive_*`：「一处驱动」表的每一行（`can_prompt`、`for_session` 与终端的 `RunState`、`link` 的组合）；`Exited`、`Failed` 的终端不阻止回复框发送。
  - `elsewhere_*`：`open_turn`、`last_activity` 的 2 分钟边界、`link`、终端四个条件。
  - `memory_*`：草稿的保存、放回、16 个的淘汰、256 KiB 的上限。
- `#[gpui::test]`（`TestAppContext`，`lectern_ui::init_fixed`，`testing::harness`）：
  - `tab_follows_the_review_route`：`ScreenChanged` 到会话审阅、到 `Outside` 审阅、回到项目主页；`started` 之前发出的 `ScreenChanged` 由 `current_route` 补上。
  - `history_renders_in_order_with_loading_phases`：`Loading` 时 100 ms 内空白、之后骨架；`Ready` 后项的顺序与快照一致。
  - `earlier_turns_load_on_request`：点「显示之前的 n 轮」发 `LoadEarlier`；新项在前面插入时列表位置不变。
  - `streaming_follows_the_bottom_only_when_at_the_bottom`：停在底部时跟随；向上滚动后流式更新不移动列表。
  - `streaming_relayouts_only_the_last_item`：200 次流式更新里只有最后一项被 `splice`。
  - `send_with_cmd_enter_sends_a_prompt`：⌘↩ 发 `Prompt { origin: User }`，快照变为 `Connecting` 后清空回复框；空白文字不可发送；提供者拒绝时文字保留。
  - `composer_shows_why_it_cannot_send`：`can_prompt` 的每个 `Err` 对应一行原因。
  - `composer_is_replaced_while_the_terminal_drives`：假 `Terminals` 的终端是 `Starting`、`Running` 时显示 `conversation-in-terminal`，「显示终端」切页；换成 `Exited` 并发 `Listed` 后回复框恢复可用。
  - `stop_sends_cancel`。
  - `permission_card_uses_kind_labels`：`OptionId` 里放金丝雀，界面和语义树里都没有它；按钮顺序固定；↵ 不触发选项。
  - `permission_command_and_edits_are_shown`：`Command` 的文字与 `workdir`；`Edits` 的路径、增删数、行与 `omitted`；`None` 时 `conversation-permission-no-detail`。
  - `answer_removes_the_card`：点「允许」发 `Answer`，卡片消失；第二个请求成为完整显示的那个。
  - `new_request_selects_the_tab`：当前会话新进入 `waiting()` 时 `dispatch(SelectInspectorTab("conversation"))` 一次；别的会话进入等待时不发。
  - `context_is_shown_only_when_given`。
  - `failed_link_shows_the_reason_and_dismisses`：每种 `LinkError` 一行；`PromptFailed` 的文字原样显示；「关闭」发 `Dismiss`。
  - `drafts_survive_hiding_the_inspector`：写字、隐藏检查器、再显示，文字还在；换会话后各自的草稿。
  - `terminal_page_resumes_and_embeds_the_pane`：恢复调 `Terminals::open`，`launch` 是 `Launch::Resume(key)`，`size` 由 `area` 换算，`theme` 来自 `theme(window, cx)`；`TerminalPane::new` 收到 `conversation-terminal` 这次激活的 token，焦点移到它的 `focus_handle()`；每种 `LaunchError` 对应「状态与异常」里的一行原因，`AlreadyRunning(id)` 时页改用那个终端；`Exited`（含 `signaled`）后恢复按钮重新出现；关闭调 `close`。
  - `terminal_failure_arrives_after_open`：`open` 返回 `Ok` 后假 `Terminals` 把状态换成 `Failed(NotInstalled(ClaudeCode))` 并发 `Listed`：状态行显示安装 `claude` 的提示，恢复按钮可用；`CwdMissing`、`Spawn` 同样各一行。
  - `terminal_frames_only_while_visible`：Terminal 页显示时有一个 `FrameInterest`；切到 Messages 页、换会话、检查器隐藏后为 0；再切回时重新 `watch`。页上那个终端的 `Frame` 和任何 `Listed` 让 `TerminalPane` 刷新一次（同一批里的多个合成一次），别的终端的 `Frame` 不刷新。
  - `leave_guard_*`：`Project(key)` 只数这个项目里 `Starting` / `Running` 的终端，`App` 数全部；数目为 0、`ServiceRef::get()` 为 None，或范围是 `View`、`Window`、`Worktree` 时立即 `done(true)`，不弹出确认；有运行中的终端时弹出确认，「结束终端」给 `done(true)`，「取消」给 `done(false)`。
  - `terminal_resume_respects_the_link`：`Idle`、`Failed` 时先 `Disconnect` 再恢复；`Connecting`、`Working`、`Stopping` 时不可用。
  - `plugins_register_and_deactivate_cleanly`：激活后假 `Contributions` 收到 `conversation` 的 `InspectorTab`、`KeyBindings`，以及 `conversation-terminal` 的 `KeyBindings`（与 `terminal_view::key_bindings()` 相同）和离开守卫；各插件停用后它的登记 drop。`live_plugin_deactivates_on_its_own`、`terminal_plugin_deactivates_on_its_own`：单独停用后另一页照常，终端的快捷键和离开守卫随 `conversation-terminal` 注销。
  - `stale_generation_is_dropped`。
- 语义树：`conversation_tree_history`、`conversation_tree_live_with_permission`、`conversation_tree_terminal`、`conversation_tree_not_session` 的 insta 快照（`ui_probe` 的 `tree::snapshot`）。
- 日志：`logs_hold_no_contents`。
- fixtures：没有外部格式；`testing::snapshot_fixture` 在代码里合成快照。ACP 消息的样本属于 `agents`（「对其他 crate 的要求」C）。

## 对其他 crate 的要求

每一项在对应 crate 的设计文档里补充，各自一个 PR，写 `Refs #23`（终端的几项另写 `Refs #12`）；在「验收」里列为前置条件。

- **A. `workspace`**：插件在 `Live` 之前收不到 `ScreenChanged`，`started` 时要读当前屏幕。`Contributions` 加一个方法：

  ```rust
  /// 窗口当前的屏幕（栈顶；栈空时是基底）。取锁复制，不阻塞，任何线程。窗口不存在时为 None。
  fn current_route(&self, window: WindowId) -> Option<Route>;
  ```

  测试：压入、弹出、打开与关闭项目之后的返回值与 `ScreenChanged::to` 一致。
- **B. `services`**：采纳本文「服务接口」一节的 `conversation` 模块（类型、`dyn Conversations` 的服务 ID、`ConversationEvent`、`ConversationInterest`）；`crates/services/AGENTS.md` 的模块列表加上它。测试：类型的 JSON 往返；`ID == "conversations"`、`MULTIPLE == false`；`ConversationInterest` drop 时调用一次回调。
- **C. `agents`**：按 [ADR 0050](../adr/0050.md) 实现 ACP 客户端，新增插件 "conversations"（provides `dyn Conversations`，requires `dyn SessionSource`、`dyn Projects`，`AfterFirstFrame`，`activate` 只登记，没有发送之前不启动进程）。设计文档的修订写明：
  - 核对（先于实现）：按两个适配器的公开源码确认程序名、能否用上游会话 ID 恢复会话（`session/resume` 或 `session/load`）、ACP 的会话 ID 是否就是上游会话 ID、经 ACP 进行的轮次是否写进 CLI 同一份本机日志（审阅的新一轮来自这份日志）、是否发出 `usage_update`、执行命令的原始输入里命令在哪个字段。哪一项不成立，这个 agent 的 `can_prompt` 就是 `Unavailable::Unsupported`，只有历史与 Terminal 页。来源提交写进「外部格式」。
  - 启动：只对 `SessionSource::resume` 给出 `ResumeCommand` 的会话（内置来源，会话 ID 是 UUID 的规范写法），程序按 `AcpProgram` 固定映射，没有参数；工作目录与 `cwd` 的检查同 [agents 设计「服务接口」](agents.md#服务接口lectern-services-的-session-模块)对恢复命令的规则；环境与 Terminal 页启动 agent 时相同：`lectern_platform::login_env::login_env()` 给出的登录 shell 环境（取不到时退回 Lectern 自己的环境），去掉 `LECTERN_` 开头的变量，程序用 `find_program(name, 登录环境的 PATH)` 查找（[terminal 设计](https://github.com/SoloJiang/lectern-app/pull/265)「启动命令」「子进程的环境」与「对其他 crate 的要求」第 2 项；`lectern-platform` 是非插件的库，`agents` 可以依赖）；`initialize` 声明 `protocolVersion: 1`，不声明 `fs`、`terminal`；`mcpServers` 为空；agent 支持 `session/resume` 时用它，否则用 `session/load` 并丢弃回放的更新（不建项，只计数）。`initialize` 与恢复各有 30 s 的超时，超时为 `Failed(Protocol)`。程序的查找在 executor 的后台任务里做，UI 线程上不查找：第一次 `watch` 一个有 ACP 程序的会话时查找，得出 `can_prompt` 的 `NotInstalled`。`login_env()` 在本次运行里第一次调用时运行一次用户的登录 shell（阻塞，至多 5 s），结果与 `terminal` 共用；这是取环境，不是启动 agent（ADR 0050）。查找完成之前 `can_prompt` 不因安装与否为 `Err`；这时发出的 prompt 等查找完成，找不到程序时 `link` 为 `Failed(Spawn)`。查找结果在本次运行里缓存，`Spawn` 失败或 `NotInstalled` 之后的下一次 `watch` 重新查找（登录环境不重取，只重新 `find_program`）。
  - 运行：每个连接一个读 agent 输出的专用线程（`Executor::run_dedicated_gated`），不启动异步运行时；一行至多 16 MiB，超过时关闭连接（`Protocol`）；不认识的 `session/update` 种类跳过并计数；`session/update` 合并后至多每 50 ms 换一次快照；stderr 只计字节数，不保留内容。
  - 关闭：空闲（`Idle` 且没有待确认的请求）10 分钟（`ManualClock` 可测）、`Disconnect`、项目关闭、插件停用时关闭 stdin、向进程组发 SIGTERM，2 s 后 SIGKILL；Lectern 退出时等待至多 500 ms。至多 3 个连接（ADR 0050）。
  - 快照：本文「服务接口」的不变量；历史窗口先读最近 20 轮，`LoadEarlier` 每次 20 轮，至多 200 轮；正文按 `ContentRef` 读，每段至多 64 KiB，合计至多 1 MiB；读取在 executor 上（`Interactive` 优先级，持有 `BusyGuard`）。工具行的 `CallKind` 从 `ToolKind`（历史）或 ACP 的 tool call kind（实时）映射。
  - 权限：请求不自动回答；`options` 按种类去重；`PermissionDetail::Command` 从各适配器原始输入里的命令字段取（取不到为 `None`）；改动预览用 `git::line_diff` 算，以两侧内容的 git blob OID 为键放在至多 64 项的 LRU 里；`Cancel` 和连接关闭时待确认的请求回答 `cancelled`。
  - 日志：进程的启动与退出按 [logging 设计「隐私」](logging.md#隐私)的子进程规则（程序名不含目录、参数个数、工作目录、pid、退出状态）；JSON-RPC 只记方法名（固定表里的字面量，表外记 `other`）、`req`、字节数、耗时；不记参数与结果的内容、原始输入、标题、环境变量、stderr。金丝雀测试同本文。
  - 测试：假的 ACP agent 放在 PATH 最前面（AGENTS.md「工作方式」）：它记录收到的参数、工作目录、环境变量和每条消息，按合成的脚本回放（`crates/agents/tests/fixtures/acp/`，不用真实会话）。覆盖：`initialize` 没有 `fs`、`terminal`；权限请求在没有 `Answer` 时一直等待；`Cancel` 回答 `cancelled`；空闲 10 分钟退出；第 4 个连接；拒绝的情况（第三方来源、非 UUID、`cwd` 在项目之外）；超长行；停用时进程组被终止。
  - 修改 agents 设计「范围」里「ACP 会话作为另一个 `SessionSource` 实现接入」一句：ACP 不是会话来源，经 ACP 的轮次从本机日志读到；ACP 对话由 `dyn Conversations` 提供。
- **D. `terminal`**：接口由 [terminal 设计](https://github.com/SoloJiang/lectern-app/pull/265)定义（`services::terminal` 模块，单实现服务 `dyn Terminals`，服务 ID 与插件 ID 都是 "terminal"），名字以它为准。terminal 设计「对其他 crate 的要求」第 4 项列出了它对本文的要求，本文已按它改写。本 crate 只用下面这些：
  - `open(OpenSpec { launch: Launch::Resume(key), size, theme })`：在调用线程上按 agents 设计对 `ResumeCommand` 的规则检查来源、会话 ID 和 cwd，不阻塞、不做 IO。返回之前终端已登记（`for_session`、`info` 查得到，状态 `Starting`）。这个会话已有 `Starting` / `Running` 的终端时返回 `Err(LaunchError::AlreadyRunning(id))`；已有 `Exited` / `Failed` 的终端时先关闭它、换成新的。`NotInstalled` 不由 `open` 返回：查 `PATH` 是 IO，结果随 `Listed` 以 `RunState::Failed(FailReason::NotInstalled(agent))` 到达。`LaunchError` 的其余变体见「状态与异常」。
  - `for_session(key)`；`info(id)` 的 `state`（是否在驱动会话只看 `Starting`、`Running`）；`list()` 的 `project` 与 `state`（离开守卫）。
  - `watch(id)` 返回的 `FrameInterest`：页显示时持有（「数据流与线程」）。`frame(id)` 由 `TerminalPane` 读。
  - `input(id, Input::Paste(text))`（I 用）：去掉 C0（保留 `\t`、`\n`）、ESC 和 C1 控制字符，程序开了 bracketed paste 时包上首尾序列，不追加回车。
  - `close(id)`。
  - 事件：`TerminalEvent::Listed { generation }`（重读 `info`）；`Frame { id, generation }`（只在有 `FrameInterest` 时发出，交给 `TerminalPane::refresh`）；`Bell` 不处理。事件只在终端线程上发出。
  - `terminal` 是 `AfterFirstFrame` 激活的插件，第一次 `open` 之前不建线程、不取登录环境；终端在项目关闭和插件停用时关闭；终端的输入、输出和粘贴的文字不进日志。
- **E. `terminal_view`**：是非插件的视图库（插件 crate 才能依赖它；terminal 设计「对其他 crate 的要求」第 5 项）。terminal_view 的设计已采纳下面的名字与签名，以它为准；本 crate 用到：

  ```rust
  impl TerminalPane {
      /// 显示一个终端。`token` 是宿主插件这次激活的 token（本 crate 传 `conversation-terminal` 的
      /// `PluginHandle::token()`），复制时交给 `Terminals::copy_selection`。
      /// 不调用 `watch`：`FrameInterest` 由宿主持有（本 crate 的 `TerminalPage::frames`），页隐藏时宿主 drop 它。
      pub fn new(terminals: ServiceRef<dyn Terminals>, token: ActivationToken, id: TerminalId, window: &mut Window, cx: &mut Context<Self>) -> Self;
      /// 宿主在收到这个终端的 `TerminalEvent::Frame`、或任何 `TerminalEvent::Listed` 时调用：读 `frame(id)` 与 `info(id)`，变了才重画。
      pub fn refresh(&mut self, cx: &mut Context<Self>);
      pub fn focus_handle(&self) -> FocusHandle;
  }
  /// 这个窗口当前外观的终端颜色（来自 `ui` 的系统语义色），用于 `OpenSpec::theme`。外观按窗口取，所以要 `window`。
  /// 外观变化之后由 `TerminalPane` 自己发 `Input::SetTheme`。
  pub fn theme(window: &Window, cx: &App) -> Theme;
  /// 按区域的大小和终端字体的单元尺寸算网格，用于 `OpenSpec::size`；至少 20 列。第一次调用时量字体并缓存，所以要 `&mut App`。
  pub fn grid_size(area: Size<Pixels>, window: &Window, cx: &mut App) -> GridSize;
  /// 宿主插件放进自己的 `KeyBindings` 贡献：⌘C、⌘V、⌘A 与 Esc 的 `NoAction`，上下文 `terminal`（`TerminalPane` 根元素的 KeyContext）。
  pub fn key_bindings() -> Vec<(&'static str, Box<dyn Action>, Option<&'static str>)>;
  ```

  约束：网格尺寸随所在区域变化（最少 20 列），变化时发 `Input::Resize`；获得焦点时不带 ⌘ 的键（包括 Esc）交给终端，⌘C / ⌘V / ⌘A 是复制、粘贴、全选，其余带 ⌘ 的组合键冒泡；声明自己的语义节点（只有尺寸、状态，不含屏幕内容），挂在 `conversation.terminal` 之下。
- **F. `ui`**：新增多行文本框（#22 的意见框也要用）：

  ```rust
  /// 多行文本框：系统样式（`control_background` 底、圆角、0.5pt `separator` 边，聚焦时 `accent` 的焦点环），
  /// 包装 gpui-component 的多行 `Input`；视图不直接用 gpui-component 的 `Input`。语义角色 `TextField`。
  pub struct TextArea { /* id, state, placeholder, min_lines, max_lines, disabled, on_submit */ }
  pub struct TextAreaState { /* Entity 里的 InputState */ }
  impl TextArea {
      pub fn new(id: NodeId, state: &Entity<TextAreaState>) -> TextArea;
      pub fn placeholder(self, text: Text) -> TextArea;
      pub fn lines(self, min: u8, max: u8) -> TextArea;   // 随内容增高
      pub fn disabled(self, disabled: bool) -> TextArea;
      /// ⌘↩：由文本框自己处理（gpui-component 的 `Input` 上下文比外面的 KeyContext 深，外面的绑定拿不到这个键）。
      pub fn on_submit(self, f: impl Fn(&mut Window, &mut App) + 'static) -> TextArea;
      /// Esc：同样由文本框自己处理；输入法组字时 Esc 先给输入法，不调用。没有设置时 Esc 照常冒泡（#22 的意见框要用）。
      pub fn on_cancel(self, f: impl Fn(&mut Window, &mut App) + 'static) -> TextArea;
  }
  impl TextAreaState {
      pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self;
      pub fn text(&self) -> SharedString;
      pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>);
      pub fn focus_handle(&self) -> FocusHandle;
  }
  ```

  输入法、撤销重做、↵ 换行由 `Input` 自带。`TextAreaState` 的文字变化时 `cx.notify()`，持有它的视图用 `cx.observe` 跟着重画（#22 的保存按钮按文字是否为空、是否超长决定可用）。语义节点不记录输入的文字。
- **G. `i18n`**：`locales/{en,zh-CN}/conversation.ftl` 加上「界面」表里的键，`keys::conversation::*`。
- **H. `project_home`**：会话行的等待标记（[project_home 设计「范围」](project_home.md#范围)里留给 Conversation 的一项）。按 `project-home-usage` 的做法加一个插件（requires `dyn Conversations`），渲染时读 `waiting()`，收到 `WaitingChanged` 后重画；`waiting()` 里的会话在行上显示等待的圆点和一行次要文字，语义节点加 `attrs`：`waiting`。键与文字写在 project_home 自己的 ftl 里。
- **I. 审阅意见的会话途径（#22）**：本 crate 另加一个小插件（如 "conversation-notes"），requires `dyn Review`、`dyn Conversations`、`dyn Terminals`，在 `started` 里调 `Review::register_note_route`，把登记交给 `PluginHandle::own`（[review 设计「审阅意见的发送途径」](review.md#审阅意见的发送途径noteroute)）。不放进 Conversation 页的插件本身：`review` 停用或失败时页面照常。`review_view` 只发 `SendNotes`，不依赖本 crate 和 `terminal`。途径的行为：
  - `applies`：审阅对象是 `ReviewTarget::Session`。
  - `deliver`：按本文「一处驱动」选一条路。有终端在驱动这个会话（`for_session` 有值且 `info(id).state` 是 `Starting` 或 `Running`）就不经 ACP：`Running` 时 `Terminals::input(id, Input::Paste(text))`（不追加回车，用户在终端里按 ↵ 提交），答复 `Delivered(SentVia::TerminalPaste)`；`Starting` 时答复 `NotNow(RouteBlock::Starting)`（进程运行之前 `terminal` 丢弃发给程序的输入，terminal 设计「接口」的 `input`）。没有终端在驱动时，`can_prompt` 为 `Ok` 就发 `Conversations::command(Prompt { text, origin: ReviewNotes { count, locations } })`（`locations` 是 `NoteDelivery::locations` 按 `root` 拼成的绝对路径，没有行号的不列），答复 `Delivered(SentVia::Acp)`；为 `Err` 时按原因答复 `NotNow(Busy)` 或 `NotNow(NoSession)`。
  - 送出之后由 `review_view` 切到 Conversation 页（`dispatch(SelectInspectorTab("conversation"))`）。Conversation 页按本 crate 记下的上一次所在的页打开：用户在 Terminal 页恢复过这个会话、之后没有切走时就是 Terminal 页，否则 Messages 页上有「显示终端」。
  - 分阶段：P1 的出口关卡 G2 不做这个途径，`review` 只有内置的剪贴板途径，用户把 Copy as Prompt 复制的 prompt 粘贴进会话（产品负责人 2026-10-30 决定，[review 设计「未决问题」](review.md#未决问题)4）。G2 之后、`terminal` 实现时先加终端粘贴这一条：插件只 requires `dyn Review`、`dyn Terminals`，没有终端驱动时答复 `NotNow(NoSession)`。ACP 一条随 P3 加上，插件那时再 requires `dyn Conversations`。在这个途径登记之前，`review` 只有剪贴板途径。
- **J. `app`**：`plugins()` 的三个插件和 `agents` 的 "conversations" 插件放进 `PluginSet`（`Profile::Desktop`；test profile 不装）。
- **K. `bench`**：预算表加 `conversation/stream_apply`（「性能」）。

## 未决问题

没有需要产品决定的问题。下面五条已于 2026-10-25 决定，人确认按默认选项：

1. **第一次经 Conversation 发送时，是否先确认会启动 ACP agent。** 已决定：选 (a)，直接发送，状态行显示「正在连接…」。发送本身是明确的操作，agent 使用工具前仍要经过权限请求。不选：(b) 每个会话第一次发送前弹出说明。
2. **是否显示「总是允许」「总是拒绝」。** 已决定：选 (a)，agent 给出就显示，与 agent 命令行里的选项一致。这两种选项让 agent 记住规则，以后不再询问（规则由 agent 自己保存）。不选：(b) 只显示「允许」「拒绝」。
3. **当前审阅的会话有新的权限请求、检查器没有显示 Conversation 时怎么办。** 已决定：选 (a)，自动切到 Conversation 页（不移动焦点）。审阅时项目侧栏是隐藏的，交互规范里的侧栏等待标记看不到。窗口不在前台时的系统通知不在本 issue，另行决定。不选：(b) 只在工具栏的检查器按钮上加圆点；(c) 只有侧栏标记。
4. **会话可能还在别处运行时，是否允许在 Lectern 里继续。** 已决定：选 (a)，允许，只显示提示。判断只靠日志活动，可能误判。不选：(b) 不允许，直到日志 2 分钟没有活动。
5. **Terminal 页的宽度。** 已决定：选 (a)，保持 300pt，先按原型实现，实测后再定。检查器 300pt 宽，终端约 40 列，agent 的命令行界面会很挤。不选：(b) 切到 Terminal 页时检查器变宽；(c) 终端改为浮动面板。

技术问题都已在上文决定。

## 已知限制

- 「一处驱动」只覆盖 Lectern 里的两条路；用户在别处运行的同一会话只能按日志活动提示（ADR 0050）。
- 重启之后，历史里经 ACP 发回的审阅意见显示为普通 prompt 的全文：`PromptOrigin::ReviewNotes` 只在本次运行里有。审阅意见的 prompt 第一行带 `[lectern-notes <批次 ID>]` 标记（[review 设计「审阅意见」](review.md#审阅意见)），以后可以按它把历史里的这种 prompt 显示成「n 条审阅意见」，不需要读标注日志；本 issue 不做。
- 上下文占用只在 agent 发 `usage_update` 时显示。
- 单段正文超过 64 KiB 时截断，完整内容在审阅页的叙事里。
- agent 说明按纯文本显示（「范围」）。
- 实时的项换成历史的项时，如果用户不在底部，列表位置可能跳到底部。
- 权限请求的 `title` 是 agent 写的；执行命令以外的请求，界面只能显示 agent 给出的内容。
- 登录 shell 的环境在一次运行里只取一次（terminal 设计「已知限制」2）：用户装了适配器、而它的目录是新加进 shell 配置的 PATH 时，要重启 Lectern 才找得到。
- 离开守卫只在关闭项目和退出时询问；关闭窗口、切换 worktree 不结束终端，不询问。

## 验收

分四步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-conversation`、`just deny` 通过，并且：

- `rg 'std::fs|std::process|std::thread::spawn|gix|git2' crates/conversation/src` 没有结果（本 crate 不做 IO）。
- `rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|Duration::from_secs|\.font_family\(|gpui_component::' crates/conversation/src` 没有结果（颜色、字体、时长、控件只来自 `ui`）。
- `rg '"[A-Z][a-z]+ [a-z]' crates/conversation/src --glob '!**/tests*' --glob '!**/testing*'` 没有界面字符串字面量（都走 `keys::conversation::*`）。
- `rg 'eprintln!|println!|log::' crates/conversation/src` 没有结果。
- `python3 scripts/check_architecture.py` 通过：`plugin = true`，不依赖其他插件 crate（含 dev-dependencies），只有 `app` 依赖它。

前置条件：A（`workspace`）、B（`services`）、G（`i18n`，第 1 步的键）在第 1 步之前；F（`ui`）在第 2 步之前；D（`terminal`）、E（`terminal_view`）在第 3 步之前；C（`agents`）、H（`project_home`）、J（`app`）、K（`bench`）在第 4 步之前。

**第 1 步：插件、检查器页、Messages 页的显示、探针节点（`Refs #23`）。**

- 「测试」里的 `items_*`、`tab_follows_*`、`history_renders_*`、`earlier_turns_*`、`streaming_*`、`context_is_shown_*`、`failed_link_*`、`stale_generation_*`、`plugins_register_*`（`conversation`、`conversation-live` 两个插件）、`live_plugin_deactivates_*`、`logs_hold_no_contents`（第 1 步的流程）通过；语义树快照 `conversation_tree_history`、`conversation_tree_not_session` 通过。
- `rg 'TextArea|ConversationCommand::Answer' crates/conversation/src/view` 没有结果（回复框与权限在第 2 步）。
- `crates/conversation/AGENTS.md` 的设计文档行指向本文（本 PR 已改）。

**第 2 步：回复框、停止、权限请求、草稿（`Refs #23`、`Refs #24`；前置 F）。**

- `options_*`、`drive_*`（回复框一侧）、`elsewhere_*`、`memory_*`、`send_with_cmd_enter_*`、`composer_shows_*`、`composer_is_replaced_*`、`stop_sends_cancel`、`permission_card_uses_kind_labels`、`permission_command_and_edits_are_shown`、`answer_removes_the_card`、`new_request_selects_the_tab`、`drafts_survive_*` 通过；`logs_hold_no_contents` 加上第 2 步的流程后仍通过；语义树快照 `conversation_tree_live_with_permission` 通过。

**第 3 步：Terminal 页（`Refs #23`、`Refs #12`；前置 D、E）。**

- `drive_*`（恢复一侧）、`terminal_page_resumes_*`、`terminal_failure_arrives_after_open`、`terminal_frames_only_while_visible`、`leave_guard_*`、`terminal_resume_respects_the_link`、`terminal_plugin_deactivates_*`、`plugins_register_*`（加上 `conversation-terminal` 的快捷键与离开守卫）通过；`logs_hold_no_contents` 加上终端的流程后仍通过；语义树快照 `conversation_tree_terminal` 通过。
- `rg 'TerminalPane::new' crates/conversation/src` 只在 Terminal 页里（`src/view/terminal.rs`）。

**第 4 步：`app` 装配、基准、真机验证（`Closes #23`、`Closes #24`；`terminal`、`terminal_view` 的 PR 都已合并时写 `Closes #12`，否则 `Refs #12`；前置 C、H、J、K）。**

- `just test -p lectern-app`：`every_profile_graph_validates` 里 desktop 的插件数加四（本 crate 三个、`agents` 一个）；启动测试里首帧之前没有 `conversation` 的记录。
- `just bench conversation/stream_apply`：p95 ≤ 2 ms。
- 在 macOS 上运行发布构建，用户已安装两个适配器并已登录：打开一个有 Claude Code 会话的仓库，进入审阅，⌘\ 打开 Conversation：历史显示，没有 agent 进程（`ps` 里没有适配器）；回复一句后状态变为「运行中」，agent 请求运行命令时出现卡片、显示命令文本，点「允许」后继续；这一轮结束后审阅页出现新一轮；空闲 10 分钟后适配器进程退出；切到 Terminal 页恢复会话，终端显示 `claude --resume` 的界面，回复框换成「在终端页中运行」。Codex 会话重复一遍。
