//! `refresh_*` and the states of docs/design/terminal_view.md「状态与异常」:
//! the pane redraws when the frame's generation or the run state changed,
//! never calls `watch`, and keeps its last frame when the service is gone.

use std::cell::Cell;
use std::rc::Rc;

use gpui::TestAppContext;
use lectern_services::terminal::{Input, RunState, Terminals as _};

use crate::colors::color_of;
use crate::paint::Op;
use crate::testing::frame;
use crate::tests::support::{ID, Opened, Rig};

/// Counts the notifications of the pane.
fn count_notifications(
    opened: &Opened,
    cx: &mut TestAppContext,
) -> (Rc<Cell<usize>>, gpui::Subscription) {
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    let subscription =
        cx.update(|cx| cx.observe(&opened.pane, move |_, _| seen.set(seen.get() + 1)));
    (count, subscription)
}

fn glyphs(ops: &[Op]) -> usize {
    ops.iter()
        .filter(|op| matches!(op, Op::Glyph { .. }))
        .count()
}

#[gpui::test]
fn refresh_notifies_for_a_new_generation_only(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["one"]).generation(1).build()));
    assert_eq!(glyphs(&opened.painted(cx)), 3);
    let (count, _subscription) = count_notifications(&opened, cx);

    // The same generation: nothing.
    opened.pane.update(cx, |pane, cx| pane.refresh(cx));
    assert_eq!(count.get(), 0);

    // A new generation: one notification and the new frame is drawn.
    rig.terminals
        .set_frame(ID, frame(&["three", "more"]).generation(2).build());
    opened.pane.update(cx, |pane, cx| pane.refresh(cx));
    assert_eq!(count.get(), 1);
    opened.draw(cx);
    assert_eq!(glyphs(&opened.painted(cx)), 9);
    opened.pane.update(cx, |pane, cx| pane.refresh(cx));
    assert_eq!(count.get(), 1, "the same generation again");
}

#[gpui::test]
fn refresh_notifies_for_a_new_run_state(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["one"]).build()));
    let (count, _subscription) = count_notifications(&opened, cx);
    rig.terminals.set_state(
        ID,
        RunState::Exited {
            code: 0,
            signaled: false,
        },
    );
    opened.pane.update(cx, |pane, cx| pane.refresh(cx));
    assert_eq!(count.get(), 1);
    opened.pane.update(cx, |pane, cx| pane.refresh(cx));
    assert_eq!(count.get(), 1);
}

#[gpui::test]
fn the_pane_never_calls_watch(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["one"]).build()));
    rig.terminals
        .set_frame(ID, frame(&["two"]).generation(2).build());
    opened.refresh(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("a");
    visual.simulate_keystrokes("cmd-a");
    assert_eq!(rig.terminals.watch_calls(), 0);
}

/// No frame yet: only the background is drawn.
#[gpui::test]
fn refresh_before_the_first_frame_draws_the_background(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, None);
    let ops = opened.painted(cx);
    let theme_background = cx.update(|cx| {
        color_of(crate::theme_of(lectern_ui::palette(lectern_ui::Appearance::Light, cx)).background)
    });
    assert!(
        matches!(ops.as_slice(), [Op::Fill { color, .. }] if *color == theme_background),
        "{ops:?}"
    );
    // The first frame arrives.
    rig.terminals.set_frame(ID, frame(&["hi"]).build());
    opened.refresh(cx);
    assert_eq!(glyphs(&opened.painted(cx)), 2);
}

/// The terminal is closed (`frame(id)` is None): only the background; the
/// host drops the pane next.
#[gpui::test]
fn refresh_after_close_draws_the_background(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["one"]).build()));
    rig.terminals.close(ID);
    opened.refresh(cx);
    let ops = opened.painted(cx);
    assert!(matches!(ops.as_slice(), [Op::Fill { .. }]), "{ops:?}");
}

/// `dyn Terminals` is gone (the plugin was deactivated): the last frame
/// stays and nothing is sent.
#[gpui::test]
fn refresh_without_the_service_keeps_the_last_frame(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["one"]).build()));
    rig.inputs();
    let kernel = rig.kernel_handle().expect("the kernel runs");
    kernel.set_enabled("mock-0-terminal", false);
    kernel.barrier();

    rig.terminals
        .set_frame(ID, frame(&["changed"]).generation(2).build());
    opened.refresh(cx);
    assert_eq!(glyphs(&opened.painted(cx)), 3, "still the last frame");
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("enter");
    visual.simulate_keystrokes("cmd-v");
    assert_eq!(rig.inputs(), Vec::<Input>::new());
}

#[gpui::test]
fn exited_and_failed_terminals_keep_their_picture(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["last words"]).build()));
    for state in [
        RunState::Exited {
            code: 1,
            signaled: false,
        },
        RunState::Failed(lectern_services::terminal::FailReason::Internal),
        RunState::Starting,
    ] {
        rig.terminals.set_state(ID, state);
        opened.refresh(cx);
        assert_eq!(glyphs(&opened.painted(cx)), 9, "{state:?}");
    }
}
