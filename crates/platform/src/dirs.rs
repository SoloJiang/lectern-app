//! Where Lectern keeps its own files (「应用数据目录」 and 「日志目录与系统版本」
//! in docs/design/platform.md). Only paths: nothing here touches the file
//! system.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// The application data directory: user data that cannot be rebuilt, such
/// as review's annotation logs (ADR 0044) and later usage's database.
///
/// - macOS: `$HOME/Library/Application Support/Lectern`.
/// - Elsewhere (Linux, for CI): `$XDG_DATA_HOME/lectern`, or
///   `$HOME/.local/share/lectern` when `XDG_DATA_HOME` is unset, empty or
///   relative (the XDG Base Directory spec ignores relative paths).
///
/// None when the `HOME` it needs is unset, empty or relative. Reads only
/// these environment variables: it does not create the directory, does not
/// check that it exists and does no other IO, so any thread may call it.
pub fn app_data() -> Option<PathBuf> {
    let home = std::env::var_os("HOME");
    #[cfg(target_os = "macos")]
    {
        mac_app_data(home.as_deref())
    }
    #[cfg(not(target_os = "macos"))]
    {
        xdg_app_data(
            home.as_deref(),
            std::env::var_os("XDG_DATA_HOME").as_deref(),
        )
    }
}

/// [`app_data`] on macOS, from the value of `HOME`. Also compiled for tests
/// elsewhere: it only joins paths.
#[cfg(any(target_os = "macos", test))]
fn mac_app_data(home: Option<&OsStr>) -> Option<PathBuf> {
    Some(absolute(home)?.join("Library/Application Support/Lectern"))
}

/// [`app_data`] off macOS, from the values of `HOME` and `XDG_DATA_HOME`.
#[cfg(any(not(target_os = "macos"), test))]
fn xdg_app_data(home: Option<&OsStr>, data_home: Option<&OsStr>) -> Option<PathBuf> {
    let base = match absolute(data_home) {
        Some(dir) => dir.to_path_buf(),
        None => absolute(home)?.join(".local/share"),
    };
    Some(base.join("lectern"))
}

/// The log directory, where `lectern-logging` writes its files
/// (docs/design/logging.md「文件」, ADR 0046).
///
/// - macOS: `$HOME/Library/Logs/Lectern`.
/// - Elsewhere (Linux, for CI): `$XDG_STATE_HOME/lectern/logs`, or
///   `$HOME/.local/state/lectern/logs` when `XDG_STATE_HOME` is unset, empty
///   or relative.
///
/// None when the `HOME` it needs is unset, empty or relative. The same rules
/// as [`app_data`]: reads only these environment variables, creates nothing
/// and does no other IO, so `app` may call it before the first frame.
pub fn logs() -> Option<PathBuf> {
    let home = std::env::var_os("HOME");
    #[cfg(target_os = "macos")]
    {
        mac_logs(home.as_deref())
    }
    #[cfg(not(target_os = "macos"))]
    {
        xdg_logs(
            home.as_deref(),
            std::env::var_os("XDG_STATE_HOME").as_deref(),
        )
    }
}

/// [`logs`] on macOS, from the value of `HOME`. Also compiled for tests
/// elsewhere: it only joins paths.
#[cfg(any(target_os = "macos", test))]
fn mac_logs(home: Option<&OsStr>) -> Option<PathBuf> {
    Some(absolute(home)?.join("Library/Logs/Lectern"))
}

/// [`logs`] off macOS, from the values of `HOME` and `XDG_STATE_HOME`.
#[cfg(any(not(target_os = "macos"), test))]
fn xdg_logs(home: Option<&OsStr>, state_home: Option<&OsStr>) -> Option<PathBuf> {
    let base = match absolute(state_home) {
        Some(dir) => dir.to_path_buf(),
        None => absolute(home)?.join(".local/state"),
    };
    Some(base.join("lectern/logs"))
}

/// The variable's value as a path when it is an absolute one; unset, empty
/// and relative values count as unset.
fn absolute(value: Option<&OsStr>) -> Option<&Path> {
    value.map(Path::new).filter(|p| p.is_absolute())
}
