//! The measurement build's file benchmarks reach their targets (docs/design/
//! app.md「文件阅读与 Files 的目标」): a real `Workspace` as a window's root
//! in GPUI's test platform, the desktop profile's real plugin set
//! (`project`, `file-reader`, `code-intel`, `review`, ... as `lectern`
//! starts them) started in a real `Kernel` over a folder in a temporary
//! directory, and the runner's requests carried out by `bench::handle`
//! exactly as the main-thread task does: `Reader::open` through the
//! `ReaderSlot`, `ShowSidebarPanel`, and `Window::dispatch_keystroke` on the
//! window `WorkspaceHost::window()` names. Each test is one sequence of a
//! benchmark and ends in the milestone that ends its sample; it fails when
//! the key does not reach the view (the focus).
//!
//! No real window: the milestones are reported in the frame callbacks the
//! test platform runs (`simulate_next_frame`).

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{AnyWindowHandle, Context, IntoElement, Keystroke, Render, TestAppContext, Window, div};
use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_file_reader::ReaderSlot;
use lectern_fs_watch::Watcher;
use lectern_fs_watch::testing::FakeBackend;
use lectern_i18n::{Locale, Strings};
use lectern_kernel::Kernel;
use lectern_services::project::Scan;
use lectern_services::workspace::Milestone;
use lectern_ui::Appearance;
use lectern_ui::semantic::Recorder;
use lectern_workspace::{WorkspaceDeps, WorkspaceHost, install};

use crate::bench::{BenchRequest, Reporter, Targets, handle};
use crate::profile::{Profile, Resources};

/// The window's root before `install`.
struct Placeholder;

impl Render for Placeholder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// What the probe would have sent as `Error`.
#[derive(Default)]
struct Errors(Mutex<Vec<String>>);

impl Reporter for Errors {
    fn error(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_owned());
    }
}

/// The folder the benchmarks open: two small TypeScript files and a
/// directory (the first row of the Files tree).
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    // `project` reports canonical roots; the temporary directory of macOS
    // is behind a symbolic link.
    let root = dir.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    for (name, text) in [
        ("a.ts", "// the first file\nexport const a = 1;\n"),
        ("b.ts", "// the second file\nexport const b = 2;\n"),
        ("wide/one.txt", "1\n"),
        ("wide/two.txt", "2\n"),
        ("wide/three.txt", "3\n"),
    ] {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    (dir, root)
}

struct Rig {
    window: AnyWindowHandle,
    host: WorkspaceHost,
    kernel: Arc<Kernel>,
    reader: ReaderSlot,
    milestones: Arc<Mutex<Vec<Milestone>>>,
    errors: Errors,
    root: PathBuf,
    _executor: Executor,
    _dir: tempfile::TempDir,
}

impl Rig {
    /// The desktop profile's plugins in a started kernel, the workspace in
    /// the window, the folder opened and its file list and directory table
    /// ready.
    fn open(cx: &mut TestAppContext) -> Rig {
        // The kernel and executor threads wake the foreground tasks.
        cx.executor().allow_parking();
        cx.update(|cx| {
            lectern_ui::init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false);
        });
        let window: AnyWindowHandle = cx.add_window(|_, _| Placeholder).into();
        let milestones = Arc::new(Mutex::new(Vec::new()));
        let seen = milestones.clone();
        let host = cx.update(|cx| {
            install(
                window,
                WorkspaceDeps {
                    on_state_changed: Arc::new(|_| {}),
                    on_milestone: Arc::new(move |milestone, _| {
                        seen.lock().unwrap().push(milestone);
                    }),
                    on_frames: Arc::new(|_| {}),
                },
                cx,
            )
        });
        let (dir, root) = fixture();
        let executor = Executor::new(lectern_executor::Config::default());
        let resources = Resources {
            executor: executor.clone(),
            cache: Cache::new(lectern_cache::Config {
                root: dir.path().join("app_cache/cache"),
                budgets: lectern_cache::Budgets::default(),
                ui_thread: None,
            }),
            watcher: Watcher::new(
                FakeBackend::new(),
                executor.clone(),
                lectern_fs_watch::Config::default(),
            ),
            workspace: host.clone(),
            home: dir.path().join("home"),
            app_cache: dir.path().join("app_cache"),
            app_data: dir.path().join("app_data"),
            git_ceilings: Arc::from([dir.path().to_owned()]),
            reader: ReaderSlot::default(),
        };
        let reader = resources.reader.clone();
        let (set, _) = Profile::Desktop.plugins(&resources);
        let kernel = Kernel::start(set);
        let rig = Rig {
            window,
            host,
            kernel,
            reader,
            milestones,
            errors: Errors::default(),
            root: root.clone(),
            _executor: executor,
            _dir: dir,
        };
        // `file-reader` has activated once the slot holds its reader.
        rig.wait(cx, "the file-reader plugin", |rig| {
            rig.reader.get().is_some()
        });
        rig.request(cx, BenchRequest::Open(root));
        rig.wait(cx, "the project's directory table", |rig| {
            rig.host.hub().open_snapshots().iter().any(|project| {
                project
                    .active
                    .as_ref()
                    .is_some_and(|active| matches!(active.dirs, Scan::Ready(_)))
            })
        });
        rig
    }

    /// One main-thread task of the probe: `bench::handle` with what the
    /// probe's task reads (`attach`).
    fn request(&self, cx: &mut TestAppContext, request: BenchRequest) {
        let reader = self.reader.get();
        let hub = self.host.hub().clone();
        let window = self.host.window();
        cx.update(|cx| {
            handle(
                request,
                &Targets {
                    workspace: Some(hub.as_ref()),
                    reader: reader.as_deref(),
                    window,
                },
                &self.errors,
                cx,
            );
        });
    }

    fn key(&self, cx: &mut TestAppContext, keystroke: &str) {
        self.request(
            cx,
            BenchRequest::Key(Keystroke::parse(keystroke).expect("a keystroke")),
        );
    }

    fn open_file(&self, cx: &mut TestAppContext, name: &str) {
        self.request(
            cx,
            BenchRequest::OpenFile {
                path: self.root.join(name),
                byte: None,
                old: None,
            },
        );
    }

    /// One frame: the queues and threads the test platform does not run on
    /// its own, a draw, and the callbacks of the next frame.
    fn frame(&self, cx: &mut TestAppContext) {
        cx.run_until_parked();
        let mut visual = gpui::VisualTestContext::from_window(self.window, cx);
        visual.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.simulate_next_frame(cx);
        });
        visual.run_until_parked();
    }

    /// Frames until `done`, or a failure naming `what` after 30 s.
    fn wait(&self, cx: &mut TestAppContext, what: &str, done: impl Fn(&Rig) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.frame(cx);
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; milestones {:?}, errors {:?}",
                self.milestones.lock().unwrap(),
                self.errors.0.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Some frames, for what needs no event to wait for.
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..40 {
            self.frame(cx);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn count(&self, milestone: Milestone) -> usize {
        self.milestones
            .lock()
            .unwrap()
            .iter()
            .filter(|m| **m == milestone)
            .count()
    }

    fn forget(&self) {
        self.milestones.lock().unwrap().clear();
    }

    /// The probe's node IDs of the window's last frame.
    fn nodes(&self, cx: &mut TestAppContext) -> Vec<String> {
        self.frame(cx);
        let mut visual = gpui::VisualTestContext::from_window(self.window, cx);
        let id = visual.update(|window, _| window.window_handle().window_id());
        Recorder::begin(id);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        Recorder::finish()
            .into_iter()
            .map(|record| record.semantic.id.as_str().to_owned())
            .collect()
    }

    /// The kernel's shutdown, so no plugin thread outlives the test.
    fn close(self) {
        let (done, finished) = channel();
        self.kernel.shutdown(move || {
            let _ = done.send(());
        });
        assert!(
            finished.recv_timeout(Duration::from_secs(30)).is_ok(),
            "the kernel shut down"
        );
    }
}

/// `app/enter_edit`: a file is opened through the reader, ⌘E reaches the
/// reading view, and the editor's first frame is `EditReady`.
#[gpui::test]
fn bench_keys_cmd_e_enters_the_edit_mode(cx: &mut TestAppContext) {
    let rig = Rig::open(cx);
    rig.open_file(cx, "a.ts");
    rig.wait(cx, "FileHighlighted", |rig| {
        rig.count(Milestone::FileHighlighted) == 1
    });
    // The runner settles here: the whole file is read, so Edit is available.
    rig.settle(cx);
    assert_eq!(rig.count(Milestone::EditReady), 0);
    rig.key(cx, "cmd-e");
    rig.wait(cx, "EditReady", |rig| rig.count(Milestone::EditReady) == 1);
    assert!(rig.errors.0.lock().unwrap().is_empty());
    rig.close();
}

/// `app/nav_back`: two files are opened one after the other, ⌘[ reaches the
/// reading view, and the first file is shown again: `FileHighlighted`.
#[gpui::test]
fn bench_keys_cmd_bracket_goes_back_to_the_first_file(cx: &mut TestAppContext) {
    let rig = Rig::open(cx);
    for (name, shown) in [("a.ts", 1), ("b.ts", 2)] {
        rig.open_file(cx, name);
        rig.wait(cx, "FileHighlighted", |rig| {
            rig.count(Milestone::FileHighlighted) == shown
        });
        rig.settle(cx);
    }
    let shows = |nodes: &[String], name: &str| {
        let key = format!("file-reader.document.item[-:{name}]");
        nodes.contains(&key)
    };
    let before = rig.nodes(cx);
    assert!(
        shows(&before, "b.ts") && !shows(&before, "a.ts"),
        "{before:?}"
    );
    rig.forget();
    rig.key(cx, "cmd-[");
    rig.wait(cx, "FileHighlighted after ⌘[", |rig| {
        rig.count(Milestone::FileHighlighted) == 1
    });
    // The first file, shown from the history.
    let after = rig.nodes(cx);
    assert!(shows(&after, "a.ts") && !shows(&after, "b.ts"), "{after:?}");
    assert!(rig.errors.0.lock().unwrap().is_empty());
    rig.close();
}

/// `app/files_expand`: the Files panel is shown (`FilesShown`), ↓ selects the
/// first row (the directory), and → opens it: `FilesExpanded`.
#[gpui::test]
fn bench_keys_arrows_open_the_first_directory_of_files(cx: &mut TestAppContext) {
    let rig = Rig::open(cx);
    rig.request(cx, BenchRequest::ShowFiles);
    rig.wait(cx, "FilesShown", |rig| {
        rig.count(Milestone::FilesShown) == 1
    });
    rig.settle(cx);
    rig.key(cx, "down");
    rig.settle(cx);
    assert_eq!(rig.count(Milestone::FilesExpanded), 0);
    rig.key(cx, "right");
    rig.wait(cx, "FilesExpanded", |rig| {
        rig.count(Milestone::FilesExpanded) == 1
    });
    let nodes = rig.nodes(cx);
    assert!(
        nodes
            .iter()
            .any(|id| id.starts_with("files.tree.item[wide/")),
        "the directory's children are rows: {nodes:?}"
    );
    assert!(rig.errors.0.lock().unwrap().is_empty());
    rig.close();
}

/// The records of the verbose log so far whose message holds `text`.
fn logged(text: &str) -> usize {
    lectern_logging::testing::capture()
        .lines()
        .iter()
        .filter(|line| line.contains(text))
        .count()
}

const OPEN_QUEUED: &str = "a request to open a file was queued";
const VIEW_BUILT: &str = "the reading view was built";
const FIRST_SCREEN: &str = "the first screen of an opened file was computed";
const DRAWN: &str = "the first screen is drawn; the milestone is reported";

/// `app/open_file` is drawn in the next frame (ADR 0063): the first screen
/// of a first open is computed before any frame ran, so no frame waits for
/// it. The reading view does not exist when `Reader::open` returns: its
/// document's task has started all the same, and the view's first frame
/// takes a document that is ready.
///
/// Frames are counted with the engine settled between them (as if the first
/// screen took no time): 2 before the first screen was started by the open,
/// 1 after.
#[gpui::test]
fn bench_keys_a_first_open_computes_the_first_screen_before_the_view_is_drawn(
    cx: &mut TestAppContext,
) {
    let log = lectern_logging::testing::capture();
    let rig = Rig::open(cx);
    log.clear();
    rig.forget();
    rig.open_file(cx, "a.ts");
    assert_eq!(logged(OPEN_QUEUED), 1);
    assert!(
        log.lines()
            .iter()
            .any(|line| line.contains(OPEN_QUEUED) && line.contains("\"prestarted\":true")),
        "the first screen was started by the open"
    );
    // No frame has run. The first screen is computed all the same.
    let deadline = Instant::now() + Duration::from_secs(30);
    while logged(FIRST_SCREEN) == 0 {
        assert!(
            Instant::now() < deadline,
            "the first screen was not computed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(logged(VIEW_BUILT), 0, "no view yet: nothing drew");
    assert_eq!(logged(FIRST_SCREEN), 1);

    let mut frames = 0;
    while rig.count(Milestone::FileHighlighted) == 0 {
        frames += 1;
        assert!(frames <= 6, "the milestone did not come");
        rig.frame(cx);
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(logged(VIEW_BUILT), 1);
    assert_eq!(logged(DRAWN), 1);
    assert_eq!(logged(FIRST_SCREEN), 1, "one first screen, one document");
    // The first snapshot the view took already showed the first screen (it
    // used to be `opening`, the highlighted one came a frame later).
    assert_eq!(logged("\"state\":\"opening\""), 0);
    assert_eq!(frames, 1, "the view's first frame takes a ready document");
    assert!(rig.errors.0.lock().unwrap().is_empty());
    rig.close();
}
