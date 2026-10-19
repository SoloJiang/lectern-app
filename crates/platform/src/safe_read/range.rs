//! Range reads (「范围读取」 in docs/design/platform.md): [`SafeRoot::open_file`]
//! hands the descriptor of a regular file to an [`OpenFile`], which reads
//! byte ranges with `pread` and reports the `fstat` taken after each read, so
//! the caller can tell whether two reads saw the same version of the file
//! ([`FileIdentity`]).
//!
//! No read uses or moves the descriptor's file offset, so any number of
//! threads may read the same `OpenFile` at once. Nothing is mapped into
//! memory: a file truncated while it is read gives a short `pread`, never
//! `SIGBUS`.
//!
//! [`SafeRoot::open_file`]: super::SafeRoot::open_file

use std::io;
use std::ops::Range;
use std::os::fd::{AsFd, OwnedFd};

use rustix::buffer::spare_capacity;
use rustix::io::Errno;

use super::{Cursor, MIN_GROWTH, Meta, Timestamp, read_limited, reserve_exact};

/// The most ranges one [`OpenFile::read_ranges`] call takes.
pub const MAX_RANGES: usize = 4;

/// The most bytes one [`OpenFile::read_ranges`] call takes: the sum of the
/// ranges' lengths as requested.
pub const MAX_RANGE_BYTES: u64 = 1 << 20;

/// `pread` takes a signed `off_t`: no range ends past this.
const MAX_END: u64 = i64::MAX as u64;

/// Past the size `fstat` reported at open, the bytes beyond a full buffer
/// are first read into a stack buffer of this size: a range that ends at EOF
/// then allocates nothing for the read that finds it. Not more than
/// [`MIN_GROWTH`], so one growth step always holds what the probe read.
const PROBE: usize = 4096;

/// The result of [`SafeRoot::open_file`](super::SafeRoot::open_file).
#[derive(Debug)]
pub enum Opened {
    File(OpenFile),
    /// As [`Read::Symlink`](super::Read::Symlink).
    Symlink {
        meta: Meta,
        target: Vec<u8>,
    },
    /// As [`Read::Unsupported`](super::Read::Unsupported).
    Unsupported {
        meta: Meta,
    },
}

/// An opened regular file. `Send + Sync`, not `Clone`: share it through an
/// `Arc`. Dropping it closes the descriptor.
///
/// Every read uses `pread`; none uses or moves the descriptor's file offset,
/// so any number of threads may call it at once. Its methods block on system
/// calls and disk IO: call them from a background task, never from the UI
/// thread.
#[derive(Debug)]
pub struct OpenFile {
    /// Opened with `O_CLOEXEC`; no other descriptor is ever opened for it.
    pub(super) fd: OwnedFd,
    /// The `fstat` on `fd` right after it was opened.
    meta: Meta,
}

impl OpenFile {
    pub(super) fn new(fd: OwnedFd, meta: Meta) -> OpenFile {
        OpenFile { fd, meta }
    }

    /// The `fstat` on this descriptor when it was opened; `kind` is always
    /// [`FileKind::File`](super::FileKind::File). No system call.
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// The identity at open, `self.meta().identity()`: compare the identity
    /// of each later read with it.
    pub fn identity(&self) -> FileIdentity {
        self.meta.identity()
    }

    /// Reads byte ranges, then `fstat`s the same descriptor once.
    ///
    /// Takes 1 to [`MAX_RANGES`] ranges, each with
    /// `start <= end <= i64::MAX`, whose lengths add up to at most
    /// [`MAX_RANGE_BYTES`]; otherwise `ErrorKind::InvalidInput` without any
    /// system call. Ranges may overlap and come in any order: overlapping or
    /// adjacent ones are read with one `pread`. `pieces[i]` holds the bytes
    /// from `ranges[i].start`, cut at EOF; empty when the range starts at or
    /// past EOF.
    pub fn read_ranges(&self, ranges: &[Range<u64>]) -> io::Result<RangeRead> {
        check(ranges)?;
        let segments = plan(ranges);
        let mut buffers = Vec::with_capacity(segments.len());
        for segment in &segments {
            buffers.push(self.read_segment(segment.clone())?);
        }
        let pieces = pieces(ranges, &segments, buffers);
        Ok(RangeRead {
            pieces,
            meta: self.stat()?,
        })
    }

    /// Reads from offset 0 to EOF, at most `max` bytes inclusive, by the
    /// rules of `SafeRoot::read`, with `pread`; then `fstat`s once. A file
    /// whose size at open exceeds `max` is not read.
    pub fn read_all(&self, max: u64) -> io::Result<FullRead> {
        if self.meta.size > max {
            return Ok(FullRead::TooLarge { meta: self.meta });
        }
        let bytes = read_limited(self.fd.as_fd(), Cursor::Start, self.meta.size, max)?;
        let meta = self.stat()?;
        Ok(match bytes {
            Some(bytes) => FullRead::File { bytes, meta },
            None => FullRead::TooLarge { meta },
        })
    }

    /// `fstat` on this descriptor.
    pub fn stat(&self) -> io::Result<Meta> {
        Ok(Meta::from_stat(&rustix::fs::fstat(&self.fd)?))
    }

    /// One `pread` (more after short reads) of `segment`, cut at EOF. The
    /// buffer starts at what the size at open leaves of the segment, so a
    /// sparse file is never allocated by its size; it grows, up to the
    /// segment's length, only when the file has grown since.
    fn read_segment(&self, segment: Range<u64>) -> io::Result<Vec<u8>> {
        let fd = self.fd.as_fd();
        let len = segment.end - segment.start;
        let mut bytes = Vec::new();
        reserve_exact(
            &mut bytes,
            len.min(self.meta.size.saturating_sub(segment.start)),
        )?;
        loop {
            let filled = bytes.len() as u64;
            if filled >= len {
                break;
            }
            let offset = segment.start + filled;
            if bytes.len() < bytes.capacity() {
                match rustix::io::pread(fd, spare_capacity(&mut bytes), offset) {
                    Ok(0) => break,
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(errno) => return Err(errno.into()),
                }
                continue;
            }
            // The buffer is full but the segment is not: EOF, or the file
            // has grown since it was opened.
            let mut probe = [0; PROBE];
            let want = (len - filled).min(PROBE as u64) as usize;
            match rustix::io::pread(fd, &mut probe[..want], offset) {
                Ok(0) => break,
                Ok(n) => {
                    let target = filled.saturating_mul(2).max(filled + MIN_GROWTH).min(len);
                    reserve_exact(&mut bytes, target - filled)?;
                    bytes.extend_from_slice(&probe[..n]);
                }
                Err(Errno::INTR) => {}
                Err(errno) => return Err(errno.into()),
            }
        }
        // The capacity never exceeds the segment; this only keeps the
        // contract should an allocator ever hand out more.
        bytes.truncate(usize::try_from(len).unwrap_or(usize::MAX));
        Ok(bytes)
    }
}

/// Step 1 of `read_ranges`: the count, bounds and total length.
fn check(ranges: &[Range<u64>]) -> io::Result<()> {
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidInput, message);
    if ranges.is_empty() || ranges.len() > MAX_RANGES {
        return Err(invalid("read_ranges takes 1 to MAX_RANGES ranges"));
    }
    let mut total: u64 = 0;
    for range in ranges {
        if range.start > range.end || range.end > MAX_END {
            return Err(invalid("a range must have start <= end <= i64::MAX"));
        }
        total = total.saturating_add(range.end - range.start);
    }
    if total > MAX_RANGE_BYTES {
        return Err(invalid("the ranges add up to more than MAX_RANGE_BYTES"));
    }
    Ok(())
}

/// Step 2 of `read_ranges`, a pure function: the ranges sorted by start,
/// with overlapping or adjacent ones merged. Each segment is one `pread`;
/// every range lies within exactly one segment. An empty range apart from
/// the others stays an empty segment, which is not read.
pub(crate) fn plan(ranges: &[Range<u64>]) -> Vec<Range<u64>> {
    let mut sorted = ranges.to_vec();
    sorted.sort_unstable_by_key(|range| range.start);
    let mut segments: Vec<Range<u64>> = Vec::with_capacity(sorted.len());
    for range in sorted {
        match segments.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => segments.push(range),
        }
    }
    segments
}

/// Each range's bytes, taken from its segment's buffer. A buffer is moved
/// rather than copied when its last range spans all of it.
fn pieces(ranges: &[Range<u64>], segments: &[Range<u64>], buffers: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let owner = |range: &Range<u64>| {
        segments
            .iter()
            .position(|s| s.start <= range.start && range.end <= s.end)
            .expect("every range lies within a segment of its plan")
    };
    let owners: Vec<usize> = ranges.iter().map(owner).collect();
    let mut users = vec![0usize; segments.len()];
    for &i in &owners {
        users[i] += 1;
    }
    let mut buffers: Vec<Option<Vec<u8>>> = buffers.into_iter().map(Some).collect();
    ranges
        .iter()
        .zip(owners)
        .map(|(range, i)| {
            users[i] -= 1;
            let buffer = buffers[i].as_deref().unwrap_or_default();
            let at = |offset: u64| {
                usize::try_from(offset - segments[i].start)
                    .unwrap_or(usize::MAX)
                    .min(buffer.len())
            };
            let (from, to) = (at(range.start), at(range.end));
            if users[i] == 0 && from == 0 && to == buffer.len() {
                buffers[i].take().unwrap_or_default()
            } else {
                buffer[from..to].to_vec()
            }
        })
        .collect()
}

/// The result of [`OpenFile::read_ranges`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RangeRead {
    /// One per requested range, in the same order: the bytes from
    /// `ranges[i].start`, cut at EOF, no longer than the range; empty when
    /// it starts at or past EOF.
    pub pieces: Vec<Vec<u8>>,
    /// The `fstat` after all ranges were read.
    pub meta: Meta,
}

/// The result of [`OpenFile::read_all`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FullRead {
    /// `meta` is the `fstat` after the read.
    File { bytes: Vec<u8>, meta: Meta },
    /// Larger than `max`: already at open (not read; `meta` is
    /// [`OpenFile::meta`]), or while reading (`meta` is the `fstat` after
    /// the read).
    TooLarge { meta: Meta },
}

/// What tells whether a file changed between two reads. Equal identities
/// before and after a read mean the bytes belong to the version that was
/// opened, up to the timestamps' resolution (「文件身份」 in the design).
/// `ctime` catches tools that set `mtime` back: user space cannot set it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: Timestamp,
    pub ctime: Timestamp,
}

impl Meta {
    /// The fields of this `Meta` that [`FileIdentity`] compares.
    pub fn identity(&self) -> FileIdentity {
        FileIdentity {
            dev: self.dev,
            ino: self.ino,
            size: self.size,
            mtime: self.mtime,
            ctime: self.ctime,
        }
    }
}
