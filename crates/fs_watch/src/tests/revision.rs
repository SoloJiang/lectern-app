//! Revisions (`revision_*`), docs/design/fs_watch.md「修订号」 and the table
//! in「git 元数据」.

use std::fmt::Write as _;
use std::fs;
use std::sync::Arc;

use insta::assert_snapshot;

use super::support::*;
use crate::testing::RecordingSink;
use crate::{Coalesce, Config, Flags, Health, WorktreeName, WorktreeSpec};

#[test]
fn revision_git_metadata_table() {
    let fx = Fixture::new();
    let o = fx.opened();
    let linked_root = fx.base.join("wt");
    fs::create_dir_all(&linked_root).unwrap();
    let linked_sink = RecordingSink::new(None);
    let linked = o.repo.watch_worktree(
        WorktreeSpec {
            name: WorktreeName::Linked("wt".into()),
            root: linked_root,
            source: fx.source.clone(),
            coalesce: Coalesce::WORKTREE,
        },
        linked_sink.clone(),
    );
    fx.settle();
    linked_sink.take();

    let rows = [
        "HEAD",
        "index",
        "config.worktree",
        "worktrees/wt/HEAD",
        "worktrees/wt/index",
        "worktrees/wt/config.worktree",
        "worktrees",
        "worktrees/wt",
        "worktrees/wt/gitdir",
        "worktrees/wt/locked",
        "worktrees/wt/commondir",
        "refs/heads/main",
        "packed-refs",
        "reftable/tables.list",
        "config",
        "info/exclude",
        "info/attributes",
        "logs/HEAD",
        "index.lock",
        "refs/heads/main.lock",
        "objects/ab/cdef",
        "FETCH_HEAD",
        "ORIG_HEAD",
        "hooks/pre-commit",
    ];
    let mut table = String::new();
    for row in rows {
        let (main, other) = (o.worktree.revision(), linked.revision());
        let (filters, configs) = (fx.source.filter_calls(), fx.source.config_calls());
        fx.emit_common(&[(row, F)]);
        fx.settle();
        fx.wait();
        let repo = o.repo_sink.take();
        let main_batches = o.sink.take();
        let linked_batches = linked_sink.take();
        let meta = |batches: &[(crate::WorktreeBatch, bool)]| {
            batches
                .iter()
                .map(|(batch, _)| format!("{:?}", batch.meta))
                .collect::<Vec<_>>()
                .join(",")
                .replace("MetaChanges ", "")
        };
        let _ = writeln!(
            table,
            "{row}: main+{} wt+{} filter+{} config+{} | repo {}| main {} | wt {}",
            o.worktree.revision().n - main.n,
            linked.revision().n - other.n,
            fx.source.filter_calls() - filters,
            fx.source.config_calls() - configs,
            repo_batches(&repo).replace('\n', " "),
            meta(&main_batches),
            meta(&linked_batches),
        );
    }
    assert_snapshot!(table);
}

#[test]
fn revision_counts_each_raw_batch_once() {
    let fx = Fixture::new();
    let o = fx.opened();
    let before = o.worktree.revision();
    fx.emit_root(&[("a", F), ("b", F), ("c/d", F)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 1);
    fx.emit_common(&[("HEAD", F), ("index", F), ("refs/heads/x", F)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 2);
    fx.emit_root(&[("e", F)]);
    fx.emit_root(&[("f", F)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 4, "two raw batches");
}

#[test]
fn revision_ignored_lock_and_object_batches_do_not_count() {
    let fx = Fixture::new();
    fx.source.ignore(&["target"]);
    let o = fx.opened();
    let before = o.worktree.revision();
    fx.emit_root(&[("target/debug/x", F), ("target", D)]);
    fx.emit_common(&[
        ("index.lock", F),
        ("HEAD.lock", F),
        ("refs/heads/a.lock", F),
    ]);
    fx.emit_common(&[("objects/pack/pack-1.pack", F), ("objects/ab", D)]);
    fx.settle();
    fx.wait();
    assert_eq!(o.worktree.revision(), before);
    assert!(o.sink.take().is_empty());
    assert!(o.repo_sink.take().is_empty());
}

#[test]
fn revision_other_worktrees_head_does_not_count_and_refs_count_for_all() {
    let fx = Fixture::new();
    let o = fx.opened();
    let before = o.worktree.revision();
    fx.emit_common(&[("worktrees/other/HEAD", F), ("worktrees/other/index", F)]);
    fx.settle();
    assert_eq!(o.worktree.revision(), before);
    fx.emit_common(&[("refs/heads/feature", F)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 1);
}

#[test]
fn revision_root_gone_counts() {
    let fx = Fixture::new();
    let o = fx.opened();
    let before = o.worktree.revision();
    fx.emit_root(&[("", D | Flags::ROOT_CHANGED)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 1);
}

#[test]
fn revision_rescan_and_overflow_count() {
    let fx = Fixture::with_config(Config {
        queue_events: 3,
        ..Config::default()
    });
    let o = fx.opened();
    let before = o.worktree.revision();
    fx.emit_root(&[("x", Flags::DIR | Flags::RESCAN)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 1);
    // Four events do not fit in a queue of three: the target overflows.
    fx.emit_root(&[("a", F), ("b", F), ("c", F), ("d", F)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 2);
    fx.emit_common(&[("", Flags::RESCAN | Flags::DROPPED)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 3);
}

#[test]
fn revision_failed_arm_counts() {
    let fx = Fixture::new();
    fx.backend
        .fail_next_arm(&fx.root, crate::BackendError::Other("stream".to_owned()));
    let (repo, _repo_sink) = fx.repo();
    let (worktree, sink) = fx.worktree(&repo);
    fx.settle();
    assert_eq!(worktree.revision().n, 1);
    let batches = sink.take();
    assert_eq!(
        batches[0].0.health,
        Some(Health::Failed(crate::Failure::Backend("stream".to_owned())))
    );
}

#[test]
fn revision_epoch_changes_when_watched_again() {
    let fx = Fixture::new();
    let o = fx.opened();
    let first = o.worktree.revision();
    // A second handle shares the state and its revision.
    let (again, _sink) = fx.worktree(&o.repo);
    assert_eq!(again.revision(), first);
    drop(again);
    let Opened { repo, worktree, .. } = o;
    drop(worktree);
    fx.settle();
    let (rewatched, _sink) = fx.worktree(&repo);
    fx.settle();
    assert_ne!(rewatched.revision().epoch, first.epoch);
    assert_ne!(rewatched.revision(), first);
}

#[test]
fn revision_config_list_file_and_its_link_target_count() {
    let fx = Fixture::new();
    let home = fx.base.join("home");
    let dotfiles = fx.base.join("dotfiles");
    write(&dotfiles.join("gitconfig"), b"[user]\n");
    fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(dotfiles.join("gitconfig"), home.join(".gitconfig")).unwrap();
    fx.source.set_config_files(&[home.join(".gitconfig")]);
    let o = fx.opened();
    let before = o.worktree.revision();
    fx.backend
        .emit(&home, &[(".gitconfig", Flags::SYMLINK | Flags::MODIFIED)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 1);
    fx.backend
        .emit(&dotfiles, &[("gitconfig", F | Flags::MODIFIED)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 2);
    // Another entry in the same directory is not on the list.
    fx.backend.emit(&home, &[(".bashrc", F)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 2);
    fx.wait();
    let batches = o.sink.take();
    assert!(batches.iter().all(|(batch, _)| batch.meta.config));
}

#[test]
fn revision_counts_before_the_window_ends() {
    let fx = Fixture::new();
    let o = fx.opened();
    let before = o.worktree.revision();
    fx.emit_root(&[("a", F)]);
    fx.settle();
    assert!(o.sink.is_empty(), "still coalescing");
    assert_eq!(o.worktree.revision().n, before.n + 1);
    let _ = Arc::clone(&o.sink);
}
