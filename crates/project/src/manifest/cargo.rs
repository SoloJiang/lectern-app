//! Cargo.toml: `package.name`, `workspace.members` (globs) and
//! `workspace.exclude` (paths). Every other table is skipped.

use serde::Deserialize;

use super::{Exclude, ManifestError, Member, Parsed, arc, syntax_at};

#[derive(Deserialize)]
struct CargoToml {
    package: Option<Package>,
    workspace: Option<Workspace>,
}

#[derive(Deserialize)]
struct Package {
    name: Option<String>,
}

#[derive(Deserialize)]
struct Workspace {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

pub(super) fn parse(text: &str) -> Result<Parsed, ManifestError> {
    let toml: CargoToml = toml::from_str(text).map_err(|e| {
        let offset = e.span().map_or(0, |span| span.start);
        syntax_at(text.as_bytes(), offset)
    })?;
    let mut parsed = Parsed {
        name: toml.package.and_then(|p| p.name).as_deref().map(arc),
        ..Parsed::default()
    };
    if let Some(workspace) = toml.workspace {
        parsed.workspace = true;
        parsed.members = workspace
            .members
            .into_iter()
            .map(|m| Member::Glob(m.into()))
            .collect();
        parsed.excludes = workspace
            .exclude
            .into_iter()
            .map(|e| Exclude::Path(e.into()))
            .collect();
    }
    Ok(parsed)
}
