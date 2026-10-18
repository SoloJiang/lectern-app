//! Candidate tiers, filters and order (「候选的分层与过滤」): the current
//! file, import targets, the same package, the same sub-project, the
//! sub-projects it depends on, the rest; within a tier the review set first,
//! then path distance, path bytes and line.

use std::collections::HashSet;

use lectern_services::code_intel::{Candidate, CandidateTier, TextOrigin};
use lectern_services::project::{BlobId, Package, RelPath};

use crate::extract::Visibility;
use crate::index::state::under;
use crate::resolve::{PackageScope, parent};

/// The deepest sub-project holding `path`, as its directory ("" for a
/// sub-project at the root).
pub(crate) fn deepest<'a>(packages: &'a [Package], path: &str) -> Option<&'a str> {
    packages
        .iter()
        .filter(|p| under(path, &p.dir))
        .max_by_key(|p| p.dir.len())
        .map(|p| &*p.dir)
}

/// The segments two paths have left once their common leading segments are
/// dropped, summed: 0 for the same path, 2 for two files of one directory.
pub(crate) fn distance(a: &str, b: &str) -> usize {
    // The segments left on both sides after the common prefix; no
    // allocation (the search sorts 100 000 paths by it).
    let (mut a, mut b) = (a.split('/'), b.split('/'));
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if x == y => {}
            (x, y) => {
                return usize::from(x.is_some()) + a.count() + usize::from(y.is_some()) + b.count();
            }
        }
    }
}

/// Whether a definition of `visibility` in `def_path` is visible from a
/// reference in `from` (「候选的分层与过滤」).
pub(crate) fn visible(visibility: Visibility, def_path: &str, from: &str) -> bool {
    match visibility {
        Visibility::Public | Visibility::Unknown => true,
        Visibility::File => def_path == from,
        Visibility::Package => parent(def_path) == parent(from),
        Visibility::Module => def_path == from || under(from, &module_dir(def_path)),
    }
}

/// The directory of a Rust module's child modules: `a.rs` → `a`;
/// `mod.rs`, `lib.rs`, `main.rs` → their directory.
fn module_dir(path: &str) -> String {
    let dir = parent(path);
    let file = path.rsplit('/').next().unwrap_or(path);
    match file {
        "mod.rs" | "lib.rs" | "main.rs" => dir.to_owned(),
        _ => {
            let stem = file.strip_suffix(".rs").unwrap_or(file);
            if dir.is_empty() {
                stem.to_owned()
            } else {
                format!("{dir}/{stem}")
            }
        }
    }
}

/// What a query ranks against: where the reference is.
pub(crate) struct Ranker<'a> {
    /// The reference's path (`Named`: the `near` path); None ranks every
    /// candidate `Elsewhere`.
    pub from: Option<&'a str>,
    /// The current text: (origin, OID) of `from`.
    pub current: Option<(TextOrigin, BlobId)>,
    pub packages: &'a [Package],
    /// "The same package" of `from`.
    pub package: Option<PackageScope>,
    /// The sub-projects `from`'s sub-project depends on.
    pub dependencies: Vec<RelPath>,
    pub review: HashSet<&'a str>,
}

impl Ranker<'_> {
    pub fn in_review(&self, path: &str) -> bool {
        self.review.contains(path)
    }

    /// The tier of a candidate at `path` with content (`origin`, `oid`).
    pub fn tier(
        &self,
        path: &str,
        origin: TextOrigin,
        oid: BlobId,
        imported: bool,
    ) -> CandidateTier {
        let Some(from) = self.from else {
            return CandidateTier::Elsewhere;
        };
        if path == from && self.current.is_none_or(|c| c == (origin, oid)) {
            return CandidateTier::CurrentFile;
        }
        if imported {
            return CandidateTier::Imported;
        }
        if self.package.as_ref().is_some_and(|p| p.contains(path)) {
            return CandidateTier::SamePackage;
        }
        let mine = deepest(self.packages, from);
        let theirs = deepest(self.packages, path);
        if mine == theirs {
            return CandidateTier::SameSubproject;
        }
        if theirs.is_some_and(|t| self.dependencies.iter().any(|d| **d == *t)) {
            return CandidateTier::DependentSubproject;
        }
        CandidateTier::Elsewhere
    }

    /// The order within one batch.
    pub fn sort(&self, candidates: &mut [Candidate]) {
        let from = self.from.unwrap_or("");
        candidates.sort_by(|a, b| {
            let key = |c: &Candidate| {
                let l = &c.definition.location;
                (
                    c.tier,
                    !c.in_review,
                    distance(from, &l.path),
                    l.path.clone(),
                    l.line,
                    l.column,
                )
            };
            key(a).cmp(&key(b))
        });
    }

    /// The order of reference files: the review set, the same sub-project,
    /// the sub-projects it depends on, the rest; then path distance.
    pub fn file_order(&self, path: &str) -> (u8, usize) {
        let from = self.from.unwrap_or("");
        let group = if self.in_review(path) {
            0
        } else {
            let mine = deepest(self.packages, from);
            let theirs = deepest(self.packages, path);
            if mine == theirs {
                1
            } else if theirs.is_some_and(|t| self.dependencies.iter().any(|d| **d == *t)) {
                2
            } else {
                3
            }
        };
        (group, distance(from, path))
    }
}
