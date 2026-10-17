//! Validated worktree-relative paths.

use std::fmt;
use std::sync::Arc;

/// A path relative to the worktree root, `/`-separated, raw bytes. Same
/// invariants as `git::RepoPath`: non-empty, no empty components, no `.` or
/// `..`, no NUL, no leading `/`. Constructed only through [`WatchPath::new`];
/// events whose path breaks them are dropped.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WatchPath(Arc<[u8]>);

impl WatchPath {
    /// `None` when `bytes` breaks an invariant.
    pub fn new(bytes: &[u8]) -> Option<WatchPath> {
        is_valid(bytes).then(|| WatchPath(bytes.into()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for WatchPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WatchPath({:?})", String::from_utf8_lossy(&self.0))
    }
}

pub(crate) fn is_valid(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.split(|&b| b == b'/').all(is_valid_name)
}

/// One path component: non-empty, not `.` or `..`, no `/` or NUL.
fn is_valid_name(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.iter().any(|&b| b == 0 || b == b'/')
}

/// The last component of a `/`-separated path.
pub(crate) fn file_name(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(slash) => &path[slash + 1..],
        None => path,
    }
}

/// The parent of a `/`-separated path; `None` for a single component.
pub(crate) fn parent(path: &[u8]) -> Option<&[u8]> {
    path.iter()
        .rposition(|&b| b == b'/')
        .map(|slash| &path[..slash])
}

/// Whether `path` is `dir` or lies under it.
pub(crate) fn is_within(path: &[u8], dir: &[u8]) -> bool {
    path.starts_with(dir) && (path.len() == dir.len() || path[dir.len()] == b'/')
}

/// Whether `path` lies strictly under `dir`.
pub(crate) fn is_under(path: &[u8], dir: &[u8]) -> bool {
    path.len() > dir.len() && path.starts_with(dir) && path[dir.len()] == b'/'
}

/// Whether the sorted slice `sorted` holds `path` itself or a path under it.
pub(crate) fn holds_within<T: AsRef<[u8]>>(sorted: &[T], path: &[u8]) -> bool {
    sorted
        .binary_search_by(|entry| entry.as_ref().cmp(path))
        .is_ok()
        || holds_under(sorted, path)
}

/// Whether the sorted slice `sorted` holds a path strictly under `path`.
/// Entries under `path` sort together, right from `path` + `/` on.
pub(crate) fn holds_under<T: AsRef<[u8]>>(sorted: &[T], path: &[u8]) -> bool {
    if sorted.is_empty() {
        return false;
    }
    let mut prefix = path.to_vec();
    prefix.push(b'/');
    let start = sorted.partition_point(|entry| entry.as_ref() < prefix.as_slice());
    sorted
        .get(start)
        .is_some_and(|entry| entry.as_ref().starts_with(&prefix))
}

impl AsRef<[u8]> for WatchPath {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_path_invariants() {
        for good in ["a", "a/b", "a b/.c", ".gitignore", "a/..b"] {
            assert!(WatchPath::new(good.as_bytes()).is_some(), "{good}");
        }
        for bad in ["", "/a", "a/", "a//b", ".", "a/./b", "a/..", "a\0b"] {
            assert!(WatchPath::new(bad.as_bytes()).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn sorted_lookups_match_the_path_and_what_is_under_it() {
        let sorted = ["a/b", "a/b-x", "a/b/c", "a/bc", "d"];
        assert!(holds_within(&sorted, b"a/b"));
        assert!(holds_within(&sorted, b"a"));
        assert!(!holds_within(&sorted, b"a/c"));
        assert!(holds_under(&sorted, b"a/b"));
        assert!(!holds_under(&sorted, b"d"));
        assert!(!holds_under(&sorted, b"a/bc"));
    }
}
