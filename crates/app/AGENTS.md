# app（`lectern-app`）

二进制 `lectern`：启动、按 profile 装配插件、首帧后恢复窗口快照。

- 层：入口层。可以依赖任何层；只有本 crate 可以依赖 tool 层，且只能作为 dev-dependency 或不在 `default` feature 里的可选依赖。其他 crate 不得依赖本 crate。
- 设计文档：[docs/design/app.md](../../docs/design/app.md)
- issue：[#199](https://github.com/SoloJiang/lectern-app/issues/199)、[#205](https://github.com/SoloJiang/lectern-app/issues/205)、[#7](https://github.com/SoloJiang/lectern-app/issues/7)、[#38](https://github.com/SoloJiang/lectern-app/issues/38)
- ADR：[0017](../../docs/adr/0017.md)、[0018](../../docs/adr/0018.md)

## 边界

- 首帧前只建窗口、画骨架，外加 `lectern_ui::init`（内存操作：内置的 `Palette`、减弱动态效果、登记 `platform` 的通知；不读系统色）和骨架窗口的 `lectern_ui::attach`；`lectern_ui::ready`（`gpui_component::init`、系统色）在 `startup::release` 里、`Kernel::start` 之前。插件激活、SQLite、文件监听、会话扫描、读窗口快照都放在首帧之后（[ADR 0017](../../docs/adr/0017.md)、[ui 设计「对其他 crate 的要求」](../../docs/design/ui.md#对其他-crate-的要求)）。
- 只做装配：把 `platform` 的实现注入 `fs_watch`、`kernel` 等定义的接口。不写业务逻辑。
- `workspace`：`startup::release` 里在 `lectern_ui::ready` 之后、`Kernel::start` 之前 `install`，窗口状态文件（`app_data()/workspace/window.json`）和 frame autosave 名在 `src/window_state.rs`。读写都在 executor 的 `Background` 任务里，路径不进日志；首帧之前只对骨架窗口设 `"welcome"` 这个 frame autosave 名（AppKit 内存里的偏好，不读文件；有 `bench-probe` 的探针时不设，见下一条）。`welcome` 的三个插件见下一条（[设计「workspace 的装配」](../../docs/design/app.md#workspace-的装配)）。
- `usage`、`project_home`：与 `project`、`review` 一起在 `profile::desktop` 里装配，`UsageDeps::data_dir` 是 `app_data()/usage/`，本地时区是 `profile.rs` 的 `SystemZone`（`platform::time::utc_offset`，向下取整到秒，None 时为 UTC），`UsageSlot` 由 `desktop` 建一个交给 `project_home` 的两个插件。数据库在 `usage` 的后台任务里打开，`app` 不碰（[设计「usage 与 project_home 的装配」](../../docs/design/app.md#usage-与-project_home-的装配)）。
- `welcome`：`lectern_welcome::plugins(WelcomeDeps { executor, data_dir: app_data()/welcome })` 的三个插件（`welcome`、`welcome-discovery`、`welcome-review`）在 `profile::desktop` 里 `workspace` 之后逐个放进 `PluginSet`。`plugins` 只建空的模型，不做 IO；`recent.json` 在 `welcome` 的后台任务里读写，`app` 不碰，路径不进日志（[设计「welcome 的装配」](../../docs/design/app.md#welcome-的装配)）。
- `review_view`：`lectern_review_view::plugins(executor)` 的 `review-view` 在 `profile::desktop` 的最后放进 `PluginSet`；同时返回的 `review-view-handoff` 被丢掉，直到有插件提供 `dyn Reader` 和 `dyn CodeIntel`（[设计「review_view 的装配」](../../docs/design/app.md#review_view-的装配)）。
- `worktrees`：`lectern_worktrees` 的 `worktrees` 与 `worktrees-usage` 在 `profile::desktop` 的最后（`review-view` 之后）放进 `PluginSet`，共用一个 `UsageLink`（`dyn Usage` 可能不存在，与 `project_home` 的 `UsageSlot` 同样的做法）；`new` 只建空状态，不做 IO，首帧之前没有它的代码运行（`tests/startup.rs` 检查）。desktop 与 probe profile 共 14 个插件（[设计「worktrees 的装配」](../../docs/design/app.md#worktrees-的装配)）。
- `ui_probe` 只在 `ui-probe` feature 下编译（不在 `default` 里，`just` 的入口打开它）；运行时只在 probe profile 里启动（[ui_probe 设计](../../docs/design/ui_probe.md)）。`run` 在首帧之前只读环境变量 `LECTERN_UI_PROBE_DIR`（`src/probe.rs`），设置了就是 `Profile::Probe`（插件与 desktop 相同）；`Probe::start` 在 `startup::release` 的末尾，`Quit` 的退出线程在 `Kernel::shutdown` 之前 `shutdown` 它。`quit` 请求靠名字 `lectern::Quit` 分发 `quit.rs` 里的 `Quit` Action，不要改这个名字。端到端测试 `tests/probe.rs` 和 `tests/startup.rs` 一样需要图形会话。发布构建不含探针：`cargo tree --locked -p lectern-app -e normal,build,features --target aarch64-apple-darwin` 里没有 `lectern-ui-probe`。
- `bench_probe`（`src/bench.rs`）只在 `bench-probe` feature 下编译（不在 `default` 里，`just` 的入口打开它）：`lectern-bench` 是 tool 层的可选依赖，`lectern-bench/probe` 打开 `lectern-platform/bench-hooks`。`run` 在 `install_early` 之后、首帧之前只读 `LECTERN_BENCH_SOCKET`、`LECTERN_BENCH_PLUGINS` 并登记首帧回调（一次方法替换，不做 IO、不建线程）；`startup::release_in` 里 `Probe::start_kernel` 代替 `Kernel::start`，随后 `bench::attach`。有探针时不设 frame autosave 名（`window_state::autosave_name`），窗口总是 820 × 520 pt 居中。探针的错误经 socket 交给运行器；日志只记 `bench probe attached`，不记 socket 路径和 `LECTERN_BENCH_PLUGINS`。`Quit` 请求走 `quit::quit`，与 ⌘Q 同一条路径。`Open(Worktree(root))` 在主线程上经 `workspace::Hub::open_snapshots` 按根路径找 worktree，发 `Push(WORKTREES)` 和 `SetActiveWorktree`，与 Worktrees 屏幕的切换按钮同一条路径；找不到或已经是当前时发 `Error`（消息不带路径）。端到端测试 `tests/bench_probe.rs` 和 `tests/startup.rs` 一样需要图形会话。发布构建不含探针：`cargo tree --locked -p lectern-app -e normal,build,features --target aarch64-apple-darwin` 里没有 `lectern-bench`、`"bench-hooks"`（[设计「测量构建的装配」](../../docs/design/app.md#测量构建的装配)）。
- 自动化构建（`tools/automation/`，[ADR 0056](../../docs/adr/0056.md)）只经 `run_with(Hooks)` 接入：`hooks::window_ready` 只在两处调用，屏障第一次成功放开且有窗口时（`Started` 设好之后），以及有 workspace 时的 Dock 重开（`attach` 之后）。本 crate 不依赖 gpui-mcp；不要在首帧之前调用回调（[设计「自动化构建的装配」](../../docs/design/app.md#自动化构建的装配)）。

## 测试

依赖 GPUI，只在 macOS 上编译和测试：

```sh
just test -p lectern-app                     # 不含会打开窗口的测试
just test --profile windows -p lectern-app   # 含 tests/startup.rs、probe.rs、bench_probe.rs
just ci                 # macOS 上包含构建 lectern 与性能基准，与 CI 相同
```

- `tests/startup.rs`、`tests/probe.rs`、`tests/bench_probe.rs` 启动真实的 `lectern`，每个测试在运行者的屏幕上打开一个窗口并取得焦点。本机的 `just test` 默认跳过这三个测试二进制（`.config/nextest.toml` 的 `default-filter`）；CI 的 `--profile ci` 和本机的 `--profile windows` 运行它们。按名字或 `-E` 选这三个测试时，默认 profile 会和 `default-filter` 取交集，结果是 0 个测试而且不报错：要运行它们，用 `--profile windows`。agent 在用户的 Mac 上工作时不要用 `--profile windows`，除非用户同意。

- 冷启动、空闲内存由 `bench` 测量（[#7](https://github.com/SoloJiang/lectern-app/issues/7)），安装包体积由 [#38](https://github.com/SoloJiang/lectern-app/issues/38) 检查。
- 装配逻辑用 test profile 注入模拟服务验证，不依赖真实的项目目录。
