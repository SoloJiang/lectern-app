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
pub use search::whole_word;

/// Queries kept at once; a new one closes the oldest beyond this.
pub const RETAINED: usize = 16;
/// A prefetch is used by the same query within this long.
pub const PREFETCH_FOR: Duration = Duration::from_secs(2);
/// The time-boxed search stops this long after the query started.
pub const SEARCH_FOR: Duration = Duration::from_secs(1);
/// Prefetches kept at once.
const PREFETCHES: usize = 4;

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
/// [`PREFETCH_FOR`] while the map, the eviction and the language generations
/// stay.
struct Prefetched {
    query: Query,
    at: Instant,
    inputs: Inputs,
    prepared: Option<Arc<Prepared>>,
}

/// The queries of one activation.
#[derive(Default)]
pub(crate) struct Queries {
    next: u64,
    pub entries: VecDeque<Entry>,
    prefetched: Vec<Prefetched>,
}

/// What a query's snapshot depends on, for the prefetch's reuse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Inputs {
    pub session: u64,
    pub map: u64,
    pub eviction: u64,
    pub grammar: u64,
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
    pub inputs: Inputs,
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

    /// What a prefetch of the project depends on now: a few loads under
    /// the project's lock, nothing copied (`query` runs on any thread).
    fn inputs(&self, key: &ProjectKey) -> Option<Inputs> {
        let project = self.project(key)?;
        let eviction = project.evictions();
        let grammar = self.grammar();
        let st = lock(&project.state);
        Some(Inputs {
            session: st.session,
            map: st.map.provenance.generation(),
            eviction,
            grammar,
        })
    }

    /// The project as a query sees it now.
    pub(crate) fn snap(&self, key: &ProjectKey) -> Option<Snap> {
        let project = self.project(key)?;
        let eviction = project.evictions();
        let (table, disabled, grammar) = {
            let langs = lock(&self.langs);
            (
                langs.table.clone(),
                langs.disabled.clone(),
                langs.generation,
            )
        };
        let st = lock(&project.state);
        let worktree = st.worktree().cloned();
        let review = st
            .focus
            .review
            .as_ref()
            .filter(|(w, _)| *w == worktree)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| Arc::from(Vec::new()));
        let snapshot = st.snapshot.clone();
        Some(Snap {
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
            root: st.root.clone(),
            tree: st.tree.clone(),
            map: st.map.clone(),
            table,
            disabled,
            packages: st
                .seen
                .packages
                .clone()
                .unwrap_or_else(|| Arc::from(Vec::new())),
            review,
            stamp: st.stamp(eviction),
            in_flight: st.in_flight.keys().cloned().collect(),
            inputs: Inputs {
                session: st.session,
                map: st.map.provenance.generation(),
                eviction,
                grammar,
            },
        })
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
        let Some(snap) = self.snap(&query.project) else {
            self.publish(run.id, |r| {
                r.state = QueryState::Failed(QueryError::NotOpen)
            });
            return;
        };
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
                let mut q = lock(&me.queries);
                q.prefetched.retain(|p| p.query != query);
                q.prefetched.push(Prefetched {
                    query,
                    at: now,
                    inputs: snap.inputs,
                    prepared: prepared.map(Arc::new),
                });
                while q.prefetched.len() > PREFETCHES {
                    q.prefetched.remove(0);
                }
            });
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
        let current = self.inputs(&query.project);
        let mut q = lock(&self.queries);
        let i = q.prefetched.iter().position(|p| &p.query == query)?;
        let p = q.prefetched.remove(i);
        let fresh =
            now.saturating_duration_since(p.at) <= PREFETCH_FOR && Some(p.inputs) == current;
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
