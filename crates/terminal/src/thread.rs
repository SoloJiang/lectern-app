//! One terminal's two threads (docs/design/terminal.md「数据流与线程」).
//!
//! `terminal-<id>` owns the libghostty-vt engine and the PTY's primary end
//! and loops: wait for the PTY, the wake socket or a deadline; take the
//! commands; read the output into the engine; write the encoded input;
//! publish a frame when one is due. `terminal-<id>-wait` blocks in
//! `Child::wait` and reports the exit. Both bodies run under `catch`, so a
//! panic fails this terminal only.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use lectern_kernel::{PluginHandle, catch};
use lectern_logging::{Value, debug, error, info, trace, warn};
use lectern_services::terminal::{
    FailReason, GridSize, Input, RunState, TerminalEvent, TerminalId, Theme,
};
use libghostty_vt::terminal::CompressionActivity;
use portable_pty::Child;
use rustix::event::PollFlags;

use crate::diag::{self, CloseReason, session_ctx};
use crate::engine::Engine;
use crate::launch::{self, Prepared};
use crate::pty::{self, Pty, Read, Write};
use crate::slot::{Command, ExitInfo, Slot};
use crate::table::Shared;

/// One `read` of the primary end.
const READ_CHUNK: usize = 64 << 10;
/// The output parsed in one round before the loop looks at frames and
/// commands again.
const READ_PER_ROUND: usize = 1 << 20;
/// Encoded input waiting for a program that does not read it.
const MAX_PENDING_WRITE: usize = 4 << 20;
/// Output stops this long before the scrollback is compressed.
const IDLE_COMPRESS: Duration = Duration::from_secs(2);
const TITLE_INTERVAL: Duration = Duration::from_millis(250);
const BELL_INTERVAL: Duration = Duration::from_millis(500);
const STATS_INTERVAL: Duration = Duration::from_secs(1);
/// After `SIGHUP`, before `SIGKILL` to the process group.
const HANGUP_GRACE: Duration = Duration::from_secs(2);
/// After `SIGKILL`.
const KILL_GRACE: Duration = Duration::from_secs(1);

/// Counts a running thread in `Config::threads`.
pub(crate) struct Counted(Arc<AtomicUsize>);

impl Counted {
    pub(crate) fn enter(count: &Arc<AtomicUsize>) -> Counted {
        count.fetch_add(1, Ordering::SeqCst);
        Counted(count.clone())
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What a terminal's thread starts with.
pub(crate) struct Job {
    pub(crate) shared: Arc<Shared>,
    pub(crate) slot: Arc<Slot>,
    pub(crate) handle: PluginHandle,
    pub(crate) prepared: Prepared,
    pub(crate) size: GridSize,
    pub(crate) theme: Theme,
    pub(crate) children: Sender<Box<dyn Child + Send + Sync>>,
}

/// A terminal as the code that fails it and reports its events sees it.
#[derive(Clone)]
struct Port {
    shared: Arc<Shared>,
    slot: Arc<Slot>,
    handle: PluginHandle,
    id: TerminalId,
}

impl Port {
    /// `Listed` with the table's generation of this moment.
    fn listed(&self) {
        let generation = self.shared.generation();
        self.handle
            .emitter()
            .emit(TerminalEvent::Listed { generation });
    }
}

/// The body of `terminal-<id>`.
pub(crate) fn run(job: Job) {
    let _count = Counted::enter(&job.shared.config.threads);
    let port = Port {
        shared: job.shared.clone(),
        slot: job.slot.clone(),
        handle: job.handle.clone(),
        id: job.slot.id,
    };
    let _scope = match &job.prepared.kind {
        lectern_services::terminal::LaunchKind::Resume(key) => {
            Some(lectern_logging::scope(session_ctx(&key.id)))
        }
        _ => None,
    };
    let outcome = catch(|| match Terminal::new(job) {
        Ok(terminal) => terminal.run(),
        Err(()) => Outcome::Internal,
    });
    match outcome {
        Ok(Outcome::Finished) => {}
        Ok(Outcome::Internal) => {
            warn!("terminal start failed"; term = port.id.0, reason = diag::fail_reason(FailReason::Internal));
            retire(&port);
        }
        Err(payload) => {
            // The engine and the PTY were dropped while unwinding, before
            // the payload goes: its message is not logged.
            drop(payload.dispose());
            error!("terminal thread panicked"; term = port.id.0, thread = Value::text("engine"));
            retire(&port);
        }
    }
}

enum Outcome {
    Finished,
    /// The engine could not be created, or a frame could not be built.
    Internal,
}

/// After an internal failure: the terminal shows its last frame as
/// `Failed(Internal)`, the process is ended the way `close` ends it, and the
/// thread waits for `close`.
fn retire(port: &Port) {
    let failed = RunState::Failed(FailReason::Internal);
    if let Some(frame) = port.shared.frame(port.id) {
        let mut frame = (*frame).clone();
        frame.state = failed;
        frame.generation += 1;
        port.shared.set_frame(port.id, Arc::new(frame));
    }
    port.shared.set_state(port.id, failed);
    port.listed();
    let mut killed = end_process(&port.slot, None);
    loop {
        for command in port.slot.take() {
            match command {
                Command::Close(reason) => {
                    port.listed();
                    // The process may have outlived an earlier wait.
                    killed |= end_process(&port.slot, None);
                    info!("terminal closed"; term = port.id.0, reason = reason.value(), killed = killed);
                    port.slot.drain();
                    return;
                }
                Command::Copy(request) => request.answer(None),
                _ => {}
            }
        }
        port.slot.wait(None, None);
    }
}

/// Ends the process like `close` does: `SIGHUP` and the primary end closed;
/// `SIGKILL` to the process group when it has not exited after 2 seconds.
/// Returns whether `SIGKILL` was used.
fn end_process(slot: &Slot, pty: Option<&mut Pty>) -> bool {
    let Some(pid) = slot.pid() else {
        return false;
    };
    if slot.exit().is_some() {
        return false;
    }
    if let Some(pty) = pty {
        pty.hang_up();
        pty.close();
    }
    if slot.wait_exit(HANGUP_GRACE) {
        return false;
    }
    pty::kill_group(pid);
    slot.wait_exit(KILL_GRACE);
    true
}

/// The body of `terminal-<id>-wait`: waits for the child that the terminal
/// thread sends and reports how it ended.
pub(crate) fn wait(
    slot: Arc<Slot>,
    children: Receiver<Box<dyn Child + Send + Sync>>,
    threads: Arc<AtomicUsize>,
) {
    let _count = Counted::enter(&threads);
    let id = slot.id;
    let outcome = catch(|| {
        // The sender is dropped when the process did not start.
        let Ok(mut child) = children.recv() else {
            return;
        };
        let info = match child.wait() {
            Ok(status) => {
                let signaled = status.signal().is_some();
                ExitInfo {
                    code: if signaled { 0 } else { status.exit_code() },
                    signaled,
                }
            }
            Err(_) => ExitInfo {
                code: 1,
                signaled: false,
            },
        };
        slot.set_exit(info);
    });
    if let Err(payload) = outcome {
        drop(payload.dispose());
        error!("terminal thread panicked"; term = id.0, thread = Value::text("wait"));
    }
}

/// The libghostty-vt state and everything the thread loop keeps.
struct Terminal {
    port: Port,
    prepared: Prepared,
    engine: Engine,
    size: GridSize,
    state: RunState,
    pty: Option<Pty>,
    children: Option<Sender<Box<dyn Child + Send + Sync>>>,
    pid: Option<u32>,
    started_at: Option<Instant>,
    /// The primary end may still have output.
    reading: bool,
    /// Writing to the primary end can still work.
    writable: bool,
    read_buf: Vec<u8>,
    pending: Vec<u8>,
    pending_from: usize,
    closing: Option<CloseReason>,
    dropped_inputs: u32,
    bells: u32,
    bell_at: Option<Instant>,
    title: Option<Arc<str>>,
    title_dirty: bool,
    title_at: Option<Instant>,
    idle_due: Option<Instant>,
    idle_done: Option<CompressionActivity>,
    idle_steps: u32,
    stats: Stats,
    engine_error: bool,
}

#[derive(Default)]
struct Stats {
    since: Option<Instant>,
    read_bytes: u64,
    frames: u32,
    max_frame_us: u64,
    rows_before: u64,
}

impl Terminal {
    fn new(job: Job) -> Result<Terminal, ()> {
        let engine = Engine::new(job.size, job.theme).map_err(|_| ())?;
        let id = job.slot.id;
        let port = Port {
            shared: job.shared,
            slot: job.slot,
            handle: job.handle,
            id,
        };
        Ok(Terminal {
            port,
            prepared: job.prepared,
            engine,
            size: job.size,
            state: RunState::Starting,
            pty: None,
            children: Some(job.children),
            pid: None,
            started_at: None,
            reading: false,
            writable: false,
            read_buf: vec![0; READ_CHUNK],
            pending: Vec::new(),
            pending_from: 0,
            closing: None,
            dropped_inputs: 0,
            bells: 0,
            bell_at: None,
            title: None,
            title_dirty: false,
            title_at: None,
            idle_due: None,
            idle_done: None,
            idle_steps: 0,
            stats: Stats::default(),
            engine_error: false,
        })
    }

    fn run(mut self) -> Outcome {
        self.start();
        while self.closing.is_none() {
            if self.round().is_err() {
                return Outcome::Internal;
            }
        }
        self.finish();
        Outcome::Finished
    }

    // ---------------------------------------------------------------- start

    fn start(&mut self) {
        // The login environment can take seconds: the round counts as work.
        let _busy = self.port.handle.busy();
        let now = Instant::now();
        self.publish(now, false);
        self.port.listed();
        if self.port.slot.close_queued() {
            return;
        }
        let resolved = match launch::resolve(&self.port.shared, self.port.id, &self.prepared) {
            Ok(resolved) => resolved,
            Err(reason) => return self.fail(reason, None, now),
        };
        let cwd = self.prepared.cwd.clone();
        let spawned = match pty::spawn(&resolved.program, &resolved.env, &cwd, self.size) {
            Ok(spawned) => spawned,
            Err(error) => {
                return self.fail(FailReason::Spawn { errno: error.errno }, error.errno, now);
            }
        };
        let pid = spawned.pty.pid();
        self.port.slot.set_pid(pid);
        self.pid = Some(pid);
        self.started_at = Some(now);
        self.pty = Some(spawned.pty);
        self.reading = true;
        self.writable = true;
        if let Some(children) = self.children.take() {
            // The wait thread is gone only when the activation died.
            let _ = children.send(spawned.child);
        }
        info!(
            "terminal started";
            term = self.port.id.0,
            launch = diag::launch(&self.prepared.kind),
            program = diag::program_name(&resolved.program_name),
            argc = resolved.argc,
            cwd = Value::path(&self.prepared.cwd),
            pid = pid,
            cols = self.size.cols,
            rows = self.size.rows,
            login_env = resolved.login_env.value(),
        );
        self.set_state(RunState::Running { pid }, now);
    }

    fn fail(&mut self, reason: FailReason, errno: Option<i32>, now: Instant) {
        match errno {
            Some(code) => {
                let error = io::Error::from_raw_os_error(code);
                warn!("terminal start failed"; term = self.port.id.0, reason = diag::fail_reason(reason), error = Value::io(&error));
            }
            None => {
                warn!("terminal start failed"; term = self.port.id.0, reason = diag::fail_reason(reason));
            }
        }
        self.children = None;
        self.set_state(RunState::Failed(reason), now);
    }

    fn set_state(&mut self, state: RunState, now: Instant) {
        self.state = state;
        self.engine.set_state(state, now);
        self.port.shared.set_state(self.port.id, state);
        self.publish(now, true);
        self.port.listed();
    }

    // ----------------------------------------------------------------- loop

    fn round(&mut self) -> Result<(), ()> {
        if let Some(hook) = &self.port.shared.config.thread_hook {
            hook(self.port.id);
        }
        let timeout = self.deadline(Instant::now());
        self.wait(timeout);
        // From here to the end of the round the kernel counts this terminal
        // as working (kernel「静止」); the view painting the frame does not.
        let _busy = self.port.handle.busy();
        let now = Instant::now();
        self.commands(now);
        if self.closing.is_some() {
            return Ok(());
        }
        self.pump_read(now, READ_PER_ROUND);
        self.pump_write();
        self.frames(now)?;
        self.timers(now);
        Ok(())
    }

    fn wait(&self, timeout: Option<Duration>) {
        let master = self.pty.as_ref().and_then(|pty| {
            let mut flags = PollFlags::empty();
            if self.reading {
                flags |= PollFlags::IN;
            }
            if self.writable && self.pending_from < self.pending.len() {
                flags |= PollFlags::OUT;
            }
            if flags.is_empty() {
                return None;
            }
            pty.fd().map(|fd| (fd, flags))
        });
        self.port.slot.wait(master, timeout);
    }

    /// The nearest deadline, None when only a command or the PTY can wake
    /// the thread.
    fn deadline(&self, now: Instant) -> Option<Duration> {
        let mut next: Option<Instant> = None;
        let mut at = |t: Instant| next = Some(next.map_or(t, |n| n.min(t)));
        if self.port.slot.watchers() > 0 {
            match self.engine.frame_deadline() {
                Some(Some(t)) => at(t),
                Some(None) => at(now),
                None => {}
            }
        }
        if let Some(t) = self.idle_due {
            at(t);
        }
        if self.title_dirty {
            at(self.title_at.map_or(now, |t| t + TITLE_INTERVAL));
        }
        if let Some(since) = self.stats.since {
            at(since + STATS_INTERVAL);
        }
        next.map(|t| t.saturating_duration_since(now))
    }

    // ------------------------------------------------------------- commands

    fn commands(&mut self, now: Instant) {
        let mut batch = self.port.slot.take();
        while let Some(command) = batch.pop_front() {
            match command {
                Command::Input(Input::Resize(_))
                    if matches!(batch.front(), Some(Command::Input(Input::Resize(_)))) =>
                {
                    // Only the last of consecutive resizes counts.
                }
                Command::Input(input) => self.input(input, now),
                Command::Watch => self.publish(now, true),
                Command::Copy(request) => request.answer(self.engine.copy_selection()),
                Command::Exited => self.exited(now),
                Command::Close(reason) => {
                    self.closing = Some(reason);
                    for rest in batch.drain(..) {
                        if let Command::Copy(request) = rest {
                            request.answer(None);
                        }
                    }
                }
            }
        }
    }

    fn input(&mut self, input: Input, now: Instant) {
        let resize = match &input {
            Input::Resize(size) => Some(*size),
            _ => None,
        };
        let kind = diag::input_kind(&input);
        if self.engine.input(input, now).is_err() {
            self.engine_failed();
            return;
        }
        let encoded = self.engine.take_pty_output();
        trace!("terminal input"; term = self.port.id.0, kind = kind, bytes = encoded.len());
        self.queue_write(&encoded, true);
        self.note_activity(now);
        if let Some(size) = resize {
            self.size = size;
            if matches!(self.state, RunState::Running { .. })
                && let Some(pty) = &self.pty
            {
                pty.resize(size);
            }
            debug!("terminal resized"; term = self.port.id.0, cols = size.cols, rows = size.rows);
        }
    }

    fn exited(&mut self, now: Instant) {
        let Some(info) = self.port.slot.exit() else {
            return;
        };
        if !matches!(self.state, RunState::Running { .. }) {
            return;
        }
        // The last output may still be in the PTY's buffer.
        self.pump_read(now, 4 * READ_PER_ROUND);
        self.pty = None;
        self.reading = false;
        self.writable = false;
        self.pending.clear();
        self.pending_from = 0;
        let ran_ms = self
            .started_at
            .map_or(0, |at| now.saturating_duration_since(at).as_millis());
        info!(
            "terminal exited";
            term = self.port.id.0,
            pid = self.pid.unwrap_or(0),
            code = info.code,
            signaled = info.signaled,
            ran_ms = u64::try_from(ran_ms).unwrap_or(u64::MAX),
        );
        self.set_state(
            RunState::Exited {
                code: info.code,
                signaled: info.signaled,
            },
            now,
        );
    }

    // ------------------------------------------------------------------ I/O

    fn pump_read(&mut self, now: Instant, limit: usize) {
        if !self.reading {
            return;
        }
        let mut total = 0;
        while total < limit {
            let Some(pty) = &self.pty else { break };
            match pty.read(&mut self.read_buf) {
                Read::Data(n) => {
                    self.engine.write_output(&self.read_buf[..n], now);
                    total += n;
                }
                Read::Again => break,
                Read::Eof => {
                    self.reading = false;
                    self.writable = false;
                    self.pending.clear();
                    self.pending_from = 0;
                    break;
                }
                Read::Failed(error) => {
                    warn!("pty read failed"; term = self.port.id.0, error = Value::io(&error));
                    self.reading = false;
                    break;
                }
            }
        }
        if total == 0 {
            return;
        }
        self.stats.read_bytes += total as u64;
        self.note_activity(now);
        let effects = self.engine.take_effects();
        let replies = self.engine.take_pty_output();
        self.queue_write(&replies, false);
        if effects.bells > 0 {
            self.bells = self.bells.saturating_add(effects.bells);
            let bells = self.bells;
            self.port
                .shared
                .update(self.port.id, |info| info.bells = bells);
            if self.bell_at.is_none_or(|at| now >= at + BELL_INTERVAL) {
                self.bell_at = Some(now);
                self.port
                    .handle
                    .emitter()
                    .emit(TerminalEvent::Bell { id: self.port.id });
            }
        }
        if effects.title_changed {
            self.title_dirty = true;
        }
    }

    /// Adds encoded bytes to the write buffer. An input that does not fit in
    /// 4 MiB is dropped whole and counted.
    fn queue_write(&mut self, bytes: &[u8], is_input: bool) {
        if bytes.is_empty() || !self.writable {
            return;
        }
        let waiting = self.pending.len() - self.pending_from;
        if waiting + bytes.len() > MAX_PENDING_WRITE {
            if is_input {
                self.dropped_inputs = self.dropped_inputs.saturating_add(1);
                let dropped = self.dropped_inputs;
                self.port
                    .shared
                    .update(self.port.id, |info| info.dropped_inputs = dropped);
                if dropped == 1 {
                    warn!("terminal input dropped"; term = self.port.id.0, bytes = bytes.len());
                }
                self.port.listed();
            }
            return;
        }
        self.pending.extend_from_slice(bytes);
    }

    fn pump_write(&mut self) {
        while self.pending_from < self.pending.len() {
            let Some(pty) = &self.pty else { break };
            match pty.write(&self.pending[self.pending_from..]) {
                Write::Done(0) | Write::Again => break,
                Write::Done(n) => self.pending_from += n,
                Write::Closed => {
                    self.writable = false;
                    self.pending.clear();
                    self.pending_from = 0;
                    return;
                }
                Write::Failed(error) => {
                    warn!("pty write failed"; term = self.port.id.0, error = Value::io(&error));
                    self.writable = false;
                    self.pending.clear();
                    self.pending_from = 0;
                    return;
                }
            }
        }
        if self.pending_from == self.pending.len() {
            self.pending.clear();
            self.pending_from = 0;
        }
    }

    // --------------------------------------------------------------- frames

    fn frames(&mut self, now: Instant) -> Result<(), ()> {
        if self.port.slot.watchers() > 0 && self.engine.frame_due(now) {
            self.publish(now, true);
        }
        if self.engine_error { Err(()) } else { Ok(()) }
    }

    /// Builds a frame and keeps it as the latest. `announce` also sends
    /// `TerminalEvent::Frame` (when someone watches).
    fn publish(&mut self, now: Instant, announce: bool) {
        let started = Instant::now();
        match self.engine.publish(now) {
            Ok(frame) => {
                let generation = frame.generation;
                self.port.shared.set_frame(self.port.id, frame);
                self.stats.frames += 1;
                let took = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                self.stats.max_frame_us = self.stats.max_frame_us.max(took);
                self.stats.since.get_or_insert(now);
                if announce && self.port.slot.watchers() > 0 {
                    self.port.handle.emitter().emit(TerminalEvent::Frame {
                        id: self.port.id,
                        generation,
                    });
                }
            }
            Err(_) => self.engine_failed(),
        }
    }

    fn engine_failed(&mut self) {
        self.engine_error = true;
    }

    // --------------------------------------------------------------- timers

    fn note_activity(&mut self, now: Instant) {
        self.idle_due = Some(now + IDLE_COMPRESS);
        self.idle_steps = 0;
        self.stats.since.get_or_insert(now);
    }

    fn timers(&mut self, now: Instant) {
        if self.title_dirty && self.title_at.is_none_or(|at| now >= at + TITLE_INTERVAL) {
            self.title_dirty = false;
            self.title_at = Some(now);
            let title = self.engine.title();
            if title != self.title {
                self.title = title.clone();
                self.port
                    .shared
                    .update(self.port.id, |info| info.title = title);
                self.port.listed();
            }
        }
        if let Some(due) = self.idle_due
            && now >= due
        {
            self.compress(now);
        }
        if let Some(since) = self.stats.since
            && now >= since + STATS_INTERVAL
        {
            self.log_stats();
        }
    }

    /// One bounded compression step of the scrollback.
    fn compress(&mut self, now: Instant) {
        let token = self.engine.compression_activity();
        if token.is_some() && token == self.idle_done {
            self.idle_due = None;
            return;
        }
        self.idle_steps += 1;
        if self.engine.compress_step() {
            // More to do: the next round continues unless input or output
            // comes first.
            self.idle_due = Some(now);
            return;
        }
        self.idle_done = self.engine.compression_activity();
        self.idle_due = None;
        debug!("terminal compressed"; term = self.port.id.0, steps = self.idle_steps);
    }

    fn log_stats(&mut self) {
        let rows = self.engine.rebuilt_rows();
        debug!(
            "terminal throughput";
            term = self.port.id.0,
            read_bytes = self.stats.read_bytes,
            frames = self.stats.frames,
            max_frame_us = self.stats.max_frame_us,
            dirty_rows = rows - self.stats.rows_before,
        );
        self.stats = Stats {
            rows_before: rows,
            ..Stats::default()
        };
    }

    // ---------------------------------------------------------------- close

    /// `close`, project closed, deactivation or replacement: the process is
    /// ended (design 「数据流与线程」 关闭) and the thread ends.
    fn finish(mut self) {
        let reason = self.closing.unwrap_or(CloseReason::User);
        self.port.listed();
        let mut killed = false;
        if matches!(self.state, RunState::Running { .. }) {
            killed = end_process(&self.port.slot, self.pty.as_mut());
            if let Some(exit) = self.port.slot.exit() {
                info!(
                    "terminal exited";
                    term = self.port.id.0,
                    pid = self.pid.unwrap_or(0),
                    code = exit.code,
                    signaled = exit.signaled,
                    ran_ms = u64::try_from(
                        self.started_at
                            .map_or(0, |at| at.elapsed().as_millis())
                    )
                    .unwrap_or(u64::MAX),
                );
            }
        }
        info!("terminal closed"; term = self.port.id.0, reason = reason.value(), killed = killed);
        self.port.slot.drain();
    }
}
