//! `edit_*`, `save_*`, `merge_*`: edit mode's domain part
//! (docs/design/file_reader.md「编辑」「测试」). The incremental parse against
//! one query over its own tree and against a whole parse; the spans moved at
//! once by `changed`; edits during a round; the BOM and `\r\n` put back on
//! save; conflicts on another identity; Save Anyway; fs_watch events while
//! editing; the three-way merge; what ending a session does to the document.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use lectern_services::reader::Anchor;

use super::rig::{Rig, modified, removed};
use super::{Rng, ample, differences, full, kinds, lang};
use crate::change::{self, Format, TextEdit};
use crate::highlight::{Budget, EditParse, Outcome, input_edit, query_tree};
use crate::{
    DocHandle, EditSession, EditState, Editable, MergeOutcome, NotEditable, SaveResult, Token,
    WindowSpans, testing,
};

/// The lines of `spans` with their tokens, for comparing.
fn lines(spans: &WindowSpans) -> Vec<(u64, Vec<Token>)> {
    spans.lines().map(|(at, t)| (at, t.to_vec())).collect()
}

/// The lines whose tokens differ, as `(line, got, expected)`.
fn unlike(got: &WindowSpans, expected: &WindowSpans) -> Vec<(u64, Vec<Token>, Vec<Token>)> {
    let (a, b) = (lines(got), lines(expected));
    let mut starts: Vec<u64> = a.iter().chain(&b).map(|(at, _)| *at).collect();
    starts.sort_unstable();
    starts.dedup();
    let of = |l: &[(u64, Vec<Token>)], at: u64| {
        l.iter()
            .find(|(s, _)| *s == at)
            .map(|(_, t)| t.clone())
            .unwrap_or_default()
    };
    starts
        .into_iter()
        .map(|at| (at, of(&a, at), of(&b, at)))
        .filter(|(_, x, y)| x != y)
        .collect()
}

/// A round of `parse` on `text` to its end; with `sliced`, in steps of no
/// budget (one query chunk each).
fn round(
    parse: &mut EditParse,
    text: &str,
    edit: Option<TextEdit>,
    sliced: bool,
) -> Arc<WindowSpans> {
    let cancel = AtomicBool::new(false);
    parse.begin(Arc::from(text), edit);
    loop {
        let budget = if sliced {
            Budget {
                budget: Duration::ZERO,
                cancel: &cancel,
                yield_probe: None,
            }
        } else {
            ample(&cancel)
        };
        match parse.step(budget) {
            Outcome::Done(spans) => return spans,
            Outcome::TimedOut => {}
            other => panic!("round: {other:?}"),
        }
    }
}

const SNIPPETS: &[&str] = &[
    "x",
    " ",
    "\n",
    "/*",
    "*/",
    "\"",
    "'",
    "`",
    "{",
    "}",
    "(",
    ")",
    ";",
    ",",
    "[",
    "]",
    "#",
    "// note\n",
    "let y = 1;\n",
    "fn f() {}\n",
    "\"key\": 2,\n",
    "0x1f",
    "é",
];

/// The char boundary at or after `at`.
fn boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// A random insertion, deletion or replacement in `text`.
fn random_edit(rng: &mut Rng, text: &str) -> (String, TextEdit) {
    let start = boundary(text, rng.below(text.len() as u64 + 1) as usize);
    let snippet = SNIPPETS[rng.below(SNIPPETS.len() as u64) as usize];
    let (end, insert) = match rng.below(3) {
        0 => (start, snippet),
        1 => (boundary(text, start + rng.below(40) as usize), ""),
        _ => (boundary(text, start + rng.below(12) as usize), snippet),
    };
    let next = format!("{}{insert}{}", &text[..start], &text[end..]);
    let edit = TextEdit {
        start,
        old_end: end,
        new_end: start + insert.len(),
    };
    (next, edit)
}

/// Random edits, a round of the incremental parse after each. Its spliced
/// spans are one query over its own tree every time. Unless `sliced`, that
/// tree is the one tree-sitter's own incremental parse of the same edits
/// gives (a parser and a tree kept here). With `sliced`, every other round
/// runs in steps of no budget: a parse halted and resumed can recover from
/// an error differently, so its tree is only checked against its spans. A
/// whole parse of an erroneous text can differ too (error recovery depends
/// on the old tree), so that comparison is only counted;
/// `edit_incremental_spans_match_a_whole_parse` requires it where the edits
/// keep the text valid.
fn incremental(name: &str, source: Vec<u8>, seed: u64, sliced: bool) {
    let lang = lang(name);
    let mut text = String::from_utf8(source).unwrap();
    let mut parse = EditParse::new(lang);
    let first = round(&mut parse, &text, None, false);
    assert_eq!(
        lines(&first),
        lines(&full(lang, text.as_bytes())),
        "{name}: first round"
    );
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(lang.language()).unwrap();
    let mut own = parser.parse(&text, None).unwrap();
    let mut rng = Rng::new(seed);
    let mut whole_equal = 0;
    for n in 0..120 {
        let (next, edit) = random_edit(&mut rng, &text);
        // Now and then no edit, or a wrong one: the texts' difference.
        let given = match n % 9 {
            4 => None,
            7 => Some(TextEdit {
                start: 0,
                old_end: 0,
                new_end: 0,
            }),
            _ => Some(edit),
        };
        let used = match given {
            Some(e) if e == edit => edit,
            _ => change::diff(text.as_bytes(), next.as_bytes()).unwrap_or(TextEdit {
                start: next.len(),
                old_end: next.len(),
                new_end: next.len(),
            }),
        };
        let spans = round(&mut parse, &next, given, sliced && n % 2 == 0);
        assert_eq!(spans.range(), 0..next.len() as u64);
        assert!(spans.exact());
        let mine = query_tree(lang, parse.tree().unwrap(), next.as_bytes());
        assert!(
            lines(&spans) == lines(&mine),
            "{name}: edit {n} {edit:?}: the spliced spans are the tree's: {:?}",
            unlike(&spans, &mine)
        );
        if !sliced {
            own.edit(&input_edit(used, text.as_bytes(), next.as_bytes()));
            own = parser.parse(&next, Some(&own)).unwrap();
            assert_eq!(
                parse.tree().unwrap().root_node().to_sexp(),
                own.root_node().to_sexp(),
                "{name}: edit {n}: tree-sitter's incremental tree"
            );
        }
        if lines(&spans) == lines(&full(lang, next.as_bytes())) {
            whole_equal += 1;
        }
        text = next;
    }
    eprintln!("{name}: {whole_equal} of 120 rounds like a whole parse");
}

#[test]
fn edit_incremental_spans_are_their_trees_ts() {
    incremental("ts", testing::ts(24 << 10, 3), 11, false);
}

#[test]
fn edit_incremental_spans_are_their_trees_rust() {
    incremental("rust", testing::rust(24 << 10, 5), 12, false);
}

#[test]
fn edit_incremental_spans_are_their_trees_json() {
    incremental("json", testing::json(24 << 10, 7), 13, false);
}

#[test]
fn edit_incremental_sliced_spans_are_their_trees() {
    incremental("ts", testing::ts(24 << 10, 3), 11, true);
    incremental("rust", testing::rust(24 << 10, 5), 12, true);
}

/// Edits that keep the text valid (whole statements, identifiers, blank
/// lines): the incremental spans are exactly a whole parse's.
#[test]
fn edit_incremental_spans_match_a_whole_parse() {
    let lang = lang("ts");
    let mut text = String::from_utf8(testing::ts(32 << 10, 1)).unwrap();
    let mut parse = EditParse::new(lang);
    round(&mut parse, &text, None, false);
    let mut rng = Rng::new(21);
    for n in 0..80 {
        let line = change::line_start(text.as_bytes(), rng.below(text.len() as u64) as usize);
        let (end, insert) = match n % 4 {
            0 => (line, "const added = \"x\" + 1; // new\n"),
            1 => (change::line_end(text.as_bytes(), line), ""),
            2 => (line, "\n"),
            _ => (line, "/* a block\n   comment */\n"),
        };
        let next = format!("{}{insert}{}", &text[..line], &text[end..]);
        let edit = TextEdit {
            start: line,
            old_end: end,
            new_end: line + insert.len(),
        };
        let spans = round(&mut parse, &next, Some(edit), n % 3 == 0);
        assert_eq!(
            lines(&spans),
            lines(&full(lang, next.as_bytes())),
            "edit {n} at {line}"
        );
        text = next;
    }
}

#[test]
fn edit_compose_describes_the_edits_in_turn() {
    let mut rng = Rng::new(5);
    for _ in 0..300 {
        let start = "fn main() {\n    let x = 1;\n}\n".to_owned();
        let mut text = start.clone();
        let mut edits = Vec::new();
        for _ in 0..1 + rng.below(5) {
            let (next, edit) = random_edit(&mut rng, &text);
            edits.push(edit);
            text = next;
        }
        let all = change::compose_all(&edits, start.len(), text.len()).expect("they fit");
        assert!(
            change::describes(all, start.as_bytes(), text.as_bytes()),
            "{edits:?} as {all:?}"
        );
    }
    // Edits that do not fit the lengths are not composed.
    let wrong = [TextEdit {
        start: 3,
        old_end: 2,
        new_end: 3,
    }];
    assert_eq!(change::compose_all(&wrong, 10, 9), None);
    assert_eq!(change::compose_all(&[], 10, 10), None);
    assert_eq!(change::diff(b"same", b"same"), None);
}

#[test]
fn edit_format_round_trips() {
    let (text, format) = Format::decode(b"\xEF\xBB\xBFa\r\nb\r\n").unwrap();
    assert_eq!(&*text, "a\nb\n");
    assert_eq!(
        format,
        Format {
            bom: true,
            crlf: true
        }
    );
    assert_eq!(format.encode("a\nb\nc"), b"\xEF\xBB\xBFa\r\nb\r\nc");
    // A `\r\n` pasted into the buffer is not doubled.
    assert_eq!(format.encode("a\r\nb\n"), b"\xEF\xBB\xBFa\r\nb\r\n");
    let (text, format) = Format::decode(b"plain\nlf\n").unwrap();
    assert_eq!((&*text, format.plain()), ("plain\nlf\n", true));
    assert_eq!(format.encode(&text), b"plain\nlf\n");
    assert!(Format::decode(b"\xFF\xFE").is_none());
}

/// A TS file, opened, read and parsed whole: the document and its session
/// after the session's first round.
fn editing(rig: &Rig, rel: &str, bytes: &[u8]) -> (DocHandle, EditSession) {
    rig.write(rel, bytes);
    let (doc, snapshot) = rig.open_settled(rig.target(rel, Anchor::Start));
    assert_eq!(snapshot.editable, Editable::Yes, "{rel}");
    let session = doc.edit().expect("editable");
    rig.settle();
    assert!(session.idle());
    (doc, session)
}

#[test]
fn edit_spans_are_the_readers_until_the_first_round() {
    let rig = Rig::new();
    let source = testing::ts(40 << 10, 2);
    rig.write("a.ts", &source);
    let (doc, snapshot) = rig.open_settled(rig.target("a.ts", Anchor::Start));
    let session = doc.edit().unwrap();
    assert_eq!(session.initial_text().as_bytes(), &source[..]);
    assert!(Arc::ptr_eq(&session.spans(), &snapshot.spans[0]));
    // Another handle is the same session.
    let again = doc.edit().unwrap();
    assert!(Arc::ptr_eq(again.session(), session.session()));
    drop(again);
    rig.settle();
    assert!(session.idle());
    assert_eq!(lines(&session.spans()), lines(&full(lang("ts"), &source)));
}

/// A file with a BOM and `\r\n` line endings: the reader's spans are moved
/// onto the buffer's text before the first parse.
#[test]
fn edit_crlf_and_bom_spans_are_moved_onto_the_buffer() {
    let rig = Rig::new();
    let lf = String::from_utf8(testing::ts(20 << 10, 4)).unwrap();
    let mut raw = b"\xEF\xBB\xBF".to_vec();
    raw.extend_from_slice(lf.replace('\n', "\r\n").as_bytes());
    rig.write("crlf.ts", &raw);
    let (doc, _) = rig.open_settled(rig.target("crlf.ts", Anchor::Start));
    let session = doc.edit().unwrap();
    assert_eq!(&*session.initial_text(), lf.as_str());
    let seen: Arc<std::sync::Mutex<Option<Arc<WindowSpans>>>> = Arc::default();
    let into = seen.clone();
    *session.session().during_round.lock().unwrap() = Some(Box::new(move |s| {
        *into.lock().unwrap() = Some(s.spans());
    }));
    rig.settle();
    let moved = seen.lock().unwrap().clone().expect("the round began");
    let expected = full(lang("ts"), lf.as_bytes());
    let range = 0..lf.len() as u64;
    let got = kinds(&[&moved], range.clone());
    let want = kinds(&[&expected], range.clone());
    assert_eq!(differences(lf.as_bytes(), range, &got, &want), []);
    assert_eq!(lines(&session.spans()), lines(&expected));
}

#[test]
fn edit_changed_moves_the_spans_at_once() {
    let rig = Rig::new();
    let source = String::from_utf8(testing::ts(40 << 10, 6)).unwrap();
    let (_doc, session) = editing(&rig, "a.ts", source.as_bytes());
    let lang = lang("ts");
    // Two lines typed into the middle, given as two edits.
    let at = change::line_start(source.as_bytes(), source.len() / 2);
    let one = "const typed = 42;\n";
    let two = "// and a comment\n";
    let next = format!("{}{one}{two}{}", &source[..at], &source[at..]);
    let edits = [
        TextEdit {
            start: at,
            old_end: at,
            new_end: at + one.len(),
        },
        TextEdit {
            start: at + one.len(),
            old_end: at + one.len(),
            new_end: at + one.len() + two.len(),
        },
    ];
    session.changed(Arc::from(next.as_str()), &edits);
    // Before any task: the lines around the edit are as a whole parse of the
    // new text has them; the typed lines have no tokens yet.
    let moved = session.spans();
    assert_eq!(moved.range(), 0..next.len() as u64);
    let expected = full(lang, next.as_bytes());
    let typed = at as u64..(at + one.len() + two.len()) as u64;
    let outside = |spans: &WindowSpans| -> Vec<(u64, Vec<Token>)> {
        lines(spans)
            .into_iter()
            .filter(|(line, _)| !typed.contains(line))
            .collect()
    };
    assert_eq!(outside(&moved), outside(&expected));
    assert!(moved.line(at as u64).is_empty());
    rig.settle();
    assert_eq!(lines(&session.spans()), lines(&expected));
    // An edit inside a line: the line's tokens before and after it stay.
    let inside = next.find("typed").unwrap() + 2;
    let third = format!("{}XY{}", &next[..inside], &next[inside..]);
    session.changed(
        Arc::from(third.as_str()),
        &[TextEdit {
            start: inside,
            old_end: inside,
            new_end: inside + 2,
        }],
    );
    let line = at as u64;
    let kept = session.spans();
    let tokens = kept.line(line);
    assert!(tokens.iter().any(|t| t.end() as usize <= inside - at));
    assert!(tokens.iter().any(|t| t.start() as usize >= inside - at + 2));
    rig.settle();
    assert_eq!(
        lines(&session.spans()),
        lines(&full(lang, third.as_bytes()))
    );
}

#[test]
fn edit_edits_during_a_round_are_kept() {
    let rig = Rig::new();
    let source = String::from_utf8(testing::rust(30 << 10, 8)).unwrap();
    rig.write("a.rs", source.as_bytes());
    let (doc, _) = rig.open_settled(rig.target("a.rs", Anchor::Start));
    let session = doc.edit().unwrap();
    let at = change::line_start(source.as_bytes(), source.len() / 3);
    let insert = "/* opened\n";
    let next = format!("{}{insert}{}", &source[..at], &source[at..]);
    let during = next.clone();
    // While the first round parses the initial text, the buffer changes.
    *session.session().during_round.lock().unwrap() = Some(Box::new(move |s| {
        let edit = TextEdit {
            start: at,
            old_end: at,
            new_end: at + insert.len(),
        };
        s.changed(Arc::from(during.as_str()), &[edit]);
    }));
    rig.settle();
    assert!(session.idle());
    assert_eq!(
        lines(&session.spans()),
        lines(&full(lang("rust"), next.as_bytes()))
    );
}

#[test]
fn edit_is_refused_with_the_reason() {
    let rig = Rig::new();
    rig.write("a.ts", b"let a = 1;\n");
    let doc = rig.open(rig.target("a.ts", Anchor::Start));
    // Before the full read.
    assert_eq!(doc.edit().err(), Some(NotEditable::NotLoaded));
    rig.settle();
    assert!(doc.edit().is_ok());
    let oid = rig.git.add_blob(b"let b = 2;\n");
    let (object, _) = rig.open_settled(rig.object("a.ts", oid, Anchor::Start));
    assert_eq!(object.edit().err(), Some(NotEditable::ReadOnlySource));
    rig.write("big.ts", &testing::ts(1100 << 10, 0));
    let (big, _) = rig.open_settled(rig.target("big.ts", Anchor::Start));
    assert_eq!(big.edit().err(), Some(NotEditable::TooLarge));
}

#[test]
fn edit_plain_text_is_editable_without_spans() {
    let rig = Rig::new();
    let (_doc, session) = editing(&rig, "notes.txt", b"one\ntwo\n");
    assert_eq!(session.spans().lines().count(), 0);
    session.changed(
        Arc::from("one\n2\ntwo\n"),
        &[TextEdit {
            start: 4,
            old_end: 4,
            new_end: 6,
        }],
    );
    session.save(Arc::from("one\n2\ntwo\n"));
    rig.settle();
    assert_eq!(session.state().save, Some(SaveResult::Saved));
    assert_eq!(
        std::fs::read(rig.path("notes.txt")).unwrap(),
        b"one\n2\ntwo\n"
    );
}

#[test]
fn save_restores_the_bom_and_crlf() {
    let rig = Rig::new();
    let (_doc, session) = editing(&rig, "a.ts", b"\xEF\xBB\xBFlet a = 1;\r\nlet b = 2;\r\n");
    assert_eq!(&*session.initial_text(), "let a = 1;\nlet b = 2;\n");
    session.save(Arc::from("let a = 1;\nlet b = 2;\nlet c = 3;\n"));
    rig.settle();
    let state = session.state();
    assert_eq!(
        (state.saves, state.save.clone()),
        (1, Some(SaveResult::Saved))
    );
    assert_eq!(
        std::fs::read(rig.path("a.ts")).unwrap(),
        b"\xEF\xBB\xBFlet a = 1;\r\nlet b = 2;\r\nlet c = 3;\r\n"
    );
    // The next save expects the identity this one left.
    session.save(Arc::from("let a = 1;\n"));
    rig.settle();
    assert_eq!(session.state().saves, 2);
    assert_eq!(session.state().save, Some(SaveResult::Saved));
    assert_eq!(
        std::fs::read(rig.path("a.ts")).unwrap(),
        b"\xEF\xBB\xBFlet a = 1;\r\n"
    );
}

#[test]
fn save_conflicts_when_the_file_changed_and_save_anyway_writes() {
    let rig = Rig::new();
    let (_doc, session) = editing(&rig, "a.ts", b"let a = 1;\n");
    // The agent writes the file; no event has come yet.
    std::fs::write(rig.path("a.ts"), b"let a = 2; // agent\n").unwrap();
    session.save(Arc::from("let a = 1; // mine\n"));
    rig.settle();
    let state = session.state();
    assert_eq!(state.save, Some(SaveResult::Conflict));
    assert!(state.dirty_on_disk);
    assert_eq!(
        std::fs::read(rig.path("a.ts")).unwrap(),
        b"let a = 2; // agent\n"
    );
    session.save_anyway(Arc::from("let a = 1; // mine\n"));
    rig.settle();
    let state = session.state();
    assert_eq!(
        (state.saves, state.save.clone()),
        (2, Some(SaveResult::Saved))
    );
    assert!(!state.dirty_on_disk);
    assert_eq!(
        std::fs::read(rig.path("a.ts")).unwrap(),
        b"let a = 1; // mine\n"
    );
}

#[test]
fn save_failure_when_the_file_is_gone() {
    let rig = Rig::new();
    let (_doc, session) = editing(&rig, "a.ts", b"let a = 1;\n");
    std::fs::remove_file(rig.path("a.ts")).unwrap();
    session.save_anyway(Arc::from("let a = 3;\n"));
    rig.settle();
    let state = session.state();
    assert!(
        matches!(state.save, Some(SaveResult::Failed(_))),
        "{state:?}"
    );
    assert!(state.unavailable);
    assert!(!rig.path("a.ts").exists(), "a save never creates the file");
}

#[test]
fn save_events_while_editing_mark_the_file_without_reloading() {
    let rig = Rig::new();
    let (doc, session) = editing(&rig, "a.ts", b"let a = 1;\n");
    let before = doc.snapshot();
    // The same bytes written again: another identity, the same content.
    std::fs::write(rig.path("a.ts"), b"let a = 1;\n").unwrap();
    rig.touch("a.ts", modified());
    assert_eq!(*session.state(), EditState::default());
    // Other content.
    std::fs::write(rig.path("a.ts"), b"let a = 9;\n").unwrap();
    rig.touch("a.ts", modified());
    assert!(session.state().dirty_on_disk);
    let after = doc.snapshot();
    assert_eq!(
        after.generation, before.generation,
        "not reloaded under the session"
    );
    assert_eq!(after.editable, Editable::Yes);
    assert!(Arc::ptr_eq(&after, &before));
    // Removed.
    std::fs::remove_file(rig.path("a.ts")).unwrap();
    rig.touch("a.ts", removed());
    assert!(session.state().unavailable);
}

#[test]
fn save_same_content_on_disk_keeps_the_save_clean() {
    let rig = Rig::new();
    let (_doc, session) = editing(&rig, "a.ts", b"let a = 1;\n");
    std::fs::write(rig.path("a.ts"), b"let a = 1;\n").unwrap();
    rig.touch("a.ts", modified());
    session.save(Arc::from("let a = 5;\n"));
    rig.settle();
    assert_eq!(session.state().save, Some(SaveResult::Saved));
}

#[test]
fn edit_ending_after_a_save_reopens_the_document() {
    let rig = Rig::new();
    let (doc, session) = editing(&rig, "a.ts", b"let a = 1;\n");
    session.save(Arc::from("let a = 1;\nlet b = 2;\n"));
    rig.settle();
    drop(session);
    assert_eq!(
        doc.snapshot().editable,
        Editable::No(NotEditable::Reloading),
        "read again before the next edit"
    );
    rig.settle();
    let snapshot = doc.snapshot();
    assert_eq!(snapshot.editable, Editable::Yes);
    assert_eq!(snapshot.size, "let a = 1;\nlet b = 2;\n".len() as u64);
    assert_eq!(rig.engine.tasks_in_flight(), 0);
    // Events reload the document again.
    std::fs::write(rig.path("a.ts"), b"let c = 3;\n").unwrap();
    rig.touch("a.ts", modified());
    assert_eq!(doc.snapshot().size, "let c = 3;\n".len() as u64);
    // A session that saved nothing leaves the document as it is.
    let session = doc.edit().unwrap();
    rig.settle();
    let generation = doc.snapshot().generation;
    drop(session);
    rig.settle();
    assert_eq!(doc.snapshot().generation, generation);
    assert_eq!(doc.snapshot().editable, Editable::Yes);
}

#[test]
fn edit_survives_the_project_closing() {
    let rig = Rig::new();
    let (doc, session) = editing(&rig, "a.ts", b"let a = 1;\n");
    rig.engine.close_project(&rig.project());
    rig.settle();
    assert!(
        doc.is_watched(),
        "the edited document keeps its subscription"
    );
    session.save(Arc::from("let a = 2;\n"));
    rig.settle();
    assert_eq!(session.state().save, Some(SaveResult::Saved));
    assert_eq!(std::fs::read(rig.path("a.ts")).unwrap(), b"let a = 2;\n");
}

/// The merge's text and its conflicts' text, for the snapshots.
fn render(merged: &MergeOutcome) -> String {
    let mut out = merged.text.to_string();
    for (n, range) in merged.conflicts.iter().enumerate() {
        out.push_str(&format!(
            "--- conflict {n}: {range:?}\n{}",
            &merged.text[range.clone()]
        ));
    }
    out
}

/// `base` on disk while editing, `ours` in the buffer, `theirs` written
/// to disk; the merge after the event, then a save of its text.
fn merged(base: &str, ours: &str, theirs: &str) -> MergeOutcome {
    let rig = Rig::new();
    let (_doc, session) = editing(&rig, "a.ts", base.as_bytes());
    std::fs::write(rig.path("a.ts"), theirs).unwrap();
    rig.touch("a.ts", modified());
    assert!(session.state().dirty_on_disk);
    session.merge(Arc::from(ours));
    rig.settle();
    let state = session.state();
    assert_eq!(state.merges, 1);
    assert!(!state.dirty_on_disk);
    let outcome = state.merged.clone().expect("merged");
    // The disk's version is the base now: saving the merge does not
    // conflict.
    session.save(outcome.text.clone());
    rig.settle();
    assert_eq!(session.state().save, Some(SaveResult::Saved));
    assert_eq!(
        std::fs::read(rig.path("a.ts")).unwrap(),
        outcome.text.as_bytes()
    );
    (*outcome).clone()
}

const BASE: &str = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\n";

#[test]
fn merge_without_conflicts() {
    let outcome = merged(
        BASE,
        "line 1\nline 2 (editor)\nline 3\nline 4\nline 5\nline 6\nline 7 (both)\n",
        "line 1\nline 2\nline 3\nline 4\nline 5 (disk)\nline 6\nline 7 (both)\n",
    );
    assert!(outcome.conflicts.is_empty());
    insta::assert_snapshot!(render(&outcome));
}

#[test]
fn merge_with_conflicts() {
    let outcome = merged(
        BASE,
        "line 1 (editor)\nline 2\nline 3 (editor)\nline 4 (editor)\nline 5\nline 6\nline 7\n",
        "line 1\nline 2\nline 3 (disk)\nline 4\nline 5\nline 6\nline 7 (disk)\n",
    );
    assert_eq!(outcome.conflicts.len(), 1);
    insta::assert_snapshot!(render(&outcome));
}

#[test]
fn merge_both_sides_change_the_same_line() {
    let outcome = merged(
        BASE,
        "line 1\nline 2\nline 3\nline four\nline 5\nline 6\nline 7\n",
        "line 1\nline 2\nline 3\nline 4!\nline 5\nline 6\nline 7\n",
    );
    assert_eq!(outcome.conflicts.len(), 1);
    insta::assert_snapshot!(render(&outcome));
}

#[test]
fn merge_of_a_file_that_is_gone() {
    let rig = Rig::new();
    let (_doc, session) = editing(&rig, "a.ts", b"let a = 1;\n");
    std::fs::remove_file(rig.path("a.ts")).unwrap();
    session.merge(Arc::from("let a = 2;\n"));
    rig.settle();
    let state = session.state();
    assert_eq!((state.merges, state.merged.is_none()), (1, true));
    assert!(state.unavailable && state.dirty_on_disk);
}
