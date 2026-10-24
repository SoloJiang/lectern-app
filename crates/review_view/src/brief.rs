//! The brief's lines (docs/design/review_view.md「简报」): which message and
//! which arguments each rule of `Brief::items` takes. A pure function of the
//! brief; the view only draws what comes out.

use lectern_i18n::{Args, FmtKey, Strings, keys};
use lectern_services::review::{
    Brief, BriefItem, BriefRule, FileKey, IncompleteKind, LineAnchor, NoiseKind, SensitiveKind,
};
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
