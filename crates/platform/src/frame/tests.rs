//! Tests of the first-frame hook. The clock conversion is pure and runs on
//! every platform; the hook itself needs macOS and `bench-hooks` (`just
//! test` passes `--all-features`); everywhere else the function is checked
//! to be `Unsupported`.
//!
//! The replacement stays for the life of the process. nextest runs each test
//! in its own process; under `cargo test` the tests share one, so none of
//! them depends on the order or on a replacement not being there.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::system_time_of;

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn frame_clock_moment_in_the_past() {
    // The moment is 0.25 s before `media_now`: 0.25 s before `now`.
    assert_eq!(
        system_time_of(1000.0, 1000.25, at(5000)),
        at(5000) - Duration::from_millis(250)
    );
    assert_eq!(system_time_of(7.0, 7.0, at(5000)), at(5000));
}

#[test]
fn frame_clock_moment_slightly_ahead() {
    // Read back to back, `now` can be a little earlier than the moment.
    assert_eq!(
        system_time_of(1000.5, 1000.0, at(5000)),
        at(5000) + Duration::from_millis(500)
    );
}

#[test]
fn frame_clock_never_panics() {
    let now = at(5000);
    // Values that are not finite, or too large for a `Duration`, give `now`.
    for (moment, media_now) in [
        (f64::NAN, 1.0),
        (1.0, f64::NAN),
        (f64::INFINITY, 1.0),
        (1.0, f64::NEG_INFINITY),
        (-1e300, 1e300),
        (1e300, -1e300),
    ] {
        assert_eq!(
            system_time_of(moment, media_now, now),
            now,
            "{moment} {media_now}"
        );
    }
    // A span the system clock cannot represent gives `now` too; one it can
    // gives a time before the epoch. Either way, no panic.
    let result = system_time_of(0.0, 1e18, now);
    assert!(result == now || result < UNIX_EPOCH);
}

#[cfg(not(all(target_os = "macos", feature = "bench-hooks")))]
#[test]
fn frame_is_unsupported_without_bench_hooks() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::HookError;

    let called = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&called);
    let result = super::on_first_presented(super::Fallback::HandlerTime, move |_| {
        seen.store(true, Ordering::SeqCst)
    });
    assert!(matches!(result, Err(HookError::Unsupported)), "{result:?}");
    // A second call gives the same answer: nothing was registered.
    let again = super::on_first_presented(super::Fallback::Wait, |_| {});
    assert!(matches!(again, Err(HookError::Unsupported)), "{again:?}");
    assert!(!called.load(Ordering::SeqCst));
    // The callback was dropped, not kept: the test holds the only handle.
    assert_eq!(Arc::strong_count(&called), 1);
}

#[cfg(all(target_os = "macos", feature = "bench-hooks"))]
mod hooked {
    use std::ffi::CStr;
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::time::Instant;

    use super::super::imp::{find_method, state};
    use super::super::{Fallback, on_first_presented};
    use crate::HookError;

    /// The tests that read or change the registration take turns: under
    /// `cargo test` they share one process.
    static TURN: Mutex<()> = Mutex::new(());

    fn take_turn() -> MutexGuard<'static, ()> {
        TURN.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[test]
    fn frame_first_presented_installs_once() {
        let _turn = take_turn();
        let (registered_before, _) = state();
        let started = Instant::now();
        let first = on_first_presented(Fallback::HandlerTime, |_| {});
        let elapsed = started.elapsed();
        if registered_before {
            // Another test in this process got there first.
            assert!(matches!(first, Err(HookError::AlreadyActive)), "{first:?}");
        } else {
            first.unwrap();
            // Measured cold by `examples/frame_hooks.rs`; this is a sanity
            // bound a loaded CI machine passes, not the 100 µs limit.
            assert!(elapsed.as_millis() < 100, "{elapsed:?}");
        }
        assert!(state().0);
        for _ in 0..2 {
            let again = on_first_presented(Fallback::HandlerTime, |_| {});
            assert!(matches!(again, Err(HookError::AlreadyActive)), "{again:?}");
        }
    }

    #[test]
    fn hook_mismatch_installs_nothing() {
        let _turn = take_turn();
        let before = state();
        let class: &CStr = c"CAMetalLayer";
        let selector: &CStr = c"nextDrawable";
        // The right class, selector and encoding are found.
        assert!(find_method(class, selector, c"@16@0:8").is_ok());
        for (class, selector, encoding, what) in [
            (class, selector, c"v16@0:8", "CAMetalLayer nextDrawable"),
            (class, selector, c"", "CAMetalLayer nextDrawable"),
            (
                class,
                c"noSuchSelector",
                c"@16@0:8",
                "CAMetalLayer noSuchSelector",
            ),
            (
                c"NoSuchClassAnywhere",
                selector,
                c"@16@0:8",
                "NoSuchClassAnywhere nextDrawable",
            ),
        ] {
            match find_method(class, selector, encoding) {
                Err(HookError::Mismatch { what: found }) => assert_eq!(found, what),
                other => panic!("{what}: {other:?}"),
            }
        }
        // None of that replaced anything or used the one registration.
        assert_eq!(state(), before);
    }

    /// Metal and Core Animation on a layer in no window, as the tests of
    /// `frame_hooks_on_an_offscreen_layer` (docs/design/platform.md) do.
    mod offscreen {
        use std::ffi::{CStr, c_char, c_void};
        use std::mem;

        pub type Id = *mut c_void;
        type Sel = *const c_void;

        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Size {
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

        #[link(name = "Metal", kind = "framework")]
        unsafe extern "C" {
            fn MTLCreateSystemDefaultDevice() -> Id;
        }

        fn sel(name: &CStr) -> Sel {
            // SAFETY: a NUL-terminated C string.
            unsafe { sel_registerName(name.as_ptr()) }
        }

        /// `[receiver selector]` returning an object (or nil).
        ///
        /// # Safety
        ///
        /// `receiver` is a live object or a class that responds to
        /// `selector` with that signature.
        unsafe fn send0(receiver: Id, selector: &CStr) -> Id {
            // SAFETY: the caller's contract.
            unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> Id =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                send(receiver, sel(selector))
            }
        }

        /// `[receiver selector:arg]` returning an object (or nothing).
        ///
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

        /// A 64 x 64 `CAMetalLayer` on the system's Metal device and a
        /// command queue of it. None when the machine has no Metal device.
        pub struct Layer {
            layer: Id,
            queue: Id,
        }

        impl Layer {
            pub fn new() -> Option<Layer> {
                // SAFETY: the classes are loaded (QuartzCore is linked by
                // the code under test, Metal here); the selectors are
                // those of Metal and QuartzCore with the signatures cast.
                unsafe {
                    let device = MTLCreateSystemDefaultDevice();
                    if device.is_null() {
                        return None;
                    }
                    let class = objc_getClass(c"CAMetalLayer".as_ptr());
                    let layer = send0(send0(class, c"alloc"), c"init");
                    assert!(!layer.is_null(), "CAMetalLayer");
                    send1(layer, c"setDevice:", device);
                    let set_size: unsafe extern "C" fn(Id, Sel, Size) =
                        mem::transmute(objc_msgSend as unsafe extern "C" fn());
                    set_size(
                        layer,
                        sel(c"setDrawableSize:"),
                        Size {
                            width: 64.0,
                            height: 64.0,
                        },
                    );
                    let queue = send0(device, c"newCommandQueue");
                    assert!(!queue.is_null(), "newCommandQueue");
                    Some(Layer { layer, queue })
                }
            }

            /// One frame in the order of the display link path:
            /// `nextDrawable`, `presentDrawable:`, `commit`, then waits
            /// until the GPU is done. Whether a drawable came.
            pub fn draw(&self) -> bool {
                // SAFETY: `layer` and `queue` are live for `self`; the
                // autoreleased objects die with the pool.
                unsafe {
                    let pool = objc_autoreleasePoolPush();
                    let drawable = send0(self.layer, c"nextDrawable");
                    let drawn = !drawable.is_null();
                    if drawn {
                        let buffer = send0(self.queue, c"commandBuffer");
                        send1(buffer, c"presentDrawable:", drawable);
                        send0(buffer, c"commit");
                        send0(buffer, c"waitUntilCompleted");
                    }
                    objc_autoreleasePoolPop(pool);
                    drawn
                }
            }
        }
    }

    #[test]
    fn frame_hook_passes_drawables_through_on_an_offscreen_layer() {
        let _turn = take_turn();
        // The registration of this process, if no other test made it.
        match on_first_presented(Fallback::HandlerTime, |_| {}) {
            Ok(()) | Err(HookError::AlreadyActive) => {}
            Err(other) => panic!("{other:?}"),
        }
        let Some(layer) = offscreen::Layer::new() else {
            // No Metal device (a virtual machine without a GPU): there is
            // no layer to draw on.
            return;
        };
        // More frames than a layer has drawables (three): each one has to
        // be released by its present for the next to come, so a hook that
        // loses a drawable, or blocks, shows here.
        for frame in 0..8 {
            assert!(layer.draw(), "frame {frame}");
        }
    }

    /// A slot holding a callback that sends what it gets.
    fn slot() -> (
        std::sync::atomic::AtomicPtr<super::super::imp::FirstPresented>,
        std::sync::mpsc::Receiver<super::super::PresentedAt>,
    ) {
        use super::super::imp::FirstPresented;

        let (sender, receiver) = std::sync::mpsc::channel();
        let callback: FirstPresented = Box::new(move |at| sender.send(at).unwrap());
        (
            std::sync::atomic::AtomicPtr::new(Box::into_raw(Box::new(callback))),
            receiver,
        )
    }

    #[test]
    fn hook_deliver_gives_a_shown_drawable_its_presented_time_once() {
        use std::ptr;
        use std::sync::atomic::Ordering;
        use std::time::{Duration, SystemTime};

        use super::super::TimeSource;
        use super::super::imp::{CACurrentMediaTime, deliver};

        let (slot, receiver) = slot();
        // A drawable shown 0.5 s ago: the callback gets the system time of
        // that moment, within the time the two clock readings are apart.
        let before = SystemTime::now();
        // SAFETY: no arguments.
        let shown = unsafe { CACurrentMediaTime() } - 0.5;
        deliver(&slot, shown, true);
        let after = SystemTime::now();
        let at = receiver.try_recv().unwrap();
        assert_eq!(at.source, TimeSource::Presented);
        let slack = Duration::from_millis(100);
        let time = at.time;
        assert!(
            time + Duration::from_millis(500) + slack >= before,
            "{time:?}"
        );
        assert!(
            time + Duration::from_millis(500) <= after + slack,
            "{time:?}"
        );

        // Once: the slot is empty, a later drawable does nothing, shown or not.
        assert!(slot.load(Ordering::Acquire).is_null());
        // SAFETY: no arguments.
        deliver(&slot, unsafe { CACurrentMediaTime() }, true);
        deliver(&slot, 0.0, true);
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            slot.swap(ptr::null_mut(), Ordering::AcqRel),
            ptr::null_mut()
        );
    }

    #[test]
    fn hook_deliver_without_a_presented_time_gives_the_handler_time_once() {
        use std::sync::atomic::Ordering;
        use std::time::{Duration, SystemTime};

        use super::super::TimeSource;
        use super::super::imp::{CACurrentMediaTime, deliver};

        // 0 (no time from the machine, or a drawable not shown), and values
        // that are no time at all: the moment of the handler, on the
        // system clock.
        for not_shown in [0.0, -1.0, f64::NAN, f64::NEG_INFINITY] {
            let (slot, receiver) = slot();
            let before = SystemTime::now();
            deliver(&slot, not_shown, true);
            let after = SystemTime::now();
            let at = receiver.try_recv().unwrap();
            assert_eq!(at.source, TimeSource::Handler, "{not_shown}");
            let slack = Duration::from_millis(100);
            assert!(at.time + slack >= before, "{not_shown}: {at:?}");
            assert!(at.time <= after + slack, "{not_shown}: {at:?}");
            // Once.
            assert!(slot.load(Ordering::Acquire).is_null(), "{not_shown}");
            // SAFETY: no arguments.
            deliver(&slot, unsafe { CACurrentMediaTime() }, true);
            assert!(receiver.try_recv().is_err(), "{not_shown}");
        }
    }

    #[test]
    fn hook_deliver_waits_for_a_shown_drawable_without_the_handler_time() {
        use std::sync::atomic::Ordering;

        use super::super::TimeSource;
        use super::super::imp::{CACurrentMediaTime, deliver};

        // `Fallback::Wait`: drawables without a time (the hidden first one)
        // leave the callback in the slot; the first shown one gets it.
        let (slot, receiver) = slot();
        for not_shown in [0.0, -1.0, f64::NAN] {
            deliver(&slot, not_shown, false);
            assert!(receiver.try_recv().is_err(), "{not_shown}");
            assert!(!slot.load(Ordering::Acquire).is_null(), "{not_shown}");
        }
        // SAFETY: no arguments.
        deliver(&slot, unsafe { CACurrentMediaTime() } - 0.1, false);
        let at = receiver.try_recv().unwrap();
        assert_eq!(at.source, TimeSource::Presented);
        assert!(slot.load(Ordering::Acquire).is_null());
    }
}
