//! This crate's actions and key bindings (docs/design/review_view.md「界面」
//! 「Action 与快捷键」). Every action has a stable name the probe can
//! trigger; the bindings travel to `workspace` as the `KeyBindings`
//! contribution. The key context of the screen's root element is `review`,
//! of the Progress page `review-progress`; the predicates name the region
//! above them (`Content`, `Inspector`).
//!
//! C (a review note) and ⌘↩ (send) are #22's; ⌘↩ and Esc inside the note
//! editor are the text box's (`ui::TextArea`), which dispatches `SaveNote`
//! and `CancelNote`.

use gpui::{Action, actions};

actions!(
    review,
    [
        /// J: select the next hunk, across files and turns.
        NextHunk,
        /// K: select the previous hunk.
        PreviousHunk,
        /// V: mark the current file as seen and go to the next unseen one.
        MarkSeenAndNext,
        /// Toggle the current file between seen and not seen.
        ToggleSeen,
        /// The bar's button: mark every file as seen.
        LooksGood,
        /// Show or hide the folded noise files.
        ToggleNoise,
        /// Show or hide the turns before the latest seen mark.
        ToggleEarlierTurns,
        /// Compare the working tree again.
        Refresh,
        /// ⇧⌘O: the selected row in the full file.
        OpenFile,
        /// ⌘E: the same (editing starts in the reader).
        Edit,
        /// Space: the selected row in the context panel.
        OpenContext,
        /// Switch between the unified and the side-by-side diff.
        ToggleDiffMode,
        /// ↵ on a fold (or on a hunk or row above one): show the folded lines.
        ExpandFold,
        /// Start an analysis of the review.
        Analyze,
        /// Stop the analysis that is running.
        CancelAnalyze,
        /// Esc: the panel first, then the selection, then back.
        Escape,
        /// ↓ in the Progress list.
        SelectNext,
        /// ↑ in the Progress list.
        SelectPrevious,
        /// ↵ in the Progress list: go to the file's step.
        JumpToSelected,
        /// The failed state's button.
        Back,
        /// C, or the + at the end of a row: write a review note on the
        /// selected row or hunk.
        AddNote,
        /// ⌘↩: send the pending review notes the first way there is.
        SendNotes,
        /// Send the pending review notes through the clipboard (the bar's
        /// fallback after a failed send into the session).
        CopyNotesAsPrompt,
        /// Throw the pending review notes away, after asking.
        DiscardNotes,
        /// Copy the batch sent last again.
        CopyNotesAgain,
        /// ⌘↩ in the editor: save the note.
        SaveNote,
        /// Esc in the editor: close it (a new note's text is kept as a draft).
        CancelNote,
        /// Change the selected note.
        EditNote,
        /// Delete the selected note.
        DeleteNote,
    ]
);

/// The `KeyContext` of the screen's root element.
pub const REVIEW: &str = "review";
/// The `KeyContext` of the Progress page.
pub const PROGRESS: &str = "review-progress";

/// The predicates of the bindings: the region of `workspace` above the
/// screen's own context. gpui tries a binding at every depth of the focused
/// element's context path, so `A > B` matches while the focus is below the
/// screen's root as well; the screen's keys are letters (J, K, V, C) and
/// Space, which the note editor's text box types, so its `Input` context is
/// left out (`!Input`; the parentheses matter, `>` binds weaker than `&&`).
const IN_REVIEW: &str = "(Content > review) && !Input";
const IN_PROGRESS: &str = "Inspector > review-progress";

/// The ID of the contribution.
pub const KEY_BINDINGS_ID: &str = "review";

/// Keystrokes (GPUI key names, not interface text), the action and the
/// context predicate.
pub fn key_bindings() -> Vec<(&'static str, Box<dyn Action>, Option<&'static str>)> {
    vec![
        ("j", Box::new(NextHunk), Some(IN_REVIEW)),
        ("k", Box::new(PreviousHunk), Some(IN_REVIEW)),
        ("v", Box::new(MarkSeenAndNext), Some(IN_REVIEW)),
        ("v", Box::new(MarkSeenAndNext), Some(IN_PROGRESS)),
        ("cmd-shift-o", Box::new(OpenFile), Some(IN_REVIEW)),
        ("cmd-e", Box::new(Edit), Some(IN_REVIEW)),
        ("space", Box::new(OpenContext), Some(IN_REVIEW)),
        ("enter", Box::new(ExpandFold), Some(IN_REVIEW)),
        ("escape", Box::new(Escape), Some(IN_REVIEW)),
        ("c", Box::new(AddNote), Some(IN_REVIEW)),
        ("cmd-enter", Box::new(SendNotes), Some(IN_REVIEW)),
        ("down", Box::new(SelectNext), Some(IN_PROGRESS)),
        ("up", Box::new(SelectPrevious), Some(IN_PROGRESS)),
        ("enter", Box::new(JumpToSelected), Some(IN_PROGRESS)),
    ]
}
