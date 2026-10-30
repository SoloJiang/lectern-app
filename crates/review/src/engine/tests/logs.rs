//! Log records of the review engine and the annotation log (`logs_*`,
//! docs/design/review.md「日志与诊断」): the warn paths write their records
//! with the review's `review` and `session`, and no file contents, session
//! contents, annotations or commit messages reach a record.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

use lectern_logging::testing::{self, Capture};

use super::*;
use crate::log::{BaseRecord, Limits, Op, READ_MAX, Record};

const CANARY: &str = "canary-r3v13w-8b2e71";
/// The target session: a UUID, so its `session` tag is the UUID itself.
const SESSION: &str = "5e55e55e-1111-4111-8111-0000000000a1";

/// The records with message `msg` that contain `needle` (this test's
/// session or directory). `cargo test` runs every test in one process,
/// sharing the capture.
fn records(log: &Capture, msg: &str, needle: &str) -> Vec<String> {
    let msg = format!("\"msg\":\"{msg}\"");
    log.lines()
        .into_iter()
        .filter(|l| l.contains(&msg) && l.contains(needle))
        .collect()
}

fn one(log: &Capture, msg: &str, needle: &str) -> String {
    let found = records(log, msg, needle);
    assert_eq!(found.len(), 1, "{msg:?}: {found:#?} in {:#?}", log.lines());
    found.into_iter().next().unwrap()
}

/// The raw JSON text of field `name` in `line`.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let start = line.find(&format!("\"{name}\":"))? + name.len() + 3;
    let rest = &line[start..];
    let end = if let Some(text) = rest.strip_prefix('"') {
        text.find('"')? + 2
    } else {
        rest.find([',', '}'])?
    };
    Some(&rest[..end])
}

fn dir_name(dir: &Path) -> String {
    dir.file_name().unwrap().to_str().unwrap().to_owned()
}

#[test]
fn logs_hold_no_contents() {
    let log = testing::capture();
    let fx = Fx::new();
    let base = format!("one {CANARY}\n");
    let worktree = format!("one {CANARY}\ntwo {CANARY}\n");
    let c1 = fx.commit(&[("a.txt", &base)], None, T - 1000);
    fx.head("refs/heads/main", c1, &[("a.txt", &base)]);
    let message = format!("commit (initial): {CANARY}");
    fx.reflog(&[(None, c1, T - 1000, &message)]);
    fx.write("a.txt", &worktree);

    // The annotation log: a valid record, then a malformed line, a record
    // of a type this version does not know, and an unfinished last line,
    // each holding the canary.
    let data_dir = fx.dir.path().join("review");
    let mut file = LogFile::new(&data_dir, fx.repo.common_dir());
    let _ = file.load(NOW);
    file.append(&[Record::Base(BaseRecord {
        at: NOW,
        session: SessionId {
            source: Arc::from(SOURCE),
            id: Arc::from("someone-else"),
        },
        commit: None,
        entry_at: None,
    })])
    .unwrap();
    let torn = format!("{{\"t\":\"seen\",\"at\":1,\"note\":\"{CANARY}");
    let mut out = OpenOptions::new().append(true).open(file.path()).unwrap();
    write!(
        out,
        "{CANARY} not json\n{{\"t\":\"pin\",\"at\":1,\"text\":\"{CANARY}\"}}\n{torn}"
    )
    .unwrap();
    drop(out);

    // The target session (prompt, agent text and edit content hold the
    // canary) and another one that ran a command with it.
    let sessions = fx.sessions(vec![
        json!({
            "id": SESSION, "cwd": fx.root(), "started_at": ms(T),
            "turns": [{
                "id": "t1", "at": ms(T + 10), "prompt": format!("prompt {CANARY}"),
                "items": [
                    { "text": format!("I will write {CANARY}") },
                    edit("c1", ms(T + 20), vec![replace("a.txt", &base, &worktree)]),
                ],
            }],
        }),
        json!({
            "id": "other", "cwd": fx.root(), "started_at": ms(T + 100),
            "turns": [{
                "id": "o1", "at": ms(T + 110), "prompt": CANARY,
                "items": [
                    { "run": "r1", "command": format!("echo {CANARY}"),
                      "at": ms(T + 120), "done": ms(T + 130) },
                ],
            }],
        }),
    ]);
    let d = fx.driven(vec![sessions]);
    let fail_append = Arc::new(AtomicBool::new(false));
    let failing = Arc::clone(&fail_append);
    d.engine.set_log_test(
        move |op| {
            if op == Op::Append && failing.load(Ordering::Relaxed) {
                return Err(std::io::Error::other(CANARY));
            }
            Ok(())
        },
        // A tiny compaction threshold: the first append compacts.
        Limits {
            read_max: READ_MAX,
            compact_above: 64,
        },
    );

    let (id, s) = open(&d, target(SESSION));
    assert_eq!(paths(&s), ["a.txt"]);
    // Rows and the prompt's text, through `Prepare`.
    let prompt = s.turns.as_ref().unwrap()[0].prompt.unwrap();
    d.command(
        id,
        ReviewCommand::Prepare(Arc::from(vec![
            Need::Rows(RowsKey::Final {
                file: modified("a.txt"),
                context: 3,
            }),
            Need::Text(prompt),
        ])),
    );
    d.run();
    // The first mark: the unfinished line is cut, the file compacted.
    d.command(id, ReviewCommand::MarkSeen(modified("a.txt")));
    d.run();
    // A failed append keeps the record; the next mark retries it.
    fail_append.store(true, Ordering::Relaxed);
    d.command(id, ReviewCommand::MarkUnseen(modified("a.txt")));
    d.run();
    fail_append.store(false, Ordering::Relaxed);
    d.command(id, ReviewCommand::MarkSeen(modified("a.txt")));
    d.run();
    // The other targets of the worktree.
    let _ = open(&d, d.outside());
    let _ = open(&d, d.range(lectern_services::changes::BaseSpec::Head));

    let tmp = dir_name(fx.dir.path());
    let recovered = one(log, "annotation log recovered", &tmp);
    assert_eq!(field(&recovered, "lv"), Some("\"warn\""));
    assert_eq!(field(&recovered, "tg"), Some("\"review\""));
    assert_eq!(field(&recovered, "bad_lines"), Some("1"));
    assert_eq!(field(&recovered, "unknown"), Some("1"));
    let torn_bytes = torn.len().to_string();
    assert_eq!(field(&recovered, "torn_bytes"), Some(torn_bytes.as_str()));

    let compacted = one(log, "annotation log compacted", &tmp);
    assert_eq!(field(&compacted, "lv"), Some("\"info\""));
    let before: u64 = field(&compacted, "before").unwrap().parse().unwrap();
    let after: u64 = field(&compacted, "after").unwrap().parse().unwrap();
    assert!(after < before, "{compacted}");

    let failed = one(
        log,
        "annotation log append failed; kept for the next mark",
        &tmp,
    );
    assert_eq!(field(&failed, "lv"), Some("\"warn\""));
    assert_eq!(field(&failed, "records"), Some("1"));
    assert_eq!(field(&failed, "failures"), Some("1"));
    assert!(field(&failed, "io").unwrap().contains("Other"), "{failed}");
    let retried = one(log, "annotation log append retried", &tmp);
    assert_eq!(
        field(&retried, "records"),
        Some("2"),
        "the kept one and the new one"
    );

    // The scope: the review's hash and the session's UUID, on the engine's
    // records and on the annotation log's.
    let work = d.engine.work(id).unwrap();
    let ctx = crate::diag::review_ctx(&target(SESSION), work.git.common_dir(), &work.target_key);
    let review = format!("\"{:016x}\"", ctx.review.unwrap());
    let ready = one(log, "review ready", SESSION);
    assert_eq!(field(&ready, "review"), Some(review.as_str()));
    assert_eq!(
        field(&ready, "session"),
        Some(format!("\"{SESSION}\"").as_str())
    );
    assert_eq!(field(&ready, "base"), Some("\"reflog\""));
    for line in [&recovered, &compacted, &failed, &retried] {
        assert_eq!(field(line, "review"), Some(review.as_str()), "{line}");
        assert_eq!(
            field(line, "session"),
            Some(format!("\"{SESSION}\"").as_str())
        );
    }
    assert!(!records(log, "review settled", SESSION).is_empty());
    assert!(!records(log, "file attributed", SESSION).is_empty());
    assert!(!records(log, "prepared", SESSION).is_empty());

    log.assert_absent(CANARY);
}

#[test]
fn logs_tree_changes_failure_is_a_warn() {
    let log = testing::capture();
    let fx = Fx::new();
    let c1 = fx.commit(&[("a.txt", "one\n")], None, T - 1000);
    fx.head("refs/heads/main", c1, &[("a.txt", "one\n")]);
    // A reflog record whose new commit is not in the repository (a gc'd
    // object): its tree changes fail, on the interactive task and again in
    // the background. (The ancestry check finds no merge base and answers
    // "not an ancestor" without an error.)
    let gone = Oid::from_hex(fx.repo.hash(), &"ab".repeat(fx.repo.hash().hex_len() / 2)).unwrap();
    fx.reflog(&[
        (None, c1, T - 1000, "commit (initial): one"),
        (Some(c1), gone, T + 50, "checkout: moving from main to gone"),
    ]);
    fx.write("a.txt", "one\ntwo\n");
    let session_id = "5e55e55e-2222-4222-8222-0000000000b2";
    let sessions = fx.sessions(vec![session(
        session_id,
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
    // A budget no machine runs out of: both records are computed on the
    // interactive task, so the background completes only the failed one and
    // finds nothing new. With the 5 ms default, a slow machine leaves the
    // second record to the background, whose success starts another round
    // and a third failure.
    let d = Driven::with_budget(
        Arc::clone(&fx.projects),
        vec![sessions],
        fx.dir.path(),
        NOW,
        Duration::from_secs(3600),
    );
    let _ = open(&d, target(session_id));

    let gone_hex = format!("\"{}\"", gone.to_hex());
    let tree = records(
        log,
        "tree changes failed; the record cuts every path",
        &gone_hex,
    );
    assert_eq!(tree.len(), 2, "{:#?}", log.lines());
    for line in &tree {
        assert_eq!(field(line, "lv"), Some("\"warn\""));
        assert_eq!(field(line, "new"), Some(gone_hex.as_str()));
        assert_eq!(
            field(line, "old"),
            Some(format!("\"{}\"", c1.to_hex()).as_str())
        );
        assert_eq!(field(line, "error"), Some("\"object_missing\""));
        assert_eq!(
            field(line, "session"),
            Some(format!("\"{session_id}\"").as_str())
        );
    }
}

#[test]
fn logs_unreadable_turns_are_a_warn() {
    let log = testing::capture();
    let fx = Fx::new();
    let c1 = fx.commit(&[("a.txt", "one\n")], None, T - 1000);
    fx.head("refs/heads/main", c1, &[("a.txt", "one\n")]);
    fx.write("a.txt", "one\ntwo\n");
    let session_id = "5e55e55e-3333-4333-8333-0000000000c3";
    let sessions = fx.sessions(vec![json!({
        "id": session_id, "cwd": fx.root(), "started_at": ms(T), "unrecognized": true,
    })]);
    let d = fx.driven(vec![sessions]);
    let (_, s) = open(&d, target(session_id));
    assert!(s.degraded.is_some());
    let line = one(
        log,
        "session turns unavailable; reviewed without them",
        session_id,
    );
    assert_eq!(field(&line, "lv"), Some("\"warn\""));
    assert_eq!(field(&line, "error"), Some("\"unrecognized\""));
    assert_eq!(
        field(&line, "from"),
        Some(format!("\"{session_id}\"").as_str())
    );
    assert_eq!(field(&line, "target"), Some("true"));
}

#[test]
fn logs_round_closed_before_it_ran_is_not_a_failure() {
    let log = testing::capture();
    let fx = Fx::new();
    let c1 = fx.commit(&[("a.txt", "one\n")], None, T - 1000);
    fx.head("refs/heads/main", c1, &[("a.txt", "one\n")]);
    fx.write("a.txt", "one\ntwo\n");
    let session_id = "5e55e55e-4444-4444-8444-0000000000d4";
    let sessions = fx.sessions(vec![session(
        session_id,
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
    let d = fx.driven(vec![sessions]);
    // Closed before its round ran: the round is cancelled and no longer
    // the review's.
    let id = d.open(target(session_id));
    d.close(id);
    d.run();
    assert!(d.snapshot(id).is_none());
    assert!(
        records(log, "review failed", session_id).is_empty(),
        "{:#?}",
        log.lines()
    );
    assert!(records(log, "review ready", session_id).is_empty());
}

#[test]
fn logs_session_tag_is_the_uuid_or_a_hash() {
    assert_eq!(
        crate::diag::session_tag("5e55e55e-1111-4111-8111-0000000000a1"),
        [
            0x5e, 0x55, 0xe5, 0x5e, 0x11, 0x11, 0x41, 0x11, 0x81, 0x11, 0, 0, 0, 0, 0, 0xa1
        ]
    );
    let sub = crate::diag::session_tag("5e55e55e-1111-4111-8111-0000000000a1/agent-a0fa0001");
    assert_ne!(
        sub,
        crate::diag::session_tag("5e55e55e-1111-4111-8111-0000000000a1")
    );
    assert_eq!(
        sub,
        crate::diag::session_tag("5e55e55e-1111-4111-8111-0000000000a1/agent-a0fa0001")
    );
}
