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
use gpui::{AnyWindowHandle, App, Keystroke};
use lectern_bench::probe::{Driver, Probe};
use lectern_bench::wire::{self, OpenTarget};
use lectern_file_reader::{FILES_PANEL_ID as FILES_PANEL, ReaderSlot};
use lectern_kernel::{Kernel, Name};
use lectern_logging::info;
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{BlobId, ProjectSnapshot, WorktreeKey};
use lectern_services::reader::{Anchor, ReadTarget, Reader};
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
        Milestone::FilesShown => wire::Milestone::FilesShown,
        Milestone::FilesExpanded => wire::Milestone::FilesExpanded,
        Milestone::EditReady => wire::Milestone::EditReady,
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
    /// The review of the worktree changes that no session claims.
    OpenChanges,
    /// A file of the open project in the reading view, as `Reader::open`
    /// takes it; with `old` the blob of that ID (the diff's old side).
    OpenFile {
        path: PathBuf,
        byte: Option<u64>,
        old: Option<BlobId>,
    },
    /// The Files panel in the sidebar, as the Files entry shows it.
    ShowFiles,
    /// One keystroke into the focus, as the keyboard sends it.
    Key(Keystroke),
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
    /// button does; a file is opened through `dyn Reader`, as the review
    /// screens' hand-off does; Files is shown as its sidebar entry shows
    /// it; a key is dispatched to the window's focus. The other kinds are
    /// added by the benchmarks that use them.
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
            OpenTarget::File { path, byte, old } => {
                let old = match old.as_deref().map(BlobId::from_hex) {
                    None => None,
                    Some(Some(oid)) => Some(oid),
                    Some(None) => {
                        self.reporter.error("open file with a bad object ID");
                        return;
                    }
                };
                let _ = self
                    .requests
                    .unbounded_send(BenchRequest::OpenFile { path, byte, old });
            }
            OpenTarget::Changes => {
                let _ = self.requests.unbounded_send(BenchRequest::OpenChanges);
            }
            OpenTarget::Files => {
                let _ = self.requests.unbounded_send(BenchRequest::ShowFiles);
            }
            OpenTarget::Key(key) => match Keystroke::parse(&key) {
                Ok(keystroke) => {
                    let _ = self.requests.unbounded_send(BenchRequest::Key(keystroke));
                }
                Err(_) => self.reporter.error("open key with a bad keystroke"),
            },
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
        OpenTarget::File { .. } => "file",
        OpenTarget::Changes => "changes",
        OpenTarget::Files => "files",
        OpenTarget::Key(_) => "key",
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
pub fn attach(probe: &Arc<Probe>, kernel: Arc<Kernel>, reader: ReaderSlot, cx: &mut App) {
    let (requests, mut incoming) = unbounded();
    let reporter: Arc<dyn Reporter> = probe.clone();
    let errors = reporter.clone();
    cx.spawn(async move |cx| {
        while let Some(request) = incoming.next().await {
            cx.update(|cx| {
                // The workspace of the window, from `Started`.
                let shell = cx
                    .try_global::<Started>()
                    .and_then(|started| started.shell.clone());
                let hub = shell.as_ref().map(|shell| shell.host.hub().clone());
                let window = shell.as_ref().and_then(|shell| shell.host.window());
                let reader = reader.get();
                handle(
                    request,
                    &Targets {
                        workspace: hub.as_deref(),
                        reader: reader.as_deref(),
                        window,
                    },
                    &*errors,
                    cx,
                );
            });
        }
    })
    .detach();
    probe.attach(kernel, Arc::new(BenchDriver::new(requests, reporter)));
    info!("bench probe attached");
}

/// What a request acts on: the workspace of the window, the reader, and the
/// window. Each is None while it is not there yet.
pub(crate) struct Targets<'a> {
    pub workspace: Option<&'a Hub>,
    pub reader: Option<&'a dyn Reader>,
    pub window: Option<AnyWindowHandle>,
}

/// One request, on the main thread. `Open` goes into `workspace`'s queue as
/// `OpenProject`, the command the Open Folder dialog's choice makes
/// (`workspace`'s `view.rs`); `OpenDiff` as the `Push` of the review route
/// that ↵ on a session row makes (`project_home`'s `present.rs`);
/// `OpenWorktree` as [`switch_worktree`] says; `OpenChanges` as
/// [`open_changes`] says; `OpenFile` as [`open_file`]
/// says; `ShowFiles` as the Files entry's `on_select` does
/// (`ShowSidebarPanel`); `Key` as `Window::dispatch_keystroke`, the path of
/// a key the platform delivers. Without what it acts on each is an error.
/// `Quit` is the path of ⌘Q.
pub(crate) fn handle(
    request: BenchRequest,
    targets: &Targets<'_>,
    errors: &dyn Reporter,
    cx: &mut App,
) {
    let workspace = targets.workspace;
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
        BenchRequest::OpenChanges => match workspace {
            Some(hub) => open_changes(hub, &hub.open_snapshots(), errors),
            None => errors.error("open changes without a workspace"),
        },
        BenchRequest::OpenFile { path, byte, old } => match (workspace, targets.reader) {
            (Some(hub), Some(reader)) => {
                open_file(reader, &hub.open_snapshots(), &path, byte, old, errors)
            }
            (None, _) => errors.error("open file without a workspace"),
            (_, None) => errors.error("open file without a reader"),
        },
        BenchRequest::ShowFiles => match workspace {
            Some(workspace) => workspace.dispatch(WorkspaceCommand::ShowSidebarPanel(Some(
                Name::Borrowed(FILES_PANEL),
            ))),
            None => errors.error("open files without a workspace"),
        },
        BenchRequest::Key(keystroke) => {
            let Some(window) = targets.window else {
                errors.error("open key without a window");
                return;
            };
            if window
                .update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx))
                .is_err()
            {
                errors.error("open key without a window");
            }
        }
        BenchRequest::Quit => quit::quit(cx),
    }
}

/// The review of the first open git project's active worktree against its
/// HEAD, outside any session: the `Push` of the review route that the
/// project home's Outside Any Session row makes. An error when no open
/// project is a git project with an active worktree. Main thread, no IO.
pub(crate) fn open_changes(
    workspace: &dyn Contributions,
    open: &[Arc<ProjectSnapshot>],
    errors: &dyn Reporter,
) {
    let found = open.iter().find_map(|project| {
        let worktree = project.active.as_ref()?.worktree.clone()?;
        Some((project.key.clone(), worktree))
    });
    match found {
        Some((project, worktree)) => workspace.dispatch(WorkspaceCommand::Push(Route {
            screen: Name::Borrowed(screens::REVIEW),
            target: Target::Review(ReviewTarget::Outside { project, worktree }),
        })),
        None => errors.error("open changes without a git project"),
    }
}

/// What a hand-off to the reader makes (`Reader::open`): the file `path`,
/// in the first open project whose active tree holds it, from the start, or
/// at `byte` of the worktree's file (the offset of a hunk), or with `old`
/// the blob of that ID as the old side of a diff (`TextOrigin::Object`, which
/// the review screen shared while it showed the diff). An error when no open
/// project holds the path. The path is not in the message. Main thread, no
/// IO.
pub(crate) fn open_file(
    reader: &dyn Reader,
    open: &[Arc<ProjectSnapshot>],
    path: &Path,
    byte: Option<u64>,
    old: Option<BlobId>,
    errors: &dyn Reporter,
) {
    match read_target(open, path, byte, old) {
        Some(target) => {
            reader.open(target);
        }
        None => errors.error("open file not found in a project"),
    }
}

/// The reader's target for `path`; None when no open project's active tree
/// holds it (or the path below the root is not UTF-8).
pub(crate) fn read_target(
    open: &[Arc<ProjectSnapshot>],
    path: &Path,
    byte: Option<u64>,
    old: Option<BlobId>,
) -> Option<ReadTarget> {
    open.iter().find_map(|project| {
        let active = project.active.as_ref()?;
        let relative = path.strip_prefix(&active.root).ok()?;
        Some(ReadTarget {
            project: project.key.clone(),
            worktree: active.worktree.clone(),
            path: Arc::from(relative.to_str()?),
            origin: if old.is_some() {
                TextOrigin::Object
            } else {
                TextOrigin::Worktree
            },
            expect: old,
            at: match byte {
                Some(byte) => Anchor::Byte {
                    byte,
                    line: 0,
                    column: 0,
                },
                None => Anchor::Start,
            },
        })
    })
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
