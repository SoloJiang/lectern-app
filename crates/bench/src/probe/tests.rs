//! `probe_*`: the probe with a real `Kernel`, test plugins and a runner side
//! on a Unix socket (docs/design/bench.md「测试」). The first-frame callback
//! itself only runs on macOS with a display; the tests call
//! `Probe::first_frame` in its place.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lectern_kernel::{Service, service};

use super::*;
use crate::wire::{ChildKind, Milestone};

const WAIT: Duration = Duration::from_secs(10);

// --- the runner side -------------------------------------------------------

struct Runner {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    listener: UnixListener,
}

impl Runner {
    fn new() -> Runner {
        let dir = tempfile::tempdir().expect("temporary directory");
        let socket = dir.path().join("s");
        let listener = UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        Runner {
            _dir: dir,
            socket,
            listener,
        }
    }

    fn accept(&self) -> Conn {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).expect("blocking");
                    stream.set_read_timeout(Some(WAIT)).expect("timeout");
                    let reader = BufReader::new(stream.try_clone().expect("clone"));
                    return Conn {
                        reader,
                        writer: stream,
                    };
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "the probe never connected");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept: {error}"),
            }
        }
    }
}

struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Conn {
    fn next(&mut self) -> Report {
        let mut line = String::new();
        let read = self.reader.read_line(&mut line).expect("a report in time");
        assert!(read > 0, "the probe closed the connection");
        Report::decode(line.trim_end_matches('\n')).expect("a valid report")
    }

    /// Reads until `wanted` matches; returns every report read, that one
    /// last.
    fn until(&mut self, wanted: impl Fn(&Report) -> bool) -> Vec<Report> {
        let mut seen = Vec::new();
        loop {
            let report = self.next();
            let done = wanted(&report);
            seen.push(report);
            if done {
                return seen;
            }
        }
    }

    /// Every report that comes within `period`.
    fn drain(&mut self, period: Duration) -> Vec<Report> {
        let end = Instant::now() + period;
        let mut seen = Vec::new();
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            self.reader
                .get_ref()
                .set_read_timeout(Some(left))
                .expect("timeout");
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => seen.push(Report::decode(line.trim_end_matches('\n')).expect("valid")),
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    break;
                }
                Err(error) => panic!("read: {error}"),
            }
        }
        self.reader
            .get_ref()
            .set_read_timeout(Some(WAIT))
            .expect("timeout");
        seen
    }

    fn send(&mut self, request: &Request) {
        self.raw(&request.encode());
    }

    fn raw(&mut self, line: &str) {
        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .expect("write");
    }

    /// `Plugins`, up to `PluginsEnd`.
    fn plugins(&mut self) -> Vec<PluginReport> {
        self.send(&Request::Plugins);
        self.until(|r| *r == Report::PluginsEnd)
            .into_iter()
            .filter_map(|r| match r {
                Report::Plugin(plugin) => Some(plugin),
                _ => None,
            })
            .collect()
    }
}

// --- test plugins ----------------------------------------------------------

trait SvcB: Send + Sync {}
impl Service for dyn SvcB {
    const ID: &'static str = "probe-test-b";
    const MULTIPLE: bool = false;
}
impl SvcB for () {}

trait SvcC: Send + Sync {}
impl Service for dyn SvcC {
    const ID: &'static str = "probe-test-c";
    const MULTIPLE: bool = false;
}
impl SvcC for () {}

type Provide = fn(&mut Context<'_>) -> Result<(), PluginError>;

struct Fake {
    manifest: Manifest,
    /// How many times `manifest()` was called.
    calls: Arc<AtomicUsize>,
    provide: Provide,
}

impl Plugin for Fake {
    fn manifest(&self) -> Manifest {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.manifest.clone()
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        (self.provide)(ctx)?;
        Ok(Box::new(()))
    }
}

fn nothing(_: &mut Context<'_>) -> Result<(), PluginError> {
    Ok(())
}

fn provide_b(ctx: &mut Context<'_>) -> Result<(), PluginError> {
    ctx.provide::<dyn SvcB>(Arc::new(()))
}

fn provide_c(ctx: &mut Context<'_>) -> Result<(), PluginError> {
    ctx.provide::<dyn SvcC>(Arc::new(()))
}

fn refuse(_: &mut Context<'_>) -> Result<(), PluginError> {
    Err(PluginError::new("refused"))
}

/// The profile: `app` (after the first frame) needs `b` (a command), which
/// needs `c` (by hand); `w` waits for a project; `bad` fails when activated.
struct Profile {
    set: PluginSet,
    calls: Vec<(&'static str, Arc<AtomicUsize>)>,
}

fn profile(with_bad: bool) -> Profile {
    let b = service::<dyn SvcB>();
    let c = service::<dyn SvcC>();
    let mut plugins = vec![
        (
            "app",
            Manifest {
                provides: Vec::new(),
                requires: vec![b],
                activation: vec![Activation::AfterFirstFrame],
            },
            nothing as Provide,
        ),
        (
            "b",
            Manifest {
                provides: vec![b],
                requires: vec![c],
                activation: vec![Activation::Command("b.open".into())],
            },
            provide_b,
        ),
        (
            "c",
            Manifest {
                provides: vec![c],
                requires: Vec::new(),
                activation: vec![Activation::Manual],
            },
            provide_c,
        ),
        (
            "w",
            Manifest {
                provides: Vec::new(),
                requires: Vec::new(),
                activation: vec![Activation::Workspace(vec!["**/*.rs".into()])],
            },
            nothing,
        ),
    ];
    if with_bad {
        plugins.push((
            "bad",
            Manifest {
                provides: Vec::new(),
                requires: Vec::new(),
                activation: vec![Activation::Manual],
            },
            refuse,
        ));
    }
    let mut set = PluginSet::new();
    let mut calls = Vec::new();
    for (id, manifest, provide) in plugins {
        let count = Arc::new(AtomicUsize::new(0));
        calls.push((id, Arc::clone(&count)));
        set = set.with(
            id,
            Fake {
                manifest,
                calls: count,
                provide,
            },
        );
    }
    Profile { set, calls }
}

/// Records what the runner asked the program to do.
struct Recorder(mpsc::Sender<String>);

impl Driver for Recorder {
    fn open(&self, target: OpenTarget) {
        let _ = self.0.send(format!("open {target:?}"));
    }

    fn quit(&self) {
        let _ = self.0.send("quit".into());
    }
}

/// One process as the app runs it: the first frame, then `start_kernel`
/// and `attach`.
struct Session {
    conn: Conn,
    kernel: Arc<Kernel>,
    calls: Vec<(&'static str, Arc<AtomicUsize>)>,
    _probe: Probe,
    _runner: Runner,
}

fn session(selection: Option<&str>, with_bad: bool) -> Session {
    let runner = Runner::new();
    let probe = Probe::with_vars(runner.socket.clone(), selection.map(OsStr::new));
    probe.first_frame(SystemTime::now());
    let mut conn = runner.accept();
    assert!(matches!(conn.next(), Report::FirstFrame { .. }));
    let Profile { set, calls } = profile(with_bad);
    let kernel = probe.start_kernel(set);
    let (sender, _) = mpsc::channel();
    probe.attach(Arc::clone(&kernel), Arc::new(Recorder(sender)));
    Session {
        conn,
        kernel,
        calls,
        _probe: probe,
        _runner: runner,
    }
}

fn ids(plugins: &[PluginReport]) -> Vec<&str> {
    plugins.iter().map(|p| p.id.as_str()).collect()
}

fn state(kernel: &Kernel, id: &str) -> Option<PluginState> {
    kernel
        .status()
        .into_iter()
        .find(|s| s.id == id)
        .map(|s| s.state)
}

fn assert_each_manifest_read_once(calls: &[(&'static str, Arc<AtomicUsize>)]) {
    for (id, count) in calls {
        assert_eq!(count.load(Ordering::SeqCst), 1, "manifest() of {id}");
    }
}

fn no_failure(reports: &[Report]) {
    for report in reports {
        assert!(
            !matches!(report, Report::PluginFailed { .. } | Report::Error { .. }),
            "{report:?}"
        );
    }
}

// --- trimming and attach ---------------------------------------------------

#[test]
fn probe_trims_to_the_closure_and_meets_command_and_manual() {
    let mut s = session(Some("closure:app"), true);
    no_failure(&s.conn.until(|r| *r == Report::Ready));
    let plugins = s.conn.plugins();
    assert_eq!(ids(&plugins), ["b", "c"]);
    assert_eq!(plugins[0].activation, [ActivationKind::Command]);
    assert_eq!(plugins[1].activation, [ActivationKind::Manual]);
    for plugin in &plugins {
        assert_eq!(plugin.state, "Active", "{plugin:?}");
        assert!(plugin.manifest.is_some(), "{plugin:?}");
        assert!(plugin.activate.is_some(), "{plugin:?}");
    }
    // The dropped plugins never started; every manifest was read once, in
    // `prepare`, also those of the dropped plugins.
    for id in ["app", "w", "bad"] {
        assert_eq!(state(&s.kernel, id), None, "{id}");
    }
    assert_each_manifest_read_once(&s.calls);
}

#[test]
fn probe_trims_to_the_closure_and_self() {
    let mut s = session(Some("closure+self:app"), true);
    no_failure(&s.conn.until(|r| *r == Report::Ready));
    let plugins = s.conn.plugins();
    assert_eq!(ids(&plugins), ["app", "b", "c"]);
    assert_eq!(plugins[0].activation, [ActivationKind::AfterFirstFrame]);
    assert!(plugins.iter().all(|p| p.state == "Active"), "{plugins:?}");
    assert_each_manifest_read_once(&s.calls);
}

#[test]
fn probe_meets_workspace_conditions_with_facts() {
    let mut s = session(Some("closure+self:w"), true);
    no_failure(&s.conn.until(|r| *r == Report::Ready));
    assert_eq!(state(&s.kernel, "w"), Some(PluginState::Active));
    assert_each_manifest_read_once(&s.calls);
}

#[test]
fn probe_is_ready_at_once_for_an_empty_closure() {
    let mut s = session(Some("closure:w"), true);
    no_failure(&s.conn.until(|r| *r == Report::Ready));
    settle(&mut s);
    assert_eq!(ids(&s.conn.plugins()), Vec::<&str>::new());
}

#[test]
fn probe_reports_a_kept_plugin_that_failed() {
    let mut s = session(Some("closure+self:bad"), true);
    let reports = s.conn.until(|r| matches!(r, Report::PluginFailed { .. }));
    let Some(Report::PluginFailed { id, message }) = reports.last() else {
        unreachable!();
    };
    assert_eq!(id, "bad");
    assert!(!message.is_empty());
    assert!(!reports.contains(&Report::Ready));
    // Once only, and never `Ready`.
    let later = s.conn.drain(Duration::from_millis(300));
    assert!(
        later
            .iter()
            .all(|r| !matches!(r, Report::PluginFailed { .. } | Report::Ready)),
        "{later:?}"
    );
}

#[test]
fn probe_reports_a_selection_it_cannot_trim() {
    let mut s = session(Some("closure:nope"), true);
    let Report::Error { message } = s.conn.next() else {
        panic!("expected an error");
    };
    assert!(message.starts_with("plugins closure of nope"), "{message}");
    // The whole set started.
    settle(&mut s);
    assert_eq!(ids(&s.conn.plugins()), ["app", "b", "c", "w", "bad"]);

    let mut s = session(Some("everything"), true);
    let Report::Error { message } = s.conn.next() else {
        panic!("expected an error");
    };
    assert!(message.starts_with("plugins "), "{message}");
    settle(&mut s);
    assert_eq!(ids(&s.conn.plugins()), ["app", "b", "c", "w", "bad"]);
}

#[test]
fn probe_parses_the_selection() {
    assert_eq!(parse_selection(None), Ok(None));
    assert_eq!(parse_selection(Some(OsStr::new(""))), Ok(None));
    assert_eq!(
        parse_selection(Some(OsStr::new("closure:git"))),
        Ok(Some(Selection::Closure("git".into())))
    );
    assert_eq!(
        parse_selection(Some(OsStr::new("closure+self:git"))),
        Ok(Some(Selection::ClosureAndSelf("git".into())))
    );
    for bad in ["closure:", "closure+self:", "git", "self:git"] {
        assert!(parse_selection(Some(OsStr::new(bad))).is_err(), "{bad}");
    }
    let not_utf8 = OsStr::from_bytes(b"closure:\xff");
    assert!(parse_selection(Some(not_utf8)).is_err());
}

// --- the whole profile -----------------------------------------------------

/// Waits until the kernel is quiescent and the probe has said so.
fn settle(s: &mut Session) -> u64 {
    loop {
        let reports = s.conn.until(|r| matches!(r, Report::Quiescent { .. }));
        no_failure(&reports);
        let Some(Report::Quiescent { epoch }) = reports.last() else {
            unreachable!();
        };
        let later = s.conn.drain(Duration::from_millis(200));
        no_failure(&later);
        match later.last() {
            None => return *epoch,
            Some(Report::Quiescent { epoch }) => return *epoch,
            Some(_) => {}
        }
    }
}

#[test]
fn probe_answers_epoch_and_plugins() {
    let mut s = session(None, true);
    let epoch = settle(&mut s);
    s.conn.send(&Request::Epoch);
    assert_eq!(s.conn.next(), Report::Epoch(Some(epoch)));
    assert_eq!(s.kernel.quiescent_epoch(), Some(epoch));

    let plugins = s.conn.plugins();
    // Not `bench-probe`; in set order, with every activation condition.
    assert_eq!(ids(&plugins), ["app", "b", "c", "w", "bad"]);
    let kinds: Vec<_> = plugins.iter().map(|p| p.activation.clone()).collect();
    assert_eq!(
        kinds,
        [
            vec![ActivationKind::AfterFirstFrame],
            vec![ActivationKind::Command],
            vec![ActivationKind::Manual],
            vec![ActivationKind::Workspace],
            vec![ActivationKind::Manual],
        ]
    );
    let states: Vec<_> = plugins.iter().map(|p| p.state.as_str()).collect();
    // No facts for the whole profile: `w` stays inactive, as in an empty
    // window; `app` waits for `b`, which nothing asked for.
    assert_eq!(
        states,
        ["Waiting", "Inactive", "Inactive", "Inactive", "Inactive"]
    );
    assert!(plugins.iter().all(|p| p.manifest.is_some()));
    assert_each_manifest_read_once(&s.calls);
    // Not trimmed: no `Ready` before `ActivateAll`.
    assert!(
        !s.conn
            .drain(Duration::from_millis(200))
            .contains(&Report::Ready)
    );
}

#[test]
fn probe_activates_all() {
    let mut s = session(None, false);
    settle(&mut s);
    s.conn.send(&Request::ActivateAll);
    no_failure(&s.conn.until(|r| *r == Report::Ready));
    for id in ["app", "b", "c", "w"] {
        assert_eq!(state(&s.kernel, id), Some(PluginState::Active), "{id}");
    }
}

#[test]
fn probe_activate_all_reports_failures() {
    let mut s = session(None, true);
    settle(&mut s);
    s.conn.send(&Request::ActivateAll);
    let reports = s.conn.until(|r| matches!(r, Report::PluginFailed { .. }));
    assert!(
        matches!(reports.last(), Some(Report::PluginFailed { id, .. }) if id == "bad"),
        "{reports:?}"
    );
    let later = s.conn.drain(Duration::from_millis(300));
    assert!(!reports.contains(&Report::Ready) && !later.contains(&Report::Ready));
}

// --- busy and quiescent ----------------------------------------------------

#[test]
fn probe_drops_stale_transitions() {
    let (sender, receiver) = mpsc::channel();
    let newest = AtomicU64::new(0);
    forward(&newest, &sender, 3, Report::Busy { epoch: 3 });
    forward(&newest, &sender, 2, Report::Quiescent { epoch: 2 });
    forward(&newest, &sender, 3, Report::Quiescent { epoch: 3 });
    forward(&newest, &sender, 4, Report::Quiescent { epoch: 4 });
    forward(&newest, &sender, 1, Report::Busy { epoch: 1 });
    let sent: Vec<Report> = receiver
        .try_iter()
        .map(|msg| match msg {
            Msg::Activity(report) => report,
            _ => panic!("not a transition"),
        })
        .collect();
    assert_eq!(
        sent,
        [Report::Busy { epoch: 3 }, Report::Quiescent { epoch: 4 }]
    );
}

#[test]
fn probe_keeps_the_kernel_quiescent() {
    let mut s = session(Some("closure+self:app"), false);
    let mut reports = s.conn.until(|r| *r == Report::Ready);
    reports.extend(s.conn.drain(Duration::from_millis(500)));
    // Every transition the probe forwarded is newer than the one before,
    // and after the last `Quiescent` the kernel stayed quiescent at that
    // epoch: the probe's listeners did not make it busy again.
    let epochs: Vec<u64> = reports
        .iter()
        .filter_map(|r| match r {
            Report::Busy { epoch } | Report::Quiescent { epoch } => Some(*epoch),
            _ => None,
        })
        .collect();
    assert!(epochs.windows(2).all(|w| w[0] < w[1]), "{epochs:?}");
    let last = reports
        .iter()
        .rev()
        .find(|r| matches!(r, Report::Busy { .. } | Report::Quiescent { .. }));
    let Some(Report::Quiescent { epoch }) = last else {
        panic!("not quiescent: {reports:?}");
    };
    assert_eq!(s.kernel.quiescent_epoch(), Some(*epoch));
}

// --- without a kernel, order, errors ---------------------------------------

#[test]
fn probe_queues_requests_until_drive() {
    let runner = Runner::new();
    let probe = Probe::with_vars(runner.socket.clone(), None);
    probe.first_frame(SystemTime::now());
    let mut conn = runner.accept();
    assert!(matches!(conn.next(), Report::FirstFrame { .. }));
    for request in [
        Request::Epoch,
        Request::Plugins,
        Request::ActivateAll,
        Request::Open(OpenTarget::Scroll),
        Request::Quit,
    ] {
        conn.send(&request);
    }
    assert_eq!(conn.drain(Duration::from_millis(200)), []);
    let (sender, done) = mpsc::channel();
    probe.drive(Arc::new(Recorder(sender)));
    assert_eq!(conn.next(), Report::Epoch(None));
    assert_eq!(conn.next(), Report::PluginsEnd);
    assert_eq!(conn.next(), Report::Ready);
    assert_eq!(done.recv_timeout(WAIT).as_deref(), Ok("open Scroll"));
    assert_eq!(done.recv_timeout(WAIT).as_deref(), Ok("quit"));
}

#[test]
fn probe_sends_first_frame_then_earlier_reports_in_order() {
    let runner = Runner::new();
    let probe = Probe::with_vars(runner.socket.clone(), None);
    probe.error("one");
    probe.child(7, ChildKind::Other, "x");
    let shown = SystemTime::UNIX_EPOCH + Duration::from_secs(5);
    probe.milestone(Milestone::ProjectHome, shown);
    let presented = SystemTime::UNIX_EPOCH + Duration::from_secs(4);
    probe.first_frame(presented);
    let mut conn = runner.accept();
    assert_eq!(
        conn.next(),
        Report::FirstFrame {
            pid: std::process::id(),
            presented,
        }
    );
    assert_eq!(
        conn.next(),
        Report::Error {
            message: "one".into()
        }
    );
    assert_eq!(
        conn.next(),
        Report::Child {
            pid: 7,
            kind: ChildKind::Other,
            id: "x".into(),
        }
    );
    assert_eq!(
        conn.next(),
        Report::Milestone {
            milestone: Milestone::ProjectHome,
            presented: shown,
        }
    );
}

#[test]
fn probe_reports_a_bad_request_line() {
    let runner = Runner::new();
    let probe = Probe::with_vars(runner.socket.clone(), None);
    probe.first_frame(SystemTime::now());
    let mut conn = runner.accept();
    assert!(matches!(conn.next(), Report::FirstFrame { .. }));
    conn.raw("hello");
    let Report::Error { message } = conn.next() else {
        panic!("expected an error");
    };
    assert!(message.starts_with("wire "), "{message}");
}
