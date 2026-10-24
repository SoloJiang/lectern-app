//! What the runtime log keeps of this crate's values (docs/design/review_view.md
//! 「日志与诊断」): counts, ids, generations and discriminant names. Never a
//! path, a title, a prompt, a message of the agent, a command, a tool name, a
//! branch, a line of a diff or the text of an Analyze reason.

use lectern_services::changes::ChangeError;
use lectern_services::review::{ReviewCommand, ReviewError, ReviewTarget};

/// A length as a log field.
pub(crate) fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The kind of review, for `review screen opened`.
pub(crate) fn target_kind(target: &ReviewTarget) -> &'static str {
    match target {
        ReviewTarget::Session(_) => "session",
        ReviewTarget::Outside { .. } => "outside",
        ReviewTarget::Range { .. } => "range",
    }
}

/// The kind of a failure, for `review failed shown`.
pub(crate) fn error_kind(error: &ReviewError) -> &'static str {
    match error {
        ReviewError::Missing => "missing",
        ReviewError::NotInOpenProject => "not_in_open_project",
        ReviewError::Changes(error) => change_error_kind(error),
    }
}

/// The kind of a change-set error, without its reason text.
pub(crate) fn change_error_kind(error: &ChangeError) -> &'static str {
    match error {
        ChangeError::Unsupported(_) => "unsupported",
        ChangeError::NoMergeBase => "no_merge_base",
        ChangeError::TooManyEntries => "too_many_entries",
        ChangeError::Io(_) => "io",
        ChangeError::Cancelled => "cancelled",
    }
}

/// The name of a command, for the `command` record.
pub(crate) fn command_kind(command: &ReviewCommand) -> &'static str {
    match command {
        ReviewCommand::MarkSeen(_) => "mark_seen",
        ReviewCommand::MarkUnseen(_) => "mark_unseen",
        ReviewCommand::LooksGood => "looks_good",
        ReviewCommand::Prepare(_) => "prepare",
        ReviewCommand::Refresh => "refresh",
        ReviewCommand::Analyze => "analyze",
        ReviewCommand::CancelAnalyze => "cancel_analyze",
    }
}
