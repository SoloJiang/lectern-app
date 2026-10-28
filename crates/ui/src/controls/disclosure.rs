//! The disclosure arrow (the round and file fold rows): a `ChevronRight`
//! that points right at `0` and down at `1`, turned in proportion between.
//! The value is a `Transition` in the caller's state (`EXPAND_HEIGHT`'s
//! curve; under reduced motion the caller `set`s it at once). Decoration:
//! the row declares the expanded state.

use std::f32::consts::FRAC_PI_2;

use gpui::{App, IntoElement, Pixels, RenderOnce, Styled, Window, px, radians};

use crate::controls::require_ready;
use crate::icon::Icon;
use crate::palette::colors;

#[derive(IntoElement)]
pub struct Disclosure {
    expanded: f32,
}

const SIZE: Pixels = px(12.0);

impl Disclosure {
    /// `expanded` is clamped to `0..=1`.
    pub fn new(expanded: f32) -> Disclosure {
        Disclosure {
            expanded: expanded.clamp(0.0, 1.0),
        }
    }

    /// The turn at `expanded`, in radians: a quarter turn clockwise at `1`.
    pub(crate) fn angle(expanded: f32) -> f32 {
        FRAC_PI_2 * expanded.clamp(0.0, 1.0)
    }
}

impl RenderOnce for Disclosure {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = colors(window, cx);
        gpui_component::Icon::new(Icon::ChevronRight)
            .size(SIZE)
            .text_color(palette.secondary_label)
            .rotate(radians(Disclosure::angle(self.expanded)))
    }
}
