//! The draw list of one frame (docs/design/terminal_view.md「绘制的顺序」).
//! The frame, its laid-out rows and the cell metrics become a list of plain
//! operations; the element replays the list onto the window. Building the
//! list is pure (no window), so the tests look at the list and the
//! `terminal_view/snap_full` benchmark times it.
//!
//! The design orders the operations row by row; the list is ordered by kind
//! over the whole grid (backgrounds, selection, block cursor, glyphs,
//! decorations, cursor). GPUI batches the operations of one kind that follow
//! each other, so the grid costs six batches instead of one set per row, and
//! a descender is no longer covered by the background of the next row.

use std::sync::Arc;

use gpui::{Bounds, FontId, GlyphId, Hsla, Pixels, Point, Size, point, px, size};
use lectern_services::terminal::{CursorShape, Frame, GridRow, Rgb, StyleFlags, Underline};
use lectern_ui::MonoMetrics;

use crate::boxes::{Coord, Prim, box_glyph};
use crate::colors::ColorMemo;
use crate::layout::{Kind, RowLayout};

/// One drawing operation, in window coordinates.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Op {
    Fill {
        bounds: Bounds<Pixels>,
        color: Hsla,
    },
    /// A hollow rectangle (the cursor of a view without focus).
    Outline {
        bounds: Bounds<Pixels>,
        color: Hsla,
        width: Pixels,
    },
    /// `origin` is the left end of the baseline.
    Glyph {
        origin: Point<Pixels>,
        font: FontId,
        glyph: GlyphId,
        color: Hsla,
        emoji: bool,
    },
    /// A quadratic curve stroked with `width` (a rounded corner).
    Curve {
        from: Point<Pixels>,
        ctrl: Point<Pixels>,
        to: Point<Pixels>,
        width: Pixels,
        color: Hsla,
    },
    /// A wavy underline from `origin` over `width`.
    Wavy {
        origin: Point<Pixels>,
        width: Pixels,
        thickness: Pixels,
        color: Hsla,
    },
}

/// The cell metrics a frame is drawn with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct CellMetrics {
    /// One cell in points.
    pub cell: Size<Pixels>,
    pub ascent: Pixels,
    pub descent: Pixels,
    /// Device pixels per point.
    pub scale: f32,
}

impl CellMetrics {
    pub(crate) fn new(metrics: &MonoMetrics, scale: f32) -> CellMetrics {
        CellMetrics {
            cell: size(metrics.cell_width, metrics.line_height),
            ascent: metrics.ascent,
            descent: metrics.descent,
            scale,
        }
    }

    /// The distance from the top of a row to its baseline.
    pub(crate) fn baseline(&self) -> Pixels {
        (self.cell.height - (self.ascent + self.descent)) / 2.0 + self.ascent
    }

    /// One device pixel in points: the width of an underline.
    fn hairline(&self) -> Pixels {
        px(1.0 / self.scale)
    }

    /// The width of a thin line of a box character: one device pixel or an
    /// eighth of the cell, the larger, on the device pixel grid.
    fn thin_line(&self) -> f32 {
        let device = (self.cell.width.as_f32() / 8.0 * self.scale)
            .round()
            .max(1.0);
        device / self.scale
    }
}

/// The shaped text of an input method's composition.
#[derive(Clone, PartialEq, Default, Debug)]
pub(crate) struct MarkedPaint {
    pub width: Pixels,
    /// `x` is the offset from the left edge of the text.
    pub glyphs: Vec<(Pixels, FontId, GlyphId, bool)>,
}

/// What `build` needs.
pub(crate) struct Scene<'a> {
    pub frame: &'a Frame,
    /// One layout for each row of the frame.
    pub layouts: &'a [Arc<RowLayout>],
    /// The top left corner of the grid.
    pub origin: Point<Pixels>,
    pub metrics: CellMetrics,
    pub focused: bool,
    /// The selection colour for the current focus state.
    pub selection: Hsla,
    pub marked: Option<&'a MarkedPaint>,
}

/// What building the list counted, for the verbose log.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct BuildStats {
    pub glyphs: u32,
}

/// Appends the operations of the scene to `ops`.
pub(crate) fn build(scene: &Scene<'_>, ops: &mut Vec<Op>) -> BuildStats {
    let frame = scene.frame;
    let metrics = scene.metrics;
    let (cell_width, line_height) = (metrics.cell.width, metrics.cell.height);
    let row_top = |row: u16| scene.origin.y + line_height * f32::from(row);
    let col_left = |col: u16| scene.origin.x + cell_width * f32::from(col);
    let cells = |from: u16, to: u16, row: u16| Bounds {
        origin: point(col_left(from), row_top(row)),
        size: size(cell_width * f32::from(to - from), line_height),
    };
    let mut colors = ColorMemo::new();
    let mut stats = BuildStats::default();

    // 1. Cell backgrounds, neighbouring runs of one colour as one rectangle.
    for (row_ix, row) in frame.rows.iter().enumerate() {
        let row_ix = row_ix as u16;
        let mut pending: Option<(u16, u16, Rgb)> = None;
        for run in row.runs.iter() {
            pending = match (run.style.bg, pending) {
                (Some(bg), Some((start, end, current))) if current == bg && end == run.start => {
                    Some((start, run.end, current))
                }
                (Some(bg), before) => {
                    if let Some((start, end, color)) = before {
                        ops.push(Op::Fill {
                            bounds: cells(start, end, row_ix),
                            color: colors.get(color),
                        });
                    }
                    Some((run.start, run.end, bg))
                }
                (None, before) => {
                    if let Some((start, end, color)) = before {
                        ops.push(Op::Fill {
                            bounds: cells(start, end, row_ix),
                            color: colors.get(color),
                        });
                    }
                    None
                }
            };
        }
        if let Some((start, end, color)) = pending {
            ops.push(Op::Fill {
                bounds: cells(start, end, row_ix),
                color: colors.get(color),
            });
        }
    }

    // 2. The selection.
    for span in frame.selection.iter() {
        if span.end > span.start {
            ops.push(Op::Fill {
                bounds: cells(span.start, span.end, span.row),
                color: scene.selection,
            });
        }
    }

    // 3. The base of a block cursor.
    let block = frame
        .cursor
        .filter(|cursor| scene.focused && cursor.shape == CursorShape::Block);
    if let Some(cursor) = block {
        let width = if cursor.wide { 2 } else { 1 };
        ops.push(Op::Fill {
            bounds: cells(cursor.col, cursor.col + width, cursor.row),
            color: colors.get(frame.colors.cursor),
        });
    }

    // 4. Glyphs and box characters.
    let baseline = metrics.baseline();
    for (row_ix, (row, layout)) in frame.rows.iter().zip(scene.layouts).enumerate() {
        let row_ix = row_ix as u16;
        for item in &layout.items {
            // A row without style runs has nothing to colour its glyphs with.
            let Some(run) = row.runs.get(usize::from(item.run)) else {
                continue;
            };
            let style = run.style;
            if style.flags.0 & StyleFlags::INVISIBLE != 0 {
                continue;
            }
            let under_cursor = block.is_some_and(|cursor| {
                cursor.row == row_ix
                    && item.col >= cursor.col
                    && item.col < cursor.col + if cursor.wide { 2 } else { 1 }
            });
            let mut color = if under_cursor {
                colors.get(frame.colors.background)
            } else {
                colors.get(style.fg)
            };
            if style.flags.0 & StyleFlags::FAINT != 0 {
                color = color.opacity(0.5);
            }
            match item.kind {
                Kind::Glyph { font, glyph, emoji } => {
                    stats.glyphs += 1;
                    ops.push(Op::Glyph {
                        origin: point(scene.origin.x + item.x, row_top(row_ix) + baseline),
                        font,
                        glyph,
                        color,
                        emoji,
                    });
                }
                Kind::Box(ch) => {
                    if let Some(prims) = box_glyph(ch) {
                        let origin = point(col_left(item.col), row_top(row_ix));
                        box_ops(prims, origin, &metrics, color, ops);
                    }
                }
            }
        }
    }

    // 5. Underlines, strikethroughs and overlines.
    for (row_ix, row) in frame.rows.iter().enumerate() {
        decorations(row, row_ix as u16, scene, &mut colors, ops);
    }

    // 6. The cursor.
    if let Some(cursor) = frame.cursor {
        let width = if cursor.wide { 2 } else { 1 };
        let cell = cells(cursor.col, cursor.col + width, cursor.row);
        let color = colors.get(frame.colors.cursor);
        if !scene.focused {
            ops.push(Op::Outline {
                bounds: cell,
                color,
                width: px(0.5),
            });
        } else {
            match cursor.shape {
                CursorShape::Block => {}
                CursorShape::Bar => ops.push(Op::Fill {
                    bounds: Bounds {
                        origin: cell.origin,
                        size: size(px(1.0), cell.size.height),
                    },
                    color,
                }),
                CursorShape::Underline => ops.push(Op::Fill {
                    bounds: Bounds {
                        origin: point(cell.origin.x, cell.origin.y + baseline),
                        size: size(cell.size.width, px(1.0)),
                    },
                    color,
                }),
            }
        }
    }

    // The composition of an input method, at the cursor cell.
    if let Some(marked) = scene.marked {
        let (col, row) = frame
            .cursor
            .map_or((0, 0), |cursor| (cursor.col, cursor.row));
        let left = col_left(col);
        let top = row_top(row);
        let grid_right = scene.origin.x + cell_width * f32::from(frame.size.cols);
        let width = marked.width.min(grid_right - left).max(px(0.0));
        let background = colors.get(frame.colors.background);
        let foreground = colors.get(frame.colors.foreground);
        ops.push(Op::Fill {
            bounds: Bounds {
                origin: point(left, top),
                size: size(width, line_height),
            },
            color: background,
        });
        for &(x, font, glyph, emoji) in &marked.glyphs {
            if x < width {
                ops.push(Op::Glyph {
                    origin: point(left + x, top + baseline),
                    font,
                    glyph,
                    color: foreground,
                    emoji,
                });
            }
        }
        ops.push(Op::Fill {
            bounds: Bounds {
                origin: point(left, top + underline_offset(&metrics)),
                size: size(width, metrics.hairline()),
            },
            color: foreground,
        });
    }
    stats
}

/// The distance from the top of a row to its underline.
fn underline_offset(metrics: &CellMetrics) -> Pixels {
    let wanted = metrics.baseline() + metrics.descent * 0.4;
    wanted.min(metrics.cell.height - metrics.hairline() * 2.0)
}

fn decorations(
    row: &GridRow,
    row_ix: u16,
    scene: &Scene<'_>,
    colors: &mut ColorMemo,
    ops: &mut Vec<Op>,
) {
    let metrics = scene.metrics;
    let (cell_width, line_height) = (metrics.cell.width, metrics.cell.height);
    let top = scene.origin.y + line_height * f32::from(row_ix);
    let hairline = metrics.hairline();
    for run in row.runs.iter() {
        let style = run.style;
        let decorated = style.underline != Underline::None
            || style.flags.0 & (StyleFlags::STRIKETHROUGH | StyleFlags::OVERLINE) != 0;
        if !decorated || style.flags.0 & StyleFlags::INVISIBLE != 0 {
            continue;
        }
        let left = scene.origin.x + cell_width * f32::from(run.start);
        let width = cell_width * f32::from(run.end - run.start);
        let fg = colors.get(style.fg);
        let line = |y: Pixels, thickness: Pixels, color: Hsla| Op::Fill {
            bounds: Bounds {
                origin: point(left, top + y),
                size: size(width, thickness),
            },
            color,
        };
        if style.flags.0 & StyleFlags::OVERLINE != 0 {
            ops.push(line(px(0.0), hairline, fg));
        }
        if style.flags.0 & StyleFlags::STRIKETHROUGH != 0 {
            ops.push(line(
                metrics.baseline() - metrics.ascent * 0.3,
                hairline,
                fg,
            ));
        }
        if style.underline != Underline::None {
            let color = style.underline_color.map_or(fg, |rgb| colors.get(rgb));
            underline(style.underline, ops, &metrics, left, top, width, color);
        }
    }
}

fn underline(
    kind: Underline,
    ops: &mut Vec<Op>,
    metrics: &CellMetrics,
    left: Pixels,
    top: Pixels,
    width: Pixels,
    color: Hsla,
) {
    let hairline = metrics.hairline();
    let y = top + underline_offset(metrics);
    let segment = |x: Pixels, length: Pixels, y: Pixels| Op::Fill {
        bounds: Bounds {
            origin: point(x, y),
            size: size(length, hairline),
        },
        color,
    };
    match kind {
        Underline::None => {}
        Underline::Single => ops.push(segment(left, width, y)),
        Underline::Double => {
            ops.push(segment(left, width, y - hairline * 2.0));
            ops.push(segment(left, width, y));
        }
        Underline::Curly => ops.push(Op::Wavy {
            origin: point(left, y - hairline),
            width,
            thickness: hairline,
            color,
        }),
        Underline::Dotted | Underline::Dashed => {
            let (dash, gap) = if kind == Underline::Dotted {
                (hairline, hairline)
            } else {
                (metrics.cell.width / 2.0, metrics.cell.width / 4.0)
            };
            let mut at = px(0.0);
            while at < width {
                ops.push(segment(left + at, dash.min(width - at), y));
                at += dash + gap;
            }
        }
    }
}

/// The operations of a box or block character in the cell at `origin`.
fn box_ops(
    prims: &[Prim],
    origin: Point<Pixels>,
    metrics: &CellMetrics,
    color: Hsla,
    ops: &mut Vec<Op>,
) {
    let scale = metrics.scale;
    let thin = metrics.thin_line();
    // Quantised first, so that the edge two neighbouring cells compute
    // differently by a rounding error is the same device pixel.
    let snap = |value: f32| ((value * scale * 16.0).round() / 16.0).round() / scale;
    let (width, height) = (metrics.cell.width.as_f32(), metrics.cell.height.as_f32());
    let x_of = |coord: Coord| {
        snap(
            origin.x.as_f32()
                + width * f32::from(coord.eighths) / 8.0
                + f32::from(coord.half_lines) * thin / 2.0,
        )
    };
    let y_of = |coord: Coord| {
        snap(
            origin.y.as_f32()
                + height * f32::from(coord.eighths) / 8.0
                + f32::from(coord.half_lines) * thin / 2.0,
        )
    };
    // The centre line of a thin line through the middle of the cell, on the
    // same device pixels as the rectangles of the straight lines.
    let curve_point = |(ex, ey): (u8, u8)| {
        let x = if ex == 4 {
            snap(origin.x.as_f32() + width / 2.0 - thin / 2.0) + thin / 2.0
        } else {
            snap(origin.x.as_f32() + width * f32::from(ex) / 8.0)
        };
        let y = if ey == 4 {
            snap(origin.y.as_f32() + height / 2.0 - thin / 2.0) + thin / 2.0
        } else {
            snap(origin.y.as_f32() + height * f32::from(ey) / 8.0)
        };
        point(px(x), px(y))
    };
    for prim in prims {
        match *prim {
            Prim::Rect { x0, y0, x1, y1 } => {
                let (left, top) = (x_of(x0), y_of(y0));
                let (right, bottom) = (x_of(x1), y_of(y1));
                if right > left && bottom > top {
                    ops.push(Op::Fill {
                        bounds: Bounds {
                            origin: point(px(left), px(top)),
                            size: size(px(right - left), px(bottom - top)),
                        },
                        color,
                    });
                }
            }
            Prim::Shade { percent } => ops.push(Op::Fill {
                bounds: Bounds {
                    origin,
                    size: metrics.cell,
                },
                color: color.opacity(f32::from(percent) / 100.0),
            }),
            Prim::Curve { from, ctrl, to } => ops.push(Op::Curve {
                from: curve_point(from),
                ctrl: curve_point(ctrl),
                to: curve_point(to),
                width: px(thin),
                color,
            }),
        }
    }
}
