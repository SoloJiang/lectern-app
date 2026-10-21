//! The resolution extension point (docs/design/code_intel.md「解析扩展点」):
//! language rules behind one trait, for jump step 2, candidate tiers 2, 3
//! and 5, the callers and tier 2's seeds. #17 has only the empty rules
//! ([`DefaultRules`]); #18 (Java) and #19 (TS / JS) add implementations in
//! this crate.

use std::sync::Arc;

use lectern_services::code_intel::{Binding, Candidate, DefinitionRef, DepOrigin};
use lectern_services::project::{BlobId, FileList, Package, RelPath};

use crate::LangName;
use crate::deps::DepFile;
use crate::extract::Extraction;
use crate::map::LiveMap;
use crate::site::{ParsedFile, Site};

/// A dependency file's extraction and the OID of the bytes read.
pub(crate) type DepExtract<'a> = &'a dyn Fn(&DepTarget) -> Option<(Arc<Extraction>, BlobId)>;

/// What a resolver reads: the query's snapshot. It never does IO itself;
/// `extraction` gives a path's extraction (a cache hit or extracted now),
/// `resolve_dep` and `extract_dep` a bare module name's dependency file and
/// its extraction ([`ResolveContext::dependency`]). #17's empty rules read
/// none of it; #18 and #19 do.
#[allow(dead_code, reason = "read by the resolvers of #18 and #19")]
pub(crate) struct ResolveContext<'a> {
    pub map: &'a LiveMap,
    pub files: &'a FileList,
    pub packages: Option<&'a [Package]>,
    pub extraction: &'a dyn Fn(&str) -> Option<Extraction>,
    pub resolve_dep: &'a dyn Fn(&str, &str) -> Option<DepTarget>,
    pub extract_dep: DepExtract<'a>,
}

#[allow(
    dead_code,
    reason = "called by #19's TS resolver (and the tests' resolver)"
)]
impl ResolveContext<'_> {
    /// The dependency file a bare module name (`react`, `@types/node`,
    /// `lodash/fp`) resolves to from `subproject` (relative to the worktree
    /// root): the package by the dependency tier's lookup rules, its
    /// declaration entry (or the subpath's file), read only through the
    /// session's dependency roots (ADR 0033). Blocking (manifests, stats);
    /// in a query's task only. None when not found, outside the roots or of
    /// an extension not allowed.
    pub(crate) fn dependency(&self, specifier: &str, subproject: &str) -> Option<DepTarget> {
        (self.resolve_dep)(specifier, subproject)
    }

    /// A dependency file's extraction and the OID of the bytes read: a
    /// `code_intel.deps` unit when stored, else read through its root and
    /// extracted now (the interactive deadline, the background size limit),
    /// queued to be written there.
    pub(crate) fn dependency_extraction(
        &self,
        target: &DepTarget,
    ) -> Option<(Arc<Extraction>, BlobId)> {
        (self.extract_dep)(target)
    }
}

/// A file in a dependency root (ADR 0033): its package, and the file built
/// by the session's `Roots`.
#[derive(Clone, Debug)]
pub(crate) struct DepTarget {
    pub origin: Arc<DepOrigin>,
    pub file: DepFile,
}

/// Where an import goes.
#[allow(dead_code, reason = "the variants are #18's and #19's")]
pub(crate) enum ImportTarget {
    /// A file of the worktree (a relative import, tsconfig `paths`, a
    /// workspace package).
    Worktree(RelPath),
    /// A bare module name resolved into a dependency root
    /// ([`ResolveContext::dependency`]).
    Dependency(DepTarget),
}

/// The names one import brings in.
#[allow(dead_code, reason = "the variants are #18's and #19's")]
pub(crate) enum ImportedNames {
    /// Every exported name (`import *` without a namespace, Go's dot import).
    All,
    /// (local name, exported name) pairs.
    Named(Vec<(Box<str>, Box<str>)>),
    /// `import * as ns`: members of `ns`.
    Namespace(Box<str>),
}

/// One import of the current file.
pub(crate) struct Import {
    pub target: ImportTarget,
    pub names: ImportedNames,
}

impl Import {
    /// The exported name `local` stands for through this import, if any.
    pub(crate) fn exported(&self, local: &str) -> Option<Box<str>> {
        match &self.names {
            ImportedNames::All => Some(local.into()),
            ImportedNames::Named(pairs) => pairs
                .iter()
                .find(|(l, _)| &**l == local)
                .map(|(_, e)| e.clone()),
            ImportedNames::Namespace(_) => None,
        }
    }
}

/// A reference bound with evidence (a qualified name, a receiver type).
pub(crate) struct Bound {
    pub definition: DefinitionRef,
    pub binding: Binding,
}

/// What "the same package" covers.
pub(crate) struct PackageScope {
    pub dirs: Vec<RelPath>,
}

impl PackageScope {
    pub(crate) fn contains(&self, path: &str) -> bool {
        let dir = parent(path);
        self.dirs.iter().any(|d| **d == *dir)
    }
}

/// A candidate before the list is final, for `disambiguate` (#18 compares
/// the arity).
#[allow(dead_code, reason = "read by #18's disambiguation")]
pub(crate) struct CandidateDraft {
    pub candidate: Candidate,
    pub arity: Option<u8>,
}

/// One language's resolution rules.
pub(crate) trait Resolver: Send + Sync {
    fn languages(&self) -> &[LangName];
    /// The current file's imports and the names each brings in.
    fn imports(&self, file: &ParsedFile, ctx: &ResolveContext<'_>) -> Vec<Import>;
    /// A reference's binding with evidence; None without any.
    fn bind(&self, site: &Site, file: &ParsedFile, ctx: &ResolveContext<'_>) -> Option<Bound>;
    /// "The same package"; None for the default (the same directory).
    fn package_of(&self, path: &str, ctx: &ResolveContext<'_>) -> Option<PackageScope>;
    /// The sub-projects one depends on (tier 5 candidates).
    fn dependencies(&self, subproject: &str, ctx: &ResolveContext<'_>) -> Vec<RelPath>;
    /// Keeps the candidates the site's evidence allows; by default all.
    fn disambiguate(&self, site: &Site, candidates: &mut Vec<CandidateDraft>);
}

/// #17's rules for every language: no imports, no bindings, the same
/// directory is the package, no dependencies, every candidate kept.
pub(crate) struct DefaultRules;

impl Resolver for DefaultRules {
    fn languages(&self) -> &[LangName] {
        &[]
    }

    fn imports(&self, _file: &ParsedFile, _ctx: &ResolveContext<'_>) -> Vec<Import> {
        Vec::new()
    }

    fn bind(&self, _site: &Site, _file: &ParsedFile, _ctx: &ResolveContext<'_>) -> Option<Bound> {
        None
    }

    fn package_of(&self, path: &str, _ctx: &ResolveContext<'_>) -> Option<PackageScope> {
        Some(default_package(path))
    }

    fn dependencies(&self, _subproject: &str, _ctx: &ResolveContext<'_>) -> Vec<RelPath> {
        Vec::new()
    }

    fn disambiguate(&self, _site: &Site, _candidates: &mut Vec<CandidateDraft>) {}
}

/// The default "same package": the same directory.
pub(crate) fn default_package(path: &str) -> PackageScope {
    PackageScope {
        dirs: vec![parent(path).into()],
    }
}

/// The directory of a path ("" at the root).
pub(crate) fn parent(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}
