//! Reflog records, read stamps and the reading progress of each reflog file
//! (docs/design/fs_watch.md「reflog」).
//!
//! Reading uses `std::fs` and stays on the fs_watch thread, so read numbers
//! form a total order. Only regular files in the fixed reflog locations are
//! read; a symbolic link is `Unreadable`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use crate::clock::Clock;
use crate::path::{is_under, is_within};
use crate::types::WorktreeName;

/// A reflog file.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum ReflogFile {
    /// `logs/HEAD` (the main worktree) or `worktrees/<name>/logs/HEAD`.
    Head(WorktreeName),
    /// `logs/refs/heads/<branch>`; the value is the full reference name,
    /// such as `refs/heads/feature/x`.
    Branch(Arc<[u8]>),
    /// `logs/refs/stash`.
    Stash,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReflogRecord {
    pub file: ReflogFile,
    /// Byte offset where this line starts in the file.
    pub offset: u64,
    /// One line, without the trailing newline; parsed by
    /// `git::parse_reflog_line`.
    pub line: Arc<[u8]>,
    /// Increases within one `observed.run` and is shared with
    /// `ReflogReset::seq`: the order records were read in; within a file it
    /// follows the line order. Not compared across runs.
    pub seq: u64,
    /// The end of the read that found this line: the record was written
    /// before it.
    pub observed: ReadStamp,
    /// The start of the previous read of the same file, which did not find
    /// this line: the record was written after it. None for a new file.
    pub after: Option<ReadStamp>,
    pub origin: Origin,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReadStamp {
    /// This run: derived from the wall clock (nanoseconds) and a per-process
    /// random key when the `Watcher` is created; the same for all of its records.
    /// `read` and `seq` restart from 0 in every run. Compared for equality
    /// only; `usage` orders runs by a sequence number it assigns.
    pub run: RunId,
    /// The read's number. All reflog reads of a run happen one after another
    /// on the fs_watch thread: a smaller number is earlier, and a read starts
    /// only after the previous one ended.
    pub read: u64,
    /// Wall clock, nanosecond resolution. The system time may be adjusted;
    /// order by `read`.
    pub at: SystemTime,
}

/// Run ID, the same kind of thing as `Revision::epoch`: tells counters'
/// sources apart, compared for equality only.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RunId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    /// Appended after the part already read.
    Appended,
    /// The file appeared while watching (a new branch, a renamed branch) and
    /// was read from the start. Old records carried over by a rename are in
    /// it too; `usage` deduplicates them by content.
    NewFile,
}

/// Shares the `seq` counter with `ReflogRecord`: merged by `seq` with
/// `reflog`, it gives the order of records and resets in each file, even for
/// "append, reset, append" within one batch. `usage` stores resets with the
/// records so that "no reset in between" can be computed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReflogReset {
    pub file: ReflogFile,
    pub reason: ResetReason,
    pub seq: u64,
    /// The end of the read that found this reset.
    pub observed: ReadStamp,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ResetReason {
    /// The file got shorter or the end of the part already read changed
    /// (`reflog expire`, `reflog delete`). Reading continues from the new
    /// end; old records are not delivered again.
    Rewritten,
    /// The file was deleted (branch deleted, `stash clear`, the old name of
    /// a rename).
    Removed,
    Unreadable(String),
}

/// At most this many bytes per read (and per batch of a subscription).
pub(crate) const READ_LIMIT: usize = 1 << 20;
/// Longer lines are skipped and counted.
pub(crate) const MAX_LINE: usize = 64 << 10;
/// Bytes kept before the offset to detect a rewrite.
const TAIL: usize = 64;

/// Read and sequence numbering of one run.
pub(crate) struct Stamps {
    pub(crate) run: RunId,
    next_read: u64,
    next_seq: u64,
}

impl Stamps {
    pub(crate) fn new(run: RunId) -> Stamps {
        Stamps {
            run,
            next_read: 0,
            next_seq: 0,
        }
    }

    fn start(&mut self, clock: &Clock) -> ReadStamp {
        let read = self.next_read;
        self.next_read += 1;
        ReadStamp {
            run: self.run,
            read,
            at: clock.wall(),
        }
    }

    pub(crate) fn seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }
}

/// What one read of one file produced.
pub(crate) enum ReadOutcome {
    Records {
        records: Vec<ReflogRecord>,
        /// Line bytes in `records`.
        bytes: usize,
        /// The read stopped at `READ_LIMIT`; more complete lines may follow.
        more: bool,
    },
    Reset(ReflogReset),
}

struct Progress {
    offset: u64,
    /// Up to `TAIL` bytes before `offset`.
    tail: Vec<u8>,
    /// The start of the last read that reached the end of the file. None
    /// while a new file is still being read in parts.
    after: Option<ReadStamp>,
    /// Inside a line longer than `MAX_LINE`: skip up to the next newline.
    skipping: bool,
}

/// The reflog progress of one repository state.
pub(crate) struct Reflogs {
    /// False for reftable repositories: their reflogs are in the tables.
    enabled: bool,
    files: BTreeMap<Vec<u8>, Progress>,
    /// Lines longer than `MAX_LINE` skipped so far.
    pub(crate) skipped: u64,
}

impl Reflogs {
    /// Phase 1 (before arm): records the current length of every reflog
    /// file; existing records are not delivered.
    pub(crate) fn baseline(common: &Path, stamps: &mut Stamps, clock: &Clock) -> Reflogs {
        let enabled = fs::symlink_metadata(common.join("reftable/tables.list")).is_err();
        let mut reflogs = Reflogs {
            enabled,
            files: BTreeMap::new(),
            skipped: 0,
        };
        if !enabled {
            return reflogs;
        }
        for rel in list_within(common, b"") {
            let stamp = stamps.start(clock);
            if let Ok((len, tail)) = measure(&common.join(bytes_path(&rel))) {
                reflogs.files.insert(
                    rel,
                    Progress {
                        offset: len,
                        tail,
                        after: Some(stamp),
                        skipping: false,
                    },
                );
            }
        }
        reflogs
    }

    /// The reflog files an event on `q` (relative to the common directory)
    /// may have touched: a reflog file itself, or every known and present
    /// reflog file under a directory. `q` empty means all of them (a rescan).
    pub(crate) fn affected(&self, common: &Path, q: &[u8]) -> Vec<Vec<u8>> {
        if !self.enabled || !is_reflog_location(q) {
            return Vec::new();
        }
        let mut files: BTreeSet<Vec<u8>> = self
            .files
            .keys()
            .filter(|rel| q.is_empty() || is_within(rel, q))
            .cloned()
            .collect();
        files.extend(list_within(common, q));
        files.into_iter().collect()
    }

    /// Reads one file from its progress, at most `limit` bytes. None when
    /// nothing happened (no progress and no file, or not a reflog path).
    pub(crate) fn read(
        &mut self,
        common: &Path,
        rel: &[u8],
        limit: usize,
        stamps: &mut Stamps,
        clock: &Clock,
    ) -> Option<ReadOutcome> {
        let file = reflog_file(rel)?;
        let start = stamps.start(clock);
        let reset = |reason, stamps: &mut Stamps| {
            Some(ReadOutcome::Reset(ReflogReset {
                file: file.clone(),
                reason,
                seq: stamps.seq(),
                observed: ReadStamp {
                    at: clock.wall(),
                    ..start
                },
            }))
        };
        let path = common.join(bytes_path(rel));
        let opened = match open_regular(&path) {
            Ok(opened) => opened,
            Err(Open::Missing) => {
                return match self.files.remove(rel) {
                    Some(_) => reset(ResetReason::Removed, stamps),
                    None => None,
                };
            }
            Err(Open::Directory) => return None,
            Err(Open::Unreadable(message)) => {
                return reset(ResetReason::Unreadable(message), stamps);
            }
        };
        let (mut handle, len) = opened;
        let progress = self.files.entry(rel.to_vec()).or_insert(Progress {
            offset: 0,
            tail: Vec::new(),
            after: None,
            skipping: false,
        });
        if progress.offset > 0 && !tail_matches(&mut handle, progress, len) {
            let tail = read_tail(&mut handle, len).unwrap_or_default();
            *progress = Progress {
                offset: len,
                tail,
                after: Some(start),
                skipping: false,
            };
            return reset(ResetReason::Rewritten, stamps);
        }
        let available = len - progress.offset;
        let want = available.min(limit as u64) as usize;
        let mut buf = vec![0; want];
        let read = handle
            .seek(SeekFrom::Start(progress.offset))
            .and_then(|_| read_up_to(&mut handle, &mut buf));
        let got = match read {
            Ok(got) => got,
            Err(error) => return reset(ResetReason::Unreadable(error.to_string()), stamps),
        };
        buf.truncate(got);
        let mut pos = 0;
        let mut lines = Vec::new();
        if progress.skipping {
            match memchr(b'\n', &buf) {
                Some(newline) => {
                    pos = newline + 1;
                    progress.skipping = false;
                }
                None => pos = buf.len(),
            }
        }
        while let Some(newline) = memchr(b'\n', &buf[pos..]) {
            let line = &buf[pos..pos + newline];
            if line.len() > MAX_LINE {
                self.skipped += 1;
            } else {
                lines.push((progress.offset + pos as u64, line));
            }
            pos += newline + 1;
        }
        if !progress.skipping && buf.len() - pos > MAX_LINE {
            // An unfinished line already over the limit: skip it.
            self.skipped += 1;
            progress.skipping = true;
            pos = buf.len();
        }
        let consumed = &buf[..pos];
        let mut tail = std::mem::take(&mut progress.tail);
        tail.extend_from_slice(&consumed[consumed.len().saturating_sub(TAIL)..]);
        let excess = tail.len().saturating_sub(TAIL);
        tail.drain(..excess);
        progress.tail = tail;
        progress.offset += pos as u64;
        let reached_end = got as u64 == available;
        let observed = ReadStamp {
            at: clock.wall(),
            ..start
        };
        let (origin, after) = match progress.after {
            Some(after) => (Origin::Appended, Some(after)),
            None => (Origin::NewFile, None),
        };
        let mut bytes = 0;
        let records = lines
            .into_iter()
            .map(|(offset, line)| {
                bytes += line.len();
                ReflogRecord {
                    file: file.clone(),
                    offset,
                    line: line.into(),
                    seq: stamps.seq(),
                    observed,
                    after,
                    origin,
                }
            })
            .collect();
        if reached_end {
            progress.after = Some(start);
        }
        Some(ReadOutcome::Records {
            records,
            bytes,
            more: !reached_end,
        })
    }
}

/// Whether an event on `q` concerns reflogs: a reflog file or a directory on
/// the way to one.
pub(crate) fn is_reflog_location(q: &[u8]) -> bool {
    if q.is_empty() || reflog_file(q).is_some() {
        return true;
    }
    if matches!(q, b"logs" | b"logs/refs" | b"worktrees") || is_within(q, b"logs/refs/heads") {
        return true;
    }
    // worktrees/<name> and worktrees/<name>/logs
    let parts: Vec<&[u8]> = q.split(|&b| b == b'/').collect();
    matches!(
        parts.as_slice(),
        [b"worktrees", _] | [b"worktrees", _, b"logs"]
    )
}

/// The reflog file at `rel` (relative to the common directory).
pub(crate) fn reflog_file(rel: &[u8]) -> Option<ReflogFile> {
    match rel {
        b"logs/HEAD" => return Some(ReflogFile::Head(WorktreeName::Main)),
        b"logs/refs/stash" => return Some(ReflogFile::Stash),
        _ => {}
    }
    if is_under(rel, b"logs/refs/heads") {
        return Some(ReflogFile::Branch(rel[b"logs/".len()..].into()));
    }
    let parts: Vec<&[u8]> = rel.split(|&b| b == b'/').collect();
    match parts.as_slice() {
        [b"worktrees", name, b"logs", b"HEAD"] if !name.is_empty() => Some(ReflogFile::Head(
            WorktreeName::Linked(String::from_utf8_lossy(name).into()),
        )),
        _ => None,
    }
}

/// The reflog files present within `q` (empty: everywhere), relative to the
/// common directory.
fn list_within(common: &Path, q: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for fixed in [&b"logs/HEAD"[..], b"logs/refs/stash"] {
        if (q.is_empty() || is_within(fixed, q)) && is_file_or_link(&common.join(bytes_path(fixed)))
        {
            out.push(fixed.to_vec());
        }
    }
    let heads = b"logs/refs/heads";
    if q.is_empty() || is_within(heads, q) {
        walk(common, heads, &mut out);
    } else if is_under(q, heads) {
        if is_file_or_link(&common.join(bytes_path(q))) {
            out.push(q.to_vec());
        } else {
            walk(common, q, &mut out);
        }
    }
    if q.is_empty() || q == b"worktrees" {
        if let Ok(entries) = fs::read_dir(common.join("worktrees")) {
            for entry in entries.flatten() {
                let mut rel = b"worktrees/".to_vec();
                rel.extend_from_slice(entry.file_name().as_encoded_bytes());
                rel.extend_from_slice(b"/logs/HEAD");
                if is_file_or_link(&common.join(bytes_path(&rel))) {
                    out.push(rel);
                }
            }
        }
    } else if is_under(q, b"worktrees") {
        let name = q[b"worktrees/".len()..]
            .split(|&b| b == b'/')
            .next()
            .unwrap_or_default();
        let mut rel = b"worktrees/".to_vec();
        rel.extend_from_slice(name);
        rel.extend_from_slice(b"/logs/HEAD");
        if is_within(&rel, q) && is_file_or_link(&common.join(bytes_path(&rel))) {
            out.push(rel);
        }
    }
    out
}

/// Every file or link under `dir` (relative), recursively; directory links
/// are not followed.
fn walk(common: &Path, dir: &[u8], out: &mut Vec<Vec<u8>>) {
    let Ok(entries) = fs::read_dir(common.join(bytes_path(dir))) else {
        return;
    };
    for entry in entries.flatten() {
        let mut rel = dir.to_vec();
        rel.push(b'/');
        rel.extend_from_slice(entry.file_name().as_encoded_bytes());
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk(common, &rel, out),
            Ok(_) => out.push(rel),
            Err(_) => {}
        }
    }
}

fn is_file_or_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_dir())
}

pub(crate) fn bytes_path(bytes: &[u8]) -> &Path {
    use std::os::unix::ffi::OsStrExt;
    Path::new(std::ffi::OsStr::from_bytes(bytes))
}

enum Open {
    Missing,
    Directory,
    Unreadable(String),
}

/// Opens a regular file that is not a symbolic link: `lstat`, open, then
/// `fstat` must name the same file.
fn open_regular(path: &Path) -> Result<(File, u64), Open> {
    let before = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Open::Missing),
        Err(error) => return Err(Open::Unreadable(error.to_string())),
    };
    if before.is_dir() {
        return Err(Open::Directory);
    }
    if !before.file_type().is_file() {
        return Err(Open::Unreadable("not a regular file".to_owned()));
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Open::Missing),
        Err(error) => return Err(Open::Unreadable(error.to_string())),
    };
    let after = file
        .metadata()
        .map_err(|error| Open::Unreadable(error.to_string()))?;
    if after.dev() != before.dev() || after.ino() != before.ino() || !after.is_file() {
        return Err(Open::Unreadable("replaced while opening".to_owned()));
    }
    Ok((file, after.len()))
}

/// The length and last `TAIL` bytes of a file (phase 1).
fn measure(path: &Path) -> Result<(u64, Vec<u8>), Open> {
    let (mut file, len) = open_regular(path)?;
    let tail = read_tail(&mut file, len).map_err(|error| Open::Unreadable(error.to_string()))?;
    Ok((len, tail))
}

fn read_tail(file: &mut File, end: u64) -> io::Result<Vec<u8>> {
    let start = end.saturating_sub(TAIL as u64);
    let mut buf = vec![0; (end - start) as usize];
    file.seek(SeekFrom::Start(start))?;
    let got = read_up_to(file, &mut buf)?;
    buf.truncate(got);
    Ok(buf)
}

/// Whether the file still holds the bytes before `progress.offset` that the
/// last read saw.
fn tail_matches(file: &mut File, progress: &Progress, len: u64) -> bool {
    if len < progress.offset {
        return false;
    }
    read_tail(file, progress.offset).is_ok_and(|tail| tail == progress.tail)
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn memchr(byte: u8, haystack: &[u8]) -> Option<usize> {
    haystack.iter().position(|&b| b == byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflog_file_locations() {
        assert_eq!(
            reflog_file(b"logs/HEAD"),
            Some(ReflogFile::Head(WorktreeName::Main))
        );
        assert_eq!(reflog_file(b"logs/refs/stash"), Some(ReflogFile::Stash));
        assert_eq!(
            reflog_file(b"logs/refs/heads/feature/x"),
            Some(ReflogFile::Branch(b"refs/heads/feature/x"[..].into()))
        );
        assert_eq!(
            reflog_file(b"worktrees/wt/logs/HEAD"),
            Some(ReflogFile::Head(WorktreeName::Linked("wt".into())))
        );
        for other in [
            &b"logs/refs/heads"[..],
            b"logs/refs/remotes/origin/main",
            b"worktrees/wt/HEAD",
            b"HEAD",
        ] {
            assert_eq!(reflog_file(other), None);
        }
        for dir in [
            &b"logs"[..],
            b"logs/refs",
            b"logs/refs/heads",
            b"logs/refs/heads/feature",
            b"worktrees",
            b"worktrees/wt",
            b"worktrees/wt/logs",
        ] {
            assert!(is_reflog_location(dir));
        }
        assert!(!is_reflog_location(b"logs/refs/remotes"));
        assert!(!is_reflog_location(b"worktrees/wt/index"));
    }
}
