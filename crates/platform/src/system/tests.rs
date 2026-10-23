//! Tests of the OS version and the preferred languages. The byte checks run
//! on every platform; the system reads run on macOS only.

use super::{language_tag, mac_os_version, os_version, preferred_languages};

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

#[test]
fn preferred_languages_keeps_language_tags() {
    for (raw, expected) in [
        (&b"en\0"[..], "en"),
        (b"en-US\0", "en-US"),
        (b"zh-Hans-CN\0", "zh-Hans-CN"),
        (b"es-419\0", "es-419"),
        // Without the NUL, and with bytes after it (the rest of the buffer).
        (b"ja-JP", "ja-JP"),
        (b"fr\0garbage", "fr"),
    ] {
        assert_eq!(language_tag(raw).as_deref(), Some(expected), "{raw:?}");
    }
}

#[test]
fn preferred_languages_rejects_anything_but_a_tag() {
    for raw in [
        &b""[..],
        b"\0",
        b"-en\0",
        b"en_US\0",
        b"en US\0",
        b"en\n\0",
        b"en-\xc3\xa9\0",
        b"\xff\0",
    ] {
        assert_eq!(language_tag(raw), None, "{raw:?}");
    }
}

#[test]
fn preferred_languages_tag_length_is_capped() {
    let longest = "a".repeat(super::MAX_TAG_LEN);
    assert_eq!(language_tag(longest.as_bytes()), Some(longest.clone()));
    let overlong = "a".repeat(super::MAX_TAG_LEN + 1);
    assert_eq!(language_tag(overlong.as_bytes()), None);
}

#[cfg(target_os = "macos")]
#[test]
fn preferred_languages_reads_the_running_macos() {
    let tags = preferred_languages();
    // A macOS user always has at least one preferred language.
    assert!(!tags.is_empty());
    assert!(tags.len() <= super::MAX_LANGUAGES, "{tags:?}");
    for tag in &tags {
        assert_eq!(language_tag(tag.as_bytes()).as_ref(), Some(tag));
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn preferred_languages_is_empty_elsewhere() {
    assert!(preferred_languages().is_empty());
}
