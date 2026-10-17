//! `lang_*`: extensions to languages, compatibility groups, packs.

use std::collections::HashMap;
use std::sync::Arc;

use crate::extract::{LanguageTable, compatible, language_for_path};
use crate::lang::{
    CORE_LANGUAGES, TREE_SITTER_TAGS_VERSION, TREE_SITTER_VERSION, core_grammar_version,
};
use crate::testing::{FakeGrammars, java_pack};
use crate::util::fnv64;
use crate::{LangName, NoGrammarPacks};

fn name(s: &str) -> LangName {
    LangName::new(s)
}

fn lang_of(path: &str) -> Option<String> {
    language_for_path(path, &NoGrammarPacks).map(|l| l.0.to_string())
}

#[test]
fn lang_extensions_map_to_core_languages() {
    let cases = [
        ("src/a.ts", "ts"),
        ("src/a.mts", "ts"),
        ("src/a.cts", "ts"),
        ("types/index.d.ts", "ts"),
        ("ui/Panel.tsx", "tsx"),
        ("a.js", "js"),
        ("a.mjs", "js"),
        ("a.cjs", "js"),
        ("App.jsx", "js"),
        (".eslintrc.js", "js"),
        ("crates/x/src/lib.rs", "rust"),
        ("src/main/java/A.java", "java"),
        ("tool.py", "python"),
        ("stubs/tool.pyi", "python"),
        ("cmd/main.go", "go"),
        ("LEGACY.JS", "js"),
    ];
    for (path, want) in cases {
        assert_eq!(lang_of(path).as_deref(), Some(want), "{path}");
    }
}

#[test]
fn lang_unclaimed_paths_have_no_language() {
    for path in [
        "Makefile",
        "README.md",
        ".gitignore",
        "dir.rs/file",
        "a.",
        "x/.ts",
    ] {
        assert_eq!(lang_of(path), None, "{path}");
    }
}

#[test]
fn lang_compatibility_groups() {
    let table = LanguageTable::core();
    let pairs = [
        ("ts", "tsx", true),
        ("ts", "js", true),
        ("tsx", "js", true),
        ("rust", "rust", true),
        ("go", "go", true),
        ("java", "java", true),
        ("rust", "go", false),
        ("python", "js", false),
        ("java", "ts", false),
        ("go", "unknown", false),
    ];
    for (a, b, want) in pairs {
        assert_eq!(compatible(&table, &name(a), &name(b)), want, "{a} {b}");
        assert_eq!(compatible(&table, &name(b), &name(a)), want, "{b} {a}");
    }
}

#[test]
fn lang_core_queries_compile() {
    let table = LanguageTable::core();
    let names: Vec<&str> = table.iter().map(|l| l.name().as_str()).collect();
    assert_eq!(names, ["ts", "tsx", "js", "rust", "java", "python", "go"]);
    for lang in table.iter() {
        assert!(lang.prepare(), "{}", lang.name());
    }
}

#[test]
fn lang_pack_claims_extension() {
    let packs = FakeGrammars::new();
    packs.install("kotlin", &["kt", "kts"], java_pack(7, Arc::new(())));
    assert_eq!(
        language_for_path("app/Main.kt", &packs),
        Some(name("kotlin"))
    );
    // Claimed but not in the table yet: no language to extract with.
    let core = LanguageTable::core();
    assert!(core.for_path("app/Main.kt", &packs).is_none());

    let grammar = packs.grammar(&name("kotlin")).expect("installed");
    let table = core.with_pack(name("kotlin"), grammar);
    let kotlin = table.for_path("app/Main.kt", &packs).expect("in the table");
    assert_eq!(kotlin.name(), &name("kotlin"));
    assert_eq!(kotlin.compat_group(), Some("jvm"));
    assert_eq!(kotlin.grammar_version(), u64::from_be_bytes([7; 8]));
    assert!(kotlin.prepare());
    // A pack registers its group: Kotlin joins Java's.
    assert!(compatible(&table, &name("kotlin"), &name("java")));
    assert!(!compatible(&core, &name("kotlin"), &name("java")));

    let removed = table.without("kotlin");
    assert!(removed.get("kotlin").is_none());
    assert!(removed.get("java").is_some());
}

use crate::grammar::GrammarSource as _;

#[test]
fn lang_core_wins_over_pack() {
    let packs = FakeGrammars::new();
    packs.install("rustish", &["rs", "rsx"], java_pack(1, Arc::new(())));
    assert_eq!(language_for_path("src/lib.rs", &packs), Some(name("rust")));
    assert_eq!(language_for_path("ui/a.rsx", &packs), Some(name("rustish")));

    // A pack named like a core language does not replace it, and removing
    // a core language does nothing.
    let core = LanguageTable::core();
    let rust_version = core.get("rust").expect("core").grammar_version();
    let table = core.with_pack(name("rust"), java_pack(2, Arc::new(())));
    assert_eq!(
        table.get("rust").expect("core").grammar_version(),
        rust_version
    );
    assert!(table.without("rust").get("rust").is_some());
}

#[test]
fn lang_pack_versions_follow_artifact() {
    let core = LanguageTable::core();
    let a = core.with_pack(name("kotlin"), java_pack(1, Arc::new(())));
    let b = a.with_pack(name("kotlin"), java_pack(2, Arc::new(())));
    let (a, b) = (a.get("kotlin").unwrap(), b.get("kotlin").unwrap());
    assert_ne!(a.grammar_version(), b.grammar_version());
    // Same queries: the same query version.
    assert_eq!(a.query_version(), b.query_version());
}

#[test]
fn lang_lease_lives_with_the_language() {
    let module: Arc<dyn std::any::Any + Send + Sync> = Arc::new(());
    let table = LanguageTable::core().with_pack(name("kotlin"), java_pack(3, module.clone()));
    assert_eq!(Arc::strong_count(&module), 2);
    let lang = table.get("kotlin").unwrap().clone();
    drop(table);
    assert_eq!(Arc::strong_count(&module), 2);
    drop(lang);
    assert_eq!(Arc::strong_count(&module), 1);
}

/// `name` → `version` of every package in the workspace's Cargo.lock.
fn locked_versions() -> HashMap<String, Vec<String>> {
    let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
        .expect("Cargo.lock");
    let mut versions: HashMap<String, Vec<String>> = HashMap::new();
    let mut current: Option<String> = None;
    for line in lock.lines() {
        if let Some(n) = line.strip_prefix("name = ") {
            current = Some(n.trim_matches('"').to_owned());
        } else if let Some(v) = line.strip_prefix("version = ")
            && let Some(n) = current.take()
        {
            versions
                .entry(n)
                .or_default()
                .push(v.trim_matches('"').to_owned());
        } else if line.starts_with("[[package]]") {
            current = None;
        }
    }
    versions
}

#[test]
fn lang_grammar_versions_match_lock() {
    let locked = locked_versions();
    let check = |krate: &str, version: &str| {
        assert_eq!(
            locked.get(krate).map(Vec::as_slice),
            Some(&[version.to_owned()][..]),
            "{krate} in Cargo.lock"
        );
    };
    check("tree-sitter", TREE_SITTER_VERSION);
    check("tree-sitter-tags", TREE_SITTER_TAGS_VERSION);
    for spec in CORE_LANGUAGES {
        check(spec.grammar_crate, spec.grammar_crate_version);
        assert_eq!(
            core_grammar_version(spec),
            fnv64(&[format!("{}@{}", spec.grammar_crate, spec.grammar_crate_version).as_bytes()]),
            "{}",
            spec.name
        );
    }
    // Languages from different crates never share a grammar version.
    let table = LanguageTable::core();
    for a in CORE_LANGUAGES {
        for b in CORE_LANGUAGES {
            let (va, vb) = (
                table.get(a.name).unwrap().grammar_version(),
                table.get(b.name).unwrap().grammar_version(),
            );
            assert_eq!(
                va == vb,
                a.grammar_crate == b.grammar_crate,
                "{} {}",
                a.name,
                b.name
            );
        }
    }
}
