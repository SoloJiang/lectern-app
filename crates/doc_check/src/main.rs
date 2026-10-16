//! `cargo run -p lectern-doc-check`: check the relative links in every tracked
//! Markdown file. Exits with status 1 when any link is broken.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn git(args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let result = git(&["rev-parse", "--show-toplevel"]).and_then(|root| {
        let root = PathBuf::from(root.trim());
        // List every tracked file and filter by extension here: pathspecs are
        // case-sensitive, so `*.md` would miss `GUIDE.MD`.
        let list = git(&["-C", &root.to_string_lossy(), "ls-files", "-z"])?;
        let files: Vec<PathBuf> = list
            .split('\0')
            .filter(|f| !f.is_empty())
            .map(PathBuf::from)
            .filter(|p| lectern_doc_check::is_markdown(p))
            .collect();
        Ok(lectern_doc_check::check(&root, &files))
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
