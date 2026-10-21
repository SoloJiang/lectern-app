//! A project's indexing state: its inputs with their generations, the
//! derived state with the generations it was computed from
//! (「输入代数与派生状态」), and the queues of the tiers.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lectern_cache::Unit;
use lectern_fs_watch::{RepoWatch, WorktreeWatch};
use lectern_git::HashKind;
use lectern_kernel::BusyGuard;
use lectern_platform::SafeRoot;
use lectern_services::code_intel::TextRef;
use lectern_services::project::{
    BlobId, FileList, Package, ProjectKey, ProjectSnapshot, RelPath, WorktreeKey,
};

use crate::LangName;
use crate::extract::{ExtractStatus, Lang, LanguageTable};
use crate::git_access::Tree;
use crate::map::{HeadDiff, HintStamp, LiveMap, Provenance};
use crate::store::{Found, PendingTouches, Store, Version};

/// One open project.
pub(crate) struct Project {
    pub key: ProjectKey,
    pub store: Store,
    /// Held around each write transaction and the eviction generations read
    /// on either side of it: a batch commit tells its own evictions from a
    /// tier 1 replace. Never held with another lock of this crate.
    pub commits: std::sync::Mutex<()>,
    /// Eviction generations this crate's own replaces caused (tier 1 redoes
    /// a `TimedOut` or `TooLarge` unit; the cache counts the replaced unit
    /// as an eviction). Counted under `commits`.
    pub replaced: std::sync::atomic::AtomicU64,
    /// Odd while a replacing commit runs (between the store's bump and
    /// `replaced` counting it); `steady` is `evictions()` from before it.
    pub replacing: std::sync::atomic::AtomicU64,
    pub steady: std::sync::atomic::AtomicU64,
    pub state: std::sync::Mutex<State>,
    /// The ⌘K name table: locked after, never with, `state`.
    pub names: std::sync::Mutex<crate::names::NameState>,
    /// The last walks of queries' accountings (「完整性的核算」), at most
    /// `WALKS_KEPT`: a repeated query on an unchanged index skips the walk.
    /// Locked after, never with, `state`.
    pub walks: std::sync::Mutex<Vec<(crate::query::WalkKey, (i64, i64))>>,
}

impl Project {
    /// The eviction generation hints and cursors depend on: the store's,
    /// less this crate's own replaces. The touches keep the store's own
    /// (`Store::touch_pending` compares with it). During a replacing commit
    /// it is the generation from before it, so no reader sees the replace's
    /// bump before it is counted.
    pub fn evictions(&self) -> u64 {
        loop {
            let seq = self.replacing.load(Ordering::SeqCst);
            if seq % 2 == 1 {
                return self.steady.load(Ordering::SeqCst);
            }
            let replaced = self.replaced.load(Ordering::SeqCst);
            let value = self.store.evictions().saturating_sub(replaced);
            if self.replacing.load(Ordering::SeqCst) == seq {
                return value;
            }
        }
    }

    /// Starts a replacing commit (under `commits`).
    pub fn begin_replace(&self) {
        self.steady.store(self.evictions(), Ordering::SeqCst);
        self.replacing.fetch_add(1, Ordering::SeqCst);
    }

    /// Ends it, once `replaced` counts its bump (under `commits`).
    pub fn end_replace(&self) {
        self.replacing.fetch_add(1, Ordering::SeqCst);
    }
}

/// The inputs' generations. Each grows by one when its input changes
/// (「增量更新」 lists which event changes which); the eviction generation
/// is the store's own.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Gens {
    pub files: u64,
    pub packages: u64,
    pub head: u64,
    pub index: u64,
    /// Ignore rules and configuration.
    pub rules: u64,
    /// Worktree content: a reread changed an overlay entry.
    pub worktree: u64,
    /// Worktree comparisons requested (directory events, rescans).
    pub compare: u64,
    // The language table's generation is shared by every project and read
    // where it is compared (`Inner::grammar`): a per-project copy can lag
    // behind it (a project opened during a grammar round).
    /// Batches of units this crate committed.
    pub writes: u64,
    /// The focus (the tier-3 anchor and the tiers 1 and 2).
    pub focus: u64,
}

impl Gens {
    /// What the worktree comparison depends on, with the language table's
    /// generation `grammar`.
    pub fn compare_deps(&self, grammar: u64) -> [u64; 6] {
        [
            self.head,
            self.index,
            self.files,
            self.rules,
            grammar,
            self.compare,
        ]
    }
}

/// What the inputs were when last looked at.
#[derive(Default)]
pub(crate) struct Seen {
    pub files: Option<Arc<FileList>>,
    pub packages: Option<Arc<[Package]>>,
    /// `Some` once read; the inner `None` is an unborn HEAD or no index.
    pub head: Option<Option<BlobId>>,
    pub index: Option<Option<BlobId>>,
}

/// The tiers (「建索引的分层」).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Tier {
    Current,
    Review,
    Subproject,
    Worktree,
    Other,
}

impl Tier {
    /// The batch tiers' queue index.
    pub fn batch(self) -> Option<usize> {
        match self {
            Tier::Subproject => Some(0),
            Tier::Worktree => Some(1),
            Tier::Other => Some(2),
            Tier::Current | Tier::Review => None,
        }
    }
}

/// Another worktree being indexed (tier 5): its root and index entries.
pub(crate) struct OtherTree {
    pub key: WorktreeKey,
    pub root: SafeRoot,
}

/// Where an item's bytes come from.
#[derive(Clone)]
pub(crate) enum Source {
    /// The current worktree, through its `SafeRoot`.
    Worktree,
    /// The object database (`TextOrigin::Object`).
    Object,
    /// Another worktree's root.
    Other(Arc<OtherTree>),
}

/// One file to index.
#[derive(Clone)]
pub(crate) struct Item {
    pub path: RelPath,
    /// The OID it was queued under; only selects what to read, never the key.
    pub oid: BlobId,
    pub lang: Lang,
    pub source: Source,
    pub tier: Tier,
}

impl Item {
    pub fn key(&self) -> (BlobId, LangName) {
        (self.oid, self.lang.name().clone())
    }
}

/// The dependencies of the batch tiers' cursor: the map it walks (its
/// provenance, so the cursor never runs ahead of what was built), the
/// sub-projects and the eviction generation of the walks. The focus is not
/// one: a focus change rebuilds the cursor only when tier 3's scope changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CursorDeps {
    pub session: u64,
    pub map: Provenance,
    pub packages: u64,
    pub eviction: u64,
}

/// A walk over one batch tier: overlay entries first (new and changed
/// files), then file list entries.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Walk {
    pub overlay: usize,
    pub base: usize,
    pub done: bool,
}

/// Where tiers 3 and 4 are in the map: the file list is walked by index
/// range, never queued as a whole.
pub(crate) struct Cursor {
    pub deps: CursorDeps,
    pub map: LiveMap,
    pub scope: Scope,
    /// The scope's range of the file list.
    pub range: Range<usize>,
    pub walks: [Walk; 2],
}

/// What tier 3 covers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    /// No sub-projects: the whole worktree.
    Whole,
    /// The current sub-project's directory.
    Dir(RelPath),
    /// Sub-projects, but none holds the current text (or there is none):
    /// everything is tier 4.
    Empty,
}

impl Cursor {
    /// Whether a path belongs to tier 3.
    pub fn in_subproject(&self, path: &str) -> bool {
        match &self.scope {
            Scope::Whole => true,
            Scope::Dir(dir) => under(path, dir),
            Scope::Empty => false,
        }
    }
}

/// `path` is `dir` or below it; everything is below "".
pub(crate) fn under(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The extraction that owns an in-flight key: its cancel flag and tier.
pub(crate) struct Owner {
    pub flag: Arc<AtomicBool>,
    pub tier: Tier,
}

/// Tier 5's progress.
#[derive(Default)]
pub(crate) struct Others {
    /// The (session, eviction, grammar) the `done` set was built under.
    pub deps: Option<(u64, u64, u64)>,
    pub done: HashSet<WorktreeKey>,
    pub current: Option<OtherWalk>,
    /// Blobs of other worktrees this session found stored (and queued for
    /// access marking) that the current worktree's map does not have: a
    /// walk again after one worktree's index changed checks and touches
    /// only what is new.
    pub confirmed: Confirmed,
}

/// A set of (OID, language) as 8-byte keys (the OID's first 8 bytes mixed
/// with the language): about 9 bytes per blob, 1.2 MB for 100k with the
/// table's spare capacity. A collision only skips a check that the next
/// session makes; the set is a hint, like `IndexedHints`.
#[derive(Default)]
pub(crate) struct Confirmed(HashSet<u64>);

impl Confirmed {
    fn key(oid: &BlobId, lang: &LangName) -> u64 {
        use std::hash::{Hash, Hasher};
        // Fixed keys: the same language mixes the same way every time.
        let mut h = std::collections::hash_map::DefaultHasher::new();
        lang.0.hash(&mut h);
        crate::map::prefix(oid) ^ h.finish()
    }

    pub fn insert(&mut self, oid: &BlobId, lang: &LangName) {
        self.0.insert(Confirmed::key(oid, lang));
    }

    pub fn contains(&self, oid: &BlobId, lang: &LangName) -> bool {
        self.0.contains(&Confirmed::key(oid, lang))
    }

    /// Bytes on the heap (the table's buckets and control bytes).
    #[cfg(test)]
    pub fn heap_bytes(&self) -> usize {
        self.0.capacity() * (std::mem::size_of::<u64>() + 1)
    }
}

impl Others {
    /// One other worktree's index changed: only it is walked again.
    pub fn reset(&mut self, key: &WorktreeKey) {
        self.done.remove(key);
        if self.current.as_ref().is_some_and(|w| &w.tree.key == key) {
            self.current = None;
        }
    }
}

/// The other worktree being walked: its index entries and the position.
pub(crate) struct OtherWalk {
    pub tree: Arc<OtherTree>,
    pub entries: Arc<[(RelPath, BlobId)]>,
    pub pos: usize,
}

/// The batch tiers' state.
#[derive(Default)]
pub(crate) struct Batch {
    pub cursor: Option<Cursor>,
    /// Files whose existence check missed, per batch tier, extracted first;
    /// a file the pause abandoned goes back to the front.
    pub resume: [VecDeque<Item>; 3],
    /// Items of tiers 4 and 5 a keepalive slot can't do (over 256 KiB, a
    /// language pack's extraction longer than a slot), in the order they
    /// were taken: back to the front of their queue once a batch runs with
    /// the yield switch off (「交互让路」).
    pub aside: [VecDeque<Item>; 3],
    /// Keys queued, in the resume queues or set aside: one OID on several
    /// paths is extracted once.
    pub queued: HashSet<(BlobId, LangName)>,
    /// Extractions in flight per batch tier.
    pub active: [usize; 3],
    /// Hits to mark as used: queued only through [`Batch::queue_touches`],
    /// whose [`Wake`] wakes the batch tasks.
    touches: PendingTouches,
    pub others: Others,
    /// Set once this session's own write evicted units (the namespace is
    /// at its quota): the eviction generation the walks keep using.
    pub full: Option<u64>,
    /// The window chains of opened large files (「大文件」), the batch
    /// tiers' first work; a batch task takes one out while it does a step.
    pub chains: VecDeque<super::chain::Chain>,
    /// The keys of the chains queued or taken: one chain per key.
    pub chain_keys: HashSet<(BlobId, LangName)>,
}

/// Work for the batch tasks was made: passed to `Inner::wake` once the
/// project's lock is released, which wakes them. Whatever queues tier 3–5
/// work outside the batch tasks gets one, so it cannot forget the kick (the
/// liveness rule of `Inner::kick_batch`).
#[must_use = "batch work was made: pass it to `Inner::wake`"]
pub(crate) struct Wake(());

impl Wake {
    /// A write outside the batch tasks: it may have evicted units, which
    /// starts the walks over.
    pub fn wrote() -> Wake {
        Wake(())
    }
}

impl Batch {
    /// Queues `found` and `windows` (read under the store's eviction
    /// `generation` by `Store::exists_with_windows`: a `Windowed` hit's
    /// window units, marked as used with their marker, 「大文件」) to be
    /// marked as used by the batch tasks.
    pub fn queue_touches(&mut self, generation: u64, found: &[Found], windows: &[Unit]) -> Wake {
        self.touches.push(generation, found);
        self.touches.push_units(generation, windows);
        Wake(())
    }

    /// Hits are queued (`Inner::plan`).
    pub fn has_touches(&self) -> bool {
        !self.touches.is_empty()
    }

    /// The queued hits, for the batch task that marks them.
    pub fn take_touches(&mut self) -> PendingTouches {
        std::mem::take(&mut self.touches)
    }

    /// Puts back what [`take_touches`](Self::take_touches) took and did
    /// not mark, unless hits were queued meanwhile. Called by the batch
    /// task itself, which goes on with them.
    pub fn put_back_touches(&mut self, touches: PendingTouches) {
        if self.touches.is_empty() {
            self.touches = touches;
        }
    }
}

/// Tiers 1 and 2: explicit small queues on the worker threads.
#[derive(Default)]
pub(crate) struct Focused {
    pub queue: [VecDeque<Item>; 2],
    /// Tasks running: tier 1 at most one, tier 2 at most two.
    pub running: [usize; 2],
    /// Tier 1's blobs this session found stored `Done`, or extracted again
    /// with the interactive limits: tier 1 needs `Done`, which a hint does
    /// not tell (a hint also stands for `TimedOut` and `TooLarge`).
    pub satisfied: HashSet<(BlobId, Version)>,
}

/// The last focus of each kind, with the worktree its paths are in.
#[derive(Default, Clone)]
pub(crate) struct Focuses {
    pub current: Option<(Option<WorktreeKey>, TextRef)>,
    pub drawer: Option<(Option<WorktreeKey>, Arc<[RelPath]>)>,
    pub review: Option<(Option<WorktreeKey>, Arc<[RelPath]>)>,
    pub touched: Option<(Option<WorktreeKey>, Arc<[RelPath]>)>,
}

/// The reconcile task (「输入代数与派生状态」「执行」): at most one per
/// project, run again when an input changed while it ran.
#[derive(Default)]
pub(crate) struct Run {
    pub running: bool,
    pub again: bool,
}

pub(crate) struct Watches {
    pub _repo: RepoWatch,
    pub _worktree: WorktreeWatch,
}

/// Stored units without the file's definitions (`State::unfinished`): by
/// OID, the languages and their status.
pub(crate) type Unfinished = HashMap<BlobId, Vec<(LangName, ExtractStatus)>>;

/// One session's index as queries see it, published whole by the reconcile
/// (`State::published`): every query snapshot comes from one of these, never
/// from the current state or the current language table. Immutable; the
/// map's hints are shared with the state while the map is current (the
/// view's hint installs cover them).
pub(crate) struct Published {
    pub session: u64,
    /// Grows with every publication of the session (`State::publishes`):
    /// identifies this object (cached walks are keyed by it).
    pub seq: u64,
    /// The map, base and overlay complete for the inputs it was built from.
    pub map: LiveMap,
    /// The language table the map was built with (`BaseMap::table`): the
    /// versions a query's lookups ask for, so they match the hints' stamp.
    pub table: LanguageTable,
    /// The languages disabled this run when this was published.
    pub disabled: HashSet<LangName>,
    pub snapshot: Option<Arc<ProjectSnapshot>>,
    pub root: Option<Arc<SafeRoot>>,
    pub tree: Option<Tree>,
    pub packages: Arc<[Package]>,
    /// The map lacks the worktree's content: the root could not be opened
    /// (nothing of the worktree was read), or the last worktree comparison
    /// failed (the overlay may be behind). Queries count no file as
    /// indexed without reading it (`Reason::Unverified`), so nothing is
    /// `Complete` from such a map.
    pub incomplete: bool,
}

/// A project's state for one active worktree (a session). Switching the
/// active worktree starts a new session: everything below is rebuilt, what
/// was written stays in the cache by OID.
pub(crate) struct State {
    pub session: u64,
    /// Set when the session ends (project closed, worktree switched): the
    /// tasks of this session stop at their next check.
    pub cancel: Arc<AtomicBool>,
    pub snapshot: Option<Arc<ProjectSnapshot>>,
    pub tree: Option<Tree>,
    pub root: Option<Arc<SafeRoot>>,
    /// The root was opened (or failed to open) for this subscription.
    pub root_tried: bool,
    pub hash: HashKind,
    pub gens: Gens,
    pub seen: Seen,
    /// HEAD and the index are read again in the next reconcile.
    pub check_head: bool,
    pub check_index: bool,
    /// Paths to read again, and directories whose paths are.
    pub dirty: BTreeSet<RelPath>,
    pub dirty_dirs: BTreeSet<RelPath>,
    /// Paths of fs_watch events the map does not know: a new directory
    /// among them requests a worktree comparison. Only events fill it, so
    /// a comparison never requests another one.
    pub probe: BTreeSet<RelPath>,
    pub map: LiveMap,
    pub base_for: Option<(u64, u64)>,
    pub compare_for: Option<[u64; 6]>,
    /// The last worktree comparison failed: the overlay is the one before
    /// it (`Published::incomplete`).
    pub compare_failed: bool,
    pub head_diff: Arc<HeadDiff>,
    pub run: Run,
    /// Guards fs_watch delivered with batches: kept until the reconcile that
    /// handles them ends, so the kernel stays busy until the overlay is
    /// updated.
    pub held: Vec<BusyGuard>,
    pub watches: Option<Watches>,
    pub focus: Focuses,
    pub focused: Focused,
    pub batch: Batch,
    /// Extractions in flight by key, with the one that owns the entry
    /// (tiers 1 and 2 take a key over from a batch, tier 1 from tier 2).
    pub in_flight: HashMap<(BlobId, LangName), Owner>,
    /// Hint installations: one more for every locked section that made
    /// some hint match ([`State::hint_paths`]). A query records it before
    /// its index lookup (its cut); a hint it sees that was installed after
    /// the cut is for a file the lookup may have missed (「输入代数与派生
    /// 状态」).
    pub hinted: u64,
    /// Queries asked while this session has no published snapshot: started
    /// when the reconcile publishes one (`Inner::resume_waiting`).
    pub waiting: Vec<crate::query::Waiting>,
    /// What queries answer from (「查询的视图」): published by the reconcile,
    /// under this lock, only when it has consumed all its input (the base
    /// and the overlay are complete for the current inputs). The previous
    /// one stays while a reconcile runs; a new session has none.
    pub published: Option<Arc<Published>>,
    /// Publications so far (`Published::seq`).
    pub publishes: u64,
    /// The keys of this session whose stored unit is `TooLarge` or
    /// `TimedOut` (`hint_paths`): their hints match, but their units hold no
    /// definitions or references. A query's snapshot takes the `Arc`; a
    /// change copies it. Usually a few generated or very large files.
    pub unfinished: Arc<Unfinished>,
    /// Set by a reconcile that consumed all its input and still has no
    /// map to publish (no current worktree, its file list unavailable):
    /// why. Queries asked while no reconcile runs fail with it at once;
    /// those waiting failed with it.
    pub unavailable: Option<Arc<str>>,
}

impl State {
    /// Sets the hints of every path the map has for `oid` in `lang`, after
    /// its unit was found or written; counts the installation in
    /// [`hinted`](Self::hinted) when some hint changed. Every hint is set
    /// here, under the project's lock, so a query that reads a hint and
    /// then the count under the lock sees that hint's installation.
    ///
    /// Once the namespace is full every setting counts: the frozen stamp
    /// keeps the hint of an evicted unit matching, so writing that unit
    /// again changes no hint, yet a query's lookup made before the write
    /// missed it while its existence check after the write finds it.
    ///
    /// `status` is the stored unit's: a `TooLarge` or `TimedOut` unit has no
    /// definitions or references of the file, so the key is recorded in
    /// [`unfinished`](Self::unfinished) (a query counts the file as not
    /// searched, and its search reads it); any other status takes it out. A
    /// change of that record counts as an installation too.
    pub fn hint_paths(
        &mut self,
        oid: &BlobId,
        lang: &str,
        stamp: HintStamp,
        status: ExtractStatus,
    ) {
        let mut changed = false;
        let mut any = false;
        for eff in self.map.paths(oid, lang) {
            any = true;
            changed |= self.map.hints.set(&eff.slot, oid, stamp);
        }
        changed |= self.record_status(oid, lang, status);
        if changed || (any && self.batch.full.is_some()) {
            self.hinted += 1;
        }
    }

    /// Records whether the unit stored for (`oid`, `lang`) is unfinished
    /// (`Unfinished`); whether the record changed.
    fn record_status(&mut self, oid: &BlobId, lang: &str, status: ExtractStatus) -> bool {
        let unfinished = matches!(status, ExtractStatus::TooLarge | ExtractStatus::TimedOut);
        let now = self
            .unfinished
            .get(oid)
            .and_then(|v| v.iter().find(|(l, _)| &*l.0 == lang))
            .map(|(_, s)| *s);
        match (now, unfinished) {
            (Some(s), true) if s == status => false,
            (None, false) => false,
            (_, true) => {
                let keys = Arc::make_mut(&mut self.unfinished).entry(*oid).or_default();
                keys.retain(|(l, _)| &*l.0 != lang);
                keys.push((LangName(Arc::from(lang)), status));
                true
            }
            (Some(_), false) => {
                let map = Arc::make_mut(&mut self.unfinished);
                if let Some(keys) = map.get_mut(oid) {
                    keys.retain(|(l, _)| &*l.0 != lang);
                    if keys.is_empty() {
                        map.remove(oid);
                    }
                }
                true
            }
        }
    }

    pub fn new(session: u64, table: &LanguageTable, grammar: u64) -> State {
        State {
            session,
            cancel: Arc::new(AtomicBool::new(false)),
            snapshot: None,
            tree: None,
            root: None,
            root_tried: false,
            hash: HashKind::Sha1,
            gens: Gens::default(),
            hinted: 0,
            seen: Seen::default(),
            check_head: true,
            check_index: true,
            dirty: BTreeSet::new(),
            dirty_dirs: BTreeSet::new(),
            probe: BTreeSet::new(),
            map: LiveMap {
                provenance: Provenance {
                    grammar,
                    ..Provenance::default()
                },
                ..LiveMap::new(table)
            },
            base_for: None,
            compare_for: None,
            compare_failed: false,
            head_diff: Arc::new(HeadDiff::default()),
            run: Run::default(),
            held: Vec::new(),
            watches: None,
            focus: Focuses::default(),
            focused: Focused::default(),
            batch: Batch::default(),
            in_flight: HashMap::new(),
            waiting: Vec::new(),
            published: None,
            publishes: 0,
            unfinished: Arc::default(),
            unavailable: None,
        }
    }

    /// Whether work of `session` goes on: it is the current session and it
    /// has not ended. Every loop that takes work checks it before each item.
    pub fn live(&self, session: u64) -> bool {
        self.session == session && !self.cancel.load(Ordering::Acquire)
    }

    /// Ends this session: every task of it stops, every extraction in
    /// flight is cancelled.
    pub fn end(&mut self) {
        self.cancel.store(true, Ordering::Release);
        for owner in self.in_flight.values() {
            owner.flag.store(true, Ordering::Release);
        }
        self.watches = None;
        self.held.clear();
    }

    /// The current worktree's key; None for a folder project.
    pub fn worktree(&self) -> Option<&WorktreeKey> {
        self.snapshot
            .as_ref()
            .and_then(|s| s.active.as_ref())
            .and_then(|a| a.worktree.as_ref())
    }

    /// The eviction generation the batch tiers' walks depend on: the
    /// store's, unless the namespace is full (`Batch::full`).
    pub fn walk_eviction(&self, eviction: u64) -> u64 {
        self.batch.full.unwrap_or(eviction)
    }

    pub fn cursor_deps(&self, eviction: u64) -> CursorDeps {
        CursorDeps {
            session: self.session,
            map: self.map.provenance,
            packages: self.gens.packages,
            eviction: self.walk_eviction(eviction),
        }
    }

    /// The stamp hints are set and compared with: the walks' eviction
    /// generation and the language generation the map was built with.
    pub fn stamp(&self, eviction: u64) -> HintStamp {
        HintStamp {
            eviction: self.walk_eviction(eviction),
            grammar: self.map.provenance.grammar,
        }
    }

    /// Ends an item of this session: no longer queued, and no longer in
    /// flight when `flag` still owns its entry. False (and nothing changed)
    /// when the item belongs to an older session.
    pub fn finish(&mut self, session: u64, item: &Item, flag: &Arc<AtomicBool>) -> bool {
        if self.session != session {
            return false;
        }
        let key = item.key();
        if self
            .in_flight
            .get(&key)
            .is_some_and(|o| Arc::ptr_eq(&o.flag, flag))
        {
            self.in_flight.remove(&key);
        }
        self.batch.queued.remove(&key);
        if let Some(t) = item.tier.batch() {
            self.batch.active[t] = self.batch.active[t].saturating_sub(1);
        }
        true
    }
}
