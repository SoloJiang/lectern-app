# review_view（`lectern-review-view`）

审阅视图：会话简报、按轮次的改动叙事、文件已看进度、Drifted 提示、行内审阅意见与底部发送栏。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：`docs/design/review_view.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#15](https://github.com/SoloJiang/lectern-app/issues/15)、[#22](https://github.com/SoloJiang/lectern-app/issues/22)
- ADR：[0012](../../docs/adr/0012.md)、[0019](../../docs/adr/0019.md)

## 边界

- 归因、片段对齐、已看状态都在 `review` 里计算；本视图只读快照、发命令。
- 简报上的 Analyze（[ADR 0019](../../docs/adr/0019.md)）：执行时发命令给领域层，显示进行中、可取消；离开审阅页或关闭项目时发出取消命令；结果、失败原因和「不可用」的原因都从领域层的快照读取；领域层判定结果的指纹或会话修订号（被分析会话的轮次数与最后一条记录的哈希）与当前不一致时，显示为「基于较早的版本」并提供重新分析。
- 打开一个会话的 Diff < 50 ms；滚动稳定 120 fps，长 diff 用虚拟滚动。
- 会话简报为本会话改到的每个分支各显示一行，P3 起每个 MR 一行（一个分支同时对应几个 MR 时，如分别合向两个目标分支，每个 MR 各占一行，显示自己区间内的累计 token，并注明与哪些 MR 共用这个分支，不只显示其中一个）：累计 token 和本会话的占比，当前审阅的改动所在的分支或 MR 排第一；每轮标题显示该轮 token（[ADR 0020](../../docs/adr/0020.md)）。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-review-view       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
