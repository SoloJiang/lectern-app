//! Frames from hand-written VT byte streams (design 「测试」: `frame_*`,
//! `row_reuse_*`, `copy_*`, `theme_*`).
#![cfg(feature = "test-support")]

use std::fmt::Write as _;
use std::sync::Arc;

use lectern_services::terminal::{
    CellStyle, ColorScheme, Frame, Input, MouseButton, MouseInput, MouseKind, Rgb, ScrollInput,
    StyleFlags, Underline,
};
use lectern_terminal::testing::{self, Engine};

fn engine(cols: u16, rows: u16) -> Engine {
    Engine::new(testing::size(cols, rows), testing::theme())
}

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}.vt", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

fn style(s: &CellStyle) -> String {
    let mut out = format!("fg {}", hex(s.fg));
    if let Some(bg) = s.bg {
        write!(out, " bg {}", hex(bg)).unwrap();
    }
    for (bit, name) in [
        (StyleFlags::BOLD, "bold"),
        (StyleFlags::ITALIC, "italic"),
        (StyleFlags::FAINT, "faint"),
        (StyleFlags::STRIKETHROUGH, "strike"),
        (StyleFlags::OVERLINE, "overline"),
        (StyleFlags::INVISIBLE, "invisible"),
    ] {
        if s.flags.0 & bit != 0 {
            write!(out, " {name}").unwrap();
        }
    }
    if s.underline != Underline::None {
        write!(out, " underline {:?}", s.underline).unwrap();
        if let Some(c) = s.underline_color {
            write!(out, " {}", hex(c)).unwrap();
        }
    }
    out
}

/// Each row's text, the columns of its clusters (only where they differ from
/// one cluster per column) and its style runs. Row keys are left out: the
/// standard hasher may change between Rust releases.
fn describe(frame: &Frame) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "size {}x{}  colors fg {} bg {} cursor {}",
        frame.size.cols,
        frame.size.rows,
        hex(frame.colors.foreground),
        hex(frame.colors.background),
        hex(frame.colors.cursor)
    )
    .unwrap();
    match frame.cursor {
        Some(c) => writeln!(
            out,
            "cursor {},{} {:?}{}{}",
            c.col,
            c.row,
            c.shape,
            if c.blinking { " blinking" } else { "" },
            if c.wide { " wide" } else { "" }
        )
        .unwrap(),
        None => writeln!(out, "cursor hidden").unwrap(),
    }
    writeln!(
        out,
        "scroll {}/{}  modes {:?}",
        frame.scroll.offset, frame.scroll.total, frame.modes
    )
    .unwrap();
    for (y, row) in frame.rows.iter().enumerate() {
        let wrapped = if row.wrapped { " (wrapped)" } else { "" };
        writeln!(out, "{y:2}: {:?}{wrapped}", &*row.text).unwrap();
        let simple = row
            .clusters
            .iter()
            .enumerate()
            .all(|(i, c)| usize::from(c.col) == i && c.byte as usize == i && !c.wide);
        if !simple {
            let clusters: Vec<String> = row
                .clusters
                .iter()
                .map(|c| format!("{}@{}{}", c.byte, c.col, if c.wide { "w" } else { "" }))
                .collect();
            writeln!(out, "    clusters {}", clusters.join(" ")).unwrap();
        }
        for run in &*row.runs {
            writeln!(
                out,
                "    {:>3}..{:<3} {}",
                run.start,
                run.end,
                style(&run.style)
            )
            .unwrap();
        }
    }
    out
}

fn frame_of(name: &str, cols: u16, rows: u16) -> String {
    let mut e = engine(cols, rows);
    e.feed(&fixture(name));
    describe(&e.frame())
}

macro_rules! frame_tests {
    ($($test:ident: $name:literal $cols:literal x $rows:literal;)*) => {$(
        #[test]
        fn $test() {
            insta::assert_snapshot!(frame_of($name, $cols, $rows));
        }
    )*};
}

frame_tests! {
    frame_ansi16: "ansi16" 30 x 3;
    frame_color256: "color256" 30 x 2;
    frame_truecolor: "truecolor" 30 x 2;
    frame_styles: "styles" 40 x 3;
    frame_inverse: "inverse" 36 x 2;
    frame_wide: "wide" 16 x 3;
    frame_combining: "combining" 12 x 2;
    frame_emoji: "emoji" 24 x 2;
    frame_tabs: "tabs" 24 x 3;
    frame_softwrap: "softwrap" 20 x 4;
    frame_alt_screen: "alt_screen" 20 x 3;
    frame_cursor: "cursor" 12 x 3;
    frame_clear: "clear" 12 x 3;
    frame_osc: "osc" 12 x 2;
    frame_sync: "sync" 16 x 2;
}

#[test]
fn frame_runs_cover_every_column() {
    for name in [
        "ansi16",
        "color256",
        "truecolor",
        "styles",
        "inverse",
        "wide",
        "combining",
        "emoji",
        "tabs",
        "softwrap",
    ] {
        let mut e = engine(20, 4);
        e.feed(&fixture(name));
        let frame = e.frame();
        assert_eq!(frame.rows.len(), 4, "{name}");
        for row in frame.rows.iter() {
            let mut col = 0;
            for run in &*row.runs {
                assert_eq!(run.start, col, "{name}: runs are contiguous");
                assert!(run.end > run.start);
                col = run.end;
            }
            assert_eq!(col, 20, "{name}: runs cover 0..cols");
            let mut last = None;
            for c in &*row.clusters {
                assert!(row.text.is_char_boundary(c.byte as usize));
                assert!(last.is_none_or(|(b, col)| c.byte > b && c.col > col));
                last = Some((c.byte, c.col));
            }
        }
    }
}

#[test]
fn frame_osc_sets_title_and_bell_counts() {
    let mut e = engine(12, 2);
    e.feed(&fixture("osc"));
    assert_eq!(e.title().as_deref(), Some("window title"));
    let effects = e.take_effects();
    assert!(effects.title_changed);
    e.feed(b"\x07\x07");
    assert_eq!(e.take_effects().bells, 2);
}

#[test]
fn row_reuse_one_changed_row_keeps_the_others() {
    let mut e = engine(20, 4);
    e.feed(b"one\r\ntwo\r\nthree\r\n");
    let first = e.frame();
    e.feed(b"\x1b[2;1Hdos");
    let second = e.frame();
    assert_eq!(&*second.rows[1].text, "dos");
    for y in [0, 2, 3] {
        assert!(Arc::ptr_eq(&first.rows[y], &second.rows[y]), "row {y}");
    }
    assert!(!Arc::ptr_eq(&first.rows[1], &second.rows[1]));
    assert_ne!(first.rows[1].key, second.rows[1].key);
    assert_eq!(second.generation, first.generation + 1);
}

#[test]
fn row_reuse_cursor_move_keeps_every_row() {
    let mut e = engine(20, 4);
    e.feed(b"one\r\ntwo\r\nthree");
    let first = e.frame();
    e.feed(b"\x1b[1;1H");
    let second = e.frame();
    for y in 0..4 {
        assert!(Arc::ptr_eq(&first.rows[y], &second.rows[y]), "row {y}");
    }
    assert_eq!(second.cursor.map(|c| (c.col, c.row)), Some((0, 0)));
}

#[test]
fn row_reuse_selection_changes_no_key() {
    let mut e = engine(20, 3);
    e.feed(b"hello world\r\nsecond line");
    let first = e.frame();
    e.input(Input::SelectAll);
    let second = e.frame();
    assert!(!second.selection.is_empty());
    for y in 0..3 {
        assert_eq!(first.rows[y].key, second.rows[y].key, "row {y}");
    }
    e.input(Input::ClearSelection);
    assert!(e.frame().selection.is_empty());
}

#[test]
fn row_reuse_same_content_has_same_key() {
    let mut e = engine(20, 3);
    e.feed(b"\x1b[31msame\x1b[0m\r\n\x1b[31msame\x1b[0m\r\nother");
    let f = e.frame();
    assert_eq!(f.rows[0].key, f.rows[1].key);
    assert_ne!(f.rows[0].key, f.rows[2].key);
}

fn mouse(kind: MouseKind, col: u16, row: u16, clicks: u8) -> Input {
    Input::Mouse(MouseInput {
        kind,
        button: Some(MouseButton::Left),
        mods: Default::default(),
        col,
        row,
        x: f32::from(col) * 8.0 + 4.0,
        y: f32::from(row) * 16.0 + 8.0,
        clicks,
    })
}

#[test]
fn copy_single_line() {
    let mut e = engine(20, 3);
    e.feed(b"hello world");
    e.input(mouse(MouseKind::Down, 0, 0, 1));
    e.input(mouse(MouseKind::Drag, 4, 0, 1));
    assert_eq!(e.copy_selection().as_deref(), Some("hello"));
    let frame = e.frame();
    assert_eq!(frame.selection.len(), 1);
    assert_eq!((frame.selection[0].start, frame.selection[0].end), (0, 5));
}

#[test]
fn copy_across_a_soft_wrap_has_no_newline() {
    let mut e = engine(10, 3);
    e.feed(b"0123456789abcde");
    e.input(mouse(MouseKind::Down, 0, 0, 1));
    e.input(mouse(MouseKind::Drag, 4, 1, 1));
    assert_eq!(e.copy_selection().as_deref(), Some("0123456789abcde"));
}

#[test]
fn copy_across_a_hard_newline_keeps_it() {
    let mut e = engine(10, 3);
    e.feed(b"one\r\ntwo");
    e.input(mouse(MouseKind::Down, 0, 0, 1));
    e.input(mouse(MouseKind::Drag, 2, 1, 1));
    assert_eq!(e.copy_selection().as_deref(), Some("one\ntwo"));
}

#[test]
fn copy_wide_characters() {
    let mut e = engine(10, 2);
    e.feed("中文ab".as_bytes());
    e.input(mouse(MouseKind::Down, 0, 0, 1));
    e.input(mouse(MouseKind::Drag, 4, 0, 1));
    assert_eq!(e.copy_selection().as_deref(), Some("中文a"));
}

#[test]
fn copy_word_and_line_clicks() {
    let mut e = engine(20, 2);
    e.feed(b"git status --short");
    e.input(mouse(MouseKind::Down, 5, 0, 2));
    assert_eq!(e.copy_selection().as_deref(), Some("status"));
    e.input(mouse(MouseKind::Down, 5, 0, 3));
    assert_eq!(e.copy_selection().as_deref(), Some("git status --short"));
}

#[test]
fn copy_without_selection_is_none() {
    let mut e = engine(10, 2);
    e.feed(b"text");
    assert_eq!(e.copy_selection(), None);
    e.input(mouse(MouseKind::Down, 1, 0, 1));
    e.input(Input::ClearSelection);
    assert_eq!(e.copy_selection(), None);
}

#[test]
fn copy_select_all() {
    let mut e = engine(10, 3);
    e.feed(b"one\r\ntwo");
    e.input(Input::SelectAll);
    assert_eq!(e.copy_selection().as_deref(), Some("one\ntwo"));
}

/// The reply to `ESC ] <n> ; ? ST`, e.g. "rgb:ffff/0000/0000".
fn color_reply(e: &mut Engine, query: &[u8]) -> String {
    e.take_pty_output();
    e.feed(query);
    String::from_utf8(e.take_pty_output()).unwrap()
}

#[test]
fn theme_color_queries_answer_with_the_theme() {
    let mut e = engine(10, 2);
    let fg = color_reply(&mut e, b"\x1b]10;?\x1b\\");
    assert!(fg.contains("rgb:e5e5/e5e5/e5e5"), "{fg:?}");
    let bg = color_reply(&mut e, b"\x1b]11;?\x1b\\");
    assert!(bg.contains("rgb:1010/1010/1010"), "{bg:?}");
    let red = color_reply(&mut e, b"\x1b]4;1;?\x1b\\");
    assert!(red.contains("rgb:cdcd/0000/0000"), "{red:?}");
}

#[test]
fn theme_color_scheme_report() {
    let mut e = engine(10, 2);
    // CSI ? 996 n: dark is 997;1.
    assert_eq!(color_reply(&mut e, b"\x1b[?996n"), "\x1b[?997;1n");
    // Mode 2031 on: a theme change reports the new scheme.
    e.feed(b"\x1b[?2031h");
    let mut light = testing::theme();
    light.scheme = ColorScheme::Light;
    e.input(Input::SetTheme(light));
    assert_eq!(e.take_pty_output(), b"\x1b[?997;2n");
}

#[test]
fn theme_set_theme_recolours_every_row() {
    let mut e = engine(12, 2);
    e.feed(b"\x1b[31mred\x1b[0m plain");
    let before = e.frame();
    let mut theme = testing::theme();
    theme.ansi[1] = Rgb { r: 1, g: 2, b: 3 };
    theme.foreground = Rgb { r: 9, g: 9, b: 9 };
    e.input(Input::SetTheme(theme));
    let after = e.frame();
    assert_eq!(after.rows[0].runs[0].style.fg, Rgb { r: 1, g: 2, b: 3 });
    assert_eq!(after.colors.foreground, Rgb { r: 9, g: 9, b: 9 });
    assert_ne!(before.rows[0].key, after.rows[0].key);
    assert_ne!(
        before.rows[1].key, after.rows[1].key,
        "blank rows change too"
    );
}

#[test]
fn scroll_moves_the_viewport_into_history() {
    let mut e = engine(10, 3);
    for i in 0..10 {
        e.feed(format!("line {i}\r\n").as_bytes());
    }
    let bottom = e.frame();
    assert_eq!(bottom.scroll.offset + 3, bottom.scroll.total);
    e.input(Input::Scroll(ScrollInput {
        lines: -2,
        col: 0,
        row: 0,
        mods: Default::default(),
    }));
    let up = e.frame();
    assert_eq!(up.scroll.offset + 2, bottom.scroll.offset);
    assert_eq!(&*up.rows[0].text, "line 6");
}
