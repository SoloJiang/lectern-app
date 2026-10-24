//! Log field helpers (docs/design/workspace.md「日志与诊断」): stable IDs and
//! variant names only, never titles, paths or payloads.

use std::borrow::Cow;
use std::sync::Mutex;

use lectern_kernel::Name;
use lectern_services::project::OpenError;
use lectern_services::workspace::WorkspaceCommand;

/// A contribution ID, screen ID or command name as a log field. A built-in
/// one is a `&'static str`; one built at runtime is interned here (not in
/// the plugin tag table, whose slots belong to process plugins); `"?"` when
/// it is not a valid ID or the table is full.
pub(crate) fn id_text(id: &Name) -> &'static str {
    match id {
        Cow::Borrowed(id) => id,
        Cow::Owned(id) => intern(id).unwrap_or("?"),
    }
}

/// The command name rule (kernel `Activation::Command`, such as
/// `terminal.open`): `[a-z0-9_.-]`, 1 to 64 bytes.
pub(crate) fn is_valid_command_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-' || b == b'.'
        })
}

/// The most IDs interned for the process; beyond it an ID logs as `"?"`.
const MAX_INTERNED: usize = 1024;
static INTERNED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// A runtime-built ID or command name as a `&'static str` for log fields,
/// interned once per name.
fn intern(name: &str) -> Option<&'static str> {
    if !is_valid_command_name(name) {
        return None;
    }
    let mut names = INTERNED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = names.iter().find(|n| **n == name) {
        return Some(found);
    }
    if names.len() >= MAX_INTERNED {
        return None;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.push(leaked);
    Some(leaked)
}

/// The contribution ID rule: `[a-z0-9_-]`, 1 to 64 bytes.
pub(crate) fn is_valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The command's variant name.
pub(crate) fn command_name(command: &WorkspaceCommand) -> &'static str {
    match command {
        WorkspaceCommand::OpenProject(_) => "open_project",
        WorkspaceCommand::PromptOpenProject => "prompt_open_project",
        WorkspaceCommand::CloseProject => "close_project",
        WorkspaceCommand::SetActiveWorktree(_) => "set_active_worktree",
        WorkspaceCommand::Push(_) => "push",
        WorkspaceCommand::Pop => "pop",
        WorkspaceCommand::PopToBase => "pop_to_base",
        WorkspaceCommand::ShowSidebarPanel(_) => "show_sidebar_panel",
        WorkspaceCommand::SetInspector(_) => "set_inspector",
        WorkspaceCommand::SelectInspectorTab(_) => "select_inspector_tab",
        WorkspaceCommand::OpenPanel(_) => "open_panel",
        WorkspaceCommand::ClosePanel => "close_panel",
        WorkspaceCommand::OpenOverlay(_) => "open_overlay",
        WorkspaceCommand::CloseOverlay => "close_overlay",
        WorkspaceCommand::Escape => "escape",
        WorkspaceCommand::CloseWindow => "close_window",
    }
}

/// The error's variant name; `RepoUnreadable` and `Io` carry text that is
/// not logged.
pub(crate) fn open_error_kind(error: &OpenError) -> &'static str {
    match error {
        OpenError::NotFound => "not_found",
        OpenError::NotADirectory => "not_a_directory",
        OpenError::PermissionDenied => "permission_denied",
        OpenError::RepoUnreadable(_) => "repo_unreadable",
        OpenError::Io(_) => "io",
    }
}
