//! Test support (`test-support` feature): a clock advanced by hand and fault
//! injection.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::clock::Clock;
use crate::util::lock;
use crate::{Cache, Config, Partition};

/// A clock that moves only when the test advances it. Clones share the time.
#[derive(Clone)]
pub struct ManualClock {
    ms: Arc<AtomicU64>,
}

impl ManualClock {
    pub fn new(start: SystemTime) -> ManualClock {
        let ms = start
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        ManualClock {
            ms: Arc::new(AtomicU64::new(ms)),
        }
    }

    pub fn advance(&self, by: Duration) {
        self.ms.fetch_add(by.as_millis() as u64, Ordering::Relaxed);
    }

    pub fn now(&self) -> SystemTime {
        crate::clock::to_system(self.now_ms())
    }

    pub(crate) fn now_ms(&self) -> u64 {
        self.ms.load(Ordering::Relaxed)
    }
}

impl Cache {
    /// A cache whose access times, write pauses and `meta.opened` follow
    /// `clock`.
    pub fn with_clock(config: Config, clock: ManualClock) -> Cache {
        Cache::with(config, Clock::manual(clock))
    }

    /// Turns the debug-build panic on `Config::ui_thread` off, to check the
    /// release behavior.
    pub fn set_ui_thread_panics(&self, panics: bool) {
        self.inner
            .shared
            .ui_thread_panics
            .store(panics, Ordering::Relaxed);
    }
}

impl Partition {
    /// The partition's `.sqlite` file (its `-wal` and `-shm` sit next to it).
    pub fn file(&self) -> &Path {
        &self.slot.file
    }

    /// Sets `PRAGMA max_page_count` on the write connection before every
    /// following write transaction, to make `SQLITE_FULL`; `None` lifts the
    /// limit. SQLite never lowers it below the current size.
    pub fn set_max_page_count(&self, pages: Option<u64>) {
        lock(&self.slot.hooks).max_page_count = Some(pages);
    }

    /// Runs `hook` inside every following write transaction, after the
    /// writes and before the commit, with the write lock held.
    pub fn set_write_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        lock(&self.slot.hooks).in_write = hook;
    }
}
