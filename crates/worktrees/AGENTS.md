# worktrees（`lectern-worktrees`）

并行总览：按 Active / Idle 分组列出每个 worktree 的分支、会话和改动量；检查器对比所选 worktree 与 main，并提示与其他 worktree 的文件重叠。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：[docs/design/worktrees.md](../../docs/design/worktrees.md)
- issue：[#16](https://github.com/SoloJiang/lectern-app/issues/16)
- ADR：[0006](../../docs/adr/0006.md)、[0022](../../docs/adr/0022.md)

## 边界

- 切换 worktree / 分支 < 100 ms，依赖以 blob OID 为键的缓存命中。
- 每个 worktree 显示当前分支的累计 token（不显示费用，[ADR 0022](../../docs/adr/0022.md)；P1 标为分支用量，P3 起为所属 MR 的用量；一个分支同时对应几个开着的 MR 时，每个 MR 各列一项自己区间内的用量，并注明与哪些 MR 共用这个分支，不只显示其中一个），数据来自 `usage` 的快照（[ADR 0020](../../docs/adr/0020.md)）。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-worktrees       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
