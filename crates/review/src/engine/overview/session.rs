//! The sessions against the count set (「概览」 steps 2–5): which files
//! each claims (edits after its cutoffs, commands by mtime), the changes
//! outside any session, and per shown session its progress, its Drifted
//! signal from `review.attribution` and its critical count.

use lectern_git::{Oid, ReflogEntry, file_change_of};

use super::round::count_limits;
use super::*;
use crate::engine::round::ends;
use crate::log::Index;

pub(super) struct Input<'a> {
    pub engine: &'a Engine,
    pub git: &'a Git,
    pub count: &'a Arc<ChangeSet>,
    pub reflog: &'a HeadReflog,
    pub sessions: &'a [Known],
    pub window_ms: i64,
    pub window_s: i64,
    pub cancel: &'a AtomicBool,
}

/// One session's base and cutoffs, and the files it claims.
struct Per {
    base_oid: Option<Oid>,
    /// Its base is the current HEAD: its files are the count set's.
    head_base: bool,
    records: Vec<Record>,
    floor: Option<i64>,
    commit_time: Option<i64>,
    /// Indices into the count set's files, edit claims first.
    claimed: Vec<usize>,
    /// How many of `claimed` are edit claims (the rest by command).
    edits: usize,
}

impl Per {
    /// Whether an edit of the session claims `f`: on either of its paths,
    /// from that path's cutoff on.
    fn claims_file(
        &self,
        k: &Known,
        table: &cutoff::Table,
        window_ms: i64,
        f: &FileChange,
    ) -> bool {
        let (start, end) = ends(f);
        k.claims.calls.iter().any(|call| match &call.kind {
            ClaimKind::Edit { paths, at, .. } => paths
                .iter()
                .any(|p| (*p == start || *p == end) && at.0 >= self.cutoff_ms(table, window_ms, p)),
            ClaimKind::Command { .. } => false,
        })
    }

    fn cutoff_ms(&self, table: &cutoff::Table, window_ms: i64, path: &ChangePath) -> i64 {
        table
            .cutoff(&self.records, self.floor, path)
            .or(self.commit_time)
            .map_or(i64::MIN, |s| s.saturating_mul(1000))
            .max(window_ms)
    }
}

pub(super) fn compute(input: &Input<'_>) -> (Vec<SessionOverview>, OutsideCount) {
    let (engine, git) = (input.engine, input.git);
    let files: Vec<FileChange> = input.count.files().iter().map(file_change_of).collect();
    let mut by_path: HashMap<ChangePath, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        let (start, end) = ends(f);
        for p in [start, end] {
            let list = by_path.entry(p).or_default();
            if !list.contains(&i) {
                list.push(i);
            }
        }
    }
    let paths: HashSet<ChangePath> = by_path.keys().cloned().collect();
    // The whole window's records: one table answers every session.
    let (all_records, _) = cutoff::window(&input.reflog.entries, 0, input.window_s, &mut |_| false);
    let table = engine.table(git, &all_records, &paths, None, input.cancel);
    let head = head_commit(&git.worktree);
    let hash = git.hash;

    // Each session's cutoffs and edit claims.
    let mut per: Vec<Per> = Vec::with_capacity(input.sessions.len());
    for k in input.sessions {
        let id = SessionId {
            source: k.key.source.0.clone(),
            id: k.key.id.clone(),
        };
        let recorded = engine.recorded_base(git, &id);
        let started_s = k.started_at.0.div_euclid(1000);
        let (base, _, mode) =
            session_base(recorded.as_ref(), input.reflog, started_s, hash, || head);
        let base_oid = base.commit.and_then(|c| oid_of(hash, &c));
        let mut leaves = |e: &ReflogEntry| match (base_oid, e.new) {
            (Some(b), Some(n)) => !engine.descends(git, b, n, input.cancel),
            _ => false,
        };
        let (records, floor) = match mode {
            CutoffMode::Records(i) => {
                cutoff::window(&input.reflog.entries, i, input.window_s, &mut leaves)
            }
            CutoffMode::CommitTime(_) => (Vec::new(), None),
        };
        let commit_time = match mode {
            CutoffMode::CommitTime(Some(c)) => git.repo.commit(c).ok().map(|i| i.committed.seconds),
            _ => None,
        };
        let mut p = Per {
            base_oid,
            head_base: base_oid == head,
            records,
            floor,
            commit_time,
            claimed: Vec::new(),
            edits: 0,
        };
        let mut seen_files: HashSet<usize> = HashSet::new();
        for call in k.claims.calls.iter() {
            let ClaimKind::Edit { paths, at, .. } = &call.kind else {
                continue;
            };
            for path in paths.iter() {
                let Some(indices) = by_path.get(path) else {
                    continue;
                };
                if at.0 < p.cutoff_ms(&table, input.window_ms, path) {
                    continue;
                }
                for &i in indices {
                    if seen_files.insert(i) {
                        p.claimed.push(i);
                    }
                }
            }
        }
        p.edits = p.claimed.len();
        per.push(p);
    }

    // Command claims for the files no edit took: the one session whose
    // command ran when the file changed (as a review decides it).
    let edited: HashSet<usize> = per.iter().flat_map(|p| p.claimed.iter().copied()).collect();
    let bare: Vec<usize> = (0..files.len()).filter(|i| !edited.contains(i)).collect();
    let bare_paths: Vec<RepoPath> = bare
        .iter()
        .filter_map(|&i| RepoPath::new(&files[i].path.0))
        .collect();
    if !bare_paths.is_empty() {
        let times = git.worktree.mtimes(&bare_paths).unwrap_or_default();
        let with_path: Vec<usize> = bare
            .iter()
            .copied()
            .filter(|&i| RepoPath::new(&files[i].path.0).is_some())
            .collect();
        for (&i, time) in with_path.iter().zip(times) {
            let f = &files[i];
            let Some(mtime) = time.map(|t| t.div_euclid(1_000_000)) else {
                continue;
            };
            let hits: Vec<usize> = input
                .sessions
                .iter()
                .enumerate()
                .filter(|(s, k)| {
                    mtime >= per[*s].cutoff_ms(&table, input.window_ms, &f.path)
                        && k.claims.calls.iter().any(|c| {
                            matches!(c.kind, ClaimKind::Command { from, to } if from.0 <= mtime && mtime <= to.0)
                        })
                })
                .map(|(s, _)| s)
                .collect();
            if let [s] = hits[..] {
                per[s].claimed.push(i);
            }
        }
    }

    let claimed_any: HashSet<usize> = per.iter().flat_map(|p| p.claimed.iter().copied()).collect();
    let outside = OutsideCount::Files((files.len() - claimed_any.len()) as u32);

    // The shown sessions: newest first (the input's order), with a claim.
    let order: Vec<usize> = (0..input.sessions.len())
        .filter(|&s| !per[s].claimed.is_empty())
        .take(MAX_SHOWN)
        .collect();
    // The IO first, with no lock held: the other-base comparisons (at
    // most `MAX_OTHER_BASES`, newest first) and the attribution cache.
    let mut other_bases = 0;
    let own_sets: Vec<Option<Vec<FileChange>>> = order
        .iter()
        .map(|&s| {
            let (k, p) = (&input.sessions[s], &per[s]);
            if p.head_base || other_bases >= MAX_OTHER_BASES {
                return None;
            }
            other_bases += 1;
            // Its files are those of its own base comparison that its
            // edits claim (the review's file set), not the count set's.
            let base = p.base_oid?;
            let set = git
                .worktree
                .changes(Base::Commit(base), &count_limits(), input.cancel)
                .ok()?;
            Some(
                set.files()
                    .iter()
                    .map(file_change_of)
                    .filter(|f| p.claims_file(k, &table, input.window_ms, f))
                    .collect(),
            )
        })
        .collect();
    let drifted = drifted_of(input, &per, &order, &files, &table);
    // The seen states, under the log's lock only for as long as they are
    // read: no IO here (`command` takes this lock on the UI thread).
    let log = engine.loaded_log(git.common_dir(), diag::overview_ctx(git.common_dir()));
    let progress: Vec<Option<Progress>> = match log.as_ref().map(|l| lock(&l.mem)) {
        None => vec![None; order.len()],
        Some(mem) => order
            .iter()
            .zip(&own_sets)
            .map(|(&s, own_set)| {
                let (k, p) = (&input.sessions[s], &per[s]);
                let target_key = target_key(&ReviewTarget::Session(k.key.clone()), git);
                if p.head_base {
                    let own = p.claimed.iter().map(|&i| &files[i]);
                    Some(progress_of(&mem.index, &target_key, own))
                } else {
                    own_set
                        .as_ref()
                        .map(|set| progress_of(&mem.index, &target_key, set.iter()))
                }
            })
            .collect(),
    };
    let shown = order
        .iter()
        .enumerate()
        .map(|(n, &s)| {
            let (k, p) = (&input.sessions[s], &per[s]);
            let critical = p
                .claimed
                .iter()
                .map(|&i| &files[i])
                .filter(|f| noise::classify(&f.path.0, f.generated).is_none())
                .fold((0u32, false), |(n, deleted), f| {
                    let critical = noise::sensitive(f).is_some_and(sensitive_is_critical);
                    let gone = matches!(f.status, ChangeStatus::Deleted) && !f.untracked;
                    (n + u32::from(critical), deleted || gone)
                });
            SessionOverview {
                session: k.key.clone(),
                progress: progress[n],
                drifted: drifted[n],
                critical: critical.0 + u32::from(critical.1),
            }
        })
        .collect();
    (shown, outside)
}

/// The seen progress of `files` for the target, from the log's index.
fn progress_of<'a>(
    index: &Index,
    target: &seen::TargetKey,
    files: impl Iterator<Item = &'a FileChange>,
) -> Progress {
    let states: Vec<_> = files
        .map(|f| seen::state(index, &SeenKey::new(target.clone(), f)))
        .collect();
    seen::progress(&states)
}

/// For each shown session, whether a file it edited has Drifted lines:
/// only from `review.attribution` (one `get_many`), None when any chain
/// of its files is not cached.
fn drifted_of(
    input: &Input<'_>,
    per: &[Per],
    order: &[usize],
    files: &[FileChange],
    table: &cutoff::Table,
) -> Vec<Option<bool>> {
    let git = input.git;
    let hash = git.hash;
    // (session position in `order`, key) for every edit-claimed file.
    let mut keys: Vec<lectern_cache::Key> = Vec::new();
    let mut owners: Vec<usize> = Vec::new();
    let mut unknown: Vec<bool> = vec![false; order.len()];
    for (n, &s) in order.iter().enumerate() {
        let p = &per[s];
        for &i in &p.claimed[..p.edits] {
            let f = &files[i];
            let (start, end) = ends(f);
            let cut = p.cutoff_ms(table, input.window_ms, &start).max(p.cutoff_ms(
                table,
                input.window_ms,
                &end,
            ));
            let steps = steps_of(input.sessions, s, &start, &end, cut);
            let paths = if start == end {
                vec![start]
            } else {
                vec![start, end]
            };
            let digest = history::digest(&steps, &paths);
            match codec::key(
                hash,
                side_oid(f.old.as_ref()),
                f.new.as_ref().map(|side| &side.content),
                &digest,
            ) {
                Some(key) => {
                    keys.push(key);
                    owners.push(n);
                }
                None => unknown[n] = true,
            }
        }
    }
    let mut drifted: Vec<Option<bool>> = vec![Some(false); order.len()];
    if !keys.is_empty() {
        let partition = input.engine.cfg.cache.repo(git.common_dir());
        let found = partition.get_many::<CachedChain>(&keys);
        for (n, hit) in owners.iter().zip(found) {
            match hit {
                Some(CachedChain(chain)) => {
                    if chain_drifted(&chain) {
                        drifted[*n] = Some(true);
                    }
                }
                None => unknown[*n] = true,
            }
        }
    }
    for (n, unknown) in unknown.iter().enumerate() {
        if *unknown && drifted[n] != Some(true) {
            drifted[n] = None;
        }
    }
    drifted
}

/// Lines with no source, or base lines removed outside the chain.
fn chain_drifted(chain: &crate::chain::Chain) -> bool {
    match &chain.detail {
        Detail::Lines(lines) => {
            lines.worktree.iter().any(|s| matches!(s, Source::None))
                || lines.base.iter().any(|f| matches!(f, Fate::Outside))
        }
        Detail::FileOnly { .. } => false,
    }
}

/// The history steps of file `i` as the review of session `s` orders
/// them: every session's edits of its paths from the cutoff on, by time,
/// then the review's session order (`s` first, the others newest first),
/// then the call's position.
fn steps_of<'a>(
    sessions: &'a [Known],
    s: usize,
    start: &ChangePath,
    end: &ChangePath,
    cut: i64,
) -> Vec<HistoryStep<'a>> {
    let order = |w: usize| {
        if w == s {
            0
        } else if w < s {
            w + 1
        } else {
            w
        }
    };
    let mut steps: Vec<(Timestamp, usize, u64, HistoryStep<'a>)> = Vec::new();
    for (w, k) in sessions.iter().enumerate() {
        for call in k.claims.calls.iter() {
            let ClaimKind::Edit {
                paths,
                at,
                status,
                input,
                result,
            } = &call.kind
            else {
                continue;
            };
            if at.0 < cut || !paths.iter().any(|p| p == start || p == end) {
                continue;
            }
            steps.push((
                *at,
                order(w),
                call.position(),
                HistoryStep {
                    session: &k.key,
                    adapter_version: k.claims.adapter_version,
                    turn: &call.turn,
                    call: &call.call,
                    status: *status,
                    input: *input,
                    result: *result,
                },
            ));
        }
    }
    steps.sort_by_key(|(at, order, position, _)| (*at, *order, *position));
    steps.into_iter().map(|(_, _, _, step)| step).collect()
}
