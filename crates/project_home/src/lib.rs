//! Project overview and project sidebar (docs/design/project_home.md).
//!
//! Step 1 (no window): the snapshot and the plugins.
//!
//! - [`snapshot`]: the immutable [`HomeSnapshot`] the views read, and
//!   [`snapshot::build`], the pure function that composes it from the
//!   snapshots of `project`, `agents`, `review` and `usage`.
//! - [`ProjectHomePlugin`] ("project-home"): holds the overview interest of
//!   the open project, listens to the services' events and composes a new
//!   snapshot in a `Background` task. [`Home`] is its state, which the views
//!   of step 2 read.
//! - [`ProjectHomeUsagePlugin`] ("project-home-usage") and [`UsageSlot`]: the
//!   optional `dyn Usage`, so that the screen works without the `usage` plugin.
//!
//! Step 2 (the views): [`HomeScreen`] (the main area) and [`HomeSidebar`] (the
//! sidebar panel), the actions and key bindings, and the contributions the
//! plugin registers with `dyn Contributions`.
//!
//! The waiting marks (prerequisite H of docs/design/conversation.md):
//! [`ProjectHomeWaitingPlugin`] ("project-home-waiting") and [`WaitingSlot`] put
//! the optional `dyn Conversations` into a cell; the views read the sessions
//! that wait for the user from it and redraw on `WaitingChanged`.
//!
//! No code here runs before the first frame or does IO; the views read the
//! snapshot and send `WorkspaceCommand`s.

mod actions;
mod contribute;
mod diag;
#[cfg(any(test, feature = "test-support"))]
pub mod fixture;
mod home;
mod nodes;
mod plugin;
mod present;
mod screen;
mod sidebar;
pub mod snapshot;
mod usage_slot;
mod waiting_slot;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

pub use actions::{
    Open, OpenOutside, OpenSession, SelectFirst, SelectLast, SelectNext, SelectPrevious,
};
pub use home::Home;
pub use plugin::{
    PLUGIN_ID, ProjectHomeDeps, ProjectHomePlugin, ProjectHomeUsagePlugin,
    ProjectHomeWaitingPlugin, USAGE_PLUGIN_ID, WAITING_PLUGIN_ID,
};
pub use screen::HomeScreen;
pub use sidebar::HomeSidebar;
pub use snapshot::HomeSnapshot;
pub use usage_slot::UsageSlot;
pub use waiting_slot::WaitingSlot;
