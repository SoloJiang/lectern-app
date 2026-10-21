//! `merge3_*`: the expected bytes are written out by hand; the tests don't
//! run git. For the LF inputs with a conflict, a join of conflicts or a
//! missing final line ending, they are what `git merge-file -p -L editor
//! -L base -L disk ours base theirs` (git 2.43, default level and style)
//! printed for the same input. The CRLF cases follow the marker rule in
//! docs/design/git.md「三方合并」. The snapshot shows the result with its
//! conflict ranges.

use std::fmt::Write as _;

use super::*;

const MARKERS: Markers<'static> = Markers {
    ours: "editor",
    theirs: "disk",
};

/// Merges, checks the bytes against `expected` and the conflict ranges
/// against the markers in them, and renders the result for the snapshot.
fn check(base: &str, ours: &str, theirs: &str, expected: &str) -> String {
    let m = merge3(
        base.as_bytes(),
        ours.as_bytes(),
        theirs.as_bytes(),
        &MARKERS,
    );
    assert_eq!(
        String::from_utf8_lossy(&m.bytes),
        expected,
        "base {base:?}, ours {ours:?}, theirs {theirs:?}"
    );
    assert_ranges(&m);
    assert_eq!(m.is_clean(), m.conflicts.is_empty());
    render(&m)
}

/// Each range covers one whole conflict and nothing else holds a marker.
fn assert_ranges(m: &Merge3) {
    let mut at = 0;
    for r in &m.conflicts {
        assert!(r.start >= at, "{:?}", m.conflicts);
        let outside = &m.bytes[at..r.start];
        assert!(
            !has_marker(outside),
            "marker outside a conflict: {outside:?}"
        );
        let inside = &m.bytes[r.clone()];
        assert!(inside.starts_with(b"<<<<<<< editor"), "{inside:?}");
        assert!(
            inside.ends_with(b">>>>>>> disk\n") || inside.ends_with(b">>>>>>> disk\r\n"),
            "{inside:?}"
        );
        assert!(r.start == 0 || m.bytes[r.start - 1] == b'\n');
        at = r.end;
    }
    assert!(!has_marker(&m.bytes[at..]));
}

fn has_marker(bytes: &[u8]) -> bool {
    bytes.split(|b| *b == b'\n').any(|l| {
        l.starts_with(b"<<<<<<<") || l.starts_with(b"=======") || l.starts_with(b">>>>>>>")
    })
}

/// The conflict ranges, then the lines with `\r` shown as `␍`, a `|` at
/// each line's end and git's note when the last line has no `\n`.
fn render(m: &Merge3) -> String {
    let mut out = format!("conflicts: {:?}\n", m.conflicts);
    let text = String::from_utf8_lossy(&m.bytes).replace('\r', "␍");
    for line in text.split_inclusive('\n') {
        match line.strip_suffix('\n') {
            Some(line) => writeln!(out, "{line}|").unwrap(),
            None => writeln!(out, "{line}\n\\ No newline at end of file").unwrap(),
        }
    }
    out
}

/// The same merge with ours and theirs swapped is the mirror image: the
/// clean results are equal, the conflicts swap their sides.
fn swapped(base: &str, ours: &str, theirs: &str) -> Merge3 {
    merge3(
        base.as_bytes(),
        theirs.as_bytes(),
        ours.as_bytes(),
        &MARKERS,
    )
}

#[test]
fn merge3_no_change() {
    let base = "a\nb\nc\n";
    insta::assert_snapshot!(check(base, base, base, base), @r"
    conflicts: []
    a|
    b|
    c|
    ");
}

#[test]
fn merge3_only_ours_changed() {
    let base = "a\nb\nc\nd\n";
    let ours = "a\nB\nc\nd\ne\n";
    insta::assert_snapshot!(check(base, ours, base, ours), @r"
    conflicts: []
    a|
    B|
    c|
    d|
    e|
    ");
}

#[test]
fn merge3_only_theirs_changed() {
    let base = "a\nb\nc\nd\n";
    let theirs = "a\nc\nD\n";
    insta::assert_snapshot!(check(base, base, theirs, theirs), @r"
    conflicts: []
    a|
    c|
    D|
    ");
}

#[test]
fn merge3_same_change_on_both_sides() {
    // The same edit of `b` on both sides, and theirs also appends a line.
    let base = "a\nb\nc\n";
    let ours = "a\nB\nc\n";
    let theirs = "a\nB\nc\nd\n";
    insta::assert_snapshot!(check(base, ours, theirs, "a\nB\nc\nd\n"), @r"
    conflicts: []
    a|
    B|
    c|
    d|
    ");
    assert_eq!(swapped(base, ours, theirs).bytes, b"a\nB\nc\nd\n");
}

#[test]
fn merge3_non_overlapping_changes() {
    let base = "1\n2\n3\n4\n5\n6\n";
    let ours = "one\n2\n3\n4\n5\n6\n";
    let theirs = "1\n2\n3\nfour\n5\n";
    let merged = "one\n2\n3\nfour\n5\n";
    insta::assert_snapshot!(check(base, ours, theirs, merged), @r"
    conflicts: []
    one|
    2|
    3|
    four|
    5|
    ");
    assert_eq!(swapped(base, ours, theirs).bytes, merged.as_bytes());
}

#[test]
fn merge3_overlapping_changes_conflict() {
    let base = "a\nb\nc\nd\n";
    let ours = "a\nours\nc\nd\n";
    let theirs = "a\ntheirs\nc\nd\n";
    let expected = "a\n<<<<<<< editor\nours\n=======\ntheirs\n>>>>>>> disk\nc\nd\n";
    insta::assert_snapshot!(check(base, ours, theirs, expected), @r"
    conflicts: [2..50]
    a|
    <<<<<<< editor|
    ours|
    =======|
    theirs|
    >>>>>>> disk|
    c|
    d|
    ");
}

#[test]
fn merge3_conflict_keeps_only_the_lines_that_differ() {
    // Both sides rewrote the same three lines; only the middle one differs.
    let (base, ours, theirs) = ("a\nb\nc\n", "x\nb\ny\n", "x\nB\ny\n");
    let expected = "x\n<<<<<<< editor\nb\n=======\nB\n>>>>>>> disk\ny\n";
    insta::assert_snapshot!(check(base, ours, theirs, expected), @r"
    conflicts: [2..42]
    x|
    <<<<<<< editor|
    b|
    =======|
    B|
    >>>>>>> disk|
    y|
    ");
}

#[test]
fn merge3_adjacent_changes_conflict() {
    // Changes to neighbouring lines touch in the base: git reports a conflict.
    let (base, ours, theirs) = ("a\nb\nc\n", "A\nb\nc\n", "a\nB\nc\n");
    let expected = "<<<<<<< editor\nA\nb\n=======\na\nB\n>>>>>>> disk\nc\n";
    insta::assert_snapshot!(check(base, ours, theirs, expected), @r"
    conflicts: [0..44]
    <<<<<<< editor|
    A|
    b|
    =======|
    a|
    B|
    >>>>>>> disk|
    c|
    ");
}

#[test]
fn merge3_joins_conflicts_three_lines_apart() {
    // Conflicts at 1 and 5 with three lines between are one conflict; the
    // change at 7 only ours made is taken.
    let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
    let ours = "o1\n2\n3\n4\no5\n6\no7\n8\n9\n";
    let theirs = "t1\n2\n3\n4\nt5\n6\n7\n8\n9\n";
    let expected =
        "<<<<<<< editor\no1\n2\n3\n4\no5\n=======\nt1\n2\n3\n4\nt5\n>>>>>>> disk\n6\no7\n8\n9\n";
    insta::assert_snapshot!(check(base, ours, theirs, expected), @r"
    conflicts: [0..60]
    <<<<<<< editor|
    o1|
    2|
    3|
    4|
    o5|
    =======|
    t1|
    2|
    3|
    4|
    t5|
    >>>>>>> disk|
    6|
    o7|
    8|
    9|
    ");
}

#[test]
fn merge3_joins_conflicts_apart_by_lines_without_letters() {
    // Four lines with letters keep 1 and 6 apart; four lines of `}` don't
    // keep 6 and 11 apart.
    let base = "1\n2\n3\n4\n5\n6\n}\n}\n}\n}\n11\n";
    let ours = "o1\n2\n3\n4\n5\no6\n}\n}\n}\n}\no11\n";
    let theirs = "t1\n2\n3\n4\n5\nt6\n}\n}\n}\n}\nt11\n";
    let expected = "<<<<<<< editor\no1\n=======\nt1\n>>>>>>> disk\n2\n3\n4\n5\n\
                    <<<<<<< editor\no6\n}\n}\n}\n}\no11\n=======\nt6\n}\n}\n}\n}\nt11\n>>>>>>> disk\n";
    insta::assert_snapshot!(check(base, ours, theirs, expected), @r"
    conflicts: [0..42, 50..116]
    <<<<<<< editor|
    o1|
    =======|
    t1|
    >>>>>>> disk|
    2|
    3|
    4|
    5|
    <<<<<<< editor|
    o6|
    }|
    }|
    }|
    }|
    o11|
    =======|
    t6|
    }|
    }|
    }|
    }|
    t11|
    >>>>>>> disk|
    ");
}

#[test]
fn merge3_taken_change_keeps_conflicts_apart() {
    // Only ours changes 3, between two conflicts one line from it each.
    let base = "1\n2\n3\n4\n5\n";
    let ours = "o1\n2\nthree\n4\no5\n";
    let theirs = "t1\n2\n3\n4\nt5\n";
    let expected = "<<<<<<< editor\no1\n=======\nt1\n>>>>>>> disk\n2\nthree\n4\n\
                    <<<<<<< editor\no5\n=======\nt5\n>>>>>>> disk\n";
    insta::assert_snapshot!(check(base, ours, theirs, expected), @r"
    conflicts: [0..42, 52..94]
    <<<<<<< editor|
    o1|
    =======|
    t1|
    >>>>>>> disk|
    2|
    three|
    4|
    <<<<<<< editor|
    o5|
    =======|
    t5|
    >>>>>>> disk|
    ");
}

#[test]
fn merge3_insert_at_start_and_end() {
    let base = "a\nb\n";
    let (ours, theirs) = ("start\na\nb\n", "a\nb\nend\n");
    insta::assert_snapshot!(check(base, ours, theirs, "start\na\nb\nend\n"), @r"
    conflicts: []
    start|
    a|
    b|
    end|
    ");
    assert_eq!(swapped(base, ours, theirs).bytes, b"start\na\nb\nend\n");
    // Two different insertions at the same point conflict, at either end
    // (four lines apart, so they stay two conflicts).
    let base = "a\nb\nc\nd\n";
    let (ours, theirs) = ("o\na\nb\nc\nd\no\n", "t\na\nb\nc\nd\nt\n");
    let expected = "<<<<<<< editor\no\n=======\nt\n>>>>>>> disk\na\nb\nc\nd\n\
                    <<<<<<< editor\no\n=======\nt\n>>>>>>> disk\n";
    insta::assert_snapshot!(check(base, ours, theirs, expected), @r"
    conflicts: [0..40, 48..88]
    <<<<<<< editor|
    o|
    =======|
    t|
    >>>>>>> disk|
    a|
    b|
    c|
    d|
    <<<<<<< editor|
    o|
    =======|
    t|
    >>>>>>> disk|
    ");
    // Two lines apart, they are one conflict.
    let expected = "<<<<<<< editor\no\na\nb\no\n=======\nt\na\nb\nt\n>>>>>>> disk\n";
    check("a\nb\n", "o\na\nb\no\n", "t\na\nb\nt\n", expected);
}

#[test]
fn merge3_deletes() {
    let base = "a\nb\nc\nd\ne\n";
    // Separate deletions on the two sides.
    insta::assert_snapshot!(check(base, "b\nc\nd\ne\n", "a\nb\nc\ne\n", "b\nc\ne\n"), @r"
    conflicts: []
    b|
    c|
    e|
    ");
    // The same deletion on both sides.
    assert_eq!(
        check(base, "a\nd\ne\n", "a\nd\ne\nf\n", "a\nd\ne\nf\n"),
        "conflicts: []\na|\nd|\ne|\nf|\n"
    );
    // Ours deletes `b` and `c`, theirs edits `b` (a conflict with an empty
    // side) and deletes `e` (taken).
    let expected = "a\n<<<<<<< editor\n=======\nB\nc\n>>>>>>> disk\nd\n";
    insta::assert_snapshot!(check(base, "a\nd\ne\n", "a\nB\nc\nd\n", expected), @r"
    conflicts: [2..42]
    a|
    <<<<<<< editor|
    =======|
    B|
    c|
    >>>>>>> disk|
    d|
    ");
}

#[test]
fn merge3_crlf() {
    // Lines keep their `\r`; a clean merge of CRLF files stays CRLF.
    let base = "a\r\nb\r\nc\r\nd\r\n";
    let ours = "A\r\nb\r\nc\r\nd\r\n";
    let theirs = "a\r\nb\r\nc\r\nD\r\n";
    insta::assert_snapshot!(check(base, ours, theirs, "A\r\nb\r\nc\r\nD\r\n"), @r"
    conflicts: []
    A␍|
    b␍|
    c␍|
    D␍|
    ");
    // A conflict in CRLF files has CRLF markers.
    let expected = "a\r\n<<<<<<< editor\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> disk\r\nc\r\nd\r\n";
    insta::assert_snapshot!(check(base, "a\r\nours\r\nc\r\nd\r\n", "a\r\ntheirs\r\nc\r\nd\r\n", expected), @r"
    conflicts: [3..56]
    a␍|
    <<<<<<< editor␍|
    ours␍|
    =======␍|
    theirs␍|
    >>>>>>> disk␍|
    c␍|
    d␍|
    ");
    // A side that turns a line's `\r\n` into `\n` changed the line.
    let expected = "a\r\n<<<<<<< editor\r\nb\n=======\r\nB\r\n>>>>>>> disk\r\nc\r\nd\r\n";
    check(base, "a\r\nb\nc\r\nd\r\n", "a\r\nB\r\nc\r\nd\r\n", expected);
    // LF files keep LF markers.
    let expected = "a\n<<<<<<< editor\nb\r\n=======\nB\n>>>>>>> disk\n";
    check("a\nb\n", "a\nb\r\n", "a\nB\n", expected);
    // A CRLF conflict at the end of files without a final line ending:
    // each side's last line gets `\r\n` before the next marker.
    let expected = "a\r\n<<<<<<< editor\r\nB\r\n=======\r\nC\r\n>>>>>>> disk\r\n";
    insta::assert_snapshot!(check("a\r\nb", "a\r\nB", "a\r\nC", expected), @r"
    conflicts: [3..48]
    a␍|
    <<<<<<< editor␍|
    B␍|
    =======␍|
    C␍|
    >>>>>>> disk␍|
    ");
    // Both sides CRLF on an empty base: CRLF markers (see git.md「三方合并」).
    let expected = "<<<<<<< editor\r\no\r\n=======\r\nt\r\n>>>>>>> disk\r\n";
    check("", "o\r\n", "t\r\n", expected);
}

#[test]
fn merge3_no_final_newline() {
    // The conflict is the last line, which has no `\n` on either side: each
    // side gets one before the next marker, so the result ends with one.
    let expected = "a\n<<<<<<< editor\nB\n=======\nC\n>>>>>>> disk\n";
    insta::assert_snapshot!(check("a\nb", "a\nB", "a\nC", expected), @r"
    conflicts: [2..42]
    a|
    <<<<<<< editor|
    B|
    =======|
    C|
    >>>>>>> disk|
    ");
    // The line without `\n` differs from the same text with one: theirs
    // adding the final line ending is a change of the last line, kept
    // apart from ours' change of the first.
    let base = "a\nb\nc";
    insta::assert_snapshot!(check(base, "A\nb\nc", "a\nb\nc\n", "A\nb\nc\n"), @r"
    conflicts: []
    A|
    b|
    c|
    ");
    // A clean merge keeps a missing final line ending.
    insta::assert_snapshot!(check(base, "A\nb\nc", "a\nb\nC", "A\nb\nC"), @r"
    conflicts: []
    A|
    b|
    C
    \ No newline at end of file
    ");
    // Ours adds a line after the last one, theirs edits it: both change
    // the last line, a conflict.
    let expected = "a\nb\n<<<<<<< editor\nc\nd\n=======\nC\n>>>>>>> disk\n";
    check(base, "a\nb\nc\nd", "a\nb\nC", expected);
}

#[test]
fn merge3_empty_files() {
    insta::assert_snapshot!(check("", "", "", ""), @"conflicts: []");
    // One side adds to an empty file, or empties it.
    assert_eq!(check("", "", "new\n", "new\n"), "conflicts: []\nnew|\n");
    assert_eq!(check("a\nb\n", "", "a\nb\n", ""), "conflicts: []\n");
    // Both add to an empty base: their common first line is kept.
    let expected = "same\n<<<<<<< editor\no\n=======\nt\n>>>>>>> disk\n";
    insta::assert_snapshot!(check("", "same\no\n", "same\nt\n", expected), @r"
    conflicts: [5..45]
    same|
    <<<<<<< editor|
    o|
    =======|
    t|
    >>>>>>> disk|
    ");
    // Ours empties the file, theirs edits it.
    let expected = "<<<<<<< editor\n=======\na\nB\n>>>>>>> disk\n";
    check("a\nb\n", "", "a\nB\n", expected);
}

#[test]
fn merge3_marker_labels() {
    let m = merge3(
        b"a\n",
        b"b\n",
        b"c\n",
        &Markers {
            ours: "",
            theirs: "other side",
        },
    );
    assert_eq!(m.bytes, b"<<<<<<<\nb\n=======\nc\n>>>>>>> other side\n");
    assert_eq!(m.conflicts.len(), 1);
    assert_eq!(m.conflicts[0], 0..m.bytes.len());
}

#[test]
fn merge3_swapped_sides_mirror_the_conflict() {
    let (base, ours, theirs) = ("a\nb\nc\n", "a\nB\nc\n", "a\nX\nc\n");
    let m = swapped(base, ours, theirs);
    assert_eq!(
        m.bytes,
        b"a\n<<<<<<< editor\nX\n=======\nB\n>>>>>>> disk\nc\n"
    );
}
