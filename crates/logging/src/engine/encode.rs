//! One record as one JSON line of at most 4 KiB (docs/design/logging.md
//! 「记录」), encoded on the calling thread into a reused thread-local
//! buffer without allocating.

use std::cell::RefCell;
use std::fmt::{self, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use log::Level;

use super::{sanitize, time};
use crate::scope::Ctx;
use crate::value::{PanicText, Repr, Value, url_parts, write_uuid};

/// The longest line, newline included.
pub(crate) const MAX_LINE: usize = 4096;
/// Room kept for `,"n":…,"trunc":true}` and the newline.
const SUFFIX_RESERVE: usize = 48;

/// A bounded output buffer. A write that does not fit sets `full` and
/// writes nothing; the caller rewinds to a mark.
pub(crate) struct Out<'b> {
    buf: &'b mut [u8],
    len: usize,
    limit: usize,
    full: bool,
}

impl<'b> Out<'b> {
    pub(crate) fn new(buf: &'b mut [u8]) -> Out<'b> {
        let limit = buf.len();
        Out {
            buf,
            len: 0,
            limit,
            full: false,
        }
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub(crate) fn raw(&mut self, b: &[u8]) {
        if self.full || self.len + b.len() > self.limit {
            self.full = true;
            return;
        }
        self.buf[self.len..self.len + b.len()].copy_from_slice(b);
        self.len += b.len();
    }

    /// `s` escaped for a JSON string, without quotes.
    pub(crate) fn escaped(&mut self, s: &str) {
        let bytes = s.as_bytes();
        let mut run = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let escape: Option<&[u8]> = match b {
                b'"' => Some(b"\\\""),
                b'\\' => Some(b"\\\\"),
                0..=0x1f => None,
                _ => continue,
            };
            self.raw(&bytes[run..i]);
            match escape {
                Some(e) => self.raw(e),
                None => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    self.raw(&[
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        HEX[usize::from(b >> 4)],
                        HEX[usize::from(b & 15)],
                    ]);
                }
            }
            run = i + 1;
        }
        self.raw(&bytes[run..]);
    }

    /// A JSON string of `s` cut to `max` bytes on a character boundary.
    pub(crate) fn string(&mut self, s: &str, max: usize) {
        self.raw(b"\"");
        self.escaped(cut(s, max));
        self.raw(b"\"");
    }

    /// `,"key":`
    pub(crate) fn key(&mut self, key: &str) {
        self.raw(b",\"");
        self.escaped(key);
        self.raw(b"\":");
    }

    pub(crate) fn u64(&mut self, mut v: u64) {
        let mut tmp = [0u8; 20];
        let mut i = tmp.len();
        loop {
            i -= 1;
            tmp[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.raw(&tmp[i..]);
    }

    fn i64(&mut self, v: i64) {
        if v < 0 {
            self.raw(b"-");
        }
        self.u64(v.unsigned_abs());
    }

    fn display(&mut self, v: impl fmt::Display) {
        let _ = write!(Raw(self), "{v}");
    }

    fn hex(&mut self, bytes: &[u8]) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for b in bytes {
            self.raw(&[HEX[usize::from(b >> 4)], HEX[usize::from(b & 15)]]);
        }
    }
}

/// `fmt::Write` into an `Out` without escaping (numbers only).
struct Raw<'o, 'b>(&'o mut Out<'b>);

impl fmt::Write for Raw<'_, '_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.raw(s.as_bytes());
        Ok(())
    }
}

/// `fmt::Write` into an `Out` with JSON escaping.
struct Escaped<'o, 'b>(&'o mut Out<'b>);

impl fmt::Write for Escaped<'_, '_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.escaped(s);
        Ok(())
    }
}

/// `s` cut to at most `max` bytes on a character boundary.
pub(crate) fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub(crate) fn level_name(level: Level) -> &'static [u8] {
    match level {
        Level::Error => b"error",
        Level::Warn => b"warn",
        Level::Info => b"info",
        Level::Debug => b"debug",
        Level::Trace => b"trace",
    }
}

/// The fixed head of a record.
pub(crate) struct Head<'a> {
    pub t: SystemTime,
    pub up: u64,
    pub level: Level,
    pub target: &'a str,
    pub file: Option<&'a str>,
    pub line: Option<u32>,
    pub ctx: Ctx,
}

/// How the message is written.
pub(crate) enum Message<'a> {
    /// A Lectern macro's literal.
    Literal(&'a str),
    /// A third-party message that is a literal (`as_str()` is Some): rule 1.
    FreeLiteral(&'a str),
    /// A formatted third-party message (its first 1 KiB): rule 3.
    FreeFormatted { text: &'a str, cut: bool },
}

/// `{"t":…,"up":…,"lv":…,"tg":…`: the start every line has.
pub(crate) fn start(out: &mut Out<'_>, t: SystemTime, up: u64, level: Level, target: &str) {
    out.raw(b"{\"t\":\"");
    out.raw(&time::rfc3339_ms(t));
    out.raw(b"\",\"up\":");
    out.u64(up);
    out.raw(b",\"lv\":\"");
    out.raw(level_name(level));
    out.raw(b"\",\"tg\":");
    out.string(target, 64);
}

/// `}` and the newline.
pub(crate) fn finish(out: &mut Out<'_>) {
    out.limit = out.buf.len();
    out.raw(b"}\n");
}

/// Encodes a whole record into `out`. Fields that do not fit in 4 KiB are
/// dropped from the last one on, and `trunc` is set.
pub(crate) fn record(
    out: &mut Out<'_>,
    head: &Head<'_>,
    message: Message<'_>,
    fields: &[(&str, Value<'_>)],
    n: u64,
    home: Option<&str>,
) {
    crate::hooks::encoded();
    out.limit = out.buf.len().min(MAX_LINE) - SUFFIX_RESERVE;
    start(out, head.t, head.up, head.level, head.target);
    out.key("th");
    with_thread_label(|label| out.string(label, 64));
    if let Some(file) = head.file {
        out.raw(b",\"at\":\"");
        home_tilde(out, file, home, 256);
        if let Some(line) = head.line {
            out.raw(b":");
            out.u64(u64::from(line));
        }
        out.raw(b"\"");
    }
    context(out, &head.ctx);
    let before_msg = out.len;
    out.key("msg");
    match message {
        Message::Literal(s) => out.string(s, 512),
        Message::FreeLiteral(s) => out.string(&sanitize::literal(s, home), 256),
        Message::FreeFormatted { text, cut } => {
            out.string(&sanitize::third_party(text, cut, home), 128)
        }
    }
    if out.full {
        out.full = false;
        out.len = before_msg;
        out.raw(b",\"msg\":\"\xe2\x80\xa6\"");
    }
    let mut trunc = false;
    for (key, value) in fields {
        let mark = out.len;
        out.key(key);
        write_value(out, value, home);
        if out.full {
            out.full = false;
            out.len = mark;
            trunc = true;
            break;
        }
    }
    out.limit = out.buf.len().min(MAX_LINE);
    if n > 0 {
        out.key("n");
        out.u64(n);
    }
    if trunc {
        out.raw(b",\"trunc\":true");
    }
    finish(out);
}

fn context(out: &mut Out<'_>, ctx: &Ctx) {
    if let Some(plugin) = ctx.plugin {
        out.key("plugin");
        out.string(plugin.as_str(), 64);
    }
    for (key, v) in [("act", ctx.act), ("task", ctx.task)] {
        if let Some(v) = v {
            out.key(key);
            out.u64(v);
        }
    }
    if let Some(session) = ctx.session {
        out.key("session");
        write_value(out, &Value::uuid(session), None);
    }
    if let Some(review) = ctx.review {
        out.key("review");
        write_value(out, &Value::hex(review), None);
    }
    for (key, v) in [("req", ctx.req), ("analysis", ctx.analysis)] {
        if let Some(v) = v {
            out.key(key);
            out.u64(v);
        }
    }
}

/// `text` with a leading home directory written as `~`, cut at `max` bytes.
fn home_tilde(out: &mut Out<'_>, text: &str, home: Option<&str>, max: usize) {
    match home.and_then(|h| strip_home(text.as_bytes(), h)) {
        Some(rest) => {
            out.raw(b"~");
            out.escaped(cut(std::str::from_utf8(rest).unwrap_or(""), max));
        }
        None => out.escaped(cut(text, max)),
    }
}

/// The bytes after `home` when `bytes` starts with it as a whole component.
pub(crate) fn strip_home<'a>(bytes: &'a [u8], home: &str) -> Option<&'a [u8]> {
    let rest = bytes.strip_prefix(home.as_bytes())?;
    (rest.is_empty() || rest[0] == b'/').then_some(rest)
}

pub(crate) fn write_value(out: &mut Out<'_>, value: &Value<'_>, home: Option<&str>) {
    match value.0 {
        Repr::Null => out.raw(b"null"),
        Repr::Bool(b) => out.raw(if b { b"true" } else { b"false" }),
        Repr::U64(v) => out.u64(v),
        Repr::I64(v) => out.i64(v),
        Repr::U128(v) => out.display(v),
        Repr::I128(v) => out.display(v),
        Repr::F64(v) if v.is_finite() => out.display(v),
        Repr::F64(_) => out.raw(b"null"),
        Repr::Text(s) => out.string(s, 1024),
        Repr::Path(p) => {
            out.raw(b"\"");
            path(out, p, home);
            out.raw(b"\"");
        }
        Repr::Oid(bytes) => {
            out.raw(b"\"");
            out.hex(&bytes[..bytes.len().min(32)]);
            out.raw(b"\"");
        }
        Repr::Uuid(u) => {
            out.raw(b"\"");
            let _ = write_uuid(&mut Raw(out), &u);
            out.raw(b"\"");
        }
        Repr::Hex(v) => {
            out.raw(b"\"");
            out.hex(&v.to_be_bytes());
            out.raw(b"\"");
        }
        Repr::Url(s) => {
            out.raw(b"\"");
            match url_parts(s) {
                Some((scheme, host, path)) => {
                    out.escaped(cut(scheme, 32));
                    out.raw(b"://");
                    out.escaped(cut(host, 256));
                    out.escaped(cut(path, 1024));
                }
                None => out.raw(b"<url>"),
            }
            out.raw(b"\"");
        }
        Repr::Io(kind, code) => {
            out.raw(b"{\"kind\":\"");
            let _ = write!(Escaped(out), "{kind:?}");
            out.raw(b"\",\"code\":");
            match code {
                Some(code) => out.i64(i64::from(code)),
                None => out.raw(b"null"),
            }
            out.raw(b"}");
        }
        Repr::Panic(text) => panic_message(out, text, home),
        #[cfg(feature = "sink")]
        Repr::Frames(s) => out.string(s, 2048),
        #[cfg(feature = "sink")]
        Repr::Time(t) => out.string(&time::rfc3339_s(t), 32),
    }
}

/// A path: home as `~`, at most 1024 bytes, U+FFFD for bytes that are not
/// UTF-8.
fn path(out: &mut Out<'_>, p: &Path, home: Option<&str>) {
    let mut bytes = p.as_os_str().as_encoded_bytes();
    if let Some(rest) = home.and_then(|h| strip_home(bytes, h)) {
        out.raw(b"~");
        bytes = rest;
    }
    let bytes = &bytes[..bytes.len().min(1024)];
    for chunk in bytes.utf8_chunks() {
        out.escaped(chunk.valid());
        if !chunk.invalid().is_empty() {
            out.raw("\u{fffd}".as_bytes());
        }
    }
}

/// `Value::panic_message`: rule 1 for `Static`, rule 2 for `Formatted`.
fn panic_message(out: &mut Out<'_>, text: PanicText<'_>, home: Option<&str>) {
    match text {
        PanicText::Static(s) => out.string(&sanitize::literal(s, home), 256),
        PanicText::Formatted(s) => {
            let summary = sanitize::formatted_panic(s);
            out.raw(b"{\"len\":");
            out.u64(summary.len as u64);
            if let Some(prefix) = summary.prefix {
                out.key("prefix");
                out.string(prefix, 64);
            }
            if !summary.nums().is_empty() {
                out.key("nums");
                out.raw(b"[");
                for (i, n) in summary.nums().iter().enumerate() {
                    if i > 0 {
                        out.raw(b",");
                    }
                    out.u64(*n);
                }
                out.raw(b"]");
            }
            if let Some(variant) = summary.variant() {
                out.key("variant");
                out.string(variant, 32);
            }
            out.raw(b"}");
        }
        PanicText::Other => out.raw(b"{\"type\":\"unknown\"}"),
    }
}

thread_local! {
    static LINE: RefCell<[u8; MAX_LINE]> = const { RefCell::new([0; MAX_LINE]) };
    static LABEL: RefCell<Label> = const { RefCell::new(Label { buf: [0; 64], len: 0, set: false }) };
}

/// Encodes with this thread's 4 KiB buffer; None when the buffer is in use
/// (a record logged while formatting another one) or the thread is exiting.
pub(crate) fn with_line<R>(f: impl FnOnce(&mut Out<'_>) -> R) -> Option<R> {
    LINE.try_with(|line| {
        let mut line = line.try_borrow_mut().ok()?;
        let mut out = Out::new(&mut line[..]);
        Some(f(&mut out))
    })
    .ok()
    .flatten()
}

struct Label {
    buf: [u8; 64],
    len: u8,
    set: bool,
}

/// Numbers for unnamed threads, from 1.
static NEXT_THREAD: AtomicU64 = AtomicU64::new(1);

/// The thread's name, or `#<n>` for an unnamed thread; at most 64 bytes.
pub(crate) fn with_thread_label<R>(f: impl FnOnce(&str) -> R) -> R {
    let label = LABEL.try_with(|label| {
        let mut label = label.borrow_mut();
        if !label.set {
            let thread = std::thread::current();
            let mut tmp = [0u8; 64];
            let len = match thread.name() {
                Some(name) => {
                    let name = cut(name, 64);
                    tmp[..name.len()].copy_from_slice(name.as_bytes());
                    name.len()
                }
                None => {
                    let mut out = Out::new(&mut tmp);
                    out.raw(b"#");
                    out.u64(NEXT_THREAD.fetch_add(1, Ordering::Relaxed));
                    out.len
                }
            };
            label.buf = tmp;
            label.len = len as u8;
            label.set = true;
        }
        (label.buf, label.len)
    });
    match label {
        Ok((buf, len)) => f(std::str::from_utf8(&buf[..usize::from(len)]).unwrap_or("")),
        Err(_) => f(""),
    }
}
