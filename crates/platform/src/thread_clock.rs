//! The CPU time of a thread, readable from any thread (「线程的 CPU 时钟」 in
//! docs/design/platform.md), for the stuck-grammar check in `syntax::health`
//! (ADR 0041): a parse counts as stuck by the CPU time its thread used, which
//! does not grow while the thread waits for a CPU.
//!
//! macOS reads `thread_info(THREAD_BASIC_INFO)` on the thread's Mach port;
//! Linux reads the thread's CPU clock (`pthread_getcpuclockid`,
//! `clock_gettime`). Both are a few lines of FFI through `libc`, the only
//! `unsafe` here; every block says why it is sound.

use std::io;
use std::time::Duration;

#[cfg(test)]
mod tests;

/// The CPU clock of one thread. [`ThreadCpuClock::current`] is called on
/// the thread to measure; the clock it returns can be read on any thread.
/// `Send + Sync`, not `Clone`.
#[derive(Debug)]
pub struct ThreadCpuClock {
    clock: imp::Clock,
}

impl ThreadCpuClock {
    /// The clock of the calling thread. `ErrorKind::Unsupported` when this
    /// platform has no thread CPU clock or the clock cannot be read (it is
    /// read once here): the caller falls back to wall time. Does not block.
    pub fn current() -> io::Result<ThreadCpuClock> {
        let clock = imp::Clock::current().map_err(unsupported)?;
        clock.cpu_time().map_err(unsupported)?;
        Ok(ThreadCpuClock { clock })
    }

    /// The CPU time, user plus system, the thread has used so far. An error
    /// once the thread has exited. Does not block.
    pub fn cpu_time(&self) -> io::Result<Duration> {
        self.clock.cpu_time()
    }
}

/// `ErrorKind::Unsupported`, keeping the system's error as the source.
fn unsupported(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::Unsupported {
        return error;
    }
    io::Error::new(io::ErrorKind::Unsupported, error)
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io;
    use std::time::Duration;

    /// The `clockid_t` of a thread's CPU clock. glibc and musl encode the
    /// thread's id in it; once the thread has exited, `clock_gettime` fails
    /// with `EINVAL`.
    #[derive(Debug)]
    pub(super) struct Clock(libc::clockid_t);

    impl Clock {
        pub(super) fn current() -> io::Result<Clock> {
            let mut id: libc::clockid_t = 0;
            // SAFETY: `pthread_self()` names the calling thread, alive for the
            // whole call; `pthread_getcpuclockid` only writes one `clockid_t`
            // to `id`, a valid, exclusive pointer for the call.
            let rc = unsafe { libc::pthread_getcpuclockid(libc::pthread_self(), &mut id) };
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc));
            }
            Ok(Clock(id))
        }

        pub(super) fn cpu_time(&self) -> io::Result<Duration> {
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: `clock_gettime` only writes one `timespec` to `ts`, a
            // valid, exclusive pointer for the call. A clock id that no longer
            // names a thread is rejected with `EINVAL`, not undefined.
            let rc = unsafe { libc::clock_gettime(self.0, &mut ts) };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            let secs = u64::try_from(ts.tv_sec).map_err(|_| out_of_range())?;
            let nanos = u32::try_from(ts.tv_nsec).map_err(|_| out_of_range())?;
            Ok(Duration::new(secs, nanos))
        }
    }

    fn out_of_range() -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, "CPU time out of range")
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;
    use std::mem::MaybeUninit;
    use std::time::Duration;

    // `<mach/mach_port.h>` and `<mach/mach_init.h>`, exported by libSystem;
    // libc 0.2 does not bind the two functions and deprecates its binding of
    // `mach_task_self`.
    unsafe extern "C" {
        /// What the `mach_task_self()` macro reads: this task's port name,
        /// set by libSystem before `main` (and in a forked child before it
        /// runs any code) and never written afterwards.
        static mach_task_self_: libc::mach_port_t;
        fn mach_port_mod_refs(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
            right: libc::natural_t,
            delta: libc::integer_t,
        ) -> libc::kern_return_t;
        fn mach_port_deallocate(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
        ) -> libc::kern_return_t;
    }

    /// `MACH_PORT_RIGHT_SEND` from `<mach/port.h>`.
    const MACH_PORT_RIGHT_SEND: libc::natural_t = 0;

    fn task_self() -> libc::mach_port_t {
        // SAFETY: a plain integer that is only written before any Rust code
        // runs (see the declaration), so reading it races with nothing.
        unsafe { mach_task_self_ }
    }

    /// A send right to a thread's Mach port, held by this clock. The name
    /// `pthread_mach_thread_np` returns is the thread's own, released when
    /// the thread exits, after which the name could come back for another
    /// port. The extra user reference keeps it ours (a dead name once the
    /// thread is gone, so `thread_info` fails) until `drop` releases exactly
    /// that reference. No `mach_thread_self()`, whose reference would leak
    /// unless released.
    #[derive(Debug)]
    pub(super) struct Clock {
        port: libc::mach_port_t,
    }

    impl Clock {
        pub(super) fn current() -> io::Result<Clock> {
            // SAFETY: `pthread_self()` names the calling thread, alive for the
            // whole call; `pthread_mach_thread_np` returns its port name
            // without adding a reference.
            let port = unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) };
            if port == 0 {
                return Err(io::Error::other("the thread has no Mach port"));
            }
            // SAFETY: plain integers in, a status out. Adding one user
            // reference to a send right this task holds has no other effect,
            // and `Drop` removes it.
            let kr = unsafe { mach_port_mod_refs(task_self(), port, MACH_PORT_RIGHT_SEND, 1) };
            if kr != libc::KERN_SUCCESS {
                return Err(kern_error("mach_port_mod_refs", kr));
            }
            Ok(Clock { port })
        }

        pub(super) fn cpu_time(&self) -> io::Result<Duration> {
            let mut info = MaybeUninit::<libc::thread_basic_info>::zeroed();
            let mut count = libc::THREAD_BASIC_INFO_COUNT;
            // SAFETY: `info` has room for `THREAD_BASIC_INFO_COUNT` integers,
            // the count passed in, and `thread_info` writes at most that many;
            // a port name that is dead or not a thread is an error, not
            // undefined.
            let kr = unsafe {
                libc::thread_info(
                    self.port,
                    libc::THREAD_BASIC_INFO as libc::thread_flavor_t,
                    info.as_mut_ptr().cast::<libc::integer_t>(),
                    &mut count,
                )
            };
            if kr != libc::KERN_SUCCESS {
                return Err(kern_error("thread_info", kr));
            }
            // SAFETY: zeroed is a valid `thread_basic_info` (plain integers),
            // and the call succeeded, so it holds the kernel's values.
            let info = unsafe { info.assume_init() };
            Ok(time_value(info.user_time)? + time_value(info.system_time)?)
        }
    }

    impl Drop for Clock {
        fn drop(&mut self) {
            // SAFETY: releases the one user reference `current` added; the
            // name stays valid (live or dead) until then.
            unsafe {
                mach_port_deallocate(task_self(), self.port);
            }
        }
    }

    fn time_value(value: libc::time_value_t) -> io::Result<Duration> {
        let out_of_range = || io::Error::new(io::ErrorKind::InvalidData, "CPU time out of range");
        let secs = u64::try_from(value.seconds).map_err(|_| out_of_range())?;
        let micros = u64::try_from(value.microseconds).map_err(|_| out_of_range())?;
        Ok(Duration::from_secs(secs) + Duration::from_micros(micros))
    }

    fn kern_error(call: &str, kr: libc::kern_return_t) -> io::Error {
        io::Error::other(format!("{call}: kern_return_t {kr}"))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use std::io;
    use std::time::Duration;

    #[derive(Debug)]
    pub(super) struct Clock;

    impl Clock {
        pub(super) fn current() -> io::Result<Clock> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no thread CPU clock on this platform",
            ))
        }

        pub(super) fn cpu_time(&self) -> io::Result<Duration> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}
