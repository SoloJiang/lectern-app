//! What the state machine and the driver ask the window to do. The root view
//! (step 2) runs them on the main thread; the tests read them.

use lectern_services::workspace::{LeaveScope, Milestone, WorkspaceEvent};

use crate::nav::{AskId, GuardId};
use crate::registry::Serial;
use crate::state::WindowState;

/// One thing to do after a command, an event or a registration was handled,
/// in order.
#[derive(Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "Event carries a WorkspaceEvent by value (about 400 bytes for ScreenChanged): a \
              handful of effects per command, each consumed at once; boxing would only add an \
              indirection for the root view and the tests"
)]
pub enum Effect {
    /// Ask the leave guard `guard` for `scope`; its answer comes back as
    /// `Item::GuardAnswer { ask, .. }`. The driver resolves these itself; the
    /// navigator's tests see them.
    Ask {
        ask: AskId,
        guard: GuardId,
        scope: LeaveScope,
    },
    /// Emit through the plugin's emitter.
    Event(WorkspaceEvent),
    /// `WorkspaceDeps::on_state_changed`.
    StateChanged(WindowState),
    /// Show the system open panel; the chosen folder comes back as
    /// `WorkspaceCommand::OpenProject`.
    PromptOpenProject,
    /// Every guard allowed it: remove the window.
    CloseWindow,
    /// Show a one-time notice.
    Notice(Notice),
    /// A `KeyBindings` contribution was applied: bind its keys (`bind_keys`).
    BindKeys(Serial),
    /// `Contributions::report_milestone`: note the next frame's time and hand
    /// it to `app`.
    Milestone(Milestone),
}

/// A one-time notice; the root view picks the Fluent key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// `Opened` arrived while a view refused to leave: the project is open,
    /// the window did not switch.
    ProjectOpenedWhileAViewRefusedToLeave,
}
