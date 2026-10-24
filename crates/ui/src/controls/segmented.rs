//! The segmented control (the inspector's Files / Conversation, Usage's
//! 7D / 14D / 30D): the slider moves to the new option (`SEGMENT`). Its
//! position is a `Transition` in the caller's state, so a switch undone
//! halfway returns from where the slider is. Under reduced motion the
//! slider sits on the selected item and fades in instead of moving.

use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnimationExt, App, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, px, relative,
};

use crate::controls::require_ready;
use crate::font::{TextRole, TextRoleExt};
use crate::motion::{SEGMENT, Transition, reduced};
use crate::palette::colors;
use crate::semantic::{self, NodeId, Role, Semantic, State};
use crate::text::Text;

type SelectHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Segmented {
    id: NodeId,
    items: Vec<(NodeId, Text)>,
    selected: usize,
    slider: Option<f32>,
    on_select: Option<SelectHandler>,
}

/// The control's height, its padding around the slider and the radii.
const HEIGHT: Pixels = px(22.0);
const INSET: Pixels = px(1.0);
const RADIUS: Pixels = px(6.0);
const SLIDER_RADIUS: Pixels = px(5.0);
/// The alpha of the `label`-coloured track behind the slider.
const TRACK_ALPHA: f32 = 0.08;

impl Segmented {
    /// `selected` is clamped to the last item; an empty `items` draws
    /// nothing.
    pub fn new(id: NodeId, items: Vec<(NodeId, Text)>, selected: usize) -> Segmented {
        let selected = selected.min(items.len().saturating_sub(1));
        Segmented {
            id,
            items,
            selected,
            slider: None,
            on_select: None,
        }
    }

    /// The slider's position, a fraction between `0` and `items.len() - 1`,
    /// from the caller's `Transition::tick`. Without it the slider sits on
    /// `selected`.
    pub fn slider(mut self, position: f32) -> Segmented {
        self.slider = Some(position);
        self
    }

    pub fn on_select(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Segmented {
        self.on_select = Some(Rc::new(f));
        self
    }

    /// Moves the caller's slider `Transition` to `index`: `SEGMENT` from the
    /// current position, or at once under reduced motion (the control then
    /// fades the slider in on the new item; no displacement).
    pub fn select(slider: &mut Transition, index: usize, now: Instant, cx: &App) {
        let target = index as f32;
        if reduced(cx) {
            slider.set(target);
        } else {
            slider.go(target, SEGMENT, now, cx);
        }
    }
}

/// What the control remembers between frames: the last selection drawn and
/// whether the slider's current place was reached by a change it saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    selected: usize,
    animate: bool,
}

impl Selection {
    pub(crate) fn new(selected: usize) -> Selection {
        Selection {
            selected,
            animate: false,
        }
    }

    /// Records this frame's selection; true once a change has been seen
    /// (the fade's element then keeps its own state until it ends).
    pub(crate) fn update(&mut self, selected: usize) -> bool {
        if selected != self.selected {
            self.animate = true;
        }
        self.selected = selected;
        self.animate
    }
}

impl RenderOnce for Segmented {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let count = self.items.len();
        let spec = Semantic::new(self.id.clone(), Role::TabBar);
        if count == 0 {
            return semantic::declare(spec, div().into_any_element());
        }
        let reduced = reduced(cx);
        let position = if reduced {
            self.selected as f32
        } else {
            self.slider.unwrap_or(self.selected as f32)
        }
        .clamp(0.0, (count - 1) as f32);
        let width = 1.0 / count as f32;
        let slider = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(relative(position * width))
            .w(relative(width))
            .rounded(SLIDER_RADIUS)
            .bg(palette.control_background)
            .shadow_sm();
        // Under reduced motion the slider fades in on a newly selected
        // item, not on a mount that is already there (keyed element state
        // remembers the last selection).
        let last = window.use_keyed_state(self.id.part("selection"), cx, |_, _| {
            Selection::new(self.selected)
        });
        let fade = reduced && last.update(cx, |last, _| last.update(self.selected));
        let slider = if fade {
            slider
                .with_animation(
                    self.id.part_indexed("slider", self.selected),
                    SEGMENT.animation(cx),
                    |slider, t| slider.opacity(t),
                )
                .into_any_element()
        } else {
            slider.into_any_element()
        };
        let selected = self.selected;
        let on_select = self.on_select;
        let items = self
            .items
            .into_iter()
            .enumerate()
            .map(|(index, (id, text))| {
                let item_spec = Semantic::new(id.clone(), Role::Tab)
                    .label(&text)
                    .state(State {
                        selected: index == selected,
                        ..State::default()
                    });
                let mut item = div()
                    .id(id.element_id())
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .whitespace_nowrap()
                    .text_role(TextRole::Body, window, cx)
                    .text_color(if index == selected {
                        palette.label
                    } else {
                        palette.secondary_label
                    })
                    .child(text.shared());
                if let Some(on_select) = on_select.clone() {
                    item = item.on_click(move |_, window, cx| on_select(index, window, cx));
                }
                semantic::declare(item_spec, item)
            });
        let control = div()
            .id(self.id.element_id())
            .h(HEIGHT)
            .p(INSET)
            .rounded(RADIUS)
            .bg(palette.label.alpha(TRACK_ALPHA))
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .child(slider)
                    .children(items),
            );
        semantic::declare(spec, control.into_any_element())
    }
}
