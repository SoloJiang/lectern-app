//! Sub-projects and their manifests (docs/design/project.md「子项目与清单」).
//! The parsers and member resolution that produce them are in `project`.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::RelPath;

/// Manifests larger than this are not parsed (`ManifestError::TooLarge`).
/// Also the read limit for `project`'s `GitAccess::read_worktree_file`.
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// One directory's sub-project. Several manifests in one directory (say
/// package.json and pom.xml) make one package.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
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

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub kind: ManifestKind,
    pub path: RelPath,
    pub name: Option<Arc<str>>,
    /// Uses forms the parser does not evaluate, or declares a member
    /// pattern that is not a valid glob; members may be missing.
    pub partial: bool,
    pub error: Option<ManifestError>,
}

/// The manifest file formats `project` understands.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum ManifestKind {
    PackageJson,
    PnpmWorkspace,
    CargoToml,
    PomXml,
    GradleSettings,
    GradleBuild,
    GoMod,
    GoWork,
}

/// The build ecosystem a manifest belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Ecosystem {
    Npm,
    Cargo,
    Maven,
    Gradle,
    Go,
}

impl ManifestKind {
    pub fn ecosystem(self) -> Ecosystem {
        match self {
            Self::PackageJson | Self::PnpmWorkspace => Ecosystem::Npm,
            Self::CargoToml => Ecosystem::Cargo,
            Self::PomXml => Ecosystem::Maven,
            Self::GradleSettings | Self::GradleBuild => Ecosystem::Gradle,
            Self::GoMod | Self::GoWork => Ecosystem::Go,
        }
    }

    /// The kind of a manifest by its file name (the last path segment), or
    /// None when the file is not a manifest.
    pub fn from_file_name(name: &str) -> Option<Self> {
        Some(match name {
            "package.json" => Self::PackageJson,
            "pnpm-workspace.yaml" => Self::PnpmWorkspace,
            "Cargo.toml" => Self::CargoToml,
            "pom.xml" => Self::PomXml,
            "settings.gradle" | "settings.gradle.kts" => Self::GradleSettings,
            "build.gradle" | "build.gradle.kts" => Self::GradleBuild,
            "go.mod" => Self::GoMod,
            "go.work" => Self::GoWork,
            _ => return None,
        })
    }
}

/// Why a manifest has no parse result.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ManifestError {
    /// Larger than `MAX_MANIFEST_BYTES`; not parsed.
    TooLarge,
    /// The input could not be parsed; `line` is 1-based.
    Syntax { line: u32 },
    /// Reading failed, or the path is not a regular file (a symlink, say).
    Unreadable,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "manifest is larger than {MAX_MANIFEST_BYTES} bytes"),
            Self::Syntax { line } => write!(f, "manifest syntax error on line {line}"),
            Self::Unreadable => f.write_str("manifest is unreadable"),
        }
    }
}

impl std::error::Error for ManifestError {}
