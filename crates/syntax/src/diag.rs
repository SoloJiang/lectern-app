//! What the runtime log keeps of this crate's values
//! (docs/design/file_reader.md「日志与诊断」, docs/design/logging.md):
//! paths, language names and the kinds of errors. Never file contents, the
//! highlight query's text or an error's message: a `GitError::Corrupt` or a
//! tree-sitter `QueryError` carries text, an `io::Error` is written as its
//! kind and code only.

use std::io;
use std::path::Path;

use lectern_git::GitError;
use lectern_logging::Value;
use lectern_platform::SafeError;

use crate::lang::CORE_LANGUAGES;

/// A document's path, relative to its root, as a path field.
pub(crate) fn path(rel: &str) -> Value<'_> {
    Value::path(Path::new(rel))
}

/// A language name as a field: the core language's own `&'static str`.
/// None (written as null) for a name that is not a core language's, which
/// another crate's caller of `health` could pass.
pub(crate) fn lang(name: &str) -> Option<&'static str> {
    CORE_LANGUAGES
        .iter()
        .find(|spec| spec.name == name)
        .map(|spec| spec.name)
}

/// A tree-sitter query error's kind; its message (which quotes the query)
/// is not written.
pub(crate) fn query_error(kind: &tree_sitter::QueryErrorKind) -> &'static str {
    use tree_sitter::QueryErrorKind as K;
    match kind {
        K::Syntax => "syntax",
        K::NodeType => "node_type",
        K::Field => "field",
        K::Capture => "capture",
        K::Predicate => "predicate",
        K::Structure => "structure",
        K::Language => "language",
    }
}

/// A `GitError`'s kind, and its `io::Error` when it has one; never the
/// text of `Corrupt`.
pub(crate) fn git_error(e: &GitError) -> (&'static str, Option<Value<'static>>) {
    let kind = match e {
        GitError::Unsupported(_) => "unsupported",
        GitError::NoMergeBase => "no_merge_base",
        GitError::ObjectMissing(_) => "object_missing",
        GitError::TooLarge { .. } => "too_large",
        GitError::TooManyEntries => "too_many_entries",
        GitError::Corrupt(_) => "corrupt",
        GitError::Io(_) => "io",
        GitError::Cancelled => "cancelled",
    };
    let io = match e {
        GitError::Io(io) => Some(Value::io(io)),
        _ => None,
    };
    (kind, io)
}

/// Why the file on disk could not be read, as a log record keeps it: the
/// step that failed and its `io::Error` when there is one.
#[derive(Debug)]
pub(crate) struct Failure {
    /// `root` (opening the root), `path` (not a valid relative path),
    /// `open`, `read`.
    pub op: &'static str,
    /// `not_found`, `path_through_symlink`, `invalid_path` or `io`.
    pub error: &'static str,
    pub io: Option<io::Error>,
}

impl Failure {
    pub(crate) fn io(op: &'static str, e: io::Error) -> Failure {
        Failure {
            op,
            error: "io",
            io: Some(e),
        }
    }

    pub(crate) fn safe(op: &'static str, e: SafeError) -> Failure {
        match e {
            SafeError::NotFound => Failure {
                op,
                error: "not_found",
                io: None,
            },
            SafeError::PathThroughSymlink => Failure {
                op,
                error: "path_through_symlink",
                io: None,
            },
            SafeError::Io(e) => Failure::io(op, e),
        }
    }

    pub(crate) fn invalid_path() -> Failure {
        Failure {
            op: "path",
            error: "invalid_path",
            io: None,
        }
    }

    /// The `io` field.
    pub(crate) fn io_value(&self) -> Option<Value<'static>> {
        self.io.as_ref().map(Value::io)
    }
}
