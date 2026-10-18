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
use lectern_services::project::{BlobId, ProjectKey, RelPath};

use super::host::yield_now;
use super::state::{Cursor, Item, OtherTree, Project, Scope, Source, State, Tier, Walk, under};
use super::{Inner, bump, lock};
use crate::extract::{
    BACKGROUND_MAX_LEN, Deadline, ExtractStatus, Extraction, INTERACTIVE_MAX_LEN, Lang,
};
use crate::git_access::{Tree, hash_blob, hash_kind};
use crate::map::{HintStamp, LangLookup};
use crate::store::{EXISTS_BATCH, Version};
use crate::util::log;

/// A batch ends after this many files…
pub const BATCH_FILES: usize = 64;
/// …or this many bytes read…
pub const BATCH_BYTES: usize = 2 << 20;
/// …or at the first file boundary after this long.
pub const BATCH_TIME: Duration = Duration::from_millis(50);
/// Entries a gathering step looks at, at most, under the project's lock.
const SCAN_LIMIT: usize = 4096;

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

/// One extraction ready to be written.
struct Done {
    item: Item,
    /// The OID of the bytes actually read (「存储表」).
    key: BlobId,
    extraction: Extraction,
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
    /// Makes sure a batch task runs (one per batch thread at most).
    pub(crate) fn kick_batch(self: &Arc<Self>) {
        {
            let mut run = lock(&self.batch);
            if run.running >= self.batch_tasks {
                run.kicked = true;
                return;
            }
            run.running += 1;
        }
        let me = self.clone();
        let spawned = self.host.spawn_batch("batch", move |guard| me.batch_loop(guard));
        if !spawned {
            lock(&self.batch).running -= 1;
        }
    }

    async fn batch_loop(self: Arc<Self>, guard: Arc<BackgroundGuard>) {
        loop {
            guard.proceed().await;
            match self.batch_step(&guard) {
                Step::Worked => yield_now().await,
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

    fn batch_project(&self) -> Option<Arc<Project>> {
        let key: Option<ProjectKey> = lock(&self.batch_project).clone();
        self.project(&key?)
    }

    /// One batch of tiers 3–5.
    fn batch_step(self: &Arc<Self>, guard: &Arc<BackgroundGuard>) -> Step {
        let Some(project) = self.batch_project() else {
            return Step::Idle;
        };
        let eviction = project.store.evictions();
        let (wide, ctx, stamp) = {
            let mut st = lock(&project.state);
            if st.base_for.is_none() || st.cancel.load(Ordering::Acquire) {
                return Step::Idle;
            }
            self.ensure_cursor(&mut st, eviction);
            let stamp = HintStamp {
                eviction,
                grammar: st.gens.grammar,
            };
            (self.wide_allowed(&st), ctx(&st), stamp)
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
                match self.gather_other(&project, &ctx, eviction) {
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

    /// Rebuilds tier 3 and 4's cursor when its dependencies changed.
    fn ensure_cursor(&self, st: &mut State, eviction: u64) {
        let deps = st.cursor_deps(eviction);
        if st.batch.cursor.as_ref().is_some_and(|c| c.deps == deps) {
            return;
        }
        bump(&self.stats.cursor_builds, 1);
        let scope = subproject(st);
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
        for (key, _) in st.in_flight.iter() {
            st.batch.queued.insert(key.clone());
        }
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
        let tier = if t == 0 { Tier::Subproject } else { Tier::Worktree };
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
        eviction: u64,
    ) -> Option<Vec<Item>> {
        loop {
            let next = {
                let mut st = lock(&project.state);
                let deps = Some((st.session, eviction, st.gens.grammar));
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
                if st.session != ctx.session {
                    return None;
                }
                match loaded {
                    Ok((root, entries)) => {
                        let other = Arc::new(OtherTree {
                            key: key.clone(),
                            root,
                        });
                        st.batch.others.current = Some((other, entries.into(), 0));
                    }
                    Err(error) => {
                        log(format_args!("reading worktree {}: {error}", tree.root.display()));
                        st.batch.others.done.insert(key);
                    }
                }
                continue;
            }
            let table = lock(&self.langs).table.clone();
            let disabled = lock(&self.langs).disabled.clone();
            let mut lookup = LangLookup::new(&table, &*self.packs);
            let mut st = lock(&project.state);
            let State {
                batch, in_flight, ..
            } = &mut *st;
            let (other, entries, pos) = batch.others.current.as_mut()?;
            let mut items = Vec::new();
            while *pos < entries.len() && items.len() < EXISTS_BATCH {
                let (path, oid) = &entries[*pos];
                *pos += 1;
                let code = lookup.code(path);
                let Some(lang) = code.checked_sub(1).and_then(|i| table.langs().get(usize::from(i)))
                else {
                    continue;
                };
                if disabled.contains(lang.name()) {
                    continue;
                }
                let key = (*oid, lang.name().clone());
                if batch.queued.contains(&key) || in_flight.contains_key(&key) {
                    continue;
                }
                batch.queued.insert(key);
                items.push(Item {
                    path: path.clone(),
                    oid: *oid,
                    lang: lang.clone(),
                    source: Source::Other(other.clone()),
                    tier: Tier::Other,
                });
            }
            if *pos >= entries.len() {
                let key = other.key.clone();
                batch.others.current = None;
                batch.others.done.insert(key);
            }
            if items.is_empty() {
                continue;
            }
            return Some(items);
        }
    }

    /// The existence check of gathered items: hits set their hints (and
    /// those of every path with the same content), misses wait in their
    /// tier's queue.
    fn exists(&self, project: &Arc<Project>, ctx: &Ctx, items: Vec<Item>, stamp: HintStamp) {
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
        let mut st = lock(&project.state);
        if st.session != ctx.session {
            return;
        }
        st.batch.touches.push(stamp.eviction, &found);
        for (oid, lang) in &hits {
            for eff in st.map.paths(oid, &lang.0) {
                st.map.hints.set(&eff.slot, oid, stamp);
            }
        }
        for (_, group) in groups {
            for item in group {
                let key = item.key();
                if hits.contains(&key) {
                    st.batch.queued.remove(&key);
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
                    st.in_flight.remove(&item.key());
                    st.batch.active[t] -= 1;
                    if st.session == ctx.session {
                        st.batch.resume[t].push_front(item);
                    }
                    break;
                }
                Outcome::Skip => self.release(project, ctx, &item),
            }
        }
        #[cfg(test)]
        lock(&self.batch_log).push(files);
        self.commit(project, ctx, done);
    }

    /// Takes the next item of tier `t`'s queue and registers it in flight.
    fn take(&self, project: &Arc<Project>, ctx: &Ctx, t: usize) -> Option<(Item, Arc<AtomicBool>)> {
        let mut st = lock(&project.state);
        if st.session != ctx.session {
            return None;
        }
        loop {
            let item = st.batch.resume[t].pop_front()?;
            let key = item.key();
            if st.in_flight.contains_key(&key) {
                st.batch.queued.remove(&key);
                continue;
            }
            let flag = Arc::new(AtomicBool::new(ctx.cancel.load(Ordering::Acquire)));
            st.in_flight.insert(key, flag.clone());
            st.batch.active[t] += 1;
            return Some((item, flag));
        }
    }

    /// Forgets an item that gave nothing to write.
    fn release(&self, project: &Arc<Project>, ctx: &Ctx, item: &Item) {
        let mut st = lock(&project.state);
        let key = item.key();
        st.in_flight.remove(&key);
        if st.session == ctx.session {
            st.batch.queued.remove(&key);
        }
        if let Some(t) = item.tier.batch() {
            st.batch.active[t] -= 1;
        }
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
                },
                bytes.len(),
            ),
        }
    }

    /// Writes extractions in one transaction, after checking each was made
    /// with the current grammar; then sets the hints of every path with that
    /// content.
    fn commit(&self, project: &Arc<Project>, ctx: &Ctx, done: Vec<Done>) {
        if done.is_empty() {
            return;
        }
        let (table, grammar) = {
            let langs = lock(&self.langs);
            (langs.table.clone(), langs.generation)
        };
        let eviction = project.store.evictions();
        let current = |lang: &Lang| {
            table
                .get(&lang.name().0)
                .is_some_and(|l| Version::of(l) == Version::of(lang))
        };
        let mut write = project.store.write();
        let mut written = Vec::new();
        for d in &done {
            if ctx.cancel.load(Ordering::Acquire) || !current(&d.item.lang) {
                continue;
            }
            if write.add(d.key, &Version::of(&d.item.lang), &d.extraction) {
                written.push((d.key, d.item.lang.name().clone()));
            }
        }
        let committed = !write.is_empty() && matches!(write.commit(), Written::Committed);
        let mut st = lock(&project.state);
        if st.session == ctx.session && committed {
            st.gens.writes += 1;
            if st.gens.grammar == grammar {
                let stamp = HintStamp { eviction, grammar };
                for (key, lang) in &written {
                    for eff in st.map.paths(key, &lang.0) {
                        st.map.hints.set(&eff.slot, key, stamp);
                    }
                }
            }
        }
        for d in &done {
            let key = d.item.key();
            st.in_flight.remove(&key);
            if st.session == ctx.session {
                st.batch.queued.remove(&key);
            }
            if let Some(t) = d.item.tier.batch() {
                st.batch.active[t] -= 1;
            }
        }
    }

    /// Queues the focus's files that are not indexed for tiers 1 and 2, and
    /// starts their tasks.
    pub(crate) fn enqueue_focus(self: &Arc<Self>, project: &Arc<Project>) {
        let eviction = project.store.evictions();
        let disabled = lock(&self.langs).disabled.clone();
        let mut st = lock(&project.state);
        if st.base_for.is_none() {
            return;
        }
        let wt = st.worktree().cloned();
        let stamp = HintStamp {
            eviction,
            grammar: st.gens.grammar,
        };
        let map = st.map.clone();
        let focus = st.focus.clone();
        let mut wanted: [Vec<Item>; 2] = Default::default();
        if let Some((w, text)) = &focus.current
            && *w == wt
        {
            match text.origin {
                TextOrigin::Worktree => {
                    if let Some(eff) = map.effective(&text.path) {
                        if !cursor_hinted(&map, &eff, stamp) {
                            wanted[0].push(item(eff, Tier::Current));
                        }
                    }
                }
                TextOrigin::Object => {
                    let table = map.base.table().clone();
                    let mut lookup = LangLookup::new(&table, &*self.packs);
                    let code = lookup.code(&text.path);
                    if let Some(lang) = map.base.lang_of_code(code) {
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
                if let Some(eff) = map.effective(path)
                    && !cursor_hinted(&map, &eff, stamp)
                {
                    wanted[t].push(item(eff, tier));
                }
            }
        }
        for (t, items) in wanted.into_iter().enumerate() {
            for item in items {
                if disabled.contains(item.lang.name()) || st.in_flight.contains_key(&item.key()) {
                    continue;
                }
                let queue = &mut st.focused.queue[t];
                if queue.iter().any(|q| q.path == item.path && q.oid == item.oid) {
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
                    lock(&project.state).focused.running[t] -= 1;
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
                let item = if st.session == session {
                    st.focused.queue[t].pop_front()
                } else {
                    None
                };
                let Some(item) = item else {
                    st.focused.running[t] -= 1;
                    return;
                };
                let c = ctx(&st);
                let key = item.key();
                if st.in_flight.contains_key(&key) {
                    continue;
                }
                let flag = Arc::new(AtomicBool::new(c.cancel.load(Ordering::Acquire)));
                st.in_flight.insert(key, flag.clone());
                (item, c, flag)
            };
            let eviction = project.store.evictions();
            let version = Version::of(&item.lang);
            bump(&self.stats.exists_checks, 1);
            let found = project.store.exists(&version, &[item.oid]).unwrap_or_default();
            if !found.is_empty() {
                let mut st = lock(&project.state);
                st.in_flight.remove(&item.key());
                if st.session == session {
                    let stamp = HintStamp {
                        eviction,
                        grammar: st.gens.grammar,
                    };
                    st.batch.touches.push(eviction, &found);
                    for eff in st.map.paths(&item.oid, &item.lang.name().0) {
                        st.map.hints.set(&eff.slot, &item.oid, stamp);
                    }
                }
                continue;
            }
            let mode = if t == 0 {
                Mode::Interactive
            } else {
                Mode::Background
            };
            match self.extract_item(&ctx, &item, mode, None, &flag) {
                Outcome::Done(d, _) => self.commit(project, &ctx, vec![d]),
                Outcome::Paused | Outcome::Skip => {
                    lock(&project.state).in_flight.remove(&item.key());
                }
            }
        }
    }
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
fn subproject(st: &State) -> Scope {
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
