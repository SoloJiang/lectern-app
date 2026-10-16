//! End-to-end tests of the binary with a fake `git` first on PATH (AGENTS.md:
//! code that runs external commands is tested against a recording fake).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    /// A repo with `files` and a fake `git` that logs its argv and cwd, then
    /// answers `rev-parse --show-toplevel` and `ls-files` (or fails if `fail`).
    fn new(name: &str, files: &[(&str, &str)], fail: bool) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lectern-doc-check-cli-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        for (path, text) in files {
            let p = repo.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let list: String = files.iter().map(|(p, _)| format!("{p}\\0")).collect();
        let script = if fail {
            "#!/bin/sh\necho \"$PWD|$*\" >> \"$FAKE_GIT_LOG\"\necho 'fatal: not a git repository' >&2\nexit 128\n".to_string()
        } else {
            format!(
                "#!/bin/sh\necho \"$PWD|$*\" >> \"$FAKE_GIT_LOG\"\ncase \"$*\" in\n  'rev-parse --show-toplevel') echo '{}' ;;\n  *ls-files*) printf '{list}' ;;\n  *) exit 1 ;;\nesac\n",
                repo.display()
            )
        };
        let git = bin.join("git");
        fs::write(&git, script).unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
        Fixture { dir }
    }

    fn run(&self) -> (bool, String, Vec<String>) {
        let log = self.dir.join("git.log");
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new(env!("CARGO_BIN_EXE_lectern-doc-check"))
            .current_dir(self.dir.join("repo"))
            .env("PATH", path)
            .env("FAKE_GIT_LOG", &log)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        let calls = fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        (out.status.success(), text, calls)
    }

    fn repo(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn lists_tracked_markdown_through_git_and_passes() {
    let f = Fixture::new("ok", &[("a.md", "# A\n"), ("b.md", "[a](a.md#a)\n")], false);
    let (ok, out, calls) = f.run();
    assert!(ok, "{out}");
    assert!(out.contains("Doc link check passed."));
    let repo = f.repo().join("repo");
    let repo = repo.display();
    assert_eq!(
        calls,
        [
            format!("{repo}|rev-parse --show-toplevel"),
            format!("{repo}|-C {repo} ls-files -z -- *.md")
        ]
    );
}

#[test]
fn reports_broken_links_with_failure_status() {
    let f = Fixture::new("broken", &[("b.md", "[x](missing.md)\n")], false);
    let (ok, out, _) = f.run();
    assert!(!ok);
    assert!(out.contains("b.md:1: missing.md: file not found"), "{out}");
}

#[test]
fn git_failure_is_reported() {
    let f = Fixture::new("fail", &[("a.md", "# A\n")], true);
    let (ok, out, calls) = f.run();
    assert!(!ok);
    assert!(out.contains("Doc link check could not run"), "{out}");
    assert_eq!(calls.len(), 1);
}
