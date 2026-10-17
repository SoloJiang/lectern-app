//! A repository state: the common directory target, git metadata, reflog
//! progress and the unfiltered list (docs/design/fs_watch.md「git 元数据」
//! 「reflog」).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use lectern_kernel::BusyGuard;

use super::config::ConfigPlan;
use super::sub::{RepoAcc, Sub};
use super::worktree::WorktreeState;
use super::{Ctx, Role};
use crate::backend::{BackendError, Flags, RawBatch, Scope, Target};
use crate::path::{WatchPath, file_name, is_within};
use crate::queue::{SubId, TargetId};
use crate::reflog::{READ_LIMIT, ReadOutcome, Reflogs, is_reflog_location};
use crate::source::Sink;
use crate::types::{Coalesce, Failure, Health, MetaChanges, RepoBatch, WorktreeName};

pub(crate) struct RepoSub {
    pub(crate) sub: Sub<RepoBatch>,
    acc: RepoAcc,
    unfiltered: Vec<WatchPath>,
}

pub(crate) struct RepoState {
    pub(crate) common: PathBuf,
    pub(crate) target: Option<TargetId>,
    failure: Option<Failure>,
    reflogs: Option<Reflogs>,
    pub(crate) subs: Vec<RepoSub>,
    /// Watched worktrees, by epoch.
    pub(crate) worktrees: BTreeMap<u64, WorktreeState>,
    /// The union of the subscriptions' unfiltered lists, sorted.
    pub(crate) unfiltered: Vec<WatchPath>,
    told: Health,
}

/// What a path in the common directory means (the table in「git 元数据」).
enum Meta {
    Head(WorktreeName),
    Index(WorktreeName),
    ConfigWorktree(WorktreeName),
    WorktreeList,
    Refs,
    Config,
    InfoExclude,
    InfoAttributes,
    Other,
}

fn meta_of(q: &[u8]) -> Meta {
    match q {
        b"HEAD" => return Meta::Head(WorktreeName::Main),
        b"index" => return Meta::Index(WorktreeName::Main),
        b"config.worktree" => return Meta::ConfigWorktree(WorktreeName::Main),
        b"config" => return Meta::Config,
        b"info/exclude" => return Meta::InfoExclude,
        b"info/attributes" => return Meta::InfoAttributes,
        b"packed-refs" => return Meta::Refs,
        b"worktrees" => return Meta::WorktreeList,
        _ => {}
    }
    if is_within(q, b"refs") || is_within(q, b"reftable") {
        return Meta::Refs;
    }
    let parts: Vec<&[u8]> = q.split(|&b| b == b'/').collect();
    match parts.as_slice() {
        [b"worktrees", _] => Meta::WorktreeList,
        [b"worktrees", name, file] => {
            let name = WorktreeName::Linked(String::from_utf8_lossy(name).into());
            match *file {
                b"HEAD" => Meta::Head(name),
                b"index" => Meta::Index(name),
                b"config.worktree" => Meta::ConfigWorktree(name),
                b"gitdir" | b"locked" | b"commondir" => Meta::WorktreeList,
                _ => Meta::Other,
            }
        }
        _ => Meta::Other,
    }
}

impl RepoState {
    pub(crate) fn new(common: PathBuf) -> RepoState {
        RepoState {
            common,
            target: None,
            failure: None,
            reflogs: None,
            subs: Vec::new(),
            worktrees: BTreeMap::new(),
            unfiltered: Vec::new(),
            told: Health::Live,
        }
    }

    pub(crate) fn add_sub(
        &mut self,
        id: SubId,
        sink: Arc<dyn Sink<RepoBatch>>,
        coalesce: Coalesce,
        guard: Option<BusyGuard>,
    ) {
        self.subs.push(RepoSub {
            sub: Sub::new(id, sink, coalesce, guard),
            acc: RepoAcc::default(),
            unfiltered: Vec::new(),
        });
    }

    pub(crate) fn remove_sub(&mut self, id: SubId) {
        self.subs.retain(|sub| sub.sub.id != id);
        self.update_unfiltered();
    }

    pub(crate) fn set_unfiltered(&mut self, id: SubId, mut paths: Vec<WatchPath>) -> bool {
        let Some(sub) = self.subs.iter_mut().find(|sub| sub.sub.id == id) else {
            return false;
        };
        paths.sort();
        paths.dedup();
        sub.unfiltered = paths;
        self.update_unfiltered();
        true
    }

    fn update_unfiltered(&mut self) {
        let union: BTreeSet<WatchPath> = self
            .subs
            .iter()
            .flat_map(|sub| sub.unfiltered.iter().cloned())
            .collect();
        self.unfiltered = union.into_iter().collect();
    }

    /// Phase 1 of the reflog progress, arm, then phase 2.
    pub(crate) fn start(&mut self, ctx: &mut Ctx) {
        self.reflogs = Some(Reflogs::baseline(&self.common, &mut ctx.stamps, &ctx.clock));
        let target = Target {
            path: self.common.clone(),
            scope: Scope::Tree,
        };
        match ctx.arm(target, Role::Common(self.common.clone())) {
            Ok(id) => self.target = Some(id),
            Err(BackendError::NotFound) => return self.fail(ctx, Failure::RootGone),
            Err(error) => return self.fail(ctx, Failure::Backend(error.to_string())),
        }
        // Lines appended between phase 1 and arm.
        self.read_reflogs(ctx, &[Vec::new()]);
    }

    pub(crate) fn health(&self) -> Health {
        match &self.failure {
            Some(failure) => Health::Failed(failure.clone()),
            None => Health::Live,
        }
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.failure.is_some()
    }

    /// A raw batch from the common directory target.
    pub(crate) fn on_batch(&mut self, ctx: &mut Ctx, batch: RawBatch) {
        if self.failure.is_some() {
            return;
        }
        let now = ctx.now();
        let mut reflog_paths: Vec<Vec<u8>> = Vec::new();
        for event in batch.events {
            let q = &*event.path;
            if q.is_empty() {
                if event.flags.contains(Flags::UNMOUNTED) {
                    return self.fail(ctx, Failure::Unmounted);
                }
                if event.flags.contains(Flags::ROOT_CHANGED) {
                    return self.fail(ctx, Failure::RootGone);
                }
            }
            if event.flags.contains(Flags::RESCAN) {
                self.rescan(ctx);
                continue;
            }
            if q.is_empty() || file_name(q).ends_with(b".lock") {
                continue;
            }
            // Config list points under the common directory.
            let common = self.common.clone();
            for worktree in self.worktrees.values_mut() {
                if ConfigPlan::matches(&worktree.plan.common, q) {
                    worktree.config_changed(ctx, &common);
                }
            }
            if is_reflog_location(q) {
                reflog_paths.push(q.to_vec());
            }
            let common = self.common.clone();
            match meta_of(q) {
                Meta::Head(name) => {
                    self.repo(now, |acc| {
                        acc.worktrees.entry(name.clone()).or_default().head = true
                    });
                    for worktree in self.named(&name) {
                        worktree.meta(ctx, |meta| meta.head = true);
                        // `includeIf "onbranch:…"` depends on HEAD.
                        worktree.reload_config(ctx, &common);
                    }
                }
                Meta::Index(name) => {
                    self.repo(now, |acc| {
                        acc.worktrees.entry(name.clone()).or_default().index = true
                    });
                    for worktree in self.named(&name) {
                        worktree.meta(ctx, |meta| meta.index = true);
                        worktree.reload_filter(ctx, BTreeSet::new());
                    }
                }
                Meta::ConfigWorktree(name) => {
                    self.repo(now, |acc| acc.config = true);
                    for worktree in self.named(&name) {
                        worktree.config_changed(ctx, &common);
                    }
                }
                Meta::WorktreeList => self.repo(now, |acc| acc.worktree_list = true),
                Meta::Refs => {
                    self.repo(now, |acc| acc.refs = true);
                    self.all(ctx, |meta| meta.refs = true);
                }
                Meta::Config => {
                    self.repo(now, |acc| acc.config = true);
                    for worktree in self.worktrees.values_mut() {
                        worktree.config_changed(ctx, &common);
                    }
                }
                Meta::InfoExclude => {
                    self.repo(now, |acc| acc.config = true);
                    self.all(ctx, |meta| meta.config = true);
                    for worktree in self.worktrees.values_mut() {
                        worktree.reload_filter(ctx, BTreeSet::new());
                    }
                }
                Meta::InfoAttributes => {
                    self.repo(now, |acc| acc.config = true);
                    self.all(ctx, |meta| meta.config = true);
                }
                Meta::Other => {}
            }
        }
        if !reflog_paths.is_empty() {
            self.read_reflogs(ctx, &reflog_paths);
        }
    }

    /// The worktree states named `name` (one, or two while a state is being
    /// replaced).
    fn named(&mut self, name: &WorktreeName) -> impl Iterator<Item = &mut WorktreeState> {
        self.worktrees
            .values_mut()
            .filter(move |worktree| worktree.name == *name)
    }

    fn all(&mut self, ctx: &Ctx, set: impl Fn(&mut MetaChanges)) {
        for worktree in self.worktrees.values_mut() {
            worktree.meta(ctx, &set);
        }
    }

    /// Records a change for every repository subscription.
    fn repo(&mut self, now: std::time::Duration, set: impl Fn(&mut RepoAcc)) {
        for sub in &mut self.subs {
            set(&mut sub.acc);
            sub.sub.touch(now);
        }
    }

    /// `RESCAN` on the common directory.
    pub(crate) fn rescan(&mut self, ctx: &mut Ctx) {
        let now = ctx.now();
        self.repo(now, |acc| acc.rescan = true);
        let common = self.common.clone();
        for worktree in self.worktrees.values_mut() {
            worktree.meta(ctx, |meta| {
                *meta = MetaChanges {
                    head: true,
                    index: true,
                    refs: true,
                    config: true,
                    rules: true,
                }
            });
            worktree.reload_filter(ctx, BTreeSet::new());
            worktree.reload_config(ctx, &common);
        }
        self.read_reflogs(ctx, &[Vec::new()]);
    }

    /// Reads the reflog files events on `paths` may have touched, right
    /// away, one after another.
    fn read_reflogs(&mut self, ctx: &mut Ctx, paths: &[Vec<u8>]) {
        let Some(reflogs) = &mut self.reflogs else {
            return;
        };
        let mut files = BTreeSet::new();
        for q in paths {
            files.extend(reflogs.affected(&self.common, q));
        }
        for rel in files {
            loop {
                let outcome =
                    reflogs.read(&self.common, &rel, READ_LIMIT, &mut ctx.stamps, &ctx.clock);
                let now = ctx.now();
                match outcome {
                    None => break,
                    Some(ReadOutcome::Reset(reset)) => {
                        for sub in &mut self.subs {
                            sub.acc.resets.push(reset.clone());
                            sub.sub.touch(now);
                        }
                        break;
                    }
                    Some(ReadOutcome::Records {
                        records,
                        bytes,
                        more,
                    }) => {
                        if !records.is_empty() {
                            for sub in &mut self.subs {
                                // A batch carries at most READ_LIMIT bytes of lines.
                                if sub.acc.reflog_bytes + bytes > READ_LIMIT {
                                    deliver(sub, None);
                                }
                                sub.acc.reflog.extend(records.iter().cloned());
                                sub.acc.reflog_bytes += bytes;
                                sub.sub.touch(now);
                            }
                        }
                        if !more {
                            break;
                        }
                        // The read hit the limit: this batch goes now, the
                        // rest is read into the next one.
                        for sub in &mut self.subs {
                            deliver(sub, None);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn fail(&mut self, ctx: &mut Ctx, failure: Failure) {
        if self.failure.is_some() {
            return;
        }
        self.failure = Some(failure.clone());
        if let Some(id) = self.target.take() {
            ctx.disarm(id);
        }
        let common = self.common.clone();
        for worktree in self.worktrees.values_mut() {
            worktree.fail(ctx, &common, failure.clone());
        }
        self.publish_health();
    }

    pub(crate) fn teardown(&mut self, ctx: &mut Ctx) {
        if let Some(id) = self.target.take() {
            ctx.disarm(id);
        }
        let common = self.common.clone();
        for worktree in self.worktrees.values_mut() {
            worktree.teardown(ctx, &common);
        }
        self.worktrees.clear();
        self.subs.clear();
    }

    pub(crate) fn publish_health(&mut self) {
        let health = self.health();
        if health != self.told {
            for sub in self.subs.iter_mut().filter(|sub| sub.sub.started()) {
                if sub.sub.is_open() {
                    deliver(sub, None);
                }
                let batch = RepoAcc::default().take(Some(health.clone()), false);
                sub.sub.deliver(batch);
            }
            self.told = health;
        }
        self.start_subs();
    }

    /// First batches: the repository state is live as soon as it is armed.
    pub(crate) fn start_subs(&mut self) {
        let health = self.health();
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
                deliver(sub, None);
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

/// Delivers a subscription's batch; its first batch carries the health and
/// `rescan`.
fn deliver(sub: &mut RepoSub, health: Option<Health>) {
    let first = !sub.sub.started();
    let health = if first {
        Some(health.unwrap_or(Health::Live))
    } else {
        health
    };
    let batch = sub.acc.take(health, first);
    sub.sub.deliver(batch);
}
