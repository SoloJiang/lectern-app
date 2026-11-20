//! This crate's actions and key bindings (docs/design/file_reader.md「界面」
//! 「Action 与快捷键」). Every action has a stable name the probe can
//! trigger; the bindings travel to `workspace` as the `KeyBindings`
//! contribution. The key context of the reading view's root element is
//! `reader`; the predicates name the region of `workspace` above it.
//!
//! The edit mode's actions are `Edit`, `Save` and the four of the row under
//! the toolbar. ⌘E and ⌘S are bound only where the edit mode is reachable
//! (`edit::EDIT_REACHABLE`): the leave guard is step 8b. The Files tree's
//! actions are the ones named `Files…` and
//! `RevealCurrent`: its key context is `files`, in the sidebar's region.
//! `ShowFiles` of the design is the Files entry's `WorkspaceCommand`
//! (`ShowSidebarPanel`), not an action: nothing but the entry shows the
//! panel, and no view exists yet to take an action before it is shown.

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
        /// While editing the editor has the key first, and what is left is
        /// "leave edit mode".
        Close,
        /// ⌘E: enters edit mode, or leaves it.
        Edit,
        /// ⌘S: saves the buffer (only while editing).
        Save,
        /// The row's button: the disk's changes come into the buffer.
        Merge,
        /// The row's button: the buffer goes, the file is read again.
        DiscardMine,
        /// The row's button: keep the buffer, the file stays changed.
        KeepEditing,
        /// Save Anyway, after Keep Editing: replaces the file as it is.
        SaveAnyway,
        /// ↓ in Files: the next row; stops at the end.
        FilesNext,
        /// ↑ in Files: the previous row; stops at the start.
        FilesPrevious,
        /// → in Files: opens the directory; open already, goes to its first
        /// row.
        FilesExpand,
        /// ← in Files: closes the directory; closed already (or a file),
        /// goes to the directory that holds the row.
        FilesCollapse,
        /// ↵ in Files: shows the file in the reading view; a directory opens
        /// or closes.
        FilesOpen,
        /// Files: opens the directories above the file the reader showed
        /// last and selects it. Files does it when it appears.
        RevealCurrent,
    ]
);

/// The `KeyContext` of the view's root element.
pub const READER: &str = "reader";

/// The `KeyContext` of the Files tree's root element.
pub const FILES: &str = "files";

/// The Files tree takes the arrows and ↵ while it has the focus. Esc is not
/// bound: `workspace`'s Esc closes the panel, which is the way back to the
/// project sidebar.
const IN_FILES: &str = "Sidebar > files";

/// The keys of the view work from the find field as well: the field is a
/// single line, and it has no use for them.
const IN_READER_OR_FIELD: &str = "Content > reader";
/// ⌘[ and ⌘] are the editor's Outdent and Indent while editing: the reader's
/// history keys are not bound then (the root element's context has `editing`).
const IN_READER_NOT_EDITING: &str = "(Content > reader) && !editing";
/// Esc is the field's own while the focus is in it (it closes the find bar
/// itself, before the key gets here).
const IN_READER: &str = "(Content > reader) && !Input";

/// The ID of the contribution.
pub const KEY_BINDINGS_ID: &str = "file-reader";

/// Keystrokes (GPUI key names, not interface text), the action and the
/// context predicate.
///
/// `edit` is whether the edit mode is reachable: ⌘E and ⌘S exist only then.
pub fn key_bindings(edit: bool) -> Vec<(&'static str, Box<dyn Action>, Option<&'static str>)> {
    let mut bindings: Vec<(&'static str, Box<dyn Action>, Option<&'static str>)> = vec![
        ("cmd-f", Box::new(Find), Some(IN_READER_OR_FIELD)),
        ("cmd-g", Box::new(FindNext), Some(IN_READER_OR_FIELD)),
        (
            "cmd-shift-g",
            Box::new(FindPrevious),
            Some(IN_READER_OR_FIELD),
        ),
        ("cmd-[", Box::new(Back), Some(IN_READER_NOT_EDITING)),
        ("cmd-]", Box::new(Forward), Some(IN_READER_NOT_EDITING)),
        ("escape", Box::new(Close), Some(IN_READER)),
        ("down", Box::new(FilesNext), Some(IN_FILES)),
        ("up", Box::new(FilesPrevious), Some(IN_FILES)),
        ("right", Box::new(FilesExpand), Some(IN_FILES)),
        ("left", Box::new(FilesCollapse), Some(IN_FILES)),
        ("enter", Box::new(FilesOpen), Some(IN_FILES)),
    ];
    if edit {
        bindings.push(("cmd-e", Box::new(Edit), Some(IN_READER_OR_FIELD)));
        bindings.push(("cmd-s", Box::new(Save), Some(IN_READER_OR_FIELD)));
    }
    bindings
}
