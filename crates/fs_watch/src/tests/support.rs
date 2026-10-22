//! Fixtures shared by the tests: a repository in a temporary directory, the
//! fake backend and source, a manual watcher, and text renderings of
//! batches for snapshots.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lectern_executor::Executor;
use tempfile::TempDir;

use crate::testing::{Driver, FakeBackend, FakeSource, RecordingSink, manual};
use crate::{
    Coalesce, Config, DirBatch, DirSpec, DirWatch, Flags, Health, RepoBatch, RepoSpec, RepoWatch,
    WatchPath, Watcher, WorktreeBatch, WorktreeName, WorktreeSpec, WorktreeWatch,
};

pub(crate) const F: Flags = Flags::FILE;
pub(crate) const D: Flags = Flags::DIR;

/// Long enough for every default coalescing window to end.
pub(crate) const LONG: Duration = Duration::from_secs(5);

pub(crate) struct Fixture {
    pub(crate) _dir: TempDir,
    /// The temporary directory, canonical.
    pub(crate) base: PathBuf,
    /// `<base>/repo`, the main worktree.
    pub(crate) root: PathBuf,
    /// `<base>/repo/.git`.
    pub(crate) common: PathBuf,
    pub(crate) backend: Arc<FakeBackend>,
    pub(crate) source: Arc<FakeSource>,
    pub(crate) watcher: Watcher,
    pub(crate) driver: Driver,
}

impl Fixture {
    pub(crate) fn new() -> Fixture {
        Fixture::with_config(Config::default())
    }

    pub(crate) fn with_config(config: Config) -> Fixture {
        let dir = tempfile::tempdir().expect("temporary directory");
        let base = fs::canonicalize(dir.path()).expect("canonical");
        let root = base.join("repo");
        let common = root.join(".git");
        fs::create_dir_all(common.join("logs/refs/heads")).unwrap();
        fs::create_dir_all(common.join("refs/heads")).unwrap();
        let backend = FakeBackend::new();
        let executor = Executor::manual();
        let (watcher, driver) = manual(backend.clone(), executor.clone(), config);
        Fixture {
            _dir: dir,
            base,
            root,
            common,
            backend,
            source: FakeSource::new(),
            watcher,
            driver,
        }
    }

    pub(crate) fn repo(&self) -> (RepoWatch, Arc<RecordingSink<RepoBatch>>) {
        self.repo_with(Coalesce::REPO)
    }

    pub(crate) fn repo_with(
        &self,
        coalesce: Coalesce,
    ) -> (RepoWatch, Arc<RecordingSink<RepoBatch>>) {
        let sink = RecordingSink::new(None);
        let watch = self.watcher.watch_repo(
            RepoSpec {
                common_dir: self.common.clone(),
                coalesce,
            },
            sink.clone(),
        );
        (watch, sink)
    }

    pub(crate) fn worktree(
        &self,
        repo: &RepoWatch,
    ) -> (WorktreeWatch, Arc<RecordingSink<WorktreeBatch>>) {
        self.worktree_with(repo, Coalesce::WORKTREE)
    }

    pub(crate) fn worktree_with(
        &self,
        repo: &RepoWatch,
        coalesce: Coalesce,
    ) -> (WorktreeWatch, Arc<RecordingSink<WorktreeBatch>>) {
        let sink = RecordingSink::new(None);
        let watch = repo.watch_worktree(
            WorktreeSpec {
                name: WorktreeName::Main,
                root: self.root.clone(),
                source: self.source.clone(),
                coalesce,
            },
            sink.clone(),
        );
        (watch, sink)
    }

    pub(crate) fn dir(&self, root: &Path) -> (DirWatch, Arc<RecordingSink<DirBatch>>) {
        let sink = RecordingSink::new(None);
        let watch = self.watcher.watch_dir(
            DirSpec {
                root: root.to_path_buf(),
                coalesce: Coalesce::DIR,
            },
            sink.clone(),
        );
        (watch, sink)
    }

    /// A repository and its main worktree, both past their first batch.
    pub(crate) fn opened(&self) -> Opened {
        let (repo, repo_sink) = self.repo();
        let (worktree, sink) = self.worktree(&repo);
        self.settle();
        let first = sink.take();
        assert_eq!(first.len(), 1, "one first batch");
        assert_eq!(first[0].0.health, Some(Health::Live));
        assert!(first[0].0.rescan);
        let first = repo_sink.take();
        assert_eq!(first.len(), 1, "one first repository batch");
        Opened {
            repo,
            repo_sink,
            worktree,
            sink,
        }
    }

    pub(crate) fn settle(&self) {
        self.driver.run_until_idle();
    }

    /// Lets every coalescing window end.
    pub(crate) fn wait(&self) {
        self.driver.advance(LONG);
    }

    pub(crate) fn clock_advance(&self, by: Duration) {
        self.driver.advance_clock(by);
    }

    pub(crate) fn emit_root(&self, events: &[(&str, Flags)]) {
        self.backend.emit(&self.root, events);
    }

    pub(crate) fn emit_common(&self, events: &[(&str, Flags)]) {
        self.backend.emit(&self.common, events);
    }
}

pub(crate) struct Opened {
    pub(crate) repo: RepoWatch,
    pub(crate) repo_sink: Arc<RecordingSink<RepoBatch>>,
    pub(crate) worktree: WorktreeWatch,
    pub(crate) sink: Arc<RecordingSink<WorktreeBatch>>,
}

pub(crate) fn wp(path: &str) -> WatchPath {
    WatchPath::new(path.as_bytes()).expect("valid path")
}

/// Writes `contents` to `path`, creating its directories.
pub(crate) fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

pub(crate) fn append(path: &Path, contents: &[u8]) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .unwrap();
    file.write_all(contents).unwrap();
}

// --- renderings ----------------------------------------------------------------

fn health(health: &Option<Health>) -> String {
    match health {
        None => String::new(),
        Some(health) => format!(" health={health:?}"),
    }
}

fn guard(guard: bool) -> &'static str {
    if guard { " guard" } else { "" }
}

/// One line per worktree batch; revisions relative to `base`.
pub(crate) fn worktree_batches(batches: &[(WorktreeBatch, bool)], base: u64) -> String {
    let mut out = String::new();
    for (batch, has_guard) in batches {
        let paths: Vec<String> = batch
            .paths
            .iter()
            .map(|path| String::from_utf8_lossy(path.as_bytes()).into_owned())
            .collect();
        let meta = batch.meta;
        let flags: Vec<&str> = [
            (meta.head, "head"),
            (meta.index, "index"),
            (meta.refs, "refs"),
            (meta.config, "config"),
            (meta.rules, "rules"),
        ]
        .into_iter()
        .filter_map(|(set, name)| set.then_some(name))
        .collect();
        let _ = writeln!(
            out,
            "rev={} paths={paths:?} meta={flags:?}{}{}{}",
            batch.revision.n - base,
            if batch.rescan { " rescan" } else { "" },
            health(&batch.health),
            guard(*has_guard),
        );
    }
    out
}

/// One line per repository batch, with one indented line per reflog record
/// or reset, merged by `seq`.
pub(crate) fn repo_batches(batches: &[(RepoBatch, bool)]) -> String {
    let mut out = String::new();
    for (batch, has_guard) in batches {
        let worktrees: Vec<String> = batch
            .worktrees
            .iter()
            .map(|(name, meta)| {
                let name = match name {
                    WorktreeName::Main => "main".to_owned(),
                    WorktreeName::Linked(name) => name.to_string(),
                };
                format!(
                    "{name}{}{}",
                    if meta.head { "+head" } else { "" },
                    if meta.index { "+index" } else { "" }
                )
            })
            .collect();
        let flags: Vec<&str> = [
            (batch.worktree_list, "worktree_list"),
            (batch.refs, "refs"),
            (batch.config, "config"),
            (batch.rescan, "rescan"),
        ]
        .into_iter()
        .filter_map(|(set, name)| set.then_some(name))
        .collect();
        let _ = writeln!(
            out,
            "worktrees={worktrees:?} {flags:?}{}{}",
            health(&batch.health),
            guard(*has_guard)
        );
        out.push_str(&reflog_lines(batch));
    }
    out
}

/// The batch's records and resets merged by `seq`.
pub(crate) fn reflog_lines(batch: &RepoBatch) -> String {
    let mut lines: Vec<(u64, String)> = Vec::new();
    for record in &batch.reflog {
        lines.push((
            record.seq,
            format!(
                "  seq={} {:?} offset={} {:?} observed=r{} after={} {:?}",
                record.seq,
                file(&record.file),
                record.offset,
                String::from_utf8_lossy(&record.line),
                record.observed.read,
                record
                    .after
                    .map_or("-".to_owned(), |after| format!("r{}", after.read)),
                record.origin,
            ),
        ));
    }
    for reset in &batch.reflog_resets {
        lines.push((
            reset.seq,
            format!(
                "  seq={} {:?} reset {:?} observed=r{}",
                reset.seq,
                file(&reset.file),
                reset.reason,
                reset.observed.read
            ),
        ));
    }
    lines.sort();
    let mut out = String::new();
    for (_, line) in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

pub(crate) fn file(file: &crate::ReflogFile) -> String {
    match file {
        crate::ReflogFile::Head(WorktreeName::Main) => "HEAD".to_owned(),
        crate::ReflogFile::Head(WorktreeName::Linked(name)) => format!("HEAD({name})"),
        crate::ReflogFile::Branch(name) => String::from_utf8_lossy(name).into_owned(),
        crate::ReflogFile::Stash => "stash".to_owned(),
    }
}

/// One line per directory batch, with paths relative to `base`.
pub(crate) fn dir_batches(batches: &[(DirBatch, bool)], base: &Path) -> String {
    let mut out = String::new();
    for (batch, has_guard) in batches {
        let paths: Vec<String> = batch
            .paths
            .iter()
            .map(|path| {
                path.strip_prefix(base)
                    .unwrap_or(path)
                    .display()
                    .to_string()
            })
            .collect();
        let _ = writeln!(
            out,
            "paths={paths:?}{}{}{}{}",
            if batch.rescan { " rescan" } else { "" },
            batch
                .root
                .map_or(String::new(), |root| format!(" root={root:?}")),
            health(&batch.health),
            guard(*has_guard),
        );
    }
    out
}

/// Targets armed now, relative to `base`.
pub(crate) fn targets(backend: &FakeBackend, base: &Path) -> Vec<String> {
    backend
        .targets()
        .into_iter()
        .map(|target| {
            let path = target.path.strip_prefix(base).unwrap_or(&target.path);
            match target.scope {
                crate::Scope::Tree => format!("{} tree", path.display()),
                crate::Scope::Entries { names } => format!(
                    "{} entries {:?}",
                    path.display(),
                    names
                        .iter()
                        .map(|name| name.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                ),
            }
        })
        .collect()
}

/// The backend's arm and drop history, relative to `base`.
pub(crate) fn history(backend: &FakeBackend, base: &Path) -> Vec<String> {
    let base = format!("{}/", base.display());
    backend
        .history()
        .into_iter()
        .map(|line| line.replace(&base, ""))
        .collect()
}
