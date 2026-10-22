//! Rate limits of third-party records (docs/design/logging.md「限流」): a
//! fixed table of 256 entries keyed by (target, file, line), linear probing
//! of at most 8, one shared overflow entry, and a total of 50 warn and info
//! records per window. Allocated once (about 6 KiB); it never grows.
//!
//! An entry also caches the filter's level for its target, tagged with the
//! filter's generation like a Lectern call site's, so that a limited call
//! needs neither the target's name nor the filter. A limited call adds to
//! its entry's count only; the count joins the process total
//! ([`SUPPRESSED`]) when the entry's next record is written, and
//! [`Table::pending`] gives what has not joined yet.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use log::{Level, LevelFilter};

use crate::callsite::{PER_WINDOW, SUPPRESSED, level_filter, take};

const SLOTS: usize = 256;
const PROBES: usize = 8;
/// Third-party warn and info records per window, all call sites together.
pub(crate) const TOTAL_PER_WINDOW: u32 = 50;

#[derive(Default)]
pub(crate) struct Entry {
    /// The key's hash, 0 for a free slot.
    key: AtomicU32,
    /// Left out since the last record written for this key.
    suppressed: AtomicU32,
    /// `window << 32 | count`.
    window: AtomicU64,
    /// `generation << 8 | LevelFilter` of the filter's level for the key's
    /// target; 0 when unknown. Never set on the overflow entry.
    level: AtomicU64,
}

impl Entry {
    /// The cached level, when it is for this filter generation.
    pub(crate) fn level(&self, generation: u64) -> Option<LevelFilter> {
        let cached = self.level.load(Ordering::Relaxed);
        (generation != 0 && cached >> 8 == generation).then(|| level_filter(cached as u8))
    }
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
    #[cfg(all(test, feature = "sink"))]
    pub(crate) fn check(&self, key: u32, level: Level, window: u64) -> Decision {
        self.check_entry(self.entry(key).0, level, window)
    }

    /// Counts one error, warn or info record of `entry` in `window`.
    pub(crate) fn check_entry(&self, entry: &Entry, level: Level, window: u64) -> Decision {
        let written = take(&entry.window, window, PER_WINDOW)
            && (level == Level::Error || take(&self.total, window, TOTAL_PER_WINDOW));
        if written {
            let n = match entry.suppressed.load(Ordering::Relaxed) {
                0 => 0,
                _ => u64::from(entry.suppressed.swap(0, Ordering::Relaxed)),
            };
            if n > 0 {
                SUPPRESSED.fetch_add(n, Ordering::Relaxed);
            }
            Decision::Write { n }
        } else {
            entry.suppressed.fetch_add(1, Ordering::Relaxed);
            Decision::Limited
        }
    }

    /// The slot holding `key`, without taking a free one.
    pub(crate) fn find(&self, key: u32) -> Option<&Entry> {
        let start = key as usize % SLOTS;
        for i in 0..PROBES {
            let entry = &self.entries[(start + i) % SLOTS];
            match entry.key.load(Ordering::Relaxed) {
                k if k == key => return Some(entry),
                0 => return None,
                _ => {}
            }
        }
        None
    }

    /// The slot for `key`, taking a free one; the overflow entry (`true`)
    /// when the probes find none.
    pub(crate) fn entry(&self, key: u32) -> (&Entry, bool) {
        let start = key as usize % SLOTS;
        for i in 0..PROBES {
            let entry = &self.entries[(start + i) % SLOTS];
            let stored = entry.key.load(Ordering::Relaxed);
            if stored == key {
                return (entry, false);
            }
            if stored == 0 {
                match entry
                    .key
                    .compare_exchange(0, key, Ordering::Relaxed, Ordering::Relaxed)
                {
                    Ok(_) => return (entry, false),
                    Err(now) if now == key => return (entry, false),
                    Err(_) => {}
                }
            }
        }
        (&self.entries[SLOTS], true)
    }

    /// Caches the filter's level for a slot's target (not the overflow
    /// entry, which several targets share).
    pub(crate) fn cache_level(entry: &Entry, overflow: bool, generation: u64, level: LevelFilter) {
        if !overflow {
            entry
                .level
                .store(generation << 8 | level as u64, Ordering::Relaxed);
        }
    }

    /// Records left out that have not joined [`SUPPRESSED`] yet.
    #[cfg(feature = "sink")]
    pub(crate) fn pending(&self) -> u64 {
        self.entries
            .iter()
            .map(|e| u64::from(e.suppressed.load(Ordering::Relaxed)))
            .sum()
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
