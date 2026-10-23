//! The cost of the first `appearance::system_colors` in a process
//! (docs/design/platform.md「系统外观」「性能」). The first read makes AppKit
//! load the system appearance and resolve its dynamic colours for the first
//! time, which the unit tests cannot separate from what an application has
//! already paid by the time `ui::init` runs: `NSApplication` exists and a
//! window may exist. AppKit's application and windows live on the main
//! thread, which Rust's test harness never uses, so this is an example:
//!
//! ```sh
//! cargo run --release -p lectern-platform --example appearance_first_read
//! ```
//!
//! Without an argument it runs itself three times, one process per stage,
//! and prints the time of the first light read, the first dark read and a
//! warm read after each stage: `bare` (nothing of AppKit used before),
//! `app` (`[NSApplication sharedApplication]` and an `NSWindow` created,
//! not shown) and `shown` (the window ordered front and displayed once,
//! which needs a window server session). Exits 0 when every read returns a
//! table. The table goes to stdout through `io::Write`: the workspace's
//! print lints cover examples too (they keep runtime output in the log),
//! and this is a hand-run measurement, not runtime output.

#[cfg(target_os = "macos")]
fn main() {
    real::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod real {
    use std::ffi::{CStr, c_char, c_void};
    use std::io::{self, Write};
    use std::mem;
    use std::process::Command;
    use std::time::Instant;

    use lectern_platform::appearance::{Appearance, system_colors};

    type Id = *mut c_void;
    type Sel = *const c_void;

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
    }

    // Loads AppKit, so that `NSApplication` and `NSWindow` exist.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    fn sel(name: &CStr) -> Sel {
        // SAFETY: a NUL-terminated C string.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    /// `[receiver selector]` returning an object.
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

    /// `[receiver selector:arg]` returning nothing.
    ///
    /// # Safety
    ///
    /// As `send0`, with one object argument and no result.
    unsafe fn send1_void(receiver: Id, selector: Sel, arg: Id) {
        // SAFETY: the caller's contract; the cast is to the method's
        // signature.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector, arg);
        }
    }

    fn class(name: &CStr) -> Id {
        // SAFETY: a NUL-terminated C string.
        let class = unsafe { objc_getClass(name.as_ptr()) };
        assert!(!class.is_null(), "{name:?}");
        class
    }

    /// `[NSApplication sharedApplication]` and `[[NSWindow alloc] init]`;
    /// the window is returned, shown when asked.
    fn prepare(stage: &str) -> Option<Id> {
        if stage == "bare" {
            return None;
        }
        // SAFETY: the classes are loaded; `sharedApplication`, `alloc`
        // and `init` return objects; `orderFront:` takes a sender and
        // `displayIfNeeded` nothing.
        unsafe {
            let app = send0(class(c"NSApplication"), sel(c"sharedApplication"));
            assert!(!app.is_null(), "NSApplication");
            let window = send0(send0(class(c"NSWindow"), sel(c"alloc")), sel(c"init"));
            assert!(!window.is_null(), "NSWindow");
            if stage == "shown" {
                send1_void(window, sel(c"orderFront:"), std::ptr::null_mut());
                send0(window, sel(c"displayIfNeeded"));
            }
            Some(window)
        }
    }

    fn timed(appearance: Appearance) -> f64 {
        let started = Instant::now();
        let colors = system_colors(appearance);
        let elapsed = started.elapsed().as_secs_f64() * 1e6;
        assert!(colors.is_some(), "{appearance:?}");
        elapsed
    }

    /// One stage: prints `<stage> <first light µs> <first dark µs> <warm µs>`.
    fn stage(stage: &str) {
        // SAFETY: no arguments; reads a flag of the calling thread.
        assert_ne!(unsafe { libc::pthread_main_np() }, 0, "main thread");
        let _window = prepare(stage);
        let light = timed(Appearance::Light);
        let dark = timed(Appearance::Dark);
        let warm = timed(Appearance::Light);
        writeln!(io::stdout(), "{stage} {light:.0} {dark:.0} {warm:.0}").expect("stdout");
    }

    pub(super) fn run() {
        if let Some(stage_name) = std::env::args().nth(1) {
            stage(&stage_name);
            return;
        }
        let mut stdout = io::stdout();
        writeln!(stdout, "stage first_light_us first_dark_us warm_us").expect("stdout");
        for stage_name in ["bare", "app", "shown"] {
            let exe = std::env::current_exe().expect("current_exe");
            let output = Command::new(exe)
                .arg(stage_name)
                .output()
                .expect("run the stage");
            assert!(output.status.success(), "stage {stage_name}: {output:?}");
            stdout.write_all(&output.stdout).expect("stdout");
        }
    }
}
