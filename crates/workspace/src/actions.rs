//! This crate's actions and key bindings (docs/design/workspace.md「界面」
//! 「Action 与快捷键」). Every action has a stable name the probe can
//! trigger; the keystrokes are bound once in `install`, the handlers live on
//! the root view. Contributed `KeyBindings` bind their keys to
//! [`Command`], the proxy of a command-activated plugin's command.

use gpui::{Action, KeyBinding, actions};

actions!(
    workspace,
    [
        /// ⌘O: `PromptOpenProject`.
        OpenFolder,
        /// ⌘W: `CloseWindow` (through the leave guards).
        CloseWindow,
        /// ⌘[ when the screen did not handle it: `PopToBase`.
        Back,
        /// Esc when the screen did not handle it: `Escape`.
        Escape,
        /// ⌥⌘I: `SetInspector(!inspector)`.
        ToggleInspector,
        /// ⌘\: `SelectInspectorTab("conversation")`.
        ShowConversation,
    ]
);

/// The proxy of a command activated by `Activation::Command` (kernel
/// design「按命令激活」): contributed `KeyBindings` bind to it; the root view
/// finds the handler or activates the plugin.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = workspace, no_json)]
pub struct Command {
    pub name: String,
}

/// The `KeyContext` names (「Action 与快捷键」): the root view, with
/// `project` added while a project is open, and the regions.
pub const WORKSPACE: &str = "Workspace";
pub const PROJECT: &str = "project";
pub const SIDEBAR: &str = "Sidebar";
pub const CONTENT: &str = "Content";
pub const INSPECTOR: &str = "Inspector";
pub const PANEL: &str = "Panel";
pub const OVERLAY: &str = "Overlay";

/// The inspector tab ⌘\ selects.
pub const CONVERSATION_TAB: &str = "conversation";

/// The bindings `install` adds to the keymap. The keystrokes are GPUI key
/// names, not interface text.
///
/// ⌥⌘I and ⌘\ are bound without a context, not on `Workspace > project`
/// (「与设计的差异」第 2 步): GPUI's `>` wants the right-hand identifier on
/// the deepest context of the dispatch path, which the regions' own
/// contexts occupy; and GPUI resolves a keystroke by the depth at which the
/// predicate matches, so a context-free binding (gpui-component's debug
/// inspector binds ⌥⌘I that way) would shadow one on `Workspace`. Among
/// context-free bindings the later one wins. The state machine ignores both
/// commands without an inspector, which the design asks for anyway.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-o", OpenFolder, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-[", Back, Some(WORKSPACE)),
        KeyBinding::new("escape", Escape, Some(WORKSPACE)),
        KeyBinding::new("alt-cmd-i", ToggleInspector, None),
        KeyBinding::new("cmd-\\", ShowConversation, None),
    ]
}
