//! `flush_revision` (`flush_*`).

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lectern_executor::{Config as ExecutorConfig, Executor};
use lectern_kernel::BusyGuard;

use super::support::*;
use crate::testing::{FakeBackend, FakeSource, RecordingSink};
use crate::{
    Coalesce, Config, Failure, FlushError, RepoBatch, RepoSpec, Sink, Watcher, WorktreeBatch,
    WorktreeName, WorktreeSpec, WorktreeWatch,
};

#[test]
fn flush_held_events_count_after_flush_revision() {
    let fx = Fixture::new();
    let o = fx.opened();
    let before = o.worktree.revision();
    fx.backend.hold(&fx.root);
    fx.backend.hold(&fx.common);
    fx.emit_root(&[("a.txt", F)]);
    fx.emit_common(&[("HEAD", F)]);
    fx.settle();
    assert_eq!(o.worktree.revision(), before, "still in the backend");
    let flushed = o.worktree.flush_revision().unwrap();
    assert_eq!(flushed.n, before.n + 2);
    assert_eq!(fx.backend.flush_count(&fx.root), 1);
    assert_eq!(fx.backend.flush_count(&fx.common), 1);
    // Nothing new: the same revision.
    assert_eq!(o.worktree.flush_revision().unwrap(), flushed);
}

#[test]
fn flush_does_not_deliver_coalescing_batches() {
    let fx = Fixture::new();
    let o = fx.opened();
    fx.backend.hold(&fx.root);
    fx.emit_root(&[("a.txt", F)]);
    o.worktree.flush_revision().unwrap();
    assert!(o.sink.is_empty());
    fx.wait();
    assert_eq!(o.sink.take().len(), 1);
}

#[test]
fn flush_flushes_config_list_targets() {
    let fx = Fixture::new();
    let home = fx.base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    fx.source.set_config_files(&[home.join(".gitconfig")]);
    let o = fx.opened();
    let before = o.worktree.revision();
    fx.backend.hold(&home);
    fx.backend.emit(&home, &[(".gitconfig", F)]);
    fx.settle();
    assert_eq!(o.worktree.flush_revision().unwrap().n, before.n + 1);
    assert_eq!(fx.backend.flush_count(&home), 1);
}

/// A sink that calls `flush_revision` from `deliver`.
struct Reentrant {
    watch: Mutex<Option<Arc<WorktreeWatch>>>,
    results: Mutex<Vec<Result<crate::Revision, FlushError>>>,
}

impl Sink<WorktreeBatch> for Reentrant {
    fn deliver(&self, _batch: WorktreeBatch, _busy: Option<BusyGuard>) {
        if let Some(watch) = self.watch.lock().unwrap().clone() {
            self.results.lock().unwrap().push(watch.flush_revision());
        }
    }
}

#[test]
fn flush_in_deliver_returns_would_deadlock() {
    let fx = Fixture::new();
    let (repo, _repo_sink) = fx.repo();
    let sink = Arc::new(Reentrant {
        watch: Mutex::new(None),
        results: Mutex::new(Vec::new()),
    });
    let watch = Arc::new(repo.watch_worktree(
        WorktreeSpec {
            name: WorktreeName::Main,
            root: fx.root.clone(),
            source: fx.source.clone(),
            coalesce: Coalesce::WORKTREE,
        },
        sink.clone(),
    ));
    *sink.watch.lock().unwrap() = Some(watch.clone());
    fx.settle();
    assert_eq!(
        *sink.results.lock().unwrap(),
        vec![Err(FlushError::WouldDeadlock)]
    );
    *sink.watch.lock().unwrap() = None;
}

#[test]
fn flush_after_failure_returns_not_watching() {
    let fx = Fixture::new();
    let o = fx.opened();
    fx.emit_root(&[("", D | crate::Flags::ROOT_CHANGED)]);
    fx.settle();
    assert_eq!(
        o.worktree.flush_revision(),
        Err(FlushError::NotWatching(Failure::RootGone))
    );
}

#[test]
fn flush_waits_for_arm() {
    let fx = Fixture::new();
    let (repo, _repo_sink) = fx.repo();
    let (worktree, _sink) = fx.worktree(&repo);
    // Nothing processed yet: flush_revision arms first, then flushes.
    let revision = worktree.flush_revision().unwrap();
    assert_eq!(revision.n, 0);
    assert_eq!(fx.backend.flush_count(&fx.root), 1);
}

#[test]
fn flush_on_a_stopped_watcher_returns_not_watching() {
    let fx = Fixture::new();
    let o = fx.opened();
    fx.watcher.shutdown();
    assert_eq!(
        o.worktree.flush_revision(),
        Err(FlushError::NotWatching(Failure::Stopped))
    );
    let (repo, _repo_sink) = fx.repo();
    let (late, _sink) = fx.worktree(&repo);
    assert_eq!(
        late.flush_revision(),
        Err(FlushError::NotWatching(Failure::Stopped))
    );
}

/// A sink whose first delivery blocks until released.
struct Blocking {
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Sink<WorktreeBatch> for Blocking {
    fn deliver(&self, _batch: WorktreeBatch, _busy: Option<BusyGuard>) {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
            if let Some(release) = self.release.lock().unwrap().take() {
                let _ = release.recv_timeout(Duration::from_secs(10));
            }
        }
    }
}

#[test]
fn flush_times_out_while_the_fs_watch_thread_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let root = base.join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let backend = FakeBackend::new();
    let executor = Executor::new(ExecutorConfig {
        workers: 2,
        on_thread_start: None,
    });
    let watcher = Watcher::new(
        backend.clone(),
        executor.clone(),
        Config {
            flush_timeout: Duration::from_millis(100),
            ..Config::default()
        },
    );
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let sink = Arc::new(Blocking {
        entered: Mutex::new(Some(entered_tx)),
        release: Mutex::new(Some(release_rx)),
    });
    let repo = watcher.watch_repo(
        RepoSpec {
            common_dir: root.join(".git"),
            coalesce: Coalesce::REPO,
        },
        RecordingSink::<RepoBatch>::new(None),
    );
    let source = FakeSource::new();
    let worktree = repo.watch_worktree(
        WorktreeSpec {
            name: WorktreeName::Main,
            root: root.clone(),
            source,
            coalesce: Coalesce::WORKTREE,
        },
        sink,
    );
    // The first batch blocks the fs_watch thread.
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(worktree.flush_revision(), Err(FlushError::Timeout));
    release.send(()).unwrap();
    // With the thread free again, a flush goes through.
    assert!(worktree.flush_revision().is_ok());
    watcher.shutdown();
    executor.shutdown(Duration::from_secs(5));
}
