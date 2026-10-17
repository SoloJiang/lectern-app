# executor 设计

状态：已接受，尚未实现。

`executor` 运行 UI 线程之外的 IO、解析和 git 操作（[技术调研「不同步阻塞 UI」](tech-research.md)）。它实现 kernel 对执行器的约定（[kernel 设计](kernel.md)「撤销」里的后台任务、`SubmitGate`、`GateSlot`），让插件提交的任务随激活失效而丢弃、任务 panic 时插件进入 `Failed`。

## 范围

- 做：
  - 固定数量的工作线程和两级优先级的任务队列。
  - 提交 future 或闭包，返回可取消的任务句柄；句柄 drop 即取消。
  - 插件经 `SubmitGate` 提交的任务：每次 poll 之前检查闸门，按 kernel 的约定处理 panic。
  - 长时间阻塞的工作（等外部进程、监听文件）用单独的线程，不占工作线程。
  - 退出时停止接收任务、丢弃未完成的任务、等工作线程结束。
  - 测试用的手动执行器：不启动线程，由测试逐步运行任务。
- 不做：
  - 把结果交给主线程：由视图层的主线程队列负责（GPUI 的前台执行器）。按 kernel 设计，排到主线程之前取 `PendingGuard`，主线程应用前取准入；executor 不知道主线程。
  - 线程的 QoS 和优先级：macOS 专属，在 `platform`（架构规则 6）。executor 提供线程启动钩子，`app` 把 `platform` 的设置传进来。
  - 定时器和异步 IO 反应器：P1 的工作都是阻塞式读取（文件、gix、SQLite），任务里直接做阻塞调用。需要定时的工作（文件监听的去抖）由 `fs_watch` 在专用线程里实现。

## 依赖

只用标准库和 `lectern-kernel`（`SubmitGate`、`GateSlot`、`catch`、`contain`）。任务状态机用 `Arc` 加 `std::task::Wake` 实现，不需要 `unsafe`，也不引入 `async-task`、`tokio` 这类依赖：Lectern 的任务数量在千级，不需要它们的优化，而 kernel 约定里「运行中被唤醒只设标记」「已结束的任务不再 poll」这类规则自己实现更容易对照测试。

## 接口

```rust
/// 后台执行器。`Clone + Send + Sync`，`app` 在启动屏障放开后创建一个，交给需要它的服务。
/// 创建时不启动线程，第一次提交任务时才启动工作线程（ADR 0017：首帧之前不提交任务）。
#[derive(Clone)]
pub struct Executor { /* Arc<Inner> */ }

pub struct Config {
    /// 工作线程数。默认 `clamp(available_parallelism - 1, 2, 6)`。
    pub workers: usize,
    /// 每个工作线程和专用线程启动时在该线程上调用，`app` 用它设置 QoS 和线程名之外的属性。
    pub on_thread_start: Option<Arc<dyn Fn(ThreadKind) + Send + Sync>>,
}

pub enum ThreadKind { Worker, Dedicated }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    /// 用户正在等的结果：当前审阅会话的 diff、打开的文件。
    Interactive,
    /// 扫描、索引、刷新。
    Background,
}

impl Executor {
    pub fn new(config: Config) -> Executor;

    /// 提交一个 future。任意线程调用，不阻塞。
    pub fn spawn<T: Send + 'static>(
        &self,
        priority: Priority,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T>;

    /// 插件经 `PluginHandle::own_with` 提交任务时用：在本调用里同步申请 `GateSlot`，申请不到（闸门已封住）时
    /// 不提交，在 `contain` 里丢弃 future（panic 时调用 `SubmitGate::panicked`），返回已取消的句柄。
    pub fn spawn_gated<T: Send + 'static>(
        &self,
        gate: &SubmitGate,
        priority: Priority,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T>;

    /// 把一个阻塞闭包当作任务运行，等同于 `spawn(priority, async move { f() })`。
    pub fn run<T: Send + 'static>(&self, priority: Priority, f: impl FnOnce() -> T + Send + 'static) -> Task<T>;
    pub fn run_gated<T: Send + 'static>(&self, gate: &SubmitGate, priority: Priority, f: impl FnOnce() -> T + Send + 'static) -> Task<T>;

    /// 在新建的专用线程上运行可能长时间阻塞的闭包（等子进程退出、阻塞读管道）。线程随闭包结束而退出。
    pub fn run_dedicated<T: Send + 'static>(&self, name: &str, f: impl FnOnce() -> T + Send + 'static) -> Task<T>;

    /// 停止接收新任务（之后的提交直接返回已取消的句柄），丢弃队列里和挂起的任务（future 在 `contain` 里 drop），
    /// 等工作线程退出，最多等 `timeout`。专用线程不等，它们的闭包结束时自行退出。
    pub fn shutdown(&self, timeout: Duration) -> Shutdown;
}

pub enum Shutdown { Clean, TimedOut { busy_workers: usize } }

/// 任务句柄。drop 时取消：任务不再被 poll，下一次轮到它时 future 被丢弃。
/// 可以在另一个任务里 `.await` 取得结果。
pub struct Task<T> { /* Arc<TaskCell<T>> */ }

impl<T> Task<T> {
    /// 放弃句柄但不取消，任务照常运行完。插件不用它：插件的任务句柄交给 kernel 保管。
    pub fn detach(self);
    pub fn is_finished(&self) -> bool;
}

impl<T> Future for Task<T> {
    type Output = Result<T, TaskError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskError {
    Cancelled,
    Panicked(String),
}
```

测试辅助在 `test-support` feature 下：

```rust
impl Executor {
    /// 不启动任何线程的执行器：任务只在调用 `run_until_idle` 时在调用线程上运行，顺序确定。
    pub fn manual() -> Executor;
    /// 依次运行队列里的任务，直到队列为空；返回运行的次数。
    pub fn run_until_idle(&self) -> usize;
}
```

## 数据结构

每个任务是一个 `Arc<TaskCell>`：

```rust
struct TaskCell<T> {
    state: AtomicU8,                       // Idle / Queued / Running / RunningWoken / Done
    future: Mutex<Option<Pin<Box<dyn Future<Output = T> + Send>>>>,
    slot: Option<GateSlot>,                // spawn_gated 才有
    priority: Priority,
    cancelled: AtomicBool,
    output: Mutex<Output<T>>,              // 结果与等待结果的 waker
}
```

- `Waker` 由 `TaskCell` 实现 `Wake` 得到。`wake` 只改状态并入队：`Idle → Queued` 时放进对应优先级的队列；`Running → RunningWoken` 只设标记；`Queued`、`RunningWoken`、`Done` 时什么也不做。所以 `wake` 从不在调用线程上 poll 或 drop future，也不会让一个任务同时被两个线程 poll。
- 工作线程取出任务后把 `Queued` 换成 `Running` 再 poll；poll 返回 `Pending` 时用 compare-exchange 把 `Running` 换回 `Idle`，失败（期间被唤醒，状态是 `RunningWoken`）就换成 `Queued` 重新入队。
- 任务结束（返回 `Ready`、被取消、闸门返回 `Drop`、panic）时状态写为 `Done`，之后的唤醒一律忽略。`Done` 之后再丢弃 `GateSlot`，闸门据此回收登记。
- 两个队列（`Interactive`、`Background`）各是 `Mutex<VecDeque>` 加一个共用的 `Condvar`。工作线程先取 `Interactive`；保留一个工作线程只取 `Interactive`，后台工作把其余线程占满时，用户等的任务仍有线程可用。

## 与 kernel 的约定

`spawn_gated` 提交的任务按 [kernel 设计](kernel.md) 处理：

- 每次 poll future 之前调用 `GateSlot::poll(cx)`：`Run` 才 poll；`Wait` 返回 `Pending`（闸门打开时 kernel 唤醒它）；`Drop` 在 `contain` 里丢弃 future，任务结束。
- poll 用 `kernel::catch` 包住。panic 时顺序固定：先把状态写为 `Done`，再在 `contain` 里 drop future，然后 `PanicPayload::dispose` 取出消息，最后调用 `GateSlot::panicked(message)`。
- future 的每一次最终 drop（完成、取消、闸门关闭、shutdown 丢弃）都在 `contain` 里进行，panic 时调用 `GateSlot::panicked`；没有闸门的任务只记日志。
- 没有闸门的任务 panic 时，`Task` 的结果为 `TaskError::Panicked(message)`，工作线程继续运行。

## 数据流与线程

- 工作线程名为 `executor-N`，专用线程名由调用方给出。线程在第一次提交时创建，之后常驻、空闲时阻塞在 `Condvar` 上，不占 CPU。
- executor 本身不做 IO；任务里做什么由提交方决定。UI 线程只提交任务和 drop 句柄，两者都不阻塞。
- 任务结果交给视图：任务在自己的 future 里把结果排到主线程队列（视图层提供），不经过 executor。

## 性能

- 首帧之前：`Executor::new` 只分配结构，不创建线程；第一次提交发生在首帧之后。
- 空闲时：工作线程阻塞，不占 CPU；每个线程的常驻内存是栈实际用到的部分（虚拟 2 MiB），6 个线程合计不到 1 MB 常驻，计入 60 MB 空闲预算。
- 开销：提交一个任务是一次 `Arc` 分配、一次加锁入队和一次 `Condvar` 通知；`bench` 增加「提交并完成 10 000 个空任务」和「1 000 个任务各唤醒 100 次」两项基准，记录耗时，不设硬预算。
- 取消：句柄 drop 只设标记并唤醒，不等任务结束；取消对 UI 线程是 O(1)。

## 状态与异常

- 任务 panic：不展开到工作线程；有闸门的按上一节交给 kernel，没有闸门的变为 `TaskError::Panicked`。
- shutdown 之后提交：返回已取消的句柄，future 在调用线程上于 `contain` 里 drop。
- 工作线程意外退出（`on_thread_start` 钩子 panic）：钩子在 `contain` 里调用，panic 只记日志，线程照常工作。

## 测试

全部在 Linux 上运行（`just test -p lectern-executor`），用真实的 `lectern-kernel`：

- 唤醒只入队：在持有一把锁时唤醒任务，任务的 future 在 poll 或 drop 时要取同一把锁，唤醒照常返回，不死锁。
- 不重入：任务 poll 期间被另一个线程反复唤醒，任务不会同时在两个线程上被 poll，返回 `Pending` 后恰好再被 poll 一次。
- 克隆走的 waker：任务 panic 期间另一个线程调用之前克隆的 waker，任务不会再被 poll。
- 闸门：闸门在 `GateSlot::poll` 的检查和登记之间打开，任务仍被调度；闸门未打开时任务不运行；激活失效时挂起的任务被唤醒并丢弃。
- panic：任务 panic 后插件进入 `Failed`、工作线程继续运行其他任务；payload 的 `Drop` 要等 future 持有的锁时，future 先被 drop，工作线程不卡住；future 的 drop panic 时插件进入 `Failed`。
- 闸门封住后提交：`spawn_gated` 返回已取消的句柄，future 没有被 poll。
- 优先级：`Background` 任务占满其余线程时，`Interactive` 任务仍在保留的线程上运行。
- 取消：drop 句柄后任务不再被 poll，future 被丢弃；`.await` 一个被取消的任务得到 `TaskError::Cancelled`。
- shutdown：挂起的任务被丢弃，之后的提交返回已取消的句柄，`shutdown` 在工作线程退出后返回。
- `manual()`：`run_until_idle` 按入队顺序运行，结果确定。
- 首帧：`Executor::new` 之后、第一次提交之前，进程里没有 `executor-*` 线程。

## 验收

- `just test -p lectern-executor` 通过，覆盖「测试」一节的每一条。
- `python3 scripts/check_architecture.py` 通过：executor 只依赖 kernel 层，不依赖 GPUI 和 `platform`。
- `rg 'unsafe' crates/executor/src` 没有结果。
- kernel 的 `own_with` 配合 `Executor::spawn_gated` 的集成测试通过（在 executor 的测试里）。

## 未决问题

没有需要产品决定的问题。
