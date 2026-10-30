//! The measurement build's wiring (docs/design/app.md「测量构建的装配」
//! 「测试」): the pure choice of the frame autosave name, and the runner's
//! requests on their way to the main thread. The probe thread and the socket
//! are in `lectern-bench`'s tests and in `tests/bench_probe.rs`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc::unbounded;
use gpui::TestAppContext;
use lectern_bench::probe::Driver;
use lectern_bench::wire::{self, OpenTarget};
use lectern_i18n::{Locale, Strings};
use lectern_kernel::Name;
use lectern_logging::EarlyConfig;
use lectern_services::review::ReviewTarget;
use lectern_services::session::{SessionKey, SourceId};
use lectern_services::workspace::{
    ContributionKind, Milestone, Payload, Route, SharedPayload, Target, WorkspaceCommand, screens,
};
use lectern_ui::Appearance;
use lectern_workspace::registry::Payloads;
use lectern_workspace::{Hub, Item, Queued};

use crate::bench::{BenchDriver, BenchRequest, Reporter, handle, wire_milestone};
use crate::profile::Profile;
use crate::quit;
use crate::startup::Startup;
use crate::window::{self};
use crate::window_state::{WELCOME, autosave_name};

/// Records what the probe would have sent to the runner as `Error`.
#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Recorder {
    fn errors(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl Reporter for Recorder {
    fn error(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_owned());
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
        OpenTarget::File(PathBuf::from("/f")),
        OpenTarget::Worktree(PathBuf::from("/w")),
        OpenTarget::Scroll,
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
        [
            "open file unsupported",
            "open worktree unsupported",
            "open scroll unsupported",
            "open animate unsupported",
            "open input unsupported",
        ]
    );
    // None of them reaches the main thread, and the path of an unsupported
    // kind is not in the message.
    assert!(incoming.try_recv().is_err());
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
            Some(&*hub),
            &recorder,
            cx,
        );
        handle(BenchRequest::OpenDiff(key.clone()), None, &recorder, cx);
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
            Some(&*hub),
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
            None,
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
        handle(BenchRequest::Quit, None, &Recorder::default(), cx);
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
