//! `focus_*` (docs/design/terminal_view.md「键盘」「绘制的顺序」): gaining and
//! losing the keyboard focus (or the window's activation) is one `Focus`
//! each; without focus the cursor is a hollow box and the selection has the
//! unemphasized colour.

use gpui::TestAppContext;
use lectern_services::terminal::{CursorShape, Input};

use crate::colors::color_of;
use crate::paint::Op;
use crate::testing::{CURSOR, frame};
use crate::tests::support::Rig;

fn cursor_frame() -> std::sync::Arc<lectern_services::terminal::Frame> {
    frame(&["$ ab"])
        .cursor(2, 0, CursorShape::Block)
        .selection(0, 0, 2)
        .build()
}

fn focus_inputs(inputs: Vec<Input>) -> Vec<bool> {
    inputs
        .into_iter()
        .filter_map(|input| match input {
            Input::Focus(on) => Some(on),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn focus_gained_and_lost_is_one_input_each(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(cursor_frame()));
    assert_eq!(
        focus_inputs(rig.inputs()),
        vec![true],
        "focused when opened"
    );

    let mut visual = opened.visual(cx);
    visual.update(|window, cx| window.blur(cx));
    opened.draw(cx);
    assert_eq!(focus_inputs(rig.inputs()), vec![false]);

    opened.draw(cx);
    assert_eq!(focus_inputs(rig.inputs()), Vec::<bool>::new(), "no repeat");

    let handle = opened.pane.read_with(cx, |pane, _| pane.focus_handle());
    visual.update(|window, cx| handle.focus(window, cx));
    opened.draw(cx);
    assert_eq!(focus_inputs(rig.inputs()), vec![true]);
}

/// A window that is no longer the active one counts as unfocused.
#[gpui::test]
fn focus_follows_the_activation_of_the_window(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(cursor_frame()));
    rig.inputs();
    let mut visual = opened.visual(cx);
    visual.deactivate_window();
    opened.draw(cx);
    assert_eq!(focus_inputs(rig.inputs()), vec![false]);
}

#[gpui::test]
fn focus_hollow_cursor_without_focus(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(cursor_frame()));
    let cursor = color_of(CURSOR);

    // With focus: the block is a filled cell; there is no outline.
    let focused = opened.painted(cx);
    assert!(
        focused
            .iter()
            .any(|op| matches!(op, Op::Fill { color, .. } if *color == cursor)),
        "a filled block cursor"
    );
    assert!(!focused.iter().any(|op| matches!(op, Op::Outline { .. })));

    let mut visual = opened.visual(cx);
    visual.update(|window, cx| window.blur(cx));
    opened.draw(cx);
    let blurred = opened.painted(cx);
    assert!(
        !blurred
            .iter()
            .any(|op| matches!(op, Op::Fill { color, .. } if *color == cursor)),
        "no filled block without focus"
    );
    let outlines: Vec<_> = blurred
        .iter()
        .filter(|op| matches!(op, Op::Outline { .. }))
        .collect();
    assert_eq!(outlines.len(), 1, "one hollow box: {outlines:?}");
    let Op::Outline { color, width, .. } = outlines[0] else {
        unreachable!()
    };
    assert_eq!(*color, cursor);
    assert_eq!(width.as_f32(), 0.5);
}

/// The hollow box is the shape of every cursor without focus.
#[gpui::test]
fn focus_hollow_cursor_for_every_shape(cx: &mut TestAppContext) {
    for shape in [CursorShape::Block, CursorShape::Bar, CursorShape::Underline] {
        let rig = Rig::new(cx);
        let opened = rig.open(cx, Some(frame(&["$"]).cursor(1, 0, shape).build()));
        let mut visual = opened.visual(cx);
        visual.update(|window, cx| window.blur(cx));
        opened.draw(cx);
        let outlines = opened
            .painted(cx)
            .iter()
            .filter(|op| matches!(op, Op::Outline { .. }))
            .count();
        assert_eq!(outlines, 1, "{shape:?}");
    }
}

#[gpui::test]
fn focus_selection_colour(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(cursor_frame()));
    let (emphasized, unemphasized) = cx.update(|cx| {
        let palette = lectern_ui::palette(lectern_ui::Appearance::Light, cx);
        (
            palette.selected_content_background,
            palette.unemphasized_selected_content_background,
        )
    });
    let has = |ops: &[Op], wanted| {
        ops.iter()
            .any(|op| matches!(op, Op::Fill { color, .. } if *color == wanted))
    };
    assert!(has(&opened.painted(cx), emphasized));
    let mut visual = opened.visual(cx);
    visual.update(|window, cx| window.blur(cx));
    opened.draw(cx);
    let blurred = opened.painted(cx);
    assert!(has(&blurred, unemphasized));
    assert!(!has(&blurred, emphasized));
}
