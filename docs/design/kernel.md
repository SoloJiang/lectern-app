# kernel 设计

对应 issue [#8](https://github.com/SoloJiang/lectern-app/issues/8)。kernel 负责插件装载、服务注册表、事件总线和生命周期，本身不含任何业务功能（[ADR 0010](../adr/0010.md)、[技术调研「插件架构」](tech-research.md#插件架构)）。本文同时定义 `services` crate 与 kernel 的分工。

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

  本设计为它们留出的扩展点：插件清单是运行时数据而不是关联常量；服务键带稳定的字符串 ID。

## 依赖

| 依赖 | 用途 | 为什么是它 |
|---|---|---|
| `parking_lot` | 注册表的锁 | 小，无 poison；GPUI 的依赖图里已有，发布构建不增加体积 |
| `thiserror` | 错误类型 | 已在依赖图里 |

kernel 只依赖 kernel 层（架构规则 1），不依赖 `executor`、`platform` 或任何业务 crate。

## 概念

| 概念 | 说明 |
|---|---|
| 插件 | 实现 `Plugin` 的类型，由 `app` 放进 `PluginSet`。一个插件 crate 通常只有一个插件。 |
| 清单 | 插件的 ID、提供的服务、依赖的服务、激活条件。静态声明，启动前就能检查依赖图。 |
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

/// 运行时的服务键：TypeId 加稳定 ID。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ServiceKey { /* type_id: TypeId, id: &'static str */ }

pub fn service<S: Service + ?Sized>() -> ServiceKey;
```

### 插件

```rust
pub struct Manifest {
    /// 全局唯一，如 "lang-java"、"session-claude-code"。
    pub id: &'static str,
    pub provides: Vec<ServiceKey>,
    pub requires: Vec<ServiceKey>,
    /// 任一条件满足即可激活；为空表示不会自动激活。
    pub activation: Vec<Activation>,
}

pub enum Activation {
    /// 首帧之后立即激活。审阅闭环必需的插件用这一项。
    AfterFirstFrame,
    /// 当前打开的项目里有匹配的文件时激活，如 &["**/*.java", "pom.xml", "build.gradle*"]。
    Workspace(&'static [&'static str]),
    /// 用户第一次触发这个命令时激活，如 "terminal.open"。
    Command(&'static str),
}

/// 清单用方法返回、而不是关联常量：以后的进程插件从安装目录里的清单文件读出它。
pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> Manifest;

    /// 在调用线程上同步执行，只做注册，不做 IO 和解析：重活交给 executor，
    /// 任务句柄用 `ctx.own` 交给 kernel。耗时计入本插件的激活预算。
    fn activate(&self, ctx: &mut Context<'_>) -> Result<(), PluginError>;
}
```

### 激活时的上下文

```rust
impl Context<'_> {
    /// 提供服务。key 必须在清单的 provides 里，否则返回错误。
    pub fn provide<S: Service + ?Sized>(&mut self, service: Arc<S>) -> Result<(), PluginError>;

    /// 查找依赖的服务。key 必须在清单的 requires 里：依赖都写进清单，才能在启动前检查。
    pub fn service<S: Service + ?Sized>(&self) -> Arc<S>;          // MULTIPLE = false
    pub fn services<S: Service + ?Sized>(&self) -> Vec<Arc<S>>;    // MULTIPLE = true

    /// 订阅事件。
    pub fn on<E: Event>(&mut self, handler: impl Fn(&E) + Send + Sync + 'static);

    /// 交给 kernel 保管的任何东西：视图层注册返回的守卫、executor 的任务句柄等。
    /// 停用时按与登记相反的顺序 drop。
    pub fn own(&mut self, guard: impl Send + 'static);

    /// 发事件的句柄，可以克隆后在后台任务里使用。
    pub fn emitter(&self) -> Emitter;
}
```

### 内核

```rust
pub struct PluginSet { /* Vec<Arc<dyn Plugin>> */ }

impl PluginSet {
    pub fn new() -> Self;
    pub fn with(self, plugin: impl Plugin) -> Self;

    /// 检查依赖图：ID 不重复；每个 requires 在本集合里都有提供者；
    /// MULTIPLE = false 的服务只有一个提供者；没有环。
    /// `app` 为每个 profile 写一个调用它的单元测试，依赖图出错在 CI 里就失败。
    pub fn validate(&self) -> Result<(), Vec<GraphError>>;
}

/// `Send + Sync`，`app` 持有 `Arc<Kernel>`。
pub struct Kernel { /* ... */ }

impl Kernel {
    /// 只建数据结构，不激活任何插件。可以在首帧之前调用，耗时在微秒级。
    pub fn new(plugins: PluginSet) -> Result<Kernel, Vec<GraphError>>;

    /// 首帧之后由 `app` 调用：激活所有 AfterFirstFrame 插件（按依赖顺序）。
    pub fn first_frame_done(&self);

    /// 项目模型就绪或变化时由 `project` 插件调用：激活匹配的 Workspace 插件。
    pub fn workspace_changed(&self, facts: Arc<dyn WorkspaceFacts>);

    /// 命令第一次被触发前由视图层调用：激活声明了这个命令的插件。已激活时立即返回。
    pub fn command(&self, name: &str);

    /// 用户在设置里停用或重新启用插件。停用会连带停用依赖它所提供服务的插件。
    pub fn set_enabled(&self, id: &str, enabled: bool);

    /// 退出时按依赖的相反顺序停用全部插件。
    pub fn shutdown(&self);

    pub fn emitter(&self) -> Emitter;
    pub fn status(&self) -> Vec<PluginStatus>;   // 设置页和 bench 读取
}

pub trait WorkspaceFacts: Send + Sync {
    /// 当前项目里是否有匹配 pattern 的文件。由 `project` 实现，读 git index，不遍历目录。
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
- `MULTIPLE = true` 的服务：依赖者激活后，新增的提供者通过 `ServiceAdded<S>` 事件通知，不重新激活依赖者。
- 激活条件是粘性的：`Workspace` 和 `Command` 一旦满足，在本次运行中一直满足。P0 只有一个窗口；多窗口时按项目分别计算，届时修订本节。

### 撤销

每个已激活的插件有一张注册表，按登记顺序保存：

1. 它提供的服务；
2. 它的事件监听；
3. 它通过 `own` 交出的守卫。

停用时按相反顺序撤销：先 drop 守卫，再移除事件监听，最后移除服务。

视图层的注册也走守卫。例如 `workspace` 提供一个服务 `dyn Contributions`（接口在 `services`），它的 `register_view` / `register_action` / `bind_key` 返回一个守卫，drop 时注销；插件把守卫交给 `ctx.own`。kernel 因此不需要知道 GPUI 的类型。守卫的 drop 若需要在主线程执行，由提供守卫的一方负责转到主线程。

后台任务同理：`executor` 的任务句柄 drop 时取消任务，插件 `ctx.own(handle)` 即可。停用后，任务不会再向视图交出结果。

### 失败

- `activate` 返回错误或 panic：
  - 这次激活已登记的内容全部撤销；
  - 插件进入 `Failed`，依赖它的插件不激活；
  - 记录错误，设置页显示原因；
  - 其他插件不受影响。
- panic 用 `catch_unwind` 截住，所以发布构建保持 `panic = "unwind"`（当前默认）。改成 `abort` 前，要先改这里的设计。
- 事件监听里的 panic 同样截住：这个监听被移除，插件进入 `Failed` 并停用。
- `Failed` 的插件在本次运行中不再自动激活；用户在设置里重新启用会重试一次。

### 事件分发

- `emit` 在调用线程上同步分发，按订阅顺序调用监听。
- 分发前先取出监听列表的快照（`Arc<[Handler]>`），不持锁调用。监听里可以订阅、退订、发事件：
  - 订阅与退订从下一次 `emit` 起生效；
  - 监听里发出的事件排在当前分发之后，在同一线程上依次处理，不会递归。
- 监听必须快：只更新状态或把工作交给 executor。事件总线用于插件之间的通知，比如会话有了新的一轮、项目模型变化；视图内部的状态变化用 GPUI 自己的订阅。
- 没有监听的事件直接丢弃，不缓存。

### 线程

- `Kernel` 的生命周期操作（激活、停用、`set_enabled`、`shutdown`）在内部串行执行：同一时间只有一个。操作期间再次触发的生命周期请求（比如激活时发出的事件导致另一个插件需要激活）排队，当前操作完成后在同一线程上执行。
- `app` 在主线程上调用 `first_frame_done`、`command`；`workspace_changed` 可以在任何线程调用。
- `activate` 在调用生命周期操作的线程上执行。因为它可能在主线程上，所以不能做 IO 和解析（AGENTS.md 禁止事项）。是否超出预算由 bench 检查（见「性能」）。

## 性能

- 首帧之前只有 `Kernel::new`：建表和检查依赖图，不调用任何 `activate`。
- 激活耗时：每个插件的 `activate` 由 kernel 计时，结果在 `status()` 里。建议预算是单个插件 1 ms、首帧后 `AfterFirstFrame` 全部激活合计 5 ms。数字在 [#7](https://github.com/SoloJiang/lectern-app/issues/7) 里用原型实测后校准，并写进 bench 的预算表。
- 常驻内存：同一进程里无法准确地按插件区分。bench 的做法是用只含一个插件（及其依赖）的 `PluginSet` 启动，与空集合的 `phys_footprint` 相减，`PluginSet` 支持这种组合。
- 服务查找是一次读锁加一次哈希表查找；插件应在 `activate` 里取出需要的 `Arc` 并保存，而不是每次使用时查找。
- 事件分发不分配内存（监听快照是 `Arc` 克隆），没有监听时只有一次查表。

## 测试

全部在 Linux 上跑（`just test -p lectern-kernel`）：

- 依赖图：`validate` 对缺提供者、重复 ID、单实现服务有两个提供者、环，分别给出对应的错误。
- 自动启停：
  - 依赖链 A → B → C，按顺序激活；
  - 停用 C 连带停用 B、A，顺序是 A、B、C；
  - 重新启用 C 后 A、B 恢复；
  - `MULTIPLE` 服务新增提供者时只发 `ServiceAdded`，依赖者不重新激活。
- 撤销：用记录 drop 顺序的守卫，检查服务、监听、守卫按登记的相反顺序撤销，撤销后服务查不到、事件收不到。
- 失败：
  - `activate` 返回错误或 panic 时，部分注册被撤销，插件进入 `Failed`；
  - 依赖者不激活；
  - 其他插件照常。
- 事件：
  - 监听里订阅、退订、发事件的顺序语义；
  - 多线程同时 `emit`、同时触发生命周期操作时不死锁（压力测试）。
- 激活条件：
  - `Workspace` 用模拟的 `WorkspaceFacts` 触发；
  - `Command` 第二次调用不重复激活；
  - 激活条件为空的插件不会自动激活。
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

技术调研里的插件接口示意用关联常量、`type Config` 和 `fn apply(ctx, cfg)`。本设计改为：

- 清单用 `manifest(&self)` 返回运行时数据，进程插件以后可以用同一个类型。
- 不在 P0 引入插件配置：配置等设置页的设计一起做，那时以服务的形式提供（`dyn Settings`），不改 `Plugin` trait。
- 去掉「首次请求服务时激活」：查找时同步激活会让查找的耗时不可预测，还会在激活中途再次进入生命周期操作。需要延迟加载的插件用 `Command` 或 `Workspace` 条件。

## 未决问题

无需产品决定的问题。实现中如果发现预算或线程模型需要改，先改本文。

## 验收

- `PluginSet::validate`、自动启停、撤销、失败隔离、事件分发的测试全部通过，结果有 insta 快照。
- kernel 不依赖 GPUI 和 kernel 层以外的 crate（`just check` 的架构检查通过）。
- `TestKernel` 能注入模拟服务并单独激活一个插件。
- `status()` 给出每个插件的激活耗时，供 #7 的 bench 读取。
