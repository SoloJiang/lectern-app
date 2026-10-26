//! The error of the measurement build's method replacements, used by
//! [`crate::frame`]: 「测量构建的方法替换」 in docs/design/platform.md,
//! ADR 0048 (there is no `metal` module, ADR 0049). It compiles on every
//! platform and with or without the `bench-hooks` feature, so that callers
//! need no `cfg`.

use std::fmt;
use std::io;

#[cfg(test)]
mod tests;

/// Why a method replacement was not installed or a hook did not start.
#[derive(Debug)]
pub enum HookError {
    /// Without the `bench-hooks` feature, or not on macOS.
    Unsupported,
    /// Already registered (`frame::on_first_presented` can be called once),
    /// or a `FrameRecorder` is already recording.
    AlreadyActive,
    /// Must be called on the main thread and was not. Nothing was done.
    NotMainThread,
    /// The handle gives no AppKit view (GPUI's test platform answers
    /// `HandleError::NotSupported`), or the view has no window, screen or
    /// `CAMetalLayer`.
    NoMetalLayer,
    /// The method to replace is missing, or its type encoding is not the
    /// expected one: the locked GPUI or the system framework changed. `what`
    /// is "class selector".
    Mismatch { what: String },
    /// A system call failed, such as creating a CVDisplayLink.
    Os(io::Error),
}

impl HookError {
    /// The kind as the name used in reports: `unsupported`, `already_active`,
    /// `not_main_thread`, `no_metal_layer`, `mismatch`, `os`.
    pub fn as_str(&self) -> &'static str {
        match self {
            HookError::Unsupported => "unsupported",
            HookError::AlreadyActive => "already_active",
            HookError::NotMainThread => "not_main_thread",
            HookError::NoMetalLayer => "no_metal_layer",
            HookError::Mismatch { .. } => "mismatch",
            HookError::Os(_) => "os",
        }
    }
}

impl fmt::Display for HookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HookError::Unsupported => {
                f.write_str("method replacement needs `bench-hooks` on macOS")
            }
            HookError::AlreadyActive => f.write_str("the hook is already active"),
            HookError::NotMainThread => f.write_str("not on the main thread"),
            HookError::NoMetalLayer => f.write_str("the handle gives no CAMetalLayer"),
            HookError::Mismatch { what } => {
                write!(
                    f,
                    "cannot replace {what}: it is missing or has another type encoding"
                )
            }
            HookError::Os(error) => write!(f, "system call failed: {error}"),
        }
    }
}

impl std::error::Error for HookError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HookError::Os(error) => Some(error),
            _ => None,
        }
    }
}

/// The check before a replacement: the method's type encoding (None when
/// the runtime gave none) must be the expected one. `what` is "class
/// selector", for the error. Pure; compiled for tests everywhere.
#[cfg(any(all(target_os = "macos", feature = "bench-hooks"), test))]
pub(crate) fn check_type_encoding(
    what: &str,
    actual: Option<&std::ffi::CStr>,
    expected: &std::ffi::CStr,
) -> Result<(), HookError> {
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(HookError::Mismatch {
            what: what.to_owned(),
        })
    }
}
