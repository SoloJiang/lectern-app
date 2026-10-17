//! Repository, worktree and sub-project model; manifest parsing
//! (docs/design/project.md).
//!
//! [`ProjectPlugin`] provides `dyn Projects` (`lectern_services::project`):
//! opening a path as a git project (every worktree of a repository is one
//! project) or a plain folder, the worktree list, the active worktree's file
//! list from the git index and its sub-projects, placing paths, and the
//! `WorkspaceFacts` that drive `Activation::Workspace`. Git is reached only
//! through `lectern-git`. Incremental refresh (step 4) is not implemented
//! yet: a project is scanned when opened, reopened or switched.
//!
//! [`manifest`] and [`resolve_packages`] are pure functions with no IO
//! (step 1). The data types are defined with the `Projects` service in
//! `lectern_services::project` (step 2) and re-exported here.

mod facts;
mod folder;
mod git_access;
pub mod manifest;
mod model;
mod packages;
mod plugin;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use facts::workspace_facts;
pub use lectern_services::project::{
    Ecosystem, FileKind, FileList, FileListBuilder, Manifest, ManifestError, ManifestKind, Package,
    RelPath,
};
pub use packages::{ManifestResult, resolve_packages};
pub use plugin::{ProjectDeps, ProjectPlugin};
