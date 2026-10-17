//! Plugin kernel: loading, service registry, event bus and lifecycle.
//!
//! Built-in plugins are linked at compile time and handed to [`Kernel::start`]
//! in a [`PluginSet`]. The kernel activates them on its own thread as their
//! activation conditions and service dependencies are met, records everything
//! each activation registers, and revokes it in reverse order when the plugin
//! is deactivated. The design is in `docs/design/kernel.md`.

mod event;
mod gate;
mod handle;
mod kernel;
mod panic;
mod plugin;
mod registry;
mod service;
mod set;
mod token;
mod util;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use event::{Busy, Emitter, Event, Quiescent, ServiceAdded, ServiceRemoved, Subscription};
pub use gate::{GatePoll, GateSlot, SubmitGate};
pub use handle::{Context, KernelControl, PluginHandle};
pub use kernel::{Kernel, WorkspaceFacts};
pub use panic::{PanicPayload, catch, contain};
pub use plugin::{
    Activation, Active, GraphError, Manifest, Name, Plugin, PluginError, PluginState, PluginStatus,
    Staged,
};
pub use registry::{Admitted, ServiceRef, ServiceSet};
pub use service::{Service, ServiceKey, service};
pub use set::PluginSet;
pub use token::{ActivationToken, Admission, BusyGuard, PendingGuard, TokenState};

#[cfg(test)]
mod tests;
