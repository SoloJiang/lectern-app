//! The measurement build (docs/design/app.md「测量构建的装配」): only
//! compiled with the `bench-probe` feature. `run` reads two environment
//! variables and registers the first-frame callback before the first frame;
//! behind the barrier the probe starts the kernel and takes the runner's
//! requests, which come to the main thread through one channel.

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::App;
use lectern_bench::probe::{Driver, Probe};
use lectern_bench::wire::OpenTarget;
use lectern_kernel::Kernel;
use lectern_logging::info;
use lectern_services::workspace::{Contributions, WorkspaceCommand};

use crate::quit;
use crate::startup::Started;

/// The probe of this process, or None without `LECTERN_BENCH_SOCKET`: the
/// app then runs as the release build does. Before the first frame: two
/// environment variables and, with a socket, one method replacement; no IO
/// and no thread.
pub fn from_env() -> Option<Arc<Probe>> {
    let probe = Probe::from_env()?;
    probe.watch_first_frame();
    Some(Arc::new(probe))
}

/// Where the probe's errors go. The probe sends them to the runner's
/// socket; the tests record them.
pub(crate) trait Reporter: Send + Sync + 'static {
    /// Any thread.
    fn error(&self, message: &str);
}

impl Reporter for Probe {
    fn error(&self, message: &str) {
        Probe::error(self, message);
    }
}

/// What the runner asked for, on the way to the main thread.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BenchRequest {
    Open(PathBuf),
    Quit,
}

/// The runner's requests to the main thread. `Driver`'s two methods run on
/// the probe thread: they only send a message and never block.
pub struct BenchDriver {
    requests: UnboundedSender<BenchRequest>,
    reporter: Arc<dyn Reporter>,
}

impl BenchDriver {
    pub(crate) fn new(
        requests: UnboundedSender<BenchRequest>,
        reporter: Arc<dyn Reporter>,
    ) -> BenchDriver {
        BenchDriver { requests, reporter }
    }
}

impl Driver for BenchDriver {
    /// A project is opened as the Open Folder dialog's choice is. The other
    /// kinds are added by the benchmarks that use them.
    fn open(&self, target: OpenTarget) {
        match target {
            OpenTarget::Project(path) => {
                // The receiving task ends only with the app.
                let _ = self.requests.unbounded_send(BenchRequest::Open(path));
            }
            other => self
                .reporter
                .error(&format!("open {} unsupported", kind(&other))),
        }
    }

    fn quit(&self) {
        let _ = self.requests.unbounded_send(BenchRequest::Quit);
    }
}

/// The `target` the wire names the kind with.
fn kind(target: &OpenTarget) -> &'static str {
    match target {
        OpenTarget::Project(_) => "project",
        OpenTarget::File(_) => "file",
        OpenTarget::Diff { .. } => "diff",
        OpenTarget::Worktree(_) => "worktree",
        OpenTarget::Scroll => "scroll",
        OpenTarget::Animate => "animate",
        OpenTarget::Input { .. } => "input",
    }
}

/// Starts the task that takes the runner's requests on the main thread, and
/// hands the probe the kernel and the driver. `release_in` calls it right
/// after `start_kernel`, so `Started` is set by the time the first request is
/// handled. Main thread, does not block.
pub fn attach(probe: &Arc<Probe>, kernel: Arc<Kernel>, cx: &mut App) {
    let (requests, mut incoming) = unbounded();
    let reporter: Arc<dyn Reporter> = probe.clone();
    let errors = reporter.clone();
    cx.spawn(async move |cx| {
        while let Some(request) = incoming.next().await {
            cx.update(|cx| {
                // The workspace of the window, from `Started`.
                let hub = cx
                    .try_global::<Started>()
                    .and_then(|started| started.shell.as_ref())
                    .map(|shell| shell.host.hub().clone());
                handle(request, hub.as_deref().map(|hub| hub as _), &*errors, cx);
            });
        }
    })
    .detach();
    probe.attach(kernel, Arc::new(BenchDriver::new(requests, reporter)));
    info!("bench probe attached");
}

/// One request, on the main thread. `Open` goes into `workspace`'s queue as
/// `OpenProject`, the command the Open Folder dialog's choice makes
/// (`workspace`'s `view.rs`); without a workspace it is an error. `Quit` is
/// the path of ⌘Q.
pub(crate) fn handle(
    request: BenchRequest,
    workspace: Option<&dyn Contributions>,
    errors: &dyn Reporter,
    cx: &mut App,
) {
    match request {
        BenchRequest::Open(path) => match workspace {
            Some(workspace) => workspace.dispatch(WorkspaceCommand::OpenProject(path)),
            None => errors.error("open project without a workspace"),
        },
        BenchRequest::Quit => quit::quit(cx),
    }
}
