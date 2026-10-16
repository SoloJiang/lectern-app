# ui（`lectern-ui`）

系统色 token、动画原语和通用控件，封装 gpui-component。视图里的颜色、字体、动画都从这里取（[ADR 0015](../../docs/adr/0015.md)）。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：`docs/design/ui.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，人审过再写代码。
- issue：[#6](https://github.com/SoloJiang/lectern-app/issues/6)
- ADR：[0015](../../docs/adr/0015.md)、[0016](../../docs/adr/0016.md)

## 边界

- 只用系统字体、系统语义色和系统控件；浅色与深色由系统外观决定。
- 动画时长与曲线见[交互规范「动效」](../../docs/design/interaction.md#动效)，在这里定义一次，视图不自定义。
- gpui-component 的 API 只在本 crate 和视图层使用；升级要和 GPUI 一起，先确认（[ADR 0016](../../docs/adr/0016.md)）。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-ui       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
