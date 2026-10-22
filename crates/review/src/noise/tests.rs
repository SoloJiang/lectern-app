use std::fmt::Write as _;
use std::sync::Arc;

use lectern_services::project::BlobId;

use super::{classify, group, sensitive};
use crate::group::{GroupInput, arrange};
use crate::types::changes::{
    ChangePath, ChangeStatus, FileChange, FileMode, FileSide, SideIdentity,
};
use crate::types::review::{GroupKind, NoiseKind, SensitiveKind};

fn kind(path: &str) -> Option<NoiseKind> {
    classify(path.as_bytes(), false)
}

fn grouped(path: &str) -> GroupKind {
    group(path.as_bytes(), false)
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

/// Each of `paths` is classified as `expected`.
fn classified(paths: &[&str], expected: Option<NoiseKind>) {
    for path in paths {
        assert_eq!(kind(path), expected, "{path}");
    }
}

#[test]
fn noise_lockfile_names() {
    for name in super::LOCKFILE_NAMES {
        let nested = format!("web/app/{}", text(name));
        classified(&[text(name), &nested], Some(NoiseKind::Lockfile));
    }
    // The whole file name, case-sensitive.
    let near = [
        "Cargo.lock.orig",
        "my-yarn.lock",
        "CARGO.LOCK",
        "Cargo.lock/x.rs",
    ];
    classified(&near, None);
}

#[test]
fn noise_generated_patterns() {
    for suffix in super::GENERATED_SUFFIXES {
        let path = format!("src/api/schema{}", text(suffix));
        classified(&[&path], Some(NoiseKind::Generated));
    }
    let paths = [
        "src/types.generated.ts",
        "gen/api.generated.go",
        "src/__generated__/query.ts",
        "__generated__/a/b.rs",
    ];
    classified(&paths, Some(NoiseKind::Generated));
    // A segment only counts as a directory; the infix needs both dots.
    let near = [
        "src/__generated__",
        "src/generated.ts",
        "src/app.min.jsx",
        "src/min.js",
    ];
    classified(&near, None);
}

#[test]
fn noise_snapshot_patterns() {
    for suffix in super::SNAPSHOT_SUFFIXES {
        let path = format!("src/snapshots/case{}", text(suffix));
        classified(&[&path], Some(NoiseKind::Snapshot));
    }
    let in_dir = ["web/__snapshots__/App.test.tsx.txt"];
    classified(&in_dir, Some(NoiseKind::Snapshot));
    let near = ["src/snap.rs", "web/__snapshots__", "src/case.snapshot"];
    classified(&near, None);
}

#[test]
fn noise_linguist_generated_wins() {
    let generated = Some(NoiseKind::Generated);
    assert_eq!(classify(b"src/main.rs", true), generated);
    // Over the lockfile and snapshot lists as well.
    assert_eq!(classify(b"Cargo.lock", true), generated);
    assert_eq!(classify(b"a.snap", true), generated);
    let noise = GroupKind::Noise(NoiseKind::Generated);
    assert_eq!(group(b"tests/fixture.rs", true), noise);
}

#[test]
fn noise_paths_are_bytes() {
    let lockfile = Some(NoiseKind::Lockfile);
    assert_eq!(classify(b"\xff\xfe/Cargo.lock", false), lockfile);
    let generated = Some(NoiseKind::Generated);
    assert_eq!(classify(b"src/\xff.min.js", false), generated);
    assert_eq!(group(b"src/\xff.rs", false), GroupKind::Code);
}

/// Every pattern of the noise table and the group rules on one list of
/// paths: the classification and the group of each.
#[test]
fn noise_and_group_table() {
    let paths = [
        "src/lib.rs",
        "src/test_utils.rs",
        "src/contest.rs",
        "package-lock.json",
        "web/pnpm-lock.yaml",
        "Cargo.lock",
        "go.sum",
        "web/dist/app.min.js",
        "proto/api.pb.go",
        "py/api_pb2_grpc.py",
        "lib/model.freezed.dart",
        "src/api.generated.ts",
        "web/src/__generated__/graphql.ts",
        "crates/review/src/snapshots/a.snap",
        "tests/__snapshots__/view.ambr",
        "tests/integration.rs",
        "src/__tests__/App.tsx",
        "spec/models/user_spec.rb",
        "pkg/testdata/input.json",
        "web/App.test.tsx",
        "web/App.spec.ts",
        "pkg/server_test.go",
        "py/test_server.py",
        "py/server_test.py",
        "java/src/FooTest.java",
        "java/src/FooTests.java",
        "README.md",
        "docs/design/review.md",
        "doc/api.txt",
        "guide/intro.mdx",
        "LICENSE-MIT",
        "CHANGELOG",
        "manual.rst",
        "book.adoc",
        "Cargo.toml",
        "crates/review/Cargo.toml",
        "web/package.json",
        "go.mod",
        "pom.xml",
        "app/build.gradle.kts",
        "pyproject.toml",
        "requirements-dev.txt",
        "Gemfile",
        "pnpm-workspace.yaml",
        ".github/CODEOWNERS",
        ".circleci/config.yml",
        ".husky/pre-commit",
        ".gitignore",
        "config/app.yaml",
        "rustfmt.toml",
        "setup.cfg",
        "Dockerfile.dev",
        "Makefile",
        "justfile",
        "Jenkinsfile",
        "vite.config.ts",
        "tsconfig.base.json",
        "tests/README.md",
        "docs/.vitepress/config.yml",
    ];
    let mut out = String::new();
    for path in paths {
        writeln!(out, "{path}: {:?} / {:?}", kind(path), grouped(path)).unwrap();
    }
    insta::assert_snapshot!(out);
}

#[test]
fn group_match_order() {
    let cases = [
        // Noise before deps: a lockfile is noise, its manifest a dependency.
        ("package-lock.json", GroupKind::Noise(NoiseKind::Lockfile)),
        ("package.json", GroupKind::Deps),
        // Tests before docs, docs before deps and config, deps before config.
        ("tests/README.md", GroupKind::Tests),
        ("docs/Cargo.toml", GroupKind::Docs),
        ("docs/settings.yml", GroupKind::Docs),
        ("Cargo.toml", GroupKind::Deps),
        (".github/workflows/ci.yml", GroupKind::Config),
        // Snapshots of tests are noise, not tests.
        (
            "tests/snapshots/a.snap",
            GroupKind::Noise(NoiseKind::Snapshot),
        ),
        ("src/main.rs", GroupKind::Code),
    ];
    for (path, expected) in cases {
        assert_eq!(grouped(path), expected, "{path}");
    }
}

fn input<'a>(path: &'a str, package: Option<&'a str>) -> GroupInput<'a> {
    GroupInput {
        kind: grouped(path),
        package,
        path: path.as_bytes(),
    }
}

#[test]
fn group_display_order() {
    let files = [
        input("tests/b.rs", None),
        input("a.snap", None),
        input("README.md", None),
        input("dist/app.min.js", None),
        input("Cargo.toml", None),
        input("src/main.rs", None),
        input("Cargo.lock", None),
        input("rustfmt.toml", None),
    ];
    let groups = arrange(&files);
    let kinds: Vec<GroupKind> = groups.iter().map(|g| g.kind).collect();
    let expected = [
        GroupKind::Code,
        GroupKind::Tests,
        GroupKind::Config,
        GroupKind::Deps,
        GroupKind::Docs,
        GroupKind::Noise(NoiseKind::Lockfile),
        GroupKind::Noise(NoiseKind::Generated),
        GroupKind::Noise(NoiseKind::Snapshot),
    ];
    assert_eq!(kinds, expected);
    let all: Vec<u32> = groups
        .iter()
        .flat_map(|g| g.files.iter().copied())
        .collect();
    assert_eq!(all, [5, 0, 7, 4, 2, 6, 3, 1]);
}

#[test]
fn group_sorted_by_package_then_path() {
    let files = [
        input("web/src/z.ts", Some("web")),
        input("crates/b/src/lib.rs", Some("crates/b")),
        input("tools/x.rs", None),
        input("crates/a/src/z.rs", Some("crates/a")),
        input("web/src/a.ts", Some("web")),
        input("crates/a/src/b.rs", Some("crates/a")),
        input("build.rs", None),
        input("crates/a/tests/t.rs", Some("crates/a")),
        input("web/src/a.test.ts", Some("web")),
    ];
    let groups = arrange(&files);
    let mut out = String::new();
    for g in groups.iter() {
        writeln!(out, "{:?}", g.kind).unwrap();
        for &i in g.files.iter() {
            let f = &files[i as usize];
            writeln!(out, "  {:?} {}", f.package, text(f.path)).unwrap();
        }
    }
    insta::assert_snapshot!(out);
}

#[test]
fn group_empty_list() {
    assert!(arrange(&[]).is_empty());
}

fn path(p: &str) -> ChangePath {
    ChangePath(Arc::from(p.as_bytes()))
}

fn side(mode: FileMode, n: u8) -> Option<FileSide> {
    Some(FileSide {
        mode,
        content: SideIdentity::Blob(BlobId::Sha1([n; 20])),
    })
}

fn change(
    status: ChangeStatus,
    p: &str,
    old: Option<FileSide>,
    new: Option<FileSide>,
) -> FileChange {
    FileChange {
        status,
        path: path(p),
        old,
        new,
        generated: false,
    }
}

fn modified(p: &str) -> FileChange {
    let (old, new) = (side(FileMode::File, 1), side(FileMode::File, 2));
    change(ChangeStatus::Modified, p, old, new)
}

fn added(p: &str, mode: FileMode) -> FileChange {
    change(ChangeStatus::Added, p, None, side(mode, 1))
}

/// A modification of `p` from `old` to `new`.
fn between(p: &str, old: (FileMode, u8), new: (FileMode, u8)) -> FileChange {
    let (old, new) = (side(old.0, old.1), side(new.0, new.1));
    change(ChangeStatus::Modified, p, old, new)
}

/// Each of `paths`, modified, triggers `expected`.
fn sensitive_paths(paths: &[&str], expected: Option<SensitiveKind>) {
    for p in paths {
        assert_eq!(sensitive(&modified(p)), expected, "{p}");
    }
}

#[test]
fn sensitive_agent_instructions() {
    let paths = [
        "CLAUDE.md",
        "AGENTS.md",
        "GEMINI.md",
        "crates/review/AGENTS.md",
        "web/CLAUDE.md",
        ".mcp.json",
        ".github/copilot-instructions.md",
        ".claude/settings.json",
        ".claude/skills/ship/SKILL.md",
        ".codex/config.toml",
        ".cursor/rules/style.mdc",
        "web/.claude/commands/x.md",
    ];
    sensitive_paths(&paths, Some(SensitiveKind::AgentInstructions));
    let near = [
        "docs/CLAUDE.md.bak",
        "web/.mcp.json",
        "x/.github/copilot-instructions.md",
        "claude/a.md",
    ];
    sensitive_paths(&near, None);
}

#[test]
fn sensitive_ci() {
    let paths = [
        ".github/workflows/ci.yml",
        ".github/workflows/sub/release.yaml",
        ".circleci/config.yml",
        ".gitlab-ci.yml",
        "Jenkinsfile",
        "azure-pipelines.yml",
    ];
    sensitive_paths(&paths, Some(SensitiveKind::Ci));
    let near = [
        ".github/dependabot.yml",
        "web/.gitlab-ci.yml",
        "ci/Jenkinsfile.groovy",
    ];
    sensitive_paths(&near, None);
}

#[test]
fn sensitive_hooks() {
    let paths = [
        ".husky/pre-commit",
        ".githooks/pre-push",
        ".pre-commit-config.yaml",
        "lefthook.yml",
    ];
    sensitive_paths(&paths, Some(SensitiveKind::Hooks));
    sensitive_paths(&["web/.husky/x"], None);
}

#[test]
fn sensitive_git_metadata() {
    let paths = [".gitattributes", "vendor/.gitattributes", ".gitmodules"];
    sensitive_paths(&paths, Some(SensitiveKind::GitMetadata));
    sensitive_paths(&["sub/.gitmodules", ".gitignore"], None);
}

#[test]
fn sensitive_symlink() {
    let symlink = Some(SensitiveKind::Symlink);
    assert_eq!(sensitive(&added("link", FileMode::Symlink)), symlink);
    let (link, file) = (FileMode::Symlink, FileMode::File);
    let retargeted = between("link", (link, 1), (link, 2));
    assert_eq!(sensitive(&retargeted), symlink);
    let mut from_file = between("link", (file, 1), (link, 1));
    from_file.status = ChangeStatus::TypeChanged;
    assert_eq!(sensitive(&from_file), symlink);
    // Renamed with the same target: not a new link target.
    let mut moved = between("link", (link, 1), (link, 1));
    moved.status = ChangeStatus::Renamed {
        from: path("old"),
        similarity: 100,
    };
    assert_eq!(sensitive(&moved), None);
    // The same blob, kept on one side and not retained on the other.
    let mut not_retained = moved.clone();
    let oid = BlobId::Sha1([1; 20]);
    not_retained.new.as_mut().unwrap().content = SideIdentity::NotRetained(oid);
    assert_eq!(sensitive(&not_retained), None);
    let gone = change(ChangeStatus::Deleted, "link", side(link, 1), None);
    assert_eq!(sensitive(&gone), None);
}

#[test]
fn sensitive_executable() {
    let executable = Some(SensitiveKind::Executable);
    let (exe, file) = (FileMode::Executable, FileMode::File);
    let made = between("run.sh", (file, 1), (exe, 1));
    assert_eq!(sensitive(&made), executable);
    assert_eq!(sensitive(&added("run.sh", exe)), executable);
    assert_eq!(sensitive(&between("run.sh", (exe, 1), (exe, 2))), None);
}

#[test]
fn sensitive_submodule() {
    let submodule = Some(SensitiveKind::Submodule);
    let link = FileMode::Gitlink;
    assert_eq!(sensitive(&between("lib", (link, 1), (link, 2))), submodule);
    assert_eq!(sensitive(&added("lib", link)), submodule);
    assert_eq!(sensitive(&between("lib", (link, 1), (link, 1))), None);
}

#[test]
fn sensitive_paths_and_order() {
    // A deleted workflow, and a rename away from one, still count.
    let old = side(FileMode::File, 1);
    let workflow = ".github/workflows/ci.yml";
    let deleted = change(ChangeStatus::Deleted, workflow, old, None);
    assert_eq!(sensitive(&deleted), Some(SensitiveKind::Ci));
    let mut renamed = modified("notes.md");
    renamed.status = ChangeStatus::Renamed {
        from: path("AGENTS.md"),
        similarity: 90,
    };
    let agent = Some(SensitiveKind::AgentInstructions);
    assert_eq!(sensitive(&renamed), agent);
    // The table's order: the path rules before the mode rules.
    let hook = added(".husky/pre-commit", FileMode::Executable);
    assert_eq!(sensitive(&hook), Some(SensitiveKind::Hooks));
    assert_eq!(sensitive(&modified("src/main.rs")), None);
}
