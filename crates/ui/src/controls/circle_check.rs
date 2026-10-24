//! The circular check (a file header's「已看」): the circle fills
//! (`CHECK_FILL`), the tick is stroked (`CHECK_STROKE`, 60 ms later). The
//! keyboard (the view's V action) and the mouse change the same `checked`
//! state, so both play the same animation element. The file name turning
//! secondary is the caller's (it owns that text).

use std::rc::Rc;

use gpui::{
    AnimationExt, App, Bounds, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement,
    PathBuilder, Pixels, Point, RenderOnce, StatefulInteractiveElement, Styled, Window, canvas,
    div, point, px,
};

use crate::controls::require_ready;
use crate::motion::{CHECK_FILL, CHECK_STROKE};
use crate::palette::{colors, on_accent};
use crate::semantic::{self, NodeId, Role, Semantic, State};

type ChangeHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct CircleCheck {
    id: NodeId,
    checked: bool,
    on_change: Option<ChangeHandler>,
}

/// The control's side.
const SIDE: Pixels = px(16.0);
/// The ring of the unchecked state and the tick's stroke.
const RING_WIDTH: Pixels = px(1.0);
const TICK_WIDTH: Pixels = px(1.5);
/// The tick's corners in a 16-unit box: start, bend, end.
const TICK: [(f32, f32); 3] = [(4.5, 8.5), (7.0, 11.0), (12.0, 5.5)];

impl CircleCheck {
    pub fn new(id: NodeId, checked: bool) -> CircleCheck {
        CircleCheck {
            id,
            checked,
            on_change: None,
        }
    }

    pub fn on_change(mut self, f: impl Fn(bool, &mut Window, &mut App) + 'static) -> CircleCheck {
        self.on_change = Some(Rc::new(f));
        self
    }

    /// The element that animates the fill when `checked` becomes true: a
    /// function of the node alone, so the key and the mouse start the same
    /// one. It is rendered only while checked; GPUI drops an unrendered
    /// element's state after the frame, so a later check starts afresh.
    pub(crate) fn fill_animation_id(id: &NodeId) -> ElementId {
        id.part("fill")
    }

    pub(crate) fn stroke_animation_id(id: &NodeId) -> ElementId {
        id.part("stroke")
    }
}

/// What the control remembers between frames (GPUI keyed element state):
/// the last `checked` it drew, and whether the current checked run began
/// with a change it saw. A control mounted already checked draws the end
/// state at once; only a change from unchecked to checked animates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    checked: bool,
    animate: bool,
}

impl Mark {
    pub(crate) fn new(checked: bool) -> Mark {
        Mark {
            checked,
            animate: false,
        }
    }

    /// Records this frame's `checked`; true when the fill and the tick are
    /// to be drawn through their animations.
    pub(crate) fn update(&mut self, checked: bool) -> bool {
        if checked && !self.checked {
            self.animate = true;
        } else if !checked {
            self.animate = false;
        }
        self.checked = checked;
        self.animate
    }
}

impl RenderOnce for CircleCheck {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let spec = Semantic::new(self.id.clone(), Role::Toggle).state(State {
            checked: Some(self.checked),
            ..State::default()
        });
        let checked = self.checked;
        let mark = window.use_keyed_state(self.id.part("mark"), cx, |_, _| Mark::new(checked));
        let animate = mark.update(cx, |mark, _| mark.update(checked));
        let mut circle = div()
            .id(self.id.element_id())
            .relative()
            .size(SIDE)
            .flex_shrink_0()
            .rounded_full()
            .cursor_default();
        if checked {
            let fill = div().absolute().inset_0().rounded_full().bg(palette.accent);
            let tick = Tick {
                fraction: 1.0,
                color: on_accent(),
            };
            if animate {
                let fill = fill.with_animation(
                    CircleCheck::fill_animation_id(&self.id),
                    CHECK_FILL.animation(cx),
                    |fill, t| fill.opacity(t),
                );
                let tick = tick.with_animation(
                    CircleCheck::stroke_animation_id(&self.id),
                    CHECK_STROKE.animation(cx),
                    |tick, t| Tick {
                        fraction: t,
                        ..tick
                    },
                );
                circle = circle.child(fill).child(tick);
            } else {
                circle = circle.child(fill).child(tick);
            }
        } else {
            circle = circle
                .border(RING_WIDTH)
                .border_color(palette.tertiary_label);
        }
        if let Some(on_change) = self.on_change {
            circle = circle.on_click(move |_, window, cx| on_change(!checked, window, cx));
        }
        semantic::declare(spec, circle)
    }
}

/// The tick, stroked up to `fraction` of its length.
#[derive(IntoElement, Clone, Copy)]
struct Tick {
    fraction: f32,
    color: Hsla,
}

impl RenderOnce for Tick {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Tick { fraction, color } = self;
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                if let Some(path) = tick_path(bounds, fraction) {
                    window.paint_path(path, color);
                }
            },
        )
        .absolute()
        .inset_0()
    }
}

/// The tick's first `fraction` as a stroke, None when nothing is drawn yet.
fn tick_path(bounds: Bounds<Pixels>, fraction: f32) -> Option<gpui::Path<Pixels>> {
    if fraction <= 0.0 {
        return None;
    }
    let scale = bounds.size.width.min(bounds.size.height) / SIDE;
    let corner = |(x, y): (f32, f32)| -> Point<Pixels> {
        point(
            bounds.origin.x + px(x) * scale,
            bounds.origin.y + px(y) * scale,
        )
    };
    let [start, bend, end] = TICK.map(corner);
    let first = distance(start, bend);
    let second = distance(bend, end);
    let drawn = (first + second) * fraction.min(1.0);
    let mut builder = PathBuilder::stroke(TICK_WIDTH * scale);
    builder.move_to(start);
    if drawn <= first {
        builder.line_to(lerp(start, bend, drawn / first));
    } else {
        builder.line_to(bend);
        builder.line_to(lerp(bend, end, (drawn - first) / second));
    }
    builder.build().ok()
}

fn distance(a: Point<Pixels>, b: Point<Pixels>) -> f32 {
    let dx = (b.x - a.x).as_f32();
    let dy = (b.y - a.y).as_f32();
    (dx * dx + dy * dy).sqrt()
}

fn lerp(a: Point<Pixels>, b: Point<Pixels>, t: f32) -> Point<Pixels> {
    point(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}
