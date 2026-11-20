//! This crate's actions and key bindings (docs/design/file_reader.md「界面」
//! 「Action 与快捷键」). Every action has a stable name the probe can
//! trigger; the bindings travel to `workspace` as the `KeyBindings`
//! contribution. The key context of the reading view's root element is
//! `reader`; the predicates name the region of `workspace` above it.
//!
//! Edit, Save and the other actions of the edit mode are the edit mode's
//! (step 8); Files' are step 7b's.

use gpui::{Action, actions};

actions!(
    file_reader,
    [
        /// ⌘F: open the find bar and put the cursor in its field.
        Find,
        /// ⌘G (and ↵ in the field): the next match.
        FindNext,
        /// ⇧⌘G (and ⇧↵ in the field): the previous match.
        FindPrevious,
        /// ⌘[: the previous position in the history; at its start, back to
        /// the view that opened the reader.
        Back,
        /// ⌘]: the next position in the history.
        Forward,
        /// Esc: the find bar first; with none open the key goes on to
        /// `workspace`, which goes back to the view that opened the reader.
        Close,
    ]
);

/// The `KeyContext` of the view's root element.
pub const READER: &str = "reader";

/// The keys of the view work from the find field as well: the field is a
/// single line, and it has no use for them.
const IN_READER_OR_FIELD: &str = "Content > reader";
/// Esc is the field's own while the focus is in it (it closes the find bar
/// itself, before the key gets here).
const IN_READER: &str = "(Content > reader) && !Input";

/// The ID of the contribution.
pub const KEY_BINDINGS_ID: &str = "file-reader";

/// Keystrokes (GPUI key names, not interface text), the action and the
/// context predicate.
pub fn key_bindings() -> Vec<(&'static str, Box<dyn Action>, Option<&'static str>)> {
    vec![
        ("cmd-f", Box::new(Find), Some(IN_READER_OR_FIELD)),
        ("cmd-g", Box::new(FindNext), Some(IN_READER_OR_FIELD)),
        (
            "cmd-shift-g",
            Box::new(FindPrevious),
            Some(IN_READER_OR_FIELD),
        ),
        ("cmd-[", Box::new(Back), Some(IN_READER_OR_FIELD)),
        ("cmd-]", Box::new(Forward), Some(IN_READER_OR_FIELD)),
        ("escape", Box::new(Close), Some(IN_READER)),
    ]
}
