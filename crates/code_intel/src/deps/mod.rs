//! The dependency tier (docs/design/code_intel.md「依赖层」, ADR 0033 and
//! 0040): npm packages that are direct dependencies of the current
//! sub-project, indexed from their declaration entries along relative
//! imports (the declaration closure), into the namespace `code_intel.deps`.
//!
//! - [`roots`]: the dependency roots of the session's worktree, the one
//!   place dependency files are opened ([`Roots`]).
//! - [`npm`]: names, direct dependencies, entries and the candidates of a
//!   relative specifier; [`json`] the manifests' ordered JSON; [`lockfile`]
//!   the streamed identities of npm's lockfile.
//! - [`specs`]: the module specifiers of a TS / JS file, from the walk's
//!   tree (`Extraction::specs`, stored in `cd_spec`).
//! - [`table`]: the packages' path tables (`cd_pkg`, `cd_pkg_file`).
//! - [`map`]: the dependency map queries resolve dependency OIDs with.
//! - [`identity`]: the identity task (`Background`, one per project at a
//!   time); [`tier`]: the dependency tier's steps in the batch layer
//!   (after tier 4, before tier 5, yielding like them).
//!
//! Only npm exists so far (TS / JS); Cargo, Go and Python join through
//! [`extensions`] and their own lookup rules when their resolution
//! extension points do.

pub(crate) mod identity;
pub(crate) mod json;
pub(crate) mod lockfile;
pub(crate) mod map;
pub(crate) mod npm;
pub(crate) mod roots;
pub(crate) mod specs;
pub(crate) mod table;
pub(crate) mod tier;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lectern_platform::FileIdentity;
use lectern_services::code_intel::{DEPENDENCY_EXTENSIONS, DepOrigin, Ecosystem};
use lectern_services::project::{BlobId, RelPath};

pub(crate) use map::DepMap;
pub(crate) use roots::{DepFile, PkgDir, Roots};
pub(crate) use table::PathTable;

/// One package's closure: at most this many files…
pub const PACKAGE_FILES: usize = 4000;
/// …and this many bytes (ADR 0033).
pub const PACKAGE_BYTES: u64 = 16 << 20;
/// A dependency file larger than this is `TooLarge`, its edges not followed.
pub const FILE_MAX: u64 = crate::extract::BACKGROUND_MAX_LEN as u64;
/// The dependency tier reads at most this much source per project and
/// session; then it stops (`Coverage::deps` `Done`), the rest on demand.
pub const SESSION_BYTES: u64 = 64 << 20;
/// A manifest (`package.json`) is read up to this size.
pub const MANIFEST_MAX: u64 = 1 << 20;
/// The dependency map holds at most this many files.
pub const MAP_FILES: usize = 20_000;
/// The dependency map keeps the packages of this many sub-projects, the
/// most recently used.
pub const MAP_SUBPROJECTS: usize = 8;
/// A stat table's OID is used only for a file whose status changed this
/// long before the table was recorded (the racy-timestamp rule).
pub const RACY: Duration = Duration::from_secs(2);

/// The dependency tier's limits (ADR 0033): [`PACKAGE_FILES`],
/// [`PACKAGE_BYTES`], [`SESSION_BYTES`]; the tests make them smaller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Limits {
    pub package_files: usize,
    pub package_bytes: u64,
    pub session_bytes: u64,
}

// Tests on the manual executor run its tasks on their own thread.
#[cfg(test)]
thread_local! {
    pub(crate) static LIMITS: std::cell::Cell<Option<Limits>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn limits() -> Limits {
    #[cfg(test)]
    if let Some(l) = LIMITS.with(std::cell::Cell::get) {
        return l;
    }
    Limits {
        package_files: PACKAGE_FILES,
        package_bytes: PACKAGE_BYTES,
        session_bytes: SESSION_BYTES,
    }
}

/// The files a dependency root of `eco` may read (ADR 0033「只读声明和源码的
/// 扩展名」): its part of `services::code_intel::DEPENDENCY_EXTENSIONS`, so
/// the TS tier opens only the TS entries (`.d.ts`, `.d.mts`, `.d.cts`,
/// `.ts`, `.tsx`, `package.json`), never a Rust, Go or Python one. The flat
/// list stays the one source; each ecosystem takes its run of it.
pub(crate) fn extensions(eco: Ecosystem) -> &'static [&'static str] {
    match eco {
        Ecosystem::Npm => &DEPENDENCY_EXTENSIONS[..6],
    }
}

/// Whether `path`'s file name is one `eco` reads: a suffix entry (starting
/// with `.`) matches a longer name ending with it, a whole-name entry the
/// name itself; bytes compared, case-sensitive.
pub(crate) fn allowed(eco: Ecosystem, path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    extensions(eco).iter().any(|e| {
        if e.starts_with('.') {
            name.len() > e.len() && name.ends_with(e)
        } else {
            name == *e
        }
    })
}

/// The language of a dependency file of the npm tier by its name: `.tsx`
/// is TSX, `.ts` (with `.d.ts`), `.mts` and `.cts` TypeScript; None for the
/// rest (`package.json`).
pub(crate) fn dep_lang<'t>(
    table: &'t crate::extract::LanguageTable,
    path: &str,
) -> Option<&'t crate::extract::Lang> {
    let name = if path.ends_with(".tsx") {
        "tsx"
    } else if path.ends_with(".ts") || path.ends_with(".mts") || path.ends_with(".cts") {
        "ts"
    } else {
        return None;
    };
    table.get(name)
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A file's five stats as stored (`FileIdentity`, timestamps in
/// nanoseconds, clamped to `i64`).
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Stat {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: i64,
    pub ctime: i64,
}

impl Stat {
    pub(crate) fn of(id: &FileIdentity) -> Stat {
        let ns = |t: lectern_platform::Timestamp| {
            i64::try_from(t.as_nanos()).unwrap_or(if t.secs < 0 { i64::MIN } else { i64::MAX })
        };
        Stat {
            dev: id.dev,
            ino: id.ino,
            size: id.size,
            mtime: ns(id.mtime),
            ctime: ns(id.ctime),
        }
    }
}

/// Nanoseconds since the epoch of a wall-clock instant.
pub(crate) fn wall_nanos(t: std::time::SystemTime) -> i64 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_nanos()).unwrap_or(i64::MAX),
    }
}

/// Whether a stat recorded at `recorded` (wall nanoseconds) may vouch for
/// the content of a file with `stat` now: the five stats equal and its
/// status changed more than [`RACY`] before the record.
pub(crate) fn vouches(recorded: &Stat, at: i64, now: &Stat) -> bool {
    recorded == now && now.ctime < at.saturating_sub(RACY.as_nanos() as i64)
}

/// One package the identity task queued for the dependency tier: where it
/// is, the key of its path table and what to compare files with.
#[derive(Clone, Debug)]
pub(crate) struct PkgWork {
    pub origin: Arc<DepOrigin>,
    pub pkg: PkgDir,
    /// The path table's identity (lockfile `integrity`, or the fallback).
    pub identity: Arc<[u8]>,
    /// The installed `package.json`'s five stats.
    pub manifest: Stat,
    /// The table to take OIDs from by stat (the package's newest, or this
    /// key's own when only its units are missing); None: read everything.
    pub stat_table: Option<Arc<PathTable>>,
    /// The sub-project it is a direct dependency of.
    pub subproject: RelPath,
}

impl PkgWork {
    /// The path table key's parts: (ecosystem, name, version, identity,
    /// package directory).
    pub(crate) fn same_key(&self, other: &PkgWork) -> bool {
        self.origin.package == other.origin.package
            && self.origin.version == other.origin.version
            && self.identity == other.identity
            && self.pkg.path() == other.pkg.path()
    }
}

/// One file of a package's closure as the walk recorded it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct WalkFile {
    /// Relative to the worktree root.
    pub path: RelPath,
    pub oid: Option<BlobId>,
    pub stat: Stat,
    /// `ExtractStatus::code`, or `TooLarge`'s for a file never read.
    pub status: u8,
}

/// The package the dependency tier is walking: its frontier (files to
/// visit, relative to the package directory) and what it recorded.
pub(crate) struct PkgWalk {
    /// Distinguishes this walk's items from an older one's.
    pub id: u64,
    pub work: PkgWork,
    /// Files to visit, relative to the package directory.
    pub frontier: VecDeque<String>,
    /// Files queued or visited, relative to the package directory.
    pub seen: HashSet<String>,
    pub files: Vec<WalkFile>,
    /// The walk's items not settled yet: queued for extraction, set aside,
    /// parked or in flight.
    pub pending: usize,
    /// The closure's bytes so far (files' sizes).
    pub bytes: u64,
    /// The entry was looked for (`tier::start_walk`).
    pub started: bool,
    /// The `@types` package used for this one's declarations.
    pub types: Option<String>,
}

impl PkgWalk {
    pub(crate) fn new(id: u64, work: PkgWork) -> PkgWalk {
        PkgWalk {
            id,
            work,
            frontier: VecDeque::new(),
            seen: HashSet::new(),
            files: Vec::new(),
            pending: 0,
            bytes: 0,
            started: false,
            types: None,
        }
    }

    /// Adds a file to visit unless seen, while the closure has fewer than
    /// [`PACKAGE_FILES`] files.
    pub(crate) fn visit(&mut self, rel: String) {
        if self.seen.len() >= limits().package_files {
            return;
        }
        if self.seen.insert(rel.clone()) {
            self.frontier.push_back(rel);
        }
    }

    pub(crate) fn done(&self) -> bool {
        self.started && self.frontier.is_empty() && self.pending == 0
    }
}

/// The identity task's run state: at most one per project, run again when
/// asked while it runs.
#[derive(Default)]
pub(crate) struct IdentityRun {
    pub running: bool,
    pub again: bool,
    /// How long each identity task took, from its start to its end
    /// (`code_intel/deps_identity`).
    #[cfg(any(test, feature = "test-support"))]
    pub took: Vec<Duration>,
}

/// One session's dependency state (`State::deps`).
#[derive(Default)]
pub(crate) struct Deps {
    pub roots: Option<Arc<Roots>>,
    pub identity: IdentityRun,
    /// The sub-project directory the last identity task ran for; None
    /// before the first this session.
    pub ran_for: Option<RelPath>,
    /// A manifest or lockfile changed: the identity task runs again.
    pub dirty: bool,
    pub queue: VecDeque<PkgWork>,
    pub walk: Option<PkgWalk>,
    pub walks: u64,
    /// The path tables ready, per sub-project, most recently used first.
    pub ready: Vec<(RelPath, Vec<Arc<PathTable>>)>,
    pub map: Arc<DepMap>,
    /// Source the dependency tier read this session.
    pub read: u64,
    /// Stopped for the session: its own write evicted dependency units, or
    /// it read [`SESSION_BYTES`].
    pub stopped: bool,
    /// The dependency tier's extractions in flight, by path: cancelled
    /// with the session.
    pub in_flight: HashMap<RelPath, Arc<AtomicBool>>,
}

impl Deps {
    /// Whether the dependency tier has a step to make now (not counting
    /// its items in the batch queues, which the tiers' plan counts).
    pub(crate) fn has_work(&self) -> bool {
        if self.stopped || self.roots.is_none() {
            return false;
        }
        match &self.walk {
            Some(w) => !w.started || !w.frontier.is_empty() || w.done(),
            None => !self.queue.is_empty(),
        }
    }

    /// The tables of every kept sub-project, each package once (the newest
    /// sub-project's first).
    pub(crate) fn tables(&self) -> Vec<Arc<PathTable>> {
        let mut out: Vec<Arc<PathTable>> = Vec::new();
        for (_, tables) in &self.ready {
            for t in tables {
                if !out.iter().any(|o| o.same_key(t)) {
                    out.push(t.clone());
                }
            }
        }
        out
    }

    /// Makes `subproject` the most recently used, with `tables`, keeping
    /// [`MAP_SUBPROJECTS`].
    pub(crate) fn set_ready(&mut self, subproject: &RelPath, tables: Vec<Arc<PathTable>>) {
        self.ready.retain(|(s, _)| s != subproject);
        self.ready.insert(0, (subproject.clone(), tables));
        self.ready.truncate(MAP_SUBPROJECTS);
    }

    /// Adds one table to `subproject`'s (a walk finished), replacing one of
    /// the same package directory.
    pub(crate) fn add_ready(&mut self, subproject: &RelPath, table: Arc<PathTable>) {
        match self.ready.iter_mut().find(|(s, _)| s == subproject) {
            Some((_, tables)) => {
                tables.retain(|t| t.dir != table.dir || t.origin.package != table.origin.package);
                tables.push(table);
            }
            None => self.set_ready(subproject, vec![table]),
        }
    }
}
