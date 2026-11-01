//! Conversation page of the inspector (docs/design/conversation.md).
//!
//! Steps 1 to 3: the plugins, the Messages page's display, the reply box,
//! Stop, the permission cards, the drafts, and the Terminal page with the
//! terminal's key bindings and leave guard.
//!
//! - [`ConversationPlugin`] ("conversation") registers the inspector's
//!   Conversation page and its key bindings, and follows the review on the
//!   screen. [`LivePlugin`] ("conversation-live") brings `dyn Conversations`
//!   into the state they share, so that the page says so when the service is
//!   not there, and brings the page up when the session under review gets a
//!   request. [`TerminalPagePlugin`] ("conversation-terminal") brings
//!   `dyn Terminals`, so that the reply box knows when a terminal drives the
//!   session and the Terminal page can embed one; it also registers the
//!   terminal's key bindings and the leave guard. [`plugins`] builds the
//!   three, for `app`.
//! - [`ConversationTab`] is the page: it holds the Messages page of the
//!   session under review. `items` turns a snapshot into the list's items and
//!   says which stretch of the list changed; `present` turns values into
//!   interface text; `drive` says who drives a session and whether the reply
//!   box may send; `memory` keeps the drafts.
//!
//! No code here runs before the first frame or does IO; the views read the
//! snapshot and send `ConversationCommand`s.

mod actions;
mod diag;
mod drive;
#[cfg(feature = "test-support")]
pub mod fixture;
mod guard;
mod items;
mod memory;
mod nodes;
mod plugin;
mod present;
mod view;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

pub use actions::{
    AllowAlways, AllowOnce, CloseTerminal, Dismiss, LoadEarlier, RejectAlways, RejectOnce,
    ResumeInTerminal, Send, ShowMessages, ShowTerminal, Stop, ToggleMoreRequests, TogglePlan,
};
pub use plugin::{
    ConversationPlugin, LIVE_ID, LivePlugin, PLUGIN_ID, TAB_ID, TERMINAL_ID, TerminalPagePlugin,
    plugins,
};
pub use view::{ConversationTab, Page};
