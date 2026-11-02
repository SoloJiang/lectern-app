//! The line range of a review note (docs/design/review_view.md「测试」
//! 「拖选范围」, #373): the pure functions that make a range of a stretch of
//! rows, and the drag, the click that stays a click, the clearing, the
//! actions without a mouse and the way a range is drawn. Headless: the mouse
//! is simulated in GPUI's test platform, a frame is drawn between the events
//! (the screen draws its tracker from the frame after a press).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{Modifiers, MouseButton, Point, TestAppContext, VisualTestContext, point, px};
use lectern_services::changes::SideText;
use lectern_services::review::{
    DiffRow, DiffSide, Excerpt, ExcerptGap, FileRows, HunkAttribution, HunkInfo, NoteAnchor,
    NotePlace, NoteState, ReviewCommand, RowsKey, RowsSide, limits,
};
use lectern_ui::semantic::Record;

use crate::actions::{ExtendRangeDown, ExtendRangeUp, ToggleDiffMode};
use crate::items::{self, DiffMode, DisplayRow, ItemKey, RangeSpan};
use crate::notes::EditorTarget;
use crate::range::LineRange;
use crate::render_notes::{ExcerptLine, excerpt_lines};
use crate::testing::{FIRST_ID, file_key};
use crate::tests::notes::{
    change, editor_target, note, note_commands, on_step, row, select, set_notes, status,
};
use crate::tests::screen::{attr, find, must};
use crate::tests::step2::{final_key, new_oid, old_oid, sample_snapshot, step_key};
use crate::tests::support::{Opened, Rig};

// ----- the rows ---------------------------------------------------------------

/// Display rows (unified), by index:
///
/// ```text
///  0 context  0:0        6 context  4:5
///  1 context  1:1        7 fold     old 5..9
///  2 removed  2:-        8 context  9:10
///  3 added    -:2        9 removed 10:-
///  4 added    -:3       10 context 11:11
///  5 context  3:4
/// ```
///
/// Side by side the removed line 2 shares a row with the added line 2, so
/// there is one row less before the fold: 0 `0:0`, 1 `1:1`, 2 `2:2`, 3 `-:3`,
/// 4 `3:4`, 5 `4:5`, 6 fold, 7 `9:10`, 8 `10:-`, 9 `11:11`.
fn long_rows(key: RowsKey) -> FileRows {
    let context = |old, new, hunk| DiffRow::Context {
        old,
        new,
        old_byte: None,
        new_byte: None,
        hunk: Some(hunk),
    };
    let rows = vec![
        context(0, 0, 0),
        context(1, 1, 0),
        DiffRow::Removed {
            old: 2,
            pair: Some(2),
            old_byte: None,
            hunk: 0,
        },
        DiffRow::Added {
            new: 2,
            pair: Some(2),
            new_byte: None,
            hunk: 0,
        },
        DiffRow::Added {
            new: 3,
            pair: None,
            new_byte: None,
            hunk: 0,
        },
        context(3, 4, 0),
        context(4, 5, 0),
        DiffRow::Folded {
            old: 5..9,
            new: 6..10,
            old_byte: None,
            new_byte: None,
        },
        context(9, 10, 1),
        DiffRow::Removed {
            old: 10,
            pair: None,
            old_byte: None,
            hunk: 1,
        },
        context(11, 11, 1),
    ];
    let text: String = (0..14).map(|n| format!("line {n}\n")).collect();
    let hunk = |old: std::ops::Range<u32>, new: std::ops::Range<u32>| HunkInfo {
        old,
        new,
        attribution: HunkAttribution {
            lines: true,
            turns: Arc::from(Vec::new()),
            others: Arc::from(Vec::new()),
            drifted: 0,
        },
    };
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
        hunks: Arc::from(vec![hunk(0..5, 0..6), hunk(9..12, 10..12)]),
        inline: Arc::from(Vec::new()),
    }
}

fn unified() -> Vec<DisplayRow> {
    items::display_rows(&long_rows(step_key()), DiffMode::Unified)
}

fn split() -> Vec<DisplayRow> {
    items::display_rows(&long_rows(step_key()), DiffMode::SideBySide)
}

fn span(side: DiffSide, line: u32, last: u32) -> Option<RangeSpan> {
    Some(RangeSpan { side, line, last })
}

// ----- the pure functions ---------------------------------------------------------

#[test]
fn range_of_unified_with_an_added_line_is_on_the_new_side() {
    let display = unified();
    // Context, removed, added, added: the removed line is not in the range.
    assert_eq!(
        items::range_of(&display, 1, 4, None, DiffMode::Unified),
        span(DiffSide::New, 1, 3)
    );
    // Dragged upward it is the same stretch.
    assert_eq!(
        items::range_of(&display, 4, 1, None, DiffMode::Unified),
        span(DiffSide::New, 1, 3)
    );
    // Starting on the removed line, which has no new side.
    assert_eq!(
        items::range_of(&display, 2, 4, None, DiffMode::Unified),
        span(DiffSide::New, 2, 3)
    );
}

#[test]
fn range_of_unified_with_removed_lines_only_is_on_the_old_side() {
    let display = unified();
    // Context and a removed line: the context line has an old number too, so
    // the whole stretch is in the range.
    assert_eq!(
        items::range_of(&display, 1, 2, None, DiffMode::Unified),
        span(DiffSide::Old, 1, 2)
    );
    // A lone removed line.
    assert_eq!(
        items::range_of(&display, 9, 9, None, DiffMode::Unified),
        span(DiffSide::Old, 10, 10)
    );
}

#[test]
fn range_of_unified_with_only_context_is_on_the_new_side() {
    let display = unified();
    assert_eq!(
        items::range_of(&display, 0, 1, None, DiffMode::Unified),
        span(DiffSide::New, 0, 1)
    );
    assert_eq!(
        items::range_of(&display, 5, 6, None, DiffMode::Unified),
        span(DiffSide::New, 4, 5)
    );
}

#[test]
fn range_of_stops_at_a_fold() {
    let display = unified();
    // The fold is row 7: the stretches are 0..7 and 8..11.
    assert_eq!(items::run_of(&display, 5), 0..7);
    assert_eq!(items::run_of(&display, 0), 0..7);
    assert_eq!(items::run_of(&display, 8), 8..11);
    assert_eq!(items::run_of(&display, 7), 7..7, "a fold starts nothing");
    assert_eq!(
        items::range_of(&display, 5, 8, None, DiffMode::Unified),
        None,
        "a range does not cross it"
    );
    assert_eq!(
        items::range_of(&display, 8, 10, None, DiffMode::Unified),
        span(DiffSide::Old, 9, 11),
        "after it: context, removed, context"
    );
}

#[test]
fn range_of_side_by_side_takes_the_pressed_half() {
    let display = split();
    // Row 2 is the pair 2:2, row 3 an added line, row 4 a context line.
    assert_eq!(
        items::range_of(&display, 2, 4, Some(DiffSide::Old), DiffMode::SideBySide),
        span(DiffSide::Old, 2, 3),
        "the left half: the lines the left side has"
    );
    assert_eq!(
        items::range_of(&display, 2, 4, Some(DiffSide::New), DiffMode::SideBySide),
        span(DiffSide::New, 2, 4)
    );
    // Pressed on the empty half of an added line: the other half.
    assert_eq!(
        items::range_of(&display, 3, 4, Some(DiffSide::Old), DiffMode::SideBySide),
        span(DiffSide::New, 3, 4)
    );
    // The pointer's horizontal place does not matter after the press: the
    // range is what the half and the rows make.
    assert_eq!(
        items::range_of(&display, 4, 2, Some(DiffSide::Old), DiffMode::SideBySide),
        span(DiffSide::Old, 2, 3)
    );
}

#[test]
fn range_of_a_row_that_is_gone_is_none() {
    let display = unified();
    assert_eq!(
        items::range_of(&display, 1, 99, None, DiffMode::Unified),
        None
    );
}

#[test]
fn excerpt_lines_leave_a_gap_and_mark_the_range() {
    let lines: Vec<Arc<str>> = ["a", "b", "c", "d"].into_iter().map(Arc::from).collect();
    // Lines 10, 11, then 5 left out, then 17, 18 (0-based); the note covers
    // 10 to 18.
    let excerpt = Excerpt {
        first: 10,
        lines: Arc::from(lines),
        gap: Some(ExcerptGap { at: 2, lines: 5 }),
    };
    assert_eq!(
        excerpt_lines(&excerpt, 10, 18),
        vec![
            ExcerptLine::Line {
                text: "a",
                own: true
            },
            ExcerptLine::Line {
                text: "b",
                own: true
            },
            ExcerptLine::Gap(5),
            ExcerptLine::Line {
                text: "c",
                own: true
            },
            ExcerptLine::Line {
                text: "d",
                own: true
            },
        ]
    );
    // The same excerpt for a note that ends before the last line: that line
    // is context.
    let own: Vec<bool> = excerpt_lines(&excerpt, 10, 17)
        .into_iter()
        .filter_map(|line| match line {
            ExcerptLine::Line { own, .. } => Some(own),
            ExcerptLine::Gap(_) => None,
        })
        .collect();
    assert_eq!(own, [true, true, true, false]);
    // No gap: just the lines, the first number as given.
    let plain = Excerpt {
        first: 3,
        lines: Arc::from(vec![Arc::<str>::from("x"), Arc::from("y")]),
        gap: None,
    };
    assert_eq!(
        excerpt_lines(&plain, 4, 4),
        vec![
            ExcerptLine::Line {
                text: "x",
                own: false
            },
            ExcerptLine::Line {
                text: "y",
                own: true
            },
        ]
    );
}

// ----- the items ----------------------------------------------------------------------

mod items_range {
    use std::collections::HashMap;

    use lectern_services::review::{FileRows, ReviewSnapshot};

    use super::*;
    use crate::items::{BuildOptions, EditorAt, Item};
    use crate::tests::notes::{noted_snapshot, words};

    fn rows_map() -> HashMap<RowsKey, Arc<FileRows>> {
        [step_key(), final_key()]
            .into_iter()
            .map(|key| (key.clone(), Arc::new(long_rows(key))))
            .collect()
    }

    fn built(snapshot: &ReviewSnapshot, editor: Option<EditorAt>) -> Vec<String> {
        let (context, known) = (HashMap::new(), HashMap::new());
        let options = BuildOptions {
            mode: DiffMode::Unified,
            earlier_collapsed: true,
            noise_expanded: false,
            context: &context,
            known_lines: &known,
            editor,
        };
        let all = words(&items::build(snapshot, &rows_map(), &options));
        let at = all.iter().position(|w| w == "step").expect("a card title");
        all[at..].to_vec()
    }

    fn ranged(id: u64, line: u32, last: u32) -> lectern_services::review::Note {
        let mut ranged = note(id, DiffSide::New, line, NoteState::Pending, on_step());
        ranged.anchor.last = last;
        ranged
    }

    #[test]
    fn items_notes_range_follow_their_last_line() {
        // New lines 1 to 3: the rows -:3 is the last one with a number; the
        // note and the dot come after it, not after the first line.
        let snapshot = noted_snapshot(vec![ranged(1, 1, 3)]);
        let all = built(&snapshot, None);
        let at = all.iter().position(|w| w == "note 1").expect("the note");
        assert_eq!(all[at - 1], "row -:3");
        assert_eq!(all[at + 1], "row 3:4");
        let one = noted_snapshot(vec![ranged(2, 1, 1)]);
        let all = built(&one, None);
        let at = all.iter().position(|w| w == "note 2").expect("the note");
        assert_eq!(all[at - 1], "row 1:1", "a note on one line is where it was");
    }

    #[test]
    fn items_notes_range_editor_follows_the_last_line() {
        let snapshot = noted_snapshot(Vec::new());
        let all = built(
            &snapshot,
            Some(EditorAt::Row {
                rows: step_key(),
                side: DiffSide::New,
                // `editor_at` gives the last line of a range.
                line: 3,
                oid: new_oid(),
            }),
        );
        let at = all.iter().position(|w| w == "editor").expect("the editor");
        assert_eq!(all[at - 1], "row -:3");
    }

    #[test]
    fn items_notes_range_in_a_fold_follow_the_fold() {
        // The last line (old side) is in the folded lines 5..9.
        let mut folded = ranged(1, 4, 7);
        folded.anchor.side = DiffSide::Old;
        folded.anchor.oid = old_oid();
        let snapshot = noted_snapshot(vec![folded]);
        let all = built(&snapshot, None);
        let fold = all.iter().position(|w| w == "fold 5").expect("the fold");
        assert_eq!(all[fold + 1], "note 1");
    }

    #[test]
    fn items_range_marks_the_row_of_the_last_line() {
        let snapshot = noted_snapshot(vec![ranged(1, 1, 3)]);
        let (context, known) = (HashMap::new(), HashMap::new());
        let options = BuildOptions {
            mode: DiffMode::Unified,
            earlier_collapsed: true,
            noise_expanded: false,
            context: &context,
            known_lines: &known,
            editor: None,
        };
        let marks: Vec<(Option<u32>, Option<u32>, u16)> =
            items::build(&snapshot, &rows_map(), &options)
                .iter()
                .filter_map(|item| match item {
                    Item::Row {
                        old, new, notes, ..
                    } if notes.count > 0 => Some((*old, *new, notes.count)),
                    _ => None,
                })
                .collect();
        assert_eq!(marks, [(None, Some(3), 1)], "one dot, at the last line");
    }
}

// ----- the screen ---------------------------------------------------------------------

/// The review open on `long_rows`, drawn.
fn open_long(cx: &mut TestAppContext) -> (Rig, Opened) {
    let rig = Rig::new(cx);
    rig.review.set_auto_prepare(true);
    for key in [step_key(), final_key()] {
        rig.review.set_rows(FIRST_ID, key.clone(), long_rows(key));
    }
    rig.review
        .set_text(FIRST_ID, crate::testing::prompt_key(0), "Make the change.");
    rig.review
        .set_text(FIRST_ID, crate::testing::agent_key(0), "Done.");
    let opened = rig.open(cx, crate::tests::screen::session(), Some(sample_snapshot()));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    (rig, opened)
}

fn row_id(lines: &str) -> String {
    format!("review.diff.item[step:0].row.item[{lines}]")
}

/// A point on a row of the last frame: `fraction` of its width from the
/// left, halfway down.
fn on(records: &[Record], lines: &str, fraction: f32) -> Point<gpui::Pixels> {
    let bounds = must(records, &row_id(lines)).bounds;
    point(
        bounds.origin.x + bounds.size.width * fraction,
        bounds.center().y,
    )
}

/// The code of a row, where a click only moves the current line.
fn code(records: &[Record], lines: &str) -> Point<gpui::Pixels> {
    on(records, lines, 0.5)
}

/// A line number of a row, where a click writes a note.
fn number(records: &[Record], lines: &str) -> Point<gpui::Pixels> {
    let bounds = must(records, &row_id(lines)).bounds;
    point(bounds.origin.x + px(12.0), bounds.center().y)
}

const LEFT: MouseButton = MouseButton::Left;

fn press(
    visual: &mut VisualTestContext,
    opened: &Opened,
    cx: &mut TestAppContext,
    at: Point<gpui::Pixels>,
) {
    visual.simulate_mouse_down(at, LEFT, Modifiers::default());
    // The tracker of the pointer is drawn from the next frame.
    opened.draw(cx);
}

fn drag(
    visual: &mut VisualTestContext,
    opened: &Opened,
    cx: &mut TestAppContext,
    to: Point<gpui::Pixels>,
) {
    visual.simulate_mouse_move(to, Some(LEFT), Modifiers::default());
    opened.draw(cx);
}

fn release(
    visual: &mut VisualTestContext,
    opened: &Opened,
    cx: &mut TestAppContext,
    at: Point<gpui::Pixels>,
) {
    visual.simulate_mouse_up(at, LEFT, Modifiers::default());
    opened.draw(cx);
}

/// A drag from the code of one row to the code of another, released there.
fn drag_rows(
    visual: &mut VisualTestContext,
    opened: &Opened,
    cx: &mut TestAppContext,
    from: &str,
    to: &str,
) {
    let records = opened.record(cx);
    let (start, end) = (code(&records, from), code(&records, to));
    press(visual, opened, cx, start);
    drag(visual, opened, cx, end);
    release(visual, opened, cx, end);
}

fn range_of_model(opened: &Opened, cx: &mut TestAppContext) -> Option<LineRange> {
    opened.read(cx, |model| model.range.clone())
}

fn in_range(records: &[Record]) -> Vec<String> {
    records
        .iter()
        .filter(|record| attr(record, "in_range").is_some())
        .map(|record| {
            record
                .semantic
                .id
                .as_str()
                .trim_start_matches("review.diff.item[step:0].row.item[")
                .trim_end_matches(']')
                .to_owned()
        })
        .collect()
}

fn expected(
    side: DiffSide,
    line: u32,
    last: u32,
    from: (Option<u32>, Option<u32>),
    at: (Option<u32>, Option<u32>),
) -> LineRange {
    LineRange {
        rows: step_key(),
        side,
        line,
        last,
        oid: match side {
            DiffSide::Old => old_oid(),
            DiffSide::New => new_oid(),
        },
        from,
        at,
        half: None,
    }
}

#[gpui::test]
fn range_drag_sets_a_range_and_shows_add_note(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    let records = opened.record(cx);
    let (start, end) = (code(&records, "1:1"), code(&records, "-:3"));

    // Down: nothing is selected, nothing opens.
    press(&mut visual, &opened, cx, start);
    assert!(opened.read(cx, |model| model.press.is_some()));
    assert_eq!(opened.read(cx, |model| model.selection.clone()), None);
    assert!(editor_target(&opened, cx).is_none());

    // Dragging: the rows are marked, there is no + and no Add Note.
    drag(&mut visual, &opened, cx, end);
    let records = opened.record(cx);
    assert_eq!(in_range(&records), ["1:1", "-:2", "-:3"]);
    assert!(
        find(&records, "review.range.add-note").is_none(),
        "not while dragging"
    );
    assert!(
        records
            .iter()
            .all(|record| !record.semantic.id.as_str().ends_with(".add-note")),
        "no + while dragging"
    );
    assert_eq!(opened.read(cx, |model| model.selection.clone()), None);

    // Up: the range is left, the row it ended on is the current line.
    release(&mut visual, &opened, cx, end);
    assert!(opened.read(cx, |model| model.press.is_none()));
    assert_eq!(
        range_of_model(&opened, cx),
        Some(expected(
            DiffSide::New,
            1,
            3,
            (Some(1), Some(1)),
            (None, Some(3))
        ))
    );
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(row(&step_key(), None, Some(3)))
    );
    assert!(editor_target(&opened, cx).is_none(), "Add Note opens it");
    let records = opened.record(cx);
    assert_eq!(in_range(&records), ["1:1", "-:2", "-:3"]);
    assert!(
        find(&records, &format!("{}.add-note", row_id("-:3"))).is_none(),
        "Add Note takes the place of the +"
    );
    let button = must(&records, "review.range.add-note");
    assert_eq!(attr(button, "side").as_deref(), Some("new"));
    assert_eq!(attr(button, "line").as_deref(), Some("1"));
    assert_eq!(attr(button, "last").as_deref(), Some("3"));
    // At the end of the row where the button went up; taller than the row.
    let released = must(&records, &row_id("-:3")).bounds;
    assert!((button.bounds.center().y - released.center().y).abs() < px(1.0));
    assert!(button.bounds.left() > released.center().x);
    assert!(button.bounds.right() <= released.right());
    assert!(button.bounds.size.height > released.size.height);

    // Dragged upward the button is on the row the button went up on, not at
    // the last line of the range.
    let (start, end) = (code(&records, "-:3"), code(&records, "1:1"));
    press(&mut visual, &opened, cx, start);
    assert!(
        find(&opened.record(cx), "review.range.add-note").is_none(),
        "a new press takes the old button away"
    );
    drag(&mut visual, &opened, cx, end);
    release(&mut visual, &opened, cx, end);
    assert_eq!(
        range_of_model(&opened, cx),
        Some(expected(
            DiffSide::New,
            1,
            3,
            (None, Some(3)),
            (Some(1), Some(1))
        ))
    );
    let records = opened.record(cx);
    let button = must(&records, "review.range.add-note");
    let released = must(&records, &row_id("1:1")).bounds;
    assert!((button.bounds.center().y - released.center().y).abs() < px(1.0));
}

#[gpui::test]
fn range_a_press_that_stays_on_a_row_is_a_click(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    let steps = step_key();
    let line_of = |target: Option<EditorTarget>| match target {
        Some(EditorTarget::New { anchor, .. }) => Some((anchor.side, anchor.line, anchor.last)),
        _ => None,
    };

    // The code: the current line moves, nothing opens, no range.
    let records = opened.record(cx);
    let at = code(&records, "-:2");
    press(&mut visual, &opened, cx, at);
    release(&mut visual, &opened, cx, at);
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(row(&steps, None, Some(2)))
    );
    assert!(editor_target(&opened, cx).is_none());
    assert_eq!(range_of_model(&opened, cx), None);
    assert!(opened.read(cx, |model| model.press.is_none()));

    // The line number: the editor opens on that one line.
    let records = opened.record(cx);
    let at = number(&records, "-:3");
    press(&mut visual, &opened, cx, at);
    release(&mut visual, &opened, cx, at);
    assert_eq!(
        line_of(editor_target(&opened, cx)),
        Some((DiffSide::New, 3, 3))
    );
    assert_eq!(range_of_model(&opened, cx), None);
    visual.dispatch_action(crate::actions::CancelNote);
    visual.run_until_parked();

    // Out and back: the button goes up on the row it went down on.
    let records = opened.record(cx);
    let (home, away) = (number(&records, "3:4"), code(&records, "-:2"));
    press(&mut visual, &opened, cx, home);
    drag(&mut visual, &opened, cx, away);
    assert!(
        opened
            .read(cx, |model| model.press.as_ref().map(|p| p.focus != p.start))
            .unwrap_or(false),
        "the focus left the row"
    );
    drag(&mut visual, &opened, cx, home);
    release(&mut visual, &opened, cx, home);
    assert_eq!(range_of_model(&opened, cx), None, "no range");
    assert_eq!(
        line_of(editor_target(&opened, cx)),
        Some((DiffSide::New, 4, 4)),
        "a click on the number"
    );
    visual.dispatch_action(crate::actions::CancelNote);
    visual.run_until_parked();

    // The same on the code: a click, the line is current.
    let records = opened.record(cx);
    let (home, away) = (code(&records, "4:5"), code(&records, "0:0"));
    press(&mut visual, &opened, cx, home);
    drag(&mut visual, &opened, cx, away);
    drag(&mut visual, &opened, cx, home);
    release(&mut visual, &opened, cx, home);
    assert_eq!(range_of_model(&opened, cx), None);
    assert!(editor_target(&opened, cx).is_none());
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(row(&steps, Some(4), Some(5)))
    );

    // A click quicker than a frame (no frame between the events) leaves no
    // press behind either.
    let records = opened.record(cx);
    visual.simulate_click(code(&records, "1:1"), Modifiers::default());
    assert!(opened.read(cx, |model| model.press.is_none()));
    assert_eq!(range_of_model(&opened, cx), None);
}

#[gpui::test]
fn range_add_note_and_c_open_the_editor(cx: &mut TestAppContext) {
    let (rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");

    // C writes on the range, whatever is selected.
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    let anchor = NoteAnchor {
        file: file_key(0),
        side: DiffSide::New,
        line: 1,
        last: 3,
        oid: new_oid(),
    };
    assert_eq!(
        editor_target(&opened, cx),
        Some(EditorTarget::New {
            rows: step_key(),
            anchor: anchor.clone()
        })
    );
    let records = opened.record(cx);
    let editor = must(&records, "review.note-editor");
    assert_eq!(attr(editor, "line").as_deref(), Some("1"));
    assert_eq!(attr(editor, "last").as_deref(), Some("3"));
    let lines = must(&records, "review.note-editor.lines");
    let label = lines.semantic.label.as_ref().expect("a label");
    assert_eq!(label.key, Some("review-note-lines"));
    assert_eq!(label.text.as_ref(), "Lines 2\u{2013}4");
    assert_eq!(in_range(&records), ["1:1", "-:2", "-:3"], "still marked");

    // Esc in the editor closes it and leaves the range; C brings the draft back.
    visual.simulate_input("half");
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(editor_target(&opened, cx).is_none());
    assert!(range_of_model(&opened, cx).is_some());
    visual.simulate_keystrokes("c");
    opened.draw(cx);
    assert_eq!(
        opened
            .model
            .read_with(cx, |model, app| model.editor.as_ref().map(|e| e
                .state
                .read(app)
                .text()
                .to_string())),
        Some("half".to_owned())
    );

    // Save: the command carries the range, and the range is over.
    visual.simulate_input(" done");
    visual.simulate_keystrokes("cmd-enter");
    visual.run_until_parked();
    match note_commands(&rig).last() {
        Some(ReviewCommand::AddNote { anchor: sent, .. }) => assert_eq!(*sent, anchor),
        other => panic!("not an AddNote: {other:?}"),
    }
    assert_eq!(range_of_model(&opened, cx), None);
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(row(&step_key(), None, Some(3))),
        "the current line stays"
    );
    let records = opened.record(cx);
    assert!(in_range(&records).is_empty());
    // The note is drawn after the last line, with its lines.
    let drawn = records
        .iter()
        .find(|record| record.semantic.id.as_str().starts_with("review.note.item["))
        .expect("the note");
    assert_eq!(attr(drawn, "last").as_deref(), Some("3"));

    // The button does the same as C.
    drag_rows(&mut visual, &opened, cx, "0:0", "-:2");
    let records = opened.record(cx);
    let button = must(&records, "review.range.add-note").bounds.center();
    visual.simulate_click(button, Modifiers::default());
    visual.run_until_parked();
    opened.draw(cx);
    assert_eq!(
        editor_target(&opened, cx),
        Some(EditorTarget::New {
            rows: step_key(),
            anchor: NoteAnchor {
                line: 0,
                last: 2,
                ..anchor
            }
        })
    );
}

#[gpui::test]
fn range_add_note_with_a_full_batch_shows_the_limit(cx: &mut TestAppContext) {
    let (rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    // As many unsent notes as there may be (the notes themselves are not
    // needed to be full).
    change(&rig, &opened, cx, |snapshot| {
        let mut state = (*snapshot.notes_state).clone();
        state.pending = limits::PENDING_NOTES;
        state.unsent = limits::PENDING_NOTES;
        snapshot.notes_state = Arc::new(state);
    });
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    visual.simulate_keystrokes("c");
    assert!(editor_target(&opened, cx).is_none());
    let records = opened.record(cx);
    assert_eq!(status(&records).0, Some("review-notes-full"));
    assert!(range_of_model(&opened, cx).is_some(), "the range stays");

    // The button says the same.
    opened.update(cx, |model, _| model.notes_full_hint = false);
    opened.draw(cx);
    let records = opened.record(cx);
    let button = must(&records, "review.range.add-note").bounds.center();
    visual.simulate_click(button, Modifiers::default());
    visual.run_until_parked();
    let records = opened.record(cx);
    assert_eq!(status(&records).0, Some("review-notes-full"));
}

#[gpui::test]
fn range_clears(cx: &mut TestAppContext) {
    let (rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    let steps = step_key();

    // Esc: the range goes first, the current line stays; a second Esc
    // clears the current line.
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    visual.simulate_keystrokes("escape");
    assert_eq!(range_of_model(&opened, cx), None);
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(row(&steps, None, Some(3)))
    );
    visual.simulate_keystrokes("escape");
    assert_eq!(opened.read(cx, |model| model.selection.clone()), None);

    // Esc during the drag ends it.
    let records = opened.record(cx);
    press(&mut visual, &opened, cx, code(&records, "1:1"));
    drag(&mut visual, &opened, cx, code(&records, "-:3"));
    visual.simulate_keystrokes("escape");
    assert!(opened.read(cx, |model| model.press.is_none()));
    assert_eq!(range_of_model(&opened, cx), None);
    release(&mut visual, &opened, cx, code(&records, "-:3"));
    assert_eq!(range_of_model(&opened, cx), None, "no range after the Esc");

    // A click on another row.
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    let records = opened.record(cx);
    visual.simulate_click(code(&records, "0:0"), Modifiers::default());
    assert_eq!(range_of_model(&opened, cx), None);
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(row(&steps, Some(0), Some(0)))
    );

    // J and K.
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    visual.simulate_keystrokes("j");
    assert_eq!(range_of_model(&opened, cx), None);
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    visual.simulate_keystrokes("k");
    assert_eq!(range_of_model(&opened, cx), None);

    // Unified to side by side.
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    visual.dispatch_action(ToggleDiffMode);
    visual.run_until_parked();
    opened.draw(cx);
    assert_eq!(range_of_model(&opened, cx), None);
    visual.dispatch_action(ToggleDiffMode);
    visual.run_until_parked();
    opened.draw(cx);

    // A snapshot that leaves the content as it is: the range stays.
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    change(&rig, &opened, cx, |_| {});
    assert!(range_of_model(&opened, cx).is_some());

    // New rows with another new side: the range is over.
    let mut changed = long_rows(step_key());
    changed.new.oid = Some(lectern_services::project::BlobId::Sha1([9; 20]));
    rig.review.set_rows(FIRST_ID, step_key(), changed);
    rig.review.emit_prepared(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);
    assert_eq!(range_of_model(&opened, cx), None);
    // (The same rows the other way, for the next case.)
    let mut back = long_rows(step_key());
    back.new.oid = Some(new_oid());
    rig.review.set_rows(FIRST_ID, step_key(), back);
    rig.review.emit_prepared(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);

    // A change of content during the press ends the press.
    let records = opened.record(cx);
    press(&mut visual, &opened, cx, code(&records, "1:1"));
    let mut changed = long_rows(step_key());
    changed.new.oid = Some(lectern_services::project::BlobId::Sha1([8; 20]));
    rig.review.set_rows(FIRST_ID, step_key(), changed);
    rig.review.emit_prepared(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);
    assert!(opened.read(cx, |model| model.press.is_none()));
}

#[gpui::test]
fn range_plus_and_line_numbers_stay_one_line(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    let one_line = |opened: &Opened, cx: &mut TestAppContext| {
        opened.read(cx, |model| match model.editor.as_ref().map(|e| &e.target) {
            Some(EditorTarget::New { anchor, .. }) => Some((anchor.line, anchor.last)),
            _ => None,
        })
    };

    // The + of a row under the pointer.
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    let records = opened.record(cx);
    visual.simulate_mouse_move(code(&records, "0:0"), None, Modifiers::default());
    let records = opened.record(cx);
    let plus = must(&records, &format!("{}.add-note", row_id("0:0")))
        .bounds
        .center();
    visual.simulate_click(plus, Modifiers::default());
    visual.run_until_parked();
    opened.draw(cx);
    assert_eq!(one_line(&opened, cx), Some((0, 0)));
    assert_eq!(range_of_model(&opened, cx), None);
    visual.dispatch_action(crate::actions::CancelNote);
    visual.run_until_parked();

    // The number of another row.
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    let records = opened.record(cx);
    visual.simulate_click(number(&records, "3:4"), Modifiers::default());
    visual.run_until_parked();
    opened.draw(cx);
    assert_eq!(one_line(&opened, cx), Some((4, 4)));
    assert_eq!(range_of_model(&opened, cx), None);
}

#[gpui::test]
fn range_extend_actions(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    let steps = step_key();
    let state = |opened: &Opened, cx: &mut TestAppContext| {
        range_of_model(opened, cx).map(|r| (r.side, r.line, r.last, r.at))
    };

    // Without a current line nothing happens.
    visual.dispatch_action(ExtendRangeDown);
    assert_eq!(state(&opened, cx), None);

    // Down from row 1:1: the same ranges a drag makes row by row.
    select(&opened, cx, row(&steps, Some(1), Some(1)));
    let mut seen = Vec::new();
    for _ in 0..6 {
        visual.dispatch_action(ExtendRangeDown);
        seen.push(state(&opened, cx));
    }
    assert_eq!(
        seen,
        [
            Some((DiffSide::Old, 1, 2, (Some(2), None))),
            Some((DiffSide::New, 1, 2, (None, Some(2)))),
            Some((DiffSide::New, 1, 3, (None, Some(3)))),
            Some((DiffSide::New, 1, 4, (Some(3), Some(4)))),
            Some((DiffSide::New, 1, 5, (Some(4), Some(5)))),
            // The fold stops it.
            Some((DiffSide::New, 1, 5, (Some(4), Some(5)))),
        ]
    );
    // The same as the drag from 1:1 to -:3.
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    let dragged = range_of_model(&opened, cx).expect("a range");
    select(&opened, cx, row(&steps, Some(1), Some(1)));
    for _ in 0..3 {
        visual.dispatch_action(ExtendRangeDown);
    }
    let extended = range_of_model(&opened, cx).expect("a range");
    assert_eq!(
        (dragged.side, dragged.line, dragged.last, dragged.at),
        (extended.side, extended.line, extended.last, extended.at)
    );
    // The other way: one row longer at the top, then the edge of the diff.
    // (The end of the range moves, as the focus of a drag does: Down after Up
    // makes the range shorter.)
    select(&opened, cx, row(&steps, Some(1), Some(1)));
    visual.dispatch_action(ExtendRangeUp);
    assert_eq!(
        state(&opened, cx),
        Some((DiffSide::New, 0, 1, (Some(0), Some(0))))
    );
    visual.dispatch_action(ExtendRangeUp);
    assert_eq!(
        state(&opened, cx),
        Some((DiffSide::New, 0, 1, (Some(0), Some(0)))),
        "the first row of the diff"
    );
    visual.dispatch_action(ExtendRangeDown);
    assert_eq!(
        state(&opened, cx),
        Some((DiffSide::New, 1, 1, (Some(1), Some(1)))),
        "back where it began: the one row it began on"
    );
}

#[gpui::test]
fn range_side_by_side_takes_the_pressed_half(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    visual.dispatch_action(ToggleDiffMode);
    visual.run_until_parked();
    opened.draw(cx);
    let records = opened.record(cx);
    // The row 2:2 holds the removed line and its added partner.
    let left = on(&records, "2:2", 0.25);
    let end = code(&records, "3:4");

    press(&mut visual, &opened, cx, left);
    drag(&mut visual, &opened, cx, end);
    release(&mut visual, &opened, cx, end);
    let range = range_of_model(&opened, cx).expect("a range");
    // Rows 2:2, -:3, 3:4: the left side has the lines 2 and 3.
    assert_eq!((range.side, range.line, range.last), (DiffSide::Old, 2, 3));
    assert_eq!(range.oid, old_oid());
    assert_eq!(range.half, Some(DiffSide::Old));

    let records = opened.record(cx);
    let (right, end) = (on(&records, "2:2", 0.75), code(&records, "3:4"));
    press(&mut visual, &opened, cx, right);
    drag(&mut visual, &opened, cx, end);
    release(&mut visual, &opened, cx, end);
    let range = range_of_model(&opened, cx).expect("a range");
    assert_eq!((range.side, range.line, range.last), (DiffSide::New, 2, 4));
    assert_eq!(range.oid, new_oid());

    // An empty half: the added line has nothing on the left.
    let records = opened.record(cx);
    let (empty, end) = (on(&records, "-:3", 0.25), code(&records, "3:4"));
    press(&mut visual, &opened, cx, empty);
    drag(&mut visual, &opened, cx, end);
    release(&mut visual, &opened, cx, end);
    let range = range_of_model(&opened, cx).expect("a range");
    assert_eq!((range.side, range.line, range.last), (DiffSide::New, 3, 4));
}

#[gpui::test]
fn range_stays_in_the_rows_before_a_fold(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    // From 3:4 over the fold to 9:10: the range ends at the last row before it.
    drag_rows(&mut visual, &opened, cx, "3:4", "9:10");
    let range = range_of_model(&opened, cx).expect("a range");
    assert_eq!((range.side, range.line, range.last), (DiffSide::New, 4, 5));
    assert_eq!(range.at, (Some(4), Some(5)));
}

#[gpui::test]
fn range_needs_a_side_with_content(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.review.set_auto_prepare(true);
    for key in [step_key(), final_key()] {
        let mut rows = long_rows(key.clone());
        rows.new.oid = None;
        rows.old.oid = None;
        rig.review.set_rows(FIRST_ID, key, rows);
    }
    let opened = rig.open(cx, crate::tests::screen::session(), Some(sample_snapshot()));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    let mut visual = opened.visual(cx);
    drag_rows(&mut visual, &opened, cx, "1:1", "-:3");
    assert_eq!(range_of_model(&opened, cx), None);
    assert!(opened.read(cx, |model| model.press.is_none()));
}

#[gpui::test]
fn range_notes_are_drawn(cx: &mut TestAppContext) {
    let (rig, opened) = open_long(cx);
    let mut ranged = note(1, DiffSide::New, 1, NoteState::Pending, on_step());
    ranged.anchor.last = 3;
    // A range whose content changed since: it follows the file title, with
    // its excerpt and the lines it left out.
    let mut drifted = note(
        2,
        DiffSide::New,
        20,
        NoteState::Pending,
        NotePlace::Drifted { file: file_key(0) },
    );
    drifted.anchor.last = 40;
    drifted.excerpt = Some(Excerpt {
        first: 20,
        lines: Arc::from(vec![Arc::<str>::from("a"), Arc::from("b"), Arc::from("c")]),
        gap: Some(ExcerptGap { at: 2, lines: 17 }),
    });
    change(&rig, &opened, cx, |snapshot| {
        set_notes(snapshot, vec![ranged, drifted]);
    });
    let records = opened.record(cx);
    // Under the last line, the dot on it, the lines before the state.
    let drawn = must(&records, "review.note.item[0000000000000001]");
    assert_eq!(attr(drawn, "last").as_deref(), Some("3"));
    let last_row = must(&records, &row_id("-:3"));
    assert_eq!(attr(last_row, "notes").as_deref(), Some("1"));
    assert!(drawn.bounds.top() >= last_row.bounds.bottom() - px(1.0));
    assert_eq!(
        must(&records, "review.note.item[0000000000000001].lines")
            .semantic
            .label
            .as_ref()
            .map(|label| label.text.to_string()),
        Some("Lines 2\u{2013}4".to_owned())
    );
    for first in ["1:1", "-:2", "0:0"] {
        assert_eq!(attr(must(&records, &row_id(first)), "notes"), None);
    }
    assert!(in_range(&records).is_empty(), "not selected: not marked");
    let drifted = must(&records, "review.note.item[0000000000000002]");
    assert_eq!(attr(drifted, "place").as_deref(), Some("drifted"));
    assert_eq!(attr(drifted, "last").as_deref(), Some("40"));

    // Selected: its rows are marked.
    select(
        &opened,
        cx,
        ItemKey::Note(lectern_services::review::NoteId(1)),
    );
    let records = opened.record(cx);
    assert_eq!(in_range(&records), ["1:1", "-:2", "-:3"]);
}

#[gpui::test]
fn range_autoscroll_follows_the_pointer(cx: &mut TestAppContext) {
    let (_rig, opened, _) = crate::tests::support::ready(cx, 1, 1, 300);
    let mut visual = opened.visual(cx);
    let (rows, ix) = opened
        .read(cx, |model| {
            model.items.iter().find_map(|item| match item {
                items::Item::Row { rows, ix, .. } => Some((rows.clone(), *ix)),
                _ => None,
            })
        })
        .expect("a row");
    let viewport = opened.read(cx, |model| model.list.viewport_bounds());
    let x = viewport.origin.x + viewport.size.width / 2.0;
    opened.update(cx, |model, cx| model.begin_press(rows, ix, None, cx));
    opened.draw(cx);

    // The pointer in the middle: the list stays where it is.
    visual.simulate_mouse_move(
        point(x, viewport.center().y),
        Some(LEFT),
        Modifiers::default(),
    );
    opened.draw(cx);
    let begin = Instant::now();
    let tick = |visual: &mut VisualTestContext, at: Instant| {
        let model = opened.model.clone();
        visual.update(|window, cx| model.update(cx, |model, cx| model.tick_drag(window, at, cx)));
    };
    let top = |cx: &mut TestAppContext| {
        opened.read(cx, |model| {
            let top = model.list.logical_scroll_top();
            (top.item_ix, top.offset_in_item)
        })
    };
    let before = top(cx);
    tick(&mut visual, begin);
    tick(&mut visual, begin + Duration::from_micros(100_000));
    assert_eq!(top(cx), before, "not near an edge");

    // Just above the floating bar: it scrolls down.
    visual.simulate_mouse_move(
        point(x, viewport.bottom() - crate::screen::BAR_RESERVE - px(4.0)),
        Some(LEFT),
        Modifiers::default(),
    );
    tick(&mut visual, begin + Duration::from_micros(120_000));
    tick(&mut visual, begin + Duration::from_micros(190_000));
    let above_bar = top(cx);
    assert!(
        above_bar > before,
        "above the bar: {before:?} to {above_bar:?}"
    );

    // Below the list's bottom edge: it scrolls down, by time.
    visual.simulate_mouse_move(
        point(x, viewport.bottom() + px(40.0)),
        Some(LEFT),
        Modifiers::default(),
    );
    // (A frame the test platform drew may have been the first one in the
    // zone; the ticks below are 100 ms apart either way.)
    tick(&mut visual, begin + Duration::from_micros(200_000));
    tick(&mut visual, begin + Duration::from_micros(300_000));
    let down = top(cx);
    assert!(down > above_bar, "scrolled down: {above_bar:?} to {down:?}");
    tick(&mut visual, begin + Duration::from_micros(400_000));
    tick(&mut visual, begin + Duration::from_micros(500_000));
    assert!(top(cx) > down, "and on");
    // The focus follows the pointer to the rows that come in view (a frame
    // later the list has laid them out).
    opened.draw(cx);
    let focus = opened.read(cx, |model| model.press.as_ref().map(|press| press.focus));
    assert!(focus.is_some_and(|focus| focus > ix));

    // Above the top: it scrolls up.
    let scrolled = top(cx);
    visual.simulate_mouse_move(
        point(x, viewport.top() - px(40.0)),
        Some(LEFT),
        Modifiers::default(),
    );
    tick(&mut visual, begin + Duration::from_micros(600_000));
    assert!(top(cx) < scrolled, "scrolled up");

    // A late frame counts for no more than 50 ms.
    let before = top(cx);
    tick(&mut visual, begin + Duration::from_micros(60_000_000));
    let step = top(cx);
    assert_ne!(step, before);
    assert!(step.0 + 100 > before.0, "no screens thrown");
}

#[gpui::test]
fn range_an_unseen_release_cancels_the_press(cx: &mut TestAppContext) {
    let (_rig, opened) = open_long(cx);
    let mut visual = opened.visual(cx);
    let records = opened.record(cx);
    press(&mut visual, &opened, cx, code(&records, "1:1"));
    // The next move says no button is down (it went up outside the window).
    visual.simulate_mouse_move(code(&records, "-:3"), None, Modifiers::default());
    assert!(opened.read(cx, |model| model.press.is_none()));
    assert_eq!(range_of_model(&opened, cx), None);
}
