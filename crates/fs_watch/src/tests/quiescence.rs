//! Quiescence (`quiescence_*`), docs/design/fs_watch.md「数据流与线程」:
//! real guards from the kernel's `TestKernel`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lectern_kernel::testing::TestKernel;
use lectern_kernel::{
    Activation, Active, Context, Kernel, Manifest, Plugin, PluginError, PluginHandle,
};

use super::support::*;
use crate::testing::RecordingSink;
use crate::{Coalesce, WorktreeBatch, WorktreeName, WorktreeSpec};

type Slot = Arc<Mutex<Option<PluginHandle>>>;

struct Capture(Slot);

impl Plugin for Capture {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: Vec::new(),
            activation: vec![Activation::Manual],
        }
    }

    fn activate(&self, _: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        Ok(Box::new(Started(self.0.clone())))
    }
}

struct Started(Slot);

impl Active for Started {
    fn started(&mut self, handle: PluginHandle) {
        *self.0.lock().unwrap() = Some(handle);
    }
}

struct Plugged {
    _test: TestKernel,
    kernel: Arc<Kernel>,
    handle: PluginHandle,
}

impl Plugged {
    fn new() -> Plugged {
        let slot = Slot::default();
        let test = TestKernel::new(Capture(slot.clone()));
        test.activate().expect("activates");
        let kernel = test.inner().expect("running");
        let handle = slot.lock().unwrap().clone().expect("started");
        Plugged {
            _test: test,
            kernel,
            handle,
        }
    }

    fn sink(&self) -> Arc<RecordingSink<WorktreeBatch>> {
        let handle = self.handle.clone();
        RecordingSink::new(Some(Arc::new(move || handle.busy())))
    }

    fn settled(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.kernel.is_quiescent() {
            assert!(Instant::now() < deadline, "the kernel did not settle");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn busy(&self) -> bool {
        !self.kernel.is_quiescent()
    }
}

/// A repository and worktree whose sinks take real guards.
fn open(
    fx: &Fixture,
    plugged: &Plugged,
) -> (
    crate::RepoWatch,
    crate::WorktreeWatch,
    Arc<RecordingSink<WorktreeBatch>>,
) {
    let (repo, _repo_sink) = fx.repo();
    let sink = plugged.sink();
    let worktree = repo.watch_worktree(
        WorktreeSpec {
            name: WorktreeName::Main,
            root: fx.root.clone(),
            source: fx.source.clone(),
            coalesce: Coalesce::WORKTREE,
        },
        sink.clone(),
    );
    (repo, worktree, sink)
}

#[test]
fn quiescence_idle_watcher_is_quiescent() {
    let plugged = Plugged::new();
    let fx = Fixture::new();
    let (_repo, _worktree, sink) = open(&fx, &plugged);
    fx.settle();
    assert_eq!(sink.take().len(), 1);
    plugged.settled();
    fx.wait();
    plugged.settled();
}

#[test]
fn quiescence_from_watch_to_the_first_batch_is_busy() {
    let plugged = Plugged::new();
    let fx = Fixture::new();
    plugged.settled();
    let (_repo, _worktree, sink) = open(&fx, &plugged);
    assert!(plugged.busy(), "watch_* took a guard");
    // Armed, loads not run yet.
    fx.driver.shared().pump(true);
    assert!(plugged.busy(), "the loads hold guards");
    fx.settle();
    // The first batch carries the guard; the consumer still holds it.
    assert!(plugged.busy());
    let batches = sink.take();
    assert!(batches[0].1, "with a guard");
    plugged.settled();
}

#[test]
fn quiescence_coalescing_batch_holds_off_until_delivered_and_dropped() {
    let plugged = Plugged::new();
    let fx = Fixture::new();
    let (_repo, _worktree, sink) = open(&fx, &plugged);
    fx.settle();
    sink.take();
    plugged.settled();

    fx.emit_root(&[("a", F)]);
    // Queued, not yet classified: the guard taken by `send` counts.
    assert!(plugged.busy());
    fx.settle();
    assert!(plugged.busy(), "the open batch holds a guard");
    fx.wait();
    assert!(plugged.busy(), "delivered: the consumer holds the guard");
    let batches = sink.take();
    assert!(batches[0].1);
    plugged.settled();
}

#[test]
fn quiescence_ignored_events_release_once_classified() {
    let plugged = Plugged::new();
    let fx = Fixture::new();
    fx.source.ignore(&["node_modules"]);
    let (_repo, _worktree, sink) = open(&fx, &plugged);
    fx.settle();
    sink.take();
    plugged.settled();
    fx.emit_root(&[("node_modules/a/b.js", F)]);
    assert!(plugged.busy());
    fx.driver.shared().pump(true);
    plugged.settled();
    assert!(sink.is_empty());
}

#[test]
fn quiescence_held_paths_keep_the_batch_busy_until_the_filter_is_in() {
    let plugged = Plugged::new();
    let fx = Fixture::new();
    let (_repo, _worktree, sink) = open(&fx, &plugged);
    fx.settle();
    sink.take();
    plugged.settled();
    fx.emit_common(&[("index", F)]);
    fx.driver.shared().pump(true);
    fx.emit_root(&[("a", F)]);
    fx.driver.shared().pump(true);
    fx.clock_advance(LONG);
    fx.driver.shared().pump(true);
    assert!(plugged.busy(), "load running and paths held");
    fx.settle();
    let batches = sink.take();
    assert_eq!(batches.len(), 1);
    plugged.settled();
}
