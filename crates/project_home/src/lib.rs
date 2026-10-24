//! Project overview and project sidebar (docs/design/project_home.md).
//!
//! This is the part of the crate that opens no window and does not depend on
//! GPUI (「验收」step 1):
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
//! No code here runs before the first frame, does IO, or touches a window.

mod diag;
mod home;
mod plugin;
pub mod snapshot;
mod usage_slot;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

pub use home::Home;
pub use plugin::{
    PLUGIN_ID, ProjectHomeDeps, ProjectHomePlugin, ProjectHomeUsagePlugin, USAGE_PLUGIN_ID,
};
pub use snapshot::HomeSnapshot;
pub use usage_slot::UsageSlot;
