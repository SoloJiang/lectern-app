# code_intel（`lectern-code-intel`）

tree-sitter 符号索引（tags → SQLite，增量更新）、Java / TS 解析、跨子项目导航、依赖源码跳转。

- 层：领域层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "code-intel"（`CodeIntelPlugin`），提供 `dyn CodeIntel`、依赖 `dyn Projects`。只依赖 kernel 层（`kernel`、`services`）和非插件的库 crate（`executor`、`cache`、`git`、`fs_watch`、`platform`）；`project` 是插件，只经 `services::project` 往来。只有 `app` 和 tool 层依赖本 crate，消费方（抽屉、⌘K）只依赖 `services::code_intel`。不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：[code_intel 设计](../../docs/design/code_intel.md)（语法索引，#17）。
- issue：[#17](https://github.com/SoloJiang/lectern-app/issues/17)、[#18](https://github.com/SoloJiang/lectern-app/issues/18)、[#19](https://github.com/SoloJiang/lectern-app/issues/19)、[#26](https://github.com/SoloJiang/lectern-app/issues/26)
- ADR：[0007](../../docs/adr/0007.md)、[0018](../../docs/adr/0018.md)、[0025](../../docs/adr/0025.md)、[0029](../../docs/adr/0029.md)、[0030](../../docs/adr/0030.md)、[0031](../../docs/adr/0031.md)

## 边界

- 核心只带常用语言的 grammar；其他语言包和 jdtls / SCIP 是插件市场里的可选插件（[ADR 0018](../../docs/adr/0018.md)）。
- 外部语言服务器按需启动、空闲即关（架构规则 3）。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-code-intel     # 只改本 crate 时最快的反馈
just test -p lectern-code-intel
```

- 测试在 `src/tests/`，按前缀分文件（`lang_*`、`extract_*`、`deadline_*`、`site_*`、`forms_*`、`scopes_*`、`store_*`、`map_*`、`overlay_*`、`derived_*`、`schedule_*`、`grammar_*`（`grammar_changes.rs`）、`battery_*`、`incremental_*`、`warm_*`，第 4 步的 `goto_*`、`read_verify_*`、`search_*`、`refs_*`、`names_*`、`escalate_*`、`head_dirty_*`、`prefetch_*`、`focus_*`、`plugin_*`、`watchdog_*`）；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 建索引（`src/index/`）的测试用 `src/tests/support.rs` 的 `Harness`：`testing::host()` 的 kernel 给出 `PluginHandle`，`Executor::manual()`，`testing::FakeGit`（git 元数据在内存里，worktree 文件是临时目录里的真实文件，记录每次调用），`testing::FakeProjects`、`FakePower`、`FakeGrammars`，fs_watch 的 `FakeBackend` 与手动监听器，临时目录里的 cache，`ManualClock` 上的看门狗。抽取经 `Indexer::set_extractor` 换成计数和可以插入动作的钩子；`Stats` 的计数（基础映射重建、工作区比较、重新读、游标重建、抽取、存在检查）是 `derived_*` 的计数钩子。要在一次抽取进行中改变状态的测试，在钩子里动作，或者用 `std::thread::scope` 在另一个线程上跑 executor。生产的 git 适配（`LecternGit`）由 `overlay_lectern_git_answers_like_the_fake` 用 `lectern_git::testing::TestRepo` 检查；`FakeGit` 的比较结果与它不一致的地方（如目录条目）会掩盖核对任务的问题，新增一种工作区情况时在 `overlay_lectern_git_and_the_fake_list_directories_alike` 里两边各构造一次、比较结果。
- 工作区文件只经 `SafeRoot` 读取：`rg -n 'std::fs::|File::open' crates/code_intel/src --glob '!**/tests*' --glob '!**/testing*'` 没有结果（设计「验收」第 3 步）。
- tags 和 locals 查询在 `queries/`，文件头写明上游版本、commit 和 Lectern 的改动；改查询、升级 grammar 或 tree-sitter 时，抽取快照（`src/tests/snapshots/`）的变化逐个看过，版本常量由 `lang_grammar_versions_match_lock` 对照 Cargo.lock。
- 引入名字的语法形式都在 `src/forms.rs` 的表里（设计「定义形式」），语法种类表由它推出：新增或改动一种形式时先改这张表，在 `src/tests/forms.rs` 的 `SAMPLES` 里加样例，再改查询和遍历；升级 grammar 时 `forms_cover_grammar` 列出没有归类的新节点种类。设计文档里的表由 `forms_table_matches_design` 对照，失败时按它打印的内容替换。
- 可能开作用域的节点种类都在 `src/tests/scopes.rs` 的 `SCOPES` 表里（设计「作用域形式」）：改 locals 查询的作用域时先改这张表和它的 `SAMPLES`，再改查询和遍历（`Ctx::scope` 是局部作用域，`Ctx::member_scope` 是类体和命名空间体，决定局部定义的可见性）；`scopes_cover_grammar` 列出没有归类的种类，`scopes_match_locals_query` 检查查询开出的作用域与表一致，`scopes_table_matches_design` 对照设计文档里的表。
- 核心语言的抽取是 `src/walk/` 下按查询写的一次遍历（ADR 0031）：改 tags 或 locals 查询时同时改对应的 `walk/<语言>.rs`，在 `src/tests/extract.rs` 的 `WALKER_CASES` 里加覆盖新模式的样例；`extract_walker_matches_tags` 检查遍历与 tree-sitter-tags 的结果相同。
- 首发语言只在 `src/lang.rs` 的 `CORE_LANGUAGES` 和两个 `Cargo.toml` 的 grammar 依赖里列出。
- 抽取夹具放 `tests/fixtures/extract/<语言>/`，只用合成的源码；格式随上游版本变化的，每个版本留一份样本。
- 存储（`src/store.rs`）只经 `lectern-cache` 的自定义表接口执行 SQL，本 crate 不依赖 rusqlite；`store_*` 用 `tempfile` 的临时目录和 `lectern-cache` 的 `test-support`（`ManualClock`、`set_max_page_count`），检查行数等测试专用的语句放在测试自己的登记里。
- 看门狗的测试用 `testing::watchdog`（`Executor` 的专用线程）；需要线程的测试用 `std::thread::scope`，`src` 里不出现 `thread::spawn`（设计「验收」第 3 步）。
- 查询（`src/query/`）的测试用 `Harness` 的 `goto`、`ask`（提交查询、跑完 executor、取结果）；要文件保持未入索引时先 `pause()` kernel 的后台工作（第 3–5 层停在批次边界）。解析扩展点用测试自己的 `Resolver`（`Indexer::set_resolver`），语义导航用测试里的假提供者（回复由测试发出）。`Harness` 的事件记在 `Host::events` 里（记录持有名字表的 `Arc`：测「没人持有」时先 `clear_events`）；`testing::host()` 的 kernel 不记录事件。
- 插件（`plugin_*`、`watchdog_*`）用 kernel 的 `TestKernel` 加模拟的 `dyn Projects`（`tests/plugin.rs` 的 `Run`）；`watchdog_*` 看线程自己的闭包更新的 `plugin::WatchdogThread`；`/proc/self/task/*/comm` 只在 Linux 上、线程已退出之后再确认（依赖 nextest 每个测试一个进程）。测试不从经过的时间推断状态：等一个由被测代码更新的信号（看门狗的 `waits()`、到期表的 `scheduled()`、探针的调用次数），再断言。
- 基准的虚拟仓库：`testing::virtual_repo`（`FakeGit::stage_oids` 只有 index 条目、没有文件，`assume_unchanged` 让工作区比较为空，单元是 `synthetic::extraction`），只有查询要读的文件写在磁盘上。
