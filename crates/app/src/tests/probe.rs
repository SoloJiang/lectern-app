//! The probe profile's start (docs/design/ui_probe.md「对其他 crate 的要求」),
//! with GPUI's test platform. The real process is in `tests/probe.rs`.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use gpui::TestAppContext;

use crate::probe;

/// A directory that is not there is not private: the start fails, the
/// record says why and does not hold the path.
#[gpui::test]
fn a_directory_that_is_not_private_is_logged_without_its_path(cx: &mut TestAppContext) {
    let canary = "canary-5d2c8e41";
    let capture = lectern_logging::testing::capture();
    capture.clear();
    let dir = PathBuf::from(format!("/nonexistent/{canary}"));
    let probe = cx.update(|cx| probe::start(Some(dir), cx));
    assert!(probe.is_none());
    let lines = capture.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"msg\":\"probe start failed\"")
                && l.contains("\"reason\":\"dir-not-private\"")),
        "{lines:#?}"
    );
    capture.assert_absent(canary);
}

/// No directory: not the probe profile, nothing starts.
#[gpui::test]
fn no_directory_starts_nothing(cx: &mut TestAppContext) {
    assert!(cx.update(|cx| probe::start(None, cx)).is_none());
}

/// A private directory: the session listens, and `shutdown` takes the
/// socket away at once.
#[gpui::test]
fn a_private_directory_starts_a_session_that_shutdown_ends(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let probe = cx
        .update(|cx| probe::start(Some(dir.path().to_owned()), cx))
        .expect("a 0700 directory of ours");
    let socket = dir.path().join(lectern_ui_probe::server::SOCKET);
    assert!(socket.exists());
    probe.shutdown();
    assert!(!socket.exists());
}
