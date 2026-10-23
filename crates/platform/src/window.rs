//! Window capabilities: the frame autosave name of AppKit
//! (「窗口 frame 的 autosave」 in docs/design/platform.md, ADR 0017, item D of
//! docs/design/workspace.md「对其他 crate 的要求」). `NSWindow` reads the
//! saved frame from the in-memory user defaults and saves it after every
//! move or resize; no file of Lectern's is read or written.

use raw_window_handle::HasWindowHandle;

#[cfg(test)]
mod tests;

/// The longest name accepted, in bytes. The names `app` uses are `welcome`
/// and `project`.
const MAX_NAME_LEN: usize = 64;

/// Why a frame autosave name was not set. The window keeps working; only
/// its frame is not restored or saved.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WindowError {
    /// The name is empty, longer than 64 bytes, or has a byte that is not
    /// an ASCII letter, digit, `-` or `_`.
    InvalidName,
    /// The handle gives no window: `window_handle` failed (GPUI's test
    /// platform), the handle is not an AppKit one, or the view is not in an
    /// `NSWindow`.
    NoWindow,
    /// Called off the main thread. Nothing was done.
    NotMainThread,
    /// AppKit refused the name: another window of this process already
    /// uses it.
    NameInUse,
}

impl WindowError {
    /// The kind as the log field `kind` (「日志与诊断」).
    pub fn as_str(self) -> &'static str {
        match self {
            WindowError::InvalidName => "invalid_name",
            WindowError::NoWindow => "no_window",
            WindowError::NotMainThread => "not_main_thread",
            WindowError::NameInUse => "name_in_use",
        }
    }
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            WindowError::InvalidName => "the frame autosave name is not valid",
            WindowError::NoWindow => "the handle gives no window",
            WindowError::NotMainThread => "not on the main thread",
            WindowError::NameInUse => "another window uses the frame autosave name",
        })
    }
}

impl std::error::Error for WindowError {}

/// Gives the window AppKit's frame autosave name
/// (`NSWindow.setFrameAutosaveName:`): AppKit restores the frame saved under
/// this name from the application's preferences at once (the user defaults
/// in memory, no file of Lectern's), then saves the frame after every move
/// or resize. Main thread only. On every other platform the name is
/// checked and nothing else is done.
///
/// The checks run in this order, so that each error is reachable without
/// the one before it: the name (`InvalidName`), the handle (`NoWindow`),
/// the thread (`NotMainThread`), then AppKit (`NoWindow` for a view without
/// a window, `NameInUse`). On macOS every error is also logged at warn
/// level with the kind only (「日志与诊断」); the caller does not log it
/// again.
pub fn set_frame_autosave_name(
    window: &impl HasWindowHandle,
    name: &str,
) -> Result<(), WindowError> {
    if !valid_name(name) {
        return imp::fail(WindowError::InvalidName);
    }
    imp::set(window, name)
}

/// Non-empty, at most 64 bytes, ASCII letters, digits, `-` and `_` only.
/// The name is a user defaults key (`NSWindow Frame <name>`); the bound
/// keeps it a plain key on every platform.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_char, c_void};
    use std::mem;

    use lectern_logging::warn;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::WindowError;

    /// An Objective-C object pointer (`id`).
    type Id = *mut c_void;
    type Sel = *const c_void;
    type CFIndex = isize;
    type CFAllocatorRef = *const c_void;
    type CFStringRef = *const c_void;
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    // The Objective-C runtime, part of libSystem's load set; AppKit is
    // linked by GPUI.
    #[link(name = "objc")]
    unsafe extern "C" {
        fn sel_registerName(name: *const c_char) -> Sel;
        /// Declared without a signature and cast to the receiver's one per
        /// call, as the runtime documents.
        fn objc_msgSend();
    }

    // Already linked by AppKit (and by `system` and `fs_events`).
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: CFAllocatorRef,
            bytes: *const u8,
            len: CFIndex,
            encoding: u32,
            is_external: u8,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    /// Logs the failure with its kind only and returns it.
    pub(super) fn fail(error: WindowError) -> Result<(), WindowError> {
        warn!(
            "the window frame autosave name was not set; the frame is not restored or saved";
            kind = error.as_str(),
        );
        Err(error)
    }

    pub(super) fn set(window: &impl HasWindowHandle, name: &str) -> Result<(), WindowError> {
        let Ok(handle) = window.window_handle() else {
            return fail(WindowError::NoWindow);
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return fail(WindowError::NoWindow);
        };
        // SAFETY: no arguments; reads a flag of the calling thread.
        if unsafe { libc::pthread_main_np() } == 0 {
            return fail(WindowError::NotMainThread);
        }
        let view: Id = handle.ns_view.as_ptr();
        // SAFETY: `ns_view` is a live `NSView` for the handle's lifetime
        // (the raw-window-handle contract, kept by GPUI's `MacWindow`); the
        // main thread is the only one that uses AppKit views. `-window`
        // takes no argument and returns an object or nil, which is the
        // signature cast here.
        let ns_window: Id = unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(view, sel_registerName(c"window".as_ptr()))
        };
        if ns_window.is_null() {
            return fail(WindowError::NoWindow);
        }
        // SAFETY: `name` is valid UTF-8 (ASCII, checked by the caller) of
        // `len` bytes; a null allocator is the default one.
        let string = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                name.as_ptr(),
                name.len() as CFIndex,
                UTF8,
                0,
            )
        };
        if string.is_null() {
            return fail(WindowError::InvalidName);
        }
        // SAFETY: `ns_window` is the view's window, alive while the view is
        // (the Get rule); `string` is a CFString, toll-free bridged to the
        // `NSString` the selector takes; the method returns `BOOL`, one
        // byte on both Apple architectures. The window copies the string,
        // so it is released right after.
        let set = unsafe {
            let send: unsafe extern "C" fn(Id, Sel, CFStringRef) -> u8 =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let set = send(
                ns_window,
                sel_registerName(c"setFrameAutosaveName:".as_ptr()),
                string,
            );
            CFRelease(string);
            set
        };
        if set == 0 {
            return fail(WindowError::NameInUse);
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use raw_window_handle::HasWindowHandle;

    use super::WindowError;

    pub(super) fn fail(error: WindowError) -> Result<(), WindowError> {
        Err(error)
    }

    /// Nothing to do: the window is not touched.
    pub(super) fn set(_window: &impl HasWindowHandle, _name: &str) -> Result<(), WindowError> {
        Ok(())
    }
}
