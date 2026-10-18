//! Incremental refresh from fs_watch (docs/design/project.md「依赖 fs_watch
//! 的接口」「数据流与线程」, acceptance step 4).
//!
//! Each open git project has one `RepoWatch` (HEAD changes of every
//! worktree, worktree list changes) and one `WorktreeWatch` on its active
//! worktree (paths filtered by the index, the worktree's index and HEAD).
//! The sinks only mark the project dirty; the scan loop refreshes it:
//!
//! - a worktree's HEAD, the worktree list, a lost repository event or a
//!   failed worktree watch: list the worktrees again; when the active one is
//!   gone or prunable, switch to the main one (else the first available);
//! - the active worktree's index: read its checksum; when it changed and
//!   the file list did too (a path, a kind or a blob OID), rebuild the file
//!   list and the facts and resolve the sub-projects again, keeping every
//!   manifest result fs_watch did not report a change for;
//! - a manifest path (or a directory above one): read it again and resolve
//!   the sub-projects again;
//! - any other path: nothing (the file list comes from the index).
//!
//! The first batch of each watch is a rescan (fs_watch「监听器与句柄」), so
//! whatever changed between a scan and the watch arming is caught by one
//! refresh that rereads every manifest (the parse cache keeps that cheap).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use lectern_fs_watch::{
    Coalesce, Health, RepoBatch, RepoSpec, RepoWatch, Sink, WorktreeBatch, WorktreeSpec,
    WorktreeWatch,
};
use lectern_git::{GitError, Oid};
use lectern_kernel::BusyGuard;
use lectern_services::project::{ActiveTree, Head, Package, ProjectSnapshot, Scan, WorktreeKey};

use super::scan::{pick, read_error};
use super::{Model, Project, RepoInfo, Wanted, index_error, lock, worktree_list};
use crate::ManifestResult;
use crate::git_access::{TreeAccess, worktree_name};

/// Every manifest's result in the last scan of the active tree, by path:
/// None when the file was not in the worktree.
pub(super) type Manifests = HashMap<Box<str>, Option<ManifestResult>>;

/// Above this many dirty paths a refresh treats every manifest as changed.
const MAX_DIRTY_PATHS: usize = 10_000;

/// A project's fs_watch subscriptions. Dropping it unsubscribes.
#[derive(Default)]
pub(super) struct Watches {
    repo: Option<RepoWatch>,
    tree: Option<TreeState>,
}

/// The active worktree's watch, and what its snapshot was built from.
struct TreeState {
    key: WorktreeKey,
    /// A moved worktree keeps its key, not its root: a new root is a new
    /// watch.
    root: PathBuf,
    /// Kept with the watch: the source holds the same handle, so the filter
    /// and the refreshes share one parsed index.
    handle: Arc<dyn TreeAccess>,
    _watch: WorktreeWatch,
    read: TreeRead,
}

/// What a scan of the active tree read.
#[derive(Clone, Default)]
pub(super) struct TreeRead {
    /// The index checksum the file list came from; None when unknown (no
    /// scan yet, or the index could not be read), so the next index change
    /// reads it again.
    pub(super) checksum: Option<Option<Oid>>,
    pub(super) manifests: Arc<Manifests>,
}

/// What fs_watch reported since the last refresh.
#[derive(Default)]
pub(super) struct Dirty {
    /// List the worktrees again.
    relist: bool,
    /// The active worktree's index may have changed.
    index: bool,
    /// Events were lost: every manifest may have changed.
    rescan: bool,
    /// Changed paths in the active worktree (directories stand for every
    /// path below them).
    paths: BTreeSet<Box<str>>,
}

impl Dirty {
    pub(super) fn is_empty(&self) -> bool {
        !self.relist && !self.index && !self.rescan && self.paths.is_empty()
    }

    /// A full scan of the active tree reads everything again; only the
    /// worktree list is left to refresh.
    pub(super) fn covered_by_scan(&mut self) {
        *self = Dirty {
            relist: self.relist,
            ..Dirty::default()
        };
    }

    fn merge(&mut self, other: Dirty) {
        self.relist |= other.relist;
        self.index |= other.index;
        self.rescan |= other.rescan;
        self.paths.extend(other.paths);
        self.cap();
    }

    fn add_tree(&mut self, batch: &WorktreeBatch) {
        if matches!(batch.health, Some(Health::Failed(_))) || batch.meta.head {
            self.relist = true;
        }
        if batch.meta.index {
            self.index = true;
        }
        if batch.rescan {
            self.index = true;
            self.rescan = true;
        }
        for path in &batch.paths {
            // The file list has UTF-8 paths only.
            if let Ok(path) = std::str::from_utf8(path.as_bytes()) {
                self.paths.insert(path.into());
            }
        }
        self.cap();
    }

    fn cap(&mut self) {
        if self.paths.len() > MAX_DIRTY_PATHS {
            self.paths.clear();
            self.rescan = true;
            self.index = true;
        }
    }

    fn tree(&self) -> bool {
        self.index || self.rescan || !self.paths.is_empty()
    }

    /// `rel`, or a directory above it, was reported changed.
    fn touches(&self, rel: &str) -> bool {
        if self.rescan || self.paths.contains(rel) {
            return true;
        }
        let mut end = rel.len();
        while let Some(slash) = rel[..end].rfind('/') {
            if self.paths.contains(&rel[..slash]) {
                return true;
            }
            end = slash;
        }
        false
    }
}

/// The refresh stopped: a request or a close is waiting.
struct Stopped;

impl Model {
    /// Subscribes to the repository's metadata once per project.
    pub(super) fn watch_repo(&self, project: &Arc<Project>, repo: &RepoInfo) {
        let Some(watcher) = self.watcher.get() else {
            return;
        };
        let mut watches = lock(&project.watch);
        if watches.repo.is_some() || lock(&project.control).closed {
            return;
        }
        let sink = Arc::new(RepoSink {
            model: self.me.clone(),
            project: Arc::downgrade(project),
        });
        let spec = RepoSpec {
            common_dir: repo.common_dir.clone(),
            coalesce: Coalesce::REPO,
        };
        watches.repo = Some(watcher.watch_repo(spec, sink));
    }

    /// The active worktree changed to none: its watch goes.
    pub(super) fn unwatch_tree(&self, project: &Project) {
        project.tree_watch.store(0, Ordering::Relaxed);
        let old = lock(&project.watch).tree.take();
        drop(old);
    }

    /// A handle on `worktree` for a scan. With a watcher, the watched
    /// worktree's handle when it is this one; otherwise a new handle, and
    /// the worktree is watched with it (the old watch goes first).
    pub(super) fn tree_handle(
        &self,
        project: &Arc<Project>,
        repo: &RepoInfo,
        worktree: &WorktreeKey,
        root: &Path,
    ) -> Result<Arc<dyn TreeAccess>, GitError> {
        if self.watcher.get().is_some() {
            let watches = lock(&project.watch);
            if let Some(tree) = &watches.tree
                && tree.key == *worktree
                && tree.root == root
            {
                return Ok(tree.handle.clone());
            }
            drop(watches);
            self.unwatch_tree(project);
        }
        let handle: Arc<dyn TreeAccess> =
            Arc::from(self.git.open_worktree(&repo.common_dir, &worktree.0)?);
        self.watch_tree(project, repo, worktree, root, &handle);
        Ok(handle)
    }

    fn watch_tree(
        &self,
        project: &Arc<Project>,
        repo: &RepoInfo,
        worktree: &WorktreeKey,
        root: &Path,
        handle: &Arc<dyn TreeAccess>,
    ) {
        if self.watcher.get().is_none() {
            return;
        }
        let Some(name) = worktree_name(&repo.common_dir, &worktree.0) else {
            return;
        };
        let mut watches = lock(&project.watch);
        if lock(&project.control).closed {
            return;
        }
        let Some(repo_watch) = &watches.repo else {
            return;
        };
        let id = self.next_watch.fetch_add(1, Ordering::Relaxed);
        // Before subscribing: the first batch must not look stale.
        project.tree_watch.store(id, Ordering::Relaxed);
        let sink = Arc::new(TreeSink {
            model: self.me.clone(),
            project: Arc::downgrade(project),
            id,
        });
        let spec = WorktreeSpec {
            name,
            root: root.to_path_buf(),
            source: handle.watch_source(),
            coalesce: Coalesce::WORKTREE,
        };
        let watch = repo_watch.watch_worktree(spec, sink);
        watches.tree = Some(TreeState {
            key: worktree.clone(),
            root: root.to_path_buf(),
            handle: handle.clone(),
            _watch: watch,
            read: TreeRead::default(),
        });
    }

    /// Records what a scan of the active tree read, if it is still the
    /// watched one.
    pub(super) fn record_tree(&self, project: &Project, worktree: &WorktreeKey, read: TreeRead) {
        let mut watches = lock(&project.watch);
        if let Some(tree) = watches.tree.as_mut()
            && tree.key == *worktree
        {
            tree.read = read;
        }
    }

    /// One refresh round for what fs_watch reported.
    pub(super) fn refresh(&self, project: &Arc<Project>, dirty: Dirty) {
        let Some(repo) = project.repo.clone() else {
            return;
        };
        let old = project.snapshot();
        let mut worktrees = old.worktrees.clone();
        if dirty.relist {
            // On failure the old list stays; the next event retries.
            if let Ok(infos) = self.git.worktrees(&repo.common_dir) {
                worktrees = worktree_list(infos);
            }
            let current = old.active.as_ref();
            let target = pick(
                &worktrees,
                current.and_then(|a| a.worktree.as_ref()),
                current,
            );
            let same = match (target, current) {
                (Some(w), Some(a)) => a.worktree.as_ref() == Some(&w.key) && *a.root == *w.root,
                (None, None) => true,
                _ => false,
            };
            if !same {
                // The active worktree is gone: switch, as a request would.
                let active = target.map(|w| w.key.clone());
                let wanted = Wanted {
                    worktrees: Some(worktrees),
                    active,
                    opened_at: "".into(),
                };
                self.apply(project, wanted);
                return;
            }
        }
        let mut next = ProjectSnapshot {
            worktrees,
            ..(*old).clone()
        };
        let mut read = None;
        if dirty.tree()
            && let Some(active) = next.active.as_mut()
        {
            match self.refresh_tree(project, &repo, &old.name, active, &dirty) {
                Ok(r) => read = r,
                Err(Stopped) => return self.restore(project, dirty),
            }
        }
        if project.superseded() {
            return self.restore(project, dirty);
        }
        let files = |s: &ProjectSnapshot| s.active.as_ref().map(|a| a.files.clone());
        let files_changed = files(&old) != files(&next);
        let worktree = next.active.as_ref().and_then(|a| a.worktree.clone());
        if next != *old && !self.publish(project, next) {
            return;
        }
        if let (Some(read), Some(worktree)) = (read, worktree) {
            self.record_tree(project, &worktree, read);
        }
        if files_changed {
            self.submit_facts();
        }
    }

    /// Brings the active tree up to date in `active`. Ok(Some) with what
    /// was read; Ok(None) when there is nothing to refresh (not watched, or
    /// no file list to resolve against).
    fn refresh_tree(
        &self,
        project: &Arc<Project>,
        repo: &RepoInfo,
        name: &str,
        active: &mut ActiveTree,
        dirty: &Dirty,
    ) -> Result<Option<TreeRead>, Stopped> {
        let Some(worktree) = active.worktree.clone() else {
            return Ok(None);
        };
        let (handle, known) = {
            let watches = lock(&project.watch);
            match &watches.tree {
                Some(tree) if tree.key == worktree && *tree.root == *active.root => {
                    (tree.handle.clone(), tree.read.clone())
                }
                _ => return Ok(None),
            }
        };
        let old_files = match &active.files {
            Scan::Ready(files) => Some(files.clone()),
            _ => None,
        };
        let mut files = old_files.clone();
        let mut checksum = known.checksum;
        if dirty.index {
            let now = handle.index_checksum();
            let unchanged =
                files.is_some() && matches!((&now, known.checksum), (Ok(c), Some(k)) if *c == k);
            if !unchanged {
                match handle.index_files(&project.cancel) {
                    Ok(index) => {
                        checksum = Some(index.checksum);
                        // Same paths and OIDs (a stat-only rewrite of the
                        // index): keep the list the snapshot already
                        // shares. `FileList` equality covers the OIDs, so
                        // staging new content alone replaces the list.
                        if old_files.as_deref() != Some(&index.files) {
                            files = Some(Arc::new(index.files));
                        }
                    }
                    Err(GitError::Cancelled) => return Err(Stopped),
                    Err(err) => {
                        let error = index_error(&err);
                        active.files = Scan::Unavailable(error.clone());
                        active.packages = Scan::Unavailable(error);
                        return Ok(Some(TreeRead::default()));
                    }
                }
            }
        }
        let Some(files) = files else {
            return Ok(None);
        };
        let files_changed = !matches!(&old_files, Some(old) if Arc::ptr_eq(old, &files));
        let packages_ready = matches!(active.packages, Scan::Ready(_));
        let touched = known.manifests.keys().any(|m| dirty.touches(m));
        let read = TreeRead {
            checksum,
            manifests: known.manifests.clone(),
        };
        if !files_changed && !touched && packages_ready {
            return Ok(Some(read));
        }
        if project.superseded() {
            return Err(Stopped);
        }
        let read_file = |rel: &str, limit: u64| handle.read_file(rel, limit).map_err(read_error);
        let reuse = |rel: &str| {
            if dirty.touches(rel) {
                None
            } else {
                known.manifests.get(rel).cloned()
            }
        };
        let (packages, manifests) = self.packages(&files, name, repo.hash, &read_file, &reuse);
        let packages: Arc<[Package]> = packages.into();
        active.files = Scan::Ready(files);
        if !matches!(&active.packages, Scan::Ready(old) if *old == packages) {
            active.packages = Scan::Ready(packages);
        }
        Ok(Some(TreeRead {
            checksum,
            manifests: Arc::new(manifests),
        }))
    }

    /// A refresh gave way to a request: what it was handling waits for the
    /// next round (a full scan covers the active tree, not the list).
    fn restore(&self, project: &Project, dirty: Dirty) {
        let mut control = lock(&project.control);
        if !control.closed {
            control.dirty.merge(dirty);
        }
    }
}

/// The active worktree's batches.
struct TreeSink {
    model: Weak<Model>,
    project: Weak<Project>,
    /// The watch this sink belongs to; batches of an older one are dropped.
    id: u64,
}

impl Sink<WorktreeBatch> for TreeSink {
    fn deliver(&self, batch: WorktreeBatch, busy: Option<BusyGuard>) {
        let (Some(model), Some(project)) = (self.model.upgrade(), self.project.upgrade()) else {
            return;
        };
        if project.tree_watch.load(Ordering::Relaxed) != self.id {
            return;
        }
        let quiet = batch.paths.is_empty()
            && !batch.rescan
            && !batch.meta.index
            && !batch.meta.head
            && !matches!(batch.health, Some(Health::Failed(_)));
        if quiet {
            return;
        }
        model.mark(&project, busy, |dirty| dirty.add_tree(&batch));
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.model.upgrade()?.busy()
    }
}

/// The repository's batches: HEADs and the worktree list.
struct RepoSink {
    model: Weak<Model>,
    project: Weak<Project>,
}

impl Sink<RepoBatch> for RepoSink {
    fn deliver(&self, batch: RepoBatch, busy: Option<BusyGuard>) {
        let (Some(model), Some(project)) = (self.model.upgrade(), self.project.upgrade()) else {
            return;
        };
        // A first commit on an unborn branch writes only `refs/`.
        let unborn = || {
            project
                .snapshot()
                .worktrees
                .iter()
                .any(|w| matches!(w.head, Head::Unborn(_)))
        };
        let relist = batch.worktree_list
            || batch.rescan
            || matches!(batch.health, Some(Health::Failed(_)))
            || batch.worktrees.iter().any(|(_, meta)| meta.head)
            || (batch.refs && unborn());
        if relist {
            model.mark(&project, busy, |dirty| dirty.relist = true);
        }
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.model.upgrade()?.busy()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirty(paths: &[&str]) -> Dirty {
        Dirty {
            paths: paths.iter().map(|p| Box::<str>::from(*p)).collect(),
            ..Dirty::default()
        }
    }

    #[test]
    fn refresh_dirty_paths_touch_files_and_what_is_below_directories() {
        let d = dirty(&["apps/web", "pom.xml"]);
        assert!(d.touches("pom.xml"));
        assert!(d.touches("apps/web/package.json"));
        assert!(d.touches("apps/web/deep/go.mod"));
        assert!(!d.touches("apps/web-old/package.json"));
        assert!(!d.touches("apps/package.json"));
        assert!(!d.touches("package.json"));
        let mut all = dirty(&[]);
        all.rescan = true;
        assert!(all.touches("anything"));
    }

    #[test]
    fn refresh_dirty_too_many_paths_becomes_a_rescan() {
        let mut d = dirty(&[]);
        let many: Vec<String> = (0..=MAX_DIRTY_PATHS).map(|i| format!("f{i}")).collect();
        d.merge(dirty(&many.iter().map(String::as_str).collect::<Vec<_>>()));
        assert!(d.paths.is_empty());
        assert!(d.rescan && d.index);
        let mut kept = dirty(&["a"]);
        kept.relist = true;
        kept.index = true;
        kept.covered_by_scan();
        assert!(kept.relist && !kept.index && kept.paths.is_empty());
    }
}
