//! Turns the snapshot's values into interface text and the commands the views
//! send (docs/design/worktrees.md「界面」「命令与事件」). Every sentence comes from
//! `keys::worktrees::*`; branch names, paths, titles and MR titles are shown as
//! written. Nothing here reads a clock: the relative times count back from the
//! snapshot's `built_at`.

use std::time::Duration;

use lectern_i18n::{Args, Strings, count_compact, keys};
use lectern_kernel::Name;
use lectern_services::changes::BaseSpec;
use lectern_services::project::WorktreeKey;
use lectern_services::review::{ChangeCount, ReviewTarget};
use lectern_services::session::{Agent, SessionKey, Timestamp};
use lectern_services::workspace::{Route, Target, WorkspaceCommand, screens};
use lectern_ui::Text;

use crate::snapshot::{HeadLabel, RowUsage, WorktreeRow, WorktreesSnapshot};

/// The separator of the parts of a row's second line.
pub(crate) const SEPARATOR: &str = "\u{00B7}";

/// `ChangeCount::Capped` is more than this many changed paths (ADR 0035).
const CAPPED_LIMIT: u32 = 4000;

/// The branch label and the small text after it: a commit checked out
/// without a branch is `detached`, a branch without commits `no commits`.
pub(crate) fn head(strings: &Strings, head: &HeadLabel) -> (Text, Option<Text>) {
    match head {
        HeadLabel::Branch(name) => (Text::verbatim(name.clone()), None),
        HeadLabel::Detached(commit) => (
            Text::verbatim(commit.clone()),
            Some(Text::key(strings, keys::worktrees::DETACHED)),
        ),
        HeadLabel::Unborn(name) => (
            Text::verbatim(name.clone()),
            Some(Text::key(strings, keys::worktrees::UNBORN)),
        ),
        HeadLabel::Unknown => (Text::key(strings, keys::worktrees::HEAD_UNKNOWN), None),
    }
}

/// The labels, in the order they follow the branch: Current, Locked,
/// Missing. `which` is the badge's key in the probe's tree.
pub(crate) fn badges(strings: &Strings, row: &WorktreeRow) -> Vec<(&'static str, Text)> {
    let mut out = Vec::new();
    if row.is_active {
        out.push(("current", Text::key(strings, keys::worktrees::CURRENT)));
    }
    if row.locked {
        out.push(("locked", Text::key(strings, keys::worktrees::LOCKED)));
    }
    if row.prunable {
        out.push(("missing", Text::key(strings, keys::worktrees::MISSING)));
    }
    out
}

/// A part of the second line: text, or a count that has not arrived (a
/// skeleton once the wait is noticed, nothing before).
pub(crate) enum Part {
    Text(String),
    Pending,
}

/// The second line of a row: the sessions, then the two counts.
pub(crate) fn second_line(
    strings: &Strings,
    snapshot: &WorktreesSnapshot,
    row: &WorktreeRow,
) -> Vec<Part> {
    let mut parts = Vec::new();
    parts.push(Part::Text(if row.running > 0 {
        strings.format(
            keys::worktrees::RUNNING,
            &Args::new().num("count", row.running),
        )
    } else if let Some(latest) = row.recent.first() {
        relative(strings, snapshot.built_at, latest.last_activity)
    } else {
        strings.get(keys::worktrees::NO_SESSIONS).to_owned()
    }));
    match row.changes.uncommitted {
        ChangeCount::Pending => parts.push(Part::Pending),
        ChangeCount::Files(count) => parts.push(Part::Text(strings.format(
            keys::worktrees::UNCOMMITTED,
            &Args::new().num("count", count),
        ))),
        ChangeCount::Capped => parts.push(Part::Text(capped(strings))),
        ChangeCount::Unavailable => {}
    }
    match (row.changes.vs_base, &snapshot.base) {
        (ChangeCount::Pending, _) => parts.push(Part::Pending),
        (ChangeCount::Files(count), Some(base)) => parts.push(Part::Text(strings.format(
            keys::worktrees::VS_BASE,
            &Args::new().num("count", count).str("base", &base.short),
        ))),
        (ChangeCount::Capped, Some(_)) => parts.push(Part::Text(capped(strings))),
        _ => {}
    }
    parts
}

fn capped(strings: &Strings) -> String {
    strings.format(
        keys::worktrees::CAPPED,
        &Args::new().num("limit", CAPPED_LIMIT),
    )
}

/// How long ago `then` was, counted back from `now`.
pub(crate) fn relative(strings: &Strings, now: Timestamp, then: Timestamp) -> String {
    let elapsed = now.0.saturating_sub(then.0).max(0);
    strings.relative_time(Duration::from_secs(elapsed.unsigned_abs() / 1000))
}

/// What the end of a row shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum End {
    /// Nothing: no `usage` plugin, or the default branch.
    Nothing,
    /// The usage is not computed yet.
    Pending,
    /// The tokens of the branch, or of the first MR, and how many more MRs.
    Tokens { total: u64, more: u32 },
}

pub(crate) fn end(row: &WorktreeRow) -> End {
    match &row.usage {
        RowUsage::Absent | RowUsage::NotCounted => End::Nothing,
        RowUsage::Pending => End::Pending,
        RowUsage::Branch(usage) => End::Tokens {
            total: usage.cumulative.total(),
            more: 0,
        },
        RowUsage::Mrs(items) => match items.first().and_then(|item| item.tokens) {
            Some(tokens) => End::Tokens {
                total: tokens.total(),
                more: u32::try_from(items.len().saturating_sub(1)).unwrap_or(u32::MAX),
            },
            None => End::Pending,
        },
    }
}

/// `12.3K tokens`.
pub(crate) fn tokens(strings: &Strings, total: u64) -> String {
    strings.format(
        keys::worktrees::TOKENS,
        &Args::new().str("tokens", &count_compact(total)),
    )
}

/// `+2 MR`.
pub(crate) fn more_mrs(strings: &Strings, more: u32) -> String {
    strings.format(keys::worktrees::MR_MORE, &Args::new().num("count", more))
}

/// The probe's attribute of a count: the number, or what it is instead.
pub(crate) fn count_attr(count: ChangeCount) -> String {
    match count {
        ChangeCount::Files(count) => count.to_string(),
        ChangeCount::Pending => "pending".to_owned(),
        ChangeCount::Capped => "capped".to_owned(),
        ChangeCount::Unavailable => "unavailable".to_owned(),
    }
}

/// The agent as `AgentDot` draws it; an agent of a third-party source has no
/// dot.
pub(crate) fn dot_agent(agent: &Agent) -> Option<lectern_ui::Agent> {
    match agent {
        Agent::ClaudeCode => Some(lectern_ui::Agent::ClaudeCode),
        Agent::Codex => Some(lectern_ui::Agent::Codex),
        Agent::Other(_) => None,
    }
}

/// The probe's `agent` attribute.
pub(crate) fn agent_attr(agent: &Agent) -> &'static str {
    match agent {
        Agent::ClaudeCode => "claude-code",
        Agent::Codex => "codex",
        Agent::Other(_) => "other",
    }
}

/// The review of the worktree's changes against the base, or against HEAD
/// when the repository has none (「命令与事件」), and the kind of target for the
/// log.
pub(crate) fn review_range(
    snapshot: &WorktreesSnapshot,
    worktree: &WorktreeKey,
) -> (WorkspaceCommand, &'static str) {
    let (base, target) = match &snapshot.base {
        Some(base) => (BaseSpec::MergeBase(base.full.clone()), "range-base"),
        None => (BaseSpec::Head, "range-head"),
    };
    let command = WorkspaceCommand::Push(Route {
        screen: Name::Borrowed(screens::REVIEW),
        target: Target::Review(ReviewTarget::Range {
            project: snapshot.project.clone(),
            worktree: worktree.clone(),
            base,
        }),
    });
    (command, target)
}

/// The review of one session.
pub(crate) fn review_session(key: SessionKey) -> WorkspaceCommand {
    WorkspaceCommand::Push(Route {
        screen: Name::Borrowed(screens::REVIEW),
        target: Target::Review(ReviewTarget::Session(key)),
    })
}
