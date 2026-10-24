//! The expanding primitive (file_reader's precondition E; the comment box,
//! the find bar): opens in place, `EXPAND_HEIGHT` for the height and
//! `EXPAND_FADE` for the opacity, and what is below moves down with the
//! height. Both values are `Transition`s in the caller's state; under
//! reduced motion only the opacity changes and the height arrives at once.

use std::time::Instant;

use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce, Styled,
    Window, div, px,
};

use crate::controls::require_ready;
use crate::motion::{EXPAND_FADE, EXPAND_HEIGHT, Transition, reduced};
use crate::semantic::{self, NodeId, Role, Semantic, State};

#[derive(IntoElement)]
pub struct Expander {
    id: NodeId,
    height: Pixels,
    opacity: f32,
    child: AnyElement,
}

impl Expander {
    /// `height` and `opacity` come from the caller's two `Transition`s
    /// (`height` in pixels, `opacity` in `0..=1`).
    pub fn new(id: NodeId, height: Pixels, opacity: f32, child: impl IntoElement) -> Expander {
        Expander {
            id,
            height: height.max(px(0.0)),
            opacity: opacity.clamp(0.0, 1.0),
            child: child.into_any_element(),
        }
    }

    /// Opens to `height` (closes when it is zero): `EXPAND_HEIGHT` for the
    /// height transition and `EXPAND_FADE` for the opacity one, or, under
    /// reduced motion, the height at once and the fade alone.
    pub fn go(
        height: &mut Transition,
        opacity: &mut Transition,
        target: Pixels,
        now: Instant,
        cx: &App,
    ) {
        let target_height = target.max(px(0.0)).as_f32();
        if reduced(cx) {
            height.set(target_height);
        } else {
            height.go(target_height, EXPAND_HEIGHT, now, cx);
        }
        let open = if target_height > 0.0 { 1.0 } else { 0.0 };
        opacity.go(open, EXPAND_FADE, now, cx);
    }
}

impl RenderOnce for Expander {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let spec = Semantic::new(self.id.clone(), Role::Group).state(State {
            expanded: Some(self.height > px(0.0)),
            ..State::default()
        });
        let expander = div()
            .id(self.id.element_id())
            .w_full()
            .h(self.height)
            .overflow_hidden()
            .child(div().opacity(self.opacity).child(self.child));
        semantic::declare(spec, expander)
    }
}
