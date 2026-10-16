# usage_view（`lectern-usage-view`）

Usage 页：额度卡片、每日 token 柱状图、按 MR 与模型的明细（未开 MR 的分支一并列出），当前项目 / 所有项目与 7D / 14D / 30D 切换（[ADR 0020](../../docs/adr/0020.md)）。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：`docs/design/usage_view.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#32](https://github.com/SoloJiang/lectern-app/issues/32)
- ADR：[0014](../../docs/adr/0014.md)、[0020](../../docs/adr/0020.md)

## 边界

- 数据来自 `usage` 的聚合快照；本视图不读日志、不查 SQLite。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-usage-view       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
