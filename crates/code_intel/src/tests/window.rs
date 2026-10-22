//! `window_*` (「大文件」, ADR 0034): the cut of a window parsed alone, the
//! window chain of an opened 1–16 MiB file (its units, its `Windowed`
//! marker, resuming, its identity, the streamed OID and the CRLF fallback,
//! the racy-timestamp delay), and what queries make of its units.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use lectern_services::code_intel::{CodeIntelEvent, Names, QueryKind};
use lectern_services::project::BlobId;

use super::extract::test_watchdog;
use super::support::Harness;
use crate::extract::{
    CutRule, Deadline, ExtractStatus, Extraction, LanguageTable, Windowed, extract, extract_window,
};
use crate::index::chain::{HASH_RANGE, racy_wait, resume_at};
use crate::store::{Part, Version, WindowRow};
use crate::window::{Base, WINDOW, ident, leaf_cut, tree_cut};

/// The core languages: (extension, language).
const LANGS: [(&str, &str); 7] = [
    ("ts", "ts"),
    ("tsx", "tsx"),
    ("js", "js"),
    ("rs", "rust"),
    ("java", "java"),
    ("py", "python"),
    ("go", "go"),
];

/// One top-level unit `k` of a synthetic file: a function and a type with
/// a member, small enough that no top-level node is longer than a window.
fn unit(ext: &str, k: usize) -> String {
    match ext {
        "ts" | "tsx" => format!(
            "export function build_{k}(value: number): number {{\n  return helper_{k}(value) + {k};\n}}\n\n\
             export class Box_{k} {{\n  size = {k};\n  get(): number {{\n    return this.size;\n  }}\n}}\n\n"
        ),
        "js" => format!(
            "export function build_{k}(value) {{\n  return helper_{k}(value) + {k};\n}}\n\n\
             export class Box_{k} {{\n  get() {{\n    return size_{k};\n  }}\n}}\n\n"
        ),
        "rs" => format!(
            "pub fn build_{k}(value: u32) -> u32 {{\n    helper_{k}(value) + {k}\n}}\n\n\
             pub struct Box{k} {{\n    size: u32,\n}}\n\n\
             impl Box{k} {{\n    pub fn get(&self) -> u32 {{\n        self.size\n    }}\n}}\n\n"
        ),
        "java" => format!(
            "class Box{k} {{\n  int size{k};\n  int get(int a) {{\n    return helper{k}(a);\n  }}\n}}\n\n"
        ),
        "py" => format!(
            "def build_{k}(value):\n    return helper_{k}(value)\n\n\n\
             class Box_{k}:\n    size = {k}\n\n    def get(self):\n        return self.size\n\n\n"
        ),
        "go" => format!(
            "func Build{k}(value int) int {{\n\treturn helper{k}(value)\n}}\n\n\
             type Box{k} struct {{\n\tsize int\n}}\n\n\
             func (b *Box{k}) Get() int {{\n\treturn b.size\n}}\n\n"
        ),
        _ => unreachable!("a core language"),
    }
}

/// At least `len` bytes of `ext` made of [`unit`]s.
pub(super) fn big(ext: &str, len: usize) -> String {
    let mut out = String::with_capacity(len + 1024);
    if ext == "go" {
        out.push_str("package main\n\n");
    }
    let mut k = 0;
    while out.len() < len {
        out.push_str(&unit(ext, k));
        k += 1;
    }
    out
}

/// A function wrapping everything (an IIFE), whose body is `const
/// <name> = (a) => <call>(a, "<filler>");` statements of lengths a fixed
/// seed varies, so that many windows end inside an identifier.
fn iife(len: usize, one_line: bool, umd: bool) -> String {
    let mut seed = 0x9e37_79b9_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let nl = if one_line { "" } else { "\n" };
    let mut out = if umd {
        format!(
            "(function (root, factory) {{{nl}  if (typeof define === 'function') {{ define([], factory); }}\
             {nl}  else {{ root.lib = factory(); }}{nl}}})(this, function () {{{nl}"
        )
    } else {
        format!("(function () {{{nl}")
    };
    let mut k = 0;
    while out.len() < len {
        let pad = "x".repeat((next() % 40) as usize);
        let name = format!("fn_{k}_{}", "q".repeat((next() % 24) as usize));
        let call = format!("call_{k}_{}", "r".repeat((next() % 24) as usize));
        out.push_str(&format!(
            "  const {name} = (a) => {call}(a, \"{pad}\");{nl}"
        ));
        k += 1;
    }
    out.push_str(if umd { "});\n" } else { "})();\n" });
    out
}

fn lang(name: &str) -> crate::extract::Lang {
    LanguageTable::core().get(name).expect("core").clone()
}

/// The whole file's extraction, without a size limit.
fn whole(lang_name: &str, text: &[u8]) -> Extraction {
    let watchdog = test_watchdog();
    let deadline = Deadline::at(
        &watchdog,
        watchdog.clock().now() + Duration::from_secs(600),
        64 << 20,
    );
    let e = extract(
        &lang(lang_name),
        text,
        &deadline,
        &Arc::new(AtomicBool::new(false)),
    );
    watchdog.stop();
    assert_eq!(e.status, ExtractStatus::Done);
    e
}

/// Opens `path` (as the current text) in a new harness with `text` and
/// runs everything.
pub(super) fn opened(path: &str, text: &[u8]) -> Harness {
    let h = Harness::new();
    h.git.add("main", &[(path, text)]);
    h.git.commit("main");
    h.open();
    h.current(path);
    h.settle();
    h
}

/// The window units of `oid`, by start, with their extractions.
fn windows(h: &Harness, oid: BlobId, lang_name: &str) -> Vec<(WindowRow, Extraction)> {
    let store = h.indexer.store(&h.key()).expect("open");
    let version = Version::of(&lang(lang_name));
    store
        .windows(oid, &version)
        .expect("read")
        .into_iter()
        .map(|row| {
            let e = store.load(row.unit).expect("stored");
            (row, e)
        })
        .collect()
}

/// The stored status of the whole file's unit.
fn marker(h: &Harness, oid: BlobId, lang_name: &str) -> Option<ExtractStatus> {
    let store = h.indexer.store(&h.key()).expect("open");
    let version = Version::of(&lang(lang_name));
    store
        .exists(&version, &[oid])
        .expect("read")
        .first()
        .map(|f| f.status)
}

/// The windows chain from 0 to `len`, each start the previous end.
fn assert_chained(rows: &[(WindowRow, Extraction)], len: u64) {
    let mut at = 0;
    for (row, _) in rows {
        assert_eq!(row.part.start, at, "a gap or an overlap at {at}");
        assert!(row.part.end > row.part.start);
        at = row.part.end;
    }
    assert_eq!(at, len);
}

/// No window boundary inside an identifier.
fn assert_no_split(text: &[u8], rows: &[(WindowRow, Extraction)]) {
    for (row, _) in rows {
        let at = row.part.end as usize;
        if at > 0 && at < text.len() {
            assert!(
                !(ident(text[at - 1]) && ident(text[at])),
                "a cut inside an identifier at {at}: {:?}",
                String::from_utf8_lossy(&text[at.saturating_sub(20)..(at + 20).min(text.len())])
            );
        }
    }
}

#[test]
fn window_chain_equals_whole_extraction() {
    for (ext, name) in LANGS {
        let path = format!("big.{ext}");
        // Just over the 1 MiB where windows start: some 17 windows each.
        let text = big(ext, 1_100_000);
        let h = opened(&path, text.as_bytes());
        let oid = h.git.oid(text.as_bytes());
        assert_eq!(
            marker(&h, oid, name),
            Some(ExtractStatus::Windowed),
            "{ext}"
        );
        assert!(h.hinted(&path), "{ext}: indexed once the chain is complete");
        let rows = windows(&h, oid, name);
        assert_chained(&rows, text.len() as u64);
        assert!(rows.len() > 15, "{ext}: {} windows", rows.len());
        assert!(rows.iter().all(|(r, _)| r.canonical));
        let reference = whole(name, text.as_bytes());
        let defs: Vec<_> = rows.iter().flat_map(|(_, e)| e.defs.clone()).collect();
        // Every definition the same, its line, column and byte offset in
        // the whole file.
        let missing: Vec<_> = reference
            .defs
            .iter()
            .filter(|d| !defs.contains(d))
            .take(3)
            .collect();
        let extra: Vec<_> = defs
            .iter()
            .filter(|d| !reference.defs.contains(d))
            .take(3)
            .collect();
        let around: Vec<String> = missing
            .first()
            .map(|d| {
                rows.iter()
                    .filter(|(r, _)| r.part.end + 200 > d.byte && r.part.start < d.byte + 200)
                    .map(|(r, _)| {
                        let (s, e) = (r.part.start as usize, r.part.end as usize);
                        format!(
                            "[{s}, {e}) …{:?}|{:?}…",
                            &text[e.saturating_sub(30)..e],
                            &text[e..(e + 30).min(text.len())]
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{ext}: missing {missing:?}, extra {extra:?}, windows {around:?}"
        );
        assert_eq!(defs.len(), reference.defs.len(), "{ext}");
        for (got, want) in defs.iter().zip(&reference.defs) {
            assert_eq!(got, want, "{ext}");
        }
        let refs: BTreeSet<_> = rows.iter().flat_map(|(_, e)| e.refs.clone()).collect();
        let want: BTreeSet<_> = reference.refs.iter().cloned().collect();
        assert_eq!(refs, want, "{ext}");
        assert_no_split(text.as_bytes(), &rows);
    }
}

#[test]
fn window_chain_equals_whole_extraction_in_an_iife() {
    for (one_line, umd) in [(false, false), (false, true)] {
        let text = iife(1_200_000, one_line, umd);
        let h = opened("bundle.js", text.as_bytes());
        let oid = h.git.oid(text.as_bytes());
        assert_eq!(marker(&h, oid, "js"), Some(ExtractStatus::Windowed));
        let rows = windows(&h, oid, "js");
        assert_chained(&rows, text.len() as u64);
        let reference = whole("js", text.as_bytes());
        let count = |names: Vec<Box<str>>| {
            let mut m: BTreeMap<Box<str>, usize> = BTreeMap::new();
            for n in names {
                *m.entry(n).or_default() += 1;
            }
            m
        };
        let defs = count(
            rows.iter()
                .flat_map(|(_, e)| e.defs.iter().map(|d| d.name.clone()))
                .collect(),
        );
        let want = count(reference.defs.iter().map(|d| d.name.clone()).collect());
        assert_eq!(defs, want, "one_line {one_line}, umd {umd}");
        // The names are unique per statement: the references too.
        let refs = count(rows.iter().flat_map(|(_, e)| e.refs.clone()).collect());
        let want = count(reference.refs.clone());
        assert_eq!(refs, want, "one_line {one_line}, umd {umd}");
        assert_no_split(text.as_bytes(), &rows);
    }
}

#[test]
fn window_cut_skips_a_node_ending_at_the_window_end() {
    let js = lang("js");
    let text = b"const a = 1;\nconst foo = 1;\n";
    // The window ends inside `foo`: `const fo` parses as a complete
    // declaration ending at the window's end, and `fo` as an identifier.
    let end = text.windows(6).position(|w| w == b"st foo").unwrap() + 5;
    let window = &text[..end];
    assert!(window.ends_with(b"const fo"));
    let watchdog = test_watchdog();
    let deadline = Deadline::interactive(&watchdog);
    let cancel = Arc::new(AtomicBool::new(false));
    match extract_window(&js, window, CutRule::Tree, &deadline, &cancel) {
        Windowed::Cut(e, cut) => {
            assert_eq!(
                &window[..cut],
                b"const a = 1;",
                "cut at the previous statement"
            );
            let names: Vec<&str> = e.defs.iter().map(|d| &*d.name).collect();
            assert_eq!(names, ["a"], "no `fo`");
        }
        _ => panic!("a cut"),
    }
    // The leaf fallback: `bet` ends at the window's end, `,` before it.
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(js.language()).unwrap();
    let call = b"call(alpha, bet";
    let tree = parser.parse(call, None).unwrap();
    assert_eq!(leaf_cut(&tree, call.len()), Some(b"call(alpha,".len()));
    // A tree whose only top-level node is truncated, with nothing complete
    // inside it either: no tree cut.
    let one = b"const veryLongName";
    let tree = parser.parse(one, None).unwrap();
    assert_eq!(tree_cut(&tree, one), None);
    // An open construct the parser cannot close (the root is `ERROR`):
    // its name and parameters are pieces on the stack, complete by
    // themselves; the cut is the last statement before them.
    let open = b"export function a() {}\nexport class B {\n  m(x) { return x; }\n  n(): number";
    let tree = parser.parse(&open[..], None).unwrap();
    let cut = tree_cut(&tree, open).expect("a cut");
    assert_eq!(&open[..cut], b"export function a() {}");
    watchdog.stop();
}

#[test]
fn window_cut_descends_into_the_truncated_node() {
    for (one_line, umd) in [(false, false), (false, true), (true, false)] {
        let text = iife(1_200_000, one_line, umd);
        let h = opened("bundle.js", text.as_bytes());
        let rows = windows(&h, h.git.oid(text.as_bytes()), "js");
        // Cut inside the function's body: no window widened past 64 KiB.
        for (row, _) in &rows {
            assert!(
                row.part.end - row.part.start <= WINDOW as u64,
                "one_line {one_line}, umd {umd}: a window of {} bytes at {}",
                row.part.end - row.part.start,
                row.part.start
            );
        }
        // Past the first few windows, every window is nearly a full one.
        let short = rows
            .iter()
            .filter(|(r, _)| r.part.end - r.part.start < (WINDOW / 2) as u64)
            .count();
        assert!(short <= 4, "{short} short windows of {}", rows.len());
    }
}

#[test]
fn window_cut_never_splits_an_identifier() {
    // One string literal over 256 KiB: no complete node, the leaf or the
    // identifier boundary cuts it.
    let mut text = String::from("const head = 1;\nconst big = \"");
    let mut k = 0;
    while text.len() < 1_300_000 {
        text.push_str(&format!("word{k} "));
        k += 1;
    }
    text.push_str("\";\nexport function tail() { return big; }\n");
    let h = opened("strings.js", text.as_bytes());
    let oid = h.git.oid(text.as_bytes());
    let rows = windows(&h, oid, "js");
    assert_chained(&rows, text.len() as u64);
    assert_no_split(text.as_bytes(), &rows);
    assert_eq!(marker(&h, oid, "js"), Some(ExtractStatus::Windowed));
}

#[test]
fn window_byte_offsets_are_in_the_whole_file() {
    let text = big("ts", 1_500_000);
    let h = opened("big.ts", text.as_bytes());
    let rows = windows(&h, h.git.oid(text.as_bytes()), "ts");
    for (row, e) in &rows {
        // Each window's start: its line and column in the whole file.
        let before = &text.as_bytes()[..row.part.start as usize];
        let line = before.iter().filter(|b| **b == b'\n').count() as u32;
        let column = before.len()
            - before
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(0, |i| i + 1);
        assert_eq!((row.part.line, row.part.column), (line, column as u32));
        for d in &e.defs {
            let at = d.byte as usize;
            assert!(text.as_bytes()[at..].starts_with(d.name.as_bytes()));
            assert!(d.byte >= row.part.start && d.byte < row.part.end);
        }
    }
    // A jump from another file lands on the window unit's definition, its
    // byte offset that of the name.
    let main = "import { build_7 } from './big';\nexport const v = build_7(1);\n";
    h.git.add("main", &[("main.ts", main.as_bytes())]);
    h.git.commit("main");
    h.refresh_files();
    let r = h.goto("main.ts", "build_7", 1);
    let c = r
        .candidates
        .iter()
        .find(|c| &*c.definition.location.path == "big.ts")
        .expect("the large file's definition");
    let at = c.definition.location.byte.expect("canonical") as usize;
    assert!(text.as_bytes()[at..].starts_with(b"build_7("));
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    assert_eq!(c.definition.location.column as usize, at - line_start);
}

#[test]
fn window_marker_replace_is_own_eviction() {
    let text = big("ts", 1_200_000);
    let h = Harness::new();
    h.git.add(
        "main",
        &[
            ("big.ts", text.as_bytes()),
            ("small.ts", b"export function small() {}\n"),
        ],
    );
    h.git.commit("main");
    h.open();
    let oid = h.git.oid(text.as_bytes());
    // The batch tiers stored the file `TooLarge`.
    assert_eq!(marker(&h, oid, "ts"), Some(ExtractStatus::TooLarge));
    assert!(h.hinted("small.ts"));
    let cursors = h.stats().cursor_builds;
    h.current("big.ts");
    h.settle();
    assert_eq!(marker(&h, oid, "ts"), Some(ExtractStatus::Windowed));
    // The replace was this crate's own: no hint fell, no walk restarted.
    assert!(h.hinted("small.ts") && h.hinted("big.ts"));
    assert_eq!(h.stats().cursor_builds, cursors);
}

#[test]
fn window_units_are_in_names() {
    let text = big("ts", 1_200_000);
    let h = opened("big.ts", text.as_bytes());
    assert!(matches!(h.indexer.names(&h.key()), Names::Building));
    h.settle();
    let table = h
        .events()
        .into_iter()
        .rev()
        .find_map(|e| match e {
            CodeIntelEvent::Names { table, .. } => Some(table),
            _ => None,
        })
        .expect("a Names event");
    for name in ["build_0", "build_100", "Box_120"] {
        assert!(table.iter().any(|n| n == name), "{name}");
    }
    // `Named` finds them, as any indexed definition.
    let r = h.ask(QueryKind::Named {
        name: "build_100".into(),
        near: None,
    });
    assert_eq!(r.candidates.len(), 1);
}

/// Every existence check that hits a `Windowed` marker marks its window
/// units used with it (「大文件」「淘汰」): the warm start's batch check,
/// tier 1's when the file is opened again, a jump in it. Otherwise the
/// windows would age while the marker stays, eviction would take them
/// first, and the file would fall back to `TooLarge`
/// (`store_marker_touch_keeps_its_windows` shows the eviction).
#[test]
fn window_marker_is_touched_with_its_windows() {
    let text = big("ts", 1_200_000);
    let h = opened("big.ts", text.as_bytes());
    let oid = h.git.oid(text.as_bytes());
    let n = windows(&h, oid, "ts").len();
    assert!(n > 10);
    // The next run: the batch tiers' check hits the marker.
    let h = h.reopen(Default::default());
    h.open();
    h.settle();
    assert_eq!(h.stats().touched, n + 1, "the marker and its windows");
    // Opened again: tier 1's check.
    let before = h.stats().touched;
    h.current("big.ts");
    h.settle();
    assert_eq!(h.stats().touched - before, n + 1);
    // A jump in it: with the query's units.
    let before = h.stats().touched;
    let at = h.text("big.ts");
    let mut position = super::support::pos_in(text.as_bytes(), "value) + 100;", 0);
    position.column += 1;
    let r = h.ask(QueryKind::Definition { at, position });
    assert!(!r.candidates.is_empty());
    assert!(h.stats().touched - before > n);
    assert_eq!(marker(&h, oid, "ts"), Some(ExtractStatus::Windowed));
}

#[test]
fn window_chain_resumes_after_pause() {
    let text = big("ts", 1_200_000);
    let h = Harness::new();
    h.git.add("main", &[("big.ts", text.as_bytes())]);
    h.git.commit("main");
    h.open();
    // A pause after the third window: the chain stops at its next
    // boundary, the windows written stay.
    let written = Arc::new(Mutex::new(0usize));
    let kernel = h.host.kernel();
    {
        let written = written.clone();
        *h.indexer.inner().window_hook.lock().unwrap() = Some(Box::new(move |_| {
            let mut n = written.lock().unwrap();
            *n += 1;
            if *n == 3 {
                kernel.pause();
            }
        }));
    }
    h.current("big.ts");
    h.settle();
    let oid = h.git.oid(text.as_bytes());
    assert_eq!(windows(&h, oid, "ts").len(), 3);
    // Not indexed yet: its whole unit is still `TooLarge`.
    assert_eq!(marker(&h, oid, "ts"), Some(ExtractStatus::TooLarge));
    let before = h.stats().windows;
    h.resume();
    h.settle();
    let rows = windows(&h, oid, "ts");
    assert_chained(&rows, text.len() as u64);
    assert_eq!(marker(&h, oid, "ts"), Some(ExtractStatus::Windowed));
    // Only the windows after the third were extracted again.
    assert_eq!(h.stats().windows - before, rows.len() - 3);
}

#[test]
fn window_chain_fills_evicted_windows() {
    let text = big("ts", 1_200_000);
    let h = opened("big.ts", text.as_bytes());
    let oid = h.git.oid(text.as_bytes());
    let rows = windows(&h, oid, "ts");
    let gone = rows[5].0.part.start;
    let store = h.indexer.store(&h.key()).expect("open");
    let mut w = store.write();
    w.remove_window(oid, &Version::of(&lang("ts")), gone);
    assert_eq!(w.commit(), lectern_cache::Written::Committed);
    // The marker no longer holds the file: opened again, it resumes there.
    assert_eq!(marker(&h, oid, "ts"), Some(ExtractStatus::TooLarge));
    let h = h.reopen(Default::default());
    h.open();
    let before = h.stats().windows;
    h.current("big.ts");
    h.settle();
    assert_eq!(h.stats().windows - before, 1, "only the missing window");
    let again = windows(&h, oid, "ts");
    assert_chained(&again, text.len() as u64);
    assert_eq!(again.len(), rows.len());
    assert_eq!(again[5].1, rows[5].1);
    assert_eq!(marker(&h, oid, "ts"), Some(ExtractStatus::Windowed));
}

#[test]
fn window_chain_aborts_on_identity_change() {
    let text = big("ts", 1_200_000);
    let h = Harness::new();
    h.git.add("main", &[("big.ts", text.as_bytes())]);
    h.git.commit("main");
    h.open();
    let changed = big("ts", 1_300_000);
    {
        let git = h.git.clone();
        let changed = changed.clone();
        let mut done = false;
        *h.indexer.inner().window_hook.lock().unwrap() = Some(Box::new(move |_| {
            if !done {
                done = true;
                git.write("main", "big.ts", changed.as_bytes());
            }
        }));
    }
    h.current("big.ts");
    h.settle();
    let oid = h.git.oid(text.as_bytes());
    // One window under the old OID, then the chain gave up.
    assert_eq!(windows(&h, oid, "ts").len(), 1);
    assert_eq!(marker(&h, oid, "ts"), Some(ExtractStatus::TooLarge));
    assert!(windows(&h, h.git.oid(changed.as_bytes()), "ts").is_empty());
}

#[test]
fn window_streamed_oid_matches_for_blob() {
    // Across several 1 MiB ranges, one ending on a range's border.
    for len in [
        HASH_RANGE as usize + 1_000,
        3 * HASH_RANGE as usize,
        2 * HASH_RANGE as usize + 7,
    ] {
        let text = big("ts", len);
        let text = &text.as_bytes()[..len.min(text.len())];
        let h = opened("big.ts", text);
        let oid = h.git.oid(text);
        // The chain went on only because the streamed OID was the file's.
        assert_eq!(
            marker(&h, oid, "ts"),
            Some(ExtractStatus::Windowed),
            "{len}"
        );
        assert_eq!(h.git.count("canonical_oid"), 0, "no conversion for {len}");
    }
}

#[test]
fn window_chain_converts_a_crlf_checkout() {
    let text = big("ts", 1_200_000);
    let crlf = text.replace('\n', "\r\n");
    let h = Harness::new();
    h.git.add("main", &[("big.ts", text.as_bytes())]);
    h.git.commit("main");
    h.git.eol("big.ts");
    h.git.write("main", "big.ts", crlf.as_bytes());
    // A checkout: git's stat cache takes the CRLF file as unchanged, so the
    // map has the index's OID (a re-read would find it over 1 MiB and mark
    // it unindexable).
    h.git.assume_unchanged(true);
    // The current text's own warming reads it too (not counted here).
    h.indexer
        .inner()
        .no_warm
        .store(true, std::sync::atomic::Ordering::Release);
    h.open();
    let oid = h.git.oid(text.as_bytes());
    let calls = h.git.count("canonical_oid");
    h.current("big.ts");
    h.settle();
    // The streamed OID is the CRLF bytes': the whole file is read once and
    // converted.
    assert_eq!(h.git.count("canonical_oid"), calls + 1);
    assert_eq!(marker(&h, oid, "ts"), Some(ExtractStatus::Windowed));
    let rows = windows(&h, oid, "ts");
    assert_chained(&rows, crlf.len() as u64);
    assert!(rows.iter().all(|(r, _)| !r.canonical));
    // Lines are right; byte offsets are of the CRLF bytes, so none goes to
    // a view.
    let main = "import { build_9 } from './big';\nexport const v = build_9(1);\n";
    h.git.add("main", &[("main.ts", main.as_bytes())]);
    h.git.commit("main");
    h.refresh_files();
    let r = h.goto("main.ts", "build_9", 1);
    let c = r
        .candidates
        .iter()
        .find(|c| &*c.definition.location.path == "big.ts")
        .expect("the large file's definition");
    assert_eq!(c.definition.location.byte, None);
    let line = text
        .lines()
        .position(|l| l.contains("function build_9("))
        .unwrap();
    assert_eq!(c.definition.location.line as usize, line);
}

#[test]
fn window_chain_waits_out_a_racy_timestamp() {
    let text = big("ts", 1_200_000);
    let h = Harness::new();
    h.git.add("main", &[("big.ts", text.as_bytes())]);
    h.git.commit("main");
    h.open();
    // The wall clock is the file's status change plus one second.
    let meta = std::fs::metadata(h.git.root("main").join("big.ts")).unwrap();
    let ctime = {
        use std::os::unix::fs::MetadataExt;
        SystemTime::UNIX_EPOCH + Duration::new(meta.ctime() as u64, meta.ctime_nsec() as u32)
    };
    h.indexer
        .set_wall_clock(move || ctime + Duration::from_secs(1));
    h.current("big.ts");
    h.settle();
    let oid = h.git.oid(text.as_bytes());
    assert!(windows(&h, oid, "ts").is_empty(), "nothing read yet");
    assert_eq!(h.stats().windows, 0);
    // Two seconds later on the watchdog's clock the chain starts.
    h.indexer
        .set_wall_clock(move || ctime + Duration::from_secs(3));
    h.advance_and_fire(Duration::from_secs(2));
    assert_eq!(marker(&h, oid, "ts"), Some(ExtractStatus::Windowed));
}

#[test]
fn window_racy_and_resume_rules() {
    let at = |secs: i64| lectern_platform::Timestamp { secs, nanos: 0 };
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    assert_eq!(racy_wait(now, at(990)), None);
    assert_eq!(racy_wait(now, at(998)), None);
    assert_eq!(racy_wait(now, at(999)), Some(Duration::from_secs(1)));
    assert_eq!(racy_wait(now, at(1005)), Some(Duration::from_secs(2)));
    let row = |start: u64, end: u64, status| WindowRow {
        unit: lectern_cache::Unit::from_id(start as i64 + 1),
        part: Part {
            start,
            end,
            line: 0,
            column: 0,
        },
        status,
        canonical: true,
    };
    let done = ExtractStatus::Done;
    let rows = [row(0, 10, done), row(10, 20, done), row(30, 40, done)];
    let (at, prev, timed_out) = resume_at(&rows, 0);
    assert_eq!(
        (at, prev.map(|r| r.part.end), timed_out),
        (20, Some(20), false)
    );
    assert_eq!(resume_at(&rows, 30).0, 40);
    let rows = [row(0, 10, ExtractStatus::TimedOut), row(10, 20, done)];
    assert_eq!(resume_at(&rows, 0), (20, Some(rows[1]), true));
    // A base after a window: its newlines, and the column past the last.
    let base = Base {
        byte: 100,
        line: 4,
        column: 7,
    };
    assert_eq!(
        base.after(b"abc"),
        Base {
            byte: 103,
            line: 4,
            column: 10
        }
    );
    assert_eq!(
        base.after(b"a\nbc\nde"),
        Base {
            byte: 107,
            line: 6,
            column: 2
        }
    );
}

#[test]
fn window_cut_ignores_a_last_piece_ended_by_the_window() {
    // One minified line: the window ends after `return ` inside
    // `run:function(x){return f2(x,2)}`. Alone, `return` is a complete
    // statement (an automatic semicolon) ending one byte before the
    // window's end, the last child of the `ERROR` root with nothing after
    // it on its line. Cutting there would split `o2`'s object literal
    // across windows: the cut is the last `;` before it.
    let js = lang("js");
    let mut text = String::from("!function(e,t){\"use strict\";");
    for i in 0..3 {
        text.push_str(&format!(
            "function f{i}(a,b){{var c=a+b*{i},d=[1,2,c];if(c>10&&d.length){{return c*2}}\
             for(var k=0;k<d.length;k++){{c+=d[k]%3?d[k]:-d[k]}}\
             return\"v\"+c+'s'}}var o{i}={{key:\"k{i}\",run:function(x){{return f{i}(x,{i})}},n:{i}.5}};"
        ));
    }
    let end = text.rfind("{return f2").unwrap() + "{return ".len();
    let window = &text.as_bytes()[..end];
    let o1 = text.find("n:1.5};").unwrap() + "n:1.5};".len();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(js.language()).unwrap();
    let tree = parser.parse(window, None).unwrap();
    assert!(tree.root_node().is_error());
    assert_eq!(tree_cut(&tree, window), Some(o1), "after `var o1={{…}};`");
    // The same window with a line break after `return `: the line break
    // ends it as a statement.
    let broken = [window, b"\n"].concat();
    let tree = parser.parse(&broken[..], None).unwrap();
    assert_eq!(tree_cut(&tree, &broken), Some(end - 1));
    let watchdog = test_watchdog();
    let deadline = Deadline::interactive(&watchdog);
    let cancel = Arc::new(AtomicBool::new(false));
    match extract_window(&js, window, CutRule::Tree, &deadline, &cancel) {
        Windowed::Cut(e, cut) => {
            assert_eq!(cut, o1);
            // `f2` and `o2`'s `run` are the next window's.
            let names: Vec<&str> = e.defs.iter().map(|d| &*d.name).collect();
            assert_eq!(names, ["f0", "run", "f1", "run"]);
        }
        _ => panic!("a cut"),
    }
    watchdog.stop();
}
