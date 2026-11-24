//! `edit_*`: the edit mode in a test window, over a session on a real
//! `Engine` and temporary files (docs/design/file_reader.md「测试」): entering,
//! typing and the highlighter's colours, saving, the row under the toolbar
//! (conflict, Merge, Discard My Changes, Keep Editing, Save Anyway), the
//! silent reload of a clean buffer, the reasons Edit… is grey, the keys of
//! the editor and the log. `logs_edit_*` is the canary test of the edit
//! mode's records.

use std::sync::atomic::Ordering;

use gpui::{
    AnyWindowHandle, Context, EntityInputHandler as _, TestAppContext, VisualTestContext, Window,
};
use lectern_i18n::{Locale, Strings, keys};
use lectern_logging::testing;
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::BlobId;
use lectern_services::reader::{Anchor, Reader};
use lectern_services::workspace::{Milestone, WorkspaceCommand};
use lectern_syntax::NotEditable;
use lectern_ui::code_style;
use lectern_ui::semantic::{Record, Recorder};

use super::rig::{file, git_snapshot};
use super::view::{Bar, Rig, attr, find_node};
use crate::ReadingView;
use crate::actions::{self, key_bindings};
use crate::edit::{reason_key, runs};

const CANARY: &str = "canary9w3k6t2h8d";

/// A file with `lines` lines of TypeScript, the first one a comment.
fn source() -> &'static str {
    "// the first line\nconst a = 1;\nconst b = 2;\nconst c = 3;\n"
}

impl Rig {
    /// Runs `f` on the view with its window, as an action's handler would.
    pub(super) fn with_view<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut ReadingView, &mut Window, &mut Context<ReadingView>) -> R,
    ) -> R {
        self.window
            .update(cx, |_, window, cx| {
                self.view.update(cx, |view, cx| f(view, window, cx))
            })
            .expect("the window is open")
    }

    /// Opens `rel` and enters edit mode in it.
    pub(super) fn start(&self, cx: &mut TestAppContext, rel: &str, text: &str) {
        self.write(rel, text);
        self.open(cx, rel, Anchor::Start);
        assert!(self.enter(cx), "edit mode starts");
    }

    pub(super) fn enter(&self, cx: &mut TestAppContext) -> bool {
        let entered = self.with_view(cx, |view, window, cx| view.enter_edit(window, cx));
        self.after(cx);
        entered
    }

    /// Lets the tasks finish and draws, twice over (each pass can wake the
    /// next).
    pub(super) fn after(&self, cx: &mut TestAppContext) {
        self.settle(cx);
        self.draw(cx);
        self.settle(cx);
        self.draw(cx);
    }

    pub(super) fn editing(&self, cx: &mut TestAppContext) -> bool {
        self.read(cx, |view| view.edit.is_some())
    }

    pub(super) fn dirty(&self, cx: &mut TestAppContext) -> bool {
        self.read(cx, |view| view.edit.as_ref().is_some_and(|e| e.dirty()))
    }

    /// The editor's text.
    pub(super) fn buffer(&self, cx: &mut TestAppContext) -> String {
        let editor = self.read(cx, |view| view.edit.as_ref().unwrap().editor.clone());
        editor.read_with(cx, |editor, _| editor.value().to_string())
    }

    /// Types `text` at `offset`, as the user would (the input handler).
    pub(super) fn type_at(&self, cx: &mut TestAppContext, offset: usize, text: &str) {
        let editor = self.read(cx, |view| view.edit.as_ref().unwrap().editor.clone());
        self.window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.replace_text_in_range(Some(offset..offset), text, window, cx);
                });
            })
            .unwrap();
        self.after(cx);
    }

    pub(super) fn on_disk(&self, rel: &str) -> String {
        std::fs::read_to_string(self.tree.root.join(rel)).unwrap()
    }

    /// The toolbar's controls, drawn as `workspace` draws them.
    pub(super) fn bar(&self, cx: &mut TestAppContext) -> Vec<Record> {
        let screen = self.contributions.screen();
        let window = cx.add_window(|_, _| Bar { screen });
        let mut visual = VisualTestContext::from_window(AnyWindowHandle::from(window), cx);
        for _ in 0..2 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
        }
        let id = visual.update(|window, _| window.window_handle().window_id());
        Recorder::begin(id);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        Recorder::finish()
    }

    pub(super) fn controls(&self, cx: &mut TestAppContext) -> usize {
        let screen = self.contributions.screen();
        cx.update(|cx| (screen.toolbar)(cx).controls.len())
    }

    pub(super) fn dispatch<A: gpui::Action>(&self, cx: &mut TestAppContext, action: A) {
        let mut visual = self.visual(cx);
        visual.dispatch_action(action);
        self.after(cx);
    }

    /// A pending save asked for through the keys.
    pub(super) fn save(&self, cx: &mut TestAppContext) {
        let mut visual = self.visual(cx);
        visual.simulate_keystrokes("cmd-s");
        self.after(cx);
    }

    /// Writes `text` to the file as another process would, with another
    /// size so that its identity is another one too.
    pub(super) fn change_on_disk(&self, rel: &str, text: &str) {
        std::fs::write(self.tree.root.join(rel), text).unwrap();
    }
}

fn disabled(records: &[Record], id: &str) -> bool {
    find_node(records, id)
        .unwrap_or_else(|| panic!("{id} is a node"))
        .semantic
        .state
        .disabled
}

fn label_key(records: &[Record], id: &str) -> Option<&'static str> {
    find_node(records, id)?.semantic.label.as_ref()?.key
}

// ----- the switch ------------------------------------------------------------------

#[gpui::test]
fn edit_is_reachable_in_the_shipped_configuration(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.write("a.ts", source());
    rig.open(cx, "a.ts", Anchor::Start);
    // ⌘E and ⌘S are in the keymap, Edit… is in the toolbar with its tooltip.
    let keys: Vec<String> = rig.contributions.keys().into_iter().map(|k| k.0).collect();
    assert!(keys.iter().any(|k| k == "cmd-e"), "{keys:?}");
    assert!(keys.iter().any(|k| k == "cmd-s"), "{keys:?}");
    assert_eq!(rig.controls(cx), 3);
    let records = rig.bar(cx);
    let edit = find_node(&records, "file-reader.toolbar.edit").expect("Edit…");
    assert!(!disabled(&records, "file-reader.toolbar.edit"));
    assert_eq!(
        attr(edit, "tooltip"),
        Some(Strings::of(Locale::En).get(keys::file_reader::EDIT_TOOLTIP))
    );
    assert_eq!(attr(edit, "reason"), None);
    // Back and Forward say what they are in their tooltips too.
    let en = Strings::of(Locale::En);
    for (id, key) in [
        ("file-reader.toolbar.back", keys::file_reader::BACK_TOOLTIP),
        (
            "file-reader.toolbar.forward",
            keys::file_reader::FORWARD_TOOLTIP,
        ),
    ] {
        let node = find_node(&records, id).expect(id);
        assert_eq!(attr(node, "tooltip"), Some(en.get(key)), "{id}");
    }
    // ⌘E enters.
    let mut visual = rig.visual(cx);
    visual.simulate_keystrokes("cmd-e");
    rig.after(cx);
    assert!(rig.editing(cx));
}

#[gpui::test]
fn edit_the_switch_keeps_the_mode_out_of_a_plugin_that_has_it_off(cx: &mut TestAppContext) {
    let rig = Rig::locked(cx);
    rig.write("a.ts", source());
    rig.open(cx, "a.ts", Anchor::Start);
    // No ⌘E and no ⌘S in the keymap, no Edit… button, and the action does
    // nothing when something dispatches it by name.
    let keys: Vec<String> = rig.contributions.keys().into_iter().map(|k| k.0).collect();
    assert!(
        !keys.iter().any(|k| k == "cmd-e" || k == "cmd-s"),
        "{keys:?}"
    );
    assert_eq!(rig.controls(cx), 2);
    assert!(find_node(&rig.bar(cx), "file-reader.toolbar.edit").is_none());
    rig.dispatch(cx, actions::Edit);
    let mut visual = rig.visual(cx);
    visual.simulate_keystrokes("cmd-e");
    assert!(!rig.editing(cx));
    assert!(!rig.with_view(cx, |view, window, cx| view.enter_edit(window, cx)));
    assert!(!rig.editing(cx));
    let records = rig.record(cx);
    let document = records
        .iter()
        .find(|r| {
            r.semantic
                .id
                .as_str()
                .starts_with("file-reader.document.item[")
        })
        .unwrap();
    assert_eq!(attr(document, "mode"), Some("readonly"));
    // The bindings of a plugin with edit mode reachable have both keys.
    let with = key_bindings(true);
    assert_eq!(with.len(), key_bindings(false).len() + 2);
}

#[gpui::test]
fn edit_nothing_exists_before_the_user_enters(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.write("a.ts", source());
    rig.open(cx, "a.ts", Anchor::Start);
    // Opened, drawn, a find opened and closed: no editor, no session, no
    // wait for one.
    let mut visual = rig.visual(cx);
    visual.simulate_keystrokes("cmd-f");
    rig.after(cx);
    assert!(rig.read(cx, |view| view.edit.is_none() && view.reenter.is_none()));
    assert_eq!(
        rig.session.engine().tasks_in_flight(),
        0,
        "no parse, no task of an edit"
    );
}

// ----- entering, typing, colours -------------------------------------------------

#[gpui::test]
fn edit_enter_puts_the_text_in_the_editor_and_marks_the_document(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    assert_eq!(rig.buffer(cx), source());
    assert!(!rig.dirty(cx));
    let records = rig.record(cx);
    let document = records
        .iter()
        .find(|r| {
            r.semantic
                .id
                .as_str()
                .starts_with("file-reader.document.item[")
        })
        .unwrap();
    assert_eq!(attr(document, "mode"), Some("editing"));
    assert_eq!(attr(document, "dirty"), Some("false"));
    // The toolbar: back greyed with its reason, Done, Save (greyed while
    // nothing changed). At most three controls, which is all `workspace`
    // shows.
    assert!(rig.controls(cx) <= 3);
    let bar = rig.bar(cx);
    assert!(disabled(&bar, "file-reader.toolbar.back"));
    assert_eq!(
        label_key(&bar, "file-reader.toolbar.back"),
        Some("file-reader-back-editing.tooltip")
    );
    // The grey Back says why in its tooltip.
    let back = find_node(&bar, "file-reader.toolbar.back").unwrap();
    assert_eq!(
        attr(back, "tooltip"),
        Some(Strings::of(Locale::En).get(keys::file_reader::BACK_EDITING_TOOLTIP))
    );
    assert!(!disabled(&bar, "file-reader.toolbar.done"));
    assert!(disabled(&bar, "file-reader.toolbar.save"));
    assert!(find_node(&bar, "file-reader.toolbar.edit").is_none());
}

#[gpui::test]
fn edit_the_milestone_is_the_editors_first_frame_once_per_edit_mode(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.write("a.ts", source());
    rig.open(cx, "a.ts", Anchor::Start);
    // The document's own milestone is the only one before the edit mode.
    let before = rig.contributions.milestones();
    assert!(!before.contains(&Milestone::EditReady));
    assert!(rig.enter(cx));
    let frame = |rig: &Rig, cx: &mut TestAppContext| {
        let mut visual = rig.visual(cx);
        visual.update(|window, cx| window.simulate_next_frame(cx));
        visual.run_until_parked();
    };
    frame(&rig, cx);
    let ready = |rig: &Rig| {
        rig.contributions
            .milestones()
            .iter()
            .filter(|m| **m == Milestone::EditReady)
            .count()
    };
    assert_eq!(ready(&rig), 1);
    // Drawing again reports nothing; a second edit mode reports its own.
    rig.draw(cx);
    frame(&rig, cx);
    assert_eq!(ready(&rig), 1);
    rig.with_view(cx, |view, window, cx| view.leave_edit(window, cx));
    rig.after(cx);
    assert!(rig.enter(cx));
    frame(&rig, cx);
    assert_eq!(ready(&rig), 2);
}

#[gpui::test]
fn edit_typing_goes_to_the_session_and_the_colours_follow(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    let before = crate::edit::styled_calls();
    rig.type_at(cx, 0, "const typed = \"x\";\n");
    assert!(rig.dirty(cx));
    assert!(
        crate::edit::styled_calls() > before,
        "the editor asked the adapter for the styles of what it painted"
    );
    let buffer = rig.buffer(cx);
    assert!(buffer.starts_with("const typed = \"x\";\n// the first line\n"));
    // The session has the buffer (the view passed it once per frame), and
    // its spans cover the new text: the string literal is a string.
    let (text, spans) = rig.read(cx, |view| {
        let edit = view.edit.as_ref().unwrap();
        (edit.text().clone(), edit.session.spans())
    });
    assert_eq!(&*text, buffer.as_str());
    let palette = cx.update(|cx| *lectern_ui::palette(lectern_ui::Appearance::Light, cx));
    let all = 0..buffer.len();
    let styled = runs(&spans, &all, &palette);
    // Ordered, not overlapping, and every byte of the range is in a run.
    let mut at = 0;
    for (range, _) in &styled {
        assert_eq!(range.start, at, "{styled:?}");
        assert!(range.end > range.start);
        at = range.end;
    }
    assert_eq!(at, buffer.len());
    let string_at = buffer.find("\"x\"").unwrap();
    let string_style = styled
        .iter()
        .find(|(range, _)| range.contains(&string_at))
        .map(|(_, style)| style.color)
        .unwrap();
    assert_eq!(
        string_style,
        Some(code_style(lectern_syntax::TokenKind::String, &palette).color)
    );
    // A range inside a line is covered too (the editor asks for the lines
    // in view, not the whole text).
    let inner = runs(&spans, &(6..12), &palette);
    assert_eq!(inner.first().map(|r| r.0.start), Some(6));
    assert_eq!(inner.last().map(|r| r.0.end), Some(12));
    assert!(runs(&spans, &(5..5), &palette).is_empty());
}

#[gpui::test]
fn edit_the_highlighter_survives_a_replaced_buffer(cx: &mut TestAppContext) {
    // `set_value` (a merge's result, a reload) makes the editor drop its
    // highlighter and make another: the edits that were in flight and the
    // session are not the old one's.
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// one\n");
    let editor = rig.read(cx, |view| view.edit.as_ref().unwrap().editor.clone());
    rig.window
        .update(cx, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_value("let replaced = 2;\n", window, cx)
            });
        })
        .unwrap();
    rig.after(cx);
    rig.type_at(cx, 0, "// two\n");
    assert_eq!(rig.buffer(cx), "// two\nlet replaced = 2;\n");
    let text = rig.read(cx, |view| view.edit.as_ref().unwrap().text().to_string());
    assert_eq!(text, "// two\nlet replaced = 2;\n", "the session has it");
}

#[gpui::test]
fn edit_the_reasons_edit_is_grey_are_the_documents(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    let strings = Strings::of(Locale::En);
    // Each reason the reading can reach, and the key the button names.
    let cases: Vec<(&str, Vec<u8>, NotEditable)> = vec![
        (
            "big.ts",
            vec![b'a'; (1 << 20) + 16]
                .into_iter()
                .enumerate()
                .map(|(i, b)| if i % 80 == 79 { b'\n' } else { b })
                .collect(),
            NotEditable::TooLarge,
        ),
        (
            "long.ts",
            format!("{}\n", "x".repeat(5 << 10)).into_bytes(),
            NotEditable::LongLine,
        ),
        (
            "bin.ts",
            b"let a = 1;\n\0\xff\n".to_vec(),
            NotEditable::NotText,
        ),
        (
            "mixed.ts",
            b"let a = 1;\r\nlet b = 2;\n".to_vec(),
            NotEditable::MixedLineEndings,
        ),
    ];
    for (name, bytes, reason) in cases {
        rig.tree.write(name, &bytes);
        rig.open(cx, name, Anchor::Start);
        let bar = rig.bar(cx);
        assert!(disabled(&bar, "file-reader.toolbar.edit"), "{name}");
        let node = find_node(&bar, "file-reader.toolbar.edit").unwrap();
        assert_eq!(
            attr(node, "reason"),
            Some(reason_key(reason).name()),
            "{name}"
        );
        assert_eq!(
            attr(node, "tooltip"),
            Some(strings.get(reason_key(reason))),
            "{name}"
        );
        // Entering anyway does nothing.
        assert!(
            !rig.with_view(cx, |view, window, cx| view.enter_edit(window, cx)),
            "{name}"
        );
        assert!(!rig.editing(cx));
    }
    // An object from history is read-only.
    let (key, wt) = rig.tree.git();
    rig.projects
        .set(git_snapshot(&key, &wt, &rig.tree.root, &[]));
    let oid = BlobId::Sha1([9; 20]);
    let _share = rig
        .session
        .share_object(oid, std::sync::Arc::from(&b"let old = 1;\n"[..]));
    let mut target = file(&key, "old.ts", Anchor::Start);
    target.worktree = Some(wt);
    target.origin = TextOrigin::Object;
    target.expect = Some(oid);
    rig.session.open(target);
    rig.after(cx);
    let bar = rig.bar(cx);
    let node = find_node(&bar, "file-reader.toolbar.edit").unwrap();
    assert!(node.semantic.state.disabled);
    assert_eq!(
        attr(node, "reason"),
        Some(reason_key(NotEditable::ReadOnlySource).name())
    );
    // A file that can be edited has no reason and an enabled button.
    rig.write("ok.ts", source());
    rig.open(cx, "ok.ts", Anchor::Start);
    let bar = rig.bar(cx);
    let node = find_node(&bar, "file-reader.toolbar.edit").unwrap();
    assert!(!node.semantic.state.disabled);
    assert_eq!(attr(node, "reason"), None);
}

#[test]
fn edit_every_reason_has_its_own_key_in_both_languages() {
    use NotEditable::*;
    let all = [
        NotLoaded,
        TooLarge,
        LongLine,
        NotText,
        MixedLineEndings,
        ReadOnlySource,
        Reloading,
    ];
    let mut names = std::collections::HashSet::new();
    for reason in all {
        for locale in [Locale::En, Locale::ZhCn] {
            assert!(!Strings::of(locale).get(reason_key(reason)).is_empty());
        }
        assert!(names.insert(reason_key(reason).name()));
        assert!(!crate::diag::not_editable(reason).is_empty());
    }
    for locale in [Locale::En, Locale::ZhCn] {
        let strings = Strings::of(locale);
        use keys::file_reader as k;
        for key in [
            k::EDIT,
            k::DONE,
            k::SAVE,
            k::SAVE_ANYWAY,
            k::BACK_EDITING_TOOLTIP,
            k::CHANGED_ON_DISK,
            k::MERGE,
            k::DISCARD_MINE,
            k::KEEP_EDITING,
            k::UNAVAILABLE_WHILE_EDITING,
        ] {
            assert!(!strings.get(key).is_empty(), "{}", key.name());
        }
    }
    let en = Strings::of(Locale::En);
    let zh = Strings::of(Locale::ZhCn);
    assert_ne!(
        en.get(keys::file_reader::DONE),
        zh.get(keys::file_reader::DONE)
    );
}

// ----- saving --------------------------------------------------------------------

#[gpui::test]
fn edit_save_writes_the_file_and_the_buffer_is_clean_again(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "const typed = 1;\n");
    assert!(rig.dirty(cx));
    assert!(!disabled(&rig.bar(cx), "file-reader.toolbar.save"));
    rig.save(cx);
    assert_eq!(
        rig.on_disk("a.ts"),
        format!("const typed = 1;\n{}", source())
    );
    assert!(!rig.dirty(cx), "the save came back");
    assert!(disabled(&rig.bar(cx), "file-reader.toolbar.save"));
    // Typing again, saving again.
    let end = rig.buffer(cx).len();
    rig.type_at(cx, end, "const more = 2;\n");
    assert!(rig.dirty(cx));
    rig.save(cx);
    assert!(rig.on_disk("a.ts").ends_with("const more = 2;\n"));
    assert!(!rig.dirty(cx));
}

#[gpui::test]
fn edit_a_change_made_after_the_save_was_asked_stays_unsaved(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// one\n");
    // Ask, and type before the answer is taken.
    rig.with_view(cx, |view, _, cx| view.save(false, cx));
    let end = rig.buffer(cx).len();
    let editor = rig.read(cx, |view| view.edit.as_ref().unwrap().editor.clone());
    rig.window
        .update(cx, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.replace_text_in_range(Some(end..end), "// two\n", window, cx);
            });
        })
        .unwrap();
    rig.after(cx);
    assert_eq!(rig.on_disk("a.ts"), format!("// one\n{}", source()));
    assert!(rig.dirty(cx), "the second change is not in the file");
}

// ----- the file changed on disk ------------------------------------------------------

/// The rig at a conflict: a buffer with a change of its own, a file changed
/// by someone else, and the save that found out.
fn conflicted(cx: &mut TestAppContext) -> Rig {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// mine\n");
    rig.change_on_disk(
        "a.ts",
        "// the first line\nconst a = 1;\nconst b = 2;\nconst c = 30;\n// theirs\n",
    );
    rig.save(cx);
    rig
}

#[gpui::test]
fn edit_a_conflict_shows_the_row_and_writes_nothing(cx: &mut TestAppContext) {
    let rig = conflicted(cx);
    assert_eq!(
        rig.on_disk("a.ts"),
        "// the first line\nconst a = 1;\nconst b = 2;\nconst c = 30;\n// theirs\n"
    );
    assert!(rig.dirty(cx));
    let records = rig.record(cx);
    let banner = find_node(&records, "file-reader.disk.banner").expect("the row");
    assert_eq!(attr(banner, "kind"), Some("changed"));
    assert_eq!(
        banner.semantic.label.as_ref().unwrap().key,
        Some("file-reader-changed-on-disk")
    );
    for button in ["merge", "discard", "keep"] {
        assert!(
            find_node(&records, &format!("file-reader.disk.{button}")).is_some(),
            "{button}"
        );
    }
    assert!(find_node(&records, "file-reader.disk.failed").is_none());
    // Done does not drop a buffer with changes: it asks.
    rig.with_view(cx, |view, window, cx| assert!(!view.leave_edit(window, cx)));
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    rig.after(cx);
    assert!(rig.editing(cx));
    assert!(rig.dirty(cx));
}

#[gpui::test]
fn edit_merge_brings_the_disks_change_in_and_saves_without_a_conflict(cx: &mut TestAppContext) {
    let rig = conflicted(cx);
    rig.dispatch(cx, actions::Merge);
    let buffer = rig.buffer(cx);
    assert_eq!(
        buffer,
        "// mine\n// the first line\nconst a = 1;\nconst b = 2;\nconst c = 30;\n// theirs\n"
    );
    assert!(rig.dirty(cx), "the merge is not saved");
    assert!(
        find_node(&rig.record(cx), "file-reader.disk.banner").is_none(),
        "the row went with the conflict"
    );
    // The disk's change is the base now: saving does not conflict.
    rig.save(cx);
    assert_eq!(rig.on_disk("a.ts"), buffer);
    assert!(!rig.dirty(cx));
    // The editor can be typed in again (it was read-only while merging).
    rig.type_at(cx, 0, "// again\n");
    assert!(rig.dirty(cx));
}

#[gpui::test]
fn edit_merge_with_a_conflict_marks_it_and_goes_to_it(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", "one\ntwo\nthree\n");
    // Both sides change the middle line.
    let editor = rig.read(cx, |view| view.edit.as_ref().unwrap().editor.clone());
    rig.window
        .update(cx, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.replace_text_in_range(Some(4..7), "mine", window, cx);
            });
        })
        .unwrap();
    rig.after(cx);
    rig.change_on_disk("a.ts", "one\ntheirs!\nthree\n");
    rig.save(cx);
    rig.dispatch(cx, actions::Merge);
    let buffer = rig.buffer(cx);
    assert_eq!(
        buffer,
        "one\n<<<<<<< editor\nmine\n=======\ntheirs!\n>>>>>>> disk\nthree\n"
    );
    assert!(rig.dirty(cx));
    // The cursor is at the first conflict.
    let editor = rig.read(cx, |view| view.edit.as_ref().unwrap().editor.clone());
    let cursor = editor.read_with(cx, |editor, _| editor.cursor());
    assert_eq!(cursor, buffer.find("<<<<<<< editor").unwrap());
}

#[gpui::test]
fn edit_discard_mine_reads_the_file_again_and_edits_on_it(cx: &mut TestAppContext) {
    let rig = conflicted(cx);
    // The cursor is on line 0 of 4; the file's own text comes back.
    rig.dispatch(cx, actions::DiscardMine);
    rig.after(cx);
    assert!(rig.editing(cx), "edit mode started again");
    assert_eq!(
        rig.buffer(cx),
        "// the first line\nconst a = 1;\nconst b = 2;\nconst c = 30;\n// theirs\n"
    );
    assert!(!rig.dirty(cx));
    assert!(find_node(&rig.record(cx), "file-reader.disk.banner").is_none());
    // A new session: saving works and conflicts with nobody.
    rig.type_at(cx, 0, "// fresh\n");
    rig.save(cx);
    assert!(rig.on_disk("a.ts").starts_with("// fresh\n"));
}

/// The cursor on line 3 of four, a change of the buffer, and the file
/// changed to `disk`; then Discard My Changes: the line the cursor is on in
/// the file read again.
fn discard_cursor_line(cx: &mut TestAppContext, disk: &str) -> u32 {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", "a\nb\nc\nd\n");
    let editor = rig.read(cx, |view| view.edit.as_ref().unwrap().editor.clone());
    rig.window
        .update(cx, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_cursor_position(gpui_component::input::Position::new(3, 0), window, cx);
                editor.replace_text_in_range(Some(6..6), "x", window, cx);
            });
        })
        .unwrap();
    rig.after(cx);
    rig.change_on_disk("a.ts", disk);
    rig.save(cx);
    rig.dispatch(cx, actions::DiscardMine);
    rig.after(cx);
    assert!(rig.editing(cx));
    assert_eq!(rig.buffer(cx), disk);
    let editor = rig.read(cx, |view| view.edit.as_ref().unwrap().editor.clone());
    editor.read_with(cx, |editor, _| editor.cursor_position().line)
}

#[gpui::test]
fn edit_discard_keeps_the_cursor_on_its_line(cx: &mut TestAppContext) {
    assert_eq!(discard_cursor_line(cx, "a\nb\nc\nd\ne\nf\n"), 3);
}

#[gpui::test]
fn edit_discard_puts_the_cursor_on_the_last_line_of_a_shorter_file(cx: &mut TestAppContext) {
    // Two lines and the empty one after the last newline.
    assert_eq!(discard_cursor_line(cx, "a\nb\n"), 2);
}

#[gpui::test]
fn edit_save_anyway_and_discard_by_name_need_a_file_changed_on_disk(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// mine\n");
    // Nothing changed on disk: the probe's Save Anyway is no save (it would
    // skip the identity check), Discard My Changes does not drop the buffer
    // (the leave guard would have asked), Merge has nothing to merge.
    rig.dispatch(cx, actions::SaveAnyway);
    assert_eq!(rig.on_disk("a.ts"), source());
    rig.dispatch(cx, actions::DiscardMine);
    rig.dispatch(cx, actions::Merge);
    rig.dispatch(cx, actions::KeepEditing);
    assert!(rig.editing(cx));
    assert!(rig.dirty(cx));
    assert!(rig.buffer(cx).starts_with("// mine\n"));
    assert_eq!(rig.on_disk("a.ts"), source());
    // Keep Editing has no effect on its own either: Save is still Save.
    assert_eq!(
        label_key(&rig.bar(cx), "file-reader.toolbar.save"),
        Some("file-reader-save")
    );
    rig.dispatch(cx, actions::SaveAnyway);
    assert_eq!(rig.on_disk("a.ts"), source());
    rig.save(cx);
    assert!(rig.on_disk("a.ts").starts_with("// mine\n"));
}

#[gpui::test]
fn edit_every_way_the_buffer_changes_counts_as_unsaved(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", "line one\nline two\n");
    assert!(!rig.dirty(cx));
    // A command of the editor (Indent), not the input handler.
    let mut visual = rig.visual(cx);
    visual.simulate_keystrokes("cmd-]");
    rig.after(cx);
    assert!(rig.dirty(cx), "indent");
    rig.save(cx);
    assert!(!rig.dirty(cx));
    // Typed text through the keys, then undone: the buffer is the saved
    // text again, and still counts as changed (a change is a change).
    visual.simulate_input("zz");
    rig.after(cx);
    assert!(rig.dirty(cx), "typed");
    rig.save(cx);
    assert!(!rig.dirty(cx));
    visual.simulate_keystrokes("cmd-z");
    rig.after(cx);
    assert!(rig.dirty(cx), "undo");
    assert_ne!(rig.buffer(cx), rig.on_disk("a.ts"));
    visual.simulate_keystrokes("cmd-shift-z");
    rig.after(cx);
    assert_eq!(
        rig.buffer(cx),
        rig.on_disk("a.ts"),
        "redo is the saved text"
    );
    assert!(rig.dirty(cx), "redo");
    // The session was told all of it: the text it holds is the buffer's.
    let held = rig.read(cx, |view| view.edit.as_ref().unwrap().text().to_string());
    assert_eq!(held, rig.buffer(cx));
}

#[gpui::test]
fn edit_keep_editing_hides_the_row_and_save_becomes_save_anyway(cx: &mut TestAppContext) {
    let rig = conflicted(cx);
    rig.dispatch(cx, actions::KeepEditing);
    assert!(find_node(&rig.record(cx), "file-reader.disk.banner").is_none());
    let bar = rig.bar(cx);
    assert_eq!(
        label_key(&bar, "file-reader.toolbar.save"),
        Some("file-reader-save-anyway")
    );
    // ⌘S is what the button is: Save Anyway replaces the file.
    rig.save(cx);
    assert!(rig.on_disk("a.ts").starts_with("// mine\n"));
    assert!(!rig.dirty(cx));
    assert!(find_node(&rig.record(cx), "file-reader.disk.banner").is_none());
    assert_eq!(
        label_key(&rig.bar(cx), "file-reader.toolbar.save"),
        Some("file-reader-save")
    );
}

#[gpui::test]
fn edit_a_clean_buffer_is_reloaded_when_the_file_changes(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    // Nothing typed; the file changes, and the session finds out (here by a
    // save of the same text: a folder project has no watcher).
    rig.change_on_disk("a.ts", "const other = 5;\nconst a = 1;\nconst b = 2;\n");
    rig.with_view(cx, |view, _, _| {
        let edit = view.edit.as_ref().unwrap();
        edit.session.save(edit.text().clone());
    });
    rig.after(cx);
    rig.after(cx);
    assert!(rig.editing(cx), "edit mode is back");
    assert_eq!(
        rig.buffer(cx),
        "const other = 5;\nconst a = 1;\nconst b = 2;\n"
    );
    assert!(!rig.dirty(cx));
    assert!(find_node(&rig.record(cx), "file-reader.disk.banner").is_none());
}

#[gpui::test]
fn edit_a_gone_file_says_so_and_only_discard_is_left(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// mine\n");
    std::fs::remove_file(rig.tree.root.join("a.ts")).unwrap();
    rig.save(cx);
    let records = rig.record(cx);
    let banner = find_node(&records, "file-reader.disk.banner").expect("the row");
    assert_eq!(attr(banner, "kind"), Some("unavailable"));
    assert_eq!(
        banner.semantic.label.as_ref().unwrap().key,
        Some("file-reader-unavailable-while-editing")
    );
    assert!(find_node(&records, "file-reader.disk.discard").is_some());
    assert!(find_node(&records, "file-reader.disk.merge").is_none());
    // Discarding ends edit mode; the file is gone, so there is nothing to
    // start again on.
    rig.dispatch(cx, actions::DiscardMine);
    rig.after(cx);
    assert!(!rig.editing(cx));
    assert!(rig.read(cx, |view| view.reenter.is_none()));
}

#[gpui::test]
fn edit_a_failed_save_shows_one_line_and_keeps_the_buffer(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::editable(cx);
    rig.write("sub/a.ts", source());
    rig.open(cx, "sub/a.ts", Anchor::Start);
    assert!(rig.enter(cx));
    rig.type_at(cx, 0, "// mine\n");
    // The folder cannot be written to: the temporary file cannot be made.
    let dir = rig.tree.root.join("sub");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    rig.save(cx);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let records = rig.record(cx);
    let failed = find_node(&records, "file-reader.disk.failed").expect("the line");
    assert_eq!(
        failed.semantic.label.as_ref().unwrap().key,
        Some("file-reader-save-failed")
    );
    assert!(
        failed
            .semantic
            .label
            .as_ref()
            .unwrap()
            .text
            .contains("a.ts")
    );
    assert!(rig.dirty(cx), "still unsaved");
    assert_eq!(rig.on_disk("sub/a.ts"), source());
    // Saving again works and the line goes.
    rig.save(cx);
    assert!(rig.on_disk("sub/a.ts").starts_with("// mine\n"));
    assert!(find_node(&rig.record(cx), "file-reader.disk.failed").is_none());
}

// ----- the keys, history and requests ------------------------------------------------

#[gpui::test]
fn edit_back_and_forward_keys_indent_and_the_toolbar_is_grey_while_editing(
    cx: &mut TestAppContext,
) {
    let rig = Rig::editable(cx);
    rig.write("b.ts", "const b = 1;\n");
    rig.open(cx, "b.ts", Anchor::Start);
    rig.start(cx, "a.ts", "line one\nline two\n");
    let history = rig.session.history_len();
    // The buttons are grey (the back one says why).
    let records = rig.bar(cx);
    assert!(disabled(&records, "file-reader.toolbar.back"));
    // ⌘] is the editor's Indent: it changes the buffer, not the history.
    let before = rig.buffer(cx);
    let mut visual = rig.visual(cx);
    visual.simulate_keystrokes("cmd-]");
    rig.after(cx);
    assert_ne!(rig.buffer(cx), before, "indented");
    assert!(rig.dirty(cx), "an indent is a change");
    assert_eq!(rig.session.history_len(), history);
    assert!(rig.editing(cx));
    assert!(!cx.has_pending_prompt());
    visual.simulate_keystrokes("cmd-[");
    rig.after(cx);
    assert_eq!(rig.buffer(cx), before, "outdented");
    assert_eq!(rig.session.history_len(), history);
    assert!(
        !rig.contributions
            .dispatched()
            .contains(&WorkspaceCommand::Pop)
    );
}

#[gpui::test]
fn edit_escape_goes_to_the_editor_first_and_never_pops_the_screen(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    // `workspace`'s Esc, as the find test's stand-in.
    cx.update(|cx| {
        cx.bind_keys([gpui::KeyBinding::new(
            "escape",
            super::view::ProbeEscape,
            Some("Content"),
        )]);
    });
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// mine\n");
    let mut visual = rig.visual(cx);
    // Unsaved: the key has nothing to close in the editor, and the view
    // asks before it drops the buffer. It does not reach `workspace` either.
    visual.simulate_keystrokes("escape");
    rig.after(cx);
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    rig.after(cx);
    assert!(rig.editing(cx));
    assert!(rig.dirty(cx));
    assert_eq!(rig.probe.load(Ordering::SeqCst), 0, "not workspace's");
    // Saved: Esc leaves edit mode, still not `workspace`'s.
    rig.save(cx);
    visual.simulate_keystrokes("escape");
    rig.after(cx);
    assert!(!rig.editing(cx));
    assert_eq!(rig.probe.load(Ordering::SeqCst), 0);
    // Not editing: the key goes on to `workspace`.
    visual.simulate_keystrokes("escape");
    assert_eq!(rig.probe.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn edit_done_after_a_save_shows_the_saved_file(cx: &mut TestAppContext) {
    let rig = Rig::editable(cx);
    rig.start(cx, "a.ts", source());
    rig.type_at(cx, 0, "// saved\n");
    rig.save(cx);
    // ⌘E leaves, the document is read again with what was saved.
    rig.dispatch(cx, actions::Edit);
    rig.after(cx);
    rig.after(cx);
    assert!(!rig.editing(cx));
    let size = rig.read(cx, |view| view.snapshot.as_ref().map(|s| s.size));
    assert_eq!(size, Some(rig.on_disk("a.ts").len() as u64));
    // And edit mode can be entered again, from the saved text.
    assert!(rig.enter(cx));
    assert!(rig.buffer(cx).starts_with("// saved\n"));
}

// ----- the log ------------------------------------------------------------------------

#[gpui::test]
fn logs_edit_holds_no_buffer_text(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let log = testing::capture();
    let rig = Rig::editable(cx);
    let name = format!("{CANARY}/{CANARY}.ts");
    rig.write(
        &name,
        &format!("// {CANARY}\nconst {CANARY} = \"{CANARY}\";\n"),
    );
    rig.open(cx, &name, Anchor::Start);
    assert!(rig.enter(cx));
    // Typing the canary, a save, a conflict, a merge that leaves markers,
    // a save that fails, a discard.
    rig.type_at(cx, 0, &format!("// typed {CANARY}\n"));
    rig.change_on_disk(
        &name,
        &format!("// disk {CANARY}\nconst {CANARY} = \"{CANARY}\";\n"),
    );
    rig.save(cx);
    rig.dispatch(cx, actions::Merge);
    let dir = rig.tree.root.join(CANARY);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    rig.save(cx);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The file changes once more, and the user gives the buffer up.
    rig.change_on_disk(&name, &format!("// again {CANARY}\n"));
    rig.save(cx);
    rig.dispatch(cx, actions::DiscardMine);
    rig.after(cx);
    // Edit mode starts again on the file read again: Done with unsaved
    // typing asks, and the user cancels.
    rig.type_at(cx, 0, &format!("// last {CANARY}\n"));
    rig.dispatch(cx, actions::Edit);
    cx.simulate_prompt_answer("Cancel");
    rig.after(cx);

    let lines = log.lines();
    let has = |msg: &str, level: &str| {
        lines.iter().any(|l| {
            l.contains(&format!("\"msg\":\"{msg}\""))
                && l.contains(&format!("\"lv\":\"{level}\""))
                && l.contains("\"tg\":\"file_reader\"")
        })
    };
    assert!(
        has("a merge left conflict markers in the buffer", "info"),
        "{lines:#?}"
    );
    assert!(
        has("a save failed; the buffer stays unsaved", "warn"),
        "{lines:#?}"
    );
    assert!(
        has("the buffer was given up for the file on disk", "info"),
        "{lines:#?}"
    );
    let given_up = lines
        .iter()
        .find(|l| l.contains("the buffer was given up for the file on disk"))
        .unwrap();
    assert!(given_up.contains("\"why\":\"discard\""), "{given_up}");
    let asked = lines
        .iter()
        .find(|l| l.contains("a leave was asked about unsaved changes"))
        .unwrap_or_else(|| panic!("{lines:#?}"));
    assert!(asked.contains("\"scope\":\"done\""), "{asked}");
    assert!(asked.contains("\"outcome\":\"cancel\""), "{asked}");
    log.assert_absent(CANARY);
}
