//! `observe_*`, `segment_*`, `repo_*` and `disk_git_*` checks added after
//! the review of step 5: a renamed branch, numbers as branch names, the
//! baselines that advance, lost batches, and `DiskGit` on repositories
//! `lectern-git`'s `TestRepo` writes (no window, no external command).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_fs_watch::{Origin, ReflogFile, WorktreeName};
use lectern_git::Oid;
use lectern_git::testing::{TestHead, TestRepo};
use lectern_services::session::ToolKind;
use lectern_services::usage::RowKey;

use super::attribution::{AT, Log, conn, session_usage, settled, source};
use super::branches::{Repo, batch, oid, watched};
use super::plugin::Rig;
use super::support::*;
use crate::gitread::{DiskGit, GitRead};

const SEC: i64 = 1_000;

fn main() -> WorktreeName {
    WorktreeName::Main
}

fn rows(h: &Harness, id: &str, fake: &crate::testing::FakeSessions) -> Vec<(String, u64)> {
    let _ = id;
    let mut found: Vec<(String, u64)> = session_usage(h, fake, "s")
        .rows
        .iter()
        .map(|r| match &r.key {
            RowKey::Branch { branch, .. } => (branch.name.to_string(), r.session_part.input),
            RowKey::Unassigned { .. } => ("(unassigned)".to_owned(), r.session_part.input),
            RowKey::Mrs { .. } => ("(mrs)".to_owned(), r.session_part.input),
        })
        .collect();
    found.sort();
    found
}

fn one_edit(h: &Harness, at: i64) -> Arc<crate::testing::FakeSessions> {
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", at);
    log.turn("t1", at).usage("t1", "m1", at + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], at + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(h, &fake, "s");
    fake
}

fn segment_names(h: &Harness) -> Vec<(i64, String)> {
    let c = conn(h);
    let mut stmt = c
        .prepare(
            "SELECT s.kind, n.name FROM segment s JOIN segment_name n ON n.segment = s.id
             ORDER BY s.id, n.id",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn observe_a_call_after_the_branch_head_is_on_was_renamed_goes_to_the_new_name() {
    let (h, repo) = watched("refs/heads/old");
    // `git branch -m old new`: HEAD's log, the new branch's log (with the
    // lines it carried over) and the old one removed.
    let rename = "Branch: renamed refs/heads/old to refs/heads/new";
    let on_head = repo.record(
        ReflogFile::Head(main()),
        Some(2),
        2,
        AT + 5 * SEC,
        100,
        None,
        rename,
        Origin::Appended,
    );
    let on_branch = repo.record(
        ReflogFile::Branch(Arc::from(b"refs/heads/new".as_slice())),
        Some(2),
        2,
        AT + 5 * SEC,
        100,
        None,
        rename,
        Origin::NewFile,
    );
    h.reflog(
        &repo,
        vec![on_head, on_branch],
        vec![repo.removed("refs/heads/old", AT + 6 * SEC)],
    );
    let fake = one_edit(&h, AT + 20 * SEC);
    assert_eq!(rows(&h, "s", &fake), [("refs/heads/new".to_owned(), 100)]);
    // No segment that is only the old name was made.
    assert!(segment_names(&h).iter().all(|(kind, _)| *kind != 2));
}

#[test]
fn observe_the_rename_of_another_branch_does_not_move_head() {
    let (h, repo) = watched("refs/heads/feature");
    let rename = "Branch: renamed refs/heads/other to refs/heads/elsewhere";
    let on_head = repo.record(
        ReflogFile::Head(main()),
        Some(2),
        2,
        AT + 5 * SEC,
        100,
        None,
        rename,
        Origin::Appended,
    );
    h.reflog(&repo, vec![on_head], vec![]);
    let fake = one_edit(&h, AT + 20 * SEC);
    assert_eq!(
        rows(&h, "s", &fake),
        [("refs/heads/feature".to_owned(), 100)]
    );
}

#[test]
fn observe_a_rename_seen_only_in_the_branch_log_still_finds_the_segment() {
    let (h, repo) = watched("refs/heads/old");
    let rename = "Branch: renamed refs/heads/old to refs/heads/new";
    let on_branch = repo.record(
        ReflogFile::Branch(Arc::from(b"refs/heads/new".as_slice())),
        Some(2),
        2,
        AT + 5 * SEC,
        100,
        None,
        rename,
        Origin::NewFile,
    );
    h.reflog(&repo, vec![on_branch], vec![]);
    let fake = one_edit(&h, AT + 20 * SEC);
    assert_eq!(rows(&h, "s", &fake), [("refs/heads/new".to_owned(), 100)]);
    assert!(segment_names(&h).iter().all(|(kind, _)| *kind != 2));
}

#[test]
fn segment_a_branch_called_like_a_number_is_a_branch_not_a_detached_head() {
    let (h, repo) = watched("refs/heads/main");
    // `git switch 20261101`: the new commit is not the one the name
    // abbreviates.
    let record = repo.record(
        ReflogFile::Head(main()),
        Some(1),
        2,
        AT,
        100,
        None,
        "checkout: moving from main to 20261101",
        Origin::Appended,
    );
    h.reflog(&repo, vec![record], vec![]);
    let found = segment_names(&h);
    assert!(
        found.contains(&(0, "refs/heads/20261101".to_owned())),
        "{found:?}"
    );
    assert!(found.iter().all(|(kind, _)| *kind != 1), "{found:?}");
    let fake = one_edit(&h, AT + 20 * SEC);
    assert_eq!(
        rows(&h, "s", &fake),
        [("refs/heads/20261101".to_owned(), 100)]
    );
}

#[test]
fn repo_the_baseline_advances_with_the_switches_so_a_replay_starts_late() {
    let (h, repo) = watched("refs/heads/a");
    let mut from = "a";
    for (n, to) in ["b", "a", "b", "a", "b"].into_iter().enumerate() {
        h.reflog(
            &repo,
            vec![repo.switch(&main(), from, to, AT - (50 - 5 * n as i64) * SEC)],
            vec![],
        );
        from = to;
    }
    let baselines: i64 = conn(&h)
        .query_row("SELECT count(*) FROM head_baseline", [], |r| r.get(0))
        .unwrap();
    assert_eq!(baselines, 1 + 5, "one for the watch and one for each batch");
    // The call is after the last switch; one before the last is on `a`.
    let fake = one_edit(&h, AT);
    assert_eq!(rows(&h, "s", &fake), [("refs/heads/b".to_owned(), 100)]);
}

#[test]
fn repo_a_batch_that_could_not_be_written_makes_the_calls_around_it_unsure() {
    let (h, repo) = watched("refs/heads/feature");
    let lost = batch(
        vec![repo.switch(&main(), "feature", "other", AT + 5 * SEC)],
        vec![],
    );
    h.store().batch_lost(&repo.common, &lost).unwrap();
    let fake = one_edit(&h, AT + 5 * SEC);
    let s = session_usage(&h, &fake, "s");
    assert!(matches!(
        &s.rows[0].key,
        RowKey::Unassigned { unattributable } if unattributable.input == 100
    ));
}

/// A command from `AT + 2 s` to `AT + 30 s` in `/work/app`, after an
/// uncommitted edit of `src/a.rs`.
fn long_command(fake: &Arc<crate::testing::FakeSessions>, h: &Harness) {
    let mut log = Log::new(fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.tool(
        "t1",
        "c2",
        ToolKind::Execute {
            command: "git checkout -b x && npm test && git checkout main".into(),
            workdir: Arc::from(Path::new("/work/app")),
            exec_session: None,
            workdir_exact: true,
        },
        AT + 2 * SEC,
        AT + 30 * SEC,
    );
    log.write();
    h.read_every(fake);
    settled(h, fake, "s");
}

#[test]
fn observe_a_call_across_two_switch_batches_sees_both_switches() {
    let (h, repo) = watched("refs/heads/feature");
    // The two checkouts of one command are read in two batches; the first
    // one moves the baseline.
    h.reflog(
        &repo,
        vec![repo.switch(&main(), "feature", "x", AT + 5 * SEC)],
        vec![],
    );
    h.reflog(
        &repo,
        vec![repo.switch(&main(), "x", "main", AT + 20 * SEC)],
        vec![],
    );
    let fake = source();
    long_command(&fake, &h);
    // The uncommitted edit goes where HEAD ended: the default branch. With
    // only the first batch it would be `x`.
    assert_eq!(rows(&h, "s", &fake), [("(unassigned)".to_owned(), 100)]);
    // The call kept: it moved HEAD, through three branches.
    let moved: i64 = conn(&h)
        .query_row(
            "SELECT count(*) FROM call_obs WHERE stays IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(moved, 1);
}

#[test]
fn observe_a_commit_in_a_later_batch_of_the_call_keeps_its_file_on_that_branch() {
    let (h, repo) = watched("refs/heads/feature");
    h.reflog(
        &repo,
        vec![repo.switch(&main(), "feature", "x", AT + 5 * SEC)],
        vec![],
    );
    // Second batch: HEAD goes on to `y`, where a commit holds the file; the
    // third: back to the default branch.
    let new = oid(9);
    h.git.set_commit(&hex_bytes(&new), &["src/a.rs"]);
    h.reflog(
        &repo,
        vec![
            repo.switch(&main(), "x", "y", AT + 12 * SEC),
            repo.record_hex(
                ReflogFile::Head(main()),
                &oid(1),
                &new,
                AT + 14 * SEC,
                100,
                None,
                "commit: work",
                Origin::Appended,
            ),
        ],
        vec![],
    );
    h.reflog(
        &repo,
        vec![repo.switch(&main(), "y", "main", AT + 20 * SEC)],
        vec![],
    );
    let fake = source();
    long_command(&fake, &h);
    assert_eq!(rows(&h, "s", &fake), [("refs/heads/y".to_owned(), 100)]);
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

#[test]
fn repo_a_lost_batch_makes_every_later_call_unsure_until_the_baseline_moves() {
    let (h, repo) = watched("refs/heads/feature");
    let lost = batch(
        vec![repo.switch(&main(), "feature", "other", AT + 5 * SEC)],
        vec![],
    );
    h.store().batch_lost(&repo.common, &lost).unwrap();
    // A call a minute later, far from the gap.
    let fake = one_edit(&h, AT + 60 * SEC);
    assert!(matches!(
        &session_usage(&h, &fake, "s").rows[0].key,
        RowKey::Unassigned { unattributable } if unattributable.input == 100
    ));
}

fn heads_of(key: &str, branch: &str) -> Vec<crate::gitread::WorktreeHead> {
    vec![crate::gitread::WorktreeHead {
        key: PathBuf::from(key),
        branch: Some(branch.to_owned()),
        rebasing: false,
    }]
}

#[test]
fn repo_a_head_read_after_a_rewritten_log_lets_later_calls_be_attributed_again() {
    let (h, repo) = watched("refs/heads/feature");
    // `reflog expire` rewrote HEAD's log at AT + 5 s; HEAD was read again at
    // AT + 10 s (feature).
    h.reflog(&repo, vec![], vec![repo.rewritten(&main(), AT + 5 * SEC)]);
    let keys = crate::store::repo::gap_worktrees(
        &repo.common,
        &batch(vec![], vec![repo.rewritten(&main(), AT + 5 * SEC)]),
        false,
    );
    h.store()
        .rebaseline(
            &repo.common,
            (AT + 10 * SEC) * 1_000_000,
            &heads_of("/work/app/.git", "refs/heads/feature"),
            &keys,
        )
        .unwrap();
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    // t1's edit spans the reset; t2's is after the read; t3's is between the
    // reset and the read.
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 4_500, 3);
    log.turn("t2", AT + 60 * SEC)
        .usage("t2", "m2", AT + 60 * SEC + 10, 50);
    log.edit("t2", "c2", &["/work/app/src/a.rs"], AT + 60 * SEC + 20, 3);
    log.turn("t3", AT + 120 * SEC)
        .usage("t3", "m3", AT + 120 * SEC + 10, 20);
    log.edit("t3", "c3", &["/work/app/src/a.rs"], AT + 120 * SEC + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let s = session_usage(&h, &fake, "s");
    let mut found: Vec<(String, u64, u64)> = s
        .rows
        .iter()
        .map(|r| match &r.key {
            RowKey::Branch { branch, .. } => (branch.name.to_string(), r.session_part.input, 0),
            RowKey::Unassigned { unattributable } => (
                "(unassigned)".to_owned(),
                r.session_part.input,
                unattributable.input,
            ),
            RowKey::Mrs { .. } => unreachable!(),
        })
        .collect();
    found.sort();
    // t1 spans the reset: unsure. t2 and t3 are after the read: `feature`.
    assert_eq!(
        found,
        [
            ("(unassigned)".to_owned(), 100, 100),
            ("refs/heads/feature".to_owned(), 70, 0)
        ]
    );
}

/// A repository directory that exists, opened as a project, with the watch
/// armed on `feature`.
pub(super) fn rig_with_repo() -> (Rig, tempfile::TempDir, Repo, Arc<crate::testing::FakeGit>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let common = root.join(".git");
    let reads = Arc::new(crate::testing::FakeGit::new());
    reads.set_heads(&common, &[(&common, Some("refs/heads/feature"))]);
    let setup = |p: &crate::testing::FakeProjects| {
        let key = p.repo(&root, &common);
        p.set_open(&key, true);
    };
    let rig = Rig::with_git(
        crate::testing::FakeSessions::new(
            "session-claude-code",
            lectern_services::session::Agent::ClaudeCode,
        ),
        setup,
        reads.clone(),
    );
    rig.settle();
    let repo = Repo::new(common.to_str().unwrap());
    (rig, dir, repo, reads)
}

pub(super) fn baselines(rig: &Rig) -> i64 {
    rusqlite::Connection::open(rig.db())
        .unwrap()
        .query_row("SELECT count(*) FROM head_baseline", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn plugin_reads_head_again_after_a_rewritten_log_and_keeps_trying_when_it_cannot() {
    let (rig, _dir, repo, reads) = rig_with_repo();
    let common = repo.common.clone();
    assert_eq!(baselines(&rig), 1);
    let now = rig.time.now_ms();
    // A rewrite: HEAD is read again, once, however many resets there are.
    let b = batch(
        vec![],
        vec![
            repo.rewritten(&main(), now + 1_000),
            repo.rewritten(&main(), now + 1_001),
        ],
    );
    rig.engine.on_repo_batch(&common, b, None);
    rig.settle();
    assert_eq!(baselines(&rig), 2);
    // The next rewrite: the repository cannot be read, the gap stays.
    reads.clear_heads(&common);
    rig.engine.on_repo_batch(
        &common,
        batch(vec![], vec![repo.rewritten(&main(), now + 2_000)]),
        None,
    );
    rig.settle();
    assert_eq!(baselines(&rig), 2);
    // Readable again: the next batch of the repository, of any kind, reads
    // HEAD and ends the gap.
    reads.set_heads(&common, &[(&common, Some("refs/heads/feature"))]);
    rig.engine.on_repo_batch(
        &common,
        batch(vec![repo.commit(&main(), 1, 2, now + 3_000)], vec![]),
        None,
    );
    rig.settle();
    assert_eq!(baselines(&rig), 3);
    // Nothing is waiting any more: another batch adds no baseline.
    rig.engine.on_repo_batch(
        &common,
        batch(vec![repo.commit(&main(), 2, 3, now + 4_000)], vec![]),
        None,
    );
    rig.settle();
    assert_eq!(baselines(&rig), 3);
}

#[test]
fn plugin_reads_head_again_after_a_batch_that_could_not_be_written() {
    let (rig, _dir, repo, _reads) = rig_with_repo();
    let common = repo.common.clone();
    let now = rig.time.now_ms();
    // The records cannot be written (the table is gone): the batch is lost,
    // and HEAD is read again.
    rusqlite::Connection::open(rig.db())
        .unwrap()
        .execute_batch("DROP TABLE reflog;")
        .unwrap();
    rig.engine.on_repo_batch(
        &common,
        batch(vec![repo.commit(&main(), 1, 2, now + 1_000)], vec![]),
        None,
    );
    rig.settle();
    let c = rusqlite::Connection::open(rig.db()).unwrap();
    let resets: i64 = c
        .query_row("SELECT count(*) FROM reflog_reset", [], |r| r.get(0))
        .unwrap();
    assert_eq!((resets, baselines(&rig)), (1, 2));
}

// ---- DiskGit on repositories written to disk ----

fn hex(o: &Oid) -> String {
    o.to_hex()
}

fn log_line(old: Option<&Oid>, new: &Oid, secs: i64, message: &str) -> String {
    let old = old.map_or("0".repeat(40), hex);
    format!(
        "{old} {} Test <t@example.org> {secs} +0000\t{message}\n",
        hex(new)
    )
}

fn write_log(t: &TestRepo, reference: &str, text: &str) {
    let path = t.common_dir().join("logs").join(reference);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Two commits (the second changes `src/a.rs` and adds `src/b.rs`), a
/// branch `feature` at the second, packed `main` at the first.
fn two_commits() -> (TestRepo, Oid, Oid) {
    let t = TestRepo::new();
    let one = t.tree(&[("src/a.rs", b"1\n")]);
    let c1 = t.commit(one, &[], "one");
    let two = t.tree(&[("src/a.rs", b"2\n"), ("src/b.rs", b"x\n")]);
    let c2 = t.commit(two, &[c1], "two");
    t.set_packed_refs(&[("refs/heads/main", c1)]);
    t.set_ref("refs/heads/feature", c2);
    t.set_head(TestHead::Branch("refs/heads/feature"));
    (t, c1, c2)
}

#[test]
fn disk_git_heads_lists_the_main_and_the_linked_worktrees() {
    let (t, c1, _) = two_commits();
    let wt = t.base().join("linked");
    let admin = t.add_worktree("linked", &wt, TestHead::Detached(c1));
    let heads = DiskGit::new().heads(t.common_dir()).unwrap();
    assert_eq!(heads.len(), 2, "{heads:?}");
    let main = heads.iter().find(|h| h.key == t.common_dir()).unwrap();
    assert_eq!(main.branch.as_deref(), Some("refs/heads/feature"));
    let linked = heads.iter().find(|h| h.key == admin).unwrap();
    assert_eq!(linked.branch, None, "detached");
}

#[test]
fn disk_git_default_branch_from_packed_refs_and_from_the_remote() {
    let (t, c1, _) = two_commits();
    let disk = DiskGit::new();
    // `main` is only in `packed-refs`.
    assert_eq!(
        disk.default_branch(t.common_dir()).as_deref(),
        Some("refs/heads/main")
    );
    // The remote's HEAD names another branch: its local name.
    t.append_config("[remote \"origin\"]\n\turl = https://example.org/x.git\n");
    t.set_ref("refs/remotes/origin/trunk", c1);
    t.set_symbolic_ref("refs/remotes/origin/HEAD", "refs/remotes/origin/trunk");
    let disk = DiskGit::new();
    assert_eq!(
        disk.default_branch(t.common_dir()).as_deref(),
        Some("refs/heads/trunk")
    );
}

#[test]
fn disk_git_branch_log_gives_the_creation_and_the_renames() {
    let (t, c1, c2) = two_commits();
    let disk = DiskGit::new();
    assert!(
        !disk
            .branch_log(t.common_dir(), "refs/heads/feature")
            .unwrap()
            .exists
    );
    write_log(
        &t,
        "refs/heads/feature",
        &(log_line(None, &c1, 1_700_000_100, "branch: Created from HEAD")
            + &log_line(Some(&c1), &c2, 1_700_000_200, "commit: two")
            + &log_line(
                Some(&c2),
                &c2,
                1_700_000_300,
                "Branch: renamed refs/heads/old to refs/heads/feature",
            )),
    );
    let log = disk
        .branch_log(t.common_dir(), "refs/heads/feature")
        .unwrap();
    assert!(log.exists);
    assert_eq!(log.created, Some(1_700_000_100_000));
    assert_eq!(
        log.renames,
        [(
            1_700_000_300_000,
            "refs/heads/old".to_owned(),
            "refs/heads/feature".to_owned()
        )]
    );
    // A log that does not start with the creation (cut or expired).
    write_log(
        &t,
        "refs/heads/feature",
        &log_line(Some(&c1), &c2, 1_700_000_200, "commit: two"),
    );
    let log = disk
        .branch_log(t.common_dir(), "refs/heads/feature")
        .unwrap();
    assert!(log.exists && log.created.is_none());
}

#[test]
fn disk_git_commit_paths_are_what_the_commit_changed() {
    let (t, c1, c2) = two_commits();
    let disk = DiskGit::new();
    let paths = disk
        .commit_paths(t.common_dir(), Some(c1.as_bytes()), c2.as_bytes())
        .unwrap();
    assert_eq!(paths, ["src/a.rs", "src/b.rs"]);
    // The first commit: everything in it.
    let paths = disk
        .commit_paths(t.common_dir(), None, c1.as_bytes())
        .unwrap();
    assert_eq!(paths, ["src/a.rs"]);
    // An object that is not there: unknown, not an error.
    assert!(
        disk.commit_paths(t.common_dir(), None, &[7u8; 20])
            .is_none()
    );
    // Stopped: the walk ends early.
    disk.stop();
    assert!(
        disk.commit_paths(t.common_dir(), Some(c1.as_bytes()), c2.as_bytes())
            .is_none()
    );
}

/// A commit, a switch, a rename and a checkout, from a repository on disk
/// through ingest and settle: the branch each turn's edit is attributed to.
#[test]
fn disk_git_commit_switch_rename_and_checkout_are_attributed_to_the_right_branch() {
    let (t, c1, c2) = two_commits();
    let common: PathBuf = t.common_dir().to_path_buf();
    let root: PathBuf = t.root().to_path_buf();
    let mut h = Harness::new();
    let key = h.projects.repo(&root, &common);
    h.open_project(&key);
    let disk = DiskGit::new();
    let heads = disk.heads(&common).unwrap();
    let logs: Vec<_> = heads
        .iter()
        .filter_map(|w| w.branch.clone())
        .filter_map(|b| disk.branch_log(&common, &b).map(|l| (b, l)))
        .collect();
    h.store()
        .arm(&crate::store::Arm {
            common_dir: &common,
            at_ns: (AT - 60 * SEC) * 1_000_000,
            heads: &heads,
            logs: &logs,
        })
        .unwrap();
    let repo = Repo::new(common.to_str().unwrap());
    let (x1, x2) = (hex(&c1), hex(&c2));
    let head = |old: &str, new: &str, at: i64, message: &str| {
        repo.record_hex(
            ReflogFile::Head(main()),
            old,
            new,
            at,
            100,
            None,
            message,
            Origin::Appended,
        )
    };
    // t1: an edit of src/a.rs, then one command during which the commit
    // that holds the edit is made on `feature` and HEAD goes to `main`.
    // t2: HEAD goes back, and `feature` is renamed to `topic`; the edit
    // after is on `topic`. t3: HEAD on `main` (the default branch).
    let rename = "Branch: renamed refs/heads/feature to refs/heads/topic";
    h.reflog(
        &repo,
        vec![
            head(&x1, &x2, AT + 4 * SEC, "commit: two"),
            head(
                &x2,
                &x1,
                AT + 6 * SEC,
                "checkout: moving from feature to main",
            ),
            head(
                &x1,
                &x2,
                AT + 30 * SEC,
                "checkout: moving from main to feature",
            ),
            head(&x2, &x2, AT + 40 * SEC, rename),
            head(
                &x2,
                &x1,
                AT + 100 * SEC,
                "checkout: moving from topic to main",
            ),
        ],
        vec![],
    );
    h.git.set_commit(c2.as_bytes(), &["src/a.rs"]);
    let a = root.join("src/a.rs");
    let a = a.to_str().unwrap();
    let fake = source();
    let cwd = root.to_str().unwrap();
    let mut log = Log::new(&fake, "s", cwd, AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &[a], AT + 20, 3);
    log.tool(
        "t1",
        "c2",
        ToolKind::Execute {
            command: "git commit -am two && git switch main".into(),
            workdir: Arc::from(Path::new(cwd)),
            exec_session: None,
            workdir_exact: true,
        },
        AT + 3 * SEC,
        AT + 8 * SEC,
    );
    log.turn("t2", AT + 60 * SEC)
        .usage("t2", "m2", AT + 60 * SEC + 10, 50);
    log.edit("t2", "c3", &[a], AT + 60 * SEC + 20, 3);
    log.turn("t3", AT + 120 * SEC)
        .usage("t3", "m3", AT + 120 * SEC + 10, 20);
    log.edit("t3", "c4", &[a], AT + 120 * SEC + 20, 3);
    log.write();
    h.read_every(&fake);
    // Settled with the real repository's reads.
    let key = fake.key("s");
    let summary = lectern_services::session::SessionSource::sessions(&*fake)
        .get(&key)
        .cloned()
        .unwrap();
    let resolve = |d: &Path| crate::paths::dir_place(Some(&h.projects), d);
    for _ in 0..10 {
        let done = crate::finalize::settle(&crate::finalize::Settling {
            store: h.store(),
            source: &*fake,
            key: &key,
            now_ms: LATER_MS,
            end_open: !summary.open_turn,
            resolve: &resolve,
            reads: &disk,
            turns: crate::finalize::BATCH_TURNS,
            time: crate::finalize::BATCH_TIME,
        })
        .unwrap();
        if !done.more {
            break;
        }
    }
    let found = rows(&h, "s", &fake);
    assert_eq!(
        found,
        [
            // t3 is on the default branch. t1 (the commit made during its
            // command holds the file; HEAD went on to `main`, which would
            // have taken an uncommitted file) and t2 are on `feature`,
            // which `topic` is now called: one segment.
            ("(unassigned)".to_owned(), 20),
            ("refs/heads/topic".to_owned(), 150),
        ],
        "{found:?}"
    );
}

const LATER_MS: i64 = AT + 2 * 24 * 3_600 * SEC;
