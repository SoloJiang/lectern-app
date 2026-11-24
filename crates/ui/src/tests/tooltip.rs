//! `Button::tooltip` (docs/design/ui.md「控件」): the tooltip shows after
//! `TOOLTIP_DELAY` of hovering, also on a disabled button, and hides on mouse
//! leave, mouse down and scroll. The tooltip's box is found by its text with
//! `debug_bounds` (the view sets a debug selector; a release build drops it).

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Bounds, Context, IntoElement, Modifiers, MouseButton, ParentElement, Pixels, Point, Render,
    ScrollDelta, ScrollWheelEvent, Styled, TestAppContext, VisualTestContext, Window, div, point,
    px,
};
use lectern_i18n::{Locale, Strings};

use crate::motion::TOOLTIP_DELAY;
use crate::semantic::NodeId;
use crate::{Appearance, Button, Text, init_fixed};

const ENABLED_TIP: &str = "Edit this file";
const DISABLED_TIP: &str = "The file is too large to edit";

/// Two buttons in a column, 30pt high each: the enabled one at y 0 to 30,
/// the disabled one at y 30 to 60; a third has no tooltip.
struct Scene {
    long_tip: Option<&'static str>,
}

fn verbatim(text: &str) -> Text {
    Text::verbatim(Arc::<str>::from(text))
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let id = NodeId::new("tooltip", "tests", None);
        let enabled = Button::new(id.child("enabled"), verbatim("Edit"))
            .tooltip(verbatim(self.long_tip.unwrap_or(ENABLED_TIP)))
            .on_click(|_, _| {});
        div()
            .flex()
            .flex_col()
            .items_start()
            .child(enabled)
            .child(
                Button::new(id.child("disabled"), verbatim("Edit…"))
                    .disabled(true)
                    .tooltip(verbatim(DISABLED_TIP)),
            )
            .child(Button::new(id.child("plain"), verbatim("Plain")))
    }
}

fn window_of<'a>(
    cx: &'a mut TestAppContext,
    long_tip: Option<&'static str>,
) -> &'a mut VisualTestContext {
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
    let (_, cx) = cx.add_window_view(|_, _| Scene { long_tip });
    cx.run_until_parked();
    cx
}

const ENABLED_AT: Point<Pixels> = Point {
    x: px(10.0),
    y: px(15.0),
};
const DISABLED_AT: Point<Pixels> = Point {
    x: px(10.0),
    y: px(45.0),
};
const PLAIN_AT: Point<Pixels> = Point {
    x: px(10.0),
    y: px(75.0),
};
const AWAY: Point<Pixels> = Point {
    x: px(600.0),
    y: px(400.0),
};

fn hover(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_mouse_move(at, None, Modifiers::default());
    cx.run_until_parked();
}

/// Lets `wait` of the test clock pass, then draws a frame.
fn wait(cx: &mut VisualTestContext, wait: Duration) {
    cx.executor().advance_clock(wait);
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn shown(cx: &mut VisualTestContext, text: &str) -> Option<Bounds<Pixels>> {
    // `debug_bounds` takes a `'static` key; the few texts are leaked.
    let key: &'static str = Box::leak(format!("lectern-tooltip:{text}").into_boxed_str());
    cx.debug_bounds(key)
}

#[gpui::test]
fn tooltip_shows_after_the_delay_on_hover(cx: &mut TestAppContext) {
    assert_eq!(TOOLTIP_DELAY, Duration::from_millis(1000));
    let cx = window_of(cx, None);
    hover(cx, ENABLED_AT);
    wait(cx, TOOLTIP_DELAY - Duration::from_millis(1));
    assert!(shown(cx, ENABLED_TIP).is_none(), "not before the delay");

    wait(cx, Duration::from_millis(1));
    let bounds = shown(cx, ENABLED_TIP).expect("shown after the delay");
    // By the pointer, below it: the box starts clear of the cursor.
    assert!(bounds.origin.y > ENABLED_AT.y);
    assert!(bounds.origin.x >= ENABLED_AT.x);
    assert!(shown(cx, DISABLED_TIP).is_none(), "only its own tooltip");
}

#[gpui::test]
fn tooltip_shows_on_a_disabled_button(cx: &mut TestAppContext) {
    let cx = window_of(cx, None);
    hover(cx, DISABLED_AT);
    wait(cx, TOOLTIP_DELAY);
    assert!(
        shown(cx, DISABLED_TIP).is_some(),
        "a grey button says why on hover"
    );
    assert!(shown(cx, ENABLED_TIP).is_none());
}

#[gpui::test]
fn tooltip_hides_after_the_pointer_leaves(cx: &mut TestAppContext) {
    let cx = window_of(cx, None);
    hover(cx, ENABLED_AT);
    wait(cx, TOOLTIP_DELAY);
    assert!(shown(cx, ENABLED_TIP).is_some());

    hover(cx, AWAY);
    wait(cx, Duration::ZERO);
    assert!(shown(cx, ENABLED_TIP).is_none());

    // A pointer that leaves before the delay never shows it.
    hover(cx, DISABLED_AT);
    wait(cx, TOOLTIP_DELAY / 2);
    hover(cx, AWAY);
    wait(cx, TOOLTIP_DELAY);
    assert!(shown(cx, DISABLED_TIP).is_none());
}

#[gpui::test]
fn tooltip_hides_on_mouse_down_and_on_scroll(cx: &mut TestAppContext) {
    let cx = window_of(cx, None);
    hover(cx, ENABLED_AT);
    wait(cx, TOOLTIP_DELAY);
    assert!(shown(cx, ENABLED_TIP).is_some());
    cx.simulate_mouse_down(ENABLED_AT, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(shown(cx, ENABLED_TIP).is_none(), "hidden on mouse down");

    // The disabled button swallows the mouse down; the tooltip hides anyway.
    hover(cx, DISABLED_AT);
    wait(cx, TOOLTIP_DELAY);
    assert!(shown(cx, DISABLED_TIP).is_some());
    cx.simulate_mouse_down(DISABLED_AT, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        shown(cx, DISABLED_TIP).is_none(),
        "hidden on mouse down on a disabled button"
    );

    cx.simulate_mouse_up(DISABLED_AT, MouseButton::Left, Modifiers::default());
    hover(cx, ENABLED_AT);
    wait(cx, TOOLTIP_DELAY);
    assert!(shown(cx, ENABLED_TIP).is_some());
    // The scroll is at the pointer's place, on the button: it is the scroll
    // that hides the tooltip, not the pointer leaving.
    cx.simulate_event(scroll_at(ENABLED_AT));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(shown(cx, ENABLED_TIP).is_none(), "hidden on scroll");
}

fn scroll_at(position: Point<Pixels>) -> ScrollWheelEvent {
    ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-12.0))),
        modifiers: Modifiers::default(),
        touch_phase: Default::default(),
    }
}

/// A known limit of GPUI's tooltip, kept as a test so an upgrade shows it
/// changed: GPUI's scroll listener does not clear a tooltip while the pointer
/// is over the tooltip's own bounds. Those start 1pt right and below where the
/// tooltip appeared and include the empty `POINTER_GAP` above the box, so a
/// pointer that has moved a few points down and right, still on the button,
/// is inside them. Mouse down hides the tooltip there all the same, and so
/// does a scroll once the pointer is left of the bounds.
#[gpui::test]
fn tooltip_stays_on_scroll_over_its_own_bounds(cx: &mut TestAppContext) {
    let nudged = point(ENABLED_AT.x + px(3.0), ENABLED_AT.y + px(3.0));
    let cx = window_of(cx, None);
    hover(cx, ENABLED_AT);
    wait(cx, TOOLTIP_DELAY);
    assert!(shown(cx, ENABLED_TIP).is_some());
    hover(cx, nudged);
    wait(cx, Duration::ZERO);
    assert!(
        shown(cx, ENABLED_TIP).is_some(),
        "still there after a nudge"
    );

    cx.simulate_event(scroll_at(nudged));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        shown(cx, ENABLED_TIP).is_some(),
        "GPUI keeps it on scroll over its own bounds"
    );

    cx.simulate_mouse_down(nudged, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        shown(cx, ENABLED_TIP).is_none(),
        "mouse down hides it there"
    );
}

#[gpui::test]
fn a_button_without_a_tooltip_shows_none(cx: &mut TestAppContext) {
    let cx = window_of(cx, None);
    hover(cx, PLAIN_AT);
    wait(cx, TOOLTIP_DELAY * 2);
    assert!(shown(cx, ENABLED_TIP).is_none());
    assert!(shown(cx, DISABLED_TIP).is_none());
}

#[gpui::test]
fn a_long_tooltip_wraps_at_the_maximum_width(cx: &mut TestAppContext) {
    const LONG: &str = "This file is larger than the editing limit, so it opens read only; \
        open it in another editor to change it, or split the file first.";
    let cx = window_of(cx, Some(LONG));
    hover(cx, ENABLED_AT);
    wait(cx, TOOLTIP_DELAY);
    let long = shown(cx, LONG).expect("shown");
    assert!(long.size.width <= px(280.0), "{:?}", long.size);
    // One line of 11pt text with its padding is well under 30pt; the text is
    // over 100 characters, so it takes three lines or more.
    assert!(long.size.height > px(40.0), "{:?}", long.size);
}
