//! The measurement build (docs/design/app.md「测量构建的装配」): only
//! compiled with the `bench-probe` feature. `run` reads two environment
//! variables and registers the first-frame callback before the first frame;
//! behind the barrier the probe starts the kernel and takes the runner's
//! requests, which come to the main thread through one channel.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::App;
use lectern_bench::probe::{Driver, Probe};
use lectern_bench::wire::{self, OpenTarget};
use lectern_kernel::{Kernel, Name};
use lectern_logging::info;
use lectern_services::project::{ProjectSnapshot, WorktreeKey};
use lectern_services::review::ReviewTarget;
use lectern_services::session::{SessionKey, SourceId};
use lectern_services::workspace::{
    Contributions, Milestone, Route, Target, WorkspaceCommand, screens,
};
use lectern_workspace::Hub;

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

/// `WorkspaceDeps::on_milestone` of the measurement build: each milestone
/// `workspace` saw drawn goes to the runner with the time of the next frame
/// callback (docs/design/app.md「里程碑与帧」). Any thread.
pub fn on_milestone(probe: Arc<Probe>) -> Arc<dyn Fn(Milestone, SystemTime) + Send + Sync> {
    Arc::new(move |milestone, presented| probe.milestone(wire_milestone(milestone), presented))
}

/// The bench milestone of the same name.
pub(crate) fn wire_milestone(milestone: Milestone) -> wire::Milestone {
    match milestone {
        Milestone::ProjectHome => wire::Milestone::ProjectHome,
        Milestone::FileHighlighted => wire::Milestone::FileHighlighted,
        Milestone::DiffShown => wire::Milestone::DiffShown,
        Milestone::WorktreeSwitched => wire::Milestone::WorktreeSwitched,
    }
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
    /// The review of one session, as ↵ on its row on the project home.
    OpenDiff(SessionKey),
    /// The worktree with this root becomes the current one, with the
    /// Worktrees screen shown.
    OpenWorktree(PathBuf),
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
    /// A project is opened as the Open Folder dialog's choice is; a diff is
    /// the review of the session `<source>:<id>`, opened as the project
    /// home opens it; a worktree is switched to as the Worktrees screen's
    /// button does. The other kinds are added by the benchmarks that use
    /// them.
    fn open(&self, target: OpenTarget) {
        match target {
            OpenTarget::Project(path) => {
                // The receiving task ends only with the app.
                let _ = self.requests.unbounded_send(BenchRequest::Open(path));
            }
            OpenTarget::Diff { session } => match session_key(&session) {
                Some(key) => {
                    let _ = self.requests.unbounded_send(BenchRequest::OpenDiff(key));
                }
                None => self.reporter.error("open diff without a source"),
            },
            OpenTarget::Worktree(root) => {
                let _ = self
                    .requests
                    .unbounded_send(BenchRequest::OpenWorktree(root));
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

/// The key of `<source>:<id>`: the source is the session source's plugin
/// ID (`session-claude-code`), the ID the one inside that source. None
/// without a `:` or with an empty part.
pub(crate) fn session_key(session: &str) -> Option<SessionKey> {
    let (source, id) = session.split_once(':')?;
    (!source.is_empty() && !id.is_empty()).then(|| SessionKey {
        source: SourceId(source.into()),
        id: id.into(),
    })
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
                handle(request, hub.as_deref(), &*errors, cx);
            });
        }
    })
    .detach();
    probe.attach(kernel, Arc::new(BenchDriver::new(requests, reporter)));
    info!("bench probe attached");
}

/// One request, on the main thread. `Open` goes into `workspace`'s queue as
/// `OpenProject`, the command the Open Folder dialog's choice makes
/// (`workspace`'s `view.rs`); `OpenDiff` as the `Push` of the review route
/// that ↵ on a session row makes (`project_home`'s `present.rs`);
/// `OpenWorktree` as [`switch_worktree`] says. Without a workspace each is an
/// error. `Quit` is the path of ⌘Q.
pub(crate) fn handle(
    request: BenchRequest,
    workspace: Option<&Hub>,
    errors: &dyn Reporter,
    cx: &mut App,
) {
    match request {
        BenchRequest::Open(path) => match workspace {
            Some(workspace) => workspace.dispatch(WorkspaceCommand::OpenProject(path)),
            None => errors.error("open project without a workspace"),
        },
        BenchRequest::OpenDiff(key) => match workspace {
            Some(workspace) => workspace.dispatch(WorkspaceCommand::Push(Route {
                screen: Name::Borrowed(screens::REVIEW),
                target: Target::Review(ReviewTarget::Session(key)),
            })),
            None => errors.error("open diff without a workspace"),
        },
        BenchRequest::OpenWorktree(root) => match workspace {
            Some(hub) => switch_worktree(hub, &hub.open_snapshots(), &root, errors),
            None => errors.error("open worktree without a workspace"),
        },
        BenchRequest::Quit => quit::quit(cx),
    }
}

/// What the Worktrees screen's "switch to this worktree" button makes:
/// pushes the Worktrees screen (ignored when it is the current one, which
/// `workspace` does for any route on top) and makes the worktree with the
/// root `root` the current one. `worktrees` reports the milestone
/// `WorktreeSwitched` from the screen. An error when no open project has a
/// worktree with that root that is not prunable, or when it is the current
/// one already (no switch would follow, and the runner would wait for the
/// milestone in vain). The path is not in the message. Main thread, no IO.
pub(crate) fn switch_worktree(
    workspace: &dyn Contributions,
    open: &[Arc<ProjectSnapshot>],
    root: &Path,
    errors: &dyn Reporter,
) {
    match find_worktree(open, root) {
        Some((key, false)) => {
            workspace.dispatch(WorkspaceCommand::Push(Route {
                screen: Name::Borrowed(screens::WORKTREES),
                target: Target::None,
            }));
            workspace.dispatch(WorkspaceCommand::SetActiveWorktree(key));
        }
        Some((_, true)) => errors.error("open worktree is the current one"),
        None => errors.error("open worktree not found"),
    }
}

/// The key of the worktree with the root `root` (compared as paths, as
/// `project` canonicalises the roots) in the first open project that has one
/// that is not prunable, and whether it is that project's current worktree.
fn find_worktree(open: &[Arc<ProjectSnapshot>], root: &Path) -> Option<(WorktreeKey, bool)> {
    open.iter().find_map(|project| {
        let worktree = project
            .worktrees
            .iter()
            .find(|worktree| !worktree.prunable && *worktree.root == *root)?;
        let current = project
            .active
            .as_ref()
            .and_then(|active| active.worktree.as_ref())
            == Some(&worktree.key);
        Some((worktree.key.clone(), current))
    })
}
