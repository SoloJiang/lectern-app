//! The process's `log::Log`: Lectern records from the macros and
//! third-party records from crates that call `log` directly (GPUI, gix).

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use log::{Level, Log, Metadata, Record};

use super::encode::{self, Head, Message};
use super::limit::{self, Decision, Table};
use crate::callsite::{self, Payload, WINDOW_US};
use crate::{clock, filter, scope};

/// Where encoded lines go.
#[allow(
    clippy::large_enum_variant,
    reason = "one per process, leaked; boxing the sink would add an indirection to every record"
)]
pub(crate) enum Output {
    /// `testing::capture()`.
    #[cfg(feature = "test-support")]
    Capture(std::sync::Mutex<Vec<String>>),
    /// `install_early`.
    #[cfg(feature = "sink")]
    Sink(crate::sink::State),
}

/// The installed logger's state. Leaked when installed: it lives as long
/// as the process.
pub(crate) struct Shared {
    /// `HOME`, without a trailing `/`: written as `~`.
    pub(crate) home: Option<Box<str>>,
    pub(crate) table: Table,
    /// Records dropped: a full buffer, a busy encoder, a failed write.
    pub(crate) dropped: AtomicU64,
    pub(crate) output: Output,
}

static CURRENT: AtomicPtr<Shared> = AtomicPtr::new(std::ptr::null_mut());
/// Set by the first `install_early` or `testing::capture`.
pub(crate) static INSTALLED: AtomicBool = AtomicBool::new(false);

struct Dispatch;
static DISPATCH: Dispatch = Dispatch;

pub(crate) fn current() -> Option<&'static Shared> {
    // SAFETY: CURRENT is null or a pointer from `Box::leak` in `install`,
    // never freed.
    unsafe { CURRENT.load(Ordering::Acquire).as_ref() }
}

/// Makes `shared` the logger of this process. The caller has set
/// [`INSTALLED`] and the active filter.
pub(crate) fn install(shared: Shared) -> &'static Shared {
    let shared: &'static Shared = Box::leak(Box::new(shared));
    // A second `set_logger` (tests reinstalling) fails and keeps `DISPATCH`,
    // which reads CURRENT.
    let _ = log::set_logger(&DISPATCH);
    CURRENT.store((shared as *const Shared).cast_mut(), Ordering::Release);
    shared
}

/// Removes the logger (tests only; the old state is leaked).
#[cfg(all(test, feature = "sink"))]
pub(crate) fn uninstall() {
    CURRENT.store(std::ptr::null_mut(), Ordering::Release);
    filter::deactivate();
    INSTALLED.store(false, Ordering::Release);
}

impl Log for Dispatch {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let (target, lectern) = third_party_target(metadata.target());
        current().is_some() && passes(metadata.level(), target, lectern)
    }

    fn log(&self, record: &Record<'_>) {
        if let Some(shared) = current() {
            callsite::ours(record, |payload| match payload {
                Some(payload) => shared.lectern(payload),
                None => shared.third_party(record),
            });
        }
    }

    fn flush(&self) {}
}

/// The first segment of a third-party target, and whether it is a Lectern
/// crate (`lectern_cache::store` → `cache`, true).
fn third_party_target(target: &str) -> (&str, bool) {
    let first = target.find("::").map_or(target, |i| &target[..i]);
    match first.strip_prefix("lectern_") {
        Some(rest) if !rest.is_empty() => (rest, true),
        _ => (first, false),
    }
}

fn passes(level: Level, target: &str, lectern: bool) -> bool {
    let Some(active) = filter::active() else {
        return false;
    };
    let allowed = if lectern {
        active.filter.lectern(target)
    } else {
        active.filter.third_party(target)
    };
    level <= allowed || (filter::PLUGINS.load(Ordering::Relaxed) && filter::plugin_allows(level))
}

thread_local! {
    /// The first line of a formatted third-party message.
    static TEXT: RefCell<[u8; super::sanitize::INPUT]> = const { RefCell::new([0; super::sanitize::INPUT]) };
}

/// Captures formatted text up to its first line break or the buffer's end.
struct FirstLine<'a> {
    buf: &'a mut [u8],
    len: usize,
    cut: bool,
    done: bool,
}

impl fmt::Write for FirstLine<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.done {
            return Err(fmt::Error);
        }
        let (s, line_ends) = match s.find(['\n', '\r']) {
            Some(i) => (&s[..i], true),
            None => (s, false),
        };
        let room = self.buf.len() - self.len;
        let part = encode::cut(s, room);
        self.buf[self.len..self.len + part.len()].copy_from_slice(part.as_bytes());
        self.len += part.len();
        if part.len() < s.len() {
            self.cut = true;
            self.done = true;
        } else if line_ends {
            self.done = true;
        }
        if self.done { Err(fmt::Error) } else { Ok(()) }
    }
}

impl Shared {
    fn home(&self) -> Option<&str> {
        self.home.as_deref()
    }

    #[cfg(feature = "sink")]
    pub(crate) fn sink(&self) -> Option<&crate::sink::State> {
        match &self.output {
            Output::Sink(state) => Some(state),
            #[allow(unreachable_patterns, reason = "Capture exists only with test-support")]
            _ => None,
        }
    }

    fn lectern(&self, payload: &Payload<'_>) {
        let callsite = payload.callsite;
        let head = Head {
            t: clock::now(),
            up: payload.up.unwrap_or_else(clock::up_micros),
            level: callsite.level,
            target: callsite.target,
            file: Some(callsite.file),
            line: Some(callsite.line),
            ctx: scope::current(),
        };
        let done = encode::with_line(|out| {
            encode::record(
                out,
                &head,
                Message::Literal(callsite.message),
                payload.fields,
                payload.n,
                self.home(),
            );
            self.deliver(callsite.level, out.bytes());
        });
        if done.is_none() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn third_party(&self, record: &Record<'_>) {
        let level = record.level();
        let (target, lectern) = third_party_target(record.target());
        if !passes(level, target, lectern) {
            return;
        }
        let n = if level <= Level::Info {
            match self.table.check(
                limit::key(record.target(), record.file(), record.line()),
                level,
                clock::window(WINDOW_US),
            ) {
                Decision::Write { n } => n,
                Decision::Limited => return,
            }
        } else {
            0
        };
        let head = Head {
            t: clock::now(),
            up: clock::up_micros(),
            level,
            target,
            file: record.file(),
            line: record.line(),
            ctx: scope::current(),
        };
        let done = match record.args().as_str() {
            Some(text) => self.encode_third_party(&head, Message::FreeLiteral(text), n),
            None => TEXT
                .try_with(|buf| {
                    let mut buf = buf.try_borrow_mut().ok()?;
                    let mut line = FirstLine {
                        buf: &mut buf[..],
                        len: 0,
                        cut: false,
                        done: false,
                    };
                    let _ = fmt::write(&mut line, *record.args());
                    let (len, cut) = (line.len, line.cut);
                    let text = std::str::from_utf8(&buf[..len]).unwrap_or("");
                    self.encode_third_party(&head, Message::FreeFormatted { text, cut }, n)
                })
                .ok()
                .flatten(),
        };
        if done.is_none() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn encode_third_party(&self, head: &Head<'_>, message: Message<'_>, n: u64) -> Option<()> {
        encode::with_line(|out| {
            encode::record(out, head, message, &[], n, self.home());
            self.deliver(head.level, out.bytes());
        })
    }

    /// Hands an encoded line to the output.
    pub(crate) fn deliver(&self, level: Level, line: &[u8]) {
        match &self.output {
            #[cfg(feature = "test-support")]
            Output::Capture(lines) => {
                let line = String::from_utf8_lossy(line)
                    .trim_end_matches('\n')
                    .to_owned();
                lines.lock().unwrap_or_else(|e| e.into_inner()).push(line);
            }
            #[cfg(feature = "sink")]
            Output::Sink(state) => {
                if !state.push(level, line) {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        let _ = level;
    }
}
