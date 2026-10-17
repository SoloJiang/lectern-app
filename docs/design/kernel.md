# kernel 设计

状态：已实现（[#58](https://github.com/SoloJiang/lectern-app/pull/58)）。

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
kernel 只用标准库，不引入新的外部依赖：锁用 `std::sync::RwLock` / `Mutex`，错误类型手写 `Display` 和 `Error`。标准库的锁在持有写锁的线程 panic 时会标记 poison；kernel 只在 kernel 线程上短暂持有写锁，并且总在 `catch_unwind` 之外，读到 poison 时用 `into_inner` 取回数据继续运行。以后需要引入外部依赖（如 `parking_lot`）时先写 ADR。

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
// 服务方法出错时返回 `Err`，不得 panic：调用方拿到的是 trait 对象，kernel 不在每次调用外面包 `catch_unwind`。
// 服务方法真的 panic 时，展开到调用方，由调用方所在的边界处理（在另一个插件的监听或 `started` 里时，算作那个插件失败）。
// 服务实现的测试覆盖错误路径。`Admitted` 的 `Drop` 用 `std::thread::panicking()` 判断是否正在展开，是时在日志里记下
// 「展开时持有哪个提供者的准入」，只作为排查的上下文：panic 可能来自服务方法，也可能来自调用方自己的代码，日志不据此判定来源。

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
/// 插件对象由 `PluginSet` 交给 kernel，`shutdown` 时在 kernel 线程上用 `catch_unwind` 逐个 drop；
/// 实现的 `Drop` 同样不得 panic（与服务、监听闭包、事件值的要求相同），`Kernel` 在其他线程上被 drop 时不再持有插件对象。
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
    /// 两个方法各自在函数体里用 `const { assert!(...) }` 检查 `S::MULTIPLE`：`service` 只接受 MULTIPLE = false 的服务，
    /// `service_set` 只接受 MULTIPLE = true 的服务，用错时编译失败，不会在运行时拿到某一个提供者的 `Arc`。
    pub fn service<S: Service + ?Sized>(&self) -> ServiceRef<S>;        // MULTIPLE = false
    pub fn service_set<S: Service + ?Sized>(&self) -> ServiceSet<S>;    // MULTIPLE = true

    /// 订阅事件。监听在提交时发布，但这次激活的 token 变为 `Live`（所有 `commit` 和 `started` 都成功）之前，
    /// 分发时跳过它，这段时间里发出的事件这个插件收不到；需要初始状态的插件在 `started` 里主动读取。
    /// 每次调用都带上本次激活的 `PluginHandle`，监听里可以直接取 `busy`、`emitter`。
    pub fn on<E: Event>(&mut self, handler: impl Fn(&E, &PluginHandle) + Send + Sync + 'static);

    /// 交给 kernel 保管的守卫。只用于创建时没有副作用的守卫；后台任务在 `started` 或之后通过 `PluginHandle::own_with` 提交，
    /// 不先提交再交出句柄（见「撤销」）。
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
pub struct ActivationToken { /* Arc<{ AtomicU8 状态, AtomicUsize 准入计数 }> */ }

impl ActivationToken {
    /// `Pending`：激活还没结束；`Live`：已完全成功且尚未停用；`Dead`：失败或已停用。
    /// 状态只会 `Pending → Live`、`Pending → Dead`、`Live → Dead`：kernel 用 compare-exchange 把 `Pending` 换成 `Live`，
    /// 换失败（其他线程已经写入 `Dead`）就按激活失败处理；`Dead` 不会再变回其他状态。
    pub fn state(&self) -> TokenState;

    /// 取得调用这次激活的代码的准入：先把准入计数加一，再检查状态，是 `Live` 时返回 `Admission`，
    /// 否则减回去并返回 None。加一之前先读一次状态，已经不是 `Live` 时直接返回 None、不碰计数，所以通过旧句柄反复调用不会让全局计数短暂变化；
    /// 只有与写入 `Dead` 同时发生的那一次可能先加后减，而那时停用请求已在队列里，kernel 本来就是忙碌的。调用插件代码（监听、界面回调、主线程上应用任务结果、通过 `ServiceSet` 调用服务）
    /// 期间一直持有，drop 时减一。不阻塞，同一线程上嵌套取得也不会死锁。停用时 kernel 先把状态置为 `Dead`，
    /// 再等准入计数归零（见「撤销」第 3 步），所以 `Dead` 之后不会有新的调用开始，已经开始的调用在守卫和 `Active`
    /// 被 drop 之前执行完。
    pub fn enter(&self) -> Option<Admission>;

    /// 取一个由 kernel 直接计数的守卫，表示还有排到其他线程、尚未完成的工作（主线程上应用或注销界面注册）。
    /// 守卫 drop 之前 kernel 不报告静止。它不属于任何插件的激活，所以提供注册的插件（如 `workspace`）
    /// 自己正在停用时也能取到；token 为 `Dead` 时同样能取到。`shutdown` 在最后一次确认计数为零时，
    /// 用同一个原子操作关闭准入（计数和「已关闭」位在同一个原子字里），之后返回 None，调用方直接丢弃要排队的工作；
    /// `shutdown` 自己撤销插件时取的守卫在关闭之前取得，不受影响。
    pub fn pending(&self) -> Option<PendingGuard>;
}

pub enum TokenState { Pending, Live, Dead }

/// 一次 `own_with` 提交的闸门，由 kernel 定义，`executor` 依赖它（基础设施层可以依赖 kernel 层）。
/// executor 为提交的每个任务向闸门申请一个 `GateSlot`，之后每次 poll 任务的 future 之前先调用 `GateSlot::poll`，按结果处理：
/// `Run` 才 poll future；`Wait`（闸门还没打开）返回 `Pending`；`Drop`（闸门关闭：`start` panic、激活失效，或 token 为 `Dead`）丢弃 future。
/// 闸门打开或关闭、token 变为 `Dead` 时 kernel 唤醒全部槽位里的 waker，所以挂起中的任务（等闸门打开，
/// 或闸门已开、future 自己返回了 `Pending`）也会被立即调度一次，看到关闭后被丢弃，不会一直挂着守卫和状态。
pub struct SubmitGate { /* Arc<{ Mutex<{ 状态, 槽位里的 waker }>, ActivationToken, Weak<Kernel>, 插件 ID }> */ }

/// `executor` 对 kernel 的约定：`Waker::wake` 只把任务放回运行队列，由工作线程之后 poll，不在调用 `wake` 的线程上同步 poll
/// 或丢弃 future（与 tokio、async-executor 等常见执行器相同），有测试覆盖。所以 kernel 在任何线程上唤醒任务（包括在持有插件锁的
/// 监听、界面回调里调用 `fail`）都不会在当前调用栈里运行或销毁插件的 future。
impl SubmitGate {
    /// executor 在提交任务的调用里同步申请槽位（`spawn` 返回之前），不推迟到别的线程。`start` 返回时 `own_with` 在闸门的锁内把它封住，
    /// 之后再申请返回 None，executor 不 poll 这个任务、在 `contain` 里直接丢弃；这次丢弃 panic 时调用 `SubmitGate::panicked(message)`，
    /// 按与 `GateSlot::panicked` 相同的方式让所属激活失效（闸门持有 token 和插件 ID），不会因为没有槽位就漏报。所以 `start` 返回之后，这次提交的任务集合就固定了，
    /// 闸门「已封住、槽位归零」时登记才可以移除，不会有迟到的任务在登记移除之后申请到槽位、在没有句柄的情况下运行。
    pub fn slot(&self) -> Option<GateSlot>;

    /// 没有申请到槽位的任务在丢弃时 panic，executor 用它报告，效果与 `GateSlot::panicked` 相同。
    pub fn panicked(&self, message: String);
}

/// 一个任务在闸门上的槽位，存放这个任务最近一次的 waker，drop 时从闸门移除。
pub struct GateSlot { /* Arc<闸门>, 槽位序号 */ }

pub enum GatePoll { Run, Wait, Drop }

impl GateSlot {
    /// 一个原子操作，不拆成「先查状态、再登记」：在闸门的互斥锁内先把 `cx.waker()` 存进槽位（替换上一次的），
    /// 再读闸门状态和 token，然后才释放锁。kernel 改变状态时先写入新状态（闸门打开或关闭在同一把锁内写，
    /// token 的 `Dead` 是原子写），再取这把锁、把全部槽位里的 waker 克隆出来，释放锁之后才逐个调用 `wake`（`wake` 按下面的约定只把任务放回运行队列，不在锁内调用是额外的保护）。所以检查和状态变化无论怎样交错，
    /// 这次 `poll` 要么已经看到新状态，要么它登记的 waker 一定会被唤醒，不会漏掉唤醒、一直停在 `Pending`。
    pub fn poll(&self, cx: &mut std::task::Context<'_>) -> GatePoll;

    /// executor 用 `kernel::catch` 包住每次 poll（不用 `contain`：`contain` 在返回之前就处理掉 payload，executor 来不及先 drop future）。
    /// panic 时 `catch` 返回 `Err(PanicPayload)`，payload 原样保留，顺序固定：先把任务标为已结束，在 `contain` 里 drop 它的 future
    /// （future 持有的锁、许可等资源随之释放；payload 的 `Drop` 可能要等这些资源），再调用 `PanicPayload::dispose` 取出消息、释放 payload
    /// （`panic_any` 抛出的 payload 也可能持有许可等资源），最后才用这条消息调用这个方法；所以这个方法唤醒的同一闸门上的其他任务不会卡在仍被这个 future 或它的 payload 占着的资源上。
    /// future 的每一次最终 drop 都用 `catch_unwind` 包住，不只是 poll panic 之后：`poll` 返回 `Drop`（取消、闸门关闭、token 为 `Dead`）、
    /// future 返回 `Ready` 之后、executor 关闭时丢弃未完成的任务，都一样；drop panic 时同样调用这个方法。
    /// kernel 按 `fail` 的方式让这个任务所属的激活失效：插件进入 `Failed`，连带停用的插件同步失效，
    /// 所以任务 panic 不会展开到 executor 的工作线程，也不会让插件在初始化中断后仍显示为 `Active`、被报告为静止。
    /// 这个方法先把当前槽位从闸门上摘掉（不再参与之后的唤醒），然后才让激活失效、唤醒其余槽位：当前任务还在唤醒名单里时，
    /// executor 会把这个正在展开或已部分 drop 的任务重新放进运行队列；摘掉之后，这个任务只由 executor 在 panic 路径上收尾丢弃。
    /// 摘掉槽位撤不回别的线程已经克隆走的 waker，所以不能只靠它：`executor` 对每个任务维护「空闲 / 运行中 / 已结束」三种状态，
    /// 唤醒一个运行中的任务只设一个「待重新 poll」标记，由正在运行的那次 poll 返回后再调度，唤醒已结束的任务直接忽略，
    /// 所以任何时候一个任务都不会被重入 poll，panic 路径上的任务在标为已结束之后也不会再被 poll。这是 `executor` 对 kernel 的约定，有测试覆盖。
    /// 激活已经失效时只记日志。这个方法只接收消息，不接收 payload，调用它时 payload 已经释放。
    /// future 的 drop 不需要先做别的事，executor 直接用 `kernel::contain` 包住。
    pub fn panicked(&self, message: String);
}

/// `catch` 捕获到的 panic payload。
pub struct PanicPayload { /* Box<dyn Any + Send> */ }

impl PanicPayload {
    /// 取出可读的消息，然后释放 payload。`panic!` 产生的 payload 只有 `&'static str` 和 `String` 两种，`Drop` 不会阻塞也不会 panic，
    /// 在调用线程上直接释放；其他类型（`panic_any` 传入的自定义值）的 `Drop` 可能 panic，也可能等某个资源，每个交给一个新建的回收线程 drop，
    /// 不在调用线程上 drop，几个 payload 之间也互不等待：它只会卡住或展开自己的回收线程（在 `catch_unwind` 里 drop，
    /// 第二个 panic 的 payload 用 `mem::forget` 泄漏），executor 的工作线程、kernel 线程、主线程和其他 payload 的释放都不等它。所以调用方拿到消息后
    /// 可以立刻让激活失效、唤醒其他任务，不必安排 payload 和 future 的释放顺序。`PanicPayload` 自己的 `Drop` 做同样的事。
    pub fn dispose(self) -> String;
}

/// 用 `catch_unwind` 运行 `f`，panic 时保留 payload 返回。
pub fn catch<T>(f: impl FnOnce() -> T) -> Result<T, PanicPayload>;

/// 等于 `catch(f).map_err(PanicPayload::dispose)`：捕获到 panic 时按上面的方式释放 payload，只返回消息。kernel 的其余边界都用它（见「失败与 panic」）。
pub fn contain<T>(f: impl FnOnce() -> T) -> Result<T, String>;

/// 一次激活的长期句柄：从 `started` 起有效，到这次激活停用为止。可以克隆并保存在插件的状态里，
/// 用于激活之后开始的工作（打开项目后的扫描、收到事件后提交的任务）。
/// 句柄带激活代数：插件停用后（包括停用后又重新激活），旧句柄上的操作都不再生效。
#[derive(Clone)]
pub struct PluginHandle { /* Weak<Kernel>, 插件 ID, 激活代数 */ }

impl PluginHandle {
    /// 交给 kernel 保管，停用时 drop。检查和插入在这次激活的守卫表的互斥锁内完成，与 `on`、`own_with` 相同：
    /// 撤销第 4 步先把表标为关闭再取出全部守卫，所以 `own` 要么在关闭之前插入（随撤销 drop），要么看到关闭返回 false，不会在 `Active` drop 之后留下守卫。
    /// 这次激活已停用时返回 false，释放锁之后在调用线程上于 `contain` 里当场 drop `guard`，不排到 kernel 线程：调用方可能正持有准入，
    /// 撤销第 3 步在等它，排队的话守卫会晚于 `Active` 才 drop。
    /// `shutdown` 完成、kernel 线程退出之后，被拒绝的守卫在调用线程上 drop，所以守卫必须能在任意线程上安全 drop；
    /// 这次 drop 同样用 `catch_unwind` 包住，panic 只记日志，不会展开到调用方的线程；
    /// kernel 线程只保证 `shutdown` 之前的 drop 是串行的。
    pub fn own(&self, guard: impl Send + 'static) -> bool;

    /// 先检查这次激活仍然存活，再调用 `start` 提交工作（如 `executor.spawn`），把返回的句柄交给 kernel 保管。
    /// token 为 `Pending`（在 `started` 里）时不取准入、不走下面三步，直接按本说明末尾的方式暂存 `start`，token 变为 `Live` 后由 kernel 线程按下面的步骤执行。
    /// 否则整个调用持有这次激活的一个准入（第 1 步之前 `enter()`，取不到就按下面第 1 步的拒绝处理；第 3 步处理完句柄之后才释放），
    /// 所以 `start` 卡住期间停用第 3 步会等它：撤销第 4 步清空守卫表、drop `Active` 和 `shutdown` 完成都在这次调用把句柄登记或 drop 之后，
    /// 句柄不会在撤销之后才在调用线程上 drop。`enter()` 不阻塞，同一线程上嵌套取得也不会死锁。
    /// 分三步，都不等待准入计数，所以在监听里调用（此时已持有准入）也不会与等待准入归零的停用互相等待：
    /// 1. 取这次激活的守卫表的互斥锁，表已关闭或 token 为 `Dead` 时释放锁、不调用 `start`，在 `contain` 里 drop `start` 闭包（它捕获的值的 `Drop` 可能 panic），返回 false；否则新建一个关着的闸门，作为一条还没有句柄的临时登记放进守卫表，然后释放锁。闸门从这时起就在表里，所以 `start` 运行期间（任务已经提交、在等闸门、`start` 自己还没返回）发生的失效或撤销也能找到它：失效时唤醒它的槽位，撤销第 4 步把它关闭并取出，任务随即被丢弃，不会等 `start` 返回才被发现。
    /// 2. 不持锁调用 `start`，所以 `start` 里可以调用这个句柄的其他方法（`own`、`own_with`、`busy`），不会在同一把锁上死锁。
    /// 3. 再取这把锁：表仍未关闭、token 不是 `Dead` 时把返回的句柄填进第 1 步的临时登记，释放锁之后才打开闸门（任务的第一次 poll 可以在别的线程上调用这个句柄的 `own`、`own_with`，
    ///    持锁打开会让它们等这把锁），返回 true；
    ///    打开只把闸门从「未打开」换成「打开」，释放锁之后撤销已经关闭了闸门的，保持关闭；即使闸门已经打开，
    ///    撤销第 1 步写下的 `Dead` 也让 `GateSlot::poll` 返回 `Drop`，任务不会在撤销之后运行；
    ///    否则（`start` 运行期间 token 已变为 `Dead`；撤销还停在第 3 步等这次调用的准入，表还没有清空）关闭闸门、移除临时登记，释放锁后在 `contain` 里 drop 句柄，然后才释放准入，返回 false。
    ///    闸门一直关着，所以这时已经提交的任务一次也没运行，随闸门关闭被丢弃。
    /// 插件提交后台任务应使用这个方法，而不是先提交再 `own`。
    /// 句柄登记时带上闸门：这次提交的所有任务都结束后（每个任务的 `GateSlot` 在任务完成或被丢弃时 drop，闸门看到已封住且槽位归零），
    /// 闸门向 kernel 线程的队列放一个回收请求（激活代数加登记序号），不在 executor 线程上动守卫表；kernel 线程处理请求时，登记还在就移除它、
    /// 在 `contain` 里 drop 句柄，已经被撤销取走就什么也不做。所以句柄要么由撤销按顺序在 `Active` 之前 drop，要么由 kernel 线程回收，
    /// 不会在撤销或 `shutdown` 完成之后才在别的线程上 drop；kernel 线程已经退出时放不进请求，这时 `shutdown` 已经取走并 drop 了全部登记。所以反复提交一次性任务（扫描、刷新、收到事件后的工作）
    /// 时，守卫表只保留还在运行的任务，不会随激活的时长一直增长；`start` 一个任务也没提交（没申请槽位）时，第 3 步封住闸门后立即放回收请求。
    /// `start` 用保留 payload 的 `kernel::catch` 包住：panic 时先关闭闸门并唤醒槽位（已提交的任务由 executor 在工作线程上丢弃，
    /// 它们持有的资源随之释放），再 `dispose` payload，所以 payload 的 `Drop` 等这些资源时不会卡住；然后按 `fail` 的方式让这次激活失效
    /// （插件进入 `Failed`，连带停用的插件同步失效），返回 false，panic 不会展开到调用方。
    /// 在 `started` 里调用时（token 还是 `Pending`），`start` 暂存到 token 变为 `Live` 之后再调用，激活失败时丢弃，返回 true 表示已经接受。
    /// `start` 收到一个 `SubmitGate`，提交任务时交给 executor（见「撤销」里的后台任务）：任务在闸门打开之前不会被 poll，
    /// `own_with` 在 `start` 正常返回、句柄登记进守卫表之后才打开闸门；`start` panic 或激活失效时闸门随之关闭，任务直接丢弃。
    pub fn own_with<G: Send + 'static>(&self, start: impl FnOnce(SubmitGate) -> G + Send + 'static) -> bool;

    /// 订阅事件。返回的 `Subscription` drop 时退订（从下一次 `emit` 起生效）；
    /// 调用 `Subscription::keep()` 则一直保留到这次激活停用。这次激活已停用时不订阅并返回 None，
    /// 被拒绝的 `handler` 在释放守卫表的锁之后于 `contain` 里 drop（它捕获的值的 `Drop` 可能 panic），不展开到调用方的线程。
    /// 检查和插入都在这次激活的守卫表的互斥锁内完成，监听登记成这张表里的一项：撤销第 4 步取同一把锁、先把表标为关闭再移除全部监听，
    /// 所以 `on` 要么在关闭之前插入（随后被撤销移除），要么看到关闭返回 None，不会在撤销之后留下监听。
    pub fn on<E: Event>(&self, handler: impl Fn(&E, &PluginHandle) + Send + Sync + 'static) -> Option<Subscription>;

    /// 声明有尚未完成的工作，守卫移进任务，任务结束时 drop（见「静止」）。这次激活已停用时返回 None。
    /// 先取一个准入，取不到就返回 None：与 `enter()` 相同（先加计数再检查状态），但 `Pending` 也接受，所以在 `started` 里
    /// （token 还是 `Pending`）也能为初始化工作取到守卫；激活在 `started` 里失败时，撤销同样等这个准入归零。在准入内把忙碌计数加一、创建守卫，然后才释放准入。停用第 3 步要等准入归零，
    /// 所以守卫要么在撤销完成之前创建（随后的工作仍算在这次激活名下、计入静止），要么看到 `Dead` 返回 None，
    /// 不会在撤销之后通过旧句柄新开始工作；计数加一时全局准入计数已经不为零，不会多出一次忙碌转换。
    pub fn busy(&self) -> Option<BusyGuard>;

    /// 带激活代数的 Emitter：这次激活停用后（包括之后重新激活），通过它发出的事件直接丢弃；
    /// token 还是 `Pending`（在 `started` 里）时，事件暂存到 token 变为 `Live` 之后再发出，激活失败时丢弃。
    pub fn emitter(&self) -> Emitter;
    pub fn is_active(&self) -> bool;

    /// 这次激活的 `ActivationToken`，与 `Staged::commit` 收到的是同一个。`started` 之后的界面注册带上它，
    /// 主线程调用注册的回调前检查（见「撤销」）。
    pub fn token(&self) -> ActivationToken;

    /// 报告这次激活在后台初始化时失败（如打开 SQLite 失败）：插件进入 `Failed`，按「失败」一节停用。
    /// 与监听 panic 相同，返回之前先在调用线程上让这次激活和连带停用的插件失效（见「事件分发」）。
    /// 这次激活已停用时不做任何事。
    pub fn fail(&self, error: PluginError);

    /// 受限的 kernel 控制句柄，供需要驱动激活的插件使用（`project` 调 `workspace_changed`，
    /// `workspace` 调 `lazy_commands` 和 `command`）。插件在 `PluginSet` 里构造时还没有 `Arc<Kernel>`，
    /// 所以通过这里取得。不包含 `start`、`set_enabled` 和 `shutdown`，这些只给 `app`。
    /// 句柄带这次激活的插件 ID 和代数：这次激活停用后，通过它发出的请求都被忽略。
    /// `command` 的 `done` 在请求方这次激活的准入内调用（`enter()` 不阻塞）：请求方已经失效时（包括发出请求之后、目标插件还在激活时失效），
    /// `done` 不被调用，在 `contain` 里 drop，所以失效之后不会再运行它的代码，也不会经它排入重放。
    pub fn control(&self) -> KernelControl;
}

#[derive(Clone)]
pub struct KernelControl { /* Weak<Kernel>, 插件 ID, 激活代数 */ }

impl KernelControl {
    pub fn workspace_changed(&self, facts: Arc<dyn WorkspaceFacts>);
    pub fn activate(&self, id: &str);
    pub fn lazy_commands(&self) -> Vec<Name>;
    pub fn command(&self, name: &str, done: impl FnOnce(Result<ActivationToken, PluginError>) + Send + 'static);
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

/// 单实现服务的句柄，可以保存在 `Active` 里长期使用。每次调用前用 `get()` 取得提供者的准入，
/// 准入持有到返回的 `Admitted` 被 drop：提供者已失效时返回 None，提供者的撤销会等正在进行的调用结束。
#[derive(Clone)]
pub struct ServiceRef<S: Service + ?Sized> { /* 提供者的 Arc<S> 与它那次激活的 ActivationToken */ }

impl<S: Service + ?Sized> ServiceRef<S> {
    pub fn get(&self) -> Option<Admitted<S>>;
}

/// 持有一个提供者准入的服务引用，通过 `Deref` 调用服务。只在一次调用内使用，不要另外保存。
pub struct Admitted<S: Service + ?Sized> { /* Arc<S> + Admission */ }

/// 多实现服务的当前提供者集合。kernel 在提供者增减时整体替换内部的快照，
/// 所以句柄可以在 `activate` 里取一次并长期保存。
#[derive(Clone)]
pub struct ServiceSet<S: Service + ?Sized> { /* Arc<ArcSwap<[Arc<S>]>> 一类的共享快照 */ }

impl<S: Service + ?Sized> ServiceSet<S> {
    /// 当前提供者，每一项都持有那个提供者这次激活的 `Admission`，通过 `Deref` 调用服务。
    /// 只取 token 为 `Live` 的提供者：还在 `Pending`（`started` 尚未成功）的新提供者和已失效的提供者都不出现。
    /// 准入持有到返回的 `Vec` 被 drop 为止，提供者的撤销会等这些调用结束，所以只在一次调用内使用，不要另外保存。
    /// 每次调用分配一个 `Vec`，并对每个提供者各做一次准入。
    pub fn current(&self) -> Vec<Admitted<S>>;

    /// 依次对每个 `Live` 的提供者调用 `f`：不分配，同一时间只持有一个提供者的准入（调用 `f` 期间）。
    /// 热路径用这个方法。
    pub fn for_each(&self, f: impl FnMut(&S));
}

/// `Send + Sync`，`app` 持有 `Arc<Kernel>`。
pub struct Kernel { /* ... */ }

impl Kernel {
    /// 启动 kernel 线程并立即返回。kernel 线程上读取清单、建表、检查依赖图，然后激活所有
    /// AfterFirstFrame 插件（按依赖顺序）。`app` 在启动屏障放开后调用（见「启动」）。
    /// 依赖图有误时，涉及的插件进入 `Failed` 并记日志；`validate` 的单元测试保证这种情况不进入发布版本。
    pub fn start(plugins: PluginSet) -> Arc<Kernel>;

    /// 以下生命周期方法都只把请求放进 kernel 线程的队列，立即返回，可以在任何线程调用。

    // 项目事实只能通过 `KernelControl::workspace_changed` 提交（由 `project` 插件调用）：
    // 那里带着提交者的插件 ID 和激活代数，kernel 才能在提交者停用时清除这份事实、
    // 并把 `WorkspaceFacts::contains` 的 panic 归到提交者。`Kernel` 本身不提供这个入口。

    /// 激活清单里带 `Manual` 的插件。依赖的服务还没有就绪时，请求被记下，依赖就绪后再激活（见「依赖与自动启停」）。
    pub fn activate(&self, id: &str);

    /// 所有插件清单里的 `Command` 名称，供视图层预先注册代理 Action（见「按命令激活」）。
    pub fn lazy_commands(&self) -> Vec<Name>;

    /// 激活声明了这个命令的插件。激活完全成功（`started` 正常返回、这次激活仍然存活，token 为 `Live`）后，
    /// 或者激活失败后，在 kernel 线程上调用 `done`；提交之后 `started` panic 或调用 `fail` 时，`done` 收到那个错误；
    /// 已激活时也在 kernel 线程上调用。`done` 用 `catch_unwind` 包住，panic 只记日志，不影响 kernel 线程。
    /// 成功时 `done` 收到目标插件这次激活的 `ActivationToken`，`workspace` 用它等这次激活的界面注册应用完、在激活失效时丢弃重放（见「按命令激活」）。
    pub fn command(&self, name: &str, done: impl FnOnce(Result<ActivationToken, PluginError>) + Send + 'static);

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

    /// 静止时返回当前的静止代数，忙碌时返回 None。每次从忙碌变为静止，代数加一，所以两次调用返回同一个值，
    /// 说明这两次调用之间 kernel 一直静止，中间没有出现过忙碌（哪怕很短）。
    pub fn quiescent_epoch(&self) -> Option<u64>;
}

/// 项目文件的不可变快照。`project` 在后台读 git index 建好后再交给 kernel；
/// `contains` 只查内存里的数据，不做 IO，在任何线程上调用都可以。
/// 提交方可能保留 `Arc` 的克隆，最后一个引用可能在任意线程上释放，所以实现的 `Drop` 不得 panic
/// （与服务、监听闭包、事件值的要求相同）；kernel 线程上的 drop 仍用 `catch_unwind` 包住。
pub trait WorkspaceFacts: Send + Sync {
    /// 当前项目里是否有匹配 pattern 的文件。
    fn contains(&self, pattern: &str) -> bool;
}

pub struct PluginStatus {
    pub id: Name,
    pub state: PluginState,                 // Inactive / Waiting { missing: Vec<ServiceKey> } / Active / Disabled / Failed(PluginError)
    pub manifest_time: Option<Duration>,    // `Kernel::start` 时调用 manifest 的耗时（只调用一次）
    pub activate_time: Option<Duration>,    // 最近一次 activate 的耗时
    pub commit_time: Option<Duration>,      // 最近一次提交的耗时
    pub started_time: Option<Duration>,     // 最近一次 started 的耗时（同步执行的部分，加上之后发出暂存事件、调用暂存 start、同步发出 ServiceAdded 并执行其监听的时间）
}
```

### 事件

```rust
/// 事件类型由发出它的一方定义（通常在 `services`），kernel 只负责分发。
/// 事件值在分发结束或被丢弃时在发出它的线程上 drop，所以它的 `Drop` 不得 panic；
/// 在 kernel 线程上 drop 时仍按「失败」一节用 `catch_unwind` 包住。
pub trait Event: Send + Sync + 'static {}

#[derive(Clone)]
pub struct Emitter { /* Weak<Kernel>, Option<ActivationToken>（来自 PluginHandle 时为发出方这次激活的 token，app 的为 None） */ }

impl Emitter {
    /// 来自 `PluginHandle::emitter()` 的 Emitter 在分发期间持有发出方这次激活的准入（`token.enter()`）：
    /// 取不到就丢弃事件，所以发出方的 `fail` 或监听 panic 与这次分发之间不会交错，失效之后开始的分发不会把事件送给其他插件。
    pub fn emit<E: Event>(&self, event: E);
}
```

## 数据结构与规则

### 依赖与自动启停

- 插件能激活的条件：已启用；激活条件之一已满足；`requires` 里的每个服务都至少有一个已激活的提供者。
- 一个插件激活后，kernel 检查依赖它所提供服务的插件，满足条件的接着激活（按拓扑顺序，同一层按 `PluginSet` 里的顺序，结果确定）。
- 一个服务（不论 `MULTIPLE` 是否为 true）的最后一个已激活的提供者停用时，依赖它的插件先停用（依赖者先于被依赖者）。
- `MULTIPLE = true` 的服务：依赖者激活后，提供者的增减不重新激活依赖者。依赖者通过 `ServiceSet<S>` 读取当前集合，集合在提供者增减时随即更新；需要在变化时做额外处理的，订阅 `ServiceAdded<S>` 和 `ServiceRemoved<S>` 事件（带提供者的插件 ID）。
- 依赖者通过 `ServiceRef::get()` 和 `ServiceSet::current()` 调用服务，每次调用都持有提供者的准入；不得把 `Admitted` 保存到本次调用之外（否则会拖住提供者的撤销），也拿不到可以绕过准入的裸 `Arc<S>`。
- 激活条件满足后保持有效：`Workspace`、`Command` 和 `Manual`（调用过 `activate(id)`）一旦满足，在本次运行中一直视为满足；依赖暂未就绪时，等依赖就绪再激活，`status()` 显示为「等待依赖」。用户在设置里停用插件时清除 `Command` 和 `Manual` 的记录；`Workspace` 条件不记在插件上，kernel 保存最近一次的 `WorkspaceFacts`，插件重新启用（包括 `Failed` 后在设置里重试）时用它重新判断。保存的 facts 属于提交它的那次激活（`KernelControl` 带激活代数）：那次激活停用时，kernel 清除这份 facts，等新的 `workspace_changed` 再判断。还在等待的 `command` 回调立即收到 `PluginError::Disabled`，`shutdown` 时收到 `PluginError::ShuttingDown`；插件所依赖的某个服务当前没有可激活的提供者时，收到 `PluginError::DependencyUnavailable`。「可激活」沿依赖链递归判断：提供者已启用、没有 `Failed`、自己的激活条件已经满足，而且它依赖的每个服务也都有可激活的提供者；命令不会替依赖去满足它们的激活条件（如替 `Workspace` 插件假定项目匹配）。回调不会一直挂起。P0 只有一个窗口；多窗口时按项目分别计算，届时修订本节。

### 激活的提交

- `activate` 执行期间，`provide`、`on`、`own` 登记的内容只暂存在本次激活里：服务不进入 `ServiceSet` 和 `service()` 的结果，监听不参与分发。
- `activate` 返回 `Ok` 后，kernel 先检查清单 `provides` 里的每个服务都已 `provide`；缺任何一个都按激活失败处理（`PluginError::MissingService`），否则依赖者会因为等不到这个服务而一直不激活。
- 检查通过后，kernel 在一次加锁里把暂存的服务和监听全部发布（提交），然后调用 `started`。发布之后、token 变为 `Live` 之前，`ServiceSet::current()` 和 `enter()` 都不会返回这次激活，`ServiceAdded<S>` 也在 token 变为 `Live` 之后才发出，所以已在运行的依赖者看不到 `started` 可能失败的新提供者。`started` 正常返回、而且期间没有通过 `fail` 或监听 panic 把插件标为失败时，kernel 才把因它而满足条件的后续插件放进队列；否则按失败停用，后续插件不激活。
- 提供者在依赖者激活期间失败时（例如提供者的监听在其他线程上 panic，或 `fail` 被调用），kernel 在当前线程上立即把提供者这次激活的 token 置为 `Dead`（见「事件分发」）。依赖者的激活还在 kernel 线程上进行时还没有发布任何东西，所以 kernel 在提交依赖者之前、以及调用它的 `started` 之前，各检查一次它 `requires` 的每个服务是否还有 token 为 `Live` 的提供者。检查不通过时中止这次激活：暂存的内容全部丢弃（已经提交的按「撤销」一节撤销），依赖者回到「等待依赖」，不进入 `Failed`。
- `started` 返回后，kernel 用 compare-exchange 把 token 从 `Pending` 换成 `Live`。暂存队列由一把互斥锁保护，这次交换在持有这把锁时进行，同时把队列整体取出并标为已封口：其他线程通过克隆的 `PluginHandle` 或 `Emitter` 暂存事件、`start` 时也先取这把锁，在锁内读 token：仍是 `Pending` 才追加进队列，已是 `Live` 就释放锁后走正常路径（直接分发、直接调用 `start`），`Dead` 就丢弃。所以一个在交换前读到 `Pending`、交换后才追加的操作不会落进已经取走的队列里丢失：它要么在交换之前进入被取出的那一批，要么在交换之后看到 `Live`。`started` 期间其他线程调用 `fail` 已经把 token 置为 `Dead` 时交换失败，按激活失败撤销，`Dead` 不会被覆盖。`started` 不直接做 IO，也不直接改外部状态，只通过 `PluginHandle` 发事件、提交任务；token 还是 `Pending` 时，这些操作暂存在这次激活里：`emit` 的事件按顺序排队，`own_with` 的 `start` 先不调用。compare-exchange 成功、token 变为 `Live` 后，kernel 在 kernel 线程上依次发出这些事件、调用这些 `start`：每一项之前都先 `token.enter()`，取不到（前一个事件的监听 panic 或调用了 `fail`，把这次激活连带置为 `Dead`）就丢弃剩下的全部；每个 `start` 单独用 `catch_unwind` 包住，panic 时这个插件进入 `Failed`，按「失败」一节撤销，剩下的项丢弃。交换失败或 `started` panic 时全部丢弃，所以 `started` 本身失败的激活不会留下已经送达的事件或已经开始的任务。丢弃暂存的 `start` 闭包和事件值时逐个用 `catch_unwind` 包住它们的 drop，闭包捕获的值与守卫、事件值一样，`Drop` 不得 panic。token 变为 `Live` 时激活就算成功：发出暂存事件、调用暂存 `start` 的过程中再失败，按激活之后的失败处理（停用并撤销），已经送达的事件和已经提交的任务不回滚，与 `Live` 之后任何时刻的失败相同。（`Pending` 期间监听不会被调用，所以 `started` 之前不会有监听 panic。）
- 服务在提交时必须已经可用：服务对象需要的内存中的状态在 `activate` 里建好（不做 IO）。`started` 只安排后台工作（提交任务、发事件），按上一条在 token 变为 `Live` 之后才真正发生，不会出现「副作用已经发生、激活却回滚」的情况。
- 返回错误或 panic 时，暂存的内容直接丢弃（守卫按相反顺序 drop），已在运行的插件从头到尾看不到这次激活注册的任何东西。
- `activate` 本身不产生副作用：任务、事件、文件读写都从 `started` 开始，激活失败时这些都不会发生。
- 激活期间的状态（`ServiceRef`、`ServiceSet`、插件自己的数据）放在 `activate` 返回的 `Active` 对象里；监听闭包和任务需要时持有它的 `Arc` 克隆。停用时 kernel 先撤销监听和守卫，最后 drop `Active`，状态随之释放。`Active` 的 drop 同样用 `catch_unwind` 包住，panic 时记录错误、插件标为 `Failed`，撤销照常完成。`Plugin` 对象本身不保存激活期间的状态，否则停用后提供者和它的内存仍被持有。
- 在 `activate` 里交给 `ctx.own` 的守卫，创建时也不得产生可见的副作用。`dyn Contributions` 的注册通过 `ctx.own_staged` 交出：`commit` 只把注册连同 `ActivationToken` 排到主线程；主线程每帧检查 `token.state()`：`Pending` 时留到下一帧再看；否则用 `token.enter()` 取准入，取到就在持有准入期间应用注册并把注销守卫交给 kernel：交接用持有的这个准入登记守卫（`Admission::own`），只看守卫表是否已关闭，不看 token 是否已经变成 `Dead`。这个准入没释放时撤销停在第 3 步，表不会关闭，所以即使应用期间另一线程让激活失效，注销守卫也一定进入表里，随撤销第 4 步在 `Active` 之前 drop，不会走 `own` 被拒绝后排到 kernel 线程、晚于 `Active` 才 drop 的路径；取不到（`Dead`）就丢弃；丢弃同样在 `contain` 里进行，注册捕获的插件值的 `Drop` panic 不会展开到主线程。应用一次注册是一个整体，也在 `contain` 里执行：`dyn Contributions` 先调用注册里由插件提供的代码（构造菜单项、快捷键表等），全部成功后才一次性插入自己的表；插件代码 panic 时什么都不插入，按 `fail` 让这次激活失效，panic 不会展开到主线程，也不会留下只应用了一半的注册。界面回调之后的调用照常按下文的规则取准入、`contain`。应用与撤销因此不会交错：撤销第 3 步等准入归零，之后才 drop 守卫和 `Active`。所以注册只在所有 `commit` 和 `started` 都成功之后应用，也不会因为主线程比激活先跑到而丢失。激活在任何一步失败（`activate`、某个 `commit` panic、`started` panic 或调用 `fail`），token 都不会变为有效，失败的注册不会出现在界面上，与 `activate` 运行多久、跨了几帧无关。`commit` 在排队之前用 `token.pending()` 取一个守卫（`shutdown` 之后返回 None，注册直接丢弃），随注册一起排到主线程，注册应用或丢弃之后才 drop。所以从排队到主线程收到注册之间，kernel 也不会报告静止，界面注册生效之前不静止。

### 撤销

每个已激活的插件有一张注册表，保存它提供的服务、它的事件监听，以及它通过 `own` 交出的守卫。

停用时按以下顺序撤销：

1. 它停用后会失去全部提供者的服务（单实现服务，以及它是最后一个提供者的多实现服务），依赖这些服务的插件先停用。
2. 移除它提供的服务：`ServiceSet` 的快照更新，`service()` 查不到，之后不会有新的调用到达。已经在进行的调用可以执行完；服务实现要能在插件停用过程中返回错误，而不是假定自己的守卫都还在。正在进行的调用可能持有服务的最后一个 `Arc`，服务实现因此可能在调用方的线程上 drop：服务实现的 `Drop` 不得 panic，并且必须能在任意线程上执行。事件监听闭包也一样：分发中的快照可能持有闭包的最后一个引用，闭包捕获的值会在发事件的线程上 drop，所以这些值的 `Drop` 同样不得 panic。kernel 线程上的 drop 仍按「失败」一节隔离。
3. 移除它的事件监听，并等这次激活的准入计数归零：正在执行的监听、主线程上的界面回调与结果应用、通过 `ServiceSet` 进行的服务调用都返回之后才继续（见「事件分发」）。第 1 步之前 token 已经是 `Dead`，所以不会有新的准入。
4. 按与登记相反的顺序 drop 守卫。每个守卫的 drop 单独用 `catch_unwind` 包住：某个守卫的 drop panic 时，记录错误、插件标为 `Failed`，然后继续 drop 剩下的守卫，撤销总是完整执行。

视图层的注册也走守卫。例如 `workspace` 提供一个服务 `dyn Contributions`（接口在 `services`），它的 `register_view` / `register_action` / `bind_key` 返回一个守卫，drop 时注销。`activate` 期间的注册只能通过 `ctx.own_staged` 交出，按上文的 `ActivationToken` 规则应用；`started` 之后的注册（在 `started`、监听或后台回调里）同样排到主线程应用：`dyn Contributions` 在排队之前用这次激活的 `token.pending()` 取一个守卫（`shutdown` 之后为 None，注册直接丢弃），随注册带到主线程，应用或丢弃之后才 drop，所以注册还在队列里时 kernel 不报告静止；返回的注销守卫通过应用时持有的准入登记（`Admission::own`，与暂存注册相同），不走 `PluginHandle::own`，所以应用期间并发的失效也不会让守卫被拒绝、晚于 `Active` 才 drop。`ctx.own` 只接收创建时没有可见副作用的守卫，不用于界面注册。kernel 因此不依赖 GPUI 的类型。守卫的 drop 若需要在主线程执行，由提供守卫的一方负责转到主线程。

界面注册的守卫在 kernel 线程上 drop 时，只是把注销排到主线程，kernel 可能先完成撤销并 drop `Active`。这段时间里界面不能调用到已停用的激活，也不能让 kernel 先报告静止：

- 每个界面注册都带着所属激活的 `ActivationToken`（`started` 之后的注册取 `PluginHandle` 的 token）。主线程调用注册里的回调（Action、命令处理、视图工厂）时先 `token.enter()`，取不到就不调用，取到的准入持有到回调返回。回调在准入范围内用 `catch_unwind` 包住：panic 时先按 `fail` 的方式让这次激活同步失效（插件进入 `Failed`，连带停用的插件一起失效），展开结束后才释放准入，panic 不会展开到主线程，插件也不会在失效后继续被调用。停用的第一步就把 token 置为 `Dead`，第 3 步等准入归零，所以回调不会与守卫和 `Active` 的 drop 同时进行。
- `dyn Contributions` 的守卫在 drop 时用所属激活的 `ActivationToken::pending()` 取一个守卫，随注销请求一起交给主线程，注销完成后 drop；主线程上的注销（从注册表移除 Action、命令处理函数、视图工厂，并 drop 最后一份闭包）在 `contain` 里执行，一个守卫对应多项注册时每一项单独包住，闭包捕获的值的 `Drop` panic 时只记日志、让所属激活按 `fail` 失效（已经失效的只记日志），不展开到主线程，其余项照常移除；主线程的队列被丢弃时守卫随之 drop。`shutdown` 关闭准入之后取不到守卫，说明主线程的队列已经不再处理，注销请求直接丢弃。这个守卫由 kernel 直接计数，不依赖 `workspace` 的激活，所以 `workspace` 自己失败、连带撤销其他插件的界面注册时同样有效。所以注销完成之前 kernel 不报告静止，内存探针在静止后采样时，界面已经不再持有这次激活的状态。

后台任务也走守卫：`executor` 的任务句柄 drop 时取消任务。插件在 `started` 或之后用 `PluginHandle::own_with` 提交任务：激活已经失效时任务根本不会被提交。`executor` 的提交接口带上 `own_with` 给的 `SubmitGate`：闸门打开之前任务不会被 poll，`own_with` 在 `start` 正常返回、句柄登记完之后才打开它，所以 `start` 在提交之后、返回句柄之前 panic 时任务一次也没运行，闸门关闭后直接丢弃。任务每次被 poll 之前调用 `GateSlot::poll`：它在闸门的锁内登记 waker 并检查闸门和 token，关闭或为 `Dead` 时丢弃 future（等同取消）。token 变为 `Dead` 时 kernel 唤醒全部槽位，所以停在 `Pending` 上的任务也会被调度一次并丢弃，不会一直持有 `BusyGuard` 和状态、挡住静止。任务的 future 在某次 poll 里 panic 时，executor 捕获这个 panic、调用 `GateSlot::panicked`，这次激活按 `fail` 失效，插件进入 `Failed`；所以初始化任务中途 panic 不会让插件留在 `Active` 并被报告为静止。`executor` 的测试覆盖这两点：闸门在检查和登记之间打开时任务仍被调度；future panic 时插件进入 `Failed`、工作线程继续运行。

任务不持有准入，撤销不等任务结束：任务需要的状态由它自己持有（`Active` 或其中部分的 `Arc` 克隆），所以撤销 drop kernel 那份 `Active` 之后，任务用到的状态仍然有效，直到任务结束或在下一次 poll 前被丢弃。任务不能假定插件的守卫（界面注册、文件监听等）在它运行期间仍然存在。

监听 panic 或调用 `fail` 时，token 由当前线程直接原子写为 `Dead`，不等准入归零（等待会让持有准入的调用方自己卡住，见「事件分发」）。所以在写入前一刻已经取得准入的工作（其他线程上的监听、界面回调）仍会执行完，kernel 不中途打断它们；撤销第 3 步等它们返回之后才 drop 守卫和 `Active`。任务不在此列：它们按上一段持有自己的状态，正在进行的那一次 poll 执行完，之后不再被 poll。约束相同：它交给主线程的结果按下面的规则取准入，失效后不会生效；插件代码要能在插件已标为 `Failed` 时运行完毕。

任务把结果排到主线程交给视图时，任务可能已经结束，取消对它不再起作用。排队本身也要经过 `token.pending()`：任务在排队之前取一个 `PendingGuard`，随结果一起带到主线程，应用或丢弃之后才 drop；`shutdown` 关闭之后取不到，结果在当前线程上 `contain` 里直接丢弃，不进主线程的队列。所以撤销不等任务、任务在撤销后跑完了那一次 poll，也不会在 `shutdown` 返回之后再往主线程排新的工作。所以排到主线程的结果带上这次激活的 `ActivationToken` 和一个 `BusyGuard`（任务把自己的守卫移进去，或者用 `PluginHandle::busy()` 新取一个）：主线程应用结果时先 `token.enter()`，取不到就丢弃，取到的准入持有到应用完；应用（调用插件提供的应用函数）和丢弃都在 `contain` 里进行，panic 时在仍持有准入的情况下按 `fail` 让这次激活失效，展开结束后才释放准入，panic 不会展开到主线程；守卫在结果应用或丢弃之后才 drop，所以结果还在队列里时 kernel 不报告静止。

### 失败

kernel 线程上调用的每一段插件代码都单独用 `catch_unwind` 包住：`manifest`、`activate`、`Staged::commit`、`started`、`command` 的 `done`、事件监听（在 kernel 线程上分发时）、判断 `Workspace` 激活条件时调用的 `WorkspaceFacts::contains`，以及 kernel 释放的插件对象的 drop（`shutdown` 时的 `Plugin` 对象本身、服务实现、守卫、`Active`、丢弃的暂存 `start` 闭包、被替换或清除的 `WorkspaceFacts`、在 kernel 线程上分发或丢弃的事件值）。`shutdown` 之后在调用线程上 drop 被拒绝的守卫时也用 `catch_unwind` 包住。后台任务不在 kernel 线程上运行，它的每次 poll 由 `executor` 用 `catch_unwind` 包住，panic 经 `GateSlot::panicked` 交给 kernel，按同样的规则让所属激活失效。`WorkspaceFacts::contains` panic 时，算作提交这份 facts 的那次激活（通常是 `project`）失败：那个插件进入 `Failed`，kernel 丢弃这份 facts，正在判断的插件按条件未满足处理。kernel 用一个共用的公开函数 `contain(f) -> Result<T, String>` 包住全部边界，不在各处各写一遍，executor 包住 future 的 drop 时也用它（poll 用保留 payload 的 `catch`，见 `GateSlot::panicked`）：捕获到 panic 时当场取出可读的消息，在 `catch_unwind` 里 drop payload，这次 drop 又 panic 时第二个 payload 用 `mem::forget` 泄漏，然后只返回消息，所以 payload 持有的资源在调用方做后续处理（让激活失效、唤醒其他任务）之前已经释放，`Drop` 会 panic 的 payload（`panic_any` 抛出）也不会在调用方或 kernel 线程上引发第二次展开。任何一处 panic 都只让对应的插件进入 `Failed`（`done` 回调只记日志），kernel 线程继续处理后面的请求，撤销和 `shutdown` 总能完成。下面按场景说明处理方式。

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
  - 监听里发出的事件排在当前分发之后，在同一线程上依次处理，不会递归。排队的事件带上发出它的那个 `Emitter` 的 token（不是正在执行的监听的 token：A 的监听通过服务方法调用了 B 的 `Emitter` 时，排队的是 B 的 token），轮到它分发时先 `token.enter()` 取准入，取到的准入持有到这次分发的全部监听返回为止；取不到（发出方这次激活已经失效）时丢弃，不通知其他插件。`app` 的 `Emitter` 没有 token，它的排队事件不受任何插件失效影响。只检查状态不够：检查之后、分发期间，另一个线程可能让发出它的激活失效，持有准入时停用的第 3 步会等这次分发结束，失效之后的事件不会再送达。
- 快照里可能有已停用插件的监听。每个监听带上所属激活的 `ActivationToken`，调用前取准入（`token.enter()`）：
  - token 属于一次激活，不属于插件：每次激活新建一个，变为 `Dead` 后不再恢复；插件重新激活时用新的 token。所以分发线程即使拿着旧快照停顿，等插件重新激活后再检查，看到的仍是旧激活的 `Dead`，不会调用上一次激活的监听；
  - 取不到准入（token 是 `Pending` 或 `Dead`）就跳过这个监听；取到就调用，调用期间一直持有准入；
  - 停用时第 1 步把 token 置为 `Dead`，第 3 步等准入计数归零，因此会等这个插件正在执行的监听返回，此后快照里它的监听都会被跳过；
  - 取准入不阻塞，只有停用的第 3 步等待。监听里请求的生命周期操作（包括停用自己所属的插件）排队到当前分发结束后执行，kernel 线程不会在自己持有准入时执行第 3 步；
  - 监听 panic 或调用 `PluginHandle::fail` 时，立即在当前线程上把这个插件以及会因它停用而连带停用的插件（依赖它所提供服务的插件，递归）的 token 都置为 `Dead`（原子写，不等待准入），再把停用请求放进 kernel 线程的队列。「这个服务还有没有存活的提供者」要跨多个 token 判断，单个 token 的原子写不够：两个线程同时让一个 `MULTIPLE` 服务的最后两个提供者失效时，各自都可能看到对方还活着，依赖者就会留在 `Live`。所以所有失效（`fail`、监听 panic、`contain` 捕获的 panic、撤销第 1 步）都在 kernel 的一把失效锁内完成：在锁内写 `Dead`、按当时的 token 状态挑出失去最后一个提供者的依赖者并递归写 `Dead`，然后才释放锁，后一个失效一定看到前一个的结果。这把锁内不等待准入、不调用插件代码，也不唤醒闸门：锁内只写状态、收集受影响的激活，释放锁之后才逐个取这些激活的闸门锁、克隆 waker、释放闸门锁再 `wake`。`wake` 按 `executor` 的约定只入队，丢弃 future 发生在 executor 的工作线程上，future 的 drop 可以调用 `fail`、再取这把失效锁，所以锁内不唤醒。所以持有准入的线程取它也不会死锁。在监听里调用 `fail` 时，这个监听自己的准入不提前释放，监听返回时由分发方 drop；`fail` 本身不等待，所以不会自己等自己。主线程在这之后取不到这些插件的准入，不会再调用它们的界面回调；同一次分发里这些插件后面的监听、其他线程上的分发也都会跳过它们。
  - 置为 `Dead` 之前已经在其他线程上取得准入的调用照常执行完。停用第 3 步会等它们返回，之后才 drop 守卫和 `Active`，所以它们执行期间插件的状态仍然有效。插件的监听因此要能在插件已标为 `Failed` 时运行完毕，与停用过程中的服务调用要求相同（见「撤销」第 2 步）。
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
2. 代理 Action 被触发时：查找处理函数时跳过 token 已不是 `Live` 的登记（插件已失效、注销还没在主线程上执行）；找到就在准入内直接调用，`enter()` 失败时同样当作没有登记；没有登记时调用 `control.command(name, done)`，由它重新激活插件或在 `done` 里返回 `Disabled` 等错误，用户的操作不会被悄悄丢掉。
3. 插件激活完全成功（`started` 正常返回）后 `done` 被调用；`workspace` 把它转到主线程，等这次激活的界面注册在主线程上应用完之后调用它。只等「下一帧」不够：注册和重放是从不同线程排到主线程的两件事，先后不确定，所以主线程为每次激活记下它暂存的界面注册是否已经应用（或丢弃），重放带着插件的 token 排队，每帧检查，这个标记置位之后才执行；token 先变成 `Dead` 时按激活失败提示。用户只需触发一次。转到主线程之前用 `workspace` 自己的 token 取 `pending()` 守卫，随这次重放一起带到主线程，重放或丢弃之后才 drop，所以用户的命令真正执行之前 kernel 不报告静止；取不到（`shutdown` 已关闭准入）时直接丢弃这次重放。主线程执行重放时先取 `workspace` 的准入，取不到（`workspace` 已停用）就丢弃，不在 `workspace` 撤销之后运行；查找和调用处理函数仍按第 2 步对插件的 token 取准入。
4. 激活失败时，`done` 收到错误，`workspace` 显示一次提示。
   激活成功、这次激活的注册应用完之后仍没有这个命令的处理函数时（插件漏了 `handle_command` 或名字写错），`workspace` 同样提示一次、记录错误，这次触发作废，不会一直等待。
   处理函数已经登记、但到下一帧调用时它的 token 已不是 `Live`（插件在这期间停用或失败）时，按激活失败处理：提示一次并记录，不静默丢弃这次触发。
5. 插件停用时，处理函数的守卫被 drop，代理仍然在。用户重新启用插件后再次触发命令，代理照常走第 2 步激活它。

### 静止

[ADR 0017](../adr/0017.md) 的内存基准和 UI 探针要等「kernel 发出已静止信号」再采样。kernel 在以下条件同时成立时为静止：

- kernel 线程已经处理完启动时的激活；
- 生命周期操作的队列为空，kernel 线程上没有正在执行的生命周期操作（激活、提交、`started`、停用与撤销、`shutdown`）；
- 没有正在进行的事件分发（kernel 计数各线程上进行中的 `emit`，监听可能正要取 `BusyGuard`）；
- 没有持有中的准入：`ServiceRef::get`、`ServiceSet` 的调用、主线程的界面回调和结果应用都持有 `Admission`，kernel 对所有激活的准入另外维护一个全局计数，`enter()` 成功时与本激活的计数一起加一，所以插件代码正在运行、还没来得及取 `BusyGuard` 时也不会报告静止（分发 `Busy` / `Quiescent` 时调用监听用的准入除外，见下文）；
- 没有未 drop 的 `BusyGuard` 和 `PendingGuard`（排到主线程、尚未完成的界面注册或注销）。

插件把初始化工作（扫描会话目录、打开 SQLite、建索引）交给 executor 时，用 `PluginHandle::busy()` 取一个守卫移进任务，任务结束时守卫随之 drop。`PluginHandle` 在 `started` 里取得并保存，所以打开项目、收到事件之后开始的工作也能标记为忙碌。kernel 不知道 executor 的存在，只计数守卫。周期性或长期运行的任务（文件监听、定时刷新）在等待期间不取守卫，但每一轮实际工作（一次刷新：读文件、解析、更新状态）开始时取一个 `BusyGuard`，这一轮结束、结果交给主线程并应用之后才 drop（见「撤销」里排到主线程的结果），所以刷新进行中时 kernel 不报告静止。插件停用时，它的任务句柄被 drop、任务被要求取消，但取消是协作式的：任务真正结束、守卫被 drop 之前，守卫仍然计入，kernel 不报告静止。

每次从忙碌变为静止，kernel 发出一次 `Quiescent` 事件；每次从静止变为忙碌，发出一次 `Busy` 事件。转换发生在取守卫、drop 守卫或释放准入的那个调用里，调用方可能正持有插件自己的锁，所以转换时只在原子计数上取得序号，事件排进 kernel 线程的队列，由 kernel 线程分发，不在这个调用栈里同步执行监听（否则同一插件的监听要取同一把锁时会死锁）；这个分发请求不算生命周期操作，不让 kernel 变忙。两个事件都带上这次转换的序号（与 `quiescent_epoch()` 同一个单调计数器，转换在计数器上原子递增时取得），`emit` 在不同线程上同步执行，事件到达监听的顺序可能和转换的顺序不同，所以监听按序号判断：序号不大于已经处理过的最大序号的事件是过时的，直接忽略。bench 因此不会被一个迟到的 `Busy` 取消计时、又等不到下一个 `Quiescent`。这两个事件本身的分发不计入进行中的 `emit`，调用它们的监听时取的准入也不计入全局准入计数（仍计入所属激活自己的计数，停用照常等它结束），否则调用 `Quiescent` 的监听就会让 kernel 重新变忙、发出 `Busy`，释放后又发出 `Quiescent`，循环下去。监听里再做的事（取服务、发事件、取 `BusyGuard`）照常计入，所以这两个事件的监听只记录序号和时间，bench 的监听就是这样；在监听里做实际工作会如实引起一次新的忙碌和静止。bench 收到 `Quiescent` 后等 5 秒再采样，期间收到 `Busy` 就重新开始等待。采样前后各读一次 `quiescent_epoch()`：两次都是同一个 `Some(n)` 时样本有效，否则（采样期间有过哪怕很短的忙碌）丢弃这次样本，重新等待静止。`Quiescent` 的用途：启动后第一次静止用于空窗口的内存基准，打开项目触发的激活与初始化结束后再次静止，用于 monorepo 的内存基准。超时（ADR 0017 规定首帧后 30 秒内不静止即失败）由 bench 判断。

## 性能

- 首帧之前不调用 kernel 的任何函数：`Kernel::start` 在启动屏障放开后才调用，建表、读清单、检查依赖图和激活都在 kernel 线程上执行，不占主线程。
- 激活耗时：每个插件的 `manifest()`（`Kernel::start` 时调用一次）、`activate`、提交（检查清单、发布服务和监听、调用各守卫的 `commit`、更新快照）和 `started`（同步执行的部分，加上 token 变为 `Live` 之后在 kernel 线程上发出暂存事件、调用暂存 `start`，以及为这次激活提供的服务同步发出 `ServiceAdded` 并执行其监听的时间）由 kernel 分别计时，结果在 `status()` 里，预算按四者之和计算。建议预算是单个插件 1 ms、首帧后 `AfterFirstFrame` 全部激活合计 5 ms。数字在 [#7](https://github.com/SoloJiang/lectern-app/issues/7) 里用原型实测后校准，并写进 bench 的预算表。
- 常驻内存在同一进程里无法准确地按插件区分。bench 对每个插件启动两次：一次只含它的依赖闭包，一次是依赖闭包加上它，两次静止后的 `phys_footprint` 之差记为这个插件的常驻内存。两次启动都要满足依赖闭包里每个插件（包括目标插件）的激活条件，否则测到的只是没激活的插件对象：命令不会替依赖满足条件，所以 bench 逐个满足：`Workspace` 插件需要一份匹配它们全部 pattern 的 `WorkspaceFacts`：bench 在两次启动里都加入同一个合成的 `bench-facts` 插件（`AfterFirstFrame` 激活，在 `started` 里通过 `control().workspace_changed` 提交这份 facts，`test-support` feature 提供），两次的差不受它影响；`Command` 插件调用一次对应的命令，`Manual` 插件调用 `activate`；确认闭包里的插件都进入 `Active` 后再等静止、采样。依赖闭包那一次满足同样的条件（目标插件自己的除外），两次的差只来自目标插件。完整 profile 的总内存另外按 ADR 0017 的预算单独检查。`PluginSet` 支持这两种组合。
- 服务查找是一次读锁加一次哈希表查找；插件应在 `activate` 里取出需要的 `ServiceRef` / `ServiceSet`，保存在激活内的状态对象里（见「激活的提交」）。`ServiceRef::get()` 的开销是一次准入（两次原子操作）；`ServiceSet::for_each` 每个提供者一次准入、不分配；`ServiceSet::current()` 另外分配一个 `Vec`。两种多实现路径都在 bench 里按 1、4、16 个提供者测量。
- 普通的事件分发（不在监听里再发事件）不分配内存：监听快照是 `Arc` 克隆，没有监听时只有一次查表。监听里发出的嵌套事件要先保存到分发结束，会装箱后放进本线程的队列，有一次分配；这条路径单独做基准。

## 测试

全部测试在 Linux 上运行（`just test -p lectern-kernel`）：

- 依赖图：`validate` 对缺提供者、重复插件 ID、两个服务共用一个稳定 ID、单实现服务有两个提供者、空的激活条件、环，分别给出对应的错误。
- 激活的提交：
  - `activate` 先 `provide` 再返回错误时，并发的依赖者和事件分发都看不到这个提供者和它的监听，`started` 不被调用；
  - `activate` 返回 `Ok` 但缺少清单里声明的服务时，按失败处理。
- 句柄：`on` 返回的 `Subscription` drop 后，下一次 `emit` 起不再调用；停用后旧的 `PluginHandle` 上 `own` 立即 drop 守卫并返回 false，`on` 不订阅，`busy` 返回 None，从它取得的 `Emitter` 发出的事件被丢弃；重新激活后旧句柄仍然无效。
- 退出：`shutdown` 的 `done` 在全部撤销完成、kernel 线程退出后调用；需要在主线程 drop 的守卫在等待期间完成。
- 静止与界面注册：只注册了 `Contributions` 的插件，在注册应用到界面之前 kernel 不静止，包括注册已排队、主线程尚未收到的那段时间；`started` 或监听里注册的界面内容同样如此。
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
- 暂存效果：暂存的第一个事件让这次激活失效后，后面暂存的事件不再发出、`start` 不再调用（已经发出的第一个事件不回滚，激活按 `Live` 之后的失败停用）；暂存的 `start` panic 时插件进入 `Failed`，kernel 线程继续运行。
- `own_with` 的 `start` panic：闸门关闭、任务一次也没运行，插件进入 `Failed`，之后的撤销正常完成，调用方线程没有展开。
- `own_with` 的 `start` 里调用同一个句柄的 `own`、`own_with`：不死锁，守卫都登记上；`start` 运行期间另一个线程让这次激活失效（只做同步的 token 失效）：`own_with` 返回 false，返回的句柄被 drop、闸门关闭，任务一次也没运行；撤销在 `start` 返回、句柄处理完、准入释放之后才完成。
- 嵌套事件的来源：A 的监听通过 B 的 `Emitter` 发出事件后 B 失效、A 仍存活：排队的事件被丢弃；A 失效、B 仍存活：事件照常送达。
- 暂存封口：`started` 返回、token 换成 `Live` 的同时，另一个线程用克隆的句柄发事件、`own_with`：事件恰好送达一次，`start` 恰好调用一次。
- payload 释放：一个自定义 panic payload 的 `Drop` 一直阻塞时，之后的 panic 照常处理，其他 payload 照常释放，撤销和 `shutdown` 不受影响。
- 任务结果的应用 panic：主线程不展开，插件进入 `Failed`。
- 并发失去提供者：一个 `MULTIPLE` 服务的最后两个提供者在两个线程上同时 `fail`，依赖者的 token 一定变为 `Dead`（重复上万次）。
- 被拒绝的 `own_with`：旧句柄在激活失效后调用 `own_with`，`start` 捕获的值 drop 时 panic，调用方不展开。
- 克隆走的 waker：另一个线程在任务 panic 期间调用之前克隆的 waker，任务不会被重入 poll。
- `shutdown` 返回之后：仍在 poll 的任务完成并尝试排出结果，主线程队列里不会出现新的工作。
- 转换事件乱序：一个 `Busy` 的分发在另一个线程上卡住、之后的 `Quiescent` 先送达，bench 忽略迟到的 `Busy`，照常采样。
- 激活失效后丢弃排队的界面注册时，注册捕获的值 drop panic，主线程不展开。
- 任务 panic 时持有同一闸门上兄弟任务需要的锁：兄弟任务被唤醒时锁已经释放，不会卡住。
- 准入与静止：一个 `ServiceRef::get` 正在执行、所有守卫计数为 0 时，kernel 不报告静止。
- `on` 与撤销并发：撤销完成之后注册表里没有这次激活的监听。
- 任务 panic 时的重入：任务 panic 触发失效、唤醒名单里还有它时，executor 不会重新 poll 这个任务，也不死锁。
- `wake` 不同步执行：在持有插件锁的监听或界面回调里调用 `fail`，被唤醒的任务的 future 的 drop 也要取这把锁时，`fail` 照常返回，丢弃在工作线程上等锁释放后完成。
- 任务句柄回收：同一次激活里反复 `own_with` 一万个立即完成的任务，守卫表的长度回到 0；future 在取消、完成后被 drop 时 panic，插件进入 `Failed`、工作线程继续运行。
- 界面注册的应用 panic：主线程不展开，注册一项都没有插入，插件进入 `Failed`。
- 闸门唤醒：任务第一次 poll 就调用同一个句柄的 `own_with` 不死锁；`start` 提交任务后卡住、期间激活失效时，等待中的任务被唤醒并丢弃，不等 `start` 返回；`manifest`、`activate`、监听、守卫的 drop 以 `Drop` 会 panic 的 payload panic 时，kernel 线程继续运行；任务以 `Drop` 会 panic 的 payload panic 时，executor 和 kernel 线程都继续运行；任务 panic 的 payload 持有同一闸门上另一个任务需要的许可时，payload 在唤醒那个任务之前已经 drop，不死锁；`Quiescent` 的监听只记录序号时，kernel 停在静止，不反复发出 `Busy` / `Quiescent`；通过已停用的句柄调用 `on`、`handler` 的 `Drop` panic 时不展开到调用方。
- 主线程回调 panic：Action 或视图工厂 panic 时主线程没有展开，插件进入 `Failed`，之后不再被调用。
- 孤儿任务：`start` 提交任务后 panic，任务一次也没有被 poll 就被丢弃；已经挂起（返回 `Pending`）的任务在激活失效时被唤醒并丢弃，它持有的 `BusyGuard` 随之释放，kernel 能回到静止。
- 暂存闭包的 drop：丢弃的暂存 `start` 闭包在 drop 时 panic，kernel 线程继续运行，撤销照常完成。
- 失效中的命令：插件已失效、注销还没执行时触发代理 Action，走 `control.command`，`done` 收到错误或插件重新激活。
- 命令重放与静止：按命令激活成功、重放还在主线程队列里时 kernel 不报告静止；`workspace` 在重放之前停用时重放被丢弃。
- 任务 panic 的顺序：payload 的 `Drop` 要等 future 持有的锁时，future 先被 drop，payload 随后释放，工作线程不卡住；`start` 把闸门交给另一个线程、`start` 返回后才申请槽位时得到 None，任务不运行；登记的回收与撤销并发时，句柄只 drop 一次且在 `Active` 之前；`own` 与撤销并发时守卫不会留在 `Active` 之后。
- `own_with` 与撤销：`start` 卡住期间激活失效时，句柄在撤销 drop `Active` 之前被 drop；`started` 里调用 `busy()` 能取到守卫。
- 静止事件与锁：持有插件锁时 `busy()` 或 drop 最后一个守卫、同一插件的 `Busy` / `Quiescent` 监听取同一把锁，不死锁；`started` 里调用 `own_with` 的任务在激活变为 `Live` 后运行；命令重放排在这次激活的界面注册应用之后。
- 注册应用与失效：主线程应用注册时另一线程调用 `fail`，注销守卫仍登记进守卫表，并在 `Active` 之前 drop。
- 失效与唤醒：失效唤醒的任务被丢弃、future 的 drop 调用 `fail` 时不死锁；`busy()` 与 `fail` 并发时，要么返回 None，要么守卫在撤销完成之前创建；主线程注销时闭包的 `Drop` panic 不展开到主线程。
- 界面注册与失效竞争：主线程应用注册时另一线程调用 `fail`，撤销在注册应用完之后才 drop 守卫和 `Active`，注册随后被注销。
- 发出方失效：后台任务的 `emit` 与 `fail` 并发时，`fail` 返回之后开始的分发不会把事件送给任何监听。
- 静止代数：两次 `quiescent_epoch()` 之间取过再释放一个 `BusyGuard` 时，两次返回值不同。
- `started` 暂存：`started` 里发事件、提交任务后再 panic 或被 `fail`，事件没有送达任何监听，`start` 没有被调用；`started` 成功时它们在 token 变为 `Live` 之后按顺序发生。
- 单实现服务：提供者失效后 `ServiceRef::get()` 返回 None；提供者的撤销等正在进行的 `Admitted` 调用结束。
- 准入：token 为 `Dead` 之后 `enter()` 返回 None；停用在准入计数归零之前不 drop 守卫和 `Active`（主线程回调、服务调用、监听各一个测试）；在监听里调用同一插件的服务、在监听里调用 `fail` 都不死锁。
- token 状态：`started` 期间另一个线程调用 `fail` 时，`started` 返回后 token 仍为 `Dead`，激活按失败处理，`done` 收到那个错误。
- 新提供者：`MULTIPLE` 服务的第二个提供者在 `started` 成功之前不出现在已激活依赖者的 `ServiceSet::current()` 里，`ServiceAdded` 在它变为 `Live` 之后才发出；`started` 失败时依赖者从头到尾看不到它。
- `shutdown` 之后：`pending()` 返回 None，`shutdown` 返回后不再有新的主线程工作排队。
- 失败：监听 panic 后，同一次分发里它所属插件后面的监听不再被调用；`PluginHandle::fail` 让插件进入 `Failed` 并停用，在其他线程上调用时，返回后它和单一提供者的依赖者的 token 已为 `Dead`；监听先发事件再 panic 时，排队的事件不再分发；`Active` 的 drop panic 时撤销照常完成。
- 提交门：`own_staged` 的守卫只在提交时 `commit`，激活失败时不 `commit`。
- 重新启用：项目没变时，停用后重新启用的 `Workspace` 插件按保存的 `WorkspaceFacts` 重新激活。
- 命令：激活成功但没有登记处理函数时，`workspace` 报错，代理不会一直等待。
- 状态对象：`started` 收到的是 `activate` 返回的同一个 `Active`；停用后它被 drop。
- 按命令激活：`command` 的 `done` 在激活完全成功后被调用一次，`started` panic 时收到错误而不是成功；已激活时立即调用；失败时收到错误；`done` panic 后 kernel 线程继续处理后续请求。
- 控制句柄：插件停用后，它之前取得的 `KernelControl` 发出的 `workspace_changed`、`activate` 被忽略，`command` 的 `done` 不被调用；请求发出后、目标插件激活期间请求方失效，`done` 同样不被调用。
- 状态：激活条件已满足、但依赖未就绪的插件，`status()` 显示 `Waiting`，并列出缺少的服务。
- 守卫：停用后通过旧句柄 `own` 的守卫被拒绝，在调用线程上当场 drop；停用之前登记的守卫在 kernel 线程上 drop。
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
        pub fn inner(&self) -> Option<Arc<Kernel>>;         // 背后的 Kernel，供需要直接操作的测试
    }
}

impl Kernel {
    /// 等 kernel 线程处理完调用之前排入的全部请求（`test-support` 下提供）。
    pub fn barrier(&self);
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
- `status()` 给出每个插件的激活耗时，`Quiescent` 事件、`is_quiescent()` 和 `quiescent_epoch()` 给出静止信号，供 #7 的 bench 读取。

## 实际验证

在 Linux 上运行（kernel 不依赖 GPUI，macOS 上行为相同）：

- `just check`（格式、架构规则、文档链接、clippy `-D warnings`）、`just test -p lectern-kernel`（76 个测试）、`just doctest`、`just deny` 全部通过；`cargo nextest run -p lectern-kernel --all-features` 连续运行 15 次全部通过。
- 依赖图、自动启停、撤销顺序、失败隔离、事件分发的结果用 insta 快照记录（`crates/kernel/src/tests/snapshots/`）。
- 「测试」一节中属于 kernel 自身的条目都有对应测试，分在 `crates/kernel/src/tests/` 的 `graph`、`lifecycle`、`events`、`services`、`handles`、`failure`、`quiescence`、`commands`、`workspace`、`gates`、`shutdown`、`testing` 里。`gates` 用测试里的最小执行器（`tests/executor.rs`）验证 `SubmitGate` / `GateSlot` 与执行器约定的配合：闸门打开前任务不运行、`start` panic 时任务一次也没运行、任务 panic 时插件进入 `Failed` 且工作线程继续运行、激活失效时挂起的任务被唤醒并丢弃、一次性任务的登记被回收。
- 并发：一个 `MULTIPLE` 服务的最后两个提供者在两个线程上同时 `fail`，依赖者一定变为 `Dead`，重复 300 次（设计写「上万次」，单次测试要启动一个 kernel，300 次已覆盖交错，测试时间控制在 1 秒内）；多线程同时 `emit` 和反复停用、启用不死锁。

没有在本 PR 验证、由使用这些原语的 crate 验证的条目：

- 主线程上的界面注册与注销、命令重放、主线程回调和结果应用的 panic 隔离（「静止与界面注册」「界面注册」「命令重放与静止」「主线程回调 panic」「界面注册的应用 panic」「注册应用与失效」「任务结果的应用 panic」等）：由 `workspace` 实现，kernel 提供 `ActivationToken::enter` / `pending`、`Admission::own` 和 `contain`，这些原语本身已测试。
- 执行器一侧的约定（`wake` 只入队、不重入 poll、克隆走的 waker、`shutdown` 返回后不再排队）：由 `executor` 的测试覆盖；本 PR 的测试执行器只演示约定。
- 激活耗时预算和内存基准：在 [#7](https://github.com/SoloJiang/lectern-app/issues/7) 的 bench 里测量，`status()` 已给出四段耗时。

## 与设计的差异

- 新增 `Kernel::barrier` 和 `TestKernel::inner`（只在 `test-support` 下）：测试需要等 kernel 线程处理完已排队的请求，否则只能轮询状态。不影响发布版本的接口。
- `PluginError` 的变体按实现需要列全：`Failed`（插件报告）、`Panicked`、`MissingService`、`UndeclaredService`、`DuplicateService`、`InvalidGraph`、`DependencyUnavailable`、`Disabled`、`ShuttingDown`、`UnknownCommand`。设计里提到的 `Stale` 没有用到（请求方失效时 `done` 不被调用），不提供。
- `Context::service` / `service_set` 收到不在 `requires` 里的服务时 panic，由 `activate` 的边界捕获、插件进入 `Failed`，错误信息写明原因；设计只写了「必须在 requires 里」。
- 启动时的依赖图检查只让出错的插件进入 `Failed`；`MissingProvider` 不让依赖者失败（提供者可能只是清单 panic），依赖者停在 `Waiting`。`validate` 仍报告全部错误。依赖环之外、依赖环里插件的依赖者也按环处理（进入 `Failed`），实现更简单，`validate` 的单元测试会先在 CI 里拦下环。判断环时，一个多实现服务的多个提供者互为替代：只要有一个提供者能排在前面就不算环（例如提供者 A 依赖消费者 C、另一个提供者 B 独立时，顺序是 B、C、A）。
- 日志写到 stderr（kernel 只用标准库，没有日志依赖），由 `app` 重定向到日志文件。
- `ServiceRemoved` 只为发出过 `ServiceAdded` 的激活发出：`started` 失败的提供者从头到尾不被依赖者看到，与「新提供者」一条一致。

