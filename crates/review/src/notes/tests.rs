//! The excerpt kept with a note (`notes_excerpt_*`, docs/design/review.md
//! 「审阅意见」 摘录): two lines either side of a note on a line, the lines of
//! a range (#373, `notes_excerpt_range_*`), each cut to 160 bytes, nothing
//! read further than 4 KiB from the first and the last line.

use std::sync::Arc;

use lectern_services::changes::SideText;
use lectern_services::project::BlobId;
use lectern_services::review::{
    DiffRow, DiffSide, FileKey, FileRows, NoteAnchor, RowsKey, RowsSide,
};

use super::{LINE_BYTES, anchored_text, excerpt, hint};

fn lines(text: &[u8], line: u32) -> Option<(u32, Vec<String>)> {
    let e = excerpt(text, line, line, None)?;
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
    let plain = excerpt(&text, 14, 14, None).unwrap();
    assert_eq!(excerpt(&text, 14, 14, Some((12, at))).unwrap(), plain);
    // An offset past the text is not believed.
    assert_eq!(excerpt(&text, 14, 14, Some((12, 1 << 40))).unwrap(), plain);
    // A line after the one asked for is no help.
    assert_eq!(
        excerpt(&text, 3, 3, Some((12, at))).unwrap(),
        excerpt(&text, 3, 3, None).unwrap()
    );
}

/// An excerpt as the tests compare it: the first line, the lines, and where
/// and how many lines are left out.
type Kept = (u32, Vec<String>, Option<(u32, u32)>);

/// The excerpt of the range `line..=last` as `(first, lines, gap)`.
fn range(text: &[u8], line: u32, last: u32) -> Option<Kept> {
    let e = excerpt(text, line, last, None)?;
    let lines = e.lines.iter().map(|l| l.to_string()).collect();
    Some((e.first, lines, e.gap.map(|g| (g.at, g.lines))))
}

/// "line a" to "line e", for the lines `from..=to` of `numbered`.
fn names(from: usize, to: usize) -> Vec<String> {
    (from..=to).map(|i| format!("line {i}")).collect()
}

#[test]
fn notes_excerpt_range_two_to_four_lines_keep_two_either_side() {
    let text = numbered(20);
    for n in [2u32, 4] {
        let (first, got, gap) = range(&text, 8, 8 + n - 1).unwrap();
        assert_eq!((first, gap), (6, None), "{n} lines");
        assert_eq!(got, names(6, 8 + n as usize - 1 + 2), "{n} lines");
    }
    // At the start and the end of the file the context is short.
    let (first, got, _) = range(&text, 0, 2).unwrap();
    assert_eq!((first, got), (0, names(0, 4)));
    let (first, got, _) = range(&text, 17, 19).unwrap();
    assert_eq!((first, got), (15, names(15, 19)));
}

#[test]
fn notes_excerpt_range_five_to_eight_lines_have_no_context() {
    let text = numbered(30);
    for n in [5usize, 8] {
        let (first, got, gap) = range(&text, 10, 10 + n as u32 - 1).unwrap();
        assert_eq!((first, gap), (10, None), "{n} lines");
        assert_eq!(got, names(10, 10 + n - 1), "{n} lines");
    }
}

#[test]
fn notes_excerpt_range_over_eight_lines_keep_four_and_four_with_a_gap() {
    let text = numbered(6000);
    let (first, got, gap) = range(&text, 10, 18).unwrap();
    assert_eq!(first, 10);
    let mut want = names(10, 13);
    want.extend(names(15, 18));
    assert_eq!(got, want, "9 lines: one is left out");
    assert_eq!(gap, Some((4, 1)));
    // The numbers of the lines after the gap: first + index + gap.
    let (first, got, gap) = range(&text, 0, 4999).unwrap();
    let (at, skipped) = gap.unwrap();
    assert_eq!((first, at, skipped), (0, 4, 4992));
    assert_eq!(got.len(), 8);
    assert_eq!(got[3], "line 3");
    assert_eq!(got[4], "line 4996", "first + 4 + 4992 is 4996");
    assert_eq!(got[7], "line 4999");
}

#[test]
fn notes_excerpt_range_to_the_start_and_the_end_of_the_file() {
    let text = numbered(10);
    // A range that reaches the last line: the lines after are not there.
    let (first, got, gap) = range(&text, 7, 9).unwrap();
    assert_eq!((first, gap), (5, None));
    assert_eq!(got, names(5, 9));
    // A range that runs past the end ends at the last line; the gap counts
    // what was found.
    let (first, got, gap) = range(&text, 0, 5000).unwrap();
    assert_eq!((first, gap), (0, Some((4, 2))));
    assert_eq!((got[3].as_str(), got[4].as_str()), ("line 3", "line 6"));
    assert_eq!(got[7], "line 9");
    let (first, got, gap) = range(&text, 8, 5000).unwrap();
    assert_eq!((first, gap), (6, None));
    assert_eq!(got, names(6, 9));
    // Without a newline at the end the last line is still one.
    let (first, got, gap) = range(b"a\nb\nc", 1, 9).unwrap();
    assert_eq!(
        (first, got, gap),
        (0, vec!["a".into(), "b".into(), "c".into()], None)
    );
    // The range starts nowhere: no excerpt.
    assert_eq!(range(&text, 10, 12), None);
}

#[test]
fn notes_excerpt_range_a_line_over_4_kib_in_it_is_cut_not_lost() {
    let long = "z".repeat(10_000);
    // A long line inside the range is cut to 160 bytes and the lines after
    // it are still found: nothing limits the range itself.
    let text = format!("a\nb\n{long}\nd\ne\nf\ng\nh\n");
    let (first, got, gap) = range(text.as_bytes(), 0, 5).unwrap();
    assert_eq!((first, gap), (0, None));
    assert_eq!(got.len(), 6);
    assert_eq!(got[2].len(), LINE_BYTES);
    assert_eq!(got[5], "f");
    // A long last line ends what follows it, as for a note on one line.
    let text = format!("a\n{long}\nnever\n");
    let (_, got, _) = range(text.as_bytes(), 0, 1).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].len(), LINE_BYTES);
    // The context before is looked for within 4 KiB of the first line.
    let text = format!("{long}\nnear\nfirst\nsecond\n");
    let (first, got, _) = range(text.as_bytes(), 2, 3).unwrap();
    assert_eq!(
        (first, got),
        (1, vec!["near".into(), "first".into(), "second".into()])
    );
}

#[test]
fn notes_excerpt_range_of_one_line_is_the_excerpt_of_the_line() {
    let text = numbered(10);
    for line in 0..10 {
        assert_eq!(
            excerpt(&text, line, line, None),
            excerpt(&text, line, line.saturating_sub(3), None),
            "a last before the line is the line"
        );
        let e = excerpt(&text, line, line, None).unwrap();
        assert_eq!(e.gap, None);
    }
}

#[test]
fn notes_excerpt_range_starts_from_a_byte_offset_the_rows_know() {
    let text = numbered(40);
    let at: u64 = (0..12).map(|i| format!("line {i}\n").len() as u64).sum();
    assert_eq!(
        excerpt(&text, 14, 30, Some((12, at))),
        excerpt(&text, 14, 30, None)
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
        last: 0,
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
