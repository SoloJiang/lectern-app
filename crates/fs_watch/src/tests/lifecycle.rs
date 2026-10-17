//! Lifecycle (`lifecycle_*`): when the thread starts, shared states and
//! targets, dropping handles, shutdown.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

use lectern_executor::{Config as ExecutorConfig, Executor, ThreadKind};

use super::support::*;
use crate::testing::{FakeBackend, RecordingSink};
use crate::{Coalesce, Config, DirBatch, DirSpec, Watcher};

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn lifecycle_new_starts_no_thread_and_calls_no_backend() {
    let dir = tempfile::tempdir().unwrap();
    let dedicated = Arc::new(AtomicUsize::new(0));
    let count = dedicated.clone();
    let executor = Executor::new(ExecutorConfig {
        workers: 1,
        batch_workers: 1,
        on_thread_start: Some(Arc::new(move |kind| {
            if kind == ThreadKind::Dedicated {
                count.fetch_add(1, SeqCst);
            }
        })),
    });
    let backend = FakeBackend::new();
    let watcher = Watcher::new(backend.clone(), executor.clone(), Config::default());
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(dedicated.load(SeqCst), 0);
    assert!(backend.history().is_empty());

    let sink = RecordingSink::<DirBatch>::new(None);
    let _watch = watcher.watch_dir(
        DirSpec {
            root: dir.path().to_path_buf(),
            coalesce: Coalesce::DIR,
        },
        sink.clone(),
    );
    wait_until("the fs_watch thread", || dedicated.load(SeqCst) == 1);
    wait_until("the first batch", || sink.len() == 1);
    assert!(backend.is_armed(dir.path()));
    // A second watch reuses the thread.
    let _again = watcher.watch_dir(
        DirSpec {
            root: dir.path().to_path_buf(),
            coalesce: Coalesce::DIR,
        },
        sink.clone(),
    );
    wait_until("the second first batch", || sink.len() == 2);
    assert_eq!(dedicated.load(SeqCst), 1);
    watcher.shutdown();
    assert!(!backend.is_armed(dir.path()));
    executor.shutdown(Duration::from_secs(5));
}

#[test]
fn lifecycle_states_and_targets_are_shared() {
    let fx = Fixture::new();
    let o = fx.opened();
    let (repo2, repo2_sink) = fx.repo();
    let (worktree2, sink2) = fx.worktree(&repo2);
    fx.settle();
    assert_eq!(
        targets(&fx.backend, &fx.base),
        ["repo tree", "repo/.git tree"]
    );
    assert_eq!(worktree2.revision(), o.worktree.revision());
    // The joining handles get their own first batch right away.
    assert!(repo2_sink.take()[0].0.rescan);
    assert!(sink2.take()[0].0.rescan);
    fx.emit_root(&[("a", F)]);
    fx.settle();
    assert_eq!(worktree2.revision(), o.worktree.revision());
    fx.wait();
    assert_eq!(o.sink.take().len(), 1);
    assert_eq!(sink2.take().len(), 1);
}

#[test]
fn lifecycle_last_handle_dropped_drops_the_targets() {
    let fx = Fixture::new();
    let o = fx.opened();
    let (repo2, _repo2_sink) = fx.repo();
    fx.settle();
    let Opened { repo, worktree, .. } = o;
    drop(worktree);
    fx.settle();
    assert_eq!(targets(&fx.backend, &fx.base), ["repo/.git tree"]);
    drop(repo);
    fx.settle();
    assert_eq!(
        targets(&fx.backend, &fx.base),
        ["repo/.git tree"],
        "repo2 still holds it"
    );
    drop(repo2);
    fx.settle();
    assert!(fx.backend.targets().is_empty());
}

#[test]
fn lifecycle_worktree_outlives_its_repo_handle() {
    let fx = Fixture::new();
    let o = fx.opened();
    let Opened {
        repo,
        worktree,
        sink,
        ..
    } = o;
    drop(repo);
    fx.settle();
    assert_eq!(
        targets(&fx.backend, &fx.base),
        ["repo tree", "repo/.git tree"]
    );
    fx.emit_common(&[("HEAD", F)]);
    fx.settle();
    fx.wait();
    assert!(sink.take()[0].0.meta.head);
    drop(worktree);
    fx.settle();
    assert!(fx.backend.targets().is_empty());
}

#[test]
fn lifecycle_shutdown_stops_delivery() {
    let fx = Fixture::new();
    let o = fx.opened();
    fx.emit_root(&[("a", F)]);
    fx.settle();
    fx.watcher.shutdown();
    assert!(fx.backend.targets().is_empty());
    fx.wait();
    assert!(o.sink.take().is_empty(), "the open batch is not delivered");
    let (repo, repo_sink) = fx.repo();
    let (_worktree, sink) = fx.worktree(&repo);
    fx.settle();
    fx.wait();
    assert!(repo_sink.take().is_empty());
    assert!(sink.take().is_empty());
    assert!(fx.backend.targets().is_empty());
}

#[test]
fn lifecycle_dropping_the_last_watcher_shuts_down() {
    let fx = Fixture::new();
    let o = fx.opened();
    let Fixture {
        watcher, backend, ..
    } = fx;
    drop(watcher);
    assert!(backend.targets().is_empty());
    drop(o);
}
