//! Panic boundaries: [`catch`] keeps the payload, [`contain`] reduces it to a message.

use std::any::Any;
use std::fmt;
use std::mem::{self, ManuallyDrop};
use std::panic::{self, AssertUnwindSafe};
use std::thread;

use lectern_logging::{PanicText, Value, error, warn};

use crate::diag::Phase;

/// A panic payload captured by [`catch`].
///
/// [`dispose`](Self::dispose) extracts the message and releases the payload.
/// Payloads produced by `panic!` (`&'static str` and `String`) are freed on
/// the calling thread. Any other payload (a value passed to
/// `std::panic::panic_any`) may block or panic in its `Drop`, so each one is
/// dropped on its own reclaim thread and nothing waits for it.
pub struct PanicPayload {
    payload: Option<Box<dyn Any + Send>>,
}

impl PanicPayload {
    /// Returns a readable message and releases the payload.
    pub fn dispose(mut self) -> String {
        match self.payload.take() {
            Some(payload) => release(payload),
            None => String::new(),
        }
    }

    /// The payload as a log value's input: the variant follows the payload's
    /// type (`&'static str`, `String`, anything else), never its content.
    pub(crate) fn text(&self) -> PanicText<'_> {
        let Some(payload) = self.payload.as_deref() else {
            return PanicText::Other;
        };
        if let Some(text) = payload.downcast_ref::<&'static str>() {
            PanicText::Static(text)
        } else if let Some(text) = payload.downcast_ref::<String>() {
            PanicText::Formatted(text)
        } else {
            PanicText::Other
        }
    }

    /// Logs error `plugin failed` with the phase and this panic's message,
    /// under the caller's scope.
    pub(crate) fn report(&self, phase: Phase) {
        error!(
            "plugin failed";
            phase = phase,
            message = Value::panic_message(self.text()),
        );
    }
}

impl Drop for PanicPayload {
    fn drop(&mut self) {
        if let Some(payload) = self.payload.take() {
            release(payload);
        }
    }
}

impl fmt::Debug for PanicPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = self.payload.as_deref().map(message);
        f.debug_struct("PanicPayload")
            .field("message", &message)
            .finish()
    }
}

/// Runs `f` under `catch_unwind` and keeps the payload if it panics.
///
/// Use this when something must be released before the payload (the executor
/// drops a panicked future first); everywhere else use [`contain`].
pub fn catch<T>(f: impl FnOnce() -> T) -> Result<T, PanicPayload> {
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|payload| PanicPayload {
        payload: Some(payload),
    })
}

/// Same as `catch(f).map_err(PanicPayload::dispose)`: on panic the payload is
/// released as described on [`PanicPayload`] and only the message is returned.
pub fn contain<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    catch(f).map_err(PanicPayload::dispose)
}

/// `contain` around plugin code inside the kernel: a panic is logged as
/// `plugin failed` with `phase` (under the caller's `plugin` / `act` scope)
/// before the payload is released.
pub(crate) fn contain_plugin<T>(phase: Phase, f: impl FnOnce() -> T) -> Result<T, String> {
    catch(f).map_err(|payload| {
        payload.report(phase);
        payload.dispose()
    })
}

/// Runs a shutdown callback (`app` code, not a plugin's), logging a panic.
pub(crate) fn run_shutdown_callback(done: impl FnOnce()) {
    if let Err(payload) = catch(done) {
        error!(
            "shutdown callback panicked";
            message = Value::panic_message(payload.text()),
        );
        payload.dispose();
    }
}

fn message(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "panic with a non-string payload".to_owned()
    }
}

fn release(payload: Box<dyn Any + Send>) -> String {
    let text = message(&*payload);
    if payload.is::<&'static str>() || payload.is::<String>() {
        drop(payload);
    } else {
        // If the thread cannot be spawned the closure is dropped here, and
        // `ManuallyDrop` turns that into a leak instead of a drop on this thread.
        let payload = ManuallyDrop::new(payload);
        let spawned = thread::Builder::new()
            .name("kernel-panic-reclaim".to_owned())
            .spawn(move || drop_isolated(ManuallyDrop::into_inner(payload)));
        if let Err(error) = spawned {
            warn!(
                "could not spawn a reclaim thread; leaking a panic payload";
                error = Value::io(&error),
            );
        }
    }
    text
}

fn drop_isolated(payload: Box<dyn Any + Send>) {
    if let Err(second) = panic::catch_unwind(AssertUnwindSafe(move || drop(payload))) {
        mem::forget(second);
    }
}
