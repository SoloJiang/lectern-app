//! `purge`: `sudo -n purge` runs a fake `sudo` put first in `PATH`, which
//! records its arguments and replays success or failure (AGENTS.md「工作方式」,
//! docs/design/bench.md「测试」). Only the command's `PATH` changes, not
//! this process's environment.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use lectern_bench::process::purge;

/// A `sudo` in `dir` that writes its arguments to `dir/args`, one per
/// line, and exits with `code`. Returns the `PATH` with `dir` first.
fn fake_sudo(dir: &Path, code: i32) -> OsString {
    let script = dir.join("sudo");
    fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\nexit {code}\n"),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = dir.as_os_str().to_owned();
    if let Some(rest) = std::env::var_os("PATH") {
        path.push(":");
        path.push(rest);
    }
    path
}

#[test]
fn purge_runs_sudo_without_a_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_sudo(dir.path(), 0);
    assert!(purge(Some(&path)).unwrap());
    let args = fs::read_to_string(dir.path().join("args")).unwrap();
    assert_eq!(args, "-n\npurge\n");
}

#[test]
fn purge_that_fails_is_skipped() {
    // `sudo -n` exits 1 when it would ask for a password.
    let dir = tempfile::tempdir().unwrap();
    let path = fake_sudo(dir.path(), 1);
    assert!(!purge(Some(&path)).unwrap());
    assert!(dir.path().join("args").exists());
}

#[test]
fn purge_without_sudo_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!purge(Some(dir.path().as_os_str())).unwrap());
}
