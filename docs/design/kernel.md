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
pub struct Manifest {
    /// 全局唯一，如 "lang-java"、"session-claude-code"。
    pub id: &'static str,
    pub provides: Vec<ServiceKey>,
    pub requires: Vec<ServiceKey>,
    /// 满足任一条件即可激活。不能为空（`validate` 报错）；只想手动激活的插件写 `Manual`。
    pub activation: Vec<Activation>,
}

pub enum Activation {
    /// 首帧之后立即激活。审阅闭环必需的插件用这一项。
    AfterFirstFrame,
    /// 当前打开的项目里有匹配的文件时激活，如 &["**/*.java", "pom.xml", "build.gradle*"]。
    Workspace(&'static [&'static str]),
    /// 用户第一次触发这个命令时激活，如 "terminal.open"。
    Command(&'static str),
    /// 只在调用 `Kernel::activate(id)` 时激活，如设置页里「立即启用」、调试工具。
    Manual,
}

/// 清单由方法返回，以后的进程插件可以用同一个类型。
pub trait Plugin: Send + Sync + 'static {
    /// 纯内存操作，不得做 IO：`Kernel::start` 可能在主线程上调用它。
    /// 进程插件的清单文件由插件宿主（#39）在 executor 上预先读好，再构造插件对象。
    fn manifest(&self) -> Manifest;

    /// 在调用线程上同步执行，只做注册，不做 IO 和解析：耗时的工作交给 executor，
    /// 任务句柄用 `ctx.own` 交给 kernel。耗时计入本插件的激活预算。
    fn activate(&self, ctx: &mut Context<'_>) -> Result<(), PluginError>;
}
```

### 激活时的上下文

```rust
impl Context<'_> {
    // `provide`、`on`、`own` 登记的内容先暂存在本次激活里，`activate` 返回 `Ok` 后才一次性发布
    //（见「激活的提交」）。

    /// 提供服务。key 必须在清单的 provides 里，否则返回错误。
    pub fn provide<S: Service + ?Sized>(&mut self, service: Arc<S>) -> Result<(), PluginError>;

    /// 查找依赖的服务。key 必须在清单的 requires 里：依赖都写进清单，才能在启动前检查。
    pub fn service<S: Service + ?Sized>(&self) -> Arc<S>;               // MULTIPLE = false
    pub fn service_set<S: Service + ?Sized>(&self) -> ServiceSet<S>;    // MULTIPLE = true

    /// 本插件的活动句柄，可以克隆并保存在插件的状态里，激活之后随时取 `BusyGuard`
    ///（如打开项目后开始扫描时）。插件停用后取到的守卫不计入（见「静止」）。
    pub fn activity(&self) -> Activity;

    /// 订阅事件。
    pub fn on<E: Event>(&mut self, handler: impl Fn(&E) + Send + Sync + 'static);

    /// 交给 kernel 保管的对象，如视图层注册返回的守卫、executor 的任务句柄。
    /// 停用时按与登记相反的顺序 drop。
    pub fn own(&mut self, guard: impl Send + 'static);

    /// 发送事件的句柄，可克隆后在后台任务里使用。
    pub fn emitter(&self) -> Emitter;
}
```

### 内核

```rust
pub struct PluginSet { /* Vec<Arc<dyn Plugin>> */ }

impl PluginSet {
    pub fn new() -> Self;
    pub fn with(self, plugin: impl Plugin) -> Self;

    /// 检查依赖图：插件 ID 不重复；每个服务的稳定 ID 只对应一个 TypeId 和一种 MULTIPLE；
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

#[derive(Clone)]
pub struct Activity { /* Weak<Kernel> + 插件 ID */ }

impl Activity {
    /// 声明有尚未完成的工作（通常是交给 executor 的任务）。守卫移进任务，任务结束时 drop。
    pub fn busy(&self) -> BusyGuard;
}

/// `Send + Sync`，`app` 持有 `Arc<Kernel>`。
pub struct Kernel { /* ... */ }

impl Kernel {
    /// 建表、读取清单、检查依赖图，然后激活所有 AfterFirstFrame 插件（按依赖顺序）。
    /// `app` 在第一帧真正显示（`presentedTime` 回调）之后调用；首帧之前不调用 kernel 的任何函数（ADR 0017）。
    pub fn start(plugins: PluginSet) -> Result<Arc<Kernel>, Vec<GraphError>>;

    /// 项目模型就绪或变化时由 `project` 插件调用：激活匹配的 Workspace 插件。
    pub fn workspace_changed(&self, facts: Arc<dyn WorkspaceFacts>);

    /// 激活清单里带 `Manual` 的插件。已激活时立即返回。
    pub fn activate(&self, id: &str);

    /// 命令第一次被触发前由视图层调用：激活声明了这个命令的插件。已激活时立即返回。
    pub fn command(&self, name: &str);

    /// 用户在设置里停用或重新启用插件。停用会连带停用依赖它所提供服务的插件。
    pub fn set_enabled(&self, id: &str, enabled: bool);

    /// 退出时按依赖的相反顺序停用全部插件。
    pub fn shutdown(&self);

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
    pub id: &'static str,
    pub state: PluginState,                 // Inactive / Active / Disabled / Failed(PluginError)
    pub activation_time: Option<Duration>,  // 最近一次 activate 的耗时
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
- 一个服务的最后一个提供者停用时，依赖它的插件先停用（依赖者先于被依赖者）。
- `MULTIPLE = true` 的服务：依赖者激活后，提供者的增减不重新激活依赖者。依赖者通过 `ServiceSet<S>` 读取当前集合，集合在提供者增减时随即更新；需要在变化时做额外处理的，订阅 `ServiceAdded<S>` 和 `ServiceRemoved<S>` 事件（带提供者的插件 ID）。
- 依赖者不得把 `ServiceSet::current()` 里的 `Arc<S>` 保存到本次调用之外，否则提供者停用后仍会被调用。
- 激活条件满足后保持有效：`Workspace` 和 `Command` 一旦满足，在本次运行中一直视为满足。P0 只有一个窗口；多窗口时按项目分别计算，届时修订本节。

### 激活的提交

- `activate` 执行期间，`provide`、`on`、`own` 登记的内容只暂存在本次激活里：服务不进入 `ServiceSet` 和 `service()` 的结果，监听不参与分发。
- `activate` 返回 `Ok` 后，kernel 在一次加锁里把暂存的服务和监听全部发布，然后按依赖顺序激活后续插件。
- 返回错误或 panic 时，暂存的内容直接丢弃（守卫按相反顺序 drop），已在运行的插件从头到尾看不到这次激活注册的任何东西。
- 视图层的注册由提供守卫的服务负责同样的语义：`dyn Contributions` 在下一帧才把新注册的视图、Action、快捷键应用到界面上，激活失败时守卫在这之前已经 drop，失败的注册不会出现在界面上。

### 撤销

每个已激活的插件有一张注册表，保存它提供的服务、它的事件监听，以及它通过 `own` 交出的守卫。

停用时按以下顺序撤销：

1. 依赖它所提供的单实现服务的插件先停用（见上一节）。
2. 移除它提供的服务：`ServiceSet` 的快照更新，`service()` 查不到，之后不会有新的调用到达。已经在进行的调用可以执行完；服务实现要能在插件停用过程中返回错误，而不是假定自己的守卫都还在。
3. 移除它的事件监听，并等待它正在执行的监听返回（见「事件分发」）。
4. 按与登记相反的顺序 drop 守卫。每个守卫的 drop 单独用 `catch_unwind` 包住：某个守卫的 drop panic 时，记录错误、插件标为 `Failed`，然后继续 drop 剩下的守卫，撤销总是完整执行。

视图层的注册也走守卫。例如 `workspace` 提供一个服务 `dyn Contributions`（接口在 `services`），它的 `register_view` / `register_action` / `bind_key` 返回一个守卫，drop 时注销；插件把守卫交给 `ctx.own`。kernel 因此不依赖 GPUI 的类型。守卫的 drop 若需要在主线程执行，由提供守卫的一方负责转到主线程。

后台任务也走守卫：`executor` 的任务句柄 drop 时取消任务，插件调用 `ctx.own(handle)` 交出句柄。停用后，任务不再向视图交出结果。

### 失败

- `activate` 返回错误或 panic：
  - 这次激活已登记的内容全部撤销；
  - 插件进入 `Failed`，依赖它的插件不激活；
  - 记录错误，设置页显示原因；
  - 其他插件不受影响。
- panic 由 `catch_unwind` 捕获，所以发布构建保持 `panic = "unwind"`（当前默认）。改成 `abort` 前，先修改本节的设计。
- 事件监听里的 panic 同样被捕获：这个监听被移除，插件进入 `Failed` 并停用。
- `Failed` 的插件在本次运行中不再自动激活；用户在设置里重新启用会重试一次。

### 事件分发

- `emit` 在调用线程上同步分发，按订阅顺序调用监听。
- 分发前先取出监听列表的快照（`Arc<[Handler]>`），不持注册表的锁调用。监听里可以订阅、退订、发事件：
  - 订阅与退订从下一次 `emit` 起生效；
  - 监听里发出的事件排在当前分发之后，在同一线程上依次处理，不会递归。
- 快照里可能有已停用插件的监听。每个监听带上所属插件的存活标记（读写锁保护的布尔值）：
  - 调用前取读锁并检查标记，插件已停用就跳过，调用期间一直持有读锁；
  - 停用时第 3 步取写锁把标记置为 false，因此会等这个插件正在执行的监听返回，此后快照里它的监听都会被跳过；
  - 监听里请求的生命周期操作（包括停用自己所属的插件）排队到当前分发结束后执行，不会在持有读锁时等待写锁；
  - 监听 panic 时，先释放读锁，再按「失败」一节停用插件。
- 监听必须快速返回：只更新状态或把工作交给 executor。事件总线用于插件之间的通知，如会话新增一轮、项目模型变化；视图内部的状态变化用 GPUI 自身的订阅。
- 没有监听的事件直接丢弃，不缓存。

### 线程

- `Kernel` 的生命周期操作（激活、停用、`set_enabled`、`shutdown`）在内部串行执行：同一时间只有一个。操作期间再次触发的生命周期请求（比如激活时发出的事件导致另一个插件需要激活），以及事件分发期间监听里发出的请求，都排队，在当前操作或分发完成后在同一线程上执行。
- `app` 在主线程上调用 `start`、`command`；`workspace_changed` 可以在任何线程调用。

### 静止

[ADR 0017](../adr/0017.md) 的内存基准和 UI 探针要等「kernel 发出已静止信号」再采样。kernel 在以下条件同时成立时为静止：

- `start` 已经返回；
- 生命周期操作的队列为空，没有正在执行的 `activate`；
- 没有未 drop 的 `BusyGuard`。

插件把初始化工作（扫描会话目录、打开 SQLite、建索引）交给 executor 时，用 `Activity::busy()` 取一个守卫移进任务，任务结束时守卫随之 drop。`Activity` 在 `activate` 里取得并保存，所以打开项目、收到事件之后开始的工作也能标记为忙碌。kernel 不知道 executor 的存在，只计数守卫。周期性或长期运行的任务（文件监听、定时刷新）不取守卫。插件停用时，它尚未 drop 的守卫不再计入。

每次从忙碌变为静止，kernel 发出一次 `Quiescent` 事件：启动后第一次静止用于空窗口的内存基准，打开项目触发的激活与初始化结束后再次静止，用于 monorepo 的内存基准。超时（ADR 0017 规定首帧后 30 秒内不静止即失败）由 bench 判断。
- `activate` 在调用生命周期操作的线程上执行。它可能在主线程上执行，所以不得做 IO 和解析（AGENTS.md 禁止事项）。是否超出预算由 bench 检查（见「性能」）。

## 性能

- 首帧之前不调用 kernel 的任何函数：`Kernel::start`（建表、读清单、检查依赖图、激活）在第一帧显示之后执行。
- 激活耗时：每个插件的 `activate` 由 kernel 计时，结果在 `status()` 里。建议预算是单个插件 1 ms、首帧后 `AfterFirstFrame` 全部激活合计 5 ms。数字在 [#7](https://github.com/SoloJiang/lectern-app/issues/7) 里用原型实测后校准，并写进 bench 的预算表。
- 常驻内存在同一进程里无法准确地按插件区分。bench 用只含一个插件（及其依赖）的 `PluginSet` 启动，所得 `phys_footprint` 减去空集合的值；`PluginSet` 支持这种组合。
- 服务查找是一次读锁加一次哈希表查找；插件应在 `activate` 里取出并保存需要的 `Arc`，使用时直接用保存的 `Arc`。
- 事件分发不分配内存（监听快照是 `Arc` 克隆），没有监听时只有一次查表。

## 测试

全部测试在 Linux 上运行（`just test -p lectern-kernel`）：

- 依赖图：`validate` 对缺提供者、重复插件 ID、两个服务共用一个稳定 ID、单实现服务有两个提供者、空的激活条件、环，分别给出对应的错误。
- 激活的提交：`activate` 先 `provide` 再返回错误时，并发的依赖者和事件分发都看不到这个提供者和它的监听。
- 自动启停：
  - A 依赖 B，B 依赖 C：激活顺序是 C、B、A；
  - 停用 C 连带停用 A、B，停用顺序是 A、B、C；
  - 重新启用 C 后 B、A 恢复；
  - `MULTIPLE` 服务的提供者增减时，依赖者不重新激活，`ServiceSet::current()` 随即反映变化，并发出 `ServiceAdded` / `ServiceRemoved`；
  - 停用一个多实现服务的提供者后，依赖者通过 `ServiceSet` 不再调用到它。
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
- 静止：`BusyGuard` 未 drop 时不静止；全部 drop 后发出一次 `Quiescent`；之后有新的激活，或已激活的插件通过保存的 `Activity` 取了新守卫时，重新变为忙碌，再次静止时再发一次；插件停用后它的守卫不再计入。
- 激活条件：
  - `Workspace` 用模拟的 `WorkspaceFacts` 触发；
  - `Command` 第二次调用不重复激活；
  - `Manual` 插件只在 `Kernel::activate(id)` 时激活。
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
