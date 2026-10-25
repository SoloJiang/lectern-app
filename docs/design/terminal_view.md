# terminal_view 设计

状态：已接受，尚未实现。

对应 issue [#5](https://github.com/SoloJiang/lectern-app/issues/5)（libghostty-vt 接入与 GPUI 网格绘制）的视图部分和 [#12](https://github.com/SoloJiang/lectern-app/issues/12) 的终端视图。`terminal_view` 把 `terminal` 给出的网格快照（[terminal 设计「帧」](terminal.md#帧)）画在 GPUI 里，把键盘、输入法、鼠标、滚轮、焦点、剪贴板和尺寸变化转成 `Input` 发回 `terminal`。它是 Conversation 的 Terminal 页里的终端视图，在界面的「收起」一层（[交互规范「三层结构」](interaction.md#三层结构)、[ADR 0012](../adr/0012.md)）。

## 范围

- 做：
  - `TerminalPane`：一个终端的视图实体。持有这个终端的 `FrameInterest`，读最新的 `Arc<Frame>`，按帧绘制。
  - 自绘元素：行的字形按网格列放置；单元底色、选区、下划线与删除线、光标；方框绘制字符和块元素按几何图形画，不用字体（「数据结构」的「方框与块元素」）。
  - 输入：按键（含 Esc、Tab、Enter、方向键、⌃ 组合键）、输入法（中文拼音等）、粘贴、复制、全选；鼠标点击、拖动选择、滚轮；焦点进出；网格尺寸随所在区域变化。
  - 颜色：从 `ui` 的终端配色得出 `Theme`，外观变化时发 `Input::SetTheme`。
  - 两个纯函数供宿主在打开终端之前用：`theme`（`OpenSpec::theme`）和 `grid_size`（`OpenSpec::size`）。
  - 本视图的快捷键表，由宿主插件登记。
  - 语义节点：只有尺寸和状态，不含屏幕内容。
- 不做：
  - PTY、终端状态机、按键编码、选区的文字：`terminal`。
  - Terminal 页、恢复按钮、状态行（启动中、运行中、已退出、启动失败的原因）、关闭按钮：`conversation`。
  - 终端的 ANSI 配色表：`ui`（「对其他 crate 的要求」第 1 项），本 crate 只做换算。
  - 快捷键的登记：本 crate 不是插件，不能登记贡献；由 `conversation` 的终端插件登记（「对其他 crate 的要求」第 2 项）。
- 本 issue 之外：
  - 右键菜单（复制、粘贴、全选、清空滚动缓冲）：`ui` 还没有菜单控件，另开 issue。P1 只有快捷键，`ClearScrollback` 有 Action、没有默认键。
  - 超链接、URL 识别、搜索：同 terminal 设计「范围」。
  - 字号缩放（⌘+、⌘-）：固定为 `TextRole::Code` 的 12pt；⌘ 组合键照常冒泡。
  - VoiceOver 朗读终端内容（「已知限制」第 4 条）。

## 依赖

本 crate 是视图层的库 crate，不是插件（没有 `plugin = true`）。插件 crate `conversation` 依赖它（[ADR 0025](../adr/0025.md)、架构规则 7）。它不依赖 `lectern-terminal`，只经 `services::terminal` 的接口用终端。

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui` | 视图、自建元素、文字排版（`layout_line`）、绘制（`paint_glyph`、`paint_quad`、`paint_path`、`paint_underline`）、输入法（`EntityInputHandler`）、剪贴板、Action | ADR 0016 锁定的版本 |
| `futures` | 复制结果从终端线程回到主线程（`channel::oneshot`） | 工作区已有，`review_view` 同样用 |
| `lectern-services` | `Terminals`、`Frame`、`Input`、`Theme`、`GridSize` 等 | 终端的接口（[terminal 设计「接口」](terminal.md#接口)） |
| `lectern-kernel` | `ServiceRef`、`ActivationToken` | 调用服务、复制的回调 |
| `lectern-ui` | `Palette`、终端配色（新增）、`mono_metrics`、`TextRole::Code`、语义节点 | ADR 0015：颜色与字体只从 `ui` 取 |
| `lectern-i18n` | `keys::terminal::*` | ADR 0013 |
| `lectern-logging` | 详细级别的统计（「日志与诊断」） | ADR 0046 |

不引入新的外部 crate。

## 接口

```rust
/// 一个终端的视图。`Entity<TerminalPane>` 由宿主（`conversation` 的 Terminal 页）在页显示时建、
/// 页隐藏时 drop。主线程；不做 IO。
pub struct TerminalPane { /* 见「数据结构」 */ }

impl TerminalPane {
    /// `terminals` 是宿主插件取得的服务引用；`token` 是宿主插件这次激活的 token
    /// （`PluginHandle::token()`），复制时交给 `Terminals::copy_selection`。
    /// 构造时调用一次 `Terminals::watch(id)`，持有返回的 `FrameInterest` 到自己 drop：
    /// 视图存在期间 `terminal` 才生成帧。不阻塞。
    pub fn new(
        terminals: ServiceRef<dyn Terminals>,
        token: ActivationToken,
        id: TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self;

    /// 宿主收到这个终端的 `TerminalEvent::Frame`，或收到 `TerminalEvent::Listed` 时调用。
    /// 取 `frame(id)` 与 `info(id)`；帧的 `generation` 或运行状态变了才 `cx.notify()`。
    /// 只复制 `Arc` 和比较，不排版。主线程。
    pub fn refresh(&mut self, cx: &mut Context<Self>);

    /// 宿主恢复终端之后把焦点交给它；Terminal 页的键盘焦点在这里。
    pub fn focus_handle(&self) -> FocusHandle;

    pub fn id(&self) -> TerminalId;
}

impl Render for TerminalPane { /* 根元素：KeyContext "terminal"，自建元素占满可用区域 */ }
impl Focusable for TerminalPane {}
/// 输入法（「数据流与线程」的「输入法」）。
impl EntityInputHandler for TerminalPane {}

/// 当前外观的终端颜色，换算成 `Theme`。纯函数；宿主在 `Terminals::open` 之前调用
/// （`ui::colors(window, cx)` 取 `palette`），本视图每次渲染也用它比较是否要发 `SetTheme`。
pub fn theme(palette: &Palette) -> Theme;

/// 把一块区域换算成网格：`area` 是视图可用的逻辑尺寸，`metrics` 是 `ui::mono_metrics`，
/// `scale` 是 `window.scale_factor()`。纯函数。列数至少 20、行数至少 1；
/// 区域不够 20 列时视图按 20 列绘制并裁掉右侧。
pub fn grid_size(area: Size<Pixels>, metrics: &MonoMetrics, scale: f32) -> GridSize;

/// 宿主插件把它放进自己的 `KeyBindings` 贡献（「界面」的「Action 与快捷键」）。
pub fn key_bindings() -> Vec<(&'static str, Box<dyn Action>, Option<&'static str>)>;

/// 根元素的 KeyContext。
pub const KEY_CONTEXT: &str = "terminal";

actions!(terminal_view, [Copy, Paste, SelectAll, ClearScrollback]);
```

宿主的用法（`conversation`）：

```rust
let palette = ui::colors(window, cx);
let size = terminal_view::grid_size(page_area, &ui::mono_metrics(window, cx), window.scale_factor());
let spec = OpenSpec { launch: Launch::Resume(key), size: Some(size), theme: terminal_view::theme(palette) };
let id = terminals.get()?.open(spec)?;
let pane = cx.new(|cx| TerminalPane::new(terminals.clone(), token.clone(), id, window, cx));
pane.read(cx).focus_handle().focus(window);
```

`page_area` 不知道时传 `size: None`，视图第一次布局后发 `Input::Resize`。`terminal` 在启动线程上准备环境和进程，`Resize` 通常在进程启动之前到达。

## 数据结构

```rust
pub struct TerminalPane {
    terminals: ServiceRef<dyn Terminals>,
    token: ActivationToken,
    id: TerminalId,
    interest: Option<FrameInterest>,
    frame: Option<Arc<Frame>>,          // None：还没有第一帧，或终端已关闭
    state: Option<RunState>,            // 来自 `info(id)`，语义节点用
    focus: FocusHandle,
    sent_size: Option<GridSize>,        // 上一次发出的 `Resize`
    sent_theme: Option<Theme>,          // 上一次发出的 `SetTheme`（或 `open` 时给的）
    rows: RowCache,
    marked: Option<Marked>,             // 输入法正在组字的文字
    scroll_rest: f32,                   // 滚轮不足一行的余量（行）
    drag: Option<Drag>,                 // 按下的按钮、上一次发出的格子、自动滚动的任务
    grid: Bounds<Pixels>,               // 自建元素在 prepaint 记下的网格范围，鼠标与输入法用
    _subscriptions: [Subscription; 3],  // 焦点进、焦点出、窗口激活
}
```

### 格子与字形

- 字体：`TextRole::Code`（SF Mono 12pt，`ui` 的字体与度量）。单元宽 `MonoMetrics::cell_width`，行高 `MonoMetrics::line_height`，基线在行顶下 `ascent` 处。粗体用 `FontWeight::BOLD`，斜体用 `FontStyle::Italic`。
- 网格在视图区域里四边各留 4pt。`GridSize::cell_width` / `cell_height` 是单元的设备像素（逻辑尺寸乘 `scale` 后取整）。
- 一行的排版：`window.text_system().layout_line(row.text, 12pt, runs, None)`。`runs` 按 `StyleRun` 换成字节范围，只区分字体（粗细与斜体），颜色在绘制时按列查。不用 `force_width`：GPUI 的 `apply_force_width_to_layout` 按字形的序号乘单元宽放置，宽字符（占两列）之后的字形会错一列。
- 放置：每个字形的 `ShapedGlyph::index`（字节）在 `GridRow::clusters` 里二分查到所在的字素簇和列。簇的第一个字形放在 `列 × 单元宽`；同一簇里后面的字形（组合符号）保持排版给出的相对位置。宽字符（`Cluster::wide`）在两列的宽度里居中。回退字体的字形比单元宽时照常放置，不缩放（「已知限制」第 2 条）。
- 行缓存 `RowCache`：`GridRow::key` → `Arc<RowLayout>`（每个字形的字体、字形 ID、x、是否表情、所在样式段）。两代：本帧用到的放进新表，上一帧的表在本帧画完后丢弃。所以缓存最多是两帧的行数，不随滚动缓冲增长。`key` 含样式和颜色，外观变化后的新帧自然换键。GPUI 自己的 `layout_line` 缓存按文字与字体缓存，帧与帧之间也复用。

### 方框与块元素

agent 的命令行界面用方框绘制字符画输入框（`╭─╮│╰─╯`）。字体里的这些字形在行高大于字形高度时上下接不上，所以下列字符不画字形，按几何图形画满整个格子：

- U+2500–U+2503、U+250C–U+254B：细线与粗线，及它们的角、T 形、十字。
- U+2550–U+256C：双线。
- U+256D–U+2570：圆角（`paint_path` 画四分之一圆弧）。
- U+2574–U+257F：半段线。
- U+2580–U+259F：块元素（八分之一的上、下、左、右块，整块，四分块）；U+2591–U+2593 的阴影按前景色 25%、50%、75% 的不透明度填满格子。

虚线（U+2504–U+250B、U+254C–U+254F）、斜线（U+2571–U+2573）和其他字符仍用字体。图元由纯函数 `box_glyph(ch) -> Option<&'static [Prim]>` 给出，坐标以格子为单位；绘制时乘单元尺寸，细线宽取 1 个设备像素与单元宽八分之一中较大者并对齐设备像素，粗线是细线的两倍。

### 绘制的顺序

每帧按行：

1. 单元底色：`CellStyle::bg` 为 Some 的样式段，一个 `paint_quad`。视图的底色是 `Theme::background`。
2. 选区：`Frame::selection` 的每段一个 `paint_quad`。有焦点时用 `Palette::selected_content_background`，没有焦点时用 `unemphasized_selected_content_background`。
3. 块光标的底：有焦点、形状为 `Block` 时用 `FrameColors::cursor` 填满光标格子（宽字符填两格）。
4. 字形：`paint_glyph`（表情用 `paint_emoji`），颜色取样式段的 `fg`；`FAINT` 时不透明度 0.5；`INVISIBLE` 时不画；块光标下的字形用 `FrameColors::background`。方框与块元素按上一节画。
5. 装饰：`Single` 下划线、删除线、上划线用 `paint_quad`；`Curly` 用 `paint_underline`（`wavy: true`）；`Double` 画两条；`Dotted`、`Dashed` 用短段的 `paint_quad`。颜色取 `underline_color`，没有时取 `fg`。
6. 光标：`Bar` 是格子左边 1pt 宽的竖条，`Underline` 是基线下 1pt 高的横条；没有焦点时不论形状都画空心方框（0.5pt 描边）。`Frame::cursor` 为 None 时不画。

光标不闪烁：忽略 `Cursor::blinking`。闪烁要每秒两次重画，空闲时也不停；macOS 终端的默认描述文件也不闪烁。

### 颜色

- `theme(palette)`：取 `ui` 的终端配色（「对其他 crate 的要求」第 1 项），把每个颜色叠在终端底色上换成不透明的 `Rgb`（系统的文字色带透明度）。`scheme` 取 `palette.appearance`。
- 渲染时比较 `theme(colors(window, cx))` 与 `sent_theme`，不同才发 `Input::SetTheme`。外观变化时 `ui` 刷新窗口，所以不需要另外订阅。
- 选区色、空心光标的描边直接从 `Palette` 取，不经 `terminal`。

## 数据流与线程

```
terminal 线程 ──TerminalEvent::Frame{id}──▶ [宿主监听] Job ──▶ [主线程] TerminalPane::refresh ──▶ cx.notify()
                                                                        │
GPUI 帧：render ─▶ 自建元素 prepaint（grid_size、Resize、行缓存）─▶ paint（底色、选区、字形、装饰、光标、输入法注册）

按键 / 输入法 / 鼠标 / 滚轮 / 焦点 ──▶ [主线程] Terminals::input（入队，不阻塞）──▶ terminal 线程
⌘C ──▶ copy_selection(id, token, reply) ──▶ terminal 线程 reply(文字) ──oneshot──▶ [主线程] 写剪贴板
```

- 全部在主线程上。本 crate 不提交 executor 任务，不做 IO；`Terminals` 的调用都不阻塞（[terminal 设计「接口」](terminal.md#接口)）。
- 帧只在 `refresh` 里取，绘制时读同一个 `Arc<Frame>`，不在绘制中途换帧。
- 尺寸：自建元素在 prepaint 里按自己的范围算 `grid_size`，与 `sent_size` 不同才发 `Input::Resize`。窗口拖动缩放时每帧最多一次；`terminal` 只处理连续 `Resize` 的最后一个。
- 每帧的排版只做缓存里没有的行：输入回显通常一行，整屏变化（清屏、滚动、备用屏幕）是全部行。

### 键盘

GPUI 先匹配快捷键，再调用元素的 `on_key_down`（gpui-pre 0.3.9 `window.rs` 的 `dispatch_key_event`）；在 macOS 上，正在组字、输入法开着时的可打印键、以及没有 `key_char` 的键先交给输入法（`gpui-pre-macos` 的 `handle_key_event`）。本视图按这个顺序处理：

1. 快捷键（`key_bindings()`，上下文 `terminal`，比所有祖先上下文深）：
   - ⌘C → `Copy`，⌘V → `Paste`，⌘A → `SelectAll`。
   - Esc → `NoAction`：只为挡住祖先上下文 `Workspace` 的 `workspace::Escape`（GPUI 的 `NoAction` 让同一个键在更浅上下文里的绑定失效），Esc 于是到达下一步。
2. `on_key_down`（根元素），按下表；处理了的调用 `cx.stop_propagation()`：

   | 按键 | 处理 |
   |---|---|
   | 带 ⌘ | 不处理，冒泡（⌘\、⌥⌘I、⌘K、⌘W 等照常工作） |
   | ⇧PageUp、⇧PageDown、⇧Home、⇧End | `Input::ScrollTo`（`PageUp` / `PageDown` / `Top` / `Bottom`） |
   | `enter`、`tab`、`backspace`、`escape`、`up` / `down` / `left` / `right`、`home`、`end`、`pageup`、`pagedown`、`insert`、`delete`、`f1`–`f20` | `Input::Key`，`Key` 取同名变体，带全部修饰键 |
   | 带 ⌃ 的字符键、⌃空格 | `Input::Key(Key::Char(..) / Key::Space)`，`text` 为 None |
   | 带 ⌥ 的字符键 | 不处理：交给输入法，输入法提交 ⌥ 组出的字符（如 ⌥e e → é）。`Option` 作为 Meta 见「未决问题」1 |
   | 其余可打印字符（含 Shift、空格） | 不处理：经输入法提交，见下一步 |

   `is_held` 为 true 时 `KeyAction::Repeat`；`on_key_up` 对按下时处理过的键发 `KeyAction::Release`（只有程序开了 kitty 键盘协议的事件报告时才会被编码）。GPUI 的 `alt` 是 `Mods::option`，`platform` 是 `Mods::command`。
3. 输入法（`EntityInputHandler`，自建元素在 paint 里有焦点时用 `window.handle_input` 注册）：
   - `replace_text_in_range`：清除组字，文字非空时发 `Input::Text`。
   - `replace_and_mark_text_in_range`：记下组字的文字和选中范围，不发给终端；在光标格子处画出（`Theme::background` 底、带下划线，宽度超出网格时裁掉）。
   - `unmark_text`：清除组字。`marked_text_range` 在组字时为 Some，GPUI 据此把后续按键先交给输入法。
   - `bounds_for_range`：返回光标格子的范围，候选窗口出现在光标处。`text_for_range`、`selected_text_range` 返回空：终端的内容不交给输入法。
- 发出 `Key`、`Text`、`Paste` 之前，视口不在底部时（`scroll.offset + rows < total`）先发 `ScrollTo(Bottom)`。
- 焦点：`cx.on_focus` / `cx.on_blur` 发 `Input::Focus(true / false)` 并重画（光标与选区色变化）。窗口失去激活时按失去焦点处理。

### 复制与粘贴

- `Copy`：帧里有选区时调用 `copy_selection(id, token, reply)`；`reply` 只把文字送进 `oneshot`，主线程的前台任务收到后 `cx.write_to_clipboard`。没有选区时不做事。
- `Paste`：`cx.read_from_clipboard()` 的文字部分（没有文字时不做事）发 `Input::Paste`。控制字符的过滤、bracketed paste 和不追加回车都在 `terminal`（[terminal 设计「输入」](terminal.md#输入)）。剪贴板里只有图片时，⌘V 不发任何东西；agent 自己的图片粘贴（如 ⌃V）是普通按键，照常交给终端。
- `SelectAll`、`ClearScrollback`：发同名 `Input`。

### 鼠标与滚轮

鼠标监听由自建元素在 paint 里按网格范围注册（`window.on_mouse_event`），位置换成格子（列、行从 0 起，夹在网格内）：

- 按下：先取焦点，再发 `Input::Mouse(Down)`，带 `click_count`（2 选词、3 选行由 `terminal` 处理）。鼠标跟踪与选区的取舍（Shift 强制选区）由 `terminal` 按模式决定。
- 移动：按着按钮时发 `Drag`；没按按钮时只在 `Frame::modes.mouse_tracking` 为 true 时发 `Move`。只在所在格子变化时发。
- 拖到网格上方或下方时，每 50 ms 发一次 `Scroll(∓1)` 和夹在边缘的 `Drag`，直到松开或回到网格内（前台定时任务，松开即停）。
- 松开：`Up`。
- 滚轮：`ScrollDelta::Lines` 直接用；`Pixels` 除以行高、累计到 `scroll_rest`，满一行发一次 `Input::Scroll`，方向换成 `ScrollInput::lines` 的约定（正数向下）。

## 性能

对照[性能预算](tech-research.md#性能预算)：

| 预算 | 本 crate 的影响 | 怎样满足 |
|---|---|---|
| 冷启动 20 ms、空闲内存 60 MB | 没有 | 本 crate 没有全局状态、不注册任何东西；`TerminalPane` 只在 Terminal 页显示时存在 |
| 滚动与输入 120 fps | 按键到回显、持续输出时的每一帧 | 按键只调用一次 `input`（入队）；每帧只排版变了的行；绘制是每格最多一个字形与几个矩形 |

- 每帧主线程的工作：`refresh` 是一次 `Arc` 复制；prepaint 对缓存里没有的行各调用一次 `layout_line` 并放置字形；paint 对 120×40 的网格最多约 4,800 个字形和每个样式段一个矩形。帧本身在 `terminal` 的线程上建好，视图不解析。
- 内存：行缓存最多两帧的行（120 列 × 80 行的字形表约 0.3 MB），视图 drop 时释放。没有打开的 Terminal 页时为 0。
- 本 crate 负责 `app/input_frames`（[bench 设计「预算表」](bench.md#预算表)），并新增下表的条目（`bench` 的预算表，实现 PR 加入）：

| ID | 预算 | 测量 |
|---|---|---|
| `app/input_frames` | 已有：每帧工作时间最大值 ≤ 8.3 ms，且相邻帧间隔都 ≤ 1.5 个刷新周期 | 已有的场景；profile 的回显程序见 terminal 设计「对其他 crate 的要求」第 7 项 |
| `app/terminal_output_frames` | 同上 | 同一个 profile，假的 agent 连续 2 秒全速输出每行 120 字符、每 8 个字符换一次 24 位色的文本；Terminal 页可见，记录这期间的帧 |
| `terminal_view/snap_full` | 中位数 ≤ 300 µs | 进程内、只在 macOS 编译：120×40 全部行不在缓存里，用合成的 `LineLayout` 放置字形、生成绘制列表（底色、装饰、方框图元），不开窗口 |

`app/terminal_output_frames` 用已有的「滚动与输入」预算，不改任何预算。

## 界面

- 位置：Conversation 的 Terminal 页（收起层）。宿主把 `TerminalPane` 放在状态行下面，占满页剩下的部分（检查器 300pt 宽时约 40 列，见 conversation 设计「未决问题」5）。
- 用到的 `ui`：`Palette`、终端配色（新增）、`mono_metrics`、`TextRole::Code`、`semantic`。没有动画；没有自定义颜色和字体。
- Fluent 键（`locales/{en,zh-CN}/terminal.ftl`，`keys::terminal::*`）：

  | 键 | en | zh-CN |
  |---|---|---|
  | `terminal-grid` | Terminal | 终端 |

  它只用作语义节点与无障碍的标签。状态、错误和按钮的文字都在 `conversation`。
- 语义节点：

  | ID | 角色 | 内容 |
  |---|---|---|
  | `terminal.grid` | `Group` | `Label::key` = `terminal-grid`；`State::focused`；`attrs`：`cols`、`rows`、`state`（`starting` / `running` / `exited` / `failed` / `closed`）、`alt`（备用屏幕）、`scrolled`（视口不在底部） |

  用 `semantic_interactive` 包装根元素，挂在宿主的 `conversation.terminal` 之下（声明的嵌套）。不记录屏幕的文字、标题、选区、组字的文字和剪贴板。
- Action 与快捷键（上下文 `terminal`，只写一层）：

  | Action | 键 | 处理 |
  |---|---|---|
  | `terminal_view::Copy` | ⌘C | 「复制与粘贴」 |
  | `terminal_view::Paste` | ⌘V | 同上 |
  | `terminal_view::SelectAll` | ⌘A | `Input::SelectAll` |
  | `terminal_view::ClearScrollback` | — | `Input::ClearScrollback`（右键菜单另开 issue） |
  | `NoAction` | Esc | 挡住 `workspace::Escape`，Esc 交给终端 |

  全部 Action 名稳定、可枚举（探针按名字触发）。⌘K 是命令面板、⌘[ 是返回，都不绑定，照常冒泡。

## 状态与异常

| 情况 | 表现 |
|---|---|
| 还没有第一帧 | 只画 `Theme::background`；`watch` 之后 `terminal` 立即生成一帧完整的，通常在下一帧到达 |
| 进程启动中 / 启动失败 | 画 `terminal` 给的帧（启动失败时为空白）；原因由宿主的状态行显示 |
| 进程已退出 | 最后的画面保留，可以滚动、选择、复制；按键与粘贴被 `terminal` 丢弃 |
| 终端已关闭（`frame(id)` 为 None） | 只画底色；宿主随后 drop 视图 |
| `dyn Terminals` 不在（`get()` 为 None） | 不发输入，保留最后一帧；宿主按 conversation 设计显示不可用 |
| 区域不够 20 列 | 按 20 列绘制，裁掉右侧 |
| 剪贴板没有文字 | ⌘V 不做事 |
| 复制的回调没有被调用（宿主已停用） | 前台任务随 `oneshot` 的发送端 drop 而结束，不写剪贴板 |

## 日志与诊断

规则见 [ADR 0046](../adr/0046.md) 与 [logging 设计「隐私」](logging.md#隐私)。

- 默认记录：没有。本 crate 的操作没有失败路径：输入入队不会失败，剪贴板没有文字是正常情况；终端的启动、退出、读写错误都由 `terminal` 记录。
- 详细级别：
  - debug `terminal view frames`：有帧绘制时每秒最多一条，`term`、`frames`、`rows_laid_out`（新排版的行数）、`glyphs`、`max_prepaint_us`、`max_paint_us`。
  - debug `terminal view resized`：`term`、`cols`、`rows`。
- 关联 ID：主线程上没有作用域，记录只带 `term`。
- 不记录：屏幕的文字、标题、选区与复制的文字、粘贴的文字、按键与输入法的文字（包括组字中的文字）、剪贴板的内容、颜色值。只记计数和耗时。
- 金丝雀测试 `logs_and_nodes_hold_no_terminal_content`：trace 级别下，用含金丝雀文字的帧绘制，输入法组字与提交含金丝雀的文字，剪贴板含金丝雀并 ⌘V，然后对日志（`testing::capture()`）和这一帧的语义记录都 `assert_absent`。

## 测试

- 视图测试用 `#[gpui::test]`。服务用假的 `dyn Terminals`（`testing::FakeTerminals`：记录每次 `input`、`watch`、`copy_selection`，按测试给的 `Frame` 应答），经 `kernel::testing::TestKernel::mock` 提供，测试插件取得 `ServiceRef` 和 token。不依赖 `lectern-terminal`，不启动进程。帧用 `testing::frame(...)` 按行的文字和样式构造。
- 按键（`keys_*`）：「键盘」表里的每一行，发出的 `Input::Key` 的 `Key`、修饰键和 `KeyAction`；⇧PageUp 等发 `ScrollTo`；⌃C 发 `Key(Char('c'))` 带 control；可打印字符不发 `Key`。
- 快捷键的优先级：`escape_reaches_the_terminal`（外层有 KeyContext `Workspace` 和绑定在它上面的 `escape`，与 workspace 相同；Esc 只发出 `Key(Escape)`，外层的 Action 没有被调用）；`cmd_keys_bubble`（⌘W 到达外层的处理函数，终端没有收到输入）。
- 输入法（`ime_*`）：`simulate_input` 发 `Text`；组字期间不发输入、组字的文字画在光标格子；提交后发一次 `Text`；`bounds_for_range` 是光标格子。
- 复制与粘贴（`copy_*`、`paste_*`）：有选区时调用 `copy_selection` 并写剪贴板；没有选区时不调用；⌘V 发 `Paste`；视口不在底部时先发 `ScrollTo(Bottom)`。
- 尺寸（`grid_size_*`、`resize_*`）：纯函数的取整、四边留白、最少 20 列；同一尺寸只发一次 `Resize`。
- 颜色（`theme_*`）：两种外观下 `theme` 的 insta 快照；切换外观（`ui` 的固定外观）后只发一次 `SetTheme`。
- 鼠标与滚轮（`mouse_*`、`wheel_*`）：像素到格子的换算；双击、三击的 `clicks`；不开鼠标跟踪时不发 `Move`；拖出网格时自动滚动、松开即停；像素滚动累计成整行，方向固定。
- 焦点（`focus_*`）：取得与失去焦点各发一次 `Focus`；失去焦点后光标为空心方框（检查绘制列表）。
- 字形放置（`snap_*`，纯函数，用合成的 `LineLayout`）：ASCII、CJK 宽字符居中于两列、组合符号跟随基字、表情、ZWJ 序列、行尾空格。
- 方框与块元素（`box_*`）：`box_glyph` 对处理范围内每个码位的图元做 insta 快照，范围外的返回 None。
- 语义树（`semantic_tree`）：insta 快照，含 `attrs`；断言没有帧里的文字。
- 只能在 macOS 上运行：本 crate 依赖 GPUI，测试只在 macOS CI 上跑；`terminal_view/snap_full` 只在 macOS 编译。

## 对其他 crate 的要求

1. `ui`：新增终端配色（与「代码配色」同类，ADR 0015 的颜色映射集中在 `ui`）：

   ```rust
   /// 终端的颜色，都来自 `Palette`。纯函数，渲染时可调用。
   pub fn terminal_colors(palette: &Palette) -> TerminalColors;
   pub struct TerminalColors {
       pub foreground: Hsla,   // label
       pub background: Hsla,   // control_background
       pub cursor: Hsla,       // label
       pub ansi: [Hsla; 16],
   }
   ```

   | ANSI | 浅色 | 深色 |
   |---|---|---|
   | 0 黑 | `label` | `tertiary_label` |
   | 1 红、2 绿、3 黄、4 蓝、5 品红、6 青 | `system(Red / Green / Yellow / Blue / Purple / Cyan)` | 同左 |
   | 7 白 | `tertiary_label` | `label` |
   | 8 亮黑 | `secondary_label` | `secondary_label` |
   | 9–14 | 同 1–6 | 同 1–6 |
   | 15 亮白 | 同 7 | 同 7 |

   派生规则：1–6 叠在 `background` 上对比度不足 3:1 时（浅色外观的绿、黄、青），向 `label` 每次混合 5%，直到不低于 3:1；与 diff 底色一样是按规则派生，不写死颜色值。检查：`terminal_colors_contrast`（两种外观下 1–6 对 `background` 不低于 3:1）、`terminal_colors_distinct`（1–6 两两之间 ΔE（CIE76）不小于 12）。某一行过不了检查时换一种系统色，写进 ui 设计的「与设计的差异」。
2. `conversation`（[conversation 设计](https://github.com/SoloJiang/lectern-app/pull/262)「对其他 crate 的要求」E）。与 E 的对应和差异：
   - `TerminalPane::new` 多一个参数 `token: ActivationToken`（终端插件这次激活的 token）：`Terminals::copy_selection` 要调用方的 token，而 `ServiceRef` 里的是提供方的 token，不对外。
   - `refresh` 在收到这个终端的 `TerminalEvent::Frame`（E 写的 `Output`）和任何 `TerminalEvent::Listed` 时调用。
   - `TerminalPane` 只在 Terminal 页显示时存在：切到 Messages 页、检查器隐藏时 drop，再显示时用同一个 `TerminalId` 新建。视图持有 `FrameInterest`，所以页不可见时 `terminal` 不建帧、主线程不重画（terminal 设计「对其他 crate 的要求」第 4 项）。
   - 恢复之前用 `terminal_view::theme` 和 `terminal_view::grid_size` 构造 `OpenSpec`（「接口」的宿主用法）；恢复之后把焦点交给 `focus_handle()`。
   - 终端插件的 `KeyBindings` 贡献里加上 `terminal_view::key_bindings()`。
   - 「获得焦点时不带 ⌘ 的键（包括 Esc）交给终端」按「键盘」一节实现；⌘C、⌘V 之外另有 ⌘A（全选，macOS 终端的习惯），其余带 ⌘ 的组合键冒泡。
   - 状态行、启动失败的原因（`FailReason`）和退出码由 Terminal 页显示，视图不显示文字。
3. `i18n`：新增 `locales/{en,zh-CN}/terminal.ftl`，内容是「界面」的键，生成 `keys::terminal::*`。
4. `app`：测试 `terminal_keys_reach_the_terminal`：用 `app` 装配后的完整快捷键表，对 Terminal 页的上下文栈（`Workspace`、`project`、`Inspector`、`conversation`、`conversation-terminal`、`terminal`），每个不带 ⌘ 的已绑定按键经 `Keymap::bindings_for_input` 得到的结果为空或只有 `terminal_view` 的 Action。以后有人在祖先上下文或全局加了不带 ⌘ 的绑定，这个测试失败，提示在 `key_bindings()` 里给同一个键加一条 `NoAction`。
5. `bench`：预算表加入「性能」里的 `app/terminal_output_frames` 和 `terminal_view/snap_full`。两个帧基准在终端进入 `Running` 之后才开始注入或计时（`Starting` 期间的按键被 `terminal` 丢弃）；夹具会话要出现在 `agents` 的会话列表里，`cwd` 在夹具项目内。

## 未决问题

1. **Option 键是否作为 Meta。** 选项：(a) Option 按 macOS 的习惯输入特殊字符（⌥e e → é，德语等键盘布局用 Option 输入 `@`、`[`、`]`），⌥ 加方向键、退格等非字符键仍带 `option` 交给终端编码（默认，与 Terminal.app 的默认设置相同）；(b) Option 作为 Meta：⌥ 加字符键发 `Key` 带 `option`、`text` 为 None，由终端编码成 Esc 前缀，readline 的 ⌥B、⌥F 可用，但这些键盘布局上无法输入那几个字符；(c) 以后在设置里提供开关，P1 先用 (a)。默认 (a)。
2. **程序没开 bracketed paste 时，多行粘贴是否先确认。** 选项：(a) 不确认（默认）：Terminal 页只运行 agent，`claude`、`codex` 都开 bracketed paste，粘贴的换行不会直接执行；(b) 确认，弹出一次对话框。选 (b) 需要 `ui` 的对话框和本 crate 的 Fluent 键。

## 已知限制

1. 按住字母键时按系统的「按住显示重音符号」设置弹出重音菜单，而不是连续输入（GPUI 的输入法处理器固定开启这一行为）；方向键、退格等照常连发。
2. 回退字体里比单元宽的字形（部分符号、未知表情）会压到右边的格子。
3. 程序开了 kitty 键盘协议的「所有键都报告为转义序列」时，可打印字符仍经输入法以文字送达，不带按键事件。
4. VoiceOver 读不到终端的内容：语义节点只有尺寸和状态（探针的隐私约束），朗读另开 issue。
5. UI 探针在 kernel 静止之后读语义树时，最后一帧可能还没画（terminal 设计「已知限制」6），探针要多等一帧。

## 验收

**第 1 步：`ui` 终端配色与 `i18n` 的键（`ui`、`i18n` 的 PR，`Refs #5`）。**

- `ui::terminal_colors` 按「对其他 crate 的要求」第 1 项实现；`just test -p lectern-ui` 里 `terminal_colors_contrast`、`terminal_colors_distinct` 和两种外观的快照通过。
- `locales/en/terminal.ftl` 与 `locales/zh-CN/terminal.ftl` 有 `terminal-grid`；`just test -p lectern-i18n` 通过。

**第 2 步：`TerminalPane`（本 crate 的 PR，`Refs #5`、`Refs #12`；前置：terminal 设计的第 1 步 `services::terminal`、本文第 1 步）。**

- `just test -p lectern-terminal-view`（macOS）里 `keys_*`、`escape_reaches_the_terminal`、`cmd_keys_bubble`、`ime_*`、`copy_*`、`paste_*`、`grid_size_*`、`resize_*`、`theme_*`、`mouse_*`、`wheel_*`、`focus_*`、`snap_*`、`box_*`（快照逐个看过）、`semantic_tree`、`logs_and_nodes_hold_no_terminal_content` 通过。
- `Cargo.toml` 没有 `plugin = true`，`python3 scripts/check_architecture.py` 通过；`cargo tree -p lectern-terminal-view --edges normal` 里没有 `lectern-terminal`。
- `rg 'force_width: Some|Hsla::from|rgb\(|hsla\(' crates/terminal_view/src` 没有结果（不强制字宽，不在视图里写颜色）。
- `just bench terminal_view/` 里 `terminal_view/snap_full` 在预算内。

**第 3 步：装配与真机验证（`conversation` 第 3 步与 `app` 的 PR 里完成；本 crate 的条件）。**

- `app` 的 `terminal_keys_reach_the_terminal` 通过。
- `app/input_frames` 与 `app/terminal_output_frames` 在预算内。
- 在 macOS 上运行发布构建，两种外观各一遍：恢复一个 Claude Code 会话，输入框的圆角边框上下相接；中文提示与英文对齐网格；用拼音输入法输入中文，候选窗口在光标处；⌘C、⌘V、⌘A 可用，Esc 交给 agent（检查器不关闭）；拖动选择、双击选词；滚轮翻看历史后输入，视口回到底部。结果写进「实际验证」。
- issue #5 由 `terminal`、`terminal_view` 里最后合并的那个 PR 写 `Closes #5`，其余写 `Refs #5`。
