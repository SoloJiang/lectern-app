//! `items_*`, `project_by_change_*`, `follow_*`, `project_by_cwd_*`,
//! `subagent_*`, `finalize_*`, `session_*` and `reset_keeps_observations`
//! (docs/design/usage.md「轮次的观察与改动项」「分摊」, step 4): which
//! project a turn's tokens go to, from the files it changed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::project::ProjectKey;
use lectern_services::session::{
    Agent, EditOp, FileEdit, Positioned, Record, Timestamp, ToolCallId, ToolFinished, ToolKind,
    ToolStarted, ToolStatus, TurnId,
};
use lectern_services::usage::{RowKey, SessionUsage, UsageQuery, UsageSnapshot, Window};

use super::support::*;
use crate::finalize::edit_lines;
use crate::testing::FakeSessions;

/// A session log being written: records at increasing positions.
pub(super) struct Log {
    fake: Arc<FakeSessions>,
    id: String,
    pub(super) records: Vec<Positioned<Record>>,
    pub(super) pos: u64,
}

impl Log {
    pub(super) fn new(fake: &Arc<FakeSessions>, id: &str, cwd: &str, at: i64) -> Log {
        fake.add(id, Path::new(cwd), at, None);
        Log::resume(fake, id, cwd, at)
    }

    /// A log for a session the source has (for a child, added with its
    /// parent).
    pub(super) fn resume(fake: &Arc<FakeSessions>, id: &str, cwd: &str, at: i64) -> Log {
        let mut log = Log {
            fake: fake.clone(),
            id: id.to_owned(),
            records: Vec::new(),
            pos: 0,
        };
        log.push(context(0, at, cwd));
        log
    }

    fn push(&mut self, mut r: Positioned<Record>) {
        r.pos = self.pos;
        self.pos += 100;
        self.records.push(r);
    }

    pub(super) fn context(&mut self, at: i64, cwd: &str) -> &mut Log {
        let pos = self.pos;
        self.push(context(pos, at, cwd));
        self
    }

    /// A `Context` record that names the branch the log recorded.
    pub(super) fn context_branch(&mut self, at: i64, cwd: &str, branch: &str) -> &mut Log {
        let pos = self.pos;
        self.push(rec(
            pos,
            Record::Context {
                at: Timestamp(at),
                cwd: Arc::from(Path::new(cwd)),
                logged_branch: Some(branch.into()),
                model: None,
            },
        ));
        self
    }

    pub(super) fn turn(&mut self, id: &str, at: i64) -> &mut Log {
        let pos = self.pos;
        self.push(turn(pos, id, at));
        self
    }

    pub(super) fn ended(&mut self, id: &str, at: i64) -> &mut Log {
        let pos = self.pos;
        self.push(rec(
            pos,
            Record::TurnEnded {
                turn: TurnId(id.into()),
                at: Timestamp(at),
                aborted: false,
            },
        ));
        self
    }

    pub(super) fn usage(&mut self, turn: &str, message: &str, at: i64, input: u64) -> &mut Log {
        let pos = self.pos;
        self.push(claude(pos, at, Some(turn), message, [input, 0, 0, 0]));
        self
    }

    /// An `Edit` call from `at` to `at + 1000` that writes `lines` lines to
    /// each of `paths`.
    pub(super) fn edit(
        &mut self,
        turn: &str,
        call: &str,
        paths: &[&str],
        at: i64,
        lines: usize,
    ) -> &mut Log {
        let named: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let content: String = (0..lines).map(|i| format!("line {i}\n")).collect();
        let edits = named
            .iter()
            .map(|p| FileEdit {
                path: p.clone(),
                op: EditOp::Write {
                    content: content.as_str().into(),
                    created: true,
                },
            })
            .collect();
        self.call(turn, call, &named, edits, at, ToolStatus::Completed)
    }

    /// An `Edit` call with edits as given, ending in `status`.
    pub(super) fn call(
        &mut self,
        turn: &str,
        call: &str,
        named: &[PathBuf],
        edits: Vec<FileEdit>,
        at: i64,
        status: ToolStatus,
    ) -> &mut Log {
        self.fake
            .edit_call_at(&self.id, turn, call, named, edits, status, (at, at + 1000));
        let pos = self.pos;
        self.push(rec(
            pos,
            Record::ToolStarted(ToolStarted {
                call: ToolCallId(call.into()),
                turn: Some(TurnId(turn.into())),
                at: Timestamp(at),
                name: "Edit".into(),
                kind: ToolKind::Edit {
                    paths: named.to_vec().into(),
                },
            }),
        ));
        let pos = self.pos;
        self.push(rec(
            pos,
            Record::ToolFinished(ToolFinished {
                call: ToolCallId(call.into()),
                at: Timestamp(at + 1000),
                status,
                exec_session: None,
            }),
        ));
        self
    }

    /// A command run in `workdir` from `at` to `at + 100`.
    pub(super) fn exec(
        &mut self,
        turn: &str,
        call: &str,
        command: &str,
        workdir: &str,
        at: i64,
    ) -> &mut Log {
        let kind = ToolKind::Execute {
            command: command.into(),
            workdir: Arc::from(Path::new(workdir)),
            exec_session: None,
            workdir_exact: true,
        };
        self.tool(turn, call, kind, at, at + 100)
    }

    /// Files read from `at` to `at + 100`.
    pub(super) fn read(&mut self, turn: &str, call: &str, paths: &[&str], at: i64) -> &mut Log {
        let kind = ToolKind::Read {
            paths: paths.iter().map(PathBuf::from).collect::<Vec<_>>().into(),
        };
        self.tool(turn, call, kind, at, at + 100)
    }

    /// A search of `paths` from `at` to `at + 100`.
    pub(super) fn search(&mut self, turn: &str, call: &str, paths: &[&str], at: i64) -> &mut Log {
        let kind = ToolKind::Search {
            paths: paths.iter().map(PathBuf::from).collect::<Vec<_>>().into(),
        };
        self.tool(turn, call, kind, at, at + 100)
    }

    /// A call of any kind, in the log and in the turn index.
    pub(super) fn tool(
        &mut self,
        turn: &str,
        call: &str,
        kind: ToolKind,
        start: i64,
        end: i64,
    ) -> &mut Log {
        self.fake
            .tool_call(&self.id, turn, call, kind.clone(), (start, end));
        let pos = self.pos;
        self.push(rec(
            pos,
            Record::ToolStarted(ToolStarted {
                call: ToolCallId(call.into()),
                turn: Some(TurnId(turn.into())),
                at: Timestamp(start),
                name: "Tool".into(),
                kind,
            }),
        ));
        let pos = self.pos;
        self.push(rec(
            pos,
            Record::ToolFinished(ToolFinished {
                call: ToolCallId(call.into()),
                at: Timestamp(end),
                status: ToolStatus::Completed,
                exec_session: None,
            }),
        ));
        self
    }

    /// A call that spawns a sub-agent.
    pub(super) fn spawn(&mut self, turn: &str, call: &str, at: i64) -> &mut Log {
        let pos = self.pos;
        self.push(rec(
            pos,
            Record::ToolStarted(ToolStarted {
                call: ToolCallId(call.into()),
                turn: Some(TurnId(turn.into())),
                at: Timestamp(at),
                name: "Task".into(),
                kind: ToolKind::SpawnAgent,
            }),
        ));
        self
    }

    /// Hands the records to the source.
    pub(super) fn write(&mut self) {
        let records = std::mem::take(&mut self.records);
        self.fake.push(&self.id, records);
    }
}

pub(super) fn source() -> Arc<FakeSessions> {
    Arc::new(FakeSessions::new("session-claude-code", Agent::ClaudeCode))
}

/// Tokens of a project over the last 7 days.
fn input_of(h: &Harness, key: &ProjectKey) -> u64 {
    h.project(key, Window::D7).total.input
}

fn other_of(h: &Harness) -> u64 {
    h.all(Window::D7).other.input
}

pub(super) const AT: i64 = OCT_20 + 9 * HOUR;
/// Long after every record: every turn is ready.
pub(super) const LATER: i64 = OCT_20 + DAY * 2;

pub(super) fn settled(h: &Harness, fake: &FakeSessions, id: &str) {
    h.settle_all(fake, id, LATER);
}

pub(super) fn conn(h: &Harness) -> rusqlite::Connection {
    rusqlite::Connection::open(h.db_path()).unwrap()
}

/// The weights of a turn's share, by the paths of the places: (place path,
/// weight), in order.
pub(super) fn share_of(h: &Harness, turn: &str) -> Vec<(String, i64)> {
    let conn = conn(h);
    let mut stmt = conn
        .prepare(
            "SELECT l.path, s.weight FROM share s
             JOIN turn t ON t.id = s.turn JOIN intern i ON i.id = t.tid
             JOIN location l ON l.id = s.location
             WHERE i.text = ?1 ORDER BY l.path",
        )
        .unwrap();
    stmt.query_map([turn.as_bytes()], |r| {
        let path: Vec<u8> = r.get(0)?;
        Ok((String::from_utf8_lossy(&path).into_owned(), r.get(1)?))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

// ---- items ----

fn replace(old: &str, new: &str) -> EditOp {
    EditOp::Replace {
        old: old.into(),
        new: new.into(),
        all: false,
    }
}

#[test]
fn items_replace_counts_the_lines_added_and_removed() {
    assert_eq!(edit_lines(&replace("a\nb\nc\n", "a\nB\nc\nd\n")), 3);
    assert_eq!(edit_lines(&replace("a\n", "a\n")), 0);
    assert_eq!(edit_lines(&replace("", "x\ny\n")), 2);
    assert_eq!(edit_lines(&replace("x\ny\n", "")), 2);
}

#[test]
fn items_replace_too_big_to_diff_counts_both_sides() {
    // Each side over the 4 MiB the diff reads: the lines of both.
    let side = "ab\n".repeat((4 << 20) / 3 + 1);
    let lines = side.lines().count() as u64;
    assert!(side.len() > 4 << 20);
    assert_eq!(edit_lines(&replace(&side, &side)), 2 * lines);
}

#[test]
fn items_patch_counts_the_plus_and_minus_lines_of_its_hunks() {
    let patch = |hunks: &str| EditOp::Patch {
        hunks: hunks.into(),
        move_to: None,
    };
    let body = "@@ -1,3 +1,3 @@\n context\n-old\n+new\n context\n";
    assert_eq!(edit_lines(&patch(body)), 2);
    // File headers before the first hunk are not changed lines; a removed
    // line that starts with dashes inside a hunk is.
    let with_headers = "--- a/f\n+++ b/f\n@@ -1,2 +1,1 @@\n--- not a header\n+kept\n";
    assert_eq!(edit_lines(&patch(with_headers)), 2);
    // Without a hunk header only the plus and minus lines count, headers
    // not.
    assert_eq!(edit_lines(&patch("--- a\n+++ b\n+x\n-y\n")), 2);
    assert_eq!(edit_lines(&patch("")), 0);
}

#[test]
fn items_write_and_add_count_the_lines_of_the_content() {
    let write = |content: &str, created: bool| EditOp::Write {
        content: content.into(),
        created,
    };
    assert_eq!(edit_lines(&write("a\nb\nc", true)), 3);
    assert_eq!(edit_lines(&write("a\nb\n", false)), 2, "written over");
    assert_eq!(edit_lines(&write("", true)), 0);
    assert_eq!(
        edit_lines(&EditOp::Add {
            content: "x\n".into()
        }),
        1
    );
    assert_eq!(edit_lines(&EditOp::Delete), 0);
}

#[test]
fn items_a_delete_counts_as_a_file_and_zero_lines_do_not() {
    let mut h = Harness::new();
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 60);
    let gone = vec![PathBuf::from("/work/lib/old.rs")];
    let delete = vec![FileEdit {
        path: gone[0].clone(),
        op: EditOp::Delete,
    }];
    log.call("t1", "c1", &gone, delete, AT + 20, ToolStatus::Completed);
    let empty = vec![PathBuf::from("/work/tools/empty.rs")];
    let nothing = vec![FileEdit {
        path: empty[0].clone(),
        op: EditOp::Write {
            content: "".into(),
            created: true,
        },
    }];
    log.call("t1", "c2", &empty, nothing, AT + 30, ToolStatus::Completed);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    // The empty write is not a change: only the delete counts.
    assert_eq!(input_of(&h, &lib), 60);
    let rows: i64 = conn(&h)
        .query_row("SELECT count(*) FROM item", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    assert_eq!(share_of(&h, "t1"), [("/work/lib/.git".to_owned(), 1)]);
}

#[test]
fn items_weigh_lines_plus_the_number_of_different_files() {
    let mut h = Harness::new();
    h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/lib", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    // Two files of 2 and 3 lines, then the first one again with 4 lines:
    // 2 + 3 + 4 lines and 2 files.
    log.edit(
        "t1",
        "c1",
        &["/work/lib/a.rs", "/work/lib/b.rs"],
        AT + 20,
        2,
    );
    log.edit("t1", "c2", &["/work/lib/b.rs"], AT + 30, 1);
    log.edit("t1", "c3", &["/work/lib/a.rs"], AT + 40, 4);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    // a: 2 + 4, b: 2 + 1.
    assert_eq!(
        share_of(&h, "t1"),
        [("/work/lib/.git".to_owned(), 6 + 3 + 2)]
    );
}

#[test]
fn items_path_prefix_is_kept_in_the_call_fact() {
    let mut h = Harness::new();
    h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 10);
    log.edit("t1", "c1", &["/work/lib/src/a.rs"], AT + 20, 2);
    log.write();
    h.read_every(&fake);
    let conn = conn(&h);
    // The place, worktree and the path below it, written when the call
    // was read (before any turn settles).
    // The path below the worktree is the path less the worktree's.
    let (place, worktree, path): (Vec<u8>, Vec<u8>, Vec<u8>) = conn
        .query_row(
            "SELECT l.path, w.text, p.text FROM call_fact f
             JOIN location l ON l.id = f.location
             JOIN intern w ON w.id = f.worktree JOIN intern p ON p.id = f.path",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(place, b"/work/lib/.git");
    // The main worktree is named by the common directory.
    assert_eq!(worktree, b"/work/lib/.git");
    assert_eq!(path, b"/work/lib/src/a.rs");
}

// ---- project_by_change ----

#[test]
fn project_by_change_moves_the_turn_to_the_changed_files_project() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/lib/src/a.rs"], AT + 20, 2);
    log.write();
    h.read_every(&fake);
    // Before the turn is settled it counts where the session runs.
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (100, 0));
    settled(&h, &fake, "s");
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 100));
    assert_buckets(&h);
}

#[test]
fn project_by_change_splits_across_projects_by_weight() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let tools = h.open_repo("/work/tools");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 90);
    // lib: 2 lines + 1 file = 3, tools: 5 lines + 1 file = 6; the cwd's
    // project has no say.
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 2);
    log.edit("t1", "c2", &["/work/tools/b.rs"], AT + 30, 5);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!(
        (input_of(&h, &app), input_of(&h, &lib), input_of(&h, &tools)),
        (0, 30, 60)
    );
    assert_eq!(h.total().input, 90, "the parts add up to the whole");
    assert_buckets(&h);
}

#[test]
fn project_by_change_splits_odd_amounts_exactly() {
    let mut h = Harness::new();
    let lib = h.open_repo("/work/lib");
    let tools = h.open_repo("/work/tools");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/lib", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    // Equal weights: a third of 100 each, nothing lost.
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 1);
    log.edit("t1", "c2", &["/work/tools/b.rs"], AT + 30, 1);
    log.edit("t1", "c3", &["/work/elsewhere/c.rs"], AT + 40, 1);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    // The rows of the all-projects list are rounded together: they add up.
    let all = h.all(Window::D7);
    assert_eq!(all.total.input, 100);
    let row = |key: &ProjectKey| {
        all.projects
            .iter()
            .find(|p| p.0.key.as_ref() == Some(key))
            .map_or(0, |p| p.1.input)
    };
    let (a, b, c) = (row(&lib), row(&tools), all.other.input);
    assert_eq!(a + b + c, 100);
    assert!(
        [a, b, c].iter().all(|n| (33..=34).contains(n)),
        "{a} {b} {c}"
    );
    assert_buckets(&h);
}

#[test]
fn project_by_change_a_linked_worktree_is_the_same_project() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    h.projects
        .repo(Path::new("/work/app-feature"), Path::new("/work/app/.git"));
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/notes", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 40);
    log.edit("t1", "c1", &["/work/app-feature/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!(input_of(&h, &app), 40);
    let conn = conn(&h);
    let worktree: Vec<u8> = conn
        .query_row(
            "SELECT w.text FROM call_fact f JOIN intern w ON w.id = f.worktree",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        worktree, b"/work/app/.git/worktrees/app-feature",
        "the worktree it was in"
    );
}

#[test]
fn project_by_change_files_outside_any_project_are_other() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 70);
    log.edit("t1", "c1", &["/scratch/notes/x.txt"], AT + 20, 2);
    log.write();
    h.read_every(&fake);
    assert_eq!((input_of(&h, &app), other_of(&h)), (70, 0));
    settled(&h, &fake, "s");
    assert_eq!((input_of(&h, &app), other_of(&h)), (0, 70));
}

#[test]
fn project_by_change_a_failed_edit_changes_nothing() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 50);
    let named = vec![PathBuf::from("/work/lib/a.rs")];
    let edits = vec![FileEdit {
        path: named[0].clone(),
        op: EditOp::Write {
            content: "x\n".into(),
            created: true,
        },
    }];
    log.call("t1", "c1", &named, edits, AT + 20, ToolStatus::Failed);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (50, 0));
}

// ---- follow ----

#[test]
fn follow_takes_the_nearest_turn_with_changes_before() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let tools = h.open_repo("/work/tools");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 10);
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 1);
    log.turn("t2", AT + 100).usage("t2", "m2", AT + 110, 20);
    log.turn("t3", AT + 200).usage("t3", "m3", AT + 210, 30);
    log.edit("t3", "c3", &["/work/tools/b.rs"], AT + 220, 1);
    log.turn("t4", AT + 300).usage("t4", "m4", AT + 310, 40);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!(
        (input_of(&h, &app), input_of(&h, &lib), input_of(&h, &tools)),
        (0, 30, 70)
    );
    assert_buckets(&h);
}

#[test]
fn follow_looks_after_when_nothing_is_before() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 10);
    log.turn("t2", AT + 100).usage("t2", "m2", AT + 110, 20);
    log.edit("t2", "c2", &["/work/lib/a.rs"], AT + 120, 1);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 30));
}

#[test]
fn follow_copies_the_whole_share_of_the_turn_it_follows() {
    let mut h = Harness::new();
    let lib = h.open_repo("/work/lib");
    let tools = h.open_repo("/work/tools");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/lib", AT);
    log.turn("t1", AT);
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 2);
    log.edit("t1", "c2", &["/work/tools/b.rs"], AT + 30, 5);
    log.turn("t2", AT + 100).usage("t2", "m2", AT + 110, 90);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    // 3 : 6, as in the turn that changed the files.
    assert_eq!((input_of(&h, &lib), input_of(&h, &tools)), (30, 60));
    assert_eq!(share_of(&h, "t2"), share_of(&h, "t1"));
}

#[test]
fn follow_turns_before_a_late_change_are_attributed_again() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 10);
    log.turn("t2", AT + 10_000)
        .usage("t2", "m2", AT + 10_010, 20);
    log.edit("t2", "c2", &["/work/lib/a.rs"], AT + 10_020, 1);
    log.write();
    h.read_every(&fake);
    // t1 is ready; t2 (the last turn of the log) is ready a little later.
    let early = AT + 10_000;
    h.settle_all(&fake, "s", early);
    assert_eq!(input_of(&h, &app), 30, "t1 settled by its cwd; t2 not yet");
    assert_eq!(input_of(&h, &lib), 0);
    settled(&h, &fake, "s");
    // t2 changes lib; t1 follows it, after.
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 30));
    assert_buckets(&h);
}

#[test]
fn follow_a_turn_between_two_changes_stays_with_the_one_before() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let tools = h.open_repo("/work/tools");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT);
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 1);
    log.turn("t2", AT + 10_000)
        .usage("t2", "m2", AT + 10_010, 10);
    log.turn("t3", AT + 20_000)
        .usage("t3", "m3", AT + 20_010, 10);
    log.edit("t3", "c3", &["/work/tools/b.rs"], AT + 20_020, 1);
    log.write();
    h.read_every(&fake);
    // t1 and t2 settle first; t2 follows t1. t3 is not ready: its usage
    // counts where the session runs.
    h.settle_all(&fake, "s", AT + 15_000);
    let now = (input_of(&h, &app), input_of(&h, &lib), input_of(&h, &tools));
    assert_eq!(now, (10, 10, 0));
    // t3 settles: its own usage goes to tools, t2 stays with t1.
    settled(&h, &fake, "s");
    let end = (input_of(&h, &app), input_of(&h, &lib), input_of(&h, &tools));
    assert_eq!(end, (0, 10, 10));
}

#[test]
fn follow_an_unsettled_turn_follows_the_last_settled_change() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 5, 3);
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 1);
    log.turn("t2", AT + 10_000);
    log.write();
    // The session is live: t2 is still open.
    fake.edit("s", |s| s.open_turn = true);
    h.read_every(&fake);
    assert_eq!(h.settle(&fake, "s", LATER).turns, 1, "t1 is settled");
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 3));
    // What t2 uses from now on counts with the change of t1.
    log.usage("t2", "m2", AT + 10_010, 25);
    log.write();
    h.read_every(&fake);
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 28));
    // And it stays there when the turn ends and settles.
    fake.edit("s", |s| s.open_turn = false);
    assert_eq!(h.settle(&fake, "s", LATER).turns, 1);
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 28));
    assert_buckets(&h);
}

// ---- project_by_cwd ----

#[test]
fn project_by_cwd_when_the_session_has_no_changes() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 10);
    log.context(AT + 100, "/work/lib/src");
    log.turn("t2", AT + 110).usage("t2", "m2", AT + 120, 20);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    // Each turn is where its own cwd was; none moves.
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (10, 20));
    assert_eq!(share_of(&h, "t1"), [("/work/app/.git".to_owned(), 1)]);
    assert_eq!(share_of(&h, "t2"), [("/work/lib/.git".to_owned(), 1)]);
    assert_buckets(&h);
}

#[test]
fn project_by_cwd_outside_every_project_is_other() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/tmp/scratch", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 12);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!((input_of(&h, &app), other_of(&h)), (0, 12));
}

#[test]
fn project_by_cwd_a_folder_project_claims_what_is_under_it() {
    let mut h = Harness::new();
    let notes = h.projects.folder(Path::new("/work/notes"));
    h.open_project(&notes);
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/notes/deep", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 8);
    log.edit("t1", "c1", &["/work/notes/other/x.md"], AT + 20, 2);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!(input_of(&h, &notes), 8);
    assert_eq!(other_of(&h), 0);
}

// ---- subagent ----

fn child(fake: &Arc<FakeSessions>, id: &str, parent: &str, call: &str, cwd: &str, at: i64) -> Log {
    fake.add(id, Path::new(cwd), at, Some(parent));
    fake.edit(id, |s| {
        s.parent = Some(lectern_services::session::Parent {
            session: fake.key(parent),
            tool_call: Some(ToolCallId(call.into())),
        });
    });
    Log::resume(fake, id, cwd, at)
}

#[test]
fn subagent_changes_count_for_the_turn_that_spawned_it() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut parent = Log::new(&fake, "p", "/work/app", AT);
    parent.turn("pt", AT).usage("pt", "pm", AT + 10, 80);
    parent.spawn("pt", "spawn1", AT + 20);
    parent.write();
    let mut kid = child(&fake, "k", "p", "spawn1", "/work/app", AT + 30);
    kid.turn("kt", AT + 30).usage("kt", "km", AT + 40, 20);
    kid.edit("kt", "kc", &["/work/lib/a.rs"], AT + 50, 4);
    kid.write();
    h.read_every(&fake);
    settled(&h, &fake, "k");
    settled(&h, &fake, "p");
    // The child's turn by its change; the parent's turn, with no change of
    // its own, by the change below it: both in lib.
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 100));
    assert_buckets(&h);
}

#[test]
fn subagent_parent_is_attributed_again_when_the_child_settles_later() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut parent = Log::new(&fake, "p", "/work/app", AT);
    parent.turn("pt", AT).usage("pt", "pm", AT + 10, 80);
    parent.spawn("pt", "spawn1", AT + 20);
    parent.write();
    let mut kid = child(&fake, "k", "p", "spawn1", "/work/app", AT + 30);
    kid.turn("kt", AT + 30);
    kid.edit("kt", "kc", &["/work/lib/a.rs"], AT + 50, 4);
    kid.write();
    h.read_every(&fake);
    // The parent goes first: nothing is known of the child's change.
    settled(&h, &fake, "p");
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (80, 0));
    settled(&h, &fake, "k");
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 80));
}

#[test]
fn subagent_without_changes_follows_the_parent_turn() {
    let mut h = Harness::new();
    let lib = h.open_repo("/work/lib");
    let tools = h.open_repo("/work/tools");
    let fake = source();
    let mut parent = Log::new(&fake, "p", "/work/app", AT);
    parent.turn("pt", AT).usage("pt", "pm", AT + 10, 10);
    parent.edit("pt", "pc", &["/work/lib/a.rs"], AT + 15, 1);
    parent.spawn("pt", "spawn1", AT + 20);
    parent.write();
    let mut kid = child(&fake, "k", "p", "spawn1", "/work/tools", AT + 30);
    kid.turn("kt", AT + 30).usage("kt", "km", AT + 40, 30);
    kid.write();
    h.read_every(&fake);
    settled(&h, &fake, "p");
    settled(&h, &fake, "k");
    // The sub-agent worked in tools but changed nothing: its parent
    // turn's change decides.
    assert_eq!((input_of(&h, &lib), input_of(&h, &tools)), (40, 0));
}

#[test]
fn subagent_with_no_change_anywhere_goes_by_its_own_cwd() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let tools = h.open_repo("/work/tools");
    let fake = source();
    let mut parent = Log::new(&fake, "p", "/work/app", AT);
    parent.turn("pt", AT).usage("pt", "pm", AT + 10, 10);
    parent.spawn("pt", "spawn1", AT + 20);
    parent.write();
    let mut kid = child(&fake, "k", "p", "spawn1", "/work/tools", AT + 30);
    kid.turn("kt", AT + 30).usage("kt", "km", AT + 40, 30);
    kid.write();
    h.read_every(&fake);
    settled(&h, &fake, "p");
    settled(&h, &fake, "k");
    assert_eq!((input_of(&h, &app), input_of(&h, &tools)), (10, 30));
}

#[test]
fn subagent_found_by_time_when_the_log_names_no_call() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut parent = Log::new(&fake, "p", "/work/app", AT);
    parent.turn("p1", AT).usage("p1", "pm1", AT + 10, 10);
    parent
        .turn("p2", AT + 1000)
        .usage("p2", "pm2", AT + 1010, 20);
    parent.write();
    // No tool call is named; the child begins during p2.
    fake.add("k", Path::new("/work/app"), AT + 1500, Some("p"));
    let mut kid = Log::resume(&fake, "k", "/work/app", AT + 1500);
    kid.turn("kt", AT + 1500);
    kid.edit("kt", "kc", &["/work/lib/a.rs"], AT + 1520, 2);
    kid.write();
    h.read_every(&fake);
    settled(&h, &fake, "k");
    settled(&h, &fake, "p");
    // p2 counts the child's change; p1 has none and follows p2, after.
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 30));
}

// ---- finalize ----

#[test]
fn finalize_waits_two_seconds_after_the_last_result() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    // The result comes at AT + 5000; the end marker is in the same batch.
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 4000, 2);
    log.ended("t1", AT + 4500);
    log.write();
    h.read_every(&fake);
    let early = h.settle(&fake, "s", AT + 5000 + 1999);
    assert_eq!(early.turns, 0);
    assert_eq!(early.wait_until, Some(AT + 5000 + 2000));
    assert_eq!(input_of(&h, &app), 100, "not settled yet");
    let done = h.settle(&fake, "s", AT + 5000 + 2000);
    assert_eq!(done.turns, 1);
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 100));
}

#[test]
fn finalize_a_claude_turn_ends_at_its_last_record_not_the_next_prompt() {
    let h = Harness::new();
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 1);
    // The next prompt comes a minute later.
    log.turn("t2", AT + 60_000)
        .usage("t2", "m2", AT + 60_010, 1);
    log.write();
    h.read_every(&fake);
    // t1's last record is at AT + 10: it is ready 2 s later; t2 is the last
    // turn of a session with no open turn: ready 2 s after its last record.
    let before = h.settle(&fake, "s", AT + 2009);
    assert_eq!(before.turns, 0);
    assert_eq!(before.wait_until, Some(AT + 2010));
    let first = h.settle(&fake, "s", AT + 2010);
    assert_eq!(first.turns, 1);
    let rest = h.settle(&fake, "s", AT + 60_010 + 2000);
    assert_eq!(rest.turns, 1);
}

#[test]
fn finalize_the_open_turn_of_a_live_session_waits_for_its_end() {
    let h = Harness::new();
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 1);
    log.turn("t2", AT + 100).usage("t2", "m2", AT + 110, 1);
    log.write();
    fake.edit("s", |s| s.open_turn = true);
    h.read_every(&fake);
    let live = h.settle(&fake, "s", LATER);
    assert_eq!(live.turns, 1, "only the turn that a later prompt ended");
    fake.edit("s", |s| s.open_turn = false);
    let over = h.settle(&fake, "s", LATER);
    assert_eq!(over.turns, 1);
}

#[test]
fn finalize_a_codex_turn_ends_with_its_end_marker() {
    let h = Harness::new();
    let fake = Arc::new(FakeSessions::new("session-codex", Agent::Codex));
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 1);
    log.write();
    fake.edit("s", |s| s.open_turn = true);
    h.read_every(&fake);
    assert_eq!(h.settle(&fake, "s", LATER).turns, 0, "no end marker yet");
    log.ended("t1", AT + 500);
    log.write();
    h.read_every(&fake);
    let done = h.settle(&fake, "s", AT + 2500);
    assert_eq!(done.turns, 1);
}

#[test]
fn finalize_waits_for_the_first_read_to_the_end() {
    let mut h = Harness::new();
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    for i in 0..4 {
        let t = format!("t{i}");
        let at = AT + 1000 * i64::from(i);
        log.turn(&t, at).usage(&t, &format!("m{i}"), at + 10, 5);
    }
    log.edit("t3", "c3", &["/work/lib/a.rs"], AT + 3500, 1);
    log.write();
    // One line of the log per batch: the session is not at its end yet.
    let first = h.read_once(&fake, "s", 100);
    assert!(!first.at_end);
    assert_eq!(h.settle(&fake, "s", LATER).turns, 0);
    assert_eq!(fake.index_reads("s"), 0, "the log is not parsed");
    h.read_all(&fake, "s");
    let done = h.settle(&fake, "s", LATER);
    assert_eq!(done.turns, 4);
    assert_eq!(fake.index_reads("s"), 1);
    assert_eq!(input_of(&h, &lib), 20);
}

#[test]
fn finalize_a_big_session_settles_in_batches_with_one_index_read_each() {
    let mut h = Harness::new();
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    let turns = 120;
    for i in 0..turns {
        let t = format!("t{i}");
        let at = AT + 10_000 * i64::from(i);
        log.turn(&t, at).usage(&t, &format!("m{i}"), at + 10, 1);
        log.edit(&t, &format!("c{i}"), &["/work/lib/a.rs"], at + 20, 1);
    }
    log.write();
    h.read_every(&fake);
    let mut batches = 0;
    loop {
        let done = h.settle(&fake, "s", LATER);
        batches += 1;
        assert!(done.turns <= 50);
        if !done.more {
            break;
        }
    }
    // 50 + 50 + 20 turns (a slow machine may cut a batch short).
    assert!(batches >= 3, "{batches}");
    assert_eq!(fake.index_reads("s"), batches, "one index read per batch");
    assert_eq!(fake.loads("s"), 120, "each edit read once");
    assert_eq!(input_of(&h, &lib), 120);
    assert_buckets(&h);
}

#[test]
fn finalize_settles_a_turn_once() {
    let mut h = Harness::new();
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 9);
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 1);
    log.write();
    h.read_every(&fake);
    assert_eq!(h.settle(&fake, "s", LATER).turns, 1);
    let again = h.settle(&fake, "s", LATER);
    assert_eq!(again.turns, 0);
    assert!(again.written.is_none());
    assert_eq!(fake.loads("s"), 1);
    assert_eq!(input_of(&h, &lib), 9);
}

#[test]
fn finalize_facts_add_up_to_each_usage_record() {
    let mut h = Harness::new();
    h.open_repo("/work/lib");
    h.open_repo("/work/tools");
    h.open_repo("/work/docs");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.usage("t1", "m2", AT + 11, 7);
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 1);
    log.edit("t1", "c2", &["/work/tools/a.rs"], AT + 21, 1);
    log.edit("t1", "c3", &["/work/docs/a.rs"], AT + 22, 1);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let conn = conn(&h);
    let sums: Vec<(i64, i64, i64)> = conn
        .prepare("SELECT source, sum(input), count(*) FROM fact GROUP BY source ORDER BY source")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(sums.len(), 2);
    assert_eq!(sums[0].1, 100 * 1_000_000);
    assert_eq!(sums[1].1, 7 * 1_000_000);
    assert!(sums.iter().all(|s| s.2 == 3), "a row per place");
    assert_eq!(h.total().input, 107);
}

#[test]
fn finalize_survives_a_rebuild_of_the_derived_tables() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 1);
    log.turn("t2", AT + 100).usage("t2", "m2", AT + 110, 50);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let before = (input_of(&h, &app), input_of(&h, &lib));
    assert_eq!(before, (0, 150));
    let shares_before = (share_of(&h, "t1"), share_of(&h, "t2"));
    h.store().rebuild_hot().unwrap();
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), before);
    assert_eq!((share_of(&h, "t1"), share_of(&h, "t2")), shares_before);
    assert_buckets(&h);
}

// ---- reset_keeps_observations ----

#[test]
fn reset_keeps_observations() {
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 2);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 100));
    // The worktree is gone by the time the adapter upgrade replays the log.
    h.projects.missing(Path::new("/work/lib"));
    let mut again = Log::resume(&fake, "s", "/work/app", AT);
    again.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    again.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 2);
    let records = std::mem::take(&mut again.records);
    fake.replace("s", records);
    h.read_every(&fake);
    // The new generation's turn is not settled: counted provisionally.
    settled(&h, &fake, "s");
    // The file's place is what the call recorded, not what is true now.
    assert_eq!((input_of(&h, &app), input_of(&h, &lib)), (0, 100));
    let conn = conn(&h);
    let places: i64 = conn
        .query_row("SELECT count(DISTINCT location) FROM call_fact", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(places, 1);
    let gens: i64 = conn
        .query_row("SELECT count(DISTINCT gen) FROM item", [], |r| r.get(0))
        .unwrap();
    assert_eq!(gens, 1, "the old generation's items went with it");
    assert_buckets(&h);
}

// ---- session ----

pub(super) fn session_usage(h: &Harness, fake: &FakeSessions, id: &str) -> SessionUsage {
    match h.snapshot(&UsageQuery::Session {
        session: fake.key(id),
    }) {
        Some(UsageSnapshot::Session(s)) => s,
        other => panic!("not a session snapshot: {other:?}"),
    }
}

#[test]
fn session_turns_are_the_tokens_counted_in_each_turn() {
    let mut h = Harness::new();
    h.open_repo("/work/lib");
    h.open_repo("/work/tools");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 60);
    log.usage("t1", "m2", AT + 11, 40);
    log.edit("t1", "c1", &["/work/lib/a.rs"], AT + 20, 1);
    log.edit("t1", "c2", &["/work/tools/a.rs"], AT + 21, 1);
    log.turn("t2", AT + 100).usage("t2", "m3", AT + 110, 7);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let s = session_usage(&h, &fake, "s");
    // Not divided between the places: the turn's own tokens.
    let turns: Vec<(&str, u64)> = s
        .turns
        .iter()
        .map(|(t, tokens)| (&*t.0, tokens.input))
        .collect();
    assert_eq!(turns, [("t1", 100), ("t2", 7)]);
    assert_eq!(s.total.input, 107);
    // Nobody watched the repositories: the branch is not known.
    assert_eq!(s.rows.len(), 1);
    assert!(matches!(
        &s.rows[0].key,
        RowKey::Unassigned { unattributable } if unattributable.input == 107
    ));
}

#[test]
fn session_total_includes_the_sub_agents() {
    let mut h = Harness::new();
    h.open_repo("/work/lib");
    let fake = source();
    let mut parent = Log::new(&fake, "p", "/work/app", AT);
    parent.turn("pt", AT).usage("pt", "pm", AT + 10, 10);
    parent.spawn("pt", "spawn1", AT + 20);
    parent.write();
    let mut kid = child(&fake, "k", "p", "spawn1", "/work/lib", AT + 30);
    kid.turn("kt", AT + 30).usage("kt", "km", AT + 40, 5);
    kid.write();
    h.read_every(&fake);
    settled(&h, &fake, "k");
    settled(&h, &fake, "p");
    let p = session_usage(&h, &fake, "p");
    assert_eq!(p.total.input, 15, "the parent and the sub-agent");
    assert_eq!(p.turns.len(), 1, "the parent's own turns only");
    assert_eq!(p.turns[0].1.input, 10);
    let k = session_usage(&h, &fake, "k");
    assert_eq!(k.total.input, 5);
}

#[test]
fn session_of_an_unknown_session_has_no_snapshot() {
    let h = Harness::new();
    let fake = source();
    let none = h.snapshot(&UsageQuery::Session {
        session: fake.key("nobody"),
    });
    assert!(none.is_none());
}

// ---- call keys ----

#[test]
fn items_call_ids_are_not_stored_as_strings() {
    use crate::store::calls::hash;
    // FNV-1a over the bytes, 64 bits: the same in every version.
    assert_eq!(hash("") as u64, 0xcbf2_9ce4_8422_2325);
    assert_eq!(hash("a") as u64, 0xaf63_dc4c_8601_ec8c);
    assert_ne!(hash("call-1"), hash("call-2"));
    let mut h = Harness::new();
    h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 5);
    log.edit("t1", "secret-call-id-0001", &["/work/lib/a.rs"], AT + 20, 1);
    log.spawn("t1", "secret-call-id-0002", AT + 30);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let conn = conn(&h);
    let strings: i64 = conn
        .query_row(
            "SELECT count(*) FROM intern WHERE instr(text, 'secret-call-id') > 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(strings, 0, "a call ID is not interned");
    // Only the calls a rule needs have a row: the spawn. The edit is a flag
    // on its turn and a row per file.
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM call", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    let (edits, files): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT sum(edits) FROM turn), (SELECT count(*) FROM call_fact)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((edits, files), (1, 1));
}

#[test]
fn items_calls_with_one_key_are_marked_and_their_children_still_count() {
    use crate::store::calls::COLLIDE;
    COLLIDE.with(|c| c.set(true));
    let mut h = Harness::new();
    let app = h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let tools = h.open_repo("/work/tools");
    let fake = source();
    let mut parent = Log::new(&fake, "p", "/work/app", AT);
    parent.turn("pt", AT).usage("pt", "pm", AT + 10, 60);
    // Two different call IDs with one key.
    parent.spawn("pt", "collide-a", AT + 20);
    parent.spawn("pt", "collide-b", AT + 30);
    parent.write();
    let mut a = child(&fake, "ka", "p", "collide-a", "/work/app", AT + 40);
    a.turn("kat", AT + 40);
    a.edit("kat", "kac", &["/work/lib/a.rs"], AT + 50, 2);
    a.write();
    let mut b = child(&fake, "kb", "p", "collide-b", "/work/app", AT + 60);
    b.turn("kbt", AT + 60);
    b.edit("kbt", "kbc", &["/work/tools/b.rs"], AT + 70, 2);
    b.write();
    h.read_every(&fake);
    settled(&h, &fake, "ka");
    settled(&h, &fake, "kb");
    settled(&h, &fake, "p");
    COLLIDE.with(|c| c.set(false));
    let conn = conn(&h);
    // The first call keeps the row, marked; the second has none.
    let (rows, status): (i64, i64) = conn
        .query_row("SELECT count(*), max(status) FROM call", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((rows, status), (1, 9));
    // The children are found by the time they began: both count for the
    // turn that spawned them.
    assert_eq!(
        (input_of(&h, &app), input_of(&h, &lib), input_of(&h, &tools)),
        (0, 30, 30)
    );
}

#[test]
fn items_a_call_read_again_is_not_a_collision() {
    let mut h = Harness::new();
    h.open_repo("/work/lib");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 5);
    log.spawn("t1", "sp1", AT + 20);
    log.write();
    h.read_every(&fake);
    // The log is read again from its start (as after a migration).
    let key = fake.key("s");
    h.close(true);
    let conn = rusqlite::Connection::open(h.db_path()).unwrap();
    conn.execute("UPDATE session SET cursor = X'01'", [])
        .unwrap();
    drop(conn);
    h.reopen(true);
    assert!(h.store().cursor(&key).unwrap().is_start());
    h.read_every(&fake);
    let (rows, ambiguous): (i64, i64) = self::conn(&h)
        .query_row(
            "SELECT count(*), count(CASE WHEN status = 9 THEN 1 END) FROM call",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((rows, ambiguous), (1, 0), "the same call, not a second one");
}
