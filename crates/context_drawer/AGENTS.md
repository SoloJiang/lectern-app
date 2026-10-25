# context_drawer（`lectern-context-drawer`）

上下文面板：点符号后滑出，显示调用方、相关测试、改动历史（影响面）。属于按需呼出层，Esc 回到原处。

- 层：视图层，插件 crate（实现时 `Cargo.toml` 加 `plugin = true`）。只依赖 kernel 层和非插件的库 crate；不得依赖其他插件 crate、`app` 和 tool 层。
- 设计文档：[docs/design/context_drawer.md](../../docs/design/context_drawer.md)
- issue：[#20](https://github.com/SoloJiang/lectern-app/issues/20)
- ADR：[0012](../../docs/adr/0012.md)

## 边界

- 符号查询由 `code_intel` 提供（经 `dyn CodeIntel`），调用方的状态和 History 来自 `review` 的快照（经 `dyn Review`）；本视图不做索引和查询，不读文件，不调用 `lectern-git`。
- 调用方经 `services::context::ContextEvent::Show` 请求面板，不依赖本 crate。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-context-drawer       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
