use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lectern_executor::{Config, Executor};
use lectern_fs_watch::{Armed, Flags, RawEvent, Scope, Target};

use crate::fs_events::{Deliver, MacBackend, lock};

/// How long a test waits for an event before failing. Generous: macOS CI
/// runners are shared and FSEvents can lag behind under load.
pub(super) const DEADLINE: Duration = Duration::from_secs(30);

pub(super) fn backend() -> MacBackend {
    MacBackend::new(Executor::new(Config::default()))
}

pub(super) fn tree(path: &Path) -> Target {
    Target {
        path: path.to_owned(),
        scope: Scope::Tree,
    }
}

pub(super) fn entries(path: &Path, names: &[&str]) -> Target {
    Target {
        path: path.to_owned(),
        scope: Scope::Entries {
            names: names.iter().map(OsString::from).collect(),
        },
    }
}

/// Records every event delivered, and how many batches arrived after
/// [`close`](Self::close).
#[derive(Default)]
pub(super) struct Recorder {
    events: Mutex<Vec<RawEvent>>,
    changed: Condvar,
    closed: AtomicBool,
    late: AtomicUsize,
}

impl Recorder {
    pub(super) fn new() -> Arc<Recorder> {
        Arc::new(Recorder::default())
    }

    pub(super) fn deliver(self: &Arc<Self>) -> Deliver {
        let me = self.clone();
        Box::new(move |batch| {
            if me.closed.load(SeqCst) {
                me.late.fetch_add(1, SeqCst);
            }
            lock(&me.events).extend(batch.events);
            me.changed.notify_all();
        })
    }

    /// Call right after the watch's drop returned: any batch from now on
    /// counts as late.
    pub(super) fn close(&self) {
        self.closed.store(true, SeqCst);
    }

    pub(super) fn late(&self) -> usize {
        self.late.load(SeqCst)
    }

    pub(super) fn take(&self) -> Vec<RawEvent> {
        std::mem::take(&mut *lock(&self.events))
    }

    pub(super) fn snapshot(&self) -> Vec<RawEvent> {
        lock(&self.events).clone()
    }

    /// Waits until `done` holds for the events recorded so far, calling
    /// `poke` (a write, a flush) before each wait. Panics after
    /// [`DEADLINE`].
    pub(super) fn wait_until(
        &self,
        what: &str,
        mut poke: impl FnMut(),
        done: impl Fn(&[RawEvent]) -> bool,
    ) -> Vec<RawEvent> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            {
                let events = lock(&self.events);
                if done(&events) {
                    return events.clone();
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; got {:?}",
                self.snapshot()
            );
            poke();
            let events = lock(&self.events);
            if done(&events) {
                return events.clone();
            }
            drop(
                self.changed
                    .wait_timeout(events, Duration::from_millis(100))
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
        }
    }
}

/// The union of the flags reported for `path`; None when it was not
/// reported.
pub(super) fn flags_of(events: &[RawEvent], path: &str) -> Option<Flags> {
    events
        .iter()
        .filter(|event| &*event.path == path.as_bytes())
        .map(|event| event.flags)
        .reduce(|a, b| a | b)
}

pub(super) fn has(events: &[RawEvent], path: &str, flags: Flags) -> bool {
    flags_of(events, path).is_some_and(|got| got.contains(flags))
}

/// Writes probe files into `dir` until the watch reports one: FSEvents can
/// take a moment after `FSEventStreamStart` before it reports anything.
/// Probe paths start with `.probe-`; the recorder is cleared afterwards.
pub(super) fn wait_live(dir: &Path, armed: &dyn Armed, recorder: &Recorder) {
    let mut n = 0;
    recorder.wait_until(
        "the stream to go live",
        || {
            n += 1;
            fs::write(dir.join(format!(".probe-{n}")), b"probe").unwrap();
            armed.flush().unwrap();
        },
        |events| events.iter().any(|event| is_probe(&event.path)),
    );
    recorder.take();
}

pub(super) fn is_probe(path: &[u8]) -> bool {
    path.starts_with(b".probe-")
}
