//! Member resolution: from a file list and the parsed manifests to the list
//! of sub-projects (docs/design/project.md「成员解析」). Pure: no IO.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use glob::{MatchOptions, Pattern};

use crate::files::{FileKind, FileList};
use crate::manifest::{Exclude, ManifestError, ManifestKind, Member, Parsed};

#[cfg(test)]
mod tests;

/// A path inside a worktree, `/` separated, no leading or trailing `/`;
/// the root is "".
pub type RelPath = Arc<str>;

/// What `resolve_packages` gets for one manifest: its parse result (shared
/// with the parse cache) or why there is none.
pub type ManifestResult = Result<Arc<Parsed>, ManifestError>;

/// One directory's sub-project. Several manifests in one directory (say
/// package.json and pom.xml) make one package.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Package {
    pub dir: RelPath,
    /// The first of: package.json `name`, Cargo `package.name`, go.mod
    /// `module`, Maven `artifactId`, the Gradle project name; else the
    /// directory name (the project name for the root).
    pub name: Arc<str>,
    pub manifests: Arc<[Manifest]>,
    /// Declared a member by some workspace manifest.
    pub declared: bool,
    /// This directory has a manifest that declares a workspace.
    pub workspace_root: bool,
    /// Index of the nearest enclosing package in the list.
    pub parent: Option<u32>,
    /// Tracked files under the directory, including nested packages'.
    pub files: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Manifest {
    pub kind: ManifestKind,
    pub path: RelPath,
    pub name: Option<Arc<str>>,
    /// Uses forms the parser does not evaluate, or declares a member
    /// pattern that is not a valid glob; members may be missing.
    pub partial: bool,
    pub error: Option<ManifestError>,
}

/// Path segments whose undeclared manifests are test data, not packages.
const SAMPLE_SEGMENTS: &[&str] = &[
    "node_modules",
    "vendor",
    "fixtures",
    "__fixtures__",
    "testdata",
];

/// Resolves the sub-projects of one worktree.
///
/// `manifest(path)` gives the parse result of a manifest in `files`: None
/// when the file is gone (not a manifest), `Some(Err)` when it could not be
/// read or parsed (the directory is still a package; its members are not
/// expanded). `root_name` names the root package when its manifests give
/// no name.
///
/// The result is sorted by `dir` (bytes); `parent` indexes into it.
pub fn resolve_packages(
    files: &FileList,
    root_name: &str,
    manifest: &dyn Fn(&str) -> Option<ManifestResult>,
) -> Vec<Package> {
    let candidates = candidates(files, manifest);
    let mut dirs: BTreeMap<&str, Entry> = BTreeMap::new();
    for (index, c) in candidates.iter().enumerate() {
        dirs.entry(c.dir).or_default().manifests.push(index);
    }

    // Directories with each kind of manifest, for glob members.
    let mut by_kind: HashMap<ManifestKind, Vec<&str>> = HashMap::new();
    for c in &candidates {
        by_kind.entry(c.kind).or_default().push(c.dir);
    }

    let mut invalid_glob = vec![false; candidates.len()];
    let mut declared: Vec<(String, Option<Arc<str>>)> = Vec::new();
    for (index, c) in candidates.iter().enumerate() {
        let Ok(parsed) = &c.result else { continue };
        invalid_glob[index] = declare(c, parsed, files, &by_kind, &mut declared);
    }
    for (dir, name) in &declared {
        let entry = dirs.entry(dir.as_str()).or_default();
        entry.declared = true;
        if entry.declared_name.is_none() {
            entry.declared_name.clone_from(name);
        }
    }

    let mut packages: Vec<Package> = Vec::with_capacity(dirs.len());
    for (dir, entry) in dirs {
        if !entry.declared && dir.split('/').any(|s| SAMPLE_SEGMENTS.contains(&s)) {
            continue;
        }
        let manifests: Vec<Manifest> = entry
            .manifests
            .iter()
            .map(|&i| {
                let c = &candidates[i];
                let (name, partial, error) = match &c.result {
                    Ok(p) => (p.name.clone(), p.partial || invalid_glob[i], None),
                    Err(e) => (None, false, Some(*e)),
                };
                Manifest {
                    kind: c.kind,
                    path: Arc::from(c.path),
                    name,
                    partial,
                    error,
                }
            })
            .collect();
        let workspace_root = entry
            .manifests
            .iter()
            .any(|&i| candidates[i].result.as_ref().is_ok_and(|p| p.workspace));
        let name = package_name(&manifests, entry.declared_name.as_ref(), dir, root_name);
        let range = files.range_under(dir);
        packages.push(Package {
            dir: Arc::from(dir),
            name,
            manifests: manifests.into(),
            declared: entry.declared,
            workspace_root,
            parent: None,
            files: u32::try_from(range.len()).unwrap_or(u32::MAX),
        });
    }

    // Nearest enclosing package: look up each ancestor directory. (A stack
    // over the byte-sorted list does not work: "a-b" sorts between "a" and
    // "a/c".)
    let index: HashMap<&str, u32> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (&*p.dir, u32::try_from(i).unwrap_or(u32::MAX)))
        .collect();
    let parents: Vec<Option<u32>> = packages
        .iter()
        .map(|p| ancestors(&p.dir).find_map(|a| index.get(a).copied()))
        .collect();
    for (p, parent) in packages.iter_mut().zip(parents) {
        p.parent = parent;
    }
    packages
}

#[derive(Default)]
struct Entry {
    /// Indexes into the candidates, in path order.
    manifests: Vec<usize>,
    declared: bool,
    declared_name: Option<Arc<str>>,
}

/// A manifest file in the list.
struct Candidate<'a> {
    dir: &'a str,
    path: &'a str,
    kind: ManifestKind,
    result: ManifestResult,
}

fn candidates<'a>(
    files: &'a FileList,
    manifest: &dyn Fn(&str) -> Option<ManifestResult>,
) -> Vec<Candidate<'a>> {
    let mut out = Vec::new();
    for (path, kind) in files.iter() {
        // Symlinks and submodules are not manifests.
        if !matches!(kind, FileKind::File | FileKind::Executable) {
            continue;
        }
        let (dir, file_name) = path.rsplit_once('/').unwrap_or(("", path));
        let Some(kind) = ManifestKind::from_file_name(file_name) else {
            continue;
        };
        let Some(result) = manifest(path) else {
            continue;
        };
        out.push(Candidate {
            dir,
            path,
            kind,
            result,
        });
    }
    out
}

/// Expands one manifest's members into `declared`. Returns true when some
/// member or exclude pattern was not a valid glob.
fn declare(
    c: &Candidate<'_>,
    parsed: &Parsed,
    files: &FileList,
    by_kind: &HashMap<ManifestKind, Vec<&str>>,
    declared: &mut Vec<(String, Option<Arc<str>>)>,
) -> bool {
    let mut invalid = false;
    // npm and pnpm globs match package.json directories, Cargo's Cargo.toml
    // directories.
    let (target, options) = match c.kind {
        ManifestKind::PackageJson | ManifestKind::PnpmWorkspace => {
            (ManifestKind::PackageJson, npm_options())
        }
        ManifestKind::CargoToml => (ManifestKind::CargoToml, cargo_options()),
        _ => (c.kind, cargo_options()),
    };
    let excludes: Vec<DirMatcher> = parsed
        .excludes
        .iter()
        .filter_map(|e| {
            let m = match e {
                Exclude::Glob(g) => DirMatcher::glob(c.dir, g, options),
                Exclude::Path(p) => join(c.dir, p).map(|p| Ok(DirMatcher::Prefix(p))),
            };
            m.and_then(|m| m.map_err(|()| invalid = true).ok())
        })
        .collect();

    for member in &parsed.members {
        match member {
            Member::Glob(glob) => {
                let Some(matcher) = DirMatcher::glob(c.dir, glob, options) else {
                    continue; // outside the worktree
                };
                let Ok(matcher) = matcher else {
                    invalid = true;
                    continue;
                };
                // A member written without wildcards wins over an exclude
                // (as in Cargo); a wildcard match does not.
                let explicit = matches!(matcher, DirMatcher::Exact(_));
                for &dir in by_kind.get(&target).map_or(&[][..], Vec::as_slice) {
                    if matcher.matches(dir)
                        && (explicit || !excludes.iter().any(|e| e.matches(dir)))
                    {
                        declared.push((dir.to_owned(), None));
                    }
                }
            }
            Member::Path { path, name } => {
                let Some(mut dir) = join(c.dir, path) else {
                    continue;
                };
                // A Maven module may name a pom file rather than a directory.
                if matches!(
                    files.find(&dir),
                    Some(FileKind::File | FileKind::Executable)
                ) {
                    dir = dir
                        .rsplit_once('/')
                        .map_or_else(String::new, |(d, _)| d.to_owned());
                }
                if !files.range_under(&dir).is_empty() {
                    declared.push((dir, name.clone()));
                }
            }
        }
    }
    invalid
}

/// Matches directories against a member or exclude pattern.
enum DirMatcher {
    /// No wildcards: the directory itself.
    Exact(String),
    /// Cargo `exclude`: the directory and everything below it.
    Prefix(String),
    Glob(Pattern, MatchOptions),
}

impl DirMatcher {
    /// None when the pattern leaves the worktree, Some(Err) when it is not a
    /// valid glob.
    fn glob(base: &str, glob: &str, options: MatchOptions) -> Option<Result<Self, ()>> {
        let joined = join(base, glob)?;
        if !joined.contains(['*', '?', '[']) {
            return Some(Ok(Self::Exact(joined)));
        }
        Some(
            Pattern::new(&joined)
                .map(|p| Self::Glob(p, options))
                .map_err(|_| ()),
        )
    }

    fn matches(&self, dir: &str) -> bool {
        match self {
            Self::Exact(d) => d == dir,
            Self::Prefix(p) => {
                p.is_empty()
                    || dir == p
                    || dir
                        .strip_prefix(p.as_str())
                        .is_some_and(|r| r.starts_with('/'))
            }
            // `*` matches the empty string, but a workspace's wildcards
            // never make the worktree root a member of itself.
            Self::Glob(p, options) => !dir.is_empty() && p.matches_with(dir, *options),
        }
    }
}

/// `*` does not cross `/`; `**` matches any number of directories.
fn cargo_options() -> MatchOptions {
    MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    }
}

/// As Cargo, but wildcards skip dot directories (npm and pnpm do).
fn npm_options() -> MatchOptions {
    MatchOptions {
        require_literal_leading_dot: true,
        ..cargo_options()
    }
}

/// `rel` relative to `base`, with `.` and empty segments dropped and `..`
/// folded literally. None for absolute paths and paths that leave the
/// worktree.
fn join(base: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') || rel.starts_with('\\') || rel.as_bytes().get(1) == Some(&b':') {
        return None;
    }
    let mut segments: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for s in rel.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            s => segments.push(s),
        }
    }
    Some(segments.join("/"))
}

/// The ancestors of `dir`, nearest first, ending with the root "".
fn ancestors(dir: &str) -> impl Iterator<Item = &str> {
    let mut rest = (!dir.is_empty()).then_some(dir);
    std::iter::from_fn(move || {
        let current = rest?;
        let parent = current.rsplit_once('/').map_or("", |(p, _)| p);
        rest = (!parent.is_empty()).then_some(parent);
        Some(parent)
    })
}

fn package_name(
    manifests: &[Manifest],
    declared: Option<&Arc<str>>,
    dir: &str,
    root_name: &str,
) -> Arc<str> {
    const ORDER: [ManifestKind; 5] = [
        ManifestKind::PackageJson,
        ManifestKind::CargoToml,
        ManifestKind::GoMod,
        ManifestKind::PomXml,
        ManifestKind::GradleSettings,
    ];
    ORDER
        .iter()
        .find_map(|&kind| {
            manifests
                .iter()
                .find(|m| m.kind == kind)
                .and_then(|m| m.name.clone())
        })
        .or_else(|| declared.cloned())
        .unwrap_or_else(|| {
            if dir.is_empty() {
                Arc::from(root_name)
            } else {
                Arc::from(dir.rsplit('/').next().unwrap_or(dir))
            }
        })
}
