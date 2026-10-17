//! The bounded queue in front of the fs_watch thread: raw batches from the
//! backend, commands from handles and load results from the executor.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use lectern_kernel::BusyGuard;

use crate::backend::RawBatch;
use crate::engine::config::ConfigPlan;
use crate::path::WatchPath;
use crate::source::{PathFilter, Sink, SourceError, WorktreeSource};
use crate::types::{Coalesce, DirBatch, RepoBatch, WorktreeBatch, WorktreeName};
use crate::util::{lock, wait, wait_timeout};
use crate::watcher::WorktreeShared;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub(crate) struct TargetId(pub(crate) u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub(crate) struct SubId(pub(crate) u64);

pub(crate) enum Item {
    Raw {
        target: TargetId,
        batch: RawBatch,
        guard: Option<BusyGuard>,
    },
    /// A raw batch for this target was dropped because the queue was full.
    Overflow(TargetId),
    Cmd(Cmd),
}

pub(crate) enum Cmd {
    WatchRepo {
        sub: SubId,
        common: PathBuf,
        coalesce: Coalesce,
        sink: Arc<dyn Sink<RepoBatch>>,
        guard: Option<BusyGuard>,
    },
    WatchWorktree {
        sub: SubId,
        common: PathBuf,
        shared: Arc<WorktreeShared>,
        name: WorktreeName,
        root: PathBuf,
        source: Arc<dyn WorktreeSource>,
        coalesce: Coalesce,
        sink: Arc<dyn Sink<WorktreeBatch>>,
        guard: Option<BusyGuard>,
    },
    WatchDir {
        sub: SubId,
        root: PathBuf,
        coalesce: Coalesce,
        sink: Arc<dyn Sink<DirBatch>>,
        guard: Option<BusyGuard>,
    },
    Unsubscribe(SubId),
    SetUnfiltered {
        sub: SubId,
        paths: Vec<WatchPath>,
    },
    FilterLoaded {
        epoch: u64,
        load: u64,
        result: Result<Arc<dyn PathFilter>, SourceError>,
    },
    ConfigLoaded {
        epoch: u64,
        load: u64,
        result: Result<ConfigPlan, SourceError>,
    },
    /// `flush_revision`'s barrier: processed after everything queued before.
    Fence(u64),
}

#[derive(Default)]
struct State {
    items: VecDeque<Item>,
    /// Raw events queued.
    events: usize,
    /// Targets with an `Overflow` marker in `items`.
    overflowed: HashSet<TargetId>,
    stopped: bool,
}

pub(crate) struct Queue {
    capacity: usize,
    state: Mutex<State>,
    ready: Condvar,
}

/// What `Queue::wait` returned.
pub(crate) enum Wait {
    Items(Vec<Item>),
    Timeout,
    Stopped,
}

impl Queue {
    pub(crate) fn new(capacity: usize) -> Queue {
        Queue {
            capacity,
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
        }
    }

    /// Queues a raw batch. Returns the guard (to drop outside the lock) when
    /// the batch was refused: queue stopped, or full (the target is then
    /// marked as overflowed).
    pub(crate) fn push_raw(
        &self,
        target: TargetId,
        batch: RawBatch,
        guard: Option<BusyGuard>,
    ) -> Option<(RawBatch, Option<BusyGuard>)> {
        let mut state = lock(&self.state);
        if state.stopped {
            return Some((batch, guard));
        }
        let n = batch.events.len();
        if state.events + n > self.capacity {
            if state.overflowed.insert(target) {
                state.items.push_back(Item::Overflow(target));
                self.ready.notify_all();
            }
            return Some((batch, guard));
        }
        state.events += n;
        state.items.push_back(Item::Raw {
            target,
            batch,
            guard,
        });
        self.ready.notify_all();
        None
    }

    /// Queues a command. Returns it when the queue is stopped.
    pub(crate) fn push_cmd(&self, cmd: Cmd) -> Option<Cmd> {
        let mut state = lock(&self.state);
        if state.stopped {
            return Some(cmd);
        }
        state.items.push_back(Item::Cmd(cmd));
        self.ready.notify_all();
        None
    }

    /// Takes everything queued, without waiting.
    pub(crate) fn drain(&self) -> Vec<Item> {
        let mut state = lock(&self.state);
        take(&mut state)
    }

    /// Waits for items, up to `timeout` (forever with None).
    pub(crate) fn wait(&self, timeout: Option<Duration>) -> Wait {
        let mut state = lock(&self.state);
        let deadline = timeout.map(|timeout| std::time::Instant::now() + timeout);
        loop {
            if state.stopped {
                return Wait::Stopped;
            }
            if !state.items.is_empty() {
                return Wait::Items(take(&mut state));
            }
            match deadline {
                None => state = wait(&self.ready, state),
                Some(deadline) => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Wait::Timeout;
                    }
                    state = wait_timeout(&self.ready, state, deadline - now);
                }
            }
        }
    }

    /// Stops the queue and returns what was still in it, to drop outside the
    /// lock.
    pub(crate) fn stop(&self) -> Vec<Item> {
        let mut state = lock(&self.state);
        state.stopped = true;
        self.ready.notify_all();
        take(&mut state)
    }
}

fn take(state: &mut State) -> Vec<Item> {
    state.events = 0;
    state.overflowed.clear();
    state.items.drain(..).collect()
}
