//! The command line (docs/design/app.md「接口」). One option, so no parsing
//! library: it would add binary size and work before the first frame.

use std::ffi::OsString;

use lectern_logging::warn;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// `--log <filter>` or `--log=<filter>`; the last one wins. A value that
    /// is not UTF-8 counts as not given.
    pub log: Option<String>,
    /// Debug builds only: quit once startup is done (`tests/startup.rs`).
    #[cfg(debug_assertions)]
    pub quit_after_start: Option<QuitPath>,
    /// Indexes (from 1, without the program name) of the arguments that were
    /// not recognised. The arguments themselves are not kept: they may hold
    /// paths or tokens.
    pub unknown: Vec<usize>,
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitPath {
    /// Through the `Quit` action: `--quit-after-start`.
    Action,
    /// `cx.quit()` directly, the path a system quit takes:
    /// `--quit-after-start=system`.
    System,
}

/// Memory only; `main` calls it before `install_early`. `args` starts with
/// the program name.
pub fn parse(args: impl Iterator<Item = OsString>) -> Args {
    let mut parsed = Args::default();
    let mut args = args.skip(1).enumerate().map(|(i, arg)| (i + 1, arg));
    while let Some((index, arg)) = args.next() {
        let Some(text) = arg.to_str() else {
            parsed.unknown.push(index);
            continue;
        };
        if text == "--log" {
            match args.next() {
                Some((_, value)) => match value.into_string() {
                    Ok(value) => parsed.log = Some(value),
                    Err(_) => parsed.unknown.push(index),
                },
                None => parsed.unknown.push(index),
            }
        } else if let Some(value) = text.strip_prefix("--log=") {
            parsed.log = Some(value.to_owned());
        } else if !debug_flag(&mut parsed, text) {
            parsed.unknown.push(index);
        }
    }
    parsed
}

#[cfg(debug_assertions)]
fn debug_flag(parsed: &mut Args, text: &str) -> bool {
    parsed.quit_after_start = match text {
        "--quit-after-start" => Some(QuitPath::Action),
        "--quit-after-start=system" => Some(QuitPath::System),
        _ => return false,
    };
    true
}

#[cfg(not(debug_assertions))]
fn debug_flag(_: &mut Args, _: &str) -> bool {
    false
}

/// One warn per unrecognised argument, with its index only. Before the
/// first frame the records stay in the early buffer.
pub fn log_unknown(args: &Args) {
    for &index in &args.unknown {
        warn!("unrecognized argument"; index = index);
    }
}
