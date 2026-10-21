//! One round of the scan loop: the active worktree's file list and
//! sub-projects, and `resolve_package` for the others.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_git::{GitError, HashKind};
use lectern_platform::SafeRoot;
use lectern_services::project::{
    ActiveTree, ChangedParts, DirTable, FileList, ManifestError, ManifestKind, Package,
    ProjectEvent, ProjectSnapshot, Scan, ScanError, Worktree, WorktreeKey,
};

use super::cache::{OtherSlot, OtherTree, Print};
use super::watch::{Manifests, TreeRead};
use super::{Model, Project, RepoInfo, Wanted, index_error, lock, worktree_error};
use crate::ManifestResult;
use crate::folder::{self, ReadError};
use crate::manifest::{self, MAX_MANIFEST_BYTES};

/// Reads one worktree file: `(rel, limit)`, as `TreeAccess::read_file`.
pub(super) type ReadFn<'a> = dyn Fn(&str, u64) -> Result<Option<Vec<u8>>, ReadError> + 'a;

/// A manifest's result kept from the last round, or None to read it.
pub(super) type Reuse<'a> = dyn Fn(&str) -> Option<Option<ManifestResult>> + 'a;

impl Model {
    /// Brings the project to `wanted`: the worktree list and the active
    /// tree first, then (unless superseded) its files and sub-projects.
    pub(super) fn apply(&self, project: &Arc<Project>, wanted: Wanted) {
        if let Some(repo) = &project.repo {
            self.watch_repo(project, repo);
        }
        let old = project.snapshot();
        let worktrees = wanted.worktrees.unwrap_or_else(|| old.worktrees.clone());
        let current = old.active.as_ref();
        let active = match &project.repo {
            None => current.cloned(),
            Some(_) => {
                let target = pick(&worktrees, wanted.active.as_ref(), current);
                target.map(|w| match current {
                    // The same worktree keeps its lists until the scan
                    // replaces them; the table goes with the old file list
                    // then (`keep_dirs`).
                    Some(a) if a.worktree.as_ref() == Some(&w.key) && *a.root == *w.root => {
                        ActiveTree {
                            opened_at: wanted.opened_at.clone(),
                            ..a.clone()
                        }
                    }
                    // Another worktree: its files are pending; the old file
                    // list goes with the old snapshot.
                    _ => ActiveTree {
                        root: w.root.clone(),
                        worktree: Some(w.key.clone()),
                        opened_at: wanted.opened_at.clone(),
                        files: Scan::Pending,
                        packages: Scan::Pending,
                        dirs: Scan::Pending,
                    },
                })
            }
        };
        let next = ProjectSnapshot {
            worktrees,
            active,
            ..(*old).clone()
        };
        let Some(active) = next.active.clone() else {
            // Nothing to scan (or to watch): this replacement is the
            // `Changed` that follows `Opened`.
            self.unwatch_tree(project);
            self.publish(project, next);
            self.submit_facts();
            return;
        };
        if next != *old && !self.publish(project, next) {
            return;
        }
        self.scan(project, &active);
    }

    fn scan(&self, project: &Arc<Project>, active: &ActiveTree) {
        let name = project.snapshot().name.clone();
        let root = &*active.root;
        let result = match (&project.repo, &active.worktree) {
            (Some(repo), Some(worktree)) => self.scan_git(project, repo, worktree, root, &name),
            _ => self.scan_folder(project, root, &name),
        };
        let Some(packages) = result else {
            return;
        };
        if project.superseded() {
            return;
        }
        let snapshot = project.snapshot();
        let mut next = (*snapshot).clone();
        if let Some(a) = next.active.as_mut() {
            a.packages = Scan::Ready(packages.into());
        }
        self.publish(project, next);
    }

    /// The file list from the index, published at once, then the
    /// sub-projects. None when the scan stopped (superseded or failed).
    /// With a watcher, the active worktree's handle is kept with its watch
    /// and what this scan read is recorded for the refreshes (`watch.rs`).
    fn scan_git(
        &self,
        project: &Arc<Project>,
        repo: &RepoInfo,
        worktree: &WorktreeKey,
        root: &Path,
        name: &str,
    ) -> Option<Vec<Package>> {
        let tree = match self.tree_handle(project, repo, worktree, root) {
            Ok(tree) => tree,
            Err(err) => {
                self.publish_files(project, Err(worktree_error(&err)));
                return None;
            }
        };
        let (files, checksum) = match tree.index_files(&project.cancel) {
            Ok(index) => (Arc::new(index.files), index.checksum),
            Err(GitError::Cancelled) => return None,
            Err(err) => {
                // Unknown checksum: the next index change reads it again.
                self.record_tree(project, worktree, TreeRead::default());
                self.publish_files(project, Err(index_error(&err)));
                return None;
            }
        };
        if !self.publish_files(project, Ok(files.clone())) {
            return None;
        }
        let read = |rel: &str, limit: u64| tree.read_file(rel, limit).map_err(read_error);
        let (packages, manifests) = self.packages(&files, name, repo.hash, &read, &|_| None);
        if !project.superseded() {
            let read = TreeRead {
                checksum: Some(checksum),
                manifests: Arc::new(manifests),
            };
            self.record_tree(project, worktree, read);
        }
        Some(packages)
    }

    fn scan_folder(&self, project: &Arc<Project>, root: &Path, name: &str) -> Option<Vec<Package>> {
        let files = match folder::walk(root, self.folder_limit, &project.cancel) {
            Ok(files) => Arc::new(files),
            Err(err) => {
                self.publish_files(project, Err(err));
                return None;
            }
        };
        if !self.publish_files(project, Ok(files.clone())) {
            return None;
        }
        let safe = SafeRoot::open(root).ok();
        let read = |rel: &str, limit: u64| match &safe {
            Some(safe) => folder::read_file(safe, rel, limit),
            None => Err(ReadError::Unreadable),
        };
        // Folder projects have no repository hash: SHA-1.
        Some(
            self.packages(&files, name, HashKind::Sha1, &read, &|_| None)
                .0,
        )
    }

    /// Publishes the active tree's file list (sub-projects pending), or the
    /// error for both, and submits the facts. False when superseded.
    fn publish_files(
        &self,
        project: &Arc<Project>,
        files: Result<Arc<FileList>, ScanError>,
    ) -> bool {
        if project.superseded() {
            return false;
        }
        let failed = files.is_err();
        let snapshot = project.snapshot();
        let mut next = (*snapshot).clone();
        if let Some(a) = next.active.as_mut() {
            match files {
                Ok(files) => {
                    a.files = Scan::Ready(files);
                    a.packages = Scan::Pending;
                }
                Err(error) => {
                    a.files = Scan::Unavailable(error.clone());
                    a.packages = Scan::Unavailable(error);
                }
            }
        }
        let published = self.publish(project, next);
        if published {
            self.submit_facts();
        }
        published && !failed
    }

    /// Replaces the snapshot and emits `Changed`. False once the project is
    /// closed. The directory table follows the file list (`keep_dirs`): a
    /// new list makes it pending and queues its build.
    pub(super) fn publish(&self, project: &Arc<Project>, next: ProjectSnapshot) -> bool {
        self.replace(project, |_| Some(next))
    }

    /// `publish` of what `make` builds from the current snapshot, under the
    /// snapshot's lock; None replaces nothing. False when nothing was
    /// replaced.
    fn replace(
        &self,
        project: &Arc<Project>,
        make: impl FnOnce(&ProjectSnapshot) -> Option<ProjectSnapshot>,
    ) -> bool {
        let _emit = lock(&project.emit);
        if lock(&project.control).closed {
            return false;
        }
        let (generation, parts, build) = {
            let mut current = lock(&project.snapshot);
            let Some(mut next) = make(&current) else {
                return false;
            };
            let build = keep_dirs(&current, &mut next);
            next.generation = current.generation + 1;
            let parts = changed(&current, &next);
            *current = Arc::new(next);
            (current.generation, parts, build)
        };
        self.emit(ProjectEvent::Changed {
            project: project.key.clone(),
            generation,
            parts,
        });
        if let Some(list) = build {
            let project = project.clone();
            self.spawn(move |model| model.build_dirs(&project, &list));
        }
        true
    }

    /// The background task that builds the directory table of `list` and
    /// publishes it as its own generation (file_reader design「目录表」).
    /// Nothing is published when the project closed or its file list was
    /// replaced in the meantime: the newer list queued its own build.
    fn build_dirs(&self, project: &Arc<Project>, list: &Arc<FileList>) {
        if !waiting_for(&project.snapshot(), list) {
            return;
        }
        let table = Arc::new(DirTable::new(list.clone()));
        self.replace(project, |current| {
            if !waiting_for(current, list) {
                return None;
            }
            let mut next = current.clone();
            if let Some(a) = next.active.as_mut() {
                a.dirs = Scan::Ready(table);
            }
            Some(next)
        });
    }

    /// Reads and parses the manifests of `files` (parse results cached by
    /// blob OID) and resolves the sub-projects. `reuse` gives the result of
    /// a manifest that need not be read again (a refresh's unchanged ones),
    /// None to read it. Returns every manifest's result too.
    pub(super) fn packages(
        &self,
        files: &FileList,
        root_name: &str,
        hash: HashKind,
        read: &ReadFn<'_>,
        reuse: &Reuse<'_>,
    ) -> (Vec<Package>, Manifests) {
        let seen = RefCell::new(Manifests::new());
        let manifest = |path: &str| -> Option<ManifestResult> {
            let result = match reuse(path) {
                Some(kept) => kept,
                None => self.read_manifest(path, hash, read),
            };
            seen.borrow_mut().insert(path.into(), result.clone());
            result
        };
        let packages = crate::resolve_packages(files, root_name, &manifest);
        (packages, seen.into_inner())
    }

    fn read_manifest(
        &self,
        path: &str,
        hash: HashKind,
        read: &ReadFn<'_>,
    ) -> Option<ManifestResult> {
        let file_name = path.rsplit_once('/').map_or(path, |(_, n)| n);
        let kind = ManifestKind::from_file_name(file_name)?;
        let bytes = match read(path, MAX_MANIFEST_BYTES as u64) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return None,
            Err(ReadError::TooLarge) => return Some(Err(ManifestError::TooLarge)),
            Err(ReadError::Unreadable) => return Some(Err(ManifestError::Unreadable)),
        };
        Some(self.parse(kind, hash, &bytes))
    }

    fn parse(&self, kind: ManifestKind, hash: HashKind, bytes: &[u8]) -> ManifestResult {
        let oid = self.git.hash_blob(hash, bytes);
        if let Some(hit) = lock(&self.parsed).get(kind, oid) {
            return hit;
        }
        #[cfg(test)]
        self.parses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let result = manifest::parse(kind, bytes).map(Arc::new);
        lock(&self.parsed).insert(kind, oid, result.clone(), bytes.len());
        result
    }

    /// The sub-projects of a worktree that is not active, from the cache
    /// when its index checksum and every manifest's `lstat` are unchanged.
    /// `active` is the active tree's list, when ready: worktrees of one
    /// repository mostly have the same sub-projects, and an equal list is
    /// kept once.
    pub(super) fn other_packages(
        &self,
        repo: &RepoInfo,
        worktree: &Worktree,
        root_name: &str,
        active: Option<&Arc<[Package]>>,
    ) -> Result<Arc<[Package]>, ScanError> {
        let slot = lock(&self.others).slot(&worktree.key.0);
        let mut cached = lock(&slot);
        let tree = self
            .git
            .open_worktree(&repo.common_dir, &worktree.key.0)
            .map_err(|e| worktree_error(&e))?;
        if let Some(c) = cached.as_ref() {
            let checksum = tree.index_checksum().map_err(|e| index_error(&e))?;
            let unchanged = checksum == c.checksum
                && c.prints
                    .iter()
                    .all(|(rel, print)| Print::of(&worktree.root.join(&**rel)) == *print);
            if unchanged {
                return Ok(c.packages.clone());
            }
        }
        let never = AtomicBool::new(false);
        let index = tree.index_files(&never).map_err(|e| index_error(&e))?;
        let prints = RefCell::new(Vec::new());
        let read = |rel: &str, limit: u64| {
            // Before the read: a write in between shows at the next check.
            let print = Print::of(&worktree.root.join(rel));
            prints.borrow_mut().push((Box::<str>::from(rel), print));
            tree.read_file(rel, limit).map_err(read_error)
        };
        let packages: Arc<[Package]> = self
            .packages(&index.files, root_name, repo.hash, &read, &|_| None)
            .0
            .into();
        let packages = self.share_packages(packages, active, &slot);
        *cached = Some(OtherTree {
            checksum: index.checksum,
            packages: packages.clone(),
            prints: prints.into_inner(),
        });
        Ok(packages)
    }
}

impl Model {
    /// The same list already held, by the active tree or another cached
    /// worktree, when one is equal to `packages`; else `packages`. The
    /// caller holds `own`'s lock; the other slots are only tried, so two
    /// computations never wait on each other.
    fn share_packages(
        &self,
        packages: Arc<[Package]>,
        active: Option<&Arc<[Package]>>,
        own: &OtherSlot,
    ) -> Arc<[Package]> {
        if let Some(active) = active
            && **active == *packages
        {
            return active.clone();
        }
        let slots = lock(&self.others).others(own);
        for slot in slots {
            let Ok(other) = slot.try_lock() else {
                continue;
            };
            if let Some(tree) = other.as_ref()
                && *tree.packages == *packages
            {
                return tree.packages.clone();
            }
        }
        packages
    }
}

/// The worktree to make active: the wanted one when it is listed and not
/// prunable; else the current one; else the main worktree or the first one
/// available.
pub(super) fn pick<'a>(
    worktrees: &'a [Worktree],
    wanted: Option<&lectern_services::project::WorktreeKey>,
    current: Option<&ActiveTree>,
) -> Option<&'a Worktree> {
    let usable = |key: Option<&lectern_services::project::WorktreeKey>| {
        key.and_then(|key| worktrees.iter().find(|w| w.key == *key && !w.prunable))
    };
    usable(wanted)
        .or_else(|| usable(current.and_then(|a| a.worktree.as_ref())))
        .or_else(|| worktrees.iter().find(|w| !w.prunable))
}

/// Makes `next`'s directory table agree with its file list, given the
/// snapshot it replaces; Some(list) when a table must be built for `list`.
///
/// - `files` pending: `dirs` pending. Unavailable: `dirs` unavailable with
///   the same error, as `packages`.
/// - `files` ready with a table built from that very list (`Arc::ptr_eq`):
///   kept. The list `current` already holds: `current`'s `dirs` (its
///   table, or pending while the build queued with that list runs), so a
///   snapshot cloned before the table was published does not drop it.
/// - Any other list: `dirs` pending, and its build is queued. An older
///   table never stays next to a newer list.
fn keep_dirs(current: &ProjectSnapshot, next: &mut ProjectSnapshot) -> Option<Arc<FileList>> {
    let a = next.active.as_mut()?;
    let list = match &a.files {
        Scan::Pending => {
            a.dirs = Scan::Pending;
            return None;
        }
        Scan::Unavailable(error) => {
            a.dirs = Scan::Unavailable(error.clone());
            return None;
        }
        Scan::Ready(list) => list,
    };
    if matches!(&a.dirs, Scan::Ready(t) if Arc::ptr_eq(t.files(), list)) {
        return None;
    }
    let held = current.active.as_ref().filter(|c| match &c.files {
        Scan::Ready(files) => Arc::ptr_eq(files, list),
        _ => false,
    });
    if let Some(c) = held {
        a.dirs = c.dirs.clone();
        return None;
    }
    let list = list.clone();
    a.dirs = Scan::Pending;
    Some(list)
}

/// `snapshot`'s file list is `list` and its directory table is not built.
fn waiting_for(snapshot: &ProjectSnapshot, list: &Arc<FileList>) -> bool {
    snapshot.active.as_ref().is_some_and(|a| {
        matches!(&a.files, Scan::Ready(files) if Arc::ptr_eq(files, list))
            && matches!(a.dirs, Scan::Pending)
    })
}

fn changed(old: &ProjectSnapshot, new: &ProjectSnapshot) -> ChangedParts {
    let tree = |s: &ProjectSnapshot| {
        s.active
            .as_ref()
            .map(|a| (a.root.clone(), a.worktree.clone()))
    };
    let files = |s: &ProjectSnapshot| s.active.as_ref().map(|a| a.files.clone());
    let packages = |s: &ProjectSnapshot| s.active.as_ref().map(|a| a.packages.clone());
    let dirs = |s: &ProjectSnapshot| s.active.as_ref().map(|a| a.dirs.clone());
    ChangedParts {
        worktrees: old.worktrees != new.worktrees,
        active: tree(old) != tree(new),
        files: files(old) != files(new),
        packages: packages(old) != packages(new),
        dirs: dirs(old) != dirs(new),
    }
}

pub(super) fn read_error(err: GitError) -> ReadError {
    match err {
        GitError::TooLarge { .. } => ReadError::TooLarge,
        _ => ReadError::Unreadable,
    }
}
