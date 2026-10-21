//! Atomic replacement of a regular file beneath the root (「原子替换」 in
//! docs/design/platform.md), for saving an edit: a temporary file in the
//! same directory is written, synced and given the original's permission
//! bits, the identity of the file on disk is compared with the one the caller
//! read, and `renameat` puts the temporary file in its place.
//!
//! The directory is opened from the root like `list` opens one, so no level
//! of the path is followed through a symbolic link; every later call is an
//! `*at` call on that directory's descriptor with a single name. The
//! temporary file is created with `O_CREAT | O_EXCL | O_NOFOLLOW`, which never
//! opens an existing entry, link or not, and `renameat` replaces the entry
//! itself, never what a link there points to.

use std::fmt;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{AtFlags, Mode, OFlags, RawMode};
use rustix::io::Errno;

use super::{FileIdentity, FileKind, Meta, RelPath, SafeError, SafeRoot, split};

/// How many temporary names are tried before giving up with
/// `Io(AlreadyExists)`. A name is taken only by a leftover of an earlier run
/// or by another process choosing the same name.
pub(crate) const TEMP_ATTEMPTS: u32 = 16;

/// The permission bits carried over to the replacement (POSIX's file
/// permission bits: owner, group, other). Set-user-ID, set-group-ID and the
/// sticky bit are not.
const PERMISSION_BITS: u32 = 0o777;

/// The temporary file's mode until `fchmod`: no one else can read the new
/// content before it is in place.
const TEMP_MODE: RawMode = 0o600;

const TEMP_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Why [`SafeRoot::replace_file`] replaced nothing. In every case the file
/// at `rel` is left as it was and no temporary file is left behind (unless
/// removing it fails, see 「原子替换」 in the design).
#[derive(Debug)]
pub enum ReplaceError {
    /// `expect` was given and the file on disk no longer has that identity:
    /// it was written, truncated, `chmod`ed or replaced since the caller read
    /// it. `now` is its identity when it was compared.
    Changed { now: FileIdentity },
    /// `rel` does not exist (this call never creates a file), or an
    /// intermediate component is missing or not a directory.
    NotFound,
    /// A component of `rel`, the last one included, is a symbolic link.
    PathThroughSymlink,
    /// `rel` is not a regular file: a directory, FIFO, socket or device.
    Unsupported,
    /// Anything else: permissions, a full disk, IO errors, an invalid path
    /// (`InvalidInput`), every temporary name taken (`AlreadyExists`).
    Io(io::Error),
}

impl fmt::Display for ReplaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplaceError::Changed { .. } => f.write_str("the file changed on disk"),
            ReplaceError::NotFound => f.write_str("no such file beneath the root"),
            ReplaceError::PathThroughSymlink => {
                f.write_str("a component of the path is a symbolic link")
            }
            ReplaceError::Unsupported => f.write_str("not a regular file"),
            ReplaceError::Io(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for ReplaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReplaceError::Io(error) => Some(error),
            _ => None,
        }
    }
}

fn io_error(errno: Errno) -> ReplaceError {
    ReplaceError::Io(errno.into())
}

impl SafeRoot {
    /// Replaces the regular file `rel` with `bytes`, atomically: readers see
    /// the old file or the new one, never a mix.
    ///
    /// In `rel`'s directory (opened like [`SafeRoot::list`] opens one), a
    /// temporary file is created with `O_CREAT | O_EXCL | O_NOFOLLOW`,
    /// written, given the permission bits (`0o777`) of the file it replaces
    /// and `fsync`ed. Then `rel` is `fstatat`ed without following a link:
    /// when `expect` is given and the identity differs
    /// ([`ReplaceError::Changed`]), or `rel` is no longer a regular file, the
    /// temporary file is removed; otherwise `renameat` puts it in place. On
    /// success, returns the new file's identity, to `expect` on the next save.
    ///
    /// Blocks on system calls and disk IO; call it from a background task,
    /// never from the UI thread.
    pub fn replace_file(
        &self,
        rel: RelPath<'_>,
        bytes: &[u8],
        expect: Option<FileIdentity>,
    ) -> Result<FileIdentity, ReplaceError> {
        self.replace_with_names(rel, bytes, expect, &mut temp_name)
    }

    /// [`SafeRoot::replace_file`] with the temporary names drawn from
    /// `names`, so tests can make them collide.
    pub(crate) fn replace_with_names(
        &self,
        rel: RelPath<'_>,
        bytes: &[u8],
        expect: Option<FileIdentity>,
        names: &mut dyn FnMut() -> Vec<u8>,
    ) -> Result<FileIdentity, ReplaceError> {
        let (parent, name) = split(rel.0).ok_or_else(invalid_path)?;
        let opened: Option<OwnedFd> = match parent {
            None => None,
            Some(parent) => Some(self.open_directory(RelPath(parent)).map_err(parent_error)?),
        };
        let dir = opened.as_ref().map_or(self.fd.as_fd(), AsFd::as_fd);

        // Checked before anything is created: a missing file or a link is
        // refused without touching the directory.
        let original = regular_file(dir, name)?;

        let (fd, temp) = Temp::create(dir, names)?;
        write_all(fd.as_fd(), bytes)?;
        let mode = Mode::from_bits_truncate(narrow(original.mode & PERMISSION_BITS));
        rustix::fs::fchmod(&fd, mode).map_err(io_error)?;
        // After `fchmod`, so the sync covers the mode as well as the data.
        rustix::fs::fsync(&fd).map_err(io_error)?;

        let now = regular_file(dir, name)?;
        if let Some(expect) = expect
            && now.identity() != expect
        {
            return Err(ReplaceError::Changed {
                now: now.identity(),
            });
        }
        rustix::fs::renameat(dir, &temp.name[..], dir, name).map_err(io_error)?;
        temp.keep();
        // The rename changes the inode's `ctime` on some file systems: the
        // identity is taken after it, from the descriptor of the new file.
        let placed = rustix::fs::fstat(&fd).map_err(io_error)?;
        Ok(Meta::from_stat(&placed).identity())
    }
}

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`, which must find a regular file.
fn regular_file(dir: BorrowedFd<'_>, name: &[u8]) -> Result<Meta, ReplaceError> {
    let meta = match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => Meta::from_stat(&st),
        Err(Errno::NOENT | Errno::NOTDIR) => return Err(ReplaceError::NotFound),
        Err(errno) => return Err(io_error(errno)),
    };
    match meta.kind {
        FileKind::File => Ok(meta),
        FileKind::Symlink => Err(ReplaceError::PathThroughSymlink),
        _ => Err(ReplaceError::Unsupported),
    }
}

/// The errors of opening `rel`'s directory. An intermediate component that
/// is not a directory means there is no such file, as in `stat`.
fn parent_error(error: SafeError) -> ReplaceError {
    match error {
        SafeError::NotFound => ReplaceError::NotFound,
        SafeError::PathThroughSymlink => ReplaceError::PathThroughSymlink,
        SafeError::Io(error) if error.kind() == io::ErrorKind::NotADirectory => {
            ReplaceError::NotFound
        }
        SafeError::Io(error) => ReplaceError::Io(error),
    }
}

fn invalid_path() -> ReplaceError {
    ReplaceError::Io(io::Error::new(
        io::ErrorKind::InvalidInput,
        "path is not a valid relative path beneath the root",
    ))
}

/// `0o777` fits every target's `mode_t`; generic so the conversion compiles
/// without `useless_conversion` where `mode_t` is `u32`.
fn narrow<T: TryFrom<u32> + Default>(value: u32) -> T {
    T::try_from(value).unwrap_or_default()
}

fn write_all(fd: BorrowedFd<'_>, mut bytes: &[u8]) -> Result<(), ReplaceError> {
    while !bytes.is_empty() {
        match rustix::io::write(fd, bytes) {
            Ok(0) => return Err(ReplaceError::Io(io::ErrorKind::WriteZero.into())),
            Ok(written) => bytes = &bytes[written..],
            Err(Errno::INTR) => {}
            Err(errno) => return Err(io_error(errno)),
        }
    }
    Ok(())
}

/// The name of a temporary file being written. Dropping it before
/// [`Temp::keep`] removes the file, on every error path.
struct Temp<'dir> {
    dir: BorrowedFd<'dir>,
    name: Vec<u8>,
    kept: bool,
}

impl<'dir> Temp<'dir> {
    /// Creates a new file in `dir` under the first free name `names` gives,
    /// and returns its descriptor with the guard for its name.
    fn create(
        dir: BorrowedFd<'dir>,
        names: &mut dyn FnMut() -> Vec<u8>,
    ) -> Result<(OwnedFd, Temp<'dir>), ReplaceError> {
        let mode = Mode::from_raw_mode(TEMP_MODE);
        for _ in 0..TEMP_ATTEMPTS {
            let name = names();
            loop {
                match rustix::fs::openat(dir, &name[..], TEMP_FLAGS, mode) {
                    Ok(fd) => {
                        let temp = Temp {
                            dir,
                            name,
                            kept: false,
                        };
                        return Ok((fd, temp));
                    }
                    Err(Errno::INTR) => {}
                    // Taken, by a file or a link: never opened, try another.
                    Err(Errno::EXIST) => break,
                    Err(errno) => return Err(io_error(errno)),
                }
            }
        }
        Err(ReplaceError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "every temporary name tried is taken",
        )))
    }

    /// The file was renamed to its final name: nothing to remove.
    fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Temp<'_> {
    fn drop(&mut self) {
        if !self.kept {
            // Best effort: the error that got us here is the one reported.
            let _ = rustix::fs::unlinkat(self.dir, &self.name[..], AtFlags::empty());
        }
    }
}

/// The next temporary name: `.lectern-save-<pid>-<sequence>-<nanos>.tmp`, in
/// hex. The sequence keeps names unique within the process; the pid and the
/// clock keep them apart from other processes and from leftovers of earlier
/// runs. `O_EXCL` makes any collision harmless.
fn temp_name() -> Vec<u8> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    format!(
        ".lectern-save-{:x}-{sequence:x}-{nanos:x}.tmp",
        std::process::id()
    )
    .into_bytes()
}
