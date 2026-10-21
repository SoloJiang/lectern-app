//! Small helpers shared by the modules.

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks, going on after a panic elsewhere: every lock here guards state
/// that stays consistent between statements.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Gives the executor back once: other tasks of the same priority (and
/// cancellation) get a turn, as between the slices of a whole-file parse.
pub(crate) fn yield_now() -> impl std::future::Future<Output = ()> {
    struct Yield(bool);
    impl std::future::Future for Yield {
        type Output = ();
        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<()> {
            if self.0 {
                std::task::Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }
    }
    Yield(false)
}

/// One line on stderr with the crate's prefix.
pub(crate) fn log(args: fmt::Arguments<'_>) {
    eprintln!("lectern-syntax: {args}");
}

/// 64-bit FNV-1a over the concatenation of `parts`, the same function as
/// `lectern-code-intel`'s grammar and query versions. Stable across builds
/// and toolchains: the versions go into cache keys.
pub(crate) fn fnv64(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &byte in *part {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}
