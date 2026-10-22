//! Subscriptions: each coalesces on its own and delivers on its own
//! (docs/design/fs_watch.md「合并」).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lectern_kernel::{BusyGuard, contain};
use lectern_logging::{Value, error};

use crate::backend::BusyFn;
use crate::diag::Watched;
use crate::path::WatchPath;
use crate::queue::SubId;
use crate::reflog::{ReflogRecord, ReflogReset};
use crate::source::Sink;
use crate::types::{
    Coalesce, DirBatch, Health, MetaChanges, RepoBatch, Revision, RootState, WorktreeBatch,
    WorktreeMeta, WorktreeName,
};

struct Window {
    first: Duration,
    last: Duration,
    guard: Option<BusyGuard>,
}

/// A handle's sink with its coalescing window.
pub(crate) struct Sub<B: 'static> {
    pub(crate) id: SubId,
    sink: Arc<dyn Sink<B>>,
    coalesce: Coalesce,
    window: Option<Window>,
    /// Until the first batch is delivered: the guard `watch_*` took.
    initial: Option<Option<BusyGuard>>,
    /// The sink panicked; nothing more is delivered.
    dead: bool,
}

impl<B: 'static> Sub<B> {
    pub(crate) fn new(
        id: SubId,
        sink: Arc<dyn Sink<B>>,
        coalesce: Coalesce,
        guard: Option<BusyGuard>,
    ) -> Sub<B> {
        Sub {
            id,
            sink,
            coalesce,
            window: None,
            initial: Some(guard),
            dead: false,
        }
    }

    pub(crate) fn busy_fn(&self) -> BusyFn {
        let sink = self.sink.clone();
        Arc::new(move || sink.busy())
    }

    /// A guard from the sink, unless it panicked.
    pub(crate) fn busy(&self) -> Option<BusyGuard> {
        if self.dead { None } else { self.sink.busy() }
    }

    pub(crate) fn is_dead(&self) -> bool {
        self.dead
    }

    /// Whether the first batch (with `health` and `rescan`) was delivered.
    pub(crate) fn started(&self) -> bool {
        self.initial.is_none()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.window.is_some()
    }

    /// A change to deliver arrived at `now`: opens the window (taking a
    /// guard) or extends it.
    pub(crate) fn touch(&mut self, now: Duration) {
        if self.dead {
            return;
        }
        match &mut self.window {
            Some(window) => window.last = now,
            None => {
                let guard = self.sink.busy();
                self.window = Some(Window {
                    first: now,
                    last: now,
                    guard,
                });
            }
        }
    }

    /// When the open window ends.
    pub(crate) fn deadline(&self) -> Option<Duration> {
        let window = self.window.as_ref()?;
        Some((window.last + self.coalesce.quiet).min(window.first + self.coalesce.max_delay))
    }
}

impl<B: Watched + 'static> Sub<B> {
    /// Delivers `batch` under `contain`, with the window's guard (or the
    /// initial one for the first batch), and closes the window. `root` is
    /// the watch's root, for the record of a panicking sink.
    pub(crate) fn deliver(&mut self, batch: B, root: &Path) {
        let window = self.window.take();
        if self.dead {
            return;
        }
        let window_guard = window.and_then(|window| window.guard);
        let guard = match self.initial.take() {
            Some(Some(initial)) => {
                drop(window_guard);
                Some(initial)
            }
            Some(None) | None => window_guard,
        };
        let sink = self.sink.clone();
        if contain(move || sink.deliver(batch, guard)).is_err() {
            // The message may quote the batch's paths or the consumer's
            // data: the panic hook records its location and length, this
            // record only which watch stops delivering.
            error!(
                "a sink panicked; it is no longer delivered to";
                watch = B::KIND,
                root = Value::path(root),
            );
            self.dead = true;
        }
    }
}

/// What a repository subscription has gathered for its next batch.
#[derive(Default)]
pub(crate) struct RepoAcc {
    pub(crate) worktrees: BTreeMap<WorktreeName, WorktreeMeta>,
    pub(crate) worktree_list: bool,
    pub(crate) refs: bool,
    pub(crate) config: bool,
    pub(crate) reflog: Vec<ReflogRecord>,
    pub(crate) resets: Vec<ReflogReset>,
    /// Line bytes in `reflog`.
    pub(crate) reflog_bytes: usize,
    pub(crate) rescan: bool,
}

impl RepoAcc {
    pub(crate) fn take(&mut self, health: Option<Health>, first: bool) -> RepoBatch {
        let acc = std::mem::take(self);
        RepoBatch {
            worktrees: acc.worktrees.into_iter().collect(),
            worktree_list: acc.worktree_list,
            refs: acc.refs,
            config: acc.config,
            reflog: acc.reflog,
            reflog_resets: acc.resets,
            rescan: acc.rescan || first,
            health,
        }
    }
}

/// What a worktree subscription has gathered for its next batch.
#[derive(Default)]
pub(crate) struct WorktreeAcc {
    pub(crate) paths: BTreeSet<WatchPath>,
    pub(crate) meta: MetaChanges,
    pub(crate) rescan: bool,
}

impl WorktreeAcc {
    pub(crate) fn add(&mut self, path: WatchPath, max: usize) {
        if self.rescan {
            return;
        }
        self.paths.insert(path);
        if self.paths.len() > max {
            self.paths.clear();
            self.rescan = true;
        }
    }

    pub(crate) fn set_rescan(&mut self) {
        self.paths.clear();
        self.rescan = true;
    }

    pub(crate) fn take(
        &mut self,
        revision: Revision,
        health: Option<Health>,
        first: bool,
    ) -> WorktreeBatch {
        let acc = std::mem::take(self);
        let rescan = acc.rescan || first;
        WorktreeBatch {
            revision,
            paths: if rescan {
                Vec::new()
            } else {
                acc.paths.into_iter().collect()
            },
            meta: acc.meta,
            rescan,
            health,
        }
    }
}

/// What a directory subscription has gathered for its next batch.
#[derive(Default)]
pub(crate) struct DirAcc {
    pub(crate) paths: BTreeSet<PathBuf>,
    pub(crate) rescan: bool,
    pub(crate) root: Option<RootState>,
}

impl DirAcc {
    pub(crate) fn add(&mut self, path: PathBuf, max: usize) {
        if self.rescan {
            return;
        }
        self.paths.insert(path);
        if self.paths.len() > max {
            self.set_rescan();
        }
    }

    pub(crate) fn set_rescan(&mut self) {
        self.paths.clear();
        self.rescan = true;
    }

    pub(crate) fn take(&mut self, health: Option<Health>, first: bool) -> DirBatch {
        let acc = std::mem::take(self);
        let rescan = acc.rescan || first;
        let mut paths: Vec<PathBuf> = if rescan {
            Vec::new()
        } else {
            acc.paths.into_iter().collect()
        };
        paths.sort_by(|a, b| {
            a.as_os_str()
                .as_encoded_bytes()
                .cmp(b.as_os_str().as_encoded_bytes())
        });
        DirBatch {
            paths,
            rescan,
            root: acc.root,
            health,
        }
    }
}
