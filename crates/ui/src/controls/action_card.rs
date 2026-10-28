//! The action card (Welcome's Open Folder…): a `fill` card of radius 10pt
//! with an accent icon, a title and a line under it, and the shortcut at
//! the right. Hovered, the fill deepens to `fill_strong` (`HOVER_IN` /
//! `HOVER_OUT`). Disabled (Welcome's Clone card), it keeps the `fill`,
//! greys its text and does not answer the pointer.

use std::rc::Rc;

use gpui::{
    App, Div, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::controls::hover::Hover;
use crate::controls::require_ready;
use crate::font::{TextRole, TextRoleExt};
use crate::icon::Icon;
use crate::palette::colors;
use crate::semantic::{self, NodeId, Role, Semantic, State};
use crate::text::Text;

type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ActionCard {
    id: NodeId,
    icon: Icon,
    title: Text,
    detail: Text,
    shortcut: Option<Text>,
    on_click: Option<ClickHandler>,
    disabled: bool,
}

const RADIUS: Pixels = px(10.0);
const PADDING_X: Pixels = px(12.0);
const PADDING_Y: Pixels = px(10.0);
const GAP: Pixels = px(12.0);
const ICON: Pixels = px(20.0);

impl ActionCard {
    pub fn new(id: NodeId, icon: Icon, title: Text, detail: Text) -> ActionCard {
        ActionCard {
            id,
            icon,
            title,
            detail,
            shortcut: None,
            on_click: None,
            disabled: false,
        }
    }

    /// The shortcut hint at the right (`Callout`, secondary).
    pub fn shortcut(mut self, hint: Text) -> ActionCard {
        self.shortcut = Some(hint);
        self
    }

    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> ActionCard {
        self.on_click = Some(Rc::new(f));
        self
    }

    /// No hover and no click; the icon and the title in `secondary_label`,
    /// the line under it and the hint in `tertiary_label`.
    pub fn disabled(mut self, disabled: bool) -> ActionCard {
        self.disabled = disabled;
        self
    }
}

trait Tint {
    fn tint(self, color: Option<Hsla>) -> Self;
}

impl Tint for Div {
    /// The text colour when given; the inherited one otherwise.
    fn tint(self, color: Option<Hsla>) -> Div {
        match color {
            Some(color) => self.text_color(color),
            None => self,
        }
    }
}

/// `fill` towards `fill_strong` by `strength`: both are the `label` colour
/// at an opacity, so the blend is the opacity in between.
fn blend(fill: Hsla, fill_strong: Hsla, strength: f32) -> Hsla {
    fill.alpha(fill.a + (fill_strong.a - fill.a) * strength.clamp(0.0, 1.0))
}

impl RenderOnce for ActionCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let spec = Semantic::new(self.id.clone(), Role::Button)
            .label(&self.title)
            .state(State {
                disabled: self.disabled,
                ..State::default()
            });
        let hover = Hover::track(&self.id, window, cx);
        let strength = if self.disabled { 0.0 } else { hover.strength };
        let (icon_color, dim) = if self.disabled {
            (palette.secondary_label, Some(palette.tertiary_label))
        } else {
            (palette.accent, None)
        };
        let mut card = div()
            .id(self.id.element_id())
            .w_full()
            .flex()
            .items_center()
            .gap(GAP)
            .px(PADDING_X)
            .py(PADDING_Y)
            .rounded(RADIUS)
            .bg(blend(palette.fill, palette.fill_strong, strength))
            .cursor_default()
            .child(
                gpui_component::Icon::new(self.icon)
                    .size(ICON)
                    .text_color(icon_color),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_role(TextRole::Headline, window, cx)
                            .tint(self.disabled.then_some(palette.secondary_label))
                            .child(self.title.shared()),
                    )
                    .child(
                        div()
                            .text_role(TextRole::Callout, window, cx)
                            .tint(dim)
                            .child(self.detail.shared()),
                    ),
            );
        if !self.disabled {
            card = card.on_hover(hover.listener());
        }
        if let Some(shortcut) = self.shortcut {
            card = card.child(
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Callout, window, cx)
                    .tint(dim)
                    .child(shortcut.shared()),
            );
        }
        if let (false, Some(on_click)) = (self.disabled, self.on_click) {
            card = card.on_click(move |_, window, cx| on_click(window, cx));
        }
        semantic::declare(spec, card)
    }
}
