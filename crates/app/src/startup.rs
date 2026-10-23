//! The startup barrier and what is started behind it (docs/design/app.md
//! 「启动顺序」「启动屏障」).

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, Global, Window};
use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::Watcher;
use lectern_kernel::{Kernel, PluginSet};
use lectern_logging::{Config, Early, Header, Logging, Value, info, warn};

use crate::profile::{Profile, Resources};

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
}

impl Global for Started {}

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
/// `Kernel::start` spawn their threads and return; `Executor::new`, the
/// cache, the watcher and the plugins spawn none and do no IO
/// (docs/design/app.md「创建顺序」). Only the first call does anything, and
/// nothing once quitting.
pub fn release(cx: &mut App) {
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
    let executor = Executor::new(lectern_executor::Config::default());
    let (set, cache, watcher) = assemble(profile, &executor);
    let kernel = Kernel::start(set);
    info!("startup barrier released"; profile = profile);
    cx.set_global(Started {
        logging,
        executor,
        kernel,
        cache,
        watcher,
    });
}

/// The profile's plugins and the resources they share. The test profile
/// assembles none and looks at no directory.
fn assemble(profile: Profile, executor: &Executor) -> (PluginSet, Option<Cache>, Option<Watcher>) {
    if profile == Profile::Test {
        return (PluginSet::new(), None, None);
    }
    match resources(executor) {
        Ok(resources) => {
            let (set, count) = profile.plugins(&resources);
            info!("plugins assembled"; profile = profile, plugins = count);
            (set, Some(resources.cache), Some(resources.watcher))
        }
        Err(missing) => {
            warn!("plugins not assembled"; missing = Value::text(missing));
            (PluginSet::new(), None, None)
        }
    }
}

/// The resources from the environment, or the name of the first one
/// missing. Reads environment variables only: the cache opens its files on
/// first use, the watcher starts its thread on the first watch, the
/// FSEvents backend arms nothing until then.
fn resources(executor: &Executor) -> Result<Resources, &'static str> {
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
    Ok(Resources {
        executor: executor.clone(),
        cache,
        watcher,
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
        window.on_next_frame(|_, cx| release(cx));
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
