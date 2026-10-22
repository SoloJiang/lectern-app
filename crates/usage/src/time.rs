//! Time sources: the kernel's monotonic `Clock` for timers, and the wall
//! clock for record timestamps and windows. Tests replace both with
//! `testing::ManualTime`.
//!
//! A timer is a callback handed to `Clock::wake_at` as a waker: it runs on
//! the clock's timer thread (`SystemClock` starts one on demand and lets it
//! exit when idle), so this crate starts no thread of its own, and tests
//! drive the same code with `ManualClock`. The callback only changes state
//! or spawns a task.
//!
//! The monotonic clock stops while the Mac sleeps and the wall clock can be
//! set: a wall-clock deadline waits at most [`WALL_CHECK`] at a time and
//! checks the wall clock when it wakes, so it fires at most that late.

use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Wake, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lectern_kernel::{Clock, SystemClock};

/// The longest a wall-clock deadline waits before it checks the wall clock
/// again.
pub(crate) const WALL_CHECK: Duration = Duration::from_secs(15 * 60);

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

    /// Runs `f` once the wall clock reaches `at_ms`: waits on the monotonic
    /// clock for the time left, at most [`WALL_CHECK`], then looks at the
    /// wall clock again.
    pub(crate) fn at_wall(&self, at_ms: i64, f: impl FnOnce() + Send + 'static) {
        self.at_wall_boxed(at_ms, Box::new(f));
    }

    fn at_wall_boxed(&self, at_ms: i64, f: Box<dyn FnOnce() + Send>) {
        let left = u64::try_from(at_ms.saturating_sub(self.now_ms())).unwrap_or(0);
        let wait = Duration::from_millis(left).min(WALL_CHECK);
        let time = self.clone();
        self.after(wait, move || {
            if time.now_ms() >= at_ms {
                f();
            } else {
                time.at_wall_boxed(at_ms, f);
            }
        });
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
