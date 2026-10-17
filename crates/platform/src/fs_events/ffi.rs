//! Hand-written bindings for the CoreFoundation, CoreServices (FSEvents) and
//! libdispatch functions the backend uses: fifteen symbols, so no binding
//! crate (docs/design/fs_watch.md「platform 后端」). CoreServices and
//! CoreFoundation are already linked by AppKit; libdispatch is part of
//! libSystem.

#![allow(
    non_snake_case,
    non_upper_case_globals,
    reason = "the C names, as Apple's headers spell them"
)]

use std::ffi::{c_char, c_int, c_void};

pub(super) type CFIndex = isize;
pub(super) type CFTypeRef = *const c_void;
pub(super) type CFAllocatorRef = *const c_void;
pub(super) type CFStringRef = *const c_void;
pub(super) type CFArrayRef = *const c_void;
/// `Boolean` (an `unsigned char`).
pub(super) type Boolean = u8;

pub(super) type FSEventStreamRef = *mut c_void;
pub(super) type ConstFSEventStreamRef = *const c_void;
pub(super) type FSEventStreamEventId = u64;
pub(super) type FSEventStreamEventFlags = u32;
pub(super) type FSEventStreamCreateFlags = u32;
pub(super) type CFTimeInterval = f64;

pub(super) type DispatchQueue = *mut c_void;
pub(super) type DispatchQueueAttr = *mut c_void;
pub(super) type DispatchFunction = extern "C" fn(*mut c_void);

/// Without `kFSEventStreamCreateFlagUseCFTypes`, `event_paths` is a
/// `const char **` of `num_events` C strings.
pub(super) type FSEventStreamCallback = extern "C" fn(
    stream: ConstFSEventStreamRef,
    info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const FSEventStreamEventFlags,
    event_ids: *const FSEventStreamEventId,
);

#[repr(C)]
pub(super) struct FSEventStreamContext {
    pub(super) version: CFIndex,
    pub(super) info: *mut c_void,
    pub(super) retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    pub(super) release: Option<extern "C" fn(*const c_void)>,
    pub(super) copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}

#[repr(C)]
pub(super) struct CFArrayCallBacks {
    version: CFIndex,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

pub(super) const kFSEventStreamEventIdSinceNow: FSEventStreamEventId = 0xFFFF_FFFF_FFFF_FFFF;
pub(super) const kFSEventStreamCreateFlagNoDefer: FSEventStreamCreateFlags = 0x0000_0002;
pub(super) const kFSEventStreamCreateFlagWatchRoot: FSEventStreamCreateFlags = 0x0000_0004;
pub(super) const kFSEventStreamCreateFlagFileEvents: FSEventStreamCreateFlags = 0x0000_0010;

/// `QOS_CLASS_UTILITY` (`sys/qos.h`).
pub(super) const QOS_CLASS_UTILITY: u32 = 0x11;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub(super) static kCFTypeArrayCallBacks: CFArrayCallBacks;

    pub(super) fn CFStringCreateWithFileSystemRepresentation(
        alloc: CFAllocatorRef,
        buffer: *const c_char,
    ) -> CFStringRef;
    pub(super) fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const *const c_void,
        num_values: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;
    pub(super) fn CFRelease(cf: CFTypeRef);
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    pub(super) fn FSEventStreamCreate(
        allocator: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *mut FSEventStreamContext,
        paths_to_watch: CFArrayRef,
        since_when: FSEventStreamEventId,
        latency: CFTimeInterval,
        flags: FSEventStreamCreateFlags,
    ) -> FSEventStreamRef;
    pub(super) fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
    pub(super) fn FSEventStreamStart(stream: FSEventStreamRef) -> Boolean;
    pub(super) fn FSEventStreamFlushSync(stream: FSEventStreamRef);
    pub(super) fn FSEventStreamStop(stream: FSEventStreamRef);
    pub(super) fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    pub(super) fn FSEventStreamRelease(stream: FSEventStreamRef);
}

// libdispatch, in libSystem.
unsafe extern "C" {
    pub(super) fn dispatch_queue_attr_make_with_qos_class(
        attr: DispatchQueueAttr,
        qos_class: u32,
        relative_priority: c_int,
    ) -> DispatchQueueAttr;
    pub(super) fn dispatch_queue_create(
        label: *const c_char,
        attr: DispatchQueueAttr,
    ) -> DispatchQueue;
    pub(super) fn dispatch_sync_f(
        queue: DispatchQueue,
        context: *mut c_void,
        work: DispatchFunction,
    );
    pub(super) fn dispatch_release(object: *mut c_void);
}
