//! The hover transition the list-like controls share (`Row`, `SidebarRow`,
//! `ActionCard`): a `Transition` in GPUI's keyed element state, so it
//! survives the re-renders of a value-like element. `HOVER_IN` when the
//! pointer enters, `HOVER_OUT` when it leaves.

use std::time::Instant;

use gpui::{App, Entity, Window};

use crate::motion::{HOVER_IN, HOVER_OUT, Transition};
use crate::semantic::NodeId;

pub(crate) struct Hover {
    state: Entity<Transition>,
    /// The strength at this frame, `0.0..=1.0`.
    pub(crate) strength: f32,
}

impl Hover {
    /// Ticks the control's hover transition; call it in `render`.
    pub(crate) fn track(id: &NodeId, window: &mut Window, cx: &mut App) -> Hover {
        let state = window.use_keyed_state(id.part("hover"), cx, |_, _| Transition::new(0.0));
        let now = Instant::now();
        let strength = state.update(cx, |hover, _| hover.tick(now, window));
        Hover { state, strength }
    }

    /// The listener for `on_hover`: starts the transition towards the new
    /// state and asks for a frame.
    pub(crate) fn listener(&self) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        let state = self.state.clone();
        move |hovered, window, cx| {
            let (target, motion) = if *hovered {
                (1.0, HOVER_IN)
            } else {
                (0.0, HOVER_OUT)
            };
            state.update(cx, |hover, cx| hover.go(target, motion, Instant::now(), cx));
            window.refresh();
        }
    }
}
