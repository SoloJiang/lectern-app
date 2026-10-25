# review_view（`lectern-review-view`）

审阅视图：会话简报、按轮次的改动叙事、文件已看进度、Drifted 提示、行内审阅意见与底部发送栏。

- 层：视图层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：两个插件，都是 `AfterFirstFrame`："review-view" 依赖 `dyn Contributions`、`dyn Review`、`dyn SessionSource`；"review-view-handoff" 依赖 `dyn Reader`、`dyn CodeIntel`、`dyn Usage`（交接：⇧⌘O、焦点、`share_object`、用量；不在时屏幕照常，只少交接）。`plugins(executor)` 一次建出两个，共用一个 `Shared`。可以依赖视图、领域、基础设施和 kernel 层的非插件 crate；不得依赖其他插件 crate（`lectern-review`、`lectern-file-reader`、`lectern-workspace` 等，dev-dependency 也不行）和 `app`、tool 层；只有 `app` 和 tool 层依赖本 crate。
- 设计文档：[docs/design/review_view.md](../../docs/design/review_view.md)（已接受）。实现按它的「验收」分三步：第 1 步（插件、屏幕、叙事与统一视图、简报、Progress、快捷键、探针节点）和第 2 步（高亮、交接插件、双栏、折叠展开、Analyze 命令）已实现；第 3 步（`app` 装配、`app/open_diff`、帧基准）尚未实现。
- issue：[#15](https://github.com/SoloJiang/lectern-app/issues/15)、[#22](https://github.com/SoloJiang/lectern-app/issues/22)
- ADR：[0012](../../docs/adr/0012.md)、[0019](../../docs/adr/0019.md)

## 边界

- 归因、片段对齐、已看状态都在 `review` 里计算；本视图只读快照、发命令。不读 git、不读会话日志、不碰文件系统，主线程上没有 IO、解析和 git 操作。
- 简报上的 Analyze（[ADR 0019](../../docs/adr/0019.md)）：执行时发命令给领域层，显示进行中、可取消；离开审阅页或关闭项目时发出取消命令；结果、失败原因和「不可用」的原因都从领域层的快照读取；领域层判定结果的指纹或会话修订号（被分析会话的轮次数与最后一条记录的哈希）与当前不一致时，显示为「基于较早的版本」并提供重新分析。视图只发 `Analyze` / `CancelAnalyze` 并读 `brief.analysis`，不接触被分析的内容，也不发任何请求；屏幕 drop 时 `Running` 的分析照常取消。
- 打开一个会话的 Diff < 50 ms；滚动稳定 120 fps，长 diff 用虚拟滚动（叙事是 gpui 的 `list`，Progress 列表也是）。
- 会话简报为本会话改到的每个分支各显示一行，P3 起每个 MR 一行（一个分支同时对应几个 MR 时，如分别合向两个目标分支，每个 MR 各占一行，显示自己区间内的累计 token，并注明与哪些 MR 共用这个分支，不只显示其中一个）：累计 token 和本会话的占比，当前审阅的改动所在的分支或 MR 排第一；每轮标题显示该轮 token（[ADR 0020](../../docs/adr/0020.md)）。用量行与轮次 token 读 `dyn Usage` 的 `SessionUsage`（交接插件在时）。
- 颜色、字体、时长、控件只来自 `lectern-ui`；界面字符串只经 `lectern-i18n`；日志只经 `lectern-logging` 的宏，字段是 `ReviewId`、计数、`generation`、判别名和耗时，不记路径、会话标题、prompt、说明、命令、工具名、分支名、diff 的行（设计文档「日志与诊断」）。设计文档「验收」里的五条 `rg` 检查每次提交前都跑。

## 模块

| 文件 | 内容 |
|---|---|
| `plugin.rs` | 插件 "review-view" 与 "review-view-handoff"、`plugins(executor)`、`Shared`（服务引用、交接的三个服务、执行器、各屏幕的事件通道、所选的模式）、三项贡献的登记 |
| `model.rs` | `ReviewModel`：快照、项、展示行与正文、选中、`Prepare`、V / J / K / Esc 的状态变化、屏幕 drop 时的收尾 |
| `items.rs` | 叙事列表的项与 `build`：快照加已准备的行的纯函数，`review_view/items_5000` 基准直接调它 |
| `anchor.rs`、`needs.rs` | 刷新后找回滚动位置；可见范围到 `Prepare` 请求的键 |
| `brief.rs` | 简报每条规则的 Fluent 键与参数；用量行（`usage_lines`）与轮次 token |
| `highlight.rs` | 两侧文本的高亮：窗口的计划（`plan_windows`）、按 OID 键的区间缓存（≤ 2 MiB LRU）、executor 任务里的 `highlight::window`（只此一处调用）、屏幕读的区间 |
| `lines.rs` | 一行 diff 的显示文本（tab、截断）与着色段（token 加词级差异） |
| `diffs.rs` | 模型对已准备的 diff 做的事：提交高亮任务和收结果、`share_object` 登记、统一 / 双栏切换、折叠展开 |
| `handoff.rs` | 交接：⇧⌘O / ⌘E 打开文件、`CodeIntel::set_focus`、Space、用量的关注 |
| `screen.rs`、`render.rs`、`render_diff.rs`、`progress_tab.rs` | `ReviewScreen`（加载 / 失败 / 内容、浮条、工具栏的模式切换与刷新）、各项的元素、diff 行（统一与双栏）与折叠、检查器的 Progress 页 |
| `actions.rs`、`nodes.rs`、`diag.rs` | Action 与 `KeyBindings`、探针的节点 ID、日志字段 |
| `testing.rs` | `test-support`：`FakeReview`、`snapshot_fixture`；GPUI 的测试台在 `src/tests/support.rs`，交接三个服务的假实现在 `src/tests/fakes.rs` |

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-review-view       # macOS
just bench review_view/                # macOS：review_view/items_5000
```

- 用 nextest 跑（`just test`）：探针的记录器是进程级的开关，`cargo test` 的多线程会让几个语义树测试互相干扰。
- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化；窗口、假的 `dyn Contributions` 和真实的 kernel 在 `src/tests/support.rs` 的 `Rig` 里（"review-view" 和 "review-view-handoff" 各一个 `TestKernel`，高亮任务由 `Executor::manual()` 在 `run_tasks` 里运行）。`#[gpui::test]` 不画真实的帧：动画（勾选、滚动、`ARRIVE`）的时间由测试直接给出。
- 视图只读领域层提供的快照。测试里直接构造快照（`testing::snapshot_fixture`），不使用真实的 git、会话扫描或文件 IO；本 crate 不能依赖 `lectern-review`，用 `testing::FakeReview`。
- 可交互元素带稳定 ID（`视图.区域.item[业务键]`），语义树用 insta 快照验收（`src/tests/snapshots/`）。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
