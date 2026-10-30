//! The multi-line text box (docs/design/ui.md「测试」): typing, ⌘↩, Esc
//! (also while an input method composes), the line range and the
//! placeholder. Headless: a `TestAppContext`, no real window. The height
//! test arms the process-wide recorder, so these run under nextest (a test
//! per process) like the probe tests.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, EntityInputHandler as _, InteractiveElement, IntoElement,
    ParentElement, Pixels, Render, Styled, TestAppContext, VisualTestContext, Window, div, px,
};
use gpui_component::input::Escape;
use lectern_i18n::{Locale, Strings};

use crate::controls::engine;
use crate::semantic::{NodeId, Recorder};
use crate::{Appearance, Text, TextArea, TextAreaState, init_fixed};

fn verbatim(text: &str) -> Text {
    Text::verbatim(Arc::<str>::from(text))
}

fn area_id() -> NodeId {
    NodeId::new("probe", "area", None)
}

/// A root view with one text box and counters for what it reports.
struct Host {
    state: Entity<TextAreaState>,
    submitted: Rc<Cell<u32>>,
    cancelled: Rc<Cell<u32>>,
    /// Esc that bubbled past the box up to the screen.
    escaped: Rc<Cell<u32>>,
    cancel: bool,
    lines: (u8, u8),
    disabled: bool,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let submitted = self.submitted.clone();
        let cancelled = self.cancelled.clone();
        let escaped = self.escaped.clone();
        let mut area = TextArea::new(area_id(), &self.state)
            .placeholder(verbatim("Write a note"))
            .lines(self.lines.0, self.lines.1)
            .disabled(self.disabled)
            .on_submit(move |_, _| submitted.set(submitted.get() + 1));
        if self.cancel {
            area = area.on_cancel(move |_, _| cancelled.set(cancelled.get() + 1));
        }
        div()
            .w(px(300.0))
            .on_action(move |_: &Escape, _, _| escaped.set(escaped.get() + 1))
            .child(area)
    }
}

struct Setup {
    view: Entity<Host>,
    submitted: Rc<Cell<u32>>,
    cancelled: Rc<Cell<u32>>,
    escaped: Rc<Cell<u32>>,
}

fn open(
    cx: &mut TestAppContext,
    cancel: bool,
    lines: (u8, u8),
    disabled: bool,
) -> (Setup, &mut VisualTestContext) {
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
    let submitted = Rc::new(Cell::new(0));
    let cancelled = Rc::new(Cell::new(0));
    let escaped = Rc::new(Cell::new(0));
    let (view, cx) = cx.add_window_view({
        let (submitted, cancelled, escaped) =
            (submitted.clone(), cancelled.clone(), escaped.clone());
        move |window, cx| Host {
            state: cx.new(|cx| TextAreaState::new(window, cx)),
            submitted,
            cancelled,
            escaped,
            cancel,
            lines,
            disabled,
        }
    });
    cx.run_until_parked();
    (
        Setup {
            view,
            submitted,
            cancelled,
            escaped,
        },
        cx,
    )
}

fn state_of(setup: &Setup, cx: &mut VisualTestContext) -> Entity<TextAreaState> {
    setup.view.read_with(cx, |host, _| host.state.clone())
}

fn focus(setup: &Setup, cx: &mut VisualTestContext) {
    let state = state_of(setup, cx);
    cx.update(|window, cx| {
        let focus = state.read(cx).focus_handle();
        window.focus(&focus, cx);
    });
    cx.run_until_parked();
}

fn text(setup: &Setup, cx: &mut VisualTestContext) -> String {
    let state = state_of(setup, cx);
    state.read_with(cx, |state, _| state.text().to_string())
}

#[gpui::test]
fn typing_changes_the_text_and_notifies(cx: &mut TestAppContext) {
    let (setup, cx) = open(cx, false, (2, 6), false);
    focus(&setup, cx);
    let state = state_of(&setup, cx);
    let notified = Rc::new(Cell::new(0));
    let _observer = cx.update(|_, cx| {
        let notified = notified.clone();
        cx.observe(&state, move |_, _| notified.set(notified.get() + 1))
    });

    assert_eq!(text(&setup, cx), "");
    cx.simulate_input("hi");
    cx.run_until_parked();
    assert_eq!(text(&setup, cx), "hi");
    assert!(notified.get() >= 1, "typing notifies the observers");

    // ↵ is a new line, not a submit.
    cx.simulate_keystrokes("enter");
    cx.simulate_input("x");
    cx.run_until_parked();
    assert_eq!(text(&setup, cx), "hi\nx");
    assert_eq!(setup.submitted.get(), 0);

    // `set_text` does not come from the engine's change event, and
    // still notifies and updates `text`.
    let before = notified.get();
    cx.update(|window, cx| {
        state.update(cx, |state, cx| state.set_text("again", window, cx));
    });
    assert_eq!(text(&setup, cx), "again");
    assert!(notified.get() > before, "set_text notifies the observers");
}

#[gpui::test]
fn cmd_enter_submits_and_inserts_no_newline(cx: &mut TestAppContext) {
    let (setup, cx) = open(cx, false, (2, 6), false);
    focus(&setup, cx);
    cx.simulate_input("ab");
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    assert_eq!(setup.submitted.get(), 1);
    assert_eq!(
        text(&setup, cx),
        "ab",
        "the key is the box's, not a newline"
    );
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(setup.submitted.get(), 2);
}

#[gpui::test]
fn escape_cancels_when_a_handler_is_set(cx: &mut TestAppContext) {
    let (setup, cx) = open(cx, true, (2, 6), false);
    focus(&setup, cx);
    cx.simulate_input("keep");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(setup.cancelled.get(), 1);
    assert_eq!(setup.escaped.get(), 0, "the box has taken Esc");
    assert_eq!(text(&setup, cx), "keep", "the box does not clear itself");
}

#[gpui::test]
fn escape_bubbles_without_a_handler(cx: &mut TestAppContext) {
    let (setup, cx) = open(cx, false, (2, 6), false);
    focus(&setup, cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(setup.cancelled.get(), 0);
    assert_eq!(setup.escaped.get(), 1, "Esc reaches the screen as usual");
}

/// While an input method composes, Esc is the input method's: the first
/// Esc ends the composition and is not a cancel; the next one is.
#[gpui::test]
fn escape_during_composition_goes_to_the_input_method(cx: &mut TestAppContext) {
    let (setup, cx) = open(cx, true, (2, 6), false);
    focus(&setup, cx);
    let state = state_of(&setup, cx);
    let input = state.read_with(cx, |state, _| engine(state));
    let marked = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| input.update(cx, |input, cx| input.marked_text_range(window, cx)))
    };
    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "に", None, window, cx);
        });
    });
    assert!(marked(cx).is_some(), "the text is marked");

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(setup.cancelled.get(), 0, "the composition took Esc");
    assert_eq!(
        setup.escaped.get(),
        0,
        "and it did not bubble to the screen"
    );
    assert!(marked(cx).is_none(), "the composition ended");

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(setup.cancelled.get(), 1, "the next Esc cancels");
}

#[gpui::test]
fn placeholder_reaches_the_engine(cx: &mut TestAppContext) {
    let (setup, cx) = open(cx, false, (2, 6), false);
    let state = state_of(&setup, cx);
    let input = state.read_with(cx, |state, _| engine(state));
    let placeholder = input.read_with(cx, |input, _| input.presentation().placeholder().clone());
    assert_eq!(placeholder, "Write a note");
}

/// The box is as high as its content between the minimum and maximum
/// number of lines.
#[gpui::test]
fn height_follows_the_content_between_the_line_limits(cx: &mut TestAppContext) {
    let (setup, cx) = open(cx, false, (2, 4), false);
    let state = state_of(&setup, cx);
    let height_with = |text: &str, cx: &mut VisualTestContext| -> Pixels {
        cx.update(|window, cx| {
            state.update(cx, |state, cx| state.set_text(text, window, cx));
        });
        // The engine counts the rows during layout: a few frames settle it.
        let mut height = px(0.0);
        for _ in 0..3 {
            let id = cx.update(|window, _| window.window_handle().window_id());
            Recorder::begin(id);
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let records = Recorder::finish();
            height = records
                .iter()
                .find(|record| record.semantic.id == area_id())
                .expect("the box declares its node")
                .bounds
                .size
                .height;
        }
        height
    };
    let empty = height_with("", cx);
    let one = height_with("a", cx);
    let two = height_with("a\nb", cx);
    let three = height_with("a\nb\nc", cx);
    let four = height_with("a\nb\nc\nd", cx);
    let ten = height_with("a\nb\nc\nd\ne\nf\ng\nh\ni\nj", cx);

    assert_eq!(empty, two, "the minimum is two lines");
    assert_eq!(one, two);
    assert!(three > two, "a third line grows the box");
    assert!(four > three, "so does a fourth");
    assert_eq!(ten, four, "the maximum is four lines");
}

#[gpui::test]
fn a_disabled_box_takes_no_input(cx: &mut TestAppContext) {
    let (setup, cx) = open(cx, true, (2, 6), true);
    focus(&setup, cx);
    cx.simulate_input("nope");
    cx.run_until_parked();
    assert_eq!(text(&setup, cx), "");
    // Its own keys are not taken either: ⌘↩ sends nothing, Esc bubbles.
    cx.simulate_keystrokes("cmd-enter");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(setup.submitted.get(), 0);
    assert_eq!(setup.cancelled.get(), 0);
    assert_eq!(setup.escaped.get(), 1);
}
