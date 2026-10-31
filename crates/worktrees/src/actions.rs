//! This crate's actions and key bindings (docs/design/worktrees.md「Action 与
//! 快捷键」). Every action has a stable name the probe can trigger. The
//! bindings are contributed as `KeyBindings` with the predicate
//! `Content > worktrees`: `workspace` puts the screen in its `Content`
//! region, and the screen's root view carries the `worktrees` context and the
//! focus. The inspector's page has no focus, so every action, the ones its
//! clicks stand for included, is handled by the screen.

use gpui::{Action, actions};
use lectern_services::project::WorktreeKey;
use lectern_services::session::SessionKey;

actions!(
    worktrees,
    [
        /// ↓: the next worktree, across Active and Idle; stops at the end.
        SelectNext,
        /// ↑: the previous worktree; stops at the start.
        SelectPrevious,
        /// ⌥↑: the first worktree.
        SelectFirst,
        /// ⌥↓: the last worktree.
        SelectLast,
        /// ↵: reviews the selected worktree's changes against the base;
        /// ignored for a prunable worktree.
        ReviewChanges,
        /// ⌘R: asks `review` to count the worktrees again.
        Refresh,
        /// The inspector's button: makes the selected worktree the current
        /// one; ignored for the current and for a prunable worktree.
        SwitchTo,
    ]
);

/// A row's click, and the probe: selects the worktree and, unless it is
/// prunable, reviews its changes.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = worktrees, no_json)]
pub struct OpenWorktree {
    pub key: WorktreeKey,
}

/// A session of the inspector, and the probe: the review of that session.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = worktrees, no_json)]
pub struct OpenSession {
    pub key: SessionKey,
}

/// An overlap item of the inspector, and the probe: opens or closes the
/// list of its paths. `key` is the other worktree.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = worktrees, no_json)]
pub struct ToggleOverlap {
    pub key: WorktreeKey,
}

/// The key context of the screen.
pub(crate) const CONTEXT: &str = "worktrees";

/// The predicate the screen's bindings are contributed with.
pub(crate) const PREDICATE: &str = "Content > worktrees";

/// The bindings, in the shape of `ui::contrib::KeyBindings`. The keystrokes
/// are GPUI key names, not interface text.
pub(crate) fn key_bindings() -> Vec<(&'static str, Box<dyn Action>, Option<&'static str>)> {
    vec![
        ("down", Box::new(SelectNext), Some(PREDICATE)),
        ("up", Box::new(SelectPrevious), Some(PREDICATE)),
        ("alt-up", Box::new(SelectFirst), Some(PREDICATE)),
        ("alt-down", Box::new(SelectLast), Some(PREDICATE)),
        ("enter", Box::new(ReviewChanges), Some(PREDICATE)),
        ("cmd-r", Box::new(Refresh), Some(PREDICATE)),
    ]
}
