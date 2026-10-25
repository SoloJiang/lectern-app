# usage_view（`lectern-usage-view`）

Usage 页：额度卡片、每日 token 柱状图；当前项目显示项目合计与按分支、按模型的明细，P2 标为「分支用量」，P3 起 `forge` 就绪后按 MR 显示（未开 MR 的分支一并列出）；「不属于任何 MR」（P2 标为「默认分支与无法归属」）单独一行，各行相加等于项目合计；所有项目显示各项目合计与占比，以及「其他」一行（还没有打开对应项目的用量）；「路径已不存在」的项目行提供「合并到…」与撤销合并；同一路径上身份无法确定而暂列为临时项目的行（根提交变了且没有远端和仓库 ID，或没有仓库 ID、只有主远端地址变了），提供「是同一个项目」（合并到原项目）与「是新项目」两个确认操作，同样可以撤销，视图只发命令，别名由 `usage` 记录；7D / 14D / 30D 切换（[ADR 0020](../../docs/adr/0020.md)）。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：[docs/design/usage_view.md](../../docs/design/usage_view.md)（已接受，尚未实现）。两个插件 `usage-view`（屏幕、侧栏条目、statusline 的接入）与 `usage-view-data`（`dyn Usage`）；statusline 的安装与恢复经 `platform::claude_statusline`，本 crate 只管策略文件和界面。
- issue：[#32](https://github.com/SoloJiang/lectern-app/issues/32)
- ADR：[0014](../../docs/adr/0014.md)、[0020](../../docs/adr/0020.md)、[0022](../../docs/adr/0022.md)

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
