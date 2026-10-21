//! `deps_*`: the dependency tier (「依赖层」, ADR 0033, 0040) over synthetic
//! npm trees the tests write under the worktree root (npm's flat layout,
//! pnpm's links into `.pnpm`, a pnpm workspace): discovery, the reading
//! limits (every path `Roots` touched is logged), identities and stat
//! tables, edges from stored specifiers, the roots' validity, the limits,
//! the namespace's quota, queries and the yield.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use lectern_cache::Tables;
use lectern_services::code_intel::{
    Binding, CandidateTier, Completeness, DepOrigin, DepRoot, Ecosystem, Evidence, Focus,
    QueryKind, QueryState, TextOrigin, TextRef, TierState,
};
use lectern_services::project::{BlobId, RelPath};

use super::support::{CHANGED, Harness, Options, packages};
use crate::deps::json::Json;
use crate::deps::npm::{direct_dependencies, entry_candidates, spec_candidates};
use crate::deps::{DepMap, Limits, PathTable, allowed, extensions};
use crate::extract::{Deadline, ExtractStatus, LanguageTable, SpecKind, extract};
use crate::index::lock;
use crate::index::state::Project;
use crate::resolve::{
    CandidateDraft, Import, ImportTarget, ImportedNames, PackageScope, ResolveContext, Resolver,
};
use crate::site::{ParsedFile, Site};
use crate::store::{DEPS_NAMESPACE, DEPS_SCHEMA, DEPS_STATEMENTS, Version};
use crate::testing::npm::{NpmTree, declarations, npm_table, ts_project};
use crate::{LangName, lang_name};

/// A harness whose `node_modules` (and `packages/*/node_modules`) are
/// ignored, as `.gitignore` has them, and the tree writer of its main
/// worktree.
fn harness() -> (Harness, NpmTree) {
    let h = Harness::new();
    ignore(&h);
    let tree = NpmTree::new(&h.git.root("main"));
    (h, tree)
}

fn ignore(h: &Harness) {
    for dir in ["node_modules/", "packages/app/node_modules/", "vendor/"] {
        h.git.ignore(dir);
    }
}

fn project(h: &Harness) -> Arc<Project> {
    h.indexer.inner().project(&h.key()).expect("open")
}

fn dep_map(h: &Harness) -> Arc<DepMap> {
    lock(&project(h).state).deps.map.clone()
}

/// The dependency map's files as (package, path).
fn dep_files(h: &Harness) -> Vec<(String, String)> {
    dep_map(h)
        .files()
        .into_iter()
        .map(|(p, path, _)| (p, path))
        .collect()
}

fn dep_paths(h: &Harness) -> Vec<String> {
    dep_files(h).into_iter().map(|(_, p)| p).collect()
}

/// What the session's roots touched: (operation, path).
fn log(h: &Harness) -> Vec<(&'static str, String)> {
    match roots(h) {
        Some(r) => lock(&r.log).clone(),
        None => Vec::new(),
    }
}

fn roots(h: &Harness) -> Option<Arc<crate::deps::Roots>> {
    let p = project(h);
    let st = lock(&p.state);
    st.deps.roots.clone()
}

/// Whether the session's dependency tier stopped.
fn stopped(h: &Harness) -> bool {
    let p = project(h);
    let st = lock(&p.state);
    st.deps.stopped
}

fn reads(h: &Harness) -> Vec<String> {
    log(h)
        .into_iter()
        .filter(|(op, _)| *op == "read")
        .map(|(_, p)| p)
        .collect()
}

fn ts_version() -> Version {
    Version::of(LanguageTable::core().get("ts").expect("ts"))
}

/// Whether `code_intel.deps` has a unit of `source` (TypeScript).
fn dep_stored(h: &Harness, source: &str) -> bool {
    project(h)
        .deps
        .exists(&ts_version(), &[h.git.oid(source.as_bytes())])
        .is_some_and(|f| !f.is_empty())
}

fn deps_state(h: &Harness) -> TierState {
    h.coverage().deps
}

/// The ready path tables of the session.
fn tables(h: &Harness) -> Vec<Arc<PathTable>> {
    lock(&project(h).state).deps.tables()
}

fn table_of(h: &Harness, package: &str) -> Arc<PathTable> {
    tables(h)
        .into_iter()
        .find(|t| &*t.origin.package == package)
        .unwrap_or_else(|| panic!("a table of {package}"))
}

/// A package whose `index.d.ts` is `index` and whose other files are `files`.
fn simple(
    tree: &NpmTree,
    dir: &str,
    name: &str,
    version: &str,
    index: &str,
    files: &[(&str, &str)],
) {
    let mut all = vec![("index.d.ts", index)];
    all.extend_from_slice(files);
    tree.package(dir, name, version, "", &all);
}

/// Opens the project (a manifest with `deps` at the root) and settles.
fn open(h: &Harness, tree: &NpmTree, deps: &[&str]) {
    tree.manifest("", deps, "");
    h.open();
}

#[test]
fn deps_extensions_are_the_npm_entries_of_the_shared_list() {
    // The TS tier's run of DEPENDENCY_EXTENSIONS: never a Rust, Go or
    // Python entry.
    assert_eq!(
        extensions(Ecosystem::Npm),
        [".d.ts", ".d.mts", ".d.cts", ".ts", ".tsx", "package.json"]
    );
    for ok in [
        "a.d.ts",
        "x/b.d.mts",
        "c.d.cts",
        "d.ts",
        "e.tsx",
        "node_modules/x/package.json",
    ] {
        assert!(allowed(Ecosystem::Npm, ok), "{ok}");
    }
    for no in [
        "a.js",
        "a.json",
        "a.rs",
        "Cargo.toml",
        "a.go",
        "go.mod",
        "a.py",
        "a.pyi",
        "RECORD",
        ".ts",
        "x/.tsx",
        "package.json5",
        "a.TS",
    ] {
        assert!(!allowed(Ecosystem::Npm, no), "{no}");
    }
}

#[test]
fn deps_direct_dependencies_take_four_fields_in_order() {
    let manifest = Json::parse(
        br#"{
          "dependencies": { "b": "1", "a": "1", "../evil": "1", "@s/x": "1" },
          "devDependencies": { "a": "1", "c": "1", "@bad": "1" },
          "peerDependencies": { "d": "1" },
          "optionalDependencies": { "e": "1", "bad/name": "1" },
          "other": { "f": "1" }
        }"#,
    )
    .expect("json");
    let (names, more) = direct_dependencies(&manifest);
    assert_eq!(names, ["b", "a", "@s/x", "c", "d", "e"]);
    assert_eq!(more, 0);
    let mut json = String::from("{\"dependencies\":{");
    for i in 0..520 {
        let sep = if i == 0 { "" } else { "," };
        json.push_str(&format!("{sep}\"p{i}\":\"1\""));
    }
    json.push_str("}}");
    let (names, more) = direct_dependencies(&Json::parse(json.as_bytes()).expect("json"));
    assert_eq!(names.len(), 512);
    assert_eq!(names[511], "p511");
    assert_eq!(more, 8);
}

#[test]
fn deps_entry_candidates_follow_the_order() {
    let entries = |json: &str| entry_candidates(&Json::parse(json.as_bytes()).expect("json"));
    // `exports`' types condition, depth first, under `import`.
    assert_eq!(
        entries(
            r#"{"exports": {".": {"import": {"types": "./esm/i.d.mts", "default": "./esm/i.js"},
                             "require": {"types": "./cjs/i.d.cts"}}, "./x": "./x.js"},
                "types": "t.d.ts", "main": "lib/main.js"}"#
        ),
        ["esm/i.d.mts", "t.d.ts", "index.d.ts", "lib/main.d.ts"]
    );
    // A conditions object at the top is `"."`.
    assert_eq!(
        entries(r#"{"exports": {"require": {"types": "./r.d.ts"}}}"#),
        ["r.d.ts", "index.d.ts"]
    );
    assert_eq!(
        entries(r#"{"typings": "ty.d.ts", "main": "m.cjs"}"#),
        ["ty.d.ts", "index.d.ts", "m.d.cts"]
    );
    // Leaving the package is dropped.
    assert_eq!(
        entries(r#"{"types": "../../.ssh/id_ed25519", "main": "/etc/passwd.js"}"#),
        ["index.d.ts"]
    );
}

#[test]
fn deps_spec_candidates_follow_the_order() {
    assert_eq!(
        spec_candidates("lib/a.d.ts", "./b.js"),
        [
            "lib/b.js",
            "lib/b.d.ts",
            "lib/b.js.d.ts",
            "lib/b.js.d.mts",
            "lib/b.js.d.cts",
            "lib/b.js.ts",
            "lib/b.js.tsx",
            "lib/b.js/index.d.ts",
            "lib/b.js/index.ts",
        ]
    );
    assert_eq!(spec_candidates("a.d.ts", "../../x"), Vec::<String>::new());
    assert_eq!(
        spec_candidates("a/b.d.ts", ".."),
        ["index.d.ts", "index.ts"]
    );
}

#[test]
fn deps_specifiers_come_from_the_walk() {
    let source = r#"/// <reference path="globals.d.ts" />
/// <reference types="node" />
import { A } from "./a";
import type { B } from './b.js';
import x = require("./c");
export * from "./d";
export { E } from "../e";
import React from "react";
export declare function f(): typeof import("./g");
const h = 1;
"#;
    let table = LanguageTable::core();
    let lang = table.get("ts").expect("ts");
    let watchdog = super::extract::test_watchdog();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let with = extract(
        lang,
        source.as_bytes(),
        &Deadline::interactive(&watchdog).with_specifiers(),
        &cancel,
    );
    let without = extract(
        lang,
        source.as_bytes(),
        &Deadline::interactive(&watchdog),
        &cancel,
    );
    watchdog.stop();
    let specs: Vec<(SpecKind, &str)> = with.specs.iter().map(|s| (s.kind, &*s.text)).collect();
    assert_eq!(
        specs,
        // In the order of the source.
        [
            (SpecKind::Path, "globals.d.ts"),
            (SpecKind::Types, "node"),
            (SpecKind::Module, "./a"),
            (SpecKind::Module, "./b.js"),
            (SpecKind::Module, "./c"),
            (SpecKind::Module, "./d"),
            (SpecKind::Module, "../e"),
            (SpecKind::Module, "react"),
            (SpecKind::Module, "./g"),
        ]
    );
    assert!(without.specs.is_empty(), "only when asked");
    assert_eq!(with.defs, without.defs);
    assert_eq!(with.refs, without.refs);
}

#[test]
fn deps_closure_follows_relative_imports_only() {
    let (h, tree) = harness();
    simple(
        &tree,
        "node_modules/lib",
        "lib",
        "1.2.3",
        "/// <reference path=\"globals.d.ts\" />\n/// <reference types=\"node\" />\nexport * from \"./a\";\nimport { B } from \"./b.js\";\nimport R from \"react\";\nexport * from \"./sub\";\nexport * from \"../../outside\";\nexport declare const lib: number;\n",
        &[
            ("globals.d.ts", "declare var G: number;\n"),
            (
                "a.d.ts",
                "export * from \"./deep/c\";\nexport declare const a: number;\n",
            ),
            ("b.d.ts", "export declare const B: number;\n"),
            ("b.js", "module.exports = {};\n"),
            ("sub/index.d.ts", "export declare const s: number;\n"),
            (
                "deep/c.d.ts",
                "export * from \"../a\";\nexport declare const c: number;\n",
            ),
            ("unused.d.ts", "export declare const u: number;\n"),
        ],
    );
    simple(
        &tree,
        "node_modules/react",
        "react",
        "19.0.0",
        "export declare const R: number;\n",
        &[],
    );
    tree.write("outside.d.ts", b"export declare const o: number;\n");
    open(&h, &tree, &["lib"]);
    assert_eq!(deps_state(&h), TierState::Done);
    assert_eq!(
        dep_paths(&h),
        [
            "node_modules/lib/a.d.ts",
            "node_modules/lib/b.d.ts",
            "node_modules/lib/deep/c.d.ts",
            "node_modules/lib/globals.d.ts",
            "node_modules/lib/index.d.ts",
            "node_modules/lib/sub/index.d.ts",
        ]
    );
    // The bare `react` is not followed (not a direct dependency here).
    assert!(log(&h).iter().all(|(_, p)| !p.contains("react")));
    let t = table_of(&h, "lib");
    assert_eq!(t.origin.version.as_deref(), Some("1.2.3"));
    assert_eq!(
        t.origin.root,
        DepRoot::Worktree {
            dir: "node_modules".into()
        }
    );
}

#[test]
fn deps_entry_falls_back_to_the_types_package() {
    let (h, tree) = harness();
    // No declarations of their own: `main` is a script without `.d.ts`.
    tree.package(
        "node_modules/lib",
        "lib",
        "1.0.0",
        "\"main\": \"lib.js\"",
        &[("lib.js", "1;\n")],
    );
    tree.package(
        "node_modules/@scope/x",
        "@scope/x",
        "2.0.0",
        "",
        &[("x.js", "1;\n")],
    );
    simple(
        &tree,
        "node_modules/@types/lib",
        "@types/lib",
        "1.0.1",
        "export declare function lib(): void;\n",
        &[],
    );
    simple(
        &tree,
        "node_modules/@types/scope__x",
        "@types/scope__x",
        "2.0.1",
        "export declare function x(): void;\n",
        &[],
    );
    open(&h, &tree, &["lib", "@scope/x"]);
    assert_eq!(deps_state(&h), TierState::Done);
    let files = dep_files(&h);
    assert!(files.contains(&(
        "@types/lib".into(),
        "node_modules/@types/lib/index.d.ts".into()
    )));
    assert!(files.contains(&(
        "@types/scope__x".into(),
        "node_modules/@types/scope__x/index.d.ts".into()
    )));
    assert_eq!(table_of(&h, "lib").types.as_deref(), Some("@types/lib"));
    assert!(reads(&h).iter().all(|p| !p.ends_with(".js")));
}

#[test]
fn deps_lookup_goes_up_from_the_subproject() {
    let (h, tree) = harness();
    h.git.add(
        "main",
        &[("packages/app/src/a.ts", b"export const a = 1;\n")],
    );
    h.git.commit("main");
    tree.manifest("packages/app", &["up"], "");
    simple(
        &tree,
        "node_modules/up",
        "up",
        "1.0.0",
        "export declare function up(): void;\n",
        &[],
    );
    h.open_with("main", packages(&["packages/app"]));
    h.current("packages/app/src/a.ts");
    h.settle();
    assert_eq!(deps_state(&h), TierState::Done);
    assert_eq!(dep_paths(&h), ["node_modules/up/index.d.ts"]);
}

#[test]
fn deps_root_symlink_is_not_a_root() {
    let (h, tree) = harness();
    let outside = tempfile::tempdir().expect("temp dir");
    let o = NpmTree::new(outside.path());
    simple(
        &o,
        "lib",
        "lib",
        "1.0.0",
        "export declare const secret: number;\n",
        &[],
    );
    tree.link("node_modules", outside.path().to_str().expect("utf-8"));
    open(&h, &tree, &["lib"]);
    assert_eq!(deps_state(&h), TierState::Done);
    assert!(dep_paths(&h).is_empty());
    assert!(roots(&h).expect("roots").dirs().is_empty());
    assert!(log(&h).iter().all(|(op, _)| *op == "open"), "{:?}", log(&h));
}

#[test]
fn deps_types_outside_the_package_is_dropped() {
    let (h, tree) = harness();
    tree.write(".ssh/id_ed25519", b"PRIVATE\n");
    tree.write(
        "node_modules/secret.d.ts",
        b"export declare const s: number;\n",
    );
    tree.package(
        "node_modules/evil",
        "evil",
        "1.0.0",
        "\"types\": \"../../.ssh/id_ed25519\"",
        &[(
            "index.d.ts",
            "export * from \"../secret\";\nexport * from \"../../.ssh/id_ed25519\";\nexport declare const e: number;\n",
        )],
    );
    open(&h, &tree, &["evil"]);
    assert_eq!(dep_paths(&h), ["node_modules/evil/index.d.ts"]);
    for (op, path) in log(&h) {
        assert!(
            path == "node_modules" || path.starts_with("node_modules/evil"),
            "{op} {path}"
        );
    }
}

#[test]
fn deps_pnpm_links_resolve_inside_the_root() {
    let (h, tree) = harness();
    let real = "node_modules/.pnpm/react@19.0.0/node_modules/react";
    simple(
        &tree,
        real,
        "react",
        "19.0.0",
        "export * from \"./jsx\";\nexport declare function useState(): void;\n",
        &[("jsx.d.ts", "export declare const jsx: number;\n")],
    );
    tree.link(
        "node_modules/react",
        ".pnpm/react@19.0.0/node_modules/react",
    );
    // pnpm-lock.yaml: not parsed, the fallback identity.
    tree.write("pnpm-lock.yaml", b"lockfileVersion: '9.0'\n");
    open(&h, &tree, &["react"]);
    assert_eq!(
        dep_paths(&h),
        [format!("{real}/index.d.ts"), format!("{real}/jsx.d.ts")]
    );
    assert!(table_of(&h, "react").identity.starts_with(b"pkg:"));
}

#[test]
fn deps_link_into_a_discovered_root_is_read() {
    let (h, tree) = harness();
    h.git.add(
        "main",
        &[("packages/app/src/a.ts", b"export const a = 1;\n")],
    );
    h.git.commit("main");
    tree.manifest("packages/app", &["react"], "");
    let real = "node_modules/.pnpm/react@19.0.0/node_modules/react";
    simple(
        &tree,
        real,
        "react",
        "19.0.0",
        "export declare function useState(): void;\n",
        &[],
    );
    tree.write("node_modules/.modules.yaml", b"layout: 5\n");
    // The workspace package's link into the root's virtual store.
    tree.link(
        "packages/app/node_modules/react",
        "../../../node_modules/.pnpm/react@19.0.0/node_modules/react",
    );
    h.open_with("main", packages(&["packages/app"]));
    h.current("packages/app/src/a.ts");
    h.settle();
    assert_eq!(deps_state(&h), TierState::Done);
    assert_eq!(dep_paths(&h), [format!("{real}/index.d.ts")]);
    let t = table_of(&h, "react");
    assert_eq!(
        t.origin.root,
        DepRoot::Worktree {
            dir: "node_modules".into()
        },
        "read through the root it lies in"
    );
}

#[test]
fn deps_link_outside_the_discovered_roots_is_rejected() {
    let (h, tree) = harness();
    let outside = tempfile::tempdir().expect("temp dir");
    let o = NpmTree::new(outside.path());
    simple(
        &o,
        "a",
        "a",
        "1.0.0",
        "export declare const a: number;\n",
        &[],
    );
    // Absolute, out of the worktree, into a worktree directory that is no
    // root, and into another worktree's root.
    tree.link(
        "node_modules/a",
        &outside.path().join("a").to_string_lossy(),
    );
    tree.link("node_modules/b", "../../b");
    simple(
        &tree,
        "vendor/c",
        "c",
        "1.0.0",
        "export declare const c: number;\n",
        &[],
    );
    tree.link("node_modules/c", "../vendor/c");
    let feature = h.git.add_worktree("feature");
    simple(
        &NpmTree::new(&feature),
        "node_modules/d",
        "d",
        "1.0.0",
        "export declare const d: number;\n",
        &[],
    );
    let rel = pathdiff(
        &h.git.root("main").join("node_modules"),
        &feature.join("node_modules/d"),
    );
    tree.link("node_modules/d", &rel);
    // One that is fine, to see the tier ran.
    simple(
        &tree,
        "node_modules/ok",
        "ok",
        "1.0.0",
        "export declare const ok: number;\n",
        &[],
    );
    open(&h, &tree, &["a", "b", "c", "d", "ok"]);
    assert_eq!(deps_state(&h), TierState::Done);
    assert_eq!(dep_paths(&h), ["node_modules/ok/index.d.ts"]);
    for (op, path) in log(&h) {
        assert!(
            path == "node_modules" || path.starts_with("node_modules/"),
            "{op} {path}"
        );
        assert!(
            !path.contains("vendor") && !path.contains("feature"),
            "{op} {path}"
        );
    }
}

/// `to` relative to the directory `from`, with `..` as needed.
fn pathdiff(from: &std::path::Path, to: &std::path::Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out: Vec<String> = vec!["..".into(); from.len() - common];
    out.extend(
        to[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    out.join("/")
}

#[test]
fn deps_reads_only_allowed_extensions() {
    let (h, tree) = harness();
    simple(
        &tree,
        "node_modules/lib",
        "lib",
        "1.0.0",
        "export * from \"./x.js\";\nexport * from \"./y.json\";\nexport * from \"./z.rs\";\nexport * from \"./w.py\";\nexport * from \"./ok\";\n",
        &[
            ("x.js", "1;\n"),
            ("y.json", "{}\n"),
            ("z.rs", "fn z() {}\n"),
            ("w.py", "def w(): pass\n"),
            ("ok.tsx", "export const ok = 1;\n"),
        ],
    );
    open(&h, &tree, &["lib"]);
    assert_eq!(
        dep_paths(&h),
        ["node_modules/lib/index.d.ts", "node_modules/lib/ok.tsx"]
    );
    for (op, path) in log(&h) {
        let name = path.rsplit('/').next().unwrap_or(&path);
        assert!(
            path == "node_modules"
                || name == "lib"
                || name == "package.json"
                || name.ends_with(".ts")
                || name.ends_with(".tsx")
                || name.ends_with(".mts")
                || name.ends_with(".cts"),
            "{op} {path}"
        );
    }
}

#[test]
fn deps_cache_roots_come_from_the_environment() {
    // TS / JS never use a root outside the worktree (ADR 0033: the cache
    // roots' paths come from Lectern's own environment, and only Cargo, Go
    // and #26 have them): a manifest naming one is not read.
    let (h, tree) = harness();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    tree.package(
        "node_modules/lib",
        "lib",
        "1.0.0",
        &format!("\"types\": \"{home}/.cargo/registry/src/x/lib.d.ts\""),
        &[("index.d.ts", "export declare const l: number;\n")],
    );
    open(&h, &tree, &["lib"]);
    assert_eq!(dep_paths(&h), ["node_modules/lib/index.d.ts"]);
    for t in tables(&h) {
        assert!(matches!(t.origin.root, DepRoot::Worktree { .. }));
    }
    assert!(
        log(&h)
            .iter()
            .all(|(_, p)| !p.starts_with('/') && !p.contains(".cargo"))
    );
}

/// Two packages with a lockfile: `a` (index → b) and `c`.
fn locked_tree(tree: &NpmTree, a_integrity: &str, a_version: &str) {
    simple(
        &tree.clone(),
        "node_modules/a",
        "a",
        a_version,
        "export * from \"./b\";\nexport declare const a: number;\n",
        &[("b.d.ts", "export declare const b: number;\n")],
    );
    simple(
        tree,
        "node_modules/c",
        "c",
        "3.0.0",
        "export declare const c: number;\n",
        &[],
    );
    lockfile(tree, a_integrity, a_version, 0);
}

fn lockfile(tree: &NpmTree, a_integrity: &str, a_version: &str, padding: usize) {
    tree.lockfile(
        "",
        &[
            ("node_modules/a", a_version, a_integrity),
            ("node_modules/c", "3.0.0", "sha512-c"),
        ],
        padding,
    );
}

fn reopen(h: Harness) -> Harness {
    let h = h.reopen(Options::default());
    ignore(&h);
    h.open();
    h
}

#[test]
fn deps_same_identity_reads_nothing() {
    let (h, tree) = harness();
    locked_tree(&tree, "sha512-a1", "1.0.0");
    open(&h, &tree, &["a", "c"]);
    assert_eq!(deps_state(&h), TierState::Done);
    assert_eq!(dep_paths(&h).len(), 3);
    assert!(table_of(&h, "a").identity.starts_with(b"npm:"));
    assert_eq!(h.extractions(), 3);
    // The reopened harness counts from zero.
    let h = reopen(h);
    assert_eq!(deps_state(&h), TierState::Done);
    assert_eq!(dep_paths(&h).len(), 3, "the map from the stored tables");
    assert!(reads(&h).is_empty(), "{:?}", reads(&h));
    for (op, path) in log(&h) {
        assert!(
            matches!(op, "open" | "revalidate" | "lookup")
                || (op == "stat" && path.ends_with("/package.json")),
            "{op} {path}"
        );
    }
    assert_eq!(h.extractions(), 0, "nothing extracted again");
}

#[test]
fn deps_integrity_change_reads_only_changed_files() {
    let (h, tree) = harness();
    locked_tree(&tree, "sha512-a1", "1.0.0");
    open(&h, &tree, &["a", "c"]);
    // A new install of `a`: its `b.d.ts` changed; the lockfile says so.
    let new_b = "export declare const b: string;\n";
    tree.write("node_modules/a/b.d.ts", new_b.as_bytes());
    lockfile(&tree, "sha512-a2", "1.0.0", 0);
    let h = reopen(h);
    assert_eq!(deps_state(&h), TierState::Done);
    let read = reads(&h);
    assert!(
        read.contains(&"node_modules/a/b.d.ts".to_owned()),
        "{read:?}"
    );
    for path in &read {
        assert!(
            path == "node_modules/a/b.d.ts" || path == "node_modules/a/package.json",
            "{path} read although its stats vouch for it"
        );
    }
    assert!(dep_stored(&h, new_b));
    let t = table_of(&h, "a");
    let b = t
        .files
        .iter()
        .find(|f| f.path.ends_with("b.d.ts"))
        .expect("b");
    assert_eq!(b.oid, Some(h.git.oid(new_b.as_bytes())));
}

#[test]
fn deps_ctime_defeats_a_restored_mtime() {
    let (h, tree) = harness();
    locked_tree(&tree, "sha512-a1", "1.0.0");
    open(&h, &tree, &["a", "c"]);
    let path = h.git.root("main").join("node_modules/a/b.d.ts");
    let mtime = std::fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");
    // Same size, other content, the mtime set back: only the ctime moved.
    let new_b = "export declare const B: number;\n";
    assert_eq!(new_b.len(), "export declare const b: number;\n".len());
    std::fs::write(&path, new_b).expect("write");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_modified(mtime)
        .expect("set mtime");
    lockfile(&tree, "sha512-a2", "1.0.0", 0);
    let h = reopen(h);
    assert!(reads(&h).contains(&"node_modules/a/b.d.ts".to_owned()));
    assert!(dep_stored(&h, new_b));
}

#[test]
fn deps_racy_record_is_not_a_shortcut() {
    let (h, tree) = harness();
    locked_tree(&tree, "sha512-a1", "1.0.0");
    // The table is recorded right after the files were written.
    h.indexer.set_wall_clock(SystemTime::now);
    open(&h, &tree, &["a", "c"]);
    lockfile(&tree, "sha512-a2", "1.0.0", 0);
    let h = reopen(h);
    let read = reads(&h);
    for path in ["node_modules/a/index.d.ts", "node_modules/a/b.d.ts"] {
        assert!(read.contains(&path.to_owned()), "{path}: {read:?}");
    }
}

#[test]
fn deps_pnpm_lock_takes_the_fallback_identity() {
    let (h, tree) = harness();
    simple(
        &tree,
        "node_modules/a",
        "a",
        "1.0.0",
        "export declare const a: number;\n",
        &[],
    );
    tree.write("pnpm-lock.yaml", b"lockfileVersion: '9.0'\n");
    open(&h, &tree, &["a"]);
    let t = table_of(&h, "a");
    assert!(t.identity.starts_with(b"pkg:"));
    assert_eq!(t.origin.version.as_deref(), Some("1.0.0"));
    let h = reopen(h);
    assert_eq!(deps_state(&h), TierState::Done);
    // The fallback reads the installed manifest for its OID, nothing else.
    assert_eq!(reads(&h), ["node_modules/a/package.json"]);
}

#[test]
fn deps_lockfile_ahead_of_install_is_not_the_identity() {
    let (h, tree) = harness();
    locked_tree(&tree, "sha512-a1", "1.0.0");
    open(&h, &tree, &["a", "c"]);
    // `git pull`: the lockfile wants 1.0.1, the installed files are 1.0.0's.
    lockfile(&tree, "sha512-a2", "1.0.1", 0);
    let h = reopen(h);
    assert_eq!(deps_state(&h), TierState::Done);
    let read = reads(&h);
    assert!(read.iter().all(|p| p.ends_with("package.json")), "{read:?}");
    assert_eq!(h.extractions(), 0, "the installed files as they were");
    // `npm install`: new files, a new package.json (its inode).
    tree.remove("node_modules/a");
    let new_b = "export declare const b: boolean;\n";
    simple(
        &tree,
        "node_modules/a",
        "a",
        "1.0.1",
        "export * from \"./b\";\nexport declare const a: number;\n",
        &[("b.d.ts", new_b)],
    );
    let h = reopen(h);
    assert_eq!(deps_state(&h), TierState::Done);
    assert!(reads(&h).contains(&"node_modules/a/b.d.ts".to_owned()));
    assert!(dep_stored(&h, new_b), "the reinstalled content");
}

#[test]
fn deps_lockfile_read_is_streamed() {
    let (h, tree) = harness();
    simple(
        &tree,
        "node_modules/a",
        "a",
        "1.0.0",
        "export declare const a: number;\n",
        &[],
    );
    simple(
        &tree,
        "node_modules/c",
        "c",
        "3.0.0",
        "export declare const c: number;\n",
        &[],
    );
    lockfile(&tree, "sha512-a1", "1.0.0", 16 << 20);
    let len = std::fs::metadata(h.git.root("main").join("package-lock.json"))
        .expect("meta")
        .len();
    assert!(len > 16 << 20);
    crate::deps::lockfile::RANGES.with(|r| r.borrow_mut().clear());
    open(&h, &tree, &["a", "c"]);
    assert!(table_of(&h, "a").identity.starts_with(b"npm:sha512-a1"));
    let ranges = crate::deps::lockfile::RANGES.with(|r| r.borrow().clone());
    assert_eq!(ranges.first().map(|r| r.start), Some(0));
    assert_eq!(ranges.last().map(|r| r.end), Some(len));
    for w in ranges.windows(2) {
        assert_eq!(w[0].end, w[1].start, "end to end");
    }
    for r in &ranges {
        assert!(r.end - r.start <= crate::deps::lockfile::LOCKFILE_RANGE);
    }
}

#[test]
fn deps_closure_from_stored_specifiers() {
    let (h, tree) = harness();
    locked_tree(&tree, "sha512-a1", "1.0.0");
    open(&h, &tree, &["a", "c"]);
    let first: Vec<(RelPath, Option<BlobId>)> = table_of(&h, "a")
        .files
        .iter()
        .map(|f| (f.path.clone(), f.oid))
        .collect();
    // Another version and identity, the same content (its units stored).
    lockfile(&tree, "sha512-a9", "1.0.9", 0);
    let h = reopen(h);
    assert_eq!(deps_state(&h), TierState::Done);
    assert_eq!(h.extractions(), 0, "edges from cd_spec, nothing parsed");
    let t = table_of(&h, "a");
    assert_eq!(t.origin.version.as_deref(), Some("1.0.9"));
    let again: Vec<(RelPath, Option<BlobId>)> =
        t.files.iter().map(|f| (f.path.clone(), f.oid)).collect();
    assert_eq!(again, first);
}

#[test]
fn deps_table_without_units_is_repaired() {
    let (h, tree) = harness();
    locked_tree(&tree, "sha512-a1", "1.0.0");
    let b = "export declare const b: number;\n";
    open(&h, &tree, &["a", "c"]);
    assert!(dep_stored(&h, b));
    // `b.d.ts`'s unit evicted, the path table kept.
    let mut w = project(&h).deps.write();
    w.remove(h.git.oid(b.as_bytes()), &ts_version());
    assert_eq!(w.commit(), lectern_cache::Written::Committed);
    assert!(!dep_stored(&h, b));
    let h = reopen(h);
    assert_eq!(deps_state(&h), TierState::Done);
    assert!(dep_stored(&h, b), "repaired");
    // Step 3 finds it again.
    h.git
        .add("main", &[("src/use.ts", b"export const x = b;\n")]);
    h.git.commit("main");
    h.refresh_files();
    let r = h.goto("src/use.ts", "b", 0);
    assert!(
        r.candidates
            .iter()
            .any(|c| c.tier == CandidateTier::Dependency
                && &*c.definition.location.path == "node_modules/a/b.d.ts"),
        "{:?}",
        Harness::found(&r)
    );
}

#[test]
fn deps_reinstalled_root_is_reopened() {
    let (h, tree) = harness();
    locked_tree(&tree, "sha512-a1", "1.0.0");
    open(&h, &tree, &["a", "c"]);
    let old = lock(&project(&h).state).deps.roots.clone().expect("roots");
    // `npm ci`: `node_modules` deleted and written again.
    tree.remove("node_modules");
    let new_b = "export declare const b: bigint;\n";
    simple(
        &tree,
        "node_modules/a",
        "a",
        "1.0.0",
        "export * from \"./b\";\nexport declare const a: number;\n",
        &[("b.d.ts", new_b)],
    );
    simple(
        &tree,
        "node_modules/c",
        "c",
        "3.0.0",
        "export declare const c: number;\n",
        &[],
    );
    h.emit(&[("package-lock.json", CHANGED)]);
    h.settle();
    assert_eq!(deps_state(&h), TierState::Done);
    assert!(dep_stored(&h, new_b), "read through the reopened root");
    let roots = lock(&project(&h).state).deps.roots.clone().expect("roots");
    assert!(
        Arc::ptr_eq(&old, &roots),
        "the session's roots, revalidated"
    );
    assert!(
        log(&h)
            .iter()
            .any(|(op, p)| *op == "open" && p == "node_modules")
    );
}

#[test]
fn deps_roots_are_per_worktree() {
    let (h, tree) = harness();
    simple(
        &tree,
        "node_modules/a",
        "a",
        "1.0.0",
        "export declare const main_a: number;\n",
        &[],
    );
    open(&h, &tree, &["a"]);
    let main_roots = lock(&project(&h).state).deps.roots.clone().expect("roots");
    let feature = h.git.add_worktree("feature");
    let f = NpmTree::new(&feature);
    f.manifest("", &["a"], "");
    let feature_a = "export declare const feature_a: number;\n";
    simple(&f, "node_modules/a", "a", "1.0.0", feature_a, &[]);
    lock(&main_roots.log).clear();
    h.publish("feature", Vec::new());
    h.settle();
    assert_eq!(deps_state(&h), TierState::Done);
    let roots = lock(&project(&h).state).deps.roots.clone().expect("roots");
    assert!(
        !Arc::ptr_eq(&roots, &main_roots),
        "a new session, new roots"
    );
    assert!(lock(&main_roots.log).is_empty(), "main's roots not read");
    assert!(dep_stored(&h, feature_a));
    // A text of main's root is not one of this session's.
    let main_path = "node_modules/a/index.d.ts";
    assert!(
        roots
            .file_at(main_path, Some("node_modules"), Ecosystem::Npm)
            .is_some()
    );
    assert_eq!(
        lock(&roots.log)
            .iter()
            .filter(|(_, p)| p.contains("main_a"))
            .count(),
        0
    );
}

/// Runs `f` with the dependency tier's limits set to `limits` on this
/// thread (the manual executor's).
fn with_limits<R>(limits: Limits, f: impl FnOnce() -> R) -> R {
    crate::deps::LIMITS.with(|l| l.set(Some(limits)));
    let out = f();
    crate::deps::LIMITS.with(|l| l.set(None));
    out
}

fn chain_package(tree: &NpmTree, name: &str, files: usize, len: usize) {
    let mut decls = Vec::new();
    for i in 0..files {
        let next = format!("./f{}", i + 1);
        let imports: Vec<&str> = if i + 1 < files {
            vec![&next]
        } else {
            Vec::new()
        };
        decls.push((
            format!("f{i}.d.ts"),
            declarations(&format!("{name}_{i}"), &imports, len),
        ));
    }
    let refs: Vec<(&str, &str)> = decls
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    tree.package(
        &format!("node_modules/{name}"),
        name,
        "1.0.0",
        "\"types\": \"f0.d.ts\"",
        &refs,
    );
}

#[test]
fn deps_limits_per_package_and_file() {
    let (h, tree) = harness();
    chain_package(&tree, "many", 6, 200);
    chain_package(&tree, "heavy", 6, 1000);
    // A file over 1 MiB: `TooLarge`, its edges not followed.
    let big = declarations("big", &["./after"], (1 << 20) + 10);
    tree.package(
        "node_modules/big",
        "big",
        "1.0.0",
        "",
        &[
            ("index.d.ts", "export * from \"./huge\";\n"),
            ("huge.d.ts", &big),
            ("after.d.ts", "export declare const after: number;\n"),
        ],
    );
    with_limits(
        Limits {
            package_files: 3,
            package_bytes: 2500,
            session_bytes: 1 << 40,
        },
        || open(&h, &tree, &["many", "heavy", "big"]),
    );
    assert_eq!(deps_state(&h), TierState::Done);
    assert_eq!(table_of(&h, "many").files.len(), 3, "4000 files a package");
    let heavy = table_of(&h, "heavy");
    let bytes: u64 = heavy.files.iter().map(|f| f.stat.size).sum();
    assert!(
        bytes <= 2500 && heavy.files.len() == 2,
        "16 MiB a package: {bytes}"
    );
    let big = table_of(&h, "big");
    let huge = big
        .files
        .iter()
        .find(|f| f.path.ends_with("huge.d.ts"))
        .expect("huge");
    assert_eq!(huge.oid, None);
    assert_eq!(huge.status, ExtractStatus::TooLarge.code().unwrap());
    assert!(!big.files.iter().any(|f| f.path.ends_with("after.d.ts")));
    assert!(
        !reads(&h).iter().any(|p| p.ends_with("huge.d.ts")),
        "never read"
    );
}

#[test]
fn deps_limits_per_session_stop_the_tier() {
    let (h, tree) = harness();
    for name in ["s1", "s2", "s3"] {
        chain_package(&tree, name, 3, 1000);
    }
    with_limits(
        Limits {
            package_files: 4000,
            package_bytes: 16 << 20,
            session_bytes: 4000,
        },
        || open(&h, &tree, &["s1", "s2", "s3"]),
    );
    assert!(stopped(&h), "64 MiB a session");
    let read = {
        let p = project(&h);
        let st = lock(&p.state);
        st.deps.read
    };
    assert!(read <= 4000 + 2000, "{read}");
    assert_eq!(deps_state(&h), TierState::Done);
}

/// `code_intel.deps` with a 16 KiB quota.
static SMALL_DEPS: Tables = Tables {
    namespace: DEPS_NAMESPACE,
    schema: DEPS_SCHEMA,
    statements: DEPS_STATEMENTS,
    quota: 16 << 10,
};

#[test]
fn deps_quota_evicts_only_dependency_units() {
    let h = Harness::new();
    ignore(&h);
    *lock(&h.indexer.inner().deps_tables) = Some(&SMALL_DEPS);
    let tree = NpmTree::new(&h.git.root("main"));
    let sources: Vec<Vec<u8>> = (0..5)
        .map(|i| declarations(&format!("proj{i}"), &[], 2000).into_bytes())
        .collect();
    for (i, s) in sources.iter().enumerate() {
        h.git.add("main", &[(&format!("src/p{i}.ts"), s)]);
    }
    h.git.commit("main");
    chain_package(&tree, "wide", 40, 2000);
    open(&h, &tree, &["wide"]);
    assert!(stopped(&h), "its own write evicted dependency units");
    assert_eq!(deps_state(&h), TierState::Done);
    for s in &sources {
        assert!(h.stored(h.git.oid(s), "ts"), "project units untouched");
    }
}

/// A project using `useThing` from `lib`, also defined in the project.
fn usage_tree(h: &Harness, tree: &NpmTree) {
    h.git.add(
        "main",
        &[
            (
                "src/a.ts",
                b"import { useThing } from \"lib\";\nexport function run() {\n  useThing();\n}\n",
            ),
            ("src/thing.ts", b"export function useThing(): void {}\n"),
        ],
    );
    h.git.commit("main");
    simple(
        tree,
        "node_modules/lib",
        "lib",
        "4.5.6",
        "export declare function useThing(): void;\nexport declare function onlyInLib(): void;\n",
        &[],
    );
}

#[test]
fn deps_candidates_come_last() {
    let (h, tree) = harness();
    usage_tree(&h, &tree);
    open(&h, &tree, &["lib"]);
    let r = h.goto("src/a.ts", "useThing", 1);
    let tiers: Vec<CandidateTier> = r.candidates.iter().map(|c| c.tier).collect();
    assert_eq!(
        tiers.last(),
        Some(&CandidateTier::Dependency),
        "{:?}",
        Harness::found(&r)
    );
    assert!(
        tiers[..tiers.len() - 1]
            .iter()
            .all(|t| *t != CandidateTier::Dependency)
    );
    let dep = r.candidates.last().expect("a dependency candidate");
    assert_eq!(
        &*dep.definition.location.path,
        "node_modules/lib/index.d.ts"
    );
    assert_eq!(dep.evidence, Evidence::NameOnly);
    match &dep.definition.location.origin {
        TextOrigin::Dependency(o) => {
            assert_eq!(&*o.package, "lib");
            assert_eq!(o.version.as_deref(), Some("4.5.6"));
            assert_eq!(o.ecosystem, Ecosystem::Npm);
        }
        other => panic!("{other:?}"),
    }
    let l = &dep.definition.location;
    let bytes = std::fs::read(h.git.root("main").join(&*l.path)).expect("file");
    let at = l.byte.expect("a byte offset") as usize;
    assert!(bytes[at..].starts_with(b"useThing"));
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
}

#[test]
fn deps_not_in_names() {
    let (h, tree) = harness();
    usage_tree(&h, &tree);
    open(&h, &tree, &["lib"]);
    let _ = h.indexer.names(&h.key());
    h.settle();
    let table = h
        .events()
        .into_iter()
        .rev()
        .find_map(|e| match e {
            lectern_services::code_intel::CodeIntelEvent::Names { table, .. } => Some(table),
            _ => None,
        })
        .expect("a name table");
    let names: Vec<&str> = table.iter().collect();
    assert!(names.contains(&"useThing"));
    assert!(!names.contains(&"onlyInLib"), "{names:?}");
    let r = h.ask(QueryKind::Named {
        name: "onlyInLib".into(),
        near: None,
    });
    assert!(r.candidates.is_empty());
}

#[test]
fn deps_do_not_count_in_completeness() {
    let (h, tree) = harness();
    usage_tree(&h, &tree);
    tree.manifest("", &["lib"], "");
    h.open();
    // Tiers 3 and 4 done, the dependency tier not: a new dependency while
    // the background work is paused.
    h.pause();
    chain_package(&tree, "slow", 5, 400);
    tree.manifest("", &["lib", "slow"], "");
    h.emit(&[("package.json", CHANGED)]);
    h.settle();
    assert_eq!(deps_state(&h), TierState::Pending);
    let r = h.goto("src/a.ts", "useThing", 1);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
    h.resume();
    h.settle();
    assert_eq!(deps_state(&h), TierState::Done);
}

fn dep_text(path: &str, oid: BlobId, wt: &Harness) -> TextRef {
    TextRef {
        worktree: Some(wt.wt()),
        path: path.into(),
        oid,
        origin: TextOrigin::Dependency(DepOrigin {
            ecosystem: Ecosystem::Npm,
            package: "lib".into(),
            version: Some("4.5.6".into()),
            root: DepRoot::Worktree {
                dir: "node_modules".into(),
            },
        }),
    }
}

#[test]
fn deps_read_verify_changed_text_is_stale() {
    let (h, tree) = harness();
    usage_tree(&h, &tree);
    open(&h, &tree, &["lib"]);
    let path = "node_modules/lib/index.d.ts";
    let bytes = std::fs::read(h.git.root("main").join(path)).expect("file");
    let at = dep_text(path, h.git.oid(&bytes), &h);
    let position = super::support::pos_in(&bytes, "onlyInLib", 0);
    let r = h.ask(QueryKind::Definition {
        at: at.clone(),
        position,
    });
    assert_eq!(
        r.jump
            .map(|j| &*r.candidates[j as usize].definition.location.path),
        Some(path),
        "a dependency text answers"
    );
    tree.write(
        path,
        b"export declare function onlyInLib(x: number): void;\n",
    );
    // The kept parse answers for its OID without reading; without it the
    // text is read and checked.
    h.indexer.clear_kept_parses();
    let r = h.ask(QueryKind::Definition { at, position });
    assert_eq!(
        r.state,
        QueryState::Failed(lectern_services::code_intel::QueryError::Stale)
    );
    // Outside the roots, or another root: never read.
    let outside = TextRef {
        path: "src/a.ts".into(),
        ..dep_text("src/a.ts", h.git.oid(b"x"), &h)
    };
    let r = h.ask(QueryKind::Definition {
        at: outside,
        position,
    });
    assert_eq!(
        r.state,
        QueryState::Failed(lectern_services::code_intel::QueryError::Stale)
    );
}

#[test]
fn deps_current_dependency_text_is_tier_1() {
    let (h, tree) = harness();
    usage_tree(&h, &tree);
    // Only on demand: the tier does not reach it (not a direct dependency).
    simple(
        &tree,
        "node_modules/other",
        "other",
        "1.0.0",
        "export declare function viaFocus(): void;\n",
        &[],
    );
    open(&h, &tree, &["lib"]);
    let source = "export declare function viaFocus(): void;\n";
    assert!(!dep_stored(&h, source));
    // Its root is discovered: the text is read through it.
    h.set_focus(Focus::Current(Some(TextRef {
        ..dep_text(
            "node_modules/other/index.d.ts",
            h.git.oid(source.as_bytes()),
            &h,
        )
    })));
    h.settle();
    assert!(dep_stored(&h, source), "tier 1 wrote it to code_intel.deps");
}

/// A resolver whose TypeScript imports are one bare module name, `lib`,
/// bringing in `useThing` as `local`.
struct BareResolver {
    langs: Vec<LangName>,
}

impl Resolver for BareResolver {
    fn languages(&self) -> &[LangName] {
        &self.langs
    }

    fn imports(&self, _file: &ParsedFile, ctx: &ResolveContext<'_>) -> Vec<Import> {
        match ctx.dependency("lib", "") {
            Some(t) => vec![Import {
                target: ImportTarget::Dependency(t),
                names: ImportedNames::Named(vec![("useThing".into(), "useThing".into())]),
            }],
            None => Vec::new(),
        }
    }

    fn bind(
        &self,
        _site: &Site,
        _file: &ParsedFile,
        _ctx: &ResolveContext<'_>,
    ) -> Option<crate::resolve::Bound> {
        None
    }

    fn package_of(&self, _path: &str, _ctx: &ResolveContext<'_>) -> Option<PackageScope> {
        None
    }

    fn dependencies(&self, _subproject: &str, _ctx: &ResolveContext<'_>) -> Vec<RelPath> {
        Vec::new()
    }

    fn disambiguate(&self, _site: &Site, _candidates: &mut Vec<CandidateDraft>) {}
}

#[test]
fn deps_on_demand_import() {
    let (h, tree) = harness();
    usage_tree(&h, &tree);
    // `lib` is not a direct dependency: only the import finds it.
    open(&h, &tree, &[]);
    assert!(dep_paths(&h).is_empty());
    // The roots exist once the identity task ran.
    assert!(lock(&project(&h).state).deps.roots.is_some());
    h.indexer.set_resolver(Arc::new(BareResolver {
        langs: vec![lang_name("ts")],
    }));
    let r = h.goto("src/a.ts", "useThing", 1);
    let bound = r.jump.map(|j| &r.candidates[j as usize]).expect("a jump");
    assert_eq!(bound.evidence, Evidence::Bound(Binding::Import));
    assert_eq!(bound.tier, CandidateTier::Imported);
    assert_eq!(
        &*bound.definition.location.path,
        "node_modules/lib/index.d.ts"
    );
    assert!(matches!(
        bound.definition.location.origin,
        TextOrigin::Dependency(_)
    ));
    let lib =
        "export declare function useThing(): void;\nexport declare function onlyInLib(): void;\n";
    assert!(dep_stored(&h, lib), "written to code_intel.deps afterwards");
}

#[test]
fn deps_read_verify_changed_file_is_extracted_again() {
    // An indexed dependency file edited in place (the lockfile unchanged):
    // the jump reads it, extracts the new bytes on the spot, and its path
    // table row and the dependency map follow.
    let (h, tree) = harness();
    usage_tree(&h, &tree);
    open(&h, &tree, &["lib"]);
    let path = "node_modules/lib/index.d.ts";
    let old = table_of(&h, "lib").files[0].oid;
    let new = "export declare function other(): void;\nexport declare function useThing(n: number): void;\n";
    tree.write(path, new.as_bytes());
    h.indexer.set_resolver(Arc::new(BareResolver {
        langs: vec![lang_name("ts")],
    }));
    let r = h.goto("src/a.ts", "useThing", 1);
    let bound = r.jump.map(|j| &r.candidates[j as usize]).expect("a jump");
    assert_eq!(&*bound.definition.location.path, path);
    assert_eq!(bound.definition.location.line, 1, "the new bytes' line");
    h.settle();
    assert!(dep_stored(&h, new));
    let row = table_of(&h, "lib").files[0].clone();
    assert_eq!(row.oid, Some(h.git.oid(new.as_bytes())));
    assert_ne!(row.oid, old);
    let map = dep_map(&h);
    assert_eq!(map.paths(&h.git.oid(new.as_bytes())).len(), 1);
    assert!(old.is_none_or(|o| map.paths(&o).is_empty()));
}

#[test]
fn deps_closure_after_an_on_demand_entry() {
    let (h, tree) = harness();
    usage_tree(&h, &tree);
    // `lib` has more behind its entry.
    simple(
        &tree,
        "node_modules/lib",
        "lib",
        "4.5.6",
        "export * from \"./more\";\nexport declare function useThing(): void;\n",
        &[("more.d.ts", "export declare const more: number;\n")],
    );
    open(&h, &tree, &[]);
    h.indexer.set_resolver(Arc::new(BareResolver {
        langs: vec![lang_name("ts")],
    }));
    let _ = h.goto("src/a.ts", "useThing", 1);
    let entry = "export * from \"./more\";\nexport declare function useThing(): void;\n";
    assert!(dep_stored(&h, entry));
    let extracted = h.extractions();
    // Now a direct dependency: the walk takes the entry's edges from
    // `cd_spec` and goes on behind it.
    tree.manifest("", &["lib"], "");
    h.emit(&[("package.json", CHANGED)]);
    h.settle();
    assert_eq!(deps_state(&h), TierState::Done);
    assert_eq!(
        dep_paths(&h),
        ["node_modules/lib/index.d.ts", "node_modules/lib/more.d.ts"]
    );
    assert_eq!(h.extractions(), extracted + 1, "only more.d.ts");
}

#[test]
fn deps_yield_in_slots_like_tiers_4_and_5() {
    let (h, tree) = harness();
    let clock = lectern_kernel::testing::ManualClock::new();
    let kernel = h.host.kernel();
    kernel.set_clock(Arc::new(clock.clone()));
    chain_package(&tree, "slow", 4, 300);
    tree.manifest("", &["slow"], "");
    kernel.set_yield(true);
    h.open();
    // A slot at a time: not done in one settle.
    assert_eq!(deps_state(&h), TierState::Pending);
    let mut rounds = 0;
    while deps_state(&h) != TierState::Done {
        rounds += 1;
        assert!(rounds < 200, "the dependency tier never got its slots");
        clock.advance(Duration::from_millis(600));
        h.settle();
    }
    assert!(rounds > 3, "one item a slot: {rounds} rounds");
    assert_eq!(table_of(&h, "slow").files.len(), 4);
    kernel.set_yield(false);
}

#[test]
fn deps_map_is_compact() {
    // 20 000 files of 40 packages: the map's own buffers stay within ADR
    // 0033's 1 MB (`code_intel/deps_map_bytes` measures the live bytes).
    let tables: Vec<Arc<PathTable>> = (0..40).map(|p| Arc::new(npm_table(p, 500))).collect();
    let map = DepMap::build(1, &tables);
    assert_eq!(map.len(), 20_000);
    assert!(map.heap_bytes() < 1 << 20, "{}", map.heap_bytes());
    let t = &tables[3];
    let f = &t.files[7];
    let found = map.paths(&f.oid.expect("an OID"));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, &*f.path);
    assert_eq!(found[0].origin.package, t.origin.package);
    // Past the limit, files are left out.
    let more: Vec<Arc<PathTable>> = (0..41).map(|p| Arc::new(npm_table(p, 500))).collect();
    assert_eq!(DepMap::build(2, &more).len(), crate::deps::MAP_FILES);
}

#[test]
fn deps_ts_project_fixture_indexes() {
    let (h, tree) = harness();
    let names = ts_project(&tree, 3, 4, 2000, 4096);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    tree.manifest("", &refs, "");
    h.open();
    assert_eq!(deps_state(&h), TierState::Done);
    assert_eq!(dep_paths(&h).len(), 12);
}
