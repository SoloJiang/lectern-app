//! The PTY and the child process (docs/design/terminal.md「portable-pty」).
//!
//! portable-pty opens the pair and starts the process. The primary end is
//! then used through its raw descriptor, set non-blocking, so the terminal
//! thread can wait for it and for its wake socket in one `poll` and a
//! closing terminal never leaves a thread blocked in a read.

use std::ffi::OsString;
use std::io;
use std::os::fd::{BorrowedFd, RawFd};
use std::path::Path;

use lectern_services::terminal::GridSize;
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group};

/// What to start.
pub(crate) enum Program {
    /// An agent's executable, found in the login environment's `PATH`;
    /// argv is the path and `args`.
    Agent {
        path: std::path::PathBuf,
        args: Vec<OsString>,
    },
    /// A login shell: argv[0] is `-` and the file name, no arguments.
    Shell { path: std::path::PathBuf },
}

/// The child as the terminal thread keeps it: the primary end and the way
/// to hang it up.
pub(crate) struct Pty {
    master: Option<Box<dyn MasterPty + Send>>,
    raw: RawFd,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pid: u32,
}

/// The process's handle for the wait thread, and what the terminal thread
/// and the closing path need to end it.
pub(crate) struct Spawned {
    pub(crate) pty: Pty,
    pub(crate) child: Box<dyn Child + Send + Sync>,
}

/// Why the process could not be started.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpawnError {
    pub(crate) errno: Option<i32>,
}

/// The result of one read of the primary end.
pub(crate) enum Read {
    Data(usize),
    /// Nothing to read now.
    Again,
    /// Every descriptor of the secondary end is closed.
    Eof,
    Failed(io::Error),
}

/// The result of one write to the primary end.
pub(crate) enum Write {
    Done(usize),
    Again,
    /// The secondary end is gone.
    Closed,
    Failed(io::Error),
}

/// Opens a PTY of `size`, starts `program` in it with exactly `env` in
/// `cwd`, and returns the primary end, set non-blocking. The secondary end
/// is dropped before returning, so the primary end reads a hang-up when the
/// process and its children close theirs.
pub(crate) fn spawn(
    program: &Program,
    env: &[(OsString, OsString)],
    cwd: &Path,
    size: GridSize,
) -> Result<Spawned, SpawnError> {
    let pair = native_pty_system()
        .openpty(pty_size(size))
        .map_err(|error| spawn_error(&*error))?;
    let mut command = match program {
        Program::Agent { path, args } => {
            let mut command = CommandBuilder::new(path);
            command.args(args);
            command
        }
        // The default program is the login shell of the builder's `SHELL`:
        // argv[0] is `-` and its file name.
        Program::Shell { .. } => CommandBuilder::new_default_prog(),
    };
    command.env_clear();
    for (name, value) in env {
        command.env(name, value);
    }
    if let Program::Shell { path } = program {
        command.env("SHELL", path);
    }
    command.cwd(cwd);
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| spawn_error(&*error))?;
    // The parent keeps no secondary end: without this the primary end never
    // reads the hang-up.
    drop(pair.slave);
    let master = pair.master;
    let Some(raw) = master.as_raw_fd() else {
        return Err(SpawnError { errno: None });
    };
    // SAFETY: `master` owns `raw` and is alive for this call.
    let fd = unsafe { BorrowedFd::borrow_raw(raw) };
    let flags = fcntl_getfl(fd).map_err(errno_error)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(errno_error)?;
    let pid = child.process_id().ok_or(SpawnError { errno: None })?;
    let killer = child.clone_killer();
    Ok(Spawned {
        pty: Pty {
            master: Some(master),
            raw,
            killer,
            pid,
        },
        child,
    })
}

fn pty_size(size: GridSize) -> PtySize {
    PtySize {
        rows: size.rows.max(1),
        cols: size.cols.max(1),
        pixel_width: size.cols.saturating_mul(size.cell_width),
        pixel_height: size.rows.saturating_mul(size.cell_height),
    }
}

/// The system error code in the chain of `error`, if one of its causes is an
/// `io::Error` that has one.
fn spawn_error(error: &(dyn std::error::Error + 'static)) -> SpawnError {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if let Some(code) = error
            .downcast_ref::<io::Error>()
            .and_then(io::Error::raw_os_error)
        {
            return SpawnError { errno: Some(code) };
        }
        cause = error.source();
    }
    SpawnError { errno: None }
}

fn errno_error(errno: Errno) -> SpawnError {
    SpawnError {
        errno: Some(errno.raw_os_error()),
    }
}

impl Pty {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// The primary end while it is open. The descriptor stays valid for the
    /// borrow because `self` owns the `MasterPty`.
    pub(crate) fn fd(&self) -> Option<BorrowedFd<'_>> {
        self.master.as_ref()?;
        // SAFETY: `raw` belongs to `self.master`, which is `Some` and cannot
        // be dropped while `self` is borrowed.
        Some(unsafe { BorrowedFd::borrow_raw(self.raw) })
    }

    pub(crate) fn read(&self, buf: &mut [u8]) -> Read {
        let Some(fd) = self.fd() else {
            return Read::Eof;
        };
        loop {
            return match rustix::io::read(fd, &mut *buf) {
                Ok(0) => Read::Eof,
                Ok(n) => Read::Data(n),
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => Read::Again,
                // EIO: every descriptor of the secondary end is closed.
                Err(Errno::IO) => Read::Eof,
                Err(error) => Read::Failed(error.into()),
            };
        }
    }

    pub(crate) fn write(&self, buf: &[u8]) -> Write {
        let Some(fd) = self.fd() else {
            return Write::Closed;
        };
        loop {
            return match rustix::io::write(fd, buf) {
                Ok(n) => Write::Done(n),
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => Write::Again,
                Err(Errno::IO | Errno::PIPE) => Write::Closed,
                Err(error) => Write::Failed(error.into()),
            };
        }
    }

    /// Tells the kernel and the process the window changed; the process
    /// gets `SIGWINCH`.
    pub(crate) fn resize(&self, size: GridSize) {
        if let Some(master) = &self.master {
            // A failed resize leaves the old size: the process keeps
            // working with it.
            let _ = master.resize(pty_size(size));
        }
    }

    /// `SIGHUP` to the process.
    pub(crate) fn hang_up(&mut self) {
        // The process may be gone already.
        let _ = self.killer.kill();
    }

    /// Closes the primary end: the foreground process group is hung up.
    pub(crate) fn close(&mut self) {
        self.master = None;
    }
}

/// `SIGKILL` to the process group of the child (`setsid` made `pid` the
/// group's ID).
pub(crate) fn kill_group(pid: u32) {
    if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
        // The group may be gone already.
        let _ = kill_process_group(pid, Signal::KILL);
    }
}
