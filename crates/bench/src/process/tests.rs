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
use crate::wire::{ActivationKind, ChildKind, FrameStats, Milestone, OpenTarget, Report, Request};

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
        let selection = launch
            .var(PLUGINS_VAR)
            .map(|value| value.to_string_lossy().into_owned());
        let (thread_inner, thread_state) = (Arc::clone(&inner), Arc::clone(&state));
        thread::spawn(move || {
            let mut probe = Probe {
                pid,
                index,
                selection,
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
    /// `LECTERN_BENCH_PLUGINS` of the launch.
    selection: Option<String>,
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

    /// Connects and sends `FirstFrame` with a `presentedTime`.
    fn first_frame(&mut self, presented: SystemTime) {
        self.first_frame_from(presented, TimeSource::Presented);
    }

    /// Connects and sends `FirstFrame` with a time from `source`.
    fn first_frame_from(&mut self, presented: SystemTime, source: TimeSource) {
        self.connect();
        let pid = self.pid;
        self.send(&Report::FirstFrame {
            pid,
            presented,
            source,
        });
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
fn process_home_is_prepared_before_the_launch() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.obey(None)
    });
    let scheduler = fake.scheduler(quick());
    let session = scheduler
        .session_in_home(&program(), &|home| {
            std::fs::create_dir_all(home.join(".claude/projects"))?;
            std::fs::write(home.join(".claude/projects/marker"), b"x")
        })
        .unwrap();
    let launch = &fake.launches()[0];
    let home = PathBuf::from(launch.var("HOME").unwrap());
    assert!(home.join(".claude/projects/marker").is_file());
    assert_eq!(launch.var(PLUGINS_VAR), None, "the whole profile");
    // The `HOME` is still there after the exit, and removed after the read.
    let read = session
        .quit_reading_home(|home| std::fs::read(home.join(".claude/projects/marker")).unwrap())
        .unwrap();
    assert_eq!(read, b"x");
    assert!(!home.exists());
    // A failing `prepare` launches nothing.
    let error =
        message(scheduler.session_in_home(&program(), &|_| Err(io::Error::other("full disk"))));
    assert!(error.contains("full disk"), "{error}");
    assert_eq!(fake.launches().len(), 1);
}

#[test]
fn process_idle_after_an_open_waits_from_the_milestone() {
    // The open takes longer than `first_quiescent` after the first frame;
    // the idle sample after it is still taken.
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.send(&Report::Quiescent { epoch: 1 });
        while let Some(request) = p.request() {
            match request {
                Request::Open(_) => {
                    thread::sleep(ms(400));
                    p.send(&Report::Milestone {
                        milestone: Milestone::ProjectHome,
                        presented: SystemTime::now(),
                    });
                    p.send(&Report::Busy { epoch: 1 });
                    thread::sleep(ms(100));
                    p.send(&Report::Quiescent { epoch: 2 });
                }
                Request::Epoch => p.send(&Report::Epoch(Some(2))),
                Request::Quit => return 0,
                _ => {}
            }
        }
        0
    });
    let limits = Limits {
        first_quiescent: ms(300),
        ..quick()
    };
    let scheduler = fake.scheduler(limits);
    let mut session = scheduler.session(&program(), None).unwrap();
    session
        .open(
            OpenTarget::Project("/tmp/p".into()),
            Milestone::ProjectHome,
            Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(session.idle(|_| Ok(7)).unwrap(), Idle::Sample(7));
    session.quit().unwrap();
}

#[test]
fn process_cold_start_skips_two_warmups_and_takes_twenty() {
    // Launch n shows its first frame 10 + n ms after the process started;
    // every third time comes from the presented handler.
    let source = |index: usize| {
        if index.is_multiple_of(3) {
            TimeSource::Handler
        } else {
            TimeSource::Presented
        }
    };
    let fake = Fake::new(move |p| {
        p.first_frame_from(start() + ms(10 + p.index as u64), source(p.index));
        p.obey(None)
    });
    let cold = fake
        .scheduler(quick())
        .cold_start(&program(), 2, 20)
        .unwrap();
    let expected: Vec<Duration> = (12..32).map(ms).collect();
    assert_eq!(cold.samples.0, expected);
    // Each sample keeps its own launch's source.
    let sources: Vec<TimeSource> = (2..22).map(source).collect();
    assert_eq!(cold.sources, sources);
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

/// A launcher whose children answer `try_wait` only once they have exited:
/// the runner's first look at the process comes after the whole life of a
/// probe that connects late, writes and exits.
struct Settled<'a>(&'a Fake);

struct SettledChild(Box<dyn Launched>);

impl Launcher for Settled<'_> {
    fn launch(&self, launch: &Launch) -> io::Result<Box<dyn Launched>> {
        Ok(Box::new(SettledChild(self.0.launch(launch)?)))
    }
}

impl Launched for SettledChild {
    fn pid(&self) -> u32 {
        self.0.pid()
    }

    fn try_wait(&mut self) -> io::Result<Option<Exit>> {
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            let exit = self.0.try_wait()?;
            if exit.is_some() || Instant::now() >= end {
                return Ok(exit);
            }
            thread::sleep(ms(1));
        }
    }

    fn kill_group(&mut self) {
        self.0.kill_group();
    }

    fn kill(&mut self, pid: u32) {
        self.0.kill(pid);
    }
}

/// The runner looks at the socket (nothing there yet), then at the process,
/// which by then has connected, written its `Error` and exited: the
/// connection is still there to be taken, and what it wrote is the reason.
#[test]
fn process_a_probe_that_exits_at_once_is_still_heard() {
    let fake = Fake::new(|p| {
        thread::sleep(ms(50));
        p.connect();
        p.send(&Report::Error {
            message: "hook missing_method".into(),
        });
        3
    });
    let launcher = Settled(&fake);
    let scheduler = Scheduler::new(&launcher, &fake).with_limits(quick());
    let error = message(scheduler.session(&program(), None));
    assert!(error.contains("hook missing_method"), "{error}");
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
            source: TimeSource::Presented,
        });
        p.ignore()
    });
    let error = message(fake.scheduler(quick()).session(&program(), None));
    assert!(error.contains("7"), "{error}");
    assert_eq!(fake.kills(), [Kill::Group(100)]);
}

/// On the judging machine a first frame without a `presentedTime` is an
/// error that ends the process group, for a session and for a cold start
/// (ADR 0052).
#[test]
fn process_judging_a_handler_first_frame_is_an_error() {
    let fake = Fake::new(|p| {
        p.first_frame_from(start(), TimeSource::Handler);
        p.obey(None)
    });
    let error = message(
        fake.scheduler(quick())
            .judging(true)
            .session(&program(), None),
    );
    assert_eq!(error, HANDLER_ON_THE_JUDGE);
    assert!(error.contains("handler"), "{error}");
    assert_eq!(fake.kills(), [Kill::Group(100)]);

    let error = message(
        fake.scheduler(quick())
            .judging(true)
            .cold_start(&program(), 2, 20),
    );
    assert_eq!(error, HANDLER_ON_THE_JUDGE);
    assert_eq!(fake.launches().len(), 2, "stops at the first launch");
}

/// Judging takes a presented first frame; not judging takes a handler one.
#[test]
fn process_judging_takes_a_presented_first_frame() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.obey(None)
    });
    let session = fake
        .scheduler(quick())
        .judging(true)
        .session(&program(), None)
        .unwrap();
    assert_eq!(session.source(), TimeSource::Presented);
    session.quit().unwrap();

    let fake = Fake::new(|p| {
        p.first_frame_from(start(), TimeSource::Handler);
        p.obey(None)
    });
    let session = fake
        .scheduler(quick())
        .judging(false)
        .session(&program(), None)
        .unwrap();
    assert_eq!(session.source(), TimeSource::Handler);
    session.quit().unwrap();
}

// --- idle samples ----------------------------------------------------------

#[test]
fn process_baseline_waits_after_the_first_frame() {
    let fake = Fake::new(|p| {
        p.first_frame_from(SystemTime::now(), TimeSource::Handler);
        p.obey(None)
    });
    let limits = Limits {
        settle: ms(150),
        ..quick()
    };
    let scheduler = fake.scheduler(limits);
    let mut session = scheduler.session(&program(), None).unwrap();
    assert_eq!(session.source(), TimeSource::Handler);
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
fn process_memory_runs_one_at_a_time_on_the_judging_machine() {
    // Windows opened together cover each other, and a covered window shows
    // no drawable (ADR 0052).
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.obey(None)
    });
    let scheduler = fake.scheduler(quick()).judging(true);
    let bytes = scheduler
        .memory(4, |s| {
            let session = s.session(&program(), None)?;
            thread::sleep(ms(20));
            let bytes = session.footprint()?;
            session.quit()?;
            Ok(bytes)
        })
        .unwrap();
    assert_eq!(bytes.len(), 4);
    assert_eq!(fake.max_live(), 1);
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

// --- app/idle_empty, app/unclassified_children, plugin/* -------------------

fn plugin_report(id: &str, state: &str, kinds: &[ActivationKind], us: [u64; 4]) -> PluginReport {
    let micros = |n: u64| Some(Duration::from_micros(n));
    PluginReport {
        id: id.to_owned(),
        state: state.to_owned(),
        activation: kinds.to_vec(),
        manifest: micros(us[0]),
        activate: micros(us[1]),
        commit: micros(us[2]),
        started: micros(us[3]),
    }
}

/// The probe of a program at rest: a quiescent kernel, `Ready` when the
/// plugin set is trimmed, and the same `plugins` as the answer to every
/// `Plugins`.
fn serve(p: &mut Probe, plugins: &[PluginReport]) -> i32 {
    p.first_frame(start());
    p.send(&Report::Quiescent { epoch: 2 });
    if p.selection.is_some() {
        p.send(&Report::Ready);
    }
    while let Some(request) = p.request() {
        match request {
            Request::Epoch => p.send(&Report::Epoch(Some(2))),
            Request::Plugins => {
                for plugin in plugins {
                    p.send(&Report::Plugin(plugin.clone()));
                }
                p.send(&Report::PluginsEnd);
            }
            Request::Quit => return 0,
            _ => {}
        }
    }
    0
}

fn git_plugin() -> PluginReport {
    plugin_report(
        "git",
        "Active",
        &[ActivationKind::AfterFirstFrame],
        [1, 20, 3, 40],
    )
}

fn selections(fake: &Fake) -> Vec<String> {
    fake.launches()
        .iter()
        .map(|launch| {
            launch
                .var(PLUGINS_VAR)
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn process_idle_footprints_take_each_launch_at_its_idle_sample() {
    let fake = Fake::new(|p| serve(p, &[]));
    let scheduler = fake.scheduler(quick());
    let runs = scheduler.idle_footprints(&program(), 3).unwrap();
    let mut bytes: Vec<u64> = runs
        .into_iter()
        .map(|run| match run {
            Idle::Sample(bytes) => bytes,
            Idle::NotQuiescent => panic!("not quiescent"),
        })
        .collect();
    // The fake's footprint is 1000 bytes per pid; launches are 100, 101, 102.
    bytes.sort_unstable();
    assert_eq!(bytes, [100_000, 101_000, 102_000]);
    // One at a time (the windows of launches at once cover each other).
    assert_eq!(fake.max_live(), 1);
}

#[test]
fn process_idle_footprints_report_a_launch_that_never_quiesces() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.obey(None)
    });
    let limits = Limits {
        first_quiescent: ms(100),
        ..quick()
    };
    let runs = fake
        .scheduler(limits)
        .idle_footprints(&program(), 2)
        .unwrap();
    assert_eq!(runs, [Idle::NotQuiescent, Idle::NotQuiescent]);
}

#[test]
fn process_plugin_pair_is_the_difference_of_two_launches() {
    let fake = Fake::new(|p| serve(p, &[git_plugin()]));
    let scheduler = fake.scheduler(quick());
    let pair = scheduler.plugin_pair(&program(), "git").unwrap();
    assert_eq!(selections(&fake), ["closure:git", "closure+self:git"]);
    // Launches 100 (without) and 101 (with), 1000 bytes per pid.
    assert_eq!((pair.without, pair.with), (100_000, 101_000));
    assert_eq!(pair.resident(), 1_000);
    assert_eq!(pair.difference(), 1_000);
    // The four durations of the plugin in the launch with it.
    assert_eq!(pair.activate, Duration::from_micros(1 + 20 + 3 + 40));
}

#[test]
fn process_plugin_resident_is_zero_when_the_launch_with_it_is_smaller() {
    let pair = PluginPair {
        without: 5_000_000,
        with: 4_900_000,
        activate: ms(0),
    };
    assert_eq!(pair.resident(), 0);
    assert_eq!(pair.difference(), -100_000);
}

#[test]
fn process_plugin_counts_the_process_plugin_groups() {
    // The launch with the plugin starts a process plugin (500, with a
    // child 501), a language server (600) and an unnamed kind (700); only
    // the process-plugin group is counted in the footprint.
    let tree = HashMap::from([
        (101, vec![]),
        (500, vec![501]),
        (501, vec![]),
        (600, vec![]),
        (700, vec![]),
    ]);
    let fake = Fake::with_tree(tree, |p| {
        p.first_frame(start());
        if p.selection.as_deref() == Some("closure+self:git") {
            for (pid, kind) in [
                (500, ChildKind::ProcessPlugin),
                (600, ChildKind::LanguageServer),
                (700, ChildKind::Other),
            ] {
                p.send(&Report::Child {
                    pid,
                    kind,
                    id: format!("child {pid}"),
                });
            }
        }
        p.send(&Report::Quiescent { epoch: 2 });
        p.send(&Report::Ready);
        while let Some(request) = p.request() {
            match request {
                Request::Epoch => p.send(&Report::Epoch(Some(2))),
                Request::Plugins => {
                    p.send(&Report::Plugin(git_plugin()));
                    p.send(&Report::PluginsEnd);
                }
                Request::Quit => return 0,
                _ => {}
            }
        }
        0
    });
    let scheduler = fake.scheduler(quick());
    let pair = scheduler.plugin_pair(&program(), "git").unwrap();
    assert_eq!(pair.without, 100_000);
    assert_eq!(pair.with, (101 + 500 + 501) * 1_000);
    assert_eq!(pair.resident(), (1 + 500 + 501) * 1_000);
}

#[test]
fn process_plugin_measures_two_more_pairs_when_the_first_is_over() {
    let fake = Fake::new(|p| serve(p, &[git_plugin()]));
    let scheduler = fake.scheduler(quick());
    let pairs = scheduler.plugin(&program(), "git", |_| false).unwrap();
    assert_eq!(pairs.len(), 1);
    assert_eq!(fake.launches().len(), 2);
    let pairs = scheduler.plugin(&program(), "git", |_| true).unwrap();
    assert_eq!(pairs.len(), 3);
    assert_eq!(fake.launches().len(), 2 + 6);
    // One at a time: the activation times are timings.
    assert_eq!(fake.max_live(), 1);
}

#[test]
fn process_plugin_that_did_not_activate_has_no_activation_time() {
    let failed = plugin_report(
        "git",
        "Failed",
        &[ActivationKind::AfterFirstFrame],
        [1, 1, 1, 1],
    );
    let fake = Fake::new(move |p| serve(p, std::slice::from_ref(&failed)));
    let error = message(fake.scheduler(quick()).plugin_pair(&program(), "git"));
    assert!(error.contains("Failed"), "{error}");

    let mut partial = git_plugin();
    partial.started = None;
    let fake = Fake::new(move |p| serve(p, &[partial.clone()]));
    let error = message(fake.scheduler(quick()).plugin_pair(&program(), "git"));
    assert!(error.contains("四段耗时不全"), "{error}");

    let other = plugin_report("other", "Active", &[], [1, 1, 1, 1]);
    let fake = Fake::new(move |p| serve(p, std::slice::from_ref(&other)));
    let error = message(fake.scheduler(quick()).plugin_pair(&program(), "git"));
    assert!(error.contains("没有插件 git"), "{error}");
}

#[test]
fn process_plugin_that_is_not_quiescent_is_an_error() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.send(&Report::Ready);
        p.obey(None)
    });
    let limits = Limits {
        first_quiescent: ms(100),
        ..quick()
    };
    let error = message(fake.scheduler(limits).plugin_pair(&program(), "git"));
    assert!(error.contains("没有静止"), "{error}");
}

#[test]
fn process_plugin_that_fails_to_start_is_an_error_at_once() {
    let fake = Fake::new(|p| {
        p.first_frame(start());
        p.send(&Report::PluginFailed {
            id: "git".into(),
            message: "boom".into(),
        });
        p.ignore()
    });
    let started = Instant::now();
    let error = message(fake.scheduler(quick()).plugin_pair(&program(), "git"));
    assert!(error.contains("git") && error.contains("boom"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn process_after_first_frame_adds_up_the_plugins_that_activate_after_it() {
    let plugins = [
        plugin_report(
            "a",
            "Active",
            &[ActivationKind::AfterFirstFrame],
            [1, 2, 3, 4],
        ),
        // Another condition besides `AfterFirstFrame`, in any position.
        plugin_report(
            "b",
            "Active",
            &[ActivationKind::Workspace, ActivationKind::AfterFirstFrame],
            [10, 20, 30, 40],
        ),
        // Not activated after the first frame: not counted, not checked.
        plugin_report("c", "Inactive", &[ActivationKind::Command], [100, 0, 0, 0]),
        plugin_report("d", "Active", &[ActivationKind::Manual], [1_000, 0, 0, 0]),
    ];
    assert_eq!(
        after_first_frame_time(&plugins).unwrap(),
        Duration::from_micros(10 + 100)
    );
    let mut broken = plugins.to_vec();
    broken[0].state = "Failed".into();
    assert!(after_first_frame_time(&broken).is_err());
    assert_eq!(after_first_frame_time(&[]).unwrap(), Duration::ZERO);
}

#[test]
fn process_after_first_frame_launches_one_at_a_time() {
    let plugins = [
        plugin_report(
            "a",
            "Active",
            &[ActivationKind::AfterFirstFrame],
            [1, 2, 3, 4],
        ),
        plugin_report("c", "Inactive", &[ActivationKind::Command], [0, 0, 0, 0]),
    ];
    let fake = Fake::new(move |p| serve(p, &plugins));
    let sums = fake
        .scheduler(quick())
        .after_first_frame(&program(), 3)
        .unwrap();
    assert_eq!(sums, Samples(vec![Duration::from_micros(10); 3]));
    assert_eq!(fake.launches().len(), 3);
    assert_eq!(fake.max_live(), 1);
}

#[test]
fn process_plugin_list_comes_from_an_idle_launch() {
    let plugins = [
        git_plugin(),
        plugin_report("b", "Active", &[], [0, 0, 0, 0]),
    ];
    let answer = plugins.clone();
    let fake = Fake::new(move |p| serve(p, &answer));
    let list = fake.scheduler(quick()).plugin_list(&program()).unwrap();
    assert_eq!(list, plugins);
    assert_eq!(selections(&fake), [""], "the whole profile: no selection");
}

#[test]
fn process_unclassified_children_leave_out_the_reported_groups() {
    // The main process (100) has a process plugin (200, child 201), an
    // unnamed child (300) that a `Child` report calls `Other`, and one
    // nobody reported (400, child 401). A language server (500) is
    // reported and also counted nowhere; it is not unclassified.
    let tree = HashMap::from([
        (100, vec![200, 300, 400, 500]),
        (200, vec![201]),
        (201, vec![]),
        (300, vec![]),
        (400, vec![401]),
        (401, vec![]),
        (500, vec![]),
    ]);
    let fake = Fake::with_tree(tree, |p| {
        p.first_frame(start());
        for (pid, kind) in [
            (200, ChildKind::ProcessPlugin),
            (300, ChildKind::Other),
            (500, ChildKind::LanguageServer),
        ] {
            p.send(&Report::Child {
                pid,
                kind,
                id: format!("child {pid}"),
            });
        }
        p.send(&Report::Quiescent { epoch: 2 });
        p.obey(Some(2))
    });
    let found = fake
        .scheduler(quick())
        .unclassified_children(&program())
        .unwrap();
    assert_eq!(
        found,
        Unclassified {
            processes: 3,
            bytes: (300 + 400 + 401) * 1_000,
        }
    );
}

#[test]
fn process_unclassified_children_of_a_program_without_children_are_zero() {
    let fake = Fake::new(|p| serve(p, &[]));
    let found = fake
        .scheduler(quick())
        .unclassified_children(&program())
        .unwrap();
    assert_eq!(found, Unclassified::default());
}
