//! The window state kept across runs (docs/design/workspace.md「窗口状态」).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What is worth keeping across runs and cannot be recovered from elsewhere.
/// `app` stores it as JSON under `app_data()/workspace/window.json` and reads
/// it after the first frame; a `version` it does not know counts as no file.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct WindowState {
    pub version: u32,
    /// The path of the last open project (the one `Projects::open` received);
    /// None when the window stayed on Welcome.
    pub project: Option<PathBuf>,
    pub inspector: bool,
}

impl WindowState {
    /// The version this crate writes.
    pub const VERSION: u32 = 1;

    /// Whether this crate can restore from the state: the version is one it
    /// knows.
    pub fn is_supported(&self) -> bool {
        self.version == WindowState::VERSION
    }
}

impl Default for WindowState {
    /// Welcome, inspector hidden.
    fn default() -> Self {
        WindowState {
            version: WindowState::VERSION,
            project: None,
            inspector: false,
        }
    }
}
