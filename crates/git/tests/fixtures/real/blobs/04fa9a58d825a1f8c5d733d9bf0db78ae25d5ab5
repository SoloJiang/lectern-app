//! Repository, worktree and sub-project model; manifest parsing.
//!
//! Implemented so far: manifest parsing ([`manifest`]) and member resolution
//! ([`resolve_packages`]), pure functions with no IO (docs/design/project.md
//! 「验收」step 1). The data types they use are defined with the `Projects`
//! service in `lectern_services::project` (step 2) and re-exported here.

pub mod manifest;
mod packages;

pub use lectern_services::project::{
    Ecosystem, FileKind, FileList, FileListBuilder, Manifest, ManifestError, ManifestKind, Package,
    RelPath,
};
pub use packages::{ManifestResult, resolve_packages};
