//! Rate limits of third-party records (docs/design/logging.md「限流」): a
//! fixed table of 256 entries keyed by (target, file, line), linear probing
//! of at most 8, one shared overflow entry, and a total of 50 warn and info
//! records per window. Allocated once (4 KiB); it never grows.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use log::Level;

use crate::callsite::{PER_WINDOW, SUPPRESSED, take};

const SLOTS: usize = 256;
const PROBES: usize = 8;
/// Third-party warn and info records per window, all call sites together.
pub(crate) const TOTAL_PER_WINDOW: u32 = 50;

#[derive(Default)]
struct Entry {
    /// The key's hash, 0 for a free slot.
    key: AtomicU32,
    /// Left out since the last record written for this key.
    suppressed: AtomicU32,
    /// `window << 32 | count`.
    window: AtomicU64,
}

pub(crate) struct Table {
    /// `SLOTS` slots and the overflow entry.
    entries: Box<[Entry]>,
    total: AtomicU64,
}

/// Whether a record is written, and how many from its key were left out
/// before it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Write { n: u64 },
    Limited,
}

impl Table {
    pub(crate) fn new() -> Table {
        Table {
            entries: (0..=SLOTS).map(|_| Entry::default()).collect(),
            total: AtomicU64::new(0),
        }
    }

    /// Counts one error, warn or info record of `key` in `window`.
    pub(crate) fn check(&self, key: u32, level: Level, window: u64) -> Decision {
        let entry = self.entry(key);
        let written = take(&entry.window, window, PER_WINDOW)
            && (level == Level::Error || take(&self.total, window, TOTAL_PER_WINDOW));
        if written {
            let n = match entry.suppressed.load(Ordering::Relaxed) {
                0 => 0,
                _ => entry.suppressed.swap(0, Ordering::Relaxed),
            };
            Decision::Write { n: u64::from(n) }
        } else {
            entry.suppressed.fetch_add(1, Ordering::Relaxed);
            SUPPRESSED.fetch_add(1, Ordering::Relaxed);
            Decision::Limited
        }
    }

    fn entry(&self, key: u32) -> &Entry {
        let start = key as usize % SLOTS;
        for i in 0..PROBES {
            let entry = &self.entries[(start + i) % SLOTS];
            let stored = entry.key.load(Ordering::Relaxed);
            if stored == key {
                return entry;
            }
            if stored == 0 {
                match entry
                    .key
                    .compare_exchange(0, key, Ordering::Relaxed, Ordering::Relaxed)
                {
                    Ok(_) => return entry,
                    Err(now) if now == key => return entry,
                    Err(_) => {}
                }
            }
        }
        &self.entries[SLOTS]
    }

    /// Slots in use, for tests.
    #[cfg(all(test, feature = "sink"))]
    pub(crate) fn used(&self) -> usize {
        self.entries[..SLOTS]
            .iter()
            .filter(|e| e.key.load(Ordering::Relaxed) != 0)
            .count()
    }

    #[cfg(all(test, feature = "sink"))]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The key of a third-party call site: the addresses and lengths of its
/// target and file (string literals at a call site) and its line. Hashing
/// addresses instead of contents keeps a limited call at a few
/// nanoseconds; a dynamically built target only ever lands in more slots,
/// and the overflow entry bounds those.
pub(crate) fn key(target: &str, file: Option<&str>, line: Option<u32>) -> u32 {
    let mut h: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut mix = |v: u64| {
        h = (h ^ v).wrapping_mul(0x0100_0000_01b3).rotate_left(29);
    };
    mix(target.as_ptr() as u64);
    mix(target.len() as u64);
    if let Some(file) = file {
        mix(file.as_ptr() as u64);
        mix(file.len() as u64);
    }
    mix(u64::from(line.unwrap_or(0)));
    let h = (h ^ (h >> 32)) as u32;
    if h == 0 { 1 } else { h }
}
