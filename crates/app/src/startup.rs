//! The startup barrier and what is started behind it (docs/design/app.md
//! 「启动顺序」「启动屏障」).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AnyWindowHandle, App, Global, Window};
use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::Watcher;
use lectern_kernel::{Kernel, PluginSet};
use lectern_logging::{Config, Early, Header, Logging, Value, info, warn};
use lectern_workspace::{WorkspaceDeps, WorkspaceHost};

use crate::profile::{Profile, Resources};
use crate::window_state::{self, WindowStateLink};

/// Before the barrier: what `main` set up before the first frame. A GPUI
/// global, taken by the first `release`.
pub struct Startup {
    early: Option<Early>,
    profile: Profile,
}

impl Global for Startup {}

/// The process-wide resources created behind the barrier (ADR 0025). Views
/// only read it; `Logging::export` runs in an executor Background task.
#[derive(Clone)]
pub struct Started {
    pub logging: Logging,
    pub executor: Executor,
    pub kernel: Arc<Kernel>,
    /// None when no plugins were assembled (a directory missing, or the
    /// test profile).
    pub cache: Option<Cache>,
    pub watcher: Option<Watcher>,
    /// The workspace in the window; None when no plugins were assembled.
    pub shell: Option<Shell>,
}

impl Global for Started {}

/// The workspace in the window: `Quit` asks its leave guards, a Dock reopen
/// attaches it to the new window and names that window's frame.
#[derive(Clone)]
pub struct Shell {
    pub host: WorkspaceHost,
    pub window_state: WindowStateLink,
}

impl Startup {
    pub fn new(early: Early, profile: Profile) -> Startup {
        Startup {
            early: Some(early),
            profile,
        }
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }
}

/// Releases the barrier. Main thread, does not block: `Early::start` and
/// `Kernel::start` spawn their threads and return; `lectern_ui::ready`,
/// `workspace::install`, `Executor::new`, the cache, the watcher and the
/// plugins spawn none and do no IO (docs/design/app.md「创建顺序」). Only the
/// first call does anything, and nothing once quitting. The test profile has
/// no window to give `workspace` and installs none; the desktop barrier calls
/// [`release_in`].
pub fn release(cx: &mut App) {
    release_in(None, cx);
}

/// [`release`] with the window the barrier fired in: `workspace` takes over
/// its root view.
pub fn release_in(window: Option<AnyWindowHandle>, cx: &mut App) {
    if crate::quit::is_quitting(cx) {
        return;
    }
    let Some(startup) = cx.try_global::<Startup>() else {
        return;
    };
    let profile = startup.profile;
    let Some(early) = cx.global_mut::<Startup>().early.take() else {
        return;
    };
    let logging = early.start(Config {
        header: header(),
        // No QoS interface in `platform` yet (docs/design/app.md「范围」).
        on_writer_start: None,
    });
    // gpui-component's init and the system colours, before any plugin can
    // mount a view (docs/design/ui.md「对其他 crate 的要求」). Main thread,
    // about 2–3 ms, no IO. A no-op after `init_fixed` (the test profile).
    lectern_ui::ready(cx);
    let executor = Executor::new(lectern_executor::Config::default());
    // `workspace::install` is in here: after `ready`, before the kernel.
    let assembled = assemble(profile, &executor, window, cx);
    let kernel = Kernel::start(assembled.set);
    // The window state file is read behind the barrier, in an executor task
    // (docs/design/workspace.md「对其他 crate 的要求」E).
    if let (Some(shell), Some(dir)) = (&assembled.shell, assembled.state_dir) {
        window_state::restore(dir, &executor, shell.host.clone());
    }
    info!("startup barrier released"; profile = profile);
    cx.set_global(Started {
        logging,
        executor,
        kernel,
        cache: assembled.cache,
        watcher: assembled.watcher,
        shell: assembled.shell,
    });
}

/// What `assemble` made.
struct Assembled {
    set: PluginSet,
    cache: Option<Cache>,
    watcher: Option<Watcher>,
    shell: Option<Shell>,
    /// Where `window.json` is, when the workspace was installed.
    state_dir: Option<PathBuf>,
}

impl Assembled {
    fn none() -> Assembled {
        Assembled {
            set: PluginSet::new(),
            cache: None,
            watcher: None,
            shell: None,
            state_dir: None,
        }
    }
}

/// The profile's plugins and the resources they share. The test profile
/// assembles none and looks at no directory. The desktop profile installs
/// `workspace` in `window` once its directories are known, so a run without
/// plugins keeps the neutral skeleton.
fn assemble(
    profile: Profile,
    executor: &Executor,
    window: Option<AnyWindowHandle>,
    cx: &mut App,
) -> Assembled {
    if profile == Profile::Test {
        return Assembled::none();
    }
    let Some(window) = window else {
        warn!("plugins not assembled"; missing = "window");
        return Assembled::none();
    };
    let mut shell = None;
    let mut state_dir = None;
    let resources = resources(executor, |app_data| {
        let dir = window_state::dir(app_data);
        let link = WindowStateLink::start(window, dir.clone(), executor.clone(), cx);
        let changed = link.clone();
        let host = lectern_workspace::install(
            window,
            WorkspaceDeps {
                on_state_changed: Arc::new(move |state| changed.state_changed(state)),
                // The bench probe (docs/design/bench.md) is not there yet:
                // it takes the milestones and the frame records here.
                on_milestone: Arc::new(|_, _| {}),
                on_frames: Arc::new(|_| {}),
            },
            cx,
        );
        state_dir = Some(dir);
        shell = Some(Shell {
            host: host.clone(),
            window_state: link,
        });
        host
    });
    match resources {
        Ok(resources) => {
            let (set, count) = profile.plugins(&resources);
            info!("plugins assembled"; profile = profile, plugins = count);
            Assembled {
                set,
                cache: Some(resources.cache),
                watcher: Some(resources.watcher),
                shell,
                state_dir,
            }
        }
        Err(missing) => {
            warn!("plugins not assembled"; missing = Value::text(missing));
            Assembled::none()
        }
    }
}

/// The resources from the environment, or the name of the first one
/// missing. Reads environment variables only: the cache opens its files on
/// first use, the watcher starts its thread on the first watch, the
/// FSEvents backend arms nothing until then. `workspace` runs once the
/// directories are there, with the application data directory, and gives
/// the handle the "workspace" plugin takes.
fn resources(
    executor: &Executor,
    workspace: impl FnOnce(&Path) -> WorkspaceHost,
) -> Result<Resources, &'static str> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or("home")?;
    let app_cache = lectern_platform::dirs::app_cache().ok_or("app_cache")?;
    let app_data = lectern_platform::dirs::app_data().ok_or("app_data")?;
    // As `project`'s `LecternGit::from_env` reads it.
    let git_ceilings: Vec<PathBuf> = std::env::var_os("GIT_CEILING_DIRECTORIES")
        .map(|value| {
            std::env::split_paths(&value)
                .filter(|path| path.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    let cache = Cache::new(lectern_cache::Config {
        root: app_cache.join("cache"),
        budgets: lectern_cache::Budgets::default(),
        // `release` runs on the main thread.
        ui_thread: Some(std::thread::current().id()),
    });
    let watcher = Watcher::new(
        lectern_platform::fs_events::backend(executor.clone()),
        executor.clone(),
        lectern_fs_watch::Config::default(),
    );
    let workspace = workspace(&app_data);
    Ok(Resources {
        executor: executor.clone(),
        cache,
        watcher,
        workspace,
        home,
        app_cache,
        app_data,
        git_ceilings: git_ceilings.into(),
    })
}

/// Arms the desktop barrier from the root view's first `render`: released
/// on the second frame request after the first frame is drawn, by when the
/// first frame has been presented.
pub fn arm(window: &mut Window) {
    window.on_next_frame(|window, _| {
        info!("first frame drawn");
        window.on_next_frame(|window, cx| release_in(Some(window.window_handle()), cx));
    });
}

fn header() -> Header {
    Header {
        version: env!("CARGO_PKG_VERSION"),
        // Set by the packaging build (#38); local builds say `dev`.
        build: option_env!("LECTERN_BUILD").unwrap_or("dev"),
        // One `sysctl`, no file read.
        os: lectern_platform::system::os_version().unwrap_or_else(|| "unknown".to_owned()),
        // As `uname -m` spells it on macOS.
        arch: match std::env::consts::ARCH {
            "aarch64" => "arm64",
            other => other,
        },
    }
}
