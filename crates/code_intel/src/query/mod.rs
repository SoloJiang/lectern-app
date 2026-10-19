//! Queries (docs/design/code_intel.md「跳转到定义」「候选的分层与过滤」
//! 「查找引用与调用方」「限时搜索」): each one runs in an `Interactive` task
//! holding a `BusyGuard`, on one snapshot of the project taken when it
//! starts, and writes its results in batches. Every batch replaces the
//! query's `Arc<QueryResults>` and emits `CodeIntelEvent::Results`.

mod definition;
mod rank;
mod references;
mod search;
mod text;

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lectern_executor::Priority;
use lectern_platform::SafeRoot;
use lectern_services::code_intel::{
    CodeIntelEvent, Query, QueryError, QueryId, QueryKind, QueryResults, QueryState, SemanticState,
};
use lectern_services::project::{BlobId, Package, ProjectKey, RelPath, Worktree, WorktreeKey};

use crate::LangName;
use crate::extract::LanguageTable;
use crate::git_access::Tree;
use crate::index::state::Project;
use crate::index::{Inner, lock};
use crate::map::{HintStamp, LiveMap};
use crate::resolve::{DefaultRules, Resolver};
use crate::semantic::SemQuery;

pub(crate) use definition::Prepared;
#[cfg(test)]
pub(crate) use rank::distance;
#[cfg(test)]
pub(crate) use search::FileDeadline;
pub use search::whole_word;

/// Queries kept at once; a new one closes the oldest beyond this.
pub const RETAINED: usize = 16;
/// A prefetch is used by the same query within this long.
pub const PREFETCH_FOR: Duration = Duration::from_secs(2);
/// The time-boxed search stops this long after the query started.
pub const SEARCH_FOR: Duration = Duration::from_secs(1);
/// Prefetches kept at once.
const PREFETCHES: usize = 4;

/// A point in a query where the tests' hook runs (`Inner::step_hook`):
/// the interleaving matrix injects a background change there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Step {
    /// The snapshot is taken.
    Snapshot,
    /// The index lookup is done (a jump's steps 1–3, the references'
    /// `ci_ref` pass, `Named`'s lookup).
    Lookup,
    /// The hint reads are done (the search's scope, the counts).
    Hints,
    /// Before a file of the snapshot is read (`read_content`).
    Read,
    /// Before results are published.
    Publish,
}

/// The tests' hook (`Inner::step_hook`): run once, at its step.
#[cfg(test)]
pub(crate) type StepHook = (Step, HookFn);

/// What the tests' hook runs.
#[cfg(test)]
pub(crate) type HookFn = Box<dyn FnOnce(&Inner, &Snap) + Send>;

/// One query's entry: its current results and what stops it.
pub(crate) struct Entry {
    pub id: QueryId,
    pub project: ProjectKey,
    pub results: Arc<QueryResults>,
    pub cancel: Arc<AtomicBool>,
    /// The semantic requests (#26) and their replies, once escalated.
    pub semantic: Option<SemQuery>,
}

/// A prefetched jump: steps 1–3 for a `Definition`, kept for
/// [`PREFETCH_FOR`] while its view's basis stays (`View::same_basis`).
struct Prefetched {
    query: Query,
    at: Instant,
    view: View,
    prepared: Option<Arc<Prepared>>,
}

/// The queries of one activation.
///
/// Bounds: at most [`RETAINED`] entries (their results stay until closed
/// or pushed out by newer queries; a finished query's semantic part drops
/// its map once no request is open, `SemQuery::map`); at most
/// [`PREFETCHES`] prefetches, none kept past [`PREFETCH_FOR`] (a due entry
/// on the watchdog's table expires them, `expire_prefetches`) or past a
/// change of its inputs (pruned at every prefetch and query).
#[derive(Default)]
pub(crate) struct Queries {
    next: u64,
    pub entries: VecDeque<Entry>,
    prefetched: Vec<Prefetched>,
    /// When the due entry that expires prefetches fires, if one is
    /// scheduled.
    expiry: Option<Instant>,
}

/// The inputs one query's results are consistent with (「查询的视图」):
/// the generations of everything a query reads that changes under it,
/// taken with its snapshot. [`Inner::check_view`] compares it with now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct View {
    /// The project's session: its current worktree and root; it ends when
    /// the project closes or the current worktree changes.
    pub session: u64,
    /// The map's generation (`Provenance::generation`).
    pub map: u64,
    /// The store's eviction generation (`Project::evictions`): units of the
    /// index only go away with it.
    pub eviction: u64,
    /// The language table's generation.
    pub grammar: u64,
    /// The hint installations (`State::hinted`); only grows.
    pub hinted: u64,
}

impl View {
    /// The same session, map, evictions and language table: what a reused
    /// prefetch must share with the query (hints may have grown).
    pub fn same_basis(&self, other: &View) -> bool {
        (self.session, self.map, self.eviction, self.grammar)
            == (other.session, other.map, other.eviction, other.grammar)
    }
}

/// A query asked while its project's session has no map yet: started when
/// the map is installed (`Inner::resume_waiting`).
pub(crate) struct Waiting {
    pub id: QueryId,
    pub cancel: Arc<AtomicBool>,
    pub query: Query,
}

/// What [`Inner::snap_or_wait`] found.
pub(crate) enum Snapped {
    Ready(Box<Snap>),
    /// The session has no map yet: the query waits for it.
    Waiting,
    /// The project is not open.
    Gone,
}

/// How a query's view relates to now ([`Inner::check_view`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ViewCheck {
    /// Nothing the query read has changed.
    Same,
    /// Only hints were installed: the lookup is made again and its new
    /// results appended (the cut; the snapshot's map still applies).
    Grew(u64),
    /// Units were evicted: lookups and hints read before or after may
    /// disagree. Before anything is published the lookup is made again
    /// under a new snapshot, once; afterwards the query ends `Stale`.
    Evicted,
    /// The session ended (the project closed, the current worktree
    /// changed): the query ends `Stale` and publishes nothing more.
    Ended,
}

/// One project as a query sees it, taken once when the query starts
/// (「跳转到定义」): every batch answers from the same map.
pub(crate) struct Snap {
    pub project: Arc<Project>,
    pub session: u64,
    /// Set when the project's session ends.
    pub ended: Arc<AtomicBool>,
    pub worktree: Option<WorktreeKey>,
    pub worktrees: Arc<[Worktree]>,
    pub root_path: Option<Arc<Path>>,
    pub root: Option<Arc<SafeRoot>>,
    pub tree: Option<Tree>,
    pub map: LiveMap,
    /// The current language table (the versions every read filters on).
    pub table: LanguageTable,
    pub disabled: HashSet<LangName>,
    pub packages: Arc<[Package]>,
    pub review: Arc<[RelPath]>,
    pub stamp: HintStamp,
    /// Keys extracted right now: neither in the index nor searched.
    pub in_flight: HashSet<(BlobId, LangName)>,
    /// The view taken with the snapshot.
    pub view: View,
}

impl Snap {
    /// The worktree root of `worktree` (the current one when it is the
    /// current or None), opened now if the reconcile has not yet.
    pub fn root_of(&self, worktree: Option<&WorktreeKey>) -> Option<Arc<SafeRoot>> {
        if worktree.is_none() || worktree == self.worktree.as_ref() {
            if let Some(root) = &self.root {
                return Some(root.clone());
            }
            return SafeRoot::open(self.root_path.as_deref()?)
                .ok()
                .map(Arc::new);
        }
        let other = self.worktrees.iter().find(|w| Some(&w.key) == worktree)?;
        SafeRoot::open(&other.root).ok().map(Arc::new)
    }

    /// The deepest sub-project holding `path`, as its directory.
    pub fn subproject_of(&self, path: &str) -> Option<&str> {
        rank::deepest(&self.packages, path)
    }
}

/// A running query: its ID and its stop flag.
#[derive(Clone)]
pub(crate) struct Run {
    pub id: QueryId,
    pub cancel: Arc<AtomicBool>,
    /// When the query was asked (the search stops [`SEARCH_FOR`] after).
    pub started: Instant,
}

impl Run {
    pub fn stopped(&self, snap: &Snap) -> bool {
        self.cancel.load(Ordering::Acquire) || snap.ended.load(Ordering::Acquire)
    }
}

fn empty_results(id: QueryId, state: QueryState) -> QueryResults {
    QueryResults {
        id,
        generation: 0,
        symbol: None,
        candidates: Arc::from(Vec::new()),
        callers: Arc::from(Vec::new()),
        same_name: Arc::from(Vec::new()),
        jump: None,
        preselect: None,
        semantic: SemanticState::NotRequested,
        state,
    }
}

impl Inner {
    /// `CodeIntel::query`: an entry and a task. Returns at once.
    pub(crate) fn query(self: &Arc<Self>, query: Query) -> QueryId {
        let project = self.project(&query.project);
        if project.is_some() {
            self.demand(&query.project);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let state = if project.is_some() {
            QueryState::Running
        } else {
            QueryState::Failed(QueryError::NotOpen)
        };
        let (id, evicted) = {
            let mut q = lock(&self.queries);
            q.next += 1;
            let id = QueryId(q.next);
            q.entries.push_back(Entry {
                id,
                project: query.project.clone(),
                results: Arc::new(empty_results(id, state)),
                cancel: cancel.clone(),
                semantic: None,
            });
            let mut evicted = Vec::new();
            while q.entries.len() > RETAINED {
                evicted.extend(q.entries.pop_front());
            }
            (id, evicted)
        };
        for entry in evicted {
            self.end_entry(entry);
        }
        if project.is_none() {
            return id;
        }
        let prepared = self.take_prefetch(&query);
        self.expire_prefetches();
        let me = self.clone();
        let run = Run {
            id,
            cancel: cancel.clone(),
            started: self.watchdog.clock().now(),
        };
        let spawned = self
            .host
            .spawn_busy("query", Priority::Interactive, move || {
                me.run_query(&run, &query, prepared);
            });
        if !spawned {
            self.publish(id, |r| r.state = QueryState::Failed(QueryError::NotOpen));
        }
        id
    }

    /// `CodeIntel::results`: one lock, one `Arc` clone.
    pub(crate) fn results(&self, id: QueryId) -> Option<Arc<QueryResults>> {
        lock(&self.queries)
            .entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.results.clone())
    }

    /// `CodeIntel::close`: stops the query, cancels its unanswered semantic
    /// requests, drops its results.
    pub(crate) fn close(&self, id: QueryId) {
        let entry = {
            let mut q = lock(&self.queries);
            let at = q.entries.iter().position(|e| e.id == id);
            at.and_then(|i| q.entries.remove(i))
        };
        if let Some(entry) = entry {
            self.end_entry(entry);
        }
    }

    /// Closes every query of a project (it closed, or its worktree changed),
    /// or every query (`project` None: deactivation).
    pub(crate) fn close_queries(&self, project: Option<&ProjectKey>) {
        let ended: Vec<Entry> = {
            let mut q = lock(&self.queries);
            let (gone, kept): (Vec<Entry>, Vec<Entry>) = std::mem::take(&mut q.entries)
                .into_iter()
                .partition(|e| project.is_none_or(|p| &e.project == p));
            q.entries = kept.into();
            q.prefetched
                .retain(|p| project.is_some_and(|k| &p.query.project != k));
            gone
        };
        for entry in ended {
            self.end_entry(entry);
        }
    }

    fn end_entry(&self, entry: Entry) {
        entry.cancel.store(true, Ordering::Release);
        if let Some(semantic) = entry.semantic {
            semantic.cancel_unanswered();
        }
    }

    /// Replaces a query's results with `f` applied to a copy, one generation
    /// later, and emits `Results`. False when the query is gone. Batches of
    /// one query are emitted in generation order.
    pub(crate) fn publish(&self, id: QueryId, f: impl FnOnce(&mut QueryResults)) -> bool {
        self.publish_entry(id, |results, _| f(results))
    }

    /// [`publish`](Self::publish) with the query's semantic state too.
    pub(crate) fn publish_entry(
        &self,
        id: QueryId,
        f: impl FnOnce(&mut QueryResults, &mut Option<SemQuery>),
    ) -> bool {
        let _order = lock(&self.publishing);
        self.publish_ordered(id, f)
    }

    /// [`publish_entry`](Self::publish_entry) with `publishing` held.
    fn publish_ordered(
        &self,
        id: QueryId,
        f: impl FnOnce(&mut QueryResults, &mut Option<SemQuery>),
    ) -> bool {
        let generation = {
            let mut q = lock(&self.queries);
            let Some(entry) = q.entries.iter_mut().find(|e| e.id == id) else {
                return false;
            };
            let mut results = (*entry.results).clone();
            f(&mut results, &mut entry.semantic);
            if let Some(semantic) = &entry.semantic {
                results.semantic = semantic.state(&results);
            }
            results.generation += 1;
            let generation = results.generation;
            entry.results = Arc::new(results);
            generation
        };
        self.host.emit(CodeIntelEvent::Results { id, generation });
        true
    }

    /// The project's view now: a few loads under the project's lock,
    /// nothing copied (`query` runs on any thread).
    fn view_of(&self, key: &ProjectKey) -> Option<View> {
        let project = self.project(key)?;
        let eviction = project.evictions();
        let st = lock(&project.state);
        let published = st.published.as_ref()?;
        Some(View {
            session: st.session,
            map: published.map.provenance.generation(),
            eviction,
            grammar: published.map.provenance.grammar,
            hinted: st.hinted,
        })
    }

    /// Compares a query's view with now (「查询的视图」): one lock of the
    /// project and one atomic; called once after the reads of each step.
    /// The map and the language table a query reads are its snapshot's own
    /// copies, so a new map or table does not change what the query has
    /// read; the shared inputs are the session (root, worktree, open), the
    /// store (evictions, and units written: hints) and the files (checked
    /// per read, `read_content`).
    pub(crate) fn check_view(&self, snap: &Snap, view: &View) -> ViewCheck {
        let eviction = snap.project.evictions();
        let hinted = {
            let st = lock(&snap.project.state);
            if st.session != view.session || st.cancel.load(Ordering::Acquire) {
                return ViewCheck::Ended;
            }
            st.hinted
        };
        if eviction != view.eviction {
            ViewCheck::Evicted
        } else if hinted != view.hinted {
            ViewCheck::Grew(hinted)
        } else {
            ViewCheck::Same
        }
    }

    /// [`publish`](Self::publish) for a query answering from `snap`: only
    /// while its session is live, checked under the lock that orders
    /// publication against the session's end (a worktree switch or a
    /// close takes it to end the session), so nothing of an ended session
    /// is published after the end. False when not published.
    pub(crate) fn publish_in(
        &self,
        snap: &Snap,
        id: QueryId,
        f: impl FnOnce(&mut QueryResults),
    ) -> bool {
        self.step_hook(Step::Publish, snap);
        let _order = lock(&self.publishing);
        {
            let st = lock(&snap.project.state);
            if st.session != snap.view.session || st.cancel.load(Ordering::Acquire) {
                return false;
            }
        }
        self.publish_ordered(id, |results, _| f(results))
    }

    /// The project as a query sees it now; None when it is not open, or
    /// its session has no map yet ([`snap_or_wait`](Self::snap_or_wait)).
    pub(crate) fn snap(&self, key: &ProjectKey) -> Option<Snap> {
        match self.take_snap(key, None) {
            Snapped::Ready(snap) => Some(*snap),
            Snapped::Waiting | Snapped::Gone => None,
        }
    }

    /// The project as a query sees it now. A session without its map yet
    /// (the project just opened, or the current worktree just changed)
    /// takes `waiting`, started again when the map is installed
    /// (`resume_waiting`); checked and queued under one lock.
    pub(crate) fn snap_or_wait(&self, key: &ProjectKey, waiting: Waiting) -> Snapped {
        self.take_snap(key, Some(waiting))
    }

    fn take_snap(&self, key: &ProjectKey, waiting: Option<Waiting>) -> Snapped {
        let Some(project) = self.project(key) else {
            return Snapped::Gone;
        };
        let eviction = project.evictions();
        let mut st = lock(&project.state);
        // Only a published snapshot of this session: never the state being
        // reconciled, never the current language table.
        let published = match &st.published {
            Some(p) if !st.cancel.load(Ordering::Acquire) && p.session == st.session => p.clone(),
            _ => {
                if let Some(w) = waiting {
                    st.waiting.push(w);
                }
                return Snapped::Waiting;
            }
        };
        let snapshot = published.snapshot.clone();
        let worktree = snapshot
            .as_ref()
            .and_then(|s| s.active.as_ref())
            .and_then(|a| a.worktree.clone());
        // The review set orders and tiers candidates only (`in_review`):
        // taken as it is now.
        let review = st
            .focus
            .review
            .as_ref()
            .filter(|(w, _)| *w == worktree)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| Arc::from(Vec::new()));
        let map = published.map.clone();
        Snapped::Ready(Box::new(Snap {
            project: project.clone(),
            session: st.session,
            ended: st.cancel.clone(),
            worktrees: snapshot
                .as_ref()
                .map_or_else(|| Arc::from(Vec::new()), |s| s.worktrees.clone()),
            root_path: snapshot
                .as_ref()
                .and_then(|s| s.active.as_ref())
                .map(|a| a.root.clone()),
            worktree,
            // The session's root: opened once, at the published root path.
            root: published.root.clone().or_else(|| st.root.clone()),
            tree: published.tree.clone(),
            table: published.table.clone(),
            disabled: published.disabled.clone(),
            packages: published.packages.clone(),
            review,
            stamp: HintStamp {
                eviction: st.walk_eviction(eviction),
                grammar: map.provenance.grammar,
            },
            in_flight: st.in_flight.keys().cloned().collect(),
            view: View {
                session: st.session,
                map: map.provenance.generation(),
                eviction,
                grammar: map.provenance.grammar,
                hinted: st.hinted,
            },
            map,
        }))
    }

    /// Starts the queries that waited for their session's map.
    pub(crate) fn resume_waiting(self: &Arc<Self>, waiting: Vec<Waiting>) {
        for w in waiting {
            if w.cancel.load(Ordering::Acquire) {
                continue;
            }
            let me = self.clone();
            let run = Run {
                id: w.id,
                cancel: w.cancel,
                started: self.watchdog.clock().now(),
            };
            let query = w.query;
            let spawned = self
                .host
                .spawn_busy("query", Priority::Interactive, move || {
                    me.run_query(&run, &query, None);
                });
            if !spawned {
                self.publish(w.id, |r| r.state = QueryState::Failed(QueryError::NotOpen));
            }
        }
    }

    /// The tests' hook at `step` (`Inner::step_hook`); nothing outside the
    /// tests.
    #[cfg_attr(
        not(test),
        allow(unused_variables, reason = "the hook runs in the tests only")
    )]
    pub(crate) fn step_hook(&self, step: Step, snap: &Snap) {
        #[cfg(test)]
        {
            let hook = {
                let mut h = lock(&self.step_hook);
                if h.as_ref().is_some_and(|(s, _)| *s == step) {
                    h.take()
                } else {
                    None
                }
            };
            if let Some((_, f)) = hook {
                f(self, snap);
            }
        }
    }

    /// The resolution rules of a language: an extension's implementation,
    /// else the empty rules.
    pub(crate) fn resolver(&self, lang: &LangName) -> Arc<dyn Resolver> {
        lock(&self.resolvers)
            .iter()
            .find(|r| r.languages().contains(lang))
            .cloned()
            .unwrap_or_else(|| Arc::new(DefaultRules))
    }

    fn run_query(self: &Arc<Self>, run: &Run, query: &Query, prepared: Option<Arc<Prepared>>) {
        let _all = crate::phases::span("query task");
        let snapped = {
            let _t = crate::phases::span("snapshot");
            self.snap_or_wait(
                &query.project,
                Waiting {
                    id: run.id,
                    cancel: run.cancel.clone(),
                    query: query.clone(),
                },
            )
        };
        let snap = match snapped {
            Snapped::Ready(snap) => *snap,
            Snapped::Waiting => return,
            Snapped::Gone => {
                self.publish(run.id, |r| {
                    r.state = QueryState::Failed(QueryError::NotOpen)
                });
                return;
            }
        };
        self.step_hook(Step::Snapshot, &snap);
        match &query.kind {
            QueryKind::Definition { at, position } => {
                self.run_definition(run, &snap, at, *position, prepared);
            }
            QueryKind::References { of } => self.run_references(run, &snap, of),
            QueryKind::Named { name, near } => self.run_named(run, &snap, name, near.as_deref()),
        }
    }

    /// `CodeIntel::prefetch`: steps 1–3 of a `Definition`, without events.
    pub(crate) fn prefetch(self: &Arc<Self>, query: Query) {
        let QueryKind::Definition { at, position } = &query.kind else {
            return;
        };
        let (at, position) = (at.clone(), *position);
        if self.project(&query.project).is_none() {
            return;
        }
        let me = self.clone();
        self.host
            .spawn_busy("prefetch", Priority::Interactive, move || {
                let Some(snap) = me.snap(&query.project) else {
                    return;
                };
                let cancel = Arc::new(AtomicBool::new(false));
                let prepared = me.prepare_definition(&snap, &at, position, &cancel).ok();
                let now = me.watchdog.clock().now();
                let mut dropped = Vec::new();
                {
                    let mut q = lock(&me.queries);
                    if let Some(i) = q.prefetched.iter().position(|p| p.query == query) {
                        dropped.push(q.prefetched.remove(i));
                    }
                    q.prefetched.push(Prefetched {
                        query,
                        at: now,
                        view: snap.view,
                        prepared: prepared.map(Arc::new),
                    });
                    while q.prefetched.len() > PREFETCHES {
                        dropped.push(q.prefetched.remove(0));
                    }
                }
                // Dropped outside the lock: a `Prepared` may hold the last
                // lease of a replaced grammar.
                drop(dropped);
                me.expire_prefetches();
            });
    }

    /// Drops every prefetch past [`PREFETCH_FOR`] or whose inputs changed
    /// (the project closed, a new map, evictions, the language table), and
    /// schedules a due entry for the next one to expire, so none outlives
    /// its use: a `Prepared` holds candidates, extractions and its
    /// language's grammar lease.
    pub(crate) fn expire_prefetches(self: &Arc<Self>) {
        let now = self.watchdog.clock().now();
        let keys: Vec<ProjectKey> = lock(&self.queries)
            .prefetched
            .iter()
            .map(|p| p.query.project.clone())
            .collect();
        // Read without the queries' lock: `view_of` locks the project.
        let current: Vec<Option<View>> = keys.iter().map(|k| self.view_of(k)).collect();
        let (dropped, next) = {
            let mut q = lock(&self.queries);
            let mut dropped = Vec::new();
            let mut i = 0;
            while i < q.prefetched.len() {
                let p = &q.prefetched[i];
                let fresh_inputs = keys
                    .iter()
                    .position(|k| *k == p.query.project)
                    .and_then(|j| current[j])
                    .is_some_and(|v| v.same_basis(&p.view));
                if now.saturating_duration_since(p.at) > PREFETCH_FOR || !fresh_inputs {
                    dropped.push(q.prefetched.remove(i));
                } else {
                    i += 1;
                }
            }
            // Just past the oldest one's last valid instant.
            let next = q
                .prefetched
                .iter()
                .map(|p| p.at + PREFETCH_FOR + Duration::from_millis(1))
                .min();
            let next = next.filter(|at| q.expiry.is_none_or(|e| *at < e));
            if let Some(at) = next {
                q.expiry = Some(at);
            }
            (dropped, next)
        };
        drop(dropped);
        let Some(at) = next else {
            return;
        };
        let me = Arc::downgrade(self);
        // The watchdog's thread only submits: the drop runs in a task.
        self.watchdog.schedule(
            at,
            Box::new(move || {
                let Some(me) = me.upgrade() else {
                    return;
                };
                {
                    let mut q = lock(&me.queries);
                    if q.expiry == Some(at) {
                        q.expiry = None;
                    }
                }
                let task = me.clone();
                me.host
                    .spawn_busy("prefetch expiry", Priority::Background, move || {
                        task.expire_prefetches();
                    });
            }),
        );
    }

    /// Whether query `id`'s semantic part holds a map (tests); None without
    /// one.
    #[cfg(test)]
    pub(crate) fn semantic_map_held(&self, id: QueryId) -> Option<bool> {
        let q = lock(&self.queries);
        let entry = q.entries.iter().find(|e| e.id == id)?;
        entry.semantic.as_ref().map(|s| s.map.is_some())
    }

    /// Prefetches kept now (tests).
    #[cfg(test)]
    pub(crate) fn prefetch_count(&self) -> usize {
        lock(&self.queries).prefetched.len()
    }

    /// A prefetch of the same query made within [`PREFETCH_FOR`] with the
    /// inputs still current.
    fn take_prefetch(&self, query: &Query) -> Option<Arc<Prepared>> {
        if !lock(&self.queries)
            .prefetched
            .iter()
            .any(|p| &p.query == query)
        {
            return None;
        }
        let now = self.watchdog.clock().now();
        let current = self.view_of(&query.project);
        let p = {
            let mut q = lock(&self.queries);
            let i = q.prefetched.iter().position(|p| &p.query == query)?;
            q.prefetched.remove(i)
        };
        // Outside the lock: a stale one drops here.
        let fresh = now.saturating_duration_since(p.at) <= PREFETCH_FOR
            && current.is_some_and(|v| v.same_basis(&p.view));
        if fresh { p.prepared } else { None }
    }

    /// Whether a prefetch of `query` is waiting (tests).
    #[cfg(test)]
    pub(crate) fn prefetched(&self, query: &Query) -> bool {
        lock(&self.queries)
            .prefetched
            .iter()
            .any(|p| &p.query == query && p.prepared.is_some())
    }
}
