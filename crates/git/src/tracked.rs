//! The index's path list (docs/design/git.md「index」).

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::handle::Handle;
use crate::{GitError, HashKind, Mode, Oid, RepoPath, tree};

/// Bounds on how many entries a call may produce.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EntryLimits {
    /// The most entries in the result: the index's own entries plus those
    /// expanded from sparse directory entries. Above it the call returns
    /// `GitError::TooManyEntries`.
    pub entries: usize,
}

impl Default for EntryLimits {
    /// One million: ten times the largest repository in the performance
    /// budget (100 000 tracked files), about 100 MB of paths.
    fn default() -> Self {
        EntryLimits { entries: 1_000_000 }
    }
}

/// Every path in a worktree's index.
#[derive(Debug, PartialEq, Eq)]
pub struct TrackedPaths {
    checksum: Option<Oid>,
    entries: Vec<TrackedEntry>,
    invalid: Vec<Arc<[u8]>>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TrackedEntry {
    pub path: RepoPath,
    pub mode: Mode,
    pub oid: Oid,
    /// A skip-worktree entry, or one expanded from a sparse directory entry:
    /// not in the worktree, treated as unchanged, never reported as deleted.
    pub skip_worktree: bool,
}

impl TrackedPaths {
    pub(crate) fn empty() -> TrackedPaths {
        TrackedPaths {
            checksum: None,
            entries: Vec::new(),
            invalid: Vec::new(),
        }
    }

    /// The checksum at the end of the index file (in the index's hash
    /// algorithm), the cache key for data derived from the file list; None
    /// without an index file.
    pub fn checksum(&self) -> Option<Oid> {
        self.checksum
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// In path byte order; each conflicted path once. Sparse directory
    /// entries are already expanded into their files.
    pub fn iter(&self) -> impl Iterator<Item = &TrackedEntry> {
        self.entries.iter()
    }

    /// The entries as a slice, for binary searches.
    pub(crate) fn entries(&self) -> &[TrackedEntry] {
        &self.entries
    }

    /// Binary search.
    pub fn contains(&self, path: &[u8]) -> bool {
        self.entries
            .binary_search_by(|e| e.path.as_bytes().cmp(path))
            .is_ok()
    }

    /// Whether any path lies under directory `dir` (given without a trailing
    /// `/`).
    pub fn contains_dir(&self, dir: &[u8]) -> bool {
        let mut prefix = Vec::with_capacity(dir.len() + 1);
        prefix.extend_from_slice(dir);
        prefix.push(b'/');
        let start = self
            .entries
            .partition_point(|e| e.path.as_bytes() < prefix.as_slice());
        self.entries
            .get(start)
            .is_some_and(|e| e.path.as_bytes().starts_with(&prefix))
    }

    /// Index paths that break the `RepoPath` invariants (`..`, `.`, empty
    /// components, NUL, a leading `/`; git rejects them, but an index can
    /// be tampered with). They are not in `iter` and are never opened.
    pub fn invalid_paths(&self) -> &[Arc<[u8]>] {
        &self.invalid
    }
}

/// Reads the index at `path` into a `TrackedPaths`. `handle` opens the
/// worktree's gix handle for expanding sparse directory entries, only when
/// there is one.
pub(crate) fn read(
    path: &Path,
    hash: HashKind,
    open_handle: &dyn Fn() -> Result<Handle, GitError>,
    limits: &EntryLimits,
    cancel: &AtomicBool,
) -> Result<TrackedPaths, GitError> {
    match parse(path, hash)? {
        None => Ok(TrackedPaths::empty()),
        Some((file, checksum)) => convert(&file, checksum, open_handle, limits, cancel),
    }
}

/// Parses the index at `path` and returns it with its checksum (see
/// [`checksum`]); None without an index file.
pub(crate) fn parse(
    path: &Path,
    hash: HashKind,
) -> Result<Option<(gix::index::File, Oid)>, GitError> {
    // With `index.skipHash` the stored checksum is null and ours is computed
    // over the file; retry when the file changes while it is parsed.
    for _ in 0..3 {
        let Some(before) = checksum(path, hash)? else {
            return Ok(None);
        };
        #[cfg(test)]
        hook::parsed();
        let options = gix::index::decode::Options {
            thread_limit: Some(1),
            ..Default::default()
        };
        let file = match gix::index::File::at(path, hash.to_gix(), true, options) {
            Ok(file) => file,
            Err(err) => {
                let err = err.into_error();
                if err.is_not_found() {
                    return Ok(None);
                }
                return Err(GitError::from_gix(err));
            }
        };
        let checksum = match file.checksum() {
            Some(stored) => Oid::from_gix(&stored),
            None => match self::checksum(path, hash)? {
                Some(after) if after == before => after,
                _ => continue,
            },
        };
        return Ok(Some((file, checksum)));
    }
    Err(GitError::Corrupt(format!(
        "{} kept changing while it was read",
        path.display()
    )))
}

fn convert(
    file: &gix::index::File,
    checksum: Oid,
    open_handle: &dyn Fn() -> Result<Handle, GitError>,
    limits: &EntryLimits,
    cancel: &AtomicBool,
) -> Result<TrackedPaths, GitError> {
    use gix::index::entry::{Flags, Mode as IndexMode, Stage};

    let mut out = TrackedPaths {
        checksum: Some(checksum),
        entries: Vec::new(),
        invalid: Vec::new(),
    };
    let mut handle: Option<Handle> = None;
    let entries = file.entries();
    let mut i = 0;
    while i < entries.len() {
        if i % 4096 == 0 && cancel.load(Ordering::Relaxed) {
            return Err(GitError::Cancelled);
        }
        // Entries are sorted by (path, stage): take one per conflicted path,
        // preferring stage 0, then ours (2), theirs (3), base (1).
        let path = entries[i].path(file);
        let mut end = i + 1;
        while end < entries.len() && entries[end].path(file) == path {
            end += 1;
        }
        let rank = |stage: Stage| match stage {
            Stage::Unconflicted => 0,
            Stage::Ours => 1,
            Stage::Theirs => 2,
            Stage::Base => 3,
        };
        let Some(entry) = entries[i..end]
            .iter()
            .filter(|e| !e.flags.contains(Flags::REMOVE))
            .min_by_key(|e| rank(e.stage()))
        else {
            i = end;
            continue;
        };
        i = end;
        let oid = Oid::from_gix(&entry.id);
        if entry.mode == IndexMode::DIR {
            // A sparse directory entry: `dir/` and the tree it stands for.
            let dir = path.strip_suffix(b"/").unwrap_or(path);
            if RepoPath::new(dir).is_none() {
                out.invalid.push(Arc::from(&**path));
                continue;
            }
            if handle.is_none() {
                handle = Some(open_handle()?);
            }
            let Some(handle) = handle.as_ref() else {
                continue;
            };
            tree::walk(handle, oid, dir, cancel, &mut |e| {
                push(&mut out, &e.path, e.mode, e.oid, true, limits)
            })?;
            continue;
        }
        let Some(mode) = Mode::from_index(entry.mode) else {
            return Err(GitError::Corrupt(format!(
                "index entry {:?} has mode {:o}",
                String::from_utf8_lossy(path),
                entry.mode.bits()
            )));
        };
        let skip = entry.flags.contains(Flags::SKIP_WORKTREE);
        push(&mut out, path, mode, oid, skip, limits)?;
    }
    if !out.entries.is_sorted_by(|a, b| a.path <= b.path) {
        out.entries.sort_by(|a, b| a.path.cmp(&b.path));
        out.entries.dedup_by(|a, b| a.path == b.path);
    }
    Ok(out)
}

fn push(
    out: &mut TrackedPaths,
    path: &[u8],
    mode: Mode,
    oid: Oid,
    skip_worktree: bool,
    limits: &EntryLimits,
) -> Result<(), GitError> {
    match RepoPath::new(path) {
        Some(path) => {
            if out.entries.len() >= limits.entries {
                return Err(GitError::TooManyEntries);
            }
            out.entries.push(TrackedEntry {
                path,
                mode,
                oid,
                skip_worktree,
            });
        }
        None => out.invalid.push(Arc::from(path)),
    }
    Ok(())
}

/// The index checksum: the trailing hash, read without parsing entries.
/// With `index.skipHash` git writes a null trailer; then it is the hash of
/// the file's content, the value git would have written, so it still
/// changes whenever the index does. None without an index file.
pub(crate) fn checksum(path: &Path, hash: HashKind) -> Result<Option<Oid>, GitError> {
    #[cfg(test)]
    hook::checksummed();
    let Some(mut file) = crate::gitdir::open(path)? else {
        return Ok(None);
    };
    let len = file.metadata()?.len();
    let trailer = hash.byte_len() as u64;
    // 12-byte header plus the trailer.
    if len < 12 + trailer {
        return Err(GitError::Corrupt(format!(
            "{} is truncated",
            path.display()
        )));
    }
    let mut buf = [0u8; 32];
    file.seek(SeekFrom::Start(len - trailer))?;
    file.read_exact(&mut buf[..hash.byte_len()])?;
    let stored = Oid::from_bytes(hash, &buf[..hash.byte_len()])
        .ok_or_else(|| GitError::Corrupt("index trailer".into()))?;
    if !stored.is_null() {
        return Ok(Some(stored));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = gix::hash::hasher(hash.to_gix());
    let mut remaining = len - trailer;
    let mut chunk = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let n = usize::try_from(remaining.min(chunk.len() as u64)).unwrap_or(chunk.len());
        file.read_exact(&mut chunk[..n])?;
        hasher.update(&chunk[..n]);
        remaining -= n as u64;
    }
    let id = hasher
        .try_finalize()
        .map_err(|err| GitError::Corrupt(format!("{}: {err}", path.display())))?;
    Ok(Some(Oid::from_gix(&id)))
}

/// Counting hook: index parses and checksum reads on this thread.
#[cfg(test)]
pub(crate) mod hook {
    use std::cell::Cell;

    thread_local! {
        static PARSED: Cell<usize> = const { Cell::new(0) };
        static CHECKSUMMED: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn parsed() {
        PARSED.with(|c| c.set(c.get() + 1));
    }

    pub(crate) fn checksummed() {
        CHECKSUMMED.with(|c| c.set(c.get() + 1));
    }

    pub(crate) fn parsed_count() -> usize {
        PARSED.with(Cell::get)
    }
}
