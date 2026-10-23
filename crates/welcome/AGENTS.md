# welcome（`lectern-welcome`）

打开项目窗口：最近项目、Open Folder / Clone、语言选择，以及从 Claude Code 与 Codex 历史里发现、尚未打开过的文件夹。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：[docs/design/welcome.md](../../docs/design/welcome.md)（已接受，尚未实现）。实现以它为准。
- issue：[#29](https://github.com/SoloJiang/lectern-app/issues/29)
- ADR：[0012](../../docs/adr/0012.md)、[0013](../../docs/adr/0013.md)

## 边界

- 会话历史的扫描在 `agents` 里、首帧之后进行，本视图只读结果快照。
- 打开项目经 `workspace` 的 `WorkspaceCommand`（`OpenProject`、`PromptOpenProject`），本 crate 不调 `Projects::open`。
- 最近项目列表存在 `platform::dirs::app_data()` 下的 `welcome/recent.json`，读写只在 executor 的 `Background` 任务里（`src/recent/store.rs`）。
- 本 crate 是插件 crate（插件 `welcome`，[ADR 0025](../../docs/adr/0025.md)）：只依赖 kernel 层和非插件的库 crate。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-welcome       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
