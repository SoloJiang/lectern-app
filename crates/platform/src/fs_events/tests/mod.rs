//! `convert` runs everywhere; the backend tests (`fsevents_*`, `kqueue_*`)
//! use real temporary directories and only build on macOS.
//!
//! FSEvents is asynchronous and may start reporting a moment after a stream
//! starts, so those tests wait with deadlines (`Recorder::wait_until`,
//! flushing between waits) rather than sleeping; only the "nothing after
//! drop" checks keep writing for a fixed window, which a slow machine can
//! only make weaker, never fail.

mod convert;
#[cfg(target_os = "macos")]
mod fsevents;
#[cfg(target_os = "macos")]
mod kqueue;
#[cfg(target_os = "macos")]
mod support;
#[cfg(target_os = "macos")]
mod watcher;
