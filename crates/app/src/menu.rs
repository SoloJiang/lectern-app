//! The menu bar and the key bindings it shows (docs/design/app.md「界面」,
//! docs/design/interaction.md「菜单栏」). `menus` builds the structure from
//! the strings and nothing else: it runs before the first frame, in the one
//! `set_menus` call of `window.rs` (ADR 0017). Every item only dispatches an
//! action that somebody else handles; this module implements none of them
//! except the three Window items (GPUI 0.3.7 has no action for them).
//!
//! # Key equivalents and the greying of items
//!
//! Read in gpui-pre 0.3.7 and gpui-pre-macos 0.3.7:
//!
//! - `App::set_menus` hands the keymap as it is at that moment to the
//!   platform (`gpui/src/app.rs`, `set_menus`), and the macOS platform makes
//!   an item's key equivalent from the first binding of its action
//!   (`gpui_macos/src/platform.rs`, `create_menu_item`; `find_or_first` takes
//!   the first binding whose context matches and otherwise the first binding
//!   there is, so a context predicate does not hide a binding). Nothing
//!   rebuilds the menu when `bind_keys` runs later.
//! - A binding without modifier keys becomes a bare key equivalent (an empty
//!   modifier mask). The view returns NO for a key equivalent that GPUI did
//!   not handle (`gpui_macos/src/window.rs`, `handle_key_event`, the
//!   `key_equivalent && modifiers != function` branch), so AppKit then asks
//!   the menu, and the menu validates an item through
//!   `Window::is_action_available`, which looks for a listener on the focus
//!   path (`gpui/src/key_dispatch.rs`, `is_action_available`). The note
//!   editor is inside the review screen, so the listener is on its path: a bare
//!   J in the editor would run "Next Change" and not type a letter.
//!
//! So the shortcuts that are shown are exactly the ones `key_bindings` binds
//! before `set_menus`, and J, K, V and C are never bound there: their items
//! show no key. The screen binds them itself after the first frame
//! (`review_view::actions::key_bindings`, with the predicate `!Input`).
//! Rebuilding the menu later (a language switch) would read those bindings and
//! must not happen without a way to hide them.
//!
//! A menu item is enabled while an action listener exists on the focus path,
//! or a global listener exists (`App::is_action_available`). The Review, Edit
//! and Close Project items have only handlers inside views, so they are grey
//! where those views are not focused. The Window items and the app menu use
//! global listeners and are always enabled.

use gpui::{
    Action, App, KeyBinding, KeyBindingContextPredicate, Menu, MenuItem, OsAction, Window, actions,
};
use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use lectern_i18n::{Strings, keys};
use lectern_review_view::actions::{self as review, SendNotes};
use lectern_workspace::actions::{CloseProject, CloseWindow, OpenFolder};

use crate::quit::Quit;
use crate::window::{Hide, HideOthers, ShowAll};

actions!(lectern, [Minimize, Zoom, BringAllToFront]);

/// The `KeyContext` of a text box (`gpui_component`'s `Input`).
const INPUT: &str = "Input";

/// A keystroke in GPUI's spelling, the action and the context predicate.
pub(crate) type Binding = (&'static str, Box<dyn Action>, Option<&'static str>);

/// The menu bar: app menu, File, Edit, View, Review, Window.
pub(crate) fn menus(strings: Strings) -> Vec<Menu> {
    let title = |key| strings.get(key);
    vec![
        Menu::new("Lectern").items([
            MenuItem::action(title(keys::app::MENU_HIDE), Hide),
            MenuItem::action(title(keys::app::MENU_HIDE_OTHERS), HideOthers),
            MenuItem::action(title(keys::app::MENU_SHOW_ALL), ShowAll),
            MenuItem::separator(),
            MenuItem::action(title(keys::app::MENU_QUIT), Quit),
        ]),
        Menu::new(title(keys::app::MENU_FILE)).items([
            MenuItem::action(title(keys::app::MENU_OPEN_FOLDER), OpenFolder),
            MenuItem::separator(),
            // Only the root view of a window with a project handles it, so on
            // Welcome the item is grey (workspace design「Action 与快捷键」).
            MenuItem::action(title(keys::app::MENU_CLOSE_PROJECT), CloseProject),
            MenuItem::action(title(keys::app::MENU_CLOSE_WINDOW), CloseWindow),
        ]),
        // The actions the text boxes handle; the item is grey while no text
        // box has the focus.
        Menu::new(title(keys::app::MENU_EDIT)).items([
            MenuItem::os_action(title(keys::app::MENU_UNDO), Undo, OsAction::Undo),
            MenuItem::os_action(title(keys::app::MENU_REDO), Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action(title(keys::app::MENU_CUT), Cut, OsAction::Cut),
            MenuItem::os_action(title(keys::app::MENU_COPY), Copy, OsAction::Copy),
            MenuItem::os_action(title(keys::app::MENU_PASTE), Paste, OsAction::Paste),
            MenuItem::os_action(
                title(keys::app::MENU_SELECT_ALL),
                SelectAll,
                OsAction::SelectAll,
            ),
        ]),
        // The review screen's two commands that have no control on screen; macOS
        // greys an item out while no view on the focus path handles its action.
        Menu::new(title(keys::app::MENU_VIEW)).items([
            MenuItem::action(
                title(keys::app::MENU_TOGGLE_DIFF_MODE),
                review::ToggleDiffMode,
            ),
            MenuItem::action(title(keys::app::MENU_REFRESH), review::Refresh),
        ]),
        // Only the review screen handles these, so they are grey elsewhere.
        // J, K, V and C are shown without a key (see the module comment).
        Menu::new(title(keys::app::MENU_REVIEW)).items([
            MenuItem::action(title(keys::app::MENU_NEXT_CHANGE), review::NextHunk),
            MenuItem::action(title(keys::app::MENU_PREVIOUS_CHANGE), review::PreviousHunk),
            MenuItem::action(
                title(keys::app::MENU_MARK_REVIEWED),
                review::MarkSeenAndNext,
            ),
            MenuItem::action(title(keys::app::MENU_ADD_NOTE), review::AddNote),
            MenuItem::separator(),
            MenuItem::action(title(keys::app::MENU_SEND_NOTES), SendNotes),
        ]),
        // GPUI's macOS platform registers a menu titled exactly "Window" as the
        // system windows menu; the zh-CN title does not match, so there it is a
        // plain menu.
        Menu::new(title(keys::app::MENU_WINDOW)).items([
            MenuItem::action(title(keys::app::MENU_MINIMIZE), Minimize),
            MenuItem::action(title(keys::app::MENU_ZOOM), Zoom),
            MenuItem::separator(),
            MenuItem::action(title(keys::app::MENU_BRING_ALL_TO_FRONT), BringAllToFront),
        ]),
    ]
}

/// The bindings that make the menu show its shortcuts: bound before
/// `set_menus`, because the menu reads the keymap once. Every one has a
/// modifier key; none is for J, K, V or C. The owners bind most of these
/// again later with the same keys and contexts, which changes nothing.
pub(crate) fn key_bindings() -> Vec<Binding> {
    // ⌘↩ with the predicate the review screen gives it, not a looser one.
    let send_notes = review::key_bindings()
        .into_iter()
        .filter(|(_, action, _)| action.as_any().is::<SendNotes>());
    let mut bindings: Vec<Binding> = vec![
        ("cmd-o", Box::new(OpenFolder), None),
        ("cmd-w", Box::new(CloseWindow), None),
        ("cmd-m", Box::new(Minimize), None),
        // The text boxes' own keys and context (`gpui_base::input`, `init`).
        ("cmd-z", Box::new(Undo), Some(INPUT)),
        ("cmd-shift-z", Box::new(Redo), Some(INPUT)),
        ("cmd-x", Box::new(Cut), Some(INPUT)),
        ("cmd-c", Box::new(Copy), Some(INPUT)),
        ("cmd-v", Box::new(Paste), Some(INPUT)),
        ("cmd-a", Box::new(SelectAll), Some(INPUT)),
    ];
    bindings.extend(send_notes);
    bindings
}

/// The `KeyBinding`s of `key_bindings`. A binding that does not parse is a
/// programming error, caught by the unit test.
pub(crate) fn load_bindings(bindings: Vec<Binding>, cx: &App) -> Vec<KeyBinding> {
    bindings
        .into_iter()
        .filter_map(|(keys, action, context)| {
            let predicate = match context {
                Some(context) => Some(KeyBindingContextPredicate::parse(context).ok()?.into()),
                None => None,
            };
            KeyBinding::load(
                keys,
                action,
                predicate,
                false,
                None,
                cx.keyboard_mapper().as_ref(),
            )
            .ok()
        })
        .collect()
}

/// The handlers of the three Window items. They are global listeners, so the
/// items are always enabled; `Window::minimize_window` and `zoom_window` act
/// on the active window, and activating the application brings all its
/// windows forward (GPUI has no `arrangeInFront:`).
pub(crate) fn on_window_actions(cx: &mut App) {
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, Window::minimize_window));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, Window::zoom_window));
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
}

fn with_active_window(cx: &mut App, f: fn(&Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| f(window)).ok();
    }
}
