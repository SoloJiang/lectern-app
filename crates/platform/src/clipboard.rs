//! Writing text to the system's general pasteboard (「剪贴板」 in
//! docs/design/platform.md; prerequisite I of docs/design/review.md「对其他
//! crate 的要求」, issue #22). The clipboard is never read and the text is
//! never logged.
//!
//! `write_text` may be called on any thread and does not block: on macOS the
//! write is queued on the main dispatch queue (AppKit objects are used on the
//! main thread only) and `done` runs there afterwards. Elsewhere `done` runs
//! at once with `Unsupported`.

use std::fmt;
use std::sync::Arc;

#[cfg(test)]
mod tests;

/// Why the text is not on the clipboard.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ClipboardError {
    /// There is no system clipboard to write to (not macOS).
    Unsupported,
    /// The pasteboard refused the text: `setString:forType:` returned `NO`,
    /// or no string object could be made from the text.
    Rejected,
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ClipboardError::Unsupported => "the system clipboard is not supported here",
            ClipboardError::Rejected => "the clipboard did not take the text",
        })
    }
}

impl std::error::Error for ClipboardError {}

/// What `write_text` calls when the write is over. It runs on the main
/// thread on macOS and must not block.
pub type Done = Box<dyn FnOnce(Result<(), ClipboardError>) + Send>;

/// Replaces the contents of the general pasteboard with `text`, as plain
/// text (`NSPasteboardTypeString`). Any thread; does not block. `done` gets
/// the result: on macOS on the main thread after the write, elsewhere
/// immediately with `Unsupported`. A panic in `done` is caught and dropped
/// (the panic hook of `logging` records it).
pub fn write_text(text: Arc<str>, done: Done) {
    imp::write_text(text, done);
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, c_char, c_void};
    use std::mem;
    use std::panic::{self, AssertUnwindSafe};
    use std::ptr;
    use std::sync::Arc;

    use super::{ClipboardError, Done};

    /// An Objective-C object pointer (`id`).
    pub(super) type Id = *mut c_void;
    pub(super) type Sel = *const c_void;
    /// `BOOL`: one byte on both Apple architectures Lectern builds for.
    type Bool = u8;

    /// `NSUTF8StringEncoding`.
    const NS_UTF8_STRING_ENCODING: usize = 4;

    // The Objective-C runtime, part of libSystem's load set.
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        /// Declared without a signature and cast to the receiver's one per
        /// call, as the runtime documents.
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    // AppKit: `NSPasteboardTypeString` is its exported string constant.
    // Linked here, not only through GPUI, so that this crate's test binary
    // (which has no GPUI) loads AppKit too.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        pub(super) static NSPasteboardTypeString: Id;
    }

    // libdispatch, in libSystem: the main queue and a function-pointer
    // `dispatch_async`.
    unsafe extern "C" {
        static _dispatch_main_q: c_void;
        fn dispatch_async_f(
            queue: *const c_void,
            context: *mut c_void,
            work: unsafe extern "C" fn(*mut c_void),
        );
    }

    pub(super) fn sel(name: &CStr) -> Sel {
        // SAFETY: a NUL-terminated C string.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    pub(super) fn class(name: &CStr) -> Option<Id> {
        // SAFETY: a NUL-terminated C string; null when the class is not
        // loaded.
        let class = unsafe { objc_getClass(name.as_ptr()) };
        (!class.is_null()).then_some(class)
    }

    /// `[receiver selector]` returning an object (or nil).
    ///
    /// # Safety
    ///
    /// `receiver` is a live object or a class that responds to `selector`
    /// with that signature.
    pub(super) unsafe fn send0(receiver: Id, selector: Sel) -> Id {
        // SAFETY: the caller's contract; the cast is to the method's
        // signature.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector)
        }
    }

    /// `[receiver selector]` for a method whose result is not used (an
    /// `NSInteger` or `void`).
    ///
    /// # Safety
    ///
    /// As `send0`.
    pub(super) unsafe fn send0_discard(receiver: Id, selector: Sel) {
        // SAFETY: the caller's contract; the result is a register that is
        // ignored.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector);
        }
    }

    /// `[receiver selector:arg]` returning an object (or nil). Only the
    /// tests read the pasteboard back.
    ///
    /// # Safety
    ///
    /// As `send0`, with one object argument.
    #[cfg(test)]
    pub(super) unsafe fn send1(receiver: Id, selector: Sel, arg: Id) -> Id {
        // SAFETY: the caller's contract; the cast is to the method's
        // signature.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector, arg)
        }
    }

    /// `[pasteboard setString:string forType:ty]`.
    ///
    /// # Safety
    ///
    /// `pasteboard` is a live `NSPasteboard`, `string` and `ty` are live
    /// `NSString`s.
    unsafe fn set_string(pasteboard: Id, string: Id, ty: Id) -> bool {
        // SAFETY: the caller's contract; `BOOL` return.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id, Id) -> Bool =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(pasteboard, sel(c"setString:forType:"), string, ty) != 0
        }
    }

    /// `[[NSString alloc] initWithBytes:bytes length:len encoding:UTF8]`,
    /// owned (+1). Unlike `stringWithUTF8String:` it takes embedded NULs.
    /// None when the class is missing or the result is nil.
    ///
    /// # Safety
    ///
    /// `bytes` is `len` readable bytes.
    unsafe fn new_ns_string(bytes: *const u8, len: usize) -> Option<Id> {
        let ns_string = class(c"NSString")?;
        // SAFETY: `NSString` responds to `alloc`; the init signature below
        // is `initWithBytes:length:encoding:`'s.
        let string = unsafe {
            let allocated = send0(ns_string, sel(c"alloc"));
            if allocated.is_null() {
                return None;
            }
            let init: unsafe extern "C" fn(Id, Sel, *const u8, usize, usize) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            init(
                allocated,
                sel(c"initWithBytes:length:encoding:"),
                bytes,
                len,
                NS_UTF8_STRING_ENCODING,
            )
        };
        (!string.is_null()).then_some(string)
    }

    /// The autorelease pool around the Objective-C calls: popped when
    /// dropped.
    pub(super) struct Pool(*mut c_void);

    impl Pool {
        pub(super) fn push() -> Pool {
            // SAFETY: no arguments; pairs with the pop in `drop`.
            Pool(unsafe { objc_autoreleasePoolPush() })
        }
    }

    impl Drop for Pool {
        fn drop(&mut self) {
            // SAFETY: the token `push` returned, popped once.
            unsafe { objc_autoreleasePoolPop(self.0) };
        }
    }

    /// Replaces the contents of `pasteboard` with `text` as plain text.
    /// Tests pass a private pasteboard; `write_text` passes the general one.
    /// Main thread in production, any thread on a private pasteboard.
    ///
    /// # Safety
    ///
    /// `pasteboard` is a live `NSPasteboard` (not nil).
    pub(super) unsafe fn write_to(pasteboard: Id, text: &str) -> Result<(), ClipboardError> {
        let _pool = Pool::push();
        // SAFETY: `text` is that many readable bytes.
        let string =
            unsafe { new_ns_string(text.as_ptr(), text.len()) }.ok_or(ClipboardError::Rejected)?;
        // SAFETY: `pasteboard` is live (the caller's contract), `string` is
        // the object made above, owned here and released once after the
        // call; `NSPasteboardTypeString` is AppKit's constant.
        let accepted = unsafe {
            send0_discard(pasteboard, sel(c"clearContents"));
            let accepted = set_string(pasteboard, string, NSPasteboardTypeString);
            send0_discard(string, sel(c"release"));
            accepted
        };
        if accepted {
            Ok(())
        } else {
            Err(ClipboardError::Rejected)
        }
    }

    /// `[NSPasteboard generalPasteboard]`, autoreleased: valid inside the
    /// caller's pool.
    fn general_pasteboard() -> Option<Id> {
        let class = class(c"NSPasteboard")?;
        // SAFETY: `NSPasteboard` responds to `generalPasteboard`.
        let pasteboard = unsafe { send0(class, sel(c"generalPasteboard")) };
        (!pasteboard.is_null()).then_some(pasteboard)
    }

    /// What the main-queue block owns.
    struct Job {
        text: Arc<str>,
        done: Done,
    }

    /// The work function of the main-queue block: runs on the main thread.
    unsafe extern "C" fn run_job(context: *mut c_void) {
        // SAFETY: `context` is the `Box<Job>` that `write_text` leaked for
        // this one call; libdispatch calls the function once.
        let job = unsafe { Box::from_raw(context.cast::<Job>()) };
        let Job { text, done } = *job;
        let result = {
            let _pool = Pool::push();
            match general_pasteboard() {
                // SAFETY: the pasteboard is live inside the pool above.
                Some(pasteboard) => unsafe { write_to(pasteboard, &text) },
                None => Err(ClipboardError::Rejected),
            }
        };
        // The callback does not unwind into libdispatch: a panic there
        // would abort the process.
        let _ = panic::catch_unwind(AssertUnwindSafe(move || done(result)));
    }

    pub(super) fn write_text(text: Arc<str>, done: Done) {
        let job = Box::into_raw(Box::new(Job { text, done }));
        // SAFETY: the main queue lives for the whole process; the context
        // is a leaked box that `run_job` takes back exactly once.
        unsafe {
            dispatch_async_f(
                ptr::addr_of!(_dispatch_main_q),
                job.cast::<c_void>(),
                run_job,
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::sync::Arc;

    use super::{ClipboardError, Done};

    pub(super) fn write_text(_text: Arc<str>, done: Done) {
        done(Err(ClipboardError::Unsupported));
    }
}
