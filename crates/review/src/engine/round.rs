//! One computation round of a review (docs/design/review.md「数据流与线程」):
//! the target and its worktree, the base, the change set, the claims of
//! every session in the worktree, the paths' cutoffs, each file's history
//! of change steps, the review's file set and the narrative's inputs. The
//! attribution of the files (`attribute`) follows: the first file on the
//! same task, the rest in the background.
//!
//! Blocking throughout (git, the cache, session reads): executor tasks
//! only.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lectern_cache::Key;
use lectern_git::{HashKind, HeadReflog, HeadState, LocalChanges, Oid, Repo, Worktree};
use lectern_logging::{Ctx, debug, info, warn};
use lectern_services::changes::{
    BaseSpec, ChangeError, ChangeList, ChangePath, ChangeSource, ChangeStatus, FileChange,
    RefreshHint, SideIdentity,
};
use lectern_services::project::{BlobId, PackageMatch, ProjectKey, RelPath, WorktreeKey};
use lectern_services::review::{
    BaseHow, BaseInfo, Degraded, FileKey, Group, GroupKind, NoiseKind, ReviewError, ReviewTarget,
    SessionRef, StepId, TurnRef,
};
use lectern_services::session::{
    Place, SessionError, SessionKey, SessionSource, SessionSummary, Timestamp, ToolCallId,
    ToolKind, TurnItem,
};

use super::narrative::{self, Own, Placement};
use super::{Engine, Held};
use crate::chain::attribute::Owner;
use crate::claims::{self, CachedClaims, ClaimKind, Claims, Relative};
use crate::cutoff::{self, Record, Table};
use crate::diag;
use crate::group::{self, GroupInput};
use crate::log::SessionId;
use crate::noise;
use crate::seen::{SeenKey, TargetKey, file_key};

/// At most this many other sessions' claims (「改动步与声明」).
pub(crate) const MAX_SESSIONS: usize = 100;
/// HEAD reflog records read at most.
const MAX_REFLOG: usize = 4096;
/// Per-file content limit of the change chain sides.
pub(crate) const MAX_TEXT: u64 = crate::chain::MAX_VERSION_BYTES;

/// How a round gets its change set.
#[derive(Clone, Debug)]
pub(crate) enum Kind {
    /// `ChangeSource::fetch`: opening.
    Fetch,
    /// `ChangeSource::refresh` on the previous list.
    Refresh(RefreshHint),
    /// The previous list again: claims or cutoffs became known since.
    Reuse,
}

/// The git side of a worktree, kept per worktree by the engine.
pub(crate) struct Git {
    pub project: ProjectKey,
    pub key: WorktreeKey,
    pub root: PathBuf,
    pub repo: Repo,
    pub worktree: Worktree,
    pub hash: HashKind,
    /// The worktree's git directory relative to the common directory, as
    /// the annotation log writes it ("" for the main worktree).
    pub log_name: Arc<str>,
}

impl Git {
    pub(crate) fn common_dir(&self) -> &std::path::Path {
        &self.repo.location().common_dir
    }
}

/// A session whose claims take part, the target's first.
pub(crate) struct Who {
    pub key: SessionKey,
    /// The order among sessions at the same time (「改动步与声明」): the
    /// target's sessions, then the others newest first.
    pub order: usize,
    /// Its index in the snapshot's `sessions` when it belongs to the target.
    pub own: Option<u16>,
    pub rel: Arc<Relative>,
    pub claims: Arc<Claims>,
}

/// One change step of a path's history: `who`'s call `call`.
#[derive(Clone, Debug)]
pub(crate) struct Hist {
    pub who: u32,
    pub call: u32,
    /// The chain's group (narrative step) key.
    pub group: u32,
}

/// One file of the snapshot.
pub(crate) struct FileWork {
    /// Index into the change list.
    pub list: u32,
    pub key: FileKey,
    pub seen: SeenKey,
    pub noise: Option<NoiseKind>,
    pub group: GroupKind,
    pub package: Option<RelPath>,
    /// Its change steps, in time order.
    pub history: Arc<[Hist]>,
    /// Claimed through commands only: (who, calls).
    pub command: Option<(u32, Vec<u32>)>,
    /// The unrecognized target session claimed it by time.
    pub by_time: bool,
}

/// The data of a narrative step, for its rows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StepWork {
    pub file: u32,
    pub session: u16,
    pub ordinal: u32,
    pub group: u32,
}

/// Stable step IDs across a review's rounds.
#[derive(Default, Debug)]
pub(crate) struct StepIds {
    next: u32,
    ids: HashMap<(SessionKey, Arc<str>, FileKey), StepId>,
}

impl StepIds {
    fn id(&mut self, session: &SessionKey, turn: &Arc<str>, file: &FileKey) -> StepId {
        let next = &mut self.next;
        *self
            .ids
            .entry((session.clone(), turn.clone(), file.clone()))
            .or_insert_with(|| {
                *next += 1;
                StepId(*next)
            })
    }
}

/// What a round found.
pub(crate) struct Work {
    pub target: ReviewTarget,
    pub target_key: TargetKey,
    pub git: Arc<Git>,
    pub source: Arc<LocalChanges>,
    pub list: Arc<ChangeList>,
    pub base: BaseInfo,
    /// For a session target: its ID and the base record's time, for the
    /// log's `base` record.
    pub session: Option<(SessionId, Option<i64>)>,
    pub sessions: Arc<[SessionRef]>,
    pub own: Vec<Own>,
    pub placed: Vec<Option<Placement>>,
    pub order: Vec<(u16, usize)>,
    pub whos: Vec<Who>,
    pub files: Vec<FileWork>,
    /// Snapshot file indices by each of their paths.
    pub by_path: HashMap<ChangePath, Vec<u32>>,
    pub unclaimed: u32,
    pub degraded: Option<Degraded>,
    pub groups: Arc<[Group]>,
    pub focus_review: Arc<[RelPath]>,
    pub focus_touched: Arc<[RelPath]>,
    pub step_files: HashMap<(u16, ToolCallId), Vec<u32>>,
    pub command_files: HashMap<(u16, ToolCallId), Vec<u32>>,
    pub steps: HashMap<StepId, StepWork>,
    pub step_of: HashMap<(u16, u32, u32), StepId>,
    /// The narrative steps of each file (its snapshot index), by ID: where
    /// a review note on a step's version is looked for.
    pub steps_of: HashMap<u32, Vec<StepId>>,
    /// What is left for the background: cutoff records not computed yet,
    /// sessions whose claims are missing, sources whose adapter version
    /// this run has not seen.
    pub pending: Pending,
    /// The attribution of each file, as it is computed.
    pub results: Mutex<Vec<Option<Arc<super::attribute::FileResult>>>>,
    /// The log scope of the review's tasks: `review` and `session`.
    pub ctx: Ctx,
    /// How long the parts of `prepare` took (the `review ready` record).
    pub timing: Timing,
}

/// The parts of a round's `prepare`, for the log.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Timing {
    /// Resolving the target, the worktree's handles, the HEAD reflog.
    pub locate: Duration,
    /// The target's sessions' turn indexes.
    pub turns: Duration,
    /// The change set: fetch, refresh or the previous one.
    pub fetch: Duration,
    /// The other sessions' claims.
    pub claims: Duration,
    /// The cutoff window and table.
    pub cutoffs: Duration,
}

#[derive(Default)]
pub(crate) struct Pending {
    pub records: Vec<Record>,
    pub table_records: Vec<Record>,
    pub table_paths: HashSet<ChangePath>,
    pub missing: Vec<(SessionKey, Arc<Relative>, Timestamp, bool)>,
    pub unverified: Vec<SessionKey>,
}

impl Pending {
    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty() && self.missing.is_empty() && self.unverified.is_empty()
    }
}

/// How the base was found, for the cutoffs.
pub(crate) enum CutoffMode {
    /// The window's records from this entry back (`cutoff::window`; the
    /// number of entries when there is no base entry: the reflog starts
    /// after the base, or the recorded base has no record).
    Records(usize),
    /// No reflog: every path is cut off at the base commit's time.
    CommitTime(Option<Oid>),
}

/// The target resolved to a project, a worktree and its sessions.
struct Resolved {
    git: Arc<Git>,
    /// The target's sessions with their summaries; empty for the others.
    own: Vec<SessionSummary>,
}

pub(crate) fn source_of(engine: &Engine, key: &SessionKey) -> Option<Held<dyn SessionSource>> {
    (engine.cfg.sources)()
        .into_iter()
        .find(|s| s.id() == key.source)
}

fn resolve(engine: &Engine, target: &ReviewTarget) -> Result<Resolved, ReviewError> {
    match target {
        ReviewTarget::Session(key) => {
            let source = source_of(engine, key).ok_or(ReviewError::Missing)?;
            let list = source.sessions();
            let summary = list.get(key).ok_or(ReviewError::Missing)?.clone();
            let (project, worktree) = match &summary.place {
                Place::Located(loc) => (loc.project.clone(), loc.worktree.clone()),
                Place::Missing => return Err(ReviewError::Missing),
                Place::Pending | Place::Resolved(_) | Place::Unresolvable(_) => {
                    return Err(ReviewError::NotInOpenProject);
                }
            };
            let Some(worktree) = worktree else {
                // A folder project: no git, handled by the caller.
                return Err(ReviewError::NotInOpenProject);
            };
            let git = engine.git(&project, &worktree)?;
            // Its subagents in the same worktree, recursively.
            let mut own = vec![summary];
            let mut i = 0;
            while i < own.len() {
                let parent = own[i].key.clone();
                let mut children: Vec<SessionSummary> = list
                    .sessions
                    .iter()
                    .filter(|s| s.parent.as_ref().is_some_and(|p| p.session == parent))
                    .filter(|s| in_worktree(s, &worktree))
                    .cloned()
                    .collect();
                children.sort_by_key(|s| s.started_at);
                own.extend(children);
                i += 1;
            }
            Ok(Resolved { git, own })
        }
        ReviewTarget::Outside { project, worktree }
        | ReviewTarget::Range {
            project, worktree, ..
        } => Ok(Resolved {
            git: engine.git(project, worktree)?,
            own: Vec::new(),
        }),
    }
}

/// Whether a folder project's session: located, without a worktree.
pub(crate) fn folder_session(engine: &Engine, key: &SessionKey) -> Option<SessionSummary> {
    let source = source_of(engine, key)?;
    let list = source.sessions();
    let summary = list.get(key)?;
    match &summary.place {
        Place::Located(loc) if loc.worktree.is_none() => Some(summary.clone()),
        _ => None,
    }
}

pub(crate) fn in_worktree(s: &SessionSummary, worktree: &WorktreeKey) -> bool {
    matches!(&s.place, Place::Located(l) if l.worktree.as_ref() == Some(worktree))
}

pub(crate) fn relative(git: &Git, s: &SessionSummary) -> Relative {
    let rel = match &s.place {
        Place::Located(l) => l.rel.clone(),
        _ => Arc::from(""),
    };
    Relative::new(&git.root, &s.cwd, &rel)
}

pub(crate) fn head_commit(worktree: &Worktree) -> Option<Oid> {
    match worktree.head().ok()? {
        HeadState::Branch { commit, .. } => commit,
        HeadState::Detached { commit } => Some(commit),
    }
}

pub(crate) fn blob_id(oid: Oid) -> BlobId {
    BlobId::from_bytes(oid.as_bytes()).unwrap_or(BlobId::Sha1([0; 20]))
}

pub(crate) fn oid_of(hash: HashKind, id: &BlobId) -> Option<Oid> {
    let hex: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    Oid::from_hex(hash, &hex)
}

/// The session's base (「审阅对象与基准」): the recorded one, else the
/// reflog's last record not after the session started, else the oldest
/// record's old value, else the current HEAD.
pub(crate) fn session_base(
    recorded: Option<&crate::log::BaseRecord>,
    reflog: &HeadReflog,
    started_s: i64,
    hash: HashKind,
    head: impl FnOnce() -> Option<Oid>,
) -> (BaseInfo, Option<i64>, CutoffMode) {
    let entries = &reflog.entries;
    if let Some(r) = recorded {
        let none = CutoffMode::Records(entries.len());
        let mode = match r.entry_at {
            Some(t) => match entries.iter().position(|e| e.time.seconds <= t) {
                Some(i) => CutoffMode::Records(i),
                None => none,
            },
            None if reflog.exists => none,
            None => CutoffMode::CommitTime(r.commit.and_then(|c| oid_of(hash, &c))),
        };
        return (
            BaseInfo {
                commit: r.commit,
                how: BaseHow::Recorded,
            },
            r.entry_at,
            mode,
        );
    }
    if reflog.exists && !entries.is_empty() {
        if let Some(i) = entries.iter().position(|e| e.time.seconds <= started_s) {
            let e = &entries[i];
            return (
                BaseInfo {
                    commit: e.new.map(blob_id),
                    how: BaseHow::Reflog,
                },
                Some(e.time.seconds),
                CutoffMode::Records(i),
            );
        }
        let oldest = entries.last().and_then(|e| e.old);
        return (
            BaseInfo {
                commit: oldest.map(blob_id),
                how: BaseHow::BeforeLog,
            },
            None,
            CutoffMode::Records(entries.len()),
        );
    }
    let head = head();
    (
        BaseInfo {
            commit: head.map(blob_id),
            how: BaseHow::CurrentHead,
        },
        None,
        CutoffMode::CommitTime(head),
    )
}

fn change_error(e: ChangeError) -> ReviewError {
    if !matches!(e, ChangeError::Cancelled) {
        warn!("change set unavailable"; error = diag::change_error(&e));
    }
    ReviewError::Changes(e)
}

/// The round's first part: everything but the attribution.
pub(crate) fn prepare(
    engine: &Engine,
    target: &ReviewTarget,
    kind: &Kind,
    prev: Option<&Arc<Work>>,
    step_ids: &Mutex<StepIds>,
    budget: Option<Duration>,
    cancel: &AtomicBool,
) -> Result<Work, ReviewError> {
    let now = (engine.cfg.clock)();
    let window_ms = now.0.saturating_sub(cutoff::WINDOW_MS);
    let window_s = window_ms.div_euclid(1000);
    let mut timing = Timing::default();
    let mut lap = Instant::now();
    let resolved = resolve(engine, target)?;
    let git = resolved.git;
    let hash = git.hash;
    let target_key = target_key(target, &git);
    // The records from here on carry the review's `review` too.
    let ctx = diag::review_ctx(target, git.common_dir(), &target_key);
    let _scope = lectern_logging::scope(ctx);
    timing.locate = lap.elapsed();
    lap = Instant::now();

    // The target's own sessions and their turn indexes.
    let mut own: Vec<Own> = Vec::with_capacity(resolved.own.len());
    let mut degraded = None;
    for (i, summary) in resolved.own.into_iter().enumerate() {
        let source = source_of(engine, &summary.key).ok_or(ReviewError::Missing)?;
        let index = match source.turns(&summary.key) {
            Ok(index) => {
                if let Some(v) = index.cursor.adapter_version() {
                    engine.adapter_seen(&summary.key.source, v);
                }
                Some(index)
            }
            Err(SessionError::NotFound) if i == 0 => return Err(ReviewError::Missing),
            Err(e) => {
                warn!("session turns unavailable; reviewed without them";
                    from = diag::session(&summary.key),
                    error = diag::session_error(&e),
                    target = i == 0);
                if i == 0 {
                    degraded = Some(Degraded::UnrecognizedSession);
                }
                None
            }
        };
        own.push(Own { summary, index });
    }
    timing.turns = lap.elapsed();
    lap = Instant::now();

    // The base and the reflog.
    let started_s = own
        .first()
        .map_or(i64::MAX, |o| o.summary.started_at.0.div_euclid(1000));
    let stop = started_s.min(window_s);
    let reflog = git
        .worktree
        .head_reflog(stop, MAX_REFLOG)
        .unwrap_or_else(|e| {
            let (error, io) = diag::git_error(&e);
            warn!("head reflog unreadable; the base is the current head";
                error = error, io = io);
            HeadReflog {
                exists: false,
                entries: Vec::new(),
                complete: true,
            }
        });
    let (base, session, mode, spec) = match target {
        ReviewTarget::Session(key) => {
            let id = SessionId {
                source: key.source.0.clone(),
                id: key.id.clone(),
            };
            let recorded = engine.recorded_base(&git, &id);
            let (base, entry_at, mode) =
                session_base(recorded.as_ref(), &reflog, started_s, hash, || {
                    head_commit(&git.worktree)
                });
            let commit = base
                .commit
                .and_then(|c| oid_of(hash, &c))
                .unwrap_or_else(|| Oid::empty_tree(hash));
            (
                base,
                Some((id, entry_at)),
                mode,
                BaseSpec::Commit(blob_id(commit)),
            )
        }
        ReviewTarget::Outside { .. } | ReviewTarget::Range { .. } => {
            let head = head_commit(&git.worktree);
            let spec = match target {
                ReviewTarget::Range { base, .. } => base.clone(),
                _ => BaseSpec::Head,
            };
            let commit = match &spec {
                BaseSpec::Head => head.map(blob_id),
                BaseSpec::Commit(id) => Some(*id),
                BaseSpec::MergeBase(name) => merge_base(&git.repo, head, name, cancel),
            };
            // The base record of these is the newest one.
            let mode = if reflog.exists {
                CutoffMode::Records(0)
            } else {
                CutoffMode::CommitTime(head)
            };
            (
                BaseInfo {
                    commit,
                    how: BaseHow::Spec,
                },
                None,
                mode,
                spec,
            )
        }
    };

    timing.locate += lap.elapsed();
    lap = Instant::now();

    // The change set.
    let reuse = match (kind, prev) {
        (Kind::Reuse | Kind::Refresh(_), Some(prev)) if prev.source_spec() == &spec => Some(prev),
        _ => None,
    };
    let (source, list) = match (kind, reuse) {
        (Kind::Reuse, Some(prev)) => (Arc::clone(&prev.source), Arc::clone(&prev.list)),
        (Kind::Refresh(hint), Some(prev)) => {
            let list = prev
                .source
                .refresh(&prev.list, hint, cancel)
                .map_err(change_error)?;
            (Arc::clone(&prev.source), list)
        }
        _ => fetch(&git, target, spec, cancel)?,
    };
    timing.fetch = lap.elapsed();
    lap = Instant::now();

    // Claims: the target's from its indexes, the others' from memory or
    // `review.claims`.
    let mut whos: Vec<Who> = Vec::new();
    for (i, o) in own.iter().enumerate() {
        let rel = Arc::new(relative(&git, &o.summary));
        let claims = match &o.index {
            Some(index) => {
                let c = Arc::new(Claims::derive(index, &rel, o.summary.last_activity));
                engine.remember_claims(&o.summary.key, &c);
                c
            }
            None => Arc::new(Claims {
                last_activity: o.summary.last_activity,
                adapter_version: 0,
                calls: Arc::from(Vec::new()),
            }),
        };
        whos.push(Who {
            key: o.summary.key.clone(),
            order: i,
            own: Some(i as u16),
            rel,
            claims,
        });
    }
    let mut pending = Pending::default();
    let others = other_sessions(engine, &git.key, &own, window_ms);
    let keys: Vec<Key> = others.iter().map(|s| claims::key(&s.key)).collect();
    let mut cached: Vec<Option<CachedClaims>> = Vec::new();
    let in_memory: Vec<Option<Arc<Claims>>> = others
        .iter()
        .map(|s| engine.memory_claims(&s.key, s.last_activity))
        .collect();
    if in_memory.iter().any(Option::is_none) {
        cached = engine.cfg.cache.app().get_many::<CachedClaims>(&keys);
    }
    let mut unverified: HashSet<SessionKey> = HashSet::new();
    let (mut from_memory, mut from_cache, mut stale) = (0u32, 0u32, 0u32);
    for (n, s) in others.iter().enumerate() {
        let rel = Arc::new(relative(&git, s));
        let claims = match &in_memory[n] {
            Some(c) => {
                from_memory += 1;
                Some(Arc::clone(c))
            }
            None => {
                let adapter = engine.adapter(&s.key.source);
                match cached.get_mut(n).and_then(Option::take) {
                    Some(CachedClaims(c)) if c.valid(s, adapter) => {
                        if adapter.is_none() && unverified.insert(s.key.clone()) {
                            pending.unverified.push(s.key.clone());
                        }
                        from_cache += 1;
                        Some(Arc::new(c))
                    }
                    found => {
                        // Cached for another `last_activity` or adapter
                        // version, or not cached.
                        stale += u32::from(found.is_some());
                        pending.missing.push((
                            s.key.clone(),
                            Arc::clone(&rel),
                            s.last_activity,
                            s.tail_pending,
                        ));
                        None
                    }
                }
            }
        };
        if let Some(claims) = claims {
            whos.push(Who {
                key: s.key.clone(),
                order: own.len() + n,
                own: None,
                rel,
                claims,
            });
        }
    }
    debug!("claims";
        sessions = others.len(),
        memory = from_memory,
        cached = from_cache,
        stale = stale,
        missing = pending.missing.len(),
        unverified = pending.unverified.len());
    timing.claims = lap.elapsed();
    lap = Instant::now();

    // Paths' histories.
    let mut by_path: HashMap<ChangePath, Vec<(u32, u32)>> = HashMap::new();
    for (w, who) in whos.iter().enumerate() {
        for (c, call) in who.claims.calls.iter().enumerate() {
            if let ClaimKind::Edit { paths, .. } = &call.kind {
                for p in paths.iter() {
                    by_path
                        .entry(p.clone())
                        .or_default()
                        .push((w as u32, c as u32));
                }
            }
        }
    }

    // The cutoffs.
    let paths: HashSet<ChangePath> = list
        .files
        .iter()
        .flat_map(|f| {
            let from = match &f.status {
                ChangeStatus::Renamed { from, .. } => Some(from.clone()),
                _ => None,
            };
            std::iter::once(f.path.clone()).chain(from)
        })
        .collect();
    // A record after the base entry cuts only when it left the base's line
    // of history; an empty-tree base is everything's ancestor.
    let base_oid = base.commit.and_then(|c| oid_of(hash, &c));
    let mut leaves = |e: &lectern_git::ReflogEntry| match (base_oid, e.new) {
        (Some(base), Some(new)) => !engine.descends(&git, base, new, cancel),
        _ => false,
    };
    let (records, floor) = match mode {
        CutoffMode::Records(i) => cutoff::window(&reflog.entries, i, window_s, &mut leaves),
        CutoffMode::CommitTime(_) => (Vec::new(), None),
    };
    let commit_time = match mode {
        CutoffMode::CommitTime(Some(c)) => git.repo.commit(c).ok().map(|i| i.committed.seconds),
        _ => None,
    };
    let table = engine.table(&git, &records, &paths, budget, cancel);
    let unknown = table.unknown(&records);
    if !unknown.is_empty() {
        pending.records = unknown;
        pending.table_records = records.clone();
        pending.table_paths = paths.clone();
    }
    let cutoff_ms = |path: &ChangePath| -> i64 {
        let at = table
            .cutoff(&records, floor, path)
            .or(commit_time)
            .map_or(i64::MIN, |s| s.saturating_mul(1000));
        at.max(window_ms)
    };
    timing.cutoffs = lap.elapsed();

    // Each file's history and claim.
    let mut groups_of: HashMap<(u32, u32), u32> = HashMap::new();
    let mut histories: Vec<Vec<Hist>> = Vec::with_capacity(list.files.len());
    for f in list.files.iter() {
        let (start, end) = ends(f);
        let cut = cutoff_ms(&start).max(cutoff_ms(&end));
        let mut refs: Vec<(u32, u32)> = Vec::new();
        for p in [&start, &end] {
            for r in by_path.get(p).into_iter().flatten() {
                if !refs.contains(r) {
                    refs.push(*r);
                }
            }
        }
        let mut hist: Vec<(Timestamp, usize, u64, u32, u32)> = refs
            .into_iter()
            .filter_map(|(w, c)| {
                let who = &whos[w as usize];
                let call = &who.claims.calls[c as usize];
                match &call.kind {
                    ClaimKind::Edit { at, .. } if at.0 >= cut => {
                        Some((*at, who.order, call.position(), w, c))
                    }
                    _ => None,
                }
            })
            .collect();
        hist.sort();
        histories.push(
            hist.into_iter()
                .map(|(_, _, _, w, c)| {
                    let ordinal = whos[w as usize].claims.calls[c as usize].ordinal;
                    let next = groups_of.len() as u32;
                    let group = *groups_of.entry((w, ordinal)).or_insert(next);
                    Hist {
                        who: w,
                        call: c,
                        group,
                    }
                })
                .collect(),
        );
    }

    // Command claims and, for an unrecognized target, claims by time: for
    // files no change step took.
    let bare: Vec<usize> = (0..list.files.len())
        .filter(|&i| histories[i].is_empty())
        .collect();
    let bare_paths: Vec<lectern_git::RepoPath> = bare
        .iter()
        .filter_map(|&i| lectern_git::RepoPath::new(&list.files[i].path.0))
        .collect();
    let mtimes: HashMap<ChangePath, i64> = if bare_paths.is_empty() {
        HashMap::new()
    } else {
        let times = git.worktree.mtimes(&bare_paths).unwrap_or_default();
        bare_paths
            .iter()
            .zip(times)
            .filter_map(|(p, t)| {
                Some((ChangePath(Arc::clone(p.shared())), t?.div_euclid(1_000_000)))
            })
            .collect()
    };
    let mut commands: HashMap<usize, (u32, Vec<u32>)> = HashMap::new();
    let mut by_time: HashSet<usize> = HashSet::new();
    for &i in &bare {
        let f = &list.files[i];
        let Some(&mtime) = mtimes.get(&f.path) else {
            continue;
        };
        if mtime < cutoff_ms(&f.path) {
            continue;
        }
        let mut hits: Vec<(u32, Vec<u32>)> = Vec::new();
        for (w, who) in whos.iter().enumerate() {
            let calls: Vec<u32> = who
                .claims
                .calls
                .iter()
                .enumerate()
                .filter(|(_, c)| matches!(c.kind, ClaimKind::Command { from, to } if from.0 <= mtime && mtime <= to.0))
                .map(|(n, _)| n as u32)
                .collect();
            if !calls.is_empty() {
                hits.push((w as u32, calls));
            }
        }
        match hits.len() {
            1 => {
                commands.insert(i, hits.remove(0));
            }
            0 if degraded.is_some() => {
                let s = &own[0].summary;
                if s.started_at.0 - claims::COMMAND_BEFORE_MS <= mtime
                    && mtime <= s.last_activity.0 + claims::COMMAND_AFTER_MS
                {
                    by_time.insert(i);
                }
            }
            // Several sessions' windows: none of them (outside any session).
            _ => {}
        }
    }

    // The review's file set.
    let session_target = matches!(target, ReviewTarget::Session(_));
    let claimed_by_own = |i: usize| {
        histories[i]
            .iter()
            .any(|h| whos[h.who as usize].own.is_some())
            || commands
                .get(&i)
                .is_some_and(|(w, _)| whos[*w as usize].own.is_some())
            || by_time.contains(&i)
    };
    let claimed = |i: usize| !histories[i].is_empty() || commands.contains_key(&i);
    let chosen: Vec<usize> = (0..list.files.len())
        .filter(|&i| match target {
            ReviewTarget::Session(_) => claimed_by_own(i),
            ReviewTarget::Outside { .. } => !claimed(i),
            ReviewTarget::Range { .. } => true,
        })
        .collect();

    // Narrative order and the review's sessions.
    let placed = narrative::place(&own);
    let order = narrative::order(&own, &placed);
    let sessions: Arc<[SessionRef]> = own
        .iter()
        .enumerate()
        .map(|(i, o)| SessionRef {
            key: o.summary.key.clone(),
            agent: o.summary.agent.clone(),
            spawned_by: placed[i].as_ref().and_then(|p| {
                let call = p.call.clone()?;
                let ordinal = own[p.parent as usize].index.as_ref()?.turns[p.turn].ordinal;
                Some((
                    TurnRef {
                        session: p.parent,
                        ordinal,
                    },
                    call,
                ))
            }),
        })
        .collect();

    // Held for the whole loop: one admission, not one per file.
    let projects = (engine.cfg.projects)();
    let mut files: Vec<FileWork> = chosen
        .iter()
        .map(|&i| {
            let f = &list.files[i];
            let path = &f.path.0;
            let package = std::str::from_utf8(path).ok().and_then(|p| {
                match projects.as_ref()?.locate(&git.root.join(p))?.package {
                    PackageMatch::Found(dir) => Some(dir),
                    PackageMatch::None | PackageMatch::NotLoaded => None,
                }
            });
            FileWork {
                list: i as u32,
                key: file_key(f),
                seen: SeenKey::new(target_key.clone(), f),
                noise: noise::classify(path, f.generated),
                group: noise::group(path, f.generated),
                package,
                history: histories[i].clone().into(),
                command: commands.get(&i).cloned(),
                by_time: by_time.contains(&i),
            }
        })
        .collect();

    // Order: a session review by first appearance in the narrative, noise
    // last; the others by group, sub-project and path.
    if session_target {
        let mut first: HashMap<u32, usize> = HashMap::new();
        let mut position = 0usize;
        let call_files = |s: u16, call: &ToolCallId| -> Vec<u32> {
            let w = whos.iter().position(|w| w.own == Some(s));
            let Some(w) = w else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for (n, f) in files.iter().enumerate() {
                let in_history = f.history.iter().any(|h| {
                    h.who as usize == w && whos[w].claims.calls[h.call as usize].call == *call
                });
                let in_command = f.command.as_ref().is_some_and(|(cw, calls)| {
                    *cw as usize == w
                        && calls
                            .iter()
                            .any(|&c| whos[w].claims.calls[c as usize].call == *call)
                });
                if in_history || in_command {
                    out.push(n as u32);
                }
            }
            out
        };
        for &(s, t) in &order {
            let Some(index) = &own[s as usize].index else {
                continue;
            };
            for item in index.turns[t].items.iter() {
                if let TurnItem::Tool(call) = item
                    && matches!(call.kind, ToolKind::Edit { .. } | ToolKind::Execute { .. })
                {
                    for f in call_files(s, &call.id) {
                        first.entry(f).or_insert_with(|| {
                            position += 1;
                            position
                        });
                    }
                }
            }
        }
        let mut idx: Vec<usize> = (0..files.len()).collect();
        idx.sort_by_key(|&n| {
            (
                files[n].noise.is_some(),
                first.get(&(n as u32)).copied().unwrap_or(usize::MAX),
                files[n].list,
            )
        });
        files = reorder(files, &idx);
    } else {
        let mut idx: Vec<usize> = (0..files.len()).collect();
        idx.sort_by(|&a, &b| {
            let (x, y) = (&files[a], &files[b]);
            (
                x.group,
                x.package.as_deref(),
                &list.files[x.list as usize].path.0[..],
            )
                .cmp(&(
                    y.group,
                    y.package.as_deref(),
                    &list.files[y.list as usize].path.0[..],
                ))
        });
        files = reorder(files, &idx);
    }

    let groups = group::arrange(
        &files
            .iter()
            .map(|f| GroupInput {
                kind: f.group,
                package: f.package.as_deref(),
                path: &list.files[f.list as usize].path.0,
            })
            .collect::<Vec<_>>(),
    );

    let mut by_path_files: HashMap<ChangePath, Vec<u32>> = HashMap::new();
    for (n, f) in files.iter().enumerate() {
        for p in [&f.key.old, &f.key.new].into_iter().flatten() {
            let list = by_path_files.entry(p.clone()).or_default();
            if !list.contains(&(n as u32)) {
                list.push(n as u32);
            }
        }
    }

    // Narrative steps: per own session and turn, per file.
    let mut step_files: HashMap<(u16, ToolCallId), Vec<u32>> = HashMap::new();
    let mut command_files: HashMap<(u16, ToolCallId), Vec<u32>> = HashMap::new();
    let mut steps: HashMap<StepId, StepWork> = HashMap::new();
    let mut step_of: HashMap<(u16, u32, u32), StepId> = HashMap::new();
    let mut steps_of: HashMap<u32, Vec<StepId>> = HashMap::new();
    {
        let mut ids = step_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (n, f) in files.iter().enumerate() {
            for h in f.history.iter() {
                let who = &whos[h.who as usize];
                let Some(s) = who.own else {
                    continue;
                };
                let call = &who.claims.calls[h.call as usize];
                let list = step_files.entry((s, call.call.clone())).or_default();
                if !list.contains(&(n as u32)) {
                    list.push(n as u32);
                }
                let id = ids.id(&who.key, &call.turn.0, &f.key);
                step_of.insert((s, call.ordinal, n as u32), id);
                let of_file = steps_of.entry(n as u32).or_default();
                if !of_file.contains(&id) {
                    of_file.push(id);
                }
                steps.insert(
                    id,
                    StepWork {
                        file: n as u32,
                        session: s,
                        ordinal: call.ordinal,
                        group: h.group,
                    },
                );
            }
            if let Some((w, calls)) = &f.command
                && let Some(s) = whos[*w as usize].own
            {
                for &c in calls {
                    let call = &whos[*w as usize].claims.calls[c as usize];
                    command_files
                        .entry((s, call.call.clone()))
                        .or_default()
                        .push(n as u32);
                }
            }
        }
    }

    // Focus: the files in order; the files the sessions wrote and read.
    let utf8 = |p: &ChangePath| p.as_str().map(RelPath::from);
    let focus_review: Arc<[RelPath]> = files
        .iter()
        .filter_map(|f| f.key.new.as_ref().or(f.key.old.as_ref()).and_then(utf8))
        .collect();
    let mut touched: Vec<RelPath> = Vec::new();
    for (i, o) in own.iter().enumerate() {
        let Some(index) = &o.index else { continue };
        let rel = &whos[i].rel;
        for turn in index.turns.iter() {
            for item in turn.items.iter() {
                let TurnItem::Tool(call) = item else { continue };
                let paths = match &call.kind {
                    ToolKind::Edit { paths } | ToolKind::Read { paths } => paths,
                    _ => continue,
                };
                for p in paths.iter() {
                    if let Some(r) = rel.rel(p).as_ref().and_then(utf8)
                        && !touched.contains(&r)
                    {
                        touched.push(r);
                    }
                }
            }
        }
    }

    let unclaimed = (list.files.len() - files.len()) as u32;
    let results = Mutex::new(vec![None; files.len()]);
    Ok(Work {
        target: target.clone(),
        target_key,
        git,
        source,
        list,
        base,
        session,
        sessions,
        own,
        placed,
        order,
        whos,
        files,
        by_path: by_path_files,
        unclaimed,
        degraded,
        groups,
        focus_review,
        focus_touched: touched.into(),
        step_files,
        command_files,
        steps,
        step_of,
        steps_of,
        pending,
        results,
        ctx,
        timing,
    })
}

impl Work {
    pub(crate) fn source_spec(&self) -> &BaseSpec {
        let lectern_services::changes::SourceKind::Worktree { base, .. } = &self.source.info().kind;
        base
    }

    /// The owner of a change step of a file's history.
    pub(crate) fn owner(&self, h: &Hist) -> Owner {
        let who = &self.whos[h.who as usize];
        match who.own {
            Some(s) => Owner::Own(TurnRef {
                session: s,
                ordinal: who.claims.calls[h.call as usize].ordinal,
            }),
            None => Owner::Other(who.key.clone()),
        }
    }
}

fn reorder<T>(items: Vec<T>, idx: &[usize]) -> Vec<T> {
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    idx.iter().filter_map(|&i| slots[i].take()).collect()
}

/// The paths a file's chain starts and ends on: a rename's source and
/// target; the path itself otherwise.
pub(crate) fn ends(f: &FileChange) -> (ChangePath, ChangePath) {
    match &f.status {
        ChangeStatus::Renamed { from, .. } => (from.clone(), f.path.clone()),
        _ => (f.path.clone(), f.path.clone()),
    }
}

fn fetch(
    git: &Git,
    target: &ReviewTarget,
    spec: BaseSpec,
    cancel: &AtomicBool,
) -> Result<(Arc<LocalChanges>, Arc<ChangeList>), ReviewError> {
    let make = |spec: BaseSpec| {
        Arc::new(LocalChanges::new(
            git.repo.clone(),
            git.worktree.clone(),
            git.project.clone(),
            git.key.clone(),
            spec,
            lectern_git::Limits::default(),
        ))
    };
    let source = make(spec);
    match source.fetch(cancel) {
        Ok(list) => Ok((source, list)),
        // A range without a merge base compares with HEAD instead.
        Err(ChangeError::NoMergeBase) if matches!(target, ReviewTarget::Range { .. }) => {
            info!("no merge base; the range compares with head");
            let source = make(BaseSpec::Head);
            let list = source.fetch(cancel).map_err(change_error)?;
            Ok((source, list))
        }
        Err(e) => Err(change_error(e)),
    }
}

fn merge_base(repo: &Repo, head: Option<Oid>, name: &str, cancel: &AtomicBool) -> Option<BlobId> {
    let head = head?;
    let other = repo.resolve(&lectern_git::RefName::new(name)?).ok()??;
    repo.merge_base(head, other, cancel).ok()?.map(blob_id)
}

/// The log's review target: a worktree as its git directory relative to
/// the common directory.
pub(crate) fn target_key(target: &ReviewTarget, git: &Git) -> TargetKey {
    match target {
        ReviewTarget::Session(key) => TargetKey::Session {
            source: key.source.0.clone(),
            id: key.id.clone(),
        },
        ReviewTarget::Outside { .. } => TargetKey::Outside {
            worktree: git.log_name.clone(),
        },
        ReviewTarget::Range { base, .. } => TargetKey::Range {
            worktree: git.log_name.clone(),
            base: base.clone(),
        },
    }
}

/// The other sessions whose claims take part: located in this worktree,
/// active within the window, not the target's, at most 100 newest first.
pub(crate) fn other_sessions(
    engine: &Engine,
    worktree: &WorktreeKey,
    own: &[Own],
    window_ms: i64,
) -> Vec<SessionSummary> {
    let mut out: Vec<SessionSummary> = Vec::new();
    for source in (engine.cfg.sources)() {
        let list = source.sessions();
        out.extend(
            list.sessions
                .iter()
                .filter(|s| in_worktree(s, worktree))
                .filter(|s| s.last_activity.0 >= window_ms)
                .filter(|s| own.iter().all(|o| o.summary.key != s.key))
                .cloned(),
        );
    }
    out.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.key.cmp(&b.key))
    });
    out.truncate(MAX_SESSIONS);
    out
}

/// The identity a file's worktree side has for keys.
pub(crate) fn side_oid(side: Option<&lectern_services::changes::FileSide>) -> Option<BlobId> {
    match side?.content {
        SideIdentity::Blob(oid) | SideIdentity::NotRetained(oid) => Some(oid),
        _ => None,
    }
}

/// The background part of a round: computes what `prepare` left pending.
/// True when anything that changes the claims or cutoffs became known, so
/// that the round is to be prepared again.
pub(crate) fn settle(engine: &Engine, work: &Work, cancel: &AtomicBool) -> bool {
    let mut changed = false;
    let p = &work.pending;
    if !p.records.is_empty() {
        let resolved = engine.complete_table(&work.git, &p.table_records, &p.table_paths, cancel);
        changed |= resolved;
    }
    // Sources this run has not read an index of: read one session's index
    // (without keeping it in the agents' LRU) for the adapter version.
    let mut verified: HashSet<lectern_services::session::SourceId> = HashSet::new();
    for key in &p.unverified {
        if !verified.insert(key.source.clone()) || engine.adapter(&key.source).is_some() {
            continue;
        }
        let Some(source) = source_of(engine, key) else {
            continue;
        };
        if let Ok(index) = source.turns_unretained(key)
            && let Some(v) = index.cursor.adapter_version()
        {
            engine.adapter_seen(&key.source, v);
            // The entries used were of another version: drop them all.
            let used = work.whos.iter().any(|w| {
                w.own.is_none() && w.key.source == key.source && w.claims.adapter_version != v
            });
            changed |= used;
        }
    }
    // Sessions whose claims were missing.
    let mut batch = engine.cfg.cache.app().batch();
    let mut writes = 0;
    for (key, rel, last_activity, tail_pending) in &p.missing {
        let Some(source) = source_of(engine, key) else {
            continue;
        };
        let index = match source.turns_unretained(key) {
            Ok(index) => index,
            Err(e) => {
                // Its claims stay unknown this round; the next activity
                // tries again.
                debug!("claims not derived";
                    from = diag::session(key), error = diag::session_error(&e));
                continue;
            }
        };
        if let Some(v) = index.cursor.adapter_version() {
            engine.adapter_seen(&key.source, v);
        }
        let claims = Arc::new(Claims::derive(&index, rel, *last_activity));
        if !tail_pending {
            batch.put(&claims::key(key), &CachedClaims((*claims).clone()));
            writes += 1;
        }
        engine.remember_claims(key, &claims);
        changed = true;
    }
    // The target's own claims, for the overview.
    for (i, o) in work.own.iter().enumerate() {
        if o.index.is_some() && !o.summary.tail_pending {
            batch.put(
                &claims::key(&o.summary.key),
                &CachedClaims((*work.whos[i].claims).clone()),
            );
            writes += 1;
        }
    }
    if writes > 0 {
        // A failed write only means deriving them again.
        let _ = batch.commit();
    }
    changed
}

/// Checks that the table answers for these records and paths.
pub(crate) fn table_covers(table: &Table, records: &[Record], paths: &HashSet<ChangePath>) -> bool {
    table.covers(records, paths.iter())
}
