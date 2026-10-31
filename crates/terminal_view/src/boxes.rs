//! Box-drawing and block-element characters drawn as geometry
//! (docs/design/terminal_view.md「数据结构」「方框与块元素」). A font's glyphs
//! for these characters do not meet at the cell edges when the line height is
//! larger than the glyph, so the cells are filled by rectangles and curves.
//!
//! The coordinates are in cells. A `Coord` is a number of eighths of the cell
//! side plus a number of half line widths: the line width depends on the
//! device pixels, so a double line or the corner of two lines is described
//! relative to the line width (`half_lines` of 2 is one thin line width) and
//! stays pure data that a snapshot can show.
//!
//! Not covered, and drawn by the font: the dashed lines (U+2504–U+250B,
//! U+254C–U+254F) and the diagonals (U+2571–U+2573).

use std::sync::OnceLock;

/// One edge of a primitive: `eighths` of the cell width or height, plus
/// `half_lines` halves of the thin line width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Coord {
    pub eighths: u8,
    pub half_lines: i8,
}

/// A point of a curve, in eighths of the cell.
pub type Point8 = (u8, u8);

/// What fills (part of) a cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prim {
    /// A rectangle in the foreground colour.
    Rect {
        x0: Coord,
        y0: Coord,
        x1: Coord,
        y1: Coord,
    },
    /// The whole cell in the foreground colour at `percent` opacity.
    Shade { percent: u8 },
    /// A quarter circle of a rounded corner, stroked with the thin line
    /// width: a quadratic curve whose control point is the corner of the
    /// sharp box character.
    Curve {
        from: Point8,
        ctrl: Point8,
        to: Point8,
    },
}

/// The primitives of `ch`, or None for a character the font draws.
pub fn box_glyph(ch: char) -> Option<&'static [Prim]> {
    let code = ch as u32;
    if !handled(code) {
        return None;
    }
    let table = TABLE.get_or_init(build);
    Some(table[(code - FIRST) as usize].as_slice())
}

const FIRST: u32 = 0x2500;
const LAST: u32 = 0x259F;

static TABLE: OnceLock<Vec<Vec<Prim>>> = OnceLock::new();

fn handled(code: u32) -> bool {
    matches!(
        code,
        0x2500..=0x2503 | 0x250C..=0x254B | 0x2550..=0x2570 | 0x2574..=0x259F
    )
}

/// The weight of one arm of a line character.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    None,
    Light,
    Heavy,
    Double,
}

impl Arm {
    fn parse(letter: u8) -> Arm {
        match letter {
            b'l' => Arm::Light,
            b'h' => Arm::Heavy,
            b'd' => Arm::Double,
            _ => Arm::None,
        }
    }

    /// The lines of an arm across its axis: the offset of the centre line
    /// from the axis and its thickness, in half line widths.
    fn lines(self) -> &'static [(i8, i8)] {
        match self {
            Arm::None => &[],
            Arm::Light => &[(0, 2)],
            Arm::Heavy => &[(0, 4)],
            Arm::Double => &[(-2, 2), (2, 2)],
        }
    }

    /// How far this arm's crossing lines reach from the axis, in half line
    /// widths, to the outer face of the outermost line.
    fn half_extent(self) -> i8 {
        match self {
            Arm::None => 0,
            Arm::Light => 1,
            Arm::Heavy => 2,
            Arm::Double => 3,
        }
    }
}

/// Up, down, left and right arms, as `UDLR` letters: `.` none, `l` light,
/// `h` heavy, `d` double. Named by the Unicode names of the characters.
const LINES: &[(u32, &str)] = &[
    (0x2500, "..ll"),
    (0x2501, "..hh"),
    (0x2502, "ll.."),
    (0x2503, "hh.."),
    (0x250C, ".l.l"),
    (0x250D, ".l.h"),
    (0x250E, ".h.l"),
    (0x250F, ".h.h"),
    (0x2510, ".ll."),
    (0x2511, ".lh."),
    (0x2512, ".hl."),
    (0x2513, ".hh."),
    (0x2514, "l..l"),
    (0x2515, "l..h"),
    (0x2516, "h..l"),
    (0x2517, "h..h"),
    (0x2518, "l.l."),
    (0x2519, "l.h."),
    (0x251A, "h.l."),
    (0x251B, "h.h."),
    (0x251C, "ll.l"),
    (0x251D, "ll.h"),
    (0x251E, "hl.l"),
    (0x251F, "lh.l"),
    (0x2520, "hh.l"),
    (0x2521, "hl.h"),
    (0x2522, "lh.h"),
    (0x2523, "hh.h"),
    (0x2524, "lll."),
    (0x2525, "llh."),
    (0x2526, "hll."),
    (0x2527, "lhl."),
    (0x2528, "hhl."),
    (0x2529, "hlh."),
    (0x252A, "lhh."),
    (0x252B, "hhh."),
    (0x252C, ".lll"),
    (0x252D, ".lhl"),
    (0x252E, ".llh"),
    (0x252F, ".lhh"),
    (0x2530, ".hll"),
    (0x2531, ".hhl"),
    (0x2532, ".hlh"),
    (0x2533, ".hhh"),
    (0x2534, "l.ll"),
    (0x2535, "l.hl"),
    (0x2536, "l.lh"),
    (0x2537, "l.hh"),
    (0x2538, "h.ll"),
    (0x2539, "h.hl"),
    (0x253A, "h.lh"),
    (0x253B, "h.hh"),
    (0x253C, "llll"),
    (0x253D, "llhl"),
    (0x253E, "lllh"),
    (0x253F, "llhh"),
    (0x2540, "hlll"),
    (0x2541, "lhll"),
    (0x2542, "hhll"),
    (0x2543, "hlhl"),
    (0x2544, "hllh"),
    (0x2545, "lhhl"),
    (0x2546, "lhlh"),
    (0x2547, "hlhh"),
    (0x2548, "lhhh"),
    (0x2549, "hhhl"),
    (0x254A, "hhlh"),
    (0x254B, "hhhh"),
    (0x2550, "..dd"),
    (0x2551, "dd.."),
    (0x2552, ".l.d"),
    (0x2553, ".d.l"),
    (0x2554, ".d.d"),
    (0x2555, ".ld."),
    (0x2556, ".dl."),
    (0x2557, ".dd."),
    (0x2558, "l..d"),
    (0x2559, "d..l"),
    (0x255A, "d..d"),
    (0x255B, "l.d."),
    (0x255C, "d.l."),
    (0x255D, "d.d."),
    (0x255E, "ll.d"),
    (0x255F, "dd.l"),
    (0x2560, "dd.d"),
    (0x2561, "lld."),
    (0x2562, "ddl."),
    (0x2563, "ddd."),
    (0x2564, ".ldd"),
    (0x2565, ".dll"),
    (0x2566, ".ddd"),
    (0x2567, "l.dd"),
    (0x2568, "d.ll"),
    (0x2569, "d.dd"),
    (0x256A, "lldd"),
    (0x256B, "ddll"),
    (0x256C, "dddd"),
    (0x2574, "..l."),
    (0x2575, "l..."),
    (0x2576, "...l"),
    (0x2577, ".l.."),
    (0x2578, "..h."),
    (0x2579, "h..."),
    (0x257A, "...h"),
    (0x257B, ".h.."),
    (0x257C, "..lh"),
    (0x257D, "lh.."),
    (0x257E, "..hl"),
    (0x257F, "hl.."),
];

const UP: usize = 0;
const DOWN: usize = 1;
const LEFT: usize = 2;
const RIGHT: usize = 3;

fn build() -> Vec<Vec<Prim>> {
    let mut table = vec![Vec::new(); (LAST - FIRST + 1) as usize];
    for &(code, letters) in LINES {
        let bytes = letters.as_bytes();
        let arms = [
            Arm::parse(bytes[UP]),
            Arm::parse(bytes[DOWN]),
            Arm::parse(bytes[LEFT]),
            Arm::parse(bytes[RIGHT]),
        ];
        table[(code - FIRST) as usize] = line_prims(arms);
    }
    // Rounded corners: a quarter curve between the middles of two edges.
    table[(0x256D - FIRST) as usize] = vec![curve((4, 8), (8, 4))];
    table[(0x256E - FIRST) as usize] = vec![curve((4, 8), (0, 4))];
    table[(0x256F - FIRST) as usize] = vec![curve((4, 0), (0, 4))];
    table[(0x2570 - FIRST) as usize] = vec![curve((4, 0), (8, 4))];
    blocks(&mut table);
    table
}

fn curve(from: Point8, to: Point8) -> Prim {
    Prim::Curve {
        from,
        ctrl: (4, 4),
        to,
    }
}

/// The rectangles of a line character from its four arms. Each arm runs from
/// its cell edge to the middle and reaches past the middle by `reach` half
/// line widths, so that corners are filled; at a crossing of double lines
/// the lines stop at the nearest crossing line (a negative reach).
fn line_prims(arms: [Arm; 4]) -> Vec<Prim> {
    let mut prims = Vec::new();
    for arm in [UP, DOWN, LEFT, RIGHT] {
        let vertical = arm == UP || arm == DOWN;
        let opposite = arm ^ 1;
        // The arms across this one: the side of a negative offset (up or
        // left) and of a positive offset (down or right).
        let (across_neg, across_pos) = if vertical { (LEFT, RIGHT) } else { (UP, DOWN) };
        for &(offset, thickness) in arms[arm].lines() {
            let reach = line_reach(&arms, opposite, across_neg, across_pos, offset);
            let (lo, hi) = (offset - thickness / 2, offset + thickness / 2);
            let at = |half_lines: i8| Coord {
                eighths: 4,
                half_lines,
            };
            let edge = |eighths: u8| Coord {
                eighths,
                half_lines: 0,
            };
            prims.push(match arm {
                UP => Prim::Rect {
                    x0: at(lo),
                    y0: edge(0),
                    x1: at(hi),
                    y1: Coord {
                        eighths: 4,
                        half_lines: reach,
                    },
                },
                DOWN => Prim::Rect {
                    x0: at(lo),
                    y0: Coord {
                        eighths: 4,
                        half_lines: -reach,
                    },
                    x1: at(hi),
                    y1: edge(8),
                },
                LEFT => Prim::Rect {
                    x0: edge(0),
                    y0: at(lo),
                    x1: Coord {
                        eighths: 4,
                        half_lines: reach,
                    },
                    y1: at(hi),
                },
                _ => Prim::Rect {
                    x0: Coord {
                        eighths: 4,
                        half_lines: -reach,
                    },
                    y0: at(lo),
                    x1: edge(8),
                    y1: at(hi),
                },
            });
        }
    }
    prims
}

/// How far the line at `offset` across `arm` reaches past the middle,
/// in half line widths (negative: it stops short of the middle).
fn line_reach(
    arms: &[Arm; 4],
    opposite: usize,
    across_neg: usize,
    across_pos: usize,
    offset: i8,
) -> i8 {
    let continues = arms[opposite] != Arm::None;
    if offset == 0 {
        // A single line on the axis: continued by the opposite arm, or it
        // ends at the middle and covers the lines across it.
        return if continues {
            0
        } else {
            arms[across_neg]
                .half_extent()
                .max(arms[across_pos].half_extent())
        };
    }
    let (near, far) = if offset < 0 {
        (arms[across_neg], arms[across_pos])
    } else {
        (arms[across_pos], arms[across_neg])
    };
    match near {
        // A double line across, on this line's side: stop at its nearest line.
        Arm::Double => -2,
        // A single line across: it covers the end of this line.
        Arm::Light | Arm::Heavy => 0,
        Arm::None if continues => 0,
        Arm::None => far.half_extent(),
    }
}

/// U+2580–U+259F.
fn blocks(table: &mut [Vec<Prim>]) {
    let rect = |x0: u8, y0: u8, x1: u8, y1: u8| Prim::Rect {
        x0: eighths(x0),
        y0: eighths(y0),
        x1: eighths(x1),
        y1: eighths(y1),
    };
    let mut set = |code: u32, prims: Vec<Prim>| table[(code - FIRST) as usize] = prims;
    set(0x2580, vec![rect(0, 0, 8, 4)]);
    // Lower one eighth to seven eighths.
    for n in 1..=7u8 {
        set(0x2580 + u32::from(n), vec![rect(0, 8 - n, 8, 8)]);
    }
    set(0x2588, vec![rect(0, 0, 8, 8)]);
    // Left seven eighths to one eighth.
    for n in 1..=7u8 {
        set(0x2588 + u32::from(n), vec![rect(0, 0, 8 - n, 8)]);
    }
    set(0x2590, vec![rect(4, 0, 8, 8)]);
    set(0x2591, vec![Prim::Shade { percent: 25 }]);
    set(0x2592, vec![Prim::Shade { percent: 50 }]);
    set(0x2593, vec![Prim::Shade { percent: 75 }]);
    set(0x2594, vec![rect(0, 0, 8, 1)]);
    set(0x2595, vec![rect(7, 0, 8, 8)]);
    let upper_left = rect(0, 0, 4, 4);
    let upper_right = rect(4, 0, 8, 4);
    let lower_left = rect(0, 4, 4, 8);
    let lower_right = rect(4, 4, 8, 8);
    set(0x2596, vec![lower_left]);
    set(0x2597, vec![lower_right]);
    set(0x2598, vec![upper_left]);
    set(0x2599, vec![upper_left, lower_left, lower_right]);
    set(0x259A, vec![upper_left, lower_right]);
    set(0x259B, vec![upper_left, upper_right, lower_left]);
    set(0x259C, vec![upper_left, upper_right, lower_right]);
    set(0x259D, vec![upper_right]);
    set(0x259E, vec![upper_right, lower_left]);
    set(0x259F, vec![upper_right, lower_left, lower_right]);
}

fn eighths(eighths: u8) -> Coord {
    Coord {
        eighths,
        half_lines: 0,
    }
}
