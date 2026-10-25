//! The time of the first frame on screen, for the measurement build of
//! `lectern-bench` (「帧的时刻」 in docs/design/platform.md, ADR 0048).
//!
//! Locked GPUI shows no drawable, so the one method the first frame passes
//! through, `-[CAMetalLayer nextDrawable]`, is replaced with the Objective-C
//! runtime's `method_setImplementation`, and the drawable it returns gets an
//! `addPresentedHandler:` block that reads `presentedTime`. All of that is
//! behind the `bench-hooks` feature on macOS; everywhere else the function
//! compiles and returns `HookError::Unsupported`, and no Objective-C runtime
//! function is called. This step has `on_first_presented` only; the
//! per-frame recording (`record`) follows in its own step.

use std::time::SystemTime;

use crate::HookError;

#[cfg(test)]
mod tests;

/// Registers the first-frame callback. The first drawable that is really
/// shown (`presentedTime` greater than 0) has its `presentedTime` converted
/// to the system clock and given to `f`.
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
pub fn on_first_presented(f: impl FnOnce(SystemTime) + Send + 'static) -> Result<(), HookError> {
    imp::on_first_presented(Box::new(f))
}

/// TEMPORARY CI diagnostics (bench step 4; removed before the review):
/// calls of the `nextDrawable` replacement, the null drawables among them,
/// presented-handler calls, the calls among them with `presentedTime` not
/// above 0, and the last `presentedTime` (bits of an `f64`). Zeros without
/// `bench-hooks`.
#[doc(hidden)]
pub fn diag_counts() -> [u64; 5] {
    #[cfg(all(target_os = "macos", feature = "bench-hooks"))]
    {
        use std::sync::atomic::Ordering::Relaxed;
        [
            imp::DIAG_NEXT.load(Relaxed),
            imp::DIAG_NULL.load(Relaxed),
            imp::DIAG_HANDLERS.load(Relaxed),
            imp::DIAG_ZERO.load(Relaxed),
            imp::DIAG_LAST.load(Relaxed),
        ]
    }
    #[cfg(not(all(target_os = "macos", feature = "bench-hooks")))]
    {
        [0; 5]
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
    use std::ffi::{CStr, c_char, c_int, c_ulong, c_void};
    use std::mem;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
    use std::time::SystemTime;

    use super::system_time_of;

    // TEMPORARY CI diagnostics (`super::diag_counts`).
    pub(super) static DIAG_NEXT: AtomicU64 = AtomicU64::new(0);
    pub(super) static DIAG_NULL: AtomicU64 = AtomicU64::new(0);
    pub(super) static DIAG_HANDLERS: AtomicU64 = AtomicU64::new(0);
    pub(super) static DIAG_ZERO: AtomicU64 = AtomicU64::new(0);
    pub(super) static DIAG_LAST: AtomicU64 = AtomicU64::new(0);
    use crate::HookError;
    use crate::hook::check_type_encoding;

    /// An Objective-C object pointer (`id`).
    type Id = *mut c_void;
    type Sel = *const c_void;
    /// A `Method` of the runtime.
    type Method = *mut c_void;
    /// The callback of `on_first_presented`.
    pub(super) type FirstPresented = Box<dyn FnOnce(SystemTime) + Send>;

    /// The class and selector replaced, and the type encoding the method
    /// must have: returns an object, takes `self` and `_cmd`.
    const CLASS: &CStr = c"CAMetalLayer";
    const NEXT_DRAWABLE: &CStr = c"nextDrawable";
    const NEXT_DRAWABLE_ENCODING: &CStr = c"@16@0:8";

    // The Objective-C runtime, part of libSystem's load set.
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
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
    }

    // QuartzCore: `CAMetalLayer`, and the media clock. Linked here, not
    // only through GPUI, so that this crate's test binary (which has no
    // GPUI) loads the class too.
    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn CACurrentMediaTime() -> f64;
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

    /// A block literal (`Block_literal_1`) with one captured value (here
    /// none, `()`). Built on the stack; Metal copies it when it keeps it,
    /// and a copy of a block without helpers is a plain memcpy, which is
    /// what a `Copy` payload wants.
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
    /// The implementation `nextDrawable` had before the replacement.
    static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    /// The selectors the replacement and the block send, registered at
    /// install time: `sel_registerName` takes a lock inside the runtime,
    /// and neither of them may.
    static ADD_PRESENTED_HANDLER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    static PRESENTED_TIME: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

    /// The block's `invoke`: Core Animation passes the block itself and the
    /// drawable.
    type PresentedInvoke = unsafe extern "C" fn(*mut c_void, Id);

    static PRESENTED_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: mem::size_of::<Block<PresentedInvoke, ()>>() as c_ulong,
    };

    /// The body of the presented handler, on a Core Animation thread. Takes
    /// no lock, allocates nothing and does not panic, up to the callback
    /// the caller gave.
    unsafe extern "C" fn presented_invoke(_block: *mut c_void, drawable: Id) {
        let selector = PRESENTED_TIME.load(Ordering::Acquire);
        if selector.is_null() || drawable.is_null() {
            return;
        }
        // SAFETY: `drawable` is the `MTLDrawable` the handler is called
        // with; `presentedTime` takes no argument and returns a
        // `CFTimeInterval` (a double), which is the signature cast here.
        let presented = unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> f64 =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(drawable, selector)
        };
        DIAG_HANDLERS.fetch_add(1, Ordering::Relaxed);
        if presented.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            DIAG_ZERO.fetch_add(1, Ordering::Relaxed);
        }
        DIAG_LAST.store(presented.to_bits(), Ordering::Relaxed);
        deliver(&CALLBACK, presented);
    }

    /// What the presented handler does with the `presentedTime` it read:
    /// nothing for 0 (this drawable was never shown, replaced by a later
    /// one; the callback waits for the next) or any value that is not
    /// greater than 0 (false for NaN); otherwise takes the callback out of
    /// `slot` (an atomic swap: only one thread gets it) and calls it with the
    /// time on the system clock. Takes no lock, allocates nothing, does not
    /// panic, up to the callback.
    pub(super) fn deliver(slot: &AtomicPtr<FirstPresented>, presented: f64) {
        if presented.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
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
        // SAFETY: `callback` came from `Box::into_raw` in
        // `on_first_presented`, and the swap gave it to this call alone.
        // Running the callback consumes the box: the free is the callback's
        // own, part of the one exception to "does not allocate" (「安装的规则」).
        let callback = unsafe { Box::from_raw(callback) };
        callback(system_time_of(presented, media_now, now));
    }

    /// The replacement of `-[CAMetalLayer nextDrawable]`: calls the original,
    /// then, while the first-frame callback is waiting, registers the
    /// presented handler on the drawable it returned. Afterwards it only
    /// reads one pointer. GPUI's two presentation paths both come after
    /// `nextDrawable`, so the handler is registered before the drawable is
    /// shown.
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
        DIAG_NEXT.fetch_add(1, Ordering::Relaxed);
        if drawable.is_null() {
            DIAG_NULL.fetch_add(1, Ordering::Relaxed);
        }
        if !drawable.is_null() && !CALLBACK.load(Ordering::Acquire).is_null() {
            let add = ADD_PRESENTED_HANDLER.load(Ordering::Acquire);
            let block = Block::new(
                presented_invoke as PresentedInvoke,
                &PRESENTED_DESCRIPTOR,
                (),
            );
            // SAFETY: `drawable` is an object that conforms to
            // `MTLDrawable`; `addPresentedHandler:` takes a
            // `void (^)(id<MTLDrawable>)` block (which `block` is laid out
            // as) and returns nothing; Metal copies the block, so it may
            // end here on the stack.
            unsafe {
                let send: unsafe extern "C" fn(Id, Sel, Id) =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                send(drawable, add, block.as_id());
            }
        }
        drawable
    }

    /// Looks the method up and checks its type encoding: the part of an
    /// installation that changes nothing. `Mismatch` when the class or the
    /// method is missing or the encoding differs.
    pub(super) fn find_method(
        class: &CStr,
        selector: &CStr,
        encoding: &CStr,
    ) -> Result<Method, HookError> {
        let mismatch = || HookError::Mismatch {
            what: mismatch_name(class, selector),
        };
        // SAFETY: NUL-terminated C strings; null when the class is not
        // loaded.
        let class_id = unsafe { objc_getClass(class.as_ptr()) };
        if class_id.is_null() {
            return Err(mismatch());
        }
        // SAFETY: `class_id` is a class; the selector is a C string.
        let method =
            unsafe { class_getInstanceMethod(class_id, sel_registerName(selector.as_ptr())) };
        if method.is_null() {
            return Err(mismatch());
        }
        // SAFETY: `method` is live; the runtime gives a NUL-terminated
        // string owned by it, or null.
        let actual = unsafe {
            let actual = method_getTypeEncoding(method);
            (!actual.is_null()).then(|| CStr::from_ptr(actual))
        };
        check_type_encoding(&mismatch_name(class, selector), actual, encoding)?;
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

    pub(super) fn on_first_presented(
        callback: Box<dyn FnOnce(SystemTime) + Send>,
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
        // SAFETY: C strings.
        unsafe {
            ADD_PRESENTED_HANDLER.store(
                sel_registerName(c"addPresentedHandler:".as_ptr()).cast_mut(),
                Ordering::Release,
            );
            PRESENTED_TIME.store(
                sel_registerName(c"presentedTime".as_ptr()).cast_mut(),
                Ordering::Release,
            );
        }
        CALLBACK.store(Box::into_raw(Box::new(callback)), Ordering::Release);

        let ours = next_drawable as unsafe extern "C" fn(Id, Sel) -> Id as *const c_void;
        // SAFETY: `method` is live. The original is stored before the
        // replacement is, so that no call of the replacement finds it null.
        // When the implementation already is ours (replaced through a
        // superclass earlier) there is nothing to replace.
        unsafe {
            let current = method_getImplementation(method);
            if current != ours {
                ORIGINAL.store(current.cast_mut(), Ordering::Release);
                method_setImplementation(method, ours);
            }
        }
        Ok(())
    }
}

#[cfg(not(all(target_os = "macos", feature = "bench-hooks")))]
mod imp {
    use std::time::SystemTime;

    use crate::HookError;

    /// Nothing is replaced and the callback is dropped unused.
    pub(super) fn on_first_presented(
        _callback: Box<dyn FnOnce(SystemTime) + Send>,
    ) -> Result<(), HookError> {
        Err(HookError::Unsupported)
    }
}
