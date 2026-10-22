//! `folder_*`, `rows_other_*` and the windows of the snapshots
//! (docs/design/usage.md「项目标识」「取整与时间窗口」): in step 1 every
//! turn belongs to its cwd's place.

use std::path::Path;

use lectern_services::session::{Agent, Record};
use lectern_services::usage::{
    ProjectRef, Tokens, UsageProjectId, UsageQuery, UsageSnapshot, UsageState, Window,
};

use super::support::*;
use crate::testing::{FakeSessions, TableZone};

/// One usage record: model and tokens.
type Use<'a> = (&'a str, [u64; 4]);

/// One session in `cwd` with one usage record per entry of `usages`.
fn session(fake: &FakeSessions, id: &str, cwd: &str, at: i64, usages: &[Use<'_>]) {
    fake.add(id, Path::new(cwd), at, None);
    let turn_id = format!("{id}-t");
    let mut records = vec![context(0, at, cwd), turn(50, &turn_id, at)];
    for (i, (model, t)) in usages.iter().enumerate() {
        let (pos, at) = (100 * (i as u64 + 1), at + i as i64);
        let message = format!("{id}-{i}");
        let mut r = claude(pos, at, Some(&turn_id), &message, *t);
        if let Record::Usage(u) = &mut r.value {
            u.model = Some((*model).into());
        }
        records.push(r);
    }
    fake.push(id, records);
}

/// One session with one record of `input` tokens of model-a.
fn one(fake: &FakeSessions, id: &str, cwd: &str, at: i64, input: u64) {
    session(fake, id, cwd, at, &[("model-a", [input, 0, 0, 0])]);
}

/// The projects' rows and 「其他」 of an all-projects snapshot.
fn rows(all: &lectern_services::usage::AllUsage) -> Vec<Tokens> {
    let mut out: Vec<Tokens> = all.projects.iter().map(|p| p.1).collect();
    out.push(all.other);
    out
}

#[test]
fn folder_has_no_rows_but_models_and_agents() {
    let mut h = Harness::new();
    let notes = h.projects.folder(Path::new("/work/notes"));
    h.open_project(&notes);
    let claude = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    let at = OCT_20 + 9 * HOUR;
    let two = [("model-a", [10, 1, 0, 0]), ("model-b", [20, 2, 0, 0])];
    session(&claude, "a", "/work/notes/sub", at, &two);
    let codex = FakeSessions::new("session-codex", Agent::Codex);
    let one = [("model-c", [5, 5, 5, 0])];
    session(&codex, "b", "/work/notes", at + DAY, &one);
    h.read_every(&claude);
    h.read_every(&codex);
    let p = h.project(&notes, Window::D7);
    assert!(p.project.folder);
    assert!(p.rows.is_empty());
    assert_eq!(p.by_model.len(), 3);
    assert_eq!(p.by_agent.len(), 2);
    assert_adds_up(&p.total, p.by_model.iter().map(|m| m.2));
    assert_adds_up(&p.total, p.by_agent.iter().map(|a| a.1));
    insta::assert_snapshot!(render_project(&p));
}

#[test]
fn folder_claims_earlier_usage_when_opened() {
    let mut h = Harness::new();
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    let at = OCT_20 + 9 * HOUR;
    one(&fake, "deep", "/work/notes/sub", at, 10);
    one(&fake, "top", "/work/scratch", at, 100);
    // Read before any folder is open: both are places in no project.
    h.read_every(&fake);
    let all = h.all(Window::D7);
    assert!(all.projects.is_empty());
    assert_eq!(all.other.input, 110);

    // Opening the folders later claims what lies under their roots; the
    // deepest root wins.
    let work = h.projects.folder(Path::new("/work"));
    let notes = h.projects.folder(Path::new("/work/notes"));
    h.open_project(&work);
    h.open_project(&notes);
    let all = h.all(Window::D7);
    assert_eq!(all.other.input, 0);
    assert_adds_up(&all.total, rows(&all));
    assert_eq!(h.project(&notes, Window::D7).total.input, 10);
    assert_eq!(h.project(&work, Window::D7).total.input, 100);
    insta::assert_snapshot!(render_all(&all));
}

#[test]
fn rows_other_holds_unopened_repositories_and_stray_places() {
    let mut h = Harness::new();
    h.open_repo("/work/a");
    let b_root = Path::new("/work/b");
    let b = h.projects.repo(b_root, &b_root.join(".git"));
    h.projects.missing(Path::new("/gone"));
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    let at = OCT_20 + 9 * HOUR;
    one(&fake, "in-a", "/work/a/src", at, 1);
    one(&fake, "in-b", "/work/b", at + DAY, 10);
    one(&fake, "scratch", "/tmp/scratch", at, 100);
    one(&fake, "gone", "/gone/dir", at, 1000);
    h.read_every(&fake);
    let all = h.all(Window::D7);
    assert_eq!(all.projects.len(), 1);
    assert_eq!(all.projects[0].1.input, 1);
    assert_eq!(all.other.input, 1110);
    assert_adds_up(&all.total, rows(&all));
    let daily: u64 = all.daily.iter().flat_map(|d| d.iter()).map(|t| t.1).sum();
    assert_eq!(daily, all.total.total());
    insta::assert_snapshot!(render_all(&all));

    // The never-opened repository is still a project of its own, reachable
    // by ID; opening it moves its usage out of 「其他」.
    let row = h.store().project_by_key(&b).unwrap().expect("its project");
    assert!(!row.opened);
    let query = UsageQuery::Project {
        project: ProjectRef::Id(row.pid),
        window: Window::D7,
    };
    let Some(UsageSnapshot::Project(p)) = h.snapshot(&query) else {
        panic!("no snapshot");
    };
    assert_eq!(p.total.input, 10);
    h.open_project(&b);
    let all = h.all(Window::D7);
    assert_eq!(all.other.input, 1100);
    assert_eq!(all.projects.len(), 2);
}

#[test]
fn rows_other_unknown_project_reference_has_no_snapshot() {
    let h = Harness::new();
    let query = UsageQuery::Project {
        project: ProjectRef::Id(UsageProjectId([7; 16])),
        window: Window::D7,
    };
    assert!(h.snapshot(&query).is_none());
}

#[test]
fn window_deltas_give_what_a_full_compute_gives() {
    let mut h = Harness::new();
    let repo = h.open_repo("/work/a");
    let notes = h.projects.folder(Path::new("/work/notes"));
    h.open_project(&notes);
    let queries = [
        UsageQuery::Project {
            project: ProjectRef::Open(repo.clone()),
            window: Window::D7,
        },
        UsageQuery::Project {
            project: ProjectRef::Open(notes.clone()),
            window: Window::D30,
        },
        UsageQuery::AllProjects {
            window: Window::D14,
        },
    ];
    let claude = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    let codex = FakeSessions::new("session-codex", Agent::Codex);
    let at = OCT_20 + 9 * HOUR;
    session(&claude, "a", "/work/a", at, &[("model-a", [10, 1, 0, 0])]);
    h.read_every(&claude);
    let mut watched: Vec<_> = queries.iter().map(|q| h.watched(q)).collect();
    // Each step stores batches; the deltas of each are applied to the
    // accumulators, and the snapshot must equal a full compute.
    let mut recomputed = 0;
    let removed = std::cell::Cell::new(0);
    let mut check =
        |h: &Harness, watched: &mut Vec<crate::snapshot::Watched>, out: &crate::store::Ingested| {
            removed.set(removed.get() + out.facts.iter().filter(|d| d.rows < 0).count());
            for (w, q) in watched.iter_mut().zip(&queries) {
                if h.apply(w, out) == crate::snapshot::Applied::Recompute {
                    recomputed += 1;
                    *w = h.watched(q);
                }
                let full = h.snapshot(q).unwrap();
                assert_eq!(w.snapshot(UsageState::Ready), full, "{q:?}");
            }
            assert_buckets(h);
        };
    let mut read = |h: &Harness, fake: &FakeSessions, id: &str, watched: &mut Vec<_>| loop {
        let out = h.read_once(fake, id, 300);
        check(h, watched, &out);
        if out.at_end {
            break;
        }
    };
    // New records in batches: a new model, a new agent, an old record
    // outside the windows.
    let more = [("model-b", [20, 2, 3, 4]), ("model-a", [1, 1, 1, 1])];
    session(&claude, "b", "/work/a/sub", at + DAY, &more);
    read(&h, &claude, "b", &mut watched);
    session(&codex, "c", "/work/a", at, &[("model-c", [5, 5, 5, 0])]);
    read(&h, &codex, "c", &mut watched);
    session(
        &claude,
        "old",
        "/work/a",
        at - 20 * DAY,
        &[("model-a", [7, 0, 0, 0])],
    );
    read(&h, &claude, "old", &mut watched);
    // The same keys again, larger in another session: the counted source
    // moves (a fact deleted, one inserted).
    claude.add("b2", Path::new("/work/a"), at + DAY, None);
    let mut dup = claude_records("b", "/work/a", at + DAY, &more);
    for r in &mut dup {
        if let Record::Usage(u) = &mut r.value
            && let lectern_services::session::RawTokens::Anthropic { input, .. } = &mut u.tokens
        {
            *input += 100;
        }
    }
    claude.push("b2", dup);
    read(&h, &claude, "b2", &mut watched);
    // A replay of the first session.
    claude.replace(
        "a",
        claude_records("a", "/work/a", at, &[("model-a", [10, 1, 0, 0])]),
    );
    read(&h, &claude, "a", &mut watched);
    // A directory first seen under the open folder: a new location the
    // folder claims, so its windows are computed again.
    session(
        &claude,
        "n",
        "/work/notes/new",
        at,
        &[("model-a", [3, 0, 0, 0])],
    );
    read(&h, &claude, "n", &mut watched);
    assert!(recomputed > 0, "the new directory needs a full compute");
    assert!(removed.get() > 0, "some deltas take facts away");
}

/// The records of [`session`], to push again or replace with.
fn claude_records(
    id: &str,
    cwd: &str,
    at: i64,
    usages: &[Use<'_>],
) -> Vec<lectern_services::session::Positioned<Record>> {
    let fake = FakeSessions::new("scratch", Agent::ClaudeCode);
    session(&fake, id, cwd, at, usages);
    let key = fake.key(id);
    lectern_services::session::SessionSource::read(
        &fake,
        &key,
        &lectern_services::session::Cursor::START,
        u64::MAX,
    )
    .unwrap()
    .records
}

#[test]
fn window_record_belongs_to_its_local_day() {
    let mut h = Harness::new();
    h.zone = TableZone::fixed(8 * 3600);
    let repo = h.open_repo("/work/a");
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    // 23:59 and 00:01 at +08:00 around the local midnight of 2026-10-21.
    let midnight = OCT_20 + 16 * HOUR;
    let before = [("model-a", [1, 0, 0, 0]), ("model-a", [0, 0, 0, 0])];
    session(&fake, "s", "/work/a", midnight - 60_000, &before);
    let late = claude(1000, midnight + 60_000, None, "late", [2, 0, 0, 0]);
    fake.push("s", vec![late]);
    // Eight local days back: outside the 7-day window, inside the 14-day one.
    let old = [("model-a", [40, 0, 0, 0])];
    session(&fake, "old", "/work/a", midnight - 7 * DAY, &old);
    h.read_every(&fake);
    let p = h.project(&repo, Window::D7);
    assert_eq!(p.total.input, 3);
    let days: Vec<(String, u64)> = p
        .days
        .iter()
        .zip(p.daily.iter())
        .map(|(d, t)| (date(*d), t.iter().map(|(_, n)| *n).sum()))
        .filter(|(_, n)| *n > 0)
        .collect();
    let expected = [("2026-10-20".to_owned(), 1), ("2026-10-21".to_owned(), 2)];
    assert_eq!(days, expected);
    assert_eq!(h.project(&repo, Window::D14).total.input, 43);
}
