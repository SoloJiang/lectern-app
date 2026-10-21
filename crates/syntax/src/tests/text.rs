//! `text_*`: the line index against naive scans, the streaming build
//! against the whole one, long lines' cell marks, CRLF, BOM, and rows that
//! only touch the visible bytes.

use std::sync::Arc;

use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{ProjectKey, RelPath};
use lectern_services::reader::{Anchor, ReadTarget};

use super::Rng;
use crate::cells;
use crate::doc::Pieces;
use crate::doc::content::{Content, PageCache, Paged};
use crate::highlight::Source;
use crate::limits::{LONG_LINE, PAGE};
use crate::text::{IndexBuilder, Text};
use crate::{
    DocSnapshot, DocState, Editable, Highlighting, LineCount, LineNo, NotEditable, Row, RowPos,
};

/// Mixed content: short and empty lines, tabs, CJK, CRLF, long lines with
/// CJK and tabs, now and then an invalid byte.
fn mixed(seed: u64, lines: usize) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    for i in 0..lines {
        match rng.below(10) {
            0 => {}
            1 => out.extend_from_slice(b"\tindented\tline"),
            2 => out.extend_from_slice("代码审阅 review 文件".as_bytes()),
            3 => {
                let n = 4200 + rng.below(9000) as usize;
                let start = out.len();
                while out.len() - start < n {
                    match rng.below(5) {
                        0 => out.extend_from_slice("宽字".as_bytes()),
                        1 => out.push(b'\t'),
                        _ => out.extend_from_slice(b"a=1;"),
                    }
                }
            }
            4 => out.extend_from_slice(b"crlf line\r"),
            5 => out.extend_from_slice(&[b'b', 0xff, b'c']),
            _ => out.extend_from_slice(format!("let v{i} = {i};").as_bytes()),
        }
        out.push(b'\n');
    }
    out
}

fn naive_starts(bytes: &[u8]) -> Vec<u64> {
    std::iter::once(0)
        .chain(memchr::memchr_iter(b'\n', bytes).map(|i| i as u64 + 1))
        .collect()
}

/// The units of `bytes` with their start cells, and the total.
fn naive_cells(bytes: &[u8]) -> (Vec<(usize, u64)>, u64) {
    let (mut at, mut cell, mut units) = (0, 0, Vec::new());
    while at < bytes.len() {
        let (u, n) = cells::unit(&bytes[at..]);
        units.push((at, cell));
        cell += cells::width(u, cell);
        at += n;
    }
    (units, cell)
}

/// A line's shown bytes: without its `\n` and a `\r` before it.
fn shown(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[test]
fn text_line_index_matches_naive_scans() {
    let bytes = mixed(7, 3000);
    let text = Text::from_bytes(bytes.clone());
    let index = text.index();
    let starts = naive_starts(&bytes);
    assert_eq!(index.line_count(), starts.len() as u64);
    for (line, &start) in starts.iter().enumerate() {
        assert_eq!(index.line_start(&text, line as u64), start, "line {line}");
    }
    let mut rng = Rng::new(3);
    for _ in 0..2000 {
        let byte = rng.below(bytes.len() as u64 + 1);
        let line = starts.partition_point(|&s| s <= byte) - 1;
        assert_eq!(index.line_of(&text, byte), line as u64, "byte {byte}");
    }
    // Widest line and long lines.
    let mut widest = 0;
    let mut longs = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(bytes.len() as u64, |&n| n);
        let line = shown(&bytes[start as usize..end as usize]);
        let (_, cells) = naive_cells(line);
        widest = widest.max(cells);
        if line.len() as u64 > LONG_LINE {
            longs.push((start, start + line.len() as u64, cells));
        }
    }
    assert_eq!(u64::from(index.widest_cells()), widest);
    let got: Vec<_> = index
        .long_lines()
        .iter()
        .map(|l| (l.start, l.end, l.cells))
        .collect();
    assert_eq!(got, longs);
    assert!(index.has_long_line());
}

#[test]
fn text_streaming_build_matches_one_pass() {
    let bytes = mixed(11, 2500);
    let whole = {
        let mut b = IndexBuilder::new();
        b.feed(&bytes);
        b.finish()
    };
    let mut rng = Rng::new(5);
    for round in 0..20 {
        let mut b = IndexBuilder::new();
        let mut at = 0;
        while at < bytes.len() {
            // Pieces of 1–3 bytes cut UTF-8 sequences; larger ones lines.
            let n = if round % 2 == 0 {
                1 + rng.below(3) as usize
            } else {
                1 + rng.below(70_000) as usize
            };
            let end = (at + n).min(bytes.len());
            b.feed(&bytes[at..end]);
            at = end;
        }
        let streamed = b.finish();
        assert_eq!(streamed.index, whole.index, "round {round}");
        assert_eq!(streamed.checkpoints, whole.checkpoints, "round {round}");
    }
}

#[test]
fn text_long_line_rows_match_a_naive_fold() {
    let bytes = mixed(13, 400);
    let text = Text::from_bytes(bytes.clone());
    let src: &dyn Source = &text;
    assert!(text.index().long_lines().len() > 10);
    for long in text.index().long_lines() {
        let line = &bytes[long.start as usize..long.end as usize];
        let (units, total) = naive_cells(line);
        assert_eq!(long.cells, total);
        for cols in [7u32, 80, 121] {
            let c = u64::from(cols);
            assert_eq!(long.rows(cols), total.div_ceil(c).max(1));
            for k in 0..long.rows(cols) {
                // Row k: the units whose start cell is in [k·cols, (k+1)·cols).
                let first = units
                    .iter()
                    .find(|u| u.1 >= k * c)
                    .map_or(line.len(), |u| u.0);
                let last = units
                    .iter()
                    .find(|u| u.1 >= (k + 1) * c)
                    .map_or(line.len(), |u| u.0);
                let want = long.start + first as u64..long.start + last as u64;
                assert_eq!(
                    long.row_range(src, k, cols),
                    Some(want.clone()),
                    "row {k} at {cols}"
                );
                if want.start < want.end {
                    assert_eq!(long.row_of(src, want.start, cols), Some(k));
                }
            }
        }
    }
}

#[test]
fn text_facts_crlf_bom_and_encoding() {
    let crlf = Text::from_bytes(b"\xEF\xBB\xBFa\r\nb\r\n".to_vec());
    let facts = crlf.index().facts();
    assert!(facts.bom && facts.is_text() && !facts.mixed_line_endings());
    assert_eq!((facts.crlf, facts.lf), (2, 0));
    // The BOM takes no cell; the `\r` is not shown.
    assert_eq!(crlf.index().widest_cells(), 1);
    let mixed = Text::from_bytes(b"a\r\nb\nc".to_vec());
    assert!(mixed.index().facts().mixed_line_endings());
    assert!(!Text::from_bytes(b"a\0b".to_vec()).index().facts().is_text());
    assert!(
        !Text::from_bytes(b"a\xffb".to_vec())
            .index()
            .facts()
            .is_text()
    );
    let empty = Text::from_bytes(Vec::new());
    assert_eq!(empty.index().line_count(), 1);
    assert_eq!(empty.index().line_start(&empty, 0), 0);
}

pub(crate) fn snapshot(content: Content, size: u64, lines: LineCount) -> DocSnapshot {
    DocSnapshot {
        generation: 1,
        target: ReadTarget {
            project: ProjectKey::Folder {
                root: std::path::Path::new("/").into(),
            },
            worktree: None,
            path: RelPath::from("a.txt"),
            origin: TextOrigin::Worktree,
            expect: None,
            at: Anchor::Start,
        },
        state: DocState::Ready,
        size,
        oid: None,
        tier: None,
        highlight: Highlighting::Plain,
        lines,
        widest_cells: 0,
        editable: Editable::No(NotEditable::NotLoaded),
        anchor: RowPos::default(),
        find: None,
        content,
        spans: Arc::new([]),
    }
}

fn row_texts(rows: &[Row<'_>]) -> Vec<String> {
    rows.iter()
        .map(|r| match r {
            Row::Text { text, .. } => text.to_string(),
            Row::Pending { .. } => "<pending>".into(),
        })
        .collect()
}

#[test]
fn text_rows_show_crlf_bom_controls_and_invalid_bytes() {
    let bytes = b"\xEF\xBB\xBFfirst\r\n\x01ctl\x7f\n\xffx\n".to_vec();
    let size = bytes.len() as u64;
    let text = Arc::new(Text::from_bytes(bytes));
    let s = snapshot(Content::Text(text), size, LineCount::Exact(4));
    let rows = s.rows(RowPos::default(), 10, 80);
    assert_eq!(
        row_texts(&rows),
        ["first", "\u{2401}ctl\u{2421}", "\u{fffd}x", ""]
    );
    let lines: Vec<_> = rows
        .iter()
        .map(|r| match r {
            Row::Text { line, .. } => *line,
            Row::Pending { .. } => LineNo::Estimated(0),
        })
        .collect();
    assert_eq!(
        lines,
        [
            LineNo::Exact(0),
            LineNo::Exact(1),
            LineNo::Exact(2),
            LineNo::Exact(3)
        ]
    );
    // Back and forth.
    let third = s.advance(RowPos::default(), 2, 80);
    assert_eq!(third.line_start, 16);
    assert_eq!(s.advance(third, -2, 80), RowPos::default());
    assert_eq!(s.total_rows(80), 4);
}

#[test]
fn text_rows_fold_long_lines_and_walk_back() {
    let mut bytes = b"short\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'a', 10_000));
    bytes.extend_from_slice(b"\nend\n");
    let size = bytes.len() as u64;
    let text = Arc::new(Text::from_bytes(bytes));
    let s = snapshot(Content::Text(text), size, LineCount::Exact(4));
    // 10,000 cells at 100 columns: 100 rows.
    assert_eq!(s.total_rows(100), 1 + 100 + 1 + 1);
    let last = s.advance(RowPos::default(), 100, 100);
    assert_eq!(
        last,
        RowPos {
            line_start: 6,
            sub_row: 99
        }
    );
    let rows = s.rows(last, 3, 100);
    assert_eq!(
        row_texts(&rows),
        ["a".repeat(100), "end".into(), String::new()]
    );
    assert_eq!(s.advance(last, -100, 100), RowPos::default());
    assert_eq!(
        s.pos_of_byte(6 + 5050, 100),
        RowPos {
            line_start: 6,
            sub_row: 50
        }
    );
}

/// A 64 MiB virtual file with only the pages around one position in
/// memory: the rows there come out whole, so they touched nothing else.
#[test]
fn text_rows_touch_only_the_visible_pages() {
    const SIZE: u64 = 64 << 20;
    let line = b"0123456789 abcdefghij klmnopqrstuvwxyz;\n";
    let byte_at = |at: u64| line[(at % line.len() as u64) as usize];
    let piece = |from: u64, to: u64| -> Vec<u8> { (from..to).map(byte_at).collect() };
    let mut builder = IndexBuilder::new();
    let mut at = 0;
    while at < SIZE {
        let end = (at + (1 << 20)).min(SIZE);
        builder.feed(&piece(at, end));
        at = end;
    }
    let index = Arc::new(builder.finish().index);
    let middle = 40 << 20;
    let mut cache = PageCache::new(SIZE);
    cache.add(
        (middle / PAGE - 1..middle / PAGE + 3)
            .map(|p| (p, Arc::from(piece(p * PAGE, (p + 1) * PAGE))))
            .collect(),
    );
    let content = Content::Paged(Arc::new(Paged {
        pages: cache.pages().clone(),
        pieces: Arc::new(Pieces::default()),
        index: Some(index),
    }));
    let s = snapshot(
        content,
        SIZE,
        LineCount::Exact(SIZE / line.len() as u64 + 1),
    );
    let top = s.pos_of_byte(middle, 120);
    assert_eq!(top.line_start % line.len() as u64, 0);
    let rows = s.rows(top, 60, 120);
    assert_eq!(rows.len(), 60);
    for row in &rows {
        match row {
            Row::Text { text, line, .. } => {
                assert_eq!(text, "0123456789 abcdefghij klmnopqrstuvwxyz;");
                assert!(matches!(line, LineNo::Exact(_)));
            }
            Row::Pending { .. } => panic!("touched a page not in memory"),
        }
    }
    // Far away: pending, not read.
    let far = s.rows(
        RowPos {
            line_start: 4 << 20,
            sub_row: 0,
        },
        5,
        120,
    );
    assert!(matches!(far[0], Row::Pending { .. }));
}
