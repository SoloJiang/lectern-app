//! Incremental refresh driven by fs_watch's fake backend (`refresh_*`,
//! docs/design/project.md「测试」).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lectern_fs_watch::Flags;
use lectern_git::HashKind;
use lectern_services::project::{
    Head, Package, ProjectEvent, ProjectKey, Projects, Scan, WorktreeKey,
};

use super::support::*;
use crate::model::Model;
use crate::testing::{Call, FakeGit};

const CHANGED: Flags = Flags(Flags::FILE.0 | Flags::MODIFIED.0);
const REMOVED_DIR: Flags = Flags(Flags::DIR.0 | Flags::REMOVED.0);

/// A model watching through a manual fs_watch, over `FakeGit`.
struct Fixture {
    h: Harness,
    w: Watch,
}

impl Fixture {
    fn new() -> Fixture {
        let w = Watch::new();
        let git = Arc::new(FakeGit::new());
        let home = git.base().join("home");
        let watcher = w.watcher.clone();
        let h = Harness::with_model(git, move |g| {
            let model = Model::new(g, home);
            model.watch_with(watcher);
            model
        });
        Fixture { h, w }
    }

    /// Opens `path` and settles everything, including the first batch of
    /// each watch; then forgets the events and calls so far.
    fn open(&self, path: &Path) -> ProjectKey {
        let request = self.h.model.open(path);
        self.settle();
        let events = self.h.host.take_events();
        check_event_order(&events);
        let key = opened(&events, request).unwrap();
        self.h.git.take_calls();
        key
    }

    fn settle(&self) {
        self.w.settle(&self.h.host);
    }

    /// Injects one raw batch and lets fs_watch take it in at once, as its
    /// thread would; the coalescing window then waits for the clock.
    fn emit(&self, target: &Path, events: &[(&str, Flags)]) {
        self.w.backend.emit(target, events);
        self.w.driver.run_until_idle();
    }

    /// Settles and returns the events, checked for order.
    fn events(&self) -> String {
        self.settle();
        let events = self.h.host.take_events();
        check_event_order(&events);
        render_events(&events, self.h.base())
    }

    fn count(&self, wanted: fn(&Call) -> bool) -> usize {
        self.h.git.calls().iter().filter(|c| wanted(c)).count()
    }

    fn parses(&self) -> usize {
        self.h.model.parses.load(Ordering::Relaxed)
    }

    fn packages(&self, key: &ProjectKey) -> Arc<[Package]> {
        match &self.h.snapshot(key).active.as_ref().unwrap().packages {
            Scan::Ready(packages) => packages.clone(),
            other => panic!("packages {other:?}"),
        }
    }

    fn package(&self, key: &ProjectKey, dir: &str) -> Package {
        self.packages(key)
            .iter()
            .find(|p| &*p.dir == dir)
            .cloned()
            .unwrap_or_else(|| panic!("no package {dir}"))
    }
}

fn is_checksum(c: &Call) -> bool {
    matches!(c, Call::IndexChecksum(_))
}

fn is_index(c: &Call) -> bool {
    matches!(c, Call::IndexFiles(_))
}

fn is_list(c: &Call) -> bool {
    matches!(c, Call::Worktrees(_))
}

/// `monorepo` at `<base>/repo`; returns (common dir, root).
fn repo(f: &Fixture) -> (PathBuf, PathBuf) {
    let common = f.h.git.init("repo");
    monorepo(&f.h.git, &common);
    let root = f.h.base().join("repo");
    (common, root)
}

#[test]
fn refresh_first_batch_after_open_checks_once_and_changes_nothing() {
    let f = Fixture::new();
    let (common, root) = repo(&f);
    let request = f.h.model.open(&root);
    f.settle();
    let events = f.h.host.take_events();
    check_event_order(&events);
    assert!(opened(&events, request).is_ok());
    // Opened, files, packages; the first batch of each watch (a rescan)
    // replaces nothing.
    insta::assert_snapshot!(render_events(&events, f.h.base()), @r#"
    Opened Ok(Git { common_dir: "[base]/repo/.git" })
    Changed 2 [files]
    Changed 3 [packages]
    "#);
    // It listed the worktrees, read the index checksum (unchanged, so not
    // the index) and every manifest (the parse cache answered).
    let calls = f.h.git.calls();
    let after_scan = calls.iter().rposition(is_index).unwrap();
    let tail = &calls[after_scan + 1..];
    assert_eq!(tail.iter().filter(|c| is_list(c)).count(), 1, "{tail:?}");
    assert_eq!(
        tail.iter().filter(|c| is_checksum(c)).count(),
        1,
        "{tail:?}"
    );
    assert_eq!(f.parses(), 4, "each manifest parsed once");
    // One watch on the common directory, one on the active worktree.
    let armed: Vec<PathBuf> = f.w.backend.targets().into_iter().map(|t| t.path).collect();
    assert_eq!(armed, vec![root.clone(), common.clone()]);
}

#[test]
fn refresh_burst_of_events_is_one_round() {
    let f = Fixture::new();
    let (common, root) = repo(&f);
    let key = f.open(&root);
    f.h.git.write(&common, "apps/web/src/a.ts", b"a\n");
    f.h.git.write(&common, "apps/web/src/b.ts", b"b\n");
    // Within fs_watch's 50 ms quiet window: one batch.
    f.emit(&common, &[("index", CHANGED)]);
    f.emit(&root, &[("apps/web/src/a.ts", CHANGED)]);
    f.w.driver.advance(Duration::from_millis(20));
    f.emit(&common, &[("index", CHANGED)]);
    f.emit(&root, &[("apps/web/src/b.ts", CHANGED)]);
    // Two more batches reach the model before its queued round runs: they
    // join that round.
    f.w.driver.advance(LONG);
    f.h.git.write(&common, "apps/web/src/c.ts", b"c\n");
    f.emit(&common, &[("index", CHANGED)]);
    f.w.driver.advance(LONG);
    f.emit(&root, &[("apps/web/src/c.ts", CHANGED)]);
    f.w.driver.advance(LONG);
    insta::assert_snapshot!(f.events(), @"Changed 4 [files, packages]");
    assert_eq!(f.count(is_checksum), 1);
    assert_eq!(f.count(is_index), 1);
    assert_eq!(f.package(&key, "apps/web").files, 5);
}

#[test]
fn refresh_event_during_a_round_starts_another() {
    let f = Arc::new(Fixture::new());
    let (common, root) = repo(&f);
    let key = f.open(&root);
    // While the round for the first change reads the manifest, an agent
    // stages a new package and fs_watch delivers that: the next round
    // picks it up.
    let during = Arc::clone(&f);
    let staged = common.clone();
    f.h.git
        .before_read(&common, "apps/web/package.json", move || {
            during
                .h
                .git
                .write(&staged, "apps/new/package.json", br#"{ "name": "new" }"#);
            during.emit(&staged, &[("index", CHANGED)]);
            during.w.driver.advance(LONG);
        });
    f.h.git
        .write_unstaged(&common, "apps/web/package.json", br#"{ "name": "web2" }"#);
    f.emit(&root, &[("apps/web/package.json", CHANGED)]);
    insta::assert_snapshot!(f.events(), @r"
    Changed 4 [packages]
    Changed 5 [files, packages]
    ");
    assert_eq!(&*f.package(&key, "apps/web").name, "web2");
    assert_eq!(&*f.package(&key, "apps/new").name, "new");
    // The fixture holds itself through the hook until it ran.
    drop(f);
}

#[test]
fn refresh_content_only_change_reads_nothing() {
    let f = Fixture::new();
    let (common, root) = repo(&f);
    let key = f.open(&root);
    let before = f.h.snapshot(&key);
    f.h.git.write_unstaged(&common, "README.md", b"# changed\n");
    f.h.git
        .write_unstaged(&common, "apps/web/src/index.ts", b"export const x = 1;\n");
    f.emit(
        &root,
        &[("README.md", CHANGED), ("apps/web/src/index.ts", CHANGED)],
    );
    insta::assert_snapshot!(f.events(), @"");
    assert_eq!(f.h.git.calls(), Vec::new(), "no index read, no file read");
    assert!(Arc::ptr_eq(&before, &f.h.snapshot(&key)));
}

#[test]
fn refresh_new_tracked_file_updates_counts_without_rereading_manifests() {
    let f = Fixture::new();
    let (common, root) = repo(&f);
    let key = f.open(&root);
    assert_eq!(f.package(&key, "apps/web").files, 2);
    let facts = f.h.host.facts_count();
    let parses = f.parses();
    f.h.git
        .write(&common, "apps/web/src/extra.ts", b"export {};\n");
    f.emit(&common, &[("index", CHANGED)]);
    f.emit(&root, &[("apps/web/src/extra.ts", CHANGED)]);
    insta::assert_snapshot!(f.events(), @"Changed 4 [files, packages]");
    assert_eq!(f.package(&key, "apps/web").files, 3);
    assert_eq!(f.package(&key, "").files, 8);
    assert_eq!(
        f.h.git.reads(),
        Vec::<String>::new(),
        "no manifest read again"
    );
    assert_eq!(f.parses(), parses);
    // The new file list reached the facts.
    assert_eq!(f.h.host.facts_count(), facts + 1);
    assert!(f.h.host.facts().unwrap().contains("extra.ts"));
}

#[test]
fn refresh_stat_only_index_rewrite_changes_nothing() {
    let f = Fixture::new();
    let (common, root) = repo(&f);
    let key = f.open(&root);
    let before = f.h.snapshot(&key);
    // `git status` rewrites the index with new stat data: a new checksum,
    // the same paths. The index is read; nothing is replaced and no
    // manifest is read.
    f.h.git.touch_index(&common);
    f.emit(&common, &[("index", CHANGED)]);
    insta::assert_snapshot!(f.events(), @"");
    assert_eq!(
        f.h.git.calls(),
        vec![
            Call::IndexChecksum(common.clone()),
            Call::IndexFiles(common.clone())
        ]
    );
    assert!(Arc::ptr_eq(&before, &f.h.snapshot(&key)));
    // The new checksum was kept: an event with the index unchanged reads
    // only the checksum.
    f.h.git.take_calls();
    f.emit(&common, &[("index", CHANGED)]);
    insta::assert_snapshot!(f.events(), @"");
    assert_eq!(f.h.git.calls(), vec![Call::IndexChecksum(common.clone())]);
}

fn staged_content_change(hash: HashKind) {
    let f = Fixture::new();
    let common = f.h.git.init_with("repo", hash);
    monorepo(&f.h.git, &common);
    let root = f.h.base().join("repo");
    let key = f.open(&root);
    let before = f.h.snapshot(&key);
    let old = active_files(&before);
    let facts = f.h.host.facts_count();
    let parses = f.parses();

    // `git add` of a new version of a source file: the same paths, one new
    // OID. The file list is replaced (a new generation), the sub-projects
    // stay, no manifest is read.
    let content = b"export const x = 1;\n";
    f.h.git.write(&common, "apps/web/src/index.ts", content);
    f.emit(&common, &[("index", CHANGED)]);
    f.emit(&root, &[("apps/web/src/index.ts", CHANGED)]);
    assert_eq!(f.events(), "Changed 4 [files]\n");
    let after = f.h.snapshot(&key);
    assert_eq!(after.generation, before.generation + 1);
    let new = active_files(&after);
    assert!(!Arc::ptr_eq(&old, &new));
    assert!(old.iter().eq(new.iter()), "the same paths and kinds");
    assert_eq!(
        oid_of(&old, "apps/web/src/index.ts"),
        blob(hash, b"export {};\n")
    );
    assert_eq!(oid_of(&new, "apps/web/src/index.ts"), blob(hash, content));
    for (i, (path, _)) in new.iter().enumerate() {
        if path != "apps/web/src/index.ts" {
            assert_eq!(new.oid(i), oid_of(&old, path), "{path}");
        }
    }
    assert_eq!(f.h.git.reads(), Vec::<String>::new());
    assert_eq!(f.parses(), parses);
    // The facts hold the new list, so the old one is released.
    assert_eq!(f.h.host.facts_count(), facts + 1);

    // Staging the same bytes again (and a stat-only rewrite): the index is
    // read, every OID is the same, nothing is replaced.
    f.h.git.take_calls();
    f.h.git.write(&common, "apps/web/src/index.ts", content);
    f.h.git.touch_index(&common);
    f.emit(&common, &[("index", CHANGED)]);
    assert_eq!(f.events(), "");
    assert_eq!(f.count(is_index), 1);
    assert!(Arc::ptr_eq(&after, &f.h.snapshot(&key)));
    assert_eq!(f.h.host.facts_count(), facts + 1);
}

#[test]
fn refresh_staged_content_change_replaces_the_file_list_sha1() {
    staged_content_change(HashKind::Sha1);
}

#[test]
fn refresh_staged_content_change_replaces_the_file_list_sha256() {
    staged_content_change(HashKind::Sha256);
}

#[test]
fn refresh_manifest_with_the_same_oid_is_not_parsed_again() {
    let f = Fixture::new();
    let (common, root) = repo(&f);
    let key = f.open(&root);
    let parses = f.parses();
    // Rewritten with the same bytes: read, not parsed, nothing changes.
    f.h.git
        .write_unstaged(&common, "apps/web/package.json", br#"{ "name": "web" }"#);
    f.emit(&root, &[("apps/web/package.json", CHANGED)]);
    insta::assert_snapshot!(f.events(), @"");
    assert_eq!(f.h.git.reads(), vec!["apps/web/package.json".to_owned()]);
    assert_eq!(f.parses(), parses);
    assert_eq!(f.count(is_checksum), 0, "a manifest change reads no index");

    // New bytes (unstaged): only this manifest is read and parsed.
    f.h.git.take_calls();
    f.h.git.write_unstaged(
        &common,
        "apps/web/package.json",
        br#"{ "name": "web-app" }"#,
    );
    f.emit(&root, &[("apps/web/package.json", CHANGED)]);
    insta::assert_snapshot!(f.events(), @"Changed 4 [packages]");
    assert_eq!(f.h.git.reads(), vec!["apps/web/package.json".to_owned()]);
    assert_eq!(f.parses(), parses + 1);
    assert_eq!(&*f.package(&key, "apps/web").name, "web-app");

    // A directory event stands for the manifests below it.
    f.h.git.take_calls();
    f.h.git
        .write_unstaged(&common, "apps/console/package.json", br#"{ "name": "c2" }"#);
    f.emit(
        &root,
        &[("apps/console", Flags(Flags::DIR.0 | Flags::RENAMED.0))],
    );
    insta::assert_snapshot!(f.events(), @"Changed 5 [packages]");
    assert_eq!(
        f.h.git.reads(),
        vec!["apps/console/package.json".to_owned()]
    );
    assert_eq!(&*f.package(&key, "apps/console").name, "c2");
}

#[test]
fn refresh_removing_the_active_worktree_switches_to_main() {
    let f = Fixture::new();
    let (common, _) = repo(&f);
    let linked = f.h.git.add_worktree(&common, "feature", "repo-feature");
    f.h.git.write(&linked, "go.work", b"go 1.22\n\nuse ./svc\n");
    f.h.git
        .write(&linked, "svc/go.mod", b"module example.com/svc\n");
    let feature_root = f.h.base().join("repo-feature");
    let key = f.open(&feature_root);
    assert!(f.w.backend.is_armed(&feature_root));

    // `git worktree remove feature`.
    f.h.git.remove_worktree(&linked);
    f.emit(&common, &[("worktrees/feature", REMOVED_DIR)]);
    insta::assert_snapshot!(f.events(), @r"
    Changed 4 [worktrees, active, files, packages]
    Changed 5 [files]
    Changed 6 [packages]
    ");
    let snapshot = f.h.snapshot(&key);
    let active = snapshot.active.as_ref().unwrap();
    assert_eq!(active.worktree, Some(WorktreeKey(common.as_path().into())));
    assert_eq!(snapshot.worktrees.len(), 1);
    // The main worktree is watched now, the removed one no longer.
    assert!(f.w.backend.is_armed(&f.h.base().join("repo")));
    assert!(!f.w.backend.is_armed(&feature_root));
}

#[test]
fn refresh_deleted_active_root_becomes_prunable_and_switches() {
    let f = Fixture::new();
    let (common, _) = repo(&f);
    let linked = f.h.git.add_worktree(&common, "feature", "repo-feature");
    f.h.git.write(&linked, "README.md", b"feature\n");
    let feature_root = f.h.base().join("repo-feature");
    let key = f.open(&feature_root);
    // The root is deleted; its administrative directory stays.
    f.h.git.delete_root(&linked);
    f.emit(
        &feature_root,
        &[("", Flags(Flags::DIR.0 | Flags::ROOT_CHANGED.0))],
    );
    insta::assert_snapshot!(f.events(), @r"
    Changed 4 [worktrees, active, files, packages]
    Changed 5 [files]
    Changed 6 [packages]
    ");
    insta::assert_snapshot!(f.h.render(&key));
}

#[test]
fn refresh_head_of_another_worktree_updates_only_the_list() {
    let f = Fixture::new();
    let (common, root) = repo(&f);
    let linked = f.h.git.add_worktree(&common, "feature", "repo-feature");
    let key = f.open(&root);
    let before = f.h.snapshot(&key);
    f.h.git.set_head(
        &linked,
        Head::Detached("0123456789abcdef0123456789abcdef01234567".into()),
    );
    f.emit(&common, &[("worktrees/feature/HEAD", CHANGED)]);
    insta::assert_snapshot!(f.events(), @"Changed 4 [worktrees]");
    assert_eq!(f.h.git.calls(), vec![Call::Worktrees(common.clone())]);
    let after = f.h.snapshot(&key);
    assert_eq!(after.active, before.active);
    assert!(matches!(after.worktrees[1].head, Head::Detached(_)));
}

#[test]
fn refresh_first_commit_on_an_unborn_branch_updates_the_head() {
    let f = Fixture::new();
    let (common, root) = repo(&f);
    f.h.git
        .set_head(&common, Head::Unborn("refs/heads/main".into()));
    let key = f.open(&root);
    // The first commit writes `refs/heads/main`, not HEAD.
    f.h.git
        .set_head(&common, Head::Branch("refs/heads/main".into()));
    f.emit(&common, &[("refs/heads/main", CHANGED)]);
    insta::assert_snapshot!(f.events(), @"Changed 4 [worktrees]");
    assert!(matches!(
        f.h.snapshot(&key).worktrees[0].head,
        Head::Branch(_)
    ));
    // With every branch born, reference updates list nothing.
    f.h.git.take_calls();
    f.emit(&common, &[("refs/heads/main", CHANGED)]);
    insta::assert_snapshot!(f.events(), @"");
    assert_eq!(f.h.git.calls(), Vec::new());
}

#[test]
fn refresh_generations_strictly_increase() {
    let f = Fixture::new();
    let (common, root) = repo(&f);
    let linked = f.h.git.add_worktree(&common, "feature", "repo-feature");
    let request = f.h.model.open(&root);
    f.settle();
    let mut all = f.h.host.take_events();
    let key = opened(&all, request).unwrap();
    for i in 0..3 {
        f.h.git
            .write(&common, &format!("apps/web/src/f{i}.ts"), b"x\n");
        f.emit(&common, &[("index", CHANGED)]);
        f.settle();
        f.h.git
            .set_head(&linked, Head::Detached(format!("{i:040}").into()));
        f.emit(&common, &[("worktrees/feature/HEAD", CHANGED)]);
        f.settle();
    }
    f.h.model
        .set_active_worktree(&key, &WorktreeKey(linked.as_path().into()));
    f.settle();
    all.extend(f.h.host.take_events());
    check_event_order(&all);
    let last = all
        .iter()
        .filter_map(|e| match e {
            ProjectEvent::Changed { generation, .. } => Some(*generation),
            _ => None,
        })
        .max()
        .unwrap();
    assert_eq!(f.h.snapshot(&key).generation, last);
    assert_eq!(
        last,
        3 + 3 * 2 + 3,
        "one per index and HEAD change, three to switch"
    );
}

#[test]
fn refresh_closed_projects_and_folders_are_not_watched() {
    let f = Fixture::new();
    let (common, root) = repo(&f);
    let key = f.open(&root);
    assert!(!f.w.backend.targets().is_empty());
    f.h.model.close(&key);
    f.settle();
    assert_eq!(f.w.backend.targets(), Vec::new(), "the watches are dropped");
    assert!(f.h.model.snapshot(&key).is_none());
    let _ = common;

    let folder = f.h.base().join("folder");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("package.json"), b"{}").unwrap();
    f.open(&folder);
    assert_eq!(
        f.w.backend.targets(),
        Vec::new(),
        "folders are scanned once"
    );
}
