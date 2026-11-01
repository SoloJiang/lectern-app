//! Pure functions of step 3 (docs/design/conversation.md「测试」): who may
//! resume (`drive_*`), the lines about a terminal and about a refused open,
//! and the count of the leave guard. No window and no service.

use std::path::Path;
use std::sync::Arc;

use lectern_i18n::{Locale, Strings};
use lectern_services::conversation::{Link, LinkError};
use lectern_services::session::{Agent, Timestamp};
use lectern_services::terminal::{
    AgentCommand, FailReason, LaunchError, LaunchKind, RunState, TerminalId, TerminalInfo,
    TerminalList,
};
use lectern_services::workspace::LeaveScope;

use crate::drive::{self, Resume};
use crate::guard::running_in;
use crate::present;
use crate::testing::folder;

#[test]
fn drive_resume_follows_the_link() {
    // No link, or no `dyn Conversations`: open the terminal.
    assert_eq!(drive::resume_mode(None), Resume::Open);
    assert_eq!(drive::resume_mode(Some(&Link::None)), Resume::Open);
    // A connection that may still be there goes first.
    assert_eq!(
        drive::resume_mode(Some(&Link::Idle)),
        Resume::DisconnectFirst
    );
    assert_eq!(
        drive::resume_mode(Some(&Link::Failed(LinkError::Protocol))),
        Resume::DisconnectFirst
    );
    // A connection that starts or a turn that runs blocks the terminal.
    for link in [Link::Connecting, Link::Working, Link::Stopping] {
        assert_eq!(drive::resume_mode(Some(&link)), Resume::Blocked);
    }
}

#[test]
fn drive_an_ended_terminal_does_not_stop_a_resume() {
    // Only a terminal that starts or runs drives the session.
    assert!(drive::drives(Some(&RunState::Starting)));
    assert!(drive::drives(Some(&RunState::Running { pid: 1 })));
    for state in [
        RunState::Exited {
            code: 1,
            signaled: false,
        },
        RunState::Failed(FailReason::CwdMissing),
    ] {
        assert!(!drive::drives(Some(&state)));
    }
    assert!(!drive::drives(None));
}

#[test]
fn terminal_status_has_a_line_for_every_state() {
    let strings = Strings::of(Locale::En);
    let key_of = |state: RunState| present::terminal_status(&strings, &state).message_id();
    assert_eq!(
        key_of(RunState::Starting),
        Some("conversation-terminal-starting")
    );
    assert_eq!(
        key_of(RunState::Running { pid: 3 }),
        Some("conversation-terminal-running")
    );
    assert_eq!(
        key_of(RunState::Exited {
            code: 2,
            signaled: false
        }),
        Some("conversation-terminal-exited")
    );
    assert_eq!(
        key_of(RunState::Exited {
            code: 0,
            signaled: true
        }),
        Some("conversation-terminal-signaled")
    );
    assert_eq!(
        key_of(RunState::Failed(FailReason::NotInstalled(
            AgentCommand::Codex
        ))),
        Some("conversation-terminal-not-installed")
    );
    // The program to install is named by the command, not translated.
    let text = present::terminal_status(
        &strings,
        &RunState::Failed(FailReason::NotInstalled(AgentCommand::Codex)),
    );
    assert!(text.shared().contains("codex"), "{}", text.shared());
    let text = present::terminal_status(
        &strings,
        &RunState::Exited {
            code: 7,
            signaled: false,
        },
    );
    assert!(text.shared().contains('7'), "{}", text.shared());
    for (reason, key) in [
        (FailReason::CwdMissing, "conversation-terminal-cwd-missing"),
        (
            FailReason::OutsideProject,
            "conversation-terminal-outside-project",
        ),
        (FailReason::NoShell, "conversation-terminal-failed"),
        (
            FailReason::Spawn { errno: None },
            "conversation-terminal-failed",
        ),
        (FailReason::Internal, "conversation-terminal-failed"),
    ] {
        assert_eq!(key_of(RunState::Failed(reason)), Some(key), "{reason:?}");
    }
}

#[test]
fn terminal_refusals_map_to_the_lines_of_the_design() {
    let strings = Strings::of(Locale::En);
    let key_of = |error: LaunchError| {
        present::terminal_refusal(&strings, &error).and_then(|text| text.message_id())
    };
    for error in [
        LaunchError::NotResumable,
        LaunchError::UnknownSession,
        LaunchError::UntrustedSource,
        LaunchError::InvalidSessionId,
    ] {
        assert_eq!(
            key_of(error),
            Some("conversation-terminal-not-resumable"),
            "{error:?}"
        );
    }
    assert_eq!(
        key_of(LaunchError::OutsideProject),
        Some("conversation-terminal-outside-project")
    );
    assert_eq!(
        key_of(LaunchError::TooMany),
        Some("conversation-terminal-too-many")
    );
    // The page uses the terminal that runs: no reason.
    assert_eq!(key_of(LaunchError::AlreadyRunning(TerminalId(4))), None);
}

#[test]
fn terminal_resume_commands_are_not_translated() {
    assert_eq!(
        present::resume_command(&Agent::ClaudeCode),
        Some("claude --resume")
    );
    assert_eq!(present::resume_command(&Agent::Codex), Some("codex resume"));
    assert_eq!(
        present::resume_command(&Agent::Other(Arc::from("someone else"))),
        None
    );
}

#[test]
fn leave_guard_counts_by_scope() {
    let info = |n: u64, project: &str, state: RunState| TerminalInfo {
        id: TerminalId(n),
        launch: LaunchKind::Shell,
        project: folder(project),
        cwd: Arc::from(Path::new("/work/app")),
        state,
        title: None,
        opened_at: Timestamp(0),
        bells: 0,
        dropped_inputs: 0,
    };
    let exited = RunState::Exited {
        code: 0,
        signaled: false,
    };
    let list = TerminalList {
        generation: 1,
        terminals: Arc::from(vec![
            info(1, "/a", RunState::Running { pid: 1 }),
            info(2, "/a", RunState::Starting),
            info(3, "/a", exited),
            info(4, "/b", RunState::Running { pid: 2 }),
            info(5, "/b", RunState::Failed(FailReason::NoShell)),
        ]),
    };
    assert_eq!(running_in(&list, &LeaveScope::Project(folder("/a"))), 2);
    assert_eq!(running_in(&list, &LeaveScope::Project(folder("/b"))), 1);
    assert_eq!(running_in(&list, &LeaveScope::Project(folder("/c"))), 0);
    assert_eq!(running_in(&list, &LeaveScope::App), 3);
    // A terminal outlives a view, a window and a worktree switch.
    for scope in [
        LeaveScope::View,
        LeaveScope::Window,
        LeaveScope::Worktree(folder("/a")),
    ] {
        assert_eq!(running_in(&list, &scope), 0, "{scope:?}");
    }
}
