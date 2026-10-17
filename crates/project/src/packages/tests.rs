use std::collections::HashMap;
use std::fmt::Write as _;

use super::*;
use crate::manifest::{self, MAX_MANIFEST_BYTES};

/// An in-memory worktree: tracked files, their kinds and contents.
struct Tree {
    files: FileList,
    contents: HashMap<String, Vec<u8>>,
    /// Paths for which `manifest` answers None (deleted from the worktree)
    /// or `Unreadable`.
    missing: Vec<&'static str>,
    unreadable: Vec<&'static str>,
}

impl Tree {
    /// Regular files with contents; "" for files whose content is not read.
    fn new(entries: &[(&str, &str)]) -> Self {
        Self::with_kinds(
            &entries
                .iter()
                .map(|&(p, c)| (p, FileKind::File, c))
                .collect::<Vec<_>>(),
        )
    }

    fn with_kinds(entries: &[(&str, FileKind, &str)]) -> Self {
        let mut builder = FileList::builder();
        let mut contents = HashMap::new();
        for &(path, kind, content) in entries {
            builder.push(path, kind);
            contents.insert(path.to_owned(), content.as_bytes().to_vec());
        }
        Self {
            files: builder.build(),
            contents,
            missing: Vec::new(),
            unreadable: Vec::new(),
        }
    }

    fn resolve(&self, root_name: &str) -> Vec<Package> {
        self.resolve_logged(root_name).0
    }

    /// Also returns every path the callback was asked for.
    fn resolve_logged(&self, root_name: &str) -> (Vec<Package>, Vec<String>) {
        let calls = std::cell::RefCell::new(Vec::new());
        let manifest = |path: &str| -> Option<ManifestResult> {
            calls.borrow_mut().push(path.to_owned());
            if self.missing.contains(&path) {
                return None;
            }
            if self.unreadable.contains(&path) {
                return Some(Err(ManifestError::Unreadable));
            }
            let kind = ManifestKind::from_file_name(path.rsplit('/').next()?)?;
            let bytes = self.contents.get(path)?;
            Some(manifest::parse(kind, bytes).map(Arc::new))
        };
        let packages = resolve_packages(&self.files, root_name, &manifest);
        check_invariants(&packages);
        (packages, calls.into_inner())
    }
}

/// The invariants of docs/design/project.md「子项目与清单」.
fn check_invariants(packages: &[Package]) {
    for pair in packages.windows(2) {
        assert!(
            pair[0].dir < pair[1].dir,
            "sorted and unique: {:?} {:?}",
            pair[0].dir,
            pair[1].dir
        );
    }
    for p in packages {
        if let Some(parent) = p.parent {
            let parent = &packages[parent as usize].dir;
            assert!(
                parent.is_empty() || p.dir.starts_with(&format!("{parent}/")),
                "{parent:?} is not a proper prefix of {:?}",
                p.dir
            );
        }
        assert!(
            !p.manifests.is_empty() || p.declared,
            "{:?} has no manifest and is not declared",
            p.dir
        );
    }
}

fn render(packages: &[Package]) -> String {
    let mut out = String::new();
    for p in packages {
        let dir = if p.dir.is_empty() { "<root>" } else { &p.dir };
        let parent = p.parent.map_or("-".to_owned(), |i| {
            let d = &packages[i as usize].dir;
            if d.is_empty() {
                "<root>".to_owned()
            } else {
                d.to_string()
            }
        });
        let mut flags = Vec::new();
        if p.declared {
            flags.push("declared");
        }
        if p.workspace_root {
            flags.push("workspace-root");
        }
        let _ = writeln!(
            out,
            "{dir}  name={:?} files={} parent={parent} [{}]",
            p.name,
            p.files,
            flags.join(", ")
        );
        for m in p.manifests.iter() {
            let _ = write!(out, "    {:?} {}", m.kind, m.path);
            if let Some(name) = &m.name {
                let _ = write!(out, " name={name:?}");
            }
            if m.partial {
                out.push_str(" partial");
            }
            if let Some(e) = m.error {
                let _ = write!(out, " error={e:?}");
            }
            out.push('\n');
        }
    }
    out
}

#[test]
fn packages_cloudprint_shape() {
    // yarn workspaces + a Maven multi-module build; the console frontend has
    // a package.json but is not in `workspaces`.
    let tree = Tree::new(&[
        (
            "package.json",
            r#"{"name": "cloudprint-miniapps", "private": true, "workspaces": ["packages/*", "apps/*/web"]}"#,
        ),
        ("yarn.lock", ""),
        (
            "pom.xml",
            "<project><groupId>com.example.cloudprint</groupId><artifactId>cloudprint-parent</artifactId>\
             <modules><module>apps/miniapp-console/backend</module><module>apps/miniapp-print/backend</module>\
             <module>libs/java-common</module></modules></project>",
        ),
        ("README.md", ""),
        ("packages/ui/package.json", r#"{"name": "@cloudprint/ui"}"#),
        ("packages/ui/src/index.ts", ""),
        (
            "packages/utils/package.json",
            r#"{"name": "@cloudprint/utils"}"#,
        ),
        ("packages/utils/src/index.ts", ""),
        ("packages/docs/README.md", ""),
        (
            "apps/miniapp-print/web/package.json",
            r#"{"name": "print-web"}"#,
        ),
        ("apps/miniapp-print/web/src/app.tsx", ""),
        (
            "apps/miniapp-print/backend/pom.xml",
            "<project><parent><groupId>com.example.cloudprint</groupId></parent><artifactId>print-backend</artifactId></project>",
        ),
        ("apps/miniapp-print/backend/src/main/java/App.java", ""),
        (
            "apps/miniapp-console/frontend/package.json",
            r#"{"name": "console-frontend"}"#,
        ),
        ("apps/miniapp-console/frontend/src/main.ts", ""),
        (
            "apps/miniapp-console/backend/pom.xml",
            "<project><artifactId>console-backend</artifactId></project>",
        ),
        ("apps/miniapp-console/backend/src/main/java/App.java", ""),
        ("libs/java-common/src/main/java/Util.java", ""),
    ]);
    insta::assert_snapshot!(render(&tree.resolve("cloudprint-miniapps")));
}

#[test]
fn packages_nested_workspaces() {
    let tree = Tree::new(&[
        (
            "pnpm-workspace.yaml",
            "packages:\n  - 'packages/*'\n  - 'tools'\n  - '!packages/old'\n",
        ),
        ("package.json", r#"{"name": "root"}"#),
        ("packages/a/package.json", r#"{"name": "a"}"#),
        ("packages/old/package.json", r#"{"name": "old"}"#),
        // A nested npm workspace inside a pnpm member.
        (
            "tools/package.json",
            r#"{"name": "tools", "workspaces": ["*"]}"#,
        ),
        ("tools/lint/package.json", r#"{"name": "lint"}"#),
        ("tools/lint/index.js", ""),
        // A Cargo workspace inside the repository, not declared by pnpm.
        (
            "native/Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n",
        ),
        (
            "native/crates/core/Cargo.toml",
            "[package]\nname = \"native-core\"\n",
        ),
        ("native/crates/core/src/lib.rs", ""),
        (
            "native/crates/.hidden/Cargo.toml",
            "[package]\nname = \"hidden\"\n",
        ),
        // Cargo globs only match Cargo.toml directories.
        (
            "native/crates/js/package.json",
            r#"{"name": "js-in-crates"}"#,
        ),
    ]);
    insta::assert_snapshot!(render(&tree.resolve("repo")));
}

#[test]
fn packages_one_directory_several_manifests() {
    let tree = Tree::new(&[
        (
            "settings.gradle.kts",
            "rootProject.name = \"poly\"\ninclude(\":service\")\n",
        ),
        ("service/build.gradle.kts", ""),
        (
            "service/pom.xml",
            "<project><artifactId>service-maven</artifactId></project>",
        ),
        ("service/package.json", r#"{"name": "service-npm"}"#),
        ("service/go.mod", "module example.com/service\n"),
        // Without package.json the go.mod name wins over Maven.
        ("other/go.mod", "module example.com/other\n"),
        (
            "other/pom.xml",
            "<project><artifactId>other-maven</artifactId></project>",
        ),
    ]);
    insta::assert_snapshot!(render(&tree.resolve("poly")));
}

#[test]
fn packages_gradle_projects_without_build_files() {
    let tree = Tree::new(&[
        (
            "settings.gradle",
            "rootProject.name = 'android'\ninclude ':app', ':docs', ':empty', ':feature:login'\nincludeBuild 'build-logic'\n",
        ),
        ("build.gradle", ""),
        ("app/build.gradle", ""),
        // A project directory needs tracked files, not a build file.
        ("docs/README.md", ""),
        ("feature/login/src/Login.kt", ""),
        (
            "build-logic/settings.gradle",
            "rootProject.name = 'build-logic'\n",
        ),
        ("build-logic/src/Plugin.kt", ""),
    ]);
    insta::assert_snapshot!(render(&tree.resolve("android")));
}

#[test]
fn packages_manifest_errors_are_kept() {
    let mut big = String::from("[package]\nname = \"big\"\n");
    big.push_str(&"#".repeat(MAX_MANIFEST_BYTES));
    let mut tree = Tree::new(&[
        ("package.json", r#"{"workspaces": ["packages/*"]}"#),
        ("packages/bad/package.json", "{\n  \"name\": \"bad\",\n}\n"),
        ("packages/good/package.json", r#"{"name": "good"}"#),
        ("packages/locked/package.json", r#"{"name": "unread"}"#),
        ("rust/Cargo.toml", &big),
        // A broken workspace manifest is still a package; its members are not expanded.
        (
            "broken/pom.xml",
            "<project><modules><module>child</module></modules>",
        ),
        (
            "broken/child/pom.xml",
            "<project><artifactId>child</artifactId></project>",
        ),
    ]);
    tree.unreadable.push("packages/locked/package.json");
    insta::assert_snapshot!(render(&tree.resolve("errors")));
}

#[test]
fn packages_maven_modules_outside_their_directory() {
    let tree = Tree::new(&[
        (
            "backend/pom.xml",
            "<project><artifactId>backend</artifactId><modules><module>../shared</module>\
             <module>./api/</module><module>tools/pom-codegen.xml</module><module>missing</module></modules></project>",
        ),
        (
            "backend/api/pom.xml",
            "<project><artifactId>api</artifactId></project>",
        ),
        ("backend/tools/pom-codegen.xml", "<project/>"),
        ("shared/src/Shared.java", ""),
    ]);
    insta::assert_snapshot!(render(&tree.resolve("maven")));
}

#[test]
fn packages_paths_outside_the_worktree_are_dropped() {
    let tree = Tree::new(&[
        (
            "go.work",
            "use (\n\t.\n\t../outside\n\t/abs/path\n\t./mod\n)\n",
        ),
        ("go.mod", "module example.com/root\n"),
        ("mod/go.mod", "module example.com/mod\n"),
        (
            "settings.gradle",
            "includeFlat 'sibling'\ninclude ':a'\nproject(':a').projectDir = file('../../a')\n",
        ),
        (
            "package.json",
            r#"{"workspaces": ["../*", "/abs/*", "mod/../web"]}"#,
        ),
        ("web/package.json", r#"{"name": "web"}"#),
        ("a/x.txt", ""),
    ]);
    insta::assert_snapshot!(render(&tree.resolve("bounds")));
}

#[test]
fn packages_sample_directories_are_not_packages() {
    let tree = Tree::new(&[
        (
            "package.json",
            r#"{"name": "root", "workspaces": ["test/fixtures/declared"]}"#,
        ),
        ("test/fixtures/basic/package.json", r#"{"name": "fixture"}"#),
        (
            "test/fixtures/declared/package.json",
            r#"{"name": "declared-fixture"}"#,
        ),
        ("src/__fixtures__/x/package.json", "{}"),
        ("node_modules/left-pad/package.json", "{}"),
        ("vendor/github.com/x/y/go.mod", "module github.com/x/y\n"),
        (
            "internal/testdata/mod/go.mod",
            "module example.com/testdata\n",
        ),
        // Only whole segments count.
        ("my-fixtures/package.json", r#"{"name": "not-a-sample"}"#),
    ]);
    insta::assert_snapshot!(render(&tree.resolve("samples")));
}

#[test]
fn packages_cargo_virtual_root_and_exclude() {
    let tree = Tree::new(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\", \"crates/experimental/keep\", \"./tools/xtask/\"]\nexclude = [\"crates/experimental\", \"crates/skip\"]\n",
        ),
        ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
        ("crates/skip/Cargo.toml", "[package]\nname = \"skip\"\n"),
        (
            "crates/experimental/Cargo.toml",
            "[package]\nname = \"experimental\"\n",
        ),
        // Listed without wildcards: an explicit member wins over exclude.
        (
            "crates/experimental/keep/Cargo.toml",
            "[package]\nname = \"keep\"\n",
        ),
        ("tools/xtask/Cargo.toml", "[package]\nname = \"xtask\"\n"),
        // `*` does not cross `/`.
        (
            "crates/a/nested/Cargo.toml",
            "[package]\nname = \"nested\"\n",
        ),
        // An invalid glob marks the manifest partial.
        ("bad/Cargo.toml", "[workspace]\nmembers = [\"[\"]\n"),
    ]);
    insta::assert_snapshot!(render(&tree.resolve("lectern")));
}

#[test]
fn packages_only_regular_present_manifests_are_candidates() {
    let mut tree = Tree::with_kinds(&[
        ("package.json", FileKind::File, r#"{"workspaces": ["*"]}"#),
        (
            "linked/package.json",
            FileKind::Symlink,
            r#"{"name": "linked"}"#,
        ),
        ("linked/index.js", FileKind::File, ""),
        (
            "script/package.json",
            FileKind::Executable,
            r#"{"name": "script"}"#,
        ),
        (
            "deleted/package.json",
            FileKind::File,
            r#"{"name": "deleted"}"#,
        ),
        ("deleted/index.js", FileKind::File, ""),
        ("sub", FileKind::Submodule, ""),
    ]);
    tree.missing.push("deleted/package.json");
    insta::assert_snapshot!(render(&tree.resolve("kinds")));
}

#[test]
fn packages_parent_skips_byte_order_neighbours() {
    // "a-b" sorts between "a" and "a/c": the parent of "a/c" is still "a".
    let tree = Tree::new(&[
        ("a/go.mod", "module a\n"),
        ("a-b/go.mod", "module ab\n"),
        ("a/c/go.mod", "module ac\n"),
        ("a/c/d/e/go.mod", "module ace\n"),
    ]);
    let packages = tree.resolve("x");
    let parent = |dir: &str| {
        let p = packages.iter().find(|p| &*p.dir == dir).unwrap();
        p.parent.map(|i| packages[i as usize].dir.to_string())
    };
    assert_eq!(parent("a"), None);
    assert_eq!(parent("a-b"), None);
    assert_eq!(parent("a/c").as_deref(), Some("a"));
    assert_eq!(parent("a/c/d/e").as_deref(), Some("a/c"));
}

#[test]
fn packages_empty_file_list() {
    assert!(Tree::new(&[]).resolve("empty").is_empty());
}

#[test]
fn packages_gradle_build_scripts_are_never_read() {
    let mut tree = Tree::new(&[
        ("settings.gradle.kts", "include(\":app\")\n"),
        ("build.gradle.kts", "not { read"),
        ("app/build.gradle", "not { read"),
        ("lib/build.gradle.kts", "not { read"),
    ]);
    // Even a build script the callback would call deleted or unreadable
    // counts by its presence in the file list.
    tree.missing.push("app/build.gradle");
    tree.unreadable.push("lib/build.gradle.kts");
    let (packages, calls) = tree.resolve_logged("gradle");
    assert_eq!(calls, ["settings.gradle.kts"]);
    let dirs: Vec<(&str, bool, Vec<ManifestKind>)> = packages
        .iter()
        .map(|p| {
            (
                &*p.dir,
                p.declared,
                p.manifests.iter().map(|m| m.kind).collect(),
            )
        })
        .collect();
    assert_eq!(
        dirs,
        [
            (
                "",
                false,
                vec![ManifestKind::GradleBuild, ManifestKind::GradleSettings]
            ),
            ("app", true, vec![ManifestKind::GradleBuild]),
            ("lib", false, vec![ManifestKind::GradleBuild]),
        ]
    );
    assert!(
        packages
            .iter()
            .flat_map(|p| p.manifests.iter())
            .all(|m| m.error.is_none() && !m.partial)
    );
}
