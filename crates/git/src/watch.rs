//! `fs_watch`'s `WorktreeSource` for a worktree (docs/design/fs_watch.md
//! 「对其他 crate 的要求」, docs/design/git.md「index」).
//!
//! The filter is the index's `TrackedPaths` (shared with
//! [`Worktree::tracked`]) plus ignore rules read once when it is built: every
//! `.gitignore` the untracked walk reads through `SafeRoot` (tracked or not,
//! never in an ignored directory, ADR 0028), the tracked ones and the
//! explicitly given ones it didn't reach, `info/exclude` and
//! `core.excludesFile`. Queries only match in memory.
//!
//! The config list comes from the origins of the sections gix loaded and
//! every `include.path` / `includeIf.*.path` value in them, plus the standard
//! locations; no git CLI runs.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gix::bstr::{BStr, ByteSlice};
use gix::glob::pattern::Case;
use gix::glob::search::pattern::List;
use gix::ignore::search::Ignore;
use lectern_fs_watch::{PathFilter, SourceError, Tracked, WatchPath, WorktreeName, WorktreeSource};
use lectern_platform::{FileKind, Read};

use crate::changes::{Blob, read_blob};
use crate::handle::{ConfigScope, Handle};
use crate::untracked::{self, IgnoreFile};
use crate::{
    EntryLimits, GitError, Limits, Mode, RepoPath, TrackedEntry, TrackedPaths, Worktree, WorktreeId,
};

/// `Worktree::watch_source`'s value.
pub(crate) struct Source {
    worktree: Worktree,
}

impl Source {
    pub(crate) fn new(worktree: Worktree) -> Source {
        Source { worktree }
    }
}

fn source_error(err: GitError) -> SourceError {
    SourceError(err.to_string())
}

impl WorktreeSource for Source {
    fn filter(
        &self,
        gitignores: &[WatchPath],
        cancel: &AtomicBool,
    ) -> Result<Arc<dyn PathFilter>, SourceError> {
        let filter = Filter::build(
            &self.worktree,
            gitignores,
            Limits::default().untracked,
            cancel,
        )
        .map_err(source_error)?;
        Ok(Arc::new(filter))
    }

    fn config_files(&self, cancel: &AtomicBool) -> Result<Vec<PathBuf>, SourceError> {
        let scope = self.worktree.repo().scope();
        config_files(&self.worktree, &Env::of(scope), scope, cancel).map_err(source_error)
    }
}

impl From<WorktreeId> for WorktreeName {
    fn from(id: WorktreeId) -> WorktreeName {
        match id {
            WorktreeId::Main => WorktreeName::Main,
            WorktreeId::Linked(name) => WorktreeName::Linked(name),
        }
    }
}

impl From<WorktreeName> for WorktreeId {
    fn from(name: WorktreeName) -> WorktreeId {
        match name {
            WorktreeName::Main => WorktreeId::Main,
            WorktreeName::Linked(name) => WorktreeId::Linked(name),
        }
    }
}

/// The index paths, looked up exactly or, with `core.ignoreCase`, with ASCII
/// case folding as git's case-insensitive index lookup does.
struct TrackedView {
    paths: Arc<TrackedPaths>,
    /// With `core.ignoreCase`: the entries' positions sorted by their
    /// ASCII-lowercased paths. Four bytes per entry; the paths themselves
    /// stay shared with `project`.
    folded: Option<Vec<u32>>,
}

fn entries(paths: &TrackedPaths) -> &[TrackedEntry] {
    paths.entries()
}

/// `a` against `b` with ASCII case folding.
fn cmp_folded(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    a.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(b.iter().map(u8::to_ascii_lowercase))
}

impl TrackedView {
    fn new(paths: Arc<TrackedPaths>, ignore_case: bool) -> TrackedView {
        let folded = ignore_case.then(|| {
            let all = entries(&paths);
            let mut order: Vec<u32> = (0..all.len() as u32).collect();
            order.sort_by(|&a, &b| {
                cmp_folded(
                    all[a as usize].path.as_bytes(),
                    all[b as usize].path.as_bytes(),
                )
            });
            order
        });
        TrackedView { paths, folded }
    }

    fn find(&self, path: &[u8], fold: bool) -> Option<&TrackedEntry> {
        let all = entries(&self.paths);
        if let Ok(i) = all.binary_search_by(|e| e.path.as_bytes().cmp(path)) {
            return Some(&all[i]);
        }
        let folded = self.folded.as_ref().filter(|_| fold)?;
        let at =
            folded.partition_point(|&i| cmp_folded(all[i as usize].path.as_bytes(), path).is_lt());
        let entry = &all[*folded.get(at)? as usize];
        entry
            .path
            .as_bytes()
            .eq_ignore_ascii_case(path)
            .then_some(entry)
    }

    fn has_beneath(&self, dir: &[u8], fold: bool) -> bool {
        if self.paths.contains_dir(dir) {
            return true;
        }
        let Some(folded) = self.folded.as_ref().filter(|_| fold) else {
            return false;
        };
        let all = entries(&self.paths);
        let mut prefix = dir.to_vec();
        prefix.push(b'/');
        let at = folded
            .partition_point(|&i| cmp_folded(all[i as usize].path.as_bytes(), &prefix).is_lt());
        folded.get(at).is_some_and(|&i| {
            let path = all[i as usize].path.as_bytes();
            path.len() > prefix.len() && path[..prefix.len()].eq_ignore_ascii_case(&prefix)
        })
    }
}

impl untracked::Index for TrackedView {
    fn entry(&self, path: &[u8], fold: bool) -> Option<Mode> {
        self.find(path, fold).map(|e| e.mode)
    }

    fn has_beneath(&self, dir: &[u8], fold: bool) -> bool {
        TrackedView::has_beneath(self, dir, fold)
    }
}

/// The parent directory of a `/`-separated path; None for one component.
fn parent(path: &[u8]) -> Option<&[u8]> {
    path.iter().rposition(|&b| b == b'/').map(|i| &path[..i])
}

fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'/').next().unwrap_or(path)
}

/// The filter `fs_watch` classifies worktree events with.
pub(crate) struct Filter {
    index: TrackedView,
    case: Case,
    precompose: bool,
    /// One pattern list per `.gitignore` read, keyed by its directory (empty
    /// for the root; ASCII-lowercased with `core.ignoreCase`).
    lists: HashMap<Box<[u8]>, List<Ignore>>,
    /// `info/exclude`, then `core.excludesFile` (or its default).
    globals: gix::ignore::Search,
    rule_files: Vec<WatchPath>,
}

impl Filter {
    /// Reads the index and the ignore rules. `walk_limit` bounds the
    /// untracked walk that finds untracked `.gitignore` files (the
    /// comparison's untracked limit); `gitignores` and the tracked
    /// `.gitignore` files it didn't reach are read afterwards, unless a
    /// directory above them is ignored or a submodule.
    pub(crate) fn build(
        worktree: &Worktree,
        gitignores: &[WatchPath],
        walk_limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Filter, GitError> {
        let paths = worktree.tracked(&EntryLimits::default(), cancel)?;
        let repo = worktree.repo();
        let handle = Handle::open(worktree.git_dir(), repo.scope())?;
        let config = handle.config_snapshot();
        let ignore_case = config.boolean("core.ignoreCase").unwrap_or(false);
        let precompose = config.boolean("core.precomposeUnicode").unwrap_or(false);
        let env = Env::of(repo.scope());
        let excludes_file = match config.trusted_path("core.excludesFile") {
            Ok(Some(path)) => Some(path),
            Ok(None) => env.xdg_config("ignore"),
            Err(err) => return Err(GitError::from_gix(err)),
        };
        let globals = gix::ignore::Search::from_git_dir(
            &repo.location().common_dir,
            excludes_file,
            &mut Vec::new(),
            Ignore::default(),
        )?;

        let tracked = TrackedView::new(paths, ignore_case);
        let read_bytes = Limits::default().read_bytes;
        let mut read: Vec<IgnoreFile> = Vec::new();
        untracked::walk(
            &handle,
            repo.hash(),
            worktree.safe_root(),
            &tracked,
            None,
            walk_limit,
            read_bytes,
            cancel,
            Some(&mut read),
        )?;

        let mut filter = Filter {
            index: tracked,
            case: if ignore_case {
                Case::Fold
            } else {
                Case::Sensitive
            },
            precompose,
            lists: HashMap::new(),
            globals,
            rule_files: Vec::new(),
        };
        let mut seen: HashSet<RepoPath> = HashSet::new();
        for (path, content) in read {
            filter.add(&path, &content);
            seen.insert(path);
        }

        // Tracked `.gitignore` files the walk didn't reach (it stopped at its
        // limit, or the directory is outside a sparse checkout) and the ones
        // the triggering events named. Parents before children, so that a
        // directory's rules are in place when its subdirectories are checked.
        let tracked_paths = Arc::clone(&filter.index.paths);
        let mut candidates: Vec<(RepoPath, Option<&TrackedEntry>)> = Vec::new();
        for entry in entries(&tracked_paths) {
            if basename(entry.path.as_bytes()) == b".gitignore" && !seen.contains(&entry.path) {
                candidates.push((entry.path.clone(), Some(entry)));
            }
        }
        for path in gitignores {
            let Some(path) = RepoPath::new(path.as_bytes()) else {
                continue;
            };
            if basename(path.as_bytes()) == b".gitignore" && !seen.contains(&path) {
                candidates.push((path, None));
            }
        }
        let dir_of = |p: &RepoPath| parent(p.as_bytes()).unwrap_or_default().to_vec();
        candidates.sort_by(|a, b| dir_of(&a.0).cmp(&dir_of(&b.0)).then(a.0.cmp(&b.0)));
        // A tracked candidate sorts before an explicit one for the same
        // path (stable sort) and is the one kept.
        candidates.dedup_by(|a, b| a.0 == b.0);
        for (path, entry) in candidates {
            if cancel.load(Ordering::Relaxed) {
                return Err(GitError::Cancelled);
            }
            if !filter.reachable(worktree, path.as_bytes()) {
                continue;
            }
            let entry = entry.or_else(|| {
                let all = entries(&tracked_paths);
                all.binary_search_by(|e| e.path.cmp(&path))
                    .ok()
                    .map(|i| &all[i])
            });
            if let Some(content) = read_ignore_file(worktree, &handle, &path, entry, read_bytes)? {
                filter.add(&path, &content);
            }
        }
        filter.rule_files.sort();
        filter.rule_files.dedup();
        Ok(filter)
    }

    fn key(&self, dir: &[u8]) -> Box<[u8]> {
        match self.case {
            Case::Fold => dir.to_ascii_lowercase().into(),
            Case::Sensitive => dir.into(),
        }
    }

    fn add(&mut self, path: &RepoPath, content: &[u8]) {
        let dir = parent(path.as_bytes()).unwrap_or_default();
        let list = List::from_bytes(
            content,
            crate::conversion::os_path(path.as_bytes()),
            Some(Path::new("")),
            Ignore::default(),
        );
        self.lists.insert(self.key(dir), list);
        if let Some(path) = WatchPath::new(path.as_bytes()) {
            self.rule_files.push(path);
        }
    }

    /// Whether git would read the `.gitignore` at `path`: no directory above
    /// it is a submodule, a nested repository or ignored (checked from the
    /// top, as fs_watch does).
    fn reachable(&self, worktree: &Worktree, path: &[u8]) -> bool {
        let Some(dir) = parent(path) else {
            return true;
        };
        let mut end = 0;
        while end < dir.len() {
            end = dir[end..]
                .iter()
                .position(|&b| b == b'/')
                .map_or(dir.len(), |i| end + i);
            let ancestor = &dir[..end];
            end += 1;
            if self.tracked(ancestor) == Tracked::Gitlink || self.ignored(ancestor, true) {
                return false;
            }
            if !self.index.has_beneath(ancestor, self.case == Case::Fold)
                && is_repository(worktree, ancestor)
            {
                return false;
            }
        }
        true
    }

    fn normalized<'a>(&self, path: &'a [u8]) -> Cow<'a, [u8]> {
        if !self.precompose {
            return Cow::Borrowed(path);
        }
        match gix::utils::str::precompose_bstr(Cow::Borrowed(path.as_bstr())) {
            Cow::Borrowed(_) => Cow::Borrowed(path),
            Cow::Owned(path) => Cow::Owned(path.into()),
        }
    }
}

/// A directory holding its own `.git` (what the untracked walk lists as a
/// nested repository and doesn't enter).
fn is_repository(worktree: &Worktree, dir: &[u8]) -> bool {
    let mut dot_git = dir.to_vec();
    dot_git.extend_from_slice(b"/.git");
    let Some(path) = RepoPath::new(&dot_git) else {
        return false;
    };
    let Some(rel) = path.as_rel() else {
        return false;
    };
    matches!(
        worktree.safe_root().stat(rel).map(|m| m.kind),
        Ok(FileKind::Dir | FileKind::File)
    )
}

/// A `.gitignore` the walk didn't read: the worktree file through `SafeRoot`
/// when it is a regular file (never a symbolic link, as git 2.32 and later);
/// otherwise, for a skip-worktree entry, the index blob, as git does.
fn read_ignore_file(
    worktree: &Worktree,
    handle: &Handle,
    path: &RepoPath,
    entry: Option<&TrackedEntry>,
    max: u64,
) -> Result<Option<Vec<u8>>, GitError> {
    let Some(rel) = path.as_rel() else {
        return Ok(None);
    };
    crate::compare::hook::read();
    if let Ok(Read::File { bytes, .. }) = worktree.safe_root().read(rel, max) {
        return Ok(Some(bytes));
    }
    match entry {
        Some(entry)
            if entry.skip_worktree && matches!(entry.mode, Mode::File | Mode::Executable) =>
        {
            match read_blob(handle, entry.oid, max)? {
                Blob::Bytes(bytes) => Ok(Some(bytes)),
                Blob::TooLarge(_) | Blob::Missing => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

impl PathFilter for Filter {
    fn tracked(&self, path: &[u8]) -> Tracked {
        let path = self.normalized(path);
        let fold = self.case == Case::Fold;
        if let Some(entry) = self.index.find(&path, false) {
            return if entry.mode == Mode::Gitlink {
                Tracked::Gitlink
            } else {
                Tracked::File
            };
        }
        if self.index.has_beneath(&path, false) {
            return Tracked::Dir;
        }
        if fold {
            if let Some(entry) = self.index.find(&path, true) {
                return if entry.mode == Mode::Gitlink {
                    Tracked::Gitlink
                } else {
                    Tracked::File
                };
            }
            if self.index.has_beneath(&path, true) {
                return Tracked::Dir;
            }
        }
        Tracked::No
    }

    /// The `.gitignore` files from the path's directory up to the root (the
    /// deepest that matches decides; within a file, the last matching
    /// pattern), then `info/exclude`, then `core.excludesFile`.
    fn ignored(&self, path: &[u8], is_dir: bool) -> bool {
        let path = self.normalized(path);
        let rel: &BStr = path.as_bstr();
        let basename_pos = path.iter().rposition(|&b| b == b'/').map(|i| i + 1);
        let mut dir = parent(&path);
        loop {
            if let Some(list) = self.lists.get(&self.key(dir.unwrap_or_default()))
                && let Some(m) = gix::ignore::search::pattern_matching_relative_path(
                    list,
                    rel,
                    basename_pos,
                    Some(is_dir),
                    self.case,
                )
            {
                return !m.pattern.is_negative();
            }
            let Some(d) = dir else {
                break;
            };
            dir = parent(d);
        }
        self.globals
            .pattern_matching_relative_path(rel, Some(is_dir), self.case)
            .is_some_and(|m| !m.pattern.is_negative())
    }

    fn rule_files(&self) -> &[WatchPath] {
        &self.rule_files
    }
}

/// The two environment variables gix reads to find the configuration
/// outside the repository (`ConfigScope::User` allows exactly these), or
/// none.
#[derive(Clone, Default, Debug)]
pub(crate) struct Env {
    pub(crate) home: Option<PathBuf>,
    pub(crate) xdg_config_home: Option<PathBuf>,
}

impl Env {
    pub(crate) fn of(scope: ConfigScope) -> Env {
        match scope {
            ConfigScope::User => Env {
                home: gix::path::env::home_dir(),
                xdg_config_home: gix::path::env::var("XDG_CONFIG_HOME").map(PathBuf::from),
            },
            ConfigScope::RepositoryOnly => Env::default(),
        }
    }

    /// What gix's environment lookup returns for `name`: only `HOME` and
    /// `XDG_CONFIG_HOME`; every `GIT_*` variable is denied.
    fn var(&self, name: &str) -> Option<OsString> {
        match name {
            "HOME" => self.home.clone().map(Into::into),
            "XDG_CONFIG_HOME" => self.xdg_config_home.clone().map(Into::into),
            _ => None,
        }
    }

    /// `$XDG_CONFIG_HOME/git/<file>`, or `~/.config/git/<file>`.
    fn xdg_config(&self, file: &str) -> Option<PathBuf> {
        gix::path::env::xdg_config(file, &mut |name| self.var(name))
    }
}

/// Every file the worktree's effective configuration, ignore rules and
/// attribute rules read or would read once it appears
/// (docs/design/fs_watch.md「配置文件清单」), absolute, sorted. `scope`
/// says which files outside the repository gix loads; `env` locates them.
pub(crate) fn config_files(
    worktree: &Worktree,
    env: &Env,
    scope: ConfigScope,
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, GitError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(GitError::Cancelled);
    }
    let common = &worktree.repo().location().common_dir;
    let user = scope == ConfigScope::User;
    let mut out = BTreeSet::new();
    out.insert(common.join("config"));
    out.insert(worktree.git_dir().join("config.worktree"));
    out.insert(common.join("info").join("exclude"));
    out.insert(common.join("info").join("attributes"));
    if user {
        // The system, XDG and `~/.gitconfig` locations, even when missing.
        for source in [
            gix::config::Source::System,
            gix::config::Source::Git,
            gix::config::Source::User,
        ] {
            out.extend(source.storage_location(&mut |name| env.var(name)));
        }
        // The system attributes file gix reads (`$(prefix)/etc/gitattributes`).
        out.extend(gix::attrs::Source::System.storage_location(&mut |name| env.var(name)));
    }

    let handle = Handle::open(worktree.git_dir(), worktree.repo().scope())?;
    let install_dir = handle.install_dir().ok();
    let ctx = gix::config::path::interpolate::Context {
        git_install_dir: install_dir.as_deref(),
        home_dir: env.home.as_deref(),
        home_for_user: Some(gix::config::path::interpolate::home_for_user),
    };
    let snapshot = handle.config_snapshot();
    let file = snapshot.plumbing();
    for section in file.sections() {
        let origin = section.meta().path.as_deref();
        if let Some(origin) = origin {
            out.insert(origin.to_owned());
        }
        let header = section.header();
        let name = header.name();
        let include = (name.eq_ignore_ascii_case(b"include") && header.subsection_name().is_none())
            || (name.eq_ignore_ascii_case(b"includeIf") && header.subsection_name().is_some());
        if !include {
            continue;
        }
        // Every target, whether or not its condition holds or it exists.
        for value in section.values("path") {
            let Ok(path) = gix::config::Path::from(value).interpolate(ctx) else {
                continue;
            };
            let path = if path.is_relative() {
                match origin.and_then(Path::parent) {
                    Some(dir) => dir.join(path),
                    None => continue,
                }
            } else {
                path
            };
            out.insert(path);
        }
    }

    // `core.excludesFile` and `core.attributesFile`, or their defaults. An
    // `:(optional)` value that is missing makes gix fall back to the default.
    for (key, default) in [
        ("core.excludesFile", "ignore"),
        ("core.attributesFile", "attributes"),
    ] {
        let fallback = match file.path(key) {
            Some(path) => {
                let optional = path.is_optional;
                if let Ok(path) = path.interpolate(ctx) {
                    out.insert(path);
                }
                optional
            }
            None => true,
        };
        if fallback && user {
            out.extend(env.xdg_config(default));
        }
    }
    Ok(out.into_iter().filter(|p| p.is_absolute()).collect())
}

#[cfg(test)]
mod tests;
