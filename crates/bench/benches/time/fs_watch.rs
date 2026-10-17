//! `fs_watch/*` (docs/design/fs_watch.md「性能」): a manually driven
//! watcher (`lectern_fs_watch::testing::manual`) on `FakeBackend` and
//! `FakeSource`, with a fake clock and `Executor::manual()`. Processing runs
//! on the benchmark thread, so the time is the fs_watch thread's work
//! without backend latency.

use std::cell::{Cell, RefCell};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, FakeSource, RecordingSink, manual};
use lectern_fs_watch::{
    Coalesce, Config, Flags, Health, Origin, RawBatch, RawEvent, ReflogFile, RepoBatch, RepoSpec,
    RepoWatch, Watcher, WorktreeBatch, WorktreeName, WorktreeSpec, WorktreeWatch,
};

/// Longer than every default coalescing window.
const LONG: Duration = Duration::from_secs(5);

/// The tracked files of the `cloudprint-miniapps` shape, as in `git/*`.
const TRACKED: usize = 728;

/// 20 ignore rules in `FakeSource`'s syntax; `node_modules/` near the end,
/// so that matching it walks most of the list.
const IGNORES: [&str; 20] = [
    "dist/",
    "build/",
    "out/",
    "coverage/",
    ".next/",
    ".turbo/",
    ".cache/",
    "target/",
    "tmp/",
    ".idea/",
    ".vscode/",
    "*.log",
    "*.tmp",
    "*.swp",
    "*.orig",
    "*.tsbuildinfo",
    ".DS_Store",
    ".env",
    "node_modules/",
    "packages/p00/generated",
];

/// A repository at `<scratch>/repo` with its common directory, and a manual
/// watcher on a fake backend.
struct Fixture {
    root: PathBuf,
    common: PathBuf,
    backend: Arc<FakeBackend>,
    watcher: Watcher,
    driver: Driver,
}

impl Fixture {
    fn new(scratch: &Path) -> Result<Fixture, BenchError> {
        let base = fs::canonicalize(scratch)?;
        let root = base.join("repo");
        let common = root.join(".git");
        fs::create_dir_all(common.join("logs/refs/heads"))?;
        fs::create_dir_all(common.join("refs/heads"))?;
        let backend = FakeBackend::new();
        let (watcher, driver) = manual(backend.clone(), Executor::manual(), Config::default());
        Ok(Fixture {
            root,
            common,
            backend,
            watcher,
            driver,
        })
    }

    fn repo(&self) -> (RepoWatch, Arc<RecordingSink<RepoBatch>>) {
        let sink = RecordingSink::new(None);
        let watch = self.watcher.watch_repo(
            RepoSpec {
                common_dir: self.common.clone(),
                coalesce: Coalesce::REPO,
            },
            sink.clone(),
        );
        (watch, sink)
    }
}

/// `packages/pNN/src/fileNNN.ts`, as `git/*`'s repository.
fn tracked_path(i: usize) -> String {
    format!("packages/p{:02}/src/file{i:03}.ts", i % 25)
}

/// The `n`-th file `npm install` writes: 50 files per package over four
/// directories, every fourth package scoped.
fn node_modules_path(n: usize) -> String {
    let (package, file) = (n / 50, n % 50);
    let dir = ["", "lib/", "dist/", "lib/util/"][file % 4];
    if package % 4 == 0 {
        format!(
            "node_modules/@scope{}/pkg{package}/{dir}f{file}.js",
            package % 50
        )
    } else {
        format!("node_modules/pkg{package}/{dir}f{file}.js")
    }
}

fn event(path: String, flags: Flags) -> RawEvent {
    RawEvent {
        path: path.into_bytes().into_boxed_slice(),
        flags,
    }
}

/// The burst in raw batches of 1000 `node_modules/**` events, each with an
/// equal share of the tracked-path events (the tracked files modified in
/// turn).
fn burst(ignored: usize, tracked: usize) -> Vec<RawBatch> {
    let batches = ignored.div_ceil(1000).max(1);
    (0..batches)
        .map(|b| {
            let mut events: Vec<RawEvent> = (b * 1000..((b + 1) * 1000).min(ignored))
                .map(|n| event(node_modules_path(n), Flags::FILE | Flags::CREATED))
                .collect();
            events.extend(
                (b * tracked / batches..(b + 1) * tracked / batches)
                    .map(|i| event(tracked_path(i % TRACKED), Flags::FILE | Flags::MODIFIED)),
            );
            RawBatch { events }
        })
        .collect()
}

/// 100 000 `node_modules/**` events and 1000 tracked-path events into a
/// worktree whose filter has 728 tracked files and 20 ignore rules, in raw
/// batches of about 1000 events, each processed as it arrives; then the
/// coalescing window ends and the one worktree batch is delivered. The raw
/// batches are built outside the clock.
pub const CLASSIFY_BURST: Bench = Bench {
    id: "fs_watch/classify_burst",
    run: classify_burst,
};

fn classify_burst(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (ignored, tracked) = ctx.scale((100_000, 1000), (1000, 10));
    let fx = Fixture::new(ctx.scratch())?;
    let source = FakeSource::new();
    let files: Vec<String> = (0..TRACKED).map(tracked_path).collect();
    source.track(&files.iter().map(String::as_str).collect::<Vec<_>>());
    source.ignore(&IGNORES);
    let (repo, repo_sink) = fx.repo();
    let sink: Arc<RecordingSink<WorktreeBatch>> = RecordingSink::new(None);
    let _worktree: WorktreeWatch = repo.watch_worktree(
        WorktreeSpec {
            name: WorktreeName::Main,
            root: fx.root.clone(),
            source,
            coalesce: Coalesce::WORKTREE,
        },
        sink.clone(),
    );
    fx.driver.advance(LONG);
    let first = sink.take();
    if first.len() != 1 || first[0].0.health != Some(Health::Live) {
        return Err(BenchError::debug("the first worktree batch", first));
    }
    repo_sink.take();

    let expected = tracked.min(TRACKED);
    // Checked outside the clock: each burst gives exactly one batch with
    // every tracked path and nothing from node_modules.
    let failure: Cell<Option<String>> = Cell::new(None);
    let time = ctx.time_with_setup(
        || burst(ignored, tracked),
        |batches| {
            for batch in batches {
                fx.backend.emit_batch(&fx.root, batch);
                fx.driver.run_until_idle();
            }
            fx.driver.advance(LONG);
            let delivered = sink.take();
            let ok = matches!(
                delivered.as_slice(),
                [(batch, _)] if !batch.rescan
                    && batch.paths.len() == expected
                    && batch.paths.iter().all(|p| p.as_bytes().starts_with(b"packages/"))
            );
            if !ok {
                failure.set(Some(format!(
                    "expected one batch of {expected} tracked paths, got {:?}",
                    delivered
                        .iter()
                        .map(|(b, _)| (b.paths.len(), b.rescan))
                        .collect::<Vec<_>>()
                )));
            }
            delivered
        },
    );
    if let Some(message) = failure.take() {
        return Err(BenchError::new(message));
    }
    Ok(vec![Measured::new(
        "fs_watch/classify_burst",
        Value::Time(time),
    )])
}

fn reflog_line(n: usize) -> String {
    format!(
        "{n:040} {:040} A U Thor <a@example.com> 1700000000 +0000\tcommit: {n}\n",
        n + 1
    )
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    OpenOptions::new()
        .append(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// One line appended to `logs/HEAD` (untimed, like `agents/append_4k`),
/// then the backend's event processed until the record is in the
/// repository subscription's batch: queue, read of the new bytes, stamps,
/// batch. Backend latency and the coalescing window are not included; the
/// batch is delivered and checked before the next sample.
pub const REFLOG_APPEND: Bench = Bench {
    id: "fs_watch/reflog_append",
    run: reflog_append,
};

fn reflog_append(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let fx = Fixture::new(ctx.scratch())?;
    let head = fx.common.join("logs/HEAD");
    fs::write(&head, reflog_line(0))?;
    let (_repo, sink) = fx.repo();
    fx.driver.run_until_idle();
    if sink.take().len() != 1 {
        return Err(BenchError::new("no first repository batch"));
    }
    // Lines appended so far, and the first wrong delivery.
    let appended = Cell::new(1);
    let checked = Cell::new(1);
    let failure: RefCell<Option<String>> = RefCell::new(None);
    let check = || {
        fx.driver.advance(LONG);
        for (batch, _) in sink.take() {
            for record in &batch.reflog {
                let n = checked.get();
                let expected = reflog_line(n);
                let ok = record.file == ReflogFile::Head(WorktreeName::Main)
                    && record.origin == Origin::Appended
                    && *record.line == *expected.trim_end().as_bytes();
                if !ok && failure.borrow().is_none() {
                    *failure.borrow_mut() = Some(format!("record {n}: {record:?}"));
                }
                checked.set(n + 1);
            }
        }
    };
    let time = ctx.time_with_setup(
        || {
            check();
            let n = appended.get();
            appended.set(n + 1);
            let modified = event("logs/HEAD".to_owned(), Flags::FILE | Flags::MODIFIED);
            let batch = RawBatch {
                events: vec![modified],
            };
            (append(&head, &reflog_line(n)), batch)
        },
        |(written, batch)| {
            if let Err(error) = written {
                *failure.borrow_mut() = Some(format!("append: {error}"));
            }
            fx.backend.emit_batch(&fx.common, batch);
            fx.driver.run_until_idle()
        },
    );
    check();
    if let Some(message) = failure.take() {
        return Err(BenchError::new(message));
    }
    if checked.get() != appended.get() {
        return Err(BenchError::new(format!(
            "{} lines appended, {} records delivered",
            appended.get() - 1,
            checked.get() - 1
        )));
    }
    Ok(vec![Measured::new(
        "fs_watch/reflog_append",
        Value::Time(time),
    )])
}
