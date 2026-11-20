//! The reading view's state and what it does (docs/design/file_reader.md
//! 「界面」「文本与显示行」「数据流与线程」): it takes the session's requests
//! and snapshots on the main thread, keeps the scroll position as a
//! [`RowPos`] and a pixel offset, and runs the find bar, the history and
//! the pointer's prefetch. It reads snapshots and sends commands; no IO,
//! no parsing, no git (architecture rule 4). The drawing is in
//! `render.rs`.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AppContext, Bounds, Context, Entity, FocusHandle, Focusable, Pixels, Point, Subscription, Task,
    Window, px,
};
use lectern_services::code_intel::{Position, Query, QueryKind};
use lectern_services::workspace::{Milestone, WorkspaceCommand};
use lectern_syntax::{
    DocSnapshot, DocState, FindQuery, FindResults, LineNo, Row, RowPos, Viewport,
};
use lectern_ui::motion::Transition;
use lectern_ui::{MonoMetrics, TextAreaState};

use crate::edit::{Editing, Leave, Reenter};
use crate::shared::Shared;
use crate::text::{digits, identifier_cell};

/// How long the pointer rests on an identifier before `CodeIntel::prefetch`
/// (docs/design/file_reader.md「数据流与线程」).
pub(crate) const HOVER_DELAY: Duration = Duration::from_millis(150);

/// The find bar's height when it is open.
pub(crate) const FIND_BAR_HEIGHT: Pixels = px(40.0);

/// Whole cells of room kept at the right of the longest line.
const RIGHT_MARGIN_CELLS: f32 = 2.0;

/// The find bar: whether it is open, its field, its two transitions and
/// where the reader is in the matches.
pub(crate) struct FindBar {
    pub open: bool,
    pub field: Entity<TextAreaState>,
    pub height: Transition,
    pub opacity: Transition,
    /// The text the session was last asked to find.
    pub query: Arc<str>,
    /// The match the reader is at, an index in the results' ranges.
    pub current: Option<usize>,
    /// The query changed and the first match has not been taken yet.
    pub wants_first: bool,
    _text: Subscription,
}

/// Where the pointer rests: a display row (by position) and a cell of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Resting {
    serial: u64,
    pos: RowPos,
    cell: usize,
}

pub struct ReadingView {
    pub(crate) shared: Arc<Shared>,
    pub(crate) id: u64,
    pub(crate) focus: FocusHandle,
    pub(crate) snapshot: Option<Arc<DocSnapshot>>,
    /// Bumped each time the session shows another document (open, back,
    /// forward): what a position, a find and a rest belong to.
    pub(crate) serial: u64,
    /// The `(serial, generation)` the position was last taken from.
    applied: Option<(u64, u64)>,
    /// The scroll position: the first display row and how many pixels of it
    /// are scrolled off (under one line).
    pub(crate) top: RowPos,
    pub(crate) offset: Pixels,
    pub(crate) scroll_x: Pixels,
    /// The code area's bounds in the last frame, measured by the element.
    pub(crate) area: Rc<Cell<Bounds<Pixels>>>,
    pub(crate) metrics: Option<MonoMetrics>,
    sent: Option<Viewport>,
    pub(crate) opened_at: Instant,
    pub(crate) find: FindBar,
    /// The display rows of the last frame, for the pointer.
    pub(crate) drawn: Vec<RowPos>,
    /// How many rows were laid out in the last frame.
    pub(crate) built_rows: usize,
    /// The document the milestone was reported for.
    reported: Option<u64>,
    resting: Option<Resting>,
    hover: Option<Task<()>>,
    /// The prefetches made, for the tests.
    pub(crate) prefetched: u32,
    /// The edit mode's state: None until the user enters edit mode (nothing
    /// of it exists before).
    pub(crate) edit: Option<Editing>,
    /// Edit mode starts again when the document has been read again.
    pub(crate) reenter: Option<Reenter>,
    _listen: Task<()>,
}

impl ReadingView {
    pub(crate) fn new(
        shared: Arc<Shared>,
        window: &mut Window,
        cx: &mut Context<ReadingView>,
    ) -> ReadingView {
        // The session wakes this from any thread; the foreground task takes
        // it. One message waits at most: a wake-up while one is queued adds
        // nothing.
        let (tx, mut rx) = mpsc::unbounded::<()>();
        let queued = Arc::new(AtomicBool::new(false));
        let flag = queued.clone();
        let id = shared.begin_view(Arc::new(move || {
            if !flag.swap(true, Ordering::SeqCst) {
                let _ = tx.unbounded_send(());
            }
        }));
        let listen = cx.spawn_in(window, async move |this, cx| {
            while rx.next().await.is_some() {
                queued.store(false, Ordering::SeqCst);
                let alive = this.update_in(cx, |view, window, cx| {
                    view.pump(window, cx);
                    view.edit_tick(window, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let field = cx.new(|cx| TextAreaState::new(window, cx));
        let text = cx.observe(&field, |view, field, cx| {
            let text = field.read(cx).text();
            view.find_text_changed(text.as_ref(), cx);
        });
        ReadingView {
            shared,
            id,
            focus: cx.focus_handle(),
            snapshot: None,
            serial: 0,
            applied: None,
            top: RowPos::default(),
            offset: px(0.0),
            scroll_x: px(0.0),
            area: Rc::new(Cell::new(Bounds::default())),
            metrics: None,
            sent: None,
            opened_at: Instant::now(),
            find: FindBar {
                open: false,
                field,
                height: Transition::new(0.0),
                opacity: Transition::new(0.0),
                query: Arc::from(""),
                current: None,
                wants_first: false,
                _text: text,
            },
            drawn: Vec::new(),
            built_rows: 0,
            reported: None,
            resting: None,
            hover: None,
            prefetched: 0,
            edit: None,
            reenter: None,
            _listen: listen,
        }
    }

    /// Takes what waits in the session: the requests (each goes through the
    /// leave guard while a buffer is edited), then the snapshot. Runs on the
    /// main thread after every wake-up.
    pub(crate) fn pump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.shared.session.clone();
        let mut asked = false;
        // While a question is open the requests wait for its answer, in
        // order.
        while !self.leaving()
            && let Some(request) = session.next_request()
        {
            asked = true;
            if self.editing() {
                // Passes at once when nothing is unsaved; otherwise the user
                // is asked, and the answer shows or cancels the request.
                self.begin_leave(Leave::Open(request), window, cx);
            } else {
                self.show(request, cx);
            }
        }
        let snapshot = session.sync();
        if snapshot.is_none() && asked && !self.leaving() {
            // The only request could not be placed: there is nothing to read
            // here, back to where the reader was opened from.
            self.shared.dispatch(WorkspaceCommand::Pop);
        }
        self.take(snapshot, cx);
        cx.notify();
    }

    /// Another document: the scroll position and the pointer's rest are not
    /// the old one's, and a find that is open runs in the new one.
    pub(crate) fn new_document(&mut self, cx: &mut Context<Self>) {
        self.top = RowPos::default();
        self.offset = px(0.0);
        self.scroll_x = px(0.0);
        self.opened_at = Instant::now();
        self.resting = None;
        self.hover = None;
        self.find.current = None;
        self.drawn.clear();
        if self.find.open && !self.find.query.is_empty() {
            self.find.wants_first = true;
            self.shared.session.find(Some(FindQuery {
                text: self.find.query.clone(),
            }));
        }
        cx.notify();
    }

    fn take(&mut self, snapshot: Option<Arc<DocSnapshot>>, cx: &mut Context<Self>) {
        if let Some(snapshot) = &snapshot {
            let now = (self.serial, snapshot.generation);
            if self.applied != Some(now) {
                self.applied = Some(now);
                self.top = snapshot.anchor;
                self.offset = px(0.0);
                self.scroll_x = px(0.0);
                self.shared.session.scrolled(self.top);
            }
        }
        self.snapshot = snapshot;
        self.sync_find(cx);
    }

    // ----- the viewport ---------------------------------------------------

    /// The width of the line number column, in cells.
    pub(crate) fn gutter_cells(&self) -> usize {
        let lines = self.snapshot.as_ref().map_or(1, |s| s.lines.get());
        digits(lines).max(3) + 2
    }

    /// What the code area holds, from its last measured bounds: display
    /// rows (the last one may be cut) and cells of a row.
    fn viewport(&self, metrics: &MonoMetrics) -> Option<Viewport> {
        let bounds = self.area.get();
        let (width, height) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let gutter = self.gutter_cells() as f32 * metrics.cell_width.as_f32();
        let cols = ((width - gutter) / metrics.cell_width.as_f32())
            .floor()
            .max(1.0);
        let rows = (height / metrics.line_height.as_f32()).ceil().max(1.0);
        Some(Viewport {
            rows: rows as u32,
            cols: cols as u32,
        })
    }

    /// Tells the session the viewport when it changed. Called by the
    /// renderer, which measured it.
    pub(crate) fn sync_viewport(&mut self, metrics: &MonoMetrics) {
        self.metrics = Some(*metrics);
        let Some(viewport) = self.viewport(metrics) else {
            return;
        };
        if self.sent != Some(viewport) {
            self.sent = Some(viewport);
            self.shared.session.set_viewport(viewport);
        }
    }

    /// The columns display rows are folded at.
    pub(crate) fn cols(&self) -> u32 {
        self.sent.map_or(100, |viewport| viewport.cols)
    }

    pub(crate) fn rows_visible(&self) -> usize {
        self.sent.map_or(40, |viewport| viewport.rows as usize)
    }

    /// The milestone: the first screen of a document is drawn, highlighted
    /// or as plain text. Once per document.
    pub(crate) fn drawn_ready(&mut self, window: &mut Window) {
        if self.reported == Some(self.serial) {
            return;
        }
        let ready = self.snapshot.as_ref().is_some_and(|s| {
            s.state == DocState::Ready && s.highlight != lectern_syntax::Highlighting::Pending
        });
        if !ready {
            return;
        }
        self.reported = Some(self.serial);
        let shared = self.shared.clone();
        window.on_next_frame(move |_, _| shared.report_milestone(Milestone::FileHighlighted));
    }

    // ----- scrolling ------------------------------------------------------

    /// The content moves by `delta` pixels (a wheel or a trackpad): down is
    /// negative, as GPUI reports it.
    pub(crate) fn scroll_by(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(metrics), Some(snapshot)) = (self.metrics, self.snapshot.clone()) else {
            return;
        };
        let line = metrics.line_height.as_f32();
        let cols = self.cols();
        let before = (self.top, self.offset);
        if delta.y.as_f32() != 0.0 {
            let moved = self.offset.as_f32() - delta.y.as_f32();
            let whole = (moved / line).floor();
            let mut top = snapshot.advance(self.top, whole as i64, cols);
            let mut offset = moved - whole * line;
            // Past the end the last rows fill the viewport; at the start
            // nothing is scrolled off the first row. Either way no pixel
            // offset is left.
            let rows = self.rows_visible();
            let shown = snapshot.rows(top, rows, cols).len();
            if shown < rows {
                top = snapshot.advance(top, -((rows - shown) as i64), cols);
                offset = 0.0;
            }
            if snapshot.advance(top, -1, cols) == top {
                offset = 0.0;
            }
            self.top = top;
            self.offset = px(offset.clamp(0.0, line - 0.01));
        }
        if delta.x.as_f32() != 0.0 {
            self.scroll_x = self.clamp_x(self.scroll_x - delta.x, &snapshot);
        }
        if (self.top, self.offset) != before {
            self.scrolled(cx);
        }
        cx.notify();
    }

    fn clamp_x(&self, x: Pixels, snapshot: &DocSnapshot) -> Pixels {
        let Some(metrics) = self.metrics else {
            return px(0.0);
        };
        let cell = metrics.cell_width.as_f32();
        let bounds = self.area.get();
        let code = bounds.size.width.as_f32() - self.gutter_cells() as f32 * cell;
        let widest = (snapshot.widest_cells as f32 + RIGHT_MARGIN_CELLS) * cell;
        px(x.as_f32().clamp(0.0, (widest - code).max(0.0)))
    }

    /// The position changed: the document highlights there, the history
    /// remembers it, and what the pointer rests on is not under it any more.
    fn scrolled(&mut self, cx: &mut Context<Self>) {
        self.shared.session.scrolled(self.top);
        self.resting = None;
        self.hover = None;
        cx.notify();
    }

    /// The row holding `byte` comes into view: nothing moves when it is
    /// already drawn, otherwise it goes a third of the way down.
    fn reveal_byte(&mut self, byte: u64, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let cols = self.cols();
        let rows = self.rows_visible();
        let pos = snapshot.pos_of_byte(byte, cols);
        let visible = snapshot.rows(self.top, rows.saturating_sub(1).max(1), cols);
        let drawn = visible.iter().any(|row| match row {
            Row::Text { pos: at, .. } | Row::Pending { pos: at } => *at == pos,
        });
        if drawn {
            return;
        }
        self.top = snapshot.advance(pos, -((rows / 3) as i64), cols);
        self.offset = px(0.0);
        self.scrolled(cx);
    }

    // ----- history ----------------------------------------------------------

    /// ⌘[: the previous position; at the start of the history, back to the
    /// view that opened the reader. While editing the toolbar and the key
    /// do not get here; by name it goes through the leave guard, since the
    /// history changes the document.
    pub(crate) fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.shared.session.can_back() {
            // `workspace` asks the guards before it pops the screen.
            self.shared.dispatch(WorkspaceCommand::Pop);
            return;
        }
        if self.editing() {
            self.begin_leave(Leave::Back, window, cx);
        } else {
            self.step_back(window, cx);
        }
    }

    /// ⌘]: the next position.
    pub(crate) fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Nothing ahead: nothing to ask about either.
        if !self.shared.session.can_forward() {
            return;
        }
        if self.editing() {
            self.begin_leave(Leave::Forward, window, cx);
        } else {
            self.step_forward(window, cx);
        }
    }

    /// The previous position, after the leave guard.
    pub(crate) fn step_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shared.session.back() {
            self.serial += 1;
            self.new_document(cx);
            self.pump(window, cx);
        }
    }

    /// The next position, after the leave guard.
    pub(crate) fn step_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shared.session.forward() {
            self.serial += 1;
            self.new_document(cx);
            self.pump(window, cx);
        }
    }

    pub(crate) fn can_forward(&self) -> bool {
        self.shared.session.can_forward()
    }

    // ----- find -----------------------------------------------------------------

    /// ⌘F: the bar opens and the cursor goes to its field.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The editor has its own search while editing.
        if self.editing() {
            return;
        }
        if !self.find.open {
            self.find.open = true;
            let now = Instant::now();
            lectern_ui::Expander::go(
                &mut self.find.height,
                &mut self.find.opacity,
                FIND_BAR_HEIGHT,
                now,
                cx,
            );
            let text: Arc<str> = Arc::from(self.find.field.read(cx).text().as_ref());
            if !text.is_empty() {
                self.find.query = text.clone();
                self.find.wants_first = true;
                self.shared.session.find(Some(FindQuery { text }));
            }
        }
        let focus = self.find.field.read(cx).focus_handle();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Esc, with the bar open: it closes, the matches go, the cursor comes
    /// back to the text. False when there was no bar to close.
    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.find.open {
            return false;
        }
        self.find.open = false;
        let now = Instant::now();
        lectern_ui::Expander::go(
            &mut self.find.height,
            &mut self.find.opacity,
            px(0.0),
            now,
            cx,
        );
        self.find.current = None;
        self.find.wants_first = false;
        self.shared.session.find(None);
        window.focus(&self.focus, cx);
        cx.notify();
        true
    }

    /// The field's text changed: a new find, whose first match is taken
    /// when its results come.
    fn find_text_changed(&mut self, text: &str, cx: &mut Context<Self>) {
        if &*self.find.query == text {
            return;
        }
        self.find.query = Arc::from(text);
        self.find.current = None;
        if !self.find.open {
            return;
        }
        if text.is_empty() {
            self.find.wants_first = false;
            self.shared.session.find(None);
        } else {
            self.find.wants_first = true;
            self.shared.session.find(Some(FindQuery {
                text: self.find.query.clone(),
            }));
        }
        cx.notify();
    }

    /// The results of the find as the snapshot has them.
    pub(crate) fn results(&self) -> Option<Arc<FindResults>> {
        self.snapshot.as_ref()?.find.clone()
    }

    /// Takes the first match once the results have one, and keeps the
    /// current match inside the results as they grow.
    fn sync_find(&mut self, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        let Some(results) = self.results() else {
            return;
        };
        if self.find.wants_first {
            if let Some(index) = first_at_or_after(&results, self.top.line_start) {
                self.find.wants_first = false;
                self.go_to_match(index, &results, cx);
            } else if results.done {
                self.find.wants_first = false;
            }
        } else if self.find.current.is_some_and(|c| c >= results.ranges.len()) {
            self.find.current = None;
        }
    }

    fn go_to_match(&mut self, index: usize, results: &FindResults, cx: &mut Context<Self>) {
        let Some(range) = results.ranges.get(index) else {
            return;
        };
        self.find.current = Some(index);
        self.reveal_byte(range.start, cx);
        cx.notify();
    }

    /// ⌘G: the next match, from the last one reached (or the top of the
    /// screen), wrapping at the end.
    pub(crate) fn find_next(&mut self, cx: &mut Context<Self>) {
        self.step_find(true, cx);
    }

    /// ⇧⌘G: the previous match.
    pub(crate) fn find_previous(&mut self, cx: &mut Context<Self>) {
        self.step_find(false, cx);
    }

    fn step_find(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        let Some(results) = self.results() else {
            return;
        };
        let len = results.ranges.len();
        if len == 0 {
            return;
        }
        let index = match (self.find.current, forward) {
            (Some(current), true) => (current + 1) % len,
            (Some(current), false) => (current + len - 1) % len,
            (None, true) => first_at_or_after(&results, self.top.line_start).unwrap_or(0),
            (None, false) => first_at_or_after(&results, self.top.line_start)
                .map_or(len - 1, |at| (at + len - 1) % len),
        };
        self.find.wants_first = false;
        self.go_to_match(index, &results, cx);
    }

    // ----- the pointer ----------------------------------------------------------

    /// The pointer moved over the code area at `position` (window
    /// coordinates). Resting 150 ms on an identifier asks `code_intel` to
    /// prefetch its definition.
    pub(crate) fn pointer_moved(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(metrics) = self.metrics else {
            return;
        };
        let bounds = self.area.get();
        let (x, y) = (
            (position.x - bounds.origin.x).as_f32(),
            (position.y - bounds.origin.y).as_f32(),
        );
        let cell = metrics.cell_width.as_f32();
        let gutter = self.gutter_cells() as f32 * cell;
        let rest = if x < gutter || y < 0.0 {
            None
        } else {
            let row = ((y + self.offset.as_f32()) / metrics.line_height.as_f32()).floor() as usize;
            let column = ((x - gutter + self.scroll_x.as_f32()) / cell).floor() as usize;
            self.drawn.get(row).map(|pos| Resting {
                serial: self.serial,
                pos: *pos,
                cell: column,
            })
        };
        if rest == self.resting {
            return;
        }
        self.resting = rest;
        self.hover = rest.map(|rest| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(HOVER_DELAY).await;
                let _ = this.update(cx, |view, _| view.rested(rest));
            })
        });
    }

    /// The pointer left the code area.
    pub(crate) fn pointer_left(&mut self) {
        self.resting = None;
        self.hover = None;
    }

    /// The pointer rested on `rest` for the delay: if it is an identifier
    /// of the document's text and nothing moved meanwhile, the definition
    /// there is prefetched.
    fn rested(&mut self, rest: Resting) {
        if self.resting != Some(rest) || rest.serial != self.serial {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if rest.pos.sub_row != 0 {
            return;
        }
        let rows = snapshot.rows(rest.pos, 1, self.cols());
        let Some(Row::Text {
            line: LineNo::Exact(line),
            bytes,
            text,
            ..
        }) = rows.first()
        else {
            return;
        };
        // The column is the byte offset in the line: the cell is that
        // where the row is printable ASCII up to it and the row starts
        // with the line (a BOM, or a long line's later rows, would shift
        // it).
        if !identifier_cell(text, rest.cell)
            || bytes.start != rest.pos.line_start
            || (bytes.end - bytes.start).saturating_sub(text.len() as u64) > 1
        {
            return;
        }
        let Some((project, at)) = self.shared.session.current_text() else {
            return;
        };
        let (Ok(line), Ok(column)) = (u32::try_from(*line), u32::try_from(rest.cell)) else {
            return;
        };
        let query = Query {
            project,
            kind: QueryKind::Definition {
                at,
                position: Position { line, column },
            },
        };
        if self.shared.prefetch(query) {
            self.prefetched += 1;
        }
    }
}

/// The index of the first match starting at or after `byte`.
fn first_at_or_after(results: &FindResults, byte: u64) -> Option<usize> {
    let index = results.ranges.partition_point(|range| range.start < byte);
    (index < results.ranges.len()).then_some(index)
}

impl Focusable for ReadingView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for ReadingView {
    /// The screen is popped: the document goes with its focus, and the
    /// plugin's wake-up is the session's again.
    fn drop(&mut self) {
        // A request that waited for the user's answer is over with the view.
        if let Some(mut edit) = self.edit.take() {
            edit.abandon(self);
        }
        self.shared.end_view(self.id);
    }
}
