# context_drawer 设计

状态：已接受，尚未实现。

对应 issue [#20](https://github.com/SoloJiang/lectern-app/issues/20)。上下文面板（抽屉）回答「这处改动影响到哪」：在 Diff 或完整文件里对一个符号按 Space，面板从右侧滑入，列出它的定义（Definition）、调用方（Callers）、相关测试（Tests）和改动历史（History），并标出没有跟着改的调用方（[ADR 0012](../adr/0012.md)「按需呼出」、[交互规范「窗口结构」](interaction.md#窗口结构)的 Panel、[技术调研「代码按需呼出」](tech-research.md)、[代码导航调研「索引不完整时」](../research/code-navigation.md#索引不完整时)）。⌘ 点击跳到定义时，有多个候选也在这个面板里列出。本 crate 只读 `code_intel`、`review`、`project` 给出的快照，经 `Reader` 打开结果；它不建索引、不做查询、不读文件和 git。

## 范围

- 做：
  - `services::context`：调用方请求面板的事件 `ContextEvent::Show` 和面板 ID 常量（「接口」）。调用方不依赖本 crate，也不 `requires` 它。
  - 两个插件：`context-drawer`（面板、控制器、监听）和 `context-drawer-reader`（打开、跳转、预取经 `dyn Reader`）。
  - 控制器：收下请求，向 `code_intel` 发 `Enclosing` / `Definition` / `References` 查询，定下「主题」（被查看的定义），把结果合成不可变的面板模型；⌘ 点击时决定直接跳转还是打开面板。
  - 面板视图：标题、分段控件（Definition / Callers / Tests / History）、四个列表、调用方的摘要行、打开按钮、各种空状态与失败状态。
  - 调用方的状态：按 `ReviewSnapshot` 判断调用方所在的文件是否被本次审阅改过（文件级，「数据结构」的「调用方的状态」）。
  - Tests：调用方里位于测试文件的那些，加上按命名规则对应、在当前 worktree 里存在的测试文件。
  - History：本次审阅里改过主题所在文件的轮次、命令数和其他会话。
  - `Focus::Drawer` 的设置、第一个结果的预取、三个 bench 里程碑的报告。
  - 全部界面字符串的 Fluent 键、UI 探针的语义节点、具名 Action 与快捷键、向 `workspace` 登记的贡献。
- 不做：
  - 符号抽取、索引、跳转与引用的查询、候选排序：`code_intel`。本 crate 只发 `Query`、读 `QueryResults`。
  - 完整文件阅读、编辑、离开守卫：`file_reader`（经 `dyn Reader`）。
  - 面板的容器、位置（贴右侧、占检查器的 300pt）、圆角阴影、滑入滑出动画、Esc 的分层关闭：`workspace`（[workspace 设计「未决问题」](workspace.md#未决问题)的产品决定）。
  - 审阅对象的打开与关闭、归因、改动集：`review`。本 crate 不调用 `Review::open` / `close` / `command`，只用调用方给的 `ReviewId` 读 `snapshot`。
  - 语义判断（原型里的「types match」「still passes a string」）：语义导航 [#26](https://github.com/SoloJiang/lectern-app/issues/26) 与 Analyze [#66](https://github.com/SoloJiang/lectern-app/issues/66)。
  - ⌘K 的符号搜索：[#102](https://github.com/SoloJiang/lectern-app/issues/102)。
- 本 issue 之外：
  - 在调用方的行上加审阅意见（原型的 Add Review Note）：[#22](https://github.com/SoloJiang/lectern-app/issues/22)。本文只预留 Action 名 `context::AddNote` 和行尾的位置；#22 之前不显示这个按钮。
  - 行级的调用方状态（「这一行在第 3 轮改过」）和按定义行范围的 History：要 `review` 给出每个文件新一侧改动的行范围（「对其他 crate 的要求」J），另开 issue。
  - 提交历史：要 `git` 的按路径 log 和一个领域层的提供者（「未决问题」3、「对其他 crate 的要求」L）。
  - 调用方行里的代码片段（原型的第二行）：要 `code_intel` 的 `Reference` 带这一行的文字（「未决问题」2）。
  - 在面板里继续下钻（调用方的调用方）。
  - 语义导航结果的呈现（`QueryResults::semantic`）：#26。本 issue 里没有提供者，它总是 `NotRequested`，本 crate 不读它。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-kernel` | `Plugin`、`Manifest`、`ActivationContext::on` / `own_staged`、`PluginHandle::own_with` / `emitter` | 插件 crate 只依赖 kernel 层和非插件的库（架构规则 7） |
| `lectern-services` | `context`（新）、`code_intel`、`reader`、`review`、`project`、`workspace` 的类型和服务接口 | 与其他插件只经 `services` 交互 |
| `lectern-executor` | `Executor::spawn_gated`、`Priority::Interactive`：控制器的步进任务 | 与 `review_view` 的高亮任务同一种提交方式 |
| `lectern-ui` | 控件、`Palette`、`Text` / `TextRole`、动效常量、`NodeId` / `Role`、`contrib::{Panel, KeyBindings}` | 颜色、字体、动效只来自 `ui`（[ADR 0015](../adr/0015.md)） |
| `lectern-i18n` | `context.ftl` 的键 | 界面字符串不硬编码（[ADR 0013](../adr/0013.md)） |
| `lectern-logging` | 记录宏、测试用的 `capture` | [ADR 0046](../adr/0046.md) |
| `gpui` | 视图、`uniform_list`、`FocusHandle`、Action | 已有依赖，版本锁定（[ADR 0016](../adr/0016.md)） |
| `futures` | 主线程通道（`mpsc::unbounded`） | 工作区已有，与 `review_view` 相同 |

不引入新的外部依赖。本 crate 改为插件 crate：`Cargo.toml` 的 `[package.metadata.lectern]` 加 `plugin = true`，层仍是 `view`。只有 `app` 依赖它。

## 接口

### `services::context`（新模块）

调用方经自己插件的 `emitter()` 发出请求，本 crate 的监听收下。用事件而不用服务：kernel 没有可选依赖，调用方 `requires` 一个抽屉服务会让审阅视图、阅读视图在抽屉没有装配或停用时一起失效。抽屉不在时请求没有监听，什么也不发生。

```rust
//! 上下文面板的请求（docs/design/context_drawer.md「接口」）。

/// 上下文面板在 `workspace` 里的 ID：`Panel` 贡献、`WorkspaceCommand::OpenPanel`、
/// `Layer::Panel` 都用它。替换 `review_view` 里的 `CONTEXT_PANEL`。
pub const PANEL: &str = "context";

/// 调用方在主线程上发出（审阅视图的 Space、阅读视图的 Space 与 ⌘ 点击）。监听只记下请求、
/// 安排一次步进，不调用其他服务。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ContextEvent {
    Show(ContextRequest),
}

impl Event for ContextEvent {}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ContextRequest {
    pub project: ProjectKey,
    /// 用户看到的那份文本（与 `Focus::Current` 相同）。
    pub at: TextRef,
    pub point: ContextPoint,
    pub intent: ContextIntent,
    /// 发起时所在的审阅：决定调用方的状态和 History。本 crate 只用它调 `Review::snapshot`，
    /// 不打开、不关闭。不在审阅里（Files 打开的文件）时为 None。
    pub review: Option<ReviewId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ContextPoint {
    /// 一个字符位置：阅读视图的光标、⌘ 点击、在 Diff 里点符号。发 `Definition`。
    Position(Position),
    /// 一整行，从 0 起（审阅视图的 Space 作用在选中行上，没有列）。发 `Enclosing`：
    /// 这一行所在的定义（「未决问题」1）。
    Line(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ContextIntent {
    /// Space、点符号：打开面板，看主题的影响面。初始页是 Callers。
    Inspect,
    /// ⌘ 点击：跳到定义。唯一的有绑定依据的候选直接打开，不打开面板；否则面板列出候选，
    /// 初始页是 Definition。
    GoTo,
}
```

- 发出方：`review_view`（「对其他 crate 的要求」E）、`file_reader`（F）、bench 的驱动（H）。任何线程都可以发出，监听只取一把短锁；实际的调用方都在主线程上。
- 同一个请求（四个字段都相等）在面板开着时再次到达，不重新查询，只在面板关着时重新打开。

### 插件

| 插件 ID | provides | requires | 激活 | 内容 |
|---|---|---|---|---|
| `context-drawer` | — | `dyn Contributions`、`dyn CodeIntel`、`dyn Review`、`dyn Projects` | `AfterFirstFrame` | `Panel` 与 `KeyBindings` 的登记；五种事件的监听；控制器与模型 |
| `context-drawer-reader` | — | `dyn Reader` | `AfterFirstFrame` | 把 `Reader` 放进共享状态：打开结果、⌘ 点击的跳转、预取 |

`dyn Reader` 单独放在第二个插件里：`file-reader` 还没有插件时面板照样能查看，只是不能打开结果（与 `review-view-handoff` 的拆分理由相同）。`dyn Review` 和 `dyn Projects` 留在第一个插件里：两者都在 desktop profile 里，调用方的状态和测试文件的存在检查都要用它们。

```rust
/// `app` 装配时调用一次，两个插件都放进 `PluginSet`。`executor` 是进程的执行器，控制器的
/// 步进任务经它提交（`own_with` 只给 `SubmitGate`）。不做 IO。
pub fn plugins(executor: lectern_executor::Executor) -> (DrawerPlugin, DrawerReaderPlugin);

pub struct DrawerPlugin { /* Arc<Shared> */ }
pub struct DrawerReaderPlugin { /* Arc<Shared> */ }
```

- `DrawerPlugin::activate`（kernel 线程）：取四个 `ServiceRef` 放进 `Shared`；`Contributions::register(…, None)` 两项贡献（`Panel(PANEL)`、`KeyBindings`），守卫经 `ctx.own_staged` 交出；`ctx.on` 订阅 `ContextEvent`、`CodeIntelEvent`（只用 `Results`）、`ReviewEvent`（只用 `Changed`）、`ProjectEvent`（只用 `Changed`）、`WorkspaceEvent`（只用 `LayerChanged`）。不提交任务、不查询。`started`：保存 `PluginHandle`。停用：`Active` drop 时关闭控制器持有的查询（`ServiceRef::get` 取不到时跳过），清空 `Shared`。
- `DrawerReaderPlugin::activate`：取 `ServiceRef<dyn Reader>`，`started` 时放进 `Shared::reader`；`Active` drop 时取出。控制器每次要打开或预取时取一份，取不到按没有 `Reader` 处理（「状态与异常」）。
- 两个插件共用一个 `Arc<Shared>`（`plugins()` 建），与 `review_view` 的做法相同。

```rust
pub(crate) struct Shared {
    executor: Executor,
    services: Mutex<Option<Services>>,          // contributions、code_intel、review、projects
    reader: Mutex<Option<ServiceRef<dyn Reader>>>,
    handle: Mutex<Option<PluginHandle>>,        // context-drawer 这次激活的句柄
    /// 监听和视图只碰这一把锁：放进输入，没有排着的步进时提交一个。
    inbox: Mutex<Inbox>,
    /// 只有步进任务取。持有它时可以取 `inbox`、`model`、`view`，反过来不行。
    controller: Mutex<Controller>,
    /// 步进写，视图读（取锁复制 `Arc`）。
    model: Mutex<Arc<DrawerModel>>,
    /// 面板视图活着时的主线程通道。
    view: Mutex<Option<futures::channel::mpsc::UnboundedSender<Job>>>,
}

pub(crate) struct Inbox { items: Vec<Input>, scheduled: bool }

pub(crate) enum Input {
    Show(ContextRequest),
    Wake { id: QueryId, generation: u32 },
    ReviewChanged { generation: u64 },
    ProjectChanged { generation: u64 },
    Panel { open: bool },
    Command(DrawerCommand),
}

/// 视图发出的命令。
pub(crate) enum DrawerCommand {
    /// ↵、⇧⌘O、打开按钮、双击：打开这一行的位置。
    Open(RowRef),
    /// Definition 页上的 Space 或单击：把这个候选定为主题，切到 Callers。
    UseSubject(u32),
    /// 「其他 worktree」提示里的切换按钮。
    SwitchWorktree,
}
```

### 本 crate 的其他公开项

只有 `plugins()` 和两个插件类型。面板视图由 `Panel` 贡献的工厂建，不对外公开。测试辅助在 `#[cfg(test)]` 的 `testing` 模块里，不导出。

## 数据结构

### 主题与查询

```rust
pub(crate) struct Controller {
    /// 每收下一个新请求加一。模型、里程碑和日志都带它。
    seq: u64,
    request: Option<ContextRequest>,
    phase: Phase,
    /// `Enclosing`（`ContextPoint::Line`）或 `Definition`（`Position`）。
    lookup: Option<Applied>,
    /// 主题的 `References`。
    references: Option<Applied>,
    subject: Option<Subject>,
    panel_open: bool,
    /// ⌘ 点击还没有决定跳转还是打开面板。
    goto_pending: bool,
    rows: Rows,
    /// 已经预取过的主题（每个主题只预取一次）。
    prefetched: Option<DefinitionRef>,
}

pub(crate) struct Applied {
    id: QueryId,
    /// 已应用的最高 generation；更低或相等的 `Wake` 丢弃。
    generation: u32,
    /// `results()` 返回 None 之后重新发起过一次。
    requeried: bool,
}

pub(crate) struct Subject {
    definition: DefinitionRef,
    /// 在 `candidates` 里的下标；来自 `Symbol::defines` 时为 None。
    candidate: Option<u32>,
    how: SubjectSource,
}

pub(crate) enum SubjectSource {
    /// 位置就在定义的名字上，或 `Enclosing` 给出的定义。
    Defines,
    /// `jump`：唯一的有绑定依据的候选。
    Jump,
    /// 查询结束时只有一个候选（只按名字找到）。
    Single,
    /// 多个候选里的 `preselect`。标题旁显示「按名字匹配，共 N 个定义」。
    Preselect,
    /// 用户在 Definition 页上选定。
    Picked,
}

pub(crate) enum Phase {
    /// 没有请求（面板由别处打开，或已关闭）。
    Idle,
    /// 等第一批结果。`since` 决定 `loading_phase`。
    Waiting { since: Instant },
    Ready,
    /// 请求的 worktree 不是当前 worktree：不查询（「状态与异常」）。
    OtherWorktree { target: WorktreeKey },
    Failed(QueryError),
}
```

主题按下面的顺序定下，前一条成立就停：

1. 查找查询的 `symbol.defines` 为 Some：主题是它（`Defines`）。`Enclosing` 总是走这一条。
2. `jump` 为 Some：`candidates[jump]`（`Jump`）。
3. 查询已经 `Done`，候选只有一个：它（`Single`）。
4. 候选不止一个：`preselect` 指的那个（`Preselect`）。候选只追加，`preselect` 指向第一个，所以它不会被后到的批次换掉。
5. 没有候选且查询已结束：没有主题，Definition 页显示 `context-definition-none`，其余三页显示各自的空状态。

主题定下或换掉时：关闭上一个 `References`，对新主题发 `References { of }`；Callers、Tests 的行清空后重新累积；History 按新主题的文件重算。用户在 Definition 页换主题（`UseSubject`）只换主题，不重发查找查询。

⌘ 点击（`GoTo`）在第一批带候选的结果到达时，或查询结束时，决定一次：

- `jump` 为 Some 且有 `Reader`：`Reader::open` 那个候选，不打开面板，关闭查找查询，回到 `Idle`。
- 其他情况：`dispatch(OpenPanel(PANEL))`，初始页是 Definition。之后到达的批次即使带了 `jump` 也不自动跳转：用户已经在看列表。

`Inspect` 在收下请求时就 `dispatch(OpenPanel(PANEL))`，初始页是 Callers。

### 行与只追加

```rust
pub(crate) struct Rows {
    definitions: Chunks<DefinitionRow>,
    callers: Chunks<CallerRow>,
    same_name: Chunks<CallerRow>,
    /// `callers` 里位于测试文件的行的下标。
    test_calls: Chunks<u32>,
    test_files: Arc<[TestFileRow]>,
    history: Arc<[HistoryRow]>,
}

/// 按批分块：追加一批只加一块，不复制已有的行。块的列表本身是 `Arc<[Arc<[T]>]>`，
/// 发布模型时只复制指针。
pub(crate) struct Chunks<T> { blocks: Arc<[Arc<[T]>]>, len: u32 }

pub(crate) struct DefinitionRow {
    location: Location,
    name: Arc<str>,
    kind: DefKind,
    scope: Option<Arc<str>>,
    tier: CandidateTier,
    by_name: bool,                 // Evidence::NameOnly
    dependency: Option<DepOrigin>, // TextOrigin::Dependency 时
}

pub(crate) struct CallerRow {
    location: Location,
    enclosing: Option<Arc<str>>,
    test: bool,                    // services::project::is_test_path
    status: CallerStatus,
}
```

不变量：

- `QueryResults` 的 `candidates`、`callers`、`same_name` 只追加（code_intel 设计「服务接口」）。控制器记下每个列表已应用的长度，新到的一批是 `results.callers[applied..]`，追加成一块。行的顺序就是 `code_intel` 给出的顺序：本 crate 不按状态、路径或层重排，用户正在看的行不会移动（[代码导航调研「索引不完整时」](../research/code-navigation.md#索引不完整时)的呈现规则）。
- 行的下标在一个主题内不变。视图的选中项按下标记，主题不变时不需要找回。
- `CodeIntelEvent::Results` 的 `generation` 只用来丢弃过时的唤醒：`id` 不是当前的查找或引用查询（旧请求、旧主题）时丢弃；`generation` 不高于已应用的时丢弃。
- `results(id)` 返回 None（查询被挤出）时重新发起同一个查询一次，已有的行保留，新结果从已应用的长度之后接上；第二次仍是 None 时记 warn、停在已有的行上。
- 例外：调用方的状态、测试文件的存在、History 依赖审阅快照和文件列表，它们变化时原地改写（行的位置不变），这是「结果与之前不同时才原地更新」的情况。
- 调用方摘要和「未跟着改」只看 `callers`，不看 `same_name`（code_intel 设计：同名引用不用于判断漏改的调用方）。

### 调用方的状态

```rust
pub(crate) enum CallerStatus {
    /// 请求没有审阅，或审阅快照取不到、还在 `Loading`：不显示状态，也没有摘要行。
    Unknown,
    /// 所在文件在本次审阅的 `files` 里。`turn` 是 `FileAttribution::turns` 的最后一项
    /// （归因 `Pending`、`FileOnly` 没有轮次、`None` 时为 None）。
    FileChanged { turn: Option<TurnRef> },
    /// 所在文件不在本次审阅的 `files` 里：这个调用方没有跟着改。
    NotUpdated,
}
```

- 判断是文件级的：`ReviewSnapshot::files` 里 `FileKey::new`（删除时为 `old`）的路径与调用方的 `Location::path` 相等，且 worktree 与请求的相同。每个审阅快照的 generation 建一次「路径 → 文件下标」的表，查一次是 O(1)。
- 不用 `Reference::in_review`：它依赖 `Focus::Review` 是谁设的、是哪个审阅；快照给出的成员关系和轮次更直接。
- 不调用 `Review::rows` 和 `Prepare`：`Prepare` 会替换审阅视图对同一个 `ReviewId` 的请求，让它可见的行被丢弃（review 设计 `ReviewCommand::Prepare`）。所以本 issue 拿不到行级的改动范围，同一个文件里没有改到的调用方显示为「所在文件改过」，不标为未跟着改（「已知限制」）。行级判断要 `review` 另给接口（「对其他 crate 的要求」J）。
- 文案只陈述能确定的事：`NotUpdated` 显示「未跟着改」；`FileChanged` 显示「所在文件第 N 轮改过」，不说「已更新」。
- 摘要行（只在有审阅时）：`outside` = `NotUpdated` 的个数，`total` = `callers` 的行数。`outside > 0` 时用 `context-summary-outside`，否则 `context-summary-inside`。两句都说「找到的」：限时搜索可能没有覆盖全部文件，而界面不提示不完整（调研「决定」第 4 条），所以不写「全部调用方都已更新」。

### 测试

- 调用方里的测试：`CallerRow::test` 为 true 的行，下标进 `test_calls`，随 `callers` 追加。判断用 `services::project::is_test_path`，与 `review` 的「Tests」分组同一条规则（「对其他 crate 的要求」A、I）。
- 按命名对应的测试文件：对主题所在文件调 `services::project::paired_tests`（[代码导航调研](../research/code-navigation.md)的首发规则：Java `src/main/java/…/X.java` ↔ `src/test/java/…/XTest.java`，TS / JS `foo.ts` ↔ `foo.test.ts`、`foo.spec.ts`、`__tests__/foo.ts`，Go `foo.go` ↔ `foo_test.go`，Python `foo.py` ↔ `test_foo.py`；与 ⌘K 的配对加分共用）。只列在当前 worktree 的 `ActiveTree::files` 里存在的（`FileList::find`，二分查找，不做 IO）；文件列表还没有 `Ready` 时这一组为空，`ProjectChanged` 后重算。
- 主题来自依赖（`TextOrigin::Dependency`）时不找对应的测试文件。
- Rust 同文件的 `mod tests` 不能按路径识别，不列入（「已知限制」）。

```rust
pub(crate) struct TestFileRow { path: RelPath, status: CallerStatus }
```

### History

```rust
pub(crate) enum HistoryRow {
    /// 改过主题所在文件的一轮：`FileAttribution::turns` 里的一项，按叙事顺序。
    Turn { at: TurnRef, started_at: Timestamp, preview: Arc<str> },
    /// `FileAttribution::commands` 不为空：命令期间改动了这个文件。
    Commands { count: u32 },
    /// `FileAttribution::other_sessions` 不为空。
    OtherSessions { count: u32 },
}
```

- 来源只有审阅快照：`files` 里主题所在文件的那一项的 `attribution`，轮次在 `turns` 里按 `TurnRef` 找到 `TurnEntry`（`started_at`、`prompt_preview`）。找不到的 `TurnRef`（不应发生）跳过并记 warn。
- 是文件级的：列出改过这个文件的轮次，不区分是否改到了主题的行。
- 没有审阅、主题的文件不在审阅里、或文件还在 `Pending` 时，显示对应的空状态（「状态与异常」）。
- 行只显示，不可点击：跳到审阅里那一轮要 `review_view` 提供接口，本 issue 不做（「已知限制」）。

### 面板模型

视图只读这个不可变的快照（架构规则 4）。步进每次改了状态就整体换一个新的 `Arc`。

```rust
pub(crate) struct DrawerModel {
    pub seq: u64,
    /// 每次发布加一。视图据此判断要不要重画。
    pub version: u64,
    pub phase: ModelPhase,         // Idle / Waiting { since } / Ready / OtherWorktree { target, current } / Failed(kind)
    pub intent: ContextIntent,
    pub header: Option<Header>,    // 名字、种类、文件名与行号、依赖的包名与版本、候选总数
    pub initial_tab: Tab,          // Inspect → Callers，GoTo → Definition
    pub definitions: Chunks<DefinitionRow>,
    pub subject: Option<u32>,      // Definition 页上主题的下标
    pub callers: Chunks<CallerRow>,
    pub same_name: Chunks<CallerRow>,
    pub test_calls: Chunks<u32>,
    pub test_files: Arc<[TestFileRow]>,
    pub history: Arc<[HistoryRow]>,
    pub history_state: HistoryState, // NoReview / NotInReview / Pending / Rows
    pub summary: Option<Summary>,  // total、outside
    pub lookup_done: bool,
    pub references_done: bool,
    /// 主题的 `References` 失败（如主题所在的文件变了：`Stale`）：Callers、Tests 显示对应的失败文案。
    pub references_failed: Option<QueryError>,
    pub can_open: bool,            // 有 Reader
}

pub(crate) enum Tab { Definition, Callers, Tests, History }
```

视图自己保存的只有界面状态：当前页、每页的选中下标、同名引用组是否展开、列表的滚动位置、分段控件的滑块。`seq` 变化时全部重置：页设为 `initial_tab`，选中项设为第一行（Definition 页设为 `subject`）。

## 数据流与线程

```
审阅视图 Space ─▶ [主线程] emit(ContextEvent::Show) ─▶ 监听：inbox 放入 Show，排一个步进
步进（executor，Interactive）─▶ 关闭旧查询；当前 worktree？否 → OtherWorktree；是 → query(Enclosing / Definition)
                             └─ Inspect：dispatch(OpenPanel(PANEL)) ─▶ workspace 建面板视图（主线程）
code_intel ─▶ CodeIntelEvent::Results（executor，code_intel 的 publishing 锁内）─▶ 监听：只放入 Wake，排一个步进
步进 ─▶ results(id) ─▶ 追加行、定主题、query(References)、set_focus(Drawer)、prefetch
     └─ GoTo 待决：jump → Reader::open；否则 dispatch(OpenPanel(PANEL))
     ─▶ 发布 Arc<DrawerModel> ─▶ 面板通道 Job::Model
[主线程] 面板的前台任务 ─▶ 读 Shared::model ─▶ cx.notify() ─▶ 下一帧画出 ─▶ report_milestone（每个 seq 每种一次）
用户 ↑↓ ←→ ─▶ 视图自己的状态，立即重画；↵ / Space / 切换 ─▶ inbox 放入 Command，排一个步进
review ─▶ ReviewEvent::Changed（id 是请求的审阅）─▶ ReviewChanged ─▶ 步进：状态、摘要、History 原地改写
project ─▶ ProjectEvent::Changed（请求的项目）─▶ ProjectChanged ─▶ 步进：当前 worktree 与测试文件重新判断
workspace ─▶ LayerChanged(Panel(PANEL), open) ─▶ Panel { open } ─▶ 关闭时：close 两个查询、set_focus(Drawer([]))、回到 Idle
```

- 控制器只在步进任务里运行，同一时刻只有一个（`Inbox::scheduled`）。步进先取出 `inbox` 里的全部输入，再取 `controller` 的锁按顺序应用；应用完再看一次 `inbox`，空了才把 `scheduled` 置回 false。
- 为什么在 executor 上而不在主线程上：⌘ 点击要在面板视图存在之前决定跳转还是打开面板，而插件在没有视图时没有主线程的入口；`CodeIntelEvent` 的监听又运行在 `code_intel` 持有 `publishing` 锁的时候（`publish_ordered`），在监听里调 `results`、`query`、`close` 有重入的风险。所以监听只转发，服务调用都在步进里。步进只做内存计算和立即返回的服务调用，没有 IO、解析和 git 操作，单次 < 1 ms（「性能」）。
- 监听只取 `inbox` 的锁，取不到别的锁，不调用任何服务。步进持有 `controller` 时调 `code_intel.query` 若同步发出事件，监听取的是 `inbox`，不会死锁。锁的顺序：`controller` → `inbox` / `model` / `view`，不能反过来。
- 步进任务经 `PluginHandle::own_with` 和 `Executor::spawn_gated(Priority::Interactive)` 提交，插件停用时随守卫取消。
- 主线程上只有：面板视图读模型、渲染、响应按键和点击、放入命令。没有 IO、解析和 git 操作（架构规则 4）。
- 服务调用的线程：`CodeIntel::query` / `results` / `close` / `set_focus`、`Reader::open` / `prefetch`、`Contributions::dispatch` / `report_milestone`、`Review::snapshot`、`Projects::snapshot` 都是立即返回、任何线程可调用的。
- `Focus::Drawer`：主题变化或 Definition 页的行增加时，发 Definition 页前 8 行的文件加上主题的文件（去重）；面板关闭时发空列表。焦点的 worktree 是请求的 worktree。
- 预取：每个主题一次，在它的第一批结果之后，对 Definition 页的主题行（`Inspect` 时为 Callers 页的第一行）调 `Reader::prefetch`，只给一个目标（[大文件与渲染调研「空闲预取」](../research/large-files-rendering.md#空闲预取)：抽屉只预取当前候选列表第一个候选的文件；`Reader` 在交互静止时运行它）。没有 `Reader` 时跳过。
- 打开一行：`Location` 换成 `ReadTarget { project, worktree, path, origin, expect: Some(oid), at }`，`at` 在 `byte` 为 Some 时是 `Anchor::Byte { byte, line, column }`，否则是 `Anchor::Line { line, column }`（`Object` 一侧必须带 `expect`，reader 设计 `ReadTarget`）。测试文件行打开到 `Anchor::Start`，`expect` 为 None。打开之后 `workspace` 压入阅读屏幕，压入会关闭面板。
- 派生数据：本 crate 不缓存派生数据。行、状态和摘要都从 `QueryResults` 和快照直接算出；`code_intel` 的结果以 OID 为键（架构规则 5 由它满足）。
- 首帧之前什么都不做：两个插件 `AfterFirstFrame` 激活，`activate` 只登记（[ADR 0017](../adr/0017.md)）。没有请求时没有任务、没有查询，`Shared` 只有空的容器。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)、[bench 设计「预算表」](bench.md#预算表)）：

| 预算 | 本 crate 的份额 |
|---|---|
| 跳转的候选列表 ≤ 50 ms（`app/goto_candidates`） | 本 crate ≤ 18 ms：从收到第一批的事件到候选列表画出，一帧 16 ms 加 2 ms 送到 UI 线程（[ADR 0037](../adr/0037.md)）。2 ms 里包括：监听放入输入 < 0.05 ms，步进任务排到 executor 并运行 < 1 ms（`results` 复制一个 `Arc`，追加一块行，发 `OpenPanel`），面板通道送到主线程 < 0.5 ms。⌘ 点击时面板视图在 `OpenPanel` 之后才建，建视图读模型 < 0.5 ms，也在这 2 ms 里 |
| 查找引用第一批 ≤ 200 ms（`app/references_first`） | 同样 ≤ 18 ms：事件到第一批调用方画出。起点是 `Show`，终点是 `Milestone::ContextCallers`；中间的 `Enclosing` / `Definition` 查询和 `References` 查询归 `code_intel`（它的 `code_intel/references_100k/first` 实测约 11 ms） |
| 10 万文件的全部引用 ≤ 2 s（`app/references_100k`） | 每一批 ≤ 18 ms；终点是 `Milestone::ContextCallersDone`（`References` 的状态为 `Done` 的那一帧画出） |
| 滚动与输入 120 fps | 列表用 `uniform_list`（行高固定），只构造可见的约 20 行；每行的元素是常数：`NodeId` 克隆、两段文字、一个状态。↑↓ ←→ 只改视图状态，不经步进 |
| 冷启动 20 ms | 首帧之前没有代码运行；`plugin/context-drawer/activate`、`plugin/context-drawer-reader/activate` 各 < 1 ms（只登记） |
| 空闲内存（没有请求） | < 16 KiB（`Shared`、登记的工厂和快捷键） |
| 面板打开时的内存 | 行持有 `QueryResults` 里 `Arc` 的克隆（路径、名字共享）；1 万个调用方约 1 万 × 120 B ≈ 1.2 MiB；「路径 → 文件下标」表按审阅的文件数，200 个文件 < 32 KiB。面板关闭时全部释放 |

步进的计算量：追加一批是 O(这一批的行数)；审阅快照换代时重建路径表 O(文件数)、重算状态 O(已有的行数)。1 万行时重算 < 2 ms，在 executor 上进行，不占主线程。

基准（`bench`，macOS 判定）：

| ID | 统计量与预算 |
|---|---|
| `app/goto_candidates` | 中位数 ≤ 50 ms（bench 设计已列出）：驱动发 `Show { intent: GoTo }`，位置上的符号有 ≥ 2 个只按名字找到的候选，终点 `Milestone::ContextCandidates` |
| `app/references_first` | 中位数 ≤ 200 ms（bench 设计已列出，归本 crate）：驱动发 `Show { intent: Inspect, point: Position }`，位置在一个定义的名字上，终点 `Milestone::ContextCallers` |
| `app/references_100k` | 中位数 ≤ 2 s（同上）：10 万文件的夹具，终点 `Milestone::ContextCallersDone` |
| `context_drawer/apply_batch`（新） | 中位数 ≤ 2 ms：已有 1 万个调用方、审阅有 200 个文件时，步进应用一批 500 个调用方（取结果、追加、算状态、发布模型）；进程内，不依赖窗口 |
| `plugin/context-drawer/activate`、`plugin/context-drawer-reader/activate`、`plugin/*/resident` | 随 bench 的插件基准自动覆盖 |

不改任何预算，不写新 ADR：本 crate 的份额沿用 ADR 0037 给抽屉的 18 ms。

## 界面

- 层：按需呼出层（[ADR 0012](../adr/0012.md)、[交互规范「三层结构」](interaction.md#三层结构)）。`workspace` 把面板贴右侧放在检查器的位置，宽 300pt（`INSPECTOR_WIDTH`），滑入用 `PANEL_OPEN`、滑出用 `PANEL_CLOSE`，阅读栏不动、不加遮罩；本 crate 只画面板里的内容，不写尺寸和动画（[workspace 设计「未决问题」](workspace.md#未决问题)第 3 条的决定）。
- 布局（自上而下）：
  - 标题行：主题的名字（`TextRole::Body`，原文），右侧关闭按钮（`Icon::Close`，发 `dispatch(ClosePanel)`）。下一行次要文字：种类 · 文件名:行号（行号从 1 起显示）；依赖里的定义显示种类 · 包名 版本（`DepOrigin`）。主题来自 `Preselect` 时再加一行 `context-by-name`。
  - 分段控件 `Segmented`：Definition / Callers N / Tests N / History。数字在对应查询有结果之后才显示。
  - 摘要行（Callers 页，有审阅时）：`context-summary-*`，`secondary_label`。
  - 列表：`GroupedList` 与 `Row` 的样式，`uniform_list` 承载。每行两段：主文字是文件名:行号（`TextRole::Body`），次要文字是目录（`secondary_label`，从开头省略，保留靠近文件名的部分）和「位于 定义名」；状态在行的右侧，一行次要文字，`NotUpdated` 前加一个 `Palette::system(SystemColor::Orange)` 的 6pt 圆点（状态用圆点或一行次要文字，不用彩色标签块：[交互规范「视觉」](interaction.md#视觉)）。300pt 宽度下路径不完整显示，完整路径在探针节点和悬停提示里。
  - Callers 页末尾：同名引用组（默认折叠，`Expander` 展开），标题 `context-same-name`。
  - Tests 页两组：`context-tests-calls`、`context-tests-files`。History 页一组 `context-history-review`。
  - 底部：打开按钮（`Button`，`context-open`），没有 `Reader` 或没有选中行时不可用。
- 加载：`ui::loading_phase(now - since)`：`Nothing` 时只有标题行的位置（没有名字时空白）；`Skeleton` 时列表位置三块 `Skeleton`；`Progress` 时一行 `context-loading`。查找查询的第一批到达后标题出现；`References` 还在进行时 Callers 页显示已到的行，不显示加载态。
- 结果到达：新的一块行直接出现，不做动画（原地追加，不移动已有的行）。
- 用到的 `ui` 原语：`colors(window, cx)`、`Text` 与 `TextRole`、`Segmented`（滑块 `Transition`，`SEGMENT`）、`GroupedList`、`Row`、`Button`、`Skeleton`、`Expander`、`Icon::{Close, ChevronRight, ChevronDown}`、`loading_phase`。不写颜色值、字体名和时长。
- 向 `workspace` 登记的贡献：

  | kind | id | order | payload |
  |---|---|---|---|
  | `Panel` | `context`（`services::context::PANEL`） | 0 | `ui::contrib::Panel { build }`：建 `ContextPanel`，读当前模型、登记主线程通道、取得焦点 |
  | `KeyBindings` | `context` | 0 | 下面 Action 表里的绑定，谓词 `Panel > context` |

- Fluent 键（`crates/i18n/locales/{en,zh-CN}/context.ftl`，「对其他 crate 的要求」D）。带 `$count` 的键都有复数形式（en 的 `[one]` / `*[other]`，zh-CN 只有 `*[other]`），表里写 other：

  | 键 | en | zh-CN | 用途 |
  |---|---|---|---|
  | `context-title` | Context | 上下文 | 面板的无障碍名 |
  | `context-close` | Close | 关闭 | 关闭按钮 |
  | `context-tab-definition` | Definition | 定义 | 分段控件 |
  | `context-tab-callers` | Callers | 调用方 | 分段控件（还没有结果） |
  | `context-tab-callers-count` | Callers { $count } | 调用方 { $count } | 分段控件 |
  | `context-tab-tests` | Tests | 测试 | 分段控件（还没有结果） |
  | `context-tab-tests-count` | Tests { $count } | 测试 { $count } | 分段控件 |
  | `context-tab-history` | History | 历史 | 分段控件 |
  | `context-header-location` | { $kind } · { $file }:{ $line } | { $kind } · { $file }:{ $line } | 标题的次要文字 |
  | `context-header-dependency` | { $kind } · { $package } { $version } | { $kind } · { $package } { $version } | 依赖里的定义 |
  | `context-header-dependency-unversioned` | { $kind } · { $package } | { $kind } · { $package } | 包清单没有写版本 |
  | `context-by-name` | Matched by name, { $count } definitions | 按名字匹配，共 { $count } 个定义 | 主题来自 `Preselect` |
  | `context-kind-function` … `context-kind-other` | Function, Method, Class, Interface, Struct, Enum, Trait, Type, Module, Macro, Constant, Field, Variable, Symbol | 函数、方法、类、接口、结构体、枚举、trait、类型、模块、宏、常量、字段、变量、符号 | `DefKind` 的 14 项，一项一个键 |
  | `context-in` | in { $name } | 位于 { $name } | 行的次要文字：所在的定义（`scope`、`enclosing`） |
  | `context-status-not-updated` | Not updated | 未跟着改 | 调用方的状态 |
  | `context-status-file-changed` | File changed in turn { $ordinal } | 所在文件第 { $ordinal } 轮改过 | 调用方的状态；编号与审阅视图的 `review-turn` 相同 |
  | `context-status-file-changed-no-turn` | File changed | 所在文件改过 | 归因没有轮次 |
  | `context-summary-outside` | { $outside } of { $total } callers found are in files this review did not change | 找到的 { $total } 个调用方里，{ $outside } 个所在的文件本次审阅没有改动 | 摘要行 |
  | `context-summary-inside` | All { $total } callers found are in files this review changed | 找到的 { $total } 个调用方所在的文件本次审阅都改过 | 摘要行 |
  | `context-same-name` | Same name, not resolved to this definition ({ $count }) | 同名、未解析到这个定义（{ $count }） | 同名引用组 |
  | `context-tests-calls` | Called from tests | 测试里的调用 | Tests 页分组 |
  | `context-tests-files` | Test files by name | 按命名对应的测试文件 | Tests 页分组 |
  | `context-history-review` | This review | 本次审阅 | History 页分组 |
  | `context-history-turn` | Turn { $ordinal } | 第 { $ordinal } 轮 | History 行；次要文字是开始时刻（`lectern-i18n` 的 `relative_time`）与 prompt 的预览（原文，一行，截断） |
  | `context-history-commands` | Changed while { $count } commands ran | 有 { $count } 条命令运行期间改动 | History 行 |
  | `context-history-other-sessions` | Also changed by { $count } other sessions | 另有 { $count } 个会话改过 | History 行 |
  | `context-definition-none` | No definition found | 没有找到定义 | 空状态 |
  | `context-callers-none` | No callers found | 没有找到调用方 | 空状态 |
  | `context-tests-none` | No related tests found | 没有找到相关测试 | 空状态 |
  | `context-history-none` | This review did not change this file | 本次审阅没有改动这个文件 | 空状态 |
  | `context-history-pending` | Working out which turns changed this file… | 正在确定哪些轮次改过这个文件… | 归因 `Pending` |
  | `context-history-no-review` | Open this panel from a review to see which turns changed the file | 从审阅里打开本面板，才能看到哪些轮次改过这个文件 | 请求没有审阅 |
  | `context-idle` | Select a line and press Space to see its callers, tests and history | 选中一行后按 Space，查看调用方、测试和历史 | 没有请求时 |
  | `context-loading` | Looking up… | 正在查找… | `Progress` 阶段 |
  | `context-failed-not-open` | The project is not open | 项目没有打开 | `Failed(NotOpen)` |
  | `context-failed-unsupported` | Symbols in this language are not supported yet | 暂不支持这种语言的符号 | `Failed(UnsupportedLanguage)` |
  | `context-failed-no-symbol` | No symbol here | 这里没有符号 | `Failed(NoSymbol)`，`Position` |
  | `context-failed-no-definition-line` | This line is not inside a definition | 这一行不在任何定义里 | `Failed(NoSymbol)`，`Line` |
  | `context-failed-stale` | The file changed after it was shown. Select the line again. | 文件在显示之后改过。请重新选中这一行。 | `Failed(Stale)` |
  | `context-failed-too-large` | The file is too large to look up symbols | 文件太大，无法查找符号 | `Failed(TooLarge)` |
  | `context-failed-unreadable` | The file could not be read | 无法读取文件 | `Failed(Unreadable)` |
  | `context-other-worktree` | Code navigation uses the current worktree, { $current }. Switch to { $target } to look this up. | 代码导航只用当前 worktree（{ $current }）。切换到 { $target } 后再查找。 | 请求的 worktree 不是当前的 |
  | `context-switch-worktree` | Switch | 切换 | 提示里的按钮 |
  | `context-open` | Open | 打开 | 底部按钮 |

  worktree 的名字与 Worktrees 屏幕显示的相同（分支名，分离 HEAD 时为目录名）。

- UI 探针的语义节点（`NodeId` 在模型发布时随行建一次，每帧只克隆）：

  | 节点 | 角色 | 内容 |
  |---|---|---|
  | `context.panel` | `Panel` | `Label::key` = `context-title`；`attrs`：`phase`（`idle` / `waiting` / `ready` / `other_worktree` / `failed`）、`intent`、`seq` |
  | `context.header` | `Text` | `Label::text` 是主题的名字；`attrs`：`kind`、`line`、`dependency`（bool）、`source`（`SubjectSource` 的名字） |
  | `context.close` | `Button` | |
  | `context.tabs` / `context.tab[definition]`、`[callers]`、`[tests]`、`[history]` | `TabBar` / `Tab` | `selected`；`attrs`：`count` |
  | `context.summary` | `Text` | `Label::key`；`attrs`：`total`、`outside` |
  | `context.definitions` | `List` | `attrs`：`rows`、`done` |
  | `context.definition[<path>:<line>]` | `ListItem` | `selected`；`attrs`：`subject`（bool）、`tier`、`by_name`、`dependency` |
  | `context.callers` | `List` | `attrs`：`rows`、`done` |
  | `context.caller[<path>:<line>:<column>]` | `ListItem` | `selected`；`attrs`：`status`（`unknown` / `file_changed` / `not_updated`）、`test`（bool） |
  | `context.same-name` | `Group` | `expanded`；`attrs`：`count` |
  | `context.same-name.item[<path>:<line>:<column>]` | `ListItem` | `selected` |
  | `context.tests.calls` / `context.tests.files` | `Group` | `attrs`：`count` |
  | `context.test-file[<path>]` | `ListItem` | `selected`；`attrs`：`status` |
  | `context.history` | `List` | `attrs`：`state`（`no_review` / `not_in_review` / `pending` / `rows`） |
  | `context.history.turn[<session>:<ordinal>]` | `ListItem` | `Label::text` 是 prompt 的预览（探针关闭时不构造） |
  | `context.history.commands`、`context.history.other-sessions` | `ListItem` | `attrs`：`count` |
  | `context.notice[other-worktree]` / `context.notice.switch` | `Text` / `Button` | `Label::key` |
  | `context.failed` | `Text` | `Label::key` 是失败的键 |
  | `context.loading` | `Group` | `attrs`：`phase` |
  | `context.open` | `Button` | `disabled` |

  `<path>` 是 `Location::path`，`<line>`、`<column>` 从 0 起，与 `Location` 相同。

- Action 与快捷键（KeyContext：面板根元素 `context`；`KeyBindings` 的谓词写 `Panel > context`）：

  | Action | 键 | 上下文 | 处理 |
  |---|---|---|---|
  | `context::SelectNext` / `SelectPrevious` | ↓ / ↑ | `context` | 当前页的下一行 / 上一行，视图状态，滚动到可见 |
  | `context::NextTab` / `PreviousTab` | → / ← | `context` | 下一页 / 上一页（分段控件的滑块动画） |
  | `context::Open` | ↵、⇧⌘O | `context` | 选中行：`DrawerCommand::Open`。没有 `Reader` 时不可用（记一次 `open unavailable`） |
  | `context::Inspect` | Space | `context` | Definition 页：`UseSubject(选中行)`，切到 Callers。其他页不处理 |
  | `context::ToggleSameName` | — | `context` | 展开 / 折叠同名引用组（组标题的点击） |
  | `context::SwitchWorktree` | — | `context` | 提示里的按钮：`DrawerCommand::SwitchWorktree` → `dispatch(SetActiveWorktree(target))` |
  | `context::Close` | — | `context` | 关闭按钮：`dispatch(ClosePanel)` |
  | `context::AddNote` | — | `context` | 预留给 #22，本 issue 不绑定、不显示 |

  Esc 不在本 crate 绑定：冒泡到 `workspace`，按「覆盖层 → 面板 → …」的顺序关闭面板（workspace 设计「数据结构」）。全部 Action 名稳定、可枚举（探针按名字触发）。

- 焦点：面板视图建好时取得焦点（工厂里 `window.focus`），↑↓ ↵ 直接作用在列表上。面板关闭后焦点回到打开它之前的元素（如审阅的叙事列表），J / K 不需要再点一次：这要 `workspace` 在 `OpenPanel` 时记下当时的焦点、`ClosePanel` / `Escape` 关闭面板时交回（「对其他 crate 的要求」C）。C 合并之前焦点回到根，J / K 要先点一下叙事列表（「已知限制」）。另一种焦点模式见「未决问题」5。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)和 [code_intel 设计「状态与异常」](code_intel.md#状态与异常)：

| 情况 | 表现 |
|---|---|
| 面板打开时没有请求（`OpenPanel` 来自别处） | `context-idle` |
| 等第一批 | 按 `loading_phase`：100 ms 内只有标题位置，之后骨架，1 s 后 `context-loading` |
| `Failed(NotOpen)`、`UnsupportedLanguage`、`NoSymbol`、`TooLarge`、`Unreadable` | 标题位置一行对应的 `context-failed-*`，分段控件隐藏；`Unreadable` 记 warn，其余记 debug |
| `Failed(Stale)`（请求的文本已经变了） | `context-failed-stale`。不自动重查：调用方刷新后会给出新的 OID，用户再按一次 Space |
| `References` 失败（主题所在的文件变了：`Stale`） | Definition 页照常；Callers、Tests 显示 `context-failed-stale` |
| 没有候选 | Definition 页 `context-definition-none`；其余页空状态 |
| 没有调用方 / 没有相关测试 | `context-callers-none` / `context-tests-none`（查询结束之后才显示；进行中保持空列表） |
| 结果不完整（`Done(Partial)`） | 不提示（调研「决定」第 4 条）；摘要说「找到的」 |
| 请求的 worktree 不是当前 worktree | 不查询；`context-other-worktree` 与切换按钮。切换后 `ProjectChanged` 让步进重新判断，当前 worktree 等于请求的 worktree 时自动开始查询 |
| 没有 `Reader`（`context-drawer-reader` 没装、失败或停用） | 打开按钮与 ↵ 不可用；⌘ 点击的 `jump` 不能直接打开，改为打开面板列出那一个候选；不预取 |
| 请求没有审阅 | 调用方没有状态、没有摘要行；History `context-history-no-review` |
| 审阅快照取不到（审阅已关闭）或 `Loading` | 同上；之后的 `ReviewChanged` 到达时补上 |
| 主题的文件不在审阅里 / 归因 `Pending` | History `context-history-none` / `context-history-pending` |
| `results(id)` 为 None | 重新发起一次，已有的行保留；再为 None 记 warn，停在已有的行 |
| 唤醒的 `id` 不是当前的查询，或 `generation` 不高于已应用的 | 丢弃 |
| 面板关闭 | 关闭两个查询，`set_focus(Drawer([]))`，模型回到 `Idle`，行全部释放 |
| 插件停用时面板开着 | `workspace` 移除面板贡献；`Active` drop 时关闭查询（`ServiceRef::get` 取不到时跳过） |
| 快照不一致（`TurnRef` 找不到 `TurnEntry`） | 跳过这一项，记 warn，不 panic |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `context_drawer`。监听和步进运行在本插件的激活里：kernel 调用监听时带 `plugin`、`act`，步进任务经 `own_with` 提交，executor 带上 `task`；主线程上的视图回调由 `workspace` 恢复 `plugin`、`act`（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 11 项）。本 crate 的记录带 `seq`（请求序号），查询带 `query`（`QueryId` 的数值）。

- 默认记录：

  | 事件 | 级别 | `msg` | 字段 |
  |---|---|---|---|
  | 一个请求的第一批行画出 | info | `context shown` | `seq`、`intent`（`inspect` / `goto`）、`point`（`position` / `line`）、`has_review`（bool）、`rows`、`took_us`（从 `Show` 起） |
  | ⌘ 点击直接跳转 | info | `goto opened` | `seq`、`took_us` |
  | 查询失败（`Unreadable`、`NotOpen`） | warn | `lookup failed` | `seq`、`query`、`kind`（`enclosing` / `definition` / `references`）、`error`（判别名） |
  | `results` 两次为 None | warn | `results evicted` | `seq`、`query`、`kind` |
  | 快照内部不一致 | warn | `snapshot inconsistent` | `seq`、`kind`（`turn_ref`） |
  | 没有 `Reader` 而用户要打开 | info | `open unavailable` | `seq`（每次激活只记一次） |
- 详细级别（debug）：`request`（`seq`、`intent`、`point`、`replaced`（bool）、`same`（与上一个请求相同））；`other worktree`（`seq`）；`lookup ended`（`seq`、`query`、`state`（`complete` / `partial` / 失败的判别名）、`candidates`、`took_us`）；`subject`（`seq`、`source`、`candidates`）；`batch applied`（`seq`、`query`、`generation`、`added`、`total`、`took_us`）；`wake dropped`（`seq`、`query`、`reason`（`old_query` / `old_generation`））；`statuses rebuilt`（`seq`、`files`、`rows`、`took_us`）；`focus drawer`（`paths`（个数））；`prefetch`（`seq`）；`panel`（`open`（bool））。trace 不另记。
- 关联 ID：`plugin`、`act` 由 kernel 和 `workspace` 设置，`task` 由 executor 填。本 crate 不设 `review`、`session` 作用域；`review` 是 `Ctx` 的字段名，宏里不能用，有没有审阅记为 `has_review`。
- 不记录：符号名、定义的 `scope`、引用的 `enclosing`（都取自文件内容）；文件路径与 `RelPath`；依赖的包名和版本；prompt 的预览与轮次的任何文字；worktree 与分支名；`QueryError::Unreadable` 附带的原因文字；`NodeId` 的业务键。用 `seq`、`QueryId`、`generation`、计数、判别名和耗时代替。
- 金丝雀测试：`logs_hold_no_contents`（`src/tests/logs.rs`，`lectern_logging::testing::capture()` 在 trace 级别）：用含金丝雀的符号名、`scope`、`enclosing`、路径、包名、prompt 预览、worktree 名和 `Unreadable` 的原因构造假 `CodeIntel` 的结果、假 `Review` 的快照和假 `Projects` 的快照，走完 `Inspect`、`GoTo` 跳转、`GoTo` 打开面板、换主题、审阅换代、其他 worktree、查询失败、`results` 被挤出、面板关闭这些流程，断言捕获的记录里没有任何一个金丝雀串。

## 测试

依赖 GPUI，只在 macOS 上编译和测试：`just test -p lectern-context-drawer`。不用真实的 code_intel、git、会话或文件 IO。`dyn CodeIntel`、`dyn Review`、`dyn Projects`、`dyn Reader`、`dyn Contributions` 都用 `testing` 里只记调用、按脚本回答的假实现，装在 `TestKernel` 上；步进用 `Executor::manual()` 手动运行。假 `CodeIntel` 在发出 `Results` 期间把一个标志置位，期间收到任何调用就让测试失败（检查监听只转发）。

- 控制器（`#[test]`，不建窗口）：
  - `request_*`：`Show` 关闭上一个请求的两个查询；相同的请求在面板开着时不重查、关着时重新打开；`Inspect` 发一次 `OpenPanel(PANEL)`，`GoTo` 在决定之前不发；`Line` 发 `Enclosing`，`Position` 发 `Definition`。
  - `goto_*`：第一批带 `jump` 时 `Reader::open` 的 `ReadTarget`（`origin`、`expect: Some(oid)`、`Anchor::Byte`；`byte` 为 None 时 `Anchor::Line`）正确、没有 `OpenPanel`、查找查询已关闭；没有 `jump` 时 `OpenPanel` 且初始页是 Definition；查询结束没有候选时 `OpenPanel` 并显示 `context-definition-none`；没有 `Reader` 时改为打开面板；决定之后再来的 `jump` 不跳转。
  - `subject_*`：`defines`、`jump`、结束时只有一个、多个时的 `preselect` 四种来源；`UseSubject` 关闭旧的 `References` 并发新的，行清空后重新累积；`Enclosing` 的 `NoSymbol` 显示行版本的文案。
  - `append_*`：三批结果依次追加，已有行的下标不变；`generation` 不高的唤醒和旧查询的唤醒被丢弃；`results` 为 None 时重查一次、行保留，第二次记 warn。
  - `status_*`：文件在审阅里且有轮次 → `FileChanged { turn }`；没有轮次 → `FileChanged { turn: None }`；不在审阅里 → `NotUpdated`；没有审阅 → `Unknown` 且没有摘要；`ReviewChanged` 后原地改写、行的位置不变；同名引用不计入摘要；删除的文件按 `FileKey::old` 匹配。
  - `tests_tab_*`：测试路径里的调用方进 `test_calls`；对应的测试文件只列 `FileList` 里存在的；文件列表 `Pending` 时为空、`ProjectChanged` 后补上；依赖里的主题不找测试文件。
  - `history_*`：轮次按叙事顺序、命令数、其他会话数；没有审阅、不在审阅里、`Pending` 三种空状态；找不到的 `TurnRef` 跳过并记 warn。
  - `worktree_*`：请求的 worktree 不是当前的 → 不查询、显示提示；`SwitchWorktree` 发 `SetActiveWorktree`；`ProjectChanged` 之后自动开始查询。
  - `panel_close_*`：`LayerChanged(Panel(PANEL), false)` 关闭两个查询、`set_focus(Drawer([]))`、模型回到 `Idle`；别的面板的 `LayerChanged` 不影响。
  - `focus_drawer_*`：Definition 页前 8 行的文件加主题的文件，去重；面板关闭时为空。
  - `prefetch_*`：每个主题只预取一次、只给一个目标；没有 `Reader` 时不预取。
  - `listener_*`：监听里不调用任何服务（假 `CodeIntel` 的标志）；同一时刻只有一个步进任务；步进持有 `controller` 时 `query` 同步发出事件不死锁。
- `#[gpui::test]`（`TestAppContext`，`lectern_ui::init_fixed`，`testing::harness`）：
  - `panel_shows_loading_phases`：100 ms 内空白，之后骨架，1 s 后 `context-loading`，结果到达后换成列表。
  - `keys_move_selection_and_tabs`：↓↑ 改选中项、→← 换页，不经步进（步进计数不变）。
  - `enter_opens_the_selected_row`、`space_on_a_definition_changes_the_subject`。
  - `panel_takes_focus_when_built`：工厂建视图后焦点在面板的根元素上。
  - `new_seq_resets_tab_and_selection`：`Inspect` 的初始页是 Callers，`GoTo` 是 Definition；同一个 `seq` 的新批次不改选中项。
  - `milestones_are_reported_once_per_seq`：第一批候选、第一批调用方、调用方 `Done` 各报一次。
  - `close_button_dispatches_close_panel`。
  - `plugins_register_and_deactivate_cleanly`：假 `Contributions` 收到 `Panel`、`KeyBindings` 两项；停用后登记 drop。`reader_plugin_deactivates_on_its_own`：`context-drawer-reader` 单独停用后面板照常、打开按钮不可用。
- 语义树：`context_tree_callers`、`context_tree_goto`、`context_tree_history`、`context_tree_failed`、`context_tree_other_worktree` 的 insta 快照（`ui_probe` 的 `tree::snapshot`，[#6](https://github.com/SoloJiang/lectern-app/issues/6) 合并后接入；之前用 `Recorder::finish` 的记录去掉 `bounds` 后快照）。
- 日志：`logs_hold_no_contents`。
- fixtures：没有外部格式。假服务的结果在 `testing` 里用代码合成，路径和名字都是合成的。

## 对其他 crate 的要求

每一项在对应 crate 的设计文档里补充、各自一个 PR，写 `Refs #20`；在「验收」里列为前置条件。

- **A. `services`**：
  - 新模块 `services::context`，内容见「接口」。
  - `services::project` 加两个纯函数（不做 IO，任何线程）：`is_test_path(path: &str) -> bool`，规则与 `review` 的 `noise.rs` 第 2 条（Tests 分组）相同；`paired_tests(path: &str) -> Vec<String>`，按「数据结构」的「测试」一节的首发规则给出候选路径（不检查存在）。⌘K（#102）的配对加分用同一个函数。
  - `services::workspace::Milestone` 加三项：`ContextCandidates`（一个请求的第一批候选画出）、`ContextCallers`（第一批调用方画出）、`ContextCallersDone`（`References` 结束后的那一帧画出）。`code_intel` 设计「验收」第 5 步（端到端基准）等这三项。
- **B. `code_intel`**：`QueryKind` 加一项，`services` 里的类型和本 crate 的实现在同一个 PR：

  ```rust
  pub enum QueryKind {
      // …
      /// 一行所在的定义（审阅里 Space 作用在一行上）。结果只有 `symbol`：`defines` 是名字在这一行上的
      /// 最内层定义，没有时是范围（起始行到 `end_line`）包含这一行的最内层定义；只考虑 tags 捕获的
      /// 定义，不含 `local` 为 true 的定义。没有时 `Failed(NoSymbol)`。不给候选，不查索引。
      Enclosing { at: TextRef, line: u32 },
  }
  ```

  只用当前文本的解析（与 `Definition` 的第 1 步相同），预算与跳转的候选相同（`code_intel` 的份额 32 ms）。「未决问题」1 选 (b) 时不需要这一项。
- **C. `workspace`**：`OpenPanel` 打开面板时记下当时的焦点（`window.focused(cx)`）；`ClosePanel` 或 `Escape` 关闭面板、而焦点在面板里或已经没有时，把焦点交回记下的元素（仍然存在时），否则交给根。`Push`、`Pop` 关闭面板时不交回（新屏幕自己取焦点）。
- **D. `i18n`**：加 `locales/{en,zh-CN}/context.ftl`，内容是「界面」表里的键，以及对应的 `keys::context::*`。
- **E. `review_view`**：
  - `open_context`（Space）改为经 `review-view` 插件（不是 `review-view-handoff`）的 `emitter()` 发 `ContextEvent::Show(ContextRequest { project, at: selected.text, point: Line(selected.line), intent: Inspect, review: Some(id) })`，不再自己 `dispatch(OpenPanel)`。这样 `review-view-handoff` 不在时（`file-reader` 还没有插件）Space 仍然有效。
  - `set_focus(Current)` 照旧由交接部分发出，交接不在时跳过。
  - `CONTEXT_PANEL` 换成 `services::context::PANEL`；`review::Escape` 判断面板是否开着时用它。
  - `Selected::line` 从 0 起，与 `Position::line` 相同（若实现里不是，在发出时换算）。
  - 以后在 Diff 里点符号（原型里可点的符号）时发 `point: Position(行, 列)`；本 issue 不要求。
- **F. `file_reader`**：Space 发 `Show { point: Position(光标), intent: Inspect, review: None }`；⌘ 点击发 `Show { point: Position(点击处), intent: GoTo, review: None }`；`at` 是当前文档的 `TextRef`（`Worktree` / `Object` / `Dependency`）。随 `file_reader` 的实现进行，不阻塞本 crate。
- **G. `app`**：`plugins()` 的两个插件放进 `PluginSet`（`Profile::Desktop` 的表加两行；test profile 不装），与 `code-intel` 的装配同一个 PR 或在它之后：`code-intel` 不在 desktop profile 里时 `context-drawer` 不能激活，现在的 Space 什么也不做。bench 的驱动经 `Kernel::emitter()` 发 `ContextEvent::Show`；三个新的 `Milestone` 经 `workspace` 转给探针。
- **H. `bench`**：预算表加 `context_drawer/apply_batch`；`app/goto_candidates`、`app/references_first`、`app/references_100k` 的场景与终点按「性能」；bench 的 `Milestone` 加 A 的三项。
- **I. `review`**：`noise.rs` 第 2 条改为调用 `services::project::is_test_path`，让 Progress 的 Tests 分组与面板的 Tests 页是同一条规则。不阻塞本 crate。
- **J. `review`（另开 issue）**：每个文件新一侧改动的行范围和它们的轮次，从快照或一个不经过 `Prepare` 的查询读出（形如 `Arc<[(Range<u32>, Arc<[TurnRef]>)]>`）。有了它，调用方的状态和 History 改为行级。
- **K. #22（审阅意见）**：在面板的调用方行上加意见（`context::AddNote`）。锚点可能在审阅的 diff 之外（没有跟着改的调用方所在的文件不在改动集里），#22 的锚点模型要能表示「文件、行、OID」。
- **L. `git` 与领域层（「未决问题」3 选 (b) 时，另开 issue）**：按路径的提交历史（`Repo::log` 现在只按 tip，不按路径过滤）和一个提供它的领域层插件；视图不调用 `git`。

## 未决问题

1. **审阅里 Space 作用在一行上时，查哪个符号。** 审阅视图只有选中行，没有列。选项：(a) 这一行所在的定义（名字在这一行上的优先，否则是包含这一行的最内层定义），需要 `code_intel` 加 `Enclosing`（要求 B）；(b) 这一行上第一个标识符：审阅视图按高亮的 token 给出列，发 `Position`，不需要 B；(c) Space 只在点过符号之后可用。默认 (a)：审阅关心「改了哪个函数、谁调用它」，在函数体里任何一行按 Space 都得到这个函数；调用处的符号用点击（`Position`）查看。
2. **调用方的行显示什么。** 面板宽 300pt。选项：(a) 文件名:行号、目录和「位于 定义名」；(b) 再加一行裁剪的代码（原型的样子），需要 `code_intel` 的 `Reference` 带这一行的文字。默认 (a)：不需要新数据；(b) 另开 issue。
3. **History 是否包含提交历史。** 选项：(a) 只有本次审阅里改过这个文件的轮次；(b) 再加这个文件最近的提交，需要 `git` 的按路径 log 和一个领域层的提供者（要求 L）。默认 (a)：审阅时最相关的是本会话的轮次。
4. **审阅的 worktree 不是当前 worktree 时。** `code_intel` 只用当前 worktree 的映射回答，并行 agent 的审阅常常在别的 worktree 里。选项：(a) 面板提示并给出切换按钮，切换后自动查询；(b) Space 时自动切换当前 worktree；(c) `code_intel` 支持在其他 worktree 里查询（大改动，另写 ADR）。默认 (a)：切换 worktree 有离开守卫，不在用户没有要求时发生。
5. **面板是否取得键盘焦点。** 选项：(a) 打开时取得焦点，↑↓ ↵ 作用在列表上，Esc 关闭后焦点回到原处（要求 C）；(b) 不取焦点，审阅里 J / K 移动、再按 Space 更新面板，面板的列表只能用指针操作。默认 (a)：键盘可以完成全部操作，与「Esc 回到原处」一致。

技术问题都已在上文决定。

## 已知限制

- 调用方的状态是文件级的：与定义在同一个文件里、或在其他被改过的文件里而这一行没有改的调用方，显示「所在文件改过」，不标为未跟着改。行级判断等要求 J。
- History 是文件级的，列出改过这个文件的全部轮次，不区分是否改到了主题的行；行不可点击。
- Rust 同文件的 `mod tests` 里的调用不进 Tests 页（按路径识别不了），它们仍在 Callers 页。
- 结果不完整时不提示（调研「决定」第 4 条）；摘要的措辞只说「找到的」。
- 从 Files 打开的文件发起的请求没有审阅：没有状态、没有 History。
- 只在当前 worktree 里查询（「未决问题」4）。
- 要求 C 合并之前，面板关闭后焦点回到根，J / K 要先点一下叙事列表。
- 步进任务与审阅视图的高亮任务共用 `Interactive` 的工作线程；高亮任务（≤ 8 ms）正在运行时，步进最多晚这么久，个别样本可能超过 18 ms 的份额，中位数不受影响。

## 验收

分三步，每步一个 PR，都要在 macOS 上 `just check`、`just test -p lectern-context-drawer`、`just deny` 通过，并且：

- `rg 'std::fs|std::process|std::thread::spawn|gix|git2|lectern_git' crates/context_drawer/src` 没有结果（本 crate 不做 IO、不读 git）。
- `rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|Duration::from_secs|\.font_family\(|gpui_component::Button' crates/context_drawer/src` 没有结果（颜色、字体、时长、控件只来自 `ui`）。
- `rg '"[A-Z][a-z]+ [a-z]' crates/context_drawer/src --glob '!**/tests*' --glob '!**/testing*'` 没有界面字符串字面量（都走 `keys::context::*`）。
- `rg 'eprintln!|println!|log::' crates/context_drawer/src` 没有结果。
- `rg '\.results\(|\.query\(|\.close\(|\.open\(' crates/context_drawer/src/listen.rs` 没有结果（监听只转发；监听都在这个文件里）。
- `python3 scripts/check_architecture.py` 通过：`plugin = true`，不依赖其他插件 crate（含 dev-dependencies），只有 `app` 依赖它。

前置条件：A（`services`）、B（`code_intel` 的 `Enclosing`）、D（`i18n`）在第 1 步之前合并；C（`workspace` 的焦点）在第 1 步之前或与它并行，`panel_takes_focus_when_built` 不依赖 C；E（`review_view`）、G（`app`）、H（`bench`）随第 3 步；F、I、J、K、L 不阻塞本 issue。

**第 1 步：插件、控制器、Definition 与 Callers、文件级状态、打开与跳转、快捷键、探针节点（`Refs #20`）。**

- 「测试」里的 `request_*`、`goto_*`、`subject_*`、`append_*`、`status_*`、`worktree_*`、`panel_close_*`、`focus_drawer_*`、`prefetch_*`、`listener_*`，以及 `#[gpui::test]` 的全部用例和 `context_tree_callers`、`context_tree_goto`、`context_tree_failed`、`context_tree_other_worktree` 通过；`logs_hold_no_contents` 覆盖第 1 步的流程并通过。
- `context_drawer/apply_batch` 在预算内。
- `crates/context_drawer/AGENTS.md` 的设计文档行指向本文（本 PR 已改）。

**第 2 步：Tests 与 History（`Refs #20`）。**

- `tests_tab_*`、`history_*`、`context_tree_history` 通过；`logs_hold_no_contents` 加上 History 的流程后仍通过。
- `rg 'paired_tests|is_test_path' crates/context_drawer/src` 有结果，`rg '"_test\.go"|"Test\.java"|"\.spec\."|"__tests__"' crates/context_drawer/src --glob '!**/tests*' --glob '!**/testing*'` 没有结果（规则只在 `services` 里有一份）。

**第 3 步：装配与端到端基准（`Closes #20`；前置 E、G、H，`code-intel` 已在 desktop profile 里）。**

- `just test -p lectern-app`：`every_profile_graph_validates` 里 desktop 的插件数加二；启动测试里首帧之前没有 `context_drawer` 的记录。
- `just bench app/goto_candidates app/references_first app/references_100k`：中位数分别 ≤ 50 ms、≤ 200 ms、≤ 2 s；日志里 `context shown` 的 `took_us` 与 `app/references_first` 一致。
- `rg 'CONTEXT_PANEL' crates/review_view/src` 没有结果，`rg 'ContextEvent::Show' crates/review_view/src` 有结果（要求 E）。
- 手动在 macOS 上运行发布构建：打开一个有 Claude Code 会话的仓库，进入审阅，选中一个改过的函数里的一行按 Space：面板从右侧滑入，标题是这个函数，Callers 列出调用方，所在文件没有改动的标「未跟着改」；→ 切到 Tests、History；↵ 打开一个调用方；⌘[ 回到审阅，再按 Space、Esc，焦点回到叙事列表，J 照常移动。
