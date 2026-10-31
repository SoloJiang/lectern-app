//! The terminal table (docs/design/terminal.md「数据结构」): every open
//! terminal's `TerminalInfo`, latest frame and `Slot`, under one mutex that
//! is held only to copy an `Arc` or to replace a value.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lectern_executor::Executor;
use lectern_kernel::{PluginHandle, ServiceRef, ServiceSet};
use lectern_services::project::{ProjectKey, Projects};
use lectern_services::session::{SessionKey, SessionSource};
use lectern_services::terminal::{
    Frame, LaunchError, LaunchKind, RunState, TerminalId, TerminalInfo, TerminalList,
};

use crate::diag::CloseReason;
use crate::slot::{Command, Slot, Wake};

/// The most terminals at once, whatever their state.
pub(crate) const MAX_TERMINALS: usize = 8;

/// A terminal's thread start, held until the plugin's handle exists.
pub(crate) type Start = Box<dyn FnOnce(&PluginHandle) -> bool + Send>;

/// What `app` and the tests choose.
#[derive(Default)]
pub(crate) struct Config {
    /// `Launch::Shell` starts this program, not the user's login shell.
    pub(crate) shell: Option<PathBuf>,
    /// The environment of agents (and of shells) instead of the login
    /// shell's.
    pub(crate) login_env: Option<Vec<(OsString, OsString)>>,
    /// Called on a terminal's thread at the start of every round (tests make
    /// a thread panic).
    pub(crate) thread_hook: Option<Arc<dyn Fn(TerminalId) + Send + Sync>>,
    /// The terminal threads running (tests: `no_threads_until_open`).
    pub(crate) threads: Arc<AtomicUsize>,
}

pub(crate) struct Shared {
    state: Mutex<State>,
    pub(crate) executor: Executor,
    pub(crate) projects: ServiceRef<dyn Projects>,
    pub(crate) sources: ServiceSet<dyn SessionSource>,
    pub(crate) config: Config,
}

struct State {
    next_id: u64,
    list: Arc<TerminalList>,
    entries: Vec<Entry>,
    host: Host,
}

struct Entry {
    info: TerminalInfo,
    frame: Option<Arc<Frame>>,
    slot: Arc<Slot>,
}

/// The plugin's handle exists from `started` on. Threads asked for before
/// are queued and start then.
enum Host {
    Waiting(Vec<Start>),
    Attached(PluginHandle),
    Stopped,
}

/// What `open` has to do once the table is updated, outside the lock.
pub(crate) enum Launched {
    /// Start now with this handle.
    Now(PluginHandle),
    /// Queued until `started`.
    Queued,
    /// The plugin is stopped.
    Stopped,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    pub(crate) fn new(
        executor: Executor,
        projects: ServiceRef<dyn Projects>,
        sources: ServiceSet<dyn SessionSource>,
        config: Config,
    ) -> Shared {
        Shared {
            state: Mutex::new(State {
                next_id: 1,
                list: Arc::new(TerminalList {
                    generation: 0,
                    terminals: Arc::from(Vec::new()),
                }),
                entries: Vec::new(),
                host: Host::Waiting(Vec::new()),
            }),
            executor,
            projects,
            sources,
            config,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    pub(crate) fn list(&self) -> Arc<TerminalList> {
        self.state().list.clone()
    }

    pub(crate) fn info(&self, id: TerminalId) -> Option<TerminalInfo> {
        self.state()
            .entries
            .iter()
            .find(|e| e.info.id == id)
            .map(|e| e.info.clone())
    }

    pub(crate) fn for_session(&self, session: &SessionKey) -> Option<TerminalId> {
        self.state()
            .entries
            .iter()
            .find(|e| matches!(&e.info.launch, LaunchKind::Resume(key) if key == session))
            .map(|e| e.info.id)
    }

    pub(crate) fn frame(&self, id: TerminalId) -> Option<Arc<Frame>> {
        self.state()
            .entries
            .iter()
            .find(|e| e.info.id == id)?
            .frame
            .clone()
    }

    pub(crate) fn slot(&self, id: TerminalId) -> Option<Arc<Slot>> {
        self.state()
            .entries
            .iter()
            .find(|e| e.info.id == id)
            .map(|e| e.slot.clone())
    }

    /// The list's current generation (read when an event is emitted, so a
    /// fast second change is not announced with a stale number).
    pub(crate) fn generation(&self) -> u64 {
        self.state().list.generation
    }

    pub(crate) fn set_frame(&self, id: TerminalId, frame: Arc<Frame>) {
        if let Some(entry) = self.state().entries.iter_mut().find(|e| e.info.id == id) {
            entry.frame = Some(frame);
        }
    }

    /// Changes one terminal's info and replaces the list. False when the
    /// terminal was closed.
    pub(crate) fn update(&self, id: TerminalId, f: impl FnOnce(&mut TerminalInfo)) -> bool {
        let mut state = self.state();
        let Some(entry) = state.entries.iter_mut().find(|e| e.info.id == id) else {
            return false;
        };
        f(&mut entry.info);
        state.publish();
        true
    }

    pub(crate) fn set_state(&self, id: TerminalId, run: RunState) -> bool {
        self.update(id, |info| info.state = run)
    }

    /// Adds a terminal, after the rules of `open` (design 「接口」): a
    /// session with a starting or running terminal is refused; one with an
    /// exited or failed terminal has it closed (`replaced`) first; at most
    /// eight terminals. Returns the new ID, the replaced terminal's slot (to
    /// close outside the lock) and what to do about the thread.
    pub(crate) fn admit(
        &self,
        wake: Wake,
        mut info: TerminalInfo,
        start: impl FnOnce(TerminalId, Arc<Slot>) -> Start,
    ) -> Result<(TerminalId, Option<Arc<Slot>>, Launched), LaunchError> {
        let mut state = self.state();
        if matches!(state.host, Host::Stopped) {
            return Err(LaunchError::UnknownSession);
        }
        let mut replaced = None;
        if let LaunchKind::Resume(key) = &info.launch
            && let Some(at) = state
                .entries
                .iter()
                .position(|e| matches!(&e.info.launch, LaunchKind::Resume(k) if k == key))
        {
            let existing = &state.entries[at];
            if matches!(
                existing.info.state,
                RunState::Starting | RunState::Running { .. }
            ) {
                return Err(LaunchError::AlreadyRunning(existing.info.id));
            }
            replaced = Some(state.entries.remove(at).slot);
        }
        // A replaced terminal made room: there are never more than eight.
        if state.entries.len() >= MAX_TERMINALS {
            return Err(LaunchError::TooMany);
        }
        let id = TerminalId(state.next_id);
        state.next_id += 1;
        info.id = id;
        let slot = Slot::new(id, wake);
        let begin = start(id, slot.clone());
        state.entries.push(Entry {
            info,
            frame: None,
            slot,
        });
        state.publish();
        let launched = match &mut state.host {
            Host::Waiting(queue) => {
                queue.push(begin);
                return Ok((id, replaced, Launched::Queued));
            }
            Host::Attached(handle) => Launched::Now(handle.clone()),
            Host::Stopped => Launched::Stopped,
        };
        drop(state);
        // The start runs outside the lock.
        if let Launched::Now(handle) = &launched
            && !begin(handle)
        {
            self.remove(id);
            return Err(LaunchError::UnknownSession);
        }
        Ok((id, replaced, launched))
    }

    /// Removes a terminal from the table and returns its slot.
    pub(crate) fn remove(&self, id: TerminalId) -> Option<Arc<Slot>> {
        let mut state = self.state();
        let at = state.entries.iter().position(|e| e.info.id == id)?;
        let entry = state.entries.remove(at);
        state.publish();
        Some(entry.slot)
    }

    /// Removes every terminal of `project`.
    pub(crate) fn remove_project(&self, project: &ProjectKey) -> Vec<Arc<Slot>> {
        let mut state = self.state();
        let mut removed = Vec::new();
        state.entries.retain(|entry| {
            if &entry.info.project == project {
                removed.push(entry.slot.clone());
                false
            } else {
                true
            }
        });
        if !removed.is_empty() {
            state.publish();
        }
        removed
    }

    /// Hands the plugin's handle to the table and starts the threads asked
    /// for before it existed.
    pub(crate) fn attach(&self, handle: PluginHandle) {
        let queued = {
            let mut state = self.state();
            match std::mem::replace(&mut state.host, Host::Attached(handle.clone())) {
                Host::Waiting(queued) => queued,
                other => {
                    // Never attached twice, never after a stop.
                    state.host = other;
                    return;
                }
            }
        };
        for begin in queued {
            if !begin(&handle) {
                // The activation is gone; `stop` follows.
                break;
            }
        }
    }

    /// The plugin is deactivated: asks every terminal to close and refuses
    /// new ones.
    pub(crate) fn stop(&self) {
        let slots: Vec<Arc<Slot>> = {
            let mut state = self.state();
            state.host = Host::Stopped;
            let slots = state.entries.drain(..).map(|e| e.slot).collect();
            state.publish();
            slots
        };
        for slot in slots {
            slot.push(Command::Close(CloseReason::Deactivated));
        }
    }
}

impl State {
    /// Replaces the list with the entries' current infos, one generation up.
    fn publish(&mut self) {
        let terminals: Arc<[TerminalInfo]> = self.entries.iter().map(|e| e.info.clone()).collect();
        self.list = Arc::new(TerminalList {
            generation: self.list.generation + 1,
            terminals,
        });
    }
}
