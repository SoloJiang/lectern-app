//! Counters and partition states.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::Dropped;

/// Counters and partition states, from [`crate::Cache::stats`].
#[derive(Clone, Debug)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
    pub decode_failures: u64,
    /// Committed writes (`put`, `remove`, `Batch::commit`).
    pub written: u64,
    /// Dropped writes by reason, one item per reason.
    pub dropped: Vec<(Dropped, u64)>,
    pub evicted_bytes: u64,
    /// Partition files deleted and created again.
    pub rebuilt: u64,
    /// Open connections.
    pub connections: usize,
    pub partitions: Vec<PartitionStats>,
}

impl Stats {
    /// Dropped writes for one reason.
    pub fn dropped(&self, reason: Dropped) -> u64 {
        self.dropped
            .iter()
            .find(|(r, _)| *r == reason)
            .map_or(0, |(_, n)| *n)
    }
}

#[derive(Clone, Debug)]
pub struct PartitionStats {
    pub kind: PartitionKind,
    /// The repository's common directory or the folder; `None` for the app
    /// partition.
    pub path: Option<PathBuf>,
    pub state: PartitionState,
    /// `meta.bytes` as last read or written.
    pub bytes: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum PartitionKind {
    App,
    Repo,
    Folder,
}

impl PartitionKind {
    /// `meta.kind`, and the prefix hashed into a project partition's name.
    pub(crate) fn name(self) -> &'static str {
        match self {
            PartitionKind::App => "app",
            PartitionKind::Repo => "repo",
            PartitionKind::Folder => "folder",
        }
    }

    /// The directory of project partitions under `Config::root`.
    pub(crate) fn dir(self) -> &'static str {
        match self {
            PartitionKind::App => "",
            PartitionKind::Repo => "repo",
            PartitionKind::Folder => "folder",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PartitionState {
    Closed,
    Open,
    /// Writes are dropped until `until` after a full disk.
    WritesSuspended {
        until: SystemTime,
    },
    /// No more IO in this run.
    Disabled {
        reason: DisableReason,
    },
}

/// Why a partition was disabled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DisableReason {
    /// Corrupt a second time in this run.
    Corrupt,
    /// The directory or file cannot be created or written
    /// (`SQLITE_CANTOPEN`, `SQLITE_READONLY`, `SQLITE_PERM`).
    NotWritable,
    /// Three IO errors in a row.
    Io,
}

#[derive(Default)]
pub(crate) struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    decode_failures: AtomicU64,
    written: AtomicU64,
    dropped: [AtomicU64; 6],
    evicted_bytes: AtomicU64,
    rebuilt: AtomicU64,
}

impl Counters {
    pub(crate) fn hit(&self, n: u64) {
        self.hits.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn miss(&self, n: u64) {
        self.misses.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn decode_failure(&self) {
        self.decode_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn written(&self) {
        self.written.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn dropped(&self, reason: Dropped) {
        let index = Dropped::ALL.iter().position(|r| *r == reason).unwrap_or(0);
        self.dropped[index].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn evicted(&self, bytes: u64) {
        self.evicted_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn rebuilt(&self) {
        self.rebuilt.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self, connections: usize, partitions: Vec<PartitionStats>) -> Stats {
        let load = |a: &AtomicU64| a.load(Ordering::Relaxed);
        Stats {
            hits: load(&self.hits),
            misses: load(&self.misses),
            decode_failures: load(&self.decode_failures),
            written: load(&self.written),
            dropped: Dropped::ALL
                .iter()
                .zip(&self.dropped)
                .map(|(reason, n)| (*reason, load(n)))
                .collect(),
            evicted_bytes: load(&self.evicted_bytes),
            rebuilt: load(&self.rebuilt),
            connections,
            partitions,
        }
    }
}
