//! Directory watches (`dir_*`), docs/design/fs_watch.md「目录监听」.

use std::fs;
use std::time::Duration;

use insta::assert_snapshot;

use super::support::*;
use crate::Flags;

#[test]
fn dir_paths_are_absolute_and_coalesced_at_250_ms() {
    let fx = Fixture::new();
    let root = fx.base.join("sessions");
    fs::create_dir_all(&root).unwrap();
    let (_watch, sink) = fx.dir(&root);
    fx.settle();
    assert_snapshot!(dir_batches(&sink.take(), &fx.base), @"paths=[] rescan health=Live");
    fx.backend.emit(
        &root,
        &[("p1/a.jsonl", F), ("p1/b.jsonl", F), ("p1/a.jsonl", F)],
    );
    fx.settle();
    fx.driver.advance(Duration::from_millis(200));
    fx.backend.emit(&root, &[(".hidden/x", F)]);
    fx.settle();
    fx.driver.advance(Duration::from_millis(249));
    assert!(sink.is_empty());
    fx.driver.advance(Duration::from_millis(1));
    let batches = sink.take();
    assert!(batches[0].0.paths.iter().all(|path| path.is_absolute()));
    assert_snapshot!(dir_batches(&batches, &fx.base), @r#"paths=["sessions/.hidden/x", "sessions/p1/a.jsonl", "sessions/p1/b.jsonl"]"#);
}

#[test]
fn dir_missing_root_watches_the_ancestor_until_it_appears() {
    let fx = Fixture::new();
    let root = fx.base.join("home/.codex/sessions");
    fs::create_dir_all(fx.base.join("home")).unwrap();
    let (_watch, sink) = fx.dir(&root);
    fx.settle();
    assert_snapshot!(dir_batches(&sink.take(), &fx.base), @"paths=[] rescan health=Live");
    assert!(targets(&fx.backend, &fx.base).contains(&r#"home entries [".codex"]"#.to_owned()));

    // One level appears: watch it for the next.
    fs::create_dir_all(fx.base.join("home/.codex")).unwrap();
    fx.backend
        .emit(&fx.base.join("home"), &[(".codex", D | Flags::CREATED)]);
    fx.settle();
    assert!(
        targets(&fx.backend, &fx.base).contains(&r#"home/.codex entries ["sessions"]"#.to_owned())
    );
    assert!(!fx.backend.is_armed(&fx.base.join("home")));
    assert!(sink.is_empty());

    // Events on other entries of the ancestor do nothing.
    fx.backend
        .emit(&fx.base.join("home/.codex"), &[("config.toml", F)]);
    fx.settle();
    assert!(sink.is_empty());

    fs::create_dir_all(&root).unwrap();
    fx.backend.emit(
        &fx.base.join("home/.codex"),
        &[("sessions", D | Flags::CREATED)],
    );
    fx.settle();
    assert_snapshot!(dir_batches(&sink.take(), &fx.base), @"paths=[] rescan root=Appeared");
    assert_snapshot!(targets(&fx.backend, &fx.base).join("\n"), @"home/.codex/sessions tree");
}

#[test]
fn dir_removed_root_reports_vanished_and_waits_again() {
    let fx = Fixture::new();
    let root = fx.base.join("projects");
    fs::create_dir_all(&root).unwrap();
    let (_watch, sink) = fx.dir(&root);
    fx.settle();
    sink.take();
    fx.backend.emit(&root, &[("a", F)]);
    fs::remove_dir_all(&root).unwrap();
    fx.backend.emit(&root, &[("", D | Flags::ROOT_CHANGED)]);
    fx.settle();
    assert_snapshot!(dir_batches(&sink.take(), &fx.base), @r#"paths=["projects/a"] root=Vanished"#);
    assert_snapshot!(targets(&fx.backend, &fx.base).join("\n"), @r#" entries ["projects"]"#);

    fs::create_dir_all(&root).unwrap();
    fx.backend
        .emit(&fx.base, &[("projects", D | Flags::CREATED)]);
    fx.settle();
    assert_snapshot!(dir_batches(&sink.take(), &fx.base), @"paths=[] rescan root=Appeared");
}

#[test]
fn dir_rescan_and_shared_roots() {
    let fx = Fixture::new();
    let root = fx.base.join("projects");
    fs::create_dir_all(&root).unwrap();
    let (_a, a) = fx.dir(&root);
    let (_b, b) = fx.dir(&root);
    fx.settle();
    a.take();
    b.take();
    assert_eq!(
        targets(&fx.backend, &fx.base),
        ["projects tree"],
        "one target"
    );
    fx.backend.emit(&root, &[("x", D | Flags::RESCAN)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(dir_batches(&a.take(), &fx.base), @"paths=[] rescan");
    assert_eq!(b.take().len(), 1);
}
