//! `plugin_*` (docs/design/usage.md「数据流与线程」): the plugin under a
//! `TestKernel`, a manual executor and manual clocks.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lectern_executor::Executor;
use lectern_fs_watch::Config;
use lectern_fs_watch::testing::{Driver, FakeBackend, manual};
use lectern_kernel::testing::TestKernel;
use lectern_kernel::{Busy, Kernel};
use lectern_services::project::{OpenRequest, ProjectEvent, ProjectKey, Projects};
use lectern_services::session::{Agent, Positioned, Record, SessionSource, Timestamp};
use lectern_services::usage::{
    AllUsage, ProjectRef, Usage, UsageDegraded, UsageEvent, UsageQuery, UsageSnapshot, UsageState,
    Window,
};

use super::support::*;
use crate::engine::{Engine, FactsReader, PACE, git_facts};
use crate::testing::{FakeGit, FakeProjects, FakeSessions, ManualTime, TableZone};
use crate::time::WALL_CHECK;
use crate::{UsageDeps, plugin};

/// The rig's now: 2026-10-22T12:00Z.
const NOW: i64 = OCT_20 + 2 * DAY + 12 * HOUR;

pub(crate) struct Rig {
    pub(crate) kernel: TestKernel,
    /// Runs the watcher's queue and the executor.
    pub(crate) driver: Driver,
    pub(crate) backend: Arc<FakeBackend>,
    pub(crate) time: ManualTime,
    pub(crate) dir: tempfile::TempDir,
    pub(crate) sessions: Arc<FakeSessions>,
    pub(crate) projects: Arc<FakeProjects>,
    pub(crate) engine: Arc<Engine>,
    /// What the repositories say.
    pub(crate) git: Arc<FakeGit>,
    facts: FactsReader,
    /// Paths that do not exist for the plugin; none at first (the paths of
    /// the fake projects are not on the disk).
    pub(crate) vanished: Arc<std::sync::Mutex<Vec<PathBuf>>>,
}

impl Rig {
    /// `setup` declares projects before the plugin starts.
    pub(crate) fn new(sessions: FakeSessions, setup: impl FnOnce(&FakeProjects)) -> Rig {
        Rig::with_facts(sessions, setup, git_facts())
    }

    /// The same with a table of what the repositories say.
    pub(crate) fn with_git(
        sessions: FakeSessions,
        setup: impl FnOnce(&FakeProjects),
        git: Arc<FakeGit>,
    ) -> Rig {
        let projects = Arc::new(FakeProjects::new());
        setup(&projects);
        Rig::build(
            Arc::new(sessions),
            projects,
            tempfile::tempdir().unwrap(),
            ManualTime::new(NOW),
            git_facts(),
            Arc::default(),
            git,
        )
    }

    /// The same with another reader of what identifies a repository.
    pub(crate) fn with_facts(
        sessions: FakeSessions,
        setup: impl FnOnce(&FakeProjects),
        facts: FactsReader,
    ) -> Rig {
        let projects = Arc::new(FakeProjects::new());
        setup(&projects);
        Rig::build(
            Arc::new(sessions),
            projects,
            tempfile::tempdir().unwrap(),
            ManualTime::new(NOW),
            facts,
            Arc::default(),
            Arc::default(),
        )
    }

    /// Stops the plugin, runs `between` (changes while Lectern is not
    /// running), and starts it again on the same data directory, sessions
    /// and clocks.
    pub(crate) fn restart(self, between: impl FnOnce(&FakeSessions)) -> Rig {
        self.kernel.deactivate();
        between(&self.sessions);
        let Rig {
            dir,
            sessions,
            projects,
            time,
            facts,
            vanished,
            git,
            kernel,
            driver,
            backend,
            engine,
        } = self;
        // The first run is gone (its engine has stopped) before the second
        // begins.
        drop((kernel, driver, backend, engine));
        Rig::build(sessions, projects, dir, time, facts, vanished, git)
    }

    fn build(
        sessions: Arc<FakeSessions>,
        projects: Arc<FakeProjects>,
        dir: tempfile::TempDir,
        time: ManualTime,
        facts: FactsReader,
        vanished: Arc<std::sync::Mutex<Vec<PathBuf>>>,
        git: Arc<FakeGit>,
    ) -> Rig {
        let executor = Executor::manual();
        let backend = FakeBackend::new();
        let (watcher, driver) = manual(backend.clone(), executor.clone(), Config::default());
        let (_, usage) = plugin(UsageDeps {
            executor: executor.clone(),
            watcher,
            data_dir: dir.path().join("usage"),
            zone: Arc::new(TableZone::fixed(0)),
        });
        let gone = vanished.clone();
        let usage = usage
            .with_time(&time)
            .with_facts(facts.clone())
            .with_reads(git.clone())
            .with_exists(Arc::new(move |path| {
                !gone.lock().unwrap().iter().any(|p| p == path)
            }));
        let slot = usage.engine.clone();
        let kernel = TestKernel::new(usage)
            .mock::<dyn Projects>(projects.clone())
            .mock::<dyn SessionSource>(sessions.clone());
        kernel.activate().unwrap();
        let engine = slot.lock().unwrap().clone().unwrap();
        Rig {
            kernel,
            driver,
            backend,
            time,
            dir,
            sessions,
            projects,
            engine,
            git,
            facts,
            vanished,
        }
    }

    pub(crate) fn kernel(&self) -> Arc<Kernel> {
        self.kernel.inner().unwrap()
    }

    pub(crate) fn db(&self) -> PathBuf {
        self.dir.path().join("usage/usage.db")
    }

    /// Runs tasks until none is left and the plugin has seen the kernel's
    /// current quiescence. (A kernel `barrier` would itself be a busy
    /// moment and restart the pacing.)
    pub(crate) fn settle(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if self.driver.run_until_idle() > 0 {
                continue;
            }
            match self.kernel().quiescent_epoch() {
                Some(epoch) if self.engine.handled_quiet() == Some(epoch) => return,
                _ => std::thread::yield_now(),
            }
        }
        panic!("never settled");
    }

    /// Moves the clocks, then settles.
    pub(crate) fn advance(&self, by: Duration) {
        self.time.advance(by);
        self.settle();
    }

    pub(crate) fn emit_changed(&self, ids: &[&str]) {
        self.kernel().emitter().emit(self.sessions.changed(ids));
        self.settle();
    }

    pub(crate) fn usage(&self) -> &dyn Usage {
        &*self.engine
    }

    /// The all-projects window of the last 7 days, watched until computed.
    pub(crate) fn all(&self) -> AllUsage {
        let query = UsageQuery::AllProjects { window: Window::D7 };
        let _interest = self.usage().watch(query.clone());
        self.settle();
        match self.usage().snapshot(&query).as_deref() {
            Some(UsageSnapshot::All(a)) => a.clone(),
            other => panic!("{other:?}"),
        }
    }
}

/// A usage record of `input` tokens at line `pos`.
fn tokens_at(pos: u64, at: i64, id: &str, input: u64) -> Positioned<Record> {
    claude(pos, at, None, id, [input, 0, 0, 0])
}

/// A session last active `age` before now, with `n` records of 1 input
/// token on lines of 3 MiB (an 8 MiB read takes three of them).
fn session(fake: &FakeSessions, id: &str, age: i64, n: u64) {
    let at = NOW - age;
    fake.add(id, Path::new("/work/app"), at, None);
    let mut records = vec![context(0, at, "/work/app")];
    for i in 0..n {
        records.push(tokens_at((i + 1) * (3 << 20), at, &format!("{id}-{i}"), 1));
    }
    fake.push(id, records);
}

fn sources() -> FakeSessions {
    FakeSessions::new("session-claude-code", Agent::ClaudeCode)
}

fn app_key() -> ProjectKey {
    ProjectKey::Git {
        common_dir: Path::new("/work/app/.git").into(),
    }
}

fn open_app(p: &FakeProjects) {
    let key = p.repo(Path::new("/work/app"), Path::new("/work/app/.git"));
    p.set_open(&key, true);
}

#[test]
fn plugin_provides_usage_and_opens_nothing_before_it_runs() {
    let fake = sources();
    session(&fake, "s", 2 * HOUR, 1);
    let rig = Rig::new(fake, |_| {});
    // Activated: the service is there, but no task has run, so the
    // database does not exist yet.
    assert!(!rig.db().exists());
    let query = UsageQuery::AllProjects { window: Window::D7 };
    let _interest = rig.usage().watch(query.clone());
    assert!(rig.usage().snapshot(&query).is_none());
    assert_eq!(rig.usage().limits().windows.len(), 0);
    rig.settle();
    assert!(rig.db().exists());
    assert!(rig.usage().snapshot(&query).is_some());
}

#[test]
fn plugin_backfill_waits_for_ten_quiet_seconds_per_batch() {
    let fake = sources();
    // Two inactive sessions of several batches each.
    session(&fake, "old", 2 * DAY, 4);
    session(&fake, "older", 3 * DAY, 4);
    let rig = Rig::new(fake, |_| {});
    rig.settle();
    let reads = || rig.sessions.total_reads();
    assert_eq!(reads(), 0, "nothing before the kernel stays quiet");
    rig.advance(PACE - Duration::from_millis(1));
    assert_eq!(reads(), 0);
    rig.advance(Duration::from_millis(1));
    assert_eq!(reads(), 1);
    // The newest session first.
    assert_eq!(rig.sessions.reads("old"), 1);
    // Between batches the kernel is quiescent again.
    assert!(rig.kernel().is_quiescent());
    rig.advance(PACE / 2);
    assert_eq!(reads(), 1);
    // A busy moment (an event's dispatch) restarts the count: the timer of
    // the earlier quiet stretch fires and is ignored.
    rig.emit_changed(&[]);
    rig.advance(PACE / 2);
    assert_eq!(reads(), 1, "busy since the last quiescence");
    rig.advance(PACE / 2);
    assert_eq!(reads(), 2);
    // An older transition is ignored.
    rig.kernel().emitter().emit(Busy { seq: 0 });
    rig.settle();
    // To the end, one batch per quiet stretch.
    for _ in 0..20 {
        rig.advance(PACE);
    }
    assert_eq!(rig.engine.dirty_sessions(), 0);
    let all = rig.all();
    assert_eq!(all.total.input, 8);
    assert_eq!(all.header.state, UsageState::Ready);
}

#[test]
fn plugin_active_sessions_are_read_without_pacing() {
    let fake = sources();
    session(&fake, "live", 60_000, 6);
    session(&fake, "old", 2 * DAY, 2);
    let rig = Rig::new(fake, |_| {});
    rig.settle();
    // Batch after batch to the end, without waiting.
    assert!(rig.sessions.reads("live") >= 3);
    assert_eq!(rig.sessions.reads("old"), 0);
    // New records of the live session are read as soon as they are
    // announced.
    let record = tokens_at(40 << 20, NOW, "live-new", 5);
    rig.sessions.push("live", vec![record]);
    rig.emit_changed(&["live"]);
    let all = rig.all();
    assert_eq!(all.total.input, 11);
    assert!(matches!(all.header.state, UsageState::Backfilling { .. }));
}

#[test]
fn plugin_snapshot_on_the_ui_thread_does_no_io() {
    let fake = sources();
    session(&fake, "live", 60_000, 1);
    let rig = Rig::new(fake, open_app);
    let query = UsageQuery::Project {
        project: ProjectRef::Open(app_key()),
        window: Window::D7,
    };
    let interest = rig.usage().watch(query.clone());
    rig.settle();
    let store = rig.engine.store().unwrap();
    let before = store.io_count();
    for _ in 0..100 {
        let snapshot = rig.usage().snapshot(&query).expect("computed");
        let UsageSnapshot::Project(p) = &*snapshot else {
            panic!("a project snapshot");
        };
        assert_eq!(p.total.input, 1);
        assert_eq!(&*p.project.name, "app");
        let _ = rig.usage().limits();
    }
    assert_eq!(store.io_count(), before);
    // Every replacement was announced.
    let events = rig.kernel.emitted::<UsageEvent>();
    let announced =
        |e: &UsageEvent| matches!(e, UsageEvent::Changed { query: q, .. } if *q == query);
    assert!(events.iter().any(announced));
    // Dropping the interest drops the snapshot.
    drop(interest);
    assert!(rig.usage().snapshot(&query).is_none());
}

#[test]
fn plugin_project_events_update_the_lists() {
    let fake = sources();
    session(&fake, "live", 60_000, 1);
    let rig = Rig::new(fake, |p| {
        p.repo(Path::new("/work/app"), Path::new("/work/app/.git"));
    });
    rig.settle();
    assert!(rig.all().projects.is_empty());
    let key = app_key();
    rig.projects.set_open(&key, true);
    rig.kernel().emitter().emit(ProjectEvent::Opened {
        request: OpenRequest(1),
        result: Ok(key.clone()),
    });
    rig.settle();
    let listed = rig.all();
    assert_eq!(listed.projects.len(), 1);
    assert!(listed.projects[0].0.key.is_some());
    assert_eq!(listed.projects[0].1.input, 1);
    let closed = ProjectEvent::Closed { project: key };
    rig.kernel().emitter().emit(closed);
    rig.settle();
    let listed = rig.all();
    let still_listed = listed.projects[0].0.key.is_none();
    assert!(still_listed, "still listed, no longer open");
}

#[test]
fn plugin_window_snapshots_move_at_local_midnight() {
    let fake = sources();
    let start = NOW - 6 * DAY;
    fake.add("s", Path::new("/work/app"), start, None);
    // 2026-10-15T23:00Z: before the window (2026-10-16 to 10-22).
    let edge = tokens_at(100, start - 13 * HOUR, "edge", 3);
    fake.push("s", vec![context(0, start, "/work/app"), edge]);
    // Active, so it is read without pacing.
    fake.edit("s", |s| s.last_activity = Timestamp(NOW - 60_000));
    let rig = Rig::new(fake, |_| {});
    let query = UsageQuery::AllProjects { window: Window::D7 };
    let _interest = rig.usage().watch(query.clone());
    rig.settle();
    let total = |rig: &Rig| match rig.usage().snapshot(&query).as_deref() {
        Some(UsageSnapshot::All(a)) => a.total.input,
        other => panic!("{other:?}"),
    };
    assert_eq!(total(&rig), 0);
    // 2026-10-16T23:00Z: inside.
    let inside = tokens_at(200, start + 11 * HOUR, "inside", 5);
    rig.sessions.push("s", vec![inside]);
    rig.emit_changed(&["s"]);
    assert_eq!(total(&rig), 5);
    // At 2026-10-23T00:00Z the window drops 10-16.
    rig.advance(Duration::from_millis(12 * HOUR as u64));
    assert_eq!(total(&rig), 0);
}

#[test]
fn plugin_write_failure_degrades_until_the_next_change() {
    let fake = sources();
    session(&fake, "live", 60_000, 1);
    let rig = Rig::new(fake, |_| {});
    rig.settle();
    let store = rig.engine.store().unwrap();
    let conn = rusqlite::Connection::open(rig.db()).unwrap();
    let pages: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap();
    drop(conn);
    store.set_max_page_count(pages as u64);
    let big: Vec<_> = (0..300)
        .map(|i| tokens_at((10 << 20) + i * 100, NOW, &format!("big-{i}"), 1))
        .collect();
    rig.sessions.push("live", big);
    rig.emit_changed(&["live"]);
    let state = rig.all().header.state;
    let unwritable = matches!(state, UsageState::Degraded(UsageDegraded::Unwritable(_)));
    assert!(unwritable, "{state:?}");
    let reads = rig.sessions.reads("live");
    rig.advance(PACE * 3);
    assert_eq!(rig.sessions.reads("live"), reads, "no retry loop");
    // Space again; the next change retries and the cursor had not moved.
    store.set_max_page_count(1 << 30);
    rig.emit_changed(&["live"]);
    let all = rig.all();
    assert_eq!(all.total.input, 301);
    assert_eq!(all.header.state, UsageState::Ready);
}

/// Two inactive sessions with files in `logs`, read to their ends.
fn read_rig(logs: &Path) -> Rig {
    let fake = FakeSessions::with_files("session-claude-code", Agent::ClaudeCode, logs);
    session(&fake, "old", 2 * DAY, 4);
    session(&fake, "older", 3 * DAY, 4);
    let rig = Rig::new(fake, |_| {});
    rig.settle();
    for _ in 0..20 {
        rig.advance(PACE);
    }
    assert_eq!(rig.engine.dirty_sessions(), 0);
    assert_eq!(rig.all().header.state, UsageState::Ready);
    rig
}

#[test]
fn plugin_restart_reads_only_changed_logs() {
    let logs = tempfile::tempdir().unwrap();
    let rig = read_rig(logs.path());
    let total = rig.all().total;
    let reads = rig.sessions.total_reads();
    let old = rig.sessions.reads("old");

    // Nothing changed while stopped: Ready at once, no backfill. Only the
    // newest session is read once, unpaced (its source's adapter version).
    let rig = rig.restart(|_| {});
    let all = rig.all();
    assert_eq!(all.header.state, UsageState::Ready);
    assert_eq!(all.total, total);
    assert_eq!(rig.engine.dirty_sessions(), 0);
    assert_eq!(rig.sessions.total_reads(), reads + 1);
    assert_eq!(rig.sessions.reads("old"), old + 1, "the newest, once");
    // The first discovery announces every session: unchanged files are not
    // read, not even after quiet stretches.
    rig.emit_changed(&["old", "older"]);
    rig.advance(PACE * 3);
    assert_eq!(rig.sessions.total_reads(), reads + 1);

    // A log that grew while Lectern was not running is backfilled.
    let rig = rig.restart(|fake| {
        let at = NOW - 3 * DAY;
        fake.push("older", vec![tokens_at(13 << 20, at, "older-new", 5)]);
    });
    let all = rig.all();
    assert!(
        matches!(all.header.state, UsageState::Backfilling { .. }),
        "{:?}",
        all.header.state
    );
    rig.advance(PACE);
    let all = rig.all();
    assert_eq!(all.header.state, UsageState::Ready);
    assert_eq!(all.total.input, total.input + 5);
}

#[test]
fn plugin_adapter_upgrade_replays_unchanged_logs() {
    let logs = tempfile::tempdir().unwrap();
    let fake = FakeSessions::with_files("session-claude-code", Agent::ClaudeCode, logs.path());
    // Six inactive sessions of one batch each.
    let ids: Vec<String> = (0..6).map(|i| format!("s{i}")).collect();
    for (i, id) in ids.iter().enumerate() {
        session(&fake, id, (1 + i as i64) * HOUR, 1);
    }
    let rig = Rig::new(fake, |_| {});
    for _ in 0..20 {
        rig.advance(PACE);
    }
    assert_eq!(rig.engine.dirty_sessions(), 0);
    let total = rig.all().total;
    assert_eq!(total.input, 6);
    let reads: Vec<usize> = ids.iter().map(|id| rig.sessions.reads(id)).collect();

    let rig = rig.restart(|fake| fake.set_adapter(2));
    // The probe of the newest session replays it from the start; the other
    // sessions of that source follow, paced.
    let all = rig.all();
    assert!(
        matches!(all.header.state, UsageState::Backfilling { .. }),
        "{:?}",
        all.header.state
    );
    for _ in 0..20 {
        rig.advance(PACE);
    }
    // Each session is read once more: an old cursor's replay does not queue
    // the source again.
    for (id, before) in ids.iter().zip(&reads) {
        assert_eq!(rig.sessions.reads(id), before + 1, "{id}");
    }
    let all = rig.all();
    assert_eq!(all.header.state, UsageState::Ready);
    assert_eq!(all.total, total, "replayed, not counted twice");
}

#[test]
fn plugin_midnight_is_seen_after_a_sleep() {
    let fake = sources();
    let start = NOW - 6 * DAY;
    fake.add("s", Path::new("/work/app"), start, None);
    // 2026-10-16T23:00Z: inside the window until 2026-10-23.
    let inside = tokens_at(100, start + 11 * HOUR, "inside", 5);
    fake.push("s", vec![context(0, start, "/work/app"), inside]);
    fake.edit("s", |s| s.last_activity = Timestamp(NOW - 60_000));
    let rig = Rig::new(fake, |_| {});
    let query = UsageQuery::AllProjects { window: Window::D7 };
    let _interest = rig.usage().watch(query.clone());
    rig.settle();
    let total = |rig: &Rig| match rig.usage().snapshot(&query).as_deref() {
        Some(UsageSnapshot::All(a)) => a.total.input,
        other => panic!("{other:?}"),
    };
    assert_eq!(total(&rig), 5);
    // The Mac sleeps past midnight: the wall clock moves, the monotonic
    // clock does not.
    rig.time.jump_wall(12 * HOUR + 60_000);
    rig.settle();
    assert_eq!(total(&rig), 5, "no timer woke yet");
    // The next wall-clock check notices the day changed.
    rig.advance(WALL_CHECK);
    assert_eq!(total(&rig), 0);
}

#[test]
fn plugin_unopenable_database_degrades_the_snapshots() {
    let fake = sources();
    session(&fake, "live", 60_000, 1);
    let rig = Rig::new(fake, open_app);
    // The data directory cannot be created: a file is in its place.
    std::fs::write(rig.dir.path().join("usage"), b"not a directory").unwrap();
    let unwritable =
        |s: &UsageState| matches!(s, UsageState::Degraded(UsageDegraded::Unwritable(_)));
    let all = rig.all();
    assert!(unwritable(&all.header.state), "{:?}", all.header.state);
    assert_eq!(all.total.total(), 0);
    let query = UsageQuery::Project {
        project: ProjectRef::Open(app_key()),
        window: Window::D7,
    };
    let _interest = rig.usage().watch(query.clone());
    rig.settle();
    match rig.usage().snapshot(&query).as_deref() {
        Some(UsageSnapshot::Project(p)) => {
            assert!(unwritable(&p.header.state), "{:?}", p.header.state);
            assert_eq!(&*p.project.name, "app");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn plugin_stop_marks_the_database_closed() {
    let fake = sources();
    session(&fake, "live", 60_000, 1);
    let rig = Rig::new(fake, |_| {});
    rig.settle();
    rig.kernel.deactivate();
    let conn = rusqlite::Connection::open(rig.db()).unwrap();
    let sql = "SELECT value FROM meta WHERE name = 'clean'";
    let clean: i64 = conn.query_row(sql, [], |r| r.get(0)).unwrap();
    assert_eq!(clean, 1);
}

#[test]
fn plugin_archive_takes_one_step_per_quiet_stretch() {
    // A session of a year and a half ago: its month is archived after the
    // backfill, one step per quiet stretch, as the backfill's batches are.
    let fake = sources();
    let at = NOW - 500 * DAY;
    fake.add("ancient", Path::new("/work/app"), at, None);
    let mut records = vec![context(0, at, "/work/app")];
    for i in 0..3 {
        records.push(tokens_at(100 * (i + 1), at, &format!("ancient-{i}"), 1));
    }
    fake.push("ancient", records);
    let rig = Rig::new(fake, open_app);
    rig.settle();
    let query = UsageQuery::History {
        project: ProjectRef::Open(app_key()),
        from: lectern_services::usage::LocalDate {
            year: 2025,
            month: 1,
            day: 1,
        },
        to: lectern_services::usage::LocalDate {
            year: 2026,
            month: 10,
            day: 22,
        },
        days: true,
    };
    let _interest = rig.usage().watch(query.clone());
    rig.settle();
    let total = |rig: &Rig| -> u64 {
        match rig.usage().snapshot(&query).as_deref() {
            Some(UsageSnapshot::History(h)) => h.months.iter().map(|(_, t)| t.input).sum(),
            other => panic!("{other:?}"),
        }
    };
    let conn = rusqlite::Connection::open(rig.db()).unwrap();
    let parts = || -> i64 {
        conn.query_row("SELECT count(*) FROM cold_part", [], |r| r.get(0))
            .unwrap()
    };
    // The backfill's batch.
    rig.advance(PACE);
    assert_eq!(rig.engine.dirty_sessions(), 0);
    assert_eq!(total(&rig), 3);
    assert_eq!(parts(), 0, "nothing archived in the batch's stretch");
    // Then the plan, setting the month to archiving, and a segment per
    // stretch (the sources with their counted-source rows, the turns'
    // table is empty, the facts).
    let mut seen = vec![parts()];
    for _ in 0..6 {
        rig.advance(PACE / 2);
        assert_eq!(
            parts(),
            *seen.last().unwrap(),
            "only after ten quiet seconds"
        );
        rig.advance(PACE / 2);
        seen.push(parts());
    }
    assert_eq!(seen, [0, 0, 0, 2, 3, 3, 3], "{seen:?}");
    assert_eq!(total(&rig), 3, "the history is the same");
    let hot: i64 = conn
        .query_row("SELECT count(*) FROM usage_source", [], |r| r.get(0))
        .unwrap();
    assert_eq!(hot, 0);
}

/// The tokens of the project at `/work/<name>` in the all-projects list.
fn project_input(all: &AllUsage, name: &str) -> u64 {
    all.projects
        .iter()
        .find(|(info, _)| &*info.name == name)
        .map_or(0, |(_, t)| t.input)
}

#[test]
fn plugin_settles_a_turn_two_seconds_after_its_last_record() {
    use lectern_services::session::{
        EditOp, FileEdit, ToolCallId, ToolFinished, ToolKind, ToolStarted, ToolStatus, TurnId,
    };
    let fake = sources();
    // The session's one turn ended just now: its last result is 500 ms old.
    let at = NOW - 500;
    fake.add("live", Path::new("/work/app"), at - 1000, None);
    let path = PathBuf::from("/work/lib/a.rs");
    fake.edit_call(
        "live",
        "t1",
        "c1",
        std::slice::from_ref(&path),
        vec![FileEdit {
            path: path.clone(),
            op: EditOp::Write {
                content: "a\nb\n".into(),
                created: true,
            },
        }],
    );
    let records = vec![
        context(0, at - 1000, "/work/app"),
        turn(100, "t1", at - 1000),
        claude(200, at - 900, Some("t1"), "m1", [5, 0, 0, 0]),
        rec(
            300,
            Record::ToolStarted(ToolStarted {
                call: ToolCallId("c1".into()),
                turn: Some(TurnId("t1".into())),
                at: Timestamp(at - 600),
                name: "Edit".into(),
                kind: ToolKind::Edit {
                    paths: vec![path].into(),
                },
            }),
        ),
        rec(
            400,
            Record::ToolFinished(ToolFinished {
                call: ToolCallId("c1".into()),
                at: Timestamp(at),
                status: ToolStatus::Completed,
                exec_session: None,
            }),
        ),
    ];
    fake.push("live", records);
    let rig = Rig::new(fake, |p| {
        open_app(p);
        let lib = p.repo(Path::new("/work/lib"), Path::new("/work/lib/.git"));
        p.set_open(&lib, true);
    });
    rig.settle();
    // Read, not settled: it counts where the session runs.
    let all = rig.all();
    assert_eq!(
        (project_input(&all, "app"), project_input(&all, "lib")),
        (5, 0)
    );
    rig.advance(Duration::from_millis(1499));
    let all = rig.all();
    assert_eq!(project_input(&all, "lib"), 0, "not yet");
    rig.advance(Duration::from_millis(1));
    let all = rig.all();
    assert_eq!(
        (project_input(&all, "app"), project_input(&all, "lib")),
        (0, 5)
    );
    assert_eq!(all.total.input, 5);
}

#[test]
fn plugin_settles_the_turns_a_restart_left_over() {
    restart_leaves_a_turn(true);
}

#[test]
fn plugin_settles_the_last_turn_of_a_session_after_a_restart() {
    // One turn that no later prompt ended: its end is known only when the
    // session is looked at, and the log does not change.
    restart_leaves_a_turn(false);
}

/// A turn whose wait is not over when Lectern stops; `second_turn` says a
/// later prompt ended it (else the session just has no open turn).
fn restart_leaves_a_turn(second_turn: bool) {
    use lectern_services::session::{
        EditOp, FileEdit, ToolCallId, ToolFinished, ToolKind, ToolStarted, ToolStatus, TurnId,
    };
    let fake = sources();
    let at = NOW - 500;
    fake.add("live", Path::new("/work/app"), at - 1000, None);
    let path = PathBuf::from("/work/lib/a.rs");
    fake.edit_call(
        "live",
        "t1",
        "c1",
        std::slice::from_ref(&path),
        vec![FileEdit {
            path: path.clone(),
            op: EditOp::Write {
                content: "a\n".into(),
                created: true,
            },
        }],
    );
    fake.push(
        "live",
        vec![
            context(0, at - 1000, "/work/app"),
            turn(100, "t1", at - 1000),
            claude(200, at - 900, Some("t1"), "m1", [6, 0, 0, 0]),
            rec(
                300,
                Record::ToolStarted(ToolStarted {
                    call: ToolCallId("c1".into()),
                    turn: Some(TurnId("t1".into())),
                    at: Timestamp(at - 600),
                    name: "Edit".into(),
                    kind: ToolKind::Edit {
                        paths: vec![path].into(),
                    },
                }),
            ),
            rec(
                400,
                Record::ToolFinished(ToolFinished {
                    call: ToolCallId("c1".into()),
                    at: Timestamp(at),
                    status: ToolStatus::Completed,
                    exec_session: None,
                }),
            ),
        ],
    );
    if second_turn {
        // The next prompt ended the turn; the session is still open.
        fake.push("live", vec![turn(500, "t2", at + 100)]);
        fake.edit("live", |s| s.open_turn = true);
    }
    let setup = |p: &FakeProjects| {
        open_app(p);
        let lib = p.repo(Path::new("/work/lib"), Path::new("/work/lib/.git"));
        p.set_open(&lib, true);
    };
    let rig = Rig::new(fake, setup);
    rig.settle();
    let all = rig.all();
    assert_eq!(project_input(&all, "lib"), 0, "the wait is not over");
    // Lectern stops; while it is not running the wait passes.
    let time = rig.time.clone();
    let rig = rig.restart(move |_| time.advance(Duration::from_secs(3)));
    rig.settle();
    let all = rig.all();
    assert_eq!(
        (project_input(&all, "app"), project_input(&all, "lib")),
        (0, 6)
    );
}

#[test]
fn plugin_settles_an_older_sessions_last_turn_after_a_restart() {
    use lectern_services::session::{
        EditOp, FileEdit, ToolCallId, ToolFinished, ToolKind, ToolStarted, ToolStatus, TurnId,
    };
    // Two inactive sessions with files: the plugin reads the newest of the
    // unchanged ones again after a restart, never the other. The other has
    // one turn that no prompt ended; it is settled all the same.
    let logs = tempfile::tempdir().unwrap();
    let fake = FakeSessions::with_files("session-claude-code", Agent::ClaudeCode, logs.path());
    let (newer, older) = (NOW - 2 * DAY, NOW - 3 * DAY);
    fake.add("newer", Path::new("/work/app"), newer, None);
    fake.push(
        "newer",
        vec![
            context(0, newer, "/work/app"),
            turn(100, "n1", newer),
            claude(200, newer + 10, Some("n1"), "mn", [1, 0, 0, 0]),
        ],
    );
    fake.add("older", Path::new("/work/app"), older, None);
    let path = PathBuf::from("/work/lib/a.rs");
    fake.edit_call(
        "older",
        "o1",
        "c1",
        std::slice::from_ref(&path),
        vec![FileEdit {
            path: path.clone(),
            op: EditOp::Write {
                content: "a\nb\n".into(),
                created: true,
            },
        }],
    );
    fake.push(
        "older",
        vec![
            context(0, older, "/work/app"),
            turn(100, "o1", older),
            claude(200, older + 10, Some("o1"), "mo", [4, 0, 0, 0]),
            rec(
                300,
                Record::ToolStarted(ToolStarted {
                    call: ToolCallId("c1".into()),
                    turn: Some(TurnId("o1".into())),
                    at: Timestamp(older + 20),
                    name: "Edit".into(),
                    kind: ToolKind::Edit {
                        paths: vec![path].into(),
                    },
                }),
            ),
            rec(
                400,
                Record::ToolFinished(ToolFinished {
                    call: ToolCallId("c1".into()),
                    at: Timestamp(older + 30),
                    status: ToolStatus::Completed,
                    exec_session: None,
                }),
            ),
        ],
    );
    let setup = |p: &FakeProjects| {
        open_app(p);
        let lib = p.repo(Path::new("/work/lib"), Path::new("/work/lib/.git"));
        p.set_open(&lib, true);
    };
    let rig = Rig::new(fake, setup);
    rig.settle();
    // One batch per quiet stretch: both sessions are read, nothing is
    // settled yet.
    rig.advance(PACE);
    rig.advance(PACE);
    assert_eq!(rig.engine.dirty_sessions(), 0);
    let all = rig.all();
    assert_eq!(project_input(&all, "lib"), 0);
    let rig = rig.restart(|_| {});
    rig.settle();
    for _ in 0..4 {
        rig.advance(PACE);
    }
    let all = rig.all();
    assert_eq!(
        (project_input(&all, "app"), project_input(&all, "lib")),
        (1, 4)
    );
}

#[test]
fn plugin_stops_reading_an_index_that_keeps_failing() {
    use crate::engine::RETRY_MAX;
    use lectern_services::session::{
        EditOp, FileEdit, ToolCallId, ToolFinished, ToolKind, ToolStarted, ToolStatus, TurnId,
    };
    let fake = sources();
    let at = NOW - 60_000;
    fake.add("live", Path::new("/work/app"), at - 1000, None);
    let path = PathBuf::from("/work/lib/a.rs");
    fake.edit_call(
        "live",
        "t1",
        "c1",
        std::slice::from_ref(&path),
        vec![FileEdit {
            path: path.clone(),
            op: EditOp::Write {
                content: "a\n".into(),
                created: true,
            },
        }],
    );
    fake.push(
        "live",
        vec![
            context(0, at - 1000, "/work/app"),
            turn(100, "t1", at - 1000),
            claude(200, at - 900, Some("t1"), "m1", [6, 0, 0, 0]),
            rec(
                300,
                Record::ToolStarted(ToolStarted {
                    call: ToolCallId("c1".into()),
                    turn: Some(TurnId("t1".into())),
                    at: Timestamp(at - 600),
                    name: "Edit".into(),
                    kind: ToolKind::Edit {
                        paths: vec![path].into(),
                    },
                }),
            ),
            rec(
                400,
                Record::ToolFinished(ToolFinished {
                    call: ToolCallId("c1".into()),
                    at: Timestamp(at),
                    status: ToolStatus::Completed,
                    exec_session: None,
                }),
            ),
        ],
    );
    // The index cannot be read, and stays so.
    fake.fail_index(true);
    let rig = Rig::new(fake, |p| {
        open_app(p);
        let lib = p.repo(Path::new("/work/lib"), Path::new("/work/lib/.git"));
        p.set_open(&lib, true);
    });
    rig.settle();
    // An hour of retries with doubling waits: a few, then none.
    for _ in 0..60 {
        rig.advance(Duration::from_secs(60));
    }
    let tried = rig.sessions.index_reads("live");
    assert_eq!(
        tried,
        1 + RETRY_MAX as usize,
        "the first try and the retries"
    );
    for _ in 0..60 {
        rig.advance(Duration::from_secs(60));
    }
    assert_eq!(rig.sessions.index_reads("live"), tried, "no more tries");
    let all = rig.all();
    assert_eq!(project_input(&all, "lib"), 0);
    // The log changes and the index works again: the session is settled.
    rig.sessions.fail_index(false);
    rig.sessions.push(
        "live",
        vec![claude(500, NOW, Some("t1"), "m2", [1, 0, 0, 0])],
    );
    rig.emit_changed(&["live"]);
    // The session is no longer active: one batch per quiet stretch.
    for _ in 0..3 {
        rig.advance(PACE);
    }
    let all = rig.all();
    assert_eq!(project_input(&all, "lib"), 7);
}

/// A repository directory that exists (a watch of one that does not fails),
/// with a table of what the repository says.
fn real_repo() -> (tempfile::TempDir, PathBuf, Arc<FakeGit>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let reads = Arc::new(FakeGit::new());
    reads.set_heads(
        &root.join(".git"),
        &[(&root.join(".git"), Some("refs/heads/feature"))],
    );
    (dir, root, reads)
}

#[test]
fn plugin_watches_an_open_repository_and_writes_its_reflog_before_settling() {
    use super::branches::{Repo, batch};
    let (_dir, root, reads) = real_repo();
    let common = root.join(".git");
    let setup = |p: &FakeProjects| {
        let key = p.repo(&root, &common);
        p.set_open(&key, true);
    };
    let rig = Rig::with_git(sources(), setup, reads);
    rig.settle();
    let conn = rusqlite::Connection::open(rig.db()).unwrap();
    let count = |table: &str| -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    // The watch began: a span, the HEAD it started from, the branch's segment.
    assert_eq!(count("watch_span"), 1);
    assert_eq!(count("head_baseline"), 1);
    assert_eq!(count("segment"), 1);
    assert!(!rig.backend.targets().is_empty());
    let open = |conn: &rusqlite::Connection| -> i64 {
        conn.query_row(
            "SELECT count(*) FROM watch_span WHERE end_ns IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(open(&conn), 1);
    // A batch of the reflog is queued and written.
    let repo = Repo::new(common.to_str().unwrap());
    let record = repo.commit(&lectern_fs_watch::WorktreeName::Main, 1, 2, NOW + 1000);
    rig.engine
        .on_repo_batch(&repo.common, batch(vec![record], vec![]), None);
    rig.settle();
    assert_eq!(count("reflog"), 1);
    let last: i64 = conn
        .query_row("SELECT last_ns FROM watch_span", [], |r| r.get(0))
        .unwrap();
    assert_eq!(last, (NOW + 1100) * 1_000_000);
    // Closing the project ends the span and the watch.
    rig.kernel().emitter().emit(ProjectEvent::Closed {
        project: ProjectKey::Git {
            common_dir: common.as_path().into(),
        },
    });
    rig.settle();
    assert_eq!(open(&conn), 0);
}

#[test]
fn plugin_ends_the_spans_when_it_stops_and_a_crash_ends_them_at_the_last_record() {
    use super::branches::{Repo, batch};
    let (_dir, root, reads) = real_repo();
    let common = root.join(".git");
    let setup = |p: &FakeProjects| {
        let key = p.repo(&root, &common);
        p.set_open(&key, true);
    };
    let rig = Rig::with_git(sources(), setup, reads);
    rig.settle();
    let repo = Repo::new(common.to_str().unwrap());
    let record = repo.commit(&lectern_fs_watch::WorktreeName::Main, 1, 2, NOW + 1000);
    rig.engine
        .on_repo_batch(&repo.common, batch(vec![record], vec![]), None);
    rig.settle();
    let db = rig.db();
    let spans = |db: &Path| -> Vec<(i64, Option<i64>)> {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.prepare("SELECT last_ns, end_ns FROM watch_span ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let last = (NOW + 1100) * 1_000_000;
    assert_eq!(spans(&db), [(last, None)]);
    // Stopping ends the span where Lectern was (the clock), restarting
    // opens another.
    let rig = rig.restart(|_| {});
    rig.settle();
    let found = spans(&db);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0], (last, Some(NOW * 1_000_000)));
    assert_eq!(found[1].1, None);
    // A run that did not end its span (a crash) leaves it open: the next
    // start ends it at the last record read in it.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE watch_span SET end_ns = NULL", [])
        .unwrap();
    drop(conn);
    let opened = crate::store::Store::open(db.parent().unwrap(), NOW)
        .unwrap()
        .store;
    assert_eq!(opened.close_stale_spans().unwrap(), 2);
    let found = spans(&db);
    assert!(
        found.iter().all(|(last, end)| *end == Some(*last)),
        "{found:?}"
    );
    drop(rig);
}
