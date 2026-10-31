//! The user's login shell environment (「登录 shell 的环境」 in
//! docs/design/platform.md): an app started from Finder or the Dock has only
//! launchd's minimal environment, so the Terminal page's agents and the ACP
//! client (`agents`) start with the environment the user's login shell sets
//! up instead (ADR 0050). Taken once per process and shared.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

/// How long the first [`login_env`] waits for the shell.
const TIMEOUT: Duration = Duration::from_secs(5);
/// The most the shell may print before it counts as unparsable.
const MAX_OUTPUT: usize = 1 << 20;
/// One read from the shell's stdout.
const CHUNK: usize = 64 << 10;
/// Variables that describe the shell that printed the environment, not the
/// user's environment.
const DROPPED: [&[u8]; 4] = [b"_", b"SHLVL", b"PWD", b"OLDPWD"];
/// The last resort when neither `$SHELL` nor the user database names one.
const FALLBACK_SHELL: &str = "/bin/zsh";

/// The user's login shell: `$SHELL` (an absolute path to an executable
/// regular file); else the login shell in the user database
/// (`getpwuid_r`), checked the same way; else `/bin/zsh`, checked the same
/// way; None when none is usable.
///
/// Reads an environment variable, the user database and stat: blocks
/// briefly; do not call it on the UI thread.
pub fn user_shell() -> Option<PathBuf> {
    pick_shell(
        std::env::var_os("SHELL").as_deref(),
        passwd_shell().as_deref(),
        Path::new(FALLBACK_SHELL),
    )
}

/// The environment variables after the user's login shell has set them up.
///
/// The first call runs `<user_shell> -l -i -c <print the environment>` once
/// and waits for it at most 5 seconds. The result, success or failure, is
/// cached in the process: later calls return it at once, and concurrent
/// first calls run the shell once (the others wait for it).
///
/// Blocks: call it only from an executor task or a dedicated thread, never
/// before the first frame (ADR 0017).
pub fn login_env() -> Result<Arc<LoginEnv>, LoginEnvError> {
    static CELL: OnceLock<Result<Arc<LoginEnv>, LoginEnvError>> = OnceLock::new();
    cached(&CELL, || {
        let shell = user_shell().ok_or(Failure::of(LoginEnvError::NoShell))?;
        capture(&shell, &shell_cwd(), TIMEOUT)
    })
}

/// The login shell's environment, as [`login_env`] returns it.
#[derive(Debug)]
pub struct LoginEnv {
    /// In the order the shell printed them, without `_`, `SHLVL`, `PWD` and
    /// `OLDPWD`.
    pub vars: Box<[(OsString, OsString)]>,
}

impl LoginEnv {
    /// The value of the first variable named `name`.
    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars
            .iter()
            .find(|(n, _)| n == OsStr::new(name))
            .map(|(_, v)| v.as_os_str())
    }
}

/// Looks in each directory of `path` (colon-separated) for an executable
/// regular file named `name` and returns the first one's path (the
/// directory joined with `name`, not canonicalized). Follows symbolic links
/// (Homebrew's programs are links).
///
/// None when `name` is empty, contains `/`, or is `.` or `..`; empty and
/// relative entries of `path` are skipped. Blocks (stat).
pub fn find_program(name: &str, path: &OsStr) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return None;
    }
    path.as_bytes()
        .split(|&b| b == b':')
        .filter(|dir| dir.first() == Some(&b'/'))
        .map(|dir| Path::new(OsStr::from_bytes(dir)).join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// Why [`login_env`] has no environment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoginEnvError {
    /// [`user_shell`] is None.
    NoShell,
    /// Creating the pipe or starting the shell failed.
    Spawn,
    /// No complete environment within 5 seconds.
    Timeout,
    /// The shell's output has no complete pair of markers, what lies between
    /// them is not `env -0`'s format, or it is over 1 MiB.
    Unparsable,
}

impl LoginEnvError {
    /// The kind's name, for the `kind` log field.
    pub fn as_str(self) -> &'static str {
        match self {
            LoginEnvError::NoShell => "no_shell",
            LoginEnvError::Spawn => "spawn",
            LoginEnvError::Timeout => "timeout",
            LoginEnvError::Unparsable => "unparsable",
        }
    }
}

/// A failed capture: the kind callers see, and the system error behind a
/// `Spawn`, which only the log record carries.
#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) kind: LoginEnvError,
    pub(crate) io: Option<io::Error>,
}

impl Failure {
    fn of(kind: LoginEnvError) -> Failure {
        Failure { kind, io: None }
    }
}

/// [`user_shell`]'s choice among the three candidates the caller gives.
pub(crate) fn pick_shell(
    shell_var: Option<&OsStr>,
    passwd_shell: Option<&OsStr>,
    fallback: &Path,
) -> Option<PathBuf> {
    [shell_var.map(Path::new), passwd_shell.map(Path::new), Some(fallback)]
        .into_iter()
        .flatten()
        .find(|p| is_executable_file(p))
        .map(Path::to_path_buf)
}

/// [`login_env`]'s cache: runs `f` at most once per `cell`, logs its result
/// (on macOS) and hands every caller the same outcome.
pub(crate) fn cached(
    cell: &OnceLock<Result<Arc<LoginEnv>, LoginEnvError>>,
    f: impl FnOnce() -> Result<LoginEnv, Failure>,
) -> Result<Arc<LoginEnv>, LoginEnvError> {
    cell.get_or_init(|| {
        let start = Instant::now();
        let result = f();
        log_result(&result, start.elapsed());
        result.map(Arc::new).map_err(|f| f.kind)
    })
    .clone()
}

#[cfg(target_os = "macos")]
fn log_result(result: &Result<LoginEnv, Failure>, took: Duration) {
    use lectern_logging::{Value, debug, warn};
    match result {
        Ok(env) => debug!(
            "the login shell environment was captured";
            vars = env.vars.len() as u64,
            took = took,
        ),
        Err(Failure { kind, io: Some(io) }) => warn!(
            "the login shell environment was not captured; agents start with the app environment";
            kind = kind.as_str(),
            io = Value::io(io),
        ),
        Err(Failure { kind, io: None }) => warn!(
            "the login shell environment was not captured; agents start with the app environment";
            kind = kind.as_str(),
        ),
    }
}

/// Lectern runs as a GUI app on macOS only; elsewhere this module is
/// compiled for `terminal`'s and `agents`' CI and logs nothing.
#[cfg(not(target_os = "macos"))]
fn log_result(_: &Result<LoginEnv, Failure>, _: Duration) {}

/// Runs `shell -l -i -c <print the environment>` in `cwd` once, without
/// caching, and parses what it prints. Waits until `timeout` has passed
/// since the start at most.
pub(crate) fn capture(shell: &Path, cwd: &Path, timeout: Duration) -> Result<LoginEnv, Failure> {
    let deadline = Instant::now() + timeout;
    let begin = format!("__lectern_env_{}__", random_token());
    let end = format!("__lectern_env_{}__", random_token());
    let script = format!("printf '%s\\n' {begin}; /usr/bin/env -0; printf '%s\\n' {end}");

    let mut command = Command::new(shell);
    command
        .args(["-l", "-i", "-c", &script])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: the closure runs in the forked child before exec and calls
    // only `setsid`, which is async-signal-safe; it allocates nothing.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| Failure {
        kind: LoginEnvError::Spawn,
        io: Some(e),
    })?;
    let Some(mut stdout) = child.stdout.take() else {
        kill_group(&mut child);
        return Err(Failure::of(LoginEnvError::Spawn));
    };

    let mut out = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    let outcome = loop {
        if let Some(block) = find_block(&out, begin.as_bytes(), end.as_bytes()) {
            break Ok(block);
        }
        let now = Instant::now();
        if now >= deadline {
            break Err(LoginEnvError::Timeout);
        }
        if !readable(stdout.as_raw_fd(), deadline - now) {
            continue;
        }
        match stdout.read(&mut buf) {
            Ok(0) => break Err(LoginEnvError::Unparsable),
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > MAX_OUTPUT {
                    break Err(LoginEnvError::Unparsable);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break Err(LoginEnvError::Unparsable),
        }
    };
    // The shell may still print (its logout files); with the pipe closed it
    // does not block on a full pipe until the deadline.
    drop(stdout);

    match outcome {
        Ok(block) => {
            // A login shell runs its logout files on exit: let it finish,
            // up to the same deadline.
            if !wait_until(&mut child, deadline) {
                kill_group(&mut child);
            }
            let vars = parse(&out[block]).ok_or(Failure::of(LoginEnvError::Unparsable))?;
            Ok(LoginEnv { vars })
        }
        Err(kind) => {
            kill_group(&mut child);
            Err(Failure::of(kind))
        }
    }
}

/// The byte range between `begin\n` and the following `end\n`.
fn find_block(out: &[u8], begin: &[u8], end: &[u8]) -> Option<std::ops::Range<usize>> {
    let start = find_line(out, begin, 0)? + begin.len() + 1;
    let stop = find_line(out, end, start)?;
    Some(start..stop)
}

/// Where `marker` followed by a newline first occurs at or after `from`.
fn find_line(out: &[u8], marker: &[u8], from: usize) -> Option<usize> {
    let len = marker.len() + 1;
    out.get(from..)?
        .windows(len)
        .position(|w| &w[..marker.len()] == marker && w[marker.len()] == b'\n')
        .map(|i| from + i)
}

/// `env -0`'s output: `name=value` entries, each ending in NUL. None when an
/// entry has no `=` or an empty name. Drops [`DROPPED`].
pub(crate) fn parse(block: &[u8]) -> Option<Box<[(OsString, OsString)]>> {
    let block = block.strip_suffix(b"\0").unwrap_or(block);
    if block.is_empty() {
        return Some(Box::new([]));
    }
    let mut vars = Vec::new();
    for entry in block.split(|&b| b == 0) {
        let eq = entry.iter().position(|&b| b == b'=')?;
        if eq == 0 {
            return None;
        }
        let (name, value) = (&entry[..eq], &entry[eq + 1..]);
        if DROPPED.contains(&name) {
            continue;
        }
        vars.push((
            OsString::from_vec(name.to_vec()),
            OsString::from_vec(value.to_vec()),
        ));
    }
    Some(vars.into_boxed_slice())
}

/// Waits up to `left` for `fd` to become readable (or reach EOF). False on
/// timeout or interruption; the caller checks its deadline again.
fn readable(fd: i32, left: Duration) -> bool {
    let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `pfd` is one valid `pollfd` for the duration of the call.
    let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
    rc > 0
}

/// Polls `child` until it exits or `deadline` passes. True when it exited
/// (and is reaped).
fn wait_until(child: &mut Child, deadline: Instant) -> bool {
    let mut pause = Duration::from_millis(1);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return false,
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(pause.min(deadline - now));
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

/// Kills the shell's process group (its id is the shell's pid: `setsid`)
/// and reaps the shell.
fn kill_group(child: &mut Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: a plain system call on a process group this function's
        // caller created; an error (the group is gone) is harmless.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.wait();
}

/// 32 hex digits from `/dev/urandom`, or from the pid and the clock when it
/// cannot be read: only so that rc-file output cannot match a marker by
/// chance.
fn random_token() -> String {
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if read.is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mixed = nanos ^ (u128::from(std::process::id()) << 64);
        bytes = mixed.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The shell starts in `HOME` when it is an absolute directory, as in a
/// terminal; else in `/`.
fn shell_cwd() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute() && h.is_dir())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// An absolute path to a regular file (following links) the user may
/// execute.
fn is_executable_file(path: &Path) -> bool {
    if !path.is_absolute() || !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a NUL-terminated path that outlives the call.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// The login shell in the user database, for the current user.
fn passwd_shell() -> Option<OsString> {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let mut size = 1024;
    loop {
        let mut buf = vec![0 as libc::c_char; size];
        let mut pwd = MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call and `buf.len()` is
        // the buffer's real size; `result` is set to `pwd` or null.
        let rc = unsafe {
            libc::getpwuid_r(uid, pwd.as_mut_ptr(), buf.as_mut_ptr(), buf.len(), &mut result)
        };
        if rc == libc::ERANGE && size < 64 << 10 {
            size *= 2;
            continue;
        }
        if rc != 0 || result.is_null() {
            return None;
        }
        // SAFETY: `getpwuid_r` returned 0 with a non-null result, so it
        // filled `pwd`; its strings point into `buf`, still alive here.
        let shell = unsafe { pwd.assume_init_ref() }.pw_shell;
        if shell.is_null() {
            return None;
        }
        // SAFETY: `pw_shell` is a NUL-terminated string inside `buf`.
        let bytes = unsafe { CStr::from_ptr(shell) }.to_bytes();
        return Some(OsStr::from_bytes(bytes).to_owned());
    }
}
