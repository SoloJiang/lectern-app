//! This crate's own fs_watch subscription to the current worktree
//! (「依赖 git、fs_watch 与 cache 的接口」): it shares `project`'s worktree
//! state. `deliver` only merges the batch into the project's dirty set,
//! bumps generations and requests a reconcile, which takes the batch's
//! guard: the kernel is busy until the overlay is up to date.

use std::path::Path;
use std::sync::{Arc, Weak};

use lectern_executor::Priority;
use lectern_fs_watch::{
    Coalesce, Health, RepoBatch, RepoSpec, Sink, WorktreeBatch, WorktreeName, WorktreeSpec,
};
use lectern_kernel::BusyGuard;
use lectern_services::project::RelPath;

use super::state::{Project, Watches, under};
use super::{Inner, lock};
use crate::git_access::Tree;
use crate::map::find_path;

/// The fs_watch name of the worktree whose git directory is `git_dir`.
pub(crate) fn worktree_name(common_dir: &Path, git_dir: &Path) -> Option<WorktreeName> {
    if git_dir == common_dir {
        return Some(WorktreeName::Main);
    }
    let name = git_dir.file_name()?.to_str()?;
    Some(WorktreeName::Linked(name.into()))
}

struct WorktreeSink {
    inner: Weak<Inner>,
    project: Weak<Project>,
    session: u64,
}

impl Sink<WorktreeBatch> for WorktreeSink {
    fn deliver(&self, batch: WorktreeBatch, busy: Option<BusyGuard>) {
        if let (Some(inner), Some(project)) = (self.inner.upgrade(), self.project.upgrade()) {
            inner.worktree_batch(&project, self.session, batch, busy);
        }
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.inner.upgrade()?.host.busy()
    }
}

struct RepoSink {
    inner: Weak<Inner>,
    project: Weak<Project>,
    session: u64,
    name: WorktreeName,
}

impl Sink<RepoBatch> for RepoSink {
    fn deliver(&self, batch: RepoBatch, busy: Option<BusyGuard>) {
        if let (Some(inner), Some(project)) = (self.inner.upgrade(), self.project.upgrade()) {
            inner.repo_batch(&project, self.session, &self.name, batch, busy);
        }
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.inner.upgrade()?.host.busy()
    }
}

impl Inner {
    /// Subscribes to the repository and the current worktree. No IO.
    pub(crate) fn watch(
        self: &Arc<Self>,
        project: &Arc<Project>,
        session: u64,
        tree: &Tree,
    ) -> Option<Watches> {
        let name = worktree_name(&tree.common_dir, &tree.git_dir)?;
        let repo = self.watcher.watch_repo(
            RepoSpec {
                common_dir: tree.common_dir.to_path_buf(),
                coalesce: Coalesce::REPO,
            },
            Arc::new(RepoSink {
                inner: Arc::downgrade(self),
                project: Arc::downgrade(project),
                session,
                name: name.clone(),
            }),
        );
        let worktree = repo.watch_worktree(
            WorktreeSpec {
                name,
                root: tree.root.to_path_buf(),
                source: self.git.watch_source(tree),
                coalesce: Coalesce::WORKTREE,
            },
            Arc::new(WorktreeSink {
                inner: Arc::downgrade(self),
                project: Arc::downgrade(project),
                session,
            }),
        );
        Some(Watches {
            _repo: repo,
            _worktree: worktree,
        })
    }

    /// 「增量更新」's table, for one worktree batch.
    fn worktree_batch(
        self: &Arc<Self>,
        project: &Arc<Project>,
        session: u64,
        batch: WorktreeBatch,
        busy: Option<BusyGuard>,
    ) {
        {
            let mut st = lock(&project.state);
            if st.session != session {
                return;
            }
            if let Some(Health::Failed(_)) = batch.health {
                // Dropped; the next ProjectEvent subscribes again.
                self.unwatch(st.watches.take());
                return;
            }
            if batch.rescan || matches!(batch.health, Some(Health::Degraded(_))) {
                st.gens.compare += 1;
                st.gens.rules += 1;
                let all: Vec<RelPath> = st
                    .map
                    .overlay
                    .entries()
                    .iter()
                    .map(|(p, _)| p.clone())
                    .collect();
                st.dirty.extend(all);
            }
            if batch.meta.head || batch.meta.refs {
                st.check_head = true;
            }
            if batch.meta.index {
                st.check_index = true;
            }
            if batch.meta.rules || batch.meta.config {
                st.gens.rules += 1;
            }
            for path in &batch.paths {
                let Ok(path) = std::str::from_utf8(path.as_bytes()) else {
                    continue;
                };
                let files = st.map.base.files();
                let known = find_path(files, path).is_some() || st.map.overlay.get(path).is_some();
                let dir = !known
                    && (!files.range_under(path).is_empty()
                        || st.map.overlay.entries().iter().any(|(p, _)| under(p, path)));
                if dir {
                    st.dirty_dirs.insert(path.into());
                    st.gens.compare += 1;
                } else {
                    // A file, or a new directory: the read tells, and a
                    // directory requests a comparison.
                    st.dirty.insert(path.into());
                }
            }
            st.held.extend(busy);
        }
        self.request_reconcile(project);
    }

    fn repo_batch(
        self: &Arc<Self>,
        project: &Arc<Project>,
        session: u64,
        name: &WorktreeName,
        batch: RepoBatch,
        busy: Option<BusyGuard>,
    ) {
        {
            let mut st = lock(&project.state);
            if st.session != session {
                return;
            }
            if let Some(Health::Failed(_)) = batch.health {
                self.unwatch(st.watches.take());
                return;
            }
            if batch.refs || batch.rescan {
                st.check_head = true;
            }
            if batch.rescan {
                st.check_index = true;
            }
            if batch.config {
                st.gens.rules += 1;
            }
            let mut others = batch.worktree_list || batch.rescan;
            for (n, meta) in &batch.worktrees {
                if n == name {
                    st.check_head |= meta.head;
                    st.check_index |= meta.index;
                } else if meta.index {
                    others = true;
                }
            }
            if others {
                st.batch.others = Default::default();
            }
            st.held.extend(busy);
        }
        self.request_reconcile(project);
    }

    /// Drops a subscription off the fs_watch thread.
    fn unwatch(&self, watches: Option<Watches>) {
        if let Some(watches) = watches {
            self.host
                .spawn_busy("unwatch", Priority::Background, move || drop(watches));
        }
    }
}
