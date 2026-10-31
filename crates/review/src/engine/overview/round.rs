//! One round of an overview, on `Background`: the current worktree, the
//! count comparison, the window's sessions and their claims, then
//! `session::compute` for each, and the publication.

use super::*;

/// `Limits` of the count comparison (「概览」 step 1, ADR 0035): nothing
/// retained, every file listed.
pub(in crate::engine) fn count_limits() -> Limits {
    Limits {
        retained_bytes: 0,
        total_patch_bytes: 0,
        ..Limits::default()
    }
}

/// What the round found, handed to the publication.
struct Found {
    git: Arc<Git>,
    count: Option<Arc<ChangeSet>>,
    sessions: Vec<Known>,
    shown: Vec<SessionOverview>,
    outside: OutsideCount,
    /// Today's changed files by sub-project (`Overview::packages`).
    packages: Option<Arc<[(RelPath, u32)]>>,
}

/// The modification time (nanoseconds) of every file of the count set in
/// the worktree, in the set's order: None for a deleted or unreadable
/// file. One stat per file, no content (`Worktree::mtimes`).
fn file_mtimes(git: &Git, count: &ChangeSet) -> Vec<Option<i64>> {
    let paths: Vec<RepoPath> = count.files().iter().map(|c| c.path.clone()).collect();
    match git.worktree.mtimes(&paths) {
        Ok(times) if times.len() == paths.len() => times,
        Ok(_) => vec![None; paths.len()],
        Err(e) => {
            let (error, io) = diag::git_error(&e);
            warn!("modification times unreadable"; error = error, io = io);
            vec![None; paths.len()]
        }
    }
}

impl Engine {
    pub(super) fn overview_round(&self, project: &ProjectKey, seq: u64, trigger: Trigger) {
        let started = Instant::now();
        let Some((prev_git, prev_count, cancel)) = ({
            let overviews = lock(&self.overviews);
            overviews
                .projects
                .get(project)
                .filter(|s| s.round == seq)
                .map(|s| (s.git.clone(), s.count.clone(), s.cancel.clone()))
        }) else {
            return;
        };
        // 1. The current worktree, and its sub-projects when scanned.
        let snapshot = (self.cfg.projects)().and_then(|p| p.snapshot(project));
        let active = snapshot
            .as_ref()
            .and_then(|s| s.active.as_ref().and_then(|a| a.worktree.clone()));
        let packages = snapshot.as_ref().and_then(|s| match s.active.as_ref() {
            Some(ActiveTree {
                packages: Scan::Ready(packages),
                ..
            }) => Some(Arc::clone(packages)),
            _ => None,
        });
        let Some(worktree) = active else {
            // A folder project, or no worktree yet: nothing to compare.
            self.publish_overview(project, seq, None, started);
            return;
        };
        let git = match prev_git {
            Some(g) if g.key == worktree && !trigger.resolve => g,
            _ => match self.git(project, &worktree) {
                Ok(g) => g,
                Err(_) => {
                    self.publish_overview(project, seq, None, started);
                    return;
                }
            },
        };
        let ctx = diag::overview_ctx(git.common_dir());
        let _scope = lectern_logging::scope(ctx);
        let same_git = prev_git_is(&prev_count, &git, project, self);
        self.ensure_overview_watch(project, &git, ctx);

        // 2. The count comparison.
        let compared = match (prev_count, same_git, trigger.full, trigger.paths.is_empty()) {
            (Some(prev), true, false, true) => Ok(prev),
            (Some(prev), true, false, false) => {
                let paths: Vec<RepoPath> = trigger
                    .paths
                    .iter()
                    .filter_map(|p| RepoPath::new(&p.0))
                    .collect();
                git.worktree.refresh(&prev, Refresh::Paths(&paths), &cancel)
            }
            (Some(prev), true, true, _) => git.worktree.refresh(&prev, Refresh::Full, &cancel),
            _ => git.worktree.changes(Base::Head, &count_limits(), &cancel),
        };
        let count = match compared {
            Ok(count) => count,
            Err(GitError::Cancelled) => return,
            Err(e) => {
                let (error, io) = diag::git_error(&e);
                warn!("overview comparison failed"; error = error, io = io);
                self.publish_overview(project, seq, None, started);
                return;
            }
        };
        if count.files().len() > MAX_PATHS {
            let found = Found {
                git,
                count: None,
                sessions: Vec::new(),
                shown: Vec::new(),
                outside: OutsideCount::Capped(count.files().len() as u32),
                packages: None,
            };
            self.publish_overview(project, seq, Some(found), started);
            return;
        }

        // 3. The window's sessions and their claims.
        let now = (self.cfg.clock)();
        let window_ms = now.0.saturating_sub(cutoff::WINDOW_MS);
        let window_s = window_ms.div_euclid(1000);
        let reflog = git
            .worktree
            .head_reflog(window_s, MAX_REFLOG)
            .unwrap_or_else(|e| {
                let (error, io) = diag::git_error(&e);
                warn!("head reflog unreadable; the base is the current head"; error = error, io = io);
                HeadReflog {
                    exists: false,
                    entries: Vec::new(),
                    complete: true,
                }
            });
        let summaries = other_sessions(self, &git.key, &[], window_ms);
        let sessions = self.overview_claims(&git, &summaries);
        if cancel.load(Ordering::Acquire) {
            return;
        }

        // 4. Each session against the count set. The files' workspace-side
        // modification times are read once: the command claims and the
        // per-package counts of today's changes both use them.
        let mtimes = file_mtimes(&git, &count);
        let input = session::Input {
            engine: self,
            git: &git,
            count: &count,
            mtimes: &mtimes,
            reflog: &reflog,
            sessions: &sessions,
            window_ms,
            window_s,
            cancel: &cancel,
        };
        let (shown, outside) = session::compute(&input);
        let today = packages.map(|packages| {
            let zone = &*self.cfg.utc_offset;
            packages::today(&packages, &count, &mtimes, now.0, zone)
        });
        let found = Found {
            git,
            count: Some(count),
            sessions,
            shown,
            outside,
            packages: today,
        };
        self.publish_overview(project, seq, Some(found), started);
    }

    /// The claims of the window's sessions: memory, then `review.claims`,
    /// then derived from `turns_unretained` and written back (never
    /// `turns`: the overview runs in the sampled state, ADR 0035).
    fn overview_claims(&self, git: &Git, summaries: &[SessionSummary]) -> Vec<Known> {
        let keys: Vec<lectern_cache::Key> = summaries.iter().map(|s| claims::key(&s.key)).collect();
        let in_memory: Vec<Option<Arc<Claims>>> = summaries
            .iter()
            .map(|s| self.memory_claims(&s.key, s.last_activity))
            .collect();
        let mut cached: Vec<Option<CachedClaims>> = Vec::new();
        if in_memory.iter().any(Option::is_none) {
            cached = self.cfg.cache.app().get_many::<CachedClaims>(&keys);
        }
        let mut out = Vec::with_capacity(summaries.len());
        let mut batch = self.cfg.cache.app().batch();
        let mut writes = 0;
        for (n, s) in summaries.iter().enumerate() {
            let rel = Arc::new(relative(git, s));
            let adapter = self.adapter(&s.key.source);
            let claims = match &in_memory[n] {
                Some(c) => Some(Arc::clone(c)),
                None => match cached.get_mut(n).and_then(Option::take) {
                    Some(CachedClaims(c)) if c.valid(s, adapter) => Some(Arc::new(c)),
                    _ => {
                        let Some(source) = source_of(self, &s.key) else {
                            continue;
                        };
                        let index = match source.turns_unretained(&s.key) {
                            Ok(index) => index,
                            Err(e) => {
                                debug!("claims not derived";
                                    from = diag::session(&s.key),
                                    error = diag::session_error(&e));
                                continue;
                            }
                        };
                        if let Some(v) = index.cursor.adapter_version() {
                            self.adapter_seen(&s.key.source, v);
                        }
                        let c = Arc::new(Claims::derive(&index, &rel, s.last_activity));
                        if !s.tail_pending {
                            batch.put(&keys[n], &CachedClaims((*c).clone()));
                            writes += 1;
                        }
                        self.remember_claims(&s.key, &c);
                        Some(c)
                    }
                },
            };
            if let Some(claims) = claims {
                out.push(Known {
                    key: s.key.clone(),
                    started_at: s.started_at,
                    claims,
                });
            }
        }
        if writes > 0 {
            // A failed write only means deriving them again.
            let _ = batch.commit();
        }
        out
    }

    /// Subscribes the overview to the worktree, once per worktree.
    fn ensure_overview_watch(&self, project: &ProjectKey, git: &Git, ctx: Ctx) {
        let Some(watcher) = &self.cfg.watcher else {
            return;
        };
        let wanted = lock(&self.overviews)
            .projects
            .get(project)
            .is_some_and(|s| s.watch.is_none() || s.git.as_ref().is_none_or(|g| g.key != git.key));
        if !wanted {
            return;
        }
        let watching = sink::start(watcher, &self.me, project, git);
        let spare = {
            let mut overviews = lock(&self.overviews);
            match overviews.projects.get_mut(project) {
                Some(state) => {
                    state.ctx = ctx;
                    state.watch.replace(watching)
                }
                None => Some(watching),
            }
        };
        if spare.is_some() {
            let ctx = lock(&self.overviews)
                .projects
                .get(project)
                .map_or(ctx, |s| s.ctx);
            self.submit(Priority::Background, ctx, move |_| drop(spare));
        }
    }

    /// Installs the round's result, unless a newer round started or the
    /// interests went; emits `ReviewEvent::Overview`.
    fn publish_overview(
        &self,
        project: &ProjectKey,
        seq: u64,
        found: Option<Found>,
        started: Instant,
    ) {
        let event = {
            let mut overviews = lock(&self.overviews);
            let Some(state) = overviews.projects.get_mut(project) else {
                return;
            };
            if state.round != seq {
                return;
            }
            state.generation += 1;
            let (sessions, outside, packages) = match found {
                Some(found) => {
                    state.git = Some(found.git);
                    state.count = found.count;
                    state.sessions = found.sessions;
                    (found.shown, found.outside, found.packages)
                }
                None => {
                    state.git = None;
                    state.count = None;
                    state.sessions = Vec::new();
                    (Vec::new(), OutsideCount::Pending, None)
                }
            };
            debug!("overview round";
                sessions = state.sessions.len(),
                shown = sessions.len(),
                outside = match outside {
                    OutsideCount::Pending => 0,
                    OutsideCount::Files(n) | OutsideCount::Capped(n) => n,
                },
                capped = matches!(outside, OutsideCount::Capped(_)),
                packages = packages.as_ref().map(|p| p.len()),
                took_us = started.elapsed());
            state.snapshot = Some(Arc::new(Overview {
                project: project.clone(),
                generation: state.generation,
                sessions: sessions.into(),
                outside,
                packages,
            }));
            let current = state.git.as_ref().map(|g| g.key.clone());
            (
                ReviewEvent::Overview {
                    project: project.clone(),
                    generation: state.generation,
                },
                current,
            )
        };
        let (event, current) = event;
        (self.cfg.emit)(event);
        // The worktrees overview takes the current worktree's
        // uncommitted count from this set (「并行 worktree 总览」). A round
        // without a set (it failed, or there is no worktree) leaves the
        // current worktree to be compared there, as when the interest goes.
        match current {
            Some(current) => self.worktrees_uncommitted_changed(project, current),
            None => self.worktrees_current_unsettled(project),
        }
        self.overview_round_ended(project);
    }
}

/// Whether the count set of the previous round is of this worktree.
fn prev_git_is(
    prev_count: &Option<Arc<ChangeSet>>,
    git: &Git,
    project: &ProjectKey,
    engine: &Engine,
) -> bool {
    prev_count.is_some()
        && lock(&engine.overviews)
            .projects
            .get(project)
            .and_then(|s| s.git.as_ref())
            .is_some_and(|g| g.key == git.key)
}
