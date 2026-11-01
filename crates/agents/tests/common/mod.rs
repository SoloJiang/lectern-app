//! Shared by the ACP client tests (`tests/acp_*.rs`): a temporary directory with the fake agent under the
//! names of the two adapters, an open project, fake session sources and a real kernel and executor around
//! the "conversations" plugin.
//!
//! Nothing here runs a real adapter, reads the user's login shell, the session directories or the
//! network; the fake agent (`src/bin/fake_acp.rs`) records how it was started and what it was sent, and
//! plays the script the test names (`tests/fixtures/acp/`). The plugin's clock is a `ManualClock`, so the
//! ten idle minutes and the thirty-second handshake are advanced, not waited for.

#![allow(dead_code, reason = "each test file uses part of the kit")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lectern_agents::testing::acp::{
    FakeProjects, FakeSource, Spec, claude_source, codex_source, folder, key,
};
use lectern_agents::{ConversationsDeps, Probe, conversations_plugin};
use lectern_executor::{Config, Executor};
use lectern_kernel::testing::{ManualClock, TestKernel};
use lectern_services::conversation::{
    ConversationCommand, ConversationInterest, ConversationSnapshot, Conversations, EntryBody,
    Link, PromptOrigin,
};
use lectern_services::project::{Location, PackageMatch, ProjectEvent, ProjectKey, Projects};
use lectern_services::session::{Agent, SessionKey, SessionSource};
use serde_json::Value;
use tempfile::TempDir;

pub const FAKE: &str = env!("CARGO_BIN_EXE_lectern-agents-fake-acp");

/// How long a test waits for something that must happen.
const PATIENCE: Duration = Duration::from_secs(30);

/// Polls `check` until it gives a value; panics with `what` after the patience runs out.
pub fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A process exists (a zombie that nobody reaped does too).
pub fn alive(pid: u32) -> bool {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    rustix::process::test_kill_process(pid).is_ok()
}

pub fn wait_dead(pid: u32) {
    wait_for(&format!("process {pid} to end"), || {
        (!alive(pid)).then_some(())
    });
}

pub struct Harness {
    pub dir: TempDir,
    /// The canonical path of the temporary directory.
    pub root: PathBuf,
    /// `claude-agent-acp` and `codex-acp`: links to the fake agent.
    pub bin: PathBuf,
    /// `FAKE_ACP_DIR`: the records.
    pub state: PathBuf,
    /// The open project's folder, the working directory of every session.
    pub project: PathBuf,
    pub project_key: ProjectKey,
    pub clock: ManualClock,
    pub claude: Arc<FakeSource>,
    pub codex: Arc<FakeSource>,
    pub projects: Arc<FakeProjects>,
    pub conversations: Arc<dyn Conversations>,
    pub kernel: TestKernel,
    interests: std::sync::Mutex<Vec<ConversationInterest>>,
}

impl Harness {
    /// A running plugin whose fake agent plays `tests/fixtures/acp/<script>`.
    pub fn new(script: &str) -> Harness {
        Harness::with_vars(script, Vec::new())
    }

    /// The same, with more variables in the login environment.
    pub fn with_vars(script: &str, extra: Vec<(&str, &str)>) -> Harness {
        let dir = TempDir::new().expect("temporary directory");
        let root = std::fs::canonicalize(dir.path()).expect("canonical path");
        let bin = root.join("bin");
        let state = root.join("state");
        let project = root.join("project");
        for path in [&bin, &state, &project] {
            std::fs::create_dir(path).expect("directory");
        }
        for name in ["claude-agent-acp", "codex-acp"] {
            std::os::unix::fs::symlink(FAKE, bin.join(name)).expect("link to the fake agent");
        }
        let script_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp")
            .join(script);
        assert!(script_path.exists(), "{}", script_path.display());
        let mut vars: Vec<(std::ffi::OsString, std::ffi::OsString)> = vec![
            ("PATH".into(), bin.clone().into()),
            ("FAKE_ACP_DIR".into(), state.clone().into()),
            ("FAKE_ACP_SCRIPT".into(), script_path.into()),
            ("FAKE_ACP_ECHO".into(), "PWD,HOME".into()),
            ("HOME".into(), "/home/test".into()),
            ("LECTERN_TOKEN".into(), "must-not-reach-the-agent".into()),
        ];
        for (name, value) in extra {
            vars.push((name.into(), value.into()));
        }

        let executor = Executor::new(Config::default());
        let clock = ManualClock::new();
        let (_, plugin) = conversations_plugin(ConversationsDeps { executor });
        let plugin = plugin
            .with_login_env(vars)
            .with_clock(Arc::new(clock.clone()));
        let probe: Probe = plugin.probe();

        let project_key = folder(project.to_str().expect("utf-8 path"));
        let claude = FakeSource::new(claude_source(), Agent::ClaudeCode);
        let codex = FakeSource::new(codex_source(), Agent::Codex);
        let projects = Arc::new(FakeProjects::default());
        projects.set_open(vec![project_key.clone()]);
        projects.set_location(Some(Location {
            project: project_key.clone(),
            worktree: None,
            rel: "".into(),
            package: PackageMatch::NotLoaded,
        }));
        let kernel = TestKernel::new(plugin)
            .mock::<dyn SessionSource>(claude.clone())
            .mock::<dyn SessionSource>(codex.clone())
            .mock::<dyn Projects>(projects.clone());
        kernel.activate().expect("the plugin activates");
        let conversations = probe.conversations().expect("activated");
        Harness {
            dir,
            root,
            bin,
            state,
            project,
            project_key,
            clock,
            claude,
            codex,
            projects,
            conversations,
            kernel,
            interests: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// A resumable Claude Code session in the open project, with one turn of history.
    pub fn session(&self, n: u32) -> SessionKey {
        let session = key(
            &claude_source(),
            &format!("c1a0de00-1111-4111-8111-{n:012x}"),
        );
        self.claude.add(
            &session,
            Spec {
                cwd: self.project.clone(),
                ..Spec::located(&session, self.project_key.clone(), &Agent::ClaudeCode)
            },
        );
        let turn = self.claude.turn(
            1,
            "history prompt",
            vec![self.claude.agent_text("history reply")],
        );
        self.claude.set_turns(&session, vec![turn]);
        session
    }

    /// The same for Codex.
    pub fn codex_session(&self, n: u32) -> SessionKey {
        let session = key(
            &codex_source(),
            &format!("c0de0000-2222-4222-8222-{n:012x}"),
        );
        self.codex.add(
            &session,
            Spec {
                cwd: self.project.clone(),
                ..Spec::located(&session, self.project_key.clone(), &Agent::Codex)
            },
        );
        let turn = self.codex.turn(1, "history prompt", vec![]);
        self.codex.set_turns(&session, vec![turn]);
        session
    }

    pub fn snapshot(&self, session: &SessionKey) -> Arc<ConversationSnapshot> {
        self.conversations.snapshot(session).expect("a snapshot")
    }

    /// Watches the session (until the harness drops) and waits for the history.
    pub fn watch(&self, session: &SessionKey) {
        let interest = self.conversations.watch(session);
        self.interests.lock().unwrap().push(interest);
        self.wait(session, "the history", |s| {
            s.load != lectern_services::conversation::Load::Loading
        });
    }

    /// Forgets every watch.
    pub fn unwatch_all(&self) {
        self.interests.lock().unwrap().clear();
    }

    /// The pid of the one agent process that was started.
    pub fn started_pid(&self) -> u32 {
        wait_for("a process to start", || {
            let records = self.records();
            (records.len() == 1).then(|| records[0].pid)
        })
    }

    pub fn prompt(&self, session: &SessionKey, text: &str) {
        self.conversations.command(
            session,
            ConversationCommand::Prompt {
                text: text.into(),
                origin: PromptOrigin::User,
            },
        );
    }

    pub fn command(&self, session: &SessionKey, command: ConversationCommand) {
        self.conversations.command(session, command);
    }

    /// Waits until the snapshot satisfies `check`.
    pub fn wait(
        &self,
        session: &SessionKey,
        what: &str,
        check: impl Fn(&ConversationSnapshot) -> bool,
    ) -> Arc<ConversationSnapshot> {
        wait_for(what, || {
            let snapshot = self.conversations.snapshot(session)?;
            check(&snapshot).then_some(snapshot)
        })
    }

    pub fn wait_link(&self, session: &SessionKey, link: Link) -> Arc<ConversationSnapshot> {
        self.wait(session, &format!("{link:?}"), |s| s.link == link)
    }

    /// Prompts and waits until the turn is over.
    pub fn turn(&self, session: &SessionKey, text: &str) -> Arc<ConversationSnapshot> {
        self.prompt(session, text);
        self.wait(session, "the turn to end", |s| {
            s.link == Link::Idle && s.entries.iter().any(|e| is_prompt_with_text(&e.body, text))
        })
    }

    /// Every process the fake agent has started, in the order of their pids.
    pub fn records(&self) -> Vec<Record> {
        let mut records: Vec<Record> = std::fs::read_dir(&self.state)
            .expect("state directory")
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let pid: u32 = path.file_stem()?.to_str()?.parse().ok()?;
                (path.extension()? == "meta").then(|| Record::read(&self.state, pid))
            })
            .collect();
        records.sort_by_key(|record| record.pid);
        records
    }

    /// The record of the process that serves `session`, once it has seen the session's ID.
    pub fn record_of(&self, session: &SessionKey) -> Record {
        wait_for("the agent's record", || {
            self.records()
                .into_iter()
                .find(|record| record.session().as_deref() == Some(&*session.id))
        })
    }

    /// The pids of the helper children the fake agent started.
    pub fn children(&self) -> Vec<u32> {
        std::fs::read_dir(&self.state)
            .expect("state directory")
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().into_string().ok()?;
                name.strip_prefix("child-")?
                    .strip_suffix(".pid")?
                    .parse()
                    .ok()
            })
            .collect()
    }

    /// Closes the project in the fake `Projects` and announces it.
    pub fn close_project(&self) {
        self.projects.set_open(Vec::new());
        self.kernel
            .inner()
            .expect("kernel")
            .emitter()
            .emit(ProjectEvent::Closed {
                project: self.project_key.clone(),
            });
    }
}

fn is_prompt_with_text(body: &EntryBody, text: &str) -> bool {
    matches!(body, EntryBody::Prompt { text: t, .. } if &*t.text == text)
}

/// What the fake agent wrote about one process.
pub struct Record {
    pub pid: u32,
    pub meta: String,
    /// Every line it was sent, parsed.
    pub messages: Vec<Value>,
    /// The lines as they arrived.
    pub lines: Vec<String>,
}

impl Record {
    fn read(dir: &Path, pid: u32) -> Record {
        let meta = std::fs::read_to_string(dir.join(format!("{pid}.meta"))).unwrap_or_default();
        let lines: Vec<String> = std::fs::read_to_string(dir.join(format!("{pid}.in")))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        let messages = lines
            .iter()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        Record {
            pid,
            meta,
            messages,
            lines,
        }
    }

    /// The method names of the messages it was sent, in order (responses have none).
    pub fn methods(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter_map(|m| m["method"].as_str().map(str::to_owned))
            .collect()
    }

    pub fn calls(&self, method: &str) -> Vec<&Value> {
        self.messages
            .iter()
            .filter(|m| m["method"].as_str() == Some(method))
            .collect()
    }

    /// The response to the request the fake agent made with this `id`.
    pub fn response(&self, id: &str) -> Option<&Value> {
        self.messages
            .iter()
            .find(|m| m.get("method").is_none() && m["id"].as_str() == Some(id))
    }

    /// The session ID of the first message that carries one.
    pub fn session(&self) -> Option<String> {
        self.messages
            .iter()
            .find_map(|m| m["params"]["sessionId"].as_str().map(str::to_owned))
    }

    pub fn args(&self) -> Vec<&str> {
        self.meta
            .lines()
            .filter_map(|line| line.strip_prefix("arg="))
            .collect()
    }

    pub fn cwd(&self) -> Option<&str> {
        self.meta.lines().find_map(|line| line.strip_prefix("cwd="))
    }

    pub fn has_env(&self, name: &str) -> bool {
        self.meta.lines().any(|line| line == format!("env={name}"))
    }

    pub fn var(&self, name: &str) -> Option<&str> {
        let prefix = format!("var:{name}=");
        self.meta
            .lines()
            .find_map(|line| line.strip_prefix(prefix.as_str()))
    }
}
