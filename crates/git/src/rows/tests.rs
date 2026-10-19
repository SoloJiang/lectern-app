use std::fmt::Write as _;
use std::sync::Arc;

use super::*;
use crate::{Text, hunks};

fn text(s: &str) -> Text {
    Text::new(Arc::from(s.as_bytes()))
}

fn numbered(n: u32) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

/// Diffs two texts and renders their rows as a unified view with both line
/// numbers (1-based) and each change's pair.
fn view(old: &str, new: &str, context: u32) -> String {
    let diff = hunks::line_diff(old.as_bytes(), new.as_bytes()).diff;
    let texts = FileTexts {
        old: SideText::Text(text(old)),
        new: SideText::Text(text(new)),
    };
    render(&rows(&diff, &texts, context), &texts)
}

/// Each row with its line numbers (1-based), pair and byte offsets (`@old`,
/// `@new`; `?` when the side has no text).
fn render(rows: &[Row], texts: &FileTexts) -> String {
    let line = |side: &SideText, n: u32| match side {
        SideText::Text(t) => String::from_utf8_lossy(t.line(n)).into_owned(),
        _ => "?".to_owned(),
    };
    let pair = |p: Option<u32>| p.map_or(String::new(), |p| format!("  (pair {})", p + 1));
    let at = |b: &Option<u64>| b.map_or("?".to_owned(), |b| b.to_string());
    let mut out = String::new();
    for row in rows {
        match row {
            Row::Context {
                old,
                new,
                old_byte,
                new_byte,
            } => writeln!(
                out,
                "  {:>3} {:>3}  @{:<4} @{:<4} {}",
                old + 1,
                new + 1,
                at(old_byte),
                at(new_byte),
                line(&texts.old, *old)
            ),
            Row::Removed {
                old,
                pair: p,
                old_byte,
            } => writeln!(
                out,
                "- {:>3}      @{:<4}       {}{}",
                old + 1,
                at(old_byte),
                line(&texts.old, *old),
                pair(*p)
            ),
            Row::Added {
                new,
                pair: p,
                new_byte,
            } => writeln!(
                out,
                "+     {:>3}        @{:<4} {}{}",
                new + 1,
                at(new_byte),
                line(&texts.new, *new),
                pair(*p)
            ),
            Row::Folded {
                old,
                new,
                old_byte,
                new_byte,
            } => writeln!(
                out,
                "~ folded: old {}..{} @{}, new {}..{} @{}",
                old.start + 1,
                old.end,
                at(old_byte),
                new.start + 1,
                new.end,
                at(new_byte)
            ),
        }
        .unwrap();
    }
    out
}

/// Checks the byte offsets of a diff's hunks and rows against the two
/// texts, for every context width the views use and one that shows every
/// line: each offset is the start of its line (offset 0 or just after a
/// `\n`), and the bytes from it are exactly the row's line, then `\n` or the
/// end. A fold's offset is its first line's. A word-level range of a pair is
/// the same bytes at the row's offset. Returns the number of rows checked.
pub(crate) fn assert_offsets(diff: &FileDiff, texts: &FileTexts, what: &str) -> usize {
    fn side(texts: &FileTexts, old: bool) -> Option<&Text> {
        match if old { &texts.old } else { &texts.new } {
            SideText::Text(t) => Some(t),
            _ => None,
        }
    }
    let check = |old: bool, line: u32, byte: Option<u64>, row: &Row| {
        let Some(text) = side(texts, old) else {
            let absent = matches!(if old { &texts.old } else { &texts.new }, SideText::Absent);
            assert_eq!(byte, absent.then_some(0), "{what}: {row:?}");
            return;
        };
        let at = byte.unwrap_or_else(|| panic!("{what}: no offset in {row:?}")) as usize;
        let bytes = text.bytes();
        let want = text.line(line);
        assert!(
            at == 0 || bytes[at - 1] == b'\n',
            "{what}: {row:?} not at a line start"
        );
        assert!(
            bytes[at..].starts_with(want),
            "{what}: {row:?} doesn't start its line"
        );
        let after = &bytes[at + want.len()..];
        assert!(
            after.is_empty() || after[0] == b'\n',
            "{what}: {row:?} ends mid-line"
        );
        assert_eq!(at as u64, text.line_start(line), "{what}: {row:?}");
    };
    for hunk in diff.hunks.iter() {
        for (old, line, byte) in [
            (true, hunk.old.start, hunk.old_byte),
            (false, hunk.new.start, hunk.new_byte),
        ] {
            if let Some(text) = side(texts, old) {
                assert_eq!(byte, text.line_start(line), "{what}: {hunk:?}");
            }
        }
    }
    let mut checked = 0;
    for context in [0, 3, u32::MAX / 4] {
        let rows = rows(diff, texts, context);
        for row in &rows {
            match row {
                Row::Context {
                    old,
                    new,
                    old_byte,
                    new_byte,
                } => {
                    check(true, *old, *old_byte, row);
                    check(false, *new, *new_byte, row);
                }
                Row::Removed { old, old_byte, .. } => check(true, *old, *old_byte, row),
                Row::Added { new, new_byte, .. } => check(false, *new, *new_byte, row),
                Row::Folded {
                    old,
                    new,
                    old_byte,
                    new_byte,
                } => {
                    check(true, old.start, *old_byte, row);
                    check(false, new.start, *new_byte, row);
                }
            }
            checked += 1;
        }
        // Word-level ranges, located through the rows' offsets.
        let (Some(o), Some(n)) = (side(texts, true), side(texts, false)) else {
            continue;
        };
        for row in &rows {
            let Row::Removed {
                old,
                pair: Some(new),
                old_byte: Some(ob),
            } = row
            else {
                continue;
            };
            let nb = n.line_start(*new);
            let (ol, nl) = (o.line(*old), n.line(*new));
            let Some(changes) = inline_changes(ol, nl) else {
                continue;
            };
            for r in &changes.old {
                let at = *ob as usize;
                assert_eq!(
                    &o.bytes()[at + r.start as usize..at + r.end as usize],
                    &ol[r.start as usize..r.end as usize],
                    "{what}: {row:?}"
                );
            }
            for r in &changes.new {
                let at = nb as usize;
                assert_eq!(
                    &n.bytes()[at + r.start as usize..at + r.end as usize],
                    &nl[r.start as usize..r.end as usize],
                    "{what}: {row:?}"
                );
            }
        }
    }
    checked
}

#[test]
fn rows_pair_removed_and_added_lines_and_fold_the_rest() {
    let old = numbered(20);
    let new = old
        .replace("line 9\nline 10\n", "line nine\n")
        .replace("line 11\n", "line 11\nline 11b\nline 11c\n");
    insta::assert_snapshot!(view(&old, &new, 3));
}

#[test]
fn rows_merge_the_context_of_nearby_hunks() {
    let old = numbered(30);
    // 6 unchanged lines between the changes: one stretch of context.
    let near = old
        .replace("line 5\n", "five\n")
        .replace("line 12\n", "twelve\n");
    // 7 unchanged lines: context, a fold of one line, context.
    let far = old
        .replace("line 5\n", "five\n")
        .replace("line 13\n", "thirteen\n");
    insta::assert_snapshot!("rows_near", view(&old, &near, 3));
    insta::assert_snapshot!("rows_far", view(&old, &far, 3));
}

#[test]
fn rows_changes_at_the_start_and_the_end() {
    let old = numbered(10);
    let new = old
        .replace("line 1\n", "first\n")
        .replace("line 10\n", "last");
    insta::assert_snapshot!(view(&old, &new, 2));
}

#[test]
fn rows_without_context_fold_every_unchanged_line() {
    let old = numbered(6);
    let new = old.replace("line 3\n", "three\n");
    insta::assert_snapshot!(view(&old, &new, 0));
}

#[test]
fn rows_of_added_and_deleted_files_have_no_pairs() {
    let added = hunks::whole(b"", b"a\nb\n").diff;
    let texts = FileTexts {
        old: SideText::Absent,
        new: SideText::Text(text("a\nb\n")),
    };
    assert_eq!(
        rows(&added, &texts, 3),
        [
            Row::Added {
                new: 0,
                pair: None,
                new_byte: Some(0)
            },
            Row::Added {
                new: 1,
                pair: None,
                new_byte: Some(2)
            }
        ]
    );
    let deleted = hunks::whole(b"a\n", b"").diff;
    let texts = FileTexts {
        old: SideText::Text(text("a\n")),
        new: SideText::Absent,
    };
    assert_eq!(
        rows(&deleted, &texts, 3),
        [Row::Removed {
            old: 0,
            pair: None,
            old_byte: Some(0)
        }]
    );
}

#[test]
fn rows_without_hunks_are_one_fold() {
    let texts = FileTexts {
        old: SideText::Text(text("a\nb\n")),
        new: SideText::Text(text("a\nb\n")),
    };
    assert_eq!(
        rows(&crate::FileDiff::empty(), &texts, 3),
        [Row::Folded {
            old: 0..2,
            new: 0..2,
            old_byte: Some(0),
            new_byte: Some(0)
        }]
    );
    let empty = FileTexts {
        old: SideText::Text(text("")),
        new: SideText::Text(text("")),
    };
    assert_eq!(rows(&crate::FileDiff::empty(), &empty, 3), []);
}

#[test]
fn rows_infer_the_length_of_a_side_without_text() {
    let old = numbered(12);
    let new = old.replace("line 2\n", "two\n");
    let diff = hunks::line_diff(old.as_bytes(), new.as_bytes()).diff;
    let texts = FileTexts {
        old: SideText::Text(text(&old)),
        new: SideText::NotRetained,
    };
    let rows = rows(&diff, &texts, 3);
    // The side without text has no offsets; the other side keeps them.
    assert_eq!(
        rows.last(),
        Some(&Row::Folded {
            old: 5..12,
            new: 5..12,
            old_byte: Some(35),
            new_byte: None
        })
    );
    assert!(rows.iter().all(|r| match r {
        Row::Context { new_byte, .. } | Row::Folded { new_byte, .. } => new_byte.is_none(),
        Row::Added { new_byte, .. } => new_byte.is_none(),
        Row::Removed { old_byte, .. } => old_byte.is_some(),
    }));
}

#[test]
fn rows_text_lines_and_missing_newline() {
    let t = text("a\r\nb\nc");
    assert_eq!(t.line_count(), 3);
    assert_eq!(t.line(0), b"a\r");
    assert_eq!(t.line(2), b"c");
    assert_eq!(t.line(3), b"");
    assert!(!t.ends_with_newline());
    assert_eq!(text("").line_count(), 0);
    assert_eq!(text("\n").line_count(), 1);
    assert!(text("x\n").ends_with_newline());
    // Line starts count the `\r` of CRLF lines; past the end is the length.
    let starts: Vec<u64> = (0..5).map(|n| t.line_start(n)).collect();
    assert_eq!(starts, [0, 3, 5, 6, 6]);
    assert_eq!(text("").line_start(0), 0);
}

#[test]
fn rows_offsets_with_crlf_and_a_missing_final_newline() {
    // CRLF on both sides, a changed last line without `\n`, and a pair
    // whose word-level ranges are located through the offsets.
    let old = "alpha\r\nbeta\r\ngamma\r\ndelta\r\nend";
    let new = "alpha\r\nbeta\r\nGAMMA ray\r\ndelta\r\nend\r\n";
    insta::assert_snapshot!(view(old, new, 1));
    let diff = hunks::line_diff(old.as_bytes(), new.as_bytes()).diff;
    let texts = FileTexts {
        old: SideText::Text(text(old)),
        new: SideText::Text(text(new)),
    };
    assert!(assert_offsets(&diff, &texts, "crlf") > 0);
}

#[test]
fn rows_offsets_of_every_row_start_its_line() {
    let old = numbered(30);
    let cases = [
        old.replace("line 5\n", "five\n")
            .replace("line 13\n", "thirteen\n"),
        old.replace("line 1\n", "").replace("line 30\n", "line 30"),
        old.replace("line 9\nline 10\n", "line nine\n") + "extra\nmore",
        String::new(),
        old.replace('\n', "\r\n"),
    ];
    for (i, new) in cases.iter().enumerate() {
        for (a, b) in [(&old, new), (new, &old)] {
            let diff = hunks::line_diff(a.as_bytes(), b.as_bytes()).diff;
            let texts = FileTexts {
                old: SideText::Text(text(a)),
                new: SideText::Text(text(b)),
            };
            assert!(assert_offsets(&diff, &texts, &format!("case {i}")) > 0);
        }
    }
    // A new and a deleted file: the absent side is at 0.
    let diff = hunks::whole(b"", b"a\r\nb").diff;
    let texts = FileTexts {
        old: SideText::Absent,
        new: SideText::Text(text("a\r\nb")),
    };
    assert_eq!(assert_offsets(&diff, &texts, "added"), 6);
}

// ---- inline_* -------------------------------------------------------------

/// The line with changed parts in brackets.
fn marked(line: &str, ranges: &[Range<u32>]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for r in ranges {
        out.push_str(&line[at..r.start as usize]);
        write!(out, "[{}]", &line[r.start as usize..r.end as usize]).unwrap();
        at = r.end as usize;
    }
    out.push_str(&line[at..]);
    out
}

fn inline(old: &str, new: &str) -> String {
    let changes = inline_changes(old.as_bytes(), new.as_bytes()).unwrap();
    format!(
        "-{}\n+{}",
        marked(old, &changes.old),
        marked(new, &changes.new)
    )
}

#[test]
fn inline_marks_the_changed_words() {
    insta::assert_snapshot!(
        "inline_expression",
        inline(
            "    let total = price * count;",
            "    let total = price * quantity + tax;"
        )
    );
    insta::assert_snapshot!(
        "inline_signature",
        inline(
            "fn open(path: &Path) -> io::Result<File>",
            "pub fn open(path: &RepoPath) -> Result<File, GitError>"
        )
    );
}

#[test]
fn inline_compares_cjk_text_by_character() {
    insta::assert_snapshot!(inline(
        "界面英文优先，支持简体中文",
        "界面中文优先，支持繁体中文"
    ));
}

#[test]
fn inline_identical_and_one_sided_lines() {
    assert_eq!(
        inline_changes(b"same", b"same"),
        Some(InlineChanges::default())
    );
    let c = inline_changes(b"", b"new text").unwrap();
    assert_eq!(c.old, []);
    assert_eq!(c.new.len(), 1);
    assert_eq!(c.new[0], 0..8);
}

#[test]
fn inline_lines_over_the_limit_get_none() {
    let at = "x".repeat(INLINE_MAX_LINE_BYTES);
    let over = "x".repeat(INLINE_MAX_LINE_BYTES + 1);
    assert!(inline_changes(at.as_bytes(), b"y").is_some());
    assert_eq!(inline_changes(over.as_bytes(), b"y"), None);
    assert_eq!(inline_changes(b"y", over.as_bytes()), None);
}

#[test]
fn inline_invalid_utf8_stays_in_bounds() {
    let c = inline_changes(b"a \xff\xe4\xb8 b", b"a \xe4\xb8\xad b").unwrap();
    for r in c.old.iter().chain(&c.new) {
        assert!(r.start < r.end && r.end <= 8);
    }
}
