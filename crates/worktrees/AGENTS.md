# worktrees（`lectern-worktrees`）

并行总览：按 Active / Idle 分组列出每个 worktree 的分支、会话和改动量；检查器对比所选 worktree 与 main，并提示与其他 worktree 的文件重叠。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层，不能依赖 `app` 与 tool 层。
- 设计文档：`docs/design/worktrees.md`，尚未编写。写代码前先按[模板](../../docs/design/TEMPLATE.md)写好，人审过再动手。
- issue：[#16](https://github.com/SoloJiang/lectern-app/issues/16)
- ADR：[0006](../../docs/adr/0006.md)

## 边界

- 切换 worktree / 分支 < 100 ms，依赖以 blob OID 为键的缓存命中。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-worktrees       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层给的快照，测试里直接构造快照，不起真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
