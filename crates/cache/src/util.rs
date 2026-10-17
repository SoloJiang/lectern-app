//! Lock helpers and logging.

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Locks a mutex, recovering the data if a panicking thread poisoned it.
///
/// No lock is held while caller code (`Entry::decode`, hooks) runs except the
/// session read lock, whose data a panic leaves consistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Writes a diagnostic line to stderr. The cache has no logging dependency;
/// `app` redirects stderr to its log file.
pub(crate) fn log(args: fmt::Arguments<'_>) {
    eprintln!("lectern-cache: {args}");
}
