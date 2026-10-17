//! The backend behind a real `Watcher`: `watch_dir` on a root that does not
//! exist yet goes through both halves (kqueue for the parent's entries,
//! FSEvents for the root once it appears) and through `EventSink`.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lectern_executor::{Config as ExecutorConfig, Executor};
use lectern_fs_watch::{Coalesce, Config, DirBatch, DirSpec, RootState, Sink, Watcher};
use lectern_kernel::BusyGuard;

use super::support::DEADLINE;
use crate::fs_events::{backend, lock};

#[derive(Default)]
struct Batches {
    got: Mutex<Vec<DirBatch>>,
    changed: Condvar,
}

impl Sink<DirBatch> for Batches {
    fn deliver(&self, batch: DirBatch, _busy: Option<BusyGuard>) {
        lock(&self.got).push(batch);
        self.changed.notify_all();
    }
}

impl Batches {
    /// Waits until a batch satisfies `done`, calling `poke` between waits.
    fn wait_until(
        &self,
        what: &str,
        mut poke: impl FnMut(),
        done: impl Fn(&DirBatch) -> bool,
    ) -> DirBatch {
        let deadline = Instant::now() + DEADLINE;
        loop {
            {
                let got = lock(&self.got);
                if let Some(batch) = got.iter().find(|batch| done(batch)) {
                    return batch.clone();
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {what}; got {got:?}"
                );
            }
            poke();
            let got = lock(&self.got);
            drop(
                self.changed
                    .wait_timeout(got, Duration::from_millis(250))
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
        }
    }
}

#[test]
fn fsevents_watcher_dir_appears_changes_and_vanishes() {
    let executor = Executor::new(ExecutorConfig::default());
    let watcher = Watcher::new(
        backend(executor.clone()),
        executor.clone(),
        Config::default(),
    );
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sessions");
    let sink = Arc::new(Batches::default());
    let watch = watcher.watch_dir(
        DirSpec {
            root: root.clone(),
            coalesce: Coalesce::DIR,
        },
        sink.clone(),
    );
    sink.wait_until("the first batch", || {}, |batch| batch.health.is_some());

    // The parent's entries are watched through kqueue.
    fs::create_dir(&root).unwrap();
    sink.wait_until(
        "Appeared",
        || {},
        |batch| batch.root == Some(RootState::Appeared),
    );

    // The root itself through FSEvents, which may take a moment to report.
    let mut n = 0;
    let mut written: Vec<PathBuf> = Vec::new();
    sink.wait_until(
        "a file in the root",
        || {
            n += 1;
            let path = root.join(format!("rollout-{n}.jsonl"));
            fs::write(&path, b"{}\n").unwrap();
            written.push(path);
        },
        |batch| {
            batch.paths.iter().any(|path| {
                path.starts_with(&root)
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("rollout-"))
            })
        },
    );
    assert!(!written.is_empty());

    fs::remove_dir_all(&root).unwrap();
    sink.wait_until(
        "Vanished",
        || {},
        |batch| batch.root == Some(RootState::Vanished),
    );

    drop(watch);
    watcher.shutdown();
}
