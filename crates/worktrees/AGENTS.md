# worktrees（`lectern-worktrees`）

并行总览：按 Active / Idle 分组列出每个 worktree 的分支、会话和改动量；检查器对比所选 worktree 与 main，并提示与其他 worktree 的文件重叠。

- 层：视图层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "worktrees"（依赖 `dyn Projects`、`dyn SessionSource`、`dyn Review`、`dyn Contributions`）和 "worktrees-usage"（依赖 `dyn Usage`），都是 `AfterFirstFrame`。只依赖 kernel 层和非插件的库 crate（`ui`、`i18n`、`executor`、`logging`），不依赖 `project`、`agents`、`review`、`usage`、`workspace`、`project_home` 这些插件 crate，也不依赖 `lectern-git`（dev-dependency 也不行，架构检查第 6 条）；只有 `app` 依赖本 crate。
- 设计文档：[docs/design/worktrees.md](../../docs/design/worktrees.md)
- issue：[#16](https://github.com/SoloJiang/lectern-app/issues/16)
- ADR：[0006](../../docs/adr/0006.md)、[0022](../../docs/adr/0022.md)、[0025](../../docs/adr/0025.md)、[0046](../../docs/adr/0046.md)
- 进度（设计文档「验收」）：第 1 步已实现，不开窗口：`snapshot`（`WorktreesSnapshot` 与纯函数 `snapshot::build`）、`Hub`（插件状态与合成任务）、`WorktreesPlugin`、`WorktreesUsagePlugin`、`UsageLink`（#339）。第 2 步（视图、Action、屏幕与检查器页的登记）和第 3 步（`app` 装配、里程碑、基准）尚未实现。

## 模块

| 文件 | 内容 |
|---|---|
| `snapshot.rs`、`snapshot/` | `WorktreesSnapshot` 与 `build`（纯函数，不读服务也不读时钟）；`NodeId` 随快照建一次；`render.rs`（测试）把快照渲染成文本做 insta 快照 |
| `hub.rs` | `Hub`：插件状态、合成任务、侧栏条目的登记、关注的持有、视图的唤醒通道（`attach_view`）、`switches` |
| `plugin.rs`、`usage_link.rs` | 两个插件与共用的 `UsageLink`（一组 `UsageQuery` 的关注，对账） |
| `nodes.rs`、`diag.rs` | 探针 ID（业务键先过 `nodes::safe`）；日志字段的辅助（只有计数） |
| `testing.rs`（测试） | `services` trait 的假实现、夹具和 `Rig` |

## 边界

- 切换 worktree / 分支 < 100 ms，依赖以 blob OID 为键的缓存命中。
- 每个 worktree 显示当前分支的累计 token（不显示费用，[ADR 0022](../../docs/adr/0022.md)；P1 标为分支用量，P3 起为所属 MR 的用量；一个分支同时对应几个开着的 MR 时，每个 MR 各列一项自己区间内的用量，并注明与哪些 MR 共用这个分支，不只显示其中一个），数据来自 `usage` 的快照（[ADR 0020](../../docs/adr/0020.md)）。
- 监听（executor 线程）只改标记并安排一次合成任务；合成任务在 `Background` 优先级、持有 `BusyGuard`，同一时刻最多一个。`activate` 只取服务引用并订阅，不提交任务、不读服务；首帧之前没有本 crate 的代码运行。
- 屏幕不是当前屏幕时不合成，不持有 `Review::watch_worktrees` 和 `Usage::watch`，只数当前 worktree 的变化（`switches`）。
- 日志只经 `lectern-logging` 的宏，字段是计数、代数、下标和判别名；不记分支名、引用名、根路径、`WorktreeKey`、重叠的路径、会话标题、项目名、MR 标题和 token 数（设计文档「日志与诊断」）。
- 本 crate 不读会话文件、不比较工作区，不做 IO（没有 `std::fs`、`std::process`、git 库）。

## 测试

依赖 `lectern-ui`（`NodeId`、`SidebarItem`），它依赖 GPUI，所以只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-worktrees       # macOS
```

- 第 1 步的测试是普通 `#[test]`，不开窗口：`src/tests/build.rs`（`snapshot::build` 的合成规则，快照渲染成文本做 insta，更新用 `INSTA_UPDATE=always just test -p lectern-worktrees`，之后用 `git diff` 逐个看）；`src/tests/plugin.rs`（两个插件在真实的 kernel 里，`Executor::manual()` 在测试说跑的时候才跑合成任务，事件由测试经 `Kernel::emitter()` 同步发出）；`src/tests/usage_link.rs`；`src/tests/logs.rs`（金丝雀测试 `canary_stays_out_of_worktrees_logs`，`lectern_logging::testing::capture()` 的开关是进程级的，要用 nextest，`just test` 就是）。假服务在 `src/testing.rs`，不依赖 `review`、`usage` 等插件 crate 的 `test-support`；本 crate 不得调用的方法一律 `unreachable!`。
- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化（第 2 步）。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
