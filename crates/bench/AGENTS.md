# bench（`lectern-bench`）

性能预算基准：冷启动、打开项目、打开文件、打开 Diff、切换 worktree、空闲内存，以及每个插件的激活耗时和常驻内存。超标即 CI 失败。

- 层：tool 层，只用于调试与测试构建，可以依赖任何层。只有 `app` 可以依赖本 crate（`bench-probe` feature，不在 `default` 里），本 crate 不进入发布构建。
- 设计文档：[docs/design/bench.md](../../docs/design/bench.md)
- issue：[#7](https://github.com/SoloJiang/lectern-app/issues/7)、[#38](https://github.com/SoloJiang/lectern-app/issues/38)
- ADR：[0017](../../docs/adr/0017.md)、[0018](../../docs/adr/0018.md)、[0048](../../docs/adr/0048.md)、[0051](../../docs/adr/0051.md)、[0053](../../docs/adr/0053.md)

## 边界

- 预算表只有一张（`src/budgets.rs`），每条写明出处。顶层预算以[技术调研「性能预算」](../../docs/design/tech-research.md#性能预算)和 ADR 为准，改动要写 ADR；各 crate 设计文档里的分项预算随那份设计文档修改（[设计文档「预算表」](../../docs/design/bench.md#预算表)）。
- 不进入发布构建。
- 只调用被测 crate 的公开接口和 `test-support` 下的 `testing` 模块，不复制它们的内部逻辑。
- macOS 专属的测量（进程创建时间、`phys_footprint`、首帧时刻）经 `platform`，本 crate 不直接调用系统接口。`benches/process.rs` 结束进程组用 POSIX 的 `kill`（`libc`），它不是 macOS 专属。
- `probe` feature（探针，只在测量构建里）是唯一打开 `lectern-platform/bench-hooks` 的地方（[ADR 0048](../../docs/adr/0048.md)）。探针的 `bench-probe` 插件实现了 `Plugin`，本 crate 仍不是插件 crate（[ADR 0051](../../docs/adr/0051.md)）：它不提供、也不要求服务。
- 进程级基准启动的每个程序都有自己的临时 `HOME` 和 socket（`src/process.rs`），不读写用户的数据。

## 测试

```sh
just test -p lectern-bench   # 运行器、预算表、分片（shards_*、runner_shard_*、report_merge_shards_*）、报告协议（wire_*）、探针（probe_*）、进程调度（process_*）、purge
just bench                   # macOS（Apple 芯片）上判定预算；其他平台是冒烟运行
just bench agents/           # 只运行 ID 含 agents/ 的基准
just bench worktrees/        # macOS：worktrees/build_1000、worktrees/resident_bytes（进程内，不开窗口）；app/switch_worktree 要窗口，用 just bench app/switch_worktree
just bench --shard rest agents/   # macOS：只运行一个分片（code-intel、process、git、rest）里的预算，不合并；CI 的合并任务合并各分片的报告
just bench --measure git/    # Linux 上也按规则采样，只报告、不判定
LECTERN_BENCH_JUDGE=1 just bench baseline/   # 只在判定机器上：首帧要真正显示的条目也判定（ADR 0052）
```

- 判定机器的条目（`Budget::judging_machine`：`app/cold_start`、`app/cold_start_purged`、`baseline/*`）在没有 `LECTERN_BENCH_JUDGE=1` 的测量运行里只记录。本机屏幕锁定时首帧的来源是 `handler`，设了这个变量就是错误，不是判定结果（[bench 设计「平台与噪声」](../../docs/design/bench.md#平台与噪声)）。

- 基准目标：`benches/time.rs`（按被测 crate 分模块，在 `benches/time/` 下）、`benches/memory.rs`（注册 `CountingAlloc`）、`benches/process.rs`（只在 macOS 运行；`baseline/*` 启动 `examples/empty_window.rs`，`app/*` 和 `plugin/*` 启动带 `bench-probe` 的 `lectern`；`just bench` 构建这两个程序并设置 `LECTERN_BENCH_BASELINE`、`LECTERN_BENCH_APP`）。新增基准时在预算表里把对应条目改为 `Active`，否则运行器在开始前以 2 退出。
- 分片表在 `src/shards.rs`（名字、目标、ID 前缀），是数据，按报告里每个基准的耗时调整，不需要新的 ADR（[ADR 0053](../../docs/adr/0053.md)）。每条 `time`、`memory`、`process` 的预算恰好属于一个分片，`process` 目标的全部预算在同一个分片里：新增预算不用改分片表，新增前缀或目标才要改，测试 `shards_*` 会提醒。`--shard` 把别的分片的预算记为 `sharded`；`lectern-bench-report merge` 接受几个目录，每条预算去掉 `sharded` 之后恰好剩一次，合并后的 `report.json` 写进第一个目录。
- 基准按 ID 前缀对应预算，前缀最长的基准产出那条预算（`plugin` 产出 `plugin/*/activate`、`plugin/*/resident` 和 `plugin/<id>/activate`，`plugin/after_first_frame` 产出自己的）。产出 `plugin/<id>/…` 的基准用 `Ctx::report_plugins` 把插件列表交给报告，合并据此展开 `plugin/*`。
- 每个目标写自己的 `target/lectern-bench/report-<target>.json`，`lectern-bench-report merge` 合并成 `report.json` 并决定 `just bench` 的退出码。报告列出每项的实测值、预算和出处；CI 的 macOS 任务判定预算，Linux 任务只做冒烟。
- 进程调度（`src/process.rs`）的测试用脚本化的假 `Launcher` / `ProcessInfo` 和假探针，不启动真实程序（空闲内存、`app/unclassified_children`、`plugin/after_first_frame`、插件的一对启动都在这里，`benches/process.rs` 只把结果变成 `Measured`）；`tests/purge.rs` 把假的 `sudo` 放在这条命令的 `PATH` 最前面。探针（`src/probe.rs`）的测试用真实的 `Kernel` 和测试插件。
- 夹具由被测 crate 的 `testing` 在运行时生成（如 `cloudprint` 形状的合成仓库），不提交大型样例项目；`tests/fixtures/` 只放本 crate 自己的小样本。
