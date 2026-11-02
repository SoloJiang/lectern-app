//! The note editor is brought above the floating bar (docs/design/
//! review_view.md「编辑框的位置」, #380): after each way of opening it, the
//! whole editor, Cancel and Add Note included, is above the bar. Headless:
//! the list is drawn in GPUI's test platform; the bar is a node of the
//! frame, so the test measures against what the person sees.

use std::sync::Arc;

use gpui::{Modifiers, MouseButton, Pixels, TestAppContext, point, px, size};
use lectern_services::changes::SideText;
use lectern_services::review::{
    DiffRow, DiffSide, FileRows, HunkAttribution, HunkInfo, NoteId, NoteState, ReviewSnapshot,
    RowsKey, RowsSide,
};
use lectern_ui::semantic::Record;

use crate::actions::CancelNote;
use crate::items::{Item, ItemKey};
use crate::notes::EditorTarget;
use crate::testing::FIRST_ID;
use crate::tests::notes::{note, on_step, set_notes};
use crate::tests::screen::{find, must, session};
use crate::tests::step2::{final_key, new_oid, old_oid, sample_snapshot, step_key};
use crate::tests::support::{Opened, Rig};

/// `count` context lines: the diff is much taller than the window.
fn tall_rows(key: RowsKey, count: u32) -> FileRows {
    let rows: Vec<DiffRow> = (0..count)
        .map(|n| DiffRow::Context {
            old: n,
            new: n,
            old_byte: None,
            new_byte: None,
            hunk: Some(0),
        })
        .collect();
    let text: String = (0..count).map(|n| format!("line {n}\n")).collect();
    FileRows {
        key,
        old: RowsSide {
            oid: Some(old_oid()),
            text: SideText::Text(Arc::from(text.as_bytes())),
        },
        new: RowsSide {
            oid: Some(new_oid()),
            text: SideText::Text(Arc::from(text.as_bytes())),
        },
        rows: Arc::from(rows),
        hunks: Arc::from(vec![HunkInfo {
            old: 0..count,
            new: 0..count,
            attribution: HunkAttribution {
                lines: true,
                turns: Arc::from(Vec::new()),
                others: Arc::from(Vec::new()),
                drifted: 0,
            },
        }]),
        inline: Arc::from(Vec::new()),
    }
}

/// The review open on `tall_rows`, drawn.
fn open_tall(cx: &mut TestAppContext) -> (Rig, Opened) {
    open_tall_with(cx, sample_snapshot())
}

fn open_tall_with(cx: &mut TestAppContext, snapshot: ReviewSnapshot) -> (Rig, Opened) {
    let rig = Rig::new(cx);
    rig.review.set_auto_prepare(true);
    for key in [step_key(), final_key()] {
        rig.review
            .set_rows(FIRST_ID, key.clone(), tall_rows(key, 300));
    }
    rig.review
        .set_text(FIRST_ID, crate::testing::prompt_key(0), "Make the change.");
    rig.review
        .set_text(FIRST_ID, crate::testing::agent_key(0), "Done.");
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    (rig, opened)
}

fn row_id(lines: &str) -> String {
    format!("review.diff.item[step:0].row.item[{lines}]")
}

/// The lowest row that is inside the list, as `old:new`.
fn lowest_row(records: &[Record], bottom: Pixels) -> String {
    records
        .iter()
        .filter_map(|record| {
            let id = record.semantic.id.as_str();
            let lines = id
                .strip_prefix("review.diff.item[step:0].row.item[")?
                .strip_suffix(']')?;
            (record.bounds.bottom() <= bottom).then(|| (record.bounds.bottom(), lines.to_owned()))
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
        .map(|(_, lines)| lines)
        .expect("a row in the list")
}

/// Scrolls the list so that rows far from the top are in view.
fn scroll_down(opened: &Opened, cx: &mut TestAppContext) {
    opened.update(cx, |model, _| {
        let at = model
            .items
            .iter()
            .position(|item| matches!(item, crate::items::Item::Row { ix, .. } if *ix == 60))
            .expect("row 60");
        model.list.scroll_to(gpui::ListOffset {
            item_ix: at,
            offset_in_item: px(0.0),
        });
    });
    opened.draw(cx);
}

/// What the person sees: the editor and the bar, in the last frame.
struct Seen {
    editor: gpui::Bounds<Pixels>,
    cancel: gpui::Bounds<Pixels>,
    save: gpui::Bounds<Pixels>,
    text: gpui::Bounds<Pixels>,
    bar: gpui::Bounds<Pixels>,
    list: gpui::Bounds<Pixels>,
}

fn seen(opened: &Opened, cx: &mut TestAppContext) -> Seen {
    let records = opened.record(cx);
    let bounds = |id: &str| must(&records, id).bounds;
    Seen {
        editor: bounds("review.note-editor"),
        cancel: bounds("review.note-editor.cancel"),
        save: bounds("review.note-editor.save"),
        text: bounds("review.note-editor.text"),
        bar: bounds("review.bar"),
        list: opened.read(cx, |model| model.list.viewport_bounds()),
    }
}

/// The whole editor is above the bar and inside the list; the text field is
/// in view.
#[track_caller]
fn assert_clear(opened: &Opened, cx: &mut TestAppContext, how: &str) {
    // Frames for the list to lay the editor out, and for the view to move.
    opened.draw(cx);
    opened.draw(cx);
    let seen = seen(opened, cx);
    let above = seen.bar.top();
    for (name, bounds) in [
        ("editor", seen.editor),
        ("cancel", seen.cancel),
        ("save", seen.save),
        ("text", seen.text),
    ] {
        assert!(
            bounds.bottom() <= above + px(0.5),
            "{how}: the {name} ends at {} under the bar at {above}",
            bounds.bottom()
        );
        assert!(
            bounds.top() >= seen.list.top() - px(0.5),
            "{how}: the {name} starts at {} above the list at {}",
            bounds.top(),
            seen.list.top()
        );
    }
}

/// The lowest row that is wholly above the bar, as `old:new`, with the row
/// `back` rows above it.
fn rows_above_the_bar(records: &[Record], back: usize) -> (String, String) {
    let bar = must(records, "review.bar").bounds.top();
    let mut rows: Vec<(Pixels, String)> = records
        .iter()
        .filter_map(|record| {
            let lines = record
                .semantic
                .id
                .as_str()
                .strip_prefix("review.diff.item[step:0].row.item[")?
                .strip_suffix(']')?;
            (record.bounds.bottom() <= bar).then(|| (record.bounds.bottom(), lines.to_owned()))
        })
        .collect();
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let last = rows.len() - 1;
    (rows[last - back].1.clone(), rows[last].1.clone())
}

fn row_key(lines: &str) -> ItemKey {
    let (old, new) = lines.split_once(':').expect("old:new");
    ItemKey::Row {
        rows: step_key(),
        old: old.parse().ok(),
        new: new.parse().ok(),
    }
}

fn click(opened: &Opened, cx: &mut TestAppContext, at: gpui::Point<Pixels>) {
    let mut visual = opened.visual(cx);
    visual.simulate_click(at, Modifiers::default());
    visual.run_until_parked();
}

/// Clicks the number of the lowest row of the list, which is under the bar.
fn click_lowest_number(opened: &Opened, cx: &mut TestAppContext) {
    let records = opened.record(cx);
    let list = opened.read(cx, |model| model.list.viewport_bounds());
    let lowest = lowest_row(&records, list.bottom());
    let bounds = must(&records, &row_id(&lowest)).bounds;
    let bar = must(&records, "review.bar").bounds;
    assert!(
        bounds.bottom() > bar.top(),
        "the lowest row {lowest} is under the bar: {} > {}",
        bounds.bottom(),
        bar.top()
    );
    click(
        opened,
        cx,
        point(bounds.origin.x + px(12.0), bounds.center().y),
    );
}

#[gpui::test]
fn a_line_number_at_the_bottom_opens_the_editor_above_the_bar(cx: &mut TestAppContext) {
    let (_rig, opened) = open_tall(cx);
    scroll_down(&opened, cx);
    click_lowest_number(&opened, cx);
    assert!(opened.read(cx, |model| model.editor.is_some()));
    assert_clear(&opened, cx, "line number");
    assert_eq!(opened.read(cx, |model| model.reveal_editor), 0, "done");
}

#[gpui::test]
fn c_at_the_bottom_opens_the_editor_above_the_bar(cx: &mut TestAppContext) {
    let (_rig, opened) = open_tall(cx);
    scroll_down(&opened, cx);
    let mut visual = opened.visual(cx);
    let records = opened.record(cx);
    let list = opened.read(cx, |model| model.list.viewport_bounds());
    let lowest = lowest_row(&records, list.bottom());
    opened.update(cx, |model, cx| model.select(Some(row_key(&lowest)), cx));
    visual.simulate_keystrokes("c");
    assert!(opened.read(cx, |model| model.editor.is_some()));
    assert_clear(&opened, cx, "C");
}

#[gpui::test]
fn the_plus_at_the_bottom_opens_the_editor_above_the_bar(cx: &mut TestAppContext) {
    let (_rig, opened) = open_tall(cx);
    scroll_down(&opened, cx);
    let records = opened.record(cx);
    let (_, above) = rows_above_the_bar(&records, 0);
    opened.update(cx, |model, cx| model.select(Some(row_key(&above)), cx));
    let records = opened.record(cx);
    let plus = must(&records, &format!("{}.add-note", row_id(&above))).bounds;
    click(&opened, cx, plus.center());
    assert!(opened.read(cx, |model| model.editor.is_some()));
    assert_clear(&opened, cx, "+");
}

#[gpui::test]
fn add_note_for_a_range_at_the_bottom_opens_the_editor_above_the_bar(cx: &mut TestAppContext) {
    let (_rig, opened) = open_tall(cx);
    scroll_down(&opened, cx);
    let mut visual = opened.visual(cx);
    let records = opened.record(cx);
    let (first, last) = rows_above_the_bar(&records, 2);
    let code = |records: &[Record], lines: &str| must(records, &row_id(lines)).bounds.center();
    let (start, end) = (code(&records, &first), code(&records, &last));
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    opened.draw(cx);
    visual.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
    opened.draw(cx);
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    opened.draw(cx);
    assert!(opened.read(cx, |model| model.range.is_some()), "a range");
    let records = opened.record(cx);
    let button = must(&records, "review.range.add-note").bounds;
    click(&opened, cx, button.center());
    assert!(opened.read(cx, |model| model.editor.is_some()));
    assert_clear(&opened, cx, "Add Note");
}

#[gpui::test]
fn edit_at_the_bottom_opens_the_editor_above_the_bar(cx: &mut TestAppContext) {
    let mut snapshot = sample_snapshot();
    // A note on line 100, which shows after that row.
    set_notes(
        &mut snapshot,
        vec![note(1, DiffSide::New, 100, NoteState::Pending, on_step())],
    );
    let (_rig, opened) = open_tall_with(cx, snapshot);
    // Scrolls so that the note is among the lowest items of the list.
    let at = opened.read(cx, |model| {
        model
            .items
            .iter()
            .position(|item| matches!(item, Item::Note { .. }))
            .expect("the note")
    });
    let mut found = false;
    for back in 20..120 {
        opened.update(cx, |model, _| {
            model.list.scroll_to(gpui::ListOffset {
                item_ix: at - back,
                offset_in_item: px(0.0),
            });
        });
        opened.draw(cx);
        let records = opened.record(cx);
        let (Some(note), Some(bar)) = (
            find(&records, "review.note.item[0000000000000001]"),
            find(&records, "review.bar"),
        ) else {
            continue;
        };
        let list = opened.read(cx, |model| model.list.viewport_bounds());
        if note.bounds.bottom() > bar.bounds.top() && note.bounds.bottom() <= list.bottom() {
            found = true;
            break;
        }
    }
    assert!(found, "the note block is under the bar");
    let model = opened.model.clone();
    let mut visual = opened.visual(cx);
    visual
        .update(|window, cx| model.update(cx, |model, cx| model.edit_note(NoteId(1), window, cx)));
    assert_eq!(
        opened.read(cx, |model| model.editor.as_ref().map(|e| e.target.clone())),
        Some(EditorTarget::Existing(NoteId(1)))
    );
    assert_clear(&opened, cx, "Edit");
}

/// The editor is higher than the room above the bar: the text field stays
/// in view, at the top of the list.
#[gpui::test]
fn an_editor_higher_than_the_room_keeps_its_text_field_in_view(cx: &mut TestAppContext) {
    let (_rig, opened) = open_tall(cx);
    scroll_down(&opened, cx);
    let visual = opened.visual(cx);
    visual.simulate_resize(size(px(900.0), px(140.0)));
    opened.draw(cx);
    click_lowest_number(&opened, cx);
    opened.draw(cx);
    opened.draw(cx);
    let seen = seen(&opened, cx);
    assert!(
        seen.editor.size.height > seen.list.size.height - px(72.0),
        "the editor {} is higher than the room",
        seen.editor.size.height
    );
    assert!(
        seen.text.top() >= seen.list.top() - px(0.5) && seen.text.bottom() <= seen.bar.top(),
        "the text field {:?} is in view above the bar {:?}",
        seen.text,
        seen.bar
    );
}

/// An editor that is in view already does not move the list.
#[gpui::test]
fn an_editor_in_view_does_not_move_the_list(cx: &mut TestAppContext) {
    let (_rig, opened) = open_tall(cx);
    scroll_down(&opened, cx);
    let records = opened.record(cx);
    let high = rows_above_the_bar(&records, 25).0;
    let top = |opened: &Opened, cx: &mut TestAppContext| {
        opened.read(cx, |model| {
            let top = model.list.logical_scroll_top();
            (top.item_ix, top.offset_in_item)
        })
    };
    let before = top(&opened, cx);
    opened.update(cx, |model, cx| model.select(Some(row_key(&high)), cx));
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("c");
    assert_clear(&opened, cx, "in view");
    assert_eq!(top(&opened, cx), before);
}

/// An editor taken away before it is drawn leaves nothing waiting.
#[gpui::test]
fn a_closed_editor_leaves_nothing_to_reveal(cx: &mut TestAppContext) {
    let (_rig, opened) = open_tall(cx);
    scroll_down(&opened, cx);
    let mut visual = opened.visual(cx);
    let records = opened.record(cx);
    let (_, above) = rows_above_the_bar(&records, 0);
    opened.update(cx, |model, cx| model.select(Some(row_key(&above)), cx));
    let model = opened.model.clone();
    visual.update(|window, cx| {
        model.update(cx, |model, cx| model.add_note_at_selection(window, cx));
    });
    assert!(opened.read(cx, |model| model.reveal_editor) > 0);
    visual.dispatch_action(CancelNote);
    opened.draw(cx);
    assert_eq!(opened.read(cx, |model| model.reveal_editor), 0);
    assert!(opened.read(cx, |model| model.editor.is_none()));
}

/// The editor of a line that is not near what is shown (the last line of a
/// range dragged upwards, scrolled away from since): the list does not lay
/// it out, so it is scrolled to first. `from` is the row to scroll to before
/// C: a screen or more away from the selected row, below it or above it.
fn c_far_from_the_row(cx: &mut TestAppContext, from: u32, how: &str) {
    let (_rig, opened) = open_tall(cx);
    let at = |opened: &Opened, cx: &mut TestAppContext, row: u32| {
        opened.update(cx, |model, _| {
            let ix = model
                .items
                .iter()
                .position(|item| matches!(item, Item::Row { ix, .. } if *ix == row))
                .expect("a row");
            model.list.scroll_to(gpui::ListOffset {
                item_ix: ix,
                offset_in_item: px(0.0),
            });
        });
        opened.draw(cx);
    };
    at(&opened, cx, 150);
    let records = opened.record(cx);
    let (_, above) = rows_above_the_bar(&records, 0);
    opened.update(cx, |model, cx| model.select(Some(row_key(&above)), cx));
    at(&opened, cx, from);
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("c");
    assert!(opened.read(cx, |model| model.editor.is_some()));
    assert_clear(&opened, cx, how);
}

#[gpui::test]
fn an_editor_far_below_the_list_is_brought_into_view(cx: &mut TestAppContext) {
    c_far_from_the_row(cx, 10, "far below");
}

#[gpui::test]
fn an_editor_above_the_list_is_brought_into_view(cx: &mut TestAppContext) {
    c_far_from_the_row(cx, 250, "above");
}
