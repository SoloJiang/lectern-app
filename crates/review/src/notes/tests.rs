//! The excerpt kept with a note (`notes_excerpt_*`, docs/design/review.md
//! 「审阅意见」 摘录): two lines either side of the anchor, each cut to 160
//! bytes, nothing read further than 4 KiB from the anchor line.

use std::sync::Arc;

use lectern_services::changes::SideText;
use lectern_services::project::BlobId;
use lectern_services::review::{
    DiffRow, DiffSide, FileKey, FileRows, NoteAnchor, RowsKey, RowsSide,
};

use super::{LINE_BYTES, anchored_text, excerpt, hint};

fn lines(text: &[u8], line: u32) -> Option<(u32, Vec<String>)> {
    let e = excerpt(text, line, None)?;
    Some((e.first, e.lines.iter().map(|l| l.to_string()).collect()))
}

fn numbered(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| format!("line {i}\n"))
        .collect::<String>()
        .into_bytes()
}

#[test]
fn notes_excerpt_two_lines_either_side() {
    let text = numbered(10);
    let (first, got) = lines(&text, 5).unwrap();
    assert_eq!(first, 3);
    assert_eq!(got, ["line 3", "line 4", "line 5", "line 6", "line 7"]);
}

#[test]
fn notes_excerpt_at_the_start_and_the_end_of_the_file() {
    let text = numbered(10);
    assert_eq!(
        lines(&text, 0).unwrap(),
        (0, vec!["line 0".into(), "line 1".into(), "line 2".into()])
    );
    assert_eq!(lines(&text, 1).unwrap().0, 0);
    assert_eq!(lines(&text, 1).unwrap().1.len(), 4);
    let (first, got) = lines(&text, 9).unwrap();
    assert_eq!((first, got.len()), (7, 3));
    assert_eq!(got[2], "line 9");
    // A last line without a newline is a line.
    let (first, got) = lines(b"a\nb\nc", 2).unwrap();
    assert_eq!((first, got), (0, vec!["a".into(), "b".into(), "c".into()]));
    // A single line, and none.
    assert_eq!(lines(b"only", 0).unwrap(), (0, vec!["only".into()]));
    assert_eq!(lines(b"", 0), None);
    assert_eq!(lines(&text, 10), None, "the line after the last is not one");
    assert_eq!(lines(&text, 50), None);
}

#[test]
fn notes_excerpt_ends_of_lines_are_not_text() {
    let (_, got) = lines(b"one\r\ntwo\r\nthree\r\n", 1).unwrap();
    assert_eq!(got, ["one", "two", "three"]);
    // An empty line is a line.
    let (_, got) = lines(b"a\n\n\nb\n", 2).unwrap();
    assert_eq!(got, ["a", "", "", "b"]);
}

#[test]
fn notes_excerpt_lines_are_cut_to_160_bytes_on_a_character_boundary() {
    let ascii = "x".repeat(300);
    let (_, got) = lines(format!("{ascii}\n").as_bytes(), 0).unwrap();
    assert_eq!(got[0].len(), LINE_BYTES);
    // Two-byte characters: 80 fit exactly.
    let two = "é".repeat(100);
    let (_, got) = lines(format!("{two}\n").as_bytes(), 0).unwrap();
    assert_eq!(got[0], "é".repeat(80));
    // Three-byte characters: the 54th would end at byte 162.
    let three = "字".repeat(100);
    let (_, got) = lines(format!("{three}\n").as_bytes(), 0).unwrap();
    assert_eq!(got[0], "字".repeat(53));
    assert_eq!(got[0].len(), 159);
    // Four-byte ones; a line of exactly 160 bytes is kept whole.
    let four = "🙂".repeat(100);
    let (_, got) = lines(format!("{four}\n").as_bytes(), 0).unwrap();
    assert_eq!(got[0], "🙂".repeat(40));
    let exact = "y".repeat(LINE_BYTES);
    let (_, got) = lines(format!("{exact}\n").as_bytes(), 0).unwrap();
    assert_eq!(got[0], exact);
}

#[test]
fn notes_excerpt_a_line_over_4_kib_is_cut_and_ends_what_follows() {
    let long = "z".repeat(10_000);
    let text = format!("before\n{long}\nafter\n");
    // The anchor line alone is long: its end is out of reach, so the lines
    // after it are not found; the one before it is.
    let (first, got) = lines(text.as_bytes(), 1).unwrap();
    assert_eq!(first, 0);
    assert_eq!(got.len(), 2);
    assert_eq!((got[0].as_str(), got[1].len()), ("before", LINE_BYTES));
    // A long line before the anchor line: its start is out of reach.
    let text = format!("{long}\nnear\ntarget\n");
    let (first, got) = lines(text.as_bytes(), 2).unwrap();
    assert_eq!((first, got), (1, vec!["near".into(), "target".into()]));
    // A long line after the anchor line is cut, and nothing follows it.
    let text = format!("target\n{long}\nnever\n");
    let (_, got) = lines(text.as_bytes(), 0).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].len(), LINE_BYTES);
}

#[test]
fn notes_excerpt_looks_no_further_than_4_kib() {
    // Short lines after the anchor line, the third one starts past 4 KiB.
    let filler = "f".repeat(4000);
    let text = format!("anchor\n{filler}\nsecond\nthird\n");
    let (_, got) = lines(text.as_bytes(), 0).unwrap();
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[2], "second");
    let filler = "f".repeat(5000);
    let text = format!("anchor\n{filler}\nsecond\n");
    assert_eq!(lines(text.as_bytes(), 0).unwrap().1.len(), 2);
    // The same before the anchor line.
    let filler = "f".repeat(4090);
    let text = format!("first\n{filler}\nprev\nanchor\n");
    let (first, got) = lines(text.as_bytes(), 3).unwrap();
    assert_eq!((first, got.last().unwrap().as_str()), (2, "anchor"));
    assert_eq!(got.len(), 2, "{got:?}");
}

#[test]
fn notes_excerpt_bytes_that_are_not_utf8_become_replacement_characters() {
    let (_, got) = lines(b"ok\ncaf\xe9 \xff\xfe\nend\n", 1).unwrap();
    assert_eq!(got, ["ok", "caf\u{fffd} \u{fffd}\u{fffd}", "end"]);
    // A character cut in two by the limit is dropped with the rest.
    let mut line = vec![b'a'; LINE_BYTES - 1];
    line.extend_from_slice("é".as_bytes());
    line.push(b'\n');
    let (_, got) = lines(&line, 0).unwrap();
    assert_eq!(got[0].len(), LINE_BYTES - 1);
}

#[test]
fn notes_excerpt_starts_from_a_byte_offset_the_rows_know() {
    let text = numbered(20);
    // Line 12 starts at the sum of the widths of lines 0 to 11.
    let at: u64 = (0..12).map(|i| format!("line {i}\n").len() as u64).sum();
    let plain = excerpt(&text, 14, None).unwrap();
    assert_eq!(excerpt(&text, 14, Some((12, at))).unwrap(), plain);
    // An offset past the text is not believed.
    assert_eq!(excerpt(&text, 14, Some((12, 1 << 40))).unwrap(), plain);
    // A line after the one asked for is no help.
    assert_eq!(
        excerpt(&text, 3, Some((12, at))).unwrap(),
        excerpt(&text, 3, None).unwrap()
    );
}

fn rows_of(text: &[u8], rows: Vec<DiffRow>, oid: BlobId) -> FileRows {
    let side = RowsSide {
        oid: Some(oid),
        text: SideText::Text(Arc::from(text)),
    };
    FileRows {
        key: RowsKey::Final {
            file: FileKey {
                old: None,
                new: None,
            },
            context: 3,
        },
        old: side.clone(),
        new: side,
        rows: rows.into(),
        hunks: Arc::from(Vec::new()),
        inline: Arc::from(Vec::new()),
    }
}

#[test]
fn notes_excerpt_hint_is_the_nearest_known_line_at_or_before() {
    let oid = BlobId::Sha1([1; 20]);
    let rows = rows_of(
        b"",
        vec![
            DiffRow::Context {
                old: 0,
                new: 0,
                old_byte: Some(0),
                new_byte: Some(0),
                hunk: None,
            },
            DiffRow::Folded {
                old: 1..40,
                new: 1..40,
                old_byte: Some(10),
                new_byte: Some(12),
            },
            DiffRow::Removed {
                old: 40,
                pair: None,
                old_byte: None,
                hunk: 0,
            },
            DiffRow::Added {
                new: 41,
                pair: None,
                new_byte: Some(500),
                hunk: 0,
            },
        ],
        oid,
    );
    assert_eq!(hint(&rows, DiffSide::New, 41), Some((41, 500)));
    assert_eq!(
        hint(&rows, DiffSide::New, 40),
        Some((1, 12)),
        "the fold's start"
    );
    assert_eq!(
        hint(&rows, DiffSide::Old, 40),
        Some((1, 10)),
        "no offset for the row"
    );
    assert_eq!(hint(&rows, DiffSide::Old, 0), Some((0, 0)));
}

#[test]
fn notes_excerpt_text_is_the_anchored_side_only() {
    let oid = BlobId::Sha1([1; 20]);
    let rows = rows_of(b"text\n", Vec::new(), oid);
    let anchor = |side, oid| NoteAnchor {
        file: FileKey {
            old: None,
            new: None,
        },
        side,
        line: 0,
        oid,
    };
    assert_eq!(
        anchored_text(&rows, &anchor(DiffSide::Old, oid)),
        Some(&b"text\n"[..])
    );
    assert_eq!(
        anchored_text(&rows, &anchor(DiffSide::New, oid)),
        Some(&b"text\n"[..])
    );
    // Another content, and a side without text.
    assert_eq!(
        anchored_text(&rows, &anchor(DiffSide::New, BlobId::Sha1([2; 20]))),
        None
    );
    let mut gone = rows;
    gone.new.text = SideText::NotRetained;
    assert_eq!(anchored_text(&gone, &anchor(DiffSide::New, oid)), None);
    assert!(anchored_text(&gone, &anchor(DiffSide::Old, oid)).is_some());
}
