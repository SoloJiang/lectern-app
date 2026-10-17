//! Lock helpers and logging shared by the kernel modules.

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Locks a mutex, recovering the data if a panicking thread poisoned it.
///
/// The kernel never holds a lock while plugin code runs inside
/// `catch_unwind`, so poisoned data is still consistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Writes a diagnostic line to stderr. The kernel has no logging dependency;
/// `app` redirects stderr to its log file.
pub(crate) fn log(args: fmt::Arguments<'_>) {
    eprintln!("lectern-kernel: {args}");
}
