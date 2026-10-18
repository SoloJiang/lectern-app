//! `forms_*`: the definition-form table (crate::forms) against the
//! grammars, the extractors, the binding model and the design document.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::binding::{Binds, Candidates, Expect, Row, check};
use super::extract::test_watchdog;
use crate::DefKind;
use crate::extract::{Deadline, LanguageTable, Visibility, extract, extract_tags};
use crate::forms::{FORMS, NAMING_FIELDS, Role, forms};
use crate::locals::Namespace;

const LANGS: &[&str] = &["ts", "tsx", "js", "rust", "java", "python", "go"];

pub(super) fn node_types(lang: &str) -> &'static str {
    match lang {
        "ts" => tree_sitter_typescript::TYPESCRIPT_NODE_TYPES,
        "tsx" => tree_sitter_typescript::TSX_NODE_TYPES,
        "js" => tree_sitter_javascript::NODE_TYPES,
        "rust" => tree_sitter_rust::NODE_TYPES,
        "java" => tree_sitter_java::NODE_TYPES,
        "python" => tree_sitter_python::NODE_TYPES,
        "go" => tree_sitter_go::NODE_TYPES,
        _ => unreachable!("{lang}"),
    }
}

/// (named node kinds, the ones with a naming field) of a grammar.
fn grammar_kinds(lang: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let types: serde_json::Value = serde_json::from_str(node_types(lang)).expect("node types");
    let mut named = BTreeSet::new();
    let mut naming = BTreeSet::new();
    for t in types.as_array().expect("an array") {
        if t["named"] != serde_json::Value::Bool(true) {
            continue;
        }
        let kind = t["type"].as_str().expect("a type").to_owned();
        if let Some(fields) = t["fields"].as_object()
            && NAMING_FIELDS.iter().any(|f| fields.contains_key(*f))
        {
            naming.insert(kind.clone());
        }
        named.insert(kind);
    }
    (named, naming)
}

#[test]
fn forms_cover_grammar() {
    // Every node kind with a naming field has a row, so a grammar upgrade
    // that adds a declaration form fails here until the form is classified;
    // every row names a kind its grammar has, so a renamed kind fails too.
    let mut missing = Vec::new();
    for lang in LANGS {
        let (named, naming) = grammar_kinds(lang);
        let listed: BTreeSet<&str> = forms(lang).map(|f| f.kind).collect();
        for kind in &naming {
            if !listed.contains(kind.as_str()) {
                missing.push(format!("{lang}: {kind} has a naming field and no row"));
            }
        }
        for kind in &listed {
            if !named.contains(*kind) {
                missing.push(format!("{lang}: row {kind} is not a node kind"));
            }
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}

#[test]
fn forms_have_no_duplicate_rows() {
    let mut seen = BTreeSet::new();
    for form in FORMS {
        for lang in form.langs {
            assert!(
                seen.insert((*lang, form.kind)),
                "{lang} {} twice",
                form.kind
            );
        }
    }
}

/// What a sample shows.
enum Shows {
    /// Extraction gives a definition of this name and kind.
    Defines(&'static str, DefKind),
    /// A binding-table row.
    Binding(Expect, (&'static str, usize)),
}

use Shows::{Binding, Defines};

/// (languages, node kind, source, what it shows).
type Sample = (&'static [&'static str], &'static str, &'static str, Shows);

const JS: &[&str] = &["js"];
const ECMA: &[&str] = &["js", "ts", "tsx"];
const TS: &[&str] = &["ts", "tsx"];

const SAMPLES: &[Sample] = &[
    // ---- JavaScript, TypeScript, TSX ----
    (
        ECMA,
        "arrow_function",
        "const f = x => x + 1;\n",
        Binding(Binds(("x", 0, 0)), ("x", 1)),
    ),
    (
        ECMA,
        "assignment_expression",
        "obj.handler = function (e) { return e; };\n",
        Defines("handler", DefKind::Function),
    ),
    (
        ECMA,
        "catch_clause",
        "try { run(); } catch (err) { report(err); }\n",
        Binding(Binds(("err", 0, 0)), ("err", 1)),
    ),
    (
        ECMA,
        "class",
        "const A = class Named {};\n",
        Defines("Named", DefKind::Class),
    ),
    (
        ECMA,
        "class_declaration",
        "export class Store {}\n",
        Defines("Store", DefKind::Class),
    ),
    (
        ECMA,
        "class_declaration",
        "function f() { class Local {} return new Local(); }\n",
        Binding(Binds(("Local", 0, 0)), ("Local", 1)),
    ),
    (
        TS,
        "class_declaration",
        "function f() { class Local {} const x: Local = new Local(); return x; }\n",
        Binding(Binds(("Local", 0, 0)), ("Local", 1)),
    ),
    (
        JS,
        "field_definition",
        "class A { handle = () => 1; }\n",
        Defines("handle", DefKind::Field),
    ),
    (
        ECMA,
        "for_in_statement",
        "for (const entry of list) { use(entry); }\n",
        Binding(Binds(("entry", 0, 0)), ("entry", 1)),
    ),
    (
        ECMA,
        "formal_parameters",
        "function f(a, { b }, c = 1) { return a + b + c; }\n",
        Binding(Binds(("b", 0, 0)), ("b", 1)),
    ),
    (
        ECMA,
        "function_declaration",
        "export function run() {}\n",
        Defines("run", DefKind::Function),
    ),
    (
        ECMA,
        "function_declaration",
        "function outer() { return inner(); function inner() {} }\n",
        Binding(Binds(("inner", 1, 0)), ("inner", 0)),
    ),
    (
        ECMA,
        "function_expression",
        "const g = function named() {};\n",
        Defines("named", DefKind::Function),
    ),
    (
        ECMA,
        "generator_function",
        "const gen = function* produce() {};\n",
        Defines("produce", DefKind::Function),
    ),
    (
        ECMA,
        "generator_function_declaration",
        "export function* items() {}\n",
        Defines("items", DefKind::Function),
    ),
    (
        ECMA,
        "generator_function_declaration",
        "function f() { return list(); function* list() {} }\n",
        Binding(Binds(("list", 1, 0)), ("list", 0)),
    ),
    (
        ECMA,
        "method_definition",
        "class A { run() {} }\n",
        Defines("run", DefKind::Method),
    ),
    (
        ECMA,
        "pair",
        "export const o = { cb: (y) => y };\n",
        Defines("cb", DefKind::Function),
    ),
    (
        ECMA,
        "variable_declarator",
        "export const make = () => 1;\n",
        Defines("make", DefKind::Function),
    ),
    (
        ECMA,
        "variable_declarator",
        "export const Klass = class {};\n",
        Defines("Klass", DefKind::Class),
    ),
    (
        ECMA,
        "variable_declarator",
        "export const LIMIT = 3;\n",
        Defines("LIMIT", DefKind::Constant),
    ),
    (
        ECMA,
        "variable_declarator",
        "export let count = 0;\n",
        Defines("count", DefKind::Variable),
    ),
    (
        ECMA,
        "variable_declarator",
        "export const { a, b: renamed, ...rest } = obj;\n",
        Defines("renamed", DefKind::Constant),
    ),
    (
        ECMA,
        "variable_declarator",
        "const { deep: { inner = 1 }, list: [first] } = obj;\n",
        Defines("inner", DefKind::Constant),
    ),
    (
        ECMA,
        "variable_declarator",
        "let [x, , y = 1, ...tail] = arr;\n",
        Defines("tail", DefKind::Variable),
    ),
    (
        ECMA,
        "variable_declarator",
        "var [[p], { q }] = arr;\n",
        Defines("q", DefKind::Variable),
    ),
    (
        ECMA,
        "variable_declarator",
        "function f() { const value = 1; return value; }\n",
        Binding(Binds(("value", 0, 0)), ("value", 1)),
    ),
    (
        TS,
        "variable_declarator",
        "declare const VERSION: string;\nexport declare let mode: number;\n",
        Defines("VERSION", DefKind::Constant),
    ),
    (
        TS,
        "variable_declarator",
        "export declare let mode: number;\n",
        Defines("mode", DefKind::Variable),
    ),
    (
        TS,
        "abstract_class_declaration",
        "export abstract class Shape {}\n",
        Defines("Shape", DefKind::Class),
    ),
    (
        TS,
        "abstract_class_declaration",
        "function f() { abstract class Base {} let b: Base; return b; }\n",
        Binding(Binds(("Base", 0, 0)), ("Base", 1)),
    ),
    (
        TS,
        "abstract_method_signature",
        "abstract class A { abstract area(): number; }\n",
        Defines("area", DefKind::Method),
    ),
    (
        TS,
        "enum_assignment",
        "enum E { A = 1 }\n",
        Defines("A", DefKind::Constant),
    ),
    (
        TS,
        "enum_body",
        "enum E { B }\n",
        Defines("B", DefKind::Constant),
    ),
    (
        TS,
        "enum_declaration",
        "export enum Color { Red }\n",
        Defines("Color", DefKind::Enum),
    ),
    (
        TS,
        "enum_declaration",
        "function f() { enum Mode { On } return Mode.On; }\n",
        Binding(Binds(("Mode", 0, 0)), ("Mode", 1)),
    ),
    (
        TS,
        "function_signature",
        "declare function load(path: string): void;\n",
        Defines("load", DefKind::Function),
    ),
    (
        TS,
        "interface_declaration",
        "export interface Shape {}\n",
        Defines("Shape", DefKind::Interface),
    ),
    (
        TS,
        "interface_declaration",
        "function f() { interface Opts { a: number } const o: Opts = { a: 1 }; return o; }\n",
        Binding(Binds(("Opts", 0, 0)), ("Opts", 1)),
    ),
    (
        TS,
        "internal_module",
        "namespace NS.Inner { }\nnamespace Tools { }\n",
        Defines("Inner", DefKind::Module),
    ),
    (
        TS,
        "internal_module",
        "namespace Tools { }\n",
        Defines("Tools", DefKind::Module),
    ),
    (
        TS,
        "mapped_type_clause",
        "type M<T> = { [Key in keyof T]: Key };\n",
        Binding(Binds(("Key", 0, 0)), ("Key", 1)),
    ),
    (
        TS,
        "method_signature",
        "interface I { run(): void; }\n",
        Defines("run", DefKind::Method),
    ),
    (
        TS,
        "module",
        "module Legacy { }\n",
        Defines("Legacy", DefKind::Module),
    ),
    (
        TS,
        "optional_parameter",
        "class A { constructor(private label?: string) {} }\n",
        Defines("label", DefKind::Field),
    ),
    (
        TS,
        "optional_parameter",
        "function f(amount?: number) { return amount; }\n",
        Binding(Binds(("amount", 0, 0)), ("amount", 1)),
    ),
    (
        TS,
        "property_signature",
        "interface I { size: number }\n",
        Defines("size", DefKind::Field),
    ),
    (
        TS,
        "public_field_definition",
        "class A { total = 0; }\n",
        Defines("total", DefKind::Field),
    ),
    (
        TS,
        "required_parameter",
        "class A { constructor(public id: string, readonly at: number) {} }\n",
        Defines("at", DefKind::Field),
    ),
    (
        TS,
        "required_parameter",
        "function f(amount: number) { return amount; }\n",
        Binding(Binds(("amount", 0, 0)), ("amount", 1)),
    ),
    (
        TS,
        "type_alias_declaration",
        "export type Id = string;\n",
        Defines("Id", DefKind::Type),
    ),
    (
        TS,
        "type_alias_declaration",
        "function f() { type Pair = [number, number]; const p: Pair = [1, 2]; return p; }\n",
        Binding(Binds(("Pair", 0, 0)), ("Pair", 1)),
    ),
    (
        TS,
        "type_parameter",
        "function first<Item>(xs: Item[]): Item { return xs[0]; }\n",
        Binding(Binds(("Item", 0, 0)), ("Item", 1)),
    ),
    // ---- Rust ----
    (
        &["rust"],
        "associated_type",
        "trait Tr { type Item; }\n",
        Defines("Item", DefKind::Type),
    ),
    (
        &["rust"],
        "closure_parameters",
        "fn f() { let g = |num: u8| num + 1; }\n",
        Binding(Binds(("num", 0, 0)), ("num", 1)),
    ),
    (
        &["rust"],
        "const_item",
        "const MAX: u8 = 1;\n",
        Defines("MAX", DefKind::Constant),
    ),
    (
        &["rust"],
        "const_item",
        "fn f() -> u8 { const K: u8 = 2; K }\n",
        Binding(Binds(("K", 0, 0)), ("K", 1)),
    ),
    (
        &["rust"],
        "const_parameter",
        "fn f<const N: usize>() -> usize { N }\n",
        Binding(Binds(("N", 0, 0)), ("N", 1)),
    ),
    (
        &["rust"],
        "enum_item",
        "pub enum Error { Full }\n",
        Defines("Error", DefKind::Enum),
    ),
    (
        &["rust"],
        "enum_item",
        "fn f() { enum Mode { A } let m: Mode = Mode::A; }\n",
        Binding(Binds(("Mode", 0, 0)), ("Mode", 1)),
    ),
    (
        &["rust"],
        "enum_variant",
        "enum Message { Move { x: u8 }, Write(u8) }\n",
        Defines("Move", DefKind::Constant),
    ),
    (
        &["rust"],
        "field_declaration",
        "struct S { size: u8 }\n",
        Defines("size", DefKind::Field),
    ),
    (
        &["rust"],
        "for_expression",
        "fn f(v: Vec<u8>) { for item in v { drop(item); } }\n",
        Binding(Binds(("item", 0, 0)), ("item", 1)),
    ),
    (
        &["rust"],
        "function_item",
        "fn run() {}\n",
        Defines("run", DefKind::Function),
    ),
    (
        &["rust"],
        "function_item",
        "struct S;\nimpl S { fn go(&self) {} }\n",
        Defines("go", DefKind::Method),
    ),
    (
        &["rust"],
        "function_item",
        "fn f() -> u8 { fn inner() -> u8 { 1 } inner() }\n",
        Binding(Binds(("inner", 0, 0)), ("inner", 1)),
    ),
    (
        &["rust"],
        "function_signature_item",
        "trait T { fn sig(&self); }\n",
        Defines("sig", DefKind::Method),
    ),
    (
        &["rust"],
        "function_signature_item",
        "extern \"C\" { fn ext(x: i32); }\n",
        Defines("ext", DefKind::Function),
    ),
    (
        &["rust"],
        "let_condition",
        "fn f(o: Option<u8>) { if let Some(val) = o { drop(val); } }\n",
        Binding(Binds(("val", 0, 0)), ("val", 1)),
    ),
    (
        &["rust"],
        "let_declaration",
        "fn f() { let total = 1; drop(total); }\n",
        Binding(Binds(("total", 0, 0)), ("total", 1)),
    ),
    (
        &["rust"],
        "macro_definition",
        "macro_rules! mac { () => {} }\n",
        Defines("mac", DefKind::Macro),
    ),
    (
        &["rust"],
        "match_arm",
        "fn f(o: Option<u8>) -> u8 { match o { Some(num) => num, None => 0 } }\n",
        Binding(Binds(("num", 0, 0)), ("num", 1)),
    ),
    (
        &["rust"],
        "mod_item",
        "mod net {}\n",
        Defines("net", DefKind::Module),
    ),
    (
        &["rust"],
        "parameter",
        "fn f(count: u8) -> u8 { count }\n",
        Binding(Binds(("count", 0, 0)), ("count", 1)),
    ),
    (
        &["rust"],
        "static_item",
        "static GLOBAL: u8 = 1;\n",
        Defines("GLOBAL", DefKind::Constant),
    ),
    (
        &["rust"],
        "static_item",
        "fn f() -> u8 { static LIMIT: u8 = 1; LIMIT }\n",
        Binding(Binds(("LIMIT", 0, 0)), ("LIMIT", 1)),
    ),
    (
        &["rust"],
        "struct_item",
        "pub struct Queue;\n",
        Defines("Queue", DefKind::Struct),
    ),
    (
        &["rust"],
        "struct_item",
        "fn f() { struct Local(u8); let v = Local(1); }\n",
        Binding(Binds(("Local", 0, 0)), ("Local", 1)),
    ),
    (
        &["rust"],
        "struct_item",
        "fn f() { struct P { x: u8 } let p: P = P { x: 1 }; }\n",
        Binding(Binds(("P", 0, 0)), ("P", 1)),
    ),
    (
        &["rust"],
        "trait_item",
        "pub trait Tr {}\n",
        Defines("Tr", DefKind::Trait),
    ),
    (
        &["rust"],
        "trait_item",
        "fn f() { trait Local {} fn g<T: Local>() {} }\n",
        Binding(Binds(("Local", 0, 0)), ("Local", 1)),
    ),
    (
        &["rust"],
        "type_item",
        "type Alias = u8;\n",
        Defines("Alias", DefKind::Type),
    ),
    (
        &["rust"],
        "type_item",
        "fn f() { type Id = u32; let x: Id = 1; }\n",
        Binding(Binds(("Id", 0, 0)), ("Id", 1)),
    ),
    (
        &["rust"],
        "type_parameter",
        "fn first<Item: Clone>(xs: &[Item]) -> Item { xs[0].clone() }\n",
        Binding(Binds(("Item", 0, 0)), ("Item", 1)),
    ),
    (
        &["rust"],
        "union_item",
        "union Bits { a: u8 }\n",
        Defines("Bits", DefKind::Struct),
    ),
    (
        &["rust"],
        "union_item",
        "fn f() { union U { a: u8 } let u: U = U { a: 1 }; }\n",
        Binding(Binds(("U", 0, 0)), ("U", 1)),
    ),
    // ---- Java ----
    (
        &["java"],
        "annotation_type_declaration",
        "@interface Marker {}\n",
        Defines("Marker", DefKind::Interface),
    ),
    (
        &["java"],
        "annotation_type_element_declaration",
        "@interface M { int level() default 1; }\n",
        Defines("level", DefKind::Method),
    ),
    (
        &["java"],
        "catch_formal_parameter",
        "class A { void f() { try { g(); } catch (Exception err) { h(err); } } }\n",
        Binding(Binds(("err", 0, 0)), ("err", 1)),
    ),
    (
        &["java"],
        "class_declaration",
        "class A {}\n",
        Defines("A", DefKind::Class),
    ),
    (
        &["java"],
        "class_declaration",
        "class A { void f() { class Local {} Local x = new Local(); } }\n",
        Binding(Binds(("Local", 0, 0)), ("Local", 1)),
    ),
    (
        &["java"],
        "constant_declaration",
        "interface I { int MAX = 1; }\n",
        Defines("MAX", DefKind::Constant),
    ),
    (
        &["java"],
        "enhanced_for_statement",
        "class A { void f(java.util.List<String> xs) { for (String item : xs) { g(item); } } }\n",
        Binding(Binds(("item", 0, 0)), ("item", 1)),
    ),
    (
        &["java"],
        "enum_constant",
        "enum Kind { TOOL }\n",
        Defines("TOOL", DefKind::Constant),
    ),
    (
        &["java"],
        "enum_declaration",
        "enum Kind { TOOL }\n",
        Defines("Kind", DefKind::Enum),
    ),
    (
        &["java"],
        "enum_declaration",
        "class A { void f() { enum Mode { ON } Mode m = Mode.ON; } }\n",
        Binding(Binds(("Mode", 0, 0)), ("Mode", 1)),
    ),
    (
        &["java"],
        "field_declaration",
        "class A { int count; }\n",
        Defines("count", DefKind::Field),
    ),
    (
        &["java"],
        "formal_parameter",
        "record Point(int value) {}\n",
        Defines("value", DefKind::Field),
    ),
    (
        &["java"],
        "formal_parameter",
        "class A { int f(int count) { return count; } }\n",
        Binding(Binds(("count", 0, 0)), ("count", 1)),
    ),
    (
        &["java"],
        "inferred_parameters",
        "class A { void f() { Op add = (left, right) -> left + right; } }\n",
        Binding(Binds(("left", 0, 0)), ("left", 1)),
    ),
    (
        &["java"],
        "instanceof_expression",
        "class A { void f(Object o) { if (o instanceof String text) { g(text); } } }\n",
        Binding(Candidates(&[("text", 0, 0)]), ("text", 1)),
    ),
    (
        &["java"],
        "interface_declaration",
        "interface Shape {}\n",
        Defines("Shape", DefKind::Interface),
    ),
    (
        &["java"],
        "interface_declaration",
        "class A { void f() { interface Op {} Op o = null; } }\n",
        Binding(Binds(("Op", 0, 0)), ("Op", 1)),
    ),
    (
        &["java"],
        "lambda_expression",
        "class A { void f() { Fn g = item -> item + 1; } }\n",
        Binding(Binds(("item", 0, 0)), ("item", 1)),
    ),
    (
        &["java"],
        "local_variable_declaration",
        "class A { int f() { int total = 1; return total; } }\n",
        Binding(Binds(("total", 0, 0)), ("total", 1)),
    ),
    (
        &["java"],
        "method_declaration",
        "class A { void run() {} }\n",
        Defines("run", DefKind::Method),
    ),
    (
        &["java"],
        "record_declaration",
        "record Point(int value) {}\n",
        Defines("Point", DefKind::Class),
    ),
    (
        &["java"],
        "record_declaration",
        "class A { void f() { record Pair(int a) {} Pair p = new Pair(1); } }\n",
        Binding(Binds(("Pair", 0, 0)), ("Pair", 1)),
    ),
    (
        &["java"],
        "record_pattern_component",
        "class A { void f(Object o) { if (o instanceof Point(int px, int py)) { g(px); } } }\n",
        Binding(Candidates(&[("px", 0, 0)]), ("px", 1)),
    ),
    (
        &["java"],
        "resource",
        "class A { void f() throws Exception { try (var stream = open()) { stream.read(); } } }\n",
        Binding(Binds(("stream", 0, 0)), ("stream", 1)),
    ),
    (
        &["java"],
        "spread_parameter",
        "record R(int... rest) {}\n",
        Defines("rest", DefKind::Field),
    ),
    (
        &["java"],
        "spread_parameter",
        "class A { int f(int... nums) { return nums.length; } }\n",
        Binding(Binds(("nums", 0, 0)), ("nums", 1)),
    ),
    (
        &["java"],
        "type_parameter",
        "class Box<Item> { Item value; }\n",
        Binding(Binds(("Item", 0, 0)), ("Item", 1)),
    ),
    (
        &["java"],
        "type_pattern",
        "class A { void f(Object o) { switch (o) { case String str -> g(str); default -> {} } } }\n",
        Binding(Candidates(&[("str", 0, 0)]), ("str", 1)),
    ),
    // ---- Python ----
    (
        &["python"],
        "as_pattern",
        "def f():\n    with open('a') as fh:\n        return fh\n",
        Binding(Binds(("fh", 0, 0)), ("fh", 1)),
    ),
    (
        &["python"],
        "assignment",
        "LIMIT = 3\n",
        Defines("LIMIT", DefKind::Constant),
    ),
    (
        &["python"],
        "assignment",
        "class A:\n    size = 1\n",
        Defines("size", DefKind::Field),
    ),
    (
        &["python"],
        "assignment",
        "A, B = load()\n",
        Defines("B", DefKind::Constant),
    ),
    (
        &["python"],
        "assignment",
        "first, *rest = items\n(p, [q, r]) = nested\n",
        Defines("r", DefKind::Constant),
    ),
    (
        &["python"],
        "assignment",
        "class A:\n    left, right = 1, 2\n",
        Defines("right", DefKind::Field),
    ),
    (
        &["python"],
        "assignment",
        "def f():\n    total = 1\n    return total\n",
        Binding(Binds(("total", 0, 0)), ("total", 1)),
    ),
    (
        &["python"],
        "augmented_assignment",
        "def f():\n    count += 1\n    return count\n",
        Binding(Binds(("count", 0, 0)), ("count", 1)),
    ),
    (
        &["python"],
        "case_pattern",
        "def f(cmd):\n    match cmd:\n        case [first, *rest]:\n            return first, rest\n",
        Binding(Binds(("rest", 0, 0)), ("rest", 1)),
    ),
    (
        &["python"],
        "class_definition",
        "class Base:\n    pass\n",
        Defines("Base", DefKind::Class),
    ),
    (
        &["python"],
        "class_definition",
        "def f():\n    class Local:\n        pass\n    return Local()\n",
        Binding(Binds(("Local", 0, 0)), ("Local", 1)),
    ),
    (
        &["python"],
        "default_parameter",
        "def f(limit=3):\n    return limit\n",
        Binding(Binds(("limit", 0, 0)), ("limit", 1)),
    ),
    (
        &["python"],
        "for_in_clause",
        "def f(xs):\n    return [elem * 2 for elem in xs]\n",
        Binding(Binds(("elem", 1, 0)), ("elem", 0)),
    ),
    (
        &["python"],
        "for_statement",
        "def f(xs):\n    for elem in xs:\n        print(elem)\n",
        Binding(Binds(("elem", 0, 0)), ("elem", 1)),
    ),
    (
        &["python"],
        "function_definition",
        "def run():\n    pass\n",
        Defines("run", DefKind::Function),
    ),
    (
        &["python"],
        "function_definition",
        "class A:\n    def go(self):\n        pass\n",
        Defines("go", DefKind::Method),
    ),
    (
        &["python"],
        "function_definition",
        "def outer():\n    def inner():\n        pass\n    return inner()\n",
        Binding(Binds(("inner", 0, 0)), ("inner", 1)),
    ),
    (
        &["python"],
        "lambda_parameters",
        "square = lambda num: num * num\n",
        Binding(Binds(("num", 0, 0)), ("num", 1)),
    ),
    (
        &["python"],
        "named_expression",
        "def f(xs):\n    if (size := len(xs)) > 3:\n        return size\n",
        Binding(Binds(("size", 0, 0)), ("size", 1)),
    ),
    (
        &["python"],
        "parameters",
        "def f(alpha, *rest, **kw):\n    return alpha, rest, kw\n",
        Binding(Binds(("rest", 0, 0)), ("rest", 1)),
    ),
    (
        &["python"],
        "type_alias_statement",
        "type Vector = list[float]\n",
        Defines("Vector", DefKind::Type),
    ),
    (
        &["python"],
        "type_alias_statement",
        "type Pairs[T] = list[tuple[T, T]]\n",
        Defines("Pairs", DefKind::Type),
    ),
    (
        &["python"],
        "type_parameter",
        "class Box[Item]:\n    def get(self) -> Item:\n        pass\n",
        Binding(Binds(("Item", 0, 0)), ("Item", 1)),
    ),
    (
        &["python"],
        "type_parameter",
        "type Pairs[Item] = list[tuple[Item, Item]]\n",
        Binding(Binds(("Item", 0, 0)), ("Item", 1)),
    ),
    (
        &["python"],
        "type_parameter",
        "def first[Item](xs: list[Item]) -> Item:\n    return xs[0]\n",
        Binding(Binds(("Item", 0, 0)), ("Item", 1)),
    ),
    (
        &["python"],
        "typed_default_parameter",
        "def f(limit: int = 3):\n    return limit\n",
        Binding(Binds(("limit", 0, 0)), ("limit", 1)),
    ),
    (
        &["python"],
        "typed_parameter",
        "def f(limit: int):\n    return limit\n",
        Binding(Binds(("limit", 0, 0)), ("limit", 1)),
    ),
    // ---- Go ----
    (
        &["go"],
        "const_spec",
        "package p\n\nconst Max = 3\n",
        Defines("Max", DefKind::Constant),
    ),
    (
        &["go"],
        "const_spec",
        "package p\n\nconst A, B = 1, 2\n",
        Defines("B", DefKind::Constant),
    ),
    (
        &["go"],
        "const_spec",
        "package p\n\nfunc f() int {\n\tconst kk = 2\n\treturn kk\n}\n",
        Binding(Binds(("kk", 0, 0)), ("kk", 1)),
    ),
    (
        &["go"],
        "field_declaration",
        "package p\n\ntype T struct{ size int }\n",
        Defines("size", DefKind::Field),
    ),
    (
        &["go"],
        "function_declaration",
        "package p\n\nfunc Run() {}\n",
        Defines("Run", DefKind::Function),
    ),
    (
        &["go"],
        "method_declaration",
        "package p\n\ntype T int\n\nfunc (t T) Go() {}\n",
        Defines("Go", DefKind::Method),
    ),
    (
        &["go"],
        "method_elem",
        "package p\n\ntype I interface{ Close() error }\n",
        Defines("Close", DefKind::Method),
    ),
    (
        &["go"],
        "parameter_declaration",
        "package p\n\nfunc f(count int) int { return count }\n",
        Binding(Binds(("count", 0, 0)), ("count", 1)),
    ),
    (
        &["go"],
        "range_clause",
        "package p\n\nfunc f(xs []int) {\n\tfor idx, val := range xs {\n\t\t_ = idx + val\n\t}\n}\n",
        Binding(Binds(("val", 0, 0)), ("val", 1)),
    ),
    (
        &["go"],
        "receive_statement",
        "package p\n\nfunc f(ch chan int) int {\n\tselect {\n\tcase msg := <-ch:\n\t\treturn msg\n\t}\n\treturn 0\n}\n",
        Binding(Binds(("msg", 0, 0)), ("msg", 1)),
    ),
    (
        &["go"],
        "short_var_declaration",
        "package p\n\nfunc f() int {\n\ttotal := 1\n\treturn total\n}\n",
        Binding(Binds(("total", 0, 0)), ("total", 1)),
    ),
    (
        &["go"],
        "type_alias",
        "package p\n\ntype Alias = int\n",
        Defines("Alias", DefKind::Type),
    ),
    (
        &["go"],
        "type_parameter_declaration",
        "package p\n\nfunc Map[Item any](xs []Item) []Item { return xs }\n",
        Binding(Binds(("Item", 0, 0)), ("Item", 1)),
    ),
    (
        &["go"],
        "type_spec",
        "package p\n\ntype Meters float64\n",
        Defines("Meters", DefKind::Type),
    ),
    (
        &["go"],
        "type_switch_statement",
        "package p\n\nfunc f(x any) {\n\tswitch val := x.(type) {\n\tcase int:\n\t\t_ = val\n\t}\n}\n",
        Binding(Binds(("val", 0, 0)), ("val", 1)),
    ),
    (
        &["go"],
        "var_spec",
        "package p\n\nvar Count int\n",
        Defines("Count", DefKind::Variable),
    ),
    (
        &["go"],
        "var_spec",
        "package p\n\nvar a, b = load()\n",
        Defines("b", DefKind::Variable),
    ),
    (
        &["go"],
        "var_spec",
        "package p\n\nfunc f() int {\n\tvar amount = 1\n\treturn amount\n}\n",
        Binding(Binds(("amount", 0, 0)), ("amount", 1)),
    ),
    (
        &["go"],
        "variadic_parameter_declaration",
        "package p\n\nfunc f(vals ...int) int { return len(vals) }\n",
        Binding(Binds(("vals", 0, 0)), ("vals", 1)),
    ),
];

#[test]
fn forms_rows_have_samples() {
    // Each kind of a definition row has a sample that defines it, and each
    // binding row (or definition row that is also local) one that binds.
    let mut missing = Vec::new();
    for form in FORMS {
        for lang in form.langs {
            let samples: Vec<&Sample> = SAMPLES
                .iter()
                .filter(|s| s.0.contains(lang) && s.1 == form.kind)
                .collect();
            let (kinds, local): (&[(DefKind, &[crate::RefSyntax])], bool) = match &form.role {
                Role::Def { kinds, local } => (kinds, local.is_some()),
                Role::Local(_) => (&[], true),
                _ => continue,
            };
            for (kind, _) in kinds {
                if !samples
                    .iter()
                    .any(|s| matches!(s.3, Defines(_, k) if k == *kind))
                {
                    missing.push(format!(
                        "{lang} {}: no sample defines a {kind:?}",
                        form.kind
                    ));
                }
            }
            if local && !samples.iter().any(|s| matches!(s.3, Binding(..))) {
                missing.push(format!("{lang} {}: no binding sample", form.kind));
            }
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}

#[test]
fn forms_samples_extract_and_bind() {
    // Definition samples: both extraction paths (the walker and
    // tree-sitter-tags) give the definition, and the same result.
    // Binding samples: the reference binds as the row says.
    let table = LanguageTable::core();
    let watchdog = test_watchdog();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut failures = Vec::new();
    for (langs, kind, source, shows) in SAMPLES {
        for lang in *langs {
            match shows {
                Defines(name, def_kind) => {
                    let l = table.get(lang).unwrap();
                    let deadline = Deadline::interactive(&watchdog);
                    let walked = extract(l, source.as_bytes(), &deadline, &cancel);
                    let tagged = extract_tags(l, source.as_bytes(), &deadline, &cancel);
                    if !walked
                        .defs
                        .iter()
                        .any(|d| &*d.name == *name && d.kind == *def_kind)
                    {
                        failures.push(format!(
                            "{lang} {kind}: no {def_kind:?} {name} in {:?}",
                            walked.defs
                        ));
                    }
                    if walked != tagged {
                        failures.push(format!("{lang} {kind}: walker and tags differ"));
                    }
                }
                Binding(expect, reference) => {
                    let expect = match expect {
                        Binds(at) => Binds(*at),
                        Candidates(at) => Candidates(at),
                        super::binding::Free => super::binding::Free,
                    };
                    let row = [Row {
                        what: kind,
                        source,
                        reference: *reference,
                        expect,
                    }];
                    let result = std::panic::catch_unwind(|| check(lang, &row));
                    if result.is_err() {
                        failures.push(format!("{lang} {kind}: binding"));
                    }
                }
            }
        }
    }
    watchdog.stop();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// (language, source, names that are local to the file, names that are
/// not): definitions nested in functions and blocks, members of local and
/// non-local classes.
const VISIBILITY: &[(&str, &str, &[&str], &[&str])] = &[
    (
        "python",
        "def register(kind):\n    def wrap(cls):\n        return cls\n    class Local:\n        size = 1\n        def run(self):\n            pass\n    return wrap\n\nclass Top:\n    class Inner:\n        def go(self):\n            pass\n",
        &["wrap", "Local", "size", "run"],
        &["register", "Top", "Inner", "go"],
    ),
    (
        "rust",
        "fn outer() {\n    fn helper() {}\n    struct Local { field: u8 }\n    impl Local { fn method(&self) {} }\n}\npub fn top() {}\nmod m { pub fn inner() {} }\nconst K: u8 = { fn in_const() -> u8 { 1 } in_const() };\n",
        &["helper", "Local", "field", "method", "in_const"],
        &["outer", "top", "inner", "K"],
    ),
    (
        "java",
        "class A {\n  void m() {\n    class Local { void run() {} }\n    Runnable r = new Runnable() { public void run2() {} };\n  }\n  static { class InStatic {} }\n  class Member { void go() {} }\n}\n",
        &["Local", "run", "run2", "InStatic"],
        &["A", "m", "Member", "go"],
    ),
    (
        "go",
        "package p\n\nfunc F() {\n\ttype local struct{ field int }\n\ttype iface interface{ M() }\n}\n\ntype Top struct{ Name string }\n",
        &["local", "field", "iface", "M"],
        &["F", "Top", "Name"],
    ),
    (
        "ts",
        "export function outer() {\n  function helper() {}\n  class Local { m() {} }\n}\nexport namespace NS { export function inside() {} }\nexport class C { constructor(public prop: number) {} method() {} }\n{ function inBlock() {} }\n",
        &["helper", "Local", "m", "inBlock"],
        &["outer", "NS", "inside", "C", "prop", "method"],
    ),
    (
        "js",
        "function outer() { function helper() {} }\nclass K { static { function inStatic() {} } }\nwith (o) { function inWith() {} }\n",
        &["helper", "inStatic", "inWith"],
        &["outer", "K"],
    ),
];

#[test]
fn forms_local_definitions_are_file_visible() {
    // A definition whose node lies inside a local scope of the binding
    // model (a function or block, not a class or namespace body) is local
    // to the file in every language, and a member is as local as its
    // class: checked against the scopes of the locals query (`ParsedFile`)
    // on every forms sample and the cases above, and the walker's scopes
    // (`extract`) give the same visibility.
    let table = LanguageTable::core();
    let watchdog = test_watchdog();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut failures = Vec::new();
    let mut sources: BTreeSet<(&str, &str)> =
        VISIBILITY.iter().map(|(l, s, _, _)| (*l, *s)).collect();
    for (langs, _, source, _) in SAMPLES {
        sources.extend(langs.iter().map(|l| (*l, *source)));
    }
    for (lang, source) in sources {
        let file = super::site::parse(lang, source);
        let (defs, scopes) = file.defs_and_local_scopes();
        for d in defs {
            let inside = scopes
                .iter()
                .any(|s| s.start <= d.range.start && d.range.end <= s.end && *s != d.range);
            let member = matches!(d.def.kind, DefKind::Field | DefKind::Method);
            if inside && !member && d.def.visibility != Visibility::File {
                failures.push(format!(
                    "{lang}: {} is in a local scope but {:?}",
                    d.def.name, d.def.visibility
                ));
            }
        }
        let l = table.get(lang).unwrap();
        let deadline = Deadline::interactive(&watchdog);
        let walked = extract(l, source.as_bytes(), &deadline, &cancel);
        let parsed: Vec<&crate::extract::Def> = defs.iter().map(|d| &d.def).collect();
        let walked: Vec<&crate::extract::Def> = walked.defs.iter().collect();
        if parsed != walked {
            failures.push(format!(
                "{lang}: the walker and ParsedFile differ on {source:?}"
            ));
        }
    }
    for (lang, source, local, not_local) in VISIBILITY {
        let file = super::site::parse(lang, source);
        let visibility = |name: &str| file.defs().find(|d| &*d.name == name).map(|d| d.visibility);
        for name in *local {
            if visibility(name) != Some(Visibility::File) {
                failures.push(format!(
                    "{lang}: {name} is {:?}, not File",
                    visibility(name)
                ));
            }
        }
        for name in *not_local {
            match visibility(name) {
                None => failures.push(format!("{lang}: no definition {name}")),
                Some(Visibility::File) => failures.push(format!("{lang}: {name} is File")),
                Some(_) => {}
            }
        }
    }
    watchdog.stop();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The table as the design document shows it.
fn render() -> String {
    let ns = |n: &Namespace| match n {
        Namespace::Value => "值",
        Namespace::Type => "类型",
        Namespace::Both => "值和类型",
    };
    let mut out = String::from("| 语言 | 节点 | 处理 | 说明 |\n|---|---|---|---|\n");
    for form in FORMS {
        let role = match &form.role {
            Role::Def { kinds, local } => {
                let kinds: Vec<String> = kinds
                    .iter()
                    .map(|(kind, by)| {
                        let by: Vec<String> = by.iter().map(|s| format!("{s:?}")).collect();
                        if by.is_empty() {
                            format!("{kind:?}")
                        } else {
                            format!("{kind:?}（{}）", by.join("、"))
                        }
                    })
                    .collect();
                match local {
                    Some(n) => format!("定义 {}；局部（{}）", kinds.join("、"), ns(n)),
                    None => format!("定义 {}", kinds.join("、")),
                }
            }
            Role::Local(n) => format!("局部（{}）", ns(n)),
            Role::Part => "组成部分".to_owned(),
            Role::Use => "引用".to_owned(),
            Role::Ignored => "忽略".to_owned(),
        };
        out.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            form.langs.join(" "),
            form.kind,
            role,
            form.note
        ));
    }
    out
}

#[test]
fn forms_table_matches_design() {
    // docs/design/code_intel.md「定义形式」 shows this table, between the
    // two markers; on a change, paste the expected block printed below.
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/design/code_intel.md"
    ))
    .expect("the design document");
    let begin = "<!-- forms:begin -->\n";
    let end = "<!-- forms:end -->";
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
