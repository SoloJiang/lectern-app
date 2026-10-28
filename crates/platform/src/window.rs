//! Window capabilities: the frame autosave name of AppKit
//! (「窗口 frame 的 autosave」 in docs/design/platform.md, ADR 0017, item D of
//! docs/design/workspace.md「对其他 crate 的要求」). `NSWindow` reads the
//! saved frame from the in-memory user defaults and saves it after every
//! move or resize; no file of Lectern's is read or written. A name used for
//! the first time gets a default size, centred on the screen
//! (`set_frame_autosave_name_or_center`).

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

/// Which frame `set_frame_autosave_name_or_center` left the window with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameSource {
    /// A frame was saved under the name, and it was applied.
    Restored,
    /// Nothing was saved: the window got the default content size, centred
    /// on its screen.
    Centered,
}

/// `set_frame_autosave_name`, and for a name with no saved frame yet the
/// window first gets the content size `default_size` (points, width and
/// height; a side larger than the screen's visible area is cut to it) and
/// is centred. AppKit's `setFrameUsingName:` tells whether a frame was
/// saved; the name is set last, so the frame saved under it is the centred
/// one.
///
/// The checks, errors and log records are those of
/// `set_frame_autosave_name`. A failed check leaves the frame as it was;
/// on `NameInUse` the window may already be centred. Main thread only. On
/// every other platform the name is checked and `Restored` returned; the
/// window is not touched.
pub fn set_frame_autosave_name_or_center(
    window: &impl HasWindowHandle,
    name: &str,
    default_size: (f64, f64),
) -> Result<FrameSource, WindowError> {
    if !valid_name(name) {
        return imp::fail(WindowError::InvalidName);
    }
    imp::set_or_center(window, name, default_size)
}

/// `size` with each side cut to the visible area's, when there is one.
fn clamp_to_visible(size: (f64, f64), visible: Option<(f64, f64)>) -> (f64, f64) {
    match visible {
        Some((width, height)) => (size.0.min(width), size.1.min(height)),
        None => size,
    }
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

    use super::{FrameSource, WindowError};

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
        fn objc_getClass(name: *const c_char) -> Id;
    }

    // x86_64 returns a struct larger than 16 bytes, such as `NSRect`,
    // through memory; arm64 has no such entry point.
    #[cfg(target_arch = "x86_64")]
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_msgSend_stret();
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NSSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NSRect {
        origin: NSSize,
        size: NSSize,
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
    pub(super) fn fail<T>(error: WindowError) -> Result<T, WindowError> {
        warn!(
            "the window frame autosave name was not set; the frame is not restored or saved";
            kind = error.as_str(),
        );
        Err(error)
    }

    pub(super) fn set(window: &impl HasWindowHandle, name: &str) -> Result<(), WindowError> {
        let ns_window = window_of(window)?;
        let string = cf_name(name)?;
        // SAFETY: see `set_name`.
        let set = unsafe { set_name(ns_window, string) };
        // SAFETY: `string` was created above and is released once.
        unsafe { CFRelease(string) };
        if !set {
            return fail(WindowError::NameInUse);
        }
        Ok(())
    }

    pub(super) fn set_or_center(
        window: &impl HasWindowHandle,
        name: &str,
        default_size: (f64, f64),
    ) -> Result<FrameSource, WindowError> {
        let ns_window = window_of(window)?;
        let string = cf_name(name)?;
        // SAFETY: as in `set_name`: `ns_window` is a live window on the
        // main thread, `string` a CFString bridged to the `NSString` the
        // selector takes, and the method returns `BOOL`.
        let restored = unsafe {
            let send: unsafe extern "C" fn(Id, Sel, CFStringRef) -> u8 =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(
                ns_window,
                sel_registerName(c"setFrameUsingName:".as_ptr()),
                string,
            ) != 0
        };
        if !restored {
            // SAFETY: `ns_window` as above.
            let visible = unsafe { visible_size(ns_window) };
            let (width, height) = super::clamp_to_visible(default_size, visible);
            // SAFETY: `ns_window` as above. `-setContentSize:` takes an
            // `NSSize` (two `f64`, passed in registers on both Apple
            // architectures) and `-center` takes nothing; both return void.
            unsafe {
                let resize: unsafe extern "C" fn(Id, Sel, NSSize) =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                resize(
                    ns_window,
                    sel_registerName(c"setContentSize:".as_ptr()),
                    NSSize { width, height },
                );
                let center: unsafe extern "C" fn(Id, Sel) =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                center(ns_window, sel_registerName(c"center".as_ptr()));
            }
        }
        // SAFETY: see `set_name`.
        let set = unsafe { set_name(ns_window, string) };
        // SAFETY: `string` was created above and is released once.
        unsafe { CFRelease(string) };
        if !set {
            return fail(WindowError::NameInUse);
        }
        Ok(if restored {
            FrameSource::Restored
        } else {
            FrameSource::Centered
        })
    }

    /// The handle's `NSWindow`, on the main thread: `NoWindow` without an
    /// AppKit handle or with a view that has no window, `NotMainThread`
    /// off it. Each error is logged.
    fn window_of(window: &impl HasWindowHandle) -> Result<Id, WindowError> {
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
        Ok(ns_window)
    }

    /// `name` as a CFString the caller releases.
    fn cf_name(name: &str) -> Result<CFStringRef, WindowError> {
        // SAFETY: `name` is valid UTF-8 (ASCII, checked by `valid_name`) of
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
        Ok(string)
    }

    /// `-setFrameAutosaveName:`; false when AppKit returns NO.
    ///
    /// # Safety
    ///
    /// On the main thread, with `ns_window` from `window_of` (the view's
    /// window, alive while the view is, by the Get rule) and `string` a
    /// live CFString, toll-free bridged to the `NSString` the selector
    /// takes. The method returns `BOOL`, one byte on both Apple
    /// architectures; the window copies the string.
    unsafe fn set_name(ns_window: Id, string: CFStringRef) -> bool {
        // SAFETY: the caller's contract.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, CFStringRef) -> u8 =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(
                ns_window,
                sel_registerName(c"setFrameAutosaveName:".as_ptr()),
                string,
            ) != 0
        }
    }

    /// The size of the visible frame of the window's screen, or of the
    /// main screen when the window is on none; None without either.
    ///
    /// # Safety
    ///
    /// On the main thread, with a live `ns_window`.
    unsafe fn visible_size(ns_window: Id) -> Option<(f64, f64)> {
        // SAFETY: `-screen` and `+mainScreen` take nothing and return an
        // object or nil; `NSScreen` is in AppKit, which GPUI links.
        let screen = unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let screen = send(ns_window, sel_registerName(c"screen".as_ptr()));
            if screen.is_null() {
                send(
                    objc_getClass(c"NSScreen".as_ptr()),
                    sel_registerName(c"mainScreen".as_ptr()),
                )
            } else {
                screen
            }
        };
        if screen.is_null() {
            return None;
        }
        // SAFETY: `screen` is a live `NSScreen`; `-visibleFrame` takes
        // nothing and returns an `NSRect`.
        let frame = unsafe { visible_frame(screen) };
        Some((frame.size.width, frame.size.height))
    }

    /// `-visibleFrame`: in registers on arm64.
    ///
    /// # Safety
    ///
    /// `screen` is a live `NSScreen`, on the main thread.
    #[cfg(target_arch = "aarch64")]
    unsafe fn visible_frame(screen: Id) -> NSRect {
        // SAFETY: the caller's contract; `NSRect` is four `f64`, a
        // homogeneous aggregate returned in d0–d3.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> NSRect =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(screen, sel_registerName(c"visibleFrame".as_ptr()))
        }
    }

    /// `-visibleFrame`: through memory on x86_64.
    ///
    /// # Safety
    ///
    /// `screen` is a live `NSScreen`, on the main thread.
    #[cfg(target_arch = "x86_64")]
    unsafe fn visible_frame(screen: Id) -> NSRect {
        let mut frame = NSRect::default();
        // SAFETY: the caller's contract; a 32-byte struct is returned
        // through the pointer passed first.
        unsafe {
            let send: unsafe extern "C" fn(*mut NSRect, Id, Sel) =
                mem::transmute(objc_msgSend_stret as unsafe extern "C" fn());
            send(
                &mut frame,
                screen,
                sel_registerName(c"visibleFrame".as_ptr()),
            );
        }
        frame
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use raw_window_handle::HasWindowHandle;

    use super::{FrameSource, WindowError};

    pub(super) fn fail<T>(error: WindowError) -> Result<T, WindowError> {
        Err(error)
    }

    /// Nothing to do: the window is not touched.
    pub(super) fn set(_window: &impl HasWindowHandle, _name: &str) -> Result<(), WindowError> {
        Ok(())
    }

    /// Nothing to do: the window is not touched.
    pub(super) fn set_or_center(
        _window: &impl HasWindowHandle,
        _name: &str,
        _default_size: (f64, f64),
    ) -> Result<FrameSource, WindowError> {
        Ok(FrameSource::Restored)
    }
}
