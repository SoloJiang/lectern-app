//! `spans_*`: whole-file highlight snapshots per language, the token
//! encoding, the per-line store and how captures resolve.

use std::cell::Cell;
use std::fmt::Write as _;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::highlight::{Budget, Outcome, resolve_for_tests, whole_file, whole_file_in};
use crate::spans::{SpansBuilder, push_tokens};
use crate::tests::health::ManualClocks;
use crate::{Languages, Token, TokenKind, WindowSpans};

fn budget(cancel: &AtomicBool) -> Budget<'_> {
    Budget {
        budget: Duration::from_secs(60),
        cancel,
        yield_probe: None,
    }
}

fn highlight(lang: &str, text: &str) -> WindowSpans {
    let langs = Languages::core();
    let lang = langs.get(lang).expect("core language");
    let cancel = AtomicBool::new(false);
    match whole_file(lang, text.as_bytes(), budget(&cancel)) {
        Outcome::Done(spans) => spans,
        other => panic!("{}: {other:?}", lang.name()),
    }
}

/// Each line with its tokens under it: `Kind "text"`.
fn render(text: &str, spans: &WindowSpans) -> String {
    let mut out = String::new();
    let mut line_start = 0;
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let shown = line.trim_end_matches('\n');
        writeln!(out, "{:>3} | {shown}", n + 1).unwrap();
        let tokens = spans.line(line_start as u64);
        if !tokens.is_empty() {
            let parts: Vec<String> = tokens
                .iter()
                .map(|t| {
                    format!(
                        "{:?} {:?}",
                        t.kind(),
                        &shown[t.start() as usize..t.end() as usize]
                    )
                })
                .collect();
            writeln!(out, "    | {}", parts.join(" · ")).unwrap();
        }
        line_start += line.len();
    }
    out
}

/// Tokens never overlap, stay in their line and never cover the `\n`.
fn check_invariants(text: &str, spans: &WindowSpans) {
    assert!(spans.exact());
    assert_eq!(spans.range(), 0..text.len() as u64);
    for (line_start, tokens) in spans.lines() {
        let line = &text[line_start as usize..];
        let line = &line[..line.find('\n').unwrap_or(line.len())];
        let mut end = 0;
        for token in tokens {
            assert!(token.start() >= end, "overlap at {line_start}: {tokens:?}");
            assert!(!token.is_empty());
            assert!(token.end() as usize <= line.len(), "{line:?}: {token:?}");
            end = token.end();
        }
    }
}

macro_rules! spans_snapshot {
    ($test:ident, $lang:literal, $file:literal) => {
        #[test]
        fn $test() {
            let text = include_str!(concat!("fixtures/", $file));
            let spans = highlight($lang, text);
            check_invariants(text, &spans);
            insta::assert_snapshot!(render(text, &spans));
        }
    };
}

spans_snapshot!(spans_ts, "ts", "sample.ts");
spans_snapshot!(spans_tsx, "tsx", "sample.tsx");
spans_snapshot!(spans_js, "js", "sample.js");
spans_snapshot!(spans_rust, "rust", "sample.rs");
spans_snapshot!(spans_java, "java", "sample.java");
spans_snapshot!(spans_python, "python", "sample.py");
spans_snapshot!(spans_go, "go", "sample.go");
spans_snapshot!(spans_json, "json", "sample.json");
spans_snapshot!(spans_yaml, "yaml", "sample.yaml");
spans_snapshot!(spans_toml, "toml", "sample.toml");
spans_snapshot!(spans_markdown, "markdown", "sample.md");

#[test]
fn spans_token_encoding() {
    for kind in TokenKind::ALL {
        let token = Token::new(u32::MAX - Token::MAX_LEN, Token::MAX_LEN, kind).unwrap();
        assert_eq!(token.start(), u32::MAX - Token::MAX_LEN);
        assert_eq!(token.len(), Token::MAX_LEN);
        assert_eq!(token.kind(), kind);
        let small = Token::new(7, 1, kind).unwrap();
        assert_eq!((small.start(), small.len(), small.kind()), (7, 1, kind));
    }
    assert_eq!(size_of::<Token>(), 8);
    assert!(Token::new(0, 0, TokenKind::Other).is_none());
    assert!(Token::new(0, Token::MAX_LEN + 1, TokenKind::Other).is_none());
}

#[test]
fn spans_long_tokens_split_at_24_bits() {
    let mut tokens = Vec::new();
    let len = 2 * Token::MAX_LEN + 5;
    push_tokens(&mut tokens, 3, len, TokenKind::String);
    let parts: Vec<(u32, u32)> = tokens.iter().map(|t| (t.start(), t.len())).collect();
    assert_eq!(
        parts,
        [
            (3, Token::MAX_LEN),
            (3 + Token::MAX_LEN, Token::MAX_LEN),
            (3 + 2 * Token::MAX_LEN, 5)
        ]
    );
    assert!(tokens.iter().all(|t| t.kind() == TokenKind::String));
    push_tokens(&mut tokens, 0, 0, TokenKind::String);
    assert_eq!(tokens.len(), 3);
}

/// A segment across lines is cut at each `\n`, which no token covers; the
/// store keeps only lines with tokens and finds them by their start.
#[test]
fn spans_builder_splits_lines() {
    let text = b"ab\n\ncd\r\nef";
    let mut builder = SpansBuilder::new(text, 100);
    builder.push(1, 9, TokenKind::Comment);
    builder.push(9, 10, TokenKind::Keyword);
    let spans = builder.finish(true);
    assert_eq!(spans.range(), 100..110);
    type Line = (u64, Vec<(u32, u32, TokenKind)>);
    let lines: Vec<Line> = spans
        .lines()
        .map(|(start, tokens)| {
            (
                start,
                tokens
                    .iter()
                    .map(|t| (t.start(), t.len(), t.kind()))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            (100, vec![(1, 1, TokenKind::Comment)]),
            // The `\r` belongs to its line.
            (104, vec![(0, 3, TokenKind::Comment)]),
            (
                108,
                vec![(0, 1, TokenKind::Comment), (1, 1, TokenKind::Keyword)]
            ),
        ]
    );
    assert_eq!(spans.line(104).len(), 1);
    // The empty line, a line start that is no line's, and outside the range.
    assert!(spans.line(103).is_empty());
    assert!(spans.line(101).is_empty());
    assert!(spans.line(0).is_empty());
    assert!(spans.line(110).is_empty());
    assert_eq!(
        spans.bytes(),
        size_of::<WindowSpans>() + 3 * 4 + 4 * 4 + 4 * 8
    );
}

#[test]
fn spans_empty_text() {
    let spans = highlight("json", "");
    assert_eq!(spans.range(), 0..0);
    assert_eq!(spans.lines().count(), 0);
    assert!(spans.line(0).is_empty());
}

/// Equal ranges: the highest priority wins. Nested ranges: the inner one
/// wins over its part of the outer one. Plain ranges colour nothing and
/// hide the range around them.
#[test]
fn spans_captures_resolve() {
    use TokenKind::*;
    let none = None;
    // (start, end, priority, kind)
    let hits = [
        (0, 20, 0, Some(String)),
        (4, 10, 1, none),
        (6, 8, 0, Some(Variable)),
        (6, 8, 5, Some(Function)),
        (6, 8, 2, Some(Type)),
        (12, 14, 0, Some(Escape)),
        (25, 30, 0, Some(Keyword)),
        (25, 30, 1, none),
        (30, 32, 0, Some(Number)),
    ];
    assert_eq!(
        resolve_for_tests(&hits),
        [
            (0, 4, String),
            (6, 8, Function),
            (10, 12, String),
            (12, 14, Escape),
            (14, 20, String),
            (30, 32, Number),
        ]
    );
}

/// The language's precedence decides which pattern colours a node that
/// several capture: Rust takes the first (`Some` is a constructor before it
/// is a call), JavaScript the last (`Foo()` is a constructor after a call).
#[test]
fn spans_precedence_per_language() {
    let kinds = |lang: &str, text: &str, word: &str| {
        let spans = highlight(lang, text);
        let at = text.find(word).unwrap() as u32;
        spans
            .line(0)
            .iter()
            .find(|t| t.start() == at)
            .map(|t| t.kind())
    };
    assert_eq!(
        kinds("rust", "fn f() { Some(1); }", "Some"),
        Some(TokenKind::Type)
    );
    assert_eq!(
        kinds("rust", "fn f() { g(1); }", "g("),
        Some(TokenKind::Function)
    );
    assert_eq!(kinds("js", "Foo(); bar();", "Foo"), Some(TokenKind::Type));
    assert_eq!(
        kinds("js", "Foo(); bar();", "bar"),
        Some(TokenKind::Function)
    );
    // JSON keys: the first pattern, over `(string) @string`.
    assert_eq!(
        kinds("json", r#"{"k": "v"}"#, r#""k""#),
        Some(TokenKind::Property)
    );
    assert_eq!(
        kinds("json", r#"{"k": "v"}"#, r#""v""#),
        Some(TokenKind::String)
    );
}

#[test]
fn spans_whole_file_cancelled() {
    let langs = Languages::core();
    let lang = langs.get("json").unwrap();
    let cancel = AtomicBool::new(true);
    let text = big_json();
    assert!(matches!(
        whole_file(lang, text.as_bytes(), budget(&cancel)),
        Outcome::Cancelled
    ));
}

#[test]
fn spans_whole_file_timed_out() {
    let langs = Languages::core();
    let lang = langs.get("json").unwrap();
    let cancel = AtomicBool::new(false);
    let text = big_json();
    let budget = Budget {
        budget: Duration::ZERO,
        cancel: &cancel,
        yield_probe: None,
    };
    assert!(matches!(
        whole_file(lang, text.as_bytes(), budget),
        Outcome::TimedOut
    ));
}

#[test]
fn spans_whole_file_yielded() {
    let langs = Languages::core();
    let lang = langs.get("json").unwrap();
    let cancel = AtomicBool::new(false);
    let text = big_json();
    let probe = || true;
    let budget = Budget {
        budget: Duration::from_secs(60),
        cancel: &cancel,
        yield_probe: Some(&probe),
    };
    assert!(matches!(
        whole_file(lang, text.as_bytes(), budget),
        Outcome::Yielded
    ));
}

/// The call is in the health table while it runs, with its budget, and not
/// after it returns; a disabled grammar version gives `Unavailable` without
/// parsing.
#[test]
fn spans_whole_file_registers_with_health() {
    let clocks = ManualClocks::new();
    let health = clocks.health();
    let langs = Languages::core();
    let lang = langs.get("json").unwrap();
    let cancel = AtomicBool::new(false);
    let text = big_json();
    let seen = Cell::new(0);
    let probe = || {
        seen.set(seen.get().max(health.running()));
        false
    };
    let budget = Budget {
        budget: Duration::from_secs(60),
        cancel: &cancel,
        yield_probe: Some(&probe),
    };
    assert!(matches!(
        whole_file_in(&health, lang, text.as_bytes(), budget),
        Outcome::Done(_)
    ));
    assert_eq!(seen.get(), 1);
    assert_eq!(health.running(), 0);

    // A stuck json call on another (simulated) thread disables json.
    let stuck = clocks.new_thread();
    clocks.run_on(&stuck);
    let guard = health
        .enter("json", lang.grammar_version(), Duration::from_millis(8))
        .unwrap();
    stuck.add_cpu(Duration::from_secs(2));
    clocks.run_on(&clocks.new_thread());
    let probe_calls = Cell::new(0);
    let probe = || {
        probe_calls.set(probe_calls.get() + 1);
        false
    };
    let budget = Budget {
        budget: Duration::from_secs(60),
        cancel: &cancel,
        yield_probe: Some(&probe),
    };
    assert!(matches!(
        whole_file_in(&health, lang, text.as_bytes(), budget),
        Outcome::Unavailable
    ));
    assert_eq!(probe_calls.get(), 0, "never entered tree-sitter");
    assert!(!health.allows("json", lang.grammar_version()));
    drop(guard);
}

/// About 200 KiB of JSON: enough for tree-sitter to call the progress
/// callback.
fn big_json() -> String {
    let mut text = String::from("[\n");
    for i in 0..10_000 {
        writeln!(text, "  {{\"id\": {i}, \"name\": \"item {i}\"}},").unwrap();
    }
    text.push_str("  null\n]\n");
    text
}
