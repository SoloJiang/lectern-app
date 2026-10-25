# conversation（`lectern-conversation`）

检查器的 Conversation 页（⌘\）：与 agent 的对话、审阅意见回传后的新一轮、权限请求，以及 Terminal 页的入口。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：[docs/design/conversation.md](../../docs/design/conversation.md)（已接受，尚未实现）。实现按它的「验收」分四步；ACP agent 的启动方式与信任边界见 ADR 0050。
- issue：[#23](https://github.com/SoloJiang/lectern-app/issues/23)、[#24](https://github.com/SoloJiang/lectern-app/issues/24)、[#12](https://github.com/SoloJiang/lectern-app/issues/12)
- ADR：[0012](../../docs/adr/0012.md)、[0050](../../docs/adr/0050.md)

## 边界

- 会话内容来自 `agents`，终端网格由 `terminal_view` 绘制。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-conversation       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
