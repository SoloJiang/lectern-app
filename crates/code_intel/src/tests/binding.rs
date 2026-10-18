//! `site_binding_*`: the binding rules of docs/design/code_intel.md
//! 「绑定规则」, one table per language. Each row names a reference (an
//! occurrence of a name) and what it resolves to: a certain binding, only
//! same-file candidates (no direct jump), or nothing local.

use super::site::{parse, pos};
use crate::site::Position;

/// An occurrence of `needle`, plus `offset` bytes.
pub(super) type At = (&'static str, usize, usize);

pub(super) enum Expect {
    /// A certain binding to this declaration.
    Binds(At),
    /// No binding; these declarations are candidates (no direct jump).
    Candidates(&'static [At]),
    /// Nothing local: the index answers.
    Free,
}

pub(super) use Expect::{Binds, Candidates, Free};

pub(super) struct Row {
    pub what: &'static str,
    pub source: &'static str,
    /// The reference: an occurrence of a name.
    pub reference: (&'static str, usize),
    pub expect: Expect,
}

pub(super) fn check(lang: &str, rows: &[Row]) {
    for row in rows {
        let file = parse(lang, row.source);
        let at = |(needle, nth, offset): At| pos(row.source, needle, nth, offset);
        let (needle, nth) = row.reference;
        let site = file
            .site_at(pos(row.source, needle, nth, 0))
            .unwrap_or_else(|| panic!("{lang} {}: no site", row.what));
        assert!(
            site.defines.is_none(),
            "{lang} {}: a declaration, not a use",
            row.what
        );
        let bound = site.binding.as_ref().map(|b| b.at);
        let possible: Vec<Position> = site.possible.iter().map(|b| b.at).collect();
        match &row.expect {
            Binds(decl) => {
                assert_eq!(bound, Some(at(*decl)), "{lang} {}", row.what);
                assert!(possible.is_empty(), "{lang} {}", row.what);
            }
            Candidates(decls) => {
                assert_eq!(bound, None, "{lang} {}: must not bind", row.what);
                let mut want: Vec<Position> = decls.iter().map(|d| at(*d)).collect();
                let mut got = possible.clone();
                want.sort_by_key(|p| (p.line, p.column));
                got.sort_by_key(|p| (p.line, p.column));
                assert_eq!(got, want, "{lang} {}", row.what);
                assert!(!site.may_jump_elsewhere(), "{lang} {}", row.what);
            }
            Free => {
                assert_eq!(bound, None, "{lang} {}", row.what);
                assert!(possible.is_empty(), "{lang} {}", row.what);
                assert!(site.may_jump_elsewhere(), "{lang} {}", row.what);
            }
        }
    }
}

#[test]
fn site_binding_javascript() {
    check(
        "js",
        &[
            Row {
                what: "a function declaration is hoisted",
                source: "function main() { return helper(); }\nfunction helper() { return 1; }\n",
                reference: ("helper", 0),
                expect: Binds(("helper", 1, 0)),
            },
            Row {
                what: "var binds the whole function",
                source: "function f(ok) {\n  if (ok) { var value = 1; }\n  return value;\n}\n",
                reference: ("value", 1),
                expect: Binds(("value", 0, 0)),
            },
            Row {
                what: "let binds its block only",
                source: "function f(ok) {\n  if (ok) { let inner = 1; }\n  return inner;\n}\n",
                reference: ("inner", 1),
                expect: Free,
            },
            Row {
                what: "an inner const shadows inside its block",
                source: "const shade = 1;\nfunction f() {\n  { const shade = 2; use(shade); }\n  return shade;\n}\n",
                reference: ("shade", 2),
                expect: Binds(("shade", 1, 0)),
            },
            Row {
                what: "outside the inner block the outer const",
                source: "const shade = 1;\nfunction f() {\n  { const shade = 2; use(shade); }\n  return shade;\n}\n",
                reference: ("shade", 3),
                expect: Binds(("shade", 0, 0)),
            },
            Row {
                what: "a closure may use a const declared after it",
                source: "function g() { return LIMIT; }\nconst LIMIT = 3;\n",
                reference: ("LIMIT", 0),
                expect: Binds(("LIMIT", 1, 0)),
            },
            Row {
                what: "an object pattern binds its names",
                source: "function f(obj) {\n  const { alpha, beta: [gamma], delta = fallback } = obj;\n  return alpha + gamma + delta;\n}\n",
                reference: ("gamma", 1),
                expect: Binds(("gamma", 0, 0)),
            },
            Row {
                what: "a pattern default is a use, not a binding",
                source: "function f(obj) {\n  const { delta = fallback } = obj;\n  return fallback;\n}\n",
                reference: ("fallback", 1),
                expect: Free,
            },
            Row {
                what: "a destructuring assignment declares nothing",
                source: "let first;\nfunction f(pair) {\n  [first] = pair;\n  return first;\n}\n",
                reference: ("first", 2),
                expect: Binds(("first", 0, 0)),
            },
            Row {
                what: "a for-of const binds the loop only",
                source: "function f(xs) {\n  for (const item of xs) { use(item); }\n  return item;\n}\n",
                reference: ("item", 2),
                expect: Free,
            },
            Row {
                what: "the for-of const inside the loop",
                source: "function f(xs) {\n  for (const item of xs) { use(item); }\n  return item;\n}\n",
                reference: ("item", 1),
                expect: Binds(("item", 0, 0)),
            },
        ],
    );
}

#[test]
fn site_binding_typescript() {
    check(
        "ts",
        &[
            Row {
                what: "a typed object-pattern parameter",
                source: "function f({ alpha, beta }: Opts, gamma: number = DEFAULT) {\n  return alpha + gamma;\n}\n",
                reference: ("alpha", 1),
                expect: Binds(("alpha", 0, 0)),
            },
            Row {
                what: "a typed parameter with a default",
                source: "function f({ alpha, beta }: Opts, gamma: number = DEFAULT) {\n  return alpha + gamma;\n}\n",
                reference: ("gamma", 1),
                expect: Binds(("gamma", 0, 0)),
            },
            Row {
                what: "the default value is a use",
                source: "function f(gamma: number = DEFAULT) {\n  return DEFAULT;\n}\n",
                reference: ("DEFAULT", 1),
                expect: Free,
            },
        ],
    );
}

#[test]
fn site_binding_python() {
    check(
        "python",
        &[
            Row {
                what: "a method does not see the class namespace",
                source: "def run():\n    return 1\n\nclass Service:\n    def run(self):\n        return run()\n\ndef main():\n    return run()\n",
                reference: ("run", 2),
                expect: Binds(("run", 0, 0)),
            },
            Row {
                what: "a method name does not leak into module scope",
                source: "def run():\n    return 1\n\nclass Service:\n    def run(self):\n        return run()\n\ndef main():\n    return run()\n",
                reference: ("run", 3),
                expect: Binds(("run", 0, 0)),
            },
            Row {
                what: "a class attribute is not visible in a method",
                source: "class Config:\n    limit = 3\n    def size(self):\n        return limit\n",
                reference: ("limit", 1),
                expect: Free,
            },
            Row {
                what: "the class body sees its own attributes",
                source: "class Config:\n    limit = 3\n    doubled = limit * 2\n",
                reference: ("limit", 1),
                expect: Binds(("limit", 0, 0)),
            },
            Row {
                what: "a nested class body does not see the outer class",
                source: "limit = 1\nclass Outer:\n    limit = 2\n    class Inner:\n        size = limit\n",
                reference: ("limit", 2),
                expect: Binds(("limit", 0, 0)),
            },
            Row {
                what: "a function-level name binds before its assignment",
                source: "def f():\n    print(total)\n    total = 1\n",
                reference: ("total", 0),
                expect: Binds(("total", 1, 0)),
            },
            Row {
                what: "tuple targets of a for loop",
                source: "def f(pairs):\n    for key, value in pairs:\n        use(key, value)\n",
                reference: ("value", 1),
                expect: Binds(("value", 0, 0)),
            },
            Row {
                what: "nested and starred assignment targets",
                source: "def f(pairs):\n    first, (second, *rest) = pairs\n    return second, rest\n",
                reference: ("rest", 1),
                expect: Binds(("rest", 0, 0)),
            },
            Row {
                what: "an attribute target binds nothing",
                source: "def f(obj):\n    obj.count = 1\n    return count\n",
                reference: ("count", 1),
                expect: Free,
            },
            Row {
                what: "global takes the name out of the function",
                source: "counter = 0\ndef bump():\n    global counter\n    counter = counter + 1\n",
                reference: ("counter", 3),
                expect: Binds(("counter", 0, 0)),
            },
            Row {
                what: "a comprehension variable has its own scope",
                source: "def f(items):\n    elem = 0\n    return [elem for elem in items]\n",
                reference: ("elem", 1),
                expect: Binds(("elem", 2, 0)),
            },
            Row {
                what: "an assignment expression in a comprehension binds the function",
                source: "def f(items):\n    if any((hit := x) > 3 for x in items):\n        return hit\n",
                reference: ("hit", 1),
                expect: Binds(("hit", 0, 0)),
            },
            Row {
                what: "a parameter default is a use",
                source: "def g(arg=limit):\n    return arg\n",
                reference: ("limit", 0),
                expect: Free,
            },
        ],
    );
}

#[test]
fn site_binding_rust() {
    check(
        "rust",
        &[
            Row {
                what: "a shadowing let reads the earlier binding",
                source: "fn f() -> u8 {\n    let x = 1;\n    let x = x + 1;\n    x\n}\n",
                reference: ("x", 2),
                expect: Binds(("x", 0, 0)),
            },
            Row {
                what: "after the shadowing let, the new binding",
                source: "fn f() -> u8 {\n    let x = 1;\n    let x = x + 1;\n    x\n}\n",
                reference: ("x", 3),
                expect: Binds(("x", 1, 0)),
            },
            Row {
                what: "a use before a later let is not bound to it",
                source: "fn f() -> u8 {\n    let y = outer;\n    let outer = 2;\n    outer + y\n}\n",
                reference: ("outer", 0),
                expect: Candidates(&[("outer", 1, 0)]),
            },
            Row {
                what: "a tuple pattern",
                source: "fn f(pair: (u8, u8)) -> u8 {\n    let (left, right) = pair;\n    left + right\n}\n",
                reference: ("right", 1),
                expect: Binds(("right", 0, 0)),
            },
            Row {
                what: "if let binds inside the if",
                source: "fn f(item: Option<u8>) -> u8 {\n    if let Some(value) = item {\n        return value;\n    }\n    0\n}\n",
                reference: ("value", 1),
                expect: Binds(("value", 0, 0)),
            },
            Row {
                what: "the else of an if let does not see its binding",
                source: "fn f(item: Option<u8>) -> u8 {\n    let value = 1;\n    if let Some(value) = item {\n        value\n    } else {\n        value\n    }\n}\n",
                reference: ("value", 3),
                expect: Binds(("value", 0, 0)),
            },
            Row {
                what: "a while let binds its body only",
                source: "fn f(mut it: I) -> u8 {\n    let item = 0;\n    while let Some(item) = it.next() {\n        take(item);\n    }\n    item\n}\n",
                reference: ("item", 3),
                expect: Binds(("item", 0, 0)),
            },
            Row {
                what: "inside the while let body, its binding",
                source: "fn f(mut it: I) -> u8 {\n    let item = 0;\n    while let Some(item) = it.next() {\n        take(item);\n    }\n    item\n}\n",
                reference: ("item", 2),
                expect: Binds(("item", 1, 0)),
            },
            Row {
                what: "a let chain sees the earlier let",
                source: "fn f(item: Option<u8>) -> u8 {\n    let value = 1;\n    if let Some(value) = item && value > 3 {\n        return value;\n    }\n    0\n}\n",
                reference: ("value", 2),
                expect: Binds(("value", 1, 0)),
            },
            Row {
                what: "the constructor of a pattern is not a binding",
                source: "fn f(item: Option<u8>) -> u8 {\n    if let Some(value) = item {\n        return value;\n    }\n    Some(1).unwrap()\n}\n",
                reference: ("Some", 1),
                expect: Free,
            },
            Row {
                what: "a for pattern binds the body, not the iterable",
                source: "fn f(x: Vec<u8>) {\n    for x in x.iter() {\n        drop(x);\n    }\n}\n",
                reference: ("x", 2),
                expect: Binds(("x", 0, 0)),
            },
            Row {
                what: "the for pattern inside the body",
                source: "fn f(x: Vec<u8>) {\n    for x in x.iter() {\n        drop(x);\n    }\n}\n",
                reference: ("x", 3),
                expect: Binds(("x", 1, 0)),
            },
            Row {
                what: "a match arm binding is visible in its guard",
                source: "fn f(item: Option<u8>) -> u8 {\n    match item {\n        Some(found) if found > 0 => found,\n        None => 0,\n        _ => 0,\n    }\n}\n",
                reference: ("found", 1),
                expect: Binds(("found", 0, 0)),
            },
            Row {
                what: "a unit variant in a pattern is not a binding",
                source: "fn f(item: Option<u8>) -> u8 {\n    match item {\n        None => 0,\n        _ => 0,\n    }\n}\nconst X: Option<u8> = None;\n",
                reference: ("None", 1),
                expect: Free,
            },
            Row {
                what: "a struct pattern binds field shorthands and renames",
                source: "fn f(p: Point) -> u8 {\n    let Point { x: px, ys } = p;\n    px + ys\n}\n",
                reference: ("px", 1),
                expect: Binds(("px", 0, 0)),
            },
            Row {
                what: "an item in a block is visible in the whole block",
                source: "fn outer() -> u8 {\n    let r = helper();\n    fn helper() -> u8 { 1 }\n    r\n}\n",
                reference: ("helper", 0),
                expect: Binds(("helper", 1, 0)),
            },
        ],
    );
}

#[test]
fn site_binding_java() {
    check(
        "java",
        &[
            Row {
                what: "a use before a later local names the field",
                source: "class A {\n  int value;\n  void f() {\n    use(value);\n    int value = 1;\n    use(value);\n  }\n}\n",
                reference: ("value", 1),
                expect: Candidates(&[("value", 2, 0)]),
            },
            Row {
                what: "after the local, the local",
                source: "class A {\n  int value;\n  void f() {\n    use(value);\n    int value = 1;\n    use(value);\n  }\n}\n",
                reference: ("value", 3),
                expect: Binds(("value", 2, 0)),
            },
            Row {
                what: "a block local ends with its block",
                source: "class A {\n  void f(boolean ok) {\n    if (ok) { int inner = 1; }\n    use(inner);\n  }\n}\n",
                reference: ("inner", 1),
                expect: Free,
            },
            Row {
                what: "a for-each variable binds the body",
                source: "class A {\n  void f(java.util.List<String> items) {\n    for (String each : items) { use(each); }\n  }\n}\n",
                reference: ("each", 1),
                expect: Binds(("each", 0, 0)),
            },
            Row {
                what: "an instanceof pattern is only a candidate",
                source: "class A {\n  void f(Object obj) {\n    if (obj instanceof String text) { use(text); }\n  }\n}\n",
                reference: ("text", 1),
                expect: Candidates(&[("text", 0, 0)]),
            },
            Row {
                what: "a lambda parameter",
                source: "class A {\n  void f() {\n    run(item -> use(item));\n  }\n}\n",
                reference: ("item", 1),
                expect: Binds(("item", 0, 0)),
            },
            Row {
                what: "a local class",
                source: "class A {\n  void f() {\n    class Local {}\n    new Local();\n  }\n}\n",
                reference: ("Local", 1),
                expect: Binds(("Local", 0, 0)),
            },
        ],
    );
}

#[test]
fn site_binding_go() {
    check(
        "go",
        &[
            Row {
                what: "an inner := reads the outer variable",
                source: "package p\n\nfunc f() int {\n\tx := 1\n\tif true {\n\t\tx := x + 1\n\t\t_ = x\n\t}\n\treturn x\n}\n",
                reference: ("x", 2),
                expect: Binds(("x", 0, 0)),
            },
            Row {
                what: "after the inner :=, the inner variable",
                source: "package p\n\nfunc f() int {\n\tx := 1\n\tif true {\n\t\tx := x + 1\n\t\t_ = x\n\t}\n\treturn x\n}\n",
                reference: ("x", 3),
                expect: Binds(("x", 1, 0)),
            },
            Row {
                what: "after the block, the outer variable",
                source: "package p\n\nfunc f() int {\n\tx := 1\n\tif true {\n\t\tx := x + 1\n\t\t_ = x\n\t}\n\treturn x\n}\n",
                reference: ("x", 4),
                expect: Binds(("x", 0, 0)),
            },
            Row {
                what: "a use before a later := is not bound to it",
                source: "package p\n\nfunc f() int {\n\ty := later\n\tlater := 2\n\treturn later + y\n}\n",
                reference: ("later", 0),
                expect: Candidates(&[("later", 1, 0)]),
            },
            Row {
                what: "package-level names do not depend on order",
                source: "package p\n\nfunc f() int { return limit }\n\nvar limit = 3\n",
                reference: ("limit", 0),
                expect: Binds(("limit", 1, 0)),
            },
            Row {
                what: "range variables and multiple assignment",
                source: "package p\n\nfunc f(m map[string]int) {\n\tfor key, val := range m {\n\t\tuse(key, val)\n\t}\n\talpha, beta := 1, 2\n\tuse(alpha, beta)\n}\n",
                reference: ("beta", 1),
                expect: Binds(("beta", 0, 0)),
            },
            Row {
                what: "an if initializer ends with the if",
                source: "package p\n\nfunc f() {\n\tif got := g(); got > 0 {\n\t\tuse(got)\n\t}\n\tuse(got)\n}\n",
                reference: ("got", 3),
                expect: Free,
            },
            Row {
                what: "the if initializer inside the if",
                source: "package p\n\nfunc f() {\n\tif got := g(); got > 0 {\n\t\tuse(got)\n\t}\n\tuse(got)\n}\n",
                reference: ("got", 2),
                expect: Binds(("got", 0, 0)),
            },
        ],
    );
}

#[test]
fn site_binding_entry_points() {
    // docs/design/code_intel.md「绑定规则」, 作用域入口: parts of a scope's
    // node evaluated in the scope around it.
    check(
        "python",
        &[
            Row {
                what: "a default value is evaluated outside the function",
                source: "value = 1\ndef f(value=value):\n    return value\n",
                reference: ("value", 2),
                expect: Binds(("value", 0, 0)),
            },
            Row {
                what: "the parameter inside the function",
                source: "value = 1\ndef f(value=value):\n    return value\n",
                reference: ("value", 3),
                expect: Binds(("value", 1, 0)),
            },
            Row {
                what: "a lambda's default is evaluated outside the lambda",
                source: "item = 1\nget = lambda item=item: item\n",
                reference: ("item", 2),
                expect: Binds(("item", 0, 0)),
            },
            Row {
                what: "the lambda's parameter inside it",
                source: "item = 1\nget = lambda item=item: item\n",
                reference: ("item", 3),
                expect: Binds(("item", 1, 0)),
            },
            Row {
                what: "an annotation is evaluated outside the function",
                source: "Kind = int\ndef f(Kind: Kind) -> Kind:\n    return Kind\n",
                reference: ("Kind", 2),
                expect: Binds(("Kind", 0, 0)),
            },
            Row {
                what: "a return annotation is evaluated outside the function",
                source: "Kind = int\ndef f(Kind: Kind) -> Kind:\n    return Kind\n",
                reference: ("Kind", 3),
                expect: Binds(("Kind", 0, 0)),
            },
            Row {
                what: "the annotated parameter inside the function",
                source: "Kind = int\ndef f(Kind: Kind) -> Kind:\n    return Kind\n",
                reference: ("Kind", 4),
                expect: Binds(("Kind", 1, 0)),
            },
            Row {
                what: "a typed default and its annotation are outside",
                source: "limit = 3\ndef f(limit: int = limit):\n    return limit\n",
                reference: ("limit", 2),
                expect: Binds(("limit", 0, 0)),
            },
            Row {
                what: "a method's default sees the class body",
                source: "class A:\n    size = 3\n    def grow(self, by=size):\n        return by\n",
                reference: ("size", 1),
                expect: Binds(("size", 0, 0)),
            },
            Row {
                what: "a base class is evaluated outside the class body",
                source: "class A(Mixin):\n    Mixin = 1\n",
                reference: ("Mixin", 0),
                expect: Free,
            },
            Row {
                what: "a comprehension's first iterable is evaluated outside it",
                source: "def f(xs):\n    return [xs for xs in xs]\n",
                reference: ("xs", 3),
                expect: Binds(("xs", 0, 0)),
            },
            Row {
                what: "the comprehension variable inside it",
                source: "def f(xs):\n    return [xs for xs in xs]\n",
                reference: ("xs", 1),
                expect: Binds(("xs", 2, 0)),
            },
            Row {
                what: "a lambda in a default value does not see the function",
                source: "def f(arg, cb=lambda: arg):\n    return cb\n",
                reference: ("arg", 1),
                expect: Free,
            },
        ],
    );
    for lang in ["js", "ts"] {
        check(
            lang,
            &[
                Row {
                    what: "a parameter default does not see the body's var",
                    source: "const x = 1;\nfunction f(a = x) { var x = 2; return a + x; }\n",
                    reference: ("x", 1),
                    expect: Binds(("x", 0, 0)),
                },
                Row {
                    what: "the body sees its var",
                    source: "const x = 1;\nfunction f(a = x) { var x = 2; return a + x; }\n",
                    reference: ("x", 3),
                    expect: Binds(("x", 2, 0)),
                },
                Row {
                    what: "a var in a nested block is the function's",
                    source: "function f() { if (ok) { var hit = 1; } return hit; }\n",
                    reference: ("hit", 1),
                    expect: Binds(("hit", 0, 0)),
                },
                Row {
                    what: "a computed method name is evaluated outside the method",
                    source: "const key = 'k';\nclass A { [key](key) { return key; } }\n",
                    reference: ("key", 1),
                    expect: Binds(("key", 0, 0)),
                },
                Row {
                    what: "the method's parameter inside it",
                    source: "const key = 'k';\nclass A { [key](key) { return key; } }\n",
                    reference: ("key", 3),
                    expect: Binds(("key", 2, 0)),
                },
            ],
        );
    }
    check(
        "ts",
        &[Row {
            what: "a parameter decorator is evaluated outside the method",
            source: "const token = 1;\nclass A { constructor(@Inject(token) token: string) {} }\n",
            reference: ("token", 1),
            expect: Binds(("token", 0, 0)),
        }],
    );
}

#[test]
fn site_binding_directives() {
    // docs/design/code_intel.md「绑定规则」, 作用域指令: statements that move
    // a name to another scope (Python's `global` / `nonlocal`) or make
    // names resolve at run time (JS `with`).
    check(
        "python",
        &[
            Row {
                what: "global skips every enclosing function",
                source: "counter = 0\ndef outer():\n    counter = 1\n    def inner():\n        global counter\n        return counter\n",
                reference: ("counter", 3),
                expect: Binds(("counter", 0, 0)),
            },
            Row {
                what: "nonlocal stops at the nearest enclosing function",
                source: "counter = 0\ndef outer():\n    counter = 1\n    def inner():\n        nonlocal counter\n        return counter\n",
                reference: ("counter", 3),
                expect: Binds(("counter", 1, 0)),
            },
            Row {
                what: "nonlocal passes an enclosing function without the name",
                source: "def a():\n    total = 0\n    def b():\n        def c():\n            nonlocal total\n            return total\n",
                reference: ("total", 2),
                expect: Binds(("total", 0, 0)),
            },
            Row {
                what: "nonlocal never binds the module's name",
                source: "total = 0\ndef a():\n    def b():\n        nonlocal total\n        return total\n",
                reference: ("total", 2),
                expect: Free,
            },
            Row {
                what: "a function inside one that declares global sees the module's name",
                source: "counter = 0\ndef outer():\n    global counter\n    counter = 1\n    def inner():\n        return counter\n",
                reference: ("counter", 3),
                expect: Binds(("counter", 0, 0)),
            },
            Row {
                what: "global at module level changes nothing",
                source: "global counter\ncounter = 0\nuse(counter)\n",
                reference: ("counter", 2),
                expect: Binds(("counter", 1, 0)),
            },
        ],
    );
    check(
        "js",
        &[
            Row {
                what: "an outer name in a with body is only possible",
                source: "function f(obj) {\n  const size = 1;\n  with (obj) { use(size); }\n}\n",
                reference: ("size", 1),
                expect: Candidates(&[("size", 0, 0)]),
            },
            Row {
                what: "a var in a with body may still be the object's property",
                source: "function f(obj) {\n  with (obj) { var total = 1; use(total); }\n}\n",
                reference: ("total", 1),
                expect: Candidates(&[("total", 0, 0)]),
            },
        ],
    );
}

#[test]
fn site_binding_namespaces() {
    // Type positions resolve to local types (type parameters, local type
    // declarations), other positions to local values.
    check(
        "ts",
        &[
            Row {
                what: "a type parameter in its function",
                source: "type Item = string;\nfunction f<Item>(x: Item): Item { return x; }\nconst y: Item = 'a';\n",
                reference: ("Item", 2),
                expect: Binds(("Item", 1, 0)),
            },
            Row {
                what: "outside the function, the type alias",
                source: "type Item = string;\nfunction f<Item>(x: Item): Item { return x; }\nconst y: Item = 'a';\n",
                reference: ("Item", 4),
                expect: Binds(("Item", 0, 0)),
            },
            Row {
                what: "a value position does not see a type parameter",
                source: "const Item = 1;\nfunction f<Item>(x: Item) { return Item; }\n",
                reference: ("Item", 3),
                expect: Binds(("Item", 0, 0)),
            },
            Row {
                what: "a class's type parameter in its methods",
                source: "class Box<Item> { get(): Item { return this.v; } }\n",
                reference: ("Item", 1),
                expect: Binds(("Item", 0, 0)),
            },
        ],
    );
    check(
        "rust",
        &[
            Row {
                what: "a type parameter in its function",
                source: "struct T;\nfn f<T>(x: T) -> T { x }\nfn g(y: T) {}\n",
                reference: ("T", 2),
                expect: Binds(("T", 1, 0)),
            },
            Row {
                what: "outside the function the index answers",
                source: "struct T;\nfn f<T>(x: T) -> T { x }\nfn g(y: T) {}\n",
                reference: ("T", 4),
                expect: Free,
            },
            Row {
                what: "an impl's type parameter in its methods",
                source: "struct W<T>(T);\nimpl<T> W<T> { fn get(&self) -> &T { &self.0 } }\n",
                reference: ("T", 4),
                expect: Binds(("T", 2, 0)),
            },
        ],
    );
    check(
        "java",
        &[
            Row {
                what: "a class's type parameter",
                source: "class Box<T> { T get() { return null; } }\nclass Other { T t; }\n",
                reference: ("T", 1),
                expect: Binds(("T", 0, 0)),
            },
            Row {
                what: "outside the class the index answers",
                source: "class Box<T> { T get() { return null; } }\nclass Other { T t; }\n",
                reference: ("T", 2),
                expect: Free,
            },
            Row {
                what: "a record component is a field, not a local",
                source: "record Point(int value) {}\nclass A { int f() { return value; } }\n",
                reference: ("value", 1),
                expect: Free,
            },
        ],
    );
    check(
        "go",
        &[
            Row {
                what: "a type parameter in its function",
                source: "package p\n\nfunc Map[T any](xs []T) []T { return xs }\n\nfunc g(y T) {}\n",
                reference: ("T", 1),
                expect: Binds(("T", 0, 0)),
            },
            Row {
                what: "outside the function the index answers",
                source: "package p\n\nfunc Map[T any](xs []T) []T { return xs }\n\nfunc g(y T) {}\n",
                reference: ("T", 3),
                expect: Free,
            },
        ],
    );
}
