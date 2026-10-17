//! package.json: `name` and `workspaces` (npm 7+, yarn, bun; yarn 1's
//! `{ "packages": [...] }` object form). Every other field is skipped.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::{Exclude, ManifestError, Member, Parsed, arc, syntax_line};

#[derive(Deserialize)]
struct PackageJson {
    name: Option<String>,
    workspaces: Option<Workspaces>,
}

/// `["a/*", ...]` or `{ "packages": ["a/*"], "nohoist": [...] }`.
struct Workspaces(Vec<String>);

impl<'de> Deserialize<'de> for Workspaces {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(WorkspacesVisitor)
    }
}

struct WorkspacesVisitor;

impl<'de> Visitor<'de> for WorkspacesVisitor {
    type Value = Workspaces;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array of globs or an object with `packages`")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Workspaces, A::Error> {
        let mut globs = Vec::new();
        while let Some(glob) = seq.next_element::<String>()? {
            globs.push(glob);
        }
        Ok(Workspaces(globs))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Workspaces, A::Error> {
        let mut globs = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            if key == "packages" {
                globs = map.next_value::<Vec<String>>()?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Workspaces(globs))
    }
}

pub(super) fn parse(bytes: &[u8]) -> Result<Parsed, ManifestError> {
    let json: PackageJson = serde_json::from_slice(bytes).map_err(|e| syntax_line(e.line()))?;
    let mut parsed = Parsed {
        name: json.name.as_deref().map(arc),
        ..Parsed::default()
    };
    if let Some(Workspaces(globs)) = json.workspaces {
        parsed.workspace = true;
        for glob in globs {
            match glob.strip_prefix('!') {
                Some(exclude) => parsed.excludes.push(Exclude::Glob(exclude.into())),
                None => parsed.members.push(Member::Glob(glob.into())),
            }
        }
    }
    Ok(parsed)
}
