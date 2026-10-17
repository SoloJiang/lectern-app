//! Repository, worktree and sub-project model; manifest parsing.
//!
//! Implemented so far: manifest parsing ([`manifest`]), the file list
//! ([`FileList`]) and member resolution ([`resolve_packages`]), all pure
//! functions with no IO (docs/design/project.md「验收」step 1).

mod files;
pub mod manifest;
mod packages;

pub use files::{FileKind, FileList, FileListBuilder};
pub use manifest::{Ecosystem, ManifestError, ManifestKind};
pub use packages::{Manifest, ManifestResult, Package, RelPath, resolve_packages};
