//! `folder_*`, `rows_other_*` and the windows of the snapshots
//! (docs/design/usage.md「项目标识」「取整与时间窗口」): in step 1 every
//! turn belongs to its cwd's place.

use std::path::Path;

use lectern_services::session::{Agent, Record};
use lectern_services::usage::{
    ProjectRef, Tokens, UsageProjectId, UsageQuery, UsageSnapshot, Window,
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
