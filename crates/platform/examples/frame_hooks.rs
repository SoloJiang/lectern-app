//! The real-window check of `frame::on_first_presented`
//! (docs/design/platform.md「帧的时刻」「测试」). AppKit windows live on the
//! main thread and Rust's test harness never runs a test there, so the check
//! is an example whose `main` is the main thread. It needs a window server
//! session (a locked screen shows nothing, so no drawable gets a
//! `presentedTime`) and runs by hand only:
//!
//! ```sh
//! cargo run --release -p lectern-platform --features bench-hooks --example frame_hooks
//! ```
//!
//! This step checks the first-frame callback only; the 120 frames driven by
//! a CVDisplayLink and the recording checks come with `frame::record`. The
//! example:
//!
//! 1. times `on_first_presented` in 11 cold child processes (it must take
//!    100 µs or less at the median; the first call of a process also pays
//!    for page faults, so single runs vary), then calls it here and checks
//!    that a second call is `AlreadyActive`;
//! 2. makes `NSApplication` and a window whose view is backed by a
//!    `CAMetalLayer` (its own device and command queue), and draws frames in
//!    the order GPUI's display link path uses (`nextDrawable`,
//!    `presentDrawable:`, `commit`) while it runs the main run loop;
//! 3. waits for the callback and checks its time is after this process
//!    started and not after now.
//!
//! It exits 0 when every check holds and panics otherwise. The numbers go to
//! stdout through `io::Write`: the workspace's print lints cover examples
//! too (they keep runtime output in the log), and this is a hand-run
//! measurement, not runtime output.

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
    use std::sync::mpsc;
    use std::time::{Duration, Instant, SystemTime};

    use lectern_platform::HookError;
    use lectern_platform::frame::on_first_presented;
    use lectern_platform::process::start_time;

    type Id = *mut c_void;
    type Sel = *const c_void;

    /// The longest install may take (docs/design/platform.md「性能」).
    const INSTALL_LIMIT: Duration = Duration::from_micros(100);
    /// How many cold processes measure the install (odd, for the median).
    const INSTALL_RUNS: usize = 11;
    /// How long to draw frames before giving up on the callback.
    const FRAMES: u32 = 300;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Size {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    // Loads AppKit, so that `NSApplication`, `NSWindow` and `NSView` exist.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    // Loads QuartzCore (`CAMetalLayer`).
    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> Id;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(
            mode: *const c_void,
            seconds: f64,
            return_after_source_handled: u8,
        ) -> i32;
    }

    fn sel(name: &CStr) -> Sel {
        // SAFETY: a NUL-terminated C string.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    fn class(name: &CStr) -> Id {
        // SAFETY: a NUL-terminated C string.
        let class = unsafe { objc_getClass(name.as_ptr()) };
        assert!(!class.is_null(), "{name:?}");
        class
    }

    /// `[receiver selector]` returning an object.
    ///
    /// # Safety
    ///
    /// `receiver` is a live object or a class that responds to `selector`
    /// with that signature.
    unsafe fn send0(receiver: Id, selector: Sel) -> Id {
        // SAFETY: the caller's contract.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector)
        }
    }

    /// `[receiver selector:arg]` returning an object (or nothing).
    ///
    /// # Safety
    ///
    /// As `send0`, with one object argument.
    unsafe fn send1(receiver: Id, selector: Sel, arg: Id) -> Id {
        // SAFETY: the caller's contract.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector, arg)
        }
    }

    /// `[receiver selector:flag]`.
    ///
    /// # Safety
    ///
    /// As `send0`, with one `BOOL` argument and no result.
    unsafe fn send_bool(receiver: Id, selector: Sel, flag: bool) {
        // SAFETY: the caller's contract; `BOOL` is one byte.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, u8) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector, u8::from(flag));
        }
    }

    /// The window with a layer-hosting view backed by `layer`, shown.
    ///
    /// # Safety
    ///
    /// The main thread, after `NSApplication` exists; `layer` is a live
    /// layer.
    unsafe fn show_window(layer: Id) -> Id {
        // SAFETY: the classes are loaded and the selectors are those of
        // AppKit with the signatures cast here; `NSRect` is four doubles,
        // passed as the ABI passes the struct.
        unsafe {
            let alloc = send0(class(c"NSWindow"), sel(c"alloc"));
            let init: unsafe extern "C" fn(Id, Sel, Rect, u64, u64, u8) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let frame = Rect {
                x: 100.0,
                y: 100.0,
                width: 200.0,
                height: 200.0,
            };
            // Titled, buffered backing, not deferred.
            let window = init(
                alloc,
                sel(c"initWithContentRect:styleMask:backing:defer:"),
                frame,
                1,
                2,
                0,
            );
            assert!(!window.is_null(), "NSWindow");
            let view = send0(send0(class(c"NSView"), sel(c"alloc")), sel(c"init"));
            assert!(!view.is_null(), "NSView");
            // A layer-hosting view: the layer is set before `wantsLayer`.
            send1(view, sel(c"setLayer:"), layer);
            send_bool(view, sel(c"setWantsLayer:"), true);
            send1(window, sel(c"setContentView:"), view);
            send1(window, sel(c"makeKeyAndOrderFront:"), std::ptr::null_mut());
            window
        }
    }

    /// One frame in the order of the display link path: `nextDrawable`,
    /// `presentDrawable:`, `commit`. Returns whether a drawable came.
    ///
    /// # Safety
    ///
    /// The main thread; `layer` is a `CAMetalLayer` with a device; `queue`
    /// is a command queue of that device.
    unsafe fn draw_frame(layer: Id, queue: Id) -> bool {
        // SAFETY: the objects are live, the selectors are Metal's and
        // QuartzCore's; the autoreleased objects die with the pool.
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let drawable = send0(layer, sel(c"nextDrawable"));
            let drawn = !drawable.is_null();
            if drawn {
                let buffer = send0(queue, sel(c"commandBuffer"));
                send1(buffer, sel(c"presentDrawable:"), drawable);
                send0(buffer, sel(c"commit"));
            }
            objc_autoreleasePoolPop(pool);
            drawn
        }
    }

    fn say(line: &str) {
        writeln!(io::stdout(), "{line}").expect("stdout");
    }

    /// The child mode: one cold install, its time in nanoseconds on stdout.
    fn install_once() {
        let started = Instant::now();
        on_first_presented(|_| {}).expect("on_first_presented");
        let elapsed = started.elapsed();
        say(&elapsed.as_nanos().to_string());
    }

    /// The times of `INSTALL_RUNS` cold installs, sorted.
    fn install_costs() -> Vec<Duration> {
        let exe = std::env::current_exe().expect("current_exe");
        let mut costs: Vec<Duration> = (0..INSTALL_RUNS)
            .map(|_| {
                let output = Command::new(&exe)
                    .arg("install")
                    .output()
                    .expect("run an install");
                assert!(output.status.success(), "{output:?}");
                let text = String::from_utf8(output.stdout).expect("utf-8");
                Duration::from_nanos(text.trim().parse().expect("nanoseconds"))
            })
            .collect();
        costs.sort();
        costs
    }

    pub(super) fn run() {
        // SAFETY: no arguments; reads a flag of the calling thread.
        assert_ne!(unsafe { libc::pthread_main_np() }, 0, "main thread");
        if std::env::args().nth(1).as_deref() == Some("install") {
            install_once();
            return;
        }

        // 1. The install: its cost is measured in cold child processes (the
        // first call of a process pays for the class lookup and the page
        // faults; one outlier is not the cost), then done here for the
        // window below.
        let costs = install_costs();
        let (sender, receiver) = mpsc::channel::<SystemTime>();
        on_first_presented(move |time| {
            // The receiver outlives the wait; if it is gone nothing reads.
            let _ = sender.send(time);
        })
        .expect("on_first_presented");
        let again = on_first_presented(|_| {});
        assert!(matches!(again, Err(HookError::AlreadyActive)), "{again:?}");
        let median = costs[costs.len() / 2];
        say(&format!(
            "on_first_presented_us min {:.1} median {:.1} max {:.1} ({} cold processes)",
            costs[0].as_secs_f64() * 1e6,
            median.as_secs_f64() * 1e6,
            costs[costs.len() - 1].as_secs_f64() * 1e6,
            costs.len()
        ));
        assert!(
            median <= INSTALL_LIMIT,
            "the median install took {median:?}, the limit is {INSTALL_LIMIT:?}"
        );

        // 2. A window and frames.
        // SAFETY: the main thread; the classes are loaded; the selectors
        // are those of AppKit and Metal with the signatures cast here.
        let (layer, queue, window) = unsafe {
            let app = send0(class(c"NSApplication"), sel(c"sharedApplication"));
            assert!(!app.is_null(), "NSApplication");
            let set_policy: unsafe extern "C" fn(Id, Sel, i64) -> u8 =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            set_policy(app, sel(c"setActivationPolicy:"), 0);
            send0(app, sel(c"finishLaunching"));
            let device = MTLCreateSystemDefaultDevice();
            assert!(!device.is_null(), "MTLCreateSystemDefaultDevice");
            let layer = send0(send0(class(c"CAMetalLayer"), sel(c"alloc")), sel(c"init"));
            assert!(!layer.is_null(), "CAMetalLayer");
            send1(layer, sel(c"setDevice:"), device);
            let set_size: unsafe extern "C" fn(Id, Sel, Size) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            set_size(
                layer,
                sel(c"setDrawableSize:"),
                Size {
                    width: 200.0,
                    height: 200.0,
                },
            );
            let queue = send0(device, sel(c"newCommandQueue"));
            assert!(!queue.is_null(), "newCommandQueue");
            let window = show_window(layer);
            send_bool(app, sel(c"activateIgnoringOtherApps:"), true);
            (layer, queue, window)
        };
        // TEMPORARY CI diagnostics (bench step 4).
        let diag = |label: &str, drawn: u32| {
            // SAFETY: the main thread; `window` is live; `occlusionState`
            // returns an NSUInteger and `isKeyWindow` a BOOL.
            let (occlusion, key) = unsafe {
                let occlusion: unsafe extern "C" fn(Id, Sel) -> u64 =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                let key: unsafe extern "C" fn(Id, Sel) -> u8 =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                (
                    occlusion(window, sel(c"occlusionState")),
                    key(window, sel(c"isKeyWindow")),
                )
            };
            let [next, null, handlers, zero, last] = lectern_platform::frame::diag_counts();
            say(&format!(
                "diag {label}: drawn {drawn} next {next} null {null} handlers {handlers} \
                 zero {zero} last_presented {} occlusion {occlusion:#x} key {key}",
                f64::from_bits(last)
            ));
        };
        diag("start", 0);

        // 3. Draw until the callback arrives.
        let mut presented = None;
        let mut drawn = 0u32;
        for _ in 0..FRAMES {
            // SAFETY: the main thread; `layer` and `queue` are live.
            if unsafe { draw_frame(layer, queue) } {
                drawn += 1;
            }
            // SAFETY: the default mode; runs the main run loop for 16 ms.
            unsafe {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.016, 0);
            }
            if let Ok(time) = receiver.try_recv() {
                presented = Some(time);
                break;
            }
            if drawn.is_multiple_of(60) {
                diag("frames", drawn);
            }
        }
        diag("end", drawn);
        let Some(presented) = presented else {
            panic!(
                "no first-frame callback after {FRAMES} frames ({drawn} drawables); \
                 is the screen locked or the display asleep?"
            );
        };

        // 4. The time is plausible.
        let now = SystemTime::now();
        let process_started = start_time(std::process::id()).expect("start_time");
        assert!(
            presented > process_started,
            "{presented:?} is not after the process start {process_started:?}"
        );
        assert!(presented <= now, "{presented:?} is after now {now:?}");
        let after_start = presented
            .duration_since(process_started)
            .expect("after start");
        say(&format!(
            "first_presented_after_start_ms {:.1}",
            after_start.as_secs_f64() * 1e3
        ));
        say(&format!("drawables {drawn}"));
    }
}
