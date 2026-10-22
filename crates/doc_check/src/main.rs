//! `cargo run -p lectern-doc-check`: check the relative links in every tracked
//! Markdown file. Exits with status 1 when any link is broken.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a tool crate: the check reports its result to the terminal and CI (docs/design/logging.md「对其他 crate 的要求」)"
)]

use std::path::PathBuf;
use std::process::{Command, ExitCode};

/// Git's stdout as raw bytes: tracked paths need not be UTF-8.
fn git(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(out.stdout)
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

fn main() -> ExitCode {
    let result = git(&["rev-parse", "--show-toplevel"]).and_then(|root| {
        // Drop only git's newline: a directory name may end in spaces.
        let root = path_from_bytes(root.strip_suffix(b"\n").unwrap_or(&root));
        // List every tracked file and filter by extension here: pathspecs are
        // case-sensitive, so `*.md` would miss `GUIDE.MD`.
        let list = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["ls-files", "-z"])
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if !list.status.success() {
            return Err(String::from_utf8_lossy(&list.stderr).into_owned());
        }
        let tracked: Vec<PathBuf> = list
            .stdout
            .split(|&b| b == 0)
            .filter(|f| !f.is_empty())
            .map(path_from_bytes)
            .collect();
        let files: Vec<PathBuf> = tracked
            .iter()
            .filter(|p| lectern_doc_check::is_markdown(p))
            .cloned()
            .collect();
        Ok(lectern_doc_check::check_tracked(
            &root,
            &files,
            Some(&tracked),
        ))
    });
    match result {
        Ok(errors) if errors.is_empty() => {
            println!("Doc link check passed.");
            ExitCode::SUCCESS
        }
        Ok(errors) => {
            println!("Doc link check failed:");
            for e in errors {
                println!("  - {e}");
            }
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("Doc link check could not run: {e}");
            ExitCode::FAILURE
        }
    }
}
