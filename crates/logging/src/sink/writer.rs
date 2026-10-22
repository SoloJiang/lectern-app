//! The `lectern-log` thread (docs/design/logging.md「数据流与线程」): every
//! file operation after `start` happens here. No periodic wake-up: it parks
//! until a record arrives, waits at most 1 s or 32 KiB before writing
//! (error records and flush requests at once), and its only timed wait is
//! the expiry of verbose logging from the settings.

use std::fs::File;
use std::io::{self, BufWriter};
use std::panic::{AssertUnwindSafe, Location};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use log::Level;

use super::files::{self, Cleanup};
use super::{
    AllCaps, BUSY, Config, FilterSource, Header, IDLE, Phase, RUN_CAP, State, Stop, VERBOSE_CAP,
    VerboseCmd, WAITING, encode_line, frames, fs, lock, verbose,
};
use crate::engine::encode::{self, Head, Message};
use crate::engine::logger::Shared;
use crate::filter::{self, Filter};
use crate::scope::Ctx;
use crate::value::{Repr, Value};
use crate::{callsite, clock, hooks};

/// Records that are not urgent wait at most this long.
const BATCH_WAIT: Duration = Duration::from_secs(1);
const WRITE_BUFFER: usize = 64 << 10;
/// The format version in every header.
const FORMAT: u64 = 1;
/// Notes kept while no file can be opened.
const MAX_PENDING_NOTES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Default,
    Verbose,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Default => "default",
            Kind::Verbose => "verbose",
        }
    }
}

struct OpenFile {
    w: BufWriter<File>,
    bytes: u64,
    records: u64,
}

#[derive(Default)]
struct Slot {
    file: Option<OpenFile>,
    next_seq: u32,
}

/// What happened while writing, turned into notes by the writer.
pub(crate) enum Event {
    Rotated { kind: Kind, seq: u32 },
    Cleaned { kind: Kind, cleanup: Cleanup },
}

/// The open files of this run: one default file and, while verbose
/// logging is on, one verbose file.
pub(crate) struct Files {
    dir: PathBuf,
    stamp: String,
    pid: u32,
    run: u64,
    caps: AllCaps,
    /// None for files a panic opened before `start`.
    header: Option<Header>,
    default: Slot,
    verbose: Slot,
    disk: [u64; 2],
    pub(crate) events: Vec<Event>,
}

impl Files {
    pub(crate) fn new(state: &State, header: Option<Header>) -> Files {
        Files {
            dir: state.dir.clone().unwrap_or_default(),
            stamp: state.stamp.clone(),
            pid: state.pid,
            run: state.run,
            caps: state.caps,
            header,
            default: Slot::default(),
            verbose: Slot::default(),
            disk: [0; 2],
            events: Vec::new(),
        }
    }

    fn slot(&mut self, kind: Kind) -> &mut Slot {
        match kind {
            Kind::Default => &mut self.default,
            Kind::Verbose => &mut self.verbose,
        }
    }

    fn dir_of(&self, kind: Kind) -> PathBuf {
        match kind {
            Kind::Default => self.dir.clone(),
            Kind::Verbose => files::verbose_dir(&self.dir),
        }
    }

    fn caps_of(&self, kind: Kind) -> files::Caps {
        match kind {
            Kind::Default => self.caps.default,
            Kind::Verbose => self.caps.verbose,
        }
    }

    /// Writes a record: to the default file at info and above, to the
    /// verbose file whenever verbose logging is on.
    pub(crate) fn write(&mut self, level: Level, line: &[u8], verbose: bool) -> io::Result<()> {
        let mut result = Ok(());
        if level <= Level::Info {
            result = self.write_to(Kind::Default, line, verbose);
        }
        if verbose {
            let r = self.write_to(Kind::Verbose, line, verbose);
            result = result.and(r);
        }
        result
    }

    fn write_to(&mut self, kind: Kind, line: &[u8], verbose: bool) -> io::Result<()> {
        if self.slot(kind).file.is_none() {
            self.open(kind, verbose)?;
        }
        let cap = self.caps_of(kind).file_bytes;
        let full = self
            .slot(kind)
            .file
            .as_ref()
            .is_some_and(|f| f.records > 0 && f.bytes + line.len() as u64 > cap);
        if full {
            self.rotate(kind, verbose)?;
        }
        let Some(file) = self.slot(kind).file.as_mut() else {
            return Err(io::ErrorKind::NotFound.into());
        };
        fs::write(&mut file.w, line)?;
        file.bytes += line.len() as u64;
        file.records += 1;
        Ok(())
    }

    /// Opens this run's default file (with its header) if it is not open.
    pub(crate) fn open_default(&mut self, verbose: bool) -> io::Result<()> {
        match self.default.file {
            Some(_) => Ok(()),
            None => self.open(Kind::Default, verbose),
        }
    }

    fn open(&mut self, kind: Kind, verbose: bool) -> io::Result<()> {
        let dir = self.dir_of(kind);
        fs::create_dir_all(&dir)?;
        let seq = self.slot(kind).next_seq;
        let file = fs::open_append(&dir.join(files::file_name(&self.stamp, self.pid, seq)))?;
        let mut w = BufWriter::with_capacity(WRITE_BUFFER, file);
        let header = self.header_line(seq, verbose);
        fs::write(&mut w, &header)?;
        let slot = self.slot(kind);
        slot.next_seq = seq + 1;
        slot.file = Some(OpenFile {
            w,
            bytes: header.len() as u64,
            records: 0,
        });
        Ok(())
    }

    fn rotate(&mut self, kind: Kind, verbose: bool) -> io::Result<()> {
        if let Some(mut old) = self.slot(kind).file.take() {
            fs::flush(&mut old.w)?;
        }
        self.open(kind, verbose)?;
        let seq = self.slot(kind).next_seq - 1;
        self.events.push(Event::Rotated { kind, seq });
        self.cleanup(kind);
        Ok(())
    }

    /// Cleans one kind's directory after a rotation: the caps apply to this
    /// run's earlier files too, only the file just opened stays.
    pub(crate) fn cleanup(&mut self, kind: Kind) {
        let slot = self.slot(kind);
        let open_seq = slot.file.as_ref().map(|_| slot.next_seq - 1);
        let keep = files::Keep {
            stamp: &self.stamp,
            pid: self.pid,
            open_seq,
        };
        let cleanup = files::cleanup(&self.dir_of(kind), keep, clock::now(), self.caps_of(kind));
        self.disk[kind as usize] = cleanup.remaining_bytes;
        self.events.push(Event::Cleaned { kind, cleanup });
    }

    fn header_line(&self, seq: u32, verbose: bool) -> Vec<u8> {
        let filter_text = filter::active()
            .map(|a| verbose::filter_text(&a.filter))
            .unwrap_or_default();
        encode_line(|out| {
            encode::start(
                out,
                clock::now(),
                clock::up_micros(),
                Level::Info,
                "logging",
            );
            out.key("msg");
            out.string("file", 8);
            out.key("format");
            out.u64(FORMAT);
            out.key("run");
            encode::write_value(out, &Value::hex(self.run), None);
            out.key("seq");
            out.u64(u64::from(seq));
            if let Some(h) = &self.header {
                out.key("version");
                out.string(h.version, 64);
                out.key("build");
                out.string(h.build, 64);
                out.key("os");
                out.string(&h.os, 128);
                out.key("arch");
                out.string(h.arch, 32);
            }
            out.key("pid");
            out.u64(u64::from(self.pid));
            out.key("filter");
            out.string(&filter_text, 512);
            out.key("verbose");
            out.raw(if verbose { b"true" } else { b"false" });
            encode::finish(out);
        })
    }

    fn end_line(&self, dropped: u64, suppressed: u64) -> Vec<u8> {
        encode_line(|out| {
            encode::start(
                out,
                clock::now(),
                clock::up_micros(),
                Level::Info,
                "logging",
            );
            out.key("msg");
            out.string("run end", 8);
            out.key("run");
            encode::write_value(out, &Value::hex(self.run), None);
            out.key("dropped");
            out.u64(dropped);
            out.key("suppressed");
            out.u64(suppressed);
            encode::finish(out);
        })
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        let mut result = Ok(());
        for slot in [&mut self.default, &mut self.verbose] {
            if let Some(f) = slot.file.as_mut() {
                result = result.and(fs::flush(&mut f.w));
            }
        }
        result
    }

    pub(crate) fn sync(&mut self) -> io::Result<()> {
        self.flush()?;
        for slot in [&self.default, &self.verbose] {
            if let Some(f) = slot.file.as_ref() {
                fs::sync(f.w.get_ref())?;
            }
        }
        Ok(())
    }

    fn close_verbose(&mut self) {
        if let Some(mut f) = self.verbose.file.take() {
            let _ = fs::flush(&mut f.w);
        }
    }

    /// The end record in every open file.
    fn end(&mut self, dropped: u64, suppressed: u64) {
        let line = self.end_line(dropped, suppressed);
        for slot in [&mut self.default, &mut self.verbose] {
            if let Some(f) = slot.file.as_mut() {
                let _ = fs::write(&mut f.w, &line);
            }
        }
    }
}

/// A record the logger writes itself: target `logging`, the caller's
/// location, no scope.
#[track_caller]
pub(crate) fn note_line(
    level: Level,
    msg: &'static str,
    fields: &[(&str, Value<'_>)],
    home: Option<&str>,
) -> Vec<u8> {
    let at = Location::caller();
    encode_line(|out| {
        let head = Head {
            t: clock::now(),
            up: clock::up_micros(),
            level,
            target: "logging",
            file: Some(at.file()),
            line: Some(at.line()),
            ctx: Ctx::default(),
        };
        encode::record(out, &head, Message::Literal(msg), fields, 0, home);
    })
}

struct Writer {
    shared: &'static Shared,
    state: &'static State,
    header: Header,
    files: Option<Files>,
    back: Vec<u8>,
    pending: Vec<(Level, Vec<u8>)>,
    reported: (u64, u64),
    wait_started: Option<Instant>,
    /// Bytes discarded since the last attempt to open the directory.
    discarded: u64,
}

pub(crate) fn run(shared: &'static Shared, mut config: Config) {
    let Some(state) = shared.sink() else { return };
    let _ = state.writer.set(std::thread::current());
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if let Some(on_start) = config.on_writer_start.take() {
            on_start();
        }
        let mut writer = Writer {
            shared,
            state,
            header: config.header,
            files: None,
            back: Vec::new(),
            pending: Vec::new(),
            reported: (0, 0),
            wait_started: None,
            discarded: 0,
        };
        writer.startup();
        writer.run();
    }));
    // A panic here leaves memory buffers only; records are still counted.
    let _ = result;
    lock(&state.control).finished = true;
    state.changed.notify_all();
}

impl Writer {
    fn home(&self) -> Option<&'static str> {
        self.shared.home.as_deref()
    }

    #[track_caller]
    fn note(&mut self, level: Level, msg: &'static str, fields: &[(&str, Value<'_>)]) {
        let line = note_line(level, msg, fields, self.home());
        self.write_line(level, line);
    }

    fn write_line(&mut self, level: Level, line: Vec<u8>) {
        let verbose = self.state.verbose_on();
        match self.files.as_mut() {
            Some(files) => {
                if files.write(level, &line, verbose).is_err() {
                    self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            None if self.pending.len() < MAX_PENDING_NOTES => self.pending.push((level, line)),
            None => {}
        }
    }

    fn startup(&mut self) {
        let state = self.state;
        let Some(dir) = state.dir.clone() else { return };
        let current = (state.stamp.as_str(), state.pid);
        let now = clock::now();
        let mut previous = None;
        let mut cleanups = Vec::new();
        let dir_ok = match fs::create_dir_all(&dir) {
            Ok(()) => {
                for (kind, path, caps) in [
                    (Kind::Default, dir.clone(), state.caps.default),
                    (Kind::Verbose, files::verbose_dir(&dir), state.caps.verbose),
                ] {
                    // This run's first file is the one the writer opens (or
                    // took over from a panic before `start`).
                    let keep = files::Keep {
                        stamp: &state.stamp,
                        pid: state.pid,
                        open_seq: Some(0),
                    };
                    cleanups.push((kind, files::cleanup(&path, keep, now, caps)));
                }
                self.read_verbose(now);
                previous = files::previous_run(&dir, current);
                true
            }
            Err(e) => {
                self.note(
                    Level::Warn,
                    "log directory unavailable",
                    &[("error", Value::io(&e))],
                );
                false
            }
        };
        let cap = if state.verbose_on() {
            VERBOSE_CAP
        } else {
            RUN_CAP
        };
        let early = {
            let mut handoff = lock(&state.handoff);
            if dir_ok {
                let mut files = handoff.take().unwrap_or_else(|| Files::new(state, None));
                files.header = Some(self.header.clone());
                files.disk = [cleanups[0].1.remaining_bytes, cleanups[1].1.remaining_bytes];
                self.files = Some(files);
            }
            let mut b = lock(&state.buffers);
            b.front = Vec::with_capacity(cap);
            b.cap = cap;
            state.cap.store(cap, Ordering::Relaxed);
            b.phase = Phase::Running;
            std::mem::take(&mut b.early)
        };
        self.back = Vec::with_capacity(cap);
        let verbose = state.verbose_on();
        if let Some(Err(e)) = self.files.as_mut().map(|f| f.open_default(verbose)) {
            self.note(
                Level::Warn,
                "log file unavailable",
                &[("error", Value::io(&e))],
            );
        }
        for (kind, cleanup) in cleanups {
            self.cleaned(kind, cleanup);
        }
        if let Some(previous) = previous.filter(|p| !p.ended) {
            let run = previous.run.map(Value::hex);
            self.note(
                Level::Info,
                "previous run ended unexpectedly",
                &[("run", run.unwrap_or(Value(Repr::Null)))],
            );
        }
        for &index in state.skipped.iter().take(8) {
            self.note(
                Level::Warn,
                "log filter directive skipped",
                &[("index", Value(Repr::U64(index as u64)))],
            );
        }
        let pending = std::mem::take(&mut self.pending);
        for (level, line) in pending {
            self.write_line(level, line);
        }
        self.write_frames(&early);
        drop(early);
        self.after_batch();
    }

    /// Applies `verbose.json` when it is valid and not expired; removes it
    /// otherwise. With `--log` or `LECTERN_LOG` the file stays but does
    /// not change this run's filter.
    fn read_verbose(&mut self, now: SystemTime) {
        let Some(dir) = self.state.dir.as_ref() else {
            return;
        };
        let path = verbose::path(dir);
        if !fs::exists(&path) {
            return;
        }
        let decoded = fs::read_to_string(&path)
            .ok()
            .and_then(|t| verbose::decode(&t));
        match decoded {
            Some((filter, until)) if until > now => {
                let mut s = lock(&self.state.settings);
                if !s.explicit {
                    filter::activate(filter);
                    s.source = FilterSource::Setting;
                    s.until = Some(until);
                    drop(s);
                    self.note(
                        Level::Info,
                        "verbose logging on",
                        &[
                            ("source", Value::text("setting")),
                            ("until", Value(Repr::Time(until))),
                        ],
                    );
                }
            }
            _ => {
                let _ = fs::remove_file(&path);
            }
        }
    }

    fn run(&mut self) {
        let state = self.state;
        loop {
            state.writer_state.store(BUSY, Ordering::Release);
            hooks::writer_batch();
            let (flush_to, fsync, cmd, stop) = {
                let mut c = lock(&state.control);
                let flush_to = (c.flush_asked > c.flushed).then_some(c.flush_asked);
                (
                    flush_to,
                    std::mem::take(&mut c.fsync),
                    std::mem::take(&mut c.verbose),
                    c.stop,
                )
            };
            for cmd in cmd {
                self.verbose_command(cmd);
            }
            self.check_expiry();
            if let Some(stop) = stop {
                self.finish(stop);
                return;
            }
            let (pending, urgent) = self.peek();
            let waited = self.wait_started.is_some_and(|s| s.elapsed() >= BATCH_WAIT);
            if pending && (urgent || waited || flush_to.is_some()) {
                self.write_pending();
            }
            // Notes written outside a batch are still in the `BufWriter`.
            if let Some(files) = self.files.as_mut() {
                let _ = if fsync { files.sync() } else { files.flush() };
            }
            if let Some(seq) = flush_to {
                lock(&state.control).flushed = seq;
                state.changed.notify_all();
            }
            self.sleep();
        }
    }

    /// Parks until there is something to do.
    fn sleep(&mut self) {
        let state = self.state;
        let (pending, urgent) = self.peek();
        if urgent || self.control_pending() {
            return;
        }
        if pending {
            let started = *self.wait_started.get_or_insert_with(Instant::now);
            let left = BATCH_WAIT.saturating_sub(started.elapsed());
            if left.is_zero() {
                return;
            }
            state.writer_state.store(WAITING, Ordering::SeqCst);
            if self.peek().1 || self.control_pending() {
                return;
            }
            std::thread::park_timeout(left);
            return;
        }
        state.writer_state.store(IDLE, Ordering::SeqCst);
        if self.peek().0 || self.control_pending() {
            return;
        }
        match self.expiry_in() {
            Some(left) => std::thread::park_timeout(left),
            None => std::thread::park(),
        }
    }

    /// (the running buffer has records, it is urgent), from the mirrors
    /// callers write under the buffer lock: the writer takes that lock only
    /// to swap, so a waiting caller seldom finds it held by the writer.
    fn peek(&self) -> (bool, bool) {
        (
            self.state.pending.load(Ordering::SeqCst),
            self.state.urgent.load(Ordering::SeqCst),
        )
    }

    fn control_pending(&self) -> bool {
        let c = lock(&self.state.control);
        !c.verbose.is_empty() || c.stop.is_some() || c.flush_asked > c.flushed
    }

    fn expiry_in(&self) -> Option<Duration> {
        let s = lock(&self.state.settings);
        let until = s.until.filter(|_| s.source == FilterSource::Setting)?;
        Some(
            until
                .duration_since(clock::now())
                .unwrap_or_default()
                .max(Duration::from_millis(1)),
        )
    }

    fn write_pending(&mut self) {
        self.wait_started = None;
        let cap = self.state.cap.load(Ordering::Relaxed);
        // Allocate outside the lock (only after the capacity grew).
        if self.back.capacity() < cap {
            self.back.reserve_exact(cap - self.back.len());
        }
        {
            let mut b = lock(&self.state.buffers);
            b.urgent = false;
            self.state.urgent.store(false, Ordering::SeqCst);
            self.state.pending.store(false, Ordering::SeqCst);
            if b.front.is_empty() {
                return;
            }
            std::mem::swap(&mut b.front, &mut self.back);
        }
        let back = std::mem::take(&mut self.back);
        self.write_frames(&back);
        self.back = back;
        self.back.clear();
        self.back.shrink_to(cap);
        self.after_batch();
    }

    fn write_frames(&mut self, bytes: &[u8]) {
        let verbose = self.state.verbose_on();
        match self.files.as_mut() {
            Some(files) => {
                for (level, line) in frames(bytes) {
                    if files.write(level, line, verbose).is_err() {
                        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            None => {
                let count = frames(bytes).count() as u64;
                self.shared.dropped.fetch_add(count, Ordering::Relaxed);
                self.discarded += bytes.len() as u64;
                if self.discarded >= self.state.caps.default.file_bytes {
                    self.discarded = 0;
                    self.retry_directory();
                }
            }
        }
    }

    /// The directory was unavailable: try again (as often as a file would
    /// rotate).
    fn retry_directory(&mut self) {
        let Some(dir) = self.state.dir.clone() else {
            return;
        };
        if fs::create_dir_all(&dir).is_ok() {
            self.files = Some(Files::new(self.state, Some(self.header.clone())));
            let pending = std::mem::take(&mut self.pending);
            for (level, line) in pending {
                self.write_line(level, line);
            }
        }
    }

    /// Notes for what the batch caused, the drop report, and `write(2)`.
    fn after_batch(&mut self) {
        for _ in 0..4 {
            let events = match self.files.as_mut() {
                Some(files) if !files.events.is_empty() => std::mem::take(&mut files.events),
                _ => break,
            };
            for event in events {
                match event {
                    Event::Rotated { kind, seq } => self.note(
                        Level::Debug,
                        "log file rotated",
                        &[
                            ("kind", Value::text(kind.name())),
                            ("seq", Value(Repr::U64(u64::from(seq)))),
                        ],
                    ),
                    Event::Cleaned { kind, cleanup } => self.cleaned(kind, cleanup),
                }
            }
        }
        let dropped = self.shared.dropped.load(Ordering::Relaxed);
        let suppressed = callsite::SUPPRESSED.load(Ordering::Relaxed);
        if dropped > self.reported.0 || suppressed > self.reported.1 {
            let fields = [
                ("dropped", Value(Repr::U64(dropped - self.reported.0))),
                ("suppressed", Value(Repr::U64(suppressed - self.reported.1))),
            ];
            self.reported = (dropped, suppressed);
            self.note(Level::Warn, "records dropped", &fields);
        }
        if let Some(files) = self.files.as_mut() {
            let _ = files.flush();
            self.state
                .disk_bytes
                .store(files.disk.iter().sum(), Ordering::Relaxed);
        }
    }

    fn cleaned(&mut self, kind: Kind, cleanup: Cleanup) {
        if let Some(files) = self.files.as_mut() {
            files.disk[kind as usize] = cleanup.remaining_bytes;
        }
        if cleanup.removed > 0 {
            self.note(
                Level::Info,
                "old log files removed",
                &[
                    ("kind", Value::text(kind.name())),
                    ("files", Value(Repr::U64(cleanup.removed as u64))),
                    ("bytes", Value(Repr::U64(cleanup.removed_bytes))),
                ],
            );
        }
        if cleanup.failed > 0 {
            self.note(
                Level::Warn,
                "old log files not removed",
                &[
                    ("kind", Value::text(kind.name())),
                    ("files", Value(Repr::U64(cleanup.failed as u64))),
                ],
            );
        }
    }

    fn verbose_command(&mut self, cmd: VerboseCmd) {
        let Some(dir) = self.state.dir.clone() else {
            return;
        };
        let path = verbose::path(&dir);
        match cmd {
            VerboseCmd::On {
                filter,
                until,
                applied,
            } => {
                let _ = fs::write_replace(&path, verbose::encode(&filter, until).as_bytes());
                let msg = if applied {
                    "verbose logging on"
                } else {
                    "verbose logging saved"
                };
                self.note(
                    Level::Info,
                    msg,
                    &[
                        ("source", Value::text("setting")),
                        ("until", Value(Repr::Time(until))),
                    ],
                );
            }
            VerboseCmd::Off { applied } => {
                if fs::exists(&path) {
                    let _ = fs::remove_file(&path);
                }
                if applied {
                    self.close_verbose();
                    self.note(
                        Level::Info,
                        "verbose logging off",
                        &[("source", Value::text("setting"))],
                    );
                }
            }
        }
    }

    fn check_expiry(&mut self) {
        let expired = {
            let mut s = lock(&self.state.settings);
            let due =
                s.source == FilterSource::Setting && s.until.is_some_and(|u| u <= clock::now());
            if due {
                filter::activate(Filter::default());
                s.source = FilterSource::Default;
                s.until = None;
            }
            due
        };
        if !expired {
            return;
        }
        self.state.set_cap(RUN_CAP);
        if let Some(dir) = self.state.dir.clone() {
            let path = verbose::path(&dir);
            if fs::exists(&path) {
                let _ = fs::remove_file(&path);
            }
        }
        self.close_verbose();
        self.note(
            Level::Info,
            "verbose logging expired",
            &[("source", Value::text("setting"))],
        );
    }

    fn close_verbose(&mut self) {
        // Write what is buffered first: it may hold verbose records.
        self.write_pending();
        if let Some(files) = self.files.as_mut() {
            files.close_verbose();
        }
    }

    fn finish(&mut self, stop: Stop) {
        lock(&self.state.buffers).phase = Phase::Closed;
        if stop == Stop::Shutdown {
            self.write_pending();
            if let Some(files) = self.files.as_mut() {
                let dropped = self.shared.dropped.load(Ordering::Relaxed);
                let suppressed = callsite::SUPPRESSED.load(Ordering::Relaxed);
                files.end(dropped, suppressed);
                let _ = files.sync();
            }
        }
        self.files = None;
    }
}
