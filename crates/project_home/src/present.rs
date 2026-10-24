//! Turns the snapshot's values into interface text and the few facts the views
//! share (docs/design/project_home.md「界面」). Every sentence comes from
//! `keys::project_home::*`; the user's own words (titles, agent names) are
//! shown as written. Nothing here reads a clock: the relative times count
//! back from the snapshot's `now`.

use std::time::Duration;

use lectern_i18n::{Args, Strings, keys};
use lectern_kernel::Name;
use lectern_services::project::ProjectKey;
use lectern_services::review::ReviewTarget;
use lectern_services::session::{Agent, SessionKey, Timestamp};
use lectern_services::workspace::{Route, Target, WorkspaceCommand, screens};
use lectern_ui::Text;

use crate::HomeSnapshot;
use crate::snapshot::{SessionRow, Signals};

/// The separator of the second line of a card and of the agent's name.
const SEPARATOR: &str = " · ";

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

/// How long ago the session moved, or `Running`; led by the agent's name when
/// the agent has no dot.
pub(crate) fn activity(strings: &Strings, row: &SessionRow, now: Timestamp) -> String {
    let when = if row.running {
        strings.get(keys::project_home::RUNNING).to_owned()
    } else {
        let elapsed = now.0.saturating_sub(row.last_activity.0).max(0);
        strings.relative_time(Duration::from_secs(elapsed.unsigned_abs() / 1000))
    };
    match &row.agent {
        Agent::Other(name) => format!("{name}{SEPARATOR}{when}"),
        _ => when,
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
