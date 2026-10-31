//! `snap_*` (docs/design/terminal_view.md「字形放置」): where `place` puts the
//! glyphs of a row, from synthetic shaping (`testing::synthetic_line`): the
//! shaped advance of a narrow character is 97 % of a cell and of a wide one
//! 5/3 of a cell, so the shaping never lands on the grid by itself.

use gpui::{FontId, GlyphId, LineLayout, ShapedGlyph, ShapedRun, point, px};

use crate::layout::{Kind, RowLayout, place};
use crate::testing::{grid_row, synthetic_line};

const CELL: f32 = 7.2;

fn laid_out(text: &str) -> RowLayout {
    let row = grid_row(text, 40, &[], false);
    place(&row, &synthetic_line(&row, px(CELL)), px(CELL))
}

/// `(column, x, glyph)` of every glyph item.
fn glyphs(layout: &RowLayout) -> Vec<(u16, f32, u32)> {
    layout
        .items
        .iter()
        .filter_map(|item| match item.kind {
            Kind::Glyph { glyph, .. } => Some((item.col, item.x.as_f32(), glyph.0)),
            Kind::Box(_) => None,
        })
        .collect()
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.001
}

#[test]
fn snap_ascii_is_on_the_grid() {
    let layout = laid_out("abcde");
    let glyphs = glyphs(&layout);
    assert_eq!(glyphs.len(), 5);
    for (ix, (col, x, glyph)) in glyphs.iter().enumerate() {
        assert_eq!(*col as usize, ix);
        // The shaping would have put it at 0.97 * ix * cell.
        assert!(close(*x, CELL * ix as f32), "{ix}: {x}");
        assert_eq!(*glyph, u32::from(b'a') + ix as u32);
    }
}

/// A wide character is centred in its two columns, and what follows it is
/// two columns on.
#[test]
fn snap_cjk_is_centred_in_two_columns() {
    let layout = laid_out("a中b");
    let glyphs = glyphs(&layout);
    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs[0].0, 0);
    assert_eq!(glyphs[1].0, 1);
    // The character advances 12 points (5/3 of 7.2): 14.4 - 12 = 2.4 spare.
    assert!(close(glyphs[1].1, CELL + 1.2), "{}", glyphs[1].1);
    assert_eq!(glyphs[2].0, 3);
    assert!(close(glyphs[2].1, 3.0 * CELL), "{}", glyphs[2].1);
}

#[test]
fn snap_wide_characters_in_a_row_stay_aligned() {
    let layout = laid_out("你好吗x");
    let glyphs = glyphs(&layout);
    let cols: Vec<u16> = glyphs.iter().map(|glyph| glyph.0).collect();
    assert_eq!(cols, vec![0, 2, 4, 6]);
    for (col, x, _) in &glyphs[..3] {
        assert!(close(*x, CELL * f32::from(*col) + 1.2), "{col}: {x}");
    }
    assert!(close(glyphs[3].1, 6.0 * CELL));
}

/// A combining mark follows its base letter with the offset the shaping gave.
#[test]
fn snap_combining_mark_follows_the_base() {
    let row = grid_row("ae\u{301}x", 40, &[], false);
    let mut line = synthetic_line(&row, px(CELL));
    // The accent sits 1.5 points to the right of the base letter.
    let base_x = line.runs[0].glyphs[1].position.x;
    line.runs[0].glyphs[2].position = point(base_x + px(1.5), px(0.0));
    let layout = place(&row, &line, px(CELL));
    let glyphs = glyphs(&layout);
    assert_eq!(glyphs.len(), 4);
    assert_eq!(
        (glyphs[1].0, glyphs[2].0),
        (1, 1),
        "the mark is in the base's column"
    );
    assert!(close(glyphs[1].1, CELL));
    assert!(close(glyphs[2].1, CELL + 1.5), "{}", glyphs[2].1);
    assert_eq!(glyphs[3].0, 2);
    assert!(close(glyphs[3].1, 2.0 * CELL));
}

#[test]
fn snap_emoji_is_wide_and_flagged() {
    let layout = laid_out("a😀b");
    let emoji: Vec<_> = layout
        .items
        .iter()
        .filter(|item| matches!(item.kind, Kind::Glyph { emoji: true, .. }))
        .collect();
    assert_eq!(emoji.len(), 1);
    assert_eq!(emoji[0].col, 1);
    assert!(close(emoji[0].x.as_f32(), CELL + 1.2));
    let b = layout.items.last().unwrap();
    assert_eq!(b.col, 3);
    assert!(close(b.x.as_f32(), 3.0 * CELL));
}

/// Man, woman and girl joined: one wide cluster of five characters. Its
/// glyphs keep the shaped offsets from the first one, and the next cluster
/// is two columns on.
#[test]
fn snap_zwj_sequence_is_one_cluster() {
    let text = "👨\u{200d}👩\u{200d}👧x";
    let row = grid_row(text, 40, &[], false);
    assert_eq!(row.clusters.len(), 2);
    assert!(row.clusters[0].wide);
    let layout = place(&row, &synthetic_line(&row, px(CELL)), px(CELL));
    let glyphs = glyphs(&layout);
    assert_eq!(glyphs.len(), 6, "five characters and the letter");
    for (col, _, _) in &glyphs[..5] {
        assert_eq!(*col, 0);
    }
    let first = glyphs[0].1;
    // The shaped offsets: the joiners do not advance, each emoji does by 12.
    assert!(close(glyphs[1].1 - first, 12.0), "{}", glyphs[1].1 - first);
    assert!(close(glyphs[2].1 - first, 12.0));
    assert_eq!(glyphs[5].0, 2);
    assert!(close(glyphs[5].1, 2.0 * CELL));
}

/// Spaces draw nothing, and the row has no trailing cells.
#[test]
fn snap_spaces_and_trailing_spaces_have_no_glyphs() {
    let layout = laid_out("a  b   ");
    let glyphs = glyphs(&layout);
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].0, 0);
    assert_eq!(glyphs[1].0, 3);
    assert!(close(glyphs[1].1, 3.0 * CELL));
    assert!(laid_out("      ").items.is_empty());
    assert!(laid_out("").items.is_empty());
}

#[test]
fn snap_box_characters_are_geometry() {
    let layout = laid_out("╭─x│");
    let kinds: Vec<_> = layout
        .items
        .iter()
        .map(|item| (item.col, item.kind))
        .collect();
    assert_eq!(kinds.len(), 4);
    assert!(kinds.contains(&(0, Kind::Box('╭'))));
    assert!(kinds.contains(&(1, Kind::Box('─'))));
    assert!(kinds.contains(&(3, Kind::Box('│'))));
    // The one glyph is the letter, on its column.
    let glyphs = glyphs(&layout);
    assert_eq!(glyphs.len(), 1);
    assert_eq!(glyphs[0].0, 2);
    // A character the font draws stays a glyph (dashed line, diagonal).
    let layout = laid_out("┄╱");
    assert_eq!(self::glyphs(&layout).len(), 2);
}

/// A glyph wider than its cell is placed where it falls, not scaled.
#[test]
fn snap_a_wide_fallback_glyph_is_not_scaled() {
    let row = grid_row("a→b", 40, &[], false);
    // The arrow comes from a fallback font and advances 1.5 cells.
    let glyph = |index: usize, id: u32, x: f32| ShapedGlyph {
        id: GlyphId(id),
        position: point(px(x), px(0.0)),
        index,
        is_emoji: false,
    };
    let line = LineLayout {
        font_size: px(12.0),
        width: px(CELL * 3.5),
        ascent: px(12.3),
        descent: px(3.3),
        runs: vec![ShapedRun {
            font_id: FontId(0),
            glyphs: vec![
                glyph(0, 1, 0.0),
                glyph(1, 2, CELL),
                glyph(1 + '→'.len_utf8(), 3, CELL * 2.5),
            ],
        }],
        len: row.text.len(),
    };
    let layout = place(&row, &line, px(CELL));
    let glyphs = glyphs(&layout);
    assert_eq!(glyphs.len(), 3);
    // The arrow stays at its column and overlaps the next one; "b" is on its own.
    assert!(close(glyphs[1].1, CELL));
    assert!(close(glyphs[2].1, 2.0 * CELL));
}
