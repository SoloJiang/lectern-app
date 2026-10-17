//! `Scope::Tree`: one FSEvents stream per target, on the backend's serial
//! dispatch queue.

use std::ffi::{CStr, CString, c_char, c_void};
use std::os::unix::ffi::OsStringExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

use lectern_fs_watch::{Armed, BackendError, RawBatch};

use super::convert;
use super::ffi::{
    CFArrayCreate, CFRelease, CFStringCreateWithFileSystemRepresentation, ConstFSEventStreamRef,
    DispatchQueue, FSEventStreamContext, FSEventStreamCreate, FSEventStreamEventFlags,
    FSEventStreamEventId, FSEventStreamFlushSync, FSEventStreamInvalidate, FSEventStreamRef,
    FSEventStreamRelease, FSEventStreamSetDispatchQueue, FSEventStreamStart, FSEventStreamStop,
    QOS_CLASS_UTILITY, dispatch_queue_attr_make_with_qos_class, dispatch_queue_create,
    dispatch_release, dispatch_sync_f, kCFTypeArrayCallBacks, kFSEventStreamCreateFlagFileEvents,
    kFSEventStreamCreateFlagNoDefer, kFSEventStreamCreateFlagWatchRoot,
    kFSEventStreamEventIdSinceNow,
};
use super::{Deliver, io_error};

/// The stream latency. Coalescing happens in fs_watch; with `NoDefer` the
/// first event after a quiet period is delivered at once.
pub(crate) const LATENCY: Duration = Duration::from_millis(20);

/// The backend's private serial dispatch queue (QoS utility). Every stream
/// is scheduled on it, so callbacks never run concurrently.
pub(crate) struct Queue(DispatchQueue);

// SAFETY: a dispatch queue is a thread-safe, reference-counted object; all
// libdispatch functions on it may be called from any thread.
unsafe impl Send for Queue {}
// SAFETY: as above; `Queue` only hands the pointer to libdispatch and
// FSEvents.
unsafe impl Sync for Queue {}

impl Queue {
    pub(crate) fn new() -> Result<Queue, BackendError> {
        // SAFETY: a null attribute is `DISPATCH_QUEUE_SERIAL`; the call
        // returns a new attribute (or null for an invalid QoS class).
        let attr = unsafe {
            dispatch_queue_attr_make_with_qos_class(ptr::null_mut(), QOS_CLASS_UTILITY, 0)
        };
        let label = c"lectern.fs-watch.fsevents";
        // SAFETY: `label` is a NUL-terminated string that libdispatch copies;
        // `attr` came from the call above (null means a plain serial queue).
        let queue = unsafe { dispatch_queue_create(label.as_ptr(), attr) };
        if queue.is_null() {
            return Err(BackendError::Other(
                "could not create the FSEvents dispatch queue".to_owned(),
            ));
        }
        Ok(Queue(queue))
    }

    /// Returns once every block queued before this call has run: the queue
    /// is serial, so an empty block queued now runs after them.
    fn fence(&self) {
        extern "C" fn nothing(_: *mut c_void) {}
        // SAFETY: the queue is alive (we hold a reference); `nothing`
        // ignores its context. This is never called on the queue itself
        // (only from `Stream::drop`, which runs on the fs_watch thread), so
        // it cannot deadlock.
        unsafe { dispatch_sync_f(self.0, ptr::null_mut(), nothing) };
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        // SAFETY: we own the reference `dispatch_queue_create` returned, and
        // every stream scheduled on the queue holds an `Arc<Queue>`, so no
        // stream uses it any more.
        unsafe { dispatch_release(self.0) };
    }
}

/// What the callback reads through the stream's `info` pointer.
struct Context {
    /// The canonical root, as FSEvents reports paths.
    root: Vec<u8>,
    deliver: Deliver,
}

/// One started stream. Dropping it stops the stream; once the drop returns
/// the callback no longer runs.
pub(crate) struct Stream {
    stream: FSEventStreamRef,
    queue: Arc<Queue>,
    /// Owned; freed in `drop` after the stream is invalidated and the queue
    /// fenced.
    context: *mut Context,
    started: bool,
}

// SAFETY: FSEvents stream functions (`FlushSync`, `Stop`, `Invalidate`,
// `Release`) may be called from any thread; the callback runs on the dispatch
// queue and only reads `Context`, which is `Send + Sync` (`Deliver` is) and
// is not touched by `Stream` until the drop has fenced the queue.
unsafe impl Send for Stream {}
// SAFETY: `flush` is the only method taking `&self`; FSEventStreamFlushSync
// is thread-safe.
unsafe impl Sync for Stream {}

impl Stream {
    pub(crate) fn start(
        path: &Path,
        queue: Arc<Queue>,
        deliver: Deliver,
    ) -> Result<Stream, BackendError> {
        Stream::start_with(path, queue, deliver, LATENCY)
    }

    /// `start` with another latency, for the tests of `flush`.
    pub(crate) fn start_with(
        path: &Path,
        queue: Arc<Queue>,
        deliver: Deliver,
        latency: Duration,
    ) -> Result<Stream, BackendError> {
        // FSEvents reports paths with symbolic links resolved and in the
        // case stored on disk; `realpath` gives the same spelling.
        let canonical = std::fs::canonicalize(path).map_err(|error| io_error(&error))?;
        let root = canonical.into_os_string().into_vec();
        let c_root = CString::new(root.clone())
            .map_err(|_| BackendError::Other("the path contains a NUL byte".to_owned()))?;

        // SAFETY: `c_root` is NUL-terminated; a null allocator is the default.
        let cf_root =
            unsafe { CFStringCreateWithFileSystemRepresentation(ptr::null(), c_root.as_ptr()) };
        if cf_root.is_null() {
            return Err(BackendError::Other(
                "could not convert the path to a CFString".to_owned(),
            ));
        }
        let values = [cf_root];
        // SAFETY: `values` holds one valid CFString; the CFType callbacks
        // make the array retain it.
        let paths = unsafe {
            CFArrayCreate(
                ptr::null(),
                values.as_ptr(),
                1,
                &raw const kCFTypeArrayCallBacks,
            )
        };
        // SAFETY: we own `cf_root` (Create rule); the array, if made, holds
        // its own reference.
        unsafe { CFRelease(cf_root) };
        if paths.is_null() {
            return Err(BackendError::Other("could not create a CFArray".to_owned()));
        }

        let context = Box::into_raw(Box::new(Context { root, deliver }));
        let mut stream_context = FSEventStreamContext {
            version: 0,
            info: context.cast(),
            retain: None,
            release: None,
            copy_description: None,
        };
        // SAFETY: the callback matches `FSEventStreamCallback`; the context
        // struct is copied by the call, and `info` stays valid until `drop`
        // frees it after the stream is invalidated; `paths` is a CFArray of
        // one CFString.
        let stream = unsafe {
            FSEventStreamCreate(
                ptr::null(),
                callback,
                &mut stream_context,
                paths,
                kFSEventStreamEventIdSinceNow,
                latency.as_secs_f64(),
                kFSEventStreamCreateFlagFileEvents
                    | kFSEventStreamCreateFlagNoDefer
                    | kFSEventStreamCreateFlagWatchRoot,
            )
        };
        // SAFETY: we own `paths` (Create rule); the stream retains what it
        // needs.
        unsafe { CFRelease(paths) };
        if stream.is_null() {
            // SAFETY: no stream refers to `context`; it came from
            // `Box::into_raw` above.
            drop(unsafe { Box::from_raw(context) });
            return Err(BackendError::Other("FSEventStreamCreate failed".to_owned()));
        }
        // From here on, dropping `armed` releases the stream and the
        // context, whether or not it started.
        let mut armed = Stream {
            stream,
            queue,
            context,
            started: false,
        };
        // SAFETY: the stream is valid and not yet scheduled; the queue
        // outlives it (`armed` holds an `Arc`).
        unsafe { FSEventStreamSetDispatchQueue(armed.stream, armed.queue.0) };
        // SAFETY: the stream is valid and scheduled on a queue.
        armed.started = unsafe { FSEventStreamStart(armed.stream) } != 0;
        if !armed.started {
            return Err(BackendError::Other("FSEventStreamStart failed".to_owned()));
        }
        Ok(armed)
    }
}

impl Armed for Stream {
    fn flush(&self) -> Result<(), BackendError> {
        // SAFETY: the stream is valid until drop. FlushSync waits for the
        // callbacks of events already received, which only queue batches, so
        // the wait is bounded; it is never called on the dispatch queue.
        unsafe { FSEventStreamFlushSync(self.stream) };
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if self.started {
            // SAFETY: the stream is valid and started.
            unsafe { FSEventStreamStop(self.stream) };
        }
        // SAFETY: the stream is valid and scheduled on the queue;
        // Invalidate unschedules it.
        unsafe { FSEventStreamInvalidate(self.stream) };
        // A callback already queued, or running, finishes before this
        // returns; none is queued after Invalidate.
        self.queue.fence();
        // SAFETY: the stream is invalidated and no callback is running, so
        // releasing it and freeing the context cannot race with the
        // callback; `context` came from `Box::into_raw` in `start_with`.
        unsafe {
            FSEventStreamRelease(self.stream);
            drop(Box::from_raw(self.context));
        }
    }
}

/// The stream callback, on the dispatch queue. Converts the events and
/// delivers them as one batch; never blocks.
extern "C" fn callback(
    _stream: ConstFSEventStreamRef,
    info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const FSEventStreamEventFlags,
    _event_ids: *const FSEventStreamEventId,
) {
    if num_events == 0 || info.is_null() {
        return;
    }
    // A panic must not unwind into FSEvents; a panicking `deliver` loses
    // this batch only.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `info` is the `Context` the stream was created with; it is
        // freed only after the stream is invalidated and the queue fenced.
        let context = unsafe { &*info.cast::<Context>() };
        // SAFETY: without `UseCFTypes` FSEvents passes `num_events` C string
        // pointers and as many flags; both arrays live for the callback.
        let (paths, flags) = unsafe {
            (
                std::slice::from_raw_parts(event_paths.cast::<*const c_char>(), num_events),
                std::slice::from_raw_parts(event_flags, num_events),
            )
        };
        let mut events = Vec::with_capacity(num_events);
        for (&path, &flags) in paths.iter().zip(flags) {
            if path.is_null() {
                continue;
            }
            // SAFETY: a non-null entry is a NUL-terminated path string.
            let path = unsafe { CStr::from_ptr(path) }.to_bytes();
            events.extend(convert::event(&context.root, path, flags));
        }
        if !events.is_empty() {
            (context.deliver)(RawBatch { events });
        }
    }));
}
