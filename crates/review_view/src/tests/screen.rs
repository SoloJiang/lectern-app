//! The screen in GPUI's test platform (docs/design/review_view.md「测试」,
//! `#[gpui::test]`).

use std::sync::Arc;
use std::time::Duration;

use gpui::{ListOffset, Modifiers, TestAppContext, point, px};
use lectern_services::review::{
    AnalysisState, Degraded, FileKey, LoadState, NoiseKind, ReviewCommand, ReviewError,
    ReviewEvent, ReviewTarget, RowsKey, SeenState, StepId,
};
use lectern_services::session::Timestamp;
use lectern_services::workspace::{Layer, Milestone, WindowId, WorkspaceCommand, WorkspaceEvent};
use lectern_ui::motion::{CHECK_STROKE, LOADING_PROGRESS_AT, LOADING_SKELETON_AT};
use lectern_ui::semantic::Record;

use crate::items::{Item, ItemKey};
use crate::testing::{FIRST_ID, file_key, session_key, snapshot_fixture};
use crate::tests::support::{ProbeEscape, Rig, ready, summary};

pub fn find<'a>(records: &'a [Record], id: &str) -> Option<&'a Record> {
    records
        .iter()
        .find(|record| record.semantic.id.as_str() == id)
}

/// The record with this ID; the failure names the IDs there are.
pub fn must<'a>(records: &'a [Record], id: &str) -> &'a Record {
    find(records, id).unwrap_or_else(|| {
        let ids: Vec<&str> = records.iter().map(|r| r.semantic.id.as_str()).collect();
        panic!("no node {id}; the nodes: {ids:#?}")
    })
}

pub fn attr(record: &Record, key: &str) -> Option<String> {
    record
        .semantic
        .attrs
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.to_string())
}

pub fn session() -> ReviewTarget {
    ReviewTarget::Session(session_key())
}

fn label_key(record: &Record) -> Option<&'static str> {
    record.semantic.label.as_ref().and_then(|label| label.key)
}

#[gpui::test]
fn screen_opens_the_review_and_shows_loading_phases(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, session(), None);
    // The factory opened the review and sent nothing.
    assert_eq!(rig.review.opened(), vec![session()]);
    assert!(rig.review.commands().is_empty());

    // Under LOADING_SKELETON_AT nothing is drawn.
    let records = opened.record(cx);
    let loading = must(&records, "review.loading");
    assert_eq!(attr(loading, "phase").as_deref(), Some("nothing"));
    assert!(
        find(&records, "review.bar").is_none(),
        "no bar while loading"
    );

    // Then skeleton blocks.
    opened.update(cx, |model, cx| {
        model.backdate(LOADING_SKELETON_AT + Duration::from_micros(1));
        cx.notify();
    });
    let records = opened.record(cx);
    let loading = must(&records, "review.loading");
    assert_eq!(attr(loading, "phase").as_deref(), Some("skeleton"));

    // Then the message and the cancel button.
    opened.update(cx, |model, cx| {
        model.backdate(LOADING_PROGRESS_AT + Duration::from_micros(1));
        cx.notify();
    });
    let records = opened.record(cx);
    assert_eq!(
        attr(must(&records, "review.loading"), "phase").as_deref(),
        Some("progress")
    );
    let cancel = must(&records, "review.loading.cancel");
    assert_eq!(label_key(cancel), Some("review-cancel"));

    // Ready: the content takes the place.
    let (snapshot, rows) = snapshot_fixture(3, 2, 8);
    for (key, rows) in rows {
        rig.review.set_rows(FIRST_ID, key, rows);
    }
    rig.review.set_auto_prepare(true);
    let mut snapshot = snapshot;
    snapshot.id = FIRST_ID;
    snapshot.target = session();
    rig.review.set_snapshot(FIRST_ID, snapshot);
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);
    let records = opened.record(cx);
    assert!(find(&records, "review.loading").is_none());
    assert!(find(&records, "review.brief").is_some());
    assert!(find(&records, "review.bar").is_some());
}

#[gpui::test]
fn the_toolbar_names_the_session(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.sessions
        .set(vec![summary(session_key(), "Fix the parser")]);
    let (snapshot, _) = snapshot_fixture(2, 1, 4);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    let strings = lectern_i18n::Strings::of(lectern_i18n::Locale::En);
    let (title, subtitle) = opened.read(cx, |model| {
        (
            model.toolbar_title(&strings).as_str().to_owned(),
            model
                .toolbar_subtitle(&strings)
                .map(|t| t.as_str().to_owned()),
        )
    });
    assert_eq!(title, "Fix the parser");
    assert_eq!(subtitle.as_deref(), Some("0/2 reviewed"));
    // The toolbar closure reads the same model.
    let items = cx.update(crate::screen::toolbar);
    assert_eq!(items.title.as_str(), "Fix the parser");
    assert_eq!(items.controls.len(), 2, "the mode switch and Refresh");
}

#[gpui::test]
fn changed_event_reaches_the_main_thread_and_keeps_the_scroll(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 12, 3, 100);
    let top = opened.read(cx, |model| model.items.len());
    assert!(top > 100, "a long list: {top}");
    // Scroll deep into the list.
    opened.update(cx, |model, _| {
        let item_ix = model.items.len() * 2 / 3;
        model.list.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(4.0),
        });
    });
    opened.draw(cx);
    let before = opened.read(cx, |model| {
        let at = model.list.logical_scroll_top();
        (model.items[at.item_ix].key(), at.offset_in_item)
    });

    // The first file grows: one more notice in front of everything.
    let mut next = (*opened.read(cx, |model| model.snapshot.clone())).clone();
    next.generation += 1;
    next.unclaimed = 4;
    rig.review.set_snapshot(FIRST_ID, next);
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);

    let after = opened.read(cx, |model| {
        let at = model.list.logical_scroll_top();
        (model.items[at.item_ix].key(), at.offset_in_item)
    });
    assert_eq!(before.0, after.0, "the same item is at the top");
    assert_eq!(before.1, after.1);
    assert!(opened.read(cx, |model| matches!(model.items[1], Item::Notice(_))));
}

#[gpui::test]
fn stale_generation_is_dropped(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 2, 1, 4);
    let applied = opened.read(cx, |model| model.snapshot.generation);
    let calls = rig.review.snapshot_calls();
    rig.review.emit_event(ReviewEvent::Changed {
        id: FIRST_ID,
        generation: applied,
    });
    rig.review.emit_event(ReviewEvent::Changed {
        id: FIRST_ID,
        generation: applied - 1,
    });
    rig.settle(cx);
    assert_eq!(
        rig.review.snapshot_calls(),
        calls,
        "an event at or below the applied generation reads nothing"
    );
    // A newer one is read.
    rig.review.emit_changed(FIRST_ID);
    rig.settle(cx);
    assert_eq!(
        opened.read(cx, |model| model.snapshot.generation),
        applied + 1
    );
}

#[gpui::test]
fn events_of_a_burst_are_applied_once(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 2, 1, 4);
    let applied = opened.read(cx, |model| model.snapshot.generation);
    let calls = rig.review.snapshot_calls();
    for _ in 0..5 {
        rig.review.emit_changed(FIRST_ID);
    }
    rig.settle(cx);
    assert_eq!(
        opened.read(cx, |model| model.snapshot.generation),
        applied + 5
    );
    assert!(
        rig.review.snapshot_calls() <= calls + 5,
        "at most one read per event, usually one for the burst"
    );
}

#[gpui::test]
fn prepared_replaces_placeholders_without_flicker(cx: &mut TestAppContext) {
    let capture = lectern_logging::testing::capture();
    let rig = Rig::new(cx);
    let (snapshot, rows) = snapshot_fixture(4, 2, 12);
    rig.review.set_all_rows(FIRST_ID, rows);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    // The view asked for the rows of the steps in view and got no answer yet.
    assert_eq!(rig.review.prepares(FIRST_ID), 1);
    assert!(opened.read(cx, |model| {
        model
            .items
            .iter()
            .any(|item| matches!(item, Item::Placeholder { .. }))
    }));
    assert_eq!(rig.review.rows_calls(), 0, "rows() waits for Prepared");
    rig.review.emit_prepared(FIRST_ID);
    rig.settle(cx);
    assert!(rig.review.rows_calls() > 0);
    let rows_shown = opened.read(cx, |model| {
        model
            .items
            .iter()
            .filter(|item| matches!(item, Item::Row { .. }))
            .count()
    });
    let asked = rig
        .review
        .last_needs(FIRST_ID)
        .iter()
        .filter(|need| matches!(need, lectern_services::review::Need::Rows(_)))
        .count();
    assert!(asked >= 1);
    let detail = opened.read(cx, |model| {
        (
            model.items.len(),
            model.rows.len(),
            (
                model.list.viewport_bounds().size.height,
                model.visible.clone(),
            ),
            model
                .items
                .iter()
                .filter(|item| matches!(item, Item::Placeholder { .. }))
                .count(),
        )
    });
    assert_eq!(
        rows_shown,
        asked * 12,
        "every key asked for got its rows: {detail:?}, calls {}, needs {:?}, prepares {}, log {:#?}",
        rig.review.rows_calls(),
        rig.review.last_needs(FIRST_ID),
        rig.review.prepares(FIRST_ID),
        capture.lines()
    );

    // A new generation: the rows in view stay while the review prepares
    // again; rows() is not called until Prepared arrives.
    let calls = rig.review.rows_calls();
    let mut next = (*opened.read(cx, |model| model.snapshot.clone())).clone();
    next.generation += 1;
    next.unclaimed = 1;
    rig.review.set_snapshot(FIRST_ID, next);
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    assert_eq!(rig.review.rows_calls(), calls, "Changed does not read rows");
    let still = opened.read(cx, |model| {
        model
            .items
            .iter()
            .filter(|item| matches!(item, Item::Row { .. }))
            .count()
    });
    assert_eq!(still, rows_shown, "the old rows are still shown");
    rig.review.emit_prepared(FIRST_ID);
    rig.settle(cx);
    assert!(rig.review.rows_calls() > calls);
}

#[gpui::test]
fn prepare_follows_the_visible_range(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (snapshot, rows) = snapshot_fixture(40, 4, 60);
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    rig.settle(cx);
    let first = rig.review.last_needs(FIRST_ID);
    assert!(!first.is_empty());
    let prepares = rig.review.prepares(FIRST_ID);

    // Drawing again changes nothing: the same set is not sent twice.
    opened.draw(cx);
    rig.settle(cx);
    assert_eq!(rig.review.prepares(FIRST_ID), prepares);

    // Scrolled far down, the request names other keys.
    opened.update(cx, |model, _| {
        let target = model.items.len() * 3 / 4;
        model.list.scroll_to(ListOffset {
            item_ix: target,
            offset_in_item: px(0.0),
        });
    });
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    rig.settle(cx);
    let later = rig.review.last_needs(FIRST_ID);
    let state = opened.read(cx, |model| {
        (
            model.visible.clone(),
            model.items.len(),
            model.list.logical_scroll_top().item_ix,
            model.list.viewport_bounds().size.height,
            model.measure_visible(),
            (0..model.items.len().min(400))
                .filter(|ix| model.list.bounds_for_item(*ix).is_some())
                .count(),
        )
    });
    assert!(rig.review.prepares(FIRST_ID) > prepares, "state {state:?}");
    assert_ne!(first, later);
    // The rows far away from the new place went back to placeholders.
    let rows_left = opened.read(cx, |model| model.rows.len());
    assert!(
        rows_left < 40,
        "the rows out of range are let go: {rows_left}"
    );
}

#[gpui::test]
fn v_marks_seen_and_scrolls_to_the_next_unseen(cx: &mut TestAppContext) {
    // Reduced motion: the scroll lands at once instead of gliding.
    let rig = Rig::with_motion(cx, true);
    let (snapshot, rows) = snapshot_fixture(6, 2, 8);
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("v");
    visual.run_until_parked();
    let first = file_key(0);
    assert!(
        rig.review
            .commands()
            .iter()
            .any(|(_, command)| *command == ReviewCommand::MarkSeen(first.clone())),
        "V marks the file at the top"
    );
    assert_eq!(
        opened.read(cx, |model| model.seen_of(&first)),
        Some(SeenState::Seen),
        "the new snapshot is shown at once"
    );
    // Not yet: the check plays first.
    let selected_before = opened.read(cx, |model| model.selection.clone());
    assert_eq!(selected_before, None);
    opened.advance(
        cx,
        CHECK_STROKE.delay + CHECK_STROKE.duration + Duration::from_micros(1),
    );
    // Turn 1 holds files 0, 2 and 4; the next unseen file after file 0 is 2.
    let selected = opened.read(cx, |model| model.selection.clone());
    assert_eq!(selected, Some(ItemKey::Step(StepId(2))));

    // V again, and again: the files run out and the last one stays put.
    for expected in [Some(4), Some(1), Some(3), Some(5), None] {
        visual.simulate_keystrokes("v");
        visual.run_until_parked();
        opened.advance(
            cx,
            CHECK_STROKE.delay + CHECK_STROKE.duration + Duration::from_micros(1),
        );
        let selected = opened.read(cx, |model| model.selection.clone());
        if let Some(step) = expected {
            assert_eq!(selected, Some(ItemKey::Step(StepId(step))));
        } else {
            assert_eq!(
                selected,
                Some(ItemKey::Step(StepId(5))),
                "nothing further: stays"
            );
        }
    }
    assert_eq!(opened.read(cx, |model| model.snapshot.progress.seen), 6);
    let records = opened.record(cx);
    assert!(find(&records, "review.bar.progress").is_some());
    assert!(
        find(&records, "review.bar.looks-good").is_none(),
        "all reviewed: the button gives way to the text"
    );
}

#[gpui::test]
fn v_works_without_the_inspector_and_glides_when_motion_is_on(cx: &mut TestAppContext) {
    let (_, opened, _) = ready(cx, 8, 2, 40);
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("v");
    opened.advance(
        cx,
        CHECK_STROKE.delay + CHECK_STROKE.duration + Duration::from_micros(1),
    );
    // A glide is under way when the next step was laid out; its end is the
    // step at the top.
    let target = opened.read(cx, |model| model.selection.clone());
    assert!(target.is_some());
    let now = std::time::Instant::now() + Duration::new(2, 0);
    let mut visual = opened.visual(cx);
    for _ in 0..3 {
        visual.update(|window, cx| {
            opened
                .model
                .update(cx, |model, _| model.tick_glide(window, now));
        });
    }
    opened.draw(cx);
    let at = opened.read(cx, |model| model.list.logical_scroll_top());
    let landed = opened.read(cx, |model| model.items[at.item_ix].key());
    assert_eq!(Some(landed), target, "the glide ends on the step");
}

#[gpui::test]
fn j_k_walk_the_hunks_across_files(cx: &mut TestAppContext) {
    let (_, opened, _) = ready(cx, 4, 2, 12);
    let hunks: Vec<ItemKey> = opened.read(cx, |model| {
        model
            .items
            .iter()
            .filter(|item| item.is_hunk())
            .map(Item::key)
            .collect()
    });
    assert_eq!(hunks.len(), 4);
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("j");
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(hunks[0].clone())
    );
    visual.simulate_keystrokes("j j");
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(hunks[2].clone())
    );
    visual.simulate_keystrokes("j j");
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(hunks[3].clone()),
        "past the last: stays"
    );
    visual.simulate_keystrokes("k");
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(hunks[2].clone())
    );
    visual.simulate_keystrokes("k k k k");
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(hunks[0].clone()),
        "before the first: stays"
    );
}

#[gpui::test]
fn looks_good_sends_the_command(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 3, 1, 6);
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::LooksGood);
    visual.run_until_parked();
    assert!(
        rig.review
            .commands()
            .iter()
            .any(|(_, command)| *command == ReviewCommand::LooksGood)
    );
    assert_eq!(opened.read(cx, |model| model.snapshot.progress.seen), 3);
    let records = opened.record(cx);
    assert!(find(&records, "review.bar.looks-good").is_none());
}

#[gpui::test]
fn toggle_seen_sends_mark_unseen(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 3, 1, 6);
    let file = file_key(1);
    opened.update(cx, |model, cx| model.toggle_seen(file.clone(), cx));
    assert_eq!(
        opened.read(cx, |model| model.seen_of(&file)),
        Some(SeenState::Seen)
    );
    opened.update(cx, |model, cx| model.toggle_seen(file.clone(), cx));
    assert_eq!(
        opened.read(cx, |model| model.seen_of(&file)),
        Some(SeenState::Unseen)
    );
    let commands: Vec<ReviewCommand> = rig
        .review
        .commands()
        .into_iter()
        .map(|(_, command)| command)
        .filter(|command| !matches!(command, ReviewCommand::Prepare(_)))
        .collect();
    assert_eq!(
        commands,
        [
            ReviewCommand::MarkSeen(file.clone()),
            ReviewCommand::MarkUnseen(file)
        ]
    );
}

#[gpui::test]
fn the_check_of_a_step_toggles_when_clicked(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 3, 1, 6);
    let records = opened.record(cx);
    let check = must(&records, "review.step.item[0].check");
    let centre = check.bounds.center();
    let mut visual = opened.visual(cx);
    visual.simulate_click(centre, Modifiers::default());
    visual.run_until_parked();
    assert!(
        rig.review
            .commands()
            .iter()
            .any(|(_, c)| *c == ReviewCommand::MarkSeen(file_key(0)))
    );
    let records = opened.record(cx);
    let check = must(&records, "review.step.item[0].check");
    assert_eq!(check.semantic.state.checked, Some(true));
}

#[gpui::test]
fn noise_is_folded_by_default(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(3, 1, 6);
    let mut files = snapshot.files.to_vec();
    files[2].noise = Some(NoiseKind::Lockfile);
    snapshot.files = Arc::from(files);
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    let steps = |cx: &mut TestAppContext| {
        opened.read(cx, |model| {
            model
                .items
                .iter()
                .filter(|item| matches!(item, Item::Step { .. }))
                .count()
        })
    };
    assert_eq!(steps(cx), 2);
    let records = opened.record(cx);
    let noise = must(&records, "review.noise");
    assert_eq!(attr(noise, "count").as_deref(), Some("1"));
    assert_eq!(noise.semantic.state.expanded, Some(false));
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::ToggleNoise);
    visual.run_until_parked();
    assert_eq!(steps(cx), 3);
}

#[gpui::test]
fn earlier_turns_fold_and_unfold(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(3, 3, 6);
    let turns: Vec<_> = snapshot
        .turns
        .as_deref()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(ix, turn)| {
            let mut turn = turn.clone();
            turn.after_last_seen = ix == 2;
            turn
        })
        .collect();
    snapshot.turns = Some(Arc::from(turns));
    rig.review.set_all_rows(FIRST_ID, rows);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    let turns_shown = |cx: &mut TestAppContext| {
        opened.read(cx, |model| {
            model
                .items
                .iter()
                .filter(|item| matches!(item, Item::Turn { .. }))
                .count()
        })
    };
    assert_eq!(turns_shown(cx), 1);
    let records = opened.record(cx);
    assert_eq!(
        must(&records, "review.earlier").semantic.state.expanded,
        Some(false)
    );
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::ToggleEarlierTurns);
    visual.run_until_parked();
    assert_eq!(turns_shown(cx), 3);
}

#[gpui::test]
fn failed_snapshot_shows_the_reason(cx: &mut TestAppContext) {
    use lectern_services::changes::ChangeError;
    let cases = [
        (ReviewError::Missing, "review-failed-missing"),
        (
            ReviewError::NotInOpenProject,
            "review-failed-not-in-project",
        ),
        (
            ReviewError::Changes(ChangeError::NoMergeBase),
            "review-failed-changes",
        ),
    ];
    for (error, key) in cases {
        let rig = Rig::new(cx);
        let (mut snapshot, _) = snapshot_fixture(1, 1, 4);
        snapshot.state = LoadState::Failed(error.clone());
        let opened = rig.open(cx, session(), Some(snapshot));
        let records = opened.record(cx);
        let failed = must(&records, "review.failed");
        assert_eq!(label_key(failed), Some(key));
        if let ReviewError::Changes(_) = error {
            assert!(
                failed
                    .semantic
                    .label
                    .as_ref()
                    .unwrap()
                    .text
                    .contains("no merge base")
            );
        }
        assert!(find(&records, "review.failed.back").is_some(), "a way back");
        assert!(find(&records, "review.bar").is_none(), "no bar");
        // The back button pops the screen.
        let back = must(&records, "review.failed.back");
        let mut visual = opened.visual(cx);
        visual.simulate_click(back.bounds.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(rig.contributions.dispatched(), [WorkspaceCommand::Pop]);
    }
}

#[gpui::test]
fn a_target_that_is_not_a_review_shows_the_missing_message(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let screen = rig.contributions.screen();
    let window = cx.add_window(|_, _| crate::tests::support::Host::default_for_tests());
    let view = window
        .update(cx, |host, window, cx| {
            let view = (screen.build)(&lectern_services::workspace::Target::None, window, cx);
            host.set_child(view.clone());
            cx.notify();
            view
        })
        .unwrap();
    let _ = view;
    cx.run_until_parked();
    assert!(rig.review.opened().is_empty(), "nothing was opened");
}

#[gpui::test]
fn unrecognized_session_uses_groups(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(3, 1, 6);
    snapshot.turns = None;
    snapshot.degraded = Some(Degraded::UnrecognizedSession);
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    let records = opened.record(cx);
    assert!(find(&records, "review.notice.item[unrecognized]").is_some());
    assert!(find(&records, "review.file.item[src/file_000.rs]").is_some());
    assert!(
        records
            .iter()
            .all(|record| !record.semantic.id.as_str().starts_with("review.turn")),
        "no turns, no attribution labels"
    );
    let file = must(&records, "review.file.item[src/file_000.rs]");
    assert_eq!(attr(file, "attribution").as_deref(), Some("lines"));
}

#[gpui::test]
fn folder_project_has_no_progress(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, _) = snapshot_fixture(3, 1, 6);
    snapshot.degraded = Some(Degraded::FolderProject);
    let opened = rig.open(cx, session(), Some(snapshot));
    let records = opened.record(cx);
    assert!(find(&records, "review.notice.item[folder]").is_some());
    assert!(find(&records, "review.bar.progress").is_none());
    assert!(find(&records, "review.bar.looks-good").is_none());
    assert!(
        find(&records, "review.bar.comments").is_some(),
        "the comment count stays"
    );
    let strings = lectern_i18n::Strings::of(lectern_i18n::Locale::En);
    assert!(
        opened
            .read(cx, |model| model.toolbar_subtitle(&strings))
            .is_none()
    );
    // The Progress page says there is nothing to list.
    let tab = rig.open_tab(cx);
    let records = tab.record(cx);
    let empty = must(&records, "review.progress");
    assert_eq!(label_key(empty), Some("review-progress-empty"));
}

#[gpui::test]
fn drop_closes_the_review_and_cancels_analysis(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, _) = snapshot_fixture(2, 1, 4);
    let mut brief = (*snapshot.brief).clone();
    brief.analysis = AnalysisState::Running {
        started_at: Timestamp(1),
    };
    snapshot.brief = Arc::new(brief);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    let window = opened.window;
    drop(opened);
    window
        .update(cx, |host, _, cx| {
            host.clear_child();
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();
    let commands: Vec<ReviewCommand> = rig
        .review
        .commands()
        .into_iter()
        .map(|(_, c)| c)
        .filter(|c| !matches!(c, ReviewCommand::Prepare(_)))
        .collect();
    assert_eq!(commands, [ReviewCommand::CancelAnalyze]);
    assert_eq!(rig.review.closed(), [FIRST_ID]);
}

#[gpui::test]
fn the_analysis_row_shows_each_state(cx: &mut TestAppContext) {
    let states = [
        (
            AnalysisState::Unavailable(Arc::from("no tool")),
            "unavailable",
        ),
        (AnalysisState::Idle, "idle"),
        (AnalysisState::Failed(Arc::from("boom")), "failed"),
        (
            AnalysisState::Done {
                stale: true,
                partial: true,
            },
            "done",
        ),
    ];
    for (analysis, name) in states {
        let rig = Rig::new(cx);
        let (mut snapshot, _) = snapshot_fixture(1, 1, 4);
        let mut brief = (*snapshot.brief).clone();
        brief.analysis = analysis.clone();
        snapshot.brief = Arc::new(brief);
        let opened = rig.open(cx, session(), Some(snapshot));
        let records = opened.record(cx);
        let brief = must(&records, "review.brief");
        assert_eq!(attr(brief, "analysis").as_deref(), Some(name));
        let analyze = must(&records, "review.brief.analyze");
        match analysis {
            AnalysisState::Unavailable(_) => {
                assert!(analyze.semantic.state.disabled, "nothing can run");
            }
            AnalysisState::Idle => {
                assert!(!analyze.semantic.state.disabled, "Analyze can start");
            }
            AnalysisState::Failed(_) => {
                assert_eq!(attr(analyze, "state").as_deref(), Some("failed"));
            }
            AnalysisState::Done { .. } => {
                assert_eq!(attr(analyze, "state").as_deref(), Some("done"));
            }
            AnalysisState::Running { .. } => {}
        }
    }
}

#[gpui::test]
fn milestone_is_reported_once(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 3, 1, 6);
    assert!(opened.read(cx, |model| model.shows_rows()));
    let mut visual = opened.visual(cx);
    visual.update(|window, cx| window.simulate_next_frame(cx));
    visual.run_until_parked();
    assert_eq!(rig.contributions.milestones(), [Milestone::DiffShown]);
    // More frames and a new Prepared do not report again.
    rig.review.emit_prepared(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);
    let mut visual = opened.visual(cx);
    visual.update(|window, cx| window.simulate_next_frame(cx));
    visual.run_until_parked();
    assert_eq!(rig.contributions.milestones(), [Milestone::DiffShown]);
}

#[gpui::test]
fn escape_goes_to_the_panel_first(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 3, 1, 6);
    cx.update(|cx| {
        cx.bind_keys([gpui::KeyBinding::new(
            "escape",
            ProbeEscape,
            Some("Content"),
        )]);
    });
    let probe = opened
        .window
        .read_with(cx, |host, _| host.probe.clone())
        .unwrap();
    let mut visual = opened.visual(cx);
    // A hunk is selected and a panel is open: Esc is not ours.
    visual.simulate_keystrokes("j");
    let selected = opened.read(cx, |model| model.selection.clone());
    assert!(selected.is_some());
    let kernel = rig.kernel.inner().unwrap();
    kernel.emitter().emit(WorkspaceEvent::LayerChanged {
        window: WindowId(0),
        layer: Layer::Panel("context".into()),
        open: true,
    });
    rig.settle(cx);
    assert!(opened.read(cx, |model| model.layer_open));
    visual.simulate_keystrokes("escape");
    assert_eq!(opened.read(cx, |model| model.selection.clone()), selected);
    assert_eq!(
        probe.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "it went on to workspace"
    );

    // The panel closes: now Esc clears the selection and stops.
    kernel.emitter().emit(WorkspaceEvent::LayerChanged {
        window: WindowId(0),
        layer: Layer::Panel("context".into()),
        open: false,
    });
    rig.settle(cx);
    visual.simulate_keystrokes("escape");
    assert_eq!(opened.read(cx, |model| model.selection.clone()), None);
    assert_eq!(
        probe.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "handled here"
    );

    // Nothing selected: Esc goes on (back).
    visual.simulate_keystrokes("escape");
    assert_eq!(probe.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[gpui::test]
fn brief_item_jumps_to_the_line(cx: &mut TestAppContext) {
    use lectern_services::review::{BriefItem, BriefRule, DiffSide, LineAnchor};
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(4, 1, 12);
    snapshot.turns = None;
    snapshot.degraded = Some(Degraded::UnrecognizedSession);
    let mut brief = (*snapshot.brief).clone();
    brief.items = Arc::from(vec![
        BriefItem {
            rule: BriefRule::Drifted,
            critical: true,
            file: Some(file_key(2)),
            line: Some(LineAnchor {
                side: DiffSide::New,
                line: 5,
            }),
            count: 1,
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
    let records = opened.record(cx);
    let first = must(&records, "review.brief.item[drifted:src/file_002.rs]");
    assert_eq!(attr(first, "line").as_deref(), Some("5"));
    let mut visual = opened.visual(cx);
    visual.simulate_click(first.bounds.center(), Modifiers::default());
    visual.run_until_parked();
    let selection = opened.read(cx, |model| model.selection.clone());
    let Some(ItemKey::Row { rows, new, .. }) = selection else {
        panic!("a row is selected: {selection:?}");
    };
    assert_eq!(new, Some(5));
    assert_eq!(
        rows,
        RowsKey::Final {
            file: file_key(2),
            context: 3
        }
    );

    // An entry without a line goes to the file's title.
    let records = opened.record(cx);
    let second = find(&records, "review.brief.item[edit-failed:src/file_003.rs]");
    if let Some(second) = second {
        let mut visual = opened.visual(cx);
        visual.simulate_click(second.bounds.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            opened.read(cx, |model| model.selection.clone()),
            Some(ItemKey::File(file_key(3)))
        );
    }
}

#[gpui::test]
fn the_brief_item_without_a_line_goes_to_the_step(cx: &mut TestAppContext) {
    let (_, opened, _) = ready(cx, 4, 2, 8);
    let file: FileKey = file_key(1);
    opened.update(cx, |model, cx| {
        model.jump_to_brief(Some(file.clone()), None, cx)
    });
    // File 1 is the first file of turn 2.
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(ItemKey::Step(StepId(1)))
    );
}

#[gpui::test]
fn the_progress_tab_lists_the_files_and_jumps(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 5, 2, 8);
    // File 4 is noise: it is listed last, behind its heading.
    opened.update(cx, |_, _| {});
    let mut next = (*opened.read(cx, |model| model.snapshot.clone())).clone();
    next.generation += 1;
    let mut files = next.files.to_vec();
    files[4].noise = Some(NoiseKind::Generated);
    next.files = Arc::from(files);
    let mut seen = next.seen.to_vec();
    seen[1] = SeenState::Seen;
    seen[2] = SeenState::ChangedSinceSeen {
        since: Timestamp(1_700_000_000_000),
    };
    next.seen = Arc::from(seen);
    next.progress = lectern_services::review::Progress {
        total: 5,
        seen: 1,
        changed_since_seen: 1,
    };
    rig.review.set_snapshot(FIRST_ID, next);
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);

    let tab = rig.open_tab(cx);
    let records = tab.record(cx);
    let list = must(&records, "review.progress");
    assert_eq!(attr(list, "total").as_deref(), Some("5"));
    assert_eq!(attr(list, "seen").as_deref(), Some("1"));
    assert_eq!(attr(list, "changed").as_deref(), Some("1"));
    for i in 0..5 {
        assert!(
            find(
                &records,
                &format!("review.progress.item[src/file_00{i}.rs]")
            )
            .is_some(),
            "file {i} is listed"
        );
    }
    let check = must(&records, "review.progress.item[src/file_001.rs].check");
    assert_eq!(check.semantic.state.checked, Some(true));
    let changed = must(&records, "review.progress-file.item[src/file_002.rs]");
    assert_eq!(attr(changed, "seen").as_deref(), Some("changed"));
    let noise = must(&records, "review.progress-file.item[src/file_004.rs]");
    assert_eq!(attr(noise, "noise").as_deref(), Some("true"));
    // The order is the snapshot's, with the noise heading before file 4.
    let order: Vec<usize> = (0..5)
        .map(|i| {
            records
                .iter()
                .position(|r| {
                    r.semantic.id.as_str() == format!("review.progress.item[src/file_00{i}.rs]")
                })
                .unwrap()
        })
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]));

    // A click selects the file and the narrative goes to its step.
    let row = must(&records, "review.progress.item[src/file_003.rs]");
    let mut visual = tab.visual(cx);
    visual.simulate_click(
        point(row.bounds.center().x, row.bounds.center().y),
        Modifiers::default(),
    );
    visual.run_until_parked();
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(ItemKey::Step(StepId(3)))
    );
    // The row shows the review's path as the aid text.
    let records = tab.record(cx);
    let row = must(&records, "review.progress.item[src/file_003.rs]");
    assert!(row.semantic.state.selected);

    // The arrow keys move the selection and Enter goes there.
    let window = tab.window;
    let focus = tab.tab.read_with(cx, gpui::Focusable::focus_handle);
    let mut visual = tab.visual(cx);
    visual.update(|window, cx| window.focus(&focus, cx));
    visual.simulate_keystrokes("down enter");
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(ItemKey::Step(StepId(4)))
    );
    let _ = window;
}

#[gpui::test]
fn the_progress_tab_without_a_review_says_so(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let tab = rig.open_tab(cx);
    let records = tab.record(cx);
    let empty = must(&records, "review.progress");
    assert_eq!(label_key(empty), Some("review-progress-empty"));
}

#[gpui::test]
fn events_of_another_review_reach_nobody(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 2, 1, 4);
    let commands = rig.review.commands().len();
    rig.review.emit_event(ReviewEvent::Changed {
        id: lectern_services::review::ReviewId(99),
        generation: 50,
    });
    rig.settle(cx);
    assert_eq!(opened.read(cx, |model| model.snapshot.generation), 2);
    assert_eq!(rig.review.commands().len(), commands);
}
