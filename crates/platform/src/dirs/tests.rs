//! Tests of the app data directory. They call the per-platform functions
//! with explicit values and never set process environment variables.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use super::{app_data, mac_app_data, xdg_app_data};

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
