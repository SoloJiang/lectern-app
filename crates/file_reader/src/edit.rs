//! The edit mode (docs/design/file_reader.md「编辑」「界面 / 编辑模式」):
//! gpui-component's code editor over the buffer, `syntax`'s
//! [`EditSession`] for the domain, and the highlighter that joins them.
//!
//! This module holds the state of an edit ([`Editing`], `None` until the
//! user enters edit mode: nothing here is allocated before), the
//! [`Highlighter`] adapter (the editor's `InputHighlighter`, which feeds
//! the session the buffer's edits and reads its spans back) and what the
//! view does with them: entering, saving, merging, discarding, and the
//! follow-up to what the disk side reports. The drawing is in
//! `render_edit.rs`.
//!
//! The leave guard (step 8b) is in `edit/leave.rs`: every way out of a
//! buffer with unsaved changes (Done, Esc, ⌘E, a request to open another
//! file, back and forward by name, and what `workspace` and `app` ask for)
//! asks the user first. Discard My Changes is the user's explicit choice and
//! goes through.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Entity, HighlightStyle, SharedString, Subscription, Window};
use gpui_component::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputEvent, InputHighlighter,
    Position, Rope, RopeExt,
};
use lectern_logging::{info, warn};
use lectern_services::reader::OpenOutcome;
use lectern_syntax::{
    EditSession, Editable, Languages, LineNo, NotEditable, Row, SaveResult, TextEdit, WindowSpans,
};
use lectern_ui::{Palette, colors};

mod leave;

pub(crate) use leave::{Leave, LeaveAsk, LeaveGuard, Leaving, Port};

use crate::diag;
use crate::session::Request;
use crate::text::highlight_style;
use crate::view::ReadingView;

/// Whether the assembled app can enter edit mode: yes, now that the leave
/// guard asks before anything drops a buffer with unsaved changes (step 8b).
/// The tests turn it off for a plugin (`FileReaderPlugin::with_edit`) to
/// check the switch itself.
pub(crate) const EDIT_REACHABLE: bool = true;

/// How long after Discard My Changes (or a silent reload) the view waits for
/// the document to be read again before it gives up entering edit mode on
/// its own.
const REENTER_WINDOW: Duration = Duration::from_secs(5);

// ----- the highlighter adapter ---------------------------------------------

/// How many times the editor asked a highlighter for styles (tests).
#[cfg(test)]
static STYLED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The calls to [`Highlighter::styles`] so far (tests).
#[cfg(test)]
pub(crate) fn styled_calls() -> u64 {
    STYLED.load(std::sync::atomic::Ordering::SeqCst)
}

/// What the highlighter hands to the view: the edits the editor made since
/// the last [`Link::take`], and the buffer's text after the last of them.
#[derive(Default)]
struct Pending {
    edits: Vec<TextEdit>,
    text: Option<Rope>,
    /// A change without an edit (the editor did not say what changed): the
    /// session works the edit out from the two texts.
    unknown: bool,
}

/// The state the view shares with the highlighters it hands the editor. The
/// editor drops its highlighter and makes another (through the factory)
/// whenever its text is replaced (`set_value`), so nothing that has to last
/// lives in the highlighter itself.
pub(crate) struct Link {
    pending: RefCell<Pending>,
    /// How many changes the editor made to its text, counted where every
    /// mutation passes (the editor drives its highlighter on typing, IME
    /// input, paste, undo and redo alike): what "unsaved" is measured by.
    changes: Cell<u64>,
    /// The palette of the window, for the colours of the spans: the view
    /// sets it every frame (`styles` has no window to ask).
    palette: Cell<Palette>,
}

impl Link {
    fn new(palette: Palette) -> Link {
        Link {
            pending: RefCell::default(),
            changes: Cell::new(0),
            palette: Cell::new(palette),
        }
    }

    /// One change of the editor's text: `text` is the buffer after it.
    fn note(&self, edit: Option<InputEdit>, text: &Rope) {
        let mut pending = self.pending.borrow_mut();
        match edit {
            // Nothing changed (the editor's first update of a text it was
            // given): the session already has it.
            Some(e) if e.start_byte == e.old_end_byte && e.old_end_byte == e.new_end_byte => {
                return;
            }
            Some(e) => pending.edits.push(TextEdit {
                start: e.start_byte,
                old_end: e.old_end_byte,
                new_end: e.new_end_byte,
            }),
            None => pending.unknown = true,
        }
        pending.text = Some(text.clone());
        self.changes.set(self.changes.get() + 1);
    }

    /// The edits since the last call and the buffer they lead to; None when
    /// nothing changed.
    fn take(&self) -> Option<(Rope, Vec<TextEdit>)> {
        let mut pending = self.pending.borrow_mut();
        let text = pending.text.take()?;
        pending.unknown = false;
        Some((text, std::mem::take(&mut pending.edits)))
    }
}

/// The editor's `InputHighlighter` over a session: edits go to the shared
/// [`Link`] (the view passes them to the session once per frame), styles
/// come from the session's spans. It holds the session weakly: the view's
/// handle is the one that keeps the session alive, and ends it.
pub(crate) struct Highlighter {
    session: Weak<EditSession>,
    link: Rc<Link>,
}

impl Highlighter {
    fn new(session: &Rc<EditSession>, link: &Rc<Link>) -> Highlighter {
        Highlighter {
            session: Rc::downgrade(session),
            link: link.clone(),
        }
    }
}

impl InputHighlighter for Highlighter {
    fn language(&self) -> SharedString {
        SharedString::default()
    }

    fn update(
        &mut self,
        edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        self.link.note(edit, text);
    }

    fn update_batch(
        &mut self,
        edits: &[(InputEdit, Rope)],
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        for (edit, text) in edits {
            self.link.note(Some(*edit), text);
        }
    }

    fn styles(
        &self,
        range: &Range<usize>,
        _resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        #[cfg(test)]
        STYLED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let palette = self.link.palette.get();
        match self.session.upgrade() {
            Some(session) => runs(&session.spans(), range, &palette),
            None if range.is_empty() => Vec::new(),
            None => vec![(range.clone(), HighlightStyle::default())],
        }
    }

    /// No folding: the editor is not asked for fold candidates.
    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

/// The style runs of `range` from the spans: ordered, not overlapping, and
/// covering the range, with the default style between the tokens. Only the
/// lines that touch the range are looked at.
pub(crate) fn runs(
    spans: &WindowSpans,
    range: &Range<usize>,
    palette: &Palette,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::new();
    if range.is_empty() {
        return out;
    }
    let mut at = range.start;
    let none = HighlightStyle::default();
    for (line_start, tokens) in spans.lines_in(range.start as u64..range.end as u64) {
        let line_start = line_start as usize;
        for token in tokens {
            let start = (line_start + token.start() as usize).max(at);
            let end = (line_start + token.end() as usize).min(range.end);
            if start >= end {
                continue;
            }
            if at < start {
                out.push((at..start, none));
            }
            out.push((start..end, highlight_style(token.kind(), palette)));
            at = end;
        }
    }
    if at < range.end {
        out.push((at..range.end, none));
    }
    out
}

// ----- the state of an edit --------------------------------------------------

/// An edit of the document shown: the session, the editor, and what the
/// view knows about the buffer against the file.
pub(crate) struct Editing {
    pub(crate) session: Rc<EditSession>,
    pub(crate) editor: Entity<EditorState>,
    link: Rc<Link>,
    /// The buffer as the session was last told it.
    text: Arc<str>,
    /// `Link::changes` when the buffer last equalled the file: entering, a
    /// save.
    clean_at: u64,
    /// The changes at each save asked and not answered yet, oldest first,
    /// and whether it was Save Anyway.
    asked: VecDeque<(u64, bool)>,
    seen_saves: u64,
    seen_merges: u64,
    /// A merge is on its way: the editor takes no input until its result is
    /// in the buffer.
    pub(crate) merging: bool,
    /// The user chose Keep Editing: the row is gone and Save is Save Anyway
    /// while the file stays changed on disk.
    pub(crate) keep_editing: bool,
    /// The last save failed (the line under the toolbar says so).
    pub(crate) failed: bool,
    unavailable_logged: bool,
    /// The question the user is being asked, or whose save is on its way
    /// (`edit/leave.rs`), and the guard's questions that wait behind it.
    pub(crate) leaving: Option<Leaving>,
    backlog: VecDeque<Leave>,
    /// The editor's first frame was reported as `Milestone::EditReady`.
    pub(crate) ready: bool,
    /// Takes the guard's questions while the edit lives.
    _port: Port,
    _changes: Subscription,
}

impl Editing {
    /// The buffer as the session was last told it (what the editor holds,
    /// once the frame's edits are passed on).
    #[cfg(test)]
    pub(crate) fn text(&self) -> &Arc<str> {
        &self.text
    }

    /// The buffer has changes the file does not.
    pub(crate) fn dirty(&self) -> bool {
        self.link.changes.get() != self.clean_at
    }

    /// Hands the session the edits made since the last call (the buffer is
    /// copied once for all of them).
    pub(crate) fn flush(&mut self) {
        if let Some((rope, edits)) = self.link.take() {
            let text: Arc<str> = Arc::from(rope.to_string());
            self.session.changed(text.clone(), &edits);
            self.text = text;
        }
    }
}

/// Entering edit mode again after the session ended, on the document read
/// again (Discard My Changes, the silent reload of a clean buffer).
pub(crate) struct Reenter {
    serial: u64,
    /// The content the document had when the session ended: the document is
    /// read again when its content is another one.
    oid: Option<lectern_services::project::BlobId>,
    line: u32,
    until: Instant,
}

/// Why Edit… is not available, as the reason's Fluent key.
pub(crate) fn reason_key(reason: NotEditable) -> lectern_i18n::Key {
    use lectern_i18n::keys::file_reader as k;
    match reason {
        NotEditable::NotLoaded => k::NOT_EDITABLE_NOT_LOADED,
        NotEditable::TooLarge => k::NOT_EDITABLE_TOO_LARGE,
        NotEditable::LongLine => k::NOT_EDITABLE_LONG_LINE,
        NotEditable::NotText => k::NOT_EDITABLE_NOT_TEXT,
        NotEditable::MixedLineEndings => k::NOT_EDITABLE_LINE_ENDINGS,
        NotEditable::ReadOnlySource => k::NOT_EDITABLE_READ_ONLY,
        NotEditable::Reloading => k::NOT_EDITABLE_RELOADING,
    }
}

// ----- what the view does ---------------------------------------------------

impl ReadingView {
    /// Whether the view is editing.
    pub(crate) fn editing(&self) -> bool {
        self.edit.is_some()
    }

    /// Why the document cannot be edited now; None when it can.
    pub(crate) fn not_editable(&self) -> Option<NotEditable> {
        match self.snapshot.as_ref().map(|s| s.editable) {
            Some(Editable::Yes) => None,
            Some(Editable::No(reason)) => Some(reason),
            None => Some(NotEditable::NotLoaded),
        }
    }

    /// The Save button says Save Anyway: the user kept editing a file that
    /// changed on disk.
    pub(crate) fn save_anyway(&self) -> bool {
        self.edit
            .as_ref()
            .is_some_and(|edit| edit.keep_editing && edit.session.state().dirty_on_disk)
    }

    /// Edit… (⌘E): enters edit mode, or leaves it.
    pub(crate) fn toggle_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing() {
            self.leave_edit(window, cx);
        } else {
            self.enter_edit(window, cx);
        }
    }

    /// Enters edit mode at the first line in view. False when it is not
    /// possible (not reachable, already editing, not editable now).
    pub(crate) fn enter_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.shared.edit_reachable() || self.editing() {
            return false;
        }
        let line = self.first_line();
        self.reenter = None;
        self.start_editing(line, window, cx)
    }

    /// The line number of the first row in view, when it is exact.
    fn first_line(&self) -> Option<u32> {
        let snapshot = self.snapshot.as_ref()?;
        match snapshot.rows(self.top, 1, self.cols()).first() {
            Some(Row::Text {
                line: LineNo::Exact(line),
                ..
            }) => u32::try_from(*line).ok(),
            _ => None,
        }
    }

    /// Makes the editor over a new session and puts the cursor at `line`.
    fn start_editing(
        &mut self,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(snapshot) = self.snapshot.clone() else {
            return false;
        };
        if snapshot.editable != Editable::Yes {
            return false;
        }
        let session = match self.shared.session.edit() {
            Ok(session) => Rc::new(session),
            Err(reason) => {
                // The snapshot said yes and the document said no (the file
                // is being read again): the button is grey in a moment.
                info!(
                    "edit mode could not start; the document is not editable now";
                    reason = diag::not_editable(reason),
                );
                return false;
            }
        };
        self.close_find(window, cx);
        let link = Rc::new(Link::new(*colors(window, cx)));
        let text = session.initial_text();
        let language = Languages::core()
            .for_path(&snapshot.target.path)
            .map(|lang| lang.name().to_owned())
            .unwrap_or_default();
        let factory_session = Rc::downgrade(&session);
        let factory_link = link.clone();
        let editor = cx.new(|cx| {
            let mut editor = EditorState::new(window, cx)
                .language(language)
                .default_value(SharedString::from(text.clone()))
                .soft_wrap(false)
                .folding(false);
            editor.set_highlighter_factory(
                Rc::new(move |_| {
                    let session = factory_session.upgrade()?;
                    Some(Box::new(Highlighter::new(&session, &factory_link))
                        as Box<dyn InputHighlighter>)
                }),
                cx,
            );
            editor
        });
        let changes = cx.subscribe_in(&editor, window, |view, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                view.buffer_changed(cx);
            }
        });
        let state = session.state();
        let port = Port::open(self, window, cx);
        self.edit = Some(Editing {
            session,
            editor: editor.clone(),
            link,
            text,
            clean_at: 0,
            asked: VecDeque::new(),
            seen_saves: state.saves,
            seen_merges: state.merges,
            merging: false,
            keep_editing: false,
            failed: false,
            unavailable_logged: false,
            leaving: None,
            backlog: VecDeque::new(),
            ready: false,
            _port: port,
            _changes: changes,
        });
        editor.update(cx, |editor, cx| match line {
            Some(line) if line > 0 => {
                // The file may have lost lines since the cursor was there.
                let last =
                    u32::try_from(editor.text().lines_len().saturating_sub(1)).unwrap_or(u32::MAX);
                editor.set_cursor_position(Position::new(line.min(last), 0), window, cx)
            }
            _ => editor.focus(window, cx),
        });
        cx.notify();
        true
    }

    /// The buffer changed by the user's hand: the frame draws it (and what
    /// is unsaved is counted where the editor hands over its edits).
    fn buffer_changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    /// Done (⌘E, Esc): leaves edit mode. A buffer with unsaved changes asks
    /// the user first (the leave guard); true when it left at once.
    pub(crate) fn leave_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.edit.is_none() {
            return true;
        }
        self.begin_leave(Leave::Done, window, cx);
        self.edit.is_none()
    }

    /// The session ends (its handle goes; the document is read again when a
    /// save went through or the file changed) and the view reads again.
    pub(crate) fn finish_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_edit(window, cx);
        // The requests that waited while the buffer was edited.
        self.pump(window, cx);
        cx.notify();
    }

    /// The edit is gone: the editor, the session and the port of the guard.
    pub(crate) fn drop_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.take().is_some() {
            window.focus(&self.focus, cx);
        }
    }

    /// A request to show another file, after the leave guard passed: the
    /// document is replaced and the view starts at its position.
    pub(crate) fn show(&mut self, request: Request, cx: &mut Context<Self>) {
        if self.shared.session.apply(request) == OpenOutcome::Shown {
            self.serial += 1;
            self.new_document(cx);
        }
    }

    /// Save (⌘S, the button): the buffer is written if the file is as it was
    /// read; a file changed on disk is a conflict, not a write. With
    /// `anyway` the file is replaced without the comparison (Save Anyway).
    ///
    /// True when a save was asked (there were changes and no merge was on its
    /// way).
    pub(crate) fn save(&mut self, anyway: bool, cx: &mut Context<Self>) -> bool {
        // Save Anyway is for a file the user chose to keep editing over; by
        // name (the probe) it must not skip the identity check otherwise.
        if anyway && !self.save_anyway() {
            return false;
        }
        let Some(edit) = self.edit.as_mut() else {
            return false;
        };
        if !edit.dirty() || edit.merging {
            return false;
        }
        edit.flush();
        edit.asked.push_back((edit.link.changes.get(), anyway));
        if anyway {
            edit.session.save_anyway(edit.text.clone());
        } else {
            edit.session.save(edit.text.clone());
        }
        edit.failed = false;
        cx.notify();
        true
    }

    /// Merge, in the row under the toolbar: the disk's changes come into the
    /// buffer. The result is written back when it arrives (`edit_tick`).
    pub(crate) fn merge(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        if edit.merging || !edit.session.state().dirty_on_disk {
            return;
        }
        edit.flush();
        // Typing while the merge runs would be lost under its result.
        edit.merging = true;
        edit.editor
            .update(cx, |editor, cx| editor.set_readonly(true, cx));
        edit.session.merge(edit.text.clone());
        cx.notify();
    }

    /// Keep Editing: the row goes, the file stays changed on disk, and Save
    /// becomes Save Anyway.
    pub(crate) fn keep_editing(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = self.edit.as_mut() {
            edit.keep_editing = true;
            cx.notify();
        }
    }

    /// Discard My Changes: the buffer goes, the file is read again and edit
    /// mode starts over on it (when the file is gone it only ends).
    pub(crate) fn discard_mine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Only where the row's button is (or was, before Keep Editing): by
        // name (the probe) it must not drop a buffer of a file that did not
        // change, which would be an exit the leave guard does not ask about.
        let Some(edit) = self.edit.as_ref() else {
            return;
        };
        // A question is open: its answer decides what becomes of the buffer.
        if edit.leaving.is_some() {
            return;
        }
        let state = edit.session.state();
        if state.dirty_on_disk || state.unavailable {
            self.restart(Why::Discard, window, cx);
        }
    }

    /// Ends the session and waits for the document to be read again.
    fn restart(&mut self, why: Why, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.as_ref() else {
            return;
        };
        let line = edit.editor.read(cx).cursor_position().line;
        let unavailable = edit.session.state().unavailable;
        info!(
            "the buffer was given up for the file on disk";
            why = why.name(),
            unavailable = unavailable,
        );
        self.reenter = (!unavailable).then(|| Reenter {
            serial: self.serial,
            oid: self.snapshot.as_ref().and_then(|s| s.oid),
            line,
            until: Instant::now() + REENTER_WINDOW,
        });
        self.finish_edit(window, cx);
    }

    /// Before each frame: the edits the editor made go to the session, and
    /// the highlighter's colours are the window's.
    pub(crate) fn flush_edit(&mut self, palette: &Palette) {
        if let Some(edit) = self.edit.as_mut() {
            edit.link.palette.set(*palette);
            edit.flush();
        }
    }

    /// After every wake-up of the session (main thread): takes what the
    /// disk side has reported (saves, merges, the file changed on disk), and
    /// enters edit mode again once the document was read again.
    pub(crate) fn edit_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.try_reenter(window, cx);
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let state = edit.session.state();
        let mut merged = None;
        if state.saves > edit.seen_saves {
            let done = state.saves - edit.seen_saves;
            edit.seen_saves = state.saves;
            // The results run together when several came between two
            // wake-ups; `save` is the last one's, for the last one asked.
            let mut asked = None;
            for _ in 0..done {
                asked = edit.asked.pop_front();
            }
            match &state.save {
                Some(SaveResult::Saved) => {
                    edit.clean_at = asked.map_or(edit.link.changes.get(), |(changes, _)| changes);
                    edit.failed = false;
                }
                Some(SaveResult::Conflict) => edit.failed = false,
                Some(SaveResult::Failed(_)) => {
                    edit.failed = true;
                    warn!(
                        "a save failed; the buffer stays unsaved";
                        forced = asked.is_some_and(|(_, forced)| forced),
                    );
                }
                None => {}
            }
        }
        if state.merges > edit.seen_merges {
            edit.seen_merges = state.merges;
            edit.merging = false;
            merged = Some(state.merged.clone());
        }
        if state.unavailable && !edit.unavailable_logged {
            edit.unavailable_logged = true;
            warn!(
                "the edited file is no longer available; only copying or discarding the buffer is left"
            );
        }
        if !state.dirty_on_disk {
            edit.keep_editing = false;
        }
        let silent = state.dirty_on_disk && !state.unavailable && !edit.dirty() && !edit.merging;
        // The save the user chose in the confirmation has its result.
        self.save_result(window, cx);
        if let Some(merged) = merged {
            self.merged(merged, window, cx);
        } else if silent {
            self.restart(Why::Clean, window, cx);
        }
        cx.notify();
    }

    /// A merge's result goes into the buffer, still unsaved; with conflicts
    /// the cursor goes to the first one.
    fn merged(
        &mut self,
        outcome: Option<Arc<lectern_syntax::MergeOutcome>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let editor = edit.editor.clone();
        let Some(outcome) = outcome else {
            // The file could not be read: `unavailable` says so.
            editor.update(cx, |editor, cx| editor.set_readonly(false, cx));
            return;
        };
        // Unsaved, even when the merge changed nothing.
        edit.link.changes.set(edit.link.changes.get() + 1);
        edit.keep_editing = false;
        let conflicts = outcome.conflicts.len();
        if conflicts > 0 {
            info!(
                "a merge left conflict markers in the buffer";
                conflicts = conflicts,
            );
        }
        let first = outcome.conflicts.first().map(|range| range.start);
        // `set_value` and not a replacement that can be undone: the file on
        // disk is the merge base now, and undoing to the text from before
        // the merge would save over the disk's changes without a conflict.
        editor.update(cx, |editor, cx| {
            editor.set_readonly(false, cx);
            editor.set_value(SharedString::from(outcome.text.clone()), window, cx);
            match first {
                Some(offset) => {
                    let at = editor.text().offset_to_position(offset);
                    editor.set_cursor_position(at, window, cx);
                }
                None => editor.focus(window, cx),
            }
        });
    }

    /// Enters edit mode on the document read again, if that is what the
    /// view waits for.
    fn try_reenter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wait) = &self.reenter else {
            return;
        };
        if self.edit.is_some() || wait.serial != self.serial || Instant::now() > wait.until {
            self.reenter = None;
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        // Read again: another content, all of it in memory.
        if snapshot.editable != Editable::Yes || snapshot.oid.is_none() || snapshot.oid == wait.oid
        {
            return;
        }
        let line = wait.line;
        self.reenter = None;
        self.start_editing(Some(line), window, cx);
    }
}

/// Why the buffer was given up for the file.
#[derive(Clone, Copy)]
enum Why {
    /// The user chose Discard My Changes.
    Discard,
    /// The file changed on disk and the buffer had no changes of its own.
    Clean,
}

impl Why {
    fn name(self) -> &'static str {
        match self {
            Why::Discard => "discard",
            Why::Clean => "clean",
        }
    }
}
