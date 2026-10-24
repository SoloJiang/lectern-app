//! The seen progress: a `Bar` whose width grows or a `Ring` at the end of an
//! Overview row. The value comes from the caller's `Transition`
//! (`PROGRESS`); the control keeps no state and runs no animation of its own.

use std::f32::consts::{FRAC_PI_2, TAU};

use gpui::{
    App, Bounds, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, Point, RenderOnce,
    SharedString, Styled, Window, canvas, div, point, px, relative,
};

use crate::controls::require_ready;
use crate::palette::colors;
use crate::semantic::{self, NodeId, Role, Semantic};

#[derive(IntoElement)]
pub struct Progress {
    id: NodeId,
    shape: ProgressShape,
    fraction: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProgressShape {
    Bar,
    Ring,
}

/// The bar's height, the ring's side and the ring's stroke.
const BAR_HEIGHT: Pixels = px(4.0);
const RING_SIDE: Pixels = px(16.0);
const RING_WIDTH: Pixels = px(2.0);

impl Progress {
    /// `fraction` is clamped to `0..=1`.
    pub fn new(id: NodeId, shape: ProgressShape, fraction: f32) -> Progress {
        Progress {
            id,
            shape,
            fraction: if fraction.is_finite() {
                fraction.clamp(0.0, 1.0)
            } else {
                0.0
            },
        }
    }

    pub fn fraction(&self) -> f32 {
        self.fraction
    }
}

impl RenderOnce for Progress {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let spec = Semantic::new(self.id.clone(), Role::Progress).attr(
            "fraction",
            SharedString::from(format!("{:.3}", self.fraction)),
        );
        let fraction = self.fraction;
        let element = match self.shape {
            ProgressShape::Bar => div()
                .w_full()
                .h(BAR_HEIGHT)
                .rounded_full()
                .bg(palette.separator)
                .child(
                    div()
                        .h_full()
                        .w(relative(fraction))
                        .rounded_full()
                        .bg(palette.accent),
                )
                .into_any_element(),
            ProgressShape::Ring => {
                let (track, fill) = (palette.separator, palette.accent);
                div()
                    .size(RING_SIDE)
                    .flex_shrink_0()
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, (), window, _| {
                                paint_ring(bounds, 1.0, track, window);
                                paint_ring(bounds, fraction, fill, window);
                            },
                        )
                        .size_full(),
                    )
                    .into_any_element()
            }
        };
        semantic::declare(spec, element)
    }
}

/// Strokes the first `fraction` of the ring, clockwise from the top.
fn paint_ring(bounds: Bounds<Pixels>, fraction: f32, color: Hsla, window: &mut Window) {
    if fraction <= 0.0 {
        return;
    }
    let side = bounds.size.width.min(bounds.size.height);
    let width = RING_WIDTH * (side / RING_SIDE);
    let radius = (side - width) / 2.0;
    let centre = point(
        bounds.origin.x + bounds.size.width / 2.0,
        bounds.origin.y + bounds.size.height / 2.0,
    );
    let at = |turn: f32| -> Point<Pixels> {
        let angle = turn * TAU - FRAC_PI_2;
        point(
            centre.x + radius * angle.cos(),
            centre.y + radius * angle.sin(),
        )
    };
    let radii = point(radius, radius);
    let mut builder = PathBuilder::stroke(width);
    builder.move_to(at(0.0));
    if fraction >= 1.0 {
        // A full circle is two half arcs: one arc cannot end where it starts.
        builder.arc_to(radii, px(0.0), false, true, at(0.5));
        builder.arc_to(radii, px(0.0), false, true, at(1.0));
    } else {
        builder.arc_to(radii, px(0.0), fraction > 0.5, true, at(fraction));
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}
