//! The scroll position and the selection through rebuilds and splices
//! (docs/design/review_view.md「滚动位置与选中项」, `anchor_*`), and the
//! lines that move the list.

use std::sync::Arc;

use gpui::{ListOffset, Modifiers, TestAppContext, px};
use lectern_services::review::{NarrativeItem, RowsKey, SessionRef, TurnRef};
use lectern_services::session::{Agent, SessionKey, SourceId, ToolCallId, TurnId};

use crate::items::{Item, ItemKey};
use crate::testing::{FIRST_ID, snapshot_fixture};
use crate::tests::screen::{attr, must, session};
use crate::tests::support::{Rig, ready};

#[gpui::test]
fn anchor_selection_survives_a_rebuild_and_goes_with_its_item(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 4, 2, 8);
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("j j");
    let selected = opened.read(cx, |model| model.selection.clone());
    assert!(matches!(selected, Some(ItemKey::Hunk { .. })));

    // A rebuild that keeps the item: the selection stays.
    let mut next = (*opened.read(cx, |model| model.snapshot.clone())).clone();
    next.generation += 1;
    next.unclaimed = 3;
    rig.review.set_snapshot(FIRST_ID, next.clone());
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    assert_eq!(opened.read(cx, |model| model.selection.clone()), selected);

    // The step of the selected hunk disappears: the selection is cleared.
    let Some(ItemKey::Hunk {
        rows: RowsKey::Step { step, .. },
        ..
    }) = selected
    else {
        panic!("a step hunk was selected");
    };
    let mut gone = next;
    gone.generation += 1;
    let turns: Vec<_> = gone
        .turns
        .as_deref()
        .unwrap()
        .iter()
        .map(|turn| {
            let mut turn = turn.clone();
            let items: Vec<_> = turn
                .items
                .iter()
                .filter(|item| !matches!(item, NarrativeItem::Step(s) if s.step == step))
                .cloned()
                .collect();
            turn.items = Arc::from(items);
            turn
        })
        .collect();
    gone.turns = Some(Arc::from(turns));
    rig.review.set_snapshot(FIRST_ID, gone);
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    assert_eq!(opened.read(cx, |model| model.selection.clone()), None);
}

#[gpui::test]
fn anchor_a_splice_inside_a_placeholder_keeps_the_row(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (snapshot, rows) = snapshot_fixture(3, 1, 40);
    rig.review.set_all_rows(FIRST_ID, rows);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    // Scroll inside the first step's placeholder: 5 rows and 3 pixels down.
    let (placeholder, height) = opened.read(cx, |model| {
        let ix = model
            .items
            .iter()
            .position(|item| matches!(item, Item::Placeholder { .. }))
            .expect("a placeholder");
        (ix, model.line_height)
    });
    opened.update(cx, |model, _| {
        model.list.scroll_to(ListOffset {
            item_ix: placeholder,
            offset_in_item: height * 5.0 + px(3.0),
        });
    });
    opened.draw(cx);
    // The rows arrive: the top of the list is in the step's rows, not at the
    // start of the step.
    rig.review.emit_prepared(FIRST_ID);
    rig.settle(cx);
    let (item, ix, offset) = opened.read(cx, |model| {
        let at = model.list.logical_scroll_top();
        (
            model.items[at.item_ix].clone(),
            at.item_ix,
            at.offset_in_item,
        )
    });
    assert!(
        matches!(item, Item::Row { .. }),
        "the top of the list is a row of the step: {item:?}"
    );
    assert!(
        ix > placeholder + 3,
        "it is some rows into the segment: {ix}"
    );
    assert_eq!(offset, px(3.0));
}

#[gpui::test]
fn anchor_a_splice_before_the_top_moves_nothing_that_is_seen(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (snapshot, rows) = snapshot_fixture(4, 1, 30);
    let first = rows
        .iter()
        .find(|(key, _)| matches!(key, RowsKey::Step { step, .. } if step.0 == 0))
        .cloned()
        .expect("the rows of step 0");
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    // The top of the list is the last item, a tool call, 7 pixels in.
    let last = opened.read(cx, |model| model.items.len() - 1);
    opened.update(cx, |model, _| {
        model.list.scroll_to(ListOffset {
            item_ix: last,
            offset_in_item: px(7.0),
        });
    });
    opened.draw(cx);
    // The list clamps the scroll to its end; whatever it settled on is the top.
    let (before, before_offset) = opened.read(cx, |model| {
        let at = model.list.logical_scroll_top();
        (at.item_ix, at.offset_in_item)
    });
    let key = opened.read(cx, |model| model.items[before].key());
    assert!(before > 2, "far below step 0: {before}");
    // The placeholder of step 0, far above, is replaced by its rows.
    let grew = opened.update(cx, |model, _| {
        let count = model.items.len();
        model.store_rows(&first.0, Arc::new(first.1.clone()));
        model.items.len() - count
    });
    assert!(grew > 20, "the step's rows are many: {grew}");
    let (ix, offset) = opened.read(cx, |model| {
        let at = model.list.logical_scroll_top();
        (at.item_ix, at.offset_in_item)
    });
    assert_eq!(ix, before + grew, "the position moves down with the items");
    assert_eq!(opened.read(cx, |model| model.items[ix].key()), key);
    assert_eq!(offset, before_offset);
}

#[gpui::test]
fn a_subagent_line_goes_to_its_turn(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(2, 1, 4);
    let sub = SessionKey {
        source: SourceId(Arc::from("fake")),
        id: Arc::from("sub-agent"),
    };
    let mut sessions = snapshot.sessions.to_vec();
    sessions.push(SessionRef {
        key: sub,
        agent: Agent::Codex,
        spawned_by: None,
    });
    snapshot.sessions = Arc::from(sessions);
    let mut turns = snapshot.turns.as_deref().unwrap().to_vec();
    let call = ToolCallId(Arc::from("spawn-1"));
    let mut items = turns[0].items.to_vec();
    items.push(NarrativeItem::Subagent {
        session: 1,
        call: call.clone(),
    });
    turns[0].items = Arc::from(items);
    let mut sub_turn = turns[0].clone();
    sub_turn.at = TurnRef {
        session: 1,
        ordinal: 1,
    };
    sub_turn.id = TurnId(Arc::from("sub-turn"));
    sub_turn.items = Arc::from(Vec::new());
    sub_turn.prompt = None;
    turns.push(sub_turn);
    snapshot.turns = Some(Arc::from(turns));
    rig.review.set_all_rows(FIRST_ID, rows);
    let opened = rig.open(cx, session(), Some(snapshot));
    let records = opened.record(cx);
    let line = must(&records, "review.call.item[spawn-1]");
    assert_eq!(attr(line, "kind").as_deref(), Some("subagent"));
    let mut visual = opened.visual(cx);
    visual.simulate_click(line.bounds.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(ItemKey::Turn(TurnRef {
            session: 1,
            ordinal: 1
        }))
    );
}

#[gpui::test]
fn two_screens_of_one_review_each_keep_their_channel(cx: &mut TestAppContext) {
    let (rig, first, _) = ready(cx, 2, 1, 4);
    // A second screen of the same review: the same ID comes back.
    let second = rig.open(cx, session(), None);
    assert_eq!(rig.review.opened().len(), 1, "one review, two screens");
    drop(second);
    // The first one still hears the review.
    let applied = first.read(cx, |model| model.snapshot.generation);
    rig.review.emit_changed(FIRST_ID);
    rig.settle(cx);
    assert_eq!(
        first.read(cx, |model| model.snapshot.generation),
        applied + 1
    );
}

#[gpui::test]
fn a_jump_into_a_folded_turn_unfolds_it(cx: &mut TestAppContext) {
    use lectern_services::review::{DiffSide, LineAnchor};
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
    assert_eq!(turns_shown(cx), 1, "turns 1 and 2 are folded");
    // File 0 was changed in the folded turn 1; a brief entry with a line in a
    // session shows the step (the final rows are not in the narrative).
    let anchor = LineAnchor {
        side: DiffSide::New,
        line: 2,
    };
    opened.update(cx, |model, cx| {
        model.jump_to_brief(Some(crate::testing::file_key(0)), Some(anchor), cx)
    });
    assert_eq!(turns_shown(cx), 3, "the fold opened");
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(ItemKey::Step(lectern_services::review::StepId(0)))
    );
}
