# project_home（`lectern-project-home`）

项目主页与项目侧栏：Overview（Needs Review、Outside Any Session、Recently Done）、Sessions、Packages，底部 Usage 额度条。

- 层：视图层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "project-home"（依赖 `dyn Projects`、`dyn SessionSource`、`dyn Review`、`dyn Contributions`）和 "project-home-usage"（依赖 `dyn Usage`），都是 `AfterFirstFrame`。只依赖 kernel 层和非插件的库 crate，不依赖 `project`、`agents`、`review`、`usage`、`workspace` 这些插件 crate（dev-dependency 也不行，架构检查第 6 条）；只有 `app` 依赖本 crate。
- 设计文档：[docs/design/project_home.md](../../docs/design/project_home.md)
- issue：[#30](https://github.com/SoloJiang/lectern-app/issues/30)
- ADR：[0012](../../docs/adr/0012.md)、[0022](../../docs/adr/0022.md)、[0025](../../docs/adr/0025.md)、[0046](../../docs/adr/0046.md)
- 进度（设计文档「验收」）：第 1 步已实现，不依赖 GPUI：`snapshot`（`HomeSnapshot` 与纯函数 `snapshot::build`）、`Home`（插件状态与合成任务）、`ProjectHomePlugin`、`ProjectHomeUsagePlugin`、`UsageSlot`。第 2 步（视图、贡献登记、Action、语义树）和第 3 步（`app` 装配、里程碑、基准）尚未实现。

## 边界

- 打开项目到主页可用 < 100 ms：文件列表直接读 git index，不遍历目录（[性能预算](../../docs/design/tech-research.md#性能预算)）。
- 会话归位、改动统计由 `project`、`agents`、`review` 计算，本视图只显示快照；本 crate 不读会话文件、不比较工作区，不做 IO（没有 `std::fs`、`std::process`、git 库）。
- 「会话外的改动」的计数由 `review` 按 [ADR 0035](../../docs/adr/0035.md) 的限制计算，见 [review 的 AGENTS.md](../review/AGENTS.md)。
- 会话卡片不显示 token。侧栏底部的 Usage 入口显示本项目最近 7 天的 token（不显示费用，[ADR 0022](../../docs/adr/0022.md)）；MR 的用量显示在并行 worktree 总览和 Usage 页（[ADR 0020](../../docs/adr/0020.md)）。
- 监听（executor 线程）只改标记并安排一次合成任务；合成任务在 `Background` 优先级、持有 `BusyGuard`，同一时刻最多一个。`activate` 只取服务引用和订阅，不提交任务；首帧之前没有本 crate 的代码运行。
- 日志只经 `lectern-logging` 的宏，字段是计数、代数、判别名和 `Value::uuid`；不记会话标题、项目名、分支名、子项目名、路径和贡献条目的文字（设计文档「日志与诊断」）。
- 所有界面字符串走 `lectern-i18n`（键在 `crates/i18n/locales/{en,zh-CN}/project-home.ftl`，视图随第 2 步），en 与 zh-CN 同时提供，视图里不写字符串字面量、颜色值、字体名和动画时长。

## 测试

```sh
cargo check -p lectern-project-home       # 只改本 crate 时最快的反馈
just test -p lectern-project-home
```

- 第 1 步的测试都是普通 `#[test]`，不开窗口、不依赖 GPUI，在 Linux 和 macOS 上都跑；第 2 步加 `gpui` 与 `lectern-ui` 后本 crate 只在 macOS 上编译和测试（Linux 的 `just` 命令按依赖图自动排除它），结果以 macOS CI 为准。
- `src/tests/build.rs`：`snapshot::build` 的分类规则，`HomeSnapshot` 渲染成文本（`snapshot/render.rs`）做 insta 快照（`src/tests/snapshots/`）；快照改动要在 PR 里逐个看过，更新用 `INSTA_UPDATE=always just test -p lectern-project-home`，之后 `git diff` 检查。
- `src/tests/plugin.rs`：两个插件在真实的 kernel 里（`Kernel::start`，假服务由 `testing::Provide` 提供，被测插件用 `Manual` 包一层，由测试决定激活顺序），`Executor::manual()` 在测试说跑的时候才跑合成任务；事件由测试经 `Kernel::emitter()` 同步发出。
- `src/tests/logs.rs`：金丝雀测试 `canary_stays_out_of_home_logs`，`lectern_logging::testing::capture()`。
- 假服务在 `src/testing.rs`（`FakeProjects`、`FakeSessions`、`FakeReview`、`FakeUsage`、`FakeContributions`），不依赖 `review`、`usage` 等插件 crate 的 `test-support`；本 crate 不得调用的方法（`read`、`turns`、`load`、`resolve` 等）一律 `unreachable!`。
- 第 2 步起：交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化；可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
