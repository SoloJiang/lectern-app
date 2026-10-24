# welcome（`lectern-welcome`）

打开项目窗口：最近项目、Open Folder、语言选择，以及从 Claude Code 与 Codex 历史里发现、尚未打开过的文件夹（Clone 在以后的 issue）。

- 层：视图层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "welcome"（屏幕与最近项目，依赖 `dyn Contributions`、`dyn Projects`）和 "welcome-discovery"（发现列表，依赖 `dyn Projects`、`dyn SessionSource`；kernel 没有可选依赖，所以分开），都是 `AfterFirstFrame`，共用一个 `WelcomeModel`。只依赖视图、领域、基础设施和 kernel 层里的非插件 crate，不依赖其他插件 crate（`workspace`、`project`、`agents`）；只有 `app` 和 tool 层依赖本 crate。
- 设计文档：[docs/design/welcome.md](../../docs/design/welcome.md)（实现中，进度见文档开头的状态行）。实现以它为准。
- issue：[#29](https://github.com/SoloJiang/lectern-app/issues/29)
- ADR：[0012](../../docs/adr/0012.md)、[0013](../../docs/adr/0013.md)
- 进度（设计文档「验收」）：第 1 步已实现，不依赖 GPUI：`recent`（最近项目的数据与 `recent.json`）、`discover`（发现列表的归并，纯函数）、`WelcomeModel`（快照与任务）、两个插件。第 2 步已实现：`view`（`WelcomeView`、渲染、选择与点击、语言选择）、`actions`（五个 `Action` 和 `KeyBindings` 载荷）、`prefs`（语言偏好的 trait 与 `platform` 实现）、`ui::contrib::Screen` 载荷。`app` 的装配（前置条件 C）在 `app` 的 PR，在它合并之前 `app` 不装配这两个插件。

## 边界

- 会话历史的扫描在 `agents` 里、首帧之后进行，本 crate 只读 `SessionSource::sessions()` 的快照；`discover` 不读任何目录，测试里的会话都是合成的值（路径是虚构的），不得读取这台机器上真实的会话目录。
- 打开项目经 `workspace` 的 `WorkspaceCommand`（`OpenProject`、`PromptOpenProject`），本 crate 不调用 `Projects` 的 open 方法。
- 最近项目列表存在 `platform::dirs::app_data()` 下的 `welcome/recent.json`（路径由 `app` 经 `WelcomeDeps::data_dir` 传入）。所有 `std::fs` 调用只在 `src/recent/store.rs`，只从 `Background` 任务调用；测试里需要碰文件的辅助函数也放在那里（`store::testing`）。
- 首帧之前本 crate 没有代码运行：`plugins`、`activate` 不做 IO、不建线程，文件在 `started` 提交的任务里读。
- UI 线程上的 `WelcomeModel` 方法只改内存、立即返回；任何耗时的事（读写文件、重算发现列表）都在 executor 的任务里，同一种任务同一时刻最多一个（排队标记 + 脏标记）。
- 日志只经 `lectern-logging` 的宏（target `welcome`）。不记项目名、路径、会话 cwd 与标题、`OpenError` 附带的文字；用条数、种类、耗时代替。

- 视图（`src/view.rs`）只读 `WelcomeModel::snapshot()`，只发 `Contributions::dispatch`、模型里只改内存的方法（`begin_open`、`clear_notice`、`remove_recent`）和语言选择的调用；不做 IO，不读系统时间和环境（「现在」来自模型的时钟，主目录由 `plugins()` 传入）。颜色、字体、控件、动效只从 `lectern-ui` 取：`rg 'rgb\(|hsla\(|Duration::from_millis|from_secs_f32' crates/welcome/src` 必须没有结果。
- 语言偏好只经 `src/prefs.rs` 的 `LanguagePrefs`：生产用 `PlatformPrefs`（`platform::system`），测试用 `FakePrefs`。测试里不得调用真实的 `set_language_override`，它会写测试进程的用户默认值。
- 行的节点：`ui::Row` 声明 `ListItem`，名字和计数在子节点 `….name` 上；`NodeId` 的业务键是路径，含换行或 `].` 的路径经 `node_key` 替换。

## 测试

```sh
just test -p lectern-welcome
```

- 第 1 步的测试是普通 `#[test]`；第 2 步加了 `gpui` 之后本 crate 只在 macOS 上编译和测试（Linux 的 `just` 命令按依赖图自动排除它，GPUI-free 任务不再覆盖它），结果以 macOS CI 为准。
- 纯函数：`src/tests/recent.rs`（列表操作、`recent.json` 的编码与解码、`store` 的读写）、`src/tests/discover.rs`（`discover::build`，insta 快照）。fixtures 在 `tests/fixtures/`，路径都是虚构的。
- 模型与插件：`src/tests/model.rs`。两个插件各在一个 `TestKernel` 上，共用一个 `WelcomeModel`、一个 `Executor::manual()`、一个手动时钟和一个临时数据目录；假的 `dyn Projects`、`dyn SessionSource`、`dyn Contributions` 在 `src/tests/support.rs`（不依赖其他插件 crate 的测试夹具）。`Stats` 的钩子让测试在写文件、重算进行到一半时插入一次变更。
- 金丝雀：`src/tests/logs.rs`，`lectern_logging::testing::capture()`，`assert_absent`。
- 视图：`src/tests/view.rs`，`#[gpui::test]` 配合 `TestAppContext`。窗口里是一个 `Content` 区域的 `KeyContext` 加上登记到假 `dyn Contributions` 里的真实 `Screen` 载荷建出的视图；按键走真实的 `KeyBindings` 载荷；`cx.executor().allow_parking()`（模型的唤醒来自别的线程）；骨架的计时用 `advance_clock`，不能在 `update` 里调。语义树是 `Recorder` 的记录去掉 `bounds` 和 `declared_at` 之后的文本，insta 快照在 `src/tests/snapshots/`；`src/tests/selection.rs` 是选择和字符串的纯函数。可交互元素带稳定 ID（`视图.区域.条目[业务键]`）；所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
- 不用真实的 git、会话扫描和会话目录；写文件的测试用临时目录。
