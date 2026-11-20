# file_reader（`lectern-file-reader`）

完整文件阅读（⇧⌘O）与编辑（Edit… / ⌘E），属于收起层；侧栏的 Files 文件树，属于按需层。

- 层：视图层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "file-reader"，提供 `dyn Reader`，依赖 `dyn Projects`、`dyn CodeIntel`、`dyn Contributions`。只依赖 kernel 层和非插件的库 crate（`ui`、`syntax`、`i18n`、`executor`、`cache`、`fs_watch`），只有 `app` 和 tool 层依赖本 crate；其他视图经 `services::reader` 打开文件。
- 设计文档：[file_reader 设计](../../docs/design/file_reader.md)。高亮、文本与文档的领域部分在新的 `syntax` crate（[ADR 0041](../../docs/adr/0041.md)），设计在同一份文档里。
- issue：[#10](https://github.com/SoloJiang/lectern-app/issues/10)
- ADR：[0012](../../docs/adr/0012.md)、[0033](../../docs/adr/0033.md)、[0034](../../docs/adr/0034.md)、[0041](../../docs/adr/0041.md)

## 边界

- 现在（第 6 步）只有插件、`dyn Reader` 和阅读状态，没有视图：`Session`（请求队列、当前文档、前进后退历史、`code_intel` 的焦点）不依赖 GPUI，第 7 步的阅读视图在主线程上读它，第 8 步的编辑模式和离开守卫接在它上面。`app` 在第 7 步才装配本插件（与 `code_intel`、`review-view-handoff` 一起）。`Session` 通过 `Host` 取项目快照、设置焦点和发事件，不直接碰 kernel 的服务，测试用替身。
- `Session` 的方法分两类：任何线程可调用的（`Reader` 的三个方法、`close_project`），只入队或转发，立即返回；主线程上由视图调用的（`next_request`、`apply`、`cancel`、`back`、`forward`、`sync`……）。不持锁调用外部：事件、焦点和放掉文档都在锁之外。
- 打开文件到首屏高亮 < 16 ms。读文件（经 `platform` 的 `SafeRoot`，不用 mmap；首屏先读要显示的那一段，整个文件随后读；完整内容装入之前只读，编辑入口不可用）和高亮解析在后台由 `syntax` 与 `executor` 完成：首屏只解析目标附近的窗口，整个文件按档位在后台计算，语法树不常驻；中部的目标按字节偏移或行检查点定位（[ADR 0034](../../docs/adr/0034.md)）。本视图只读它们给出的快照，不做 IO 和解析（架构规则 4），也不依赖 `tree-sitter`。
- 阅读视图是自绘的固定行高列表，只排版可见的显示行；编辑模式用 gpui-component 的 `InputState`，只对 1 MiB 以内、没有超过 4 KiB 的行的文件开放（ADR 0034），不开 gpui-component 的 `tree-sitter` feature，高亮经本 crate 的 `InputHighlighter` 适配器读 `syntax` 的区间。
- 只做轻量编辑，编辑为审阅服务（[ADR 0012](../../docs/adr/0012.md)）；保存不运行任何程序（设计「未决问题」1）。

## 测试

第 6 步的部分（插件、`Session`、历史、焦点的判定）不依赖 GPUI，Linux CI 覆盖；第 7 步起加入 GPUI，视图的测试只在 macOS 上编译：

```sh
just test -p lectern-file-reader
```

- 前缀：`plugin_*`（清单、激活不提交任务、`ProjectEvent` 的监听，用 kernel 的 `TestKernel` 和三个服务的替身）、`session_*`（队列、票号、`apply` / `cancel`、历史、预取与 `share_object` 的转发、焦点；用真实的 `syntax::Engine`、手动 executor 和临时目录里的合成文件）、`history_*`、`focus_*`（只读快照的纯函数）、`logs_*`（`lectern-logging` 的 `testing::capture()` 和金丝雀）。
- 测试不用真实的 git、会话扫描和网络；git 项目的 `.git` 目录不存在，只用来给 `ProjectKey` 和快照命名；对象库内容经 `share_object` 登记，没有登记的读不到（「找不到」）。
- 视图（第 7 步起）：交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化，只读领域层提供的快照：测试里直接构造 `DocSnapshot` 和 `DirTable`，不使用真实的 git、会话扫描或文件 IO；领域部分的测试在 `syntax` 里。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`（`file-reader.ftl`），en 与 zh-CN 同时提供（第 7 步起）。
