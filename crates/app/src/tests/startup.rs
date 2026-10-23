//! The barrier and quit with the test profile (`#[gpui::test]`). The quit
//! order itself runs in `tests/quit.rs` with fakes and in `tests/startup.rs`
//! with the real binary.

use std::sync::Arc;

use gpui::TestAppContext;
use lectern_logging::EarlyConfig;

use crate::profile::Profile;
use crate::startup::{self, Started, Startup};
use crate::{quit, window};

fn startup(dir: &tempfile::TempDir) -> Startup {
    let early = lectern_logging::install_early(EarlyConfig {
        dir: Some(dir.path().to_owned()),
        cli_filter: None,
    })
    .expect("one install per test process");
    Startup::new(early, Profile::Test)
}

#[gpui::test]
fn test_profile_releases_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let startup = startup(&dir);
    cx.update(|cx| {
        cx.set_global(startup);
        window::open(cx);
        let kernel = Arc::as_ptr(&cx.global::<Started>().kernel);
        startup::release(cx);
        assert_eq!(Arc::as_ptr(&cx.global::<Started>().kernel), kernel);
    });
}

#[gpui::test]
fn quit_before_release_starts_nothing(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let startup = startup(&dir);
    cx.update(|cx| {
        cx.set_global(startup);
        quit::quit(cx);
        assert!(quit::is_quitting(cx));
        // A barrier that fires after ⌘Q starts nothing.
        startup::release(cx);
        assert!(!cx.has_global::<Started>());
        // A second ⌘Q is ignored.
        quit::quit(cx);
    });
}
