# bench（`lectern-bench`）

性能预算基准：冷启动、打开项目、打开文件、打开 Diff、切换 worktree、空闲内存，以及每个插件的激活耗时和常驻内存。超标即 CI 失败。

- 层：tool 层，只用于调试与测试构建，可以依赖任何层。只有 `app` 可以依赖本 crate（`bench-probe` feature，不在 `default` 里），本 crate 不进入发布构建。
- 设计文档：[docs/design/bench.md](../../docs/design/bench.md)
- issue：[#7](https://github.com/SoloJiang/lectern-app/issues/7)、[#38](https://github.com/SoloJiang/lectern-app/issues/38)
- ADR：[0017](../../docs/adr/0017.md)、[0018](../../docs/adr/0018.md)

## 边界

- 预算表只有一张（`src/budgets.rs`），每条写明出处。顶层预算以[技术调研「性能预算」](../../docs/design/tech-research.md#性能预算)和 ADR 为准，改动要写 ADR；各 crate 设计文档里的分项预算随那份设计文档修改（[设计文档「预算表」](../../docs/design/bench.md#预算表)）。
- 不进入发布构建。
- 只调用被测 crate 的公开接口和 `test-support` 下的 `testing` 模块，不复制它们的内部逻辑。
- macOS 专属的测量（进程创建时间、`phys_footprint`、首帧时刻）经 `platform`，本 crate 不直接调用系统接口。

## 测试

```sh
just test -p lectern-bench   # 运行器、预算表、报告协议、进程调度的单元测试
just bench                   # macOS（Apple 芯片）上判定预算；其他平台是冒烟运行
just bench agents/           # 只运行 ID 含 agents/ 的基准
```

- 基准报告列出每项的实测值、预算和出处，写入 `target/lectern-bench/report.json`；CI 的 macOS 任务判定预算，Linux 任务只做冒烟。
- 夹具由被测 crate 的 `testing` 在运行时生成（如 `cloudprint` 形状的合成仓库），不提交大型样例项目；`tests/fixtures/` 只放本 crate 自己的小样本。
