//! `ime_*` (docs/design/terminal_view.md「键盘」第 3 步): the input method's
//! text reaches the terminal as `Text`; a composition is drawn at the cursor
//! cell and sent to nobody; the candidate window is placed at the cursor.

use std::sync::Arc;

use gpui::{EntityInputHandler, Point, TestAppContext};
use lectern_services::terminal::{CursorShape, Input};

use crate::paint::Op;
use crate::testing::frame;
use crate::tests::support::Rig;

const CELL_WIDTH: f32 = 7.2;
const LINE_HEIGHT: f32 = 16.0;

#[gpui::test]
fn ime_commit_sends_text(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    rig.inputs();
    let mut visual = opened.visual(cx);
    visual.simulate_input("你好");
    assert_eq!(
        rig.inputs(),
        vec![Input::Text(Arc::from("你")), Input::Text(Arc::from("好"))]
    );
}

#[gpui::test]
fn ime_composition_is_not_sent_and_is_drawn_at_the_cursor(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(
        cx,
        Some(frame(&["$ ab"]).cursor(4, 0, CursorShape::Block).build()),
    );
    rig.inputs();
    let mut visual = opened.visual(cx);
    opened.pane.update_in(&mut visual, |pane, window, cx| {
        pane.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
        assert_eq!(pane.marked_text_range(window, cx), Some(0..2));
        let selection = pane.selected_text_range(false, window, cx).unwrap();
        assert_eq!(selection.range, 2..2);
    });
    opened.draw(cx);
    assert_eq!(
        rig.inputs(),
        Vec::<Input>::new(),
        "a composition is not sent"
    );

    // The composition is drawn from the cursor cell: a background, the glyphs
    // of "ni", and an underline.
    let origin = opened.pane.read_with(cx, |pane, _| pane.grid.origin);
    let left = origin.x.as_f32() + 4.0 * CELL_WIDTH;
    let ops = opened.painted(cx);
    let marked_glyphs: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Glyph { origin, glyph, .. } if origin.x.as_f32() >= left - 0.01 => {
                Some((origin.x.as_f32(), glyph.0))
            }
            _ => None,
        })
        .collect();
    assert_eq!(marked_glyphs.len(), 2, "{marked_glyphs:?}");
    assert!((marked_glyphs[0].0 - left).abs() < 0.01);
    assert!((marked_glyphs[1].0 - (left + CELL_WIDTH)).abs() < 0.01);
    let background = ops.iter().find(|op| match op {
        Op::Fill { bounds, .. } => {
            (bounds.origin.x.as_f32() - left).abs() < 0.01
                && (bounds.size.width.as_f32() - 2.0 * CELL_WIDTH).abs() < 0.01
                && (bounds.size.height.as_f32() - LINE_HEIGHT).abs() < 0.01
        }
        _ => false,
    });
    assert!(background.is_some(), "the background of the composition");
    let underline = ops.iter().find(|op| match op {
        Op::Fill { bounds, .. } => {
            (bounds.origin.x.as_f32() - left).abs() < 0.01
                && (bounds.size.width.as_f32() - 2.0 * CELL_WIDTH).abs() < 0.01
                && bounds.size.height.as_f32() < 1.0
        }
        _ => false,
    });
    assert!(underline.is_some(), "the underline of the composition");

    // Committing sends one `Text` and ends the composition.
    opened.pane.update_in(&mut visual, |pane, window, cx| {
        pane.replace_text_in_range(None, "你", window, cx);
        assert_eq!(pane.marked_text_range(window, cx), None);
    });
    opened.draw(cx);
    assert_eq!(rig.inputs(), vec![Input::Text(Arc::from("你"))]);
    let after = opened.painted(cx);
    assert!(
        after.iter().all(|op| !matches!(
            op,
            Op::Glyph { origin, .. } if origin.x.as_f32() >= left - 0.01
        )),
        "no composition left"
    );
}

#[gpui::test]
fn ime_composition_wider_than_the_grid_is_clipped(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(
        cx,
        Some(
            frame(&["$"])
                .size(20, 1)
                .cursor(18, 0, CursorShape::Block)
                .build(),
        ),
    );
    let mut visual = opened.visual(cx);
    opened.pane.update_in(&mut visual, |pane, window, cx| {
        pane.replace_and_mark_text_in_range(None, "wxyz", None, window, cx);
    });
    opened.draw(cx);
    let (origin, size) = opened
        .pane
        .read_with(cx, |pane, _| (pane.grid.origin, pane.grid.size));
    let right = origin.x.as_f32() + size.width.as_f32();
    let background = opened
        .painted(cx)
        .into_iter()
        .filter_map(|op| match op {
            Op::Fill { bounds, .. }
                if (bounds.origin.x.as_f32() - (origin.x.as_f32() + 18.0 * CELL_WIDTH)).abs()
                    < 0.01 =>
            {
                Some(bounds)
            }
            _ => None,
        })
        .next()
        .expect("the background of the composition");
    assert!(
        background.origin.x.as_f32() + background.size.width.as_f32() <= right + 0.01,
        "cut at the right edge of the grid"
    );
}

#[gpui::test]
fn ime_cancelled_composition_sends_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    rig.inputs();
    let mut visual = opened.visual(cx);
    opened.pane.update_in(&mut visual, |pane, window, cx| {
        pane.replace_and_mark_text_in_range(None, "ni", None, window, cx);
        pane.unmark_text(window, cx);
        assert_eq!(pane.marked_text_range(window, cx), None);
        pane.replace_and_mark_text_in_range(None, "ni", None, window, cx);
        // An empty composition ends it.
        pane.replace_and_mark_text_in_range(None, "", None, window, cx);
        assert_eq!(pane.marked_text_range(window, cx), None);
    });
    assert_eq!(rig.inputs(), Vec::<Input>::new());
}

#[gpui::test]
fn ime_bounds_for_range_is_the_cursor_cell(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(
        cx,
        Some(
            frame(&["$ ", "", "abc"])
                .cursor(3, 2, CursorShape::Bar)
                .build(),
        ),
    );
    let mut visual = opened.visual(cx);
    let origin = opened.pane.read_with(cx, |pane, _| pane.grid.origin);
    let bounds = opened.pane.update_in(&mut visual, |pane, window, cx| {
        let element = gpui::Bounds::default();
        // Nothing of the terminal's content is given to the input method.
        assert_eq!(pane.text_for_range(0..3, &mut None, window, cx), None);
        assert_eq!(
            pane.character_index_for_point(Point::default(), window, cx),
            None
        );
        pane.bounds_for_range(0..0, element, window, cx)
    });
    let bounds = bounds.expect("the cursor cell");
    assert!((bounds.origin.x.as_f32() - (origin.x.as_f32() + 3.0 * CELL_WIDTH)).abs() < 0.01);
    assert!((bounds.origin.y.as_f32() - (origin.y.as_f32() + 2.0 * LINE_HEIGHT)).abs() < 0.01);
    assert!((bounds.size.width.as_f32() - CELL_WIDTH).abs() < 0.01);
    assert!((bounds.size.height.as_f32() - LINE_HEIGHT).abs() < 0.01);
}
