# ui（`lectern-ui`）

系统色 token、动画原语和通用控件，封装 gpui-component。视图里的颜色、字体、动画都从这里取（[ADR 0015](../../docs/adr/0015.md)）。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：[docs/design/ui.md](../../docs/design/ui.md)（已接受）。实现按它的「验收」分四步：第 1 步（初始化、颜色、文本、字体与代码配色、`contrib`）、第 2 步（动效原语 `motion`）和第 3 步（控件、`IconAssets`、`semantic` 的节点类型）已实现；第 4 步（探针的记录器与包装元素、控件的语义节点快照）尚未实现。前置条件 A 在 `platform`（已实现）；`app` 的接入（`with_assets(IconAssets)`、`init`、`attach`、`ready`）在 `app` 的 PR。
- issue：[#214](https://github.com/SoloJiang/lectern-app/issues/214)；探针节点与 [#6](https://github.com/SoloJiang/lectern-app/issues/6) 对齐
- ADR：[0013](../../docs/adr/0013.md)、[0015](../../docs/adr/0015.md)、[0016](../../docs/adr/0016.md)、[0017](../../docs/adr/0017.md)、[0046](../../docs/adr/0046.md)

## 边界

- 只用系统字体、系统语义色和系统控件；浅色与深色由系统外观决定。
- 颜色值只出现在 `src/palette/builtin.rs`（内置表、agent 标识色、强调色上的文字色）。其他文件不写 `hsla(`、`rgb(`、`rgba(` 和十六进制颜色，验收用 `rg` 检查（设计文档「验收」）。
- 首帧之前只有 `init`：读减弱动态效果、登记 `platform` 的通知、用内置表建 `Palette`；系统色在 `ready`（首帧之后）才读（[platform 设计「系统外观」](../../docs/design/platform.md#系统外观)）。不做 IO、不建线程。
- 动画时长与曲线见[交互规范「动效」](../../docs/design/interaction.md#动效)，在 `motion.rs` 里定义一次，视图不自定义；本 crate 其他文件也不写 `Duration::from_millis`（验收用 `rg` 检查）。控件用到的加载阈值与提示时长（`LOADING_SKELETON_AT`、`LOADING_PROGRESS_AT`、`HINT_DURATION`）也在 `motion.rs`。
- 控件只接受 `Text`，每个 render 先 `require_ready`（`ready` 之前 panic，是 `app` 的装配错误）；颜色从 `Palette` 取，悬停 / 按下只做明度偏移或 `label` 的淡色，不写新颜色值。不包装 gpui-component 的 `Skeleton`、`Progress`、`ProgressCircle`（自带关不掉的动画），自绘；只包装它的 `Button`、`Icon`。
- 跨帧的控件状态（`Row` 的悬停过渡）放在 GPUI 的 keyed element state（`window.use_keyed_state`），不放在视图里，也不建 Entity。
- 减弱动态效果由 `motion::reduced` 和 `Motion::effective` 处理（位移改为淡入淡出、时长减半）；不设置 GPUI 自己的 `App::reduce_motion` 标志，它会让 `with_animation` 直接画终态。
- `Transition::tick` 只在视图的 render 里调用：它请求下一帧，GPUI 只允许正在渲染的视图这样做。
- gpui-component 的 API 只在本 crate 和视图层使用；升级要和 GPUI 一起，先确认（[ADR 0016](../../docs/adr/0016.md)）。
- 日志只经 `lectern-logging` 的宏，字段是事件名、外观、`'static` 的字体家族常量和计数；不记 `Text` 的内容、颜色数值、字体的实际名字。

## 模块

| 文件 | 内容 |
|---|---|
| `init.rs` | `init`、`ready`、`is_ready`、`init_fixed`、`attach`，外观 / 强调色 / 减弱动态效果变化的处理，`Palette` 到 gpui-component `Theme` 的投影 |
| `motion.rs` | `Easing`、`Motion` 与交互规范「动效」表的全部常量、`reduced`、`Effective`、`Motion::animation`、可打断的 `Transition`（含弹簧）、`active_transitions` |
| `palette/` | `Appearance`、`SystemColor`、`Palette`、`colors`、`palette`、`accent_follows_system`；`builtin.rs` 是内置表 |
| `text.rs` | `Text`、`Verbatim`、`strings`、`set_locale` |
| `font.rs` | 字体家族常量、`TextRole`、`TextRoleExt`、`MonoMetrics`、`mono_metrics` |
| `code.rs` | `CodeStyle`、`code_style`、`code_styles` |
| `contrib.rs` | `workspace` 贡献的载荷类型与 `into_payload` |
| `icon.rs` | `Icon` 枚举、Lucide 文件名的对应、`IconAssets`（`icon_assets!` 只嵌入这 10 个 SVG） |
| `controls/` | 共用控件，每个一个文件：`Button`、`AgentDot`、`CircleCheck`、`Progress`、`Segmented`、`GroupedList` 与 `Row`、`loading_phase` 与 `Skeleton`、`Expander`、`Hint`；`mod.rs` 的 `require_ready` 是每个 render 开头的断言 |
| `semantic.rs` | `NodeId`、`Role`、`Label`、`State`、`Semantic`，以及控件声明节点的接缝 `declare`（第 3 步是恒等函数，第 4 步换成记录的包装元素） |
| `state.rs` | 唯一的 GPUI 全局 `UiState` |
| `testing.rs` | `test-support` feature：`simulate_appearance_change`、`simulate_platform_change`、`mono_resolutions` |

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-ui       # macOS
just doctest                  # compile_fail：字面量传不进 Text 和控件
```

- 测试在 `src/tests/`，全用 `init_fixed`；只有 `init_then_ready_reads_the_system` 读真实的 AppKit。
- `Transition` 的测试把测试体放进一个探针视图的 render 里跑（`tests/motion.rs` 的 `in_render`），时间由测试构造的 `Instant` 给出。基准 `ui/transition_tick`、`ui/bezier_eval` 在 `bench`（`just bench ui/`，只在 macOS）。
- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化（`tests/controls.rs` 的 `circle_check_toggles_from_key_and_click`：`cx.bind_keys` + `simulate_keystrokes` 走视图的 Action，`simulate_click` 点元素中心；`every_control_renders_after_ready` 每个控件各渲染一次）。测试平台不能从 GPUI 之外改窗口外观，外观变化经 `testing::simulate_appearance_change`。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 配色表用 insta 快照（`src/tests/snapshots/`）；颜色的色觉辨识检查在 `tests/colors.rs`，换一行颜色先让它通过。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供；本 crate 没有自己的界面字符串。
