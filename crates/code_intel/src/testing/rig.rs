//! For the benchmarks: an indexer over a `FakeGit` repository with every
//! dependency in memory (`Rig`), and tree-sitter's C allocations routed
//! through Rust's global allocator so that a counting allocator sees
//! syntax trees.

use std::alloc::{Layout, alloc, alloc_zeroed, dealloc, handle_alloc_error, realloc};
use std::ffi::c_void;
use std::sync::Arc;
use std::time::Duration;

use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, manual};
use lectern_fs_watch::{Config as WatchConfig, Watcher};

use super::{FakeGit, FakePower, FakeProjects, TestHost, WatchdogTask, host, watchdog};
use crate::index::{IndexDeps, Indexer};
use crate::watchdog::Clock;

/// An indexer over a `FakeGit` repository: a host kernel, a manual
/// executor, a manual fs_watch, AC power and no language packs.
pub struct Rig {
    pub host: TestHost,
    pub executor: Executor,
    pub git: Arc<FakeGit>,
    pub projects: Arc<FakeProjects>,
    pub indexer: Indexer,
    driver: Driver,
    _backend: Arc<FakeBackend>,
    _watcher: Watcher,
    _watchdog: WatchdogTask,
    _subscriptions: Vec<Box<dyn Send>>,
}

impl Rig {
    pub fn new(git: Arc<FakeGit>, cache: Cache, clock: Arc<dyn Clock>) -> Rig {
        let host = host();
        let executor = Executor::manual();
        let projects = FakeProjects::new();
        let backend = FakeBackend::new();
        let (watcher, driver) = manual(backend.clone(), Executor::manual(), WatchConfig::default());
        let (wd, task) = watchdog(&executor, clock);
        let indexer = Indexer::with_fake_git(
            IndexDeps {
                executor: executor.clone(),
                cache,
                watcher: watcher.clone(),
                power: FakePower::new(false),
                grammars: Arc::new(crate::NoGrammarPacks),
                projects: projects.clone(),
                handle: host.handle.clone(),
                watchdog: wd,
                batch_tasks: 1,
            },
            git.clone(),
        );
        let subscriptions = indexer.subscribe();
        Rig {
            host,
            executor,
            git,
            projects,
            indexer,
            driver,
            _backend: backend,
            _watcher: watcher,
            _watchdog: task,
            _subscriptions: subscriptions,
        }
    }

    /// Opens the project with the main worktree active, without running
    /// anything.
    pub fn open(&self) {
        self.open_with(Vec::new());
    }

    /// The same with sub-projects.
    pub fn open_with(&self, packages: Vec<lectern_services::project::Package>) {
        let event = self.projects.set(self.git.snapshot(1, "main", packages));
        self.indexer.project_event(&event);
    }

    /// Subscribes to the main worktree as `project` does before this crate
    /// (「依赖 git、fs_watch 与 cache 的接口」): the worktree state, its
    /// filter of the tracked paths included, is fs_watch's and shared, not
    /// this crate's memory. Keep the returned value.
    pub fn watch_as_project(&self) -> Box<dyn Send> {
        use crate::git_access::{GitAccess, Tree};
        use lectern_fs_watch::testing::RecordingSink;
        use lectern_fs_watch::{
            Coalesce, RepoBatch, RepoSpec, WorktreeBatch, WorktreeName, WorktreeSpec,
        };
        let common: Arc<std::path::Path> = Arc::from(self.git.common_dir());
        let root: Arc<std::path::Path> = Arc::from(self.git.root("main").as_path());
        let tree = Tree {
            common_dir: common.clone(),
            git_dir: common.clone(),
            root: root.clone(),
        };
        let repo = self._watcher.watch_repo(
            RepoSpec {
                common_dir: common.to_path_buf(),
                coalesce: Coalesce::REPO,
            },
            RecordingSink::<RepoBatch>::new(None),
        );
        let worktree = repo.watch_worktree(
            WorktreeSpec {
                name: WorktreeName::Main,
                root: root.to_path_buf(),
                source: self.git.watch_source(&tree),
                coalesce: Coalesce::WORKTREE,
            },
            RecordingSink::<WorktreeBatch>::new(None),
        );
        self.settle();
        Box::new((repo, worktree))
    }

    /// Runs everything until nothing is left to do.
    pub fn settle(&self) {
        loop {
            let a = self.driver.run_until_idle();
            self.driver.advance(Duration::from_secs(5));
            let b = self.executor.run_until_idle();
            if a == 0 && b == 0 && self.driver.run_until_idle() == 0 {
                return;
            }
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.indexer.close_all();
    }
}

/// Sub-projects at `dirs`.
pub fn packages(dirs: &[String]) -> Vec<lectern_services::project::Package> {
    dirs.iter()
        .map(|d| lectern_services::project::Package {
            dir: d.as_str().into(),
            name: d.as_str().into(),
            manifests: Arc::from(Vec::new()),
            declared: false,
            workspace_root: false,
            parent: None,
            files: 0,
        })
        .collect()
}

/// A virtual repository for the benchmarks: files `0..n` of the synthetic
/// layout (`synthetic::path`, `synthetic::oid`) staged in `worktrees` with
/// no content behind them, the worktree comparison taken as clean, and the
/// synthetic extraction (`synthetic::extraction`) of every source file
/// stored in the repository's partition of `cache`. Opening it is a warm
/// start that reads no file. Returns the number of units stored.
pub fn virtual_repo(git: &FakeGit, cache: &Cache, n: usize, worktrees: &[&str]) -> usize {
    virtual_repo_with(git, cache, n, worktrees, None)
}

/// [`virtual_repo`] whose every unit also defines the function `common`
/// (a name like `id` that tens of thousands of files define).
pub fn virtual_repo_with(
    git: &FakeGit,
    cache: &Cache,
    n: usize,
    worktrees: &[&str],
    common: Option<&str>,
) -> usize {
    virtual_repo_sharing(git, cache, n, worktrees, common.map(|c| (c, 1)))
}

/// [`virtual_repo`] whose every `every`th file also defines the function
/// `name` of `common` (a popular name like `get`: one file in 30 of a
/// 100 000-file repository is some 3000 definitions).
pub fn virtual_repo_sharing(
    git: &FakeGit,
    cache: &Cache,
    n: usize,
    worktrees: &[&str],
    common: Option<(&str, usize)>,
) -> usize {
    use crate::store::{Store, Version};
    git.assume_unchanged(true);
    let entries: Vec<(String, lectern_services::project::BlobId)> = (0..n)
        .map(|i| (super::synthetic::path(i), super::synthetic::oid(i)))
        .collect();
    for wt in worktrees {
        git.stage_oids(wt, entries.iter().cloned());
    }
    let store = Store::new(&cache.repo(git.common_dir()));
    let table = crate::extract::LanguageTable::core();
    let mut stored = 0;
    let mut write = store.write();
    for (i, (path, oid)) in entries.iter().enumerate() {
        let Some(lang) = table.for_path(path, &crate::NoGrammarPacks) else {
            continue;
        };
        let mut extraction = super::synthetic::extraction(i);
        if let Some((name, every)) = common
            && i % every.max(1) == 0
        {
            let mut def = extraction.defs[1].clone();
            def.name = name.into();
            def.line = 200;
            def.end_line = 203;
            extraction.defs.push(def);
        }
        write.add(*oid, &Version::of(lang), &extraction);
        stored += 1;
        if write.len() == 64 {
            let full = std::mem::replace(&mut write, store.write());
            let written = full.commit();
            assert!(
                matches!(written, lectern_cache::Written::Committed),
                "{written:?}"
            );
        }
    }
    if !write.is_empty() {
        let written = write.commit();
        assert!(
            matches!(written, lectern_cache::Written::Committed),
            "{written:?}"
        );
    }
    stored
}

/// A [`virtual_repo_sharing`] of the main worktree written once and opened
/// by many rigs in turn (the benchmarks' shared fixture, ADR 0036): writing
/// 100 000 units takes about a minute, opening a copy a second or two.
/// After [`build`](Self::build) nothing opens the written cache again; each
/// [`open`](Self::open) copies its files and stages the entries in a new
/// `FakeGit`, so what a rig writes (units, access times, evictions) stays in
/// its own copy.
pub struct VirtualTemplate {
    /// `git`: where each opened repository lives (its common directory names
    /// the cache partition, so every copy must see the same one); `cache`:
    /// the written cache, only ever copied.
    dir: tempfile::TempDir,
    n: usize,
    units: usize,
}

impl VirtualTemplate {
    /// Writes the units of `n` files, every `every`th of them also defining
    /// `common`'s name, into a cache with `budgets` in the template's own
    /// directory.
    pub fn build(
        n: usize,
        common: Option<(&str, usize)>,
        budgets: lectern_cache::Budgets,
    ) -> std::io::Result<VirtualTemplate> {
        let dir = tempfile::tempdir()?;
        let git = FakeGit::at(&dir.path().join("git"))?;
        let cache = Cache::new(lectern_cache::Config {
            root: dir.path().join("cache"),
            budgets,
            ui_thread: None,
        });
        let units = virtual_repo_sharing(&git, &cache, n, &["main"], common);
        // Closes the partition before anything copies it.
        drop(cache);
        drop(git);
        Ok(VirtualTemplate { dir, n, units })
    }

    /// The number of files.
    pub fn files(&self) -> usize {
        self.n
    }

    /// The number of units stored.
    pub fn units(&self) -> usize {
        self.units
    }

    /// A repository of its own with the entries staged in the main worktree
    /// (the comparison taken as clean), and a cache with `budgets` rooted at
    /// `root` (which must not exist) over a copy of the written one. Fails
    /// while the repository of an earlier `open` is still alive.
    pub fn open(
        &self,
        root: &std::path::Path,
        budgets: lectern_cache::Budgets,
    ) -> std::io::Result<(Arc<FakeGit>, Cache)> {
        let git = FakeGit::at(&self.dir.path().join("git"))?;
        git.assume_unchanged(true);
        git.stage_oids(
            "main",
            (0..self.n).map(|i| (super::synthetic::path(i), super::synthetic::oid(i))),
        );
        copy_tree(&self.dir.path().join("cache"), root)?;
        let cache = Cache::new(lectern_cache::Config {
            root: root.to_path_buf(),
            budgets,
            ui_thread: None,
        });
        Ok((Arc::new(git), cache))
    }
}

/// Copies the directory `from` to the new directory `to`, leaving out
/// SQLite's shared-memory files (rebuilt on open).
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if !entry.file_name().to_string_lossy().ends_with("-shm") {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Every block starts with its size, so `free` and `realloc` can rebuild
/// the layout. 16 keeps the alignment `malloc` gives.
const HEADER: usize = 16;

fn layout(size: usize) -> Layout {
    Layout::from_size_align(size + HEADER, HEADER)
        .unwrap_or_else(|_| handle_alloc_error(Layout::new::<u128>()))
}

/// Writes the header of a block and returns the pointer handed out.
///
/// # Safety
///
/// `base` was allocated with `layout(size)` and is not null.
unsafe fn finish(base: *mut u8, size: usize) -> *mut c_void {
    // SAFETY: `base` holds at least HEADER bytes aligned to 16 (the
    // caller's contract), so a `usize` fits at its start and `base + HEADER`
    // stays inside the block.
    unsafe {
        base.cast::<usize>().write(size);
        base.add(HEADER).cast()
    }
}

unsafe extern "C" fn ts_malloc(size: usize) -> *mut c_void {
    let layout = layout(size);
    // SAFETY: the layout has a non-zero size.
    let base = unsafe { alloc(layout) };
    if base.is_null() {
        handle_alloc_error(layout);
    }
    // SAFETY: allocated just above with `layout(size)`, not null.
    unsafe { finish(base, size) }
}

unsafe extern "C" fn ts_calloc(count: usize, size: usize) -> *mut c_void {
    let size = count.saturating_mul(size);
    let layout = layout(size);
    // SAFETY: the layout has a non-zero size.
    let base = unsafe { alloc_zeroed(layout) };
    if base.is_null() {
        handle_alloc_error(layout);
    }
    // SAFETY: allocated just above with `layout(size)`, not null.
    unsafe { finish(base, size) }
}

unsafe extern "C" fn ts_realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
    if ptr.is_null() {
        // SAFETY: `ts_malloc` has no precondition.
        return unsafe { ts_malloc(size) };
    }
    // SAFETY: `ptr` came from one of these functions, so its block starts
    // HEADER bytes before it with the block's size.
    let (base, old) = unsafe {
        let base = ptr.cast::<u8>().sub(HEADER);
        (base, base.cast::<usize>().read())
    };
    // SAFETY: `base` was allocated with `layout(old)`; the new size is
    // non-zero.
    let grown = unsafe { realloc(base, layout(old), size + HEADER) };
    if grown.is_null() {
        handle_alloc_error(layout(size));
    }
    // SAFETY: `realloc` returned a block of `layout(size)`, not null.
    unsafe { finish(grown, size) }
}

unsafe extern "C" fn ts_free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: as in `ts_realloc`: the block and its size precede `ptr`.
    unsafe {
        let base = ptr.cast::<u8>().sub(HEADER);
        let size = base.cast::<usize>().read();
        dealloc(base, layout(size));
    }
}

/// Routes tree-sitter's C allocations through Rust's global allocator, so
/// that a counting global allocator (the memory benchmarks') sees parsers,
/// syntax trees and query cursors.
///
/// # Safety
///
/// Call it once, before the process makes any tree-sitter call, and never
/// while tree-sitter objects are alive (`tree_sitter::set_allocator`).
pub unsafe fn route_tree_sitter_allocations() {
    // SAFETY: the four functions are one allocator family over the global
    // allocator, never return null (they abort instead) and align like
    // `malloc`; the caller guarantees the timing.
    unsafe {
        tree_sitter::set_allocator(Some(tree_sitter::Allocator {
            malloc: ts_malloc,
            calloc: ts_calloc,
            realloc: ts_realloc,
            free: ts_free,
        }));
    }
}
