//! `goto_*`: jump to definition (「跳转到定义」「候选的分层与过滤」). The
//! steps stop at the first with binding evidence; only one candidate with
//! evidence jumps; the filters, tiers and order; results only grow; no
//! duplicates across steps; the resolution extension point's step 2; text
//! from the object database.

use std::sync::Arc;

use lectern_services::code_intel::{
    Binding, CandidateSource, CandidateTier, Completeness, DefinitionRef, Evidence, Focus,
    QueryError, QueryKind, QueryState, TextOrigin, TextRef,
};
use lectern_services::project::RelPath;

use crate::resolve::{
    Bound, CandidateDraft, Import, ImportedNames, PackageScope, ResolveContext, Resolver,
};
use crate::site::{ParsedFile, Site};
use crate::LangName;

use super::support::{Harness, packages};

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
        ("b.ts", "export function helper(x: number) {\n  return x;\n}\nexport const limit = 5;\n"),
    ];
    let cases = [
        Case {
            what: "a local binding: step 1 only",
            paused: false,
            expect: &[("a.ts", 2, Evidence::Bound(Binding::Local), CandidateSource::Scanned)],
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
        let want: Vec<_> = case.expect.iter().map(|(p, l, e, s)| (*p, *l, *e, *s)).collect();
        assert_eq!(got, want, "{}", case.what);
        assert_eq!(r.jump, case.jump, "{}", case.what);
        assert_eq!(r.preselect, Some(0), "{}", case.what);
        assert_eq!(done(&r), Completeness::Complete, "{}", case.what);
    }
}

#[test]
fn goto_one_bound_candidate_jumps_one_name_only_preselects() {
    let h = indexed(&[
        ("a.py", "def run():\n    total = 1\n    return total + other()\n"),
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
    let defines = own.symbol.as_ref().and_then(|s| s.defines.clone()).expect("defines");
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
                ("b.ts", "export function other() {}\nfunction hidden() { return 1; }\n"),
                ("c.ts", "export function hidden() { return 2; }\n"),
            ],
            from: ("a.ts", "hidden", 0),
            found: &["c.ts"],
        },
        Case {
            what: "go lower case is package-visible: the same directory only",
            files: &[
                ("p/a.go", "package p\n\nfunc Run() int { return helper() }\n"),
                ("p/b.go", "package p\n\nfunc helper() int { return 1 }\n"),
                ("q/c.go", "package q\n\nfunc helper() int { return 2 }\n"),
            ],
            from: ("p/a.go", "helper", 0),
            found: &["p/b.go"],
        },
        Case {
            what: "go Meters(value) is a conversion: a call reaches the type",
            files: &[
                ("u/a.go", "package u\n\nfunc Run(value int) Meters { return Meters(value) }\n"),
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
    assert_eq!(Harness::found(&r), [("src/a.rs".to_owned(), 0, Evidence::NameOnly)]);
}

/// The same bytes under `.java` and `.js` are one OID and two units: the
/// `java` unit only gives the `.java` path.
#[test]
fn goto_oid_language_match() {
    let both = "class Shared { }\n";
    let h = indexed(&[
        ("a.java", "class A { Shared make() { return new Shared(); } }\n"),
        ("x/Shared.java", both),
        ("x/shared.js", both),
    ]);
    assert_eq!(h.git.oid(both.as_bytes()), h.map().effective("x/shared.js").unwrap().oid);
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
            ("app/lib/deep/d.ts".to_owned(), CandidateTier::SameSubproject),
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
            ("app/lib/deep/d.ts".to_owned(), CandidateTier::SameSubproject),
            ("app/lib/c.ts".to_owned(), CandidateTier::SameSubproject),
        ]
    );
    assert!(r.candidates[2].in_review);
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
    h.git.write("main", "c.ts", b"export function item() { return 2; }\n");
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
    let r = h.indexer.results(id.lock().unwrap().unwrap()).expect("kept");
    let all = Harness::found(&r);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen, [all[..1].to_vec()], "the first batch, before the search");
    assert_eq!(
        all,
        [
            ("b.ts".to_owned(), 0, Evidence::NameOnly),
            ("c.ts".to_owned(), 0, Evidence::NameOnly),
        ]
    );
    for batch in &seen {
        assert_eq!(batch[..], all[..batch.len()], "a batch is a prefix: {seen:?}");
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
        ("a.ts", "export function run() { return local() + bound() + other(); }\n"),
        ("b.ts", "export function helper() { return 1; }\nexport function other() { return 2; }\n"),
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
        [("c.ts".to_owned(), 0, Evidence::Bound(Binding::QualifiedName))]
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
    h.git.add("main", &[("a.ts", b"export function run() { return 0; }\n")]);
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
