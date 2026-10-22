//! 「测试」编码: a set of records against a snapshot, each line valid JSON,
//! `trunc` only past 4 KiB.

use std::io;
use std::path::Path;
use std::time::Duration;

use log::Level;

use super::{HOME, base_time};
use crate::engine::encode::{self, Head, MAX_LINE, Message, Out};
use crate::{Ctx, Field, PanicText, PluginTag, Value};

enum Reason {
    Corrupt,
}

impl Field for Reason {
    fn value(&self) -> Value<'_> {
        match self {
            Reason::Corrupt => Value::text("corrupt"),
        }
    }
}

fn head(level: Level, ctx: Ctx) -> Head<'static> {
    Head {
        t: base_time() + Duration::from_millis(123),
        up: 1_834_211,
        level,
        target: "cache",
        file: Some("crates/cache/src/store.rs"),
        line: Some(212),
        ctx,
    }
}

fn line(head: &Head<'_>, message: Message<'_>, fields: &[(&str, Value<'_>)], n: u64) -> String {
    let mut buf = [0u8; MAX_LINE];
    let mut out = Out::new(&mut buf);
    encode::record(&mut out, head, message, fields, n, Some(HOME));
    String::from_utf8(out.bytes().to_vec()).expect("UTF-8")
}

/// On a named thread, so that `th` is fixed.
fn on_worker<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .name("lectern-worker-2".to_owned())
        .spawn(f)
        .expect("thread")
        .join()
        .expect("no panic")
}

#[test]
fn encoding_snapshot_and_every_line_is_json() {
    let lines = on_worker(|| {
        let scoped = Ctx {
            plugin: Some(PluginTag::new("review")),
            act: Some(12),
            task: Some(3401),
            ..Ctx::default()
        };
        let correlated = Ctx {
            session: Some([
                0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 1, 2, 3, 4, 5, 6, 7, 8,
            ]),
            review: Some(0xab12),
            req: Some(7),
            analysis: Some(2),
            ..Ctx::default()
        };
        let path = Path::new("/home/tester/Library/Caches/Lectern/repo/ab12cd.db");
        let oid = [
            0xab_u8, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45,
            0x67, 0x89, 0xab, 0xcd, 0xef, 0x01,
        ];
        let missing = io::Error::from_raw_os_error(2);
        let none: Option<u32> = None;
        vec![
            line(
                &head(Level::Warn, scoped),
                Message::Literal("partition disabled"),
                &[
                    ("reason", Reason::Corrupt.value()),
                    ("path", Value::path(path)),
                    ("sqlite", 11.value()),
                ],
                0,
            ),
            line(
                &head(Level::Info, correlated),
                Message::Literal("values"),
                &[
                    ("yes", true.value()),
                    ("neg", (-5_i64).value()),
                    ("big", u128::MAX.value()),
                    ("ratio", 0.25_f64.value()),
                    ("nan", f64::NAN.value()),
                    ("took", Duration::from_millis(3).value()),
                    ("text", Value::text("a \"quoted\"\tline\n")),
                    ("oid", Value::oid(&oid)),
                    ("hex", Value::hex(0x9f2c)),
                    (
                        "url",
                        Value::url("https://user:secret@api.github.com:443/repos/a/b?token=x#frag"),
                    ),
                    ("error", missing.value()),
                    ("none", none.value()),
                ],
                3,
            ),
            line(
                &head(Level::Error, Ctx::default()),
                Message::Literal("panics"),
                &[
                    (
                        "static",
                        Value::panic_message(PanicText::Static(
                            "called `Option::unwrap()` on a `None` value",
                        )),
                    ),
                    (
                        "formatted",
                        Value::panic_message(PanicText::Formatted(
                            "index out of bounds: the len is 3 but the index is 5",
                        )),
                    ),
                    ("other", Value::panic_message(PanicText::Other)),
                ],
                0,
            ),
            line(
                &head(Level::Warn, Ctx::default()),
                Message::FreeLiteral("window not found"),
                &[],
                0,
            ),
            line(
                &head(Level::Warn, Ctx::default()),
                Message::FreeFormatted {
                    text: "failed to open \"/home/tester/x\": permission denied",
                    cut: false,
                },
                &[],
                0,
            ),
        ]
    });
    for l in &lines {
        assert!(l.ends_with('\n') && l.len() <= MAX_LINE, "{l}");
        let v: serde_json::Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"));
        assert!(v.get("trunc").is_none(), "{l}");
    }
    insta::assert_snapshot!(lines.concat());
}

#[test]
fn trunc_only_past_4_kib_keeps_the_fixed_fields_and_msg() {
    let long = "/home/tester/".to_owned() + &"d/".repeat(600);
    let path = Path::new(&long);
    let fields: Vec<(&str, Value<'_>)> = (0..12).map(|_| ("p", Value::path(path))).collect();
    let l = line(
        &head(Level::Info, Ctx::default()),
        Message::Literal("long"),
        &fields,
        4,
    );
    assert!(l.len() <= MAX_LINE, "{}", l.len());
    let v: serde_json::Value = serde_json::from_str(&l).expect("JSON");
    assert_eq!(v["trunc"], true);
    assert_eq!(v["msg"], "long");
    assert_eq!(v["n"], 4);
    for key in ["t", "up", "lv", "tg", "th", "at"] {
        assert!(v.get(key).is_some(), "{key} kept");
    }
    // The first fields fit; the line holds as many as fit.
    assert!(l.matches("\"p\":").count() >= 2);

    let short = line(
        &head(Level::Info, Ctx::default()),
        Message::Literal("short"),
        &fields[..2],
        0,
    );
    let v: serde_json::Value = serde_json::from_str(&short).expect("JSON");
    assert!(v.get("trunc").is_none());
}
