//! `window_*`: the first read's ranges, the safe start rules, the JSON pad
//! and the first screen's plan.

use std::fmt::Write as _;
use std::ops::Range;

use crate::Rules;
use crate::testing;
use crate::window::{
    Encoding, JsonPad, Plan, Target, checkpoint_read, first_read, json_last_comma, json_pad, plan,
    safe_start,
};

use super::lang;

const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;

#[test]
#[expect(clippy::single_range_in_vec_init, reason = "the read's ranges")]
fn window_first_read_cuts() {
    // Up to tier A: the whole file in one range.
    assert_eq!(first_read(0, 0), [0..0]);
    assert_eq!(first_read(5 * KIB, 3 * KIB), [0..5 * KIB]);
    assert_eq!(first_read(MIB, MIB / 2), [0..MIB]);
    let size = 64 * MIB;
    // At the start: the window is cut at 0 and holds the head.
    assert_eq!(first_read(size, 0), [0..8 * KIB, 0..68 * KIB]);
    assert_eq!(first_read(size, 10 * KIB), [0..8 * KIB, 0..78 * KIB]);
    // Within 72 KiB the window starts at most at 8 KiB (one pread).
    assert_eq!(first_read(size, 72 * KIB)[1].start, 8 * KIB);
    let mid = 32 * MIB;
    assert_eq!(
        first_read(size, mid),
        [0..8 * KIB, mid - 64 * KIB..mid + 68 * KIB]
    );
    // At and past the end: cut at the size.
    assert_eq!(first_read(size, size), [0..8 * KIB, size - 64 * KIB..size]);
    assert_eq!(
        first_read(size, u64::MAX),
        [0..8 * KIB, size - 64 * KIB..size]
    );
    // Never more than platform's 1 MiB per call.
    for t in [0, 1, mid, size] {
        let total: u64 = first_read(size, t).iter().map(|r| r.end - r.start).sum();
        assert!(total <= MIB);
    }
}

#[test]
#[expect(clippy::single_range_in_vec_init, reason = "the read's ranges")]
fn window_checkpoint_read_cuts() {
    let size = 16 * MIB;
    assert_eq!(checkpoint_read(size, 0), [0..8 * KIB, 0..132 * KIB]);
    let c = 64 * KIB * 100;
    assert_eq!(
        checkpoint_read(size, c),
        [0..8 * KIB, c - 64 * KIB..c + 132 * KIB]
    );
    assert_eq!(
        checkpoint_read(size, size),
        [0..8 * KIB, size - 64 * KIB..size]
    );
    assert_eq!(checkpoint_read(512 * KIB, 0), [0..512 * KIB]);
}

/// Lines `(indent, first bytes)`, each 20 bytes long with its `\n`.
fn lines(spec: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in spec {
        let mut line = line.as_bytes().to_vec();
        line.resize(19, b' ');
        line.push(b'\n');
        out.extend(line);
    }
    out
}

#[test]
fn window_safe_start_code_column_zero() {
    let bytes = lines(&[
        "fn a() {", // 0
        "    x();", // 20
        "}",        // 40
        "/**",      // 60
        " * docs",  // 80
        " */",      // 100
        "fn b() {", // 120
        "    y();", // 140
        "}",        // 160
        "",         // 180
        "    z();", // 200
    ]);
    let base = 1000;
    // The closest column-0 line at or before `before` that is not a
    // closing bracket or a comment continuation.
    assert_eq!(
        safe_start(Rules::Code, &bytes, base, base + 200),
        Some(base + 120)
    );
    assert_eq!(
        safe_start(Rules::Code, &bytes, base, base + 119),
        Some(base + 60)
    );
    // The slice's first byte is not known to start a line (it is not the
    // file's start): the least indented line is taken.
    assert_eq!(
        safe_start(Rules::Code, &bytes, base, base + 59),
        Some(base + 20)
    );
    assert_eq!(safe_start(Rules::Code, &bytes, 0, 59), Some(0));
    // `before` itself may be a line start.
    assert_eq!(
        safe_start(Rules::Code, &bytes, base, base + 120),
        Some(base + 120)
    );
}

/// No column-0 line within 4 KiB: the least indented line there, the
/// closest of equals.
#[test]
fn window_safe_start_code_least_indent() {
    let mut bytes = Vec::new();
    for i in 0..400 {
        let indent = if i % 50 == 7 { 2 } else { 8 };
        bytes.extend(std::iter::repeat_n(b' ', indent));
        bytes.extend(format!("call({i});\n").as_bytes());
    }
    let before = bytes.len() as u64 - 10;
    let start = safe_start(Rules::Code, &bytes, 100, 100 + before).unwrap() - 100;
    let start = start as usize;
    assert_eq!(&bytes[start..start + 2], b"  ");
    assert_ne!(bytes[start + 2], b' ');
    // The closest of the two-space lines.
    let closest = (0..bytes.len() - 10)
        .rev()
        .find(|&p| (p == 0 || bytes[p - 1] == b'\n') && bytes[p..].starts_with(b"  c"))
        .unwrap();
    assert_eq!(start, closest);
}

/// One long line: just after the last `;` or `}` before `before`.
#[test]
fn window_safe_start_long_line() {
    let bytes = b"a=1;b(function(){return 2});c=3;d=\"x;y\"+4".repeat(200);
    let before = 5000;
    let start = safe_start(Rules::Code, &bytes, 0, before).unwrap() as usize;
    assert!(start <= before as usize);
    assert!(matches!(bytes[start - 1], b';' | b'}'));
    assert!(
        !bytes[start..before as usize]
            .iter()
            .any(|b| matches!(b, b';' | b'}'))
    );
}

/// Nothing found in the bytes given: None, unless they start at the file's
/// start, which is always safe.
#[test]
fn window_safe_start_none_without_a_position() {
    let bytes = vec![b'x'; 70 << 10];
    assert_eq!(safe_start(Rules::Code, &bytes, 4096, 4096 + 60_000), None);
    assert_eq!(safe_start(Rules::Code, &bytes, 0, 60_000), Some(0));
    assert_eq!(safe_start(Rules::Json, &bytes, 4096, 4096 + 60_000), None);
    assert_eq!(safe_start(Rules::Markup, &bytes, 4096, 4096 + 60_000), None);
    assert_eq!(safe_start(Rules::Code, &bytes, 0, 0), Some(0));
}

#[test]
fn window_safe_start_json_members() {
    let text = br#"{"a":1,"bb":{"c":2},  "d" :[1,2],"e":"x"}"#;
    let at = |s: &str| {
        text.windows(s.len())
            .position(|w| w == s.as_bytes())
            .unwrap() as u64
    };
    let end = text.len() as u64;
    assert_eq!(
        safe_start(Rules::Json, text, 7, 7 + end),
        Some(7 + at("\"e\""))
    );
    // Its comma is at `before`, not before it.
    assert_eq!(
        safe_start(Rules::Json, text, 7, 7 + at("\"e\"") - 1),
        Some(7 + at("\"d\""))
    );
    // `,"c"` is not there: `{"c"` is the first member of its object.
    assert_eq!(
        safe_start(Rules::Json, text, 7, 7 + at("2}")),
        Some(7 + at("\"bb\""))
    );
    // Keys over 256 bytes and keys with a backslash are not taken.
    let long = format!(r#"{{"a":1,"b":2,"{}":3}}"#, "k".repeat(300));
    assert_eq!(
        safe_start(Rules::Json, long.as_bytes(), 1, 1 + long.len() as u64),
        Some(1 + 7)
    );
    let escaped = br#"{"a":1,"b\"c":2}"#;
    assert_eq!(
        safe_start(Rules::Json, escaped, 1, 1 + escaped.len() as u64),
        None
    );
    // A pretty-printed member: whitespace between the comma and the key.
    let pretty = b"{\n  \"a\": 1,\n  \"b\": 2\n}\n";
    assert_eq!(safe_start(Rules::Json, pretty, 1, 1 + 20), Some(1 + 14));
}

#[test]
fn window_safe_start_markup_after_blank_line() {
    let text = b"# Title\n\nSome text\nmore text\n   \n## Next\nbody\n";
    let at = |s: &str| {
        text.windows(s.len())
            .position(|w| w == s.as_bytes())
            .unwrap() as u64
    };
    assert_eq!(
        safe_start(Rules::Markup, text, 9, 9 + at("body")),
        Some(9 + at("## Next"))
    );
    assert_eq!(
        safe_start(Rules::Markup, text, 9, 9 + at("more")),
        Some(9 + at("Some"))
    );
    assert_eq!(safe_start(Rules::Markup, text, 9, 9 + 5), None);
}

/// Multi-line comments and template strings: the safe start never falls
/// inside one in the TypeScript shape (their continuation lines are
/// indented), checked against the whole file's tree.
#[test]
fn window_safe_start_skips_comments_and_template_strings() {
    let text = testing::ts(256 << 10, 3);
    let lang = lang("ts");
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(lang.language()).unwrap();
    let tree = parser.parse(&text, None).unwrap();
    let mut checked = 0;
    for before in (70_000..text.len() as u64).step_by(997) {
        let base = before - 64 * KIB;
        let bytes = &text[base as usize..];
        let start = safe_start(Rules::Code, bytes, base, before).expect("a start") as usize;
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(start, start + 1)
            .unwrap();
        loop {
            let inside = matches!(node.kind(), "comment" | "template_string" | "string")
                && node.start_byte() < start;
            assert!(
                !inside,
                "start {start} inside a {} at {}",
                node.kind(),
                node.start_byte()
            );
            match node.parent() {
                Some(parent) => node = parent,
                None => break,
            }
        }
        checked += 1;
    }
    assert!(checked > 100);
}

#[test]
fn window_json_pad_opens_and_closes_the_containers() {
    let pad = |s: &str, inside: bool| {
        let JsonPad { before, after } = json_pad(s.as_bytes(), inside);
        (
            String::from_utf8(before.into()).unwrap(),
            String::from_utf8(after.into()).unwrap(),
        )
    };
    let cases = [
        // (window, inside, before, after)
        (r#""a":1,"b":2"#, true, "{", "}"),
        (r#""a":1},"b":[1,{"c":2}],"d":3"#, true, r#"{"":{"#, "}"),
        (r#"1,2],"x":true}"#, true, r#"{"":["#, ""),
        (r#""a":{"b":[1,2"#, true, "{", "]}}"),
        (r#""a":"abc"#, true, "{", "\"}"),
        (r#"{"a":[1,2"#, false, "", "]}"),
        // An array's elements: no key at its level.
        (r#"3, 4, {"k": 5}"#, true, "[", "]"),
        ("", true, "", ""),
    ];
    let lang = lang("json");
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(lang.language()).unwrap();
    for (window, inside, before, after) in cases {
        assert_eq!(
            pad(window, inside),
            (before.into(), after.into()),
            "{window}"
        );
        let whole = format!("{before}{window}{after}");
        if !whole.is_empty() {
            let tree = parser.parse(&whole, None).unwrap();
            assert!(!tree.root_node().has_error(), "{whole}");
        }
    }
    // Brackets and commas inside strings do not count.
    assert_eq!(pad(r#""a":"}],\"","b":1"#, true), ("{".into(), "}".into()));
    assert_eq!(json_last_comma(br#""a":1,"b":"x,y""#), Some(5));
}

const ROWS: u32 = 30;

fn plan_whole(rules: Rules, text: &[u8], target: Target) -> Plan {
    let head = &text[..text.len().min(8 << 10)];
    plan(rules, head, text, 0, text.len() as u64, target, ROWS)
}

/// The start of every line.
fn line_starts(text: &[u8]) -> Vec<u64> {
    std::iter::once(0)
        .chain(
            memchr::memchr_iter(b'\n', text)
                .map(|i| i as u64 + 1)
                .filter(|&s| s < text.len() as u64),
        )
        .collect()
}

#[test]
fn window_plan_visible_lines() {
    let text = testing::rust(400 << 10, 0);
    let starts = line_starts(&text);
    let n = 5000;
    // A target in the middle of line `n`.
    let target = starts[n] + 3;
    let plan = plan_whole(Rules::Code, &text, Target::Byte(target));
    assert_eq!(plan.encoding, Encoding::Utf8);
    assert_eq!(plan.target_line_start, starts[n]);
    let before = (ROWS / 3) as usize;
    assert_eq!(plan.visible, starts[n - before]..starts[n + ROWS as usize]);
    let window = plan.window.clone().unwrap();
    assert!(window.start <= plan.visible.start - 8 * KIB);
    assert!(window.start >= plan.visible.start - 12 * KIB);
    assert!(starts.contains(&window.start), "a line start");
    assert_eq!(window.end, plan.visible.end + 4 * KIB);
    assert!(plan.json_pad.is_empty());
}

#[test]
fn window_plan_at_the_file_start_and_end() {
    let text = testing::ts(200 << 10, 0);
    let plan = plan_whole(Rules::Code, &text, Target::Byte(0));
    assert_eq!(plan.target_line_start, 0);
    assert_eq!(plan.visible.start, 0);
    assert_eq!(plan.window.unwrap().start, 0);
    let size = text.len() as u64;
    let plan = plan_whole(Rules::Code, &text, Target::Byte(size));
    assert_eq!(plan.visible.end, size);
    assert_eq!(plan.window.unwrap().end, size);
    let empty = plan_whole(Rules::Code, b"", Target::Byte(0));
    assert_eq!((empty.visible, empty.window), (0..0, Some(0..0)));
}

/// Lines of 3 KiB: `rows` of them would be 90 KiB, the visible part stops
/// at 64 KiB.
#[test]
fn window_plan_visible_at_most_64_kib() {
    let mut text = Vec::new();
    for i in 0..400 {
        text.extend(format!("x{i} = ").as_bytes());
        text.extend(std::iter::repeat_n(b'1', 3 << 10));
        text.extend(b";\n");
    }
    let plan = plan_whole(Rules::Code, &text, Target::Byte(text.len() as u64 / 2));
    assert!(plan.visible.end - plan.visible.start <= 64 * KIB);
    assert!(plan.visible.end - plan.visible.start > 60 * KIB);
}

/// A target in a line longer than 4 KiB: the line's start is not looked
/// for, the visible part is 8 KiB from the target, the window starts after
/// a `;` or `}`.
#[test]
fn window_plan_long_line() {
    let text = testing::minified_js(512 << 10, 0);
    let target = 300_000;
    let plan = plan_whole(Rules::Code, &text, Target::Byte(target));
    assert_eq!(plan.target_line_start, target);
    assert_eq!(plan.visible, target..target + 8 * KIB);
    let window = plan.window.unwrap();
    assert!(matches!(text[window.start as usize - 1], b';' | b'}'));
    assert!(window.start <= target - 8 * KIB);
}

#[test]
fn window_plan_line_from_a_checkpoint() {
    let text = testing::ts(2 << 20, 0);
    let starts = line_starts(&text);
    let size = text.len() as u64;
    // The checkpoint at 1 MiB: the number of line breaks before it.
    let c = MIB;
    let c_line = starts.iter().filter(|&&s| s <= c).count() as u32 - 1;
    let ranges = checkpoint_read(size, c);
    let around = ranges[1].clone();
    let head = &text[..8 << 10];
    for line in [c_line - 3, c_line, c_line + 1, c_line + 700] {
        let plan = plan(
            Rules::Code,
            head,
            &text[around.start as usize..around.end as usize],
            around.start,
            size,
            Target::LineFrom {
                checkpoint_byte: c,
                checkpoint_line: c_line,
                line,
            },
            ROWS,
        );
        assert_eq!(plan.target_line_start, starts[line as usize], "line {line}");
    }
}

#[test]
fn window_plan_encoding() {
    let mut text = b"\xEF\xBB\xBFfn main() {}\n".to_vec();
    let plan = plan_whole(Rules::Code, &text, Target::Byte(0));
    assert_eq!(plan.encoding, Encoding::Utf8Bom);
    assert_eq!(plan.window.unwrap().start, 0, "tree-sitter skips the BOM");
    // A UTF-8 sequence cut by the end of the bytes read is still text.
    text.extend("é".as_bytes());
    text.pop();
    assert_ne!(
        plan_whole(Rules::Code, &text, Target::Byte(0)).encoding,
        Encoding::NotText
    );
}

/// The plan only reads the two slices it is given: bytes outside them do
/// not change it.
#[test]
fn window_plan_looks_only_at_its_slices() {
    let text = testing::json(3 << 20, 0);
    let size = text.len() as u64;
    let target = size / 2 + 12_345;
    let ranges = first_read(size, target);
    let around = ranges[1].clone();
    let compute = |text: &[u8]| {
        plan(
            Rules::Json,
            &text[..8 << 10],
            &text[around.start as usize..around.end as usize],
            around.start,
            size,
            Target::Byte(target),
            ROWS,
        )
    };
    let first = compute(&text);
    let mut other = text.clone();
    for (i, b) in other.iter_mut().enumerate() {
        let i = i as u64;
        if !(i < 8 * KIB || (around.start..around.end).contains(&i)) {
            *b = 0;
        }
    }
    assert_eq!(compute(&other), first);
    assert!(first.window.is_some());
}

/// A JSON window ends after the last whole value at or past the visible
/// part, and its pad makes the window parse without errors.
#[test]
fn window_plan_json_trims_and_pads() {
    let text: Vec<u8> = testing::json(1 << 20, 0)
        .split(|&b| b == b'\n')
        .flat_map(|line| line.iter().skip_while(|b| **b == b' ').copied())
        .collect();
    let target = text.len() as u64 / 3;
    let plan = plan_whole(Rules::Json, &text, Target::Byte(target));
    let window = plan.window.clone().unwrap();
    assert!(window.end >= plan.visible.end);
    assert!(
        matches!(text[window.end as usize], b',' | b'}' | b']')
            || matches!(text[window.end as usize - 1], b'}' | b']')
    );
    let mut parsed = plan.json_pad.before.to_vec();
    parsed.extend(&text[window.start as usize..window.end as usize]);
    parsed.extend(&*plan.json_pad.after);
    let lang = lang("json");
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(lang.language()).unwrap();
    assert!(!parser.parse(&parsed, None).unwrap().root_node().has_error());
}

/// Code windows end where their tree confirms a top-level boundary; JSON
/// windows end after a comma.
#[test]
fn window_chain_cuts() {
    let text = testing::ts(600 << 10, 0);
    let windows = super::chain(lang("ts"), &text);
    for w in &windows[..windows.len() - 1] {
        let end = w.range().end as usize;
        // After a block, a statement or a comment.
        assert!(
            matches!(text[end - 1], b'}' | b';' | b'/'),
            "{:?}",
            w.range()
        );
        assert!(w.range().end - w.range().start > 32 * KIB);
    }
    let text = testing::json(600 << 10, 0);
    for w in &super::chain(lang("json"), &text) {
        let end = w.range().end as usize;
        assert!(
            end == text.len() || text[end - 1] == b',',
            "{:?}",
            w.range()
        );
    }
}

/// A window with no complete node before its end (inside a 100 KiB string)
/// is widened to 256 KiB and cut by the tree there.
#[test]
fn window_chain_widens() {
    let mut text = b"var s = \"".to_vec();
    text.extend(std::iter::repeat_n(b'x', 100 << 10));
    text.extend(b"\";\n");
    text.extend(testing::minified_js(300 << 10, 0));
    let windows = super::chain(lang("js"), &text);
    let first = windows[0].range();
    assert!(first.end > 64 * KIB && first.end <= 256 * KIB, "{first:?}");
    // The string's token is whole in the first window.
    let string = 8..(8 + (100 << 10) + 2) as u64;
    let kinds = super::kinds(&[&windows[0]], string.clone());
    assert!(kinds.iter().all(|k| *k == Some(crate::TokenKind::String)));
}

/// No complete node in the widened window: after its last token (`var`);
/// no token either (an identifier over 256 KiB): at the first identifier
/// boundary after the window.
#[test]
fn window_chain_cuts_after_a_token_or_an_identifier() {
    let mut text = b"var ".to_vec();
    text.extend(std::iter::repeat_n(b'x', 300 << 10));
    text.extend(b" = 1;\nvar y = 2;\n");
    assert_eq!(super::chain(lang("js"), &text)[0].range(), 0..3);
    let mut text = std::iter::repeat_n(b'x', 300 << 10).collect::<Vec<u8>>();
    let boundary = text.len() as u64;
    text.extend(b" = 1;\nvar y = 2;\n");
    assert_eq!(super::chain(lang("js"), &text)[0].range(), 0..boundary);
}

/// The plans of each shape at a few targets: where the window starts and
/// what the visible part opens with.
#[test]
fn window_plan_snapshot() {
    let one_line_json: Vec<u8> = testing::json(300 << 10, 0)
        .split(|&b| b == b'\n')
        .flat_map(|line| line.iter().skip_while(|b| **b == b' ').copied())
        .collect();
    let shapes: [(&str, Rules, Vec<u8>); 5] = [
        ("ts", Rules::Code, testing::ts(300 << 10, 0)),
        ("rust", Rules::Code, testing::rust(300 << 10, 0)),
        ("json", Rules::Json, testing::json(300 << 10, 0)),
        ("json one line", Rules::Json, one_line_json),
        (
            "minified js",
            Rules::Code,
            testing::minified_js(300 << 10, 0),
        ),
    ];
    let mut out = String::new();
    for (name, rules, text) in &shapes {
        writeln!(out, "## {name} ({} bytes)", text.len()).unwrap();
        for target in [100_000u64, 200_017] {
            let plan = plan_whole(*rules, text, Target::Byte(target));
            let show = |r: &Range<u64>| {
                let s = r.start as usize;
                String::from_utf8_lossy(&text[s..(s + 40).min(r.end as usize)]).replace('\n', "⏎")
            };
            writeln!(
                out,
                "target {target}: line start {}",
                plan.target_line_start
            )
            .unwrap();
            writeln!(
                out,
                "  visible {:?}: {:?}",
                plan.visible,
                show(&plan.visible)
            )
            .unwrap();
            match &plan.window {
                Some(w) => writeln!(out, "  window {w:?}: {:?}", show(w)).unwrap(),
                None => writeln!(out, "  window None").unwrap(),
            }
            if !plan.json_pad.is_empty() {
                writeln!(
                    out,
                    "  pad {:?} … {:?}",
                    String::from_utf8_lossy(&plan.json_pad.before),
                    String::from_utf8_lossy(&plan.json_pad.after)
                )
                .unwrap();
            }
        }
    }
    insta::assert_snapshot!(out);
}
