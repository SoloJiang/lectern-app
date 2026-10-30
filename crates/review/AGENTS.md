# review（`lectern-review`）

审阅的领域逻辑：会话改动归因、片段对齐、已看状态、Drifted 判断、审阅意见攒批与回传。

- 层：领域层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "review"（`plugin`），提供 `dyn Review`，requires `dyn Projects`、`dyn SessionSource`。只依赖 kernel 层（`kernel`、`services`、`logging`）和非插件的库 crate（`executor`、`fs_watch`、`cache`、`git`、`platform`，对 `platform` 只用 `time::utc_offset` 和审阅意见剪贴板途径的 `clipboard::write_text`）；`project`、`agents`、`usage` 是插件，只经 `services` 的接口往来，测试用本 crate `testing` 里的假服务。不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：[docs/design/review.md](../../docs/design/review.md)。
- issue：[#15](https://github.com/SoloJiang/lectern-app/issues/15)、[#22](https://github.com/SoloJiang/lectern-app/issues/22)
- ADR：[0012](../../docs/adr/0012.md)、[0019](../../docs/adr/0019.md)、[0035](../../docs/adr/0035.md)、[0044](../../docs/adr/0044.md)、[0045](../../docs/adr/0045.md)、[0046](../../docs/adr/0046.md)

## 边界

- 归因结果的缓存键是文件内容的 blob OID 加上会话轮次历史的摘要（架构规则 5）：两个会话的最终内容相同时，改动叙事仍然不同，不能只按 OID 命中。
- 已看状态和审阅意见可以从 git 对象、会话 JSONL 和用户标注日志重建。
- 审阅意见（#22，设计文档「审阅意见」）：锚点是（文件，一侧，行号，这一侧的 OID），位置只按 OID 判断，不猜；prompt 由本 crate 按固定模板拼出，经发送途径送出：剪贴板途径内置（只经 `platform::clipboard`），终端粘贴与 ACP 的途径由其他插件经 `register_note_route` 登记，本 crate 不依赖 `terminal`、`conversation`。意见的文字、摘录和拼好的 prompt 不进日志。
- 项目主页「会话外的改动」的计数比较用 `retained_bytes = 0`、`total_patch_bytes = 0` 的 `Limits`，改动路径超过 4000 个时只保留计数、不保留改动集（[ADR 0035](../../docs/adr/0035.md)）。
- 已看标记的键包含审阅对象（会话或比较范围）、路径、改动前后两个 blob OID、改动前后的文件模式和改动状态；重命名按（旧路径，新路径）成对匹配，旧路径之后被一个无关的新文件占用时，不当作同一个改动。同一审阅对象和路径的这些值任一变化时，标为「审过之后又被改过」，作为第三种状态交给视图。设计依据见 [pulls.review 调研](../../docs/research/pulls-review.md#对-lectern-的建议)。
- 生成文件、lockfile、快照用同一份模式列表识别，同时决定默认折叠、规则分组和模型输入里的占位。
- 标注日志（[ADR 0044](../../docs/adr/0044.md)）只写 `data_dir` 下本 crate 自己的文件，用 `std::fs` 直接读写，不经 `platform`；除此以外不访问文件系统，工作区只经 `git` 读取（`std::fs` 只出现在 `src/log/` 和测试支持里）。审阅引擎（`src/engine/`）的 `open`、`snapshot`、`command`、`rows`、`text`、`close` 立即返回，不在调用线程上做 IO：git、缓存、会话日志和标注日志都在 executor 任务里读写，持锁期间不做 IO。任务经 `Engine::submit` 提交：插件里经 `PluginHandle::own_with` 并持有 `BusyGuard` 到结果发布（kernel 静止），最后一次 `close` 之后保留 30 秒（kernel 的 `Clock` 计时，期间不持有守卫）再释放；每个审阅的 fs_watch 订阅在 `Open` 里，随它释放；fs_watch 的 sink 在它的线程上只取状态锁、请求一轮，不做 IO。压缩只在内存索引来自对这个文件的一次完整读取加本进程之后的追加、且文件的设备号、inode 和长度没有变时进行，否则只追加；文件要超过 4 MiB、且超过本进程上一次压缩后长度的两倍（[ADR 0045](../../docs/adr/0045.md)）。
- 日志经 `lectern-logging` 的宏（[ADR 0046](../../docs/adr/0046.md)），不启用 `sink`；字段值的写法集中在 `src/diag.rs`，错误只记种类，路径只经 `Value::path`。引擎的每个任务经 `Engine::submit` 带审阅的作用域（`review` 是审阅对象的哈希，`session` 是会话 ID 的 UUID 或 128 位哈希）。文件内容、diff 的行、会话内容（prompt、说明、改动步正文、命令文本）、提交说明、标注日志的行和错误消息不进日志（设计文档「日志与诊断」）。
- 会话简报默认由规则生成；模型分析只在用户执行 Analyze 时进行（[ADR 0019](../../docs/adr/0019.md)）。模型输出经过结构校验、覆盖检查、一次纠错重试和对账后才交给视图；任何文件都不能因为模型漏掉而从界面上消失，没被分到的文件进入「未分类」。行备注只挂在 diff 里显示的行上，否则降为文件备注。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-review     # 只改本 crate 时最快的反馈
just test -p lectern-review
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 标注日志的测试（`src/log/tests.rs`）用 `tempfile` 的临时目录和合成记录。IO 故障经 `LogFile::set_faults` 注入（读取、追加、写压缩的临时文件写到一半、改名之前、改名之后的目录 `fsync`），尺寸上限经 `LogFile::set_limits` 调小；`log_compacts_above_4_mib` 用真实的 4 MiB。日志的行格式有 insta 快照（`log_format`），改格式要同时改设计文档「标注日志」。
- 改动链的测试（`src/chain/tests.rs`）用合成的基准、工作区内容和改动步，不经会话来源；链、逐行来源和归因渲染成文本做 insta 快照，每个快照的链同时检查 `review.attribution` 的编码往返。缓存命中不 `load` 由 `history_digest_cache_hit_does_not_load` 在临时目录的 `cache` 上检查。
- 插件的测试（`src/plugin/tests.rs`，`plugin_*`）用 kernel 的 `TestKernel`（`Projects`、`SessionSource` 是 `FakeProjects`、`FakeSessions` 的 mock）、fs_watch 的 `testing::manual` 与 `FakeBackend`、同一个 `Executor::manual()`；`Driver::run_until_idle` 同时处理 fs_watch 的队列和 executor，要让合并窗口结束时先 `run_until_idle` 再 `advance`。30 秒的保留用 kernel 的 `ManualClock`（`ReviewPlugin::with_clock`），引擎的时刻用 `with_wall_clock`，不用墙上时钟。观察日志的测试给自己一个 UUID 会话（`cargo test` 共用捕获器）。批次到请求的映射与请求的合并是 `src/engine/watch.rs` 里的单元测试。
- 概览的测试（`src/engine/tests/overview.rs`，`overview_*`）用 `Driven::watch_overview`、`overview_refresh`、`FakeProjects::set_active`；合并窗口的测试用 `Driven::with_timers`。概览的一轮不调用 `turns`（`overview_never_calls_turns`）。按子项目的今天改动数（`Overview::packages`）的测试是 `overview_packages_*`：子项目目录经 `FakeProjects::set_packages`，本地时区经 `Driven::set_utc_offset`（插件测试里是 `ReviewPlugin::with_utc_offset`），文件的修改时间经 `TestRepo::set_mtime`，不依赖机器的时区。
- 简报的测试：规则是纯函数（`src/brief/tests.rs`，手工构造的 `FileChange`）；引擎侧的 `brief_*`（`src/engine/tests/brief.rs`）用 `Driven::with_timers`（手动 `Host`，`advance` 推进 10 秒的提供者超时）和 `testing::host()`（只交出 `PluginHandle` 的 kernel，给假提供者一个活的 `ActivationToken`；`deactivate` 让它失效）。
- 审阅引擎的测试（`src/engine/tests.rs`：`base_*`、`claims_*`、`files_*`、`narrative_*`、`rows_*`）不经插件，用 `testing::Driven`（`Executor::manual()`，任务只在 `Driven::run` 里执行）驱动；仓库是 `TestRepo`，HEAD reflog 用 `testing::write_reflog` 写，会话来自 `testing::FakeSessions` 的脚本（格式见 `src/testing/fake.rs`），项目是 `testing::FakeProjects`。`FakeSessions::calls` 记下每个会话的 `turns`、`turns_unretained`、`load` 次数。快照渲染成文本做 insta 快照。
- 日志的测试（`src/engine/tests/logs.rs`，`logs_*`）用 `lectern-logging` 的 `testing::capture()` 在 trace 级别检查记录；金丝雀放在文件内容、提交说明、会话内容和标注日志的行里，`assert_absent` 确认它们不进日志。标注日志的故障和上限经 `Engine::set_log_test`（`cfg(test)`）注入。`cargo test` 在同一进程里共用捕获器，按各自的会话 ID 或临时目录筛选记录。
- 性能：`lectern-bench` 的 `review/refresh_one`（`Driven::refresh`、`Driven::sessions_changed`、`SessionFixture::agent_step`）和进程级的 `plugin/review/activate`（`process` 目标），以及 `review/log_load_16k`、`review/chain_3000_lines`、`review/open_session_50`、`review/open_session_50_warm`、`review/open_session_settled`、`review/cutoffs_cold`、`review/attribute_session_50`、`review/rows_first`、`review/mark_seen_5000`、`review/brief_5000`、`review/overview_round_warm`、`review/overview_cloudprint`（`just bench --measure review/`），以及 `memory` 目标的 `review/resident_cloudprint_bytes`、`review/resident_100k_bytes`，夹具是 `testing::write_log`、`testing::chain_fixture`、`testing::SessionFixture`、`testing::MarkSeenFixture`。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
