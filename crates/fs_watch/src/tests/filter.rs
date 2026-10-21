//! Path classification (`filter_*`) and the stale filter (`filter_stale_*`),
//! docs/design/fs_watch.md「路径分类」.

use insta::assert_snapshot;

use super::support::*;
use crate::{Degraded, Flags, Health};

const MODIFIED: Flags = Flags::MODIFIED;

fn base(opened: &Opened) -> u64 {
    opened.worktree.revision().n
}

#[test]
fn filter_tracked_and_untracked_files_are_delivered() {
    let fx = Fixture::new();
    fx.source.track(&["src/main.rs", "README.md"]);
    fx.source.ignore(&["node_modules", "*.log"]);
    let o = fx.opened();
    let base = base(&o);
    fx.emit_root(&[
        ("src/main.rs", F | MODIFIED),
        ("notes.txt", F | Flags::CREATED),
        ("debug.log", F),
        ("src/new.rs", F),
    ]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=1 paths=["notes.txt", "src/main.rs", "src/new.rs"] meta=[]"#);
}

#[test]
fn filter_ignored_directory_contents_are_dropped() {
    let fx = Fixture::new();
    fx.source.track(&["package.json"]);
    fx.source.ignore(&["node_modules"]);
    let o = fx.opened();
    let base = base(&o);
    fx.emit_root(&[
        ("node_modules", D),
        ("node_modules/left-pad/index.js", F),
        ("node_modules/left-pad/lib/a.js", F),
    ]);
    fx.settle();
    fx.wait();
    assert!(o.sink.take().is_empty());
    assert_eq!(o.worktree.revision().n, base, "dropped paths do not count");
}

#[test]
fn filter_tracked_file_in_an_ignored_directory_is_delivered() {
    let fx = Fixture::new();
    fx.source.track(&["build/keep.txt"]);
    fx.source.ignore(&["build"]);
    let o = fx.opened();
    let base = base(&o);
    fx.emit_root(&[("build/keep.txt", F), ("build/out.js", F)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=1 paths=["build/keep.txt"] meta=[]"#);
}

#[test]
fn filter_rule_files_are_delivered_even_when_ignored() {
    let fx = Fixture::new();
    fx.source.ignore(&[".gitignore", ".gitattributes", "*.tmp"]);
    let o = fx.opened();
    let base = base(&o);
    let calls = fx.source.filter_calls();
    fx.emit_root(&[("docs/.gitattributes", F), ("x.tmp", F)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=1 paths=["docs/.gitattributes"] meta=["rules"]"#);
    assert_eq!(
        fx.source.filter_calls(),
        calls,
        ".gitattributes does not reload"
    );

    fx.emit_root(&[("sub/.gitignore", F)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=2 paths=["sub/.gitignore"] meta=["rules"]"#);
    assert_eq!(fx.source.filter_calls(), calls + 1, ".gitignore reloads");
    assert_eq!(
        fx.source.gitignores().last().unwrap(),
        &vec![wp("sub/.gitignore")]
    );
}

#[test]
fn filter_rule_file_in_an_ignored_directory_is_dropped_without_reload() {
    let fx = Fixture::new();
    fx.source.ignore(&["node_modules"]);
    let o = fx.opened();
    let base = base(&o);
    let calls = fx.source.filter_calls();
    fx.emit_root(&[("node_modules/x/.gitignore", F)]);
    fx.settle();
    fx.wait();
    assert!(o.sink.take().is_empty());
    assert_eq!(fx.source.filter_calls(), calls);
    assert_eq!(o.worktree.revision().n, base);
}

#[test]
fn filter_git_directories_are_dropped() {
    let fx = Fixture::new();
    fx.source.track(&["vendor/lib/a.c"]);
    let o = fx.opened();
    let base = base(&o);
    fx.emit_root(&[
        (".git/index", F),
        (".git/objects/ab/cdef", F),
        ("vendor/lib/.git/HEAD", F),
        ("vendor/lib/.git", D),
    ]);
    fx.settle();
    fx.wait();
    assert!(o.sink.take().is_empty());
    assert_eq!(o.worktree.revision().n, base);
}

#[test]
fn filter_submodule_contents_are_dropped() {
    let fx = Fixture::new();
    fx.source.gitlink(&["deps/sub"]);
    let o = fx.opened();
    let base = base(&o);
    fx.emit_root(&[
        ("deps/sub", D),
        ("deps/sub/src/a.c", F),
        ("deps/sub/x/y", F),
    ]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=1 paths=["deps/sub"] meta=[]"#);
}

#[test]
fn filter_unfiltered_paths_and_their_directories_are_delivered() {
    let fx = Fixture::new();
    fx.source.ignore(&["build", "*.log"]);
    let o = fx.opened();
    let base = base(&o);
    o.repo
        .set_unfiltered(vec![wp("build/cache/out.bin"), wp("debug.log")]);
    fx.emit_root(&[
        ("build/cache/out.bin", F),
        ("build/cache", D | Flags::REMOVED),
        ("build", D | Flags::REMOVED),
        ("build/other.bin", F),
        ("debug.log", F),
        ("other.log", F),
    ]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=1 paths=["build", "build/cache", "build/cache/out.bin", "debug.log"] meta=[]"#);

    // The list is the union over the handles; clearing it filters again.
    o.repo.set_unfiltered(Vec::new());
    fx.emit_root(&[("debug.log", F)]);
    fx.settle();
    fx.wait();
    assert!(o.sink.take().is_empty());
}

#[test]
fn filter_unfiltered_list_is_the_union_of_the_handles() {
    let fx = Fixture::new();
    fx.source.ignore(&["*.log"]);
    let o = fx.opened();
    let (other, _other_sink) = fx.repo();
    o.repo.set_unfiltered(vec![wp("a.log")]);
    other.set_unfiltered(vec![wp("b.log")]);
    fx.settle();
    let base = base(&o);
    fx.emit_root(&[("a.log", F), ("b.log", F), ("c.log", F)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=1 paths=["a.log", "b.log"] meta=[]"#);
    // Unfiltered list changes neither count nor reload.
    assert_eq!(o.worktree.revision().n, base + 1);
}

#[test]
fn filter_invalid_paths_are_dropped() {
    let fx = Fixture::new();
    let o = fx.opened();
    let base = base(&o);
    fx.emit_root(&[
        ("a//b", F),
        ("./x", F),
        ("../out", F),
        ("/abs", F),
        ("ok", F),
    ]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=1 paths=["ok"] meta=[]"#);
}

#[test]
fn filter_directory_cache_is_cleared_when_the_filter_changes() {
    let fx = Fixture::new();
    fx.source.ignore(&["out"]);
    let o = fx.opened();
    let base = base(&o);
    fx.emit_root(&[("out/a.js", F)]);
    fx.settle();
    fx.wait();
    assert!(
        o.sink.take().is_empty(),
        "out/ is ignored and cached as such"
    );

    // A new filter no longer ignores `out` (`info/exclude` changed).
    fx.source.unignore(&["out"]);
    fx.emit_common(&[("info/exclude", F)]);
    fx.settle();
    fx.wait();
    o.sink.take();
    fx.emit_root(&[("out/b.js", F)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=2 paths=["out/b.js"] meta=[]"#);
}

#[test]
fn filter_directory_rename_or_removal_reloads_the_filter() {
    let fx = Fixture::new();
    fx.source.track(&["pkg/a/x.rs"]);
    fx.source.rule_files(&["pkg/a/.gitignore"]);
    let o = fx.opened();
    let calls = fx.source.filter_calls();

    // Only the directory is reported, and it holds a rule file.
    fx.emit_root(&[("pkg", D | MODIFIED)]);
    fx.settle();
    assert_eq!(fx.source.filter_calls(), calls + 1);

    // A directory renamed (moved in): its `.gitignore` is found by the walk.
    fx.emit_root(&[("vendored", D | Flags::RENAMED)]);
    fx.settle();
    assert_eq!(fx.source.filter_calls(), calls + 2);

    fx.emit_root(&[("gone", D | Flags::REMOVED)]);
    fx.settle();
    assert_eq!(fx.source.filter_calls(), calls + 3);

    // A plain file change does not reload.
    fx.emit_root(&[("pkg/a/x.rs", F | MODIFIED)]);
    fx.settle();
    assert_eq!(fx.source.filter_calls(), calls + 3);
    fx.wait();
    let batches = o.sink.take();
    assert!(!batches.is_empty());
}

#[test]
fn filter_ignored_directory_rename_does_not_reload() {
    let fx = Fixture::new();
    fx.source.ignore(&["node_modules"]);
    let _o = fx.opened();
    let calls = fx.source.filter_calls();
    fx.emit_root(&[
        ("node_modules/.staging/pkg", D | Flags::RENAMED),
        ("node_modules", D | Flags::RENAMED),
    ]);
    fx.settle();
    assert_eq!(fx.source.filter_calls(), calls);
}

// --- the stale filter ----------------------------------------------------------

/// Processes the queue without running the executor: loads stay pending.
fn pump(fx: &Fixture) {
    fx.driver.shared().pump(true);
}

#[test]
fn filter_stale_held_paths_follow_the_new_filter() {
    let fx = Fixture::new();
    fx.source.ignore(&["build"]);
    let o = fx.opened();
    let base = base(&o);

    // `git add -f build/out.js`: the index changes, then the file is edited
    // before the new filter is in.
    fx.source.track(&["build/out.js"]);
    fx.emit_common(&[("index", F)]);
    pump(&fx);
    fx.emit_root(&[("build/out.js", F | MODIFIED), ("build/tmp.o", F)]);
    pump(&fx);
    fx.driver.advance(LONG);
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=2 paths=["build/out.js"] meta=["index"]"#);
}

#[test]
fn filter_stale_batches_wait_for_the_new_filter() {
    let fx = Fixture::new();
    let o = fx.opened();
    fx.emit_common(&[("index", F)]);
    pump(&fx);
    fx.emit_root(&[("a.txt", F)]);
    pump(&fx);
    // The window ended, but the filter is stale.
    fx.clock_advance(LONG);
    pump(&fx);
    assert!(o.sink.is_empty());
    assert!(!fx.driver.shared().has_timer(), "no timer while stale");
    fx.settle();
    assert_eq!(o.sink.take().len(), 1, "delivered once the filter is in");
}

#[test]
fn filter_stale_new_gitignore_drops_the_burst_that_follows() {
    let fx = Fixture::new();
    let o = fx.opened();
    let base = base(&o);

    // A new, untracked `.gitignore`, then `npm install` before the reload.
    write(&fx.root.join(".gitignore"), b"node_modules\n");
    fx.emit_root(&[(".gitignore", F | Flags::CREATED)]);
    pump(&fx);
    fx.source.ignore(&["node_modules"]);
    for chunk in 0..10 {
        let paths: Vec<String> = (0..1000)
            .map(|i| format!("node_modules/p{chunk}x{i}/index.js"))
            .collect();
        let events: Vec<(&str, Flags)> = paths.iter().map(|path| (path.as_str(), F)).collect();
        fx.emit_root(&events);
        pump(&fx);
    }
    fx.settle();
    fx.wait();
    assert_eq!(
        fx.source.gitignores().last().unwrap(),
        &vec![wp(".gitignore")]
    );
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=11 paths=[".gitignore"] meta=["rules"]"#);
}

#[test]
fn filter_stale_held_paths_count() {
    let fx = Fixture::new();
    fx.source.ignore(&["build"]);
    let o = fx.opened();
    let before = o.worktree.revision();
    fx.emit_common(&[("index", F)]);
    pump(&fx);
    let after_index = o.worktree.revision();
    assert_eq!(after_index.n, before.n + 1);
    fx.emit_root(&[("build/x", F)]);
    pump(&fx);
    assert_eq!(o.worktree.revision().n, before.n + 2, "held, and counted");
    fx.settle();
    fx.wait();
    // The new filter still ignores build/: the held path is dropped.
    assert_snapshot!(worktree_batches(&o.sink.take(), before.n), @r#"rev=2 paths=[] meta=["index"]"#);
}

#[test]
fn filter_stale_events_before_the_first_filter_are_kept() {
    let fx = Fixture::new();
    let (repo, _repo_sink) = fx.repo();
    let (worktree, sink) = fx.worktree(&repo);
    pump(&fx);
    let armed = worktree.revision();
    // The first load has not run: everything after step 4 is held.
    write(&fx.root.join("sub/.gitignore"), b"*.o\n");
    fx.emit_root(&[("a.txt", F), ("sub/.gitignore", F | Flags::CREATED)]);
    pump(&fx);
    assert_eq!(worktree.revision().n, armed.n + 1, "counted while held");
    assert!(sink.is_empty());
    let calls = fx.source.filter_calls();
    fx.settle();
    // The first batch is a rescan; the held `.gitignore` the first walk may
    // have missed reloads the filter.
    assert_eq!(fx.source.filter_calls(), calls + 2);
    assert_eq!(
        fx.source.gitignores().last().unwrap(),
        &vec![wp("sub/.gitignore")]
    );
    let batches = sink.take();
    assert_eq!(batches.len(), 1);
    assert!(batches[0].0.rescan);
    assert_eq!(batches[0].0.health, Some(Health::Live));
}

#[test]
fn filter_stale_overflow_at_the_root_becomes_a_rescan() {
    let fx = Fixture::new();
    let o = fx.opened();
    let base = base(&o);
    fx.emit_common(&[("index", F)]);
    pump(&fx);
    let paths: Vec<String> = (0..5000).map(|i| format!("f{i}")).collect();
    for chunk in paths.chunks(1000) {
        let events: Vec<(&str, Flags)> = chunk.iter().map(|path| (path.as_str(), F)).collect();
        fx.emit_root(&events);
        pump(&fx);
    }
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=6 paths=[] meta=["index"] rescan"#);
}

#[test]
fn filter_stale_failed_load_degrades_and_delivers_everything() {
    let fx = Fixture::new();
    fx.source.ignore(&["node_modules"]);
    let o = fx.opened();
    let base = base(&o);
    fx.source.fail_filter(Some("index is corrupt"));
    fx.emit_common(&[("index", F)]);
    fx.settle();
    fx.emit_root(&[("node_modules/a.js", F), (".git/HEAD", F)]);
    fx.settle();
    fx.wait();
    let degraded = Health::Degraded(Degraded::Filter("index is corrupt".to_owned()));
    let batches = o.sink.take();
    assert_eq!(
        batches
            .iter()
            .filter(|(b, _)| b.health == Some(degraded.clone()))
            .count(),
        1
    );
    assert_snapshot!(worktree_batches(&batches, base), @r#"
    rev=1 paths=[] meta=["index"]
    rev=1 paths=[] meta=[] health=Degraded(Filter("index is corrupt"))
    rev=2 paths=["node_modules/a.js"] meta=[]
    "#);

    // The next trigger retries; success goes back to Live.
    fx.source.fail_filter(None);
    fx.emit_common(&[("index", F)]);
    fx.settle();
    fx.wait();
    let batches = o.sink.take();
    assert!(batches.iter().any(|(b, _)| b.health == Some(Health::Live)));
}

// --- what a reload may reuse (`FilterRequest::previous`) -----------------------

#[test]
fn filter_reuse_index_change_passes_the_filter_in_place() {
    let fx = Fixture::new();
    let _o = fx.opened();
    assert_eq!(fx.source.previous(), [None], "the first load reads it all");
    fx.emit_common(&[("index", F)]);
    fx.settle();
    fx.emit_common(&[("index", F)]);
    fx.settle();
    assert_eq!(fx.source.previous(), [None, Some(1), Some(2)]);
}

#[test]
fn filter_reuse_every_other_reload_reads_the_worktree() {
    let fx = Fixture::new();
    fx.source.track(&["pkg/a/x.rs"]);
    fx.source.rule_files(&["pkg/a/.gitignore"]);
    let _o = fx.opened();
    fx.emit_root(&[("sub/.gitignore", F | Flags::CREATED)]);
    fx.settle();
    fx.emit_root(&[("pkg", D | MODIFIED)]);
    fx.settle();
    fx.emit_root(&[("vendored", D | Flags::RENAMED)]);
    fx.settle();
    fx.emit_common(&[("info/exclude", F)]);
    fx.settle();
    fx.emit_common(&[("config", F)]);
    fx.settle();
    fx.emit_root(&[("", Flags::RESCAN)]);
    fx.settle();
    fx.emit_common(&[("", Flags::RESCAN)]);
    fx.settle();
    let previous = fx.source.previous();
    assert_eq!(previous.len(), 8, "{previous:?}");
    assert!(previous.iter().all(Option::is_none), "{previous:?}");
}

#[test]
fn filter_reuse_gitignore_during_an_index_load_reads_the_worktree_next() {
    let fx = Fixture::new();
    let _o = fx.opened();
    fx.emit_common(&[("index", F)]);
    pump(&fx);
    // While that load runs: a new `.gitignore`, then the index again. One
    // load serves both, and it reads the worktree.
    write(&fx.root.join("sub/.gitignore"), b"*.o\n");
    fx.emit_root(&[("sub/.gitignore", F | Flags::CREATED)]);
    pump(&fx);
    fx.emit_common(&[("index", F)]);
    pump(&fx);
    fx.settle();
    assert_eq!(fx.source.previous(), [None, Some(1), None]);
    assert_eq!(
        fx.source.gitignores().last().unwrap(),
        &vec![wp("sub/.gitignore")]
    );

    // Index changes queued behind one another, nothing else: reused.
    fx.emit_common(&[("index", F)]);
    pump(&fx);
    fx.emit_common(&[("index", F)]);
    pump(&fx);
    fx.settle();
    assert_eq!(
        fx.source.previous(),
        [None, Some(1), None, Some(3), Some(4)]
    );
}

#[test]
fn filter_reuse_waits_for_paths_held_before_the_first_filter() {
    let fx = Fixture::new();
    let (repo, _repo_sink) = fx.repo();
    let (_worktree, _sink) = fx.worktree(&repo);
    pump(&fx);
    // Before the first filter: a new `.gitignore` (held, its trigger not
    // checked yet), then an index change queued behind the first load.
    write(&fx.root.join("sub/.gitignore"), b"*.o\n");
    fx.emit_root(&[("sub/.gitignore", F | Flags::CREATED)]);
    pump(&fx);
    fx.emit_common(&[("index", F)]);
    pump(&fx);
    fx.settle();
    let previous = fx.source.previous();
    assert!(previous.iter().all(Option::is_none), "{previous:?}");
}

/// Index changes until one gets `previous`: the loads in between read the
/// worktree.
fn index_changes(fx: &Fixture, n: usize) {
    for _ in 0..n {
        fx.emit_common(&[("index", F)]);
        fx.settle();
    }
}

#[test]
fn filter_reuse_after_the_load_a_batch_started() {
    let fx = Fixture::new();
    let _o = fx.opened();
    // A `.gitignore` edit and its batch-mates start a load that reads the
    // worktree after all of them: the next index change reuses that load.
    fx.emit_root(&[
        (".gitignore", F | MODIFIED),
        ("a.txt", F | MODIFIED),
        ("moved", D | Flags::RENAMED),
    ]);
    fx.settle();
    index_changes(&fx, 1);
    assert_eq!(fx.source.previous(), [None, None, Some(2)]);
}

#[test]
fn filter_reuse_not_after_a_rule_file_the_old_filter_dropped() {
    let fx = Fixture::new();
    fx.source.ignore(&["out"]);
    let _o = fx.opened();
    // `out` taken out of `.gitignore`: a load reading the worktree starts.
    fx.source.unignore(&["out"]);
    fx.emit_root(&[(".gitignore", F | MODIFIED)]);
    pump(&fx);
    // After that load listed `out/`, a new `out/.gitignore`: the filter in
    // place still ignores `out/`, so its event drops without a trigger.
    fx.emit_root(&[("out/.gitignore", F | Flags::CREATED)]);
    pump(&fx);
    fx.settle();
    assert_eq!(fx.source.filter_calls(), 2, "no reload for it");
    // So the next load reads the worktree again; the one after reuses.
    index_changes(&fx, 2);
    assert_eq!(fx.source.previous(), [None, None, None, Some(3)]);
}

#[test]
fn filter_reuse_not_after_events_during_a_load_over_a_failed_filter() {
    let fx = Fixture::new();
    let _o = fx.opened();
    fx.source.fail_filter(Some("index is corrupt"));
    index_changes(&fx, 1);
    // Over a failed filter only `.gitignore` paths trigger: a directory
    // moved in during the next load triggers nothing.
    fx.source.fail_filter(None);
    fx.emit_common(&[("index", F)]);
    pump(&fx);
    fx.emit_root(&[("vendored", D | Flags::RENAMED)]);
    pump(&fx);
    fx.settle();
    assert_eq!(fx.source.filter_calls(), 3, "no reload for it");
    index_changes(&fx, 2);
    assert_eq!(fx.source.previous(), [None, Some(1), None, None, Some(4)]);
}

#[test]
fn filter_reuse_not_after_an_overflow_before_the_first_filter() {
    let fx = Fixture::new();
    let (repo, _repo_sink) = fx.repo();
    let (_worktree, _sink) = fx.worktree(&repo);
    pump(&fx);
    // Before the first filter: more paths than the stash holds, a new
    // `.gitignore` among them; merged into directories, its trigger is lost.
    let paths: Vec<String> = (0..5000)
        .map(|i| format!("gen/d{:02}/f{i}", i % 50))
        .chain(["gen/d00/.gitignore".to_owned()])
        .collect();
    for chunk in paths.chunks(1000) {
        let events: Vec<(&str, Flags)> = chunk.iter().map(|path| (path.as_str(), F)).collect();
        fx.emit_root(&events);
        pump(&fx);
    }
    fx.settle();
    let calls = fx.source.filter_calls();
    index_changes(&fx, 2);
    let previous = fx.source.previous();
    assert_eq!(previous[calls..], [None, Some(calls + 1)], "{previous:?}");
}

#[test]
fn filter_reuse_not_after_a_failed_load() {
    let fx = Fixture::new();
    let _o = fx.opened();
    fx.source.fail_filter(Some("index is corrupt"));
    fx.emit_common(&[("index", F)]);
    fx.settle();
    fx.source.fail_filter(None);
    fx.emit_common(&[("index", F)]);
    fx.settle();
    assert_eq!(fx.source.previous(), [None, Some(1), None]);
}
