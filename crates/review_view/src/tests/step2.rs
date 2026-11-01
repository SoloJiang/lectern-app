//! Step 2 of the design's acceptance: highlighting on the executor, the
//! handoff and usage plugins, the side-by-side diff, folds that expand and the Analyze
//! commands (docs/design/review_view.md「测试」「验收」第 2 步).

use std::sync::Arc;

use gpui::TestAppContext;
use lectern_services::changes::SideText;
use lectern_services::code_intel::{Focus, TextOrigin};
use lectern_services::project::Location;
use lectern_services::project::{BlobId, ProjectKey, WorktreeKey};
use lectern_services::reader::Anchor;
use lectern_services::review::{
    AnalysisState, DiffRow, FileRows, HunkAttribution, HunkInfo, InlineSpans, Need, ReviewCommand,
    ReviewSnapshot, RowsKey, RowsSide, StepId,
};
use lectern_services::session::Place;
use lectern_services::usage::{
    BranchLabel, RowKey, SessionRow, SessionUsage, SnapshotHeader, Tokens, UsageEvent, UsageQuery,
    UsageSnapshot, UsageState, UseId,
};
use lectern_services::workspace::WorkspaceCommand;
use lectern_syntax::TokenKind;
use lectern_ui::semantic::Record;

use crate::highlight::Half;
use crate::items::{DiffMode, Item, ItemKey};
use crate::testing::{FIRST_ID, file_key, session_key, snapshot_fixture};
use crate::tests::screen::{attr, find, must, session};
use crate::tests::support::{Opened, Rig, summary};

pub const OLD: &str = "fn main() {\n    let a = 1;\n    print(\"hi\");\n}\n";
pub const NEW: &str = "fn main() {\n    let a = 2;\n    print(\"hi\");\n}\n";

pub fn old_oid() -> BlobId {
    BlobId::Sha1([1; 20])
}

pub fn new_oid() -> BlobId {
    BlobId::Sha1([2; 20])
}

/// A context line, a removed line with its added partner, two context lines:
/// `OLD` and `NEW` side by side, the changed word of the pair marked.
pub fn sample_rows(key: RowsKey) -> FileRows {
    let rows = vec![
        DiffRow::Context {
            old: 0,
            new: 0,
            old_byte: Some(0),
            new_byte: Some(0),
            hunk: Some(0),
        },
        DiffRow::Removed {
            old: 1,
            pair: Some(1),
            old_byte: Some(12),
            hunk: 0,
        },
        DiffRow::Added {
            new: 1,
            pair: Some(1),
            new_byte: Some(12),
            hunk: 0,
        },
        DiffRow::Context {
            old: 2,
            new: 2,
            old_byte: Some(27),
            new_byte: Some(27),
            hunk: Some(0),
        },
        DiffRow::Context {
            old: 3,
            new: 3,
            old_byte: Some(44),
            new_byte: Some(44),
            hunk: Some(0),
        },
    ];
    FileRows {
        key,
        old: RowsSide {
            oid: Some(old_oid()),
            text: SideText::Text(Arc::from(OLD.as_bytes())),
        },
        new: RowsSide {
            oid: Some(new_oid()),
            text: SideText::Text(Arc::from(NEW.as_bytes())),
        },
        rows: Arc::from(rows),
        hunks: Arc::from(vec![HunkInfo {
            old: 0..4,
            new: 0..4,
            attribution: HunkAttribution {
                lines: true,
                turns: Arc::from(Vec::new()),
                others: Arc::from(Vec::new()),
                drifted: 0,
            },
        }]),
        inline: Arc::from(vec![(
            1,
            InlineSpans {
                old: Arc::from_iter(std::iter::once(12..13)),
                new: Arc::from_iter(std::iter::once(12..13)),
            },
        )]),
    }
}

pub fn step_key() -> RowsKey {
    RowsKey::Step {
        step: StepId(0),
        context: 3,
    }
}

pub fn final_key() -> RowsKey {
    RowsKey::Final {
        file: file_key(0),
        context: 3,
    }
}

/// A one-file session review whose diffs are the sample rows.
pub fn sample_snapshot() -> ReviewSnapshot {
    let (snapshot, _) = snapshot_fixture(1, 1, 4);
    snapshot
}

/// The rig with the sample review open, the rows set and answered at once.
pub fn open_sample(cx: &mut TestAppContext, rig: &Rig) -> Opened {
    rig.review.set_auto_prepare(true);
    for key in [step_key(), final_key()] {
        rig.review.set_rows(FIRST_ID, key.clone(), sample_rows(key));
    }
    rig.review
        .set_text(FIRST_ID, crate::testing::prompt_key(0), "Make the change.");
    rig.review
        .set_text(FIRST_ID, crate::testing::agent_key(0), "Done.");
    let opened = rig.open(cx, session(), Some(sample_snapshot()));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    opened
}

fn project() -> ProjectKey {
    ProjectKey::Folder {
        root: Arc::from(std::path::Path::new("/fake")),
    }
}

fn worktree() -> WorktreeKey {
    WorktreeKey(Arc::from(std::path::Path::new("/fake")))
}

/// The session is located in the project, as a real session list would
/// say once the project is open.
pub fn locate(rig: &Rig) {
    let mut located = summary(session_key(), "Fix the parser");
    located.place = Place::Located(Location {
        project: project(),
        worktree: Some(worktree()),
        rel: Arc::from(""),
        package: lectern_services::project::PackageMatch::None,
    });
    rig.sessions.set(vec![located]);
}

/// The screen goes away: the view leaves the window and the model is
/// released.
fn close(cx: &mut TestAppContext, opened: Opened) {
    let window = opened.window;
    drop(opened);
    window
        .update(cx, |host, _, cx| {
            host.clear_child();
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();
}

fn select(opened: &Opened, cx: &mut TestAppContext, key: ItemKey) {
    opened.update(cx, |model, cx| model.select(Some(key), cx));
}

fn row_key(rows: &RowsKey, old: Option<u32>, new: Option<u32>) -> ItemKey {
    ItemKey::Row {
        rows: rows.clone(),
        old,
        new,
    }
}

// ----- highlighting ---------------------------------------------------------

#[gpui::test]
fn highlight_runs_on_the_executor_and_colours_after_the_fact(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = open_sample(cx, &rig);
    // The rows are drawn, plain, before any task ran.
    let records = opened.record(cx);
    let diff = must(&records, "review.diff.item[step:0]");
    assert_eq!(attr(diff, "prepared").as_deref(), Some("true"));
    assert_eq!(
        attr(diff, "highlighted").as_deref(),
        Some("old=plain,new=plain")
    );
    assert!(opened.read(cx, |model| !model.inflight.is_empty()));

    // The tasks run: the spans arrive and the next frame is coloured.
    assert!(rig.run_tasks(cx) > 0);
    let records = opened.record(cx);
    let diff = must(&records, "review.diff.item[step:0]");
    assert_eq!(
        attr(diff, "highlighted").as_deref(),
        Some("old=spans,new=spans")
    );
    let keyword = opened.read(cx, |model| {
        model.highlighted[&step_key()]
            .line(Half::New, 12)
            .iter()
            .find(|token| token.kind() == TokenKind::Keyword)
            .map(|token| (token.start(), token.len()))
    });
    assert_eq!(
        keyword,
        Some((4, 3)),
        "`let` is a keyword in the added line"
    );
    assert!(opened.read(cx, |model| model.inflight.is_empty()));
}

#[gpui::test]
fn highlight_cache_is_shared_by_oid(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = open_sample(cx, &rig);
    // The step's diff and the file's final diff hold the same two blobs:
    // two windows, one task each, not four.
    let tasks = rig.run_tasks(cx);
    assert_eq!(tasks, 2, "one task per blob and window");
    let both = opened.read(cx, |model| {
        (
            model
                .highlighted
                .get(&step_key())
                .map(|s| s.state(Half::New)),
            model
                .highlighted
                .get(&final_key())
                .map(|s| s.state(Half::New)),
        )
    });
    // The final diff is not prepared until it is on screen (the narrative
    // shows steps); when it is, the cache serves it without a task.
    assert_eq!(both.0, Some("spans"));
    let again = opened.update(cx, |model, _| {
        let rows = Arc::new(sample_rows(final_key()));
        model.store_rows(&final_key(), rows)
    });
    assert!(again);
    assert!(
        opened.read(cx, |model| model.inflight.is_empty()),
        "the cache had both windows"
    );
    assert_eq!(
        opened.read(cx, |model| model.highlighted[&final_key()].state(Half::Old)),
        "spans"
    );
}

#[gpui::test]
fn highlight_leaves_a_file_without_a_language_plain(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, _) = snapshot_fixture(1, 1, 4);
    // A path no grammar claims.
    let plain = lectern_services::changes::ChangePath(Arc::from(&b"notes.unknownext"[..]));
    let key = lectern_services::review::FileKey {
        old: Some(plain.clone()),
        new: Some(plain),
    };
    let mut files = snapshot.files.to_vec();
    files[0].key = key.clone();
    snapshot.files = Arc::from(files);
    let mut turns = snapshot.turns.as_deref().unwrap().to_vec();
    let mut items = turns[0].items.to_vec();
    for item in &mut items {
        if let lectern_services::review::NarrativeItem::Step(step) = item {
            step.file = key.clone();
        }
    }
    turns[0].items = Arc::from(items);
    snapshot.turns = Some(Arc::from(turns));
    rig.review.set_auto_prepare(true);
    rig.review
        .set_rows(FIRST_ID, step_key(), sample_rows(step_key()));
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    assert_eq!(rig.run_tasks(cx), 0, "nothing to run");
    let records = opened.record(cx);
    assert_eq!(
        attr(must(&records, "review.diff.item[step:0]"), "highlighted").as_deref(),
        Some("old=plain,new=plain")
    );
}

#[gpui::test]
fn highlight_results_of_rows_that_left_are_dropped(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = open_sample(cx, &rig);
    assert!(opened.read(cx, |model| !model.inflight.is_empty()));
    // The rows leave the held set before the task ran: it is cancelled and
    // what it brings is not kept.
    opened.update(cx, |model, _| {
        model.rows.remove(&step_key());
        model.drop_rows(&step_key());
    });
    assert!(opened.read(cx, |model| model.inflight.is_empty()));
    rig.run_tasks(cx);
    assert!(opened.read(cx, |model| !model.highlighted.contains_key(&step_key())));
}

// ----- the reader ------------------------------------------------------------

#[gpui::test]
fn open_file_hands_off_to_the_reader(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.enable_handoff(cx);
    locate(&rig);
    let opened = open_sample(cx, &rig);
    // A removed line is on the old side, a blob.
    select(&opened, cx, row_key(&step_key(), Some(1), None));
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::OpenFile);
    visual.run_until_parked();
    let targets = rig.reader.opened();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].origin, TextOrigin::Object);
    assert_eq!(targets[0].expect, Some(old_oid()));
    assert_eq!(
        targets[0].at,
        Anchor::Byte {
            byte: 12,
            line: 1,
            column: 0
        }
    );
    assert_eq!(&*targets[0].path, "src/file_000.rs");

    // An added line is on the new side; in the step's diff that side is a
    // blob too.
    select(&opened, cx, row_key(&step_key(), None, Some(1)));
    visual.dispatch_action(crate::actions::OpenFile);
    visual.run_until_parked();
    let targets = rig.reader.opened();
    assert_eq!(targets[1].origin, TextOrigin::Object);
    assert_eq!(targets[1].expect, Some(new_oid()));
}

#[gpui::test]
fn open_file_in_a_final_diff_reads_the_working_tree(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.enable_handoff(cx);
    // A review of a range has no turns: the files' final diffs are shown.
    let target = crate::testing::head_range();
    let (mut snapshot, _) = snapshot_fixture(1, 1, 4);
    snapshot.turns = None;
    rig.review.set_auto_prepare(true);
    rig.review
        .set_rows(FIRST_ID, final_key(), sample_rows(final_key()));
    let opened = rig.open(cx, target, Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    select(&opened, cx, row_key(&final_key(), Some(2), Some(2)));
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::Edit);
    visual.run_until_parked();
    let targets = rig.reader.opened();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].origin, TextOrigin::Worktree);
    assert_eq!(targets[0].expect, Some(new_oid()));
    assert_eq!(
        targets[0].at,
        Anchor::Byte {
            byte: 27,
            line: 2,
            column: 0
        }
    );
    // The removed line of a final diff is a blob.
    select(&opened, cx, row_key(&final_key(), Some(1), None));
    visual.dispatch_action(crate::actions::OpenFile);
    visual.run_until_parked();
    let targets = rig.reader.opened();
    assert_eq!(targets[1].origin, TextOrigin::Object);
    assert_eq!(targets[1].expect, Some(old_oid()));
}

#[gpui::test]
fn open_file_without_the_handoff_is_disabled_and_logged(cx: &mut TestAppContext) {
    let capture = lectern_logging::testing::capture();
    let rig = Rig::new(cx);
    locate(&rig);
    let opened = open_sample(cx, &rig);
    select(&opened, cx, row_key(&step_key(), Some(1), None));
    let records = opened.record(cx);
    let button = must(&records, "review.step.item[0].open");
    assert!(button.semantic.state.disabled);
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::OpenFile);
    visual.run_until_parked();
    assert!(rig.reader.opened().is_empty());
    let lines = capture.lines();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"msg\":\"handoff unavailable\"")),
        "{lines:?}"
    );
}

#[gpui::test]
fn share_object_follows_the_rows(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.enable_handoff(cx);
    let opened = open_sample(cx, &rig);
    // The step's rows: both sides registered.
    assert_eq!(rig.reader.total(), 2);
    assert_eq!(rig.reader.live(), 2);
    let oids: Vec<_> = rig
        .reader
        .registered()
        .into_iter()
        .map(|(oid, _)| oid)
        .collect();
    assert_eq!(oids, [old_oid(), new_oid()]);
    // The rows leave the held set: the registrations drop with them.
    opened.update(cx, |model, _| {
        model.rows.remove(&step_key());
        model.drop_rows(&step_key());
    });
    assert_eq!(rig.reader.live(), 0);
    // The screen going away drops what is left.
    opened.update(cx, |model, cx| {
        let rows = Arc::new(sample_rows(final_key()));
        model.store_rows(&final_key(), rows);
        cx.notify();
    });
    assert_eq!(rig.reader.live(), 2);
    close(cx, opened);
    assert_eq!(rig.reader.live(), 0);
}

// ----- the code navigation -------------------------------------------------------

#[gpui::test]
fn focus_is_set_from_the_snapshot(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.enable_handoff(cx);
    locate(&rig);
    let mut snapshot = sample_snapshot();
    let review: Arc<[Arc<str>]> = Arc::from(vec![Arc::<str>::from("src/file_000.rs")]);
    let touched: Arc<[Arc<str>]> = Arc::from(vec![Arc::<str>::from("README.md")]);
    snapshot.focus_review = review.clone();
    snapshot.focus_touched = touched.clone();
    rig.review.set_auto_prepare(true);
    for key in [step_key(), final_key()] {
        rig.review.set_rows(FIRST_ID, key.clone(), sample_rows(key));
    }
    let opened = rig.open(cx, session(), Some(snapshot.clone()));
    opened.draw(cx);
    rig.settle(cx);
    assert_eq!(rig.code_intel.of("review"), [Focus::Review(review.clone())]);
    assert_eq!(
        rig.code_intel.of("touched"),
        [Focus::Touched(touched.clone())]
    );
    let (project_sent, worktree_sent, _) = rig.code_intel.focus().remove(0);
    assert_eq!(project_sent, project());
    assert_eq!(worktree_sent, Some(worktree()));

    // A newer snapshot with other lists sends them again; the same lists don't.
    let mut next = snapshot.clone();
    next.focus_review = Arc::from(vec![
        Arc::<str>::from("src/file_000.rs"),
        Arc::<str>::from("src/other.rs"),
    ]);
    next.id = FIRST_ID;
    rig.review.set_snapshot(FIRST_ID, next.clone());
    rig.review.emit_changed(FIRST_ID);
    rig.settle(cx);
    assert_eq!(rig.code_intel.of("review").len(), 2);
    assert_eq!(rig.code_intel.of("touched").len(), 1, "unchanged");

    // A selected row becomes `Current`; clearing the selection sends nothing;
    // closing the screen clears it once.
    select(&opened, cx, row_key(&step_key(), None, Some(1)));
    let current = rig.code_intel.of("current");
    assert_eq!(current.len(), 1);
    let Focus::Current(Some(text)) = &current[0] else {
        panic!("a text: {current:?}");
    };
    assert_eq!(&*text.path, "src/file_000.rs");
    assert_eq!(text.oid, new_oid());
    assert_eq!(text.origin, TextOrigin::Object);
    opened.update(cx, |model, cx| model.select(None, cx));
    assert_eq!(rig.code_intel.of("current").len(), 1);
    close(cx, opened);
    let current = rig.code_intel.of("current");
    assert_eq!(current.len(), 2);
    assert_eq!(current[1], Focus::Current(None));
}

#[gpui::test]
fn space_opens_the_context_panel_on_the_selected_row(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.enable_handoff(cx);
    locate(&rig);
    let opened = open_sample(cx, &rig);
    select(&opened, cx, row_key(&step_key(), Some(0), Some(0)));
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::OpenContext);
    visual.run_until_parked();
    assert!(
        rig.contributions.dispatched().iter().any(
            |command| matches!(command, WorkspaceCommand::OpenPanel(name) if name == "context")
        )
    );
}

// ----- usage -----------------------------------------------------------------

/// A `SessionUsage` in the shape `usage` gives it (docs/design/usage.md
/// 「接口」, `snapshot::session`): the branch rows with the session's most
/// first, each naming the worktree that has the branch checked out and
/// carrying the full reference name; the "unassigned" row last, with no
/// worktree and the session's part as its cumulative; `shared` false (a
/// branch in several MRs comes with the MR steps); the turns of the
/// session's own that have usage, by the `TurnId` the log gave them.
pub fn session_usage() -> UsageSnapshot {
    let tokens = |n: u64| Tokens {
        input: n,
        ..Tokens::default()
    };
    let branch = |name: &str, wt: &str| BranchLabel {
        name: Arc::from(name),
        worktree: Some(WorktreeKey(Arc::from(std::path::Path::new(wt)))),
    };
    UsageSnapshot::Session(SessionUsage {
        header: SnapshotHeader {
            generation: 1,
            state: UsageState::Ready,
            may_include_inherited: false,
        },
        total: tokens(5_000),
        rows: Arc::from(vec![
            SessionRow {
                key: RowKey::Branch {
                    use_id: UseId([2; 16]),
                    branch: branch("refs/heads/feat", "/fake"),
                },
                worktree: Some(worktree()),
                cumulative: tokens(12_000),
                session_part: tokens(3_000),
                shared: false,
            },
            SessionRow {
                key: RowKey::Branch {
                    use_id: UseId([1; 16]),
                    branch: branch("refs/heads/other", "/elsewhere"),
                },
                worktree: Some(WorktreeKey(Arc::from(std::path::Path::new("/elsewhere")))),
                cumulative: tokens(10_000),
                session_part: tokens(1_000),
                shared: false,
            },
            SessionRow {
                key: RowKey::Unassigned {
                    unattributable: tokens(100),
                },
                worktree: None,
                cumulative: tokens(1_000),
                session_part: tokens(1_000),
                shared: false,
            },
        ]),
        turns: Arc::from(vec![(
            lectern_services::session::TurnId(Arc::from("turn-0")),
            tokens(1_234),
        )]),
    })
}

fn has_usage_rows(records: &[Record]) -> bool {
    records.iter().any(|record| {
        record
            .semantic
            .id
            .as_str()
            .starts_with("review.brief.usage")
    })
}

#[gpui::test]
fn usage_rows_and_turn_tokens(cx: &mut TestAppContext) {
    // The usage plugin alone: the reader and the code navigation are absent.
    let rig = Rig::new(cx);
    rig.enable_usage(cx);
    locate(&rig);
    let query = UsageQuery::Session {
        session: session_key(),
    };
    rig.usage.set(query.clone(), session_usage());
    let opened = open_sample(cx, &rig);
    assert_eq!(rig.usage.watched(), std::slice::from_ref(&query));
    let records = opened.record(cx);
    // The branch under review comes first, the rows without a branch last.
    let rows: Vec<&str> = records
        .iter()
        .filter(|record| {
            record
                .semantic
                .id
                .as_str()
                .starts_with("review.brief.usage.item[")
        })
        .map(|record| record.semantic.id.as_str())
        .collect();
    assert_eq!(
        rows,
        [
            "review.brief.usage.item[feat]",
            "review.brief.usage.item[other]",
            "review.brief.usage.item[Not on any branch]"
        ]
    );
    let feat = must(&records, "review.brief.usage.item[feat]");
    assert_eq!(attr(feat, "tokens").as_deref(), Some("12000"));
    assert_eq!(attr(feat, "percent").as_deref(), Some("25"));
    assert_eq!(attr(feat, "shared").as_deref(), Some("false"));
    let other = must(&records, "review.brief.usage.item[other]");
    assert_eq!(attr(other, "percent").as_deref(), Some("10"));
    assert_eq!(attr(other, "shared").as_deref(), Some("false"));
    let unassigned = must(&records, "review.brief.usage.item[Not on any branch]");
    assert_eq!(attr(unassigned, "tokens").as_deref(), Some("1000"));
    assert_eq!(attr(unassigned, "percent").as_deref(), Some("100"));
    let turn = must(&records, "review.turn.item[0:1]");
    assert_eq!(attr(turn, "tokens").as_deref(), Some("1234"));
    // The toolbar's second line names the agent, the branch of the worktree
    // under review (from its usage row) and the turns.
    let strings = lectern_i18n::Strings::of(lectern_i18n::Locale::En);
    let subtitle = opened
        .read(cx, |model| model.toolbar_subtitle(&strings))
        .map(|text| text.as_str().to_owned());
    assert_eq!(
        subtitle.as_deref(),
        Some("Claude Code \u{00B7} feat \u{00B7} 1 turn")
    );
    // The handoff is absent and the screen says so; usage did not need it.
    assert!(
        must(&records, "review.step.item[0].open")
            .semantic
            .state
            .disabled
    );
    assert!(rig.code_intel.focus().is_empty());

    // A change of the snapshot reaches the screen through the event.
    let UsageSnapshot::Session(mut changed) = session_usage() else {
        unreachable!()
    };
    changed.turns = Arc::from(vec![(
        lectern_services::session::TurnId(Arc::from("turn-0")),
        Tokens {
            input: 99,
            ..Tokens::default()
        },
    )]);
    rig.usage
        .set(query.clone(), UsageSnapshot::Session(changed));
    rig.usage_kernel
        .inner()
        .expect("the kernel runs")
        .emitter()
        .emit(UsageEvent::Changed {
            query: query.clone(),
            generation: 2,
        });
    rig.settle(cx);
    let records = opened.record(cx);
    assert_eq!(
        attr(must(&records, "review.turn.item[0:1]"), "tokens").as_deref(),
        Some("99")
    );

    // The watch ends with the screen.
    assert_eq!(rig.usage.released(), 0);
    close(cx, opened);
    assert_eq!(rig.usage.released(), 1);
}

#[gpui::test]
fn without_the_usage_plugin_there_is_no_usage(cx: &mut TestAppContext) {
    // The handoff is there; the usage plugin is not.
    let rig = Rig::new(cx);
    rig.enable_handoff(cx);
    locate(&rig);
    rig.usage.set(
        UsageQuery::Session {
            session: session_key(),
        },
        session_usage(),
    );
    let opened = open_sample(cx, &rig);
    assert!(rig.usage.watched().is_empty());
    let records = opened.record(cx);
    assert!(!has_usage_rows(&records));
    assert!(attr(must(&records, "review.turn.item[0:1]"), "tokens").is_none());
    // The buttons of the handoff work all the same.
    assert!(
        !must(&records, "review.step.item[0].open")
            .semantic
            .state
            .disabled
    );
}

#[gpui::test]
fn usage_plugin_deactivates_on_its_own(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.enable_handoff(cx);
    rig.enable_usage(cx);
    locate(&rig);
    let query = UsageQuery::Session {
        session: session_key(),
    };
    rig.usage.set(query.clone(), session_usage());
    let opened = open_sample(cx, &rig);
    assert!(has_usage_rows(&opened.record(cx)));
    assert_eq!(rig.usage.released(), 0);

    rig.usage_kernel.deactivate();
    rig.settle(cx);
    // The screen goes on; the rows and the tokens are gone; the watch is let
    // go; the handoff is untouched.
    let records = opened.record(cx);
    assert!(!has_usage_rows(&records));
    assert!(attr(must(&records, "review.turn.item[0:1]"), "tokens").is_none());
    assert!(find(&records, "review.brief").is_some());
    assert_eq!(rig.usage.released(), 1);
    assert!(
        !must(&records, "review.step.item[0].open")
            .semantic
            .state
            .disabled
    );
    assert_eq!(rig.reader.live(), 2);
    assert_eq!(rig.contributions.released(), 0);

    // It comes back: the screen watches again.
    rig.enable_usage(cx);
    let records = opened.record(cx);
    assert!(has_usage_rows(&records));
    assert_eq!(
        attr(must(&records, "review.turn.item[0:1]"), "tokens").as_deref(),
        Some("1234")
    );
    assert_eq!(rig.usage.watched().len(), 2);
    close(cx, opened);
    assert_eq!(rig.usage.released(), 2);
}

#[gpui::test]
fn usage_that_arrives_after_the_screen_is_read_when_it_does(cx: &mut TestAppContext) {
    // The screen is open before the usage plugin has started (the plugins
    // activate after the first frame; a screen may be first).
    let rig = Rig::new(cx);
    locate(&rig);
    rig.usage.set(
        UsageQuery::Session {
            session: session_key(),
        },
        session_usage(),
    );
    let opened = open_sample(cx, &rig);
    assert!(rig.usage.watched().is_empty());
    rig.enable_usage(cx);
    assert_eq!(rig.usage.watched().len(), 1);
    let records = opened.record(cx);
    assert!(has_usage_rows(&records));
    assert_eq!(
        attr(must(&records, "review.turn.item[0:1]"), "tokens").as_deref(),
        Some("1234")
    );
    close(cx, opened);
    assert_eq!(rig.usage.released(), 1);
}

#[gpui::test]
fn handoff_plugin_deactivates_on_its_own(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.enable_handoff(cx);
    rig.enable_usage(cx);
    locate(&rig);
    rig.usage.set(
        UsageQuery::Session {
            session: session_key(),
        },
        session_usage(),
    );
    let opened = open_sample(cx, &rig);
    assert_eq!(rig.reader.live(), 2);
    assert!(
        !must(&opened.record(cx), "review.step.item[0].open")
            .semantic
            .state
            .disabled
    );
    assert_eq!(rig.usage.released(), 0);

    rig.handoff.deactivate();
    rig.settle(cx);
    // The screen goes on; the buttons are grey; the usage is not touched.
    let records = opened.record(cx);
    assert!(
        must(&records, "review.step.item[0].open")
            .semantic
            .state
            .disabled
    );
    assert!(find(&records, "review.brief").is_some());
    assert!(has_usage_rows(&records));
    assert_eq!(
        attr(must(&records, "review.turn.item[0:1]"), "tokens").as_deref(),
        Some("1234")
    );
    assert_eq!(rig.usage.released(), 0);
    select(&opened, cx, row_key(&step_key(), Some(1), None));
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::OpenFile);
    visual.run_until_parked();
    assert!(rig.reader.opened().is_empty());
    // The review-view plugin is still active: registrations are held.
    assert_eq!(rig.contributions.released(), 0);

    // It comes back: everything is done again, the usage watch stays one.
    rig.enable_handoff(cx);
    assert!(
        !must(&opened.record(cx), "review.step.item[0].open")
            .semantic
            .state
            .disabled
    );
    assert_eq!(rig.usage.watched().len(), 1);
}

// ----- side by side ---------------------------------------------------------------

#[test]
fn items_display_rows_unified_is_one_per_row() {
    let rows = sample_rows(step_key());
    let display = crate::items::display_rows(&rows, DiffMode::Unified);
    assert_eq!(display.len(), 5);
    // A removed line has the old side only, an added line the new side only.
    assert!(display[1].left.is_some() && display[1].right.is_none());
    assert!(display[2].left.is_none() && display[2].right.is_some());
    // A context line has both.
    assert_eq!(display[0].left.map(|s| s.line), Some(0));
    assert_eq!(display[0].right.map(|s| s.line), Some(0));
    // The added line shares the removed line's word-level changes.
    assert_eq!(display[1].inline, Some(0));
    assert_eq!(display[2].inline, Some(0));
    assert_eq!(display[0].inline, None);
}

#[test]
fn items_display_rows_side_by_side_pairs_and_leaves_the_others_alone() {
    let rows = sample_rows(step_key());
    let display = crate::items::display_rows(&rows, DiffMode::SideBySide);
    assert_eq!(display.len(), 4, "the pair is one row");
    let pair = &display[1];
    assert_eq!(
        pair.left.map(|s| (s.line, s.kind)),
        Some((1, crate::items::RowKind::Removed))
    );
    assert_eq!(
        pair.right.map(|s| (s.line, s.kind)),
        Some((1, crate::items::RowKind::Added))
    );
    assert_eq!(pair.inline, Some(0));

    // Three removed lines, two added: the first two pair, the third is alone;
    // an added line without a partner is alone on the right.
    let mut rows = sample_rows(step_key());
    let mut list = vec![
        DiffRow::Removed {
            old: 0,
            pair: Some(0),
            old_byte: None,
            hunk: 0,
        },
        DiffRow::Removed {
            old: 1,
            pair: Some(1),
            old_byte: None,
            hunk: 0,
        },
        DiffRow::Removed {
            old: 2,
            pair: None,
            old_byte: None,
            hunk: 0,
        },
        DiffRow::Added {
            new: 0,
            pair: Some(0),
            new_byte: None,
            hunk: 0,
        },
        DiffRow::Added {
            new: 1,
            pair: Some(1),
            new_byte: None,
            hunk: 0,
        },
        DiffRow::Added {
            new: 2,
            pair: None,
            new_byte: None,
            hunk: 0,
        },
    ];
    rows.rows = Arc::from(std::mem::take(&mut list));
    rows.inline = Arc::from(Vec::new());
    let display = crate::items::display_rows(&rows, DiffMode::SideBySide);
    let shape: Vec<(Option<u32>, Option<u32>)> = display
        .iter()
        .map(|row| (row.left.map(|s| s.line), row.right.map(|s| s.line)))
        .collect();
    assert_eq!(
        shape,
        [
            (Some(0), Some(0)),
            (Some(1), Some(1)),
            (Some(2), None),
            (None, Some(2))
        ]
    );
}

#[gpui::test]
fn side_by_side_pairs_removed_and_added(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = open_sample(cx, &rig);
    let unified = opened.read(cx, |model| model.items.len());
    let records = opened.record(cx);
    assert_eq!(
        attr(must(&records, "review.narrative"), "mode").as_deref(),
        Some("unified")
    );
    assert!(find(&records, "review.diff.item[step:0].row.item[1:-]").is_some());
    assert!(find(&records, "review.diff.item[step:0].row.item[-:1]").is_some());

    // Select the removed line, then switch: the same pair of lines is one
    // item, the selection and the scroll position stay on it.
    select(&opened, cx, row_key(&step_key(), Some(1), None));
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::ToggleDiffMode);
    visual.run_until_parked();
    opened.draw(cx);
    assert_eq!(opened.read(cx, |model| model.mode), DiffMode::SideBySide);
    assert_eq!(
        opened.read(cx, |model| model.items.len()),
        unified - 1,
        "one item for the pair"
    );
    let records = opened.record(cx);
    assert_eq!(
        attr(must(&records, "review.narrative"), "mode").as_deref(),
        Some("side-by-side")
    );
    let pair = must(&records, "review.diff.item[step:0].row.item[1:1]");
    assert_eq!(attr(pair, "kind").as_deref(), Some("changed"));
    assert!(find(&records, "review.diff.item[step:0].row.item[1:-]").is_none());
    assert!(
        pair.semantic.state.selected,
        "the selection found the pair by its removed line"
    );
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(row_key(&step_key(), Some(1), Some(1)))
    );
    // The content area gets the whole width, and back.
    assert_eq!(
        rig.contributions.dispatched(),
        [WorkspaceCommand::SetReadingColumn(false)]
    );
    visual.dispatch_action(crate::actions::ToggleDiffMode);
    visual.run_until_parked();
    assert_eq!(
        rig.contributions.dispatched().last(),
        Some(&WorkspaceCommand::SetReadingColumn(true))
    );
    assert_eq!(opened.read(cx, |model| model.items.len()), unified);
    // The selection is on the removed row again.
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(row_key(&step_key(), Some(1), None))
    );
}

#[gpui::test]
fn side_by_side_is_remembered_by_the_next_screen(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = open_sample(cx, &rig);
    opened.update(cx, |model, cx| model.set_mode(DiffMode::SideBySide, cx));
    close(cx, opened);
    rig.contributions.clear_dispatched();
    // A screen opened later in the same run starts in the mode, and asks
    // for the width it needs.
    let again = rig.open(cx, session(), Some(sample_snapshot()));
    assert_eq!(again.read(cx, |model| model.mode), DiffMode::SideBySide);
    assert_eq!(
        rig.contributions.dispatched(),
        [WorkspaceCommand::SetReadingColumn(false)]
    );
}

#[gpui::test]
fn side_by_side_keeps_the_rows_and_asks_for_nothing_new(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = open_sample(cx, &rig);
    let prepares = rig.review.prepares(FIRST_ID);
    let rows_calls = rig.review.rows_calls();
    opened.update(cx, |model, cx| model.set_mode(DiffMode::SideBySide, cx));
    opened.draw(cx);
    rig.settle(cx);
    assert_eq!(
        rig.review.prepares(FIRST_ID),
        prepares,
        "the mode is not part of the RowsKey"
    );
    assert_eq!(rig.review.rows_calls(), rows_calls);
    // A text cell is drawn for each side.
    let records = opened.record(cx);
    assert!(find(&records, "review.diff.item[step:0].row.item[0:0]").is_some());
}

#[gpui::test]
fn side_by_side_sideways_scroll_is_shared_by_the_diff(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = open_sample(cx, &rig);
    opened.update(cx, |model, cx| model.set_mode(DiffMode::SideBySide, cx));
    opened.draw(cx);
    let cell = gpui::px(8.0);
    opened.update(cx, |model, cx| {
        // Nothing wider than the room: no sideways scroll.
        model.widest.insert(step_key(), 8);
        model.scroll_sideways(&step_key(), gpui::px(-40.0), cell, cx);
        assert_eq!(model.hscroll.get(&step_key()), None);
        model.widest.insert(step_key(), 80);
        model.scroll_sideways(&step_key(), gpui::px(-40.0), cell, cx);
        assert_eq!(model.hscroll.get(&step_key()), Some(&gpui::px(40.0)));
        model.scroll_sideways(&step_key(), gpui::px(-4000.0), cell, cx);
        assert_eq!(
            model.hscroll.get(&step_key()),
            Some(&(cell * 72.0)),
            "stops where the text does"
        );
        model.scroll_sideways(&step_key(), gpui::px(10000.0), cell, cx);
        assert_eq!(model.hscroll.get(&step_key()), Some(&gpui::px(0.0)));
    });
}

// ----- folds -----------------------------------------------------------------------

fn folded_rows(key: RowsKey) -> FileRows {
    let mut rows = sample_rows(key);
    let mut list = rows.rows.to_vec();
    list.push(DiffRow::Folded {
        old: 4..44,
        new: 4..44,
        old_byte: None,
        new_byte: None,
    });
    rows.rows = Arc::from(list);
    rows
}

#[gpui::test]
fn fold_expands_by_preparing_more_context(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.review.set_auto_prepare(true);
    rig.review
        .set_rows(FIRST_ID, step_key(), folded_rows(step_key()));
    let bigger = RowsKey::Step {
        step: StepId(0),
        context: 43,
    };
    rig.review
        .set_rows(FIRST_ID, bigger.clone(), sample_rows(bigger.clone()));
    let opened = rig.open(cx, session(), Some(sample_snapshot()));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    let records = opened.record(cx);
    let fold = must(&records, "review.diff.item[step:0].fold.item[4]");
    assert_eq!(
        fold.semantic.label.as_ref().and_then(|l| l.key),
        Some("review-fold")
    );
    let before = rig.review.last_needs(FIRST_ID);
    assert!(before.contains(&Need::Rows(step_key())));

    // Clicking the fold selects it; ↵ expands: the file's context grows by
    // the run, so the diff is asked for again under another key.
    let fold_key = ItemKey::Fold {
        rows: step_key(),
        old: 4,
    };
    select(&opened, cx, fold_key);
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::ExpandFold);
    visual.run_until_parked();
    opened.draw(cx);
    rig.settle(cx);
    let needs = rig.review.last_needs(FIRST_ID);
    assert!(needs.contains(&Need::Rows(bigger.clone())), "{needs:?}");
    assert!(!needs.contains(&Need::Rows(step_key())));
    assert_eq!(
        opened.read(cx, |model| model.context.get(&file_key(0)).copied()),
        Some(43)
    );
    let records = opened.record(cx);
    assert!(find(&records, "review.diff.item[step:0]").is_some());
    assert!(
        find(&records, "review.diff.item[step:0].fold.item[4]").is_none(),
        "the run is shown"
    );
}

#[test]
fn fold_expands_and_keeps_the_place() {
    // The items of a diff under context 3 and, after the fold was expanded,
    // the placeholder of the same diff under context 43: a position inside
    // the old rows is found in the new placeholder, not past it.
    let mut old = vec![Item::Brief];
    crate::items::rows_items(
        &mut old,
        &step_key(),
        &folded_rows(step_key()),
        DiffMode::Unified,
    );
    old.push(Item::Notice(crate::items::Notice::Folder));
    let bigger = RowsKey::Step {
        step: StepId(0),
        context: 43,
    };
    let new = vec![
        Item::Brief,
        Item::Placeholder {
            rows: bigger,
            lines: 9,
        },
        Item::Notice(crate::items::Notice::Folder),
    ];
    let row = old
        .iter()
        .position(|item| matches!(item, Item::Row { .. }))
        .expect("a row");
    let at = gpui::ListOffset {
        item_ix: row + 1,
        offset_in_item: gpui::px(0.0),
    };
    let restored = crate::anchor::restore(&old, at, &new);
    assert_eq!(restored.item_ix, 1, "the diff's placeholder");
    // A selected row of the old diff is found there too.
    let key = old[row].key();
    assert_eq!(crate::anchor::find(&new, &key), Some(1));
}

#[gpui::test]
fn highlight_answer_of_a_cancelled_task_does_not_end_its_replacement(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = open_sample(cx, &rig);
    // The rows leave before their tasks ran (cancelled), then come back:
    // new tasks for the same windows.
    opened.update(cx, |model, _| {
        model.rows.remove(&step_key());
        model.drop_rows(&step_key());
        let rows = Arc::new(sample_rows(step_key()));
        model.store_rows(&step_key(), rows);
    });
    assert!(rig.run_tasks(cx) > 0);
    let state = opened.read(cx, |model| {
        (
            model.highlighted[&step_key()].state(Half::New),
            model.plain.len(),
        )
    });
    assert_eq!(state, ("spans", 0), "the replacement's spans are used");
}

#[gpui::test]
fn fold_expands_from_a_hunk_above_it(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.review.set_auto_prepare(true);
    rig.review
        .set_rows(FIRST_ID, step_key(), folded_rows(step_key()));
    let opened = rig.open(cx, session(), Some(sample_snapshot()));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    let hunk = opened.read(cx, |model| {
        model
            .items
            .iter()
            .find(|item| item.is_hunk())
            .map(Item::key)
            .expect("a hunk")
    });
    select(&opened, cx, hunk);
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("enter");
    assert_eq!(
        opened.read(cx, |model| model.context.get(&file_key(0)).copied()),
        Some(43)
    );
    // With nothing to expand the key goes on (no change).
    select(&opened, cx, ItemKey::Brief);
    visual.simulate_keystrokes("enter");
    assert_eq!(
        opened.read(cx, |model| model.context.get(&file_key(0)).copied()),
        Some(43)
    );
}

// ----- Analyze ----------------------------------------------------------------------

fn snapshot_with(analysis: AnalysisState) -> ReviewSnapshot {
    let mut snapshot = sample_snapshot();
    let mut brief = (*snapshot.brief).clone();
    brief.analysis = analysis;
    snapshot.brief = Arc::new(brief);
    snapshot
}

fn commands(rig: &Rig) -> Vec<ReviewCommand> {
    rig.review
        .commands()
        .into_iter()
        .map(|(_, command)| command)
        .filter(|command| !matches!(command, ReviewCommand::Prepare(_)))
        .collect()
}

#[gpui::test]
fn analyze_commands_follow_the_state(cx: &mut TestAppContext) {
    // Idle: the button starts it.
    let rig = Rig::new(cx);
    let opened = rig.open(cx, session(), Some(snapshot_with(AnalysisState::Idle)));
    opened.draw(cx);
    let records = opened.record(cx);
    let button = must(&records, "review.brief.analyze");
    assert!(!button.semantic.state.disabled);
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::Analyze);
    visual.run_until_parked();
    assert_eq!(commands(&rig), [ReviewCommand::Analyze]);
    // Cancel is for a running analysis only.
    visual.dispatch_action(crate::actions::CancelAnalyze);
    visual.run_until_parked();
    assert_eq!(commands(&rig), [ReviewCommand::Analyze]);

    // Running: the state, a cancel button, no second start.
    let rig = Rig::new(cx);
    let running = AnalysisState::Running {
        started_at: lectern_services::session::Timestamp(0),
    };
    let opened = rig.open(cx, session(), Some(snapshot_with(running)));
    opened.draw(cx);
    let records = opened.record(cx);
    assert_eq!(
        attr(must(&records, "review.brief.analyze"), "state").as_deref(),
        Some("running")
    );
    let cancel = must(&records, "review.brief.analyze.cancel");
    assert_eq!(
        cancel.semantic.label.as_ref().and_then(|l| l.key),
        Some("review-cancel")
    );
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::Analyze);
    visual.run_until_parked();
    assert!(commands(&rig).is_empty(), "already running");
    visual.dispatch_action(crate::actions::CancelAnalyze);
    visual.run_until_parked();
    assert_eq!(commands(&rig), [ReviewCommand::CancelAnalyze]);

    // Done and Failed: again.
    for state in [
        AnalysisState::Done {
            stale: true,
            partial: false,
        },
        AnalysisState::Failed(Arc::from("boom")),
    ] {
        let rig = Rig::new(cx);
        let opened = rig.open(cx, session(), Some(snapshot_with(state)));
        opened.draw(cx);
        let records = opened.record(cx);
        let again = must(&records, "review.brief.analyze.again");
        assert_eq!(
            again.semantic.label.as_ref().and_then(|l| l.key),
            Some("review-analyze-again")
        );
        let mut visual = opened.visual(cx);
        visual.dispatch_action(crate::actions::Analyze);
        visual.run_until_parked();
        assert_eq!(commands(&rig), [ReviewCommand::Analyze]);
    }

    // Unavailable: no command.
    let rig = Rig::new(cx);
    let opened = rig.open(
        cx,
        session(),
        Some(snapshot_with(AnalysisState::Unavailable(Arc::from(
            "no tool",
        )))),
    );
    opened.draw(cx);
    let mut visual = opened.visual(cx);
    visual.dispatch_action(crate::actions::Analyze);
    visual.run_until_parked();
    assert!(commands(&rig).is_empty());
}
