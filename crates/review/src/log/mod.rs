//! The annotation log (docs/adr/0044.md, docs/design/review.md「标注日志」):
//! the user's seen marks and session bases, one append-only JSON lines file
//! per repository under the app data directory's `review/`.
//!
//! - [`LogFile`] does the IO: the whole-file read at open, appends followed
//!   by `fdatasync`, compaction by atomic rename. Blocking; executor tasks
//!   only, never the UI thread.
//! - [`Index`] is the in-memory index: per review target and file identity
//!   the latest `seen` or `unseen`, per session its `base`, plus the records
//!   this version does not know, kept verbatim. Memory only.
//!
//! Compaction rewrites the file from the index, so it runs only when the
//! index is the file: built by one complete, successful read of it plus this
//! process's own appends since, with the file's device, inode and length
//! still what that read and those appends left (ADR 0044「关键约束」).
//! Anything else (a failed or partial read, a failed append, another
//! writer) only appends until the next complete read.

mod wire;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::changes::ChangePath;
use lectern_services::project::BlobId;
use lectern_services::review::FileKey;
use lectern_services::session::Timestamp;
use sha2::{Digest, Sha256};

use crate::seen::{SeenKey, TargetKey};

/// A file above this, and above twice the length this process's last
/// compaction left (ADR 0045), is compacted on the next write.
pub(crate) const COMPACT_ABOVE: u64 = 4 << 20;
/// At most this much of the end of the file is read.
pub(crate) const READ_MAX: u64 = 64 << 20;
/// How much of the start of the file is read for the header when the rest
/// is over `READ_MAX`.
const HEADER_MAX: u64 = 64 << 10;
/// `seen` and `unseen` records older than this are dropped by compaction.
const RETAIN_MS: i64 = 365 * 24 * 60 * 60 * 1000;

/// One record.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Record {
    Seen {
        at: Timestamp,
        key: SeenKey,
    },
    /// Cancels the earlier marks of `file` in `target`.
    Unseen {
        at: Timestamp,
        target: TargetKey,
        file: FileKey,
    },
    Base(BaseRecord),
}

/// A session's base, fixed the first time one of its files is marked seen.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct BaseRecord {
    pub at: Timestamp,
    pub session: SessionId,
    /// None: the empty tree (HEAD was unborn).
    pub commit: Option<BlobId>,
    /// The time of the base entry in the HEAD reflog, Unix seconds; None
    /// when there was none.
    pub entry_at: Option<i64>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct SessionId {
    pub source: Arc<str>,
    pub id: Arc<str>,
}

impl Record {
    fn at(&self) -> Timestamp {
        match self {
            Record::Seen { at, .. } | Record::Unseen { at, .. } => *at,
            Record::Base(b) => b.at,
        }
    }
}

/// The in-memory index of one repository's log.
#[derive(Clone, Default, Debug)]
pub(crate) struct Index {
    /// Per review target, per file identity (the new path, or the old one
    /// when there is none): the latest `seen` or `unseen`.
    marks: HashMap<TargetKey, HashMap<ChangePath, Entry>>,
    /// Per session, its first `base` record.
    bases: HashMap<SessionId, Entry>,
    /// Lines with a record type this version does not know, verbatim
    /// (without the newline).
    unknown: Vec<(u64, Arc<[u8]>)>,
    /// The position of the next record, so that compaction keeps the order.
    next: u64,
    /// Recorded in memory, not yet appended.
    pending: Vec<Record>,
}

#[derive(Clone, Debug)]
struct Entry {
    seq: u64,
    record: Record,
}

impl Index {
    /// The latest `seen` or `unseen` of the file `identity` in `target`.
    pub(crate) fn mark(&self, target: &TargetKey, identity: &ChangePath) -> Option<&Record> {
        self.marks.get(target)?.get(identity).map(|e| &e.record)
    }

    /// The session's base, the first one recorded.
    pub(crate) fn base(&self, session: &SessionId) -> Option<&BaseRecord> {
        match &self.bases.get(session)?.record {
            Record::Base(b) => Some(b),
            _ => None,
        }
    }

    /// The records kept: marks, bases and unknown records.
    pub(crate) fn len(&self) -> usize {
        self.marks.values().map(HashMap::len).sum::<usize>() + self.bases.len() + self.unknown.len()
    }

    /// Records `record` in memory and queues it for the next append.
    pub(crate) fn record(&mut self, record: Record) {
        self.apply(record.clone());
        self.pending.push(record);
    }

    /// The queued records, in order; the queue is left empty.
    pub(crate) fn take_pending(&mut self) -> Vec<Record> {
        std::mem::take(&mut self.pending)
    }

    /// Puts back a batch whose append failed, before anything queued since.
    pub(crate) fn restore_pending(&mut self, mut batch: Vec<Record>) {
        batch.append(&mut self.pending);
        self.pending = batch;
    }

    pub(crate) fn pending(&self) -> &[Record] {
        &self.pending
    }

    /// Keeps `record` as the latest of its file. A `seen` or `unseen` of a
    /// rename also retires the record under its old path: that change is now
    /// this rename, so a later file at the old path is not the same change,
    /// and an `unseen` of the rename is not undone by the old path's `seen`.
    /// Replaying the file gives the same index, so compaction needs no
    /// tombstone.
    fn apply(&mut self, record: Record) {
        let seq = self.next;
        self.next += 1;
        let (target, old, new) = match &record {
            Record::Seen { key, .. } => (&key.target, &key.old_path, &key.new_path),
            Record::Unseen { target, file, .. } => (target, &file.old, &file.new),
            Record::Base(b) => {
                self.bases
                    .entry(b.session.clone())
                    .or_insert(Entry { seq, record });
                return;
            }
        };
        // The decoder and `SeenKey` never give a record without a path.
        let Some(identity) = new.as_ref().or(old.as_ref()).cloned() else {
            return;
        };
        let retired = match (old, new) {
            (Some(old), Some(new)) if old != new => Some(old.clone()),
            _ => None,
        };
        let files = self.marks.entry(target.clone()).or_default();
        if let Some(old) = retired {
            files.remove(&old);
        }
        files.insert(identity, Entry { seq, record });
    }

    fn keep_unknown(&mut self, line: &[u8]) {
        let seq = self.next;
        self.next += 1;
        self.unknown.push((seq, line.into()));
    }

    /// The file compaction writes: the header, then every record kept that
    /// is not an expired mark, in their original order.
    fn compacted(&self, repo: &[u8], now: Timestamp) -> Vec<u8> {
        enum Line<'a> {
            Record(&'a Record),
            Raw(&'a [u8]),
        }
        let cutoff = now.0.saturating_sub(RETAIN_MS);
        let mut lines: Vec<(u64, Line<'_>)> = Vec::with_capacity(self.len());
        for e in self.marks.values().flat_map(HashMap::values) {
            if e.record.at().0 >= cutoff {
                lines.push((e.seq, Line::Record(&e.record)));
            }
        }
        lines.extend(
            self.bases
                .values()
                .map(|e| (e.seq, Line::Record(&e.record))),
        );
        lines.extend(self.unknown.iter().map(|(seq, raw)| (*seq, Line::Raw(raw))));
        lines.sort_unstable_by_key(|(seq, _)| *seq);
        let mut out = wire::header(repo);
        for (_, line) in lines {
            match line {
                Line::Record(r) => wire::encode(r, &mut out),
                Line::Raw(raw) => {
                    out.extend_from_slice(raw);
                    out.push(b'\n');
                }
            }
        }
        out
    }

    /// Drops the marks compaction dropped.
    fn prune(&mut self, now: Timestamp) {
        let cutoff = now.0.saturating_sub(RETAIN_MS);
        self.marks.retain(|_, files| {
            files.retain(|_, e| e.record.at().0 >= cutoff);
            !files.is_empty()
        });
    }
}

/// The size limits; tests use small ones.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub read_max: u64,
    pub compact_above: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            read_max: READ_MAX,
            compact_above: COMPACT_ABOVE,
        }
    }
}

/// An IO step tests can make fail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Op {
    Read,
    Append,
    /// Halfway through writing compaction's temporary file.
    CompactWrite,
    CompactRename,
    /// The directory sync after compaction's rename.
    CompactSyncDir,
}

type Faults = Arc<dyn Fn(Op) -> io::Result<()> + Send + Sync>;

/// What this process knows of the file on disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Known {
    /// Nothing reliable: before a read, after a failed one or a failed
    /// append, or after another writer.
    Unknown,
    /// Absent at the last read.
    Absent,
    /// Its identity and length after the last read or own append, and
    /// where an unfinished last line starts (cut before the next append).
    File {
        dev: u64,
        ino: u64,
        len: u64,
        torn_at: Option<u64>,
    },
}

/// What a read found.
#[derive(Debug, Default)]
pub(crate) struct ReadReport {
    /// The index is the whole file: read to its end without an IO error and
    /// without the `READ_MAX` limit, with this repository's header of this
    /// version. Compaction is allowed.
    pub complete: bool,
    /// Lines skipped as malformed (the header included).
    pub bad_lines: u32,
    /// Records of types this version does not know, kept.
    pub unknown: u32,
    /// The last line had no newline; it was dropped and is cut before the
    /// next append.
    pub torn: bool,
    /// The read failed; the index is empty.
    pub error: Option<io::Error>,
    /// The file had another repository's header and was renamed here.
    pub foreign: Option<PathBuf>,
}

/// What a [`flush`] did.
#[derive(Debug)]
pub(crate) struct Flushed {
    pub appended: usize,
    pub compacted: bool,
    /// The records were appended but compaction failed; the file is as the
    /// append left it.
    pub compaction_error: Option<io::Error>,
}

/// One repository's log file.
pub(crate) struct LogFile {
    dir: PathBuf,
    path: PathBuf,
    /// The repository's git common directory, as written in the header.
    repo: PathBuf,
    limits: Limits,
    faults: Option<Faults>,
    known: Known,
    /// The index the caller holds came from a complete read of this file
    /// plus this process's appends since.
    complete: bool,
    /// The length the last compaction by this process left; 0 before one.
    /// The next compaction waits until the file is twice that.
    compacted_len: u64,
    /// A compaction's rename whose directory sync failed: the next append
    /// syncs the directory again.
    dir_unsynced: bool,
}

/// `SHA-256("repo\0" + common_dir)`, first 16 bytes in hex, plus `.jsonl`:
/// the name `cache` gives the repository's partition, with another
/// extension.
pub(crate) fn file_name(common_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"repo\0");
    hasher.update(common_dir.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let mut name = String::with_capacity(38);
    for byte in &digest[..16] {
        name.push_str(&format!("{byte:02x}"));
    }
    name.push_str(".jsonl");
    name
}

impl LogFile {
    /// The log of the repository whose git common directory is `common_dir`
    /// (absolute and canonical), in `data_dir`. No IO: the directory is
    /// created by the first append.
    pub(crate) fn new(data_dir: &Path, common_dir: &Path) -> LogFile {
        LogFile {
            dir: data_dir.to_path_buf(),
            path: data_dir.join(file_name(common_dir)),
            repo: common_dir.to_path_buf(),
            limits: Limits::default(),
            faults: None,
            known: Known::Unknown,
            complete: false,
            compacted_len: 0,
            dir_unsynced: false,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    #[cfg(test)]
    pub(crate) fn set_faults(
        &mut self,
        faults: impl Fn(Op) -> io::Result<()> + Send + Sync + 'static,
    ) {
        self.faults = Some(Arc::new(faults));
    }

    fn fault(&self, op: Op) -> io::Result<()> {
        self.faults.as_ref().map_or(Ok(()), |f| f(op))
    }

    fn repo_bytes(&self) -> &[u8] {
        self.repo.as_os_str().as_encoded_bytes()
    }

    /// Reads the whole file (at most its last `READ_MAX`) into a new index.
    /// Malformed lines are skipped and counted, unknown record types kept,
    /// an unfinished last line dropped. A file with another repository's
    /// header is renamed to `<name>.foreign-<now>` and not read. On an IO
    /// error the index is empty and `error` set. Blocking.
    pub(crate) fn load(&mut self, now: Timestamp) -> (Index, ReadReport) {
        self.known = Known::Unknown;
        self.complete = false;
        let mut index = Index::default();
        let mut report = ReadReport::default();
        if let Err(error) = self.read_into(&mut index, &mut report, now) {
            self.known = Known::Unknown;
            self.complete = false;
            let report = ReadReport {
                error: Some(error),
                ..ReadReport::default()
            };
            return (Index::default(), report);
        }
        report.complete = self.complete;
        (index, report)
    }

    fn read_into(
        &mut self,
        index: &mut Index,
        report: &mut ReadReport,
        now: Timestamp,
    ) -> io::Result<()> {
        self.fault(Op::Read)?;
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.known = Known::Absent;
                self.complete = true;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(not_a_file());
        }
        let (dev, ino, len) = (meta.dev(), meta.ino(), meta.len());
        let start = len.saturating_sub(self.limits.read_max);
        let buf = read_at(&file, start, len - start)?;
        let end = start + buf.len() as u64;

        // The header line and where the records start.
        let (head, body, body_at) = if start == 0 {
            match newline(&buf) {
                Some(i) => (buf[..i].to_vec(), &buf[i + 1..], i as u64 + 1),
                None => {
                    // Empty, or a single unfinished line (the header's
                    // first write was cut short): nothing to read.
                    report.torn = !buf.is_empty();
                    let torn_at = report.torn.then_some(0);
                    self.known = Known::File {
                        dev,
                        ino,
                        len: end,
                        torn_at,
                    };
                    self.complete = end == len;
                    return Ok(());
                }
            }
        } else {
            let first = read_at(&file, 0, HEADER_MAX.min(len))?;
            let head = newline(&first).map_or(Vec::new(), |i| first[..i].to_vec());
            // The first line of the tail started before it: skip it.
            match newline(&buf) {
                Some(i) => (head, &buf[i + 1..], start + i as u64 + 1),
                None => (head, &buf[buf.len()..], end),
            }
        };

        let header_ok = match serde_json::from_slice::<wire::Header<'_>>(&head) {
            Ok(h) if h.lectern == wire::MAGIC => {
                if &h.repo.0[..] != self.repo_bytes() {
                    drop(file);
                    let foreign = self.path.with_file_name(format!(
                        "{}.foreign-{}",
                        file_name_of(&self.path),
                        now.0
                    ));
                    fs::rename(&self.path, &foreign)?;
                    report.foreign = Some(foreign);
                    self.known = Known::Absent;
                    self.complete = true;
                    return Ok(());
                }
                h.v == wire::VERSION
            }
            _ => {
                report.bad_lines += 1;
                false
            }
        };

        let mut rest = body;
        let mut at = body_at;
        let mut torn_at = None;
        let mut strs = wire::Interner::default();
        while !rest.is_empty() {
            let Some(i) = newline(rest) else {
                torn_at = Some(at);
                report.torn = true;
                break;
            };
            read_line(&rest[..i], &mut strs, index, report);
            rest = &rest[i + 1..];
            at += i as u64 + 1;
        }

        // A tail with no newline at all cannot be cut: where its line
        // starts is before the part read.
        let cuttable = start == 0 || newline(&buf).is_some();
        self.known = if end == len && cuttable {
            Known::File {
                dev,
                ino,
                len,
                torn_at,
            }
        } else {
            Known::Unknown
        };
        self.complete = start == 0 && end == len && header_ok;
        Ok(())
    }

    /// Appends `records` and `fdatasync`s. Before them: the header when the
    /// file is new or empty; the unfinished last line cut off when this
    /// process knows where it starts, otherwise a newline when the file does
    /// not end with one, so that the records start on a line of their own.
    /// Any failure, or a file that changed since this process last saw it,
    /// stops compaction until the next complete read. Blocking.
    pub(crate) fn append(&mut self, records: &[Record]) -> io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let result = self.append_inner(records);
        if result.is_err() {
            self.known = Known::Unknown;
            self.complete = false;
        }
        result
    }

    fn append_inner(&mut self, records: &[Record]) -> io::Result<()> {
        self.fault(Op::Append)?;
        let mut options = OpenOptions::new();
        options.read(true).append(true);
        let (file, created) = match options.open(&self.path) {
            Ok(file) => (file, false),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&self.dir)?;
                (options.create(true).open(&self.path)?, true)
            }
            Err(e) => return Err(e),
        };
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(not_a_file());
        }
        let (dev, ino, mut len) = (meta.dev(), meta.ino(), meta.len());
        let tail = match self.known {
            Known::File {
                dev: d,
                ino: i,
                len: l,
                torn_at,
            } if (d, i, l) == (dev, ino, len) => Some(torn_at),
            Known::Absent if len == 0 => Some(None),
            _ => None,
        };
        let mut out = Vec::new();
        match tail {
            Some(Some(torn_at)) => {
                file.set_len(torn_at)?;
                len = torn_at;
            }
            Some(None) => {}
            None => {
                self.complete = false;
                if len > 0 && !ends_with_newline(&file, len) {
                    out.push(b'\n');
                }
            }
        }
        if len == 0 {
            out.extend_from_slice(&wire::header(self.repo_bytes()));
        }
        for record in records {
            wire::encode(record, &mut out);
        }
        (&file).write_all(&out)?;
        file.sync_data()?;
        if self.dir_unsynced {
            self.dir_unsynced = sync_dir(&self.dir).is_err();
        } else if created {
            // Best effort: the new entry survives a crash once the
            // directory is synced; until then the marks may be lost, which
            // is all a failure here could do anyway.
            let _ = sync_dir(&self.dir);
        }
        self.known = Known::File {
            dev,
            ino,
            len: len + out.len() as u64,
            torn_at: None,
        };
        Ok(())
    }

    /// Whether the next [`LogFile::compact`] would rewrite the file: over
    /// `max(compact_above, 2 × the length the last compaction left)`, and
    /// the compaction precondition holds as far as this process knows. The
    /// doubling keeps a log whose live records alone are over the limit
    /// from being rewritten on every write (ADR 0045): rewrites stay
    /// amortized, and while compaction is allowed the file stays under
    /// `max(compact_above, twice its live size)` plus one batch.
    pub(crate) fn should_compact(&self) -> bool {
        let doubled = self.compacted_len.saturating_mul(2);
        let above = self.limits.compact_above.max(doubled);
        self.complete && matches!(self.known, Known::File { len, torn_at: None, .. } if len > above)
    }

    /// When [`LogFile::should_compact`] and the file on disk still has the
    /// identity and length this process left it with: writes the header and
    /// `index`'s records (marks older than a year dropped) to a temporary
    /// file, `fsync`s it, renames it over the log and `fsync`s the
    /// directory, then drops the expired marks from `index` too. On an
    /// error the log is unchanged. Returns whether it compacted. Blocking.
    ///
    /// `index` must be the one this file's last [`LogFile::load`] returned,
    /// with every record appended since recorded in it.
    pub(crate) fn compact(&mut self, index: &mut Index, now: Timestamp) -> io::Result<bool> {
        if !self.should_compact() {
            return Ok(false);
        }
        let Known::File { dev, ino, len, .. } = self.known else {
            return Ok(false);
        };
        let meta = fs::symlink_metadata(&self.path)?;
        if !meta.is_file() || (meta.dev(), meta.ino(), meta.len()) != (dev, ino, len) {
            // Another writer: the index is no longer the file.
            self.known = Known::Unknown;
            self.complete = false;
            return Ok(false);
        }
        let bytes = index.compacted(self.repo_bytes(), now);
        let temp = self
            .path
            .with_file_name(format!("{}.tmp", file_name_of(&self.path)));
        let meta = match self.write_replacement(&temp, &bytes) {
            Ok(meta) => meta,
            Err(e) => {
                let _ = fs::remove_file(&temp);
                return Err(e);
            }
        };
        self.compacted_len = bytes.len() as u64;
        index.prune(now);
        let synced = self.fault(Op::CompactSyncDir);
        if synced.and_then(|()| sync_dir(&self.dir)).is_err() {
            // The rename may not be durable yet: a crash could bring back
            // the old file. Treat the file as unknown (append only until the
            // next complete read) and sync the directory again with the
            // next append.
            self.known = Known::Unknown;
            self.complete = false;
            self.dir_unsynced = true;
        } else {
            self.known = Known::File {
                dev: meta.dev(),
                ino: meta.ino(),
                len: bytes.len() as u64,
                torn_at: None,
            };
        }
        Ok(true)
    }

    fn write_replacement(&self, temp: &Path, bytes: &[u8]) -> io::Result<fs::Metadata> {
        // A leftover of a run that stopped halfway.
        let _ = fs::remove_file(temp);
        let mut file = OpenOptions::new().write(true).create_new(true).open(temp)?;
        let half = bytes.len() / 2;
        file.write_all(&bytes[..half])?;
        self.fault(Op::CompactWrite)?;
        file.write_all(&bytes[half..])?;
        file.sync_all()?;
        let meta = file.metadata()?;
        drop(file);
        self.fault(Op::CompactRename)?;
        fs::rename(temp, &self.path)?;
        Ok(meta)
    }
}

/// Appends the queued records of `index` to `file` and compacts when due.
/// When the append fails the records go back to the front of the queue and
/// the error is returned; a failed compaction is reported in `Flushed`.
pub(crate) fn flush(file: &mut LogFile, index: &mut Index, now: Timestamp) -> io::Result<Flushed> {
    let batch = index.take_pending();
    if let Err(e) = file.append(&batch) {
        index.restore_pending(batch);
        return Err(e);
    }
    let (compacted, compaction_error) = match file.compact(index, now) {
        Ok(compacted) => (compacted, None),
        Err(e) => (false, Some(e)),
    };
    Ok(Flushed {
        appended: batch.len(),
        compacted,
        compaction_error,
    })
}

fn read_line(line: &[u8], strs: &mut wire::Interner, index: &mut Index, report: &mut ReadReport) {
    if line.iter().all(u8::is_ascii_whitespace) {
        // The newline put before an append onto a file of unknown state.
        return;
    }
    match wire::decode(line, strs) {
        wire::Decoded::Record(record) => index.apply(record),
        wire::Decoded::Unknown => {
            index.keep_unknown(line);
            report.unknown += 1;
        }
        wire::Decoded::Bad => report.bad_lines += 1,
    }
}

fn not_a_file() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "not a regular file")
}

fn newline(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|&b| b == b'\n')
}

/// Up to `len` bytes from `offset`; fewer when the file is shorter.
fn read_at(file: &File, offset: u64, len: u64) -> io::Result<Vec<u8>> {
    let mut buf = vec![0; len as usize];
    let mut filled = 0;
    while filled < buf.len() {
        match file.read_at(&mut buf[filled..], offset + filled as u64) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

fn ends_with_newline(file: &File, len: u64) -> bool {
    let mut last = [0];
    matches!(file.read_at(&mut last, len - 1), Ok(1)) && last[0] == b'\n'
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
