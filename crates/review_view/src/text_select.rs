//! Selecting the text of a diff (docs/design/review_view.md「文字选择
//! （#421）」): ⌥ and a drag over the code select characters, ⌘C (and the
//! Edit menu's Copy) copies the source bytes they cover. The selection lives
//! in the screen only: nothing of it is written or put in a snapshot, and
//! the log gets counts and names, never a line number or the text.
//!
//! A point is a line (its old and new numbers) and a byte offset into that
//! side's source line, not a display index or a pixel: it still names the
//! same character after a fold opens, a sideways scroll or new rows.
//! Pointer positions become offsets through the `TextLayout` of the code
//! cell that was painted in the last frame (`Layouts`), never through the
//! cell width: CJK and emoji are wider than one cell.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    ClickEvent, ClipboardItem, Context, HighlightStyle, Hsla, Pixels, Point, TextLayout, point,
};
use lectern_logging::debug;
use lectern_services::project::BlobId;
use lectern_services::review::{DiffSide, FileRows, RowsKey};

use crate::diag;
use crate::highlight::Half;
use crate::items::{self, DisplayRow, Item, Side};
use crate::lines::{self, DisplayLine};
use crate::model::ReviewModel;
use crate::range::Press;

/// A place in the text of a diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TextPoint {
    /// The (old, new) line numbers of the row, as `LineRange::at`: showing a
    /// fold changes the indexes, not these.
    pub(crate) at: (Option<u32>, Option<u32>),
    /// The side whose text the point is in.
    pub(crate) side: DiffSide,
    /// The offset in that side's source line (not in the displayed text).
    pub(crate) byte: u32,
}

/// The text a ⌥ drag left.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct TextSelection {
    pub(crate) rows: RowsKey,
    /// Side by side: the half pressed; unified: None (the selection follows
    /// the shown lines, removed and added ones alike).
    pub(crate) half: Option<DiffSide>,
    /// The contents of the two sides when the drag began: a change of
    /// either clears the selection.
    pub(crate) oids: (Option<BlobId>, Option<BlobId>),
    pub(crate) anchor: TextPoint,
    pub(crate) head: TextPoint,
}

/// What a press on a row of a diff is making.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PressMode {
    /// A range of lines (#373).
    Lines,
    /// A text selection (#421): from where the button went down to the
    /// point under the pointer.
    Text { anchor: TextPoint, head: TextPoint },
}

/// The code cells painted in the last frame, by diff, display row and
/// half: filled in the paint phase (where the layouts are ready), cleared
/// when the screen renders.
pub(crate) type Layouts = Rc<RefCell<HashMap<(RowsKey, u32, Half), TextLayout>>>;

/// The selection as display rows of the diff it is in, for one frame: the
/// first and the last point by (row index, byte), in order.
#[derive(Clone, Debug)]
pub(crate) struct TextSpan {
    pub(crate) rows: RowsKey,
    pub(crate) half: Option<DiffSide>,
    pub(crate) first: (usize, u32),
    pub(crate) last: (usize, u32),
}

/// Whether a button went down with ⌥ and no other modifier.
pub(crate) fn option_only(modifiers: &gpui::Modifiers) -> bool {
    *modifiers == gpui::Modifiers::alt()
}

/// Whether a click came from a press with ⌥ held: a text selection's, which
/// a row's click handler leaves alone. Keyboard and touch clicks are clicks.
pub(crate) fn option_click(event: &ClickEvent) -> bool {
    matches!(event, ClickEvent::Mouse(click) if click.down.modifiers.alt)
}

/// The side a display row shows in `half` (side by side) or, unified
/// (`half` None), the side it shows: the new one when it has one.
pub(crate) fn row_side(row: &DisplayRow, half: Option<DiffSide>) -> Option<(DiffSide, Side)> {
    match half {
        None => row
            .right
            .map(|side| (DiffSide::New, side))
            .or(row.left.map(|side| (DiffSide::Old, side))),
        Some(DiffSide::Old) => row.left.map(|side| (DiffSide::Old, side)),
        Some(DiffSide::New) => row.right.map(|side| (DiffSide::New, side)),
    }
}

/// The (old, new) line numbers of a display row.
pub(crate) fn lines_of(row: &DisplayRow) -> (Option<u32>, Option<u32>) {
    (
        row.left.map(|side| side.line),
        row.right.map(|side| side.line),
    )
}

/// The display row with these line numbers (folds have none).
fn row_index(display: &[DisplayRow], at: (Option<u32>, Option<u32>)) -> Option<usize> {
    display
        .iter()
        .position(|row| row.fold.is_none() && lines_of(row) == at)
}

/// The half of the highlighting that draws a side.
pub(crate) fn half_of(side: DiffSide) -> Half {
    match side {
        DiffSide::Old => Half::Old,
        DiffSide::New => Half::New,
    }
}

fn rows_side(rows: &FileRows, side: DiffSide) -> &lectern_services::review::RowsSide {
    match side {
        DiffSide::Old => &rows.old,
        DiffSide::New => &rows.new,
    }
}

/// The selection's points as display rows of `display`, in order. None when
/// either line is not a row of it (any more).
pub(crate) fn span_of(display: &[DisplayRow], selection: &TextSelection) -> Option<TextSpan> {
    let a = (
        row_index(display, selection.anchor.at)?,
        selection.anchor.byte,
    );
    let h = (row_index(display, selection.head.at)?, selection.head.byte);
    let (first, last) = if a <= h { (a, h) } else { (h, a) };
    Some(TextSpan {
        rows: selection.rows.clone(),
        half: selection.half,
        first,
        last,
    })
}

/// The part of display row `ix`'s text (shown on `side`) the selection
/// covers, in display offsets. None outside it or when empty.
pub(crate) fn display_range(
    span: &TextSpan,
    ix: usize,
    side: DiffSide,
    line: &DisplayLine,
    display: &[DisplayRow],
) -> Option<Range<usize>> {
    if ix < span.first.0 || ix > span.last.0 {
        return None;
    }
    let (shown, _) = row_side(display.get(ix)?, span.half)?;
    if shown != side {
        return None;
    }
    let len = line.text.len();
    let start = if ix == span.first.0 {
        line.map(span.first.1.min(line.kept())).min(len)
    } else {
        0
    };
    let end = if ix == span.last.0 {
        line.map(span.last.1.min(line.kept())).min(len)
    } else {
        len
    };
    (start < end).then_some(start..end)
}

/// The colour runs of a line with the selected part (`chosen`, display
/// offsets) on `background` (pure): the runs are cut at its edges, the text
/// colours stay, and the selection covers the backgrounds of added and
/// removed words. Sorted and not overlapping, as the runs were.
pub(crate) fn paint_selection(
    runs: Vec<(Range<usize>, HighlightStyle)>,
    chosen: Range<usize>,
    background: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut points: Vec<usize> = vec![chosen.start, chosen.end];
    for (range, _) in &runs {
        points.push(range.start);
        points.push(range.end);
    }
    points.sort_unstable();
    points.dedup();
    let mut out = Vec::new();
    for pair in points.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let mut style = runs
            .iter()
            .find(|(range, _)| range.start <= from && to <= range.end)
            .map(|(_, style)| *style)
            .unwrap_or_default();
        if chosen.start <= from && to <= chosen.end {
            style.background_color = Some(background);
        }
        if style != HighlightStyle::default() {
            out.push((from..to, style));
        }
    }
    out
}

/// The text Copy puts on the clipboard (pure): the source bytes of each
/// selected line on its side, without numbers or signs, joined with `\n`.
/// The first line starts at the first point and the last ends at the other;
/// a line cut where it is drawn gives its whole source when the selection
/// reaches the end of what is shown. A side without text, or a line that is
/// not valid UTF-8, gives the text that is shown.
pub(crate) fn copied_text(
    rows: &FileRows,
    display: &[DisplayRow],
    selection: &TextSelection,
) -> Option<String> {
    let span = span_of(display, selection)?;
    let mut pieces: Vec<String> = Vec::new();
    for ix in span.first.0..=span.last.0 {
        let Some((side, shown)) = display.get(ix).and_then(|row| row_side(row, span.half)) else {
            continue;
        };
        let held = rows_side(rows, side);
        let line = lines::display_line(held, shown.byte, shown.line);
        let start = if ix == span.first.0 { span.first.1 } else { 0 };
        let end = if ix == span.last.0 {
            span.last.1
        } else {
            u32::MAX
        };
        let piece = match lines::source_line(held, shown.byte, shown.line) {
            Some(source) if !line.lossy() => {
                let len = u32::try_from(source.len()).unwrap_or(u32::MAX);
                let end = if end >= line.kept() { len } else { end };
                let (start, end) = (start.min(len) as usize, end.min(len) as usize);
                String::from_utf8_lossy(&source[start.min(end)..end]).into_owned()
            }
            _ => {
                let text = line.text.as_ref();
                let a = line.map(start.min(line.kept())).min(text.len());
                let b = if end == u32::MAX {
                    text.len()
                } else {
                    line.map(end.min(line.kept())).min(text.len())
                };
                text.get(a.min(b)..b).unwrap_or_default().to_owned()
            }
        };
        pieces.push(piece);
    }
    Some(pieces.join("\n"))
}

/// The display offset under `position` in a painted line: the nearer edge of
/// the character there; before the text 0, after it the end.
fn hit(layout: &TextLayout, position: Point<Pixels>, text: &str) -> usize {
    let bounds = layout.bounds();
    if position.x <= bounds.left() {
        return 0;
    }
    let at = point(position.x, bounds.center().y);
    let index = match layout.index_for_position(at) {
        Ok(index) => {
            let next = text[index.min(text.len())..]
                .chars()
                .next()
                .map_or(index, |c| index + c.len_utf8());
            match (
                layout.position_for_index(index),
                layout.position_for_index(next),
            ) {
                (Some(a), Some(b)) if (position.x - a.x) > (b.x - position.x) => next,
                _ => index,
            }
        }
        Err(index) => index,
    };
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

impl ReviewModel {
    /// The point under `position` on display row `ix` of `rows` (its half
    /// `half` side by side), from the layout painted in the last frame.
    /// None when that cell was not painted.
    fn point_at(
        &self,
        rows: &RowsKey,
        ix: u32,
        half: Option<DiffSide>,
        position: Point<Pixels>,
    ) -> Option<TextPoint> {
        let row = self.displays.get(rows)?.get(ix as usize)?.clone();
        let (side, shown) = row_side(&row, half)?;
        let layout = self
            .text_layouts
            .borrow()
            .get(&(rows.clone(), ix, half_of(side)))?
            .clone();
        let held = self.rows.get(rows)?;
        let line = lines::display_line(rows_side(held, side), shown.byte, shown.line);
        let index = hit(&layout, position, line.text.as_ref());
        Some(TextPoint {
            at: lines_of(&row),
            side,
            byte: line.source_offset(index),
        })
    }

    /// The end of display row `ix` (`end`) or its start, on the side it
    /// shows: where a drag past the stretch stops.
    fn edge_of(
        &self,
        rows: &RowsKey,
        ix: u32,
        half: Option<DiffSide>,
        end: bool,
    ) -> Option<TextPoint> {
        let row = self.displays.get(rows)?.get(ix as usize)?.clone();
        let (side, shown) = row_side(&row, half)?;
        let byte = if end {
            let held = self.rows.get(rows)?;
            lines::display_line(rows_side(held, side), shown.byte, shown.line).kept()
        } else {
            0
        };
        Some(TextPoint {
            at: lines_of(&row),
            side,
            byte,
        })
    }

    /// ⌥ went down on the code of display row `ix` (its half `half` side by
    /// side): a text drag begins. The range and the old selection end; the
    /// current line stays, no editor opens.
    pub(crate) fn begin_text(
        &mut self,
        rows: RowsKey,
        ix: u32,
        half: Option<DiffSide>,
        position: Point<Pixels>,
        cx: &mut Context<ReviewModel>,
    ) {
        let Some(display) = self.displays.get(&rows) else {
            return;
        };
        let run = items::run_of(display, ix as usize);
        if run.is_empty() {
            return;
        }
        let Some(anchor) = self.point_at(&rows, ix, half, position) else {
            return;
        };
        self.drop_range();
        self.drop_text("cleared");
        let held = self.rows.get(&rows);
        let oids = (
            held.and_then(|rows| rows.old.oid),
            held.and_then(|rows| rows.new.oid),
        );
        self.press = Some(Press::text(
            rows,
            ix,
            run.start as u32..run.end as u32,
            half,
            oids,
            anchor,
        ));
        cx.notify();
    }

    /// A text drag follows the pointer: the row under it in the pressed
    /// stretch gives the head; above or below the stretch the head is its
    /// first or last row's edge; anything else (a note, an editor) leaves
    /// the head where it is. True when the head moved.
    pub(crate) fn follow_text(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<ReviewModel>,
    ) -> bool {
        let Some(press) = &self.press else {
            return false;
        };
        let PressMode::Text { head, .. } = press.mode else {
            return false;
        };
        let (rows, run, half) = (press.rows.clone(), press.run.clone(), press.half);
        let Some(under) = self.item_under(position.y) else {
            return false;
        };
        let item_of = |target: u32| {
            self.items.iter().position(
                |item| matches!(item, Item::Row { rows: at, ix, .. } if *at == rows && *ix == target),
            )
        };
        let next = match &self.items[under] {
            Item::Row { rows: at, ix, .. } if *at == rows && run.contains(ix) => {
                self.point_at(&rows, *ix, half, position)
            }
            _ => match (item_of(run.start), item_of(run.end - 1)) {
                (Some(first), _) if under < first => self.edge_of(&rows, run.start, half, false),
                (_, Some(last)) if under > last => self.edge_of(&rows, run.end - 1, half, true),
                _ => None,
            },
        };
        let Some(next) = next else {
            return false;
        };
        if next == head {
            return false;
        }
        if let Some(press) = &mut self.press
            && let PressMode::Text { head, .. } = &mut press.mode
        {
            *head = next;
        }
        cx.notify();
        true
    }

    /// The button went up after a text drag: a selection when it covers
    /// something (a ⌥ click leaves none). No line is selected.
    pub(crate) fn finish_text(&mut self, press: Press) {
        let PressMode::Text { anchor, head } = press.mode else {
            return;
        };
        if anchor == head {
            return;
        }
        let selection = TextSelection {
            rows: press.rows,
            half: press.half,
            oids: press.oids,
            anchor,
            head,
        };
        let lines = self.selection_lines(&selection);
        self.text = Some(selection);
        debug!(
            "text selection";
            id = self.id.0,
            how = "set",
            lines = lines,
            mode = diag::mode_kind(self.mode)
        );
    }

    /// How many display rows the selection spans (for the log).
    fn selection_lines(&self, selection: &TextSelection) -> u32 {
        self.displays
            .iter()
            .find(|(key, _)| items::same_diff(key, &selection.rows))
            .and_then(|(_, display)| span_of(display, selection))
            .map_or(0, |span| (span.last.0 - span.first.0 + 1) as u32)
    }

    /// The selection is over; the caller redraws.
    pub(crate) fn drop_text(&mut self, how: &'static str) {
        let Some(selection) = self.text.take() else {
            return;
        };
        let lines = self.selection_lines(&selection);
        debug!(
            "text selection";
            id = self.id.0,
            how = how,
            lines = lines,
            mode = diag::mode_kind(self.mode)
        );
    }

    /// After a snapshot or new rows: a selection whose file left the review,
    /// whose rows are not held any more, or whose contents changed is over.
    pub(crate) fn check_text(&mut self) {
        let Some(selection) = &self.text else {
            return;
        };
        let held = self
            .rows
            .iter()
            .find(|(key, _)| items::same_diff(key, &selection.rows))
            .map(|(_, rows)| (rows.old.oid, rows.new.oid));
        let gone = items::rows_file(&self.snapshot, &selection.rows).is_none()
            || held != Some(selection.oids);
        if gone {
            self.drop_text("cleared");
        }
    }

    /// The selection as rows of the diff it is in now (whichever context
    /// holds it), for this frame's rows to draw.
    pub(crate) fn work_out_span(&self) -> Option<TextSpan> {
        // A ⌥ drag in progress is drawn from the press, so the highlight
        // follows the pointer (as in any macOS text view); otherwise the
        // finished selection.
        let live = self.press.as_ref().and_then(|press| match press.mode {
            PressMode::Text { anchor, head } if anchor != head => Some(TextSelection {
                rows: press.rows.clone(),
                half: press.half,
                oids: press.oids,
                anchor,
                head,
            }),
            _ => None,
        });
        let selection = live.as_ref().or(self.text.as_ref())?;
        let (key, display) = self
            .displays
            .iter()
            .find(|(key, _)| items::same_diff(key, &selection.rows))?;
        let mut span = span_of(display, selection)?;
        span.rows = key.clone();
        Some(span)
    }

    /// Copy (⌘C, the Edit menu): the selected source text to the
    /// clipboard. The selection stays. A selection whose rows are not held
    /// any more is cleared instead.
    pub(crate) fn copy_text(&mut self, cx: &mut Context<ReviewModel>) {
        let Some(selection) = self.text.clone() else {
            return;
        };
        let found = self
            .rows
            .iter()
            .find(|(key, _)| items::same_diff(key, &selection.rows))
            .and_then(|(key, rows)| Some((rows.clone(), self.displays.get(key)?.clone())));
        let text = found.and_then(|(rows, display)| copied_text(&rows, &display, &selection));
        let Some(text) = text else {
            self.drop_text("cleared");
            cx.notify();
            return;
        };
        debug!(
            "text selection";
            id = self.id.0,
            how = "copied",
            lines = self.selection_lines(&selection),
            bytes = text.len(),
            mode = diag::mode_kind(self.mode)
        );
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}
