//! The stuck-grammar check (docs/adr/0041.md「external scanner 的处理」,
//! docs/design/file_reader.md「grammar 的卡死检测」).
//!
//! tree-sitter's progress callback cannot interrupt a hand-written external
//! scanner, so a scanner stuck in a loop keeps its thread. Every parse call
//! registers here right before it enters tree-sitter, on its own thread, and
//! unregisters when it returns with any result: only calls that are running
//! now are in the table. The next registration for the same grammar version
//! looks at the running calls of that version; one whose thread used more
//! CPU time since it registered than its budget plus
//! [`STUCK_GRACE`](crate::limits::STUCK_GRACE) disables the version for this
//! run, and that language shows as plain text from then on.
//!
//! CPU time does not grow while a thread waits for a CPU (a utility-QoS
//! batch thread pushed aside by a build), so waiting is never counted as
//! stuck. When the thread's CPU clock cannot be read, wall time is used with
//! [`STUCK_GRACE_WALL`](crate::limits::STUCK_GRACE_WALL).

use std::cell::OnceCell;
use std::io;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use lectern_logging::{Value, warn};
use lectern_platform::ThreadCpuClock;

use crate::diag;
use crate::limits::{STUCK_GRACE, STUCK_GRACE_WALL};

/// The one table of the process. `code_intel` uses it too.
pub fn global() -> &'static Health {
    static GLOBAL: OnceLock<Health> = OnceLock::new();
    GLOBAL.get_or_init(|| Health::with_clocks(Box::new(SystemClocks::new())))
}

/// The running parse calls and the grammar versions disabled in this run.
pub struct Health {
    shared: Arc<Shared>,
}

struct Shared {
    clocks: Box<dyn Clocks>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    running: Vec<Running>,
    /// (language, grammar version).
    disabled: Vec<(Box<str>, u64)>,
}

/// One call running on some thread.
struct Running {
    id: u64,
    lang: Box<str>,
    grammar_version: u64,
    budget: Duration,
    since: Since,
}

/// The reading taken when the call registered.
enum Since {
    Cpu {
        clock: Arc<dyn ThreadClock>,
        at: Duration,
    },
    /// The thread's CPU clock could not be read.
    Wall { at: Duration },
}

impl Health {
    pub(crate) fn with_clocks(clocks: Box<dyn Clocks>) -> Health {
        Health {
            shared: Arc::new(Shared {
                clocks,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// Another handle on the same table, kept by a sliced parse between its
    /// steps.
    pub(crate) fn handle(&self) -> Health {
        Health {
            shared: self.shared.clone(),
        }
    }

    /// Whether this grammar version may still be used in this run. Does not
    /// block (one uncontended lock).
    pub fn allows(&self, lang: &str, grammar_version: u64) -> bool {
        !self.shared.lock().is_disabled(lang, grammar_version)
    }

    /// Registers a call on the calling thread, right before it enters
    /// tree-sitter. `budget` is what is left of this call's budget (the
    /// remainder when it resumes, not the original deadline). The guard is
    /// dropped when the call returns, with any result; it never crosses an
    /// `await`, a stop or a thread.
    ///
    /// None when the version is disabled, or becomes disabled now: a
    /// running call of the same version has used more CPU time since it
    /// registered than its budget plus [`STUCK_GRACE`]. The caller shows the
    /// language as plain text.
    pub fn enter(&self, lang: &str, grammar_version: u64, budget: Duration) -> Option<ParseGuard> {
        // Read this thread's clock before taking the lock.
        let since = match self.shared.clocks.thread() {
            Some(clock) => match clock.cpu_time() {
                Ok(at) => Since::Cpu { clock, at },
                Err(_) => self.wall_since(),
            },
            None => self.wall_since(),
        };
        let mut state = self.shared.lock();
        if state.is_disabled(lang, grammar_version) {
            return None;
        }
        let stuck = state
            .running
            .iter()
            .filter(|r| &*r.lang == lang && r.grammar_version == grammar_version)
            .find(|r| self.is_stuck(r));
        if let Some(stuck) = stuck {
            warn!(
                "a parse ran past its budget plus the grace; the grammar is disabled for this run";
                lang = diag::lang(lang),
                grammar = Value::hex(grammar_version),
                budget_us = stuck.budget,
            );
            state.disabled.push((lang.into(), grammar_version));
            return None;
        }
        let id = state.next_id;
        state.next_id += 1;
        state.running.push(Running {
            id,
            lang: lang.into(),
            grammar_version,
            budget,
            since,
        });
        drop(state);
        Some(ParseGuard {
            shared: self.shared.clone(),
            id,
            _not_send: PhantomData,
        })
    }

    /// Starts measuring the calling thread's CPU time (wall time when its
    /// CPU clock cannot be read), on the same clocks as the stuck check.
    /// Read it back on the same thread.
    pub(crate) fn stopwatch(&self) -> Stopwatch {
        Stopwatch {
            cpu: self
                .shared
                .clocks
                .thread()
                .and_then(|clock| clock.cpu_time().ok().map(|at| (clock, at))),
            wall_at: self.shared.clocks.wall(),
            shared: self.shared.clone(),
        }
    }

    fn wall_since(&self) -> Since {
        Since::Wall {
            at: self.shared.clocks.wall(),
        }
    }

    fn is_stuck(&self, running: &Running) -> bool {
        match &running.since {
            // A clock that no longer reads belongs to a thread that has
            // exited, so the call is not running.
            Since::Cpu { clock, at } => clock.cpu_time().is_ok_and(|now| {
                now.saturating_sub(*at) > running.budget.saturating_add(STUCK_GRACE)
            }),
            Since::Wall { at } => {
                self.shared.clocks.wall().saturating_sub(*at)
                    > running.budget.saturating_add(STUCK_GRACE_WALL)
            }
        }
    }

    /// The number of calls in the table, for tests.
    #[cfg(test)]
    pub(crate) fn running(&self) -> usize {
        self.shared.lock().running.len()
    }

    /// The budgets the calls in the table registered with, for tests.
    #[cfg(test)]
    pub(crate) fn running_budgets(&self) -> Vec<Duration> {
        self.shared
            .lock()
            .running
            .iter()
            .map(|r| r.budget)
            .collect()
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // The state stays consistent if a holder panicked: every update is
        // a single push or remove.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl State {
    fn is_disabled(&self, lang: &str, grammar_version: u64) -> bool {
        self.disabled
            .iter()
            .any(|(l, v)| &**l == lang && *v == grammar_version)
    }
}

/// A registered call. Not `Send`: it exists only on the thread that
/// registered it, and dropping it unregisters the call.
///
/// ```compile_fail
/// fn send<T: Send>(_: T) {}
/// let guard = lectern_syntax::health::global()
///     .enter("json", 1, std::time::Duration::from_millis(8))
///     .unwrap();
/// send(guard);
/// ```
pub struct ParseGuard {
    shared: Arc<Shared>,
    id: u64,
    _not_send: PhantomData<*const ()>,
}

impl Drop for ParseGuard {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        if let Some(i) = state.running.iter().position(|r| r.id == self.id) {
            state.running.swap_remove(i);
        }
    }
}

/// What a [`Health::stopwatch`] measured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Elapsed {
    /// The thread's CPU time: it does not grow while the thread waits for a
    /// CPU.
    Cpu(Duration),
    /// Wall time: the thread's CPU clock could not be read.
    Wall(Duration),
}

/// A measurement started on one thread.
pub(crate) struct Stopwatch {
    cpu: Option<(Arc<dyn ThreadClock>, Duration)>,
    wall_at: Duration,
    shared: Arc<Shared>,
}

impl Stopwatch {
    pub(crate) fn elapsed(&self) -> Elapsed {
        match &self.cpu {
            Some((clock, at)) => match clock.cpu_time() {
                Ok(now) => Elapsed::Cpu(now.saturating_sub(*at)),
                Err(_) => self.wall(),
            },
            None => self.wall(),
        }
    }

    fn wall(&self) -> Elapsed {
        Elapsed::Wall(self.shared.clocks.wall().saturating_sub(self.wall_at))
    }
}

/// A thread's CPU clock, readable from any thread.
pub(crate) trait ThreadClock: Send + Sync {
    fn cpu_time(&self) -> io::Result<Duration>;
}

impl ThreadClock for ThreadCpuClock {
    fn cpu_time(&self) -> io::Result<Duration> {
        ThreadCpuClock::cpu_time(self)
    }
}

/// Where [`Health`] reads time: `platform`'s clocks, or a test's.
pub(crate) trait Clocks: Send + Sync {
    /// The calling thread's CPU clock; None when it cannot be read (wall
    /// time is used instead).
    fn thread(&self) -> Option<Arc<dyn ThreadClock>>;
    /// Monotonic wall time since an arbitrary origin.
    fn wall(&self) -> Duration;
}

struct SystemClocks {
    origin: Instant,
}

impl SystemClocks {
    fn new() -> SystemClocks {
        SystemClocks {
            origin: Instant::now(),
        }
    }
}

impl Clocks for SystemClocks {
    fn thread(&self) -> Option<Arc<dyn ThreadClock>> {
        thread_local! {
            /// This thread's clock, opened on its first parse and kept for
            /// the thread's life.
            static CLOCK: OnceCell<Option<Arc<ThreadCpuClock>>> = const { OnceCell::new() };
        }
        CLOCK.with(|cell| {
            cell.get_or_init(|| ThreadCpuClock::current().ok().map(Arc::new))
                .clone()
                .map(|clock| clock as Arc<dyn ThreadClock>)
        })
    }

    fn wall(&self) -> Duration {
        self.origin.elapsed()
    }
}
