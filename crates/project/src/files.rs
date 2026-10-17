//! The tracked files of one worktree, as one path buffer plus end offsets.

use std::fmt;
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FileKind {
    File,
    Executable,
    Symlink,
    Submodule,
}

/// A worktree's tracked files, sorted by bytes (index order), so the files
/// under a directory are one contiguous range. Paths are relative, `/`
/// separated, with no empty, `.` or `..` segments.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct FileList {
    buf: Box<str>,
    ends: Box<[u32]>,
    kinds: Box<[FileKind]>,
    skipped: u32,
    truncated: bool,
}

impl FileList {
    pub fn builder() -> FileListBuilder {
        FileListBuilder::default()
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<(&str, FileKind)> {
        let end = *self.ends.get(i)? as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        Some((&self.buf[start..end], self.kinds[i]))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, FileKind)> + '_ {
        (0..self.len()).filter_map(|i| self.get(i))
    }

    /// The index range of the files under `dir` (all of them for "").
    /// Binary search; `dir` has no leading or trailing `/`.
    pub fn range_under(&self, dir: &str) -> Range<usize> {
        if dir.is_empty() {
            return 0..self.len();
        }
        // Everything under "a/b" sorts between "a/b/" and "a/b0" ('0' follows '/').
        let path = |i: usize| self.get(i).map_or("", |(p, _)| p);
        let start = partition(self.len(), |i| cmp_prefixed(path(i), dir, b'/'));
        let end = partition(self.len(), |i| cmp_prefixed(path(i), dir, b'0'));
        start..end.max(start)
    }

    /// Whether `path` is in the list, and its kind.
    pub fn find(&self, path: &str) -> Option<FileKind> {
        let i = partition(self.len(), |i| self.get(i).is_some_and(|(p, _)| p < path));
        self.get(i).filter(|(p, _)| *p == path).map(|(_, k)| k)
    }

    /// Entries not collected (not UTF-8, invalid paths).
    pub fn skipped(&self) -> u32 {
        self.skipped
    }

    /// The folder walk stopped at its limit.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// The first index in `0..len` for which `before` is false.
fn partition(len: usize, before: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0, len);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if before(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// `path < dir + sep`, comparing bytes, without building the string.
fn cmp_prefixed(path: &str, dir: &str, sep: u8) -> bool {
    let p = path.as_bytes();
    let d = dir.as_bytes();
    let n = p.len().min(d.len());
    match p[..n].cmp(&d[..n]) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        // Equal so far: compare the next byte of path with the separator.
        std::cmp::Ordering::Equal => p.get(d.len()).is_none_or(|&b| b < sep),
    }
}

impl fmt::Debug for FileList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// Collects entries for a `FileList`. Entries pushed in sorted order (as
/// the index gives them) are stored without sorting; equal paths (merge
/// stages of a conflict) are kept once.
#[derive(Default)]
pub struct FileListBuilder {
    entries: Vec<(Box<str>, FileKind)>,
    sorted: bool,
    skipped: u32,
    truncated: bool,
}

impl FileListBuilder {
    pub fn push(&mut self, path: &str, kind: FileKind) {
        if !is_valid_path(path) {
            self.skipped = self.skipped.saturating_add(1);
            return;
        }
        if self.entries.is_empty() {
            self.sorted = true;
        } else if let Some((last, _)) = self.entries.last() {
            self.sorted &= **last <= *path;
        }
        self.entries.push((path.into(), kind));
    }

    /// Counts an entry that was not collected.
    pub fn skip(&mut self) {
        self.skipped = self.skipped.saturating_add(1);
    }

    pub fn set_truncated(&mut self) {
        self.truncated = true;
    }

    pub fn build(mut self) -> FileList {
        if !self.sorted {
            // Stable, so the first of equal paths is the one kept.
            self.entries.sort_by(|a, b| a.0.cmp(&b.0));
        }
        self.entries.dedup_by(|b, a| a.0 == b.0);
        let mut buf = String::new();
        let mut ends = Vec::with_capacity(self.entries.len());
        let mut kinds = Vec::with_capacity(self.entries.len());
        for (path, kind) in self.entries {
            let Ok(end) = u32::try_from(buf.len() + path.len()) else {
                self.skipped = self.skipped.saturating_add(1);
                continue;
            };
            buf.push_str(&path);
            ends.push(end);
            kinds.push(kind);
        }
        FileList {
            buf: buf.into_boxed_str(),
            ends: ends.into_boxed_slice(),
            kinds: kinds.into_boxed_slice(),
            skipped: self.skipped,
            truncated: self.truncated,
        }
    }
}

fn is_valid_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(paths: &[&str]) -> FileList {
        let mut b = FileList::builder();
        for p in paths {
            b.push(p, FileKind::File);
        }
        b.build()
    }

    #[test]
    fn files_range_under_covers_only_the_directory() {
        // "a-b" and "a.c" sort between "a" and "a/"; "a0" after it.
        let files = list(&["a-b/x", "a.c", "a/b", "a/c/d", "a0", "b", "ab/x"]);
        let under = |dir| {
            files
                .range_under(dir)
                .map(|i| files.get(i).unwrap().0)
                .collect::<Vec<_>>()
        };
        assert_eq!(under("a"), ["a/b", "a/c/d"]);
        assert_eq!(under("a/c"), ["a/c/d"]);
        assert_eq!(under("ab"), ["ab/x"]);
        assert!(under("a/b").is_empty(), "a file is not a directory");
        assert!(under("zzz").is_empty());
        assert_eq!(under("").len(), 7);
    }

    #[test]
    fn files_builder_sorts_dedups_and_skips_invalid_paths() {
        let mut b = FileList::builder();
        b.push("b", FileKind::File);
        b.push("a", FileKind::Executable);
        b.push("a", FileKind::File);
        b.push("/abs", FileKind::File);
        b.push("x/../y", FileKind::File);
        b.push("", FileKind::File);
        b.skip();
        let files = b.build();
        assert_eq!(
            files.iter().collect::<Vec<_>>(),
            [("a", FileKind::Executable), ("b", FileKind::File)]
        );
        assert_eq!(files.skipped(), 4);
        assert!(!files.truncated());
        assert_eq!(files.find("a"), Some(FileKind::Executable));
        assert_eq!(files.find("c"), None);
    }
}
