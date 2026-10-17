//! The config list (`config_*`), docs/design/fs_watch.md「配置文件清单」.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use insta::assert_snapshot;

use super::support::*;
use crate::{BackendError, Degraded, Flags, Health};

#[test]
fn config_points_go_to_the_common_root_and_entries_targets() {
    let fx = Fixture::new();
    let home = fx.base.join("home");
    fs::create_dir_all(home.join(".config/git")).unwrap();
    write(&home.join(".gitconfig"), b"");
    write(&home.join(".config/git/config"), b"");
    fx.source.set_config_files(&[
        fx.common.join("config"),
        fx.common.join("info/exclude"),
        fx.root.join(".lectern/gitconfig"),
        home.join(".gitconfig"),
        home.join(".config/git/config"),
        home.join(".config/git/ignore"),
        home.join(".config/git/attributes"),
    ]);
    let _o = fx.opened();
    assert_snapshot!(targets(&fx.backend, &fx.base).join("\n"), @r#"
    home entries [".gitconfig"]
    home/.config/git entries ["attributes", "config", "ignore"]
    repo tree
    repo/.git tree
    "#);
}

#[test]
fn config_entries_directory_is_armed_once_for_two_repositories() {
    let fx = Fixture::new();
    let home = fx.base.join("home");
    fs::create_dir_all(&home).unwrap();
    fx.source.set_config_files(&[home.join(".gitconfig")]);
    let _o = fx.opened();
    // A second repository with the same global config.
    let other_root = fx.base.join("other");
    fs::create_dir_all(other_root.join(".git")).unwrap();
    let repo_sink = crate::testing::RecordingSink::new(None);
    let other = fx.watcher.watch_repo(
        crate::RepoSpec {
            common_dir: other_root.join(".git"),
            coalesce: crate::Coalesce::REPO,
        },
        repo_sink,
    );
    let other_source = crate::testing::FakeSource::new();
    other_source.set_config_files(&[home.join(".gitconfig"), home.join(".config/git/config")]);
    let sink = crate::testing::RecordingSink::new(None);
    let _other_worktree = other.watch_worktree(
        crate::WorktreeSpec {
            name: crate::WorktreeName::Main,
            root: other_root,
            source: other_source,
            coalesce: crate::Coalesce::WORKTREE,
        },
        sink,
    );
    fx.settle();
    assert_snapshot!(targets(&fx.backend, &fx.base).join("\n"), @r#"
    home entries [".config", ".gitconfig"]
    other tree
    other/.git tree
    repo tree
    repo/.git tree
    "#);
}

#[test]
fn config_missing_file_watches_the_nearest_ancestor_and_moves_down() {
    let fx = Fixture::new();
    let xdg = fx.base.join("xdg");
    fx.source.set_config_files(&[xdg.join("git/config")]);
    let o = fx.opened();
    // The temporary directory is the nearest existing ancestor.
    assert_eq!(
        targets(&fx.backend, &fx.base)[0],
        r#" entries ["xdg"]"#,
        "{:?}",
        targets(&fx.backend, &fx.base)
    );
    let before = o.worktree.revision();
    fs::create_dir_all(xdg.join("git")).unwrap();
    fx.backend.emit(&fx.base, &[("xdg", D | Flags::CREATED)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 1);
    assert!(
        targets(&fx.backend, &fx.base).contains(&r#"xdg/git entries ["config"]"#.to_owned()),
        "{:?}",
        targets(&fx.backend, &fx.base)
    );
    write(&xdg.join("git/config"), b"[core]\n");
    fx.backend
        .emit(&xdg.join("git"), &[("config", F | Flags::CREATED)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 2);
}

#[test]
fn config_symlink_and_its_target_are_both_watched() {
    let fx = Fixture::new();
    let home = fx.base.join("home");
    let dotfiles = fx.base.join("dotfiles");
    write(&dotfiles.join("gitconfig"), b"");
    fs::create_dir_all(&home).unwrap();
    symlink(dotfiles.join("gitconfig"), home.join(".gitconfig")).unwrap();
    fx.source.set_config_files(&[home.join(".gitconfig")]);
    let _o = fx.opened();
    assert_snapshot!(targets(&fx.backend, &fx.base).join("\n"), @r#"
    dotfiles entries ["gitconfig"]
    home entries [".gitconfig"]
    repo tree
    repo/.git tree
    "#);
}

#[test]
fn config_linked_parent_directory_is_followed() {
    let fx = Fixture::new();
    let home = fx.base.join("home");
    let dotfiles = fx.base.join("dotfiles");
    fs::create_dir_all(dotfiles.join("config")).unwrap();
    fs::create_dir_all(&home).unwrap();
    // `~/.config -> ../dotfiles/config`; `git/config` does not exist yet.
    symlink("../dotfiles/config", home.join(".config")).unwrap();
    fx.source
        .set_config_files(&[home.join(".config/git/config")]);
    let o = fx.opened();
    assert_snapshot!(targets(&fx.backend, &fx.base).join("\n"), @r#"
    dotfiles/config entries ["git"]
    home entries [".config"]
    repo tree
    repo/.git tree
    "#);
    let before = o.worktree.revision();

    // A new file behind the link.
    write(&dotfiles.join("config/git/config"), b"[user]\n");
    fx.backend
        .emit(&dotfiles.join("config"), &[("git", D | Flags::CREATED)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 1);
    assert_snapshot!(targets(&fx.backend, &fx.base).join("\n"), @r#"
    dotfiles/config/git entries ["config"]
    home entries [".config"]
    repo tree
    repo/.git tree
    "#);

    // The link is replaced.
    let other = fx.base.join("other-dotfiles");
    fs::create_dir_all(&other).unwrap();
    fs::remove_file(home.join(".config")).unwrap();
    symlink(&other, home.join(".config")).unwrap();
    fx.backend
        .emit(&home, &[(".config", Flags::SYMLINK | Flags::RENAMED)]);
    fx.settle();
    assert_eq!(o.worktree.revision().n, before.n + 2);
    assert_snapshot!(targets(&fx.backend, &fx.base).join("\n"), @r#"
    home entries [".config"]
    other-dotfiles entries ["git"]
    repo tree
    repo/.git tree
    "#);
    fx.wait();
    assert!(o.sink.take().iter().all(|(batch, _)| batch.meta.config));
}

#[test]
fn config_more_than_forty_links_count_as_missing() {
    let fx = Fixture::new();
    let links = fx.base.join("links");
    fs::create_dir_all(&links).unwrap();
    // l0 -> l1 -> ... -> l41 -> real
    write(&links.join("real"), b"");
    for i in 0..42 {
        let target = if i == 41 {
            PathBuf::from("real")
        } else {
            PathBuf::from(format!("l{}", i + 1))
        };
        symlink(target, links.join(format!("l{i}"))).unwrap();
    }
    fx.source.set_config_files(&[links.join("l0")]);
    let _o = fx.opened();
    let names: Vec<String> = targets(&fx.backend, &fx.base)
        .into_iter()
        .filter(|target| target.starts_with("links "))
        .collect();
    assert_eq!(names.len(), 1);
    assert!(names[0].contains("\"l40\""), "{names:?}");
    assert!(!names[0].contains("\"l41\""));
    assert!(
        !names[0].contains("\"real\""),
        "not resolved past the limit"
    );
}

#[test]
fn config_ignored_file_in_the_worktree_is_seen_before_filtering() {
    let fx = Fixture::new();
    fx.source.ignore(&[".local"]);
    fx.source
        .set_config_files(&[fx.root.join(".local/gitconfig")]);
    let o = fx.opened();
    let before = o.worktree.revision();
    let (filters, configs) = (fx.source.filter_calls(), fx.source.config_calls());
    fx.emit_root(&[(".local/gitconfig", F | Flags::MODIFIED)]);
    fx.settle();
    fx.wait();
    assert_eq!(o.worktree.revision().n, before.n + 1);
    assert_eq!(fx.source.filter_calls(), filters + 1);
    assert_eq!(fx.source.config_calls(), configs + 1);
    assert_snapshot!(worktree_batches(&o.sink.take(), before.n), @r#"rev=1 paths=[] meta=["config"]"#);
}

#[test]
fn config_changes_arm_only_new_targets() {
    let fx = Fixture::new();
    let a = fx.base.join("a");
    let b = fx.base.join("b");
    let c = fx.base.join("c");
    for dir in [&a, &b, &c] {
        fs::create_dir_all(dir).unwrap();
    }
    fx.source.set_config_files(&[a.join("x"), b.join("y")]);
    let _o = fx.opened();
    let start = fx.backend.history().len();
    fx.source.set_config_files(&[a.join("x"), c.join("z")]);
    fx.emit_common(&[("config", F)]);
    fx.settle();
    let changes = history(&fx.backend, &fx.base)[start..].join("\n");
    assert_snapshot!(changes, @r#"
    drop b entries ["y"]
    arm c entries ["z"]
    "#);
}

#[test]
fn config_head_change_recomputes_the_list() {
    let fx = Fixture::new();
    let _o = fx.opened();
    let calls = fx.source.config_calls();
    fx.emit_common(&[("HEAD", F)]);
    fx.settle();
    assert_eq!(fx.source.config_calls(), calls + 1);
    // Another worktree's HEAD does not.
    fx.emit_common(&[("worktrees/other/HEAD", F)]);
    fx.settle();
    assert_eq!(fx.source.config_calls(), calls + 1);
}

#[test]
fn config_load_failure_degrades_and_recovers() {
    let fx = Fixture::new();
    let home = fx.base.join("home");
    fs::create_dir_all(&home).unwrap();
    fx.source.set_config_files(&[home.join(".gitconfig")]);
    let o = fx.opened();
    fx.source.fail_config(Some("bad include"));
    fx.emit_common(&[("config", F)]);
    fx.settle();
    let batches = o.sink.take();
    let degraded = Health::Degraded(Degraded::Config("bad include".to_owned()));
    assert!(
        batches
            .iter()
            .any(|(batch, _)| batch.health == Some(degraded.clone()))
    );
    // Only the config files in the repository stay watched.
    assert!(!fx.backend.is_armed(&home));
    fx.source.fail_config(None);
    fx.emit_common(&[("config", F)]);
    fx.settle();
    let batches = o.sink.take();
    assert!(
        batches
            .iter()
            .any(|(batch, _)| batch.health == Some(Health::Live))
    );
    assert!(fx.backend.is_armed(&home));
}

#[test]
fn config_entries_arm_failure_degrades() {
    let fx = Fixture::new();
    let home = fx.base.join("home");
    fs::create_dir_all(&home).unwrap();
    fx.source.set_config_files(&[home.join(".gitconfig")]);
    fx.backend.fail_next_arm(&home, BackendError::Exhausted);
    let (repo, _repo_sink) = fx.repo();
    let (_worktree, sink) = fx.worktree(&repo);
    fx.settle();
    let batches = sink.take();
    assert!(matches!(
        &batches[0].0.health,
        Some(Health::Degraded(Degraded::Config(message))) if message.contains("out of watch descriptors")
    ));
}
