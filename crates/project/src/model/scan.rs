//! One round of the scan loop: the active worktree's file list and
//! sub-projects, and `resolve_package` for the others.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_git::{GitError, HashKind};
use lectern_platform::SafeRoot;
use lectern_services::project::{
    ActiveTree, ChangedParts, FileList, ManifestError, ManifestKind, Package, ProjectEvent,
    ProjectSnapshot, Scan, ScanError, Worktree,
};

use super::cache::{OtherTree, Print};
use super::{Model, Project, RepoInfo, Wanted, index_error, lock, worktree_error};
use crate::ManifestResult;
use crate::folder::{self, ReadError};
use crate::manifest::{self, MAX_MANIFEST_BYTES};

/// Reads one worktree file: `(rel, limit)`, as `TreeAccess::read_file`.
type ReadFn<'a> = dyn Fn(&str, u64) -> Result<Option<Vec<u8>>, ReadError> + 'a;

impl Model {
    /// Brings the project to `wanted`: the worktree list and the active
    /// tree first, then (unless superseded) its files and sub-projects.
    pub(super) fn apply(&self, project: &Arc<Project>, wanted: Wanted) {
        let old = project.snapshot();
        let worktrees = wanted.worktrees.unwrap_or_else(|| old.worktrees.clone());
        let current = old.active.as_ref();
        let active = match &project.repo {
            None => current.cloned(),
            Some(_) => {
                let target = pick(&worktrees, wanted.active.as_ref(), current);
                target.map(|w| match current {
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
            // Nothing to scan: this replacement is the `Changed` that
            // follows `Opened`.
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
            (Some(repo), Some(worktree)) => self.scan_git(project, repo, &worktree.0, &name),
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
    fn scan_git(
        &self,
        project: &Arc<Project>,
        repo: &RepoInfo,
        git_dir: &Path,
        name: &str,
    ) -> Option<Vec<Package>> {
        let tree = match self.git.open_worktree(&repo.common_dir, git_dir) {
            Ok(tree) => tree,
            Err(err) => {
                self.publish_files(project, Err(worktree_error(&err)));
                return None;
            }
        };
        let files = match tree.index_files(&project.cancel) {
            Ok(index) => Arc::new(index.files),
            Err(GitError::Cancelled) => return None,
            Err(err) => {
                self.publish_files(project, Err(index_error(&err)));
                return None;
            }
        };
        if !self.publish_files(project, Ok(files.clone())) {
            return None;
        }
        let read = |rel: &str, limit: u64| tree.read_file(rel, limit).map_err(read_error);
        Some(self.packages(&files, name, repo.hash, &read))
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
        Some(self.packages(&files, name, HashKind::Sha1, &read))
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
    /// closed.
    fn publish(&self, project: &Arc<Project>, mut next: ProjectSnapshot) -> bool {
        let _emit = lock(&project.emit);
        if lock(&project.control).closed {
            return false;
        }
        let parts = {
            let mut current = lock(&project.snapshot);
            next.generation = current.generation + 1;
            let parts = changed(&current, &next);
            *current = Arc::new(next);
            (current.generation, parts)
        };
        self.emit(ProjectEvent::Changed {
            project: project.key.clone(),
            generation: parts.0,
            parts: parts.1,
        });
        true
    }

    /// Reads and parses the manifests of `files` (parse results cached by
    /// blob OID) and resolves the sub-projects.
    pub(super) fn packages(
        &self,
        files: &FileList,
        root_name: &str,
        hash: HashKind,
        read: &ReadFn<'_>,
    ) -> Vec<Package> {
        let manifest = |path: &str| -> Option<ManifestResult> {
            let file_name = path.rsplit_once('/').map_or(path, |(_, n)| n);
            let kind = ManifestKind::from_file_name(file_name)?;
            let bytes = match read(path, MAX_MANIFEST_BYTES as u64) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return None,
                Err(ReadError::TooLarge) => return Some(Err(ManifestError::TooLarge)),
                Err(ReadError::Unreadable) => return Some(Err(ManifestError::Unreadable)),
            };
            Some(self.parse(kind, hash, &bytes))
        };
        crate::resolve_packages(files, root_name, &manifest)
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
    pub(super) fn other_packages(
        &self,
        repo: &RepoInfo,
        worktree: &Worktree,
        root_name: &str,
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
            .packages(&index.files, root_name, repo.hash, &read)
            .into();
        *cached = Some(OtherTree {
            checksum: index.checksum,
            packages: packages.clone(),
            prints: prints.into_inner(),
        });
        Ok(packages)
    }
}

/// The worktree to make active: the wanted one when it is listed and not
/// prunable; else the current one; else the main worktree or the first one
/// available.
fn pick<'a>(
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

fn changed(old: &ProjectSnapshot, new: &ProjectSnapshot) -> ChangedParts {
    let tree = |s: &ProjectSnapshot| {
        s.active
            .as_ref()
            .map(|a| (a.root.clone(), a.worktree.clone()))
    };
    let files = |s: &ProjectSnapshot| s.active.as_ref().map(|a| a.files.clone());
    let packages = |s: &ProjectSnapshot| s.active.as_ref().map(|a| a.packages.clone());
    ChangedParts {
        worktrees: old.worktrees != new.worktrees,
        active: tree(old) != tree(new),
        files: files(old) != files(new),
        packages: packages(old) != packages(new),
    }
}

fn read_error(err: GitError) -> ReadError {
    match err {
        GitError::TooLarge { .. } => ReadError::TooLarge,
        _ => ReadError::Unreadable,
    }
}
