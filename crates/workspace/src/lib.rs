//! Window skeleton: sidebar, toolbar, inspector, floating panel and bar
//! (docs/design/workspace.md).
//!
//! The GPUI-free part (「验收」step 1):
//!
//! - [`nav`]: the navigation state machine (「数据结构」「导航状态机」) and the
//!   leave-guard sequencing around it.
//! - [`registry`]: the contribution registry (「贡献注册表」) behind
//!   `dyn Contributions`.
//! - [`driver`]: the main-thread loop over both, fed by the queue the
//!   [`Hub`] fills from any thread; its [`Effect`]s are what the root view
//!   turns into windows, views, key bindings and events.
//! - [`WorkspacePlugin`]: the "workspace" plugin, providing `dyn Contributions`.
//! - [`WindowState`]: what `app` keeps across runs (「窗口状态」).
//!
//! The part in the window (step 2): [`install`] and [`WorkspaceHost`], the
//! root view [`Workspace`] with the regions and the three layers, the
//! actions of「快捷键」, and the semantic nodes of the regions.
//!
//! No code here runs before the first frame, does IO, or touches a window
//! before `install`.

pub mod actions;
mod diag;
pub mod driver;
mod effect;
mod host;
mod hub;
pub mod nav;
mod payloads;
mod plugin;
pub mod registry;
pub mod semantic;
mod state;
mod view;

#[cfg(test)]
mod tests;

pub use effect::{Effect, Notice};
pub use host::{FrameRecord, FrameTimes, WorkspaceDeps, WorkspaceHost, install};
pub use hub::{Hub, Item, Queued, RecordStop, Sender};
pub use plugin::WorkspacePlugin;
pub use state::WindowState;
pub use view::Workspace;
