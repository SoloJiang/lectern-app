//! The worktrees overview's fs_watch subscription: one `RepoWatch` of the
//! project's repository. Its batches only mark counts stale (a worktree's
//! HEAD or index, a reference, a rescan). Only the repository is watched:
//! the files of the worktrees other than the current one are not, which is
//! why a count older than five minutes is made again when a watch starts.
//! The sink runs on the fs_watch thread, takes only the `worktrees` lock
//! and does no IO.

use std::sync::atomic::AtomicBool;

use lectern_fs_watch::Health;

use super::*;

/// Subscribes to `common_dir`'s repository. Never blocks: fs_watch arms on
/// its own thread.
pub(super) fn start(
    watcher: &lectern_fs_watch::Watcher,
    engine: &Weak<Engine>,
    project: &ProjectKey,
    common_dir: &std::path::Path,
) -> RepoWatch {
    watcher.watch_repo(
        RepoSpec {
            common_dir: common_dir.to_path_buf(),
            coalesce: Coalesce::REPO,
        },
        Arc::new(WorktreesSink {
            engine: engine.clone(),
            project: project.clone(),
            first: AtomicBool::new(true),
        }),
    )
}

struct WorktreesSink {
    engine: Weak<Engine>,
    project: ProjectKey,
    /// The first batch only says the watch is armed; the round that
    /// subscribed compared after it asked.
    first: AtomicBool,
}

impl Sink<RepoBatch> for WorktreesSink {
    fn deliver(&self, batch: RepoBatch, busy: Option<BusyGuard>) {
        let first = self.first.swap(false, Ordering::AcqRel);
        if !first && let Some(engine) = self.engine.upgrade() {
            mark_batch(&engine, &self.project, &batch);
        }
        drop(busy);
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.engine.upgrade()?.cfg.host.as_ref()?.busy()
    }
}

/// What a repository batch makes stale (docs/design/review.md「概览」:
/// 失效).
pub(super) fn mark_batch(engine: &Engine, project: &ProjectKey, batch: &RepoBatch) {
    let lost = batch.rescan || matches!(batch.health, Some(Health::Failed(_)));
    let moved = batch.refs || batch.config || !batch.reflog.is_empty();
    if !lost && !moved && batch.worktrees.is_empty() && !batch.worktree_list {
        return;
    }
    engine.worktrees_mark(project, |marks| {
        if lost {
            marks.all = true;
            marks.refs = true;
        }
        if moved {
            marks.refs = true;
        }
        for (name, meta) in &batch.worktrees {
            // A new HEAD changes both counts; an index alone (an agent
            // staging, a commit's index refresh) only the uncommitted one.
            let need = if meta.head {
                Need::BOTH
            } else if meta.index {
                Need::UNCOMMITTED
            } else {
                continue;
            };
            marks.names.entry(name.clone()).or_default().merge(need);
        }
    });
}
