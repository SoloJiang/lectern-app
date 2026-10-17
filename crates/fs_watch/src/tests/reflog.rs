//! Reflog reading (`reflog_*`), docs/design/fs_watch.md「reflog」.

use std::fs;
use std::sync::Arc;

use insta::assert_snapshot;
use lectern_executor::Executor;

use super::support::*;
use crate::testing::{FakeBackend, RecordingSink, manual};
use crate::{
    Coalesce, Config, Flags, Origin, ReflogFile, ReflogRecord, ReflogReset, RepoBatch, RepoSpec,
    RunId, WorktreeName,
};

const MODIFIED: Flags = Flags::MODIFIED;

fn line(n: usize) -> String {
    format!(
        "{n:040} {:040} A U Thor <a@example.com> 1700000000 +0000\tcommit: {n}\n",
        n + 1
    )
}

/// Records and resets of the batches, in order.
fn all(batches: &[(RepoBatch, bool)]) -> (Vec<ReflogRecord>, Vec<ReflogReset>) {
    let mut records = Vec::new();
    let mut resets = Vec::new();
    for (batch, _) in batches {
        records.extend(batch.reflog.iter().cloned());
        resets.extend(batch.reflog_resets.iter().cloned());
    }
    (records, resets)
}

/// The order rules of「reflog」, as `usage` applies them: `Some(true)` when A
/// is certainly earlier than B. `run_order` gives the run sequence number
/// `usage` assigns.
fn earlier(
    a: &ReflogRecord,
    b: &ReflogRecord,
    resets: &[ReflogReset],
    run_order: &dyn Fn(RunId) -> u64,
) -> bool {
    if a.observed.run == b.observed.run {
        let no_reset_between = !resets.iter().any(|reset| {
            reset.observed.run == a.observed.run
                && reset.file == a.file
                && reset.seq > a.seq.min(b.seq)
                && reset.seq < a.seq.max(b.seq)
        });
        (a.file == b.file && a.offset < b.offset && no_reset_between)
            || b.after.is_some_and(|after| a.observed.read < after.read)
    } else {
        run_order(a.observed.run) < run_order(b.observed.run) && b.after.is_some()
    }
}

#[test]
fn reflog_append_gives_one_appended_record() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, line(0).as_bytes());
    let o = fx.opened();
    append(&head, line(1).as_bytes());
    fx.emit_common(&[("logs/HEAD", F | MODIFIED)]);
    fx.settle();
    fx.wait();
    let batches = o.repo_sink.take();
    assert_snapshot!(repo_batches(&batches), @r#"
    worktrees=[] []
      seq=0 "HEAD" offset=134 "0000000000000000000000000000000000000001 0000000000000000000000000000000000000002 A U Thor <a@example.com> 1700000000 +0000\tcommit: 1" observed=r2 after=r1 Appended
    "#);
    let record = &batches[0].0.reflog[0];
    assert_eq!(record.file, ReflogFile::Head(WorktreeName::Main));
    // The fake wall clock: the read ended no earlier than it started.
    assert!(record.observed.at >= record.after.unwrap().at);
}

#[test]
fn reflog_interleaved_files_order_where_the_rules_allow() {
    let fx = Fixture::new();
    let a = fx.common.join("logs/refs/heads/a");
    let b = fx.common.join("logs/refs/heads/b");
    write(&a, b"");
    write(&b, b"");
    let o = fx.opened();
    // The true write order: a0 b0 a1 (read) b1 a2 (read) b2 (read).
    let mut order = Vec::new();
    for (file, text) in [(&a, "a0"), (&b, "b0"), (&a, "a1")] {
        append(file, format!("{text}\n").as_bytes());
        order.push(text.to_owned());
    }
    fx.emit_common(&[("logs/refs/heads/a", F), ("logs/refs/heads/b", F)]);
    fx.settle();
    for (file, text) in [(&b, "b1"), (&a, "a2")] {
        append(file, format!("{text}\n").as_bytes());
        order.push(text.to_owned());
    }
    fx.emit_common(&[("logs/refs/heads/a", F), ("logs/refs/heads/b", F)]);
    fx.settle();
    append(&b, b"b2\n");
    order.push("b2".to_owned());
    fx.emit_common(&[("logs/refs/heads/b", F)]);
    fx.settle();
    fx.wait();
    let (records, resets) = all(&o.repo_sink.take());
    assert_eq!(records.len(), 6);
    let position = |record: &ReflogRecord| {
        let text = String::from_utf8_lossy(&record.line).into_owned();
        order.iter().position(|t| *t == text).unwrap()
    };
    let run = records[0].observed.run;
    let run_order = move |id: RunId| u64::from(id != run);
    let mut decided = 0;
    let mut undecided = 0;
    for x in &records {
        for y in &records {
            if std::ptr::eq(x, y) {
                continue;
            }
            if earlier(x, y, &resets, &run_order) {
                assert!(
                    position(x) < position(y),
                    "{:?} before {:?}",
                    x.line,
                    y.line
                );
                decided += 1;
            } else if !earlier(y, x, &resets, &run_order) {
                undecided += 1;
            }
        }
    }
    assert!(decided > 0);
    // a0 and b0 were read by the same pair of reads: no rule orders them.
    assert!(undecided > 0);
}

#[test]
fn reflog_incomplete_line_waits_for_the_newline() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, b"");
    let o = fx.opened();
    append(&head, b"partial");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    fx.wait();
    assert!(o.repo_sink.take().is_empty());
    append(&head, b" line\nnext");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(repo_batches(&o.repo_sink.take()), @r#"
    worktrees=[] []
      seq=0 "HEAD" offset=0 "partial line" observed=r3 after=r2 Appended
    "#);
}

#[test]
fn reflog_shorter_or_rewritten_tail_is_rewritten() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, b"one\ntwo\nthree\n");
    let o = fx.opened();
    // `reflog expire`: shorter.
    write(&head, b"one\n");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    // Same length as read so far, different end: `reflog delete`.
    write(&head, b"ONE\n");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    append(&head, b"four\n");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(repo_batches(&o.repo_sink.take()), @r#"
    worktrees=[] []
      seq=0 "HEAD" reset Rewritten observed=r2
      seq=1 "HEAD" reset Rewritten observed=r3
      seq=2 "HEAD" offset=4 "four" observed=r4 after=r3 Appended
    "#);
}

#[test]
fn reflog_deleted_file_is_removed() {
    let fx = Fixture::new();
    let branch = fx.common.join("logs/refs/heads/topic");
    write(&branch, b"x\n");
    let o = fx.opened();
    fs::remove_file(&branch).unwrap();
    fx.emit_common(&[("logs/refs/heads/topic", F | Flags::REMOVED)]);
    fx.settle();
    // A second event for the gone file reports nothing more.
    fx.emit_common(&[("logs/refs/heads/topic", F | Flags::REMOVED)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(repo_batches(&o.repo_sink.take()), @r#"
    worktrees=[] []
      seq=0 "refs/heads/topic" reset Removed observed=r2
    "#);
}

#[test]
fn reflog_new_and_renamed_branches_are_new_files() {
    let fx = Fixture::new();
    let old = fx.common.join("logs/refs/heads/old");
    write(&old, b"o1\no2\n");
    let o = fx.opened();
    write(&fx.common.join("logs/refs/heads/feature/x"), b"f1\n");
    fx.emit_common(&[("logs/refs/heads/feature", D | Flags::CREATED)]);
    fx.settle();
    // `git branch -m old new`: the reflog moves.
    fs::rename(&old, fx.common.join("logs/refs/heads/new")).unwrap();
    fx.emit_common(&[
        ("logs/refs/heads/old", F | Flags::RENAMED),
        ("logs/refs/heads/new", F | Flags::RENAMED),
    ]);
    fx.settle();
    fx.wait();
    assert_snapshot!(repo_batches(&o.repo_sink.take()), @r#"
    worktrees=[] []
      seq=0 "refs/heads/feature/x" offset=0 "f1" observed=r2 after=- NewFile
      seq=1 "refs/heads/new" offset=0 "o1" observed=r3 after=- NewFile
      seq=2 "refs/heads/new" offset=3 "o2" observed=r3 after=- NewFile
      seq=3 "refs/heads/old" reset Removed observed=r4
    "#);
}

#[test]
fn reflog_linked_worktree_head_and_stash() {
    let fx = Fixture::new();
    let o = fx.opened();
    write(&fx.common.join("worktrees/wt/logs/HEAD"), b"w1\n");
    fx.emit_common(&[("worktrees/wt", D | Flags::CREATED)]);
    fx.settle();
    write(&fx.common.join("logs/refs/stash"), b"s1\n");
    fx.emit_common(&[("logs/refs/stash", F | Flags::CREATED)]);
    fx.settle();
    fx.wait();
    let batches = o.repo_sink.take();
    let (records, _) = all(&batches);
    assert_eq!(
        records[0].file,
        ReflogFile::Head(WorktreeName::Linked("wt".into()))
    );
    assert_eq!(records[1].file, ReflogFile::Stash);
    assert_snapshot!(repo_batches(&batches), @r#"
    worktrees=[] ["worktree_list"]
      seq=0 "HEAD(wt)" offset=0 "w1" observed=r0 after=- NewFile
      seq=1 "stash" offset=0 "s1" observed=r1 after=- NewFile
    "#);
}

#[test]
fn reflog_large_appends_are_split_into_batches_of_one_mib() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, line(0).as_bytes());
    let o = fx.opened();
    // About 1.4 MiB appended at once.
    let appended: String = (1..=11_000).map(line).collect();
    append(&head, appended.as_bytes());
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    fx.wait();
    let batches = o.repo_sink.take();
    assert_eq!(batches.len(), 2);
    let mut seq = 0;
    let after = batches[0].0.reflog[0].after;
    for (batch, _) in &batches {
        let bytes: usize = batch.reflog.iter().map(|record| record.line.len()).sum();
        assert!(bytes <= 1 << 20);
        for record in &batch.reflog {
            assert_eq!(record.seq, seq);
            seq += 1;
            assert_eq!(
                record.after, after,
                "the rest reuses the first part's `after`"
            );
            assert_eq!(record.origin, Origin::Appended);
        }
    }
    assert_eq!(seq, 11_000);

    // A renamed 3 MiB branch: read from the start, in parts.
    let big: String = (0..23_500).map(line).collect();
    write(&fx.base.join("big"), big.as_bytes());
    fs::rename(fx.base.join("big"), fx.common.join("logs/refs/heads/big")).unwrap();
    fx.emit_common(&[("logs/refs/heads/big", F | Flags::RENAMED)]);
    fx.settle();
    fx.wait();
    let batches = o.repo_sink.take();
    // Each read takes the whole lines that fit in 1 MiB.
    let per_read = (1 << 20) / line(0).len();
    assert_eq!(batches.len(), 23_500usize.div_ceil(per_read));
    let mut offset = 0;
    for (batch, _) in &batches {
        let bytes: usize = batch.reflog.iter().map(|record| record.line.len()).sum();
        assert!(bytes <= 1 << 20);
        for record in &batch.reflog {
            assert_eq!(record.seq, seq);
            assert_eq!(record.offset, offset);
            assert_eq!(record.after, None);
            assert_eq!(record.origin, Origin::NewFile);
            seq += 1;
            offset += record.line.len() as u64 + 1;
        }
    }
    assert_eq!(offset, big.len() as u64);
}

#[test]
fn reflog_append_reset_append_in_one_batch_orders_by_seq() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, b"a\n");
    let o = fx.opened();
    append(&head, b"b\n");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    write(&head, b"x\n");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    append(&head, b"c\n");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    fx.wait();
    let batches = o.repo_sink.take();
    assert_eq!(batches.len(), 1, "within one window");
    assert_snapshot!(repo_batches(&batches), @r#"
    worktrees=[] []
      seq=0 "HEAD" offset=2 "b" observed=r2 after=r1 Appended
      seq=1 "HEAD" reset Rewritten observed=r3
      seq=2 "HEAD" offset=2 "c" observed=r4 after=r3 Appended
    "#);
    // The two records have the same offset but a reset in between: the
    // offset rule does not apply; `after` still orders them.
    let (records, resets) = all(&batches);
    let run = records[0].observed.run;
    let run_order = move |id: RunId| u64::from(id != run);
    assert!(earlier(&records[0], &records[1], &resets, &run_order));
    let mut forged = records[1].clone();
    forged.after = None;
    forged.offset = 10;
    assert!(
        !earlier(&records[0], &forged, &resets, &run_order),
        "offsets across a reset are not compared"
    );
}

#[test]
fn reflog_long_lines_are_skipped() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, b"");
    let o = fx.opened();
    let long = "x".repeat(70 << 10);
    append(&head, format!("ok1\n{long}\nok2\n").as_bytes());
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    fx.wait();
    let (records, _) = all(&o.repo_sink.take());
    let lines: Vec<String> = records
        .iter()
        .map(|record| String::from_utf8_lossy(&record.line).into_owned())
        .collect();
    assert_eq!(lines, ["ok1", "ok2"]);
    assert_eq!(records[1].offset, (4 + long.len() + 1) as u64);
}

#[test]
fn reflog_records_before_arm_are_not_delivered() {
    let fx = Fixture::new();
    write(&fx.common.join("logs/HEAD"), b"h1\nh2\n");
    write(&fx.common.join("logs/refs/heads/main"), b"m1\n");
    write(&fx.common.join("logs/refs/stash"), b"s1\n");
    let (_repo, sink) = fx.repo();
    fx.settle();
    let batches = sink.take();
    assert_eq!(batches.len(), 1);
    assert!(batches[0].0.reflog.is_empty());
    assert!(batches[0].0.rescan);
}

#[test]
fn reflog_lines_between_the_baseline_and_arm_are_delivered_once() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, b"old\n");
    let written = head.clone();
    fx.backend
        .before_arm(&fx.common, move || append(&written, b"during\n"));
    // The backend also reports the write once the watch exists.
    fx.backend
        .emit_on_arm(&fx.common, &[("logs/HEAD", F | MODIFIED)]);
    let (_repo, sink) = fx.repo();
    fx.settle();
    fx.wait();
    assert_snapshot!(repo_batches(&sink.take()), @r#"
    worktrees=[] ["rescan"] health=Live
      seq=0 "HEAD" offset=4 "during" observed=r1 after=r0 Appended
    "#);
}

#[test]
fn reflog_runs_are_compared_only_through_after() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, b"an older line\n");
    let o = fx.opened();
    append(&head, b"first run, a long line\n");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    fx.wait();
    let (first, _) = all(&o.repo_sink.take());
    drop(o);
    fx.watcher.shutdown();

    // Lectern is closed; `reflog expire` makes the file shorter.
    write(&head, b"x\n");
    write(&fx.common.join("logs/refs/heads/renamed"), b"carried\n");
    let backend = FakeBackend::new();
    let executor = Executor::manual();
    let (watcher, driver) = manual(backend.clone(), executor, Config::default());
    let sink = RecordingSink::new(None);
    let _repo = watcher.watch_repo(
        RepoSpec {
            common_dir: fx.common.clone(),
            coalesce: Coalesce::REPO,
        },
        sink.clone(),
    );
    driver.run_until_idle();
    sink.take();
    append(&head, b"second\n");
    fs::rename(
        fx.common.join("logs/refs/heads/renamed"),
        fx.common.join("logs/refs/heads/moved"),
    )
    .unwrap();
    backend.emit(
        &fx.common,
        &[
            ("logs/HEAD", F),
            ("logs/refs/heads/renamed", F),
            ("logs/refs/heads/moved", F),
        ],
    );
    driver.run_until_idle();
    driver.advance(LONG);
    let batches = sink.take();
    let (second, resets) = all(&batches);
    assert_ne!(first[0].observed.run, second[0].observed.run);
    // Read numbers restart in every run (baseline and catch-up of two
    // files, then the event's reads).
    assert!(second[0].observed.read < 8);
    let first_run = first[0].observed.run;
    let run_order = move |id: RunId| u64::from(id != first_run);
    let appended = second
        .iter()
        .find(|r| r.origin == Origin::Appended)
        .unwrap();
    let carried = second.iter().find(|r| r.origin == Origin::NewFile).unwrap();
    assert!(appended.offset < first[0].offset);
    assert!(earlier(&first[0], appended, &resets, &run_order));
    assert!(
        !earlier(appended, &first[0], &resets, &run_order),
        "offsets are not compared across runs"
    );
    assert!(
        !earlier(&first[0], carried, &resets, &run_order),
        "a NewFile record is not ordered across runs"
    );
}

#[test]
fn reflog_reftable_repository_has_no_records() {
    let fx = Fixture::new();
    write(&fx.common.join("reftable/tables.list"), b"0x01.ref\n");
    write(&fx.common.join("logs/HEAD"), b"");
    let o = fx.opened();
    append(&fx.common.join("logs/HEAD"), b"ignored\n");
    fx.emit_common(&[("logs/HEAD", F), ("reftable/0x02.ref", F)]);
    fx.settle();
    fx.wait();
    let batches = o.repo_sink.take();
    assert_snapshot!(repo_batches(&batches), @r#"worktrees=[] ["refs"]"#);
}

#[test]
fn reflog_after_a_common_rescan_records_are_delivered() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, b"a\n");
    let o = fx.opened();
    append(&head, b"b\n");
    write(&fx.common.join("logs/refs/heads/new"), b"n\n");
    fx.emit_common(&[("", Flags::RESCAN | Flags::DROPPED)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(repo_batches(&o.repo_sink.take()), @r#"
    worktrees=[] ["rescan"]
      seq=0 "HEAD" offset=2 "b" observed=r2 after=r1 Appended
      seq=1 "refs/heads/new" offset=0 "n" observed=r3 after=- NewFile
    "#);
    append(&head, b"c\n");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    fx.wait();
    let (records, _) = all(&o.repo_sink.take());
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].after.unwrap().read,
        2,
        "the rescan read is the previous read"
    );
}

#[test]
fn reflog_symlink_is_unreadable_and_retried() {
    let fx = Fixture::new();
    let head = fx.common.join("logs/HEAD");
    write(&head, b"a\n");
    let o = fx.opened();
    fs::remove_file(&head).unwrap();
    write(&fx.base.join("elsewhere"), b"a\nb\n");
    std::os::unix::fs::symlink(fx.base.join("elsewhere"), &head).unwrap();
    fx.emit_common(&[("logs/HEAD", Flags::SYMLINK)]);
    fx.settle();
    fs::remove_file(&head).unwrap();
    write(&head, b"a\nb\n");
    fx.emit_common(&[("logs/HEAD", F)]);
    fx.settle();
    fx.wait();
    assert_snapshot!(repo_batches(&o.repo_sink.take()), @r#"
    worktrees=[] []
      seq=0 "HEAD" reset Unreadable("not a regular file") observed=r2
      seq=1 "HEAD" offset=2 "b" observed=r3 after=r1 Appended
    "#);
    let _ = Arc::clone(&o.repo_sink);
}
