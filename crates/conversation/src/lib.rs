//! Conversation page of the inspector (docs/design/conversation.md).
//!
//! Step 1 (no reply box, no permission card, no Terminal page): the plugins
//! and the Messages page's display.
//!
//! - [`ConversationPlugin`] ("conversation") registers the inspector's
//!   Conversation page and its key bindings, and follows the review on the
//!   screen. [`LivePlugin`] ("conversation-live") brings `dyn Conversations`
//!   into the state they share, so that the page says so when the service is
//!   not there. [`plugins`] builds both, for `app`.
//! - [`ConversationTab`] is the page: it holds the Messages page of the
//!   session under review. `items` turns a snapshot into the list's items and
//!   says which stretch of the list changed; `present` turns values into
//!   interface text.
//!
//! No code here runs before the first frame or does IO; the views read the
//! snapshot and send `ConversationCommand`s.

mod actions;
mod diag;
mod items;
mod nodes;
mod plugin;
mod present;
mod view;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

pub use actions::{Dismiss, LoadEarlier, ShowMessages, ShowTerminal, Stop, TogglePlan};
pub use plugin::{ConversationPlugin, LIVE_ID, LivePlugin, PLUGIN_ID, TAB_ID, plugins};
pub use view::{ConversationTab, Page};
