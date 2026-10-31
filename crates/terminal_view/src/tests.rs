//! Tests of the terminal view (docs/design/terminal_view.md「测试」). The
//! view tests use `#[gpui::test]` with the fake `dyn Terminals`
//! (`testing::FakeTerminals`); the pure parts (`snap_*`, `box_*`, `cells`)
//! need no window.

mod box_glyphs;
mod cache;
mod clipboard;
mod focus;
mod ime;
mod keys;
mod logs;
mod mouse;
mod refresh;
mod size;
mod snap;
mod support;
mod theme;
mod tree;
