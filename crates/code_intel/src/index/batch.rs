//! The tiers (「建索引的分层」「调度、批次与取消」).
//!
//! Tiers 1 and 2 are small explicit queues on the worker threads
//! (`Interactive` / `Background`, `BusyGuard`). Tiers 3–5 run in
//! long-running `Batch` tasks holding a `BackgroundGuard`, one per batch
//! thread: each batch is an existence check of up to 500 files, or the
//! extraction of the files it missed, ending at 64 files, 2 MiB read or the
//! first file boundary after 50 ms; between batches the task asks its guard
//! whether to pause. A file the pause interrupts is abandoned (the watchdog
//! cancels it) and goes back to the front of its queue.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lectern_cache::Written;
use lectern_executor::Priority;
use lectern_kernel::BackgroundGuard;
use lectern_platform::{Read, RelPath as SafeRel, SafeRoot};
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{BlobId, ProjectKey};

use super::host::yield_now;
use super::state::{
    Cursor, Item, OtherTree, OtherWalk, Owner, Project, Scope, Source, State, Tier, Walk, under,
};
use super::{Inner, bump, lock};
use crate::extract::{
    BACKGROUND_MAX_LEN, Deadline, ExtractStatus, Extraction, INTERACTIVE_MAX_LEN, Lang,
    LanguageTable,
};
use crate::git_access::{Tree, hash_blob, hash_kind};
use crate::map::{HintStamp, LangLookup};
use crate::store::{EXISTS_BATCH, Found, Version};
use crate::util::log;

/// A batch ends after this many files…
pub const BATCH_FILES: usize = 64;
/// …or this many bytes read…
pub const BATCH_BYTES: usize = 2 << 20;
/// …or at the first file boundary after this long.
pub const BATCH_TIME: Duration = Duration::from_millis(50);
/// Entries a gathering step looks at, at most, under the project's lock.
const SCAN_LIMIT: usize = 4096;
/// Files larger than this are extracted by one batch task at a time.
pub const LARGE_FILE: usize = 256 << 10;

/// The one batch extraction of a large file (`LARGE_FILE`).
#[derive(Default)]
pub(crate) struct LargeSlot {
    busy: std::sync::Mutex<bool>,
    freed: std::sync::Condvar,
}

pub(crate) struct LargeTurn<'a>(&'a LargeSlot);

impl LargeSlot {
    /// Waits for the slot, checking `stop` every 10 ms; None once `stop`
    /// says so.
    pub(crate) fn acquire(&self, stop: impl Fn() -> bool) -> Option<LargeTurn<'_>> {
        let mut busy = lock(&self.busy);
        while *busy {
            if stop() {
                return None;
            }
            busy = self
                .freed
                .wait_timeout(busy, crate::CHECK_INTERVAL)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        if stop() {
            return None;
        }
        *busy = true;
        Some(LargeTurn(self))
    }
}

impl Drop for LargeTurn<'_> {
    fn drop(&mut self) {
        *lock(&self.0.busy) = false;
        self.0.freed.notify_one();
    }
}

/// How an item was extracted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Tier 1: the interactive deadline and size limit.
    Interactive,
    /// Tier 2: the background deadline.
    Background,
    /// Tiers 3–5: the background deadline, interrupted by the pause.
    Batch,
}

/// An extraction a query made of a file not stored yet (a search hit, an
/// import target), to be written like tier 2's (「输入代数与派生状态」).
pub(crate) struct Scanned {
    pub path: lectern_services::project::RelPath,
    /// The OID of the bytes extracted.
    pub oid: BlobId,
    /// The language as the query's (current) table had it.
    pub lang: Lang,
    pub extraction: Extraction,
}

impl Clone for Scanned {
    fn clone(&self) -> Scanned {
        Scanned {
            path: self.path.clone(),
            oid: self.oid,
            lang: self.lang.clone(),
            extraction: self.extraction.clone(),
        }
    }
}

/// One extraction ready to be written.
struct Done {
    item: Item,
    /// The OID of the bytes actually read (「存储表」).
    key: BlobId,
    extraction: Extraction,
    /// The cancel flag the item was taken with (`State::finish`).
    flag: Arc<AtomicBool>,
}

enum Outcome {
    Done(Done, usize),
    /// The pause interrupted it: back to the front of its queue.
    Paused,
    /// Nothing to write (gone, changed in another worktree, cancelled).
    Skip,
}

/// What a batch task should do next.
enum Step {
    Worked,
    Idle,
}

/// The read side of one session, copied out of the lock.
struct Ctx {
    session: u64,
    cancel: Arc<AtomicBool>,
    root: Option<Arc<SafeRoot>>,
    tree: Option<Tree>,
}

impl Inner {
    /// Work for tiers 3–5 may have appeared: the pool runs `batch_tasks`
    /// tasks (one per batch thread), and a task about to stop goes on.
    ///
    /// The liveness rule: after every operation that can create batch work
    /// (open, close, a change of the project that runs tiers 3–5, focus,
    /// power, grammar, the end of a reconcile), either no project has
    /// runnable tier 3–5 work or the pool is full (`Inner::live`, checked
    /// by the tests' harness).
    pub(crate) fn kick_batch(self: &Arc<Self>) {
        self.fill_pool();
        lock(&self.batch).kicked = true;
    }

    /// Starts batch tasks up to `batch_tasks`.
    fn fill_pool(self: &Arc<Self>) {
        loop {
            {
                let mut run = lock(&self.batch);
                if run.running >= self.batch_tasks {
                    return;
                }
                run.running += 1;
                #[cfg(test)]
                {
                    run.peak = run.peak.max(run.running);
                }
            }
            let me = self.clone();
            let spawned = self
                .host
                .spawn_batch("batch", move |guard| me.batch_loop(guard));
            if !spawned {
                lock(&self.batch).running -= 1;
                return;
            }
        }
    }

    async fn batch_loop(self: Arc<Self>, guard: Arc<BackgroundGuard>) {
        loop {
            guard.proceed().await;
            match self.batch_step(&guard) {
                Step::Worked => {
                    // A step can leave work for others (missed files in a
                    // tier's queue): tasks that stopped start again.
                    self.fill_pool();
                    yield_now().await;
                }
                Step::Idle => {
                    let mut run = lock(&self.batch);
                    if run.kicked {
                        run.kicked = false;
                        continue;
                    }
                    run.running -= 1;
                    return;
                }
            }
        }
    }

    /// Whether the project that runs tiers 3–5 has work a new batch task
    /// would do now (`batch_step` would not be idle): files to extract,
    /// a walk to rebuild or to go on with, other worktrees left, touches.
    #[cfg(test)]
    pub(crate) fn runnable(&self) -> bool {
        let Some(project) = self.batch_project() else {
            return false;
        };
        let eviction = project.evictions();
        let st = lock(&project.state);
        if st.base_for.is_none() || st.cancel.load(Ordering::Acquire) {
            return false;
        }
        let wide = self.wide_allowed(&st);
        let tiers = if wide { 3 } else { 1 };
        if st.batch.resume[..tiers].iter().any(|q| !q.is_empty()) || !st.batch.touches.is_empty() {
            return true;
        }
        let deps = st.cursor_deps(eviction);
        let Some(cursor) = st
            .batch
            .cursor
            .as_ref()
            .filter(|c| c.deps == deps && c.scope == subproject(&st))
        else {
            return true;
        };
        if !cursor.walks[0].done || (wide && !cursor.walks[1].done) {
            return true;
        }
        if !wide {
            return false;
        }
        let others = &st.batch.others;
        let stamp = st.stamp(eviction);
        if others.deps != Some((st.session, stamp.eviction, stamp.grammar)) {
            return true;
        }
        let active = st.worktree();
        others.current.is_some()
            || st.snapshot.as_ref().is_some_and(|s| {
                s.worktrees
                    .iter()
                    .any(|w| !w.prunable && Some(&w.key) != active && !others.done.contains(&w.key))
            })
    }

    /// The liveness rule of `kick_batch`: no runnable batch work, or a full
    /// pool. False is a missed kick.
    #[cfg(test)]
    pub(crate) fn live(&self) -> bool {
        let running = lock(&self.batch).running;
        running >= self.batch_tasks || !self.runnable()
    }

    fn batch_project(&self) -> Option<Arc<Project>> {
        let key: Option<ProjectKey> = lock(&self.batch_project).clone();
        self.project(&key?)
    }

    /// One batch of tiers 3–5.
    fn batch_step(self: &Arc<Self>, guard: &Arc<BackgroundGuard>) -> Step {
        let Some(project) = self.batch_project() else {
            return Step::Idle;
        };
        let eviction = project.evictions();
        let (wide, ctx, stamp) = {
            let mut st = lock(&project.state);
            if st.base_for.is_none() || st.cancel.load(Ordering::Acquire) {
                return Step::Idle;
            }
            self.ensure_cursor(&mut st, eviction);
            (self.wide_allowed(&st), ctx(&st), st.stamp(eviction))
        };
        let tiers: &[usize] = if wide { &[0, 1, 2] } else { &[0] };
        // Files known to be missing go first, tier by tier.
        for &t in tiers {
            if !lock(&project.state).batch.resume[t].is_empty() {
                self.extract_batch(&project, &ctx, t, guard);
                return Step::Worked;
            }
        }
        for &t in tiers {
            let items = if t == 2 {
                match self.gather_other(&project, &ctx, stamp) {
                    Some(items) => items,
                    None => continue,
                }
            } else {
                let items = self.gather(&project, t, stamp);
                match items {
                    Some(items) => items,
                    None => continue,
                }
            };
            self.exists(&project, &ctx, items, stamp);
            return Step::Worked;
        }
        // Everything is checked: mark the warm start's hits as used.
        let mut touches = std::mem::take(&mut lock(&project.state).batch.touches);
        if !touches.is_empty() {
            project.store.touch_pending(&mut touches);
            let mut st = lock(&project.state);
            if st.session == ctx.session && st.batch.touches.is_empty() {
                st.batch.touches = touches;
            }
            return Step::Worked;
        }
        Step::Idle
    }

    /// Rebuilds tier 3 and 4's cursor when the map it walks, the
    /// sub-projects, the walks' eviction generation or tier 3's scope
    /// changed (a focus change alone does not).
    fn ensure_cursor(&self, st: &mut State, eviction: u64) {
        let deps = st.cursor_deps(eviction);
        let scope = subproject(st);
        if st
            .batch
            .cursor
            .as_ref()
            .is_some_and(|c| c.deps == deps && c.scope == scope)
        {
            return;
        }
        bump(&self.stats.cursor_builds, 1);
        let files = st.map.base.files().clone();
        let range = match &scope {
            Scope::Dir(dir) => files.range_under(dir),
            Scope::Whole => 0..files.len(),
            Scope::Empty => 0..0,
        };
        st.batch.cursor = Some(Cursor {
            deps,
            map: st.map.clone(),
            scope,
            range,
            walks: [Walk::default(); 2],
        });
        for queue in &mut st.batch.resume[..2] {
            queue.clear();
        }
        st.batch.queued.clear();
        let in_flight: Vec<_> = st.in_flight.keys().cloned().collect();
        st.batch.queued.extend(in_flight);
    }

    /// Up to 500 files of tier 3 (`t` 0) or 4 (`t` 1) whose hint does not
    /// match; None when the tier is walked through.
    fn gather(&self, project: &Arc<Project>, t: usize, stamp: HintStamp) -> Option<Vec<Item>> {
        let disabled = lock(&self.langs).disabled.clone();
        let mut st = lock(&project.state);
        let State {
            batch, in_flight, ..
        } = &mut *st;
        let cursor = batch.cursor.as_mut()?;
        if cursor.walks[t].done {
            return None;
        }
        let tier = if t == 0 {
            Tier::Subproject
        } else {
            Tier::Worktree
        };
        let mut items = Vec::new();
        let mut looked = 0;
        let mut consider = |eff: crate::map::Effective, items: &mut Vec<Item>| {
            if disabled.contains(eff.lang.name()) || cursor_hinted(&cursor.map, &eff, stamp) {
                return;
            }
            let key = (eff.oid, eff.lang.name().clone());
            if batch.queued.contains(&key) || in_flight.contains_key(&key) {
                return;
            }
            batch.queued.insert(key);
            items.push(Item {
                path: eff.path,
                oid: eff.oid,
                lang: eff.lang,
                source: Source::Worktree,
                tier,
            });
        };
        let overlay_len = cursor.map.overlay.len();
        let range = cursor.range.clone();
        let total = cursor.map.base.files().len();
        let base_len = if t == 0 {
            range.len()
        } else {
            total - range.len()
        };
        while items.len() < EXISTS_BATCH && looked < SCAN_LIMIT {
            let walk = &mut cursor.walks[t];
            if walk.overlay < overlay_len {
                let i = walk.overlay;
                walk.overlay += 1;
                looked += 1;
                let (path, _) = &cursor.map.overlay.entries()[i];
                if cursor.in_subproject(path) != (t == 0) {
                    continue;
                }
                if let Some(eff) = cursor.map.overlay_effective(i) {
                    consider(eff, &mut items);
                }
            } else if walk.base < base_len {
                let n = walk.base;
                walk.base += 1;
                looked += 1;
                let i = if t == 0 {
                    range.start + n
                } else if n < range.start {
                    n
                } else {
                    n + range.len()
                };
                if let Some(eff) = cursor.map.base_effective(i) {
                    consider(eff, &mut items);
                }
            } else {
                walk.done = true;
                break;
            }
        }
        if items.is_empty() && cursor.walks[t].done {
            return None;
        }
        Some(items)
    }

    /// Tier 5: up to 500 files of the next other worktree; None when every
    /// other worktree is done.
    fn gather_other(
        &self,
        project: &Arc<Project>,
        ctx: &Ctx,
        stamp: HintStamp,
    ) -> Option<Vec<Item>> {
        loop {
            let next = {
                let mut st = lock(&project.state);
                if !st.live(ctx.session) {
                    return None;
                }
                let deps = Some((st.session, stamp.eviction, stamp.grammar));
                if st.batch.others.deps != deps {
                    st.batch.others = super::state::Others {
                        deps,
                        ..Default::default()
                    };
                    st.batch.resume[2].clear();
                }
                if st.batch.others.current.is_some() {
                    None
                } else {
                    let snapshot = st.snapshot.clone()?;
                    let active = st.worktree().cloned();
                    let ProjectKey::Git { common_dir } = &project.key else {
                        return None;
                    };
                    let w = snapshot.worktrees.iter().find(|w| {
                        !w.prunable
                            && Some(&w.key) != active.as_ref()
                            && !st.batch.others.done.contains(&w.key)
                    })?;
                    Some((
                        w.key.clone(),
                        Tree {
                            common_dir: common_dir.clone(),
                            git_dir: w.key.0.clone(),
                            root: w.root.clone(),
                        },
                    ))
                }
            };
            if let Some((key, tree)) = next {
                // Read that worktree's index; it is dropped once walked.
                let loaded = SafeRoot::open(&tree.root)
                    .map_err(|e| e.to_string())
                    .and_then(|root| {
                        self.git
                            .tracked(&tree, &ctx.cancel)
                            .map(|entries| (root, entries))
                            .map_err(|e| e.to_string())
                    });
                let mut st = lock(&project.state);
                if !st.live(ctx.session) {
                    return None;
                }
                match loaded {
                    Ok((root, entries)) => {
                        let other = Arc::new(OtherTree {
                            key: key.clone(),
                            root,
                        });
                        st.batch.others.current = Some(OtherWalk {
                            tree: other,
                            entries: entries.into(),
                            pos: 0,
                        });
                    }
                    Err(error) => {
                        log(format_args!(
                            "reading worktree {}: {error}",
                            tree.root.display()
                        ));
                        st.batch.others.done.insert(key);
                    }
                }
                continue;
            }
            let disabled = lock(&self.langs).disabled.clone();
            let mut st = lock(&project.state);
            if st.batch.others.deps != Some((st.session, stamp.eviction, stamp.grammar)) {
                continue;
            }
            // Languages as the map knows them (its provenance), like tiers
            // 3 and 4.
            let map = st.map.clone();
            let table = map.base.table().clone();
            let mut lookup = LangLookup::new(&table, &*self.packs);
            let State {
                batch, in_flight, ..
            } = &mut *st;
            let walk = batch.others.current.as_mut()?;
            let mut items = Vec::new();
            let mut looked = 0;
            while walk.pos < walk.entries.len() && items.len() < EXISTS_BATCH && looked < SCAN_LIMIT
            {
                let (path, oid) = &walk.entries[walk.pos];
                walk.pos += 1;
                looked += 1;
                let code = lookup.code(path);
                let Some(lang) = code
                    .checked_sub(1)
                    .and_then(|i| table.langs().get(usize::from(i)))
                else {
                    continue;
                };
                if disabled.contains(lang.name()) {
                    continue;
                }
                let key = (*oid, lang.name().clone());
                // Known stored: the current worktree has it indexed, or this
                // session already found it for another worktree.
                if batch.others.confirmed.contains(oid, lang.name())
                    || map.hinted(oid, &lang.name().0, stamp)
                    || batch.queued.contains(&key)
                    || in_flight.contains_key(&key)
                {
                    continue;
                }
                batch.queued.insert(key);
                items.push(Item {
                    path: path.clone(),
                    oid: *oid,
                    lang: lang.clone(),
                    source: Source::Other(walk.tree.clone()),
                    tier: Tier::Other,
                });
            }
            if walk.pos >= walk.entries.len() {
                let key = walk.tree.key.clone();
                batch.others.current = None;
                batch.others.done.insert(key);
            }
            if items.is_empty() && batch.others.current.is_none() {
                continue;
            }
            return Some(items);
        }
    }

    /// The existence check of gathered items: hits set their hints (and
    /// those of every path with the same content), misses wait in their
    /// tier's queue.
    fn exists(&self, project: &Arc<Project>, ctx: &Ctx, items: Vec<Item>, stamp: HintStamp) {
        // Read before the queries: the touches are queued under it.
        let eviction = project.store.evictions();
        let mut groups: Vec<(Version, Vec<Item>)> = Vec::new();
        for item in items {
            let version = Version::of(&item.lang);
            match groups.iter_mut().find(|(v, _)| *v == version) {
                Some((_, group)) => group.push(item),
                None => groups.push((version, vec![item])),
            }
        }
        let mut found = Vec::new();
        let mut hits = Vec::new();
        for (version, group) in &groups {
            let oids: Vec<BlobId> = group.iter().map(|i| i.oid).collect();
            bump(&self.stats.exists_checks, oids.len().div_ceil(EXISTS_BATCH));
            let hit = project.store.exists(version, &oids).unwrap_or_default();
            for f in &hit {
                hits.push((f.oid, version.lang.clone()));
            }
            found.extend(hit);
        }
        let table = lock(&self.langs).table.clone();
        let mut st = lock(&project.state);
        if !st.live(ctx.session) {
            return;
        }
        st.batch.touches.push(eviction, &found);
        for (oid, lang) in &hits {
            // A hit stamps hints only for the version the map and the current
            // language table both have, and with the map's own stamp: a hit
            // of an older grammar never makes a newer map's path look stored.
            if stamp.grammar != st.map.provenance.grammar
                || !same_version(&table, &st.map, lang, &groups)
            {
                continue;
            }
            for eff in st.map.paths(oid, &lang.0) {
                st.map.hints.set(&eff.slot, oid, stamp);
            }
        }
        for (_, group) in groups {
            for item in group {
                let key = item.key();
                if hits.contains(&key) {
                    st.batch.queued.remove(&key);
                    if item.tier == Tier::Other {
                        st.batch
                            .others
                            .confirmed
                            .insert(&item.oid, item.lang.name());
                    }
                    continue;
                }
                let Some(t) = item.tier.batch() else {
                    continue;
                };
                st.batch.resume[t].push_back(item);
            }
        }
    }

    /// Extracts files of tier `t`'s queue until the batch's limits.
    fn extract_batch(
        self: &Arc<Self>,
        project: &Arc<Project>,
        ctx: &Ctx,
        t: usize,
        guard: &Arc<BackgroundGuard>,
    ) {
        let clock = self.watchdog.clock().clone();
        let start = clock.now();
        let (mut files, mut bytes) = (0, 0);
        let mut done = Vec::new();
        while files < BATCH_FILES && bytes < BATCH_BYTES && clock.now() - start < BATCH_TIME {
            if guard.pause_requested() || ctx.cancel.load(Ordering::Acquire) {
                break;
            }
            let Some((item, flag)) = self.take(project, ctx, t) else {
                break;
            };
            match self.extract_item(ctx, &item, Mode::Batch, Some(guard), &flag) {
                Outcome::Done(d, len) => {
                    files += 1;
                    bytes += len;
                    done.push(d);
                }
                Outcome::Paused => {
                    let mut st = lock(&project.state);
                    // Not when tier 1 took it over (`focus_task`).
                    if st.finish(ctx.session, &item, &flag) && !flag.load(Ordering::Acquire) {
                        // Still queued: back to the front.
                        st.batch.queued.insert(item.key());
                        st.batch.resume[t].push_front(item);
                    }
                    break;
                }
                Outcome::Skip => self.release(project, ctx, &item, &flag),
            }
        }
        #[cfg(test)]
        lock(&self.batch_log).push(files);
        self.commit(project, ctx, done, true);
    }

    /// Takes the next item of tier `t`'s queue and registers it in flight.
    /// Items stored since their existence check (by tier 1 or 2, or for
    /// another worktree) are dropped.
    fn take(&self, project: &Arc<Project>, ctx: &Ctx, t: usize) -> Option<(Item, Arc<AtomicBool>)> {
        let eviction = project.evictions();
        let mut st = lock(&project.state);
        if !st.live(ctx.session) {
            return None;
        }
        let stamp = st.stamp(eviction);
        loop {
            let item = st.batch.resume[t].pop_front()?;
            let key = item.key();
            if st.in_flight.contains_key(&key)
                || st
                    .batch
                    .others
                    .confirmed
                    .contains(&item.oid, item.lang.name())
                || st.map.hinted(&item.oid, &item.lang.name().0, stamp)
            {
                st.batch.queued.remove(&key);
                continue;
            }
            let flag = Arc::new(AtomicBool::new(ctx.cancel.load(Ordering::Acquire)));
            let owner = Owner {
                flag: flag.clone(),
                tier: item.tier,
            };
            st.in_flight.insert(key, owner);
            st.batch.active[t] += 1;
            return Some((item, flag));
        }
    }

    /// Forgets an item that gave nothing to write.
    fn release(&self, project: &Arc<Project>, ctx: &Ctx, item: &Item, flag: &Arc<AtomicBool>) {
        lock(&project.state).finish(ctx.session, item, flag);
    }

    /// Reads and extracts one item. The key is the OID of the bytes read:
    /// the raw bytes', or the queued OID when the built-in conversions turn
    /// them into it (「存储表」).
    fn extract_item(
        &self,
        ctx: &Ctx,
        item: &Item,
        mode: Mode,
        guard: Option<&Arc<BackgroundGuard>>,
        cancel: &Arc<AtomicBool>,
    ) -> Outcome {
        let max = match mode {
            Mode::Interactive => INTERACTIVE_MAX_LEN,
            Mode::Background | Mode::Batch => BACKGROUND_MAX_LEN,
        };
        let too_large = |item: &Item| {
            Outcome::Done(
                Done {
                    item: item.clone(),
                    key: item.oid,
                    extraction: Extraction {
                        status: ExtractStatus::TooLarge,
                        defs: Vec::new(),
                        refs: Vec::new(),
                    },
                    flag: cancel.clone(),
                },
                0,
            )
        };
        let read = |root: &SafeRoot| {
            let rel = SafeRel::new(item.path.as_bytes())?;
            match root.read(rel, max as u64) {
                Ok(Read::File { bytes, .. }) => Some(Ok(bytes)),
                Ok(Read::TooLarge { .. }) => Some(Err(())),
                _ => None,
            }
        };
        let bytes: Vec<u8> = match &item.source {
            Source::Worktree => match ctx.root.as_deref().and_then(read) {
                Some(Ok(bytes)) => bytes,
                Some(Err(())) => return too_large(item),
                None => return Outcome::Skip,
            },
            Source::Other(other) => match read(&other.root) {
                Some(Ok(bytes)) => bytes,
                Some(Err(())) => return too_large(item),
                None => return Outcome::Skip,
            },
            Source::Object => {
                let Some(tree) = &ctx.tree else {
                    return Outcome::Skip;
                };
                match self.git.blob(tree, item.oid, max as u64) {
                    Ok(bytes) => bytes.to_vec(),
                    Err(lectern_git::GitError::TooLarge { .. }) => return too_large(item),
                    Err(_) => return Outcome::Skip,
                }
            }
        };
        let raw = hash_blob(hash_kind(&item.oid), &bytes);
        let key = if raw == item.oid {
            raw
        } else {
            match &item.source {
                // Another worktree's file that differs from its index waits
                // until that worktree is the current one.
                Source::Other(_) => return Outcome::Skip,
                Source::Object => raw,
                Source::Worktree => match &ctx.tree {
                    Some(tree) => match self.git.canonical_oid(tree, &item.path, &bytes) {
                        Ok(Some(canonical)) if canonical == item.oid => item.oid,
                        _ => raw,
                    },
                    None => raw,
                },
            }
        };
        // A large file's syntax tree is most of a batch's memory (a file near
        // 1 MiB about 31 MiB): one batch task at a time extracts one, so the
        // temporary memory stays within its share with several batch
        // threads. The wait gives up on a pause or a cancel.
        let _large = if mode == Mode::Batch && bytes.len() > LARGE_FILE {
            let stop =
                || guard.is_some_and(|g| g.pause_requested()) || ctx.cancel.load(Ordering::Acquire);
            match self.large.acquire(stop) {
                Some(slot) => Some(slot),
                None if guard.is_some_and(|g| g.pause_requested()) => return Outcome::Paused,
                None => return Outcome::Skip,
            }
        } else {
            None
        };
        let deadline = match mode {
            Mode::Interactive => Deadline::interactive(&self.watchdog),
            Mode::Background => Deadline::background(&self.watchdog),
            Mode::Batch => {
                let deadline = Deadline::background(&self.watchdog);
                match guard {
                    Some(guard) => {
                        let guard = guard.clone();
                        deadline.with_pause(Arc::new(move || guard.pause_requested()))
                    }
                    None => deadline,
                }
            }
        };
        let extract = lock(&self.extract).clone();
        bump(&self.stats.extractions, 1);
        let extraction = extract(&item.lang, &bytes, &deadline, cancel);
        match extraction.status {
            ExtractStatus::Cancelled => {
                let paused = guard.is_some_and(|g| g.pause_requested())
                    && !ctx.cancel.load(Ordering::Acquire);
                if paused {
                    Outcome::Paused
                } else {
                    Outcome::Skip
                }
            }
            ExtractStatus::Unsupported => {
                lock(&self.langs).disabled.insert(item.lang.name().clone());
                Outcome::Skip
            }
            _ => Outcome::Done(
                Done {
                    item: item.clone(),
                    key,
                    extraction,
                    flag: cancel.clone(),
                },
                bytes.len(),
            ),
        }
    }

    /// Writes extractions in one transaction, after checking each was made
    /// with the current grammar; then sets the hints of every path with that
    /// content.
    ///
    /// A key stored meanwhile with a status that satisfies the item's tier
    /// (`satisfies`) is not written again, so a batch commit only adds
    /// units: an eviction during it is the quota's (`Batch::full`). Only a
    /// tier 1 commit replaces a unit (a `TimedOut` or `TooLarge` one), which
    /// the cache counts as an eviction; it never sets `full`.
    ///
    /// True when every item is now stored (written, or found stored).
    fn commit(&self, project: &Arc<Project>, ctx: &Ctx, done: Vec<Done>, batch: bool) -> bool {
        if done.is_empty() {
            return true;
        }
        let table = lock(&self.langs).table.clone();
        let live = |d: &Done| {
            !ctx.cancel.load(Ordering::Acquire)
                && !d.flag.load(Ordering::Acquire)
                && table
                    .get(&d.item.lang.name().0)
                    .is_some_and(|l| Version::of(l) == Version::of(&d.item.lang))
        };
        // Keys stored since the existence check: one query per version.
        // The touches keep the store's own eviction generation, the hints
        // the one without our replaces.
        let (before_raw, before) = (project.store.evictions(), project.evictions());
        let mut groups: Vec<(Version, Vec<BlobId>)> = Vec::new();
        for d in done.iter().filter(|d| live(d)) {
            let version = Version::of(&d.item.lang);
            match groups.iter_mut().find(|(v, _)| *v == version) {
                Some((_, keys)) => keys.push(d.key),
                None => groups.push((version, vec![d.key])),
            }
        }
        let mut found: Vec<(Version, Found)> = Vec::new();
        for (version, keys) in &groups {
            bump(&self.stats.exists_checks, 1);
            for f in project.store.exists(version, keys).unwrap_or_default() {
                found.push((version.clone(), f));
            }
        }
        let mut stored = Vec::new();
        let mut written = Vec::new();
        let mut hits = Vec::new();
        let (eviction, committed, after) = {
            let _turn = lock(&project.commits);
            let eviction = project.store.evictions();
            let mut write = project.store.write();
            let mut replaces = false;
            for d in done.iter().filter(|d| live(d)) {
                let version = Version::of(&d.item.lang);
                let stored_as = found.iter().find(|(v, f)| *v == version && f.oid == d.key);
                if let Some((_, f)) = stored_as.filter(|(_, f)| satisfies(d.item.tier, f.status)) {
                    hits.push(*f);
                    stored.push((d.key, version, d.item.tier));
                    continue;
                }
                if write.add(d.key, &version, &d.extraction) {
                    replaces |= stored_as.is_some();
                    written.push((d.key, version, d.item.tier));
                }
            }
            if replaces {
                project.begin_replace();
            }
            let committed = !write.is_empty() && matches!(write.commit(), Written::Committed);
            let after = project.store.evictions();
            // Under the lock nobody else changes `replaced`.
            let replaced = project.replaced.load(Ordering::Acquire);
            // Our own replace's one bump (one per transaction): hints and
            // cursors do not count it. Should the same transaction also
            // evict for the quota, that eviction is not seen either; its
            // units are extracted again when used or on the next open.
            let own = committed && replaces && after == eviction + 1;
            if own {
                project.replaced.fetch_add(1, Ordering::SeqCst);
            }
            if replaces {
                project.end_replace();
            }
            (
                eviction.saturating_sub(replaced),
                committed,
                after.saturating_sub(replaced + u64::from(own)),
            )
        };
        if !committed {
            written.clear();
        }
        let settled = stored.len() + written.len() == done.len();
        let mut st = lock(&project.state);
        if st.session == ctx.session {
            if committed {
                st.gens.writes += 1;
                if batch && after != eviction && st.batch.full.is_none() {
                    // Our own write evicted units: the namespace is at its
                    // quota. From now on an eviction does not restart the
                    // batch tiers' walks, or they would extract what they
                    // evicted and evict again; each file is extracted at
                    // most once per session.
                    let walked = st
                        .batch
                        .cursor
                        .as_ref()
                        .map_or(eviction, |c| c.deps.eviction);
                    st.batch.full = Some(walked);
                }
            }
            st.batch.touches.push(before_raw, &hits);
            // The units just written are the newest: they outlive the
            // eviction they caused.
            let sets = [(stored, st.stamp(before)), (written, st.stamp(after))];
            for (units, stamp) in sets {
                for (key, version, tier) in units {
                    if !current(&table, &st.map, &version) {
                        continue;
                    }
                    if tier == Tier::Other {
                        st.batch.others.confirmed.insert(&key, &version.lang);
                    }
                    for eff in st.map.paths(&key, &version.lang.0) {
                        st.map.hints.set(&eff.slot, &key, stamp);
                    }
                }
            }
        }
        for d in &done {
            st.finish(ctx.session, &d.item, &d.flag);
        }
        drop(st);
        if committed && let Some(me) = self.arc() {
            // A written batch makes a held name table behind.
            me.names_changed(project);
        }
        settled
    }

    /// Writes a query's extractions in a `Background` task, as tier 2 would:
    /// a key stored meanwhile is not written again, the hints of every path
    /// with that content are set once committed. Nothing for an ended
    /// session.
    pub(crate) fn store_scanned(
        &self,
        project: &Arc<Project>,
        session: u64,
        scanned: Vec<Scanned>,
    ) {
        if scanned.is_empty() {
            return;
        }
        let Some(me) = self.arc() else {
            return;
        };
        let p = project.clone();
        self.host
            .spawn_busy("store", Priority::Background, move || {
                let ctx = {
                    let st = lock(&p.state);
                    if !st.live(session) {
                        return;
                    }
                    ctx(&st)
                };
                let done = scanned
                    .into_iter()
                    .map(|s| Done {
                        item: Item {
                            path: s.path,
                            oid: s.oid,
                            lang: s.lang,
                            source: Source::Worktree,
                            tier: Tier::Review,
                        },
                        key: s.oid,
                        extraction: s.extraction,
                        flag: Arc::new(AtomicBool::new(false)),
                    })
                    .collect();
                me.commit(&p, &ctx, done, false);
            });
    }

    /// Queues the focus's files that are not indexed for tiers 1 and 2, and
    /// starts their tasks. Tier 1 needs `Done`, which a hint does not tell
    /// (it also stands for `TimedOut` and `TooLarge`): it checks each blob
    /// once per session (`Focused::satisfied`). Tier 2 trusts the hints.
    pub(crate) fn enqueue_focus(self: &Arc<Self>, project: &Arc<Project>) {
        let eviction = project.evictions();
        let disabled = lock(&self.langs).disabled.clone();
        let mut st = lock(&project.state);
        if st.base_for.is_none() || st.cancel.load(Ordering::Acquire) {
            return;
        }
        let wt = st.worktree().cloned();
        let stamp = st.stamp(eviction);
        let map = st.map.clone();
        let focus = st.focus.clone();
        let checked = st.focused.satisfied.clone();
        let satisfied = |oid: BlobId, lang: &Lang| checked.contains(&(oid, Version::of(lang)));
        let mut wanted: [Vec<Item>; 2] = Default::default();
        if let Some((w, text)) = &focus.current
            && *w == wt
        {
            match text.origin {
                TextOrigin::Worktree => {
                    if let Some(eff) = map.effective(&text.path)
                        && !satisfied(eff.oid, &eff.lang)
                    {
                        wanted[0].push(item(eff, Tier::Current));
                    }
                }
                TextOrigin::Object => {
                    let table = map.base.table().clone();
                    let mut lookup = LangLookup::new(&table, &*self.packs);
                    let code = lookup.code(&text.path);
                    if let Some(lang) = map.base.lang_of_code(code)
                        && !satisfied(text.oid, lang)
                    {
                        wanted[0].push(Item {
                            path: text.path.clone(),
                            oid: text.oid,
                            lang: lang.clone(),
                            source: Source::Object,
                            tier: Tier::Current,
                        });
                    }
                }
            }
        }
        let lists = [
            (&focus.drawer, 0, Tier::Current),
            (&focus.review, 1, Tier::Review),
            (&focus.touched, 1, Tier::Review),
        ];
        for (list, t, tier) in lists {
            let Some((w, paths)) = list else {
                continue;
            };
            if *w != wt {
                continue;
            }
            for path in paths.iter() {
                let Some(eff) = map.effective(path) else {
                    continue;
                };
                let indexed = if t == 0 {
                    satisfied(eff.oid, &eff.lang)
                } else {
                    cursor_hinted(&map, &eff, stamp)
                };
                if !indexed {
                    wanted[t].push(item(eff, tier));
                }
            }
        }
        for (t, items) in wanted.into_iter().enumerate() {
            for item in items {
                // Tiers 1 and 2 take a key over from a batch (`focus_task`);
                // tier 2 leaves one that tier 1 or 2 is extracting.
                let focused = st
                    .in_flight
                    .get(&item.key())
                    .is_some_and(|o| o.tier.batch().is_none());
                if disabled.contains(item.lang.name()) || (t == 1 && focused) {
                    continue;
                }
                let queue = &mut st.focused.queue[t];
                if queue
                    .iter()
                    .any(|q| q.path == item.path && q.oid == item.oid)
                {
                    continue;
                }
                queue.push_back(item);
            }
        }
        drop(st);
        self.kick_focus(project);
    }

    /// Starts tier 1 (one task) and tier 2 (up to two) tasks for queued
    /// files.
    fn kick_focus(self: &Arc<Self>, project: &Arc<Project>) {
        for (t, limit, priority) in [(0, 1, Priority::Interactive), (1, 2, Priority::Background)] {
            loop {
                let session = {
                    let mut st = lock(&project.state);
                    if st.cancel.load(Ordering::Acquire) {
                        return;
                    }
                    let f = &mut st.focused;
                    if f.running[t] >= limit || f.queue[t].len() <= f.running[t] {
                        break;
                    }
                    f.running[t] += 1;
                    st.session
                };
                let me = self.clone();
                let p = project.clone();
                let name = if t == 0 { "tier1" } else { "tier2" };
                if !self
                    .host
                    .spawn_busy(name, priority, move || me.focus_task(&p, t, session))
                {
                    let mut st = lock(&project.state);
                    if st.session == session {
                        st.focused.running[t] -= 1;
                    }
                    break;
                }
            }
        }
    }

    /// A tier 1 or 2 task: takes files from its queue until it is empty.
    fn focus_task(self: &Arc<Self>, project: &Arc<Project>, t: usize, session: u64) {
        loop {
            let (item, ctx, flag) = {
                let mut st = lock(&project.state);
                if !st.live(session) {
                    // Ended, or a new session (which counts its own tasks):
                    // nothing more is read.
                    return;
                }
                let Some(item) = st.focused.queue[t].pop_front() else {
                    st.focused.running[t] -= 1;
                    return;
                };
                let c = ctx(&st);
                let key = item.key();
                let version = Version::of(&item.lang);
                if t == 0 && st.focused.satisfied.contains(&(item.oid, version)) {
                    // Once per session (`enqueue_focus`).
                    continue;
                }
                if let Some(other) = st.in_flight.get(&key) {
                    // Tiers 1 and 2 never wait on batch work (ADR 0029):
                    // they cancel the extraction in flight and own the key.
                    // Tier 1 also takes one over from tier 2 (the
                    // interactive limits); tier 2 leaves tier 1's and
                    // tier 2's to them.
                    let batch = other.tier.batch().is_some();
                    if t == 1 && !batch {
                        continue;
                    }
                    other.flag.store(true, Ordering::Release);
                }
                let flag = Arc::new(AtomicBool::new(c.cancel.load(Ordering::Acquire)));
                let owner = Owner {
                    flag: flag.clone(),
                    tier: item.tier,
                };
                st.in_flight.insert(key, owner);
                (item, c, flag)
            };
            let (before_raw, before) = (project.store.evictions(), project.evictions());
            let version = Version::of(&item.lang);
            bump(&self.stats.exists_checks, 1);
            let found = project
                .store
                .exists(&version, &[item.oid])
                .unwrap_or_default();
            if found.iter().any(|f| satisfies(item.tier, f.status)) {
                let table = lock(&self.langs).table.clone();
                let mut st = lock(&project.state);
                if st.finish(session, &item, &flag) {
                    if t == 0 {
                        st.focused.satisfied.insert((item.oid, version.clone()));
                    }
                    st.batch.touches.push(before_raw, &found);
                    if current(&table, &st.map, &version) {
                        let stamp = st.stamp(before);
                        for eff in st.map.paths(&item.oid, &item.lang.name().0) {
                            st.map.hints.set(&eff.slot, &item.oid, stamp);
                        }
                    }
                }
                continue;
            }
            // Not stored, or stored `TimedOut` or `TooLarge` and opened: tier
            // 1 extracts it again with the interactive limits.
            let mode = if t == 0 {
                Mode::Interactive
            } else {
                Mode::Background
            };
            match self.extract_item(&ctx, &item, mode, None, &flag) {
                Outcome::Done(d, _) => {
                    // Settled for the session only once a result is stored:
                    // a file that could not be read is tried again.
                    if self.commit(project, &ctx, vec![d], false) && t == 0 {
                        let mut st = lock(&project.state);
                        if st.session == session {
                            st.focused.satisfied.insert((item.oid, version));
                        }
                    }
                }
                Outcome::Paused | Outcome::Skip => {
                    lock(&project.state).finish(session, &item, &flag);
                }
            }
        }
    }
}

/// Whether a stored unit of status `status` satisfies `tier`: an opened
/// file (tier 1) needs `Done`, or `Binary`, which no limit changes; the
/// other tiers take any stored status.
fn satisfies(tier: Tier, status: ExtractStatus) -> bool {
    tier != Tier::Current || !matches!(status, ExtractStatus::TimedOut | ExtractStatus::TooLarge)
}

/// Whether `version` is the version of its language both in the current
/// language table and in the table `map` was built with: only then may a
/// unit stored under it stamp the map's hints.
fn current(table: &LanguageTable, map: &crate::map::LiveMap, version: &Version) -> bool {
    let of = |l: Option<&Lang>| l.is_some_and(|l| Version::of(l) == *version);
    of(table.get(&version.lang.0)) && of(map.lang(&version.lang.0))
}

/// `current` for the version a group of `exists` queried `lang` under.
fn same_version(
    table: &LanguageTable,
    map: &crate::map::LiveMap,
    lang: &crate::LangName,
    groups: &[(Version, Vec<Item>)],
) -> bool {
    groups
        .iter()
        .find(|(v, _)| v.lang == *lang)
        .is_some_and(|(v, _)| current(table, map, v))
}

fn item(eff: crate::map::Effective, tier: Tier) -> Item {
    Item {
        path: eff.path,
        oid: eff.oid,
        lang: eff.lang,
        source: Source::Worktree,
        tier,
    }
}

fn ctx(st: &State) -> Ctx {
    Ctx {
        session: st.session,
        cancel: st.cancel.clone(),
        root: st.root.clone(),
        tree: st.tree.clone(),
    }
}

fn cursor_hinted(map: &crate::map::LiveMap, eff: &crate::map::Effective, stamp: HintStamp) -> bool {
    map.hints.matches(&eff.slot, &eff.oid, stamp)
}

/// Tier 3: the deepest sub-project containing the current text, else the
/// review set's first file, else the path the project was opened at (when
/// not its root). The whole worktree without sub-projects; empty when no
/// sub-project contains the anchor or there is none.
pub(super) fn subproject(st: &State) -> Scope {
    let Some(packages) = st.seen.packages.as_ref().filter(|p| !p.is_empty()) else {
        return Scope::Whole;
    };
    let wt = st.worktree();
    let opened = st
        .snapshot
        .as_ref()
        .and_then(|s| s.active.as_ref())
        .map(|a| a.opened_at.clone())
        .filter(|p| !p.is_empty());
    let anchor = st
        .focus
        .current
        .as_ref()
        .filter(|(w, _)| w.as_ref() == wt)
        .map(|(_, t)| t.path.clone())
        .or_else(|| {
            st.focus
                .review
                .as_ref()
                .filter(|(w, _)| w.as_ref() == wt)
                .and_then(|(_, p)| p.first().cloned())
        })
        .or(opened);
    let Some(anchor) = anchor else {
        return Scope::Empty;
    };
    match packages
        .iter()
        .filter(|p| under(&anchor, &p.dir))
        .max_by_key(|p| p.dir.len())
    {
        Some(p) if p.dir.is_empty() => Scope::Whole,
        Some(p) => Scope::Dir(p.dir.clone()),
        None => Scope::Empty,
    }
}
