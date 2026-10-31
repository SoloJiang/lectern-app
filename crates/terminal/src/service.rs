//! `dyn Terminals` of the plugin (docs/design/terminal.md「接口」). Every
//! method returns at once: a lookup in the table, a queued command, or the
//! checks of `launch::validate`, which read memory only.

use std::sync::mpsc::channel;
use std::sync::{Arc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use lectern_kernel::{ActivationToken, PluginHandle};
use lectern_logging::warn;
use lectern_services::session::{SessionKey, Timestamp};
use lectern_services::terminal::{
    FrameInterest, GridSize, Input, Launch, LaunchError, OpenSpec, RunState, TerminalId,
    TerminalInfo, TerminalList, Terminals,
};

use crate::diag::{self, CloseReason};
use crate::launch::{self, Prepared};
use crate::slot::{Command, CopyRequest, Slot, Wake};
use crate::table::{Shared, Start};
use crate::thread::{self, Job};

/// 80×24 with 8×16 pixel cells, until the view's first `Input::Resize`.
const DEFAULT_SIZE: GridSize = GridSize {
    cols: 80,
    rows: 24,
    cell_width: 8,
    cell_height: 16,
};

pub(crate) struct Service {
    pub(crate) shared: Arc<Shared>,
}

impl Terminals for Service {
    fn open(&self, spec: OpenSpec) -> Result<TerminalId, LaunchError> {
        let launch_name = match &spec.launch {
            Launch::Resume(_) => "resume",
            Launch::NewSession { .. } => "new_session",
            Launch::Shell { .. } => "shell",
        };
        let refused = |error: LaunchError| {
            if !matches!(error, LaunchError::AlreadyRunning(_)) {
                warn!(
                    "terminal launch refused";
                    reason = diag::launch_error(error),
                    launch = lectern_logging::Value::text(launch_name),
                );
            }
            error
        };
        let prepared = launch::validate(&self.shared, &spec.launch).map_err(refused)?;
        // Out of descriptors is the same to the caller as out of terminals.
        let wake = Wake::new().map_err(|_| refused(LaunchError::TooMany))?;
        let info = TerminalInfo {
            id: TerminalId(0),
            launch: prepared.kind.clone(),
            project: prepared.project.clone(),
            cwd: prepared.cwd.clone(),
            state: RunState::Starting,
            title: None,
            opened_at: now(),
            bells: 0,
            dropped_inputs: 0,
        };
        let size = spec.size.unwrap_or(DEFAULT_SIZE);
        let shared = Arc::downgrade(&self.shared);
        let (id, replaced, _) = self
            .shared
            .admit(wake, info, move |id, slot| {
                start(shared, id, slot, prepared, size, spec.theme)
            })
            .map_err(refused)?;
        if let Some(old) = replaced {
            old.push(Command::Close(CloseReason::Replaced));
        }
        Ok(id)
    }

    fn list(&self) -> Arc<TerminalList> {
        self.shared.list()
    }

    fn info(&self, id: TerminalId) -> Option<TerminalInfo> {
        self.shared.info(id)
    }

    fn for_session(&self, session: &SessionKey) -> Option<TerminalId> {
        self.shared.for_session(session)
    }

    fn frame(&self, id: TerminalId) -> Option<Arc<lectern_services::terminal::Frame>> {
        self.shared.frame(id)
    }

    fn watch(&self, id: TerminalId) -> Option<FrameInterest> {
        let slot = self.shared.slot(id)?;
        slot.add_watcher();
        slot.push(Command::Watch);
        Some(FrameInterest::new(move || slot.remove_watcher()))
    }

    fn input(&self, id: TerminalId, input: Input) {
        if let Some(slot) = self.shared.slot(id) {
            slot.push(Command::Input(input));
        }
    }

    fn copy_selection(
        &self,
        id: TerminalId,
        token: ActivationToken,
        reply: Box<dyn FnOnce(Option<String>) + Send>,
    ) {
        let request = CopyRequest { token, reply };
        match self.shared.slot(id) {
            Some(slot) => slot.push(Command::Copy(request)),
            None => request.answer(None),
        }
    }

    fn close(&self, id: TerminalId) {
        if let Some(slot) = self.shared.remove(id) {
            slot.push(Command::Close(CloseReason::User));
        }
    }
}

/// The start of a terminal's two threads, run with the plugin's handle:
/// immediately when the plugin is started, else when it is.
fn start(
    shared: Weak<Shared>,
    id: TerminalId,
    slot: Arc<Slot>,
    prepared: Prepared,
    size: GridSize,
    theme: lectern_services::terminal::Theme,
) -> Start {
    Box::new(move |handle: &PluginHandle| {
        let Some(shared) = shared.upgrade() else {
            return false;
        };
        let (children, receiver) = channel();
        let executor = shared.executor.clone();
        let wait_slot = slot.clone();
        let threads = shared.config.threads.clone();
        let job = Job {
            shared,
            slot,
            handle: handle.clone(),
            prepared,
            size,
            theme,
            children,
        };
        let name = format!("terminal-{}", id.0);
        handle.own_with(move |gate| {
            let engine = executor.run_dedicated_gated(&gate, &name, move || thread::run(job));
            let wait = executor.run_dedicated_gated(&gate, &format!("{name}-wait"), move || {
                thread::wait(wait_slot, receiver, threads)
            });
            (engine, wait)
        })
    })
}

/// UTC milliseconds.
fn now() -> Timestamp {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp(i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}
