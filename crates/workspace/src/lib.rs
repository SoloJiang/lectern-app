//! Window skeleton: sidebar, toolbar, inspector, floating panel and bar
//! (docs/design/workspace.md).
//!
//! This is the part of the crate that does not depend on GPUI (「验收」step 1):
//!
//! - [`nav`]: the navigation state machine (「数据结构」「导航状态机」) and the
//!   leave-guard sequencing around it.
//! - [`registry`]: the contribution registry (「贡献注册表」) behind
//!   `dyn Contributions`.
//! - [`driver`]: the main-thread loop over both, fed by the queue the
//!   [`Hub`] fills from any thread; its [`Effect`]s are what the root view
//!   (step 2) turns into windows, views, key bindings and events.
//! - [`WorkspacePlugin`]: the "workspace" plugin, providing `dyn Contributions`.
//! - [`WindowState`]: what `app` keeps across runs (「窗口状态」).
//!
//! No code here runs before the first frame, does IO, or touches a window.

mod diag;
pub mod driver;
mod effect;
mod hub;
pub mod nav;
mod plugin;
pub mod registry;
mod state;

#[cfg(test)]
mod tests;

pub use effect::{Effect, Notice};
pub use hub::{Hub, Item, Queued, Sender};
pub use plugin::WorkspacePlugin;
pub use state::WindowState;
