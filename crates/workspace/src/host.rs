//! `install` and `WorkspaceHost` (docs/design/workspace.md「本 crate 的公开
//! 项」): the main-thread queue's receiving end, the root view in the
//! window, and the handle `app` and `WorkspacePlugin` hold.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyWindowHandle, App, AppContext, Context, Entity, Global, IntoElement, Render, Subscription,
    Task, Window,
};
use lectern_kernel::Name;
use lectern_services::workspace::{LeaveScope, Milestone, WindowId};

use crate::driver::Driver;
use crate::hub::{Hub, Item, Queued, RecordStop, Sender};
use crate::payloads::UiPayloads;
use crate::state::WindowState;
use crate::view::Workspace;

/// What `app` gives the workspace.
pub struct WorkspaceDeps {
    /// The window state changed (the debounce is `app`'s). Main thread, so
    /// `app` can switch the frame autosave name here.
    pub on_state_changed: Arc<dyn Fn(WindowState) + Send + Sync>,
    /// A milestone was drawn; `presented` is the next frame callback's time
    /// (「已知限制」). Any thread.
    pub on_milestone: Arc<dyn Fn(Milestone, SystemTime) + Send + Sync>,
    /// A frame recording ended. Any thread.
    pub on_frames: Arc<dyn Fn(FrameRecord) + Send + Sync>,
}

/// One frame's times. Stands in for `platform::frame::FrameTimes` until
/// `platform` implements frame recording (「已知限制」); until then a
/// `FrameRecord` carries no frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameTimes {
    pub presented: SystemTime,
}

/// The frames of one recording; `app` turns it into the bench's `FrameStats`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrameRecord {
    pub scenario: Name,
    pub frames: Vec<FrameTimes>,
}

struct Inner {
    hub: Arc<Hub>,
    /// The window the root view is in; None between a close and `attach`.
    window: Mutex<Option<AnyWindowHandle>>,
}

/// `Send + Sync`: `app` holds one, `WorkspacePlugin` holds one.
#[derive(Clone)]
pub struct WorkspaceHost(Arc<Inner>);

/// Keeps the root view alive across windows: the window's root is a thin
/// [`WindowRoot`] over it, so a Dock reopen (`attach`) keeps the registry
/// and the navigation state. Holds the queue task too.
struct Shell {
    workspace: Entity<Workspace>,
    _queue: Task<()>,
}

impl Global for Shell {}

/// The shared root view, once `install` ran.
pub(crate) fn workspace(cx: &App) -> Option<Entity<Workspace>> {
    cx.try_global::<Shell>()
        .map(|shell| shell.workspace.clone())
}

/// The window's root: renders the shared [`Workspace`].
pub(crate) struct WindowRoot {
    workspace: Entity<Workspace>,
    /// `lectern_ui::attach`'s observer for this window.
    _appearance: Subscription,
}

impl Render for WindowRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.workspace.clone()
    }
}

/// Called once by `app` on the main thread after the startup barrier:
/// replaces the window's root view with the workspace, starts the
/// main-thread queue and returns the handle for the plugin and `app`. No
/// IO, no thread.
pub fn install(window: AnyWindowHandle, deps: WorkspaceDeps, cx: &mut App) -> WorkspaceHost {
    let (tx, mut rx) = mpsc::unbounded::<Queued>();
    let sender: Sender = Arc::new(move |queued| {
        // The receiver is gone only with the app: nothing to do then.
        let _ = tx.unbounded_send(queued);
    });
    let hub = Hub::new(Box::new(UiPayloads), sender.clone());
    let driver = Driver::new(WindowId(0), hub.registry().clone(), sender);
    let deps = Arc::new(deps);
    let workspace = cx.new(|cx| Workspace::new(driver, hub.clone(), deps, cx));
    cx.bind_keys(crate::actions::key_bindings());
    let host = WorkspaceHost(Arc::new(Inner {
        hub,
        window: Mutex::new(Some(window)),
    }));
    mount(window, &workspace, cx);
    let shell = workspace.downgrade();
    let queue = cx.spawn(async move |cx| {
        while let Some(queued) = rx.next().await {
            if shell
                .update(cx, |workspace, cx| workspace.handle_queued(queued, cx))
                .is_err()
            {
                break;
            }
        }
    });
    cx.set_global(Shell {
        workspace,
        _queue: queue,
    });
    host
}

/// Puts the workspace in `window`: `lectern_ui::attach`, the root view, the
/// close interception and the focus.
fn mount(window: AnyWindowHandle, workspace: &Entity<Workspace>, cx: &mut App) {
    let shell = workspace.clone();
    // Fails only for a closed window; there is nothing to mount in then.
    let _ = window.update(cx, |_, window, cx| {
        let appearance = lectern_ui::attach(window, cx);
        window.replace_root(cx, |_, _| WindowRoot {
            workspace: shell.clone(),
            _appearance: appearance,
        });
        shell.update(cx, |workspace, cx| workspace.bind_window(window, cx));
    });
}

impl WorkspaceHost {
    /// The GPUI-free part: the registry and the queue.
    pub fn hub(&self) -> &Arc<Hub> {
        &self.0.hub
    }

    /// The window the workspace is in, if any.
    pub fn window(&self) -> Option<AnyWindowHandle> {
        *self.0.window.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A Dock reopen: `app` opened a new skeleton window; this puts the
    /// workspace in it, keeping the registry and the navigation state. Main
    /// thread. Does nothing before `install`.
    pub fn attach(&self, window: AnyWindowHandle, cx: &mut App) {
        let Some(workspace) = workspace(cx) else {
            return;
        };
        *self.0.window.lock().unwrap_or_else(|e| e.into_inner()) = Some(window);
        mount(window, &workspace, cx);
    }

    /// `app` read the window state file: opens the project it names (the
    /// command waits for `dyn Projects` when `started` has not run yet) and
    /// restores the inspector flag. Any thread.
    pub fn restore(&self, state: WindowState) {
        if !state.is_supported() {
            return;
        }
        self.0.hub.enqueue(Item::Restore(state));
    }

    /// Asks the leave guards for `scope` (`Quit` asks `LeaveScope::App`
    /// before `Kernel::shutdown`); `done(true)` means every guard allowed,
    /// at once when there is none. Any thread.
    pub fn ask_leave(&self, scope: LeaveScope, done: Box<dyn FnOnce(bool) + Send>) {
        self.0.hub.enqueue(Item::AskLeave { scope, done });
    }

    /// Starts recording the window's frames until `stop` holds; the record
    /// comes back through `on_frames`. A recording in progress ends first.
    /// Any thread.
    pub fn record_frames(&self, scenario: Name, stop: RecordStop) {
        self.0.hub.enqueue(Item::RecordFrames { scenario, stop });
    }

    /// Ends the recording in progress (`RecordStop::Manual`, or early).
    pub fn stop_recording(&self) {
        self.0.hub.enqueue(Item::StopRecording);
    }
}
