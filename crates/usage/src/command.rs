//! What a shell command did to the repository (docs/design/usage.md「命令文本」):
//! the directories it went into and the git subcommands that matter to
//! attribution. The text is read in memory and never stored or logged; only
//! the facts leave this module.
//!
//! The reading is a small POSIX shell lexer: words with single and double
//! quotes and backslash escapes, commands separated by `;`, `&&`, `||`, `|`,
//! `&`, a newline or a parenthesis. Nothing is expanded: a word with a
//! variable or a command substitution makes the command it belongs to
//! unsure, and an unsure `cd` is not followed. Only the common ways of
//! writing git commands are read; the rest (aliases, scripts, other tools
//! that switch branches) is a known limit.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A git subcommand that decides where work goes (docs/design/usage.md
/// 「命令文本」), with the names and OIDs it names but not the text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitFact {
    /// `git stash`, `git stash push`, `git stash save`.
    StashCreate,
    /// `git stash apply` / `pop`: the stash named (`stash@{n}`, an OID), or
    /// the newest.
    StashRestore { stash: Option<String> },
    /// `git stash drop`.
    StashDrop,
    /// `git push <remote> <source>:<branch>`; the remote is only a name that
    /// is configured as one, never an address (an address can carry a token).
    Push {
        remote: Option<String>,
        source: String,
        branch: String,
    },
    /// `git cherry-pick <commit>...`.
    CherryPick { commits: Vec<String> },
    /// `git rebase`, `git commit --amend`.
    Rewrite,
    /// `git reset`, `git branch -f`, `git checkout -B`, `git switch -C`,
    /// `git worktree add -B`: the branch may leave commits behind.
    Reset,
    /// `git switch -c`, `git checkout -b`, `git worktree add -b`.
    NewBranch,
}

/// What a command line did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// The directories it went into (`cd`, `pushd`, `git -C`), absolute and
    /// lexically normalized, in order, each once.
    pub entered: Vec<PathBuf>,
    pub facts: Vec<GitFact>,
    /// It runs git (any subcommand).
    pub git: bool,
    /// Some command could not be read (a variable or substitution in a
    /// directory or in a subcommand's operands).
    pub unsure: bool,
}

#[derive(Debug, Clone)]
struct Word {
    text: String,
    /// Holds `$` or a backtick outside single quotes.
    dynamic: bool,
}

/// Reads `command`, run in `workdir`.
pub fn parse(command: &str, workdir: &Path) -> Parsed {
    let mut out = Parsed::default();
    let mut cwd = workdir.to_path_buf();
    let mut stack: Vec<PathBuf> = Vec::new();
    for words in commands(command) {
        read_command(&words, &mut cwd, &mut stack, &mut out);
    }
    out
}

/// The commands of a line, each a list of words.
fn commands(text: &str) -> Vec<Vec<Word>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Vec<Word>> = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    let mut current: Option<Word> = None;
    let mut i = 0;
    // A `<<` waiting for its delimiter word.
    let mut heredoc_next = false;
    let mut heredocs: Vec<String> = Vec::new();
    let flush = |current: &mut Option<Word>,
                 words: &mut Vec<Word>,
                 heredoc_next: &mut bool,
                 heredocs: &mut Vec<String>| {
        if let Some(w) = current.take() {
            if *heredoc_next {
                heredocs.push(w.text.clone());
                *heredoc_next = false;
            }
            words.push(w);
        }
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' => {
                let w = current.get_or_insert(Word {
                    text: String::new(),
                    dynamic: false,
                });
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    w.text.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            '"' => {
                let w = current.get_or_insert(Word {
                    text: String::new(),
                    dynamic: false,
                });
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    match chars[i] {
                        '\\' if i + 1 < chars.len() && "$`\"\\\n".contains(chars[i + 1]) => {
                            if chars[i + 1] != '\n' {
                                w.text.push(chars[i + 1]);
                            }
                            i += 2;
                        }
                        '$' | '`' => {
                            w.dynamic = true;
                            w.text.push(chars[i]);
                            i += 1;
                        }
                        other => {
                            w.text.push(other);
                            i += 1;
                        }
                    }
                }
                i += 1;
            }
            '\\' => {
                let w = current.get_or_insert(Word {
                    text: String::new(),
                    dynamic: false,
                });
                if i + 1 < chars.len() {
                    if chars[i + 1] != '\n' {
                        w.text.push(chars[i + 1]);
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '$' | '`' => {
                let w = current.get_or_insert(Word {
                    text: String::new(),
                    dynamic: false,
                });
                w.dynamic = true;
                w.text.push(c);
                i += 1;
            }
            '#' if current.is_none() => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ' ' | '\t' | '\r' => {
                flush(&mut current, &mut words, &mut heredoc_next, &mut heredocs);
                i += 1;
            }
            '\n' | ';' | '&' | '|' | '(' | ')' => {
                flush(&mut current, &mut words, &mut heredoc_next, &mut heredocs);
                if !words.is_empty() {
                    out.push(std::mem::take(&mut words));
                }
                i += 1;
                // `&&`, `||`, `|&`, `;;`: one separator.
                while i < chars.len() && matches!(chars[i], '&' | '|') && c != '\n' {
                    i += 1;
                }
                if c == '\n' && !heredocs.is_empty() {
                    // The bodies of here-documents run to their delimiter.
                    for delimiter in std::mem::take(&mut heredocs) {
                        while i < chars.len() {
                            let end = chars[i..]
                                .iter()
                                .position(|&c| c == '\n')
                                .map_or(chars.len(), |p| i + p);
                            let line: String = chars[i..end].iter().collect();
                            i = (end + 1).min(chars.len());
                            if line.trim() == delimiter {
                                break;
                            }
                        }
                    }
                }
            }
            '<' if chars.get(i + 1) == Some(&'<') && chars.get(i + 2) != Some(&'<') => {
                flush(&mut current, &mut words, &mut heredoc_next, &mut heredocs);
                i += 2;
                if chars.get(i) == Some(&'-') {
                    i += 1;
                }
                heredoc_next = true;
            }
            other => {
                let w = current.get_or_insert(Word {
                    text: String::new(),
                    dynamic: false,
                });
                w.text.push(other);
                i += 1;
            }
        }
    }
    flush(&mut current, &mut words, &mut heredoc_next, &mut heredocs);
    if !words.is_empty() {
        out.push(words);
    }
    out
}

fn is_assignment(w: &Word) -> bool {
    match w.text.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Resolves `dir` against `cwd`, without touching the file system.
fn join(cwd: &Path, dir: &str) -> PathBuf {
    let path = Path::new(dir);
    let mut out = if path.is_absolute() {
        PathBuf::new()
    } else {
        cwd.to_path_buf()
    };
    for part in path.components() {
        match part {
            Component::RootDir => out.push("/"),
            Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

fn enter(out: &mut Parsed, dir: PathBuf) {
    if !out.entered.contains(&dir) {
        out.entered.push(dir);
    }
}

fn read_command(words: &[Word], cwd: &mut PathBuf, stack: &mut Vec<PathBuf>, out: &mut Parsed) {
    let mut at = 0;
    // Assignments and wrappers in front of the command.
    loop {
        match words.get(at) {
            Some(w) if is_assignment(w) => at += 1,
            Some(w)
                if matches!(
                    w.text.as_str(),
                    "env" | "command" | "exec" | "time" | "nohup" | "builtin"
                ) =>
            {
                at += 1;
                // `env -i FOO=bar`: options of the wrapper.
                while words
                    .get(at)
                    .is_some_and(|w| w.text.starts_with('-') && w.text.len() > 1)
                {
                    at += 1;
                }
            }
            _ => break,
        }
    }
    let Some(head) = words.get(at) else {
        return;
    };
    let rest = &words[at + 1..];
    match head.text.as_str() {
        "cd" | "pushd" => {
            // The last word that is not an option.
            let target = rest
                .iter()
                .rev()
                .find(|w| !w.text.starts_with('-') || w.text == "-");
            match target {
                Some(w) if w.dynamic || w.text.starts_with('~') || w.text == "-" => {
                    out.unsure = true;
                }
                Some(w) => {
                    if head.text == "pushd" {
                        stack.push(cwd.clone());
                    }
                    *cwd = join(cwd, &w.text);
                    enter(out, cwd.clone());
                }
                // `cd` alone goes home.
                None if head.text == "cd" => out.unsure = true,
                None => {}
            }
        }
        "popd" => {
            if let Some(back) = stack.pop() {
                *cwd = back;
                enter(out, cwd.clone());
            }
        }
        "git" => read_git(rest, cwd, out),
        _ => {}
    }
}

fn read_git(words: &[Word], cwd: &Path, out: &mut Parsed) {
    out.git = true;
    let mut here = cwd.to_path_buf();
    let mut i = 0;
    // Options in front of the subcommand.
    while let Some(w) = words.get(i) {
        let t = w.text.as_str();
        match t {
            "-C" => {
                match words.get(i + 1) {
                    Some(d) if !d.dynamic && !d.text.starts_with('~') => {
                        here = join(&here, &d.text);
                        enter(out, here.clone());
                    }
                    _ => out.unsure = true,
                }
                i += 2;
            }
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" => i += 2,
            _ if t.starts_with("--") || (t.starts_with('-') && t.len() > 1) => i += 1,
            _ => break,
        }
    }
    let Some(sub) = words.get(i) else {
        return;
    };
    if sub.dynamic {
        out.unsure = true;
        return;
    }
    let args = &words[i + 1..];
    let flag = |name: &str| args.iter().any(|w| w.text == name);
    let operands = || args.iter().filter(|w| !w.text.starts_with('-'));
    match sub.text.as_str() {
        "stash" => match operands().next().map(|w| w.text.as_str()) {
            None | Some("push") | Some("save") => out.facts.push(GitFact::StashCreate),
            Some("apply") | Some("pop") => {
                let stash = operands().nth(1).map(|w| w.text.clone());
                out.facts.push(GitFact::StashRestore { stash });
            }
            Some("drop") => out.facts.push(GitFact::StashDrop),
            Some(_) => {}
        },
        "push" => {
            let mut plain = operands();
            let remote = plain.next();
            for spec in plain {
                if let Some((source, branch)) = spec.text.split_once(':')
                    && !source.is_empty()
                    && !branch.is_empty()
                {
                    out.facts.push(GitFact::Push {
                        remote: remote.and_then(|r| remote_name(&r.text)),
                        source: source.to_owned(),
                        branch: branch
                            .strip_prefix("refs/heads/")
                            .unwrap_or(branch)
                            .to_owned(),
                    });
                }
            }
        }
        "cherry-pick" => {
            let skip = ["--continue", "--abort", "--quit", "--skip"];
            if !args.iter().any(|w| skip.contains(&w.text.as_str())) {
                let commits: Vec<String> = operands()
                    .filter(|w| !w.dynamic)
                    .map(|w| w.text.clone())
                    .collect();
                if !commits.is_empty() {
                    out.facts.push(GitFact::CherryPick { commits });
                }
            }
        }
        "rebase" => out.facts.push(GitFact::Rewrite),
        "commit" if flag("--amend") => out.facts.push(GitFact::Rewrite),
        "reset" => out.facts.push(GitFact::Reset),
        "branch" if flag("-f") || flag("--force") => out.facts.push(GitFact::Reset),
        "checkout" if flag("-B") => out.facts.push(GitFact::Reset),
        "checkout" if flag("-b") => out.facts.push(GitFact::NewBranch),
        "switch" if flag("-C") || flag("--force-create") => out.facts.push(GitFact::Reset),
        "switch" if flag("-c") || flag("--create") => out.facts.push(GitFact::NewBranch),
        "worktree" if operands().next().is_some_and(|w| w.text == "add") => {
            if flag("-B") {
                out.facts.push(GitFact::Reset);
            } else if flag("-b") {
                out.facts.push(GitFact::NewBranch);
            }
        }
        _ => {}
    }
}

/// A remote's name when `text` looks like one (not an address).
fn remote_name(text: &str) -> Option<String> {
    let plain = !text.is_empty()
        && !text.contains("://")
        && !text.contains('@')
        && !text.contains(':')
        && !text.starts_with('.')
        && !text.starts_with('/');
    plain.then(|| text.to_owned())
}

/// The bit mask of `call.git` (docs/design/usage.md「数据库」): what a command
/// does, for the rows of commands that mention git.
pub(crate) mod bits {
    use super::{GitFact, Parsed};

    pub(crate) const STASH_CREATE: i64 = 1;
    pub(crate) const STASH_RESTORE: i64 = 2;
    pub(crate) const STASH_DROP: i64 = 4;
    pub(crate) const PUSH: i64 = 8;
    pub(crate) const CHERRY_PICK: i64 = 16;
    pub(crate) const REWRITE: i64 = 32;
    pub(crate) const RESET: i64 = 64;
    pub(crate) const NEW_BRANCH: i64 = 128;
    pub(crate) const ENTERS: i64 = 256;
    pub(crate) const GIT: i64 = 512;
    pub(crate) const UNSURE: i64 = 1024;

    pub(crate) fn of(p: &Parsed) -> i64 {
        let mut mask = 0;
        for fact in &p.facts {
            mask |= match fact {
                GitFact::StashCreate => STASH_CREATE,
                GitFact::StashRestore { .. } => STASH_RESTORE,
                GitFact::StashDrop => STASH_DROP,
                GitFact::Push { .. } => PUSH,
                GitFact::CherryPick { .. } => CHERRY_PICK,
                GitFact::Rewrite => REWRITE,
                GitFact::Reset => RESET,
                GitFact::NewBranch => NEW_BRANCH,
            };
        }
        if !p.entered.is_empty() {
            mask |= ENTERS;
        }
        if p.git {
            mask |= GIT;
        }
        if p.unsure {
            mask |= UNSURE;
        }
        mask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Parsed {
        parse(text, Path::new("/w/repo"))
    }

    fn dirs(parsed: &Parsed) -> Vec<&str> {
        parsed.entered.iter().map(|d| d.to_str().unwrap()).collect()
    }

    #[test]
    fn command_cd_follows_the_working_directory() {
        let r = p("cd sub && ls && cd ../other/. ; pwd");
        assert_eq!(dirs(&r), ["/w/repo/sub", "/w/repo/other"]);
        assert!(!r.git && !r.unsure);
    }

    #[test]
    fn command_cd_absolute_and_quoted() {
        let r = p("cd '/a b/c' && cd \"d e\"");
        assert_eq!(dirs(&r), ["/a b/c", "/a b/c/d e"]);
    }

    #[test]
    fn command_pushd_and_popd() {
        let r = p("pushd /x && pwd && popd");
        assert_eq!(dirs(&r), ["/x", "/w/repo"]);
    }

    #[test]
    fn command_git_c_enters_for_that_command_only() {
        let r = p("git -C ../other status && git status");
        assert_eq!(dirs(&r), ["/w/other"]);
        assert!(r.git);
        // The directory of the second command is still the working one.
        let r = p("git -C /a -C b log");
        assert_eq!(dirs(&r), ["/a", "/a/b"]);
    }

    #[test]
    fn command_variables_make_a_directory_unsure() {
        let r = p("cd \"$HOME/x\" && cd ~ && cd - && git -C $DIR status");
        assert!(r.entered.is_empty());
        assert!(r.unsure);
        // Single quotes do not expand.
        let r = p("cd '$x'");
        assert_eq!(dirs(&r), ["/w/repo/$x"]);
        assert!(!r.unsure);
    }

    #[test]
    fn command_stash_forms() {
        assert_eq!(p("git stash").facts, [GitFact::StashCreate]);
        assert_eq!(p("git stash push -u -m x").facts, [GitFact::StashCreate]);
        assert_eq!(p("git stash save wip").facts, [GitFact::StashCreate]);
        assert_eq!(
            p("git stash pop").facts,
            [GitFact::StashRestore { stash: None }]
        );
        assert_eq!(
            p("git stash apply 'stash@{2}'").facts,
            [GitFact::StashRestore {
                stash: Some("stash@{2}".into())
            }]
        );
        assert_eq!(p("git stash drop").facts, [GitFact::StashDrop]);
        assert!(p("git stash list").facts.is_empty());
    }

    #[test]
    fn command_push_names_only_a_configured_remote() {
        let r = p("git push origin HEAD:refs/heads/feat");
        assert_eq!(
            r.facts,
            [GitFact::Push {
                remote: Some("origin".into()),
                source: "HEAD".into(),
                branch: "feat".into()
            }]
        );
        // An address can carry a token: it is not kept.
        let r = p("git push https://user:SECRET@host/x.git HEAD:feat");
        assert_eq!(
            r.facts,
            [GitFact::Push {
                remote: None,
                source: "HEAD".into(),
                branch: "feat".into()
            }]
        );
        assert!(p("git push -u origin feat").facts.is_empty());
    }

    #[test]
    fn command_cherry_pick_rewrite_reset_and_new_branches() {
        assert_eq!(
            p("git cherry-pick abc123 def456").facts,
            [GitFact::CherryPick {
                commits: vec!["abc123".into(), "def456".into()]
            }]
        );
        assert!(p("git cherry-pick --continue").facts.is_empty());
        assert_eq!(p("git rebase -i main").facts, [GitFact::Rewrite]);
        assert_eq!(p("git commit --amend -m x").facts, [GitFact::Rewrite]);
        assert!(p("git commit -m x").facts.is_empty());
        for text in [
            "git reset --hard HEAD~1",
            "git branch -f feat main",
            "git checkout -B feat",
            "git switch -C feat",
            "git worktree add -B feat ../w",
        ] {
            assert_eq!(p(text).facts, [GitFact::Reset], "{text}");
        }
        for text in [
            "git switch -c feat",
            "git checkout -b feat main",
            "git worktree add -b feat ../w",
        ] {
            assert_eq!(p(text).facts, [GitFact::NewBranch], "{text}");
        }
    }

    #[test]
    fn command_wrappers_assignments_and_heredocs() {
        let r = p("GIT_PAGER=cat env -i git -C /a status");
        assert_eq!(dirs(&r), ["/a"]);
        // The body of a here-document is data.
        let r = p("cat <<EOF\ngit stash\ncd /nope\nEOF\ngit status");
        assert!(r.facts.is_empty());
        assert!(r.entered.is_empty());
        assert!(r.git);
    }

    #[test]
    fn command_other_commands_say_nothing() {
        let r = p("cargo test -p x | tee out.txt");
        assert_eq!(r, Parsed::default());
    }

    #[test]
    fn command_mask_names_what_a_command_does() {
        let r = p("cd x && git stash && git push origin HEAD:b");
        let mask = bits::of(&r);
        assert!(mask & bits::STASH_CREATE != 0);
        assert!(mask & bits::PUSH != 0);
        assert!(mask & bits::ENTERS != 0);
        assert!(mask & bits::GIT != 0);
        assert!(mask & bits::UNSURE == 0);
    }
}
