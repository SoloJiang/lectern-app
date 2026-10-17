//! Linux: one-step opens with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`.
//! The kernel refuses any symbolic link and any resolution that leaves the
//! root (`..`, absolute paths), as a second line of defence behind the
//! `RelPath` rules.

use std::os::fd::{BorrowedFd, OwnedFd};

use rustix::fs::{Mode, OFlags, ResolveFlags};
use rustix::io::Errno;

use super::{Backend, Target};

const RESOLVE: ResolveFlags = ResolveFlags::BENEATH.union(ResolveFlags::NO_SYMLINKS);

/// Opens `path` beneath `root`: the parent directory for `stat`
/// (`O_PATH | O_DIRECTORY`), or the entry for `read`
/// (`O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_NOCTTY`).
pub(crate) fn open_beneath(
    root: BorrowedFd<'_>,
    path: &[u8],
    target: Target,
) -> Result<OwnedFd, Errno> {
    let flags = match target {
        Target::Dir => OFlags::PATH | OFlags::DIRECTORY,
        Target::Entry => OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY,
    };
    rustix::fs::openat2(root, path, flags | OFlags::CLOEXEC, Mode::empty(), RESOLVE)
}

/// The probe `SafeRoot::open` runs once per root.
pub(crate) fn probe(root: BorrowedFd<'_>) -> Result<(), Errno> {
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC;
    rustix::fs::openat2(root, ".", flags, Mode::empty(), RESOLVE).map(drop)
}

/// `ENOSYS` (kernels before 5.6), `EPERM` (blocked by seccomp, as in some
/// container runtimes), `EINVAL` and `E2BIG` mean `openat2` cannot be used.
/// Any other outcome keeps it: should a later call fail anyway, the walk
/// redoes it.
pub(crate) fn backend_for_probe(probe: Result<(), Errno>) -> Backend {
    match probe {
        Err(Errno::NOSYS | Errno::PERM | Errno::INVAL | Errno::TOOBIG) => Backend::Walk,
        Ok(()) | Err(_) => Backend::Openat2,
    }
}
