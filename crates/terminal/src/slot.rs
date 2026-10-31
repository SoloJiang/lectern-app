//! The link between a terminal's callers and its thread: a command queue
//! with a wake socket (docs/design/terminal.md「数据流与线程」).
//!
//! Callers push a command and, if the thread is not already awake for an
//! earlier one, write one byte to the socket; the thread `poll`s the socket
//! together with the PTY. Nothing here blocks.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lectern_kernel::ActivationToken;
use lectern_services::terminal::{Input, TerminalId};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use crate::diag::CloseReason;

/// What a terminal's thread is asked to do.
pub(crate) enum Command {
    Input(Input),
    /// A new `FrameInterest`: publish a frame now.
    Watch,
    /// `Terminals::copy_selection`.
    Copy(CopyRequest),
    /// The wait thread reported the process's exit (read it with
    /// `Slot::exit`).
    Exited,
    Close(CloseReason),
}

pub(crate) struct CopyRequest {
    pub(crate) token: ActivationToken,
    pub(crate) reply: Box<dyn FnOnce(Option<String>) + Send>,
}

impl CopyRequest {
    /// Calls `reply` once inside the caller's admission; dropped without a
    /// call when the caller is deactivated.
    pub(crate) fn answer(self, text: Option<String>) {
        if let Some(_admission) = self.token.enter() {
            (self.reply)(text);
        }
    }
}

/// How the process ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ExitInfo {
    pub(crate) code: u32,
    pub(crate) signaled: bool,
}

pub(crate) struct Slot {
    pub(crate) id: TerminalId,
    queue: Mutex<VecDeque<Command>>,
    /// A wake byte is on its way or the thread has not yet looked at the
    /// queue.
    woken: AtomicBool,
    wake_tx: UnixStream,
    wake_rx: UnixStream,
    watchers: AtomicU32,
    /// 0 until the process is started.
    pid: AtomicU32,
    exit: Mutex<Option<ExitInfo>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The two ends of a wake socket, made before the table is locked.
pub(crate) struct Wake {
    tx: UnixStream,
    rx: UnixStream,
}

impl Wake {
    pub(crate) fn new() -> io::Result<Wake> {
        let (tx, rx) = UnixStream::pair()?;
        tx.set_nonblocking(true)?;
        rx.set_nonblocking(true)?;
        Ok(Wake { tx, rx })
    }
}

impl Slot {
    pub(crate) fn new(id: TerminalId, wake: Wake) -> Arc<Slot> {
        Arc::new(Slot {
            id,
            queue: Mutex::new(VecDeque::new()),
            woken: AtomicBool::new(false),
            wake_tx: wake.tx,
            wake_rx: wake.rx,
            watchers: AtomicU32::new(0),
            pid: AtomicU32::new(0),
            exit: Mutex::new(None),
        })
    }

    /// Queues a command and wakes the thread. Any thread; never blocks.
    pub(crate) fn push(&self, command: Command) {
        lock(&self.queue).push_back(command);
        if !self.woken.swap(true, Ordering::AcqRel) {
            // A full socket buffer means a byte is already waiting.
            let _ = (&self.wake_tx).write(&[1]);
        }
    }

    /// The thread's side: empties the wake socket, then takes every queued
    /// command. A command pushed after the flag is cleared writes a new byte.
    pub(crate) fn take(&self) -> VecDeque<Command> {
        let mut sink = [0u8; 64];
        while matches!((&self.wake_rx).read(&mut sink), Ok(n) if n > 0) {}
        self.woken.store(false, Ordering::Release);
        std::mem::take(&mut *lock(&self.queue))
    }

    /// Whether a `Close` is queued, without taking anything.
    pub(crate) fn close_queued(&self) -> bool {
        lock(&self.queue)
            .iter()
            .any(|command| matches!(command, Command::Close(_)))
    }

    pub(crate) fn watchers(&self) -> u32 {
        self.watchers.load(Ordering::Acquire)
    }

    pub(crate) fn add_watcher(&self) {
        self.watchers.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn remove_watcher(&self) {
        let _ = self
            .watchers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
    }

    pub(crate) fn set_pid(&self, pid: u32) {
        self.pid.store(pid, Ordering::Release);
    }

    /// The process's ID, once it is started.
    pub(crate) fn pid(&self) -> Option<u32> {
        match self.pid.load(Ordering::Acquire) {
            0 => None,
            pid => Some(pid),
        }
    }

    /// The wait thread's report.
    pub(crate) fn set_exit(&self, info: ExitInfo) {
        *lock(&self.exit) = Some(info);
        self.push(Command::Exited);
    }

    pub(crate) fn exit(&self) -> Option<ExitInfo> {
        *lock(&self.exit)
    }

    /// Waits for the wake socket, or for `master` with `flags` when given,
    /// until `timeout` (forever when None). The caller looks at everything
    /// again afterwards, so an interrupted or spurious wake-up costs nothing.
    pub(crate) fn wait(
        &self,
        master: Option<(BorrowedFd<'_>, PollFlags)>,
        timeout: Option<Duration>,
    ) {
        let wake = self.wake_rx.as_fd();
        let spec = timeout.map(|t| Timespec {
            tv_sec: i64::try_from(t.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(t.subsec_nanos()),
        });
        match master {
            Some((fd, flags)) => {
                let mut fds = [PollFd::new(&wake, PollFlags::IN), PollFd::new(&fd, flags)];
                let _ = poll(&mut fds, spec.as_ref());
            }
            None => {
                let mut fds = [PollFd::new(&wake, PollFlags::IN)];
                let _ = poll(&mut fds, spec.as_ref());
            }
        }
    }

    /// Waits until the wait thread has reported the exit or `timeout` has
    /// passed, answering queued copy requests with nothing and dropping
    /// the other commands (the terminal is closing). Returns whether the
    /// process has exited.
    pub(crate) fn wait_exit(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            for command in self.take() {
                if let Command::Copy(request) = command {
                    request.answer(None);
                }
            }
            if self.exit().is_some() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.wait(None, Some(deadline - now));
        }
    }

    /// Answers every queued copy request with nothing and drops the other
    /// commands.
    pub(crate) fn drain(&self) {
        for command in self.take() {
            if let Command::Copy(request) = command {
                request.answer(None);
            }
        }
    }
}
