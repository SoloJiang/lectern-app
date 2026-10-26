//! The measurement build end to end (docs/design/app.md「测量构建的装配」
//! 「测试」): launch the real `lectern`, built with `bench-probe`, with a
//! temporary `HOME` and `LECTERN_BENCH_SOCKET`. The test plays the runner:
//! it listens on a Unix domain socket, reads the probe's reports and sends
//! its requests. `CLAUDE_CONFIG_DIR`, `CODEX_HOME` and `LECTERN_LOG` are
//! removed (the canary test sets the last one), so the session sources look
//! only under the temporary `HOME`. Needs a macOS graphical session, like
//! `tests/startup.rs`; the timeouts only end a hang.

#![cfg(feature = "bench-probe")]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use lectern_bench::wire::{OpenTarget, PluginReport, Report, Request, TimeSource};

const HANG: Duration = Duration::from_secs(30);
/// How long the process may take to quit after `Quit` (docs/design/app.md).
const QUIT: Duration = Duration::from_secs(10);
/// `sun_path` of macOS holds 104 bytes with the terminating NUL.
const SUN_PATH: usize = 104;

/// The runner's side: the temporary `HOME`, the socket, and the process,
/// which is killed if the test ends before it has quit.
struct Session {
    home: tempfile::TempDir,
    child: Child,
    requests: UnixStream,
    reports: Receiver<Report>,
    /// Keeps the socket's directory until the test ends.
    _socket_dir: tempfile::TempDir,
}

impl Session {
    /// Launches `lectern` and waits for its connection (the first frame).
    /// `plugins` is `LECTERN_BENCH_PLUGINS`; `canary` names the socket's
    /// directory; `lectern_log` is the log filter.
    fn launch(plugins: Option<&str>, canary: Option<&str>, lectern_log: Option<&str>) -> Session {
        // Under the system temporary directory, not the test's own: the
        // socket's path must fit `sun_path`.
        let socket_dir = tempfile::Builder::new()
            .prefix(canary.unwrap_or("lbs"))
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        let socket = socket_dir.path().join("s");
        assert!(
            socket.as_os_str().len() < SUN_PATH,
            "the socket path is too long for sun_path: {}",
            socket.display()
        );
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_lectern"));
        command
            .env("HOME", home.path())
            .env("LECTERN_BENCH_SOCKET", &socket)
            .env_remove("LECTERN_BENCH_JUDGE")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME");
        match plugins {
            Some(plugins) => command.env("LECTERN_BENCH_PLUGINS", plugins),
            None => command.env_remove("LECTERN_BENCH_PLUGINS"),
        };
        match lectern_log {
            Some(filter) => command.env("LECTERN_LOG", filter),
            None => command.env_remove("LECTERN_LOG"),
        };
        let mut child = command.spawn().unwrap();
        let start = Instant::now();
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > HANG {
                        let _ = child.kill();
                        panic!("lectern did not connect within {HANG:?}");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("{e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        let (sender, reports) = mpsc::channel();
        let reading = stream.try_clone().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(reading).lines() {
                let Ok(line) = line else { break };
                let report = Report::decode(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
                if sender.send(report).is_err() {
                    break;
                }
            }
        });
        Session {
            home,
            child,
            requests: stream,
            reports,
            _socket_dir: socket_dir,
        }
    }

    fn send(&mut self, request: &Request) {
        let line = request.encode();
        self.requests
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
    }

    /// The next report that `pick` takes; the others (`Busy`, `Quiescent`)
    /// are skipped.
    fn next<T>(&self, what: &str, mut pick: impl FnMut(&Report) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            let left = HANG.saturating_sub(start.elapsed());
            match self.reports.recv_timeout(left) {
                Ok(report) => {
                    if let Some(found) = pick(&report) {
                        return found;
                    }
                }
                Err(RecvTimeoutError::Timeout) => panic!("no {what} within {HANG:?}"),
                Err(RecvTimeoutError::Disconnected) => panic!("the probe hung up before {what}"),
            }
        }
    }

    /// `Quit`, then waits for the exit.
    fn quit(&mut self) -> ExitStatus {
        self.send(&Request::Quit);
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                start.elapsed() < QUIT,
                "lectern did not exit within {QUIT:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn logs(&self) -> PathBuf {
        self.home.path().join("Library/Logs/Lectern")
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text of every file under `dir`.
fn all_text(dir: &Path) -> String {
    let mut text = String::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return text;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            text.push_str(&all_text(&path));
        } else if let Ok(file) = fs::read_to_string(&path) {
            text.push_str(&file);
        }
    }
    text
}

#[test]
fn first_frame_plugins_and_quit() {
    let mut session = Session::launch(None, None, None);
    let (pid, source) = session.next("FirstFrame", |report| match report {
        Report::FirstFrame { pid, source, .. } => Some((*pid, *source)),
        _ => None,
    });
    assert_eq!(pid, session.child.id());
    assert!(matches!(
        source,
        TimeSource::Presented | TimeSource::Handler
    ));

    // The kernel's status is filled by its thread: ask once it is quiescent,
    // as the runner does (docs/design/bench.md「进程级基准」).
    session.next("Quiescent", |report| {
        matches!(report, Report::Quiescent { .. }).then_some(())
    });
    session.send(&Request::Plugins);
    let mut plugins: Vec<PluginReport> = Vec::new();
    session.next("PluginsEnd", |report| match report {
        Report::Plugin(plugin) => {
            plugins.push(plugin.clone());
            None
        }
        Report::PluginsEnd => Some(()),
        _ => None,
    });
    // The desktop profile's plugins; the probe's own is not among them.
    assert_eq!(plugins.len(), 11, "{plugins:?}");
    assert!(plugins.iter().all(|plugin| plugin.id != "bench-probe"));
    assert!(plugins.iter().any(|plugin| plugin.id == "workspace"));

    session.send(&Request::Epoch);
    session.next("Epoch", |report| {
        matches!(report, Report::Epoch(_)).then_some(())
    });

    // A kind no benchmark of this step uses is an error for the runner.
    session.send(&Request::Open(OpenTarget::Scroll));
    let message = session.next("Error", |report| match report {
        Report::Error { message } => Some(message.clone()),
        _ => None,
    });
    assert_eq!(message, "open scroll unsupported");

    assert!(session.quit().success());
}

#[test]
fn a_trimmed_set_becomes_ready() {
    let mut session = Session::launch(Some("closure+self:project"), None, None);
    session.next("Ready", |report| {
        matches!(report, Report::Ready).then_some(())
    });
    assert!(session.quit().success());
}

/// The socket's path (here a directory named with a canary) and the other
/// values of the measurement build stay out of the log, at trace level.
#[test]
fn bench_socket_stays_out_of_the_log() {
    let canary = "canary-3b7d52e1";
    let mut session = Session::launch(Some("closure:project"), Some(canary), Some("trace"));
    session.next("Ready", |report| {
        matches!(report, Report::Ready).then_some(())
    });
    assert!(session.quit().success());
    let text = all_text(&session.logs());
    // The attach is logged, so the check below is not about an empty log.
    assert!(text.contains("bench probe attached"), "{text}");
    assert!(!text.contains(canary), "{text}");
    assert!(!text.contains("closure:project"), "{text}");
}
