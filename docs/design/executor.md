# executor 设计

状态：已实现（设计 [#60](https://github.com/SoloJiang/lectern-app/pull/60)、[#104](https://github.com/SoloJiang/lectern-app/pull/104)，实现 [#67](https://github.com/SoloJiang/lectern-app/pull/67)、[#PR](https://github.com/SoloJiang/lectern-app/pull/PR)）。批量线程组（[ADR 0029](../adr/0029.md)）在 [#PR](https://github.com/SoloJiang/lectern-app/pull/PR) 实现。

`executor` 运行 UI 线程之外的 IO、解析和 git 操作（[技术调研「不同步阻塞 UI」](tech-research.md)）。它实现 kernel 对执行器的约定（[kernel 设计](kernel.md)「撤销」里的后台任务、`SubmitGate`、`GateSlot`），让插件提交的任务随激活失效而丢弃、任务 panic 时插件进入 `Failed`。

## 范围

- 做：
  - 固定数量的工作线程和两级优先级的任务队列。
  - 一组线程数较少的批量线程，只运行 `Priority::Batch` 的任务（可暂停的后台工作，[ADR 0029](../adr/0029.md)）。
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
/// 创建时不启动线程：第一次提交 `Interactive` / `Background` 任务时才启动工作线程，第一次提交 `Batch` 任务时才启动批量线程
/// （ADR 0017：首帧之前不提交任务；ADR 0029）。
#[derive(Clone)]
pub struct Executor { /* Arc<Inner> */ }

pub struct Config {
    /// 工作线程数。默认 `clamp(available_parallelism - 1, 2, 6)`。0 按 1 处理；只有 1 个时不保留 `Interactive` 线程。
    pub workers: usize,
    /// 批量线程数。默认 `clamp(available_parallelism / 4, 1, 3)`，0 按 1 处理（ADR 0029）。
    pub batch_workers: usize,
    /// 每个工作线程、批量线程和专用线程启动时在该线程上调用，`app` 用它设置 QoS 和线程名之外的属性。
    /// `app` 经 `platform` 把 `Batch` 线程设为 utility QoS，`Worker` 的设置不变。
    pub on_thread_start: Option<Arc<dyn Fn(ThreadKind) + Send + Sync>>,
}

impl Default for Config { /* workers、batch_workers 取上面的默认值，on_thread_start 为 None */ }

pub enum ThreadKind { Worker, Batch, Dedicated }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    /// 用户正在等的结果：当前审阅会话的 diff、打开的文件。
    Interactive,
    /// 扫描、刷新，以及持有 `BusyGuard` 的索引工作（第 2 层）。
    Background,
    /// 可暂停的长时间批量工作（建索引的第 3–5 层，ADR 0029）：只在批量线程上运行，
    /// 不进 `Interactive` / `Background` 的队列，也不占用工作线程。
    Batch,
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

    /// 插件经 `own_with` 提交长时间阻塞的工作时用（jdtls 的管道读取、文件监听）。与 `spawn_gated` 一样在本调用里
    /// 同步申请 `GateSlot`，申请不到时不建线程，在 `contain` 里丢弃闭包，返回已取消的句柄。
    /// 专用线程先等闸门打开再运行闭包，见「与 kernel 的约定」。
    pub fn run_dedicated_gated<T: Send + 'static>(
        &self,
        gate: &SubmitGate,
        name: &str,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Task<T>;

    /// 停止接收新任务（之后的提交返回已取消的句柄），按存活任务登记表取消所有未结束的任务，包括挂起的任务；
    /// 它们的 future 由工作线程在 `contain` 里 drop，不在调用 `shutdown` 的线程上 drop。等工作线程退出，最多等 `timeout`。
    /// 专用线程不等：还在等闸门的专用任务被唤醒并丢弃闭包，正在运行的闭包结束时线程自行退出。
    pub fn shutdown(&self, timeout: Duration) -> Shutdown;
}

/// `busy_workers` 是超时时还没退出的工作线程和批量线程数之和。
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

/// 实现 `Display` 和 `std::error::Error`。有闸门的任务 panic 时，句柄同样得到 `Panicked`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskError {
    Cancelled,
    Panicked(String),
}
```

测试辅助在 `test-support` feature 下：

```rust
impl Executor {
    /// 不启动任何工作线程的执行器：任务只在调用 `run_until_idle` 时在调用线程上运行，顺序确定。
    /// `shutdown` 在调用线程上运行被取消的任务、丢弃它们的 future；专用任务仍然各自建线程。
    pub fn manual() -> Executor;
    /// 依次运行队列里的任务，直到队列为空：先 `Interactive`，再 `Background`，最后 `Batch`，同一队列按入队顺序。返回运行的步数。
    pub fn run_until_idle(&self) -> usize;
}
```

## 数据结构

每个任务是一个 `Arc<TaskCell>`：

```rust
struct TaskCell<T> {
    state: AtomicU8,                       // Idle / Queued / Running / RunningWoken / Done
    future: Mutex<Option<Pin<Box<dyn Future<Output = T> + Send>>>>,
    slot: Mutex<Option<GateSlot>>,         // spawn_gated 才有；写为 Done 之后取出 drop
    priority: Priority,
    cancelled: AtomicBool,
    output: Output<T>,                     // Mutex 里的结果、等待结果的 waker、句柄是否已放弃
}
```

专用任务是 `Arc<DedicatedCell>`，字段相同，`future` 换成 `Mutex<Option<Box<dyn FnOnce() -> T + Send>>>`，另有 `thread: Mutex<Option<Thread>>`：线程启动后先在锁下登记自己再读 `cancelled`，取消时在同一把锁下取线程并 `unpark`，所以取消要么被线程看到，要么唤醒它。

句柄 drop 或 `detach` 时标记 `output` 为已放弃：之后的结果在结束任务的线程上于 `contain` 里 drop，不留在 `TaskCell` 里等最后一个引用释放。这时结果已经存进 `output` 的（任务先结束、句柄后放弃），句柄把它作为一个 `Background` 任务交给执行器，由工作线程（`shutdown` 之后由 `executor-reclaim` 线程）在 `contain` 里 drop：放弃句柄只是一次加锁入队，`T` 的 `Drop` 从不在放弃句柄的线程（可能是 UI 线程）上运行。`Err` 只带消息、没有 drop glue 的 `T`（`std::mem::needs_drop` 为假）不运行任何代码，这两种当场 drop，不另排任务。

- `Waker` 由 `TaskCell` 实现 `Wake` 得到。`wake` 只改状态并入队：`Idle → Queued` 时放进对应优先级的队列；`Running → RunningWoken` 只设标记；`Queued`、`RunningWoken`、`Done` 时什么也不做。所以 `wake` 从不在调用线程上 poll 或 drop future，也不会让一个任务同时被两个线程 poll。
- 工作线程取出任务后把 `Queued` 换成 `Running` 再 poll；poll 返回 `Pending` 时用 compare-exchange 把 `Running` 换回 `Idle`，失败（期间被唤醒，状态是 `RunningWoken`）就换成 `Queued` 重新入队。
- 任务结束（返回 `Ready`、被取消、闸门返回 `Drop`、panic）时状态写为 `Done`，之后的唤醒一律忽略。`Done` 之后再丢弃 `GateSlot`，闸门据此回收登记。
- 调度状态放在一把锁里，四个 `Condvar` 与这把锁配对：普通工作线程等 `work`，保留线程等 `reserved`，批量线程等 `batch_work`，`shutdown` 等 `exited`：

  ```rust
  struct Sched {
      interactive: VecDeque<Arc<dyn Job>>,
      background: VecDeque<Arc<dyn Job>>,
      batch: VecDeque<Arc<dyn Job>>,             // 只有批量线程取
      live: HashMap<TaskId, Arc<dyn Job>>,       // 两组线程的存活池任务：提交时插入，结束后移除
      dedicated: HashMap<TaskId, Arc<dyn Job>>,  // 存活的专用任务
      shutdown: bool,
      workers: PoolState,                        // 工作线程组
      batch_pool: PoolState,                     // 批量线程组
  }

  struct PoolState {
      started: bool,                             // 这一组的第一次提交时置位并启动这一组的线程
      starting: bool,                            // 那次提交还在逐个建线程
      alive: usize,                              // 已启动、尚未退出的线程数
      threads: Vec<JoinHandle<()>>,              // 已启动的线程，建好后立即记入
  }

  trait Job: Send + Sync {
      fn run(self: Arc<Self>);      // 池任务：一步 poll；专用任务：整个线程体
      fn cancel(self: Arc<Self>);   // 设 cancelled 并唤醒，不在调用线程上 drop
      fn discard(self: Arc<Self>);  // 没交给工作线程的任务：在 contain 里 drop，结束为 Cancelled
  }
  ```

  入队、出队、检查 `shutdown` 和工作线程的等待条件都在这把锁下进行，入队后在同一把锁释放前通知：`Background` 通知一次 `work`，`Interactive` 各通知一次 `work` 和 `reserved`，`Batch` 通知一次 `batch_work`。所以不会出现一个工作线程检查完队列为空、还没开始等待时错过入队通知的情况；保留线程和 `shutdown` 的等待也不会吞掉发给普通工作线程的通知。工作线程先取 `Interactive`；保留一个工作线程（`executor-0`）只取 `Interactive`，后台工作把其余线程占满时，用户等的任务仍有线程可用。
- 每组线程的第一次提交在同一次加锁里设这一组的 `started` 和 `starting`，然后逐个建这一组的线程（下面的规则对两组相同）：每建一个之前在锁下检查 `shutdown` 并把 `alive` 加一，建好后在锁下记入 `threads`（失败时记日志、`alive` 减一），全部建完才清掉 `starting` 并通知 `exited`。`shutdown` 等到两组的 `starting` 都清掉、`alive` 都归零，才在同一把锁下取走两组的 `threads` 去 join，所以不会有线程在 `Clean` 之后才被记入或才建出来。建线程期间开始的 `shutdown` 让后面的线程不再建；只有一个都还没建、又有被取消的任务等着 drop 时才建第一个。`shutdown` 之后保留线程也取 `Background` 任务（`Background` 入队时一并通知它），因为它可能是唯一建出来的线程，剩下的工作只是 drop 被取消的任务。
- 登记表持有任务的强引用，任务写为 `Done` 之前 `TaskCell` 不会因为句柄和 waker 都被 drop 而在任意线程上释放；future 只在工作线程上（或 `manual()` 的 `run_until_idle` 里）被 drop。
- shutdown：在锁下设 `shutdown`，复制一份登记表；释放锁后对每个任务设 `cancelled` 并唤醒它，挂起（`Idle`）的任务因此入队，工作线程取出后发现已取消，在 `contain` 里 drop future 并写为 `Done`。工作线程和批量线程在 `shutdown` 已设、`live` 为空时退出，每次移除登记都检查这个条件并通知所有等待的线程。线程退出前把所在组的 `alive` 减一并通知 `exited`；两组的 `starting` 清掉、`alive` 归零后 `shutdown` 再 join 这些线程（此时它们已离开循环，join 只等线程收尾，线程局部变量的析构也已完成），然后返回 `Clean`。

## 与 kernel 的约定

`spawn_gated` 提交的任务按 [kernel 设计](kernel.md) 处理：

- 每次 poll future 之前调用 `GateSlot::poll(cx)`：`Run` 才 poll；`Wait` 返回 `Pending`（闸门打开时 kernel 唤醒它）；`Drop` 在 `contain` 里丢弃 future，任务结束。
- poll 用 `kernel::catch` 包住。panic 时顺序固定：先把状态写为 `Done`，再在 `contain` 里 drop future，然后 `PanicPayload::dispose` 取出消息，最后调用 `GateSlot::panicked(message)`。
- future 的每一次最终 drop（完成、取消、闸门关闭、shutdown 丢弃）都在 `contain` 里进行，panic 时调用 `GateSlot::panicked`；没有闸门的任务只记日志。
- 没有闸门的任务 panic 时，`Task` 的结果为 `TaskError::Panicked(message)`，工作线程继续运行。

`run_dedicated_gated` 在专用线程上执行同一套规则，没有 future，闸门检查只在运行闭包之前做一次：

- 专用线程用一个唤醒时 `unpark` 本线程的 waker 调用 `GateSlot::poll`：`Wait` 时 `park`，被唤醒后再检查；`Run` 时运行闭包；`Drop`（闸门关闭、激活失效）或任务被取消（句柄 drop、shutdown）时不运行，在 `contain` 里丢弃闭包，panic 时调用 `GateSlot::panicked`。
- 闭包在 `kernel::catch` 里运行。panic 时闭包捕获的值在展开时已 drop，然后 `PanicPayload::dispose` 取出消息，接着写为 `Done`（结果 `Panicked(message)` 对句柄可见），最后调用 `GateSlot::panicked(message)`。专用任务没有 waker 会把它重新排队，`Done` 只决定结果何时可见，所以它排在 `dispose` 之后：结果里要带消息。
- 闭包开始运行后 executor 不能中断它。闭包要能随激活失效而结束，比如把子进程登记为插件的注册项，激活失效时 kernel 撤销注册项、关闭子进程，闭包里阻塞的读取随之返回。
- 闭包结束（或被丢弃）后写为 `Done`，再 drop `GateSlot`，线程退出。

## 数据流与线程

- 工作线程名为 `executor-N`，专用线程名由调用方给出。线程在第一次提交时创建，之后常驻、空闲时阻塞在 `Condvar` 上，不占 CPU。
- 批量线程名为 `executor-batch-N`，有自己的队列，在第一次提交 `Batch` 任务时才创建，所以只打开空窗口时不存在；只提交 `Batch` 任务时也不创建工作线程。工作线程不取 `Batch` 任务（`shutdown` 之后也不取），批量线程也不取另外两级的任务，所以批量工作占满批量线程时，交互和初始化任务仍有全部工作线程可用。批量线程组没有保留线程。闸门、取消、panic 和 shutdown 的规则与工作线程相同：被取消的 `Batch` 任务由批量线程在 `contain` 里 drop；`Batch` 任务的句柄放弃时已存好的结果也交给批量线程 drop，不为此启动工作线程。`manual()` 的 `run_until_idle` 在 `Background` 之后运行 `Batch`。
- executor 本身不做 IO；任务里做什么由提交方决定。UI 线程只提交任务和 drop 句柄，两者都不阻塞。
- 任务结果交给视图：任务在自己的 future 里把结果排到主线程队列（视图层提供），不经过 executor。

## 性能

- 首帧之前：`Executor::new` 只分配结构，不创建线程；第一次提交发生在首帧之后。
- 空闲时：工作线程阻塞，不占 CPU；每个线程的常驻内存是栈实际用到的部分（虚拟 2 MiB），6 个线程合计不到 1 MB 常驻，计入 60 MB 空闲预算。没有提交过 `Batch` 任务时批量线程不存在，批量线程组只多出空的队列和计数，不分配堆内存；批量线程启动后最多 3 个，同样空闲阻塞。
- 开销：提交一个任务是一次 `Arc` 分配、一次加锁入队和一次 `Condvar` 通知；`bench` 增加「提交并完成 10 000 个空任务」和「1 000 个任务各唤醒 100 次」两项基准，记录耗时，不设硬预算。这两项随 `bench` crate 的设计一起在 [#7](https://github.com/SoloJiang/lectern-app/issues/7) 加入，见「与设计的差异」。
- 取消：句柄 drop 只设标记并唤醒，不等任务结束；取消对 UI 线程是 O(1)。

## 状态与异常

- 任务 panic：不展开到工作线程；有闸门的按上一节交给 kernel，没有闸门的变为 `TaskError::Panicked`。
- shutdown 之后提交：返回已取消的句柄。future 或闭包交给一个新建的分离线程 `executor-reclaim`，在那里于 `contain` 里 drop，提交调用不等它，所以 UI 线程在退出过程中提交也不会被 future 的 `Drop` 阻塞。shutdown 之后的提交只在退出过程中出现，按次建线程的开销可以接受。
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
- 批量线程组：`Batch` 任务只在批量线程上运行，其他任务从不在批量线程上运行；第一次提交 `Batch` 之前没有批量线程；默认线程数按 `clamp(available_parallelism / 4, 1, 3)`（注入并行度，不依赖本机）；批量线程占满时 `Interactive` 和 `Background` 任务照常完成；`shutdown` join 批量线程；启动钩子在批量线程上以 `ThreadKind::Batch` 调用。
- 取消：drop 句柄后任务不再被 poll，future 被丢弃；`.await` 一个被取消的任务得到 `TaskError::Cancelled`。
- 队列通知不丢失：多个线程并发提交、工作线程反复在空队列上等待，所有任务都运行完，没有任务停在队列里。
- shutdown：挂起的任务（不在任何队列里、等着被唤醒）也被丢弃，future 在工作线程上 drop；`shutdown` 在工作线程退出后返回；任务阻塞不结束时返回 `TimedOut`。
- shutdown 之后提交：`spawn` 立即返回已取消的句柄；future 的 `Drop` 阻塞在一把锁上时，提交线程不受影响，drop 发生在 `executor-reclaim` 线程上。
- 带闸门的专用任务：闸门未打开时闭包不运行；打开后运行；闸门关闭或激活失效时闭包被丢弃、没有运行；闭包 panic 时插件进入 `Failed`；闸门封住后提交返回已取消的句柄，没有新建线程。
- `manual()`：`run_until_idle` 按入队顺序运行，结果确定。
- 首帧：`Executor::new` 之后、第一次提交之前，进程里没有 `executor-*` 线程。

## 验收

- `just test -p lectern-executor` 通过，覆盖「测试」一节的每一条。
- `python3 scripts/check_architecture.py` 通过：executor 只依赖 kernel 层，不依赖 GPUI 和 `platform`。
- `rg 'unsafe' crates/executor/src` 没有结果。
- kernel 的 `own_with` 配合 `Executor::spawn_gated` 的集成测试通过（在 executor 的测试里）。

## 未决问题

没有需要产品决定的问题。

## 实际验证

在 Linux 上运行（executor 不依赖 GPUI，macOS 上是同一份代码；本 PR 没有在 macOS 上实测）：

- `just check`（格式、架构规则、文档链接、clippy `-D warnings`）、`just test -p lectern-executor`（45 个测试）、`just doctest`、`just deny` 全部通过。`rg unsafe crates/executor/src` 没有结果。
- `just test -p lectern-executor` 连续运行 40 次、用 `taskset -c 0` 限制在单核上再运行 15 次，全部通过；`cargo test`（所有测试在同一进程的多个线程里）也通过。
- 批量线程组（[#PR](https://github.com/SoloJiang/lectern-app/pull/PR)）：`just check`、`just test`、`just doctest`、`just deny` 全部通过，`just test -p lectern-executor` 共 61 个测试；同样连续运行 40 次、`taskset -c 0` 单核 15 次，`cargo test` 同一进程，全部通过。`just bench --measure executor/` 改动前后在同一台 4 核 Linux 容器上各跑 3 次，中位数：`executor/spawn_10k` 改动前 21.1、36.3、18.3 ms，改动后 12.5、14.3、17.2 ms（两边都标为 noisy）；`executor/wake_1000x100` 改动前 64.2、69.6、73.9 ms，改动后 64.0、53.1、55.2 ms。差别在同一版本多次运行的波动之内，没有变慢（只记录，不设预算）。两项都只提交 `Background` 任务，不创建批量线程。
- 测试用真实的 `lectern-kernel`（dev-dependency 打开 `test-support`，用 `Kernel::start` 和 `Kernel::barrier`），等待一律用通道或 `block_on`，超时只作为失败保护；只有插件状态、kernel 静止这类没有事件可等的地方按 1 ms 间隔轮询，上限 10 秒。

「测试」一节逐条对应（`crates/executor/src/tests/`）：

- 唤醒只入队：`waking::waking_or_cancelling_while_holding_the_tasks_lock_does_not_deadlock`。
- 不重入：`waking::a_task_woken_while_running_is_polled_once_more_and_never_on_two_threads`（每次 poll 期间另一线程唤醒 1000 次，共 poll 恰好 3 次）。
- 克隆走的 waker：`waking::a_cloned_waker_called_while_the_task_panics_does_not_poll_it_again`。
- 闸门：`gates::a_gate_that_opens_while_the_task_checks_it_still_schedules_the_task`（300 次 `own_with`，工作线程的第一次检查与闸门打开竞争）、`gates::a_gated_task_does_not_run_before_the_gate_opens`、`gates::suspended_gated_tasks_are_dropped_when_the_activation_dies`。
- panic：`panics::a_panicking_gated_task_fails_the_plugin_and_the_worker_keeps_running`（只有 1 个工作线程）、`panics::a_panicking_task_drops_its_future_before_the_payload_and_reports_last`（payload 用 `panic_any` 抛出，它的 `Drop` 等 future 持有的资源；同时检查 future drop 时激活还没有失效）、`panics::a_panic_dropping_a_finished_future_fails_the_plugin`、`panics::a_panic_dropping_a_cancelled_future_fails_the_plugin`。
- 闸门封住后提交：`gates::spawn_gated_on_a_sealed_gate_returns_a_cancelled_handle_without_polling`、`gates::a_panic_dropping_a_future_refused_by_a_sealed_gate_fails_the_plugin`。
- 优先级：`scheduling::interactive_tasks_run_on_the_reserved_worker_while_background_work_fills_the_rest`。
- 批量线程组（`batch.rs`）：
  - 只在批量线程上运行：`batch::batch_tasks_run_only_on_batch_threads_and_other_tasks_never_do`（2 个工作线程、2 个批量线程，300 个会自我唤醒的 `Batch` 任务和 300 个 `Interactive` / `Background` 任务交错提交；每个任务读线程名和启动钩子记在线程局部变量里的 `ThreadKind`）。
  - 第一次提交才创建、钩子带 `Batch`：`batch::batch_threads_start_on_the_first_batch_submission_with_the_batch_hook`（先跑 `Interactive` 和 `Background`，这时 `batch_pool.threads` 为空、钩子只见过 `Worker`；提交 `Batch` 后 3 个 `executor-batch-N` 以 `ThreadKind::Batch` 调用钩子，钩子在任务之前运行）、`batch::batch_only_work_starts_no_worker_thread`（只提交 `Batch` 时不建工作线程，放弃句柄时已存好的结果在 `executor-batch-0` 上 drop）、`batch::a_submission_after_shutdown_is_cancelled_and_starts_no_batch_thread`；`crates/executor/tests/first_frame.rs` 读 `/proc/self/task/*/comm`，只提交 `Background` 时只有 `executor-0`、`executor-1`，提交 `Batch` 后才多出两个批量线程（Linux 只保留线程名的前 15 字节，读到的是 `executor-batch-`）。
  - 线程数：`batch::the_default_batch_thread_count_is_a_quarter_of_the_parallelism_clamped_to_one_to_three`（`Config::for_parallelism` 注入 1、2、3、4、7、8、11、12、16、64，同时核对工作线程数）、`batch::the_configured_number_of_batch_threads_start_and_zero_counts_as_one`。
  - 不拖慢交互任务：`batch::interactive_and_background_work_is_not_delayed_by_saturated_batch_threads`（2 个批量线程都阻塞、另有 20 个 `Batch` 排队时，100 个 `Interactive` / `Background` 任务全部完成，排队的 `Batch` 一个都没被工作线程取走）。
  - shutdown：`batch::shutdown_joins_the_batch_threads`（钩子在每个批量线程上放一个线程局部变量，`shutdown` 返回时 3 个析构都已运行）、`batch::shutdown_drops_suspended_batch_tasks_on_a_batch_thread`、`batch::shutdown_times_out_while_a_batch_task_blocks`（只启动了批量线程组，`busy_workers` 为 1）、`batch::a_shutdown_during_batch_startup_stops_further_spawns_and_joins_the_started_ones`。
  - 取消、panic、闸门：`batch::dropping_a_batch_handle_drops_the_future_on_a_batch_thread`、`batch::a_panicking_batch_task_becomes_the_result_and_the_batch_thread_keeps_running`、`batch::a_gated_batch_task_runs_after_start_and_its_panic_fails_the_plugin`、`batch::deactivating_the_plugin_drops_its_suspended_batch_tasks_on_a_batch_thread`。
  - `manual()`：`batch::run_until_idle_runs_batch_tasks_after_background_ones`。
- 取消：`cancel::dropping_the_handle_stops_polling_and_drops_the_future`、`cancel::awaiting_a_cancelled_task_gives_cancelled`；已存好的结果不在放弃句柄的线程上 drop：`cancel::an_unread_result_is_dropped_by_the_executor_not_by_the_thread_dropping_the_handle`（drop、`detach` 和 `Drop` 会 panic 的结果）、`cancel::an_unread_result_left_after_shutdown_is_dropped_on_the_reclaim_thread`。
- 队列通知不丢失：`scheduling::concurrent_submissions_all_run_while_workers_keep_going_idle`（20 轮，每轮 4 个线程各提交 50 个会自我唤醒的任务，轮与轮之间工作线程回到空队列上等待）。
- shutdown：`shutdown::shutdown_drops_suspended_tasks_on_a_worker`、`shutdown::shutdown_returns_after_the_workers_exit`（钩子在每个工作线程上放一个线程局部变量，`shutdown` 返回时三个析构都已运行）、`shutdown::shutdown_times_out_while_a_task_blocks`、`shutdown::shutdown_drops_a_dedicated_task_still_waiting_for_its_gate`、`shutdown::a_shutdown_during_worker_startup_stops_further_spawns_and_joins_the_started_workers`（用测试专用的 `before_spawn` 钩子让第一次提交停在建第二个线程之前，在这时 `shutdown`：它等启动结束才返回 `Clean`，之后没有再建线程，建出的那个已 join）。
- shutdown 之后提交：`shutdown::a_submission_after_shutdown_is_cancelled_and_dropped_on_the_reclaim_thread`。
- 带闸门的专用任务：`dedicated::a_gated_dedicated_closure_waits_for_the_gate_then_runs`、`dedicated::a_gated_dedicated_closure_is_dropped_unrun_when_the_gate_closes`、`dedicated::a_gated_dedicated_closure_is_dropped_unrun_when_the_activation_dies`、`dedicated::a_panicking_gated_dedicated_closure_fails_the_plugin`、`dedicated::run_dedicated_gated_on_a_sealed_gate_starts_no_thread`，另有句柄 drop 时丢弃等闸门的闭包。
- `manual()`：`manual::run_until_idle_runs_tasks_in_queue_order`。
- 首帧：`scheduling::no_thread_starts_before_the_first_submission`（用线程启动钩子计数，各平台都跑）；`crates/executor/tests/first_frame.rs` 在 Linux 上读 `/proc/self/task/*/comm`，确认第一次提交前没有 `executor-*` 线程、`shutdown` 之后线程消失。它单独一个测试二进制，不和其他测试共用进程。
- kernel 集成：`kernel::tasks_submitted_through_own_with_run_after_start_returns`（`own_with` + `spawn_gated`，任务在 `start` 返回后才运行，带走的 `BusyGuard` 释放后 kernel 静止）、`kernel::deactivating_the_plugin_drops_its_pending_tasks_on_a_worker`（停用时 kernel drop 任务句柄，future 在工作线程上 drop）。

没有验证到的：

- 「性能」一节的两项基准：推迟到 [#7](https://github.com/SoloJiang/lectern-app/issues/7)，见下一节。
- `on_thread_start` 与 `platform` 的 QoS 设置的配合：`app` 还没有创建 `Executor`，接入时在 macOS 上验证。批量线程的 utility QoS 同样随 `platform` 和 `app` 的接入验证，本 PR 不改这两个 crate。
- 批量线程组没有单独的基准：现有两项基准只覆盖工作线程组，`Batch` 任务的吞吐随 `code_intel` 的索引基准衡量。

## 与设计的差异

- 一把锁配三个 `Condvar`（`work`、`reserved`、`exited`），不是一个。只用一个 `Condvar` 时，`notify_one` 可能唤醒只取 `Interactive` 的保留线程或等在 `shutdown` 里的线程，`Background` 任务的通知就丢了；改用 `notify_all` 又会让每次提交唤醒所有空闲线程。分开之后每次入队只通知能处理它的线程。
- `Sched` 增加 `started`、`starting`、`alive` 和 `threads`：`started` 实现第一次提交时启动线程，`alive` 让 `shutdown` 知道还有几个工作线程没退出（也是 `TimedOut { busy_workers }` 的值）。线程句柄放在 `Sched` 里、由 `starting` 标出启动是否结束，否则与第一次提交并发的 `shutdown` 可能在某个线程的句柄记入之前就取走句柄列表返回 `Clean`，之后还会再建线程，线程和它的线程局部变量析构就活过了「干净」的关闭。`shutdown` 返回 `Clean` 时线程已经全部 join。
- `TaskCell::slot` 改为 `Mutex<Option<GateSlot>>`：要在写为 `Done` 之后把它取出来 drop。`output` 增加「句柄已放弃」标记，被 drop 或 `detach` 的句柄的结果在结束任务的线程上于 `contain` 里 drop；放弃句柄时结果已经存好的，交给执行器另排一个任务 drop（设计没有提到这种情况）。
- `Job` trait 写明三个方法（`run`、`cancel`、`discard`），池任务和专用任务都实现它，专用任务也以 `Arc<dyn Job>` 登记在 `dedicated` 里。
- 专用任务 panic 时 `dispose` 在写为 `Done` 之前：结果 `TaskError::Panicked` 要带上消息，而消息只能由 `dispose` 取出。专用任务没有 waker 会让它重新运行，`GateSlot::panicked` 仍在最后调用，所以 kernel 看到的顺序不变。
- 有闸门的任务 panic 时，句柄也得到 `TaskError::Panicked(message)`（设计只写了没有闸门的任务）；插件的句柄由 kernel 保管，不影响插件，但另一个任务 `.await` 它时能拿到原因。
- `run_until_idle` 先运行 `Interactive` 再运行 `Background`，同一队列按入队顺序；设计写的是「按入队顺序」，与工作线程的取任务顺序保持一致更容易对照。`manual()` 的 `shutdown` 在调用线程上运行被取消的任务（没有工作线程可以做这件事），专用任务在 `manual()` 下仍然建线程。
- `Config::workers` 为 0 时按 1 处理；只有 1 个工作线程时不保留 `Interactive` 线程，否则 `Background` 任务永远没有线程运行。
- 增加了设计没写的细节：`TaskError` 实现 `Display` 和 `Error`；`Task` 标 `#[must_use]`（drop 即取消）；`Task` 在结果被取走后再次 poll 得到 `Err(Cancelled)`，不 panic；专用线程建不起来时记日志、在调用线程上于 `contain` 里丢弃闭包、句柄为已取消；`executor-reclaim` 线程建不起来时记日志并泄漏这个任务（与 kernel 的 payload 回收相同），不在提交线程上 drop。
- 「性能」一节的两项基准没有加：`bench` crate 还没有设计文档和基准框架（`crates/bench/AGENTS.md`），随 [#7](https://github.com/SoloJiang/lectern-app/issues/7) 一起加入。bench 第 1 步已加入 `executor/spawn_10k`、`executor/wake_1000x100`（只记录），只用公开接口。
- 日志写到 stderr（`lectern-executor:` 前缀），与 kernel 相同，由 `app` 重定向。
- 批量线程组（[#PR](https://github.com/SoloJiang/lectern-app/pull/PR)）：
  - 两组线程各自按需启动：工作线程在第一次提交 `Interactive` / `Background` 时启动，不再是「第一次提交任何任务」。只有 `Batch` 工作时不建工作线程，空闲内存和线程数都更少。启动、退出的记账从 `Sched` 移到每组一个的 `PoolState`，两组的规则相同。
  - `Batch` 任务的句柄放弃时，已存好的结果交给批量线程 drop（设计只写了「交给执行器另排一个 `Background` 任务」）：批量线程组这时一定已经启动，交给 `Background` 会只为 drop 一个结果去启动工作线程。
  - `shutdown` 之后工作线程也不取 `Batch` 任务：被取消的 `Batch` 任务只由批量线程 drop，与「工作线程不取 `Batch` 任务」一致；只启动了工作线程时不会有 `Batch` 任务存活。`TimedOut { busy_workers }` 计入批量线程。
  - 默认线程数由私有的 `Config::for_parallelism(n)` 计算，`Default` 用本机的 `available_parallelism` 调用它，测试注入并行度。
  - 测试专用的 `before_spawn` 钩子多带一个 `ThreadKind`，用来让批量线程组的启动停在建第二个线程之前。
  - `Config` 多了一个字段，`fs_watch` 测试里两处 `Config { .. }` 字面量补上 `batch_workers`。
