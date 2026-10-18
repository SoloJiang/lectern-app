//! `extract_*`: definitions and reference names of the language fixtures,
//! statuses, and the key's versions.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_executor::Executor;

use crate::extract::{Deadline, ExtractStatus, Extraction, LanguageTable, Visibility, extract};
use crate::lang::{Dialect, EXTRACTOR_VERSION, query_version, query_version_with};
use crate::rules::count_params;
use crate::testing::{FIXTURES, ManualClock, watchdog};
use crate::{DefKind, SystemClock, Watchdog};

pub(super) fn test_watchdog() -> Watchdog {
    let executor = Executor::manual();
    watchdog(&executor, Arc::new(SystemClock)).0
}

pub(super) fn run(lang: &str, source: &str) -> Extraction {
    let table = LanguageTable::core();
    let lang = table.get(lang).expect("core language");
    let watchdog = test_watchdog();
    let extraction = extract(
        lang,
        source.as_bytes(),
        &Deadline::interactive(&watchdog),
        &Arc::new(AtomicBool::new(false)),
    );
    watchdog.stop();
    extraction
}

/// One line per definition, then the reference names.
fn render(extraction: &Extraction) -> String {
    let mut out = format!("status: {:?}\ndefs:\n", extraction.status);
    for d in &extraction.defs {
        let _ = write!(
            out,
            "  {}:{}-{} {} {}",
            d.line + 1,
            d.column,
            d.end_line + 1,
            crate::def_kind_name(d.kind),
            d.name
        );
        if let Some(scope) = &d.scope {
            let _ = write!(out, " in {scope}");
        }
        let _ = write!(out, " [{}]", d.visibility.as_str());
        if let Some(arity) = d.arity {
            let _ = write!(out, " ({arity})");
        }
        out.push('\n');
    }
    out.push_str("refs:");
    for r in &extraction.refs {
        out.push(' ');
        out.push_str(r);
    }
    out.push('\n');
    out
}

#[test]
fn extract_fixtures_snapshot() {
    for (lang, file, source) in FIXTURES {
        let extraction = run(lang, source);
        assert_eq!(extraction.status, ExtractStatus::Done, "{file}");
        assert!(!extraction.defs.is_empty(), "{file}");
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_suffix(file.replace('.', "_"));
        settings.set_description(format!("tests/fixtures/extract/{lang}/{file}"));
        settings.bind(|| insta::assert_snapshot!("extract", render(&extraction)));
    }
}

fn def<'a>(extraction: &'a Extraction, name: &str) -> &'a crate::extract::Def {
    extraction
        .defs
        .iter()
        .find(|d| &*d.name == name)
        .unwrap_or_else(|| panic!("no definition {name}"))
}

#[test]
fn extract_ts_overlays_js_queries() {
    let source = FIXTURES.iter().find(|f| f.1 == "shapes.ts").unwrap().2;
    let ts = run("ts", source);
    // From the JavaScript query.
    assert_eq!(def(&ts, "Circle").kind, DefKind::Class);
    assert_eq!(def(&ts, "describe").kind, DefKind::Method);
    assert_eq!(def(&ts, "sum").kind, DefKind::Function);
    // From the TypeScript query.
    assert_eq!(def(&ts, "Shape").kind, DefKind::Interface);
    assert_eq!(def(&ts, "Base").kind, DefKind::Class);
    assert_eq!(def(&ts, "Geometry").kind, DefKind::Module);
    assert_eq!(def(&ts, "Pair").kind, DefKind::Type);
    assert_eq!(def(&ts, "Unit").kind, DefKind::Enum);
    // Overloads: three definitions of `scale`.
    assert_eq!(ts.defs.iter().filter(|d| &*d.name == "scale").count(), 3);
}

#[test]
fn extract_local_references_are_not_names() {
    let source =
        "export function outer(x) {\n  const inner = () => x;\n  inner();\n  shared();\n}\n";
    let js = run("js", source);
    assert!(js.refs.iter().any(|r| &**r == "shared"));
    assert!(!js.refs.iter().any(|r| &**r == "inner"), "{:?}", js.refs);
    // The nested helper is a definition, local to the file.
    let inner = def(&js, "inner");
    assert_eq!(inner.visibility, Visibility::File);
    assert_eq!(inner.scope.as_deref(), Some("outer"));
    assert_eq!(def(&js, "outer").visibility, Visibility::Public);
}

#[test]
fn extract_visibility_rules() {
    let rust = run(
        "rust",
        "pub fn a() {}\nfn b() {}\npub(crate) struct S { pub x: u8, y: u8 }\n\
         trait T { fn t(&self); }\nimpl T for S { fn t(&self) {} }\nimpl S { fn own(&self) {} pub fn open() {} }\n",
    );
    let vis = |e: &Extraction, n: &str| def(e, n).visibility;
    assert_eq!(vis(&rust, "a"), Visibility::Public);
    assert_eq!(vis(&rust, "b"), Visibility::Module);
    assert_eq!(vis(&rust, "S"), Visibility::Public);
    assert_eq!(vis(&rust, "x"), Visibility::Public);
    assert_eq!(vis(&rust, "y"), Visibility::Module);
    assert_eq!(vis(&rust, "own"), Visibility::Module);
    assert_eq!(vis(&rust, "open"), Visibility::Public);
    assert!(
        rust.defs
            .iter()
            .filter(|d| &*d.name == "t")
            .all(|d| d.visibility == Visibility::Public)
    );
    assert_eq!(def(&rust, "own").scope.as_deref(), Some("S"));

    let go = run("go", "package p\nfunc Open() {}\nfunc close() {}\n");
    assert_eq!(vis(&go, "Open"), Visibility::Public);
    assert_eq!(vis(&go, "close"), Visibility::Package);

    let java = run(
        "java",
        "class A { private void hidden() {} void shared() {} @Deprecated private int n; }",
    );
    assert_eq!(vis(&java, "hidden"), Visibility::File);
    assert_eq!(vis(&java, "shared"), Visibility::Public);
    assert_eq!(vis(&java, "n"), Visibility::File);

    let python = run("python", "def f():\n    pass\n");
    assert_eq!(vis(&python, "f"), Visibility::Unknown);

    // An ES module: what it does not export is file-local; a script or
    // CommonJS file shares its top level.
    let module = run(
        "js",
        "export function a() {}\nfunction b() {}\nfunction c() {}\nexport { c };\n",
    );
    assert_eq!(vis(&module, "a"), Visibility::Public);
    assert_eq!(vis(&module, "b"), Visibility::File);
    assert_eq!(vis(&module, "c"), Visibility::Public);
    let script = run("js", "function b() {}\nmodule.exports = { b };\n");
    assert_eq!(vis(&script, "b"), Visibility::Public);
}

#[test]
fn extract_binary_and_too_large() {
    let table = LanguageTable::core();
    let lang = table.get("js").unwrap();
    let watchdog = test_watchdog();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut binary = b"function a() {}\n".to_vec();
    binary.push(0);
    let e = extract(lang, &binary, &Deadline::background(&watchdog), &cancel);
    assert_eq!(e.status, ExtractStatus::Binary);
    assert!(e.defs.is_empty() && e.refs.is_empty());

    // A NUL past the probe is not binary.
    let mut late = vec![b' '; crate::extract::BINARY_PROBE];
    late.extend_from_slice(b"function a() {}\0");
    let e = extract(lang, &late, &Deadline::background(&watchdog), &cancel);
    assert_eq!(e.status, ExtractStatus::Done);

    let large = vec![b' '; crate::extract::BACKGROUND_MAX_LEN + 1];
    let e = extract(lang, &large, &Deadline::background(&watchdog), &cancel);
    assert_eq!(e.status, ExtractStatus::TooLarge);
    let e = extract(lang, &large, &Deadline::interactive(&watchdog), &cancel);
    assert_eq!(e.status, ExtractStatus::Done);
    watchdog.stop();
}

#[test]
fn extract_status_codes() {
    assert_eq!(ExtractStatus::Done.code(), Some(0));
    assert_eq!(ExtractStatus::TimedOut.code(), Some(1));
    assert_eq!(ExtractStatus::TooLarge.code(), Some(2));
    assert_eq!(ExtractStatus::Binary.code(), Some(3));
    assert_eq!(ExtractStatus::Cancelled.code(), None);
    assert_eq!(ExtractStatus::Unsupported.code(), None);
}

#[test]
fn extract_query_version_follows_text_and_extractor() {
    let (tags, locals) = ("(a) @name", "(b) @local.scope");
    let base = query_version_with(EXTRACTOR_VERSION, tags, locals);
    assert_eq!(base, query_version(tags, locals));
    assert_ne!(
        base,
        query_version_with(EXTRACTOR_VERSION + 1, tags, locals)
    );
    assert_ne!(base, query_version("(a) @name ", locals));
    assert_ne!(base, query_version(tags, "(c) @local.scope"));
    // Moving text between the two queries changes it too.
    assert_ne!(query_version("ab", ""), query_version("a", "b"));
}

#[test]
fn extract_same_bytes_differ_by_language() {
    let source = "export class A { run(x) { return x; } }\n";
    let table = LanguageTable::core();
    let (js, ts) = (table.get("js").unwrap(), table.get("ts").unwrap());
    assert_ne!(js.query_version(), ts.query_version());
    assert_ne!(js.grammar_version(), ts.grammar_version());
    assert_eq!(run("js", source).defs.len(), run("ts", source).defs.len());
}

#[test]
fn extract_uses_the_watchdog_clock() {
    // A deadline on a manual clock that never moves: the extraction runs to
    // the end however long it takes.
    let executor = Executor::manual();
    let clock = ManualClock::new();
    let (watchdog, _) = watchdog(&executor, Arc::new(clock));
    let table = LanguageTable::core();
    let source = crate::testing::corpus("rust", 64 << 10);
    let e = extract(
        table.get("rust").unwrap(),
        &source,
        &Deadline::background(&watchdog),
        &Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(e.status, ExtractStatus::Done);
    watchdog.stop();
}

#[test]
fn extract_arity_counts_parameters() {
    let cases = [
        ("()", Dialect::Rust, 0),
        ("(&self)", Dialect::Rust, 0),
        ("(&'a mut self, x: u8)", Dialect::Rust, 1),
        ("(self: Box<Self>, y: T)", Dialect::Rust, 1),
        ("(selfish: u8)", Dialect::Rust, 1),
        (
            "(a: HashMap<K, V>, f: impl Fn(u8, u8) -> u8, #[cfg(x)] c: u8,)",
            Dialect::Rust,
            3,
        ),
        (
            "(a: Map<string, number>, cb: (x: number) => void = () => {})",
            Dialect::TypeScript,
            2,
        ),
        ("(a = \",\", b = '(', c = `)`)", Dialect::JavaScript, 3),
        ("(a /* , */, b // c, d\n)", Dialect::JavaScript, 2),
        ("x", Dialect::JavaScript, 1),
        ("(self, a, /, b=(1, 2), *, c, **kw)", Dialect::Python, 5),
        ("(a, # one, two\n b)", Dialect::Python, 2),
        (
            "(Map<String, List<Item>> groups, boolean flag)",
            Dialect::Java,
            2,
        ),
        ("(a, b int, c ...string)", Dialect::Go, 3),
        ("(m map[string]int, f func(a, b int) error)", Dialect::Go, 2),
    ];
    for (text, dialect, want) in cases {
        assert_eq!(count_params(text, dialect), want, "{text}");
    }
    let many = format!("({})", vec!["a"; 300].join(", "));
    assert_eq!(count_params(&many, Dialect::JavaScript), 255);
}

#[test]
fn extract_plain_and_member_uses_are_names() {
    // `ci_ref` picks the candidate files of a reference search, so every
    // file that may use a name lists it: plain uses of a constant or
    // variable, and member names, not only calls and types. Locals stay out.
    let cases: &[(&str, &str, &[&str], &[&str])] = &[
        (
            "rust",
            "fn f(o: &Obj) -> u8 {\n    let v = PUBLIC_CONSTANT;\n    o.field + v\n}\n",
            &["PUBLIC_CONSTANT", "field"],
            &["v", "o", "f"],
        ),
        (
            "ts",
            "export function f(o: Obj) {\n  const v = LIMIT;\n  const cb = utils.handle;\n  return o.size + v;\n}\n",
            &["LIMIT", "utils", "handle", "size"],
            &["v", "o", "cb", "f"],
        ),
        (
            "js",
            "export function f(o) {\n  const v = LIMIT;\n  return { v, total: o.total + COUNT };\n}\n",
            &["LIMIT", "total", "COUNT"],
            &["v", "o", "f"],
        ),
        (
            "java",
            "class A {\n  int f(Inventory inv) {\n    int v = MAX_ITEMS;\n    return inv.counts + v;\n  }\n}\n",
            &["MAX_ITEMS", "counts"],
            &["v", "inv", "f"],
        ),
        (
            "python",
            "def f(o):\n    v = TAX_RATE\n    return models.User(o.amount, v)\n",
            &["TAX_RATE", "models", "User", "amount"],
            &["v", "o", "f"],
        ),
        (
            "go",
            "package p\n\nfunc f(t *table) float64 {\n\tv := Precision\n\treturn t.units + v\n}\n",
            &["Precision", "units"],
            &["v", "t", "f"],
        ),
    ];
    for (lang, source, present, absent) in cases {
        let e = run(lang, source);
        for name in *present {
            assert!(
                e.refs.iter().any(|r| &**r == *name),
                "{lang}: {name} missing from {:?}",
                e.refs
            );
        }
        for name in *absent {
            assert!(
                !e.refs.iter().any(|r| &**r == *name),
                "{lang}: {name} in {:?}",
                e.refs
            );
        }
    }
}

#[test]
fn extract_enum_members_are_definitions() {
    // Enum variants and members are definitions in the enum's scope and
    // take the enum's visibility; Python class attributes are fields.
    type Case<'a> = (
        &'a str,
        &'a str,
        &'a [(&'a str, DefKind, &'a str, Visibility)],
    );
    let cases: &[Case] = &[
        (
            "rust",
            "pub enum Error { Full, Io(u8) }\nenum Private { Hidden }\n",
            &[
                ("Full", DefKind::Constant, "Error", Visibility::Public),
                ("Io", DefKind::Constant, "Error", Visibility::Public),
                ("Hidden", DefKind::Constant, "Private", Visibility::Module),
            ],
        ),
        (
            "ts",
            "export enum Unit { Metre, Inch = 2 }\nenum Local { Only }\n",
            &[
                ("Metre", DefKind::Constant, "Unit", Visibility::Public),
                ("Inch", DefKind::Constant, "Unit", Visibility::Public),
                ("Only", DefKind::Constant, "Local", Visibility::File),
            ],
        ),
        (
            "java",
            "class A {\n  private enum Kind { TOOL }\n  public enum Open { WIDE }\n}\n",
            &[
                ("TOOL", DefKind::Constant, "Kind", Visibility::File),
                ("WIDE", DefKind::Constant, "Open", Visibility::Public),
            ],
        ),
        (
            "python",
            "class Color(Enum):\n    RED = 1\n    GREEN = 2\n",
            &[
                ("RED", DefKind::Field, "Color", Visibility::Unknown),
                ("GREEN", DefKind::Field, "Color", Visibility::Unknown),
            ],
        ),
    ];
    for (lang, source, want) in cases {
        let e = run(lang, source);
        for (name, kind, scope, visibility) in *want {
            let d = def(&e, name);
            assert_eq!(d.kind, *kind, "{lang} {name}");
            assert_eq!(d.scope.as_deref(), Some(*scope), "{lang} {name}");
            assert_eq!(d.visibility, *visibility, "{lang} {name}");
        }
    }
}
