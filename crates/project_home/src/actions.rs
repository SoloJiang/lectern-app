//! This crate's actions and key bindings (docs/design/project_home.md「Action 与快捷键」).
//! Every action has a stable name the probe can trigger. The bindings are
//! contributed as `KeyBindings` with the predicate `Content > project-home`:
//! `workspace` puts the screen in its `Content` region, and the screen's
//! root view carries the `project-home` context and the focus.

use gpui::{Action, actions};
use lectern_services::session::SessionKey;

actions!(
    project_home,
    [
        /// ↓: the next item, across the three groups; stops at the end.
        SelectNext,
        /// ↑: the previous item; stops at the start.
        SelectPrevious,
        /// ⌥↑: the first item.
        SelectFirst,
        /// ⌥↓: the last item.
        SelectLast,
        /// ↵: opens the selected item in the review; with nothing selected,
        /// selects the first item.
        Open,
        /// The Outside Any Session row's click, and the probe.
        OpenOutside,
    ]
);

/// A card's click, and the probe: the review of one session.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = project_home, no_json)]
pub struct OpenSession {
    pub key: SessionKey,
}

/// The key context of both views.
pub(crate) const CONTEXT: &str = "project-home";

/// The predicate the screen's bindings are contributed with.
pub(crate) const PREDICATE: &str = "Content > project-home";

/// The bindings, in the shape of `ui::contrib::KeyBindings`. The keystrokes
/// are GPUI key names, not interface text.
pub(crate) fn key_bindings() -> Vec<(&'static str, Box<dyn Action>, Option<&'static str>)> {
    vec![
        ("down", Box::new(SelectNext), Some(PREDICATE)),
        ("up", Box::new(SelectPrevious), Some(PREDICATE)),
        ("alt-up", Box::new(SelectFirst), Some(PREDICATE)),
        ("alt-down", Box::new(SelectLast), Some(PREDICATE)),
        ("enter", Box::new(Open), Some(PREDICATE)),
    ]
}
