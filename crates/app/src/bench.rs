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
use gpui::{
    AnyWindowHandle, App, Keystroke, PlatformInput, ScrollDelta, ScrollWheelEvent, TouchPhase,
    Window, point, px,
};
use lectern_bench::probe::{Driver, Probe};
use lectern_bench::wire::{
    self, FrameStats, OpenTarget, PresentedAt, SCROLL_DOWN_EVENTS, SCROLL_EVENTS, SCROLL_STEP_PX,
    SCROLL_TAIL_FRAMES,
};
use lectern_file_reader::{FILES_PANEL_ID as FILES_PANEL, ReaderSlot};
use lectern_kernel::{Kernel, Name};
use lectern_logging::info;
use lectern_platform::frame::FrameRecording;
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{BlobId, ProjectSnapshot, WorktreeKey};
use lectern_services::reader::{Anchor, ReadTarget, Reader};
use lectern_services::review::ReviewTarget;
use lectern_services::session::{SessionKey, SourceId};
use lectern_services::workspace::{
    Contributions, Milestone, Route, Target, WorkspaceCommand, screens,
};
use lectern_workspace::{FrameRecord, Hub, RecordStop, WorkspaceHost};

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

/// `WorkspaceDeps::on_frames` of the measurement build: a recording that
/// ended goes to the runner as `Frames` when it is clean, and as `Error`
/// otherwise (docs/design/app.md「里程碑与帧」). Any thread.
pub fn on_frames(probe: Arc<Probe>) -> Arc<dyn Fn(FrameRecord) + Send + Sync> {
    Arc::new(move |record| report_frames(record, &*probe))
}

/// One finished recording to its reporter.
pub(crate) fn report_frames(record: FrameRecord, reporter: &dyn Reporter) {
    match frame_stats(record) {
        Ok(stats) => reporter.frames(stats),
        Err(message) => reporter.error(&message),
    }
}

/// The report of a recording: the frames when `record` worked and the
/// recording is clean (`FrameProblems::is_clean`: no frame was lost or
/// unmatched), otherwise the message of the `Error` the runner records the
/// benchmark with (docs/design/bench.md `Probe::frames`):
/// `frames <HookError::as_str()>` or `frames unmatched=<n> incomplete=<n>
/// overflowed=<n>`. The error's own text (an `io::Error` of the display
/// link) and the scenario are not in it.
pub(crate) fn frame_stats(record: FrameRecord) -> Result<FrameStats, String> {
    let recording = record
        .result
        .map_err(|error| format!("frames {}", error.as_str()))?;
    if !recording.problems.is_clean() {
        let problems = recording.problems;
        return Err(format!(
            "frames unmatched={} incomplete={} overflowed={}",
            problems.unmatched, problems.incomplete, problems.overflowed
        ));
    }
    Ok(wire_frames(&record.scenario, recording))
}

/// `platform`'s recording as the bench's `FrameStats`: the moments of
/// each frame in order, the `presented` time with its source, the nominal
/// refresh and the count of frames the display link did not ask for.
pub(crate) fn wire_frames(scenario: &str, recording: FrameRecording) -> FrameStats {
    FrameStats {
        scenario: scenario.to_owned(),
        refresh: recording.refresh,
        without_tick: recording.problems.without_tick,
        frames: recording
            .frames
            .into_iter()
            .map(|frame| wire::FrameTimes {
                requested: frame.requested,
                started: frame.started,
                committed: frame.committed,
                gpu_started: frame.gpu_started,
                completed: frame.completed,
                presented: frame.presented.map(|at| PresentedAt {
                    time: at.time,
                    source: at.source,
                }),
            })
            .collect(),
    }
}

/// Where the probe's reports go. The probe sends them to the runner's
/// socket; the tests record them.
pub(crate) trait Reporter: Send + Sync + 'static {
    /// Any thread.
    fn error(&self, message: &str);

    /// The frames of a clean recording. Any thread.
    fn frames(&self, stats: FrameStats);
}

impl Reporter for Probe {
    fn error(&self, message: &str) {
        Probe::error(self, message);
    }

    fn frames(&self, stats: FrameStats) {
        Probe::frames(self, stats);
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
    /// The scroll scenario: the window's frames are recorded while the
    /// reading view is scrolled by wheel events, one per frame
    /// ([`scroll`]). With `yielding` the kernel's yield switch is on.
    Scroll {
        yielding: bool,
    },
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
    /// it; a key is dispatched to the window's focus; a scroll records the
    /// frames of wheel events sent to the window. The other kinds are
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
            OpenTarget::Scroll { yielding } => {
                let _ = self
                    .requests
                    .unbounded_send(BenchRequest::Scroll { yielding });
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
        OpenTarget::File { .. } => "file",
        OpenTarget::Changes => "changes",
        OpenTarget::Files => "files",
        OpenTarget::Key(_) => "key",
        OpenTarget::Diff { .. } => "diff",
        OpenTarget::Worktree(_) => "worktree",
        OpenTarget::Scroll { .. } => "scroll",
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
    let for_requests = reporter.clone();
    let kernel_handle = kernel.clone();
    cx.spawn(async move |cx| {
        while let Some(request) = incoming.next().await {
            cx.update(|cx| {
                // The workspace of the window, from `Started`.
                let shell = cx
                    .try_global::<Started>()
                    .and_then(|started| started.shell.clone());
                let host = shell.as_ref().map(|shell| shell.host.clone());
                let hub = host.as_ref().map(|host| host.hub().clone());
                let window = host.as_ref().and_then(|host| host.window());
                let reader = reader.get();
                handle(
                    request,
                    &Targets {
                        workspace: hub.as_deref(),
                        reader: reader.as_deref(),
                        window,
                        host: host.as_ref(),
                        kernel: Some(&kernel_handle),
                        reporter: Some(for_requests.clone()),
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

/// What a request acts on: the workspace of the window, the reader, the
/// window, the workspace's handle (to record frames), the kernel (the
/// yield switch) and the probe's error channel (for what happens after the
/// request returns). Each is None while it is not there yet.
#[derive(Default)]
pub(crate) struct Targets<'a> {
    pub workspace: Option<&'a Hub>,
    pub reader: Option<&'a dyn Reader>,
    pub window: Option<AnyWindowHandle>,
    pub host: Option<&'a WorkspaceHost>,
    pub kernel: Option<&'a Arc<Kernel>>,
    pub reporter: Option<Arc<dyn Reporter>>,
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
        BenchRequest::Scroll { yielding } => match (targets.host, targets.window) {
            (Some(host), Some(window)) => {
                if yielding {
                    match targets.kernel {
                        Some(kernel) => kernel.set_yield(true),
                        None => {
                            errors.error("open scroll without a kernel");
                            return;
                        }
                    }
                }
                let Some(reporter) = targets.reporter.clone() else {
                    errors.error("open scroll without a reporter");
                    return;
                };
                let run = ScrollRun {
                    host: host.clone(),
                    kernel: targets.kernel.cloned(),
                    reporter,
                    yielding,
                };
                if scroll(run, window, cx).is_err() {
                    errors.error("open scroll without a window");
                }
            }
            (None, _) => errors.error("open scroll without a workspace"),
            (_, None) => errors.error("open scroll without a window"),
        },
        BenchRequest::Quit => quit::quit(cx),
    }
}

/// What the frames of a scroll need, passed from one frame callback to the
/// next.
pub(crate) struct ScrollRun {
    host: WorkspaceHost,
    kernel: Option<Arc<Kernel>>,
    reporter: Arc<dyn Reporter>,
    /// The yield switch was turned on for this scroll and goes off at its
    /// end (also when it does not start).
    yielding: bool,
}

impl ScrollRun {
    /// The scroll is over: the yield switch goes back off.
    fn yield_off(&self) {
        if let (true, Some(kernel)) = (self.yielding, &self.kernel) {
            kernel.set_yield(false);
        }
    }
}

/// How many frames the scroll waits for the window to be the active one
/// before it gives up: the activation answers from the platform a little
/// after `activate_window` (and an inactive window with pending frame
/// callbacks is drawn at about 30 fps). 120 frames is 4 s at that rate.
pub(crate) const ACTIVATION_FRAMES: u32 = 120;

/// Where a scroll is: waiting for the window to become the active one (the
/// frames waited so far), or sending the wheel event of a step.
#[derive(Clone, Copy)]
enum ScrollPhase {
    Activating(u32),
    Events(u32),
}

/// The scroll scenario (docs/design/bench.md「进程级基准」): the window is
/// made the active one (an inactive window is drawn at about 30 fps, which
/// no frame budget survives: platform design「帧的时刻」), and the scenario
/// starts only in the frame in which it is: the platform tells the window
/// after `activate_window` returns, so an immediate start would record
/// 33 ms frames. A window that is not active after [`ACTIVATION_FRAMES`]
/// frames is an `Error`, not a slow recording. Then `workspace` records the
/// window's frames until the end, and in each frame handled a wheel event
/// arrives at the window's centre, where the reading view (or its editor)
/// is: [`SCROLL_DOWN_EVENTS`] events scroll down by [`SCROLL_STEP_PX`]
/// pixels, the remaining ones of the [`SCROLL_EVENTS`] back up; the events
/// are those the platform sends, through `Window::dispatch_event`.
/// [`SCROLL_TAIL_FRAMES`] frames after the last one the recording stops and
/// goes to `on_frames`; the log says how much background work the kernel
/// still had then (`app/scroll_frames_indexing` checks it). Main thread.
pub(crate) fn scroll(run: ScrollRun, window: AnyWindowHandle, cx: &mut App) -> gpui::Result<()> {
    window.update(cx, |_, window, _| {
        window.activate_window();
        scroll_frame(run, window, ScrollPhase::Activating(0));
    })
}

/// The callback of one frame of the scroll.
fn scroll_frame(run: ScrollRun, window: &mut Window, phase: ScrollPhase) {
    window.on_next_frame(move |window, cx| match phase {
        ScrollPhase::Activating(waited) => {
            if window.is_window_active() {
                run.host
                    .record_frames(Name::Borrowed("scroll"), RecordStop::Manual);
                scroll_frame(run, window, ScrollPhase::Events(0));
            } else if waited >= ACTIVATION_FRAMES {
                run.yield_off();
                run.reporter.error("open scroll window not active");
            } else {
                scroll_frame(run, window, ScrollPhase::Activating(waited + 1));
            }
        }
        ScrollPhase::Events(step) => {
            if step < SCROLL_EVENTS {
                let down = step < SCROLL_DOWN_EVENTS;
                let size = window.viewport_size();
                window.dispatch_event(
                    PlatformInput::ScrollWheel(ScrollWheelEvent {
                        position: point(size.width / 2.0, size.height / 2.0),
                        delta: ScrollDelta::Pixels(point(
                            px(0.0),
                            px(if down {
                                -SCROLL_STEP_PX
                            } else {
                                SCROLL_STEP_PX
                            }),
                        )),
                        touch_phase: TouchPhase::Moved,
                        ..ScrollWheelEvent::default()
                    }),
                    cx,
                );
            } else if step >= SCROLL_EVENTS + SCROLL_TAIL_FRAMES {
                if let Some(kernel) = &run.kernel {
                    info!("bench scroll ended"; background_items = kernel.background().count);
                }
                run.yield_off();
                run.host.stop_recording();
                return;
            }
            scroll_frame(run, window, ScrollPhase::Events(step + 1));
        }
    });
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
