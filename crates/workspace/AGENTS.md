# workspace（`lectern-workspace`）

窗口骨架：侧栏、工具栏、检查器、浮动面板、底部浮条，以及按需呼出与收起的层级切换。

- 层：视图层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "workspace"，提供 `dyn Contributions`，依赖 `dyn Projects`，`AfterFirstFrame`。只依赖 kernel 层和非插件的库 crate（第 2 步起加 `ui`、`i18n`、`platform`），不依赖贡献方（`project_home`、`review_view`、`file_reader` 等）；只有 `app` 和 tool 层依赖本 crate。
- 设计文档：[docs/design/workspace.md](../../docs/design/workspace.md)
- issue：[#215](https://github.com/SoloJiang/lectern-app/issues/215)
- ADR：[0012](../../docs/adr/0012.md)、[0015](../../docs/adr/0015.md)、[0017](../../docs/adr/0017.md)、[0025](../../docs/adr/0025.md)、[0046](../../docs/adr/0046.md)
- 进度（设计文档「验收」）：第 1 步已实现，不依赖 GPUI：`nav`（导航状态机与守卫轮询）、`registry`（贡献注册表）、`driver`（主线程循环，输出 `Effect`）、`Hub`（`dyn Contributions` 与队列发送端）、`WorkspacePlugin`、`WindowState`。第 2 步（根视图、区域、代理 Action、离开守卫与窗口的联动）和第 3 步（`app` 装配、窗口状态文件、frame autosave、里程碑）尚未实现。

## 边界

- 新增界面元素前对照[交互规范「三层结构」](../../docs/design/interaction.md#三层结构)和 [ADR 0012](../../docs/adr/0012.md)：不做活动栏、文件标签页、常驻文件树、状态栏、底部终端停靠。
- 只提供容器与布局，具体内容由各视图 crate 经 `dyn Contributions` 填入（设计文档「接口」）。载荷的具体类型归 `ui::contrib`，本 crate 经 `registry::Payloads` 检查和取回。
- 不做 IO：窗口状态文件由 `app` 读写，本 crate 只给出 `WindowState` 和变化回调；首帧之前没有本 crate 的代码运行（[ADR 0017](../../docs/adr/0017.md)）。
- 状态机和注册表不依赖 GPUI：`nav`、`registry`、`driver`、`hub`、`plugin` 里不得出现 `gpui`；贡献方的闭包只在主线程、在它的激活准入和日志作用域里、`contain` 包住调用。
- 日志只经 `lectern-logging` 的宏，字段是屏幕 ID、贡献 ID、命令名、计数和错误种类；不记项目路径、标题、侧栏文字和载荷（设计文档「日志与诊断」）。

## 测试

```sh
just test -p lectern-workspace
```

- 第 1 步的测试都是普通 `#[test]`，不依赖 GPUI，在 Linux 和 macOS 上都跑；第 2 步加 `gpui` 后本 crate 只在 macOS 上编译和测试（Linux 的 `just` 命令按依赖图自动排除它），结果以 macOS CI 为准。
- 状态机（`src/tests/nav.rs`）用假的 `nav::Env`（已注册的屏幕与面板、守卫 ID、记录 `Projects` 调用），断言 `Effect` 列表和 `Nav` 的字段。
- 注册表与驱动（`src/tests/registry.rs`）用 `lectern_kernel::testing::TestKernel`：`WorkspacePlugin` 配假的 `dyn Projects` 激活一次；贡献方是一个在 `activate` 里 `own_staged`、在 `started` 里晚登记的测试插件，把 `Hub` 作为 `dyn Contributions` 注入；队列项由测试交给 `driver::Driver`。
- 金丝雀（`src/tests/logs.rs`）：`lectern_logging::testing::capture()`，`assert_absent`。
- 第 2 步起：交互行为用 `#[gpui::test]` 配合 `TestAppContext`；可交互元素带稳定 ID（`视图.区域.条目[业务键]`），语义树用 insta 快照验收；所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
- 不用真实的 git、会话扫描和文件 IO。
