//! The capsule (the review's floating bar with Looks Good and Send): radius
//! 22pt, 16pt left and 8pt right, 8pt above and below, children 12pt apart,
//! on `control_background` with a 0.5pt `separator` ring and a shadow. The
//! canvas blurs what is behind it; GPUI cannot, so it is opaque. It
//! declares no node: its content does.

use gpui::{
    AnyElement, App, IntoElement, ParentElement, Pixels, RenderOnce, Styled, Window, div, px,
};

use crate::controls::require_ready;
use crate::palette::colors;

#[derive(IntoElement, Default)]
pub struct Capsule {
    children: Vec<AnyElement>,
}

const RADIUS: Pixels = px(22.0);
const PADDING_Y: Pixels = px(8.0);
const PADDING_LEFT: Pixels = px(16.0);
const PADDING_RIGHT: Pixels = px(8.0);
const GAP: Pixels = px(12.0);
const RING: Pixels = px(0.5);

impl Capsule {
    pub fn new() -> Capsule {
        Capsule::default()
    }

    pub fn child(mut self, child: impl IntoElement) -> Capsule {
        self.children.push(child.into_any_element());
        self
    }
}

impl RenderOnce for Capsule {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = colors(window, cx);
        div()
            .flex()
            .items_center()
            .gap(GAP)
            .py(PADDING_Y)
            .pl(PADDING_LEFT)
            .pr(PADDING_RIGHT)
            .rounded(RADIUS)
            .bg(palette.control_background)
            .border(RING)
            .border_color(palette.separator)
            .shadow_md()
            .children(self.children)
    }
}
