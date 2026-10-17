//! Repository format check: `core.repositoryformatversion` and
//! `extensions.*`, read from the common directory's own config file before
//! gix opens anything (gix 0.88 doesn't reject unknown extensions).

use std::path::Path;
use std::sync::Arc;

use crate::{GitError, UnsupportedRepo};

/// Extensions git honours in format version 0 too (`handle_extension_v0`).
const V0_EXTENSIONS: &[&str] = &["noop", "preciousobjects", "partialclone", "worktreeconfig"];

/// Extensions only valid in version 1 that this crate can read.
/// `relativeworktrees` makes the worktree `gitdir` files relative, which
/// `worktrees` resolves; `compatobjectformat` only adds a mapping that
/// reading the main format ignores.
const V1_EXTENSIONS: &[&str] = &[
    "noop-v1",
    "objectformat",
    "compatobjectformat",
    "refstorage",
    "relativeworktrees",
];

pub(crate) fn check(common_dir: &Path) -> Result<(), GitError> {
    let path = common_dir.join("config");
    // No config file: format version 0 without extensions, as git reads it.
    let Some(bytes) = crate::gitdir::read(&path)? else {
        return Ok(());
    };
    let config = gix::config::File::from_bytes_owned(
        &mut bytes.clone(),
        gix::config::file::Metadata::from(gix::config::Source::Local),
        Default::default(),
    )
    .map_err(|err| GitError::Corrupt(format!("{}: {err}", path.display())))?;
    check_config(&config)
}

fn check_config(config: &gix::config::File) -> Result<(), GitError> {
    let version = match config.integer("core.repositoryformatversion") {
        Ok(None) => 0,
        Ok(Some(v)) if (0..=1).contains(&v) => v,
        Ok(Some(v)) if v > 1 => {
            return Err(GitError::Unsupported(UnsupportedRepo::FormatVersion(v)));
        }
        Ok(Some(v)) => {
            return Err(GitError::Corrupt(format!(
                "core.repositoryformatversion = {v}"
            )));
        }
        Err(err) => {
            return Err(GitError::Corrupt(format!(
                "core.repositoryformatversion: {err}"
            )));
        }
    };
    let Some(sections) = config.sections_by_name("extensions") else {
        return Ok(());
    };
    for section in sections {
        if section.header().subsection_name().is_some() {
            continue;
        }
        for key in section.value_names() {
            let name = key.to_ascii_lowercase();
            let value = section
                .value(&key)
                .map(|v| String::from_utf8_lossy(&v).trim().to_ascii_lowercase());
            check_extension(version, &name, value.as_deref())?;
        }
    }
    Ok(())
}

fn check_extension(version: i64, name: &str, value: Option<&str>) -> Result<(), GitError> {
    let unsupported = || {
        GitError::Unsupported(UnsupportedRepo::Extension {
            name: name.into(),
            value: value.map(Arc::from),
        })
    };
    if V0_EXTENSIONS.contains(&name) {
        return Ok(());
    }
    // Reftable is refused in any version: gix would otherwise misread refs.
    if name == "refstorage" {
        return match value {
            Some("reftable") => Err(GitError::Unsupported(UnsupportedRepo::Reftable)),
            Some("files") if version == 1 => Ok(()),
            _ => Err(unsupported()),
        };
    }
    if version == 0 {
        // git dies on a v1-only extension in a v0 repository and ignores
        // the ones it doesn't know.
        return if V1_EXTENSIONS.contains(&name) {
            Err(unsupported())
        } else {
            Ok(())
        };
    }
    match name {
        "objectformat" | "compatobjectformat" => match value {
            Some("sha1" | "sha256") => Ok(()),
            _ => Err(unsupported()),
        },
        _ if V1_EXTENSIONS.contains(&name) => Ok(()),
        _ => Err(unsupported()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_text(text: &str) -> Result<(), GitError> {
        let config = gix::config::File::from_bytes_owned(
            &mut text.as_bytes().to_vec(),
            gix::config::file::Metadata::from(gix::config::Source::Local),
            Default::default(),
        )
        .unwrap();
        check_config(&config)
    }

    fn unsupported(text: &str) -> UnsupportedRepo {
        match check_text(text) {
            Err(GitError::Unsupported(why)) => why,
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn repo_format_accepts_known_extensions() {
        check_text("").unwrap();
        check_text("[core]\nrepositoryformatversion = 0\n").unwrap();
        check_text("[core]\nrepositoryformatversion = 0\n[extensions]\nworktreeConfig = true\nunknownThing = 1\n")
            .unwrap();
        check_text("[core]\nrepositoryformatversion = 1\n[extensions]\nobjectFormat = sha256\n")
            .unwrap();
        check_text("[core]\nrepositoryformatversion = 1\n[extensions]\nrefStorage = files\nrelativeWorktrees = true\n")
            .unwrap();
        check_text("[core]\nrepositoryformatversion = 1\n[extensions]\npartialClone = origin\n")
            .unwrap();
    }

    #[test]
    fn repo_format_refuses_what_it_cannot_read() {
        assert_eq!(
            unsupported(
                "[core]\nrepositoryformatversion = 1\n[extensions]\nrefstorage = reftable\n"
            ),
            UnsupportedRepo::Reftable
        );
        assert_eq!(
            unsupported("[core]\nrepositoryformatversion = 2\n"),
            UnsupportedRepo::FormatVersion(2)
        );
        assert_eq!(
            unsupported("[core]\nrepositoryformatversion = 1\n[extensions]\nsomethingNew = yes\n"),
            UnsupportedRepo::Extension {
                name: "somethingnew".into(),
                value: Some("yes".into())
            }
        );
        assert_eq!(
            unsupported(
                "[core]\nrepositoryformatversion = 1\n[extensions]\nobjectFormat = sha512\n"
            ),
            UnsupportedRepo::Extension {
                name: "objectformat".into(),
                value: Some("sha512".into())
            }
        );
        // git refuses v1-only extensions in a v0 repository.
        assert_eq!(
            unsupported(
                "[core]\nrepositoryformatversion = 0\n[extensions]\nobjectFormat = sha256\n"
            ),
            UnsupportedRepo::Extension {
                name: "objectformat".into(),
                value: Some("sha256".into())
            }
        );
        assert!(matches!(
            check_text("[core]\nrepositoryformatversion = -1\n"),
            Err(GitError::Corrupt(_))
        ));
    }
}
