//! `logs_*`: the log records (docs/design/code_intel.md「日志与诊断」),
//! and that file contents, the names and identifiers in them, query text,
//! the queries' own text and error messages never reach one.

use std::sync::Arc;

use lectern_logging::testing::{self, Capture};
use lectern_services::code_intel::{Names, QueryKind, QueryState};

use crate::grammar::{GrammarLease, LoadedGrammar};
use crate::testing::npm::NpmTree;

use super::goto::{indexed, unindexed};
use super::support::{CHANGED, Harness};

/// In identifiers, strings and comments of every file, in the names of a
/// query, of dependencies and of a language pack, and in a pack's broken
/// query; never in a record. Never in a path either: paths are logged.
const CANARY: &str = "canary7k2m9q4x";

/// A tags query that does not compile (the last pattern is never closed)
/// and quotes the canary.
const BAD_QUERY: &str =
    "((identifier) @name (#eq? @name \"canary7k2m9q4x\")) @reference.call\n((class_declaration";

/// The records with message `msg`.
fn records(log: &Capture, msg: &str) -> Vec<String> {
    let msg = format!("\"msg\":\"{msg}\"");
    log.lines()
        .into_iter()
        .filter(|l| l.contains(&msg))
        .collect()
}

/// The raw JSON text of field `name` in `line`.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let start = line.find(&format!("\"{name}\":"))? + name.len() + 3;
    let rest = &line[start..];
    let end = if let Some(text) = rest.strip_prefix('"') {
        text.find('"')? + 2
    } else if rest.starts_with('{') {
        rest.find('}')? + 1
    } else {
        rest.find([',', '}'])?
    };
    Some(&rest[..end])
}

/// The one record with message `msg` whose field `key` is the JSON string
/// `value`: `cargo test` runs other tests in the same process.
fn record_with(log: &Capture, msg: &str, key: &str, value: &str) -> String {
    let want = format!("\"{value}\"");
    let found: Vec<String> = records(log, msg)
        .into_iter()
        .filter(|l| field(l, key) == Some(want.as_str()))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{msg:?} with {key} = {value}: {:#?}",
        log.lines()
    );
    found.into_iter().next().expect("one record")
}

fn root(h: &Harness, wt: &str) -> String {
    h.git.root(wt).display().to_string()
}

fn crlf(text: &str) -> Vec<u8> {
    text.replace('\n', "\r\n").into_bytes()
}

/// A TypeScript library and its user, a file of every other core language
/// and an untracked file, each with the canary in identifiers, strings and
/// comments.
fn sources() -> Vec<(&'static str, String)> {
    let c = CANARY;
    vec![
        (
            "src/lib.ts",
            format!(
                "// {c} in a comment\n\
                 export function {c}Helper(a: number): number {{\n  return a + 1;\n}}\n\
                 export const {c}Text = \"{c} in a string\";\n"
            ),
        ),
        (
            "src/main.ts",
            format!(
                "import {{ {c}Helper }} from './lib';\n\
                 export function run(): number {{\n  const {c}Local = {c}Helper(1);\n  \
                 return {c}Local + {c}Helper(2);\n}}\n"
            ),
        ),
        (
            "src/view.tsx",
            format!("export const {c}View = () => <div title=\"{c}\">{{{c}Helper(1)}}</div>;\n"),
        ),
        (
            "src/old.js",
            format!("function {c}Old() {{ return '{c}'; }}\nmodule.exports = {c}Old;\n"),
        ),
        (
            "src/Item.java",
            format!(
                "class {c}Item {{\n  // {c}\n  int {c}Size() {{ return \"{c}\".length(); }}\n}}\n"
            ),
        ),
        (
            "src/item.py",
            format!("def {c}_item():\n    \"\"\"{c} doc\"\"\"\n    return '{c}'\n"),
        ),
        (
            "src/item.rs",
            format!("pub fn {c}_item() -> &'static str {{\n    \"{c}\" // {c}\n}}\n"),
        ),
        (
            "src/item.go",
            format!("package item\n\n// {c}\nfunc {c}Item() string {{ return \"{c}\" }}\n"),
        ),
    ]
}

/// Indexes, jumps, finds references, searches, builds the name table and
/// reads dependencies over files with the canary, and runs the failure
/// paths a test can reach; the records are written and none holds the
/// canary or an error's message.
#[test]
fn logs_hold_no_file_contents() {
    let log = testing::capture();
    let files = sources();
    let texts: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();

    // Index every core language, an untracked file too; jump, find the
    // references, list a name (a ⌘K query for the canary) and build the
    // name table.
    let h = indexed(&texts);
    h.git.write(
        "main",
        "src/untracked.ts",
        format!("export const {CANARY}New = '{CANARY}';\n").as_bytes(),
    );
    h.emit(&[("src/untracked.ts", CHANGED)]);
    h.settle();
    let jump = h.goto("src/main.ts", &format!("{CANARY}Helper"), 1);
    assert!(!jump.candidates.is_empty(), "{jump:?}");
    let def = h
        .goto("src/lib.ts", &format!("{CANARY}Helper"), 0)
        .candidates[0]
        .definition
        .clone();
    let refs = h.ask(QueryKind::References { of: def });
    assert!(refs.callers.len() + refs.same_name.len() >= 3, "{refs:?}");
    let named = h.ask(QueryKind::Named {
        name: Arc::from(format!("{CANARY}Helper")),
        near: None,
    });
    assert!(!named.candidates.is_empty(), "{named:?}");
    let _ = h.indexer.names(&h.key());
    h.settle();
    assert!(matches!(h.indexer.names(&h.key()), Names::Ready(_)));
    // The worktree comparison fails (a `Corrupt` with its own text).
    h.git.fail_compare(true);
    h.git.move_head("main");
    h.emit_git(&[("HEAD", CHANGED)]);
    h.settle();

    // Nothing indexed: the jump's time-boxed search reads the files.
    let u = unindexed(&texts, &[]);
    let searched = u.goto("src/main.ts", &format!("{CANARY}Helper"), 1);
    assert!(
        matches!(searched.state, QueryState::Done(_)),
        "{searched:?}"
    );

    // Converting CRLF checkouts: one file fails, so its batch is tried
    // one by one. Another worktree's root is gone.
    let c = Harness::new();
    for i in 0..5 {
        let path = format!("crlf/f{i}.ts");
        c.git.eol(&path);
        let text = format!("export const {CANARY}{i} = \"{CANARY}\";\n");
        c.git.add("main", &[(&path, &crlf(&text))]);
    }
    c.git.commit("main");
    let other = c.git.add_worktree("other");
    c.git.add(
        "other",
        &[("o.ts", format!("const {CANARY} = 1;\n").as_bytes())],
    );
    std::fs::remove_dir_all(&other).expect("removed");
    c.pause();
    c.open();
    c.git.fail_canonical("crlf/f2.ts");
    let events: Vec<String> = (0..5).map(|i| format!("crlf/f{i}.ts")).collect();
    let events: Vec<(&str, _)> = events.iter().map(|p| (p.as_str(), CHANGED)).collect();
    c.emit(&events);
    c.settle();
    c.resume();
    c.settle();

    // A language pack whose tags query does not compile; its name and
    // its file have the canary.
    let p = Harness::new();
    p.git.add(
        "main",
        &[(
            "src/a.zpack",
            format!("class {CANARY}Pack {{ void {CANARY}() {{}} }}\n").as_bytes(),
        )],
    );
    p.git.commit("main");
    p.open();
    p.grammars.install(
        &format!("{CANARY}lang"),
        &["zpack"],
        LoadedGrammar {
            lease: GrammarLease::new(Arc::new(())),
            language: tree_sitter_java::LANGUAGE.into(),
            tags_query: Arc::from(BAD_QUERY),
            locals_query: None,
            artifact_sha256: [7; 32],
            compat_group: None,
        },
    );
    p.settle();

    // More direct dependencies than the tier takes, named after the
    // canary; the first one installed with the canary in its declarations.
    let d = Harness::new();
    d.git.ignore("node_modules/");
    let tree = NpmTree::new(&d.git.root("main"));
    let deps: Vec<String> = (0..520).map(|i| format!("{CANARY}-p{i}")).collect();
    let names: Vec<&str> = deps.iter().map(String::as_str).collect();
    tree.manifest("", &names, "");
    tree.package(
        &format!("node_modules/{CANARY}-p0"),
        &deps[0],
        "1.0.0",
        "",
        &[(
            "index.d.ts",
            &format!("export declare function {CANARY}Dep(a: string): string; // {CANARY}\n"),
        )],
    );
    d.open();

    // The records, their levels and fields.
    let compare = record_with(
        log,
        "the worktree comparison failed; the overlay is kept until the next event",
        "root",
        &root(&h, "main"),
    );
    assert_eq!(field(&compare, "lv"), Some("\"warn\""));
    assert_eq!(field(&compare, "tg"), Some("\"code_intel\""));
    assert_eq!(field(&compare, "error"), Some("\"corrupt\""));
    let batch = record_with(
        log,
        "converting a batch to canonical form failed; converting its files one by one",
        "root",
        &root(&c, "main"),
    );
    assert_eq!(field(&batch, "lv"), Some("\"warn\""));
    assert_eq!(field(&batch, "files"), Some("5"));
    assert_eq!(field(&batch, "error"), Some("\"corrupt\""));
    let file: Vec<String> = records(
        log,
        "cannot convert a file to its canonical form; it is not indexed",
    )
    .into_iter()
    .filter(|l| field(l, "path") == Some("\"crlf/f2.ts\""))
    .collect();
    assert!(!file.is_empty(), "{:#?}", log.lines());
    for line in &file {
        assert_eq!(field(line, "lv"), Some("\"warn\""));
        assert_eq!(field(line, "error"), Some("\"corrupt\""));
        assert!(field(line, "bytes").is_some());
    }
    let skipped = record_with(
        log,
        "cannot read another worktree; its files are skipped",
        "root",
        &root(&c, "other"),
    );
    assert_eq!(field(&skipped, "lv"), Some("\"warn\""));
    assert_eq!(field(&skipped, "op"), Some("\"root\""));
    assert_eq!(field(&skipped, "error"), Some("\"io\""));
    assert!(
        field(&skipped, "io").is_some_and(|io| io.contains("NotFound")),
        "{skipped}"
    );
    let pack = record_with(
        log,
        "the tags queries do not compile; the language is disabled for this run",
        "grammar",
        "0707070707070707",
    );
    assert_eq!(field(&pack, "lv"), Some("\"error\""));
    assert_eq!(field(&pack, "lang"), Some("null"));
    assert_eq!(field(&pack, "error"), Some("\"syntax\""));
    assert!(field(&pack, "row").is_some_and(|r| r != "null"), "{pack}");
    let direct: Vec<String> = records(
        log,
        "more direct dependencies than the tier takes; the rest only resolve on demand",
    )
    .into_iter()
    .filter(|l| field(l, "more") == Some("8"))
    .collect();
    assert_eq!(direct.len(), 1, "{:#?}", log.lines());
    assert_eq!(field(&direct[0], "lv"), Some("\"warn\""));
    assert_eq!(field(&direct[0], "path"), Some("\"package.json\""));
    assert_eq!(field(&direct[0], "kept"), Some("512"));

    log.assert_absent(CANARY);
    // The fake's error text (`Corrupt("<path>: conversion failed")`) is
    // written as its kind only.
    log.assert_absent("conversion failed");
}
