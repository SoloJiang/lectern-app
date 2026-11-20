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
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use lectern_bench::{Bench, BenchError, Ctx, Measured, SampleConfig, Samples, Value};
use lectern_code_intel::{CodeIntelDeps, CodeIntelPlugin, NoGrammarPacks};
use lectern_executor::{Executor, Priority};
use lectern_fs_watch::testing::{Driver, FakeBackend, manual};
use lectern_fs_watch::{Config, Flags, Watcher};
use lectern_kernel::{
    Activation, Active, Context, Kernel, Manifest, Plugin, PluginError, PluginSet, ServiceRef,
    WorkspaceFacts, service,
};
use lectern_project::testing::{Driven, Logged, Shape};
use lectern_project::{FileKind, ProjectDeps, ProjectPlugin};
use lectern_services::code_intel::CodeIntel;
use lectern_services::project::{
    FileList, PackageMatch, ProjectEvent, ProjectKey, ProjectSnapshot, Projects, Scan, WorktreeKey,
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

/// The active tree has `files` files and `packages` sub-projects, and the
/// directory table of that file list.
fn check_ready(snapshot: &ProjectSnapshot, files: usize, packages: usize) -> Result<(), String> {
    check_dirs(snapshot, files)?;
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

/// The active tree's directory table is ready, built from its file list of
/// `files` files.
fn check_dirs(snapshot: &ProjectSnapshot, files: usize) -> Result<(), String> {
    let active = snapshot.active.as_ref().ok_or("no active tree")?;
    match (&active.files, &active.dirs) {
        (Scan::Ready(list), Scan::Ready(table))
            if Arc::ptr_eq(list, table.files())
                && table.file_count(table.root()) as usize == files =>
        {
            Ok(())
        }
        (_, Scan::Ready(_)) => Err("the directory table is from another file list".into()),
        (_, dirs) => Err(format!("the directory table is {dirs:?}")),
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
/// the parse cache warm). The watch handover, the new watch's first batch
/// and the directory table's build (`project/dirs_100k` times that) run in
/// fs_watch and in the setup, outside the clock.
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
            // The switch's scan: the one job the request queued.
            model.step();
            target
        },
    );
    watch.settle(&model);
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
    // Each round is two generations: the round replaced the file list once
    // and kept the sub-projects (the directory table pending), then the
    // table's build it queued published the new table alone. The build
    // runs here, outside the clock, and the log goes before the next round
    // (its facts hold a file list).
    let drain = || {
        model.run();
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
            let expected = if (g - start) % 2 == 1 {
                parts.files && parts.dirs && !parts.packages && !parts.worktrees
            } else {
                parts.dirs && !parts.files && !parts.packages && !parts.worktrees && !parts.active
            };
            if !expected || g != *last + 1 {
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
        // The refresh round only: the one job the batch queued.
        |()| model.step(),
    );
    drain();
    let s = snapshot(&model, &key)?;
    if s.generation != *generation.borrow() || s.generation != start + 2 * *rounds.borrow() {
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

/// The directory table of the large monorepo's file list (file_reader
/// design「性能」): the background job the model queues once a new list is
/// published, which builds the table (`DirTable::new`) and publishes it as
/// its own generation. Each sample switches to the other of the two
/// worktrees and runs that scan in the setup (no watcher), then times the
/// table's job it queued.
pub const DIRS_100K: Bench = Bench {
    id: "project/dirs_100k",
    run: dirs_100k,
};

fn dirs_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let repo = large(ctx)?;
    let model = Driven::new(repo.t.base().join("home"), None);
    let key = open(&model, repo.root())?;
    let worktrees: Vec<WorktreeKey> = snapshot(&model, &key)?
        .worktrees
        .iter()
        .map(|w| w.key.clone())
        .collect();
    let files = repo.shape.paths.len();
    let next = RefCell::new(0usize);
    let failure: RefCell<Option<String>> = RefCell::new(None);
    // The last sample's table was published, and alone in its generation.
    let check = || {
        let checked = model
            .projects()
            .snapshot(&key)
            .ok_or_else(|| "no snapshot".to_owned())
            .and_then(|s| check_dirs(&s, files));
        let last = model
            .take_log()
            .into_iter()
            .rev()
            .find_map(|(_, l)| match l {
                Logged::Event(ProjectEvent::Changed { parts, .. }) => Some(parts),
                _ => None,
            });
        let alone =
            last.is_none_or(|p| p.dirs && !p.files && !p.packages && !p.active && !p.worktrees);
        match checked {
            Err(e) => *failure.borrow_mut() = Some(e),
            Ok(()) if !alone => {
                *failure.borrow_mut() = Some(format!("the table's generation changed {last:?}"));
            }
            Ok(()) => {}
        }
    };
    let time = ctx.time_with_setup(
        || {
            check();
            let mut n = next.borrow_mut();
            *n = (*n + 1) % worktrees.len();
            model.projects().set_active_worktree(&key, &worktrees[*n]);
            // The switch's scan; the table's build stays queued.
            if !model.step() {
                *failure.borrow_mut() = Some("the switch queued no scan".into());
            }
            model.take_log();
        },
        |()| model.step(),
    );
    check();
    if model.run() != 0 {
        *failure.borrow_mut() = Some("more than one job per table".into());
    }
    if let Some(message) = failure.take() {
        return Err(BenchError::new(message));
    }
    Ok(vec![Measured::new("project/dirs_100k", Value::Time(time))])
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

/// `set_active_worktree` to the first task a screen of the app queues on the
/// switch's `Changed` has run, with the real `project` and `code-intel`
/// plugins on a real executor of the app's smallest size (2 workers, so one
/// general worker serves every `Background` task; 1 batch thread), over the
/// `cloudprint` shape written to disk (11 worktrees, every file there and
/// stat'ed). The screen is a stand-in subscribed after `code-intel` (it also
/// requires `dyn CodeIntel`, so it activates later) that queues one
/// `Background` task on `Changed`, as `worktrees`' hub does; its task is the
/// compose that ends in `Milestone::WorktreeSwitched`. The batch work is
/// finished before each sample and paused during it (as the process
/// benchmarks do after `settle`); the index of the new worktree finishes
/// outside the clock. Record only: the executor's threads make it noisier
/// than `project/switch_worktree`, which `app/switch_worktree` bounds.
pub const SWITCH_WORKTREE_WITH_INDEX: Bench = Bench {
    id: "project/switch_worktree_with_index",
    run: switch_worktree_with_index,
};

/// What the stand-in screen composed.
#[derive(Default)]
struct Composed {
    state: Mutex<ComposeState>,
    done: Condvar,
}

#[derive(Default)]
struct ComposeState {
    /// A task is queued or running: the next `Changed` only marks it dirty.
    building: bool,
    composed: usize,
}

impl Composed {
    fn lock(&self) -> std::sync::MutexGuard<'_, ComposeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn reset(&self) {
        self.lock().composed = 0;
    }

    /// Waits for the first compose since `reset`.
    fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        while state.composed == 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            state = self
                .done
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }
}

type Held<T> = Arc<Mutex<Option<ServiceRef<T>>>>;

/// The screen: requires `dyn Projects` (kept for the benchmark) and
/// `dyn CodeIntel` (to activate after it, as `file_reader`'s screens do),
/// and queues a `Background` task on every `Changed`.
struct Screen {
    executor: Executor,
    composed: Arc<Composed>,
    projects: Held<dyn Projects>,
    index: Held<dyn CodeIntel>,
}

impl Plugin for Screen {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Projects>(), service::<dyn CodeIntel>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        *self.projects.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(ctx.service::<dyn Projects>());
        *self.index.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(ctx.service::<dyn CodeIntel>());
        let executor = self.executor.clone();
        let composed = self.composed.clone();
        ctx.on::<ProjectEvent>(move |event, handle| {
            if !matches!(event, ProjectEvent::Changed { .. }) {
                return;
            }
            {
                let mut state = composed.lock();
                if state.building {
                    return;
                }
                state.building = true;
            }
            let Some(busy) = handle.busy() else {
                composed.lock().building = false;
                return;
            };
            let (executor, task) = (executor.clone(), composed.clone());
            let accepted = handle.own_with(move |gate| {
                executor.run_gated(&gate, Priority::Background, move || {
                    let _busy = busy;
                    let mut state = task.lock();
                    state.building = false;
                    state.composed += 1;
                    task.done.notify_all();
                })
            });
            if !accepted {
                composed.lock().building = false;
            }
        });
        Ok(Box::new(()))
    }
}

/// Waits until the kernel has nothing busy and no batch work is left, twice
/// in a row with nothing changed in between. The manual fs_watch runs here
/// (its batches hold busy guards until they are delivered), outside the
/// clock.
fn wait_idle(kernel: &Kernel, watch: &Watch) -> Result<(), BenchError> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        watch.driver.run_until_idle();
        watch.driver.advance(LONG);
        watch.driver.run_until_idle();
        let look = || (kernel.quiescent_epoch(), kernel.background());
        let first = look();
        if first.0.is_some() && first.1.count == 0 {
            std::thread::sleep(Duration::from_millis(2));
            if look() == first {
                return Ok(());
            }
        }
        if Instant::now() > deadline {
            return Err(BenchError::new(format!(
                "the kernel did not come to rest: {:?} {:?} {:?}",
                kernel.quiescent_epoch(),
                kernel.background(),
                kernel
                    .status()
                    .iter()
                    .map(|s| (s.id.clone(), s.state.clone()))
                    .collect::<Vec<_>>()
            )));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn switch_worktree_with_index(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let shape = Shape::cloudprint();
    let (t, _) = crate::git_worktree::shaped_linked(&shape, CLOUDPRINT_WORKTREES - 1)?;
    let watch = Watch::new();
    let executor = Executor::new(lectern_executor::Config {
        workers: 2,
        batch_workers: 1,
        on_thread_start: None,
    });
    let cache_dir = tempfile::tempdir_in(ctx.scratch())?;
    let cache = lectern_cache::Cache::new(lectern_cache::Config {
        root: cache_dir.path().to_path_buf(),
        budgets: lectern_cache::Budgets::default(),
        ui_thread: None,
    });
    let composed = Arc::new(Composed::default());
    let projects: Held<dyn Projects> = Held::default();
    let index: Held<dyn CodeIntel> = Held::default();
    let set = PluginSet::new()
        .with(
            "project",
            ProjectPlugin::with_repository_config_only(ProjectDeps {
                executor: executor.clone(),
                watcher: watch.watcher.clone(),
                home: t.base().join("home"),
            }),
        )
        .with(
            "code-intel",
            CodeIntelPlugin::with_repository_config_only(CodeIntelDeps {
                executor: executor.clone(),
                cache,
                watcher: watch.watcher.clone(),
                grammars: Arc::new(NoGrammarPacks),
            })
            .with_batch_tasks(1),
        )
        .with(
            "screen",
            Screen {
                executor: executor.clone(),
                composed: composed.clone(),
                projects: projects.clone(),
                index: index.clone(),
            },
        );
    let kernel = Kernel::start(set);
    kernel.barrier();
    let result = switch_rounds(
        ctx,
        &Rig {
            kernel: &kernel,
            watch: &watch,
            composed: &composed,
            projects: &projects,
            index: &index,
        },
        &t,
        &shape,
    );
    drop(projects.lock().unwrap_or_else(|e| e.into_inner()).take());
    drop(index.lock().unwrap_or_else(|e| e.into_inner()).take());
    kernel.resume();
    let (done, wait) = std::sync::mpsc::channel();
    kernel.shutdown(move || {
        let _ = done.send(());
    });
    let _ = wait.recv_timeout(Duration::from_secs(10));
    let _ = executor.shutdown(Duration::from_secs(5));
    result
}

/// What the rounds run against.
struct Rig<'a> {
    kernel: &'a Kernel,
    watch: &'a Watch,
    composed: &'a Composed,
    projects: &'a Held<dyn Projects>,
    index: &'a Held<dyn CodeIntel>,
}

fn switch_rounds(
    ctx: &mut Ctx,
    rig: &Rig<'_>,
    t: &lectern_git::testing::TestRepo,
    shape: &Shape,
) -> Result<Vec<Measured>, BenchError> {
    let Rig {
        kernel,
        watch,
        composed,
        projects,
        index,
    } = *rig;
    // `ServiceRef::get` holds the provider busy until the handle is dropped:
    // each use takes one and drops it, never across a wait.
    let service = projects
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| BenchError::new("the screen is not active"))?;
    let intel = index
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| BenchError::new("the screen is not active"))?;
    let projects = || {
        service
            .get()
            .ok_or_else(|| BenchError::new("no Projects provider"))
    };
    let check_indexed = |key: &ProjectKey| {
        let intel = intel
            .get()
            .ok_or_else(|| BenchError::new("no CodeIntel provider"))?;
        indexed(&*intel, key)
    };
    projects()?.open(t.root());
    wait_idle(kernel, watch)?;
    let key = projects()?
        .open_projects()
        .pop()
        .ok_or_else(|| BenchError::new("the project did not open"))?;
    let snapshot = projects()?
        .snapshot(&key)
        .ok_or_else(|| BenchError::new("no snapshot"))?;
    let worktrees: Vec<WorktreeKey> = snapshot.worktrees.iter().map(|w| w.key.clone()).collect();
    if worktrees.len() != CLOUDPRINT_WORKTREES {
        return Err(BenchError::new(format!("{} worktrees", worktrees.len())));
    }
    ready(&projects()?.snapshot(&key), shape)?;
    check_indexed(&key)?;

    ctx.config(SampleConfig {
        max_wall: Duration::from_secs(60),
        max_samples: 30,
        ..SampleConfig::default()
    });
    let next = RefCell::new(0usize);
    let failure: RefCell<Option<String>> = RefCell::new(None);
    let time = ctx.time_with_setup(
        || {
            kernel.resume();
            if let Err(e) = wait_idle(kernel, watch) {
                *failure.borrow_mut() = Some(format!("{e:?}"));
            }
            composed.reset();
            kernel.pause();
            let mut n = next.borrow_mut();
            *n = (*n + 1) % worktrees.len();
            worktrees[*n].clone()
        },
        |target| {
            match projects() {
                Ok(projects) => projects.set_active_worktree(&key, &target),
                Err(e) => *failure.borrow_mut() = Some(format!("{e:?}")),
            }
            if !composed.wait(Duration::from_secs(30)) {
                *failure.borrow_mut() = Some("the screen composed nothing".into());
            }
            target
        },
    );
    kernel.resume();
    wait_idle(kernel, watch)?;
    if let Some(message) = failure.take() {
        return Err(BenchError::new(message));
    }
    // The last switch, checked: the new worktree is current, its files are
    // ready, and `code-intel` indexed it (its reconcile ran).
    let snapshot = projects()?.snapshot(&key);
    let active = snapshot
        .as_ref()
        .and_then(|s| s.active.as_ref())
        .and_then(|a| a.worktree.as_ref());
    if active != Some(&worktrees[*next.borrow()]) {
        return Err(BenchError::new("the switch did not happen"));
    }
    ready(&snapshot, shape)?;
    check_indexed(&key)?;
    Ok(vec![Measured::new(
        "project/switch_worktree_with_index",
        Value::Time(time),
    )])
}

/// The active tree's file list and sub-projects are ready.
fn ready(snapshot: &Option<Arc<ProjectSnapshot>>, shape: &Shape) -> Result<(), BenchError> {
    let snapshot = snapshot
        .as_ref()
        .ok_or_else(|| BenchError::new("no snapshot"))?;
    check_ready(snapshot, shape.paths.len(), shape.packages).map_err(BenchError::new)
}

/// Every tier the current worktree's index has is done.
fn indexed(intel: &dyn CodeIntel, key: &ProjectKey) -> Result<(), BenchError> {
    use lectern_services::code_intel::TierState;
    let c = intel
        .coverage(key)
        .ok_or_else(|| BenchError::new("code-intel does not have the project"))?;
    if [c.focus, c.subproject, c.worktree, c.other_worktrees] != [TierState::Done; 4] {
        return Err(BenchError::new(format!("the index is not done: {c:?}")));
    }
    Ok(())
}
