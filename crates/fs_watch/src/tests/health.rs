//! Rescans (`rescan_*`) and health (`health_*`): the table in
//! docs/design/fs_watch.md「状态与异常」.

use std::fs;
use std::sync::Arc;

use insta::assert_snapshot;
use lectern_kernel::BusyGuard;

use super::support::*;
use crate::testing::RecordingSink;
use crate::{
    BackendError, Coalesce, Config, Failure, Flags, FlushError, Health, Sink, WorktreeBatch,
    WorktreeName, WorktreeSpec,
};

#[test]
fn rescan_on_the_worktree_root() {
    let fx = Fixture::new();
    let o = fx.opened();
    let base = o.worktree.revision().n;
    let calls = fx.source.filter_calls();
    fx.emit_root(&[
        ("a", F),
        ("src", D | Flags::RESCAN | Flags::DROPPED),
        ("b", F),
    ]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @"rev=1 paths=[] meta=[] rescan");
    assert_eq!(fx.source.filter_calls(), calls + 1);
}

#[test]
fn rescan_on_the_common_directory() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, b"a\n");
    let o = fx.opened();
    let base = o.worktree.revision().n;
    let (filters, configs) = (fx.source.filter_calls(), fx.source.config_calls());
    append(&head, b"b\n");
    fx.emit_common(&[("", Flags::RESCAN)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(repo_batches(&o.repo_sink.take()), @r#"
    worktrees=[] ["rescan"]
      seq=0 "HEAD" offset=2 "b" observed=r2 after=r1 Appended
    "#);
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=1 paths=[] meta=["head", "index", "refs", "config", "rules"]"#);
    assert_eq!(fx.source.filter_calls(), filters + 1);
    assert_eq!(fx.source.config_calls(), configs + 1);
}

#[test]
fn rescan_on_queue_overflow() {
    let fx = Fixture::with_config(Config {
        queue_events: 4,
        ..Config::default()
    });
    let o = fx.opened();
    let base = o.worktree.revision().n;
    fx.emit_root(&[("a", F), ("b", F)]);
    // Does not fit next to the first batch: this target overflows.
    fx.emit_root(&[("c", F), ("d", F), ("e", F)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @"rev=2 paths=[] meta=[] rescan");
}

#[test]
fn health_worktree_root_gone() {
    let fx = Fixture::new();
    let o = fx.opened();
    let base = o.worktree.revision().n;
    fx.emit_root(&[("", D | Flags::ROOT_CHANGED)]);
    fx.settle();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @"rev=1 paths=[] meta=[] health=Failed(RootGone)");
    assert!(!fx.backend.is_armed(&fx.root), "the root target is dropped");
    assert!(fx.backend.is_armed(&fx.common));
    assert_eq!(
        o.worktree.flush_revision(),
        Err(FlushError::NotWatching(Failure::RootGone))
    );
    // Later events change nothing.
    fx.emit_common(&[("HEAD", F)]);
    fx.settle();
    fx.wait();
    assert!(o.sink.take().is_empty());
    assert_eq!(o.worktree.revision().n, base + 1);
}

#[test]
fn health_unmounted_volume() {
    let fx = Fixture::new();
    let o = fx.opened();
    fx.emit_root(&[("", Flags::UNMOUNTED)]);
    fx.settle();
    let batches = o.sink.take();
    assert_eq!(
        batches.last().unwrap().0.health,
        Some(Health::Failed(Failure::Unmounted))
    );
}

#[test]
fn health_common_directory_gone_fails_every_state() {
    let fx = Fixture::new();
    let o = fx.opened();
    fx.emit_common(&[("", D | Flags::ROOT_CHANGED)]);
    fx.settle();
    assert_snapshot!(repo_batches(&o.repo_sink.take()), @"worktrees=[] [] health=Failed(RootGone)");
    assert_eq!(
        o.sink.take().last().unwrap().0.health,
        Some(Health::Failed(Failure::RootGone))
    );
    assert!(fx.backend.targets().is_empty());
}

#[test]
fn health_arm_failure_fails_the_state() {
    let fx = Fixture::new();
    fx.backend
        .fail_next_arm(&fx.common, BackendError::PermissionDenied);
    let (repo, repo_sink) = fx.repo();
    let (worktree, sink) = fx.worktree(&repo);
    fx.settle();
    let failed = Some(Health::Failed(Failure::Backend(
        "permission denied".to_owned(),
    )));
    assert_eq!(repo_sink.take()[0].0.health, failed);
    assert_eq!(sink.take()[0].0.health, failed);
    assert!(worktree.flush_revision().is_err());

    // A root that does not exist.
    let fx = Fixture::new();
    fs::remove_dir_all(&fx.root).unwrap();
    let (_repo, repo_sink) = fx.repo();
    fx.settle();
    assert_eq!(
        repo_sink.take()[0].0.health,
        Some(Health::Failed(Failure::RootGone))
    );
}

#[test]
fn health_reflog_unreadable_is_a_reset() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    fs::create_dir_all(fx.base.join("target")).unwrap();
    std::os::unix::fs::symlink(fx.base.join("target"), &head).unwrap();
    let o = fx.opened();
    fx.emit_common(&[("logs/HEAD", Flags::SYMLINK)]);
    fx.settle();
    fx.wait();
    let batches = o.repo_sink.take();
    assert_eq!(batches.len(), 1);
    assert!(matches!(
        batches[0].0.reflog_resets[0].reason,
        crate::ResetReason::Unreadable(_)
    ));
}

/// Panics on every delivery.
struct Panicking;

impl Sink<WorktreeBatch> for Panicking {
    fn deliver(&self, _batch: WorktreeBatch, _busy: Option<BusyGuard>) {
        panic!("consumer bug");
    }
}

#[test]
fn health_panicking_sink_stops_alone() {
    let fx = Fixture::new();
    let o = fx.opened();
    let _bad = o.repo.watch_worktree(
        WorktreeSpec {
            name: WorktreeName::Main,
            root: fx.root.clone(),
            source: fx.source.clone(),
            coalesce: Coalesce::WORKTREE,
        },
        Arc::new(Panicking),
    );
    fx.settle();
    for name in ["a", "b"] {
        fx.emit_root(&[(name, F)]);
        fx.settle();
        fx.wait();
    }
    assert_eq!(o.sink.take().len(), 2, "the other subscription carries on");
    let _ = RecordingSink::<WorktreeBatch>::new(None);
}
