//! The probe of the measurement builds (docs/design/bench.md「探针」): `app`
//! with `bench-probe`, and the `empty_window` example, call it to report the
//! first frame, the kernel's quiescence and the plugins to the runner, and
//! to carry out the runner's requests.
//!
//! Before the first frame it only reads two environment variables and
//! registers the first-frame callback (one method replacement in
//! `platform`): no IO, no thread, no socket. The callback starts the probe
//! thread, which connects to the runner's socket, sends `FirstFrame`, and
//! from then on writes every report and answers every request. Everything
//! the app and the `bench-probe` plugin hand to the probe goes through one
//! channel to that thread, so no caller writes to the socket (the main
//! thread does no IO) and the reports keep their order.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::SystemTime;

use lectern_kernel::{
    Activation, Active, Busy, Context, Kernel, KernelControl, Manifest, Plugin, PluginError,
    PluginHandle, PluginSet, PluginState, PluginStatus, PreparedSet, Quiescent, WorkspaceFacts,
};

use crate::wire::{
    ActivationKind, ChildKind, FrameStats, Milestone, OpenTarget, PluginReport, Report, Request,
};

#[cfg(test)]
mod tests;

/// The runner's socket. Without it the probe is off and the app runs as
/// usual.
pub const SOCKET_VAR: &str = "LECTERN_BENCH_SOCKET";
/// `closure:<id>` or `closure+self:<id>`: the plugin set is trimmed to the
/// dependency closure of `<id>`, without or with `<id>`.
pub const PLUGINS_VAR: &str = "LECTERN_BENCH_PLUGINS";
/// The ID of the probe's own plugin, left out of the `Plugins` answer.
pub const PLUGIN_ID: &str = "bench-probe";
/// The exit code after the first-frame callback could not be registered:
/// the runner records an error and never waits for its deadline (ADR 0048).
pub const HOOK_EXIT: i32 = 3;

/// Implemented by the program under test: carries out the runner's requests
/// the way the user's actions would. Called on the probe thread; the
/// implementation moves the work to the main thread and does not block.
pub trait Driver: Send + Sync {
    fn open(&self, target: OpenTarget);
    fn quit(&self);
}

/// The probe of one process. `Send + Sync`; hold it for the life of the
/// process.
pub struct Probe {
    shared: Arc<Shared>,
}

struct Shared {
    socket: PathBuf,
    /// `LECTERN_BENCH_PLUGINS`; an error when it is set but not valid.
    selection: Result<Option<Selection>, String>,
    sender: Sender<Msg>,
    /// Taken by the probe thread when it starts (once).
    receiver: Mutex<Option<Receiver<Msg>>>,
    /// Written by `start_kernel`.
    plan: Mutex<Option<Plan>>,
    /// The `bench-probe` plugin's control, set in its `started`: submits the
    /// facts of `ActivateAll`.
    control: Arc<OnceLock<KernelControl>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Selection {
    /// `closure:<id>`
    Closure(String),
    /// `closure+self:<id>`
    ClosureAndSelf(String),
}

/// What `start_kernel` learned from the manifests.
#[derive(Clone, Debug, Default)]
struct Plan {
    /// Every plugin started, except `bench-probe`, with its conditions, in
    /// set order.
    plugins: Vec<(String, Vec<Activation>)>,
    /// With a trimmed set, the plugins kept: they must all become `Active`
    /// before `Ready`.
    selected: Option<Vec<String>>,
}

/// What the probe thread receives.
enum Msg {
    /// A report to send as it is.
    Report(Report),
    /// A `Busy` or `Quiescent` from the `bench-probe` plugin: sent, then the
    /// awaited plugins are checked.
    Activity(Report),
    /// A request read from the socket.
    Request(Request),
    Attach {
        kernel: Option<Arc<Kernel>>,
        driver: Arc<dyn Driver>,
        awaited: Option<Vec<String>>,
    },
    /// The runner closed the connection.
    Closed,
}

impl Probe {
    /// Called at the start of `main`, before the first frame. Reads only
    /// `LECTERN_BENCH_SOCKET` and `LECTERN_BENCH_PLUGINS`; no IO, no thread.
    /// None when `LECTERN_BENCH_SOCKET` is not set (or empty): the app runs
    /// as usual.
    pub fn from_env() -> Option<Probe> {
        let socket = std::env::var_os(SOCKET_VAR).filter(|s| !s.is_empty())?;
        let plugins = std::env::var_os(PLUGINS_VAR);
        Some(Probe::with_vars(PathBuf::from(socket), plugins.as_deref()))
    }

    /// The probe for these variable values (`from_env`, and the tests).
    pub(crate) fn with_vars(socket: PathBuf, plugins: Option<&OsStr>) -> Probe {
        let (sender, receiver) = mpsc::channel();
        Probe {
            shared: Arc::new(Shared {
                socket,
                selection: parse_selection(plugins),
                sender,
                receiver: Mutex::new(Some(receiver)),
                plan: Mutex::new(None),
                control: Arc::new(OnceLock::new()),
            }),
        }
    }

    /// Called before the first frame, before the window is created:
    /// registers the first-frame callback with
    /// `platform::frame::on_first_presented`. The callback only notes the
    /// time and starts the probe thread. If the registration fails
    /// (`HookError`, such as after a GPUI upgrade changed the method's type
    /// encoding), connects to the socket at once, sends
    /// `Error { message: "hook <kind>" }` and exits with code 3: the runner
    /// must neither wait for its deadline nor measure a wrong number
    /// (ADR 0048).
    pub fn watch_first_frame(&self) {
        let shared = Arc::clone(&self.shared);
        let registered = lectern_platform::frame::on_first_presented(move |presented| {
            start_thread(shared, presented);
        });
        if let Err(error) = registered {
            fail(&self.shared.socket, &format!("hook {}", error.as_str()));
        }
    }

    /// The first frame was shown at `presented`: starts the probe thread.
    /// What the first-frame callback does; the tests call it directly.
    #[cfg(test)]
    pub(crate) fn first_frame(&self, presented: SystemTime) {
        start_thread(Arc::clone(&self.shared), presented);
    }

    /// After the first frame, in place of `Kernel::start`: adds the
    /// `bench-probe` plugin and starts the kernel. Every plugin's
    /// `manifest()` is called once, here (`PluginSet::prepare`), and
    /// `Kernel::start_prepared` uses those manifests and times, so the
    /// manifest part of `plugin/*/activate` is that one timed call. With
    /// `LECTERN_BENCH_PLUGINS` the set is trimmed first: `closure:<id>`
    /// keeps the dependency closure of `<id>`, `closure+self:<id>` keeps it
    /// and `<id>`. A trim that cannot be done is reported as an `Error` and
    /// the whole set starts.
    pub fn start_kernel(&self, set: PluginSet) -> Arc<Kernel> {
        let prepared = set.prepare();
        let (prepared, selected) = match &self.shared.selection {
            Ok(None) => (prepared, None),
            Ok(Some(selection)) => match trim(prepared, selection) {
                Ok((prepared, kept)) => (prepared, Some(kept)),
                Err((prepared, message)) => {
                    self.send(Report::Error { message });
                    (prepared, None)
                }
            },
            Err(message) => {
                self.send(Report::Error {
                    message: message.clone(),
                });
                (prepared, None)
            }
        };
        let plan = Plan {
            plugins: prepared
                .manifests()
                .map(|(id, manifest)| (id.to_string(), manifest.activation.clone()))
                .collect(),
            selected,
        };
        let plugin = ProbePlugin {
            sender: self.shared.sender.clone(),
            control: Arc::clone(&self.shared.control),
            facts: plan.selected.is_some(),
        };
        *self
            .shared
            .plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(plan);
        Kernel::start_prepared(prepared.with(PLUGIN_ID, plugin))
    }

    /// After `start_kernel`. With a trimmed set, meets the activation
    /// condition of every kept plugin that has no `AfterFirstFrame`:
    /// `Command` through one `Kernel::command`, `Manual` through
    /// `Kernel::activate`, `Workspace` through the facts the `bench-probe`
    /// plugin submits in its `started` (they match every pattern). Once all
    /// kept plugins are `Active` the probe sends `Ready`; a kept plugin that
    /// enters `Failed`, before or after, is reported at once
    /// (`PluginFailed`). Requests that came before this call are answered
    /// now.
    pub fn attach(&self, kernel: Arc<Kernel>, driver: Arc<dyn Driver>) {
        let plan = self
            .shared
            .plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_default();
        // `Workspace` plugins of a trimmed set get the facts the
        // `bench-probe` plugin submits in its `started`.
        if let Some(selected) = &plan.selected {
            for (id, conditions) in &plan.plugins {
                if selected.contains(id) {
                    // The facts come from `started` (above), not from here.
                    let _ = meet(&kernel, id, conditions);
                }
            }
        }
        let _ = self.shared.sender.send(Msg::Attach {
            kernel: Some(kernel),
            driver,
            awaited: plan.selected,
        });
    }

    /// For a program without a kernel (`empty_window`), in place of
    /// `attach`: only the driver. `Epoch` is answered with None, `Plugins`
    /// with `PluginsEnd` alone, and `ActivateAll` with `Ready`.
    pub fn drive(&self, driver: Arc<dyn Driver>) {
        let _ = self.shared.sender.send(Msg::Attach {
            kernel: None,
            driver,
            awaited: None,
        });
    }

    /// A milestone of `workspace` was shown at `presented`. Any thread.
    pub fn milestone(&self, milestone: Milestone, presented: SystemTime) {
        self.send(Report::Milestone {
            milestone,
            presented,
        });
    }

    /// The frame times of one scenario, from a clean recording
    /// (`FrameProblems::is_clean`); otherwise the app calls `error` with
    /// `frames <kind>` or `frames unmatched=<n> incomplete=<n>
    /// overflowed=<n>`. Any thread.
    pub fn frames(&self, stats: FrameStats) {
        self.send(Report::Frames(stats));
    }

    /// Sends `Error { message }`; the runner records the benchmark as
    /// `error`. Any thread.
    pub fn error(&self, message: &str) {
        self.send(Report::Error {
            message: message.to_owned(),
        });
    }

    /// A process plugin's host or a language server started the process
    /// group whose root is `pid`. Any thread.
    pub fn child(&self, pid: u32, kind: ChildKind, id: &str) {
        self.send(Report::Child {
            pid,
            kind,
            id: id.to_owned(),
        });
    }

    fn send(&self, report: Report) {
        // The probe thread may never start (no first frame); then nothing
        // is sent, as nothing could be.
        let _ = self.shared.sender.send(Msg::Report(report));
    }
}

fn parse_selection(value: Option<&OsStr>) -> Result<Option<Selection>, String> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let bad = || format!("plugins {PLUGINS_VAR} must be closure:<id> or closure+self:<id>");
    let value = value.to_str().ok_or_else(bad)?;
    let selection = if let Some(id) = value.strip_prefix("closure+self:") {
        Selection::ClosureAndSelf(id.to_owned())
    } else if let Some(id) = value.strip_prefix("closure:") {
        Selection::Closure(id.to_owned())
    } else {
        return Err(bad());
    };
    match &selection {
        Selection::Closure(id) | Selection::ClosureAndSelf(id) if id.is_empty() => Err(bad()),
        _ => Ok(Some(selection)),
    }
}

/// Keeps the dependency closure of the selected plugin (and the plugin,
/// with `closure+self`). Returns the kept IDs in set order, or the set
/// unchanged and the message of the error.
fn trim(
    prepared: PreparedSet,
    selection: &Selection,
) -> Result<(PreparedSet, Vec<String>), (PreparedSet, String)> {
    let (id, with_self) = match selection {
        Selection::Closure(id) => (id.as_str(), false),
        Selection::ClosureAndSelf(id) => (id.as_str(), true),
    };
    let closure = match prepared.dependency_closure(id) {
        Ok(closure) => closure,
        Err(errors) => {
            let reasons: Vec<String> = errors.iter().map(ToString::to_string).collect();
            return Err((
                prepared,
                format!("plugins closure of {id}: {}", reasons.join("; ")),
            ));
        }
    };
    let keep = |name: &str| (with_self && name == id) || closure.iter().any(|c| c == name);
    let prepared = prepared.retain(keep);
    let kept = prepared
        .manifests()
        .map(|(name, _)| name.to_string())
        .collect();
    Ok((prepared, kept))
}

/// Meets one activation condition of plugin `id` through the kernel:
/// `Command` with one `Kernel::command`, else `Manual` with
/// `Kernel::activate`. Returns true when the plugin waits for facts instead
/// (`Workspace`), which the caller submits once for all plugins. Nothing for
/// a plugin that activates after the first frame on its own.
///
/// The order of the calls does not matter: a met condition stays met for the
/// run, and a plugin whose dependency is not ready yet activates once it is
/// (kernel「激活」). A command whose provider chain is not activatable yet
/// answers `DependencyUnavailable` to its callback, which the probe ignores.
fn meet(kernel: &Kernel, id: &str, conditions: &[Activation]) -> bool {
    if conditions.contains(&Activation::AfterFirstFrame) {
        return false;
    }
    if conditions
        .iter()
        .any(|c| matches!(c, Activation::Workspace(_)))
    {
        return true;
    }
    if let Some(name) = conditions.iter().find_map(|c| match c {
        Activation::Command(name) => Some(name),
        _ => None,
    }) {
        kernel.command(name, |_| {});
    } else if conditions.contains(&Activation::Manual) {
        kernel.activate(id);
    }
    false
}

/// Facts that contain every pattern, so that every `Workspace` plugin
/// activates.
struct EveryPattern;

impl WorkspaceFacts for EveryPattern {
    fn contains(&self, _pattern: &str) -> bool {
        true
    }
}

/// The `bench-probe` plugin (`AfterFirstFrame`): forwards `Busy` and
/// `Quiescent` and nothing else in its listeners, dropping events whose
/// `seq` is not greater than the largest seen (kernel「静止」), so it never
/// makes the kernel busy again. With a trimmed set it submits facts that
/// match every pattern in `started`.
struct ProbePlugin {
    sender: Sender<Msg>,
    control: Arc<OnceLock<KernelControl>>,
    facts: bool,
}

impl Plugin for ProbePlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            activation: vec![Activation::AfterFirstFrame],
            ..Manifest::default()
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        // The largest `seq` seen plus one (0: none yet), shared by both
        // events: they share the kernel's counter.
        let newest = Arc::new(AtomicU64::new(0));
        let (sender, seen) = (self.sender.clone(), Arc::clone(&newest));
        ctx.on::<Busy>(move |event, _| {
            forward(&seen, &sender, event.seq, Report::Busy { epoch: event.seq });
        });
        let (sender, seen) = (self.sender.clone(), newest);
        ctx.on::<Quiescent>(move |event, _| {
            forward(
                &seen,
                &sender,
                event.seq,
                Report::Quiescent { epoch: event.seq },
            );
        });
        Ok(Box::new(ProbeActive {
            control: Arc::clone(&self.control),
            facts: self.facts,
        }))
    }
}

fn forward(newest: &AtomicU64, sender: &Sender<Msg>, seq: u64, report: Report) {
    let mark = seq.saturating_add(1);
    if newest.fetch_max(mark, Ordering::SeqCst) < mark {
        let _ = sender.send(Msg::Activity(report));
    }
}

struct ProbeActive {
    control: Arc<OnceLock<KernelControl>>,
    facts: bool,
}

impl Active for ProbeActive {
    fn started(&mut self, handle: PluginHandle) {
        let control = handle.control();
        if self.facts {
            control.workspace_changed(Arc::new(EveryPattern));
        }
        let _ = self.control.set(control);
    }
}

/// Starts the probe thread (once). Runs in the first-frame callback, on a
/// Core Animation thread: it must not block or panic, so a thread that
/// cannot start leaves the runner to its deadline.
fn start_thread(shared: Arc<Shared>, presented: SystemTime) {
    let _ = thread::Builder::new()
        .name("bench-probe".into())
        .spawn(move || serve(&shared, presented));
}

/// Registration failed: one `Error` on a new connection, then exit 3.
fn fail(socket: &std::path::Path, message: &str) -> ! {
    if let Ok(mut stream) = UnixStream::connect(socket) {
        let line = Report::Error {
            message: message.to_owned(),
        }
        .encode();
        let _ = stream.write_all(format!("{line}\n").as_bytes());
    }
    std::process::exit(HOOK_EXIT);
}

/// The probe thread: connects, sends `FirstFrame`, then sends every report
/// and answers every request until the runner closes the connection.
fn serve(shared: &Shared, presented: SystemTime) {
    let Some(receiver) = shared
        .receiver
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    else {
        return;
    };
    let Ok(stream) = UnixStream::connect(&shared.socket) else {
        return;
    };
    let Ok(reading) = stream.try_clone() else {
        return;
    };
    let sender = shared.sender.clone();
    let reader = thread::Builder::new()
        .name("bench-probe-read".into())
        .spawn(move || read_requests(reading, &sender));
    let mut state = Serving {
        out: stream,
        kernel: None,
        driver: None,
        pending: Vec::new(),
        awaited: None,
        ready: false,
        failed: HashSet::new(),
        plan: Plan::default(),
        control: Arc::clone(&shared.control),
    };
    let first = Report::FirstFrame {
        pid: std::process::id(),
        presented,
    };
    if state.write(&first).is_err() || reader.is_err() {
        return;
    }
    for msg in receiver {
        let result = match msg {
            Msg::Report(report) => state.write(&report),
            Msg::Activity(report) => state.write(&report).and_then(|()| state.check()),
            Msg::Request(request) => state.request(request),
            Msg::Attach {
                kernel,
                driver,
                awaited,
            } => {
                state.plan = shared
                    .plan
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
                    .unwrap_or_default();
                state.attach(kernel, driver, awaited)
            }
            Msg::Closed => return,
        };
        if result.is_err() {
            return;
        }
    }
}

fn read_requests(stream: UnixStream, sender: &Sender<Msg>) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            break;
        };
        let msg = match Request::decode(&line) {
            Ok(request) => Msg::Request(request),
            Err(error) => Msg::Report(Report::Error {
                message: format!("wire {error}"),
            }),
        };
        if sender.send(msg).is_err() {
            return;
        }
    }
    let _ = sender.send(Msg::Closed);
}

/// The probe thread's state.
struct Serving {
    out: UnixStream,
    kernel: Option<Arc<Kernel>>,
    driver: Option<Arc<dyn Driver>>,
    /// Requests that came before `attach`/`drive`.
    pending: Vec<Request>,
    /// The plugins `Ready` waits for.
    awaited: Option<Vec<String>>,
    ready: bool,
    /// Plugins already reported as `PluginFailed`.
    failed: HashSet<String>,
    plan: Plan,
    control: Arc<OnceLock<KernelControl>>,
}

impl Serving {
    fn write(&mut self, report: &Report) -> io::Result<()> {
        let line = report.encode();
        self.out.write_all(format!("{line}\n").as_bytes())
    }

    fn attach(
        &mut self,
        kernel: Option<Arc<Kernel>>,
        driver: Arc<dyn Driver>,
        awaited: Option<Vec<String>>,
    ) -> io::Result<()> {
        self.kernel = kernel;
        self.driver = Some(driver);
        self.awaited = awaited;
        self.ready = false;
        self.check()?;
        for request in std::mem::take(&mut self.pending) {
            self.request(request)?;
        }
        Ok(())
    }

    fn request(&mut self, request: Request) -> io::Result<()> {
        let Some(driver) = self.driver.clone() else {
            self.pending.push(request);
            return Ok(());
        };
        match request {
            Request::Epoch => {
                let epoch = self.kernel.as_ref().and_then(|k| k.quiescent_epoch());
                self.write(&Report::Epoch(epoch))
            }
            Request::Plugins => {
                let statuses = self.kernel.as_ref().map(|k| k.status()).unwrap_or_default();
                for status in statuses.iter().filter(|s| s.id != PLUGIN_ID) {
                    let report = Report::Plugin(self.plugin_report(status));
                    self.write(&report)?;
                }
                self.write(&Report::PluginsEnd)
            }
            Request::Open(target) => {
                driver.open(target);
                Ok(())
            }
            Request::Quit => {
                driver.quit();
                Ok(())
            }
            Request::ActivateAll => self.activate_all(),
        }
    }

    /// Meets one condition of every plugin of the profile that is not
    /// `Active` yet, then waits for all of them.
    fn activate_all(&mut self) -> io::Result<()> {
        let Some(kernel) = self.kernel.clone() else {
            return self.write(&Report::Ready);
        };
        let Some(control) = self.control.get() else {
            return self.write(&Report::Error {
                message: format!("activate-all before {PLUGIN_ID} started"),
            });
        };
        let statuses = kernel.status();
        let mut facts = false;
        for (id, conditions) in &self.plan.plugins {
            let active = statuses
                .iter()
                .any(|s| s.id == id.as_str() && s.state == PluginState::Active);
            if !active {
                facts |= meet(&kernel, id, conditions);
            }
        }
        if facts {
            control.workspace_changed(Arc::new(EveryPattern));
        }
        // Every plugin of the profile, also one whose `manifest()` panicked
        // (it is `Failed`, so the runner hears `PluginFailed`).
        let mut awaited: Vec<String> = self.plan.plugins.iter().map(|(id, _)| id.clone()).collect();
        for status in &statuses {
            if status.id != PLUGIN_ID && !awaited.iter().any(|id| *id == status.id) {
                awaited.push(status.id.to_string());
            }
        }
        self.awaited = Some(awaited);
        self.ready = false;
        self.check()
    }

    /// Sends `PluginFailed` for every awaited plugin that newly failed, and
    /// `Ready` once all of them are `Active`.
    fn check(&mut self) -> io::Result<()> {
        let (Some(kernel), Some(awaited)) = (&self.kernel, &self.awaited) else {
            return Ok(());
        };
        let statuses = kernel.status();
        let mut all_active = true;
        let mut failures = Vec::new();
        for id in awaited {
            let state = statuses
                .iter()
                .find(|s| s.id == id.as_str())
                .map(|s| &s.state);
            match state {
                Some(PluginState::Active) => {}
                Some(PluginState::Failed(error)) => {
                    all_active = false;
                    if !self.failed.contains(id) {
                        failures.push((id.clone(), error.to_string()));
                    }
                }
                _ => all_active = false,
            }
        }
        for (id, message) in failures {
            self.failed.insert(id.clone());
            self.write(&Report::PluginFailed { id, message })?;
        }
        if all_active && !self.ready {
            self.ready = true;
            self.write(&Report::Ready)?;
        }
        Ok(())
    }

    fn plugin_report(&self, status: &PluginStatus) -> PluginReport {
        let activation = self
            .plan
            .plugins
            .iter()
            .find(|(id, _)| *id == status.id)
            .map(|(_, conditions)| kinds(conditions))
            .unwrap_or_default();
        PluginReport {
            id: status.id.to_string(),
            state: state_name(&status.state).to_owned(),
            activation,
            manifest: status.manifest_time,
            activate: status.activate_time,
            commit: status.commit_time,
            started: status.started_time,
        }
    }
}

fn kinds(conditions: &[Activation]) -> Vec<ActivationKind> {
    conditions
        .iter()
        .map(|c| match c {
            Activation::AfterFirstFrame => ActivationKind::AfterFirstFrame,
            Activation::Workspace(_) => ActivationKind::Workspace,
            Activation::Command(_) => ActivationKind::Command,
            Activation::Manual => ActivationKind::Manual,
        })
        .collect()
}

fn state_name(state: &PluginState) -> &'static str {
    match state {
        PluginState::Inactive => "Inactive",
        PluginState::Waiting { .. } => "Waiting",
        PluginState::Active => "Active",
        PluginState::Disabled => "Disabled",
        PluginState::Failed(_) => "Failed",
    }
}
