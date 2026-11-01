//! `lines_*`: reflog lines the command-line tool wrote, for a checkout of a
//! name that is not a branch and for a worktree that was added while the
//! watch ran (docs/design/usage.md「分支观察」「实际验证」).
//!
//! The lines are copied from a repository made for a one-off check (only the
//! time is replaced by the test's own): the other tests write lines by hand
//! and so could not show that the real ones differ. The tests run no
//! external command; the reads of the repository are a table
//! (`testing::FakeGit`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_fs_watch::{Origin, ReflogFile, WorktreeName};
use lectern_services::usage::RowKey;

use super::attribution::{AT, Log, conn, session_usage, source};
use super::branch_fixes::{baselines, rig_with_repo};
use super::branches::{Repo, batch, watched};
use super::support::*;
use crate::gitread::WorktreeHead;
use crate::store::repo::{Facts, Reconcile};

const SEC: i64 = 1_000;

/// The time the lines below were written; `at` replaces it.
const WRITTEN: &str = "1793541395";

// `git checkout v1.0`, `git checkout o/x` (a remote-tracking name),
// `git checkout main~1`, `git switch --detach main` and
// `git checkout --detach`, each with HEAD on `main` (or `br`) before.
const TAG: &str = "5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 Dev <dev@example.org> 1793541395 +0800\tcheckout: moving from main to v1.0";
const REMOTE: &str = "5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 6a7df07806b5eb58859f81392587175558bf653f Dev <dev@example.org> 1793541395 +0800\tcheckout: moving from main to o/x";
const PARENT: &str = "5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 6a7df07806b5eb58859f81392587175558bf653f Dev <dev@example.org> 1793541395 +0800\tcheckout: moving from main to main~1";
const DETACH_MAIN: &str = "5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 Dev <dev@example.org> 1793541395 +0800\tcheckout: moving from main to main";
const DETACH_HEAD: &str = "5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 Dev <dev@example.org> 1793541395 +0800\tcheckout: moving from br to HEAD";
// `git switch -c br`, the branch that is gone in the test.
const TO_BRANCH: &str = "5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 Dev <dev@example.org> 1793541395 +0800\tcheckout: moving from main to br";
// The first two lines of the HEAD reflog of a worktree made by
// `git worktree add -b wtb`: a line without a message (and without a tab),
// then the reset. No line names the branch.
const LINKED_FIRST: &str = "0000000000000000000000000000000000000000 5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 Dev <dev@example.org> 1793541395 +0800";
const LINKED_RESET: &str = "5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 5b27a6b1aed8bd5e7829a6dc3c4f21ca7d6dbe14 Dev <dev@example.org> 1793541395 +0800\treset: moving to HEAD";

fn main() -> WorktreeName {
    WorktreeName::Main
}

/// A line written at `at` (milliseconds).
fn at(line: &str, at: i64) -> String {
    line.replace(WRITTEN, &(at / 1000).to_string())
}

fn facts(branches: &[&str]) -> Facts {
    Facts {
        branches: Some(branches.iter().map(|b| (*b).to_owned()).collect()),
    }
}

/// The repository `/work/app` watched with HEAD on `main`, which is not its
/// default branch (so that a call credited to it shows).
fn watched_on_main() -> (Harness, Repo) {
    let (h, repo) = watched("refs/heads/main");
    h.git.set_default(&repo.common, "refs/heads/trunk");
    (h, repo)
}

/// The session `s` in `cwd` with a call at each of `times`, 100 tokens each.
fn calls(h: &Harness, cwd: &str, times: &[i64]) -> Arc<crate::testing::FakeSessions> {
    let fake = source();
    let mut log = Log::new(&fake, "s", cwd, times[0] - SEC);
    let file = format!("{cwd}/src/a.rs");
    for (n, t) in times.iter().enumerate() {
        let turn = format!("t{n}");
        log.turn(&turn, *t)
            .usage(&turn, &format!("m{n}"), *t + 10, 100);
        log.edit(&turn, &format!("c{n}"), &[file.as_str()], *t + 20, 3);
    }
    log.write();
    h.read_every(&fake);
    h.settle_all(&fake, "s", AT + 3600 * SEC);
    fake
}

/// (branch name or `(unassigned)`, input, unattributable input).
fn rows(h: &Harness, fake: &crate::testing::FakeSessions) -> Vec<(String, u64, u64)> {
    let mut found: Vec<(String, u64, u64)> = session_usage(h, fake, "s")
        .rows
        .iter()
        .map(|r| match &r.key {
            RowKey::Branch { branch, .. } => (branch.name.to_string(), r.session_part.input, 0),
            RowKey::Unassigned { unattributable } => (
                "(unassigned)".to_owned(),
                r.session_part.input,
                unattributable.input,
            ),
            RowKey::Mrs { .. } => ("(mrs)".to_owned(), r.session_part.input, 0),
        })
        .collect();
    found.sort();
    found
}

/// (act, a, b) of the HEAD records.
fn head_acts(h: &Harness) -> Vec<(i64, Option<String>, Option<String>)> {
    let c = conn(h);
    let mut stmt = c
        .prepare("SELECT act, a, b FROM reflog WHERE file = 0 ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn segment_count(h: &Harness) -> i64 {
    conn(h)
        .query_row("SELECT count(*) FROM segment", [], |r| r.get(0))
        .unwrap()
}

const DETACH: i64 = 2;
const SWITCH: i64 = 1;

#[test]
fn lines_a_checkout_of_a_tag_a_remote_name_a_parent_or_head_is_a_detach() {
    for (line, from) in [
        (TAG, "refs/heads/main"),
        (REMOTE, "refs/heads/main"),
        (PARENT, "refs/heads/main"),
        (DETACH_HEAD, "refs/heads/br"),
    ] {
        let (h, repo) = watched_on_main();
        let record = repo.literal(
            ReflogFile::Head(main()),
            &at(line, AT + 5 * SEC),
            AT + 5 * SEC + 100,
            Origin::Appended,
        );
        let before = segment_count(&h);
        h.store()
            .repo_batch_with(
                &repo.common,
                &batch(vec![record], vec![]),
                &facts(&["refs/heads/main", "refs/heads/br"]),
            )
            .unwrap();
        // A detach from the branch HEAD was on; no segment for the name.
        assert_eq!(
            head_acts(&h),
            [(DETACH, Some(from.to_owned()), None)],
            "{line}"
        );
        assert_eq!(
            segment_count(&h),
            before + i64::from(from.ends_with("br")) + 1
        );
        let fake = calls(&h, "/work/app", &[AT + 20 * SEC]);
        assert_eq!(
            rows(&h, &fake),
            [("(unassigned)".to_owned(), 100, 0)],
            "{line}"
        );
    }
}

#[test]
fn lines_a_switch_to_a_branch_is_a_switch() {
    let (h, repo) = watched_on_main();
    let record = repo.literal(
        ReflogFile::Head(main()),
        &at(TO_BRANCH, AT + 5 * SEC),
        AT + 5 * SEC + 100,
        Origin::Appended,
    );
    h.store()
        .repo_batch_with(
            &repo.common,
            &batch(vec![record], vec![]),
            &facts(&["refs/heads/main", "refs/heads/br"]),
        )
        .unwrap();
    assert_eq!(
        head_acts(&h),
        [(
            SWITCH,
            Some("refs/heads/main".to_owned()),
            Some("refs/heads/br".to_owned())
        )]
    );
    let fake = calls(&h, "/work/app", &[AT + 20 * SEC]);
    assert_eq!(rows(&h, &fake), [("refs/heads/br".to_owned(), 100, 0)]);
}

#[test]
fn lines_a_branch_deleted_in_the_same_batch_is_still_a_switch() {
    let (h, repo) = watched_on_main();
    let record = repo.literal(
        ReflogFile::Head(main()),
        &at(TO_BRANCH, AT + 5 * SEC),
        AT + 5 * SEC + 100,
        Origin::Appended,
    );
    // The branch is gone when the repository is read, and its log was removed
    // in the batch.
    h.store()
        .repo_batch_with(
            &repo.common,
            &batch(
                vec![record],
                vec![repo.removed("refs/heads/br", AT + 6 * SEC)],
            ),
            &facts(&["refs/heads/main"]),
        )
        .unwrap();
    assert_eq!(head_acts(&h)[0].0, SWITCH);
}

#[test]
fn lines_without_a_read_of_the_repository_a_name_is_a_branch_unless_it_is_the_commit() {
    let (h, repo) = watched_on_main();
    let record = repo.literal(
        ReflogFile::Head(main()),
        &at(TAG, AT + 5 * SEC),
        AT + 5 * SEC + 100,
        Origin::Appended,
    );
    h.store()
        .repo_batch_with(
            &repo.common,
            &batch(vec![record], vec![]),
            &Facts::default(),
        )
        .unwrap();
    // The known limit: nothing says `v1.0` is a tag.
    assert_eq!(head_acts(&h)[0].0, SWITCH);
}

#[test]
fn lines_a_detach_written_as_a_switch_is_corrected_by_the_head_that_is_read() {
    let (h, repo) = watched_on_main();
    // `git switch --detach main` while on `main`: the line is the one of
    // `git switch main`.
    let record = repo.literal(
        ReflogFile::Head(main()),
        &at(DETACH_MAIN, AT + 5 * SEC),
        AT + 5 * SEC + 100,
        Origin::Appended,
    );
    let (_, what) = h
        .store()
        .repo_batch_with(
            &repo.common,
            &batch(vec![record], vec![]),
            &facts(&["refs/heads/main"]),
        )
        .unwrap();
    assert_eq!(head_acts(&h)[0].0, SWITCH);
    // HEAD is read after the batch: detached.
    let key = PathBuf::from("/work/app/.git");
    let heads = [WorktreeHead {
        key: key.clone(),
        branch: None,
        rebasing: false,
    }];
    h.store()
        .reconcile(&repo.common, (AT + 8 * SEC) * 1_000_000, &heads, &what)
        .unwrap();
    // A call across the switch and the read is unsure; one after is not.
    let fake = calls(&h, "/work/app", &[AT + 5 * SEC, AT + 20 * SEC]);
    assert_eq!(rows(&h, &fake), [("(unassigned)".to_owned(), 200, 100)]);
    let c = conn(&h);
    let resets: i64 = c
        .query_row(
            "SELECT count(*) FROM reflog_reset WHERE reason = 4",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        (resets, baselines_of(&h)),
        (1, 3),
        "the watch, the batch, the read"
    );
}

fn baselines_of(h: &Harness) -> i64 {
    conn(h)
        .query_row("SELECT count(*) FROM head_baseline", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn lines_a_head_that_is_where_the_records_left_it_adds_nothing() {
    let (h, repo) = watched_on_main();
    let record = repo.literal(
        ReflogFile::Head(main()),
        &at(TO_BRANCH, AT + 5 * SEC),
        AT + 5 * SEC + 100,
        Origin::Appended,
    );
    let (_, what) = h
        .store()
        .repo_batch_with(
            &repo.common,
            &batch(vec![record], vec![]),
            &facts(&["refs/heads/br"]),
        )
        .unwrap();
    let before = baselines_of(&h);
    let heads = [WorktreeHead {
        key: PathBuf::from("/work/app/.git"),
        branch: Some("refs/heads/br".to_owned()),
        rebasing: false,
    }];
    h.store()
        .reconcile(&repo.common, (AT + 8 * SEC) * 1_000_000, &heads, &what)
        .unwrap();
    assert_eq!(baselines_of(&h), before);
    let resets: i64 = conn(&h)
        .query_row("SELECT count(*) FROM reflog_reset", [], |r| r.get(0))
        .unwrap();
    assert_eq!(resets, 0);
}

#[test]
fn lines_a_worktree_made_while_the_watch_runs_is_on_its_branch_once_it_is_read() {
    let (h, repo) = watched_on_main();
    let linked = WorktreeName::Linked("app-wt".into());
    let file = ReflogFile::Head(linked);
    let first = repo.literal(
        file.clone(),
        &at(LINKED_FIRST, AT + 5 * SEC),
        AT + 5 * SEC + 100,
        Origin::NewFile,
    );
    let reset = repo.literal(
        file,
        &at(LINKED_RESET, AT + 5 * SEC),
        AT + 5 * SEC + 100,
        Origin::NewFile,
    );
    let (stored, what) = h
        .store()
        .repo_batch_with(
            &repo.common,
            &batch(vec![first, reset], vec![]),
            &Facts::default(),
        )
        .unwrap();
    assert_eq!(stored.records, 2);
    // Neither line is a switch; the worktree has records and no baseline.
    let key = PathBuf::from("/work/app/.git/worktrees/app-wt");
    assert!(what.moved.is_empty());
    assert_eq!(what.fresh, [key.clone()].into_iter().collect());
    let heads = [
        WorktreeHead {
            key: PathBuf::from("/work/app/.git"),
            branch: Some("refs/heads/main".to_owned()),
            rebasing: false,
        },
        WorktreeHead {
            key,
            branch: Some("refs/heads/wtb".to_owned()),
            rebasing: false,
        },
    ];
    h.store()
        .reconcile(&repo.common, (AT + 8 * SEC) * 1_000_000, &heads, &what)
        .unwrap();
    // One baseline for the new worktree, none for the main one.
    assert_eq!(baselines_of(&h), 2);
    h.projects.repo(Path::new("/work/app-wt"), &repo.common);
    let fake = calls(&h, "/work/app-wt", &[AT + 20 * SEC]);
    assert_eq!(rows(&h, &fake), [("refs/heads/wtb".to_owned(), 100, 0)]);
}

// ---- through the engine ----

fn main_line(
    line: &str,
    rig: &super::plugin::Rig,
    repo: &Repo,
    ms_after: i64,
) -> lectern_fs_watch::ReflogRecord {
    let now = rig.time.now_ms();
    repo.literal(
        ReflogFile::Head(main()),
        &at(line, now + ms_after),
        now + ms_after + 100,
        Origin::Appended,
    )
}

fn acts(rig: &super::plugin::Rig) -> Vec<i64> {
    let c = rusqlite::Connection::open(rig.db()).unwrap();
    let mut stmt = c
        .prepare("SELECT act FROM reflog WHERE file = 0 ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn lines_plugin_reads_which_names_are_branches_and_corrects_a_detach_with_one_head_read() {
    let (rig, _dir, repo, reads) = rig_with_repo();
    let common = repo.common.clone();
    reads.set_branches(&common, &["refs/heads/feature"]);
    assert_eq!(baselines(&rig), 1);
    let heads_before = reads.head_reads();
    // Two batches queued together: a tag, then `switch --detach feature`.
    let tag = main_line(&TAG.replace("main", "feature"), &rig, &repo, 1_000);
    let detach = main_line(&DETACH_MAIN.replace("main", "feature"), &rig, &repo, 2_000);
    rig.engine
        .on_repo_batch(&common, batch(vec![tag], vec![]), None);
    rig.engine
        .on_repo_batch(&common, batch(vec![detach], vec![]), None);
    // HEAD is detached.
    reads.set_heads(&common, &[(&common, None)]);
    rig.settle();
    // The tag is a detach; the other line is a switch to a branch that
    // exists.
    assert_eq!(acts(&rig), [DETACH, SWITCH]);
    assert_eq!(reads.branch_reads(), 2);
    // One read of HEAD for both batches; it differs from the records.
    assert_eq!(reads.head_reads() - heads_before, 1);
    let c = rusqlite::Connection::open(rig.db()).unwrap();
    let resets: i64 = c
        .query_row(
            "SELECT count(*) FROM reflog_reset WHERE reason = 4",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(resets, 1);
    // Nothing waits: another batch with no switch reads nothing.
    let commit = repo.commit(&main(), 1, 2, rig.time.now_ms() + 3_000);
    rig.engine
        .on_repo_batch(&common, batch(vec![commit], vec![]), None);
    rig.settle();
    assert_eq!(reads.head_reads() - heads_before, 1);
}

#[test]
fn lines_plugin_does_not_correct_a_head_that_is_where_the_records_left_it() {
    let (rig, _dir, repo, reads) = rig_with_repo();
    let common = repo.common.clone();
    reads.set_branches(&common, &["refs/heads/feature", "refs/heads/br"]);
    let line = main_line(&TO_BRANCH.replace("main", "feature"), &rig, &repo, 1_000);
    reads.set_heads(&common, &[(&common, Some("refs/heads/br"))]);
    rig.engine
        .on_repo_batch(&common, batch(vec![line], vec![]), None);
    rig.settle();
    assert_eq!(acts(&rig), [SWITCH]);
    assert_eq!(baselines(&rig), 2, "the watch, and the batch's own");
}

#[test]
fn lines_plugin_reads_a_worktree_made_while_it_watches() {
    let (rig, _dir, repo, reads) = rig_with_repo();
    let common = repo.common.clone();
    let key = common.join("worktrees/wt");
    assert_eq!(baselines(&rig), 1);
    // The first lines of the new worktree's HEAD log (a new file), then
    // the repository says it is on `wtb`.
    reads.set_heads(
        &common,
        &[
            (&common, Some("refs/heads/feature")),
            (&key, Some("refs/heads/wtb")),
        ],
    );
    let now = rig.time.now_ms();
    let file = ReflogFile::Head(WorktreeName::Linked("wt".into()));
    let first = repo.literal(
        file.clone(),
        &at(LINKED_FIRST, now + 1_000),
        now + 1_100,
        Origin::NewFile,
    );
    let reset = repo.literal(
        file,
        &at(LINKED_RESET, now + 1_000),
        now + 1_100,
        Origin::NewFile,
    );
    rig.engine
        .on_repo_batch(&common, batch(vec![first, reset], vec![]), None);
    rig.settle();
    let c = rusqlite::Connection::open(rig.db()).unwrap();
    let branches = |c: &rusqlite::Connection| -> Vec<Option<String>> {
        let mut stmt = c
            .prepare(
                "SELECT b.branch FROM head_baseline b JOIN intern i ON i.id = b.wt
                 WHERE i.text = ?1",
            )
            .unwrap();
        stmt.query_map([key.to_str().unwrap().as_bytes()], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(branches(&c), [Some("refs/heads/wtb".to_owned())]);
    assert_eq!(baselines(&rig), 2);
    // Once: more lines of the worktree read nothing again.
    let reads_before = reads.head_reads();
    let more = repo.literal(
        ReflogFile::Head(WorktreeName::Linked("wt".into())),
        &at(LINKED_RESET, now + 2_000),
        now + 2_100,
        Origin::Appended,
    );
    rig.engine
        .on_repo_batch(&common, batch(vec![more], vec![]), None);
    rig.settle();
    assert_eq!(reads.head_reads(), reads_before);
}

#[test]
fn lines_plugin_reads_the_worktrees_when_the_list_changed_and_keeps_trying() {
    let (rig, _dir, repo, reads) = rig_with_repo();
    let common = repo.common.clone();
    let key = common.join("worktrees/wt");
    // The directory is there, HEAD cannot be read yet.
    reads.clear_heads(&common);
    let mut list = batch(vec![], vec![]);
    list.worktree_list = true;
    rig.engine.on_repo_batch(&common, list, None);
    rig.settle();
    assert_eq!(baselines(&rig), 1);
    // Readable: the next batch of any kind reads it, once.
    reads.set_heads(
        &common,
        &[
            (&common, Some("refs/heads/feature")),
            (&key, Some("refs/heads/wtb")),
        ],
    );
    let commit = repo.commit(&main(), 1, 2, rig.time.now_ms() + 1_000);
    rig.engine
        .on_repo_batch(&common, batch(vec![commit], vec![]), None);
    rig.settle();
    assert_eq!(baselines(&rig), 2);
    let again = reads.head_reads();
    let commit = repo.commit(&main(), 2, 3, rig.time.now_ms() + 2_000);
    rig.engine
        .on_repo_batch(&common, batch(vec![commit], vec![]), None);
    rig.settle();
    assert_eq!((baselines(&rig), reads.head_reads()), (2, again));
}

#[test]
fn lines_reconcile_does_not_run_for_a_repository_that_is_not_watched() {
    let (h, repo) = watched_on_main();
    let what = Reconcile {
        scan: true,
        ..Reconcile::default()
    };
    let other = Path::new("/work/other/.git");
    h.store()
        .reconcile(
            other,
            AT * 1_000_000,
            &[WorktreeHead {
                key: other.to_path_buf(),
                branch: Some("refs/heads/x".to_owned()),
                rebasing: false,
            }],
            &what,
        )
        .unwrap();
    assert_eq!(baselines_of(&h), 1);
    let _ = repo;
}

// ---- a rebase in progress ----

// `git switch feat`, then `git rebase main` stopping on a conflict, then
// `git add` and `git rebase --continue`, in one worktree: HEAD is detached
// (`rebase-merge` exists) from the start line to the finish line.
const REBASE_SWITCH: &str = "88c522627fdd35e74eccf930edf2a3fc50df7345 263dff551fe37b65a03d4a366bb138d1e67efb2b Dev <dev@example.org> 1793542742 +0800\tcheckout: moving from main to feat";
const REBASE_START: &str = "263dff551fe37b65a03d4a366bb138d1e67efb2b 88c522627fdd35e74eccf930edf2a3fc50df7345 Dev <dev@example.org> 1793542742 +0800\trebase (start): checkout main";
const REBASE_CONTINUE: &str = "88c522627fdd35e74eccf930edf2a3fc50df7345 b0c8435d809e56a2ce33b792cc583e8330dde899 Dev <dev@example.org> 1793542742 +0800\trebase (continue): featwork";
const REBASE_FINISH: &str = "b0c8435d809e56a2ce33b792cc583e8330dde899 b0c8435d809e56a2ce33b792cc583e8330dde899 Dev <dev@example.org> 1793542742 +0800\trebase (finish): returning to refs/heads/feat";
// `git switch -` and `git checkout -` write the names they resolve to.
const SWITCH_BACK: &str = "b0c8435d809e56a2ce33b792cc583e8330dde899 88c522627fdd35e74eccf930edf2a3fc50df7345 Dev <dev@example.org> 1793542742 +0800\tcheckout: moving from feat to main";

fn corrections(rig: &super::plugin::Rig) -> i64 {
    rusqlite::Connection::open(rig.db())
        .unwrap()
        .query_row(
            "SELECT count(*) FROM reflog_reset WHERE reason = 4",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// The branches of the baselines of a worktree, oldest first.
fn baseline_branches(rig: &super::plugin::Rig, key: &Path) -> Vec<Option<String>> {
    let c = rusqlite::Connection::open(rig.db()).unwrap();
    let mut stmt = c
        .prepare(
            "SELECT b.branch FROM head_baseline b JOIN intern i ON i.id = b.wt
             WHERE i.text = ?1 ORDER BY b.at_ns, b.id",
        )
        .unwrap();
    stmt.query_map([key.to_str().unwrap().as_bytes()], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn line_at(
    rig: &super::plugin::Rig,
    repo: &Repo,
    line: &str,
    ms_after: i64,
) -> lectern_fs_watch::ReflogRecord {
    let now = rig.time.now_ms();
    repo.literal(
        ReflogFile::Head(main()),
        &at(line, now + ms_after),
        now + ms_after + 100,
        Origin::Appended,
    )
}

#[test]
fn lines_plugin_does_not_take_a_head_read_in_a_rebase_for_fact() {
    let (rig, _dir, repo, reads) = rig_with_repo();
    let common = repo.common.clone();
    reads.set_branches(&common, &["refs/heads/main", "refs/heads/feat"]);
    // The rebase stops on a conflict: HEAD reads detached.
    reads.set_heads(&common, &[(&common, None)]);
    reads.set_rebasing(&common, true);
    let switch = line_at(&rig, &repo, REBASE_SWITCH, 1_000);
    let start = line_at(&rig, &repo, REBASE_START, 1_500);
    rig.engine
        .on_repo_batch(&common, batch(vec![switch, start], vec![]), None);
    rig.settle();
    // No correction, the predicted baseline stands.
    assert_eq!(corrections(&rig), 0);
    assert_eq!(
        baseline_branches(&rig, &common).last(),
        Some(&Some("refs/heads/feat".to_owned()))
    );
    // Still in the rebase, the read is tried again and again writes nothing.
    let before = reads.head_reads();
    rig.advance(crate::engine::REBASE_RETRY);
    assert!(reads.head_reads() > before);
    assert_eq!(corrections(&rig), 0);
    // `--continue` ends it; its lines arrive, HEAD is back on the branch.
    reads.set_rebasing(&common, false);
    reads.set_heads(&common, &[(&common, Some("refs/heads/feat"))]);
    let cont = line_at(&rig, &repo, REBASE_CONTINUE, 1_000);
    let finish = line_at(&rig, &repo, REBASE_FINISH, 1_500);
    rig.engine
        .on_repo_batch(&common, batch(vec![cont, finish], vec![]), None);
    rig.settle();
    assert_eq!(corrections(&rig), 0);
    assert_eq!(
        baseline_branches(&rig, &common).last(),
        Some(&Some("refs/heads/feat".to_owned()))
    );
    // Nothing waits any more: the read stops.
    let after = reads.head_reads();
    rig.advance(crate::engine::REBASE_RETRY);
    rig.advance(crate::engine::REBASE_RETRY);
    assert_eq!(reads.head_reads(), after);
    // A detach written as a switch, with no rebase, is still corrected.
    let back = line_at(&rig, &repo, SWITCH_BACK, -500);
    reads.set_heads(&common, &[(&common, None)]);
    rig.engine
        .on_repo_batch(&common, batch(vec![back], vec![]), None);
    rig.settle();
    assert_eq!(corrections(&rig), 1);
    assert_eq!(baseline_branches(&rig, &common).last(), Some(&None));
}

#[test]
fn lines_plugin_waits_for_the_end_of_a_rebase_when_the_batch_comes_first() {
    let (rig, _dir, repo, reads) = rig_with_repo();
    let common = repo.common.clone();
    reads.set_branches(&common, &["refs/heads/main", "refs/heads/feat"]);
    reads.set_heads(&common, &[(&common, None)]);
    reads.set_rebasing(&common, true);
    let switch = line_at(&rig, &repo, REBASE_SWITCH, 1_000);
    rig.engine
        .on_repo_batch(&common, batch(vec![switch], vec![]), None);
    rig.settle();
    // The finish line is drained while the directory is still there (it is
    // removed after the line); nothing else comes. The timer reads HEAD.
    let finish = line_at(&rig, &repo, REBASE_FINISH, 1_000);
    rig.engine
        .on_repo_batch(&common, batch(vec![finish], vec![]), None);
    rig.settle();
    reads.set_rebasing(&common, false);
    reads.set_heads(&common, &[(&common, Some("refs/heads/feat"))]);
    rig.advance(crate::engine::REBASE_RETRY);
    assert_eq!(corrections(&rig), 0);
    let after = reads.head_reads();
    rig.advance(crate::engine::REBASE_RETRY);
    assert_eq!(reads.head_reads(), after);
}

#[test]
fn lines_plugin_gives_a_worktree_made_in_a_rebase_its_baseline_when_the_rebase_ends() {
    let (rig, _dir, repo, reads) = rig_with_repo();
    let common = repo.common.clone();
    let key = common.join("worktrees/wt");
    assert_eq!(baselines(&rig), 1);
    reads.set_heads(
        &common,
        &[(&common, Some("refs/heads/feature")), (&key, None)],
    );
    reads.set_rebasing(&key, true);
    let now = rig.time.now_ms();
    let file = ReflogFile::Head(WorktreeName::Linked("wt".into()));
    let first = repo.literal(
        file.clone(),
        &at(LINKED_FIRST, now + 1_000),
        now + 1_100,
        Origin::NewFile,
    );
    let reset = repo.literal(
        file,
        &at(LINKED_RESET, now + 1_000),
        now + 1_100,
        Origin::NewFile,
    );
    rig.engine
        .on_repo_batch(&common, batch(vec![first, reset], vec![]), None);
    rig.settle();
    // The detached HEAD of the rebase is not a baseline.
    assert!(baseline_branches(&rig, &key).is_empty());
    rig.advance(crate::engine::REBASE_RETRY);
    assert!(baseline_branches(&rig, &key).is_empty());
    // The rebase ends: the branch is the baseline, once.
    reads.set_rebasing(&key, false);
    reads.set_heads(
        &common,
        &[
            (&common, Some("refs/heads/feature")),
            (&key, Some("refs/heads/wtb")),
        ],
    );
    rig.advance(crate::engine::REBASE_RETRY);
    assert_eq!(
        baseline_branches(&rig, &key),
        [Some("refs/heads/wtb".to_owned())]
    );
    let after = reads.head_reads();
    rig.advance(crate::engine::REBASE_RETRY);
    assert_eq!(reads.head_reads(), after);
}

#[test]
fn lines_plugin_does_not_arm_a_baseline_in_a_rebase() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let common = root.join(".git");
    let reads = Arc::new(crate::testing::FakeGit::new());
    reads.set_heads(&common, &[(&common, None)]);
    reads.set_rebasing(&common, true);
    let setup = |p: &crate::testing::FakeProjects| {
        let key = p.repo(&root, &common);
        p.set_open(&key, true);
    };
    let rig = super::plugin::Rig::with_git(
        crate::testing::FakeSessions::new(
            "session-claude-code",
            lectern_services::session::Agent::ClaudeCode,
        ),
        setup,
        reads.clone(),
    );
    rig.settle();
    // The watch began, with no baseline: the detached HEAD is the rebase's.
    assert_eq!(baselines(&rig), 0);
    let spans: i64 = rusqlite::Connection::open(rig.db())
        .unwrap()
        .query_row("SELECT count(*) FROM watch_span", [], |r| r.get(0))
        .unwrap();
    assert_eq!(spans, 1);
    reads.set_rebasing(&common, false);
    reads.set_heads(&common, &[(&common, Some("refs/heads/feature"))]);
    rig.advance(crate::engine::REBASE_RETRY);
    assert_eq!(
        baseline_branches(&rig, &common),
        [Some("refs/heads/feature".to_owned())]
    );
}
