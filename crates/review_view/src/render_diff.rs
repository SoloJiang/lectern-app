//! Drawing the rows of a diff (docs/design/review_view.md「界面」「高亮」):
//! the unified row, the side-by-side row, a fold, and the probe's node for a
//! whole diff. Colours come from the `Palette`; the text of a line and its
//! runs of colour from [`crate::lines`].

use std::ops::Range;

use gpui::{
    AnyElement, Context, HighlightStyle, InteractiveElement, IntoElement, ParentElement, Pixels,
    ScrollDelta, ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, StyledText,
    Window, div, px,
};
use lectern_i18n::{Args, Strings, keys};
use lectern_services::review::{FileRows, RowsKey};
use lectern_ui::semantic::{Role, Semantic, State, semantic, semantic_interactive};
use lectern_ui::{
    Button, ButtonSize, Icon, Palette, SystemColor, Text, TextRole, TextRoleExt, mono_metrics,
};

use crate::highlight::{Half, SideSpans};
use crate::items::{self, DiffMode, DisplayRow, ItemKey, RowKind, RowNotes, Side};
use crate::lines::{self, DisplayLine};
use crate::model::ReviewModel;
use crate::nodes;

/// One side of a side-by-side row: the line, its text and its runs.
type Half2 = (Side, DisplayLine, Vec<(Range<usize>, HighlightStyle)>);

/// The cells of a line-number column: five digits and the room after them.
const NUMBER_CELLS: f32 = 6.0;

/// The bar at a row's left edge, where a selected row shows the accent.
const BAR: Pixels = px(2.0);

/// The dot that marks a row with a review note.
const DOT: Pixels = px(6.0);

/// A line that has nothing to scroll to keeps this many cells in view when
/// the diff is scrolled sideways to its end.
const KEEP_CELLS: f32 = 8.0;

impl ReviewModel {
    /// Where the code of a line starts, counted from the card's left edge:
    /// past the bar, the two line numbers and the sign. A fold's text and a
    /// drift note line up with it.
    pub(crate) fn code_indent(&self, window: &mut Window, cx: &mut gpui::App) -> Pixels {
        let cell = mono_metrics(window, cx).cell_width;
        BAR + cell * (2.0 * NUMBER_CELLS + 1.0)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "an item renderer: the pieces are the item's and the frame's"
    )]
    pub(crate) fn render_row(
        &mut self,
        rows_key: RowsKey,
        ix: u32,
        notes: RowNotes,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let blank = |model: &ReviewModel| div().h(model.line_height).into_any_element();
        let (Some(rows), Some(row)) = (
            self.rows.get(&rows_key).cloned(),
            self.displays
                .get(&rows_key)
                .and_then(|display| display.get(ix as usize))
                .cloned(),
        ) else {
            return blank(self);
        };
        if row.fold.is_some() {
            return blank(self);
        }
        let old = row.left.map(|side| side.line);
        let new = row.right.map(|side| side.line);
        let drifted = row
            .hunk
            .and_then(|h| rows.hunks.get(h as usize))
            .is_some_and(|info| info.attribution.drifted > 0)
            || matches!(&rows_key, RowsKey::Step { step, .. } if items::step_unaligned(&self.snapshot, *step));
        let key = ItemKey::Row {
            rows: rows_key.clone(),
            old,
            new,
        };
        // A selected hunk shows on its first row: in a session's narrative
        // the hunk has no line of its own.
        let first_of_hunk = row.hunk.is_some()
            && (ix == 0
                || self
                    .displays
                    .get(&rows_key)
                    .and_then(|display| display.get(ix as usize - 1))
                    .is_none_or(|before| before.hunk != row.hunk));
        let hunk_selected = first_of_hunk
            && match (
                &self.selection,
                row.hunk.and_then(|h| rows.hunks.get(h as usize)),
            ) {
                (Some(ItemKey::Hunk { rows: at, old, new }), Some(info)) => {
                    *at == rows_key && *old == info.old.start && *new == info.new.start
                }
                _ => false,
            };
        // The probe's node says only whether this row is the selection; the
        // row draws as selected also for the hunk it opens.
        let chosen = self.selection.as_ref() == Some(&key);
        let selected = chosen || hunk_selected;
        let kind = row_kind(&row);
        let node = nodes::keyed("diff", &nodes::rows_key_text(&rows_key))
            .child("row")
            .item(&nodes::lines_key(old, new));
        let mut spec = Semantic::new(node.clone(), Role::DiffLine)
            .state(State {
                selected: chosen,
                ..State::default()
            })
            .attr("kind", kind)
            .attr("drifted", drifted.to_string());
        if notes.count > 0 {
            spec = spec.attr("notes", notes.count.to_string());
        }
        let spans = self.highlighted.get(&rows_key).cloned().unwrap_or_default();
        let offset = self.hscroll.get(&rows_key).copied().unwrap_or(Pixels::ZERO);
        let mono = mono_metrics(window, cx);
        let content = match self.mode {
            DiffMode::Unified => {
                // The new side when the row has one (a context line is on both).
                let (side, half) = match (row.right, row.left) {
                    (Some(side), _) => (side, Half::New),
                    (None, Some(side)) => (side, Half::Old),
                    (None, None) => return blank(self),
                };
                let line = self.draw_line(&rows, half, side);
                self.note_width(&rows_key, &line);
                self.unified_row(
                    old,
                    new,
                    Some(side.kind),
                    selected,
                    &line,
                    code(&line, &spans, half, side, &rows, &row, palette),
                    offset,
                    mono.cell_width,
                    palette,
                )
            }
            DiffMode::SideBySide => {
                let mut halves = Vec::new();
                for (side, half) in [(row.left, Half::Old), (row.right, Half::New)] {
                    match side {
                        Some(side) => {
                            let line = self.draw_line(&rows, half, side);
                            self.note_width(&rows_key, &line);
                            let runs = code(&line, &spans, half, side, &rows, &row, palette);
                            halves.push(Some((side, line, runs)));
                        }
                        None => halves.push(None),
                    }
                }
                self.split_row(halves, selected, offset, mono.cell_width, palette)
            }
        };
        let mut line = div()
            .id(&node)
            .relative()
            .w_full()
            .h(self.line_height)
            .flex()
            .flex_row()
            .items_center()
            .overflow_hidden()
            .text_role(TextRole::Code, window, cx)
            .line_height(self.line_height)
            .whitespace_nowrap()
            .child(
                div()
                    .w(BAR)
                    .h_full()
                    .flex_shrink_0()
                    .bg(palette.accent.alpha(if selected { 1.0 } else { 0.0 })),
            )
            .child(content);
        if notes.count > 0 {
            // A dot in the line-number column: the accent while a note is
            // not sent, the tertiary label once all are.
            line = line.child(
                div()
                    .absolute()
                    .left(BAR + px(3.0))
                    .top((self.line_height - DOT) / 2.0)
                    .size(DOT)
                    .rounded_full()
                    .bg(if notes.unsent {
                        palette.accent
                    } else {
                        palette.tertiary_label
                    }),
            );
        }
        if chosen {
            // The + at the end of the row writes a note on it, as C does.
            let weak = cx.weak_entity();
            let add_key = ItemKey::Row {
                rows: rows_key.clone(),
                old,
                new,
            };
            line = line.child(
                div()
                    .absolute()
                    .right(px(8.0))
                    .top_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(
                        Button::icon_only(
                            node.child("add-note"),
                            Icon::Plus,
                            Text::key(strings, keys::review::NOTE_ADD_HERE),
                        )
                        .size(ButtonSize::Small)
                        .on_click(move |window, cx| {
                            let _ = weak.update(cx, |model, cx| {
                                model.select(Some(add_key.clone()), cx);
                                model.add_note_at_selection(window, cx);
                            });
                        }),
                    ),
            );
        }
        let click_key = key;
        let scroll_key = rows_key.clone();
        let line_height = self.line_height;
        line = line
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select(Some(click_key.clone()), cx);
            }))
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                let delta = match event.delta {
                    ScrollDelta::Pixels(delta) => delta.x,
                    ScrollDelta::Lines(delta) => delta.x * line_height,
                };
                this.scroll_sideways(&scroll_key, delta, mono.cell_width, cx);
            }));
        semantic_interactive(spec, line).into_any_element()
    }

    /// The text of a line of one side of a row.
    fn draw_line(&self, rows: &FileRows, half: Half, side: Side) -> DisplayLine {
        let rows_side = match half {
            Half::Old => &rows.old,
            Half::New => &rows.new,
        };
        lines::display_line(rows_side, side.byte, side.line)
    }

    /// Remembers how wide the widest line drawn so far is, so that a
    /// sideways scroll stops where the text does.
    fn note_width(&mut self, rows_key: &RowsKey, line: &DisplayLine) {
        let cells = u32::try_from(line.text.chars().count()).unwrap_or(u32::MAX);
        let widest = self.widest.entry(rows_key.clone()).or_insert(0);
        *widest = (*widest).max(cells);
    }

    /// A horizontal wheel or trackpad movement over a row: the lines of the
    /// whole diff move together.
    pub(crate) fn scroll_sideways(
        &mut self,
        rows_key: &RowsKey,
        delta: Pixels,
        cell: Pixels,
        cx: &mut Context<ReviewModel>,
    ) {
        if delta == Pixels::ZERO {
            return;
        }
        let widest = self.widest.get(rows_key).copied().unwrap_or(0) as f32;
        let limit = (cell * (widest - KEEP_CELLS)).max(Pixels::ZERO);
        let current = self.hscroll.get(rows_key).copied().unwrap_or(Pixels::ZERO);
        // The delta is the content's movement: scrolling left (negative)
        // shows what is further right.
        let next = (current - delta).max(Pixels::ZERO).min(limit);
        if next != current {
            self.hscroll.insert(rows_key.clone(), next);
            cx.notify();
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "a row's pieces: numbers, kind, text and the shared offset"
    )]
    fn unified_row(
        &self,
        old: Option<u32>,
        new: Option<u32>,
        kind: Option<RowKind>,
        selected: bool,
        line: &DisplayLine,
        runs: Vec<(Range<usize>, HighlightStyle)>,
        offset: Pixels,
        cell: Pixels,
        palette: &Palette,
    ) -> AnyElement {
        let number_color = if selected {
            palette.accent
        } else {
            palette.tertiary_label
        };
        let number = |line: Option<u32>| {
            div()
                .w(cell * NUMBER_CELLS)
                .pr(cell)
                .flex_shrink_0()
                .text_right()
                .text_color(number_color)
                .child(SharedString::from(
                    line.map_or_else(String::new, |line| (line + 1).to_string()),
                ))
        };
        let mut row = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .overflow_hidden();
        if let Some(background) = kind.and_then(|kind| kind_background(kind, palette)) {
            row = row.bg(background);
        }
        row.child(number(old))
            .child(number(new))
            .child(sign_cell(kind.unwrap_or(RowKind::Context), cell, palette))
            .child(text_cell(line, runs, offset))
            .into_any_element()
    }

    fn split_row(
        &self,
        halves: Vec<Option<Half2>>,
        selected: bool,
        offset: Pixels,
        cell: Pixels,
        palette: &Palette,
    ) -> AnyElement {
        let mut row = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .overflow_hidden();
        for (position, half) in halves.into_iter().enumerate() {
            if position == 1 {
                row = row.child(
                    div()
                        .w(px(0.5))
                        .h_full()
                        .flex_shrink_0()
                        .bg(palette.separator),
                );
            }
            let mut cell_div = div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_row()
                .items_center()
                .overflow_hidden();
            if let Some((side, line, runs)) = half {
                if let Some(background) = kind_background(side.kind, palette) {
                    cell_div = cell_div.bg(background);
                }
                cell_div = cell_div
                    .child(
                        div()
                            .w(cell * NUMBER_CELLS)
                            .pr(cell)
                            .flex_shrink_0()
                            .text_right()
                            .text_color(if selected {
                                palette.accent
                            } else {
                                palette.tertiary_label
                            })
                            .child(SharedString::from((side.line + 1).to_string())),
                    )
                    .child(sign_cell(side.kind, cell, palette))
                    .child(text_cell(&line, runs, offset));
            }
            row = row.child(cell_div);
        }
        row.into_any_element()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "an item renderer: the pieces are the item's and the frame's"
    )]
    pub(crate) fn render_fold(
        &mut self,
        rows_key: RowsKey,
        ix: u32,
        old: u32,
        palette: &Palette,
        strings: &Strings,
        window: &mut Window,
        cx: &mut Context<ReviewModel>,
    ) -> AnyElement {
        let count = self
            .displays
            .get(&rows_key)
            .and_then(|display| display.get(ix as usize))
            .and_then(|row| row.fold.clone())
            .map_or(0, |fold| fold.end.saturating_sub(fold.start));
        let text = Text::fmt(
            strings,
            keys::review::FOLD,
            &Args::new().num("count", count),
        );
        let node = nodes::keyed("diff", &nodes::rows_key_text(&rows_key))
            .child("fold")
            .item(&old.to_string());
        let spec = Semantic::new(node.clone(), Role::Button).label(&text);
        let key = ReviewModel::fold_key(&rows_key, old);
        let selected = self.selection.as_ref() == Some(&key);
        let mut row = div()
            .id(&node)
            .w_full()
            .h(self.line_height)
            .pl(self.code_indent(window, cx))
            .pr(px(14.0))
            .flex()
            .items_center()
            .cursor_pointer()
            .text_role(TextRole::Caption, window, cx)
            .text_color(palette.secondary_label)
            .child(text.shared());
        if selected {
            row = row.bg(palette.unemphasized_selected_content_background);
        }
        row = row.on_click(cx.listener(move |this, _, _, cx| {
            this.select(Some(key.clone()), cx);
            this.expand_fold(&rows_key, ix, cx);
        }));
        semantic_interactive(spec, row).into_any_element()
    }

    /// The probe's node for a whole diff (`review.diff[<rows-key>]`): how
    /// many rows, under which context, whether they are prepared, and what
    /// colour reached which side.
    pub(crate) fn diff_node(&self, rows_key: &RowsKey) -> AnyElement {
        let rows = self.rows.get(rows_key);
        let context = match rows_key {
            RowsKey::Final { context, .. } | RowsKey::Step { context, .. } => *context,
        };
        let highlighted = match self.highlighted.get(rows_key) {
            Some(spans) => format!(
                "old={},new={}",
                spans.state(Half::Old),
                spans.state(Half::New)
            ),
            None => "old=plain,new=plain".to_owned(),
        };
        let spec = Semantic::new(
            nodes::keyed("diff", &nodes::rows_key_text(rows_key)),
            Role::Diff,
        )
        .attr("rows", rows.map_or(0, |rows| rows.rows.len()).to_string())
        .attr("context", context.to_string())
        .attr("prepared", rows.is_some().to_string())
        .attr("highlighted", highlighted);
        semantic(spec, div().h(px(0.0)).w_full()).into_any_element()
    }
}

/// The runs of colour of a line: the highlight tokens of its side and the
/// changed words of the row.
fn code(
    line: &DisplayLine,
    spans: &SideSpans,
    half: Half,
    side: Side,
    rows: &FileRows,
    row: &DisplayRow,
    palette: &Palette,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let tokens = side.byte.map_or(&[][..], |byte| spans.line(half, byte));
    let (inline, background) = match (row.inline, side.kind) {
        (Some(entry), RowKind::Removed) => (
            rows.inline.get(entry as usize).map(|(_, spans)| &spans.old),
            Some(palette.diff_removed_inline_background),
        ),
        (Some(entry), RowKind::Added) => (
            rows.inline.get(entry as usize).map(|(_, spans)| &spans.new),
            Some(palette.diff_added_inline_background),
        ),
        _ => (None, None),
    };
    lines::highlights(
        line,
        tokens,
        inline.map_or(&[][..], |ranges| &ranges[..]),
        background,
        palette,
    )
}

fn text_cell(
    line: &DisplayLine,
    runs: Vec<(Range<usize>, HighlightStyle)>,
    offset: Pixels,
) -> AnyElement {
    let text: AnyElement = if runs.is_empty() {
        div().child(line.text.clone()).into_any_element()
    } else {
        div()
            .child(StyledText::new(line.text.clone()).with_highlights(runs))
            .into_any_element()
    };
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_hidden()
        .child(div().flex_shrink_0().ml(-offset).child(text))
        .into_any_element()
}

/// The sign column: `+` in green, `\u{2212}` in red.
fn sign_cell(kind: RowKind, cell: Pixels, palette: &Palette) -> AnyElement {
    let (text, color) = match kind {
        RowKind::Added => ("+", Some(palette.system(SystemColor::Green))),
        RowKind::Removed => ("\u{2212}", Some(palette.system(SystemColor::Red))),
        RowKind::Context => (" ", None),
    };
    let sign = div().w(cell).flex_shrink_0().child(text);
    match color {
        Some(color) => sign.text_color(color),
        None => sign,
    }
    .into_any_element()
}

fn kind_background(kind: RowKind, palette: &Palette) -> Option<gpui::Hsla> {
    match kind {
        RowKind::Added => Some(palette.diff_added_background),
        RowKind::Removed => Some(palette.diff_removed_background),
        RowKind::Context => None,
    }
}

/// The probe's `kind` of a row.
fn row_kind(row: &DisplayRow) -> &'static str {
    match (row.left, row.right) {
        (Some(left), Some(right)) => {
            if left.kind == RowKind::Context || right.kind == RowKind::Context {
                "context"
            } else {
                "changed"
            }
        }
        (Some(side), None) | (None, Some(side)) => match side.kind {
            RowKind::Context => "context",
            RowKind::Removed => "removed",
            RowKind::Added => "added",
        },
        (None, None) => "context",
    }
}
