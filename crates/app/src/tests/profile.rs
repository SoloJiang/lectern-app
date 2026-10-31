use std::sync::Arc;

use gpui::{AnyWindowHandle, Context, IntoElement, Render, TestAppContext, Window, div};
use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::Watcher;
use lectern_fs_watch::testing::FakeBackend;
use lectern_i18n::{Locale, Strings};
use lectern_ui::Appearance;
use lectern_workspace::{WorkspaceDeps, install};

use crate::profile::{Profile, Resources};

/// The window's root before `install`.
struct Placeholder;

impl Render for Placeholder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The per-profile graph test kernel.md asks `app` for: a wrong graph
/// fails CI rather than a release. Building the plugins does no IO: the
/// directories they are given stay empty. The "workspace" plugin takes the
/// handle `install` returns, so the test installs it in a test window.
#[gpui::test]
fn every_profile_graph_validates(cx: &mut TestAppContext) {
    cx.update(|cx| {
        lectern_ui::init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false);
    });
    let window: AnyWindowHandle = cx.add_window(|_, _| Placeholder).into();
    let workspace = cx.update(|cx| {
        install(
            window,
            WorkspaceDeps {
                on_state_changed: Arc::new(|_| {}),
                on_milestone: Arc::new(|_, _| {}),
                on_frames: Arc::new(|_| {}),
            },
            cx,
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let executor = Executor::new(lectern_executor::Config::default());
    let resources = Resources {
        executor: executor.clone(),
        cache: Cache::new(lectern_cache::Config {
            root: dir.path().join("cache"),
            budgets: lectern_cache::Budgets::default(),
            ui_thread: None,
        }),
        watcher: Watcher::new(
            FakeBackend::new(),
            executor.clone(),
            lectern_fs_watch::Config::default(),
        ),
        workspace,
        home: dir.path().join("home"),
        app_cache: dir.path().join("app_cache"),
        app_data: dir.path().join("app_data"),
        git_ceilings: Arc::from([]),
    };
    for &profile in Profile::ALL {
        let (set, count) = profile.plugins(&resources);
        if let Err(errors) = set.validate() {
            panic!("{profile:?}: {errors:?}");
        }
        let expected = match profile {
            // project, two session sources, review, usage, workspace,
            // welcome, welcome-discovery, welcome-review, project-home,
            // project-home-usage, review-view, worktrees, worktrees-usage, terminal.
            Profile::Desktop => 15,
            // The probe is not a plugin: the same set as desktop.
            #[cfg(feature = "ui-probe")]
            Profile::Probe => 15,
            Profile::Test => 0,
        };
        assert_eq!(count, expected, "{profile:?}");
    }
    let written: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(written.is_empty(), "{written:?}");
}
