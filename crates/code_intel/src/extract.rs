//! Extraction of one blob's definitions and reference names, and the language
//! table it runs against. Pure and blocking functions for the scheduler,
//! tests and benchmarks (docs/design/code_intel.md「本 crate 的公开项」).

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tree_sitter_tags::{Error as TagsError, TagsContext};

use crate::DefKind;
use crate::rules::{self, RawTag};
use crate::util::log;
use crate::walk;
use crate::watchdog::{PauseProbe, Registration, Trip, Watchdog};

pub use crate::lang::{EXTRACTOR_VERSION, Lang, LanguageTable, compatible, language_for_path};

/// The per-file deadline of background extraction.
pub const BACKGROUND_DEADLINE: Duration = Duration::from_millis(250);
/// The per-file deadline of an extraction the user waits for.
pub const INTERACTIVE_DEADLINE: Duration = Duration::from_secs(1);
/// Background extraction skips larger content (`TooLarge`).
pub const BACKGROUND_MAX_LEN: usize = 1 << 20;
/// Interactive extraction of an opened file skips larger content.
pub const INTERACTIVE_MAX_LEN: usize = 4 << 20;
/// Content with a NUL in this many leading bytes is binary, as git decides.
pub const BINARY_PROBE: usize = 8000;

/// When an extraction gives up, and how much content it takes.
#[derive(Clone)]
pub struct Deadline {
    watchdog: Watchdog,
    at: Instant,
    max_len: usize,
    pause: Option<PauseProbe>,
}

impl Deadline {
    /// [`BACKGROUND_DEADLINE`] from now on the watchdog's clock, at most
    /// [`BACKGROUND_MAX_LEN`] bytes.
    pub fn background(watchdog: &Watchdog) -> Deadline {
        let at = watchdog.clock().now() + BACKGROUND_DEADLINE;
        Deadline::at(watchdog, at, BACKGROUND_MAX_LEN)
    }

    /// [`INTERACTIVE_DEADLINE`] from now, at most [`INTERACTIVE_MAX_LEN`]
    /// bytes.
    pub fn interactive(watchdog: &Watchdog) -> Deadline {
        let at = watchdog.clock().now() + INTERACTIVE_DEADLINE;
        Deadline::at(watchdog, at, INTERACTIVE_MAX_LEN)
    }

    /// An explicit deadline, e.g. the time-boxed search's
    /// `min(250 ms, time left)`.
    pub fn at(watchdog: &Watchdog, at: Instant, max_len: usize) -> Deadline {
        Deadline {
            watchdog: watchdog.clone(),
            at,
            max_len,
            pause: None,
        }
    }

    /// Also give up when `pause` returns true (the batch tiers pass
    /// `BackgroundGuard::pause_requested`); checked with the deadline.
    pub fn with_pause(mut self, pause: PauseProbe) -> Deadline {
        self.pause = Some(pause);
        self
    }

    pub fn instant(&self) -> Instant {
        self.at
    }

    pub fn max_len(&self) -> usize {
        self.max_len
    }
}

/// One blob's extraction under one key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Extraction {
    pub status: ExtractStatus,
    /// In name position order.
    pub defs: Vec<Def>,
    /// Reference names, deduplicated, in byte order. References the locals
    /// query resolves to a local definition are not here
    /// (tree-sitter-tags' `#is-not? local`).
    pub refs: Vec<Box<str>>,
}

impl Extraction {
    pub(crate) fn empty(status: ExtractStatus) -> Extraction {
        Extraction {
            status,
            defs: Vec::new(),
            refs: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ExtractStatus {
    Done,
    /// Past the deadline. Recorded and not retried; redone with the
    /// interactive deadline when the file is opened.
    TimedOut,
    /// Larger than the deadline's `max_len`.
    TooLarge,
    /// A NUL in the first [`BINARY_PROBE`] bytes.
    Binary,
    /// Cancelled, paused, or the watchdog stopped. Not written to the cache.
    Cancelled,
    /// The grammar or its queries do not compile; the language is disabled
    /// for this run. Not written to the cache.
    Unsupported,
}

impl ExtractStatus {
    /// The code stored in `ci_ext.status`; None for the statuses that are
    /// never stored.
    pub fn code(self) -> Option<u8> {
        match self {
            ExtractStatus::Done => Some(0),
            ExtractStatus::TimedOut => Some(1),
            ExtractStatus::TooLarge => Some(2),
            ExtractStatus::Binary => Some(3),
            ExtractStatus::Cancelled | ExtractStatus::Unsupported => None,
        }
    }
}

/// One definition.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Def {
    pub name: Box<str>,
    pub kind: DefKind,
    /// Row of the name, from 0.
    pub line: u32,
    /// Byte column of the name's start.
    pub column: u32,
    /// The last row of the definition node.
    pub end_line: u32,
    /// The innermost enclosing definition (for Rust, the type of the `impl`
    /// block; for Go methods, the receiver type).
    pub scope: Option<Box<str>>,
    pub visibility: Visibility,
    /// Parameters of a function or method, capped at 255.
    pub arity: Option<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Visibility {
    Public,
    /// This file only: JS / TS top-level definitions an ES module does not
    /// export and definitions inside function bodies, Java `private`.
    File,
    /// This directory only: Go package-level names starting in lower case.
    Package,
    /// This module and its children: Rust items without `pub`.
    Module,
    /// No rule (Python, language packs).
    Unknown,
}

impl Visibility {
    /// Bits 0–2 of `ci_def.flags`.
    pub fn code(self) -> u8 {
        match self {
            Visibility::Public => 0,
            Visibility::File => 1,
            Visibility::Package => 2,
            Visibility::Module => 3,
            Visibility::Unknown => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::File => "file",
            Visibility::Package => "package",
            Visibility::Module => "module",
            Visibility::Unknown => "unknown",
        }
    }
}

/// Whether content is binary: a NUL in its first [`BINARY_PROBE`] bytes.
pub fn is_binary(source: &[u8]) -> bool {
    source[..source.len().min(BINARY_PROBE)].contains(&0)
}

thread_local! {
    /// One parser and query cursor per thread, reused across extractions.
    static CONTEXT: RefCell<Option<TagsContext>> = const { RefCell::new(None) };
}

/// Extracts one blob. Blocking (parsing); any thread but the UI thread.
/// Returns `TimedOut` once the deadline passes and `Cancelled` once `cancel`
/// is set, the deadline's pause probe says so, or the watchdog stops; the
/// watchdog interrupts parsing within [`crate::CHECK_INTERVAL`].
pub fn extract(
    lang: &Lang,
    source: &[u8],
    deadline: &Deadline,
    cancel: &Arc<AtomicBool>,
) -> Extraction {
    if cancel.load(Ordering::Acquire) {
        return Extraction::empty(ExtractStatus::Cancelled);
    }
    if source.len() > deadline.max_len {
        return Extraction::empty(ExtractStatus::TooLarge);
    }
    if is_binary(source) {
        return Extraction::empty(ExtractStatus::Binary);
    }
    if lang.lease().is_static() && walk::supports(lang.dialect(), lang.language()) {
        return extract_walking(lang, source, deadline, cancel);
    }
    extract_tags(lang, source, deadline, cancel)
}

/// A core language: one walk over the tree (`crate::walk`, ADR 0031).
fn extract_walking(
    lang: &Lang,
    source: &[u8],
    deadline: &Deadline,
    cancel: &Arc<AtomicBool>,
) -> Extraction {
    let registration =
        deadline
            .watchdog
            .register(deadline.at, Some(cancel.clone()), deadline.pause.clone());
    if registration.slot().tripped().is_some() {
        return interrupted(&registration);
    }
    let walked = walk::extract(
        lang.dialect(),
        lang.language(),
        source,
        registration.slot().flag(),
    );
    if registration.slot().tripped().is_some() {
        return interrupted(&registration);
    }
    let Some(walked) = walked else {
        log(format_args!("parsing {} failed", lang.name().0));
        return Extraction::empty(ExtractStatus::Unsupported);
    };
    // The rules run under the same registration, and are checked once more
    // after: the deadline covers the whole extraction.
    let finished = rules::finish_with_refs(
        lang.dialect(),
        source,
        walked.tags,
        walked.refs,
        &walked.local_scopes,
    );
    if registration.slot().tripped().is_some() {
        return interrupted(&registration);
    }
    drop(registration);
    Extraction {
        status: ExtractStatus::Done,
        defs: finished.defs.into_iter().map(|d| d.def).collect(),
        refs: finished.refs,
    }
}

/// tree-sitter-tags with the language's queries: language packs, and the
/// reference for the walkers' tests.
pub(crate) fn extract_tags(
    lang: &Lang,
    source: &[u8],
    deadline: &Deadline,
    cancel: &Arc<AtomicBool>,
) -> Extraction {
    if cancel.load(Ordering::Acquire) {
        return Extraction::empty(ExtractStatus::Cancelled);
    }
    if source.len() > deadline.max_len {
        return Extraction::empty(ExtractStatus::TooLarge);
    }
    if is_binary(source) {
        return Extraction::empty(ExtractStatus::Binary);
    }
    let Some(compiled) = lang.compiled() else {
        return Extraction::empty(ExtractStatus::Unsupported);
    };
    let registration =
        deadline
            .watchdog
            .register(deadline.at, Some(cancel.clone()), deadline.pause.clone());
    // tree-sitter checks the flag only every so many parse steps and tags,
    // so a flag set before the start or near the end is checked here too.
    if registration.slot().tripped().is_some() {
        return interrupted(&registration);
    }
    let tags = CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let context = context.get_or_insert_with(TagsContext::new);
        let flag = registration.slot().flag();
        let (iter, _) = context.generate_tags(&compiled.tags, source, Some(flag))?;
        let mut tags = Vec::new();
        for tag in iter {
            let tag = tag?;
            tags.push(RawTag {
                class: compiled.class(tag.syntax_type_id, tag.is_definition),
                range: tag.range,
                name_range: tag.name_range,
                row: u32::try_from(tag.span.start.row).unwrap_or(u32::MAX),
                column: u32::try_from(tag.span.start.column).unwrap_or(u32::MAX),
                params: tag.docs,
            });
        }
        Ok::<_, TagsError>(tags)
    });
    // The thread's parser keeps a pointer to the language it last parsed.
    // A language pack can be unloaded once its lease is gone, so the context
    // goes now, while this call still holds the lease through `lang`.
    if !lang.lease().is_static() {
        CONTEXT.with(|context| drop(context.borrow_mut().take()));
    }
    let tags = match tags {
        Ok(tags) => tags,
        Err(TagsError::Cancelled) => return interrupted(&registration),
        Err(error) => {
            log(format_args!("extracting {} failed: {error}", lang.name().0));
            return Extraction::empty(ExtractStatus::Unsupported);
        }
    };
    // A core language's local scopes come from its locals query, as in
    // `ParsedFile` (this path is the walkers' reference, and the fallback
    // when a grammar has no walker); a language pack's scopes are not
    // classified.
    let scopes = if lang.lease().is_static() {
        let flag = registration.slot().flag();
        crate::site::local_scopes(lang, source, &|| flag.load(Ordering::Acquire) != 0)
    } else {
        None
    };
    if registration.slot().tripped().is_some() {
        return interrupted(&registration);
    }
    let finished = rules::finish(lang.dialect(), source, tags, scopes.as_deref());
    if registration.slot().tripped().is_some() {
        return interrupted(&registration);
    }
    drop(registration);
    Extraction {
        status: ExtractStatus::Done,
        defs: finished.defs.into_iter().map(|d| d.def).collect(),
        refs: finished.refs,
    }
}

fn interrupted(registration: &Registration) -> Extraction {
    Extraction::empty(match registration.slot().tripped() {
        Some(Trip::Deadline) => ExtractStatus::TimedOut,
        _ => ExtractStatus::Cancelled,
    })
}
