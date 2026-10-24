# ui 设计

状态：已接受。第 1 步（初始化、颜色、文本、字体与代码配色，`contrib`）已实现（[#230](https://github.com/SoloJiang/lectern-app/pull/230)），`app` 的接入与第 1 步的 `ui/*` 基准在下一个 PR；第 2 步（动效原语 `motion`，基准 `ui/transition_tick`、`ui/bezier_eval`）已实现（[#233](https://github.com/SoloJiang/lectern-app/pull/233)）；第 3、4 步尚未实现。

对应 issue [#214](https://github.com/SoloJiang/lectern-app/issues/214)。`ui` 是每个视图 crate 从中取颜色、字体、动效、共用控件和探针节点的地方，而且只从这里取：视图里不出现颜色值、字体名、动画时长（[ADR 0015](../adr/0015.md)、[交互规范「视觉」](interaction.md#视觉)、[「动效」](interaction.md#动效)）。它封装 gpui-component（[ADR 0016](../adr/0016.md)），把 macOS 的系统语义色、系统字体和「减弱动态效果」的设置接进 GPUI。它不做 IO，不建线程，首帧之前只做一次内存里的读取（[ADR 0017](../adr/0017.md)）。

## 范围

- 做：
  - 系统语义色与强调色：从 macOS 读取（经 `platform`），按窗口外观（浅色 / 深色）解析成 `Palette`；外观或强调色变化时重新读取并刷新窗口。
  - 字体：系统 UI 字体与系统等宽字体的家族常量、字号层级（`TextRole`），以及 `file_reader` 和 diff 行需要的等宽字体度量（`MonoMetrics`，[file_reader 设计「对其他 crate 的要求」](file_reader.md#对其他-crate-的要求)前置条件 E）。
  - 代码配色：`lectern_syntax::TokenKind` 到系统色与字重的映射，浅色 / 深色各一组。
  - 动效原语：[交互规范「动效」](interaction.md#动效)表里的时长与曲线作为具名常量；可打断的过渡（`Transition`，含弹簧）、三次贝塞尔求值；减弱动态效果的替代方案。
  - 界面文本的类型 `Text`：控件只接受来自 `lectern-i18n` 的文本或明确标为原样显示的文本（[i18n 设计](i18n.md)「本 issue 之外」留给本 crate 的两件事之一）；`Strings` 的 GPUI 全局与切换语言后的刷新（另一件）。
  - 共用的小控件：只做 Welcome、项目主页、审阅视图需要、而 gpui-component 没有或与 macOS 不一致的几个（见「控件」），加上 `workspace` 的一次性提示 `Hint`。
  - 贡献载荷的类型（`contrib` 模块）：`workspace` 的注册表里类型擦除的 `Payload` 装的具体类型（屏幕、侧栏面板与条目、检查器页、面板、覆盖层、快捷键）。形状由 [workspace 设计](workspace.md)「`ui::contrib` 的载荷类型」规定，放在本 crate 是因为 `services` 不能依赖 GPUI 和 `lectern-i18n`。
  - UI 探针的语义节点（`semantic` 模块）：节点的类型（角色、稳定 ID、文字、状态、屏幕范围）、给元素附上节点的包装元素，以及探针开关时收集记录的 `Recorder`。`ui_probe`（tool 层）读它，视图 crate 只依赖本 crate（[技术调研「UI 结构探针」](tech-research.md#6-ui-结构探针)、[#6](https://github.com/SoloJiang/lectern-app/issues/6)；形状与 `ui_probe` 的设计一致）。
- 不做：
  - 读 AppKit：`NSColor`、`NSWorkspace` 的减弱动态效果、系统通知，归 `platform`（架构规则 6，见「对其他 crate 的要求」）。本 crate 只经 GPUI 和 `platform` 的接口。
  - 窗口结构：侧栏、工具栏、检查器、面板容器、底部浮条，归 `workspace`（[#30](https://github.com/SoloJiang/lectern-app/issues/30)）。本 crate 给面板滑入的偏移量和透明度，不给容器。
  - 各屏幕的内容、Fluent 文案和 Action：归 `welcome`、`project_home`、`review_view`、`file_reader`。本 crate 没有自己的界面字符串。
  - 探针的导出、截图、Action 触发：归 `ui_probe`。
  - 读写语言设置：`app` 经 `platform` 读系统语言、选出 `Locale` 后传进来（[app 设计](app.md)）。
  - 贡献的注册、注销、导航状态机和把 `Payload` 还原成 `contrib` 类型：归 `workspace`；本 crate 只定义类型和 `into_payload`。
- 本 issue 之外：
  - 开关（Toggle）：第一批屏幕没有开关，只定义它的动效常量 `SWITCH`；控件随设置页一起做。
  - SF Symbols：GPUI 不能直接画 `NSImage`，第一批屏幕的图标用 gpui-component 自带的 Lucide 图标（见「未决问题」1）。
  - 提高对比度（Increase Contrast）、降低透明度：读到的系统色已经随这两个设置变化，但本 crate 不另做 0.5pt 分隔线加粗之类的调整，记为已知限制。
  - 系统无障碍树：语义节点将来可以映射到它，等 GPUI 的 accessibility 稳定后另写设计。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui`、`gpui-component` | 元素、窗口外观、文字系统、动画元素；gpui-component 的 `Button`、`Skeleton`、`ProgressCircle`、`Icon` 作为底层 | 已锁定（[ADR 0016](../adr/0016.md)） |
| `gpui-kit-assets`（`=0.7.3`，gpui-component 的依赖） | `icon_assets!` 宏生成 `IconAssets`：只嵌入 10 个图标 | 已在 Cargo.lock，与 gpui-component 同版本、一起升级；直接依赖只为用它的宏 |
| `lectern-i18n` | `Strings`、`Key`、`FmtKey`、`Args`、`Locale`：`Text` 的构造 | 界面字符串不硬编码（[ADR 0013](../adr/0013.md)） |
| `lectern-platform` | `appearance::{system_colors, reduce_motion, watch}`（前置条件 A） | macOS 专属能力只在 `platform`（架构规则 6）；视图层可以依赖基础设施层 |
| `lectern-services` | `workspace::Payload`：`contrib` 类型的 `into_payload`；`WorkspaceCommand`（`SidebarItem::on_select`） | kernel 层，任何层都可以依赖 |
| `lectern-syntax` | `TokenKind`（[file_reader 设计](file_reader.md)「`syntax` 的公开项」） | 映射的键就是它的枚举，不另抄一份；它是非插件的领域库，视图层可以依赖 |
| `lectern-logging` | 「日志与诊断」的记录；测试用 `test-support` 的 `testing::capture()` | [ADR 0046](../adr/0046.md) |
| `insta`（dev） | 语义树和配色表的快照 | 已在工作区 |

不新增 Cargo.lock 里没有的外部依赖。不开 gpui-component 的 `tree-sitter`、`inspector` feature（后者在 `debug_assertions` 下自动开启，`ui` 不依赖它）。所在层与依赖方向见 [crates/ui/AGENTS.md](../../crates/ui/AGENTS.md)。

## 接口

全部公开项都在主线程上调用（GPUI 的 `App`、`Window` 只在主线程上可用），都不阻塞、不做 IO。例外单独写明。

### 初始化

```rust
/// 首帧之前，`app` 在开窗口之前调用一次。只做内存操作：经 `platform::appearance` 读减弱动态效果，
/// 用内置的表（见「数据结构」）建两份 `Palette`，把 `Strings` 设为 GPUI 全局，登记 `platform` 的变化通知。
/// 不读系统色：AppKit 第一次解析自己的动态颜色约 2 ms（[platform 设计「系统外观」](platform.md#系统外观)的「性能」），
/// 放在 `ready`。预算 ≤ 300 µs（`ui/init`）。不建线程、不读文件、不调用 `gpui_component::init`。
pub fn init(cx: &mut App, strings: Strings);

/// 启动屏障放开后（首帧之后），`app` 在 `startup::release` 里调用一次：运行 `gpui_component::init`
///（它在运行时解析内嵌的主题 JSON，所以不放在首帧前），经 `platform::appearance::system_colors` 读两套系统色
///（读不到时留着内置的表，记一条 warn）并换掉 `Palette`，把 gpui-component 的主题改成跟随 `Palette`，
/// 并按当前窗口外观设置它的浅色 / 深色。「跟随」是一次投影：把 `Palette` 的颜色写进 gpui-component 的 `Theme`；
/// 之后每次换 `Palette`（外观变化、强调色变化）都在 `is_ready` 时重做这次投影，所以包装的控件不会留着旧的强调色。主线程，预算 ≤ 3 ms（`ui/ready`）。
pub fn ready(cx: &mut App);

/// `ready` 是否已经运行。视图在它为 false 时不挂载（首帧是 `app` 的中性骨架，不含任何 `ui` 控件）。
pub fn is_ready(cx: &App) -> bool;

/// 测试与 headless、probe profile 用：不读系统，用内置的表和给定的外观、减弱动态效果；同时运行 `ready`（测试没有首帧的约束）。
pub fn init_fixed(cx: &mut App, strings: Strings, appearance: Appearance, reduced_motion: bool);
```

### 颜色

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Appearance { Light, Dark }

impl Appearance {
    /// `WindowAppearance::{Light, VibrantLight}` → `Light`，`{Dark, VibrantDark}` → `Dark`。
    pub fn of(window: WindowAppearance) -> Appearance;
}

/// 一个外观下的全部颜色。字段都是 GPUI 的 `Hsla`，由 `platform` 给出的 sRGB 分量换算。
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Palette {
    pub appearance: Appearance,
    // 文字与分隔：NSColor 的 labelColor、secondaryLabelColor、tertiaryLabelColor、separatorColor
    pub label: Hsla,
    pub secondary_label: Hsla,
    pub tertiary_label: Hsla,
    pub separator: Hsla,
    // 表面：windowBackgroundColor、controlBackgroundColor、underPageBackgroundColor
    pub window_background: Hsla,
    pub control_background: Hsla,
    pub under_page_background: Hsla,
    // 强调与选中：controlAccentColor、selectedContentBackgroundColor、unemphasizedSelectedContentBackgroundColor
    pub accent: Hsla,
    pub selected_content_background: Hsla,
    pub unemphasized_selected_content_background: Hsla,
    // 系统色：systemRed … systemGray，按 `SystemColor` 的顺序
    pub system: [Hsla; SystemColor::COUNT],
    // 派生：系统绿 / 红按本外观的透明度叠出的 diff 底色（交互规范「视觉」的「淡底色」）
    pub diff_added_background: Hsla,
    pub diff_removed_background: Hsla,
    // 唯一的自定义颜色（ADR 0015）：agent 标识
    pub agent_claude: Hsla,
    pub agent_codex: Hsla,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum SystemColor { Red, Orange, Yellow, Green, Mint, Teal, Cyan, Blue, Indigo, Purple, Pink, Brown, Gray }

impl SystemColor { pub const COUNT: usize = 13; }

impl Palette {
    pub fn system(&self, color: SystemColor) -> Hsla;
}

/// 当前窗口外观下的颜色。渲染时每帧调用多次：只是一次全局查找和一次下标，不分配。
pub fn colors(window: &Window, cx: &App) -> &Palette;

/// 按外观取，供没有窗口的地方（测试、配色表快照）。
pub fn palette(appearance: Appearance, cx: &App) -> &Palette;

/// 强调色是否来自系统（`platform` 读到了 `controlAccentColor`）。读不到时是内置表里的蓝色。
pub fn accent_follows_system(cx: &App) -> bool;
```

视图不订阅颜色变化：每个窗口打开后，打开它的 crate 调用一次 `ui::attach(window, cx)`，`attach` 登记 `Window::observe_window_appearance`；外观变化时 `ui` 换一套 `Palette`、调用 `cx.refresh_windows()`，并且只在 `is_ready(cx)` 时才同步 gpui-component 的浅色 / 深色（GPUI 在 `open_window` 返回后就延后触发一次 `appearance_changed`，这一次发生在 `ready` 之前，那时 gpui-component 的全局还不存在；`ready` 自己做第一次同步）。第一个窗口是 `app` 的骨架窗口（Welcome 和 Overview 只替换它的根视图，窗口不换），所以 `app` 在 `open_window` 的回调里调用 `attach`；以后的窗口由 `workspace` 调用（「对其他 crate 的要求」）。`platform` 的强调色变化通知到达时重新读两套系统色、同样重做投影（`is_ready` 时）并刷新；通知的回调不带 GPUI 的上下文，`init` 把 `cx.to_async()` 交给回调，回调经它把更新排到主线程的下一次运行（与 GPUI 自己处理外观回调的做法相同），不在回调里直接借用 `App`。两次刷新之间 `Palette` 不变，返回的引用在一帧之内有效。

```rust
/// 窗口打开后调用一次（`app` 对骨架窗口，`workspace` 对以后的窗口）：登记外观观察者，使 `colors(window, cx)` 在这个窗口的外观变化时更新。
/// 返回的 `Subscription` 由窗口的根视图持有；重复调用只登记一次。
pub fn attach(window: &mut Window, cx: &mut App) -> Subscription;
```

### 文本

```rust
/// 控件接受的文本。没有 `From<&str>`、`From<SharedString>`：字面量传不进控件。
/// 带着来源：`key` 是 Fluent 消息 ID（`Key::name()`，见「对其他 crate 的要求」的 `i18n`），原样显示的文本为 None；
/// 探针把它记进节点的 `Label`。
#[derive(Clone, Debug)]
pub struct Text { text: SharedString, key: Option<&'static str> }

impl Text {
    /// 来自资源文件的消息（`Strings::get`）。
    pub fn key(strings: &Strings, key: Key) -> Text;
    /// 带参数的消息（`Strings::format`）。
    pub fn fmt(strings: &Strings, key: FmtKey, args: &Args<'_>) -> Text;
    /// 人和 agent 写的内容、路径、命令、快捷键：原样显示，不翻译（ADR 0013）。
    /// 只接受 `Arc<str>` 和 `String`（领域快照里的形式），不接受 `&str`：
    /// 字面量要写成 `Text::verbatim(Arc::from("…"))`，审阅时一眼可见。
    pub fn verbatim(content: impl Into<Verbatim>) -> Text;
    pub fn as_str(&self) -> &str;
    pub fn shared(&self) -> SharedString;
    pub fn message_id(&self) -> Option<&'static str>;
}

/// `From<Arc<str>>`、`From<String>`，没有 `From<&str>`。
pub struct Verbatim(SharedString);

/// 当前界面语言的字符串表。`init` 设为全局；渲染时读。
pub fn strings(cx: &App) -> Strings;

/// 切换界面语言：换全局的 `Strings`，`cx.refresh_windows()`。Welcome 的语言选择和设置页调用；
/// 写入系统或 Lectern 的设置由调用方经 `platform` / 设置模块完成，本函数只管界面。
pub fn set_locale(locale: Locale, cx: &mut App);
```

### 字体与度量

```rust
/// 系统 UI 字体。GPUI 把这个特殊名字解析为 Core Text 的 `.AppleSystemUIFont`（SF Pro，中文回退苹方），
/// 不按用户可见的字体名查找（ADR 0017）。
pub const UI_FAMILY: &str = ".SystemUIFont";
/// 系统等宽字体的家族名。Core Text 按家族名匹配到 `.AppleSystemUIFontMonospaced-*`（SF Mono）；
/// GPUI 的 Apple 文字系统正是经家族名匹配加载 `.SystemUIFont` 的，这条路径对点开头的系统家族同样有效（已在 macOS 上验证）。
pub const MONO_FAMILY: &str = ".AppleSystemUIFontMonospaced";
/// `MONO_FAMILY` 解析失败时的后备；用它时记一条 warn（「日志与诊断」）。
pub const MONO_FALLBACK_FAMILY: &str = "Menlo";

/// 字号层级（交互规范「视觉」）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextRole {
    /// 40pt，项目主页的大标题
    LargeTitle,
    /// 26pt
    Title,
    /// 22pt
    Subtitle,
    /// 13pt，正文
    Body,
    /// 13pt，`secondary_label`
    Secondary,
    /// 11pt，`secondary_label`
    Caption,
    /// SF Mono 12pt
    Code,
}

impl TextRole {
    pub fn size(self) -> Pixels;
    pub fn line_height(self) -> Pixels;
    pub fn font(self) -> Font;
}

/// 给 `Styled` 元素套一个字号层级：字体、字号、行高、颜色。
pub trait TextRoleExt: Styled + Sized {
    fn text_role(self, role: TextRole, window: &Window, cx: &App) -> Self;
}

/// 等宽字体的度量，`file_reader` 的自绘列表和 `review_view` 的 diff 行按它排格子和行高。
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MonoMetrics {
    /// 一个格子的宽度：`TextSystem::em_advance` 在 `TextRole::Code` 的字号下的值（SF Mono 等宽，全部 ASCII 相同）。
    pub cell_width: Pixels,
    /// 行高：`ascent + descent + line_gap` 向上取到 0.5pt。
    pub line_height: Pixels,
    pub ascent: Pixels,
    pub descent: Pixels,
    /// 这份度量对应的字体：解析成功时为 `MONO_FAMILY`，否则为 `MONO_FALLBACK_FAMILY`。
    pub family: &'static str,
}

/// `TextRole::Code` 的度量。第一次调用时经 `window.text_system()` 解析字体并计算（主线程，几十微秒，
/// 字体由系统映射、不读 Lectern 的文件），之后返回缓存的值。缓存按字号一项，最多 4 项（Code 与将来的放大档位），不增长。
pub fn mono_metrics(window: &Window, cx: &mut App) -> MonoMetrics;
```

东亚宽字符占两格、制表符的展开和显示行的折行规则不在这里：它们在 `syntax` 的 `cells` 模块（[file_reader 设计](file_reader.md)），只取本函数的 `cell_width` 做乘数。

### 代码配色

```rust
/// 一个 `TokenKind` 的样式。颜色都来自 `Palette`，不写死。
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CodeStyle {
    pub color: Hsla,
    pub weight: FontWeight,
    pub italic: bool,
    pub underline: bool,
}

/// `kind` 在当前外观下的样式。渲染时按 token 调用：一次下标。
pub fn code_style(kind: TokenKind, palette: &Palette) -> CodeStyle;

/// 整张表，供快照测试和探针。
pub fn code_styles(palette: &Palette) -> [(TokenKind, CodeStyle); 18];
```

映射（两种外观相同，颜色随 `Palette` 变）：

| `TokenKind` | 颜色 | 字重 / 样式 |
|---|---|---|
| `Keyword`、`Tag` | `system(Purple)` | — |
| `String` | `system(Red)` | — |
| `Number`、`Constant` | `system(Blue)` | — |
| `Type`、`Attribute` | `system(Pink)`（原定 `system(Indigo)`，见「与设计的差异」） | — |
| `Comment` | `secondary_label` | — |
| `Link` | `system(Blue)` | 下划线 |
| `Heading` | `label` | 半粗（`FontWeight::SEMIBOLD`） |
| `Emphasis` | `label` | 斜体 |
| `Function`、`Property`、`Variable`、`Operator`、`Punctuation`、`Escape`、`Other` | `label` | — |

只用系统色，所以 ADR 0015「agent 标识色是唯一的自定义颜色」仍然成立；只上五种颜色，与「安静」的目标一致。色觉辨识用「测试」里的 `code_colors_*` 检查：五种颜色两两之间在正常、红色盲、绿色盲三种模拟下的 ΔE（CIE76）不小于 12，每种与两套 `control_background` 的对比度不低于 3:1。实现时表里的某一行过不了检查就换另一种系统色，并在「与设计的差异」里写明。

### 动效

```rust
pub mod motion {
    /// 一段过渡的曲线。
    #[derive(Clone, Copy, PartialEq, Debug)]
    pub enum Easing {
        /// CSS 的 cubic-bezier(x1, y1, x2, y2)。GPUI 没有贝塞尔缓动，本 crate 自带求值（牛顿迭代，≤ 8 次，纯函数）。
        CubicBezier([f32; 4]),
        /// 交互规范里写作 ease-out 的项
        EaseOut,
        Linear,
        /// 弹簧（进入会话）。`Transition` 对它用 GPUI 的 `SpringConfig::step` 逐帧推进、保留速度，
        /// 重新设目标时不重开；`Motion::duration` 是 `settle_time`，只用来判断结束和减弱动态效果时的淡入淡出时长。
        Spring(SpringConfig),
    }

    impl Easing {
        /// 进度 0..=1 → 值；贝塞尔允许越过 1（开关的回弹）。`Spring` 用 GPUI 的 `sampled_easing`（从静止起步的采样）。
        pub fn eval(self, t: f32) -> f32;
        /// 转成 `gpui::Animation::with_easing` 接受的闭包。
        pub fn into_gpui(self) -> impl Fn(f32) -> f32 + 'static;
    }

    /// 一个具名的过渡：交互规范「动效」表里的一行。
    #[derive(Clone, Copy, PartialEq, Debug)]
    pub struct Motion {
        pub duration: Duration,
        pub delay: Duration,
        pub easing: Easing,
        /// 这个过渡是否移动元素。减弱动态效果时，移动的过渡改为淡入淡出、时长减半；不移动的只把时长减半。
        pub moves: bool,
    }

    pub const STANDARD: Easing = Easing::CubicBezier([0.32, 0.72, 0.0, 1.0]);

    pub const PANEL_OPEN: Motion;     // 380 ms · STANDARD · moves
    pub const PANEL_CLOSE: Motion;    // 280 ms · STANDARD · moves
    pub const CHECK_FILL: Motion;     // 180 ms · STANDARD
    pub const CHECK_STROKE: Motion;   // 220 ms · STANDARD · 延迟 60 ms
    pub const PROGRESS: Motion;       // 360 ms · STANDARD
    pub const SCROLL_NEXT: Motion;    // 300 ms · EaseOut · moves（滚动本身由列表做，这里只给时长与曲线）
    pub const EXPAND_HEIGHT: Motion;  // 280 ms · STANDARD · moves
    pub const EXPAND_FADE: Motion;    // 200 ms · STANDARD
    pub const SEGMENT: Motion;        // 260 ms · STANDARD · moves
    pub const SWITCH: Motion;         // 240 ms · CubicBezier([0.34, 1.3, 0.64, 1.0]) · moves
    pub const ARRIVE: Motion;         // 240 ms · EaseOut · moves（新轮次从下方 8pt 淡入）
    pub const ARRIVE_OFFSET: Pixels;  // 8pt
    pub const HOVER_IN: Motion;       // 120 ms · Linear
    pub const HOVER_OUT: Motion;      // 200 ms · Linear

    /// 进入会话：spring · response 0.35 · damping 0.86。换算（质量 1）：
    /// stiffness = (2π / 0.35)² ≈ 322.3，damping = 2 × 0.86 × √stiffness ≈ 30.9。
    pub const ENTER_SESSION_SPRING: SpringConfig;
    pub const ENTER_SESSION: Motion;  // Easing::Spring(ENTER_SESSION_SPRING) · moves · duration = settle_time（≈ 0.6 s）

    /// 系统的减弱动态效果是否开启（`platform::appearance::reduce_motion`，变化时由通知更新）。
    pub fn reduced(cx: &App) -> bool;

    impl Motion {
        /// 考虑减弱动态效果后实际使用的过渡：`reduced` 时时长减半，`moves` 的改为 `Fade`。
        pub fn effective(self, cx: &App) -> Effective;
        /// 一次性的 GPUI 动画（`with_animation` 用），已按 `effective` 调整。只用于不需要打断的短动画（勾选、悬停、到达）。
        pub fn animation(self, cx: &App) -> Animation;
    }

    #[derive(Clone, Copy, PartialEq, Debug)]
    pub enum Effective {
        /// 照原样播放
        Move(Motion),
        /// 位移改为淡入淡出：调用方把偏移置 0，只对透明度应用这段过渡
        Fade(Motion),
    }

    /// 可打断的标量过渡：面板偏移、进度、展开高度、进入会话的标题位移。视图把它放在自己的状态里。
    /// 重新设目标时从当前值和当前进度继续（交互规范「随时可打断」），不排队、不跳回起点；弹簧还保留速度。
    /// 进行中的个数记在一个进程级计数里（`go` 开始时加一，`tick` 到达、`set` 和 drop 时减一），供 `active_transitions`。
    #[derive(Debug)]
    pub struct Transition {
        /* from, to, start: Instant, motion: Motion, 当前值, 弹簧的速度, 是否计入计数 */
    }

    impl Transition {
        pub fn new(value: f32) -> Transition;
        /// 设新目标。`now` 由调用方从 `Instant::now()` 传入，测试里可控。
        /// 正在进行时取当前值作为新的起点，重新计时；目标相同则不做任何事。
        pub fn go(&mut self, target: f32, motion: Motion, now: Instant, cx: &App);
        /// 立即到达，不过渡。
        pub fn set(&mut self, value: f32);
        /// 当前值。渲染时调用；还在进行时同时 `window.request_animation_frame()`。纯计算，不分配。
        pub fn tick(&mut self, now: Instant, window: &mut Window) -> f32;
        pub fn value(&self) -> f32;
        pub fn target(&self) -> f32;
        pub fn is_running(&self, now: Instant) -> bool;
    }

    /// 进行中的 `Transition` 个数（含弹簧）。`workspace` 的帧记录用它判断动画都已停下（`RecordStop::AnimationsSettled`）。
    /// 只数 `Transition`；`Motion::animation` 建的一次性 GPUI 动画（勾选、悬停、到达）不计，它们最长 240 ms，
    /// 调用方在计数归零后再等 `ARRIVE.duration` 即可。计数在 `tick` 里减，所以持有者在过渡结束前就不再渲染（也不 drop）时，
    /// 这个过渡一直计着；bench 的场景持续渲染到停止条件满足，不会碰到（已知限制）。
    pub fn active_transitions(cx: &App) -> usize;
}
```

规则（视图照做，`ui` 的控件也照做）：

- 动画只改位移、透明度和裁剪（`Transition` 的值只用在 `translate`、`opacity`、`overflow_hidden` 的高度上），不改布局属性。`bench` 的帧时间检查（ADR 0015「影响」）以此为前提。
- 时长只能来自 `motion` 的常量。验收用 `rg` 检查视图 crate 里没有 `Duration::from_millis`、`from_secs_f32`。
- 减弱动态效果：GPUI 自己的 `App::reduce_motion` 让 `with_animation` 直接画终态，与交互规范「位移改为淡入淡出，时长减半」不同。`ui` 不调用 `set_reduce_motion`，GPUI 的标志保持 false；`Motion::animation` 和 `Transition` 自己按 `reduced` 调整。gpui-component 内部的少数动画（如 `Sheet` 的 150 ms 淡入）不在本 crate 的控制内，第一批屏幕不用这些组件。

### 控件

只列第一批屏幕需要、而 gpui-component 没有或与 macOS 不一致的。每个控件都是 `RenderOnce` 元素（按 [gpui 技能](../../.claude/skills/gpui/SKILL.md)的指南选择），接受 `Text`，不接受 `&str`；都自带探针的角色、文字和状态（见「探针」），视图只给 `NodeId`。

```rust
/// 按钮：macOS 的推按钮高度 24pt（`Small`）/ 30pt（`Regular`），主按钮用 `accent`。包装 gpui-component 的 `Button`，
/// 把它的变体、圆角和颜色换成系统的；视图不直接用 gpui-component 的 `Button`。
pub struct Button { /* id, text, kind, size, icon, disabled, on_click */ }
pub enum ButtonKind { Primary, Secondary, Plain }
pub enum ButtonSize { Small, Regular }
impl Button {
    pub fn new(id: NodeId, text: Text) -> Button;
    pub fn kind(self, kind: ButtonKind) -> Button;
    pub fn size(self, size: ButtonSize) -> Button;
    pub fn icon(self, icon: Icon) -> Button;
    pub fn disabled(self, disabled: bool) -> Button;
    pub fn on_click(self, f: impl Fn(&mut Window, &mut App) + 'static) -> Button;
}

/// 第一批屏幕用到的图标，来自 gpui-component 自带的 Lucide 集合（见「未决问题」1）。只列需要的，不暴露整个 `IconName`。
pub enum Icon { Folder, Clone, ChevronRight, ChevronDown, Check, Close, Search, Inspector, Back, Forward }

/// 这 10 个图标的 SVG 资源来源（gpui-kit-assets 的 `icon_assets!` 宏生成，只嵌入这 10 个文件）。
/// `app` 在 `Application::with_assets` 里注册它；GPUI 只认这一处注册的来源。
#[derive(Clone, Copy, Default)]
pub struct IconAssets;

/// agent 标识：8pt 圆点。`blink()` 让它轻闪一次（`ARRIVE` 的时长，透明度 1 → 0.3 → 1）。
pub struct AgentDot { /* agent, blink: Option<ElementId> */ }
pub enum Agent { ClaudeCode, Codex }
impl AgentDot {
    pub fn new(agent: Agent) -> AgentDot;
    pub fn blink(self, id: ElementId) -> AgentDot;
}

/// 圆形勾选（文件头的「已看」）：圆圈填色 `CHECK_FILL`，对勾描出 `CHECK_STROKE`。
/// 勾选后文件名变次要色由调用方做（它拥有那段文字）。键盘（V）和鼠标触发同一个元素、同一段动画。
pub struct CircleCheck { /* id, checked, on_change */ }
impl CircleCheck {
    pub fn new(id: NodeId, checked: bool) -> CircleCheck;
    pub fn on_change(self, f: impl Fn(bool, &mut Window, &mut App) + 'static) -> CircleCheck;
}

/// 已看进度：`Bar`（宽度增长，`PROGRESS`）或 `Ring`（Overview 行尾的进度环，同一曲线）。
/// 值由调用方的 `Transition` 给出，控件本身不保存状态。
pub struct Progress { /* id, shape, fraction: f32 */ }
pub enum ProgressShape { Bar, Ring }
impl Progress {
    pub fn new(id: NodeId, shape: ProgressShape, fraction: f32) -> Progress;
}

/// 分段控件（检查器的 Files / Conversation，Usage 的 7D / 14D / 30D）：滑块平移到新选项，`SEGMENT`。
/// 滑块位置是调用方状态里的一个 `Transition`，所以切换到一半再切回去时从当前位置返回。
pub struct Segmented { /* id, items: Vec<(NodeId, Text)>, selected: usize, slider: f32, on_select */ }
impl Segmented {
    pub fn new(id: NodeId, items: Vec<(NodeId, Text)>, selected: usize) -> Segmented;
    /// 滑块的当前位置（0..items.len()-1 之间的小数），由调用方的 `Transition::tick` 给出。
    pub fn slider(self, position: f32) -> Segmented;
    pub fn on_select(self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Segmented;
}

/// 分组列表：圆角 12pt，`control_background` 底，行间 0.5pt `separator`（交互规范「视觉」的「分组」）。
pub struct GroupedList { /* id, header: Option<Text>, rows */ }
impl GroupedList {
    pub fn new(id: NodeId) -> GroupedList;
    pub fn header(self, text: Text) -> GroupedList;
    pub fn row(self, row: Row) -> GroupedList;
}

/// 列表行：悬停背景 `HOVER_IN` / `HOVER_OUT`，选中用 `selected_content_background`。
/// 内容由调用方给（agent 圆点、两行文字、进度环都是它的子元素）。
pub struct Row { /* id, selected, on_click, children */ }
impl Row {
    pub fn new(id: NodeId) -> Row;
    pub fn selected(self, selected: bool) -> Row;
    pub fn on_click(self, f: impl Fn(&mut Window, &mut App) + 'static) -> Row;
    pub fn child(self, child: impl IntoElement) -> Row;
}

/// 加载态的阶段（交互规范「状态与异常」的「加载」行）。纯函数，视图按它决定画什么。
pub enum LoadingPhase { Nothing, Skeleton, Progress }
pub fn loading_phase(elapsed: Duration) -> LoadingPhase;   // < 100 ms → Nothing，< 1 s → Skeleton，否则 Progress

/// 骨架块：包装 gpui-component 的 `Skeleton`，颜色换成 `tertiary_label` 的淡版，无闪烁动画（减弱动态效果时也一样）。
pub struct Skeleton { /* width, height */ }

/// 展开原语（file_reader 前置条件 E 的「展开原语」；评论框、查找栏）：在原处撑开，`EXPAND_HEIGHT` 的高度 + `EXPAND_FADE` 的透明度，
/// 下方内容随高度平滑下移。高度和透明度各是调用方状态里的一个 `Transition`；减弱动态效果时只有透明度变化、高度直接到位。
pub struct Expander { /* id, height: f32, opacity: f32, child */ }
impl Expander {
    pub fn new(id: NodeId, height: Pixels, opacity: f32, child: impl IntoElement) -> Expander;
}
```

```rust
/// 一次性提示（`workspace` 的 `workspace-command-failed` 等）：内容区顶部居中的一行次要文字，`control_background` 底、圆角、阴影，
/// 用 `ARRIVE` 从上方 8pt 淡入，`duration` 后用 `HOVER_OUT` 淡出并由调用方 drop；再次触发时替换文字、重新计时。不可交互，不加遮罩，不排队。
pub struct Hint { /* id, text, shown_at: Instant, duration */ }
impl Hint {
    /// `duration` 默认 2.5 s。
    pub fn new(id: NodeId, text: Text, shown_at: Instant) -> Hint;
    pub fn duration(self, duration: Duration) -> Hint;
    /// 调用方每帧问一次：到期后返回 true，调用方把它从状态里去掉。
    pub fn is_expired(&self, now: Instant) -> bool;
}
```

面板的滑入（`PANEL_OPEN` / `PANEL_CLOSE`）没有控件：`workspace` 的面板容器持有一个 `Transition`（0 = 收起，1 = 展开），把 `(1 - value) × 面板宽度` 用作 `translate_x`；减弱动态效果时 `Effective::Fade`，偏移为 0、透明度取 value。

### 贡献载荷（`contrib`）

`workspace` 的注册表（`services::workspace`）在 kernel 层，装不下 GPUI 的类型和 Fluent 键，所以每种贡献的具体载荷放在本 crate 的 `contrib` 模块，`workspace` 从 `Payload` 里 `downcast` 回来。字段、调用线程和生命周期以 [workspace 设计](workspace.md)「`ui::contrib` 的载荷类型」为准，本文只列名字和本 crate 决定的两点：

```rust
pub mod contrib {
    pub type ViewFactory<Arg> = Arc<dyn Fn(&Arg, &mut Window, &mut App) -> AnyView + Send + Sync>;
    pub struct Screen { pub build: ViewFactory<Target>, pub toolbar: Arc<dyn Fn(&mut App) -> ToolbarItems + Send + Sync> }
    pub struct ToolbarItems { pub title: Text, pub subtitle: Option<Text>, pub controls: Vec<AnyElement> /* ≤ 3 */ }
    pub struct SidebarPanel { pub build: ViewFactory<()> }
    /// 侧栏的一个条目（`services::workspace::SidebarEntry` 的载荷）。图标用本 crate 的 `Icon`（见「未决问题」1），不是 SF Symbols 的名字。
    pub struct SidebarItem { pub label: Key, pub icon: Option<Icon>, pub on_select: WorkspaceCommand }
    pub struct InspectorTab { pub label: Key, pub build: ViewFactory<()> }
    pub struct Panel { pub build: ViewFactory<()> }
    pub struct Overlay { pub build: ViewFactory<()> }
    pub struct KeyBindings(pub Vec<(&'static str, Box<dyn gpui::Action>, Option<&'static str>)>);

    /// 每个类型都有：装进 `services::workspace::Payload`。`workspace` 用 `Payload::downcast::<Screen>()` 一类取回。
    impl Screen { pub fn into_payload(self) -> Payload; }
}
```

- 本 crate 不调用这些闭包，不登记、不注销：只是类型。`Text`、`Key`、`Icon` 是本 crate 的类型，所以 `label` 不能是字面量。
- `SidebarItem` 原名 `SidebarEntry`，与 `services::workspace::SidebarEntry`（注册表的条目）重名，改为 `SidebarItem`；workspace 设计的改名和 `icon` 类型的对应修改由 `workspace` 的下一个文档 PR 做。

### 探针

语义节点的类型在本 crate 的 `semantic` 模块，`ui_probe`（tool 层）依赖本 crate 读它们；本 crate 不依赖 `ui_probe`。形状按 `ui_probe` 设计的契约（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）：探针不读 GPUI 的 AccessKit 树（锁定的 gpui-pre 0.3.9 只在辅助技术激活时构造它，`#[gpui::test]` 拿不到），所以本 crate 自己记录。

```rust
pub mod semantic {
    /// 稳定 ID 的构造：`segment('.' segment)*`，segment 是 `name` 或 `name[key]`，name 是 `[a-z0-9-]+`，
    /// key 是任意 UTF-8（不含换行、不含 `].`）。`NodeId::new("review", "files", Some("src/main.rs"))` →
    /// `review.files.item[src/main.rs]`（技术调研「UI 结构探针」）。业务键是路径、会话 ID，不是下标；同一帧内唯一。
    /// 调试构建里校验格式，发布构建不校验。
    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    pub struct NodeId(SharedString);
    impl NodeId {
        pub fn new(view: &'static str, area: &'static str, key: Option<&str>) -> NodeId;
        /// 子节点：`parent.child("edit")` → `….edit`；`parent.item(key)` → `….item[key]`。
        pub fn child(&self, name: &'static str) -> NodeId;
        pub fn item(&self, key: &str) -> NodeId;
        pub fn as_str(&self) -> &str;
    }

    /// 角色：探针和快照里的类型名，与 `ui_probe` 设计的表一致。
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Role {
        Window, Sidebar, Toolbar, Panel, Group, List, ListItem, Tree, TreeItem, TabBar, Tab, Button, Toggle,
        Text, TextField, SearchField, Link, Menu, MenuItem, Dialog, Diff, DiffHunk, DiffLine, Comment, Badge, Progress,
    }

    /// 节点显示的文字和它的来源：`key` 是 Fluent 消息 ID（来自 `Text::message_id`），原样显示的文本为 None。
    #[derive(Clone, PartialEq, Debug)]
    pub struct Label { pub key: Option<&'static str>, pub text: SharedString }

    /// 探针要看的事实，不放样式。
    #[derive(Clone, Default, PartialEq, Debug)]
    pub struct State {
        pub selected: bool,
        pub focused: bool,
        pub disabled: bool,
        pub expanded: Option<bool>,
        pub checked: Option<bool>,
    }

    /// 一个节点的声明。`attrs` 是视图自己的键值（如 `("tier", "B")`、`("rows", "120")`），每个节点最多 8 个。
    #[derive(Clone, PartialEq, Debug)]
    pub struct Semantic {
        pub id: NodeId,
        pub role: Role,
        pub label: Option<Label>,
        pub state: State,
        pub attrs: Vec<(&'static str, SharedString)>,
    }

    /// 记录器收到的一条：声明、这一帧的屏幕范围、是否在内容遮罩内可见、父节点在同一帧记录里的下标、声明处的源码位置。
    #[derive(Clone, Debug)]
    pub struct Record {
        pub semantic: Semantic,
        pub bounds: Bounds<Pixels>,
        pub visible: bool,
        pub parent: Option<usize>,
        pub declared_at: &'static core::panic::Location<'static>,
    }

    /// 包装元素：给任何元素附上语义节点。`#[track_caller]`，所以 `declared_at` 是视图里的调用处（技术调研：只记录业务视图层）。
    /// prepaint 里取子元素的 `bounds`，按 `Window::content_mask` 算 `visible`；记录器打开时推一条 `Record`，并在子元素 prepaint 期间
    /// 把自己压进线程本地的父节点栈。记录器关闭时只读一次 `AtomicBool`（Relaxed）就返回，不分配、不记录。
    #[track_caller]
    pub fn semantic(spec: Semantic, child: impl IntoElement) -> SemanticElement;
    /// 同上，另外把同一份声明写进子元素的 GPUI 无障碍属性：`accessibility_id`、映射的 `accesskit::Role`、`aria_label`、
    /// `aria_selected`、`aria_expanded`、`aria_toggled`。子元素要是 `StatefulInteractiveElement`（带 id 的 div 等，GPUI 的无障碍属性只在它上面），所以是单独的入口；角色映射表在本模块。
    #[track_caller]
    pub fn semantic_interactive<E: StatefulInteractiveElement + IntoElement>(spec: Semantic, child: E) -> SemanticElement;
    pub struct SemanticElement { /* spec, child, declared_at */ }

    /// 进程一个，只在主线程上用。`ui_probe` 和 `#[gpui::test]` 在一帧之前 `begin`，帧画完 `finish` 取走这一帧的记录。
    /// 记录按声明顺序，`parent` 指向同一个 `Vec` 里的下标。
    pub struct Recorder;
    impl Recorder {
        pub fn begin(window: WindowId);
        pub fn finish() -> Vec<Record>;
        pub fn is_armed() -> bool;
    }
}
```

本 crate 的控件自带角色、`Label`（来自它们的 `Text`）和状态，视图只给 `NodeId`；视图自己的容器与自绘元素用 `semantic(...)` 包装。`Semantic` 是现成构造、不是闭包：包装元素自己的代价只有关闭时的一次原子读，构造 `Semantic` 的代价（`NodeId` 的字符串、`Label` 的 `SharedString` 克隆、`attrs` 默认空 `Vec` 不分配）归视图，由帧基准覆盖；行数多的视图把 `NodeId` 放在自己的状态里（随快照建一次），每帧只克隆。每个视图 crate 的设计文档「界面」一节列出自己的节点（角色与 ID）；纯装饰元素不声明。窗口根节点由探针合成，视图不声明。

## 数据结构

- `Palette`：每个外观一份，`init` 时建两份，`platform` 通知时整份替换，每份 27 个 `Hsla`（432 字节），放在一个 GPUI 全局 `UiState` 里，连同 `Strings`、减弱动态效果标志、`accent_follows_system`、`MonoMetrics` 的缓存（≤ 4 项）和 `platform::appearance::Watch`。`UiState` 固定大小，不随运行增长。
- 内置的系统色表：`src/palette/builtin.rs`，两个外观各 27 项，值是 `platform::appearance::system_colors` 在 macOS 27.0.1 上读到的 sRGB 分量（HIG「Color」页面只发布 13 个系统色、没有语义色，见「与设计的差异」），文件头写明来源与读取日期。用途：`platform` 返回 None 时（Linux 的 `cargo check`、headless、测试、系统接口失败），以及测试的参照。`init_fixed` 只用这张表。`diff_added_background` / `diff_removed_background`：`system(Green)` / `system(Red)` 在浅色下 12% 透明度、深色下 18% 透明度叠在 `control_background` 上，预先算好。
- agent 标识色：浅 `#D9790A` / 深 `#C77A1A`（Claude Code），浅 `#5856D6` / 深 `#6260D8`（Codex），来自交互规范「视觉」；不从系统读。
- `Transition`：`from`、`to`、`start`、`motion`、当前值、弹簧的速度、`reduced` 快照（开始时的值，过渡中途不换）、是否计入计数，共 56 字节，由视图持有，不可 `Clone`（drop 时要减计数）。
- 贝塞尔求值：`Easing::eval` 对 `CubicBezier` 用牛顿迭代求 x(t)=进度 的 t，再取 y(t)；不建查表，每次调用常数时间（≤ 8 次迭代）。
- 记录器（`semantic::Recorder`）：进程一个，一个 `AtomicBool` 开关加一个 `Vec<Record>` 和线程本地的父节点栈。关闭时 `Vec` 为空、不分配；`begin` 到 `finish` 之间按这一帧的节点数增长，`finish` 把它整个取走。发布构建里编译，但开关永远不打开（只有 `ui_probe` 和测试调用 `begin`），每个包装元素每帧的代价是一次原子读。

## 数据流与线程

- 系统 → `ui`：`init` 在主线程上经 `platform::appearance::reduce_motion()` 读一次、`watch` 登记一次，`Palette` 先用内置的表；`ready`（首帧之后）在主线程上经 `system_colors(Light)`、`system_colors(Dark)` 读一次，换 `Palette`。`watch` 的回调在主线程上到达（AppKit 的通知中心在主线程投递），不带 GPUI 的上下文，经 `init` 交给它的 `AsyncApp` 把更新排到主线程的下一次运行；更新里重新读两套系统色、换 `Palette`、`cx.refresh_windows()`。外观（浅 / 深）变化经 GPUI 的 `observe_window_appearance`，同样在主线程。
- `ui` → 视图：视图在 `render` 里读 `colors`、`strings`、`mono_metrics`、`code_style`、`motion` 的常量，只读不写（架构规则 4）。视图持有自己的 `Transition`，每帧 `tick`。
- 所有公开项都在主线程上，没有后台任务、没有 executor 的工作、没有 IO。`platform::appearance` 的三个函数同样是主线程上的内存操作。
- 派生数据只有 `MonoMetrics`（按字号缓存）和两份 `Palette`；没有以 blob OID 为键的东西（架构规则 5 不涉及）。

## 性能

影响[性能预算](tech-research.md#性能预算)的项：

| 项 | 影响 | 约束 |
|---|---|---|
| 冷启动到首帧 < 20 ms | `init` 在首帧前运行 | ≤ 300 µs：一次 `NSWorkspace` 读取、两份内置表的换算（54 个颜色）、设全局、登记通知。系统色的读取（约 70 次 `NSColor` 读取与换算，本进程第一次约 2 ms）在 `ready`，首帧之后。骨架窗口的 `attach` 在 `open_window` 的回调里，只登记一个观察者，不在这项基准里。不建线程、不读文件、不解析任何东西。`gpui_component::init`（运行时解析两份内嵌 JSON）放在 `ready`，首帧之后 |
| 空闲内存（空窗口）< 60 MB | `UiState` | < 8 KiB；`gpui_component::init` 建的主题全局与图标资源按 `ui/idle_memory` 实测，预期 < 1 MB，作为 `ui` 的份额写进 bench 的预算表 |
| 滚动与输入 120 fps | 每帧的查找与动画 | `colors`、`code_style`、`Transition::tick` 都是 O(1)、不分配；动画只改位移、透明度、裁剪（ADR 0015）。动画期间帧的工作时间 ≤ 8.3 ms 由 `bench` 的 `app/animation_frames` 在 `workspace` 接入后判定（[bench 设计](bench.md)的 `frames` 应答） |
| 滚动与输入 120 fps（探针关闭时） | 每个 `semantic` 包装元素的 prepaint | 一次 `AtomicBool` 的 Relaxed 读，≤ 2 ns（`ui/semantic_disarmed`）；不分配 |
| 打开文件到首屏高亮 < 16 ms | `mono_metrics` 的第一次计算 | 第一次调用 ≤ 200 µs（一次字体解析、三次度量），之后是缓存；`file_reader` 可以在首屏之前的任一帧先调一次 |

首帧之前做的事：`init`（上表）。其余都在 `ready` 之后。

新增基准（`bench`，macOS 判定，Linux 不编译）：`ui/init`（≤ 300 µs）、`ui/ready`（≤ 3 ms）、`ui/palette_resolve`（通知到达到新 `Palette` 就位 ≤ 300 µs）、`ui/transition_tick`（≤ 100 ns）、`ui/bezier_eval`（≤ 50 ns）、`ui/mono_metrics_first`（≤ 200 µs）、`ui/semantic_disarmed`（≤ 2 ns）、`ui/idle_memory`（`ready` 之后 `UiState` 加 gpui-component 全局 < 1 MB）。

## 界面

本 crate 没有自己的屏幕；它服务于交互规范的全部屏幕和三层。

- 用到的控件与动效原语见「接口」；本 crate 自己不定义任何颜色值、字体名和时长之外的东西：颜色来自 `platform` 或内置表，时长来自交互规范的表。
- Fluent 键：没有。本 crate 不显示自己的文字；`Text` 只是容器。
- 探针节点：控件自带角色（`Button` → `Role::Button`，`CircleCheck` → `Role::Toggle`（`checked`），`Progress` → `Role::Progress`（`attrs` 里 `fraction`），`Segmented` → `Role::TabBar` 与每项 `Role::Tab`（`selected`），`GroupedList` → `Role::List`，`Row` → `Role::ListItem`（`selected`），`AgentDot` → `Role::Badge`（`attrs` 里 `agent`），`Expander` → `Role::Group`（`expanded`），`Hint` → `Role::Text`），`Label` 来自它们的 `Text`，`disabled` 来自 `Button::disabled`；ID 由视图给。
- Action 与快捷键：没有。控件只回调，Action 在视图里定义（技术调研：探针只经 Action 操作）。

## 状态与异常

| 情况 | 表现 |
|---|---|
| `platform` 读不到系统色（接口失败、非 macOS、headless） | 用内置表，`accent_follows_system` 为 false，记一条 warn；界面照常 |
| 读不到减弱动态效果 | 视为关闭，不记日志 |
| `MONO_FAMILY` 解析失败 | `mono_metrics` 改用 `MONO_FALLBACK_FAMILY`（Menlo），`family` 字段写明，记一条 warn；行高和格宽按 Menlo 算，界面照常 |
| 两种字体都解析失败 | 改用 `UI_FAMILY` 算度量（`family` 为 `UI_FAMILY`），记 error。「解析失败」的判断：`TextSystem::resolve_font` 对目标家族返回的 `FontId` 与对 GPUI 内置后备家族返回的相同，GPUI 没有直接的失败信号 |
| 外观在一帧中途变化 | 本帧用旧 `Palette`，下一帧用新的；`observe_window_appearance` 的回调已由 GPUI 延后到帧之间 |
| `ready` 之前有视图调用控件 | panic：包装的 gpui-component 控件读 `Theme::global`，它只由 `gpui_component::init` 装上。本 crate 的控件先 `assert!(is_ready(cx))`，让 panic 带着明确的消息；这是 `app` 的装配错误，不在运行时处理 |
| 减弱动态效果在过渡中途开启 | 进行中的 `Transition` 按开始时的方式做完；下一次 `go` 按新设置 |
| `NodeId` 格式不对 | 调试构建 panic；发布构建原样使用 |

## 日志与诊断

本 crate 经 `lectern-logging` 的宏记录（[ADR 0046](../adr/0046.md)、[logging 设计](logging.md)），target 为 `ui`。全部记录在主线程上发出，只进内存缓冲。

- 默认记录：

  | 事件 | 级别 | `msg` | 字段 |
  |---|---|---|---|
  | `init` 完成 | info | `ui initialised` | `appearance`（`light` / `dark`）、`accent`（总是 `builtin`：系统色在 `ready` 才读）、`reduce_motion`（bool）、`elapsed_us` |
  | 系统色读不到，用内置表 | warn | `system colours are unavailable; the built-in table is used` | `appearance`（`platform` 的 `system_colors` 返回 `Option`，没有错误种类可记） |
  | 系统等宽字体解析失败 | warn | `the system monospaced font is unavailable; the fallback family is used` | `family`（`MONO_FALLBACK_FAMILY` 或 `UI_FAMILY`，都是 `'static`） |
  | 等宽字体与后备都失败 | error | `no monospaced font resolved; the UI font is used for code` | — |
  | `ready` 完成 | info | `ui ready` | `accent`（`system` / `builtin`）、`elapsed_us` |

  本次运行里每种 warn 只记一次（`AtomicBool`）。
- 详细级别（debug）：外观变化（`appearance`）、强调色变化（`accent`）、减弱动态效果变化（`reduce_motion`）、语言切换（`locale` 为 `Locale::tag()`）。每次事件一条，事件本身由用户或系统触发，频率低。不在 `colors`、`code_style`、`Transition::tick`、`mono_metrics` 的热路径上加记录。
- 关联 ID：本 crate 不设作用域（logging 设计「对其他 crate 的要求」第 11 项：主线程上的作用域由 `workspace` 和视图层设置），记录带调用线程上已有的作用域。
- 不记录：`Text` 的内容（界面文案、路径、会话标题、prompt 都经它显示）、`NodeId` 的业务键（路径、会话 ID）、`Semantic::attrs` 的值、`Label` 的文字、颜色的数值、字体的实际 PostScript 名（只记上面三个 `'static` 家族常量的其中一个）、窗口标题。用事件名、外观、`'static` 的家族常量和计数代替。
- 金丝雀测试：`tests::logs_hold_no_text`（`lectern_logging::testing::capture()`，trace 级别，`init_fixed`）：用含金丝雀的 `Text::verbatim` 渲染 `Button`、`Row`、`GroupedList`，用含金丝雀的业务键建 `NodeId`，用含金丝雀的 `attrs` 和 `Label` 调 `semantic(...)`，`Recorder::begin` 后渲染一帧，再切一次语言、触发一次模拟的外观变化；`assert_absent` 确认金丝雀不进日志，并检查记录只有 `ui initialised`、`ui ready`（`init_fixed` 也运行 `ready`）和两条 debug。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate。`just test -p lectern-ui`。

- 单元测试（不需要窗口）：
  - `bezier_*`：`Easing::eval` 对 `STANDARD` 和 `SWITCH` 的曲线与 CSS 参考值（WebKit 的 `UnitBezier`，预先算好 11 个采样点放在测试里）误差 ≤ 1e-3；`t=0` 为 0、`t=1` 为 1；`SWITCH` 在 0.6 附近越过 1。
  - `spring_*`：`ENTER_SESSION_SPRING` 的 `canonical()` 给出阻尼比 0.86 ± 0.005、自然频率 2π/0.35 ± 0.01；`Transition` 用 `ENTER_SESSION` 从 0 到 1，在 100 ms 时 `go(0, …)`，紧接的一帧里值仍朝 1 走（速度保留）然后折返，没有跳变。
  - `transition_*`：从 0 到 1 的 380 ms 过渡，在 190 ms 时 `go(0, …)`，之后的值从当时的值单调回到 0，没有跳变；`go` 到相同目标不重新计时；`set` 立即到位；减弱动态效果下 `moves` 的过渡时长减半且 `effective` 为 `Fade`。
  - `loading_phase_*`：99 ms → `Nothing`，100 ms → `Skeleton`，1 s → `Progress`。
  - `active_transitions_*`：两个 `Transition` 开始后为 2，一个到达后为 1，另一个 drop 后为 0；`set` 中断进行中的过渡也减一。
  - `hint_*`：`is_expired` 在 `duration` 到期后为 true；再次 `new` 同一个 ID 从新的 `shown_at` 计时。
  - `node_id_*`：格式、转义、`child`；调试构建下非法的 `view` panic。
  - `code_colors_*`：对内置表的两个外观，五种颜色两两之间在正常、红色盲（protanopia）、绿色盲（deuteranopia）三种模拟（Viénot 1999 的 3×3 矩阵，在测试里写出）下的 ΔE（CIE76）≥ 12；每种与 `control_background` 的对比度（WCAG 2 的公式）≥ 3:1。`code_styles` 的两个外观各一份 insta 快照。
  - `palette_builtin_*`：内置表每个颜色的透明度在 0..=1、`diff_*_background` 的叠加结果与手算一致；两个外观的 `label` 与 `window_background` 对比度 ≥ 7:1。
  - `text_*`：`Text::verbatim` 接受 `Arc<str>` 和 `String`；`compile_fail` 文档测试确认 `Text::verbatim("literal")`、`Text::key(&strings, "literal")` 和 `contrib::ToolbarItems { title: "literal", … }` 不编译（`just doctest`）；`Button::new(id, "literal")` 的那条随第 3 步的 `Button` 加入。
- `#[gpui::test]`（`TestAppContext`，`init_fixed`）：
  - `appearance_change_swaps_palette`：模拟窗口外观从浅到深（经 `testing::simulate_appearance_change`，见「与设计的差异」），`colors` 返回另一套，gpui-component 的主题跟着切到深色，窗口被刷新一次。
  - `set_locale_refreshes_windows`：切到 zh-CN 后 `strings(cx).locale()` 为 `ZhCn`，所有窗口刷新。
  - `ready_runs_component_init_once`：`ready` 两次只初始化一次（第二次不重置 gpui-component 的主题）；`is_ready` 之前控件在调试构建下 panic（控件的这部分随第 3 步）。
  - `mono_metrics_is_cached`：两次调用返回同一份值，第二次不再解析字体（计数的测试钩子）。
  - `circle_check_toggles_from_key_and_click`：按键和点击都触发 `on_change`，都启动同一个动画元素。
  - `segmented_slider_continues_from_current_position`：切换中途再切回，滑块的 `Transition` 从当前位置返回。
  - `reduced_motion_has_no_displacement`：`init_fixed(…, reduced_motion: true)` 下，`Expander`、`Segmented` 的滑块和面板偏移（用 `Transition` + `Effective::Fade` 的示例）在整个过渡里位移为 0，只有透明度变化。ADR 0015「影响」里「开启减弱动态效果时没有位移动画」的检查项。
  - `semantic_records_bounds_and_parents`：`Recorder::begin` 后渲染一个 `GroupedList` 里两个 `Row` 的帧，`finish` 给出 3 条记录，`Row` 的 `parent` 指向 `GroupedList`，`bounds` 与元素一致，滚出内容遮罩的行 `visible` 为 false；没有 `begin` 时渲染不产生记录，`is_armed` 为 false。
  - 每个控件的语义节点各一份 insta 快照（`Recorder::finish` 的记录去掉 `bounds` 和 `declared_at` 后的 JSON）。
- fixtures：没有外部格式。内置系统色表是源码里的常量，不是夹具。
- 只能在 macOS 上跑的部分：全部（依赖 GPUI）。`platform::appearance` 的真实读取在 `platform` 的测试里验证；本 crate 的测试全用 `init_fixed`。另有一个手动检查在「验收」里：发布构建下 `mono_metrics().family` 为 `MONO_FAMILY`。

## 对其他 crate 的要求

- `platform`（前置条件 A，在 [platform 设计](platform.md)里增加「系统外观」一节后实现，单独一个 PR）：

  ```rust
  pub mod appearance {
      #[derive(Clone, Copy, PartialEq, Eq, Debug)]
      pub enum Appearance { Light, Dark }

      /// sRGB 分量 0..=1 加透明度。不依赖 GPUI。
      #[derive(Clone, Copy, PartialEq, Debug)]
      pub struct Srgba { pub r: f32, pub g: f32, pub b: f32, pub a: f32 }

      /// `ui::Palette` 里来自系统的那些颜色，字段名与之一一对应（不含 diff 底色和 agent 色）。
      pub struct SystemColors { /* label, secondary_label, …, accent, …, system: [Srgba; 13] */ }

      /// 在给定外观下解析 `NSColor` 的语义色（`NSAppearance` 的 `performAsCurrentDrawingAppearance`
      /// 加 `colorUsingType(.componentBased)`，取 sRGB 分量）。主线程，不阻塞，不做 IO；每次调用约 50 次 ObjC 消息。
      /// 非 macOS 或解析失败时为 None；`accent_available` 指出 `controlAccentColor` 是否读到（macOS 10.14+ 都有）。
      pub fn system_colors(appearance: Appearance) -> Option<SystemColors>;

      /// `NSWorkspace.shared.accessibilityDisplayShouldReduceMotion`。主线程。非 macOS 为 None。
      pub fn reduce_motion() -> Option<bool>;

      #[derive(Clone, Copy, PartialEq, Eq, Debug)]
      pub enum Change { SystemColors, ReduceMotion }

      /// 登记 `NSSystemColorsDidChangeNotification`（强调色、提高对比度）和
      /// `NSWorkspace.accessibilityDisplayOptionsDidChangeNotification`。回调在主线程上到达；
      /// 返回的 `Watch` drop 时注销，之后不再回调。非 macOS 返回一个什么也不做的 `Watch`。
      pub fn watch(on_change: Box<dyn FnMut(Change)>) -> Watch;
      pub struct Watch { /* observer tokens */ }
  }
  ```

  浅色 / 深色本身不经 `platform`：GPUI 已经给出窗口外观和变化通知。
- `app`：
  - `Application` 建好后、`run` 之前 `with_assets(lectern_ui::IconAssets)`：GPUI 的资源来源只能在这里装（`Application::with_assets`），`gpui_component::init` 不装；没有它，所有 `Icon` 画成空白。`IconAssets` 由本 crate 用 gpui-kit-assets 的 `icon_assets!` 宏只嵌入「控件」里列的 10 个图标（每个 SVG 不到 1 KiB），不嵌入整套 Lucide（ADR 0018）。
  - `run` 里在开窗口之前调用 `lectern_ui::init(cx, strings)`（在 `set_menus` 之前，菜单文字可以继续直接用传入的 `Strings`）。
  - `open_window` 的回调里对骨架窗口调用 `lectern_ui::attach(window, cx)`，`Subscription` 放在 `app` 的根视图里。
  - `startup::release` 在 `Kernel::start` 之前调用 `lectern_ui::ready(cx)`；test profile 用 `init_fixed`。`app/cold_start` 的预算不变。
- `workspace`：每个窗口打开后调用 `lectern_ui::attach`；面板容器用 `Transition` 和 `PANEL_OPEN` / `PANEL_CLOSE`；贡献的载荷用 `contrib` 的类型，`RecordStop::AnimationsSettled` 用 `motion::active_transitions`，一次性提示用 `Hint`（[workspace 设计](workspace.md)「对其他 crate 的要求」B 项的四样）；SF Symbols 不提供，图标用 `Icon`（「未决问题」1）；侧栏、工具栏、检查器、面板、浮条各声明一个节点（`workspace.sidebar` 等），Action 名稳定、可枚举。
- `welcome`、`project_home`、`review_view`、`file_reader`：颜色、字体、动效、控件只从本 crate 取（「验收」里的 `rg` 检查）；可交互元素都经 `NodeId` 命名，容器和自绘元素用 `semantic(...)` 包装，各自的设计文档「界面」列出节点。
- `ui_probe`：依赖 `lectern-ui` 的 `semantic` 模块，按帧 `Recorder::begin` / `finish` 取记录，自己合成窗口根节点（`window[<id>]`）；按 Action 名触发操作。它的设计与本节的形状一致（[#6](https://github.com/SoloJiang/lectern-app/issues/6)），两边改动时一起改；两份设计里形状不同的地方以本文为准（`Semantic::id` 是 `NodeId`，`label` 是 `Option<Label>`：纯容器没有文字）。
- `i18n`：`Key::name(&self) -> &'static str`、`FmtKey::name(&self) -> &'static str`，返回 Fluent 消息 ID（`Text::message_id` 与 `Label::key` 用），随第 1 步在 `i18n` 里加上。
- `bench`：「性能」里的 `ui/*` 基准；`app/animation_frames` 随 `workspace` 的里程碑一起加入。

## 未决问题

1. **图标的来源**。GPUI 画不了 SF Symbols（`NSImage`），要用它们得经 `platform` 栅格化成位图再交给 GPUI，每个尺寸和外观各一份。选项：(a) 第一批屏幕用 gpui-component 自带的 Lucide 图标，只暴露「控件」里列的 10 个；(b) 现在就做 SF Symbols 的栅格化。默认 (a)：图标很少、都在按钮和行尾，Lucide 的线宽与 SF Pro 搭配得过去；视觉验收时如果人觉得不像原生，再按 (b) 另开 issue，接口（`Icon` 枚举）不变。
2. **代码字号是否跟随系统的文字大小设置**。macOS 没有全局的字号设置，只有辅助功能里的缩放；Xcode 自己有字号偏好。选项：(a) 固定 SF Mono 12pt（交互规范）；(b) 加一个设置项，`TextRole::Code` 可选 11 / 12 / 13 / 14。默认 (a)；`mono_metrics` 的缓存已经按字号留了 4 项，将来加 (b) 不改接口。

强调色跟随系统还是固定：ADR 0015 已经决定跟随系统，本文照此实现，不再列为未决。

## 验收

分四步，每步一个 PR；前置条件 A 在 `platform` 单独一个 PR；第 1 步的 `contrib` 还要 `services` 的 `workspace` 模块（[workspace 设计](workspace.md)「对其他 crate 的要求」A，[#221](https://github.com/SoloJiang/lectern-app/pull/221)）先合并。每步都要在 macOS 上 `just check`、`just test -p lectern-ui`、`just doctest`、`just deny` 通过，并且 `rg 'std::fs|std::thread::spawn|std::net' crates/ui/src` 没有结果。

**前置条件 A（`platform`，`Refs #214`）**：`platform::appearance` 的三个函数与 `Watch`；[platform 设计](platform.md)增加「系统外观」一节。`just test -p lectern-platform` 里 `appearance_*` 通过：两个外观各读到一份 `SystemColors`、`label` 的明度在浅色下低于 0.5、深色下高于 0.5；`reduce_motion` 返回 Some；`watch` 在 drop 后不再回调（测试里经 `NSNotificationCenter` 手动投递一次通知）。

**第 1 步：初始化、颜色、文本、字体与代码配色（`Refs #214`）。**

- `init`、`ready`、`is_ready`、`init_fixed`、`attach`、`Palette`、`colors`、`palette`、`Text`、`strings`、`set_locale`、`TextRole`、`MonoMetrics`、`mono_metrics`、`code_style`、`code_styles` 与「数据结构」的内置表；`contrib` 模块（`workspace` 的实现要用，所以放在第 1 步；`Icon` 枚举随它一起定义，图标资源在第 3 步）。
- 「测试」里 `palette_builtin_*`、`code_colors_*`、`text_*`、`appearance_change_swaps_palette`、`set_locale_refreshes_windows`、`ready_runs_component_init_once`、`mono_metrics_is_cached`、`logs_hold_no_text` 通过；`just doctest` 里两个 `compile_fail` 通过。
- `app` 接入 `with_assets(IconAssets)`、`init`、骨架窗口的 `attach` 与 `ready`（可以在同一个 PR 或紧接的 `app` PR）：`just test -p lectern-app` 通过，其中 `#[gpui::test]` `appearance_change_refreshes_the_skeleton_window`（模拟外观变化后窗口刷新一次）和 `appearance_change_before_ready_does_not_touch_the_theme`（直接调 `init`、`open_window`、`attach`，不经 `init_fixed` 和 `startup::release`；`ready` 之前触发一次外观变化：不 panic、`Palette` 已换、gpui-component 的主题没有被读写）；`just bench ui/`（`ui/init`、`ui/ready`、`ui/palette_resolve`、`ui/mono_metrics_first`、`ui/idle_memory`）在预算内；`app/cold_start` 不变慢。
- 手动（macOS，发布构建）：系统设置里切换强调色，窗口里 `accent` 随之变化、不重启；切换浅色 / 深色同样；日志里 `ui ready` 的 `accent` 为 `system`（`ui initialised` 的总是 `builtin`，见「与设计的差异」）、`mono_metrics().family` 为 `.AppleSystemUIFontMonospaced`（经 `--log ui=debug` 的调试输出或探针读出）。
- `rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}' crates/ui/src --glob '!**/builtin.rs' --glob '!**/tests*'` 没有结果（颜色值只在内置表和 agent 色常量里，后者放在 `builtin.rs`）。

**第 2 步：动效原语（`Refs #214`）。**

- `motion` 模块的全部常量与类型，含 `active_transitions`。
- `bezier_*`、`spring_*`、`transition_*`、`active_transitions_*`、`reduced_motion_has_no_displacement` 通过；`just bench ui/`（`ui/transition_tick`、`ui/bezier_eval`）在预算内。
- `rg 'set_reduce_motion' crates/ui/src crates/app/src` 没有结果。

**第 3 步：控件（`Refs #214`）。**

- `Button`、`IconAssets`、`AgentDot`、`CircleCheck`、`Progress`、`Segmented`、`GroupedList`、`Row`、`loading_phase`、`Skeleton`、`Expander`、`Hint`。
- `loading_phase_*`、`hint_*`、`circle_check_toggles_from_key_and_click`、`segmented_slider_continues_from_current_position`、每个控件的语义节点快照通过；`icon_assets_hold_every_icon`：`IconAssets` 对 `Icon` 的每个成员都能给出非空的 SVG。
- `rg 'Duration::from_millis|Duration::from_secs' crates/ui/src --glob '!**/motion*' --glob '!**/tests*'` 没有结果（时长只在 `motion` 里定义）。

**第 4 步：探针节点（`Refs #214`；[#6](https://github.com/SoloJiang/lectern-app/issues/6) 的 `ui_probe` 设计合并后；最后完成的那个 PR 写 `Closes #214`）。**

- `semantic` 模块：`NodeId`、`Role`、`Label`、`State`、`Semantic`、`Record`、`semantic(...)`、`Recorder`、无障碍属性的映射；控件的角色、`Label` 与状态。
- `node_id_*`、`semantic_records_bounds_and_parents`、控件的语义节点快照通过；`just bench ui/` 的 `ui/semantic_disarmed` 在预算内。
- `rg 'Recorder::begin' crates --glob '!crates/ui_probe/**' --glob '!**/tests*' --glob '!**/benches/**'` 没有结果（只有探针和测试打开记录器）。

**视图 crate 接入后的持续检查**（写进各视图设计的验收，这里列出口径）：`rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|\.font_family\(|gpui_component::Button' crates/{workspace,welcome,project_home,review_view,file_reader}/src` 没有结果。

## 实际验证

第 1 步（macOS，Apple 芯片，macOS 27.0.1）：

- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过（`ui` 在视图层，依赖 `i18n`、`platform`、`services`、`syntax`、`logging`）。
- `just test -p lectern-ui`：23 个测试通过，其中「测试」里第 1 步的 `palette_builtin_*`（4 个）、`code_colors_*`（3 个）、`code_styles_*` 的两份 insta 快照、`text_*`（2 个）、`appearance_change_swaps_palette`、`set_locale_refreshes_windows`、`ready_runs_component_init_once`、`mono_metrics_is_cached`、`logs_hold_no_text`；另有 `attach_registers_once_per_window`、`init_then_ready_reads_the_system`（唯一读真实 AppKit 的测试：`init` 后 `is_ready` 为 false，`ready` 后为 true 且 `accent_follows_system` 为 true）、字体解析三种结果的 `mono_*`、`text_roles_have_the_spec_sizes`、`contrib_payloads_round_trip_and_are_send`。`just test -p lectern-i18n` 的 32 个通过，新增 `key_names_are_the_fluent_message_ids`。
- `just doctest`：`lectern-ui` 的 3 个 `compile_fail` 通过。
- `rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}' crates/ui/src --glob '!**/builtin.rs' --glob '!**/tests*'` 和 `rg 'std::fs|std::thread::spawn|std::net' crates/ui/src` 都没有结果。
- `git diff origin/main -- Cargo.lock | rg '^\+name = '` 没有结果：没有新的外部 crate。
- 没有做：`app` 的接入（`with_assets`、`init`、`attach`、`ready` 和两个 `#[gpui::test]`）、`ui/*` 基准、手动的强调色与外观切换检查，都在下一个 PR；`mono_metrics().family` 在发布构建下的手动检查随 `app` 接入一起做。

第 2 步（macOS，Apple 芯片，macOS 27.0.1）：

- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过（`bench` 对 `lectern-ui` 和 `gpui` 的依赖只在 macOS 声明，见「与设计的差异」）。
- `just test -p lectern-ui`：38 个测试通过，其中本步新增 15 个：`bezier_matches_the_css_reference`、`bezier_ends_at_0_and_1`、`bezier_switch_overshoots_near_0_6`、`spring_constants_match_the_spec`、`spring_keeps_its_velocity_when_retargeted`、`spec_constants_hold_the_table`、`transition_continues_from_the_current_value`、`transition_same_target_does_not_restart`、`transition_set_arrives_at_once`、`transition_delay_holds_the_start_value`、`transition_reduced_motion_halves_and_fades`、`transition_not_reduced_moves`、`active_transitions_counts_the_ones_in_progress`、`reduced_motion_has_no_displacement`、`panel_moves_when_motion_is_not_reduced`。`cargo nextest run -p lectern-bench`：78 个通过（新增 `runner_macos_only_budgets_are_skipped_off_macos`）。
- `just doctest`：`lectern-ui` 的 3 个 `compile_fail` 仍通过。
- `just bench --measure ui/`：`ui/transition_tick` 中位数 43 ns（预算 ≤ 100 ns；`ENTER_SESSION` 弹簧的 tick 40 ns，记在结果的 note 里），`ui/bezier_eval` 中位数 8 ns（预算 ≤ 50 ns）。
- `rg 'set_reduce_motion' crates/ui/src crates/app/src`、`rg 'Duration::from_millis|Duration::from_secs' crates/ui/src --glob '!**/motion*' --glob '!**/tests*'`、`rg 'std::fs|std::thread::spawn|std::net' crates/ui/src` 都没有结果。
- `git diff origin/main -- Cargo.lock | rg '^\+name = '` 没有结果：没有新的外部 crate。

## 与设计的差异

第 1 步：

- `init` 不读系统色。[platform 设计「系统外观」](platform.md#系统外观)实测本进程第一次 `system_colors` 约 2 ms（AppKit 第一次解析自己的动态颜色），超出 `ui/init` 的 300 µs；已决定推迟到首帧之后：`init` 只读 `reduce_motion`、登记 `watch`，`Palette` 从内置表建；`ready` 对两个外观各读一次并换掉 `Palette`。「初始化」「数据流与线程」「性能」「日志与诊断」已照改：`ui initialised` 的 `accent` 总是 `builtin`，`ui ready` 多一个 `accent` 字段，验收的手动检查看 `ui ready`。
- 内置表的值取自本机 `platform::appearance::system_colors` 在 macOS 27.0.1 上的读数（`builtin.rs` 文件头写明日期），不是 HIG「Color」页面的值：HIG 只发布 13 个系统色，没有 `labelColor` 等语义色的数值，而且它列的是较早 macOS 的值，与当前系统差几个单位；取同一来源，内置表与实时读数一致。
- `Type`、`Attribute` 改用 `system(Pink)`：按「代码配色」的检查，`Indigo` 与 `Blue` 在深色下的绿色盲模拟里 ΔE 只有 2.7，浅色下与 `Purple` 在红色盲模拟里 11.1；13 个系统色里只有 `Pink` 让五种颜色在两个外观、三种模拟下两两 ΔE ≥ 12（最小的一对是浅色红色盲模拟下的 `Pink` 与 `secondary_label`，14.3）且对比度 ≥ 3:1。`Red` 与 `Pink` 的 ΔE 在 15 到 17 之间，刚过门槛，视觉验收时如果字符串与类型分不开，再考虑把 `Number` 的颜色让出来。
- `init_fixed` 的 `appearance` 是固定外观：`colors(window, cx)` 用它而不是窗口的外观。gpui-pre 0.3.9 的测试平台不能从 GPUI 之外改窗口的外观（`TestAppContext::test_window` 是 `pub(crate)`），所以外观变化的测试经 `testing::simulate_appearance_change(appearance, cx)`（`test-support` feature）直接调 `attach` 的观察者所调的那个处理函数，它在固定模式下同时改固定外观；`testing::simulate_platform_change(change, cx)` 同样直接调 `watch` 回调所排的处理函数。`app` 的 `appearance_change_refreshes_the_skeleton_window` 和 `appearance_change_before_ready_does_not_touch_the_theme` 也要走这两个钩子。
- `MonoMetrics::line_height` 是 `ascent + descent` 向上取到 0.5pt，不含 `line_gap`：GPUI 的 `TextSystem` 不按 `FontId` 公开 `line_gap`（只在私有的 `FontMetrics` 上），SF Mono 的 `line_gap` 为 0。`descent` 字段取绝对值（GPUI 照字体给出负值）。
- 「解析失败」的判断按原文（目标家族的 `FontId` 与 GPUI 内置后备的相同），后备的 `FontId` 用一个不存在的家族名探出；如果探针家族与 `UI_FAMILY` 的 `FontId` 也相同（GPUI 的测试文字系统给所有家族同一个 id），视为无法探测、按 `MONO_FAMILY` 解析成功，不记 warn。
- `attach` 的「重复调用只登记一次」按 `AnyWindowHandle` 记在 `UiState` 里，每次 `attach` 先去掉已关闭的窗口；重复调用返回一个空的 `Subscription`。
- gpui-component 主题的投影只写 `Palette` 有直接来源的字段（背景、前景、边框、主色、次要色、列表与选中、侧栏、弹层、链接、骨架、滚动条、danger / success / warning / info 等 40 个）；强调色上的文字用 `builtin.rs` 里的 `on_accent()`（AppKit 的 `alternateSelectedControlTextColor`，两个外观都是白色），不是 `Palette` 的字段。其余字段保持 gpui-component 注册主题的值，第 3 步包装控件时再补。
- `i18n` 的 `Key::name` / `FmtKey::name`（`Text::message_id` 用）由 [#227](https://github.com/SoloJiang/lectern-app/pull/227) 提供，不在本 PR 里。

第 2 步：

- `ENTER_SESSION.duration` 是 `ENTER_SESSION_SPRING` 从静止起步、`SPRING_EPSILON`（0.001，GPUI 的默认值，它没有公开）下的 `settle_time`，实测 491 ms，不是原文估的 0.6 s。`settle_time` 不是 const fn，所以常量写死，`spring_constants_match_the_spec` 核对两者相差 ≤ 1 ms。
- 弹簧的结束由 `SpringConfig::is_settled` 判断；`duration` 的两倍是中途改目标后仍未静止时的后备上限。减弱动态效果下弹簧按定时的淡入淡出播放：时长减半、曲线用 `sampled_easing`。
- `Easing::EaseOut` 取 CSS 的 `ease-out`，即 `cubic-bezier(0, 0, 0.58, 1)`；原文只写「ease-out」。
- `Motion::effective` 在减弱动态效果下只把 `duration` 减半，`delay` 不变（原文没有写 `delay`）。`Fade` 里的 `Motion` 的 `moves` 为 false。
- GPUI 的 `Animation` 没有延迟，`Motion::animation` 把延迟折进去：动画总长 `delay + duration`，曲线在延迟结束前保持 0。
- `Transition::tick` 自己的工作是常数时间、不分配；它调用的 `Window::request_animation_frame` 在 GPUI 里装箱一个回调（一次分配），这是 GPUI 的代价，不在本 crate 控制内。`ui/transition_tick` 测的是含这次请求的 `tick`：`tick` 只能在视图的 render 里调用（GPUI 只允许正在渲染的视图请求下一帧），所以基准用 GPUI 的测试平台开一个窗口，取「一次 render 里 4096 次 tick」与「不 tick 的 render」之差除以 4096 作为一个样本，用 `Ctx::compare` 交替采样。
- `Transition` 按字段算约 112 字节，不是 56：它存了两个 `Instant`（当前段的起点和上次采样的时刻，弹簧按后者逐步推进）和整份 `Motion`（56 字节）。由视图持有，个数以个位数计，不影响内存预算。
- 进行中的计数是进程级的 `AtomicUsize`；nextest 一个测试一个进程，`active_transitions_*` 以测试开始时的值为基准断言差值。
- `reduced_motion_has_no_displacement` 本步只覆盖设计里的面板偏移示例（`Transition` + `Effective::Fade`），`Expander` 和 `Segmented` 的滑块随第 3 步加入；另加 `panel_moves_when_motion_is_not_reduced` 作为对照。
- `bench` 对 `lectern-ui` 和 `gpui`（`test-support`）的 dev 依赖只在 macOS 声明（`[target.'cfg(target_os = "macos")'.dev-dependencies]`），Linux 上 `bench` 仍不依赖 GPUI、仍在 `just check` / `just test` 的范围内。为此 `scripts/check_architecture.py` 第 4 条改为：Apple 专属的 target 谓词只对外部的、非 GPUI 的依赖算作 macOS 绑定（工作区成员由它自己的清单检查，GPUI 只在 macOS 上编译是构建约束，不是绑定）。预算表加 `Budget::macos_only`：Linux 上 `just bench` 把这些条目记为 `skipped`（只在 macOS 运行），不算缺基准（[bench 设计「预算表」](bench.md#预算表)已补）。

