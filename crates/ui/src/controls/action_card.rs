//! The action card (Welcome's Open Folder…): a `fill` card of radius 10pt
//! with an accent icon, a title and a line under it, and the shortcut at
//! the right. Hovered, the fill deepens to `fill_strong` (`HOVER_IN` /
//! `HOVER_OUT`).

use std::rc::Rc;

use gpui::{
    App, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::controls::hover::Hover;
use crate::controls::require_ready;
use crate::font::{TextRole, TextRoleExt};
use crate::icon::Icon;
use crate::palette::colors;
use crate::semantic::{self, NodeId, Role, Semantic};
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
        let spec = Semantic::new(self.id.clone(), Role::Button).label(&self.title);
        let hover = Hover::track(&self.id, window, cx);
        let mut card = div()
            .id(self.id.element_id())
            .w_full()
            .flex()
            .items_center()
            .gap(GAP)
            .px(PADDING_X)
            .py(PADDING_Y)
            .rounded(RADIUS)
            .bg(blend(palette.fill, palette.fill_strong, hover.strength))
            .cursor_default()
            .child(
                gpui_component::Icon::new(self.icon)
                    .size(ICON)
                    .text_color(palette.accent),
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
                            .child(self.title.shared()),
                    )
                    .child(
                        div()
                            .text_role(TextRole::Callout, window, cx)
                            .child(self.detail.shared()),
                    ),
            )
            .on_hover(hover.listener());
        if let Some(shortcut) = self.shortcut {
            card = card.child(
                div()
                    .flex_shrink_0()
                    .text_role(TextRole::Callout, window, cx)
                    .child(shortcut.shared()),
            );
        }
        if let Some(on_click) = self.on_click {
            card = card.on_click(move |_, window, cx| on_click(window, cx));
        }
        semantic::declare(spec, card)
    }
}
