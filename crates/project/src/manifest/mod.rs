//! Manifest parsing: pure functions from a manifest's bytes to the members it
//! declares (docs/design/project.md「清单格式」, ADR 0023).
//!
//! Nothing here reads files, runs code or touches the network. Every parser
//! returns an error for input it cannot read instead of panicking.

use std::borrow::Cow;
use std::sync::Arc;

mod cargo;
mod go;
mod gradle;
mod package_json;
mod pnpm;
mod pom;

#[cfg(test)]
mod tests;

// The data types moved to `services` in step 2 (docs/design/project.md
// 「验收」); re-exported so `manifest::ManifestKind` and friends keep their
// paths.
pub use lectern_services::project::{Ecosystem, MAX_MANIFEST_BYTES, ManifestError, ManifestKind};

/// A member declaration, relative to the directory of the manifest that
/// declares it, exactly as written apart from quoting.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Member {
    /// A glob (npm, pnpm, Cargo `members`). Matches only candidate
    /// directories with a manifest of the same ecosystem.
    Glob(Box<str>),
    /// An explicit path (Maven, Gradle, go.work). Holds when the directory
    /// has tracked files. A Maven module may name a pom file instead of a
    /// directory. `name` is the name the declaration gives the member
    /// (the last segment of a Gradle project path).
    Path {
        path: Box<str>,
        name: Option<Arc<str>>,
    },
}

/// A pattern that removes members matched by the same manifest's globs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Exclude {
    /// pnpm (and npm) `!pattern`.
    Glob(Box<str>),
    /// Cargo `workspace.exclude`: the directory and everything below it.
    Path(Box<str>),
}

/// One manifest's parse result.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Parsed {
    /// package.json `name`, Cargo `package.name`, go.mod `module`, Maven
    /// `artifactId`, Gradle `rootProject.name`.
    pub name: Option<Arc<str>>,
    /// Maven `groupId`, or the parent's when the project has none.
    pub group: Option<Arc<str>>,
    /// The manifest declares a workspace (`workspaces`, `[workspace]`,
    /// `modules` / `subprojects`, a settings script, go.work,
    /// pnpm-workspace.yaml), even an empty one.
    pub workspace: bool,
    pub members: Vec<Member>,
    pub excludes: Vec<Exclude>,
    /// Some declarations use forms the parser does not evaluate (Gradle
    /// variables and loops, Maven properties, YAML anchors); the members may
    /// be incomplete.
    pub partial: bool,
}

/// Parses one manifest. Pure: no IO, callable from any thread.
pub fn parse(kind: ManifestKind, bytes: &[u8]) -> Result<Parsed, ManifestError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge);
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match kind {
        ManifestKind::PackageJson => package_json::parse(bytes),
        ManifestKind::PnpmWorkspace => pnpm::parse(&lossy(bytes)),
        ManifestKind::CargoToml => cargo::parse(strict(bytes)?),
        ManifestKind::PomXml => pom::parse(&lossy(bytes)),
        ManifestKind::GradleSettings => gradle::parse(&lossy(bytes)),
        // Only marks the directory as a Gradle project; never read.
        ManifestKind::GradleBuild => Ok(Parsed::default()),
        ManifestKind::GoMod => go::parse_mod(&lossy(bytes)),
        ManifestKind::GoWork => go::parse_work(&lossy(bytes)),
    }
}

/// UTF-8 text, or a syntax error on the line of the first invalid byte.
fn strict(bytes: &[u8]) -> Result<&str, ManifestError> {
    std::str::from_utf8(bytes).map_err(|e| syntax_at(bytes, e.valid_up_to()))
}

/// The hand-written parsers and pom.xml only look at ASCII syntax and
/// values, so a stray Latin-1 comment must not fail the whole file.
fn lossy(bytes: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

/// `Syntax` for the line containing byte `offset` (clamped to the input).
fn syntax_at(text: &[u8], offset: usize) -> ManifestError {
    let end = offset.min(text.len());
    let newlines = text[..end].iter().filter(|&&b| b == b'\n').count();
    ManifestError::Syntax {
        line: u32::try_from(newlines + 1).unwrap_or(u32::MAX),
    }
}

/// `Syntax` for a 1-based line number from a library error.
fn syntax_line(line: usize) -> ManifestError {
    ManifestError::Syntax {
        line: u32::try_from(line.max(1)).unwrap_or(u32::MAX),
    }
}

fn arc(s: &str) -> Arc<str> {
    Arc::from(s)
}
