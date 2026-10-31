//! `box_*` (docs/design/terminal_view.md「方框与块元素」): the primitives of
//! every character in the handled ranges, shown as a raster of 16 by 16
//! dots in a snapshot (a thin line is 2 dots wide, `half_lines` is one dot,
//! an eighth of the cell two), and `None` outside the ranges.

use std::fmt::Write as _;

use crate::boxes::{Coord, Prim, box_glyph};

const SIZE: usize = 16;

/// The characters `box_glyph` draws.
fn handled() -> impl Iterator<Item = char> {
    [
        0x2500..=0x2503,
        0x250C..=0x254B,
        0x2550..=0x2570,
        0x2574..=0x259F,
    ]
    .into_iter()
    .flatten()
    .map(|code| char::from_u32(code).unwrap())
}

/// A coordinate in dots: eighths are 2 dots, a half line width 1.
fn dots(coord: Coord) -> i32 {
    i32::from(coord.eighths) * 2 + i32::from(coord.half_lines)
}

fn raster(prims: &[Prim]) -> String {
    let mut cells = vec![vec!['.'; SIZE]; SIZE];
    for prim in prims {
        match *prim {
            Prim::Rect { x0, y0, x1, y1 } => {
                for y in dots(y0).max(0)..dots(y1).min(SIZE as i32) {
                    for x in dots(x0).max(0)..dots(x1).min(SIZE as i32) {
                        cells[y as usize][x as usize] = '#';
                    }
                }
            }
            Prim::Shade { percent } => {
                let mark = match percent {
                    25 => '1',
                    50 => '2',
                    75 => '3',
                    other => panic!("shade {other}"),
                };
                for row in &mut cells {
                    row.fill(mark);
                }
            }
            Prim::Curve { from, ctrl, to } => {
                let point = |(x, y): (u8, u8)| (f32::from(x) * 2.0, f32::from(y) * 2.0);
                let (p0, p1, p2) = (point(from), point(ctrl), point(to));
                for step in 0..=64 {
                    let t = step as f32 / 64.0;
                    let u = 1.0 - t;
                    let x = u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0;
                    let y = u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1;
                    // A line of 2 dots centred on the point.
                    for dy in -1..=0 {
                        for dx in -1..=0 {
                            let (px, py) = (x.round() as i32 + dx, y.round() as i32 + dy);
                            if (0..SIZE as i32).contains(&px) && (0..SIZE as i32).contains(&py) {
                                cells[py as usize][px as usize] = '#';
                            }
                        }
                    }
                }
            }
        }
    }
    cells
        .into_iter()
        .map(|row| row.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn box_every_handled_character_in_a_snapshot() {
    let mut text = String::new();
    for ch in handled() {
        let prims =
            box_glyph(ch).unwrap_or_else(|| panic!("U+{:04X} has no primitives", ch as u32));
        assert!(!prims.is_empty(), "U+{:04X}", ch as u32);
        let _ = write!(text, "U+{:04X} {ch}\n{}\n\n", ch as u32, raster(prims));
    }
    insta::assert_snapshot!(text);
}

#[test]
fn box_outside_the_ranges_is_none() {
    let handled: Vec<char> = handled().collect();
    for code in 0x2400u32..0x2600 {
        let ch = char::from_u32(code).unwrap();
        assert_eq!(
            box_glyph(ch).is_some(),
            handled.contains(&ch),
            "U+{code:04X}"
        );
    }
    // The dashed lines, the diagonals and ordinary characters stay with the font.
    for ch in [
        '\u{2504}', '\u{250B}', '\u{254C}', '\u{254F}', '\u{2571}', '\u{2573}',
    ] {
        assert_eq!(box_glyph(ch), None, "U+{:04X}", ch as u32);
    }
    for ch in ['a', ' ', '-', '|', '\u{25a0}', '中'] {
        assert_eq!(box_glyph(ch), None, "{ch}");
    }
    // 4 lines, 64 junctions, 33 doubles and arcs, 44 halves and blocks.
    assert_eq!(handled.len(), 4 + 64 + 33 + 44);
}

fn rect(x0: u8, y0: u8, x1: u8, y1: u8) -> Prim {
    let at = |eighths| Coord {
        eighths,
        half_lines: 0,
    };
    Prim::Rect {
        x0: at(x0),
        y0: at(y0),
        x1: at(x1),
        y1: at(y1),
    }
}

#[test]
fn box_blocks_are_exact_fractions_of_the_cell() {
    assert_eq!(box_glyph('█'), Some(&[rect(0, 0, 8, 8)][..]));
    assert_eq!(box_glyph('▀'), Some(&[rect(0, 0, 8, 4)][..]));
    assert_eq!(box_glyph('▄'), Some(&[rect(0, 4, 8, 8)][..]));
    assert_eq!(box_glyph('▁'), Some(&[rect(0, 7, 8, 8)][..]));
    assert_eq!(box_glyph('▌'), Some(&[rect(0, 0, 4, 8)][..]));
    assert_eq!(box_glyph('▐'), Some(&[rect(4, 0, 8, 8)][..]));
    assert_eq!(box_glyph('▏'), Some(&[rect(0, 0, 1, 8)][..]));
    assert_eq!(box_glyph('▉'), Some(&[rect(0, 0, 7, 8)][..]));
    assert_eq!(box_glyph('▔'), Some(&[rect(0, 0, 8, 1)][..]));
    assert_eq!(box_glyph('▕'), Some(&[rect(7, 0, 8, 8)][..]));
    assert_eq!(box_glyph('░'), Some(&[Prim::Shade { percent: 25 }][..]));
    assert_eq!(box_glyph('▒'), Some(&[Prim::Shade { percent: 50 }][..]));
    assert_eq!(box_glyph('▓'), Some(&[Prim::Shade { percent: 75 }][..]));
    // The four quadrants make the whole cell.
    let quadrants: Vec<Prim> = ['▘', '▝', '▖', '▗']
        .into_iter()
        .flat_map(|ch| box_glyph(ch).unwrap().iter().copied())
        .collect();
    assert_eq!(quadrants.len(), 4);
}

/// The lines of the box meet at the middle of the cell edges, so that the
/// characters of neighbouring cells join.
#[test]
fn box_lines_reach_the_cell_edges() {
    let Some([Prim::Rect { x0, x1, .. }, Prim::Rect { x0: r0, x1: r1, .. }]) = box_glyph('─')
    else {
        panic!("─ is two arms")
    };
    let mut xs = [(x0.eighths, x1.eighths), (r0.eighths, r1.eighths)];
    xs.sort_unstable();
    assert_eq!(xs, [(0, 4), (4, 8)], "the left arm and the right arm");
    let prims = box_glyph('│').unwrap();
    let top = prims
        .iter()
        .filter(|prim| matches!(prim, Prim::Rect { y0, .. } if y0.eighths == 0))
        .count();
    let bottom = prims
        .iter()
        .filter(|prim| matches!(prim, Prim::Rect { y1, .. } if y1.eighths == 8))
        .count();
    assert_eq!((top, bottom), (1, 1));
}

#[test]
fn box_rounded_corners_join_the_middles_of_two_edges() {
    let corners = [
        ('╭', (4, 8), (8, 4)),
        ('╮', (4, 8), (0, 4)),
        ('╯', (4, 0), (0, 4)),
        ('╰', (4, 0), (8, 4)),
    ];
    for (ch, from, to) in corners {
        assert_eq!(
            box_glyph(ch),
            Some(
                &[Prim::Curve {
                    from,
                    ctrl: (4, 4),
                    to
                }][..]
            ),
            "{ch}"
        );
    }
}
