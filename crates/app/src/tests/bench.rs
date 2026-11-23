//! The measurement build's wiring (docs/design/app.md「测量构建的装配」
//! 「测试」): the pure choice of the frame autosave name, and the runner's
//! requests on their way to the main thread. The probe thread and the socket
//! are in `lectern-bench`'s tests and in `tests/bench_probe.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::channel::mpsc::unbounded;
use gpui::{Keystroke, TestAppContext};
use lectern_bench::probe::Driver;
use lectern_bench::wire::{self, FrameStats, OpenTarget, PresentedAt, TimeSource};
use lectern_i18n::{Locale, Strings};
use lectern_kernel::Name;
use lectern_logging::EarlyConfig;
use lectern_platform::HookError;
use lectern_platform::frame::{self as platform_frame, FrameProblems, FrameRecording};
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{
    ActiveTree, BlobId, FileList, Head, ProjectKey, ProjectSnapshot, Scan, Worktree, WorktreeKey,
};
use lectern_services::reader::{Anchor, ObjectShare, OpenTicket, ReadTarget, Reader};
use lectern_services::review::ReviewTarget;
use lectern_services::session::{SessionKey, SourceId};
use lectern_services::workspace::{
    ContributionKind, Milestone, Payload, Route, SharedPayload, Target, WorkspaceCommand, screens,
};
use lectern_ui::Appearance;
use lectern_workspace::registry::Payloads;
use lectern_workspace::{FrameRecord, Hub, Item, Queued};

use crate::bench::{
    BenchDriver, BenchRequest, Reporter, Targets, frame_stats, handle, open_changes, open_file,
    read_target, report_frames, switch_worktree, wire_milestone,
};
use crate::profile::Profile;
use crate::quit;
use crate::startup::Startup;
use crate::window::{self};
use crate::window_state::{WELCOME, autosave_name};

/// Records what the probe would have sent to the runner: each `Error`, and
/// the scenario of each `Frames`.
#[derive(Default)]
struct Recorder(Mutex<Vec<String>>, Mutex<Vec<FrameStats>>);

impl Recorder {
    fn errors(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    fn frames(&self) -> Vec<FrameStats> {
        self.1.lock().unwrap().clone()
    }
}

impl Reporter for Recorder {
    fn error(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_owned());
    }

    fn frames(&self, stats: FrameStats) {
        self.1.lock().unwrap().push(stats);
    }
}

/// What a request acts on when only the workspace is there.
fn at(workspace: Option<&Hub>) -> Targets<'_> {
    Targets {
        workspace,
        ..Targets::default()
    }
}

/// No contribution is made in these tests.
struct NoPayloads;

impl Payloads for NoPayloads {
    fn accepts(&self, _: &ContributionKind, _: &Payload) -> bool {
        false
    }

    fn share(&self, _: Payload) -> Option<SharedPayload> {
        None
    }
}

#[test]
fn no_frame_autosave_name_with_the_bench_probe() {
    // The desktop profile names its windows, `window_state` renames them
    // with the same function.
    assert_eq!(autosave_name(true, false, WELCOME), Some(WELCOME));
    assert_eq!(autosave_name(true, false, "project"), Some("project"));
    // With the probe, the skeleton window and `window_state` get none.
    assert_eq!(autosave_name(true, true, WELCOME), None);
    assert_eq!(autosave_name(true, true, "project"), None);
    // The other profiles never name a window, probe or not.
    assert_eq!(autosave_name(false, false, WELCOME), None);
    assert_eq!(autosave_name(false, true, WELCOME), None);
}

#[test]
fn only_a_project_is_forwarded_and_the_rest_is_an_error() {
    let (requests, mut incoming) = unbounded();
    let recorder = Arc::new(Recorder::default());
    let driver = BenchDriver::new(requests, recorder.clone());
    driver.open(OpenTarget::Project(PathBuf::from("/p")));
    driver.quit();
    assert_eq!(
        incoming.try_recv().unwrap(),
        BenchRequest::Open(PathBuf::from("/p"))
    );
    assert_eq!(incoming.try_recv().unwrap(), BenchRequest::Quit);
    assert!(recorder.errors().is_empty());
}

#[test]
fn open_is_reported_as_unsupported() {
    let (requests, mut incoming) = unbounded();
    let recorder = Arc::new(Recorder::default());
    let driver = BenchDriver::new(requests, recorder.clone());
    for target in [
        OpenTarget::Animate,
        OpenTarget::Input {
            text: "x".to_owned(),
            interval: Duration::from_millis(10),
        },
    ] {
        driver.open(target);
    }
    assert_eq!(
        recorder.errors(),
        ["open animate unsupported", "open input unsupported"]
    );
    // None of them reaches the main thread, and the path of an unsupported
    // kind is not in the message.
    assert!(incoming.try_recv().is_err());
}

#[test]
fn a_scroll_is_forwarded_with_its_yield_switch() {
    let (requests, mut incoming) = unbounded();
    let recorder = Arc::new(Recorder::default());
    let driver = BenchDriver::new(requests, recorder.clone());
    driver.open(OpenTarget::Scroll { yielding: false });
    driver.open(OpenTarget::Scroll { yielding: true });
    assert_eq!(
        incoming.try_recv().unwrap(),
        BenchRequest::Scroll { yielding: false }
    );
    assert_eq!(
        incoming.try_recv().unwrap(),
        BenchRequest::Scroll { yielding: true }
    );
    assert!(recorder.errors().is_empty());
}

/// A scroll acts on the workspace's handle: without it there is nothing to
/// record, and the runner must hear of it.
#[gpui::test]
fn a_scroll_without_a_workspace_is_an_error(cx: &mut TestAppContext) {
    let recorder = Recorder::default();
    cx.update(|cx| {
        handle(
            BenchRequest::Scroll { yielding: false },
            &Targets::default(),
            &recorder,
            cx,
        );
    });
    assert_eq!(recorder.errors(), ["open scroll without a workspace"]);
}

fn micros(us: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_micros(us)
}

fn recording(problems: FrameProblems) -> FrameRecording {
    FrameRecording {
        refresh: Duration::from_micros(8_333),
        frames: vec![
            platform_frame::FrameTimes {
                requested: micros(100),
                started: micros(150),
                committed: Some(micros(400)),
                gpu_started: Some(micros(500)),
                completed: micros(900),
                presented: Some(platform_frame::PresentedAt {
                    time: micros(1_200),
                    source: platform_frame::TimeSource::Presented,
                }),
            },
            platform_frame::FrameTimes {
                requested: micros(8_433),
                started: micros(8_433),
                committed: None,
                gpu_started: None,
                completed: micros(9_000),
                presented: None,
            },
        ],
        problems,
    }
}

fn record(result: Result<FrameRecording, HookError>) -> FrameRecord {
    FrameRecord {
        scenario: Name::Borrowed("scroll"),
        result,
    }
}

/// A clean recording becomes the `Frames` report with each frame's four
/// moments, the presented source, the refresh and the frames without a tick.
#[test]
fn a_clean_recording_is_the_frames_report() {
    let problems = FrameProblems {
        without_tick: 1,
        ..FrameProblems::default()
    };
    let stats = frame_stats(record(Ok(recording(problems)))).unwrap();
    assert_eq!(stats.scenario, "scroll");
    assert_eq!(stats.refresh, Duration::from_micros(8_333));
    assert_eq!(stats.without_tick, 1);
    assert_eq!(stats.frames.len(), 2);
    assert_eq!(
        stats.frames[0],
        wire::FrameTimes {
            requested: micros(100),
            started: micros(150),
            committed: Some(micros(400)),
            gpu_started: Some(micros(500)),
            completed: micros(900),
            presented: Some(PresentedAt {
                time: micros(1_200),
                source: TimeSource::Presented,
            }),
        }
    );
    assert_eq!(stats.frames[1].presented, None);
    assert_eq!(stats.frames[1].committed, None);
    assert_eq!(stats.frames[1].gpu_started, None);

    let recorder = Recorder::default();
    report_frames(record(Ok(recording(problems))), &recorder);
    assert_eq!(recorder.frames(), [stats]);
    assert!(recorder.errors().is_empty());
}

/// A recording that did not start, or that is not clean, is an `Error` with
/// the messages of the bench design (`Probe::frames`), never `Frames`.
#[test]
fn an_unclean_or_failed_recording_is_an_error() {
    let recorder = Recorder::default();
    report_frames(record(Err(HookError::NoMetalLayer)), &recorder);
    report_frames(record(Err(HookError::Unsupported)), &recorder);
    report_frames(record(Err(HookError::NotMainThread)), &recorder);
    report_frames(
        record(Ok(recording(FrameProblems {
            unmatched: 1,
            incomplete: 2,
            overflowed: 3,
            without_tick: 0,
        }))),
        &recorder,
    );
    // One problem is enough, and a frame the display link did not ask for is
    // only reported.
    report_frames(
        record(Ok(recording(FrameProblems {
            incomplete: 1,
            ..FrameProblems::default()
        }))),
        &recorder,
    );
    assert_eq!(
        recorder.errors(),
        [
            "frames no_metal_layer",
            "frames unsupported",
            "frames not_main_thread",
            "frames unmatched=1 incomplete=2 overflowed=3",
            "frames unmatched=0 incomplete=1 overflowed=0",
        ]
    );
    assert!(recorder.frames().is_empty());
}

#[test]
fn a_worktree_is_forwarded_with_its_root() {
    let (requests, mut incoming) = unbounded();
    let recorder = Arc::new(Recorder::default());
    let driver = BenchDriver::new(requests, recorder.clone());
    driver.open(OpenTarget::Worktree(PathBuf::from("/repos/a-w1")));
    assert_eq!(
        incoming.try_recv().unwrap(),
        BenchRequest::OpenWorktree(PathBuf::from("/repos/a-w1"))
    );
    assert!(recorder.errors().is_empty());
}

#[test]
fn a_diff_is_forwarded_with_its_session_key() {
    let (requests, mut incoming) = unbounded();
    let recorder = Arc::new(Recorder::default());
    let driver = BenchDriver::new(requests, recorder.clone());
    driver.open(OpenTarget::Diff {
        session: "session-claude-code:1111/agent-a".to_owned(),
    });
    assert_eq!(
        incoming.try_recv().unwrap(),
        BenchRequest::OpenDiff(SessionKey {
            source: SourceId("session-claude-code".into()),
            id: "1111/agent-a".into(),
        })
    );
    // Without a source, or with an empty part, nothing is opened.
    for session in ["1111", ":1111", "session-claude-code:"] {
        driver.open(OpenTarget::Diff {
            session: session.to_owned(),
        });
    }
    assert!(incoming.try_recv().is_err());
    assert_eq!(recorder.errors(), ["open diff without a source"; 3]);
}

#[test]
fn milestones_keep_their_names() {
    for (milestone, wire) in [
        (Milestone::ProjectHome, wire::Milestone::ProjectHome),
        (Milestone::FileHighlighted, wire::Milestone::FileHighlighted),
        (Milestone::DiffShown, wire::Milestone::DiffShown),
        (
            Milestone::WorktreeSwitched,
            wire::Milestone::WorktreeSwitched,
        ),
        (Milestone::FilesShown, wire::Milestone::FilesShown),
        (Milestone::FilesExpanded, wire::Milestone::FilesExpanded),
        (Milestone::EditReady, wire::Milestone::EditReady),
    ] {
        assert_eq!(wire_milestone(milestone), wire);
    }
}

#[gpui::test]
fn open_diff_takes_the_session_row_path(cx: &mut TestAppContext) {
    let queued = Arc::new(Mutex::new(Vec::<Queued>::new()));
    let sink = queued.clone();
    let hub = Hub::new(
        Box::new(NoPayloads),
        Arc::new(move |item| sink.lock().unwrap().push(item)),
    );
    let recorder = Recorder::default();
    let key = SessionKey {
        source: SourceId("session-claude-code".into()),
        id: "s00".into(),
    };
    cx.update(|cx| {
        handle(
            BenchRequest::OpenDiff(key.clone()),
            &at(Some(&*hub)),
            &recorder,
            cx,
        );
        handle(
            BenchRequest::OpenDiff(key.clone()),
            &at(None),
            &recorder,
            cx,
        );
    });
    // The command ↵ on the session's row makes.
    let queued = queued.lock().unwrap();
    assert_eq!(queued.len(), 1, "{queued:?}");
    match &queued[0].item {
        Item::Command(command) => assert_eq!(
            *command,
            WorkspaceCommand::Push(Route {
                screen: Name::Borrowed(screens::REVIEW),
                target: Target::Review(ReviewTarget::Session(key)),
            })
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(recorder.errors(), ["open diff without a workspace"]);
}

/// A git project `a` with the worktrees `main` (current), `w1`, `w2` and
/// `gone` (prunable), roots at `/repos/a-<name>`.
fn project() -> Arc<ProjectSnapshot> {
    let tree = |name: &str, prunable: bool| Worktree {
        key: WorktreeKey(Arc::from(Path::new(&format!(
            "/repos/a/.git/worktrees/{name}"
        )))),
        root: Arc::from(Path::new(&format!("/repos/a-{name}"))),
        is_main: false,
        head: Head::Unknown,
        locked: false,
        prunable,
    };
    let worktrees = vec![
        tree("main", false),
        tree("w1", false),
        tree("w2", false),
        tree("gone", true),
    ];
    Arc::new(ProjectSnapshot {
        key: ProjectKey::Git {
            common_dir: Arc::from(Path::new("/repos/a/.git")),
        },
        generation: 1,
        name: Arc::from("a"),
        active: Some(ActiveTree {
            root: worktrees[0].root.clone(),
            worktree: Some(worktrees[0].key.clone()),
            opened_at: Arc::from(""),
            files: Scan::Ready(Arc::new(FileList::builder().build())),
            packages: Scan::Ready(Arc::from(Vec::new())),
            dirs: Scan::Pending,
        }),
        worktrees: Arc::from(worktrees),
    })
}

fn queue_hub() -> (Arc<Hub>, Arc<Mutex<Vec<Queued>>>) {
    let queued = Arc::new(Mutex::new(Vec::<Queued>::new()));
    let sink = queued.clone();
    let hub = Hub::new(
        Box::new(NoPayloads),
        Arc::new(move |item| sink.lock().unwrap().push(item)),
    );
    (hub, queued)
}

fn commands(queued: &Mutex<Vec<Queued>>) -> Vec<WorkspaceCommand> {
    queued
        .lock()
        .unwrap()
        .iter()
        .map(|queued| match &queued.item {
            Item::Command(command) => command.clone(),
            other => panic!("{other:?}"),
        })
        .collect()
}

/// A reader that records what it was asked to open.
#[derive(Default)]
struct Opened(Mutex<Vec<ReadTarget>>);

impl Reader for Opened {
    fn open(&self, target: ReadTarget) -> OpenTicket {
        self.0.lock().unwrap().push(target);
        OpenTicket(0)
    }

    fn prefetch(&self, _: Vec<ReadTarget>) {}

    fn share_object(&self, _: BlobId, _: Arc<[u8]>) -> ObjectShare {
        ObjectShare::new(|| {})
    }
}

#[test]
fn the_changes_review_pushes_the_outside_route_of_the_active_worktree() {
    let (hub, queued) = queue_hub();
    let recorder = Recorder::default();
    let open = [project()];
    open_changes(&*hub, &open, &recorder);
    assert_eq!(
        commands(&queued),
        [WorkspaceCommand::Push(Route {
            screen: Name::Borrowed(screens::REVIEW),
            target: Target::Review(ReviewTarget::Outside {
                project: open[0].key.clone(),
                worktree: open[0].active.as_ref().unwrap().worktree.clone().unwrap(),
            }),
        })]
    );
    assert!(recorder.errors().is_empty());
    // No project, or a folder project (no worktree, no base): an error.
    open_changes(&*hub, &[], &recorder);
    let mut folder = (*open[0]).clone();
    folder.active.as_mut().unwrap().worktree = None;
    open_changes(&*hub, &[Arc::new(folder)], &recorder);
    assert_eq!(recorder.errors(), ["open changes without a git project"; 2]);
    assert_eq!(commands(&queued).len(), 1);
}

#[test]
fn changes_are_forwarded() {
    let (requests, mut incoming) = unbounded();
    let recorder = Arc::new(Recorder::default());
    BenchDriver::new(requests, recorder.clone()).open(OpenTarget::Changes);
    assert_eq!(incoming.try_recv().unwrap(), BenchRequest::OpenChanges);
    assert!(recorder.errors().is_empty());
}

#[test]
fn a_file_is_forwarded_with_its_anchor_and_old_side() {
    let (requests, mut incoming) = unbounded();
    let recorder = Arc::new(Recorder::default());
    let driver = BenchDriver::new(requests, recorder.clone());
    driver.open(OpenTarget::File {
        path: PathBuf::from("/repos/a-main/src/lib.rs"),
        byte: Some(12),
        old: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
    });
    assert_eq!(
        incoming.try_recv().unwrap(),
        BenchRequest::OpenFile {
            path: PathBuf::from("/repos/a-main/src/lib.rs"),
            byte: Some(12),
            old: BlobId::from_hex("0123456789abcdef0123456789abcdef01234567"),
        }
    );
    // An object ID that is not hex is an error, and nothing is sent.
    driver.open(OpenTarget::File {
        path: PathBuf::from("/f"),
        byte: None,
        old: Some("not hex".to_owned()),
    });
    assert!(incoming.try_recv().is_err());
    assert_eq!(recorder.errors(), ["open file with a bad object ID"]);
}

#[test]
fn files_and_keys_are_forwarded() {
    let (requests, mut incoming) = unbounded();
    let recorder = Arc::new(Recorder::default());
    let driver = BenchDriver::new(requests, recorder.clone());
    driver.open(OpenTarget::Files);
    driver.open(OpenTarget::Key("cmd-[".to_owned()));
    driver.open(OpenTarget::Key("right".to_owned()));
    assert_eq!(incoming.try_recv().unwrap(), BenchRequest::ShowFiles);
    assert_eq!(
        incoming.try_recv().unwrap(),
        BenchRequest::Key(Keystroke::parse("cmd-[").unwrap())
    );
    assert_eq!(
        incoming.try_recv().unwrap(),
        BenchRequest::Key(Keystroke::parse("right").unwrap())
    );
    assert!(recorder.errors().is_empty());
    // A keystroke GPUI cannot parse is an error and is not sent.
    driver.open(OpenTarget::Key("a-b".to_owned()));
    assert!(incoming.try_recv().is_err());
    assert_eq!(recorder.errors(), ["open key with a bad keystroke"]);
}

#[test]
fn a_file_target_is_relative_to_the_active_tree_of_its_project() {
    let open = [project()];
    let oid = BlobId::from_hex("0123456789abcdef0123456789abcdef01234567");
    let target = |path: &str, byte, old| read_target(&open, Path::new(path), byte, old);
    let main = open[0].active.as_ref().unwrap().worktree.clone();
    // The worktree's file from its start.
    let start = target("/repos/a-main/packages/p00/src/lib.rs", None, None).unwrap();
    assert_eq!(start.project, open[0].key);
    assert_eq!(start.worktree, main);
    assert_eq!(&*start.path, "packages/p00/src/lib.rs");
    assert_eq!(start.origin, TextOrigin::Worktree);
    assert_eq!((start.expect, start.at), (None, Anchor::Start));
    // At a byte offset (a hunk of a diff of the worktree's file).
    let middle = target("/repos/a-main/package-lock.json", Some(2_097_152), None).unwrap();
    assert_eq!(middle.origin, TextOrigin::Worktree);
    assert_eq!(
        middle.at,
        Anchor::Byte {
            byte: 2_097_152,
            line: 0,
            column: 0
        }
    );
    // The old side: the blob, which the open diff shared.
    let old = target("/repos/a-main/package-lock.json", Some(7), oid).unwrap();
    assert_eq!((old.origin, old.expect), (TextOrigin::Object, oid));
    // A path outside every open project's active tree is not opened.
    assert!(target("/repos/a-w1/src/lib.rs", None, None).is_none());
    assert!(target("/elsewhere/lib.rs", None, None).is_none());
    assert!(read_target(&[], Path::new("/repos/a-main/x"), None, None).is_none());
}

#[test]
fn a_file_is_opened_through_the_reader_or_is_an_error_without_the_path() {
    let reader = Opened::default();
    let recorder = Recorder::default();
    let open = [project()];
    open_file(
        &reader,
        &open,
        Path::new("/repos/a-main/src/lib.rs"),
        None,
        None,
        &recorder,
    );
    open_file(
        &reader,
        &open,
        Path::new("/repos/a-nobody/src/lib.rs"),
        None,
        None,
        &recorder,
    );
    let asked = reader.0.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(&*asked[0].path, "src/lib.rs");
    assert_eq!(recorder.errors(), ["open file not found in a project"]);
}

#[gpui::test]
fn files_and_file_requests_take_their_paths(cx: &mut TestAppContext) {
    let (hub, queued) = queue_hub();
    let recorder = Recorder::default();
    let reader = Opened::default();
    cx.update(|cx| {
        // The Files entry's command.
        handle(BenchRequest::ShowFiles, &at(Some(&*hub)), &recorder, cx);
        handle(BenchRequest::ShowFiles, &at(None), &recorder, cx);
        // A workspace whose `dyn Projects` has not started has no project.
        let targets = Targets {
            workspace: Some(&*hub),
            reader: Some(&reader),
            ..Targets::default()
        };
        let file = || BenchRequest::OpenFile {
            path: PathBuf::from("/repos/a-main/src/lib.rs"),
            byte: None,
            old: None,
        };
        handle(file(), &targets, &recorder, cx);
        handle(file(), &at(Some(&*hub)), &recorder, cx);
        handle(file(), &at(None), &recorder, cx);
        // A key needs the window.
        handle(
            BenchRequest::Key(Keystroke::parse("cmd-e").unwrap()),
            &at(Some(&*hub)),
            &recorder,
            cx,
        );
    });
    assert_eq!(
        commands(&queued),
        [WorkspaceCommand::ShowSidebarPanel(Some(Name::Borrowed(
            "files"
        )))]
    );
    assert!(reader.0.lock().unwrap().is_empty());
    assert_eq!(
        recorder.errors(),
        [
            "open files without a workspace",
            "open file not found in a project",
            "open file without a reader",
            "open file without a workspace",
            "open key without a window",
        ]
    );
}

/// The Worktrees screen's button: the screen is pushed, then the worktree
/// with that root becomes the current one.
#[test]
fn a_worktree_switch_pushes_the_screen_and_sets_the_worktree() {
    let (hub, queued) = queue_hub();
    let recorder = Recorder::default();
    switch_worktree(&*hub, &[project()], Path::new("/repos/a-w2"), &recorder);
    assert_eq!(
        commands(&queued),
        [
            WorkspaceCommand::Push(Route {
                screen: Name::Borrowed(screens::WORKTREES),
                target: Target::None,
            }),
            WorkspaceCommand::SetActiveWorktree(WorktreeKey(Arc::from(Path::new(
                "/repos/a/.git/worktrees/w2"
            )))),
        ]
    );
    assert!(recorder.errors().is_empty());
}

#[test]
fn a_worktree_switch_that_cannot_happen_is_an_error_without_the_path() {
    let (hub, queued) = queue_hub();
    let recorder = Recorder::default();
    let open = [project()];
    // The current worktree, a prunable one, one nobody has, and no project.
    for root in ["/repos/a-main", "/repos/a-gone", "/repos/a-nobody"] {
        switch_worktree(&*hub, &open, Path::new(root), &recorder);
    }
    switch_worktree(&*hub, &[], Path::new("/repos/a-w1"), &recorder);
    assert_eq!(
        recorder.errors(),
        [
            "open worktree is the current one",
            "open worktree not found",
            "open worktree not found",
            "open worktree not found",
        ]
    );
    assert!(commands(&queued).is_empty());
}

#[gpui::test]
fn a_worktree_request_without_a_workspace_or_a_project_is_an_error(cx: &mut TestAppContext) {
    let (hub, queued) = queue_hub();
    let recorder = Recorder::default();
    cx.update(|cx| {
        let root = || PathBuf::from("/repos/a-w1");
        handle(BenchRequest::OpenWorktree(root()), &at(None), &recorder, cx);
        // A workspace whose `dyn Projects` has not started has no project.
        handle(
            BenchRequest::OpenWorktree(root()),
            &at(Some(&*hub)),
            &recorder,
            cx,
        );
    });
    assert_eq!(
        recorder.errors(),
        [
            "open worktree without a workspace",
            "open worktree not found"
        ]
    );
    assert!(commands(&queued).is_empty());
}

#[gpui::test]
fn open_project_takes_the_open_folder_path(cx: &mut TestAppContext) {
    let queued = Arc::new(Mutex::new(Vec::<Queued>::new()));
    let sink = queued.clone();
    let hub = Hub::new(
        Box::new(NoPayloads),
        Arc::new(move |item| sink.lock().unwrap().push(item)),
    );
    let recorder = Recorder::default();
    cx.update(|cx| {
        handle(
            BenchRequest::Open(PathBuf::from("/repos/a")),
            &at(Some(&*hub)),
            &recorder,
            cx,
        );
    });
    // The command the Open Folder dialog's choice makes, in the workspace's
    // queue.
    let queued = queued.lock().unwrap();
    assert_eq!(queued.len(), 1, "{queued:?}");
    match &queued[0].item {
        Item::Command(command) => assert_eq!(
            *command,
            WorkspaceCommand::OpenProject(PathBuf::from("/repos/a"))
        ),
        other => panic!("{other:?}"),
    }
    assert!(recorder.errors().is_empty());
}

#[gpui::test]
fn open_project_without_a_workspace_is_an_error(cx: &mut TestAppContext) {
    let recorder = Recorder::default();
    cx.update(|cx| {
        handle(
            BenchRequest::Open(PathBuf::from("/repos/a")),
            &at(None),
            &recorder,
            cx,
        );
    });
    assert_eq!(recorder.errors(), ["open project without a workspace"]);
}

/// `Quit` runs the quit order of ⌘Q: the marker is set, and the log of the
/// run says `quit requested` by the action, then ends with `run end`. The
/// order of the steps is tested with fake steps in `quit.rs`.
#[gpui::test]
fn a_quit_request_takes_the_quit_path(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let early = lectern_logging::install_early(EarlyConfig {
        dir: Some(dir.path().to_owned()),
        cli_filter: None,
    })
    .expect("one install per test process");
    cx.update(|cx| {
        cx.set_global(Startup::new(early, Profile::Test));
        lectern_ui::init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false);
        // The test profile releases the barrier once the window exists.
        window::open(cx);
        assert!(!quit::is_quitting(cx));
        handle(BenchRequest::Quit, &at(None), &Recorder::default(), cx);
        assert!(quit::is_quitting(cx));
    });
    let start = Instant::now();
    let log = loop {
        let text = log_text(dir.path());
        if text.contains("\"run end\"") {
            break text;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "{text}");
        std::thread::sleep(Duration::from_millis(10));
    };
    let requested = log.find("\"quit requested\"").expect("quit requested");
    assert!(log[requested..].contains("\"path\":\"action\""), "{log}");
    assert!(requested < log.find("\"run end\"").unwrap(), "{log}");
}

/// The text of this run's log file; empty while there is none.
fn log_text(dir: &std::path::Path) -> String {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("lectern-"))
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .collect()
}
