//! macOS: one-step calls with `O_NOFOLLOW_ANY` (`openat`) and
//! `AT_SYMLINK_NOFOLLOW_ANY` (`fstatat`), which fail with `ELOOP` when a
//! symbolic link would be followed at any level of the path. macOS has no
//! counterpart of `RESOLVE_BENEATH`; the `RelPath` rules (no `..`, not
//! absolute) keep the resolution beneath the root once no link is followed.

use std::os::fd::{BorrowedFd, OwnedFd};

use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;

use super::Meta;

/// `AT_SYMLINK_NOFOLLOW_ANY` from `<sys/fcntl.h>` (macOS 11 and later, like
/// `O_NOFOLLOW_ANY`). Neither rustix 1.1 nor libc 0.2 defines it; `AtFlags`
/// keeps bits it does not name (`const _ = !0`), so the raw value reaches
/// `fstatat` unchanged.
const AT_SYMLINK_NOFOLLOW_ANY: AtFlags = AtFlags::from_bits_retain(0x0800);

/// `AT_SYMLINK_NOFOLLOW` keeps the last component unfollowed, as in `lstat`:
/// a symbolic link there is reported, not refused. `AT_SYMLINK_NOFOLLOW_ANY`
/// refuses one anywhere before it; should the kernel refuse a last one too,
/// the walk that redoes the call reports it.
const STAT_FLAGS: AtFlags = AtFlags::SYMLINK_NOFOLLOW.union(AT_SYMLINK_NOFOLLOW_ANY);

/// Opens the entry at `path` beneath `root` for `read`
/// (`O_RDONLY | O_NONBLOCK | O_NOCTTY`).
pub(crate) fn open_nofollow_any(root: BorrowedFd<'_>, path: &[u8]) -> Result<OwnedFd, Errno> {
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY;
    rustix::fs::openat(
        root,
        path,
        flags | OFlags::NOFOLLOW_ANY | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

/// `stat` of `path` beneath `root` in one `fstatat`, whatever its depth.
pub(crate) fn stat_nofollow_any(root: BorrowedFd<'_>, path: &[u8]) -> Result<Meta, Errno> {
    rustix::fs::statat(root, path, STAT_FLAGS).map(|st| Meta::from_stat(&st))
}
