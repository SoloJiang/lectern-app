//! The prompt of a batch (`prompt_*`, docs/design/review.md「prompt 的模板」):
//! the example of the design as text, and the cases around it as insta
//! snapshots.

use std::sync::Arc;

use lectern_services::changes::ChangePath;
use lectern_services::review::{BatchId, Excerpt};

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
        place,
        excerpt: excerpt.map(|(first, lines)| Excerpt {
            first,
            lines: lines.iter().map(|l| Arc::<str>::from(*l)).collect(),
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
    assert_eq!(TEMPLATE_VERSION, 1);
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
