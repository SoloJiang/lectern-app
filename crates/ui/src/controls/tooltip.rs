//! The hover tooltip of `Button::tooltip`: a small label in the system font
//! and colours, a thin separator border, a small radius and a shadow, at most
//! `MAX_WIDTH` wide with the text wrapping. GPUI places it by the pointer
//! (1pt right and below, flipped at the window edge); the view keeps
//! `POINTER_GAP` of empty space above and below its box, so it sits clear of
//! the cursor on either side. It has no animation of its own: GPUI shows and
//! hides it after `TOOLTIP_DELAY`, on mouse leave, mouse down and scroll.

use gpui::{
    AnyView, App, AppContext, Context, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, SharedString, Styled, Window, div, px,
};

use crate::controls::require_ready;
use crate::font::{TextRole, TextRoleExt};
use crate::palette::colors;

/// The widest the box grows; longer text wraps.
const MAX_WIDTH: Pixels = px(280.0);
const RADIUS: Pixels = px(5.0);
const PADDING_X: Pixels = px(8.0);
const PADDING_Y: Pixels = px(4.0);
const BORDER_WIDTH: Pixels = px(0.5);
/// Empty space above and below the box, so the pointer does not cover it.
const POINTER_GAP: Pixels = px(12.0);

/// The text of the tooltip is the only state; it is built when the tooltip is
/// due and dropped when it hides.
pub(crate) struct TooltipView {
    text: SharedString,
}

impl TooltipView {
    pub(crate) fn build(text: SharedString, cx: &mut App) -> AnyView {
        cx.new(|_| TooltipView { text }).into()
    }
}

impl Render for TooltipView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let text = self.text.clone();
        div().py(POINTER_GAP).child(
            div()
                // Test builds look the box up by its text (`debug_bounds`);
                // a release build drops the call.
                .debug_selector(|| format!("lectern-tooltip:{text}"))
                .max_w(MAX_WIDTH)
                .px(PADDING_X)
                .py(PADDING_Y)
                .rounded(RADIUS)
                .border(BORDER_WIDTH)
                .border_color(palette.separator)
                .bg(palette.control_background)
                .shadow_sm()
                .text_role(TextRole::Caption, window, cx)
                .text_color(palette.label)
                .child(self.text.clone()),
        )
    }
}
