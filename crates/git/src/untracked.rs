//! Untracked files (docs/design/git.md「工作区比较」第 4 步, ADR 0028): a
//! directory walk through `SafeRoot::list`, so that no level of a listed
//! path is a symbolic link and every name comes from beneath the worktree
//! root. In-tree `.gitignore` files are read through `SafeRoot` and matched
//! in memory; `info/exclude` and `core.excludesFile` come from gix's exclude
//! stack, configured with no worktree source, so gix reads no worktree file.

use std::borrow::Cow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use gix::bstr::{BStr, ByteSlice};
use gix::glob::pattern::Case;
use gix::ignore::search::Ignore;
use lectern_platform::{FileKind, Read, SafeRoot};

use crate::handle::Handle;
use crate::{GitError, HashKind, Mode, RepoPath};

/// What an untracked entry is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    File,
    Symlink,
    /// A FIFO, socket or device file.
    Special,
    /// A directory with its own `.git`: listed once, not entered.
    Repository,
}

/// How the index knows a path.
pub(crate) trait Index {
    /// The stage-0 (or conflict) entry's mode at `path`, if any.
    fn entry(&self, path: &[u8]) -> Option<Mode>;
    /// Whether any index path lies beneath directory `dir`.
    fn has_beneath(&self, dir: &[u8]) -> bool;
}

/// Which paths a walk covers: each scope path and everything beneath it
/// (all paths when None).
pub(crate) trait Covers {
    fn covers(&self, path: &[u8]) -> bool;
    /// Whether `dir` is above some scope path.
    fn leads_to(&self, dir: &[u8]) -> bool;
}

pub(crate) struct Found {
    pub entries: Vec<(RepoPath, Kind)>,
    /// More untracked entries than `limit` exist.
    pub capped: bool,
}

/// Walks the worktree for untracked entries: index paths are skipped,
/// ignored files and directories are skipped (an ignored directory isn't
/// entered), nested repositories are listed once, and symbolic links are
/// listed, never followed. At most `limit` entries, in path order.
#[allow(
    clippy::too_many_arguments,
    reason = "one step of the comparison's `run`"
)]
pub(crate) fn walk(
    handle: &Handle,
    hash: HashKind,
    root: &SafeRoot,
    index: &dyn Index,
    scope: Option<&dyn Covers>,
    limit: usize,
    read_bytes: u64,
    cancel: &AtomicBool,
) -> Result<Found, GitError> {
    let config = handle.config_snapshot();
    let case = if config.boolean("core.ignoreCase").unwrap_or(false) {
        Case::Fold
    } else {
        Case::Sensitive
    };
    let precompose = config.boolean("core.precomposeUnicode").unwrap_or(false);

    // `info/exclude` and `core.excludesFile` (git-directory and user files).
    // The id-mapping source with no mappings reads no in-tree `.gitignore`.
    let globals = handle
        .excludes(
            &gix::index::State::new(hash.to_gix()),
            None,
            gix::worktree::stack::state::ignore::Source::IdMapping,
        )
        .map_err(GitError::from_gix)?;
    let mut walker = Walker {
        root,
        index,
        scope,
        limit,
        read_bytes,
        cancel,
        case,
        precompose,
        search: gix::ignore::Search::default(),
        globals,
        found: Vec::new(),
        capped: false,
    };
    walker.dir(&[])?;
    let mut entries = walker.found;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.dedup_by(|a, b| a.0 == b.0);
    Ok(Found {
        entries,
        capped: walker.capped,
    })
}

struct Walker<'a> {
    root: &'a SafeRoot,
    index: &'a dyn Index,
    scope: Option<&'a dyn Covers>,
    limit: usize,
    read_bytes: u64,
    cancel: &'a AtomicBool,
    case: Case,
    precompose: bool,
    /// One pattern list per directory being walked, deepest last.
    search: gix::ignore::Search,
    globals: gix::AttributeStack<'a>,
    found: Vec<(RepoPath, Kind)>,
    capped: bool,
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = Vec::with_capacity(dir.len() + 1 + name.len());
    if !dir.is_empty() {
        path.extend_from_slice(dir);
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

impl Walker<'_> {
    /// Walks directory `dir` (empty for the root). Returns false once the
    /// limit is reached.
    fn dir(&mut self, dir: &[u8]) -> Result<bool, GitError> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(GitError::Cancelled);
        }
        let listed = match RepoPath::new(dir) {
            None if dir.is_empty() => self.root.list(None),
            None => return Ok(true),
            Some(path) => match path.as_rel() {
                Some(rel) => self.root.list(Some(rel)),
                None => return Ok(true),
            },
        };
        // Replaced or removed since it was seen, or unreadable: nothing to
        // list here (a path through a symbolic link is never followed).
        let Ok(mut entries) = listed else {
            return Ok(true);
        };
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let ignore_file = entries
            .iter()
            .any(|e| e.name == b".gitignore" && e.kind == FileKind::File);
        let content = if ignore_file {
            self.read_ignore_file(&join(dir, b".gitignore"))
        } else {
            None
        };
        // One list per directory level, so that pop matches push.
        let source = join(dir, b".gitignore");
        self.search.add_patterns_buffer(
            content.as_deref().unwrap_or_default(),
            crate::conversion::os_path(&source),
            Some(Path::new("")),
            Ignore::default(),
        );
        let result = self.entries(dir, entries);
        self.search.patterns.pop();
        result
    }

    fn read_ignore_file(&self, path: &[u8]) -> Option<Vec<u8>> {
        let path = RepoPath::new(path)?;
        crate::compare::hook::read();
        match self.root.read(path.as_rel()?, self.read_bytes) {
            Ok(Read::File { bytes, .. }) => Some(bytes),
            _ => None,
        }
    }

    fn entries(
        &mut self,
        dir: &[u8],
        entries: Vec<lectern_platform::DirEntry>,
    ) -> Result<bool, GitError> {
        for entry in entries {
            if entry.name == b".git" {
                continue;
            }
            let name: Cow<'_, [u8]> = if self.precompose {
                match gix::utils::str::precompose_bstr(Cow::Borrowed(entry.name.as_bstr())) {
                    Cow::Borrowed(_) => Cow::Borrowed(&entry.name[..]),
                    Cow::Owned(name) => Cow::Owned(name.into()),
                }
            } else {
                Cow::Borrowed(&entry.name[..])
            };
            let path = join(dir, &name);
            let covered = self.scope.is_none_or(|s| s.covers(&path));
            if entry.kind == FileKind::Dir {
                if !covered && !self.scope.is_some_and(|s| s.leads_to(&path)) {
                    continue;
                }
                if !self.directory(&path, covered)? {
                    return Ok(false);
                }
                continue;
            }
            if !covered || self.index.entry(&path).is_some() || self.excluded(&path, false)? {
                continue;
            }
            let kind = match entry.kind {
                FileKind::File => Kind::File,
                FileKind::Symlink => Kind::Symlink,
                _ => Kind::Special,
            };
            if !self.push(&path, kind) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A directory entry at `path`.
    fn directory(&mut self, path: &[u8], covered: bool) -> Result<bool, GitError> {
        // A submodule's worktree isn't walked. A tracked file replaced by a
        // directory is: its files are untracked.
        if self.index.entry(path) == Some(Mode::Gitlink) {
            return Ok(true);
        }
        // Untracked files beneath an ignored directory are ignored too, even
        // when it holds tracked files: git can't re-include them.
        if self.excluded(path, true)? {
            return Ok(true);
        }
        if covered && !self.index.has_beneath(path) && self.is_repository(path) {
            return Ok(self.push(path, Kind::Repository));
        }
        self.dir(path)
    }

    fn is_repository(&self, dir: &[u8]) -> bool {
        let dot_git = join(dir, b".git");
        let Some(path) = RepoPath::new(&dot_git) else {
            return false;
        };
        let Some(rel) = path.as_rel() else {
            return false;
        };
        matches!(
            self.root.stat(rel).map(|m| m.kind),
            Ok(FileKind::Dir | FileKind::File)
        )
    }

    /// `.gitignore` files from the root down to the path's directory (the
    /// deepest wins, the last matching pattern of a file wins), then
    /// `info/exclude` and `core.excludesFile`.
    fn excluded(&mut self, path: &[u8], is_dir: bool) -> Result<bool, GitError> {
        if let Some(m) =
            self.search
                .pattern_matching_relative_path(path.as_bstr(), Some(is_dir), self.case)
        {
            return Ok(!m.pattern.is_negative());
        }
        let mode = if is_dir {
            gix::index::entry::Mode::DIR
        } else {
            gix::index::entry::Mode::FILE
        };
        let bstr: &BStr = path.as_bstr();
        Ok(self.globals.at_entry(bstr, Some(mode))?.is_excluded())
    }

    /// Adds an entry; false once the limit is reached.
    fn push(&mut self, path: &[u8], kind: Kind) -> bool {
        let Some(path) = RepoPath::new(path) else {
            return true;
        };
        if self.found.len() >= self.limit {
            self.capped = true;
            return false;
        }
        self.found.push((path, kind));
        true
    }
}
