//! The probe's wire protocol: JSON Lines, one request and one response per
//! line (docs/design/ui_probe.md「协议类型」). Pure types and validators, no
//! GPUI and no IO except `check_dir`'s one `is_dir`.

use std::path::{Path, PathBuf};

use lectern_logging::{Field, Value};
use serde::{Deserialize, Serialize};

/// The protocol version a request must carry in `v`.
pub const PROTOCOL: u32 = 1;
/// The schema of the `ui_tree` result.
pub const TREE_SCHEMA: &str = "ui-tree/1";
/// A request line is at most this long; a longer one closes the connection.
pub const MAX_LINE: usize = 1 << 20;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub v: u32,
    /// The client's sequence number, written back unchanged.
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub v: u32,
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

#[derive(Debug, Serialize)]
pub struct Error {
    pub kind: ErrorKind,
    /// A fixed text: no path and no text of the UI.
    pub message: &'static str,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    BadRequest,
    UnknownMethod,
    NoWindow,
    Timeout,
    Render,
    Io,
    Quitting,
}

impl ErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorKind::BadRequest => "bad-request",
            ErrorKind::UnknownMethod => "unknown-method",
            ErrorKind::NoWindow => "no-window",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Render => "render",
            ErrorKind::Io => "io",
            ErrorKind::Quitting => "quitting",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            ErrorKind::BadRequest => "the request is not valid",
            ErrorKind::UnknownMethod => "the method is not known",
            ErrorKind::NoWindow => "the application has no window",
            ErrorKind::Timeout => "no frame was drawn in time",
            ErrorKind::Render => "the window could not be rendered to an image",
            ErrorKind::Io => "the file could not be written",
            ErrorKind::Quitting => "the application is quitting",
        }
    }
}

impl Field for ErrorKind {
    fn value(&self) -> Value<'_> {
        Value::text(self.as_str())
    }
}

impl Response {
    pub fn ok(id: u64, result: serde_json::Value) -> Response {
        Response {
            v: PROTOCOL,
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, kind: ErrorKind) -> Response {
        Response {
            v: PROTOCOL,
            id,
            ok: false,
            result: None,
            error: Some(Error {
                kind,
                message: kind.message(),
            }),
        }
    }

    /// One line, with the newline.
    pub fn to_line(&self) -> Vec<u8> {
        // Serialising these types cannot fail: every key is a string and
        // every value is a number, a string or a `serde_json::Value`.
        let mut line = serde_json::to_vec(self).unwrap_or_default();
        line.push(b'\n');
        line
    }
}

/// Parses one request line. `Err` is the `bad-request` response to write
/// back: it carries the request's `id` when the line is JSON with a numeric
/// `id`, and 0 otherwise.
pub fn parse_request(line: &[u8]) -> Result<Request, Response> {
    let bad = |id: u64| Response::err(id, ErrorKind::BadRequest);
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
        return Err(bad(0));
    };
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let request: Request = serde_json::from_value(value).map_err(|_| bad(id))?;
    if request.v != PROTOCOL {
        return Err(bad(request.id));
    }
    Ok(request)
}

/// `window`, the only parameter of `ui_tree`.
#[derive(Debug, Default, Deserialize)]
pub struct TreeParams {
    #[serde(default)]
    pub window: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ScreenshotParams {
    #[serde(default)]
    pub window: Option<u64>,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub annotate: Option<bool>,
}

/// `[A-Za-z0-9._-]+`, and not `.` or `..`.
pub fn check_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// An absolute path of an existing directory.
pub fn check_dir(dir: &str) -> Option<PathBuf> {
    let path = Path::new(dir);
    (path.is_absolute() && path.is_dir()).then(|| path.to_path_buf())
}
