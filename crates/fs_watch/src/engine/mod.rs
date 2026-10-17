//! The fs_watch thread's state: targets, repository, worktree and directory
//! states, and the routing of raw batches and commands between them.
//!
//! Everything here runs on one thread at a time (the fs_watch thread, or
//! the caller of `testing::Driver` and of manual `flush_revision`), under
//! `Shared`'s engine lock.

pub(crate) mod cache;
pub(crate) mod config;
mod dir;
mod repo;
mod stash;
mod sub;
mod worktree;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Weak};
use std::time::Duration;

use lectern_executor::Executor;

use crate::backend::{
    Armed, Backend, BackendError, BusyFn, EventSink, Flags, RawBatch, RawEvent, Scope, Target,
    TargetShared,
};
use crate::clock::Clock;
use crate::queue::{Cmd, Item, Queue, SubId, TargetId};
use crate::reflog::{RunId, Stamps};
use crate::types::Failure;
use crate::watcher::{Config, Fences};

use dir::DirState;
use repo::RepoState;
use worktree::WorktreeState;

/// What a target's events are for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Role {
    /// A repository's common directory.
    Common(PathBuf),
    /// A worktree root.
    Root { common: PathBuf, epoch: u64 },
    /// A shared `Entries` target: config list points and directory watches
    /// waiting for their root.
    Entries(PathBuf),
    /// A directory watch's root.
    Dir(PathBuf),
}

/// Who uses a shared `Entries` target.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum EntriesUser {
    Config { common: PathBuf, epoch: u64 },
    Dir(PathBuf),
}

struct TargetEntry {
    armed: Arc<dyn Armed>,
    shared: Arc<TargetShared>,
    role: Role,
}

#[derive(Default)]
struct EntriesReg {
    id: Option<TargetId>,
    armed: BTreeSet<OsString>,
    users: BTreeMap<EntriesUser, BTreeSet<OsString>>,
}

/// Resources shared by the states.
pub(crate) struct Ctx {
    backend: Arc<dyn Backend>,
    pub(crate) executor: Executor,
    pub(crate) queue: Arc<Queue>,
    pub(crate) clock: Clock,
    config: Config,
    pub(crate) stamps: Stamps,
    next_target: u64,
    next_load: u64,
    targets: HashMap<TargetId, TargetEntry>,
    entries: BTreeMap<PathBuf, EntriesReg>,
    /// Targets changed since the busy lists were last refreshed.
    targets_dirty: bool,
}

impl Ctx {
    pub(crate) fn now(&self) -> Duration {
        self.clock.now()
    }

    pub(crate) fn max_paths(&self) -> usize {
        self.config.max_batch_paths
    }

    pub(crate) fn next_load(&mut self) -> u64 {
        self.next_load += 1;
        self.next_load
    }

    pub(crate) fn arm(&mut self, target: Target, role: Role) -> Result<TargetId, BackendError> {
        self.next_target += 1;
        let id = TargetId(self.next_target);
        let shared = Arc::new(TargetShared::default());
        let sink = EventSink {
            target: id,
            shared: shared.clone(),
            queue: self.queue.clone(),
        };
        let armed = self.backend.arm(&target, sink)?;
        self.targets.insert(
            id,
            TargetEntry {
                armed,
                shared,
                role,
            },
        );
        self.targets_dirty = true;
        Ok(id)
    }

    /// Drops a target's backend watch, on this thread.
    pub(crate) fn disarm(&mut self, id: TargetId) {
        if let Some(entry) = self.targets.remove(&id) {
            self.targets_dirty = true;
            drop(entry);
        }
    }

    pub(crate) fn armed(&self, id: TargetId) -> Option<Weak<dyn Armed>> {
        self.targets
            .get(&id)
            .map(|entry| Arc::downgrade(&entry.armed))
    }

    pub(crate) fn entries_target(&self, dir: &Path) -> Option<TargetId> {
        self.entries.get(dir).and_then(|reg| reg.id)
    }

    /// The `Entries` targets `user` takes part in.
    pub(crate) fn entries_targets_of(&self, user: &EntriesUser) -> Vec<TargetId> {
        self.entries
            .values()
            .filter(|reg| reg.users.contains_key(user))
            .filter_map(|reg| reg.id)
            .collect()
    }

    /// Sets the names `user` watches in `dir` (none: leaves). The target is
    /// armed again with the union of the users' names when that changes; a
    /// directory several users share is armed once.
    pub(crate) fn entries_set(
        &mut self,
        user: &EntriesUser,
        dir: &Path,
        names: BTreeSet<OsString>,
    ) -> Result<(), String> {
        let reg = self.entries.entry(dir.to_path_buf()).or_default();
        if names.is_empty() {
            reg.users.remove(user);
        } else {
            reg.users.insert(user.clone(), names);
        }
        let union: BTreeSet<OsString> = reg.users.values().flatten().cloned().collect();
        if union.is_empty() {
            let id = reg.id.take();
            self.entries.remove(dir);
            if let Some(id) = id {
                self.disarm(id);
            }
            return Ok(());
        }
        if reg.id.is_some() && reg.armed == union {
            return Ok(());
        }
        let target = Target {
            path: dir.to_path_buf(),
            scope: Scope::Entries {
                names: union.iter().cloned().collect(),
            },
        };
        let armed = self.arm(target, Role::Entries(dir.to_path_buf()));
        let reg = self.entries.get_mut(dir).expect("inserted above");
        match armed {
            Ok(id) => {
                reg.armed = union;
                // Armed before the old target is dropped: no gap.
                if let Some(old) = reg.id.replace(id) {
                    self.disarm(old);
                }
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// The users of an `Entries` target an event named `name` concerns
    /// (every user for the directory itself or a rescan).
    fn entries_users(&self, dir: &Path, name: &[u8], all: bool) -> Vec<EntriesUser> {
        let Some(reg) = self.entries.get(dir) else {
            return Vec::new();
        };
        reg.users
            .iter()
            .filter(|(_, names)| {
                all || name.is_empty()
                    || names
                        .iter()
                        .any(|candidate| candidate.as_encoded_bytes() == name)
            })
            .map(|(user, _)| user.clone())
            .collect()
    }
}

/// Where a subscription lives.
#[derive(Clone)]
enum SubLoc {
    Repo(PathBuf),
    Worktree(PathBuf, u64),
    Dir(PathBuf),
}

pub(crate) struct Engine {
    ctx: Ctx,
    fences: Arc<Fences>,
    stopping: Arc<AtomicBool>,
    repos: BTreeMap<PathBuf, RepoState>,
    dirs: BTreeMap<PathBuf, DirState>,
    subs: HashMap<SubId, SubLoc>,
    down: bool,
}

impl Engine {
    #[allow(clippy::too_many_arguments, reason = "built once, by Watcher")]
    pub(crate) fn new(
        backend: Arc<dyn Backend>,
        executor: Executor,
        queue: Arc<Queue>,
        fences: Arc<Fences>,
        stopping: Arc<AtomicBool>,
        clock: Clock,
        config: Config,
        run: RunId,
    ) -> Engine {
        Engine {
            ctx: Ctx {
                backend,
                executor,
                queue,
                clock,
                config,
                stamps: Stamps::new(run),
                next_target: 0,
                next_load: 0,
                targets: HashMap::new(),
                entries: BTreeMap::new(),
                targets_dirty: false,
            },
            fences,
            stopping,
            repos: BTreeMap::new(),
            dirs: BTreeMap::new(),
            subs: HashMap::new(),
            down: false,
        }
    }

    pub(crate) fn now(&self) -> Duration {
        self.ctx.now()
    }

    pub(crate) fn handle(&mut self, item: Item) {
        if self.down {
            return;
        }
        match item {
            Item::Raw {
                target,
                batch,
                guard,
            } => {
                self.on_raw(target, batch);
                // The subscriptions took their own guards.
                drop(guard);
            }
            Item::Overflow(target) => {
                let batch = RawBatch {
                    events: vec![RawEvent {
                        path: Box::default(),
                        flags: Flags::RESCAN | Flags::DROPPED,
                    }],
                };
                self.on_raw(target, batch);
            }
            Item::Cmd(cmd) => self.on_cmd(cmd),
        }
        self.end_item();
    }

    /// Applies each worktree's pending revision increment and refreshes the
    /// busy lists if targets changed.
    fn end_item(&mut self) {
        for repo in self.repos.values_mut() {
            for worktree in repo.worktrees.values_mut() {
                worktree.end_item();
            }
        }
        if std::mem::take(&mut self.ctx.targets_dirty) {
            self.refresh_busy();
        }
    }

    fn on_raw(&mut self, target: TargetId, batch: RawBatch) {
        let Some(role) = self
            .ctx
            .targets
            .get(&target)
            .map(|entry| entry.role.clone())
        else {
            // Dropped since the backend sent it.
            return;
        };
        let ctx = &mut self.ctx;
        match role {
            Role::Common(common) => {
                if let Some(repo) = self.repos.get_mut(&common) {
                    repo.on_batch(ctx, batch);
                }
            }
            Role::Root { common, epoch } => {
                if let Some(repo) = self.repos.get_mut(&common) {
                    let unfiltered = std::mem::take(&mut repo.unfiltered);
                    if let Some(worktree) = repo.worktrees.get_mut(&epoch) {
                        worktree.on_root_batch(ctx, &common, &unfiltered, batch);
                    }
                    repo.unfiltered = unfiltered;
                }
            }
            Role::Dir(root) => {
                if let Some(dir) = self.dirs.get_mut(&root) {
                    dir.on_root_batch(ctx, batch);
                }
            }
            Role::Entries(path) => {
                let mut users = BTreeSet::new();
                for event in &batch.events {
                    let all = event.flags.any(Flags::RESCAN | Flags::ROOT_CHANGED);
                    users.extend(ctx.entries_users(&path, &event.path, all));
                }
                for user in users {
                    match user {
                        EntriesUser::Config { common, epoch } => {
                            if let Some(worktree) = self
                                .repos
                                .get_mut(&common)
                                .and_then(|repo| repo.worktrees.get_mut(&epoch))
                            {
                                worktree.config_changed(ctx, &common);
                            }
                        }
                        EntriesUser::Dir(root) => {
                            if let Some(dir) = self.dirs.get_mut(&root) {
                                dir.on_ancestor_event(ctx);
                            }
                        }
                    }
                }
            }
        }
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::WatchRepo {
                sub,
                common,
                coalesce,
                sink,
                guard,
            } => {
                self.subs.insert(sub, SubLoc::Repo(common.clone()));
                let ctx = &mut self.ctx;
                let repo = self
                    .repos
                    .entry(common.clone())
                    .or_insert_with(|| RepoState::new(common));
                repo.add_sub(sub, sink, coalesce, guard);
                if repo.target.is_none() && !repo.is_failed() {
                    repo.start(ctx);
                }
                repo.start_subs();
            }
            Cmd::WatchWorktree {
                sub,
                common,
                shared,
                name,
                root,
                source,
                coalesce,
                sink,
                guard,
            } => {
                let ctx = &mut self.ctx;
                let Some(repo) = self.repos.get_mut(&common) else {
                    shared.fail(Failure::Stopped);
                    return;
                };
                let epoch = shared.epoch;
                self.subs
                    .insert(sub, SubLoc::Worktree(common.clone(), epoch));
                let repo_failure = repo.is_failed().then(|| repo.health());
                let common_target = repo.target;
                let created = !repo.worktrees.contains_key(&epoch);
                let worktree = repo
                    .worktrees
                    .entry(epoch)
                    .or_insert_with(|| WorktreeState::new(shared, name, root, source));
                worktree.add_sub(sub, sink, coalesce, guard);
                if created {
                    match repo_failure {
                        Some(crate::types::Health::Failed(failure)) => {
                            worktree.fail(ctx, &common, failure);
                        }
                        _ => worktree.start(ctx, &common),
                    }
                }
                worktree.publish_armed(ctx, &common, common_target);
                worktree.publish_health();
            }
            Cmd::WatchDir {
                sub,
                root,
                coalesce,
                sink,
                guard,
            } => {
                self.subs.insert(sub, SubLoc::Dir(root.clone()));
                let ctx = &mut self.ctx;
                let created = !self.dirs.contains_key(&root);
                let dir = self
                    .dirs
                    .entry(root.clone())
                    .or_insert_with(|| DirState::new(root));
                dir.add_sub(sub, sink, coalesce, guard);
                if created {
                    dir.locate(ctx);
                }
                dir.publish_health();
            }
            Cmd::Unsubscribe(sub) => self.unsubscribe(sub),
            Cmd::SetUnfiltered { sub, paths } => {
                if let Some(SubLoc::Repo(common)) = self.subs.get(&sub)
                    && let Some(repo) = self.repos.get_mut(common)
                {
                    repo.set_unfiltered(sub, paths);
                }
            }
            Cmd::FilterLoaded {
                epoch,
                load,
                result,
            } => {
                let Some(common) = self.common_of(epoch) else {
                    return;
                };
                let ctx = &mut self.ctx;
                let repo = self.repos.get_mut(&common).expect("found above");
                let unfiltered = std::mem::take(&mut repo.unfiltered);
                let worktree = repo.worktrees.get_mut(&epoch).expect("found above");
                let guards = worktree.on_filter_loaded(ctx, &unfiltered, load, result);
                repo.unfiltered = unfiltered;
                drop(guards);
            }
            Cmd::ConfigLoaded {
                epoch,
                load,
                result,
            } => {
                let Some(common) = self.common_of(epoch) else {
                    return;
                };
                let ctx = &mut self.ctx;
                let repo = self.repos.get_mut(&common).expect("found above");
                let common_target = repo.target;
                let worktree = repo.worktrees.get_mut(&epoch).expect("found above");
                let guards = worktree.on_config_loaded(ctx, &common, load, result);
                worktree.publish_armed(ctx, &common, common_target);
                drop(guards);
            }
            Cmd::Fence(n) => self.fences.complete(n),
        }
        self.refresh_busy();
    }

    /// The repository holding the worktree state `epoch`.
    fn common_of(&self, epoch: u64) -> Option<PathBuf> {
        self.repos
            .iter()
            .find(|(_, repo)| repo.worktrees.contains_key(&epoch))
            .map(|(common, _)| common.clone())
    }

    fn unsubscribe(&mut self, sub: SubId) {
        let Some(loc) = self.subs.remove(&sub) else {
            return;
        };
        let ctx = &mut self.ctx;
        match loc {
            SubLoc::Repo(common) => {
                if let Some(repo) = self.repos.get_mut(&common) {
                    repo.remove_sub(sub);
                }
                self.collect_repo(&common);
            }
            SubLoc::Worktree(common, epoch) => {
                if let Some(repo) = self.repos.get_mut(&common)
                    && let Some(worktree) = repo.worktrees.get_mut(&epoch)
                {
                    worktree.remove_sub(sub);
                    if worktree.subs.is_empty() {
                        let mut worktree = repo.worktrees.remove(&epoch).expect("present");
                        worktree.teardown(ctx, &common);
                    }
                }
                self.collect_repo(&common);
            }
            SubLoc::Dir(root) => {
                if let Some(dir) = self.dirs.get_mut(&root) {
                    dir.remove_sub(sub);
                    if dir.subs.is_empty() {
                        let mut dir = self.dirs.remove(&root).expect("present");
                        dir.teardown(ctx);
                    }
                }
            }
        }
    }

    /// Drops a repository state with no subscription and no worktree left.
    fn collect_repo(&mut self, common: &Path) {
        if let Some(repo) = self.repos.get(common)
            && repo.subs.is_empty()
            && repo.worktrees.is_empty()
        {
            let mut repo = self.repos.remove(common).expect("present");
            repo.teardown(&mut self.ctx);
        }
    }

    /// Hands every target the subscriptions it may take a guard from,
    /// oldest first.
    fn refresh_busy(&mut self) {
        self.ctx.targets_dirty = false;
        for entry in self.ctx.targets.values() {
            let mut sources: Vec<(SubId, BusyFn)> = Vec::new();
            match &entry.role {
                Role::Common(common) => {
                    if let Some(repo) = self.repos.get(common) {
                        sources.extend(repo.busy_sources());
                        for worktree in repo.worktrees.values() {
                            sources.extend(worktree.busy_sources());
                        }
                    }
                }
                Role::Root { common, epoch } => {
                    if let Some(worktree) = self
                        .repos
                        .get(common)
                        .and_then(|repo| repo.worktrees.get(epoch))
                    {
                        sources.extend(worktree.busy_sources());
                    }
                }
                Role::Dir(root) => {
                    if let Some(dir) = self.dirs.get(root) {
                        sources.extend(dir.busy_sources());
                    }
                }
                Role::Entries(path) => {
                    if let Some(reg) = self.ctx.entries.get(path) {
                        for user in reg.users.keys() {
                            match user {
                                EntriesUser::Config { common, epoch } => {
                                    if let Some(worktree) = self
                                        .repos
                                        .get(common)
                                        .and_then(|repo| repo.worktrees.get(epoch))
                                    {
                                        sources.extend(worktree.busy_sources());
                                    }
                                }
                                EntriesUser::Dir(root) => {
                                    if let Some(dir) = self.dirs.get(root) {
                                        sources.extend(dir.busy_sources());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            sources.sort_by_key(|(id, _)| *id);
            sources.dedup_by_key(|(id, _)| *id);
            entry
                .shared
                .set_busy(sources.into_iter().map(|(_, busy)| busy).collect());
        }
    }

    /// Delivers the batches whose window ended. Returns how many.
    pub(crate) fn fire_timers(&mut self) -> usize {
        if self.down {
            return 0;
        }
        let ctx = &self.ctx;
        let mut delivered = 0;
        for repo in self.repos.values_mut() {
            delivered += repo.fire(ctx);
            for worktree in repo.worktrees.values_mut() {
                delivered += worktree.fire(ctx);
            }
        }
        for dir in self.dirs.values_mut() {
            delivered += dir.fire(ctx);
        }
        delivered
    }

    /// The earliest end of an open window; None when nothing waits (the
    /// fs_watch thread then sleeps without a timer).
    pub(crate) fn next_deadline(&self) -> Option<Duration> {
        let repos = self.repos.values().flat_map(|repo| {
            std::iter::once(repo.next_deadline())
                .chain(repo.worktrees.values().map(WorktreeState::next_deadline))
        });
        let dirs = self.dirs.values().map(DirState::next_deadline);
        repos.chain(dirs).flatten().min()
    }

    /// Drops every state and backend watch without delivering anything.
    pub(crate) fn shutdown(&mut self) {
        if self.down {
            return;
        }
        self.down = true;
        self.stopping.store(true, SeqCst);
        let ctx = &mut self.ctx;
        for (_, mut repo) in std::mem::take(&mut self.repos) {
            repo.teardown(ctx);
        }
        for (_, mut dir) in std::mem::take(&mut self.dirs) {
            dir.teardown(ctx);
        }
        self.subs.clear();
        ctx.entries.clear();
        ctx.targets.clear();
    }

    #[cfg(test)]
    pub(crate) fn has_timer(&self) -> bool {
        self.next_deadline().is_some()
    }
}
