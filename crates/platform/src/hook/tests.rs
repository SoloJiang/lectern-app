//! Tests of the hook error and of the check before a replacement. Pure, so
//! they run on every platform.

use std::ffi::CStr;
use std::io;

use super::{HookError, check_type_encoding};

#[test]
fn hook_error_kinds_are_distinct_static_names() {
    let errors = [
        HookError::Unsupported,
        HookError::AlreadyActive,
        HookError::NotMainThread,
        HookError::NoMetalLayer,
        HookError::Mismatch {
            what: "CAMetalLayer nextDrawable".to_owned(),
        },
        HookError::Os(io::Error::from_raw_os_error(1)),
    ];
    let names: Vec<&'static str> = errors.iter().map(HookError::as_str).collect();
    assert_eq!(
        names,
        [
            "unsupported",
            "already_active",
            "not_main_thread",
            "no_metal_layer",
            "mismatch",
            "os"
        ]
    );
    for error in &errors {
        assert!(!error.to_string().is_empty(), "{error:?}");
    }
    // `Mismatch` says what it could not replace; `Os` has its cause.
    assert!(errors[4].to_string().contains("CAMetalLayer nextDrawable"));
    assert!(std::error::Error::source(&errors[5]).is_some());
    assert!(std::error::Error::source(&errors[0]).is_none());
}

#[test]
fn hook_type_encoding_check() {
    let expected: &CStr = c"@16@0:8";
    assert!(check_type_encoding("CAMetalLayer nextDrawable", Some(c"@16@0:8"), expected).is_ok());
    for actual in [
        None,
        Some(c""),
        Some(c"v16@0:8"),
        Some(c"@16@0:8@16"),
        Some(c"@16@0:"),
    ] {
        let error = check_type_encoding("CAMetalLayer nextDrawable", actual, expected).unwrap_err();
        match error {
            HookError::Mismatch { what } => assert_eq!(what, "CAMetalLayer nextDrawable"),
            other => panic!("{actual:?}: {other:?}"),
        }
    }
}
