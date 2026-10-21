//! `goto_*`: jump to definition (「跳转到定义」「候选的分层与过滤」). The
//! steps stop at the first with binding evidence; only one candidate with
//! evidence jumps; the filters, tiers and order; results only grow; no
//! duplicates across steps; the resolution extension point's step 2; text
//! from the object database.

use std::collections::HashSet;
use std::sync::Arc;

use lectern_services::code_intel::{
    Binding, CandidateSource, CandidateTier, Completeness, DefinitionRef, Evidence, Focus,
    QueryError, QueryKind, QueryResults, QueryState, TextOrigin, TextRef,
};
use lectern_services::project::RelPath;

use crate::LangName;
use crate::index::lock;
use crate::resolve::{
    Bound, CandidateDraft, Import, ImportedNames, PackageScope, ResolveContext, Resolver,
};
use crate::site::{ParsedFile, Site};

use super::support::{Harness, packages, parsed};

/// A harness with `files` committed and indexed.
pub(crate) fn indexed(files: &[(&str, &str)]) -> Harness {
    let h = Harness::new();
    for (path, text) in files {
        h.git.add("main", &[(path, text.as_bytes())]);
    }
    h.git.commit("main");
    h.open();
    h
}

/// The same, with the background paused before opening: nothing indexed.
pub(crate) fn unindexed(files: &[(&str, &str)], dirs: &[&str]) -> Harness {
    let h = Harness::new();
    for (path, text) in files {
        h.git.add("main", &[(path, text.as_bytes())]);
    }
    h.git.commit("main");
    h.pause();
    h.open_with("main", packages(dirs));
    h
}

fn done(r: &lectern_services::code_intel::QueryResults) -> Completeness {
    match &r.state {
        QueryState::Done(c) => *c,
        other => panic!("not done: {other:?}"),
    }
}

/// Each step answers only when the steps before it found nothing with
/// binding evidence: the local binding hides the index; the index answers
/// when the file has nothing; the search answers when neither has.
#[test]
fn goto_steps_stop_at_the_first_with_evidence() {
    struct Case {
        what: &'static str,
        paused: bool,
        expect: &'static [(&'static str, u32, Evidence, CandidateSource)],
        jump: Option<u32>,
    }
    let files: &[(&str, &str)] = &[
        (
            "a.ts",
            "import { helper } from './b';\nexport function run() {\n  const limit = 3;\n  return helper(limit);\n}\n",
        ),
        (
            "b.ts",
            "export function helper(x: number) {\n  return x;\n}\nexport const limit = 5;\n",
        ),
    ];
    let cases = [
        Case {
            what: "a local binding: step 1 only",
            paused: false,
            expect: &[(
                "a.ts",
                2,
                Evidence::Bound(Binding::Local),
                CandidateSource::Scanned,
            )],
            jump: Some(0),
        },
        Case {
            what: "nothing local: the index",
            paused: false,
            expect: &[("b.ts", 0, Evidence::NameOnly, CandidateSource::Index)],
            jump: None,
        },
        Case {
            what: "nothing indexed: the search",
            paused: true,
            expect: &[("b.ts", 0, Evidence::NameOnly, CandidateSource::Scanned)],
            jump: None,
        },
    ];
    for (i, case) in cases.iter().enumerate() {
        let h = if case.paused {
            unindexed(files, &[])
        } else {
            indexed(files)
        };
        let r = if i == 0 {
            h.goto("a.ts", "limit", 1)
        } else {
            h.goto("a.ts", "helper", 1)
        };
        let got: Vec<_> = r
            .candidates
            .iter()
            .map(|c| {
                (
                    &*c.definition.location.path,
                    c.definition.location.line,
                    c.evidence,
                    c.source,
                )
            })
            .collect();
        let want: Vec<_> = case
            .expect
            .iter()
            .map(|(p, l, e, s)| (*p, *l, *e, *s))
            .collect();
        assert_eq!(got, want, "{}", case.what);
        assert_eq!(r.jump, case.jump, "{}", case.what);
        assert_eq!(r.preselect, Some(0), "{}", case.what);
        assert_eq!(done(&r), Completeness::Complete, "{}", case.what);
    }
}

#[test]
fn goto_one_bound_candidate_jumps_one_name_only_preselects() {
    let h = indexed(&[
        (
            "a.py",
            "def run():\n    total = 1\n    return total + other()\n",
        ),
        ("b.py", "def other():\n    return 2\n"),
    ]);
    let local = h.goto("a.py", "total", 1);
    assert_eq!(local.jump, Some(0));
    assert_eq!(local.preselect, Some(0));
    let symbol = local.symbol.as_ref().expect("a symbol");
    assert_eq!(&*symbol.name, "total");
    assert!(symbol.defines.is_none());
    let elsewhere = h.goto("a.py", "other", 0);
    assert_eq!(
        Harness::found(&elsewhere),
        [("b.py".to_owned(), 0, Evidence::NameOnly)]
    );
    assert_eq!(elsewhere.jump, None, "one NameOnly candidate never jumps");
    assert_eq!(elsewhere.preselect, Some(0));
    // On a definition's own name: it is the only candidate, `defines` set.
    let own = h.goto("b.py", "other", 0);
    assert_eq!(own.jump, Some(0));
    let defines = own
        .symbol
        .as_ref()
        .and_then(|s| s.defines.clone())
        .expect("defines");
    assert_eq!(&*defines.name, "other");
    assert!(!defines.local);
}

/// The filters of「候选的分层与过滤」, one row each: the language group,
/// visibility, syntax kind.
#[test]
fn goto_filters_group_visibility_and_kind() {
    struct Case {
        what: &'static str,
        files: &'static [(&'static str, &'static str)],
        from: (&'static str, &'static str, usize),
        found: &'static [&'static str],
    }
    let cases = [
        Case {
            what: "ts finds js in its group, not python",
            files: &[
                ("app/a.ts", "export function run() { return shared(); }\n"),
                ("app/b.js", "function shared() { return 1; }\n"),
                ("app/c.py", "def shared():\n    return 1\n"),
            ],
            from: ("app/a.ts", "shared", 0),
            found: &["app/b.js"],
        },
        Case {
            what: "a module's own definitions are file-visible",
            files: &[
                ("a.ts", "export function run() { return hidden(); }\n"),
                (
                    "b.ts",
                    "export function other() {}\nfunction hidden() { return 1; }\n",
                ),
                ("c.ts", "export function hidden() { return 2; }\n"),
            ],
            from: ("a.ts", "hidden", 0),
            found: &["c.ts"],
        },
        Case {
            what: "go lower case is package-visible: the same directory only",
            files: &[
                (
                    "p/a.go",
                    "package p\n\nfunc Run() int { return helper() }\n",
                ),
                ("p/b.go", "package p\n\nfunc helper() int { return 1 }\n"),
                ("q/c.go", "package q\n\nfunc helper() int { return 2 }\n"),
            ],
            from: ("p/a.go", "helper", 0),
            found: &["p/b.go"],
        },
        Case {
            what: "go Meters(value) is a conversion: a call reaches the type",
            files: &[
                (
                    "u/a.go",
                    "package u\n\nfunc Run(value int) Meters { return Meters(value) }\n",
                ),
                ("u/b.go", "package u\n\ntype Meters int\n"),
            ],
            from: ("u/a.go", "Meters", 1),
            found: &["u/b.go"],
        },
        Case {
            what: "rust module visibility: the module's children only",
            files: &[
                ("src/a.rs", "fn helper() -> u8 { 1 }\n"),
                ("src/a/b.rs", "pub fn run() -> u8 { helper() }\n"),
                ("src/c.rs", "pub fn run() -> u8 { helper() }\n"),
            ],
            from: ("src/c.rs", "helper", 0),
            found: &[],
        },
        Case {
            what: "rust: a call never names a trait",
            files: &[
                ("src/a.rs", "pub fn run() -> u8 { Shape() }\n"),
                ("src/b.rs", "pub trait Shape {}\n"),
                ("src/c.rs", "pub fn Shape() -> u8 { 1 }\n"),
            ],
            from: ("src/a.rs", "Shape", 0),
            found: &["src/c.rs"],
        },
    ];
    for case in cases {
        let h = indexed(case.files);
        let (path, needle, nth) = case.from;
        let r = h.goto(path, needle, nth);
        let found: Vec<&str> = r
            .candidates
            .iter()
            .map(|c| &*c.definition.location.path)
            .collect();
        assert_eq!(found, case.found, "{}", case.what);
    }
    // The module's child sees it.
    let h = indexed(&[
        ("src/a.rs", "fn helper() -> u8 { 1 }\n"),
        ("src/a/b.rs", "pub fn run() -> u8 { helper() }\n"),
    ]);
    let r = h.goto("src/a/b.rs", "helper", 0);
    assert_eq!(
        Harness::found(&r),
        [("src/a.rs".to_owned(), 0, Evidence::NameOnly)]
    );
}

/// The same bytes under `.java` and `.js` are one OID and two units: the
/// `java` unit only gives the `.java` path.
#[test]
fn goto_oid_language_match() {
    let both = "class Shared { }\n";
    let h = indexed(&[
        (
            "a.java",
            "class A { Shared make() { return new Shared(); } }\n",
        ),
        ("x/Shared.java", both),
        ("x/shared.js", both),
    ]);
    assert_eq!(
        h.git.oid(both.as_bytes()),
        h.map().effective("x/shared.js").unwrap().oid
    );
    let r = h.goto("a.java", "Shared", 1);
    let paths: Vec<&str> = r
        .candidates
        .iter()
        .map(|c| &*c.definition.location.path)
        .collect();
    assert_eq!(paths, ["x/Shared.java"]);
    assert_eq!(&*r.candidates[0].definition.lang.0, "java");
}

/// Tiers in order, and within a tier the review set, path distance, path,
/// line.
#[test]
fn goto_tiers_and_order() {
    let def = "export function item() { return 1; }\n";
    let h = Harness::new();
    let files = [
        ("app/src/a.ts", "export function run() { return item(); }\n"),
        ("app/src/z.ts", def),
        ("app/src/b.ts", def),
        ("app/lib/c.ts", def),
        ("app/lib/deep/d.ts", def),
        ("other/e.ts", def),
    ];
    for (p, t) in files {
        // Distinct blobs, so every path is one unit of its own.
        let text = format!("{t}// {p}\n");
        h.git.add("main", &[(p, text.as_bytes())]);
    }
    h.git.commit("main");
    h.open_with("main", packages(&["app", "other"]));
    let tiers = |r: &lectern_services::code_intel::QueryResults| -> Vec<(String, CandidateTier)> {
        r.candidates
            .iter()
            .map(|c| (c.definition.location.path.to_string(), c.tier))
            .collect()
    };
    let r = h.goto("app/src/a.ts", "item", 0);
    assert_eq!(
        tiers(&r),
        [
            ("app/src/b.ts".to_owned(), CandidateTier::SamePackage),
            ("app/src/z.ts".to_owned(), CandidateTier::SamePackage),
            ("app/lib/c.ts".to_owned(), CandidateTier::SameSubproject),
            (
                "app/lib/deep/d.ts".to_owned(),
                CandidateTier::SameSubproject
            ),
            ("other/e.ts".to_owned(), CandidateTier::Elsewhere),
        ]
    );
    // The review set goes first within its tier.
    let wt = h.wt();
    h.indexer.set_focus(
        &h.key(),
        Some(&wt),
        Focus::Review(Arc::from(vec![RelPath::from("app/lib/deep/d.ts")])),
    );
    h.settle();
    let r = h.goto("app/src/a.ts", "item", 0);
    assert_eq!(
        tiers(&r)[2..4],
        [
            (
                "app/lib/deep/d.ts".to_owned(),
                CandidateTier::SameSubproject
            ),
            ("app/lib/c.ts".to_owned(), CandidateTier::SameSubproject),
        ]
    );
    assert!(r.candidates[2].in_review);
}

/// A tier holds only when what it compares with is known. `Named` has no
/// current text: a definition at the `near` path is not `CurrentFile` (nor
/// `Imported`), only the nearest. Files outside every sub-project of a
/// project that has some are not in the same sub-project.
#[test]
fn goto_tiers_need_what_they_compare_with() {
    let def = "export function item() { return 1; }\n";
    let h = Harness::new();
    let files = [
        ("app/a.ts", def),
        ("app/b.ts", def),
        (
            "tools/top.ts",
            "export function call() { return item(); }\n",
        ),
        ("scripts/root.ts", def),
    ];
    for (p, t) in files {
        let text = format!("{t}// {p}\n");
        h.git.add("main", &[(p, text.as_bytes())]);
    }
    h.git.commit("main");
    h.open_with("main", packages(&["app"]));
    let tiers = |r: &lectern_services::code_intel::QueryResults| -> Vec<(String, CandidateTier)> {
        r.candidates
            .iter()
            .map(|c| (c.definition.location.path.to_string(), c.tier))
            .collect()
    };
    let r = h.ask(QueryKind::Named {
        name: "item".into(),
        near: Some("app/a.ts".into()),
    });
    assert_eq!(
        tiers(&r),
        [
            ("app/a.ts".to_owned(), CandidateTier::SamePackage),
            ("app/b.ts".to_owned(), CandidateTier::SamePackage),
            ("scripts/root.ts".to_owned(), CandidateTier::Elsewhere),
        ]
    );
    // From a file no sub-project holds: another such file (in another
    // directory) is not in the same sub-project.
    let r = h.goto("tools/top.ts", "item", 0);
    assert_eq!(
        tiers(&r),
        [
            ("app/a.ts".to_owned(), CandidateTier::Elsewhere),
            ("app/b.ts".to_owned(), CandidateTier::Elsewhere),
            ("scripts/root.ts".to_owned(), CandidateTier::Elsewhere),
        ]
    );
}

/// Results only grow: a later batch appends; the first batch keeps its
/// indices.
#[test]
fn goto_results_only_grow() {
    let h = Harness::new();
    h.git.add(
        "main",
        &[
            ("a.ts", b"export function run() { return item(); }\n"),
            ("b.ts", b"export function item() { return 1; }\n"),
        ],
    );
    h.git.commit("main");
    h.open();
    // A file the index does not have yet: only the search finds it.
    h.pause();
    h.git
        .write("main", "c.ts", b"export function item() { return 2; }\n");
    h.emit(&[("c.ts", super::support::CREATED)]);
    h.settle();
    // What the query had published when the search extracted `c.ts`.
    let id = Arc::new(std::sync::Mutex::new(None));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (inner, i, s) = (h.indexer.inner().clone(), id.clone(), seen.clone());
    h.set_hook(move |_, _| {
        if let Some(id) = *i.lock().unwrap()
            && let Some(r) = inner.results(id)
        {
            s.lock().unwrap().push(Harness::found(&r));
        }
    });
    let at = h.text("a.ts");
    let position = h.pos("a.ts", "item", 0);
    *id.lock().unwrap() = Some(h.indexer.query(lectern_services::code_intel::Query {
        project: h.key(),
        kind: QueryKind::Definition { at, position },
    }));
    h.settle();
    let r = h
        .indexer
        .results(id.lock().unwrap().unwrap())
        .expect("kept");
    let all = Harness::found(&r);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        [all[..1].to_vec()],
        "the first batch, before the search"
    );
    assert_eq!(
        all,
        [
            ("b.ts".to_owned(), 0, Evidence::NameOnly),
            ("c.ts".to_owned(), 0, Evidence::NameOnly),
        ]
    );
    for batch in &seen {
        assert_eq!(
            batch[..],
            all[..batch.len()],
            "a batch is a prefix: {seen:?}"
        );
    }
    assert_eq!(r.candidates[1].source, CandidateSource::Scanned);
}

/// The current text's definitions listed by step 1 are not listed again by
/// the index when the text is indexed.
#[test]
fn goto_no_duplicates_current_file() {
    let h = indexed(&[(
        "a.ts",
        "class A {\n  helper() { return 1; }\n  run() { return this.helper(); }\n}\n",
    )]);
    let r = h.goto("a.ts", "helper", 1);
    assert_eq!(
        Harness::found(&r),
        [("a.ts".to_owned(), 1, Evidence::NameOnly)]
    );
    assert_eq!(r.candidates[0].tier, CandidateTier::CurrentFile);
}

/// A resolver for the tests of step 2: `a.ts` imports `b.ts`'s `helper`
/// under the local name `local`, and `bind` says `bound` is `c.ts`'s line 0.
struct TestResolver {
    langs: Vec<LangName>,
}

impl TestResolver {
    fn new() -> Arc<TestResolver> {
        Arc::new(TestResolver {
            langs: vec![crate::lang_name("ts")],
        })
    }
}

impl Resolver for TestResolver {
    fn languages(&self) -> &[LangName] {
        &self.langs
    }

    fn imports(&self, _file: &ParsedFile, _ctx: &ResolveContext<'_>) -> Vec<Import> {
        vec![Import {
            target: "b.ts".into(),
            names: ImportedNames::Named(vec![
                ("local".into(), "helper".into()),
                // `b.ts` exports no `missing`: step 2 reads it and finds
                // nothing.
                ("other".into(), "missing".into()),
            ]),
        }]
    }

    fn bind(&self, site: &Site, _file: &ParsedFile, ctx: &ResolveContext<'_>) -> Option<Bound> {
        if &*site.name != "bound" {
            return None;
        }
        let eff = ctx.map.effective("c.ts")?;
        Some(Bound {
            definition: DefinitionRef {
                location: lectern_services::code_intel::Location {
                    worktree: None,
                    path: eff.path,
                    origin: TextOrigin::Worktree,
                    oid: eff.oid,
                    line: 0,
                    column: 16,
                    byte: Some(16),
                    end_line: 0,
                },
                name: "bound".into(),
                kind: crate::DefKind::Function,
                lang: crate::lang_name("ts"),
                scope: None,
                local: false,
            },
            binding: Binding::QualifiedName,
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

/// Step 2: an import's target's exported definition is `Bound(Import)` and
/// jumps; `bind` gives its own binding; the target read for step 2 is not
/// extracted again by the search, and what it gave is not listed twice.
#[test]
fn goto_resolver_step_two() {
    let files: &[(&str, &str)] = &[
        (
            "a.ts",
            "export function run() { return local() + bound() + other(); }\n",
        ),
        (
            "b.ts",
            "export function helper() { return 1; }\nexport function other() { return 2; }\n",
        ),
        ("c.ts", "export function bound() { return 3; }\n"),
        ("d.ts", "export function other() { return 4; }\n"),
    ];
    // `other`'s import names nothing `b.ts` exports: step 2 extracted
    // `b.ts`, the search finds `b.ts` and `d.ts`, each once, `b.ts` from
    // step 2's extraction.
    let h = unindexed(files, &[]);
    h.indexer.set_resolver(TestResolver::new());
    let b = h.git.oid(files[1].1.as_bytes());
    let r = h.goto("a.ts", "other", 0);
    let paths: Vec<String> = Harness::found(&r).into_iter().map(|(p, _, _)| p).collect();
    assert_eq!(paths, ["b.ts", "d.ts"]);
    assert_eq!(r.jump, None);
    let extracted_b = h.extracted().iter().filter(|o| **o == b).count();
    assert_eq!(extracted_b, 1, "b.ts extracted once for one query");

    let h = unindexed(files, &[]);
    h.indexer.set_resolver(TestResolver::new());
    let r = h.goto("a.ts", "local", 0);
    assert_eq!(
        Harness::found(&r),
        [("b.ts".to_owned(), 0, Evidence::Bound(Binding::Import))]
    );
    assert_eq!(r.candidates[0].tier, CandidateTier::Imported);
    assert_eq!(r.candidates[0].source, CandidateSource::Resolver);
    assert_eq!(r.jump, Some(0));

    let r = h.goto("a.ts", "bound", 0);
    assert_eq!(
        Harness::found(&r),
        [(
            "c.ts".to_owned(),
            0,
            Evidence::Bound(Binding::QualifiedName)
        )]
    );
    assert_eq!(r.jump, Some(0));
}

#[test]
fn goto_no_symbol_and_unsupported_language() {
    let h = indexed(&[
        ("a.ts", "export function run() {\n  return 1 + 2;\n}\n"),
        ("notes.txt", "run\n"),
    ]);
    let r = h.goto("a.ts", "+", 0);
    assert_eq!(r.state, QueryState::Failed(QueryError::NoSymbol));
    let r = h.goto("notes.txt", "run", 0);
    assert_eq!(r.state, QueryState::Failed(QueryError::UnsupportedLanguage));
    // A project not open: `NotOpen` at once, no task.
    let id = h.indexer.query(lectern_services::code_intel::Query {
        project: lectern_services::project::ProjectKey::Folder {
            root: Arc::from(std::path::Path::new("/nowhere")),
        },
        kind: QueryKind::Named {
            name: "run".into(),
            near: None,
        },
    });
    let r = h.indexer.results(id).expect("kept");
    assert_eq!(r.state, QueryState::Failed(QueryError::NotOpen));
    assert_eq!(r.generation, 0);
}

/// Text from the object database (a diff's old side): read by OID, never
/// from the worktree; its own candidates and the references from them have
/// `origin` `Object`.
#[test]
fn goto_object_origin_reads_the_blob() {
    let old = "export function run() {\n  const limit = 1;\n  return limit;\n}\n";
    let h = indexed(&[("a.ts", old)]);
    // The worktree moved on; the old blob stays in the object database.
    h.git.add(
        "main",
        &[("a.ts", b"export function run() { return 0; }\n")],
    );
    h.git.commit("main");
    h.refresh_files();
    let at = TextRef {
        worktree: Some(h.wt()),
        path: "a.ts".into(),
        oid: h.git.oid(old.as_bytes()),
        origin: TextOrigin::Object,
    };
    let blobs = h.git.count("blob");
    let position = super::support::pos_in(old.as_bytes(), "limit", 1);
    let r = h.ask(QueryKind::Definition { at, position });
    assert_eq!(h.git.count("blob"), blobs + 1);
    assert_eq!(r.jump, Some(0));
    let def = r.candidates[0].definition.clone();
    assert_eq!(def.location.origin, TextOrigin::Object);
    assert_eq!(def.location.oid, h.git.oid(old.as_bytes()));
    // References from it read the same blob, not the worktree.
    let r = h.ask(QueryKind::References { of: def });
    assert_eq!(h.git.count("blob"), blobs + 2);
    let callers: Vec<(TextOrigin, u32)> = r
        .callers
        .iter()
        .map(|c| (c.location.origin, c.location.line))
        .collect();
    assert_eq!(callers, [(TextOrigin::Object, 2)]);
}

/// Definitions of `get` in [`popular`]: more than two pages (`NAME_PAGE`).
const POPULAR: usize = 601;

/// A name more than two pages of units define (`NAME_PAGE`): 600 files of
/// sub-project `aa` define `get`, then `zz/near.ts` (indexed last, so its
/// unit comes after the first page's), and `zz/main.ts` calls it. The
/// background is paused afterwards.
fn popular() -> Harness {
    let h = Harness::new();
    for i in 0..POPULAR - 1 {
        let text = format!("export function get() {{ return {i}; }}\n");
        h.git
            .add("main", &[(&format!("aa/f{i:03}.ts"), text.as_bytes())]);
    }
    h.git.add(
        "main",
        &[("zz/main.ts", b"export function run() { return get(); }\n")],
    );
    h.git.commit("main");
    h.open_with("main", packages(&["aa", "zz"]));
    h.git.add(
        "main",
        &[("zz/near.ts", b"export function get() { return -1; }\n")],
    );
    h.git.commit("main");
    h.refresh_files();
    h.pause();
    h
}

/// The results of the query's first batch: read at its second publish.
fn first_batch(h: &Harness) -> Arc<std::sync::Mutex<Option<Arc<QueryResults>>>> {
    let seen: Arc<std::sync::Mutex<Option<Arc<QueryResults>>>> = Arc::default();
    let s = seen.clone();
    let second: crate::query::HookFn = Box::new(move |inner, _| {
        let q = lock(&inner.queries);
        *s.lock().unwrap() = q.entries.back().map(|e| e.results.clone());
    });
    *lock(&h.indexer.inner().step_hook) = Some((
        crate::query::Step::Publish,
        Box::new(move |inner, _| {
            *lock(&inner.step_hook) = Some((crate::query::Step::Publish, second));
        }),
    ));
    seen
}

/// A3: the first batch of a jump to a name more than a page of units
/// define reads one page and the definitions in the files nearest the jump
/// (its directory and sub-project), so the nearest candidate leads it
/// whatever the name's count; the other pages follow in later batches,
/// every definition once, complete.
#[test]
fn goto_popular_name_nearest_first() {
    let h = popular();
    let seen = first_batch(&h);
    let r = h.goto("zz/main.ts", "get", 0);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
    assert_eq!(r.candidates.len(), POPULAR);
    let paths: HashSet<&str> = r
        .candidates
        .iter()
        .map(|c| &*c.definition.location.path)
        .collect();
    assert_eq!(paths.len(), POPULAR, "each once");
    let first = seen.lock().unwrap().clone().expect("a later batch");
    assert!(
        first.candidates.len() < POPULAR,
        "{}",
        first.candidates.len()
    );
    assert_eq!(&*first.candidates[0].definition.location.path, "zz/near.ts");
    assert_eq!(first.candidates[0].tier, CandidateTier::SamePackage);
    assert_eq!(first.state, QueryState::Running);
    // `Named` from the same file: the same first batch.
    let seen = first_batch(&h);
    let r = h.ask(QueryKind::Named {
        name: "get".into(),
        near: Some("zz/main.ts".into()),
    });
    assert_eq!(r.candidates.len(), POPULAR);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
    let first = seen.lock().unwrap().clone().expect("a later batch");
    assert!(first.candidates.len() < POPULAR);
    assert_eq!(&*first.candidates[0].definition.location.path, "zz/near.ts");
}

/// A3, the near files in the ranker's order: a sub-project of many
/// directories whose files define `get` and come first in path order, more
/// than `NEAR_FILES` of them, and in the jump's own directory a committed
/// sibling and a new file of the worktree (an overlay entry): both
/// siblings are in the first batch, ahead of every other candidate, for the
/// jump and for `Named`.
#[test]
fn goto_popular_name_nearest_in_a_deep_subproject() {
    let far = crate::query::NEAR_FILES + 52;
    let h = Harness::new();
    for i in 0..far {
        let text = format!("export function get() {{ return {i}; }}\n");
        h.git.add(
            "main",
            &[(&format!("web/a/d{:02}/f{i:04}.ts", i % 40), text.as_bytes())],
        );
    }
    h.git.add(
        "main",
        &[
            (
                "web/src/main.ts",
                b"export function run() { return get(); }\n",
            ),
            (
                "web/src/store.ts",
                b"export function get() { return -1; }\n",
            ),
        ],
    );
    h.git.commit("main");
    h.open_with("main", packages(&["web"]));
    h.git.write(
        "main",
        "web/src/fresh.ts",
        b"export function get() { return -2; }\n",
    );
    h.emit(&[("web/src/fresh.ts", super::support::CREATED)]);
    h.settle();
    h.pause();
    let nearest = ["web/src/fresh.ts", "web/src/store.ts"];
    let check = |first: &QueryResults| {
        let lead: Vec<&str> = first.candidates[..2]
            .iter()
            .map(|c| &*c.definition.location.path)
            .collect();
        assert_eq!(lead, nearest);
        assert!(first.candidates.len() < far + 2, "a first batch");
    };
    let seen = first_batch(&h);
    let r = h.goto("web/src/main.ts", "get", 0);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
    assert_eq!(r.candidates.len(), far + 2);
    check(&seen.lock().unwrap().clone().expect("a later batch"));
    let seen = first_batch(&h);
    let r = h.ask(QueryKind::Named {
        name: "get".into(),
        near: Some("web/src/main.ts".into()),
    });
    assert_eq!(r.candidates.len(), far + 2);
    check(&seen.lock().unwrap().clone().expect("a later batch"));
}

/// A3: the later pages stop at the query's time box; the definitions not
/// read count as unsearched (at most that many files), never complete.
#[test]
fn goto_popular_name_pages_are_time_boxed() {
    let h = popular();
    let clock = h.clock.clone();
    let second: crate::query::HookFn =
        Box::new(move |_, _| clock.advance(std::time::Duration::from_secs(2)));
    *lock(&h.indexer.inner().step_hook) = Some((
        crate::query::Step::Publish,
        Box::new(move |inner, _| {
            *lock(&inner.step_hook) = Some((crate::query::Step::Publish, second));
        }),
    ));
    let r = h.goto("zz/main.ts", "get", 0);
    assert!(r.candidates.len() < POPULAR, "{}", r.candidates.len());
    match &r.state {
        QueryState::Done(Completeness::Partial { unsearched, .. }) => {
            assert!(
                *unsearched as usize >= POPULAR - r.candidates.len(),
                "{unsearched}"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// A2: the text that becomes the current one is parsed in the background
/// and kept; a jump in it then reads and parses nothing, at any position,
/// and answers as the cold jump does (a parse for its site).
#[test]
fn goto_kept_parse_answers_as_the_cold_one() {
    let a = "import { other } from './b';\nexport function run(x: number) {\n  const local = x + 1;\n  return helper(local) + other();\n}\nfunction helper(y: number) { return y; }\n";
    let b =
        "export function other() { return 2; }\nexport function helper(z: number) { return z; }\n";
    let h = indexed(&[("a.ts", a), ("b.ts", b)]);
    let texts = [("a.ts", a), ("b.ts", b)];
    let sites = [
        ("local", 1),
        ("helper", 0),
        ("other", 1),
        ("run", 0),
        ("x", 1),
    ];
    // Cold: nothing kept, each jump parses its site.
    h.indexer
        .inner()
        .no_warm
        .store(true, std::sync::atomic::Ordering::Release);
    let cold: Vec<_> = sites.iter().map(|(n, i)| h.goto("a.ts", n, *i)).collect();
    assert_eq!(h.indexer.kept_parses().0, 0);
    h.indexer
        .inner()
        .no_warm
        .store(false, std::sync::atomic::Ordering::Release);
    h.current("a.ts");
    h.settle();
    assert_eq!(h.indexer.kept_parses().0, 1, "warmed in the background");
    parsed(&texts);
    for ((n, i), cold) in sites.iter().zip(&cold) {
        let warm = h.goto("a.ts", n, *i);
        assert_eq!(warm.symbol, cold.symbol, "{n} #{i}");
        assert_eq!(warm.candidates, cold.candidates, "{n} #{i}");
        assert_eq!(
            (warm.jump, warm.preselect),
            (cold.jump, cold.preselect),
            "{n} #{i}"
        );
        assert_eq!(warm.state, cold.state, "{n} #{i}");
    }
    assert_eq!(parsed(&texts), Vec::<String>::new(), "no parse");
    // Another content (the file changed): not the kept one, parsed.
    let a2 = a.replace("x + 1", "x + 2");
    h.git.write("main", "a.ts", a2.as_bytes());
    h.emit(&[("a.ts", super::support::CHANGED)]);
    h.settle();
    let texts = [("a2", a2.as_str())];
    parsed(&texts);
    let r = h.goto("a.ts", "helper", 0);
    assert!(!r.candidates.is_empty());
    assert_eq!(parsed(&texts).first().map(String::as_str), Some("a2"));
}

/// A2: the bounds of the kept parses: at most `PARSE_CACHE_ENTRIES` texts
/// (the least recently used goes), none over `PARSE_CACHE_MAX_SOURCE`, and
/// none kept past `PARSE_CACHE_KEEP_FOR` unused (a due entry drops them).
#[test]
fn goto_kept_parses_are_bounded() {
    use crate::query::{PARSE_CACHE_ENTRIES, PARSE_CACHE_KEEP_FOR, PARSE_CACHE_MAX_SOURCE};
    let mut files: Vec<(String, String)> = (0..PARSE_CACHE_ENTRIES + 1)
        .map(|i| {
            (
                format!("f{i}.ts"),
                format!("export function f{i}() {{ return {i}; }}\n"),
            )
        })
        .collect();
    let mut big = "export function big() { return 1; }\n".to_owned();
    while big.len() <= PARSE_CACHE_MAX_SOURCE {
        big.push_str("// padding padding padding padding padding padding padding\n");
    }
    files.push(("big.ts".to_owned(), big));
    let pairs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let h = indexed(&pairs);
    for (path, _) in &files[..PARSE_CACHE_ENTRIES + 1] {
        h.current(path);
        h.settle();
    }
    let (kept, bytes) = h.indexer.kept_parses();
    assert_eq!(kept, PARSE_CACHE_ENTRIES);
    assert!(bytes <= crate::query::PARSE_CACHE_BYTES);
    // The first one went: its jump parses again.
    let texts: Vec<(&str, &str)> = pairs.clone();
    parsed(&texts);
    h.goto("f0.ts", "f0", 0);
    assert_eq!(parsed(&texts).first().map(String::as_str), Some("f0.ts"));
    // Over the size: never kept.
    h.indexer.clear_kept_parses();
    h.current("big.ts");
    h.settle();
    assert_eq!(h.indexer.kept_parses().0, 0);
    // Unused for long once no longer current: dropped by the due entry.
    h.current("f1.ts");
    h.settle();
    assert_eq!(h.indexer.kept_parses().0, 1);
    h.set_focus(Focus::Current(None));
    h.advance_and_fire(PARSE_CACHE_KEEP_FOR + std::time::Duration::from_millis(1));
    assert_eq!(h.indexer.kept_parses().0, 0);
}

/// A2: the current text's parse stays while it is current, however long
/// the reader takes: its jumps never take the cold path. The text before it
/// gets its `PARSE_CACHE_KEEP_FOR` from when it stopped being current, and
/// the current one too once its project has none or closes.
#[test]
fn goto_current_text_parse_stays_while_current() {
    use crate::query::PARSE_CACHE_KEEP_FOR;
    use std::time::Duration;
    let files = [
        (
            "a.ts",
            "export function a() { return 1; }\nexport const x = a();\n",
        ),
        (
            "b.ts",
            "export function b() { return 2; }\nexport const y = b();\n",
        ),
    ];
    let h = indexed(&files);
    h.current("a.ts");
    h.settle();
    // Read for five minutes, then jump: no parse.
    for _ in 0..5 {
        h.advance_and_fire(PARSE_CACHE_KEEP_FOR);
    }
    assert_eq!(h.indexer.kept_parses().0, 1);
    parsed(&files);
    h.goto("a.ts", "a", 1);
    assert!(parsed(&files).is_empty(), "the kept parse answered");
    // b.ts becomes current: a.ts is kept for its own 60 s, then dropped.
    h.current("b.ts");
    h.settle();
    assert_eq!(h.indexer.kept_parses().0, 2);
    h.clock
        .advance(PARSE_CACHE_KEEP_FOR - Duration::from_secs(1));
    h.settle();
    assert_eq!(h.indexer.kept_parses().0, 2);
    h.advance_and_fire(Duration::from_secs(2));
    assert_eq!(h.indexer.kept_parses().0, 1);
    parsed(&files);
    h.goto("b.ts", "b", 1);
    assert!(parsed(&files).is_empty(), "b.ts is still kept");
    // No current text: 60 s later nothing is kept.
    h.set_focus(Focus::Current(None));
    h.advance_and_fire(PARSE_CACHE_KEEP_FOR + Duration::from_millis(1));
    assert_eq!(h.indexer.kept_parses().0, 0);
    // A current text whose project closes is released the same way.
    h.current("a.ts");
    h.settle();
    assert_eq!(h.indexer.kept_parses().0, 1);
    h.indexer
        .project_event(&lectern_services::project::ProjectEvent::Closed { project: h.key() });
    h.advance_and_fire(PARSE_CACHE_KEEP_FOR + Duration::from_millis(1));
    assert_eq!(h.indexer.kept_parses().0, 0);
}

/// A jump from an object's text whose path has other content in the
/// worktree: the index's candidate there (a nested `File` definition) goes
/// out as the index has it, `local` not known (false); the one file parsed
/// is the jump's own text, not the worktree's file.
#[test]
fn goto_object_side_parses_only_its_text() {
    let old = "export function run() {\n  return helper();\n}\n";
    let new = "export function run() {\n  const helper = () => 1;\n  return helper();\n}\n";
    let h = indexed(&[("a.ts", old)]);
    h.git.add("main", &[("a.ts", new.as_bytes())]);
    h.git.commit("main");
    h.refresh_files();
    let at = TextRef {
        worktree: Some(h.wt()),
        path: "a.ts".into(),
        oid: h.git.oid(old.as_bytes()),
        origin: TextOrigin::Object,
    };
    let position = super::support::pos_in(old.as_bytes(), "helper", 0);
    let texts = [("object", old), ("worktree", new)];
    parsed(&texts);
    let r = h.ask(QueryKind::Definition {
        at: at.clone(),
        position,
    });
    let found: Vec<(String, TextOrigin, u32, bool)> = r
        .candidates
        .iter()
        .map(|c| {
            let l = &c.definition.location;
            (l.path.to_string(), l.origin, l.line, c.definition.local)
        })
        .collect();
    assert_eq!(
        found,
        [("a.ts".to_owned(), TextOrigin::Worktree, 1, false)],
        "{r:?}"
    );
    // The jump's own parse of its text, then the whole parse kept for the
    // next jumps in it (in the background): never the worktree's file.
    assert_eq!(parsed(&texts), ["object", "object"]);
    let again = h.ask(QueryKind::Definition { at, position });
    assert_eq!(again.candidates, r.candidates);
    assert_eq!(parsed(&texts), Vec::<String>::new(), "kept: no parse");
}

/// Once a project's map is installed, `ParsedFile`'s query for every
/// language of the map is compiled by a background task, so the first
/// jump in a language does not compile it on the query's thread; a
/// language the map has no file in is not compiled.
#[test]
fn goto_site_queries_compiled_after_open() {
    let h = unindexed(
        &[
            ("a.ts", "export function one() { return two(); }\n"),
            ("b.rs", "pub fn two() -> u32 { 2 }\n"),
        ],
        &[],
    );
    h.settle();
    let map = h.map();
    let names: Vec<String> = map
        .languages()
        .iter()
        .map(|l| l.name().0.to_string())
        .collect();
    assert_eq!(names, ["ts", "rust"]);
    for lang in map.languages() {
        assert!(lang.site_query_ready(), "{}", lang.name().0);
    }
    let go = map.lang("go").expect("a core language");
    assert!(!go.site_query_ready(), "no Go file: not compiled");
}

/// Two projects reconcile while the compiling task is pending (the
/// plugin activating with several projects open): the second's request
/// finds the task submitted, and its language is compiled all the same.
#[test]
fn goto_site_queries_of_every_open_project() {
    use lectern_services::project::{
        ActiveTree, FileKind, FileList, ProjectKey, ProjectSnapshot, Scan,
    };

    let h = Harness::new();
    h.git.add(
        "main",
        &[("a.ts", b"export function one() { return 1; }\n")],
    );
    h.git.commit("main");
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical");
    std::fs::write(root.join("b.py"), b"def two():\n    return 2\n").expect("write");
    let folder = ProjectKey::Folder {
        root: Arc::from(root.as_path()),
    };
    let mut files = FileList::builder();
    files.push("b.py", FileKind::File);
    // Both projects' events before anything runs: both reconciles are
    // queued before the compiling task the first one submits.
    h.publish("main", Vec::new());
    let event = h.projects.set(ProjectSnapshot {
        key: folder.clone(),
        generation: 1,
        name: "folder".into(),
        worktrees: Arc::from(Vec::new()),
        active: Some(ActiveTree {
            root: Arc::from(root.as_path()),
            worktree: None,
            opened_at: "".into(),
            files: Scan::Ready(Arc::new(files.build())),
            packages: Scan::Ready(Arc::from(Vec::new())),
        }),
    });
    h.indexer.project_event(&event);
    h.settle();
    let repo = h.map();
    let other = h.indexer.map(&folder).expect("open");
    let ts = repo.lang("ts").expect("ts");
    let python = other.lang("python").expect("python");
    assert!(repo.languages().iter().any(|l| l.name().0 == ts.name().0));
    assert!(
        other
            .languages()
            .iter()
            .any(|l| l.name().0 == python.name().0)
    );
    assert!(ts.site_query_ready(), "the first project's language");
    assert!(python.site_query_ready(), "the second project's language");
    assert!(h.indexer.inner().cold_languages().is_empty());
    assert!(
        !h.indexer
            .inner()
            .warming
            .load(std::sync::atomic::Ordering::Acquire),
        "the task ended"
    );
}
