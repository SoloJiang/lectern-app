//! The window state file and the frame autosave name (docs/design/app.md
//! 「窗口状态」). The file functions run on a temporary directory; the task
//! that debounces and writes runs in a `#[gpui::test]` with the test
//! scheduler's clock and a manual executor.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyWindowHandle, AppContext as _, Context, IntoElement, Render, TestAppContext, Window, div,
};
use lectern_executor::Executor;
use lectern_i18n::{Locale, Strings};
use lectern_ui::Appearance;
use lectern_workspace::{WindowState, WorkspaceDeps, WorkspaceHost, install};

use crate::window_state::{
    PROJECT, Read, Unreadable, WELCOME, WindowStateLink, apply_name, dir, frame_name, read,
    restore, write,
};

fn state(project: Option<&str>, inspector: bool) -> WindowState {
    WindowState {
        version: WindowState::VERSION,
        project: project.map(PathBuf::from),
        inspector,
    }
}

#[test]
fn the_name_follows_the_project() {
    assert_eq!(frame_name(&state(None, false)), WELCOME);
    assert_eq!(frame_name(&state(None, true)), WELCOME);
    assert_eq!(frame_name(&state(Some("/p"), false)), PROJECT);
}

#[test]
fn the_file_is_under_the_workspace_directory() {
    let dir = dir(&PathBuf::from("/data"));
    assert_eq!(dir, PathBuf::from("/data/workspace"));
}

#[test]
fn a_written_state_reads_back() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("workspace");
    assert!(matches!(read(&dir), Read::Missing));
    let state = state(Some("/p"), true);
    write(&dir, &serde_json::to_vec(&state).unwrap()).unwrap();
    match read(&dir) {
        Read::Loaded(read) => assert_eq!(read, state),
        other => panic!("{other:?}"),
    }
    // The directory holds the file and no temporary one; nothing else was
    // created under the root.
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["window.json"]);
    let top: Vec<_> = std::fs::read_dir(root.path()).unwrap().collect();
    assert_eq!(top.len(), 1);
}

#[test]
fn a_second_write_replaces_the_first() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("workspace");
    write(
        &dir,
        &serde_json::to_vec(&state(Some("/a"), false)).unwrap(),
    )
    .unwrap();
    write(&dir, &serde_json::to_vec(&state(None, true)).unwrap()).unwrap();
    match read(&dir) {
        Read::Loaded(read) => assert_eq!(read, state(None, true)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_file_that_is_not_a_state_is_unreadable() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("workspace");
    write(&dir, b"{\"version\":").unwrap();
    assert!(matches!(read(&dir), Read::Unreadable(Unreadable::Json)));
}

/// The window's root before `install`.
struct Placeholder;

impl Render for Placeholder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn window(cx: &mut TestAppContext) -> AnyWindowHandle {
    cx.update(|cx| {
        lectern_ui::init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false);
    });
    cx.add_window(|_, _| Placeholder).into()
}

/// A debounced write needs a moment of stillness, the executor and the
/// foreground task that waits for it.
fn settle(executor: &Executor, cx: &mut TestAppContext) {
    cx.run_until_parked();
    executor.run_until_idle();
    cx.run_until_parked();
}

/// Watches a window's bounds from outside its root view.
struct Watcher;

/// AppKit resizes the window inside `apply_name`, where GPUI's resize
/// callback cannot reach the window (#315), so `apply_name` reads the
/// bounds again itself. The test platform has no frame autosave name; the
/// read is seen as the bounds observers running.
#[gpui::test]
fn applying_a_name_reads_the_bounds_again(cx: &mut TestAppContext) {
    let window = cx.add_window(|_, _| Placeholder);
    for name in [WELCOME, PROJECT] {
        let fired = Rc::new(Cell::new(0));
        window
            .update(cx, |_, window, cx| {
                let watcher = cx.new(|_| Watcher);
                let seen = fired.clone();
                let _subscription = watcher.update(cx, |_, cx| {
                    cx.observe_window_bounds(window, move |_, _, _| seen.set(seen.get() + 1))
                });
                apply_name(window, cx, name);
            })
            .unwrap();
        assert_eq!(fired.get(), 1, "{name}");
    }
}

#[gpui::test]
fn changes_are_written_once_after_500_ms_of_stillness(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("workspace");
    let window = window(cx);
    let executor = Executor::manual();
    let link = cx.update(|cx| WindowStateLink::start(window, dir.clone(), executor.clone(), cx));

    link.state_changed(state(Some("/a"), false));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    // A newer state restarts the wait.
    link.state_changed(state(Some("/b"), true));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    settle(&executor, cx);
    assert!(matches!(read(&dir), Read::Missing), "written too early");

    cx.executor().advance_clock(Duration::from_millis(200));
    settle(&executor, cx);
    match read(&dir) {
        Read::Loaded(read) => assert_eq!(read, state(Some("/b"), true)),
        other => panic!("{other:?}"),
    }
}

#[gpui::test]
fn the_name_for_a_new_window_follows_the_last_state(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    let window = window(cx);
    let executor = Executor::manual();
    let link = cx.update(|cx| WindowStateLink::start(window, root.path().join("w"), executor, cx));
    assert_eq!(link.frame_name(), WELCOME);
    link.state_changed(state(Some("/p"), false));
    assert_eq!(link.frame_name(), PROJECT);
    link.state_changed(state(None, false));
    assert_eq!(link.frame_name(), WELCOME);
    // A reopened window takes over the task's window.
    let next: AnyWindowHandle = cx.add_window(|_, _| Placeholder).into();
    link.window_changed(next, WELCOME);
    link.state_changed(state(Some("/p"), false));
    cx.run_until_parked();
    assert_eq!(link.frame_name(), PROJECT);
}

#[gpui::test]
fn a_failed_write_is_logged_without_the_path(cx: &mut TestAppContext) {
    let canary = "canary-2b6d41e8";
    let capture = lectern_logging::testing::capture();
    capture.clear();
    let root = tempfile::tempdir().unwrap();
    // A file where the directory should be: creating it fails.
    let dir = root.path().join(canary);
    std::fs::write(&dir, b"not a directory").unwrap();
    let window = window(cx);
    let executor = Executor::manual();
    let link =
        cx.update(|cx| WindowStateLink::start(window, dir.join("workspace"), executor.clone(), cx));

    link.state_changed(state(Some(&format!("/{canary}")), false));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(600));
    settle(&executor, cx);

    let lines = capture.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"msg\":\"window state not written\"")
                && l.contains("\"step\":\"create_dir\"")),
        "{lines:#?}"
    );
    // The project path is in the state, and so is the file's path in the
    // error: neither is logged.
    capture.assert_absent(canary);
    capture.assert_absent("window.json");
}

fn host(window: AnyWindowHandle, cx: &mut TestAppContext) -> WorkspaceHost {
    cx.update(|cx| {
        install(
            window,
            WorkspaceDeps {
                on_state_changed: Arc::new(|_| {}),
                on_milestone: Arc::new(|_, _| {}),
                on_frames: Arc::new(|_| {}),
            },
            cx,
        )
    })
}

/// What `restore` logs for a file: only the reason, never the content.
#[gpui::test]
fn restore_ignores_what_it_cannot_use(cx: &mut TestAppContext) {
    let canary = "canary-9c0a57d3";
    let capture = lectern_logging::testing::capture();
    let window = window(cx);
    let host = host(window, cx);
    let executor = Executor::manual();
    let root = tempfile::tempdir().unwrap();

    for (bytes, reason) in [
        (
            serde_json::to_vec(&WindowState {
                version: 2,
                project: Some(PathBuf::from(format!("/{canary}"))),
                inspector: true,
            })
            .unwrap(),
            Some("version"),
        ),
        (
            format!("{{\"project\":\"/{canary}\"").into_bytes(),
            Some("json"),
        ),
        (
            serde_json::to_vec(&state(Some(&format!("/{canary}")), true)).unwrap(),
            None,
        ),
    ] {
        let dir = root.path().join(reason.unwrap_or("fine"));
        write(&dir, &bytes).unwrap();
        capture.clear();
        restore(dir, &executor, host.clone());
        executor.run_until_idle();
        cx.run_until_parked();
        let lines = capture.lines();
        let warned: Vec<_> = lines
            .iter()
            .filter(|l| l.contains("window state"))
            .collect();
        match reason {
            Some(reason) => {
                assert_eq!(warned.len(), 1, "{lines:#?}");
                assert!(
                    warned[0].contains(&format!("\"reason\":\"{reason}\"")),
                    "{lines:#?}"
                );
            }
            None => assert!(warned.is_empty(), "{lines:#?}"),
        }
        capture.assert_absent(canary);
    }

    // No file is the first run: nothing is logged.
    capture.clear();
    restore(root.path().join("none"), &executor, host);
    executor.run_until_idle();
    assert!(
        capture.lines().iter().all(|l| !l.contains("window state")),
        "{:#?}",
        capture.lines()
    );
}
