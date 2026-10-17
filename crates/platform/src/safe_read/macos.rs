//! macOS: one-step calls with `O_NOFOLLOW_ANY` (`openat`, macOS 11 and later)
//! and `AT_SYMLINK_NOFOLLOW_ANY` (`fstatat`, macOS 13 and later), which fail
//! with `ELOOP` when a symbolic link would be followed at any level of the
//! path. macOS has no counterpart of `RESOLVE_BENEATH`; the `RelPath` rules
//! (no `..`, not absolute) keep the resolution beneath the root once no link
//! is followed.

use std::os::fd::{BorrowedFd, OwnedFd};

use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;

use super::{Backend, Meta};

/// `AT_SYMLINK_NOFOLLOW_ANY` from `<sys/fcntl.h>`. `fstatat` accepts it from
/// macOS 13 (XNU 8792); earlier kernels reject the unknown bit with `EINVAL`,
/// which the probe in `SafeRoot::open` detects. Neither rustix 1.1 nor libc
/// 0.2 defines it; `AtFlags` keeps bits it does not name (`const _ = !0`), so
/// the raw value reaches `fstatat` unchanged.
const AT_SYMLINK_NOFOLLOW_ANY: AtFlags = AtFlags::from_bits_retain(0x0800);

/// `AT_SYMLINK_NOFOLLOW` keeps the last component unfollowed, as in `lstat`:
/// a symbolic link there is reported, not refused. `AT_SYMLINK_NOFOLLOW_ANY`
/// refuses one anywhere before it; should the kernel refuse a last one too,
/// the walk that redoes the call reports it.
const STAT_FLAGS: AtFlags = AtFlags::SYMLINK_NOFOLLOW.union(AT_SYMLINK_NOFOLLOW_ANY);

/// Opens the entry at `path` beneath `root` for `read`
/// (`O_RDONLY | O_NONBLOCK | O_NOCTTY`).
pub(crate) fn open_nofollow_any(root: BorrowedFd<'_>, path: &[u8]) -> Result<OwnedFd, Errno> {
    open(
        root,
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY,
    )
}

/// Opens the directory at `path` beneath `root`, the parent for `stat` where
/// `fstatat` lacks `AT_SYMLINK_NOFOLLOW_ANY` (`O_RDONLY | O_DIRECTORY`).
pub(crate) fn open_dir_nofollow_any(root: BorrowedFd<'_>, path: &[u8]) -> Result<OwnedFd, Errno> {
    open(root, path, OFlags::RDONLY | OFlags::DIRECTORY)
}

fn open(root: BorrowedFd<'_>, path: &[u8], flags: OFlags) -> Result<OwnedFd, Errno> {
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

/// The probe `SafeRoot::open` runs once per root: `fstatat(root, ".")` with
/// the flags of [`stat_nofollow_any`]. `"."` is always a valid name for an
/// open directory and has no link to refuse, so `EINVAL` can only mean the
/// kernel does not know `AT_SYMLINK_NOFOLLOW_ANY`.
pub(crate) fn probe(root: BorrowedFd<'_>) -> Result<(), Errno> {
    rustix::fs::statat(root, ".", STAT_FLAGS).map(drop)
}

/// `EINVAL` (before macOS 13) keeps `stat` on the parent open
/// ([`Backend::NofollowAnyParent`]). Any other outcome keeps the one
/// `fstatat`: should a later call fail anyway, the walk redoes it.
pub(crate) fn backend_for_probe(probe: Result<(), Errno>) -> Backend {
    match probe {
        Err(Errno::INVAL) => Backend::NofollowAnyParent,
        Ok(()) | Err(_) => Backend::NofollowAny,
    }
}
