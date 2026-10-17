//! Validated repository paths, file modes and reference names
//! (docs/design/git.md「对象 ID 与路径」).

use std::fmt;
use std::sync::Arc;

use lectern_platform::RelPath;

/// A path relative to the worktree root, `/`-separated, raw bytes (git
/// allows names that aren't UTF-8). Same invariants as
/// [`lectern_platform::RelPath`]: non-empty, no empty components, no `.` or
/// `..`, no NUL, no leading `/`. Constructed only through [`RepoPath::new`].
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoPath(Arc<[u8]>);

impl RepoPath {
    /// `None` when `bytes` breaks an invariant.
    pub fn new(bytes: &[u8]) -> Option<RepoPath> {
        RelPath::new(bytes)?;
        Some(RepoPath(bytes.into()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The same bytes as a platform path. Always succeeds because the
    /// invariants are the same; `None` only if they ever drift apart.
    pub(crate) fn as_rel(&self) -> Option<RelPath<'_>> {
        RelPath::new(&self.0)
    }
}

impl fmt::Debug for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RepoPath({:?})", String::from_utf8_lossy(&self.0))
    }
}

/// The mode of a file in a tree, the index or the worktree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    File,
    Executable,
    Symlink,
    Gitlink,
    /// Only on the worktree side: an entry git can't record (FIFO, socket,
    /// device file, an untracked nested repository directory). Its content
    /// is always `Unavailable` (`UnsupportedType` or `NestedRepository`).
    Special,
}

impl Mode {
    /// The mode of a tree entry; `None` for subtrees.
    pub(crate) fn from_tree(mode: gix::objs::tree::EntryMode) -> Option<Mode> {
        use gix::objs::tree::EntryKind;
        Some(match mode.kind() {
            EntryKind::Blob => Mode::File,
            EntryKind::BlobExecutable => Mode::Executable,
            EntryKind::Link => Mode::Symlink,
            EntryKind::Commit => Mode::Gitlink,
            EntryKind::Tree => return None,
        })
    }

    /// The mode of an index entry; `None` for sparse directory entries.
    pub(crate) fn from_index(mode: gix::index::entry::Mode) -> Option<Mode> {
        use gix::index::entry::Mode as M;
        if mode == M::FILE {
            Some(Mode::File)
        } else if mode == M::FILE_EXECUTABLE {
            Some(Mode::Executable)
        } else if mode == M::SYMLINK {
            Some(Mode::Symlink)
        } else if mode == M::COMMIT {
            Some(Mode::Gitlink)
        } else {
            None
        }
    }
}

/// A full reference name such as `refs/heads/main`, or `HEAD`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RefName(Arc<str>);

impl RefName {
    /// `None` unless `name` is `HEAD` or a valid name under `refs/`
    /// (`git check-ref-format` rules).
    pub fn new(name: &str) -> Option<RefName> {
        if name != "HEAD" && !name.starts_with("refs/") {
            return None;
        }
        gix::refs::FullName::try_from(name).ok()?;
        Some(RefName(name.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_gix(name: &gix::refs::FullNameRef) -> Option<RefName> {
        let name = std::str::from_utf8(name.as_bstr()).ok()?;
        RefName::new(name)
    }
}

impl fmt::Debug for RefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RefName({:?})", &*self.0)
    }
}

impl fmt::Display for RefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_path_rejects_what_relpath_rejects() {
        for bad in [
            &b""[..],
            b"/a",
            b"a/",
            b"a//b",
            b".",
            b"..",
            b"a/../b",
            b"a/./b",
            b"../a",
            b"a\0b",
        ] {
            assert_eq!(RepoPath::new(bad), None, "{bad:?}");
        }
        for good in [
            &b"a"[..],
            b"a/b",
            b".git",
            b"..a",
            b"a..",
            b".hidden/x",
            b"\xff\xfe",
        ] {
            let path = RepoPath::new(good).unwrap();
            assert_eq!(path.as_bytes(), good);
            assert!(path.as_rel().is_some());
        }
    }

    #[test]
    fn refs_name_validation() {
        assert!(RefName::new("refs/heads/main").is_some());
        assert!(RefName::new("HEAD").is_some());
        assert!(RefName::new("refs/heads/feature/x").is_some());
        for bad in [
            "main",
            "refs/heads/a..b",
            "refs/heads/a b",
            "refs/heads/.x",
            "refs/heads/x.lock",
            "",
        ] {
            assert_eq!(RefName::new(bad), None, "{bad}");
        }
    }
}
