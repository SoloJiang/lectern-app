//! Tests of the clipboard write. The general pasteboard is never touched:
//! the macOS test writes to a private pasteboard through the module's own
//! write function, on the test thread (the dispatch to the main queue needs
//! a running main thread, which a test process does not have; the manual
//! check is in docs/design/review_view.md「验收」 step 4).

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, c_char};

    use super::super::imp::{
        Id, NSPasteboardTypeString, Pool, class, sel, send0, send0_discard, send1, write_to,
    };

    /// `[NSPasteboard pasteboardWithUniqueName]`: a private, named
    /// pasteboard that the user never sees.
    fn private_pasteboard() -> Id {
        let class = class(c"NSPasteboard").expect("AppKit is linked");
        // SAFETY: `NSPasteboard` responds to `pasteboardWithUniqueName`.
        let pasteboard = unsafe { send0(class, sel(c"pasteboardWithUniqueName")) };
        assert!(!pasteboard.is_null());
        pasteboard
    }

    /// `[pasteboard stringForType:NSPasteboardTypeString]` as a Rust string.
    fn read_back(pasteboard: Id) -> Option<String> {
        // SAFETY: a live pasteboard and AppKit's constant; the NSString is
        // autoreleased and the bytes are copied before the caller's pool
        // pops.
        unsafe {
            let string = send1(pasteboard, sel(c"stringForType:"), NSPasteboardTypeString);
            if string.is_null() {
                return None;
            }
            let utf8 = send0(string, sel(c"UTF8String"));
            if utf8.is_null() {
                return None;
            }
            Some(
                CStr::from_ptr(utf8.cast::<c_char>())
                    .to_str()
                    .expect("UTF-8")
                    .to_owned(),
            )
        }
    }

    #[test]
    fn clipboard_round_trip_on_a_private_pasteboard() {
        let _pool = Pool::push();
        let pasteboard = private_pasteboard();

        let first = "line one\nline two: 审阅 – café 🙂";
        // SAFETY: a live private pasteboard.
        unsafe { write_to(pasteboard, first) }.expect("the private pasteboard takes text");
        assert_eq!(read_back(pasteboard).as_deref(), Some(first));

        // A second write replaces the first.
        let second = "second";
        // SAFETY: as above.
        unsafe { write_to(pasteboard, second) }.expect("the private pasteboard takes text");
        assert_eq!(read_back(pasteboard).as_deref(), Some(second));

        // SAFETY: `releaseGlobally` takes no arguments; the pasteboard is
        // not used afterwards.
        unsafe { send0_discard(pasteboard, sel(c"releaseGlobally")) };
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn clipboard_is_unsupported_elsewhere() {
    use std::sync::{Arc, Mutex};

    use super::{ClipboardError, write_text};

    let seen = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&seen);
    write_text(
        Arc::from("text"),
        Box::new(move |result| *sink.lock().unwrap() = Some(result)),
    );
    // `done` has already run: nothing was queued.
    assert_eq!(
        seen.lock().unwrap().take(),
        Some(Err(ClipboardError::Unsupported))
    );
}
