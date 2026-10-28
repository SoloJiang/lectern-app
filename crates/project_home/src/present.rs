//! Turns the snapshot's values into interface text and the few facts the views
//! share (docs/design/project_home.md「界面」). Every sentence comes from
//! `keys::project_home::*`; the user's own words (titles, agent names) are
//! shown as written. Nothing here reads a clock: the relative times count
//! back from the snapshot's `now`.

use std::ops::Range;
use std::time::Duration;

use lectern_i18n::{Args, Strings, keys};
use lectern_kernel::Name;
use lectern_services::project::{ProjectKey, RelPath};
use lectern_services::review::ReviewTarget;
use lectern_services::session::{Agent, SessionKey, Timestamp};
use lectern_services::workspace::{Route, Target, WorkspaceCommand, screens};
use lectern_ui::Text;

use crate::HomeSnapshot;
use crate::snapshot::{
    Card, MainModel, OutsideRow, SessionRow, SessionStatus, Signals, TightestLimit,
};

/// The separator of the lines of a card.
const SEPARATOR: &str = " · ";

const MINUTES_PER_DAY: u32 = 24 * 60;

/// The session's title, or the placeholder for one without.
pub(crate) fn title(strings: &Strings, row: &SessionRow) -> Text {
    match &row.title {
        Some(title) => Text::verbatim(title.clone()),
        None => Text::key(strings, keys::project_home::UNTITLED_SESSION),
    }
}

/// The agent as `AgentDot` draws it; an agent of a third-party source has no
/// dot (its name leads the secondary text instead).
pub(crate) fn dot_agent(agent: &Agent) -> Option<lectern_ui::Agent> {
    match agent {
        Agent::ClaudeCode => Some(lectern_ui::Agent::ClaudeCode),
        Agent::Codex => Some(lectern_ui::Agent::Codex),
        Agent::Other(_) => None,
    }
}

/// The agent's name: ours from the resource files, a third party's as written.
pub(crate) fn agent_name(strings: &Strings, agent: &Agent) -> String {
    match agent {
        Agent::ClaudeCode => strings
            .get(keys::project_home::AGENT_CLAUDE_CODE)
            .to_owned(),
        Agent::Codex => strings.get(keys::project_home::AGENT_CODEX).to_owned(),
        Agent::Other(name) => name.to_string(),
    }
}

/// How long ago the session moved.
pub(crate) fn relative(strings: &Strings, row: &SessionRow, now: Timestamp) -> String {
    let elapsed = now.0.saturating_sub(row.last_activity.0).max(0);
    strings.relative_time(Duration::from_secs(elapsed.unsigned_abs() / 1000))
}

/// The first line of a card: the agent's name, the branch, how long ago and
/// the status word, joined by ` · `; the branch and the status are left out
/// when the card has none.
pub(crate) struct Meta {
    pub text: String,
    /// Where the status word is in `text`, and which one it is: the view
    /// colours it.
    pub status: Option<(Range<usize>, SessionStatus)>,
}

pub(crate) fn meta(strings: &Strings, card: &Card, now: Timestamp) -> Meta {
    let mut text = agent_name(strings, &card.session.agent);
    if let Some(branch) = &card.branch {
        text.push_str(SEPARATOR);
        text.push_str(branch);
    }
    text.push_str(SEPARATOR);
    text.push_str(&relative(strings, &card.session, now));
    let status = card.status.map(|status| {
        text.push_str(SEPARATOR);
        let start = text.len();
        text.push_str(status_word(strings, status));
        (start..text.len(), status)
    });
    Meta { text, status }
}

pub(crate) fn status_word(strings: &Strings, status: SessionStatus) -> &str {
    strings.get(match status {
        SessionStatus::Running => keys::project_home::STATUS_RUNNING,
        SessionStatus::Waiting => keys::project_home::STATUS_WAITING,
        SessionStatus::Ended => keys::project_home::STATUS_ENDED,
    })
}

/// The two sentences under the large title: how many agents are running and
/// how many files changed outside any session. A sentence whose count is 0
/// (or unknown: `Pending`, `Capped`, `Hidden`) is left out; None when both are.
pub(crate) fn lede(strings: &Strings, main: &MainModel) -> Option<String> {
    let mut parts = Vec::new();
    if main.running > 0 {
        parts.push(strings.format(
            keys::project_home::LEDE_RUNNING,
            &Args::new().num("count", main.running),
        ));
    }
    if let OutsideRow::Files { count, .. } = &main.outside
        && *count > 0
    {
        parts.push(strings.format(
            keys::project_home::LEDE_OUTSIDE,
            &Args::new().num("count", *count),
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The toolbar's second line, "main · 11 worktrees"; None without a branch
/// (a folder project has neither a branch nor worktrees).
pub(crate) fn subtitle(strings: &Strings, snapshot: &HomeSnapshot) -> Option<Text> {
    let branch = snapshot.branch.as_deref()?;
    Some(Text::fmt(
        strings,
        keys::project_home::SUBTITLE,
        &Args::new()
            .str("branch", branch)
            .num("count", snapshot.worktree_count),
    ))
}

/// The first three changed paths of the Outside card, in the code font, with
/// an ellipsis when there are more than the ones given; None without paths.
pub(crate) fn outside_names(count: u32, names: &[RelPath]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let mut text = names
        .iter()
        .take(3)
        .map(|name| &**name)
        .collect::<Vec<_>>()
        .join(SEPARATOR);
    if count as usize > names.len().min(3) {
        text.push('…');
    }
    Some(text)
}

/// The tightest limit's line, for example Codex, 5-hour, 64 %: the length of
/// the limit window in hours, or in days when it is a whole number of days;
/// without a length the line names only the agent and the percentage.
pub(crate) fn limit(strings: &Strings, limit: &TightestLimit) -> String {
    let agent = agent_name(strings, &limit.agent);
    let percent = limit.used_percent;
    let window = limit
        .window_minutes
        .filter(|minutes| *minutes > 0)
        .map(|minutes| {
            if minutes % MINUTES_PER_DAY == 0 {
                strings.format(
                    keys::project_home::WINDOW_DAYS,
                    &Args::new().num("count", minutes / MINUTES_PER_DAY),
                )
            } else {
                strings.format(
                    keys::project_home::WINDOW_HOURS,
                    &Args::new().num("count", minutes.div_ceil(60)),
                )
            }
        });
    match window {
        Some(window) => strings.format(
            keys::project_home::LIMIT,
            &Args::new()
                .str("agent", &agent)
                .str("window", &window)
                .num("percent", percent),
        ),
        None => strings.format(
            keys::project_home::LIMIT_NO_WINDOW,
            &Args::new().str("agent", &agent).num("percent", percent),
        ),
    }
}

/// The second grey line of a card: the signals that hold, joined; None when
/// none does.
pub(crate) fn signals(strings: &Strings, signals: &Signals) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if signals.drifted {
        parts.push(strings.get(keys::project_home::SIGNAL_DRIFTED).to_owned());
    }
    if signals.critical > 0 {
        parts.push(strings.format(
            keys::project_home::SIGNAL_CRITICAL,
            &Args::new().num("count", signals.critical),
        ));
    }
    if signals.changed_since_seen > 0 {
        parts.push(strings.format(
            keys::project_home::SIGNAL_CHANGED,
            &Args::new().num("count", signals.changed_since_seen),
        ));
    }
    (!parts.is_empty()).then(|| parts.join(SEPARATOR))
}

/// `Push(REVIEW, Session(key))`.
pub(crate) fn review_session(key: SessionKey) -> WorkspaceCommand {
    push_review(ReviewTarget::Session(key))
}

/// `Push(REVIEW, Outside { project, worktree })` for the snapshot's active
/// worktree; None for a folder project, which has no base.
pub(crate) fn review_outside(snapshot: &HomeSnapshot) -> Option<WorkspaceCommand> {
    let worktree = snapshot.active_worktree.clone()?;
    Some(push_review(ReviewTarget::Outside {
        project: snapshot.project.clone(),
        worktree,
    }))
}

fn push_review(target: ReviewTarget) -> WorkspaceCommand {
    WorkspaceCommand::Push(Route {
        screen: Name::Borrowed(screens::REVIEW),
        target: Target::Review(target),
    })
}

/// `Push(USAGE, Project(key))`.
pub(crate) fn usage_screen(project: ProjectKey) -> WorkspaceCommand {
    WorkspaceCommand::Push(Route {
        screen: Name::Borrowed(screens::USAGE),
        target: Target::Project(project),
    })
}

/// A boolean as the semantic attributes write it.
pub(crate) fn flag(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
