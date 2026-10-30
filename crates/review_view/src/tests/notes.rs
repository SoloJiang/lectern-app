//! Review notes (docs/design/review_view.md「测试」「审阅意见（#22）」): the
//! items they add to the list, the editor, the commands, the floating bar
//! and the Conversation page after a send. Headless: `FakeReview` applies
//! the note commands to its snapshot before `command` returns, as the real
//! service does.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::TestAppContext;
use lectern_services::project::BlobId;
use lectern_services::review::{
    BatchId, DiffSide, Excerpt, FileKey, FileRows, Note, NoteAnchor, NoteId, NotePlace, NoteRows,
    NoteState, NotesState, ReviewCommand, ReviewSnapshot, RouteKind, RowsKey, SendFailure,
    SendOutcome, SentBatch, SentVia, StepId, TurnRef, limits,
};
use lectern_services::session::Timestamp;
use lectern_services::workspace::WorkspaceCommand;
use lectern_ui::semantic::Record;

use crate::actions::{CancelNote, DiscardNotes, SendNotes};
use crate::items::{self, BuildOptions, DiffMode, EditorAt, Item, ItemKey};
use crate::notes::EditorTarget;
use crate::testing::{FIRST_ID, file_key, snapshot_fixture};
use crate::tests::screen::{attr, find, must, session};
use crate::tests::step2::{final_key, new_oid, old_oid, open_sample, sample_rows, step_key};
use crate::tests::support::{Opened, Rig};

// ----- helpers ----------------------------------------------------------

pub(crate) fn note(id: u64, side: DiffSide, line: u32, state: NoteState, place: NotePlace) -> Note {
    Note {
        id: NoteId(id),
        anchor: NoteAnchor {
            file: file_key(0),
            side,
            line,
            oid: match side {
                DiffSide::Old => old_oid(),
                DiffSide::New => new_oid(),
            },
        },
        text: Arc::from(format!("text {id}")),
        created_at: Timestamp(1_700_000_000_000 + id as i64 * 1000),
        edited_at: None,
        state,
        place,
        excerpt: None,
    }
}

pub(crate) fn on_step() -> NotePlace {
    NotePlace::Rows(Arc::from(vec![NoteRows::Step(StepId(0))]))
}

pub(crate) fn on_final() -> NotePlace {
    NotePlace::Rows(Arc::from(vec![NoteRows::Final]))
}

/// The notes of a snapshot, with the counts the review would give.
pub(crate) fn set_notes(snapshot: &mut ReviewSnapshot, notes: Vec<Note>) {
    let mut state: NotesState = (*snapshot.notes_state).clone();
    let pending = notes
        .iter()
        .filter(|n| n.state == NoteState::Pending)
        .count() as u32;
    let sending = notes
        .iter()
        .filter(|n| matches!(n.state, NoteState::Sending(_)))
        .count() as u32;
    state.pending = pending;
    state.unsent = pending + sending;
    snapshot.notes = Arc::from(notes);
    snapshot.notes_state = Arc::new(state);
}

/// A snapshot of the sample review (one file, one step, one turn) with these
/// notes.
pub(crate) fn noted_snapshot(notes: Vec<Note>) -> ReviewSnapshot {
    let (mut snapshot, _) = snapshot_fixture(1, 1, 4);
    set_notes(&mut snapshot, notes);
    snapshot
}

fn rows_map() -> HashMap<RowsKey, Arc<FileRows>> {
    [step_key(), final_key()]
        .into_iter()
        .map(|key| (key.clone(), Arc::new(sample_rows(key))))
        .collect()
}

fn options<'a>(
    context: &'a HashMap<FileKey, u32>,
    known: &'a HashMap<RowsKey, u32>,
    editor: Option<EditorAt>,
) -> BuildOptions<'a> {
    BuildOptions {
        mode: DiffMode::Unified,
        earlier_collapsed: true,
        noise_expanded: false,
        context,
        known_lines: known,
        editor,
    }
}

/// The items of the sample review as short words: rows by their line
/// numbers, notes by their IDs.
pub(crate) fn words(items: &[Item]) -> Vec<String> {
    let line = |n: &Option<u32>| n.map_or("-".to_owned(), |n| n.to_string());
    items
        .iter()
        .map(|item| match item {
            Item::Step { .. } => "step".to_owned(),
            Item::File { .. } => "file".to_owned(),
            Item::Hunk { .. } => "hunk".to_owned(),
            Item::Row { old, new, .. } => format!("row {}:{}", line(old), line(new)),
            Item::Fold { old, .. } => format!("fold {old}"),
            Item::Placeholder { .. } => "placeholder".to_owned(),
            Item::Note { id, rows } => format!(
                "note {}{}",
                id.0,
                if rows.is_some() { "" } else { " (elsewhere)" }
            ),
            Item::NoteEditor { .. } => "editor".to_owned(),
            Item::NotesElsewhere { count } => format!("elsewhere {count}"),
            Item::Header => "header".to_owned(),
            Item::Brief => "brief".to_owned(),
            Item::Turn { .. } => "turn".to_owned(),
            Item::Prompt { .. } => "prompt".to_owned(),
            Item::Text { .. } => "text".to_owned(),
            Item::Tool { .. } => "tool".to_owned(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// The words from the first `step` or `file` on.
fn from_title(items: &[Item]) -> Vec<String> {
    let all = words(items);
    let at = all
        .iter()
        .position(|w| w == "step" || w == "file")
        .expect("a card title");
    all[at..].to_vec()
}

fn build(snapshot: &ReviewSnapshot, rows: &HashMap<RowsKey, Arc<FileRows>>) -> Vec<Item> {
    let (context, known) = (HashMap::new(), HashMap::new());
    items::build(snapshot, rows, &options(&context, &known, None))
}

// ----- the items ---------------------------------------------------------

#[test]
fn items_notes_follow_their_rows() {
    // Two notes on the added line, one on the removed line; the sample
    // rows are: context 0, removed 1, added 1, context 2, context 3.
    let snapshot = noted_snapshot(vec![
        note(1, DiffSide::New, 1, NoteState::Pending, on_step()),
        note(2, DiffSide::New, 1, NoteState::Sent(BatchId(7)), on_step()),
        note(3, DiffSide::Old, 1, NoteState::Pending, on_step()),
    ]);
    let items = build(&snapshot, &rows_map());
    assert_eq!(
        &from_title(&items)[..10],
        [
            "step", "hunk", "row 0:0", "row 1:-", "note 3", "row -:1", "note 1", "note 2",
            "row 2:2", "row 3:3"
        ]
    );
    let marks: Vec<(u16, bool)> = items
        .iter()
        .filter_map(|item| match item {
            Item::Row { notes, .. } => Some((notes.count, notes.unsent)),
            _ => None,
        })
        .collect();
    assert_eq!(
        marks,
        [(0, false), (1, true), (2, true), (0, false), (0, false)]
    );
    // The note items sit inside the diff's card: they carry its rows key.
    assert!(items.iter().all(|item| match item {
        Item::Note { rows, .. } => rows.as_ref() == Some(&step_key()),
        _ => true,
    }));
}

#[test]
fn items_notes_in_the_grouped_view_use_the_final_rows() {
    let mut snapshot = noted_snapshot(vec![
        note(1, DiffSide::New, 1, NoteState::Pending, on_final()),
        // Its place lists a step only: the grouped view shows no step.
        note(2, DiffSide::New, 2, NoteState::Pending, on_step()),
    ]);
    snapshot.turns = None;
    let items = build(&snapshot, &rows_map());
    assert_eq!(
        &from_title(&items)[..9],
        [
            "file", "note 2", "hunk", "row 0:0", "row 1:-", "row -:1", "note 1", "row 2:2",
            "row 3:3"
        ],
        "the note that is in no final row sits at the head of the diff"
    );
}

#[test]
fn items_notes_in_a_fold_follow_the_fold() {
    let mut rows = sample_rows(step_key());
    let mut list = rows.rows.to_vec();
    list.push(lectern_services::review::DiffRow::Folded {
        old: 4..44,
        new: 4..44,
        old_byte: None,
        new_byte: None,
    });
    rows.rows = Arc::from(list);
    let map: HashMap<RowsKey, Arc<FileRows>> = [(step_key(), Arc::new(rows))].into();
    let snapshot = noted_snapshot(vec![
        note(1, DiffSide::New, 20, NoteState::Pending, on_step()),
        // Past the fold and not among the rows at all: the head.
        note(2, DiffSide::New, 900, NoteState::Pending, on_step()),
    ]);
    let items = build(&snapshot, &map);
    let all = from_title(&items);
    assert_eq!(all[1], "note 2");
    let fold = all.iter().position(|w| w == "fold 4").expect("the fold");
    assert_eq!(
        all[fold + 1],
        "note 1",
        "after the fold that hides its line"
    );
}

#[test]
fn items_notes_drifted_follow_the_file_title() {
    let drifted = NotePlace::Drifted { file: file_key(0) };
    let snapshot = noted_snapshot(vec![
        note(1, DiffSide::New, 1, NoteState::Pending, drifted.clone()),
        note(2, DiffSide::New, 3, NoteState::Pending, drifted),
    ]);
    let items = build(&snapshot, &rows_map());
    assert_eq!(
        &from_title(&items)[..5],
        ["step", "note 1", "note 2", "hunk", "row 0:0"]
    );
    // Rows that are not prepared: the title, the notes, the placeholder.
    let items = build(&snapshot, &HashMap::new());
    assert_eq!(
        &from_title(&items)[..4],
        ["step", "note 1", "note 2", "placeholder"]
    );
}

#[test]
fn items_notes_elsewhere_make_one_block() {
    let snapshot = noted_snapshot(vec![
        note(
            1,
            DiffSide::New,
            1,
            NoteState::Pending,
            NotePlace::Elsewhere,
        ),
        note(
            2,
            DiffSide::New,
            2,
            NoteState::Sent(BatchId(3)),
            NotePlace::Elsewhere,
        ),
        note(3, DiffSide::New, 1, NoteState::Pending, on_step()),
    ]);
    let items = build(&snapshot, &rows_map());
    let all = words(&items);
    let tail: Vec<&str> = all.iter().rev().take(3).map(String::as_str).collect();
    assert_eq!(
        tail,
        ["note 2 (elsewhere)", "note 1 (elsewhere)", "elsewhere 2"],
        "the block is at the end of the narrative"
    );
    assert!(
        all.contains(&"note 3".to_owned()),
        "the note on a row stays"
    );
}

#[test]
fn items_notes_editor_position() {
    let snapshot = noted_snapshot(vec![note(
        1,
        DiffSide::New,
        1,
        NoteState::Pending,
        on_step(),
    )]);
    let (context, known) = (HashMap::new(), HashMap::new());
    let at_row = |oid| EditorAt::Row {
        rows: step_key(),
        side: DiffSide::New,
        line: 1,
        oid,
    };
    let built = |editor, rows: &HashMap<RowsKey, Arc<FileRows>>| {
        from_title(&items::build(
            &snapshot,
            rows,
            &options(&context, &known, Some(editor)),
        ))
    };
    // After the row and the notes of the row.
    let all = built(at_row(new_oid()), &rows_map());
    assert_eq!(
        &all[3..8],
        ["row 1:-", "row -:1", "note 1", "editor", "row 2:2"]
    );
    // The row's content changed: the editor stays at the head of the diff.
    let all = built(at_row(BlobId::Sha1([9; 20])), &rows_map());
    assert_eq!(&all[..3], ["step", "editor", "hunk"]);
    // The rows are not prepared: after the placeholder and its notes.
    let all = built(at_row(new_oid()), &HashMap::new());
    assert_eq!(&all[..4], ["step", "placeholder", "note 1", "editor"]);
    // Changing a note: right after it.
    let all = built(EditorAt::AfterNote(NoteId(1)), &rows_map());
    assert_eq!(&all[4..7], ["row -:1", "note 1", "editor"]);
}

// ----- the screen --------------------------------------------------------

pub(crate) fn row(rows: &RowsKey, old: Option<u32>, new: Option<u32>) -> ItemKey {
    ItemKey::Row {
        rows: rows.clone(),
        old,
        new,
    }
}

pub(crate) fn select(opened: &Opened, cx: &mut TestAppContext, key: ItemKey) {
    opened.update(cx, |model, cx| model.select(Some(key), cx));
}

/// The sample review, open and drawn; C works on its rows (they have OIDs).
pub(crate) fn sample(cx: &mut TestAppContext) -> (Rig, Opened) {
    let rig = Rig::new(cx);
    let opened = open_sample(cx, &rig);
    (rig, opened)
}

/// The review with a new snapshot: the notes (and whatever else) changed by
/// `change`, announced as the review does.
pub(crate) fn change(
    rig: &Rig,
    opened: &Opened,
    cx: &mut TestAppContext,
    change: impl FnOnce(&mut ReviewSnapshot),
) {
    let mut next = (*opened.read(cx, |model| model.snapshot.clone())).clone();
    change(&mut next);
    next.generation += 1;
    rig.review.set_snapshot(FIRST_ID, next);
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);
}

pub(crate) fn editor_target(opened: &Opened, cx: &mut TestAppContext) -> Option<EditorTarget> {
    opened.read(cx, |model| model.editor.as_ref().map(|e| e.target.clone()))
}

pub(crate) fn editor_text(opened: &Opened, cx: &mut TestAppContext) -> Option<String> {
    opened.model.read_with(cx, |model, app| {
        model
            .editor
            .as_ref()
            .map(|editor| editor.state.read(app).text().to_string())
    })
}

pub(crate) fn note_commands(rig: &Rig) -> Vec<ReviewCommand> {
    rig.review
        .commands()
        .into_iter()
        .map(|(_, command)| command)
        .filter(|command| {
            matches!(
                command,
                ReviewCommand::AddNote { .. }
                    | ReviewCommand::EditNote { .. }
                    | ReviewCommand::RemoveNote(_)
                    | ReviewCommand::DiscardNotes
                    | ReviewCommand::SendNotes { .. }
                    | ReviewCommand::CopyBatch(_)
            )
        })
        .collect()
}

pub(crate) fn texts(records: &[Record], id: &str) -> String {
    must(records, id)
        .semantic
        .label
        .as_ref()
        .map(|label| label.text.to_string())
        .unwrap_or_default()
}

#[gpui::test]
fn c_opens_the_editor_on_the_selected_row(cx: &mut TestAppContext) {
    let (_rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    let steps = step_key();
    let target = |side, line, oid| {
        Some(EditorTarget::New {
            rows: steps.clone(),
            anchor: NoteAnchor {
                file: file_key(0),
                side,
                line,
                oid,
            },
        })
    };
    let close = |opened: &Opened, cx: &mut TestAppContext| {
        let mut visual = opened.visual(cx);
        visual.dispatch_action(CancelNote);
        visual.run_until_parked();
    };

    // An added line: its new side.
    select(&opened, cx, row(&steps, None, Some(1)));
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    assert_eq!(
        editor_target(&opened, cx),
        target(DiffSide::New, 1, new_oid())
    );
    let records = opened.record(cx);
    let editor = must(&records, "review.note-editor");
    assert_eq!(attr(editor, "target").as_deref(), Some("new"));
    assert_eq!(attr(editor, "side").as_deref(), Some("new"));
    assert_eq!(attr(editor, "line").as_deref(), Some("1"));
    assert_eq!(texts(&records, "review.note-editor.save"), "Add Note ⌘↩");
    close(&opened, cx);
    assert_eq!(editor_target(&opened, cx), None);

    // A removed line: its old side.
    select(&opened, cx, row(&steps, Some(1), None));
    visual.simulate_keystrokes("c");
    assert_eq!(
        editor_target(&opened, cx),
        target(DiffSide::Old, 1, old_oid())
    );
    close(&opened, cx);

    // A context line: its new side.
    select(&opened, cx, row(&steps, Some(2), Some(2)));
    visual.simulate_keystrokes("c");
    assert_eq!(
        editor_target(&opened, cx),
        target(DiffSide::New, 2, new_oid())
    );
    close(&opened, cx);

    // A hunk: its first added line.
    select(
        &opened,
        cx,
        ItemKey::Hunk {
            rows: steps.clone(),
            old: 0,
            new: 0,
        },
    );
    visual.simulate_keystrokes("c");
    assert_eq!(
        editor_target(&opened, cx),
        target(DiffSide::New, 1, new_oid())
    );
    close(&opened, cx);

    // Side by side, a removed and an added line are one row: the new side.
    visual.dispatch_action(crate::actions::ToggleDiffMode);
    visual.run_until_parked();
    opened.draw(cx);
    select(&opened, cx, row(&steps, Some(1), Some(1)));
    visual.simulate_keystrokes("c");
    assert_eq!(
        editor_target(&opened, cx),
        target(DiffSide::New, 1, new_oid())
    );
}

#[gpui::test]
fn c_opens_nothing_without_a_row_or_an_oid(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    // The old side has no OID (it has no text).
    let mut rows = sample_rows(step_key());
    rows.old.oid = None;
    rig.review.set_auto_prepare(true);
    rig.review.set_rows(FIRST_ID, step_key(), rows);
    rig.review
        .set_rows(FIRST_ID, final_key(), sample_rows(final_key()));
    let opened = rig.open(cx, session(), Some(noted_snapshot(Vec::new())));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    let mut visual = opened.visual(cx);

    // Nothing selected.
    visual.simulate_keystrokes("c");
    assert_eq!(editor_target(&opened, cx), None);
    // A card's title is not a row.
    select(&opened, cx, ItemKey::Step(StepId(0)));
    visual.simulate_keystrokes("c");
    assert_eq!(editor_target(&opened, cx), None);
    // The removed line: its side has no OID.
    select(&opened, cx, row(&step_key(), Some(1), None));
    visual.simulate_keystrokes("c");
    assert_eq!(editor_target(&opened, cx), None);
    // The other lines are fine.
    select(&opened, cx, row(&step_key(), None, Some(1)));
    visual.simulate_keystrokes("c");
    assert!(editor_target(&opened, cx).is_some());
}

#[gpui::test]
fn cmd_return_in_the_editor_saves_the_note(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    select(&opened, cx, row(&step_key(), None, Some(1)));
    visual.simulate_keystrokes("c");
    opened.draw(cx);

    // Nothing but white space: Save is off and ⌘↩ sends nothing.
    visual.simulate_input("  \n ");
    opened.draw(cx);
    let records = opened.record(cx);
    assert!(
        must(&records, "review.note-editor.save")
            .semantic
            .state
            .disabled
    );
    visual.simulate_keystrokes("cmd-enter");
    assert!(note_commands(&rig).is_empty());
    assert!(editor_target(&opened, cx).is_some());

    // Too long: the hint says so, Save is off.
    let long = "x".repeat(limits::NOTE_BYTES + 1);
    let state = opened.read(cx, |model| model.editor.as_ref().unwrap().state.clone());
    visual.update(|window, cx| state.update(cx, |state, cx| state.set_text(&long, window, cx)));
    opened.draw(cx);
    let records = opened.record(cx);
    assert_eq!(
        attr(must(&records, "review.note-editor"), "too_long").as_deref(),
        Some("true")
    );
    assert!(find(&records, "review.note-editor.too-long").is_some());
    assert!(
        must(&records, "review.note-editor.save")
            .semantic
            .state
            .disabled
    );
    visual.simulate_keystrokes("cmd-enter");
    assert!(note_commands(&rig).is_empty());

    // Some text: ⌘↩ adds the note as it was written.
    visual.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.set_text("check  this\nline", window, cx)
        })
    });
    opened.draw(cx);
    let records = opened.record(cx);
    assert!(find(&records, "review.note-editor.too-long").is_none());
    assert!(
        !must(&records, "review.note-editor.save")
            .semantic
            .state
            .disabled
    );
    visual.simulate_keystrokes("cmd-enter");
    visual.run_until_parked();
    let commands = note_commands(&rig);
    assert_eq!(
        commands,
        [ReviewCommand::AddNote {
            rows: Some(step_key()),
            anchor: NoteAnchor {
                file: file_key(0),
                side: DiffSide::New,
                line: 1,
                oid: new_oid(),
            },
            text: Arc::from("check  this\nline"),
            excerpt: None,
        }]
    );
    // The note is in the snapshot, the editor is gone and the focus is
    // back on the screen; the selection did not move.
    assert_eq!(editor_target(&opened, cx), None);
    assert_eq!(opened.read(cx, |model| model.snapshot.notes.len()), 1);
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(row(&step_key(), None, Some(1)))
    );
    let on_screen = opened.read(cx, |model| model.screen_focus.clone().unwrap());
    assert!(visual.update(|window, _| on_screen.is_focused(window)));
    let items = opened.read(cx, |model| words(&model.items));
    let at = items.iter().position(|w| w == "row -:1").unwrap();
    assert_eq!(items[at + 1], "note 1");
    assert!(!items.contains(&"editor".to_owned()));
    let records = opened.record(cx);
    assert_eq!(texts(&records, "review.bar.comments"), "1 review note");
}

#[gpui::test]
fn cmd_return_outside_the_editor_sends(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    // No pending notes: nothing to send.
    visual.simulate_keystrokes("cmd-enter");
    assert!(note_commands(&rig).is_empty());

    let pending = note(1, DiffSide::New, 1, NoteState::Pending, on_step());
    change(&rig, &opened, cx, |snapshot| {
        set_notes(snapshot, vec![pending.clone()]);
    });
    visual.simulate_keystrokes("cmd-enter");
    visual.run_until_parked();
    assert_eq!(
        note_commands(&rig),
        [ReviewCommand::SendNotes {
            route: Arc::from("terminal")
        }]
    );
    // It is being sent: another ⌘↩ sends nothing.
    assert!(opened.read(cx, |model| model.snapshot.notes_state.sending.is_some()));
    visual.simulate_keystrokes("cmd-enter");
    assert_eq!(note_commands(&rig).len(), 1);
}

#[gpui::test]
fn escape_in_the_editor_keeps_a_draft(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    select(&opened, cx, row(&step_key(), None, Some(1)));
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    visual.simulate_input("half a thought");
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    opened.draw(cx);
    assert_eq!(editor_target(&opened, cx), None, "Esc closed the editor");
    assert_eq!(opened.read(cx, |model| model.drafts.len()), 1);
    assert!(note_commands(&rig).is_empty());
    let on_screen = opened.read(cx, |model| model.screen_focus.clone().unwrap());
    assert!(visual.update(|window, _| on_screen.is_focused(window)));

    // C on the same row brings the text back; another row starts empty.
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    assert_eq!(editor_text(&opened, cx).as_deref(), Some("half a thought"));
    assert_eq!(opened.read(cx, |model| model.drafts.len()), 0);
    visual.dispatch_action(CancelNote);
    visual.run_until_parked();
    select(&opened, cx, row(&step_key(), Some(2), Some(2)));
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    assert_eq!(editor_text(&opened, cx).as_deref(), Some(""));

    // Opening another editor over one with text keeps that text (C from the
    // screen: in the editor it is a letter).
    visual.simulate_input("second");
    select(&opened, cx, row(&step_key(), None, Some(1)));
    let on_screen = opened.read(cx, |model| model.screen_focus.clone().unwrap());
    visual.update(|window, cx| window.focus(&on_screen, cx));
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    assert_eq!(editor_text(&opened, cx).as_deref(), Some("half a thought"));
    assert_eq!(opened.read(cx, |model| model.drafts.len()), 1);
}

#[gpui::test]
fn escape_in_the_editor_drops_the_changes_to_a_note(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    change(&rig, &opened, cx, |snapshot| {
        set_notes(
            snapshot,
            vec![note(1, DiffSide::New, 1, NoteState::Pending, on_step())],
        );
    });
    let records = opened.record(cx);
    let edit = must(&records, "review.note.item[0000000000000001].edit");
    visual.simulate_click(edit.bounds.center(), gpui::Modifiers::default());
    opened.draw(cx);
    assert_eq!(
        editor_target(&opened, cx),
        Some(EditorTarget::Existing(NoteId(1)))
    );
    assert_eq!(editor_text(&opened, cx).as_deref(), Some("text 1"));
    visual.simulate_input(" and more");
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert_eq!(editor_target(&opened, cx), None);
    assert_eq!(opened.read(cx, |model| model.drafts.len()), 0, "no draft");
    assert!(note_commands(&rig).is_empty(), "the note is as it was");
    assert_eq!(
        opened.read(cx, |model| model.snapshot.notes[0].text.to_string()),
        "text 1"
    );
}

#[gpui::test]
fn escape_in_the_editor_keeps_at_most_twenty_drafts(cx: &mut TestAppContext) {
    let (_rig, opened) = sample(cx);
    let anchor = |line| NoteAnchor {
        file: file_key(0),
        side: DiffSide::New,
        line,
        oid: new_oid(),
    };
    opened.update(cx, |model, _| {
        for line in 0..21 {
            model.keep_draft(anchor(line), format!("draft {line}").into());
        }
        // The same anchor again replaces its draft, white space is no draft.
        model.keep_draft(anchor(5), "again".into());
        model.keep_draft(anchor(99), "  \n".into());
    });
    let (len, first, last) = opened.read(cx, |model| {
        (
            model.drafts.len(),
            model.drafts.front().map(|(at, _)| at.line),
            model
                .drafts
                .back()
                .map(|(at, text)| (at.line, text.to_string())),
        )
    });
    assert_eq!(len, 20);
    assert_eq!(first, Some(1), "the earliest was dropped");
    assert_eq!(last, Some((5, "again".to_owned())));
}

// ----- how notes are drawn -------------------------------------------------

/// The key and the text of the first place of the bar.
pub(crate) fn status(records: &[Record]) -> (Option<&'static str>, String) {
    let label = must(records, "review.bar.comments")
        .semantic
        .label
        .as_ref()
        .expect("a label");
    (label.key, label.text.to_string())
}

pub(crate) fn index_of(records: &[Record], id: &str) -> usize {
    records
        .iter()
        .position(|record| record.semantic.id.as_str() == id)
        .unwrap_or_else(|| panic!("no node {id}"))
}

pub(crate) fn batch(id: u64, via: SentVia, count: u32) -> SentBatch {
    SentBatch {
        id: BatchId(id),
        at: Timestamp(1_700_000_100_000),
        via,
        count,
        answered_by: None,
    }
}

#[gpui::test]
fn notes_are_drawn_under_their_rows(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut drifted = note(
        3,
        DiffSide::New,
        1,
        NoteState::Pending,
        NotePlace::Drifted { file: file_key(0) },
    );
    drifted.excerpt = Some(Excerpt {
        first: 0,
        lines: Arc::from(vec![Arc::<str>::from("a"), Arc::from("b"), Arc::from("c")]),
    });
    change(&rig, &opened, cx, |snapshot| {
        set_notes(
            snapshot,
            vec![
                note(1, DiffSide::New, 1, NoteState::Pending, on_step()),
                note(2, DiffSide::New, 1, NoteState::Sent(BatchId(5)), on_step()),
                drifted,
                note(
                    4,
                    DiffSide::New,
                    7,
                    NoteState::Pending,
                    NotePlace::Elsewhere,
                ),
                note(
                    5,
                    DiffSide::Old,
                    1,
                    NoteState::Sending(BatchId(6)),
                    on_step(),
                ),
            ],
        );
        let mut state = (*snapshot.notes_state).clone();
        state.batches = Arc::from(vec![batch(5, SentVia::Clipboard, 1)]);
        state.sending = Some(BatchId(6));
        snapshot.notes_state = Arc::new(state);
    });
    // The block of the notes elsewhere is at the end of the narrative.
    opened.update(cx, |model, _| {
        let last = model.items.len() - 1;
        model.list.scroll_to(gpui::ListOffset {
            item_ix: last,
            offset_in_item: gpui::px(0.0),
        });
    });
    opened.draw(cx);
    let near_end = opened.record(cx);
    assert_eq!(
        attr(must(&near_end, "review.notes-elsewhere"), "count").as_deref(),
        Some("1")
    );
    let elsewhere = must(&near_end, "review.note.item[0000000000000004]");
    assert_eq!(attr(elsewhere, "place").as_deref(), Some("elsewhere"));
    opened.update(cx, |model, _| {
        model.list.scroll_to(gpui::ListOffset::default())
    });
    opened.draw(cx);
    let records = opened.record(cx);

    let pending = must(&records, "review.note.item[0000000000000001]");
    assert_eq!(attr(pending, "state").as_deref(), Some("pending"));
    assert_eq!(attr(pending, "place").as_deref(), Some("rows"));
    assert_eq!(attr(pending, "side").as_deref(), Some("new"));
    assert_eq!(attr(pending, "line").as_deref(), Some("1"));
    assert_eq!(
        texts(&records, "review.note.item[0000000000000001]"),
        "text 1"
    );
    // The row's dot says how many notes it has.
    let added = must(&records, "review.diff.item[step:0].row.item[-:1]");
    assert_eq!(attr(added, "notes").as_deref(), Some("2"));
    let removed = must(&records, "review.diff.item[step:0].row.item[1:-]");
    assert_eq!(attr(removed, "notes").as_deref(), Some("1"));
    assert!(
        attr(
            must(&records, "review.diff.item[step:0].row.item[0:0]"),
            "notes"
        )
        .is_none()
    );
    // Under the row, in the order they were written.
    assert!(
        index_of(&records, "review.diff.item[step:0].row.item[-:1]")
            < index_of(&records, "review.note.item[0000000000000001]")
    );
    assert!(
        index_of(&records, "review.note.item[0000000000000001]")
            < index_of(&records, "review.note.item[0000000000000002]")
    );
    // Not sent: both buttons; sent: delete; being sent: neither.
    let has = |id: &str| find(&records, id).is_some();
    assert!(has("review.note.item[0000000000000001].edit"));
    assert!(has("review.note.item[0000000000000001].delete"));
    assert!(!has("review.note.item[0000000000000002].edit"));
    assert!(has("review.note.item[0000000000000002].delete"));
    assert_eq!(
        attr(
            must(&records, "review.note.item[0000000000000002]"),
            "state"
        )
        .as_deref(),
        Some("sent")
    );
    assert!(!has("review.note.item[0000000000000005].edit"));
    assert!(!has("review.note.item[0000000000000005].delete"));
    // The drifted one follows the title, before the diff.
    let title = index_of(&records, "review.step.item[0]");
    let drift = index_of(&records, "review.note.item[0000000000000003]");
    assert!(title < drift);
    assert!(drift < index_of(&records, "review.diff.item[step:0].row.item[0:0]"));
    assert_eq!(
        attr(
            must(&records, "review.note.item[0000000000000003]"),
            "place"
        )
        .as_deref(),
        Some("drifted")
    );
}

#[gpui::test]
fn the_bar_follows_the_notes_state(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let send_label = |records: &[Record]| texts(records, "review.bar.send");
    let none = opened.record(cx);
    assert_eq!(
        status(&none),
        (Some("review-comments"), "No review notes".to_owned())
    );
    let send = must(&none, "review.bar.send");
    assert!(send.semantic.state.disabled, "nothing to send");
    assert_eq!(attr(send, "route").as_deref(), Some("session"));
    assert!(find(&none, "review.bar.discard").is_none());
    assert!(find(&none, "review.bar.copy-again").is_none());
    assert!(find(&none, "review.bar.copy").is_none());

    // Two pending notes.
    let two = vec![
        note(1, DiffSide::New, 1, NoteState::Pending, on_step()),
        note(2, DiffSide::Old, 1, NoteState::Pending, on_step()),
    ];
    change(&rig, &opened, cx, |snapshot| {
        set_notes(snapshot, two.clone())
    });
    let records = opened.record(cx);
    assert_eq!(status(&records).1, "2 review notes");
    assert!(!must(&records, "review.bar.send").semantic.state.disabled);
    assert!(send_label(&records).starts_with("Send to"));
    assert!(find(&records, "review.bar.discard").is_some());
    let comments = must(&records, "review.bar.comments");
    assert_eq!(attr(comments, "pending").as_deref(), Some("2"));
    assert_eq!(attr(comments, "sending").as_deref(), Some("false"));

    // Being sent.
    let sending: Vec<Note> = two
        .iter()
        .cloned()
        .map(|mut n| {
            n.state = NoteState::Sending(BatchId(1));
            n
        })
        .collect();
    change(&rig, &opened, cx, |snapshot| {
        set_notes(snapshot, sending.clone());
        let mut state = (*snapshot.notes_state).clone();
        state.sending = Some(BatchId(1));
        snapshot.notes_state = Arc::new(state);
    });
    let records = opened.record(cx);
    assert_eq!(
        status(&records),
        (Some("review-notes-sending"), "Sending…".to_owned())
    );
    assert!(must(&records, "review.bar.send").semantic.state.disabled);
    assert!(find(&records, "review.bar.discard").is_none());
    assert_eq!(
        attr(must(&records, "review.bar.comments"), "sending").as_deref(),
        Some("true")
    );

    // What the last send came to.
    let outcome = |last: SendOutcome, pending: bool| {
        let notes = if pending {
            two.clone()
        } else {
            two.iter()
                .cloned()
                .map(|mut n| {
                    n.state = NoteState::Sent(BatchId(1));
                    n
                })
                .collect()
        };
        move |snapshot: &mut ReviewSnapshot| {
            set_notes(snapshot, notes.clone());
            let mut state = (*snapshot.notes_state).clone();
            state.sending = None;
            state.last = Some(last);
            state.batches = Arc::from(vec![batch(1, SentVia::Clipboard, 2)]);
            snapshot.notes_state = Arc::new(state);
        }
    };
    let sent = |via| SendOutcome::Sent {
        batch: BatchId(1),
        via,
        count: 2,
    };
    for (via, key, text) in [
        (
            SentVia::Clipboard,
            "review-notes-copied",
            "Copied 2 review notes. Paste them into the session.",
        ),
        (
            SentVia::TerminalPaste,
            "review-notes-pasted",
            "Pasted 2 review notes into the terminal. Press Return there to send.",
        ),
        (SentVia::Acp, "review-notes-sent", "Sent 2 review notes"),
    ] {
        change(&rig, &opened, cx, outcome(sent(via), false));
        let records = opened.record(cx);
        assert_eq!(status(&records), (Some(key), text.to_owned()));
    }

    // A send that failed: the notes are pending again; into the session,
    // the clipboard is offered as the other way.
    for (reason, route, why) in [
        (
            SendFailure::NotNow(lectern_services::review::RouteBlock::Starting),
            RouteKind::Session,
            "the terminal is still starting",
        ),
        (
            SendFailure::NotNow(lectern_services::review::RouteBlock::Busy),
            RouteKind::Session,
            "the agent is working",
        ),
        (
            SendFailure::NotNow(lectern_services::review::RouteBlock::NoSession),
            RouteKind::Session,
            "no session can receive them",
        ),
        (
            SendFailure::TimedOut,
            RouteKind::Session,
            "no answer in 10 seconds",
        ),
        (
            SendFailure::Failed,
            RouteKind::Session,
            "the session didn't take them",
        ),
        (
            SendFailure::Failed,
            RouteKind::Clipboard,
            "the clipboard couldn't be written",
        ),
    ] {
        change(
            &rig,
            &opened,
            cx,
            outcome(SendOutcome::Failed { route, reason }, true),
        );
        let records = opened.record(cx);
        let (key, text) = status(&records);
        assert_eq!(key, Some("review-notes-not-sent"));
        assert!(
            text.starts_with("Not sent: ") && text.contains(why),
            "{text}"
        );
        assert_eq!(
            find(&records, "review.bar.copy").is_some(),
            route == RouteKind::Session,
            "Copy as Prompt follows a failed send into the session only"
        );
    }

    // Copied again.
    let again = |failed| SendOutcome::CopiedAgain {
        batch: BatchId(1),
        count: 2,
        failed,
    };
    change(&rig, &opened, cx, outcome(again(None), true));
    let records = opened.record(cx);
    assert_eq!(
        status(&records).0,
        Some("review-notes-copied"),
        "even with pending notes"
    );
    change(
        &rig,
        &opened,
        cx,
        outcome(again(Some(SendFailure::Failed)), false),
    );
    let records = opened.record(cx);
    let (key, text) = status(&records);
    assert_eq!(key, Some("review-notes-not-copied"));
    assert!(text.contains("the clipboard couldn't be written"), "{text}");
    change(
        &rig,
        &opened,
        cx,
        outcome(again(Some(SendFailure::TimedOut)), false),
    );
    let records = opened.record(cx);
    assert!(status(&records).1.contains("no answer in 10 seconds"));

    // Only the clipboard: Send is Copy as Prompt.
    change(&rig, &opened, cx, |snapshot| {
        set_notes(snapshot, two.clone());
        let mut state = (*snapshot.notes_state).clone();
        state.routes = Arc::from(vec![state.routes[1].clone()]);
        state.last = None;
        snapshot.notes_state = Arc::new(state);
    });
    let records = opened.record(cx);
    let send = must(&records, "review.bar.send");
    assert_eq!(attr(send, "route").as_deref(), Some("clipboard"));
    assert_eq!(send_label(&records), "Copy as Prompt ⌘↩");
    assert!(!send.semantic.state.disabled);

    // Copy Again, Discard, Looks Good, Copy as Prompt and Send, in order.
    change(&rig, &opened, cx, |snapshot| {
        let mut notes = two.clone();
        notes[0].state = NoteState::Sent(BatchId(1));
        set_notes(snapshot, notes);
        let mut state = (*snapshot.notes_state).clone();
        state.last = Some(SendOutcome::Failed {
            route: RouteKind::Session,
            reason: SendFailure::Failed,
        });
        state.batches = Arc::from(vec![batch(1, SentVia::TerminalPaste, 1)]);
        state.routes = Arc::from(vec![
            lectern_services::review::RouteOffer {
                id: Arc::from("terminal"),
                kind: RouteKind::Session,
            },
            lectern_services::review::RouteOffer {
                id: Arc::from("clipboard"),
                kind: RouteKind::Clipboard,
            },
        ]);
        snapshot.notes_state = Arc::new(state);
    });
    let records = opened.record(cx);
    let order: Vec<usize> = [
        "review.bar.comments",
        "review.bar.copy-again",
        "review.bar.discard",
        "review.bar.looks-good",
        "review.bar.copy",
        "review.bar.send",
    ]
    .iter()
    .map(|id| index_of(&records, id))
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    assert_eq!(
        attr(must(&records, "review.bar.copy-again"), "batch").as_deref(),
        Some("0000000000000001")
    );
    // Copy as Prompt sends the pending notes by the clipboard.
    let mut visual = opened.visual(cx);
    visual.simulate_click(
        must(&records, "review.bar.copy").bounds.center(),
        gpui::Modifiers::default(),
    );
    visual.run_until_parked();
    assert_eq!(
        note_commands(&rig),
        [ReviewCommand::SendNotes {
            route: Arc::from("clipboard")
        }]
    );
}

#[gpui::test]
fn discard_asks_first(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let two = vec![
        note(1, DiffSide::New, 1, NoteState::Pending, on_step()),
        note(2, DiffSide::Old, 1, NoteState::Pending, on_step()),
    ];
    change(&rig, &opened, cx, |snapshot| {
        set_notes(snapshot, two.clone())
    });
    let mut visual = opened.visual(cx);

    // The button asks; Cancel sends nothing.
    let records = opened.record(cx);
    visual.simulate_click(
        must(&records, "review.bar.discard").bounds.center(),
        gpui::Modifiers::default(),
    );
    visual.run_until_parked();
    assert!(cx.has_pending_prompt());
    let (message, detail) = cx.pending_prompt().expect("a question");
    assert_eq!(message, "Discard 2 review notes?");
    assert_eq!(detail, "They haven't been sent. This can't be undone.");
    cx.simulate_prompt_answer("Cancel");
    visual.run_until_parked();
    assert!(note_commands(&rig).is_empty());
    assert_eq!(opened.read(cx, |model| model.snapshot.notes.len()), 2);

    // Discard sends the command.
    visual.dispatch_action(DiscardNotes);
    visual.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Discard");
    visual.run_until_parked();
    assert_eq!(note_commands(&rig), [ReviewCommand::DiscardNotes]);
    assert_eq!(opened.read(cx, |model| model.snapshot.notes.len()), 0);
    // Nothing is left to ask about.
    visual.dispatch_action(DiscardNotes);
    visual.run_until_parked();
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
fn a_full_batch_blocks_c(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    change(&rig, &opened, cx, |snapshot| {
        crate::testing::add_notes(snapshot, limits::PENDING_NOTES);
    });
    select(&opened, cx, row(&step_key(), None, Some(1)));
    visual.simulate_keystrokes("c");
    assert_eq!(editor_target(&opened, cx), None);
    let records = opened.record(cx);
    assert_eq!(
        status(&records),
        (
            Some("review-notes-full"),
            "Up to 40 notes can be unsent at a time. Send or discard some first.".to_owned()
        )
    );

    // 30 of them being sent and 10 pending is as full.
    change(&rig, &opened, cx, |snapshot| {
        let mut notes = snapshot.notes.to_vec();
        for n in notes.iter_mut().take(30) {
            n.state = NoteState::Sending(BatchId(1));
        }
        set_notes(snapshot, notes);
        let mut state = (*snapshot.notes_state).clone();
        state.sending = Some(BatchId(1));
        snapshot.notes_state = Arc::new(state);
    });
    visual.simulate_keystrokes("c");
    assert_eq!(editor_target(&opened, cx), None);
    assert_eq!(
        opened.read(cx, |model| model.snapshot.notes_state.unsent),
        40
    );

    // The next command clears the hint.
    opened.update(cx, |model, cx| {
        model.discard_confirmed(cx);
    });
    opened.draw(cx);
    let records = opened.record(cx);
    assert_eq!(status(&records).0, Some("review-notes-sending"));
    assert!(!opened.read(cx, |model| model.notes_full_hint));
    // With room again, C opens the editor (the long list of notes took the
    // rows out of view; they are brought back first).
    opened.update(cx, |model, _| {
        model.list.scroll_to(gpui::ListOffset::default())
    });
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    select(&opened, cx, row(&step_key(), None, Some(1)));
    visual.simulate_keystrokes("c");
    assert!(editor_target(&opened, cx).is_some());
}

#[gpui::test]
fn copy_again_follows_the_latest_batch(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    let pending = |id, line| note(id, DiffSide::New, line, NoteState::Pending, on_step());
    change(&rig, &opened, cx, |snapshot| {
        set_notes(snapshot, vec![pending(1, 1), pending(2, 2)]);
    });
    // No batch yet: no button.
    assert!(find(&opened.record(cx), "review.bar.copy-again").is_none());

    // The first batch goes out by the clipboard.
    visual.dispatch_action(SendNotes);
    visual.run_until_parked();
    let first = rig
        .review
        .finish_send(FIRST_ID, SentVia::Clipboard)
        .expect("a batch was in the air");
    rig.settle(cx);
    opened.draw(cx);
    let records = opened.record(cx);
    let button = must(&records, "review.bar.copy-again");
    assert_eq!(
        attr(button, "batch"),
        Some(format!("{:016x}", first.0)),
        "it points at the batch just sent"
    );
    // A second batch: the button points at it.
    opened.update(cx, |model, cx| {
        model.command(ReviewCommand::AddNote {
            rows: Some(step_key()),
            anchor: NoteAnchor {
                file: file_key(0),
                side: DiffSide::New,
                line: 3,
                oid: new_oid(),
            },
            text: Arc::from("third"),
            excerpt: None,
        });
        model.adopt_latest(cx);
    });
    visual.dispatch_action(SendNotes);
    visual.run_until_parked();
    // While it is being sent there is no button.
    opened.draw(cx);
    assert!(find(&opened.record(cx), "review.bar.copy-again").is_none());
    let second = rig
        .review
        .finish_send(FIRST_ID, SentVia::TerminalPaste)
        .unwrap();
    rig.settle(cx);
    opened.draw(cx);
    let records = opened.record(cx);
    assert_eq!(
        attr(must(&records, "review.bar.copy-again"), "batch"),
        Some(format!("{:016x}", second.0))
    );

    // Pressed: CopyBatch of that batch; the result replaces the status.
    visual.simulate_click(
        must(&records, "review.bar.copy-again").bounds.center(),
        gpui::Modifiers::default(),
    );
    visual.run_until_parked();
    assert_eq!(
        note_commands(&rig).last(),
        Some(&ReviewCommand::CopyBatch(second))
    );
    rig.review.finish_copy(FIRST_ID, second, None);
    rig.settle(cx);
    opened.draw(cx);
    let records = opened.record(cx);
    assert_eq!(status(&records).0, Some("review-notes-copied"));
    assert!(find(&records, "review.bar.copy-again").is_some());
    rig.review
        .finish_copy(FIRST_ID, second, Some(SendFailure::Failed));
    rig.settle(cx);
    opened.draw(cx);
    assert_eq!(
        status(&opened.record(cx)).0,
        Some("review-notes-not-copied")
    );

    // The next AddNote brings the count back.
    opened.update(cx, |model, cx| {
        model.command(ReviewCommand::AddNote {
            rows: Some(step_key()),
            anchor: NoteAnchor {
                file: file_key(0),
                side: DiffSide::New,
                line: 4,
                oid: new_oid(),
            },
            text: Arc::from("fourth"),
            excerpt: None,
        });
        model.adopt_latest(cx);
    });
    opened.draw(cx);
    assert_eq!(status(&opened.record(cx)).0, Some("review-comments"));

    // The batch is answered: the button goes. So it does when the notes of
    // the batch are gone.
    rig.review.answer_batch(
        FIRST_ID,
        second,
        TurnRef {
            session: 0,
            ordinal: 1,
        },
    );
    rig.settle(cx);
    opened.draw(cx);
    assert!(find(&opened.record(cx), "review.bar.copy-again").is_none());
    change(&rig, &opened, cx, |snapshot| {
        let mut state = (*snapshot.notes_state).clone();
        let mut batches = state.batches.to_vec();
        batches.last_mut().unwrap().answered_by = None;
        state.batches = Arc::from(batches);
        snapshot.notes_state = Arc::new(state);
    });
    assert!(find(&opened.record(cx), "review.bar.copy-again").is_some());
    change(&rig, &opened, cx, |snapshot| {
        let notes: Vec<Note> = snapshot
            .notes
            .iter()
            .filter(|n| n.state != NoteState::Sent(second))
            .cloned()
            .collect();
        set_notes(snapshot, notes);
    });
    assert!(find(&opened.record(cx), "review.bar.copy-again").is_none());
}

#[gpui::test]
fn a_session_route_switches_to_the_conversation_once(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    let switches = |rig: &Rig| {
        rig.contributions
            .dispatched()
            .iter()
            .filter(|command| {
                matches!(command, WorkspaceCommand::SelectInspectorTab(name) if name == "conversation")
            })
            .count()
    };
    let write = |opened: &Opened, cx: &mut TestAppContext, line: u32| {
        opened.update(cx, |model, cx| {
            model.command(ReviewCommand::AddNote {
                rows: Some(step_key()),
                anchor: NoteAnchor {
                    file: file_key(0),
                    side: DiffSide::New,
                    line,
                    oid: new_oid(),
                },
                text: Arc::from("a note"),
                excerpt: None,
            });
            model.adopt_latest(cx);
        });
    };
    write(&opened, cx, 1);
    visual.dispatch_action(SendNotes);
    visual.run_until_parked();
    assert_eq!(switches(&rig), 0, "not before the send is done");
    rig.review.finish_send(FIRST_ID, SentVia::TerminalPaste);
    rig.settle(cx);
    assert_eq!(switches(&rig), 1);
    // The same batch, another generation: not again.
    rig.review.emit_changed(FIRST_ID);
    rig.settle(cx);
    assert_eq!(switches(&rig), 1);

    // By the clipboard: no switch.
    write(&opened, cx, 2);
    visual.dispatch_action(crate::actions::CopyNotesAsPrompt);
    visual.run_until_parked();
    assert_eq!(
        note_commands(&rig).last(),
        Some(&ReviewCommand::SendNotes {
            route: Arc::from("clipboard")
        })
    );
    rig.review.finish_send(FIRST_ID, SentVia::Clipboard);
    rig.settle(cx);
    assert_eq!(switches(&rig), 1);

    // A new batch into the session switches again.
    write(&opened, cx, 3);
    visual.dispatch_action(SendNotes);
    visual.run_until_parked();
    rig.review.finish_send(FIRST_ID, SentVia::Acp);
    rig.settle(cx);
    assert_eq!(switches(&rig), 2);
}

#[gpui::test]
fn edit_and_delete_act_on_the_note(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    change(&rig, &opened, cx, |snapshot| {
        set_notes(
            snapshot,
            vec![
                note(1, DiffSide::New, 1, NoteState::Pending, on_step()),
                note(2, DiffSide::New, 2, NoteState::Sent(BatchId(4)), on_step()),
                note(
                    3,
                    DiffSide::Old,
                    1,
                    NoteState::Sending(BatchId(5)),
                    on_step(),
                ),
            ],
        );
    });
    let records = opened.record(cx);
    let click = |visual: &mut gpui::VisualTestContext, records: &[Record], id: &str| {
        visual.simulate_click(
            must(records, id).bounds.center(),
            gpui::Modifiers::default(),
        );
        visual.run_until_parked();
    };

    // Edit: the editor has the text, Save is off until it changes.
    click(
        &mut visual,
        &records,
        "review.note.item[0000000000000001].edit",
    );
    opened.draw(cx);
    assert_eq!(
        editor_target(&opened, cx),
        Some(EditorTarget::Existing(NoteId(1)))
    );
    assert_eq!(editor_text(&opened, cx).as_deref(), Some("text 1"));
    let records = opened.record(cx);
    let editor = must(&records, "review.note-editor");
    assert_eq!(attr(editor, "target").as_deref(), Some("existing"));
    let save = must(&records, "review.note-editor.save");
    assert!(save.semantic.state.disabled);
    assert_eq!(texts(&records, "review.note-editor.save"), "Save ⌘↩");
    let state = opened.read(cx, |model| model.editor.as_ref().unwrap().state.clone());
    visual.update(|window, cx| state.update(cx, |state, cx| state.set_text("changed", window, cx)));
    opened.draw(cx);
    let records = opened.record(cx);
    assert!(
        !must(&records, "review.note-editor.save")
            .semantic
            .state
            .disabled
    );
    click(&mut visual, &records, "review.note-editor.save");
    assert_eq!(
        note_commands(&rig),
        [ReviewCommand::EditNote {
            note: NoteId(1),
            text: Arc::from("changed")
        }]
    );
    assert_eq!(editor_target(&opened, cx), None);
    assert_eq!(
        opened.read(cx, |model| model.note(NoteId(1)).unwrap().text.to_string()),
        "changed"
    );

    // Delete: the note goes at once, with no question.
    opened.draw(cx);
    let records = opened.record(cx);
    click(
        &mut visual,
        &records,
        "review.note.item[0000000000000002].delete",
    );
    assert_eq!(
        note_commands(&rig).last(),
        Some(&ReviewCommand::RemoveNote(NoteId(2)))
    );
    assert!(!cx.has_pending_prompt());
    assert!(opened.read(cx, |model| model.note(NoteId(2)).is_none()));
    // One that is being sent cannot be deleted or changed.
    let before = note_commands(&rig).len();
    opened.update(cx, |model, cx| model.delete_note(NoteId(3), cx));
    assert_eq!(note_commands(&rig).len(), before);

    // The selected note: the actions work on it.
    select(&opened, cx, ItemKey::Note(NoteId(1)));
    visual.dispatch_action(crate::actions::DeleteNote);
    visual.run_until_parked();
    assert_eq!(
        note_commands(&rig).last(),
        Some(&ReviewCommand::RemoveNote(NoteId(1)))
    );
}

#[gpui::test]
fn the_editor_follows_its_line_across_a_refresh(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    select(&opened, cx, row(&step_key(), None, Some(1)));
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    let after_row = |opened: &Opened, cx: &mut TestAppContext| {
        opened.read(cx, |model| {
            let words = words(&model.items);
            let at = words
                .iter()
                .position(|w| w == "editor")
                .expect("the editor");
            (words[at - 1].clone(), words[at + 1].clone())
        })
    };
    assert_eq!(after_row(&opened, cx).0, "row -:1");

    // A new generation with a note on the line above: the editor moves
    // with its line.
    change(&rig, &opened, cx, |snapshot| {
        set_notes(
            snapshot,
            vec![note(7, DiffSide::Old, 1, NoteState::Pending, on_step())],
        );
    });
    assert_eq!(
        after_row(&opened, cx),
        ("row -:1".to_owned(), "row 2:2".to_owned())
    );
    assert!(editor_target(&opened, cx).is_some());

    // The file changed under the editor: its row is gone, so the editor
    // sits below the title; saving still sends the note as it was written.
    let mut changed = sample_rows(step_key());
    changed.new.oid = Some(BlobId::Sha1([7; 20]));
    rig.review.set_rows(FIRST_ID, step_key(), changed);
    rig.review.emit_prepared(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);
    assert!(opened.read(cx, |model| model.editor.is_some()));
    let near = opened.read(cx, |model| {
        let words = words(&model.items);
        let at = words
            .iter()
            .position(|w| w == "editor")
            .expect("the editor");
        (words[at - 1].clone(), words[at + 1].clone())
    });
    assert_eq!(near.0, "step");
    assert!(near.1 == "note 7" || near.1 == "hunk", "{near:?}");
    visual.simulate_input("still here");
    visual.simulate_keystrokes("cmd-enter");
    visual.run_until_parked();
    match note_commands(&rig).last() {
        Some(ReviewCommand::AddNote { anchor, text, .. }) => {
            assert_eq!(anchor.oid, new_oid(), "the content it was written on");
            assert_eq!(&**text, "still here");
        }
        other => panic!("not an AddNote: {other:?}"),
    }
}

#[gpui::test]
fn the_turn_title_shows_the_answered_notes(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    change(&rig, &opened, cx, |snapshot| {
        set_notes(
            snapshot,
            vec![note(1, DiffSide::New, 1, NoteState::Pending, on_step())],
        );
    });
    assert!(attr(must(&opened.record(cx), "review.turn.item[0:1]"), "answers").is_none());
    visual.dispatch_action(SendNotes);
    visual.run_until_parked();
    let sent = rig
        .review
        .finish_send(FIRST_ID, SentVia::TerminalPaste)
        .unwrap();
    let turn = TurnRef {
        session: 0,
        ordinal: 1,
    };
    rig.review.answer_batch(FIRST_ID, sent, turn);
    rig.settle(cx);
    opened.draw(cx);
    assert_eq!(
        attr(must(&opened.record(cx), "review.turn.item[0:1]"), "answers").as_deref(),
        Some("1")
    );
    // Folded into one line, the title says it as well.
    opened.update(cx, |model, cx| model.toggle_turn(turn, cx));
    opened.draw(cx);
    let records = opened.record(cx);
    let folded = must(&records, "review.turn.item[0:1]");
    assert_eq!(folded.semantic.state.expanded, Some(false));
    assert_eq!(attr(folded, "answers").as_deref(), Some("1"));
}

#[gpui::test]
fn the_keys_of_the_screen_are_letters_in_the_editor(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    select(&opened, cx, row(&step_key(), None, Some(1)));
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    // J, K, V, C and Space are typed, not taken as shortcuts.
    visual.simulate_input("c j k v c");
    assert_eq!(editor_text(&opened, cx).as_deref(), Some("c j k v c"));
    assert!(
        rig.review
            .commands()
            .iter()
            .all(|(_, command)| !matches!(command, ReviewCommand::MarkSeen(_))),
        "V did not mark the file"
    );
    assert_eq!(
        editor_target(&opened, cx),
        Some(EditorTarget::New {
            rows: step_key(),
            anchor: NoteAnchor {
                file: file_key(0),
                side: DiffSide::New,
                line: 1,
                oid: new_oid(),
            },
        }),
        "the same editor is still open"
    );
}
