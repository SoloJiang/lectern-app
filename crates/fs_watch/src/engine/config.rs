//! Expanding the config list into watch points (docs/design/fs_watch.md
//! 「配置文件清单」). Runs in the load task on the executor: it `lstat`s
//! every prefix of every path.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// At most this many symbolic links are resolved along one path; beyond
/// that the path counts as missing.
const MAX_LINKS: usize = 40;

/// Where the config list's watch points are: each point is an entry name in
/// a directory, whose events mean "the config list may have changed".
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct ConfigPlan {
    /// Points under the common directory, relative to it; matched against
    /// the common directory target's events.
    pub(crate) common: BTreeSet<Vec<u8>>,
    /// Points under the worktree root, relative to it; matched in step 2 of
    /// the classification, before filtering.
    pub(crate) root: BTreeSet<Vec<u8>>,
    /// The other points, by directory: one `Entries` target each.
    pub(crate) entries: BTreeMap<PathBuf, BTreeSet<OsString>>,
}

impl ConfigPlan {
    /// Builds the plan for `files`, relative to the given common directory
    /// and worktree root (each compared both as given and canonicalized).
    pub(crate) fn build(files: &[PathBuf], common: &Path, root: &Path) -> ConfigPlan {
        let commons = with_canonical(common);
        let roots = with_canonical(root);
        let mut points = BTreeSet::new();
        for file in files.iter().filter(|file| file.is_absolute()) {
            expand(file, &mut points);
        }
        let mut plan = ConfigPlan::default();
        for (dir, name) in points {
            let full = dir.join(&name);
            if let Some(rel) = relative(&full, &commons) {
                plan.common.insert(rel);
            } else if let Some(rel) = relative(&full, &roots) {
                plan.root.insert(rel);
            } else {
                plan.entries.entry(dir).or_default().insert(name);
            }
        }
        plan
    }

    /// Whether an event on `q` (relative to a `Tree` target) touches one of
    /// `points`: the point itself, a directory on the way to it, or a path
    /// below it (a missing directory that appeared).
    pub(crate) fn matches(points: &BTreeSet<Vec<u8>>, q: &[u8]) -> bool {
        if points.is_empty() || q.is_empty() {
            return !points.is_empty();
        }
        if points.contains(q) {
            return true;
        }
        // Below a point: a missing directory on the way appeared.
        let mut end = q.len();
        while let Some(dir) = crate::path::parent(&q[..end]) {
            if points.contains(dir) {
                return true;
            }
            end = dir.len();
        }
        let mut prefix = q.to_vec();
        prefix.push(b'/');
        points
            .range(prefix.clone()..)
            .next()
            .is_some_and(|point| point.starts_with(&prefix))
    }
}

fn with_canonical(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    if let Ok(canonical) = fs::canonicalize(path)
        && canonical != path
    {
        out.push(canonical);
    }
    out
}

fn relative(full: &Path, bases: &[PathBuf]) -> Option<Vec<u8>> {
    bases.iter().find_map(|base| {
        let rel = full.strip_prefix(base).ok()?;
        let bytes = rel.as_os_str().as_encoded_bytes();
        (!bytes.is_empty()).then(|| bytes.to_vec())
    })
}

/// Walks `path` from the root, `lstat`ing every prefix: each symbolic link
/// on the way is a point (its parent's entry), resolved relative to its
/// directory, and the walk continues from its target; the resolved last
/// component is a point; at the first missing component the longest
/// existing (resolved) prefix is watched for that name.
pub(crate) fn expand(path: &Path, out: &mut BTreeSet<(PathBuf, OsString)>) {
    let mut cur = PathBuf::from("/");
    let mut rest = components(path);
    let mut links = 0;
    while let Some(name) = rest.pop_front() {
        if name == ".." {
            cur.pop();
            continue;
        }
        let next = cur.join(&name);
        match fs::symlink_metadata(&next) {
            Err(_) => {
                out.insert((cur, name));
                return;
            }
            Ok(meta) if meta.file_type().is_symlink() => {
                out.insert((cur.clone(), name));
                links += 1;
                if links > MAX_LINKS {
                    return;
                }
                let Ok(target) = fs::read_link(&next) else {
                    return;
                };
                if target.is_absolute() {
                    cur = PathBuf::from("/");
                }
                let mut resolved = components(&target);
                resolved.extend(rest.drain(..));
                rest = resolved;
            }
            Ok(_) => {
                if rest.is_empty() {
                    out.insert((cur.clone(), name));
                }
                cur = next;
            }
        }
    }
}

fn components(path: &Path) -> VecDeque<OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_os_string()),
            Component::ParentDir => Some(OsString::from("..")),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        })
        .collect()
}
