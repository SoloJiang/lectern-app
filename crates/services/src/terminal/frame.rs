//! 视口的不可变快照（terminal 设计「帧」）。

use std::sync::Arc;

use super::{GridSize, Rgb, RunState};

#[derive(Clone, PartialEq, Debug)]
pub struct Frame {
    /// 每生成一帧加一，与 `TerminalEvent::Frame::generation` 一致。
    pub generation: u64,
    pub size: GridSize,
    /// 视口的行，从上到下，恰好 `size.rows` 行。内容没变的行在相邻两帧里是同一个 `Arc`。
    pub rows: Arc<[Arc<GridRow>]>,
    /// None：光标隐藏，或视口滚到了历史里、光标不在视口内。
    pub cursor: Option<Cursor>,
    /// 选区在视口里的部分，按行升序。不放进行里，选区变化不改变行的 `key`。
    pub selection: Arc<[SelectionSpan]>,
    pub colors: FrameColors,
    pub scroll: ScrollState,
    pub modes: Modes,
    pub state: RunState,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GridRow {
    /// 文字、字素簇的列位置、样式段和 `wrapped` 的 64 位哈希。相同内容的行 `key` 相同，
    /// 视图按它缓存排版结果。
    pub key: u64,
    /// 每个字素簇一段：空格子写成空格，宽字符的占位格不写，行尾的空格子去掉。
    pub text: Box<str>,
    /// 每个字素簇在 `text` 里的起始字节和所在的列，按字节升序。
    pub clusters: Box<[Cluster]>,
    /// 样式段，按列升序、首尾相接，覆盖 0..cols（行尾空格子的底色也在内）。
    pub runs: Box<[StyleRun]>,
    /// 这一行是上一行软换行的延续（复制时不插入换行）。
    pub wrapped: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cluster {
    pub byte: u32,
    pub col: u16,
    pub wide: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StyleRun {
    pub start: u16,
    pub end: u16,
    pub style: CellStyle,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CellStyle {
    /// 已解析的前景色：调色板序号、默认色、反显（SGR 7）都已换算。
    pub fg: Rgb,
    /// None：与 `FrameColors::background` 相同，视图不画底色。
    pub bg: Option<Rgb>,
    pub flags: StyleFlags,
    pub underline: Underline,
    /// None：与 `fg` 相同。
    pub underline_color: Option<Rgb>,
}

/// 位标志：BOLD、ITALIC、FAINT、STRIKETHROUGH、OVERLINE、INVISIBLE。闪烁不支持，按普通文字
/// 显示。
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub struct StyleFlags(pub u8);

impl StyleFlags {
    pub const BOLD: u8 = 1 << 0;
    pub const ITALIC: u8 = 1 << 1;
    pub const FAINT: u8 = 1 << 2;
    pub const STRIKETHROUGH: u8 = 1 << 3;
    pub const OVERLINE: u8 = 1 << 4;
    pub const INVISIBLE: u8 = 1 << 5;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Underline {
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
    pub blinking: bool,
    /// 光标所在的格子是宽字符：视图画两格宽。
    pub wide: bool,
}

/// 失去焦点时的空心方块由视图画，`terminal` 只给程序请求的形状（DECSCUSR）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

/// `end` 不含。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SelectionSpan {
    pub row: u16,
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScrollState {
    /// 滚动缓冲与屏幕的总行数。
    pub total: u32,
    /// 视口顶行在全部行里的序号；`offset + size.rows == total` 时在底部。
    pub offset: u32,
}

/// 程序开启的、视图需要知道的模式。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Modes {
    pub bracketed_paste: bool,
    pub mouse_tracking: bool,
    pub alt_screen: bool,
    pub focus_events: bool,
}
