//! What the runtime log keeps of this crate's values
//! (docs/design/file_reader.md「日志与诊断」): the kind of a request and of
//! an origin. Never a path, a file's content or a position in it.

use lectern_services::code_intel::TextOrigin;

/// The kind of an origin, as a field.
pub(crate) fn origin(origin: &TextOrigin) -> &'static str {
    match origin {
        TextOrigin::Worktree => "worktree",
        TextOrigin::Object => "object",
        TextOrigin::Dependency(_) => "dependency",
    }
}
