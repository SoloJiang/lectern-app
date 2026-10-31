//! The self-drawn element of the grid (docs/design/terminal_view.md「数据流与线程」).
//! Prepaint builds the list of drawing operations (the pane lays out the rows
//! and sends `Resize`); paint replays the list under a clip, registers the
//! input handler of the input method and the mouse listeners.

use std::time::Instant;

use gpui::{
    App, Bounds, ContentMask, DispatchPhase, Element, ElementId, ElementInputHandler, Entity,
    FocusHandle, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement,
    LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, ScrollWheelEvent,
    Style, UnderlineStyle, Window, point, px, relative,
};

use crate::paint::Op;
use crate::pane::TerminalPane;

pub(crate) struct GridElement {
    pane: Entity<TerminalPane>,
    focus: FocusHandle,
}

impl GridElement {
    pub(crate) fn new(pane: Entity<TerminalPane>, focus: FocusHandle) -> GridElement {
        GridElement { pane, focus }
    }
}

/// What prepaint hands to paint.
pub(crate) struct Prepared {
    hitbox: Hitbox,
    ops: Vec<Op>,
}

impl IntoElement for GridElement {
    type Element = GridElement;

    fn into_element(self) -> GridElement {
        self
    }
}

impl Element for GridElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepared;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepared {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        // The entity is not borrowed while its view paints: its render has
        // returned. Nothing here calls `notify`.
        let ops = self
            .pane
            .update(cx, |pane, cx| pane.prepare(bounds, window, cx));
        Prepared { hitbox, ops }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepared: &mut Prepared,
        window: &mut Window,
        cx: &mut App,
    ) {
        let started = Instant::now();
        let font_size = lectern_ui::TextRole::Code.size();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for op in &prepared.ops {
                replay(op, font_size, window);
            }
        });

        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.pane.clone()),
            cx,
        );
        self.listen(prepared.hitbox.clone(), window);

        let ops = std::mem::take(&mut prepared.ops);
        let took = started.elapsed();
        self.pane.update(cx, |pane, _| {
            pane.stats.record_paint(pane.id(), took);
            #[cfg(test)]
            {
                pane.painted = ops;
            }
            #[cfg(not(test))]
            drop(ops);
        });
    }
}

impl GridElement {
    /// The mouse listeners. A press is the grid's only when the pointer is
    /// over its hitbox (a panel above it takes the press); the release and
    /// the move of a drag go to the pane wherever the pointer is.
    fn listen(&self, hitbox: Hitbox, window: &mut Window) {
        let pane = self.pane.clone();
        let over = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && over.is_hovered(window) {
                pane.update(cx, |pane, cx| pane.mouse_down(event, window, cx));
            }
        });
        let pane = self.pane.clone();
        let over = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = over.is_hovered(window);
                pane.update(cx, |pane, cx| pane.mouse_move(event, hovered, cx));
            }
        });
        let pane = self.pane.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                pane.update(cx, |pane, cx| pane.mouse_up(event, cx));
            }
        });
        let pane = self.pane.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                pane.update(cx, |pane, _| pane.wheel(event));
            }
        });
    }
}

/// Draws one operation. A glyph that cannot be rasterised is skipped.
fn replay(op: &Op, font_size: Pixels, window: &mut Window) {
    match op {
        Op::Fill { bounds, color } => window.paint_quad(gpui::fill(*bounds, *color)),
        Op::Outline {
            bounds,
            color,
            width,
        } => window.paint_quad(gpui::PaintQuad {
            bounds: *bounds,
            corner_radii: (0.0).into(),
            background: gpui::transparent_black().into(),
            border_widths: (*width).into(),
            border_color: *color,
            border_style: gpui::BorderStyle::default(),
        }),
        Op::Glyph {
            origin,
            font,
            glyph,
            color,
            emoji,
        } => {
            if *emoji {
                window.paint_emoji(*origin, *font, *glyph, font_size).ok();
            } else {
                window
                    .paint_glyph(*origin, *font, *glyph, font_size, *color)
                    .ok();
            }
        }
        Op::Curve {
            from,
            ctrl,
            to,
            width,
            color,
        } => {
            let mut path = PathBuilder::stroke(*width);
            path.move_to(*from);
            path.curve_to(*to, *ctrl);
            if let Ok(path) = path.build() {
                window.paint_path(path, *color);
            }
        }
        Op::Wavy {
            origin,
            width,
            thickness,
            color,
        } => window.paint_underline(
            point(origin.x, origin.y),
            *width,
            &UnderlineStyle {
                thickness: px(thickness.as_f32()),
                color: Some(*color),
                wavy: true,
            },
        ),
    }
}
