//! What the runtime log keeps of this crate's values (docs/design/worktrees.md
//! 「日志与诊断」): counts, generations, indices and discriminant names. Never a
//! branch or reference name, a root or git directory, an overlap path, a
//! session title, a project or MR title, or a token count.

/// A length as a log field.
pub(crate) fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
