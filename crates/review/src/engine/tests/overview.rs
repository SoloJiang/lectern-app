//! The project overview (docs/design/review.md「测试」`overview_*`): the
//! sessions with their claims and progress, the changes outside any
//! session, the zero-retention limits, the 4000-path cap, at most eight
//! other-base comparisons per round, the release, the merged triggers,
//! and that a round never calls `turns`.

use std::time::Duration;

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
