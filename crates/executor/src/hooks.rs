//! Shutdown hooks: callbacks run when [`Executor::shutdown`] starts.
//!
//! [`Executor::shutdown`]: crate::Executor::shutdown

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use lectern_kernel::contain;

use crate::util::{lock, log};

type Callback = Box<dyn FnOnce() + Send>;

/// The registered callbacks of one executor.
#[derive(Default)]
pub(crate) struct ShutdownHooks {
    table: Mutex<Table>,
}

#[derive(Default)]
struct Table {
    /// Set when `shutdown` starts; later registrations run at once.
    started: bool,
    next_id: u64,
    /// Keyed by registration order, so callbacks run in that order.
    callbacks: BTreeMap<u64, Callback>,
}

impl ShutdownHooks {
    /// Registers `f`, or runs it on this thread when shutdown has already
    /// started.
    pub(crate) fn register(self: &Arc<Self>, f: Callback) -> ShutdownHook {
        let mut table = lock(&self.table);
        if table.started {
            drop(table);
            run(f);
            return ShutdownHook { slot: None };
        }
        let id = table.next_id;
        table.next_id += 1;
        table.callbacks.insert(id, f);
        ShutdownHook {
            slot: Some((Arc::downgrade(self), id)),
        }
    }

    /// Marks shutdown as started and runs every registered callback on this
    /// thread, in registration order. Only the first call finds any.
    pub(crate) fn run_all(&self) {
        let callbacks = {
            let mut table = lock(&self.table);
            table.started = true;
            std::mem::take(&mut table.callbacks)
        };
        for f in callbacks.into_values() {
            run(f);
        }
    }

    fn unregister(&self, id: u64) {
        let removed = lock(&self.table).callbacks.remove(&id);
        if let Some(f) = removed
            && let Err(message) = contain(move || drop(f))
        {
            log(format_args!(
                "dropping an unregistered shutdown hook panicked: {message}"
            ));
        }
    }
}

/// Runs a callback under `contain`; a panic is logged and does not reach the
/// caller.
fn run(f: Callback) {
    if let Err(message) = contain(f) {
        log(format_args!("shutdown hook panicked: {message}"));
    }
}

/// A callback registered with [`Executor::on_shutdown`]. Dropping it
/// unregisters the callback: it is dropped without running, on the dropping
/// thread. A callback `shutdown` has already taken runs anyway; the drop does
/// not wait for it.
///
/// [`Executor::on_shutdown`]: crate::Executor::on_shutdown
#[must_use = "dropping the hook unregisters the callback"]
pub struct ShutdownHook {
    /// None when the callback already ran at registration.
    slot: Option<(Weak<ShutdownHooks>, u64)>,
}

impl Drop for ShutdownHook {
    fn drop(&mut self) {
        if let Some((hooks, id)) = self.slot.take()
            && let Some(hooks) = hooks.upgrade()
        {
            hooks.unregister(id);
        }
    }
}

impl fmt::Debug for ShutdownHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShutdownHook")
            .field("registered", &self.slot.is_some())
            .finish()
    }
}
