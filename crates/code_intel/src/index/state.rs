//! A project's indexing state: its inputs with their generations, the
//! derived state with the generations it was computed from
//! (「输入代数与派生状态」), and the queues of the tiers.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lectern_fs_watch::{RepoWatch, WorktreeWatch};
use lectern_git::HashKind;
use lectern_kernel::BusyGuard;
use lectern_platform::SafeRoot;
use lectern_services::code_intel::TextRef;
use lectern_services::project::{BlobId, FileList, Package, ProjectKey, ProjectSnapshot, RelPath, WorktreeKey};

use crate::LangName;
use crate::extract::{Lang, LanguageTable};
use crate::git_access::Tree;
use crate::map::{HeadDiff, LiveMap};
use crate::store::{PendingTouches, Store};

/// One open project.
pub(crate) struct Project {
    pub key: ProjectKey,
    pub store: Store,
    pub state: std::sync::Mutex<State>,
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
    /// The language table.
    pub grammar: u64,
    /// Batches of units this crate committed.
    pub writes: u64,
    /// The focus (the tier-3 anchor and the tiers 1 and 2).
    pub focus: u64,
}

impl Gens {
    /// The map's generation: not an input of its own, it changes with the
    /// file list, the worktree content and the language table.
    pub fn map(&self) -> u64 {
        self.files + self.worktree + self.grammar
    }

    /// What the worktree comparison depends on.
    pub fn compare_deps(&self) -> [u64; 6] {
        [
            self.head,
            self.index,
            self.files,
            self.rules,
            self.grammar,
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

/// The dependencies of the batch tiers' cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CursorDeps {
    pub session: u64,
    pub map: u64,
    pub packages: u64,
    pub eviction: u64,
    pub grammar: u64,
    pub focus: u64,
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
    /// The current sub-project's directory; None when tier 3 is the whole
    /// worktree.
    pub dir: Option<RelPath>,
    /// Its range of the file list.
    pub range: Range<usize>,
    pub walks: [Walk; 2],
}

impl Cursor {
    /// Whether a path belongs to tier 3.
    pub fn in_subproject(&self, path: &str) -> bool {
        match &self.dir {
            None => true,
            Some(dir) => under(path, dir),
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

/// Tier 5's progress.
#[derive(Default)]
pub(crate) struct Others {
    /// The (session, eviction, grammar) the `done` set was built under.
    pub deps: Option<(u64, u64, u64)>,
    pub done: HashSet<WorktreeKey>,
    /// The worktree being walked and the position in its entries.
    pub current: Option<(Arc<OtherTree>, Arc<[(RelPath, BlobId)]>, usize)>,
}

/// The batch tiers' state.
#[derive(Default)]
pub(crate) struct Batch {
    pub cursor: Option<Cursor>,
    /// Files whose existence check missed, per batch tier, extracted first;
    /// a file the pause abandoned goes back to the front.
    pub resume: [VecDeque<Item>; 3],
    /// Keys queued or in the resume queues: one OID on several paths is
    /// extracted once.
    pub queued: HashSet<(BlobId, LangName)>,
    /// Extractions in flight per batch tier.
    pub active: [usize; 3],
    pub touches: PendingTouches,
    pub others: Others,
}

/// Tiers 1 and 2: explicit small queues on the worker threads.
#[derive(Default)]
pub(crate) struct Focused {
    pub queue: [VecDeque<Item>; 2],
    /// Tasks running: tier 1 at most one, tier 2 at most two.
    pub running: [usize; 2],
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
    pub hash: HashKind,
    pub gens: Gens,
    pub seen: Seen,
    /// HEAD and the index are read again in the next reconcile.
    pub check_head: bool,
    pub check_index: bool,
    /// Paths to read again, and directories whose paths are.
    pub dirty: BTreeSet<RelPath>,
    pub dirty_dirs: BTreeSet<RelPath>,
    pub map: LiveMap,
    pub base_for: Option<(u64, u64)>,
    pub compare_for: Option<[u64; 6]>,
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
    pub in_flight: HashMap<(BlobId, LangName), Arc<AtomicBool>>,
    /// On battery, a query or `names` lets tiers 4 and 5 go on until the
    /// project closes or the power source changes.
    pub demand: bool,
}

impl State {
    pub fn new(session: u64, table: &LanguageTable, grammar: u64) -> State {
        State {
            session,
            cancel: Arc::new(AtomicBool::new(false)),
            snapshot: None,
            tree: None,
            root: None,
            hash: HashKind::Sha1,
            gens: Gens {
                grammar,
                ..Gens::default()
            },
            seen: Seen::default(),
            check_head: true,
            check_index: true,
            dirty: BTreeSet::new(),
            dirty_dirs: BTreeSet::new(),
            map: LiveMap::new(table),
            base_for: None,
            compare_for: None,
            head_diff: Arc::new(HeadDiff::default()),
            run: Run::default(),
            held: Vec::new(),
            watches: None,
            focus: Focuses::default(),
            focused: Focused::default(),
            batch: Batch::default(),
            in_flight: HashMap::new(),
            demand: false,
        }
    }

    /// Ends this session: every task of it stops, every extraction in
    /// flight is cancelled.
    pub fn end(&mut self) {
        self.cancel.store(true, Ordering::Release);
        for flag in self.in_flight.values() {
            flag.store(true, Ordering::Release);
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

    pub fn cursor_deps(&self, eviction: u64) -> CursorDeps {
        CursorDeps {
            session: self.session,
            map: self.gens.map(),
            packages: self.gens.packages,
            eviction,
            grammar: self.gens.grammar,
            focus: self.gens.focus,
        }
    }
}
