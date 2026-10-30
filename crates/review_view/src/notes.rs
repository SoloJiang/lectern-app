//! What the model does with review notes (docs/design/review_view.md「审阅意见
//! （#22）」): the editor, its drafts, the commands for notes, the batch the
//! bar offers to copy again and the switch to the Conversation page after a
//! send. The notes themselves, their places and the prompt are the review
//! service's; this module reads `snapshot.notes` and `snapshot.notes_state`
//! and sends commands, and it logs counts and names only, never a note's
//! text, a draft, an excerpt or a line number.

use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, FocusHandle, SharedString, Subscription, Window};
use lectern_kernel::Name;
use lectern_logging::debug;
use lectern_services::review::{
    BatchId, DiffSide, FileKey, Note, NoteAnchor, NoteId, NoteState, ReviewCommand, RouteKind,
    RowsKey, SendOutcome, SentVia, limits,
};
use lectern_services::workspace::WorkspaceCommand;
use lectern_ui::TextAreaState;

use crate::anchor;
use crate::items::{self, EditorAt, Item, ItemKey};
use crate::model::ReviewModel;

/// The most drafts kept; the earliest is dropped first.
pub(crate) const MAX_DRAFTS: usize = 20;

/// The Conversation page of the inspector, selected after a send into the
/// session.
const CONVERSATION_TAB: &str = "conversation";

/// The open note editor: at most one per screen.
pub(crate) struct NoteEditor {
    pub(crate) target: EditorTarget,
    pub(crate) state: Entity<TextAreaState>,
    /// The text of the note being changed: saving is not possible while the
    /// text is the same.
    pub(crate) original: Option<Arc<str>>,
    /// Redraws when the text changes (the Save button, the too-long hint).
    _changed: Subscription,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum EditorTarget {
    New { rows: RowsKey, anchor: NoteAnchor },
    Existing(NoteId),
}

impl EditorTarget {
    fn name(&self) -> &'static str {
        match self {
            EditorTarget::New { .. } => "new",
            EditorTarget::Existing(_) => "existing",
        }
    }
}

/// Why the editor cannot save now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Saving {
    Ready,
    /// Nothing but white space, or the same text as before.
    Nothing,
    TooLong,
}

/// What the bar shows in its first place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Status {
    Full,
    Sending,
    Sent { via: SentVia, count: u32 },
    NotSent { route: RouteKind, reason: Reason },
    Copied { count: u32 },
    NotCopied { reason: Reason },
    Count(u32),
}

/// A reason for a failure, as the bar words it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reason {
    Starting,
    Busy,
    NoSession,
    TimedOut,
    Clipboard,
    Failed,
}

impl ReviewModel {
    // ----- the editor ---------------------------------------------------

    /// Where the open editor sits, for the item builder.
    pub(crate) fn editor_at(&self) -> Option<EditorAt> {
        match &self.editor.as_ref()?.target {
            EditorTarget::New { rows, anchor } => Some(EditorAt::Row {
                rows: rows.clone(),
                side: anchor.side,
                line: anchor.line,
                oid: anchor.oid,
            }),
            EditorTarget::Existing(id) => Some(EditorAt::AfterNote(*id)),
        }
    }

    /// C, or the + at the end of a row: the row or hunk selected becomes the
    /// anchor of a new note. Nothing selected, or a side without text: no-op.
    pub(crate) fn add_note_at_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((rows, anchor)) = self.selection_anchor() else {
            return;
        };
        if self.snapshot.notes_state.unsent >= limits::PENDING_NOTES {
            self.notes_full_hint = true;
            cx.notify();
            return;
        }
        self.open_editor(EditorTarget::New { rows, anchor }, window, cx);
    }

    /// The diff and the anchor of the selection: a row on its new side when
    /// it has one (a context line too), otherwise its old side; a hunk on
    /// its first added line, or its first removed line when it adds none.
    fn selection_anchor(&self) -> Option<(RowsKey, NoteAnchor)> {
        let selected = self.selection.as_ref()?;
        let at = anchor::find(&self.items, selected)?;
        let (rows_key, row) = match &self.items[at] {
            Item::Row { rows, ix, .. } => {
                let row = self.displays.get(rows)?.get(*ix as usize)?.clone();
                (rows.clone(), row)
            }
            Item::Hunk { rows, hunk, .. } => {
                let display = self.displays.get(rows)?;
                let in_hunk = || display.iter().filter(|row| row.hunk == Some(*hunk));
                let row = in_hunk()
                    .find(|row| {
                        row.right
                            .is_some_and(|side| side.kind == items::RowKind::Added)
                    })
                    .or_else(|| {
                        in_hunk().find(|row| {
                            row.left
                                .is_some_and(|side| side.kind == items::RowKind::Removed)
                        })
                    })?
                    .clone();
                (rows.clone(), row)
            }
            _ => return None,
        };
        let (side, line) = match (row.right, row.left) {
            (Some(side), _) => (DiffSide::New, side.line),
            (None, Some(side)) => (DiffSide::Old, side.line),
            (None, None) => return None,
        };
        let held = self.rows.get(&rows_key)?;
        let oid = match side {
            DiffSide::Old => held.old.oid?,
            DiffSide::New => held.new.oid?,
        };
        let file = items::rows_file(&self.snapshot, &rows_key)?;
        Some((
            rows_key,
            NoteAnchor {
                file,
                side,
                line,
                oid,
            },
        ))
    }

    /// Opens the editor on `target`, after saving the text of one that is
    /// open as a draft. The draft of the anchor, if there is one, is put in.
    fn open_editor(&mut self, target: EditorTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.park_editor(cx);
        let (text, original) = match &target {
            EditorTarget::New { anchor, .. } => {
                let draft = self
                    .drafts
                    .iter()
                    .position(|(at, _)| at == anchor)
                    .and_then(|ix| self.drafts.remove(ix))
                    .map(|(_, text)| text);
                (draft.unwrap_or_default(), None)
            }
            EditorTarget::Existing(id) => {
                let Some(note) = self.note(*id) else {
                    return;
                };
                (
                    SharedString::from(note.text.clone()),
                    Some(note.text.clone()),
                )
            }
        };
        let state = cx.new(|cx| TextAreaState::new(window, cx));
        if !text.is_empty() {
            state.update(cx, |state, cx| state.set_text(&text, window, cx));
        }
        let changed = cx.observe(&state, |_, _, cx| cx.notify());
        let focus = state.read(cx).focus_handle();
        debug!("note editor"; id = self.id.0, target = target.name(), how = "opened");
        self.editor = Some(NoteEditor {
            target,
            state,
            original,
            _changed: changed,
        });
        self.rebuild_in_place(cx);
        if let Some(ix) = self
            .items
            .iter()
            .position(|item| matches!(item, Item::NoteEditor { .. }))
        {
            self.list.scroll_to_reveal_item(ix);
        }
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Takes an open editor down, keeping the text of a new note as the
    /// draft of its anchor. Does not rebuild the items.
    pub(crate) fn park_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        if let EditorTarget::New { anchor, .. } = &editor.target {
            let text = editor.state.read(cx).text();
            self.keep_draft(anchor.clone(), text);
        }
    }

    pub(crate) fn keep_draft(&mut self, anchor: NoteAnchor, text: SharedString) {
        if text.trim().is_empty() {
            return;
        }
        self.drafts.retain(|(at, _)| *at != anchor);
        self.drafts.push_back((anchor, text));
        while self.drafts.len() > MAX_DRAFTS {
            self.drafts.pop_front();
        }
    }

    /// Whether the editor can save, and why not.
    pub(crate) fn saving(&self, cx: &gpui::App) -> Saving {
        let Some(editor) = &self.editor else {
            return Saving::Nothing;
        };
        let text = editor.state.read(cx).text();
        if text.trim().is_empty() || editor.original.as_deref() == Some(text.as_ref()) {
            return Saving::Nothing;
        }
        if text.len() > limits::NOTE_BYTES {
            return Saving::TooLong;
        }
        Saving::Ready
    }

    /// ⌘↩ in the editor and its Save button: sends `AddNote` or `EditNote`
    /// and puts the editor away; the note is in the snapshot when `command`
    /// returns.
    pub(crate) fn save_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving(cx) != Saving::Ready {
            return;
        }
        let Some(editor) = self.editor.take() else {
            return;
        };
        let text: Arc<str> = Arc::from(editor.state.read(cx).text().as_ref());
        let target_name = editor.target.name();
        match editor.target {
            EditorTarget::New { rows, anchor } => {
                self.drafts.retain(|(at, _)| *at != anchor);
                let rows = self.current_rows_key(&rows);
                self.command(ReviewCommand::AddNote {
                    rows: Some(rows),
                    anchor,
                    text,
                    excerpt: None,
                });
                self.notes_added = self.notes_added.saturating_add(1);
            }
            EditorTarget::Existing(note) => {
                self.command(ReviewCommand::EditNote { note, text });
            }
        }
        debug!("note editor"; id = self.id.0, target = target_name, how = "saved");
        self.finish_editor(window, cx);
    }

    /// Esc in the editor: a new note's text becomes the anchor's draft, a
    /// changed note's changes are dropped.
    pub(crate) fn cancel_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let target = editor.target.clone();
        let drafted = matches!(&target, EditorTarget::New { .. })
            && !editor.state.read(cx).text().trim().is_empty();
        self.park_editor(cx);
        debug!(
            "note editor";
            id = self.id.0,
            target = target.name(),
            how = if drafted { "drafted" } else { "cancelled" }
        );
        self.finish_editor(window, cx);
    }

    /// After the editor went away: the items without it, the focus back on
    /// the screen.
    fn finish_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adopt_latest(cx);
        if self
            .items
            .iter()
            .any(|item| matches!(item, Item::NoteEditor { .. }))
        {
            self.rebuild_in_place(cx);
        }
        if let Some(focus) = &self.screen_focus {
            window.focus(focus, cx);
        }
        cx.notify();
    }

    /// The key of `rows` under the context the screen holds for its file now
    /// (the editor may have been opened before a fold was expanded).
    fn current_rows_key(&self, rows: &RowsKey) -> RowsKey {
        let Some(file) = items::rows_file(&self.snapshot, rows) else {
            return rows.clone();
        };
        match rows {
            RowsKey::Step { step, .. } => items::step_rows_key(*step, &file, &self.context),
            RowsKey::Final { .. } => items::final_rows_key(&file, &self.context),
        }
    }

    // ----- one note -------------------------------------------------------

    pub(crate) fn note(&self, id: NoteId) -> Option<&Note> {
        self.snapshot
            .notes
            .get(*self.note_pos.get(&id)? as usize)
            .filter(|note| note.id == id)
    }

    /// The note selected, if one is.
    fn selected_note(&self) -> Option<NoteId> {
        match self.selection {
            Some(ItemKey::Note(id)) => Some(id),
            _ => None,
        }
    }

    /// The Edit button, and the action on the selected note: only a note
    /// that is not sent can be changed.
    pub(crate) fn edit_note(&mut self, id: NoteId, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(
            self.note(id).map(|note| note.state),
            Some(NoteState::Pending)
        ) {
            return;
        }
        self.open_editor(EditorTarget::Existing(id), window, cx);
    }

    pub(crate) fn edit_selected_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selected_note() {
            self.edit_note(id, window, cx);
        }
    }

    /// The Delete button: no question, one note is cheap to write again. A
    /// note in a batch that is being sent cannot be deleted.
    pub(crate) fn delete_note(&mut self, id: NoteId, cx: &mut Context<Self>) {
        if matches!(
            self.note(id).map(|note| note.state),
            None | Some(NoteState::Sending(_))
        ) {
            return;
        }
        self.command(ReviewCommand::RemoveNote(id));
        self.adopt_latest(cx);
    }

    pub(crate) fn delete_selected_note(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected_note() {
            self.delete_note(id, cx);
        }
    }

    // ----- sending ----------------------------------------------------------

    /// ⌘↩ and the Send button: the pending notes go out the first route.
    pub(crate) fn send_notes(&mut self, cx: &mut Context<Self>) {
        let state = self.snapshot.notes_state.clone();
        if state.pending == 0 || state.sending.is_some() {
            return;
        }
        let Some(route) = state.routes.first() else {
            return;
        };
        self.command(ReviewCommand::SendNotes {
            route: route.id.clone(),
        });
        self.adopt_latest(cx);
    }

    /// The bar's Copy as Prompt after a failed send: the same notes go out
    /// the clipboard route.
    pub(crate) fn copy_notes_as_prompt(&mut self, cx: &mut Context<Self>) {
        let state = self.snapshot.notes_state.clone();
        if state.pending == 0 || state.sending.is_some() {
            return;
        }
        let Some(route) = state
            .routes
            .iter()
            .find(|route| route.kind == RouteKind::Clipboard)
        else {
            return;
        };
        self.command(ReviewCommand::SendNotes {
            route: route.id.clone(),
        });
        self.adopt_latest(cx);
    }

    /// The batch Copy Again points at: the latest one, if nothing answered
    /// it, if some of its notes are still there and if nothing is sending.
    pub(crate) fn copy_again_batch(&self) -> Option<(BatchId, u32)> {
        let state = &self.snapshot.notes_state;
        if state.sending.is_some() {
            return None;
        }
        let batch = state.batches.last()?;
        if batch.answered_by.is_some() {
            return None;
        }
        let count = self
            .snapshot
            .notes
            .iter()
            .filter(|note| note.state == NoteState::Sent(batch.id))
            .count();
        (count > 0).then_some((batch.id, u32::try_from(count).unwrap_or(u32::MAX)))
    }

    pub(crate) fn copy_notes_again(&mut self, cx: &mut Context<Self>) {
        if let Some((batch, _)) = self.copy_again_batch() {
            self.command(ReviewCommand::CopyBatch(batch));
            self.adopt_latest(cx);
        }
    }

    /// The confirmation said Discard.
    pub(crate) fn discard_confirmed(&mut self, cx: &mut Context<Self>) {
        let pending = self.snapshot.notes_state.pending;
        debug!("notes discard"; id = self.id.0, confirmed = true, notes = pending);
        self.command(ReviewCommand::DiscardNotes);
        self.adopt_latest(cx);
    }

    /// The confirmation said Cancel (or went away).
    pub(crate) fn discard_declined(&self) {
        debug!(
            "notes discard";
            id = self.id.0,
            confirmed = false,
            notes = self.snapshot.notes_state.pending
        );
    }

    /// Whether Discard can be asked for: there are pending notes and none
    /// is on its way.
    pub(crate) fn can_discard(&self) -> bool {
        let state = &self.snapshot.notes_state;
        state.pending > 0 && state.sending.is_none()
    }

    // ----- what the bar says -----------------------------------------------

    pub(crate) fn status(&self) -> Status {
        let state = &self.snapshot.notes_state;
        if self.notes_full_hint {
            return Status::Full;
        }
        if state.sending.is_some() {
            return Status::Sending;
        }
        match state.last {
            Some(SendOutcome::Sent { via, count, .. }) => Status::Sent { via, count },
            Some(SendOutcome::Failed { route, reason }) => Status::NotSent {
                route,
                reason: match reason {
                    lectern_services::review::SendFailure::NotNow(block) => match block {
                        lectern_services::review::RouteBlock::Starting => Reason::Starting,
                        lectern_services::review::RouteBlock::Busy => Reason::Busy,
                        lectern_services::review::RouteBlock::NoSession => Reason::NoSession,
                    },
                    lectern_services::review::SendFailure::TimedOut => Reason::TimedOut,
                    lectern_services::review::SendFailure::Failed => match route {
                        RouteKind::Clipboard => Reason::Clipboard,
                        RouteKind::Session => Reason::Failed,
                    },
                },
            },
            Some(SendOutcome::CopiedAgain { count, failed, .. }) => match failed {
                None => Status::Copied { count },
                Some(reason) => Status::NotCopied {
                    reason: match reason {
                        lectern_services::review::SendFailure::TimedOut => Reason::TimedOut,
                        _ => Reason::Clipboard,
                    },
                },
            },
            None => Status::Count(state.pending),
        }
    }

    /// How many notes the batch `answers` names has (the turn title).
    pub(crate) fn answered_count(&self, batch: BatchId) -> Option<u32> {
        self.snapshot
            .notes_state
            .batches
            .iter()
            .find(|sent| sent.id == batch)
            .map(|sent| sent.count)
    }

    // ----- a new snapshot ---------------------------------------------------

    /// Called with each ready snapshot: puts a stale editor away and goes to
    /// the Conversation page once for each batch sent into the session.
    pub(crate) fn notes_snapshot_applied(&mut self) {
        if let Some(EditorTarget::Existing(id)) = self.editor.as_ref().map(|e| &e.target)
            && !matches!(
                self.note(*id).map(|note| note.state),
                Some(NoteState::Pending)
            )
        {
            self.editor = None;
            debug!("note editor"; id = self.id.0, target = "existing", how = "closed_by_change");
        }
        if let Some(SendOutcome::Sent {
            batch,
            via: SentVia::TerminalPaste | SentVia::Acp,
            ..
        }) = self.snapshot.notes_state.last
            && self.switched_for != Some(batch)
        {
            self.switched_for = Some(batch);
            self.shared
                .dispatch(WorkspaceCommand::SelectInspectorTab(Name::Borrowed(
                    CONVERSATION_TAB,
                )));
        }
    }

    /// The file of a diff when there are notes (or an editor) to place
    /// in it; None when there are none, which is the common case and takes
    /// the plain path.
    pub(crate) fn note_file(&self, key: &RowsKey) -> Option<FileKey> {
        if self.note_index.is_empty() {
            return None;
        }
        items::rows_file(&self.snapshot, key)
    }

    /// The positions of the notes, to find one by its ID.
    pub(crate) fn index_notes(&mut self) {
        self.note_pos = self
            .snapshot
            .notes
            .iter()
            .enumerate()
            .map(|(ix, note)| (note.id, u32::try_from(ix).unwrap_or(u32::MAX)))
            .collect();
    }

    /// Gives the screen's focus handle to the model, for the editor to
    /// return the focus to.
    pub(crate) fn set_screen_focus(&mut self, focus: FocusHandle) {
        self.screen_focus = Some(focus);
    }
}
