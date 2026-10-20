//! Worktree fixtures for the `git/changes_*`, `git/refresh_*` and
//! `git/resident` benchmarks (docs/design/git.md「性能」), shared by the
//! `time` and `memory` targets. Built with `lectern_git::testing::TestRepo`:
//! files on disk with the stat `git add` records, backdated so that no
//! index entry is racy. `large` is the 100 000-file shape of the
//! `git/*_100k*` benchmarks.

use std::rc::Rc;
use std::sync::atomic::AtomicBool;

use lectern_bench::{BenchError, Ctx};
use lectern_git::testing::{IndexEntry, IndexOptions, IndexStat, TestHead, TestRepo};
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

/// `large` at the bench's scale (100 000 files and 2 000 manifests, 2 000
/// and 40 in smoke mode), built once per target process and shared by the
/// `git/*_100k*` benchmarks (`Ctx::shared`): writing 100 000 files with
/// their stat takes seconds. Every user only reads it (`changes`,
/// `watch_source().filter`, `SafeRoot` reads, new `Worktree` handles,
/// whose caches live in the handle); none writes the worktree or the git
/// directory.
#[allow(dead_code, reason = "each target uses its own subset of the fixtures")]
pub fn large_shared(ctx: &Ctx) -> Result<Rc<TestRepo>, BenchError> {
    let (files, manifests) = ctx.scale((100_000, 2_000), (2_000, 40));
    ctx.shared(
        &format!("git_worktree::large({files}, {manifests})"),
        || Ok(large(files, manifests)),
    )
}

/// A project design shape as one clean worktree with `core.ignoreCase`,
/// as on macOS: every path a file on disk (the manifests with their
/// contents, `.gitignore` with three rules, the rest one line each), the
/// index with their stat and the cache-tree git writes after a commit, HEAD
/// a tree with the same blob OIDs. Only the trees go into the object
/// database (loose): a clean comparison reads no blob, and with the
/// cache-tree no tree either.
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
    let tree = t.tree_entries(&items);
    let commit = t.commit(tree, &[], "base");
    t.set_ref("refs/heads/main", commit);
    // With the cache-tree git writes after a commit or `read-tree`.
    let options = IndexOptions {
        cache_tree: Some((tree, &[])),
        ..IndexOptions::default()
    };
    t.write_index_with(t.common_dir(), &entries, &options);
    t
}

/// A worktree with realistic changes: `modified` tracked files and
/// `untracked` untracked files of 10–50 KB each (text), plus one untracked
/// file of `large` bytes. The tracked files are committed and staged with
/// their stat before they are rewritten; only trees go into the object
/// database.
#[allow(dead_code, reason = "only the memory target's git/changes_count")]
pub fn mixed(modified: usize, untracked: usize, large: usize) -> TestRepo {
    let t = TestRepo::new();
    let hash = t.hash();
    let size = |i: usize| 10 * 1024 + (i * 7919) % (40 * 1024);
    let text = |seed: &str, len: usize| {
        let line = format!("export const value_{seed} = compute({seed});\n");
        line.repeat(len / line.len() + 1).into_bytes()[..len].to_vec()
    };
    let paths: Vec<String> = (0..modified)
        .map(|i| format!("src/m{:02}/file{i:04}.ts", i % 40))
        .collect();
    let mut entries = Vec::with_capacity(modified);
    for (i, path) in paths.iter().enumerate() {
        let content = text(&format!("old{i}"), size(i));
        t.write_file(path, &content);
        t.backdate(path);
        entries.push(IndexEntry {
            stat: Some(IndexStat::of(&t.root().join(path))),
            ..IndexEntry::new(path, Mode::File, Oid::for_blob(hash, &content))
        });
    }
    let items: Vec<(&str, Mode, Oid)> = paths
        .iter()
        .zip(&entries)
        .map(|(p, e)| (p.as_str(), e.mode, e.oid))
        .collect();
    let commit = t.commit(t.tree_entries(&items), &[], "base");
    t.set_ref("refs/heads/main", commit);
    t.write_index(t.common_dir(), &entries);
    for (i, path) in paths.iter().enumerate() {
        let content = text(&format!("new{i}"), size(i));
        t.write_file(path, &content);
    }
    for i in 0..untracked {
        let content = text(&format!("gen{i}"), size(i + modified));
        t.write_file(&format!("gen/d{:02}/out{i:04}.js", i % 40), &content);
    }
    let dump = text("dump", large);
    t.write_file("dump/trace.log", &dump);
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
