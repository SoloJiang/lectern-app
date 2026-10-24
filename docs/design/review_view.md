# review_view 设计

状态：实现中。「验收」第 1 步（插件、屏幕、叙事与统一视图、简报、Progress、快捷键、探针节点）已实现；第 2、3 步尚未实现。

对应 issue [#15](https://github.com/SoloJiang/lectern-app/issues/15)（审阅视图的视图部分；领域部分见 [review 设计](review.md)）。`review_view` 是审阅屏幕：顶部的会话简报、按轮次的改动叙事、每个文件的 diff（统一与双栏，[#14](https://github.com/SoloJiang/lectern-app/issues/14)）、检查器里的 Progress 列表、底部浮条，以及 J / K / V 等快捷键（[交互规范「核心闭环」](interaction.md#核心闭环)第 3、4 步、[「屏幕」](interaction.md#屏幕)03 画板、[ADR 0012](../adr/0012.md)）。它只读 `dyn Review` 的快照、展示行和正文，只发 `ReviewCommand`；由 `workspace` 承载（[workspace 设计](workspace.md)）。

## 范围

- 做：
  - 审阅屏幕（`Screen("review")`）：简报、叙事、diff、浮条；检查器的 Files 页（`InspectorTab("files")`）是 Progress 列表；两者共用一个 `ReviewModel`。
  - 简报：`Brief::items` 按快照顺序（critical 在前），`omitted`、`pending_providers`、`analysis` 的状态；点一条跳到对应的文件或行；本会话改到的分支的用量行与每轮的 token（[ADR 0020](../adr/0020.md)「界面位置」）；Analyze 的触发与取消（[ADR 0019](../adr/0019.md)），结果的显示随 [#66](https://github.com/SoloJiang/lectern-app/issues/66)。
  - 叙事：每轮 prompt → agent 说明 → 改动步、命令、子 agent、其他工具调用；正文经 `Prepare(Need::Text)` 与 `text()` 按可见范围取。
  - diff：展示行经 `Prepare(Need::Rows)` 与 `rows()` 取；统一视图与双栏视图；hunk 的归因标签与 Drifted 标记；折叠的未改动行可展开；噪声文件默认折叠；归因 `Pending` 时的显示。
  - Progress：`files`、`seen`、`progress`；勾选发 `MarkSeen` / `MarkUnseen`，V 标记并跳到下一个未看文件，Looks good 发 `LooksGood`；`ChangedSinceSeen` 的提示。
  - 刷新：`ReviewEvent::Changed` / `Prepared` 到达后换快照和行，滚动位置与选中项按稳定键保留。
  - 交接：⇧⌘O 经 `dyn Reader` 打开完整文件（[file_reader 设计](file_reader.md)），diff 两侧的对象库内容经 `Reader::share_object` 登记；`CodeIntel::set_focus` 的 `Review`、`Touched`、`Current`（[code_intel 设计「接口」](code_intel.md#接口)）；Space 打开上下文面板。
  - 两侧文本的语法高亮：在 executor 上调用 `syntax` 的 `highlight::window`（[file_reader 设计「`syntax` 的公开项」](file_reader.md#syntax-的公开项)），结果按 OID 缓存。
  - 全部界面字符串的 Fluent 键、UI 探针的语义节点、具名 Action 与快捷键、向 `workspace` 登记的贡献、`DiffShown` 里程碑。
- 不做：
  - 归因、改动链、已看状态、简报规则、噪声列表、展示行与正文的计算：`review`。本 crate 不读 git、不读会话日志、不碰文件系统。
  - 窗口骨架、侧栏、工具栏容器、检查器容器、面板容器、导航栈：`workspace`。
  - 颜色、字体、动效常量、控件、语义节点的类型：`ui`（[#214](https://github.com/SoloJiang/lectern-app/issues/214) 的设计）。本 crate 不写任何颜色值、字体名和时长。
  - 完整文件阅读与编辑：`file_reader`。上下文面板的内容与符号查询：`context_drawer`（[#20](https://github.com/SoloJiang/lectern-app/issues/20)）。Conversation 页：`conversation`。
  - 分支用量的计算：`usage`；本 crate 只读 `UsageSnapshot::Session`。
- 本 issue 之外：
  - 审阅意见（[#22](https://github.com/SoloJiang/lectern-app/issues/22)）：C 加意见、⌘↩ 发送、浮条里的意见数。本文为它留出浮条的位置和 `ReviewSnapshot::notes` 的读取点；#22 之前浮条显示「没有审阅意见」、发送按钮不可用，C 与 ⌘↩ 不绑定。
  - Analyze 的结果显示（#66）：`AnalysisState::Done` / `Failed` 的内容由 #66 在 `brief` 上加字段后显示；本文只定 `Analyze` / `CancelAnalyze` 两个命令的触发、进行中与不可用的显示。
  - 提交与开 MR（Looks good 之后的下一步，[git 设计「未决问题」](git.md#未决问题)1）：P1 不做，浮条上没有对应按钮。
  - 结构化 diff（[#21](https://github.com/SoloJiang/lectern-app/issues/21)）：展示行的来源不变，本 crate 不受影响。
  - agent 说明按 Markdown 渲染：第一版按纯文本显示、保留换行（「未决问题」4）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui` | 视图、`list` 与 `ListState`（变高的虚拟列表）、Action、`cx.spawn` 的前台任务 | 已锁定（[ADR 0016](../adr/0016.md)） |
| `lectern-ui` | 颜色、`TextRole`、`MonoMetrics`、`code_style`、`motion`、`Transition`、控件（`CircleCheck`、`Progress`、`Segmented`、`Button`、`AgentDot`、`GroupedList`、`Row`、`Skeleton`、`Expander`）、`Text`、`semantic`、`contrib` 的载荷类型 | 视图层的公共库（[ADR 0015](../adr/0015.md)） |
| `lectern-i18n` | `keys::review::*`、`Args`、`relative_time`、`count_compact` | [ADR 0013](../adr/0013.md) |
| `lectern-kernel`、`lectern-services` | `Plugin`、`PluginHandle`、`ServiceRef`；使用 `dyn Contributions`、`dyn Review`、`dyn SessionSource`、`dyn Reader`、`dyn CodeIntel`、`dyn Usage` 和它们的数据类型 | 架构规则 7（[ADR 0025](../adr/0025.md)）：插件之间只经 `services` |
| `lectern-syntax` | `Languages`、`Lang`、`highlight::window`、`WindowSpans`、`Token` | 非插件的领域库，视图层可以依赖；高亮不另写一份 |
| `lectern-executor` | 高亮任务的 `Priority` | 任务经 `PluginHandle::own_with` 提交 |
| `lectern-logging` | 日志宏；测试用 `test-support` 的 `testing::capture()` | [ADR 0046](../adr/0046.md) |
| `futures`（`channel` feature） | 事件到主线程的 `mpsc::unbounded` | 已在工作区依赖（`workspace` 同样用它） |
| `lectern-kernel`（`test-support`，dev）、`insta`（dev） | `TestKernel`、语义树快照 | 工作区统一依赖 |

本 crate 是插件 crate（`[package.metadata.lectern] plugin = true`），含两个插件（见「接口」）。它不依赖 `lectern-review`、`lectern-file-reader`、`lectern-code-intel`、`lectern-usage`、`lectern-workspace`（都是插件，dev-dependency 也不行，架构规则 7），所以测试不用 `review::testing`，用本 crate 自己的假 `dyn Review`（「测试」）。不新增 Cargo.lock 里没有的外部 crate。所在层与依赖方向见 [crates/review_view/AGENTS.md](../../crates/review_view/AGENTS.md)。

## 接口

### 插件

kernel 没有可选依赖：`requires` 里的每个服务都要有活着的提供者，插件才能激活；查找不在 `requires` 里的服务让插件失败（[kernel 设计「激活时的上下文」](kernel.md#激活时的上下文)）。审阅闭环不该因为 `code-intel`、`usage` 或 `file-reader` 失败而消失，所以分成两个插件：

| 插件 ID | provides | requires | 激活 | 内容 |
|---|---|---|---|---|
| `review-view` | — | `dyn Contributions`、`dyn Review`、`dyn SessionSource` | `AfterFirstFrame` | 屏幕、检查器页、快捷键的登记；`ReviewEvent` 的监听；审阅屏幕本身 |
| `review-view-handoff` | — | `dyn Reader`、`dyn CodeIntel`、`dyn Usage` | `AfterFirstFrame` | 把三个服务的引用放进共享状态；在时交接生效，不在时屏幕照常、只少交接 |

```rust
/// `app` 装配时调用一次，两个插件都放进 `PluginSet`（id 见上表）。不做 IO。
pub fn plugins() -> (ReviewViewPlugin, HandoffPlugin);

pub struct ReviewViewPlugin { /* Arc<Shared> */ }
pub struct HandoffPlugin { /* Arc<Shared> */ }
```

- `ReviewViewPlugin::activate`（kernel 线程）：取 `ctx.service::<dyn Contributions>()`、`service::<dyn Review>()`、`service_set::<dyn SessionSource>()` 存进 `Shared`；`Contributions::register(…, None)` 三项贡献（「界面」的「向 `workspace` 登记的贡献」；`activate` 里没有 token），返回的 `Registration` 经 `ctx.own_staged` 交出；`ctx.on::<ReviewEvent>` 与 `ctx.on::<WorkspaceEvent>`（只用 `LayerChanged`）订阅。不提交任务。`started`：保存 `PluginHandle`（高亮任务用 `own_with`）。停用：`Active` drop 时清空 `Shared` 里的引用，正在显示的屏幕由 `workspace` 按 `Pop` 处理。
- `HandoffPlugin::activate`：取三个 `ServiceRef`，`started` 时放进 `Shared::handoff`；`Active` drop 时取出。屏幕每次要交接时从 `Shared::handoff` 取一份（取锁复制），`ServiceRef::get()` 为 None（提供者已停用）时按没有处理。
- 两个插件共用一个 `Arc<Shared>`（`plugins()` 建），与 `workspace` 的 `WorkspaceHost` 由 `app` 和插件共用是同一种做法。

```rust
pub(crate) struct Shared {
    contributions: Mutex<Option<ServiceRef<dyn Contributions>>>,
    review: Mutex<Option<ServiceRef<dyn Review>>>,
    sessions: Mutex<Option<ServiceSet<dyn SessionSource>>>,
    handoff: Mutex<Option<Handoff>>,
    handle: Mutex<Option<PluginHandle>>,            // review-view 这次激活的句柄
    /// 活着的审阅屏幕的主线程通道（「数据流与线程」）：`ReviewEvent` 的监听按 `ReviewId` 投递。
    screens: Mutex<Vec<(ReviewId, futures::channel::mpsc::UnboundedSender<Job>)>>,
}
pub(crate) struct Handoff {
    reader: ServiceRef<dyn Reader>,
    code_intel: ServiceRef<dyn CodeIntel>,
    usage: ServiceRef<dyn Usage>,
}
```

### 视图

```rust
/// 审阅屏幕。`Screen("review")` 的工厂对每次 `Push(Route { screen: "review", target: Target::Review(t) })` 建一个，
/// 弹出时 drop。主线程。
pub struct ReviewScreen {
    model: Entity<ReviewModel>,
    list: ListState,              // 叙事列表（gpui `list`：变高、虚拟，只排版可见项）
    focus: FocusHandle,
    mode_slider: Transition,      // 统一 / 双栏分段控件的滑块
    progress: Transition,         // 浮条里的已看进度条
    milestone_reported: bool,
}

/// 检查器 Files 页：Progress 列表。`InspectorTab("files")` 的工厂建，检查器隐藏或换页时 drop。
/// 它从 GPUI 全局 `Current`（主线程，栈顶审阅屏幕的 `WeakEntity<ReviewModel>`）取模型；没有时显示空状态。
pub struct ProgressTab { model: Option<WeakEntity<ReviewModel>>, focus: FocusHandle, selected: Option<FileKey> }

/// 屏幕与 Progress 页共用的状态，一个审阅一份。只在主线程上读写；没有 IO。
pub struct ReviewModel {
    target: ReviewTarget,
    id: ReviewId,
    opened_at: Instant,                         // `Push` 到达的时刻（里程碑与加载态按它算）
    title: Option<Arc<str>>,                    // 会话标题：`SessionSource::sessions()` 里的 `SessionSummary::title`
    snapshot: Arc<ReviewSnapshot>,              // 当前快照；`Changed` 后换成 `Review::snapshot` 的新 Arc
    items: Vec<Item>,                           // 叙事列表的项（「数据结构」）
    index: HashMap<ItemKey, u32>,               // 稳定键 → 下标，刷新后重建
    rows: HashMap<RowsKey, Arc<FileRows>>,      // 最近一次拿到的展示行，`Prepared` 后才替换
    texts: HashMap<TextKey, Arc<str>>,
    spans: SpansCache,                          // 两侧文本的高亮区间，键是 (OID, 语言的 query_version, 区间)
    shares: HashMap<RowsKey, [Option<ObjectShare>; 2]>, // `Reader::share_object` 的登记，与 `rows` 的条目同生共死
    context: HashMap<FileKey, u32>,             // 每个文件的上下文行数（默认 3，展开折叠时加大）
    mode: DiffMode,
    earlier_collapsed: bool,                    // 最近一次已看之前的轮次折叠成一行（「未决问题」3）
    noise_expanded: HashSet<FileKey>,
    selection: Option<ItemKey>,                 // J / K / 点击选中的项
    anchor: Option<(ItemKey, Pixels)>,          // 刷新前记下的 `logical_scroll_top`
    needs: Arc<[Need]>,                         // 最近一次发出的 `Prepare`
    visible: Range<u32>,                        // `ListScrollEvent::visible_range`
    usage: Option<Arc<SessionUsage>>,
    usage_interest: Option<UsageInterest>,
    marked: u32,                                // 本次打开发出的 `MarkSeen` 数（日志用）
    layer_open: bool,                           // 面板或覆盖层是否开着（`WorkspaceEvent::LayerChanged`），Esc 的归属按它判断
    // 事件通道的接收端不在这里：它由构造时 `cx.spawn` 的前台任务持有（「数据流与线程」）。
}

pub enum DiffMode { Unified, SideBySide }

/// 执行器线程、kernel 线程上产生、排到主线程的工作。
pub(crate) enum Job {
    Review(ReviewEvent),
    Layer { open: bool },
    Usage { generation: u64 },
    Spans { key: SpanKey, spans: Arc<WindowSpans> },
    SpansUnavailable { key: SpanKey },
}
```

- 构造（`Screen` 工厂，主线程，< 1 ms）：`Review::open(target)` 立即返回；建 `futures::channel::mpsc::unbounded::<Job>()`，发送端先登记进 `Shared::screens`，然后才 `snapshot()`（这个顺序让登记之前发出的 `Changed` 不会丢：之后读到的快照至少是那一代）；接收端由 `cx.spawn` 的前台任务持有：每次 `await` 到一项就把队列里已有的项一次取完、按种类只留最高的 `generation`，然后 `model.update(cx, …)` 应用。发送端的 `wake` 经 GPUI 的平台分发器唤醒主线程，任何线程都能投递，不轮询。`ReviewTarget::Session(key)` 时另取 `Handoff::usage`，`Usage::watch(UsageQuery::Session { session: key })` 的 `UsageInterest` 存进模型。构造里不等待任何事件。
- drop（`Pop`、插件停用）：`Review::close(id)`（30 秒内再进入同一会话不重算，[review 设计「接口」](review.md#接口)）；`brief.analysis` 是 `Running` 时先发 `CancelAnalyze`（`crates/review_view/AGENTS.md`「边界」）；`shares`、`usage_interest` 随模型 drop；从 `Shared::screens` 移除通道。
- 读快照、行与正文都在主线程上：`snapshot`、`rows`、`text` 只取锁复制 `Arc`（review 设计）；`command`、`open`、`close` 立即返回。本 crate 在主线程上没有 IO、解析和 git 操作。

### 本 crate 的其他公开项

```rust
/// 叙事列表的项由快照算出。纯函数，任何线程；`review_view/items_5000` 基准和 `items_*` 测试直接调它。
pub mod items {
    pub fn build(snapshot: &ReviewSnapshot, rows: &HashMap<RowsKey, Arc<FileRows>>, opts: &BuildOptions) -> Vec<Item>;
    pub fn display_rows(rows: &FileRows, mode: DiffMode) -> Vec<DisplayRow>;
}

/// 测试与 bench 用（`test-support` feature）。本 crate 不能依赖 `lectern-review`（插件之间不得依赖），
/// 所以有自己的假审阅服务：按脚本给出快照、行与正文，记下收到的命令，按测试的要求发事件。
pub mod testing {
    pub struct FakeReview;
    impl FakeReview {
        pub fn new() -> Arc<FakeReview>;
        /// 下一份快照；`emit_changed` 发 `ReviewEvent::Changed` 并让 `snapshot()` 返回它。
        pub fn set_snapshot(&self, id: ReviewId, snapshot: ReviewSnapshot);
        pub fn set_rows(&self, id: ReviewId, key: RowsKey, rows: FileRows);
        pub fn set_text(&self, id: ReviewId, key: TextKey, text: &str);
        pub fn emit_changed(&self, id: ReviewId) -> u64;
        pub fn emit_prepared(&self, id: ReviewId) -> u64;
        pub fn commands(&self) -> Vec<(ReviewId, ReviewCommand)>;
    }
    impl Review for FakeReview { /* `open` 返回递增的 ID；`rows` / `text` 只对最近一次 `Prepare` 里的键返回 Some */ }
    /// 合成的快照：`files` 个文件、`turns` 轮、每个文件 `rows_per_file` 行的 `FileRows`（纯文本 Rust 源码）。
    pub fn snapshot_fixture(files: u32, turns: u32, rows_per_file: u32) -> (ReviewSnapshot, Vec<(RowsKey, FileRows)>);
    /// `TestKernel` 上装好 `FakeReview`、假的 `Contributions`（只记登记）、假的 `SessionSource` 和两个插件。
    pub fn harness(cx: &mut TestAppContext) -> Harness;
}
```

## 数据结构

### 叙事列表的项

叙事是一个 gpui `list`：每一项是一个独立的元素，列表只排版和绘制可见的项（变高的虚拟列表，`ListState` 记每项测过的高度），所以 5000 行的 diff 不会在一帧里全部排版。

```rust
pub enum Item {
    Brief,                                   // 简报整块（最多 12 条，一项）
    Notice(Notice),                          // `BaseHow::CurrentHead`、`Degraded`、`unclaimed`、`invalid_paths` 的提示行
    EarlierTurns { count: u32 },             // 折叠的早先轮次（`earlier_collapsed`）
    Turn(TurnRef),                           // 轮次标题：序号、agent 圆点、开始时间、这一轮的 token
    Prompt(TextKey), Text(TextKey), Injected(TextKey),
    Step(StepId),                            // 改动步标题：路径、+a −r、对齐、存活、圆形勾选
    File(FileKey),                           // 规则分组视图里的文件标题（`turns` 为 None 时）
    Group(GroupKind),                        // 规则分组的组标题
    Noise { count: u32 },                    // 折叠的噪声文件一行（叙事末尾；展开后变成各自的 Step / File）
    Hunk { rows: RowsKey, hunk: u32 },       // hunk 标题行：归因标签、Drifted 标记
    Row { rows: RowsKey, ix: u32 },          // 一个显示行（`DisplayRow`，统一视图一行一个 `DiffRow`，双栏一行一对）
    Fold { rows: RowsKey, ix: u32 },         // 折叠的未改动行：「Show n lines」
    Placeholder { rows: RowsKey, lines: u32 }, // 还没有准备好的展示行：按估计的行数占高度
    Command(CallRef), Subagent { session: u16, call: ToolCallId }, Tool(CallRef),
}

/// 项的稳定键：刷新后按它找回滚动位置和选中项。`Row` 的键是两侧行号，不是下标。
pub enum ItemKey {
    Brief, Notice(NoticeKind), EarlierTurns, Turn(TurnRef), Text(TextKey), Step(StepId), File(FileKey), Group(GroupKind), Noise,
    Hunk { rows: RowsKey, old: u32, new: u32 }, Row { rows: RowsKey, old: Option<u32>, new: Option<u32> },
    Fold { rows: RowsKey, old: u32 }, Placeholder(RowsKey), Call(ToolCallId),
}

/// 一个显示行。统一视图：与 `DiffRow` 一一对应。双栏：`Removed` 与它 `pair` 指向的 `Added` 合成一行，
/// 没有配对的一侧为空；`Context`、`Folded` 两侧相同。
pub struct DisplayRow { pub left: Option<Side>, pub right: Option<Side>, pub hunk: Option<u32>, pub inline: Option<u32> /* `FileRows::inline` 的下标 */ }
pub struct Side { pub line: u32, pub byte: Option<u64>, pub kind: RowKind }
pub enum RowKind { Context, Removed, Added }
```

- 顺序（会话审阅，`turns` 为 Some）：`Brief`、`Notice*`、`EarlierTurns`（有时）、每轮 `Turn` → `Prompt` → 按 `items` 顺序的 `Text` / `Injected` / `Step`（紧跟它的 `Hunk` / `Row` / `Fold` 或 `Placeholder`）/ `Command` / `Subagent` / `Tool`；最后 `Noise`。噪声文件的 `Step` 不在轮次里出现，折叠成 `Noise` 一行，展开后按叙事顺序出现在各自的轮次里。
- 规则分组视图（`turns` 为 None：认不出格式、会话外的改动、比较范围）：`Brief`、`Notice*`、每个 `Group` → 组内 `File` 与它的行；`Noise(…)` 组折叠成 `Noise`。
- `Prompt` 在正文到达前显示 `TurnEntry::prompt_preview`，到达后换成 `texts[key]`；`Text` / `Injected` 到达前显示一块 `Skeleton`（一行高）。
- `Step` 的行用 `RowsKey::Step { step, context }`；`File` 的行用 `RowsKey::Final { file, context }`。`context` 取 `ReviewModel::context[file]`，默认 3；点 `Fold` 时把这个文件的 `context` 加大到覆盖那段折叠（上限 `u32::MAX` 表示全部），重新 `Prepare`，不自己切两侧文本。
- `Placeholder` 的高度 = 估计的行数 × `MonoMetrics::line_height`：`ChangeDetail::Text { hunks, added, removed }` 时取 `added + removed + hunks × (2 × context + 1)`；`StepEntry` 取 `added + removed + 7`；其余 1 行。行准备好后用 `ListState::splice(range, new_count)` 换成真实的项，列表总高度只在这一处变化。
- `items::build` 的代价与 `files + turns + items + 已准备的行数` 成线性，不读两侧文本；`NodeId` 随项建一次（`ui` 设计「探针」：行数多的视图把 `NodeId` 放在状态里，每帧只克隆）。

### 高亮区间的缓存

```rust
pub(crate) struct SpanKey { oid: BlobId, query_version: u64, window: Range<u64> }
pub(crate) struct SpansCache { /* HashMap<SpanKey, Arc<WindowSpans>>，LRU，合计 ≤ 2 MiB（`WindowSpans::bytes`） */ }
```

键只含内容的 OID、语言查询的版本和窗口（架构规则 5）：同一个 blob 在叙事步和最终改动里出现时只算一次；`RowsSide::oid` 为 None（中间版本不在对象库里时 review 也给出按 blob 格式算的 OID，所以只有 `SideText` 不是 `Text` 时为 None）的一侧不高亮。行 drop 时它引用的条目留在 LRU 里，超过 2 MiB 从最久没用的淘汰。

### 滚动位置与选中项

- 滚动锚点：每次 `ListScrollEvent` 记下 `ListState::logical_scroll_top()`（`ListOffset { item_ix, offset_in_item }`）对应的 `ItemKey` 和 `offset_in_item`。
- 刷新（`Changed`）：重建 `items` 与 `index`，`ListState::reset(items.len())`；锚点的键还在时 `scroll_to(ListOffset { item_ix: index[key], offset_in_item })`；不在时（文件被删、行被改）取旧 `items` 里锚点之后第一个仍存在的键，都没有时留在原来的下标（不超过新的长度）。选中项同样按键找回，找不到时清空。
- `Prepared`：只换受影响的 `Placeholder` 段（`splice`），锚点在段内时按段内的行号找回；段外的项下标按 `splice` 的差值平移，`ListState` 自己维护。
- 这两条让 5000 行的 diff 在后台每 50 ms 换一次快照（review 设计「数据流与线程」）时，用户看到的位置不动、选中的 hunk 不变。

### 可见范围与 `Prepare`

- `visible` 来自 `ListState::set_scroll_handler` 的 `ListScrollEvent::visible_range`。需要的键 = `visible` 向前、向后各扩一屏（`count` 项）里出现的 `RowsKey`（含 `Placeholder` 的）和 `TextKey`，按与可见范围的距离排序（可见的在前；review 按请求顺序保留，超出 8 MiB 时丢后面的）。
- 集合与上一次相同时不发；不同时在这一帧结束后发一次 `command(id, Prepare(needs))`（每帧至多一次）。`Prepared` 到达后对 `needs` 里的每个键调 `rows` / `text`，返回 Some 的存进 `rows` / `texts` 并替换对应的项；仍为 None 的保持 `Placeholder`。
- 离开保留范围的键：下一次 `Prepare` 不含它，review 丢弃它的行；本 crate 同时从 `rows`、`shares` 删掉，项换回 `Placeholder`（高度按已知的行数，不再估计）。`texts` 保留（正文合计 ≤ 2 MiB，review 不丢）。
- 新一轮（`Changed`）之后 review 丢掉全部展示行并按最近一次请求重新准备、随后发 `Prepared`（[review 设计「与设计的差异」](review.md#与设计的差异)第 3 步）。本 crate 在 `Changed` 时不调 `rows()`，继续显示手里的 `Arc<FileRows>`，`Prepared` 到达、`rows()` 为 Some 时才替换：画面不闪成骨架。旧行与新快照不一致的那一小段时间里，行的归因标签按新快照的 `FileAttribution` 显示、hunk 标签按旧行显示，不超过一轮准备的时间（< 3 ms，review 设计「性能」）。

### 高亮

- 时机：一份 `FileRows` 存进 `rows` 之后，对两侧 `SideText::Text` 的内容各提交一个任务（`Shared::handle` 的 `own_with`，`Priority::Interactive`，持有 `BusyGuard`）。窗口取这份行里全部 `Row` 的 `*_byte` 的最小值到最大值所在行的行尾（没有 `*_byte` 的一侧不高亮）；可见部分 `visible` 取同一区间。超过 256 KiB 的区间按 hunk 拆成多个窗口，每个不超过 256 KiB。`Rules::Json` 的语言不切窗口：内容不超过 256 KiB 时整份作为一个窗口（`pad` 为空），超过时这一侧纯文本（JSON 的窗口切点和补齐属于 `syntax` 的档位 B / C，diff 不用）。
- 任务里：`Languages::core().for_path(path)`（路径不是 UTF-8 或没有语言时不高亮）、`lang.prepare()`、`highlight::window(lang, &src, window, &JsonPad::empty(), visible, Budget { budget: 8 ms, cancel, yield_probe: None })`（`JsonPad::empty()` 是不补齐的空值，名字以 `syntax` 的实现为准）；`src` 是本 crate 的 `ArcSource(Arc<[u8]>)`（实现 `highlight::Source`，`chunk` 返回整段）。`Done` 时投递 `Job::Spans`；`TimedOut`、`Unavailable`、`Cancelled` 投递 `SpansUnavailable`，这一侧本次保持纯文本，不重试（已知限制：超过 8 ms 才能解析的窗口显示纯文本，与 file_reader 的首屏一致）。
- 行在区间到达前按纯文本画（`label` 色、`TextRole::Code`）；到达后下一帧按 `WindowSpans::line(行首字节)` 的 `Token` 用 `ui::code_style` 着色。词级差异（`FileRows::inline`）用 `Palette::diff_added_inline_background` / `diff_removed_inline_background`（「对其他 crate 的要求」F，本 crate 不算颜色），与高亮叠加。
- 取消：行被丢弃（离开保留范围、屏幕 drop）时置位任务的 `cancel`；结果到达时键不在 `rows` 里就丢弃。

### 简报

- 条目文案按 `BriefRule` 取 Fluent 键（「界面」表），`$count`、`$path`（`Args::str`，原样显示）作参数。`Provided { provider, rule, args }`：`i18n` 只有编译期的 `Key` 常量，没有按字符串查消息的接口，所以本 crate 用一张静态表把已知的（provider, rule）对映射到键（前置条件 F 的两条规则由 `code_intel` 的设计定名后加进表，`args` 里的参数按名字传入）；不在表里的用 `review-brief-provided` 显示 `rule` 原文。这与 review 设计「按 `brief-<provider>-<rule>` 取 Fluent 文案」的措辞不同，意图相同。
- 点一条：`line` 为 Some 时滚到 `Final` 行里 `(side, line)` 对应的 `Row` 并选中；否则滚到文件的 `Step`（会话审阅取 `FileAttribution::turns` 里第一轮的步）或 `File` 项。
- `omitted > 0`：末尾一行 `review-brief-omitted`；`pending_providers > 0`：标题旁一行次要文字 `review-brief-pending`。
- 用量：`usage` 为 Some 时每个 `SessionRow` 一行（`RowKey::Branch` 显示分支名，`Mrs` 显示 MR 标题，`Unassigned` 用 `review-brief-usage-unassigned`）：累计 token（`count_compact`）、本会话占比（`session_part.total() / cumulative.total()`，整数百分比）、`shared` 时注明；当前审阅的改动所在的分支排第一（`SessionRow::worktree` 等于审阅对象的 worktree 的那一行）。每轮标题显示 `SessionUsage::turns` 里这一轮的 `Tokens::total()`。
- Analyze：按 `brief.analysis`：`Unavailable(reason)` 时按钮不可用、悬停显示 `review-analyze-unavailable`（原因原样）；`Idle` 时按钮可用，点击发 `Analyze`；`Running` 时显示 `review-analyze-running` 加 Cancel（发 `CancelAnalyze`）；`Done { stale, partial }` 时显示 `review-analyze-stale` / `review-analyze-partial` 的提示和 Reanalyze；`Failed(reason)` 时显示 `review-analyze-failed` 加 Retry。结果内容随 #66。

### 归因、Drifted 与 Pending

| 情况 | hunk 标题行 | 行 |
|---|---|---|
| `FileAttribution::state == Pending` | 不显示标签（不写「计算中」：100 ms 内不显示加载态） | 照常显示 |
| `Lines`，`HunkAttribution { drifted: 0, turns, others }` | `turns` 显示「Turn n」（多轮用顿号连接），`others` 显示 `review-hunk-other-session`（n 个） | 照常 |
| `Lines`，`drifted > 0` 且 `turns`、`others` 都为空 | `review-hunk-drifted`（不显示归因，不猜） | 行的左缘用 `tertiary_label` 的细线标出 |
| `Lines`，`drifted > 0` 且有归因 | 归因加 `review-hunk-drifted-lines { $count }` | 同上 |
| `FileOnly` | 步标题显示 `FileAttribution::turns`，hunk 行不显示标签 | 照常 |
| `None`（会话外） | 不显示 | 照常 |
| `StepEntry::alignment` 为 `Partial` / `Unaligned` | 步标题加 `review-step-partial` / `review-step-unaligned`；`Unaligned` 的行按日志原文（`RowsKey::Step` 的行），hunk 标题全部为 Drifted | |

## 数据流与线程

```
project_home ↵ ─▶ Contributions::dispatch(Push(review, Target::Review(t))) ─▶ [主线程] Screen 工厂：建 ReviewModel、Review::open(t)、snapshot()=Loading
review（executor）─▶ ReviewEvent::Changed ─▶ [发出事件的线程上的监听] Shared::screens 里按 id 投递 Job ─▶ [主线程前台任务] snapshot() ─▶ items::build ─▶ reset / 锚点找回 ─▶ needs ─▶ command(Prepare)
review（executor）─▶ ReviewEvent::Prepared ─▶ Job ─▶ [主线程] rows() / text() ─▶ splice ─▶ 下一帧画出 ─▶ report_milestone(DiffShown)（一次）
                                                                   └─▶ own_with(Interactive) 高亮任务 ─▶ Job::Spans ─▶ [主线程] 下一帧着色
用户滚动 ─▶ ListScrollEvent ─▶ visible ─▶ 帧末 needs 变了就 command(Prepare)
V / 勾选 ─▶ command(MarkSeen)（review 在返回前换出 seen）─▶ 本 crate 立即按新快照重画、播放勾选动画 ─▶ 动画结束后滚到下一个未看文件
usage（executor）─▶ UsageEvent::Changed ─▶ Job::Usage ─▶ [主线程] Usage::snapshot ─▶ 简报用量行、轮次 token
⇧⌘O ─▶ Reader::open(ReadTarget)；Space ─▶ CodeIntel::set_focus(Current) + dispatch(OpenPanel("context"))
```

- 主线程上只有：读快照与行（复制 `Arc`）、`items::build`、显示行的配对、锚点与可见范围的计算、发命令、渲染。没有 IO、解析和 git 操作（架构规则 4）。
- 执行器上只有高亮任务：`highlight::window` 是阻塞调用，只在 `own_with` 提交的任务里运行；任务持有 `BusyGuard`，结果投递后释放，kernel 的静止不会漏掉它。
- 事件的监听在发出事件的线程（executor）上运行，只做转发：取 `Shared::screens` 的锁、按 `ReviewId` 找通道、`unbounded_send`，不做别的。`UsageEvent` 由 `review-view-handoff` 订阅（它才 requires `dyn Usage`），同样只转发 `Job::Usage`。
- 事件的合并：前台任务每次醒来把通道里的项全部取出：`Review(Changed)` 只应用最高的 `generation`（低的丢弃）；`Prepared` 合并成一次；`Spans` 逐条应用。应用之后 `cx.notify()` 一次。
- 派生数据：高亮区间以（OID、查询版本、窗口）为键（架构规则 5）；其余都从快照和行直接算，不缓存。
- 首帧之前什么都不做：两个插件 `AfterFirstFrame` 激活，`activate` 只登记（[ADR 0017](../adr/0017.md)）。没有审阅屏幕时本 crate 没有任务、没有通道，`Shared` 只有几个空的 `Mutex`。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)、[bench 设计「预算表」](bench.md#预算表)）：

| 预算 | 本 crate 的份额 |
|---|---|
| 打开一个会话的 Diff < 50 ms（`app/open_diff`：项目主页 ↵ 到第一个文件的 diff 行画出，终点 `Milestone::DiffShown`） | review 的份额 < 35 ms（冷）加第一个文件的行 < 3 ms（review 设计「性能」）。本 crate ≤ 11 ms：工厂建模型与 `open` < 1 ms；`Changed` 投递到主线程 < 1 ms；`snapshot()` 加 `items::build`（50 个文件、20 轮、第一个文件 300 行）< 2 ms；`Prepare` 的发出在同一次更新里；`Prepared` 到达后 `rows()` 与 `splice` < 0.5 ms；画出第一屏（约 60 行等宽文本，纯文本）≤ 6 ms。高亮不在这条路径上：行先按纯文本画出并报告里程碑，区间之后到达 |
| 滚动与输入 120 fps（5000 行的 diff） | `list` 只排版可见的项（约 60 行）；每项的元素构造是常数：`NodeId` 克隆、`Token` 切片、固定行高。每帧 ≤ 8.3 ms（`review_view/scroll_5000`）。滚动期间 `Prepare` 每帧至多一次，命令立即返回 |
| 勾选 | `command(MarkSeen)` 返回前 review 已换出 `seen`（< 0.2 ms）；本 crate 只重画 Progress 行和步标题，勾选动画只改透明度与描边进度（`CHECK_FILL`、`CHECK_STROKE`） |
| 刷新 | `Changed` 到新画面：`items::build` < 2 ms（5000 行时 < 4 ms），锚点找回 O(1)；不等 `Prepared` |
| 空闲内存（采样状态，没有打开审阅） | 本 crate 没有屏幕时 < 16 KiB（`Shared` 与注册的工厂）。屏幕在 `Pop` 时 drop，不保留 |
| 审阅打开时的内存 | 展示行由 review 保留（≤ 8 MiB，只为 `Prepare` 的范围）；本 crate 另有：`items`（5000 行约 5000 × 40 B = 200 KiB）、显示行的配对（双栏时同量级）、高亮区间 ≤ 2 MiB（LRU）、`texts` 的引用（内容由 review 持有）。合计 ≤ 3 MiB 之外不持有两侧文本的副本（`ArcSource` 只借 review 给的 `Arc<[u8]>`） |
| 冷启动 20 ms | 首帧之前没有代码运行；`plugin/review-view/activate`、`plugin/review-view-handoff/activate` 各 < 1 ms（只登记） |

基准（`bench`，macOS 判定）：

| ID | 统计量与预算 |
|---|---|
| `app/open_diff` | 中位数 < 50 ms（bench 设计已列出，归本 crate）：`Driver::open(OpenTarget::Session)` 到 `DiffShown`；夹具与 `review/open_session_50` 相同的仓库与会话，缓存全空 |
| `review_view/items_5000` | 中位数 < 4 ms（`items::build` 对 `testing::snapshot_fixture(50, 20, 100)` 全部行已准备；进程内，不依赖窗口） |
| `review_view/scroll_5000` | 每帧工作时间 ≤ 8.3 ms（p95）：`WorkspaceHost::record_frames` 在滚过 5000 行的场景上记录（bench 的 `frames` 应答，随 `workspace` 第 3 步接入） |
| `plugin/review-view/activate`、`plugin/review-view-handoff/activate`、`plugin/*/resident` | 随 bench 的插件基准自动覆盖 |

## 界面

- 屏幕与层：审阅屏幕占内容区，`ScreenLayout { sidebar: Hidden, back_button: true, reading_column: true, inspector: true, floating_bar: true }`（[workspace 设计「数据结构」](workspace.md#数据结构)的区域表）。常驻层：简报、叙事、浮条；按需层：检查器的 Files 页（Progress）与 Conversation 页、上下文面板；收起层经 ⇧⌘O 进入阅读视图。双栏视图需要整个内容区（「对其他 crate 的要求」A）。
- 工具栏（`ui::contrib::Screen::toolbar`）：标题是会话标题（`SessionSummary::title`，没有时 `review-title-untitled`；`Outside` 为 `review-title-outside`，`Range` 为 `review-title-range`），副标题是 `review-progress`（`已看 2/6`）；右侧控件至多 3 个：统一 / 双栏的 `Segmented`、Refresh（发 `Refresh`）、检查器开关由 `workspace` 画。
- `ui` 的控件与动效原语：颜色经 `colors(window, cx)`，文字经 `Text` 与 `TextRole`，代码行 `TextRole::Code` 加 `mono_metrics` 的格宽与行高，`code_style` 着色；`CircleCheck`（步标题与 Progress 行）、`Progress`（浮条，`ProgressShape::Bar`）、`Segmented`、`Button`、`AgentDot`（轮次标题与子 agent 行）、`GroupedList` 与 `Row`（Progress 列表）、`Skeleton`、`Expander`（展开 `Fold` 时下方内容下移）；动效：`CHECK_FILL`、`CHECK_STROKE`、`PROGRESS`、`SCROLL_NEXT`（V 之后的滚动，`ListState::scroll_to` 的目标由 `Transition` 逐帧给出）、`ARRIVE`（新一轮到达时 `Turn` 项从下方 8pt 淡入）、`SEGMENT`、`HOVER_IN` / `HOVER_OUT`、`EXPAND_HEIGHT` / `EXPAND_FADE`；减弱动态效果由 `Motion::effective` 处理。进入会话的 `ENTER_SESSION` 弹簧动画由 `project_home` 的行标题在 `Push` 前后完成（它拥有那段文字），本 crate 的内容按 `ARRIVE` 淡入。本 crate 不写任何颜色值、字体和时长（验收用 `rg` 检查）。
- diff 行：增删行用 `diff_added_background` / `diff_removed_background` 的淡底色加 `+` / `−` 符号（[交互规范「视觉」](interaction.md#视觉)），行号栏 `secondary_label`，词级差异叠加 `diff_*_inline_background`；双栏时左右各占一半，分隔线 0.5pt `separator`，超出宽度的行在行内裁剪、整个文件可横向滚动（同一个文件的行共用一个横向偏移）。
- 加载：`ui::loading_phase(now - opened_at)`：`Nothing` 时内容区空白（工具栏已显示标题）；`Skeleton` 时简报与前两轮的位置各一块 `Skeleton`；`Progress` 时一行 `review-loading` 加取消（`dispatch(Pop)`）。
- Fluent 键（`crates/i18n/locales/{en,zh-CN}/review.ftl`，前 5 个已存在）：

  | 键 | en | zh-CN | 用途 |
  |---|---|---|---|
  | `review-looks-good`（`.tooltip`） | Looks good（Mark every file as reviewed） | 看起来没问题（将所有文件标记为已看） | 浮条按钮 |
  | `review-progress` | { $reviewed }/{ $total } reviewed | 已看 { $reviewed }/{ $total } | 副标题、浮条、Progress 标题 |
  | `review-files-changed` | { $count } file(s) changed | { $count } 个文件有改动 | 规则分组视图的标题 |
  | `review-comments` | No comments / { $count } comment(s) | 没有审阅意见 / { $count } 条审阅意见 | 浮条（#22 之前总是 0） |
  | `review-reviewing` | Reviewing { $path } | 正在审阅 { $path } | Progress 列表选中行的辅助文字 |
  | `review-title-untitled` | Untitled session | 未命名会话 | 工具栏标题 |
  | `review-title-outside` | Outside any session | 会话外的改动 | 工具栏标题 |
  | `review-title-range` | Changes against { $base } | 相对 { $base } 的改动 | 工具栏标题（`$base` 是引用名或提交，原样） |
  | `review-send` | Send | 发送 | 浮条按钮（#22 之前不可用） |
  | `review-all-reviewed` | All reviewed | 全部已看 | 浮条，`progress.seen == total` 时 |
  | `review-mode-unified` / `review-mode-side-by-side` | Unified / Side by side | 统一 / 双栏 | 分段控件 |
  | `review-refresh`（`.tooltip`） | Refresh（Compare the working tree again） | 刷新（重新比较工作区） | 工具栏控件 |
  | `review-loading` | Preparing the review… | 正在准备审阅… | 超过 1 s 的加载态 |
  | `review-cancel` | Cancel | 取消 | 加载态、Analyze |
  | `review-brief-title` | Worth a look first | 值得先看 | 简报标题 |
  | `review-brief-empty` | Nothing stands out. | 没有需要特别注意的地方。 | 简报为空 |
  | `review-brief-omitted` | { $count } more | 还有 { $count } 条 | `omitted` |
  | `review-brief-pending` | Still gathering… | 还在补充… | `pending_providers` |
  | `review-brief-changed-since-seen` | { $path } changed after you reviewed it | { $path } 在你看过之后又被改过 | `ChangedSinceSeen` |
  | `review-brief-drifted` | { $path } has changes Lectern can't attribute | { $path } 有无法对齐的改动 | `Drifted` |
  | `review-brief-edit-failed` | An edit to { $path } failed; the agent may think it applied | 对 { $path } 的一次改动失败了，agent 可能以为已经改上 | `EditFailed` |
  | `review-brief-command-only` | { $count } file(s) changed by a command, not by an edit | { $count } 个文件由命令改动，不是由编辑 | `CommandOnly` |
  | `review-brief-sensitive-agent-instructions` | { $path } is loaded by agents as instructions | { $path } 会被 agent 当作指令加载 | `Sensitive(AgentInstructions)` |
  | `review-brief-sensitive-ci` | { $path } runs in CI | { $path } 会在 CI 里运行 | `Sensitive(Ci)` |
  | `review-brief-sensitive-hooks` | { $path } runs on commit | { $path } 会在提交时运行 | `Sensitive(Hooks)` |
  | `review-brief-sensitive-git-metadata` | { $path } changes how git treats files | { $path } 改变 git 对文件的处理 | `Sensitive(GitMetadata)` |
  | `review-brief-sensitive-symlink` | { $path } is a new or retargeted symbolic link | { $path } 是新增或改了目标的符号链接 | `Sensitive(Symlink)` |
  | `review-brief-sensitive-executable` | { $path } became executable | { $path } 变成了可执行文件 | `Sensitive(Executable)` |
  | `review-brief-sensitive-submodule` | { $path } points to another commit | { $path } 指向了另一个提交 | `Sensitive(Submodule)` |
  | `review-brief-deleted` | { $count } tracked file(s) deleted | 删除了 { $count } 个受跟踪的文件 | `Deleted` |
  | `review-brief-incomplete-truncated` / `-too-large` / `-binary` / `-not-retained` / `-filtered` / `-unavailable` / `-listed` | { $count } file(s) shown without a full diff: truncated / too large / binary / content not kept / filtered by git / unreadable / listed only | { $count } 个文件没有完整的 diff：已截断 / 过大 / 二进制 / 内容未保留 / 经 git 过滤 / 无法读取 / 只列出 | `Incomplete(…)` |
  | `review-brief-noise-lockfile` / `-generated` / `-snapshot` | { $count } lockfile(s) / generated file(s) / snapshot(s) folded | 已折叠 { $count } 个 lockfile / 生成文件 / 快照 | `Noise(…)` |
  | `review-brief-provided` | { $rule } | { $rule } | 静态表里没有的提供者规则（`$rule` 原样） |
  | `review-brief-usage-row` | { $branch } · { $tokens } tokens · { $percent }% this session | { $branch } · { $tokens } token · 本会话占 { $percent }% | 用量行 |
  | `review-brief-usage-shared` | shared with { $count } other MR(s) | 与另外 { $count } 个 MR 共用 | `shared` |
  | `review-brief-usage-unassigned` | Not on any branch | 不属于任何分支 | `RowKey::Unassigned` |
  | `review-analyze` | Analyze | 分析 | 按钮 |
  | `review-analyze-unavailable` | Analyze isn't available: { $reason } | 无法分析：{ $reason } | 悬停（原因原样） |
  | `review-analyze-running` | Analyzing… | 正在分析… | `Running` |
  | `review-analyze-stale` | Based on an earlier version | 基于较早的版本 | `Done { stale: true }` |
  | `review-analyze-partial` | Some content wasn't provided to the analysis | 部分内容未提供给分析 | `Done { partial: true }` |
  | `review-analyze-failed` | Analysis failed: { $reason } | 分析失败：{ $reason } | `Failed` |
  | `review-analyze-again` | Analyze again | 重新分析 | `Done`、`Failed` |
  | `review-notice-base-current-head` | No reflog: the base may not be where this session started. | 没有 reflog：基准可能不是会话开始时的提交。 | `BaseHow::CurrentHead` |
  | `review-notice-unrecognized` | Lectern can't read this session's log; changes are shown by file. | Lectern 无法读取这个会话的日志，改动按文件显示。 | `Degraded::UnrecognizedSession` |
  | `review-notice-folder` | This folder isn't a git repository: only the recorded edits are shown. | 这个文件夹不是 git 仓库，只显示记录到的改动。 | `Degraded::FolderProject` |
  | `review-notice-unclaimed` | { $count } more file(s) changed by other sessions or by hand | 另有 { $count } 个文件由其他会话或手工改动 | `unclaimed` |
  | `review-notice-invalid-paths` | { $count } path(s) couldn't be read | { $count } 个路径无效，未读取 | `totals.invalid_paths` |
  | `review-earlier-turns` | { $count } earlier turn(s) | 之前的 { $count } 轮 | `EarlierTurns` |
  | `review-turn` | Turn { $ordinal } | 第 { $ordinal } 轮 | 轮次标题 |
  | `review-turn-tokens` | { $tokens } tokens | { $tokens } token | 轮次标题的 token |
  | `review-turn-running` | Running… | 进行中… | `ended_at` 为 None |
  | `review-step-counts` | +{ $added } −{ $removed } | +{ $added } −{ $removed } | 步标题 |
  | `review-step-partial` | Part of this edit didn't apply | 这次改动有一部分没有应用上 | `Alignment::Partial` |
  | `review-step-unaligned` | Shown as recorded; it doesn't match the working tree | 按记录显示，与工作区对不上 | `Alignment::Unaligned` |
  | `review-step-survival-partly` / `-none` | Partly overwritten since / Overwritten since | 之后被部分覆盖 / 之后被覆盖 | `Survival` |
  | `review-hunk-turn` | Turn { $ordinal } | 第 { $ordinal } 轮 | hunk 归因 |
  | `review-hunk-other-session` | { $count } other session(s) | 另外 { $count } 个会话 | `others` |
  | `review-hunk-drifted` | Drifted | 已漂移 | 无归因 |
  | `review-hunk-drifted-lines` | { $count } drifted line(s) | { $count } 行已漂移 | 有归因 |
  | `review-fold` | Show { $count } unchanged line(s) | 显示 { $count } 行未改动的内容 | `Fold` |
  | `review-noise-folded` | { $count } lockfile(s), generated file(s) and snapshot(s) folded | 已折叠 { $count } 个 lockfile、生成文件和快照 | `Noise` |
  | `review-noise-show` / `review-noise-hide` | Show / Hide | 显示 / 隐藏 | `Noise` 的按钮 |
  | `review-command` | Ran { $command } | 运行了 { $command } | `Command`（命令原样） |
  | `review-command-files` | { $count } file(s) changed while it ran | 运行期间改动了 { $count } 个文件 | `Command` |
  | `review-subagent` | Spawned a subagent | 派出了子 agent | `Subagent` |
  | `review-tool` | { $name } | { $name } | `Tool`（工具名原样） |
  | `review-tool-failed` | { $name } failed | { $name } 失败 | `ToolStatus` 不是完成 |
  | `review-injected` | Added to the conversation | 注入的内容 | `Injected` |
  | `review-mark-seen`（`.tooltip`） | Mark as reviewed (V) | 标记为已看（V） | 圆形勾选 |
  | `review-mark-unseen`（`.tooltip`） | Mark as not reviewed | 标记为未看 | 圆形勾选 |
  | `review-seen-changed` | Changed { $when } after you reviewed it | 看过之后在{ $when }又被改过 | `ChangedSinceSeen`（`$when` 是 `relative_time`） |
  | `review-progress-title` | Progress | 进度 | 检查器页的标题（`InspectorTab::label`） |
  | `review-progress-empty` | No review open | 没有打开的审阅 | 没有审阅屏幕时 |
  | `review-progress-noise` | Folded files | 已折叠的文件 | Progress 里噪声组的标题 |
  | `review-failed-missing` | This session or its worktree no longer exists. | 这个会话或它的 worktree 已不存在。 | `ReviewError::Missing` |
  | `review-failed-not-in-project` | This session doesn't belong to an open project. | 这个会话不属于已打开的项目。 | `NotInOpenProject` |
  | `review-failed-changes` | Lectern can't compare this worktree: { $reason } | Lectern 无法比较这个 worktree：{ $reason } | `Changes(…)`（`$reason` 是错误种类的键的文字） |
  | `review-error-unsupported` / `-no-merge-base` / `-too-many-entries` / `-io` / `-cancelled` | unsupported repository format / no merge base / too many entries / read error / cancelled | 仓库格式不支持 / 没有 merge-base / 条目过多 / 读取出错 / 已取消 | `ChangeError` 的种类，作 `$reason` |
  | `review-open-file`（`.tooltip`） | Open file (⇧⌘O) | 打开文件（⇧⌘O） | 步标题的按钮 |
  | `review-back` | Back | 返回 | 失败态的按钮 |

  路径、命令、工具名、分支名、会话标题、prompt 与说明都经 `Text::verbatim` 或 `Args::str` 原样显示，不翻译（ADR 0013）。复数的 en 写法在 .ftl 里用 `{ $count -> [one] … *[other] … }`，表里简写为 `(s)`。

- UI 探针的语义节点（`lectern_ui::semantic`；`workspace.content.screen[review]` 之下；窗口根由探针合成）：

  | ID | `Role` | `Label` / `State` / `attrs` |
  |---|---|---|
  | `review.brief` | `Group` | `attrs`：`items`、`omitted`、`pending`、`analysis`（`AnalysisState` 的判别名） |
  | `review.brief.item[<rule>:<path>]`（合成条目没有路径时 `<rule>`） | `ListItem` | `Label::key` 是规则的键；`attrs`：`critical`、`line` |
  | `review.brief.usage[<branch>]` | `ListItem` | `attrs`：`tokens`、`percent`、`shared` |
  | `review.brief.analyze` | `Button` | `disabled`；`attrs`：`state` |
  | `review.notice[<kind>]` | `Text` | `Label::key` |
  | `review.mode` / `review.mode.tab[unified]` / `[side-by-side]` | `TabBar` / `Tab` | `selected` |
  | `review.narrative` | `List` | `attrs`：`items`（总数）、`visible`（可见范围）、`mode` |
  | `review.earlier` | `Group` | `expanded` |
  | `review.turn[<session>:<ordinal>]` | `Group` | `Label::text` 是轮次标题；`attrs`：`tokens`、`after_last_seen`、`running` |
  | `review.turn[…].prompt`、`.text[<pos>]`、`.injected[<pos>]` | `Text` | `Label::text` 是正文（探针关闭时不构造） |
  | `review.step[<id>]` | `Group` | `Label::text` 是路径；`attrs`：`added`、`removed`、`alignment`、`survival`、`seen` |
  | `review.step[<id>].check`、`review.file[<path>].check` | `Toggle` | `checked` = 已看 |
  | `review.step[<id>].open` | `Button` | ⇧⌘O |
  | `review.group[<kind>]`、`review.file[<path>]` | `Group` | 规则分组视图；`attrs`：`seen`、`attribution`、`drifted_hunks` |
  | `review.noise` | `Group` | `expanded`；`attrs`：`count` |
  | `review.diff[<rows-key>]` | `Diff` | `attrs`：`rows`、`context`、`prepared`、`highlighted`（两侧各 `plain` / `spans`） |
  | `review.diff[…].hunk[<old>:<new>]` | `DiffHunk` | `attrs`：`turns`、`others`、`drifted` |
  | `review.diff[…].row[<old>:<new>]`（只导出可见的） | `DiffLine` | `attrs`：`kind`、`drifted`；`selected` |
  | `review.diff[…].fold[<old>]` | `Button` | `Label::key` = `review-fold` |
  | `review.diff[…].placeholder` | `Group` | `attrs`：`lines` |
  | `review.call[<id>]` | `Text` | `attrs`：`kind`（`command` / `subagent` / `tool`）、`files`、`status` |
  | `review.progress` | `List` | `attrs`：`total`、`seen`、`changed` |
  | `review.progress.item[<path>]` | `ListItem` | `Label::text` 是路径；`selected`；`attrs`：`seen`（`unseen` / `seen` / `changed`）、`group`、`noise` |
  | `review.progress.item[<path>].check` | `Toggle` | `checked` |
  | `review.bar` | `Group` | 浮条（在 `workspace.bar` 之下） |
  | `review.bar.progress` | `Progress` | `fraction` |
  | `review.bar.comments` | `Text` | `Label::key` = `review-comments` |
  | `review.bar.looks-good`、`review.bar.send` | `Button` | `disabled` |
  | `review.failed` | `Text` | `Label::key` 是失败的键 |
  | `review.loading` | `Group` | `attrs`：`phase` |

  `<rows-key>` 写成 `final:<path>` 或 `step:<id>`；`<path>` 是 `FileKey` 的新路径（删除时旧路径）。`NodeId` 随 `Item` 建一次放在项里，每帧只克隆。
- Action 与快捷键（KeyContext：屏幕根元素 `review`，Progress 页 `review-progress`；`KeyBindings` 贡献的谓词写 `Content > review` 与 `Inspector > review-progress`）：

  | Action | 键 | 上下文 | 处理 |
  |---|---|---|---|
  | `review::NextHunk` / `PreviousHunk` | J / K | `review` | 选中下一个 / 上一个 `Hunk`（跨文件、跨轮次），`scroll_to_reveal_item`，并 `set_focus(Current)` |
  | `review::MarkSeenAndNext` | V | `review`、`review-progress` | 对当前文件（选中项所在的，没有时可见范围里第一个步）发 `MarkSeen`；勾选动画结束后按 `SCROLL_NEXT` 滚到叙事里下一个 `Unseen` 文件的步标题并选中；没有时停在原处 |
  | `review::ToggleSeen` | — | 同上 | 已看时 `MarkUnseen`，否则 `MarkSeen`（圆形勾选的点击） |
  | `review::LooksGood` | — | `review` | `LooksGood`（浮条按钮） |
  | `review::OpenFile` | ⇧⌘O | `review` | 选中行所在的一侧（增删行取它那一侧，上下文行取新一侧）：`Reader::open(ReadTarget { origin: Worktree / Object, expect: RowsSide::oid, at: Anchor::Byte { byte, line, column: 0 } })`；`*_byte` 为 None 时 `Anchor::Line`；没有选中行时打开当前步的文件开头。没有 `Handoff` 时不可用 |
  | `review::Edit` | ⌘E | `review` | 同 `OpenFile`（进入编辑由 `file_reader` 的 ⌘E 完成；`Reader::open` 没有「直接编辑」的参数，记为已知限制） |
  | `review::OpenContext` | Space | `review` | 选中行有符号时 `set_focus(Current(TextRef))` 并 `dispatch(OpenPanel("context"))`；符号的位置如何交给面板由 #20 的设计定 |
  | `review::ToggleDiffMode` | — | `review` | 统一 ↔ 双栏（分段控件） |
  | `review::ExpandFold` | ↵（选中 `Fold` 时） | `review` | 加大这个文件的 `context`，重新 `Prepare` |
  | `review::ToggleNoise` | — | `review` | 展开 / 折叠噪声文件 |
  | `review::ToggleEarlierTurns` | — | `review` | 展开 / 折叠早先的轮次 |
  | `review::Refresh` | — | `review` | `ReviewCommand::Refresh` |
  | `review::Analyze` / `CancelAnalyze` | — | `review` | 对应命令 |
  | `review::Escape` | Esc | `review` | 面板或覆盖层开着时（`WorkspaceEvent::LayerChanged` 记在模型的 `layer_open` 里）不处理，Esc 冒泡到 `workspace` 先关面板；没有开着的层而有选中项时清空选中并停止传播；否则不处理，冒泡到 `workspace`（返回） |
  | `review::SelectNext` / `SelectPrevious` / `JumpToSelected` | ↓ / ↑ / ↵ | `review-progress` | Progress 列表的选择与跳转（跳到那个文件的步标题） |
  | `review::Back` | — | `review` | 失败态的返回按钮：`dispatch(Pop)` |

  C（加审阅意见）与 ⌘↩（发送）随 #22；⌥⌘I、⌘\、⌘[、⌘O、⌘W 由 `workspace` 处理。全部 Action 名稳定、可枚举（探针按名字触发）。

  `set_focus(project, worktree, focus)` 的 `project`、`worktree`：`ReviewTarget::Outside` / `Range` 取自己的字段；`Session` 取 `SessionSummary::place` 为 `Located` 时的项目与 worktree（`sessions()` 里查不到时不调用）。`Review(focus_review)`、`Touched(focus_touched)` 在 `Ready` 后发一次，之后快照里这两个 `Arc` 变了再发；`Current(Some(TextRef { worktree, path, oid: RowsSide::oid, origin: Worktree / Object }))` 在选中行变化时发；清空选中不发 `Current(None)`，只在屏幕 drop 时发一次，所以面板开着期间焦点不变。
- 向 `workspace` 登记的贡献（[workspace 设计「接口」](workspace.md#接口)）：

  | `Contribution` | `id` | `order` | 载荷 |
  |---|---|---|---|
  | `Screen(ScreenLayout { sidebar: Hidden, back_button: true, reading_column: true, inspector: true, floating_bar: true })` | `review`（`screens::REVIEW`） | 0 | `ui::contrib::Screen { build, toolbar }`：`build` 对 `Target::Review(t)` 建 `ReviewScreen`，其他 `Target` 记 warn、返回只显示 `review-failed-missing` 的视图；`toolbar` 每帧给标题、副标题和 ≤ 2 个控件 |
  | `InspectorTab` | `files` | 0 | `ui::contrib::InspectorTab { label: review-progress-title, build }`：建 `ProgressTab` |
  | `KeyBindings` | `review` | 0 | 上表里有键的 Action |

  浮条由本 crate 在屏幕视图里画（`workspace` 只留出区域）；Space 的面板、Esc 与 ⌘[ 的返回经 `Contributions::dispatch`。`DiffShown`：第一次画出已准备的行的那一帧，在 `on_next_frame` 回调里 `report_milestone(DiffShown)`，每个屏幕一次。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)和 [review 设计「状态与异常」](review.md#状态与异常)：

| 情况 | 表现 |
|---|---|
| `LoadState::Loading` | 按 `loading_phase`：100 ms 内空白，之后骨架，1 s 后 `review-loading` 与取消；`Ready` 到达时骨架换成内容（`ARRIVE`） |
| `Failed(Missing)`、`Failed(NotInOpenProject)`、`Failed(Changes(e))` | 内容区一行说明（对应的 `review-failed-*`，`Changes` 带错误种类）和 `review-back`；浮条隐藏；记 warn |
| `Degraded::UnrecognizedSession` | 规则分组视图；`review-notice-unrecognized`；没有轮次、说明和归因标签 |
| `Degraded::FolderProject` | 只有轮次与按日志原文的改动（`Tool` 行）；`review-notice-folder`；Progress 页显示 `review-progress-empty`；浮条只有意见数 |
| `BaseHow::CurrentHead` | `review-notice-base-current-head` |
| `unclaimed > 0`、`invalid_paths` 非空 | 对应的 `Notice` 行 |
| 归因 `Pending` | 不显示标签（见「归因、Drifted 与 Pending」） |
| Drifted | 不显示归因，不做猜测；左缘细线 |
| `ChangedSinceSeen` | Progress 行与步标题的圆形勾选为空心带点，次要文字 `review-seen-changed`；简报有对应条目 |
| `SideText` 不是 `Text`（二进制、过大、未保留、过滤、不可用） | 这个文件没有行，步标题下一行 `review-brief-incomplete-*` 同一文案的单数形式；不高亮 |
| `rows()` 在 `Prepared` 之后仍为 None（请求超出 review 的 8 MiB 上限） | 保持 `Placeholder`；离开可见范围的键在下一次 `Prepare` 里排到后面，可见的排前面，所以可见部分总能准备到 |
| 高亮超时、grammar 不可用、路径不是 UTF-8 | 这一侧纯文本，不提示 |
| `Handoff` 不在（插件没装、失败或停用） | ⇧⌘O、⌘E 不可用（按钮置灰、悬停无说明）；没有用量行与轮次 token；不调用 `set_focus`；`share_object` 不登记 |
| `SessionSource::sessions()` 里找不到会话 | 标题 `review-title-untitled` |
| `AnalysisState::Unavailable` | Analyze 置灰，悬停显示原因（#66 之前总是这样） |
| 插件停用时屏幕正在显示 | `workspace` 按 `Pop` 处理；模型 drop 时 `close` 与 `CancelAnalyze` 照常发出（`ServiceRef::get` 取不到时跳过） |
| 事件的 `generation` 低于已应用的 | 丢弃 |
| 快照里的 `TurnRef::session` 超出 `sessions` 范围等不一致（不应发生） | 这一项显示为 `Tool` 行，记 warn，不 panic |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `review_view`。主线程上的记录带调用线程已有的作用域（`workspace` 调用视图工厂时恢复本插件的 `plugin`、`act`，[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 11 项）；高亮任务经 `own_with` 提交，executor 带上 `task`。本 crate 的记录带 `id`（`ReviewId` 的数值；`review` 是 `Ctx` 的字段名，宏里不能用作字段）：与 `review` crate 的 `review ready` 等记录的 `id` 相同，所以两边的记录在同一次运行里能对应。

- 默认记录：

  | 事件 | 级别 | `msg` | 字段 |
  |---|---|---|---|
  | 屏幕建好、`open` 已发出 | info | `review screen opened` | `id`、`target`（`session` / `outside` / `range`） |
  | 第一个文件的行画出（里程碑） | info | `diff shown` | `id`、`took_us`（从 `Push` 起）、`files`、`turns`、`rows`（第一屏的行数）、`generation` |
  | 屏幕 drop | info | `review screen closed` | `id`、`marked`（发出的 `MarkSeen` 数）、`looks_good`（bool）、`took_us` |
  | 快照为 `Failed` | warn | `review failed shown` | `id`、`error`（`missing` / `not_in_open_project` / 改动集错误的种类） |
  | `Prepared` 之后请求的键仍没有行或正文 | warn | `prepared keys missing` | `id`、`rows`、`texts`（缺的个数） |
  | 快照内部不一致（下标越界） | warn | `snapshot inconsistent` | `id`、`generation`、`kind`（`turn_ref` / `step` / `file_index`） |
  | 工厂收到的 `Target` 不是 `Review` | warn | `screen built without a review target` | `target`（判别名） |
  | 高亮任务 `Unavailable` | warn | `highlight unavailable` | `id`、`bytes`（本次运行每种语言只记一次） |
  | `Handoff` 不在而用户触发交接 | info | `handoff unavailable` | `id`、`what`（`open_file` / `focus` / `share` / `usage`） |
- 详细级别（debug）：`snapshot applied`（`id`、`generation`、`state`、`items`、`files`、`turns`、`anchor_found`（bool）、`took_us`）；`prepare sent`（`id`、`rows`、`texts`、`visible`（范围长度））；`prepared applied`（`id`、`rows`、`texts`、`spliced`（换掉的项数））；`highlight`（`id`、`bytes`、`outcome`（`done` / `timed_out` / `cancelled`）、`took_us`）；`focus set`（`id`、`kind`、`paths`）；`usage applied`（`id`、`rows`、`turns`）；`command`（`id`、`kind`：`ReviewCommand` 的判别名）。trace 不另记；渲染路径（每帧的 `render`、滚动事件）上不加记录。
- 关联 ID：`plugin`、`act` 由 `workspace` 在调用工厂和 Action 处理时恢复，`task` 由 executor 填。本 crate 不设 `review`、`session` 作用域（`review` 的哈希由 `review` crate 定义和计算，会话标识由它的记录带，本 crate 不重复），用 `id` 字段对应。
- 不记录：文件路径与 `FileKey`、会话标题、prompt 与 agent 说明、注入内容、命令文本、工具名、分支名、diff 的行与两侧文本、简报条目的文字与 `Provided` 的 `args`、`Analyze` 的原因文字、`Reader` 的目标路径、`NodeId` 的业务键。用计数、下标、`ReviewId`、`generation`、判别名和耗时代替。
- 金丝雀测试：`logs_hold_no_contents`（`src/tests/logs.rs`，`lectern_logging::testing::capture()` 在 trace 级别）：用含金丝雀的路径、会话标题、prompt、说明、命令、工具名、分支名、两侧文本、`Provided` 的 `args`、`Unavailable` 的原因构造快照与行，经 `FakeReview` 打开屏幕、应用快照、`Prepare`、`Prepared`、V、失败态（第 1 步的流程），第 2 步再加上高亮、⇧⌘O（假 `Reader`）、Space 和用量行，`assert_absent(金丝雀)`，并检查 `review screen opened`、`diff shown`、`review screen closed` 的字段。

## 测试

依赖 GPUI，只在 macOS 上编译和测试：`just test -p lectern-review-view`。不用真实的 git、会话扫描、文件 IO；`dyn Review` 用 `testing::FakeReview`（本 crate 不能依赖 `lectern-review` 的 `testing`，架构规则 7），`dyn Contributions`、`dyn SessionSource`、`dyn Reader`、`dyn CodeIntel`、`dyn Usage` 用只记调用的假实现，装在 `TestKernel` 上（`testing::harness`）。

- 纯函数（`#[test]`，不依赖窗口）：
  - `items_*`：会话审阅的顺序（简报、提示、轮次、步与行、命令、子 agent、噪声在末尾）；规则分组视图的顺序；`Placeholder` 的高度估计；`display_rows` 统一与双栏的配对（`pair` 为 None 的行单侧）；`ItemKey` 与下标的映射；`items_snapshot`（`snapshot_fixture(3, 2, 20)` 的 insta 快照）。
  - `anchor_*`：刷新后锚点的键还在、不在（取之后第一个存在的）、都不在；选中项的找回；`Prepared` 的 `splice` 之后锚点在段内、段外。
  - `needs_*`：可见范围前后各一屏；可见的排在前面；集合相同不重发；离开范围的键从 `rows`、`shares` 删掉。
  - `brief_*`：每条规则的键与参数；`Provided` 的静态表命中与回退；用量行的排序与百分比；`omitted`、`pending`。
  - `highlight_window_*`：窗口取行的 `*_byte` 的范围；超过 256 KiB 按 hunk 拆；没有 `*_byte` 的一侧不高亮。
- `#[gpui::test]`（`TestAppContext`，`lectern_ui::init_fixed`，`testing::harness`）：
  - `screen_opens_the_review_and_shows_loading_phases`：工厂建视图后 `FakeReview::commands` 里没有命令、`open` 被调一次；100 ms 内空白，之后骨架，`Ready` 后内容。
  - `changed_event_reaches_the_main_thread_and_keeps_the_scroll`：从另一个线程 `emit_changed`，前台任务应用快照；滚到第 3000 行后再 `emit_changed`（第一个文件多了一个 hunk），`logical_scroll_top` 对应的键不变。
  - `prepared_replaces_placeholders_without_flicker`：`Changed` 之后不调 `rows()`（`FakeReview` 计数），旧行仍显示；`Prepared` 后换成新行。
  - `prepare_follows_the_visible_range`：滚动后 `Prepare` 的键是可见范围前后各一屏；不变时不重发。
  - `v_marks_seen_and_scrolls_to_the_next_unseen`：V 发 `MarkSeen(当前文件)`，`FakeReview` 换出 `seen` 后视图立即显示已看，动画结束后列表滚到下一个 `Unseen` 的步；全部已看时不滚动、浮条显示 `review-all-reviewed`；检查器隐藏时 V 同样有效。
  - `j_k_walk_the_hunks_across_files`。
  - `looks_good_sends_the_command`、`toggle_seen_sends_mark_unseen`。
  - `progress_tab_lists_the_files_and_jumps`：Progress 页的行与 `files` 一致、噪声在折叠组里、`ChangedSinceSeen` 的次要文字；↵ 跳到步标题；没有屏幕时显示 `review-progress-empty`。
  - `brief_item_jumps_to_the_line`：有 `line` 的条目滚到对应的行并选中；没有的滚到步标题。
  - `side_by_side_pairs_removed_and_added`：切换模式后同一对行在一项里；锚点保持。
  - `fold_expands_by_preparing_more_context`：点 `Fold` 后 `Prepare` 里这个文件的 `context` 变大。
  - `noise_is_folded_by_default`。
  - `highlight_runs_on_the_executor_and_colours_after_the_fact`：`Executor::manual()` 跑完任务前行是纯文本，跑完后带 `Token`；缓存键按 OID 共用（同一 blob 两处只算一次）；离开范围后 LRU 超过 2 MiB 淘汰。
  - `open_file_hands_off_to_the_reader`：选中增删行按它那一侧、上下文行按新一侧，`ReadTarget` 的 `origin`、`expect`、`Anchor::Byte` 正确；`Handoff` 不在时按钮不可用且记 `handoff unavailable`。
  - `share_object_follows_the_rows`：行存进 `rows` 时假 `Reader` 收到两次登记；行离开范围时 `ObjectShare` drop。
  - `focus_is_set_from_the_snapshot`：`Ready` 后 `set_focus(Review)`、`set_focus(Touched)` 各一次，`focus_*` 变化时再发；选中行时 `Current(TextRef)`。
  - `usage_rows_and_turn_tokens`：假 `Usage` 的 `SessionUsage` 显示为用量行与轮次 token；`watch` 的 `UsageInterest` 随屏幕 drop。
  - `failed_snapshot_shows_the_reason`：三种 `ReviewError`。
  - `unrecognized_session_uses_groups`、`folder_project_has_no_progress`。
  - `drop_closes_the_review_and_cancels_analysis`：`Running` 时 drop 发 `CancelAnalyze` 再 `close`。
  - `stale_generation_is_dropped`。
  - `milestone_is_reported_once`：第一次画出行后 `report_milestone(DiffShown)` 一次，之后的 `Prepared` 不再报。
  - `plugins_register_and_deactivate_cleanly`（第 1 步）：`TestKernel` 激活后假 `Contributions` 收到 `Screen`、`InspectorTab`、`KeyBindings` 三项；`review-view` 停用后登记 drop。`handoff_plugin_deactivates_on_its_own`（第 2 步）：`review-view-handoff` 单独停用后屏幕照常、交接按钮置灰。
  - `escape_goes_to_the_panel_first`：面板开着（假 `Contributions` 发 `LayerChanged { open: true }`）且有选中项时 Esc 不清空选中、冒泡到 `workspace`；面板关了之后 Esc 清空选中。
- 语义树：`review_tree_session`、`review_tree_groups`、`review_tree_progress`、`review_tree_failed` 的 insta 快照（`ui_probe` 的 `tree::snapshot`，[#6](https://github.com/SoloJiang/lectern-app/issues/6) 合并后接入；之前用 `Recorder::finish` 的记录去掉 `bounds` 后快照）。
- 日志：`logs_hold_no_contents`。
- fixtures：没有外部格式；`testing::snapshot_fixture` 在代码里合成。

## 对其他 crate 的要求

每一项在对应 crate 的设计文档里补充、各自一个 PR，写 `Refs #15`；在「验收」里列为前置条件。

- **A. `workspace`**：双栏视图要整个内容区，而 `ScreenLayout::reading_column` 在登记时固定。加一条命令：

  ```rust
  pub enum WorkspaceCommand {
      // …
      /// 栈顶屏幕的内容是否限制在阅读栏里，覆盖它 `ScreenLayout::reading_column` 的值；弹出这个屏幕时恢复。立即返回，主线程应用。
      SetReadingColumn(bool),
  }
  ```

  本 crate 切到双栏时 `dispatch(SetReadingColumn(false))`，切回统一时 `dispatch(SetReadingColumn(true))`。`workspace` 设计「数据结构」的 `Nav` 加一个 `reading_column: Option<bool>`，`Pop` 时清掉。
- **B. `i18n`**（[#237](https://github.com/SoloJiang/lectern-app/pull/237)）：`locales/{en,zh-CN}/review.ftl` 加上「界面」表里的键（已有的 5 个不改）。
- **C. `app`**：`plugins()` 的两个插件放进 `PluginSet`（`Profile::Desktop` 的表加两行；test profile 不装）；bench 的 `Driver::open(OpenTarget::Session { … })` 经 `Contributions::dispatch(Push(Route { screen: REVIEW, target: Target::Review(ReviewTarget::Session(key)) }))`；`Milestone::DiffShown` 经 `workspace` 转给探针。
- **D. `bench`**：预算表加 `review_view/items_5000`、`review_view/scroll_5000`；`app/open_diff` 的夹具与场景按「性能」。
- **E. `code_intel`**（随 review 设计的前置条件 F，不阻塞）：两条 `BriefRules` 规则的 `rule` 名与 `args` 的参数名定下后，本 crate 的静态表加两行、`review.ftl` 加两个键。
- **F. `ui`**（#214 的设计加两个字段，随它的第 1 步或之后一个小 PR）：`Palette` 加 `diff_added_inline_background`、`diff_removed_inline_background`：词级差异的底色，`system(Green)` / `system(Red)` 按浅色 28%、深色 36% 的透明度叠在 `control_background` 上（比 `diff_*_background` 深一档），内置表与系统色两条路径都给出，`palette_builtin_*` 里一并检查对比度。本 crate 第 2 步之前合并。已实现：[#238](https://github.com/SoloJiang/lectern-app/pull/238)。

## 未决问题

1. **默认用统一视图还是双栏。** 阅读栏 760pt 放不下双栏（每侧约 45 格），双栏要离开阅读栏（「对其他 crate 的要求」A）。选项：(a) 默认统一，双栏是工具栏上的切换，切换时内容区变宽；(b) 默认双栏。默认 (a)：审阅按轮次从上往下读，统一视图与叙事同宽；切换后本次运行记住（不跨次保存）。
2. **Looks good 之后留在审阅页还是回到项目主页。** 交互规范写「全部标记已看，进入提交或开 MR」，P1 没有提交。选项：(a) 留在审阅页，浮条显示「全部已看」，⌘[ 返回；(b) 自动 `PopToBase`。默认 (a)。
3. **再次进入会话时，最近一次已看之前的轮次是否折叠。** 交互规范「回到第 3 步只看新轮次」。选项：(a) `after_last_seen` 为 false 的轮次折叠成一行「之前的 n 轮」，可展开，列表滚到第一个新轮次；(b) 不折叠，只滚到第一个新轮次。默认 (a)；没有已看记录时不折叠。
4. **agent 说明按纯文本还是 Markdown 显示。** 选项：(a) 纯文本、保留换行、等宽的代码块不识别；(b) 渲染 Markdown（标题、列表、代码块）。默认 (a)，Markdown 渲染另开 issue。

技术问题都已在上文决定。

## 已知限制

- 高亮窗口 8 ms 内解析不完时这一侧保持纯文本，本次打开不重试。
- `Changed` 与 `Prepared` 之间（< 3 ms）旧行与新快照并存，hunk 标签可能与新的 `FileAttribution` 短暂不一致。
- ⌘E 只能打开文件，进入编辑要再按一次 ⌘E（`Reader::open` 没有直接编辑的参数）。
- `review-view-handoff` 三个依赖里任一个失败，三项交接一起不可用。
- 别的 worktree 里的子 agent 不在这个审阅里，它的派出在叙事里只是一行 `Tool`（review 设计「未决问题」1 的 (a)），不能从这里进入它的审阅；同一 worktree 里的子 agent 的 `Subagent` 行点开只滚到它的第一轮。
- `Provided` 规则的文案只覆盖静态表里的（provider, rule）；新的提供者要改表。

## 验收

分三步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-review-view`、`just deny` 通过，并且：

- `rg 'std::fs|std::process|std::thread::spawn|gix|git2' crates/review_view/src` 没有结果（本 crate 不做 IO）。
- `rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|Duration::from_secs|\.font_family\(|gpui_component::Button' crates/review_view/src` 没有结果（颜色、字体、时长、控件只来自 `ui`，与 `ui` 设计的持续检查同一口径）。
- `rg '"[A-Z][a-z]+ [a-z]' crates/review_view/src --glob '!**/tests*' --glob '!**/testing*'` 没有界面字符串字面量（都走 `keys::review::*`）。
- `rg 'eprintln!|println!|log::' crates/review_view/src` 没有结果。
- `python3 scripts/check_architecture.py` 通过：`plugin = true`，不依赖其他插件 crate（含 dev-dependencies），只有 `app` 依赖它。

前置条件：A（`workspace`）、F（`ui` 的两个字段）在第 2 步之前；B（`i18n`）随第 1 步或之前；C、D 随第 3 步；`ui` 设计（#214）的第 1、3、4 步和 `workspace` 的第 2 步在第 1 步之前合并。

**第 1 步：插件、屏幕、叙事与统一视图、简报、Progress、快捷键、探针节点（`Refs #15`）。**

- 「测试」里的 `items_*`、`anchor_*`、`needs_*`、`brief_*`（不含用量）、`screen_opens_*`、`changed_event_*`、`prepared_replaces_*`、`prepare_follows_*`、`v_marks_*`、`j_k_*`、`looks_good_*`、`toggle_seen_*`、`progress_tab_*`、`brief_item_jumps_*`、`noise_is_folded_*`、`failed_snapshot_*`、`unrecognized_*`、`folder_project_*`、`drop_closes_*`、`stale_generation_*`、`milestone_is_reported_once`、`plugins_register_and_deactivate_cleanly`、`escape_goes_to_the_panel_first`、`logs_hold_no_contents`（第 1 步的流程）通过；语义树快照 `review_tree_*` 通过。
- `rg 'highlight::|Languages' crates/review_view/src` 没有结果（高亮在第 2 步）。
- `review_view/items_5000` 在预算内。
- `crates/review_view/AGENTS.md` 的设计文档行指向本文（本 PR 已改）。

**第 2 步：高亮、交接插件、双栏、折叠展开、Analyze 命令（`Refs #15`；前置 A）。**

- `highlight_window_*`、`highlight_runs_*`、`open_file_*`、`share_object_*`、`focus_is_set_*`、`usage_rows_*`、`side_by_side_*`、`fold_expands_*`、`handoff_plugin_deactivates_on_its_own` 通过；`logs_hold_no_contents` 加上第 2 步的流程后仍通过。
- `rg 'highlight::window' crates/review_view/src` 只出现在 executor 任务里（`src/highlight.rs`），`rg 'highlight::window' crates/review_view/src/view` 没有结果。
- `rg 'SetReadingColumn' crates/review_view/src` 有结果。

**第 3 步：`app` 装配、`app/open_diff`、帧基准（`Closes #15`；前置 C、D）。**

- `just test -p lectern-app`：`every_profile_graph_validates` 里 desktop 的插件数加二；启动测试里首帧之前没有 `review_view` 的记录。
- `just bench app/open_diff`：中位数 < 50 ms；日志里 `diff shown` 的 `took_us` 与基准一致。
- `just bench review_view/scroll_5000`：每帧 ≤ 8.3 ms（p95）。
- 手动在 macOS 上运行发布构建：打开一个有 Claude Code 会话的仓库，项目主页 ↵ 进入审阅：简报、轮次、diff 行显示；V 勾选并滚到下一个文件；⌥⌘I 打开 Progress；⇧⌘O 打开完整文件；⌘[ 返回；切换双栏时内容区变宽。
- `app/cold_start`、`app/idle_empty` 与改动前相比不变。

## 实际验证

### 第 1 步

插件、屏幕、叙事与统一视图、简报、Progress、快捷键、探针节点，用到 `ui` 第 4 步（[#242](https://github.com/SoloJiang/lectern-app/pull/242)，`semantic`、`Recorder`），在 macOS（arm64，Rust 1.97.0）上运行。屏幕锁定，没有运行应用：下面全部是无窗口的 `#[gpui::test]` 和 `just` 入口。

- `just check`（含 `cargo clippy --workspace --all-targets --all-features -- -D warnings`、文档链接和架构检查：32 个 crate，`review_view` 带 `plugin = true`，依赖 `gpui`、`futures`、`lectern-i18n`、`lectern-kernel`、`lectern-logging`、`lectern-services`、`lectern-ui`，都不是插件 crate）、`just deny` 通过。`git diff -- Cargo.lock` 只有 `lectern-review-view` 的依赖行和 `lectern-bench` 的一行，没有新的外部 crate。
- 「验收」的五条 `rg` 检查都没有结果：`std::fs|std::process|std::thread::spawn|gix|git2`；`hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|Duration::from_secs|\.font_family\(|gpui_component::Button`（测试里要用时长的地方用 `Duration::from_micros`、`Duration::new` 或 `ui::motion` 的常量）；界面字符串字面量；`eprintln!|println!|log::`；`highlight::|Languages`。
- `just test -p lectern-review-view`：77 个测试通过，都由 nextest 一个测试一个进程地跑。
  - 纯函数（`#[test]`）：`items_*`（13 个：会话审阅的顺序、规则分组的顺序、`Placeholder` 的估计与记下的高度、`context` 改 `RowsKey`、噪声折叠与展开、提示行在简报之后、早先轮次的折叠规则、无完整 diff 的提示、键唯一且能找回、`items_snapshot` 的 insta 快照）、`anchor_*`（5 个）、`needs_*`（6 个）、`brief_*`（5 个：每条规则的键与参数、`Provided` 的回退、zh-CN）、`labels_*`（7 个：归因标签表的每一行）、`visible_range_follows_a_splice`。
  - `#[gpui::test]`（真实的 kernel 加 `FakeReview`）：设计「验收」第 1 步点名的测试都在：`screen_opens_the_review_and_shows_loading_phases`（工厂不发命令、100 ms 与 1 s 两个阶段）、`changed_event_reaches_the_main_thread_and_keeps_the_scroll`、`prepared_replaces_placeholders_without_flicker`、`prepare_follows_the_visible_range`、`v_marks_seen_and_scrolls_to_the_next_unseen`（减弱动态效果下落在下一个未看的步；全部已看后停住）、`v_works_without_the_inspector_and_glides_when_motion_is_on`（`tick_glide` 用测试给的时刻走完）、`j_k_walk_the_hunks_across_files`、`looks_good_sends_the_command`、`toggle_seen_sends_mark_unseen`、`progress_tab_lists_the_files_and_jumps` 对应 `the_progress_tab_lists_the_files_and_jumps`（含点击、↓ 与 ↵）、`brief_item_jumps_to_the_line`、`noise_is_folded_by_default`、`failed_snapshot_shows_the_reason`（三种错误和返回按钮）、`unrecognized_session_uses_groups`、`folder_project_has_no_progress`、`drop_closes_the_review_and_cancels_analysis`（`CancelAnalyze` 在 `close` 之前）、`stale_generation_is_dropped`、`milestone_is_reported_once`、`plugins_register_and_deactivate_cleanly`、`escape_goes_to_the_panel_first`（用 `Content` 区域上的探针 Action 看 Esc 有没有冒泡）；另外 `a_jump_into_a_folded_turn_unfolds_it`（Progress 与简报的跳转遇到折叠的早先轮次时先展开）、`events_of_a_burst_are_applied_once`、`anchor_*`（选中项找回与随项消失、`Prepared` 的 `splice` 在段内与段前）、`a_subagent_line_goes_to_its_turn`、`two_screens_of_one_review_each_keep_their_channel`、`earlier_turns_fold_and_unfold`、`the_analysis_row_shows_each_state`、`the_check_of_a_step_toggles_when_clicked`、`a_target_that_is_not_a_review_shows_the_missing_message`。
  - 语义树：`review_tree_session`、`review_tree_groups`、`review_tree_progress`、`review_tree_failed` 四份 insta 快照（`src/tests/snapshots/`），不含 bounds。
  - `logs_hold_no_contents`：金丝雀放在会话标题、路径、prompt 与说明的正文、工具名、diff 的行、`Provided` 的规则与参数、`Analyze` 的原因和改动集错误的原因里，trace 级别跑完打开、`Prepare`、`Prepared`、J、V、Esc、Looks good、新一代快照、失败态和屏幕 drop；没有记录含金丝雀，`review screen opened`、`diff shown`、`review screen closed`（`marked`、`looks_good`）与 `review failed shown`（`error`）的字段按「日志与诊断」。
- `just bench review_view/`：`review_view/items_5000` 中位数 18 µs（两次运行 18–24 µs）（预算 < 4 ms）。
- `just test -p lectern-bench`：78 个测试通过（预算表的一致性检查含新条目）。
- 没有验证：真实的帧。`#[gpui::test]` 不画真实的帧，勾选、`ARRIVE`、进度条的 `Transition` 随真实时间走，测试里只验证终态（`tick_glide` 由测试给时刻）；`Content > review` 的谓词只对测试台里复刻 `workspace` 的 `Content` 上下文验证过，没有对真实的 `workspace` 验证；滚动 5000 行的帧时间（`review_view/scroll_5000`）和 `app/open_diff` 随第 3 步。真实窗口里的外观、行高与横向裁剪、减弱动态效果下的滚动，随第 3 步在 macOS 上手动运行发布构建时检查。

## 与设计的差异

### 第 1 步

- **只有一个插件**：设计的 `plugins()` 返回 `(ReviewViewPlugin, HandoffPlugin)`。交接插件在第 2 步，所以第 1 步导出 `plugin() -> ReviewViewPlugin`，`Shared` 没有 `handoff`、`handle` 两项；第 2 步改成 `plugins()`，`app` 装配随第 3 步，现在没有调用方。
- **`testing::harness` 在测试里**：`testing`（`test-support`）有 `FakeReview`（加 `set_emitter`、`set_auto_prepare`、`announce`、`emit_event`、`last_needs`、`prepares`、`closed`、`rows_calls` 等供测试读的方法，第一个审阅的 ID 是 `FIRST_ID`）、`snapshot_fixture` 与带标记的 `snapshot_fixture_tagged`（金丝雀测试用）；窗口、假的 `dyn Contributions` 和 `dyn SessionSource` 在 `src/tests/support.rs` 的 `Rig`：基准只要纯函数，不该带上这些。`snapshot_fixture` 返回每一步和每个文件的展示行（`Step` 与 `Final` 两种键）。
- **项的形状**：`Item` 带它在快照里的位置（`Pos { turn, item }`、`ix`），渲染时不查表；`Prompt` 的键是 `TurnRef`（`prompt` 可以为 `None`，这时只有预览）；新增 `Item::Incomplete`（无完整 diff 的文件的提示行，设计只写在「状态与异常」里）。`index: HashMap<ItemKey, u32>` 没有做：找键只发生在刷新、跳转和 J / K 时，`anchor::find` 线性扫描。`NodeId` 不存在项里，渲染时按业务键现建（每帧只建可见项的，约 60 个）。
- **`display_rows`、`DisplayRow`、`DiffMode`、`SetReadingColumn`、统一 / 双栏的 `Segmented` 随第 2 步**：第 1 步只有统一视图，行直接按 `DiffRow` 画；工具栏右侧只有 Refresh。`Fold` 是一行说明，点不开；`context` 表（`BuildOptions::context`）已经决定 `RowsKey`，第 2 步的展开只要写它。
- **`ListState` 在 `ReviewModel` 里**，不在 `ReviewScreen`：Progress 页的跳转和刷新后的找回都在模型里做，`ListState` 是 `Rc` 的句柄，屏幕从模型取。屏幕保留焦点、进度条的 `Transition` 和里程碑标记。
- **可见范围由屏幕每次 render 从列表量**：`ListState::set_scroll_handler` 只在用户滚动（滚轮、滚动条）时触发，首次布局、`scroll_to` 和 `splice` 都不触发。`ReviewModel::measure_visible` 用 `logical_scroll_top`、`viewport_bounds` 和 `bounds_for_item` 算出可见的项：遇到还没布局的项就返回 None、留到下一帧，已知视口被填满后的未布局项不算；首次重建时先假定可见 40 项，使第一屏的 `Prepare` 不必等第一帧。`splice` 之后可见范围按「碰到那一段就整段算可见」调整（`shift_visible`），下一帧再量真实的范围；否则刚展开的行会被当成不在范围内、立刻放回占位。
- **早先轮次的折叠规则**（「未决问题」3 的默认 (a) 的细化）：折叠的是开头那一串 `after_last_seen == false` 的轮次，且只在至少有一轮更新的时候折叠；没有已看记录（全是 true）或没有新轮次（全是 false）时不折叠。这样「之前的 n 轮」总是后面跟着新内容。
- **规则分组视图的组标题**：「界面」的键表里没有 Code / Tests / Config / Deps / Docs 的名字，所以组之间只有一条分隔线，第一个组上方是 `review-files-changed`（总文件数）。要显示组名需要新的 Fluent 键，不在本 PR 范围。
- **多轮归因的连接**：设计写「多轮用顿号连接」；`i18n` 没有连接词的消息，所以每一轮一个标签（`review-hunk-turn`），并排显示。`FileOnly` 的文件把轮次标签放在步标题 / 文件标题下，与表一致。
- **探针节点的 ID 按 `ui::semantic::NodeId` 的形式**：带业务键的节点写成 `视图.区域.item[键]`，设计表里的 `review.step[<id>]` 是 `review.step.item[<id>]`，`.check`、`.prompt`、`.hunk.item[…]`、`.row.item[…]`、`.fold.item[…]`、`.placeholder` 是子节点；文本项是 `review.turn.item[<会话>:<序号>].text.item[<位置>]`。Progress 行的节点由 `ui::Row` 声明（`review.progress.item[<路径>]`，`ListItem`、`selected`），它不接受属性，所以 `seen`、`group`、`noise` 放在外面一层的 `review.progress-file.item[<路径>]`（`Group`）上。`Role::DiffHunk` 的节点带 `selected`；`DiffLine` 的 `selected` 同。`review.diff[…]`（`Role::Diff`，`prepared`、`highlighted` 属性）随第 2 步，第 1 步没有。
- **Analyze 行只显示状态**：`Unavailable` / `Idle` 是置灰的 Analyze 按钮，`Running`、`Done`、`Failed` 是说明文字；触发、取消、重新分析的按钮随第 2 步（「验收」把 Analyze 命令放在第 2 步），所以没有 `review::Analyze` / `CancelAnalyze` 两个 Action。`Unavailable` 的原因（设计：悬停显示）不显示：`ui` 没有提示气泡的原语（`Hint` 是短暂的提示），原因只在 `Analyze` 的探针节点之外不可见；`.tooltip` 属性的几条键（Looks good、Refresh、勾选）同理不用。
- **简报条目的 `line`**：设计写「滚到 `Final` 行里 `(side, line)` 对应的 `Row`」，而会话审阅的叙事只显示每一步的行（「叙事列表的项」），`Final` 的行只在规则分组视图里出现。所以规则分组视图里滚到那一行并选中（行还没准备好时等 `Prepared`），会话审阅里落在这个文件的第一步；`line` 在会话审阅里只决定不了行，条目的 `line` 属性仍在探针节点上。
- **折叠的早先轮次里的文件**：Progress 的行、简报条目和子 agent 行跳向折叠的早先轮次里的内容时，先展开（`review::ToggleEarlierTurns` 的效果）再跳，与噪声文件一样。
- **`review.narrative` 节点**：`Role::List`，属性 `items`（项数）、`visible`（可见范围，`起..止`）、`mode`（第 1 步总是 `unified`）；它包着整个列表，所以叙事里的节点在语义树里是它的子节点。
- **V 之后的滚动**：`SCROLL_NEXT` 用 `Transition` 逐帧给出位移，经 `ListState::scroll_by` 滚动，最后用 `scroll_to` 落在目标项上。目标项还没有布局过（离得远）或系统开了减弱动态效果时直接 `scroll_to`。下一个未看的文件只向后找，不绕回开头（设计「没有时停在原处」）。Progress 里点噪声文件时，噪声先展开再跳转。
- **`Current`**：屏幕建好时设成 GPUI 全局 `Current` 的是它的 `WeakEntity<ReviewModel>`，工具栏和 Progress 页读它；屏幕 drop 后弱引用升级失败，页面显示空状态。同时有两个审阅屏幕时（不应发生）后建的赢。
- **`ReviewModel::missing`**：工厂收到不是 `Target::Review` 的目标时，建一个不打开任何审阅、直接是 `Failed(Missing)` 的模型，显示 `review-failed-missing` 加返回按钮。
- **日志**：`snapshot inconsistent` 只检查 `turn_ref`（轮次的 `session` 超出 `sessions`），`step`、`file_index` 两种不查：文件夹项目里叙事的文件本来可以不在 `files` 里。
- **`Screen::toolbar` 的 `Refresh` 没有提示文字**（见 Analyze 一条）。Progress 页的标题用 `review-progress`（已看 n/m），`review-progress-title` 只作检查器页的标签。
- **基准**：`review_view/items_5000` 加进预算表（`share_macos`，中位数 < 4 ms）和 bench 的 `time` 目标，`lectern-bench` 在 macOS 上加 dev 依赖 `lectern-review-view`（`test-support`）；设计把预算表与 `scroll_5000` 放在前置条件 D（第 3 步），这里只加第 1 步要判定的这一条，`scroll_5000` 与 `app/open_diff` 仍是 `Planned`。基准夹具是纯函数，不开窗口。
- **默认的产品问题**（「未决问题」）：1 统一视图是默认（双栏随第 2 步，所以现在没有切换）；2 Looks good 之后留在审阅页，浮条显示 `review-all-reviewed`；3 折叠早先轮次（见上）；4 agent 说明按纯文本、保留换行，行内代码块不识别。
