//! Viewport rows from libghostty-vt's render state (design 「帧」「网格与颜色」).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use lectern_services::terminal::{
    CellStyle, Cluster, StyleFlags, StyleRun, Underline as RowUnderline,
};
use libghostty_vt::render::{CellIteration, Colors};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{StyleColor, Underline};

use lectern_services::terminal::GridRow;

use crate::palette::from_vt;

/// Reusable buffers for building one row.
#[derive(Default)]
pub(crate) struct RowBuilder {
    text: String,
    grapheme: String,
    clusters: Vec<Cluster>,
    runs: Vec<StyleRun>,
}

impl RowBuilder {
    /// Builds the row the cell iteration walks over, `cols` cells wide.
    pub(crate) fn build(
        &mut self,
        cells: &mut CellIteration<'_, '_>,
        colors: &Colors,
        wrapped: bool,
    ) -> libghostty_vt::error::Result<GridRow> {
        self.text.clear();
        self.clusters.clear();
        self.runs.clear();
        // The end of `text` and `clusters` after the last cell with text, so
        // trailing empty cells can be dropped.
        let mut kept = (0usize, 0usize);
        let mut col: u16 = 0;
        while let Some(cell) = cells.next() {
            let style = cell_style(cell, colors)?;
            push_run(&mut self.runs, col, style);
            let wide = cell.raw_cell()?.wide()?;
            if wide != CellWide::SpacerTail {
                let byte = self.text.len() as u32;
                self.grapheme.clear();
                if wide != CellWide::SpacerHead && cell.graphemes_len()? > 0 {
                    cell.graphemes_utf8(&mut self.grapheme)?;
                }
                let has_text = !self.grapheme.is_empty();
                if has_text {
                    self.text.push_str(&self.grapheme);
                } else {
                    self.text.push(' ');
                }
                self.clusters.push(Cluster {
                    byte,
                    col,
                    wide: wide == CellWide::Wide,
                });
                if has_text {
                    kept = (self.text.len(), self.clusters.len());
                }
            }
            col += 1;
        }
        self.text.truncate(kept.0);
        self.clusters.truncate(kept.1);
        let key = row_key(&self.text, &self.clusters, &self.runs, wrapped);
        Ok(GridRow {
            key,
            text: self.text.as_str().into(),
            clusters: self.clusters.as_slice().into(),
            runs: self.runs.as_slice().into(),
            wrapped,
        })
    }
}

/// Extends the last run when the style is the same, else starts a new one.
fn push_run(runs: &mut Vec<StyleRun>, col: u16, style: CellStyle) {
    match runs.last_mut() {
        Some(run) if run.style == style && run.end == col => run.end = col + 1,
        _ => runs.push(StyleRun {
            start: col,
            end: col + 1,
            style,
        }),
    }
}

fn cell_style(
    cell: &CellIteration<'_, '_>,
    colors: &Colors,
) -> libghostty_vt::error::Result<CellStyle> {
    let foreground = from_vt(colors.foreground);
    let background = from_vt(colors.background);
    let mut fg = cell.fg_color()?.map_or(foreground, from_vt);
    let mut bg = cell.bg_color()?.map(from_vt);
    if !cell.has_styling()? {
        return Ok(CellStyle {
            fg,
            bg: bg.filter(|bg| *bg != background),
            flags: StyleFlags::default(),
            underline: RowUnderline::None,
            underline_color: None,
        });
    }
    let style = cell.style()?;
    // libghostty-vt resolves palette indices but leaves SGR 7 to the caller.
    if style.inverse {
        let swapped_fg = bg.unwrap_or(background);
        bg = Some(fg);
        fg = swapped_fg;
    }
    let mut flags = 0u8;
    for (on, bit) in [
        (style.bold, StyleFlags::BOLD),
        (style.italic, StyleFlags::ITALIC),
        (style.faint, StyleFlags::FAINT),
        (style.strikethrough, StyleFlags::STRIKETHROUGH),
        (style.overline, StyleFlags::OVERLINE),
        (style.invisible, StyleFlags::INVISIBLE),
    ] {
        if on {
            flags |= bit;
        }
    }
    Ok(CellStyle {
        fg,
        bg: bg.filter(|bg| *bg != background),
        flags: StyleFlags(flags),
        underline: match style.underline {
            Underline::None => RowUnderline::None,
            Underline::Single => RowUnderline::Single,
            Underline::Double => RowUnderline::Double,
            Underline::Curly => RowUnderline::Curly,
            Underline::Dotted => RowUnderline::Dotted,
            Underline::Dashed => RowUnderline::Dashed,
            _ => RowUnderline::Single,
        },
        underline_color: match style.underline_color {
            StyleColor::None => None,
            StyleColor::Palette(index) => Some(from_vt(colors.palette[usize::from(index.0)])),
            StyleColor::Rgb(color) => Some(from_vt(color)),
        },
    })
}

/// 64-bit hash of everything a view lays out (design 「网格与颜色」). The
/// hasher has a fixed seed, so equal rows have equal keys within a process.
fn row_key(text: &str, clusters: &[Cluster], runs: &[StyleRun], wrapped: bool) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    for c in clusters {
        (c.byte, c.col, c.wide).hash(&mut h);
    }
    for r in runs {
        (r.start, r.end, r.style).hash(&mut h);
    }
    wrapped.hash(&mut h);
    h.finish()
}

/// Keeps the previous frame's row when the rebuilt one has the same content,
/// so unchanged rows stay the same `Arc` (a cursor move marks rows dirty
/// without changing them).
pub(crate) fn reuse(previous: Option<&Arc<GridRow>>, row: GridRow) -> Arc<GridRow> {
    match previous {
        Some(prev) if **prev == row => prev.clone(),
        _ => Arc::new(row),
    }
}
