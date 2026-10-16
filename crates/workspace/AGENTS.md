# workspace（`lectern-workspace`）

窗口骨架：侧栏、工具栏、检查器、浮动面板、底部浮条，以及按需呼出与收起的层级切换。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层，不能依赖 `app` 与 tool 层。
- 设计文档：`docs/design/workspace.md`，尚未编写。写代码前先按[模板](../../docs/design/TEMPLATE.md)写好，人审过再动手。
- issue：[#30](https://github.com/SoloJiang/lectern-app/issues/30)
- ADR：[0012](../../docs/adr/0012.md)、[0015](../../docs/adr/0015.md)

## 边界

- 新增界面元素前对照[交互规范「三层结构」](../../docs/design/interaction.md#三层结构)和 [ADR 0012](../../docs/adr/0012.md)：不做活动栏、文件标签页、常驻文件树、状态栏、底部终端停靠。
- 只提供容器与布局，具体内容由各视图 crate 填入。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-workspace       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层给的快照，测试里直接构造快照，不起真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
