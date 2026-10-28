//! The real-window check of `window::set_frame_autosave_name` and
//! `window::set_frame_autosave_name_or_center`
//! (docs/design/platform.md「窗口 frame 的 autosave」「测试」). AppKit windows
//! live on the main thread and Rust's test harness never runs a test there,
//! so the check is an example whose `main` is the main thread. It needs a
//! window server session and runs by hand only:
//!
//! ```sh
//! cargo run -p lectern-platform --example frame_autosave
//! ```
//!
//! It exits 0 when every assertion holds and panics otherwise. The windows
//! are never shown. AppKit keeps the frames it saves under this process's
//! defaults domain, named after the example binary; the frame the centring
//! check saves is removed before the example exits.

#[cfg(target_os = "macos")]
fn main() {
    real::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod real {
    use std::ffi::{CStr, c_char, c_void};
    use std::mem;
    use std::ptr::NonNull;

    use lectern_platform::window::{
        FrameSource, WindowError, set_frame_autosave_name, set_frame_autosave_name_or_center,
    };
    use raw_window_handle::{
        AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle,
    };

    type Id = *mut c_void;
    type Sel = *const c_void;
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            is_external: u8,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
    }

    // Loads AppKit, so that `NSWindow` exists in this process.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    fn sel(name: &CStr) -> Sel {
        // SAFETY: a NUL-terminated C string.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    /// `[receiver selector]` for a selector without arguments that returns
    /// an object.
    ///
    /// # Safety
    ///
    /// `receiver` is a live object or a class that responds to `selector`
    /// with that signature.
    unsafe fn send0(receiver: Id, selector: Sel) -> Id {
        // SAFETY: the caller's contract; the cast is to the method's
        // signature.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector)
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_msgSend_stret();
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    struct Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    /// `[receiver selector]` for a selector without arguments that returns
    /// an `NSRect`.
    ///
    /// # Safety
    ///
    /// As `send0`, for a method that returns an `NSRect`.
    unsafe fn rect(receiver: Id, selector: Sel) -> Rect {
        #[cfg(target_arch = "aarch64")]
        // SAFETY: the caller's contract; four `f64` come back in d0–d3.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Rect =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector)
        }
        #[cfg(target_arch = "x86_64")]
        // SAFETY: the caller's contract; 32 bytes come back through memory.
        unsafe {
            let mut out = Rect::default();
            let send: unsafe extern "C" fn(*mut Rect, Id, Sel) =
                mem::transmute(objc_msgSend_stret as unsafe extern "C" fn());
            send(&mut out, receiver, selector);
            out
        }
    }

    /// A CFString (toll-free an `NSString`) the caller releases.
    fn cf_string(text: &str) -> *const c_void {
        // SAFETY: `text` is UTF-8 of `len` bytes; the default allocator.
        let string = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                text.as_ptr(),
                text.len() as isize,
                UTF8,
                0,
            )
        };
        assert!(!string.is_null(), "CFStringCreateWithBytes");
        string
    }

    /// `[[NSWindow alloc] init]` on the main thread.
    fn new_window() -> Id {
        // SAFETY: `NSWindow` is a class of the loaded AppKit; `alloc` and
        // `init` return an object.
        unsafe {
            let class = objc_getClass(c"NSWindow".as_ptr());
            assert!(!class.is_null(), "NSWindow");
            let window = send0(send0(class, sel(c"alloc")), sel(c"init"));
            assert!(!window.is_null(), "[[NSWindow alloc] init]");
            window
        }
    }

    /// The window's frame autosave name as AppKit reports it.
    fn autosave_name(window: Id) -> String {
        let mut buf = [0u8; 256];
        // SAFETY: `window` is live; `frameAutosaveName` returns an NSString
        // (toll-free a CFString), never nil; `buf` is valid for its length.
        let ok = unsafe {
            let string = send0(window, sel(c"frameAutosaveName"));
            assert!(!string.is_null(), "frameAutosaveName");
            CFStringGetCString(
                string,
                buf.as_mut_ptr().cast::<c_char>(),
                buf.len() as isize,
                UTF8,
            )
        };
        assert_ne!(ok, 0, "CFStringGetCString");
        let end = buf.iter().position(|&b| b == 0).unwrap();
        String::from_utf8(buf[..end].to_vec()).unwrap()
    }

    /// The handle GPUI gives: the window's content view.
    struct View(NonNull<c_void>);

    impl View {
        fn of(window: Id) -> View {
            // SAFETY: `window` is live; `contentView` returns its view.
            let view = unsafe { send0(window, sel(c"contentView")) };
            View(NonNull::new(view).expect("contentView"))
        }
    }

    impl HasWindowHandle for View {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let raw = RawWindowHandle::AppKit(AppKitWindowHandle::new(self.0));
            // SAFETY: the view lives as long as its window, which this
            // example never releases.
            Ok(unsafe { WindowHandle::borrow_raw(raw) })
        }
    }

    pub(super) fn run() {
        // SAFETY: no arguments.
        assert_ne!(unsafe { libc::pthread_main_np() }, 0, "main thread");
        let first = new_window();
        let second = new_window();
        let name = format!("lectern-example-{}", std::process::id());

        assert_eq!(autosave_name(first), "");
        assert_eq!(set_frame_autosave_name(&View::of(first), &name), Ok(()));
        assert_eq!(autosave_name(first), name);

        // The same name on another window of the process: AppKit either
        // takes it or answers NO (「已知限制」); nothing else may happen.
        match set_frame_autosave_name(&View::of(second), &name) {
            Ok(()) => assert_eq!(autosave_name(second), name),
            Err(WindowError::NameInUse) => assert_eq!(autosave_name(second), ""),
            Err(other) => panic!("the second window: {other:?}"),
        }

        // Another name on the second window, and a new name on the first.
        let other = format!("{name}-2");
        assert_eq!(set_frame_autosave_name(&View::of(second), &other), Ok(()));
        assert_eq!(autosave_name(second), other);
        let renamed = format!("{name}-3");
        assert_eq!(set_frame_autosave_name(&View::of(first), &renamed), Ok(()));
        assert_eq!(autosave_name(first), renamed);

        // Off the main thread the window is never touched.
        let view = View::of(first).0.as_ptr() as usize;
        let name_for_thread = name.clone();
        let off = std::thread::spawn(move || {
            let view = View(NonNull::new(view as *mut c_void).unwrap());
            set_frame_autosave_name(&view, &name_for_thread)
        })
        .join()
        .unwrap();
        assert_eq!(off, Err(WindowError::NotMainThread));
        assert_eq!(autosave_name(first), renamed);

        center_then_restore();
    }

    /// A name with no saved frame: the window gets the size and is centred;
    /// a window named the same later gets the frame saved after a move.
    fn center_then_restore() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("lectern-center-{}-{nanos}", std::process::id());
        let size = (640.0, 400.0);

        let window = new_window();
        assert_eq!(
            set_frame_autosave_name_or_center(&View::of(window), &name, size),
            Ok(FrameSource::Centered)
        );
        assert_eq!(autosave_name(window), name);
        // SAFETY: `window` is live; `contentView`, `frame`, `screen` and
        // `visibleFrame` take nothing.
        let (content, frame, visible) = unsafe {
            let content = rect(send0(window, sel(c"contentView")), sel(c"frame"));
            let frame = rect(window, sel(c"frame"));
            let screen = send0(window, sel(c"screen"));
            let screen = if screen.is_null() {
                send0(objc_getClass(c"NSScreen".as_ptr()), sel(c"mainScreen"))
            } else {
                screen
            };
            assert!(!screen.is_null(), "no screen");
            (content, frame, rect(screen, sel(c"visibleFrame")))
        };
        assert_eq!(
            (content.width, content.height),
            (size.0.min(visible.width), size.1.min(visible.height)),
            "content size"
        );
        // `center` puts the window in the middle horizontally and somewhat
        // above the middle vertically.
        let mid_x = frame.x + frame.width / 2.0;
        let screen_mid_x = visible.x + visible.width / 2.0;
        assert!(
            (mid_x - screen_mid_x).abs() <= 1.0,
            "{frame:?} on {visible:?}"
        );
        assert!(frame.y >= visible.y && frame.y + frame.height <= visible.y + visible.height + 1.0);

        // Move it and save, as AppKit does after a move of a shown window.
        let moved = Rect {
            x: visible.x + 10.0,
            y: visible.y + 10.0,
            ..frame
        };
        let string = cf_string(&name);
        // SAFETY: `window` is live; `setFrameOrigin:` takes an `NSPoint` (two
        // `f64`), `saveFrameUsingName:` an `NSString`; both return void.
        unsafe {
            let origin: unsafe extern "C" fn(Id, Sel, f64, f64) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            origin(window, sel(c"setFrameOrigin:"), moved.x, moved.y);
            let save: unsafe extern "C" fn(Id, Sel, *const c_void) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            save(window, sel(c"saveFrameUsingName:"), string);
        }

        // Another window, the same name: the saved frame comes back. AppKit
        // may refuse the name itself while the first window holds it
        // (「已知限制」); the frame is applied before the name is set.
        let again = new_window();
        match set_frame_autosave_name_or_center(&View::of(again), &name, size) {
            Ok(FrameSource::Restored) | Err(WindowError::NameInUse) => {}
            other => panic!("the second window: {other:?}"),
        }
        // SAFETY: `again` is live.
        let restored = unsafe { rect(again, sel(c"frame")) };
        assert_eq!(restored, moved);

        // Leave no key behind in the example's defaults.
        // SAFETY: `removeFrameUsingName:` is a class method of `NSWindow`
        // that takes an `NSString` and returns void; `string` is released
        // once.
        unsafe {
            let remove: unsafe extern "C" fn(Id, Sel, *const c_void) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            remove(
                objc_getClass(c"NSWindow".as_ptr()),
                sel(c"removeFrameUsingName:"),
                string,
            );
            CFRelease(string);
        }
    }
}
