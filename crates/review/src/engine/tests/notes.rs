//! Review notes in the engine (`notes_cmd_*`, `notes_place_*`,
//! `notes_excerpt_*`, docs/design/review.md「测试」): the commands answer in
//! memory, the log gets their records from a task, and the notes of a
//! snapshot sit where the OID of the content they were written on says.

use lectern_logging::testing;
use lectern_services::review::{
    DiffSide, Excerpt, NoteAnchor, NoteId, NotePlace, NoteRows, NoteState, RouteKind, RouteOffer,
    SentVia, StepId, limits,
};

use super::*;
use crate::chain::version_id;
use crate::log::{NoteRecord, Record as LogRecord, SentRecord, flush};
use crate::seen::TargetKey;

const DAY: i64 = 24 * 60 * 60 * 1000;

pub(super) fn session_target(id: &str) -> TargetKey {
    TargetKey::Session {
        source: SOURCE.into(),
        id: id.into(),
    }
}

pub(super) fn excerpt(first: u32, lines: &[&str]) -> Excerpt {
    Excerpt {
        first,
        lines: lines.iter().map(|l| Arc::<str>::from(*l)).collect(),
    }
}

pub(super) fn anchor(file: FileKey, side: DiffSide, line: u32, oid: BlobId) -> NoteAnchor {
    NoteAnchor {
        file,
        side,
        line,
        oid,
    }
}

pub(super) fn add(
    rows: Option<RowsKey>,
    anchor: NoteAnchor,
    text: &str,
    excerpt: Option<Excerpt>,
) -> ReviewCommand {
    ReviewCommand::AddNote {
        rows,
        anchor,
        text: text.into(),
        excerpt,
    }
}

/// A note record for the annotation log.
pub(super) fn record(
    target: &TargetKey,
    id: u64,
    anchor: NoteAnchor,
    text: &str,
    at: Timestamp,
) -> LogRecord {
    LogRecord::Note(NoteRecord {
        at,
        target: target.clone(),
        id: NoteId(id),
        anchor,
        excerpt: Some(excerpt(0, &["seeded"])),
        text: text.into(),
        created: at,
    })
}

pub(super) fn sent(target: &TargetKey, batch: u64, ids: &[u64], at: Timestamp) -> LogRecord {
    LogRecord::NotesSent(SentRecord {
        at,
        target: target.clone(),
        batch: lectern_services::review::BatchId(batch),
        notes: ids.iter().map(|n| NoteId(*n)).collect(),
        via: SentVia::Clipboard,
    })
}

/// Writes `records` into the annotation log of the fixture's repository,
/// before any engine reads it.
pub(super) fn seed(fx: &Fx, records: Vec<LogRecord>) {
    let mut file = LogFile::new(&fx.dir.path().join("review"), fx.repo.common_dir());
    let (mut index, _) = file.load(NOW);
    for r in records {
        index.record(r);
    }
    flush(&mut file, &mut index, NOW).unwrap();
}

/// The annotation log's lines of kind `t`.
pub(super) fn lines_of(fx: &Fx, t: &str) -> Vec<String> {
    let dir = fx.dir.path().join("review");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        out.extend(
            text.lines()
                .filter(|l| l.contains(&format!("\"t\":\"{t}\"")))
                .map(str::to_owned),
        );
    }
    out
}

pub(super) struct Rig {
    pub(super) fx: Fx,
    pub(super) d: Driven,
    pub(super) id: ReviewId,
}

/// The review of session "a" of `unrelated_commit`: f.txt is "a\nb\nc\n" in
/// HEAD and "A\nb\nC\n" in the worktree, session b changed `a` to `A`,
/// session a `c` to `C`.
pub(super) fn rig() -> Rig {
    let (fx, sessions) = unrelated_commit();
    let d = fx.driven(vec![sessions]);
    let (id, _) = open(&d, target("a"));
    Rig { fx, d, id }
}

/// What a view that shows f.txt prepares: the final change and the step of
/// the review's one turn, with their keys.
pub(super) struct Shown {
    pub(super) fin: Arc<lectern_services::review::FileRows>,
    pub(super) step: Arc<lectern_services::review::FileRows>,
    pub(super) fin_key: RowsKey,
    pub(super) step_key: RowsKey,
    pub(super) step_id: StepId,
}

impl Rig {
    pub(super) fn shown(&self) -> Shown {
        let s = self.d.snapshot(self.id).unwrap();
        let step_id = s.turns.as_ref().unwrap()[0]
            .items
            .iter()
            .find_map(|i| match i {
                NarrativeItem::Step(st) => Some(st.step),
                _ => None,
            })
            .unwrap();
        let fin_key = RowsKey::Final {
            file: modified("f.txt"),
            context: 3,
        };
        let step_key = RowsKey::Step {
            step: step_id,
            context: 3,
        };
        self.d.command(
            self.id,
            ReviewCommand::Prepare(
                vec![Need::Rows(fin_key.clone()), Need::Rows(step_key.clone())].into(),
            ),
        );
        self.d.run();
        Shown {
            fin: self.d.rows(self.id, &fin_key).unwrap(),
            step: self.d.rows(self.id, &step_key).unwrap(),
            fin_key,
            step_key,
            step_id,
        }
    }

    pub(super) fn snapshot(&self) -> Arc<ReviewSnapshot> {
        self.d.snapshot(self.id).unwrap()
    }

    pub(super) fn command(&self, command: ReviewCommand) -> Arc<ReviewSnapshot> {
        self.d.command(self.id, command);
        self.snapshot()
    }

    /// The command changes nothing: the snapshot is the same `Arc`.
    pub(super) fn ignored(&self, command: ReviewCommand) {
        let before = self.snapshot();
        self.d.command(self.id, command);
        assert!(Arc::ptr_eq(&before, &self.snapshot()));
        self.d.run();
        assert!(Arc::ptr_eq(&before, &self.snapshot()));
    }
}

pub(super) fn new_oid(shown: &Shown) -> BlobId {
    shown.fin.new.oid.unwrap()
}

// ---- notes_cmd_* ----------------------------------------------------------

#[test]
fn notes_cmd_each_command_swaps_the_snapshot_before_it_returns() {
    let r = rig();
    let shown = r.shown();
    let first = r.snapshot();
    // The clipboard route is there from the first snapshot on.
    let clipboard = RouteOffer {
        id: "clipboard".into(),
        kind: RouteKind::Clipboard,
    };
    assert_eq!(&*first.notes_state.routes, &[clipboard]);
    assert!(first.notes.is_empty());

    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    let s1 = r.command(add(Some(shown.fin_key.clone()), a, "Why C?", None));
    assert!(s1.generation > first.generation, "swapped before returning");
    let n = &s1.notes[0];
    assert_eq!(
        (&*n.text, n.state, n.edited_at),
        ("Why C?", NoteState::Pending, None)
    );
    assert_eq!((n.created_at, n.id.0 >> 10), (NOW, NOW.0 as u64));
    assert_eq!(
        n.place,
        NotePlace::Rows(vec![NoteRows::Final, NoteRows::Step(shown.step_id)].into())
    );
    assert_eq!((s1.notes_state.pending, s1.notes_state.unsent), (1, 1));
    // The text is not scanned on the calling thread: the excerpt comes
    // with a task, and a new snapshot.
    assert_eq!(n.excerpt, None);
    r.d.run();
    let s2 = r.snapshot();
    assert!(s2.generation > s1.generation);
    assert_eq!(s2.notes[0].excerpt, Some(excerpt(0, &["A", "b", "C"])));

    // Edit: the text, and the time of the edit.
    r.d.set_now(Timestamp(NOW.0 + 5000));
    let id = s2.notes[0].id;
    let s3 = r.command(ReviewCommand::EditNote {
        note: id,
        text: "Why C, again?".into(),
    });
    assert!(s3.generation > s2.generation);
    let n = &s3.notes[0];
    assert_eq!(&*n.text, "Why C, again?");
    assert_eq!(n.edited_at, Some(Timestamp(NOW.0 + 5000)));
    assert_eq!(n.created_at, NOW);
    assert_eq!(n.excerpt, Some(excerpt(0, &["A", "b", "C"])));

    // A second note; remove the first; discard the rest.
    let a = anchor(modified("f.txt"), DiffSide::New, 0, new_oid(&shown));
    let s4 = r.command(add(None, a, "Second", Some(excerpt(0, &["A"]))));
    assert_eq!(s4.notes.len(), 2);
    assert_eq!((s4.notes_state.pending, s4.notes_state.unsent), (2, 2));
    let s5 = r.command(ReviewCommand::RemoveNote(id));
    assert_eq!(s5.notes.len(), 1);
    assert_eq!(&*s5.notes[0].text, "Second");
    let s6 = r.command(ReviewCommand::DiscardNotes);
    assert!(s6.notes.is_empty());
    assert_eq!((s6.notes_state.pending, s6.notes_state.unsent), (0, 0));
    assert!(s6.generation > s5.generation);
    r.d.run();
}

#[test]
fn notes_cmd_records_reach_the_log_and_survive_a_restart() {
    let r = rig();
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    r.d.command(
        r.id,
        add(Some(shown.fin_key.clone()), a.clone(), "keep", None),
    );
    r.d.command(
        r.id,
        add(None, a.clone(), "edit me", Some(excerpt(1, &["b", "C"]))),
    );
    r.d.command(
        r.id,
        add(None, a, "remove me", Some(excerpt(1, &["b", "C"]))),
    );
    let notes = r.snapshot().notes.clone();
    let by_text = |t: &str| notes.iter().find(|n| &*n.text == t).unwrap().id;
    r.d.set_now(Timestamp(NOW.0 + 1000));
    r.d.command(
        r.id,
        ReviewCommand::EditNote {
            note: by_text("edit me"),
            text: "edited".into(),
        },
    );
    r.d.command(r.id, ReviewCommand::RemoveNote(by_text("remove me")));
    // Nothing is written on the calling thread.
    assert!(lines_of(&r.fx, "note").is_empty());
    r.d.run();
    // The excerpt was taken before the record was written: one record.
    let written = lines_of(&r.fx, "note");
    assert_eq!(
        written
            .iter()
            .filter(|l| l.contains("\"text\":\"keep\""))
            .count(),
        1
    );
    assert!(
        written
            .iter()
            .any(|l| l.contains("\"lines\":[\"A\",\"b\",\"C\"]"))
    );
    assert_eq!(lines_of(&r.fx, "note_removed").len(), 1);
    let before = r.snapshot().notes.clone();
    let Rig { fx, d, .. } = r;
    drop(d);

    // A new engine on the same data directory reads them back.
    let (_, sessions) = unrelated_commit_sessions(&fx);
    let d = fx.driven(vec![sessions]);
    let (_, s) = open(&d, target("a"));
    let texts: Vec<&str> = s.notes.iter().map(|n| &*n.text).collect();
    assert_eq!(texts.len(), 2);
    assert!(texts.contains(&"keep") && texts.contains(&"edited"));
    assert!(!texts.contains(&"remove me"));
    for n in s.notes.iter() {
        let was = before.iter().find(|b| b.id == n.id).unwrap();
        assert_eq!(
            (&n.text, n.created_at, &n.excerpt),
            (&was.text, was.created_at, &was.excerpt)
        );
    }
    assert_eq!(s.notes_state.pending, 2);
}

/// The sessions of `unrelated_commit` again, for a second engine on the
/// same repository.
fn unrelated_commit_sessions(fx: &Fx) -> (&Fx, Arc<FakeSessions>) {
    let root = fx.root();
    let sessions = fx.sessions(vec![
        session(
            "b",
            &root,
            ms(T - 4000),
            vec![turn(
                "bt",
                ms(T - 3950),
                vec![edit(
                    "b1",
                    ms(T - 3900),
                    vec![replace("f.txt", "a\n", "A\n")],
                )],
            )],
        ),
        session(
            "a",
            &root,
            ms(T - 2000),
            vec![turn(
                "at",
                ms(T - 1950),
                vec![edit(
                    "a1",
                    ms(T - 1900),
                    vec![replace("f.txt", "c\n", "C\n")],
                )],
            )],
        ),
    ]);
    (fx, sessions)
}

#[test]
fn notes_cmd_ignored_commands_leave_the_snapshot_alone() {
    let r = rig();
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    let some = |text: &str| add(None, a.clone(), text, Some(excerpt(2, &["C"])));
    // Nothing but white space; one byte over the limit.
    r.ignored(some(" \n\t "));
    r.ignored(some(&"x".repeat(limits::NOTE_BYTES + 1)));
    // A file that is not in the snapshot and no excerpt to stand on.
    let elsewhere = anchor(modified("nope.txt"), DiffSide::New, 0, new_oid(&shown));
    r.ignored(add(None, elsewhere.clone(), "no file", None));
    // The note is not there.
    r.ignored(ReviewCommand::EditNote {
        note: NoteId(7),
        text: "x".into(),
    });
    r.ignored(ReviewCommand::RemoveNote(NoteId(7)));
    r.ignored(ReviewCommand::DiscardNotes);
    // The limit itself is fine.
    let s = r.command(some(&"x".repeat(limits::NOTE_BYTES)));
    assert_eq!(s.notes.len(), 1);
    let id = s.notes[0].id;
    // Edit: nothing changes, or the text is not one.
    r.ignored(ReviewCommand::EditNote {
        note: id,
        text: "x".repeat(limits::NOTE_BYTES).into(),
    });
    r.ignored(ReviewCommand::EditNote {
        note: id,
        text: "  ".into(),
    });
    // The caller's excerpt stands in for a file that is not in the review.
    let s = r.command(add(
        None,
        elsewhere,
        "context panel",
        Some(excerpt(0, &["z"])),
    ));
    assert_eq!(s.notes.len(), 2);
    assert!(s.notes.iter().any(|n| n.place == NotePlace::Elsewhere));

    // The most that are not sent: the next one is refused.
    let r = rig();
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    for n in 0..limits::PENDING_NOTES {
        r.command(add(
            None,
            a.clone(),
            &format!("note {n}"),
            Some(excerpt(2, &["C"])),
        ));
    }
    assert_eq!(r.snapshot().notes_state.unsent, limits::PENDING_NOTES);
    r.ignored(add(None, a, "one too many", Some(excerpt(2, &["C"]))));
}

#[test]
fn notes_cmd_sent_notes_cannot_be_edited_or_discarded() {
    let (fx, sessions) = unrelated_commit();
    let t = session_target("a");
    let anchor = |line| {
        anchor(
            modified("f.txt"),
            DiffSide::New,
            line,
            BlobId::from_bytes(&[9; 20]).unwrap(),
        )
    };
    let day = Timestamp(NOW.0 - DAY);
    seed(
        &fx,
        vec![
            record(&t, 0x100, anchor(0), "sent", day),
            record(&t, 0x101, anchor(1), "pending", day),
            sent(&t, 0x1ff, &[0x100], day),
        ],
    );
    let d = fx.driven(vec![sessions]);
    let (id, s) = open(&d, target("a"));
    assert_eq!(s.notes.len(), 2);
    assert_eq!((s.notes_state.pending, s.notes_state.unsent), (1, 1));
    assert_eq!(
        s.notes[0].state,
        NoteState::Sent(lectern_services::review::BatchId(0x1ff))
    );
    assert_eq!(s.notes_state.batches.len(), 1);
    assert_eq!(s.notes_state.batches[0].count, 1);
    let r = Rig { fx, d, id };
    r.ignored(ReviewCommand::EditNote {
        note: NoteId(0x100),
        text: "changed".into(),
    });
    // Discard takes the pending note only.
    let s = r.command(ReviewCommand::DiscardNotes);
    assert_eq!(s.notes.len(), 1);
    assert_eq!(&*s.notes[0].text, "sent");
    // A sent note can be removed.
    let s = r.command(ReviewCommand::RemoveNote(NoteId(0x100)));
    assert!(s.notes.is_empty());
    // The batch stays as it was sent.
    assert_eq!(s.notes_state.batches.len(), 1);
}

#[test]
fn notes_cmd_more_than_the_limit_in_the_log_are_all_kept() {
    let (fx, sessions) = unrelated_commit();
    let t = session_target("a");
    let a = anchor(
        modified("f.txt"),
        DiffSide::New,
        0,
        BlobId::from_bytes(&[9; 20]).unwrap(),
    );
    let records = (0..45u64)
        .map(|n| record(&t, 0x200 + n, a.clone(), &format!("n{n}"), NOW))
        .collect();
    seed(&fx, records);
    let d = fx.driven(vec![sessions]);
    let (id, s) = open(&d, target("a"));
    assert_eq!((s.notes.len(), s.notes_state.unsent), (45, 45));
    let r = Rig { fx, d, id };
    r.ignored(add(None, a, "more", Some(excerpt(0, &["x"]))));
}

#[test]
fn notes_cmd_old_sent_notes_are_not_in_the_snapshot() {
    let (fx, sessions) = unrelated_commit();
    let t = session_target("a");
    let a = anchor(
        modified("f.txt"),
        DiffSide::New,
        0,
        BlobId::from_bytes(&[9; 20]).unwrap(),
    );
    let old = Timestamp(NOW.0 - 15 * DAY);
    let year = Timestamp(NOW.0 - 400 * DAY);
    seed(
        &fx,
        vec![
            record(&t, 0x300, a.clone(), "sent long ago", old),
            sent(&t, 0x3ff, &[0x300], old),
            record(&t, 0x301, a, "pending for a year", year),
        ],
    );
    let d = fx.driven(vec![sessions]);
    let (_, s) = open(&d, target("a"));
    let texts: Vec<&str> = s.notes.iter().map(|n| &*n.text).collect();
    assert_eq!(texts, ["pending for a year"]);
    assert!(s.notes_state.batches.is_empty());
}

#[test]
fn notes_cmd_on_a_review_that_is_still_loading_are_ignored() {
    let (fx, sessions) = unrelated_commit();
    let d = fx.driven(vec![sessions]);
    let id = d.open(target("a"));
    let before = d.snapshot(id).unwrap();
    assert_eq!(before.state, LoadState::Loading);
    let a = anchor(
        modified("f.txt"),
        DiffSide::New,
        0,
        BlobId::from_bytes(&[9; 20]).unwrap(),
    );
    d.command(id, add(None, a, "too early", Some(excerpt(0, &["x"]))));
    assert!(Arc::ptr_eq(&before, &d.snapshot(id).unwrap()));
    d.run();
    assert!(d.snapshot(id).unwrap().notes.is_empty());
}

#[test]
fn notes_cmd_ids_are_new_each_time() {
    let r = rig();
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    for n in 0..5 {
        r.command(add(
            None,
            a.clone(),
            &format!("n{n}"),
            Some(excerpt(0, &["x"])),
        ));
    }
    let mut ids: Vec<u64> = r.snapshot().notes.iter().map(|n| n.id.0).collect();
    let all = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), all, "same clock reading, different IDs");
    assert!(ids.iter().all(|id| id >> 10 == NOW.0 as u64));
}

#[test]
fn notes_cmd_folder_project_notes_stay_in_the_review() {
    let fx = Fx::new();
    let root = fx.root();
    let sessions = fx.sessions(vec![session(
        "f",
        &root,
        ms(T - 100),
        vec![turn("t", ms(T - 90), vec![])],
    )]);
    let d = fx.driven(vec![Arc::clone(&sessions)]);
    sessions.place_in_folder("f", &d.project());
    let id = d.open(target("f"));
    d.run();
    let s = d.snapshot(id).unwrap();
    assert_eq!(
        s.degraded,
        Some(lectern_services::review::Degraded::FolderProject)
    );
    assert_eq!(
        &*s.notes_state.routes,
        &[RouteOffer {
            id: "clipboard".into(),
            kind: RouteKind::Clipboard,
        }]
    );
    let a = anchor(
        modified("src/a.rs"),
        DiffSide::New,
        3,
        BlobId::from_bytes(&[5; 20]).unwrap(),
    );
    // No file in the snapshot: the caller's excerpt is what it stands on.
    d.command(id, add(None, a.clone(), "no excerpt", None));
    assert!(d.snapshot(id).unwrap().notes.is_empty());
    d.command(id, add(None, a, "context", Some(excerpt(2, &["x", "y"]))));
    let s = d.snapshot(id).unwrap();
    assert_eq!(
        (s.notes.len(), s.notes[0].place.clone()),
        (1, NotePlace::Elsewhere)
    );
    // A new round keeps them; nothing is written; a release drops them.
    d.command(id, ReviewCommand::Refresh);
    d.run();
    assert_eq!(d.snapshot(id).unwrap().notes.len(), 1);
    assert!(!fx.dir.path().join("review").exists());
    d.close(id);
    let id = d.open(target("f"));
    d.run();
    assert!(d.snapshot(id).unwrap().notes.is_empty());
}

#[test]
fn notes_cmd_logs_kinds_and_counts_never_contents() {
    let log = testing::capture();
    const CANARY: &str = "canary-n0t3-5c1f94";
    const UUID: &str = "5e55e55e-2222-4222-8222-0000000000b1";
    let fx = Fx::new();
    let c1 = fx.commit(&[("a.txt", "one\ntwo\n")], None, T - 1000);
    fx.head("refs/heads/main", c1, &[("a.txt", "one\ntwo\n")]);
    fx.reflog(&[(None, c1, T - 1000, "commit (initial): start")]);
    fx.write("a.txt", &format!("one\ntwo {CANARY}\n"));
    let root = fx.root();
    let sessions = fx.sessions(vec![session(
        UUID,
        &root,
        ms(T - 500),
        vec![turn(
            "t",
            ms(T - 450),
            vec![edit(
                "e1",
                ms(T - 400),
                vec![replace("a.txt", "two\n", &format!("two {CANARY}\n"))],
            )],
        )],
    )]);
    let d = fx.driven(vec![sessions]);
    let (id, s) = open(&d, target(UUID));
    let file = s.files[0].key.clone();
    d.command(
        id,
        ReviewCommand::Prepare(
            vec![Need::Rows(RowsKey::Final {
                file: file.clone(),
                context: 3,
            })]
            .into(),
        ),
    );
    d.run();
    let rows = d
        .rows(
            id,
            &RowsKey::Final {
                file: file.clone(),
                context: 3,
            },
        )
        .unwrap();
    let a = anchor(file, DiffSide::New, 1, rows.new.oid.unwrap());
    d.command(
        id,
        add(
            Some(RowsKey::Final {
                file: a.file.clone(),
                context: 3,
            }),
            a.clone(),
            &format!("note text {CANARY}"),
            None,
        ),
    );
    d.command(id, add(None, a, "  ", None));
    d.run();
    // The commands run on the calling thread, outside the review's scope:
    // this test's records are told by its thread.
    let mine = |msg: &str| -> Vec<String> {
        log.lines()
            .into_iter()
            .filter(|l| {
                l.contains(&format!("\"msg\":\"{msg}\""))
                    && l.contains("notes::notes_cmd_logs_kinds")
            })
            .collect()
    };
    let changed = mine("note changed");
    assert_eq!(changed.len(), 1, "{:#?}", log.lines());
    for field in [
        "\"op\":\"add\"",
        "\"pending\":1",
        "\"sent\":0",
        "\"place\":\"rows\"",
    ] {
        assert!(changed[0].contains(field), "{field} in {}", changed[0]);
    }
    let ignored = mine("note command ignored");
    assert_eq!(ignored.len(), 1);
    assert!(
        ignored[0].contains("\"reason\":\"empty\""),
        "{}",
        ignored[0]
    );
    for line in log.lines() {
        assert!(!line.contains(CANARY), "{line}");
    }
    // The excerpt and the text are in the annotation log, not in the
    // runtime log.
    assert!(lines_of(&fx, "note")[0].contains(CANARY));
}

// ---- notes_place_* --------------------------------------------------------

#[test]
fn notes_place_rows_of_the_final_change_the_step_or_both() {
    let r = rig();
    let shown = r.shown();
    let f = modified("f.txt");
    let ex = Some(excerpt(0, &["x"]));
    let at = |side, oid| anchor(f.clone(), side, 0, oid);
    // The base side: only the final change has it.
    r.command(add(
        None,
        at(DiffSide::Old, shown.fin.old.oid.unwrap()),
        "base",
        ex.clone(),
    ));
    // The version before the step (B's work): only the step.
    r.command(add(
        None,
        at(DiffSide::Old, shown.step.old.oid.unwrap()),
        "before",
        ex.clone(),
    ));
    // The worktree: the last step's new side is the worktree.
    r.command(add(
        None,
        at(DiffSide::New, new_oid(&shown)),
        "worktree",
        ex,
    ));
    let s = r.snapshot();
    let place = |t: &str| {
        s.notes
            .iter()
            .find(|n| &*n.text == t)
            .unwrap()
            .place
            .clone()
    };
    assert_eq!(place("base"), NotePlace::Rows(vec![NoteRows::Final].into()));
    assert_eq!(
        place("before"),
        NotePlace::Rows(vec![NoteRows::Step(shown.step_id)].into())
    );
    assert_eq!(
        place("worktree"),
        NotePlace::Rows(vec![NoteRows::Final, NoteRows::Step(shown.step_id)].into())
    );
    // The same OID on the other side is another content.
    r.command(add(
        None,
        at(DiffSide::Old, new_oid(&shown)),
        "other side",
        Some(excerpt(0, &["x"])),
    ));
    let s = r.snapshot();
    let n = s.notes.iter().find(|n| &*n.text == "other side").unwrap();
    assert_eq!(n.place, NotePlace::Drifted { file: f });
}

#[test]
fn notes_place_drifted_after_the_code_changes() {
    // The worktree has a line no step wrote: its version is the final
    // change's alone.
    let (fx, sessions) = unrelated_commit();
    fx.write("f.txt", "A\nb\nC\nby hand\n");
    let d = fx.driven(vec![sessions]);
    let (id, _) = open(&d, target("a"));
    let r = Rig { fx, d, id };
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 3, new_oid(&shown));
    r.command(add(None, a, "about the line", Some(excerpt(0, &["x"]))));
    assert_eq!(
        r.snapshot().notes[0].place,
        NotePlace::Rows(vec![NoteRows::Final].into())
    );
    // The code moves on: the note does not follow the line.
    r.fx.write("f.txt", "A\nb\nC\nby hand, again\n");
    r.d.command(r.id, ReviewCommand::Refresh);
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes[0].place,
        NotePlace::Drifted {
            file: modified("f.txt")
        }
    );
    assert_eq!(s.notes[0].anchor.line, 3, "the anchor is as it was written");
}

#[test]
fn notes_place_on_a_step_stays_when_the_worktree_moves_on() {
    let r = rig();
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    r.command(add(None, a, "about C", Some(excerpt(0, &["A", "b", "C"]))));
    assert_eq!(
        r.snapshot().notes[0].place,
        NotePlace::Rows(vec![NoteRows::Final, NoteRows::Step(shown.step_id)].into())
    );
    // Someone edits the file: the step still has the version, the final
    // change no longer does.
    r.fx.write("f.txt", "A\nb\nCC\n");
    r.d.command(r.id, ReviewCommand::Refresh);
    r.d.run();
    assert_eq!(
        r.snapshot().notes[0].place,
        NotePlace::Rows(vec![NoteRows::Step(shown.step_id)].into())
    );
}

#[test]
fn notes_place_elsewhere_when_the_file_has_no_change_any_more() {
    let r = rig();
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    r.command(add(None, a, "first", Some(excerpt(0, &["x"]))));
    let g = anchor(
        modified("f.txt"),
        DiffSide::Old,
        0,
        shown.fin.old.oid.unwrap(),
    );
    r.command(add(None, g, "second", Some(excerpt(0, &["x"]))));
    r.fx.write("f.txt", "a\nb\nc\n");
    r.d.command(r.id, ReviewCommand::Refresh);
    r.d.run();
    let s = r.snapshot();
    assert!(s.files.is_empty());
    assert_eq!(s.notes.len(), 2);
    assert!(s.notes.iter().all(|n| n.place == NotePlace::Elsewhere));
    // Of no file: in the order they were written.
    assert_eq!(&*s.notes[0].text, "first");
}

#[test]
fn notes_place_follows_a_rename_by_path() {
    let fx = Fx::new();
    let body: String = (0..12).map(|n| format!("line {n} of the file\n")).collect();
    let c1 = fx.commit(&[("old.txt", &body)], None, T - 1000);
    let moved = body.replace("line 5 of the file", "line 5 changed");
    // Staged as a rename: the deleted and the added file are paired.
    fx.head("refs/heads/main", c1, &[("new.txt", &moved)]);
    fx.reflog(&[(None, c1, T - 1000, "commit (initial): start")]);
    let sessions = fx.sessions(vec![]);
    let t = TargetKey::Outside {
        worktree: "".into(),
    };
    // Written while the file was still old.txt: on its base side, and on
    // the worktree side of the time (which is gone now).
    let base = oid(fx.repo.blob(body.as_bytes())).unwrap();
    let gone = BlobId::from_bytes(&[8; 20]).unwrap();
    seed(
        &fx,
        vec![
            record(
                &t,
                0x400,
                anchor(modified("old.txt"), DiffSide::Old, 5, base),
                "on the base",
                NOW,
            ),
            record(
                &t,
                0x401,
                anchor(modified("old.txt"), DiffSide::New, 5, gone),
                "on a gone version",
                NOW,
            ),
        ],
    );
    // The worktree moves the file.
    fx.write("new.txt", &moved);
    let d = fx.driven(vec![sessions]);
    let (_, s) = open(&d, d.outside());
    let renamed = FileKey {
        old: Some(path("old.txt")),
        new: Some(path("new.txt")),
    };
    assert!(
        matches!(s.files[0].change.status, ChangeStatus::Renamed { .. }),
        "{:?}",
        s.files[0].change.status
    );
    assert_eq!(s.files[0].key, renamed);
    let place = |t: &str| {
        s.notes
            .iter()
            .find(|n| &*n.text == t)
            .unwrap()
            .place
            .clone()
    };
    assert_eq!(
        place("on the base"),
        NotePlace::Rows(vec![NoteRows::Final].into())
    );
    assert_eq!(
        place("on a gone version"),
        NotePlace::Drifted { file: renamed }
    );
}

#[test]
fn notes_place_attribution_pending_is_drifted_until_it_is_computed() {
    let fx = Fx::new();
    let (f0, g0) = ("a\nb\nc\n", "1\n2\n3\n");
    let c1 = fx.commit(&[("f.txt", f0), ("g.txt", g0)], None, T - 5000);
    fx.head("refs/heads/main", c1, &[("f.txt", f0), ("g.txt", g0)]);
    fx.reflog(&[(None, c1, T - 5000, "commit (initial): start")]);
    fx.write("f.txt", "A\nb\nC\n");
    fx.write("g.txt", "X\n2\nZ\n");
    let root = fx.root();
    let sessions = fx.sessions(vec![session(
        "a",
        &root,
        ms(T - 3000),
        vec![
            turn(
                "t1",
                ms(T - 2950),
                vec![
                    edit("f1", ms(T - 2900), vec![replace("f.txt", "a\n", "A\n")]),
                    edit("g1", ms(T - 2800), vec![replace("g.txt", "1\n", "X\n")]),
                ],
            ),
            turn(
                "t2",
                ms(T - 2000),
                vec![
                    edit("f2", ms(T - 1900), vec![replace("f.txt", "c\n", "C\n")]),
                    edit("g2", ms(T - 1800), vec![replace("g.txt", "3\n", "Z\n")]),
                ],
            ),
        ],
    )]);
    // The versions after the first turn: neither is a final side.
    let hash = fx.repo.hash();
    let mid_f = version_id(hash, b"A\nb\nc\n");
    let mid_g = version_id(hash, b"X\n2\n3\n");
    let t = session_target("a");
    seed(
        &fx,
        vec![
            record(
                &t,
                0x500,
                anchor(modified("f.txt"), DiffSide::New, 0, mid_f),
                "f",
                NOW,
            ),
            record(
                &t,
                0x501,
                anchor(modified("g.txt"), DiffSide::New, 0, mid_g),
                "g",
                NOW,
            ),
        ],
    );
    let d = fx.driven(vec![sessions]);
    d.capture_snapshots();
    let (_, last) = open(&d, target("a"));
    let all = d.take_snapshots();
    let places = |s: &ReviewSnapshot| -> Vec<bool> {
        s.notes
            .iter()
            .map(|n| matches!(n.place, NotePlace::Rows(_)))
            .collect()
    };
    // The first snapshot has the first file in the narrative's order
    // attributed, not the other: one note sits on its step, one is drifted.
    // The log is read in the background, before the other file is.
    let first = all.iter().find(|s| !s.notes.is_empty()).unwrap();
    assert_eq!(first.notes.len(), 2);
    assert_eq!(
        places(first).iter().filter(|r| **r).count(),
        1,
        "{:?}",
        places(first)
    );
    // Once both are, both sit on the step they were written on.
    assert_eq!(places(&last), [true, true]);
    for n in last.notes.iter() {
        let NotePlace::Rows(rows) = &n.place else {
            panic!("{:?}", n.place);
        };
        assert!(matches!(&rows[..], [NoteRows::Step(_)]), "{rows:?}");
    }
}

#[test]
fn notes_place_orders_by_file_then_line_and_puts_elsewhere_last() {
    let r = rig();
    let shown = r.shown();
    let f = modified("f.txt");
    let new = new_oid(&shown);
    let ex = Some(excerpt(0, &["x"]));
    let other = anchor(modified("gone.txt"), DiffSide::New, 0, new);
    r.command(add(None, other.clone(), "elsewhere 1", ex.clone()));
    r.d.set_now(Timestamp(NOW.0 + 10));
    r.command(add(
        None,
        anchor(f.clone(), DiffSide::New, 2, new),
        "line 2",
        ex.clone(),
    ));
    r.d.set_now(Timestamp(NOW.0 + 20));
    r.command(add(
        None,
        anchor(f.clone(), DiffSide::New, 0, new),
        "line 0",
        ex.clone(),
    ));
    r.d.set_now(Timestamp(NOW.0 + 30));
    r.command(add(None, other, "elsewhere 2", ex));
    let texts: Vec<String> = r
        .snapshot()
        .notes
        .iter()
        .map(|n| n.text.to_string())
        .collect();
    assert_eq!(texts, ["line 0", "line 2", "elsewhere 1", "elsewhere 2"]);
}

#[test]
fn notes_place_is_reused_when_nothing_changed() {
    let r = rig();
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    r.command(add(None, a, "n", Some(excerpt(0, &["x"]))));
    let before = r.snapshot();
    // A seen mark rebuilds the snapshot; the notes are the same `Arc`s.
    let s = r.command(ReviewCommand::LooksGood);
    assert!(s.generation > before.generation);
    assert!(Arc::ptr_eq(&s.notes, &before.notes));
    assert!(Arc::ptr_eq(&s.notes_state, &before.notes_state));
}

// ---- notes_excerpt_* -------------------------------------------------------

#[test]
fn notes_excerpt_comes_from_the_prepared_rows_in_the_background() {
    let r = rig();
    let shown = r.shown();
    // The old side: "a\nb\nc\n", line 1.
    let a = anchor(
        modified("f.txt"),
        DiffSide::Old,
        1,
        shown.fin.old.oid.unwrap(),
    );
    let s = r.command(add(Some(shown.fin_key.clone()), a, "b?", None));
    assert_eq!(s.notes[0].excerpt, None);
    r.d.run();
    assert_eq!(
        r.snapshot().notes[0].excerpt,
        Some(excerpt(0, &["a", "b", "c"]))
    );
    // The step's rows: the other side's text.
    let a = anchor(
        modified("f.txt"),
        DiffSide::Old,
        0,
        shown.step.old.oid.unwrap(),
    );
    r.command(add(Some(shown.step_key.clone()), a, "step", None));
    r.d.run();
    let s = r.snapshot();
    let n = s.notes.iter().find(|n| &*n.text == "step").unwrap();
    assert_eq!(n.excerpt, Some(excerpt(0, &["A", "b", "c"])));
}

#[test]
fn notes_excerpt_is_none_when_the_rows_were_dropped_or_are_another_content() {
    let r = rig();
    let shown = r.shown();
    // The OID of the anchor is not the side's.
    let wrong = anchor(
        modified("f.txt"),
        DiffSide::New,
        2,
        shown.fin.old.oid.unwrap(),
    );
    r.command(add(
        Some(shown.fin_key.clone()),
        wrong,
        "wrong content",
        Some(excerpt(9, &["kept"])),
    ));
    // …without the caller's, nothing is taken.
    let wrong = anchor(
        modified("f.txt"),
        DiffSide::New,
        2,
        shown.fin.old.oid.unwrap(),
    );
    r.command(add(Some(shown.fin_key.clone()), wrong, "no excerpt", None));
    // The view moved on: a request without the rows drops them.
    r.d.command(r.id, ReviewCommand::Prepare(Arc::from(Vec::new())));
    r.d.run();
    assert!(r.d.rows(r.id, &shown.fin_key).is_none());
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    r.command(add(Some(shown.fin_key.clone()), a, "dropped", None));
    r.d.run();
    let s = r.snapshot();
    let excerpt_of = |t: &str| {
        s.notes
            .iter()
            .find(|n| &*n.text == t)
            .unwrap()
            .excerpt
            .clone()
    };
    assert_eq!(excerpt_of("wrong content"), Some(excerpt(9, &["kept"])));
    assert_eq!(excerpt_of("no excerpt"), None);
    assert_eq!(excerpt_of("dropped"), None);
    // Still written: a note without an excerpt is a note.
    assert_eq!(lines_of(&r.fx, "note").len(), 3);
}

#[test]
fn notes_excerpt_given_by_the_caller_is_kept_as_it_is() {
    let r = rig();
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    let given = excerpt(40, &["not", "the", "file's", "lines"]);
    // The rows are there too; the caller's lines win.
    r.command(add(
        Some(shown.fin_key.clone()),
        a,
        "context",
        Some(given.clone()),
    ));
    r.d.run();
    assert_eq!(r.snapshot().notes[0].excerpt, Some(given));
    let written = lines_of(&r.fx, "note");
    assert_eq!(written.len(), 1);
    assert!(written[0].contains("\"first\":40"));
}

#[test]
fn notes_cmd_a_new_id_never_replaces_a_note() {
    // The log holds a note with the ID the clock would give next (written
    // by a clock that was ahead).
    let (fx, sessions) = unrelated_commit();
    let t = session_target("a");
    let a = anchor(
        modified("f.txt"),
        DiffSide::New,
        0,
        BlobId::from_bytes(&[9; 20]).unwrap(),
    );
    let same = NOW.0 as u64 * 1024;
    seed(
        &fx,
        vec![
            record(&t, same, a.clone(), "from the log", NOW),
            record(&t, same + 1, a.clone(), "and the next", NOW),
        ],
    );
    let d = fx.driven(vec![sessions]);
    let (id, _) = open(&d, target("a"));
    let r = Rig { fx, d, id };
    let s = r.command(add(None, a, "new", Some(excerpt(0, &["x"]))));
    let ids: Vec<u64> = s.notes.iter().map(|n| n.id.0).collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert!(s.notes.iter().any(|n| &*n.text == "from the log"));
    assert!(s.notes.iter().any(|n| &*n.text == "and the next"));
}
