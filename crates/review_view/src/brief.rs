//! The brief's lines (docs/design/review_view.md「简报」): which message and
//! which arguments each rule of `Brief::items` takes. A pure function of the
//! brief; the view only draws what comes out.

use lectern_i18n::{Args, FmtKey, Strings, keys};
use lectern_services::project::WorktreeKey;
use lectern_services::review::{
    Brief, BriefItem, BriefRule, FileKey, IncompleteKind, LineAnchor, NoiseKind, SensitiveKind,
};
use lectern_services::usage::{RowKey, SessionRow, SessionUsage};
use lectern_ui::Text;

/// One entry of the brief, ready to draw.
#[derive(Clone, Debug)]
pub struct BriefLine {
    /// `<rule>:<path>`, or `<rule>` when the entry has no file: the key of
    /// the entry's probe node.
    pub id: String,
    pub text: Text,
    pub critical: bool,
    pub file: Option<FileKey>,
    pub line: Option<LineAnchor>,
}

/// The `Provided` rules the view has a message for: (provider, rule) and the
/// message. `i18n` has no lookup by string, only compile-time keys, so the
/// known pairs are listed here; an unknown pair is shown with the rule as
/// written (`review-brief-provided`). The two rules of `code_intel` join when
/// its design names them (docs/design/review_view.md「对其他 crate 的要求」E).
pub const PROVIDED: &[(&str, &str, FmtKey)] = &[];

/// A file key as text: the new path, the old path for a deletion. Not valid
/// UTF-8 shows lossily. For display and node IDs only.
pub fn file_text(file: &FileKey) -> String {
    let path = file.new.as_ref().or(file.old.as_ref());
    match path {
        Some(path) => String::from_utf8_lossy(&path.0).into_owned(),
        None => String::new(),
    }
}

/// The lines of the brief, in the snapshot's order.
pub fn lines(brief: &Brief, strings: &Strings) -> Vec<BriefLine> {
    brief
        .items
        .iter()
        .map(|item| line_of(item, strings))
        .collect()
}

fn line_of(item: &BriefItem, strings: &Strings) -> BriefLine {
    let path = item.file.as_ref().map(file_text).unwrap_or_default();
    let with_path = |key: FmtKey| Text::fmt(strings, key, &Args::new().str("path", &path));
    let with_count = |key: FmtKey| Text::fmt(strings, key, &Args::new().num("count", item.count));
    let text = match &item.rule {
        BriefRule::ChangedSinceSeen => with_path(keys::review::BRIEF_CHANGED_SINCE_SEEN),
        BriefRule::Drifted => with_path(keys::review::BRIEF_DRIFTED),
        BriefRule::EditFailed => with_path(keys::review::BRIEF_EDIT_FAILED),
        BriefRule::CommandOnly => with_count(keys::review::BRIEF_COMMAND_ONLY),
        BriefRule::Sensitive(kind) => with_path(sensitive_key(*kind)),
        BriefRule::Deleted => with_count(keys::review::BRIEF_DELETED),
        BriefRule::Incomplete(kind) => with_count(incomplete_key(*kind)),
        BriefRule::Noise(kind) => with_count(noise_key(*kind)),
        BriefRule::Provided {
            provider,
            rule,
            args,
        } => provided_text(strings, provider, rule, args),
    };
    BriefLine {
        id: if path.is_empty() {
            rule_name(&item.rule)
        } else {
            format!("{}:{path}", rule_name(&item.rule))
        },
        text,
        critical: item.critical,
        file: item.file.clone(),
        line: item.line,
    }
}

fn provided_text(
    strings: &Strings,
    provider: &str,
    rule: &str,
    args: &[(std::sync::Arc<str>, std::sync::Arc<str>)],
) -> Text {
    let known = PROVIDED
        .iter()
        .find(|(p, r, _)| *p == provider && *r == rule);
    match known {
        Some((_, _, key)) => {
            let mut built = Args::new();
            for (name, value) in args.iter().take(8) {
                built = built.str(name, value);
            }
            Text::fmt(strings, *key, &built)
        }
        None => Text::fmt(
            strings,
            keys::review::BRIEF_PROVIDED,
            &Args::new().str("rule", rule),
        ),
    }
}

fn sensitive_key(kind: SensitiveKind) -> FmtKey {
    match kind {
        SensitiveKind::AgentInstructions => keys::review::BRIEF_SENSITIVE_AGENT_INSTRUCTIONS,
        SensitiveKind::Ci => keys::review::BRIEF_SENSITIVE_CI,
        SensitiveKind::Hooks => keys::review::BRIEF_SENSITIVE_HOOKS,
        SensitiveKind::GitMetadata => keys::review::BRIEF_SENSITIVE_GIT_METADATA,
        SensitiveKind::Symlink => keys::review::BRIEF_SENSITIVE_SYMLINK,
        SensitiveKind::Executable => keys::review::BRIEF_SENSITIVE_EXECUTABLE,
        SensitiveKind::Submodule => keys::review::BRIEF_SENSITIVE_SUBMODULE,
    }
}

/// The message for a file shown without a full diff, with the count of
/// files (the brief's) …
pub fn incomplete_key(kind: IncompleteKind) -> FmtKey {
    match kind {
        IncompleteKind::Truncated => keys::review::BRIEF_INCOMPLETE_TRUNCATED,
        IncompleteKind::TooLarge => keys::review::BRIEF_INCOMPLETE_TOO_LARGE,
        IncompleteKind::Binary => keys::review::BRIEF_INCOMPLETE_BINARY,
        IncompleteKind::NotRetained => keys::review::BRIEF_INCOMPLETE_NOT_RETAINED,
        IncompleteKind::Filtered => keys::review::BRIEF_INCOMPLETE_FILTERED,
        IncompleteKind::Unavailable => keys::review::BRIEF_INCOMPLETE_UNAVAILABLE,
        IncompleteKind::Listed => keys::review::BRIEF_INCOMPLETE_LISTED,
    }
}

/// … and the same message for one file, which the step title's note uses.
pub fn incomplete_text(strings: &Strings, kind: IncompleteKind) -> Text {
    Text::fmt(strings, incomplete_key(kind), &Args::new().num("count", 1))
}

fn noise_key(kind: NoiseKind) -> FmtKey {
    match kind {
        NoiseKind::Lockfile => keys::review::BRIEF_NOISE_LOCKFILE,
        NoiseKind::Generated => keys::review::BRIEF_NOISE_GENERATED,
        NoiseKind::Snapshot => keys::review::BRIEF_NOISE_SNAPSHOT,
    }
}

/// A rule's name for node IDs: lower case, dashes.
pub fn rule_name(rule: &BriefRule) -> String {
    match rule {
        BriefRule::ChangedSinceSeen => "changed-since-seen".into(),
        BriefRule::Drifted => "drifted".into(),
        BriefRule::EditFailed => "edit-failed".into(),
        BriefRule::CommandOnly => "command-only".into(),
        BriefRule::Sensitive(kind) => format!(
            "sensitive-{}",
            match kind {
                SensitiveKind::AgentInstructions => "agent-instructions",
                SensitiveKind::Ci => "ci",
                SensitiveKind::Hooks => "hooks",
                SensitiveKind::GitMetadata => "git-metadata",
                SensitiveKind::Symlink => "symlink",
                SensitiveKind::Executable => "executable",
                SensitiveKind::Submodule => "submodule",
            }
        ),
        BriefRule::Deleted => "deleted".into(),
        BriefRule::Incomplete(kind) => format!(
            "incomplete-{}",
            match kind {
                IncompleteKind::Truncated => "truncated",
                IncompleteKind::TooLarge => "too-large",
                IncompleteKind::Binary => "binary",
                IncompleteKind::NotRetained => "not-retained",
                IncompleteKind::Filtered => "filtered",
                IncompleteKind::Unavailable => "unavailable",
                IncompleteKind::Listed => "listed",
            }
        ),
        BriefRule::Noise(kind) => format!(
            "noise-{}",
            match kind {
                NoiseKind::Lockfile => "lockfile",
                NoiseKind::Generated => "generated",
                NoiseKind::Snapshot => "snapshot",
            }
        ),
        BriefRule::Provided { provider, rule, .. } => format!("{provider}-{rule}"),
    }
}

/// One branch (or MR) of the session's usage, ready to draw
/// (docs/design/review_view.md「简报」).
#[derive(Clone, Debug)]
pub struct UsageLine {
    /// The branch or MR title: the key of the entry's probe node.
    pub id: String,
    /// `{branch} · {tokens} tokens · {percent}% this session`.
    pub text: Text,
    /// `shared with n other MR(s)`, when the row is shared.
    pub shared: Option<Text>,
    /// The cumulative tokens of the branch.
    pub tokens: u64,
    /// The session's share of them, in whole percent.
    pub percent: u32,
}

/// The usage rows of a session: one per branch (per MR), the rows of
/// `worktree` (the one under review) first, the others in the order
/// `usage` gave them.
pub fn usage_lines(
    usage: &SessionUsage,
    worktree: Option<&WorktreeKey>,
    strings: &Strings,
) -> Vec<UsageLine> {
    let (mut own, mut other): (Vec<&SessionRow>, Vec<&SessionRow>) = usage
        .rows
        .iter()
        .partition(|row| worktree.is_some() && row.worktree.as_ref() == worktree);
    own.append(&mut other);
    own.into_iter()
        .map(|row| usage_line(row, strings))
        .collect()
}

fn usage_line(row: &SessionRow, strings: &Strings) -> UsageLine {
    let name: String = match &row.key {
        RowKey::Branch { branch, .. } => branch_name(&branch.name),
        RowKey::Mrs { mrs, branch } => mrs
            .first()
            .map_or_else(|| branch_name(&branch.name), |mr| mr.title.to_string()),
        RowKey::Unassigned { .. } => strings.get(keys::review::BRIEF_USAGE_UNASSIGNED).to_owned(),
    };
    let total = row.cumulative.total();
    let percent = if total == 0 {
        0
    } else {
        u32::try_from(u128::from(row.session_part.total()) * 100 / u128::from(total))
            .unwrap_or(100)
            .min(100)
    };
    let compact = lectern_i18n::count_compact(total);
    let text = Text::fmt(
        strings,
        keys::review::BRIEF_USAGE_ROW,
        &Args::new()
            .str("branch", &name)
            .str("tokens", &compact)
            .num("percent", percent),
    );
    let shared = row.shared.then(|| {
        let others = match &row.key {
            RowKey::Mrs { mrs, .. } => mrs.len().saturating_sub(1).max(1),
            _ => 1,
        };
        Text::fmt(
            strings,
            keys::review::BRIEF_USAGE_SHARED,
            &Args::new().num("count", u32::try_from(others).unwrap_or(u32::MAX)),
        )
    });
    UsageLine {
        id: name,
        text,
        shared,
        tokens: total,
        percent,
    }
}

/// A branch as the interface names it: without `refs/heads/`.
fn branch_name(name: &str) -> String {
    name.strip_prefix("refs/heads/").unwrap_or(name).to_owned()
}

/// The tokens of a turn as the title says them (`12.3K tokens`).
pub fn turn_tokens_text(strings: &Strings, tokens: u64) -> Text {
    Text::fmt(
        strings,
        keys::review::TURN_TOKENS,
        &Args::new().str("tokens", &lectern_i18n::count_compact(tokens)),
    )
}
