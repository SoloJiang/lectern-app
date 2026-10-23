//! `plugin_*` (docs/design/review.md「测试」): the plugin under a
//! `TestKernel`, fs_watch's manual watcher on a `FakeBackend`, and one
//! `Executor::manual()` that runs the review's tasks, fs_watch's loads and
//! the retention's release. Nothing here waits for time: the retention's
//! timer is a `ManualClock`, the coalescing windows are fs_watch's fake
//! clock, and a task runs when the test runs the executor.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use lectern_cache::{Budgets, Cache};
use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, manual};
use lectern_fs_watch::{Config as WatchConfig, Flags};
use lectern_git::Oid;
use lectern_kernel::testing::{ManualClock, TestKernel};
use lectern_kernel::{Kernel, Plugin, PluginState};
use lectern_logging::testing::{Capture, capture};
use lectern_services::changes::{ChangePath, RefreshHint};
use lectern_services::project::Projects;
use lectern_services::review::{
    LoadState, Need, Review, ReviewCommand, ReviewEvent, ReviewId, ReviewSnapshot, ReviewTarget,
    RowsKey,
};
use lectern_services::session::SessionSource;

use super::{ReviewDeps, plugin};
use crate::engine::Engine;
use crate::engine::round::Kind;
use crate::engine::tests::{Fx, NOW, T, edit, ms, replace, session, target, turn};
use crate::testing::FakeSessions;

struct Rig {
    fx: Fx,
    sessions: Arc<FakeSessions>,
    kernel: TestKernel,
    executor: Executor,
    driver: Driver,
    backend: Arc<FakeBackend>,
    clock: ManualClock,
    engine: Arc<Engine>,
    review: Arc<dyn Review>,
    data_dir: std::path::PathBuf,
    sid: &'static str,
}

/// The sessions of the tests that read the log: a UUID each, so that their
/// records are told from the other tests' in a shared capture.
const SID_ASKS: &str = "5e55e55e-2222-4222-8222-0000000000b2";
const SID_MERGE: &str = "5e55e55e-2222-4222-8222-0000000000b3";

/// The records with message `msg` whose line holds `needle`.
fn records(log: &Capture, msg: &str, needle: &str) -> Vec<String> {
    let msg = format!("\"msg\":\"{msg}\"");
    log.lines()
        .into_iter()
        .filter(|l| l.contains(&msg) && l.contains(needle))
        .collect()
}

/// The raw JSON text of field `name` in `line`.
fn field<'a>(line: &'a str, name: &str) -> &'a str {
    let start = line.find(&format!("\"{name}\":")).expect("field") + name.len() + 3;
    let rest = &line[start..];
    let end = if let Some(text) = rest.strip_prefix('"') {
        text.find('"').expect("closing quote") + 2
    } else {
        rest.find([',', '}']).expect("end")
    };
    &rest[..end]
}

/// One commit with `a.txt` and `b.txt`; the session `sid` adds a line to
/// `a.txt` in its first turn, and the worktree has it.
fn fixture(sid: &str) -> (Fx, Arc<FakeSessions>) {
    let fx = Fx::new();
    let c1 = fx.commit(&[("a.txt", "one\n"), ("b.txt", "bee\n")], None, T - 1000);
    fx.head(
        "refs/heads/main",
        c1,
        &[("a.txt", "one\n"), ("b.txt", "bee\n")],
    );
    fx.reflog(&[(None::<Oid>, c1, T - 1000, "commit (initial): one")]);
    fx.write("a.txt", "one\ntwo\n");
    fx.write("b.txt", "bee\n");
    let sessions = fx.sessions(vec![session(
        sid,
        &fx.root(),
        ms(T),
        vec![turn(
            "t1",
            ms(T + 10),
            vec![edit(
                "c1",
                ms(T + 20),
                vec![replace("a.txt", "one\n", "one\ntwo\n")],
            )],
        )],
    )]);
    (fx, sessions)
}

impl Rig {
    fn new() -> Rig {
        Rig::with_session("s")
    }

    fn with_session(sid: &'static str) -> Rig {
        let (fx, sessions) = fixture(sid);
        Rig::build(fx, sessions, None, sid)
    }

    fn build(
        fx: Fx,
        sessions: Arc<FakeSessions>,
        ui: Option<std::thread::ThreadId>,
        sid: &'static str,
    ) -> Rig {
        sessions.locate(&*fx.projects);
        let backend = FakeBackend::new();
        let executor = Executor::manual();
        let (watcher, driver) = manual(backend.clone(), executor.clone(), WatchConfig::default());
        let data_dir = fx.dir.path().join("review");
        let clock = ManualClock::new();
        let (_, review) = plugin(ReviewDeps {
            executor: executor.clone(),
            cache: Cache::new(lectern_cache::Config {
                root: fx.dir.path().join("cache"),
                budgets: Budgets::default(),
                ui_thread: ui,
            }),
            watcher,
            data_dir: data_dir.clone(),
            git_ceilings: Arc::from(Vec::new()),
        });
        let review = review
            .with_clock(Arc::new(clock.clone()))
            .with_wall_clock(|| NOW)
            .with_repository_config_only();
        let provided = review.provided.clone();
        let kernel = TestKernel::new(review)
            .mock::<dyn Projects>(fx.projects.clone())
            .mock::<dyn SessionSource>(sessions.clone());
        kernel.activate().unwrap();
        let (engine, service) = provided.lock().unwrap().clone().unwrap();
        Rig {
            fx,
            sessions,
            kernel,
            executor,
            driver,
            backend,
            clock,
            engine,
            review: service,
            data_dir,
            sid,
        }
    }

    fn target(&self) -> ReviewTarget {
        target(self.sid)
    }

    fn kernel(&self) -> Arc<Kernel> {
        self.kernel.inner().unwrap()
    }

    /// Runs fs_watch's queue and the executor until neither has work.
    fn run(&self) {
        self.driver.run_until_idle();
    }

    /// Ends every coalescing window (the watch's first batch included),
    /// then runs what that started.
    fn windows(&self) {
        // The events are taken in first, so that their windows start before
        // the clock moves.
        self.driver.run_until_idle();
        self.driver.advance(Duration::from_secs(5));
    }

    /// Opens the session's review and runs it to its last snapshot, the
    /// first (rescan) batch of its watches included.
    fn open_settled(&self) -> (ReviewId, Arc<ReviewSnapshot>) {
        let id = self.review.open(self.target());
        self.run();
        self.windows();
        let snapshot = self.review.snapshot(id).unwrap();
        assert_eq!(snapshot.state, LoadState::Ready, "{:?}", snapshot.state);
        (id, snapshot)
    }

    fn changed(&self) -> Vec<(ReviewId, u64)> {
        self.kernel
            .emitted::<ReviewEvent>()
            .into_iter()
            .filter_map(|e| match e {
                ReviewEvent::Changed { id, generation } => Some((id, generation)),
                _ => None,
            })
            .collect()
    }

    /// An event of the backend at the worktree root, as FSEvents reports a
    /// write to `path`.
    fn touch(&self, path: &str) {
        self.backend
            .emit(self.fx.repo.root(), &[(path, Flags::FILE)]);
    }

    fn quiescent(&self) -> bool {
        let kernel = self.kernel();
        kernel.barrier();
        let start = std::time::Instant::now();
        while !kernel.is_quiescent() {
            if start.elapsed() > Duration::from_secs(2) {
                return false;
            }
            std::thread::yield_now();
        }
        true
    }
}

#[test]
fn plugin_provides_review_and_activation_does_nothing() {
    let rig = Rig::new();
    let status = rig
        .kernel()
        .status()
        .into_iter()
        .find(|s| s.id == "plugin-under-test")
        .unwrap();
    assert_eq!(status.state, PluginState::Active);
    let manifest = plugin(ReviewDeps {
        executor: rig.executor.clone(),
        cache: Cache::new(lectern_cache::Config {
            root: rig.fx.dir.path().join("cache2"),
            budgets: Budgets::default(),
            ui_thread: None,
        }),
        watcher: manual(
            FakeBackend::new(),
            rig.executor.clone(),
            WatchConfig::default(),
        )
        .0,
        data_dir: rig.data_dir.clone(),
        git_ceilings: Arc::from(Vec::new()),
    })
    .1
    .manifest();
    assert_eq!(manifest.provides, [lectern_kernel::service::<dyn Review>()]);
    assert_eq!(
        manifest.requires,
        [
            lectern_kernel::service::<dyn Projects>(),
            lectern_kernel::service::<dyn SessionSource>()
        ]
    );
    assert_eq!(
        manifest.activation,
        [lectern_kernel::Activation::AfterFirstFrame]
    );
    // No task, no watch, no file: `activate` built empty tables.
    assert_eq!(rig.executor.run_until_idle(), 0);
    assert!(rig.backend.targets().is_empty());
    assert!(!rig.data_dir.exists());
    assert!(rig.quiescent());
}

#[test]
fn plugin_open_returns_at_once_and_the_snapshot_goes_from_loading_to_ready() {
    let rig = Rig::new();
    let id = rig.review.open(rig.target());
    // Nothing ran: the call only queued the first round.
    let first = rig.review.snapshot(id).unwrap();
    assert_eq!(first.state, LoadState::Loading);
    assert_eq!(first.generation, 0);
    assert!(rig.changed().is_empty());
    // The same target is the same review.
    assert_eq!(rig.review.open(rig.target()), id);

    rig.run();
    let ready = rig.review.snapshot(id).unwrap();
    assert_eq!(ready.state, LoadState::Ready);
    assert_eq!(ready.files.len(), 1);
    assert_eq!(ready.totals.added, 1);
    let events = rig.changed();
    assert!(!events.is_empty());
    assert!(events.iter().all(|(event, _)| *event == id));
    assert!(
        events.windows(2).all(|w| w[0].1 < w[1].1),
        "generations rise: {events:?}"
    );
    assert_eq!(
        events.last().unwrap().1,
        rig.review.snapshot(id).unwrap().generation
    );
    // A review is watched through its own subscriptions.
    assert!(
        rig.backend.is_armed(rig.fx.repo.root()),
        "{:?}",
        rig.backend.targets()
    );
}

#[test]
fn plugin_a_watch_batch_refreshes_the_changed_path() {
    let rig = Rig::new();
    let (id, before) = rig.open_settled();
    assert_eq!(before.totals.added, 1);
    let events = rig.changed().len();

    rig.fx.write("a.txt", "one\ntwo\nthree\n");
    rig.touch("a.txt");
    rig.windows();

    let after = rig.review.snapshot(id).unwrap();
    assert!(after.generation > before.generation);
    assert_eq!(after.totals.added, 2, "the new line is in the change set");
    assert!(rig.changed().len() > events);
}

#[test]
fn plugin_a_path_batch_asks_for_those_paths_and_a_rescan_for_all() {
    // The capture is shared by the tests of the process: it holds what the
    // opening asked for, from the first batch of the watches, already.
    let log = capture();
    let rig = Rig::with_session(SID_ASKS);
    let (_, _) = rig.open_settled();
    let opening = records(log, "refresh requested", SID_ASKS).len();
    assert_eq!(
        opening, 1,
        "the first batch of the watches: one full refresh"
    );
    rig.touch("a.txt");
    rig.windows();
    rig.backend
        .emit(rig.fx.repo.root(), &[("", Flags::DIR | Flags::RESCAN)]);
    rig.windows();
    let asked: Vec<(String, String)> = records(log, "refresh requested", SID_ASKS)
        .iter()
        .skip(opening)
        .map(|l| (field(l, "how").to_owned(), field(l, "paths").to_owned()))
        .collect();
    let text = |how: &str, paths: &str| (how.to_owned(), paths.to_owned());
    assert_eq!(
        asked,
        [text("\"paths\"", "1"), text("\"full\"", "0")],
        "{:#?}",
        log.lines()
    );
}

#[test]
fn plugin_sessions_changed_refreshes_the_turns() {
    let rig = Rig::new();
    let (id, before) = rig.open_settled();
    assert_eq!(before.turns.as_ref().unwrap().len(), 1);

    rig.sessions.append(
        "s",
        &json!({"id": "t2", "at": ms(T + 100), "prompt": "again", "items": []}).to_string(),
    );
    for event in rig.sessions.take_events() {
        rig.kernel().emitter().emit(event);
    }
    rig.run();

    let after = rig.review.snapshot(id).unwrap();
    assert_eq!(after.turns.as_ref().unwrap().len(), 2);
    assert!(after.generation > before.generation);
}

#[test]
fn plugin_requests_during_a_round_become_one_round() {
    let log = capture();
    let rig = Rig::with_session(SID_MERGE);
    let (id, before) = rig.open_settled();
    rig.fx.write("a.txt", "one\ntwo\nthree\n");
    rig.fx.write("b.txt", "bee\nbuzz\n");

    // The first request starts a round; nothing runs in between, so the
    // next two wait for it and are merged: the paths of both, one round.
    let paths = |p: &str| {
        Kind::Refresh(RefreshHint::Paths(
            vec![ChangePath(Arc::from(p.as_bytes()))].into(),
        ))
    };
    let rounds = || records(log, "review round", SID_MERGE).len();
    let before_rounds = rounds();
    rig.engine.request(id, paths("a.txt"));
    rig.engine.request(id, paths("b.txt"));
    rig.engine.request(id, paths("b.txt"));
    rig.run();

    let after = rig.review.snapshot(id).unwrap();
    assert_eq!(after.totals.files, 2);
    assert_eq!(after.totals.added, 3, "a.txt and b.txt were both compared");
    assert_eq!(
        rounds() - before_rounds,
        2,
        "the round the first request started, and one for the other two"
    );
    assert!(after.generation > before.generation);
}

#[test]
fn plugin_kernel_is_quiescent_only_after_the_results_are_published() {
    let rig = Rig::new();
    assert!(rig.quiescent());
    let id = rig.review.open(rig.target());
    // The round is queued, not run: the kernel has work to wait for.
    rig.kernel().barrier();
    assert!(!rig.kernel().is_quiescent());
    rig.run();
    let snapshot = rig.review.snapshot(id).unwrap();
    assert_eq!(snapshot.state, LoadState::Ready);
    // The watches' first batches are part of the work too.
    rig.windows();
    assert!(rig.quiescent());
}

#[test]
fn plugin_close_keeps_the_review_for_30_seconds_then_releases_it() {
    let rig = Rig::new();
    let (id, before) = rig.open_settled();
    let calls = rig.sessions.calls("s");
    let events = rig.changed().len();
    assert_eq!(rig.engine.kept(), 1);

    rig.review.close(id);
    // Closed: no snapshot for the ID, but the state is kept.
    assert!(rig.review.snapshot(id).is_none());
    assert_eq!(rig.engine.kept(), 1);
    rig.clock.advance(Duration::from_secs(29));
    rig.run();
    assert_eq!(rig.engine.kept(), 1);

    // Back within the 30 seconds: the same review, as it was, nothing
    // computed again.
    assert_eq!(rig.review.open(rig.target()), id);
    rig.run();
    let back = rig.review.snapshot(id).unwrap();
    assert_eq!(back.generation, before.generation);
    assert_eq!(rig.sessions.calls("s"), calls);
    assert_eq!(rig.changed().len(), events);

    // The first close's timer ends now and finds the review held.
    rig.clock.advance(Duration::from_secs(2));
    rig.run();
    assert_eq!(
        rig.engine.kept(),
        1,
        "held again: the old timer does nothing"
    );

    // Closed again: 30 seconds from this close, not the first.
    rig.review.close(id);
    rig.clock.advance(Duration::from_secs(29));
    rig.run();
    assert_eq!(rig.engine.kept(), 1);
    assert!(rig.backend.is_armed(rig.fx.repo.root()));
    rig.clock.advance(Duration::from_secs(1));
    rig.run();
    assert_eq!(
        rig.engine.kept(),
        0,
        "snapshot, change list, state released"
    );
    assert!(
        !rig.backend.is_armed(rig.fx.repo.root()),
        "the watches went with it: {:?}",
        rig.backend.targets()
    );
    assert_eq!(rig.engine.version_bytes(), 0);
    // A new open is a new review.
    let again = rig.review.open(rig.target());
    assert_ne!(again, id);
    assert_eq!(
        rig.review.snapshot(again).unwrap().state,
        LoadState::Loading
    );
    rig.run();
    assert!(rig.quiescent());
}

#[test]
fn plugin_a_kept_review_is_still_refreshed_by_its_watches() {
    let rig = Rig::new();
    let (id, _) = rig.open_settled();
    rig.review.close(id);
    rig.fx.write("a.txt", "one\ntwo\nthree\n");
    rig.touch("a.txt");
    rig.windows();
    // Reopened within the 30 seconds it is current without a new fetch.
    assert_eq!(rig.review.open(rig.target()), id);
    assert_eq!(rig.review.snapshot(id).unwrap().totals.added, 2);
}

#[test]
fn plugin_commands_on_the_ui_thread_do_no_io() {
    let (fx, sessions) = fixture("s");
    // The cache refuses (panics, in a debug build) any call from this
    // thread, which stands for the UI thread; the tasks run on another.
    let rig = Rig::build(fx, sessions, Some(std::thread::current().id()), "s");
    let off_thread = |rig: &Rig| {
        let driver = &rig.driver;
        std::thread::scope(|s| {
            s.spawn(|| driver.run_until_idle()).join().unwrap();
        });
    };
    let id = rig.review.open(rig.target());
    off_thread(&rig);
    let snapshot = rig.review.snapshot(id).unwrap();
    assert_eq!(snapshot.state, LoadState::Ready);
    assert!(!rig.data_dir.exists(), "reading the log creates nothing");

    let file = snapshot.files[0].key.clone();
    let prompt = snapshot.turns.as_ref().unwrap()[0].prompt.unwrap();
    let rows = RowsKey::Final {
        file: file.clone(),
        context: 3,
    };
    rig.review
        .command(id, ReviewCommand::MarkSeen(file.clone()));
    let marked = rig.review.snapshot(id).unwrap();
    assert!(
        marked.generation > snapshot.generation,
        "swapped before the call returned"
    );
    assert_eq!(marked.progress.seen, 1);
    rig.review.command(id, ReviewCommand::MarkUnseen(file));
    rig.review.command(id, ReviewCommand::LooksGood);
    rig.review.command(
        id,
        ReviewCommand::Prepare(vec![Need::Rows(rows.clone()), Need::Text(prompt)].into()),
    );
    assert!(rig.review.rows(id, &rows).is_none(), "prepared by a task");
    assert!(rig.review.text(id, &prompt).is_none());
    rig.review.close(id);
    assert_eq!(rig.review.open(rig.target()), id);
    // The marks are in the snapshot already; the log is written by a task.
    assert!(!rig.data_dir.exists());

    off_thread(&rig);
    assert!(rig.review.rows(id, &rows).is_some());
    assert!(rig.review.text(id, &prompt).is_some());
    assert!(
        rig.data_dir.exists(),
        "the marks were written in the background"
    );
}

#[test]
fn plugin_deactivation_drops_the_reviews_and_their_watches() {
    let rig = Rig::new();
    let (_, _) = rig.open_settled();
    assert!(rig.backend.is_armed(rig.fx.repo.root()));
    rig.kernel.deactivate();
    rig.run();
    assert!(!rig.backend.is_armed(rig.fx.repo.root()));
    assert_eq!(rig.engine.kept(), 0);
}

#[test]
fn plugin_overview_and_brief_rules_are_empty_until_step_5() {
    let rig = Rig::new();
    let project = rig.fx.projects.key().clone();
    assert!(rig.review.overview(&project).is_none());
    drop(rig.review.watch_overview(&project));
    assert_eq!(rig.executor.run_until_idle(), 0, "no task for it");
}
