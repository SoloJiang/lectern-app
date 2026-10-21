# file_reader（`lectern-file-reader`）

完整文件阅读（⇧⌘O）与编辑（Edit… / ⌘E），属于收起层；侧栏的 Files 文件树，属于按需层。

- 层：视图层，插件 crate（实现插件的那一步加 `plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "file-reader"，提供 `dyn Reader`，依赖 `dyn Projects`、`dyn CodeIntel`、`dyn Contributions`。只依赖 kernel 层和非插件的库 crate（`ui`、`syntax`、`i18n`、`executor`、`cache`、`fs_watch`），只有 `app` 和 tool 层依赖本 crate；其他视图经 `services::reader` 打开文件。
- 设计文档：[file_reader 设计](../../docs/design/file_reader.md)。高亮、文本与文档的领域部分在新的 `syntax` crate（[ADR 0041](../../docs/adr/0041.md)），设计在同一份文档里。
- issue：[#10](https://github.com/SoloJiang/lectern-app/issues/10)
- ADR：[0012](../../docs/adr/0012.md)、[0033](../../docs/adr/0033.md)、[0034](../../docs/adr/0034.md)、[0041](../../docs/adr/0041.md)

## 边界

- 打开文件到首屏高亮 < 16 ms。读文件（经 `platform` 的 `SafeRoot`，不用 mmap；首屏先读要显示的那一段，整个文件随后读；完整内容装入之前只读，编辑入口不可用）和高亮解析在后台由 `syntax` 与 `executor` 完成：首屏只解析目标附近的窗口，整个文件按档位在后台计算，语法树不常驻；中部的目标按字节偏移或行检查点定位（[ADR 0034](../../docs/adr/0034.md)）。本视图只读它们给出的快照，不做 IO 和解析（架构规则 4），也不依赖 `tree-sitter`。
- 阅读视图是自绘的固定行高列表，只排版可见的显示行；编辑模式用 gpui-component 的 `InputState`，只对 1 MiB 以内、没有超过 4 KiB 的行的文件开放（ADR 0034），不开 gpui-component 的 `tree-sitter` feature，高亮经本 crate 的 `InputHighlighter` 适配器读 `syntax` 的区间。
- 只做轻量编辑，编辑为审阅服务（[ADR 0012](../../docs/adr/0012.md)）；保存不运行任何程序（设计「未决问题」1）。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-file-reader       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造 `DocSnapshot` 和 `DirTable`，不使用真实的 git、会话扫描或文件 IO；领域部分的测试在 `syntax` 里，Linux CI 覆盖。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`（`file-reader.ftl`），en 与 zh-CN 同时提供。
