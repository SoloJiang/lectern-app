//! `project/*` (docs/design/project.md「性能」): the project model on the
//! production git access (`lectern_project::testing::Driven`, which reads
//! only each repository's own configuration), its work run on the
//! benchmark thread, over repositories written by `TestRepo` in the
//! `cloudprint` shape (728 files, 11 worktrees, 25 manifests) and the large
//! monorepo shape (100 000 files, 2 000 manifests). A manually driven
//! fs_watch (`FakeBackend`, fake clock) stands in for FSEvents; its own
//! work runs outside the clock. The OS file cache is warm: the fixture was
//! just written.

use std::cell::RefCell;
use std::fs::File;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Samples, Value};
use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, manual};
use lectern_fs_watch::{Config, Flags, Watcher};
use lectern_kernel::WorkspaceFacts;
use lectern_project::FileKind;
use lectern_project::testing::{Driven, Logged, Shape};
use lectern_services::project::{
    FileList, PackageMatch, ProjectEvent, ProjectKey, ProjectSnapshot, Scan, WorktreeKey,
};

use crate::project_repo::{self, ProjectRepo};

/// Longer than every fs_watch coalescing window.
const LONG: Duration = Duration::from_secs(5);

const CLOUDPRINT_WORKTREES: usize = 11;

fn cloudprint() -> Result<ProjectRepo, BenchError> {
    project_repo::build(Shape::cloudprint(), CLOUDPRINT_WORKTREES)
}

/// The large monorepo with one linked worktree (for `resolve_package`);
/// 2 000 files and 40 manifests in smoke mode.
fn large(ctx: &Ctx) -> Result<ProjectRepo, BenchError> {
    let (files, manifests) = ctx.scale((100_000, 2_000), (2_000, 40));
    project_repo::build(Shape::monorepo(files, manifests), 2)
}

/// A manual fs_watch on a fake backend, as `app` would inject FSEvents.
struct Watch {
    backend: Arc<FakeBackend>,
    watcher: Watcher,
    driver: Driver,
}

impl Watch {
    fn new() -> Watch {
        let backend = FakeBackend::new();
        let (watcher, driver) = manual(backend.clone(), Executor::manual(), Config::default());
        Watch {
            backend,
            watcher,
            driver,
        }
    }

    fn model(&self, home: &Path) -> Driven {
        Driven::new(home.to_path_buf(), Some(self.watcher.clone()))
    }

    /// Runs fs_watch (past every window) and the model in turn until
    /// neither has work left: arming, the first batches, their refreshes.
    fn settle(&self, model: &Driven) {
        loop {
            let before = self.driver.run_until_idle();
            self.driver.advance(LONG);
            let ran = model.run();
            let after = self.driver.run_until_idle();
            if before == 0 && ran == 0 && after == 0 {
                return;
            }
        }
    }
}

/// Opens `root` and runs the model until it is idle; the project's key.
fn open(model: &Driven, root: &Path) -> Result<ProjectKey, BenchError> {
    model.projects().open(root);
    model.run();
    let key = model
        .projects()
        .open_projects()
        .pop()
        .ok_or_else(|| BenchError::new("the project did not open"))?;
    Ok(key)
}

fn snapshot(model: &Driven, key: &ProjectKey) -> Result<Arc<ProjectSnapshot>, BenchError> {
    model
        .projects()
        .snapshot(key)
        .ok_or_else(|| BenchError::new("no snapshot"))
}

/// The active tree has `files` files and `packages` sub-projects.
fn check_ready(snapshot: &ProjectSnapshot, files: usize, packages: usize) -> Result<(), String> {
    let active = snapshot.active.as_ref().ok_or("no active tree")?;
    let got = (
        match &active.files {
            Scan::Ready(f) => Some(f.len()),
            _ => None,
        },
        match &active.packages {
            Scan::Ready(p) => Some(p.len()),
            _ => None,
        },
    );
    if got == (Some(files), Some(packages)) {
        Ok(())
    } else {
        Err(format!(
            "expected {files} files and {packages} sub-projects, got {got:?}"
        ))
    }
}

/// When the file list was ready (its facts built and submitted) and when
/// the sub-projects were, from the model's log of one open.
fn phases(log: &[(Instant, Logged)]) -> Result<(Instant, Instant), String> {
    let facts = log
        .iter()
        .find(|(_, l)| matches!(l, Logged::Facts(_)))
        .map(|(t, _)| *t)
        .ok_or("no facts submitted")?;
    let packages = log
        .iter()
        .rev()
        .find(|(_, l)| {
            matches!(l, Logged::Event(ProjectEvent::Changed { parts, .. }) if parts.packages)
        })
        .map(|(t, _)| *t)
        .ok_or("no Changed [packages]")?;
    if packages < facts {
        return Err("the sub-projects were ready before the file list".into());
    }
    Ok((facts, packages))
}

/// One open per sample, each in a new model (an empty parse cache): from
/// `open` to the file list's facts submitted, and from there to the
/// sub-projects published. fs_watch's arming is not timed.
fn open_phases(ctx: &mut Ctx, repo: &ProjectRepo) -> Result<(Samples, Samples), BenchError> {
    let watch = Watch::new();
    let home = repo.t.base().join("home");
    let files = repo.shape.paths.len();
    let packages = repo.shape.packages;
    let splits: RefCell<Vec<(Duration, Duration)>> = RefCell::new(Vec::new());
    let failure: RefCell<Option<String>> = RefCell::new(None);
    let samples = ctx.time_with_setup(
        || {
            // The last sample's model is gone: let fs_watch drop its watches.
            watch.driver.run_until_idle();
            watch.model(&home)
        },
        |model| {
            let start = Instant::now();
            model.projects().open(black_box(repo.root()));
            model.run();
            let log = model.take_log();
            match phases(&log) {
                Ok((facts, done)) => splits.borrow_mut().push((facts - start, done - facts)),
                Err(e) => *failure.borrow_mut() = Some(e),
            }
            let checked = model
                .projects()
                .open_projects()
                .pop()
                .and_then(|key| model.projects().snapshot(&key))
                .ok_or_else(|| "no snapshot".to_owned())
                .and_then(|s| check_ready(&s, files, packages));
            if let Err(e) = checked {
                *failure.borrow_mut() = Some(e);
            }
            (model, log)
        },
    );
    if let Some(message) = failure.take() {
        return Err(BenchError::new(message));
    }
    // Warm-up calls came first.
    let splits = splits.into_inner();
    let taken = &splits[splits.len() - samples.0.len()..];
    Ok((
        Samples(taken.iter().map(|s| s.0).collect()),
        Samples(taken.iter().map(|s| s.1).collect()),
    ))
}

/// Opening the `cloudprint` shape to its file list ready: discover, list
/// the 11 worktrees, read the index, build the facts.
pub const OPEN_CLOUDPRINT: Bench = Bench {
    id: "project/open_cloudprint",
    run: open_cloudprint,
};

fn open_cloudprint(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let repo = cloudprint()?;
    let (files, _) = open_phases(ctx, &repo)?;
    Ok(vec![Measured::new(
        "project/open_cloudprint",
        Value::Time(files),
    )])
}

/// The same open, from the file list ready to the sub-projects ready: read
/// and parse the 25 manifests, resolve the members.
pub const PACKAGES_CLOUDPRINT: Bench = Bench {
    id: "project/packages_cloudprint",
    run: packages_cloudprint,
};

fn packages_cloudprint(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let repo = cloudprint()?;
    let (_, packages) = open_phases(ctx, &repo)?;
    Ok(vec![Measured::new(
        "project/packages_cloudprint",
        Value::Time(packages),
    )])
}

/// Both phases of opening the large monorepo.
pub const OPEN_100K: Bench = Bench {
    id: "project/open_100k",
    run: open_100k,
};

fn open_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let repo = large(ctx)?;
    let (files, packages) = open_phases(ctx, &repo)?;
    Ok(vec![
        Measured::new("project/open_100k/files", Value::Time(files)),
        Measured::new("project/open_100k/packages", Value::Time(packages)),
    ])
}

/// `set_active_worktree` to the next of the 11 worktrees until its file
/// list and sub-projects are published (the `Pending` generation, a new
/// worktree handle and its index, the facts, the 25 manifests read with
/// the parse cache warm). The watch handover and the new watch's first
/// batch run in fs_watch and in the setup, outside the clock.
pub const SWITCH_WORKTREE: Bench = Bench {
    id: "project/switch_worktree",
    run: switch_worktree,
};

fn switch_worktree(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let repo = cloudprint()?;
    let watch = Watch::new();
    let model = watch.model(&repo.t.base().join("home"));
    let key = open(&model, repo.root())?;
    watch.settle(&model);
    let worktrees: Vec<WorktreeKey> = snapshot(&model, &key)?
        .worktrees
        .iter()
        .map(|w| w.key.clone())
        .collect();
    if worktrees.len() != CLOUDPRINT_WORKTREES {
        return Err(BenchError::new(format!("{} worktrees", worktrees.len())));
    }
    let files = repo.shape.paths.len();
    let packages = repo.shape.packages;
    let next = RefCell::new(0usize);
    let failure: RefCell<Option<String>> = RefCell::new(None);
    let time = ctx.time_with_setup(
        || {
            watch.settle(&model);
            model.take_log();
            let mut n = next.borrow_mut();
            *n = (*n + 1) % worktrees.len();
            worktrees[*n].clone()
        },
        |target| {
            model.projects().set_active_worktree(&key, &target);
            model.run();
            target
        },
    );
    // The last switch, checked; the others went the same way.
    let s = snapshot(&model, &key)?;
    let target = &worktrees[*next.borrow()];
    if s.active.as_ref().and_then(|a| a.worktree.as_ref()) != Some(target) {
        *failure.borrow_mut() = Some("the switch did not happen".into());
    }
    if let Err(e) = check_ready(&s, files, packages) {
        *failure.borrow_mut() = Some(e);
    }
    if let Some(message) = failure.take() {
        return Err(BenchError::new(message));
    }
    Ok(vec![Measured::new(
        "project/switch_worktree",
        Value::Time(time),
    )])
}

/// A model with `repo` open (no watcher), and a linked worktree with a
/// path in one of its sub-projects.
struct Placed {
    model: Driven,
    key: ProjectKey,
    worktree: WorktreeKey,
    root: PathBuf,
    rel: String,
    /// The sub-project `rel` is in, and its manifest.
    dir: String,
    manifest: String,
}

fn placed(repo: &ProjectRepo) -> Result<Placed, BenchError> {
    let model = Driven::new(repo.t.base().join("home"), None);
    let key = open(&model, repo.root())?;
    let s = snapshot(&model, &key)?;
    let linked = s
        .worktrees
        .iter()
        .find(|w| !w.is_main)
        .ok_or_else(|| BenchError::new("no linked worktree"))?;
    // The deepest sub-project of the last manifest.
    let (manifest, _) = repo
        .shape
        .manifests
        .iter()
        .rfind(|(p, _)| p.contains('/'))
        .ok_or_else(|| BenchError::new("no sub-project manifest"))?;
    let dir = manifest.rsplit_once('/').map_or("", |(d, _)| d).to_owned();
    Ok(Placed {
        worktree: linked.key.clone(),
        root: linked.root.to_path_buf(),
        rel: format!("{dir}/src/index.ts"),
        manifest: manifest.clone(),
        dir,
        key,
        model,
    })
}

impl Placed {
    fn resolve(&self) -> Result<PackageMatch, String> {
        self.model
            .projects()
            .resolve_package(&self.key, &self.worktree, black_box(&self.rel))
            .map_err(|e| format!("{e:?}"))
    }

    fn check(&self, found: Result<PackageMatch, String>) -> Result<(), BenchError> {
        match found {
            Ok(PackageMatch::Found(dir)) if *dir == *self.dir => Ok(()),
            other => Err(BenchError::new(format!(
                "expected {:?}, got {other:?}",
                self.dir
            ))),
        }
    }
}

/// `resolve_package` on a worktree that is not active:
/// - `hit`: the cache is valid (index checksum read, one `lstat` per
///   manifest) in the `cloudprint` shape;
/// - `miss_cloudprint`, `miss_100k`: a manifest's modification time moved
///   (untimed), so the index is read and the sub-projects resolved again,
///   every manifest read again and found in the warm parse cache.
pub const RESOLVE_PACKAGE: Bench = Bench {
    id: "project/resolve_package",
    run: resolve_package,
};

fn resolve_package(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let repo = cloudprint()?;
    let p = placed(&repo)?;
    p.check(p.resolve())?;
    let hit = ctx.time(|| p.resolve());
    p.check(p.resolve())?;
    let miss = resolve_misses(ctx, &p)?;

    let large = large(ctx)?;
    let q = placed(&large)?;
    q.check(q.resolve())?;
    let miss_100k = resolve_misses(ctx, &q)?;
    Ok(vec![
        Measured::new("project/resolve_package/hit", Value::Time(hit)),
        Measured::new("project/resolve_package/miss_cloudprint", Value::Time(miss)),
        Measured::new("project/resolve_package/miss_100k", Value::Time(miss_100k)),
    ])
}

fn resolve_misses(ctx: &mut Ctx, p: &Placed) -> Result<Samples, BenchError> {
    let manifest = p.root.join(&p.manifest);
    let file = File::options().write(true).open(&manifest)?;
    let base = SystemTime::now() - Duration::from_secs(7200);
    let round = RefCell::new(0u64);
    let failure: RefCell<Option<String>> = RefCell::new(None);
    let time = ctx.time_with_setup(
        || {
            let mut n = round.borrow_mut();
            *n += 1;
            if let Err(e) = file.set_modified(base + Duration::from_secs(*n)) {
                *failure.borrow_mut() = Some(format!("{}: {e}", manifest.display()));
            }
        },
        |()| p.resolve(),
    );
    if let Some(message) = failure.take() {
        return Err(BenchError::new(message));
    }
    p.check(p.resolve())?;
    Ok(time)
}

/// The main worktree's index replaced by one whose only change is a blob
/// OID (staged content, as `git add` of an edited file), then the refresh
/// round of the batch fs_watch delivers: index checksum, a new file list,
/// the facts, the sub-projects resolved again with every manifest kept from
/// the last round. fs_watch's own work (its filter reload parses the new
/// index into the `TrackedPaths` the model's handle shares) runs in the
/// setup, outside the clock, as it does on its own thread before the
/// coalescing window ends.
pub const REFRESH_INDEX: Bench = Bench {
    id: "project/refresh_index",
    run: refresh_index,
};

fn refresh_index(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let repo = cloudprint()?;
    let cloudprint = refresh_rounds(ctx, &repo)?;
    drop(repo);
    let repo = large(ctx)?;
    let large = refresh_rounds(ctx, &repo)?;
    Ok(vec![
        Measured::new("project/refresh_index/cloudprint", Value::Time(cloudprint)),
        Measured::new("project/refresh_index/100k", Value::Time(large)),
    ])
}

fn refresh_rounds(ctx: &mut Ctx, repo: &ProjectRepo) -> Result<Samples, BenchError> {
    let watch = Watch::new();
    let model = watch.model(&repo.t.base().join("home"));
    let key = open(&model, repo.root())?;
    watch.settle(&model);
    model.take_log();
    let changed = Flags(Flags::FILE.0 | Flags::MODIFIED.0);
    let failure: RefCell<Option<String>> = RefCell::new(None);
    let start = snapshot(&model, &key)?.generation;
    let generation = RefCell::new(start);
    let rounds = RefCell::new(0u64);
    // Each round replaced the file list once and kept the sub-projects; the
    // log goes before the next round (its facts hold a file list).
    let drain = || {
        for (_, logged) in model.take_log() {
            let Logged::Event(ProjectEvent::Changed {
                generation: g,
                parts,
                ..
            }) = logged
            else {
                continue;
            };
            let mut last = generation.borrow_mut();
            if !parts.files || parts.packages || parts.worktrees || g != *last + 1 {
                *failure.borrow_mut() = Some(format!("generation {g} after {last}: {parts:?}"));
            }
            *last = g;
        }
    };
    let time = ctx.time_with_setup(
        || {
            drain();
            if let Err(e) = repo.flip_index() {
                *failure.borrow_mut() = Some(format!("index: {e}"));
            }
            watch.backend.emit(repo.common_dir(), &[("index", changed)]);
            watch.driver.run_until_idle();
            watch.driver.advance(LONG);
            *rounds.borrow_mut() += 1;
        },
        |()| model.run(),
    );
    drain();
    let s = snapshot(&model, &key)?;
    if s.generation != *generation.borrow() || s.generation != start + *rounds.borrow() {
        *failure.borrow_mut() = Some(format!(
            "{} rounds made generation {}",
            rounds.borrow(),
            s.generation
        ));
    }
    check_ready(&s, repo.shape.paths.len(), repo.shape.packages).map_err(BenchError::new)?;
    if let Some(message) = failure.take() {
        return Err(BenchError::new(message));
    }
    Ok(time)
}

/// `WorkspaceFacts::contains` on facts built from the large monorepo's
/// file list, each pattern asked once per set of facts (nothing cached),
/// as the kernel asks the patterns of its `Workspace` plugins after a
/// submission; a new set of facts every round of patterns, which take
/// turns at being asked first:
/// - `name`: file names and `*.ext`, two found and two not;
/// - `path`: patterns with `/` from the root, one found among the last
///   quarter of the paths and one found nowhere (every path matched);
/// - `name_first` (recorded): `name`'s patterns, only the first one asked
///   of each new set of facts.
///
/// 「未缓存」 in the budget means the pattern's result is not cached yet,
/// not the first call after the facts are built.
pub const FACTS_CONTAINS: Bench = Bench {
    id: "project/facts_contains",
    run: facts_contains,
};

const NAMES: [(&str, bool); 4] = [
    ("pom.xml", true),
    ("*.java", true),
    ("Cargo.lock", false),
    ("*.kt", false),
];

const PATHS: [(&str, bool); 2] = [("services/*/pom.xml", true), ("docs/**/*.md", false)];

fn facts_contains(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (files, manifests) = ctx.scale((100_000, 2_000), (2_000, 40));
    let shape = Shape::monorepo(files, manifests);
    let mut list = FileList::builder();
    for path in &shape.paths {
        list.push(path, FileKind::File);
    }
    let list = Arc::new(list.build());
    let name = contains(ctx, &list, &NAMES)?;
    let name_first = first_contains(ctx, &list, &NAMES)?;
    let path = contains(ctx, &list, &PATHS)?;
    Ok(vec![
        Measured::new("project/facts_contains/name", Value::Time(name)),
        Measured::new("project/facts_contains/name_first", Value::Time(name_first)),
        Measured::new("project/facts_contains/path", Value::Time(path)),
    ])
}

/// Only the first pattern asked of each new set of facts (recorded): what
/// the first `Workspace` plugin's check pays for the facts just built.
fn first_contains(
    ctx: &mut Ctx,
    list: &Arc<FileList>,
    patterns: &[(&'static str, bool)],
) -> Result<Samples, BenchError> {
    let round = RefCell::new(0usize);
    let wrong: RefCell<Option<&str>> = RefCell::new(None);
    let time = ctx.time_with_setup(
        || {
            let mut n = round.borrow_mut();
            *n += 1;
            let (pattern, expected) = patterns[*n % patterns.len()];
            (
                lectern_project::workspace_facts(vec![list.clone()]),
                pattern,
                expected,
            )
        },
        |(facts, pattern, expected)| {
            if facts.contains(black_box(pattern)) != expected {
                *wrong.borrow_mut() = Some(pattern);
            }
            facts
        },
    );
    if let Some(pattern) = wrong.take() {
        return Err(BenchError::new(format!("contains({pattern:?}) was wrong")));
    }
    Ok(time)
}

fn contains(
    ctx: &mut Ctx,
    list: &Arc<FileList>,
    patterns: &[(&'static str, bool)],
) -> Result<Samples, BenchError> {
    let round = RefCell::new(0usize);
    let facts: RefCell<Option<Arc<dyn WorkspaceFacts>>> = RefCell::new(None);
    let wrong: RefCell<Option<&str>> = RefCell::new(None);
    let time = ctx.time_with_setup(
        || {
            let mut n = round.borrow_mut();
            let (set, i) = (*n / patterns.len(), *n % patterns.len());
            *n += 1;
            let mut current = facts.borrow_mut();
            if i == 0 {
                // The last set goes first, outside the clock.
                *current = None;
                *current = Some(lectern_project::workspace_facts(vec![list.clone()]));
            }
            let (pattern, expected) = patterns[(i + set) % patterns.len()];
            (current.clone(), pattern, expected)
        },
        |(facts, pattern, expected)| {
            let hit = facts
                .as_ref()
                .is_some_and(|f| f.contains(black_box(pattern)));
            if hit != expected {
                *wrong.borrow_mut() = Some(pattern);
            }
            hit
        },
    );
    if let Some(pattern) = wrong.take() {
        return Err(BenchError::new(format!("contains({pattern:?}) was wrong")));
    }
    Ok(time)
}
