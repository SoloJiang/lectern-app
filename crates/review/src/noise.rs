//! The noise list and rule grouping (docs/design/review.md「噪声文件与规则分组」),
//! and the path and mode checks of the `Sensitive` brief rules
//! (「简报规则」). Pure functions, any thread.
//!
//! Paths are `/`-separated raw bytes, compared case-sensitively. The file
//! name is the last segment; the directory segments are the ones before it.

use crate::types::changes::{FileChange, FileMode, FileSide, SideIdentity};
use crate::types::review::{GroupKind, NoiseKind, SensitiveKind};

#[cfg(test)]
mod tests;

/// The version of the pattern lists below. Goes into the hash of an analysis
/// request; bumped whenever a list changes.
pub const RULES_VERSION: u32 = 1;

const LOCKFILE_NAMES: &[&[u8]] = &[
    b"package-lock.json",
    b"npm-shrinkwrap.json",
    b"yarn.lock",
    b"pnpm-lock.yaml",
    b"bun.lock",
    b"bun.lockb",
    b"deno.lock",
    b"Cargo.lock",
    b"go.sum",
    b"go.work.sum",
    b"poetry.lock",
    b"Pipfile.lock",
    b"pdm.lock",
    b"uv.lock",
    b"composer.lock",
    b"Gemfile.lock",
    b"Podfile.lock",
    b"Package.resolved",
    b"pubspec.lock",
    b"mix.lock",
    b"flake.lock",
    b"gradle.lockfile",
    b"packages.lock.json",
];

const GENERATED_SUFFIXES: &[&[u8]] = &[
    b".min.js",
    b".min.css",
    b".js.map",
    b".css.map",
    b".d.ts.map",
    b".pb.go",
    b".pb.h",
    b".pb.cc",
    b"_pb2.py",
    b"_pb2.pyi",
    b"_pb2_grpc.py",
    b".g.dart",
    b".freezed.dart",
    b".designer.cs",
];
const GENERATED_INFIX: &[u8] = b".generated.";
const GENERATED_DIRS: &[&[u8]] = &[b"__generated__"];

const SNAPSHOT_SUFFIXES: &[&[u8]] = &[b".snap", b".snap.new", b".pending-snap", b".ambr"];
const SNAPSHOT_DIRS: &[&[u8]] = &[b"__snapshots__"];

const TEST_DIRS: &[&[u8]] = &[b"test", b"tests", b"__tests__", b"spec", b"testdata"];
const TEST_INFIXES: &[&[u8]] = &[b".test.", b".spec."];
const TEST_SUFFIXES: &[&[u8]] = &[
    b"_test.go",
    b"_test.py",
    b"Test.java",
    b"Tests.java",
    b"_spec.rb",
];

const DOC_SUFFIXES: &[&[u8]] = &[b".md", b".mdx", b".rst", b".adoc"];
const DOC_DIRS: &[&[u8]] = &[b"docs", b"doc"];
const DOC_PREFIXES: &[&[u8]] = &[b"LICENSE", b"CHANGELOG", b"README"];

const DEPS_NAMES: &[&[u8]] = &[
    b"package.json",
    b"Cargo.toml",
    b"go.mod",
    b"go.work",
    b"pom.xml",
    b"build.gradle",
    b"build.gradle.kts",
    b"settings.gradle",
    b"settings.gradle.kts",
    b"pyproject.toml",
    b"Gemfile",
    b"composer.json",
    b"pnpm-workspace.yaml",
];

const CONFIG_DIRS: &[&[u8]] = &[b".github", b".circleci", b".husky"];
const CONFIG_SUFFIXES: &[&[u8]] = &[b".yml", b".yaml", b".toml", b".ini", b".cfg", b".conf"];
const CONFIG_NAMES: &[&[u8]] = &[b"Makefile", b"justfile", b"Jenkinsfile"];

/// Which kind of noise file `path` is, if any. `generated` is
/// `FileChange::generated` (`linguist-generated`) and wins over the lists.
pub fn classify(path: &[u8], generated: bool) -> Option<NoiseKind> {
    if generated {
        return Some(NoiseKind::Generated);
    }
    let p = Parts::new(path);
    if LOCKFILE_NAMES.contains(&p.name) {
        Some(NoiseKind::Lockfile)
    } else if p.name_ends_with_any(GENERATED_SUFFIXES)
        || contains(p.name, GENERATED_INFIX)
        || p.has_dir_in(GENERATED_DIRS)
    {
        Some(NoiseKind::Generated)
    } else if p.name_ends_with_any(SNAPSHOT_SUFFIXES) || p.has_dir_in(SNAPSHOT_DIRS) {
        Some(NoiseKind::Snapshot)
    } else {
        None
    }
}

/// The rule group of `path`: the first of noise, tests, docs, deps, config
/// that matches, otherwise code.
pub fn group(path: &[u8], generated: bool) -> GroupKind {
    if let Some(kind) = classify(path, generated) {
        return GroupKind::Noise(kind);
    }
    let p = Parts::new(path);
    if is_test(&p) {
        GroupKind::Tests
    } else if is_docs(&p) {
        GroupKind::Docs
    } else if is_deps(&p) {
        GroupKind::Deps
    } else if is_config(&p) {
        GroupKind::Config
    } else {
        GroupKind::Code
    }
}

fn is_test(p: &Parts<'_>) -> bool {
    p.has_dir_in(TEST_DIRS)
        || TEST_INFIXES.iter().any(|i| contains(p.name, i))
        || p.name_ends_with_any(TEST_SUFFIXES)
        || (p.name.starts_with(b"test_") && p.name.ends_with(b".py"))
}

fn is_docs(p: &Parts<'_>) -> bool {
    p.name_ends_with_any(DOC_SUFFIXES)
        || p.has_dir_in(DOC_DIRS)
        || DOC_PREFIXES.iter().any(|x| p.name.starts_with(x))
}

fn is_deps(p: &Parts<'_>) -> bool {
    DEPS_NAMES.contains(&p.name)
        || (p.name.starts_with(b"requirements") && p.name.ends_with(b".txt"))
}

fn is_config(p: &Parts<'_>) -> bool {
    p.has_dir_in(CONFIG_DIRS)
        || p.name.starts_with(b".")
        || p.name_ends_with_any(CONFIG_SUFFIXES)
        || p.name.starts_with(b"Dockerfile")
        || CONFIG_NAMES.contains(&p.name)
        || contains(p.name, b".config.")
        || (p.name.starts_with(b"tsconfig") && p.name.ends_with(b".json"))
}

/// Agent instruction files matched by file name at any depth: agents also
/// load the ones in subdirectories.
const AGENT_NAMES: &[&[u8]] = &[b"CLAUDE.md", b"AGENTS.md", b"GEMINI.md"];
/// Matched as the whole path, from the worktree root.
const AGENT_PATHS: &[&[u8]] = &[b".mcp.json", b".github/copilot-instructions.md"];
/// Agent configuration directories, at any depth.
const AGENT_DIRS: &[&[u8]] = &[b".claude", b".codex", b".cursor"];

/// Path prefixes and whole paths from the worktree root.
const CI_PREFIXES: &[&[u8]] = &[b".github/workflows/", b".circleci/"];
const CI_PATHS: &[&[u8]] = &[b".gitlab-ci.yml", b"Jenkinsfile", b"azure-pipelines.yml"];
const HOOK_PREFIXES: &[&[u8]] = &[b".husky/", b".githooks/"];
const HOOK_PATHS: &[&[u8]] = &[b".pre-commit-config.yaml", b"lefthook.yml"];
/// `.gitattributes` applies in any directory; `.gitmodules` only at the root.
const GIT_NAMES: &[&[u8]] = &[b".gitattributes"];
const GIT_PATHS: &[&[u8]] = &[b".gitmodules"];

/// The `Sensitive` brief rule a change triggers, the first in the table's
/// order. The path rules look at the path and, for a rename, its source as
/// well; the mode rules at the two sides.
pub fn sensitive(change: &FileChange) -> Option<SensitiveKind> {
    let from = match &change.status {
        crate::types::changes::ChangeStatus::Renamed { from, .. } => Some(&from.0[..]),
        _ => None,
    };
    let paths = std::iter::once(&change.path.0[..]).chain(from);
    let any = |rule: fn(&[u8]) -> bool| paths.clone().any(rule);
    if any(is_agent_instructions) {
        return Some(SensitiveKind::AgentInstructions);
    }
    if any(|p| CI_PREFIXES.iter().any(|x| p.starts_with(x)) || CI_PATHS.contains(&p)) {
        return Some(SensitiveKind::Ci);
    }
    if any(|p| HOOK_PREFIXES.iter().any(|x| p.starts_with(x)) || HOOK_PATHS.contains(&p)) {
        return Some(SensitiveKind::Hooks);
    }
    if any(|p| GIT_NAMES.contains(&Parts::new(p).name) || GIT_PATHS.contains(&p)) {
        return Some(SensitiveKind::GitMetadata);
    }
    let new = change.new.as_ref()?;
    let old = change.old.as_ref();
    let unchanged = |mode: FileMode| old.is_some_and(|o| o.mode == mode && same_content(o, new));
    match new.mode {
        // Added, or its target changed.
        FileMode::Symlink if !unchanged(FileMode::Symlink) => Some(SensitiveKind::Symlink),
        FileMode::Executable if old.is_none_or(|o| o.mode != FileMode::Executable) => {
            Some(SensitiveKind::Executable)
        }
        // Added, or it points at another commit.
        FileMode::Gitlink if !unchanged(FileMode::Gitlink) => Some(SensitiveKind::Submodule),
        _ => None,
    }
}

fn is_agent_instructions(path: &[u8]) -> bool {
    let p = Parts::new(path);
    AGENT_NAMES.contains(&p.name) || AGENT_PATHS.contains(&path) || p.has_dir_in(AGENT_DIRS)
}

/// The same content on both sides. A blob kept and the same blob not kept
/// are the same content.
fn same_content(a: &FileSide, b: &FileSide) -> bool {
    match (&a.content, &b.content) {
        (
            SideIdentity::Blob(x) | SideIdentity::NotRetained(x),
            SideIdentity::Blob(y) | SideIdentity::NotRetained(y),
        ) => x == y,
        (x, y) => x == y,
    }
}

/// A path split into its directory part and file name.
#[derive(Clone, Copy)]
struct Parts<'a> {
    /// Everything before the last `/`; empty at the root.
    dirs: &'a [u8],
    name: &'a [u8],
}

impl<'a> Parts<'a> {
    fn new(path: &'a [u8]) -> Parts<'a> {
        match path.iter().rposition(|&b| b == b'/') {
            Some(i) => Parts {
                dirs: &path[..i],
                name: &path[i + 1..],
            },
            None => Parts {
                dirs: &[],
                name: path,
            },
        }
    }

    fn has_dir_in(&self, names: &[&[u8]]) -> bool {
        !self.dirs.is_empty() && self.dirs.split(|&b| b == b'/').any(|d| names.contains(&d))
    }

    fn name_ends_with_any(&self, suffixes: &[&[u8]]) -> bool {
        suffixes.iter().any(|s| self.name.ends_with(s))
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
