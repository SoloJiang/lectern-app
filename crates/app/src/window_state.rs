//! The window state file and the frame autosave name (docs/design/workspace.md
//! 「窗口状态」「对其他 crate 的要求」D、E; docs/design/app.md「窗口状态」).
//!
//! `workspace` reports every change of its [`WindowState`] on the main
//! thread and with no `cx`, so the report goes through a channel to a
//! foreground task started here. The task switches the frame autosave name
//! (`project` while a project is open, `welcome` otherwise), waits until the
//! state has been still for 500 ms and writes `window.json` in an executor
//! `Background` task. The file is read in the same kind of task after the
//! barrier; both run behind the first frame, and no path reaches the log.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::future::{Either, select};
use gpui::{AnyWindowHandle, App};
use lectern_executor::{Executor, Priority};
use lectern_logging::{Value, warn};
use lectern_workspace::{WindowState, WorkspaceHost};

/// How long the state must be still before it is written.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// The frame autosave name of the window with no project (Welcome).
pub const WELCOME: &str = "welcome";
/// The frame autosave name of the window with a project open.
pub const PROJECT: &str = "project";

/// The directory under `platform::dirs::app_data()`, and the file in it.
const DIR: &str = "workspace";
const FILE: &str = "window.json";
const TEMP: &str = "window.json.tmp";

/// The directory of `window.json` under the application data directory.
pub fn dir(app_data: &Path) -> PathBuf {
    app_data.join(DIR)
}

/// The frame autosave name for a state.
pub fn frame_name(state: &WindowState) -> &'static str {
    if state.project.is_some() {
        PROJECT
    } else {
        WELCOME
    }
}

enum Msg {
    State(WindowState),
    /// A Dock reopen: the workspace is in another window, which got this
    /// frame autosave name when it was opened.
    Window(AnyWindowHandle, &'static str),
}

/// The sending end of the window state task. `Clone`; the task ends with
/// the last one.
#[derive(Clone)]
pub struct WindowStateLink {
    tx: UnboundedSender<Msg>,
    /// Whether the last reported state has a project, for the name a window
    /// opened later starts with.
    project: Arc<AtomicBool>,
}

impl WindowStateLink {
    /// Starts the task on the main thread. `dir` is where `window.json` is
    /// written; `window` is the window `workspace` was installed in, which
    /// already has the `welcome` frame autosave name. Does no IO and starts
    /// no thread: the first write is a task for `executor`.
    pub fn start(
        window: AnyWindowHandle,
        dir: PathBuf,
        executor: Executor,
        cx: &mut App,
    ) -> WindowStateLink {
        let (tx, rx) = unbounded();
        cx.spawn(async move |cx| run(rx, window, dir, executor, cx).await)
            .detach();
        WindowStateLink {
            tx,
            project: Arc::new(AtomicBool::new(false)),
        }
    }

    /// `WorkspaceDeps::on_state_changed`: the state changed. Main thread.
    pub fn state_changed(&self, state: WindowState) {
        self.project
            .store(state.project.is_some(), Ordering::Relaxed);
        // The task ends only with the app.
        let _ = self.tx.unbounded_send(Msg::State(state));
    }

    /// The frame autosave name a window opened now starts with: the one the
    /// last reported state has.
    pub fn frame_name(&self) -> &'static str {
        if self.project.load(Ordering::Relaxed) {
            PROJECT
        } else {
            WELCOME
        }
    }

    /// The workspace moved to `window`, which was opened with `name`
    /// ([`frame_name`](Self::frame_name)). Main thread.
    pub fn window_changed(&self, window: AnyWindowHandle, name: &'static str) {
        let _ = self.tx.unbounded_send(Msg::Window(window, name));
    }
}

async fn run(
    mut rx: futures::channel::mpsc::UnboundedReceiver<Msg>,
    mut window: AnyWindowHandle,
    dir: PathBuf,
    executor: Executor,
    cx: &mut gpui::AsyncApp,
) {
    // The name the window has now.
    let mut named = WELCOME;
    // The newest state not written yet.
    let mut latest: Option<WindowState> = None;
    loop {
        let msg = match latest {
            None => rx.next().await,
            Some(_) => {
                let timer = cx.background_executor().timer(DEBOUNCE);
                match select(rx.next(), timer).await {
                    Either::Left((msg, _)) => msg,
                    Either::Right(_) => {
                        if let Some(state) = latest.take() {
                            write_in_background(&executor, &dir, &state).await;
                        }
                        continue;
                    }
                }
            }
        };
        match msg {
            Some(Msg::State(state)) => {
                rename(cx, window, &mut named, frame_name(&state));
                latest = Some(state);
            }
            Some(Msg::Window(next, name)) => {
                window = next;
                named = name;
            }
            // Every sender is gone: the app is quitting.
            None => break,
        }
    }
}

/// Gives `window` the frame autosave name `wanted` unless it has it.
/// `platform` logs a failure; the window keeps its frame.
fn rename(
    cx: &mut gpui::AsyncApp,
    window: AnyWindowHandle,
    named: &mut &'static str,
    wanted: &'static str,
) {
    if *named == wanted {
        return;
    }
    *named = wanted;
    cx.update(|cx| {
        // A window that is gone has nothing to name.
        let _ = window.update(cx, |_, window, _| {
            let _ = lectern_platform::window::set_frame_autosave_name(&*window, wanted);
        });
    });
}

async fn write_in_background(executor: &Executor, dir: &Path, state: &WindowState) {
    let Ok(bytes) = serde_json::to_vec(state) else {
        // A project path that is not UTF-8.
        warn!("window state not written"; step = "encode");
        return;
    };
    let dir = dir.to_owned();
    let task = executor.run(Priority::Background, move || {
        if let Err((step, error)) = write(&dir, &bytes) {
            warn!("window state not written"; step = step.name(), io = Value::io(&error));
        }
    });
    // One write at a time: they share the temporary file. A cancelled task
    // (the executor is shutting down) has nothing more to do.
    let _ = task.await;
}

/// Reads `window.json` in a `Background` task and hands the state to
/// `workspace`. A missing file is the first run; a file that cannot be read
/// or whose `version` is unknown is ignored (the next change overwrites
/// it). Any thread; no path is logged.
pub fn restore(dir: PathBuf, executor: &Executor, host: WorkspaceHost) {
    executor
        .run(Priority::Background, move || match read(&dir) {
            Read::Missing => {}
            Read::Loaded(state) if state.is_supported() => host.restore(state),
            Read::Loaded(_) => {
                warn!("window state ignored"; reason = "version");
            }
            Read::Unreadable(Unreadable::Io(error)) => {
                warn!("window state unreadable"; reason = "io", io = Value::io(&error));
            }
            Read::Unreadable(Unreadable::Json) => {
                warn!("window state unreadable"; reason = "json");
            }
        })
        .detach();
}

/// What reading the file gave.
#[derive(Debug)]
pub(crate) enum Read {
    /// There is no file: the first run.
    Missing,
    Loaded(WindowState),
    Unreadable(Unreadable),
}

#[derive(Debug)]
pub(crate) enum Unreadable {
    Io(std::io::Error),
    /// Not JSON of the expected shape.
    Json,
}

/// The step of a write that failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WriteStep {
    CreateDir,
    Create,
    Write,
    Rename,
}

impl WriteStep {
    /// The value of the `step` log field.
    pub(crate) fn name(self) -> &'static str {
        match self {
            WriteStep::CreateDir => "create_dir",
            WriteStep::Create => "create",
            WriteStep::Write => "write",
            WriteStep::Rename => "rename",
        }
    }
}

/// Reads `dir/window.json`. Blocks: only in a `Background` task.
pub(crate) fn read(dir: &Path) -> Read {
    match std::fs::read(dir.join(FILE)) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(state) => Read::Loaded(state),
            Err(_) => Read::Unreadable(Unreadable::Json),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Read::Missing,
        Err(error) => Read::Unreadable(Unreadable::Io(error)),
    }
}

/// Writes `bytes` as `dir/window.json`: a temporary file next to it, then a
/// rename; creates `dir` and nothing else. Blocks: only in a `Background`
/// task.
pub(crate) fn write(dir: &Path, bytes: &[u8]) -> Result<(), (WriteStep, std::io::Error)> {
    std::fs::create_dir_all(dir).map_err(|e| (WriteStep::CreateDir, e))?;
    let temp = dir.join(TEMP);
    let mut file = std::fs::File::create(&temp).map_err(|e| (WriteStep::Create, e))?;
    file.write_all(bytes).map_err(|e| (WriteStep::Write, e))?;
    drop(file);
    std::fs::rename(&temp, dir.join(FILE)).map_err(|e| (WriteStep::Rename, e))
}
