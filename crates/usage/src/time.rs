//! Time sources: the kernel's monotonic `Clock` for timers, and the wall
//! clock for record timestamps and windows. Tests replace both with
//! `testing::ManualTime`.
//!
//! A timer is a callback handed to `Clock::wake_at` as a waker: it runs on
//! the clock's timer thread (`SystemClock` starts one on demand and lets it
//! exit when idle) without an executor task. A task would take the
//! activation's admission when polled and so end the quiescence the pacing
//! timer is waiting out.

use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Wake, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lectern_kernel::{Clock, SystemClock};

#[derive(Clone)]
pub(crate) struct Time {
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) wall: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl Time {
    pub(crate) fn system() -> Time {
        Time {
            clock: Arc::new(SystemClock),
            wall: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            }),
        }
    }

    /// Unix milliseconds now.
    pub(crate) fn now_ms(&self) -> i64 {
        (self.wall)()
    }

    /// Runs `f` once `after` has passed, on the clock's timer thread. `f`
    /// must not block.
    pub(crate) fn after(&self, after: Duration, f: impl FnOnce() + Send + 'static) {
        let at = self.clock.now() + after;
        self.at(at, f);
    }

    /// Runs `f` once the wall clock reaches `at_ms` (measured on the
    /// monotonic clock from now).
    pub(crate) fn at_wall(&self, at_ms: i64, f: impl FnOnce() + Send + 'static) {
        let wait = u64::try_from(at_ms.saturating_sub(self.now_ms())).unwrap_or(0);
        self.after(Duration::from_millis(wait), f);
    }

    fn at(&self, at: Instant, f: impl FnOnce() + Send + 'static) {
        let once: Arc<Once> = Arc::new(Once(Mutex::new(Some(Box::new(f)))));
        self.clock.wake_at(at, Waker::from(once));
    }
}

/// A waker that runs its callback the first time it is woken.
struct Once(Mutex<Option<Box<dyn FnOnce() + Send>>>);

impl Wake for Once {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let f = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(f) = f {
            // The timer thread is shared: a panic must not end it. The
            // payload's text is not logged (ADR 0046).
            if lectern_kernel::contain(f).is_err() {
                lectern_logging::error!("usage timer callback panicked");
            }
        }
    }
}
