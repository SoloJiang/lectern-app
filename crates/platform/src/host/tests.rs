//! Tests of the machine description. The byte check runs on every platform;
//! the system reads run on macOS only.

use super::{MAX_VALUE_LEN, host_value};

#[test]
fn host_value_accepts_plain_values() {
    for (raw, expected) in [
        (&b"Mac14,2"[..], "Mac14,2"),
        (b"Apple M2 Pro", "Apple M2 Pro"),
        (b"Apple M1 (Virtual)", "Apple M1 (Virtual)"),
        // With one NUL, several, or none at the end.
        (b"Mac14,2\0", "Mac14,2"),
        (b"Mac14,2\0\0\0", "Mac14,2"),
        // The ends of the printable range.
        (b" ~", " ~"),
    ] {
        assert_eq!(host_value(raw).as_deref(), Some(expected), "{raw:?}");
    }
    let longest = "a".repeat(MAX_VALUE_LEN);
    assert_eq!(host_value(longest.as_bytes()), Some(longest.clone()));
    let mut with_nul = longest.clone().into_bytes();
    with_nul.push(0);
    assert_eq!(host_value(&with_nul), Some(longest));
}

#[test]
fn host_value_rejects_anything_else() {
    for raw in [
        &b""[..],
        b"\0",
        b"\0\0",
        b"Mac14,2\n",
        b"Mac14,2\n\0",
        b"Mac\t14",
        b"Mac14,\x7f",
        b"Apple M2 \xe2\x84\xa2",
        b"\xff",
        b"Mac\x0014,2",
        b"\0Mac14,2",
    ] {
        assert_eq!(host_value(raw), None, "{raw:?}");
    }
    let overlong = "a".repeat(MAX_VALUE_LEN + 1);
    assert_eq!(host_value(overlong.as_bytes()), None);
}

#[cfg(target_os = "macos")]
#[test]
fn host_describe_reads_this_mac() {
    let info = super::describe().unwrap();
    assert!(!info.model.is_empty());
    assert!(!info.chip.is_empty());
    assert!(!info.os_version.is_empty());
    assert!(info.cpus >= 1);
    assert_eq!(
        crate::system::os_version().as_deref(),
        Some(format!("macOS {}", info.os_version).as_str())
    );
    // Every field obeys the rules of `HostInfo`.
    for value in [&info.model, &info.chip, &info.os_version] {
        assert_eq!(host_value(value.as_bytes()).as_ref(), Some(value));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn host_describe_is_stable() {
    assert_eq!(super::describe().unwrap(), super::describe().unwrap());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn host_is_unsupported_elsewhere() {
    let err = super::describe().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
}
