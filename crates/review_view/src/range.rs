//! The line range of a review note (docs/design/review_view.md「审阅意见
//! （#22）」「拖选范围」): the press on a row, the drag over the rows below
//! and above it, the range that is left when the button goes up, and the
//! actions that build the same range without a mouse. The range lives in the
//! screen only: nothing of it is written or put in a snapshot, and the log
//! gets counts and names, never a line number.

use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::{Context, MouseButton, MouseMoveEvent, Pixels, Point, Window, px};
use lectern_logging::debug;
use lectern_services::project::BlobId;
use lectern_services::review::{DiffSide, NoteAnchor, NotePlace, NoteRows, RowsKey};

use crate::anchor;
use crate::diag;
use crate::items::{self, Item, ItemKey, RangeSpan};
use crate::model::{Direction, ReviewModel};

/// The distance from the list's top or bottom edge within which a drag
/// scrolls the list.
pub(crate) const EDGE: Pixels = px(24.0);

/// The depth into (or beyond) the edge zone where the scroll reaches its
/// top speed: twice the zone.
const FULL_SPEED_DEPTH: Pixels = px(48.0);

/// The top speed of the scroll, in lines of a diff each second.
const MAX_LINES_PER_SECOND: f32 = 40.0;

/// The longest time one frame counts for: a frame that was late must not
/// throw the list a screen further.
const LONGEST_FRAME: Duration = Duration::from_micros(50_000);

/// A button held down on a row of a diff.
#[derive(Clone, Debug)]
pub(crate) struct Press {
    /// The diff the press began in.
    pub(crate) rows: RowsKey,
    /// The display rows at the press and now under the pointer.
    pub(crate) start: u32,
    pub(crate) focus: u32,
    /// The stretch of display rows without a fold the drag stays in.
    pub(crate) run: Range<u32>,
    /// The half of a side-by-side row that was pressed.
    pub(crate) half: Option<DiffSide>,
    /// The range the rows from `start` to `focus` make now.
    pub(crate) span: Option<RangeSpan>,
    /// Where the focus row is in the list, to look at its bounds first.
    focus_item: Option<usize>,
    /// The contents of the two sides when the press began: a change of
    /// either ends the press.
    oids: (Option<BlobId>, Option<BlobId>),
    /// The frame the list was last scrolled for.
    last_tick: Option<Instant>,
}

/// The range a drag left: lines `line` to `last` of one side of one diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LineRange {
    pub(crate) rows: RowsKey,
    pub(crate) side: DiffSide,
    pub(crate) line: u32,
    pub(crate) last: u32,
    /// The content of that side, as the anchor will name it.
    pub(crate) oid: BlobId,
    /// The line numbers (old, new) of the row the drag began on and of the
    /// row where it ended (the button is there), not indexes: showing a
    /// fold makes the indexes change.
    pub(crate) from: (Option<u32>, Option<u32>),
    pub(crate) at: (Option<u32>, Option<u32>),
    /// The half that was pressed, for the actions that extend the range.
    pub(crate) half: Option<DiffSide>,
}

/// A range as a row draws it: the lines on one side, and the content they
/// must belong to when the range comes from a note or an editor.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RangeMark {
    pub(crate) side: DiffSide,
    pub(crate) line: u32,
    pub(crate) last: u32,
    pub(crate) oid: Option<BlobId>,
}

impl RangeMark {
    /// Whether `line` of `side` is one of the marked lines.
    pub(crate) fn covers(&self, side: DiffSide, line: Option<u32>) -> bool {
        self.side == side && line.is_some_and(|n| (self.line..=self.last).contains(&n))
    }
}

impl ReviewModel {
    // ----- the press -----------------------------------------------------

    /// The button went down on row `ix` of a diff (on `half` of it, side by
    /// side). Nothing is selected or opened: a press that ends on the same row
    /// is a click, and its handlers do what they did before.
    pub(crate) fn begin_press(
        &mut self,
        rows: RowsKey,
        ix: u32,
        half: Option<DiffSide>,
        cx: &mut Context<ReviewModel>,
    ) {
        let Some(display) = self.displays.get(&rows) else {
            return;
        };
        let run = items::run_of(display, ix as usize);
        if run.is_empty() {
            return;
        }
        let held = self.rows.get(&rows);
        let oids = (
            held.and_then(|rows| rows.old.oid),
            held.and_then(|rows| rows.new.oid),
        );
        self.press = Some(Press {
            rows,
            start: ix,
            focus: ix,
            run: run.start as u32..run.end as u32,
            half,
            span: None,
            focus_item: None,
            oids,
            last_tick: None,
        });
        // The screen draws the tracker of the pointer from the next frame on.
        cx.notify();
    }

    /// A pointer move while a press exists (the tracker's listener).
    pub(crate) fn drag_moved(&mut self, event: &MouseMoveEvent, cx: &mut Context<ReviewModel>) {
        if self.press.is_none() {
            return;
        }
        // The button went up where the screen did not see it (outside the
        // window, say).
        if event.pressed_button != Some(MouseButton::Left) {
            self.cancel_press(cx);
            return;
        }
        self.follow_pointer(event.position, cx);
    }

    /// The row under the pointer is the new focus of the press, if there is
    /// one. True when the focus changed.
    fn follow_pointer(&mut self, position: Point<Pixels>, cx: &mut Context<ReviewModel>) -> bool {
        let Some(press) = &self.press else {
            return false;
        };
        let (rows, run, current) = (press.rows.clone(), press.run.clone(), press.focus);
        // The row that had the focus is the usual answer: one look at it.
        if let Some(item) = press.focus_item
            && matches!(
                self.items.get(item),
                Some(Item::Row { rows: at, ix, .. }) if *at == rows && *ix == current
            )
            && self
                .list
                .bounds_for_item(item)
                .is_some_and(|bounds| bounds.top() <= position.y && position.y < bounds.bottom())
        {
            return false;
        }
        let Some(under) = self.item_under(position.y) else {
            return false;
        };
        let (focus, item) = match &self.items[under] {
            Item::Row { rows: at, ix, .. } if *at == rows => {
                ((*ix).clamp(run.start, run.end - 1), Some(under))
            }
            // Something else: notes, hunk titles and the like leave the focus
            // where it is, but a place before the first row of the stretch or
            // after its last one stands for that row.
            _ => {
                let item_of = |target: u32| {
                    self.items.iter().position(|item| {
                        matches!(item, Item::Row { rows: at, ix, .. } if *at == rows && *ix == target)
                    })
                };
                match (item_of(run.start), item_of(run.end - 1)) {
                    (Some(first), _) if under < first => (run.start, Some(first)),
                    (_, Some(last)) if under > last => (run.end - 1, Some(last)),
                    _ => return false,
                }
            }
        };
        let moved = focus != current;
        let span = if moved {
            self.press_span(&rows, focus)
        } else {
            self.press.as_ref().and_then(|press| press.span)
        };
        if let Some(press) = &mut self.press {
            press.focus = focus;
            press.focus_item = item;
            press.span = span;
        }
        if moved {
            cx.notify();
        }
        moved
    }

    /// The list item under height `y`: the pointer above or below the list
    /// stands for the item at that edge.
    fn item_under(&self, y: Pixels) -> Option<usize> {
        let viewport = self.list.viewport_bounds();
        if viewport.size.height <= Pixels::ZERO {
            return None;
        }
        let y = y.max(viewport.top()).min(viewport.bottom() - px(1.0));
        let end = self.visible.end.min(self.items.len());
        (self.visible.start..end).find(|&item| {
            self.list
                .bounds_for_item(item)
                .is_some_and(|bounds| bounds.top() <= y && y < bounds.bottom())
        })
    }

    /// The range of the pressed rows when the focus is on `focus`.
    fn press_span(&self, rows: &RowsKey, focus: u32) -> Option<RangeSpan> {
        let press = self.press.as_ref()?;
        let display = self.displays.get(rows)?;
        let span = items::range_of(
            display,
            press.start as usize,
            focus as usize,
            press.half,
            self.mode,
        )?;
        // A side without text has no content to anchor a note to.
        self.side_oid(rows, span.side)?;
        Some(span)
    }

    /// The content of one side of the rows held for `rows`.
    fn side_oid(&self, rows: &RowsKey, side: DiffSide) -> Option<BlobId> {
        let held = self.rows.get(rows)?;
        match side {
            DiffSide::Old => held.old.oid,
            DiffSide::New => held.new.oid,
        }
    }

    /// The content of one side of the diff `rows` names, whichever context
    /// it is held under now. None when it is not held.
    fn diff_oid(&self, rows: &RowsKey, side: DiffSide) -> Option<Option<BlobId>> {
        let (_, held) = self
            .rows
            .iter()
            .find(|(key, _)| items::same_diff(key, rows))?;
        Some(match side {
            DiffSide::Old => held.old.oid,
            DiffSide::New => held.new.oid,
        })
    }

    /// The button went up (the tracker's listener, or the screen's own when no
    /// frame was drawn since the press). On the row it went down on, it was a
    /// click and the row's handler does it; elsewhere a range is left.
    pub(crate) fn finish_press(&mut self, cx: &mut Context<ReviewModel>) {
        let Some(press) = self.press.take() else {
            return;
        };
        cx.notify();
        if press.focus == press.start {
            return;
        }
        let Some(span) = press.span else {
            return;
        };
        let (Some(oid), Some(display)) = (
            self.side_oid(&press.rows, span.side),
            self.displays.get(&press.rows).cloned(),
        ) else {
            return;
        };
        let lines = |ix: u32| {
            display
                .get(ix as usize)
                .map(|row| {
                    (
                        row.left.map(|side| side.line),
                        row.right.map(|side| side.line),
                    )
                })
                .unwrap_or_default()
        };
        let (from, at) = (lines(press.start), lines(press.focus));
        // The row where the button went up is the current line, and a new
        // selection ends the range before it: this one is set after it.
        self.select(
            Some(ItemKey::Row {
                rows: press.rows.clone(),
                old: at.0,
                new: at.1,
            }),
            cx,
        );
        self.range = Some(LineRange {
            rows: press.rows,
            side: span.side,
            line: span.line,
            last: span.last,
            oid,
            from,
            at,
            half: press.half,
        });
        debug!(
            "note range";
            id = self.id.0,
            how = "set",
            lines = span.last - span.line + 1,
            mode = diag::mode_kind(self.mode)
        );
    }

    /// The press ends without a range (Esc, a change of the diff, a button
    /// that went up unseen).
    pub(crate) fn cancel_press(&mut self, cx: &mut Context<ReviewModel>) {
        self.end_press("cancelled");
        cx.notify();
    }

    /// Takes the press away and logs how it ended; the caller redraws.
    pub(crate) fn end_press(&mut self, how: &'static str) {
        let Some(press) = self.press.take() else {
            return;
        };
        debug!(
            "note range";
            id = self.id.0,
            how = how,
            lines = press.span.map_or(0, |span| span.last - span.line + 1),
            mode = diag::mode_kind(self.mode)
        );
    }

    /// Scrolls the list while the pointer is held near its top or bottom
    /// edge, and finds the row under the pointer again (once for each frame;
    /// `now` is the frame's time). True when the pointer is in the zone, so
    /// that the screen asks for another frame: the list goes on moving
    /// while the pointer is still.
    pub(crate) fn tick_drag(
        &mut self,
        window: &mut Window,
        now: Instant,
        cx: &mut Context<ReviewModel>,
    ) -> bool {
        let Some(press) = &mut self.press else {
            return false;
        };
        let viewport = self.list.viewport_bounds();
        if viewport.size.height <= Pixels::ZERO {
            return false;
        }
        let pointer = window.mouse_position();
        let (top, bottom) = (viewport.top() + EDGE, viewport.bottom() - EDGE);
        let (depth, upward) = if pointer.y < top {
            (top - pointer.y, true)
        } else if pointer.y > bottom {
            (pointer.y - bottom, false)
        } else {
            press.last_tick = None;
            return false;
        };
        let elapsed = press
            .last_tick
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last))
            .min(LONGEST_FRAME);
        press.last_tick = Some(now);
        // The speed grows with how far the pointer is into the zone, or past
        // the edge.
        let speed = MAX_LINES_PER_SECOND * (depth / FULL_SPEED_DEPTH).min(1.0);
        let distance = self.line_height * (speed * elapsed.as_secs_f32());
        self.list
            .scroll_by(if upward { -distance } else { distance });
        self.follow_pointer(pointer, cx);
        true
    }

    // ----- the range -----------------------------------------------------

    /// The range is over (a new selection, Esc, a change of mode, a note
    /// saved on it, a change of the content).
    pub(crate) fn drop_range(&mut self) {
        let Some(range) = self.range.take() else {
            return;
        };
        debug!(
            "note range";
            id = self.id.0,
            how = "cleared",
            lines = range.last - range.line + 1,
            mode = diag::mode_kind(self.mode)
        );
    }

    /// After a snapshot or new rows: a press or a range whose content
    /// changed, or whose file left the review, is over. Rows that are not
    /// held (scrolled far away and let go) leave the range as it is.
    pub(crate) fn check_range(&mut self) {
        if let Some(press) = &self.press {
            let held = self.rows.get(&press.rows);
            let now = (
                held.and_then(|rows| rows.old.oid),
                held.and_then(|rows| rows.new.oid),
            );
            if items::rows_file(&self.snapshot, &press.rows).is_none() || now != press.oids {
                self.end_press("cancelled");
            }
        }
        let gone = self.range.as_ref().is_some_and(|range| {
            items::rows_file(&self.snapshot, &range.rows).is_none()
                || self
                    .diff_oid(&range.rows, range.side)
                    .is_some_and(|oid| oid != Some(range.oid))
        });
        if gone {
            self.drop_range();
        }
    }

    /// The anchor of the range, for C and the Add Note button.
    pub(crate) fn range_anchor(&self) -> Option<(RowsKey, NoteAnchor)> {
        let range = self.range.as_ref()?;
        let file = items::rows_file(&self.snapshot, &range.rows)?;
        Some((
            range.rows.clone(),
            NoteAnchor {
                file,
                side: range.side,
                line: range.line,
                last: range.last,
                oid: range.oid,
            },
        ))
    }

    /// The range row `rows` of the screen draws, if it belongs to one: the one
    /// being dragged; otherwise the one the drag left; otherwise the one of
    /// the new note's editor; otherwise the one of the note selected. At most
    /// one is looked at, and each look is a few comparisons, so a row costs
    /// the same with or without notes.
    pub(crate) fn range_mark(&self, rows: &RowsKey) -> Option<RangeMark> {
        if let Some(press) = &self.press {
            let span = press.span.filter(|_| press.rows == *rows)?;
            return Some(RangeMark {
                side: span.side,
                line: span.line,
                last: span.last,
                oid: None,
            });
        }
        if let Some(range) = &self.range {
            return items::same_diff(&range.rows, rows).then_some(RangeMark {
                side: range.side,
                line: range.line,
                last: range.last,
                oid: Some(range.oid),
            });
        }
        if let Some(editor) = &self.editor
            && let crate::notes::EditorTarget::New { rows: at, anchor } = &editor.target
        {
            return (anchor.last > anchor.line && items::same_diff(at, rows)).then_some(
                RangeMark {
                    side: anchor.side,
                    line: anchor.line,
                    last: anchor.last,
                    oid: Some(anchor.oid),
                },
            );
        }
        let Some(ItemKey::Note(id)) = &self.selection else {
            return None;
        };
        let note = self.note(*id)?;
        let anchor = &note.anchor;
        if anchor.last <= anchor.line {
            return None;
        }
        // The diff that draws the note: the step in a session's narrative,
        // the file's final rows otherwise.
        let NotePlace::Rows(kinds) = &note.place else {
            return None;
        };
        let narrative = self.snapshot.turns.is_some();
        let shown = kinds
            .iter()
            .find(|kind| matches!(kind, NoteRows::Step(_)) == narrative)?;
        let here = match (shown, rows) {
            (NoteRows::Step(a), RowsKey::Step { step: b, .. }) => a == b,
            (NoteRows::Final, RowsKey::Final { file, .. }) => *file == anchor.file,
            _ => false,
        };
        here.then_some(RangeMark {
            side: anchor.side,
            line: anchor.line,
            last: anchor.last,
            oid: Some(anchor.oid),
        })
    }

    // ----- without a mouse -----------------------------------------------

    /// `ExtendRangeUp` / `ExtendRangeDown`: the range from the selected row
    /// (or the range there is) grows by one row at its end, as a drag by
    /// one row would make it. A fold, or the edge of the diff, stops it.
    pub(crate) fn extend_range(&mut self, direction: Direction, cx: &mut Context<ReviewModel>) {
        let (rows, from, at, half) = match (&self.range, &self.selection) {
            (Some(range), _) => (range.rows.clone(), range.from, range.at, range.half),
            (None, Some(ItemKey::Row { rows, old, new })) => {
                (rows.clone(), (*old, *new), (*old, *new), None)
            }
            _ => return,
        };
        // The key the screen holds the diff under now.
        let Some((key, display)) = self
            .displays
            .iter()
            .find(|(key, _)| items::same_diff(key, &rows))
            .map(|(key, display)| (key.clone(), display.clone()))
        else {
            return;
        };
        let find = |lines: (Option<u32>, Option<u32>)| {
            display.iter().position(|row| {
                row.fold.is_none()
                    && row.left.map(|side| side.line) == lines.0
                    && row.right.map(|side| side.line) == lines.1
            })
        };
        let (Some(start), Some(end)) = (find(from), find(at)) else {
            return;
        };
        let next = match direction {
            Direction::Forward => end + 1,
            Direction::Backward => match end.checked_sub(1) {
                Some(next) => next,
                None => return,
            },
        };
        if display.get(next).is_none_or(|row| row.fold.is_some()) {
            return;
        }
        let Some(span) = items::range_of(&display, start, next, half, self.mode) else {
            return;
        };
        let Some(oid) = self.side_oid(&key, span.side) else {
            return;
        };
        let line_of = |ix: usize| {
            display
                .get(ix)
                .map(|row| {
                    (
                        row.left.map(|side| side.line),
                        row.right.map(|side| side.line),
                    )
                })
                .unwrap_or_default()
        };
        let at = line_of(next);
        let row_key = ItemKey::Row {
            rows: key.clone(),
            old: at.0,
            new: at.1,
        };
        self.select(Some(row_key.clone()), cx);
        self.range = Some(LineRange {
            rows: key,
            side: span.side,
            line: span.line,
            last: span.last,
            oid,
            from,
            at,
            half,
        });
        if let Some(item) = anchor::find(&self.items, &row_key) {
            self.list.scroll_to_reveal_item(item);
        }
        debug!(
            "note range";
            id = self.id.0,
            how = "set",
            lines = span.last - span.line + 1,
            mode = diag::mode_kind(self.mode)
        );
        cx.notify();
    }
}
