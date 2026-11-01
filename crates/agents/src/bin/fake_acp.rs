//! The fake ACP agent of the conversation tests (docs/design/agents.md「ACP 客户端」的「测试」): it
//! stands in for `claude-agent-acp` and `codex-acp`, records how it was started and what it was sent,
//! and plays a synthetic script. It reads no user data and no network.
//!
//! The environment says what to do (the tests give it through `with_login_env`):
//!
//! - `FAKE_ACP_DIR=<dir>`: write `<pid>.meta` (argv, working directory, the names of the environment
//!   variables, the values of the ones listed in `FAKE_ACP_ECHO`) and `<pid>.in` (every line received,
//!   verbatim) to `<dir>`.
//! - `FAKE_ACP_SCRIPT=<file>`: the script, one rule per line:
//!   `{"on":"<method>","do":[<action>, ...]}`. A message with that method runs the actions of the first
//!   rule for it that has not run yet (the last one again when all have); a request no rule matches gets
//!   a "method not found" error, a notification is ignored.
//! - `FAKE_ACP_CHILD=<dir>`: the fake's own helper child (see `spawn_child`); never set by tests.
//!
//! Actions, run in order; `$SESSION` in a message is replaced by the session ID the client last sent:
//! `{"reply": <result>}` and `{"error": {"code": n, "message": "..."}}` answer the request being handled,
//! `{"send": <message>}` writes a message, `{"ask": {"id": <id>, "params": <params>}}` sends a
//! `session/request_permission` request and waits for its answer, `{"wait_for": "<method>"}` waits for a
//! message of that method, `{"raw": "<text>"}` writes a line that is not JSON, `{"flood": <bytes>}` writes
//! that many bytes without a newline, `{"stderr": "<text>"}` writes to stderr, `{"spawn_child": true}`
//! starts a helper child in the same process group, `{"ignore_term": true}` ignores SIGTERM,
//! `{"exit": <code>}` exits. After `ignore_term` the agent also stays when its input ends.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

fn main() {
    if let Some(dir) = std::env::var_os("FAKE_ACP_CHILD") {
        child(Path::new(&dir));
        return;
    }
    let dir = std::env::var_os("FAKE_ACP_DIR").map(PathBuf::from);
    let pid = std::process::id();
    if let Some(dir) = &dir {
        record_start(&dir.join(format!("{pid}.meta")));
    }
    let script = std::env::var_os("FAKE_ACP_SCRIPT")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let rules: Vec<Rule> = script
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .map(|rule| Rule {
            on: rule["on"].as_str().unwrap_or_default().to_owned(),
            actions: rule["do"].as_array().cloned().unwrap_or_default(),
            used: false,
        })
        .collect();
    let mut agent = Agent {
        rules,
        input: dir.map(|dir| dir.join(format!("{pid}.in"))),
        backlog: VecDeque::new(),
        session: String::new(),
        dir: std::env::var_os("FAKE_ACP_DIR").map(PathBuf::from),
        stubborn: false,
    };
    agent.serve();
}

struct Rule {
    on: String,
    actions: Vec<Value>,
    used: bool,
}

struct Agent {
    rules: Vec<Rule>,
    input: Option<PathBuf>,
    /// Lines read while waiting for something else.
    backlog: VecDeque<String>,
    session: String,
    dir: Option<PathBuf>,
    /// `ignore_term` ran: the agent also stays when its input ends, until it is killed.
    stubborn: bool,
}

impl Agent {
    fn serve(&mut self) {
        loop {
            let Some(line) = self.next_line() else {
                if self.stubborn {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(3600));
                    }
                }
                return;
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(method) = message["method"].as_str() else {
                // A response nobody waits for.
                continue;
            };
            if let Some(session) = message["params"]["sessionId"].as_str() {
                self.session = session.to_owned();
            }
            let id = message.get("id").cloned();
            let method = method.to_owned();
            self.handle(&method, id);
        }
    }

    /// The next line: from the backlog, else from stdin (recorded). None at end of input.
    fn next_line(&mut self) -> Option<String> {
        if let Some(line) = self.backlog.pop_front() {
            return Some(line);
        }
        self.read_stdin()
    }

    fn read_stdin(&mut self) -> Option<String> {
        let mut line = String::new();
        let read = std::io::stdin().lock().read_line(&mut line).ok()?;
        if read == 0 {
            return None;
        }
        let line = line.trim_end_matches(['\n', '\r']).to_owned();
        if let Some(path) = &self.input
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
        Some(line)
    }

    fn handle(&mut self, method: &str, id: Option<Value>) {
        let at = self
            .rules
            .iter()
            .position(|rule| rule.on == method && !rule.used)
            .or_else(|| self.rules.iter().rposition(|rule| rule.on == method));
        let Some(at) = at else {
            if let Some(id) = id {
                let error = serde_json::json!({"code": -32601, "message": "method not found"});
                self.write(&serde_json::json!({"jsonrpc": "2.0", "id": id, "error": error}));
            }
            return;
        };
        self.rules[at].used = true;
        let actions = self.rules[at].actions.clone();
        for action in &actions {
            self.run(action, id.as_ref());
        }
    }

    fn run(&mut self, action: &Value, id: Option<&Value>) {
        let Some((name, arg)) = action.as_object().and_then(|o| o.iter().next()) else {
            return;
        };
        match name.as_str() {
            "reply" => self.write(&serde_json::json!({"jsonrpc": "2.0", "id": id, "result": arg})),
            "error" => self.write(&serde_json::json!({"jsonrpc": "2.0", "id": id, "error": arg})),
            "send" => self.write(arg),
            "ask" => {
                let ask_id = arg["id"].clone();
                self.write(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": ask_id,
                    "method": "session/request_permission",
                    "params": arg["params"],
                }));
                self.wait(|message| message.get("method").is_none() && message["id"] == ask_id);
            }
            "wait_for" => {
                let wanted = arg.as_str().unwrap_or_default().to_owned();
                self.wait(|message| message["method"].as_str() == Some(wanted.as_str()));
            }
            "raw" => self.write_raw(arg.as_str().unwrap_or_default().as_bytes(), true),
            "flood" => {
                let mut left = arg.as_u64().unwrap_or(0);
                let chunk = vec![b'x'; 64 << 10];
                while left > 0 {
                    let n = left.min(chunk.len() as u64) as usize;
                    self.write_raw(&chunk[..n], false);
                    left -= n as u64;
                }
            }
            "stderr" => {
                let mut err = std::io::stderr().lock();
                let _ = err.write_all(arg.as_str().unwrap_or_default().as_bytes());
                let _ = err.flush();
            }
            "spawn_child" => self.spawn_child(),
            "ignore_term" => {
                ignore_term();
                self.stubborn = true;
            }
            "exit" => std::process::exit(arg.as_i64().unwrap_or(0).try_into().unwrap_or(1)),
            _ => {}
        }
    }

    /// Reads messages (keeping the other ones for later) until `matches` accepts one. Stops at the end
    /// of input.
    fn wait(&mut self, matches: impl Fn(&Value) -> bool) {
        let mut kept = VecDeque::new();
        // What is already in the backlog comes first.
        let mut pending: VecDeque<String> = std::mem::take(&mut self.backlog);
        loop {
            let line = match pending.pop_front() {
                Some(line) => line,
                None => match self.read_stdin() {
                    Some(line) => line,
                    None => break,
                },
            };
            match serde_json::from_str::<Value>(&line) {
                Ok(message) if matches(&message) => break,
                _ => kept.push_back(line),
            }
        }
        kept.extend(pending);
        self.backlog = kept;
    }

    fn write(&self, message: &Value) {
        let text = serde_json::to_string(message)
            .unwrap_or_default()
            .replace("$SESSION", &self.session);
        self.write_raw(text.as_bytes(), true);
    }

    fn write_raw(&self, bytes: &[u8], newline: bool) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(bytes);
        if newline {
            let _ = out.write_all(b"\n");
        }
        let _ = out.flush();
    }

    fn spawn_child(&self) {
        let Some(dir) = &self.dir else {
            return;
        };
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let _ = std::process::Command::new(exe)
            .env("FAKE_ACP_CHILD", dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// The helper child: records its pid and sleeps until it is killed.
fn child(dir: &Path) {
    let pid = std::process::id();
    let _ = std::fs::write(dir.join(format!("child-{pid}.pid")), pid.to_string());
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

fn ignore_term() {
    // SAFETY: `signal` with `SIG_IGN` only changes the process's signal disposition.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
}

fn record_start(path: &Path) {
    let mut out = String::new();
    for (at, arg) in std::env::args().enumerate() {
        if at == 0 {
            out.push_str(&format!("argv0={arg}\n"));
        } else {
            out.push_str(&format!("arg={arg}\n"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push_str(&format!("cwd={}\n", cwd.display()));
    }
    let mut names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in &names {
        out.push_str(&format!("env={name}\n"));
    }
    if let Ok(echo) = std::env::var("FAKE_ACP_ECHO") {
        for name in echo.split(',').filter(|name| !name.is_empty()) {
            if let Ok(value) = std::env::var(name) {
                out.push_str(&format!("var:{name}={value}\n"));
            }
        }
    }
    let _ = std::fs::write(path, out);
}
