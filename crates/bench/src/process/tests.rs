//! `process_*`: the scheduling with a scripted `Launcher` / `ProcessInfo`
//! (docs/design/bench.md「测试」). Each launch runs a script on a thread in
//! place of the program: it connects to the session's socket like the probe
//! and answers the runner's requests.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::*;
use crate::wire::{ChildKind, FrameStats, Milestone, OpenTarget, Report, Request};

fn start() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000)
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Deadlines long enough for a script that answers at once, and a short
/// settle time.
fn quick() -> Limits {
    Limits {
        first_frame: Duration::from_secs(2),
        first_quiescent: Duration::from_secs(2),
        again: Duration::from_secs(2),
        reply: Duration::from_secs(2),
        quit: Duration::from_secs(2),
        settle: ms(50),
        poll: ms(5),
    }
}

// --- the fake processes ----------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kill {
    Group(u32),
    One(u32),
}

type Script = dyn Fn(&mut Probe) -> i32 + Send + Sync;

struct Inner {
    script: Box<Script>,
    next_pid: AtomicU32,
    launches: Mutex<Vec<Launch>>,
    live: AtomicUsize,
    max_live: AtomicUsize,
    kills: Mutex<Vec<Kill>>,
    alive: Mutex<HashSet<u32>>,
    /// `children` answers; a pid that is neither here nor alive has exited.
    tree: HashMap<u32, Vec<u32>>,
}

struct Fake(Arc<Inner>);

impl Fake {
    fn new(script: impl Fn(&mut Probe) -> i32 + Send + Sync + 'static) -> Fake {
        Fake::with_tree(HashMap::new(), script)
    }

    fn with_tree(
        tree: HashMap<u32, Vec<u32>>,
        script: impl Fn(&mut Probe) -> i32 + Send + Sync + 'static,
    ) -> Fake {
        Fake(Arc::new(Inner {
            script: Box::new(script),
            next_pid: AtomicU32::new(100),
            launches: Mutex::new(Vec::new()),
            live: AtomicUsize::new(0),
            max_live: AtomicUsize::new(0),
            kills: Mutex::new(Vec::new()),
            alive: Mutex::new(HashSet::new()),
            tree,
        }))
    }

    fn scheduler(&self, limits: Limits) -> Scheduler<'_> {
        Scheduler::new(self, self).with_limits(limits)
    }

    fn launches(&self) -> Vec<Launch> {
        self.0.launches.lock().unwrap().clone()
    }

    fn kills(&self) -> Vec<Kill> {
        self.0.kills.lock().unwrap().clone()
    }

    fn max_live(&self) -> usize {
        self.0.max_live.load(Ordering::SeqCst)
    }
}

struct State {
    exit: Mutex<Option<Exit>>,
    killed: AtomicBool,
}

fn finish(inner: &Inner, pid: u32, state: &State, exit: Exit) {
    let mut current = state.exit.lock().unwrap();
    if current.is_none() {
        *current = Some(exit);
        inner.alive.lock().unwrap().remove(&pid);
        inner.live.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Launcher for Fake {
    fn launch(&self, launch: &Launch) -> io::Result<Box<dyn Launched>> {
        let inner = Arc::clone(&self.0);
        let pid = inner.next_pid.fetch_add(1, Ordering::SeqCst);
        let index = {
            let mut launches = inner.launches.lock().unwrap();
            launches.push(launch.clone());
            launches.len() - 1
        };
        inner.alive.lock().unwrap().insert(pid);
        let live = inner.live.fetch_add(1, Ordering::SeqCst) + 1;
        inner.max_live.fetch_max(live, Ordering::SeqCst);
        let state = Arc::new(State {
            exit: Mutex::new(None),
            killed: AtomicBool::new(false),
        });
        let socket = PathBuf::from(launch.var(SOCKET_VAR).expect("the socket variable"));
        let (thread_inner, thread_state) = (Arc::clone(&inner), Arc::clone(&state));
        thread::spawn(move || {
            let mut probe = Probe {
                pid,
                index,
                socket,
                state: Arc::clone(&thread_state),
                conn: None,
            };
            let code = (thread_inner.script)(&mut probe);
            finish(&thread_inner, pid, &thread_state, Exit { code: Some(code) });
        });
        Ok(Box::new(FakeChild { pid, inner, state }))
    }
}

impl ProcessInfo for Fake {
    fn start_time(&self, _: u32) -> io::Result<SystemTime> {
        Ok(start())
    }

    fn phys_footprint(&self, pid: u32) -> io::Result<u64> {
        Ok(u64::from(pid) * 1_000)
    }

    fn children(&self, pid: u32) -> io::Result<Vec<u32>> {
        if let Some(children) = self.0.tree.get(&pid) {
            return Ok(children.clone());
        }
        if self.0.alive.lock().unwrap().contains(&pid) {
            return Ok(Vec::new());
        }
        Err(io::Error::from(ErrorKind::NotFound))
    }
}

struct FakeChild {
    pid: u32,
    inner: Arc<Inner>,
    state: Arc<State>,
}

impl Launched for FakeChild {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn try_wait(&mut self) -> io::Result<Option<Exit>> {
        Ok(*self.state.exit.lock().unwrap())
    }

    fn kill_group(&mut self) {
        self.inner.kills.lock().unwrap().push(Kill::Group(self.pid));
        self.state.killed.store(true, Ordering::SeqCst);
        finish(&self.inner, self.pid, &self.state, Exit { code: None });
    }

    fn kill(&mut self, pid: u32) {
        self.inner.kills.lock().unwrap().push(Kill::One(pid));
    }
}

/// What a script sees: the socket of its launch.
struct Probe {
    pid: u32,
    /// The launch's index, from 0.
    index: usize,
    socket: PathBuf,
    state: Arc<State>,
    conn: Option<(BufReader<UnixStream>, UnixStream)>,
}

impl Probe {
    fn connect(&mut self) {
        let stream = UnixStream::connect(&self.socket).expect("connect");
        let reader = BufReader::new(stream.try_clone().expect("clone"));
        self.conn = Some((reader, stream));
    }

    fn send(&mut self, report: &Report) {
        if let Some((_, writer)) = &mut self.conn {
            let _ = writer.write_all(format!("{}\n", report.encode()).as_bytes());
        }
    }

    /// Connects and sends `FirstFrame`.
    fn first_frame(&mut self, presented: SystemTime) {
        self.connect();
        let pid = self.pid;
        self.send(&Report::FirstFrame { pid, presented });
    }

    /// The next request; None once the runner closed the connection.
    fn request(&mut self) -> Option<Request> {
        let (reader, _) = self.conn.as_mut()?;
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(Request::decode(line.trim_end_matches('\n')).expect("a valid request")),
        }
    }

    /// Answers like the probe of a program at rest: `Epoch` with `epoch`,
    /// `Plugins` with nothing, `ActivateAll` with `Ready`; exits 0 on
    /// `Quit` or when the runner goes away.
    fn obey(&mut self, epoch: Option<u64>) -> i32 {
        while let Some(request) = self.request() {
            match request {
                Request::Epoch => self.send(&Report::Epoch(epoch)),
                Request::Plugins => self.send(&Report::PluginsEnd),
                Request::ActivateAll => self.send(&Report::Ready),
                Request::Open(_) => {}
                Request::Quit => return 0,
            }
        }
        0
    }

    /// Reads requests and answers none, until the runner goes away.
    fn ignore(&mut self) -> i32 {
        while self.request().is_some() {}
        0
    }

    /// Runs until killed (or 10 s), without connecting.
    fn hang(&self) -> i32 {
        let end = Instant::now() + Duration::from_secs(10);
        while !self.state.killed.load(Ordering::SeqCst) && Instant::now() < end {
            thread::sleep(ms(5));
        }
        0
    }
}

fn program() -> PathBuf {
    PathBuf::from("/fake/empty_window")
}

fn message<T>(result: Result<T, BenchError>) -> String {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error.message().to_owned(),
    }
}

// --- launch and cold start -------------------------------------------------

#[test]
fn process_launches_with_a_fresh_socket_and_home() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.obey(None)
    });
    let scheduler = fake.scheduler(quick());
    scheduler.session(&program(), None).unwrap().quit().unwrap();
    scheduler
        .session(&program(), Some("closure:git"))
        .unwrap()
        .quit()
        .unwrap();
    let launches = fake.launches();
    assert_eq!(launches.len(), 2);
    let (a, b) = (&launches[0], &launches[1]);
    assert_eq!(a.program, program());
    let (socket, home) = (a.var(SOCKET_VAR).unwrap(), a.var("HOME").unwrap());
    assert!(Path::new(socket).ends_with("s"), "{socket:?}");
    assert_eq!(Path::new(home).parent(), Path::new(socket).parent());
    assert_ne!(b.var(SOCKET_VAR), a.var(SOCKET_VAR));
    assert_ne!(b.var("HOME"), a.var("HOME"));
    // Removed once the session ended.
    assert!(!Path::new(home).exists());
    assert_eq!(a.var(PLUGINS_VAR), None);
    assert!(a.env.contains(&(PLUGINS_VAR.into(), None)));
    assert_eq!(b.var(PLUGINS_VAR), Some(OsStr::new("closure:git")));
    for name in ["LECTERN_LOG", "CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
        assert!(a.env.contains(&(name.into(), None)), "{name}");
    }
}

#[test]
fn process_cold_start_skips_two_warmups_and_takes_twenty() {
    // Launch n shows its first frame 10 + n ms after the process started.
    let fake = Fake::new(|p| {
        p.first_frame(start() + ms(10 + p.index as u64));
        p.obey(None)
    });
    let samples = fake
        .scheduler(quick())
        .cold_start(&program(), 2, 20)
        .unwrap();
    let expected: Vec<Duration> = (12..32).map(ms).collect();
    assert_eq!(samples.0, expected);
    assert_eq!(fake.launches().len(), 22);
    // Timing launches run one at a time; every one exited on `Quit`.
    assert_eq!(fake.max_live(), 1);
    assert_eq!(fake.kills(), []);
}

#[test]
fn process_cold_start_stops_at_a_hook_error() {
    // The probe's registration failed: one `Error`, then exit code 3.
    let fake = Fake::new(|p| {
        p.connect();
        p.send(&Report::Error {
            message: "hook missing_method".into(),
        });
        3
    });
    let error = message(fake.scheduler(quick()).cold_start(&program(), 2, 20));
    assert!(error.contains("hook missing_method"), "{error}");
    assert_eq!(fake.launches().len(), 1);
}

#[test]
fn process_warmup_that_exits_with_3_is_an_error() {
    let fake = Fake::new(|_| 3);
    let error = message(fake.scheduler(quick()).cold_start(&program(), 2, 20));
    assert!(error.contains("退出码 3"), "{error}");
    assert_eq!(fake.launches().len(), 1);
}

#[test]
fn process_no_connection_is_an_error() {
    let fake = Fake::new(|p| p.hang());
    let limits = Limits {
        first_frame: ms(150),
        ..quick()
    };
    let error = message(fake.scheduler(limits).session(&program(), None));
    assert!(error.contains("FirstFrame"), "{error}");
    assert_eq!(fake.kills(), [Kill::Group(100)]);
}

#[test]
fn process_exit_while_waiting_is_an_error() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        let _ = p.request();
        1
    });
    let scheduler = fake.scheduler(quick());
    let mut session = scheduler.session(&program(), None).unwrap();
    let error = message(session.epoch());
    assert!(
        error.contains("Epoch") && error.contains("退出码 1"),
        "{error}"
    );
}

#[test]
fn process_no_exit_after_quit_is_an_error() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.ignore()
    });
    let limits = Limits {
        quit: ms(150),
        ..quick()
    };
    let scheduler = fake.scheduler(limits);
    let error = message(scheduler.session(&program(), None).unwrap().quit());
    assert!(error.contains("Quit"), "{error}");
    assert_eq!(fake.kills(), [Kill::Group(100)]);
}

#[test]
fn process_nonzero_exit_after_quit_is_an_error() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.obey(None);
        2
    });
    let scheduler = fake.scheduler(quick());
    let error = message(scheduler.session(&program(), None).unwrap().quit());
    assert!(error.contains("退出码 2"), "{error}");
}

#[test]
fn process_first_frame_from_another_pid_is_an_error() {
    let fake = Fake::new(|p| {
        p.connect();
        p.send(&Report::FirstFrame {
            pid: 7,
            presented: start(),
        });
        p.ignore()
    });
    let error = message(fake.scheduler(quick()).session(&program(), None));
    assert!(error.contains("7"), "{error}");
    assert_eq!(fake.kills(), [Kill::Group(100)]);
}

// --- idle samples ----------------------------------------------------------

#[test]
fn process_baseline_waits_after_the_first_frame() {
    let fake = Fake::new(|p| {
        p.first_frame(SystemTime::now());
        p.obey(None)
    });
    let limits = Limits {
        settle: ms(150),
        ..quick()
    };
    let scheduler = fake.scheduler(limits);
    let mut session = scheduler.session(&program(), None).unwrap();
    session.settle_after_first_frame().unwrap();
    let waited = SystemTime::now()
        .duration_since(session.presented())
        .unwrap();
    assert!(waited >= ms(150), "{waited:?}");
    assert_eq!(session.footprint().unwrap(), 100_000);
    session.quit().unwrap();
}

#[test]
fn process_busy_restarts_the_settle() {
    // Quiescent, then Busy 30 ms later, then Quiescent again: the sample
    // comes a full settle time after the second Quiescent.
    let second: Arc<Mutex<Option<Instant>>> = Arc::default();
    let seen = Arc::clone(&second);
    let fake = Fake::new(move |p| {
        p.first_frame(start());
        p.send(&Report::Busy { epoch: 1 });
        p.send(&Report::Quiescent { epoch: 2 });
        thread::sleep(ms(30));
        p.send(&Report::Busy { epoch: 3 });
        thread::sleep(ms(30));
        *seen.lock().unwrap() = Some(Instant::now());
        p.send(&Report::Quiescent { epoch: 4 });
        p.obey(Some(4))
    });
    let limits = Limits {
        settle: ms(100),
        ..quick()
    };
    let scheduler = fake.scheduler(limits);
    let mut session = scheduler.session(&program(), None).unwrap();
    let idle = session.idle(|_| Ok(Instant::now())).unwrap();
    let Idle::Sample(sampled) = idle else {
        panic!("not quiescent");
    };
    let second = second.lock().unwrap().unwrap();
    assert!(sampled.duration_since(second) >= ms(100));
    session.quit().unwrap();
}

#[test]
fn process_busy_before_the_first_epoch_answer_restarts_the_settle() {
    // The kernel is busy once more after the settle time, and the probe
    // reports it just before its first `Epoch` answer: the sample still
    // comes a full settle time after the last `Quiescent`.
    let last: Arc<Mutex<Option<Instant>>> = Arc::default();
    let seen = Arc::clone(&last);
    let fake = Fake::new(move |p| {
        p.first_frame(start());
        p.send(&Report::Quiescent { epoch: 2 });
        let mut epochs = 0;
        while let Some(request) = p.request() {
            match request {
                Request::Epoch => {
                    epochs += 1;
                    if epochs == 1 {
                        p.send(&Report::Busy { epoch: 3 });
                        *seen.lock().unwrap() = Some(Instant::now());
                        p.send(&Report::Quiescent { epoch: 4 });
                    }
                    p.send(&Report::Epoch(Some(4)));
                }
                Request::Quit => return 0,
                _ => {}
            }
        }
        0
    });
    let limits = Limits {
        settle: ms(100),
        ..quick()
    };
    let scheduler = fake.scheduler(limits);
    let mut session = scheduler.session(&program(), None).unwrap();
    let mut samples = 0;
    let idle = session
        .idle(|_| {
            samples += 1;
            Ok(Instant::now())
        })
        .unwrap();
    let Idle::Sample(sampled) = idle else {
        panic!("not quiescent");
    };
    let last = last.lock().unwrap().unwrap();
    assert!(
        sampled.duration_since(last) >= ms(100),
        "{:?}",
        sampled.duration_since(last)
    );
    assert_eq!(samples, 1);
    session.quit().unwrap();
}

#[test]
fn process_changed_epoch_takes_the_sample_again() {
    // The second `Epoch` comes after the kernel was busy once more.
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.send(&Report::Quiescent { epoch: 2 });
        let mut epochs = 0;
        while let Some(request) = p.request() {
            match request {
                Request::Epoch => {
                    epochs += 1;
                    if epochs == 2 {
                        p.send(&Report::Busy { epoch: 3 });
                        p.send(&Report::Quiescent { epoch: 4 });
                    }
                    let epoch = if epochs == 1 { 2 } else { 4 };
                    p.send(&Report::Epoch(Some(epoch)));
                }
                Request::Quit => return 0,
                _ => {}
            }
        }
        0
    });
    let scheduler = fake.scheduler(quick());
    let mut session = scheduler.session(&program(), None).unwrap();
    let mut samples = 0;
    let idle = session
        .idle(|s| {
            samples += 1;
            s.footprint()
        })
        .unwrap();
    assert_eq!(idle, Idle::Sample(100_000));
    assert_eq!(samples, 2);
    session.quit().unwrap();
}

#[test]
fn process_three_invalid_samples_are_an_error() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.send(&Report::Quiescent { epoch: 2 });
        // Busy in every answer.
        p.obey(None)
    });
    let scheduler = fake.scheduler(quick());
    let mut session = scheduler.session(&program(), None).unwrap();
    let mut samples = 0;
    let error = message(session.idle(|_| {
        samples += 1;
        Ok(())
    }));
    assert!(error.contains("3"), "{error}");
    assert_eq!(samples, 3);
}

#[test]
fn process_not_quiescent_after_the_first_frame() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.send(&Report::Busy { epoch: 1 });
        p.obey(None)
    });
    let limits = Limits {
        first_quiescent: ms(150),
        ..quick()
    };
    let scheduler = fake.scheduler(limits);
    let mut session = scheduler.session(&program(), None).unwrap();
    let begin = Instant::now();
    assert_eq!(session.idle(|_| Ok(())).unwrap(), Idle::NotQuiescent);
    assert!(begin.elapsed() < Duration::from_secs(1));
}

// --- deadlines and failures ------------------------------------------------

fn short() -> Limits {
    Limits {
        again: ms(150),
        reply: ms(150),
        ..quick()
    }
}

/// A session whose probe answers nothing; `wait` must fail naming `what`
/// and end the process group.
fn deadline(what: &str, wait: impl FnOnce(&mut Session<'_>) -> Result<(), BenchError>) {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.ignore()
    });
    {
        let scheduler = fake.scheduler(short());
        let mut session = scheduler.session(&program(), None).unwrap();
        let begin = Instant::now();
        let error = message(wait(&mut session));
        assert!(error.contains(what), "{error}");
        assert!(begin.elapsed() >= ms(150), "{:?}", begin.elapsed());
    }
    assert_eq!(fake.kills(), [Kill::Group(100)]);
}

#[test]
fn process_deadline_epoch() {
    deadline("Epoch", |s| s.epoch().map(drop));
}

#[test]
fn process_deadline_plugins() {
    deadline("PluginsEnd", |s| s.plugins().map(drop));
}

#[test]
fn process_deadline_ready() {
    deadline("Ready", |s| s.activate_all());
}

#[test]
fn process_deadline_milestone() {
    deadline("Milestone", |s| {
        s.open(
            OpenTarget::Project("/tmp/p".into()),
            Milestone::ProjectHome,
            ms(150),
        )
        .map(drop)
    });
}

#[test]
fn process_deadline_frames() {
    deadline("Frames", |s| {
        s.frames(OpenTarget::Scroll, ms(150)).map(drop)
    });
}

#[test]
fn process_plugin_failed_is_an_error_at_once() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.send(&Report::PluginFailed {
            id: "git".into(),
            message: "panicked".into(),
        });
        p.ignore()
    });
    let scheduler = fake.scheduler(quick());
    let mut session = scheduler.session(&program(), None).unwrap();
    let begin = Instant::now();
    let error = message(session.ready());
    assert!(
        error.contains("git") && error.contains("panicked"),
        "{error}"
    );
    assert!(begin.elapsed() < Duration::from_secs(1));
    drop(session);
    assert_eq!(fake.kills(), [Kill::Group(100)]);
}

#[test]
fn process_frames_error_from_the_probe_is_an_error() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        while let Some(request) = p.request() {
            if request == Request::Open(OpenTarget::Scroll) {
                p.send(&Report::Error {
                    message: "frames unmatched=1 incomplete=0 overflowed=0".into(),
                });
            }
        }
        0
    });
    let scheduler = fake.scheduler(quick());
    let mut session = scheduler.session(&program(), None).unwrap();
    let error = message(session.frames(OpenTarget::Scroll, Duration::from_secs(2)));
    assert!(error.contains("frames unmatched=1"), "{error}");
}

#[test]
fn process_answers_reach_the_runner() {
    let plugin = PluginReport {
        id: "git".into(),
        state: "Active".into(),
        activation: vec![crate::wire::ActivationKind::AfterFirstFrame],
        manifest: Some(ms(1)),
        activate: None,
        commit: None,
        started: None,
    };
    let answer = plugin.clone();
    let stats = FrameStats {
        scenario: "scroll".into(),
        refresh: Duration::from_nanos(16_666_667),
        frames: Vec::new(),
        without_tick: 0,
    };
    let frames = stats.clone();
    let fake = Fake::new(move |p| {
        p.first_frame(start());
        while let Some(request) = p.request() {
            match request {
                Request::Plugins => {
                    p.send(&Report::Plugin(answer.clone()));
                    p.send(&Report::PluginsEnd);
                }
                Request::Open(OpenTarget::Scroll) => p.send(&Report::Frames(frames.clone())),
                Request::Open(_) => p.send(&Report::Milestone {
                    milestone: Milestone::ProjectHome,
                    presented: SystemTime::now(),
                }),
                Request::Quit => return 0,
                _ => {}
            }
        }
        0
    });
    let scheduler = fake.scheduler(quick());
    let mut session = scheduler.session(&program(), None).unwrap();
    assert_eq!(session.plugins().unwrap(), [plugin]);
    assert_eq!(
        session
            .frames(OpenTarget::Scroll, Duration::from_secs(2))
            .unwrap(),
        stats
    );
    let time = session
        .open(
            OpenTarget::Project("/tmp/p".into()),
            Milestone::ProjectHome,
            Duration::from_secs(2),
        )
        .unwrap();
    assert!(time < Duration::from_secs(2));
    session.quit().unwrap();
}

// --- process groups and concurrency ---------------------------------------

#[test]
fn process_groups_expand_to_grandchildren() {
    // 14 exited during the walk; 13 lists 11 again.
    let tree = HashMap::from([
        (10, vec![11, 12, 14]),
        (11, vec![13]),
        (12, vec![]),
        (13, vec![11]),
        (20, vec![12]),
    ]);
    let fake = Fake::with_tree(tree, |_| 0);
    assert_eq!(expand(&fake, &[10]), [10, 11, 12, 13]);
    assert_eq!(expand(&fake, &[10, 20]), [10, 20, 11, 12, 13]);
    assert_eq!(expand(&fake, &[14]), Vec::<u32>::new());
}

#[test]
fn process_ending_a_session_kills_the_reported_groups() {
    let tree = HashMap::from([(50, vec![51]), (51, vec![])]);
    let fake = Fake::with_tree(tree, |p| {
        p.first_frame(start());
        p.send(&Report::Child {
            pid: 50,
            kind: ChildKind::ProcessPlugin,
            id: "jdtls host".into(),
        });
        p.ignore()
    });
    {
        let scheduler = fake.scheduler(short());
        let mut session = scheduler.session(&program(), None).unwrap();
        let _ = session.epoch();
        assert_eq!(
            session.children(),
            [Spawned {
                pid: 50,
                kind: ChildKind::ProcessPlugin,
                id: "jdtls host".into(),
            }]
        );
    }
    assert_eq!(
        fake.kills(),
        [Kill::Group(100), Kill::One(50), Kill::One(51)]
    );
}

#[test]
fn process_memory_runs_at_most_three_at_once() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.obey(None)
    });
    let scheduler = fake.scheduler(quick());
    let bytes = scheduler
        .memory(7, |s| {
            let session = s.session(&program(), None)?;
            thread::sleep(ms(50));
            let bytes = session.footprint()?;
            session.quit()?;
            Ok(bytes)
        })
        .unwrap();
    assert_eq!(bytes.len(), 7);
    assert_eq!(fake.max_live(), MEMORY_PARALLEL);
}

#[test]
fn process_memory_stops_at_the_first_error() {
    let fake = Fake::new(|_| 1);
    let scheduler = fake.scheduler(quick());
    let error = message(scheduler.memory(9, |s| s.session(&program(), None).map(drop)));
    assert!(error.contains("退出码 1"), "{error}");
    // The runs in flight may start one more each; not all nine.
    assert!(fake.launches().len() < 9, "{}", fake.launches().len());
}
