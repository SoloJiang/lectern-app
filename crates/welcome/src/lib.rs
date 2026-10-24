//! The open-a-project screen: the recent projects and the folders found
//! through agent sessions (docs/design/welcome.md).
//!
//! Step 1 of the design, the part that does not depend on GPUI:
//!
//! - [`plugins`]: the "welcome" and "welcome-discovery" plugins, which share
//!   one [`WelcomeModel`].
//! - [`WelcomeModel`] and [`WelcomeSnapshot`]: what the view reads and
//!   the commands it sends back (memory only).
//! - `recent`: the list, and `recent.json` under the data directory (the
//!   only file with filesystem calls in this crate).
//! - [`discover`]: the pure function that groups session lists into the
//!   discovered folders. It never reads a directory; the session sources
//!   (`agents`) do.
//!
//! No code here runs before the first frame, and no GPUI type appears: the
//! view, the actions and the language picker are step 2.

mod discover;
mod model;
mod plugin;
mod recent;

#[cfg(test)]
mod tests;

pub use discover::DiscoveredFolder;
pub use model::{
    DiscoveredState, Notice, OpenFailure, OpenSource, Opening, RecentState, WakeGuard,
    WelcomeModel, WelcomeSnapshot,
};
pub use plugin::{DISCOVERY_ID, WELCOME_ID, WelcomeDeps, WelcomePlugin, plugins};
pub use recent::RecentEntry;
