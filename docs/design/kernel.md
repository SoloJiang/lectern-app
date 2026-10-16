# kernel 设计

对应 issue [#8](https://github.com/SoloJiang/lectern-app/issues/8)。kernel 负责插件装载、服务注册表、事件总线和生命周期，本身不含任何业务功能（[ADR 0010](../adr/0010.md)、[技术调研「插件架构」](tech-research.md#插件架构)）。具体的服务接口（如 `SessionSource`）放在 `services` crate，由 `services` 为它们实现 kernel 的 `Service` trait。

## 范围

- 做：
  - 编译期链接的内置插件：清单、激活条件、按服务依赖自动激活与停用。
  - 服务注册表：按接口类型提供和查找服务，支持一个接口多个实现。
  - 事件总线：类型化事件，插件之间广播通知。
  - 每个插件一张注册表：插件注册的所有内容在停用时按相反顺序撤销。
  - 激活耗时的记录，供 `bench` 读取。
  - 测试用的内核：注入模拟服务，单独激活一个插件。
- 不做：
  - 具体的服务接口（`SessionSource` 等）：在 `services`。
  - 视图、Action、快捷键的注册：由视图层提供的服务实现（见「撤销」），kernel 不依赖 GPUI。
  - 后台任务：在 `executor`。kernel 只负责在停用时丢弃插件持有的任务句柄。
  - 选择每个 profile 装哪些插件：在 `app`。
- 本 issue 之外：
  - 独立进程插件宿主与 grammar 加载器（[#39](https://github.com/SoloJiang/lectern-app/issues/39)）。
  - 插件市场（[#40](https://github.com/SoloJiang/lectern-app/issues/40)）。
  - [#42](https://github.com/SoloJiang/lectern-app/issues/42) 列出的进程组、scanner 抢占、内存总量、回滚、缓存键问题，在 #39 / #40 的设计里回答。

  为它们预留的扩展点：插件清单是运行时数据；服务键带稳定的字符串 ID。

## 依赖

| 依赖 | 用途 | 为什么是它 |
|---|---|---|
| `parking_lot` | 注册表的锁 | 体积小，没有 poison；已在 GPUI 的依赖图里，发布构建不增加体积 |
| `thiserror` | 错误类型 | 已在依赖图里 |

kernel 只依赖 kernel 层（架构规则 1），不依赖 `executor`、`platform` 或任何业务 crate。

## 概念

| 概念 | 说明 |
|---|---|
| 插件 | 实现 `Plugin` 的类型，由 `app` 放进 `PluginSet`。一个插件 crate 通常只有一个插件。 |
| 清单 | 插件的 ID、提供的服务、依赖的服务、激活条件。不调用 `activate` 就能读取，激活之前就能检查依赖图。 |
| 服务 | 一个 trait 对象接口，如 `dyn SessionSource`。插件只依赖接口（架构规则 7）。 |
| 激活 | 条件满足、依赖的服务都已就绪时，kernel 调用插件的 `activate`。 |
| 注册表 | 插件在激活时注册的所有内容：提供的服务、事件监听、视图层的注册、任务句柄。停用时全部撤销。 |

## 接口

### 服务

```rust
/// 服务接口。由 `services` crate 为每个 trait 对象实现：
/// `impl Service for dyn SessionSource { const ID: &'static str = "session-source"; const MULTIPLE: bool = true; }`
pub trait Service: Send + Sync + 'static {
    /// 稳定的字符串 ID：日志、清单、以后的进程插件协议都用它。
    const ID: &'static str;
    /// 是否允许多个实现同时存在（会话来源、语言导航）。为 false 时第二个实现注册失败。
    const MULTIPLE: bool;
}

/// 运行时的服务键：TypeId、稳定 ID 和 `MULTIPLE`。类型擦除后无法再从 TypeId 取回关联常量，
/// 所以 `service::<S>()` 构造时把 `S::MULTIPLE` 一并存进键里，`validate` 据此检查单实现服务。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ServiceKey { /* type_id: TypeId, id: &'static str, multiple: bool */ }

pub fn service<S: Service + ?Sized>() -> ServiceKey;
```

### 插件

```rust
/// 名称：内置插件用 `&'static str`，进程插件用从清单文件读出的 `String`，不泄漏内存。
pub type Name = Cow<'static, str>;

/// 插件的 ID 不在清单里，而是在加入 `PluginSet` 时给出（见「内核」），
/// 这样 `manifest()` panic 时 kernel 仍知道是哪个插件失败了。
pub struct Manifest {
    /// 激活成功时必须全部提供（见「激活的提交」）。
    pub provides: Vec<ServiceKey>,
    pub requires: Vec<ServiceKey>,
    /// 满足任一条件即可激活。不能为空（`validate` 报错）；只想手动激活的插件写 `Manual`。
    pub activation: Vec<Activation>,
}

pub enum Activation {
    /// 启动屏障放开后立即激活（见「启动」）。审阅闭环必需的插件用这一项。
    AfterFirstFrame,
    /// 当前打开的项目里有匹配的文件时激活，如 ["**/*.java", "pom.xml", "build.gradle*"]。
    Workspace(Vec<Name>),
    /// 用户第一次触发这个命令时激活，如 "terminal.open"（见「按命令激活」）。
    Command(Name),
    /// 只在调用 `Kernel::activate(id)` 时激活，如设置页里「立即启用」、调试工具。
    Manual,
}

/// 清单由方法返回，以后的进程插件可以用同一个类型。
pub trait Plugin: Send + Sync + 'static {
    /// 纯内存操作，不得做 IO。进程插件的清单文件由插件宿主（#39）在 executor 上预先读好，再构造插件对象。
    /// kernel 用 `catch_unwind` 包住每次调用；panic 时这个插件进入 `Failed`，其余插件照常检查和激活。
    fn manifest(&self) -> Manifest;

    /// 第一阶段：只登记，不产生任何副作用（不启动任务、不发事件、不读写文件）。
    /// 在 kernel 线程上执行，耗时计入本插件的激活预算。
    /// 返回这次激活的状态对象：缓存的服务、`ServiceSet`、插件自己的数据都放在里面。
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError>;
}

/// 一次激活的状态，由 kernel 持有：提交后调用 `started`，停用时在撤销完监听和守卫之后最后 drop。
/// `Plugin` 对象本身不保存激活期间的状态。
pub trait Active: Send + 'static {
    /// 第二阶段：激活提交（服务和监听已发布）之后在 kernel 线程上调用一次，副作用从这里开始，
    /// 如向 executor 提交任务。任务句柄和之后的注册通过 `handle` 交给 kernel。
    fn started(&mut self, handle: PluginHandle) {}
}
```

### 激活时的上下文

```rust
impl Context<'_> {
    // 这里登记的内容只暂存在本次激活里，`activate` 返回 `Ok` 后才一次性发布（见「激活的提交」）。

    /// 提供服务。key 必须在清单的 provides 里，否则返回错误。
    pub fn provide<S: Service + ?Sized>(&mut self, service: Arc<S>) -> Result<(), PluginError>;

    /// 查找依赖的服务。key 必须在清单的 requires 里：依赖都写进清单，才能在启动前检查。
    pub fn service<S: Service + ?Sized>(&self) -> Arc<S>;               // MULTIPLE = false
    pub fn service_set<S: Service + ?Sized>(&self) -> ServiceSet<S>;    // MULTIPLE = true

    /// 订阅事件。
    pub fn on<E: Event>(&mut self, handler: impl Fn(&E) + Send + Sync + 'static);

    /// 交给 kernel 保管的守卫。只用于创建时没有副作用的守卫；任务句柄在 `started` 里通过 `PluginHandle::own` 交出。
    pub fn own(&mut self, guard: impl Send + 'static);

    /// 交给 kernel 保管、需要在提交时才生效的守卫，如 `dyn Contributions` 的注册。
    /// kernel 在发布服务和监听的同一步里调用每个守卫的 `commit`；激活失败时不调用，直接 drop。
    pub fn own_staged(&mut self, guard: impl Staged);
}

/// 提交时才生效的守卫。`commit` 在 kernel 线程上调用，只做排队等轻量工作。
pub trait Staged: Send + 'static {
    /// `token` 表示这次激活是否已经完全成功：所有守卫的 `commit` 和 `started` 都正常结束后才变为有效，
    /// 激活失败或之后停用时变为无效。守卫排到其他线程的工作（如主线程上应用界面注册）在执行前检查 `token`，
    /// 无效就丢弃。
    fn commit(&mut self, token: ActivationToken);
}

/// 一次激活的成功标记，可以克隆后带到其他线程。
#[derive(Clone)]
pub struct ActivationToken { /* Arc<AtomicU8>：待定 / 成功 / 已失效 */ }

impl ActivationToken {
    /// `Pending`：激活还没结束；`Live`：已完全成功且尚未停用；`Dead`：失败或已停用。
    pub fn state(&self) -> TokenState;
}

pub enum TokenState { Pending, Live, Dead }

/// 一次激活的长期句柄：从 `started` 起有效，到这次激活停用为止。可以克隆并保存在插件的状态里，
/// 用于激活之后开始的工作（打开项目后的扫描、收到事件后提交的任务）。
/// 句柄带激活代数：插件停用后（包括停用后又重新激活），旧句柄上的操作都不再生效。
#[derive(Clone)]
pub struct PluginHandle { /* Weak<Kernel>, 插件 ID, 激活代数 */ }

impl PluginHandle {
    /// 交给 kernel 保管，停用时 drop。这次激活已停用时返回 false，`guard` 送到 kernel 线程上 drop。
    /// `shutdown` 完成、kernel 线程退出之后，被拒绝的守卫在调用线程上 drop，所以守卫必须能在任意线程上安全 drop；
    /// kernel 线程只保证 `shutdown` 之前的 drop 是串行的。
    pub fn own(&self, guard: impl Send + 'static) -> bool;

    /// 订阅事件。返回的 `Subscription` drop 时退订（从下一次 `emit` 起生效）；
    /// 调用 `Subscription::keep()` 则一直保留到这次激活停用。这次激活已停用时不订阅并返回 None。
    pub fn on<E: Event>(&self, handler: impl Fn(&E) + Send + Sync + 'static) -> Option<Subscription>;

    /// 声明有尚未完成的工作，守卫移进任务，任务结束时 drop（见「静止」）。这次激活已停用时返回 None。
    pub fn busy(&self) -> Option<BusyGuard>;

    /// 带激活代数的 Emitter：这次激活停用后（包括之后重新激活），通过它发出的事件直接丢弃。
    pub fn emitter(&self) -> Emitter;
    pub fn is_active(&self) -> bool;

    /// 报告这次激活在后台初始化时失败（如打开 SQLite 失败）：插件进入 `Failed`，按「失败」一节停用。
    /// 这次激活已停用时不做任何事。
    pub fn fail(&self, error: PluginError);

    /// 受限的 kernel 控制句柄，供需要驱动激活的插件使用（`project` 调 `workspace_changed`，
    /// `workspace` 调 `lazy_commands` 和 `command`）。插件在 `PluginSet` 里构造时还没有 `Arc<Kernel>`，
    /// 所以通过这里取得。不包含 `start`、`set_enabled` 和 `shutdown`，这些只给 `app`。
    /// 句柄带这次激活的插件 ID 和代数：这次激活停用后，通过它发出的请求都被忽略（`command` 的 `done` 收到 `PluginError::Stale`）。
    pub fn control(&self) -> KernelControl;
}

#[derive(Clone)]
pub struct KernelControl { /* Weak<Kernel>, 插件 ID, 激活代数 */ }

impl KernelControl {
    pub fn workspace_changed(&self, facts: Arc<dyn WorkspaceFacts>);
    pub fn activate(&self, id: &str);
    pub fn lazy_commands(&self) -> Vec<Name>;
    pub fn command(&self, name: &str, done: impl FnOnce(Result<(), PluginError>) + Send + 'static);
}

/// 事件订阅。drop 时退订。
pub struct Subscription { /* ... */ }

impl Subscription {
    /// 不再单独退订，保留到这次激活停用。
    pub fn keep(self);
}
```

### 内核

```rust
pub struct PluginSet { /* Vec<Arc<dyn Plugin>> */ }

impl PluginSet {
    pub fn new() -> Self;
    /// `id` 全局唯一，如 "lang-java"、"session-claude-code"；进程插件的 ID 由插件宿主从安装目录取得。
    pub fn with(self, id: impl Into<Name>, plugin: impl Plugin) -> Self;

    /// 检查依赖图：插件 ID 不重复；`Command` 激活条件里的命令名在所有插件间不重复；每个服务的稳定 ID 只对应一个 TypeId 和一种 MULTIPLE；
    /// 每个 requires 在本集合里都有提供者；MULTIPLE = false 的服务只有一个提供者；
    /// activation 不为空；没有环。
    /// `app` 为每个 profile 写一个调用它的单元测试，依赖图有误时 CI 失败。
    pub fn validate(&self) -> Result<(), Vec<GraphError>>;
}

/// 多实现服务的当前提供者集合。kernel 在提供者增减时整体替换内部的快照，
/// 所以句柄可以在 `activate` 里取一次并长期保存。
#[derive(Clone)]
pub struct ServiceSet<S: Service + ?Sized> { /* Arc<ArcSwap<[Arc<S>]>> 一类的共享快照 */ }

impl<S: Service + ?Sized> ServiceSet<S> {
    /// 当前提供者的快照。只在一次调用内使用，不要把其中的 `Arc<S>` 另外保存。
    pub fn current(&self) -> Arc<[Arc<S>]>;
}

/// `Send + Sync`，`app` 持有 `Arc<Kernel>`。
pub struct Kernel { /* ... */ }

impl Kernel {
    /// 启动 kernel 线程并立即返回。kernel 线程上读取清单、建表、检查依赖图，然后激活所有
    /// AfterFirstFrame 插件（按依赖顺序）。`app` 在启动屏障放开后调用（见「启动」）。
    /// 依赖图有误时，涉及的插件进入 `Failed` 并记日志；`validate` 的单元测试保证这种情况不进入发布版本。
    pub fn start(plugins: PluginSet) -> Arc<Kernel>;

    /// 以下生命周期方法都只把请求放进 kernel 线程的队列，立即返回，可以在任何线程调用。

    /// 项目模型就绪或变化时由 `project` 插件（通过 `KernelControl`）调用：激活匹配的 Workspace 插件。
    pub fn workspace_changed(&self, facts: Arc<dyn WorkspaceFacts>);

    /// 激活清单里带 `Manual` 的插件。依赖的服务还没有就绪时，请求被记下，依赖就绪后再激活（见「依赖与自动启停」）。
    pub fn activate(&self, id: &str);

    /// 所有插件清单里的 `Command` 名称，供视图层预先注册代理 Action（见「按命令激活」）。
    pub fn lazy_commands(&self) -> Vec<Name>;

    /// 激活声明了这个命令的插件。激活提交（或失败）后在 kernel 线程上调用 `done`；
    /// 已激活时也在 kernel 线程上调用。`done` 用 `catch_unwind` 包住，panic 只记日志，不影响 kernel 线程。
    pub fn command(&self, name: &str, done: impl FnOnce(Result<(), PluginError>) + Send + 'static);

    /// 用户在设置里停用或重新启用插件。停用会连带停用依赖它所提供服务的插件。
    pub fn set_enabled(&self, id: &str, enabled: bool);

    /// 退出时按依赖的相反顺序停用全部插件。撤销全部完成、kernel 线程退出后调用 `done`。
    /// `app` 在 `applicationShouldTerminate:` 里返回 `NSTerminateLater`，`done` 被调用（或等满 2 秒）后
    /// 再 `replyToApplicationShouldTerminate:`。等待期间主线程照常运行，需要在主线程上执行的守卫 drop 不会死锁。
    pub fn shutdown(&self, done: impl FnOnce() + Send + 'static);

    /// 不属于任何插件的 Emitter（`app` 用），不随插件停用失效。
    pub fn emitter(&self) -> Emitter;
    pub fn status(&self) -> Vec<PluginStatus>;   // 设置页和 bench 读取

    /// 当前是否静止（见「静止」）。kernel 每次从忙碌变为静止时发出 `Quiescent` 事件。
    pub fn is_quiescent(&self) -> bool;
}

/// 项目文件的不可变快照。`project` 在后台读 git index 建好后再交给 kernel；
/// `contains` 只查内存里的数据，不做 IO，在任何线程上调用都可以。
pub trait WorkspaceFacts: Send + Sync {
    /// 当前项目里是否有匹配 pattern 的文件。
    fn contains(&self, pattern: &str) -> bool;
}

pub struct PluginStatus {
    pub id: Name,
    pub state: PluginState,                 // Inactive / Waiting { missing: Vec<ServiceKey> } / Active / Disabled / Failed(PluginError)
    pub activate_time: Option<Duration>,    // 最近一次 activate 的耗时
    pub commit_time: Option<Duration>,      // 最近一次提交的耗时
    pub started_time: Option<Duration>,     // 最近一次 started 的耗时（只算同步执行的部分）
}
```

### 事件

```rust
/// 事件类型由发出它的一方定义（通常在 `services`），kernel 只负责分发。
pub trait Event: Send + Sync + 'static {}

#[derive(Clone)]
pub struct Emitter { /* Weak<Kernel> */ }

impl Emitter {
    pub fn emit<E: Event>(&self, event: E);
}
```

## 数据结构与规则

### 依赖与自动启停

- 插件能激活的条件：已启用；激活条件之一已满足；`requires` 里的每个服务都至少有一个已激活的提供者。
- 一个插件激活后，kernel 检查依赖它所提供服务的插件，满足条件的接着激活（按拓扑顺序，同一层按 `PluginSet` 里的顺序，结果确定）。
- 一个服务（不论 `MULTIPLE` 是否为 true）的最后一个已激活的提供者停用时，依赖它的插件先停用（依赖者先于被依赖者）。
- `MULTIPLE = true` 的服务：依赖者激活后，提供者的增减不重新激活依赖者。依赖者通过 `ServiceSet<S>` 读取当前集合，集合在提供者增减时随即更新；需要在变化时做额外处理的，订阅 `ServiceAdded<S>` 和 `ServiceRemoved<S>` 事件（带提供者的插件 ID）。
- 依赖者不得把 `ServiceSet::current()` 里的 `Arc<S>` 保存到本次调用之外，否则提供者停用后仍会被调用。
- 激活条件满足后保持有效：`Workspace`、`Command` 和 `Manual`（调用过 `activate(id)`）一旦满足，在本次运行中一直视为满足；依赖暂未就绪时，等依赖就绪再激活，`status()` 显示为「等待依赖」。用户在设置里停用插件时清除 `Command` 和 `Manual` 的记录；`Workspace` 条件不记在插件上，kernel 保存最近一次的 `WorkspaceFacts`，插件重新启用（包括 `Failed` 后在设置里重试）时用它重新判断。保存的 facts 属于提交它的那次激活（`KernelControl` 带激活代数）：那次激活停用时，kernel 清除这份 facts，等新的 `workspace_changed` 再判断。还在等待的 `command` 回调立即收到 `PluginError::Disabled`，`shutdown` 时收到 `PluginError::ShuttingDown`；插件所依赖的某个服务当前没有可激活的提供者时，收到 `PluginError::DependencyUnavailable`。「可激活」沿依赖链递归判断：提供者已启用、没有 `Failed`、自己的激活条件已经满足，而且它依赖的每个服务也都有可激活的提供者；命令不会替依赖去满足它们的激活条件（如替 `Workspace` 插件假定项目匹配）。回调不会一直挂起。P0 只有一个窗口；多窗口时按项目分别计算，届时修订本节。

### 激活的提交

- `activate` 执行期间，`provide`、`on`、`own` 登记的内容只暂存在本次激活里：服务不进入 `ServiceSet` 和 `service()` 的结果，监听不参与分发。
- `activate` 返回 `Ok` 后，kernel 先检查清单 `provides` 里的每个服务都已 `provide`；缺任何一个都按激活失败处理（`PluginError::MissingService`），否则依赖者会因为等不到这个服务而一直不激活。
- 检查通过后，kernel 在一次加锁里把暂存的服务和监听全部发布（提交），然后调用 `started`。`started` 正常返回、而且期间没有通过 `fail` 或监听 panic 把插件标为失败时，kernel 才把因它而满足条件的后续插件放进队列；否则按失败停用，后续插件不激活。
- 服务在提交时必须已经可用：服务对象需要的内存中的状态在 `activate` 里建好（不做 IO）。`started` 只开始后台工作（提交任务、发事件），这些副作用都发生在提交之后，不会出现「副作用已经发生、激活却回滚」的情况。
- 返回错误或 panic 时，暂存的内容直接丢弃（守卫按相反顺序 drop），已在运行的插件从头到尾看不到这次激活注册的任何东西。
- `activate` 本身不产生副作用：任务、事件、文件读写都从 `started` 开始，激活失败时这些都不会发生。
- 激活期间的状态（缓存的 `Arc<S>`、`ServiceSet`、插件自己的数据）放在 `activate` 返回的 `Active` 对象里；监听闭包和任务需要时持有它的 `Arc` 克隆。停用时 kernel 先撤销监听和守卫，最后 drop `Active`，状态随之释放。`Active` 的 drop 同样用 `catch_unwind` 包住，panic 时记录错误、插件标为 `Failed`，撤销照常完成。`Plugin` 对象本身不保存激活期间的状态，否则停用后提供者和它的内存仍被持有。
- 在 `activate` 里交给 `ctx.own` 的守卫，创建时也不得产生可见的副作用。`dyn Contributions` 的注册通过 `ctx.own_staged` 交出：`commit` 只把注册连同 `ActivationToken` 排到主线程；主线程每帧检查 `token.state()`：`Pending` 时留到下一帧再看，`Live` 时应用，`Dead` 时丢弃。所以注册只在所有 `commit` 和 `started` 都成功之后应用，也不会因为主线程比激活先跑到而丢失。激活在任何一步失败（`activate`、某个 `commit` panic、`started` panic 或调用 `fail`），token 都不会变为有效，失败的注册不会出现在界面上，与 `activate` 运行多久、跨了几帧无关。`workspace` 本身是插件，收到注册时用自己的 `PluginHandle::busy()` 取一个守卫，在主线程上应用完这批注册后 drop，所以 kernel 在界面注册生效之前不会报告静止。

### 撤销

每个已激活的插件有一张注册表，保存它提供的服务、它的事件监听，以及它通过 `own` 交出的守卫。

停用时按以下顺序撤销：

1. 它停用后会失去全部提供者的服务（单实现服务，以及它是最后一个提供者的多实现服务），依赖这些服务的插件先停用。
2. 移除它提供的服务：`ServiceSet` 的快照更新，`service()` 查不到，之后不会有新的调用到达。已经在进行的调用可以执行完；服务实现要能在插件停用过程中返回错误，而不是假定自己的守卫都还在。正在进行的调用可能持有服务的最后一个 `Arc`，服务实现因此可能在调用方的线程上 drop：服务实现的 `Drop` 不得 panic，并且必须能在任意线程上执行。事件监听闭包也一样：分发中的快照可能持有闭包的最后一个引用，闭包捕获的值会在发事件的线程上 drop，所以这些值的 `Drop` 同样不得 panic。kernel 线程上的 drop 仍按「失败」一节隔离。
3. 移除它的事件监听，并等待它正在执行的监听返回（见「事件分发」）。
4. 按与登记相反的顺序 drop 守卫。每个守卫的 drop 单独用 `catch_unwind` 包住：某个守卫的 drop panic 时，记录错误、插件标为 `Failed`，然后继续 drop 剩下的守卫，撤销总是完整执行。

视图层的注册也走守卫。例如 `workspace` 提供一个服务 `dyn Contributions`（接口在 `services`），它的 `register_view` / `register_action` / `bind_key` 返回一个守卫，drop 时注销。`activate` 期间的注册只能通过 `ctx.own_staged` 交出，按上文的 `ActivationToken` 规则应用；`started` 之后的注册直接生效，守卫交给 `PluginHandle::own`。`ctx.own` 只接收创建时没有可见副作用的守卫，不用于界面注册。kernel 因此不依赖 GPUI 的类型。守卫的 drop 若需要在主线程执行，由提供守卫的一方负责转到主线程。

后台任务也走守卫：`executor` 的任务句柄 drop 时取消任务。插件在 `started` 或之后提交任务，用 `PluginHandle::own` 交出句柄。停用后，任务不再向视图交出结果；停用之后才提交的任务，`own` 返回 false 并立即取消。

### 失败

kernel 线程上调用的每一段插件代码都单独用 `catch_unwind` 包住：`manifest`、`activate`、`Staged::commit`、`started`、`command` 的 `done`、事件监听（在 kernel 线程上分发时），以及 kernel 释放的插件对象的 drop（服务实现、守卫、`Active`）。任何一处 panic 都只让对应的插件进入 `Failed`（`done` 回调只记日志），kernel 线程继续处理后面的请求，撤销和 `shutdown` 总能完成。下面按场景说明处理方式。

- `activate` 返回错误或 panic：
  - 这次激活已登记的内容全部撤销；
  - 插件进入 `Failed`，依赖它的插件不激活；
  - 记录错误，设置页显示原因；
  - 其他插件不受影响。
- panic 由 `catch_unwind` 捕获，所以发布构建保持 `panic = "unwind"`（当前默认）。改成 `abort` 前，先修改本节的设计。
- 事件监听里的 panic 同样被捕获：这个监听被移除，插件进入 `Failed` 并停用。
- `started` 里的 panic 同样被捕获：此时激活已经提交，按提交之后的失败处理，即插件进入 `Failed` 并按「撤销」一节停用（依赖者先停用）。与监听 panic 的处理相同。
- `Failed` 的插件在本次运行中不再自动激活；用户在设置里重新启用会重试一次。

### 事件分发

- `emit` 在调用线程上同步分发，按订阅顺序调用监听。
- 分发前先取出监听列表的快照（`Arc<[Handler]>`），不持注册表的锁调用。监听里可以订阅、退订、发事件：
  - 订阅与退订从下一次 `emit` 起生效；
  - 监听里发出的事件排在当前分发之后，在同一线程上依次处理，不会递归。
- 快照里可能有已停用插件的监听。每个监听带上所属插件的存活标记（读写锁保护的布尔值）：
  - 存活标记是原子布尔值，配一把读写锁：调用前取读锁并检查标记，插件已停用就跳过，调用期间一直持有读锁；
  - 停用时第 3 步取写锁把标记置为 false，因此会等这个插件正在执行的监听返回，此后快照里它的监听都会被跳过；
  - 监听里请求的生命周期操作（包括停用自己所属的插件）排队到当前分发结束后执行，不会在持有读锁时等待写锁；
  - 监听 panic 时，先释放读锁，立即在当前线程上把这个插件以及会因它停用而连带停用的插件（依赖它所提供服务的插件，递归）的存活标记都置为 false（原子写，不等写锁），再把停用请求放进 kernel 线程的队列。这样同一次分发里这些插件后面的监听、其他线程上的分发，都不会再调用它们。
  - 置为 false 之前已经在其他线程上通过检查的监听（持有读锁），与 panic 发生时已在执行的监听同样对待：kernel 无法中途打断它们，它们照常执行完。停用第 3 步取写锁时会等这些监听返回，之后才 drop 守卫和 `Active`，所以它们执行期间插件的状态仍然有效。插件的监听因此要能在插件已标为 `Failed` 时运行完毕，与停用过程中的服务调用要求相同（见「撤销」第 2 步）。
- 监听必须快速返回：只更新状态或把工作交给 executor。事件总线用于插件之间的通知，如会话新增一轮、项目模型变化；视图内部的状态变化用 GPUI 自身的订阅。
- 没有监听的事件直接丢弃，不缓存。

### 线程

- kernel 有一个自己的线程（kernel 线程，`Kernel::start` 时创建，`shutdown` 后退出）。所有生命周期操作都在这个线程上串行执行：读取清单、检查依赖图、`activate`、`started`、停用与撤销。公开的生命周期方法只把请求放进队列，立即返回，所以主线程上不会执行激活（[ADR 0017](../adr/0017.md) 规定插件激活在后台线程上进行）。
- 生命周期操作期间产生的新请求（如激活时发出的事件导致另一个插件需要激活），以及事件分发期间监听里发出的请求，都进入同一个队列，按顺序执行。
- `emit` 在调用线程上同步分发（见「事件分发」），与 kernel 线程无关。
- `shutdown` 完成之前，守卫的 drop 在 kernel 线程上执行；kernel 线程退出之后，通过旧句柄被拒绝的守卫在调用线程上 drop（见 `PluginHandle::own`）。需要在主线程执行的（如注销视图层的注册），由提供守卫的一方负责转到主线程。

### 启动

- 首帧之前不调用 kernel 的任何函数（ADR 0017）。`app` 在启动屏障放开后调用 `Kernel::start`：
  - desktop、probe：第一帧真正显示（`presentedTime` 回调）时放开；
  - headless（命令行与基准）、test：没有窗口，立即放开。
- 屏障由 `app` 按 profile 实现，kernel 不区分 profile。

### 按命令激活

用户第一次用快捷键或菜单触发一个由 `Command` 激活的插件提供的 Action 时，这个 Action 还没有注册。处理方式：

1. `workspace` 在 `started` 里从 `handle.control().lazy_commands()` 取得所有这类命令名，为每个命令注册一个常驻的代理 Action（不依赖插件）。命令的 Action 只有这一个；插件通过 `Contributions::handle_command(name, handler)` 登记处理函数，而不是另外注册同名的 Action。
2. 代理 Action 被触发时：处理函数已登记就直接调用；否则调用 `control.command(name, done)`。
3. 插件激活提交后 `done` 被调用；`workspace` 把它转到主线程，等这次激活的处理函数在下一帧登记之后调用它。用户只需触发一次。
4. 激活失败时，`done` 收到错误，`workspace` 显示一次提示。
   激活成功、但下一帧应用完注册后仍没有这个命令的处理函数时（插件漏了 `handle_command` 或名字写错），`workspace` 同样提示一次、记录错误，这次触发作废，不会一直等待。
5. 插件停用时，处理函数的守卫被 drop，代理仍然在。用户重新启用插件后再次触发命令，代理照常走第 2 步激活它。

### 静止

[ADR 0017](../adr/0017.md) 的内存基准和 UI 探针要等「kernel 发出已静止信号」再采样。kernel 在以下条件同时成立时为静止：

- kernel 线程已经处理完启动时的激活；
- 生命周期操作的队列为空，kernel 线程上没有正在执行的生命周期操作（激活、提交、`started`、停用与撤销、`shutdown`）；
- 没有正在进行的事件分发（kernel 计数各线程上进行中的 `emit`，监听可能正要取 `BusyGuard`）；
- 没有未 drop 的 `BusyGuard`。

插件把初始化工作（扫描会话目录、打开 SQLite、建索引）交给 executor 时，用 `PluginHandle::busy()` 取一个守卫移进任务，任务结束时守卫随之 drop。`PluginHandle` 在 `started` 里取得并保存，所以打开项目、收到事件之后开始的工作也能标记为忙碌。kernel 不知道 executor 的存在，只计数守卫。周期性或长期运行的任务（文件监听、定时刷新）不取守卫。插件停用时，它的任务句柄被 drop、任务被要求取消，但取消是协作式的：任务真正结束、守卫被 drop 之前，守卫仍然计入，kernel 不报告静止。

每次从忙碌变为静止，kernel 发出一次 `Quiescent` 事件；每次从静止变为忙碌，发出一次 `Busy` 事件。这两个事件本身的分发不计入进行中的 `emit`，否则发出 `Quiescent` 会让 kernel 重新变忙。bench 收到 `Quiescent` 后等 5 秒再采样，期间收到 `Busy` 就重新开始等待，采样前再用 `is_quiescent()` 确认一次。`Quiescent` 的用途：启动后第一次静止用于空窗口的内存基准，打开项目触发的激活与初始化结束后再次静止，用于 monorepo 的内存基准。超时（ADR 0017 规定首帧后 30 秒内不静止即失败）由 bench 判断。

## 性能

- 首帧之前不调用 kernel 的任何函数：`Kernel::start` 在启动屏障放开后才调用，建表、读清单、检查依赖图和激活都在 kernel 线程上执行，不占主线程。
- 激活耗时：每个插件的 `activate`、提交（检查清单、发布服务和监听、调用各守卫的 `commit`、更新快照）和 `started`（同步执行的部分）由 kernel 分别计时，结果在 `status()` 里，预算按三者之和计算。建议预算是单个插件 1 ms、首帧后 `AfterFirstFrame` 全部激活合计 5 ms。数字在 [#7](https://github.com/SoloJiang/lectern-app/issues/7) 里用原型实测后校准，并写进 bench 的预算表。
- 常驻内存在同一进程里无法准确地按插件区分。bench 对每个插件启动两次：一次只含它的依赖闭包，一次是依赖闭包加上它，两次静止后的 `phys_footprint` 之差记为这个插件的常驻内存。完整 profile 的总内存另外按 ADR 0017 的预算单独检查。`PluginSet` 支持这两种组合。
- 服务查找是一次读锁加一次哈希表查找；插件应在 `activate` 里取出需要的 `Arc`，保存在激活内的状态对象里（见「激活的提交」），使用时直接用保存的 `Arc`。
- 普通的事件分发（不在监听里再发事件）不分配内存：监听快照是 `Arc` 克隆，没有监听时只有一次查表。监听里发出的嵌套事件要先保存到分发结束，会装箱后放进本线程的队列，有一次分配；这条路径单独做基准。

## 测试

全部测试在 Linux 上运行（`just test -p lectern-kernel`）：

- 依赖图：`validate` 对缺提供者、重复插件 ID、两个服务共用一个稳定 ID、单实现服务有两个提供者、空的激活条件、环，分别给出对应的错误。
- 激活的提交：
  - `activate` 先 `provide` 再返回错误时，并发的依赖者和事件分发都看不到这个提供者和它的监听，`started` 不被调用；
  - `activate` 返回 `Ok` 但缺少清单里声明的服务时，按失败处理。
- 句柄：`on` 返回的 `Subscription` drop 后，下一次 `emit` 起不再调用；停用后旧的 `PluginHandle` 上 `own` 立即 drop 守卫并返回 false，`on` 不订阅，`busy` 返回 None，从它取得的 `Emitter` 发出的事件被丢弃；重新激活后旧句柄仍然无效。
- 退出：`shutdown` 的 `done` 在全部撤销完成、kernel 线程退出后调用；需要在主线程 drop 的守卫在等待期间完成。
- 静止与界面注册：只注册了 `Contributions` 的插件，在注册应用到界面之前 kernel 不静止。
- 线程：所有 `activate` / `started` 和 `shutdown` 之前的守卫 drop 都在 kernel 线程上执行，公开方法在调用线程上立即返回；`shutdown` 之后被拒绝的守卫在调用线程上 drop。
- 界面注册：`started` panic 或调用 `fail` 时，已经 `commit` 的界面注册不会被应用（token 为 `Dead`）；主线程在 token 为 `Pending` 时先处理，注册不会丢失，激活成功后下一帧应用。
- 连带失效：提供者的监听 panic 后，同一次分发里依赖它的插件的监听也不再被调用。
- 取消的任务：停用后仍在运行的任务结束之前，kernel 不静止。
- 命令：依赖链更深处的提供者激活条件不满足时，`done` 收到 `DependencyUnavailable`。
- 静止：停用与撤销进行中时不静止。
- workspace facts：提交 facts 的 `project` 激活停用后，保存的 facts 被清除。
- 命令：依赖的唯一提供者激活条件不满足时，`done` 收到 `DependencyUnavailable`。
- 清单：某个插件的 `manifest` panic 时，按 `PluginSet` 里给出的 ID 把它标为 `Failed`，其余插件照常激活。
- panic 隔离：`Staged::commit`、服务实现的 drop 各自 panic 时，插件进入 `Failed`，撤销照常完成。
- 后续插件：`started` 里调用 `fail` 时，依赖它的插件不会被激活。
- 命令：依赖的服务再也不会就绪时，挂起的 `done` 收到 `DependencyUnavailable`。
- 命令：两个插件声明同一个命令名时 `validate` 报错；停用一个正在等待依赖的命令插件，挂起的 `done` 收到 `Disabled`。
- 静止：发出 `Quiescent` / `Busy` 不会触发新的状态切换。
- 失败：监听 panic 后，同一次分发里它所属插件后面的监听不再被调用；`PluginHandle::fail` 让插件进入 `Failed` 并停用；`Active` 的 drop panic 时撤销照常完成。
- 提交门：`own_staged` 的守卫只在提交时 `commit`，激活失败时不 `commit`。
- 重新启用：项目没变时，停用后重新启用的 `Workspace` 插件按保存的 `WorkspaceFacts` 重新激活。
- 命令：激活成功但没有登记处理函数时，`workspace` 报错，代理不会一直等待。
- 状态对象：`started` 收到的是 `activate` 返回的同一个 `Active`；停用后它被 drop。
- 按命令激活：`command` 的 `done` 在激活提交后被调用一次；已激活时立即调用；失败时收到错误；`done` panic 后 kernel 线程继续处理后续请求。
- 控制句柄：插件停用后，它之前取得的 `KernelControl` 发出的 `workspace_changed`、`activate` 被忽略，`command` 的 `done` 收到 `Stale`。
- 状态：激活条件已满足、但依赖未就绪的插件，`status()` 显示 `Waiting`，并列出缺少的服务。
- 守卫：停用后通过旧句柄 `own` 的守卫在 kernel 线程上 drop。
- 启动：`Kernel::start` 之前没有任何插件代码运行。
- 自动启停：
  - A 依赖 B，B 依赖 C：激活顺序是 C、B、A；
  - 停用 C 连带停用 A、B，停用顺序是 A、B、C；
  - 重新启用 C 后 B、A 恢复；
  - `MULTIPLE` 服务的提供者增减时，依赖者不重新激活，`ServiceSet::current()` 随即反映变化，并发出 `ServiceAdded` / `ServiceRemoved`；
  - 停用一个多实现服务的提供者后，依赖者通过 `ServiceSet` 不再调用到它；停用的是最后一个提供者时，依赖者先停用。
- 撤销：用记录 drop 顺序的守卫，检查服务、监听、守卫按规定的顺序撤销，撤销后服务查不到、事件收不到；某个守卫的 drop panic 时，其余守卫仍被 drop，插件进入 `Failed`。
- 失败：
  - `activate` 返回错误或 panic 时，部分注册被撤销，插件进入 `Failed`；
  - 依赖者不激活；
  - 其他插件不受影响。
- 事件：
  - 监听里订阅、退订、发事件的顺序语义；
  - 插件在分发进行中被停用（另一个监听 panic，或另一个线程停用它）后，快照里它的监听不再被调用；停用会等它正在执行的监听返回；
  - 监听里停用自己所属的插件不死锁；
  - 多线程同时 `emit`、同时触发生命周期操作时不死锁（压力测试）。
- 静止：`BusyGuard` 未 drop 时不静止；全部 drop 后发出一次 `Quiescent`；之后有新的激活，或已激活的插件通过保存的 `PluginHandle` 取了新守卫时，重新变为忙碌，再次静止时再发一次；插件停用后，已取消但还没结束的任务的守卫仍然计入，直到任务结束。
- 激活条件：
  - `Workspace` 用模拟的 `WorkspaceFacts` 触发；
  - `Command` 第二次调用不重复激活；
  - `Manual` 插件只在 `activate(id)` 时激活；依赖未就绪时请求被保留，依赖就绪后激活。
  - `started` panic 时，插件按提交之后的失败停用，依赖者先停用。
- 状态释放：插件停用后，它缓存的其他插件服务的 `Arc` 被释放（测试检查提供者的强引用数回到停用前的基线）。
- 静止：有正在进行的 `emit` 时不静止；静止后又变忙时发出 `Busy`。
- 按命令激活：停用后重新启用的命令插件，再次触发命令仍能激活。
- 生命周期的结果（激活顺序、停用顺序、状态）用 insta 快照记录。

测试辅助放在 `test-support` feature 下：

```rust
pub mod testing {
    /// 只装一个被测插件，依赖的服务由测试注入模拟实现。
    pub struct TestKernel { /* ... */ }
    impl TestKernel {
        pub fn new(plugin: impl Plugin) -> Self;
        pub fn mock<S: Service + ?Sized>(self, service: Arc<S>) -> Self;
        pub fn activate(&self) -> Result<(), PluginError>;
        pub fn deactivate(&self);
        pub fn emitted<E: Event + Clone>(&self) -> Vec<E>;   // 被测插件发出的事件
    }
}
```

每个插件 crate 都用 `TestKernel` 写自己的测试，不需要启动整个应用（[技术调研「与 agent 开发的关系」](tech-research.md#与-agent-开发的关系)）。

## 与技术调研草案的差别

技术调研里的插件接口示意用关联常量、`type Config` 和 `fn apply(ctx, cfg)`。kernel 的接口改为：

- 清单用 `manifest(&self)` 返回运行时数据，进程插件以后可以用同一个类型。
- P0 不引入插件配置：配置等到设计设置页时一起做，届时以服务的形式提供（`dyn Settings`），不改 `Plugin` trait。
- 去掉「首次请求服务时激活」：查找时同步激活会让查找的耗时不可预测，还会在激活中途再次进入生命周期操作。需要延迟加载的插件用 `Command` 或 `Workspace` 条件。

## 未决问题

没有需要产品决定的问题。实现中发现预算或线程模型需要修改时，先修改本文。

## 验收

- `PluginSet::validate`、自动启停、撤销、失败隔离、事件分发的测试全部通过，结果有 insta 快照。
- kernel 不依赖 GPUI 和 kernel 层以外的 crate（`just check` 的架构检查通过）。
- `TestKernel` 能注入模拟服务并单独激活一个插件。
- `status()` 给出每个插件的激活耗时，`Quiescent` 事件和 `is_quiescent()` 给出静止信号，供 #7 的 bench 读取。
