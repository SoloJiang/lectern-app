//! The fs_watch thread's clock: real, or driven by `testing::Driver`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::util::lock;

#[derive(Clone)]
pub(crate) enum Clock {
    Real(Instant),
    /// Elapsed time set by the test. The wall clock is a fixed instant plus
    /// that time, so reflog stamps are reproducible.
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        allow(dead_code, reason = "only testing::manual uses the fake clock")
    )]
    Fake(Arc<Mutex<Duration>>),
}

/// The fake wall clock's zero: 2023-11-14 22:13:20 UTC.
const FAKE_EPOCH: Duration = Duration::from_secs(1_700_000_000);

impl Clock {
    pub(crate) fn real() -> Clock {
        Clock::Real(Instant::now())
    }

    /// Monotonic time since the clock was created.
    pub(crate) fn now(&self) -> Duration {
        match self {
            Clock::Real(start) => start.elapsed(),
            Clock::Fake(now) => *lock(now),
        }
    }

    pub(crate) fn wall(&self) -> SystemTime {
        match self {
            Clock::Real(_) => SystemTime::now(),
            Clock::Fake(now) => SystemTime::UNIX_EPOCH + FAKE_EPOCH + *lock(now),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn advance(&self, by: Duration) {
        if let Clock::Fake(now) = self {
            *lock(now) += by;
        }
    }
}
