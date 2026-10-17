//! The backend interface: `platform` implements it (FSEvents and kqueue),
//! tests use `testing::FakeBackend`.

use std::ffi::OsString;
use std::fmt;
use std::ops::{BitOr, BitOrAssign};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lectern_kernel::BusyGuard;

use crate::queue::{Queue, TargetId};
use crate::util::lock;

/// A file watching backend.
pub trait Backend: Send + Sync + 'static {
    /// Starts watching `target`; its events are then delivered through
    /// `sink`. Called only on the fs_watch thread; may block (create a stream,
    /// open descriptors) and should return within a few milliseconds. When
    /// `target.path` does not exist, returns `BackendError::NotFound` and
    /// creates no watch.
    fn arm(&self, target: &Target, sink: EventSink) -> Result<Arc<dyn Armed>, BackendError>;
}

/// A started watch. It stops when the last `Arc` is dropped; once that drop
/// returns, the backend no longer calls this target's `EventSink`.
pub trait Armed: Send + Sync {
    /// Hands events that already happened but were not yet delivered to the
    /// `EventSink` right away; when it returns they are in the fs_watch queue.
    /// Called on the thread calling `flush_revision`, blocking;
    /// `EventSink::send` never blocks, so the wait is bounded.
    fn flush(&self) -> Result<(), BackendError>;
}

/// A path the backend watches.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target {
    /// Absolute path. The backend resolves it itself (FSEvents reports paths
    /// with symbolic links resolved); delivered paths are relative to it.
    pub path: PathBuf,
    pub scope: Scope,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Scope {
    /// The directory and everything below it.
    Tree,
    /// Entries added to, removed from or renamed in the directory itself,
    /// and content and metadata changes of each entry in `names` (not
    /// following symbolic links). A named entry that is deleted or replaced
    /// by a rename and then reappears is watched again. With no `names`, only
    /// entries being added and removed are reported.
    Entries { names: Vec<OsString> },
}

/// Where the backend delivers events. `Clone + Send + Sync`; `send` never
/// blocks: when the fs_watch queue is full the batch is dropped and the
/// target is marked as overflowed, which fs_watch handles as a rescan.
///
/// Before queueing, `send` takes a `BusyGuard` from one of the target's
/// subscriptions; it travels with the raw batch (docs/design/fs_watch.md
/// 「数据流与线程」).
#[derive(Clone)]
pub struct EventSink {
    pub(crate) target: TargetId,
    pub(crate) shared: Arc<TargetShared>,
    pub(crate) queue: Arc<Queue>,
}

impl EventSink {
    pub fn send(&self, batch: RawBatch) {
        if batch.events.is_empty() {
            return;
        }
        let guard = self.shared.busy();
        let refused = self.queue.push_raw(self.target, batch, guard);
        // A guard refused by a full or stopped queue is dropped here, outside
        // the queue's lock.
        drop(refused);
    }
}

impl fmt::Debug for EventSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventSink")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

pub(crate) type BusyFn = Arc<dyn Fn() -> Option<BusyGuard> + Send + Sync>;

/// What an `EventSink` shares with the fs_watch thread: the subscriptions
/// it can take a guard from, oldest first.
#[derive(Default)]
pub(crate) struct TargetShared {
    busy: Mutex<Vec<BusyFn>>,
}

impl TargetShared {
    pub(crate) fn set_busy(&self, sources: Vec<BusyFn>) {
        *lock(&self.busy) = sources;
    }

    /// A guard from the oldest subscription that still gives one. The list
    /// is copied out so `Sink::busy` runs without this lock held.
    fn busy(&self) -> Option<BusyGuard> {
        let sources = lock(&self.busy).clone();
        sources.iter().find_map(|busy| busy())
    }
}

/// The events of one backend callback; paths are relative to the target.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RawBatch {
    pub events: Vec<RawEvent>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RawEvent {
    /// Relative to the target, `/`-separated, raw bytes; empty for the
    /// target itself. For `Entries` it is the entry name.
    pub path: Box<[u8]>,
    pub flags: Flags,
}

impl fmt::Debug for RawEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RawEvent({:?}, {:?})",
            String::from_utf8_lossy(&self.path),
            self.flags
        )
    }
}

/// Event flags. In file-event mode FSEvents merges successive changes of one
/// path into one event, so CREATED / REMOVED / RENAMED / MODIFIED are hints
/// only: fs_watch reads them as "this path may have changed" and never
/// derives the final state from them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags(pub u32);

impl Flags {
    /// The entry is a regular file.
    pub const FILE: Flags = Flags(1);
    /// The entry is a directory.
    pub const DIR: Flags = Flags(1 << 1);
    pub const SYMLINK: Flags = Flags(1 << 2);
    pub const CREATED: Flags = Flags(1 << 3);
    pub const REMOVED: Flags = Flags(1 << 4);
    pub const RENAMED: Flags = Flags(1 << 5);
    /// Content.
    pub const MODIFIED: Flags = Flags(1 << 6);
    /// Permissions, owner, extended attributes, times.
    pub const META: Flags = Flags(1 << 7);
    /// Events below this path could not be reported one by one; rescan
    /// (FSEvents' MustScanSubDirs).
    pub const RESCAN: Flags = Flags(1 << 8);
    /// With RESCAN: events were dropped in user space or in the kernel
    /// (UserDropped / KernelDropped).
    pub const DROPPED: Flags = Flags(1 << 9);
    /// The target itself was deleted, renamed or replaced (FSEvents'
    /// RootChanged, NOTE_DELETE / NOTE_RENAME on a kqueue directory
    /// descriptor).
    pub const ROOT_CHANGED: Flags = Flags(1 << 10);
    /// The volume holding the target was unmounted.
    pub const UNMOUNTED: Flags = Flags(1 << 11);

    pub fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any flag of `other` is set.
    pub(crate) fn any(self, other: Flags) -> bool {
        self.0 & other.0 != 0
    }
}

impl BitOr for Flags {
    type Output = Flags;

    fn bitor(self, other: Flags) -> Flags {
        Flags(self.0 | other.0)
    }
}

impl BitOrAssign for Flags {
    fn bitor_assign(&mut self, other: Flags) {
        self.0 |= other.0;
    }
}

impl fmt::Debug for Flags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const NAMES: [(Flags, &str); 12] = [
            (Flags::FILE, "FILE"),
            (Flags::DIR, "DIR"),
            (Flags::SYMLINK, "SYMLINK"),
            (Flags::CREATED, "CREATED"),
            (Flags::REMOVED, "REMOVED"),
            (Flags::RENAMED, "RENAMED"),
            (Flags::MODIFIED, "MODIFIED"),
            (Flags::META, "META"),
            (Flags::RESCAN, "RESCAN"),
            (Flags::DROPPED, "DROPPED"),
            (Flags::ROOT_CHANGED, "ROOT_CHANGED"),
            (Flags::UNMOUNTED, "UNMOUNTED"),
        ];
        let mut first = true;
        for (flag, name) in NAMES {
            if self.contains(flag) {
                if !first {
                    f.write_str("|")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        if first {
            f.write_str("NONE")?;
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BackendError {
    NotFound,
    PermissionDenied,
    /// Out of descriptors or streams.
    Exhausted,
    Other(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::NotFound => f.write_str("the path does not exist"),
            BackendError::PermissionDenied => f.write_str("permission denied"),
            BackendError::Exhausted => f.write_str("out of watch descriptors"),
            BackendError::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for BackendError {}
