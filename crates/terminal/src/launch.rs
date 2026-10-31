//! What a terminal starts and with which environment (docs/design/terminal.md
//! 「启动命令」「子进程的环境」「安全边界」).
//!
//! `validate` runs on the calling thread of `Terminals::open` and does no IO.
//! `resolve` runs on the terminal's thread and does: it canonicalizes the
//! working directory, asks for the login environment and looks the program up.
//! Programs and arguments come only from the fixed table here; a source gives
//! a session ID and nothing else.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_logging::warn;
use lectern_platform::login_env::{find_program, login_env, user_shell};
use lectern_services::project::ProjectKey;
use lectern_services::session::{Place, ResumeCommand, SessionKey};
use lectern_services::terminal::{
    AgentCommand, FailReason, Launch, LaunchError, LaunchKind, TerminalId,
};

use crate::diag::EnvSource;
use crate::pty::Program;
use crate::table::Shared;

const CLAUDE_SOURCE: &str = "session-claude-code";
const CODEX_SOURCE: &str = "session-codex";

/// A launch that passed the checks of the calling thread.
#[derive(Clone)]
pub(crate) struct Prepared {
    pub(crate) kind: LaunchKind,
    pub(crate) project: ProjectKey,
    /// The working directory as given: for a resume, the session log's `cwd`
    /// unchanged (the agent looks its sessions up by it).
    pub(crate) cwd: Arc<Path>,
    pub(crate) plan: Plan,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Plan {
    /// `argv` after the program name.
    Agent {
        agent: AgentCommand,
        args: Vec<OsString>,
    },
    Shell,
}

/// The checks of `open` that need no IO (design 「安全边界」, in this order).
pub(crate) fn validate(shared: &Shared, launch: &Launch) -> Result<Prepared, LaunchError> {
    match launch {
        Launch::Resume(key) => resume(shared, key),
        Launch::NewSession { agent, cwd } => {
            let project = locate(shared, cwd)?;
            Ok(Prepared {
                kind: LaunchKind::NewSession(*agent),
                project,
                cwd: cwd.clone(),
                plan: Plan::Agent {
                    agent: *agent,
                    args: Vec::new(),
                },
            })
        }
        Launch::Shell { cwd } => {
            let project = locate(shared, cwd)?;
            Ok(Prepared {
                kind: LaunchKind::Shell,
                project,
                cwd: cwd.clone(),
                plan: Plan::Shell,
            })
        }
    }
}

/// The project of a canonical path among the open projects.
fn locate(shared: &Shared, cwd: &Path) -> Result<ProjectKey, LaunchError> {
    let projects = shared.projects.get().ok_or(LaunchError::OutsideProject)?;
    projects
        .locate(cwd)
        .map(|location| location.project)
        .ok_or(LaunchError::OutsideProject)
}

fn resume(shared: &Shared, key: &SessionKey) -> Result<Prepared, LaunchError> {
    // 1. A built-in source. A process plugin's proxy has an ID the host
    //    generates (`ext.<plugin>.<id>`), never one of these.
    let source = match &*key.source.0 {
        CLAUDE_SOURCE => AgentCommand::ClaudeCode,
        CODEX_SOURCE => AgentCommand::Codex,
        _ => return Err(LaunchError::UntrustedSource),
    };
    // 2. The provider is there and lists the session.
    let provider = shared
        .sources
        .current()
        .into_iter()
        .find(|provider| provider.id() == key.source)
        .ok_or(LaunchError::UnknownSession)?;
    let summary = provider
        .sessions()
        .get(key)
        .cloned()
        .ok_or(LaunchError::UnknownSession)?;
    // 3. The source gives a resume command.
    let command = provider.resume(key).ok_or(LaunchError::NotResumable)?;
    drop(provider);
    // 4. The command matches the source and the key, and the ID is a
    //    canonical UUID.
    let (agent, session_id, args) = match (&command, source) {
        (ResumeCommand::ClaudeCode { session_id }, AgentCommand::ClaudeCode) => {
            (AgentCommand::ClaudeCode, session_id.clone(), "--resume")
        }
        (ResumeCommand::Codex { session_id }, AgentCommand::Codex) => {
            (AgentCommand::Codex, session_id.clone(), "resume")
        }
        _ => return Err(LaunchError::InvalidSessionId),
    };
    if *session_id != *key.id || !is_canonical_uuid(&session_id) {
        return Err(LaunchError::InvalidSessionId);
    }
    // 5. The session is placed in a project that is still open.
    let Place::Located(location) = &summary.place else {
        return Err(LaunchError::OutsideProject);
    };
    let projects = shared.projects.get().ok_or(LaunchError::OutsideProject)?;
    if !projects.open_projects().contains(&location.project) {
        return Err(LaunchError::OutsideProject);
    }
    drop(projects);
    Ok(Prepared {
        kind: LaunchKind::Resume(key.clone()),
        project: location.project.clone(),
        cwd: summary.cwd.clone(),
        plan: Plan::Agent {
            agent,
            args: vec![OsString::from(args), OsString::from(&*session_id)],
        },
    })
}

/// 36 characters, 8-4-4-4-12, lowercase hexadecimal.
pub(crate) fn is_canonical_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(at, &b)| match at {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

/// Everything the process needs, found on the terminal's thread.
pub(crate) struct Resolved {
    pub(crate) program: Program,
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) login_env: EnvSource,
    /// The program's file name for the log, from the fixed table.
    pub(crate) program_name: String,
    pub(crate) argc: usize,
}

/// The IO of a start: the working directory, the environment, the program
/// (design 「数据流与线程」, 「启动」). `Err` is the terminal's `Failed` reason.
pub(crate) fn resolve(
    shared: &Shared,
    id: TerminalId,
    prepared: &Prepared,
) -> Result<Resolved, FailReason> {
    check_cwd(shared, &prepared.cwd, &prepared.project)?;
    let (base, login, program) = match &prepared.plan {
        Plan::Agent { agent, args } => {
            let (mut base, login) = agent_env(shared, id);
            // portable-pty writes a `$SHELL` that is not executable to the
            // `log` facade; keep such a value out of the environment (the
            // child gets the user database's shell instead).
            base.retain(|(name, value)| name != "SHELL" || is_executable(Path::new(value)));
            let path = base
                .iter()
                .rev()
                .find(|(name, _)| name == "PATH")
                .map(|(_, value)| value.as_os_str())
                .and_then(|path| find_program(agent.program(), path))
                .ok_or(FailReason::NotInstalled(*agent))?;
            let argc = 1 + args.len();
            (
                base,
                login,
                (
                    Program::Agent {
                        path,
                        args: args.clone(),
                    },
                    agent.program().to_owned(),
                    argc,
                ),
            )
        }
        Plan::Shell => {
            let path = shell(shared).ok_or(FailReason::NoShell)?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (
                shell_env(shared),
                EnvSource::NotNeeded,
                (Program::Shell { path }, name, 1),
            )
        }
    };
    let (program, program_name, argc) = program;
    let env = build_env(base, &prepared.cwd);
    Ok(Resolved {
        program,
        env,
        login_env: login,
        program_name,
        argc,
    })
}

/// The working directory exists, is a directory, and (canonicalized) is in
/// the project the terminal was opened for (design 「安全边界」 6).
fn check_cwd(shared: &Shared, cwd: &Path, project: &ProjectKey) -> Result<(), FailReason> {
    let canonical = std::fs::canonicalize(cwd).map_err(|_| FailReason::CwdMissing)?;
    if !std::fs::metadata(&canonical).is_ok_and(|m| m.is_dir()) {
        return Err(FailReason::CwdMissing);
    }
    let projects = shared.projects.get().ok_or(FailReason::OutsideProject)?;
    match projects.locate(&canonical) {
        Some(location) if &location.project == project => Ok(()),
        _ => Err(FailReason::OutsideProject),
    }
}

/// The environment of an agent: the login shell's, else Lectern's own.
fn agent_env(shared: &Shared, id: TerminalId) -> (Vec<(OsString, OsString)>, EnvSource) {
    if let Some(vars) = &shared.config.login_env {
        return (vars.clone(), EnvSource::Captured);
    }
    match login_env() {
        Ok(env) => (env.vars.to_vec(), EnvSource::Captured),
        Err(_) => {
            warn!("login environment unavailable"; term = id.0);
            (std::env::vars_os().collect(), EnvSource::Fallback)
        }
    }
}

/// A shell starts with Lectern's own environment (it reads its own
/// configuration), or the test's.
fn shell_env(shared: &Shared) -> Vec<(OsString, OsString)> {
    match &shared.config.login_env {
        Some(vars) => vars.clone(),
        None => std::env::vars_os().collect(),
    }
}

fn shell(shared: &Shared) -> Option<PathBuf> {
    match &shared.config.shell {
        Some(path) => is_executable(path).then(|| path.clone()),
        None => user_shell(),
    }
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The child's environment (design 「子进程的环境」): `base` without the
/// `LECTERN_` variables and the ones set here, then `TERM`, `COLORTERM`,
/// `TERM_PROGRAM`, `TERM_PROGRAM_VERSION` and `PWD`, and `LC_CTYPE=UTF-8`
/// when no locale variable is set.
pub(crate) fn build_env(base: Vec<(OsString, OsString)>, cwd: &Path) -> Vec<(OsString, OsString)> {
    const SET: [&str; 5] = [
        "TERM",
        "COLORTERM",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "PWD",
    ];
    let has = |name: &str| {
        base.iter()
            .any(|(n, v)| n.as_os_str() == OsStr::new(name) && !v.is_empty())
    };
    let locale = has("LANG") || has("LC_ALL") || has("LC_CTYPE");
    let mut env: Vec<(OsString, OsString)> = base
        .into_iter()
        .filter(|(name, _)| {
            !name.as_bytes().starts_with(b"LECTERN_") && !SET.iter().any(|set| name == set)
        })
        .collect();
    let mut set = |name: &str, value: &OsStr| env.push((name.into(), value.to_owned()));
    set("TERM", OsStr::new("xterm-256color"));
    set("COLORTERM", OsStr::new("truecolor"));
    set("TERM_PROGRAM", OsStr::new("Lectern"));
    set(
        "TERM_PROGRAM_VERSION",
        OsStr::new(env!("CARGO_PKG_VERSION")),
    );
    set("PWD", cwd.as_os_str());
    if !locale {
        set("LC_CTYPE", OsStr::new("UTF-8"));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str, value: &str) -> (OsString, OsString) {
        (name.into(), value.into())
    }

    fn get<'a>(env: &'a [(OsString, OsString)], name: &str) -> Option<&'a OsStr> {
        env.iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_os_str())
    }

    #[test]
    fn launch_uuid_check_accepts_only_the_canonical_form() {
        assert!(is_canonical_uuid("00112233-4455-6677-8899-aabbccddeeff"));
        assert!(!is_canonical_uuid("00112233-4455-6677-8899-AABBCCDDEEFF"));
        assert!(!is_canonical_uuid("00112233445566778899aabbccddeeff"));
        assert!(!is_canonical_uuid("00112233-4455-6677-8899-aabbccddeef "));
        assert!(!is_canonical_uuid("00112233-4455-6677-8899-aabbccddeeffa"));
        assert!(!is_canonical_uuid(""));
    }

    #[test]
    fn launch_env_drops_lectern_variables_and_sets_the_terminal_ones() {
        let env = build_env(
            vec![
                var("PATH", "/bin"),
                var("LECTERN_TOKEN", "x"),
                var("TERM", "dumb"),
                var("PWD", "/old"),
                var("LANG", "en_US.UTF-8"),
            ],
            Path::new("/work"),
        );
        assert_eq!(get(&env, "PATH"), Some(OsStr::new("/bin")));
        assert_eq!(get(&env, "LECTERN_TOKEN"), None);
        assert_eq!(get(&env, "TERM"), Some(OsStr::new("xterm-256color")));
        assert_eq!(get(&env, "COLORTERM"), Some(OsStr::new("truecolor")));
        assert_eq!(get(&env, "TERM_PROGRAM"), Some(OsStr::new("Lectern")));
        assert_eq!(get(&env, "PWD"), Some(OsStr::new("/work")));
        assert_eq!(get(&env, "LC_CTYPE"), None);
        assert_eq!(env.iter().filter(|(n, _)| n == "TERM").count(), 1);
    }

    #[test]
    fn launch_env_sets_a_utf8_locale_when_none_is_set() {
        let env = build_env(vec![var("PATH", "/bin")], Path::new("/work"));
        assert_eq!(get(&env, "LC_CTYPE"), Some(OsStr::new("UTF-8")));
        let env = build_env(vec![var("LC_ALL", "C")], Path::new("/work"));
        assert_eq!(get(&env, "LC_CTYPE"), None);
    }
}
