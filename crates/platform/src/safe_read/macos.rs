//! macOS: one-step opens with `O_NOFOLLOW_ANY`, which fails with `ELOOP` when
//! any component of the path is a symbolic link. macOS has no counterpart of
//! `RESOLVE_BENEATH`; the `RelPath` rules (no `..`, not absolute) keep the
//! resolution beneath the root once no link is followed.

use std::os::fd::{BorrowedFd, OwnedFd};

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;

use super::Target;

/// Opens `path` beneath `root`: the parent directory for `stat`
/// (`O_RDONLY | O_DIRECTORY`), or the entry for `read`
/// (`O_RDONLY | O_NONBLOCK | O_NOCTTY`).
pub(crate) fn open_nofollow_any(
    root: BorrowedFd<'_>,
    path: &[u8],
    target: Target,
) -> Result<OwnedFd, Errno> {
    let flags = match target {
        Target::Dir => OFlags::RDONLY | OFlags::DIRECTORY,
        Target::Entry => OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY,
    };
    rustix::fs::openat(
        root,
        path,
        flags | OFlags::NOFOLLOW_ANY | OFlags::CLOEXEC,
        Mode::empty(),
    )
}
