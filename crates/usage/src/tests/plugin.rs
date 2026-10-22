//! `plugin_*` (docs/design/usage.md「数据流与线程」): the plugin under a
//! `TestKernel`, a manual executor and manual clocks.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lectern_executor::Executor;
use lectern_kernel::testing::TestKernel;
use lectern_kernel::{Busy, Kernel};
use lectern_services::project::{OpenRequest, ProjectEvent, ProjectKey, Projects};
use lectern_services::session::{Agent, Positioned, Record, SessionSource, Timestamp};
use lectern_services::usage::{
    AllUsage, ProjectRef, Usage, UsageDegraded, UsageEvent, UsageQuery, UsageSnapshot, UsageState,
    Window,
};

use super::support::*;
use crate::engine::{Engine, PACE};
use crate::testing::{FakeProjects, FakeSessions, ManualTime, TableZone};
use crate::time::WALL_CHECK;
use crate::{UsageDeps, plugin};

/// The rig's now: 2026-10-22T12:00Z.
const NOW: i64 = OCT_20 + 2 * DAY + 12 * HOUR;

pub(crate) struct Rig {
    pub(crate) kernel: TestKernel,
    pub(crate) executor: Executor,
    pub(crate) time: ManualTime,
    pub(crate) dir: tempfile::TempDir,
    pub(crate) sessions: Arc<FakeSessions>,
    pub(crate) projects: Arc<FakeProjects>,
    pub(crate) engine: Arc<Engine>,
}

impl Rig {
    /// `setup` declares projects before the plugin starts.
    pub(crate) fn new(sessions: FakeSessions, setup: impl FnOnce(&FakeProjects)) -> Rig {
        let projects = Arc::new(FakeProjects::new());
        setup(&projects);
        Rig::build(
            Arc::new(sessions),
            projects,
            tempfile::tempdir().unwrap(),
            ManualTime::new(NOW),
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
            ..
        } = self;
        Rig::build(sessions, projects, dir, time)
    }

    fn build(
        sessions: Arc<FakeSessions>,
        projects: Arc<FakeProjects>,
        dir: tempfile::TempDir,
        time: ManualTime,
    ) -> Rig {
        let executor = Executor::manual();
        let (_, usage) = plugin(UsageDeps {
            executor: executor.clone(),
            data_dir: dir.path().join("usage"),
            zone: Arc::new(TableZone::fixed(0)),
        });
        let usage = usage.with_time(&time);
        let slot = usage.engine.clone();
        let kernel = TestKernel::new(usage)
            .mock::<dyn Projects>(projects.clone())
            .mock::<dyn SessionSource>(sessions.clone());
        kernel.activate().unwrap();
        let engine = slot.lock().unwrap().clone().unwrap();
        Rig {
            kernel,
            executor,
            time,
            dir,
            sessions,
            projects,
            engine,
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
            if self.executor.run_until_idle() > 0 {
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
    let rig = read_rig(logs.path());
    let total = rig.all().total;
    let older = rig.sessions.reads("older");
    let rig = rig.restart(|fake| fake.set_adapter(2));
    // The probe of the newest session replays it from the start; the other
    // session of that source follows, paced.
    let all = rig.all();
    assert!(
        matches!(all.header.state, UsageState::Backfilling { .. }),
        "{:?}",
        all.header.state
    );
    for _ in 0..20 {
        rig.advance(PACE);
    }
    assert!(rig.sessions.reads("older") > older);
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
