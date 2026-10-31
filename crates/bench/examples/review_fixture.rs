//! Writes the fixture of `app/open_diff` for a manual check of the project
//! home and the review screen (docs/design/review_view.md「验收」第 3 步,
//! docs/design/project_home.md「验收」第 3 步):
//!
//! ```sh
//! cargo run -p lectern-bench --example review_fixture -- <home>
//! ```
//!
//! `<home>` must not exist yet; it becomes a temporary `HOME` holding only
//! the Claude Code session log. The repository is written under the system's
//! temporary directory and kept (delete it by hand). Prints the repository
//! root, the `HOME` and the session key. Synthetic data only: nothing is
//! read from the user's own directories.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a tool crate: the example prints the fixture's paths for the person who runs it (docs/design/logging.md「对其他 crate 的要求」)"
)]

#[allow(dead_code, reason = "the example uses only `module`")]
#[path = "../benches/fixtures/git_worktree.rs"]
mod git_worktree;
#[path = "../benches/fixtures/review_session.rs"]
mod review_session;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;

fn main() -> ExitCode {
    let Some(home) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: review_fixture <home>");
        return ExitCode::from(2);
    };
    if home.exists() {
        eprintln!("{} exists already", home.display());
        return ExitCode::from(2);
    }
    let fixture = review_session::ReviewSession::new(200, 50, 20, SystemTime::now());
    if let Err(error) = fixture.write_home(&home) {
        eprintln!("cannot write {}: {error}", home.display());
        return ExitCode::FAILURE;
    }
    println!("root {}", fixture.root().display());
    println!("home {}", home.display());
    println!("session {}", fixture.session());
    // Keep the repository: the check runs after this process.
    std::mem::forget(fixture);
    ExitCode::SUCCESS
}
