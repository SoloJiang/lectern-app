//! The prompt of a batch (`prompt_*`, docs/design/review.md「prompt 的模板」):
//! the example of the design as text, and the cases around it as insta
//! snapshots.

use std::sync::Arc;

use lectern_services::changes::ChangePath;
use lectern_services::review::{BatchId, Excerpt, ExcerptGap};

use super::{PromptNote, TEMPLATE_VERSION, Where, marker, prompt};

const BATCH: BatchId = BatchId(0x0190_f3a2_c4b6_0401);

fn note(
    path: &[u8],
    line: u32,
    place: Where,
    excerpt: Option<(u32, &[&str])>,
    text: &str,
) -> PromptNote {
    PromptNote {
        path: ChangePath(Arc::from(path)),
        line,
        last: line,
        place,
        excerpt: excerpt.map(|(first, lines)| Excerpt {
            first,
            lines: lines.iter().map(|l| Arc::<str>::from(*l)).collect(),
            gap: None,
        }),
        text: Arc::from(text),
    }
}

/// The two notes of the design's example.
fn example() -> Vec<PromptNote> {
    vec![
        note(
            b"src/engine/mod.rs",
            41,
            Where::Current,
            Some((
                39,
                &[
                    "    let rows = prepare(&key);",
                    "    let hunks = rows.hunks();",
                    "    let first = hunks[0];",
                    "    emit(first);",
                    "}",
                ],
            )),
            "This panics when there are no hunks.",
        ),
        note(
            b"src/log.rs",
            16,
            Where::Earlier,
            Some((
                14,
                &[
                    "fn append(&mut self) {",
                    "    self.flush();",
                    "    self.sync()?;",
                    "}",
                ],
            )),
            "Keep the sync here.",
        ),
    ]
}

#[test]
fn prompt_the_design_example() {
    let text = prompt(BATCH, &example());
    let expected = "\
Review notes from Lectern [lectern-notes 0190f3a2c4b60401]: 2 notes on your changes.
Please address each note.

1. src/engine/mod.rs:42
   ```
     40 |     let rows = prepare(&key);
     41 |     let hunks = rows.hunks();
   > 42 |     let first = hunks[0];
     43 |     emit(first);
     44 | }
   ```
   Note:
   This panics when there are no hunks.

2. src/log.rs (line 17 when this note was written; the code may have changed since)
   ```
     15 | fn append(&mut self) {
     16 |     self.flush();
   > 17 |     self.sync()?;
     18 | }
   ```
   Note:
   Keep the sync here.";
    assert_eq!(text, expected);
    insta::assert_snapshot!("prompt_the_design_example", text);
}

#[test]
fn prompt_the_marker_is_within_the_first_60_characters() {
    assert_eq!(TEMPLATE_VERSION, 2);
    let text = prompt(BatchId(0x1), &example());
    let first = text.lines().next().unwrap();
    let end = first.find(']').unwrap();
    assert!(end < 60, "{first}");
    assert!(first.contains(&marker(BatchId(0x1))));
    assert_eq!(marker(BatchId(0x1)), "[lectern-notes 0000000000000001]");
}

#[test]
fn prompt_one_note_and_the_places() {
    let one = prompt(
        BATCH,
        &[note(b"a.rs", 0, Where::Base, None, "Why was this removed?")],
    );
    assert!(one.starts_with(
        "Review notes from Lectern [lectern-notes 0190f3a2c4b60401]: 1 note on your changes.\n"
    ));
    insta::assert_snapshot!("prompt_one_note_base", one);
    let current = prompt(BATCH, &[note(b"a.rs", 9, Where::Current, None, "x")]);
    assert!(
        current.ends_with("\n\n1. a.rs:10\n   Note:\n   x"),
        "{current}"
    );
}

#[test]
fn prompt_no_excerpt_has_no_fence() {
    let text = prompt(
        BATCH,
        &[
            note(b"a.rs", 3, Where::Current, None, "First"),
            note(b"b.rs", 4, Where::Earlier, None, "Second"),
        ],
    );
    assert!(!text.contains("```"), "{text}");
    insta::assert_snapshot!("prompt_no_excerpt", text);
}

#[test]
fn prompt_the_fence_is_longer_than_any_run_of_backticks() {
    let text = prompt(
        BATCH,
        &[
            note(
                b"doc.md",
                1,
                Where::Current,
                Some((0, &["# Title", "```rust", "``` ````", "tail"])),
                "Fence.",
            ),
            note(
                b"a.rs",
                0,
                Where::Current,
                Some((0, &["let s = \"``\";"])),
                "Two backticks keep the fence at three.",
            ),
        ],
    );
    insta::assert_snapshot!("prompt_fence", text);
}

#[test]
fn prompt_line_numbers_are_right_aligned_to_one_width() {
    let text = prompt(
        BATCH,
        &[note(
            b"long.rs",
            98,
            Where::Current,
            Some((96, &["a", "b", "c", "d", "e"])),
            "Width.",
        )],
    );
    insta::assert_snapshot!("prompt_line_width", text);
}

#[test]
fn prompt_the_note_text_is_kept_as_written() {
    let text = prompt(
        BATCH,
        &[
            note(
                b"a.rs",
                0,
                Where::Current,
                None,
                "First line.\n\n  Indented after a blank line.\r\nCRLF line.\n\n",
            ),
            note(
                b"b.rs",
                0,
                Where::Current,
                None,
                "1. Not a list item of the prompt\n- nor this",
            ),
        ],
    );
    assert!(!text.contains('\r'));
    assert!(!text.ends_with('\n'), "{text:?}");
    insta::assert_snapshot!("prompt_text", text);
}

#[test]
fn prompt_a_path_that_is_not_utf8_is_lossy() {
    let text = prompt(
        BATCH,
        &[note(b"src/caf\xe9.rs", 0, Where::Current, None, "Name.")],
    );
    assert!(text.contains("1. src/caf\u{fffd}.rs:1"), "{text}");
}

#[test]
fn prompt_the_anchor_line_outside_the_excerpt_has_no_marker() {
    let text = prompt(
        BATCH,
        &[note(
            b"a.rs",
            50,
            Where::Earlier,
            Some((0, &["x", "y"])),
            "Moved.",
        )],
    );
    assert!(!text.contains("> "), "{text}");
    assert!(text.contains("1 | x"), "{text}");
}

// ---- prompt_range_* (#373) ------------------------------------------------

/// An excerpt as the tests write it: the first line, the lines, and where
/// and how many lines are left out.
type Shown<'a> = (u32, &'a [&'a str], Option<(u32, u32)>);

/// A note on `line..=last` (0-based) with an excerpt.
fn ranged(
    path: &[u8],
    (line, last): (u32, u32),
    place: Where,
    excerpt: Option<Shown<'_>>,
    text: &str,
) -> PromptNote {
    PromptNote {
        last,
        excerpt: excerpt.map(|(first, lines, gap)| Excerpt {
            first,
            lines: lines.iter().map(|l| Arc::<str>::from(*l)).collect(),
            gap: gap.map(|(at, lines)| ExcerptGap { at, lines }),
        }),
        ..note(path, line, place, None, text)
    }
}

/// The range of the design: lines 42 to 51, the middle two left out.
fn long_range() -> PromptNote {
    ranged(
        b"src/engine/mod.rs",
        (41, 50),
        Where::Current,
        Some((
            41,
            &[
                "fn prepare(key: &Key) -> Rows {",
                "    let rows = load(key);",
                "    let hunks = rows.hunks();",
                "    check(&hunks);",
                "    emit(&hunks);",
                "    flush();",
                "    close(key);",
                "}",
            ],
            Some((4, 2)),
        )),
        "Split this function.",
    )
}

#[test]
fn prompt_range_the_design_example() {
    let text = prompt(BATCH, &[long_range()]);
    let expected = "\
Review notes from Lectern [lectern-notes 0190f3a2c4b60401]: 1 note on your changes.
Please address each note.

1. src/engine/mod.rs:42-51
   ```
   > 42 | fn prepare(key: &Key) -> Rows {
   > 43 |     let rows = load(key);
   > 44 |     let hunks = rows.hunks();
   > 45 |     check(&hunks);
   > ... 2 more lines ...
   > 48 |     emit(&hunks);
   > 49 |     flush();
   > 50 |     close(key);
   > 51 | }
   ```
   Note:
   Split this function.";
    assert_eq!(text, expected);
    insta::assert_snapshot!("prompt_range_the_design_example", text);
}

#[test]
fn prompt_range_the_three_places() {
    let text = prompt(
        BATCH,
        &[
            ranged(b"a.rs", (9, 11), Where::Current, None, "Current."),
            ranged(b"b.rs", (0, 4), Where::Base, None, "Base."),
            ranged(b"c.rs", (99, 100), Where::Earlier, None, "Earlier."),
        ],
    );
    assert!(text.contains("\n1. a.rs:10-12\n"), "{text}");
    assert!(
        text.contains("\n2. b.rs (lines 1-5 of the version before your changes)\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "\n3. c.rs (lines 100-101 when this note was written; the code may have changed since)\n"
        ),
        "{text}"
    );
    insta::assert_snapshot!("prompt_range_places", text);
}

#[test]
fn prompt_range_every_line_of_the_range_is_marked() {
    // Three lines with two lines of context either side.
    let text = prompt(
        BATCH,
        &[ranged(
            b"a.rs",
            (4, 6),
            Where::Current,
            Some((2, &["c2", "c3", "r4", "r5", "r6", "a7", "a8"], None)),
            "Three.",
        )],
    );
    let marked: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("   > "))
        .map(|l| l.trim_start_matches("   > "))
        .collect();
    assert_eq!(marked, ["5 | r4", "6 | r5", "7 | r6"], "{text}");
    assert!(text.contains("\n     3 | c2\n"), "{text}");
    assert!(text.contains("\n     8 | a7\n"), "{text}");
    insta::assert_snapshot!("prompt_range_context", text);
}

#[test]
fn prompt_range_the_gap_keeps_the_width_of_the_largest_number() {
    // 9990 to 10009 (1-based) with the middle left out: the numbers after
    // the gap have five digits, so all are aligned to five.
    let text = prompt(
        BATCH,
        &[ranged(
            b"big.rs",
            (9989, 10008),
            Where::Current,
            Some((
                9989,
                &["a", "b", "c", "d", "w", "x", "y", "z"],
                Some((4, 12)),
            )),
            "Wide.",
        )],
    );
    assert!(text.contains("1. big.rs:9990-10009\n"), "{text}");
    assert!(text.contains("\n   >  9990 | a\n"), "{text}");
    assert!(text.contains("\n   > ... 12 more lines ...\n"), "{text}");
    assert!(text.contains("\n   > 10006 | w\n"), "{text}");
    assert!(text.contains("\n   > 10009 | z\n"), "{text}");
}

#[test]
fn prompt_range_a_note_on_one_line_is_written_as_before() {
    let lines: &[&str] = &["a", "b", "c", "d", "e"];
    let one = note(b"a.rs", 9, Where::Current, Some((7, lines)), "x");
    let same = PromptNote {
        last: 9,
        ..one.clone()
    };
    // A `last` before `line` (not made by the engine) is the line too.
    let before = PromptNote {
        last: 3,
        ..one.clone()
    };
    let text = prompt(BATCH, std::slice::from_ref(&one));
    assert_eq!(text, prompt(BATCH, &[same]));
    assert_eq!(text, prompt(BATCH, &[before]));
    assert!(text.contains("1. a.rs:10\n"), "{text}");
    assert!(!text.contains("more lines"), "{text}");
    assert_eq!(text.matches("\n   > ").count(), 1, "{text}");
}

#[test]
fn prompt_range_a_gap_line_is_inside_the_fence_and_longer_fences_hold() {
    let text = prompt(
        BATCH,
        &[ranged(
            b"doc.md",
            (0, 19),
            Where::Current,
            Some((
                0,
                &["a", "```", "c", "d", "w", "x", "y", "z"],
                Some((4, 12)),
            )),
            "Fence.",
        )],
    );
    assert!(text.contains("\n   ````\n   >  1 | a\n"), "{text}");
    assert!(text.ends_with("\n   Note:\n   Fence."), "{text}");
    let gap = text.find("... 12 more lines ...").unwrap();
    let close = text.rfind("   ````").unwrap();
    assert!(gap < close, "{text}");
}

#[test]
fn prompt_range_forty_full_notes_fit_in_256_kib() {
    let text = "t".repeat(4096);
    let lines: Vec<String> = (0..8).map(|_| "x".repeat(160)).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let path = vec![b'p'; 200];
    let notes: Vec<PromptNote> = (0..40u32)
        .map(|i| {
            ranged(
                &path,
                (100 + i, 119 + i),
                Where::Earlier,
                Some((100 + i, &refs, Some((4, 12)))),
                &text,
            )
        })
        .collect();
    let size = prompt(BATCH, &notes).len();
    assert!(size <= 256 * 1024, "{size}");
    assert!(size > 40 * 4096, "{size}");
}
