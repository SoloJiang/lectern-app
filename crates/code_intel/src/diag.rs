//! What the runtime log keeps of this crate's values
//! (docs/design/code_intel.md「日志与诊断」, docs/design/logging.md): paths,
//! core language names, grammar versions, counts and the kinds of errors.
//! Never file contents, the names and identifiers taken from them, search or
//! query text, the queries' own text or an error's message: a tree-sitter
//! query error quotes the query, `GitError::Corrupt` carries text, an
//! `io::Error` is written as its kind and code only.

use std::path::Path;

use lectern_git::GitError;
use lectern_logging::Value;
use lectern_services::project::ProjectKey;
use tree_sitter::{QueryError, QueryErrorKind};
use tree_sitter_tags::Error as TagsError;

use crate::lang::{CORE_LANGUAGES, Lang};

/// A path relative to a worktree or package root, as a path field.
pub(crate) fn path(rel: &str) -> Value<'_> {
    Value::path(Path::new(rel))
}

/// A project's key as a path field: the git common directory, or the
/// folder's root.
pub(crate) fn project(key: &ProjectKey) -> Value<'_> {
    match key {
        ProjectKey::Git { common_dir } => Value::path(common_dir),
        ProjectKey::Folder { root } => Value::path(root),
    }
}

/// A language's name as a field: the core language's own `&'static str`;
/// None (null) for a language pack, whose name is not written (the
/// `grammar` field, its version, tells packs apart).
pub(crate) fn lang(lang: &Lang) -> Option<&'static str> {
    if !lang.lease().is_static() {
        return None;
    }
    CORE_LANGUAGES
        .iter()
        .find(|spec| *spec.name == *lang.name().0)
        .map(|spec| spec.name)
}

/// A language's grammar version as a field.
pub(crate) fn grammar(lang: &Lang) -> Value<'static> {
    Value::hex(lang.grammar_version())
}

/// A `GitError`'s kind, and its `io::Error` when it has one; never the
/// text of `Corrupt` or a path in `ObjectMissing`.
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

/// Why a language's queries did not compile, as a log record keeps it: the
/// kind and, for a query error, its position in the query. The message is
/// not kept (it quotes the query).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompileError {
    /// A query error's kind (`syntax`, `node_type`, `field`, `capture`,
    /// `predicate`, `structure`, `language`), or `regex`,
    /// `invalid_language`, `invalid_capture` from tree-sitter-tags,
    /// `unknown_marker` (a `reference.lectern.*` capture the extractor does
    /// not know) or `syntax_type` (tree-sitter-tags numbered the kinds
    /// otherwise).
    pub kind: &'static str,
    /// The query error's row and column; None for the other kinds.
    pub row: Option<usize>,
    pub column: Option<usize>,
}

impl CompileError {
    pub(crate) fn other(kind: &'static str) -> CompileError {
        CompileError {
            kind,
            row: None,
            column: None,
        }
    }
}

impl From<QueryError> for CompileError {
    fn from(e: QueryError) -> CompileError {
        CompileError {
            kind: query_error(&e.kind),
            row: Some(e.row),
            column: Some(e.column),
        }
    }
}

impl From<TagsError> for CompileError {
    fn from(e: TagsError) -> CompileError {
        match e {
            TagsError::Query(e) => e.into(),
            other => CompileError::other(tags_error(&other)),
        }
    }
}

fn query_error(kind: &QueryErrorKind) -> &'static str {
    use QueryErrorKind as K;
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

/// A tree-sitter-tags error's kind; never its message (a capture's name, a
/// regex, the query).
pub(crate) fn tags_error(e: &TagsError) -> &'static str {
    match e {
        TagsError::Query(e) => query_error(&e.kind),
        TagsError::Regex(_) => "regex",
        TagsError::Cancelled => "cancelled",
        TagsError::InvalidLanguage(_) => "invalid_language",
        TagsError::InvalidCapture(_) => "invalid_capture",
    }
}
