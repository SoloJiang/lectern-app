//! The real-window check of `window::set_frame_autosave_name`
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
//! defaults domain, named after the example binary.

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

    use lectern_platform::window::{WindowError, set_frame_autosave_name};
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
    }
}
