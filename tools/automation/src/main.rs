//! `lectern` with gpui-mcp's bridge in each window (ADR 0056,
//! docs/design/app.md「自动化构建的装配」).

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{AnyWindowHandle, App, Global};
use gpui_mcp::{AppId, BridgeConfig, BridgeHandle, StartError};
use lectern_app::Hooks;
use lectern_logging::{Value, info, warn};

/// Where the endpoint is: a short directory, because a Unix socket path holds
/// at most 104 bytes on macOS. Set by `just automation`.
const ENDPOINTS: &str = "LECTERN_MCP_ENDPOINTS";

/// The bridge of the window in front. A Dock reopen replaces it: the old
/// window is closed by then.
struct Bridge(#[allow(dead_code, reason = "kept alive, never read")] BridgeHandle);

impl Global for Bridge {}

fn main() {
    lectern_app::run_with(Hooks {
        on_window: Some(Rc::new(install)),
    });
}

/// Main thread, after the first frame. The barrier calls it from inside the
/// window's frame callback, where the window cannot be updated: install once
/// that update is over.
fn install(window: AnyWindowHandle, cx: &mut App) {
    cx.defer(move |cx| install_now(window, cx));
}

fn install_now(window: AnyWindowHandle, cx: &mut App) {
    let Some(dir) = std::env::var_os(ENDPOINTS)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
    else {
        warn!("automation bridge failed"; error = "no-endpoint-dir");
        return;
    };
    let config = AppId::new("lectern").map_err(|_| "app-id").and_then(|id| {
        BridgeConfig::new(id, "Lectern")
            .endpoint_dir(dir)
            .map_err(|_| "endpoint-dir")
    });
    let config = match config {
        Ok(config) => config,
        Err(kind) => {
            warn!("automation bridge failed"; error = Value::text(kind));
            return;
        }
    };
    let installed = window.update(cx, |_, window, cx| {
        BridgeHandle::install(window, cx, config)
    });
    match installed {
        Ok(Ok(handle)) => {
            cx.set_global(Bridge(handle));
            info!("automation bridge installed");
        }
        Ok(Err(error)) => warn!("automation bridge failed"; error = Value::text(kind(&error))),
        Err(_) => warn!("automation bridge failed"; error = "window-closed"),
    }
}

/// The kind only: the messages carry paths.
fn kind(error: &StartError) -> &'static str {
    match error {
        StartError::InvalidConfig(_) => "invalid-config",
        StartError::InvalidProcessId => "invalid-pid",
        StartError::Io(_) => "io",
        StartError::Serialize(_) => "serialize",
        StartError::RuntimePath(_) => "runtime-path",
    }
}
