//! The panic hook (docs/design/logging.md「数据流与线程」5): an error record
//! with the location, the thread, the scope and the handled message; it
//! waits up to 100 ms for the writer to write it, then calls the previous
//! hook. Before `start`, it writes the early buffer and the record itself.

use std::cell::Cell;
use std::panic::PanicHookInfo;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use log::Level;

use super::writer::Files;
use super::{Phase, State, encode_line, frames, lock};
use crate::engine::encode::{self, Head, Message};
use crate::engine::logger;
use crate::scope;
use crate::value::{PanicText, Repr, Value};

/// How long the panicking thread waits for its record to be written.
pub(crate) const WAIT: Duration = Duration::from_millis(100);

static EPOCH: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_epoch() -> u64 {
    EPOCH.fetch_add(1, Ordering::AcqRel) + 1
}

/// Wraps the current hook. Only the hook of the latest install acts (tests
/// install more than once per process); each calls the hook before it.
pub(crate) fn install(epoch: u64) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if EPOCH.load(Ordering::Acquire) == epoch {
            record(info);
        }
        previous(info);
    }));
}

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// The payload's type decides the rule, not its content.
pub(crate) fn panic_text<'a>(payload: &'a (dyn std::any::Any + Send)) -> PanicText<'a> {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        PanicText::Static(s)
    } else if let Some(s) = payload.downcast_ref::<String>() {
        PanicText::Formatted(s)
    } else {
        PanicText::Other
    }
}

fn record(info: &PanicHookInfo<'_>) {
    if IN_HOOK.try_with(|h| h.replace(true)).unwrap_or(true) {
        return;
    }
    let _reset = Reset;
    let Some(shared) = logger::current() else {
        return;
    };
    let Some(state) = shared.sink() else { return };
    let backtrace = (state.backtrace || state.verbose_on()).then(frame_names);
    let location = info.location();
    let mut fields = vec![("message", Value::panic_message(panic_text(info.payload())))];
    if let Some(frames) = backtrace.as_deref() {
        fields.push(("backtrace", Value(Repr::Frames(frames))));
    }
    let line = encode_line(|out| {
        let head = Head {
            t: crate::clock::now(),
            up: crate::clock::up_micros(),
            level: Level::Error,
            target: "logging",
            file: location.map(|l| l.file()),
            line: location.map(|l| l.line()),
            ctx: scope::current(),
        };
        encode::record(
            out,
            &head,
            Message::Literal("panic"),
            &fields,
            0,
            shared.home.as_deref(),
        );
    });
    if write_before_start(state, &line) {
        return;
    }
    if !state.push(Level::Error, &line) {
        shared.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let on_writer = state
        .writer
        .get()
        .is_some_and(|w| w.id() == std::thread::current().id());
    if !on_writer {
        state.flush(WAIT, true);
    }
}

struct Reset;

impl Drop for Reset {
    fn drop(&mut self) {
        let _ = IN_HOOK.try_with(|h| h.set(false));
    }
}

/// Before the writer starts, with a directory: opens this run's file on the
/// panicking thread and writes the early buffer and `line`. False when the
/// writer has taken over (or there is no directory: the record stays in
/// memory).
fn write_before_start(state: &State, line: &[u8]) -> bool {
    if state.dir.is_none() {
        return false;
    }
    let mut handoff = lock(&state.handoff);
    let early = {
        let mut b = lock(&state.buffers);
        if b.phase != Phase::Early {
            return false;
        }
        let early = b.early.clone();
        b.early.clear();
        early
    };
    let verbose = state.verbose_on();
    let files = handoff.get_or_insert_with(|| Files::new(state, None));
    for (level, record) in frames(&early) {
        let _ = files.write(level, record, verbose);
    }
    let _ = files.write(Level::Error, line, verbose);
    let _ = files.sync();
    true
}

/// The backtrace's frame names (code, not data), `; `-separated.
fn frame_names() -> String {
    let text = std::backtrace::Backtrace::force_capture().to_string();
    let mut names = String::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some((index, name)) = line.split_once(": ") else {
            continue;
        };
        if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if !names.is_empty() {
            names.push_str("; ");
        }
        names.push_str(name);
        if names.len() > 2048 {
            break;
        }
    }
    names
}
