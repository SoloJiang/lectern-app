//! The review's fs_watch subscriptions and what their batches ask for
//! (docs/design/review.md「数据流与线程」, fs_watch design「对其他 crate 的
//! 要求」): a worktree batch with changed paths is `RefreshHint::Paths`, a
//! rescan or any git metadata flag is `Full`; a repository batch with reflog
//! lines or lost events is `Full`. The sinks run on the fs_watch thread:
//! they take the state lock to ask for a round and do no IO.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use lectern_fs_watch::{
    Coalesce, Health, RepoBatch, RepoSpec, RepoWatch, Sink, WorktreeBatch, WorktreeSpec,
    WorktreeWatch,
};
use lectern_kernel::BusyGuard;
use lectern_logging::debug;
use lectern_services::changes::{ChangePath, RefreshHint};
use lectern_services::review::ReviewId;

use super::Engine;
use super::round::{Git, Kind};

/// A review's two subscriptions. Dropping it unsubscribes.
pub(crate) struct Watching {
    _repo: RepoWatch,
    _tree: WorktreeWatch,
}

/// Subscribes to `git`'s repository and worktree for review `id`. Never
/// blocks: fs_watch queues the arming for its own thread.
pub(super) fn start(
    watcher: &lectern_fs_watch::Watcher,
    engine: &Weak<Engine>,
    id: ReviewId,
    git: &Git,
) -> Watching {
    let repo = watcher.watch_repo(
        RepoSpec {
            common_dir: git.common_dir().to_path_buf(),
            coalesce: Coalesce::REPO,
        },
        Arc::new(ReviewSink {
            engine: engine.clone(),
            id,
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
        Arc::new(ReviewSink {
            engine: engine.clone(),
            id,
            first: AtomicBool::new(false),
        }),
    );
    Watching {
        _repo: repo,
        _tree: tree,
    }
}

/// The sink of both subscriptions of one review.
pub(crate) struct ReviewSink {
    engine: Weak<Engine>,
    id: ReviewId,
    /// The repository's first batch only says the watch is armed, which the
    /// worktree's first batch (a rescan) says as well: one refresh, not two.
    first: AtomicBool,
}

impl ReviewSink {
    fn busy(&self) -> Option<BusyGuard> {
        self.engine.upgrade()?.cfg.host.as_ref()?.busy()
    }

    fn ask(&self, kind: Kind) {
        if let Some(engine) = self.engine.upgrade() {
            engine.request(self.id, kind);
        }
    }
}

impl Sink<WorktreeBatch> for ReviewSink {
    fn deliver(&self, batch: WorktreeBatch, busy: Option<BusyGuard>) {
        if let Some(hint) = worktree_hint(&batch) {
            self.ask(Kind::Refresh(hint));
        }
        // The round this started holds its own guard from its submission
        // (`Engine::submit`); one merged into a running round is covered by
        // that round, which submits the next before it ends.
        drop(busy);
    }

    fn busy(&self) -> Option<BusyGuard> {
        ReviewSink::busy(self)
    }
}

impl Sink<RepoBatch> for ReviewSink {
    fn deliver(&self, batch: RepoBatch, busy: Option<BusyGuard>) {
        let first = self.first.swap(false, Ordering::AcqRel);
        if !first && repo_changed(&batch) {
            self.ask(Kind::Refresh(RefreshHint::Full));
        }
        drop(busy);
    }

    fn busy(&self) -> Option<BusyGuard> {
        ReviewSink::busy(self)
    }
}

/// What a worktree batch asks of the review's change set; None when it
/// changes nothing (a health report alone).
pub(super) fn worktree_hint(batch: &WorktreeBatch) -> Option<RefreshHint> {
    let m = &batch.meta;
    if batch.rescan
        || m.head
        || m.index
        || m.refs
        || m.config
        || m.rules
        || matches!(batch.health, Some(Health::Failed(_)))
    {
        return Some(RefreshHint::Full);
    }
    if batch.paths.is_empty() {
        return None;
    }
    Some(RefreshHint::Paths(
        batch
            .paths
            .iter()
            .map(|p| ChangePath(Arc::from(p.as_bytes())))
            .collect(),
    ))
}

/// Whether a repository batch can change the cutoffs or the base: new reflog
/// lines (a commit, a branch switch), a rewritten reflog, lost events.
pub(super) fn repo_changed(batch: &RepoBatch) -> bool {
    !batch.reflog.is_empty()
        || !batch.reflog_resets.is_empty()
        || batch.rescan
        || matches!(batch.health, Some(Health::Failed(_)))
}

/// The one round that serves both requests: `Full` over the rest, the path
/// union for two path sets, a path set over a plain reuse, `Fetch` (the
/// whole preparation) over all.
pub(crate) fn merge_hints(held: Option<Kind>, new: Kind) -> Kind {
    use RefreshHint::{Full, Paths};
    match (held, new) {
        (None, new) => new,
        (Some(Kind::Fetch), _) | (_, Kind::Fetch) => Kind::Fetch,
        (Some(Kind::Refresh(Full)), _) | (_, Kind::Refresh(Full)) => Kind::Refresh(Full),
        (Some(Kind::Refresh(Paths(a))), Kind::Refresh(Paths(b))) => {
            let mut all: Vec<ChangePath> = a.iter().chain(b.iter()).cloned().collect();
            all.sort();
            all.dedup();
            Kind::Refresh(Paths(all.into()))
        }
        (Some(Kind::Refresh(paths)), Kind::Reuse) | (Some(Kind::Reuse), Kind::Refresh(paths)) => {
            Kind::Refresh(paths)
        }
        (Some(Kind::Reuse), Kind::Reuse) => Kind::Reuse,
    }
}

impl Engine {
    /// Asks for a round of `kind` on review `id` (a watch batch). Merged
    /// into the running round's successor when one is running.
    pub(crate) fn request(&self, id: ReviewId, kind: Kind) {
        let how = match &kind {
            Kind::Refresh(RefreshHint::Full) => "full",
            Kind::Refresh(_) => "paths",
            Kind::Reuse => "sessions",
            Kind::Fetch => "fetch",
        };
        let n = match &kind {
            Kind::Refresh(RefreshHint::Paths(p)) => p.len(),
            _ => 0,
        };
        let mut state = super::lock(&self.state);
        if let Some(open) = state.reviews.get(&id) {
            // The sinks run on fs_watch's thread, outside any scope.
            let _scope = lectern_logging::scope(open.ctx);
            debug!("refresh requested"; id = id.0, how = how, paths = n);
            self.start_round(
                &mut state,
                id,
                kind,
                lectern_executor::Priority::Interactive,
            );
        }
    }

    /// The session sources changed: every review is reused with the turns
    /// as they are now (`Kind::Reuse` keeps the change set).
    pub(crate) fn sessions_changed(&self, event: &lectern_services::session::SessionsChanged) {
        if event.changed.is_empty() && event.removed.is_empty() {
            return;
        }
        {
            let mut state = super::lock(&self.state);
            let ids: Vec<ReviewId> = state.reviews.keys().copied().collect();
            for id in ids {
                self.start_round(
                    &mut state,
                    id,
                    Kind::Reuse,
                    lectern_executor::Priority::Interactive,
                );
            }
        }
        // The overviews' claims and sessions change too.
        self.overviews_request_all(super::overview::Trigger::default());
        // So are the sessions' worktrees' uncommitted counts.
        self.worktrees_sessions_changed(event);
    }

    /// Subscribes the review to its repository and worktree, once.
    pub(super) fn ensure_watch(&self, id: ReviewId, git: &Git) {
        let Some(watcher) = &self.cfg.watcher else {
            return;
        };
        let wanted = super::lock(&self.state)
            .reviews
            .get(&id)
            .is_some_and(|o| o.watch.is_none());
        if !wanted {
            return;
        }
        let watching = start(watcher, &self.me, id, git);
        let spare = {
            let mut state = super::lock(&self.state);
            match state.reviews.get_mut(&id) {
                Some(open) if open.watch.is_none() => {
                    open.watch = Some(watching);
                    None
                }
                _ => Some(watching),
            }
        };
        drop(spare);
    }
}

#[cfg(test)]
mod tests {
    use lectern_fs_watch::{MetaChanges, Revision, WatchPath};

    use super::*;

    fn paths(names: &[&str]) -> Kind {
        Kind::Refresh(RefreshHint::Paths(
            names
                .iter()
                .map(|n| ChangePath(Arc::from(n.as_bytes())))
                .collect(),
        ))
    }

    fn batch(names: &[&str]) -> WorktreeBatch {
        WorktreeBatch {
            revision: Revision { epoch: 1, n: 1 },
            paths: names
                .iter()
                .map(|n| WatchPath::new(n.as_bytes()).unwrap())
                .collect(),
            meta: MetaChanges::default(),
            rescan: false,
            health: None,
        }
    }

    fn describe(kind: &Kind) -> String {
        match kind {
            Kind::Fetch => "fetch".into(),
            Kind::Reuse => "reuse".into(),
            Kind::Refresh(RefreshHint::Full) => "full".into(),
            Kind::Refresh(RefreshHint::Paths(p)) => format!(
                "paths {:?}",
                p.iter()
                    .map(|p| String::from_utf8_lossy(&p.0).into_owned())
                    .collect::<Vec<_>>()
            ),
        }
    }

    #[test]
    fn plugin_merge_joins_paths_and_lets_the_bigger_request_win() {
        let merged = |a: Option<Kind>, b: Kind| describe(&merge_hints(a, b));
        assert_eq!(
            merged(Some(paths(&["b", "a"])), paths(&["c", "a"])),
            r#"paths ["a", "b", "c"]"#
        );
        assert_eq!(merged(None, paths(&["a"])), r#"paths ["a"]"#);
        assert_eq!(merged(Some(paths(&["a"])), Kind::Reuse), r#"paths ["a"]"#);
        assert_eq!(merged(Some(Kind::Reuse), paths(&["a"])), r#"paths ["a"]"#);
        assert_eq!(merged(Some(Kind::Reuse), Kind::Reuse), "reuse");
        assert_eq!(
            merged(Some(paths(&["a"])), Kind::Refresh(RefreshHint::Full)),
            "full"
        );
        assert_eq!(
            merged(Some(Kind::Refresh(RefreshHint::Full)), paths(&["a"])),
            "full"
        );
        assert_eq!(
            merged(Some(Kind::Refresh(RefreshHint::Full)), Kind::Fetch),
            "fetch"
        );
        assert_eq!(merged(Some(Kind::Fetch), paths(&["a"])), "fetch");
    }

    #[test]
    fn plugin_worktree_batches_become_hints() {
        let hint = |b: &WorktreeBatch| worktree_hint(b).map(|h| describe(&Kind::Refresh(h)));
        assert_eq!(
            hint(&batch(&["src", "a.txt"])),
            Some(r#"paths ["src", "a.txt"]"#.to_owned())
        );
        assert_eq!(hint(&batch(&[])), None, "a health report alone");
        let mut b = batch(&["a.txt"]);
        b.rescan = true;
        assert_eq!(hint(&b), Some("full".to_owned()));
        for set in [
            |m: &mut MetaChanges| m.head = true,
            |m: &mut MetaChanges| m.index = true,
            |m: &mut MetaChanges| m.refs = true,
            |m: &mut MetaChanges| m.config = true,
            |m: &mut MetaChanges| m.rules = true,
        ] {
            let mut b = batch(&["a.txt"]);
            set(&mut b.meta);
            assert_eq!(hint(&b), Some("full".to_owned()), "{:?}", b.meta);
        }
        let mut b = batch(&[]);
        b.health = Some(Health::Failed(lectern_fs_watch::Failure::RootGone));
        assert_eq!(hint(&b), Some("full".to_owned()));
        b.health = Some(Health::Live);
        assert_eq!(hint(&b), None);
    }
}
