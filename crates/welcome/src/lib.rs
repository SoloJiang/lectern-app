//! The open-a-project screen: the recent projects and the folders found
//! through agent sessions (docs/design/welcome.md).
//!
//! - [`plugins`]: the "welcome" and "welcome-discovery" plugins, which share
//!   one [`WelcomeModel`]. "welcome" registers `Screen(WELCOME)` and the
//!   screen's `KeyBindings`.
//! - [`WelcomeModel`] and [`WelcomeSnapshot`]: what the view reads and
//!   the commands it sends back (memory only).
//! - `recent`: the list, and `recent.json` under the data directory (the
//!   only file with filesystem calls in this crate).
//! - [`discover`]: the pure function that groups session lists into the
//!   discovered folders. It never reads a directory; the session sources
//!   (`agents`) do.
//! - `view`: the screen's view, the five actions and the language picker.
//!   It reads the snapshot and sends commands; it does no IO.
//!
//! No code here runs before the first frame.

mod actions;
mod discover;
mod model;
mod plugin;
mod prefs;
mod recent;
mod view;

#[cfg(test)]
mod tests;

pub use actions::{OpenSelected, RemoveSelected, SelectLanguage, SelectNext, SelectPrevious};
pub use discover::DiscoveredFolder;
pub use model::{
    DiscoveredState, Notice, OpenFailure, OpenSource, Opening, RecentState, WakeGuard,
    WelcomeModel, WelcomeSnapshot,
};
pub use plugin::{DISCOVERY_ID, WELCOME_ID, WelcomeDeps, WelcomePlugin, plugins};
pub use recent::RecentEntry;
