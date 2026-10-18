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
        let event = self.projects.set(self.git.snapshot(1, "main", Vec::new()));
        self.indexer.project_event(&event);
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
