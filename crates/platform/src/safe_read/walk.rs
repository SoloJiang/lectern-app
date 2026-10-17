//! The component-wise implementation (`Walk` in the design): opens one level
//! at a time from the root's descriptor with `O_NOFOLLOW`, and when an open
//! fails, asks `fstatat` on the same parent why. It is the whole
//! implementation where no one-step open exists, and it redoes every call
//! whose one-step open failed with anything but `ENOENT`, so its answer is
//! the one callers see.
//!
//! At most two descriptors are held at a time: each level is opened from the
//! previous one, which is closed right after. Nothing is kept across calls.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;

use super::{
    FileKind, Meta, Read, SafeError, invalid_path, io_error, is_valid_name, read_opened, stat_entry,
};

/// How often one level is retried when `fstatat` contradicts the error of the
/// open just before it, which means the entry was replaced between the two
/// calls (see `contradicts`).
const RACE_RETRIES: u32 = 8;

/// Intermediate directories. Linux opens them with `O_PATH`, which needs only
/// search permission on the directory, like `lstat`.
#[cfg(target_os = "linux")]
const DIR_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[cfg(not(target_os = "linux"))]
const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// The last component of `read`. `O_NONBLOCK` returns at once for a FIFO
/// without a writer.
const ENTRY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOCTTY)
    .union(OFlags::CLOEXEC);

/// `stat` of `name` inside `parent` (both already checked against the
/// `RelPath` rules).
pub(crate) fn stat(
    root: BorrowedFd<'_>,
    parent: Option<&[u8]>,
    name: &[u8],
) -> Result<Meta, SafeError> {
    let dir = open_parent(root, parent)?;
    stat_entry(dir.as_ref().map_or(root, AsFd::as_fd), name)
}

/// `read` of `name` inside `parent` (both already checked against the
/// `RelPath` rules).
pub(crate) fn read(
    root: BorrowedFd<'_>,
    parent: Option<&[u8]>,
    name: &[u8],
    max: u64,
) -> Result<Read, SafeError> {
    let dir = open_parent(root, parent)?;
    let dir = dir.as_ref().map_or(root, AsFd::as_fd);
    let mut retries = 0;
    loop {
        let error = match rustix::fs::openat(dir, name, ENTRY_FLAGS, Mode::empty()) {
            Ok(fd) => return read_opened(fd, max),
            Err(Errno::NOENT | Errno::NOTDIR) => return Err(SafeError::NotFound),
            Err(error) => error,
        };
        let meta = match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => Meta::from_stat(&st),
            Err(Errno::NOENT | Errno::NOTDIR) => return Err(SafeError::NotFound),
            Err(_) => return Err(io_error(error)),
        };
        match meta.kind {
            FileKind::Symlink => match rustix::fs::readlinkat(dir, name, Vec::new()) {
                Ok(target) => {
                    return Ok(Read::Symlink {
                        meta,
                        target: target.into_bytes(),
                    });
                }
                Err(Errno::NOENT) => return Err(SafeError::NotFound),
                // No longer a link: replaced since `fstatat`.
                Err(Errno::INVAL) if retries < RACE_RETRIES => retries += 1,
                Err(error) => return Err(io_error(error)),
            },
            FileKind::File | FileKind::Dir => {
                if contradicts(error) && retries < RACE_RETRIES {
                    retries += 1;
                } else {
                    // Such as no read permission.
                    return Err(io_error(error));
                }
            }
            FileKind::Fifo
            | FileKind::Socket
            | FileKind::CharDevice
            | FileKind::BlockDevice
            | FileKind::Other => return Ok(Read::Unsupported { meta }),
        }
    }
}

/// Opens every component of `parent` in turn, starting at `root`. `None` when
/// there is no parent (the entry is directly beneath the root).
fn open_parent(root: BorrowedFd<'_>, parent: Option<&[u8]>) -> Result<Option<OwnedFd>, SafeError> {
    let Some(parent) = parent else {
        return Ok(None);
    };
    let mut current: Option<OwnedFd> = None;
    for name in parent.split(|&b| b == b'/') {
        if !is_valid_name(name) {
            return Err(invalid_path());
        }
        let next = open_dir(current.as_ref().map_or(root, AsFd::as_fd), name)?;
        // Closes the previous level now that the next one is open.
        current = Some(next);
    }
    Ok(current)
}

/// Opens one intermediate directory, or says why it cannot be used.
fn open_dir(dir: BorrowedFd<'_>, name: &[u8]) -> Result<OwnedFd, SafeError> {
    let mut retries = 0;
    loop {
        let error = match rustix::fs::openat(dir, name, DIR_FLAGS, Mode::empty()) {
            Ok(fd) => return Ok(fd),
            Err(Errno::NOENT) => return Err(SafeError::NotFound),
            Err(error) => error,
        };
        match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => match Meta::from_stat(&st).kind {
                FileKind::Symlink => return Err(SafeError::PathThroughSymlink),
                FileKind::Dir if contradicts(error) && retries < RACE_RETRIES => retries += 1,
                // Such as no search permission.
                FileKind::Dir => return Err(io_error(error)),
                _ => return Err(SafeError::NotFound),
            },
            Err(Errno::NOENT) => return Err(SafeError::NotFound),
            Err(_) => return Err(io_error(error)),
        }
    }
}

/// Whether `fstatat` finding a directory or a regular file contradicts the
/// error of the `O_NOFOLLOW` open before it. These errors come only from a
/// symbolic link (`ELOOP`; `ENOTDIR` for `O_PATH | O_DIRECTORY`), a non
/// directory opened with `O_DIRECTORY` (`ENOTDIR`) or a socket (`ENXIO`,
/// `EOPNOTSUPP`), so the entry was replaced between the two calls and the
/// level is tried again. Without this, a directory swapped back and forth with
/// a symbolic link would sometimes give `Io` instead of a consistent answer.
fn contradicts(error: Errno) -> bool {
    matches!(
        error,
        Errno::LOOP | Errno::NOTDIR | Errno::NXIO | Errno::OPNOTSUPP
    )
}
