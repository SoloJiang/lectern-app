//! 视图发给终端的输入、网格尺寸与颜色（terminal 设计「输入」）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Debug)]
pub enum Input {
    /// 一次按键。终端线程按程序当前的模式（DECCKM、kitty 键盘协议的标志）编码。
    Key(KeyInput),
    /// 输入法提交的文字、或不经按键编码的文字。原样以 UTF-8 写入。
    Text(Arc<str>),
    /// 粘贴（⌘V，以及审阅意见发回会话 #22）。终端线程先去掉 C0 控制字符（保留 `\t`、`\n`；
    /// ESC 也在其中）和 C1 控制字符（U+0080–U+009F），再交给 libghostty-vt 的
    /// `paste::encode`：程序开了 bracketed paste（mode 2004，`claude`、`codex` 都开）时包上
    /// `ESC[200~` / `ESC[201~`，没开时把 `\n` 换成 `\r`。不追加回车：是否提交由用户在终端里
    /// 按 ↵ 决定。是否需要用户确认（多行、未开 bracketed paste）由视图按 `Frame::modes`
    /// 决定，本 crate 收到就写。
    Paste(Arc<str>),
    /// 鼠标。程序开了鼠标跟踪、且没按 Shift 时编码给程序；否则用于选区
    /// （`clicks` 为 2 选词、3 选行）。
    Mouse(MouseInput),
    /// 滚轮或触控板。开了鼠标跟踪时编码为滚轮事件；在备用屏幕且没开鼠标跟踪时
    /// 换成上下方向键（alternate scroll）；否则滚动视口。
    Scroll(ScrollInput),
    ScrollTo(ScrollTarget),
    /// 视口的格子数与单元的像素尺寸。连续的多个只处理最后一个；进程在运行时同时调整
    /// PTY 的窗口大小（子进程收到 `SIGWINCH`），libghostty-vt 按新宽度重排滚动缓冲。
    Resize(GridSize),
    /// 视图获得或失去焦点。程序开了焦点事件（mode 1004）时写入 `ESC[I` / `ESC[O`。
    Focus(bool),
    SelectAll,
    ClearSelection,
    /// 清空滚动缓冲（不改视口的内容）。
    ClearScrollback,
    /// 外观变化时由视图发出：更新默认颜色，下一帧全部行重新解析颜色。
    SetTheme(Theme),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KeyInput {
    pub key: Key,
    pub mods: Mods,
    pub action: KeyAction,
    /// 这次按键产生的文字（GPUI 的 `key_char`），如 Shift+a 为 "A"。没有时为 None。
    pub text: Option<Arc<str>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    /// 不按 Shift 时这个键产生的字符，如 'a'、'1'、'/'。
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Space,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1 到 F20。
    F(u8),
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mods {
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MouseInput {
    pub kind: MouseKind,
    pub button: Option<MouseButton>,
    pub mods: Mods,
    /// 视口里的格子（列、行），从 0 开始。
    pub col: u16,
    pub row: u16,
    /// 视口里的像素位置（SGR 像素模式用）。
    pub x: f32,
    pub y: f32,
    pub clicks: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Down,
    Up,
    Drag,
    Move,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ScrollInput {
    /// 正数向下（看更新的内容）。视图把像素滚动量累计成整行再发。
    pub lines: i32,
    pub col: u16,
    pub row: u16,
    pub mods: Mods,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScrollTarget {
    Top,
    Bottom,
    PageUp,
    PageDown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    /// 单元的设备像素尺寸，用于应答程序的尺寸查询（CSI 14 / 16 / 18 t）。
    pub cell_width: u16,
    pub cell_height: u16,
}

/// 颜色由视图从 `ui` 的 `Palette`（系统语义色）推出，本 crate 不定义颜色值（ADR 0015）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// ANSI 0–15。16–255 由 `terminal` 按 xterm 的 6×6×6 色立方和灰阶算出。
    pub ansi: [Rgb; 16],
    /// 应答程序的明暗查询（mode 2031、`CSI ? 996 n`）。
    pub scheme: ColorScheme,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorScheme {
    Light,
    Dark,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}
