//! Worktree fixtures for the `git/changes_*`, `git/refresh_*` and
//! `git/resident` benchmarks (docs/design/git.md「性能」), shared by the
//! `time` and `memory` targets. Built with `lectern_git::testing::TestRepo`:
//! files on disk with the stat `git add` records, backdated so that no
//! index entry is racy. `large` is the 100 000-file shape of the
//! `git/*_100k*` benchmarks.

use std::sync::atomic::AtomicBool;

use lectern_bench::BenchError;
use lectern_git::testing::{IndexEntry, IndexStat, TestHead, TestRepo};
use lectern_git::{Base, ChangeSet, Limits, Mode, Oid, Worktree, WorktreeId};
use lectern_project::testing::Shape;

/// Linked worktrees besides the main one (11 in all, as in `git/open`).
const LINKED: usize = 10;

/// A file of about 300 lines; `edited` changes 10 of them, `round` makes
/// the content new to the process-wide hunk cache.
pub fn module(n: usize, round: usize, edited: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for line in 0..300 {
        let text = if edited && line % 30 == 7 {
            format!("    let changed_{n}_{line} = compute({round}, {line});\n")
        } else {
            format!("    let value_{n}_{line} = compute({round}, {line});\n")
        };
        out.extend_from_slice(text.as_bytes());
    }
    out
}

pub fn module_path(n: usize) -> String {
    format!("src/module{n:02}.rs")
}

/// `files` tracked files (of which `modules` are `module`s), committed as
/// HEAD and staged with their stat, plus 10 linked worktrees with the same
/// index: a clean `cloudprint-miniapps`-shaped worktree.
pub fn clean(files: usize, modules: usize, round: usize) -> TestRepo {
    let t = TestRepo::new();
    let mut contents: Vec<(String, Vec<u8>)> = (0..files - modules)
        .map(|i| {
            (
                format!("packages/p{:02}/src/file{i:03}.ts", i % 25),
                format!("export const x{i} = {round};\n").into_bytes(),
            )
        })
        .collect();
    contents.extend((0..modules).map(|n| (module_path(n), module(n, round, false))));
    for (path, content) in &contents {
        t.write_file(path, content);
        t.backdate(path);
    }
    let refs: Vec<(&str, &[u8])> = contents
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    let commit = t.commit(t.tree(&refs), &[], "base");
    t.set_ref("refs/heads/main", commit);
    let entries: Vec<IndexEntry> = contents.iter().map(|(p, _)| t.add(p)).collect();
    t.write_index(t.common_dir(), &entries);
    for i in 0..LINKED {
        let branch = format!("refs/heads/wt{i}");
        t.set_ref(&branch, commit);
        let admin = t.add_worktree(
            &format!("wt{i}"),
            &t.base().join(format!("wt{i}")),
            TestHead::Branch(&branch),
        );
        t.write_index(&admin, &entries);
    }
    t
}

/// Edits every module in 10 lines; `backdate` moves the edits more than a
/// second into the past (stat-cache hits for a held change set).
pub fn edit_modules(t: &TestRepo, modules: usize, round: usize, backdate: bool) {
    for n in 0..modules {
        t.write_file(&module_path(n), &module(n, round, true));
        if backdate {
            t.backdate(&module_path(n));
        }
    }
}

/// The large repository of the project design (`Shape::monorepo(files,
/// manifests)`, 100 000 files in「性能」) as a clean worktree, see
/// `shaped`.
pub fn large(files: usize, manifests: usize) -> TestRepo {
    shaped(&Shape::monorepo(files, manifests))
}

/// A project design shape as one clean worktree with `core.ignoreCase`,
/// as on macOS: every path a file on disk (the manifests with their
/// contents, `.gitignore` with three rules, the rest one line each), the
/// index with their stat, HEAD a tree with the same blob OIDs. Only the
/// trees go into the object database: a clean comparison reads no blob.
pub fn shaped(shape: &Shape) -> TestRepo {
    let t = TestRepo::new();
    t.append_config("[core]\n\tignoreCase = true\n");
    let hash = t.hash();
    let mut manifests = shape.manifests.iter().peekable();
    let mut entries: Vec<IndexEntry> = Vec::with_capacity(shape.paths.len());
    for path in &shape.paths {
        let content: Vec<u8> = match manifests.peek() {
            Some((p, content)) if p == path => {
                manifests.next();
                content.clone()
            }
            _ if path == ".gitignore" => b"node_modules/\ndist/\n*.log\n".to_vec(),
            _ => format!("// {path}\n").into_bytes(),
        };
        t.write_file(path, &content);
        t.backdate(path);
        entries.push(IndexEntry {
            stat: Some(IndexStat::of(&t.root().join(path))),
            ..IndexEntry::new(path, Mode::File, Oid::for_blob(hash, &content))
        });
    }
    let items: Vec<(&str, Mode, Oid)> = shape
        .paths
        .iter()
        .zip(&entries)
        .map(|(p, e)| (p.as_str(), e.mode, e.oid))
        .collect();
    let commit = t.commit(t.tree_entries(&items), &[], "base");
    t.set_ref("refs/heads/main", commit);
    t.write_index(t.common_dir(), &entries);
    t
}

/// A clean repository with `untracked` untracked files of three lines.
pub fn untracked(untracked: usize) -> TestRepo {
    let t = clean(2, 0, 0);
    for i in 0..untracked {
        t.write_file(
            &format!("gen/d{:02}/f{i:05}.txt", i % 50),
            format!("line one {i}\nline two\nline three\n").as_bytes(),
        );
    }
    t
}

pub fn main_worktree(t: &TestRepo) -> Result<Worktree, BenchError> {
    Ok(t.repo().worktree(&WorktreeId::Main)?)
}

pub fn changes(
    wt: &Worktree,
    limits: &Limits,
    cancel: &AtomicBool,
) -> Result<std::sync::Arc<ChangeSet>, BenchError> {
    Ok(wt.changes(Base::Head, limits, cancel)?)
}

/// Checks a change set's file and listed counts.
pub fn expect(set: &ChangeSet, files: usize, listed: usize) -> Result<(), BenchError> {
    if set.files().len() != files || set.totals().listed != listed {
        return Err(BenchError::new(format!(
            "expected {files} changed files ({listed} listed), got {} ({} listed)",
            set.files().len(),
            set.totals().listed
        )));
    }
    Ok(())
}
