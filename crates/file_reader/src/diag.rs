//! What the runtime log keeps of this crate's values
//! (docs/design/file_reader.md「日志与诊断」): the kind of a request and of
//! an origin, and the reason a document cannot be edited. Never a path, a file's content or a position in it.

use lectern_services::code_intel::TextOrigin;
use lectern_syntax::NotEditable;

/// The kind of an origin, as a field.
pub(crate) fn origin(origin: &TextOrigin) -> &'static str {
    match origin {
        TextOrigin::Worktree => "worktree",
        TextOrigin::Object => "object",
        TextOrigin::Dependency(_) => "dependency",
    }
}

/// Why a document cannot be edited, as a field.
pub(crate) fn not_editable(reason: NotEditable) -> &'static str {
    match reason {
        NotEditable::NotLoaded => "not_loaded",
        NotEditable::TooLarge => "too_large",
        NotEditable::LongLine => "long_line",
        NotEditable::NotText => "not_text",
        NotEditable::MixedLineEndings => "mixed_line_endings",
        NotEditable::ReadOnlySource => "read_only_source",
        NotEditable::Reloading => "reloading",
    }
}
