//! Activation tokens, admissions and the quiescence counters.

use std::fmt;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering::Relaxed, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::Duration;

use lectern_logging::{Ctx, PluginTag, Scope, warn};

use crate::background::BackgroundState;
use crate::diag::Phase;
use crate::gate::GateInner;
use crate::handle::PluginHandle;
use crate::kernel::Shared;
use crate::panic::contain_plugin;
use crate::plugin::{Name, PluginError};
use crate::util::lock;

/// The next activation number (`ActivationToken::id`), process-wide.
static NEXT_ACTIVATION: AtomicU64 = AtomicU64::new(1);

const PENDING: u8 = 0;
const LIVE: u8 = 1;
const DEAD: u8 = 2;

/// State of one activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenState {
    /// The activation has not finished.
    Pending,
    /// Every `commit` and `started` succeeded and the plugin has not been
    /// deactivated.
    Live,
    /// The activation failed or has been deactivated. Never changes again.
    Dead,
}

/// A busy/quiescent transition with its sequence number, or a change of the
/// background state with the background epoch.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Transition {
    Busy(u64),
    Quiescent(u64),
    Background(BackgroundState, u64),
}

/// Global counters behind quiescence (`docs/design/kernel.md`, 静止).
///
/// One atomic word holds the number of outstanding busy units in the low 32
/// bits and the transition sequence number in the high 32 bits, so every
/// transition takes its sequence number in the same operation that crosses
/// zero. Busy units are: global admissions, `BusyGuard`s, `PendingGuard`s,
/// in-progress `emit`s and queued or running lifecycle requests.
pub(crate) struct Counters {
    word: AtomicU64,
    /// `PendingGuard` count, with `PENDING_CLOSED` set once shutdown closes it.
    pending: AtomicUsize,
    notify: OnceLock<Box<dyn Fn(Transition) + Send + Sync>>,
}

const COUNT_MASK: u64 = 0xFFFF_FFFF;
const PENDING_CLOSED: usize = 1 << (usize::BITS - 1);

impl Counters {
    pub(crate) fn new() -> Self {
        Counters {
            word: AtomicU64::new(0),
            pending: AtomicUsize::new(0),
            notify: OnceLock::new(),
        }
    }

    pub(crate) fn set_notify(&self, notify: Box<dyn Fn(Transition) + Send + Sync>) {
        let _ = self.notify.set(notify);
    }

    pub(crate) fn inc(&self) {
        let mut current = self.word.load(SeqCst);
        loop {
            let count = current & COUNT_MASK;
            let seq = current >> 32;
            let (next, transition) = if count == 0 {
                let seq = seq.wrapping_add(1) & COUNT_MASK;
                ((seq << 32) | 1, Some(Transition::Busy(seq)))
            } else {
                (current + 1, None)
            };
            match self
                .word
                .compare_exchange_weak(current, next, SeqCst, SeqCst)
            {
                Ok(_) => {
                    if let Some(transition) = transition {
                        self.announce(transition);
                    }
                    return;
                }
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn dec(&self) {
        let mut current = self.word.load(SeqCst);
        loop {
            let count = current & COUNT_MASK;
            let seq = current >> 32;
            debug_assert!(count > 0, "busy counter underflow");
            if count == 0 {
                return;
            }
            let (next, transition) = if count == 1 {
                let seq = seq.wrapping_add(1) & COUNT_MASK;
                (seq << 32, Some(Transition::Quiescent(seq)))
            } else {
                (current - 1, None)
            };
            match self
                .word
                .compare_exchange_weak(current, next, SeqCst, SeqCst)
            {
                Ok(_) => {
                    if let Some(transition) = transition {
                        self.announce(transition);
                    }
                    return;
                }
                Err(actual) => current = actual,
            }
        }
    }

    /// The current quiescent epoch, or `None` while busy.
    pub(crate) fn epoch(&self) -> Option<u64> {
        let word = self.word.load(SeqCst);
        (word & COUNT_MASK == 0).then_some(word >> 32)
    }

    fn announce(&self, transition: Transition) {
        if let Some(notify) = self.notify.get() {
            notify(transition);
        }
    }

    pub(crate) fn pending(self: &Arc<Self>) -> Option<PendingGuard> {
        let mut current = self.pending.load(SeqCst);
        loop {
            if current & PENDING_CLOSED != 0 {
                return None;
            }
            match self
                .pending
                .compare_exchange_weak(current, current + 1, SeqCst, SeqCst)
            {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        self.inc();
        Some(PendingGuard {
            counters: self.clone(),
        })
    }

    /// Closes `pending()` once no guard is outstanding, or when `timeout`
    /// expires. Called by `shutdown`.
    pub(crate) fn close_pending(&self, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self
                .pending
                .compare_exchange(0, PENDING_CLOSED, SeqCst, SeqCst)
                .is_ok()
            {
                return;
            }
            if std::time::Instant::now() >= deadline {
                self.pending.fetch_or(PENDING_CLOSED, SeqCst);
                warn!("shutdown closed pending() while main-thread work was still queued");
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(crate) fn busy(self: &Arc<Self>) -> BusyGuard {
        self.inc();
        BusyGuard {
            counters: self.clone(),
        }
    }
}

/// Entries of an activation's guard table, dropped in reverse order when the
/// activation is revoked.
pub(crate) enum Entry {
    Guard(Box<dyn Send>),
    /// A listener; the registry entry is removed by the revocation.
    Listener,
    Task {
        gate: Arc<GateInner>,
        handle: Option<Box<dyn Send>>,
    },
}

pub(crate) struct GuardTable {
    pub(crate) closed: bool,
    next_seq: u64,
    pub(crate) entries: Vec<(u64, Entry)>,
}

impl GuardTable {
    pub(crate) fn push(&mut self, entry: Entry) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.entries.push((seq, entry));
        seq
    }

    pub(crate) fn take(&mut self, seq: u64) -> Option<Entry> {
        let index = self.entries.iter().position(|(s, _)| *s == seq)?;
        Some(self.entries.remove(index).1)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Work held while the activation is still `Pending` (inside `started`).
pub(crate) enum StagedItem {
    Emit(Box<dyn FnOnce() + Send>),
    Start(Box<dyn FnOnce(&PluginHandle) + Send>),
}

pub(crate) struct Staging {
    pub(crate) sealed: bool,
    pub(crate) items: Vec<StagedItem>,
}

pub(crate) struct ActInner {
    state: AtomicU8,
    admissions: AtomicUsize,
    idle: Mutex<()>,
    idle_changed: Condvar,
    pub(crate) counters: Arc<Counters>,
    pub(crate) id: Name,
    pub(crate) generation: u64,
    /// The activation number, process-wide from 1: `act` in log records.
    number: u64,
    /// The plugin's log tag: `plugin` in log records.
    tag: Option<PluginTag>,
    pub(crate) kernel: Weak<Shared>,
    pub(crate) guards: Mutex<GuardTable>,
    pub(crate) staging: Mutex<Staging>,
    /// The failure that killed this activation, if it failed rather than being
    /// deactivated or losing a dependency.
    pub(crate) failure: Mutex<Option<PluginError>>,
}

impl ActInner {
    fn scope(&self) -> Scope {
        lectern_logging::scope(Ctx {
            plugin: self.tag,
            act: Some(self.number),
            ..Ctx::default()
        })
    }
}

/// The success marker of one activation. Cloneable and usable on any thread.
///
/// Its state only moves `Pending → Live`, `Pending → Dead` or `Live → Dead`.
#[derive(Clone)]
pub struct ActivationToken(pub(crate) Arc<ActInner>);

impl ActivationToken {
    pub(crate) fn new(
        counters: Arc<Counters>,
        kernel: Weak<Shared>,
        id: Name,
        generation: u64,
        tag: Option<PluginTag>,
    ) -> Self {
        ActivationToken(Arc::new(ActInner {
            state: AtomicU8::new(PENDING),
            admissions: AtomicUsize::new(0),
            idle: Mutex::new(()),
            idle_changed: Condvar::new(),
            counters,
            id,
            generation,
            number: NEXT_ACTIVATION.fetch_add(1, Relaxed),
            tag,
            kernel,
            guards: Mutex::new(GuardTable {
                closed: false,
                next_seq: 0,
                entries: Vec::new(),
            }),
            staging: Mutex::new(Staging {
                sealed: false,
                items: Vec::new(),
            }),
            failure: Mutex::new(None),
        }))
    }

    /// This activation's number, process-wide from 1: one per token, never
    /// reused. Log records carry it as `act`.
    pub fn id(&self) -> u64 {
        self.0.number
    }

    /// Sets this activation's `plugin` and `act` log scope until dropped.
    pub(crate) fn scope(&self) -> Scope {
        self.0.scope()
    }

    /// The current state.
    pub fn state(&self) -> TokenState {
        match self.0.state.load(SeqCst) {
            PENDING => TokenState::Pending,
            LIVE => TokenState::Live,
            _ => TokenState::Dead,
        }
    }

    /// Admission to call this activation's code. Returns `None` unless the
    /// token is `Live`. Never blocks; nesting on one thread does not deadlock.
    /// Deactivation waits for every admission to be dropped before it drops
    /// guards and the `Active` state.
    pub fn enter(&self) -> Option<Admission> {
        self.admit(|state| state == LIVE, true)
    }

    /// A guard counted directly by the kernel for work queued to another thread
    /// (applying or removing UI registrations). Works for `Dead` tokens; returns
    /// `None` only after `shutdown` has closed it.
    pub fn pending(&self) -> Option<PendingGuard> {
        self.0.counters.pending()
    }

    /// Like `enter`, but also accepts `Pending` (used by `busy()` inside
    /// `started`).
    pub(crate) fn enter_busy(&self) -> Option<Admission> {
        self.admit(|state| state != DEAD, true)
    }

    /// Like `enter_busy`, but not counted towards quiescence: taking a
    /// `BackgroundGuard` must not make the kernel busy for a moment.
    pub(crate) fn enter_background(&self) -> Option<Admission> {
        self.admit(|state| state != DEAD, false)
    }

    /// Like `enter`, but not counted towards quiescence. Used only to call
    /// `Busy` / `Quiescent` / `BackgroundChanged` listeners.
    pub(crate) fn enter_uncounted(&self) -> Option<Admission> {
        self.admit(|state| state == LIVE, false)
    }

    fn admit(&self, accept: impl Fn(u8) -> bool, counted: bool) -> Option<Admission> {
        if !accept(self.0.state.load(SeqCst)) {
            return None;
        }
        self.0.admissions.fetch_add(1, SeqCst);
        if counted {
            self.0.counters.inc();
        }
        let admission = Admission {
            act: self.0.clone(),
            counted,
        };
        if accept(self.0.state.load(SeqCst)) {
            Some(admission)
        } else {
            drop(admission);
            None
        }
    }

    /// Marks the token `Dead`. Returns false if it already was.
    pub(crate) fn mark_dead(&self) -> bool {
        self.0.state.swap(DEAD, SeqCst) != DEAD
    }

    /// `Pending → Live`. Fails if another thread already wrote `Dead`.
    pub(crate) fn go_live(&self) -> bool {
        self.0
            .state
            .compare_exchange(PENDING, LIVE, SeqCst, SeqCst)
            .is_ok()
    }

    /// Blocks until no admission is held. Only the kernel thread calls this,
    /// after the token is `Dead`, so no new admission can start.
    pub(crate) fn wait_idle(&self) {
        let mut guard = lock(&self.0.idle);
        while self.0.admissions.load(SeqCst) != 0 {
            guard = match self
                .0
                .idle_changed
                .wait_timeout(guard, Duration::from_millis(50))
            {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// Wakes every task gate of this activation so suspended tasks are polled
    /// once and see the new state.
    pub(crate) fn wake_gates(&self) {
        let gates: Vec<Arc<GateInner>> = lock(&self.0.guards)
            .entries
            .iter()
            .filter_map(|(_, entry)| match entry {
                Entry::Task { gate, .. } => Some(gate.clone()),
                _ => None,
            })
            .collect();
        for gate in gates {
            gate.wake_all();
        }
    }

    pub(crate) fn record_failure(&self, error: PluginError) {
        let mut failure = lock(&self.0.failure);
        if failure.is_none() {
            *failure = Some(error);
        }
    }

    pub(crate) fn failure(&self) -> Option<PluginError> {
        lock(&self.0.failure).clone()
    }

    pub(crate) fn same(&self, other: &ActivationToken) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for ActivationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActivationToken")
            .field("plugin", &self.0.id)
            .field("generation", &self.0.generation)
            .field("state", &self.state())
            .finish()
    }
}

/// Admission into one activation, held while calling its code. Dropping it
/// lets a waiting deactivation continue.
pub struct Admission {
    act: Arc<ActInner>,
    counted: bool,
}

impl Admission {
    /// Hands a guard to the kernel using this admission. Only checks whether
    /// the guard table is closed, not whether the token is already `Dead`: the
    /// table cannot close while this admission is held, so the guard is always
    /// dropped by the revocation before the `Active` state. Returns false (and
    /// drops the guard here, under `contain`) only if the table was already
    /// closed.
    pub fn own(&self, guard: impl Send + 'static) -> bool {
        let mut table = lock(&self.act.guards);
        if table.closed {
            drop(table);
            let _scope = self.act.scope();
            drop_rejected(guard);
            return false;
        }
        table.push(Entry::Guard(Box::new(guard)));
        true
    }

    /// The number of the activation this admission is into.
    pub(crate) fn act_id(&self) -> u64 {
        self.act.number
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        let previous = self.act.admissions.fetch_sub(1, SeqCst);
        if self.counted {
            self.act.counters.dec();
        }
        if previous == 1 && self.act.state.load(SeqCst) == DEAD {
            let _guard = lock(&self.act.idle);
            self.act.idle_changed.notify_all();
        }
    }
}

/// Declares outstanding work; the kernel is not quiescent until it is dropped.
pub struct BusyGuard {
    counters: Arc<Counters>,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.counters.dec();
    }
}

/// Main-thread work queued but not yet applied; the kernel is not quiescent
/// until it is dropped.
pub struct PendingGuard {
    counters: Arc<Counters>,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.counters.pending.fetch_sub(1, SeqCst);
        self.counters.dec();
    }
}

/// Drops a value the kernel refused, isolating a panic in its `Drop` (logged
/// as `plugin failed` in the `drop` phase, under the caller's scope).
pub(crate) fn drop_rejected<T>(value: T) {
    let _ = contain_plugin(Phase::Drop, move || drop(value));
}
