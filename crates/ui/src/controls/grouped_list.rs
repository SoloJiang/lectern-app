//! The grouped list (interaction spec「视觉」,「分组」): 12pt radius,
//! `control_background`, 0.5pt `separator` between rows. A `Row` fades its
//! hover background in (`HOVER_IN`) and out (`HOVER_OUT`) and shows
//! selection with `selected_content_background`; its content is the
//! caller's (agent dot, two lines of text, progress ring).

use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::controls::require_ready;
use crate::font::{TextRole, TextRoleExt};
use crate::motion::{HOVER_IN, HOVER_OUT, Transition};
use crate::palette::{colors, on_accent};
use crate::semantic::{self, NodeId, Role, Semantic, State};
use crate::text::Text;

type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct GroupedList {
    id: NodeId,
    header: Option<Text>,
    rows: Vec<Row>,
}

#[derive(IntoElement)]
pub struct Row {
    id: NodeId,
    selected: bool,
    separated: bool,
    on_click: Option<ClickHandler>,
    children: Vec<AnyElement>,
}

const GROUP_RADIUS: Pixels = px(12.0);
const SEPARATOR_WIDTH: Pixels = px(0.5);
/// The gap between a header and its group, and the header's inset.
const HEADER_GAP: Pixels = px(6.0);
const HEADER_INSET: Pixels = px(12.0);
/// A row's padding, the gap between its children and its least height.
const ROW_PADDING_X: Pixels = px(12.0);
const ROW_PADDING_Y: Pixels = px(8.0);
const ROW_GAP: Pixels = px(10.0);
const ROW_MIN_HEIGHT: Pixels = px(36.0);
/// The alpha of the `label`-coloured hover wash at full strength.
const HOVER_ALPHA: f32 = 0.06;

impl GroupedList {
    pub fn new(id: NodeId) -> GroupedList {
        GroupedList {
            id,
            header: None,
            rows: Vec::new(),
        }
    }

    pub fn header(mut self, text: Text) -> GroupedList {
        self.header = Some(text);
        self
    }

    pub fn row(mut self, row: Row) -> GroupedList {
        self.rows.push(row);
        self
    }
}

impl RenderOnce for GroupedList {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let mut spec = Semantic::new(self.id.clone(), Role::List);
        if let Some(header) = &self.header {
            spec = spec.label(header);
        }
        let group = div()
            .id(self.id.element_id())
            .flex()
            .flex_col()
            .rounded(GROUP_RADIUS)
            .overflow_hidden()
            .bg(palette.control_background)
            .children(
                self.rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, row)| row.separated(index > 0)),
            );
        let list: AnyElement = match self.header {
            Some(header) => div()
                .flex()
                .flex_col()
                .gap(HEADER_GAP)
                .child(
                    div()
                        .pl(HEADER_INSET)
                        .text_role(TextRole::Caption, window, cx)
                        .child(header.shared()),
                )
                .child(group)
                .into_any_element(),
            None => group.into_any_element(),
        };
        semantic::declare(spec, list)
    }
}

impl Row {
    pub fn new(id: NodeId) -> Row {
        Row {
            id,
            selected: false,
            separated: false,
            on_click: None,
            children: Vec::new(),
        }
    }

    pub fn selected(mut self, selected: bool) -> Row {
        self.selected = selected;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Row {
        self.on_click = Some(Rc::new(f));
        self
    }

    pub fn child(mut self, child: impl IntoElement) -> Row {
        self.children.push(child.into_any_element());
        self
    }

    /// Set by `GroupedList` on every row but the first.
    fn separated(mut self, separated: bool) -> Row {
        self.separated = separated;
        self
    }
}

impl RenderOnce for Row {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let spec = Semantic::new(self.id.clone(), Role::ListItem).state(State {
            selected: self.selected,
            ..State::default()
        });
        // The hover transition lives in GPUI's keyed element state, so it
        // survives the re-renders of this value-like element.
        let hover = window.use_keyed_state(self.id.part("hover"), cx, |_, _| Transition::new(0.0));
        let now = Instant::now();
        let strength = hover.update(cx, |hover, _| hover.tick(now, window));
        let mut row = div()
            .id(self.id.element_id())
            .flex()
            .items_center()
            .gap(ROW_GAP)
            .px(ROW_PADDING_X)
            .py(ROW_PADDING_Y)
            .min_h(ROW_MIN_HEIGHT)
            .cursor_default()
            .text_role(TextRole::Body, window, cx);
        if self.separated {
            row = row
                .border_t(SEPARATOR_WIDTH)
                .border_color(palette.separator);
        }
        if self.selected {
            row = row
                .bg(palette.selected_content_background)
                .text_color(on_accent());
        } else if strength > 0.0 {
            row = row.bg(palette.label.alpha(HOVER_ALPHA * strength));
        }
        row = row.on_hover(move |hovered, window, cx| {
            let (target, motion) = if *hovered {
                (1.0, HOVER_IN)
            } else {
                (0.0, HOVER_OUT)
            };
            hover.update(cx, |hover, cx| hover.go(target, motion, Instant::now(), cx));
            window.refresh();
        });
        if let Some(on_click) = self.on_click {
            row = row.on_click(move |_, window, cx| on_click(window, cx));
        }
        semantic::declare(spec, row.children(self.children))
    }
}
