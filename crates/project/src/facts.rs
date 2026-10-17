//! `WorkspaceFacts` over the active trees of every open project
//! (docs/design/project.md「WorkspaceFacts」).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use glob::{MatchOptions, Pattern};

use lectern_kernel::WorkspaceFacts;
use lectern_services::project::{FileKind, FileList};

#[cfg(test)]
mod tests;

/// The facts of the union of `trees`. Builds the file name and extension
/// indexes here, so call it on the executor.
pub fn workspace_facts(trees: Vec<Arc<FileList>>) -> Arc<dyn WorkspaceFacts> {
    Arc::new(ProjectFacts::new(trees))
}

struct ProjectFacts {
    trees: Vec<Arc<FileList>>,
    /// The last segment of every file (submodules excluded).
    names: HashSet<Box<str>>,
    /// What follows the last `.` of every file name.
    exts: HashSet<Box<str>>,
    /// Results by pattern; one set of facts never computes a pattern twice.
    cache: Mutex<HashMap<Box<str>, bool>>,
}

impl ProjectFacts {
    fn new(trees: Vec<Arc<FileList>>) -> ProjectFacts {
        let mut names = HashSet::new();
        let mut exts = HashSet::new();
        for tree in &trees {
            for (path, _) in files(tree) {
                let name = file_name(path);
                if let Some((_, ext)) = name.rsplit_once('.') {
                    exts.insert(ext.into());
                }
                names.insert(name.into());
            }
        }
        ProjectFacts {
            trees,
            names,
            exts,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn compute(&self, pattern: &str) -> bool {
        // `**/x` is `x` when `x` is a file name: both match at any depth.
        let mut rest = pattern;
        while let Some(stripped) = rest.strip_prefix("**/") {
            rest = stripped;
        }
        if rest.is_empty() {
            return false;
        }
        if rest.contains('/') {
            // From the worktree root.
            let Ok(compiled) = Pattern::new(pattern) else {
                return false;
            };
            return self
                .trees
                .iter()
                .flat_map(|t| files(t))
                .any(|(path, _)| compiled.matches_with(path, OPTIONS));
        }
        let Ok(compiled) = Pattern::new(rest) else {
            return false;
        };
        if !has_meta(rest) {
            return self.names.contains(rest);
        }
        if let Some(ext) = rest.strip_prefix("*.")
            && !has_meta(ext)
            && !ext.contains('.')
        {
            return self.exts.contains(ext);
        }
        self.names
            .iter()
            .any(|name| compiled.matches_with(name, OPTIONS))
    }
}

impl WorkspaceFacts for ProjectFacts {
    fn contains(&self, pattern: &str) -> bool {
        if let Some(&hit) = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(pattern)
        {
            return hit;
        }
        let hit = self.compute(pattern);
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(pattern.into(), hit);
        hit
    }
}

/// `*` and `?` stay within one segment; `**` spans any number.
const OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// Files only: submodule entries are directories in the worktree.
fn files(tree: &FileList) -> impl Iterator<Item = (&str, FileKind)> + '_ {
    tree.iter().filter(|(_, kind)| *kind != FileKind::Submodule)
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn has_meta(s: &str) -> bool {
    s.contains(['*', '?', '[', ']'])
}
