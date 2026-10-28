//! The flat sidebar-style row (docs/design/ui.md「第二批控件」): no card, no
//! separator line, radius 8pt. The default is Welcome's right column (10 ×
//! 8pt, children stacked 2pt apart); `compact` is the workspace sidebar's
//! (28pt high, 8pt sides, children side by side 8pt apart, radius 6pt),
//! whose `trailing` text (a count) sits at the right in the secondary
//! colour. Hovered it shows `fill`; selected it is `fill_strong`, or with
//! `accent` the accent with `on_accent` text. A `Row` outside a
//! `GroupedList` is the inspector's compact list row; this is the sidebar's.

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
pub struct SidebarRow {
    id: NodeId,
    selected: bool,
    accent: bool,
    compact: bool,
    trailing: Option<Text>,
    on_click: Option<ClickHandler>,
    menu: Option<ContextMenu>,
    children: Vec<AnyElement>,
}

const RADIUS: Pixels = px(8.0);
const PADDING_X: Pixels = px(10.0);
const PADDING_Y: Pixels = px(8.0);
/// The gap between the stacked children of the default form.
const GAP: Pixels = px(2.0);
/// The gap between the content and the trailing text.
const TRAILING_GAP: Pixels = px(8.0);
const COMPACT_RADIUS: Pixels = px(6.0);
const COMPACT_HEIGHT: Pixels = px(28.0);
const COMPACT_PADDING_X: Pixels = px(8.0);
const COMPACT_GAP: Pixels = px(8.0);

impl SidebarRow {
    pub fn new(id: NodeId) -> SidebarRow {
        SidebarRow {
            id,
            selected: false,
            accent: false,
            compact: false,
            trailing: None,
            on_click: None,
            menu: None,
            children: Vec::new(),
        }
    }

    pub fn selected(mut self, selected: bool) -> SidebarRow {
        self.selected = selected;
        self
    }

    /// Selection on the accent (`selected_content_background`, text in
    /// `on_accent`): a focused source list. The second line of text takes
    /// `on_accent` at 85 % from the caller.
    pub fn accent(mut self) -> SidebarRow {
        self.accent = true;
        self
    }

    /// The workspace sidebar's form: 28pt high, children in one line.
    pub fn compact(mut self) -> SidebarRow {
        self.compact = true;
        self
    }

    /// Text at the row's end in the secondary colour (a count).
    pub fn trailing(mut self, text: Text) -> SidebarRow {
        self.trailing = Some(text);
        self
    }

    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> SidebarRow {
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
    ) -> SidebarRow {
        self.menu = Some(ContextMenu::new(items, select));
        self
    }

    pub fn child(mut self, child: impl IntoElement) -> SidebarRow {
        self.children.push(child.into_any_element());
        self
    }
}

impl RenderOnce for SidebarRow {
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
        row = row.gap(TRAILING_GAP);
        row = if self.compact {
            row.h(COMPACT_HEIGHT)
                .px(COMPACT_PADDING_X)
                .rounded(COMPACT_RADIUS)
        } else {
            row.px(PADDING_X).py(PADDING_Y).rounded(RADIUS)
        };
        if self.selected {
            row = if self.accent {
                row.bg(palette.selected_content_background)
                    .text_color(on_accent())
            } else {
                row.bg(palette.fill_strong)
            };
        } else if hover.strength > 0.0 {
            row = row.bg(palette.fill.alpha(palette.fill.a * hover.strength));
        }
        row = row.on_hover(hover.listener());
        if let Some(on_click) = self.on_click {
            row = row.on_click(move |_, window, cx| on_click(window, cx));
        }
        if let Some(menu) = &self.menu {
            row = row.on_mouse_down(MouseButton::Right, menu.on_right_down());
        }
        let content = div().flex_1().min_w_0().flex().children(self.children);
        let content = if self.compact {
            content.items_center().gap(COMPACT_GAP)
        } else {
            content.flex_col().gap(GAP)
        };
        let row = row.child(content);
        let row = match self.trailing {
            Some(trailing) => {
                let colour = if self.selected && self.accent {
                    on_accent()
                } else {
                    palette.secondary_label
                };
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_role(TextRole::Callout, window, cx)
                        .text_color(colour)
                        .child(trailing.shared()),
                )
            }
            None => row,
        };
        semantic::declare(spec, row)
    }
}
