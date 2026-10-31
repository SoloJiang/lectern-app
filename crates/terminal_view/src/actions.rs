//! This crate's actions and key bindings (docs/design/terminal_view.md「界面」
//! 「Action 与快捷键」). This module holds nothing but the `actions!` call:
//! it defines a struct named `Copy`, which must not share a module with a
//! `#[derive(Copy)]`.

use gpui::{Action, NoAction, actions};

actions!(
    terminal_view,
    [
        /// ⌘C: the selection to the clipboard.
        Copy,
        /// ⌘V: the clipboard's text to the program.
        Paste,
        /// ⌘A: select the whole screen and the scrollback.
        SelectAll,
        /// Clear the scrollback (no key; the context menu is a later issue).
        ClearScrollback,
    ]
);

/// The `KeyContext` of the root element.
pub const KEY_CONTEXT: &str = "terminal";

/// The bindings the host plugin puts into its `KeyBindings` contribution.
/// One level of context: `terminal` is deeper than every ancestor's.
/// Esc is bound to `NoAction`: GPUI lets a `NoAction` of a deeper context
/// disable the same key in a shallower one, which stops `workspace::Escape`
/// and lets Esc reach `on_key_down`.
pub fn key_bindings() -> Vec<(&'static str, Box<dyn Action>, Option<&'static str>)> {
    vec![
        ("cmd-c", Box::new(Copy), Some(KEY_CONTEXT)),
        ("cmd-v", Box::new(Paste), Some(KEY_CONTEXT)),
        ("cmd-a", Box::new(SelectAll), Some(KEY_CONTEXT)),
        ("escape", Box::new(NoAction), Some(KEY_CONTEXT)),
    ]
}
