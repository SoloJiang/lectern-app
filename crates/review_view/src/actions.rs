//! This crate's actions and key bindings (docs/design/review_view.md「界面」
//! 「Action 与快捷键」). Every action has a stable name the probe can
//! trigger; the bindings travel to `workspace` as the `KeyBindings`
//! contribution. The key context of the screen's root element is `review`,
//! of the Progress page `review-progress`; the predicates name the region
//! above them (`Content`, `Inspector`).
//!
//! C (a review comment) and ⌘↩ (send) come with #22; ⇧⌘O, ⌘E, Space, the
//! diff mode, folds and Analyze with step 2.

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
    ]
);

/// The `KeyContext` of the screen's root element.
pub const REVIEW: &str = "review";
/// The `KeyContext` of the Progress page.
pub const PROGRESS: &str = "review-progress";

/// The predicates of the bindings: the region of `workspace` above the
/// screen's own context. `A > B` wants `B` on the deepest context of the
/// dispatch path, which the screen's root is as long as it holds the focus.
const IN_REVIEW: &str = "Content > review";
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
        ("escape", Box::new(Escape), Some(IN_REVIEW)),
        ("down", Box::new(SelectNext), Some(IN_PROGRESS)),
        ("up", Box::new(SelectPrevious), Some(IN_PROGRESS)),
        ("enter", Box::new(JumpToSelected), Some(IN_PROGRESS)),
    ]
}
