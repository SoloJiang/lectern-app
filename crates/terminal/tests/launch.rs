//! `launch_*`, `resume_refuses_*` and `no_echo_of_program_text`
//! (docs/design/terminal.md「测试」): what is started, with which argv and
//! environment, and what is refused. The programs are the fake program.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    FAKE, Kit, UUID_A, UUID_B, claude_command, codex_command, located, resume, running_pid, shell,
    size,
};
use lectern_services::session::{Place, SessionKey, SourceId};
use lectern_services::terminal::{
    AgentCommand, FailReason, Launch, LaunchError, LaunchKind, RunState,
};
use lectern_terminal::testing::Harness;

fn started(kit: &Kit, harness: &Harness, launch: Launch) -> common::Record {
    let id = harness.open(launch, size()).expect("opens");
    let info = harness.wait_state(id, |s| matches!(s, RunState::Running { .. }));
    kit.record(harness, running_pid(&info))
}

#[test]
fn launch_resume_claude_runs_claude_resume_with_the_session_id() {
    let kit = Kit::new();
    let harness = kit.harness();
    let project = kit.project_key(&harness);
    let key = harness.claude.add(
        UUID_A,
        &kit.project,
        located(&project),
        Some(claude_command(UUID_A)),
    );
    let record = started(&kit, &harness, resume(&key));
    assert_eq!(record.argv0, kit.bin.join("claude").to_string_lossy());
    assert_eq!(record.args, ["--resume", UUID_A]);
    assert_eq!(record.cwd, kit.project.to_string_lossy());
    assert!(record.tty);
}

#[test]
fn launch_resume_codex_runs_codex_resume_with_the_session_id() {
    let kit = Kit::new();
    let harness = kit.harness();
    let project = kit.project_key(&harness);
    let key = harness.codex.add(
        UUID_B,
        &kit.project,
        located(&project),
        Some(codex_command(UUID_B)),
    );
    let record = started(&kit, &harness, resume(&key));
    assert_eq!(record.argv0, kit.bin.join("codex").to_string_lossy());
    assert_eq!(record.args, ["resume", UUID_B]);
}

#[test]
fn launch_new_session_runs_the_agent_without_arguments() {
    let kit = Kit::new();
    let harness = kit.harness();
    for (agent, name) in [
        (AgentCommand::ClaudeCode, "claude"),
        (AgentCommand::Codex, "codex"),
    ] {
        let record = started(
            &kit,
            &harness,
            Launch::NewSession {
                agent,
                cwd: Arc::from(kit.project.as_path()),
            },
        );
        assert_eq!(record.argv0, kit.bin.join(name).to_string_lossy());
        assert!(record.args.is_empty(), "{name}: {:?}", record.args);
    }
}

#[test]
fn launch_shell_is_a_login_shell_without_arguments() {
    let kit = Kit::new();
    let harness = kit.harness();
    let record = started(&kit, &harness, shell(&kit.project));
    // A leading `-` makes it a login shell.
    assert_eq!(record.argv0, "-lectern-terminal-fake");
    assert!(record.args.is_empty());
    assert_eq!(record.vars.get("SHELL").map(String::as_str), Some(FAKE));
    assert_eq!(record.cwd, kit.project.to_string_lossy());
}

#[test]
fn launch_environment_is_the_terminal_one_without_lectern_variables() {
    let kit = Kit::new();
    let harness = kit.harness();
    let record = started(&kit, &harness, shell(&kit.project));
    let var = |name: &str| record.vars.get(name).map(String::as_str);
    assert_eq!(var("TERM"), Some("xterm-256color"));
    assert_eq!(var("COLORTERM"), Some("truecolor"));
    assert_eq!(var("TERM_PROGRAM"), Some("Lectern"));
    assert_eq!(var("TERM_PROGRAM_VERSION"), Some(env!("CARGO_PKG_VERSION")));
    assert_eq!(var("PWD"), Some(&*kit.project.to_string_lossy()));
    // No locale variable was set, so the UTF-8 one is.
    assert_eq!(var("LC_CTYPE"), Some("UTF-8"));
    assert!(
        !record.names.iter().any(|n| n.starts_with("LECTERN_")),
        "{:?}",
        record.names
    );
    assert!(record.names.iter().any(|n| n == "PATH"));
}

#[test]
fn launch_does_not_pass_on_a_shell_variable_that_is_not_executable() {
    // portable-pty would log such a `$SHELL` to the `log` facade.
    let kit = Kit::new();
    let mut env = kit.env();
    env.push(("SHELL".into(), "/no/such/shell-for-the-test".into()));
    let harness = Harness::start(Harness::plugin().with_login_env(env));
    harness.projects.add(&kit.project);
    let record = started(
        &kit,
        &harness,
        Launch::NewSession {
            agent: AgentCommand::ClaudeCode,
            cwd: Arc::from(kit.project.as_path()),
        },
    );
    assert_ne!(
        record.vars.get("SHELL").map(String::as_str),
        Some("/no/such/shell-for-the-test")
    );
}

#[test]
fn launch_keeps_the_locale_the_environment_sets() {
    let kit = Kit::new();
    let mut env = kit.env();
    env.push(("LANG".into(), "en_US.UTF-8".into()));
    let harness = Harness::start(Harness::plugin().with_login_env(env).with_shell(FAKE));
    harness.projects.add(&kit.project);
    let record = started(&kit, &harness, shell(&kit.project));
    assert_eq!(record.vars.get("LC_CTYPE"), None);
}

#[test]
fn launch_resume_keeps_the_logged_working_directory_spelling() {
    // The session log's `cwd` is used as written, here with a `..` in it.
    let kit = Kit::new();
    std::fs::create_dir(kit.project.join("sub")).unwrap();
    let harness = kit.harness();
    let project = kit.project_key(&harness);
    let written = kit.project.join("sub").join("..");
    let key = harness.claude.add(
        UUID_A,
        &written,
        located(&project),
        Some(claude_command(UUID_A)),
    );
    let id = harness.open(resume(&key), size()).unwrap();
    assert_eq!(
        harness.terminals.info(id).unwrap().cwd.as_ref(),
        written.as_path()
    );
    let info = harness.wait_state(id, |s| matches!(s, RunState::Running { .. }));
    let record = kit.record(&harness, running_pid(&info));
    assert_eq!(record.vars.get("PWD").map(String::as_str), written.to_str());
}

#[test]
fn launch_without_the_agent_on_the_path_fails_as_not_installed() {
    let kit = Kit::new();
    std::fs::remove_file(kit.bin.join("claude")).unwrap();
    let harness = kit.harness();
    let id = harness
        .open(
            Launch::NewSession {
                agent: AgentCommand::ClaudeCode,
                cwd: Arc::from(kit.project.as_path()),
            },
            size(),
        )
        .unwrap();
    let info = harness.wait_state(id, |s| matches!(s, RunState::Failed(_)));
    assert_eq!(
        info.state,
        RunState::Failed(FailReason::NotInstalled(AgentCommand::ClaudeCode))
    );
    assert_eq!(kit.records(), 0, "the fake program was not started");
}

#[test]
fn launch_with_a_missing_directory_fails_as_cwd_missing() {
    let kit = Kit::new();
    let harness = kit.harness();
    let id = harness
        .open(shell(&kit.project.join("gone")), size())
        .unwrap();
    let info = harness.wait_state(id, |s| matches!(s, RunState::Failed(_)));
    assert_eq!(info.state, RunState::Failed(FailReason::CwdMissing));
    assert_eq!(kit.records(), 0);
}

#[test]
fn launch_with_a_file_for_a_directory_fails_as_cwd_missing() {
    let kit = Kit::new();
    std::fs::write(kit.project.join("file"), b"x").unwrap();
    let harness = kit.harness();
    let id = harness
        .open(shell(&kit.project.join("file")), size())
        .unwrap();
    let info = harness.wait_state(id, |s| matches!(s, RunState::Failed(_)));
    assert_eq!(info.state, RunState::Failed(FailReason::CwdMissing));
}

#[test]
fn launch_without_a_shell_fails_as_no_shell() {
    let kit = Kit::new();
    let harness = Harness::start(
        Harness::plugin()
            .with_login_env(kit.env())
            .with_shell(kit.root.join("no-such-shell")),
    );
    harness.projects.add(&kit.project);
    let id = harness.open(shell(&kit.project), size()).unwrap();
    let info = harness.wait_state(id, |s| matches!(s, RunState::Failed(_)));
    assert_eq!(info.state, RunState::Failed(FailReason::NoShell));
}

#[test]
fn launch_outside_every_project_is_refused_at_open() {
    let kit = Kit::new();
    let harness = kit.harness();
    let outside = kit.root.join("elsewhere");
    std::fs::create_dir(&outside).unwrap();
    assert_eq!(
        harness.open(shell(&outside), size()),
        Err(LaunchError::OutsideProject)
    );
    assert!(harness.terminals.list().terminals.is_empty());
}

#[test]
fn launch_registers_the_terminal_before_open_returns() {
    let kit = Kit::new();
    kit.plan("say:READY,nl,hold");
    let harness = kit.harness();
    let project = kit.project_key(&harness);
    let key = harness.claude.add(
        UUID_A,
        &kit.project,
        located(&project),
        Some(claude_command(UUID_A)),
    );
    let id = harness.open(resume(&key), size()).unwrap();
    let info = harness.terminals.info(id).expect("registered");
    assert!(matches!(
        info.state,
        RunState::Starting | RunState::Running { .. }
    ));
    assert_eq!(info.launch, LaunchKind::Resume(key.clone()));
    assert_eq!(info.project, project);
    assert_eq!(harness.terminals.for_session(&key), Some(id));
    assert_eq!(harness.terminals.list().terminals.len(), 1);
}

#[test]
fn launch_stops_at_eight_terminals() {
    let kit = Kit::new();
    let harness = kit.harness();
    for _ in 0..8 {
        harness.open(shell(&kit.project), size()).unwrap();
    }
    assert_eq!(
        harness.open(shell(&kit.project), size()),
        Err(LaunchError::TooMany)
    );
    let first = harness.terminals.list().terminals[0].id;
    harness.terminals.close(first);
    harness.open(shell(&kit.project), size()).unwrap();
}

// ---------------------------------------------------------------- refusals

/// A session in the Claude source that the terminal is asked to resume.
fn refuses(
    kit: &Kit,
    harness: &Harness,
    key: &SessionKey,
    expected: LaunchError,
) -> Result<(), String> {
    let got = harness.open(resume(key), size());
    if got != Err(expected) {
        return Err(format!("expected {expected:?}, got {got:?}"));
    }
    if !harness.terminals.list().terminals.is_empty() {
        return Err("a terminal was registered".into());
    }
    if kit.records() != 0 {
        return Err("the fake program was started".into());
    }
    Ok(())
}

#[test]
fn resume_refuses_ids_that_are_not_canonical_uuids() {
    let kit = Kit::new();
    let harness = kit.harness();
    let project = kit.project_key(&harness);
    for id in [
        "not-a-uuid",
        "00112233-4455-6677-8899-AABBCCDDEEFF",
        "00112233-4455-6677-8899-aabbccddeef ",
        " 00112233-4455-6677-8899-aabbccddeeff",
        "00112233445566778899aabbccddeeff",
        "--help",
    ] {
        let key = harness.claude.add(
            id,
            &kit.project,
            located(&project),
            Some(claude_command(id)),
        );
        refuses(&kit, &harness, &key, LaunchError::InvalidSessionId)
            .unwrap_or_else(|e| panic!("{id:?}: {e}"));
    }
}

#[test]
fn resume_refuses_a_command_whose_id_is_not_the_key_id() {
    let kit = Kit::new();
    let harness = kit.harness();
    let project = kit.project_key(&harness);
    let key = harness.claude.add(
        UUID_A,
        &kit.project,
        located(&project),
        Some(claude_command(UUID_B)),
    );
    refuses(&kit, &harness, &key, LaunchError::InvalidSessionId).unwrap();
}

#[test]
fn resume_refuses_a_command_of_the_other_agent() {
    let kit = Kit::new();
    let harness = kit.harness();
    let project = kit.project_key(&harness);
    let in_claude = harness.claude.add(
        UUID_A,
        &kit.project,
        located(&project),
        Some(codex_command(UUID_A)),
    );
    refuses(&kit, &harness, &in_claude, LaunchError::InvalidSessionId).unwrap();
    let in_codex = harness.codex.add(
        UUID_B,
        &kit.project,
        located(&project),
        Some(claude_command(UUID_B)),
    );
    refuses(&kit, &harness, &in_codex, LaunchError::InvalidSessionId).unwrap();
}

#[test]
fn resume_refuses_sources_that_are_not_built_in() {
    let kit = Kit::new();
    let harness = kit.harness();
    for source in ["ext.x.y", "session-claude-code2", "", "session-codex "] {
        let key = SessionKey {
            source: SourceId(Arc::from(source)),
            id: Arc::from(UUID_A),
        };
        refuses(&kit, &harness, &key, LaunchError::UntrustedSource)
            .unwrap_or_else(|e| panic!("{source:?}: {e}"));
    }
}

#[test]
fn resume_refuses_sessions_the_source_does_not_list() {
    let kit = Kit::new();
    let harness = kit.harness();
    let key = harness.claude.key(UUID_A);
    refuses(&kit, &harness, &key, LaunchError::UnknownSession).unwrap();
}

#[test]
fn resume_refuses_sessions_without_a_resume_command() {
    let kit = Kit::new();
    let harness = kit.harness();
    let project = kit.project_key(&harness);
    let key = harness
        .claude
        .add(UUID_A, &kit.project, located(&project), None);
    refuses(&kit, &harness, &key, LaunchError::NotResumable).unwrap();
}

#[test]
fn resume_refuses_sessions_that_are_not_located_in_an_open_project() {
    let kit = Kit::new();
    let harness = kit.harness();
    let project = kit.project_key(&harness);
    let places = [
        Place::Pending,
        Place::Missing,
        Place::Unresolvable(lectern_services::project::OpenError::NotFound),
    ];
    for (n, place) in places.into_iter().enumerate() {
        let id = format!("00112233-4455-6677-8899-aabbccddee{n:02x}");
        let key = harness
            .claude
            .add(&id, &kit.project, place, Some(claude_command(&id)));
        refuses(&kit, &harness, &key, LaunchError::OutsideProject)
            .unwrap_or_else(|e| panic!("{n}: {e}"));
    }
    // Located, but the project has been closed since.
    let id = "00112233-4455-6677-8899-aabbccddeeaa";
    let key = harness.claude.add(
        id,
        &kit.project,
        located(&project),
        Some(claude_command(id)),
    );
    harness.projects.remove(&project);
    refuses(&kit, &harness, &key, LaunchError::OutsideProject).unwrap();
}

#[test]
fn resume_refuses_a_directory_that_became_a_link_out_of_the_project() {
    // The session scan placed the directory in the project; later it was
    // replaced by a link to a place outside it (design 「安全边界」 6).
    let kit = Kit::new();
    let outside = kit.root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let link = kit.project.join("link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let harness = kit.harness();
    let project = kit.project_key(&harness);
    let key = harness.claude.add(
        UUID_A,
        &link,
        located(&project),
        Some(claude_command(UUID_A)),
    );
    let id = harness.open(resume(&key), size()).expect("opens");
    let info = harness.wait_state(id, |s| matches!(s, RunState::Failed(_)));
    assert_eq!(info.state, RunState::Failed(FailReason::OutsideProject));
    assert_eq!(kit.records(), 0, "the fake program was not started");
}

#[test]
fn resume_refuses_a_new_session_in_a_link_out_of_the_project() {
    let kit = Kit::new();
    let outside = kit.root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let link = kit.project.join("link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let harness = kit.harness();
    let id = harness.open(shell(&link), size()).expect("opens");
    let info = harness.wait_state(id, |s| matches!(s, RunState::Failed(_)));
    assert_eq!(info.state, RunState::Failed(FailReason::OutsideProject));
    assert_eq!(kit.records(), 0);
}

// ------------------------------------------------------------------ echo

#[test]
fn no_echo_of_program_text() {
    // The program shows a title and asks the terminal questions: the answers
    // that reach its input hold fixed information only, never the title it
    // set (design 「安全边界」).
    let kit = Kit::new();
    let canary = "SECRET-TITLE-CANARY";
    let output = kit.root.join("output.vt");
    let received = kit.root.join("received.bin");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("\x1b]0;{canary}\x07").as_bytes());
    bytes.extend_from_slice(b"\x1b[21t\x1b[20t"); // title and icon reports
    bytes.push(0x05); // ENQ
    bytes.extend_from_slice(b"\x1b[>q"); // XTVERSION
    bytes.extend_from_slice(b"\x1b[14t\x1b[18t"); // sizes
    bytes.extend_from_slice(b"\x1b[c"); // device attributes: the last answer
    std::fs::write(&output, bytes).unwrap();
    kit.plan(&format!(
        "raw,out:{},input:{}",
        output.display(),
        received.display()
    ));
    let harness = kit.harness();
    let id = harness.open(shell(&kit.project), size()).unwrap();
    // The device attributes answer ends with `c` after the version answer.
    let got = harness
        .wait_for(Duration::from_secs(20), || {
            let bytes = std::fs::read(&received).ok()?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            (text.contains("\x1b[?") && text.ends_with('c')).then_some(text)
        })
        .expect("the answers reach the program");
    assert!(!got.contains(canary), "{got:?}");
    assert!(got.contains("Lectern"), "XTVERSION is answered: {got:?}");
    // The title itself is shown, as text, to the user.
    let title = harness.wait_for(Duration::from_secs(5), || {
        harness
            .terminals
            .info(id)
            .and_then(|i| i.title)
            .filter(|t| &**t == canary)
    });
    assert!(title.is_some(), "the title is shown to the user");
}
