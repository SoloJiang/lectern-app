//! A macro call site: its cached filter decision and its rate limit
//! (docs/design/logging.md「调用点」「限流」), and the record it hands to
//! the `log` facade.

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use log::{Level, LevelFilter, Record};

use crate::value::Value;
use crate::{clock, filter};

/// A rate-limit window: 10 seconds.
pub(crate) const WINDOW_US: u64 = 10_000_000;
/// Records per call site and window, for error, warn and info.
pub(crate) const PER_WINDOW: u32 = 20;
/// The per-window limit of Lectern call sites. Only `testing` changes it
/// (the benchmarks time the written path); otherwise always [`PER_WINDOW`].
pub(crate) static LECTERN_LIMIT: AtomicU32 = AtomicU32::new(PER_WINDOW);
/// Records left out by rate limits, Lectern and third-party, in this process.
pub(crate) static SUPPRESSED: AtomicU64 = AtomicU64::new(0);

/// The static each macro call expands to.
pub struct Callsite {
    pub(crate) target: &'static str,
    pub(crate) module_path: &'static str,
    pub(crate) file: &'static str,
    pub(crate) line: u32,
    pub(crate) level: Level,
    pub(crate) message: &'static str,
    /// `generation << 8 | LevelFilter`: the filter's level for this target.
    cache: AtomicU64,
    /// `window << 32 | count`.
    window: AtomicU64,
    /// Left out since the last record written here: the next one's `n`.
    suppressed: AtomicU64,
}

/// A record the call site lets through.
#[derive(Clone, Copy)]
pub struct Permit {
    pub(crate) n: u64,
    /// The `up` the rate limit already read, if it did.
    pub(crate) up: Option<u64>,
}

impl Callsite {
    pub const fn new(
        module_path: &'static str,
        file: &'static str,
        line: u32,
        level: Level,
        message: &'static str,
    ) -> Callsite {
        Callsite {
            target: short_target(module_path),
            module_path,
            file,
            line,
            level,
            message,
            cache: AtomicU64::new(0),
            window: AtomicU64::new(0),
            suppressed: AtomicU64::new(0),
        }
    }

    /// The filter and the rate limit, after the macro's `log::max_level()`
    /// check.
    #[inline]
    pub fn permit(&'static self) -> Option<Permit> {
        let generation = filter::GENERATION.load(Ordering::Acquire);
        let cached = self.cache.load(Ordering::Relaxed);
        let allowed = if cached >> 8 == generation {
            level_filter(cached as u8)
        } else {
            self.refresh()
        };
        if self.level > allowed
            && !(filter::PLUGINS.load(Ordering::Relaxed) && filter::plugin_allows(self.level))
        {
            return None;
        }
        if self.level <= Level::Info {
            self.limit()
        } else {
            Some(Permit { n: 0, up: None })
        }
    }

    #[cold]
    fn refresh(&self) -> LevelFilter {
        let Some(active) = filter::active() else {
            return LevelFilter::Off;
        };
        let level = active.filter.lectern(self.target);
        self.cache
            .store(active.generation << 8 | level as u64, Ordering::Relaxed);
        level
    }

    fn limit(&self) -> Option<Permit> {
        if take(
            &self.window,
            clock::window(WINDOW_US),
            LECTERN_LIMIT.load(Ordering::Relaxed),
        ) {
            let n = match self.suppressed.load(Ordering::Relaxed) {
                0 => 0,
                _ => self.suppressed.swap(0, Ordering::Relaxed),
            };
            Some(Permit { n, up: None })
        } else {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            SUPPRESSED.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

/// Counts one record in `state` (`window << 32 | count`) for `window`;
/// false when the window already has `limit`. A window older than the one
/// stored (a clock read on another thread) counts in the stored one.
pub(crate) fn take(state: &AtomicU64, window: u64, limit: u32) -> bool {
    let window = window & 0xffff_ffff;
    let mut s = state.load(Ordering::Relaxed);
    loop {
        let (stored, count) = (s >> 32, s & 0xffff_ffff);
        let next = if stored < window {
            window << 32 | 1
        } else if count < u64::from(limit) {
            s + 1
        } else {
            return false;
        };
        match state.compare_exchange_weak(s, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(now) => s = now,
        }
    }
}

fn level_filter(v: u8) -> LevelFilter {
    match v {
        1 => LevelFilter::Error,
        2 => LevelFilter::Warn,
        3 => LevelFilter::Info,
        4 => LevelFilter::Debug,
        5 => LevelFilter::Trace,
        _ => LevelFilter::Off,
    }
}

/// The first segment of a module path, without `lectern_`:
/// `lectern_cache::store` → `cache`.
pub(crate) const fn short_target(module_path: &'static str) -> &'static str {
    let bytes = module_path.as_bytes();
    let mut end = 0;
    while end < bytes.len() && bytes[end] != b':' {
        end += 1;
    }
    let prefix = b"lectern_";
    let mut start = 0;
    if end > prefix.len() {
        let mut i = 0;
        while i < prefix.len() && bytes[i] == prefix[i] {
            i += 1;
        }
        if i == prefix.len() {
            start = prefix.len();
        }
    }
    let (head, _) = bytes.split_at(end);
    let (_, segment) = head.split_at(start);
    match std::str::from_utf8(segment) {
        Ok(s) => s,
        Err(_) => module_path,
    }
}

/// Messages are plain literals: no braces (nothing is interpolated; values
/// go in fields) and at most 512 bytes.
pub const fn check_message(message: &str) {
    let bytes = message.as_bytes();
    assert!(bytes.len() <= 512, "a log message is at most 512 bytes");
    let mut i = 0;
    while i < bytes.len() {
        assert!(
            bytes[i] != b'{' && bytes[i] != b'}',
            "a log message is a plain literal; put values in fields: `name = value`"
        );
        i += 1;
    }
}

/// At most 12 fields.
pub const fn check_fields(keys: &[&str]) {
    assert!(keys.len() <= 12, "a log record has at most 12 fields");
}

/// What `emit` hands the logger beside the `log::Record`: the typed fields.
#[cfg_attr(
    not(any(feature = "sink", feature = "test-support")),
    allow(dead_code, reason = "read by the logger, which these features build")
)]
pub(crate) struct Payload<'a> {
    pub(crate) callsite: &'static Callsite,
    pub(crate) n: u64,
    pub(crate) up: Option<u64>,
    pub(crate) fields: &'a [(&'static str, Value<'a>)],
}

thread_local! {
    /// The payload of the record `emit` is logging on this thread.
    static MARK: Cell<*const ()> = const { Cell::new(std::ptr::null()) };
}

/// Writes a record that passed [`Callsite::permit`].
#[inline(never)]
pub fn emit(callsite: &'static Callsite, permit: Permit, fields: &[(&'static str, Value<'_>)]) {
    let payload = Payload {
        callsite,
        n: permit.n,
        up: permit.up,
        fields,
    };
    let me = (&payload as *const Payload<'_>).cast::<()>();
    let prev = MARK.try_with(|m| m.replace(me)).ok();
    log::logger().log(
        &Record::builder()
            .args(format_args!("{}", callsite.message))
            .level(callsite.level)
            .target(callsite.target)
            .module_path_static(Some(callsite.module_path))
            .file_static(Some(callsite.file))
            .line(Some(callsite.line))
            .key_values(&payload)
            .build(),
    );
    if let Some(prev) = prev {
        let _ = MARK.try_with(|m| m.set(prev));
    }
}

/// The payload of `record` when `emit` on this thread built it.
#[cfg(any(feature = "sink", feature = "test-support"))]
pub(crate) fn ours<R>(record: &Record<'_>, f: impl FnOnce(Option<&Payload<'_>>) -> R) -> R {
    let mark = MARK.try_with(Cell::get).unwrap_or(std::ptr::null());
    let source = (record.key_values() as *const dyn log::kv::Source).cast::<()>();
    if mark.is_null() || mark != source {
        return f(None);
    }
    // SAFETY: `mark` is the address of the `Payload` on the stack of the
    // `emit` call that is running on this thread (it is reset before `emit`
    // returns), and `record`'s key-values point at that same payload, so it
    // is alive for the whole of this call.
    let payload = unsafe { &*mark.cast::<Payload<'_>>() };
    f(Some(payload))
}

impl log::kv::Source for Payload<'_> {
    fn visit<'kvs>(
        &'kvs self,
        visitor: &mut dyn log::kv::VisitSource<'kvs>,
    ) -> Result<(), log::kv::Error> {
        for (key, value) in self.fields {
            visitor.visit_pair(log::kv::Key::from_str(key), value.kv_value())?;
        }
        if self.n > 0 {
            visitor.visit_pair(log::kv::Key::from_str("n"), self.n.into())?;
        }
        Ok(())
    }
}
