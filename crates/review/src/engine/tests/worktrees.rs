//! The parallel worktree overview (docs/design/review.md「测试」
//! `worktrees_*`): the uncommitted, committed and against-base counts of
//! every worktree, the current worktree's use of the overview's count set,
//! the overlaps, the caps, the throttle, the retention after the last
//! interest, and what marks a count stale.

use std::path::PathBuf;
use std::time::Duration;

use lectern_fs_watch::{RepoBatch, WorktreeMeta, WorktreeName};
use lectern_git::Mode;
use lectern_services::project::{ChangedParts, ProjectKey, WorktreeKey};
use lectern_services::review::{
    ChangeCount, OutsideCount, OverviewInterest, ReviewEvent, WorktreeChanges, WorktreesOverview,
};
use lectern_services::session::SessionsChanged;

use super::*;
use crate::engine::RETAIN;

/// A linked worktree of the fixture.
pub(crate) struct Linked {
    pub(crate) key: WorktreeKey,
    pub(crate) root: PathBuf,
}

/// The repository of most tests: `main` (the main worktree, at the base,
/// with `c.txt` edited), `feat` (two commits' worth of changes against the
/// base: `a.txt` edited and `d.txt` added; edited `b.txt` and untracked
/// `e.txt` in the worktree) and `other` (at the base, `a.txt` edited).
pub(crate) struct Trio {
    pub(crate) fx: Fx,
    pub(crate) feat: Linked,
    pub(crate) other: Linked,
    pub(crate) c1: Oid,
    pub(crate) c2: Oid,
}

pub(super) const BASE: [(&str, &str); 3] =
    [("a.txt", "one\n"), ("b.txt", "bee\n"), ("c.txt", "sea\n")];
const FEAT: [(&str, &str); 4] = [
    ("a.txt", "one\nfeat\n"),
    ("b.txt", "bee\n"),
    ("c.txt", "sea\n"),
    ("d.txt", "dee\n"),
];

pub(super) fn put(root: &std::path::Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }
}

/// A linked worktree on `branch` (at `commit` when given, else unborn),
/// with `index` staged and `files` written.
pub(super) fn linked(
    fx: &Fx,
    name: &str,
    branch: &str,
    commit: Option<Oid>,
    index: &[(&str, &str)],
    files: &[(&str, &str)],
) -> Linked {
    let root = fx.dir.path().join(format!("wt-{name}"));
    let admin = fx.repo.add_worktree(name, &root, TestHead::Branch(branch));
    if let Some(commit) = commit {
        fx.repo.set_ref(branch, commit);
    }
    let entries: Vec<IndexEntry> = index
        .iter()
        .map(|(p, c)| IndexEntry::new(p, Mode::File, fx.repo.blob(c.as_bytes())))
        .collect();
    fx.repo.write_index(&admin, &entries);
    let root = std::fs::canonicalize(root).unwrap();
    put(&root, files);
    let key = fx.projects.add_worktree(&admin, &root);
    Linked { key, root }
}

pub(crate) fn trio() -> Trio {
    let fx = Fx::new();
    let c1 = fx.commit(&BASE, None, T - 1000);
    fx.head("refs/heads/main", c1, &BASE);
    let c2 = fx.commit(&FEAT, Some(c1), T - 500);
    put(fx.repo.root(), &BASE);
    fx.write("c.txt", "sea\nshore\n");
    let mut feat_files = FEAT.to_vec();
    feat_files[1] = ("b.txt", "bee\nlocal\n");
    feat_files.push(("e.txt", "eel\n"));
    let feat = linked(&fx, "feat", "refs/heads/feat", Some(c2), &FEAT, &feat_files);
    let mut other_files = BASE.to_vec();
    other_files[0] = ("a.txt", "one\nother\n");
    let other = linked(
        &fx,
        "other",
        "refs/heads/other",
        Some(c1),
        &BASE,
        &other_files,
    );
    Trio {
        fx,
        feat,
        other,
        c1,
        c2,
    }
}

impl Trio {
    fn driven(&self) -> Driven {
        self.fx.driven(vec![self.fx.sessions(Vec::new())])
    }

    fn timed(&self, sessions: Vec<Arc<FakeSessions>>) -> Driven {
        Driven::with_timers(
            Arc::clone(&self.fx.projects),
            sessions,
            self.fx.dir.path(),
            NOW,
        )
    }

    fn main(&self) -> WorktreeKey {
        self.fx.projects.main()
    }
}

fn row<'a>(o: &'a WorktreesOverview, key: &WorktreeKey) -> &'a WorktreeChanges {
    o.worktrees
        .iter()
        .find(|w| w.worktree == *key)
        .unwrap_or_else(|| panic!("{key:?} in {o:#?}"))
}

fn counts(w: &WorktreeChanges) -> (ChangeCount, ChangeCount, ChangeCount) {
    (w.uncommitted, w.committed, w.vs_base)
}

fn files(n: u32) -> ChangeCount {
    ChangeCount::Files(n)
}

fn overlap_paths(w: &WorktreeChanges) -> Vec<(WorktreeKey, u32, Vec<String>)> {
    w.overlaps
        .as_ref()
        .expect("a known path set")
        .iter()
        .map(|o| {
            (
                o.other.clone(),
                o.files,
                o.paths.iter().map(|p| p.to_string()).collect(),
            )
        })
        .collect()
}

fn events(d: &Driven) -> Vec<u64> {
    d.take_events()
        .into_iter()
        .filter_map(|(e, _)| match e {
            ReviewEvent::Worktrees { generation, .. } => Some(generation),
            _ => None,
        })
        .collect()
}

/// Watches the project and runs the first rounds.
fn watch(d: &Driven) -> (OverviewInterest, Arc<WorktreesOverview>) {
    let project = d.project();
    let interest = d.watch_worktrees(&project);
    d.run();
    let o = d.worktrees(&project).expect("counts after the first round");
    (interest, o)
}

fn repo_batch() -> RepoBatch {
    RepoBatch {
        worktrees: Vec::new(),
        worktree_list: false,
        refs: false,
        config: false,
        reflog: Vec::new(),
        reflog_resets: Vec::new(),
        rescan: false,
        health: None,
    }
}

/// A batch in which the index of the linked worktree `name` changed.
fn index_batch(name: &str) -> RepoBatch {
    RepoBatch {
        worktrees: vec![(
            WorktreeName::Linked(name.into()),
            WorktreeMeta {
                head: false,
                index: true,
            },
        )],
        ..repo_batch()
    }
}

#[test]
fn worktrees_count_uncommitted_and_committed_of_each_worktree() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    assert!(
        d.worktrees(&project).is_none(),
        "nothing before an interest"
    );
    let (_interest, o) = watch(&d);
    assert_eq!(o.base.as_deref(), Some("refs/heads/main"));
    let keys: Vec<_> = o.worktrees.iter().map(|w| w.worktree.clone()).collect();
    assert_eq!(keys, [t.main(), t.feat.key.clone(), t.other.key.clone()]);

    let main = row(&o, &t.main());
    assert_eq!(counts(main), (files(1), files(0), files(1)));
    assert_eq!(main.head, oid(t.c1));
    assert!(main.counted_at.is_some());
    let feat = row(&o, &t.feat.key);
    assert_eq!(counts(feat), (files(2), files(2), files(4)));
    assert_eq!(feat.head, oid(t.c2));
    let other = row(&o, &t.other.key);
    assert_eq!(counts(other), (files(1), files(0), files(1)));

    // Each worktree's result was published when it was done: three
    // generations, the current worktree first.
    assert_eq!(events(&d), [1, 2, 3]);
    assert_eq!(o.generation, 3);
}

#[test]
fn worktrees_overlap_in_both_directions_with_one_sample_list() {
    let t = trio();
    let d = t.driven();
    let (_interest, o) = watch(&d);
    let feat = row(&o, &t.feat.key);
    let other = row(&o, &t.other.key);
    assert_eq!(
        overlap_paths(feat),
        [(t.other.key.clone(), 1, vec!["a.txt".to_owned()])]
    );
    assert_eq!(
        overlap_paths(other),
        [(t.feat.key.clone(), 1, vec!["a.txt".to_owned()])]
    );
    let (a, b) = (
        &feat.overlaps.as_ref().unwrap()[0],
        &other.overlaps.as_ref().unwrap()[0],
    );
    assert!(Arc::ptr_eq(&a.paths, &b.paths), "one Arc for both");
    // Nothing in common with the main worktree: a known, empty list.
    assert_eq!(overlap_paths(row(&o, &t.main())), []);
}

#[test]
fn worktrees_overlaps_are_ordered_by_files_and_capped_at_eight_items() {
    let t = trio();
    // Eleven more worktrees editing `a.txt`; `late0` also edits `d.txt`
    // like `feat` does (two files in common with it).
    for i in 0..11 {
        let name = format!("many{i}");
        let mut files = BASE.to_vec();
        files[0] = ("a.txt", "one\nmany\n");
        linked(
            &t.fx,
            &name,
            &format!("refs/heads/{name}"),
            Some(t.c1),
            &BASE,
            &files,
        );
    }
    let d = t.driven();
    let (_interest, o) = watch(&d);
    let feat = row(&o, &t.feat.key);
    let list = feat.overlaps.as_ref().unwrap();
    assert_eq!(list.len(), 8, "at most eight items");
    assert!(list.iter().all(|o| o.files == 1));
    // Equal counts: by the other worktree's key.
    let keys: Vec<_> = list.iter().map(|o| o.other.clone()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn worktrees_current_uses_the_overview_count_set() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    // The overview compares the main worktree first…
    let _overview = d.watch_overview(&project);
    d.run();
    // …and a file changes after, which nothing reports.
    t.fx.write("b.txt", "bee\nsting\n");
    let (_interest, o) = watch(&d);
    let main = row(&o, &t.main());
    assert_eq!(
        main.uncommitted,
        files(1),
        "the count set of the overview, not another comparison"
    );
    assert_eq!(main.committed, files(0));
    // Another worktree is compared here.
    assert_eq!(row(&o, &t.feat.key).uncommitted, files(2));

    // The overview publishes a new count set: the count follows at once,
    // with no throttle in between.
    d.overview_refresh(&project);
    d.run();
    let o = d.worktrees(&project).unwrap();
    assert_eq!(row(&o, &t.main()).uncommitted, files(2));
    assert!(o.generation > 3);
}

#[test]
fn worktrees_current_waits_for_the_overview_first_round() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    let _overview = d.watch_overview(&project);
    // Both interests before anything ran: one comparison of the current
    // worktree, the overview's.
    let _interest = d.watch_worktrees(&project);
    d.run();
    let o = d.worktrees(&project).unwrap();
    assert_eq!(row(&o, &t.main()).uncommitted, files(1));
    assert_eq!(row(&o, &t.main()).vs_base, files(1));
}

#[test]
fn worktrees_current_is_compared_again_when_the_overview_interest_goes() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    let (_interest, _) = watch(&d);
    // The overview was held (and may not have published) and is released:
    // the current worktree's count no longer waits for it.
    t.fx.write("b.txt", "bee\nsting\n");
    drop(d.watch_overview(&project));
    d.set_now(Timestamp(NOW.0 + 60_000));
    d.run();
    let o = d.worktrees(&project).unwrap();
    assert_eq!(row(&o, &t.main()).uncommitted, files(2));
}

#[test]
fn worktrees_overview_capped_makes_the_current_worktree_capped() {
    let fx = Fx::new();
    let c1 = fx.commit(&BASE, None, T - 1000);
    fx.head("refs/heads/main", c1, &BASE);
    put(fx.repo.root(), &BASE);
    for i in 0..4001 {
        fx.write(&format!("gen/d{:03}/f{i}.txt", i % 100), &format!("{i}\n"));
    }
    let d = fx.driven(vec![fx.sessions(Vec::new())]);
    let project = d.project();
    let _overview = d.watch_overview(&project);
    d.run();
    assert_eq!(
        d.overview(&project).unwrap().outside,
        OutsideCount::Capped(4001)
    );
    let (_interest, o) = watch(&d);
    let main = row(&o, &fx.projects.main());
    assert_eq!(
        counts(main),
        (ChangeCount::Capped, files(0), ChangeCount::Capped)
    );
    assert!(main.overlaps.is_none());
}

#[test]
fn worktrees_capped_worktree_has_no_overlap_and_is_in_no_list() {
    let t = trio();
    for i in 0..4001 {
        let file = t.other.root.join(format!("gen/d{:03}/f{i}.txt", i % 100));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, format!("{i}\n")).unwrap();
    }
    let d = t.driven();
    let (_interest, o) = watch(&d);
    let other = row(&o, &t.other.key);
    assert_eq!(other.uncommitted, ChangeCount::Capped);
    assert_eq!(other.vs_base, ChangeCount::Capped);
    assert!(other.overlaps.is_none(), "its path set is not known");
    // `feat` and `other` both edit `a.txt`, but `other` is not in `feat`'s
    // list.
    assert_eq!(overlap_paths(row(&o, &t.feat.key)), []);
}

#[test]
fn worktrees_without_a_default_branch_use_the_uncommitted_paths() {
    let fx = Fx::new();
    let c1 = fx.commit(&BASE, None, T - 1000);
    // A branch that is neither `main` nor `master`, and no remote.
    fx.head("refs/heads/trunk", c1, &BASE);
    put(fx.repo.root(), &BASE);
    fx.write("a.txt", "one\nmain\n");
    let other = linked(
        &fx,
        "other",
        "refs/heads/other",
        Some(c1),
        &BASE,
        &[
            ("a.txt", "one\nother\n"),
            ("b.txt", "bee\n"),
            ("c.txt", "sea\n"),
        ],
    );
    let d = fx.driven(vec![fx.sessions(Vec::new())]);
    let (_interest, o) = watch(&d);
    assert_eq!(o.base, None);
    let main = row(&o, &fx.projects.main());
    assert_eq!(
        counts(main),
        (files(1), ChangeCount::Unavailable, ChangeCount::Unavailable)
    );
    assert_eq!(
        overlap_paths(main),
        [(other.key.clone(), 1, vec!["a.txt".to_owned()])]
    );
}

#[test]
fn worktrees_unborn_head_has_no_commit_counts() {
    let t = trio();
    let fresh = linked(
        &t.fx,
        "fresh",
        "refs/heads/fresh",
        None,
        &[],
        &[("x.txt", "ex\n")],
    );
    let d = t.driven();
    let (_interest, o) = watch(&d);
    let w = row(&o, &fresh.key);
    assert_eq!(w.head, None);
    assert_eq!(w.uncommitted, files(1));
    assert_eq!(w.committed, ChangeCount::Unavailable);
    assert_eq!(w.vs_base, ChangeCount::Unavailable);
    assert_eq!(overlap_paths(w), []);
}

#[test]
fn worktrees_a_worktree_that_cannot_be_opened_is_unavailable() {
    let t = trio();
    std::fs::remove_dir_all(&t.other.root).unwrap();
    let d = t.driven();
    let (_interest, o) = watch(&d);
    let other = row(&o, &t.other.key);
    assert_eq!(
        counts(other),
        (
            ChangeCount::Unavailable,
            ChangeCount::Unavailable,
            ChangeCount::Unavailable
        )
    );
    assert!(other.overlaps.is_none());
    // The rest is counted.
    assert_eq!(counts(row(&o, &t.feat.key)), (files(2), files(2), files(4)));
}

#[test]
fn worktrees_cache_the_commit_pair_in_review_tree_changes() {
    let t = trio();
    let d = t.driven();
    let (_interest, _o) = watch(&d);
    let record = crate::cutoff::Record {
        time: 0,
        old: Some(t.c1),
        new: t.c2,
    };
    let cached = d
        .engine
        .cfg
        .cache
        .repo(t.fx.repo.common_dir())
        .get::<crate::cutoff::CachedTreeChanges>(&crate::cutoff::key(t.fx.repo.hash(), &record));
    assert!(
        cached.is_some(),
        "the pair (merge base, HEAD) of `feat` is in the cache"
    );
}

#[test]
fn worktrees_keep_no_handle_of_a_worktree_nobody_reviews() {
    let t = trio();
    let d = t.driven();
    let (_interest, _o) = watch(&d);
    assert_eq!(d.engine.gits.lock().unwrap().len(), 0, "handles dropped");
}

#[test]
fn worktrees_folder_project_has_no_worktrees() {
    let t = trio();
    let d = t.driven();
    let folder = ProjectKey::Folder {
        root: Arc::from(t.fx.dir.path()),
    };
    let interest = d.watch_worktrees(&folder);
    assert_eq!(d.run(), 0, "no task");
    assert!(d.worktrees(&folder).is_none());
    drop(interest);
}

#[test]
fn worktrees_nothing_is_compared_without_an_interest() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    d.refresh_worktrees(&project);
    let mut batch = index_batch("feat");
    batch.refs = true;
    d.worktrees_batch(&project, &batch);
    d.project_changed(
        &project,
        ChangedParts {
            worktrees: true,
            active: true,
            ..ChangedParts::default()
        },
    );
    assert_eq!(d.run(), 0, "no state, no task");
    assert!(d.worktrees(&project).is_none());
    assert!(!d.has_worktrees_state(&project));
}

#[test]
fn worktrees_stay_after_the_last_interest_until_the_project_closes() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    let (interest, before) = watch(&d);
    drop(interest);
    // The result is kept; events only mark.
    let kept = d.worktrees(&project).expect("kept");
    assert!(Arc::ptr_eq(&kept, &before));
    t.fx.write("c.txt", "sea\nmore\n");
    d.worktrees_batch(&project, &index_batch("feat"));
    d.refresh_worktrees(&project);
    d.sessions_changed(&SessionsChanged {
        source: SourceId(SOURCE.into()),
        generation: 2,
        changed: Arc::from(vec![key("s")]),
        removed: Arc::from(Vec::new()),
    });
    assert_eq!(d.run(), 0, "nothing is compared while nobody watches");
    assert!(Arc::ptr_eq(&d.worktrees(&project).unwrap(), &before));

    d.engine.project_closed(&project);
    d.run();
    assert!(d.worktrees(&project).is_none());
    assert!(!d.has_worktrees_state(&project));
}

#[test]
fn worktrees_watching_again_compares_only_the_marked_and_the_stale() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    let (interest, _) = watch(&d);
    drop(interest);
    // `feat` was changed and marked, `other` changed with no sign.
    put(&t.feat.root, &[("f.txt", "eff\n")]);
    put(&t.other.root, &[("g.txt", "gee\n")]);
    d.worktrees_batch(&project, &index_batch("feat"));
    d.set_now(Timestamp(NOW.0 + 60_000));
    let _interest = d.watch_worktrees(&project);
    d.run();
    let o = d.worktrees(&project).unwrap();
    assert_eq!(row(&o, &t.feat.key).uncommitted, files(3), "marked");
    assert_eq!(
        row(&o, &t.other.key).uncommitted,
        files(1),
        "no file watch, no mark, not yet five minutes"
    );

    // Five minutes on, a count that old is made again.
    drop(_interest);
    d.set_now(Timestamp(NOW.0 + 6 * 60_000));
    let _interest = d.watch_worktrees(&project);
    d.run();
    let o = d.worktrees(&project).unwrap();
    assert_eq!(row(&o, &t.other.key).uncommitted, files(2));
}

#[test]
fn worktrees_release_stops_a_round_that_has_not_started() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    let interest = d.watch_worktrees(&project);
    drop(interest);
    d.run();
    assert!(d.worktrees(&project).is_none(), "cancelled before it began");
    assert!(events(&d).is_empty());
    // Watched again, it computes.
    let (_interest, o) = watch(&d);
    assert_eq!(o.worktrees.len(), 3);
}

#[test]
fn worktrees_throttle_merges_triggers_into_one_comparison() {
    let t = trio();
    let d = t.timed(vec![t.fx.sessions(Vec::new())]);
    let project = d.project();
    let (_interest, first) = watch(&d);
    assert_eq!(first.generation, 3);
    assert_eq!(events(&d).len(), 3);

    // Two triggers within the gap: nothing is compared yet.
    put(&t.feat.root, &[("f.txt", "eff\n")]);
    d.worktrees_batch(&project, &index_batch("feat"));
    d.run();
    put(&t.feat.root, &[("g.txt", "gee\n")]);
    d.worktrees_batch(&project, &index_batch("feat"));
    d.run();
    assert_eq!(d.worktrees(&project).unwrap().generation, 3);
    assert_eq!(
        row(&d.worktrees(&project).unwrap(), &t.feat.key).uncommitted,
        files(2)
    );

    // When the gap has passed, one comparison sees both.
    d.set_now(Timestamp(NOW.0 + 60_000));
    d.advance(Duration::from_secs(60));
    d.run();
    let o = d.worktrees(&project).unwrap();
    assert_eq!(o.generation, 4, "one publication");
    assert_eq!(row(&o, &t.feat.key).uncommitted, files(4));
}

#[test]
fn worktrees_refresh_marks_all_and_follows_the_throttle() {
    let t = trio();
    let d = t.timed(vec![t.fx.sessions(Vec::new())]);
    let project = d.project();
    let (_interest, _) = watch(&d);
    put(&t.other.root, &[("g.txt", "gee\n")]);
    d.refresh_worktrees(&project);
    d.run();
    assert_eq!(
        row(&d.worktrees(&project).unwrap(), &t.other.key).uncommitted,
        files(1),
        "within the gap"
    );
    d.set_now(Timestamp(NOW.0 + 60_000));
    d.advance(Duration::from_secs(60));
    d.run();
    assert_eq!(
        row(&d.worktrees(&project).unwrap(), &t.other.key).uncommitted,
        files(2)
    );
}

#[test]
fn worktrees_the_gap_is_ten_times_the_last_comparison() {
    use crate::engine::worktrees::gap_ms;
    assert_eq!(gap_ms(Duration::from_millis(5)), 2000);
    assert_eq!(gap_ms(Duration::from_millis(200)), 2000);
    assert_eq!(gap_ms(Duration::from_millis(1000)), 10_000);
}

#[test]
fn worktrees_a_moved_default_branch_makes_the_commit_counts_again() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    let (_interest, before) = watch(&d);
    assert_eq!(
        counts(row(&before, &t.feat.key)),
        (files(2), files(2), files(4))
    );

    // `main` moves to `feat`'s commit, as a fast-forward merge does.
    t.fx.repo.set_ref("refs/heads/main", t.c2);
    let mut batch = repo_batch();
    batch.refs = true;
    d.worktrees_batch(&project, &batch);
    d.set_now(Timestamp(NOW.0 + 60_000));
    d.run();
    let o = d.worktrees(&project).unwrap();
    // `feat` is at the base now; its two uncommitted files stay.
    assert_eq!(counts(row(&o, &t.feat.key)), (files(2), files(0), files(2)));
    // `other` is behind the base: its merge base is its HEAD.
    assert_eq!(row(&o, &t.other.key).committed, files(0));
    // `main` moved with no HEAD file change: it is found through the
    // reference.
    assert_eq!(row(&o, &t.main()).head, oid(t.c2));
    assert_eq!(row(&o, &t.main()).committed, files(0));
    // The overlap on `a.txt` went with `feat`'s commit.
    assert_eq!(overlap_paths(row(&o, &t.feat.key)), []);
}

#[test]
fn worktrees_a_head_change_and_a_rescan_make_counts_stale() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    let (_interest, _) = watch(&d);

    // `other` is checked out to `feat`'s commit: its HEAD changed.
    t.fx.repo.set_ref("refs/heads/other", t.c2);
    let mut batch = repo_batch();
    batch.worktrees = vec![(
        WorktreeName::Linked("other".into()),
        WorktreeMeta {
            head: true,
            index: false,
        },
    )];
    d.worktrees_batch(&project, &batch);
    d.set_now(Timestamp(NOW.0 + 60_000));
    d.run();
    let o = d.worktrees(&project).unwrap();
    assert_eq!(row(&o, &t.other.key).head, oid(t.c2));
    assert_eq!(row(&o, &t.other.key).committed, files(2));
    assert_eq!(row(&o, &t.feat.key).uncommitted, files(2), "not marked");

    // A rescan (events were lost): everything is made again.
    put(&t.feat.root, &[("f.txt", "eff\n")]);
    put(t.fx.repo.root(), &[("z.txt", "zed\n")]);
    d.set_now(Timestamp(NOW.0 + 120_000));
    let mut batch = repo_batch();
    batch.rescan = true;
    d.worktrees_batch(&project, &batch);
    d.run();
    let o = d.worktrees(&project).unwrap();
    assert_eq!(row(&o, &t.feat.key).uncommitted, files(3));
    assert_eq!(row(&o, &t.main()).uncommitted, files(2));
}

#[test]
fn worktrees_a_ref_change_that_moves_nothing_publishes_nothing() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    let (_interest, before) = watch(&d);
    let mut batch = repo_batch();
    batch.refs = true;
    d.worktrees_batch(&project, &batch);
    d.set_now(Timestamp(NOW.0 + 60_000));
    d.run();
    let after = d.worktrees(&project).unwrap();
    assert_eq!(after.generation, before.generation);
    assert!(events(&d).len() == 3, "only the first round's events");
}

#[test]
fn worktrees_a_session_marks_the_uncommitted_count_of_its_worktree() {
    let t = trio();
    let feat_root = t.feat.root.to_string_lossy().into_owned();
    let sessions = t.fx.sessions(vec![session(
        "s1",
        &feat_root,
        ms(T),
        vec![turn("t1", ms(T + 10), Vec::new())],
    )]);
    let d = t.fx.driven(vec![sessions]);
    let project = d.project();
    let (_interest, _) = watch(&d);
    put(&t.feat.root, &[("f.txt", "eff\n")]);
    put(t.fx.repo.root(), &[("z.txt", "zed\n")]);
    d.set_now(Timestamp(NOW.0 + 60_000));
    d.sessions_changed(&SessionsChanged {
        source: SourceId(SOURCE.into()),
        generation: 2,
        changed: Arc::from(vec![key("s1")]),
        removed: Arc::from(Vec::new()),
    });
    d.run();
    let o = d.worktrees(&project).unwrap();
    assert_eq!(row(&o, &t.feat.key).uncommitted, files(3), "the session's");
    assert_eq!(row(&o, &t.main()).uncommitted, files(1), "not marked");
}

#[test]
fn worktrees_a_new_worktree_is_counted_and_listed() {
    let t = trio();
    let d = t.driven();
    let project = d.project();
    let (_interest, _) = watch(&d);
    let late = linked(&t.fx, "late", "refs/heads/late", Some(t.c1), &BASE, &BASE);
    d.project_changed(
        &project,
        ChangedParts {
            worktrees: true,
            ..ChangedParts::default()
        },
    );
    d.run();
    let o = d.worktrees(&project).unwrap();
    assert_eq!(o.worktrees.len(), 4);
    assert_eq!(counts(row(&o, &late.key)), (files(0), files(0), files(0)));
    assert!(row(&o, &late.key).counted_at.is_some());
}

#[test]
fn worktrees_hashes_go_after_the_retention_and_the_overlaps_stay() {
    let t = trio();
    let d = t.timed(vec![t.fx.sessions(Vec::new())]);
    let project = d.project();
    d.set_worktrees_hash_limit(1);
    let (interest, _) = watch(&d);
    assert!(d.worktrees_hash_count(&project) > 1);
    drop(interest);
    assert!(d.worktrees_hash_count(&project) > 1, "kept for the 30 s");
    d.advance(RETAIN - Duration::from_secs(1));
    d.run();
    assert!(d.worktrees_hash_count(&project) > 1);
    d.advance(Duration::from_secs(1));
    d.run();
    assert_eq!(d.worktrees_hash_count(&project), 0);
    let kept = d.worktrees(&project).unwrap();
    assert_eq!(
        overlap_paths(row(&kept, &t.feat.key)),
        [(t.other.key.clone(), 1, vec!["a.txt".to_owned()])],
        "counts and overlaps stay"
    );

    // Watched again, the worktrees without hashes are compared again, and
    // the overlaps are the same.
    d.set_now(Timestamp(NOW.0 + 60_000));
    let _interest = d.watch_worktrees(&project);
    d.run();
    assert!(d.worktrees_hash_count(&project) > 1);
    let o = d.worktrees(&project).unwrap();
    assert_eq!(
        overlap_paths(row(&o, &t.feat.key)),
        [(t.other.key.clone(), 1, vec!["a.txt".to_owned()])]
    );
}

#[test]
fn worktrees_hashes_are_kept_when_watched_again_within_the_retention() {
    let t = trio();
    let d = t.timed(vec![t.fx.sessions(Vec::new())]);
    let project = d.project();
    d.set_worktrees_hash_limit(1);
    let (interest, _) = watch(&d);
    drop(interest);
    d.advance(Duration::from_secs(10));
    let _interest = d.watch_worktrees(&project);
    d.run();
    d.advance(Duration::from_secs(60));
    d.run();
    assert!(
        d.worktrees_hash_count(&project) > 1,
        "the timer found a new watch"
    );
}

#[test]
fn worktrees_hash_is_the_same_for_the_same_path() {
    use crate::engine::worktrees::hash_path;
    assert_eq!(hash_path(b"src/a.rs"), hash_path(b"src/a.rs"));
    assert_ne!(hash_path(b"src/a.rs"), hash_path(b"src/b.rs"));
}
