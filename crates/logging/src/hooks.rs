//! Test hooks (docs/design/logging.md「测试」): every file operation, every
//! encoding and every thread this crate starts goes through here. Outside
//! `cfg(test)` they compile to nothing.

#![cfg_attr(
    not(all(test, feature = "sink")),
    allow(dead_code, reason = "the hooks only record in tests")
)]

/// A file operation is about to run on the current thread.
#[inline(always)]
pub(crate) fn file_op(_op: &'static str) {
    #[cfg(all(test, feature = "sink"))]
    imp::file_op(_op);
}

/// A record is about to be encoded.
#[inline(always)]
pub(crate) fn encoded() {
    #[cfg(all(test, feature = "sink"))]
    imp::ENCODED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A thread is about to be started.
#[inline(always)]
pub(crate) fn spawned() {
    #[cfg(all(test, feature = "sink"))]
    imp::SPAWNED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// The writer thread is about to write a batch: waits while a test stalls it.
#[inline(always)]
pub(crate) fn writer_batch() {
    #[cfg(all(test, feature = "sink"))]
    imp::wait_while_stalled();
}

#[cfg(all(test, feature = "sink"))]
pub(crate) mod imp {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Condvar, Mutex};

    pub(crate) static ENCODED: AtomicU64 = AtomicU64::new(0);
    pub(crate) static SPAWNED: AtomicU64 = AtomicU64::new(0);
    static RECORDING: AtomicBool = AtomicBool::new(false);
    static FILE_OPS: Mutex<Vec<(String, &'static str)>> = Mutex::new(Vec::new());
    static STALLED: Mutex<bool> = Mutex::new(false);
    static RELEASED: Condvar = Condvar::new();

    pub(crate) fn file_op(op: &'static str) {
        if RECORDING.load(Ordering::Acquire) {
            let thread = std::thread::current();
            let name = thread.name().unwrap_or("<unnamed>").to_owned();
            FILE_OPS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((name, op));
        }
    }

    /// Starts recording file operations (clearing earlier ones).
    pub(crate) fn record_file_ops() {
        FILE_OPS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        RECORDING.store(true, Ordering::Release);
    }

    /// The (thread, operation) pairs recorded so far.
    pub(crate) fn file_ops() -> Vec<(String, &'static str)> {
        FILE_OPS.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn stall_writer(stalled: bool) {
        *STALLED.lock().unwrap_or_else(|e| e.into_inner()) = stalled;
        RELEASED.notify_all();
    }

    pub(crate) fn wait_while_stalled() {
        let mut stalled = STALLED.lock().unwrap_or_else(|e| e.into_inner());
        while *stalled {
            stalled = RELEASED.wait(stalled).unwrap_or_else(|e| e.into_inner());
        }
    }

    pub(crate) fn reset() {
        RECORDING.store(false, Ordering::Release);
        FILE_OPS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        stall_writer(false);
    }
}
