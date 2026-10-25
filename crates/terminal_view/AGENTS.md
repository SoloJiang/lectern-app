# terminal_view（`lectern-terminal-view`）

终端网格绘制（Conversation 的 Terminal 页）。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：[docs/design/terminal_view.md](../../docs/design/terminal_view.md)（已接受，尚未实现）。本 crate 是非插件的视图库，由 `conversation` 的 Terminal 页使用。实现按它的「验收」分三步：`ui` 终端配色与 `i18n` 的键、`TerminalPane`、装配与真机验证。
- issue：[#5](https://github.com/SoloJiang/lectern-app/issues/5)、[#12](https://github.com/SoloJiang/lectern-app/issues/12)
- ADR：[0002](../../docs/adr/0002.md)、[0012](../../docs/adr/0012.md)

## 边界

- 终端状态在 `terminal` 的独占线程里；本视图只读网格快照，按行缓存字形。
- 属于收起层，不做底部终端停靠（[ADR 0012](../../docs/adr/0012.md)）。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-terminal-view       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
