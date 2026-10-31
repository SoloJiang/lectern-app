//! Parallel worktree overview (docs/design/worktrees.md).
//!
//! Step 1 (no window): the snapshot and the plugins.
//!
//! - [`snapshot`]: the immutable [`WorktreesSnapshot`] the views read, and
//!   [`snapshot::build`], the pure function that composes it from the
//!   snapshots of `project`, `agents`, `review` and `usage`.
//! - [`WorktreesPlugin`] ("worktrees"): registers the sidebar entry of a git
//!   project, holds the interest in the worktrees' changes while the screen is
//!   the current one, listens to the services' events and composes a new
//!   snapshot in a `Background` task. [`Hub`] is its state, which the views of
//!   step 2 read.
//! - [`WorktreesUsagePlugin`] ("worktrees-usage") and [`UsageLink`]: the
//!   optional `dyn Usage`, so that the screen works without the `usage`
//!   plugin.
//!
//! Step 2 adds the views, the actions and the registration of the screen, the
//! inspector page and the key bindings.
//!
//! No code here runs before the first frame or does IO; the views read the
//! snapshot and send `WorkspaceCommand`s.

mod diag;
mod hub;
mod nodes;
mod plugin;
pub mod snapshot;
mod usage_link;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

pub use hub::Hub;
pub use plugin::{
    PLUGIN_ID, USAGE_PLUGIN_ID, WorktreesDeps, WorktreesPlugin, WorktreesUsagePlugin,
};
pub use snapshot::WorktreesSnapshot;
pub use usage_link::UsageLink;
