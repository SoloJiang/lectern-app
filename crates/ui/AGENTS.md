# ui（`lectern-ui`）

系统色 token、动画原语和通用控件，封装 gpui-component。视图里的颜色、字体、动画都从这里取（[ADR 0015](../../docs/adr/0015.md)）。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：[docs/design/ui.md](../../docs/design/ui.md)（已接受）。实现按它的「验收」分六步：第 1 步（初始化、颜色、文本、字体与代码配色、`contrib`）、第 2 步（动效原语 `motion`）、第 3 步（控件、`IconAssets`、`semantic` 的节点类型）、第 4 步（探针的记录器 `Recorder` 与包装元素 `semantic(...)`、无障碍映射、控件的语义节点快照）、第 5 步（第二批控件）和第 6 步（多行文本框 `TextArea`）。前置条件 A 在 `platform`（已实现）；`app` 的接入（`with_assets(IconAssets)`、`init`、`attach`、`ready`）在 `app` 的 PR。
- issue：[#214](https://github.com/SoloJiang/lectern-app/issues/214)；探针节点与 [#6](https://github.com/SoloJiang/lectern-app/issues/6) 对齐
- ADR：[0013](../../docs/adr/0013.md)、[0015](../../docs/adr/0015.md)、[0016](../../docs/adr/0016.md)、[0017](../../docs/adr/0017.md)、[0046](../../docs/adr/0046.md)

## 边界

- 只用系统字体、系统语义色和系统控件；浅色与深色由系统外观决定。
- 颜色值只出现在 `src/palette/builtin.rs`（内置表、agent 标识色、强调色上的文字色）。其他文件不写 `hsla(`、`rgb(`、`rgba(` 和十六进制颜色，验收用 `rg` 检查（设计文档「验收」）。
- 首帧之前只有 `init`：读减弱动态效果、登记 `platform` 的通知、用内置表建 `Palette`；系统色在 `ready`（首帧之后）才读（[platform 设计「系统外观」](../../docs/design/platform.md#系统外观)）。不做 IO、不建线程。
- 动画时长与曲线见[交互规范「动效」](../../docs/design/interaction.md#动效)，在 `motion.rs` 里定义一次，视图不自定义；本 crate 其他文件也不写 `Duration::from_millis`（验收用 `rg` 检查）。控件用到的加载阈值与提示时长（`LOADING_SKELETON_AT`、`LOADING_PROGRESS_AT`、`HINT_DURATION`）也在 `motion.rs`。
- 弹出的菜单是系统菜单（gpui-component 的 `native_menu::NativeMenu`），不用它自己画的 `PopupMenu` / `ContextMenu`；菜单项不是探针节点，选中项经 `Window::dispatch_action` 派发。
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
| `palette/` | `Appearance`、`SystemColor`、`Palette`、`colors`、`palette`、`accent_follows_system`、`on_accent`（强调色上的字色）；`builtin.rs` 是内置表 |
| `text.rs` | `Text`、`Verbatim`、`strings`、`set_locale` |
| `font.rs` | 字体家族常量、`TextRole`、`TextRoleExt`、`MonoMetrics`、`mono_metrics` |
| `code.rs` | `CodeStyle`、`code_style`、`code_styles` |
| `terminal.rs` | `TerminalColors`、`terminal_colors`（终端的默认色与 ANSI 16 色，[terminal_view 设计](../../docs/design/terminal_view.md#对其他-crate-的要求)） |
| `contrib.rs` | `workspace` 贡献的载荷类型与 `into_payload` |
| `icon.rs` | `Icon` 枚举、Lucide 文件名的对应、`IconAssets`（包着 `icon_assets!` 生成的来源，只嵌入这 16 个 SVG 和应用图标 `assets/app-icon.svg`） |
| `controls/` | 共用控件，每个一个文件：`Button`（含只画图标的 `icon_only`）、`AgentDot`、`CircleCheck`、`Progress`、`Segmented`、`GroupedList` 与 `Row`（卡片行与紧凑行）、`loading_phase` 与 `Skeleton`、`Expander`、`Hint`，第二批的 `ActionCard`、`menu.rs`（`MenuItem`、`show_menu`、`PopUpButton`、行的右键菜单）、`SidebarRow`、`Capsule`、`Disclosure`、`AppIcon`（整块是一个 SVG，颜色在文件里，不在 Rust 里），多行文本框 `TextArea` 与 `TextAreaState`（包装 gpui-component 的 `Textarea`；⌘↩ 与 Esc 在外框上以 `capture_action` 处理，见设计文档「多行文本框」），以及共用的悬停过渡 `hover.rs`；`mod.rs` 的 `require_ready` 是每个 render 开头的断言 |
| `semantic.rs` | `NodeId`、`Role`、`Label`、`State`、`Semantic`、`Record`、`Recorder`、包装元素 `semantic(...)` / `semantic_interactive(...)`（`SemanticElement`）、无障碍属性的映射，以及控件声明节点的接缝 `declare` |
| `state.rs` | 唯一的 GPUI 全局 `UiState` |
| `testing.rs` | `test-support` feature：`simulate_appearance_change`、`simulate_platform_change`、`mono_resolutions` |

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-ui       # macOS
just doctest                  # compile_fail：字面量传不进 Text 和控件
```

- 测试在 `src/tests/`，全用 `init_fixed`；只有 `init_then_ready_reads_the_system` 读真实的 AppKit。
- `Transition` 的测试把测试体放进一个探针视图的 render 里跑（`tests/motion.rs` 的 `in_render`），时间由测试构造的 `Instant` 给出。基准 `ui/transition_tick`（`testing::step_transition`，不含帧请求）、`ui/transition_tick_frame`（含帧请求，只记录）、`ui/bezier_eval` 在 `bench`（`just bench ui/`，只在 macOS）。
- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化（`tests/controls.rs` 的 `circle_check_toggles_from_key_and_click`：`cx.bind_keys` + `simulate_keystrokes` 走视图的 Action，`simulate_click` 点元素中心；`every_control_renders_after_ready` 每个控件各渲染一次）。测试平台不能从 GPUI 之外改窗口外观，外观变化经 `testing::simulate_appearance_change`。
- `TextArea` 的行为测试在 `tests/text_area.rs`（无头的 `TestAppContext`；输入用 `simulate_input` / `simulate_keystrokes`，输入法组字用 `replace_and_mark_text_in_range` 造标记范围）。其中高度测试打开记录器，同样要用 nextest。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 配色表用 insta 快照（`src/tests/snapshots/`）；颜色的色觉辨识检查在 `tests/colors.rs`，换一行颜色先让它通过。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`）。每个控件声明的节点各有一份 insta 快照（`tests/probe.rs`，`semantic_snapshot_*`）；记录器、父子关系、`declared_at` 和无障碍映射的测试也在那里。记录器的开关是进程级的，测试要用 nextest（一个测试一个进程）；只有 `ui_probe`、测试和基准打开记录器（设计文档「验收」第 4 步的 `rg` 检查这一点）。
- 基准 `ui/semantic_disarmed`（`bench`）测包装元素在关闭时读的那一次原子加载。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供；本 crate 没有自己的界面字符串。
