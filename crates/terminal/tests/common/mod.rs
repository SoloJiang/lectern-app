//! Shared by the PTY tests: a temporary directory with the fake program
//! under the names of the agents, a project folder, and the environment that
//! makes the plugin start them.
//!
//! Nothing here runs a real agent, reads the user's login shell or touches
//! the network; the fake program (`src/bin/fake.rs`) records how it was
//! started and plays the plan the test wrote.

#![allow(dead_code, reason = "each test file uses part of the kit")]

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lectern_services::project::{Location, PackageMatch, ProjectKey, Projects};
use lectern_services::session::{Place, ResumeCommand, SessionKey};
use lectern_services::terminal::{Launch, TerminalId};
use lectern_terminal::TerminalPlugin;
use lectern_terminal::testing::{self, Harness};
use tempfile::TempDir;

pub const FAKE: &str = env!("CARGO_BIN_EXE_lectern-terminal-fake");
pub const UUID_A: &str = "00112233-4455-6677-8899-aabbccddeeff";
pub const UUID_B: &str = "ffeeddcc-bbaa-9988-7766-554433221100";

pub struct Kit {
    _dir: TempDir,
    /// The canonical path of the temporary directory.
    pub root: PathBuf,
    /// `claude` and `codex`: links to the fake program.
    pub bin: PathBuf,
    /// `FAKE_TERMINAL_DIR`: the plan and the records.
    pub state: PathBuf,
    /// An open project's folder.
    pub project: PathBuf,
}

impl Kit {
    pub fn new() -> Kit {
        let dir = TempDir::new().expect("temporary directory");
        let root = std::fs::canonicalize(dir.path()).expect("canonical path");
        let bin = root.join("bin");
        let state = root.join("state");
        let project = root.join("project");
        for path in [&bin, &state, &project] {
            std::fs::create_dir(path).expect("directory");
        }
        let kit = Kit {
            _dir: dir,
            root,
            bin,
            state,
            project,
        };
        kit.install("claude");
        kit.install("codex");
        kit
    }

    /// A link named `name` in `bin` to the fake program.
    pub fn install(&self, name: &str) {
        std::os::unix::fs::symlink(FAKE, self.bin.join(name)).expect("link");
    }

    /// What the fake program plays (see `src/bin/fake.rs`).
    pub fn plan(&self, plan: &str) {
        std::fs::write(self.state.join("plan"), plan).expect("plan");
    }

    /// The environment of agents and shells: a `PATH` with only the fake
    /// agents, the fake's directory, and a `LECTERN_` variable that must
    /// not reach the program. No locale variable.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        vec![
            ("PATH".into(), self.bin.clone().into_os_string()),
            ("HOME".into(), self.root.clone().into_os_string()),
            (
                "FAKE_TERMINAL_DIR".into(),
                self.state.clone().into_os_string(),
            ),
            ("LECTERN_CANARY".into(), "must-not-reach-the-program".into()),
        ]
    }

    /// The plugin with the fake environment and the fake as the shell.
    pub fn plugin(&self) -> TerminalPlugin {
        Harness::plugin()
            .with_login_env(self.env())
            .with_shell(FAKE)
    }

    pub fn harness(&self) -> Harness {
        let harness = Harness::start(self.plugin());
        harness.projects.add(&self.project);
        harness
    }

    pub fn project_key(&self, harness: &Harness) -> ProjectKey {
        Projects::open_projects(&*harness.projects)
            .into_iter()
            .next()
            .expect("an open project")
    }

    /// What the fake recorded for the process `pid`, once it has.
    pub fn record(&self, harness: &Harness, pid: u32) -> Record {
        let path = self.state.join(format!("{pid}.txt"));
        let text = harness
            .wait_for(Duration::from_secs(20), || {
                std::fs::read_to_string(&path)
                    .ok()
                    .filter(|t| t.contains("tty="))
            })
            .expect("the fake program recorded its start");
        Record::parse(&text)
    }

    /// The records written so far.
    pub fn records(&self) -> usize {
        std::fs::read_dir(&self.state)
            .map(|dir| {
                dir.filter_map(Result::ok)
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".txt"))
                    .count()
            })
            .unwrap_or(0)
    }
}

#[derive(Debug)]
pub struct Record {
    pub argv0: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub names: Vec<String>,
    pub vars: HashMap<String, String>,
    pub tty: bool,
}

impl Record {
    fn parse(text: &str) -> Record {
        let mut record = Record {
            argv0: String::new(),
            args: Vec::new(),
            cwd: String::new(),
            names: Vec::new(),
            vars: HashMap::new(),
            tty: false,
        };
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("argv0=") {
                record.argv0 = v.into();
            } else if let Some(v) = line.strip_prefix("arg=") {
                record.args.push(v.into());
            } else if let Some(v) = line.strip_prefix("cwd=") {
                record.cwd = v.into();
            } else if let Some(v) = line.strip_prefix("env=") {
                record.names.push(v.into());
            } else if let Some(v) = line.strip_prefix("var:")
                && let Some((name, value)) = v.split_once('=')
            {
                record.vars.insert(name.into(), value.into());
            } else if let Some(v) = line.strip_prefix("tty=") {
                record.tty = v == "true";
            }
        }
        record
    }
}

/// A `Place` in `project`.
pub fn located(project: &ProjectKey) -> Place {
    Place::Located(Location {
        project: project.clone(),
        worktree: None,
        rel: Arc::from(""),
        package: PackageMatch::None,
    })
}

pub fn claude_command(id: &str) -> ResumeCommand {
    ResumeCommand::ClaudeCode {
        session_id: Arc::from(id),
    }
}

pub fn codex_command(id: &str) -> ResumeCommand {
    ResumeCommand::Codex {
        session_id: Arc::from(id),
    }
}

pub fn resume(key: &SessionKey) -> Launch {
    Launch::Resume(key.clone())
}

pub fn shell(cwd: &Path) -> Launch {
    Launch::Shell {
        cwd: Arc::from(cwd),
    }
}

pub fn size() -> lectern_services::terminal::GridSize {
    testing::size(80, 24)
}

/// Whether the process still exists (a zombie counts: the wait thread reaps
/// it at once).
pub fn alive(pid: u32) -> bool {
    use rustix::process::{Pid, test_kill_process};
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    test_kill_process(pid).is_ok()
}

/// Waits until the process is gone; false after `timeout`.
pub fn wait_gone(harness: &Harness, pid: u32, timeout: Duration) -> bool {
    harness
        .wait_for(timeout, || (!alive(pid)).then_some(()))
        .is_some()
}

pub fn running_pid(info: &lectern_services::terminal::TerminalInfo) -> u32 {
    match info.state {
        lectern_services::terminal::RunState::Running { pid } => pid,
        other => panic!("not running: {other:?}"),
    }
}

pub fn id(n: u64) -> TerminalId {
    TerminalId(n)
}
