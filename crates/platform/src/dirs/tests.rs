//! Tests of the app data and log directories. They call the per-platform
//! functions with explicit values and never set process environment
//! variables.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use super::{app_data, logs, mac_app_data, mac_logs, xdg_app_data, xdg_logs};

fn os(s: &str) -> Option<&OsStr> {
    Some(OsStr::new(s))
}

#[test]
fn app_data_xdg_data_home_set_is_used() {
    assert_eq!(
        xdg_app_data(os("/home/dev"), os("/data/share")),
        Some(PathBuf::from("/data/share/lectern"))
    );
    // A trailing slash adds no empty component.
    assert_eq!(
        xdg_app_data(os("/home/dev"), os("/data/share/")),
        Some(PathBuf::from("/data/share/lectern"))
    );
    // HOME is not needed then.
    assert_eq!(
        xdg_app_data(None, os("/data/share")),
        Some(PathBuf::from("/data/share/lectern"))
    );
}

#[test]
fn app_data_xdg_data_home_unset_empty_or_relative_falls_back_to_home() {
    let fallback = Some(PathBuf::from("/home/dev/.local/share/lectern"));
    assert_eq!(xdg_app_data(os("/home/dev"), None), fallback);
    assert_eq!(xdg_app_data(os("/home/dev"), os("")), fallback);
    assert_eq!(xdg_app_data(os("/home/dev"), os("data/share")), fallback);
    assert_eq!(xdg_app_data(os("/home/dev"), os("./share")), fallback);
    assert_eq!(xdg_app_data(os("/home/dev"), os("~/share")), fallback);
}

#[test]
fn app_data_without_a_usable_home_is_none() {
    for home in [None, os(""), os("home/dev"), os("~")] {
        assert_eq!(xdg_app_data(home, None), None, "{home:?}");
        assert_eq!(xdg_app_data(home, os("")), None, "{home:?}");
        assert_eq!(xdg_app_data(home, os("data")), None, "{home:?}");
        assert_eq!(mac_app_data(home), None, "{home:?}");
    }
}

#[test]
fn app_data_macos_is_under_application_support() {
    let expected = Some(PathBuf::from(
        "/Users/dev/Library/Application Support/Lectern",
    ));
    assert_eq!(mac_app_data(os("/Users/dev")), expected);
    assert_eq!(mac_app_data(os("/Users/dev/")), expected);
}

#[test]
fn app_data_keeps_non_utf8_bytes() {
    let home = OsStr::from_bytes(b"/home/d\xffv");
    assert_eq!(
        xdg_app_data(Some(home), None)
            .unwrap()
            .as_os_str()
            .as_bytes(),
        b"/home/d\xffv/.local/share/lectern"
    );
    assert_eq!(
        mac_app_data(Some(home)).unwrap().as_os_str().as_bytes(),
        b"/home/d\xffv/Library/Application Support/Lectern"
    );
    let data = OsStr::from_bytes(b"/d\xfe");
    assert_eq!(
        xdg_app_data(None, Some(data))
            .unwrap()
            .as_os_str()
            .as_bytes(),
        b"/d\xfe/lectern"
    );
}

#[test]
fn app_data_does_not_create_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    let xdg = xdg_app_data(Some(home.as_os_str()), None).unwrap();
    let xdg_set = xdg_app_data(Some(home.as_os_str()), Some(data.as_os_str())).unwrap();
    let mac = mac_app_data(Some(home.as_os_str())).unwrap();
    assert_eq!(xdg, home.join(".local/share/lectern"));
    assert_eq!(xdg_set, data.join("lectern"));
    assert_eq!(mac, home.join("Library/Application Support/Lectern"));
    assert!(!home.exists());
    assert!(!data.exists());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

/// `app_data` reads the process environment as it is; this test only reads
/// it too, so it runs safely next to the others.
#[test]
fn app_data_reads_the_process_environment() {
    let home = std::env::var_os("HOME");
    #[cfg(target_os = "macos")]
    let expected = mac_app_data(home.as_deref());
    #[cfg(not(target_os = "macos"))]
    let expected = xdg_app_data(
        home.as_deref(),
        std::env::var_os("XDG_DATA_HOME").as_deref(),
    );
    assert_eq!(app_data(), expected);
}

#[test]
fn logs_xdg_state_home_set_is_used() {
    assert_eq!(
        xdg_logs(os("/home/dev"), os("/data/state")),
        Some(PathBuf::from("/data/state/lectern/logs"))
    );
    // A trailing slash adds no empty component.
    assert_eq!(
        xdg_logs(os("/home/dev"), os("/data/state/")),
        Some(PathBuf::from("/data/state/lectern/logs"))
    );
    // HOME is not needed then.
    assert_eq!(
        xdg_logs(None, os("/data/state")),
        Some(PathBuf::from("/data/state/lectern/logs"))
    );
}

#[test]
fn logs_xdg_state_home_unset_empty_or_relative_falls_back_to_home() {
    let fallback = Some(PathBuf::from("/home/dev/.local/state/lectern/logs"));
    assert_eq!(xdg_logs(os("/home/dev"), None), fallback);
    assert_eq!(xdg_logs(os("/home/dev"), os("")), fallback);
    assert_eq!(xdg_logs(os("/home/dev"), os("data/state")), fallback);
    assert_eq!(xdg_logs(os("/home/dev"), os("./state")), fallback);
    assert_eq!(xdg_logs(os("/home/dev"), os("~/state")), fallback);
}

#[test]
fn logs_without_a_usable_home_is_none() {
    for home in [None, os(""), os("home/dev"), os("~")] {
        assert_eq!(xdg_logs(home, None), None, "{home:?}");
        assert_eq!(xdg_logs(home, os("")), None, "{home:?}");
        assert_eq!(xdg_logs(home, os("state")), None, "{home:?}");
        assert_eq!(mac_logs(home), None, "{home:?}");
    }
}

#[test]
fn logs_macos_is_under_library_logs() {
    let expected = Some(PathBuf::from("/Users/dev/Library/Logs/Lectern"));
    assert_eq!(mac_logs(os("/Users/dev")), expected);
    assert_eq!(mac_logs(os("/Users/dev/")), expected);
}

#[test]
fn logs_keeps_non_utf8_bytes() {
    let home = OsStr::from_bytes(b"/home/d\xffv");
    assert_eq!(
        xdg_logs(Some(home), None).unwrap().as_os_str().as_bytes(),
        b"/home/d\xffv/.local/state/lectern/logs"
    );
    assert_eq!(
        mac_logs(Some(home)).unwrap().as_os_str().as_bytes(),
        b"/home/d\xffv/Library/Logs/Lectern"
    );
    let state = OsStr::from_bytes(b"/s\xfe");
    assert_eq!(
        xdg_logs(None, Some(state)).unwrap().as_os_str().as_bytes(),
        b"/s\xfe/lectern/logs"
    );
}

#[test]
fn logs_does_not_create_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    let xdg = xdg_logs(Some(home.as_os_str()), None).unwrap();
    let xdg_set = xdg_logs(Some(home.as_os_str()), Some(state.as_os_str())).unwrap();
    let mac = mac_logs(Some(home.as_os_str())).unwrap();
    assert_eq!(xdg, home.join(".local/state/lectern/logs"));
    assert_eq!(xdg_set, state.join("lectern/logs"));
    assert_eq!(mac, home.join("Library/Logs/Lectern"));
    assert!(!home.exists());
    assert!(!state.exists());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

/// `logs` reads the process environment as it is; this test only reads it
/// too, so it runs safely next to the others.
#[test]
fn logs_reads_the_process_environment() {
    let home = std::env::var_os("HOME");
    #[cfg(target_os = "macos")]
    let expected = mac_logs(home.as_deref());
    #[cfg(not(target_os = "macos"))]
    let expected = xdg_logs(
        home.as_deref(),
        std::env::var_os("XDG_STATE_HOME").as_deref(),
    );
    assert_eq!(logs(), expected);
}
