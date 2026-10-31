//! The fake program of the terminal's tests and benchmarks
//! (docs/design/terminal.md「测试」): it stands in for `claude`, `codex` and
//! a login shell, records how it was started and plays a scripted
//! behaviour. Synthetic only; it reads no user data and no network.
//!
//! The environment says what to do (the tests give it through
//! `with_login_env`):
//!
//! - `FAKE_TERMINAL_DIR=<dir>`: write argv, working directory and the names
//!   of the environment variables (values only for a few) to `<dir>/<pid>.txt`;
//!   the plan is the content of `<dir>/plan` when that file exists.
//! - `FAKE_TERMINAL_PLAN=<steps>`: the plan otherwise. Comma-separated steps,
//!   in order:
//!   `raw` (raw mode on the terminal), `say:<text>` (write the text),
//!   `nl` (write CR LF), `out:<file>` (write the file), `echo` (copy input to
//!   output until it ends), `input:<file>` (append the input to the file,
//!   write nothing, until it ends), `sleep:<ms>`, `exit:<code>`,
//!   `signal:<number>` (send it to itself), `ignore-hup`, `size` (write
//!   `SIZE:<columns>x<rows>`), `winch` (write the size again after every
//!   `SIGWINCH`, forever), `hold` (wait forever). The default plan is
//!   `say:READY,nl,hold`.

use std::io::{Read, Write};
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::env::var_os("FAKE_TERMINAL_DIR").map(std::path::PathBuf::from);
    if let Some(dir) = &dir {
        record(&dir.join(format!("{}.txt", std::process::id())), &args);
    }
    let plan = dir
        .and_then(|dir| std::fs::read_to_string(dir.join("plan")).ok())
        .or_else(|| std::env::var("FAKE_TERMINAL_PLAN").ok())
        .unwrap_or_else(|| "say:READY,nl,hold".into());
    for step in plan.trim().split(',') {
        run(step);
    }
}

fn record(path: &std::path::Path, args: &[String]) {
    let mut out = String::new();
    out.push_str(&format!(
        "argv0={}\n",
        args.first().map_or("", String::as_str)
    ));
    for arg in args.iter().skip(1) {
        out.push_str(&format!("arg={arg}\n"));
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
    for name in [
        "TERM",
        "COLORTERM",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "PWD",
        "LC_CTYPE",
        "SHELL",
    ] {
        if let Ok(value) = std::env::var(name) {
            out.push_str(&format!("var:{name}={value}\n"));
        }
    }
    // SAFETY: `isatty` only reads the descriptor's type.
    let tty = unsafe { libc::isatty(0) } == 1;
    out.push_str(&format!("tty={tty}\n"));
    let _ = std::fs::write(path, out);
}

fn run(step: &str) {
    let (name, value) = step.split_once(':').unwrap_or((step, ""));
    match name {
        "raw" => raw(),
        "say" => write_out(value.as_bytes()),
        "nl" => write_out(b"\r\n"),
        "out" => {
            if let Ok(bytes) = std::fs::read(value) {
                write_out(&bytes);
            }
        }
        "echo" => copy_input(None),
        "input" => copy_input(Some(value)),
        "sleep" => std::thread::sleep(Duration::from_millis(value.parse().unwrap_or(0))),
        "exit" => std::process::exit(value.parse().unwrap_or(0)),
        "signal" => {
            // SAFETY: `kill` to the own process with a number the test gives.
            unsafe {
                libc::kill(libc::getpid(), value.parse().unwrap_or(libc::SIGTERM));
            }
            std::thread::sleep(Duration::from_secs(5));
        }
        "ignore-hup" => {
            // SAFETY: `signal` with `SIG_IGN` installs no handler code.
            unsafe {
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
            }
        }
        "size" => report_size(),
        "winch" => watch_size(),
        "hold" => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        _ => {}
    }
}

static WINCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn report_size() {
    // SAFETY: `winsize` is plain data that the ioctl fills in.
    let size = unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        libc::ioctl(0, libc::TIOCGWINSZ, &mut size);
        size
    };
    write_out(format!("SIZE:{}x{}\r\n", size.ws_col, size.ws_row).as_bytes());
}

fn watch_size() {
    // SAFETY: the handler only stores into an atomic.
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
    }
    write_out(b"WATCHING\r\n");
    loop {
        if WINCH.swap(false, std::sync::atomic::Ordering::SeqCst) {
            report_size();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn write_out(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

/// Raw mode: no echo, no line editing, bytes at once.
fn raw() {
    // SAFETY: `termios` is plain data that `tcgetattr` fills in; the calls
    // only touch the terminal on descriptor 0.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut termios) == 0 {
            libc::cfmakeraw(&mut termios);
            libc::tcsetattr(0, libc::TCSANOW, &termios);
        }
    }
}

/// Copies the input to the output, or appends it to `file`, until it ends.
fn copy_input(file: Option<&str>) {
    let mut input = std::io::stdin().lock();
    let mut sink = file.and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    let mut buf = [0u8; 4096];
    loop {
        match input.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => match &mut sink {
                Some(file) => {
                    let _ = file.write_all(&buf[..n]);
                }
                None => write_out(&buf[..n]),
            },
        }
    }
}
