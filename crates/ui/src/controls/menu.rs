//! System menus (docs/design/ui.md「第二批控件」): `MenuItem`, `show_menu`
//! and `PopUpButton`. gpui-component's own `PopupMenu` and `ContextMenu`
//! draw the menu themselves, unlike macOS and clipped by the window, so
//! the menu is `NativeMenu` (an `NSMenu` on macOS): drawn by the system,
//! it can extend past the window. A selected item dispatches its `Action`
//! with `Window::dispatch_action`, the path the menu bar and the key
//! bindings take. The items are not probe nodes: the system menu is not in
//! GPUI's element tree.

use std::rc::Rc;

use gpui::{
    Action, App, Bounds, InteractiveElement, IntoElement, MouseDownEvent, ParentElement, Pixels,
    Point, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, px,
};
use gpui_component::native_menu::NativeMenu;

use crate::controls::require_ready;
use crate::font::{TextRole, TextRoleExt};
use crate::icon::Icon;
use crate::palette::colors;
use crate::semantic::{self, NodeId, Role, Semantic};
use crate::text::Text;

type SelectHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// One item of a system menu: selecting it dispatches `action` to the
/// window, like the menu bar and the key bindings.
pub struct MenuItem {
    pub text: Text,
    pub action: Box<dyn Action>,
    /// Drawn with a check mark.
    pub checked: bool,
}

impl Clone for MenuItem {
    fn clone(&self) -> MenuItem {
        MenuItem {
            text: self.text.clone(),
            action: self.action.boxed_clone(),
            checked: self.checked,
        }
    }
}

/// Pops up a system menu at `position` (window coordinates): the right
/// click of a row (`Row::context_menu`, `SidebarRow::context_menu`) and the
/// menu of a button that pulls one down (`usage_view`'s「合并到…」) both
/// use it. The selected item is dispatched with `Window::dispatch_action`,
/// from the window's current focus, so the screen's `on_action` must be on
/// the focus path. A choice that takes an argument is an `Action` with
/// fields. An empty `items` shows nothing.
pub fn show_menu(items: Vec<MenuItem>, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
    let menu = items.into_iter().fold(NativeMenu::new(), |menu, item| {
        menu.menu_with_check(item.text.shared(), item.checked, item.action)
    });
    menu.show(position, window, cx);
}

/// What a row holds for its right click: the items, and the callback that
/// moves the selection to the row first. The menu's actions carry no row
/// identity: they act on the selection, so `select` runs before the menu
/// opens.
#[derive(Clone)]
pub(crate) struct ContextMenu {
    items: Rc<Vec<MenuItem>>,
    select: SelectHandler,
}

impl ContextMenu {
    pub(crate) fn new(
        items: Vec<MenuItem>,
        select: impl Fn(&mut Window, &mut App) + 'static,
    ) -> ContextMenu {
        ContextMenu {
            items: Rc::new(items),
            select: Rc::new(select),
        }
    }

    /// `select`, then the menu at the pointer.
    pub(crate) fn open(&self, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        (self.select)(window, cx);
        show_menu(self.items.as_ref().clone(), position, window, cx);
    }

    /// The listener for the right mouse button.
    pub(crate) fn on_right_down(
        &self,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        let menu = self.clone();
        move |event, window, cx| menu.open(event.position, window, cx)
    }
}

/// A pop-up button (Welcome's language choice): 24pt high on `fill_strong`,
/// radius 6pt, the current choice in `Callout` and a `ChevronDown`. A click
/// pops the system menu up at the button's bottom-left corner; the checked
/// item carries the check mark.
#[derive(IntoElement)]
pub struct PopUpButton {
    id: NodeId,
    label: Text,
    current: Text,
    value: Option<SharedString>,
    items: Vec<MenuItem>,
}

const HEIGHT: Pixels = px(24.0);
const RADIUS: Pixels = px(6.0);
const PADDING_X: Pixels = px(6.0);
const GAP: Pixels = px(4.0);
const CHEVRON: Pixels = px(12.0);

impl PopUpButton {
    /// `label` is the accessible name and the probe node's label; `current`
    /// is the text on the button (the current choice).
    pub fn new(id: NodeId, label: Text, current: Text, items: Vec<MenuItem>) -> PopUpButton {
        PopUpButton {
            id,
            label,
            current,
            value: None,
            items,
        }
    }

    /// The probe node's `value` attribute: what the choice is now, which a
    /// probe reads after it has triggered an item's action.
    pub fn value(mut self, value: SharedString) -> PopUpButton {
        self.value = Some(value);
        self
    }
}

impl RenderOnce for PopUpButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = *colors(window, cx);
        let mut spec = Semantic::new(self.id.clone(), Role::Button).label(&self.label);
        if let Some(value) = &self.value {
            spec = spec.attr("value", value.clone());
        }
        // The button's own bounds, kept from the last prepaint: the menu
        // opens at its bottom-left corner.
        let bounds = window.use_keyed_state(self.id.part("bounds"), cx, |_, _| {
            Bounds::<Pixels>::default()
        });
        let recorded = bounds.clone();
        let anchor = canvas(
            move |bounds, _, cx| recorded.update(cx, |recorded, _| *recorded = bounds),
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0();
        let items = self.items;
        let button = div()
            .id(self.id.element_id())
            .relative()
            .h(HEIGHT)
            .px(PADDING_X)
            .flex()
            .items_center()
            .gap(GAP)
            .rounded(RADIUS)
            .bg(palette.fill_strong)
            .cursor_default()
            .text_role(TextRole::Callout, window, cx)
            .text_color(palette.label)
            .whitespace_nowrap()
            .child(anchor)
            .child(self.current.shared())
            .child(
                gpui_component::Icon::new(Icon::ChevronDown)
                    .size(CHEVRON)
                    .text_color(palette.secondary_label),
            )
            .on_click(move |_, window, cx| {
                let corner = bounds.read(cx).bottom_left();
                show_menu(items.clone(), corner, window, cx);
            });
        semantic::declare(spec, button)
    }
}
