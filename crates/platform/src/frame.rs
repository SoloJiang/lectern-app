//! The time of frames on screen, for the measurement build of
//! `lectern-bench` (「帧的时刻」 in docs/design/platform.md, ADR 0048).
//!
//! Locked GPUI shows no drawable, so the method every frame passes through,
//! `-[CAMetalLayer nextDrawable]`, is replaced with the Objective-C runtime's
//! `method_setImplementation`, and the drawable it returns gets an
//! `addPresentedHandler:` block that reads `presentedTime`. A drawable that
//! was not shown has 0. For the first frame ([`on_first_presented`]), with
//! [`Fallback::HandlerTime`] (machines that give no `presentedTime`, as the
//! hosted CI runner) the time is then the moment the handler runs, and the
//! result says so; with [`Fallback::Wait`] (the judging machine) the next
//! drawable is waited for (ADR 0052). A recording ([`record`]) also replaces
//! the command buffer's `commit`, to read each frame's `GPUEndTime`, and
//! runs a CVDisplayLink for the refresh each frame answers; the pairing of
//! those moments is in `pairing`, which needs no Objective-C. All of that is
//! behind the `bench-hooks` feature on macOS; everywhere else the functions
//! compile and return `HookError::Unsupported`, and no Objective-C runtime
//! function is called.

use std::marker::PhantomData;
use std::time::{Duration, SystemTime};

use raw_window_handle::HasWindowHandle;

use crate::HookError;

#[cfg(any(all(target_os = "macos", feature = "bench-hooks"), test))]
mod pairing;
#[cfg(test)]
mod tests;

/// Where a frame's time came from (ADR 0052).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeSource {
    /// The drawable's `presentedTime`: when it was shown on the display.
    Presented,
    /// The drawable's `presentedTime` was not above 0: the machine gives none
    /// (the hosted CI runner, a locked screen) or the drawable was not shown.
    /// The time is the moment the presented handler ran, which is after the
    /// drawable was handed to Core Animation. It is not a display time.
    Handler,
}

impl TimeSource {
    /// "presented" or "handler", for reports.
    pub fn as_str(self) -> &'static str {
        match self {
            TimeSource::Presented => "presented",
            TimeSource::Handler => "handler",
        }
    }
}

/// What a presented handler with `presentedTime` 0 does (ADR 0052). The first
/// drawable GPUI draws comes before the window is on screen and always has 0,
/// also on a physical Mac; the drawable drawn when the window is shown has
/// its real time there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// Give the moment the handler runs ([`TimeSource::Handler`]). For a
    /// machine that gives no `presentedTime` at all.
    HandlerTime,
    /// Give nothing and wait for a drawable with a `presentedTime`. For the
    /// judging machine.
    Wait,
}

/// A frame's time on the system clock, and its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentedAt {
    pub time: SystemTime,
    pub source: TimeSource,
}

/// Registers the first-frame callback. The first presented handler whose
/// drawable has a `presentedTime` greater than 0 gives `f` that time,
/// converted to the system clock ([`TimeSource::Presented`]). A handler with
/// 0 (or any value not greater than 0) gives the moment it runs
/// ([`TimeSource::Handler`]) when `fallback` is [`Fallback::HandlerTime`],
/// and nothing when it is [`Fallback::Wait`]. After `f` has run, handlers
/// give nothing.
///
/// `f` runs once, on the thread Core Animation calls the presented handler
/// on; it should not block, and it must not panic (a panic in a callback
/// from the system aborts the process). Call it before the window is
/// created (the probe calls it at the start of `main`); any thread.
///
/// Before the first frame this makes exactly one method replacement: no IO,
/// no thread, no Metal object; at most 100 µs. The class, the selector and
/// the type encoding are checked first, and on a mismatch nothing is
/// replaced. The replacement is never undone.
///
/// A second call is `AlreadyActive`; a method that cannot be replaced is
/// `Mismatch`; without the `bench-hooks` feature, or not on macOS,
/// `Unsupported` (and `f` is dropped, never called).
pub fn on_first_presented(
    fallback: Fallback,
    f: impl FnOnce(PresentedAt) + Send + 'static,
) -> Result<(), HookError> {
    imp::on_first_presented(fallback, Box::new(f))
}

/// Starts recording the per-frame moments of `window`. After the first
/// frame, on the main thread; at most one recorder at a time.
///
/// The first call replaces the command buffer's `commit` (and
/// `nextDrawable`, if `on_first_presented` has not) and makes a
/// CVDisplayLink for the window's display, kept for later calls; under
/// 1 ms. Off the main thread `NotMainThread`; a handle without a
/// `CAMetalLayer`, window or screen `NoMetalLayer`; a recorder that exists
/// `AlreadyActive`; a method that cannot be replaced `Mismatch`; a display
/// link that cannot be made `Os`; without the `bench-hooks` feature, or not
/// on macOS, `Unsupported`.
pub fn record(window: &impl HasWindowHandle) -> Result<FrameRecorder, HookError> {
    imp::record(window).map(FrameRecorder::new)
}

/// One recording. Not `Send`: made, used and stopped on the main thread.
/// Dropped without `stop`, it stops and its frames are lost.
pub struct FrameRecorder {
    inner: imp::Recorder,
    _main_thread: PhantomData<*const ()>,
}

impl FrameRecorder {
    fn new(inner: imp::Recorder) -> FrameRecorder {
        FrameRecorder {
            inner,
            _main_thread: PhantomData,
        }
    }

    /// A frame starts. Called at the start of GPUI's frame handling, in the
    /// `Window::on_next_frame` callback (before layout), for every frame
    /// handling while recording, also those that draw nothing. Main thread;
    /// does not block or allocate.
    pub fn frame_started(&self) {
        self.inner.frame_started();
    }

    /// The nominal refresh period of the window's display
    /// (`CVDisplayLinkGetNominalOutputVideoRefreshPeriod`), read by `record`.
    pub fn refresh(&self) -> Duration {
        self.inner.refresh()
    }

    /// Stops recording: later frames are not recorded and the display link
    /// stops. Main thread; does not block.
    pub fn stop(self) -> PendingFrames {
        PendingFrames {
            inner: self.inner.stop(),
        }
    }
}

/// A stopped recording whose frames may still be on the GPU or waiting to
/// be shown. `Send`.
pub struct PendingFrames {
    inner: imp::Pending,
}

impl PendingFrames {
    /// Waits until every frame has its completion time and its presented
    /// handler, or until `timeout`, then converts the moments and gives the
    /// result. Blocks: on a background thread. Checks every 1 ms (the
    /// handlers take no lock, so there is no condition to wait on).
    pub fn wait(self, timeout: Duration) -> FrameRecording {
        self.inner.wait(timeout)
    }
}

/// A recording's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameRecording {
    pub refresh: Duration,
    /// In frame order; only frames with a completion time.
    pub frames: Vec<FrameTimes>,
    pub problems: FrameProblems,
}

/// The four moments of a frame, on the system clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTimes {
    /// The `CVTimeStamp.hostTime` of this module's CVDisplayLink's first
    /// tick after the `frame_started` before this frame: the vertical sync
    /// of the refresh the frame answers. Without a tick between the two
    /// starts (a frame the display link did not ask for), `started`, counted
    /// in `FrameProblems::without_tick`.
    pub requested: SystemTime,
    /// This frame's `frame_started`.
    pub started: SystemTime,
    /// The `GPUEndTime` of this frame's command buffer.
    pub completed: SystemTime,
    /// The drawable's presented handler, by the rule of the first frame
    /// (ADR 0052): `presentedTime` when greater than 0 (`Presented`),
    /// otherwise the moment the handler ran (`Handler`). None when the
    /// handler had not run by the end of `wait`. Reported, not judged.
    pub presented: Option<PresentedAt>,
}

/// The counts of frames that do not pair. Any of the first three above 0
/// makes the recording untrustworthy (bench reports it as `error`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameProblems {
    /// Drawables taken outside a frame handling, or a second one in the
    /// same frame handling: the hooks no longer match GPUI's frames.
    pub unmatched: u32,
    /// Frames without a completion time by the end of `wait` (the `commit`
    /// hook did not take, the command buffer failed, or the wait was too
    /// short). They are not in `frames`.
    pub incomplete: u32,
    /// Frames past the capacity (8192), not recorded.
    pub overflowed: u32,
    /// Frames the display link did not ask for (`requested` is `started`).
    /// Reported only.
    pub without_tick: u32,
}

impl FrameProblems {
    /// `unmatched`, `incomplete` and `overflowed` are all 0.
    pub fn is_clean(&self) -> bool {
        self.unmatched == 0 && self.incomplete == 0 && self.overflowed == 0
    }
}

/// The system time of a moment given on the media clock (`CACurrentMediaTime`
/// and `presentedTime`: seconds on the `mach_absolute_time` clock, which does
/// not run while the machine sleeps). `media_now` and `now` are read back to
/// back, so their difference from the moment is the age, and the result is
/// `now` minus the age (plus it, for a moment slightly ahead). Never panics:
/// a value that is not finite, or too large for a `Duration`, gives `now`.
#[cfg(any(all(target_os = "macos", feature = "bench-hooks"), test))]
fn system_time_of(moment: f64, media_now: f64, now: SystemTime) -> SystemTime {
    use std::time::Duration;

    let age = media_now - moment;
    let Ok(span) = Duration::try_from_secs_f64(age.abs()) else {
        return now;
    };
    let shifted = if age >= 0.0 {
        now.checked_sub(span)
    } else {
        now.checked_add(span)
    };
    shifted.unwrap_or(now)
}

#[cfg(all(target_os = "macos", feature = "bench-hooks"))]
mod imp {
    use std::cell::Cell;
    use std::ffi::{CStr, c_char, c_int, c_ulong, c_void};
    use std::io;
    use std::mem;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock, PoisonError};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime};

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::pairing::{Clock, Store, Taken};
    use super::{Fallback, FrameRecording, PresentedAt, TimeSource, system_time_of};
    use crate::HookError;
    use crate::hook::check_type_encoding;

    /// An Objective-C object pointer (`id`).
    type Id = *mut c_void;
    type Sel = *const c_void;
    /// A `Method` of the runtime.
    type Method = *mut c_void;
    /// A `CVDisplayLinkRef`.
    type Link = *mut c_void;
    /// The callback of `on_first_presented`.
    pub(super) type FirstPresented = Box<dyn FnOnce(PresentedAt) + Send>;

    /// The class and selector replaced, and the type encoding the method
    /// must have: returns an object, takes `self` and `_cmd`.
    const CLASS: &CStr = c"CAMetalLayer";
    const NEXT_DRAWABLE: &CStr = c"nextDrawable";
    const NEXT_DRAWABLE_ENCODING: &CStr = c"@16@0:8";
    /// The command buffer's `commit`: returns nothing, takes `self` and
    /// `_cmd`. Its class is private (the GPU driver's, or Metal API
    /// validation's wrapper), so it is found from an instance.
    const COMMIT: &CStr = c"commit";
    const COMMIT_ENCODING: &CStr = c"v16@0:8";
    /// How often `PendingFrames::wait` checks.
    const WAIT_STEP: Duration = Duration::from_millis(1);

    // The Objective-C runtime, part of libSystem's load set.
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn object_getClass(object: Id) -> Id;
        fn class_getName(class: Id) -> *const c_char;
        fn sel_registerName(name: *const c_char) -> Sel;
        /// Declared without a signature and cast to the receiver's one per
        /// call, as the runtime documents.
        fn objc_msgSend();
        fn class_getInstanceMethod(class: Id, selector: Sel) -> Method;
        fn method_getTypeEncoding(method: Method) -> *const c_char;
        /// `IMP` is a function pointer; held as a plain pointer here.
        fn method_getImplementation(method: Method) -> *const c_void;
        fn method_setImplementation(method: Method, implementation: *const c_void)
        -> *const c_void;
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    // QuartzCore: `CAMetalLayer`, and the media clock. Linked here, not
    // only through GPUI, so that this crate's test binary (which has no
    // GPUI) loads the class too.
    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn CACurrentMediaTime() -> f64;
    }

    /// `mach_timebase_info_data_t`.
    #[repr(C)]
    #[derive(Default)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    // The tick clock, in libSystem. Declared here: `libc` marks both
    // deprecated in favour of `clock_gettime_nsec_np`, which is not the
    // clock `CVTimeStamp.hostTime` is on.
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> c_int;
    }

    /// `CVTime`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CvTime {
        time_value: i64,
        time_scale: i32,
        flags: i32,
    }

    /// `kCVTimeIsIndefinite`.
    const CV_TIME_IS_INDEFINITE: i32 = 1;

    /// The head of `CVTimeStamp`, up to `hostTime`: only read through the
    /// pointer CoreVideo passes, never made here.
    #[repr(C)]
    struct CvTimeStampHead {
        version: u32,
        video_time_scale: i32,
        video_time: i64,
        host_time: u64,
    }

    /// `CVDisplayLinkOutputCallback`.
    type OutputCallback = unsafe extern "C" fn(
        Link,
        *const CvTimeStampHead,
        *const CvTimeStampHead,
        u64,
        *mut u64,
        *mut c_void,
    ) -> i32;

    // CoreVideo, linked by GPUI too (its own display link).
    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        fn CVDisplayLinkCreateWithCGDisplay(display: u32, link: *mut Link) -> i32;
        fn CVDisplayLinkSetOutputCallback(
            link: Link,
            callback: OutputCallback,
            context: *mut c_void,
        ) -> i32;
        fn CVDisplayLinkStart(link: Link) -> i32;
        fn CVDisplayLinkStop(link: Link) -> i32;
        fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link: Link) -> CvTime;
    }

    // The blocks runtime, in libSystem: the class of a block literal on the
    // stack.
    unsafe extern "C" {
        static _NSConcreteStackBlock: [*const c_void; 32];
    }

    /// The fixed part of a block literal's descriptor (the blocks ABI,
    /// `Block_descriptor_1`): with no copy and dispose helpers and no
    /// signature in `flags`, nothing follows these two.
    #[repr(C)]
    struct BlockDescriptor {
        reserved: c_ulong,
        size: c_ulong,
    }

    // SAFETY: two integers, read only.
    unsafe impl Sync for BlockDescriptor {}

    /// A block literal (`Block_literal_1`) with one captured value. Built
    /// on the stack; Metal copies it when it keeps it, and a copy of a
    /// block without helpers is a plain memcpy, which is what a `Copy`
    /// payload wants.
    #[repr(C)]
    struct Block<F: Copy, T: Copy> {
        isa: *const c_void,
        flags: c_int,
        reserved: c_int,
        invoke: F,
        descriptor: *const BlockDescriptor,
        payload: T,
    }

    impl<F: Copy, T: Copy> Block<F, T> {
        /// A stack block with no copy or dispose helpers and no signature.
        /// `descriptor.size` must be `size_of::<Self>()`.
        fn new(invoke: F, descriptor: &'static BlockDescriptor, payload: T) -> Self {
            Block {
                isa: ptr::addr_of!(_NSConcreteStackBlock).cast::<c_void>(),
                flags: 0,
                reserved: 0,
                invoke,
                descriptor,
                payload,
            }
        }

        /// The block as the `id` an Objective-C method takes.
        fn as_id(&self) -> Id {
            ptr::from_ref(self).cast_mut().cast::<c_void>()
        }
    }

    /// Set by the one successful registration. A second call is
    /// `AlreadyActive`.
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    /// The first-frame callback: a leaked `Box<FirstPresented>`, null once
    /// taken (by the one handler that sees a shown drawable) or before the
    /// registration. The replacement and the block only load or swap it.
    static CALLBACK: AtomicPtr<FirstPresented> = AtomicPtr::new(ptr::null_mut());
    /// `Fallback::HandlerTime` as true, set once before `CALLBACK`.
    static HANDLER_TIME: AtomicBool = AtomicBool::new(false);
    /// The implementation `nextDrawable` had before the replacement.
    static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    /// The implementation `commit` had before its replacement, on the one
    /// command buffer class replaced.
    static COMMIT_ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    /// Serializes the installations, so that no replacement is stored as
    /// its own original.
    static INSTALL: Mutex<()> = Mutex::new(());
    /// The selectors the replacements and the blocks send, registered at
    /// install time: `sel_registerName` takes a lock inside the runtime,
    /// and none of them may.
    static ADD_PRESENTED_HANDLER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    static PRESENTED_TIME: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    static ADD_COMPLETED_HANDLER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    static GPU_END_TIME: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    /// The memory of every recording, allocated by the first `record` and
    /// never freed (「存储与生命周期」).
    static STORE: OnceLock<&'static Store> = OnceLock::new();
    /// The thread that records (`pthread_self`), 0 for none: the hooks
    /// pair only that thread's drawables and commits.
    static RECORDING_THREAD: AtomicUsize = AtomicUsize::new(0);
    /// One display link per display, made by the first recording on it and
    /// never released: `CVDisplayLinkStop` does not wait for a callback in
    /// flight. Locked on the recording thread only.
    static LINKS: Mutex<Vec<(u32, usize)>> = Mutex::new(Vec::new());

    /// The blocks' `invoke`: the system passes the block itself and the
    /// drawable or the command buffer.
    type Invoke = unsafe extern "C" fn(*mut c_void, Id);

    static PRESENTED_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: mem::size_of::<Block<Invoke, ()>>() as c_ulong,
    };

    /// A recording's blocks capture the generation and the slot.
    static RECORDED_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: mem::size_of::<Block<Invoke, u64>>() as c_ulong,
    };

    fn payload(generation: u32, index: u32) -> u64 {
        (u64::from(generation) << 32) | u64::from(index)
    }

    /// The generation and the slot a recording's block captured.
    ///
    /// # Safety
    ///
    /// `block` is a block made with `RECORDED_DESCRIPTOR` (or Metal's copy
    /// of one).
    unsafe fn captured(block: *mut c_void) -> (u32, u32) {
        // SAFETY: the caller's contract: the payload is a `u64`.
        let payload = unsafe { (*block.cast::<Block<Invoke, u64>>()).payload };
        ((payload >> 32) as u32, payload as u32)
    }

    /// `[receiver selector]` returning a double.
    ///
    /// # Safety
    ///
    /// `receiver` responds to `selector` with that signature.
    unsafe fn send_f64(receiver: Id, selector: *mut c_void) -> f64 {
        // SAFETY: the caller's contract.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> f64 =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector)
        }
    }

    /// `[receiver selector]` returning an object (or nil).
    ///
    /// # Safety
    ///
    /// `receiver` responds to `selector` with that signature.
    unsafe fn send0(receiver: Id, selector: &CStr) -> Id {
        // SAFETY: the caller's contract; the selector is a C string.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, sel_registerName(selector.as_ptr()))
        }
    }

    /// `[receiver selector:arg]` returning an object, or nothing.
    ///
    /// # Safety
    ///
    /// As `send0`, with one object argument.
    unsafe fn send1(receiver: Id, selector: *mut c_void, arg: Id) -> Id {
        // SAFETY: the caller's contract.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector, arg)
        }
    }

    /// The body of the first-frame presented handler, on a Core Animation
    /// thread. Takes no lock, allocates nothing and does not panic, up to
    /// the callback the caller gave.
    unsafe extern "C" fn presented_invoke(_block: *mut c_void, drawable: Id) {
        let selector = PRESENTED_TIME.load(Ordering::Acquire);
        if selector.is_null() || drawable.is_null() {
            return;
        }
        // SAFETY: `drawable` is the `MTLDrawable` the handler is called
        // with; `presentedTime` takes no argument and returns a
        // `CFTimeInterval` (a double).
        let presented = unsafe { send_f64(drawable, selector) };
        deliver(&CALLBACK, presented, HANDLER_TIME.load(Ordering::Acquire));
    }

    /// What the presented handler does with the `presentedTime` it read:
    /// takes the callback out of `slot` (an atomic swap: only one thread
    /// gets it) and calls it with the time on the system clock. A
    /// `presentedTime` greater than 0 is that time ([`TimeSource::Presented`]).
    /// 0 or any value that is not greater than 0 (false for NaN) gives the
    /// moment of this call ([`TimeSource::Handler`]) when `handler_time`,
    /// and otherwise leaves the callback in `slot` for a later drawable
    /// (ADR 0052). Takes no lock, allocates nothing, does not panic, up to
    /// the callback.
    pub(super) fn deliver(slot: &AtomicPtr<FirstPresented>, presented: f64, handler_time: bool) {
        let shown = presented.partial_cmp(&0.0) == Some(std::cmp::Ordering::Greater);
        if !shown && !handler_time {
            return;
        }
        let callback = slot.swap(ptr::null_mut(), Ordering::AcqRel);
        if callback.is_null() {
            return;
        }
        // The two readings back to back: their difference is the error of
        // the conversion.
        // SAFETY: no arguments.
        let media_now = unsafe { CACurrentMediaTime() };
        let now = SystemTime::now();
        let (moment, source) = if shown {
            (presented, TimeSource::Presented)
        } else {
            (media_now, TimeSource::Handler)
        };
        // SAFETY: `callback` came from `Box::into_raw` in
        // `on_first_presented`, and the swap gave it to this call alone.
        // Running the callback consumes the box: the free is the callback's
        // own, part of the one exception to "does not allocate" (「安装的规则」).
        let callback = unsafe { Box::from_raw(callback) };
        callback(PresentedAt {
            time: system_time_of(moment, media_now, now),
            source,
        });
    }

    /// A recording's presented handler, on a Core Animation thread: stores
    /// `presentedTime` and the moment it ran into the frame's slot.
    unsafe extern "C" fn recorded_presented_invoke(block: *mut c_void, drawable: Id) {
        let selector = PRESENTED_TIME.load(Ordering::Acquire);
        let Some(store) = STORE.get() else {
            return;
        };
        if selector.is_null() || drawable.is_null() {
            return;
        }
        // SAFETY: the block was made by `next_drawable` with
        // `RECORDED_DESCRIPTOR`; `drawable` is an `MTLDrawable` and
        // `presentedTime` returns a double.
        let ((generation, index), presented, now) = unsafe {
            (
                captured(block),
                send_f64(drawable, selector),
                CACurrentMediaTime(),
            )
        };
        store.presented(generation, index, presented, now);
    }

    /// A recording's completed handler, on a Metal thread: stores the
    /// command buffer's `GPUEndTime` into the frame's slot.
    unsafe extern "C" fn completed_invoke(block: *mut c_void, buffer: Id) {
        let selector = GPU_END_TIME.load(Ordering::Acquire);
        let Some(store) = STORE.get() else {
            return;
        };
        if selector.is_null() || buffer.is_null() {
            return;
        }
        // SAFETY: the block was made by `commit` with
        // `RECORDED_DESCRIPTOR`; `buffer` is an `MTLCommandBuffer` and
        // `GPUEndTime` returns a `CFTimeInterval`.
        let ((generation, index), gpu_end) =
            unsafe { (captured(block), send_f64(buffer, selector)) };
        store.completed(generation, index, gpu_end);
    }

    /// The store, while it records and the caller is the recording thread.
    fn recording_here() -> Option<&'static Store> {
        let store = *STORE.get()?;
        if !store.active() {
            return None;
        }
        // SAFETY: no arguments.
        let this = unsafe { libc::pthread_self() } as usize;
        (this == RECORDING_THREAD.load(Ordering::Acquire)).then_some(store)
    }

    /// The replacement of `-[CAMetalLayer nextDrawable]`: calls the original,
    /// then registers the presented handlers the drawable needs: the
    /// first-frame one while its callback waits, a recording's one while a
    /// recording runs on this thread. Otherwise it only reads two atomics.
    /// GPUI's two presentation paths both come after `nextDrawable`, so the
    /// handlers are registered before the drawable is shown.
    unsafe extern "C" fn next_drawable(this: Id, selector: Sel) -> Id {
        let original = ORIGINAL.load(Ordering::Acquire);
        if original.is_null() {
            // Not reachable: `ORIGINAL` is set before the replacement.
            return ptr::null_mut();
        }
        // SAFETY: `original` is the implementation of `nextDrawable` the
        // runtime gave: `id (*)(id, SEL)`, checked by the type encoding.
        let drawable = unsafe {
            let original: unsafe extern "C" fn(Id, Sel) -> Id = mem::transmute(original);
            original(this, selector)
        };
        if drawable.is_null() {
            return drawable;
        }
        let add = ADD_PRESENTED_HANDLER.load(Ordering::Acquire);
        if !CALLBACK.load(Ordering::Acquire).is_null() {
            let block = Block::new(presented_invoke as Invoke, &PRESENTED_DESCRIPTOR, ());
            // SAFETY: `drawable` conforms to `MTLDrawable`;
            // `addPresentedHandler:` takes a `void (^)(id<MTLDrawable>)`
            // block (which `block` is laid out as) and returns nothing;
            // Metal copies the block, so it may end here on the stack.
            unsafe {
                send1(drawable, add, block.as_id());
            }
        }
        if let Some(store) = recording_here()
            && let Some(index) = store.drawable()
        {
            let block = Block::new(
                recorded_presented_invoke as Invoke,
                &RECORDED_DESCRIPTOR,
                payload(store.generation(), index),
            );
            // SAFETY: as above.
            unsafe {
                send1(drawable, add, block.as_id());
            }
        }
        drawable
    }

    /// The replacement of the command buffer's `commit`: while a recording
    /// runs on this thread, the first commit after a frame's drawable gets
    /// a completed handler (before the commit: Metal takes none after),
    /// then the original runs. Otherwise it only reads two atomics.
    unsafe extern "C" fn commit(this: Id, selector: Sel) {
        let original = COMMIT_ORIGINAL.load(Ordering::Acquire);
        if original.is_null() {
            // Not reachable: `COMMIT_ORIGINAL` is set before the replacement.
            return;
        }
        if let Some(store) = recording_here()
            && let Some(index) = store.commit()
        {
            let block = Block::new(
                completed_invoke as Invoke,
                &RECORDED_DESCRIPTOR,
                payload(store.generation(), index),
            );
            // SAFETY: `this` is an `MTLCommandBuffer` not yet committed;
            // `addCompletedHandler:` takes a
            // `void (^)(id<MTLCommandBuffer>)` block and returns nothing;
            // Metal copies the block.
            unsafe {
                send1(
                    this,
                    ADD_COMPLETED_HANDLER.load(Ordering::Acquire),
                    block.as_id(),
                );
            }
        }
        // SAFETY: `original` is the implementation of `commit` the runtime
        // gave: `void (*)(id, SEL)`, checked by the type encoding.
        unsafe {
            let original: unsafe extern "C" fn(Id, Sel) = mem::transmute(original);
            original(this, selector);
        }
    }

    /// Looks the method up and checks its type encoding: the part of an
    /// installation that changes nothing. `Mismatch` when the class or the
    /// method is missing or the encoding differs.
    pub(super) fn find_method(
        class: &CStr,
        selector: &CStr,
        encoding: &CStr,
    ) -> Result<Method, HookError> {
        // SAFETY: NUL-terminated C strings; null when the class is not
        // loaded.
        let class_id = unsafe { objc_getClass(class.as_ptr()) };
        if class_id.is_null() {
            return Err(HookError::Mismatch {
                what: mismatch_name(class, selector),
            });
        }
        method_of(class_id, class, selector, encoding)
    }

    /// The method of a class, checked as `find_method` does; `name` is the
    /// class's name, for the error.
    fn method_of(
        class_id: Id,
        name: &CStr,
        selector: &CStr,
        encoding: &CStr,
    ) -> Result<Method, HookError> {
        let what = mismatch_name(name, selector);
        // SAFETY: `class_id` is a class; the selector is a C string.
        let method =
            unsafe { class_getInstanceMethod(class_id, sel_registerName(selector.as_ptr())) };
        if method.is_null() {
            return Err(HookError::Mismatch { what });
        }
        // SAFETY: `method` is live; the runtime gives a NUL-terminated
        // string owned by it, or null.
        let actual = unsafe {
            let actual = method_getTypeEncoding(method);
            (!actual.is_null()).then(|| CStr::from_ptr(actual))
        };
        check_type_encoding(&what, actual, encoding)?;
        Ok(method)
    }

    /// "class selector", for `HookError::Mismatch`.
    fn mismatch_name(class: &CStr, selector: &CStr) -> String {
        format!("{} {}", class.to_string_lossy(), selector.to_string_lossy())
    }

    /// Whether a registration has happened, and the implementation
    /// `-[CAMetalLayer nextDrawable]` has now (0 if there is none): what a
    /// failed installation must leave as it was. For the tests.
    #[cfg(test)]
    pub(super) fn state() -> (bool, usize) {
        let implementation = find_method(CLASS, NEXT_DRAWABLE, NEXT_DRAWABLE_ENCODING)
            // SAFETY: `method` is live.
            .map_or(0, |method| unsafe {
                method_getImplementation(method) as usize
            });
        (REGISTERED.load(Ordering::Acquire), implementation)
    }

    /// Replaces `method` with `ours`, keeping the implementation it had in
    /// `original`, once: an implementation that already is ours (replaced
    /// earlier, or through a superclass) is left. With `only_one`, a
    /// second method (another class) is left too. Registers the selectors
    /// the replacements send first.
    fn replace(method: Method, ours: *const c_void, original: &AtomicPtr<c_void>, only_one: bool) {
        let _install = INSTALL.lock().unwrap_or_else(PoisonError::into_inner);
        // SAFETY: C strings.
        unsafe {
            for (slot, name) in [
                (&ADD_PRESENTED_HANDLER, c"addPresentedHandler:"),
                (&PRESENTED_TIME, c"presentedTime"),
                (&ADD_COMPLETED_HANDLER, c"addCompletedHandler:"),
                (&GPU_END_TIME, c"GPUEndTime"),
            ] {
                slot.store(
                    sel_registerName(name.as_ptr()).cast_mut(),
                    Ordering::Release,
                );
            }
        }
        if only_one && !original.load(Ordering::Acquire).is_null() {
            return;
        }
        // SAFETY: `method` is live. The original is stored before the
        // replacement is, so that no call of the replacement finds it null.
        unsafe {
            let current = method_getImplementation(method);
            if current != ours {
                original.store(current.cast_mut(), Ordering::Release);
                method_setImplementation(method, ours);
            }
        }
    }

    fn replace_next_drawable() -> Result<(), HookError> {
        let method = find_method(CLASS, NEXT_DRAWABLE, NEXT_DRAWABLE_ENCODING)?;
        let ours = next_drawable as unsafe extern "C" fn(Id, Sel) -> Id as *const c_void;
        replace(method, ours, &ORIGINAL, false);
        Ok(())
    }

    pub(super) fn on_first_presented(
        fallback: Fallback,
        callback: FirstPresented,
    ) -> Result<(), HookError> {
        if REGISTERED.load(Ordering::Acquire) {
            return Err(HookError::AlreadyActive);
        }
        let method = find_method(CLASS, NEXT_DRAWABLE, NEXT_DRAWABLE_ENCODING)?;
        if REGISTERED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(HookError::AlreadyActive);
        }
        HANDLER_TIME.store(fallback == Fallback::HandlerTime, Ordering::Release);
        CALLBACK.store(Box::into_raw(Box::new(callback)), Ordering::Release);
        let ours = next_drawable as unsafe extern "C" fn(Id, Sel) -> Id as *const c_void;
        replace(method, ours, &ORIGINAL, false);
        Ok(())
    }

    /// Replaces `commit` on the class of the command buffers of `layer`'s
    /// device: a command queue and a command buffer are made to find it,
    /// then released. Only the first class is replaced; a device with
    /// another class gets no completion times (every frame `incomplete`).
    ///
    /// # Safety
    ///
    /// `layer` is a live `CAMetalLayer`.
    unsafe fn replace_commit(layer: Id) -> Result<(), HookError> {
        // SAFETY: the caller's contract; `device`, `newCommandQueue` and
        // `commandBuffer` are Metal's, with no arguments; the queue is
        // owned (+1) and released here, the buffer dies with the pool.
        unsafe {
            let device = send0(layer, c"device");
            if device.is_null() {
                return Err(HookError::NoMetalLayer);
            }
            let pool = objc_autoreleasePoolPush();
            let queue = send0(device, c"newCommandQueue");
            let result = if queue.is_null() {
                Err(HookError::Os(io::Error::other("newCommandQueue gave nil")))
            } else {
                let buffer = send0(queue, c"commandBuffer");
                let result = if buffer.is_null() {
                    Err(HookError::Os(io::Error::other("commandBuffer gave nil")))
                } else {
                    let class = object_getClass(buffer);
                    let name = CStr::from_ptr(class_getName(class));
                    method_of(class, name, COMMIT, COMMIT_ENCODING).map(|method| {
                        let ours = commit as unsafe extern "C" fn(Id, Sel) as *const c_void;
                        replace(method, ours, &COMMIT_ORIGINAL, true);
                    })
                };
                send0(queue, c"release");
                result
            };
            objc_autoreleasePoolPop(pool);
            result
        }
    }

    /// The display link's callback, on a CoreVideo thread: one tick into
    /// the store given as the context.
    unsafe extern "C" fn tick(
        _link: Link,
        now: *const CvTimeStampHead,
        _output: *const CvTimeStampHead,
        _flags_in: u64,
        _flags_out: *mut u64,
        context: *mut c_void,
    ) -> i32 {
        if !now.is_null() && !context.is_null() {
            // SAFETY: the context is the leaked `Store`; `now` is the
            // time stamp CoreVideo passes, valid for the call.
            unsafe { (*context.cast::<Store>()).tick((*now).host_time) };
        }
        0
    }

    /// The display link of `display`, made (with `store` as its context)
    /// the first time, and its nominal refresh period.
    fn link_of(display: u32, store: &'static Store) -> Result<(Link, Duration), HookError> {
        let mut links = LINKS.lock().unwrap_or_else(PoisonError::into_inner);
        let link = match links.iter().find(|(id, _)| *id == display) {
            Some(&(_, link)) => link as Link,
            None => {
                let mut link: Link = ptr::null_mut();
                // SAFETY: `link` is written on success; the callback and the
                // context live for the process (the store is never freed,
                // nor is the link).
                let code = unsafe {
                    let code = CVDisplayLinkCreateWithCGDisplay(display, &mut link);
                    if code == 0 {
                        CVDisplayLinkSetOutputCallback(
                            link,
                            tick,
                            ptr::from_ref(store).cast_mut().cast(),
                        )
                    } else {
                        code
                    }
                };
                if code != 0 || link.is_null() {
                    return Err(HookError::Os(io::Error::other(format!(
                        "CVDisplayLink for display {display}: {code}"
                    ))));
                }
                links.push((display, link as usize));
                link
            }
        };
        // SAFETY: `link` is a live display link.
        let period = unsafe { CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link) };
        let refresh = if period.flags & CV_TIME_IS_INDEFINITE == 0
            && period.time_scale > 0
            && period.time_value > 0
        {
            Duration::from_secs_f64(period.time_value as f64 / f64::from(period.time_scale))
        } else {
            Duration::ZERO
        };
        Ok((link, refresh))
    }

    fn now_ticks() -> u64 {
        // SAFETY: no arguments.
        unsafe { mach_absolute_time() }
    }

    /// The tick clock and the system clock, read back to back.
    fn clock_now() -> Clock {
        let mut timebase = Timebase::default();
        // SAFETY: `timebase` is written; the call does not fail on Apple
        // systems.
        unsafe { mach_timebase_info(&mut timebase) };
        let ticks = now_ticks();
        Clock::new(timebase.numer, timebase.denom, ticks, SystemTime::now())
    }

    /// One recording, on the thread that made it.
    pub(super) struct Recorder {
        store: &'static Store,
        refresh: Duration,
        link: Option<usize>,
        stopped: Cell<bool>,
    }

    impl Recorder {
        pub(super) fn frame_started(&self) {
            self.store.frame_started(now_ticks());
        }

        pub(super) fn refresh(&self) -> Duration {
            self.refresh
        }

        pub(super) fn stop(&self) -> Pending {
            let taken = self.halt();
            Pending {
                store: self.store,
                taken,
                refresh: self.refresh,
            }
        }

        fn halt(&self) -> Taken {
            if let Some(link) = self.link {
                // SAFETY: a live display link (never released).
                unsafe { CVDisplayLinkStop(link as Link) };
            }
            RECORDING_THREAD.store(0, Ordering::Release);
            self.stopped.set(true);
            self.store.stop()
        }
    }

    impl Drop for Recorder {
        fn drop(&mut self) {
            if !self.stopped.get() {
                self.halt();
            }
        }
    }

    /// A stopped recording.
    pub(super) struct Pending {
        store: &'static Store,
        taken: Taken,
        refresh: Duration,
    }

    impl Pending {
        pub(super) fn wait(self, timeout: Duration) -> FrameRecording {
            let deadline = Instant::now() + timeout;
            while !self.store.settled(self.taken) && Instant::now() < deadline {
                thread::sleep(WAIT_STEP);
            }
            self.store.collect(self.taken, self.refresh, &clock_now())
        }
    }

    /// Starts recording on this thread: `layer`'s frames, the ticks of
    /// `display`'s link (none without one: every frame is `without_tick`).
    ///
    /// # Safety
    ///
    /// `layer` is a live `CAMetalLayer`.
    unsafe fn start(layer: Id, display: Option<u32>) -> Result<Recorder, HookError> {
        replace_next_drawable()?;
        // SAFETY: the caller's contract.
        unsafe { replace_commit(layer)? };
        let store: &'static Store = STORE.get_or_init(|| Box::leak(Box::new(Store::new())));
        if !store.claim() {
            return Err(HookError::AlreadyActive);
        }
        let (link, refresh) = match display.map(|display| link_of(display, store)) {
            None => (None, Duration::ZERO),
            Some(Ok((link, refresh))) => (Some(link), refresh),
            Some(Err(error)) => {
                store.release();
                return Err(error);
            }
        };
        // SAFETY: no arguments.
        let this = unsafe { libc::pthread_self() } as usize;
        RECORDING_THREAD.store(this, Ordering::Release);
        store.begin(now_ticks());
        if let Some(link) = link {
            // SAFETY: a live display link.
            unsafe { CVDisplayLinkStart(link) };
        }
        Ok(Recorder {
            store,
            refresh,
            link: link.map(|link| link as usize),
            stopped: Cell::new(false),
        })
    }

    /// The view's `CAMetalLayer` and its display (`NSScreenNumber`).
    ///
    /// # Safety
    ///
    /// The main thread; `view` is a live `NSView`.
    unsafe fn layer_and_display(view: Id) -> Result<(Id, u32), HookError> {
        // SAFETY: the caller's contract; the selectors are AppKit's and
        // Foundation's with the signatures cast here; the autoreleased
        // objects die with the pool.
        unsafe {
            let layer = send0(view, c"layer");
            let class = objc_getClass(CLASS.as_ptr());
            if layer.is_null() || class.is_null() {
                return Err(HookError::NoMetalLayer);
            }
            let is_kind: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            if is_kind(layer, sel_registerName(c"isKindOfClass:".as_ptr()), class) == 0 {
                return Err(HookError::NoMetalLayer);
            }
            let window = send0(view, c"window");
            let screen = if window.is_null() {
                ptr::null_mut()
            } else {
                send0(window, c"screen")
            };
            if screen.is_null() {
                return Err(HookError::NoMetalLayer);
            }
            let pool = objc_autoreleasePoolPush();
            let description = send0(screen, c"deviceDescription");
            let string: unsafe extern "C" fn(Id, Sel, *const c_char) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let key = string(
                objc_getClass(c"NSString".as_ptr()),
                sel_registerName(c"stringWithUTF8String:".as_ptr()),
                c"NSScreenNumber".as_ptr(),
            );
            let number = if description.is_null() || key.is_null() {
                ptr::null_mut()
            } else {
                send1(
                    description,
                    sel_registerName(c"objectForKey:".as_ptr()).cast_mut(),
                    key,
                )
            };
            let display = if number.is_null() {
                None
            } else {
                let value: unsafe extern "C" fn(Id, Sel) -> u32 =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                Some(value(
                    number,
                    sel_registerName(c"unsignedIntValue".as_ptr()),
                ))
            };
            objc_autoreleasePoolPop(pool);
            display
                .map(|display| (layer, display))
                .ok_or(HookError::NoMetalLayer)
        }
    }

    pub(super) fn record(window: &impl HasWindowHandle) -> Result<Recorder, HookError> {
        // SAFETY: no arguments; reads a flag of the calling thread.
        if unsafe { libc::pthread_main_np() } == 0 {
            return Err(HookError::NotMainThread);
        }
        let Ok(handle) = window.window_handle() else {
            return Err(HookError::NoMetalLayer);
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return Err(HookError::NoMetalLayer);
        };
        // SAFETY: the main thread; `ns_view` is a live `NSView` for the
        // handle's lifetime (the raw-window-handle contract, kept by GPUI's
        // `MacWindow`), and its layer lives as long as the view.
        unsafe {
            let (layer, display) = layer_and_display(handle.ns_view.as_ptr())?;
            start(layer, Some(display))
        }
    }

    /// `record` for a layer in no window, on any thread, without a display
    /// link: the offscreen test (「测试」).
    ///
    /// # Safety
    ///
    /// `layer` is a live `CAMetalLayer`.
    #[cfg(test)]
    pub(super) unsafe fn record_layer(layer: Id) -> Result<Recorder, HookError> {
        // SAFETY: the caller's contract.
        unsafe { start(layer, None) }
    }
}

#[cfg(not(all(target_os = "macos", feature = "bench-hooks")))]
mod imp {
    use std::time::Duration;

    use raw_window_handle::HasWindowHandle;

    use super::{FrameRecording, PresentedAt};
    use crate::HookError;

    /// Nothing is replaced and the callback is dropped unused.
    pub(super) fn on_first_presented(
        _fallback: super::Fallback,
        _callback: Box<dyn FnOnce(PresentedAt) + Send>,
    ) -> Result<(), HookError> {
        Err(HookError::Unsupported)
    }

    /// Never made: `record` is `Unsupported` here.
    pub(super) enum Recorder {}

    impl Recorder {
        pub(super) fn frame_started(&self) {
            match *self {}
        }

        pub(super) fn refresh(&self) -> Duration {
            match *self {}
        }

        pub(super) fn stop(&self) -> Pending {
            match *self {}
        }
    }

    /// Never made.
    pub(super) enum Pending {}

    impl Pending {
        pub(super) fn wait(self, _timeout: Duration) -> FrameRecording {
            match self {}
        }
    }

    pub(super) fn record(_window: &impl HasWindowHandle) -> Result<Recorder, HookError> {
        Err(HookError::Unsupported)
    }
}
