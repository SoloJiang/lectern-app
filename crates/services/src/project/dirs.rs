//! The directory table of a worktree's file list, for the Files tree
//! (docs/design/file_reader.md「目录表」).

use std::cmp::Ordering;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use serde::de::IgnoredAny;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

use super::FileList;

/// A directory in a `DirTable`: its index in the table. The root is 0.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct DirId(pub u32);

/// The directories of one `FileList`, each with its direct subdirectories
/// and files in Finder order (`finder_cmp`). Built from the list it holds,
/// so the two always agree: `project` builds it in the background once the
/// file list is ready (ADR 0030). Read-only, no IO, any thread.
///
/// Directories are the parents of the list's entries (the root "" always
/// exists); a submodule is a file entry and never a directory.
///
/// Serialized as `{ dirs: [[path, parent], ...], files }` with `parent`
/// the index of the parent in `dirs` (null for the root). Deserializing
/// rebuilds the table from `files` alone (which goes through
/// `FileListBuilder`), so input from another process cannot break the
/// invariants; `dirs` is not read.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "Wire")]
pub struct DirTable {
    /// Sorted by path bytes, so a parent comes before its children.
    dirs: Box<[Dir]>,
    /// The directories' paths, in table order.
    names: Box<str>,
    /// Each directory's direct subdirectories, in table order, each slice
    /// in Finder order.
    subdirs: Box<[DirId]>,
    /// Each directory's direct files as indices into `list`, in table
    /// order, each slice in Finder order.
    files: Box<[u32]>,
    list: Arc<FileList>,
}

/// Ends are exclusive; a directory's ranges start at the previous
/// directory's ends (0 for the root).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Dir {
    path_end: u32,
    /// `u32::MAX` for the root.
    parent: u32,
    subdirs_end: u32,
    files_end: u32,
    file_count: u32,
}

impl DirTable {
    /// Builds the table: one pass over the sorted list for the directories
    /// and the runs of files in each, the children grouped by counting, then
    /// each directory's children sorted. No hashing and no per-directory
    /// allocation. Pure computation; for a large list run it on the
    /// executor.
    pub fn new(list: Arc<FileList>) -> DirTable {
        // Every directory with a file, and all their ancestors. Files of one
        // directory are contiguous (a run), and each run adds only the
        // ancestors it does not share with the run before it; the list is
        // sorted, so a directory is added again only when a sibling sorts
        // between it and its subdirectories (`a`, `a-b`, `a/c`).
        let mut paths: Vec<&str> = vec![""];
        // (directory, index of its first file) of each run.
        let mut runs: Vec<(&str, u32)> = Vec::new();
        let mut prev: Option<&str> = None;
        // Each file's name, for the sorting below.
        let mut file_names: Vec<&str> = Vec::with_capacity(list.len());
        for (i, (path, _)) in list.iter().enumerate() {
            let (dir, name) = split_path(path);
            file_names.push(name);
            if prev == Some(dir) {
                continue;
            }
            runs.push((dir, to_u32(i)));
            let shared = prev.map_or("", |p| common_dir(p, dir));
            let start = paths.len();
            let mut d = dir;
            while d.len() > shared.len() {
                paths.push(d);
                d = parent_of(d);
            }
            paths[start..].reverse();
            prev = Some(dir);
        }
        if !paths.is_sorted() {
            paths.sort_unstable();
        }
        paths.dedup();
        let n = paths.len();
        let index = |p: &str| paths.binary_search(&p).map_or(0, to_u32);

        // Parents: a parent sorts before its children, so it is usually on
        // the stack of the previous directory's ancestors.
        let mut parents: Vec<u32> = Vec::with_capacity(n);
        parents.push(u32::MAX);
        let mut stack: Vec<u32> = vec![0];
        for (i, p) in paths.iter().enumerate().skip(1) {
            let want = parent_of(p);
            while let Some(&top) = stack.last()
                && paths[top as usize] != want
            {
                stack.pop();
            }
            let parent = match stack.last() {
                Some(&top) => top,
                None => {
                    let found = index(want);
                    stack.push(found);
                    found
                }
            };
            parents.push(parent);
            stack.push(to_u32(i));
        }

        // Direct subdirectories and files, grouped by counting: `sub_ends`
        // and `file_ends` count each directory's children, then hold the
        // end of its slice (the table's `Dir` ends).
        let run_dirs: Vec<u32> = runs.iter().map(|&(dir, _)| index(dir)).collect();
        let run_end = |r: usize| runs.get(r + 1).map_or(to_u32(list.len()), |&(_, s)| s);
        let mut sub_ends = vec![0u32; n];
        for &parent in &parents[1..] {
            sub_ends[parent as usize] += 1;
        }
        let mut file_ends = vec![0u32; n];
        for (r, &d) in run_dirs.iter().enumerate() {
            file_ends[d as usize] += run_end(r) - runs[r].1;
        }
        // Subtree counts: children come after their parent.
        let mut counts = file_ends.clone();
        for i in (1..n).rev() {
            let parent = parents[i] as usize;
            counts[parent] = counts[parent].saturating_add(counts[i]);
        }
        let sub_starts = prefix_sums(&mut sub_ends);
        let file_starts = prefix_sums(&mut file_ends);
        let mut subdirs = vec![DirId(0); n - 1];
        let mut next = sub_starts.clone();
        for (i, &parent) in parents.iter().enumerate().skip(1) {
            let at = &mut next[parent as usize];
            subdirs[*at as usize] = DirId(to_u32(i));
            *at += 1;
        }
        let mut files = vec![0u32; list.len()];
        let mut next = file_starts.clone();
        for (r, &d) in run_dirs.iter().enumerate() {
            for f in runs[r].1..run_end(r) {
                let at = &mut next[d as usize];
                files[*at as usize] = f;
                *at += 1;
            }
        }
        drop(next);

        // Finder order within each directory; byte order (the list's) often
        // is already.
        let file_name = |i: u32| file_names[i as usize];
        let in_order = |a: &u32, b: &u32| finder_cmp(file_name(*a), file_name(*b)).is_lt();
        let mut names = String::with_capacity(paths.iter().map(|p| p.len()).sum());
        let mut dirs = Vec::with_capacity(n);
        for (i, path) in paths.iter().enumerate() {
            let sub = &mut subdirs[sub_starts[i] as usize..sub_ends[i] as usize];
            if sub.len() > 1 {
                sub.sort_unstable_by(|a, b| {
                    finder_cmp(name_of(paths[a.0 as usize]), name_of(paths[b.0 as usize]))
                });
            }
            let own = &mut files[file_starts[i] as usize..file_ends[i] as usize];
            if own.len() > 1 && !own.is_sorted_by(in_order) {
                own.sort_unstable_by(|&a, &b| finder_cmp(file_name(a), file_name(b)));
            }
            names.push_str(path);
            dirs.push(Dir {
                path_end: to_u32(names.len()),
                parent: parents[i],
                subdirs_end: sub_ends[i],
                files_end: file_ends[i],
                file_count: counts[i],
            });
        }
        DirTable {
            dirs: dirs.into_boxed_slice(),
            names: names.into_boxed_str(),
            subdirs: subdirs.into_boxed_slice(),
            files: files.into_boxed_slice(),
            list,
        }
    }

    /// The file list the table was built from.
    pub fn files(&self) -> &Arc<FileList> {
        &self.list
    }

    pub fn root(&self) -> DirId {
        DirId(0)
    }

    /// The number of directories, the root included.
    pub fn len(&self) -> usize {
        self.dirs.len()
    }

    /// Never true: the root always exists.
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    /// The directory at `dir` (no leading or trailing `/`; "" is the root).
    /// Binary search.
    pub fn find(&self, dir: &str) -> Option<DirId> {
        let (mut lo, mut hi) = (0, self.dirs.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.path(DirId(to_u32(mid))).cmp(dir) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return Some(DirId(to_u32(mid))),
            }
        }
        None
    }

    /// The directory's path; "" for the root. Panics on an id from another
    /// table that is out of range, like slice indexing.
    pub fn path(&self, dir: DirId) -> &str {
        &self.names[self.range(dir, |d| d.path_end)]
    }

    /// The last segment of the path; "" for the root.
    pub fn name(&self, dir: DirId) -> &str {
        name_of(self.path(dir))
    }

    pub fn parent(&self, dir: DirId) -> Option<DirId> {
        let parent = self.dirs[dir.0 as usize].parent;
        (parent != u32::MAX).then_some(DirId(parent))
    }

    /// The direct subdirectories, in Finder order.
    pub fn subdirs(&self, dir: DirId) -> &[DirId] {
        &self.subdirs[self.range(dir, |d| d.subdirs_end)]
    }

    /// The direct files as indices into `files()`, in Finder order.
    pub fn dir_files(&self, dir: DirId) -> &[u32] {
        &self.files[self.range(dir, |d| d.files_end)]
    }

    /// The end of the chain of directories with one subdirectory and no
    /// files that starts at `dir` (`src/main/java/com/example`); `dir`
    /// itself when it does not fold.
    pub fn fold_end(&self, dir: DirId) -> DirId {
        let mut d = dir;
        while let [only] = self.subdirs(d)
            && self.dir_files(d).is_empty()
        {
            d = *only;
        }
        d
    }

    /// The number of files in the subtree (for probes and tests; the row
    /// does not show it).
    pub fn file_count(&self, dir: DirId) -> u32 {
        self.dirs[dir.0 as usize].file_count
    }

    /// A directory's range in one of the arrays, given which end it is:
    /// from the previous directory's end to its own.
    fn range(&self, dir: DirId, end: fn(&Dir) -> u32) -> Range<usize> {
        let i = dir.0 as usize;
        let start = i.checked_sub(1).map_or(0, |prev| end(&self.dirs[prev]));
        start as usize..end(&self.dirs[i]) as usize
    }
}

/// The directory part of a path: everything before the last `/`, or "".
fn parent_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// The last segment of a path.
fn name_of(path: &str) -> &str {
    path.rfind('/').map_or(path, |i| &path[i + 1..])
}

/// `(parent_of(path), name_of(path))` with one search.
fn split_path(path: &str) -> (&str, &str) {
    path.rfind('/')
        .map_or(("", path), |i| (&path[..i], &path[i + 1..]))
}

/// The deepest directory that is `a` or above it and also `b` or above it
/// (whole segments; "" when they share none).
fn common_dir<'a>(a: &'a str, b: &str) -> &'a str {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let n = x.iter().zip(y).take_while(|(p, q)| p == q).count();
    let boundary = |s: &[u8]| n == s.len() || s[n] == b'/';
    if boundary(x) && boundary(y) {
        return &a[..n];
    }
    // `n` may fall inside a character; a `/` byte never does.
    let cut = x[..n].iter().rposition(|&c| c == b'/').unwrap_or(0);
    &a[..cut]
}

/// Turns counts into running ends in place; returns the starts.
fn prefix_sums(counts: &mut [u32]) -> Vec<u32> {
    let mut starts = Vec::with_capacity(counts.len());
    let mut sum = 0u32;
    for c in counts.iter_mut() {
        starts.push(sum);
        sum = sum.saturating_add(*c);
        *c = sum;
    }
    starts
}

/// Counts and offsets fit in `u32`: `FileList` keeps its path buffer under
/// `u32::MAX` bytes, and there are fewer directories than path bytes.
fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Finder's comparison: case-insensitive (`to_lowercase` per character),
/// runs of ASCII digits compared by value, then by bytes when otherwise
/// equal, so the order is total. Not quite `localizedStandardCompare`
/// (accents, locale rules), see file_reader's known limitations. Pure.
pub fn finder_cmp(a: &str, b: &str) -> Ordering {
    folded_cmp(a, b).then_with(|| a.cmp(b))
}

fn folded_cmp(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    loop {
        // Two ASCII bytes: the same result as the characters' path below,
        // without decoding.
        if let (Some(&x), Some(&y)) = (ab.get(i), bb.get(j))
            && x.is_ascii()
            && y.is_ascii()
            && !(x.is_ascii_digit() && y.is_ascii_digit())
        {
            let ord = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
            continue;
        }
        let (Some(c), Some(d)) = (a[i..].chars().next(), b[j..].chars().next()) else {
            return (i < a.len()).cmp(&(j < b.len()));
        };
        if c.is_ascii_digit() && d.is_ascii_digit() {
            let x = digit_run(&ab[i..]);
            let y = digit_run(&bb[j..]);
            let (nx, ny) = (trim_zeros(x), trim_zeros(y));
            let ord = nx.len().cmp(&ny.len()).then_with(|| nx.cmp(ny));
            if ord != Ordering::Equal {
                return ord;
            }
            i += x.len();
            j += y.len();
        } else {
            let ord = c.to_lowercase().cmp(d.to_lowercase());
            if ord != Ordering::Equal {
                return ord;
            }
            i += c.len_utf8();
            j += d.len_utf8();
        }
    }
}

fn digit_run(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|b| b.is_ascii_digit()).count();
    &s[..n]
}

fn trim_zeros(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|&&b| b == b'0').count();
    &s[n..]
}

impl Serialize for DirTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Dirs<'a>(&'a DirTable);
        impl Serialize for Dirs<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let table = self.0;
                serializer.collect_seq((0..table.len()).map(|i| {
                    let dir = DirId(to_u32(i));
                    (table.path(dir), table.parent(dir).map(|p| p.0))
                }))
            }
        }
        let mut s = serializer.serialize_struct("DirTable", 2)?;
        s.serialize_field("dirs", &Dirs(self))?;
        s.serialize_field("files", &*self.list)?;
        s.end()
    }
}

/// The serialized form of a `DirTable`.
#[derive(Deserialize)]
struct Wire {
    #[serde(rename = "dirs")]
    _dirs: IgnoredAny,
    files: FileList,
}

impl From<Wire> for DirTable {
    fn from(wire: Wire) -> Self {
        DirTable::new(Arc::new(wire.files))
    }
}

impl fmt::Debug for DirTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let paths = (0..self.len()).map(|i| self.path(DirId(to_u32(i))));
        f.debug_struct("DirTable")
            .field("dirs", &paths.collect::<Vec<_>>())
            .field("files", &self.list.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::super::FileKind;
    use super::*;

    fn table(entries: &[(&str, FileKind)]) -> DirTable {
        let mut b = FileList::builder();
        for (p, kind) in entries {
            b.push(p, *kind);
        }
        DirTable::new(Arc::new(b.build()))
    }

    fn files(paths: &[&str]) -> DirTable {
        let entries: Vec<_> = paths.iter().map(|p| (*p, FileKind::File)).collect();
        table(&entries)
    }

    /// The names of a directory's subdirectories and files, in table order.
    fn listing(t: &DirTable, dir: &str) -> (Vec<String>, Vec<String>) {
        let d = t.find(dir).expect("directory");
        let subdirs = t.subdirs(d).iter().map(|&s| t.name(s).to_owned());
        let files = t.dir_files(d).iter().map(|&i| {
            let (path, _) = t.files().get(i as usize).unwrap();
            path.rsplit('/').next().unwrap().to_owned()
        });
        (subdirs.collect(), files.collect())
    }

    #[test]
    fn dirs_match_a_naive_construction() {
        let paths = [
            "Cargo.toml",
            "a-b/x",
            "a.c",
            "a/b/c/d.rs",
            "a/b/e.rs",
            "a/f",
            "a0/g",
            "docs/adr/0001.md",
            "docs/README.md",
            "z",
        ];
        let t = assert_matches_naive(&paths);
        let root = t.root();
        assert_eq!((t.path(root), t.name(root), t.parent(root)), ("", "", None));
        assert_eq!(t.file_count(root), paths.len() as u32);
        let c = t.find("a/b/c").unwrap();
        assert_eq!(t.name(c), "c");
        assert_eq!(t.parent(c), t.find("a/b"));
        for missing in ["a/b/c/d.rs", "a/", "/a", "q", "a/b/c/"] {
            assert_eq!(t.find(missing), None, "{missing}");
        }
    }

    /// Siblings that sort between a directory and its subdirectories
    /// (`a-b`, `a.c` between `a` and `a/…`), directories whose files are
    /// not contiguous with their subdirectories', deep chains, non-ASCII
    /// names.
    #[test]
    fn dirs_match_a_naive_construction_on_interleaved_trees() {
        let mut paths = Vec::new();
        for top in ["a", "a-b", "a.c", "a0", "b", "é", "É-x"] {
            paths.push(format!("{top}/f"));
            for mid in ["x", "x-y", "x/z", "Y", "10", "9"] {
                paths.push(format!("{top}/{mid}/g"));
                paths.push(format!("{top}/{mid}/deep/er/still/h"));
            }
        }
        paths.push("top".to_owned());
        paths.push("a-b-c/d/e".to_owned());
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        assert_matches_naive(&paths);
    }

    /// Builds the table of `paths` and checks it against a naive
    /// construction: every ancestor of every path, with direct children by
    /// name, in path byte order.
    fn assert_matches_naive(paths: &[&str]) -> DirTable {
        let t = files(paths);
        // Naive: every ancestor of every path, with direct children by name.
        let mut naive: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>, u32)> =
            BTreeMap::new();
        naive.entry(String::new()).or_default();
        for p in paths {
            let segs: Vec<&str> = p.split('/').collect();
            for depth in 0..segs.len() {
                let dir = segs[..depth].join("/");
                let entry = naive.entry(dir).or_default();
                entry.2 += 1;
                if depth + 1 == segs.len() {
                    entry.1.insert(segs[depth].to_owned());
                } else {
                    entry.0.insert(segs[depth].to_owned());
                }
            }
        }

        assert_eq!(t.len(), naive.len());
        assert!(!t.is_empty());
        for (i, (dir, (subdirs, own, count))) in naive.iter().enumerate() {
            let d = t.find(dir).unwrap_or_else(|| panic!("{dir:?} missing"));
            assert_eq!(d, DirId(i as u32), "sorted by bytes like the naive map");
            assert_eq!(t.path(d), dir);
            assert_eq!(t.file_count(d), *count, "{dir:?}");
            let (s, f) = listing(&t, dir);
            assert_eq!(s.into_iter().collect::<BTreeSet<_>>(), *subdirs, "{dir:?}");
            assert_eq!(f.into_iter().collect::<BTreeSet<_>>(), *own, "{dir:?}");
            for &sub in t.subdirs(d) {
                assert_eq!(t.parent(sub), Some(d));
            }
            // Children in Finder order.
            let names = |ids: &[DirId]| -> Vec<String> {
                ids.iter().map(|&s| t.name(s).to_owned()).collect()
            };
            let mut sorted = names(t.subdirs(d));
            sorted.sort_by(|a, b| finder_cmp(a, b));
            assert_eq!(names(t.subdirs(d)), sorted, "{dir:?}");
            let (_, own) = listing(&t, dir);
            let mut sorted = own.clone();
            sorted.sort_by(|a, b| finder_cmp(a, b));
            assert_eq!(own, sorted, "{dir:?}");
        }
        t
    }

    #[test]
    fn dirs_children_are_in_finder_order() {
        let t = files(&[
            "src/file10.rs",
            "src/File2.rs",
            "src/file1.rs",
            "src/b/x",
            "src/A/x",
            "src/a/x",
            "src/v10/x",
            "src/v9/x",
        ]);
        let (subdirs, own) = listing(&t, "src");
        assert_eq!(subdirs, ["A", "a", "b", "v9", "v10"]);
        assert_eq!(own, ["file1.rs", "File2.rs", "file10.rs"]);
    }

    #[test]
    fn dirs_fold_single_child_chains() {
        let t = files(&[
            "src/main/java/com/example/App.java",
            "src/main/java/com/example/util/Str.java",
            "src/test/T.java",
            "README",
        ]);
        let at = |p: &str| t.find(p).unwrap();
        // `main` → `java` → `com` → `example`, which has a file.
        assert_eq!(t.fold_end(at("src/main")), at("src/main/java/com/example"));
        // Two subdirectories: no fold.
        assert_eq!(t.fold_end(at("src")), at("src"));
        // A file in it: no fold.
        assert_eq!(t.fold_end(at("src/test")), at("src/test"));
        // A leaf directory folds to itself.
        let util = at("src/main/java/com/example/util");
        assert_eq!(t.fold_end(util), util);
        assert_eq!(t.fold_end(t.root()), t.root());
    }

    #[test]
    fn dirs_submodules_are_files() {
        let t = table(&[
            ("vendor/sub", FileKind::Submodule),
            ("vendor/lib.rs", FileKind::File),
            ("run.sh", FileKind::Executable),
            ("link", FileKind::Symlink),
        ]);
        assert_eq!(t.find("vendor/sub"), None);
        let (subdirs, own) = listing(&t, "vendor");
        assert!(subdirs.is_empty());
        assert_eq!(own, ["lib.rs", "sub"]);
        assert_eq!(listing(&t, "").1, ["link", "run.sh"]);
    }

    #[test]
    fn dirs_empty_list_has_only_the_root() {
        let t = files(&[]);
        assert_eq!(t.len(), 1);
        let root = t.root();
        assert_eq!(t.find(""), Some(root));
        assert!(t.subdirs(root).is_empty() && t.dir_files(root).is_empty());
        assert_eq!((t.file_count(root), t.fold_end(root)), (0, root));
        assert!(t.files().is_empty());
    }

    /// The comparison with every character decoded and lowercased, as
    /// before the ASCII shortcut.
    fn finder_cmp_by_chars(a: &str, b: &str) -> Ordering {
        let (ab, bb) = (a.as_bytes(), b.as_bytes());
        let (mut i, mut j) = (0, 0);
        let folded = loop {
            let (Some(c), Some(d)) = (a[i..].chars().next(), b[j..].chars().next()) else {
                break (i < a.len()).cmp(&(j < b.len()));
            };
            if c.is_ascii_digit() && d.is_ascii_digit() {
                let x = digit_run(&ab[i..]);
                let y = digit_run(&bb[j..]);
                let (nx, ny) = (trim_zeros(x), trim_zeros(y));
                let ord = nx.len().cmp(&ny.len()).then_with(|| nx.cmp(ny));
                if ord != Ordering::Equal {
                    break ord;
                }
                i += x.len();
                j += y.len();
            } else {
                let ord = c.to_lowercase().cmp(d.to_lowercase());
                if ord != Ordering::Equal {
                    break ord;
                }
                i += c.len_utf8();
                j += d.len_utf8();
            }
        };
        folded.then_with(|| a.cmp(b))
    }

    #[test]
    fn finder_cmp_ascii_shortcut_matches_the_character_comparison() {
        let words = [
            "",
            "a",
            "A",
            "b",
            "Z",
            "_",
            "-",
            ".",
            "a1",
            "a01",
            "a10",
            "A9",
            "x9y",
            "x09Y",
            "é",
            "É",
            "éa",
            "Ea",
            "aé",
            "aÉ",
            "ß",
            "SS",
            "İ",
            "i̇",
            "Ω",
            "ω1",
            "1ω",
            "file2.rs",
            "File10.rs",
            "日本",
            "a日",
            "a-b",
            "a.c",
            "a/b",
            "ǅ",
            "ǆ",
            "\u{7f}",
            "~",
        ];
        for a in words {
            for b in words {
                for (x, y) in [
                    (a.to_owned(), b.to_owned()),
                    (format!("{a}{b}"), format!("{b}{a}")),
                ] {
                    assert_eq!(
                        finder_cmp(&x, &y),
                        finder_cmp_by_chars(&x, &y),
                        "{x:?} vs {y:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn finder_cmp_cases() {
        use Ordering::{Equal, Greater, Less};
        let cases = [
            ("file2", "file10", Less),
            ("file10", "file2", Greater),
            ("a", "B", Less),
            ("A", "a", Less),
            ("a", "A", Greater),
            ("abc", "ABD", Less),
            ("file01", "file1", Less),
            ("file1", "file01", Greater),
            ("file001", "file01", Less),
            ("file2", "file02x", Less),
            ("x9", "x0010", Less),
            ("a", "a.rs", Less),
            ("", "a", Less),
            ("README", "readme.md", Less),
            ("Éa", "éb", Less),
            ("v1.10", "v1.9", Greater),
            ("same", "same", Equal),
            ("99999999999999999999999", "100000000000000000000000", Less),
        ];
        for (a, b, want) in cases {
            assert_eq!(finder_cmp(a, b), want, "{a} vs {b}");
            assert_eq!(finder_cmp(b, a), want.reverse(), "{b} vs {a}");
        }
        // Total: equal only for equal strings.
        let mut names = [
            "b", "B", "a10", "a9", "A9", "a09", "_x", "1", "01", "é", "E",
        ];
        names.sort_by(|a, b| finder_cmp(a, b));
        // Digits sort before `_`, which sorts before letters (by the
        // lowercased character); equal folds fall back to bytes.
        assert_eq!(
            names,
            [
                "01", "1", "_x", "A9", "a09", "a9", "a10", "B", "b", "E", "é"
            ]
        );
    }
}
