//! The highlighting of a diff's two sides (docs/design/review_view.md「高亮」).
//!
//! The only place that calls `syntax`'s `highlight::window`, and only inside
//! the task that runs on the executor ([`run`]); everything else here is a
//! pure function (the windows a side needs, the cache of spans) or a plain
//! value the views read. The cache is keyed by the content's blob OID, the
//! language's query version and the window (AGENTS.md architecture rule 5),
//! so one blob that appears in a step and in the final diff is parsed once.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use lectern_services::changes::SideText;
use lectern_services::project::BlobId;
use lectern_services::review::{DiffRow, FileRows};
use lectern_syntax::highlight::{self, Budget, Outcome, Source};
use lectern_syntax::limits::{FIRST_WINDOW_DEADLINE, WINDOW_MAX};
use lectern_syntax::window::JsonPad;
use lectern_syntax::{Languages, Rules, Token, WindowSpans};

use crate::plugin::Job;

/// The most the spans of all windows may weigh together (`WindowSpans::bytes`).
pub(crate) const CACHE_LIMIT: usize = 2 << 20;

/// The longest a window may be: `syntax`'s window ceiling (256 KiB).
pub(crate) const WINDOW_LIMIT: u64 = WINDOW_MAX;

/// One side of a diff.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Half {
    Old,
    New,
}

impl Half {
    pub(crate) const BOTH: [Half; 2] = [Half::Old, Half::New];
}

/// What the cache is keyed by.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct SpanKey {
    pub oid: BlobId,
    pub query_version: u64,
    pub window: Range<u64>,
}

/// The language of a path as far as the view needs to know it. A table
/// lookup (`syntax` builds the table without IO and compiles nothing).
#[derive(Clone, Debug)]
pub(crate) struct LangInfo {
    pub name: Arc<str>,
    pub query_version: u64,
    pub json: bool,
}

pub(crate) fn lang_of(path: &str) -> Option<LangInfo> {
    let languages = Languages::core();
    let lang = languages.for_path(path)?;
    Some(LangInfo {
        name: Arc::from(lang.name()),
        query_version: lang.query_version(),
        json: lang.rules() == Rules::Json,
    })
}

/// The bytes and OID of a side that can be highlighted: its text is there and
/// has an OID.
pub(crate) fn side_of(rows: &FileRows, half: Half) -> Option<(&BlobId, &Arc<[u8]>)> {
    let side = match half {
        Half::Old => &rows.old,
        Half::New => &rows.new,
    };
    match (&side.oid, &side.text) {
        (Some(oid), SideText::Text(bytes)) => Some((oid, bytes)),
        _ => None,
    }
}

/// The windows to highlight on one side of a diff: from the first row's line
/// start to the end of the last row's line (the lines the rows show), cut
/// at a row when the window would pass [`WINDOW_LIMIT`]. A JSON file is one
/// window of the whole text or none (its safe cut points are `syntax`'s
/// tiers B and C, which a diff does not use). A side without `*_byte`
/// (no text) has none.
pub(crate) fn plan_windows(rows: &FileRows, half: Half, json: bool) -> Vec<Range<u64>> {
    let Some((_, bytes)) = side_of(rows, half) else {
        return Vec::new();
    };
    let len = bytes.len() as u64;
    if json {
        return if len > 0 && len <= WINDOW_LIMIT {
            std::iter::once(0..len).collect()
        } else {
            Vec::new()
        };
    }
    let mut out: Vec<Range<u64>> = Vec::new();
    let mut current: Option<Range<u64>> = None;
    for row in rows.rows.iter() {
        let byte = match (row, half) {
            (DiffRow::Context { old_byte, .. }, Half::Old) => *old_byte,
            (DiffRow::Context { new_byte, .. }, Half::New) => *new_byte,
            (DiffRow::Removed { old_byte, .. }, Half::Old) => *old_byte,
            (DiffRow::Added { new_byte, .. }, Half::New) => *new_byte,
            _ => None,
        };
        let Some(start) = byte.filter(|byte| *byte < len) else {
            continue;
        };
        let end = line_end(bytes, start);
        current = match current.take() {
            None => Some(start..end),
            Some(window) => {
                if end.max(window.end) - start.min(window.start) > WINDOW_LIMIT {
                    out.push(window);
                    Some(start..end)
                } else {
                    Some(window.start.min(start)..window.end.max(end))
                }
            }
        };
    }
    out.extend(current);
    out.retain(|window| window.start < window.end);
    out
}

/// The offset of the end of the line that starts at `start` (before its
/// newline).
fn line_end(bytes: &[u8], start: u64) -> u64 {
    let rest = &bytes[start as usize..];
    start + rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len()) as u64
}

/// The text `highlight::window` reads, as `syntax`'s `Source`: the whole
/// side in one chunk, borrowed from the `Arc` `review` holds.
pub(crate) struct ArcSource(pub Arc<[u8]>);

impl Source for ArcSource {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn chunk(&self, at: u64) -> Option<&[u8]> {
        usize::try_from(at).ok().and_then(|at| self.0.get(at..))
    }
}

/// One window to highlight, for the executor.
pub(crate) struct Work {
    pub key: SpanKey,
    pub text: Arc<[u8]>,
    pub path: String,
    pub language: Arc<str>,
    pub cancel: Arc<AtomicBool>,
}

/// The task body (executor thread; blocking): parses the window alone and
/// highlights it within the first-screen deadline. Gives the job to deliver.
pub(crate) fn run(work: Work) -> Job {
    let Work {
        key,
        text,
        path,
        language,
        cancel,
    } = work;
    let bytes = key.window.end - key.window.start;
    let started = Instant::now();
    let unavailable = |unavailable: bool| Job::SpansUnavailable {
        key: key.clone(),
        unavailable,
        language: language.clone(),
        bytes,
        took_us: elapsed_us(started),
    };
    let languages = Languages::core();
    let Some(lang) = languages.for_path(&path) else {
        return unavailable(true);
    };
    if !lang.prepare() {
        return unavailable(true);
    }
    let outcome = highlight::window(
        lang,
        &ArcSource(text),
        key.window.clone(),
        &JsonPad::default(),
        key.window.clone(),
        Budget {
            budget: FIRST_WINDOW_DEADLINE,
            cancel: &cancel,
            yield_probe: None,
        },
    );
    match outcome {
        Outcome::Done(spans) => Job::Spans {
            key: key.clone(),
            spans: Arc::new(spans),
            took_us: elapsed_us(started),
        },
        Outcome::Unavailable => unavailable(true),
        Outcome::TimedOut | Outcome::Cancelled | Outcome::Yielded => unavailable(false),
    }
}

fn elapsed_us(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// The spans of windows, by key, least recently used out when they weigh
/// more than [`CACHE_LIMIT`].
pub(crate) struct SpansCache {
    entries: HashMap<SpanKey, (Arc<WindowSpans>, u64)>,
    bytes: usize,
    tick: u64,
    limit: usize,
}

impl SpansCache {
    pub(crate) fn new() -> SpansCache {
        SpansCache::with_limit(CACHE_LIMIT)
    }

    pub(crate) fn with_limit(limit: usize) -> SpansCache {
        SpansCache {
            entries: HashMap::new(),
            bytes: 0,
            tick: 0,
            limit,
        }
    }

    pub(crate) fn get(&mut self, key: &SpanKey) -> Option<Arc<WindowSpans>> {
        self.tick += 1;
        let tick = self.tick;
        let (spans, used) = self.entries.get_mut(key)?;
        *used = tick;
        Some(spans.clone())
    }

    pub(crate) fn insert(&mut self, key: SpanKey, spans: Arc<WindowSpans>) {
        self.tick += 1;
        let weight = spans.bytes();
        if let Some((old, _)) = self.entries.insert(key, (spans, self.tick)) {
            self.bytes -= old.bytes();
        }
        self.bytes += weight;
        while self.bytes > self.limit && self.entries.len() > 1 {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some((evicted, _)) = self.entries.remove(&oldest) {
                self.bytes -= evicted.bytes();
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, key: &SpanKey) -> bool {
        self.entries.contains_key(key)
    }
}

/// The spans that arrived for the two sides of one diff.
#[derive(Default, Clone)]
pub(crate) struct SideSpans {
    old: Vec<(Range<u64>, Arc<WindowSpans>)>,
    new: Vec<(Range<u64>, Arc<WindowSpans>)>,
}

impl SideSpans {
    pub(crate) fn push(&mut self, half: Half, window: Range<u64>, spans: Arc<WindowSpans>) {
        let list = match half {
            Half::Old => &mut self.old,
            Half::New => &mut self.new,
        };
        if !list.iter().any(|(range, _)| *range == window) {
            list.push((window, spans));
        }
    }

    /// The tokens of the line that starts at `byte`; empty when no window of
    /// that side holds it (plain text).
    pub(crate) fn line(&self, half: Half, byte: u64) -> &[Token] {
        let list = match half {
            Half::Old => &self.old,
            Half::New => &self.new,
        };
        list.iter()
            .find(|(range, _)| range.contains(&byte))
            .map_or(&[][..], |(_, spans)| spans.line(byte))
    }

    /// `spans` once a window of the side arrived, otherwise `plain`.
    pub(crate) fn state(&self, half: Half) -> &'static str {
        let list = match half {
            Half::Old => &self.old,
            Half::New => &self.new,
        };
        if list.is_empty() { "plain" } else { "spans" }
    }
}
