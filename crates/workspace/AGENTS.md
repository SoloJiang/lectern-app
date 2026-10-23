# workspace（`lectern-workspace`）

窗口骨架：侧栏、工具栏、检查器、浮动面板、底部浮条，以及按需呼出与收起的层级切换。

- 层：视图层，插件 crate（实现插件的那一步加 `plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "workspace"，提供 `dyn Contributions`，依赖 `dyn Projects`。只依赖 kernel 层和非插件的库 crate（`ui`、`i18n`），不依赖贡献方（`project_home`、`review_view`、`file_reader` 等）；只有 `app` 和 tool 层依赖本 crate。
- 设计文档：[docs/design/workspace.md](../../docs/design/workspace.md)
- issue：[#215](https://github.com/SoloJiang/lectern-app/issues/215)
- ADR：[0012](../../docs/adr/0012.md)、[0015](../../docs/adr/0015.md)、[0017](../../docs/adr/0017.md)、[0025](../../docs/adr/0025.md)

## 边界

- 新增界面元素前对照[交互规范「三层结构」](../../docs/design/interaction.md#三层结构)和 [ADR 0012](../../docs/adr/0012.md)：不做活动栏、文件标签页、常驻文件树、状态栏、底部终端停靠。
- 只提供容器与布局，具体内容由各视图 crate 经 `dyn Contributions` 填入（设计文档「接口」）。
- 不做 IO：窗口状态文件由 `app` 读写，本 crate 只给出 `WindowState` 和变化回调；首帧之前没有本 crate 的代码运行（[ADR 0017](../../docs/adr/0017.md)）。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-workspace       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
