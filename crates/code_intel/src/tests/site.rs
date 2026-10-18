//! `site_*`: the identifier and reference syntax at a position, local
//! bindings, `defines`, and the syntax-kind table.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::extract::{LanguageTable, Visibility};
use crate::site::{ParseError, ParsedFile, Position, Site, accepts};
use crate::testing::ManualClock;
use crate::{Clock, DefKind, RefSyntax, SystemClock};

/// The core languages, compiled once for the tests.
pub(super) fn core_table() -> &'static LanguageTable {
    static TABLE: std::sync::OnceLock<LanguageTable> = std::sync::OnceLock::new();
    TABLE.get_or_init(LanguageTable::core)
}

pub(super) fn parse(lang: &str, source: &str) -> ParsedFile {
    ParsedFile::parse(
        core_table().get(lang).expect("core language"),
        Arc::from(source.as_bytes()),
        &SystemClock,
        Instant::now() + Duration::from_secs(10),
        &AtomicBool::new(false),
    )
    .expect("parsed")
}

/// The position of the `nth` occurrence of `needle`, plus `offset` bytes.
pub(super) fn pos(source: &str, needle: &str, nth: usize, offset: usize) -> Position {
    let start = source
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("no {needle} #{nth}"))
        .0
        + offset;
    let line = source[..start].matches('\n').count();
    let column = start - source[..start].rfind('\n').map_or(0, |i| i + 1);
    Position {
        line: u32::try_from(line).unwrap(),
        column: u32::try_from(column).unwrap(),
    }
}

fn site(file: &ParsedFile, source: &str, needle: &str, nth: usize) -> Site {
    file.site_at(pos(source, needle, nth, 0))
        .unwrap_or_else(|| panic!("no site at {needle} #{nth}"))
}

/// An identifier (its `nth` occurrence) and the syntax expected there.
type Expected = (&'static str, usize, RefSyntax);

#[test]
fn site_reference_syntax_per_language() {
    let cases: &[(&str, &str, &[Expected])] = &[
        (
            "rust",
            "fn f(q: Queue) { run(); q.push(1); q.len; Queue::new(); println!(); let s = Stats { a: 1 }; }",
            &[
                ("run", 0, RefSyntax::Call),
                ("push", 0, RefSyntax::MemberCall),
                ("len", 0, RefSyntax::Member),
                ("Queue", 0, RefSyntax::Type),
                ("new", 0, RefSyntax::Call),
                ("println", 0, RefSyntax::Call),
                ("Stats", 0, RefSyntax::Construct),
                ("q", 1, RefSyntax::Plain),
            ],
        ),
        (
            "ts",
            "let s: Shape = new Circle(1); s.area(); s.name; load(); new geo.Ring();",
            &[
                ("Shape", 0, RefSyntax::Type),
                ("Circle", 0, RefSyntax::Construct),
                ("area", 0, RefSyntax::MemberCall),
                ("name", 0, RefSyntax::Member),
                ("load", 0, RefSyntax::Call),
                ("Ring", 0, RefSyntax::Construct),
                ("s", 1, RefSyntax::Plain),
            ],
        ),
        (
            "java",
            "class A { void f() { run(); items.add(x); this.count = 1; new Item(); List<Item> l; } }",
            &[
                ("run", 0, RefSyntax::Call),
                ("add", 0, RefSyntax::MemberCall),
                ("count", 0, RefSyntax::Member),
                ("Item", 0, RefSyntax::Construct),
                ("List", 0, RefSyntax::Type),
                ("x", 0, RefSyntax::Plain),
            ],
        ),
        (
            "python",
            "pay(inv)\ninv.total()\ninv.lines\n",
            &[
                ("pay", 0, RefSyntax::Call),
                ("total", 0, RefSyntax::MemberCall),
                ("lines", 0, RefSyntax::Member),
                ("inv", 0, RefSyntax::Plain),
            ],
        ),
        (
            "go",
            "package p\nfunc f(t *table) { check(Meters(1)); t.Convert(1, \"m\"); _ = t.units }\n",
            &[
                ("check", 0, RefSyntax::Call),
                ("Meters", 0, RefSyntax::Call),
                ("Convert", 0, RefSyntax::MemberCall),
                ("units", 0, RefSyntax::Member),
                ("table", 0, RefSyntax::Type),
            ],
        ),
    ];
    for (lang, source, sites) in cases {
        let file = parse(lang, source);
        for (needle, nth, want) in *sites {
            let s = site(&file, source, needle, *nth);
            assert_eq!(&*s.name, *needle, "{lang} {needle}");
            assert_eq!(s.syntax, *want, "{lang} {needle}");
        }
    }
}

#[test]
fn site_no_symbol_off_identifiers() {
    let source = "fn f() {\n    run( 1 );\n}\n";
    let file = parse("rust", source);
    // Inside `run`, and with the cursor right after it.
    assert_eq!(
        &*file.site_at(pos(source, "run", 0, 1)).unwrap().name,
        "run"
    );
    assert_eq!(
        &*file.site_at(pos(source, "run", 0, 3)).unwrap().name,
        "run"
    );
    // On a literal, on whitespace, past the line, past the file.
    assert!(file.site_at(pos(source, "1", 0, 0)).is_none());
    assert!(file.site_at(Position { line: 1, column: 1 }).is_none());
    assert!(
        file.site_at(Position {
            line: 0,
            column: 40
        })
        .is_none()
    );
    assert!(file.site_at(Position { line: 9, column: 0 }).is_none());
}

#[test]
fn site_locals_bind_references() {
    let source = "function outer(a) {\n  const b = a + 1;\n  return b + glob;\n}\n";
    let file = parse("js", source);
    let b = site(&file, source, "b", 1);
    let binding = b.binding.expect("bound");
    assert_eq!(binding.at, pos(source, "b", 0, 0));
    assert!(binding.local);
    assert!(binding.def.is_none());
    let a = site(&file, source, "a", 1).binding.expect("parameter");
    assert_eq!(a.at, pos(source, "a", 0, 0));
    assert!(site(&file, source, "glob", 0).binding.is_none());

    let source = "fn main() {\n    let x = 1;\n    let y = |v| v + x;\n    y(x);\n}\n";
    let file = parse("rust", source);
    assert_eq!(
        site(&file, source, "x", 1).binding.expect("let").at,
        pos(source, "x", 0, 0)
    );
    assert_eq!(
        site(&file, source, "v", 1)
            .binding
            .expect("closure parameter")
            .at,
        pos(source, "v", 0, 0)
    );
    let y = site(&file, source, "y", 1);
    assert_eq!(y.syntax, RefSyntax::Call);
    assert_eq!(y.binding.expect("let").at, pos(source, "y", 0, 0));

    // An inner binding shadows an outer one.
    let source = "def f(n):\n    def g(n):\n        return n\n    return n\n";
    let file = parse("python", source);
    let at = |nth| file.site_at(pos(source, "return n", nth, 7)).expect("n");
    assert_eq!(at(0).binding.expect("inner").at, pos(source, "(n)", 1, 1));
    assert_eq!(at(1).binding.expect("outer").at, pos(source, "(n)", 0, 1));
}

#[test]
fn site_defines_on_definition_names() {
    let source = "export function outer(x) {\n  const helper = () => x;\n  let count = 0;\n  return helper();\n}\nconst top = 1;\n";
    let file = parse("js", source);

    let outer = site(&file, source, "outer", 0).defines.expect("outer");
    assert_eq!(outer.def.kind, DefKind::Function);
    assert_eq!(outer.def.visibility, Visibility::Public);
    assert!(!outer.local);

    // Both tags and locals capture the nested helper: local.
    let helper = site(&file, source, "helper", 0).defines.expect("helper");
    assert_eq!(helper.def.kind, DefKind::Function);
    assert_eq!(helper.def.scope.as_deref(), Some("outer"));
    assert!(helper.local);

    // Only locals captures `count`: a local variable.
    let count = site(&file, source, "count", 0).defines.expect("count");
    assert_eq!(count.def.kind, DefKind::Variable);
    assert_eq!(count.def.scope.as_deref(), Some("outer"));
    assert!(count.local);

    // Root scope: locals captures it too, but it is not local.
    let top = site(&file, source, "top", 0).defines.expect("top");
    assert_eq!(top.def.kind, DefKind::Constant);
    assert!(!top.local);

    // A reference to the helper binds to it and carries its definition.
    let call = site(&file, source, "helper", 1);
    assert!(call.defines.is_none());
    let binding = call.binding.expect("bound");
    assert_eq!(
        binding.def.expect("tags definition").kind,
        DefKind::Function
    );
}

#[test]
fn site_references_include_locals() {
    let source = "function f() {\n  const inner = () => 1;\n  inner();\n  other();\n}\n";
    let file = parse("js", source);
    let names: Vec<&str> = file
        .references()
        .iter()
        .map(|r| std::str::from_utf8(&file.source()[r.start..r.end]).unwrap())
        .collect();
    // tags leaves out `inner()`; the locals reference keeps it.
    assert!(names.contains(&"inner"), "{names:?}");
    assert!(names.contains(&"other"), "{names:?}");
    // Deduplicated by position: `other` is both a tags and a locals reference.
    assert_eq!(names.iter().filter(|n| **n == "other").count(), 1);
    let at = file
        .references()
        .iter()
        .find(|r| &file.source()[r.start..r.end] == b"inner" && r.at.line == 2)
        .expect("call site");
    assert_eq!(at.at, pos(source, "inner", 1, 0));
}

#[test]
fn site_parse_deadline_and_cancel() {
    let table = LanguageTable::core();
    let rust = table.get("rust").unwrap();
    let source: Arc<[u8]> = Arc::from(crate::testing::corpus("rust", 256 << 10));
    let clock = ManualClock::new();
    let passed = ParsedFile::parse(
        rust,
        source.clone(),
        &clock,
        clock.now(),
        &AtomicBool::new(false),
    );
    assert!(matches!(passed, Err(ParseError::TimedOut)));
    let cancelled = ParsedFile::parse(
        rust,
        source.clone(),
        &clock,
        clock.now() + Duration::from_secs(1),
        &AtomicBool::new(true),
    );
    assert!(matches!(cancelled, Err(ParseError::Cancelled)));
    let file = ParsedFile::parse(
        rust,
        source,
        &clock,
        clock.now() + Duration::from_secs(1),
        &AtomicBool::new(false),
    )
    .expect("parsed");
    assert!(file.defs().count() > 0);
}

#[test]
fn site_parsed_defs_match_extraction() {
    for (lang, file, source) in crate::testing::FIXTURES {
        let parsed = parse(lang, source);
        let extracted = super::extract::run(lang, source);
        let parsed: Vec<_> = parsed.defs().cloned().collect();
        assert_eq!(parsed, extracted.defs, "{file}");
    }
}

/// (language, reference syntax, definition kind, source, the use: an
/// occurrence of the definition's name): an idiomatic use of a definition
/// of that kind in that syntax.
type Use = (&'static str, RefSyntax, DefKind, &'static str, (&'static str, usize));

const USES: &[Use] = &[
    // ---- JavaScript, TypeScript ----
    ("js", RefSyntax::Call, DefKind::Function, "function helper() {}\nhelper();\n", ("helper", 1)),
    ("js", RefSyntax::Call, DefKind::Variable, "let handler = make();\nhandler();\n", ("handler", 1)),
    ("js", RefSyntax::Call, DefKind::Constant, "const runner = make();\nrunner();\n", ("runner", 1)),
    ("js", RefSyntax::MemberCall, DefKind::Method, "class A { go() {} }\nnew A().go();\n", ("go", 1)),
    ("js", RefSyntax::MemberCall, DefKind::Function, "const api = { load: () => 1 };\napi.load();\n", ("load", 1)),
    ("js", RefSyntax::MemberCall, DefKind::Field, "class A { cb = make(); run() { this.cb(); } }\n", ("cb", 1)),
    ("js", RefSyntax::Construct, DefKind::Class, "class Store {}\nnew Store();\n", ("Store", 1)),
    ("js", RefSyntax::Construct, DefKind::Function, "function Legacy() {}\nnew Legacy();\n", ("Legacy", 1)),
    ("js", RefSyntax::Construct, DefKind::Variable, "let Impl = pick();\nnew Impl();\n", ("Impl", 1)),
    ("js", RefSyntax::Construct, DefKind::Constant, "const Impl = pick();\nnew Impl();\n", ("Impl", 1)),
    ("ts", RefSyntax::Call, DefKind::Function, "function helper(): void {}\nhelper();\n", ("helper", 1)),
    ("ts", RefSyntax::Construct, DefKind::Class, "class Store {}\nnew Store();\n", ("Store", 1)),
    ("ts", RefSyntax::Type, DefKind::Class, "class Shape {}\nlet s: Shape;\n", ("Shape", 1)),
    ("ts", RefSyntax::Type, DefKind::Interface, "interface Opts {}\nlet o: Opts;\n", ("Opts", 1)),
    ("ts", RefSyntax::Type, DefKind::Enum, "enum Kind { A }\nlet k: Kind;\n", ("Kind", 1)),
    ("ts", RefSyntax::Type, DefKind::Type, "type Id = number;\nlet i: Id;\n", ("Id", 1)),
    ("tsx", RefSyntax::Type, DefKind::Interface, "interface Props {}\nlet p: Props;\n", ("Props", 1)),
    // ---- Rust ----
    ("rust", RefSyntax::Call, DefKind::Function, "fn helper() {}\nfn main() { helper(); }\n", ("helper", 1)),
    ("rust", RefSyntax::Call, DefKind::Struct, "struct Meters(u8);\nfn f() { Meters(1); }\n", ("Meters", 1)),
    ("rust", RefSyntax::Call, DefKind::Macro, "macro_rules! log { () => {} }\nfn f() { log!(); }\n", ("log", 1)),
    ("rust", RefSyntax::Call, DefKind::Constant, "enum E { Wrap(u8) }\nfn f() { E::Wrap(1); }\n", ("Wrap", 1)),
    ("rust", RefSyntax::Call, DefKind::Method, "struct S;\nimpl S { fn create() -> S { S } }\nfn f() { S::create(); }\n", ("create", 1)),
    ("rust", RefSyntax::MemberCall, DefKind::Method, "struct S;\nimpl S { fn go(&self) {} }\nfn f(s: S) { s.go(); }\n", ("go", 1)),
    ("rust", RefSyntax::Construct, DefKind::Struct, "struct Point { x: u8 }\nfn f() { Point { x: 1 }; }\n", ("Point", 1)),
    ("rust", RefSyntax::Construct, DefKind::Constant, "enum M { Move { x: u8 } }\nfn f() { M::Move { x: 1 }; }\n", ("Move", 1)),
    ("rust", RefSyntax::Construct, DefKind::Type, "struct P { x: u8 }\ntype Alias = P;\nfn f() { Alias { x: 1 }; }\n", ("Alias", 1)),
    ("rust", RefSyntax::Type, DefKind::Struct, "struct Point;\nfn f(p: Point) {}\n", ("Point", 1)),
    ("rust", RefSyntax::Type, DefKind::Enum, "enum Kind { A }\nfn f(k: Kind) {}\n", ("Kind", 1)),
    ("rust", RefSyntax::Type, DefKind::Trait, "trait Draw {}\nfn f(d: &dyn Draw) {}\n", ("Draw", 1)),
    ("rust", RefSyntax::Type, DefKind::Type, "type Id = u8;\nfn f(i: Id) {}\n", ("Id", 1)),
    ("rust", RefSyntax::Type, DefKind::Constant, "enum M { Move { x: u8 } }\nfn f(m: M) { if let M::Move { x } = m {} }\n", ("Move", 1)),
    ("rust", RefSyntax::Member, DefKind::Field, "struct P { width: u8 }\nfn f(p: P) -> u8 { p.width }\n", ("width", 1)),
    // ---- Java ----
    ("java", RefSyntax::Call, DefKind::Method, "class A { void go() {} void run() { go(); } }\n", ("go", 1)),
    ("java", RefSyntax::Call, DefKind::Field, "record R(int size) { int twice() { return size() * 2; } }\n", ("size", 1)),
    ("java", RefSyntax::MemberCall, DefKind::Method, "class A { void go() {} void run(A a) { a.go(); } }\n", ("go", 1)),
    ("java", RefSyntax::MemberCall, DefKind::Field, "record R(int size) {}\nclass B { int f(R r) { return r.size(); } }\n", ("size", 1)),
    ("java", RefSyntax::Construct, DefKind::Class, "class Store {}\nclass B { Object o = new Store(); }\n", ("Store", 1)),
    ("java", RefSyntax::Construct, DefKind::Interface, "interface Task { void run(); }\nclass B { Task t = new Task() { public void run() {} }; }\n", ("Task", 2)),
    ("java", RefSyntax::Type, DefKind::Class, "class Store {}\nclass B { Store s; }\n", ("Store", 1)),
    ("java", RefSyntax::Type, DefKind::Interface, "interface Task {}\nclass B { Task t; }\n", ("Task", 1)),
    ("java", RefSyntax::Type, DefKind::Enum, "enum Kind { A }\nclass B { Kind k; }\n", ("Kind", 1)),
    // ---- Python ----
    ("python", RefSyntax::Call, DefKind::Function, "def helper():\n    pass\nhelper()\n", ("helper", 1)),
    ("python", RefSyntax::Call, DefKind::Class, "class Store:\n    pass\nStore()\n", ("Store", 1)),
    ("python", RefSyntax::Call, DefKind::Constant, "HANDLER = make()\nHANDLER()\n", ("HANDLER", 1)),
    ("python", RefSyntax::MemberCall, DefKind::Method, "class A:\n    def go(self):\n        pass\nA().go()\n", ("go", 1)),
    ("python", RefSyntax::MemberCall, DefKind::Function, "def helper():\n    pass\nmod.helper()\n", ("helper", 1)),
    ("python", RefSyntax::MemberCall, DefKind::Class, "class Store:\n    pass\nmodels.Store()\n", ("Store", 1)),
    ("python", RefSyntax::MemberCall, DefKind::Field, "class A:\n    def __init__(self, cb):\n        self.cb = cb\n    def run(self):\n        self.cb()\n", ("cb", 3)),
    // ---- Go ----
    ("go", RefSyntax::Call, DefKind::Function, "package p\n\nfunc helper() {}\n\nfunc f() { helper() }\n", ("helper", 1)),
    ("go", RefSyntax::Call, DefKind::Type, "package p\n\ntype Meters float64\n\nfunc f() { _ = Meters(1) }\n", ("Meters", 1)),
    ("go", RefSyntax::Call, DefKind::Variable, "package p\n\nvar handler = make()\n\nfunc f() { handler() }\n", ("handler", 1)),
    ("go", RefSyntax::MemberCall, DefKind::Method, "package p\n\ntype T struct{}\n\nfunc (T) Go() {}\n\nfunc f(t T) { t.Go() }\n", ("Go", 1)),
    ("go", RefSyntax::MemberCall, DefKind::Function, "package p\n\nfunc Helper() {}\n\nfunc f() { pkg.Helper() }\n", ("Helper", 1)),
    ("go", RefSyntax::MemberCall, DefKind::Type, "package p\n\ntype Meters float64\n\nfunc f() { _ = units.Meters(1) }\n", ("Meters", 1)),
    ("go", RefSyntax::MemberCall, DefKind::Field, "package p\n\ntype T struct{ cb func() }\n\nfunc f(t T) { t.cb() }\n", ("cb", 1)),
    ("go", RefSyntax::Type, DefKind::Type, "package p\n\ntype T struct{}\n\nfunc f(t T) {}\n", ("T", 1)),
];

#[test]
fn site_kind_masks_accept_uses() {
    // The kind masks come from the definition forms; each idiomatic use
    // checks that its syntax accepts the kind of what it names, so a form
    // that can be named in a way its row does not list (`new Runnable()
    // { … }` naming an interface) fails here. Every narrowed syntax of
    // every core language has uses.
    let table = LanguageTable::core();
    let mut failures = Vec::new();
    for (lang, syntax, kind, source, (needle, nth)) in USES {
        let file = parse(lang, source);
        if !file.defs().any(|d| &*d.name == *needle && d.kind == *kind) {
            failures.push(format!("{lang} {syntax:?}: no {kind:?} {needle} in {source:?}"));
            continue;
        }
        let at = site(&file, source, needle, *nth);
        if at.syntax != *syntax {
            failures.push(format!(
                "{lang}: {needle} #{nth} is {:?}, not {syntax:?}",
                at.syntax
            ));
        }
        if !accepts(table.get(lang).unwrap(), *syntax, *kind) {
            failures.push(format!("{lang} {syntax:?} does not accept {kind:?}"));
        }
    }
    let narrowed: &[(&str, &[RefSyntax])] = &[
        ("js", &[RefSyntax::Call, RefSyntax::MemberCall, RefSyntax::Construct]),
        ("ts", &[RefSyntax::Call, RefSyntax::Construct, RefSyntax::Type]),
        ("tsx", &[RefSyntax::Type]),
        (
            "rust",
            &[
                RefSyntax::Call,
                RefSyntax::MemberCall,
                RefSyntax::Construct,
                RefSyntax::Type,
                RefSyntax::Member,
            ],
        ),
        (
            "java",
            &[RefSyntax::Call, RefSyntax::MemberCall, RefSyntax::Construct, RefSyntax::Type],
        ),
        ("python", &[RefSyntax::Call, RefSyntax::MemberCall]),
        ("go", &[RefSyntax::Call, RefSyntax::MemberCall, RefSyntax::Type]),
    ];
    for (lang, syntaxes) in narrowed {
        for syntax in *syntaxes {
            if !USES.iter().any(|u| u.0 == *lang && u.1 == *syntax) {
                failures.push(format!("{lang} {syntax:?}: no use sample"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn site_kind_table() {
    let table = LanguageTable::core();
    let (rust, go, ts) = (
        table.get("rust").unwrap(),
        table.get("go").unwrap(),
        table.get("ts").unwrap(),
    );
    let accepted = |lang, syntax| -> Vec<DefKind> {
        crate::DEF_KINDS
            .into_iter()
            .filter(|k| accepts(lang, syntax, *k))
            .collect()
    };
    use DefKind::*;
    use RefSyntax::{Call, Construct, MemberCall};
    // The masks derived from the definition forms (crate::forms): every
    // kind is accepted by each syntax its rows say can name it.
    let expected: &[(&str, RefSyntax, &[DefKind])] = &[
        // JS / TS: any value can hold a function or a class.
        ("ts", Call, &[Function, Method, Constant, Field, Variable]),
        (
            "ts",
            MemberCall,
            &[Function, Method, Constant, Field, Variable],
        ),
        ("ts", Construct, &[Function, Class, Constant, Variable]),
        (
            "ts",
            RefSyntax::Type,
            &[Class, Interface, Enum, Type, Module],
        ),
        ("tsx", Call, &[Function, Method, Constant, Field, Variable]),
        ("js", Call, &[Function, Method, Constant, Field, Variable]),
        ("js", Construct, &[Function, Class, Constant, Variable]),
        ("js", RefSyntax::Type, &[Class]),
        // Rust: tuple variants are called, record variants constructed and
        // matched (`Message::Move { x, y }`), consts and statics hold
        // function pointers.
        ("rust", Call, &[Function, Method, Struct, Macro, Constant]),
        ("rust", MemberCall, &[Method]),
        ("rust", Construct, &[Struct, Type, Constant]),
        (
            "rust",
            RefSyntax::Type,
            &[Struct, Enum, Trait, Type, Constant],
        ),
        ("rust", RefSyntax::Member, &[Method, Field]),
        // Java: a record component's accessor is called.
        ("java", Call, &[Method, Field]),
        ("java", MemberCall, &[Method, Field]),
        // `new Runnable() { … }` constructs an anonymous class of an
        // interface.
        ("java", Construct, &[Class, Interface]),
        ("java", RefSyntax::Type, &[Class, Interface, Enum]),
        // Python: classes, module constants and class attributes can be
        // called.
        ("python", Call, &[Function, Method, Class, Constant, Field]),
        (
            "python",
            MemberCall,
            &[Function, Method, Class, Constant, Field],
        ),
        ("python", Construct, &[]),
        // Go: conversions are calls naming a type; function-typed variables
        // and fields are called.
        ("go", Call, &[Function, Type, Variable]),
        ("go", MemberCall, &[Function, Method, Type, Field, Variable]),
        ("go", RefSyntax::Type, &[Type]),
    ];
    for (lang, syntax, kinds) in expected {
        assert_eq!(
            accepted(table.get(lang).unwrap(), *syntax),
            *kinds,
            "{lang} {syntax:?}"
        );
    }
    assert_eq!(accepted(rust, RefSyntax::Plain), crate::DEF_KINDS);
    // Outside Rust the left side of `a.b` can be a namespace, module, package
    // or class: a member can name any kind (`utils.handle`, `models.User`).
    for lang in ["ts", "tsx", "js", "java", "python", "go"] {
        let lang = table.get(lang).unwrap();
        assert_eq!(accepted(lang, RefSyntax::Member), crate::DEF_KINDS);
    }
    assert!(accepts(ts, RefSyntax::Member, Function));
    assert!(accepts(
        table.get("python").unwrap(),
        RefSyntax::Member,
        Class
    ));
    assert!(accepts(go, RefSyntax::MemberCall, Type));
    assert!(!accepts(rust, RefSyntax::Call, Type));
}

#[test]
fn site_rust_variants_are_reachable() {
    // A variant is a constant: called (`Write(s)`), constructed
    // (`Message::Move { x: 1 }`) and matched (`Message::Move { x } =>`).
    let table = LanguageTable::core();
    let rust = table.get("rust").unwrap();
    let source = "enum Message { Move { x: u8 }, Write(u8) }\nfn f(m: Message) -> u8 {\n    let _ = Message::Move { x: 1 };\n    let _ = Message::Write(2);\n    match m { Message::Move { x } => x, _ => 0 }\n}\n";
    let file = parse("rust", source);
    let cases = [
        ("Move", 1, RefSyntax::Construct),
        ("Write", 1, RefSyntax::Call),
        ("Move", 2, RefSyntax::Type),
    ];
    for (needle, nth, syntax) in cases {
        let at = site(&file, source, needle, nth);
        assert_eq!(at.syntax, syntax, "{needle} #{nth}");
        assert!(
            accepts(rust, at.syntax, DefKind::Constant),
            "{needle} #{nth}"
        );
    }
}

#[test]
fn site_member_values_accept_every_kind() {
    // A member that is not called can name a function or a class: an
    // exported function taken as a value, a qualified Python name.
    let table = LanguageTable::core();
    let source = "import * as utils from './utils';\nconst cb = utils.handle;\n";
    let file = parse("ts", source);
    let handle = site(&file, source, "handle", 0);
    assert_eq!(handle.syntax, RefSyntax::Member);
    assert!(accepts(
        table.get("ts").unwrap(),
        handle.syntax,
        DefKind::Function
    ));

    let source = "import models\nuser = models.User\n";
    let file = parse("python", source);
    let user = site(&file, source, "User", 0);
    assert_eq!(user.syntax, RefSyntax::Member);
    assert!(accepts(
        table.get("python").unwrap(),
        user.syntax,
        DefKind::Class
    ));

    // Rust's `a.b` has a value on the left: fields and methods only.
    let rust = table.get("rust").unwrap();
    assert!(!accepts(rust, RefSyntax::Member, DefKind::Function));
    assert!(accepts(rust, RefSyntax::Member, DefKind::Field));
}

#[test]
fn site_nested_declarations_bind_locally() {
    // (language, source, the call's needle and occurrence, the definition's
    // needle and occurrence, offset of the name inside the definition needle)
    let cases = [
        (
            "python",
            "def outer():\n    def helper():\n        return 1\n    return helper()\n",
            ("helper", 1),
            ("def helper", 0, 4),
        ),
        (
            "python",
            "def outer():\n    class Local:\n        pass\n    return Local()\n",
            ("Local", 1),
            ("class Local", 0, 6),
        ),
        (
            "js",
            "function outer() {\n  function helper() { return 1; }\n  return helper();\n}\n",
            ("helper", 1),
            ("function helper", 0, 9),
        ),
        (
            "rust",
            "fn outer() -> u8 {\n    fn helper() -> u8 { 1 }\n    helper()\n}\n",
            ("helper", 1),
            ("fn helper", 0, 3),
        ),
        (
            "java",
            "class A {\n  void outer() {\n    class Local {}\n    new Local();\n  }\n}\n",
            ("Local", 1),
            ("class Local", 0, 6),
        ),
    ];
    for (lang, source, (call, call_nth), (decl, decl_nth, offset)) in cases {
        let file = parse(lang, source);
        let at = pos(source, decl, decl_nth, offset);
        let use_site = site(&file, source, call, call_nth);
        let binding = use_site
            .binding
            .unwrap_or_else(|| panic!("{lang}: {call} is not bound"));
        assert_eq!(binding.at, at, "{lang}");
        // In the enclosing function's scope, not the declaration's own body.
        assert!(binding.local, "{lang}");
        let defines = file
            .site_at(at)
            .expect("the name")
            .defines
            .expect("defines");
        assert!(defines.local, "{lang}: the nested declaration is local");
    }

    // At the top level the name is in the root scope: not local.
    let source = "def top():\n    return 1\n\ntop()\n";
    let file = parse("python", source);
    let top = file
        .site_at(pos(source, "top", 0, 0))
        .unwrap()
        .defines
        .unwrap();
    assert!(!top.local);
    let call = site(&file, source, "top", 1).binding.expect("root binding");
    assert!(!call.local);
}

#[test]
fn site_member_names_do_not_bind_locals() {
    // (language, source, member needle and occurrence, plain use of the
    // same name and its occurrence)
    let cases = [
        (
            "java",
            "class A {\n  void f(java.util.List<Integer> items) {\n    int add = 0;\n    items.add(add);\n    this.add = add;\n  }\n}\n",
            &[("add", 1), ("add", 3)][..],
            ("add", 2),
        ),
        (
            "python",
            "def f(obj):\n    x = 1\n    obj.x()\n    return obj.x + x\n",
            &[("x", 1), ("x", 2)][..],
            ("x", 3),
        ),
        (
            "rust",
            "fn f() {\n    let new = 1;\n    let q = Queue::new();\n    g(new);\n}\n",
            &[("new", 1)][..],
            ("new", 2),
        ),
    ];
    for (lang, source, members, plain) in cases {
        let file = parse(lang, source);
        for &(needle, nth) in members {
            let s = site(&file, source, needle, nth);
            assert!(
                s.binding.is_none(),
                "{lang}: {needle} #{nth} bound to a local"
            );
        }
        let s = site(&file, source, plain.0, plain.1);
        assert!(s.binding.is_some(), "{lang}: plain {} unbound", plain.0);
    }
}
