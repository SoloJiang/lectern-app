# syntax（`lectern-syntax`）

语法高亮与打开文档的领域库：语言表与高亮查询、着色区间（`Token`、`WindowSpans`）、首屏窗口（`window`）、首屏窗口的解析、分片的整文件解析（`FullParse`）、窗口链（`ChainWindow`）、降级、grammar 的卡死检测（`health`）；文本与行索引（`text`）、行检查点（`syntax.lines`）、8 MiB 的高亮与文本 LRU、对象库内容的登记表、打开的文档（`Engine`、`DocHandle`、`DocSnapshot`）、监听与重新打开、查找（`find`）、空闲预取（`Engine::prefetch`）、档位 B / C 的窗口链与批量窗口、交互让路；编辑模式的领域部分（`EditSession`、增量高亮、保存、冲突、三方合并）。

- 层：领域层，普通的库 crate（不实现 `Plugin`，没有 `plugin = true`，[ADR 0041](../../docs/adr/0041.md)）。`file_reader`、`review_view` 和 `code_intel` 都可以依赖它；它不依赖任何插件 crate，不依赖 GPUI（架构规则 2）、`gix`、`rusqlite`、`ropey`。
- 设计文档：[file_reader 设计](../../docs/design/file_reader.md)的「`syntax` 的公开项」；实现进度见那里的状态行和「验收」。已实现第 1 步（`Languages`、`Lang`、`Token`、`TokenKind`、`WindowSpans`、`health`）和第 2 步（`window::{first_read, checkpoint_read, plan, safe_start}`、`highlight::{window, FullParse, ChainWindow, SlowWindows, Source}`、`limits` 里的档位与窗口常量、`test-support` 的 `testing`）、第 3 步（`text::{Text, LineIndex, LongLine, LineCheckpoints, Pages}`、`find`、`Engine`、`DocHandle`、`DocSnapshot` 与它的 `rows` / `advance` / `at_fraction` / `pos_of_byte` / `total_rows`、LRU、`share_object`、fs_watch 的订阅与重新打开）、第 4 步（`Engine::prefetch`、`limits::{PREFETCH_MAX, BATCH_SEGMENT}`、窗口链 `src/doc/chain.rs`、预取 `src/doc/prefetch.rs`、`test-support` 的 `Engine::set_trace` 与 `Trace`）、第 5 步（`DocHandle::edit`、`EditSession`、`EditState`、`SaveResult`、`MergeOutcome`、`TextEdit`，会话 `src/doc/edit.rs`、增量解析 `src/highlight/incremental.rs`、编辑的合成与文件格式 `src/change.rs`，`test-support` 的 `EditSession::idle`）。第 1 步的 `highlight::whole_file` 已由 `FullParse` 取代。
- issue：[#10](https://github.com/SoloJiang/lectern-app/issues/10)
- ADR：[0018](../../docs/adr/0018.md)（核心高亮语言）、[0030](../../docs/adr/0030.md)（与 `code_intel` 共用 tree-sitter 和 grammar）、[0033](../../docs/adr/0033.md)、[0034](../../docs/adr/0034.md)、[0041](../../docs/adr/0041.md)、[0042](../../docs/adr/0042.md)

## 边界

- 解析和高亮查询只在 executor 的线程上运行，UI 线程不调用本 crate 的阻塞函数。每次调用、每次 `step` 进入 tree-sitter 之前在调用的线程上 `health` 登记这一次的预算，返回时（任何结果）drop 守卫；两次 `step` 之间不在表里。得到 None 时这种语言显示纯文本。
- `window` 模块是纯计算，只看传入的切片；新规则不得从文件开头扫描或用行索引（ADR 0034「首屏路径的准备工作」）。
- 分片的解析（`FullParse`、`ChainWindow`）在两次 `step` 之间保留 tree-sitter 的解析器和查询进度；查询按 8 KiB 的字节块跑，被块尾截断的区间在下一块开头接上，所以分片的结果与一次查询整棵树相同（`highlight::one_shot` 是测试里的对照）。改捕获解析或分块方式时，`deadline_*` 和 `spans_*` 的对照必须仍然逐个 token 相同。
- 窗口链的切点规则在 `src/cut.rs`，与 `crates/code_intel/src/window.rs` 同源；`ERROR` 节点里最后一个子节点的判定不同（见设计「与设计的差异」第 2 步），改其中一边时对照另一边。
- 首帧之前不做任何事：`Languages::core()` 只建表，不编译查询、不做 IO；查询在第一次高亮时编译。
- 核心语言只在 `src/lang.rs` 的 `CORE_LANGUAGES` 和两个 `Cargo.toml` 的 grammar 依赖里列出；名字和扩展名与 `code_intel` 的语言表一致。
- 高亮查询在 `queries/<语言>/highlights.scm`，文件头写上游文件、版本、commit、版权行、这个查询用的优先规则（同一节点被几个模式捕获时取第一个还是最后一个）和 Lectern 的改动；MIT 的许可声明在 `queries/NOTICE`。JavaScript、TypeScript、TSX 由 `ecmascript`、`js`、`jsx`、`ts` 几个文件拼成。
- 捕获名到 `TokenKind` 的映射在 `lang::classify`；改映射、优先规则或按行切分的方式时把 `SPANS_VERSION` 加一（它进查询版本，缓存的区间随之失效）。
- IO 只有三处，都在 executor 的任务里：工作区文件经 `SafeRoot`（`open_file`、`read_ranges`、`read_all`、`stat`），对象库和过滤器的来源经 `src/git_access.rs` 的 `GitAccess`（生产实现转给 `lectern-git`，每个仓库留一个 `git::Repo`），行检查点经 `lectern-cache`。`Engine::open`、`DocHandle` 的命令和 `snapshot` 在 UI 线程上只取锁、入队、复制 `Arc`；`DocSnapshot` 的方法只做与请求的行数成正比的工作（行尾至多往后找 4 KiB，长行用格数检查点）。
- 文档的任务按载入编号（`src/doc/load.rs`）：打开和每次外部改动后的重新打开开始新的一次载入，旧载入的任务落地时丢弃结果。每个任务经插件句柄的 `own_with` 提交、持有 `BusyGuard`。新任务照这个做法写，不在 `Doc` 的锁里提交任务。
- 只存内存的批量工作（ADR 0033）只有两种：预取（`src/doc/prefetch.rs`）和窗口链的批量窗口（`src/doc/chain.rs`）。两者都经 `EngineInner::spawn_yielding` 提交 `Batch` 任务、持有让路的 `BackgroundGuard`（不持有 `BusyGuard`），各自整个引擎同一时刻至多一个任务；预取每一轮一个文件，批量窗口每一轮至多一次 `BATCH_SEGMENT` 的 `step`，每一轮之后 `yield_now()`、排在已在队列里的批量工作之后（与 `code_intel` 的批量循环相同），没有工作时任务结束。不要改成结束再提交新任务：新任务会在旧任务的守卫释放之前入队。任务里不阻塞等待、不取 `code_intel` 的 `LargeSlot`（设计「批量线程上的先后」的活性论证依赖这一点）。解析的让路探针是 `yield_requested() || pause_requested()`，停下时保留解析器的状态。结果只进 LRU（后台一类），不通知文档；批量窗口只在可见部分要它时把区间放进 LRU。预取在 LRU 里查到的条目也按后台一类用（`load::first_screen`、`load::object` 的 `class` 参数），不升为用户打开过的一类；重复预取按身份查到 OID 时不再读文件（`prefetch_*` 检查）。`chain::kick` 和 `chain::land` 不在文档的锁里调用：引擎的调度锁（`EngineInner::chain`、`prefetch`）与文档的锁从不嵌套，LRU 的锁总在最里层。
- 可见窗口按 `chain::want`：链的起点覆盖可见部分时用链的窗口和前后各一个，否则用启发式的窗口；改动时 `doc_tier_b_visible_windows_are_the_chains`、`doc_tier_c_chain_windows_are_read_from_the_file` 的对照必须仍然通过。
- 格数和显示行的规则只在 `src/cells.rs`：行索引（`text::IndexBuilder`）和 `rows` 共用它，改其中一边会让折行和显示的文字对不上。行索引必须能分段喂入、结果与整段喂入相同（`text_streaming_build_matches_one_pass`）。
- 内存的归属（ADR 0035）：LRU 按条目的 `bytes()` 加固定开销计、合计不超过 8 MiB，超过上限的条目不放；对象库内容的登记表只在 `ObjectShare` 存活期间持有强引用，不用 `Weak`；档位 C 的页每个文档至多 8 MiB；`Pages` 不可变，读到新页时换一份。
- 首屏只按核对过的 OID 从 LRU 取整文件的区间（工作区文件：按刚打开的身份记下的 OID；对象库内容：`expect`），完整读取算出 OID 之后再核对一次；外部改动之后的重新打开不用 `expect`（`load::reload`）。可见区域、页和窗口都按顶部显示行自己的字节算（`DocSnapshot::visible_bytes`），不按它所在行的行首。
- 编辑会话（`src/doc/edit.rs`）：UI 线程上只有 `changed`（合成编辑、按编辑平移区间，`WindowSpans::shifted`）、`spans`、`state`，其余排队。解析一个任务、磁盘一个任务，各自同一时刻至多一个：解析一轮做完它开始时的那份文本，期间到达的编辑合成到下一轮，落地时区间按它们平移；磁盘的保存、合并、事件后的检查和结束按顺序做，检查不会看到半个保存。会话活着时 `watch::check` 把事件交给会话，不重新打开文档；`close_project` 跳过这样的文档。会话结束时已排队的保存照常写入，`Close` 做完之前 `DocHandle::edit` 返回 `Reloading`。检查比较的是磁盘上的字节和按会话格式编码的基准，只改 BOM 或换行也算改动。两把锁只按「文档的锁在外、会话的锁在内」嵌套（`edit::begin` 取会话当前的缓冲区），持有会话的锁时不取文档的锁。增量查询只重跑编辑碰到的行（含新终点所在的行）和 `changed_ranges` 的行，改平移或拼接时 `edit_incremental_spans_are_their_trees_*` 的逐行对照必须仍然通过。冲突标记 `editor`、`disk` 是文件内容，不走 i18n。
- 行检查点（`text::LineCheckpoints`）只为 ≥ 1 MiB 的内容写，键是原始字节的 OID；命名空间 `syntax.lines` 的版本在编码或计算方式改变时加一，并改 [cache 设计「命名空间」](../../docs/design/cache.md#命名空间)的那一行。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-syntax     # 只改本 crate 时最快的反馈
just test -p lectern-syntax
just doctest                      # ParseGuard 不是 Send 的 compile_fail 示例
```

- 测试在 `src/tests/`，按前缀分文件：`lang_*`（`lang.rs`：扩展名、规则族、版本对照 Cargo.lock、ABI 范围、查询能编译、捕获名都认识、查询文件头）、`spans_*`（`spans.rs`：每种语言一份整文件区间的 insta 快照，经 `FullParse` 并与 `one_shot` 逐个 token 对照；`Token` 的编码边界、按行存放、捕获的解析）、`health_*`（`health.rs`）、`window_*`（`window.rs`：首屏读取的截取、安全位置的规则、JSON 的补齐、`plan`、窗口链的切点，`window_plan_snapshot` 是各形状的 insta 快照）、`consistency_*`（`consistency.rs`：首屏窗口和窗口链对整文件解析逐字节比较，合成形状 100% 一致）、`deadline_*`（`deadline.rs`：超时、取消、让路，分片续做与一次解析相同，每次 `step` 按自己的预算登记 `health`、两次之间不在表里）、`degrade_*`（`degrade.rs`：NUL、无效 UTF-8、`SlowWindows`；窗口的解析用量按 `health` 的时钟计，测试用 `ManualClocks` 在进度回调里推进 CPU 时间或墙上时间）、`text_*`（`text.rs`：行索引对照朴素扫描、分段喂入与整段相同、长行的显示行对照朴素的折行、CRLF / BOM / 控制字符、64 MiB 的虚拟文件只在内存里放几页时可见行照样完整）、`lines_*`（`lines.rs`：编码、≥ 1 MiB 才写、按 OID 读回、长度不同时不用）、`lru_*`（`lru.rs`：上限与淘汰的先后、打开的文档不受淘汰影响）、`doc_*`（`doc.rs`：四种 `Anchor`、各档位、完整读取看到另一个版本、`Byte` 的 OID 不同、不读内容的条目、fs_watch 事件后重新打开、删除与恢复、档位 C 的扫描与页的上限、drop 后描述符关闭、改写后重新打开的区间、OID 不对的 LRU 区间被丢掉、重新载入时旧的查找不发布、长行深处的页与窗口、依赖根的越界）、`object_*`（`object.rs`：登记、注销、整份取出、超过 16 MiB）、`find_*`（`find.rs`：只对 ASCII 字母不区分大小写、跨块、10,000 的上限、新查询取代旧的）、`prefetch_*`（`prefetch.rs`：至多 4 个、新请求替换没开始的、同一时刻一个任务、结果只进 LRU 且首屏直接用、超过 1 MiB 的第一屏窗口与行检查点、让路时停在分片边界）、`batch_order_*`（`batch_order.rs`：真实的 1 个和 3 个批量线程加模拟的 `code_intel` 批次：预取在每个线程的一批加一个窗口任务之内开始、窗口任务至多 50 ms 并与批次轮流、64 MiB 的链运行时批次照常前进、在途窗口在一片之内停下）、`edit_*` / `save_*` / `merge_*`（`edit.rs`：随机编辑下增量解析拼出的区间与对增量树整棵查询一次相同、不分片时增量树与 tree-sitter 自己的增量解析相同，合法编辑下与整文件解析相同；`changed` 之后区间当场平移；一轮期间到达的编辑；BOM 与 CRLF 的区间和保存；冲突、Save Anyway、编辑期间的事件、会话结束后文档重新读取；三方合并的 insta 快照）。`doc_*` 里另有链的两条：档位 B 的可见窗口是链的窗口、与整文件解析一致、再次打开时从起点表接上；档位 C 的链窗口从文件读。`tests/mod.rs` 有共用的 `full`、`chain`、`kinds`、`differences`。
- 文档的测试用 `src/tests/rig.rs` 的 `Rig`：临时目录里的真实文件、`Executor::manual()`、`testing::host()` 的插件句柄、临时目录里的 cache、fs_watch 的 `testing::manual` 加 `FakeBackend` 与 `FakeSource`，和记录调用次数、在内存里存 blob 的 `FakeGit`（`Engine::with_git` 注入）。`Rig::settle` 跑完 executor 和监听器的队列；`Rig::touch` 注入一个工作区事件并等它交付。要在首屏和完整读取之间改动文件时用 `EngineInner::after_first`（只在测试里）。批量线程上的先后和让路用 `rig.rs` 的 `Threaded`：真实的 executor（批量线程数由测试给）、文件夹项目、保活时段用不前进的 `ManualClock`，`Engine::set_trace` 的每个事件带时刻记下，`wait_for` 等事件；测试里的时间界限只作宽松的检查，判定靠事件的先后。描述符的计数（`open_fds`）在 Linux 上读 `/proc/self/fd`，在 macOS 上读 `/dev/fd`，目录不存在时 panic，不返回 0；计数精确依赖 nextest 每个测试一个进程。
- 合成源码在 `src/testing.rs`（`test-support` feature，`lectern-bench` 也用它）：TypeScript、Rust、JSON（`package-lock.json` 形状）、压缩成一行的 JavaScript；另有 `testing::host`（一个内核和插件句柄）。改生成器会改变 `window_plan_snapshot` 和基准的输入。
- 基准在 `crates/bench/benches/time/syntax.rs`、`memory/syntax.rs`，引擎由 `benches/fixtures/syntax_engine.rs` 搭建；首屏的几项经 `Engine::first_screen`（`test-support`，与 `open` 的第一个任务同一段代码）计时；`syntax/prefetch_wait` 与真实的 `code_intel` 索引器共用一个 executor，经 `Engine::set_trace` 取预取任务开始的时刻；`syntax/edit_tree_bytes` 等 `EditSession::idle`（`test-support`）之后数会话持有的字节，减去缓冲区文本和区间。改首屏、读取或 LRU 时运行 `just bench --measure syntax/`（Linux 上只报告，macOS CI 判定预算）。
- 快照的源码是 `src/tests/fixtures/` 下合成的小文件；改查询、升级 grammar 或 tree-sitter 时，`src/tests/snapshots/` 的变化逐个看过。
- `health_*` 用 `src/tests/health.rs` 的 `ManualClocks` 代替 `platform::ThreadCpuClock`：测试推进每个模拟线程的 CPU 时间和墙上时间，`run_on` 决定下一次 `enter` 在哪个线程上；`health_global_on_real_clocks` 用真实时钟。
- `rg -n 'std::fs::|File::open|mmap' crates/syntax/src --glob '!**/tests*'` 和 `rg -n 'thread::spawn' crates/syntax/src` 没有结果。
