//! Content-addressed SQLite cache keyed by git blob OID.
//!
//! Derived data (hunks, indexes, summaries) that can always be recomputed is
//! stored by the identity of its input in per-repository and app-wide SQLite
//! files under the app cache directory. Every blocking method runs on the
//! caller's thread; this crate starts no thread and submits no task. Failures
//! never reach the caller: a failed read is a miss, a failed write returns
//! [`Written::Dropped`]. The design is in `docs/design/cache.md`.

mod clock;
mod codec;
mod db;
mod key;
mod pool;
mod slot;
mod stats;
mod sweep;
mod util;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};

use sha2::{Digest, Sha256};

use crate::clock::Clock;
use crate::db::Op;
use crate::pool::Pool;
use crate::slot::Slot;
use crate::stats::Counters;
use crate::util::lock;

pub use crate::key::Key;
pub use crate::stats::{DisableReason, PartitionKind, PartitionState, PartitionStats, Stats};

/// Largest encoded key.
pub const MAX_KEY: usize = 512;
/// Largest encoded (and compressed) value.
pub const MAX_VALUE: usize = 8 << 20;
/// Largest batch, keys plus values.
pub const MAX_BATCH: u64 = 16 << 20;

/// The process's one cache. `Clone + Send + Sync`; `app` creates it after the
/// startup barrier is released and hands it to the crates that need it.
///
/// Dropping the last clone closes every connection (a WAL checkpoint, so it
/// happens on the thread that drops it, at exit). Handles that outlive it
/// read nothing and write nothing.
#[derive(Clone)]
pub struct Cache {
    inner: Arc<Inner>,
}

/// Cache settings.
pub struct Config {
    /// The directory this crate owns: `app` passes `cache/` under the app
    /// cache directory from `platform`. Created when the first partition
    /// opens.
    pub root: PathBuf,
    pub budgets: Budgets,
    /// The UI thread. When set, any blocking method called on it panics in
    /// debug builds; release builds do no IO there, reads return `None` and
    /// writes return `Written::Dropped(Dropped::WrongThread)`.
    pub ui_thread: Option<ThreadId>,
}

/// Byte budgets and connection limits.
#[derive(Clone, Debug)]
pub struct Budgets {
    /// Byte budget of the app partition. Default 256 MiB.
    pub app: u64,
    /// Byte budget of each repository or folder partition. Default 512 MiB.
    pub project: u64,
    /// All partition files under `root` (`-wal` included); beyond it the
    /// least recently opened project partitions are deleted. Default 2 GiB.
    pub total: u64,
    /// Project partitions open at once. Default 2; the app partition is not
    /// counted.
    pub open_projects: usize,
    /// Page cache of each connection. Default 1 MiB; at most
    /// 2 × (1 + `open_projects`) connections, 6 MiB by default (ADR 0027).
    pub page_cache: u64,
}

impl Default for Budgets {
    fn default() -> Self {
        Budgets {
            app: 256 << 20,
            project: 512 << 20,
            total: 2 << 30,
            open_projects: 2,
            page_cache: 1 << 20,
        }
    }
}

/// State shared by the cache and every partition handle.
pub(crate) struct Shared {
    pub(crate) root: PathBuf,
    pub(crate) budgets: Budgets,
    ui_thread: Option<ThreadId>,
    pub(crate) clock: Clock,
    pub(crate) counters: Counters,
    /// Open connections, across partitions.
    pub(crate) connections: AtomicUsize,
    /// Set when the last `Cache` clone is dropped.
    closed: AtomicBool,
    /// Ever-increasing number of the next opened session, across partitions.
    pub(crate) sessions: AtomicU64,
    /// Ever-increasing count of calls, for `Slot::last_used`.
    pub(crate) ticks: AtomicU64,
    /// The open project partitions.
    pub(crate) pool: Pool,
    /// Whether this run has swept the directory.
    swept: Mutex<bool>,
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) ui_thread_panics: AtomicBool,
}

impl Shared {
    /// Whether a blocking call may proceed on this thread. Panics on the UI
    /// thread in debug builds.
    pub(crate) fn may_block(&self) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let Some(ui) = self.ui_thread else {
            return true;
        };
        if thread::current().id() != ui {
            return true;
        }
        if cfg!(debug_assertions) && self.ui_thread_panics() {
            panic!("lectern-cache: blocking call on the UI thread");
        }
        false
    }

    /// Why a write that may not block is dropped.
    pub(crate) fn blocked_reason(&self) -> Dropped {
        if self.closed.load(Ordering::Acquire) {
            Dropped::Unavailable
        } else {
            Dropped::WrongThread
        }
    }

    /// Sweeps the directory on the first open of the run, before that
    /// partition's file is opened. Later opens wait for it, so the sweep
    /// never deletes a file being opened.
    pub(crate) fn sweep_once(&self) {
        let mut swept = lock(&self.swept);
        if !*swept {
            *swept = true;
            let now = clock::to_system(self.clock.now_ms());
            sweep::sweep(&self.root, now, self.budgets.total, &self.pool.files());
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn ui_thread_panics(&self) -> bool {
        self.ui_thread_panics.load(Ordering::Relaxed)
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn ui_thread_panics(&self) -> bool {
        true
    }
}

struct Inner {
    shared: Arc<Shared>,
    app: Arc<Slot>,
    projects: Mutex<HashMap<(PartitionKind, PathBuf), Arc<Slot>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.app.close(false);
        for slot in lock(&self.projects).values() {
            slot.close(false);
        }
    }
}

impl Cache {
    /// Allocates the structure. No IO, no directory; any thread.
    pub fn new(config: Config) -> Cache {
        Cache::with(config, Clock::system())
    }

    pub(crate) fn with(config: Config, clock: Clock) -> Cache {
        let shared = Arc::new(Shared {
            root: config.root,
            budgets: config.budgets,
            ui_thread: config.ui_thread,
            clock,
            counters: Counters::default(),
            connections: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            sessions: AtomicU64::new(1),
            ticks: AtomicU64::new(1),
            pool: Pool::default(),
            swept: Mutex::new(false),
            #[cfg(any(test, feature = "test-support"))]
            ui_thread_panics: AtomicBool::new(true),
        });
        let app = Slot::new(
            shared.clone(),
            PartitionKind::App,
            None,
            shared.root.join("app.sqlite"),
        );
        Cache {
            inner: Arc::new(Inner {
                shared,
                app,
                projects: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The app partition. No IO; the database opens on the first read or
    /// write.
    pub fn app(&self) -> Partition {
        Partition {
            slot: self.inner.app.clone(),
        }
    }

    /// The partition of a repository. `common_dir` is the absolute canonical
    /// path of its git common directory (`git::Location::common_dir`); all
    /// worktrees of the repository pass the same value and get the same
    /// partition. The path is used as given. No IO.
    pub fn repo(&self, common_dir: &Path) -> Partition {
        self.project(PartitionKind::Repo, common_dir)
    }

    /// The partition of a folder project (not a git repository). `root` is
    /// the canonical path of the opened directory. No IO.
    pub fn folder(&self, root: &Path) -> Partition {
        self.project(PartitionKind::Folder, root)
    }

    fn project(&self, kind: PartitionKind, path: &Path) -> Partition {
        let mut projects = lock(&self.inner.projects);
        let slot = projects
            .entry((kind, path.to_path_buf()))
            .or_insert_with(|| {
                let shared = &self.inner.shared;
                let file = shared
                    .root
                    .join(kind.dir())
                    .join(format!("{}.sqlite", file_stem(kind, path)));
                Slot::new(shared.clone(), kind, Some(path.to_path_buf()), file)
            })
            .clone();
        Partition { slot }
    }

    /// Counters and partition states. Reads memory only; never blocks on IO.
    pub fn stats(&self) -> Stats {
        let shared = &self.inner.shared;
        let mut partitions = vec![self.inner.app.stats()];
        let mut projects: Vec<_> = lock(&self.inner.projects).values().cloned().collect();
        projects.sort_by(|a, b| (a.kind(), a.path()).cmp(&(b.kind(), b.path())));
        partitions.extend(projects.iter().map(|slot| slot.stats()));
        shared
            .counters
            .snapshot(shared.connections.load(Ordering::Relaxed), partitions)
    }

    /// Writes pending access times back and closes every connection.
    /// Blocking; background threads only. Optional: `app` may call it after
    /// the kernel's `shutdown`; dropping the cache is just as safe and only
    /// loses those access times. Later reads and writes open the partitions
    /// again.
    pub fn close(&self) {
        if !self.inner.shared.may_block() {
            return;
        }
        self.inner.app.close(true);
        let projects: Vec<_> = lock(&self.inner.projects).values().cloned().collect();
        for slot in projects {
            slot.close(true);
        }
    }
}

/// `SHA-256(kind + "\0" + path bytes)`, first 16 bytes in hex.
fn file_stem(kind: PartitionKind, path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.name().as_bytes());
    hasher.update([0]);
    hasher.update(path.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let mut stem = String::with_capacity(32);
    for byte in &digest[..16] {
        stem.push_str(&format!("{byte:02x}"));
    }
    stem
}

/// One partition. `Clone + Send + Sync`; holds no connection, so dropping it
/// does no IO on any thread.
#[derive(Clone)]
pub struct Partition {
    slot: Arc<Slot>,
}

impl Partition {
    /// Reads one entry. Blocking (a query; the first call opens the
    /// database); executor tasks only. `None` when absent, written by another
    /// namespace version, undecodable or the partition is unavailable: the
    /// caller recomputes either way. Undecodable entries are deleted by the
    /// next write transaction.
    pub fn get<E: Entry>(&self, key: &Key) -> Option<E> {
        self.slot
            .read::<E>(std::slice::from_ref(key))
            .pop()
            .flatten()
    }

    /// Reads several entries in one read transaction; the result matches
    /// `keys` one to one. Blocking; executor tasks only. For reads of dozens
    /// of entries at once (all hunks of a change set).
    pub fn get_many<E: Entry>(&self, keys: &[Key]) -> Vec<Option<E>> {
        self.slot.read::<E>(keys)
    }

    /// Writes one entry, replacing an entry with the same key. Blocking (one
    /// write transaction); executor tasks only. Returns `Dropped` instead of
    /// an error when the write does not happen; callers usually ignore it.
    pub fn put<E: Entry>(&self, key: &Key, value: &E) -> Written {
        let shared = &self.slot.shared;
        if !shared.may_block() {
            return self.slot.dropped(shared.blocked_reason());
        }
        let (codec, bytes) = codec::encode(value);
        if key.len() > MAX_KEY || bytes.len() > MAX_VALUE {
            return self.slot.dropped(Dropped::TooLarge);
        }
        self.slot.write(&[Op {
            namespace: E::NAMESPACE,
            key: key.as_bytes(),
            value: Some((codec, &bytes)),
        }])
    }

    /// Deletes one entry. Blocking; executor tasks only.
    pub fn remove<E: Entry>(&self, key: &Key) -> Written {
        let shared = &self.slot.shared;
        if !shared.may_block() {
            return self.slot.dropped(shared.blocked_reason());
        }
        if key.len() > MAX_KEY {
            return self.slot.dropped(Dropped::TooLarge);
        }
        self.slot.write(&[Op {
            namespace: E::NAMESPACE,
            key: key.as_bytes(),
            value: None,
        }])
    }

    /// Starts a batch of writes. No IO.
    pub fn batch(&self) -> Batch {
        Batch {
            partition: self.clone(),
            ops: Vec::new(),
            bytes: 0,
            too_large: false,
        }
    }
}

/// Writes that `commit` applies in one transaction, all or none. `Send`: a
/// task can build it up and commit it later.
pub struct Batch {
    partition: Partition,
    ops: Vec<BatchOp>,
    bytes: u64,
    too_large: bool,
}

struct BatchOp {
    namespace: Namespace,
    key: Key,
    value: Option<(u8, Vec<u8>)>,
}

impl Batch {
    /// Encodes and records a write. No IO. The last write of a key in a batch
    /// wins.
    pub fn put<E: Entry>(&mut self, key: &Key, value: &E) -> &mut Batch {
        let (codec, bytes) = codec::encode(value);
        self.too_large |= key.len() > MAX_KEY || bytes.len() > MAX_VALUE;
        self.bytes += (key.len() + bytes.len()) as u64;
        self.ops.push(BatchOp {
            namespace: E::NAMESPACE,
            key: key.clone(),
            value: Some((codec, bytes)),
        });
        self
    }

    /// Records a delete. No IO.
    pub fn remove<E: Entry>(&mut self, key: &Key) -> &mut Batch {
        self.too_large |= key.len() > MAX_KEY;
        self.bytes += key.len() as u64;
        self.ops.push(BatchOp {
            namespace: E::NAMESPACE,
            key: key.clone(),
            value: None,
        });
        self
    }

    /// Encoded bytes so far (keys plus values); callers split batches so they
    /// stay within `MAX_BATCH`.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Commits in one write transaction. Blocking; executor tasks only. When
    /// any write fails (an entry over `MAX_VALUE` or the batch over
    /// `MAX_BATCH` included), nothing is written and `Dropped` is returned.
    pub fn commit(self) -> Written {
        let slot = &self.partition.slot;
        let shared = &slot.shared;
        if !shared.may_block() {
            return slot.dropped(shared.blocked_reason());
        }
        if self.too_large || self.bytes > MAX_BATCH {
            return slot.dropped(Dropped::TooLarge);
        }
        if self.ops.is_empty() {
            return Written::Committed;
        }
        let ops: Vec<Op<'_>> = self
            .ops
            .iter()
            .map(|op| Op {
                namespace: op.namespace,
                key: op.key.as_bytes(),
                value: op.value.as_ref().map(|(codec, bytes)| (*codec, &bytes[..])),
            })
            .collect();
        slot.write(&ops)
    }
}

/// The outcome of a write.
#[must_use]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Written {
    Committed,
    Dropped(Dropped),
}

/// Why a write did not happen.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Dropped {
    /// A key over `MAX_KEY`, a value over `MAX_VALUE`, or a batch over
    /// `MAX_BATCH`.
    TooLarge,
    /// The disk is full (`SQLITE_FULL`). Writes in the next 60 seconds are
    /// dropped without trying.
    DiskFull,
    /// Another process held the write lock for over 100 ms (`SQLITE_BUSY`).
    Busy,
    /// The partition is disabled (repeated corruption, unwritable directory).
    Unavailable,
    /// Another IO error.
    Io,
    /// Called on `Config::ui_thread` (release builds only).
    WrongThread,
}

impl Dropped {
    pub(crate) const ALL: [Dropped; 6] = [
        Dropped::TooLarge,
        Dropped::DiskFull,
        Dropped::Busy,
        Dropped::Unavailable,
        Dropped::Io,
        Dropped::WrongThread,
    ];
}

/// A kind of cached data. Users implement it for each type they cache.
pub trait Entry: Sized {
    const NAMESPACE: Namespace;
    /// Encodes into bytes, on the thread calling `put` / `Batch::put`. No IO.
    fn encode(&self, out: &mut Vec<u8>);
    /// Decodes. `None` is a miss, and the entry is deleted. Runs on the
    /// thread calling `get`.
    fn decode(bytes: &[u8]) -> Option<Self>;
}

/// The namespace of an [`Entry`] type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Namespace {
    /// Globally unique, `<crate>.<data>`, registered in the design doc's
    /// namespace table. ASCII lowercase letters, digits, `.` and `_` only.
    pub name: &'static str,
    /// Bumped when the producing algorithm or the encoding changes. Entries
    /// of other versions are invisible; old ones are deleted gradually.
    pub version: u32,
    pub compression: Compression,
}

impl Namespace {
    /// Same name and version; compression is not part of the identity.
    pub(crate) fn same_version(&self, other: &Namespace) -> bool {
        self.name == other.name && self.version == other.version
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_')
    }
}

/// How a namespace stores its values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compression {
    None,
    /// Encoded values of 4 KiB or more are compressed with zstd (`ruzstd`,
    /// `Fastest`); the compressed form is kept only when it is under 90% of
    /// the original.
    Zstd,
}
