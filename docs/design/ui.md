# ui 设计

状态：已接受。第 1 步（初始化、颜色、文本、字体与代码配色，`contrib`）已实现（[#230](https://github.com/SoloJiang/lectern-app/pull/230)），`app` 的接入（`init`、骨架窗口的 `attach`、`ready`）已实现（[#234](https://github.com/SoloJiang/lectern-app/pull/234)），`with_assets(IconAssets)` 已随 `workspace` 的 `app` 装配加上（见 [app 设计「workspace 的装配」](app.md#workspace-的装配)），第 1 步的 `ui/*` 基准仍在下一个 `app` PR；第 2 步（动效原语 `motion`，基准 `ui/transition_tick`、`ui/bezier_eval`）已实现（[#233](https://github.com/SoloJiang/lectern-app/pull/233)）；第 3 步（控件与 `semantic` 的节点类型）已实现（[#236](https://github.com/SoloJiang/lectern-app/pull/236)）；第 4 步（探针的记录器与包装元素、控件的语义节点快照，基准 `ui/semantic_disarmed`）已实现（[#242](https://github.com/SoloJiang/lectern-app/pull/242)）。`Palette` 的词级差异底色（`review_view` 前置条件 F）已实现（[#238](https://github.com/SoloJiang/lectern-app/pull/238)）。控件与设计画布对齐（[#283](https://github.com/SoloJiang/lectern-app/issues/283) 的控件 PR）已实现：`TextRole` 改为 HIG 文本样式的字号，`GroupedList` / `Row` 的卡片边线、行距、分隔线缩进与安静的选中样式，图标按钮 `Button::icon_only`，进度环 20pt / 2.5pt；窗口里的视觉检查还没有做（见「实际验证」）。第 5 步（「控件」的「第二批控件」，#283 的三个屏幕要用）尚未实现。

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
| `gpui`、`gpui-component` | 元素、窗口外观、文字系统、动画元素；gpui-component 的 `Button`、`Icon` 作为底层（`Skeleton`、`ProgressCircle` 自带不可关的动画，不用，见「与设计的差异」第 3 步） | 已锁定（[ADR 0016](../adr/0016.md)） |
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
    // 派生：同样的两个来源再深一档，供行内的词级差异叠在上面（`review_view` 前置条件 F）
    pub diff_added_inline_background: Hsla,
    pub diff_removed_inline_background: Hsla,
    // 派生：`label` 的 RGB 加固定的不透明度（「第二批控件」）
    pub fill: Hsla,
    pub fill_strong: Hsla,
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

/// 字号层级（交互规范「视觉」），按 macOS HIG 的文本样式：Large Title 26、Title 1 22、Title 2 17、Title 3 15、
/// Headline 13 semibold、Body 13、Callout 12、Subheadline 11（`Caption`）、Footnote 10。
/// 比 `Body` 小的几档和 `Secondary` 用 `secondary_label`。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TextRole {
    /// 26pt 粗体（bold），行高 32：项目主页的大标题
    LargeTitle,
    /// 22pt semibold，行高 26（HIG Title 1）
    Title1,
    /// 17pt semibold，行高 22（HIG Title 2）
    Title2,
    /// 15pt semibold，行高 20（HIG Title 3）
    Title3,
    /// 13pt semibold，行高 16：行标题、分组标题、工具栏标题
    Headline,
    /// 13pt，行高 16：正文
    Body,
    /// 13pt，`secondary_label`
    Secondary,
    /// 12pt，行高 15，`secondary_label`：行标题下面的几行
    Callout,
    /// 11pt，行高 14，`secondary_label`（HIG 的 Subheadline）
    Caption,
    /// 10pt，行高 13，`secondary_label`
    Footnote,
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
/// 按钮：macOS 的推按钮高度 24pt（`Small`）/ 30pt（`Regular`），主按钮用 `accent`；工具栏的图标按钮 28pt（`Toolbar`）。包装 gpui-component 的 `Button`，
/// 把它的变体、圆角和颜色换成系统的；视图不直接用 gpui-component 的 `Button`。
pub struct Button { /* id, text, kind, size, icon, disabled, on_click */ }
pub enum ButtonKind { Primary, Secondary, Plain }
pub enum ButtonSize { Small, Regular, Toolbar }
impl Button {
    pub fn new(id: NodeId, text: Text) -> Button;
    pub fn kind(self, kind: ButtonKind) -> Button;
    pub fn size(self, size: ButtonSize) -> Button;
    pub fn icon(self, icon: Icon) -> Button;
    /// 只画图标的方形按钮（`Toolbar` 大小、`Plain`）：工具栏的 Back 与 Inspector。`label` 不画出来，是辅助名和探针节点的文字。
    pub fn icon_only(id: NodeId, icon: Icon, label: Text) -> Button;
    /// 把按钮变成开关并给出开关状态（检查器是否打开）：探针节点是带 `checked` 的 `Toggle`，开着时图标按钮的图标用强调色。
    pub fn active(self, active: bool) -> Button;
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
/// 给了 `node` 才声明 `Badge` 节点；没给的是装饰（行里紧挨着标题的圆点）。
pub struct AgentDot { /* agent, blink: Option<ElementId>, node: Option<NodeId> */ }
pub enum Agent { ClaudeCode, Codex }
impl AgentDot {
    pub fn new(agent: Agent) -> AgentDot;
    pub fn blink(self, id: ElementId) -> AgentDot;
    pub fn node(self, id: NodeId) -> AgentDot;
}

/// 圆形勾选（文件头的「已看」）：圆圈填色 `CHECK_FILL`，对勾描出 `CHECK_STROKE`。
/// 勾选后文件名变次要色由调用方做（它拥有那段文字）。键盘（V）和鼠标触发同一个元素、同一段动画。
pub struct CircleCheck { /* id, checked, on_change */ }
impl CircleCheck {
    pub fn new(id: NodeId, checked: bool) -> CircleCheck;
    pub fn on_change(self, f: impl Fn(bool, &mut Window, &mut App) + 'static) -> CircleCheck;
}

/// 已看进度：`Bar`（宽度增长，`PROGRESS`）或 `Ring`（Overview 行尾的进度环，同一曲线；20pt，线宽 2.5pt）。
/// 值由调用方的 `Transition` 给出，控件本身不保存状态。
pub struct Progress { /* id, shape, fraction: f32 */ }
pub enum ProgressShape { Bar, Ring }
impl Progress {
    pub fn new(id: NodeId, shape: ProgressShape, fraction: f32) -> Progress;
}

/// 分段控件（检查器的 Files / Conversation，Usage 的 7D / 14D / 30D）：高 24pt、圆角 8pt、边距 2pt、滑块圆角 6pt、文字 medium；滑块平移到新选项，`SEGMENT`。
/// 滑块位置是调用方状态里的一个 `Transition`，所以切换到一半再切回去时从当前位置返回。
pub struct Segmented { /* id, items: Vec<(NodeId, Text)>, selected: usize, slider: f32, on_select */ }
impl Segmented {
    pub fn new(id: NodeId, items: Vec<(NodeId, Text)>, selected: usize) -> Segmented;
    /// 滑块的当前位置（0..items.len()-1 之间的小数），由调用方的 `Transition::tick` 给出。
    pub fn slider(self, position: f32) -> Segmented;
    pub fn on_select(self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Segmented;
    /// 调用方把滑块的 `Transition` 交给它：`SEGMENT` 从当前位置过渡；减弱动态效果时 `set` 直接到位，
    /// 控件在新选项上把滑块淡入（`SEGMENT` 的减半时长），没有位移。
    pub fn select(slider: &mut Transition, index: usize, now: Instant, cx: &App);
}

/// 分组列表：圆角 12pt 的卡片，`control_background` 底，外圈 0.5pt `separator`（画在行上方、不占位置，所以页面底色和卡片同色时也有边），
/// 行间 0.5pt `separator`，从文字起点开始（`separator_inset`，默认 16pt = 行的左边距；行首有 10pt 圆点和 14pt 间距的行传 40pt）。
/// 标题 13pt semibold（`Headline`）、`secondary_label`，缩进 12pt，与卡片相距 8pt。
pub struct GroupedList { /* id, header: Option<Text>, separator_inset, rows */ }
impl GroupedList {
    pub fn new(id: NodeId) -> GroupedList;
    pub fn header(self, text: Text) -> GroupedList;
    pub fn separator_inset(self, inset: Pixels) -> GroupedList;
    pub fn row(self, row: Row) -> GroupedList;
}

/// 列表行：悬停背景 `HOVER_IN` / `HOVER_OUT`。内容由调用方给（agent 圆点、几行文字、进度环都是它的子元素），
/// 文字默认 `Body`；行标题要 semibold 的调用方自己用 `TextRole::Headline`（画板里只有会话卡片的标题是 600，
/// 其余行是常规字重，所以不由行统一设置）。
/// 在 `GroupedList` 里是卡片行：左右 16pt、上下 14pt，子元素间距 14pt。
/// 在 `GroupedList` 之外是紧凑行（检查器的文件列表）：高 30pt、左右 6pt、间距 8pt、圆角 6pt。
/// 选中默认是安静的：`unemphasized_selected_content_background`（卡片里随卡片裁成方的，紧凑行圆角 6pt），文字仍是 `label`。
/// `accent()` 改用 `selected_content_background` 与 `on_accent`：只给 macOS 标准里用强调色的获得焦点的源列表（Welcome 的两个列表）。
pub struct Row { /* id, selected, accent, in_card, on_click, children */ }
impl Row {
    pub fn new(id: NodeId) -> Row;
    pub fn selected(self, selected: bool) -> Row;
    pub fn accent(self) -> Row;
    pub fn on_click(self, f: impl Fn(&mut Window, &mut App) + 'static) -> Row;
    pub fn child(self, child: impl IntoElement) -> Row;
}

/// 加载态的阶段（交互规范「状态与异常」的「加载」行）。纯函数，视图按它决定画什么。
pub enum LoadingPhase { Nothing, Skeleton, Progress }
pub fn loading_phase(elapsed: Duration) -> LoadingPhase;   // < 100 ms → Nothing，< 1 s → Skeleton，否则 Progress

/// 骨架块：`tertiary_label` 的淡版，圆角，无闪烁动画（减弱动态效果时也一样）。自己画，不包装 gpui-component 的
/// `Skeleton`（它的脉动关不掉，见「与设计的差异」第 3 步）。装饰元素，不声明节点。
pub struct Skeleton { /* width, height */ }
impl Skeleton { pub fn new(width: Pixels, height: Pixels) -> Skeleton; }

/// 展开原语（file_reader 前置条件 E 的「展开原语」；评论框、查找栏）：在原处撑开，`EXPAND_HEIGHT` 的高度 + `EXPAND_FADE` 的透明度，
/// 下方内容随高度平滑下移。高度和透明度各是调用方状态里的一个 `Transition`；减弱动态效果时只有透明度变化、高度直接到位。
pub struct Expander { /* id, height: f32, opacity: f32, child */ }
impl Expander {
    pub fn new(id: NodeId, height: Pixels, opacity: f32, child: impl IntoElement) -> Expander;
    /// 调用方把两个 `Transition` 交给它：高度走 `EXPAND_HEIGHT`、透明度走 `EXPAND_FADE`（`target` 为 0 时收起）；
    /// 减弱动态效果时高度 `set` 直接到位，只剩透明度的过渡。
    pub fn go(height: &mut Transition, opacity: &mut Transition, target: Pixels, now: Instant, cx: &App);
}
```

```rust
/// 一次性提示（`workspace` 的 `workspace-command-failed` 等）：内容区顶部居中的一行次要文字，`control_background` 底、圆角、阴影，
/// 用 `ARRIVE` 从上方 8pt 淡入，`duration` 后用 `HOVER_OUT` 淡出并由调用方 drop；再次触发时替换文字、重新计时。不可交互，不加遮罩，不排队。
pub struct Hint { /* id, text, shown_at: Instant, duration */ }
impl Hint {
    /// `duration` 默认 `motion::HINT_DURATION`（2.5 s）。
    pub fn new(id: NodeId, text: Text, shown_at: Instant) -> Hint;
    pub fn duration(self, duration: Duration) -> Hint;
    /// 调用方每帧问一次：`duration` 过后再过 `HOVER_OUT` 的淡出，返回 true，调用方把它从状态里去掉。
    pub fn is_expired(&self, now: Instant) -> bool;
    /// 某一刻的偏移与透明度（到达、停留、淡出三段），到期为 None。纯函数，`reduced` 为减弱动态效果。
    pub fn frame_when(&self, now: Instant, reduced: bool) -> Option<HintFrame>;
}
```

面板的滑入（`PANEL_OPEN` / `PANEL_CLOSE`）没有控件：`workspace` 的面板容器持有一个 `Transition`（0 = 收起，1 = 展开），把 `(1 - value) × 面板宽度` 用作 `translate_x`；减弱动态效果时 `Effective::Fade`，偏移为 0、透明度取 value。

#### 第二批控件（[#283](https://github.com/SoloJiang/lectern-app/issues/283)）

三个屏幕按设计画布重画要用到的控件（[welcome 设计](welcome.md)「对其他 crate 的要求」D、[project_home 设计](project_home.md)「对其他 crate 的要求」C、[review_view 设计](review_view.md)「对其他 crate 的要求」I）。数值取画布（`Welcome` / `Main` / `Review` 画板），颜色仍是系统语义色或由它们派生（ADR 0015）。工具栏的搜索框不在这一批：`project_home` 在搜索有归属之前不画它。

```rust
impl Palette {
    // 派生（与 diff 底色同一做法）：`label` 的 RGB 加固定的不透明度。画布的 `--fill` / `--fill2`。
    // fill：浅色 4%、深色 5%（操作卡片的底）；fill_strong：浅色 8%、深色 10%（弹出按钮、次要按钮、悬停的操作卡片、分段控件的轨道）。
    pub fill: Hsla,
    pub fill_strong: Hsla,
}

/// 新增 6 个，共 16 个，仍来自 Lucide：Overview → `layout-grid`，Worktrees → `git-branch`，Files → `file`，
/// MergeRequest → `git-pull-request`，Plus → `plus`，Sidebar → `panel-left`。`IconAssets` 只嵌入这 16 个文件。
pub enum Icon { /* 原有 10 个 */ Overview, Worktrees, Files, MergeRequest, Plus, Sidebar }

/// 操作卡片（Welcome 的 Open Folder…）：`fill` 底、圆角 10pt、内边距上下 10pt 左右 12pt、间距 12pt；
/// 左边 20pt 的图标用 `accent`，中间标题（`Headline`）加一行说明（`Callout`、次要色），右边快捷键提示（`Callout`、次要色）。
/// 悬停时底色 `fill` → `fill_strong`（`HOVER_IN` / `HOVER_OUT`）。宽度撑满父元素，最宽由调用方限定。探针节点是 `Button`，`Label` 是标题。
pub struct ActionCard { /* id, icon, title, detail, shortcut: Option<Text>, on_click */ }
impl ActionCard {
    pub fn new(id: NodeId, icon: Icon, title: Text, detail: Text) -> ActionCard;
    pub fn shortcut(self, hint: Text) -> ActionCard;
    pub fn on_click(self, f: impl Fn(&mut Window, &mut App) + 'static) -> ActionCard;
}

/// 菜单的一项：选中时把 `action` 派发到窗口（`Window::dispatch_action`，与菜单栏和快捷键同一条路）。
pub struct MenuItem { pub text: Text, pub action: Box<dyn Action>, pub checked: bool }

/// 弹出按钮（Welcome 的语言选择）：高 24pt、`fill_strong` 底、圆角 6pt、左右 6pt，文字 `Callout`，右边一个 `ChevronDown`。
/// 点击时在按钮下沿弹出系统菜单，`checked` 的项带勾。探针节点是 `Button`，`Label` 由调用方给（`label`），`attrs.value` 由调用方给（`value`）。
pub struct PopUpButton { /* id, label, current: Text, value, items: Vec<MenuItem> */ }
impl PopUpButton {
    /// `current` 是按钮上显示的文字（当前选项）。
    pub fn new(id: NodeId, label: Text, current: Text, items: Vec<MenuItem>) -> PopUpButton;
    pub fn value(self, value: SharedString) -> PopUpButton;
}

/// 右键菜单：在指针处弹出系统菜单。行（`Row`、`SidebarRow`）经 `context_menu` 挂上；视图不直接调用。
pub fn show_menu(items: Vec<MenuItem>, position: Point<Pixels>, window: &mut Window, cx: &mut App);

/// 侧栏样式的平铺行：没有卡片、没有分隔线，圆角 8pt、左右内缩由父元素给。内容由调用方给（文字层级由行内元素给出）。
/// 默认（Welcome 的右栏）：内边距上下 8pt 左右 10pt、子元素间距 2pt；悬停 `fill`；选中 `accent()` 时用 `selected_content_background` 与 `on_accent`，
/// 第二行文字由调用方用 `on_accent` 的 85% 不透明度；不带 `accent()` 时选中是 `fill_strong`。
/// `compact()`（工作区侧栏）：高 28pt、左右 8pt、子元素间距 8pt、圆角 6pt；`trailing`（行尾的数量，次要色）。探针节点同 `Row`：`ListItem` 加 `State::selected`。
pub struct SidebarRow { /* id, selected, accent, compact, trailing: Option<Text>, on_click, menu: Vec<MenuItem>, children */ }
impl SidebarRow {
    pub fn new(id: NodeId) -> SidebarRow;
    pub fn selected(self, selected: bool) -> SidebarRow;
    pub fn accent(self) -> SidebarRow;
    pub fn compact(self) -> SidebarRow;
    pub fn trailing(self, text: Text) -> SidebarRow;
    pub fn on_click(self, f: impl Fn(&mut Window, &mut App) + 'static) -> SidebarRow;
    pub fn context_menu(self, items: Vec<MenuItem>) -> SidebarRow;
    pub fn child(self, child: impl IntoElement) -> SidebarRow;
}
impl Row { pub fn context_menu(self, items: Vec<MenuItem>) -> Row; }

impl Segmented {
    /// 撑满父元素的宽度，各项等分（检查器的 Files / Conversation）。轨道 `fill_strong`，滑块 `control_background`。
    pub fn fill(self) -> Segmented;
}

impl Button {
    /// 胶囊形：圆角为高度的一半，左右 14pt（浮条里的 Looks Good 与 Send）。`Secondary` 的底是 `fill_strong`、无边框；`Primary` 是 `accent`。
    pub fn pill(self) -> Button;
}

/// 浮条容器（审阅的 Looks Good / Send）：圆角 22pt，内边距上下 8pt、左 16pt、右 8pt，子元素间距 12pt，
/// `control_background` 底、0.5pt `separator` 外圈加阴影；画布的背景模糊 GPUI 画不了，所以不透明。不声明节点，内容自己声明。
pub struct Capsule { /* children */ }
impl Capsule { pub fn new() -> Capsule; pub fn child(self, child: impl IntoElement) -> Capsule; }

/// 展开箭头（轮次折叠行、文件折叠行）：`ChevronRight`，`expanded` 为 0 时朝右、1 时朝下，中间按比例旋转。
/// 值是调用方状态里的 `Transition`（`EXPAND_HEIGHT` 的曲线），减弱动态效果时直接到位。装饰元素，不声明节点（行自己声明展开状态）。
pub struct Disclosure { /* expanded: f32 */ }
impl Disclosure { pub fn new(expanded: f32) -> Disclosure; }

/// 开关（审阅工具栏的 Hide formatting，#21 之后才有调用方，实现随它）：32 × 18pt，旋钮 14pt；开时 `accent`、关时 `fill_strong`。
/// 旋钮的位置是调用方的 `Transition`（`SEGMENT`）。探针节点是带 `checked` 的 `Toggle`。
pub struct Switch { /* id, on, knob: f32, on_change */ }
```

- 为什么自己做：gpui-component 的 `Button`、`Select`、`PopupMenu`、`ContextMenu` 都是自己画的菜单，样式与 macOS 不一致，也会被窗口边界裁掉。`PopUpButton` 和 `show_menu` 用 gpui-component 的 `native_menu::NativeMenu`，菜单由系统画（`NSMenu`），与菜单栏同一条 `Action` 路径；macOS 之外它退回 gpui-component 自己画的菜单，Lectern 只发 macOS。`ActionCard`、`SidebarRow`、`Capsule`、`Disclosure`、`Switch` 在 gpui-component 里没有对应的形态，或圆角、颜色与画布不同。
- 菜单项不是探针节点：系统菜单不在 GPUI 的元素树里。探针按名字触发菜单项的 `Action`（如 `welcome::SelectLanguage`），读弹出按钮的 `attrs.value` 确认结果。
- 「侧栏材质」（Welcome 的右栏、检查器）：画布用半透明加背景模糊。GPUI 的元素画不了模糊，所以不透明的面板用 `under_page_background`；工作区的侧栏仍然不画底色，透出窗口的模糊。
- `contrib::SidebarItem` 加 `count: Option<u32>`：行尾的数量（Worktrees 的 11、Merge Requests 的 2），`workspace` 交给 `SidebarRow::trailing`；`None` 不画。

### 贡献载荷（`contrib`）

`workspace` 的注册表（`services::workspace`）在 kernel 层，装不下 GPUI 的类型和 Fluent 键，所以每种贡献的具体载荷放在本 crate 的 `contrib` 模块，`workspace` 从 `Payload` 里 `downcast` 回来。字段、调用线程和生命周期以 [workspace 设计](workspace.md)「`ui::contrib` 的载荷类型」为准，本文只列名字和本 crate 决定的两点：

```rust
pub mod contrib {
    pub type ViewFactory<Arg> = Arc<dyn Fn(&Arg, &mut Window, &mut App) -> AnyView + Send + Sync>;
    pub struct Screen { pub build: ViewFactory<Target>, pub toolbar: Arc<dyn Fn(&mut App) -> ToolbarItems + Send + Sync> }
    pub struct ToolbarItems { pub title: Text, pub subtitle: Option<Text>, pub controls: Vec<AnyElement> /* ≤ 3 */ }
    pub struct SidebarPanel { pub build: ViewFactory<()> }
    /// 侧栏的一个条目（`services::workspace::SidebarEntry` 的载荷）。图标用本 crate 的 `Icon`（见「未决问题」1），不是 SF Symbols 的名字。
    pub struct SidebarItem { pub label: Key, pub icon: Option<Icon>, pub count: Option<u32>, pub on_select: WorkspaceCommand }
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

- `Palette`：每个外观一份，`init` 时建两份，`platform` 通知时整份替换，每份 29 个 `Hsla`（464 字节），放在一个 GPUI 全局 `UiState` 里，连同 `Strings`、减弱动态效果标志、`accent_follows_system`、`MonoMetrics` 的缓存（≤ 4 项）和 `platform::appearance::Watch`。`UiState` 固定大小，不随运行增长。
- 内置的系统色表：`src/palette/builtin.rs`，两个外观各 29 项（含派生的 diff 底色与 agent 色，内置表本身只存系统读数的分量），值是 `platform::appearance::system_colors` 在 macOS 27.0.1 上读到的 sRGB 分量（HIG「Color」页面只发布 13 个系统色、没有语义色，见「与设计的差异」），文件头写明来源与读取日期。用途：`platform` 返回 None 时（Linux 的 `cargo check`、headless、测试、系统接口失败），以及测试的参照。`init_fixed` 只用这张表。`diff_added_background` / `diff_removed_background`：`system(Green)` / `system(Red)` 在浅色下 12% 透明度、深色下 18% 透明度叠在 `control_background` 上，预先算好。`diff_added_inline_background` / `diff_removed_inline_background`：同样的两个来源在浅色下 28%、深色下 36% 透明度叠在 `control_background` 上，比行底色深一档；内置表与系统色两条路径都经同一个 `Palette::from_system` 给出，所以读到系统色之后它们随 `system(Green)` / `system(Red)` 一起更新。
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
| 冷启动到首帧 < 20 ms | `init` 在首帧前运行 | ≤ 300 µs：一次 `NSWorkspace` 读取、两份内置表的换算（58 个颜色）、设全局、登记通知。系统色的读取（约 70 次 `NSColor` 读取与换算，本进程第一次约 2 ms）在 `ready`，首帧之后。骨架窗口的 `attach` 在 `open_window` 的回调里，只登记一个观察者，不在这项基准里。不建线程、不读文件、不解析任何东西。`gpui_component::init`（运行时解析两份内嵌 JSON）放在 `ready`，首帧之后 |
| 空闲内存（空窗口）< 60 MB | `UiState` | < 8 KiB；`gpui_component::init` 建的主题全局与图标资源按 `ui/idle_memory` 实测，预期 < 1 MB，作为 `ui` 的份额写进 bench 的预算表 |
| 滚动与输入 120 fps | 每帧的查找与动画 | `colors`、`code_style`、`Transition::tick` 都是 O(1)、不分配；动画只改位移、透明度、裁剪（ADR 0015）。动画期间帧的工作时间 ≤ 8.3 ms 由 `bench` 的 `app/animation_frames` 在 `workspace` 接入后判定（[bench 设计](bench.md)的 `frames` 应答） |
| 滚动与输入 120 fps（探针关闭时） | 每个 `semantic` 包装元素的 prepaint | 一次 `AtomicBool` 的 Relaxed 读，≤ 2 ns（`ui/semantic_disarmed`）；不分配 |
| 打开文件到首屏高亮 < 16 ms | `mono_metrics` 的第一次计算 | 第一次调用 ≤ 200 µs（一次字体解析、三次度量），之后是缓存；`file_reader` 可以在首屏之前的任一帧先调一次 |

首帧之前做的事：`init`（上表）。其余都在 `ready` 之后。

新增基准（`bench`，macOS 判定，Linux 不编译）：`ui/init`（≤ 300 µs）、`ui/ready`（≤ 3 ms）、`ui/palette_resolve`（通知到达到新 `Palette` 就位 ≤ 300 µs）、`ui/transition_tick`（`Transition` 自己的一步，不含 GPUI 的帧请求，≤ 100 ns）、`ui/transition_tick_frame`（含帧请求的 `tick`，只记录：代价在 GPUI，随机器变化；帧的预算由 `app/animation_frames` 判定）、`ui/bezier_eval`（≤ 50 ns）、`ui/mono_metrics_first`（≤ 200 µs）、`ui/semantic_disarmed`（≤ 2 ns）、`ui/idle_memory`（`ready` 之后 `UiState` 加 gpui-component 全局 < 1 MB）。

## 界面

本 crate 没有自己的屏幕；它服务于交互规范的全部屏幕和三层。

- 用到的控件与动效原语见「接口」；本 crate 自己不定义任何颜色值、字体名和时长之外的东西：颜色来自 `platform` 或内置表，时长来自交互规范的表。
- Fluent 键：没有。本 crate 不显示自己的文字；`Text` 只是容器。
- 探针节点：控件自带角色（`Button` → `Role::Button`，`CircleCheck` → `Role::Toggle`（`checked`），`Progress` → `Role::Progress`（`attrs` 里 `fraction`），`Segmented` → `Role::TabBar` 与每项 `Role::Tab`（`selected`），`Button` 调了 `.active(bool)` 的 → `Role::Toggle`（`checked`），`GroupedList` → `Role::List`，`Row` → `Role::ListItem`（`selected`），`AgentDot` → `Role::Badge`（`attrs` 里 `agent`），`Expander` → `Role::Group`（`expanded`），`Hint` → `Role::Text`），`Label` 来自它们的 `Text`，`disabled` 来自 `Button::disabled`；ID 由视图给。
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
  - `hint_*`：`is_expired` 在 `duration` 加 `HOVER_OUT` 的淡出过后为 true（见「与设计的差异」第 3 步）；再次 `new` 同一个 ID 从新的 `shown_at` 计时；`frame_when` 的到达、停留、淡出三段与减弱动态效果下的零位移。
  - `node_id_*`：格式、转义、`child`；调试构建下非法的 `view` panic。
  - `code_colors_*`：对内置表的两个外观，五种颜色两两之间在正常、红色盲（protanopia）、绿色盲（deuteranopia）三种模拟（Viénot 1999 的 3×3 矩阵，在测试里写出）下的 ΔE（CIE76）≥ 12；每种与 `control_background` 的对比度（WCAG 2 的公式）≥ 3:1。`code_styles` 的两个外观各一份 insta 快照。
  - `palette_builtin_*`：内置表每个颜色的透明度在 0..=1、四个 `diff_*_background` 的叠加结果与手算一致；词级差异底色比同侧的行底色离 `control_background` 更远（ΔE 至少 1.5 倍）；两个外观的 `label` 与 `window_background` 对比度 ≥ 7:1；`label` 在四个 diff 底色上的对比度 ≥ 4.5:1（WCAG 2 AA，最小值是深色下的词级绿色 6.0）。
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

分五步（第 5 步是 [#283](https://github.com/SoloJiang/lectern-app/issues/283) 的第二批控件），每步一个 PR；前置条件 A 在 `platform` 单独一个 PR；第 1 步的 `contrib` 还要 `services` 的 `workspace` 模块（[workspace 设计](workspace.md)「对其他 crate 的要求」A，[#221](https://github.com/SoloJiang/lectern-app/pull/221)）先合并。每步都要在 macOS 上 `just check`、`just test -p lectern-ui`、`just doctest`、`just deny` 通过，并且 `rg 'std::fs|std::thread::spawn|std::net' crates/ui/src` 没有结果。

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

**第 5 步：第二批控件（`Refs #283`）。**

- `Palette::fill`、`fill_strong`，6 个新图标，`ActionCard`、`MenuItem`、`PopUpButton`、`show_menu`、`SidebarRow`、`Row::context_menu`、`Segmented::fill`、`Button::pill`、`Capsule`、`Disclosure`，`contrib::SidebarItem::count`。`Switch` 随 #21 的调用方实现。
- `palette_builtin_*` 检查两个新颜色在两个外观下都比底色深一档（浅色）或浅一档（深色）；`icon_assets_hold_every_icon` 覆盖 16 个图标；`ActionCard`、`PopUpButton`、`SidebarRow`（默认、`accent`、`compact` 加 `trailing`）、`Segmented::fill` 的语义节点快照；`disclosure_rotates_with_its_value`（0、0.5、1 三个值的角度）；`popup_items_dispatch_their_action`（菜单项的 `Action` 经 `dispatch_action` 到达，用 `TestAppContext`，不弹出系统菜单）。
- 同一节里的 `rg` 检查仍没有结果。

**视图 crate 接入后的持续检查**（写进各视图设计的验收，这里列出口径）：`rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}|Duration::from_millis|\.font_family\(|gpui_component::Button' crates/{workspace,welcome,project_home,review_view,file_reader}/src` 没有结果。

## 实际验证

第 1 步（macOS，Apple 芯片，macOS 27.0.1）：

- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过（`ui` 在视图层，依赖 `i18n`、`platform`、`services`、`syntax`、`logging`）。
- `just test -p lectern-ui`：23 个测试通过，其中「测试」里第 1 步的 `palette_builtin_*`（4 个）、`code_colors_*`（3 个）、`code_styles_*` 的两份 insta 快照、`text_*`（2 个）、`appearance_change_swaps_palette`、`set_locale_refreshes_windows`、`ready_runs_component_init_once`、`mono_metrics_is_cached`、`logs_hold_no_text`；另有 `attach_registers_once_per_window`、`init_then_ready_reads_the_system`（唯一读真实 AppKit 的测试：`init` 后 `is_ready` 为 false，`ready` 后为 true 且 `accent_follows_system` 为 true）、字体解析三种结果的 `mono_*`、`text_roles_have_the_spec_sizes`、`contrib_payloads_round_trip_and_are_send`。`just test -p lectern-i18n` 的 32 个通过，新增 `key_names_are_the_fluent_message_ids`。
- `just doctest`：`lectern-ui` 的 3 个 `compile_fail` 通过。
- `rg 'hsla\(|rgb\(|rgba\(|#[0-9a-fA-F]{6}' crates/ui/src --glob '!**/builtin.rs' --glob '!**/tests*'` 和 `rg 'std::fs|std::thread::spawn|std::net' crates/ui/src` 都没有结果。
- `git diff origin/main -- Cargo.lock | rg '^\+name = '` 没有结果：没有新的外部 crate。
- 没有做（[#230](https://github.com/SoloJiang/lectern-app/pull/230) 时）：`app` 的接入、`ui/*` 基准、手动的强调色与外观切换检查、`mono_metrics().family` 的手动检查。

`app` 的接入（[#234](https://github.com/SoloJiang/lectern-app/pull/234)，同一台机器，[app 设计「实际验证」](app.md#实际验证)的「`ui` 接入」有完整数据）：

- `init` 在 `run` 里开窗口之前（`set_menus` 之前），`attach` 在骨架窗口 `open_window` 的回调里（`Subscription` 放在根视图 `Skeleton`），`ready` 在 `startup::release` 里 `Early::start` 之后、`Kernel::start` 之前；test profile 用 `init_fixed`。`with_assets(IconAssets)` 没有加：`IconAssets` 随第 3 步的控件才存在。
- `lectern-app` 的 17 个单元测试通过，其中 `appearance_change_refreshes_the_skeleton_window` 与 `appearance_change_before_ready_does_not_touch_the_theme`（经 `testing::simulate_appearance_change`）。`tests/startup.rs` 的 `startup_writes_a_header_and_an_end_record` 新增检查 `ui initialised` → `first frame drawn` → `ui ready` → `plugins assembled` → `startup barrier released` 的顺序、`ui initialised` 的 `accent` 为 `builtin`、`ui ready` 的 `accent` 为 `system`；本机图形会话锁屏，启动测试收不到帧（app 设计「已知限制」），结果以 macOS CI 为准。
- 发布构建成功；临时 `HOME` 下的手动运行（`ui ready` 的 `accent`、两条记录的 `elapsed_us`、空闲内存）因同一个锁屏原因没有做，解锁后补测。
- 没有做：系统设置里切换强调色和浅色 / 深色的手动检查（agent 不改用户的系统设置；代码路径由 `appearance_change_*` 两个测试和 `ui` 的 `init_then_ready_reads_the_system` 覆盖）；`mono_metrics().family` 的手动检查（骨架窗口没有视图调用 `mono_metrics`，`font.rs` 只在后备和失败时记日志，等第一个用它的视图接入）；`ui/*` 基准（下一个 PR）。

第 2 步（macOS，Apple 芯片，macOS 27.0.1）：

- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过（`bench` 对 `lectern-ui` 和 `gpui` 的依赖只在 macOS 声明，见「与设计的差异」）。
- `just test -p lectern-ui`：38 个测试通过，其中本步新增 15 个：`bezier_matches_the_css_reference`、`bezier_ends_at_0_and_1`、`bezier_switch_overshoots_near_0_6`、`spring_constants_match_the_spec`、`spring_keeps_its_velocity_when_retargeted`、`spec_constants_hold_the_table`、`transition_continues_from_the_current_value`、`transition_same_target_does_not_restart`、`transition_set_arrives_at_once`、`transition_delay_holds_the_start_value`、`transition_reduced_motion_halves_and_fades`、`transition_not_reduced_moves`、`active_transitions_counts_the_ones_in_progress`、`reduced_motion_has_no_displacement`、`panel_moves_when_motion_is_not_reduced`。`cargo nextest run -p lectern-bench`：78 个通过（新增 `runner_macos_only_budgets_are_skipped_off_macos`）。
- `just doctest`：`lectern-ui` 的 3 个 `compile_fail` 仍通过。
- `just bench --measure ui/`：`ui/transition_tick` 中位数 43 ns（当时含帧请求，预算 ≤ 100 ns；`ENTER_SESSION` 弹簧的 tick 40 ns，记在结果的 note 里），`ui/bezier_eval` 中位数 8 ns（预算 ≤ 50 ns）。
- 预算分项之后（`ui/transition_tick` 只测 `step`，见「与设计的差异」第 2 步）：`just bench ui/` 本机 `ui/transition_tick` 中位数 14 ns（预算 ≤ 100 ns），`ui/transition_tick_frame` 43 ns（只记录），`ui/bezier_eval` 8 ns。
- `rg 'set_reduce_motion' crates/ui/src crates/app/src`、`rg 'Duration::from_millis|Duration::from_secs' crates/ui/src --glob '!**/motion*' --glob '!**/tests*'`、`rg 'std::fs|std::thread::spawn|std::net' crates/ui/src` 都没有结果。
- `git diff origin/main -- Cargo.lock | rg '^\+name = '` 没有结果：没有新的外部 crate。

第 3 步（macOS，Apple 芯片，macOS 27.0.1）：

- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过（`ui` 新增直接依赖 `gpui-kit-assets =0.7.3`，GPUI 系的 crate，已在 Cargo.lock）。
- `just test -p lectern-ui`：54 个测试通过，其中本步新增 16 个：`loading_phase_follows_the_thresholds`、`circle_check_animates_only_a_change_to_checked`、`segmented_fades_only_a_changed_selection`、`hint_expires_after_its_duration_and_the_fade`、`hint_shown_again_counts_from_the_new_instant`、`hint_frames_arrive_hold_and_fade`、`icon_assets_hold_every_icon`、`circle_check_toggles_from_key_and_click`（`v` 键经视图的 Action 与点击 16pt 元素中心都触发 `on_change`，填色动画的元素 ID 只由 `NodeId` 决定）、`segmented_slider_continues_from_current_position`、`every_control_renders_after_ready`（每个控件各渲染一次并推进三帧）、`control_before_ready_panics`、`node_id_formats_view_area_and_key`、两个调试构建下 `NodeId` 的 `should_panic`、`semantic_builder_takes_label_state_and_attrs`、`semantic_label_of_a_key_names_the_message`；`reduced_motion_has_no_displacement` 按第 2 步的说明扩到 `Expander` 的高度与 `Segmented` 的滑块；`logs_hold_no_text` 多渲染一个带金丝雀的 `Button`、`GroupedList`、`Row`（文本与业务键），断言的事件列表不变。
- `just doctest`：`lectern-ui` 的 4 个 `compile_fail` 通过（新增 `Button::new(id, "literal")`）。
- 验收的三条 `rg`（时长、颜色、IO）与 `rg 'set_reduce_motion'` 都没有结果；`git diff origin/main -- Cargo.lock | rg '^\+name = '` 没有结果。
- 没有做：每个控件的语义节点快照（要 `Recorder`，随第 4 步）；`app` 的 `with_assets(IconAssets)` 在 `app` 的接入 PR（已随 `workspace` 的 `app` 装配加上：`gpui_platform::application().with_assets(lectern_ui::IconAssets)`，`run` 之前；图标画出来的视觉检查要等第一个使用 `Icon` 的屏幕装配，现在 `workspace` 的工具栏还是文字元素）；「性能」没有列控件的基准，本步不加。手动的视觉检查（按钮高度、分组列表、分段控件的滑块）随第一个使用这些控件的屏幕一起做。

`Palette` 的词级差异底色（`ui/palette-diff-inline`，`review_view` 前置条件 F，macOS，Apple 芯片）：

- `just test -p lectern-ui`：57 个测试通过，新增 `palette_from_system_derives_the_diff_backgrounds`（系统色路径）、`palette_builtin_diff_inline_is_one_notch_deeper`、`palette_builtin_label_contrast_on_diff_backgrounds_is_at_least_4_5`，`palette_builtin_diff_backgrounds_match_the_hand_composite` 与 `palette_builtin_has_29_colors_with_alpha_in_range` 扩到四个 diff 底色。
- `just check`、`just deny`、`just doctest`（`lectern-ui` 的 4 个 `compile_fail` 仍通过）通过；`just bench ui/`：`ui/transition_tick` 中位数 55 ns（预算 ≤ 100 ns，noisy）、`ui/bezier_eval` 11 ns（≤ 50 ns），本 PR 不影响它们。

第 4 步（macOS，Apple 芯片，macOS 27.0.1）：

- `just check`、`just deny`、`just doctest`（`lectern-ui` 的 4 个 `compile_fail` 仍通过）通过。
- `just test -p lectern-ui`：73 个测试通过，其中本步新增 16 个：`semantic_records_bounds_and_parents`、`semantic_records_nothing_when_disarmed`、`semantic_nests_three_levels_and_records_the_call_site`、`semantic_declared_at_is_the_line_of_the_call`、`semantic_begin_drops_unclaimed_records_and_other_windows`（第二次 `begin` 丢掉没取走的记录；为另一个窗口打开时本窗口不记录）、`semantic_roles_map_to_accessibility_roles`、`semantic_access_writes_the_declaration`、`semantic_interactive_declares_like_semantic`，以及 `Button`、`AgentDot`、`CircleCheck`、`Progress`、`Segmented`、`GroupedList`（含两个 `Row`）、`Expander`、`Hint` 各一份 insta 快照（`semantic_snapshot_*`，在 `tests/probe.rs`）；`logs_hold_no_text` 多渲染一个带金丝雀的 `semantic(...)`（金丝雀在 `NodeId`、`Label`、`attrs` 里），并在记录器打开时画一帧，断言记录里有金丝雀而日志里没有。
- `just bench ui/`：`ui/semantic_disarmed` 中位数 0 ns（预算 ≤ 2 ns；单次读取低于基准框架整纳秒的分辨率，所以显示为 0，不是没测），`ui/transition_tick` 41 ns（≤ 100 ns）、`ui/bezier_eval` 8 ns（≤ 50 ns）。
- `rg 'Recorder::begin' crates --glob '!crates/ui_probe/**' --glob '!**/tests*' --glob '!**/benches/**'` 没有结果；三条 `rg`（时长、颜色、IO）与 `rg 'set_reduce_motion'` 都没有结果；`git diff origin/main -- Cargo.lock | rg '^\+name = '` 没有结果。
- 没有做：`ui_probe` 读记录器的部分（`ui_probe` 自己的 PR）；用屏幕阅读器读控件的无障碍树（`#[gpui::test]` 拿不到 AccessKit 树，映射由 `semantic_access_writes_the_declaration` 在纯函数上验证）；手动的视觉检查。

控件对齐设计画布（[#283](https://github.com/SoloJiang/lectern-app/issues/283) 的控件 PR；macOS，Apple 芯片，macOS 27.0.1）：

- `just check`、`just deny`、`just doctest`、`cargo metadata --format-version 1 --locked --all-features` 通过。
- `just test -p lectern-ui`：75 个测试通过，新增 `card_rows_are_16_by_14_and_standalone_rows_are_compact`（卡片行高 44pt = 16 + 28，卡片外的行 30pt）和 `icon_only_button_is_a_28pt_square_with_a_label`（28 × 28、辅助名、`active` 时是带 `checked` 的 `Toggle`）；`text_roles_have_the_spec_sizes` 改为新的 11 档；`semantic_records_bounds_and_parents` 改为计入行间 0.5pt 的分隔线。控件的 insta 快照没有变（节点的角色、标签、状态没有变，分隔线与卡片外圈是不声明节点的装饰元素）。
- `just test -p lectern-workspace -p lectern-welcome -p lectern-project-home -p lectern-review-view`：329 个测试通过，没有快照变化；`welcome` 的 `the_selected_row_is_scrolled_into_view` 因行内边距由 8 变 14pt 失败过一次，`REVEAL_MARGIN` 12 → 18 之后通过。
- `just bench ui/`：`ui/transition_tick` 中位数 14 ns（≤ 100 ns）、`ui/bezier_eval` 9 ns（≤ 50 ns）、`ui/semantic_disarmed` 0 ns（≤ 2 ns），`ui/transition_tick_frame` 42 ns（只记录）；本改动不在这些基准的路径上。
- 三条 `rg`（颜色值、`Duration::from_millis`、IO）在改动的 crate 里没有新增的结果。
- **没有做：看真实窗口的效果。** 这台机器的屏幕锁着，无法看到窗口，下面这些只能解锁后在真实窗口里看：卡片外圈 0.5pt 在 1x / 2x 屏上的清晰度；卡片对页面底色的边界（页面是 `under_page_background`，深色下它比 `control_background` 浅，卡片显得凹下去，要看是否合适；系统的实际读数可能与内置表不同）；工具栏的 Lucide 图标（`arrow-left`、`inspector`）真的画出来、28pt 方形的悬停底色；检查器 `Segmented` 的 24pt 高度、滑块平移与减弱动态效果下的淡入；紧凑行（检查器的文件列表）30pt 的密度；Welcome 的强调色选中行在 16 × 14pt 内边距下的样子；进度环 20pt / 2.5pt 的描边与端点。

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
- `app` 的 `appearance_change_before_ready_does_not_touch_the_theme` 不检查「`Palette` 已换」：它按原文直接用 `init`（不是 `init_fixed`），这时 `colors(window, cx)` 跟随窗口自己的外观，而测试平台改不了窗口外观，`simulate_appearance_change` 只触发处理函数。测试改为检查不 panic、窗口重画一次、`is_ready` 仍为 false、gpui-component 的 `Theme` 全局不存在；`Palette` 的切换由 `appearance_change_refreshes_the_skeleton_window`（`init_fixed`，固定外观跟着变）和本 crate 的 `appearance_change_swaps_palette` 检查。
- `app` 不加 `with_assets(IconAssets)`：`IconAssets` 随第 3 步的控件才定义，第 1 步的接入里没有它（「对其他 crate 的要求」的 `app` 项第 1 条随第 3 步做）。

第 2 步：

- `ENTER_SESSION.duration` 是 `ENTER_SESSION_SPRING` 从静止起步、`SPRING_EPSILON`（0.001，GPUI 的默认值，它没有公开）下的 `settle_time`，实测 491 ms，不是原文估的 0.6 s。`settle_time` 不是 const fn，所以常量写死，`spring_constants_match_the_spec` 核对两者相差 ≤ 1 ms。
- 弹簧的结束由 `SpringConfig::is_settled` 判断；`duration` 的两倍是中途改目标后仍未静止时的后备上限。减弱动态效果下弹簧按定时的淡入淡出播放：时长减半、曲线用 `sampled_easing`。
- `Easing::EaseOut` 取 CSS 的 `ease-out`，即 `cubic-bezier(0, 0, 0.58, 1)`；原文只写「ease-out」。
- `Motion::effective` 在减弱动态效果下只把 `duration` 减半，`delay` 不变（原文没有写 `delay`）。`Fade` 里的 `Motion` 的 `moves` 为 false。
- GPUI 的 `Animation` 没有延迟，`Motion::animation` 把延迟折进去：动画总长 `delay + duration`，曲线在延迟结束前保持 0。
- `Transition::tick` 自己的工作是常数时间、不分配；它调用的 `Window::request_animation_frame` 在 GPUI 里装箱一个回调（一次分配），这是 GPUI 的代价，不在本 crate 控制内。`tick` 拆成 crate 内部的 `step`（提交 `now` 时的值，返回是否仍在进行）加帧请求；`testing::step_transition`（`test-support`）只调 `step`。`ui/transition_tick` 用它在普通循环里计时，测的是「性能」约束的那部分。含帧请求的 `tick` 改名 `ui/transition_tick_frame`，只记录：`tick` 只能在视图的 render 里调用（GPUI 只允许正在渲染的视图请求下一帧），所以它用 GPUI 的测试平台开一个窗口，取「一次 render 里 4096 次 tick」与「不 tick 的 render」之差除以 4096 作为一个样本，用 `Ctx::compare` 交替采样。原来 `ui/transition_tick` 测的就是这个含帧请求的量：本机 41–43 ns，macOS CI 上 100–152 ns（#234、#235、#240 的基准任务因此超过 100 ns），超出的部分是 GPUI 的分配、唤醒平台和 `notify`，`ui/bezier_eval` 在 CI 上同样慢约 2.5 倍（8 → 20 ns）。预算数字没有变。
- `Transition` 按字段算约 112 字节，不是 56：它存了两个 `Instant`（当前段的起点和上次采样的时刻，弹簧按后者逐步推进）和整份 `Motion`（56 字节）。由视图持有，个数以个位数计，不影响内存预算。
- 进行中的计数是进程级的 `AtomicUsize`；nextest 一个测试一个进程，`active_transitions_*` 以测试开始时的值为基准断言差值。
- `reduced_motion_has_no_displacement` 本步只覆盖设计里的面板偏移示例（`Transition` + `Effective::Fade`），`Expander` 和 `Segmented` 的滑块随第 3 步加入；另加 `panel_moves_when_motion_is_not_reduced` 作为对照。
- `bench` 对 `lectern-ui` 和 `gpui`（`test-support`）的 dev 依赖只在 macOS 声明（`[target.'cfg(target_os = "macos")'.dev-dependencies]`），Linux 上 `bench` 仍不依赖 GPUI、仍在 `just check` / `just test` 的范围内。为此 `scripts/check_architecture.py` 第 4 条改为：Apple 专属的 target 谓词只对外部的、非 GPUI 的依赖算作 macOS 绑定（工作区成员由它自己的清单检查，GPUI 只在 macOS 上编译是构建约束，不是绑定）。预算表加 `Budget::macos_only`：Linux 上 `just bench` 把这些条目记为 `skipped`（只在 macOS 运行），不算缺基准（[bench 设计「预算表」](bench.md#预算表)已补）。

第 3 步：

- `semantic` 的数据类型提前到本步：控件的构造函数接受 `NodeId`，所以 `NodeId`、`Role`、`Label`、`State`、`Semantic` 与 `node_id_*` 测试随控件一起实现；`semantic(...)`、`semantic_interactive(...)`、`SemanticElement`、`Record`、`Recorder`、无障碍属性映射和每个控件的语义节点快照仍在第 4 步。控件经一个 crate 内的接缝 `semantic::declare(spec, element)` 声明节点，本步它是恒等函数；第 4 步把它换成记录的包装元素，控件不用改。`Semantic` 多了构造用的 `new` / `label` / `state` / `attr`，`Label::of(&Text)` 从 `Text` 取键与文字。
- 不包装 gpui-component 的 `Skeleton`、`Progress`、`ProgressCircle`：`Skeleton` 带一个关不掉的 2 s 脉动，`Progress` / `ProgressCircle` 对传入的值再做一次自己的过渡（gpui-component 的 motion token，不是 `PROGRESS`）并读 GPUI 的 `reduce_motion` 标志，都与「动效」规则相抵。骨架块用 `div` 画（`tertiary_label` 35% 透明度、4pt 圆角）；进度条用两层 `div`（轨道 `separator`、填充 `accent`，宽度 `relative(fraction)`）；进度环与勾选的对勾用 `canvas` + `PathBuilder::stroke` 自绘（环 16pt、2pt 描边，`arc_to`；对勾按长度比例描到 `fraction`）。「依赖」表已照改。
- `Button` 的文字不走 gpui-component 的 `label`：它把字号定成 14px / 16px，外面改不了；文字作为子元素按 `TextRole::Body`（13pt）排，`accessibility_label` 另外设成同一段文字。颜色用 `ButtonVariant::Custom`，但它把普通态的填色稀释到 20% 透明度，所以普通态的填色在实例样式上另设一次（只在未禁用时，实例样式会在禁用态重放）。悬停 / 按下的填色是强调色的明度偏移（−0.04 / −0.10）与 `label` 的 6% / 12% 淡色，没有新的颜色值。`Regular` 高度 30pt 由实例样式覆盖 gpui-component 的 32pt，圆角 6pt（`Small` 5pt）。
- `CircleCheck` 只在它看到 `checked` 从 false 变成 true 时播放填色与对勾的动画：已勾选状态下挂载（打开已看过一半的会话、虚拟列表里的行滚回来）直接画终态，否则每次挂载都会把已看的文件重新「勾一遍」。上一帧的 `checked` 记在 GPUI 的 keyed element state（`id.part("mark")`）里，与动画元素同生命周期；减弱动态效果下 `Segmented` 滑块的淡入按同一规则（`id.part("selection")`）。自审发现的 P1。
- `AgentDot` 多一个 `node(NodeId)`：设计的签名没有 ID，而 `Badge` 节点需要；不给时是装饰元素，不声明节点。
- `Segmented::select` 与 `Expander::go` 是新加的关联函数：减弱动态效果下「高度直接到位」「滑块位移为 0」要有人在 `Transition` 上调 `set` 而不是 `go`，放在控件里让所有调用方一致，`reduced_motion_has_no_displacement` 也经它们断言。减弱动态效果下 `Segmented` 忽略 `slider`，把滑块放在 `selected` 上并以 `SEGMENT` 的减半时长淡入（按 `selected` 换动画元素的键）。
- `Row` 的悬停过渡放在 GPUI 的 keyed element state（`window.use_keyed_state`）里，与 gpui-component 的 `Button` 放焦点句柄的做法相同：`RenderOnce` 元素每帧重建，状态要跨帧。`on_hover` 的回调对 `Transition` 调 `go`（`HOVER_IN` / `HOVER_OUT`）再 `window.refresh()`；render 里 `tick`，背景是 `label` 的 6% × 当前值。选中行文字用 `on_accent()`（`selected_content_background` 上的白字）。
- `Hint` 的到期是 `duration` 加 `HOVER_OUT` 的淡出：原文「`duration` 后用 `HOVER_OUT` 淡出并由调用方 drop」，`is_expired` 在淡出结束后才为 true，否则调用方在淡出开始时就会 drop 它。位移用绝对定位的 `top`（`TOP + offset`），GPUI 的 `div` 没有 translate；绝对定位只动它自己，不触发兄弟重排。显示期间每帧 `request_animation_frame`：调用方本来就每帧问 `is_expired`，不另加帧；停留段也在请求帧，记为已知限制（一次提示 ≈ 2.7 s，少见）。时长常量 `HINT_DURATION`、`LOADING_SKELETON_AT`、`LOADING_PROGRESS_AT` 放在 `motion`，验收的 `rg` 才能不放过控件文件。
- 图标名的对应：`Clone` → Lucide `copy`，`Back` / `Forward` → `arrow-left` / `arrow-right`，`Close` 与 `Inspector` 是 gpui-kit-assets 自带的 `close.svg`、`inspector.svg`，其余同名。`Icon` 实现 gpui-component 的 `IconNamed`，所以 `Button::icon(Icon)` 走它的 `Icon` 元素，`IconName` 不暴露。`icon_assets!` 的结构体不能带文档注释，文档在 `lib.rs` 的再导出上。

词级差异底色（`review_view` 前置条件 F，[#238](https://github.com/SoloJiang/lectern-app/pull/238)）：

- `Palette` 多两个字段 `diff_added_inline_background`、`diff_removed_inline_background`，规格按 [review_view 设计「对其他 crate 的要求」F](review_view.md#对其他-crate-的要求)：`system(Green)` / `system(Red)` 在浅色 28%、深色 36% 透明度下叠在 `control_background` 上。内置表（`builtin.rs`）只存系统色的分量，没有存派生色，所以「内置表与系统色两条路径」落在同一个 `Palette::from_system`：`Palette::builtin` 是它对内置分量的调用，`ready` 与外观通知对系统读数的调用，两条路径不会不一致。`builtin.rs` 因此没有改动。
- `label` 在四个 diff 底色上的对比度检查取 4.5:1（WCAG 2 AA 的正文标准），不是 `label` 与 `window_background` 的 7:1：深色下 36% 的绿色底上 `label` 是 6.0:1，达不到 7:1，而 diff 行里文字本来就多由代码配色着色。词级差异底色上叠加的代码配色（`Red` 的字符串在 `Red` 的底色上）不在 `palette_builtin_*` 检查的范围内，随 `review_view` 的视觉验收。

第 4 步：

- `declare` 的返回类型从 `E` 改为 `AnyElement`（`#[track_caller]`），控件的代码没有改：它们本来就在 `render` 里把各自的元素传进去，`AnyElement` 也让一个 `match` 的两个分支（声明节点 / 装饰元素）有同一种类型。因为是 GPUI 调控件的 `render`，不是视图，控件自己节点的 `declared_at` 是控件的 `render` 所在行（`controls/*.rs`），不是视图里的构造处；视图自己用 `semantic(...)` 声明的节点才指向视图的调用处。「只记录业务视图层」由 `ui_probe` 按 `declared_at` 的文件路径过滤。
- 记录器的 `Vec<Record>` 和父节点栈都在线程本地（`thread_local!` 的 `RefCell`），只有开关 `ARMED` 是进程级的 `AtomicBool`；设计原文是一个进程级的 `Vec`。理由：记录器只在主线程上用，线程本地省掉一把锁，关闭时的路径仍然只有一次原子读。限制：两个线程不能同时记录（nextest 一个测试一个进程；`ui_probe` 只在主线程上调用）。`begin(window)` 记下窗口，包装元素只在自己的窗口等于它时记录，所以另一个窗口的帧不会混进来。
- `Record::bounds` 是包装元素在 prepaint 里拿到的范围，也就是子元素的范围（`SemanticElement::request_layout` 返回子元素的布局），不是 `Window::layout_bounds`；`visible` 是这个范围与 prepaint 时的内容遮罩相交。`ui_probe` 设计里写的 `Window::layout_bounds` 在这里得到同一个值，探针按本文为准（两边已一致的形状见「对其他 crate 的要求」）。
- `semantic_interactive` 的约束是 `StatefulInteractiveElement + IntoElement`，不是 `ui_probe` 设计里写的 `InteractiveElement`：GPUI 的 `accessibility_id`、`role`、`aria_*` 只在带 ID 的元素上。映射 `accesskit_role` 里没有任何角色映射到 `GenericContainer`（GPUI 把它从无障碍树里去掉，而且对它断言）：`Sidebar` → `Complementary`、`Panel` → `Pane`、`Toggle` → `CheckBox`、`TabBar` → `TabList`、`Text` 与 `Badge` → `Label`、`Diff` 与 `DiffHunk` → `Group`、`DiffLine` → `ListItem`。`aria_selected` 对 `ListItem`、`TreeItem`、`Tab`、`MenuItem` 总是写（「未选中」也是事实），其他角色只在选中时写。
- 控件仍经 `declare`，即 `semantic(...)`，不写 `semantic_interactive` 的无障碍属性：控件的无障碍属性（`Button` 的 `accessibility_label`、gpui-component 自己设的）第 3 步已经有，本步不改控件。`semantic_interactive` 给视图自己的带 ID 容器用。
- `ui/semantic_disarmed` 测的是 `Recorder::is_armed()`，即包装元素在关闭时自己读的那一次原子加载；包装元素把 `request_layout`、`prepaint`、`paint` 转给子元素的 `AnyElement` 调用是 GPUI 的元素管道，不在这条预算里，也无法在一帧的噪声里分辨 2 ns。预算表登记为 `share_macos`，与第 2 步的两条相同。
- 每个控件的快照放在 `tests/probe.rs`（和记录器的测试一起），`tests/semantic.rs` 仍是 `NodeId` 与构造器的测试；快照去掉 `bounds` 和 `declared_at`，保留 `visible` 与 `parent`。

### 控件对齐设计画布（[#283](https://github.com/SoloJiang/lectern-app/issues/283)）

- 以设计画布（`Welcome` / `Main` / `Review`）为准，控件的数值取画布，类名与颜色仍是系统语义色（ADR 0015）。画布与 HIG 不一致的地方取 HIG：进度环 20pt / 2.5pt 取画布（HIG 对确定进度的环没有规定尺寸，16pt 是不确定进度的小转轮），`Segmented` 总高取 HIG 的 24pt（画板的按钮 24pt 加两边各 2pt 的内边距是 28pt），圆角 8pt、滑块圆角 6pt、文字 medium 取画布。
- `TextRole` 的名字：`Title`（26pt）→ `Title1`（22pt），`Subtitle`（22pt，没有调用方）→ `Title2`（17pt），新增 `Title3`、`Headline`、`Callout`、`Footnote`；`Caption`（11pt）就是 HIG 的 Subheadline，没有改名，因为设计文档里到处在用。`LargeTitle` 40 → 26pt。唯一的调用方变化是 `project_home` 的空状态 `Title` → `Title1`。比 `Body` 小的档（`Callout`、`Caption`、`Footnote`）都是 `secondary_label`，与 `Secondary` 一致。
- 卡片边线用一个绝对定位的 0.5pt 外圈画在行的上方，不是边框：边框会占 0.5pt，让行的内容区偏移，选中行的底色也盖不到边。页面底色在 `workspace`：有阅读栏的屏幕（`layout.reading_column`）的内容区用 `under_page_background`，其他（Welcome）仍是 `window_background`。内置表里浅色下 `under_page_background`（0.9647）比卡片的白色深一点；深色下它（0.1569）反而比 `control_background`（0.1176）浅，卡片看起来凹下去，画布的深色是卡片比页面浅。系统颜色里没有更合适的一对，外圈保证了两种外观下卡片都有边；要不要为深色另取一对由真实窗口的效果决定。`review_view` 的根元素自己画了 `window_background`，盖住了阅读栏的底色，它的屏幕 PR 去掉那一行之后才会显示。
- `Row` 有两种密度：在 `GroupedList` 里是卡片行（16 × 14pt、间距 14pt），在外面是紧凑行（30pt、6pt、间距 8pt、圆角 6pt，检查器的文件列表）。`GroupedList` 在渲染时把自己的行标成卡片行（私有的 `in_card`）。行标题的 semibold 不由 `Row` 统一设置：画板里只有会话卡片的标题是 600，Outside 与 Recently Done 的行是常规字重，所以提供 `TextRole::Headline`，由屏幕给标题用。
- 分隔线从文字起点开始：`GroupedList::separator_inset`，默认是行的左内边距 16pt（画板的 Recently Done）。行首有圆点的行（会话卡片）要传 40pt（16 + 10 + 14），这在 `project_home` 的屏幕 PR 里传。`Row` 看不到子元素，所以不自己猜。
- 选中默认是安静的灰底，`Row::accent()` 才是强调色：Welcome 的两个列表是获得焦点的源列表，传 `accent()`（机械改动）。`project_home` 卡片选中时副行用 `on_accent` 的淡色，在灰底上看不清，已去掉这一支（机械改动）。
- `Button::icon_only` 与 `ButtonSize::Toolbar`（28pt）：画板的工具栏按钮是 28pt 方形，标准的推按钮 24 / 30pt 都不是。图标按钮的 `label` 是辅助名，不画出来；`active` 让图标变强调色并把探针节点换成 `Toggle`。
- `welcome` 的 `REVEAL_MARGIN` 12 → 18pt：卡片行内边距由 8pt 变 14pt，滚到选中行时行底部多出的部分要计入（机械改动）。

### project_home 第 2 步

- `on_accent()` 公开（`lectern_ui::on_accent`）：视图在选中的 `Row` 里放自己的文字时，次要文字要用强调色上的字色（`TextRole::Secondary` 的灰色在强调色上看不清）。仍然不是 `Palette` 的字段，值仍只在 `palette/builtin.rs`。控件对齐画布之后，选中默认是安静的灰底，`on_accent` 只剩 `Row::accent()` 的行（Welcome 的源列表）、`Button`、`CircleCheck` 和 gpui-component 的主题投影用。
