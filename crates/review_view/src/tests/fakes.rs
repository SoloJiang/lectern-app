//! The fakes of the three handoff services: they record what the screens
//! hand over and answer from what the test sets.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lectern_kernel::ActivationToken;
use lectern_services::code_intel::{
    CodeIntel, Coverage, Focus, Names, NavigationProvider, ProviderRegistration, Query, QueryId,
    QueryResults,
};
use lectern_services::project::{BlobId, ProjectKey, WorktreeKey};
use lectern_services::reader::{ObjectShare, OpenTicket, ReadTarget, Reader};
use lectern_services::usage::{
    AnalysisId, AnalysisStart, Limits, MeterEvent, MrSource, MrSourceRegistration, Usage,
    UsageCommand, UsageError, UsageInterest, UsageQuery, UsageSnapshot,
};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// `dyn Reader`: the targets it was asked to open, and the shared objects
/// that are registered now.
#[derive(Default)]
pub struct FakeReader {
    opened: Mutex<Vec<ReadTarget>>,
    registered: Mutex<Vec<(BlobId, usize)>>,
    live: Arc<AtomicUsize>,
    total: AtomicUsize,
    next_ticket: AtomicU64,
}

impl FakeReader {
    pub fn opened(&self) -> Vec<ReadTarget> {
        lock(&self.opened).clone()
    }

    /// Every `share_object` call so far: the OID and the byte length.
    pub fn registered(&self) -> Vec<(BlobId, usize)> {
        lock(&self.registered).clone()
    }

    /// Registrations not dropped yet.
    pub fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    pub fn total(&self) -> usize {
        self.total.load(Ordering::SeqCst)
    }
}

impl Reader for FakeReader {
    fn open(&self, target: ReadTarget) -> OpenTicket {
        lock(&self.opened).push(target);
        OpenTicket(self.next_ticket.fetch_add(1, Ordering::SeqCst))
    }

    fn prefetch(&self, _: Vec<ReadTarget>) {}

    fn share_object(&self, oid: BlobId, bytes: Arc<[u8]>) -> ObjectShare {
        lock(&self.registered).push((oid, bytes.len()));
        self.total.fetch_add(1, Ordering::SeqCst);
        self.live.fetch_add(1, Ordering::SeqCst);
        let live = self.live.clone();
        ObjectShare::new(move || {
            live.fetch_sub(1, Ordering::SeqCst);
        })
    }
}

/// `dyn CodeIntel`: the focus it was given, in order.
#[derive(Default)]
pub struct FakeCodeIntel {
    focus: Mutex<Vec<(ProjectKey, Option<WorktreeKey>, Focus)>>,
}

impl FakeCodeIntel {
    pub fn focus(&self) -> Vec<(ProjectKey, Option<WorktreeKey>, Focus)> {
        lock(&self.focus).clone()
    }

    /// The calls of one kind of focus.
    pub fn of(&self, kind: &str) -> Vec<Focus> {
        self.focus()
            .into_iter()
            .map(|(_, _, focus)| focus)
            .filter(|focus| {
                matches!(
                    (kind, focus),
                    ("review", Focus::Review(_))
                        | ("touched", Focus::Touched(_))
                        | ("current", Focus::Current(_))
                        | ("drawer", Focus::Drawer(_))
                )
            })
            .collect()
    }
}

impl CodeIntel for FakeCodeIntel {
    fn set_focus(&self, project: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus) {
        lock(&self.focus).push((project.clone(), worktree.cloned(), focus));
    }

    fn query(&self, _: Query) -> QueryId {
        unimplemented!("the review view does not query")
    }

    fn results(&self, _: QueryId) -> Option<Arc<QueryResults>> {
        None
    }

    fn close(&self, _: QueryId) {}

    fn prefetch(&self, _: Query) {}

    fn names(&self, _: &ProjectKey) -> Names {
        Names::Unavailable
    }

    fn register_provider(
        &self,
        _: Arc<dyn NavigationProvider>,
        _: ActivationToken,
    ) -> ProviderRegistration {
        ProviderRegistration::new(|| {})
    }

    fn coverage(&self, _: &ProjectKey) -> Option<Coverage> {
        None
    }
}

/// `dyn Usage`: snapshots by query, the queries watched, and how many
/// watches were dropped.
#[derive(Default)]
pub struct FakeUsage {
    snapshots: Mutex<Vec<(UsageQuery, Arc<UsageSnapshot>)>>,
    watched: Mutex<Vec<UsageQuery>>,
    released: Arc<AtomicUsize>,
}

impl FakeUsage {
    pub fn set(&self, query: UsageQuery, snapshot: UsageSnapshot) {
        let mut snapshots = lock(&self.snapshots);
        snapshots.retain(|(q, _)| *q != query);
        snapshots.push((query, Arc::new(snapshot)));
    }

    pub fn watched(&self) -> Vec<UsageQuery> {
        lock(&self.watched).clone()
    }

    pub fn released(&self) -> usize {
        self.released.load(Ordering::SeqCst)
    }
}

impl Usage for FakeUsage {
    fn watch(&self, query: UsageQuery) -> UsageInterest {
        lock(&self.watched).push(query);
        let released = self.released.clone();
        UsageInterest::new(move || {
            released.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn snapshot(&self, query: &UsageQuery) -> Option<Arc<UsageSnapshot>> {
        lock(&self.snapshots)
            .iter()
            .find(|(q, _)| q == query)
            .map(|(_, snapshot)| snapshot.clone())
    }

    fn limits(&self) -> Arc<Limits> {
        Arc::new(Limits::default())
    }

    fn command(&self, _: UsageCommand) {}

    fn begin_analysis(&self, _: AnalysisStart) -> Result<AnalysisId, UsageError> {
        Err(UsageError::Unavailable)
    }

    fn meter(&self, _: MeterEvent) -> Result<(), UsageError> {
        Err(UsageError::Unavailable)
    }

    fn register_mr_source(&self, _: Arc<dyn MrSource>, _: ActivationToken) -> MrSourceRegistration {
        MrSourceRegistration::new(|| {})
    }
}
