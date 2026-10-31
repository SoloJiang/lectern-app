//! Laying a row of the frame out on the grid (docs/design/terminal_view.md
//! 「数据结构」「格子与字形」). GPUI shapes the row's text as one line; this
//! module puts every glyph at the column of its grapheme cluster, because the
//! shaped positions of a proportional fallback font or a wide character do not
//! fall on the grid. `force_width` is not used: GPUI places a glyph at its
//! index times that width, so a wide character shifts every glyph after it by
//! one column.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    FontId, FontStyle, FontWeight, GlyphId, Hsla, LineLayout, Pixels, TextRun, WindowTextSystem, px,
};
use lectern_services::terminal::{GridRow, StyleFlags};
use lectern_ui::{MonoMetrics, TextRole};

use crate::boxes::box_glyph;

/// What is drawn at one place of a row.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kind {
    /// A glyph of the shaped line.
    Glyph {
        font: FontId,
        glyph: GlyphId,
        emoji: bool,
    },
    /// A box-drawing or block character, drawn as geometry (`boxes`).
    Box(char),
}

/// One glyph or box character with its place.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Placed {
    /// The column of the grapheme cluster it belongs to.
    pub col: u16,
    /// The index of the style run that holds that column.
    pub run: u16,
    /// The offset from the left edge of the row.
    pub x: Pixels,
    pub kind: Kind,
}

/// A laid-out row. Cached by `GridRow::key`, which covers the text, the
/// clusters and the style runs.
#[derive(Clone, PartialEq, Default, Debug)]
pub(crate) struct RowLayout {
    pub items: Vec<Placed>,
}

/// Puts the glyphs of `line`, the shaping of `row.text`, on the grid.
///
/// The first glyph of a cluster is at `column * cell_width`; a wide cluster
/// is centred in its two columns; the later glyphs of the cluster (combining
/// marks) keep the offset the shaping gave them. A glyph wider than its
/// cell is placed as it is, not scaled.
pub(crate) fn place(row: &GridRow, line: &LineLayout, cell_width: Pixels) -> RowLayout {
    let clusters = &row.clusters;
    if clusters.is_empty() {
        return RowLayout::default();
    }
    let cluster_of = |index: usize| {
        clusters
            .partition_point(|cluster| cluster.byte as usize <= index)
            .saturating_sub(1)
    };
    let text = &row.text;
    let cluster_end = |ix: usize| {
        clusters
            .get(ix + 1)
            .map_or(text.len(), |next| next.byte as usize)
    };

    let mut items = Vec::with_capacity(clusters.len());
    // The clusters that are drawn as geometry or not at all (a space).
    let mut skipped = vec![false; clusters.len()];
    for (ix, cluster) in clusters.iter().enumerate() {
        let slice = &text[cluster.byte as usize..cluster_end(ix)];
        let mut chars = slice.chars();
        let first = chars.next();
        let single = chars.next().is_none();
        if first == Some(' ') && single {
            skipped[ix] = true;
            continue;
        }
        if let (Some(ch), true) = (first, single)
            && box_glyph(ch).is_some()
        {
            skipped[ix] = true;
            items.push(Placed {
                col: cluster.col,
                run: run_of(row, cluster.col),
                x: cell_width * f32::from(cluster.col),
                kind: Kind::Box(ch),
            });
        }
    }

    // Where the shaping put the first glyph of each cluster.
    let mut shaped: Vec<Option<Pixels>> = vec![None; clusters.len()];
    for run in &line.runs {
        for glyph in &run.glyphs {
            let slot = &mut shaped[cluster_of(glyph.index)];
            if slot.is_none_or(|x| glyph.position.x < x) {
                *slot = Some(glyph.position.x);
            }
        }
    }
    let advance = |ix: usize, start: Pixels| {
        let next = shaped[ix + 1..].iter().flatten().next().copied();
        next.unwrap_or(line.width) - start
    };

    for run in &line.runs {
        for glyph in &run.glyphs {
            let ix = cluster_of(glyph.index);
            if skipped[ix] {
                continue;
            }
            let cluster = clusters[ix];
            let Some(start) = shaped[ix] else { continue };
            let mut x = cell_width * f32::from(cluster.col);
            if cluster.wide {
                let spare = cell_width * 2.0 - advance(ix, start);
                x += (spare / 2.0).max(px(0.0));
            }
            items.push(Placed {
                col: cluster.col,
                run: run_of(row, cluster.col),
                x: x + (glyph.position.x - start),
                kind: Kind::Glyph {
                    font: run.font_id,
                    glyph: glyph.id,
                    emoji: glyph.is_emoji,
                },
            });
        }
    }
    RowLayout { items }
}

/// The index of the style run that holds `col`.
fn run_of(row: &GridRow, col: u16) -> u16 {
    let ix = row.runs.partition_point(|run| run.end <= col);
    ix.min(row.runs.len().saturating_sub(1)) as u16
}

/// Shapes and places one row. `GridRow::runs` only matter here for the font
/// (bold, italic); the colours are looked up per column when painting.
pub(crate) fn layout_row(
    row: &GridRow,
    text_system: &WindowTextSystem,
    metrics: &MonoMetrics,
) -> RowLayout {
    if row.text.is_empty() {
        return RowLayout::default();
    }
    let runs = text_runs(row, metrics.family);
    let line = text_system.layout_line(&row.text, TextRole::Code.size(), &runs, None);
    place(row, &line, metrics.cell_width)
}

/// The font runs of a row in bytes of its text. They tile the whole text.
fn text_runs(row: &GridRow, family: &'static str) -> Vec<TextRun> {
    let clusters = &row.clusters;
    let byte_at = |col: u16| {
        clusters
            .get(clusters.partition_point(|cluster| cluster.col < col))
            .map_or(row.text.len(), |cluster| cluster.byte as usize)
    };
    let mut runs: Vec<TextRun> = Vec::new();
    let mut at = 0;
    for run in row.runs.iter() {
        let end = byte_at(run.end).max(at);
        if end == at {
            continue;
        }
        let font = font_of(family, run.style.flags);
        match runs.last_mut() {
            Some(last) if last.font == font => last.len += end - at,
            _ => runs.push(TextRun {
                len: end - at,
                font,
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }),
        }
        at = end;
    }
    if at < row.text.len() {
        match runs.last_mut() {
            Some(last) => last.len += row.text.len() - at,
            None => runs.push(TextRun {
                len: row.text.len(),
                font: font_of(family, StyleFlags::default()),
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }),
        }
    }
    runs
}

fn font_of(family: &'static str, flags: StyleFlags) -> gpui::Font {
    let mut font = gpui::font(family);
    if flags.0 & StyleFlags::BOLD != 0 {
        font.weight = FontWeight::BOLD;
    }
    if flags.0 & StyleFlags::ITALIC != 0 {
        font.style = FontStyle::Italic;
    }
    font
}

/// Two generations of laid-out rows (docs/design/terminal_view.md「行缓存」):
/// the rows this frame used, and the rows the frame before used. A row of the
/// frame before that is gone, so the cache holds at most two frames of rows
/// however long the scrollback is.
#[derive(Default)]
pub(crate) struct RowCache {
    current: HashMap<u64, Arc<RowLayout>>,
    previous: HashMap<u64, Arc<RowLayout>>,
    /// The cell width the rows were laid out for.
    cell_width: Option<Pixels>,
    /// How many rows were laid out (not found) since the last `take_laid_out`.
    laid_out: u32,
}

impl RowCache {
    /// The layout of `row`: from this frame's rows, else the last frame's,
    /// else made by `layout`.
    pub(crate) fn get(
        &mut self,
        row: &GridRow,
        cell_width: Pixels,
        layout: impl FnOnce() -> RowLayout,
    ) -> Arc<RowLayout> {
        if self.cell_width != Some(cell_width) {
            self.current.clear();
            self.previous.clear();
            self.cell_width = Some(cell_width);
        }
        if let Some(found) = self.current.get(&row.key) {
            return found.clone();
        }
        let layout = match self.previous.get(&row.key) {
            Some(found) => found.clone(),
            None => {
                self.laid_out += 1;
                Arc::new(layout())
            }
        };
        self.current.insert(row.key, layout.clone());
        layout
    }

    /// Ends a frame: the rows it did not use are dropped.
    pub(crate) fn end_frame(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    pub(crate) fn take_laid_out(&mut self) -> u32 {
        std::mem::take(&mut self.laid_out)
    }

    /// How many rows are held, for the tests of the two generations.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.current.len() + self.previous.len()
    }
}
