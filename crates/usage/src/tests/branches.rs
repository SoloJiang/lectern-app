//! `segment_*`, `repo_*`, `observe_*`, `order_*`, `multi_switch_*`,
//! `branch_by_change_*`, `enter_*`, `follow_switched_*`, `cwd_branch_*`,
//! `imported_*` and `rebuild_*` (docs/design/usage.md「分支观察」「分摊」,
//! step 5): which branch a turn's tokens go to, from the reflog of the
//! repository and what the session did.
//!
//! The reflog records are built by hand, with the times `fs_watch` would
//! give them; nothing reads a repository (the reads are a table,
//! `testing::FakeGit`).

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use lectern_fs_watch::{
    Origin, ReadStamp, ReflogFile, ReflogRecord, ReflogReset, RepoBatch, ResetReason, RunId,
    WorktreeName,
};
use lectern_services::project::Head;
use lectern_services::usage::{RowKey, SessionUsage, UsageQuery, UsageSnapshot, WorktreeUsage};

use super::attribution::{AT, Log, conn, session_usage, settled, source};
use super::support::*;
use crate::gitread::{BranchLog, WorktreeHead};
use crate::store::Arm;
use crate::testing::FakeSessions;

const SEC: i64 = 1_000;

/// A 40-digit object ID.
pub(super) fn oid(n: u64) -> String {
    format!("{n:040x}")
}

/// A repository under test: its common directory, and the numbers of the
/// records written to its reflog (`seq` of the records, the offsets).
pub(super) struct Repo {
    pub(super) common: PathBuf,
    seq: Cell<u64>,
}

fn stamp(ms: i64) -> ReadStamp {
    ReadStamp {
        run: RunId(7),
        read: u64::try_from(ms).unwrap_or(0),
        at: UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).unwrap_or(0)),
    }
}

impl Repo {
    pub(super) fn new(common: &str) -> Repo {
        Repo {
            common: PathBuf::from(common),
            seq: Cell::new(0),
        }
    }

    fn next(&self) -> u64 {
        let n = self.seq.get();
        self.seq.set(n + 1);
        n
    }

    /// A record of `file` written at `written` (milliseconds) and found by a
    /// read that ended `lag` milliseconds later; the previous read of the
    /// file began at `after` when given.
    #[allow(clippy::too_many_arguments, reason = "a test fixture: one value each")]
    pub(super) fn record(
        &self,
        file: ReflogFile,
        old: Option<u64>,
        new: u64,
        written: i64,
        lag: i64,
        after: Option<i64>,
        message: &str,
        origin: Origin,
    ) -> ReflogRecord {
        let old = old.map_or("0".repeat(40), oid);
        self.record_hex(file, &old, &oid(new), written, lag, after, message, origin)
    }

    /// The same with the object IDs as 40 hexadecimal digits.
    #[allow(clippy::too_many_arguments, reason = "a test fixture: one value each")]
    pub(super) fn record_hex(
        &self,
        file: ReflogFile,
        old: &str,
        new: &str,
        written: i64,
        lag: i64,
        after: Option<i64>,
        message: &str,
        origin: Origin,
    ) -> ReflogRecord {
        let line = format!(
            "{old} {new} Dev <dev@example.org> {} +0000\t{message}",
            written.div_euclid(1000)
        );
        let seq = self.next();
        ReflogRecord {
            file,
            offset: seq * 100,
            line: Arc::from(line.into_bytes()),
            seq,
            observed: stamp(written + lag),
            after: after.map(stamp),
            origin,
        }
    }

    /// `checkout: moving from <from> to <to>` in the HEAD of a worktree.
    pub(super) fn switch(
        &self,
        wt: &WorktreeName,
        from: &str,
        to: &str,
        written: i64,
    ) -> ReflogRecord {
        // A commit named by its abbreviation is the commit HEAD was or is at.
        let hex = |name: &str, default: u64| {
            if name.len() >= 7 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
                format!("{name:0<40}")
            } else {
                oid(default)
            }
        };
        self.record_hex(
            ReflogFile::Head(wt.clone()),
            &hex(from, 1),
            &hex(to, 2),
            written,
            100,
            None,
            &format!("checkout: moving from {from} to {to}"),
            Origin::Appended,
        )
    }

    /// A commit in the HEAD of a worktree: `old` to `new`.
    pub(super) fn commit(
        &self,
        wt: &WorktreeName,
        old: u64,
        new: u64,
        written: i64,
    ) -> ReflogRecord {
        self.record(
            ReflogFile::Head(wt.clone()),
            Some(old),
            new,
            written,
            100,
            None,
            "commit: work",
            Origin::Appended,
        )
    }

    /// The creation line of a branch's reflog.
    pub(super) fn created(&self, name: &str, new: u64, written: i64) -> ReflogRecord {
        self.record(
            ReflogFile::Branch(Arc::from(name.as_bytes())),
            None,
            new,
            written,
            100,
            None,
            "branch: Created from HEAD",
            Origin::NewFile,
        )
    }

    /// A removed reflog file.
    pub(super) fn removed(&self, name: &str, observed: i64) -> ReflogReset {
        ReflogReset {
            file: ReflogFile::Branch(Arc::from(name.as_bytes())),
            reason: ResetReason::Removed,
            seq: self.next(),
            observed: stamp(observed),
        }
    }

    /// A HEAD reflog that was rewritten (`reflog expire`).
    pub(super) fn rewritten(&self, wt: &WorktreeName, observed: i64) -> ReflogReset {
        ReflogReset {
            file: ReflogFile::Head(wt.clone()),
            reason: ResetReason::Rewritten,
            seq: self.next(),
            observed: stamp(observed),
        }
    }

    /// A reflog file that could not be read.
    pub(super) fn unreadable(&self, wt: &WorktreeName, observed: i64) -> ReflogReset {
        ReflogReset {
            file: ReflogFile::Head(wt.clone()),
            reason: ResetReason::Unreadable("denied".into()),
            seq: self.next(),
            observed: stamp(observed),
        }
    }
}

pub(super) fn batch(records: Vec<ReflogRecord>, resets: Vec<ReflogReset>) -> RepoBatch {
    RepoBatch {
        worktrees: Vec::new(),
        worktree_list: false,
        refs: false,
        config: false,
        reflog: records,
        reflog_resets: resets,
        rescan: false,
        health: None,
    }
}

impl Harness {
    /// The watch of a repository begins at `at` (milliseconds) with the
    /// HEADs `heads` (a worktree's key and its branch).
    pub(super) fn arm(&self, repo: &Repo, at: i64, heads: &[(&str, Option<&str>)]) {
        let heads: Vec<WorktreeHead> = heads
            .iter()
            .map(|(key, branch)| WorktreeHead {
                key: PathBuf::from(key),
                branch: branch.map(str::to_owned),
            })
            .collect();
        let logs: Vec<(String, BranchLog)> = heads
            .iter()
            .filter_map(|h| h.branch.clone())
            .filter_map(|b| {
                crate::gitread::GitRead::branch_log(&self.git, &repo.common, &b).map(|l| (b, l))
            })
            .collect();
        self.store()
            .arm(&Arm {
                common_dir: &repo.common,
                at_ns: at * 1_000_000,
                heads: &heads,
                logs: &logs,
            })
            .unwrap();
    }

    /// Records the repository's reflog delivered.
    pub(super) fn reflog(&self, repo: &Repo, records: Vec<ReflogRecord>, resets: Vec<ReflogReset>) {
        self.store()
            .repo_batch(&repo.common, &batch(records, resets))
            .unwrap();
    }
}

fn main() -> WorktreeName {
    WorktreeName::Main
}

/// A repository `/work/app` with its main worktree watched from a minute
/// before the turns of the tests, HEAD on `branch`.
pub(super) fn watched(branch: &str) -> (Harness, Repo) {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    let repo = Repo::new("/work/app/.git");
    h.git.set_default(&repo.common, "refs/heads/main");
    h.arm(&repo, AT - 60 * SEC, &[("/work/app/.git", Some(branch))]);
    (h, repo)
}

/// The branch rows of a session: (name, the session's part of the input).
fn rows(s: &SessionUsage) -> Vec<(String, u64)> {
    s.rows
        .iter()
        .map(|r| match &r.key {
            RowKey::Branch { branch, .. } => (branch.name.to_string(), r.session_part.input),
            RowKey::Unassigned { .. } => ("(unassigned)".to_owned(), r.session_part.input),
            RowKey::Mrs { .. } => ("(mrs)".to_owned(), r.session_part.input),
        })
        .collect()
}

fn branch_rows(h: &Harness, fake: &FakeSessions) -> Vec<(String, u64)> {
    rows(&session_usage(h, fake, "s"))
}

// ---- segment ----

/// (name, kind, begin reason, begin time, end reason, end time) of each
/// segment, by the newest name.
type SegmentRow = (String, i64, i64, Option<i64>, Option<i64>, Option<i64>);

fn segments(h: &Harness) -> Vec<SegmentRow> {
    let conn = conn(h);
    let mut stmt = conn
        .prepare(
            "SELECT coalesce((SELECT name FROM segment_name n WHERE n.segment = s.id
                              ORDER BY coalesce(n.at, -9223372036854775808) DESC, n.id DESC LIMIT 1), ''),
                    kind, begin_reason, begin_at, end_reason, end_at
             FROM segment s ORDER BY s.id",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn segment_a_branch_the_watch_found_is_open_to_the_past() {
    let (h, _) = watched("refs/heads/feature");
    assert_eq!(
        segments(&h),
        [("refs/heads/feature".to_owned(), 0, 2, None, None, None)]
    );
}

#[test]
fn segment_the_log_of_a_branch_gives_its_creation() {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    let repo = Repo::new("/work/app/.git");
    h.git.set_log(
        &repo.common,
        "refs/heads/feature",
        Some(AT - 3600 * SEC),
        &[],
    );
    h.arm(&repo, AT, &[("/work/app/.git", Some("refs/heads/feature"))]);
    assert_eq!(
        segments(&h),
        [(
            "refs/heads/feature".to_owned(),
            0,
            1,
            Some(AT - 3600 * SEC),
            None,
            None
        )]
    );
}

#[test]
fn segment_a_creation_record_after_the_first_sight_is_the_same_segment() {
    let (h, repo) = watched("refs/heads/main");
    // `git switch -c topic`: HEAD first, then the new branch's log.
    let wt = main();
    h.reflog(
        &repo,
        vec![
            repo.switch(&wt, "main", "topic", AT),
            repo.created("refs/heads/topic", 2, AT),
        ],
        vec![],
    );
    let found = segments(&h);
    assert_eq!(found.len(), 2, "main and topic: {found:?}");
    assert_eq!(found[1].0, "refs/heads/topic");
    assert_eq!(found[1].2, 1, "created");
    assert_eq!(found[1].3, Some(AT / SEC * SEC));
}

#[test]
fn segment_a_rename_keeps_the_segment_and_its_names() {
    let (h, repo) = watched("refs/heads/old");
    let renamed = repo.record(
        ReflogFile::Branch(Arc::from(b"refs/heads/new".as_slice())),
        Some(2),
        2,
        AT,
        100,
        None,
        "Branch: renamed refs/heads/old to refs/heads/new",
        Origin::NewFile,
    );
    // The lines the new file carried over from the old one are not a creation.
    let carried = repo.created("refs/heads/new", 1, AT - 3600 * SEC);
    h.reflog(
        &repo,
        vec![carried, renamed],
        vec![repo.removed("refs/heads/old", AT + 100)],
    );
    let found = segments(&h);
    assert_eq!(found.len(), 1, "one segment: {found:?}");
    assert_eq!(found[0].0, "refs/heads/new");
    assert_eq!(found[0].4, None, "the old name's removal is not an end");
    let conn = conn(&h);
    let names: i64 = conn
        .query_row("SELECT count(*) FROM segment_name", [], |r| r.get(0))
        .unwrap();
    assert_eq!(names, 2);
}

#[test]
fn segment_a_removed_log_ends_it_and_a_new_branch_of_the_name_is_another() {
    let (h, repo) = watched("refs/heads/main");
    let wt = main();
    h.reflog(
        &repo,
        vec![
            repo.switch(&wt, "main", "topic", AT),
            repo.created("refs/heads/topic", 2, AT),
        ],
        vec![],
    );
    h.reflog(
        &repo,
        vec![],
        vec![repo.removed("refs/heads/topic", AT + 10 * SEC)],
    );
    h.reflog(
        &repo,
        vec![repo.created("refs/heads/topic", 5, AT + 20 * SEC)],
        vec![],
    );
    let found = segments(&h);
    let topics: Vec<_> = found.iter().filter(|s| s.0 == "refs/heads/topic").collect();
    assert_eq!(topics.len(), 2, "{found:?}");
    assert_eq!(topics[0].4, Some(1), "removed");
    assert_eq!(topics[1].2, 1, "created again");
}

#[test]
fn segment_a_detached_head_has_a_segment_until_a_branch_is_checked_out() {
    let (h, repo) = watched("refs/heads/main");
    let wt = main();
    h.reflog(&repo, vec![repo.switch(&wt, "main", "1234567", AT)], vec![]);
    let found = segments(&h);
    assert!(found.iter().any(|s| s.1 == 1 && s.4.is_none()), "{found:?}");
    h.reflog(
        &repo,
        vec![repo.switch(&wt, "1234567", "main", AT + SEC)],
        vec![],
    );
    let found = segments(&h);
    assert!(
        found.iter().any(|s| s.1 == 1 && s.4 == Some(3)),
        "{found:?}"
    );
}

// ---- repo (the records and spans) ----

#[test]
fn repo_records_are_stored_parsed_without_the_line() {
    let (h, repo) = watched("refs/heads/main");
    let wt = main();
    h.reflog(
        &repo,
        vec![
            repo.commit(&wt, 1, 2, AT),
            repo.switch(&wt, "main", "topic", AT + SEC),
        ],
        vec![],
    );
    let conn = conn(&h);
    let (n, acts): (i64, String) = conn
        .query_row(
            "SELECT count(*), group_concat(act) FROM (SELECT act FROM reflog ORDER BY seq)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(acts, "3,1");
    // Nothing of the line (the committer, the message) is in the file.
    let text: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE name = 'reflog'
                            AND sql LIKE '%line%')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!text);
}

#[test]
fn repo_a_span_ends_when_the_watch_does_and_stale_ones_end_at_the_last_record() {
    let (h, repo) = watched("refs/heads/main");
    h.reflog(&repo, vec![repo.commit(&main(), 1, 2, AT)], vec![]);
    // The run ended without closing the span.
    assert_eq!(h.store().close_stale_spans().unwrap(), 1);
    let conn = conn(&h);
    let end: i64 = conn
        .query_row("SELECT end_ns FROM watch_span", [], |r| r.get(0))
        .unwrap();
    assert_eq!(end, (AT + 100) * 1_000_000);
    drop(conn);
    // Watched until the last record, not after.
    assert!(h.store().watched(&repo.common, AT, AT + SEC).unwrap());
    assert!(
        !h.store()
            .watched(&repo.common, AT + 10 * SEC, AT + 11 * SEC)
            .unwrap()
    );
}

// ---- observe ----

#[test]
fn observe_an_edit_belongs_to_the_branch_head_was_on() {
    let (h, _) = watched("refs/heads/feature");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/feature".to_owned(), 100)]
    );
}

#[test]
fn observe_the_default_branch_and_a_detached_head_are_unassigned() {
    let (h, _) = watched("refs/heads/main");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let s = session_usage(&h, &fake, "s");
    assert_eq!(rows(&s), [("(unassigned)".to_owned(), 100)]);
    // The default branch is known: nothing is "unattributable".
    assert!(matches!(
        &s.rows[0].key,
        RowKey::Unassigned { unattributable } if unattributable.input == 0
    ));
}

#[test]
fn observe_a_call_nobody_watched_is_imported_and_unattributable() {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let s = session_usage(&h, &fake, "s");
    assert!(matches!(
        &s.rows[0].key,
        RowKey::Unassigned { unattributable } if unattributable.input == 100
    ));
}

#[test]
fn observe_the_edit_before_the_arm_second_is_not_covered() {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    let repo = Repo::new("/work/app/.git");
    // Armed half a second before the call: the second before it is not
    // watched.
    h.arm(
        &repo,
        AT + 20 - 500,
        &[("/work/app/.git", Some("refs/heads/feature"))],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let s = session_usage(&h, &fake, "s");
    assert!(matches!(&s.rows[0].key, RowKey::Unassigned { .. }));
}

#[test]
fn observe_a_switch_before_the_call_sets_the_branch_it_ran_on() {
    let (h, repo) = watched("refs/heads/main");
    h.reflog(
        &repo,
        vec![repo.switch(&main(), "main", "topic", AT - 30 * SEC)],
        vec![],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/topic".to_owned(), 100)]
    );
}

// ---- order ----

#[test]
fn order_a_switch_across_the_result_time_makes_the_edit_unattributable() {
    let (h, repo) = watched("refs/heads/feature");
    // The edit runs from AT+20 to AT+1020; the switch was written in the
    // second AT+1000, found at AT+1100: its interval holds the result time.
    h.reflog(
        &repo,
        vec![repo.switch(&main(), "feature", "other", AT + 1000)],
        vec![],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let s = session_usage(&h, &fake, "s");
    assert!(matches!(
        &s.rows[0].key,
        RowKey::Unassigned { unattributable } if unattributable.input == 100
    ));
}

#[test]
fn order_the_previous_read_narrows_the_time_of_a_record() {
    let (h, repo) = watched("refs/heads/feature");
    // The same record, but the file was read at AT+900 without it: it was
    // written after that, so after the edit began... and before the result
    // (AT+1020) because it was found at AT+950? No: found at AT+1100, after
    // the result, in a second that began at AT+1000: not before.
    let rec = repo.record(
        ReflogFile::Head(main()),
        Some(1),
        2,
        AT + 1000,
        100,
        Some(AT + 1050),
        "checkout: moving from feature to other",
        Origin::Appended,
    );
    h.reflog(&repo, vec![rec], vec![]);
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    // Written after AT+1050: after the result.
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/feature".to_owned(), 100)]
    );
}

#[test]
fn order_a_gap_in_the_reading_makes_the_call_unsure() {
    let (h, repo) = watched("refs/heads/feature");
    h.reflog(&repo, vec![], vec![repo.unreadable(&main(), AT + 500)]);
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let s = session_usage(&h, &fake, "s");
    assert!(
        matches!(&s.rows[0].key, RowKey::Unassigned { unattributable } if unattributable.input == 100)
    );
}

// ---- multi_switch ----

/// A turn that edits `a.rs` and `b.rs` on `feature`, then runs one long
/// command (`git switch other`) during which HEAD moves to `other`.
fn two_files_and_a_switch(h: &Harness, repo: &Repo, commit_a: bool) -> Arc<FakeSessions> {
    let wt = main();
    let mut records = Vec::new();
    if commit_a {
        // `a.rs` is committed on `feature` before the switch.
        records.push(repo.commit(&wt, 1, 5, AT + 14 * SEC));
        h.git.set_commit(&hex(5), &["src/a.rs"]);
    }
    records.push(repo.switch(&wt, "feature", "other", AT + 16 * SEC));
    h.reflog(repo, records, vec![]);
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 4);
    log.edit("t1", "c2", &["/work/app/src/b.rs"], AT + 40, 4);
    log.exec(
        "t1",
        "c3",
        "git commit -am x; git switch other",
        "/work/app",
        AT + 10 * SEC,
    );
    log.tool(
        "t1",
        "c4",
        lectern_services::session::ToolKind::Execute {
            command: "git switch other".into(),
            workdir: Arc::from(Path::new("/work/app")),
            exec_session: None,
            workdir_exact: true,
        },
        AT + 12 * SEC,
        AT + 20 * SEC,
    );
    log.write();
    h.read_every(&fake);
    settled(h, &fake, "s");
    fake
}

fn hex(n: u64) -> Vec<u8> {
    let s = oid(n);
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

#[test]
fn multi_switch_uncommitted_work_goes_with_the_working_tree_to_the_last_branch() {
    let (h, repo) = watched("refs/heads/feature");
    let fake = two_files_and_a_switch(&h, &repo, false);
    // Neither file was committed: both follow HEAD to `other`.
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/other".to_owned(), 100)]
    );
}

#[test]
fn multi_switch_a_commit_keeps_its_file_on_the_branch_it_was_made_on() {
    let (h, repo) = watched("refs/heads/feature");
    let fake = two_files_and_a_switch(&h, &repo, true);
    let mut found = branch_rows(&h, &fake);
    found.sort();
    // `a.rs` (4 lines + 1 file) stayed on `feature`; `b.rs` (4 + 1) went on.
    assert_eq!(
        found,
        [
            ("refs/heads/feature".to_owned(), 50),
            ("refs/heads/other".to_owned(), 50)
        ]
    );
    assert_eq!(h.git.commit_reads(), 1);
}

// ---- branch_by_change ----

#[test]
fn branch_by_change_several_branches_are_split_by_lines_plus_files() {
    let (h, repo) = watched("refs/heads/feature");
    h.reflog(
        &repo,
        vec![repo.switch(&main(), "feature", "other", AT + 5 * SEC)],
        vec![],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 90);
    // 2 lines + 1 file on `feature`, 5 lines + 1 file on `other`.
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 2);
    log.edit("t1", "c2", &["/work/app/src/b.rs"], AT + 10 * SEC, 5);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let mut found = branch_rows(&h, &fake);
    found.sort();
    assert_eq!(
        found,
        [
            ("refs/heads/feature".to_owned(), 30),
            ("refs/heads/other".to_owned(), 60)
        ]
    );
}

#[test]
fn branch_by_change_cross_worktree() {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    h.projects
        .repo(Path::new("/work/app-b"), Path::new("/work/app/.git"));
    let repo = Repo::new("/work/app/.git");
    h.arm(
        &repo,
        AT - 60 * SEC,
        &[
            ("/work/app/.git", Some("refs/heads/feature")),
            ("/work/app/.git/worktrees/app-b", Some("refs/heads/other")),
        ],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 80);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.edit("t1", "c2", &["/work/app-b/src/b.rs"], AT + 40, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let mut found = branch_rows(&h, &fake);
    found.sort();
    assert_eq!(
        found,
        [
            ("refs/heads/feature".to_owned(), 40),
            ("refs/heads/other".to_owned(), 40)
        ]
    );
}

// ---- enter ----

/// A repository with a second worktree on `other`; the session runs in
/// `/work/app` (on `feature`).
fn two_worktrees() -> (Harness, Repo) {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    h.projects
        .repo(Path::new("/work/app-b"), Path::new("/work/app/.git"));
    let repo = Repo::new("/work/app/.git");
    h.git.set_default(&repo.common, "refs/heads/main");
    h.arm(
        &repo,
        AT - 60 * SEC,
        &[
            ("/work/app/.git", Some("refs/heads/feature")),
            ("/work/app/.git/worktrees/app-b", Some("refs/heads/other")),
        ],
    );
    (h, repo)
}

#[test]
fn enter_a_command_that_goes_into_another_worktree() {
    let (h, _) = two_worktrees();
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.exec(
        "t1",
        "c1",
        "cd ../app-b && cargo test",
        "/work/app",
        AT + 20,
    );
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/other".to_owned(), 100)]
    );
}

#[test]
fn enter_git_c_names_the_worktree() {
    let (h, _) = two_worktrees();
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.exec("t1", "c1", "git -C /work/app-b log", "/work/app", AT + 20);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/other".to_owned(), 100)]
    );
}

#[test]
fn enter_reads_and_searches_name_their_worktrees_and_split_equally() {
    let (h, _) = two_worktrees();
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.read("t1", "c1", &["/work/app/src/a.rs"], AT + 20);
    log.search("t1", "c2", &["/work/app-b/src"], AT + 40);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let mut found = branch_rows(&h, &fake);
    found.sort();
    assert_eq!(
        found,
        [
            ("refs/heads/feature".to_owned(), 50),
            ("refs/heads/other".to_owned(), 50)
        ]
    );
}

#[test]
fn enter_a_write_that_left_no_change_still_says_where_the_session_was() {
    let (h, _) = two_worktrees();
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app-b/src/a.rs"], AT + 20, 0);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/other".to_owned(), 100)]
    );
}

#[test]
fn enter_comes_before_following_the_nearest_changes() {
    let (h, _) = two_worktrees();
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 10);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.turn("t2", AT + 20 * SEC)
        .usage("t2", "m2", AT + 20 * SEC + 10, 40);
    log.read("t2", "c2", &["/work/app-b/src/b.rs"], AT + 20 * SEC + 20);
    log.turn("t3", AT + 40 * SEC)
        .usage("t3", "m3", AT + 40 * SEC + 10, 50);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let mut found = branch_rows(&h, &fake);
    found.sort();
    // t1 on `feature`; t2 read in `other` (not the follow); t3 follows t1.
    assert_eq!(
        found,
        [
            ("refs/heads/feature".to_owned(), 60),
            ("refs/heads/other".to_owned(), 40)
        ]
    );
}

#[test]
fn enter_commands_that_are_not_about_git_or_a_directory_say_nothing() {
    let (h, _) = two_worktrees();
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.exec("t1", "c1", "cargo test", "/work/app", AT + 20);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    // The turn was observed in its own worktree.
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/feature".to_owned(), 100)]
    );
    let n: i64 = conn(&h)
        .query_row("SELECT count(*) FROM call", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "no row for a command that says nothing");
}

// ---- follow_switched / cwd_branch ----

#[test]
fn follow_switched_the_branches_of_the_worktree_during_a_turn_without_changes() {
    let (h, repo) = watched("refs/heads/feature");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    // t1 changes a file in this worktree.
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 10);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    // t2 only switches branches.
    let t2 = AT + 30 * SEC;
    log.turn("t2", t2).usage("t2", "m2", t2 + 10, 40);
    log.tool(
        "t2",
        "c2",
        lectern_services::session::ToolKind::Execute {
            command: "git switch other".into(),
            workdir: Arc::from(Path::new("/work/app")),
            exec_session: None,
            workdir_exact: true,
        },
        t2 + SEC,
        t2 + 3 * SEC,
    );
    log.write();
    h.reflog(
        &repo,
        vec![repo.switch(&main(), "feature", "other", t2 + 2 * SEC)],
        vec![],
    );
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let mut found = branch_rows(&h, &fake);
    found.sort();
    // t1: `feature`. t2: both branches it was on, equally.
    assert_eq!(
        found,
        [
            ("refs/heads/feature".to_owned(), 30),
            ("refs/heads/other".to_owned(), 20)
        ]
    );
}

#[test]
fn cwd_branch_a_session_without_changes_follows_the_branch_the_turn_ended_on() {
    let (h, repo) = watched("refs/heads/main");
    h.reflog(
        &repo,
        vec![repo.switch(&main(), "main", "topic", AT + 10 * SEC)],
        vec![],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 40);
    log.turn("t2", AT + 60 * SEC)
        .usage("t2", "m2", AT + 60 * SEC + 10, 60);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    // t1 ended (its last record at AT+10) on `main`, t2 on `topic`.
    let mut found = branch_rows(&h, &fake);
    found.sort();
    assert_eq!(
        found,
        [
            ("(unassigned)".to_owned(), 40),
            ("refs/heads/topic".to_owned(), 60)
        ]
    );
}

// ---- imported ----

fn imported_log(branch: &str, cwd: &str) -> (Harness, Arc<FakeSessions>) {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    h.git
        .set_default(Path::new("/work/app/.git"), "refs/heads/main");
    let fake = source();
    let mut log = Log::new(&fake, "s", cwd, AT);
    log.context_branch(AT, cwd, branch);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    (h, fake)
}

#[test]
fn imported_a_branch_that_was_deleted_is_a_segment_of_its_name() {
    let (h, fake) = imported_log("topic", "/work/app");
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/topic".to_owned(), 100)]
    );
    let found = segments(&h);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1, 2, "only a name");
}

#[test]
fn imported_a_branch_with_a_log_is_open_to_the_past_when_the_log_is_cut() {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    let common = PathBuf::from("/work/app/.git");
    // The reflog exists but does not start with the branch's creation.
    h.git.set_log(&common, "refs/heads/topic", None, &[]);
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.context_branch(AT, "/work/app", "topic");
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/topic".to_owned(), 100)]
    );
    let found = segments(&h);
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].1, found[0].3), (0, None), "open to the past");
    assert_eq!(h.git.log_reads(), 1);
}

#[test]
fn imported_a_branch_made_after_the_turn_is_only_a_name() {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    let common = PathBuf::from("/work/app/.git");
    h.git
        .set_log(&common, "refs/heads/topic", Some(AT + 3600 * SEC), &[]);
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.context_branch(AT, "/work/app", "topic");
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let found = segments(&h);
    assert!(found.iter().any(|s| s.1 == 2), "{found:?}");
}

#[test]
fn imported_the_log_branch_counts_only_for_files_in_the_worktree_it_ran_in() {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    h.projects
        .repo(Path::new("/work/app-b"), Path::new("/work/app/.git"));
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.context_branch(AT, "/work/app", "topic");
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app-b/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let s = session_usage(&h, &fake, "s");
    assert!(matches!(
        &s.rows[0].key,
        RowKey::Unassigned { unattributable } if unattributable.input == 100
    ));
}

#[test]
fn imported_the_default_branch_is_unassigned() {
    let (h, fake) = imported_log("main", "/work/app");
    assert_eq!(branch_rows(&h, &fake), [("(unassigned)".to_owned(), 100)]);
}

// ---- the worktree query ----

fn worktree_usage(h: &Harness, key: &lectern_services::project::ProjectKey) -> WorktreeUsage {
    let snapshot = lectern_services::project::Projects::snapshot(&h.projects, key).unwrap();
    match h.snapshot(&UsageQuery::Worktree {
        project: key.clone(),
        worktree: snapshot.worktrees[0].key.clone(),
    }) {
        Some(UsageSnapshot::Worktree(w)) => w,
        other => panic!("not a worktree snapshot: {other:?}"),
    }
}

#[test]
fn branch_by_change_the_worktree_query_gives_the_cumulative_of_its_branch() {
    let mut h = Harness::new();
    let key = h.open_repo("/work/app");
    let repo = Repo::new("/work/app/.git");
    h.projects.set_head(
        Path::new("/work/app"),
        Head::Branch("refs/heads/feature".into()),
    );
    h.git.set_default(&repo.common, "refs/heads/main");
    h.arm(
        &repo,
        AT - 60 * SEC,
        &[("/work/app/.git", Some("refs/heads/feature"))],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let w = worktree_usage(&h, &key);
    assert_eq!(&*w.branch.unwrap().name, "refs/heads/feature");
    assert_eq!(w.cumulative.unwrap().input, 100);

    // On the default branch there is no cumulative.
    h.projects.set_head(
        Path::new("/work/app"),
        Head::Branch("refs/heads/main".into()),
    );
    let w = worktree_usage(&h, &key);
    assert!(w.branch.is_some() && w.cumulative.is_none());
    h.projects
        .set_head(Path::new("/work/app"), Head::Detached("abc".into()));
    let w = worktree_usage(&h, &key);
    assert!(w.branch.is_none() && w.cumulative.is_none());
}

#[test]
fn branch_by_change_a_session_row_names_the_worktree_that_has_the_branch() {
    // What `review_view` reads (docs/design/review_view.md「简报」): the full
    // reference name, the key of the worktree on the branch (the one the
    // project gives the session's location), the session's part, and the
    // turns by the `TurnId` of the log.
    let mut h = Harness::new();
    let key = h.open_repo("/work/app");
    let repo = Repo::new("/work/app/.git");
    h.projects.set_head(
        Path::new("/work/app"),
        Head::Branch("refs/heads/feature".into()),
    );
    h.git.set_default(&repo.common, "refs/heads/main");
    h.arm(
        &repo,
        AT - 60 * SEC,
        &[("/work/app/.git", Some("refs/heads/feature"))],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.turn("t2", AT + 100).usage("t2", "m2", AT + 110, 7);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let s = session_usage(&h, &fake, "s");
    let worktree = lectern_services::project::Projects::snapshot(&h.projects, &key)
        .unwrap()
        .worktrees[0]
        .key
        .clone();
    assert_eq!(s.rows.len(), 1, "{:?}", s.rows);
    let row = &s.rows[0];
    let RowKey::Branch { branch, .. } = &row.key else {
        panic!("not a branch row: {row:?}");
    };
    assert_eq!(&*branch.name, "refs/heads/feature");
    assert_eq!(branch.worktree.as_ref(), Some(&worktree));
    assert_eq!(row.worktree.as_ref(), Some(&worktree));
    assert_eq!(row.session_part.input, 107);
    assert_eq!(row.cumulative.input, 107);
    assert!(!row.shared);
    let turns: Vec<_> = s.turns.iter().map(|(t, n)| (&*t.0, n.input)).collect();
    assert_eq!(turns, [("t1", 100), ("t2", 7)]);

    // A branch that is checked out nowhere has a row without a worktree.
    h.projects.set_head(
        Path::new("/work/app"),
        Head::Branch("refs/heads/other".into()),
    );
    let s = session_usage(&h, &fake, "s");
    assert_eq!(s.rows[0].worktree, None, "{:?}", s.rows);
}

#[test]
fn branch_by_change_the_worktree_query_of_a_branch_without_usage_counts_nothing() {
    // A watched branch that no session has used yet is counted (zero), not
    // "not counted": the worktrees detail tells the two apart.
    let mut h = Harness::new();
    let key = h.open_repo("/work/app");
    let repo = Repo::new("/work/app/.git");
    h.projects.set_head(
        Path::new("/work/app"),
        Head::Branch("refs/heads/feature".into()),
    );
    h.git.set_default(&repo.common, "refs/heads/main");
    h.arm(
        &repo,
        AT - 60 * SEC,
        &[("/work/app/.git", Some("refs/heads/feature"))],
    );
    let w = worktree_usage(&h, &key);
    assert_eq!(
        w.branch.as_ref().map(|b| &*b.name),
        Some("refs/heads/feature")
    );
    assert_eq!(w.cumulative.map(|t| t.total()), Some(0), "{w:?}");
}

// ---- rebuild ----

#[test]
fn rebuild_gives_the_same_branches_from_the_observations() {
    let (h, repo) = watched("refs/heads/feature");
    h.reflog(
        &repo,
        vec![repo.switch(&main(), "feature", "other", AT + 5 * SEC)],
        vec![],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 90);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 2);
    log.edit("t1", "c2", &["/work/app/src/b.rs"], AT + 10 * SEC, 5);
    log.turn("t2", AT + 30 * SEC)
        .usage("t2", "m2", AT + 30 * SEC + 10, 10);
    log.read("t2", "c3", &["/work/app/src/a.rs"], AT + 30 * SEC + 20);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let before = branch_rows(&h, &fake);
    assert!(before.len() >= 2, "{before:?}");
    // The derived tables are deleted and made again from the observations.
    h.store().rebuild_hot().unwrap();
    let mut a = before.clone();
    let mut b = branch_rows(&h, &fake);
    a.sort();
    b.sort();
    assert_eq!(a, b);
    assert_buckets(&h);
}

#[test]
fn rebuild_keeps_what_was_observed_after_the_branch_is_gone() {
    let (h, repo) = watched("refs/heads/feature");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    // The branch is deleted, and the default branch moves: the observation
    // stays as it was.
    h.reflog(
        &repo,
        vec![],
        vec![repo.removed("refs/heads/feature", AT + 3600 * SEC)],
    );
    h.git.set_default(&repo.common, "refs/heads/feature");
    h.store().rebuild_hot().unwrap();
    assert_eq!(
        branch_rows(&h, &fake),
        [("refs/heads/feature".to_owned(), 100)]
    );
}
