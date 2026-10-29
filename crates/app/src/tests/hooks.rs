//! Where `run_with`'s window callback is called (docs/design/app.md
//! 「自动化构建的装配」), with the test profile and a counting callback.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyWindowHandle, Context, IntoElement, Render, TestAppContext, Window, div};
use lectern_executor::Executor;
use lectern_i18n::{Locale, Strings};
use lectern_logging::EarlyConfig;
use lectern_ui::Appearance;
use lectern_workspace::{WorkspaceDeps, install};

use crate::hooks::Hooks;
use crate::profile::Profile;
use crate::startup::{self, Shell, Started, Startup};
use crate::window_state::WindowStateLink;
use crate::{quit, window};

/// The window's root before `install`.
struct Placeholder;

impl Render for Placeholder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The windows the callback was given, in order.
type Calls = Rc<RefCell<Vec<AnyWindowHandle>>>;

/// The test profile's startup with a counting callback, and a window that no
/// barrier has been released in.
fn set_up(dir: &tempfile::TempDir, cx: &mut TestAppContext) -> (Calls, AnyWindowHandle) {
    let early = lectern_logging::install_early(EarlyConfig {
        dir: Some(dir.path().to_owned()),
        cli_filter: None,
    })
    .expect("one install per test process");
    let calls = Calls::default();
    let seen = calls.clone();
    cx.update(|cx| {
        cx.set_global(Startup::new(early, Profile::Test));
        cx.set_global(Hooks {
            on_window: Some(Rc::new(move |window, _| seen.borrow_mut().push(window))),
        });
        lectern_ui::init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false);
    });
    let window = cx.add_window(|_, _| Placeholder).into();
    (calls, window)
}

#[gpui::test]
fn the_barrier_calls_it_once_with_its_window(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (calls, window) = set_up(&dir, cx);
    cx.update(|cx| {
        startup::release_in(Some(window), cx);
        assert!(cx.has_global::<Started>());
        // Only the first release does anything.
        startup::release_in(Some(window), cx);
    });
    assert_eq!(*calls.borrow(), [window]);
}

#[gpui::test]
fn the_test_profile_barrier_does_not_call_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (calls, _) = set_up(&dir, cx);
    cx.update(|cx| {
        // The test profile releases with no window once the window exists.
        window::open(cx);
        assert!(cx.has_global::<Started>());
    });
    assert!(calls.borrow().is_empty());
}

#[gpui::test]
fn a_barrier_after_quit_does_not_call_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (calls, window) = set_up(&dir, cx);
    cx.update(|cx| {
        quit::quit(cx);
        startup::release_in(Some(window), cx);
        assert!(!cx.has_global::<Started>());
    });
    assert!(calls.borrow().is_empty());
}

#[gpui::test]
fn a_reopen_before_the_barrier_does_not_call_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (calls, _) = set_up(&dir, cx);
    // No shell yet: in the desktop profile the new window's own barrier calls
    // it, which `the_barrier_calls_it_once_with_its_window` covers.
    cx.update(window::reopen);
    assert!(calls.borrow().is_empty());
}

#[gpui::test]
fn a_reopen_with_the_workspace_calls_it_for_the_new_window(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (calls, window) = set_up(&dir, cx);
    cx.update(|cx| startup::release_in(Some(window), cx));
    // The test profile assembles no workspace: give `Started` the shell the
    // desktop profile would have.
    cx.update(|cx| {
        let link =
            WindowStateLink::start(window, dir.path().join("workspace"), Executor::manual(), cx);
        let host = install(
            window,
            WorkspaceDeps {
                on_state_changed: Arc::new(|_| {}),
                on_milestone: Arc::new(|_, _| {}),
                on_frames: Arc::new(|_| {}),
            },
            cx,
        );
        cx.global_mut::<Started>().shell = Some(Shell {
            host,
            window_state: link,
        });
    });
    cx.update(|cx| {
        window
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
    });
    cx.update(window::reopen);
    let calls = calls.borrow();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0], window);
    assert_ne!(calls[1], window);
}
