//! The pure parts of highlighting (docs/design/review_view.md「测试」,
//! `highlight_window_*`): the windows a side needs, the cache of spans, the
//! text of a line and its runs of colour.

use std::sync::Arc;

use gpui::TestAppContext;
use lectern_i18n::{Locale, Strings};
use lectern_services::changes::SideText;
use lectern_services::project::BlobId;
use lectern_services::review::{DiffRow, FileRows, HunkAttribution, HunkInfo, RowsSide};
use lectern_syntax::TokenKind;
use lectern_ui::{Appearance, Palette, code_style, init_fixed, palette};

use crate::highlight::{self, Half, SideSpans, SpanKey, SpansCache, WINDOW_LIMIT};
use crate::lines;
use crate::tests::step2::{final_key, new_oid, old_oid, sample_rows};

fn rows_of(text: &str, rows: Vec<DiffRow>) -> FileRows {
    let mut rows_out = sample_rows(final_key());
    rows_out.old = RowsSide {
        oid: Some(old_oid()),
        text: SideText::Text(Arc::from(text.as_bytes())),
    };
    rows_out.new = RowsSide {
        oid: Some(new_oid()),
        text: SideText::Text(Arc::from(text.as_bytes())),
    };
    rows_out.rows = Arc::from(rows);
    rows_out.hunks = Arc::from(vec![HunkInfo {
        old: 0..1,
        new: 0..1,
        attribution: HunkAttribution {
            lines: true,
            turns: Arc::from(Vec::new()),
            others: Arc::from(Vec::new()),
            drifted: 0,
        },
    }]);
    rows_out.inline = Arc::from(Vec::new());
    rows_out
}

fn context(old: u32, new: u32, byte: u64) -> DiffRow {
    DiffRow::Context {
        old,
        new,
        old_byte: Some(byte),
        new_byte: Some(byte),
        hunk: Some(0),
    }
}

#[test]
fn highlight_window_spans_the_lines_the_rows_show() {
    let rows = sample_rows(final_key());
    // From the first row's line start to the end of the last row's line
    // (`}` at byte 44, its newline at 45), on both sides.
    assert_eq!(
        highlight::plan_windows(&rows, Half::New, false),
        vec![0..45]
    );
    assert_eq!(
        highlight::plan_windows(&rows, Half::Old, false),
        vec![0..45]
    );
}

#[test]
fn highlight_window_ends_at_the_end_of_the_last_line() {
    // Two lines far apart: the window takes everything between them.
    let text = "aaaa\nbbbb\ncccc\ndddd\neeee\n";
    let rows = rows_of(text, vec![context(0, 0, 0), context(3, 3, 15)]);
    // Lines 0 and 3: bytes 0 to the end of "dddd".
    assert_eq!(
        highlight::plan_windows(&rows, Half::New, false),
        vec![0..19]
    );
    // A last line without a newline ends at the text's end.
    let text = "aaaa\nbbbb";
    let rows = rows_of(text, vec![context(1, 1, 5)]);
    assert_eq!(highlight::plan_windows(&rows, Half::New, false), vec![5..9]);
}

#[test]
fn highlight_window_splits_over_the_limit() {
    // Rows every 100 KiB: more than 256 KiB between the first and the last.
    let line = format!("{}\n", "x".repeat(99));
    let text = line.repeat(6000);
    let step = 1000u32;
    let rows = rows_of(
        &text,
        (0..6)
            .map(|n| context(n * step, n * step, u64::from(n * step) * 100))
            .collect(),
    );
    let windows = highlight::plan_windows(&rows, Half::New, false);
    assert!(windows.len() >= 2, "{windows:?}");
    for window in &windows {
        assert!(window.end - window.start <= WINDOW_LIMIT, "{window:?}");
    }
    // In order, not overlapping, and every row's line is inside one.
    for pair in windows.windows(2) {
        assert!(pair[0].end <= pair[1].start);
    }
    for n in 0..6u64 {
        let start = n * 1000 * 100;
        assert!(
            windows
                .iter()
                .any(|window| window.start <= start && start + 99 <= window.end),
            "row {n}"
        );
    }
}

#[test]
fn highlight_window_skips_a_side_without_bytes_or_oid() {
    let mut rows = sample_rows(final_key());
    rows.old = RowsSide {
        oid: Some(old_oid()),
        text: SideText::TooLarge { size: 99 },
    };
    assert!(highlight::plan_windows(&rows, Half::Old, false).is_empty());
    assert!(!highlight::plan_windows(&rows, Half::New, false).is_empty());
    rows.new.oid = None;
    assert!(
        highlight::plan_windows(&rows, Half::New, false).is_empty(),
        "a side without an OID is not cached, so it is not highlighted"
    );
    // Rows without `*_byte` have no window.
    let mut rows = sample_rows(final_key());
    rows.rows = Arc::from(vec![DiffRow::Context {
        old: 0,
        new: 0,
        old_byte: None,
        new_byte: None,
        hunk: Some(0),
    }]);
    assert!(highlight::plan_windows(&rows, Half::New, false).is_empty());
}

#[test]
fn highlight_window_json_is_one_window_or_none() {
    let rows = sample_rows(final_key());
    let len = rows.new.text_len();
    assert_eq!(
        highlight::plan_windows(&rows, Half::New, true),
        vec![0..len]
    );
    // Over the limit a JSON side is plain text.
    let big = "x".repeat(WINDOW_LIMIT as usize + 1);
    let rows = rows_of(&big, vec![context(0, 0, 0)]);
    assert!(highlight::plan_windows(&rows, Half::New, true).is_empty());
    // The language of a path decides.
    assert!(highlight::lang_of("data/config.json").is_some_and(|lang| lang.json));
    assert!(highlight::lang_of("src/main.rs").is_some_and(|lang| !lang.json));
    assert!(highlight::lang_of("notes.unknownext").is_none());
}

trait TextLen {
    fn text_len(&self) -> u64;
}

impl TextLen for RowsSide {
    fn text_len(&self) -> u64 {
        match &self.text {
            SideText::Text(bytes) => bytes.len() as u64,
            _ => 0,
        }
    }
}

fn spans_for(source: &str) -> Arc<lectern_syntax::WindowSpans> {
    let bytes: Arc<[u8]> = Arc::from(source.as_bytes());
    let work = highlight::Work {
        key: SpanKey {
            oid: BlobId::Sha1([9; 20]),
            query_version: 0,
            window: 0..bytes.len() as u64,
        },
        text: bytes,
        path: "a.rs".to_owned(),
        language: Arc::from("rust"),
        cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    match highlight::run(work) {
        crate::plugin::Job::Spans { spans, .. } => spans,
        other => panic!("the task gave no spans: {other:?}"),
    }
}

fn key(oid: u8, start: u64) -> SpanKey {
    SpanKey {
        oid: BlobId::Sha1([oid; 20]),
        query_version: 1,
        window: start..start + 10,
    }
}

#[test]
fn highlight_cache_drops_the_least_recently_used_over_its_limit() {
    let spans = spans_for("fn main() { let a = 1; }\n");
    let weight = spans.bytes();
    let mut cache = SpansCache::with_limit(weight * 2 + weight / 2);
    cache.insert(key(1, 0), spans.clone());
    cache.insert(key(2, 0), spans.clone());
    assert_eq!(cache.len(), 2);
    // Reading the first makes the second the oldest.
    assert!(cache.get(&key(1, 0)).is_some());
    cache.insert(key(3, 0), spans.clone());
    assert_eq!(cache.len(), 2, "over the limit: one goes");
    assert!(cache.contains(&key(1, 0)));
    assert!(!cache.contains(&key(2, 0)));
    assert!(cache.contains(&key(3, 0)));
    assert!(cache.bytes() <= weight * 2 + weight / 2);
    // The same key again replaces, it does not add weight twice.
    let before = cache.bytes();
    cache.insert(key(3, 0), spans);
    assert_eq!(cache.bytes(), before);
}

#[test]
fn highlight_cache_is_keyed_by_oid_version_and_window() {
    let spans = spans_for("let a = 1;\n");
    let mut cache = SpansCache::new();
    cache.insert(key(1, 0), spans);
    assert!(cache.get(&key(1, 0)).is_some());
    assert!(cache.get(&key(2, 0)).is_none(), "another blob");
    assert!(cache.get(&key(1, 5)).is_none(), "another window");
    let mut other_version = key(1, 0);
    other_version.query_version = 2;
    assert!(cache.get(&other_version).is_none(), "another query version");
}

#[test]
fn highlight_side_spans_find_the_window_of_a_byte() {
    let source = "fn main() {\n    let a = 1;\n}\n";
    let spans = spans_for(source);
    let mut side = SideSpans::default();
    assert_eq!(side.state(Half::New), "plain");
    assert!(side.line(Half::New, 12).is_empty());
    side.push(Half::New, 0..source.len() as u64, spans);
    assert_eq!(side.state(Half::New), "spans");
    assert_eq!(side.state(Half::Old), "plain");
    let tokens = side.line(Half::New, 12);
    assert!(
        tokens
            .iter()
            .any(|t| t.kind() == TokenKind::Keyword && t.start() == 4 && t.len() == 3)
    );
    // A byte outside every window has no tokens.
    assert!(side.line(Half::New, 5000).is_empty());
}

// ----- the text of a line ------------------------------------------------------

fn side(text: &str) -> RowsSide {
    RowsSide {
        oid: None,
        text: SideText::Text(Arc::from(text.as_bytes())),
    }
}

#[test]
fn lines_text_comes_from_the_byte_or_the_line_number() {
    let side = side("one\r\ntwo\n\tthree\n");
    assert_eq!(lines::display_line(&side, Some(5), 99).text, "two");
    assert_eq!(lines::display_line(&side, None, 0).text, "one");
    assert_eq!(
        lines::display_line(&side, None, 2).text,
        "    three",
        "a tab is four cells"
    );
    assert_eq!(lines::display_line(&side, None, 40).text, "");
}

/// The light palette, as a window would give it.
fn light(cx: &mut TestAppContext) -> Palette {
    cx.update(|cx| {
        init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false);
        *palette(Appearance::Light, cx)
    })
}

#[gpui::test]
fn lines_long_lines_are_cut_and_repaired_lines_have_no_runs(cx: &mut TestAppContext) {
    let long = "a".repeat(lines::MAX_LINE_BYTES + 100);
    let line = lines::display_line(&side(&long), Some(0), 0);
    assert_eq!(line.text.len(), lines::MAX_LINE_BYTES);
    let palette = light(cx);
    // A line that is not UTF-8 is shown repaired and gets no colour: the
    // offsets of the source do not apply to it.
    let broken = RowsSide {
        oid: None,
        text: SideText::Text(Arc::from(&b"let \xff = 1;\n"[..])),
    };
    let line = lines::display_line(&broken, Some(0), 0);
    let token = lectern_syntax::Token::new(0, 3, TokenKind::Keyword).unwrap();
    assert!(
        lines::highlights(
            &line,
            &[token],
            std::slice::from_ref(&(0..2)),
            Some(palette.label),
            &palette,
        )
        .is_empty()
    );
}

#[gpui::test]
fn lines_runs_follow_the_tokens_through_the_tabs(cx: &mut TestAppContext) {
    let palette = light(cx);
    let line = lines::display_line(&side("\tlet a = 1;\n"), Some(0), 0);
    assert_eq!(line.text, "    let a = 1;");
    let keyword = lectern_syntax::Token::new(1, 3, TokenKind::Keyword).unwrap();
    let number = lectern_syntax::Token::new(9, 1, TokenKind::Number).unwrap();
    let runs = lines::highlights(&line, &[keyword, number], &[], None, &palette);
    assert_eq!(runs.len(), 2);
    // `let` starts after the tab's four cells.
    assert_eq!(runs[0].0, 4..7);
    assert_eq!(
        runs[0].1.color,
        Some(code_style(TokenKind::Keyword, &palette).color)
    );
    assert_eq!(runs[1].0, 12..13);
    assert_eq!(
        runs[1].1.color,
        Some(code_style(TokenKind::Number, &palette).color)
    );
}

#[gpui::test]
fn lines_changed_words_lie_over_the_tokens(cx: &mut TestAppContext) {
    let palette = light(cx);
    let background = palette.diff_added_inline_background;
    let line = lines::display_line(&side("let a = 2;\n"), Some(0), 0);
    let keyword = lectern_syntax::Token::new(0, 3, TokenKind::Keyword).unwrap();
    let number = lectern_syntax::Token::new(8, 1, TokenKind::Number).unwrap();
    // The changed word is the number, and part of the keyword.
    let runs = lines::highlights(
        &line,
        &[keyword, number],
        &[2..4, 8..9],
        Some(background),
        &palette,
    );
    let shape: Vec<(std::ops::Range<usize>, bool, bool)> = runs
        .iter()
        .map(|(range, style)| {
            (
                range.clone(),
                style.color.is_some(),
                style.background_color == Some(background),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            (0..2, true, false),
            (2..3, true, true),
            (3..4, false, true),
            (8..9, true, true)
        ]
    );
    // Runs never overlap and are in order.
    for pair in runs.windows(2) {
        assert!(pair[0].0.end <= pair[1].0.start);
    }
    // No background colour given: the words are not marked.
    let plain = lines::highlights(
        &line,
        &[keyword],
        std::slice::from_ref(&(2..4)),
        None,
        &palette,
    );
    assert_eq!(plain.len(), 1);
    assert!(plain[0].1.background_color.is_none());
}
