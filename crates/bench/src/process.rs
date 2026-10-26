//! The scheduling of the process-level benchmarks (docs/design/bench.md
//! 「进程级基准」): launches the program under test with a fresh socket and a
//! temporary `HOME`, reads the probe's reports, sends its requests, and ends
//! the process group when a wait passes its deadline.
//!
//! The runner reaches the child processes only through [`Launcher`]
//! (launch, pid, wait, end the process group) and [`ProcessInfo`]
//! (`start_time`, `phys_footprint`, `children`). `benches/process.rs`
//! implements them with `std::process` and `platform::process`; the
//! `process_*` tests with scripted fakes.

use std::collections::{HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::wire::{
    ActivationKind, ChildKind, FrameStats, Milestone, OpenTarget, PLUGINS_VAR, PluginReport,
    Report, Request, SOCKET_VAR, TimeSource,
};
use crate::{BenchError, Samples};

#[cfg(test)]
mod tests;

/// Memory benchmarks run at most this many processes at once; timing
/// benchmarks one (「进程级基准」).
pub const MEMORY_PARALLEL: usize = 3;

/// What the launched program must not inherit from the runner: the
/// developer's log filter and the agents' configuration directories (the
/// program reads its data under the temporary `HOME` only).
const REMOVED: &[&str] = &["LECTERN_LOG", "CLAUDE_CONFIG_DIR", "CODEX_HOME"];

/// The footprint a launch is given when it did not become quiescent within
/// [`Limits::first_quiescent`] of its first frame: over every byte budget
/// (ADR 0017: no quiescence is a failure). The report shows it as such.
pub const NEVER_QUIESCENT: u64 = u64::MAX;

/// How long `Drop` waits for a killed process to be reaped.
const REAP: Duration = Duration::from_secs(2);
/// How long an exit is given to deliver the reports written before it (the
/// probe's `Error` before exit code 3).
const LAST_WORDS: Duration = Duration::from_millis(300);

/// The error for a `handler` first frame on the judging machine (ADR 0052).
pub const HANDLER_ON_THE_JUDGE: &str = "首帧的来源是 handler：判定机器上 drawable 没有给出 presentedTime（屏幕锁定、显示器关闭或没有接？ADR 0052）";

/// Starts the program under test.
pub trait Launcher: Sync {
    /// Starts `launch.program` as the leader of a new process group (its
    /// group ID is its pid), with `launch.env` over the runner's
    /// environment.
    fn launch(&self, launch: &Launch) -> io::Result<Box<dyn Launched>>;
}

/// One launched process.
pub trait Launched: Send {
    fn pid(&self) -> u32;
    /// The exit, once the process has exited (it is then reaped).
    fn try_wait(&mut self) -> io::Result<Option<Exit>>;
    /// `SIGKILL` to the process group whose ID is [`pid`](Launched::pid).
    fn kill_group(&mut self);
    /// `SIGKILL` to one process: a descendant that left the group.
    fn kill(&mut self, pid: u32);
}

/// What the runner reads about a process (`platform::process` on macOS).
pub trait ProcessInfo: Sync {
    fn start_time(&self, pid: u32) -> io::Result<SystemTime>;
    fn phys_footprint(&self, pid: u32) -> io::Result<u64>;
    fn children(&self, pid: u32) -> io::Result<Vec<u32>>;
}

/// The program and its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    /// Set (`Some`) or removed (`None`) over the runner's environment.
    pub env: Vec<(OsString, Option<OsString>)>,
}

impl Launch {
    /// The value this launch sets for `name`.
    pub fn var(&self, name: &str) -> Option<&OsStr> {
        self.env
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, value)| value.as_deref())
    }
}

/// How a process exited: its code, or None when a signal ended it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i32>,
}

impl Exit {
    fn describe(self) -> String {
        match self.code {
            Some(code) => format!("退出码 {code}"),
            None => "被信号结束".to_owned(),
        }
    }
}

/// The deadlines of「进程级基准」's table, and the quiet time before an
/// idle sample. The tests shorten them.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Launch to `FirstFrame`, the connection included.
    pub first_frame: Duration,
    /// The first frame to the first `Quiescent`; past it the idle sample is
    /// [`Idle::NotQuiescent`] (ADR 0017).
    pub first_quiescent: Duration,
    /// `Quiescent` again, and `Ready`, after a request or an invalid sample.
    pub again: Duration,
    /// The answers to `Epoch` and to `Plugins` (up to `PluginsEnd`).
    pub reply: Duration,
    /// `Quit` to the exit.
    pub quit: Duration,
    /// The quiet time before an idle sample, and the wait after the first
    /// frame of a program without a kernel.
    pub settle: Duration,
    /// How often a wait checks whether the process exited.
    pub poll: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            first_frame: Duration::from_secs(10),
            first_quiescent: Duration::from_secs(30),
            again: Duration::from_secs(30),
            reply: Duration::from_secs(10),
            quit: Duration::from_secs(10),
            settle: Duration::from_secs(5),
            poll: Duration::from_millis(20),
        }
    }
}

/// A process plugin's host or a language server reported by the probe: the
/// root of its process group (ADR 0018).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spawned {
    pub pid: u32,
    pub kind: ChildKind,
    pub id: String,
}

/// The outcome of [`Session::idle`].
#[derive(Debug, PartialEq, Eq)]
pub enum Idle<T> {
    /// A valid sample: the quiescent epoch was the same `Some(n)` before
    /// and after it.
    Sample(T),
    /// No `Quiescent` within [`Limits::first_quiescent`] of the first
    /// frame: `over` for the memory budgets (ADR 0017).
    NotQuiescent,
}

/// Launches processes and runs the process-level measurements.
pub struct Scheduler<'a> {
    launcher: &'a dyn Launcher,
    info: &'a dyn ProcessInfo,
    limits: Limits,
    judging: bool,
}

impl<'a> Scheduler<'a> {
    pub fn new(launcher: &'a dyn Launcher, info: &'a dyn ProcessInfo) -> Scheduler<'a> {
        Scheduler {
            launcher,
            info,
            limits: Limits::default(),
            judging: false,
        }
    }

    pub fn with_limits(mut self, limits: Limits) -> Scheduler<'a> {
        self.limits = limits;
        self
    }

    /// On the judging machine ([`Ctx::judge`](crate::Ctx::judge), ADR
    /// 0052) every first frame must come with its `presentedTime`: a
    /// `FirstFrame` whose source is `handler` ends the session with an
    /// error, so the benchmark is `error`, not a sample.
    pub fn judging(mut self, judging: bool) -> Scheduler<'a> {
        self.judging = judging;
        self
    }

    pub fn info(&self) -> &'a dyn ProcessInfo {
        self.info
    }

    /// Launches `program` with a new socket and a new temporary `HOME`
    /// (and `LECTERN_BENCH_PLUGINS` when `plugins` is given), and waits for
    /// its `FirstFrame` ([`Limits::first_frame`]). An error ends the
    /// process group.
    pub fn session(
        &self,
        program: &Path,
        plugins: Option<&str>,
    ) -> Result<Session<'a>, BenchError> {
        Session::start(self, program, plugins)
    }

    /// Cold start (「进程级基准」): `warmups` launches not counted, then
    /// `samples` launches, one at a time. Each sample is the first frame's
    /// `presented` minus the process's `start_time`, with the source of
    /// that time (ADR 0052); each launch is quit and must exit. Stops at the
    /// first failure.
    pub fn cold_start(
        &self,
        program: &Path,
        warmups: usize,
        samples: usize,
    ) -> Result<ColdStart, BenchError> {
        let mut times = Vec::with_capacity(samples);
        let mut sources = Vec::with_capacity(samples);
        for launch in 0..warmups + samples {
            let session = self.session(program, None)?;
            let started = self.info.start_time(session.pid()).map_err(|e| {
                BenchError::new(format!("读不到进程 {} 的创建时间：{e}", session.pid()))
            })?;
            let time = session.presented().duration_since(started).map_err(|_| {
                BenchError::new(format!("进程 {} 的首帧早于它的创建时间", session.pid()))
            })?;
            let source = session.source();
            session.quit()?;
            if launch >= warmups {
                times.push(time);
                sources.push(source);
            }
        }
        Ok(ColdStart {
            samples: Samples(times),
            sources,
        })
    }

    /// `app/idle_empty` (「进程级基准」): `launches` launches of the whole
    /// profile, one at a time, each the main process's `phys_footprint` at a
    /// valid idle sample. A launch that was not quiescent within
    /// [`Limits::first_quiescent`] is [`Idle::NotQuiescent`] (`over`, not an
    /// error).
    ///
    /// Not [`Scheduler::memory`]'s three at once: with three launches
    /// together (their windows cover each other) the `Quit` of one
    /// `lectern` took more than 10 s in about a third of the launches.
    /// A sample of a hung process shows its quit thread done (the kernel,
    /// executor and log threads gone) and the main thread idle in the run
    /// loop, before the 10 ms timer loop of `quit.rs` that ends in
    /// `cx.quit()`; some exited by themselves after 9 to 11 s. One launch at
    /// a time never hung (bench design「与设计的差异」, step 5).
    pub fn idle_footprints(
        &self,
        program: &Path,
        launches: usize,
    ) -> Result<Vec<Idle<u64>>, BenchError> {
        let mut runs = Vec::with_capacity(launches);
        for _ in 0..launches {
            let mut session = self.session(program, None)?;
            let idle = session.idle(|s| s.footprint())?;
            session.quit()?;
            runs.push(idle);
        }
        Ok(runs)
    }

    /// `app/unclassified_children`: one launch of the whole profile; at a
    /// valid idle sample, the descendants of the main process that belong
    /// to no reported process-plugin or language-server group.
    pub fn unclassified_children(&self, program: &Path) -> Result<Unclassified, BenchError> {
        let mut session = self.session(program, None)?;
        let idle = session.idle(|s| s.unclassified())?;
        let Idle::Sample(found) = idle else {
            return Err(not_quiescent("app/unclassified_children", self.limits));
        };
        session.quit()?;
        Ok(found)
    }

    /// `plugin/after_first_frame`: `launches` launches of the whole
    /// profile, one at a time (the sum is a timing); each the activation
    /// times of the plugins with an `AfterFirstFrame` condition, added up,
    /// read from the `Plugins` reply at a valid idle sample.
    pub fn after_first_frame(
        &self,
        program: &Path,
        launches: usize,
    ) -> Result<Samples, BenchError> {
        let mut sums = Vec::with_capacity(launches);
        for _ in 0..launches {
            let mut session = self.session(program, None)?;
            let idle = session.idle(|s| s.plugins())?;
            let Idle::Sample(plugins) = idle else {
                return Err(not_quiescent("plugin/after_first_frame", self.limits));
            };
            sums.push(after_first_frame_time(&plugins)?);
            session.quit()?;
        }
        Ok(Samples(sums))
    }

    /// The plugins of the whole profile: the `Plugins` reply of one launch
    /// at a valid idle sample (before the kernel starts, the reply is
    /// empty), in the kernel's order (`bench-probe` is not in it).
    pub fn plugin_list(&self, program: &Path) -> Result<Vec<PluginReport>, BenchError> {
        let mut session = self.session(program, None)?;
        let idle = session.idle(|s| s.plugins())?;
        let Idle::Sample(plugins) = idle else {
            return Err(not_quiescent("plugin_list", self.limits));
        };
        session.quit()?;
        Ok(plugins)
    }

    /// One launch of a trimmed plugin set (`selection` is the value of
    /// `LECTERN_BENCH_PLUGINS`): waits for `Ready` and a valid idle sample,
    /// whose value is the main process's `phys_footprint` plus those of the
    /// process-plugin groups; then asks for the `Plugins` reply.
    pub fn plugin_launch(
        &self,
        program: &Path,
        selection: &str,
    ) -> Result<PluginLaunch, BenchError> {
        let mut session = self.session(program, Some(selection))?;
        session.ready()?;
        let idle = session.idle(|s| s.footprint_with_plugins())?;
        let Idle::Sample(footprint) = idle else {
            return Err(not_quiescent(selection, self.limits));
        };
        let plugins = session.plugins()?;
        session.quit()?;
        Ok(PluginLaunch { footprint, plugins })
    }

    /// One pair of launches for plugin `id` (「进程级基准」每个插件):
    /// `closure:<id>`, then `closure+self:<id>`. The pair's activation time
    /// is the plugin's four durations in the second launch.
    pub fn plugin_pair(&self, program: &Path, id: &str) -> Result<PluginPair, BenchError> {
        let without = self.plugin_launch(program, &format!("closure:{id}"))?;
        let with = self.plugin_launch(program, &format!("closure+self:{id}"))?;
        let activate = with
            .plugins
            .iter()
            .find(|plugin| plugin.id == id)
            .ok_or_else(|| BenchError::new(format!("Plugins 的应答里没有插件 {id}")))
            .and_then(activation_time)?;
        Ok(PluginPair {
            without: without.footprint,
            with: with.footprint,
            activate,
        })
    }

    /// The pairs of plugin `id`: one, and two more when the first pair's
    /// activation time is over its budget (`over`), as「进程级基准」
    /// says; the statistic is then taken over the three.
    pub fn plugin(
        &self,
        program: &Path,
        id: &str,
        over: impl Fn(Duration) -> bool,
    ) -> Result<Vec<PluginPair>, BenchError> {
        let mut pairs = vec![self.plugin_pair(program, id)?];
        if over(pairs[0].activate) {
            for _ in 0..2 {
                pairs.push(self.plugin_pair(program, id)?);
            }
        }
        Ok(pairs)
    }

    /// `launches` runs of `run`, at most [`MEMORY_PARALLEL`] at once (one at
    /// a time on the judging machine: windows opened together cover each
    /// other, and a covered window never shows a drawable, so its first
    /// frame would never come, ADR 0052); the
    /// values in the order the runs were started. The first error (by that
    /// order) is returned; once a run has failed, the runs not yet started
    /// are not started.
    pub fn memory<T: Send>(
        &self,
        launches: usize,
        run: impl Fn(&Scheduler<'a>) -> Result<T, BenchError> + Sync,
    ) -> Result<Vec<T>, BenchError> {
        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<Option<Result<T, BenchError>>>> =
            Mutex::new((0..launches).map(|_| None).collect());
        thread::scope(|scope| {
            let parallel = if self.judging { 1 } else { MEMORY_PARALLEL };
            for _ in 0..parallel.min(launches) {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::SeqCst);
                        if index >= launches {
                            return;
                        }
                        let result = run(self);
                        let failed = result.is_err();
                        results.lock().unwrap_or_else(PoisonError::into_inner)[index] =
                            Some(result);
                        if failed {
                            next.store(launches, Ordering::SeqCst);
                            return;
                        }
                    }
                });
            }
        });
        let mut values = Vec::with_capacity(launches);
        for result in results
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .into_iter()
            .flatten()
        {
            values.push(result?);
        }
        Ok(values)
    }
}

/// What [`Scheduler::unclassified_children`] found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unclassified {
    /// How many descendants of the main process belong to no reported
    /// group (those of a [`ChildKind::Other`] root included).
    pub processes: usize,
    /// Their `phys_footprint` added up.
    pub bytes: u64,
}

/// One launch of a trimmed plugin set ([`Scheduler::plugin_launch`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginLaunch {
    /// The main process plus the process-plugin groups.
    pub footprint: u64,
    pub plugins: Vec<PluginReport>,
}

/// The two launches of plugin `id`: its dependency closure without it, and
/// with it ([`Scheduler::plugin_pair`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginPair {
    pub without: u64,
    pub with: u64,
    /// The plugin's manifest, activate, commit and started durations in the
    /// launch with it, added up (kernel「性能」).
    pub activate: Duration,
}

impl PluginPair {
    /// `plugin/<id>/resident`: the footprint the plugin adds. Zero when the
    /// launch with it measured less than the one without (noise larger
    /// than the plugin); [`PluginPair::difference`] keeps the sign.
    pub fn resident(&self) -> u64 {
        self.with.saturating_sub(self.without)
    }

    pub fn difference(&self) -> i128 {
        i128::from(self.with) - i128::from(self.without)
    }
}

/// The four durations of a plugin's activation added up. The plugin must be
/// `Active` and have all four: a plugin that did not activate has no
/// activation time to report.
pub fn activation_time(plugin: &PluginReport) -> Result<Duration, BenchError> {
    if plugin.state != "Active" {
        return Err(BenchError::new(format!(
            "插件 {} 的状态是 {}，不是 Active",
            plugin.id, plugin.state
        )));
    }
    match (
        plugin.manifest,
        plugin.activate,
        plugin.commit,
        plugin.started,
    ) {
        (Some(manifest), Some(activate), Some(commit), Some(started)) => {
            Ok(manifest + activate + commit + started)
        }
        _ => Err(BenchError::new(format!(
            "插件 {} 的四段耗时不全",
            plugin.id
        ))),
    }
}

/// `plugin/after_first_frame` of one launch: the activation times of the
/// plugins whose manifest has an `AfterFirstFrame` condition (any other
/// condition besides it too), added up.
pub fn after_first_frame_time(plugins: &[PluginReport]) -> Result<Duration, BenchError> {
    let mut sum = Duration::ZERO;
    for plugin in plugins {
        if plugin.activation.contains(&ActivationKind::AfterFirstFrame) {
            sum += activation_time(plugin)?;
        }
    }
    Ok(sum)
}

fn not_quiescent(what: &str, limits: Limits) -> BenchError {
    BenchError::new(format!(
        "{what}：首帧后 {:?} 内没有静止（ADR 0017）",
        limits.first_quiescent
    ))
}

/// The root processes and all their descendants, breadth first, each pid
/// once. A process that exited during the walk (its `children` fails) is
/// left out (「进程级基准」, ADR 0018).
pub fn expand(info: &dyn ProcessInfo, roots: &[u32]) -> Vec<u32> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    let mut queue: VecDeque<u32> = roots.iter().copied().collect();
    while let Some(pid) = queue.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        if let Ok(children) = info.children(pid) {
            found.push(pid);
            queue.extend(children);
        }
    }
    found
}

/// `sudo -n purge`, which empties the file cache before
/// `app/cold_start_purged`. False when sudo is missing, would ask for a
/// password or purge failed: the benchmark is then `skipped`. `path`
/// replaces `PATH` for this command only (the tests' fake `sudo`).
pub fn purge(path: Option<&OsStr>) -> io::Result<bool> {
    let mut command = Command::new("sudo");
    command
        .args(["-n", "purge"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    match command.status() {
        Ok(status) => Ok(status.success()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// The launched process. Dropping it while the process runs ends the
/// whole group, then the descendants that left it.
struct Process<'a> {
    child: Box<dyn Launched>,
    info: &'a dyn ProcessInfo,
    exit: Option<Exit>,
    /// The `Child` roots the probe reported.
    roots: Vec<u32>,
}

impl Process<'_> {
    fn poll(&mut self) -> Result<Option<Exit>, BenchError> {
        if self.exit.is_none() {
            self.exit = self
                .child
                .try_wait()
                .map_err(|e| BenchError::new(format!("等待进程 {} 出错：{e}", self.child.pid())))?;
        }
        Ok(self.exit)
    }
}

impl Process<'_> {
    /// Ends the process group if the process still runs, then the
    /// descendants that left it. Once only.
    fn end(&mut self) {
        if self.exit.is_some() {
            return;
        }
        if let Ok(Some(exit)) = self.child.try_wait() {
            self.exit = Some(exit);
            return;
        }
        let root = self.child.pid();
        let mut roots = vec![root];
        roots.extend(&self.roots);
        // Read the tree before the group dies: its members' children are
        // reparented once they are gone.
        let tree = expand(self.info, &roots);
        self.child.kill_group();
        for pid in tree.into_iter().filter(|&pid| pid != root) {
            self.child.kill(pid);
        }
        let end = Instant::now() + REAP;
        while Instant::now() < end {
            match self.child.try_wait() {
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Ok(Some(exit)) => {
                    self.exit = Some(exit);
                    return;
                }
                Err(_) => return,
            }
        }
    }
}

impl Drop for Process<'_> {
    fn drop(&mut self) {
        self.end();
    }
}

enum Event {
    Report(Report),
    /// A line that is not a valid report.
    Bad(String),
    /// The probe closed the connection.
    Closed,
}

/// Whether the kernel was last reported busy or quiescent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Activity {
    Unknown,
    Busy,
    Quiescent,
}

/// The cold-start samples and, in the same order, the source of each
/// sample's first-frame time (ADR 0052).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColdStart {
    pub samples: Samples,
    pub sources: Vec<TimeSource>,
}

/// One launch, from its `FirstFrame` to its exit.
pub struct Session<'a> {
    limits: Limits,
    pid: u32,
    presented: SystemTime,
    source: TimeSource,
    /// When `FirstFrame` arrived: the first `Quiescent` is due
    /// [`Limits::first_quiescent`] after it.
    first_frame_at: Instant,
    writer: UnixStream,
    events: Receiver<Event>,
    closed: bool,
    activity: Activity,
    /// Times a `Busy` arrived; `calm` compares it.
    busy_count: u64,
    children: Vec<Spawned>,
    process: Process<'a>,
    /// The socket and `HOME`, removed after the process.
    _dir: tempfile::TempDir,
}

impl<'a> Session<'a> {
    fn start(
        scheduler: &Scheduler<'a>,
        program: &Path,
        plugins: Option<&str>,
    ) -> Result<Session<'a>, BenchError> {
        let limits = scheduler.limits;
        // Under the system temporary directory: the socket path must fit
        // `sun_path` (104 bytes on macOS).
        let dir = tempfile::Builder::new()
            .prefix("lectern-bench-")
            .tempdir()
            .map_err(|e| BenchError::new(format!("建不了启动目录：{e}")))?;
        let socket = dir.path().join("s");
        let home = dir.path().join("home");
        std::fs::create_dir(&home).map_err(|e| BenchError::new(format!("建不了临时 HOME：{e}")))?;
        let listener = UnixListener::bind(&socket)
            .and_then(|l| l.set_nonblocking(true).map(|()| l))
            .map_err(|e| BenchError::new(format!("建不了 socket {}：{e}", socket.display())))?;
        let mut env: Vec<(OsString, Option<OsString>)> = vec![
            (SOCKET_VAR.into(), Some(socket.into_os_string())),
            ("HOME".into(), Some(home.into_os_string())),
            (PLUGINS_VAR.into(), plugins.map(OsString::from)),
        ];
        env.extend(REMOVED.iter().map(|name| (OsString::from(name), None)));
        let launch = Launch {
            program: program.to_owned(),
            env,
        };
        let child = scheduler
            .launcher
            .launch(&launch)
            .map_err(|e| BenchError::new(format!("启动不了 {}：{e}", program.display())))?;
        let pid = child.pid();
        let mut process = Process {
            child,
            info: scheduler.info,
            exit: None,
            roots: Vec::new(),
        };

        let deadline = Instant::now() + limits.first_frame;
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if let Some(exit) = process.poll()? {
                        // It may have connected, written its last words (the
                        // probe's `Error` before exit code 3) and exited
                        // since the `accept` above: the connection is still
                        // there, and what it wrote is the reason.
                        if let Ok((stream, _)) = listener.accept() {
                            break stream;
                        }
                        return Err(BenchError::new(format!(
                            "进程在连上 socket 之前退出（{}）",
                            exit.describe()
                        )));
                    }
                    if Instant::now() >= deadline {
                        return Err(late("FirstFrame", limits.first_frame));
                    }
                    thread::sleep(limits.poll.min(Duration::from_millis(5)));
                }
                Err(error) => return Err(BenchError::new(format!("socket 出错：{error}"))),
            }
        };
        let (writer, events) = connect(stream)?;
        let mut session = Session {
            limits,
            pid,
            presented: SystemTime::UNIX_EPOCH,
            source: TimeSource::Presented,
            first_frame_at: Instant::now(),
            writer,
            events,
            closed: false,
            activity: Activity::Unknown,
            busy_count: 0,
            children: Vec::new(),
            process,
            _dir: dir,
        };
        let first = limits.first_frame;
        let (from, presented, source) =
            session.wait_for("FirstFrame", deadline, first, |report| match report {
                Report::FirstFrame {
                    pid,
                    presented,
                    source,
                } => Some((*pid, *presented, *source)),
                _ => None,
            })?;
        if from != pid {
            return Err(BenchError::new(format!(
                "FirstFrame 来自进程 {from}，启动的是 {pid}"
            )));
        }
        if scheduler.judging && source == TimeSource::Handler {
            return Err(BenchError::new(HANDLER_ON_THE_JUDGE));
        }
        session.presented = presented;
        session.source = source;
        session.first_frame_at = Instant::now();
        Ok(session)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The first frame's time: its `presentedTime`, or the moment its
    /// presented handler ran ([`Session::source`]).
    pub fn presented(&self) -> SystemTime {
        self.presented
    }

    /// Where [`Session::presented`] came from (ADR 0052).
    pub fn source(&self) -> TimeSource {
        self.source
    }

    /// The process groups the probe reported so far.
    pub fn children(&self) -> &[Spawned] {
        &self.children
    }

    /// The main process's `phys_footprint`.
    pub fn footprint(&self) -> Result<u64, BenchError> {
        self.process
            .info
            .phys_footprint(self.pid)
            .map_err(|e| BenchError::new(format!("读不到进程 {} 的 phys_footprint：{e}", self.pid)))
    }

    /// The main process's `phys_footprint` plus those of the reported
    /// process-plugin groups (the root and all its descendants each, ADR
    /// 0018). Language-server groups are not in it: they are reported on
    /// their own.
    pub fn footprint_with_plugins(&self) -> Result<u64, BenchError> {
        let roots = self.roots(|kind| kind == ChildKind::ProcessPlugin);
        let mut total = self.footprint()?;
        for pid in expand(self.process.info, &roots) {
            // Exited since it was listed (as in `expand`): not counted.
            if let Ok(bytes) = self.process.info.phys_footprint(pid) {
                total += bytes;
            }
        }
        Ok(total)
    }

    /// The descendants of the main process that are in no group a `Child`
    /// report named a process plugin or a language server: the processes
    /// nobody reported, and those of a [`ChildKind::Other`] root. A process
    /// that exited while the walk was reading it is left out.
    pub fn unclassified(&self) -> Result<Unclassified, BenchError> {
        let reported = expand(
            self.process.info,
            &self.roots(|kind| kind != ChildKind::Other),
        );
        let mut found = Unclassified::default();
        for pid in expand(self.process.info, &[self.pid]) {
            if pid == self.pid || reported.contains(&pid) {
                continue;
            }
            // Exited since it was listed: not a child to report.
            if let Ok(bytes) = self.process.info.phys_footprint(pid) {
                found.processes += 1;
                found.bytes += bytes;
            }
        }
        Ok(found)
    }

    /// The roots of the groups whose kind `wanted` accepts.
    fn roots(&self, wanted: impl Fn(ChildKind) -> bool) -> Vec<u32> {
        self.children
            .iter()
            .filter(|child| wanted(child.kind))
            .map(|child| child.pid)
            .collect()
    }

    /// For a program without a kernel (`baseline/empty_window_idle`):
    /// returns [`Limits::settle`] after the first frame's time.
    pub fn settle_after_first_frame(&mut self) -> Result<(), BenchError> {
        let left = (self.presented + self.limits.settle)
            .duration_since(SystemTime::now())
            .unwrap_or_default();
        let deadline = Instant::now() + left;
        while self.recv(deadline, "首帧后的等待")?.is_some() {}
        Ok(())
    }

    /// An idle sample by the rules of kernel「静止」: after a `Quiescent`,
    /// [`Limits::settle`] without `Busy` (a `Busy` starts the wait again,
    /// also one that comes before the first `Epoch` answer), then `Epoch`,
    /// `sample`, `Epoch`. Valid when both answers are the
    /// same `Some(n)`; otherwise the wait starts again, and the third
    /// invalid sample in a row is an error. No `Quiescent` within
    /// [`Limits::first_quiescent`] of the first frame is
    /// [`Idle::NotQuiescent`]; once one came, a later wait for `Quiescent`
    /// longer than [`Limits::again`] is an error.
    pub fn idle<T>(
        &mut self,
        mut sample: impl FnMut(&mut Session<'a>) -> Result<T, BenchError>,
    ) -> Result<Idle<T>, BenchError> {
        let mut invalid = 0;
        let mut seen = false;
        loop {
            let (deadline, limit) = if seen {
                (Instant::now() + self.limits.again, self.limits.again)
            } else {
                (
                    self.first_frame_at + self.limits.first_quiescent,
                    self.limits.first_quiescent,
                )
            };
            if !self.quiescent(deadline)? {
                return if seen {
                    Err(late("Quiescent", limit))
                } else {
                    Ok(Idle::NotQuiescent)
                };
            }
            seen = true;
            if !self.calm()? {
                continue;
            }
            // A `Busy` while waiting for the first answer means the settle
            // time no longer holds: wait for it again.
            let busy = self.busy_count;
            let before = self.epoch()?;
            if self.busy_count != busy {
                continue;
            }
            let value = sample(self)?;
            let after = self.epoch()?;
            if before.is_some() && before == after {
                return Ok(Idle::Sample(value));
            }
            invalid += 1;
            if invalid == 3 {
                return Err(BenchError::new("连续 3 次采样无效：采样前后的静止代数不同"));
            }
        }
    }

    /// `Epoch`: the kernel's quiescent epoch, None while busy (and for a
    /// program without a kernel).
    pub fn epoch(&mut self) -> Result<Option<u64>, BenchError> {
        self.send(&Request::Epoch)?;
        let limit = self.limits.reply;
        self.wait_for(
            "Epoch",
            Instant::now() + limit,
            limit,
            |report| match report {
                Report::Epoch(epoch) => Some(*epoch),
                _ => None,
            },
        )
    }

    /// `Plugins`, up to `PluginsEnd`, within [`Limits::reply`].
    pub fn plugins(&mut self) -> Result<Vec<PluginReport>, BenchError> {
        self.send(&Request::Plugins)?;
        let limit = self.limits.reply;
        let deadline = Instant::now() + limit;
        let mut plugins = Vec::new();
        loop {
            let report = self.wait_for("PluginsEnd", deadline, limit, |report| match report {
                Report::Plugin(_) | Report::PluginsEnd => Some(report.clone()),
                _ => None,
            })?;
            match report {
                Report::Plugin(plugin) => plugins.push(plugin),
                _ => return Ok(plugins),
            }
        }
    }

    /// Waits for `Ready` ([`Limits::again`]): every selected plugin is
    /// active. A `PluginFailed` before it is an error at once.
    pub fn ready(&mut self) -> Result<(), BenchError> {
        let limit = self.limits.again;
        self.wait_for("Ready", Instant::now() + limit, limit, |report| {
            (*report == Report::Ready).then_some(())
        })
    }

    /// `ActivateAll`, then [`ready`](Session::ready).
    pub fn activate_all(&mut self) -> Result<(), BenchError> {
        self.send(&Request::ActivateAll)?;
        self.ready()
    }

    /// `Open(target)`, then its `milestone` within `limit` (30 s or 1.5
    /// times the budget, whichever is longer). The sample: from just
    /// before the request (the runner's clock) to the milestone's
    /// `presented` (the same machine's clock).
    pub fn open(
        &mut self,
        target: OpenTarget,
        milestone: Milestone,
        limit: Duration,
    ) -> Result<Duration, BenchError> {
        let sent = SystemTime::now();
        self.send(&Request::Open(target))?;
        let deadline = Instant::now() + limit;
        let presented = self.wait_for("Milestone", deadline, limit, |report| match report {
            Report::Milestone {
                milestone: shown,
                presented,
            } if *shown == milestone => Some(*presented),
            _ => None,
        })?;
        presented
            .duration_since(sent)
            .map_err(|_| BenchError::new(format!("{milestone:?} 的显示时刻早于请求")))
    }

    /// `Open(target)` for a scroll, animation or input scenario, then its
    /// `Frames` within `limit` (the scenario's length plus 10 s).
    pub fn frames(
        &mut self,
        target: OpenTarget,
        limit: Duration,
    ) -> Result<FrameStats, BenchError> {
        self.send(&Request::Open(target))?;
        let deadline = Instant::now() + limit;
        self.wait_for("Frames", deadline, limit, |report| match report {
            Report::Frames(stats) => Some(stats.clone()),
            _ => None,
        })
    }

    /// `Quit`, then the exit within [`Limits::quit`]; an exit other than 0
    /// is an error. On an error the process group is ended.
    pub fn quit(mut self) -> Result<(), BenchError> {
        self.send(&Request::Quit)?;
        let deadline = Instant::now() + self.limits.quit;
        loop {
            if let Some(exit) = self.process.poll()? {
                return if exit.code == Some(0) {
                    Ok(())
                } else {
                    Err(BenchError::new(format!("Quit 之后{}", exit.describe())))
                };
            }
            if Instant::now() >= deadline {
                return Err(late("Quit 之后的退出", self.limits.quit));
            }
            // Reports after `Quit` still count: an `Error` is a failure.
            self.next_event(self.limits.poll)?;
        }
    }

    fn send(&mut self, request: &Request) -> Result<(), BenchError> {
        let line = format!("{}\n", request.encode());
        self.writer
            .write_all(line.as_bytes())
            .map_err(|e| BenchError::new(format!("发不出 {}：{e}", request.encode())))
    }

    /// Waits until the kernel is reported quiescent; false at `deadline`.
    fn quiescent(&mut self, deadline: Instant) -> Result<bool, BenchError> {
        while self.activity != Activity::Quiescent {
            if self.recv(deadline, "Quiescent")?.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// [`Limits::settle`] without `Busy`.
    fn calm(&mut self) -> Result<bool, BenchError> {
        let busy = self.busy_count;
        let deadline = Instant::now() + self.limits.settle;
        while self.recv(deadline, "静止后的等待")?.is_some() {
            if self.busy_count != busy {
                return Ok(false);
            }
        }
        Ok(self.busy_count == busy)
    }

    /// The first report `pick` accepts, before `deadline`; past it an
    /// error that names `what` and its `limit`.
    fn wait_for<T>(
        &mut self,
        what: &str,
        deadline: Instant,
        limit: Duration,
        mut pick: impl FnMut(&Report) -> Option<T>,
    ) -> Result<T, BenchError> {
        loop {
            match self.recv(deadline, what)? {
                Some(report) => {
                    if let Some(found) = pick(&report) {
                        return Ok(found);
                    }
                }
                None => return Err(late(what, limit)),
            }
        }
    }

    /// The next report before `deadline` (None at the deadline). Notes
    /// `Busy`, `Quiescent` and `Child`; an `Error`, a `PluginFailed`, a bad
    /// line or the exit of the process is an error.
    fn recv(&mut self, deadline: Instant, what: &str) -> Result<Option<Report>, BenchError> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            if let Some(report) = self.next_event((deadline - now).min(self.limits.poll))? {
                return Ok(Some(report));
            }
            if let Some(exit) = self.process.poll()? {
                return Err(self.exited(exit, what));
            }
        }
    }

    /// One event within `wait`: a report, or None.
    fn next_event(&mut self, wait: Duration) -> Result<Option<Report>, BenchError> {
        if self.closed {
            thread::sleep(wait);
            return Ok(None);
        }
        match self.events.recv_timeout(wait) {
            Ok(Event::Report(report)) => self.note(report),
            Ok(Event::Bad(message)) => Err(BenchError::new(format!("探针发来的行无效：{message}"))),
            Ok(Event::Closed) | Err(RecvTimeoutError::Disconnected) => {
                self.closed = true;
                Ok(None)
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
        }
    }

    fn note(&mut self, report: Report) -> Result<Option<Report>, BenchError> {
        match report {
            Report::Error { message } => Err(BenchError::new(format!("探针报告出错：{message}"))),
            Report::PluginFailed { id, message } => {
                Err(BenchError::new(format!("插件 {id} 进入 Failed：{message}")))
            }
            Report::Child { pid, kind, id } => {
                self.process.roots.push(pid);
                self.children.push(Spawned { pid, kind, id });
                Ok(None)
            }
            Report::Busy { .. } => {
                self.activity = Activity::Busy;
                self.busy_count += 1;
                Ok(Some(report))
            }
            Report::Quiescent { .. } => {
                self.activity = Activity::Quiescent;
                Ok(Some(report))
            }
            other => Ok(Some(other)),
        }
    }

    /// The process exited while the runner waited for `what`. Reports it
    /// wrote before exiting still arrive (the probe's `Error` before exit
    /// code 3); the first failure among them is the reason.
    fn exited(&mut self, exit: Exit, what: &str) -> BenchError {
        let end = Instant::now() + LAST_WORDS;
        while !self.closed && Instant::now() < end {
            if let Err(error) = self.next_event(end.saturating_duration_since(Instant::now())) {
                return error;
            }
        }
        BenchError::new(format!("等 {what} 时进程退出（{}）", exit.describe()))
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        // The process first: a probe that sees the socket close must not
        // get the chance to exit on its own and look like a clean exit.
        self.process.end();
        // Ends the reader thread.
        let _ = self.writer.shutdown(Shutdown::Both);
    }
}

fn late(what: &str, limit: Duration) -> BenchError {
    BenchError::new(format!("{limit:?} 内没有等到 {what}"))
}

/// The writing half and the events of the reading half, read on their own
/// thread.
fn connect(stream: UnixStream) -> Result<(UnixStream, Receiver<Event>), BenchError> {
    stream
        .set_nonblocking(false)
        .map_err(|e| BenchError::new(format!("socket 出错：{e}")))?;
    let reading = stream
        .try_clone()
        .map_err(|e| BenchError::new(format!("socket 出错：{e}")))?;
    let (sender, events) = mpsc::channel();
    thread::Builder::new()
        .name("bench-reports".into())
        .spawn(move || read_reports(reading, &sender))
        .map_err(|e| BenchError::new(format!("建不了读取线程：{e}")))?;
    Ok((stream, events))
}

fn read_reports(stream: UnixStream, sender: &Sender<Event>) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            break;
        };
        let event = match Report::decode(&line) {
            Ok(report) => Event::Report(report),
            Err(error) => Event::Bad(error.to_string()),
        };
        if sender.send(event).is_err() {
            return;
        }
    }
    let _ = sender.send(Event::Closed);
}
