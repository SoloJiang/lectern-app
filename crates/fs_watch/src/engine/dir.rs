//! A directory state: a recursive, unfiltered watch whose root may not exist
//! yet (docs/design/fs_watch.md「目录监听」).

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_kernel::BusyGuard;

use super::sub::{DirAcc, Sub};
use super::{Ctx, EntriesUser, Role};
use crate::backend::{BackendError, Flags, RawBatch, Scope, Target};
use crate::queue::{SubId, TargetId};
use crate::source::Sink;
use crate::types::{Coalesce, DirBatch, Failure, Health, RootState};

pub(crate) struct DirSub {
    pub(crate) sub: Sub<DirBatch>,
    acc: DirAcc,
}

enum Mode {
    /// Not armed yet, or failed.
    Idle,
    Rooted(TargetId),
    /// The root does not exist: `dir` is its nearest existing ancestor,
    /// watched for `name`, the next component towards the root.
    Waiting {
        dir: PathBuf,
    },
}

pub(crate) struct DirState {
    pub(crate) root: PathBuf,
    mode: Mode,
    failure: Option<Failure>,
    pub(crate) subs: Vec<DirSub>,
    told: Health,
}

impl DirState {
    pub(crate) fn new(root: PathBuf) -> DirState {
        DirState {
            root,
            mode: Mode::Idle,
            failure: None,
            subs: Vec::new(),
            told: Health::Live,
        }
    }

    pub(crate) fn add_sub(
        &mut self,
        id: SubId,
        sink: Arc<dyn Sink<DirBatch>>,
        coalesce: Coalesce,
        guard: Option<BusyGuard>,
    ) {
        self.subs.push(DirSub {
            sub: Sub::new(id, sink, coalesce, guard),
            acc: DirAcc::default(),
        });
    }

    pub(crate) fn remove_sub(&mut self, id: SubId) {
        self.subs.retain(|sub| sub.sub.id != id);
    }

    fn user(&self) -> EntriesUser {
        EntriesUser::Dir(self.root.clone())
    }

    /// Arms the root, or its nearest existing ancestor. True when the root
    /// is armed.
    pub(crate) fn locate(&mut self, ctx: &mut Ctx) -> bool {
        // Each round goes one level further down; the bound only guards
        // against a directory that keeps flapping.
        for _ in 0..64 {
            if is_dir(&self.root) {
                let target = Target {
                    path: self.root.clone(),
                    scope: Scope::Tree,
                };
                match ctx.arm(target, Role::Dir(self.root.clone())) {
                    Ok(id) => {
                        self.leave_ancestor(ctx);
                        self.mode = Mode::Rooted(id);
                        return true;
                    }
                    // Gone again: watch an ancestor.
                    Err(BackendError::NotFound) => {}
                    Err(error) => {
                        self.fail(Failure::Backend(error.to_string()));
                        return false;
                    }
                }
            }
            let Some((dir, name)) = nearest_existing(&self.root) else {
                self.fail(Failure::RootGone);
                return false;
            };
            if let Mode::Waiting { dir: current } = &self.mode
                && *current != dir
            {
                let current = current.clone();
                let _ = ctx.entries_set(&self.user(), &current, BTreeSet::new());
            }
            match ctx.entries_set(&self.user(), &dir, BTreeSet::from([name])) {
                Ok(()) => {
                    self.mode = Mode::Waiting { dir: dir.clone() };
                    // The next level may have appeared meanwhile.
                    if !is_dir(&self.root)
                        && nearest_existing(&self.root).is_some_and(|(now, _)| now == dir)
                    {
                        return false;
                    }
                }
                Err(message) => {
                    self.fail(Failure::Backend(message));
                    return false;
                }
            }
        }
        false
    }

    fn leave_ancestor(&mut self, ctx: &mut Ctx) {
        if let Mode::Waiting { dir } = std::mem::replace(&mut self.mode, Mode::Idle) {
            let _ = ctx.entries_set(&self.user(), &dir, BTreeSet::new());
        }
    }

    /// An event on the ancestor being watched: move down, or arm the root.
    pub(crate) fn on_ancestor_event(&mut self, ctx: &mut Ctx) {
        if self.failure.is_some() || !matches!(self.mode, Mode::Waiting { .. }) {
            return;
        }
        if self.locate(ctx) {
            let now = ctx.now();
            for sub in &mut self.subs {
                sub.acc.root = Some(RootState::Appeared);
                sub.acc.set_rescan();
                sub.sub.touch(now);
                deliver(sub);
            }
        }
        self.publish_health();
    }

    /// A raw batch from the root target.
    pub(crate) fn on_root_batch(&mut self, ctx: &mut Ctx, batch: RawBatch) {
        if self.failure.is_some() {
            return;
        }
        let now = ctx.now();
        let max = ctx.max_paths();
        for event in batch.events {
            if event.path.is_empty() {
                if event.flags.contains(Flags::UNMOUNTED) {
                    if let Mode::Rooted(id) = std::mem::replace(&mut self.mode, Mode::Idle) {
                        ctx.disarm(id);
                    }
                    self.fail(Failure::Unmounted);
                    return self.publish_health();
                }
                if event.flags.contains(Flags::ROOT_CHANGED) {
                    return self.vanished(ctx);
                }
            }
            if event.flags.contains(Flags::RESCAN) {
                for sub in &mut self.subs {
                    sub.acc.set_rescan();
                    sub.sub.touch(now);
                }
                continue;
            }
            if event.path.is_empty() {
                continue;
            }
            let path = self.root.join(OsStr::from_bytes(&event.path));
            for sub in &mut self.subs {
                sub.acc.add(path.clone(), max);
                sub.sub.touch(now);
            }
        }
    }

    /// The root was deleted or renamed: deliver `Vanished` and go back to
    /// watching an ancestor (or straight to the new root, if one is there).
    fn vanished(&mut self, ctx: &mut Ctx) {
        if let Mode::Rooted(id) = std::mem::replace(&mut self.mode, Mode::Idle) {
            ctx.disarm(id);
        }
        let now = ctx.now();
        for sub in &mut self.subs {
            sub.sub.touch(now);
            if sub.acc.root.is_none() {
                sub.acc.root = Some(RootState::Vanished);
            }
            deliver(sub);
        }
        if self.locate(ctx) {
            for sub in &mut self.subs {
                sub.acc.root = Some(RootState::Appeared);
                sub.acc.set_rescan();
                sub.sub.touch(now);
                deliver(sub);
            }
        }
        self.publish_health();
    }

    fn fail(&mut self, failure: Failure) {
        if self.failure.is_none() {
            self.failure = Some(failure);
        }
    }

    pub(crate) fn health(&self) -> Health {
        match &self.failure {
            Some(failure) => Health::Failed(failure.clone()),
            None => Health::Live,
        }
    }

    pub(crate) fn teardown(&mut self, ctx: &mut Ctx) {
        match std::mem::replace(&mut self.mode, Mode::Idle) {
            Mode::Rooted(id) => ctx.disarm(id),
            Mode::Waiting { dir } => {
                let _ = ctx.entries_set(&self.user(), &dir, BTreeSet::new());
            }
            Mode::Idle => {}
        }
        self.subs.clear();
    }

    pub(crate) fn publish_health(&mut self) {
        let health = self.health();
        if health != self.told {
            for sub in self.subs.iter_mut().filter(|sub| sub.sub.started()) {
                if sub.sub.is_open() {
                    deliver(sub);
                }
                let batch = DirAcc::default().take(Some(health.clone()), false);
                sub.sub.deliver(batch);
            }
            self.told = health.clone();
        }
        for sub in self.subs.iter_mut().filter(|sub| !sub.sub.started()) {
            let batch = sub.acc.take(Some(health.clone()), true);
            sub.sub.deliver(batch);
        }
    }

    pub(crate) fn fire(&mut self, ctx: &Ctx) -> usize {
        let now = ctx.now();
        let mut delivered = 0;
        for sub in self.subs.iter_mut().filter(|sub| sub.sub.started()) {
            if sub.sub.deadline().is_some_and(|deadline| deadline <= now) {
                deliver(sub);
                delivered += 1;
            }
        }
        delivered
    }

    pub(crate) fn next_deadline(&self) -> Option<std::time::Duration> {
        self.subs
            .iter()
            .filter(|sub| sub.sub.started())
            .filter_map(|sub| sub.sub.deadline())
            .min()
    }

    pub(crate) fn busy_sources(
        &self,
    ) -> impl Iterator<Item = (SubId, crate::backend::BusyFn)> + '_ {
        self.subs
            .iter()
            .filter(|sub| !sub.sub.is_dead())
            .map(|sub| (sub.sub.id, sub.sub.busy_fn()))
    }
}

fn is_dir(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

fn deliver(sub: &mut DirSub) {
    let first = !sub.sub.started();
    let health = first.then_some(Health::Live);
    let batch = sub.acc.take(health, first);
    sub.sub.deliver(batch);
}

/// The nearest existing ancestor directory of `root` and the name of the
/// next component towards it.
fn nearest_existing(root: &Path) -> Option<(PathBuf, OsString)> {
    let mut child = root;
    while let Some(parent) = child.parent() {
        if is_dir(parent) {
            let name = child.file_name()?.to_os_string();
            return Some((parent.to_path_buf(), name));
        }
        child = parent;
    }
    None
}
