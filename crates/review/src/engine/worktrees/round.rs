//! One round of the worktrees overview, on `Background`: what the marks
//! made stale is found, then the worktrees are compared one after another
//! (the current one first), each result stored and published before the
//! next comparison starts. The round ends when nothing is stale, or only
//! what the throttle holds back (a wake is then asked for), or when the
//! interests went.

use lectern_services::project::ProjectSnapshot;
use lectern_services::session::Place;

use super::compute::{Computed, Job, Outcome, Source, hash_path};
use super::state::{BaseRef, Entry};
use super::*;
use crate::engine::overview::Counted;
use crate::engine::round::blob_id;

#[derive(Default)]
struct Stats {
    worktrees: usize,
    compared: u32,
    capped: u32,
}

enum Step {
    Continue,
    End,
}

/// What a step found to do.
enum Pick {
    /// The list of worktrees changed: publish before anything else.
    Publish(ReviewEvent),
    Compute(Box<Job>),
    /// Marks came in while the step ran: take them first.
    Again,
    /// Nothing is stale (or the throttle holds it back until `.0`).
    Idle(Option<i64>),
}

/// What a step knows when it picks: the marks and what they made stale,
/// the overview's count, the time.
struct Stale<'a> {
    needs: &'a HashMap<WorktreeKey, Need>,
    marks: &'a Marks,
    counted: Counted,
    now: Timestamp,
    cancel: &'a Arc<AtomicBool>,
}

/// The gap the throttle keeps after a comparison that took `took`.
pub(in crate::engine) fn gap_ms(took: Duration) -> i64 {
    MIN_GAP_MS.max(GAP_FACTOR.saturating_mul(took.as_millis() as i64))
}

impl Engine {
    pub(super) fn worktrees_round(&self, project: &ProjectKey, seq: u64) {
        let started = Instant::now();
        let mut stats = Stats::default();
        self.ensure_worktrees_watch(project, seq);
        while let Step::Continue = self.worktrees_step(project, seq, &mut stats) {}
        debug!("worktrees round";
            worktrees = stats.worktrees,
            compared = stats.compared,
            capped = stats.capped,
            took_us = started.elapsed());
    }

    /// Subscribes to the repository, once per state.
    fn ensure_worktrees_watch(&self, project: &ProjectKey, seq: u64) {
        let (Some(watcher), ProjectKey::Git { common_dir }) = (&self.cfg.watcher, project) else {
            return;
        };
        let wanted = lock(&self.worktrees)
            .projects
            .get(project)
            .is_some_and(|s| s.round == seq && s.watch.is_none());
        if !wanted {
            return;
        }
        let watch = sink::start(watcher, &self.me, project, common_dir);
        let spare = match lock(&self.worktrees).projects.get_mut(project) {
            Some(state) if state.watch.is_none() => {
                state.watch = Some(watch);
                None
            }
            _ => Some(watch),
        };
        drop(spare);
    }

    /// Ends the round the state is running (nothing more to do).
    fn worktrees_end(&self, project: &ProjectKey, seq: u64) {
        if let Some(state) = lock(&self.worktrees).projects.get_mut(project)
            && state.round == seq
        {
            state.running = false;
        }
    }

    /// Puts marks a step took but could not use back.
    fn worktrees_restore(&self, project: &ProjectKey, marks: Marks) {
        if let Some(state) = lock(&self.worktrees).projects.get_mut(project) {
            state.marks.merge(marks);
        }
    }

    fn worktrees_step(&self, project: &ProjectKey, seq: u64, stats: &mut Stats) -> Step {
        // 1. The marks.
        let (marks, cancel) = {
            let mut states = lock(&self.worktrees);
            let Some(state) = states
                .projects
                .get_mut(project)
                .filter(|s| s.round == seq && s.interests > 0)
            else {
                return Step::End;
            };
            (std::mem::take(&mut state.marks), Arc::clone(&state.cancel))
        };
        // 2. The worktrees now.
        let Some(snapshot) = (self.cfg.projects)().and_then(|p| p.snapshot(project)) else {
            self.worktrees_restore(project, marks);
            self.worktrees_end(project, seq);
            return Step::End;
        };
        let active = snapshot.active.as_ref().and_then(|a| a.worktree.clone());
        // 3. What the marks made stale, by worktree.
        let needs = self.worktrees_needs(project, &snapshot, active.as_ref(), &marks);
        // 4. The overview's count, 5. what to do now.
        let counted = self.overview_counted(project);
        let now = (self.cfg.clock)();
        let pick = {
            let mut states = lock(&self.worktrees);
            let Some(state) = states
                .projects
                .get_mut(project)
                .filter(|s| s.round == seq && s.interests > 0)
            else {
                if let Some(state) = states.projects.get_mut(project) {
                    state.marks.merge(marks);
                }
                return Step::End;
            };
            let stale = Stale {
                needs: &needs,
                marks: &marks,
                counted,
                now,
                cancel: &cancel,
            };
            self.worktrees_pick(project, state, &snapshot, active.as_ref(), stale)
        };
        stats.worktrees = snapshot.worktrees.iter().filter(|w| !w.prunable).count();
        match pick {
            Pick::Publish(event) => {
                (self.cfg.emit)(event);
                Step::Continue
            }
            Pick::Again => Step::Continue,
            Pick::Idle(wake) => {
                if let Some(at) = wake {
                    self.worktrees_wake_at(project, at);
                }
                Step::End
            }
            Pick::Compute(job) => self.worktrees_compute_and_store(project, seq, &job, stats),
        }
    }

    /// The stale parts the marks name, by worktree key. Reads the session
    /// lists (cheap: the sources hold them) but does no IO.
    fn worktrees_needs(
        &self,
        project: &ProjectKey,
        snapshot: &ProjectSnapshot,
        active: Option<&WorktreeKey>,
        marks: &Marks,
    ) -> HashMap<WorktreeKey, Need> {
        let mut needs: HashMap<WorktreeKey, Need> = HashMap::new();
        let mut add =
            |key: &WorktreeKey, need: Need| needs.entry(key.clone()).or_default().merge(need);
        for (name, need) in &marks.names {
            for w in snapshot.worktrees.iter().filter(|w| !w.prunable) {
                let is = match name {
                    WorktreeName::Main => w.is_main,
                    WorktreeName::Linked(n) => {
                        !w.is_main && w.key.0.file_name().is_some_and(|f| f == &**n)
                    }
                };
                if is {
                    add(&w.key, *need);
                }
            }
        }
        for (key, need) in &marks.keys {
            add(key, *need);
        }
        if marks.active
            && let Some(active) = active
        {
            add(active, Need::UNCOMMITTED);
        }
        if marks.sessions_all {
            for w in snapshot.worktrees.iter().filter(|w| !w.prunable) {
                add(&w.key, Need::UNCOMMITTED);
            }
        } else if !marks.sessions.is_empty() {
            for source in (self.cfg.sources)() {
                let list = source.sessions();
                for s in list.sessions.iter() {
                    if !marks.sessions.contains(&s.key) {
                        continue;
                    }
                    let worktree = match &s.place {
                        Place::Located(l) if l.project == *project => l.worktree.as_ref(),
                        Place::Resolved(r) if r.project == *project => r.worktree.as_ref(),
                        _ => None,
                    };
                    if let Some(worktree) = worktree {
                        add(worktree, Need::UNCOMMITTED);
                    }
                }
            }
        }
        needs
    }

    /// Applies the marks to the state and picks the next worktree to
    /// compare (under the lock; no IO).
    fn worktrees_pick(
        &self,
        project: &ProjectKey,
        state: &mut WtState,
        snapshot: &ProjectSnapshot,
        active: Option<&WorktreeKey>,
        stale: Stale<'_>,
    ) -> Pick {
        let Stale {
            needs,
            marks,
            counted,
            now,
            cancel,
        } = stale;
        let listed: Vec<WorktreeKey> = snapshot
            .worktrees
            .iter()
            .filter(|w| !w.prunable)
            .map(|w| w.key.clone())
            .collect();
        let listed_set: HashSet<&WorktreeKey> = listed.iter().collect();
        let mut changed = state.order != listed;
        let before = state.entries.len();
        state.entries.retain(|k, _| listed_set.contains(k));
        changed |= state.entries.len() != before;
        state
            .pairs
            .retain(|(a, b), _| listed_set.contains(a) && listed_set.contains(b));
        state.order = listed;

        if marks.refs || marks.all {
            state.base_stale = true;
        }
        for e in state.entries.values_mut() {
            if marks.all {
                e.need = Need::BOTH;
            } else if marks.refs {
                e.need.merge(Need::COMMITTED);
            }
        }
        for (key, need) in needs {
            if let Some(e) = state.entries.get_mut(key) {
                e.need.merge(*need);
            }
        }
        if changed && state.snapshot.is_some() {
            return Pick::Publish(publish(project, state));
        }

        // The current worktree first, the rest in the project's order.
        let mut order: Vec<&WorktreeKey> = state.order.iter().collect();
        if let Some(active) = active
            && let Some(i) = order.iter().position(|k| *k == active)
        {
            let current = order.remove(i);
            order.insert(0, current);
        }
        let mut wait: Option<i64> = None;
        for key in order {
            let (need, prev, due) = match state.entries.get(key) {
                None => (Need::BOTH, None, None),
                Some(e) if e.need.any() => (
                    e.need,
                    Some((e.head, e.merge_base)),
                    e.finished.map(|f| f.0.saturating_add(gap_ms(e.took))),
                ),
                Some(_) => continue,
            };
            let source = Source::of(counted.clone(), key, active == Some(key));
            let costly = need.committed || (need.uncommitted && !source.is_free());
            if costly
                && let Some(due) = due
                && due > now.0
            {
                wait = Some(wait.map_or(due, |w| w.min(due)));
                continue;
            }
            return Pick::Compute(Box::new(Job {
                project: project.clone(),
                key: key.clone(),
                need,
                source,
                prev,
                resolve_base: state.base_stale || !state.base_known,
                base: state.base.clone(),
                cancel: Arc::clone(cancel),
            }));
        }
        if !state.marks.is_empty() {
            // Marked while this step ran: the next step takes them.
            return Pick::Again;
        }
        state.running = false;
        match wait {
            Some(at) if state.wake != Some(at) => {
                state.wake = Some(at);
                Pick::Idle(Some(at))
            }
            _ => Pick::Idle(None),
        }
    }

    /// Compares one worktree, stores the result and publishes it.
    fn worktrees_compute_and_store(
        &self,
        project: &ProjectKey,
        seq: u64,
        job: &Job,
        stats: &mut Stats,
    ) -> Step {
        let began = Instant::now();
        let outcome = self.worktrees_compute(job);
        let took = began.elapsed();
        let now = (self.cfg.clock)();
        let event = {
            let mut states = lock(&self.worktrees);
            let Some(state) = states
                .projects
                .get_mut(project)
                .filter(|s| s.round == seq && s.interests > 0)
            else {
                // Superseded: the worktree stays marked.
                return Step::End;
            };
            let (computed, base) = match outcome {
                Outcome::Cancelled => {
                    state.running = false;
                    return Step::End;
                }
                Outcome::Unchanged { base } => (None, base),
                Outcome::Done { computed, base } => (Some(computed), base),
            };
            let mut changed = false;
            if let Some(resolved) = base {
                state.base_known = true;
                state.base_stale = false;
                if state.base != resolved {
                    state.base = resolved;
                    changed = true;
                    for (k, e) in state.entries.iter_mut() {
                        if *k != job.key {
                            e.need.merge(Need::COMMITTED);
                        }
                    }
                }
            }
            match computed {
                None => {
                    if let Some(e) = state.entries.get_mut(&job.key) {
                        e.need = Need::default();
                        e.finished = Some(now);
                        e.took = took;
                    }
                }
                Some(c) => {
                    changed = true;
                    stats.compared += 1;
                    if c.vs_base == ChangeCount::Capped || c.uncommitted == ChangeCount::Capped {
                        stats.capped += 1;
                    }
                    store(state, &job.key, c, now, took);
                }
            }
            if changed {
                Some(publish(project, state))
            } else {
                None
            }
        };
        if let Some(event) = event {
            (self.cfg.emit)(event);
        }
        Step::Continue
    }
}

/// Stores a worktree's counts and recomputes its overlaps with the others.
fn store(state: &mut WtState, key: &WorktreeKey, c: Computed, now: Timestamp, took: Duration) {
    let mut hashes: Option<Vec<u64>> = c
        .paths
        .as_ref()
        .map(|paths| paths.iter().map(|p| hash_path(p.as_bytes())).collect());
    if let Some(h) = &mut hashes {
        h.sort_unstable();
        h.dedup();
    }
    let complete = c.uncommitted != ChangeCount::Pending;
    let counted_at = if complete {
        Some(now)
    } else {
        state.entries.get(key).and_then(|e| e.counted_at)
    };
    state.entries.insert(
        key.clone(),
        Entry {
            head: c.head,
            merge_base: c.merge_base,
            uncommitted: c.uncommitted,
            committed: c.committed,
            vs_base: c.vs_base,
            counted_at,
            finished: Some(now),
            took,
            hashes: hashes.map(Arc::from),
            paths_known: c.paths.is_some(),
            need: Need::default(),
        },
    );
    // The overlaps with every other worktree, from this side's paths.
    let others: Vec<WorktreeKey> = state
        .entries
        .keys()
        .filter(|k| *k != key)
        .cloned()
        .collect();
    for other in others {
        let pair_key = if *key < other {
            (key.clone(), other.clone())
        } else {
            (other.clone(), key.clone())
        };
        let Some(paths) = &c.paths else {
            state.pairs.remove(&pair_key);
            continue;
        };
        let theirs = state.entries.get(&other);
        match theirs {
            Some(Entry {
                hashes: Some(theirs),
                ..
            }) => {
                let mut files = 0u32;
                let mut samples: Vec<RelPath> = Vec::new();
                for p in paths {
                    if theirs.binary_search(&hash_path(p.as_bytes())).is_ok() {
                        files += 1;
                        if samples.len() < MAX_SAMPLES {
                            samples.push(RelPath::from(String::from_utf8_lossy(p.as_bytes())));
                        }
                    }
                }
                if files == 0 {
                    state.pairs.remove(&pair_key);
                } else {
                    state.pairs.insert(
                        pair_key,
                        Pair {
                            files,
                            paths: Arc::from(samples),
                        },
                    );
                }
            }
            // Its hashes were let go: the overlap stays as it was until it
            // is compared again.
            Some(e) if e.paths_known => {}
            _ => {
                state.pairs.remove(&pair_key);
            }
        }
    }
}

/// The next snapshot of the state, at the next generation.
fn publish(project: &ProjectKey, state: &mut WtState) -> ReviewEvent {
    state.generation += 1;
    let mut by_key: HashMap<&WorktreeKey, Vec<(&WorktreeKey, &Pair)>> = HashMap::new();
    for ((a, b), pair) in &state.pairs {
        by_key.entry(a).or_default().push((b, pair));
        by_key.entry(b).or_default().push((a, pair));
    }
    let worktrees: Vec<WorktreeChanges> = state
        .order
        .iter()
        .map(|key| match state.entries.get(key) {
            Some(e) => WorktreeChanges {
                worktree: key.clone(),
                head: e.head.map(blob_id),
                uncommitted: e.uncommitted,
                committed: e.committed,
                vs_base: e.vs_base,
                overlaps: e.paths_known.then(|| {
                    let mut list = by_key.get(key).cloned().unwrap_or_default();
                    list.sort_by(|x, y| y.1.files.cmp(&x.1.files).then_with(|| x.0.cmp(y.0)));
                    list.truncate(MAX_OVERLAPS);
                    list.into_iter()
                        .map(|(other, pair)| Overlap {
                            other: other.clone(),
                            files: pair.files,
                            paths: Arc::clone(&pair.paths),
                        })
                        .collect()
                }),
                counted_at: e.counted_at,
            },
            None => WorktreeChanges {
                worktree: key.clone(),
                head: None,
                uncommitted: ChangeCount::Pending,
                committed: ChangeCount::Pending,
                vs_base: ChangeCount::Pending,
                overlaps: None,
                counted_at: None,
            },
        })
        .collect();
    state.snapshot = Some(Arc::new(WorktreesOverview {
        project: project.clone(),
        generation: state.generation,
        base: state.base.as_ref().map(|b: &BaseRef| Arc::clone(&b.name)),
        worktrees: worktrees.into(),
    }));
    ReviewEvent::Worktrees {
        project: project.clone(),
        generation: state.generation,
    }
}
