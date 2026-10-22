//! Change chains, provenance, attribution, the history digest and the
//! cache value (docs/design/review.md「测试」: `chain_*`, `provenance_*`,
//! `attribute_*`, `history_digest_*`). Synthetic contents; structured
//! results are insta snapshots rendered as text.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use lectern_cache::{Budgets, Cache, Config};
use lectern_git::HashKind;
use lectern_services::changes::{ChangePath, DiffHunk, SideIdentity};
use lectern_services::review::{Alignment, AttributionState, Survival, TurnRef};
use lectern_services::session::{
    ContentRef, EditOp, FileEdit, SessionKey, SourceId, ToolCallId, ToolStatus, TurnId,
};

use super::attribute::{self, Owner};
use super::codec::{self, CachedChain};
use super::history::{self, HistoryStep};
use super::*;

fn path(p: &str) -> ChangePath {
    ChangePath(Arc::from(p.as_bytes()))
}

fn write(p: &str, content: &str) -> Edit {
    Edit {
        path: path(p),
        op: Op::Write {
            content: content.into(),
        },
    }
}

fn replace(p: &str, old: &str, new: &str) -> Edit {
    Edit {
        path: path(p),
        op: Op::Replace {
            old: old.into(),
            new: new.into(),
            all: false,
        },
    }
}

fn patch(p: &str, hunks: &str, move_to: Option<&str>) -> Edit {
    Edit {
        path: path(p),
        op: Op::Patch {
            hunks: hunks.into(),
            move_to: move_to.map(path),
        },
    }
}

fn delete(p: &str) -> Edit {
    Edit {
        path: path(p),
        op: Op::Delete,
    }
}

fn step(group: u32, edits: Vec<Edit>) -> StepInput {
    StepInput {
        group,
        completed: true,
        edits: edits.into(),
    }
}

fn side(text: Option<&str>) -> Side<'_> {
    text.map_or(Side::Absent, |t| Side::Text(t.as_bytes()))
}

fn run(base: Option<&str>, worktree: Option<&str>, steps: &[StepInput]) -> Chain {
    run_at(("f.rs", "f.rs"), base, worktree, steps)
}

/// A chain from `paths.0` (the base side's path) to `paths.1` (the
/// worktree side's).
fn run_at(
    paths: (&str, &str),
    base: Option<&str>,
    worktree: Option<&str>,
    steps: &[StepInput],
) -> Chain {
    let (start, end) = (path(paths.0), path(paths.1));
    build(&ChainInput {
        hash: HashKind::Sha1,
        start: &start,
        end: &end,
        base: side(base),
        worktree: side(worktree),
        steps,
    })
}

/// The base-to-worktree hunks, as the `ChangeList` has them.
fn final_hunks(base: Option<&str>, worktree: Option<&str>) -> Vec<DiffHunk> {
    let (b, w) = (base.unwrap_or_default(), worktree.unwrap_or_default());
    lectern_git::line_diff(b.as_bytes(), w.as_bytes(), MAX_VERSION_BYTES)
        .unwrap()
        .hunks
        .iter()
        .map(hunk)
        .collect()
}

fn other(id: &str) -> SessionKey {
    SessionKey {
        source: SourceId("session-fake".into()),
        id: id.into(),
    }
}

fn own(ordinal: u32) -> Owner {
    Owner::Own(TurnRef {
        session: 0,
        ordinal,
    })
}

fn short(oid: Option<BlobId>) -> String {
    oid.map_or_else(|| "absent".to_owned(), |o| o.to_string()[..8].to_owned())
}

/// The chain as text: steps, narrative steps, then each worktree line with
/// its source and each base line with its fate.
fn render(chain: &Chain, base: Option<&str>, worktree: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("steps:\n");
    for (k, s) in chain.steps.iter().enumerate() {
        let _ = writeln!(out, "  {k} {:?} {}", s.outcome, short(s.version));
    }
    out.push_str("groups:\n");
    for g in chain.groups.iter() {
        let _ = write!(out, "  {:?}", g.steps);
        match &g.diff {
            None => out.push_str(" file-level\n"),
            Some(d) => {
                let _ = writeln!(out, " +{} -{} {:?}", d.added, d.removed, d.survival);
                for h in d.hunks.iter() {
                    let _ = writeln!(
                        out,
                        "    old {:?} new {:?} bytes {}/{}",
                        h.old, h.new, h.old_byte, h.new_byte
                    );
                }
            }
        }
    }
    match &chain.detail {
        Detail::Lines(lines) => {
            let _ = writeln!(out, "aligned: {}", lines.aligned);
            out.push_str("worktree:\n");
            let w: Vec<&[u8]> = super::lines(worktree.unwrap_or_default().as_bytes()).collect();
            for (s, l) in lines.worktree.iter().zip(&w) {
                let s = match s {
                    Source::Base(b) => format!("base {b}"),
                    Source::Step(k) => format!("step {k}"),
                    Source::None => "none".to_owned(),
                };
                let _ = writeln!(out, "  {s:<7} | {}", String::from_utf8_lossy(l).trim_end());
            }
            out.push_str("base:\n");
            let b: Vec<&[u8]> = super::lines(base.unwrap_or_default().as_bytes()).collect();
            for (f, l) in lines.base.iter().zip(&b) {
                let f = match f {
                    Fate::Kept => "kept".to_owned(),
                    Fate::Deleted(k) => format!("del {k}"),
                    Fate::Outside => "outside".to_owned(),
                };
                let _ = writeln!(out, "  {f:<7} | {}", String::from_utf8_lossy(l).trim_end());
            }
        }
        Detail::FileOnly { reason, matched } => {
            let _ = writeln!(out, "file-only: {reason:?}, matched {matched:?}");
        }
    }
    out
}

/// The final hunks' and the file's attribution for `owners`.
fn render_attribution(
    chain: &Chain,
    owners: &[Owner],
    base: Option<&str>,
    worktree: Option<&str>,
) -> String {
    let hunks = final_hunks(base, worktree);
    let attributed = attribute::hunks(chain, owners, &hunks);
    let mut out = String::new();
    for (h, a) in hunks.iter().zip(&attributed) {
        let turns: Vec<String> = a
            .turns
            .iter()
            .map(|t| format!("{}#{}", t.session, t.ordinal))
            .collect();
        let others: Vec<&str> = a.others.iter().map(|s| &*s.id).collect();
        let _ = writeln!(
            out,
            "hunk old {:?} new {:?}: lines {} turns {turns:?} others {others:?} drifted {}",
            h.old, h.new, a.lines, a.drifted
        );
    }
    let file = attribute::file(chain, owners, &attributed);
    let turns: Vec<String> = file
        .turns
        .iter()
        .map(|t| format!("{}#{}", t.session, t.ordinal))
        .collect();
    let others: Vec<&str> = file.other_sessions.iter().map(|s| &*s.id).collect();
    let _ = writeln!(
        out,
        "file: {:?} turns {turns:?} others {others:?} drifted_hunks {}",
        file.state, file.drifted_hunks
    );
    out
}

fn snapshot(
    name: &str,
    chain: &Chain,
    owners: &[Owner],
    base: Option<&str>,
    worktree: Option<&str>,
) {
    let text = format!(
        "{}\n{}",
        render(chain, base, worktree),
        render_attribution(chain, owners, base, worktree)
    );
    insta::assert_snapshot!(name, text);
    // Every chain survives the cache value.
    let mut bytes = Vec::new();
    codec::encode(chain, &mut bytes);
    assert_eq!(
        codec::decode(&bytes).as_ref(),
        Some(chain),
        "{name}: round trip"
    );
}

// ---- chain_* ----

#[test]
fn chain_applies_the_five_ops() {
    let base = Some("a\nb\nc\n");
    let worktree = Some("fresh\nfile\n");
    let steps = [
        step(1, vec![replace("f.rs", "b\n", "B\n")]),
        step(1, vec![patch("f.rs", "@@ -3,1 +3,2 @@\n c\n+d\n", None)]),
        step(2, vec![write("f.rs", "a\nB\nwritten\n")]),
        step(3, vec![delete("f.rs")]),
        // `Add` loads as `Op::Write`, like `Write`.
        step(
            4,
            vec![
                Edit::from_file_edit(
                    &FileEdit {
                        path: "/w/f.rs".into(),
                        op: EditOp::Add {
                            content: "fresh\nfile\n".into(),
                        },
                    },
                    &|p| p.strip_prefix("/w").ok().map(|p| path(p.to_str().unwrap())),
                )
                .unwrap(),
            ],
        ),
    ];
    let chain = run(base, worktree, &steps);
    assert!(chain.steps.iter().all(|s| s.outcome == Outcome::Applied));
    assert_eq!(chain.steps[3].version, None, "deleted");
    let owners = [own(1), own(1), own(2), own(3), own(4)];
    snapshot(
        "chain_applies_the_five_ops",
        &chain,
        &owners,
        base,
        worktree,
    );
}

#[test]
fn chain_replace_that_misses_does_not_apply_and_later_steps_do() {
    let base = Some("x\ny\nx\n");
    // The failed steps' lines show up in the worktree without a source.
    let worktree = Some("x\nY\nx\nz\n");
    let steps = [
        step(1, vec![replace("f.rs", "missing\n", "z\n")]),
        // Twice in the version: not the version the agent saw.
        step(1, vec![replace("f.rs", "x\n", "X\n")]),
        step(2, vec![replace("f.rs", "y\n", "Y\n")]),
    ];
    let chain = run(base, worktree, &steps);
    let outcomes: Vec<Outcome> = chain.steps.iter().map(|s| s.outcome).collect();
    assert_eq!(
        outcomes,
        [Outcome::Failed, Outcome::Failed, Outcome::Applied]
    );
    assert_eq!(chain.steps[0].version, chain.steps[1].version, "unchanged");
    let owners = [own(1), own(1), own(2)];
    snapshot("chain_replace_that_misses", &chain, &owners, base, worktree);
}

#[test]
fn chain_patch_applies_at_an_offset_and_not_on_other_context() {
    // Two lines were added on top since the patch was made against "1..5".
    let base = Some("new0\nnew1\n1\n2\n3\n4\n5\n");
    let worktree = Some("new0\nnew1\n1\n2\nthree\n4\n5\nsix\n");
    let steps = [
        step(
            1,
            vec![patch("f.rs", "@@ -2,3 +2,3 @@\n 2\n-3\n+three\n 4\n", None)],
        ),
        // Context "4" / "x" is not in the version.
        step(
            2,
            vec![patch("f.rs", "@@ -4,2 +4,3 @@\n 4\n x\n+six\n", None)],
        ),
    ];
    let chain = run(base, worktree, &steps);
    assert_eq!(chain.steps[0].outcome, Outcome::Applied);
    assert_eq!(chain.steps[1].outcome, Outcome::Failed);
    let owners = [own(1), own(2)];
    snapshot("chain_patch_offset", &chain, &owners, base, worktree);
}

#[test]
fn chain_write_realigns_the_chain() {
    // The file changed outside the chain before the session's steps.
    let base = Some("a\nb\nc\n");
    let worktree = Some("a\nB\nc\nd\n");
    let steps = [
        step(1, vec![replace("f.rs", "not here\n", "x\n")]),
        step(2, vec![write("f.rs", "a\nB\nc\nd\n")]),
    ];
    let chain = run(base, worktree, &steps);
    let Detail::Lines(lines) = &chain.detail else {
        panic!("line-level")
    };
    assert!(lines.aligned);
    let owners = [own(1), own(2)];
    snapshot("chain_write_realigns", &chain, &owners, base, worktree);
}

#[test]
fn chain_follows_move_to() {
    let base = Some("1\n2\n");
    let worktree = Some("one\n2\nthree\n");
    let steps = [
        step(1, vec![patch("a.rs", "@@\n-1\n+one\n", Some("b.rs"))]),
        step(2, vec![replace("b.rs", "2\n", "2\nthree\n")]),
        // The chain is on b.rs now: an edit of a.rs is not on its path.
        step(3, vec![replace("a.rs", "one\n", "uno\n")]),
    ];
    let chain = run_at(("a.rs", "b.rs"), base, worktree, &steps);
    let outcomes: Vec<Outcome> = chain.steps.iter().map(|s| s.outcome).collect();
    assert_eq!(
        outcomes,
        [Outcome::Applied, Outcome::Applied, Outcome::Failed]
    );
    let owners = [own(1), own(2), own(3)];
    snapshot("chain_follows_move_to", &chain, &owners, base, worktree);
}

#[test]
fn chain_follows_a_rename_by_command() {
    // Turn 1 edits a.rs, turn 2 runs `git mv a.rs b.rs` (a command, no
    // change step), turn 3 edits b.rs: the change set has a.rs → b.rs.
    let base = Some("1\n2\n3\n");
    let worktree = Some("one\n2\nthree\n");
    let steps = [
        step(1, vec![replace("a.rs", "1\n", "one\n")]),
        step(3, vec![replace("b.rs", "3\n", "three\n")]),
    ];
    let chain = run_at(("a.rs", "b.rs"), base, worktree, &steps);
    assert!(chain.steps.iter().all(|s| s.outcome == Outcome::Applied));
    let owners = [own(1), own(3)];
    let hunks = attribute::hunks(&chain, &owners, &final_hunks(base, worktree));
    assert!(hunks.iter().all(|h| h.drifted == 0 && h.turns.len() == 1));
    snapshot("chain_rename_by_command", &chain, &owners, base, worktree);

    // Without the rename, the edit of b.rs is not on the chain's path.
    let alone = run(base, worktree, &steps);
    assert_eq!(alone.steps[1].outcome, Outcome::Failed);

    // A `Write` of the new path before the old one is removed by hand
    // realigns the chain on b.rs as well.
    let written = [step(1, vec![write("b.rs", "one\n2\nthree\n")])];
    let chain = run_at(("a.rs", "b.rs"), base, worktree, &written);
    assert!(matches!(&chain.detail, Detail::Lines(l) if l.aligned));
    assert_eq!(chain.steps[0].outcome, Outcome::Applied);
}

#[test]
fn chain_follows_a_delete_and_add_in_one_patch() {
    // Codex's `*** Delete File: a.rs` and `*** Add File: b.rs` in one
    // `apply_patch`, then an edit of b.rs.
    let base = Some("x\ny\n");
    let worktree = Some("x\ny\nZ\n");
    let steps = [
        step(1, vec![delete("a.rs"), write("b.rs", "x\ny\nz\n")]),
        step(2, vec![replace("b.rs", "z\n", "Z\n")]),
    ];
    let chain = run_at(("a.rs", "b.rs"), base, worktree, &steps);
    assert!(chain.steps.iter().all(|s| s.outcome == Outcome::Applied));
    let Detail::Lines(lines) = &chain.detail else {
        panic!("line-level")
    };
    assert!(lines.aligned);
    // The lines the addition kept from the old file stay base lines.
    assert_eq!(
        &*lines.worktree,
        [Source::Base(0), Source::Base(1), Source::Step(1)]
    );
    let owners = [own(1), own(2)];
    snapshot("chain_delete_and_add", &chain, &owners, base, worktree);

    // File-level chains follow the rename the same way.
    let oid = blob_id(HashKind::Sha1, b"x\ny\nz\n");
    let (start, end) = (path("a.rs"), path("b.rs"));
    let file_only = build(&ChainInput {
        hash: HashKind::Sha1,
        start: &start,
        end: &end,
        base: Side::Text(b"x\ny\n"),
        worktree: Side::Opaque(Some(oid)),
        steps: &steps[..1],
    });
    assert!(matches!(
        file_only.detail,
        Detail::FileOnly {
            matched: Some(0),
            ..
        }
    ));
}

#[test]
fn chain_patch_context_header_keeps_the_change_where_it_was_made() {
    // Codex's `@@ fn b() {` in a `load(input)` body: the change goes in the
    // second of two identical blocks, as `apply_patch` made it, and nothing
    // is Drifted.
    let base = Some("fn a() {\n    x\n}\nfn b() {\n    x\n}\n");
    let worktree = Some("fn a() {\n    x\n}\nfn b() {\n    y\n}\n");
    let steps = [step(
        1,
        vec![patch("f.rs", "@@ fn b() {\n-    x\n+    y\n", None)],
    )];
    let chain = run(base, worktree, &steps);
    assert!(matches!(&chain.detail, Detail::Lines(l) if l.aligned));
    let hunks = attribute::hunks(&chain, &[own(1)], &final_hunks(base, worktree));
    assert_eq!(hunks.len(), 1);
    assert_eq!((hunks[0].turns.len(), hunks[0].drifted), (1, 0));
}

#[test]
fn chain_retries_crlf_as_lf() {
    // Canonical sides (autocrlf): LF. The agent's edits are raw CRLF.
    let base = Some("a\nb\n");
    let worktree = Some("A\nb\nc\n");
    let steps = [
        step(1, vec![replace("f.rs", "a\r\nb\r\n", "A\r\nb\r\n")]),
        step(2, vec![write("f.rs", "A\r\nb\r\nc\r\n")]),
    ];
    let chain = run(base, worktree, &steps);
    assert!(chain.steps.iter().all(|s| s.outcome == Outcome::Applied));
    let Detail::Lines(lines) = &chain.detail else {
        panic!("line-level")
    };
    assert!(lines.aligned);
    // A worktree with CRLF keeps a `Write`'s CRLF as written.
    let crlf = Some("A\r\nb\r\n");
    let kept = run(crlf, crlf, &[step(1, vec![write("f.rs", "A\r\nb\r\n")])]);
    assert!(matches!(&kept.detail, Detail::Lines(l) if l.aligned));
    let owners = [own(1), own(2)];
    snapshot("chain_crlf", &chain, &owners, base, worktree);
}

#[test]
fn chain_call_is_atomic_and_unfinished_calls_do_not_apply() {
    let base = Some("a\nb\n");
    let worktree = Some("a\nb\n");
    let mut unfinished = step(1, vec![replace("f.rs", "a\n", "A\n")]);
    unfinished.completed = false;
    let steps = [
        // A `MultiEdit` whose second edit misses: nothing applies.
        step(
            1,
            vec![replace("f.rs", "a\n", "A\n"), replace("f.rs", "zz", "y")],
        ),
        unfinished,
    ];
    let chain = run(base, worktree, &steps);
    let outcomes: Vec<Outcome> = chain.steps.iter().map(|s| s.outcome).collect();
    assert_eq!(outcomes, [Outcome::Failed, Outcome::NotCompleted]);
    let calls = [ToolCallId("c1".into()), ToolCallId("c2".into())];
    assert_eq!(
        attribute::alignment(&chain, 0, &calls),
        Alignment::Unaligned,
        "nothing of the turn applied"
    );
}

#[test]
fn chain_beyond_the_limits_is_file_level() {
    let base = Some("a\n");
    // More than 200 steps.
    let many: Vec<StepInput> = (0..=MAX_STEPS as u32)
        .map(|i| step(i, vec![write("f.rs", "a\n")]))
        .collect();
    let chain = run(base, base, &many);
    assert!(matches!(
        chain.detail,
        Detail::FileOnly {
            reason: Limit::Steps,
            ..
        }
    ));
    assert!(chain.steps.iter().all(|s| s.outcome == Outcome::Unaligned));
    assert!(chain.groups.iter().all(|g| g.diff.is_none()));
    assert!(chain.cacheable());

    // A version above 4 MiB.
    let big = "x\n".repeat((MAX_VERSION_BYTES / 2) as usize + 1);
    let chain = run(base, base, &[step(1, vec![write("f.rs", &big)])]);
    assert!(matches!(
        chain.detail,
        Detail::FileOnly {
            reason: Limit::Size,
            ..
        }
    ));
    let chain = run(Some(&big), base, &[step(1, vec![write("f.rs", "a\n")])]);
    assert!(matches!(
        chain.detail,
        Detail::FileOnly {
            reason: Limit::Size,
            ..
        }
    ));

    // A worktree side that is not text: goes to the last `Write` whose
    // content is the worktree blob, and is not cached.
    let content = "binary-ish\n";
    let oid = blob_id(HashKind::Sha1, content.as_bytes());
    let steps = [
        step(1, vec![write("f.rs", "first\n")]),
        step(2, vec![write("f.rs", content)]),
    ];
    let start = path("f.rs");
    let chain = build(&ChainInput {
        hash: HashKind::Sha1,
        start: &start,
        end: &start,
        base: Side::Text(b"a\n"),
        worktree: Side::Opaque(Some(oid)),
        steps: &steps,
    });
    assert_eq!(
        chain.detail,
        Detail::FileOnly {
            reason: Limit::Sides,
            matched: Some(1)
        }
    );
    assert!(!chain.cacheable());
}

#[test]
fn chain_cache_value_rejects_damage() {
    let base = Some("a\nb\n");
    let worktree = Some("a\nB\nc\n");
    let chain = run(
        base,
        worktree,
        &[step(1, vec![replace("f.rs", "b\n", "B\n")])],
    );
    let mut bytes = Vec::new();
    codec::encode(&chain, &mut bytes);
    assert_eq!(codec::decode(&bytes), Some(chain.clone()));
    for cut in 0..bytes.len() {
        assert_eq!(codec::decode(&bytes[..cut]), None, "truncated at {cut}");
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(codec::decode(&trailing), None);
    // A step index beyond the steps: the first group's first member.
    let mut wrong = bytes.clone();
    let at = 1 + 1 + 21 + 1 + 1;
    assert_eq!(wrong[at], 0);
    wrong[at] = 9;
    assert_eq!(codec::decode(&wrong), None);
    // A file-level chain round-trips too.
    let many: Vec<StepInput> = (0..=MAX_STEPS as u32)
        .map(|i| step(i, vec![write("f.rs", "a\n")]))
        .collect();
    let file_only = run(base, base, &many);
    bytes.clear();
    codec::encode(&file_only, &mut bytes);
    assert_eq!(codec::decode(&bytes), Some(file_only));
}

#[test]
fn chain_survival_of_each_narrative_step() {
    let base = Some("a\n");
    // Turn 1 adds two lines, turn 2 rewrites one of them, turn 3 adds a
    // line that is removed by hand afterwards.
    let worktree = Some("a\nb\nC\n");
    let steps = [
        step(1, vec![replace("f.rs", "a\n", "a\nb\nc\n")]),
        step(2, vec![replace("f.rs", "c\n", "C\n")]),
        step(3, vec![replace("f.rs", "C\n", "C\nd\n")]),
    ];
    let chain = run(base, worktree, &steps);
    let survival: Vec<Survival> = chain
        .groups
        .iter()
        .map(|g| g.diff.as_ref().unwrap().survival)
        .collect();
    assert_eq!(survival, [Survival::Partly, Survival::All, Survival::None]);
    let owners = [own(1), own(2), own(3)];
    snapshot("chain_survival", &chain, &owners, base, worktree);
}

// ---- provenance_* ----

#[test]
fn provenance_manual_edit_after_the_session_is_drifted() {
    let base = Some("fn a() {}\nfn b() {}\n");
    let worktree = Some("fn a() { 1 }\nfn b() {}\n// by hand\n");
    let steps = [step(1, vec![replace("f.rs", "fn a() {}", "fn a() { 1 }")])];
    let chain = run(base, worktree, &steps);
    let hunks = attribute::hunks(&chain, &[own(1)], &final_hunks(base, worktree));
    assert_eq!(hunks.len(), 2);
    assert_eq!((hunks[0].turns.len(), hunks[0].drifted), (1, 0));
    // Drifted: no turn, no other session.
    assert_eq!(
        (
            hunks[1].turns.len(),
            hunks[1].others.len(),
            hunks[1].drifted
        ),
        (0, 0, 1)
    );
    snapshot("provenance_manual_edit", &chain, &[own(1)], base, worktree);
}

#[test]
fn provenance_base_lines_removed_by_hand_count_as_drifted() {
    let base = Some("keep\ngone\nedit\n");
    let worktree = Some("keep\nEDIT\n");
    let steps = [step(1, vec![replace("f.rs", "edit\n", "EDIT\n")])];
    let chain = run(base, worktree, &steps);
    let Detail::Lines(lines) = &chain.detail else {
        panic!("line-level")
    };
    assert_eq!(&*lines.base, [Fate::Kept, Fate::Outside, Fate::Deleted(0)]);
    let hunks = attribute::hunks(&chain, &[own(1)], &final_hunks(base, worktree));
    assert_eq!(hunks.iter().map(|h| h.drifted).sum::<u32>(), 1);
    snapshot(
        "provenance_manual_deletion",
        &chain,
        &[own(1)],
        base,
        worktree,
    );
}

// ---- attribute_* ----

#[test]
fn attribute_final_hunks_to_their_turns() {
    let base = Some("1\n2\n3\n4\n5\n6\n7\n8\n");
    let worktree = Some("one\n2\n3\n4\n5\n6\n8\neight\n");
    let steps = [
        step(1, vec![replace("f.rs", "1\n", "one\n")]),
        step(2, vec![replace("f.rs", "7\n8\n", "8\neight\n")]),
    ];
    let chain = run(base, worktree, &steps);
    let owners = [own(1), own(2)];
    let hunks = attribute::hunks(&chain, &owners, &final_hunks(base, worktree));
    let turns: Vec<Vec<u32>> = hunks
        .iter()
        .map(|h| h.turns.iter().map(|t| t.ordinal).collect())
        .collect();
    assert_eq!(turns, [vec![1], vec![2], vec![2]]);
    assert!(hunks.iter().all(|h| h.lines && h.drifted == 0));
    snapshot("attribute_turns", &chain, &owners, base, worktree);
}

#[test]
fn attribute_other_sessions_lines_are_others() {
    // Session B edits the file, then session A does. One chain serves both
    // reviews; only the owners differ.
    let base = Some("a\nb\nc\n");
    let worktree = Some("A\nb\nC\n");
    let steps = [
        step(10, vec![replace("f.rs", "a\n", "A\n")]),
        step(20, vec![replace("f.rs", "c\n", "C\n")]),
    ];
    let chain = run(base, worktree, &steps);
    let in_a = [Owner::Other(other("b")), own(1)];
    let hunks = attribute::hunks(&chain, &in_a, &final_hunks(base, worktree));
    assert_eq!(&*hunks[0].others, [other("b")]);
    assert!(hunks[0].turns.is_empty());
    assert_eq!(hunks[1].turns.len(), 1);
    let file = attribute::file(&chain, &in_a, &hunks);
    assert_eq!(&*file.other_sessions, [other("b")]);
    let in_b = [own(1), Owner::Other(other("a"))];
    let text = format!(
        "review of a:\n{}\nreview of b:\n{}",
        render_attribution(&chain, &in_a, base, worktree),
        render_attribution(&chain, &in_b, base, worktree)
    );
    insta::assert_snapshot!("attribute_others", text);
}

#[test]
fn attribute_alignment_of_narrative_steps() {
    let base = Some("a\nb\n");
    let worktree = Some("A\nb\n");
    let steps = [
        step(1, vec![replace("f.rs", "a\n", "A\n")]),
        step(1, vec![replace("f.rs", "zz\n", "y\n")]),
        step(2, vec![replace("f.rs", "qq\n", "y\n")]),
    ];
    let chain = run(base, worktree, &steps);
    let calls: Vec<ToolCallId> = ["c0", "c1", "c2"].map(|c| ToolCallId(c.into())).into();
    assert_eq!(
        attribute::alignment(&chain, 0, &calls),
        Alignment::Partial {
            failed: vec![ToolCallId("c1".into())].into()
        }
    );
    assert_eq!(
        attribute::alignment(&chain, 1, &calls),
        Alignment::Unaligned
    );
    let aligned = run(base, worktree, &steps[..1]);
    assert_eq!(
        attribute::alignment(&aligned, 0, &calls),
        Alignment::Aligned
    );
}

#[test]
fn attribute_file_level_goes_to_the_matching_write() {
    let content = "final\n";
    let oid = blob_id(HashKind::Sha1, content.as_bytes());
    let steps = [
        step(1, vec![write("f.rs", "draft\n")]),
        step(2, vec![write("f.rs", content)]),
    ];
    let start = path("f.rs");
    let input = |worktree| ChainInput {
        hash: HashKind::Sha1,
        start: &start,
        end: &start,
        base: Side::Absent,
        worktree,
        steps: &steps,
    };
    let owners = [own(1), Owner::Other(other("b"))];
    let chain = build(&input(Side::Opaque(Some(oid))));
    let hunks = attribute::hunks(&chain, &owners, &final_hunks(None, Some(content)));
    assert!(
        hunks
            .iter()
            .all(|h| !h.lines && h.turns.is_empty() && h.drifted == 0)
    );
    let file = attribute::file(&chain, &owners, &hunks);
    assert_eq!(file.state, AttributionState::FileOnly);
    assert!(file.turns.is_empty());
    assert_eq!(&*file.other_sessions, [other("b")]);
    assert_eq!(attribute::alignment(&chain, 0, &[]), Alignment::Unaligned);
    // No match: every step's turn and session.
    let chain = build(&input(Side::Opaque(None)));
    let file = attribute::file(&chain, &owners, &[]);
    assert_eq!(file.turns.len(), 1);
    assert_eq!(file.other_sessions.len(), 1);
}

// ---- history_digest_* ----

struct Call {
    session: SessionKey,
    adapter: u32,
    turn: TurnId,
    call: ToolCallId,
    status: ToolStatus,
    input: ContentRef,
    result: Option<ContentRef>,
}

fn call(session: &str, n: u64) -> Call {
    Call {
        session: other(session),
        adapter: 3,
        turn: TurnId(format!("turn-{n}").into()),
        call: ToolCallId(format!("call-{n}").into()),
        status: ToolStatus::Completed,
        input: ContentRef::new(n * 1000, 400, 0),
        result: Some(ContentRef::new(n * 1000 + 400, 300, 1)),
    }
}

fn digest(calls: &[Call], paths: &[&str]) -> [u8; 32] {
    let steps: Vec<HistoryStep<'_>> = calls
        .iter()
        .map(|c| HistoryStep {
            session: &c.session,
            adapter_version: c.adapter,
            turn: &c.turn,
            call: &c.call,
            status: c.status,
            input: c.input,
            result: c.result,
        })
        .collect();
    let paths: Vec<ChangePath> = paths.iter().map(|p| path(p)).collect();
    history::digest(&steps, &paths)
}

#[test]
fn history_digest_differs_between_sessions_with_the_same_content() {
    // Same positions, same turn and call IDs, same final content: only the
    // session differs, and so must the narrative.
    let a = digest(&[call("a", 1)], &["f.rs"]);
    let b = digest(&[call("b", 1)], &["f.rs"]);
    assert_ne!(a, b);
    let oid = SideIdentity::Blob(blob_id(HashKind::Sha1, b"same\n"));
    assert_ne!(
        codec::key(HashKind::Sha1, None, &oid, &a),
        codec::key(HashKind::Sha1, None, &oid, &b)
    );
}

#[test]
fn history_digest_is_the_same_in_two_worktrees() {
    // The same session history replayed in two worktrees of one repository
    // (the main one and a linked one): the logged edits carry absolute
    // paths under each worktree's root, and both are made relative before
    // the digest and the chain see them. Nothing worktree-specific is left,
    // so both look up the same entry and would store the same chain.
    let in_worktree = |root: &str| {
        let relative = |p: &Path| p.strip_prefix(root).ok().and_then(|p| p.to_str()).map(path);
        let logged = FileEdit {
            path: Path::new(root).join("src/f.rs"),
            op: EditOp::Replace {
                old: "b\n".into(),
                new: "B\n".into(),
                all: false,
            },
        };
        let edit = Edit::from_file_edit(&logged, &relative).unwrap();
        let claimed = relative(&logged.path).unwrap();
        let calls = [call("a", 1)];
        let steps: Vec<HistoryStep<'_>> = calls
            .iter()
            .map(|c| HistoryStep {
                session: &c.session,
                adapter_version: c.adapter,
                turn: &c.turn,
                call: &c.call,
                status: c.status,
                input: c.input,
                result: c.result,
            })
            .collect();
        let digest = history::digest(&steps, std::slice::from_ref(&claimed));
        let chain = run_at(
            ("src/f.rs", "src/f.rs"),
            Some("a\nb\n"),
            Some("a\nB\n"),
            &[step(1, vec![edit])],
        );
        (digest, chain)
    };
    let (one, chain_one) = in_worktree("/work/repo");
    let (two, chain_two) = in_worktree("/work/repo/.claude/worktrees/feature");
    assert_eq!(chain_one.steps[0].outcome, Outcome::Applied);
    assert_eq!(one, two);
    assert_eq!(chain_one, chain_two);
    assert_eq!(one, digest(&[call("a", 1)], &["src/f.rs"]));
    // The relative path itself is part of the history.
    assert_ne!(one, digest(&[call("a", 1)], &["lib/f.rs"]));
    let w = SideIdentity::Blob(blob_id(HashKind::Sha1, b"w\n"));
    let not_retained = SideIdentity::NotRetained(blob_id(HashKind::Sha1, b"w\n"));
    let base = Some(blob_id(HashKind::Sha1, b"b\n"));
    assert_eq!(
        codec::key(HashKind::Sha1, base, &w, &one),
        codec::key(HashKind::Sha1, base, &not_retained, &one),
        "retention is not part of the content"
    );
    let unhashed = SideIdentity::Unhashed {
        size: 1,
        mtime_ns: 2,
        inode: 3,
    };
    assert_eq!(codec::key(HashKind::Sha1, base, &unhashed, &one), None);
}

#[test]
fn history_digest_changes_with_each_input() {
    let base = digest(&[call("a", 1)], &["f.rs"]);
    let changed = |f: &dyn Fn(&mut Call)| {
        let mut c = call("a", 1);
        f(&mut c);
        digest(&[c], &["f.rs"])
    };
    assert_ne!(base, changed(&|c| c.status = ToolStatus::Failed), "status");
    assert_ne!(base, changed(&|c| c.adapter = 4), "adapter version");
    assert_ne!(base, changed(&|c| c.turn = TurnId("other".into())), "turn");
    assert_ne!(
        base,
        changed(&|c| c.call = ToolCallId("other".into())),
        "call"
    );
    assert_ne!(
        base,
        changed(&|c| c.input = ContentRef::new(1000, 400, 1)),
        "input"
    );
    assert_ne!(base, changed(&|c| c.result = None), "result");
    assert_ne!(
        base,
        digest(&[call("a", 1)], &["f.rs", "g.rs"]),
        "rename chain"
    );
    assert_ne!(
        digest(&[call("a", 1), call("a", 2)], &["f.rs"]),
        digest(&[call("a", 2), call("a", 1)], &["f.rs"]),
        "order"
    );
}

#[test]
fn history_digest_cache_hit_does_not_load() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(Config {
        root: dir.path().join("cache"),
        budgets: Budgets::default(),
        ui_thread: None,
    });
    let partition = cache.repo(Path::new("/fixture/repo/.git"));
    let base = "a\nb\n";
    let worktree = "a\nB\n";
    let w = SideIdentity::Blob(blob_id(HashKind::Sha1, worktree.as_bytes()));
    let base_oid = Some(blob_id(HashKind::Sha1, base.as_bytes()));
    let loads = std::cell::Cell::new(0);
    // The edits are loaded only to build the chain.
    let compute = || {
        loads.set(loads.get() + 1);
        let steps = [step(1, vec![replace("f.rs", "b\n", "B\n")])];
        run(Some(base), Some(worktree), &steps)
    };
    let key = codec::key(
        HashKind::Sha1,
        base_oid,
        &w,
        &digest(&[call("a", 1)], &["f.rs"]),
    );
    let first = codec::cached(&partition, key.as_ref(), compute);
    assert_eq!(loads.get(), 1);
    let hit = codec::cached(&partition, key.as_ref(), compute);
    assert_eq!(loads.get(), 1, "a hit loads nothing");
    assert_eq!(hit, first);
    assert!(
        partition
            .get::<CachedChain>(key.as_ref().unwrap())
            .is_some()
    );
    // Another session's history with the same contents is another entry.
    let key_b = codec::key(
        HashKind::Sha1,
        base_oid,
        &w,
        &digest(&[call("b", 1)], &["f.rs"]),
    );
    codec::cached(&partition, key_b.as_ref(), compute);
    assert_eq!(loads.get(), 2);
    // A chain made file-level by its sides is not written.
    let key_c = codec::key(
        HashKind::Sha1,
        base_oid,
        &w,
        &digest(&[call("c", 1)], &["f.rs"]),
    );
    let opaque = || {
        loads.set(loads.get() + 1);
        let start = path("f.rs");
        build(&ChainInput {
            hash: HashKind::Sha1,
            start: &start,
            end: &start,
            base: Side::Text(base.as_bytes()),
            worktree: Side::Opaque(None),
            steps: &[],
        })
    };
    codec::cached(&partition, key_c.as_ref(), opaque);
    codec::cached(&partition, key_c.as_ref(), opaque);
    assert_eq!(loads.get(), 4);
    cache.close();
}

#[test]
fn chain_bench_fixture_applies_every_step() {
    let run = crate::testing::chain_fixture(300, 10).run();
    assert_eq!(
        run,
        crate::testing::ChainRun {
            applied: 10,
            aligned: true,
            groups: 2,
        }
    );
}
