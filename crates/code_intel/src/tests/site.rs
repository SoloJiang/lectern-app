//! `site_*`: the identifier and reference syntax at a position, local
//! bindings, `defines`, and the syntax-kind table.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::extract::{LanguageTable, Visibility};
use crate::site::{ParseError, ParsedFile, Position, Site, accepts};
use crate::testing::ManualClock;
use crate::{Clock, DefKind, RefSyntax, SystemClock};

fn parse(lang: &str, source: &str) -> ParsedFile {
    let table = LanguageTable::core();
    ParsedFile::parse(
        table.get(lang).expect("core language"),
        Arc::from(source.as_bytes()),
        &SystemClock,
        Instant::now() + Duration::from_secs(10),
        &AtomicBool::new(false),
    )
    .expect("parsed")
}

/// The position of the `nth` occurrence of `needle`, plus `offset` bytes.
fn pos(source: &str, needle: &str, nth: usize, offset: usize) -> Position {
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
    assert_eq!(
        accepted(rust, RefSyntax::Call),
        [Function, Method, Class, Struct, Macro]
    );
    assert_eq!(
        accepted(ts, RefSyntax::MemberCall),
        accepted(rust, RefSyntax::Call)
    );
    assert_eq!(accepted(rust, RefSyntax::Construct), [Class, Struct]);
    assert_eq!(
        accepted(rust, RefSyntax::Type),
        [Class, Interface, Struct, Enum, Trait, Type, Module]
    );
    assert_eq!(
        accepted(rust, RefSyntax::Member),
        [Method, Constant, Field, Variable]
    );
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
    // Go conversions are call syntax naming a type.
    assert_eq!(
        accepted(go, RefSyntax::Call),
        [Function, Method, Class, Struct, Type, Macro]
    );
    assert!(accepts(go, RefSyntax::MemberCall, Type));
    assert!(!accepts(rust, RefSyntax::Call, Type));
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
