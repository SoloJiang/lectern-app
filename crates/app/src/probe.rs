//! The probe profile (docs/design/ui_probe.md「对其他 crate 的要求」): only
//! compiled with the `ui-probe` feature. `run` reads one environment variable
//! before the first frame; the session starts at the end of
//! `startup::release`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::App;
use lectern_logging::{Value, warn};
use lectern_ui_probe::server::{Config, Probe};

use crate::startup::Startup;

/// The probe directory the client made.
const DIR_VAR: &str = "LECTERN_UI_PROBE_DIR";

/// `startup` for the probe profile when the variable is set (and not empty),
/// otherwise `startup` itself. Memory only: before the first frame nothing
/// else is read, no directory is looked at.
pub fn select(startup: Startup) -> Startup {
    match dir_from(std::env::var_os(DIR_VAR)) {
        Some(dir) => startup.probe(dir),
        None => startup,
    }
}

/// The directory a value of the variable names; an empty value is no value.
fn dir_from(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// Starts the session in `dir` (None: not the probe profile). Main thread,
/// does not block. A failure is logged and the application runs without the
/// probe; the record carries the kind of the failure, never the directory.
pub fn start(dir: Option<PathBuf>, cx: &mut App) -> Option<Arc<Probe>> {
    let dir = dir?;
    match Probe::start(cx, Config::new(dir)) {
        Ok(probe) => Some(Arc::new(probe)),
        Err(e) => {
            match e.io() {
                Some(io) => {
                    warn!("probe start failed"; reason = Value::text(e.reason()), error = Value::io(io));
                }
                None => warn!("probe start failed"; reason = Value::text(e.reason())),
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::dir_from;

    #[test]
    fn the_variable_names_the_probe_directory() {
        assert_eq!(dir_from(None), None);
        assert_eq!(dir_from(Some(OsString::new())), None);
        assert_eq!(
            dir_from(Some(OsString::from("/tmp/probe"))),
            Some(PathBuf::from("/tmp/probe"))
        );
    }
}
