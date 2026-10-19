//! Reading files beneath a directory root without following symbolic links at
//! any level of the path and without leaving the root (docs/design/platform.md).
//!
//! Every call starts from the root's descriptor. A one-step call resolves the
//! whole path in the kernel (`openat2` with `RESOLVE_BENEATH |
//! RESOLVE_NO_SYMLINKS` on Linux; `O_NOFOLLOW_ANY` for opens and
//! `AT_SYMLINK_NOFOLLOW_ANY` for `fstatat` on macOS); when it fails for any
//! reason other than `ENOENT`, the call is redone by [`walk`], which opens one
//! level at a time and tells the caller why the path is unusable.
//!
//! [`SafeRoot::open_file`] opens a regular file the same way and hands its
//! descriptor to an [`OpenFile`], which reads byte ranges with `pread`
//! ([`range`], 「范围读取」 in the design).

use std::fmt;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::buffer::spare_capacity;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux::Target;
#[cfg(target_os = "macos")]
mod macos;
mod range;
mod walk;

pub use range::{FileIdentity, FullRead, MAX_RANGE_BYTES, MAX_RANGES, OpenFile, Opened, RangeRead};

#[cfg(test)]
mod tests;

/// A descriptor of a directory root. `Send + Sync`, not `Clone`: share it
/// through an `Arc`.
///
/// It is bound to the directory that was opened: if the root path is renamed
/// or replaced later, calls still act on the original directory (`NotFound`
/// once that directory is deleted).
#[derive(Debug)]
pub struct SafeRoot {
    fd: OwnedFd,
    backend: Backend,
}

/// How a [`SafeRoot`] opens a path in one step. Chosen when the root is
/// opened and never changed afterwards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Backend {
    /// `openat2(…, RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`.
    #[cfg(target_os = "linux")]
    Openat2,
    /// `openat(…, O_NOFOLLOW_ANY)` for `read`, one
    /// `fstatat(…, AT_SYMLINK_NOFOLLOW | AT_SYMLINK_NOFOLLOW_ANY)` for `stat`
    /// (macOS 13 and later).
    #[cfg(target_os = "macos")]
    NofollowAny,
    /// `NofollowAny` where `fstatat` rejects `AT_SYMLINK_NOFOLLOW_ANY`
    /// (macOS 11 and 12): `stat` opens the parent with `O_NOFOLLOW_ANY`, then
    /// `fstatat(…, AT_SYMLINK_NOFOLLOW)` on the last component.
    #[cfg(target_os = "macos")]
    NofollowAnyParent,
    /// No one-step open: every call goes through [`walk`].
    #[cfg_attr(
        all(target_os = "macos", not(test)),
        expect(
            dead_code,
            reason = "macOS always opens in one step; walk only redoes failed calls"
        )
    )]
    Walk,
}

impl SafeRoot {
    /// Opens the root directory (`O_RDONLY | O_DIRECTORY | O_CLOEXEC`). The
    /// root path itself resolves normally, following symbolic links (pass a
    /// canonical path, such as `/private/tmp` for `/tmp`); the restrictions
    /// apply only to the relative paths beneath it. This is where the
    /// implementation is chosen (one probe `fstatat` or `openat2`).
    pub fn open(root: &Path) -> io::Result<SafeRoot> {
        let fd = open_root(root)?;
        let backend = default_backend(fd.as_fd());
        Ok(SafeRoot { fd, backend })
    }

    /// [`SafeRoot::open`] with a forced implementation, for tests.
    #[cfg(test)]
    pub(crate) fn open_with(root: &Path, backend: Backend) -> io::Result<SafeRoot> {
        Ok(SafeRoot {
            fd: open_root(root)?,
            backend,
        })
    }

    /// `stat` that follows no symbolic link at any level. When the last
    /// component is a symbolic link, returns the link's own metadata
    /// ([`FileKind::Symlink`]); when it is a directory, the directory's.
    ///
    /// Blocks on system calls; call it from a background task, never from the
    /// UI thread. Any number of threads may call it on the same root.
    pub fn stat(&self, rel: RelPath<'_>) -> Result<Meta, SafeError> {
        let (parent, name) = split(rel.0).ok_or_else(invalid_path)?;
        let root = self.fd.as_fd();
        match self.backend {
            #[cfg(target_os = "linux")]
            Backend::Openat2 => stat_in_parent(root, parent, name, |dir| {
                linux::open_beneath(root, dir, Target::Dir)
            }),
            #[cfg(target_os = "macos")]
            Backend::NofollowAnyParent => stat_in_parent(root, parent, name, |dir| {
                macos::open_dir_nofollow_any(root, dir)
            }),
            // One `fstatat` on the whole path, whatever its depth.
            #[cfg(target_os = "macos")]
            Backend::NofollowAny => match macos::stat_nofollow_any(root, rel.0) {
                Ok(meta) => Ok(meta),
                Err(Errno::NOENT) => Err(SafeError::NotFound),
                Err(_) => walk::stat(root, parent, name),
            },
            Backend::Walk => walk::stat(root, parent, name),
        }
    }

    /// Reads one entry. A regular file is read in full, up to `max` bytes
    /// inclusive; a symbolic link in the last component is read with
    /// `readlinkat`, not followed; directories, FIFOs, sockets and devices are
    /// not read. The type check and the read of a regular file use the same
    /// descriptor.
    ///
    /// Blocks on system calls and disk IO; call it from a background task,
    /// never from the UI thread. Any number of threads may call it on the same
    /// root.
    pub fn read(&self, rel: RelPath<'_>, max: u64) -> Result<Read, SafeError> {
        Ok(match self.open_entry(rel)? {
            Entry::File { meta, .. } if meta.size > max => Read::TooLarge { meta },
            Entry::File { fd, meta } => {
                match read_limited(fd.as_fd(), Cursor::Current, meta.size, max)? {
                    Some(bytes) => Read::File { meta, bytes },
                    None => Read::TooLarge { meta },
                }
            }
            Entry::Symlink { meta, target } => Read::Symlink { meta, target },
            Entry::Unsupported { meta } => Read::Unsupported { meta },
        })
    }

    /// Opens an entry for range reads ([`OpenFile`]). The open, its flags,
    /// the `fstat` on the descriptor and the [`walk`] fallback are those of
    /// [`SafeRoot::read`]: a regular file is [`Opened::File`], which keeps the
    /// descriptor and reads nothing yet, whatever its size; a symbolic link
    /// in the last component is read with `readlinkat`, not followed;
    /// directories, FIFOs, sockets and devices are closed unread. The errors
    /// are those of `read`.
    ///
    /// Unlike the other calls, the descriptor of a regular file stays open
    /// after the call: the [`OpenFile`] owns it and closes it when dropped.
    ///
    /// Blocks on system calls; call it from a background task, never from the
    /// UI thread. Any number of threads may call it on the same root.
    pub fn open_file(&self, rel: RelPath<'_>) -> Result<Opened, SafeError> {
        Ok(match self.open_entry(rel)? {
            Entry::File { fd, meta } => Opened::File(OpenFile::new(fd, meta)),
            Entry::Symlink { meta, target } => Opened::Symlink { meta, target },
            Entry::Unsupported { meta } => Opened::Unsupported { meta },
        })
    }

    /// The open shared by `read` and `open_file`: the one-step open of the
    /// entry, then `fstat` on the same descriptor; [`walk`] redoes any
    /// failure but `ENOENT`.
    fn open_entry(&self, rel: RelPath<'_>) -> Result<Entry, SafeError> {
        let (parent, name) = split(rel.0).ok_or_else(invalid_path)?;
        let root = self.fd.as_fd();
        match self.open_one_step(rel.0) {
            None => walk::entry(root, parent, name),
            Some(Ok(fd)) => classify(fd),
            Some(Err(Errno::NOENT)) => Err(SafeError::NotFound),
            Some(Err(_)) => walk::entry(root, parent, name),
        }
    }

    /// Lists a directory: the root when `dir` is None. Its entries' names
    /// (without `.` and `..`) and kinds, unsorted; a symbolic link is listed
    /// as a link, never followed. The directory is opened like `read` opens
    /// an entry, so no level of `dir` may be a symbolic link
    /// (`PathThroughSymlink`, also when `dir` itself is one); a `dir` that
    /// isn't a directory is `Io(NotADirectory)`. The names come from the
    /// directory that descriptor names, which stays beneath the root even if
    /// the path is replaced while it is read. A kind the directory entry
    /// doesn't record is taken from `fstatat(AT_SYMLINK_NOFOLLOW)` on the
    /// same descriptor.
    ///
    /// Blocks on system calls; call it from a background task, never from the
    /// UI thread. Any number of threads may call it on the same root.
    pub fn list(&self, dir: Option<RelPath<'_>>) -> Result<Vec<DirEntry>, SafeError> {
        let root = self.fd.as_fd();
        let Some(dir) = dir else {
            // `read_from` reopens `.`: the root's own file offset is unused.
            let listed = rustix::fs::Dir::read_from(root).map_err(io_error)?;
            return list_entries(listed);
        };
        let fd = self.open_directory(dir)?;
        list_entries(rustix::fs::Dir::new(fd).map_err(io_error)?)
    }

    /// Opens the directory `rel` beneath this root as a new root: the
    /// dependency roots of ADR 0033, such as `node_modules` or `.venv` in a
    /// worktree. It is opened like [`SafeRoot::list`] opens a directory, so no
    /// level of `rel` may be a symbolic link (`PathThroughSymlink`, also when
    /// `rel` itself is one), and an `rel` that isn't a directory is
    /// `Io(NotADirectory)`. The new root has this root's implementation and is
    /// bound to the directory that descriptor names: its calls never resolve
    /// `rel` again, so replacing the path afterwards does not move it, and
    /// they follow the same rules beneath it as this root's calls do beneath
    /// this root. No directory entry is read.
    ///
    /// Blocks on system calls; call it from a background task, never from the
    /// UI thread. Any number of threads may call it on the same root.
    pub fn open_dir(&self, rel: RelPath<'_>) -> Result<SafeRoot, SafeError> {
        Ok(SafeRoot {
            fd: self.open_directory(rel)?,
            backend: self.backend,
        })
    }

    /// The open shared by `list` and `open_dir`: the one-step open of
    /// `read`, then `fstat` on the same descriptor to check it is a
    /// directory; [`walk`] redoes any failure but `ENOENT`.
    fn open_directory(&self, dir: RelPath<'_>) -> Result<OwnedFd, SafeError> {
        split(dir.0).ok_or_else(invalid_path)?;
        let root = self.fd.as_fd();
        match self.open_one_step(dir.0) {
            None => walk::directory(root, dir.0),
            Some(Ok(fd)) => checked_directory(fd),
            Some(Err(Errno::NOENT)) => Err(SafeError::NotFound),
            Some(Err(_)) => walk::directory(root, dir.0),
        }
    }

    /// Opens the entry at `path` (already checked against the [`RelPath`]
    /// rules) in one step, or `None` when this root has no one-step open.
    fn open_one_step(&self, path: &[u8]) -> Option<Result<OwnedFd, Errno>> {
        match self.backend {
            #[cfg(target_os = "linux")]
            Backend::Openat2 => Some(linux::open_beneath(self.fd.as_fd(), path, Target::Entry)),
            #[cfg(target_os = "macos")]
            Backend::NofollowAny | Backend::NofollowAnyParent => {
                Some(macos::open_nofollow_any(self.fd.as_fd(), path))
            }
            Backend::Walk => {
                let _ = path;
                None
            }
        }
    }
}

/// `stat` through a one-step open of the parent (`open_parent`), then
/// `fstatat` on the last component; a single component needs no open.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stat_in_parent(
    root: BorrowedFd<'_>,
    parent: Option<&[u8]>,
    name: &[u8],
    open_parent: impl FnOnce(&[u8]) -> Result<OwnedFd, Errno>,
) -> Result<Meta, SafeError> {
    let Some(parent_path) = parent else {
        return stat_entry(root, name);
    };
    match open_parent(parent_path) {
        Ok(dir) => stat_entry(dir.as_fd(), name),
        Err(Errno::NOENT) => Err(SafeError::NotFound),
        Err(_) => walk::stat(root, parent, name),
    }
}

fn open_root(root: &Path) -> io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    Ok(rustix::fs::open(root, flags, Mode::empty())?)
}

#[cfg(target_os = "linux")]
fn default_backend(root: BorrowedFd<'_>) -> Backend {
    linux::backend_for_probe(linux::probe(root))
}

/// Apple Silicon runs macOS 11 or later, where `O_NOFOLLOW_ANY` exists;
/// `fstatat` takes `AT_SYMLINK_NOFOLLOW_ANY` from macOS 13.
#[cfg(target_os = "macos")]
fn default_backend(root: BorrowedFd<'_>) -> Backend {
    macos::backend_for_probe(macos::probe(root))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn default_backend(_root: BorrowedFd<'_>) -> Backend {
    Backend::Walk
}

/// A relative path beneath a root: raw bytes separated by `/`.
///
/// Invariants: not empty; every component is non-empty and neither `.` nor
/// `..`; no NUL byte; does not start with `/` (so it neither ends with `/` nor
/// has consecutive `/`). The field is private: build one with
/// [`RelPath::new`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct RelPath<'a>(&'a [u8]);

impl<'a> RelPath<'a> {
    /// Checks the invariants in one linear scan; `None` when they do not hold.
    /// Length is not checked (an overlong path fails in the system call).
    pub fn new(bytes: &'a [u8]) -> Option<RelPath<'a>> {
        is_valid_path(bytes).then_some(RelPath(bytes))
    }

    pub fn as_bytes(self) -> &'a [u8] {
        self.0
    }
}

fn is_valid_path(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.split(|&b| b == b'/').all(is_valid_name)
}

/// One path component: non-empty, not `.` or `..`, no `/` or NUL.
fn is_valid_name(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.iter().any(|&b| b == 0 || b == b'/')
}

/// Re-checks `path` against the [`RelPath`] rules and splits it into the
/// parent path (`None` for a single component) and the last component.
fn split(path: &[u8]) -> Option<(Option<&[u8]>, &[u8])> {
    if !is_valid_path(path) {
        return None;
    }
    Some(match path.iter().rposition(|&b| b == b'/') {
        Some(slash) => {
            let (parent, rest) = path.split_at(slash);
            (Some(parent), &rest[1..])
        }
        None => (None, path),
    })
}

fn invalid_path() -> SafeError {
    SafeError::Io(io::Error::new(
        io::ErrorKind::InvalidInput,
        "path is not a valid relative path beneath the root",
    ))
}

/// The result of [`SafeRoot::read`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Read {
    File {
        meta: Meta,
        bytes: Vec<u8>,
    },
    /// `target` is what `readlinkat` returned. It is not limited by `max`
    /// (the system's path length limit bounds it).
    Symlink {
        meta: Meta,
        target: Vec<u8>,
    },
    /// A regular file larger than `max`: no content is returned.
    TooLarge {
        meta: Meta,
    },
    /// A type whose content is not read: `meta.kind` is `Dir`, `Fifo`,
    /// `Socket`, `CharDevice`, `BlockDevice` or `Other`.
    Unsupported {
        meta: Meta,
    },
}

/// Why [`SafeRoot::stat`] or [`SafeRoot::read`] has no result.
#[derive(Debug)]
pub enum SafeError {
    /// The path does not exist: a component is missing (`ENOENT`), or an
    /// intermediate component exists but is neither a directory nor a
    /// symbolic link (`ENOTDIR`). Both mean "no such file in the worktree",
    /// like git's `is_missing_file_error`.
    NotFound,
    /// An intermediate component is a symbolic link. A symbolic link in the
    /// last component is not an error ([`SafeRoot::stat`] returns its
    /// metadata, [`SafeRoot::read`] returns [`Read::Symlink`]).
    PathThroughSymlink,
    /// Anything else: permissions, overlong names, IO errors, out of memory.
    Io(io::Error),
}

impl fmt::Display for SafeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SafeError::NotFound => f.write_str("no such file beneath the root"),
            SafeError::PathThroughSymlink => {
                f.write_str("a directory in the path is a symbolic link")
            }
            SafeError::Io(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for SafeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SafeError::Io(error) => Some(error),
            SafeError::NotFound | SafeError::PathThroughSymlink => None,
        }
    }
}

impl From<SafeError> for io::Error {
    /// For callers that only propagate the error: `NotFound` becomes
    /// `ErrorKind::NotFound`, `PathThroughSymlink` becomes `ErrorKind::Other`
    /// carrying the `SafeError`, and `Io(e)` becomes `e`.
    fn from(error: SafeError) -> io::Error {
        match error {
            SafeError::NotFound => io::Error::from(io::ErrorKind::NotFound),
            SafeError::PathThroughSymlink => io::Error::other(error),
            SafeError::Io(error) => error,
        }
    }
}

/// `SafeError::Io` from an errno. A function rather than `From<Errno>`, which
/// would put rustix in the public interface.
fn io_error(errno: Errno) -> SafeError {
    SafeError::Io(errno.into())
}

/// The metadata git's stat comparison needs, at full width. All fields come
/// from one `fstat` or `fstatat(AT_SYMLINK_NOFOLLOW)` call; `kind` is derived
/// from `mode & S_IFMT`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Meta {
    pub kind: FileKind,
    /// Every bit of `st_mode` (type and permissions); macOS's `u16` widened.
    pub mode: u32,
    /// `st_size`. For a symbolic link, the length of its target.
    pub size: u64,
    pub mtime: Timestamp,
    /// `st_ctime`: when the inode last changed, not the creation time (as in
    /// git and gix-index).
    pub ctime: Timestamp,
    /// `st_dev`; macOS's `i32` is widened with `as u32 as u64`.
    pub dev: u64,
    pub ino: u64,
    pub uid: u32,
    pub gid: u32,
}

impl Meta {
    /// A regular file whose owner may execute it (`mode & 0o100`), git's rule
    /// for the executable bit.
    pub fn is_executable(&self) -> bool {
        self.kind == FileKind::File && self.mode & 0o100 != 0
    }

    fn from_stat(st: &Stat) -> Meta {
        Meta {
            kind: FileKind::from_file_type(FileType::from_raw_mode(st.st_mode)),
            mode: widen_u32(st.st_mode),
            size: st.st_size.try_into().unwrap_or(0),
            mtime: Timestamp::new(st.st_mtime, st.st_mtime_nsec),
            ctime: Timestamp::new(st.st_ctime, st.st_ctime_nsec),
            dev: dev(st),
            ino: widen_u64(st.st_ino),
            uid: st.st_uid,
            gid: st.st_gid,
        }
    }
}

// The `stat` field types differ between targets (u16 or u32 modes, u64 or i64
// nanoseconds). These generic conversions compile on every target without
// tripping `useless_conversion` where the types already match.
fn widen_u32<T: Into<u32>>(value: T) -> u32 {
    value.into()
}

fn widen_u64<T: Into<u64>>(value: T) -> u64 {
    value.into()
}

#[cfg(target_vendor = "apple")]
fn dev(st: &Stat) -> u64 {
    u64::from(st.st_dev as u32)
}

#[cfg(not(target_vendor = "apple"))]
fn dev(st: &Stat) -> u64 {
    widen_u64(st.st_dev)
}

/// An instant since 1970-01-01 UTC, like `timespec`: `nanos` is in
/// `0..1_000_000_000`, and `secs` is negative before 1970.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Timestamp {
    pub secs: i64,
    pub nanos: u32,
}

impl Timestamp {
    /// Nanoseconds since the epoch (git's `Content::Unhashed::mtime_ns`).
    pub fn as_nanos(self) -> i128 {
        i128::from(self.secs) * 1_000_000_000 + i128::from(self.nanos)
    }

    fn new<S: Into<i64>, N: TryInto<u32>>(secs: S, nanos: N) -> Timestamp {
        let nanos = nanos.try_into().ok().filter(|&n| n < 1_000_000_000);
        Timestamp {
            secs: secs.into(),
            nanos: nanos.unwrap_or(0),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Fifo,
    Socket,
    CharDevice,
    BlockDevice,
    /// `S_IFMT` is none of the above (such as a macOS whiteout).
    Other,
}

impl FileKind {
    fn from_file_type(file_type: FileType) -> FileKind {
        match file_type {
            FileType::RegularFile => FileKind::File,
            FileType::Directory => FileKind::Dir,
            FileType::Symlink => FileKind::Symlink,
            FileType::Fifo => FileKind::Fifo,
            FileType::Socket => FileKind::Socket,
            FileType::CharacterDevice => FileKind::CharDevice,
            FileType::BlockDevice => FileKind::BlockDevice,
            FileType::Unknown => FileKind::Other,
        }
    }
}

/// One entry of [`SafeRoot::list`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    /// The name in the directory, raw bytes: never empty, `.` or `..`, and
    /// without `/` or NUL.
    pub name: Vec<u8>,
    pub kind: FileKind,
}

/// Checks with `fstat` on an opened descriptor that it is a directory, for
/// `list` and `open_dir`.
fn checked_directory(fd: OwnedFd) -> Result<OwnedFd, SafeError> {
    let meta = Meta::from_stat(&rustix::fs::fstat(&fd).map_err(io_error)?);
    if meta.kind != FileKind::Dir {
        return Err(SafeError::Io(io::ErrorKind::NotADirectory.into()));
    }
    Ok(fd)
}

fn list_entries(mut dir: rustix::fs::Dir) -> Result<Vec<DirEntry>, SafeError> {
    let mut out = Vec::new();
    while let Some(entry) = dir.read() {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let kind = match entry.file_type() {
            FileType::Unknown => {
                let fd = dir.fd().map_err(io_error)?;
                match rustix::fs::statat(fd, name, AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(st) => Meta::from_stat(&st).kind,
                    // Removed since it was listed.
                    Err(Errno::NOENT) => continue,
                    Err(errno) => return Err(io_error(errno)),
                }
            }
            other => FileKind::from_file_type(other),
        };
        out.push(DirEntry {
            name: name.to_vec(),
            kind,
        });
    }
    Ok(out)
}

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)` on the last component.
fn stat_entry(dir: BorrowedFd<'_>, name: &[u8]) -> Result<Meta, SafeError> {
    match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => Ok(Meta::from_stat(&st)),
        Err(Errno::NOENT | Errno::NOTDIR) => Err(SafeError::NotFound),
        Err(errno) => Err(io_error(errno)),
    }
}

/// An entry opened by `read` or `open_file`, classified with `fstat` on the
/// descriptor (or, after the walk, with `fstatat` on its parent).
pub(crate) enum Entry {
    /// A regular file and the `fstat` on its descriptor.
    File {
        fd: OwnedFd,
        meta: Meta,
    },
    Symlink {
        meta: Meta,
        target: Vec<u8>,
    },
    /// The descriptor, if any, is already closed.
    Unsupported {
        meta: Meta,
    },
}

/// Step 2 of `read`: classifies an opened entry with `fstat` on the same
/// descriptor. Anything but a regular file closes it unread.
fn classify(fd: OwnedFd) -> Result<Entry, SafeError> {
    let meta = Meta::from_stat(&rustix::fs::fstat(&fd).map_err(io_error)?);
    if meta.kind != FileKind::File {
        // The descriptor closes here without reading (FIFOs, devices, dirs).
        return Ok(Entry::Unsupported { meta });
    }
    Ok(Entry::File { fd, meta })
}

/// Below this, growing the buffer for a file that has grown since `fstat`
/// (or reports size 0, like procfs) adds at least this much.
const MIN_GROWTH: u64 = 8 * 1024;

/// Where [`read_limited`] reads from.
#[derive(Clone, Copy)]
enum Cursor {
    /// `read` at the descriptor's file offset (`SafeRoot::read`, whose
    /// descriptor is its own).
    Current,
    /// `pread` from offset 0, leaving the file offset alone
    /// (`OpenFile::read_all`, which several threads may call at once).
    Start,
}

/// Reads `fd` to EOF. `size` is the size `fstat` reported (`size <= max`).
/// Returns `None` when there are more than `max` bytes.
fn read_limited(
    fd: BorrowedFd<'_>,
    cursor: Cursor,
    size: u64,
    max: u64,
) -> Result<Option<Vec<u8>>, SafeError> {
    // At most `max` bytes plus the one that shows the file is larger.
    let limit = max.saturating_add(1);
    let mut bytes = Vec::new();
    // One byte past `size`, so the read that finds EOF needs no reallocation.
    reserve_exact(&mut bytes, size.saturating_add(1))?;
    loop {
        if bytes.len() == bytes.capacity() {
            let len = bytes.len() as u64;
            let target = len
                .saturating_mul(2)
                .max(len.saturating_add(MIN_GROWTH))
                .min(limit);
            reserve_exact(&mut bytes, target - len)?;
        }
        let read = match cursor {
            Cursor::Current => rustix::io::read(fd, spare_capacity(&mut bytes)),
            Cursor::Start => {
                let offset = bytes.len() as u64;
                rustix::io::pread(fd, spare_capacity(&mut bytes), offset)
            }
        };
        match read {
            Ok(0) => return Ok(Some(bytes)),
            Ok(_) if bytes.len() as u64 > max => return Ok(None),
            Ok(_) | Err(Errno::INTR) => {}
            Err(errno) => return Err(io_error(errno)),
        }
    }
}

fn reserve_exact(bytes: &mut Vec<u8>, additional: u64) -> Result<(), SafeError> {
    let out_of_memory = || SafeError::Io(io::ErrorKind::OutOfMemory.into());
    let additional = usize::try_from(additional).map_err(|_| out_of_memory())?;
    bytes
        .try_reserve_exact(additional)
        .map_err(|_| out_of_memory())
}
