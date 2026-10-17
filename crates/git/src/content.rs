//! Why a file's content isn't available (docs/design/git.md「数据结构」).
//! Step 1 uses `UnsupportedType`, `PathThroughSymlink` and `Unreadable`
//! for `Worktree::read_file`; the worktree comparison (step 3) uses the
//! rest.

use crate::Oid;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unavailable {
    /// A clean / process filter is configured (e.g. Git LFS): "uses a
    /// filter, comparison not supported". `raw` identifies the worktree's
    /// raw bytes, to tell whether they changed.
    Filtered { raw: RawIdentity },
    /// FIFO, socket, device file (or, for `read_file`, a directory).
    UnsupportedType,
    /// A component in the middle of the path is a symbolic link.
    PathThroughSymlink,
    /// An untracked directory with its own `.git`.
    NestedRepository,
    /// Not in the object database (e.g. a partial clone).
    Missing,
    /// Reading failed.
    Unreadable,
    /// Over the change set's retained-content limit: read and hashed during
    /// the comparison, bytes not kept.
    NotRetained { oid: Oid },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RawIdentity {
    /// SHA-256 of the raw bytes, up to 64 MB.
    Sha256([u8; 32]),
    /// Above 64 MB the content isn't read.
    Stat {
        size: u64,
        mtime_ns: i128,
        inode: u64,
    },
}
