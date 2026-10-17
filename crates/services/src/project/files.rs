//! The tracked files of one worktree, as one path buffer plus end offsets
//! (docs/design/project.md「文件列表」).

use std::fmt;
use std::ops::Range;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

use super::BlobId;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum FileKind {
    File,
    Executable,
    Symlink,
    Submodule,
}

/// A worktree's tracked files, sorted by bytes (index order), so the files
/// under a directory are one contiguous range. Paths are relative, `/`
/// separated, with no empty, `.` or `..` segments.
///
/// A git project's list has every entry's blob OID from the index; a
/// folder project's has none. The OIDs are stored at the list's hash width
/// (20 or 32 bytes each).
///
/// Serialized as `{ entries: [[path, kind, oid], ...], skipped, truncated }`
/// with `oid` in hex or null. Deserializing goes through `FileListBuilder`,
/// so input from another process cannot break the invariants: invalid
/// paths are counted in `skipped`, unsorted entries are sorted, duplicates
/// kept once.
#[derive(Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(from = "Wire")]
pub struct FileList {
    buf: Box<str>,
    ends: Box<[u32]>,
    kinds: Box<[FileKind]>,
    /// `oid_width` bytes per entry, or empty when the list has no OIDs.
    oids: Box<[u8]>,
    /// 0 (no OIDs), 20 or 32.
    oid_width: u8,
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

    /// The blob OID of entry `i`: always present in a git project's list,
    /// None in a folder project's.
    pub fn oid(&self, i: usize) -> Option<BlobId> {
        let w = usize::from(self.oid_width);
        let start = i.checked_mul(w)?;
        BlobId::from_bytes(self.oids.get(start..start.checked_add(w)?)?)
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

impl Serialize for FileList {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Entries<'a>(&'a FileList);
        impl Serialize for Entries<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let list = self.0;
                serializer.collect_seq((0..list.len()).filter_map(|i| {
                    let (path, kind) = list.get(i)?;
                    Some((path, kind, list.oid(i)))
                }))
            }
        }
        let mut s = serializer.serialize_struct("FileList", 3)?;
        s.serialize_field("entries", &Entries(self))?;
        s.serialize_field("skipped", &self.skipped)?;
        s.serialize_field("truncated", &self.truncated)?;
        s.end()
    }
}

/// The serialized form of a `FileList`.
#[derive(Deserialize)]
struct Wire {
    entries: Vec<(Box<str>, FileKind, Option<BlobId>)>,
    skipped: u32,
    truncated: bool,
}

impl From<Wire> for FileList {
    fn from(wire: Wire) -> Self {
        let mut b = FileList::builder();
        for (path, kind, oid) in &wire.entries {
            match oid {
                Some(oid) => b.push_with_oid(path, *kind, *oid),
                None => b.push(path, *kind),
            }
        }
        b.skipped = b.skipped.saturating_add(wire.skipped);
        b.truncated = wire.truncated;
        b.build()
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
///
/// The list keeps OIDs only when every collected entry was pushed with
/// `push_with_oid` and all OIDs have the same width; otherwise `oid` is
/// None for every entry.
#[derive(Default)]
pub struct FileListBuilder {
    entries: Vec<(Box<str>, FileKind, Option<BlobId>)>,
    sorted: bool,
    skipped: u32,
    truncated: bool,
}

impl FileListBuilder {
    pub fn push(&mut self, path: &str, kind: FileKind) {
        self.push_entry(path, kind, None);
    }

    /// Like `push`, with the entry's blob OID (git projects).
    pub fn push_with_oid(&mut self, path: &str, kind: FileKind, oid: BlobId) {
        self.push_entry(path, kind, Some(oid));
    }

    fn push_entry(&mut self, path: &str, kind: FileKind, oid: Option<BlobId>) {
        if !is_valid_path(path) {
            self.skipped = self.skipped.saturating_add(1);
            return;
        }
        if self.entries.is_empty() {
            self.sorted = true;
        } else if let Some((last, _, _)) = self.entries.last() {
            self.sorted &= **last <= *path;
        }
        self.entries.push((path.into(), kind, oid));
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
        let width = oid_width(&self.entries);
        let mut buf = String::new();
        let mut ends = Vec::with_capacity(self.entries.len());
        let mut kinds = Vec::with_capacity(self.entries.len());
        let mut oids = Vec::with_capacity(self.entries.len() * usize::from(width));
        for (path, kind, oid) in self.entries {
            let Ok(end) = u32::try_from(buf.len() + path.len()) else {
                self.skipped = self.skipped.saturating_add(1);
                continue;
            };
            buf.push_str(&path);
            ends.push(end);
            kinds.push(kind);
            if width > 0
                && let Some(oid) = oid
            {
                oids.extend_from_slice(oid.as_bytes());
            }
        }
        FileList {
            buf: buf.into_boxed_str(),
            ends: ends.into_boxed_slice(),
            kinds: kinds.into_boxed_slice(),
            oids: oids.into_boxed_slice(),
            oid_width: width,
            skipped: self.skipped,
            truncated: self.truncated,
        }
    }
}

/// The OID width shared by every entry, or 0 when an entry has no OID, the
/// widths differ, or there are no entries.
fn oid_width(entries: &[(Box<str>, FileKind, Option<BlobId>)]) -> u8 {
    let mut widths = entries
        .iter()
        .map(|(_, _, oid)| oid.map_or(0, |o| o.as_bytes().len()));
    let first = widths.next().unwrap_or(0);
    if widths.all(|w| w == first) {
        u8::try_from(first).unwrap_or(0)
    } else {
        0
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
