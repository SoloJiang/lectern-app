//! This crate's actions and key bindings (docs/design/conversation.md「Action
//! 与快捷键」). Every action has a stable name the probe can trigger. The
//! page's root view carries the `conversation` context.

use gpui::{Action, actions};

actions!(
    conversation,
    [
        /// ⌘.: asks the agent to stop the turn that is running. Ignored when
        /// no turn is running.
        Stop,
        /// The page switch's Messages item.
        ShowMessages,
        /// The page switch's Terminal item.
        ShowTerminal,
        /// The earlier-turns button: reads the history window further back.
        LoadEarlier,
        /// The plan's line: opens or closes the steps.
        TogglePlan,
        /// "Dismiss" on the notice about a failed link.
        Dismiss,
    ]
);

/// The key context of the page's root.
pub(crate) const CONTEXT: &str = "conversation";

/// The ID of the `KeyBindings` contribution.
pub(crate) const KEY_BINDINGS_ID: &str = "conversation";

/// The bindings, in the shape of `ui::contrib::KeyBindings`. The keystrokes
/// are GPUI key names, not interface text.
pub(crate) fn key_bindings() -> Vec<(&'static str, Box<dyn Action>, Option<&'static str>)> {
    vec![("cmd-.", Box::new(Stop), Some(CONTEXT))]
}
