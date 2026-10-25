//! The review screen (docs/design/review_view.md): the session brief at the
//! top, the turn-by-turn change narrative below it with each file's diff,
//! the Progress list in the inspector and the floating bar. It reads the
//! snapshots, rows and texts of `dyn Review` and sends `ReviewCommand`s; it
//! reads no git, no session log and no file.
//!
//! Two plugins ([`plugins`]): "review-view" with the screen
//! ([`ReviewScreen`]), the shared state ([`ReviewModel`]), the pure item list
//! ([`items`]), the brief ([`brief`]), the Progress page ([`ProgressTab`]),
//! the actions and key bindings ([`actions`]) and the probe's nodes; and
//! "review-view-handoff" ([`HandoffPlugin`]), which gives the screens the
//! reader, the code navigation and the usage. The diff is highlighted on the
//! executor (`highlight`), laid out unified or side by side, and its folds
//! expand.

pub mod actions;
mod anchor;
pub mod brief;
mod diag;
mod diffs;
mod handoff;
mod highlight;
pub mod items;
mod lines;
mod model;
mod needs;
mod nodes;
mod plugin;
mod progress_tab;
mod render;
mod render_diff;
mod screen;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use items::DiffMode;
pub use model::ReviewModel;
pub use plugin::{
    FILES_TAB, HANDOFF_ID, HandoffPlugin, LAYOUT, PLUGIN_ID, ReviewViewPlugin, plugins,
};
pub use progress_tab::ProgressTab;
pub use screen::ReviewScreen;
