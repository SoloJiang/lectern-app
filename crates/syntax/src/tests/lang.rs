//! `lang_*`: the language table, extensions, versions and queries.

use std::collections::HashMap;
use std::sync::Arc;

use crate::lang::{
    CORE_LANGUAGES, Capture, TREE_SITTER_VERSION, classify, core_grammar_version, query_version,
};
use crate::util::fnv64;
use crate::{Languages, Rules, SPANS_VERSION};

#[test]
fn lang_names_and_order() {
    let langs = Languages::core();
    let names: Vec<&str> = langs.iter().map(|l| l.name()).collect();
    assert_eq!(
        names,
        [
            "ts", "tsx", "js", "rust", "java", "python", "go", "json", "yaml", "toml", "markdown"
        ]
    );
    assert!(Arc::ptr_eq(&langs, &Languages::core()));
}

#[test]
fn lang_extensions() {
    let langs = Languages::core();
    let cases = [
        ("src/main.ts", Some("ts")),
        ("a.mts", Some("ts")),
        ("a.cts", Some("ts")),
        ("ui/App.tsx", Some("tsx")),
        ("index.js", Some("js")),
        ("a.mjs", Some("js")),
        ("a.cjs", Some("js")),
        ("a.jsx", Some("js")),
        ("crates/syntax/src/lib.rs", Some("rust")),
        ("Main.java", Some("java")),
        ("setup.py", Some("python")),
        ("stubs.pyi", Some("python")),
        ("main.go", Some("go")),
        ("package.json", Some("json")),
        ("tsconfig.jsonc", Some("json")),
        ("events.jsonl", Some("json")),
        (".github/workflows/ci.yml", Some("yaml")),
        ("compose.yaml", Some("yaml")),
        ("Cargo.toml", Some("toml")),
        ("README.md", Some("markdown")),
        ("notes.markdown", Some("markdown")),
        // Whole file names.
        ("Cargo.lock", Some("toml")),
        ("vendor/Cargo.lock", Some("toml")),
        ("poetry.lock", Some("toml")),
        ("uv.lock", Some("toml")),
        ("Pipfile", Some("toml")),
        ("web/package-lock.json", Some("json")),
        (".babelrc", Some("json")),
        (".eslintrc", Some("json")),
        // Case: the exact extension first, then its lower case.
        ("README.MD", Some("markdown")),
        ("Main.JAVA", Some("java")),
        // None: plain text.
        ("yarn.lock", None),
        (".gitignore", None),
        ("Makefile", None),
        ("archive.tar.gz", None),
        ("src.rs/notes", None),
        ("", None),
    ];
    for (path, expected) in cases {
        assert_eq!(langs.for_path(path).map(|l| l.name()), expected, "{path:?}");
    }
}

#[test]
fn lang_rules() {
    let langs = Languages::core();
    for lang in langs.iter() {
        let expected = match lang.name() {
            "json" => Rules::Json,
            "markdown" => Rules::Markup,
            _ => Rules::Code,
        };
        assert_eq!(lang.rules(), expected, "{}", lang.name());
    }
}

/// `name` → the versions of every package by that name in Cargo.lock.
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
    let langs = Languages::core();
    for a in CORE_LANGUAGES {
        for b in CORE_LANGUAGES {
            let (va, vb) = (
                langs.get(a.name).unwrap().grammar_version(),
                langs.get(b.name).unwrap().grammar_version(),
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

#[test]
fn lang_abi_in_range() {
    let range = tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION;
    for spec in CORE_LANGUAGES {
        let abi = (spec.language)().abi_version();
        assert!(
            range.contains(&abi),
            "{}: ABI {abi} not in {range:?}",
            spec.name
        );
    }
}

#[test]
fn lang_queries_compile() {
    for lang in Languages::core().iter() {
        assert!(lang.prepare(), "{}", lang.name());
    }
}

/// Every capture of the core queries means something to Lectern: a token
/// kind, plain, or a predicate's `_` capture.
#[test]
fn lang_captures_known() {
    for lang in Languages::core().iter() {
        let compiled = lang.compiled().expect("compiles");
        for name in compiled.query.capture_names() {
            assert!(
                classify(name).is_some(),
                "{}: unknown capture @{name}",
                lang.name()
            );
        }
        assert!(
            compiled
                .captures
                .iter()
                .any(|c| matches!(c, Capture::Kind(_))),
            "{}",
            lang.name()
        );
    }
}

#[test]
fn lang_query_versions() {
    let langs = Languages::core();
    for spec in CORE_LANGUAGES {
        let lang = langs.get(spec.name).unwrap();
        assert_eq!(
            lang.query_version(),
            fnv64(&[
                &SPANS_VERSION.to_le_bytes(),
                format!("tree-sitter@{TREE_SITTER_VERSION}\0").as_bytes(),
                spec.query.as_bytes(),
            ]),
            "{}",
            spec.name
        );
        assert_eq!(lang.query_version(), query_version(spec.query));
    }
    // Every language has its own query text, so its own version.
    let mut versions: Vec<u64> = langs.iter().map(|l| l.query_version()).collect();
    versions.sort_unstable();
    versions.dedup();
    assert_eq!(versions.len(), CORE_LANGUAGES.len());
}

/// Every query file says where it came from and what Lectern changed.
#[test]
fn lang_query_headers() {
    for spec in CORE_LANGUAGES {
        for part in spec.query.split("; Lectern highlight query for ").skip(1) {
            // The header runs to the first line that is not a comment.
            let header: String = part
                .lines()
                .enumerate()
                .take_while(|(i, l)| *i == 0 || l.starts_with(';'))
                .map(|(_, l)| l)
                .collect::<Vec<_>>()
                .join("\n");
            for needle in [
                "Based on",
                "(commit ",
                "Copyright (c)",
                "MIT",
                "Precedence: the ",
                "Lectern's changes",
            ] {
                assert!(
                    header.contains(needle),
                    "{}: {needle:?} in {header}",
                    spec.name
                );
            }
        }
    }
}
