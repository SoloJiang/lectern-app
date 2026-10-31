use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use tempfile::TempDir;

use super::*;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/login_env/vars.bin");
const LONG: Duration = Duration::from_secs(5);
/// Sets `$1` and `$2` to the two markers in the command the shell received.
const MARKERS: &str = "set -- $(printf '%s' \"$4\" | grep -o '__lectern_env_[0-9a-f]*__')\n";

/// A temporary directory with a fake login shell: a `/bin/sh` script that
/// records its arguments, working directory and whether stdin is a terminal,
/// then runs `body`.
struct FakeShell {
    dir: TempDir,
    path: PathBuf,
}

impl FakeShell {
    fn new(body: &[u8]) -> FakeShell {
        FakeShell::named("shell", body)
    }

    fn named(name: &str, body: &[u8]) -> FakeShell {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join(name);
        let mut script = format!(
            "#!/bin/sh\nrec='{}'\nprintf '%s\\n' \"$@\" > \"$rec/args\"\npwd > \"$rec/cwd\"\n\
             if [ -t 0 ]; then echo tty > \"$rec/stdin\"; else echo not_tty > \"$rec/stdin\"; fi\n",
            root.display()
        )
        .into_bytes();
        script.extend_from_slice(body);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        FakeShell { dir, path }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn record(&self, name: &str) -> String {
        std::fs::read_to_string(self.root().join(name)).unwrap()
    }

    fn capture(&self, timeout: Duration) -> Result<LoginEnv, Failure> {
        capture(&self.path, &self.root(), timeout)
    }
}

/// Shell words that run the received command under exactly `vars`
/// (NUL-terminated `name=value` entries).
fn replay(vars: &[u8]) -> Vec<u8> {
    let mut line = b"/usr/bin/env -i".to_vec();
    for entry in vars.split(|&b| b == 0).filter(|e| !e.is_empty()) {
        line.extend_from_slice(b" '");
        for &b in entry {
            if b == b'\'' {
                line.extend_from_slice(b"'\\''");
            } else {
                line.push(b);
            }
        }
        line.push(b'\'');
    }
    line.extend_from_slice(b" /bin/sh -c \"$4\"\n");
    line
}

fn fixture_body(after: &str) -> Vec<u8> {
    let mut body = b"echo 'Welcome to the fake shell'\n".to_vec();
    body.extend(replay(FIXTURE));
    body.extend_from_slice(after.as_bytes());
    body
}

fn kind(result: Result<LoginEnv, Failure>) -> LoginEnvError {
    result.expect_err("the capture should fail").kind
}

fn gone(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// A killed process may stay a zombie until init reaps it.
fn wait_gone(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if gone(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn login_env_replays_the_fixture() {
    let shell = FakeShell::new(&fixture_body("echo 'bye'\n"));
    let env = shell.capture(LONG).unwrap();

    let expected = parse(FIXTURE).unwrap();
    assert_eq!(expected.len(), 7);
    for (name, value) in expected.iter() {
        let got = env.vars.iter().find(|(n, _)| n == name);
        assert_eq!(got.map(|(_, v)| v), Some(value), "{name:?}");
    }
    assert_eq!(env.vars.len(), expected.len(), "{:?}", env.vars);
    for dropped in ["_", "SHLVL", "PWD", "OLDPWD"] {
        assert_eq!(env.get(dropped), None, "{dropped}");
    }
    assert_eq!(env.get("MULTI"), Some(OsStr::new("line one\nline two")));
    assert_eq!(env.get("WITH_EQUALS"), Some(OsStr::new("a=b=c")));
    assert_eq!(env.get("NON_UTF8"), Some(OsStr::from_bytes(b"caf\xe9")));
    assert_eq!(env.get("EMPTY"), Some(OsStr::new("")));
    assert_eq!(env.get("ABSENT"), None);
}

#[test]
fn login_env_passes_login_interactive_flags() {
    let shell = FakeShell::new(&fixture_body(""));
    shell.capture(LONG).unwrap();

    let args = shell.record("args");
    let lines: Vec<&str> = args.lines().collect();
    assert_eq!(lines[..3], ["-l", "-i", "-c"], "{args}");
    assert_eq!(lines.len(), 4, "{args}");
    assert_eq!(lines[3].matches("__lectern_env_").count(), 2, "{args}");
    assert!(lines[3].contains("/usr/bin/env -0"), "{args}");
    assert_eq!(shell.record("stdin").trim(), "not_tty");
    assert_eq!(shell.record("cwd").trim(), shell.root().to_str().unwrap());
}

#[test]
fn login_env_markers_differ_between_runs() {
    let shell = FakeShell::new(&fixture_body(""));
    shell.capture(LONG).unwrap();
    let first = shell.record("args");
    shell.capture(LONG).unwrap();
    assert_ne!(first, shell.record("args"));
}

#[test]
fn login_env_without_markers_is_unparsable() {
    let shell = FakeShell::new(b"echo 'just a banner'\n");
    assert_eq!(kind(shell.capture(LONG)), LoginEnvError::Unparsable);
}

#[test]
fn login_env_only_the_start_marker_is_unparsable() {
    let shell = FakeShell::new(format!("{MARKERS}printf '%s\\n' \"$1\"\n").as_bytes());
    assert_eq!(kind(shell.capture(LONG)), LoginEnvError::Unparsable);
}

#[test]
fn login_env_an_entry_without_equals_is_unparsable() {
    let shell = FakeShell::new(
        format!(
            "{MARKERS}printf '%s\\n' \"$1\"\nprintf 'OK=1\\000noequals\\000'\nprintf '%s\\n' \"$2\"\n"
        )
        .as_bytes(),
    );
    assert_eq!(kind(shell.capture(LONG)), LoginEnvError::Unparsable);
}

#[test]
fn login_env_output_over_1_mib_is_unparsable() {
    let shell = FakeShell::new(b"exec yes\n");
    let start = Instant::now();
    assert_eq!(kind(shell.capture(LONG)), LoginEnvError::Unparsable);
    assert!(start.elapsed() < LONG, "{:?}", start.elapsed());
}

#[test]
fn login_env_missing_shell_is_spawn() {
    let dir = TempDir::new().unwrap();
    let failure = capture(&dir.path().join("absent"), dir.path(), LONG).unwrap_err();
    assert_eq!(failure.kind, LoginEnvError::Spawn);
    assert_eq!(failure.io.map(|e| e.kind()), Some(io::ErrorKind::NotFound));
}

#[test]
fn login_env_non_executable_shell_is_spawn() {
    let shell = FakeShell::new(b"");
    std::fs::set_permissions(&shell.path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(kind(shell.capture(LONG)), LoginEnvError::Spawn);
}

#[test]
fn login_env_times_out_and_kills_the_group() {
    let shell = FakeShell::new(
        b"sleep 30 &\necho $! > \"$rec/background\"\necho $$ > \"$rec/shell\"\n\
          touch \"$rec/ready\"\nsleep 30\n",
    );
    let start = Instant::now();
    let result = shell.capture(Duration::from_secs(2));
    assert_eq!(kind(result), LoginEnvError::Timeout);
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "{:?}",
        start.elapsed()
    );
    assert!(
        shell.root().join("ready").exists(),
        "the shell was too slow"
    );
    for name in ["background", "shell"] {
        let pid: i32 = shell.record(name).trim().parse().unwrap();
        assert!(wait_gone(pid), "{name} is still running");
    }
}

#[test]
fn login_env_waits_for_a_slow_exit() {
    let shell = FakeShell::new(&fixture_body("sleep 0.2\ntouch \"$rec/exited\"\n"));
    shell.capture(LONG).unwrap();
    assert!(shell.root().join("exited").exists());
}

#[test]
fn login_env_hanging_exit_still_succeeds() {
    let shell = FakeShell::new(&fixture_body("echo $$ > \"$rec/shell\"\nsleep 30\n"));
    let start = Instant::now();
    let env = shell.capture(Duration::from_secs(2)).unwrap();
    assert_eq!(env.get("EMPTY"), Some(OsStr::new("")));
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "{:?}",
        start.elapsed()
    );
    let pid: i32 = shell.record("shell").trim().parse().unwrap();
    assert!(wait_gone(pid), "the shell is still running");
}

#[test]
fn login_env_caches_once() {
    let cell = OnceLock::new();
    let runs = AtomicUsize::new(0);
    let results: Vec<_> = thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    cached(&cell, || {
                        runs.fetch_add(1, Ordering::SeqCst);
                        Ok(LoginEnv { vars: Box::new([]) })
                    })
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let first = results[0].as_ref().unwrap();
    for result in &results {
        assert!(Arc::ptr_eq(first, result.as_ref().unwrap()));
    }
}

#[test]
fn login_env_caches_a_failure_once() {
    let cell = OnceLock::new();
    let runs = AtomicUsize::new(0);
    for _ in 0..3 {
        let result = cached(&cell, || {
            runs.fetch_add(1, Ordering::SeqCst);
            Err(Failure::of(LoginEnvError::Timeout))
        });
        assert_eq!(result.unwrap_err(), LoginEnvError::Timeout);
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn login_env_debug_prints_no_names_or_values() {
    let env = LoginEnv {
        vars: Box::new([(
            OsString::from("SECRET_NAME"),
            OsString::from("secret_value"),
        )]),
    };
    assert_eq!(format!("{env:?}"), "LoginEnv { vars: 1 }");
}

#[test]
fn login_env_error_kinds_are_distinct_static_names() {
    let names = [
        LoginEnvError::NoShell,
        LoginEnvError::Spawn,
        LoginEnvError::Timeout,
        LoginEnvError::Unparsable,
    ]
    .map(LoginEnvError::as_str);
    assert_eq!(names, ["no_shell", "spawn", "timeout", "unparsable"]);
}

/// Executable file `name` in a directory of `dir`.
fn executable(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn pick_shell_prefers_a_usable_shell_var() {
    let dir = TempDir::new().unwrap();
    let a = executable(dir.path(), "a");
    let b = executable(dir.path(), "b");
    let c = executable(dir.path(), "c");
    assert_eq!(
        pick_shell(Some(a.as_os_str()), Some(b.as_os_str()), &c),
        Some(a)
    );
}

#[test]
fn pick_shell_falls_through_unusable_candidates() {
    let dir = TempDir::new().unwrap();
    let plain = dir.path().join("plain");
    std::fs::write(&plain, "").unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let b = executable(dir.path(), "b");
    let c = executable(dir.path(), "c");
    let absent = dir.path().join("absent");
    let bad: [&OsStr; 5] = [
        OsStr::new(""),
        OsStr::new("relative/zsh"),
        absent.as_os_str(),
        dir.path().as_os_str(),
        plain.as_os_str(),
    ];
    for shell_var in bad {
        assert_eq!(
            pick_shell(Some(shell_var), Some(b.as_os_str()), &c).as_ref(),
            Some(&b),
            "{shell_var:?}"
        );
        assert_eq!(
            pick_shell(Some(shell_var), Some(shell_var), &c).as_ref(),
            Some(&c),
            "{shell_var:?}"
        );
        assert_eq!(
            pick_shell(Some(shell_var), None, Path::new(shell_var)),
            None,
            "{shell_var:?}"
        );
    }
    assert_eq!(pick_shell(None, None, &c).as_ref(), Some(&c));
}

#[test]
fn find_program_returns_the_first_usable_entry() {
    let dir = TempDir::new().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let a = executable(&first, "tool");
    executable(&second, "tool");
    let path = std::env::join_paths([&first, &second]).unwrap();
    assert_eq!(find_program("tool", &path), Some(a));
    assert_eq!(find_program("absent", &path), None);
}

#[test]
fn find_program_skips_what_it_cannot_run() {
    let dir = TempDir::new().unwrap();
    let not_exec = dir.path().join("not_exec");
    let is_dir = dir.path().join("is_dir");
    let good = dir.path().join("good");
    for d in [&not_exec, &is_dir, &good] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(not_exec.join("tool"), "").unwrap();
    std::fs::set_permissions(
        not_exec.join("tool"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    std::fs::create_dir(is_dir.join("tool")).unwrap();
    let want = executable(&good, "tool");
    // An empty entry and a relative one are skipped, even when the relative
    // one would resolve.
    let path = format!(
        "::relative:{}:{}:{}",
        not_exec.display(),
        is_dir.display(),
        good.display()
    );
    assert_eq!(find_program("tool", OsStr::new(&path)), Some(want));
}

#[test]
fn find_program_follows_a_link_to_an_executable() {
    let dir = TempDir::new().unwrap();
    let real = dir.path().join("real");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let target = executable(&real, "target");
    symlink(&target, bin.join("tool")).unwrap();
    symlink(real.join("absent"), bin.join("dangling")).unwrap();
    assert_eq!(
        find_program("tool", bin.as_os_str()),
        Some(bin.join("tool"))
    );
    assert_eq!(find_program("dangling", bin.as_os_str()), None);
}

#[test]
fn find_program_rejects_names_that_are_not_one_component() {
    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    executable(&sub, "tool");
    for name in ["", ".", "..", "sub/tool", "/bin/sh"] {
        assert_eq!(find_program(name, dir.path().as_os_str()), None, "{name}");
    }
}

/// Runs the user's real shell: by hand only, never on CI. Prints counts, not
/// names or values.
#[test]
#[ignore = "runs the user's real login shell"]
fn login_env_reads_this_users_shell() {
    let start = Instant::now();
    let env = login_env().unwrap();
    println!("vars={} took={:?}", env.vars.len(), start.elapsed());
    assert!(env.get("PATH").is_some());
}

#[cfg(target_os = "macos")]
mod logs {
    use lectern_logging::testing;

    use super::*;

    const CANARY: &str = "canary7q3x9m2v";

    #[test]
    fn login_env_failure_logs_the_kind_only() {
        let log = testing::capture();
        log.clear();
        let body = format!("echo '{CANARY}_NAME={CANARY}_value'\necho '{CANARY}_stderr' >&2\n");
        let shell = FakeShell::named(&format!("{CANARY}_shell"), body.as_bytes());
        let cell = OnceLock::new();

        let result = cached(&cell, || shell.capture(LONG));

        assert_eq!(result.unwrap_err(), LoginEnvError::Unparsable);
        let lines = log.lines();
        assert_eq!(lines.len(), 1, "{lines:#?}");
        let line = &lines[0];
        assert!(line.contains("\"lv\":\"warn\""), "{line}");
        assert!(line.contains("\"tg\":\"platform\""), "{line}");
        assert!(line.contains("\"kind\":\"unparsable\""), "{line}");
        log.assert_absent(CANARY);
    }

    #[test]
    fn login_env_spawn_failure_logs_the_io_error_without_the_path() {
        let log = testing::capture();
        log.clear();
        let dir = TempDir::new().unwrap();
        let shell = dir.path().join(CANARY);
        let cell = OnceLock::new();

        let result = cached(&cell, || capture(&shell, dir.path(), LONG));

        assert_eq!(result.unwrap_err(), LoginEnvError::Spawn);
        let lines = log.lines();
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert!(lines[0].contains("\"kind\":\"spawn\""), "{}", lines[0]);
        assert!(lines[0].contains("\"io\":"), "{}", lines[0]);
        log.assert_absent(CANARY);
    }

    #[test]
    fn login_env_success_logs_counts_only() {
        let log = testing::capture();
        log.clear();
        let vars = format!("{CANARY}_NAME={CANARY}_value\0");
        let shell = FakeShell::new(&replay(vars.as_bytes()));
        let cell = OnceLock::new();

        let env = cached(&cell, || shell.capture(LONG)).unwrap();

        assert_eq!(env.vars.len(), 1);
        let lines = log.lines();
        assert_eq!(lines.len(), 1, "{lines:#?}");
        let line = &lines[0];
        assert!(line.contains("\"lv\":\"debug\""), "{line}");
        assert!(line.contains("\"vars\":1"), "{line}");
        assert!(line.contains("\"took"), "{line}");
        log.assert_absent(CANARY);
    }
}
