//! The review screen (docs/design/review_view.md): the session brief at the
//! top, the turn-by-turn change narrative below it with each file's diff,
//! the Progress list in the inspector and the floating bar. It reads the
//! snapshots, rows and texts of `dyn Review` and sends `ReviewCommand`s; it
//! reads no git, no session log and no file.
//!
//! Step 1 of the design's acceptance: the plugin "review-view" ([`plugin`]),
//! the screen ([`ReviewScreen`]) and the shared state ([`ReviewModel`]), the
//! pure item list ([`items`]), the brief ([`brief`]), the Progress page
//! ([`ProgressTab`]), the actions and key bindings ([`actions`]) and the
//! probe's nodes. Highlighting, the handoff plugin, the side-by-side diff,
//! folds that expand and Analyze's trigger are step 2.

pub mod actions;
mod anchor;
pub mod brief;
mod diag;
pub mod items;
mod model;
mod needs;
mod nodes;
mod plugin;
mod progress_tab;
mod render;
mod screen;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use model::ReviewModel;
pub use plugin::{FILES_TAB, LAYOUT, PLUGIN_ID, ReviewViewPlugin, plugin};
pub use progress_tab::ProgressTab;
pub use screen::ReviewScreen;
