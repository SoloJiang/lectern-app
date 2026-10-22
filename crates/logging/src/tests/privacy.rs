//! 「测试」隐私: the free-text rules, and canaries written through the real
//! panic hook and `Log::log` to disk. The canaries are synthetic strings.

use std::hint::black_box;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::Duration;

use log::Level;

use super::{Fixture, HOME, assert_absent, records, third_party, with_msg};
use crate::engine::encode::{self, Out};
use crate::engine::sanitize::{formatted_panic, literal, third_party as rule3};
use crate::{PanicText, Value};

const CANARY: &str = "CANARY-7f3a-synthetic";

fn rule1(s: &str) -> String {
    literal(s, Some(HOME)).to_string()
}

fn rule3_text(s: &str) -> String {
    rule3(s, false, Some(HOME)).to_string()
}

fn value_text(v: Value<'_>) -> String {
    let mut buf = [0u8; 4096];
    let mut out = Out::new(&mut buf);
    encode::write_value(&mut out, &v, Some(HOME));
    String::from_utf8(out.bytes().to_vec()).expect("UTF-8")
}

#[test]
fn static_panic_text_is_kept_up_to_256_bytes() {
    let text = "called `Option::unwrap()` on a `None` value";
    assert_eq!(rule1(text), text);
    assert_eq!(rule1(&"x ".repeat(200)).len(), 256);
    assert_eq!(
        value_text(Value::panic_message(PanicText::Static(text))),
        format!("\"{}\"", text)
    );
}

#[test]
fn free_text_keeps_the_first_line() {
    assert_eq!(rule1("first line\nsecond line"), "first line");
    assert_eq!(rule1("first\r\nsecond"), "first");
    assert_eq!(rule3_text("connection reset\nretrying"), "connection reset");
    assert_eq!(
        formatted_panic("assertion `left == right` failed\n  left: 1\n right: 2").prefix,
        Some("assertion `left == right` failed")
    );
}

#[test]
fn free_text_masks_credential_shapes() {
    let cases = [
        ("key sk-abc123 end", "key <redacted> end"),
        ("token=ghp_0123456789 rest", "token=<redacted> rest"),
        (
            "gho_abc github_pat_x glpat-y xoxb-1-2",
            "<redacted> <redacted> <redacted> <redacted>",
        ),
        (
            "Authorization: Bearer abc.def-ghi done",
            "Authorization: Bearer <redacted> done",
        ),
        ("jwt eyJhbGciOi.eyJzdWIiOi.c2lnbmF0dXJl", "jwt <redacted>"),
        (
            "aws AKIA0123456789ABCDEFGHIJKLMNOPQRSTUV x",
            "aws <redacted> x",
        ),
        ("short abc123 kept", "short abc123 kept"),
    ];
    for (input, expected) in cases {
        assert_eq!(rule1(input), expected, "{input}");
    }
    // Rule 3 masks the same shapes before its own rules.
    assert_eq!(rule3_text("using sk-abc123 now"), "using <redacted> now");
}

#[test]
fn free_text_is_cut_on_a_char_boundary() {
    let euro = "€".repeat(100);
    let one = rule1(&euro);
    assert_eq!(one.len(), 255);
    assert!(one.chars().all(|c| c == '€'));
    let three = rule3_text(&euro);
    assert_eq!(three.len(), 126);
    assert!(three.chars().all(|c| c == '€'));
}

#[test]
fn formatted_panic_keeps_only_location_and_length() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    let secret = black_box(format!("formatted {CANARY}"));
    let line = line!() + 1;
    let result = catch_unwind(AssertUnwindSafe(|| panic!("{secret}")));
    assert!(result.is_err());
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    let panics = with_msg(&records, "panic");
    assert_eq!(panics.len(), 1, "{records:?}");
    assert_eq!(
        panics[0]["message"],
        serde_json::json!({ "len": secret.len() })
    );
    assert_eq!(panics[0]["at"], format!("{}:{line}", file!()));
    assert_absent(fx.path(), CANARY);
}

#[test]
fn std_prefix_table_keeps_prefix_and_numbers() {
    let s = formatted_panic("index out of bounds: the len is 3 but the index is 5");
    assert_eq!(
        (s.prefix, s.nums()),
        (Some("index out of bounds"), &[3, 5][..])
    );
    let s = formatted_panic(
        "byte index 5 is not a char boundary; it is inside 'é' (bytes 4..6) of `ab1é9`",
    );
    assert_eq!((s.prefix, s.nums()), (Some("byte index"), &[5, 4, 6][..]));
    // The forms of Rust 1.97.
    let s = formatted_panic(
        "end byte index 4 is not a char boundary; it is inside 'é' (bytes 3..5 of string)",
    );
    assert_eq!(
        (s.prefix, s.nums()),
        (Some("end byte index"), &[4, 3, 5][..])
    );
    let s = formatted_panic(
        "start byte index 4 is not a char boundary; it is inside 'é' (bytes 3..5 of string)",
    );
    assert_eq!(
        (s.prefix, s.nums()),
        (Some("start byte index"), &[4, 3, 5][..])
    );
    let s = formatted_panic("byte range starts at 3 but ends at 1");
    assert_eq!(
        (s.prefix, s.nums()),
        (Some("byte range starts at"), &[3, 1][..])
    );
    let s = formatted_panic("range end index 10 out of range for slice of length 4");
    assert_eq!(
        (s.prefix, s.nums()),
        (Some("range end index"), &[10, 4][..])
    );
    let s = formatted_panic("slice index starts at 1 but ends at 0 1 2 3 4 5 6 7");
    assert_eq!(s.nums(), &[1, 0, 1, 2, 3, 4][..], "at most 6 numbers");
    let s = formatted_panic("called `Result::unwrap_err()` on an `Ok` value: 42");
    assert_eq!(
        (s.prefix, s.nums()),
        (
            Some("called `Result::unwrap_err()` on an `Ok` value"),
            &[][..]
        )
    );
    let s = formatted_panic("not in the table 1 2 3");
    assert_eq!(
        (s.prefix, s.nums(), s.variant(), s.len),
        (None, &[][..], None, 22)
    );
}

#[test]
fn unwrap_err_keeps_only_the_variant() {
    let s = formatted_panic(
        "called `Result::unwrap()` on an `Err` value: Os { code: 2, kind: NotFound, message: \"No such file or directory\" }",
    );
    assert_eq!(
        (s.prefix.is_some(), s.variant(), s.nums()),
        (true, Some("Os"), &[][..])
    );
    let s = formatted_panic(
        "called `Result::unwrap()` on an `Err` value: Error { input: \"secret\", line: 3 }",
    );
    assert_eq!(s.variant(), Some("Error"));
    let s = formatted_panic("called `Result::unwrap()` on an `Err` value: \"secret text\"");
    assert_eq!(s.variant(), None);
    let s = formatted_panic("called `Result::unwrap()` on an `Err` value: Custom(\"secret\")");
    assert_eq!(s.variant(), Some("Custom"));
    let s = formatted_panic("called `Result::unwrap()` on an `Err` value: some/path here");
    assert_eq!(s.variant(), None, "not an identifier");
}

#[test]
fn unpaired_quote_drops_to_the_end_of_line() {
    let s = formatted_panic("index out of bounds: the len is 3 but 'the index is 99");
    assert_eq!(s.nums(), &[3][..]);
    assert_eq!(
        rule3_text("cannot open 'secret 42 file"),
        "cannot open \u{2026}"
    );
    assert_eq!(
        rule3_text("value \"a\" then `b"),
        "value \"\u{2026}\" then \u{2026}"
    );
}

#[test]
fn third_party_masks_quoted_and_backtick_segments() {
    assert_eq!(
        rule3_text(r#"open "secret" and 'other' and `code`"#),
        "open \"\u{2026}\" and '\u{2026}' and `\u{2026}`"
    );
}

#[test]
fn third_party_keeps_std_method_names_in_backticks() {
    assert_eq!(
        rule3_text("called `Option::unwrap()` on a `None` value"),
        "called `Option::unwrap()` on a `None` value"
    );
    assert_eq!(
        rule3_text("`Result::expect()` `Err` `Ok` `Some` `foo()` `Option::Unwrap()`"),
        "`Result::expect()` `Err` `Ok` `Some` `\u{2026}` `\u{2026}`"
    );
}

#[test]
fn third_party_masks_after_the_first_colon() {
    assert_eq!(
        rule3_text("failed to read config: file not found: x"),
        "failed to read config: \u{2026}"
    );
    assert_eq!(rule3_text("no colon here"), "no colon here");
    assert_eq!(rule3_text("ratio 1:2 kept"), "ratio 1:2 kept");
}

#[test]
fn third_party_is_cut_at_128_bytes() {
    let text = rule3_text(&"word ".repeat(60));
    assert_eq!(text.len(), 128);
}

#[test]
fn non_literal_lectern_message_is_treated_as_third_party() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    let detail = black_box(CANARY);
    third_party(
        Level::Warn,
        "lectern_cache::store",
        "crates/cache/src/store.rs",
        9,
        format_args!("open \"{detail}\": {detail}"),
    );
    third_party(
        Level::Warn,
        "lectern_cache::store",
        "crates/cache/src/store.rs",
        10,
        format_args!("plain literal"),
    );
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    let masked = with_msg(&records, "open \"\u{2026}\": \u{2026}");
    assert_eq!(masked.len(), 1, "{records:?}");
    assert_eq!(masked[0]["tg"], "cache");
    assert_eq!(with_msg(&records, "plain literal").len(), 1);
    assert_absent(fx.path(), CANARY);
}

#[test]
fn url_drops_userinfo_query_and_fragment() {
    let cases = [
        (
            "https://user:pass@api.github.com/repos/a/b?access_token=x#top",
            "\"https://api.github.com/repos/a/b\"",
        ),
        (
            "https://gitlab.example.com:8443/api/v4?private_token=y",
            "\"https://gitlab.example.com:8443/api/v4\"",
        ),
        ("http://localhost:4000", "\"http://localhost:4000\""),
        ("not a url", "\"<url>\""),
        ("://nothing", "\"<url>\""),
    ];
    for (input, expected) in cases {
        assert_eq!(value_text(Value::url(input)), expected, "{input}");
    }
}

#[test]
fn home_is_written_as_tilde() {
    assert_eq!(
        value_text(Value::path(Path::new("/home/tester/src/a.rs"))),
        "\"~/src/a.rs\""
    );
    assert_eq!(value_text(Value::path(Path::new("/home/tester"))), "\"~\"");
    assert_eq!(
        value_text(Value::path(Path::new("/home/testers/a"))),
        "\"/home/testers/a\""
    );
    assert_eq!(
        rule1("cannot open /home/tester/a.db now"),
        "cannot open ~/a.db now"
    );
    assert_eq!(rule1("not /x/home/tester/a"), "not /x/home/tester/a");
}

/// The free texts of the canary tests, written under a filter.
fn sanitized_under(filter: Option<&str>) -> Vec<String> {
    let fx = Fixture::new();
    let logging = fx.start(filter);
    let texts = [
        "open \"a\" failed: denied",
        "called `Option::unwrap()` on a `None` value",
        "token sk-abc123 and /home/tester/x",
    ];
    for (i, text) in texts.iter().enumerate() {
        third_party(
            Level::Warn,
            "gpui::window",
            "src/window.rs",
            i as u32 + 1,
            format_args!("{}", black_box(text)),
        );
    }
    third_party(
        Level::Warn,
        "gpui::window",
        "src/window.rs",
        99,
        format_args!("a literal: kept"),
    );
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    records
        .iter()
        .filter(|r| r["tg"] == "gpui")
        .map(|r| r["msg"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn verbose_does_not_change_sanitizing() {
    let default = sanitized_under(None);
    let trace = sanitized_under(Some("trace,gpui=trace"));
    assert_eq!(default.len(), 4);
    assert_eq!(default, trace);
}

#[test]
fn canary_str_slicing_panic() {
    let fx = Fixture::new();
    let logging = fx.start(Some("trace,gpui=trace"));
    let s = black_box(format!("{CANARY}é{CANARY}"));
    let n = black_box(CANARY.len() + 1);
    let result = catch_unwind(AssertUnwindSafe(|| s[..n].len()));
    assert!(result.is_err(), "slicing inside 'é' panics");
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    let panics = with_msg(&records, "panic");
    assert_eq!(panics.len(), 1, "{records:?}");
    // "byte index …" in older std, "end byte index …" in Rust 1.97.
    let prefix = panics[0]["message"]["prefix"].as_str().unwrap_or_default();
    assert!(prefix.ends_with("byte index"), "{}", panics[0]);
    let nums = panics[0]["message"]["nums"].as_array().expect("numbers");
    assert_eq!(nums[0], n as u64);
    assert_absent(fx.path(), CANARY);
}

#[test]
#[allow(
    clippy::expect_fun_call,
    reason = "the canary is exactly `expect(&format!(..))`"
)]
fn canary_formatted_panic() {
    let fx = Fixture::new();
    let logging = fx.start(Some("trace,gpui=trace"));
    let canary = black_box(CANARY.to_owned());
    let runs: [Box<dyn Fn()>; 4] = [
        Box::new(|| panic!("{canary}")),
        Box::new(|| black_box(Err::<(), _>(canary.clone())).expect(&format!("ctx {canary}"))),
        Box::new(|| assert!(black_box(false), "{canary}")),
        Box::new(|| black_box(Option::<()>::None).expect(&canary)),
    ];
    for run in &runs {
        assert!(catch_unwind(AssertUnwindSafe(run)).is_err());
    }
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    let panics = with_msg(&records, "panic");
    assert_eq!(panics.len(), 4, "{records:?}");
    for p in panics {
        assert!(p["message"]["len"].as_u64().is_some_and(|l| l > 0), "{p}");
    }
    assert_absent(fx.path(), CANARY);
}

#[test]
fn canary_third_party_record() {
    let fx = Fixture::new();
    let logging = fx.start(Some("trace,gpui=trace"));
    let canary = black_box(CANARY);
    third_party(
        Level::Warn,
        "gpui::window",
        "src/window.rs",
        1,
        format_args!("quoted \"{canary}\" end"),
    );
    third_party(
        Level::Warn,
        "gpui::window",
        "src/window.rs",
        2,
        format_args!("single '{canary}' end"),
    );
    third_party(
        Level::Warn,
        "gpui::window",
        "src/window.rs",
        3,
        format_args!("ticks `{canary}` end"),
    );
    third_party(
        Level::Warn,
        "gpui::window",
        "src/window.rs",
        4,
        format_args!("failed: {canary}"),
    );
    third_party(
        Level::Debug,
        "gpui::window",
        "src/window.rs",
        5,
        format_args!("debug: {canary}"),
    );
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    assert_eq!(
        records
            .iter()
            .filter(|r| r["tg"] == "gpui" && r["lv"] == "warn")
            .count(),
        4,
        "{records:?}"
    );
    assert_absent(fx.path(), CANARY);
}
