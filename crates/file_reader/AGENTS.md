# file_reader（`lectern-file-reader`）

完整文件阅读（⇧⌘O）与编辑（Edit… / ⌘E），属于收起层。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：`docs/design/file_reader.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#10](https://github.com/SoloJiang/lectern-app/issues/10)
- ADR：[0012](../../docs/adr/0012.md)

## 边界

- 打开文件到首屏高亮 < 16 ms。读文件（mmap）和高亮解析在后台由 `code_intel` 与 `executor` 完成：先解析可见区域，整文件解析随后进行。本视图只读它们给出的快照，不做 IO 和解析（架构规则 4）。
- 只做轻量编辑，编辑为审阅服务（[ADR 0012](../../docs/adr/0012.md)）。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-file-reader       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
