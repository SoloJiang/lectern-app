//! Snapshot values as interface text (docs/design/conversation.md「界面」):
//! pure functions of a value and the strings. Every sentence is a Fluent
//! message; what an agent or a person wrote (a summary, a path, an error
//! text) is inserted as an argument or shown verbatim, never translated.

use std::path::Path;
use std::sync::Arc;

use lectern_i18n::{Args, Strings, keys};
use lectern_services::conversation::{
    AcpProgram, CallKind, CallState, ContextUsage, EndReason, Link, LinkError, NoteLocation,
    PermissionRequest, ToolEntry,
};
use lectern_services::session::Agent;
use lectern_ui::Text;

use crate::items::shown_tools;

/// The separator between the pieces of one line.
pub(crate) const SEPARATOR: &str = " \u{00B7} ";

/// The agent's product name: ours from the resource files, another source's
/// as written (the same names as the project home's cards).
pub(crate) fn agent_name(strings: &Strings, agent: &Agent) -> String {
    match agent {
        Agent::ClaudeCode => strings
            .get(keys::project_home::AGENT_CLAUDE_CODE)
            .to_owned(),
        Agent::Codex => strings.get(keys::project_home::AGENT_CODEX).to_owned(),
        Agent::Other(name) => name.to_string(),
    }
}

/// The agent dot's colour for an agent; none for one without a colour.
pub(crate) fn dot_agent(agent: &Agent) -> Option<lectern_ui::Agent> {
    match agent {
        Agent::ClaudeCode => Some(lectern_ui::Agent::ClaudeCode),
        Agent::Codex => Some(lectern_ui::Agent::Codex),
        Agent::Other(_) => None,
    }
}

/// The `agent` attribute of the status node.
pub(crate) fn agent_attr(agent: &Agent) -> String {
    match agent {
        Agent::ClaudeCode => lectern_ui::Agent::ClaudeCode.as_str().to_owned(),
        Agent::Codex => lectern_ui::Agent::Codex.as_str().to_owned(),
        Agent::Other(_) => "other".to_owned(),
    }
}

/// The program the ACP adapter of an agent runs as (ADR 0050), for the
/// messages about starting it.
fn program_name(strings: &Strings, agent: &Agent) -> String {
    match agent {
        Agent::ClaudeCode => AcpProgram::ClaudeCode.name().to_owned(),
        Agent::Codex => AcpProgram::Codex.name().to_owned(),
        Agent::Other(_) => agent_name(strings, agent),
    }
}

/// The status line's message: a request waiting for the user beats the
/// link's own state.
pub(crate) fn status_text(strings: &Strings, link: &Link, waiting: bool) -> Text {
    let key = if waiting {
        keys::conversation::STATUS_WAITING
    } else {
        match link {
            Link::None => keys::conversation::STATUS_HISTORY,
            Link::Connecting => keys::conversation::STATUS_CONNECTING,
            Link::Idle => keys::conversation::STATUS_IDLE,
            Link::Working => keys::conversation::STATUS_WORKING,
            Link::Stopping => keys::conversation::STATUS_STOPPING,
            Link::Failed(_) => keys::conversation::STATUS_FAILED,
        }
    };
    Text::key(strings, key)
}

/// The context window's occupancy as a whole percent, 0 to 100. None when the
/// window has no size (nothing to divide by).
pub(crate) fn context_percent(usage: &ContextUsage) -> Option<u32> {
    if usage.size == 0 {
        return None;
    }
    let percent = u128::from(usage.used) * 100 / u128::from(usage.size);
    Some(u32::try_from(percent.min(100)).unwrap_or(100))
}

pub(crate) fn context_text(strings: &Strings, percent: u32) -> Text {
    Text::fmt(
        strings,
        keys::conversation::CONTEXT,
        &Args::new().num("percent", percent),
    )
}

/// Whether the snapshot waits for the user to answer a request.
pub(crate) fn is_waiting(permissions: &[PermissionRequest]) -> bool {
    !permissions.is_empty()
}

/// One tool call as text: the verb from the resource files and the summary
/// the provider cut (a file name, the first line of a command, the agent's
/// own title), then how the call ended when it did not go well.
fn tool_text(strings: &Strings, call: &ToolEntry) -> String {
    let with_target = |key| strings.format(key, &Args::new().str("target", &call.summary));
    let mut text = match call.kind {
        CallKind::Read => with_target(keys::conversation::TOOL_READ),
        CallKind::Edit => with_target(keys::conversation::TOOL_EDIT),
        CallKind::Delete => with_target(keys::conversation::TOOL_DELETE),
        CallKind::Move => with_target(keys::conversation::TOOL_MOVE),
        CallKind::Search => with_target(keys::conversation::TOOL_SEARCH),
        CallKind::Execute => with_target(keys::conversation::TOOL_EXECUTE),
        CallKind::Fetch => with_target(keys::conversation::TOOL_FETCH),
        CallKind::SpawnAgent => strings.get(keys::conversation::TOOL_AGENT).to_owned(),
        // The agent's own title or the tool's name.
        CallKind::Think | CallKind::Other => call.summary.to_string(),
    };
    let outcome = match call.state {
        CallState::Failed => Some(keys::conversation::TOOL_FAILED),
        CallState::Denied => Some(keys::conversation::TOOL_DENIED),
        CallState::Cancelled => Some(keys::conversation::TOOL_CANCELLED),
        CallState::Pending | CallState::Running | CallState::Completed => None,
    };
    if let Some(outcome) = outcome {
        text.push_str(SEPARATOR);
        text.push_str(strings.get(outcome));
    }
    text
}

/// The row of consecutive tool calls: at most four, then "n more".
pub(crate) fn tools_line(strings: &Strings, calls: &[ToolEntry]) -> String {
    let (shown, more) = shown_tools(calls);
    let mut parts: Vec<String> = shown.iter().map(|call| tool_text(strings, call)).collect();
    if more > 0 {
        parts.push(strings.format(
            keys::conversation::TOOL_MORE,
            &Args::new().num("count", more as f64),
        ));
    }
    parts.join(SEPARATOR)
}

/// "n review notes · place, place": the places relative to the session's
/// folder when they are inside it.
pub(crate) fn notes_line(
    strings: &Strings,
    count: u32,
    locations: &[NoteLocation],
    cwd: Option<&Path>,
) -> String {
    let mut line = strings.format(
        keys::conversation::NOTES_SENT,
        &Args::new().num("count", count),
    );
    let places: Vec<String> = locations
        .iter()
        .map(|location| {
            let shown = cwd
                .and_then(|cwd| location.path.strip_prefix(cwd).ok())
                .unwrap_or(&location.path);
            format!("{}:{}", shown.display(), location.line)
        })
        .collect();
    if !places.is_empty() {
        line.push_str(SEPARATOR);
        line.push_str(&places.join(", "));
    }
    line
}

pub(crate) fn ended_text(strings: &Strings, reason: EndReason) -> Text {
    Text::key(
        strings,
        match reason {
            EndReason::MaxTokens => keys::conversation::ENDED_MAX_TOKENS,
            EndReason::MaxTurnRequests => keys::conversation::ENDED_MAX_TURNS,
            EndReason::Refusal => keys::conversation::ENDED_REFUSAL,
            EndReason::Cancelled => keys::conversation::ENDED_CANCELLED,
        },
    )
}

/// The one line about a failed link, and the agent's own words for a failed
/// prompt (shown as it came, secondary).
pub(crate) fn link_error_text(
    strings: &Strings,
    error: &LinkError,
    agent: &Agent,
) -> (Text, Option<Text>) {
    let name = agent_name(strings, agent);
    let named = |key| Text::fmt(strings, key, &Args::new().str("agent", &name));
    match error {
        LinkError::Spawn => (
            Text::fmt(
                strings,
                keys::conversation::ERROR_SPAWN,
                &Args::new().str("program", &program_name(strings, agent)),
            ),
            None,
        ),
        LinkError::Unsupported => (named(keys::conversation::ERROR_UNSUPPORTED), None),
        LinkError::LoadFailed => (named(keys::conversation::ERROR_LOAD), None),
        LinkError::Protocol => (named(keys::conversation::ERROR_PROTOCOL), None),
        LinkError::Exited { .. } => (named(keys::conversation::ERROR_EXITED), None),
        LinkError::AuthRequired => (named(keys::conversation::ERROR_AUTH), None),
        LinkError::PromptFailed { message } => (
            named(keys::conversation::ERROR_PROMPT),
            Some(Text::verbatim(Arc::clone(message))),
        ),
    }
}
