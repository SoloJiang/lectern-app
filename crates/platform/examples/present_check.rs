//! TEMPORARY CI diagnostics (bench step 4; removed before the review):
//! does a plain `CAMetalLayer` in a visible window ever get a
//! `presentedTime` above 0 on this machine? No method is replaced here.
//!
//! One window with a layer-hosting view. Three variants of 120 frames
//! each; every frame takes `[layer nextDrawable]`, adds a presented handler
//! to the drawable, clears its texture with a new colour in a render pass,
//! and presents it:
//!
//! - `buffer`: `[buffer presentDrawable:]`, `[buffer commit]` (GPUI's
//!   display-link path);
//! - `transaction`: `presentsWithTransaction = YES`, `[buffer commit]`,
//!   `[buffer waitUntilScheduled]`, `[drawable present]` (GPUI's other
//!   path);
//! - `at_time`: `[buffer presentDrawable:atTime:]` 50 ms ahead.
//!
//! Prints, per variant, the handlers called, those with a time not above
//! 0, and the first times seen. Exits 0 always.
//!
//! ```sh
//! cargo run --release -p lectern-platform --example present_check
//! ```

#[cfg(target_os = "macos")]
fn main() {
    real::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod real {
    use std::ffi::{CStr, c_char, c_int, c_ulong, c_void};
    use std::io::{self, Write};
    use std::mem;
    use std::ptr;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    type Id = *mut c_void;
    type Sel = *const c_void;

    const FRAMES: u32 = 120;
    /// How many presented times to print per variant.
    const KEPT: usize = 8;

    static HANDLERS: AtomicU64 = AtomicU64::new(0);
    static ZERO: AtomicU64 = AtomicU64::new(0);
    static NONZERO: AtomicU64 = AtomicU64::new(0);
    /// The first `KEPT` times (bits of an `f64`), in call order.
    static TIMES: [AtomicU64; KEPT] = [const { AtomicU64::new(0) }; KEPT];
    static PRESENTED_TIME: AtomicU64 = AtomicU64::new(0);

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

    /// `MTLClearColor`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ClearColor {
        red: f64,
        green: f64,
        blue: f64,
        alpha: f64,
    }

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {
        fn CACurrentMediaTime() -> f64;
    }

    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> Id;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayIsAsleep(display: u32) -> u32;
        fn CGDisplayIsActive(display: u32) -> u32;
        fn CGDisplayIsOnline(display: u32) -> u32;
        fn CGDisplayPixelsWide(display: u32) -> usize;
        fn CGDisplayPixelsHigh(display: u32) -> usize;
        fn CGSessionCopyCurrentDictionary() -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFShow(object: *const c_void);
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(
            mode: *const c_void,
            seconds: f64,
            return_after_source_handled: u8,
        ) -> i32;
    }

    unsafe extern "C" {
        static _NSConcreteStackBlock: [*const c_void; 32];
    }

    #[repr(C)]
    struct BlockDescriptor {
        reserved: c_ulong,
        size: c_ulong,
    }

    // SAFETY: two integers, read only.
    unsafe impl Sync for BlockDescriptor {}

    type Invoke = unsafe extern "C" fn(*mut c_void, Id);

    #[repr(C)]
    struct Block {
        isa: *const c_void,
        flags: c_int,
        reserved: c_int,
        invoke: Invoke,
        descriptor: *const BlockDescriptor,
    }

    static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: mem::size_of::<Block>() as c_ulong,
    };

    unsafe extern "C" fn presented(_block: *mut c_void, drawable: Id) {
        let selector = PRESENTED_TIME.load(Relaxed) as Sel;
        // SAFETY: `drawable` is the `MTLDrawable` the handler is called
        // with; `presentedTime` returns a double.
        let time = unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> f64 =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(drawable, selector)
        };
        let index = HANDLERS.fetch_add(1, Relaxed);
        if time > 0.0 {
            NONZERO.fetch_add(1, Relaxed);
        } else {
            ZERO.fetch_add(1, Relaxed);
        }
        if let Some(slot) = usize::try_from(index).ok().and_then(|i| TIMES.get(i)) {
            slot.store(time.to_bits(), Relaxed);
        }
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

    /// # Safety
    ///
    /// `receiver` responds to `selector` with no argument, returning an
    /// object (or nothing).
    unsafe fn send0(receiver: Id, selector: &CStr) -> Id {
        // SAFETY: the caller's contract.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, sel(selector))
        }
    }

    /// # Safety
    ///
    /// As `send0`, with one object argument.
    unsafe fn send1(receiver: Id, selector: &CStr, arg: Id) -> Id {
        // SAFETY: the caller's contract.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, sel(selector), arg)
        }
    }

    /// # Safety
    ///
    /// As `send0`, with one integer argument.
    unsafe fn send_u64(receiver: Id, selector: &CStr, arg: u64) -> Id {
        // SAFETY: the caller's contract.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, u64) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, sel(selector), arg)
        }
    }

    /// # Safety
    ///
    /// As `send0`, with one `BOOL` argument and no result.
    unsafe fn send_bool(receiver: Id, selector: &CStr, flag: bool) {
        // SAFETY: the caller's contract.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, u8) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, sel(selector), u8::from(flag));
        }
    }

    fn say(line: &str) {
        let _ = writeln!(io::stdout(), "present_check: {line}");
    }

    fn run_loop(seconds: f64) {
        // SAFETY: the main thread's run loop, default mode.
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 0);
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Variant {
        Buffer,
        Transaction,
        AtTime,
    }

    /// One frame of `variant`. Returns whether a drawable came.
    ///
    /// # Safety
    ///
    /// The main thread; `layer` has a device whose queue is `queue`.
    unsafe fn frame(layer: Id, queue: Id, variant: Variant, n: u32) -> bool {
        // SAFETY: Metal and QuartzCore selectors with the signatures cast
        // here; the autoreleased objects die with the pool.
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let drawable = send0(layer, c"nextDrawable");
            let drawn = !drawable.is_null();
            if drawn {
                let block = Block {
                    isa: ptr::addr_of!(_NSConcreteStackBlock).cast::<c_void>(),
                    flags: 0,
                    reserved: 0,
                    invoke: presented as Invoke,
                    descriptor: &DESCRIPTOR,
                };
                send1(
                    drawable,
                    c"addPresentedHandler:",
                    ptr::from_ref(&block).cast_mut().cast::<c_void>(),
                );
                let buffer = send0(queue, c"commandBuffer");
                let pass = send0(class(c"MTLRenderPassDescriptor"), c"renderPassDescriptor");
                let attachment = send_u64(
                    send0(pass, c"colorAttachments"),
                    c"objectAtIndexedSubscript:",
                    0,
                );
                send1(attachment, c"setTexture:", send0(drawable, c"texture"));
                send_u64(attachment, c"setLoadAction:", 2); // MTLLoadActionClear
                send_u64(attachment, c"setStoreAction:", 1); // MTLStoreActionStore
                let set_clear: unsafe extern "C" fn(Id, Sel, ClearColor) =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                let shade = f64::from(n % 60) / 60.0;
                set_clear(
                    attachment,
                    sel(c"setClearColor:"),
                    ClearColor {
                        red: shade,
                        green: 0.3,
                        blue: 1.0 - shade,
                        alpha: 1.0,
                    },
                );
                let encoder = send1(buffer, c"renderCommandEncoderWithDescriptor:", pass);
                send0(encoder, c"endEncoding");
                match variant {
                    Variant::Buffer => {
                        send1(buffer, c"presentDrawable:", drawable);
                        send0(buffer, c"commit");
                    }
                    Variant::Transaction => {
                        send0(buffer, c"commit");
                        send0(buffer, c"waitUntilScheduled");
                        send0(drawable, c"present");
                    }
                    Variant::AtTime => {
                        let at: unsafe extern "C" fn(Id, Sel, Id, f64) =
                            mem::transmute(objc_msgSend as unsafe extern "C" fn());
                        at(
                            buffer,
                            sel(c"presentDrawable:atTime:"),
                            drawable,
                            CACurrentMediaTime() + 0.05,
                        );
                        send0(buffer, c"commit");
                    }
                }
            }
            objc_autoreleasePoolPop(pool);
            drawn
        }
    }

    pub(super) fn run() {
        PRESENTED_TIME.store(sel(c"presentedTime") as u64, Relaxed);
        // SAFETY: CoreGraphics queries with no pointers; `CFShow` prints the
        // session dictionary (or "(null)") to stderr.
        unsafe {
            let display = CGMainDisplayID();
            say(&format!(
                "main display {display}: asleep {} active {} online {} {}x{}",
                CGDisplayIsAsleep(display),
                CGDisplayIsActive(display),
                CGDisplayIsOnline(display),
                CGDisplayPixelsWide(display),
                CGDisplayPixelsHigh(display),
            ));
            say("session dictionary on stderr:");
            CFShow(CGSessionCopyCurrentDictionary());
        }
        // SAFETY: the main thread; AppKit, Metal and QuartzCore selectors
        // with the signatures cast here; `NSRect` is four doubles.
        let (layer, queue, window) = unsafe {
            let app = send0(class(c"NSApplication"), c"sharedApplication");
            let set_policy: unsafe extern "C" fn(Id, Sel, i64) -> u8 =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            set_policy(app, sel(c"setActivationPolicy:"), 0);
            send0(app, c"finishLaunching");
            let device = MTLCreateSystemDefaultDevice();
            assert!(!device.is_null(), "MTLCreateSystemDefaultDevice");
            let name = send0(device, c"name");
            let utf8: unsafe extern "C" fn(Id, Sel) -> *const c_char =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let name = CStr::from_ptr(utf8(name, sel(c"UTF8String")));
            say(&format!("device {}", name.to_string_lossy()));
            let layer = send0(send0(class(c"CAMetalLayer"), c"alloc"), c"init");
            send1(layer, c"setDevice:", device);
            let set_size: unsafe extern "C" fn(Id, Sel, Size) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            set_size(
                layer,
                sel(c"setDrawableSize:"),
                Size {
                    width: 400.0,
                    height: 400.0,
                },
            );
            let queue = send0(device, c"newCommandQueue");
            let alloc = send0(class(c"NSWindow"), c"alloc");
            let init: unsafe extern "C" fn(Id, Sel, Rect, u64, u64, u8) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let window = init(
                alloc,
                sel(c"initWithContentRect:styleMask:backing:defer:"),
                Rect {
                    x: 200.0,
                    y: 200.0,
                    width: 400.0,
                    height: 400.0,
                },
                1,
                2,
                0,
            );
            let view = send0(send0(class(c"NSView"), c"alloc"), c"init");
            send1(view, c"setLayer:", layer);
            send_bool(view, c"setWantsLayer:", true);
            send1(window, c"setContentView:", view);
            send1(window, c"makeKeyAndOrderFront:", ptr::null_mut());
            send_bool(app, c"activateIgnoringOtherApps:", true);
            (layer, queue, window)
        };
        // Let the window reach the screen first.
        run_loop(0.5);
        for variant in [Variant::Buffer, Variant::Transaction, Variant::AtTime] {
            HANDLERS.store(0, Relaxed);
            ZERO.store(0, Relaxed);
            NONZERO.store(0, Relaxed);
            for slot in &TIMES {
                slot.store(0, Relaxed);
            }
            // SAFETY: the main thread; `layer` is live.
            unsafe {
                send_bool(
                    layer,
                    c"setPresentsWithTransaction:",
                    matches!(variant, Variant::Transaction),
                );
            }
            let mut drawn = 0;
            for n in 0..FRAMES {
                // SAFETY: the main thread; `layer` and `queue` are live.
                if unsafe { frame(layer, queue, variant, n) } {
                    drawn += 1;
                }
                run_loop(0.016);
            }
            run_loop(0.5);
            // SAFETY: the main thread; `window` is live; `occlusionState`
            // returns an NSUInteger.
            let occlusion = unsafe {
                let get: unsafe extern "C" fn(Id, Sel) -> u64 =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                get(window, sel(c"occlusionState"))
            };
            let times: Vec<f64> = TIMES
                .iter()
                .map(|t| f64::from_bits(t.load(Relaxed)))
                .collect();
            // SAFETY: no arguments.
            let now = unsafe { CACurrentMediaTime() };
            say(&format!(
                "{variant:?}: drawn {drawn} handlers {} zero {} nonzero {} occlusion {occlusion:#x} \
                 media_now {now:.3} first_times {times:?}",
                HANDLERS.load(Relaxed),
                ZERO.load(Relaxed),
                NONZERO.load(Relaxed),
            ));
        }
    }
}
