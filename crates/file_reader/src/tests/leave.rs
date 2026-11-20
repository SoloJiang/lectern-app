//! `leave_guard_*`: the leave guard in a test window (docs/design/file_reader.md
//! 「编辑」的「离开守卫」, 「测试」): what `workspace` and `app` ask through the
//! registered guard, and what the view's own ways out (Done, Esc, ⌘E, a
//! request to open another file, back and forward by name) ask themselves.
//! The confirmation is the platform's alert: the test platform records it and
//! the test answers it by the label of a button.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use gpui::TestAppContext;
use lectern_i18n::{Locale, Strings, keys};
use lectern_logging::testing;
use lectern_services::project::ProjectKey;
use lectern_services::reader::{Anchor, OpenOutcome, Reader, ReaderEvent};
use lectern_services::workspace::LeaveScope;

use super::rig::file;
use super::view::{Rig, attr, find_node};
use crate::actions;

const CANARY: &str = "canary4q8m2z7v1c";

fn source() -> &'static str {
    "// the first line\nconst a = 1;\nconst b = 2;\n"
}

/// What a question was answered, once it is.
type Verdict = Arc<Mutex<Option<bool>>>;

impl Rig {
    /// Asks the registered guard, as `workspace` or `app` does.
    fn ask(&self, cx: &mut TestAppContext, scope: LeaveScope) -> Verdict {
        let guards = self.contributions.guards();
        assert_eq!(guards.len(), 1, "one guard for the window");
        let verdict: Verdict = Arc::default();
        let sink = verdict.clone();
        guards[0].ask(
            scope,
            Box::new(move |pass| *sink.lock().unwrap() = Some(pass)),
        );
        self.after(cx);
        verdict
    }

    /// Answers the confirmation that is up with the button `label`.
    fn answer(&self, cx: &mut TestAppContext, label: &str) {
        assert!(cx.has_pending_prompt(), "a confirmation is up");
        cx.simulate_prompt_answer(label);
        self.after(cx);
    }

    /// The rig in edit mode on `a.ts` with something typed.
    fn dirty_edit(cx: &mut TestAppContext) -> Rig {
        let rig = Rig::editable(cx);
        rig.start(cx, "a.ts", source());
        rig.type_at(cx, 0, "// mine\n");
        assert!(rig.dirty(cx));
        rig
    }

    fn mine(&self) -> String {
        format!("// mine\n{}", source())
    }
}

fn done(verdict: &Verdict) -> Option<bool> {
    *verdict.lock().unwrap()
}

fn another(key: &ProjectKey) -> ProjectKey {
    match key {
        ProjectKey::Folder { root } => ProjectKey::Folder {
            root: root.join("elsewhere").into(),
        },
        other => other.clone(),
    }
}

// ----- nothing before the user edits -------------------------------------------------

#[gpui::test]
fn leave_guard_one_guard_per_window_and_nothing_before_edit_mode(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    assert_eq!(rig.contributions.guards().len(), 1);
    rig.write("a.ts", source());
    rig.open(cx, "a.ts", Anchor::Start);
    assert!(rig.read(cx, |view| view.edit.is_none()));
    assert!(!rig.read(cx, |view| view.shared.asks_served()), "no task");
    // Every scope passes at once, with no alert.
    for scope in [
        LeaveScope::View,
        LeaveScope::Project(rig.key.clone()),
        LeaveScope::Worktree(rig.key.clone()),
        LeaveScope::Window,
        LeaveScope::App,
    ] {
        let verdict = rig.ask(cx, scope);
        assert_eq!(done(&verdict), Some(true));
    }
    assert!(!cx.has_pending_prompt());
    assert_eq!(rig.session.engine().tasks_in_flight(), 0);
}

#[gpui::test]
fn leave_guard_the_question_goes_only_to_a_view_that_is_editing(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    assert!(rig.read(cx, |view| view.shared.asks_served()));
    // Done (nothing unsaved): the edit ends and the slot is empty again.
    rig.dispatch(cx, actions::Edit);
    assert!(!rig.editing(cx));
    assert!(!rig.read(cx, |view| view.shared.asks_served()));
    let verdict = rig.ask(cx, LeaveScope::App);
    assert_eq!(done(&verdict), Some(true));
}

// ----- who is asked ------------------------------------------------------------------

#[gpui::test]
fn leave_guard_a_clean_buffer_passes_without_asking(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    for scope in [
        LeaveScope::View,
        LeaveScope::Project(rig.key.clone()),
        LeaveScope::Worktree(rig.key.clone()),
        LeaveScope::Window,
        LeaveScope::App,
    ] {
        let verdict = rig.ask(cx, scope);
        assert_eq!(done(&verdict), Some(true));
        assert!(!cx.has_pending_prompt());
    }
    assert!(rig.editing(cx), "the guard ends nothing by itself");
    assert!(!rig.dirty(cx));
}

#[gpui::test]
fn leave_guard_a_scope_that_misses_the_document_passes_with_unsaved_changes(
    cx: &mut TestAppContext,
) {
    let rig = Rig::dirty_edit(cx);
    let other = another(&rig.key);
    for scope in [
        LeaveScope::Project(other.clone()),
        LeaveScope::Worktree(other),
    ] {
        let verdict = rig.ask(cx, scope);
        assert_eq!(done(&verdict), Some(true));
        assert!(!cx.has_pending_prompt());
    }
    assert!(rig.editing(cx));
    assert!(rig.dirty(cx));
}

#[gpui::test]
fn leave_guard_every_scope_that_reaches_the_document_asks_and_cancel_keeps_it(
    cx: &mut TestAppContext,
) {
    let rig = Rig::dirty_edit(cx);
    for scope in [
        LeaveScope::View,
        LeaveScope::Project(rig.key.clone()),
        LeaveScope::Worktree(rig.key.clone()),
        LeaveScope::Window,
        LeaveScope::App,
    ] {
        let verdict = rig.ask(cx, scope.clone());
        assert_eq!(done(&verdict), None, "{scope:?} waits for the user");
        // The alert names the file and what is at stake, in the language.
        let (title, detail) = cx.pending_prompt().expect("a confirmation");
        let strings = Strings::of(Locale::En);
        // (Fluent wraps the file name in direction isolates.)
        assert_eq!(
            title.replace(['\u{2068}', '\u{2069}'], ""),
            "Save changes to a.ts?"
        );
        assert_eq!(detail, strings.get(keys::file_reader::UNSAVED_MESSAGE));
        rig.answer(cx, "Cancel");
        assert_eq!(done(&verdict), Some(false), "{scope:?}");
        assert!(rig.editing(cx));
        assert!(rig.dirty(cx));
        assert_eq!(rig.buffer(cx), rig.mine(), "the buffer is as it was");
        assert_eq!(rig.on_disk("a.ts"), source(), "nothing was written");
    }
}

// ----- the three answers ---------------------------------------------------------------

#[gpui::test]
fn leave_guard_save_writes_the_file_and_passes(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    let verdict = rig.ask(cx, LeaveScope::App);
    assert_eq!(done(&verdict), None);
    rig.answer(cx, "Save");
    assert_eq!(done(&verdict), Some(true));
    assert_eq!(rig.on_disk("a.ts"), rig.mine());
    assert!(!rig.dirty(cx), "the buffer is the file");
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
fn leave_guard_dont_save_passes_and_drops_the_session(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    let verdict = rig.ask(cx, LeaveScope::Window);
    rig.answer(cx, "Don't Save");
    assert_eq!(done(&verdict), Some(true));
    assert!(!rig.editing(cx), "the session is gone");
    assert!(!rig.read(cx, |view| view.shared.asks_served()));
    assert_eq!(rig.on_disk("a.ts"), source(), "nothing was written");
}

#[gpui::test]
fn leave_guard_a_save_that_conflicts_fails_and_shows_the_row(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    let theirs = "// the first line\nconst a = 1;\nconst b = 2;\n// theirs\n";
    rig.change_on_disk("a.ts", theirs);
    let verdict = rig.ask(cx, LeaveScope::App);
    rig.answer(cx, "Save");
    assert_eq!(done(&verdict), Some(false));
    assert!(rig.editing(cx));
    assert!(rig.dirty(cx));
    assert_eq!(rig.on_disk("a.ts"), theirs, "the other side's file is kept");
    let records = rig.record(cx);
    let banner = find_node(&records, "file-reader.disk.banner").expect("the row");
    assert_eq!(attr(banner, "kind"), Some("changed"));
    assert!(!cx.has_pending_prompt(), "no second question");
}

#[gpui::test]
fn leave_guard_save_after_keep_editing_is_save_anyway(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    // A save that finds the file changed shows the row; the user keeps
    // editing, and the toolbar's Save is Save Anyway from then on. So is the
    // guard's.
    rig.change_on_disk("a.ts", "// theirs\n");
    rig.save(cx);
    assert!(find_node(&rig.record(cx), "file-reader.disk.banner").is_some());
    rig.dispatch(cx, actions::KeepEditing);
    assert!(find_node(&rig.record(cx), "file-reader.disk.banner").is_none());
    let verdict = rig.ask(cx, LeaveScope::View);
    rig.answer(cx, "Save");
    assert_eq!(done(&verdict), Some(true));
    assert_eq!(rig.on_disk("a.ts"), rig.mine());
}

#[gpui::test]
fn leave_guard_a_failed_save_fails_and_says_so(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    std::fs::create_dir_all(rig.tree.root.join("sub")).unwrap();
    rig.start(cx, "sub/a.ts", source());
    rig.type_at(cx, 0, "// mine\n");
    let dir = rig.tree.root.join("sub");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let verdict = rig.ask(cx, LeaveScope::App);
    rig.answer(cx, "Save");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(done(&verdict), Some(false));
    assert!(rig.editing(cx));
    assert!(rig.dirty(cx));
    assert!(find_node(&rig.record(cx), "file-reader.disk.failed").is_some());
    assert_eq!(rig.on_disk("sub/a.ts"), source());
}

#[gpui::test]
fn leave_guard_a_save_while_merging_fails_at_once(cx: &mut TestAppContext) {
    let log = testing::capture();
    let rig = Rig::dirty_edit(cx);
    // A merge is on its way (the flag is what `save` looks at): the buffer
    // cannot be saved now, and the guard says no without a write.
    rig.with_view(cx, |view, _, _| {
        view.edit.as_mut().expect("editing").merging = true;
    });
    let verdict = rig.ask(cx, LeaveScope::App);
    rig.answer(cx, "Save");
    assert_eq!(done(&verdict), Some(false));
    assert!(rig.editing(cx));
    assert!(rig.dirty(cx));
    assert_eq!(rig.on_disk("a.ts"), source());
    assert!(
        log.lines()
            .iter()
            .any(|l| l.contains("a leave was asked about unsaved changes")
                && l.contains("\"outcome\":\"failed\"")),
        "{:#?}",
        log.lines()
    );
}

#[gpui::test]
fn leave_guard_save_passes_when_the_buffer_was_saved_while_the_question_was_up(
    cx: &mut TestAppContext,
) {
    let rig = Rig::dirty_edit(cx);
    let verdict = rig.ask(cx, LeaveScope::App);
    assert_eq!(done(&verdict), None);
    // ⌘S, with the confirmation still up: the buffer is the file.
    rig.save(cx);
    assert!(!rig.dirty(cx));
    assert!(cx.has_pending_prompt());
    rig.answer(cx, "Save");
    assert_eq!(done(&verdict), Some(true));
    assert_eq!(rig.on_disk("a.ts"), rig.mine());
}

// ----- the questions that come together ------------------------------------------------

#[gpui::test]
fn leave_guard_a_second_question_waits_and_cancel_refuses_both(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    let first = rig.ask(cx, LeaveScope::View);
    let second = rig.ask(cx, LeaveScope::App);
    assert_eq!(done(&second), None);
    rig.answer(cx, "Cancel");
    assert_eq!(done(&first), Some(false));
    assert_eq!(done(&second), Some(false));
    assert!(!cx.has_pending_prompt(), "asked once");
}

#[gpui::test]
fn leave_guard_a_second_question_passes_after_a_save(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    let first = rig.ask(cx, LeaveScope::View);
    let second = rig.ask(cx, LeaveScope::App);
    rig.answer(cx, "Save");
    assert_eq!(done(&first), Some(true));
    assert_eq!(done(&second), Some(true), "the buffer is saved now");
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
fn leave_guard_a_question_is_answered_when_the_edit_ends_under_it(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    let first = rig.ask(cx, LeaveScope::View);
    let second = rig.ask(cx, LeaveScope::App);
    rig.answer(cx, "Don't Save");
    assert_eq!(done(&first), Some(true));
    assert_eq!(done(&second), Some(true), "no buffer is left to lose");
}

// ----- the view's own ways out -----------------------------------------------------------

#[gpui::test]
fn leave_guard_done_esc_and_cmd_e_ask_and_cancel_stays(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    let mut visual = rig.visual(cx);
    // Done, as the button and ⌘E do.
    rig.dispatch(cx, actions::Edit);
    rig.answer(cx, "Cancel");
    assert!(rig.editing(cx));
    // The key.
    visual.simulate_keystrokes("cmd-e");
    rig.after(cx);
    rig.answer(cx, "Cancel");
    assert!(rig.editing(cx));
    // Esc.
    visual.simulate_keystrokes("escape");
    rig.after(cx);
    rig.answer(cx, "Cancel");
    assert!(rig.editing(cx));
    assert!(rig.dirty(cx));
    assert_eq!(rig.on_disk("a.ts"), source());
}

#[gpui::test]
fn leave_guard_done_with_dont_save_leaves_and_shows_the_file_as_it_is(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    rig.dispatch(cx, actions::Edit);
    rig.answer(cx, "Don't Save");
    assert!(!rig.editing(cx));
    assert_eq!(rig.on_disk("a.ts"), source());
    let size = rig.read(cx, |view| view.snapshot.as_ref().map(|s| s.size));
    assert_eq!(size, Some(source().len() as u64));
}

#[gpui::test]
fn leave_guard_done_with_save_leaves_and_shows_what_was_saved(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    rig.dispatch(cx, actions::Edit);
    rig.answer(cx, "Save");
    rig.after(cx);
    assert!(!rig.editing(cx));
    assert_eq!(rig.on_disk("a.ts"), rig.mine());
    let size = rig.read(cx, |view| view.snapshot.as_ref().map(|s| s.size));
    assert_eq!(size, Some(rig.mine().len() as u64));
}

#[gpui::test]
fn leave_guard_done_with_a_save_that_conflicts_stays_in_edit_mode(cx: &mut TestAppContext) {
    let rig = Rig::dirty_edit(cx);
    rig.change_on_disk("a.ts", "// theirs\n");
    rig.dispatch(cx, actions::Edit);
    rig.answer(cx, "Save");
    assert!(rig.editing(cx));
    assert!(find_node(&rig.record(cx), "file-reader.disk.banner").is_some());
}

#[gpui::test]
fn leave_guard_a_clean_buffer_lets_a_request_through(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.write("b.ts", "const b = 1;\n");
    rig.start(cx, "a.ts", source());
    let ticket = rig.session.open(file(&rig.key, "b.ts", Anchor::Start));
    rig.after(cx);
    assert!(!cx.has_pending_prompt());
    assert!(!rig.editing(cx), "the edit ended for the other file");
    assert_eq!(rig.session.pending(), 0);
    assert_eq!(&*rig.session.last_target().unwrap().path, "b.ts");
    assert!(
        rig.kernel
            .emitted::<ReaderEvent>()
            .contains(&ReaderEvent::Open {
                ticket,
                outcome: OpenOutcome::Shown,
            })
    );
}

#[gpui::test]
fn leave_guard_a_request_with_unsaved_changes_asks_and_cancel_cancels_it(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.write("b.ts", "const b = 1;\n");
    rig.write("c.ts", "const c = 1;\n");
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// mine\n");
    let first = rig.session.open(file(&rig.key, "b.ts", Anchor::Start));
    let second = rig.session.open(file(&rig.key, "c.ts", Anchor::Start));
    rig.after(cx);
    assert!(cx.has_pending_prompt());
    assert_eq!(&*rig.session.last_target().unwrap().path, "a.ts");
    rig.answer(cx, "Cancel");
    // The request, and the one behind it, are over; the buffer stays.
    assert!(!cx.has_pending_prompt(), "asked once");
    let events = rig.kernel.emitted::<ReaderEvent>();
    for ticket in [first, second] {
        assert!(events.contains(&ReaderEvent::Open {
            ticket,
            outcome: OpenOutcome::Cancelled,
        }));
    }
    assert!(rig.editing(cx));
    assert!(rig.dirty(cx));
    assert_eq!(rig.session.pending(), 0);
    assert_eq!(&*rig.session.last_target().unwrap().path, "a.ts");
    assert_eq!(rig.buffer(cx), rig.mine());
}

#[gpui::test]
fn leave_guard_a_request_with_dont_save_shows_the_other_file(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.write("b.ts", "const b = 1;\n");
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// mine\n");
    let ticket = rig.session.open(file(&rig.key, "b.ts", Anchor::Start));
    rig.after(cx);
    rig.answer(cx, "Don't Save");
    assert!(!rig.editing(cx));
    assert_eq!(&*rig.session.last_target().unwrap().path, "b.ts");
    assert_eq!(rig.on_disk("a.ts"), source());
    assert!(
        rig.kernel
            .emitted::<ReaderEvent>()
            .contains(&ReaderEvent::Open {
                ticket,
                outcome: OpenOutcome::Shown,
            })
    );
}

#[gpui::test]
fn leave_guard_requests_wait_in_order_behind_the_question(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.write("b.ts", "const b = 1;\n");
    rig.write("c.ts", "const c = 1;\n");
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// mine\n");
    let first = rig.session.open(file(&rig.key, "b.ts", Anchor::Start));
    let second = rig.session.open(file(&rig.key, "c.ts", Anchor::Start));
    rig.after(cx);
    assert_eq!(rig.session.pending(), 1, "the second waits for the answer");
    rig.answer(cx, "Don't Save");
    rig.after(cx);
    assert!(!cx.has_pending_prompt());
    let shown: Vec<_> = rig
        .kernel
        .emitted::<ReaderEvent>()
        .into_iter()
        .filter(|event| {
            matches!(
                event,
                ReaderEvent::Open {
                    outcome: OpenOutcome::Shown,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(
        shown[shown.len() - 2..],
        [first, second].map(|ticket| ReaderEvent::Open {
            ticket,
            outcome: OpenOutcome::Shown
        })
    );
    assert_eq!(&*rig.session.last_target().unwrap().path, "c.ts");
}

#[gpui::test]
fn leave_guard_a_request_with_save_writes_then_shows_the_other_file(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.write("b.ts", "const b = 1;\n");
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// mine\n");
    rig.session.open(file(&rig.key, "b.ts", Anchor::Start));
    rig.after(cx);
    rig.answer(cx, "Save");
    rig.after(cx);
    assert_eq!(rig.on_disk("a.ts"), rig.mine());
    assert!(!rig.editing(cx));
    assert_eq!(&*rig.session.last_target().unwrap().path, "b.ts");
}

#[gpui::test]
fn leave_guard_a_request_after_a_conflicting_save_is_cancelled(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.write("b.ts", "const b = 1;\n");
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// mine\n");
    rig.change_on_disk("a.ts", "// theirs\n");
    let ticket = rig.session.open(file(&rig.key, "b.ts", Anchor::Start));
    rig.after(cx);
    rig.answer(cx, "Save");
    rig.after(cx);
    assert!(rig.editing(cx));
    assert!(
        rig.kernel
            .emitted::<ReaderEvent>()
            .contains(&ReaderEvent::Open {
                ticket,
                outcome: OpenOutcome::Cancelled,
            })
    );
    assert_eq!(&*rig.session.last_target().unwrap().path, "a.ts");
}

#[gpui::test]
fn leave_guard_back_and_forward_by_name_ask(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.write("b.ts", "const b = 1;\n");
    rig.open(cx, "b.ts", Anchor::Start);
    rig.start(cx, "a.ts", source());
    let history = rig.session.history_len();
    rig.type_at(cx, 0, "// mine\n");
    // Back by name (the probe): asks, Cancel changes nothing.
    rig.with_view(cx, |view, window, cx| view.go_back(window, cx));
    rig.after(cx);
    rig.answer(cx, "Cancel");
    assert!(rig.editing(cx));
    assert_eq!(rig.session.history_len(), history);
    // Forward with nothing ahead: nothing to ask about.
    rig.with_view(cx, |view, window, cx| view.go_forward(window, cx));
    rig.after(cx);
    assert!(!cx.has_pending_prompt());
    assert!(rig.editing(cx));
    // Back with Don't Save goes back to b.ts.
    rig.with_view(cx, |view, window, cx| view.go_back(window, cx));
    rig.after(cx);
    rig.answer(cx, "Don't Save");
    assert!(!rig.editing(cx));
    assert_eq!(&*rig.session.last_target().unwrap().path, "b.ts");
    assert_eq!(rig.on_disk("a.ts"), source());
    // With a position ahead, forward by name asks as well.
    assert!(rig.enter(cx));
    rig.type_at(cx, 0, "// mine\n");
    let history = rig.session.history_len();
    rig.with_view(cx, |view, window, cx| view.go_forward(window, cx));
    rig.after(cx);
    rig.answer(cx, "Cancel");
    assert!(rig.editing(cx));
    assert_eq!(rig.session.history_len(), history);
    rig.with_view(cx, |view, window, cx| view.go_forward(window, cx));
    rig.after(cx);
    rig.answer(cx, "Don't Save");
    assert!(!rig.editing(cx));
    assert_eq!(&*rig.session.last_target().unwrap().path, "a.ts");
}

#[gpui::test]
fn leave_guard_back_with_a_clean_buffer_goes_without_asking(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.write("b.ts", "const b = 1;\n");
    rig.open(cx, "b.ts", Anchor::Start);
    rig.start(cx, "a.ts", source());
    rig.with_view(cx, |view, window, cx| view.go_back(window, cx));
    rig.after(cx);
    assert!(!cx.has_pending_prompt());
    assert!(!rig.editing(cx));
    assert_eq!(&*rig.session.last_target().unwrap().path, "b.ts");
}

// ----- the log -----------------------------------------------------------------------------

#[gpui::test]
fn logs_leave_guard_holds_the_scope_and_the_outcome_only(cx: &mut TestAppContext) {
    let log = testing::capture();
    let rig = Rig::editable(cx);
    let name = format!("{CANARY}.ts");
    rig.write(&name, &format!("// {CANARY}\nconst {CANARY} = 1;\n"));
    rig.open(cx, &name, Anchor::Start);
    assert!(rig.enter(cx));
    rig.type_at(cx, 0, &format!("// typed {CANARY}\n"));
    // Cancel, a conflict, Don't Save, Save.
    let verdict = rig.ask(cx, LeaveScope::Window);
    rig.answer(cx, "Cancel");
    assert_eq!(done(&verdict), Some(false));
    rig.change_on_disk(&name, &format!("// disk {CANARY}\nconst {CANARY} = 1;\n"));
    let verdict = rig.ask(cx, LeaveScope::Project(rig.key.clone()));
    rig.answer(cx, "Save");
    assert_eq!(done(&verdict), Some(false));
    rig.dispatch(cx, actions::Edit);
    rig.answer(cx, "Don't Save");
    assert!(!rig.editing(cx));
    rig.write(&name, "const again = 1;\n");
    rig.open(cx, &name, Anchor::Start);
    rig.after(cx);
    assert!(rig.enter(cx));
    rig.type_at(cx, 0, "// more\n");
    let verdict = rig.ask(cx, LeaveScope::App);
    rig.answer(cx, "Save");
    assert_eq!(done(&verdict), Some(true));

    let lines = log.lines();
    let asked: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("a leave was asked about unsaved changes"))
        .collect();
    let has = |scope: &str, outcome: &str| {
        asked.iter().any(|l| {
            l.contains("\"lv\":\"info\"")
                && l.contains("\"tg\":\"file_reader\"")
                && l.contains(&format!("\"scope\":\"{scope}\""))
                && l.contains(&format!("\"outcome\":\"{outcome}\""))
        })
    };
    assert!(has("window", "cancel"), "{lines:#?}");
    assert!(has("project", "conflict"), "{lines:#?}");
    assert!(has("done", "dont_save"), "{lines:#?}");
    assert!(has("app", "saved"), "{lines:#?}");
    log.assert_absent(CANARY);
}
