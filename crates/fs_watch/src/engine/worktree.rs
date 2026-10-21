//! A worktree state: the worktree root target, the filter, the config list
//! and the revision (docs/design/fs_watch.md「路径分类」「修订号」).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Weak};

use lectern_executor::{Priority, Task};
use lectern_kernel::{BusyGuard, contain};

use super::cache::DirCache;
use super::config::ConfigPlan;
use super::stash::Stash;
use super::sub::{Sub, WorktreeAcc};
use super::{Ctx, EntriesUser, Role};
use crate::backend::{Armed, BackendError, Flags, RawBatch, Scope, Target};
use crate::path::{WatchPath, file_name, holds_under, holds_within, parent};
use crate::queue::{Cmd, SubId, TargetId};
use crate::source::{PathFilter, Sink, SourceError, Tracked, WorktreeSource};
use crate::types::{Coalesce, Degraded, Failure, Health, MetaChanges, WorktreeBatch, WorktreeName};
use crate::watcher::WorktreeShared;

pub(crate) struct WorktreeSub {
    pub(crate) sub: Sub<WorktreeBatch>,
    acc: WorktreeAcc,
}

/// A load running on the executor. Dropping it cancels the task.
struct Load {
    id: u64,
    cancel: Arc<AtomicBool>,
    _task: Task<()>,
    /// One per subscription, taken when the load started; dropped once the
    /// result is installed and the batches it led to are delivered.
    guards: Vec<BusyGuard>,
}

impl Drop for Load {
    fn drop(&mut self) {
        self.cancel.store(true, SeqCst);
    }
}

enum Filter {
    /// No load has finished yet.
    Missing,
    Ready(Arc<dyn PathFilter>),
    Failed(String),
}

/// Why the filter is reloaded (docs/design/fs_watch.md「路径分类」).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reload {
    /// Only the worktree's `index` changed: the load may reuse what the
    /// filter in place found on disk (`FilterRequest::previous`).
    Index,
    /// Anything else (the first load, a `.gitignore` or directory event,
    /// `info/exclude`, the config, a rescan): the load reads the worktree.
    Full,
}

impl Reload {
    /// Two reloads served by one load: `Index` only when both are.
    fn and(self, other: Reload) -> Reload {
        if self == Reload::Index && other == Reload::Index {
            Reload::Index
        } else {
            Reload::Full
        }
    }
}

/// Steps 4–9 of the classification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    Deliver { rules: bool },
    Drop,
    Stash,
}

pub(crate) struct WorktreeState {
    pub(crate) shared: Arc<WorktreeShared>,
    pub(crate) name: WorktreeName,
    root: PathBuf,
    source: Arc<dyn WorktreeSource>,
    pub(crate) root_target: Option<TargetId>,
    pub(crate) failure: Option<Failure>,
    filter: Filter,
    /// From the raw batch that triggered a reload until the new filter is
    /// installed; also true until the first filter is in.
    stale: bool,
    filter_load: Option<Load>,
    /// Reloads triggered while one was running, with their `.gitignore`
    /// paths and their reasons combined.
    filter_again: Option<(BTreeSet<WatchPath>, Reload)>,
    config_load: Option<Load>,
    config_again: bool,
    config_ready: bool,
    config_error: Option<String>,
    /// An `Entries` target of the config list could not be armed.
    entries_error: Option<String>,
    pub(crate) plan: ConfigPlan,
    stash: Stash,
    cache: DirCache,
    pub(crate) subs: Vec<WorktreeSub>,
    /// The health the started subscriptions were last told.
    told: Health,
    /// The revision must go up once for the item being processed.
    changed: bool,
    bumped: bool,
}

impl WorktreeState {
    pub(crate) fn new(
        shared: Arc<WorktreeShared>,
        name: WorktreeName,
        root: PathBuf,
        source: Arc<dyn WorktreeSource>,
    ) -> WorktreeState {
        WorktreeState {
            shared,
            name,
            root,
            source,
            root_target: None,
            failure: None,
            filter: Filter::Missing,
            stale: true,
            filter_load: None,
            filter_again: None,
            config_load: None,
            config_again: false,
            config_ready: false,
            config_error: None,
            entries_error: None,
            plan: ConfigPlan::default(),
            stash: Stash::default(),
            cache: DirCache::default(),
            subs: Vec::new(),
            told: Health::Live,
            changed: false,
            bumped: false,
        }
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.shared.epoch
    }

    pub(crate) fn user(&self, common: &Path) -> EntriesUser {
        EntriesUser::Config {
            common: common.to_path_buf(),
            epoch: self.epoch(),
        }
    }

    pub(crate) fn add_sub(
        &mut self,
        id: SubId,
        sink: Arc<dyn Sink<WorktreeBatch>>,
        coalesce: Coalesce,
        guard: Option<BusyGuard>,
    ) {
        self.subs.push(WorktreeSub {
            sub: Sub::new(id, sink, coalesce, guard),
            acc: WorktreeAcc::default(),
        });
    }

    pub(crate) fn remove_sub(&mut self, id: SubId) {
        self.subs.retain(|sub| sub.sub.id != id);
    }

    /// Arms the worktree root and starts the first loads.
    pub(crate) fn start(&mut self, ctx: &mut Ctx, common: &Path) {
        let target = Target {
            path: self.root.clone(),
            scope: Scope::Tree,
        };
        let role = Role::Root {
            common: common.to_path_buf(),
            epoch: self.epoch(),
        };
        match ctx.arm(target, role) {
            Ok(id) => self.root_target = Some(id),
            Err(BackendError::NotFound) => return self.fail(ctx, common, Failure::RootGone),
            Err(error) => return self.fail(ctx, common, Failure::Backend(error.to_string())),
        }
        self.reload_filter(ctx, BTreeSet::new(), Reload::Full);
        self.reload_config(ctx, common);
    }

    /// Marks the revision to go up once for the current item.
    fn mark(&mut self) {
        self.changed = true;
    }

    /// Applies a pending revision increment now (at most once per item).
    fn bump_now(&mut self) {
        if self.changed && !self.bumped {
            self.shared.bump();
            self.bumped = true;
        }
    }

    /// The end of an item: applies the increment and resets the marks.
    pub(crate) fn end_item(&mut self) {
        self.bump_now();
        self.changed = false;
        self.bumped = false;
    }

    pub(crate) fn health(&self) -> Health {
        if let Some(failure) = &self.failure {
            return Health::Failed(failure.clone());
        }
        if let Filter::Failed(message) = &self.filter {
            return Health::Degraded(Degraded::Filter(message.clone()));
        }
        if let Some(message) = self.config_error.as_ref().or(self.entries_error.as_ref()) {
            return Health::Degraded(Degraded::Config(message.clone()));
        }
        Health::Live
    }

    fn ready(&self) -> bool {
        self.failure.is_some() || (!matches!(self.filter, Filter::Missing) && self.config_ready)
    }

    /// The backend watches `flush_revision` flushes: the worktree root, the
    /// common directory and the config list's `Entries` targets.
    pub(crate) fn publish_armed(&self, ctx: &Ctx, common: &Path, common_target: Option<TargetId>) {
        if self.failure.is_some() || !self.ready_to_flush() {
            return;
        }
        let user = self.user(common);
        let mut armed: Vec<Weak<dyn Armed>> = Vec::new();
        for id in [self.root_target, common_target].into_iter().flatten() {
            armed.extend(ctx.armed(id));
        }
        for id in ctx.entries_targets_of(&user) {
            armed.extend(ctx.armed(id));
        }
        self.shared.set_watching(armed);
    }

    fn ready_to_flush(&self) -> bool {
        self.root_target.is_some()
    }

    // --- events on the worktree root ---------------------------------------

    /// A raw batch from the worktree root target.
    pub(crate) fn on_root_batch(
        &mut self,
        ctx: &mut Ctx,
        common: &Path,
        unfiltered: &[WatchPath],
        batch: RawBatch,
    ) {
        if self.failure.is_some() {
            return;
        }
        let now = ctx.now();
        // Pass 1: does this raw batch trigger a filter reload? From this raw
        // batch on, the filter is stale.
        let mut gitignores = BTreeSet::new();
        let mut reload = false;
        for event in &batch.events {
            if event.path.is_empty() || event.flags.contains(Flags::RESCAN) {
                continue;
            }
            if let Some(gitignore) = self.trigger(&event.path, event.flags, unfiltered) {
                reload = true;
                gitignores.extend(gitignore);
            }
        }
        if reload {
            self.reload_filter(ctx, gitignores, Reload::Full);
        }
        // Pass 2: classify.
        let mut config = false;
        let mut rescan = false;
        for event in batch.events {
            let path = &*event.path;
            if path.is_empty() {
                if event.flags.contains(Flags::UNMOUNTED) {
                    return self.fail(ctx, common, Failure::Unmounted);
                }
                if event.flags.contains(Flags::ROOT_CHANGED) {
                    return self.fail(ctx, common, Failure::RootGone);
                }
            }
            if event.flags.contains(Flags::RESCAN) {
                rescan = true;
                continue;
            }
            if path.is_empty() {
                continue;
            }
            // Step 2: a file on the config list, before filtering.
            if ConfigPlan::matches(&self.plan.root, path) {
                config = true;
            }
            // Validated without allocating: most events are dropped.
            if !crate::path::is_valid(path) {
                continue;
            }
            // Step 3.
            if path.split(|&b| b == b'/').any(|name| name == b".git") {
                continue;
            }
            match self.verdict(path, event.flags, unfiltered, self.stale) {
                Verdict::Deliver { rules } => {
                    self.mark();
                    let watch_path = WatchPath::new(path).expect("validated above");
                    self.emit_path(now, watch_path, rules, ctx.max_paths());
                }
                Verdict::Stash => {
                    self.mark();
                    let fresh = matches!(self.filter, Filter::Missing);
                    self.stash.insert(path, event.flags, fresh);
                    self.touch_all(now);
                }
                Verdict::Drop => {}
            }
        }
        if rescan {
            self.rescan(ctx, common);
        }
        if config {
            self.config_changed(ctx, common);
        }
    }

    /// A rescan of the worktree: every path may have changed.
    pub(crate) fn rescan(&mut self, ctx: &mut Ctx, common: &Path) {
        let now = ctx.now();
        self.mark();
        for sub in &mut self.subs {
            sub.acc.set_rescan();
            sub.sub.touch(now);
        }
        self.reload_filter(ctx, BTreeSet::new(), Reload::Full);
        self.reload_config(ctx, common);
    }

    /// Whether an event triggers a filter reload; `Some` with the
    /// `.gitignore` path to read, or `Some(None)`.
    fn trigger(
        &mut self,
        path: &[u8],
        flags: Flags,
        unfiltered: &[WatchPath],
    ) -> Option<Option<WatchPath>> {
        let is_gitignore = file_name(path) == b".gitignore";
        let moved_dir = flags.contains(Flags::DIR) && flags.any(Flags::RENAMED | Flags::REMOVED);
        let rule_dir = match &self.filter {
            Filter::Ready(filter) => holds_under(filter.rule_files(), path),
            _ => false,
        };
        if !(is_gitignore || moved_dir || rule_dir) {
            return None;
        }
        if path.split(|&b| b == b'/').any(|name| name == b".git") {
            return None;
        }
        let watch_path = WatchPath::new(path)?;
        match &self.filter {
            // Checked once the first filter is in (`Held::fresh`).
            Filter::Missing => None,
            Filter::Failed(_) => is_gitignore.then_some(Some(watch_path)),
            Filter::Ready(_) => match self.verdict(path, flags, unfiltered, false) {
                Verdict::Deliver { .. } => Some(is_gitignore.then_some(watch_path)),
                Verdict::Drop | Verdict::Stash => None,
            },
        }
    }

    /// Steps 4–9 for a path that passed steps 1–3.
    fn verdict(
        &mut self,
        path: &[u8],
        flags: Flags,
        unfiltered: &[WatchPath],
        stale: bool,
    ) -> Verdict {
        let is_dir = flags.contains(Flags::DIR);
        let rules = is_rule_file(path);
        // Step 4.
        if holds_within(unfiltered, path) {
            return Verdict::Deliver { rules };
        }
        let filter = match &self.filter {
            Filter::Missing => return Verdict::Stash,
            Filter::Failed(_) => return Verdict::Deliver { rules },
            Filter::Ready(filter) => filter.clone(),
        };
        // Step 5.
        if filter.tracked(path) != Tracked::No {
            return Verdict::Deliver { rules };
        }
        // Step 6.
        let passes = self.parents_pass(&*filter, path);
        if stale {
            return if rules && passes {
                Verdict::Deliver { rules }
            } else {
                Verdict::Stash
            };
        }
        if !passes {
            return Verdict::Drop;
        }
        // Step 7.
        if rules {
            return Verdict::Deliver { rules };
        }
        // Step 8.
        if filter.ignored(path, is_dir) {
            return Verdict::Drop;
        }
        // Step 9.
        Verdict::Deliver { rules: false }
    }

    /// Step 6: every directory above `path` lets it through (not a
    /// submodule, not an ignored untracked directory). Cached per directory.
    fn parents_pass(&mut self, filter: &dyn PathFilter, path: &[u8]) -> bool {
        let Some(dir) = parent(path) else {
            return true;
        };
        if let Some(passes) = self.cache.get(dir) {
            return passes;
        }
        let mut passes = true;
        for (i, _) in dir.iter().enumerate().filter(|&(_, &b)| b == b'/') {
            if !self.dir_passes(filter, &dir[..i]) {
                passes = false;
                break;
            }
        }
        if passes {
            passes = self.dir_passes(filter, dir);
        } else {
            self.cache.insert(dir, false);
        }
        passes
    }

    /// One directory, given that the directories above it pass.
    fn dir_passes(&mut self, filter: &dyn PathFilter, dir: &[u8]) -> bool {
        if let Some(passes) = self.cache.get(dir) {
            return passes;
        }
        // An ignored directory hides its untracked contents even when it
        // holds tracked files (those were delivered at step 5): git does not
        // re-include anything below an excluded directory.
        let passes = match filter.tracked(dir) {
            Tracked::Gitlink => false,
            Tracked::No | Tracked::File | Tracked::Dir => !filter.ignored(dir, true),
        };
        self.cache.insert(dir, passes);
        passes
    }

    fn emit_path(&mut self, now: std::time::Duration, path: WatchPath, rules: bool, max: usize) {
        for sub in &mut self.subs {
            sub.acc.add(path.clone(), max);
            sub.acc.meta.rules |= rules;
            sub.sub.touch(now);
        }
    }

    fn touch_all(&mut self, now: std::time::Duration) {
        for sub in &mut self.subs {
            sub.sub.touch(now);
        }
    }

    /// A change of git metadata that concerns this worktree: sets `meta`
    /// flags and counts.
    pub(crate) fn meta(&mut self, ctx: &Ctx, set: impl Fn(&mut MetaChanges)) {
        if self.failure.is_some() {
            return;
        }
        let now = ctx.now();
        self.mark();
        for sub in &mut self.subs {
            set(&mut sub.acc.meta);
            sub.sub.touch(now);
        }
    }

    /// A file on the config list changed: `meta.config`, count, recompute
    /// the list and the filter.
    pub(crate) fn config_changed(&mut self, ctx: &mut Ctx, common: &Path) {
        if self.failure.is_some() {
            return;
        }
        self.meta(ctx, |meta| meta.config = true);
        self.reload_config(ctx, common);
        self.reload_filter(ctx, BTreeSet::new(), Reload::Full);
    }

    // --- loads -------------------------------------------------------------

    /// Starts a filter load, or queues one after the running load. The filter
    /// is stale from now until a result is installed.
    ///
    /// Every reload requested after a load started is served by the next
    /// load (queued ones in `filter_again`), so when all of those are
    /// `Reload::Index`, nothing but the index changed since the filter in
    /// place began reading the worktree, and it goes to the source as
    /// `FilterRequest::previous`.
    pub(crate) fn reload_filter(
        &mut self,
        ctx: &mut Ctx,
        gitignores: BTreeSet<WatchPath>,
        reload: Reload,
    ) {
        if self.failure.is_some() {
            return;
        }
        self.stale = true;
        if self.filter_load.is_some() {
            let again = self
                .filter_again
                .get_or_insert_with(|| (BTreeSet::new(), Reload::Index));
            again.0.extend(gitignores);
            again.1 = again.1.and(reload);
            return;
        }
        let source = self.source.clone();
        let gitignores: Vec<WatchPath> = gitignores.into_iter().collect();
        // Paths held before the first filter haven't been checked for
        // reload triggers yet (`reclassify`): one may be a new `.gitignore`.
        let previous = match (&self.filter, reload) {
            (Filter::Ready(filter), Reload::Index) if !self.stash.fresh => Some(filter.clone()),
            _ => None,
        };
        let epoch = self.epoch();
        self.filter_load = Some(self.spawn(ctx, move |cancel| {
            let request = crate::source::FilterRequest {
                gitignores: &gitignores,
                previous: previous.as_deref(),
            };
            let result = contain(|| source.filter(&request, cancel))
                .unwrap_or_else(|message| Err(SourceError(format!("panicked: {message}"))));
            drop(previous);
            move |load| Cmd::FilterLoaded {
                epoch,
                load,
                result,
            }
        }));
    }

    /// Starts recomputing the config list, or queues it after the running
    /// one.
    pub(crate) fn reload_config(&mut self, ctx: &mut Ctx, common: &Path) {
        if self.failure.is_some() {
            return;
        }
        if self.config_load.is_some() {
            self.config_again = true;
            return;
        }
        let source = self.source.clone();
        let (common, root) = (common.to_path_buf(), self.root.clone());
        let epoch = self.epoch();
        self.config_load = Some(self.spawn(ctx, move |cancel| {
            let result = contain(|| {
                source
                    .config_files(cancel)
                    .map(|files| ConfigPlan::build(&files, &common, &root))
            })
            .unwrap_or_else(|message| Err(SourceError(format!("panicked: {message}"))));
            move |load| Cmd::ConfigLoaded {
                epoch,
                load,
                result,
            }
        }));
    }

    fn spawn<R: FnOnce(u64) -> Cmd + Send + 'static>(
        &self,
        ctx: &mut Ctx,
        work: impl FnOnce(&AtomicBool) -> R + Send + 'static,
    ) -> Load {
        let id = ctx.next_load();
        let cancel = Arc::new(AtomicBool::new(false));
        let guards = self.subs.iter().filter_map(|sub| sub.sub.busy()).collect();
        let queue = ctx.queue.clone();
        let flag = cancel.clone();
        let task = ctx.executor.run(Priority::Background, move || {
            if flag.load(SeqCst) {
                return;
            }
            let reply = work(&flag);
            if flag.load(SeqCst) {
                return;
            }
            if let Some(refused) = queue.push_cmd(reply(id)) {
                drop(refused);
            }
        });
        Load {
            id,
            cancel,
            _task: task,
            guards,
        }
    }

    /// A filter load finished. Returns the load's guards, to drop after the
    /// batches it leads to are delivered.
    pub(crate) fn on_filter_loaded(
        &mut self,
        ctx: &mut Ctx,
        unfiltered: &[WatchPath],
        load: u64,
        result: Result<Arc<dyn PathFilter>, SourceError>,
    ) -> Vec<BusyGuard> {
        if self
            .filter_load
            .as_ref()
            .is_none_or(|running| running.id != load)
        {
            return Vec::new();
        }
        let mut finished = self.filter_load.take().expect("checked above");
        let guards = std::mem::take(&mut finished.guards);
        drop(finished);
        self.filter = match result {
            Ok(filter) => Filter::Ready(filter),
            Err(error) => Filter::Failed(error.0),
        };
        self.cache.clear();
        if let Some((gitignores, reload)) = self.filter_again.take() {
            // Paths stay held for the newer filter.
            self.reload_filter(ctx, gitignores, reload);
        } else {
            self.stale = false;
            self.reclassify(ctx, unfiltered);
        }
        self.settle(ctx);
        guards
    }

    /// A config list load finished.
    pub(crate) fn on_config_loaded(
        &mut self,
        ctx: &mut Ctx,
        common: &Path,
        load: u64,
        result: Result<ConfigPlan, SourceError>,
    ) -> Vec<BusyGuard> {
        if self
            .config_load
            .as_ref()
            .is_none_or(|running| running.id != load)
        {
            return Vec::new();
        }
        let mut finished = self.config_load.take().expect("checked above");
        let guards = std::mem::take(&mut finished.guards);
        drop(finished);
        self.config_ready = true;
        match result {
            Ok(plan) => {
                self.config_error = None;
                self.install_plan(ctx, common, plan);
            }
            Err(error) => {
                // Only the config files in the repository stay watched.
                self.config_error = Some(error.0);
                self.install_plan(ctx, common, ConfigPlan::default());
            }
        }
        if std::mem::take(&mut self.config_again) {
            self.reload_config(ctx, common);
        }
        self.settle(ctx);
        guards
    }

    /// Arms only the new `Entries` targets and drops the ones no longer
    /// needed.
    fn install_plan(&mut self, ctx: &mut Ctx, common: &Path, plan: ConfigPlan) {
        let user = self.user(common);
        let old = std::mem::replace(&mut self.plan, plan);
        let mut error = None;
        for dir in old.entries.keys() {
            if !self.plan.entries.contains_key(dir) {
                let _ = ctx.entries_set(&user, dir, BTreeSet::new());
            }
        }
        for (dir, names) in &self.plan.entries {
            if (old.entries.get(dir) != Some(names) || ctx.entries_target(dir).is_none())
                && let Err(message) = ctx.entries_set(&user, dir, names.clone())
            {
                error = Some(format!("{}: {message}", dir.display()));
            }
        }
        self.entries_error = error;
    }

    /// Paths held while the filter was stale, classified again with the new
    /// filter; delivered ones join the current batch.
    fn reclassify(&mut self, ctx: &mut Ctx, unfiltered: &[WatchPath]) {
        let (entries, rescan) = self.stash.take();
        let now = ctx.now();
        let max = ctx.max_paths();
        if rescan {
            for sub in &mut self.subs {
                sub.acc.set_rescan();
                sub.sub.touch(now);
            }
        }
        let mut gitignores = BTreeSet::new();
        let mut reload = false;
        for (path, held) in entries {
            let Some(watch_path) = WatchPath::new(&path) else {
                continue;
            };
            if let Verdict::Deliver { rules } = self.verdict(&path, held.flags, unfiltered, false) {
                // A path held before the first filter: the walk building it
                // may have missed a new rule file or a moved-in directory.
                if held.fresh
                    && let Some(gitignore) = self.trigger(&path, held.flags, unfiltered)
                {
                    reload = true;
                    gitignores.extend(gitignore);
                }
                self.emit_path(now, watch_path, rules, max);
            }
        }
        if reload {
            self.reload_filter(ctx, gitignores, Reload::Full);
        }
    }

    /// After a load: tell subscriptions about readiness and health, and
    /// deliver what became due while the filter was stale.
    pub(crate) fn settle(&mut self, ctx: &mut Ctx) {
        self.publish_health();
        self.fire(ctx);
    }

    // --- failure -----------------------------------------------------------

    pub(crate) fn fail(&mut self, ctx: &mut Ctx, common: &Path, failure: Failure) {
        if self.failure.is_some() {
            return;
        }
        self.failure = Some(failure.clone());
        self.shared.fail(failure);
        if let Some(id) = self.root_target.take() {
            ctx.disarm(id);
        }
        self.filter_load = None;
        self.config_load = None;
        self.stash = Stash::default();
        let user = self.user(common);
        for dir in std::mem::take(&mut self.plan).entries.into_keys() {
            let _ = ctx.entries_set(&user, &dir, BTreeSet::new());
        }
        self.mark();
        self.bump_now();
        self.publish_health();
    }

    /// Drops everything without delivering (last handle gone, shutdown).
    pub(crate) fn teardown(&mut self, ctx: &mut Ctx, common: &Path) {
        self.shared.fail(Failure::Stopped);
        if let Some(id) = self.root_target.take() {
            ctx.disarm(id);
        }
        self.filter_load = None;
        self.config_load = None;
        let user = self.user(common);
        for dir in std::mem::take(&mut self.plan).entries.into_keys() {
            let _ = ctx.entries_set(&user, &dir, BTreeSet::new());
        }
        self.subs.clear();
    }

    // --- delivery ----------------------------------------------------------

    /// Health changes go out right away: first the current batch, then one
    /// with the new health. Subscriptions that have not had their first
    /// batch get it once the state is ready.
    pub(crate) fn publish_health(&mut self) {
        let health = self.health();
        if health != self.told {
            self.bump_now();
            let revision = self.shared.revision();
            for sub in self.subs.iter_mut().filter(|sub| sub.sub.started()) {
                if sub.sub.is_open() {
                    let batch = sub.acc.take(revision, None, false);
                    sub.sub.deliver(batch);
                }
                let batch = WorktreeAcc::default().take(revision, Some(health.clone()), false);
                sub.sub.deliver(batch);
            }
            self.told = health.clone();
        }
        self.start_subs();
    }

    /// The first batch (`health` and `rescan`) for subscriptions that have
    /// not had it, once the first loads are done.
    pub(crate) fn start_subs(&mut self) {
        if !self.ready() {
            return;
        }
        let health = self.health();
        let revision = self.shared.revision();
        for sub in self.subs.iter_mut().filter(|sub| !sub.sub.started()) {
            let batch = sub.acc.take(revision, Some(health.clone()), true);
            sub.sub.deliver(batch);
        }
        if self.subs.iter().all(|sub| sub.sub.started()) {
            self.told = health;
        }
    }

    /// Delivers the batches whose window ended. Nothing goes out while the
    /// filter is stale.
    pub(crate) fn fire(&mut self, ctx: &Ctx) -> usize {
        if self.stale && self.failure.is_none() {
            return 0;
        }
        let now = ctx.now();
        let revision = self.shared.revision();
        let mut delivered = 0;
        for sub in self.subs.iter_mut().filter(|sub| sub.sub.started()) {
            if sub.sub.deadline().is_some_and(|deadline| deadline <= now) {
                let batch = sub.acc.take(revision, None, false);
                sub.sub.deliver(batch);
                delivered += 1;
            }
        }
        delivered
    }

    pub(crate) fn next_deadline(&self) -> Option<std::time::Duration> {
        if self.stale && self.failure.is_none() {
            return None;
        }
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

/// `.gitignore` or `.gitattributes`.
fn is_rule_file(path: &[u8]) -> bool {
    matches!(file_name(path), b".gitignore" | b".gitattributes")
}
