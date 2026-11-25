//! Selecting the text of a diff (docs/design/review_view.md「测试」「文字选择
//! （#421）」): the offsets between the shown and the source text, the text
//! Copy takes, and the ⌥ drag, Copy, Esc and what clears a selection.
//! Headless: the mouse is simulated in GPUI's test platform, whose text
//! system places real glyphs, and a frame is drawn between the events (the
//! code cells register their layouts when they are painted).

use std::sync::Arc;

use gpui::{Modifiers, Point, TestAppContext, VisualTestContext, point, px};
use lectern_services::changes::SideText;
use lectern_services::review::{
    DiffRow, DiffSide, FileRows, HunkAttribution, HunkInfo, RowsKey, RowsSide,
};

use crate::actions::ToggleDiffMode;
use crate::highlight::Half;
use crate::items::{self, DiffMode};
use crate::lines::{self, MAX_LINE_BYTES};
use crate::testing::FIRST_ID;
use crate::tests::notes::{editor_target, row};
use crate::tests::range::{LEFT, code, drag_rows, long_rows, number, on, open_long};
use crate::tests::step2::{new_oid, old_oid, step_key};
use crate::tests::support::Opened;
use crate::text_select::{TextPoint, TextSelection, copied_text, lines_of};

type At = (Option<u32>, Option<u32>);

// ----- the pure functions ---------------------------------------------------------

fn side_of(bytes: &[u8]) -> RowsSide {
    RowsSide {
        oid: None,
        text: SideText::Text(Arc::from(bytes)),
    }
}

#[test]
fn source_offset_inverts_display_offset() {
    // Tabs: each is four cells; inside one, the offset is the tab's own.
    let tabs = lines::display_line(&side_of(b"\tab\tcd"), None, 0);
    assert_eq!(tabs.text.as_ref(), "    ab    cd");
    for source in 0..=6u32 {
        assert_eq!(tabs.source_offset(tabs.map(source)), source, "{source}");
    }
    assert_eq!(
        (1..4).map(|d| tabs.source_offset(d)).collect::<Vec<_>>(),
        [0, 0, 0],
        "in the first tab's spaces: before it"
    );
    assert_eq!(tabs.source_offset(7), 3, "in the second tab's spaces");
    assert_eq!(tabs.source_offset(99), 6, "past the end: the end");

    // Several bytes to a character: the edges map both ways.
    let wide = lines::display_line(&side_of("é中😀x".as_bytes()), None, 0);
    let text = wide.text.to_string();
    for (at, _) in text.char_indices().chain([(text.len(), ' ')]) {
        let source = wide.source_offset(at);
        assert_eq!(wide.map(source), at, "{at}");
    }

    // A long line is cut where it is drawn: the end of the text is the end
    // of what it holds, not of the source.
    let long = "y".repeat(MAX_LINE_BYTES + 50);
    let cut = lines::display_line(&side_of(long.as_bytes()), None, 0);
    assert_eq!(cut.kept() as usize, MAX_LINE_BYTES);
    assert_eq!(cut.source_offset(cut.text.len()) as usize, MAX_LINE_BYTES);
    assert_eq!(cut.map(cut.kept()), cut.text.len());

    // Not UTF-8: only the two ends.
    let lossy = lines::display_line(&side_of(&[b'a', 0xff, b'b']), None, 0);
    assert!(lossy.lossy());
    assert_eq!(lossy.source_offset(0), 0);
    assert_eq!(lossy.source_offset(2), lossy.kept());
    assert_eq!(lossy.map(0), 0);
    assert_eq!(lossy.map(lossy.kept()), lossy.text.len());
}

/// Unified rows, by index: 0 `0:0` context, 1 `1:-` removed, 2 `-:1`
/// added, 3 `2:2` context, 4 `-:3` added (a line longer than is drawn).
fn copy_rows(new_text: SideText) -> FileRows {
    let context = |old, new| DiffRow::Context {
        old,
        new,
        old_byte: None,
        new_byte: None,
        hunk: Some(0),
    };
    FileRows {
        key: step_key(),
        old: RowsSide {
            oid: Some(old_oid()),
            text: SideText::Text(Arc::from("keep\n\tgone 中\nrest\n".as_bytes())),
        },
        new: RowsSide {
            oid: Some(new_oid()),
            text: new_text,
        },
        rows: Arc::from(vec![
            context(0, 0),
            DiffRow::Removed {
                old: 1,
                pair: None,
                old_byte: None,
                hunk: 0,
            },
            DiffRow::Added {
                new: 1,
                pair: None,
                new_byte: None,
                hunk: 0,
            },
            context(2, 2),
            DiffRow::Added {
                new: 3,
                pair: None,
                new_byte: None,
                hunk: 0,
            },
        ]),
        hunks: Arc::from(vec![HunkInfo {
            old: 0..3,
            new: 0..4,
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

fn point_on(at: At, side: DiffSide, byte: u32) -> TextPoint {
    TextPoint { at, side, byte }
}

fn selection(anchor: TextPoint, head: TextPoint) -> TextSelection {
    TextSelection {
        rows: step_key(),
        half: None,
        oids: (Some(old_oid()), Some(new_oid())),
        anchor,
        head,
    }
}

#[test]
fn copied_text_is_the_source() {
    let long = "x".repeat(MAX_LINE_BYTES + 100);
    let new = format!("keep\n\tnew line\nrest\n{long}\n");
    let rows = copy_rows(SideText::Text(Arc::from(new.as_bytes())));
    let display = items::display_rows(&rows, DiffMode::Unified);

    // From "ep" of the context line over the removed and the added line:
    // each its own side's source, the tab a tab, no numbers or signs.
    let first = point_on((Some(0), Some(0)), DiffSide::New, 2);
    let last = point_on((None, Some(1)), DiffSide::New, 4);
    let expected = "ep\n\tgone 中\n\tnew";
    assert_eq!(
        copied_text(&rows, &display, &selection(first, last)).as_deref(),
        Some(expected)
    );
    assert_eq!(
        copied_text(&rows, &display, &selection(last, first)).as_deref(),
        Some(expected),
        "dragged upward"
    );

    // To the end of a line that is cut where it is drawn: the whole source.
    let from = point_on((Some(2), Some(2)), DiffSide::New, 0);
    let to = point_on((None, Some(3)), DiffSide::New, MAX_LINE_BYTES as u32);
    assert_eq!(
        copied_text(&rows, &display, &selection(from, to)),
        Some(format!("rest\n{long}"))
    );

    // A side without text copies what is shown for it (nothing).
    let large = copy_rows(SideText::TooLarge { size: 1 << 30 });
    let display = items::display_rows(&large, DiffMode::Unified);
    let first = point_on((Some(0), Some(0)), DiffSide::New, 0);
    let last = point_on((None, Some(1)), DiffSide::New, 0);
    assert_eq!(
        copied_text(&large, &display, &selection(first, last)).as_deref(),
        Some("\n\tgone 中\n")
    );
}

// ----- the screen ---------------------------------------------------------------------

/// The display index of the row with these line numbers.
fn ix_of(opened: &Opened, cx: &mut TestAppContext, at: At) -> (RowsKey, u32) {
    opened.read(cx, |model| {
        let (key, display) = model
            .displays
            .iter()
            .find(|(key, _)| items::same_diff(key, &step_key()))
            .expect("the step's rows");
        let ix = display
            .iter()
            .position(|row| row.fold.is_none() && lines_of(row) == at)
            .expect("the row");
        (key.clone(), ix as u32)
    })
}

/// Just right of the left edge of character `index` (display offset) of a
/// painted code cell: the hit test takes that edge.
pub(super) fn char_at(
    opened: &Opened,
    cx: &mut TestAppContext,
    at: At,
    half: Half,
    index: usize,
) -> Point<gpui::Pixels> {
    let (key, ix) = ix_of(opened, cx, at);
    opened.read(cx, |model| {
        let layout = model
            .text_layouts
            .borrow()
            .get(&(key, ix, half))
            .cloned()
            .expect("the cell was painted");
        // The end of the line has no character: a little past it.
        let x = match layout.position_for_index(index) {
            Some(place) if index < layout.len() => place.x + px(0.5),
            _ => layout.bounds().right() + px(2.0),
        };
        point(x, layout.bounds().center().y)
    })
}

fn down(
    visual: &mut VisualTestContext,
    opened: &Opened,
    cx: &mut TestAppContext,
    at: Point<gpui::Pixels>,
    modifiers: Modifiers,
) {
    visual.simulate_mouse_down(at, LEFT, modifiers);
    opened.draw(cx);
}

fn moved(
    visual: &mut VisualTestContext,
    opened: &Opened,
    cx: &mut TestAppContext,
    to: Point<gpui::Pixels>,
    modifiers: Modifiers,
) {
    visual.simulate_mouse_move(to, Some(LEFT), modifiers);
    opened.draw(cx);
}

fn up(
    visual: &mut VisualTestContext,
    opened: &Opened,
    cx: &mut TestAppContext,
    at: Point<gpui::Pixels>,
    modifiers: Modifiers,
) {
    visual.simulate_mouse_up(at, LEFT, modifiers);
    opened.draw(cx);
}

/// A drag from `from` to `to` with `modifiers` held, released at `to`.
pub(super) fn drag_with(
    visual: &mut VisualTestContext,
    opened: &Opened,
    cx: &mut TestAppContext,
    from: Point<gpui::Pixels>,
    to: Point<gpui::Pixels>,
    modifiers: Modifiers,
) {
    down(visual, opened, cx, from, modifiers);
    moved(visual, opened, cx, to, modifiers);
    up(visual, opened, cx, to, modifiers);
}

/// What Copy would put on the clipboard now.
fn copied(opened: &Opened, cx: &mut TestAppContext) -> Option<String> {
    opened.read(cx, |model| {
        let selection = model.text.clone()?;
        let (key, rows) = model
            .rows
            .iter()
            .find(|(key, _)| items::same_diff(key, &selection.rows))?;
        copied_text(rows, model.displays.get(key)?, &selection)
    })
}

fn has_text(opened: &Opened, cx: &mut TestAppContext) -> bool {
    opened.read(cx, |model| model.text.is_some())
}

#[gpui::test]
fn option_drag_selects_text_and_plain_drag_selects_lines(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);

    // ⌥: "ne 1" of 1:1 over the removed and the added line 2 to "line" of -:3.
    let from = char_at(&opened, cx, (Some(1), Some(1)), Half::New, 2);
    let to = char_at(&opened, cx, (None, Some(3)), Half::New, 4);
    down(&mut visual, &opened, cx, from, Modifiers::alt());
    moved(&mut visual, &opened, cx, to, Modifiers::alt());
    // While the button is still down the highlight already follows the
    // pointer.
    assert!(opened.read(cx, |model| model.press.is_some()));
    let live = opened
        .record(cx)
        .iter()
        .filter(|record| crate::tests::screen::attr(record, "text_selected").is_some())
        .count();
    assert_eq!(live, 4);
    up(&mut visual, &opened, cx, to, Modifiers::alt());
    assert!(opened.read(cx, |model| model.press.is_none()));
    assert_eq!(opened.read(cx, |model| model.range.clone()), None);
    assert_eq!(opened.read(cx, |model| model.selection.clone()), None);
    assert_eq!(
        copied(&opened, cx).as_deref(),
        Some("ne 1\nline 2\nline 2\nline")
    );
    // The rows it covers say so.
    let records = opened.record(cx);
    let marked: Vec<String> = records
        .iter()
        .filter(|record| crate::tests::screen::attr(record, "text_selected").is_some())
        .map(|record| record.semantic.id.as_str().to_owned())
        .collect();
    assert_eq!(marked.len(), 4, "{marked:?}");

    // Without a modifier: a line range, and the text is gone.
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    assert!(opened.read(cx, |model| model.range.is_some()));
    assert!(!has_text(&opened, cx));

    // ⌘⌥ (or any other set) does nothing.
    let records = opened.record(cx);
    let range = opened.read(cx, |model| model.range.clone());
    let both = Modifiers {
        alt: true,
        platform: true,
        ..Modifiers::default()
    };
    drag_with(
        &mut visual,
        &opened,
        cx,
        code(&records, "0:0"),
        code(&records, "1:1"),
        both,
    );
    assert!(!has_text(&opened, cx));
    assert_eq!(opened.read(cx, |model| model.range.clone()), range);
}

#[gpui::test]
fn side_by_side_selects_one_half(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    visual.dispatch_action(ToggleDiffMode);
    visual.run_until_parked();
    opened.draw(cx);

    // From the start of the left half of 2:2 to the right half of 3:4: the
    // pointer's place only says how far along the left half it is (past
    // its end), the added line -:3 has no left half.
    let from = char_at(&opened, cx, (Some(2), Some(2)), Half::Old, 0);
    let records = opened.record(cx);
    let to = on(&records, "3:4", 0.9);
    drag_with(&mut visual, &opened, cx, from, to, Modifiers::alt());
    let selection = opened
        .read(cx, |model| model.text.clone())
        .expect("a selection");
    assert_eq!(selection.half, Some(DiffSide::Old));
    assert_eq!(selection.head.side, DiffSide::Old);
    assert_eq!(copied(&opened, cx).as_deref(), Some("line 2\nline 3"));
}

#[gpui::test]
fn selection_stops_at_a_fold(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    // From 3:4 over the fold to 9:10: the head stops at the end of 4:5.
    let from = char_at(&opened, cx, (Some(3), Some(4)), Half::New, 0);
    let records = opened.record(cx);
    let to = code(&records, "9:10");
    drag_with(&mut visual, &opened, cx, from, to, Modifiers::alt());
    let selection = opened
        .read(cx, |model| model.text.clone())
        .expect("a selection");
    assert_eq!(selection.head.at, (Some(4), Some(5)));
    assert_eq!(copied(&opened, cx).as_deref(), Some("line 4\nline 5"));

    // Back above the stretch: the head is the start of its first row.
    let from = char_at(&opened, cx, (Some(9), Some(10)), Half::New, 4);
    let records = opened.record(cx);
    let to = code(&records, "4:5");
    drag_with(&mut visual, &opened, cx, from, to, Modifiers::alt());
    let selection = opened
        .read(cx, |model| model.text.clone())
        .expect("a selection");
    assert_eq!(
        selection.head,
        point_on((Some(9), Some(10)), DiffSide::New, 0)
    );
    assert_eq!(copied(&opened, cx).as_deref(), Some("line"));
}

#[gpui::test]
fn cmd_c_copies_and_the_editor_keeps_its_own(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    let copy_available = |visual: &mut VisualTestContext| {
        visual.update(|window, cx| window.is_action_available(&gpui_component::input::Copy, cx))
    };
    assert!(
        !copy_available(&mut visual),
        "nothing selected: Copy is greyed out"
    );

    // The current line first; the drag keeps it.
    let records = opened.record(cx);
    visual.simulate_click(code(&records, "0:0"), Modifiers::default());
    opened.draw(cx);
    let from = char_at(&opened, cx, (Some(0), Some(0)), Half::New, 0);
    let to = char_at(&opened, cx, (Some(1), Some(1)), Half::New, 4);
    drag_with(&mut visual, &opened, cx, from, to, Modifiers::alt());
    assert!(copy_available(&mut visual));
    visual.simulate_keystrokes("cmd-c");
    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some("line 0\nline")
    );
    assert!(has_text(&opened, cx), "the selection stays after Copy");

    // C opens the editor on the current line and keeps the selection; ⌘C
    // in the editor copies the editor's text.
    visual.simulate_keystrokes("c");
    visual.run_until_parked();
    opened.draw(cx);
    assert!(editor_target(&opened, cx).is_some());
    assert!(has_text(&opened, cx));
    visual.simulate_input("typed words");
    visual.simulate_keystrokes("cmd-a cmd-c");
    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some("typed words")
    );
}

#[gpui::test]
fn escape_clears_the_text_first(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    let records = opened.record(cx);
    visual.simulate_click(code(&records, "0:0"), Modifiers::default());
    opened.draw(cx);
    let selected = opened.read(cx, |model| model.selection.clone());
    assert_eq!(selected, Some(row(&step_key(), Some(0), Some(0))));
    let from = char_at(&opened, cx, (Some(1), Some(1)), Half::New, 0);
    let to = char_at(&opened, cx, (None, Some(2)), Half::New, 3);
    drag_with(&mut visual, &opened, cx, from, to, Modifiers::alt());
    assert!(has_text(&opened, cx));

    visual.simulate_keystrokes("escape");
    assert!(!has_text(&opened, cx));
    assert_eq!(opened.read(cx, |model| model.selection.clone()), selected);
    visual.simulate_keystrokes("escape");
    assert_eq!(opened.read(cx, |model| model.selection.clone()), None);

    // Esc during a ⌥ drag cancels it: no selection is left.
    down(&mut visual, &opened, cx, from, Modifiers::alt());
    moved(&mut visual, &opened, cx, to, Modifiers::alt());
    visual.simulate_keystrokes("escape");
    assert!(opened.read(cx, |model| model.press.is_none()));
    up(&mut visual, &opened, cx, to, Modifiers::alt());
    assert!(!has_text(&opened, cx));
}

#[gpui::test]
fn option_drag_in_one_line_keeps_the_selected_item(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    let records = opened.record(cx);
    visual.simulate_click(code(&records, "0:0"), Modifiers::default());
    opened.draw(cx);
    let selected = opened.read(cx, |model| model.selection.clone());

    // Down and up on the same row: GPUI calls it a click, the row leaves it.
    let from = char_at(&opened, cx, (Some(1), Some(1)), Half::New, 1);
    let to = char_at(&opened, cx, (Some(1), Some(1)), Half::New, 4);
    drag_with(&mut visual, &opened, cx, from, to, Modifiers::alt());
    assert_eq!(copied(&opened, cx).as_deref(), Some("ine"));
    assert_eq!(opened.read(cx, |model| model.selection.clone()), selected);
    assert!(editor_target(&opened, cx).is_none());

    // The same on a line number: no editor, the current line stays.
    let records = opened.record(cx);
    let at = number(&records, "1:1");
    down(&mut visual, &opened, cx, at, Modifiers::alt());
    up(&mut visual, &opened, cx, at, Modifiers::alt());
    assert!(editor_target(&opened, cx).is_none());
    assert_eq!(opened.read(cx, |model| model.selection.clone()), selected);
}

#[gpui::test]
fn selection_survives_a_fold_expand_and_drops_on_change(cx: &mut TestAppContext) {
    let (rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    let from = char_at(&opened, cx, (Some(9), Some(10)), Half::New, 0);
    let to = char_at(&opened, cx, (Some(11), Some(11)), Half::New, 7);
    drag_with(&mut visual, &opened, cx, from, to, Modifiers::alt());
    let before = copied(&opened, cx);
    assert_eq!(before.as_deref(), Some("line 10\nline 10\nline 11"));
    let (key, first) = ix_of(&opened, cx, (Some(9), Some(10)));

    // Open the fold above it (the review has the rows with more context,
    // without the fold): the rows move down, the text is the same.
    let grown = match &key {
        RowsKey::Step { step, context } => RowsKey::Step {
            step: *step,
            context: context + 4,
        },
        RowsKey::Final { .. } => unreachable!("a step"),
    };
    let mut open = long_rows(grown.clone());
    let mut unfolded: Vec<DiffRow> = Vec::new();
    for diff_row in open.rows.iter() {
        match diff_row {
            DiffRow::Folded { old, .. } => {
                for line in old.clone() {
                    unfolded.push(DiffRow::Context {
                        old: line,
                        new: line + 1,
                        old_byte: None,
                        new_byte: None,
                        hunk: Some(0),
                    });
                }
            }
            other => unfolded.push(other.clone()),
        }
    }
    open.rows = Arc::from(unfolded);
    rig.review.set_rows(FIRST_ID, grown, open);
    let fold = ix_of(&opened, cx, (Some(4), Some(5))).1 + 1;
    opened.update(cx, |model, cx| model.expand_fold(&key, fold, cx));
    rig.settle(cx);
    opened.draw(cx);
    assert!(
        ix_of(&opened, cx, (Some(9), Some(10))).1 > first,
        "the fold opened"
    );
    assert_eq!(copied(&opened, cx), before);
    let span = opened
        .read(cx, |model| model.text_span.clone())
        .expect("drawn");
    assert_eq!(
        span.first.0 as u32,
        ix_of(&opened, cx, (Some(9), Some(10))).1
    );

    // New rows with another new side: the selection is over.
    let mut changed = long_rows(step_key());
    changed.new.oid = Some(lectern_services::project::BlobId::Sha1([7; 20]));
    for key in opened.read(cx, |model| {
        model
            .rows
            .keys()
            .filter(|held| items::same_diff(held, &step_key()))
            .cloned()
            .collect::<Vec<_>>()
    }) {
        let mut rows = changed.clone();
        rows.key = key.clone();
        rig.review.set_rows(FIRST_ID, key, rows);
    }
    rig.review.emit_prepared(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);
    assert!(!has_text(&opened, cx));
}
