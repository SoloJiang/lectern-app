//! Tests of the OS version. The byte checks run on every platform; the
//! `sysctl` read runs on macOS only.

use super::{mac_os_version, os_version};

#[test]
fn os_version_formats_the_product_version() {
    for (raw, expected) in [
        (&b"15.1\0"[..], "macOS 15.1"),
        (b"26.0.1\0", "macOS 26.0.1"),
        (b"14\0", "macOS 14"),
        // Without the trailing NUL, or with several.
        (b"13.7.2", "macOS 13.7.2"),
        (b"15.1\0\0\0", "macOS 15.1"),
    ] {
        assert_eq!(mac_os_version(raw).as_deref(), Some(expected), "{raw:?}");
    }
}

#[test]
fn os_version_rejects_anything_but_a_version() {
    for raw in [
        &b""[..],
        b"\0",
        b".1\0",
        b"v15.1\0",
        b"15.1 beta\0",
        b"15\x001\0",
        b"15.1\n\0",
        b"15.\xff\0",
        b"\xd9\xa1\xd9\xa5\0", // Arabic-Indic digits: not ASCII.
    ] {
        assert_eq!(mac_os_version(raw), None, "{raw:?}");
    }
}

#[test]
fn os_version_length_is_capped() {
    let longest = "1".repeat(super::MAX_VERSION_LEN);
    assert_eq!(
        mac_os_version(longest.as_bytes()),
        Some(format!("macOS {longest}"))
    );
    let overlong = "1".repeat(super::MAX_VERSION_LEN + 1);
    assert_eq!(mac_os_version(overlong.as_bytes()), None);
}

#[cfg(target_os = "macos")]
#[test]
fn os_version_reads_the_running_macos() {
    let version = os_version().expect("kern.osproductversion");
    let number = version.strip_prefix("macOS ").expect(&version);
    let major: u32 = number.split('.').next().unwrap().parse().unwrap();
    // `kern.osproductversion` exists since 10.13.4; the safe reads need
    // `O_NOFOLLOW_ANY`, macOS 11 or later.
    assert!(major >= 11, "{version}");
    eprintln!("{version}");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn os_version_is_none_elsewhere() {
    assert_eq!(os_version(), None);
}
