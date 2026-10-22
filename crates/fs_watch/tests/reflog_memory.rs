//! 「性能」: what one repository state keeps per reflog file, and the state
//! with 1000 branch reflogs under 256 KB. Its own test binary, with a global
//! allocator that counts the live bytes of each thread; the manual watcher
//! runs the fs_watch engine on the calling thread.

#![cfg(feature = "test-support")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::path::Path;
use std::time::Duration;

use lectern_executor::Executor;
use lectern_fs_watch::testing::{FakeBackend, RecordingSink, manual};
use lectern_fs_watch::{Coalesce, Config, RepoSpec};

struct PerThread;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

fn count(bytes: isize) {
    let _ = LIVE.try_with(|live| live.set(live.get() + bytes));
}

// SAFETY: forwards every call to `System` unchanged; the bookkeeping uses a
// const-initialized thread local without a destructor, which never allocates.
unsafe impl GlobalAlloc for PerThread {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            count(layout.size() as isize);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by `System` with `layout`.
        unsafe { System.dealloc(ptr, layout) };
        count(-(layout.size() as isize));
    }
}

#[global_allocator]
static ALLOC: PerThread = PerThread;

fn live() -> isize {
    LIVE.with(Cell::get)
}

const BRANCHES: usize = 1000;

fn line(n: usize) -> String {
    format!(
        "{n:040} {:040} A U Thor <a@example.com> 1700000000 +0000\tcommit: {n}\n",
        n + 1
    )
}

fn write(path: &Path, contents: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Live bytes a watched repository with `branches` branch reflogs (and
/// `logs/HEAD`) holds once its first batch is delivered, the `Watcher`
/// itself excluded.
fn held(branches: usize) -> isize {
    let dir = tempfile::tempdir().unwrap();
    let common = dir.path().join("repo/.git");
    let two = format!("{}{}", line(0), line(1));
    write(&common.join("logs/HEAD"), two.as_bytes());
    fs::create_dir_all(common.join("refs/heads")).unwrap();
    for i in 0..branches {
        let rel = format!("logs/refs/heads/feature/review-{i:04}");
        write(&common.join(rel), two.as_bytes());
    }
    let backend = FakeBackend::new();
    let (watcher, driver) = manual(backend.clone(), Executor::manual(), Config::default());
    let sink = RecordingSink::new(None);
    let before = live();
    let repo = watcher.watch_repo(
        RepoSpec {
            common_dir: common,
            coalesce: Coalesce::REPO,
        },
        sink.clone(),
    );
    driver.run_until_idle();
    driver.advance(Duration::from_secs(5));
    let batches = sink.take();
    assert_eq!(batches.len(), 1);
    assert!(batches[0].0.rescan);
    assert!(batches[0].0.reflog.is_empty());
    drop(batches);
    let held = live() - before;
    drop(repo);
    driver.run_until_idle();
    held
}

#[test]
fn reflog_progress_per_file_and_per_repository_state() {
    // The first run sets up lazy, process-wide state.
    held(0);
    let empty = held(0);
    let full = held(BRANCHES);
    let per_file = (full - empty) / BRANCHES as isize;
    eprintln!(
        "repository state: {empty} B without branches, {full} B with {BRANCHES}; {per_file} B per reflog file"
    );
    assert!(full < 256 << 10, "{full} B for {BRANCHES} branch reflogs");
    // 115 B on Linux; the map's fill depends on the directory order.
    assert!(per_file < 150, "{per_file} B per reflog file");
}
