//! The project overview (docs/design/review.md「测试」`overview_*`): the
//! sessions with their claims and progress, the changes outside any
//! session, the zero-retention limits, the 4000-path cap, at most eight
//! other-base comparisons per round, the release, the merged triggers,
//! and that a round never calls `turns`.

use std::time::Duration;

use lectern_services::project::ChangedParts;
use lectern_services::review::{OutsideCount, Overview, OverviewInterest, Progress, ReviewEvent};

use super::*;
use crate::engine::overview::MERGE_WINDOW;

fn overview_events(d: &Driven) -> usize {
    d.take_events()
        .iter()
        .filter(|(e, _)| matches!(e, ReviewEvent::Overview { .. }))
        .count()
}

/// Watches the project and runs the first round.
fn watch(d: &Driven) -> (OverviewInterest, Arc<Overview>) {
    let project = d.project();
    let interest = d.watch_overview(&project);
    d.run();
    let overview = d
        .overview(&project)
        .expect("an overview after the first round");
    (interest, overview)
}

/// `a.txt`, `b.txt` and `c.txt` committed; the session `s1` edits
/// `a.txt`; the worktree has that edit and one of `c.txt` by nobody.
fn fixture() -> (Fx, Arc<FakeSessions>) {
    let fx = Fx::new();
    let files = [("a.txt", "one\n"), ("b.txt", "bee\n"), ("c.txt", "sea\n")];
    let c1 = fx.commit(&files, None, T - 1000);
    fx.head("refs/heads/main", c1, &files);
    fx.reflog(&[(None, c1, T - 1000, "commit (initial): one")]);
    fx.write("a.txt", "one\ntwo\n");
    fx.write("b.txt", "bee\n");
    fx.write("c.txt", "sea\nshore\n");
    let sessions = fx.sessions(vec![session(
        "s1",
        &fx.root(),
        ms(T),
        vec![turn(
            "t1",
            ms(T + 10),
            vec![edit(
                "c1",
                ms(T + 20),
                vec![replace("a.txt", "one\n", "one\ntwo\n")],
            )],
        )],
    )]);
    (fx, sessions)
}

#[test]
fn overview_lists_the_sessions_with_claims_and_counts_the_outside() {
    let (fx, sessions) = fixture();
    let d = fx.driven(vec![sessions]);
    let (_interest, o) = watch(&d);
    // One round for the interest, one when the annotation log was read.
    assert_eq!(o.generation, 2);
    assert_eq!(overview_events(&d), 2);
    assert_eq!(o.outside, OutsideCount::Files(1), "{o:#?}");
    assert_eq!(o.sessions.len(), 1, "{o:#?}");
    let s = &o.sessions[0];
    assert_eq!(s.session, key("s1"));
    assert_eq!(
        s.progress,
        Some(Progress {
            total: 1,
            seen: 0,
            changed_since_seen: 0
        })
    );
    assert_eq!(s.critical, 0);
    // No review computed the chain yet: the cache has nothing.
    assert_eq!(s.drifted, None);

    // After a review attributed the file, the cached chain answers.
    let (_, snapshot) = open(&d, target("s1"));
    assert_eq!(snapshot.files.len(), 1);
    d.overview_refresh(&d.project());
    d.run();
    let o = d.overview(&d.project()).unwrap();
    assert_eq!(o.sessions[0].drifted, Some(false), "{o:#?}");
}

#[test]
fn overview_uses_zero_retention_limits() {
    let (fx, sessions) = fixture();
    let d = fx.driven(vec![sessions]);
    let (_interest, _) = watch(&d);
    let set = d
        .overview_count(&d.project())
        .expect("the count set is kept");
    assert_eq!(set.files().len(), 2);
    assert!(
        set.files()
            .iter()
            .all(|c| matches!(c.detail, lectern_git::Detail::Listed)),
        "every file listed, no patch"
    );
}

#[test]
fn overview_capped() {
    let fx = Fx::new();
    for i in 0..4001 {
        fx.write(&format!("gen/d{:03}/f{i}.txt", i % 100), &format!("{i}\n"));
    }
    let sessions = fx.sessions(Vec::new());
    let d = fx.driven(vec![sessions]);
    let (_interest, o) = watch(&d);
    assert_eq!(o.outside, OutsideCount::Capped(4001));
    assert!(o.sessions.is_empty());
    assert!(
        d.overview_count(&d.project()).is_none(),
        "the set is not kept"
    );
}

#[test]
fn overview_never_calls_turns() {
    let fx = Fx::new();
    let names: Vec<String> = (0..20).map(|i| format!("f{i}.txt")).collect();
    let files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "x\n")).collect();
    let c1 = fx.commit(&files, None, T - 1000);
    fx.head("refs/heads/main", c1, &files);
    fx.reflog(&[(None, c1, T - 1000, "commit (initial): x")]);
    let mut scripts = Vec::new();
    for (i, name) in names.iter().enumerate() {
        fx.write(name, "x\ny\n");
        scripts.push(session(
            &format!("s{i}"),
            &fx.root(),
            ms(T + i as i64),
            vec![turn(
                "t",
                ms(T + 10),
                vec![edit("c", ms(T + 20), vec![replace(name, "x\n", "x\ny\n")])],
            )],
        ));
    }
    let sessions = fx.sessions(scripts);
    let d = fx.driven(vec![Arc::clone(&sessions)]);
    let (_interest, o) = watch(&d);
    assert_eq!(o.sessions.len(), 20, "{o:#?}");
    assert_eq!(o.outside, OutsideCount::Files(0));
    for i in 0..20 {
        let calls = sessions.calls(&format!("s{i}"));
        assert_eq!(calls.turns, 0, "s{i}: {calls:?}");
        assert_eq!(calls.turns_unretained, 1, "s{i}: {calls:?}");
    }
    // The next round finds the claims in memory: no index read at all.
    sessions.reset_calls();
    d.overview_refresh(&d.project());
    d.run();
    assert_eq!(sessions.calls("s0").turns_unretained, 0);
}

#[test]
fn overview_compares_at_most_eight_other_bases_per_round() {
    let fx = Fx::new();
    let names: Vec<String> = (0..10).map(|i| format!("f{i}.txt")).collect();
    let mut files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "x\n")).collect();
    let c0 = fx.commit(&files, None, T - 5000);
    files.push(("README", "next\n"));
    let c1 = fx.commit(&files, Some(c0), T - 100);
    fx.head("refs/heads/main", c1, &files);
    fx.reflog(&[
        (None, c0, T - 5000, "commit (initial): x"),
        (Some(c0), c1, T - 100, "commit: next"),
    ]);
    let mut scripts = Vec::new();
    for (i, name) in names.iter().enumerate() {
        fx.write(name, "x\ny\n");
        // Started before `next`: the base is `c0`, not HEAD.
        scripts.push(session(
            &format!("s{i}"),
            &fx.root(),
            ms(T - 4000 + i as i64),
            vec![turn(
                "t",
                ms(T - 3990),
                vec![edit(
                    "c",
                    ms(T - 3980),
                    vec![replace(name, "x\n", "x\ny\n")],
                )],
            )],
        ));
    }
    let sessions = fx.sessions(scripts);
    let d = fx.driven(vec![sessions]);
    let (_interest, o) = watch(&d);
    assert_eq!(o.sessions.len(), 10, "{o:#?}");
    let with_progress = o.sessions.iter().filter(|s| s.progress.is_some()).count();
    assert_eq!(with_progress, 8, "{o:#?}");
    // The newest eight.
    for s in &o.sessions[..8] {
        assert_eq!(s.progress.map(|p| p.total), Some(1), "{s:?}");
    }
    assert!(o.sessions[8..].iter().all(|s| s.progress.is_none()));
}

#[test]
fn overview_progress_matches_review_after_agent_commit() {
    let fx = Fx::new();
    let c1 = fx.commit(&[("a.txt", "one\n"), ("b.txt", "bee\n")], None, T - 1000);
    // The agent edits both, then commits `a.txt`: HEAD passes its base.
    let c2 = fx.commit(
        &[("a.txt", "one\ntwo\n"), ("b.txt", "bee\n")],
        Some(c1),
        T + 100,
    );
    fx.head(
        "refs/heads/main",
        c2,
        &[("a.txt", "one\ntwo\n"), ("b.txt", "bee\n")],
    );
    fx.reflog(&[
        (None, c1, T - 1000, "commit (initial): one"),
        (Some(c1), c2, T + 100, "commit: a"),
    ]);
    fx.write("a.txt", "one\ntwo\n");
    fx.write("b.txt", "bee\nhive\n");
    let sessions = fx.sessions(vec![session(
        "s",
        &fx.root(),
        ms(T),
        vec![turn(
            "t1",
            ms(T + 10),
            vec![edit(
                "c1",
                ms(T + 20),
                vec![
                    replace("a.txt", "one\n", "one\ntwo\n"),
                    replace("b.txt", "bee\n", "bee\nhive\n"),
                ],
            )],
        )],
    )]);
    let d = fx.driven(vec![sessions]);
    let (_, review) = open(&d, target("s"));
    assert_eq!(review.files.len(), 2, "{:?}", paths(&review));
    let (_interest, o) = watch(&d);
    assert_eq!(o.sessions.len(), 1, "{o:#?}");
    let progress = o.sessions[0].progress.expect("compared against its base");
    assert_eq!(progress.total as usize, review.files.len());
    // Against HEAD only `b.txt` changed, and the session claims it.
    assert_eq!(o.outside, OutsideCount::Files(0));
}

#[test]
fn overview_is_released_with_the_last_interest() {
    let (fx, sessions) = fixture();
    let d = fx.driven(vec![sessions]);
    let project = d.project();
    let (first, _) = watch(&d);
    let second = d.watch_overview(&project);
    drop(first);
    d.run();
    assert!(d.overview(&project).is_some());
    drop(second);
    assert!(d.overview(&project).is_none());
    // The state is dropped on a task, not on the caller's thread.
    d.run();
    assert!(!d.has_overview_state(&project));
    assert!(d.overview_count(&project).is_none());
}

#[test]
fn overview_triggers_merge_within_two_seconds() {
    let (fx, sessions) = fixture();
    let d = Driven::with_timers(
        Arc::clone(&fx.projects),
        vec![Arc::clone(&sessions)],
        fx.dir.path(),
        NOW,
    );
    let project = d.project();
    let (_interest, o) = watch(&d);
    let generation = o.generation;
    let _ = overview_events(&d);
    // Three triggers within the window: no round before it ends.
    d.overview_refresh(&project);
    d.overview_refresh(&project);
    d.sessions_changed(&lectern_services::session::SessionsChanged {
        source: lectern_services::session::SourceId(SOURCE.into()),
        generation: 2,
        changed: Arc::from(vec![key("s1")]),
        removed: Arc::from(Vec::new()),
    });
    d.run();
    assert_eq!(overview_events(&d), 0);
    d.advance(MERGE_WINDOW - Duration::from_millis(1));
    d.run();
    assert_eq!(overview_events(&d), 0);
    // One round for all three.
    d.advance(Duration::from_millis(1));
    d.run();
    assert_eq!(overview_events(&d), 1);
    assert_eq!(d.overview(&project).unwrap().generation, generation + 1);
}

#[test]
fn overview_mark_seen_updates_the_progress() {
    let (fx, sessions) = fixture();
    let d = fx.driven(vec![sessions]);
    let project = d.project();
    let (_interest, o) = watch(&d);
    assert_eq!(o.sessions[0].progress.map(|p| p.seen), Some(0));
    let (id, snapshot) = open(&d, target("s1"));
    d.command(id, ReviewCommand::MarkSeen(snapshot.files[0].key.clone()));
    d.run();
    let o = d.overview(&project).unwrap();
    assert_eq!(o.sessions[0].progress.map(|p| p.seen), Some(1), "{o:#?}");
}

#[test]
fn overview_without_an_active_worktree_is_pending() {
    let (fx, sessions) = fixture();
    fx.projects.set_active(None);
    let d = fx.driven(vec![sessions]);
    let (_interest, o) = watch(&d);
    assert_eq!(o.outside, OutsideCount::Pending);
    assert!(o.sessions.is_empty());
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// `committed` at HEAD, then each of `changed` rewritten in the worktree
/// (a change against HEAD) with the modification time given in
/// milliseconds; `deleted` removed. No session.
fn packages_fixture(committed: &[&str], changed: &[(&str, i64)], deleted: &[&str]) -> Fx {
    let fx = Fx::new();
    let files: Vec<(&str, &str)> = committed.iter().map(|p| (*p, "old\n")).collect();
    let c1 = fx.commit(&files, None, T - 1000);
    fx.head("refs/heads/main", c1, &files);
    fx.reflog(&[(None, c1, T - 1000, "commit (initial): old")]);
    for (path, _) in &files {
        fx.write(path, "old\n");
    }
    for (path, at) in changed {
        fx.write(path, "newer content\n");
        fx.mtime(path, *at);
    }
    for path in deleted {
        std::fs::remove_file(fx.repo.root().join(path)).unwrap();
    }
    fx
}

fn packages_of(o: &Overview) -> Vec<(&str, u32)> {
    o.packages
        .as_deref()
        .expect("per-package counts")
        .iter()
        .map(|(dir, n)| (&**dir, *n))
        .collect()
}

#[test]
fn overview_packages_assign_by_longest_prefix() {
    let now = NOW.0;
    let fx = packages_fixture(
        &[
            "crates/a/x.txt",
            "crates/a/y.txt",
            "crates/a/sub/z.txt",
            "crates/b/w.txt",
            "crates/ab/v.txt",
            "crates/c/gone.txt",
            "docs/n.txt",
            "top.txt",
        ],
        &[
            ("crates/a/x.txt", now - 1000),
            ("crates/a/y.txt", now - 2000),
            ("crates/a/sub/z.txt", now - 3000),
            ("crates/b/w.txt", now - 4000),
            ("crates/ab/v.txt", now - 5000),
            ("docs/n.txt", now - 6000),
            ("top.txt", now - 7000),
        ],
        &["crates/c/gone.txt"],
    );
    fx.projects
        .set_packages(Some(&["crates/a", "crates/a/sub", "crates/b", "crates/c"]));
    let d = fx.driven(vec![fx.sessions(Vec::new())]);
    let (_interest, o) = watch(&d);
    // The nested package takes its file from the outer one; `crates/ab`
    // is not under `crates/a`; a deleted file has no time; the files in
    // no package are not counted; the packages without a file are not
    // listed, sorted by `dir`.
    assert_eq!(
        packages_of(&o),
        [("crates/a", 2), ("crates/a/sub", 1), ("crates/b", 1)],
        "{o:#?}"
    );
    assert_eq!(o.outside, OutsideCount::Files(8));
}

#[test]
fn overview_packages_the_root_package_holds_the_rest() {
    let now = NOW.0;
    let fx = packages_fixture(
        &["crates/a/x.txt", "docs/n.txt", "top.txt"],
        &[
            ("crates/a/x.txt", now - 1000),
            ("docs/n.txt", now - 2000),
            ("top.txt", now - 3000),
        ],
        &[],
    );
    fx.projects.set_packages(Some(&["", "crates/a"]));
    let d = fx.driven(vec![fx.sessions(Vec::new())]);
    let (_interest, o) = watch(&d);
    assert_eq!(packages_of(&o), [("", 2), ("crates/a", 1)], "{o:#?}");
}

#[test]
fn overview_packages_count_only_the_local_day_of_the_modification_time() {
    // UTC+8: the local day of `NOW` began 16:00 UTC the day before.
    let offset = 8 * 3600;
    let midnight = (NOW.0 + offset * 1000).div_euclid(DAY_MS) * DAY_MS - offset * 1000;
    let fx = packages_fixture(
        &["p/a.txt", "p/b.txt", "p/c.txt", "p/d.txt", "p/e.txt"],
        &[
            ("p/a.txt", midnight - 1),
            ("p/b.txt", midnight),
            ("p/c.txt", NOW.0),
            ("p/d.txt", midnight + DAY_MS - 1),
            ("p/e.txt", midnight + DAY_MS),
        ],
        &[],
    );
    fx.projects.set_packages(Some(&["p"]));
    let d = fx.driven(vec![fx.sessions(Vec::new())]);
    d.set_utc_offset(offset as i32);
    let (_interest, o) = watch(&d);
    // The last millisecond of yesterday and the first of tomorrow are out.
    assert_eq!(packages_of(&o), [("p", 3)], "{o:#?}");

    // The same boundary in UTC: the day began at 00:00 UTC.
    let utc_midnight = NOW.0.div_euclid(DAY_MS) * DAY_MS;
    let fx = packages_fixture(
        &["p/a.txt", "p/b.txt"],
        &[("p/a.txt", utc_midnight - 1), ("p/b.txt", utc_midnight)],
        &[],
    );
    fx.projects.set_packages(Some(&["p"]));
    let d = fx.driven(vec![fx.sessions(Vec::new())]);
    let (_interest, o) = watch(&d);
    assert_eq!(packages_of(&o), [("p", 1)], "{o:#?}");
}

#[test]
fn overview_packages_is_empty_when_nothing_changed_today() {
    let fx = packages_fixture(&["p/a.txt"], &[("p/a.txt", NOW.0 - 2 * DAY_MS)], &[]);
    fx.projects.set_packages(Some(&["p"]));
    let d = fx.driven(vec![fx.sessions(Vec::new())]);
    let (_interest, o) = watch(&d);
    assert_eq!(o.packages.as_deref(), Some(&[][..]), "{o:#?}");
    assert_eq!(o.outside, OutsideCount::Files(1));
}

#[test]
fn overview_packages_none_when_capped() {
    let fx = Fx::new();
    for i in 0..4001 {
        fx.write(&format!("gen/d{:03}/f{i}.txt", i % 100), &format!("{i}\n"));
    }
    fx.projects.set_packages(Some(&["gen"]));
    let d = fx.driven(vec![fx.sessions(Vec::new())]);
    let (_interest, o) = watch(&d);
    assert_eq!(o.outside, OutsideCount::Capped(4001));
    assert_eq!(o.packages, None);
}

#[test]
fn overview_packages_none_while_packages_are_pending() {
    let fx = packages_fixture(&["p/a.txt"], &[("p/a.txt", NOW.0)], &[]);
    let d = fx.driven(vec![fx.sessions(Vec::new())]);
    let (_interest, o) = watch(&d);
    assert_eq!(o.outside, OutsideCount::Files(1));
    assert_eq!(o.packages, None);
}

#[test]
fn overview_packages_follow_the_projects_packages() {
    let fx = packages_fixture(&["p/a.txt"], &[("p/a.txt", NOW.0)], &[]);
    let d = fx.driven(vec![fx.sessions(Vec::new())]);
    let (_interest, first) = watch(&d);
    assert_eq!(first.packages, None);

    // The scan finishes: a `Changed` with only `packages` makes a round
    // that reuses the count set.
    fx.projects.set_packages(Some(&["p"]));
    let project = d.project();
    d.project_changed(
        &project,
        ChangedParts {
            packages: true,
            ..ChangedParts::default()
        },
    );
    d.run();
    let o = d.overview(&project).unwrap();
    assert_eq!(packages_of(&o), [("p", 1)], "{o:#?}");
    assert!(o.generation > first.generation);

    // Other parts ask for nothing.
    overview_events(&d);
    d.project_changed(
        &project,
        ChangedParts {
            files: true,
            ..ChangedParts::default()
        },
    );
    d.run();
    assert_eq!(overview_events(&d), 0);
}
