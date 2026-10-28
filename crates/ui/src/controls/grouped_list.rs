//! The grouped list (interaction spec「视觉」,「分组」): a card of 12pt radius
//! on `control_background` with a 0.5pt `separator` ring, so its edge shows
//! on any page background; rows 16 × 14pt apart by 0.5pt `separator` lines
//! that start at the text (`separator_inset`), under a 13pt semibold
//! `secondary_label` header. A `Row` fades its hover background in
//! (`HOVER_IN`) and out (`HOVER_OUT`). Selection is quiet
//! (`unemphasized_selected_content_background`) unless the row asks for the
//! accent (`Row::accent`, a focused source list); its content is the
//! caller's (agent dot, lines of text, progress ring). A `Row` outside a
//! `GroupedList` is a compact list row (30pt, radius 6, the inspector's).

use std::rc::Rc;

use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels,
    RenderOnce, StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::controls::hover::Hover;
use crate::controls::menu::{ContextMenu, MenuItem};
use crate::controls::require_ready;
use crate::font::{TextRole, TextRoleExt};
use crate::palette::{colors, on_accent};
use crate::semantic::{self, NodeId, Role, Semantic, State};
use crate::text::Text;

type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct GroupedList {
    id: NodeId,
    header: Option<Text>,
    separator_inset: Pixels,
    rows: Vec<Row>,
}

#[derive(IntoElement)]
pub struct Row {
    id: NodeId,
    selected: bool,
    accent: bool,
    in_card: bool,
    on_click: Option<ClickHandler>,
    menu: Option<ContextMenu>,
    children: Vec<AnyElement>,
}

const GROUP_RADIUS: Pixels = px(12.0);
const SEPARATOR_WIDTH: Pixels = px(0.5);
/// The gap between a header and its group, and the header's inset.
const HEADER_GAP: Pixels = px(8.0);
const HEADER_INSET: Pixels = px(12.0);
/// A card row's padding and the gap between its children.
const CARD_PADDING_X: Pixels = px(16.0);
const CARD_PADDING_Y: Pixels = px(14.0);
const CARD_GAP: Pixels = px(14.0);
/// A compact row's (outside a card) padding, gap, least height and the
/// radius of its selection.
const COMPACT_PADDING_X: Pixels = px(6.0);
const COMPACT_GAP: Pixels = px(8.0);
const COMPACT_MIN_HEIGHT: Pixels = px(30.0);
const COMPACT_RADIUS: Pixels = px(6.0);
/// The alpha of the `label`-coloured hover wash at full strength.
const HOVER_ALPHA: f32 = 0.06;

impl GroupedList {
    pub fn new(id: NodeId) -> GroupedList {
        GroupedList {
            id,
            header: None,
            separator_inset: CARD_PADDING_X,
            rows: Vec::new(),
        }
    }

    /// Where the separators start, from the card's left edge: the text of
    /// the rows. The default is the rows' padding (16pt); rows that lead
    /// with an element (a 10pt dot and the 14pt gap: 40pt) give the offset
    /// of their text.
    pub fn separator_inset(mut self, inset: Pixels) -> GroupedList {
        self.separator_inset = inset;
        self
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
        let inset = self.separator_inset;
        let separator = palette.separator;
        let mut rows = Vec::new();
        for (index, row) in self.rows.into_iter().enumerate() {
            if index > 0 {
                rows.push(
                    div()
                        .h(SEPARATOR_WIDTH)
                        .flex_shrink_0()
                        .ml(inset)
                        .bg(separator)
                        .into_any_element(),
                );
            }
            rows.push(row.in_card().into_any_element());
        }
        let group = div()
            .id(self.id.element_id())
            .flex()
            .flex_col()
            .rounded(GROUP_RADIUS)
            .overflow_hidden()
            .relative()
            .bg(palette.control_background)
            .children(rows)
            // The ring is drawn over the rows and takes no room, so the
            // rows keep the full width and the selection fill meets it.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(GROUP_RADIUS)
                    .border_t(SEPARATOR_WIDTH)
                    .border_b(SEPARATOR_WIDTH)
                    .border_l(SEPARATOR_WIDTH)
                    .border_r(SEPARATOR_WIDTH)
                    .border_color(palette.separator),
            );
        let list: AnyElement = match self.header {
            Some(header) => div()
                .flex()
                .flex_col()
                .gap(HEADER_GAP)
                .child(
                    div()
                        .pl(HEADER_INSET)
                        .text_role(TextRole::Headline, window, cx)
                        .text_color(palette.secondary_label)
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
            accent: false,
            in_card: false,
            on_click: None,
            menu: None,
            children: Vec::new(),
        }
    }

    pub fn selected(mut self, selected: bool) -> Row {
        self.selected = selected;
        self
    }

    /// Selection on the accent (`selected_content_background`, text in
    /// `on_accent`) instead of the quiet grey: for a focused source list,
    /// where the macOS standard uses the accent. The row's own text then
    /// takes `on_accent` colours; the default (quiet) keeps `label`.
    pub fn accent(mut self) -> Row {
        self.accent = true;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Row {
        self.on_click = Some(Rc::new(f));
        self
    }

    /// The right-click menu: `select` first (the caller moves the selection
    /// to this row, as a click would, without the click's other effects such
    /// as opening it), then the system menu of `items` at the pointer
    /// (`show_menu`). The items' actions carry no row identity and act on
    /// the selection, so `select` must be done before the menu opens.
    pub fn context_menu(
        mut self,
        items: Vec<MenuItem>,
        select: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Row {
        self.menu = Some(ContextMenu::new(items, select));
        self
    }

    pub fn child(mut self, child: impl IntoElement) -> Row {
        self.children.push(child.into_any_element());
        self
    }

    /// Set by `GroupedList` on its rows: the card form (16 × 14pt) instead
    /// of the compact one.
    fn in_card(mut self) -> Row {
        self.in_card = true;
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
        let hover = Hover::track(&self.id, window, cx);
        let mut row = div()
            .id(self.id.element_id())
            .flex()
            .items_center()
            .cursor_default()
            .text_role(TextRole::Body, window, cx);
        row = if self.in_card {
            row.gap(CARD_GAP).px(CARD_PADDING_X).py(CARD_PADDING_Y)
        } else {
            row.gap(COMPACT_GAP)
                .px(COMPACT_PADDING_X)
                .min_h(COMPACT_MIN_HEIGHT)
                .rounded(COMPACT_RADIUS)
        };
        if self.selected {
            row = if self.accent {
                row.bg(palette.selected_content_background)
                    .text_color(on_accent())
            } else {
                row.bg(palette.unemphasized_selected_content_background)
            };
        } else if hover.strength > 0.0 {
            row = row.bg(palette.label.alpha(HOVER_ALPHA * hover.strength));
        }
        row = row.on_hover(hover.listener());
        if let Some(on_click) = self.on_click {
            row = row.on_click(move |_, window, cx| on_click(window, cx));
        }
        if let Some(menu) = &self.menu {
            row = row.on_mouse_down(MouseButton::Right, menu.on_right_down());
        }
        semantic::declare(spec, row.children(self.children))
    }
}
