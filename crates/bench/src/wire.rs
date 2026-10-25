//! The report protocol between the runner and the probe
//! (docs/design/bench.md「报告协议」).
//!
//! One message per line over a Unix domain socket, UTF-8: the first word is
//! the message name, the rest are `key=value` pairs, such as
//! `first-frame pid=812 presented_unix_ns=1729000000000000000 source=presented`. A value is
//! percent-encoded: every byte outside printable ASCII (`!` to `~`) and `%`
//! itself becomes `%XX`, so spaces, newlines and the bytes of a path that is
//! not UTF-8 never break a line. Times are nanoseconds since the Unix epoch,
//! durations nanoseconds. A missing key is a missing optional field; an
//! unknown or repeated key is an error, because both ends are built from the
//! same commit.

use std::ffi::OsString;
use std::fmt;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use lectern_platform::frame::TimeSource;

#[cfg(test)]
mod tests;

/// The runner's socket, set for every launch. Without it the probe is off
/// and the program runs as usual.
pub const SOCKET_VAR: &str = "LECTERN_BENCH_SOCKET";
/// `closure:<id>` or `closure+self:<id>`: the plugin set is trimmed to the
/// dependency closure of `<id>`, without or with `<id>`.
pub const PLUGINS_VAR: &str = "LECTERN_BENCH_PLUGINS";

/// Probe → runner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// The first frame's time: its `presentedTime`, or the moment its
    /// presented handler ran where the machine gives none; `source` says
    /// which (ADR 0052).
    FirstFrame {
        pid: u32,
        presented: SystemTime,
        source: TimeSource,
    },
    /// The kernel became busy; `epoch` is the event's `seq`.
    Busy {
        epoch: u64,
    },
    /// The kernel became quiescent; `epoch` is the event's `seq`.
    Quiescent {
        epoch: u64,
    },
    /// Every selected plugin is `Active` (after trimming the plugin set, or
    /// in answer to `Request::ActivateAll`).
    Ready,
    /// The answer to `Request::Epoch`: `Kernel::quiescent_epoch()`.
    Epoch(Option<u64>),
    /// The answer to `Request::Plugins`, one per plugin, ended by
    /// `PluginsEnd`.
    Plugin(PluginReport),
    PluginsEnd,
    /// A selected plugin entered `Failed`.
    PluginFailed {
        id: String,
        message: String,
    },
    /// The frame that drew `milestone` was shown at `presented`. It has no
    /// time source yet: it gets one with its users, `frame::record` (B2) and
    /// bench step 5 (ADR 0052).
    Milestone {
        milestone: Milestone,
        presented: SystemTime,
    },
    Frames(FrameStats),
    /// A process Lectern started: the root of a plugin's or a language
    /// server's process group (ADR 0018).
    Child {
        pid: u32,
        kind: ChildKind,
        id: String,
    },
    Error {
        message: String,
    },
}

/// Runner → probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Epoch,
    Plugins,
    Open(OpenTarget),
    Quit,
    /// Activates every plugin of the profile, then `Ready`.
    ActivateAll,
}

/// One plugin in the answer to `Request::Plugins`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginReport {
    pub id: String,
    /// The name of its `PluginState`: `Inactive`, `Waiting`, `Active`,
    /// `Disabled` or `Failed`.
    pub state: String,
    /// Every activation condition of its manifest, in no particular order.
    pub activation: Vec<ActivationKind>,
    pub manifest: Option<Duration>,
    pub activate: Option<Duration>,
    pub commit: Option<Duration>,
    pub started: Option<Duration>,
}

/// The kind of an activation condition, without its patterns or command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationKind {
    AfterFirstFrame,
    Workspace,
    Command,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Milestone {
    ProjectHome,
    FileHighlighted,
    DiffShown,
    WorktreeSwitched,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    Project(PathBuf),
    File(PathBuf),
    Diff {
        session: String,
    },
    Worktree(PathBuf),
    Scroll,
    Animate,
    /// Keys injected into the focus at a fixed interval, through the same
    /// dispatch as real keyboard input.
    Input {
        text: String,
        interval: Duration,
    },
}

/// The frame times of one scroll, animation or input scenario, in frame
/// order. `without_tick` counts frames the display link did not start
/// (reported only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameStats {
    pub scenario: String,
    pub refresh: Duration,
    pub frames: Vec<FrameTimes>,
    pub without_tick: u32,
}

/// `presented` is None when the frame has no presented time. It has no time
/// source yet: like `platform`'s `FrameTimes::presented`, it gets one with
/// its users, `frame::record` (B2) and bench step 5 (ADR 0052).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTimes {
    pub requested: SystemTime,
    pub started: SystemTime,
    pub completed: SystemTime,
    pub presented: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildKind {
    ProcessPlugin,
    LanguageServer,
    Other,
}

/// A line that is not a valid message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireError {
    message: String,
}

impl WireError {
    fn new(message: impl Into<String>) -> WireError {
        WireError {
            message: message.into(),
        }
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WireError {}

impl Report {
    /// One line, without the newline.
    pub fn encode(&self) -> String {
        let mut line = Line::new(self.name());
        match self {
            Report::FirstFrame {
                pid,
                presented,
                source,
            } => {
                line.number("pid", *pid);
                line.time("presented_unix_ns", *presented);
                line.text("source", source.as_str());
            }
            Report::Busy { epoch } | Report::Quiescent { epoch } => line.number("epoch", *epoch),
            Report::Ready | Report::PluginsEnd => {}
            Report::Epoch(epoch) => {
                if let Some(epoch) = epoch {
                    line.number("value", *epoch);
                }
            }
            Report::Plugin(plugin) => {
                line.text("id", &plugin.id);
                line.text("state", &plugin.state);
                let kinds: Vec<&str> = plugin.activation.iter().map(|k| k.name()).collect();
                line.text("activation", &kinds.join(","));
                for (key, value) in [
                    ("manifest_ns", plugin.manifest),
                    ("activate_ns", plugin.activate),
                    ("commit_ns", plugin.commit),
                    ("started_ns", plugin.started),
                ] {
                    if let Some(value) = value {
                        line.duration(key, value);
                    }
                }
            }
            Report::PluginFailed { id, message } => {
                line.text("id", id);
                line.text("message", message);
            }
            Report::Milestone {
                milestone,
                presented,
            } => {
                line.text("kind", milestone.name());
                line.time("presented_unix_ns", *presented);
            }
            Report::Frames(stats) => {
                line.text("scenario", &stats.scenario);
                line.duration("refresh_ns", stats.refresh);
                line.number("without_tick", stats.without_tick);
                let times: Vec<String> = stats.frames.iter().map(encode_frame).collect();
                line.text("times", &times.join(";"));
            }
            Report::Child { pid, kind, id } => {
                line.number("pid", *pid);
                line.text("kind", kind.name());
                line.text("id", id);
            }
            Report::Error { message } => line.text("message", message),
        }
        line.finish()
    }

    pub fn decode(line: &str) -> Result<Report, WireError> {
        let mut fields = Fields::parse(line)?;
        let report = match fields.name {
            "first-frame" => Report::FirstFrame {
                pid: fields.number("pid")?,
                presented: fields.time("presented_unix_ns")?,
                source: time_source(&fields.text("source")?)?,
            },
            "busy" => Report::Busy {
                epoch: fields.number("epoch")?,
            },
            "quiescent" => Report::Quiescent {
                epoch: fields.number("epoch")?,
            },
            "ready" => Report::Ready,
            "epoch" => Report::Epoch(fields.optional_number("value")?),
            "plugin" => Report::Plugin(PluginReport {
                id: fields.text("id")?,
                state: fields.text("state")?,
                activation: decode_kinds(&fields.text("activation")?)?,
                manifest: fields.optional_duration("manifest_ns")?,
                activate: fields.optional_duration("activate_ns")?,
                commit: fields.optional_duration("commit_ns")?,
                started: fields.optional_duration("started_ns")?,
            }),
            "plugins-end" => Report::PluginsEnd,
            "plugin-failed" => Report::PluginFailed {
                id: fields.text("id")?,
                message: fields.text("message")?,
            },
            "milestone" => Report::Milestone {
                milestone: Milestone::from_name(&fields.text("kind")?)?,
                presented: fields.time("presented_unix_ns")?,
            },
            "frames" => Report::Frames(FrameStats {
                scenario: fields.text("scenario")?,
                refresh: fields.duration("refresh_ns")?,
                without_tick: fields.number("without_tick")?,
                frames: decode_frames(&fields.text("times")?)?,
            }),
            "child" => Report::Child {
                pid: fields.number("pid")?,
                kind: ChildKind::from_name(&fields.text("kind")?)?,
                id: fields.text("id")?,
            },
            "error" => Report::Error {
                message: fields.text("message")?,
            },
            other => return Err(WireError::new(format!("unknown report `{other}`"))),
        };
        fields.finish()?;
        Ok(report)
    }

    fn name(&self) -> &'static str {
        match self {
            Report::FirstFrame { .. } => "first-frame",
            Report::Busy { .. } => "busy",
            Report::Quiescent { .. } => "quiescent",
            Report::Ready => "ready",
            Report::Epoch(_) => "epoch",
            Report::Plugin(_) => "plugin",
            Report::PluginsEnd => "plugins-end",
            Report::PluginFailed { .. } => "plugin-failed",
            Report::Milestone { .. } => "milestone",
            Report::Frames(_) => "frames",
            Report::Child { .. } => "child",
            Report::Error { .. } => "error",
        }
    }
}

impl Request {
    /// One line, without the newline.
    pub fn encode(&self) -> String {
        let mut line = Line::new(match self {
            Request::Epoch => "epoch",
            Request::Plugins => "plugins",
            Request::Open(_) => "open",
            Request::Quit => "quit",
            Request::ActivateAll => "activate-all",
        });
        if let Request::Open(target) = self {
            match target {
                OpenTarget::Project(path) => {
                    line.text("target", "project");
                    line.path("path", path);
                }
                OpenTarget::File(path) => {
                    line.text("target", "file");
                    line.path("path", path);
                }
                OpenTarget::Diff { session } => {
                    line.text("target", "diff");
                    line.text("session", session);
                }
                OpenTarget::Worktree(path) => {
                    line.text("target", "worktree");
                    line.path("path", path);
                }
                OpenTarget::Scroll => line.text("target", "scroll"),
                OpenTarget::Animate => line.text("target", "animate"),
                OpenTarget::Input { text, interval } => {
                    line.text("target", "input");
                    line.text("text", text);
                    line.duration("interval_ns", *interval);
                }
            }
        }
        line.finish()
    }

    pub fn decode(line: &str) -> Result<Request, WireError> {
        let mut fields = Fields::parse(line)?;
        let request = match fields.name {
            "epoch" => Request::Epoch,
            "plugins" => Request::Plugins,
            "quit" => Request::Quit,
            "activate-all" => Request::ActivateAll,
            "open" => Request::Open(match fields.text("target")?.as_str() {
                "project" => OpenTarget::Project(fields.path("path")?),
                "file" => OpenTarget::File(fields.path("path")?),
                "diff" => OpenTarget::Diff {
                    session: fields.text("session")?,
                },
                "worktree" => OpenTarget::Worktree(fields.path("path")?),
                "scroll" => OpenTarget::Scroll,
                "animate" => OpenTarget::Animate,
                "input" => OpenTarget::Input {
                    text: fields.text("text")?,
                    interval: fields.duration("interval_ns")?,
                },
                other => return Err(WireError::new(format!("unknown open target `{other}`"))),
            }),
            other => return Err(WireError::new(format!("unknown request `{other}`"))),
        };
        fields.finish()?;
        Ok(request)
    }
}

/// `presented` or `handler` ([`TimeSource::as_str`]).
fn time_source(name: &str) -> Result<TimeSource, WireError> {
    [TimeSource::Presented, TimeSource::Handler]
        .into_iter()
        .find(|s| s.as_str() == name)
        .ok_or_else(|| WireError::new(format!("unknown time source `{name}`")))
}

impl ActivationKind {
    pub fn name(self) -> &'static str {
        match self {
            ActivationKind::AfterFirstFrame => "after_first_frame",
            ActivationKind::Workspace => "workspace",
            ActivationKind::Command => "command",
            ActivationKind::Manual => "manual",
        }
    }

    fn from_name(name: &str) -> Result<ActivationKind, WireError> {
        [
            ActivationKind::AfterFirstFrame,
            ActivationKind::Workspace,
            ActivationKind::Command,
            ActivationKind::Manual,
        ]
        .into_iter()
        .find(|k| k.name() == name)
        .ok_or_else(|| WireError::new(format!("unknown activation `{name}`")))
    }
}

impl Milestone {
    pub fn name(self) -> &'static str {
        match self {
            Milestone::ProjectHome => "project_home",
            Milestone::FileHighlighted => "file_highlighted",
            Milestone::DiffShown => "diff_shown",
            Milestone::WorktreeSwitched => "worktree_switched",
        }
    }

    fn from_name(name: &str) -> Result<Milestone, WireError> {
        [
            Milestone::ProjectHome,
            Milestone::FileHighlighted,
            Milestone::DiffShown,
            Milestone::WorktreeSwitched,
        ]
        .into_iter()
        .find(|m| m.name() == name)
        .ok_or_else(|| WireError::new(format!("unknown milestone `{name}`")))
    }
}

impl ChildKind {
    pub fn name(self) -> &'static str {
        match self {
            ChildKind::ProcessPlugin => "process_plugin",
            ChildKind::LanguageServer => "language_server",
            ChildKind::Other => "other",
        }
    }

    fn from_name(name: &str) -> Result<ChildKind, WireError> {
        [
            ChildKind::ProcessPlugin,
            ChildKind::LanguageServer,
            ChildKind::Other,
        ]
        .into_iter()
        .find(|k| k.name() == name)
        .ok_or_else(|| WireError::new(format!("unknown child kind `{name}`")))
    }
}

fn decode_kinds(value: &str) -> Result<Vec<ActivationKind>, WireError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value.split(',').map(ActivationKind::from_name).collect()
}

/// `requested,started,completed,presented` in Unix nanoseconds; `-` for a
/// frame that was not shown.
fn encode_frame(frame: &FrameTimes) -> String {
    let presented = frame
        .presented
        .map_or_else(|| "-".to_owned(), |t| unix_ns(t).to_string());
    format!(
        "{},{},{},{presented}",
        unix_ns(frame.requested),
        unix_ns(frame.started),
        unix_ns(frame.completed)
    )
}

fn decode_frames(value: &str) -> Result<Vec<FrameTimes>, WireError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(';')
        .map(|frame| {
            let parts: Vec<&str> = frame.split(',').collect();
            let [requested, started, completed, presented] = parts.as_slice() else {
                return Err(WireError::new(format!("bad frame `{frame}`")));
            };
            Ok(FrameTimes {
                requested: parse_time(requested)?,
                started: parse_time(started)?,
                completed: parse_time(completed)?,
                presented: match *presented {
                    "-" => None,
                    t => Some(parse_time(t)?),
                },
            })
        })
        .collect()
}

/// Nanoseconds since the Unix epoch; a time before it is 0 (no clock the
/// probe reads gives one).
fn unix_ns(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

fn parse_time(text: &str) -> Result<SystemTime, WireError> {
    let ns: u64 = text
        .parse()
        .map_err(|_| WireError::new(format!("bad time `{text}`")))?;
    Ok(UNIX_EPOCH + Duration::from_nanos(ns))
}

/// Writes one line.
struct Line(String);

impl Line {
    fn new(name: &str) -> Line {
        Line(name.to_owned())
    }

    fn bytes(&mut self, key: &str, value: &[u8]) {
        self.0.push(' ');
        self.0.push_str(key);
        self.0.push('=');
        for &byte in value {
            if byte.is_ascii_graphic() && byte != b'%' {
                self.0.push(char::from(byte));
            } else {
                self.0.push_str(&format!("%{byte:02X}"));
            }
        }
    }

    fn text(&mut self, key: &str, value: &str) {
        self.bytes(key, value.as_bytes());
    }

    fn path(&mut self, key: &str, value: &std::path::Path) {
        self.bytes(key, value.as_os_str().as_bytes());
    }

    fn number(&mut self, key: &str, value: impl fmt::Display) {
        self.text(key, &value.to_string());
    }

    fn time(&mut self, key: &str, value: SystemTime) {
        self.number(key, unix_ns(value));
    }

    fn duration(&mut self, key: &str, value: Duration) {
        self.number(key, value.as_nanos());
    }

    fn finish(self) -> String {
        self.0
    }
}

/// The parsed fields of one line; each is taken once, and `finish` rejects
/// the ones left.
struct Fields<'a> {
    name: &'a str,
    values: Vec<(&'a str, Vec<u8>)>,
}

impl<'a> Fields<'a> {
    fn parse(line: &'a str) -> Result<Fields<'a>, WireError> {
        let mut words = line.split(' ');
        let name = words.next().unwrap_or_default();
        if name.is_empty() {
            return Err(WireError::new("empty line"));
        }
        let mut values: Vec<(&str, Vec<u8>)> = Vec::new();
        for word in words {
            let Some((key, value)) = word.split_once('=') else {
                return Err(WireError::new(format!("`{word}` is not key=value")));
            };
            if values.iter().any(|(k, _)| *k == key) {
                return Err(WireError::new(format!("key `{key}` twice")));
            }
            values.push((key, unescape(value)?));
        }
        Ok(Fields { name, values })
    }

    fn take(&mut self, key: &str) -> Option<Vec<u8>> {
        let index = self.values.iter().position(|(k, _)| *k == key)?;
        Some(self.values.remove(index).1)
    }

    fn bytes(&mut self, key: &str) -> Result<Vec<u8>, WireError> {
        self.take(key)
            .ok_or_else(|| WireError::new(format!("`{}` has no `{key}`", self.name)))
    }

    fn text(&mut self, key: &str) -> Result<String, WireError> {
        String::from_utf8(self.bytes(key)?)
            .map_err(|_| WireError::new(format!("`{key}` is not UTF-8")))
    }

    fn path(&mut self, key: &str) -> Result<PathBuf, WireError> {
        Ok(PathBuf::from(OsString::from_vec(self.bytes(key)?)))
    }

    fn number<T: std::str::FromStr>(&mut self, key: &str) -> Result<T, WireError> {
        let text = self.text(key)?;
        text.parse()
            .map_err(|_| WireError::new(format!("`{key}` is not a number: `{text}`")))
    }

    fn optional_number<T: std::str::FromStr>(&mut self, key: &str) -> Result<Option<T>, WireError> {
        if self.values.iter().any(|(k, _)| *k == key) {
            self.number(key).map(Some)
        } else {
            Ok(None)
        }
    }

    fn time(&mut self, key: &str) -> Result<SystemTime, WireError> {
        parse_time(&self.text(key)?)
    }

    fn duration(&mut self, key: &str) -> Result<Duration, WireError> {
        self.number(key).map(Duration::from_nanos)
    }

    fn optional_duration(&mut self, key: &str) -> Result<Option<Duration>, WireError> {
        Ok(self.optional_number(key)?.map(Duration::from_nanos))
    }

    fn finish(self) -> Result<(), WireError> {
        match self.values.first() {
            None => Ok(()),
            Some((key, _)) => Err(WireError::new(format!(
                "`{}` has an unknown key `{key}`",
                self.name
            ))),
        }
    }
}

/// Undoes the percent-encoding. A `%` not followed by two hex digits is an
/// error.
fn unescape(value: &str) -> Result<Vec<u8>, WireError> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| WireError::new(format!("bad percent-encoding in `{value}`")))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}
