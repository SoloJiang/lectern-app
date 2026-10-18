//! `scopes_*`: the scope-form table (docs/design/code_intel.md「作用域形式」),
//! the counterpart of the definition forms: every node kind of a core
//! grammar that looks like it may open a lexical scope, whether the locals
//! query opens one there and of which kind, what it binds, and which of its
//! parts are evaluated in the scope around it. The tests check the table
//! against the grammars, the locals queries, the binding model and the
//! design document.

use std::collections::BTreeSet;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, QueryCursor};

use super::binding::{Binds, Candidates, Free, Row, check};
use super::forms::node_types;
use crate::lang::Capture;

const LANGS: &[&str] = &["ts", "tsx", "js", "rust", "java", "python", "go"];

/// What the locals query makes of a node kind.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Opens {
    /// A function scope: does not inherit for tree-sitter-tags; `var` and
    /// Python's `:=` bind here.
    Function,
    /// A block scope.
    Block,
    /// A Python class body: visible to code directly in it only.
    Class,
    /// A TS namespace or module body: a block whose declarations are
    /// members, not locals.
    Namespace,
    /// A JS `with` body: what is found outside it is only possible.
    Dynamic,
    /// No scope of its own.
    No,
}

use Opens::{Block, Class, Dynamic, Function, Namespace, No};

struct ScopeForm {
    langs: &'static [&'static str],
    kind: &'static str,
    opens: Opens,
    /// What it binds, or why it opens no scope.
    binds: &'static str,
    /// Its parts evaluated in the scope around it.
    outside: &'static str,
}

const fn row(
    langs: &'static [&'static str],
    kind: &'static str,
    opens: Opens,
    binds: &'static str,
    outside: &'static str,
) -> ScopeForm {
    ScopeForm {
        langs,
        kind,
        opens,
        binds,
        outside,
    }
}

const ECMA: &[&str] = &["js", "ts", "tsx"];
const TS: &[&str] = &["ts", "tsx"];
const RUST: &[&str] = &["rust"];
const JAVA: &[&str] = &["java"];
const PYTHON: &[&str] = &["python"];
const GO: &[&str] = &["go"];

const SCOPES: &[ScopeForm] = &[
    // ---- JavaScript, TypeScript, TSX ----
    row(ECMA, "arrow_function", Function, "参数", ""),
    row(ECMA, "catch_clause", Block, "`catch (e)` 的参数", ""),
    row(
        ECMA,
        "class",
        No,
        "类表达式的名字只在类里可见（不建模，经索引找到）",
        "",
    ),
    row(
        ECMA,
        "class_body",
        No,
        "成员经 `this` 访问，不是局部绑定",
        "",
    ),
    row(ECMA, "class_declaration", No, "类名绑定在外层的块里", ""),
    row(
        ECMA,
        "class_static_block",
        No,
        "它的 `statement_block` 开块作用域",
        "",
    ),
    row(ECMA, "do_statement", No, "循环体的块开作用域", ""),
    row(ECMA, "else_clause", No, "", ""),
    row(ECMA, "export_clause", No, "导出名，不绑定", ""),
    row(ECMA, "export_statement", No, "", ""),
    row(ECMA, "finally_clause", No, "", ""),
    row(
        ECMA,
        "for_in_statement",
        Block,
        "`for (const x of …)` / `for (let k in …)` 的变量",
        "",
    ),
    row(ECMA, "for_statement", Block, "`for (let i …)` 的变量", ""),
    row(
        ECMA,
        "function_declaration",
        Function,
        "参数；函数名绑定在外层（提升）",
        "",
    ),
    row(
        ECMA,
        "function_expression",
        Function,
        "参数；名字只在函数里可见（不建模，经索引找到）",
        "",
    ),
    row(
        ECMA,
        "generator_function",
        Function,
        "同 `function_expression`",
        "",
    ),
    row(
        ECMA,
        "generator_function_declaration",
        Function,
        "同 `function_declaration`",
        "",
    ),
    row(ECMA, "if_statement", No, "", ""),
    row(ECMA, "import_clause", No, "import 经索引解析", ""),
    row(ECMA, "labeled_statement", No, "标签不是绑定", ""),
    row(
        ECMA,
        "method_definition",
        Function,
        "参数",
        "计算出的方法名（`[key]() {}`）；TS 的参数装饰器",
    ),
    row(
        ECMA,
        "statement_block",
        Block,
        "`let` / `const` / `class` / 函数声明；TS 的 namespace / module 体是命名空间",
        "",
    ),
    row(
        ECMA,
        "switch_body",
        Block,
        "各 `case` 里的 `let` / `const`：整个 switch 体",
        "",
    ),
    row(ECMA, "switch_case", No, "属于 `switch_body`", ""),
    row(ECMA, "switch_default", No, "属于 `switch_body`", ""),
    row(ECMA, "switch_statement", No, "`switch_body` 开块作用域", ""),
    row(ECMA, "ternary_expression", No, "", ""),
    row(ECMA, "try_statement", No, "", ""),
    row(ECMA, "while_statement", No, "", ""),
    row(
        ECMA,
        "with_statement",
        Dynamic,
        "体是动态作用域：体里声明的名字照常绑定，体外的声明只是候选（名字可能是对象的属性）",
        "对象表达式",
    ),
    row(
        TS,
        "abstract_class_declaration",
        No,
        "类名绑定在外层的块里",
        "",
    ),
    row(
        TS,
        "abstract_method_signature",
        Function,
        "参数只在签名里",
        "",
    ),
    row(
        TS,
        "ambient_declaration",
        Namespace,
        "`declare global { … }` 的块是命名空间，同 `internal_module`",
        "",
    ),
    row(TS, "call_signature", Function, "参数只在签名里", ""),
    row(TS, "conditional_type", No, "`infer U` 的类型名不建模", ""),
    row(TS, "construct_signature", Function, "参数只在签名里", ""),
    row(TS, "constructor_type", Function, "参数只在类型里", ""),
    row(TS, "enum_body", No, "成员不是局部绑定", ""),
    row(TS, "enum_declaration", No, "枚举名绑定在外层的块里", ""),
    row(TS, "extends_clause", No, "", ""),
    row(TS, "extends_type_clause", No, "", ""),
    row(TS, "function_signature", Function, "参数只在签名里", ""),
    row(TS, "function_type", Function, "参数只在类型里", ""),
    row(TS, "implements_clause", No, "", ""),
    row(TS, "import_require_clause", No, "import 经索引解析", ""),
    row(TS, "interface_body", No, "成员不是局部绑定", ""),
    row(
        TS,
        "interface_declaration",
        No,
        "接口名绑定在外层的块里（类型）",
        "",
    ),
    row(
        TS,
        "internal_module",
        Namespace,
        "体（`statement_block`）是命名空间：照常绑定，但其中的定义是成员（`NS.x`），不是局部",
        "",
    ),
    row(
        TS,
        "mapped_type_clause",
        No,
        "键名只在映射类型里（`owner` 规则）",
        "",
    ),
    row(TS, "method_signature", Function, "参数只在签名里", ""),
    row(TS, "module", Namespace, "同 `internal_module`", ""),
    // ---- Rust ----
    row(RUST, "async_block", No, "它的 `block` 开块作用域", ""),
    row(RUST, "base_field_initializer", No, "", ""),
    row(
        RUST,
        "block",
        Block,
        "`let`（从声明结束起）、块里的项（整个块）",
        "",
    ),
    row(RUST, "closure_expression", Function, "参数", ""),
    row(RUST, "const_block", No, "它的 `block` 开块作用域", ""),
    row(
        RUST,
        "else_clause",
        No,
        "`if let` 的模式在 `else` 里不可见（见 `let_condition`）",
        "",
    ),
    row(RUST, "enum_item", No, "变体是定义，不是局部", ""),
    row(RUST, "enum_variant", No, "", ""),
    row(RUST, "field_initializer", No, "", ""),
    row(
        RUST,
        "for_expression",
        Block,
        "模式，从循环体开始",
        "被迭代的表达式",
    ),
    row(RUST, "foreign_mod_item", No, "项是定义，不是局部", ""),
    row(RUST, "function_item", Function, "参数", ""),
    row(
        RUST,
        "function_signature_item",
        Function,
        "参数只在签名里",
        "",
    ),
    row(RUST, "function_type", Function, "参数只在类型里", ""),
    row(RUST, "gen_block", No, "它的 `block` 开块作用域", ""),
    row(RUST, "if_expression", No, "`if let` 见 `let_condition`", ""),
    row(RUST, "impl_item", No, "项是定义，不是局部", ""),
    row(
        RUST,
        "let_condition",
        No,
        "模式从条件结束起，到 `else` 之前（`while let`：到循环结束）",
        "",
    ),
    row(
        RUST,
        "let_declaration",
        No,
        "模式从声明结束起；`let … else` 的 `else` 块看不到它",
        "",
    ),
    row(RUST, "loop_expression", No, "循环体的块开作用域", ""),
    row(RUST, "macro_rule", No, "宏的元变量不建模", ""),
    row(RUST, "match_arm", Block, "模式（守卫和分支体里）", ""),
    row(RUST, "match_block", No, "", ""),
    row(RUST, "match_expression", No, "", ""),
    row(RUST, "match_pattern", No, "属于 `match_arm`", ""),
    row(RUST, "mod_item", No, "项是定义，不是局部", ""),
    row(RUST, "shorthand_field_initializer", No, "", ""),
    row(RUST, "struct_expression", No, "", ""),
    row(RUST, "struct_item", No, "", ""),
    row(RUST, "trait_item", No, "项是定义，不是局部", ""),
    row(RUST, "try_block", No, "它的 `block` 开块作用域", ""),
    row(RUST, "union_item", No, "", ""),
    row(RUST, "unsafe_block", No, "它的 `block` 开块作用域", ""),
    row(RUST, "use_as_clause", No, "import 经索引解析", ""),
    row(RUST, "where_clause", No, "", ""),
    row(
        RUST,
        "while_expression",
        No,
        "`while let` 见 `let_condition`",
        "",
    ),
    // ---- Java ----
    row(JAVA, "annotation_type_body", No, "成员", ""),
    row(JAVA, "annotation_type_declaration", No, "", ""),
    row(JAVA, "array_initializer", No, "", ""),
    row(
        JAVA,
        "block",
        Block,
        "局部变量（从声明符结束起）、局部类",
        "",
    ),
    row(JAVA, "catch_clause", Block, "catch 参数", ""),
    row(JAVA, "class_body", No, "成员", ""),
    row(JAVA, "class_declaration", No, "", ""),
    row(
        JAVA,
        "compact_constructor_declaration",
        No,
        "体是块；记录的分量是字段，不建模为参数",
        "",
    ),
    row(JAVA, "constructor_body", No, "属于构造器的函数作用域", ""),
    row(JAVA, "constructor_declaration", Function, "参数", ""),
    row(JAVA, "do_statement", No, "", ""),
    row(JAVA, "element_value_array_initializer", No, "", ""),
    row(
        JAVA,
        "enhanced_for_statement",
        Block,
        "循环变量，从循环体开始",
        "被迭代的表达式",
    ),
    row(JAVA, "enum_body", No, "成员", ""),
    row(JAVA, "enum_constant", No, "常量的类体是成员", ""),
    row(JAVA, "enum_declaration", No, "", ""),
    row(JAVA, "finally_clause", No, "", ""),
    row(JAVA, "for_statement", Block, "初始化里的变量", ""),
    row(
        JAVA,
        "if_statement",
        No,
        "`instanceof` 的模式变量不确定（见「定义形式」）",
        "",
    ),
    row(JAVA, "interface_body", No, "成员", ""),
    row(JAVA, "interface_declaration", No, "", ""),
    row(JAVA, "lambda_expression", Function, "参数", ""),
    row(JAVA, "method_declaration", Function, "参数", ""),
    row(JAVA, "module_body", No, "", ""),
    row(JAVA, "module_declaration", No, "", ""),
    row(JAVA, "record_declaration", No, "分量是字段", ""),
    row(JAVA, "record_pattern_body", No, "模式变量不确定", ""),
    row(JAVA, "static_initializer", No, "它的块开块作用域", ""),
    row(
        JAVA,
        "switch_block",
        Block,
        "各组里的局部变量：从声明到 switch 块结束，后面的组也看得到（JLS 6.3）",
        "",
    ),
    row(
        JAVA,
        "switch_block_statement_group",
        No,
        "属于 `switch_block`",
        "",
    ),
    row(
        JAVA,
        "switch_expression",
        No,
        "`switch_block` 开块作用域",
        "",
    ),
    row(JAVA, "switch_rule", Block, "`case … ->` 的模式变量和体", ""),
    row(JAVA, "synchronized_statement", No, "", ""),
    row(JAVA, "ternary_expression", No, "", ""),
    row(JAVA, "try_statement", No, "", ""),
    row(
        JAVA,
        "try_with_resources_statement",
        Block,
        "资源变量，从声明结束起",
        "",
    ),
    row(JAVA, "while_statement", No, "", ""),
    // ---- Python ----
    row(PYTHON, "block", No, "Python 的块不开作用域", ""),
    row(PYTHON, "case_clause", No, "`case` 的捕获绑定所在的函数", ""),
    row(
        PYTHON,
        "class_definition",
        Class,
        "类体里的名字只对类体直接可见，方法里看不到",
        "基类",
    ),
    row(
        PYTHON,
        "dictionary_comprehension",
        Function,
        "`for` 的变量；`:=` 越过它绑定所在的函数",
        "第一个 `for` 的可迭代对象",
    ),
    row(PYTHON, "elif_clause", No, "", ""),
    row(PYTHON, "else_clause", No, "", ""),
    row(
        PYTHON,
        "except_clause",
        No,
        "`as` 的名字是函数的局部变量（离开子句时被删除，不建模）",
        "",
    ),
    row(PYTHON, "finally_clause", No, "", ""),
    row(PYTHON, "for_in_clause", No, "属于推导式", ""),
    row(PYTHON, "for_statement", No, "循环变量是函数的局部变量", ""),
    row(
        PYTHON,
        "function_definition",
        Function,
        "参数",
        "默认值、参数注解、返回注解",
    ),
    row(
        PYTHON,
        "generator_expression",
        Function,
        "同 `dictionary_comprehension`",
        "第一个 `for` 的可迭代对象",
    ),
    row(PYTHON, "if_clause", No, "属于推导式", ""),
    row(PYTHON, "if_statement", No, "", ""),
    row(PYTHON, "lambda", Function, "参数", "默认值"),
    row(
        PYTHON,
        "list_comprehension",
        Function,
        "同 `dictionary_comprehension`",
        "第一个 `for` 的可迭代对象",
    ),
    row(PYTHON, "match_statement", No, "", ""),
    row(
        PYTHON,
        "set_comprehension",
        Function,
        "同 `dictionary_comprehension`",
        "第一个 `for` 的可迭代对象",
    ),
    row(PYTHON, "try_statement", No, "", ""),
    row(
        PYTHON,
        "type_alias_statement",
        No,
        "类型参数只在语句里（`within` 规则）",
        "",
    ),
    row(PYTHON, "while_statement", No, "", ""),
    row(PYTHON, "with_clause", No, "`as` 的名字是函数的局部变量", ""),
    row(PYTHON, "with_statement", No, "", ""),
    // ---- Go ----
    row(GO, "assignment_statement", No, "给已有的变量赋值", ""),
    row(
        GO,
        "block",
        Block,
        "`:=`、`var`、`const`（从声明结束起）",
        "",
    ),
    row(
        GO,
        "communication_case",
        Block,
        "`case v := <-ch` 的变量和分支里的声明",
        "",
    ),
    row(GO, "composite_literal", No, "", ""),
    row(GO, "default_case", Block, "分支里的声明", ""),
    row(GO, "expression_case", Block, "分支里的声明", ""),
    row(
        GO,
        "expression_switch_statement",
        Block,
        "初始化语句的变量",
        "",
    ),
    row(GO, "for_clause", No, "属于 `for_statement`", ""),
    row(
        GO,
        "for_statement",
        Block,
        "初始化语句和 `range` 的变量（`range` 的从循环体开始）",
        "",
    ),
    row(GO, "func_literal", Function, "参数和结果", ""),
    row(GO, "function_declaration", Function, "参数和结果", ""),
    row(GO, "function_type", Function, "参数只在类型里", ""),
    row(
        GO,
        "if_statement",
        Block,
        "初始化语句的变量（`else` 里也可见）",
        "",
    ),
    row(GO, "method_declaration", Function, "接收者、参数和结果", ""),
    row(GO, "method_elem", Function, "参数只在签名里", ""),
    row(GO, "package_clause", No, "", ""),
    row(GO, "range_clause", No, "属于 `for_statement`", ""),
    row(
        GO,
        "receive_statement",
        No,
        "`v := <-ch` 的变量属于外层",
        "",
    ),
    row(GO, "send_statement", No, "", ""),
    row(GO, "type_case", Block, "分支里的声明", ""),
    row(
        GO,
        "type_switch_statement",
        Block,
        "`switch v := x.(type)` 的别名（每个分支里）",
        "",
    ),
];

fn scopes(lang: &str) -> impl Iterator<Item = &'static ScopeForm> + '_ {
    SCOPES.iter().filter(move |s| s.langs.contains(&lang))
}

/// Suffixes of node kinds that may open a scope.
const SUFFIXES: &[&str] = &[
    "_clause",
    "_block",
    "_case",
    "_body",
    "_arm",
    "_comprehension",
    "_initializer",
    "_group",
    "_rule",
];

/// Fields of node kinds that may open a scope: a body, parameters, or the
/// parts of a conditional or loop head.
const SCOPE_FIELDS: &[&str] = &[
    "body",
    "parameters",
    "parameter",
    "consequence",
    "alternative",
    "condition",
    "initializer",
];

/// (named node kinds, the ones that look like they may open a scope) of a
/// grammar.
fn scope_like_kinds(lang: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let types: serde_json::Value = serde_json::from_str(node_types(lang)).expect("node types");
    let mut named = BTreeSet::new();
    let mut like = BTreeSet::new();
    for t in types.as_array().expect("an array") {
        if t["named"] != serde_json::Value::Bool(true) {
            continue;
        }
        let kind = t["type"].as_str().expect("a type").to_owned();
        named.insert(kind.clone());
        if t.get("subtypes").is_some() {
            // A supertype (`_statement`, `expression`) is no node.
            continue;
        }
        let fields: Vec<&str> = t["fields"]
            .as_object()
            .map(|f| f.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let statement_head =
            kind.ends_with("_statement") && fields.iter().any(|f| *f == "left" || *f == "value");
        if kind == "block"
            || SUFFIXES.iter().any(|s| kind.ends_with(s))
            || fields.iter().any(|f| SCOPE_FIELDS.contains(f))
            || statement_head
        {
            like.insert(kind);
        }
    }
    (named, like)
}

#[test]
fn scopes_cover_grammar() {
    // Every node kind that looks like it may open a scope has a row, even
    // if only to say it opens none, so a grammar upgrade that adds one fails
    // here until it is classified; every row names a kind its grammar has.
    let mut missing = Vec::new();
    for lang in LANGS {
        let (named, like) = scope_like_kinds(lang);
        let listed: BTreeSet<&str> = scopes(lang).map(|s| s.kind).collect();
        for kind in &like {
            if !listed.contains(kind.as_str()) {
                missing.push(format!("{lang}: {kind} is not in the scope table"));
            }
        }
        for kind in &listed {
            if !named.contains(*kind) {
                missing.push(format!("{lang}: the grammar has no {kind}"));
            }
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}

#[test]
fn scopes_have_no_duplicate_rows() {
    let mut seen = BTreeSet::new();
    for form in SCOPES {
        for lang in form.langs {
            assert!(seen.insert((*lang, form.kind)), "{lang} {}", form.kind);
        }
    }
}

/// (languages, node kind, binding row): each sample needs the scope of its
/// kind to resolve as the row says.
type Sample = (&'static [&'static str], &'static str, Row);

const SAMPLES: &[Sample] = &[
    // ---- JavaScript, TypeScript, TSX ----
    (
        ECMA,
        "arrow_function",
        Row {
            what: "an arrow function's parameter",
            source: "const f = (param) => param;\nuse(param);\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        ECMA,
        "catch_clause",
        Row {
            what: "the catch parameter stays in the clause",
            source: "function f() {\n  try { g(); } catch (failure) { use(failure); }\n  return failure;\n}\n",
            reference: ("failure", 2),
            expect: Free,
        },
    ),
    (
        ECMA,
        "catch_clause",
        Row {
            what: "the catch parameter in the clause",
            source: "function f() {\n  try { g(); } catch (failure) { use(failure); }\n  return failure;\n}\n",
            reference: ("failure", 1),
            expect: Binds(("failure", 0, 0)),
        },
    ),
    (
        ECMA,
        "for_in_statement",
        Row {
            what: "a for-of variable",
            source: "for (const item of xs) { use(item); }\nuse(item);\n",
            reference: ("item", 2),
            expect: Free,
        },
    ),
    (
        ECMA,
        "for_statement",
        Row {
            what: "a for variable",
            source: "for (let idx = 0; idx < 3; idx++) {}\nuse(idx);\n",
            reference: ("idx", 3),
            expect: Free,
        },
    ),
    (
        ECMA,
        "function_declaration",
        Row {
            what: "a function's parameter",
            source: "function f(param) { return param; }\nuse(param);\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        ECMA,
        "function_expression",
        Row {
            what: "a function expression's parameter",
            source: "const f = function (param) { return param; };\nuse(param);\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        ECMA,
        "generator_function",
        Row {
            what: "a generator expression's parameter",
            source: "const g = function* (param) { yield param; };\nuse(param);\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        ECMA,
        "generator_function_declaration",
        Row {
            what: "a generator's parameter",
            source: "function* g(param) { yield param; }\nuse(param);\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        ECMA,
        "method_definition",
        Row {
            what: "a method's parameter",
            source: "class A { m(param) { return param; } }\nuse(param);\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        ECMA,
        "statement_block",
        Row {
            what: "a block's const",
            source: "{ const inner = 1; }\nuse(inner);\n",
            reference: ("inner", 1),
            expect: Free,
        },
    ),
    (
        ECMA,
        "switch_body",
        Row {
            what: "a case's let is in scope in the later cases",
            source: "function f(k) {\n  switch (k) { case 1: let shared = 1; break; case 2: use(shared); }\n  return shared;\n}\n",
            reference: ("shared", 1),
            expect: Binds(("shared", 0, 0)),
        },
    ),
    (
        ECMA,
        "switch_body",
        Row {
            what: "a case's let ends with the switch body",
            source: "function f(k) {\n  switch (k) { case 1: let shared = 1; break; case 2: use(shared); }\n  return shared;\n}\n",
            reference: ("shared", 2),
            expect: Free,
        },
    ),
    (
        ECMA,
        "with_statement",
        Row {
            what: "a name in a with body may be the object's property",
            source: "function f(obj) {\n  const size = 1;\n  with (obj) { use(size); }\n}\n",
            reference: ("size", 1),
            expect: Candidates(&[("size", 0, 0)]),
        },
    ),
    (
        ECMA,
        "with_statement",
        Row {
            what: "a declaration in the with body binds",
            source: "function f(obj) {\n  with (obj) { const inner = 1; use(inner); }\n}\n",
            reference: ("inner", 1),
            expect: Binds(("inner", 0, 0)),
        },
    ),
    (
        TS,
        "abstract_method_signature",
        Row {
            what: "an abstract signature's parameter",
            source: "abstract class A { abstract m(param: number): void; }\nuse(param);\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        TS,
        "call_signature",
        Row {
            what: "a call signature's parameter",
            source: "interface F { (param: number): void }\nuse(param);\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        TS,
        "construct_signature",
        Row {
            what: "a construct signature's parameter",
            source: "interface C { new (param: number): C }\nuse(param);\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        TS,
        "constructor_type",
        Row {
            what: "a constructor type's parameter",
            source: "type K = new (param: number) => object;\nuse(param);\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        TS,
        "function_signature",
        Row {
            what: "a declared function's parameter",
            source: "declare function f(param: number): void;\nuse(param);\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        TS,
        "function_type",
        Row {
            what: "a function type's parameter",
            source: "function f(cb: (param: number) => void) { return param; }\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        TS,
        "method_signature",
        Row {
            what: "a method signature's parameter",
            source: "interface I { m(param: number): void }\nuse(param);\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        TS,
        "internal_module",
        Row {
            what: "a namespace body binds lexically",
            source: "namespace NS { const inner = 1; use(inner); }\nuse(inner);\n",
            reference: ("inner", 1),
            expect: Binds(("inner", 0, 0)),
        },
    ),
    (
        TS,
        "internal_module",
        Row {
            what: "a namespace's declaration stays in it",
            source: "namespace NS { const inner = 1; use(inner); }\nuse(inner);\n",
            reference: ("inner", 2),
            expect: Free,
        },
    ),
    (
        TS,
        "ambient_declaration",
        Row {
            what: "a declare global block",
            source: "declare global { const inner: number; }\nuse(inner);\n",
            reference: ("inner", 1),
            expect: Free,
        },
    ),
    (
        TS,
        "module",
        Row {
            what: "a module declaration's body",
            source: "declare module 'm' { const inner: number; }\nuse(inner);\n",
            reference: ("inner", 1),
            expect: Free,
        },
    ),
    // ---- Rust ----
    (
        RUST,
        "block",
        Row {
            what: "a block's let",
            source: "fn f() -> u8 { { let inner = 1; } inner }\n",
            reference: ("inner", 1),
            expect: Free,
        },
    ),
    (
        RUST,
        "closure_expression",
        Row {
            what: "a closure's parameter",
            source: "fn f() { let g = |param: u8| param; param; }\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        RUST,
        "for_expression",
        Row {
            what: "a for pattern",
            source: "fn f(xs: V) { for item in xs { item; } item; }\n",
            reference: ("item", 2),
            expect: Free,
        },
    ),
    (
        RUST,
        "function_item",
        Row {
            what: "a function's parameter",
            source: "fn f(param: u8) -> u8 { param }\nconst K: u8 = param;\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        RUST,
        "function_signature_item",
        Row {
            what: "a signature's parameter",
            source: "trait T { fn m(&self, param: u8); }\nfn f() -> u8 { param }\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        RUST,
        "function_type",
        Row {
            what: "a function type's parameter",
            source: "fn f() { let g: fn(param: u8) = h; param; }\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        RUST,
        "match_arm",
        Row {
            what: "an arm's pattern",
            source: "fn f(o: O) -> u8 { match o { Some(val) => val, _ => val } }\n",
            reference: ("val", 2),
            expect: Free,
        },
    ),
    // ---- Java ----
    (
        JAVA,
        "block",
        Row {
            what: "a block's local",
            source: "class A { void m() { { int inner = 1; } use(inner); } }\n",
            reference: ("inner", 1),
            expect: Free,
        },
    ),
    (
        JAVA,
        "catch_clause",
        Row {
            what: "a catch parameter",
            source: "class A { void m() { try { } catch (Exception failure) { } use(failure); } }\n",
            reference: ("failure", 1),
            expect: Free,
        },
    ),
    (
        JAVA,
        "constructor_declaration",
        Row {
            what: "a constructor's parameter",
            source: "class A { A(int param) { } void m() { use(param); } }\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        JAVA,
        "enhanced_for_statement",
        Row {
            what: "a for-each variable",
            source: "class A { void m(java.util.List<String> xs) { for (String item : xs) { } use(item); } }\n",
            reference: ("item", 1),
            expect: Free,
        },
    ),
    (
        JAVA,
        "for_statement",
        Row {
            what: "a for variable",
            source: "class A { void m() { for (int idx = 0; idx < 3; idx++) { } use(idx); } }\n",
            reference: ("idx", 3),
            expect: Free,
        },
    ),
    (
        JAVA,
        "lambda_expression",
        Row {
            what: "a lambda's parameter",
            source: "class A { void m() { Fn f = (int param) -> param; use(param); } }\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        JAVA,
        "method_declaration",
        Row {
            what: "a method's parameter",
            source: "class A { void m(int param) { } void n() { use(param); } }\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        JAVA,
        "switch_block",
        Row {
            what: "a group's local is in scope in the later groups",
            source: "class A { void m(int k) { switch (k) { case 1: int shared = 1; break; default: shared = 2; } } }\n",
            reference: ("shared", 1),
            expect: Binds(("shared", 0, 0)),
        },
    ),
    (
        JAVA,
        "switch_rule",
        Row {
            what: "a rule's pattern variable stays in the rule",
            source: "class A { void m(Object o) { switch (o) { case String text -> use(text); default -> use(text); } } }\n",
            reference: ("text", 2),
            expect: Free,
        },
    ),
    (
        JAVA,
        "try_with_resources_statement",
        Row {
            what: "a resource variable",
            source: "class A { void m() { try (Res handle = open()) { use(handle); } use(handle); } }\n",
            reference: ("handle", 2),
            expect: Free,
        },
    ),
    // ---- Python ----
    (
        PYTHON,
        "class_definition",
        Row {
            what: "a class body's name is not visible in its methods",
            source: "class A:\n    size = 1\n    def m(self):\n        return size\n",
            reference: ("size", 1),
            expect: Free,
        },
    ),
    (
        PYTHON,
        "dictionary_comprehension",
        Row {
            what: "a dict comprehension's variable",
            source: "def f(xs):\n    d = {key: 1 for key in xs}\n    return key\n",
            reference: ("key", 2),
            expect: Free,
        },
    ),
    (
        PYTHON,
        "function_definition",
        Row {
            what: "a function's parameter",
            source: "def f(param):\n    return param\nuse(param)\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        PYTHON,
        "generator_expression",
        Row {
            what: "a generator expression's variable",
            source: "def f(xs):\n    g = (elem for elem in xs)\n    return elem\n",
            reference: ("elem", 2),
            expect: Free,
        },
    ),
    (
        PYTHON,
        "lambda",
        Row {
            what: "a lambda's parameter",
            source: "f = lambda param: param\nuse(param)\n",
            reference: ("param", 2),
            expect: Free,
        },
    ),
    (
        PYTHON,
        "list_comprehension",
        Row {
            what: "a list comprehension's variable",
            source: "def f(xs):\n    return [elem for elem in xs] + [elem]\n",
            reference: ("elem", 2),
            expect: Free,
        },
    ),
    (
        PYTHON,
        "set_comprehension",
        Row {
            what: "a set comprehension's variable",
            source: "def f(xs):\n    s = {elem for elem in xs}\n    return elem\n",
            reference: ("elem", 2),
            expect: Free,
        },
    ),
    // ---- Go ----
    (
        GO,
        "block",
        Row {
            what: "a block's short declaration",
            source: "package p\n\nfunc f() {\n\t{\n\t\tinner := 1\n\t\t_ = inner\n\t}\n\t_ = inner\n}\n",
            reference: ("inner", 2),
            expect: Free,
        },
    ),
    (
        GO,
        "communication_case",
        Row {
            what: "a receive case's variable",
            source: "package p\n\nfunc f(ch chan int) {\n\tselect {\n\tcase msg := <-ch:\n\t\t_ = msg\n\t}\n\t_ = msg\n}\n",
            reference: ("msg", 2),
            expect: Free,
        },
    ),
    (
        GO,
        "default_case",
        Row {
            what: "a default case's declaration stays in it",
            source: "package p\n\nfunc f(n int) {\n\tswitch n {\n\tdefault:\n\t\tinner := 1\n\t\t_ = inner\n\tcase 1:\n\t\t_ = inner\n\t}\n}\n",
            reference: ("inner", 2),
            expect: Free,
        },
    ),
    (
        GO,
        "expression_case",
        Row {
            what: "a case's declaration stays in it",
            source: "package p\n\nfunc f(n int) {\n\tswitch n {\n\tcase 1:\n\t\tinner := 1\n\t\t_ = inner\n\tcase 2:\n\t\t_ = inner\n\t}\n}\n",
            reference: ("inner", 2),
            expect: Free,
        },
    ),
    (
        GO,
        "expression_switch_statement",
        Row {
            what: "a switch initializer's variable",
            source: "package p\n\nfunc f() {\n\tswitch sel := g(); sel {\n\t}\n\t_ = sel\n}\n",
            reference: ("sel", 2),
            expect: Free,
        },
    ),
    (
        GO,
        "for_statement",
        Row {
            what: "a for initializer's variable",
            source: "package p\n\nfunc f() {\n\tfor idx := 0; idx < 3; idx++ {\n\t}\n\t_ = idx\n}\n",
            reference: ("idx", 3),
            expect: Free,
        },
    ),
    (
        GO,
        "func_literal",
        Row {
            what: "a function literal's parameter",
            source: "package p\n\nfunc f() {\n\tg := func(param int) {}\n\t_ = g\n\t_ = param\n}\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        GO,
        "function_declaration",
        Row {
            what: "a function's parameter",
            source: "package p\n\nfunc f(param int) {}\n\nvar v = param\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        GO,
        "function_type",
        Row {
            what: "a function type's parameter",
            source: "package p\n\nfunc f(cb func(param int)) {\n\t_ = param\n}\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        GO,
        "if_statement",
        Row {
            what: "an if initializer's variable",
            source: "package p\n\nfunc f() {\n\tif val := g(); val > 0 {\n\t}\n\t_ = val\n}\n",
            reference: ("val", 2),
            expect: Free,
        },
    ),
    (
        GO,
        "method_declaration",
        Row {
            what: "a method's parameter",
            source: "package p\n\nfunc (t T) M(param int) {}\n\nvar v = param\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        GO,
        "method_elem",
        Row {
            what: "an interface method's parameter",
            source: "package p\n\ntype I interface{ M(param int) }\n\nfunc f() {\n\t_ = param\n}\n",
            reference: ("param", 1),
            expect: Free,
        },
    ),
    (
        GO,
        "type_case",
        Row {
            what: "a type case's declaration stays in it",
            source: "package p\n\nfunc f(x any) {\n\tswitch x.(type) {\n\tcase int:\n\t\tinner := 1\n\t\t_ = inner\n\tcase string:\n\t\t_ = inner\n\t}\n}\n",
            reference: ("inner", 2),
            expect: Free,
        },
    ),
    (
        GO,
        "type_switch_statement",
        Row {
            what: "a type switch's alias",
            source: "package p\n\nfunc f(x any) {\n\tswitch val := x.(type) {\n\tcase int:\n\t\t_ = val\n\t}\n\t_ = val\n}\n",
            reference: ("val", 2),
            expect: Free,
        },
    ),
];

#[test]
fn scopes_samples_bind() {
    for (langs, _, row) in SAMPLES {
        for lang in *langs {
            check(lang, std::slice::from_ref(row));
        }
    }
}

/// The scopes the locals query opens on `source`: (node kind, its parent's
/// kind, what the query makes of it).
fn query_scopes(lang: &str, source: &str) -> Vec<(String, String, Opens)> {
    let lang = super::site::core_table().get(lang).expect("core language");
    let compiled = lang.compiled().expect("compiled");
    let mut parser = Parser::new();
    parser.set_language(lang.language()).expect("language");
    let tree = parser.parse(source, None).expect("parsed");
    let query = &compiled.tags.query;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        if m.pattern_index >= compiled.locals_patterns {
            continue;
        }
        let kind = query
            .property_settings(m.pattern_index)
            .iter()
            .find(|p| &*p.key == "local.kind")
            .and_then(|p| p.value.as_deref());
        let opens = match kind {
            Some("function" | "comprehension") => Function,
            Some("class") => Class,
            Some("namespace") => Namespace,
            Some("dynamic") => Dynamic,
            _ => Block,
        };
        for capture in m.captures() {
            if compiled.captures[capture.index as usize] == Capture::LocalScope {
                let node = capture.node;
                let parent = node
                    .parent()
                    .map(|p| p.kind().to_owned())
                    .unwrap_or_default();
                out.push((node.kind().to_owned(), parent, opens));
            }
        }
    }
    out
}

#[test]
fn scopes_match_locals_query() {
    // Every scope the locals query opens on the samples is a row of that
    // kind (on the node, or on its parent for a body the parent's row
    // describes: a namespace or `with` body); every opening row is opened
    // on its samples.
    let mut failures = Vec::new();
    for lang in LANGS {
        let mut seen: BTreeSet<(&str, Opens)> = BTreeSet::new();
        for (langs, _, row) in SAMPLES {
            if !langs.contains(lang) {
                continue;
            }
            for (kind, parent, opens) in query_scopes(lang, row.source) {
                let found = scopes(lang)
                    .find(|s| s.opens == opens && s.kind == kind)
                    .or_else(|| scopes(lang).find(|s| s.opens == opens && s.kind == parent));
                match found {
                    Some(s) => {
                        seen.insert((s.kind, s.opens));
                    }
                    None => failures.push(format!(
                        "{lang}: the query opens a {opens:?} scope on {kind} (in {parent}), which no row says"
                    )),
                }
            }
        }
        for form in scopes(lang).filter(|s| s.opens != No) {
            if !seen.contains(&(form.kind, form.opens)) {
                failures.push(format!(
                    "{lang}: no sample opens the {:?} scope of {}",
                    form.opens, form.kind
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The table as the design document shows it.
fn render() -> String {
    let mut out =
        String::from("| 语言 | 节点 | 作用域 | 绑定 | 在外层求值 |\n|---|---|---|---|---|\n");
    for form in SCOPES {
        let opens = match form.opens {
            Function => "函数",
            Block => "块",
            Class => "类体",
            Namespace => "命名空间",
            Dynamic => "动态",
            No => "不开",
        };
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} |\n",
            form.langs.join(" "),
            form.kind,
            opens,
            form.binds,
            form.outside
        ));
    }
    out
}

#[test]
fn scopes_table_matches_design() {
    // docs/design/code_intel.md「作用域形式」 shows this table, between the
    // two markers; on a change, paste the expected block printed below.
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/design/code_intel.md"
    ))
    .expect("the design document");
    let begin = "<!-- scopes:begin -->\n";
    let end = "<!-- scopes:end -->";
    let shown = doc
        .split_once(begin)
        .and_then(|(_, rest)| rest.split_once(end))
        .map(|(table, _)| table)
        .unwrap_or_default();
    let expected = render();
    assert!(
        shown == expected,
        "the design document's table differs; expected:\n{begin}{expected}{end}"
    );
}
