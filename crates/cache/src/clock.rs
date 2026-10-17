//! Wall clock, replaceable in tests.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(any(test, feature = "test-support"))]
use crate::testing::ManualClock;

/// Milliseconds since the Unix epoch: entry access times, the write pause
/// after a full disk, `meta.opened`.
pub(crate) struct Clock {
    #[cfg(any(test, feature = "test-support"))]
    manual: Option<ManualClock>,
}

impl Clock {
    pub(crate) fn system() -> Clock {
        Clock {
            #[cfg(any(test, feature = "test-support"))]
            manual: None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn manual(clock: ManualClock) -> Clock {
        Clock {
            manual: Some(clock),
        }
    }

    pub(crate) fn now_ms(&self) -> u64 {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(manual) = &self.manual {
            return manual.now_ms();
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
}

/// The wall-clock time of a clock reading.
pub(crate) fn to_system(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}
