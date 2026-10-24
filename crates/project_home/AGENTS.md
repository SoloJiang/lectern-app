# project_home（`lectern-project-home`）

项目主页与项目侧栏：Overview（Needs Review、Outside Any Session、Recently Done）、Sessions、Packages，底部 Usage 额度条。

- 层：视图层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "project-home"（依赖 `dyn Projects`、`dyn SessionSource`、`dyn Review`、`dyn Contributions`）和 "project-home-usage"（依赖 `dyn Usage`），都是 `AfterFirstFrame`。只依赖 kernel 层和非插件的库 crate（`ui`、`i18n`、`executor`、`logging`），不依赖 `project`、`agents`、`review`、`usage`、`workspace` 这些插件 crate（dev-dependency 也不行，架构检查第 6 条）；只有 `app` 依赖本 crate。
- 设计文档：[docs/design/project_home.md](../../docs/design/project_home.md)
- issue：[#30](https://github.com/SoloJiang/lectern-app/issues/30)
- ADR：[0012](../../docs/adr/0012.md)、[0022](../../docs/adr/0022.md)、[0025](../../docs/adr/0025.md)、[0046](../../docs/adr/0046.md)
- 进度（设计文档「验收」）：第 1 步已实现，不依赖 GPUI：`snapshot`（`HomeSnapshot` 与纯函数 `snapshot::build`）、`Home`（插件状态与合成任务）、`ProjectHomePlugin`、`ProjectHomeUsagePlugin`、`UsageSlot`。第 2 步已实现，依赖 GPUI：`HomeScreen`（主区）、`HomeSidebar`（侧栏面板）、`actions`（Action 与快捷键）、`contribute`（`activate` 里登记的四项贡献）、`nodes`（探针 ID）、`present`（行里的文字）。第 3 步（`app` 装配、基准、`Overview.packages` 的接入）尚未实现。

## 模块

| 文件 | 内容 |
|---|---|
| `snapshot.rs`、`snapshot/` | `HomeSnapshot` 与 `build`（纯函数，不依赖视图）；`NodeId` 随快照建一次 |
| `home.rs` | `Home`：插件状态、合成任务、视图的唤醒通道（`attach_view`）、经 `Contributions` 的 `dispatch` / `sidebar_entries` / 里程碑 |
| `plugin.rs`、`usage_slot.rs` | 两个插件与共用的 `UsageSlot` |
| `contribute.rs` | `Screen`、`SidebarPanel`、`SidebarEntry("overview")`、`KeyBindings` 的载荷与登记 |
| `screen.rs` | `HomeScreen`：大标题与三个 `GroupedList`、选中项、进度环的 `Transition`、等待概览的占位、滚动到可见 |
| `sidebar.rs` | `HomeSidebar`：一个 `uniform_list` 装 `Project` 条目、快照的行和 `Views` 条目，底部固定 `Bottom` 条目（Usage 行） |
| `actions.rs`、`nodes.rs`、`present.rs` | Action 与键绑定（谓词 `Content > project-home`）；探针 ID；行里的文字（相对时间、信号） |

## 视图的规则

- 视图只读 `Home::snapshot()` 的 `Arc` 并发 `WorkspaceCommand`（经 `Home::dispatch`），不做 IO、不调阻塞的服务方法。`Home` 在快照、条目或当前屏幕变化时唤醒视图（`attach_view` 的通道，消息不带内容），视图自己重读。
- 不写颜色值、字体名和动画时长：颜色与字体经 `lectern_ui::colors` / `TextRole`，动画用 `motion` 里的常量（`PROGRESS`、`HOVER_IN`、`LOADING_SKELETON_AT` 等）。`Duration::from_millis` 也不出现（验收的 `rg`）。
- 界面字符串只经 `keys::project_home::*`；会话标题、分支名、子项目名、agent 名用 `Text::verbatim` 或 `Args::str`。
- `NodeId` 的键来自文件和路径，一律先过 `nodes::safe`（换行和 `].` 会让 `NodeId` 在调试构建里 panic）。
- 日志不含标题、项目名、路径、分支名和贡献条目的文字。

## 边界

- 打开项目到主页可用 < 100 ms：文件列表直接读 git index，不遍历目录（[性能预算](../../docs/design/tech-research.md#性能预算)）。
- 会话归位、改动统计由 `project`、`agents`、`review` 计算，本视图只显示快照；本 crate 不读会话文件、不比较工作区，不做 IO（没有 `std::fs`、`std::process`、git 库）。
- 「会话外的改动」的计数由 `review` 按 [ADR 0035](../../docs/adr/0035.md) 的限制计算，见 [review 的 AGENTS.md](../review/AGENTS.md)。
- 会话卡片不显示 token。侧栏底部的 Usage 入口显示本项目最近 7 天的 token（不显示费用，[ADR 0022](../../docs/adr/0022.md)）；MR 的用量显示在并行 worktree 总览和 Usage 页（[ADR 0020](../../docs/adr/0020.md)）。
- 监听（executor 线程）只改标记并安排一次合成任务；合成任务在 `Background` 优先级、持有 `BusyGuard`，同一时刻最多一个。`activate` 取服务引用、经 `ctx.own_staged` 登记四项贡献（`Screen`、`SidebarPanel`、`SidebarEntry`、`KeyBindings`）并订阅，不提交任务、不读服务；视图由 `workspace` 在需要时建；首帧之前没有本 crate 的代码运行。
- 日志只经 `lectern-logging` 的宏，字段是计数、代数、判别名和 `Value::uuid`；不记会话标题、项目名、分支名、子项目名、路径和贡献条目的文字（设计文档「日志与诊断」）。
- 所有界面字符串走 `lectern-i18n`（键在 `crates/i18n/locales/{en,zh-CN}/project-home.ftl`，视图随第 2 步），en 与 zh-CN 同时提供，视图里不写字符串字面量、颜色值、字体名和动画时长。

## 测试

```sh
cargo check -p lectern-project-home       # 只改本 crate 时最快的反馈
just test -p lectern-project-home
```

- 本 crate 依赖 `gpui` 与 `lectern-ui`，只在 macOS 上编译和测试（Linux 的 `just` 命令按依赖图自动排除它），结果以 macOS CI 为准。`snapshot::build` 与插件的测试仍是普通 `#[test]`，不开窗口。
- `src/tests/build.rs`：`snapshot::build` 的分类规则，`HomeSnapshot` 渲染成文本（`snapshot/render.rs`）做 insta 快照（`src/tests/snapshots/`）；快照改动要在 PR 里逐个看过，更新用 `INSTA_UPDATE=always just test -p lectern-project-home`，之后 `git diff` 检查。
- `src/tests/plugin.rs`：两个插件在真实的 kernel 里（`Kernel::start`，假服务由 `testing::Provide` 提供，被测插件用 `Manual` 包一层，由测试决定激活顺序），`Executor::manual()` 在测试说跑的时候才跑合成任务；事件由测试经 `Kernel::emitter()` 同步发出。
- `src/tests/logs.rs`：金丝雀测试 `canary_stays_out_of_home_logs`，`lectern_logging::testing::capture()`。
- 假服务在 `src/testing.rs`（`FakeProjects`、`FakeSessions`、`FakeReview`、`FakeUsage`、`FakeContributions`），不依赖 `review`、`usage` 等插件 crate 的 `test-support`；本 crate 不得调用的方法（`read`、`turns`、`load`、`resolve` 等）一律 `unreachable!`。
- `src/tests/stage.rs`：视图的 GPUI 台架。`rig(cx)` 做 `init_fixed`、`allow_parking`（kernel 线程会唤醒视图的前台任务）并建 `Rig`；`standard(&rig)` 是有六个会话的夹具；`window(cx, &rig)` 用插件登记的 `Screen` / `SidebarPanel` 工厂建两个视图（侧栏在 248pt 的 `Sidebar` 区，主区在 `Content` 区的 760pt 栏里），绑定登记的键绑定（谓词必须能被 GPUI 解析）；`tree(vcx, …)` 在 `ui::semantic::Recorder` 下画一帧，输出紧凑的语义树文本（没有 `bounds` 和 `declared_at`）；`click(vcx, id)` 点节点的中心。
- `src/tests/views.rs`：`#[gpui::test]` 的行为测试（按键、点击、跟随服务、占位、滚动、里程碑）。`src/tests/tree.rs`：三份语义树的 insta 快照（`src/tests/snapshots/`，更新用 `INSTA_UPDATE=always just test -p lectern-project-home`，之后用 `git diff` 逐个看）。`src/tests/present.rs`：行里的文字（文字不在语义树里）。记录器的开关是进程级的，要用 nextest（`just test` 就是）。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`）；语义树里 `Row` 自己的节点只有 `selected`，卡片的事实（agent、进度、信号）在行内容的 `….facts` 节点上。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
