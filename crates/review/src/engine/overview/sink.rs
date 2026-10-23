//! The overview's fs_watch subscriptions: the current worktree's batches
//! become path or full triggers, the repository's reflog lines a full
//! one. The sinks run on the fs_watch thread, take only the `overviews`
//! lock and do no IO (as a review's, `engine::watch`).

use super::*;

/// Subscribes the overview of `project` to `git`'s repository and
/// worktree. Never blocks: fs_watch arms on its own thread.
pub(super) fn start(
    watcher: &lectern_fs_watch::Watcher,
    engine: &Weak<Engine>,
    project: &ProjectKey,
    git: &Git,
) -> Watching {
    let repo = watcher.watch_repo(
        RepoSpec {
            common_dir: git.common_dir().to_path_buf(),
            coalesce: Coalesce::REPO,
        },
        Arc::new(OverviewSink {
            engine: engine.clone(),
            project: project.clone(),
            first: AtomicBool::new(true),
        }),
    );
    let tree = repo.watch_worktree(
        WorktreeSpec {
            name: git.worktree.id().clone().into(),
            root: git.root.clone(),
            source: git.worktree.watch_source(),
            coalesce: Coalesce::WORKTREE,
        },
        Arc::new(OverviewSink {
            engine: engine.clone(),
            project: project.clone(),
            // The worktree's first batch is a rescan: a full round, which
            // the round that armed it already was.
            first: AtomicBool::new(true),
        }),
    );
    Watching {
        _repo: repo,
        _tree: tree,
    }
}

struct OverviewSink {
    engine: Weak<Engine>,
    project: ProjectKey,
    first: AtomicBool,
}

impl OverviewSink {
    fn busy(&self) -> Option<BusyGuard> {
        self.engine.upgrade()?.cfg.host.as_ref()?.busy()
    }

    fn ask(&self, trigger: Trigger) {
        if let Some(engine) = self.engine.upgrade() {
            engine.overview_request(&self.project, trigger);
        }
    }
}

impl Sink<WorktreeBatch> for OverviewSink {
    fn deliver(&self, batch: WorktreeBatch, busy: Option<BusyGuard>) {
        let first = self.first.swap(false, Ordering::AcqRel);
        match worktree_hint(&batch) {
            // The arming's rescan: the round that subscribed compared
            // everything already.
            Some(RefreshHint::Full) if first && batch.rescan => {}
            Some(RefreshHint::Full) => self.ask(Trigger::full()),
            Some(RefreshHint::Paths(paths)) => self.ask(Trigger {
                paths: paths.to_vec(),
                ..Trigger::default()
            }),
            None => {}
        }
        drop(busy);
    }

    fn busy(&self) -> Option<BusyGuard> {
        OverviewSink::busy(self)
    }
}

impl Sink<RepoBatch> for OverviewSink {
    fn deliver(&self, batch: RepoBatch, busy: Option<BusyGuard>) {
        let first = self.first.swap(false, Ordering::AcqRel);
        if !first && repo_changed(&batch) {
            self.ask(Trigger::full());
        }
        drop(busy);
    }

    fn busy(&self) -> Option<BusyGuard> {
        OverviewSink::busy(self)
    }
}
