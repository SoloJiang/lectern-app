# review_view（`lectern-review-view`）

审阅视图：会话简报、按轮次的改动叙事、文件已看进度、Drifted 提示、行内审阅意见与底部发送栏。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：`docs/design/review_view.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#15](https://github.com/SoloJiang/lectern-app/issues/15)、[#22](https://github.com/SoloJiang/lectern-app/issues/22)
- ADR：[0012](../../docs/adr/0012.md)

## 边界

- 归因、片段对齐、已看状态都在 `review` 里计算；本视图只读快照、发命令。
- 打开一个会话的 Diff < 50 ms；滚动稳定 120 fps，长 diff 用虚拟滚动。
- 会话简报显示所属 MR 的累计 token 和本会话的占比，每轮标题显示该轮 token（[ADR 0020](../../docs/adr/0020.md)）。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-review-view       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
