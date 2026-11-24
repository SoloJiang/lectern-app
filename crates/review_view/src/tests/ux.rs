//! #422, the audit of the review: the actions for what only the mouse did, Esc
//! with the Conversation page open, the drafts that outlive a screen, and the
//! hover fill (docs/design/review_view.md「Action 与快捷键」「审阅意见（#22）」
//! 「离开审阅」, interaction.md「动效」).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use gpui::{Modifiers, SharedString, TestAppContext};
use lectern_services::project::{ProjectKey, WorktreeKey};
use lectern_services::review::{
    BriefItem, BriefRule, DiffSide, NarrativeItem, NoteAnchor, Review, ReviewTarget, SessionRef,
    TurnRef,
};
use lectern_services::session::{Agent, SessionKey, SourceId, ToolCallId, TurnId};
use lectern_ui::motion::{HOVER_OUT, active_transitions};

use crate::actions::{JumpToBriefEntry, JumpToSubagent, ToggleTurn};
use crate::items::{Item, ItemKey};
use crate::notes::MAX_DRAFTS;
use crate::testing::{FIRST_ID, file_key, snapshot_fixture};
use crate::tests::notes::{editor_text, row, sample, select};
use crate::tests::screen::{must, session};
use crate::tests::step2::{open_sample, sample_snapshot, step_key};
use crate::tests::support::{Opened, Rig, ready};

fn turn(ordinal: u32) -> TurnRef {
    TurnRef {
        session: 0,
        ordinal,
    }
}

/// The screen goes away: the view leaves the window and the model is
/// released, which hands its drafts to `Shared`.
fn close(cx: &mut TestAppContext, opened: Opened) {
    let window = opened.window;
    drop(opened);
    window
        .update(cx, |host, _, cx| {
            host.clear_child();
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();
}

/// Keeps the review of `session()` alive when its screen goes away, as the
/// review service does for a while (a screen of it opened again gets the
/// same review and its rows).
fn hold(rig: &Rig) {
    let _ = rig.review.open(session());
}

fn outside() -> ReviewTarget {
    ReviewTarget::Outside {
        project: ProjectKey::Folder {
            root: Arc::from(std::path::Path::new("/elsewhere")),
        },
        worktree: WorktreeKey(Arc::from(std::path::Path::new("/elsewhere"))),
    }
}

// ----- the actions of what only the mouse did -------------------------------

/// `ToggleTurn` folds the turn of the selected item the way a click on the
/// turn's title does, and opens it again; the selection of a step that goes
/// with the fold moves to the title, so that the second key finds the turn.
#[gpui::test]
fn toggle_turn_does_what_the_click_on_the_title_does(cx: &mut TestAppContext) {
    // By the click.
    let (_rig, clicked, _) = ready(cx, 3, 2, 8);
    let records = clicked.record(cx);
    let title = must(&records, "review.turn.item[0:1]");
    let mut visual = clicked.visual(cx);
    visual.simulate_click(title.bounds.center(), Modifiers::default());
    visual.run_until_parked();
    let by_click = clicked.read(cx, |model| model.collapsed_turns.clone());
    assert_eq!(by_click.len(), 1);

    // By the action, from a step of the first turn.
    let (_rig, opened, _) = ready(cx, 3, 2, 8);
    let step = ItemKey::Step(lectern_services::review::StepId(0));
    select(&opened, cx, step);
    let mut visual = opened.visual(cx);
    visual.dispatch_action(ToggleTurn);
    visual.run_until_parked();
    assert_eq!(
        opened.read(cx, |model| model.collapsed_turns.clone()),
        by_click,
        "the same turn is folded"
    );
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(ItemKey::Turn(turn(1))),
        "the selection stays in view, on the title"
    );
    let records = opened.record(cx);
    assert_eq!(
        must(&records, "review.turn.item[0:1]")
            .semantic
            .state
            .expanded,
        Some(false)
    );

    // The second use opens it again, the selection stays on the title.
    visual.dispatch_action(ToggleTurn);
    visual.run_until_parked();
    assert!(opened.read(cx, |model| model.collapsed_turns.is_empty()));
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(ItemKey::Turn(turn(1)))
    );
}

/// Items that belong to no turn (the brief above the first, the folded noise
/// below the last) do not fold one, and the key goes on.
#[gpui::test]
fn toggle_turn_without_a_turn_does_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(3, 2, 8);
    // File 2 is noise: it is listed in the block at the end, after the turns.
    let mut files = snapshot.files.to_vec();
    files[2].noise = Some(lectern_services::review::NoiseKind::Generated);
    snapshot.files = Arc::from(files);
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    assert!(
        opened.read(cx, |model| model
            .items
            .iter()
            .any(|item| matches!(item, Item::Noise { .. }))),
        "the fixture has a noise block"
    );
    let mut visual = opened.visual(cx);
    for key in [ItemKey::Brief, ItemKey::Noise] {
        select(&opened, cx, key.clone());
        assert!(
            !opened.update(cx, |model, cx| model.toggle_current_turn(cx)),
            "{key:?} is in no turn"
        );
        visual.dispatch_action(ToggleTurn);
        visual.run_until_parked();
        assert!(
            opened.read(cx, |model| model.collapsed_turns.is_empty()),
            "{key:?}: no turn was folded"
        );
    }
}

fn with_brief(cx: &mut TestAppContext) -> (Rig, Opened) {
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(4, 1, 12);
    snapshot.turns = None;
    let mut brief = (*snapshot.brief).clone();
    brief.items = Arc::from(vec![
        BriefItem {
            rule: BriefRule::Drifted,
            critical: true,
            file: Some(file_key(2)),
            line: Some(lectern_services::review::LineAnchor {
                side: DiffSide::New,
                line: 5,
            }),
            count: 1,
        },
        BriefItem {
            rule: BriefRule::CommandOnly,
            critical: false,
            file: None,
            line: None,
            count: 2,
        },
        BriefItem {
            rule: BriefRule::EditFailed,
            critical: false,
            file: Some(file_key(3)),
            line: None,
            count: 1,
        },
    ]);
    snapshot.brief = Arc::new(brief);
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    (rig, opened)
}

/// `JumpToBriefEntry` goes where a click on the entry goes, entry after entry
/// (an entry without a file is passed), and round to the first.
#[gpui::test]
fn jump_to_brief_entry_goes_where_the_click_goes(cx: &mut TestAppContext) {
    let (_rig, clicked) = with_brief(cx);
    let records = clicked.record(cx);
    let first = must(&records, "review.brief.item[drifted:src/file_002.rs]");
    let mut visual = clicked.visual(cx);
    visual.simulate_click(first.bounds.center(), Modifiers::default());
    visual.run_until_parked();
    let by_click = clicked.read(cx, |model| model.selection.clone());
    assert!(
        matches!(by_click, Some(ItemKey::Row { .. })),
        "{by_click:?}"
    );

    let (_rig, opened) = with_brief(cx);
    let mut visual = opened.visual(cx);
    visual.dispatch_action(JumpToBriefEntry);
    visual.run_until_parked();
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        by_click,
        "the first entry"
    );
    visual.dispatch_action(JumpToBriefEntry);
    visual.run_until_parked();
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(ItemKey::File(file_key(3))),
        "the entry without a file is passed"
    );
    visual.dispatch_action(JumpToBriefEntry);
    visual.run_until_parked();
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        by_click,
        "and round to the first"
    );
}

fn with_subagents(cx: &mut TestAppContext) -> (Rig, Opened) {
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(2, 1, 4);
    let mut sessions = snapshot.sessions.to_vec();
    for name in ["sub-a", "sub-b"] {
        sessions.push(SessionRef {
            key: SessionKey {
                source: SourceId(Arc::from("fake")),
                id: Arc::from(name),
            },
            agent: Agent::Codex,
            spawned_by: None,
        });
    }
    snapshot.sessions = Arc::from(sessions);
    let mut turns = snapshot.turns.as_deref().unwrap().to_vec();
    let mut items = turns[0].items.to_vec();
    for (session, call) in [(1, "spawn-a"), (2, "spawn-b")] {
        items.push(NarrativeItem::Subagent {
            session,
            call: ToolCallId(Arc::from(call)),
        });
    }
    turns[0].items = Arc::from(items);
    for session in [1u16, 2] {
        let mut sub = turns[0].clone();
        sub.at = TurnRef {
            session,
            ordinal: 1,
        };
        sub.id = TurnId(Arc::from(format!("sub-turn-{session}")));
        sub.items = Arc::from(Vec::new());
        sub.prompt = None;
        turns.push(sub);
    }
    snapshot.turns = Some(Arc::from(turns));
    rig.review.set_all_rows(FIRST_ID, rows);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    (rig, opened)
}

/// `JumpToSubagent` goes where a click on a sub-agent line goes; used again it
/// goes to the next line, not back to the first (the turn it went to is
/// after both lines).
#[gpui::test]
fn jump_to_subagent_goes_where_the_click_goes(cx: &mut TestAppContext) {
    let (_rig, clicked) = with_subagents(cx);
    let records = clicked.record(cx);
    let line = must(&records, "review.call.item[spawn-a]");
    let mut visual = clicked.visual(cx);
    visual.simulate_click(line.bounds.center(), Modifiers::default());
    visual.run_until_parked();
    let sub_turn = |session| {
        Some(ItemKey::Turn(TurnRef {
            session,
            ordinal: 1,
        }))
    };
    assert_eq!(
        clicked.read(cx, |model| model.selection.clone()),
        sub_turn(1)
    );

    let (_rig, opened) = with_subagents(cx);
    let mut visual = opened.visual(cx);
    visual.dispatch_action(JumpToSubagent);
    visual.run_until_parked();
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        sub_turn(1)
    );
    visual.dispatch_action(JumpToSubagent);
    visual.run_until_parked();
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        sub_turn(2)
    );
    visual.dispatch_action(JumpToSubagent);
    visual.run_until_parked();
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        sub_turn(1),
        "round to the first"
    );

    // A review without sub-agent lines: the action is not handled.
    let (_rig, plain, _) = ready(cx, 2, 1, 4);
    assert!(!plain.update(cx, |model, cx| model.open_next_subagent(cx)));
}

// ----- Esc with the Conversation page open ----------------------------------

/// While the inspector shows the Conversation page the screen leaves Esc to
/// `workspace` (which switches to Files); the selection stays. On the Files
/// page, or with the inspector hidden, Esc is the screen's again.
#[gpui::test]
fn escape_with_the_conversation_page_open_goes_on(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 3, 1, 6);
    cx.update(|cx| {
        cx.bind_keys([gpui::KeyBinding::new(
            "escape",
            crate::tests::support::ProbeEscape,
            Some("Content"),
        )]);
    });
    let probe = opened
        .window
        .read_with(cx, |host, _| host.probe.clone())
        .unwrap();
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("j");
    let selected = opened.read(cx, |model| model.selection.clone());
    assert!(selected.is_some());

    rig.contributions.show_inspector_tab(Some("conversation"));
    visual.simulate_keystrokes("escape");
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        selected,
        "the screen did not clear its selection"
    );
    assert_eq!(probe.load(Ordering::SeqCst), 1, "it went on to workspace");

    // The page is Files now, as `workspace` made it: Esc is the screen's.
    rig.contributions.show_inspector_tab(Some("files"));
    visual.simulate_keystrokes("escape");
    assert_eq!(opened.read(cx, |model| model.selection.clone()), None);
    assert_eq!(probe.load(Ordering::SeqCst), 1, "handled here");
}

// ----- drafts that outlive the screen ---------------------------------------

/// The text typed in an open editor for a new note is still there after the
/// screen is gone and the same review is opened again; the same row opens
/// with it. The drafts are taken back (not copied), and another review does
/// not get them.
#[gpui::test]
fn a_draft_survives_leaving_the_review(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    select(&opened, cx, row(&step_key(), None, Some(1)));
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    visual.simulate_input("not finished");
    assert_eq!(editor_text(&opened, cx).as_deref(), Some("not finished"));
    let shared = opened.read(cx, |model| model.shared.clone());
    assert_eq!(shared.draft_count(), 0, "nothing is kept while it is open");
    hold(&rig);

    // Leaving with the editor open: no question, the text is kept.
    close(cx, opened);
    assert_eq!(shared.draft_count(), 1);

    // Another review does not get it.
    let other = rig.open(cx, outside(), Some(sample_snapshot()));
    other.draw(cx);
    assert_eq!(other.read(cx, |model| model.drafts.len()), 0);
    assert_eq!(shared.draft_count(), 1, "it is still there");
    close(cx, other);
    assert_eq!(shared.draft_count(), 1);

    // The same one does, on the same row.
    let again = open_sample(cx, &rig);
    assert_eq!(again.read(cx, |model| model.drafts.len()), 1);
    assert_eq!(shared.draft_count(), 0, "taken back");
    let mut visual = again.visual(cx);
    select(&again, cx, row(&step_key(), None, Some(1)));
    visual.simulate_keystrokes("c");
    again.draw(cx);
    assert_eq!(editor_text(&again, cx).as_deref(), Some("not finished"));
    // The other row starts empty.
    visual.simulate_keystrokes("escape");
    select(&again, cx, row(&step_key(), Some(2), Some(2)));
    visual.simulate_keystrokes("c");
    again.draw(cx);
    assert_eq!(editor_text(&again, cx).as_deref(), Some(""));
}

/// A draft that Esc closed before is kept too; the changes to an existing
/// note are dropped, and an empty editor leaves no draft.
#[gpui::test]
fn only_new_text_is_left_behind(cx: &mut TestAppContext) {
    let (rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    let shared = opened.read(cx, |model| model.shared.clone());
    hold(&rig);
    // Esc closed a draft; then an editor with no text is open.
    select(&opened, cx, row(&step_key(), None, Some(1)));
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    visual.simulate_input("closed by Esc");
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    select(&opened, cx, row(&step_key(), Some(2), Some(2)));
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    assert_eq!(editor_text(&opened, cx).as_deref(), Some(""));
    close(cx, opened);
    assert_eq!(shared.draft_count(), 1, "the empty editor adds none");

    // The text typed over an existing note is not a draft.
    let again = open_sample(cx, &rig);
    let mut next = (*again.read(cx, |model| model.snapshot.clone())).clone();
    crate::tests::notes::set_notes(
        &mut next,
        vec![crate::tests::notes::note(
            1,
            DiffSide::New,
            1,
            lectern_services::review::NoteState::Pending,
            crate::tests::notes::on_step(),
        )],
    );
    next.generation += 1;
    rig.review.set_snapshot(FIRST_ID, next);
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    again.draw(cx);
    let records = again.record(cx);
    let edit = must(&records, "review.note.item[0000000000000001].edit");
    let mut visual = again.visual(cx);
    visual.simulate_click(edit.bounds.center(), Modifiers::default());
    again.draw(cx);
    visual.simulate_input(" and more");
    close(cx, again);
    assert_eq!(shared.draft_count(), 1, "only the first draft came back");
}

/// At most 20 drafts in all, over the targets; the earliest go first, and a
/// draft for a place that has one already replaces it.
#[gpui::test]
fn at_most_twenty_drafts_are_kept(cx: &mut TestAppContext) {
    let (_rig, opened) = sample(cx);
    let shared = opened.read(cx, |model| model.shared.clone());
    let anchor = |line| NoteAnchor {
        file: file_key(0),
        side: DiffSide::New,
        line,
        last: line,
        oid: crate::tests::step2::new_oid(),
    };
    let drafts = |lines: std::ops::Range<u32>| -> VecDeque<(NoteAnchor, SharedString)> {
        lines
            .map(|line| (anchor(line), SharedString::from(format!("draft {line}"))))
            .collect()
    };
    // The screen of the sample review is open: its target is not the ones
    // that leave their drafts here.
    let (a, b) = (outside(), session());
    shared.keep_drafts(&a, drafts(0..15));
    shared.keep_drafts(&b, drafts(0..10));
    assert_eq!(shared.draft_count(), MAX_DRAFTS, "25 went in, 20 stay");
    // The earliest five, all of `a`'s, are gone.
    let taken = shared.take_drafts(&a);
    assert_eq!(taken.len(), 10);
    assert_eq!(taken.front().map(|(at, _)| at.line), Some(5));
    assert_eq!(shared.draft_count(), 10, "`b`'s are left");
    // The same place again replaces; it does not add.
    shared.keep_drafts(&b, drafts(0..1));
    assert_eq!(shared.draft_count(), 10);
    let taken = shared.take_drafts(&b);
    assert_eq!(
        taken.back().map(|(at, _)| at.line),
        Some(0),
        "the newest last"
    );
}

// ----- hover ----------------------------------------------------------------

/// A turn's title fills when the pointer is on it: its hover transition
/// starts at once (`active_transitions`).
#[gpui::test]
fn a_turn_title_fills_under_the_pointer(cx: &mut TestAppContext) {
    let (_rig, opened, _) = ready(cx, 3, 2, 8);
    let records = opened.record(cx);
    let title = must(&records, "review.turn.item[0:1]");
    let at = title.bounds.center();
    let mut visual = opened.visual(cx);
    let base = cx.update(|cx| active_transitions(cx));
    visual.simulate_mouse_move(at, None, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        cx.update(|cx| active_transitions(cx)),
        base + 1,
        "the hover transition of the title runs"
    );
    // Leaving turns the same transition round; no other is made.
    visual.simulate_mouse_move(
        gpui::point(gpui::px(1.0), gpui::px(1.0)),
        None,
        Modifiers::default(),
    );
    visual.run_until_parked();
    assert_eq!(cx.update(|cx| active_transitions(cx)), base + 1);
}

/// A diff line fades in under the pointer and the one it came from fades
/// out: two transitions in the model however many lines there are, none for
/// the lines that do not have the pointer.
#[gpui::test]
fn a_diff_line_fills_under_the_pointer(cx: &mut TestAppContext) {
    let (_rig, opened) = sample(cx);
    let mut visual = opened.visual(cx);
    let records = opened.record(cx);
    let added = must(&records, "review.diff.item[step:0].row.item[-:1]");
    let context = must(&records, "review.diff.item[step:0].row.item[2:2]");
    assert!(!opened.read(cx, |model| model.row_fade.running()));
    let base = cx.update(|cx| active_transitions(cx));

    let on = |record: &lectern_ui::semantic::Record| {
        gpui::point(
            record.bounds.origin.x + gpui::px(40.0),
            record.bounds.center().y,
        )
    };
    visual.simulate_mouse_move(on(added), None, Modifiers::default());
    visual.run_until_parked();
    assert!(opened.read(cx, |model| model.row_fade.running()));
    assert_eq!(cx.update(|cx| active_transitions(cx)), base + 1);

    // The pointer crosses to the next line: that one fades in, this one out.
    visual.simulate_mouse_move(on(context), None, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(cx.update(|cx| active_transitions(cx)), base + 2);

    // The pointer leaves, and the list scrolls to where the line is not
    // drawn; the screen still moves the fade on, which ends and lets go of
    // its transition (a fade that only the line's own drawing moved would
    // stay counted).
    visual.simulate_mouse_move(
        gpui::point(gpui::px(1.0), gpui::px(1.0)),
        None,
        Modifiers::default(),
    );
    visual.run_until_parked();
    opened.update(cx, |model, _| {
        let last = model.items.len() - 1;
        model.list.scroll_to(gpui::ListOffset {
            item_ix: last,
            offset_in_item: gpui::Pixels::ZERO,
        });
    });
    std::thread::sleep(HOVER_OUT.duration * 2);
    opened.draw(cx);
    assert!(!opened.read(cx, |model| model.row_fade.running()));
    assert_eq!(
        cx.update(|cx| active_transitions(cx)),
        base,
        "the fade ended without its line drawn"
    );
}
