# conversation（`lectern-conversation`）

检查器的 Conversation 页（⌘\）：与 agent 的对话、审阅意见回传后的新一轮、权限请求，以及 Terminal 页的入口。

- 层：视图层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "conversation"（依赖 `dyn Contributions`、`dyn SessionSource`）和 "conversation-live"（依赖 `dyn Conversations`），都是 `AfterFirstFrame`；第三个 "conversation-terminal" 随第 3 步。只依赖 kernel 层和非插件的库 crate（`ui`、`i18n`、`logging`），不依赖 `agents`、`terminal`、`workspace`、`review_view`、`project_home` 这些插件 crate（dev-dependencies 也不行，架构检查第 6 条）；只有 `app` 依赖本 crate。
- 设计文档：[docs/design/conversation.md](../../docs/design/conversation.md)（已接受）。实现按它的「验收」分四步；ACP agent 的启动方式与信任边界见 ADR 0050。
- 进度（设计文档「验收」）：第 1 步已实现，不开真实窗口：两个插件、检查器页（页内分段控件，Terminal 页是占位）、Messages 页的显示（状态行、列表、计划、链路失败的提示、更早的轮次）、探针节点、⌘. 的 `Stop`。回复框、权限请求、草稿（第 2 步）、真正的 Terminal 页与离开守卫（第 3 步）、`app` 装配与基准（第 4 步）还没有做。

## 模块

| 文件 | 内容 |
|---|---|
| `plugin.rs` | `ConversationPlugin`、`LivePlugin`、`plugins()`；`Shared`（服务引用、当前会话 `Current`、活着的页的通道）、`Job`；`ScreenChanged` 与 `ConversationEvent` 的监听只转发 |
| `items.rs` | 纯函数：`build(&ConversationSnapshot)` 算列表的项，`diff` 找出变化的一段，`arrivals` 找出到达的项；不读服务 |
| `present.rs` | 纯函数：快照里的值变成界面文字（状态行、工具调用行、审阅意见行、链路失败的原因） |
| `view/tab.rs` | `ConversationTab`：检查器页，跟随 `Shared::current`，持有 `MessagesModel`，在主线程上取通道里的 `Job` |
| `view/messages.rs` | `MessagesModel`：一个会话的 Messages 页（关注、快照、列表、计划、命令） |
| `actions.rs`、`nodes.rs`、`diag.rs` | Action 与快捷键；探针 ID；日志字段的辅助（只有计数和判别名） |
| `testing.rs`（测试） | `services` trait 的假实现（`FakeSessions`、`FakeConversations`、`FakeContributions`）和 `Rig`（真实的 kernel） |
- issue：[#23](https://github.com/SoloJiang/lectern-app/issues/23)、[#24](https://github.com/SoloJiang/lectern-app/issues/24)、[#12](https://github.com/SoloJiang/lectern-app/issues/12)
- ADR：[0012](../../docs/adr/0012.md)、[0050](../../docs/adr/0050.md)

## 边界

- 会话内容来自 `agents`，终端网格由 `terminal_view` 绘制。
- 本 crate 不做 IO、不启动进程、不读会话日志：视图读 `dyn Conversations` 的快照（取锁复制一个 `Arc`），经 `command` 发命令。监听（executor 线程）只转发到主线程的通道。
- 日志只经 `lectern-logging` 的宏，字段是计数、代数和判别名；不记 prompt、草稿、agent 的文字、注入内容、工具的 `summary`、权限请求的标题与命令、改动预览、计划、`PromptFailed` 的文字、选项 ID、审阅意见的位置、会话标题和 `EntryKey`（设计文档「日志与诊断」，金丝雀测试 `logs_hold_no_contents`）。
- 界面字符串都走 `keys::conversation::*`；agent 写的内容用 `Text::verbatim`。颜色、字体、时长、控件只来自 `ui`（验收的 `rg` 检查）。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-conversation       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化：`src/tests/stage.rs`（测试窗口里放检查器的页、`frame`、`tree_of`）、`views.rs`、`plugin.rs`（真实的 kernel 加假服务）、`tree.rs`（语义树的 insta 快照，更新用 `INSTA_UPDATE=always just test -p lectern-conversation`，之后用 `git diff` 逐个看）。不要写启动、探针或基准类的开真实窗口的测试。
- 纯函数（`items_*`）是普通 `#[test]`，在 `src/tests/items.rs`。
- 金丝雀测试 `logs_hold_no_contents` 的开关是进程级的，要用 nextest（`just test` 就是）。
- 视图只读领域层提供的快照。测试里直接构造快照（`testing::snapshot`、`with_entries`），不使用真实的 git、会话扫描或文件 IO。本 crate 不依赖 `agents` 等插件 crate 的 `test-support`。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），语义树用 insta 快照验收（`ui::semantic::Recorder`）。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
