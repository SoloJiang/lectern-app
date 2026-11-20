# file_reader（`lectern-file-reader`）

完整文件阅读（⇧⌘O）与编辑（Edit… / ⌘E），属于收起层；侧栏的 Files 文件树，属于按需层。

- 层：视图层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "file-reader"，提供 `dyn Reader`，依赖 `dyn Projects`、`dyn CodeIntel`、`dyn Contributions`。只依赖 kernel 层和非插件的库 crate（`ui`、`syntax`、`i18n`、`executor`、`cache`、`fs_watch`），只有 `app` 和 tool 层依赖本 crate；其他视图经 `services::reader` 打开文件。
- 设计文档：[file_reader 设计](../../docs/design/file_reader.md)。高亮、文本与文档的领域部分在新的 `syntax` crate（[ADR 0041](../../docs/adr/0041.md)），设计在同一份文档里。
- issue：[#10](https://github.com/SoloJiang/lectern-app/issues/10)
- ADR：[0012](../../docs/adr/0012.md)、[0033](../../docs/adr/0033.md)、[0034](../../docs/adr/0034.md)、[0041](../../docs/adr/0041.md)

## 边界

- 现在（第 7 步的 7a）有插件、`dyn Reader`、阅读状态和阅读视图，没有编辑模式和 Files：`Session`（请求队列、当前文档、前进后退历史、`code_intel` 的焦点，不依赖 GPUI）和 `ReadingView`（`view.rs` 是状态与行为，`render.rs` 是绘制，`screen.rs` 是向 `workspace` 登记的屏幕、工具栏和布局，`shared.rs` 是插件与视图共用的会话和服务，`actions.rs` 是 Action 与快捷键，`text.rs` 是一行文本的制表符展开、着色区间和副标题的纯函数）。第 8 步的编辑模式和离开守卫接在 `ReadingView` 与 `Session` 上。`app` 装配本插件（与 `code_intel`、`review-view-handoff` 一起）。`Session` 通过 `Host` 取项目快照、设置焦点和发事件，不直接碰 kernel 的服务，测试用替身。
- `Session` 的方法分两类：任何线程可调用的（`Reader` 的三个方法、`close_project`），只入队或转发，立即返回；主线程上由视图调用的（`next_request`、`apply`、`cancel`、`back`、`forward`、`sync`……）。不持锁调用外部：事件、焦点和放掉文档都在锁之外。
- 打开文件到首屏高亮 < 16 ms。读文件（经 `platform` 的 `SafeRoot`，不用 mmap；首屏先读要显示的那一段，整个文件随后读；完整内容装入之前只读，编辑入口不可用）和高亮解析在后台由 `syntax` 与 `executor` 完成：首屏只解析目标附近的窗口，整个文件按档位在后台计算，语法树不常驻；中部的目标按字节偏移或行检查点定位（[ADR 0034](../../docs/adr/0034.md)）。本视图只读它们给出的快照，不做 IO 和解析（架构规则 4），也不依赖 `tree-sitter`。
- 阅读视图是自绘的固定行高列表，只排版可见的显示行；编辑模式用 gpui-component 的 `InputState`，只对 1 MiB 以内、没有超过 4 KiB 的行的文件开放（ADR 0034），不开 gpui-component 的 `tree-sitter` feature，高亮经本 crate 的 `InputHighlighter` 适配器读 `syntax` 的区间。
- 只做轻量编辑，编辑为审阅服务（[ADR 0012](../../docs/adr/0012.md)）；保存不运行任何程序（设计「未决问题」1）。

## 测试

本 crate 依赖 GPUI（第 7 步起），Linux 的 `just` 命令按依赖图排除它，测试以 macOS CI 为准；视图的测试用 `#[gpui::test]` 的 `TestAppContext`，不开真实窗口：

```sh
just test -p lectern-file-reader
```

- 前缀：`view_*`（阅读视图：配合 `Rig` 的真实 `Engine`、`FileReaderPlugin` 在 `TestKernel` 里、`FakeContributions` 记录登记的屏幕和快捷键、派发的命令和报告的里程碑；按键用 `simulate_keystrokes`，语义树用 `Recorder`）、`text_*`（`text.rs` 的纯函数）、`plugin_*`（清单、激活不提交任务、`ProjectEvent` 的监听，用 kernel 的 `TestKernel` 和三个服务的替身）、`session_*`（队列、票号、`apply` / `cancel`、历史、预取与 `share_object` 的转发、焦点；用真实的 `syntax::Engine`、手动 executor 和临时目录里的合成文件）、`history_*`、`focus_*`（只读快照的纯函数）、`logs_*`（`lectern-logging` 的 `testing::capture()` 和金丝雀）。
- 测试不用真实的 git、会话扫描和网络；git 项目的 `.git` 目录不存在，只用来给 `ProjectKey` 和快照命名；对象库内容经 `share_object` 登记，没有登记的读不到（「找不到」）。
- 视图：交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、滚轮、Action 和状态变化，只读领域层提供的快照。`DocSnapshot` 的字段是 `syntax` 私有的，测试不能直接构造它，所以用第 6 步的 `Rig`：真实的 `syntax::Engine`、手动 executor 和临时目录里由夹具写入的合成文件（视图本身不做 IO，`rg -n 'std::fs|SafeRoot|read_ranges' crates/file_reader/src --glob '!**/tests*'` 没有结果）；Files 的 `DirTable` 可以直接构造。领域部分的测试在 `syntax` 里。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`（`file-reader.ftl`），en 与 zh-CN 同时提供。
