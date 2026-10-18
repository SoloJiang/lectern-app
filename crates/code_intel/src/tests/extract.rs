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
    // Only program-level import and export make an ES module: an `export`
    // in a namespace exports a member of it.
    let namespaced = run(
        "ts",
        "namespace NS { export function inner() {} export const version = 1; }\nfunction top() {}\n",
    );
    assert_eq!(vis(&namespaced, "top"), Visibility::Public);
    assert_eq!(vis(&namespaced, "version"), Visibility::Public);
    let module = run(
        "ts",
        "export namespace NS { export const version = 1; }\nfunction top() {}\n",
    );
    assert_eq!(vis(&module, "top"), Visibility::File);
    assert_eq!(vis(&module, "version"), Visibility::Public);
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

/// The JS / TS export forms (docs/design/code_intel.md「抽取结果」):
/// (language, form, an ES module that exports `name` this way, the same
/// module without the export, name).
const EXPORT_FORMS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "js",
        "export function",
        "import m from 'm';\nexport function run() {}\n",
        "import m from 'm';\nfunction run() {}\n",
        "run",
    ),
    (
        "js",
        "export const",
        "import m from 'm';\nexport const LIMIT = 1;\n",
        "import m from 'm';\nconst LIMIT = 1;\n",
        "LIMIT",
    ),
    (
        "js",
        "export class",
        "import m from 'm';\nexport class Store {}\n",
        "import m from 'm';\nclass Store {}\n",
        "Store",
    ),
    (
        "js",
        "export default function",
        "import m from 'm';\nexport default function run() {}\n",
        "import m from 'm';\nfunction run() {}\n",
        "run",
    ),
    (
        "js",
        "export default name",
        "import m from 'm';\nfunction run() {}\nexport default run;\n",
        "import m from 'm';\nfunction run() {}\n",
        "run",
    ),
    (
        "js",
        "export { name }",
        "import m from 'm';\nfunction run() {}\nexport { run };\n",
        "import m from 'm';\nfunction run() {}\n",
        "run",
    ),
    (
        "js",
        "export { name as alias }",
        "import m from 'm';\nfunction run() {}\nexport { run as go };\n",
        "import m from 'm';\nfunction run() {}\n",
        "run",
    ),
    (
        "ts",
        "export = name",
        "import m from 'm';\nfunction run() {}\nexport = run;\n",
        "import m from 'm';\nfunction run() {}\n",
        "run",
    ),
    (
        "ts",
        "export declare const",
        "import m from 'm';\nexport declare const VERSION: string;\n",
        "import m from 'm';\ndeclare const VERSION: string;\n",
        "VERSION",
    ),
    (
        "ts",
        "export interface",
        "import m from 'm';\nexport interface Opts {}\n",
        "import m from 'm';\ninterface Opts {}\n",
        "Opts",
    ),
    (
        "ts",
        "export namespace",
        "import m from 'm';\nexport namespace NS { export const version = 1; }\n",
        "import m from 'm';\nnamespace NS { export const version = 1; }\n",
        "NS",
    ),
    (
        "ts",
        "declare global",
        "import m from 'm';\ndeclare global { interface Window { app: number } }\n",
        "import m from 'm';\ninterface Window { app: number }\n",
        "Window",
    ),
];

#[test]
fn extract_export_forms_change_visibility() {
    // Each export form makes the name `Public` in an ES module, where the
    // same name unexported is `File`; both extraction paths agree.
    let table = LanguageTable::core();
    let watchdog = test_watchdog();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut failures = Vec::new();
    for (lang, form, exported, plain, name) in EXPORT_FORMS {
        let l = table.get(lang).unwrap();
        for (source, want) in [(exported, Visibility::Public), (plain, Visibility::File)] {
            let deadline = Deadline::interactive(&watchdog);
            let walked = extract(l, source.as_bytes(), &deadline, &cancel);
            let tagged = crate::extract::extract_tags(l, source.as_bytes(), &deadline, &cancel);
            if walked != tagged {
                failures.push(format!("{lang} {form}: walker and tags differ"));
            }
            match walked.defs.iter().find(|d| &*d.name == *name) {
                Some(d) if d.visibility == want => {}
                found => failures.push(format!(
                    "{lang} {form}: {name} is {:?}, not {want:?} in {source:?}",
                    found.map(|d| d.visibility)
                )),
            }
        }
    }
    watchdog.stop();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn extract_python_module_level_and_attributes() {
    // Module level is no function or class scope around: assignments under
    // `if`, `try`, `with` and `for`, and every name of a chain, are module
    // constants; ones in a function or class are not. `self.name = …` in a
    // method (or a function nested in it) is a field of the class; other
    // attribute assignments stay member references.
    let source = "import os\nif os.name == 'nt':\n    SEP = 'a'\nelse:\n    SEP = 'b'\ntry:\n    FAST = True\nexcept ImportError:\n    FAST = None\n\
                  with open('x') as fh:\n    DATA = fh.read()\nfor item in []:\n    LAST = item\nA = B = None\n\
                  def f():\n    local = 1\n    return local\n\
                  class Config:\n    size = 1\n    if True:\n        hidden = 2\n    def __init__(self, name):\n        self.name = name\n        self.size = 3\n        other.attr = 1\n\
                  \x20   def reset(this):\n        this.count = 0\n        def inner():\n            this.nested = 1\n        inner()\n\
                  def free(obj):\n    obj.x = 1\n";
    let table = LanguageTable::core();
    let watchdog = test_watchdog();
    let cancel = Arc::new(AtomicBool::new(false));
    let python = table.get("python").unwrap();
    let deadline = Deadline::interactive(&watchdog);
    let walked = extract(python, source.as_bytes(), &deadline, &cancel);
    let tagged = crate::extract::extract_tags(python, source.as_bytes(), &deadline, &cancel);
    watchdog.stop();
    assert_eq!(walked, tagged);
    let find = |name: &str| walked.defs.iter().filter(|d| &*d.name == name).collect::<Vec<_>>();
    for name in ["SEP", "FAST", "DATA", "LAST", "A", "B"] {
        let defs = find(name);
        assert!(!defs.is_empty(), "{name}");
        for d in defs {
            assert_eq!((d.kind, d.scope.as_deref()), (DefKind::Constant, None), "{name}");
        }
    }
    for name in ["local", "hidden", "attr", "x"] {
        assert!(find(name).is_empty(), "{name}: {:?}", find(name));
    }
    for name in ["size", "name", "count", "nested"] {
        let defs = find(name);
        assert_eq!(defs.len(), 1, "{name}: {defs:?}");
        assert_eq!(defs[0].kind, DefKind::Field, "{name}");
        assert_eq!(defs[0].scope.as_deref(), Some("Config"), "{name}");
    }
    for name in ["attr", "x"] {
        assert!(walked.refs.iter().any(|r| &**r == name), "{name} in {:?}", walked.refs);
    }
}

#[test]
fn extract_rust_impl_self_types() {
    // The methods of a trait implementation are public and in the
    // implemented type's scope, whatever wraps the type; an inherent impl's
    // methods keep their own visibility.
    let source = "impl Display for &Point { fn a(&self) {} }\n\
                  impl Display for &mut Grid { fn b(&self) {} }\n\
                  impl<T> Display for Box<T> { fn c(&self) {} }\n\
                  impl Display for [Cell] { fn d(&self) {} }\n\
                  impl Display for &[Row] { fn e(&self) {} }\n\
                  impl Display for (Left, Right) { fn f(&self) {} }\n\
                  impl Display for *const Raw { fn g(&self) {} }\n\
                  impl Display for u8 { fn h(&self) {} }\n\
                  impl Display for std::path::PathBuf { fn i(&self) {} }\n\
                  impl Display for fn() { fn j(&self) {} }\n\
                  impl Point { fn k(&self) {} }\n\
                  impl Grid<u8> { pub fn l(&self) {} }\n";
    let want: &[(&str, Option<&str>, Visibility)] = &[
        ("a", Some("Point"), Visibility::Public),
        ("b", Some("Grid"), Visibility::Public),
        ("c", Some("Box"), Visibility::Public),
        ("d", Some("Cell"), Visibility::Public),
        ("e", Some("Row"), Visibility::Public),
        ("f", Some("Left"), Visibility::Public),
        ("g", Some("Raw"), Visibility::Public),
        ("h", Some("u8"), Visibility::Public),
        ("i", Some("PathBuf"), Visibility::Public),
        ("j", None, Visibility::Public),
        ("k", Some("Point"), Visibility::Module),
        ("l", Some("Grid"), Visibility::Public),
    ];
    let table = LanguageTable::core();
    let watchdog = test_watchdog();
    let cancel = Arc::new(AtomicBool::new(false));
    let rust = table.get("rust").unwrap();
    let deadline = Deadline::interactive(&watchdog);
    let walked = extract(rust, source.as_bytes(), &deadline, &cancel);
    let tagged = crate::extract::extract_tags(rust, source.as_bytes(), &deadline, &cancel);
    watchdog.stop();
    assert_eq!(walked, tagged);
    for (name, scope, visibility) in want {
        let d = def(&walked, name);
        assert_eq!(d.scope.as_deref(), *scope, "{name}");
        assert_eq!(d.visibility, *visibility, "{name}");
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

#[test]
fn extract_definitions_on_one_node_are_siblings() {
    // `name, kind string` and `int a, b;` are two definitions on one node:
    // each is in the type's scope, not in the other's.
    type Case<'a> = (&'a str, &'a str, &'a [(&'a str, Option<&'a str>)]);
    let cases: &[Case] = &[
        (
            "go",
            "package p\n\nconst A, B = 1, 2\n\ntype T struct { name, kind string }\n",
            &[
                ("A", None),
                ("B", None),
                ("name", Some("T")),
                ("kind", Some("T")),
            ],
        ),
        (
            "java",
            "class C {\n  int a, b;\n}\n",
            &[("a", Some("C")), ("b", Some("C"))],
        ),
    ];
    for (lang, source, want) in cases {
        let e = run(lang, source);
        for (name, scope) in *want {
            assert_eq!(def(&e, name).scope.as_deref(), *scope, "{lang} {name}");
        }
    }
}

/// Sources for the walkers' parity test beyond the fixtures: each pattern
/// of the tags and locals queries and its near misses.
const WALKER_CASES: &[(&str, &str)] = &[
    (
        "rust",
        "pub(crate) struct S<T>(T);\nunion U { a: u8 }\npub type Alias = u8;\nmod m { pub fn f() {} }\nmacro_rules! mac { ($x:ident) => { $x }; }\n\
         impl<T> S<T> { pub fn new(t: T) -> Self { S(t) } }\nimpl a::b::Trait for crate::x::S<u8> { fn go(&self) {} }\nimpl Display for S<u8> { fn fmt(&self, f: &mut F) -> R { write!(f, \"\") } }\n\
         trait T { fn sig(&self, a: u8); fn body() {} }\nextern \"C\" { fn ext(x: i32); }\nstatic mut G: u8 = 0;\n\
         fn f(Point { x, y: z }: P, (a, b): (u8, u8), c @ Some(_): O) -> u8 {\n    let helper = |q: u8| q + x;\n    fn inner() {}\n    const K: u8 = 1;\n    \
         let s = S::new(a).field.method(b);\n    m::f(); mac!(z); inner(); helper(K);\n    if let Some(v) = c { v } else { v }\n    \
         while let Some(w) = it.next() { w; }\n    for (i, j) in pairs { i + j; }\n    match s { Some(k) if k > 0 => k, None => G, _ => 0 }\n    S { field: 1 }.field\n}\n",
    ),
    (
        "go",
        "package p\n\nimport \"fmt\"\n\nconst A, B = 1, 2\nvar (\n\tx int\n\ty, z = 1, 2\n)\nvar w = 3\n\ntype T[K any] struct { name, kind string }\ntype I interface { M(a int) error }\ntype Alias = T[int]\n\n\
         func (t *T[K]) M(a int) error { return nil }\nfunc (T) N() {}\nfunc (t *T[K]) O(xs ...int) {}\n\
         func f(a, b int, rest ...string) (int, error) {\n\tc := a + x\n\tvar d, e = b, c\n\tconst g = 1\n\tfor i, v := range rest { fmt.Println(i, v, d, e, g) }\n\t\
         switch q := c.(type) { case int: _ = q }\n\tselect { case m := <-ch: _ = m }\n\tselect { case w = <-ch: _ = w }\n\tfor x = range rest { _ = x }\n\tout, d := g2()\n\t_ = out\n\tif h := f2(); h > 0 { return h, nil } else { return h, nil }\n\t\
         fn := func(k int) int { return k + c }\n\treturn (fn)(1), t.M(2)\n}\n",
    ),
    (
        "python",
        "import os\nfrom a import b as c\nLIMIT = 1\nx, y = 1, 2\n[pa, *pb] = xs\n(pc, (pd, [pe, *pf])) = ys\n\nclass Base(object, metaclass=Meta):\n    count = 0\n    left, (right, *more) = 1, (2, 3)\n    other: int = 1\n    @staticmethod\n    def make(cls, *args, key=None, **kw):\n        return cls(count)\n    \
         def run(self, /, a, b: int = 2, *, c):\n        global LIMIT\n        LIMIT = a\n        return [v for v in self.items if (w := v)] + [w]\n    class Inner(Base):\n        size = count\n\n\
         @decorator\ndef top(p, q=LIMIT):\n    def helper(r):\n        nonlocal p\n        return r + p\n    with open(p) as fh, other() as (g, h):\n        pass\n    try:\n        pass\n    except E as err:\n        print(err)\n    \
         for k, (m, n) in items:\n        top(k)\n    lam = lambda s, t=1: s + t\n    q += 1\n    os.path.join(helper(q), lam(1))\n    return Base.make(q)\n",
    ),
    (
        "java",
        "package p;\nimport java.util.List;\n\n@interface Marker { int value(); }\npublic record Point(int x, int y) {}\ninterface Shape { double area(); }\n\
         enum Kind { A, B; private static final int MAX = 3; }\n\
         public class C<T> extends Base implements Shape, Other {\n    private int count;\n    public static final String NAME = \"n\";\n    private C(int count) { this.count = count; }\n    \
         public double area() { return count; }\n    private <U> void run(List<U> items, int... more) {\n        int total = 0;\n        for (U item : items) { total += item.hashCode(); }\n        \
         try (var in = open()) { in.read(); } catch (IOException e) { log(e); }\n        Runnable r = () -> run(items);\n        java.util.function.Function<Integer, Integer> f = x -> x + total;\n        \
         if (items instanceof List<?> l) { l.size(); }\n        switch (total) { case 1: int k = 2; break; default: k = 3; }\n        \
         class Local { }\n        Object o = new Local();\n        this.count = items.size() + Math.max(total, count);\n    }\n}\n",
    ),
    (
        "ts",
        "import { a } from './a';\nexport { a as b };\nexport default thing;\nexport const LIMIT = 3, other = () => 1;\nlet counter = 0;\nvar legacy = 1;\nexport let flag = true;\nexport const { tx, ty: [tz] } = obj;\n\
         export abstract class Shape<T> extends Base implements IShape, Other {\n  private readonly id: number = 1;\n  #secret = 2;\n  abstract area(): number;\n  constructor(public x: number, private y?: string) { super(); }\n  \
         method({ a, b: [c] }: Opts, d = LIMIT, ...rest: number[]): void { this.#secret; c(); }\n  get size() { return this.id; }\n}\n\
         interface IShape { area(): number; name: string; [key: string]: unknown; }\ntype Alias<T> = T | null;\nenum Color { Red, Green = 2, 'Blue' }\nconst enum E { A }\n\
         declare function overload(a: number): void;\nnamespace NS { export const inner = 1; }\ndeclare module 'mod' { export const fromMod: number; }\nmodule Legacy { export var old = 1, older = 2; let hidden = old; export const { a1 } = obj; }\n\
         function outer(p: number) {\n  const inner = (q: number) => q + p;\n  function nested() { return inner(p); }\n  for (const k of keys) { use(k); }\n  for (let i = 0; i < 3; i++) { i; }\n  \
         try { } catch (err) { err; }\n  return new Shape.Sub() || new Thing() || require('x');\n}\nobj.handler = function (e) { e; };\nexport default b = 1;\nconst o = { m(x) { return x; }, f: async (y) => y, g: z => z };\n",
    ),
    (
        "tsx",
        "import React from 'react';\nexport function Panel({ items }: Props) {\n  const [open, setOpen] = useState(false);\n  return <Box title=\"t\">{items.map(item => <Row key={item.id} {...item} />)}<div onClick={() => setOpen(!open)} /><ui.Menu /></Box>;\n}\nclass View extends React.Component<P> { state = { n: 1 }; render() { return <View.Item />; } }\n",
    ),
    (
        "js",
        "const path = require('path');\nmodule.exports = { resolve, other: function () {} };\nexports.helper = (a) => a;\nclass Store extends Base {\n  items = [];\n  #count = 0;\n  static #hidden;\n  constructor(root) { super(); this.root = root; }\n  \
         *gen() { yield 1; }\n  async load(key) { const { [key]: value, ...rest } = this.cache; return value ?? rest; }\n}\nclass Sub extends mixins.Base {}\nconst Anon = class Named {};\n\
         function* generator() {}\nconst fn = function named(x) { return named; };\nvar a = 1, b = function () {};\nlet c;\nconst { p, q: r = 1, s = 2, ...t, n: { nn }, l: [l1] } = src;\nlet [u, , [v], { w }, ...z] = arr;\nexport var { deep: { inner: [notBound] } } = obj;\nfor (var i in obj) { i; }\nfor (x of y) { x; }\n\
         function hoisted() { return later(); function later() {} }\nexport default function () {}\nconst el = <Comp prop={value}><span>{text}</span><Other /></Comp>;\n",
    ),
];

#[test]
fn extract_walker_matches_tags() {
    // The walkers (crate::walk) follow the tags and locals queries: both
    // paths give the same extraction for every source.
    let table = LanguageTable::core();
    let watchdog = test_watchdog();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut sources: Vec<(&str, String, Vec<u8>)> = FIXTURES
        .iter()
        .map(|(lang, file, source)| (*lang, (*file).to_owned(), source.as_bytes().to_vec()))
        .collect();
    for (lang, source) in WALKER_CASES {
        sources.push((lang, format!("{lang} case"), source.as_bytes().to_vec()));
    }
    for lang in table.iter() {
        let name: &str = &lang.name().0;
        let name = CORE_NAMES.iter().copied().find(|n| *n == name).unwrap();
        sources.push((
            name,
            format!("{name} corpus"),
            crate::testing::corpus(name, 64 << 10),
        ));
    }
    sources.push((
        "js",
        "pathological".to_owned(),
        crate::testing::pathological_js(64 << 10),
    ));
    let mut failures = Vec::new();
    for (lang, what, source) in &sources {
        let lang = table.get(lang).unwrap();
        let deadline = Deadline::interactive(&watchdog);
        let walked = extract(lang, source, &deadline, &cancel);
        let tagged = crate::extract::extract_tags(lang, source, &deadline, &cancel);
        assert_eq!(walked.status, ExtractStatus::Done, "{what}");
        let show = |d: &crate::extract::Def| {
            format!(
                "{}:{} {} {} {:?} {} {:?}",
                d.line + 1,
                d.column,
                crate::def_kind_name(d.kind),
                d.name,
                d.scope,
                d.visibility.as_str(),
                d.arity
            )
        };
        let walked_defs: Vec<String> = walked.defs.iter().map(show).collect();
        let tagged_defs: Vec<String> = tagged.defs.iter().map(show).collect();
        for d in &tagged_defs {
            if !walked_defs.contains(d) {
                failures.push(format!("{what}: walker lacks definition {d}"));
            }
        }
        for d in &walked_defs {
            if !tagged_defs.contains(d) {
                failures.push(format!("{what}: walker adds definition {d}"));
            }
        }
        for r in &tagged.refs {
            if !walked.refs.contains(r) {
                failures.push(format!("{what}: walker lacks reference {r}"));
            }
        }
        for r in &walked.refs {
            if !tagged.refs.contains(r) {
                failures.push(format!("{what}: walker adds reference {r}"));
            }
        }
    }
    watchdog.stop();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

const CORE_NAMES: &[&str] = &["ts", "tsx", "js", "rust", "java", "python", "go"];
