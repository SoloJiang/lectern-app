//! Pattern semantics of `WorkspaceFacts::contains` (`facts_*`).

use std::sync::Arc;

use lectern_services::project::{FileKind, FileList};

use super::workspace_facts;

fn tree(paths: &[(&str, FileKind)]) -> Arc<FileList> {
    let mut b = FileList::builder();
    for (path, kind) in paths {
        b.push(path, *kind);
    }
    Arc::new(b.build())
}

fn files(paths: &[&str]) -> Arc<FileList> {
    tree(
        &paths
            .iter()
            .map(|p| (*p, FileKind::File))
            .collect::<Vec<_>>(),
    )
}

#[test]
fn facts_file_names_match_at_any_depth() {
    let facts = workspace_facts(vec![files(&[
        "README.md",
        "apps/console/pom.xml",
        "x/build.gradle.kts",
        "src/main/java/A.java",
    ])]);
    assert!(facts.contains("pom.xml"));
    assert!(facts.contains("README.md"));
    assert!(facts.contains("build.gradle*"));
    assert!(facts.contains("*.java"));
    assert!(facts.contains("**/*.java"));
    assert!(facts.contains("**/pom.xml"));
    assert!(facts.contains("A.jav?"));
    assert!(facts.contains("[AB].java"));
    assert!(!facts.contains("*.kt"));
    assert!(!facts.contains("Cargo.toml"));
    // Case-sensitive.
    assert!(!facts.contains("readme.md"));
    // `*.ext` matches the last extension only through the general path.
    assert!(facts.contains("*.gradle.kts"));
    assert!(facts.contains("*.kts"));
}

#[test]
fn facts_patterns_with_a_slash_start_at_the_root() {
    let facts = workspace_facts(vec![files(&[
        "pom.xml",
        "apps/console/src/main.ts",
        "crates/a/src/lib.rs",
    ])]);
    assert!(facts.contains("crates/*/src/lib.rs"));
    assert!(facts.contains("crates/**/lib.rs"));
    assert!(facts.contains("**/src/*.rs"));
    assert!(facts.contains("apps/**"));
    // Anchored at the root, and `*` does not cross `/`.
    assert!(!facts.contains("src/*.rs"));
    assert!(!facts.contains("crates/*.rs"));
    assert!(!facts.contains("apps/*"));
}

#[test]
fn facts_match_files_only() {
    let facts = workspace_facts(vec![tree(&[
        ("apps/web/index.ts", FileKind::File),
        ("libs/sub", FileKind::Submodule),
        ("bin/tool", FileKind::Executable),
        ("link", FileKind::Symlink),
    ])]);
    // Directories are never matched, submodules neither.
    assert!(!facts.contains("apps"));
    assert!(!facts.contains("web"));
    assert!(!facts.contains("sub"));
    assert!(!facts.contains("libs/sub"));
    assert!(facts.contains("tool"));
    assert!(facts.contains("link"));
}

#[test]
fn facts_are_the_union_of_the_trees() {
    let facts = workspace_facts(vec![files(&["pom.xml"]), files(&["go.mod", "cmd/main.go"])]);
    assert!(facts.contains("pom.xml"));
    assert!(facts.contains("*.go"));
    assert!(facts.contains("cmd/*.go"));
    let empty = workspace_facts(Vec::new());
    assert!(!empty.contains("pom.xml"));
    assert!(!empty.contains("**/*"));
}

#[test]
fn facts_invalid_patterns_are_false() {
    let facts = workspace_facts(vec![files(&["a/b.txt", "[x"])]);
    for pattern in ["[", "a/[", "a**", "**a/b", "", "**/", "a/***/b"] {
        assert!(!facts.contains(pattern), "{pattern:?}");
    }
    // Asking twice gives the cached answer.
    assert!(facts.contains("*.txt"));
    assert!(facts.contains("*.txt"));
    assert!(!facts.contains("["));
}

#[test]
fn facts_name_table_spans_trees_and_grows() {
    // 3501 entries over three trees (one empty, one with a submodule
    // entry) and 2000 distinct names.
    let a: Vec<String> = (0..1500)
        .map(|i| format!("a/d{}/n{i}.txt", i % 7))
        .collect();
    let c: Vec<String> = (0..1500)
        .map(|i| format!("c/n{}.rs", i % 500))
        .chain((0..500).map(|i| format!("c/x/n{i}.rs")))
        .collect();
    let mut with_module: Vec<(&str, FileKind)> =
        c.iter().map(|p| (p.as_str(), FileKind::File)).collect();
    with_module.push(("c/vendor", FileKind::Submodule));
    let trees = vec![
        files(&a.iter().map(String::as_str).collect::<Vec<_>>()),
        files(&[]),
        tree(&with_module),
    ];
    let facts = workspace_facts(trees);
    for i in [0, 7, 1499] {
        assert!(facts.contains(&format!("n{i}.txt")), "n{i}.txt");
    }
    for i in [0, 499] {
        assert!(facts.contains(&format!("n{i}.rs")), "n{i}.rs");
    }
    assert!(!facts.contains("n1500.txt"));
    assert!(!facts.contains("n500.rs"));
    assert!(!facts.contains("vendor"), "a submodule is not a file");
    assert!(facts.contains("n149?.txt"));
    assert!(!facts.contains("n5??.rs"));

    // Every name distinct: the table grows past 3/4 of its first size.
    let many: Vec<String> = (0..5000).map(|i| format!("f{i}")).collect();
    let facts = workspace_facts(vec![files(
        &many.iter().map(String::as_str).collect::<Vec<_>>(),
    )]);
    assert!(many.iter().all(|n| facts.contains(n)));
    assert!(!facts.contains("f5000"));
}
