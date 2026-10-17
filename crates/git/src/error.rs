//! Errors (docs/design/git.md「错误」).

use std::fmt;
use std::io;
use std::sync::Arc;

use crate::Oid;

#[derive(Debug)]
pub enum GitError {
    /// A repository format this crate can't read: reftable, an unknown
    /// `extensions.*`, `core.repositoryformatversion` above 1.
    Unsupported(UnsupportedRepo),
    NoMergeBase,
    ObjectMissing(Oid),
    TooLarge {
        size: u64,
    },
    TooManyEntries,
    Corrupt(String),
    Io(io::Error),
    Cancelled,
}

/// Why a repository is [`GitError::Unsupported`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UnsupportedRepo {
    /// `extensions.refStorage = reftable` (gix 0.88 reads only the files
    /// backend; ADR 0024「已知限制」).
    Reftable,
    /// `core.repositoryformatversion` above 1.
    FormatVersion(i64),
    /// An extension git would refuse to ignore and this crate doesn't know,
    /// or a known one with a value it can't read (e.g. an unknown
    /// `objectFormat`). Names are lowercase, as git compares them.
    Extension {
        name: Arc<str>,
        value: Option<Arc<str>>,
    },
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitError::Unsupported(why) => write!(f, "unsupported repository format: {why}"),
            GitError::NoMergeBase => f.write_str("no merge base"),
            GitError::ObjectMissing(id) => write!(f, "object {id} is missing"),
            GitError::TooLarge { size } => write!(f, "too large ({size} bytes)"),
            GitError::TooManyEntries => f.write_str("too many entries"),
            GitError::Corrupt(what) => write!(f, "corrupt repository data: {what}"),
            GitError::Io(err) => write!(f, "{err}"),
            GitError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl fmt::Display for UnsupportedRepo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnsupportedRepo::Reftable => f.write_str("reftable reference storage"),
            UnsupportedRepo::FormatVersion(v) => write!(f, "core.repositoryformatversion = {v}"),
            UnsupportedRepo::Extension {
                name,
                value: Some(value),
            } => {
                write!(f, "extensions.{name} = {value}")
            }
            UnsupportedRepo::Extension { name, value: None } => write!(f, "extensions.{name}"),
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GitError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for GitError {
    fn from(err: io::Error) -> Self {
        GitError::Io(err)
    }
}

impl GitError {
    /// Maps a gix error: I/O failures stay [`GitError::Io`] with their kind
    /// (the message keeps gix's context), everything else is
    /// [`GitError::Corrupt`].
    pub(crate) fn from_gix(err: gix::Error) -> GitError {
        let kind = err
            .iter_errors()
            .find_map(|e| e.downcast_ref::<io::Error>().map(io::Error::kind));
        match kind {
            Some(kind) => GitError::Io(io::Error::new(kind, format!("{err:#}"))),
            None => GitError::Corrupt(format!("{err:#}")),
        }
    }
}

/// `GitError::from_gix` for any error gix returns.
pub(crate) fn gix_err(err: impl Into<gix::Error>) -> GitError {
    GitError::from_gix(err.into())
}
