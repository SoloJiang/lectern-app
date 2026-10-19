//! `refs_*`: find references and callers (「查找引用与调用方」). A local's
//! references are only in its scope; a root definition's are found through
//! `ci_ref` and the search; what the locals or a resolver bind back to the
//! definition is a caller, what they bind elsewhere is dropped, the rest is
//! same-name.

use std::sync::Arc;

use lectern_services::code_intel::{
    Binding, Completeness, DefinitionRef, QueryKind, QueryResults, QueryState,
};
use lectern_services::project::RelPath;

use crate::LangName;
use crate::index::lock;
use crate::query::Step;
use crate::resolve::{Bound, CandidateDraft, Import, PackageScope, ResolveContext, Resolver};
use crate::site::{ParsedFile, Site};

use super::goto::{indexed, unindexed};
use super::support::{CREATED, Harness, parsed};

/// The definition named at the `nth` `needle` of `path`.
fn def(h: &Harness, path: &str, needle: &str, nth: usize) -> DefinitionRef {
    let r = h.goto(path, needle, nth);
    r.symbol
        .as_ref()
        .and_then(|s| s.defines.clone())
        .unwrap_or_else(|| panic!("{needle} #{nth} in {path} is no definition: {r:?}"))
}

fn refs(h: &Harness, of: &DefinitionRef) -> Arc<QueryResults> {
    h.ask(QueryKind::References { of: of.clone() })
}

/// (path, line) of each reference.
fn at(list: &[lectern_services::code_intel::Reference]) -> Vec<(String, u32)> {
    list.iter()
        .map(|r| (r.location.path.to_string(), r.location.line))
        .collect()
}

/// A local's references are the uses in its scope (positions that only
/// `@local.reference` has): no `ci_ref`, no same-name, nothing from another
/// scope or file.
#[test]
fn refs_local_variable_stays_in_its_scope() {
    let h = indexed(&[
        (
            "a.ts",
            "export function run(items: number[]) {\n  let total = 0;\n  for (const x of items) {\n    total += x;\n  }\n  return total;\n}\nexport function other() {\n  const total = 5;\n  return total;\n}\n",
        ),
        (
            "b.ts",
            "export const total = 1;\nexport function use() { return total; }\n",
        ),
    ]);
    let of = def(&h, "a.ts", "total", 0);
    assert!(of.local);
    let r = refs(&h, &of);
    assert_eq!(
        at(&r.callers),
        [("a.ts".to_owned(), 3), ("a.ts".to_owned(), 5)]
    );
    assert!(r.same_name.is_empty(), "{:?}", r.same_name);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
    let enclosing: Vec<_> = r.callers.iter().map(|c| c.enclosing.as_deref()).collect();
    assert_eq!(enclosing, [Some("run"), Some("run")]);
}

/// A top-level function the locals also record (a program-scope local):
/// its uses in the same file bind to it and are callers.
#[test]
fn refs_toplevel_function_same_file_callers() {
    let h = indexed(&[(
        "a.js",
        "function helper() { return 1; }\nfunction run() {\n  return helper() + helper();\n}\n",
    )]);
    let of = def(&h, "a.js", "helper", 0);
    assert!(!of.local, "root scope: not a local");
    let r = refs(&h, &of);
    assert_eq!(
        at(&r.callers),
        [("a.js".to_owned(), 2), ("a.js".to_owned(), 2)]
    );
    assert!(r.same_name.is_empty());
}

/// An exported top-level function, captured by tags and locals: `local` is
/// false; another file's use is found through `ci_ref`, an unindexed file's
/// through the search. Without a resolver both are same-name.
#[test]
fn refs_toplevel_captured_by_locals() {
    let h = indexed(&[
        ("a.js", "export function helper() { return 1; }\n"),
        (
            "b.js",
            "import { helper } from './a.js';\nexport const x = helper();\n",
        ),
    ]);
    h.pause();
    h.git.write(
        "main",
        "c.js",
        b"import { helper } from './a.js';\n\nhelper();\n",
    );
    h.emit(&[("c.js", CREATED)]);
    h.settle();
    assert!(!h.hinted("c.js"));
    let of = def(&h, "a.js", "helper", 0);
    assert!(!of.local);
    let r = refs(&h, &of);
    assert!(r.callers.is_empty(), "{:?}", r.callers);
    // The imports name it too.
    assert_eq!(
        at(&r.same_name),
        [
            ("b.js".to_owned(), 0),
            ("b.js".to_owned(), 1),
            ("c.js".to_owned(), 0),
            ("c.js".to_owned(), 2)
        ]
    );
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
}

/// `const helper = () => …` inside a function: both tags and locals capture
/// it, `local` is true, and its references are only in the outer function.
#[test]
fn refs_nested_helper_is_local() {
    let h = indexed(&[
        (
            "a.js",
            "export function outer() {\n  const helper = () => 1;\n  return helper() + helper();\n}\n",
        ),
        (
            "b.js",
            "export function helper() { return 2; }\nhelper();\n",
        ),
    ]);
    let of = def(&h, "a.js", "helper", 0);
    assert!(of.local);
    let r = refs(&h, &of);
    assert_eq!(
        at(&r.callers),
        [("a.js".to_owned(), 2), ("a.js".to_owned(), 2)]
    );
    assert!(
        r.same_name.is_empty(),
        "nothing from b.js: {:?}",
        r.same_name
    );
}

/// The same nested `helper` found by `Named` (⌘K): the index does not store
/// locality, so the candidate's `local` is false (not known). Find
/// references knows it from the definition's own file, whatever the
/// `DefinitionRef` says: the one parse of that file gives the locality and
/// the references; nothing else is parsed, the scope is the outer function.
#[test]
fn refs_nested_helper_from_named_is_local() {
    let files = [
        (
            "a.js",
            "export function outer() {\n  const helper = () => 1;\n  return helper() + helper();\n}\n",
        ),
        (
            "b.js",
            "export function helper() { return 2; }\nhelper();\n",
        ),
    ];
    let h = indexed(&files);
    parsed(&files);
    let r = h.ask(QueryKind::Named {
        name: "helper".into(),
        near: None,
    });
    let local: Vec<(String, bool)> = r
        .candidates
        .iter()
        .map(|c| {
            let d = &c.definition;
            (d.location.path.to_string(), d.local)
        })
        .collect();
    assert_eq!(
        local,
        [("a.js".to_owned(), false), ("b.js".to_owned(), false)],
        "{local:?}"
    );
    assert_eq!(parsed(&files), Vec::<String>::new());
    let mut of = r
        .candidates
        .iter()
        .find(|c| &*c.definition.location.path == "a.js")
        .expect("a.js")
        .definition
        .clone();
    for given in [false, true] {
        of.local = given;
        let r = refs(&h, &of);
        assert_eq!(parsed(&files), ["a.js"], "given {given}");
        assert_eq!(
            at(&r.callers),
            [("a.js".to_owned(), 2), ("a.js".to_owned(), 2)],
            "given {given}"
        );
        assert!(r.same_name.is_empty(), "given {given}: {:?}", r.same_name);
        assert_eq!(r.state, QueryState::Done(Completeness::Complete));
        let defines = r.symbol.as_ref().and_then(|s| s.defines.as_ref());
        assert!(defines.is_some_and(|d| d.local), "given {given}");
    }
    // A root definition said to be local is searched across files: its own
    // file's parse says it is not.
    let mut of = r
        .candidates
        .iter()
        .find(|c| &*c.definition.location.path == "b.js")
        .expect("b.js")
        .definition
        .clone();
    of.local = true;
    let r = refs(&h, &of);
    let defines = r.symbol.as_ref().and_then(|s| s.defines.as_ref());
    assert!(defines.is_some_and(|d| !d.local));
    // Parsed once; a.js's uses are bound to its own local, which `ci_ref`
    // does not list, so no other file is a candidate.
    assert_eq!(parsed(&files), ["b.js"]);
}

/// `Named` lists what the index has: however many of its definitions are
/// `File` (each could be local), no file is parsed before the candidates
/// go out, nor after.
#[test]
fn refs_named_parses_no_file() {
    let files: Vec<(String, String)> = (0..40)
        .map(|i| {
            (
                format!("m{i}.js"),
                format!(
                    "export function outer{i}() {{\n  const helper = () => {i};\n  return helper();\n}}\n"
                ),
            )
        })
        .collect();
    let pairs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let h = indexed(&pairs);
    parsed(&pairs);
    let r = h.ask(QueryKind::Named {
        name: "helper".into(),
        near: Some("m0.js".into()),
    });
    assert_eq!(r.candidates.len(), 40);
    assert!(r.candidates.iter().all(|c| !c.definition.local));
    assert!(matches!(r.state, QueryState::Done(_)), "{:?}", r.state);
    assert_eq!(parsed(&pairs), Vec::<String>::new());
}

/// The latest results of the newest query (in a step hook).
fn latest(inner: &crate::index::Inner) -> Option<Arc<QueryResults>> {
    lock(&inner.queries)
        .entries
        .back()
        .map(|e| e.results.clone())
}

type Seen = Arc<std::sync::Mutex<Option<Arc<QueryResults>>>>;

/// B2: what the definition's file and the `ci_ref` pass found goes out
/// before the search's scope is read: two files (fewer than the first
/// batch's four) and no time passed, published all the same.
#[test]
fn refs_indexed_references_go_out_before_the_search() {
    let h = indexed(&[
        ("c.ts", "export function item() { return 2; }\n"),
        ("a.ts", "export function run() { return item(); }\n"),
    ]);
    let of = def(&h, "c.ts", "item", 0);
    let seen: Seen = Arc::default();
    let s = seen.clone();
    *lock(&h.indexer.inner().step_hook) = Some((
        Step::Hints,
        Box::new(move |inner, _| *s.lock().unwrap() = latest(inner)),
    ));
    let r = refs(&h, &of);
    assert_eq!(at(&r.same_name), [("a.ts".to_owned(), 0)]);
    let before = seen.lock().unwrap().clone().expect("the hook ran");
    assert_eq!(before.state, QueryState::Running);
    assert_eq!(at(&before.same_name), [("a.ts".to_owned(), 0)]);
}

/// B2: the search's batches go out on their timers between hits too: a
/// hit in the first file, then 200 ms of files without one (past the first
/// batch's 150 ms), and the hit is out before the next file is read.
#[test]
fn refs_search_batches_go_out_on_time() {
    let mut files = vec![
        (
            "c.ts".to_owned(),
            "export function item() { return 2; }\n".to_owned(),
        ),
        (
            "a.ts".to_owned(),
            "export function run() { return item(); }\n".to_owned(),
        ),
    ];
    for i in 0..4 {
        files.push((format!("b{i}.ts"), format!("export const b{i} = {i};\n")));
    }
    let pairs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let h = unindexed(&pairs, &[]);
    let of = def(&h, "c.ts", "item", 0);
    let seen: Seen = Arc::default();
    let (s, clock) = (seen.clone(), h.clock.clone());
    // Before a.ts is read: arm the next hook. Before b0.ts: 200 ms pass.
    // Before b1.ts: what is published.
    let third: crate::query::HookFn = Box::new(move |inner, _| *s.lock().unwrap() = latest(inner));
    let second: crate::query::HookFn = Box::new(move |inner, _| {
        clock.advance(std::time::Duration::from_millis(200));
        *lock(&inner.step_hook) = Some((Step::Read, third));
    });
    *lock(&h.indexer.inner().step_hook) = Some((
        Step::Read,
        Box::new(move |inner, _| *lock(&inner.step_hook) = Some((Step::Read, second))),
    ));
    let r = refs(&h, &of);
    assert_eq!(at(&r.same_name), [("a.ts".to_owned(), 0)]);
    let before = seen.lock().unwrap().clone().expect("the hooks ran");
    assert_eq!(before.state, QueryState::Running);
    assert_eq!(at(&before.same_name), [("a.ts".to_owned(), 0)]);
}

/// A use bound by the locals to another (shadowing) local is dropped.
#[test]
fn refs_shadowed_uses_are_dropped() {
    let h = indexed(&[
        ("a.ts", "export function helper() { return 1; }\n"),
        (
            "b.ts",
            "export function use() {\n  const helper = 1;\n  return helper;\n}\n",
        ),
        (
            "c.ts",
            "import { helper } from './a';\nexport const y = helper();\n",
        ),
    ]);
    let of = def(&h, "a.ts", "helper", 0);
    let r = refs(&h, &of);
    // b.ts's uses are its own local's; c.ts's import and call remain.
    assert_eq!(
        at(&r.same_name),
        [("c.ts".to_owned(), 0), ("c.ts".to_owned(), 1)]
    );
    assert!(r.callers.is_empty());
}

/// One position reached twice (the own file is also a `ci_ref` unit, here
/// through a member use) is listed once.
#[test]
fn refs_no_duplicates_across_sources() {
    let h = indexed(&[(
        "a.py",
        "def helper():\n    return 1\n\ndef run(obj):\n    return obj.helper()\n",
    )]);
    let of = def(&h, "a.py", "helper", 0);
    let r = refs(&h, &of);
    assert_eq!(at(&r.same_name), [("a.py".to_owned(), 4)]);
    assert!(r.callers.is_empty());
}

/// A resolver whose `bind` sends `b.ts`'s use back to `a.ts`'s definition.
struct BindBack {
    langs: Vec<LangName>,
    to: std::sync::Mutex<Option<DefinitionRef>>,
}

impl Resolver for BindBack {
    fn languages(&self) -> &[LangName] {
        &self.langs
    }

    fn imports(&self, _file: &ParsedFile, _ctx: &ResolveContext<'_>) -> Vec<Import> {
        Vec::new()
    }

    fn bind(&self, _site: &Site, _file: &ParsedFile, _ctx: &ResolveContext<'_>) -> Option<Bound> {
        Some(Bound {
            definition: self.to.lock().unwrap().clone()?,
            binding: Binding::Import,
        })
    }

    fn package_of(&self, _path: &str, _ctx: &ResolveContext<'_>) -> Option<PackageScope> {
        None
    }

    fn dependencies(&self, _subproject: &str, _ctx: &ResolveContext<'_>) -> Vec<RelPath> {
        Vec::new()
    }

    fn disambiguate(&self, _site: &Site, _candidates: &mut Vec<CandidateDraft>) {}
}

/// The extension point's `bind` makes a cross-file use a caller.
#[test]
fn refs_resolver_bind_makes_callers() {
    let h = indexed(&[
        ("a.ts", "export function helper() { return 1; }\n"),
        (
            "b.ts",
            "import { helper } from './a';\nexport const y = helper();\n",
        ),
    ]);
    let of = def(&h, "a.ts", "helper", 0);
    let resolver = Arc::new(BindBack {
        langs: vec![crate::lang_name("ts")],
        to: std::sync::Mutex::new(Some(of.clone())),
    });
    h.indexer.set_resolver(resolver);
    let r = refs(&h, &of);
    assert_eq!(
        at(&r.callers),
        [("b.ts".to_owned(), 0), ("b.ts".to_owned(), 1)]
    );
    assert!(r.same_name.is_empty());
}

/// References of a name nothing indexed uses yet: the search finds them in
/// the sub-project, in batches that only grow.
#[test]
fn refs_unindexed_files_through_the_search() {
    let h = unindexed(
        &[
            ("a.ts", "export function helper() { return 1; }\n"),
            (
                "b.ts",
                "import { helper } from './a';\nexport const y = helper();\n",
            ),
            ("c.ts", "export const z = 2;\n"),
        ],
        &[],
    );
    let of = def(&h, "a.ts", "helper", 0);
    let r = refs(&h, &of);
    assert_eq!(
        at(&r.same_name),
        [("b.ts".to_owned(), 0), ("b.ts".to_owned(), 1)]
    );
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
}
