//! The startup barrier and what is started behind it (docs/design/app.md
//! 「启动顺序」「启动屏障」).

use std::sync::Arc;

use gpui::{App, Global, Window};
use lectern_executor::Executor;
use lectern_kernel::Kernel;
use lectern_logging::{Config, Early, Header, Logging, info};

use crate::profile::Profile;

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
/// `Kernel::start` spawn their threads and return, `Executor::new` spawns
/// none. Only the first call does anything, and nothing once quitting.
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
    let kernel = Kernel::start(profile.plugins());
    info!("startup barrier released"; profile = profile);
    cx.set_global(Started {
        logging,
        executor,
        kernel,
    });
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
