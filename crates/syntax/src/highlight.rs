//! Parsing and highlighting (docs/design/file_reader.md「解析与高亮」).
//! Blocking; executor threads only, never the UI thread.
//!
//! Three entry points share one capture resolution:
//!
//! - [`window`]: the first screen's window parsed alone, only its visible
//!   part highlighted. One call, not resumable.
//! - [`FullParse`]: tier A's whole file, in slices.
//! - [`ChainWindow`]: one window of tiers B and C's chain, cut by its tree,
//!   in slices.
//!
//! How captures become tokens: a node captured by several patterns takes
//! the first or the last of them, as the language's query header says; nested
//! captures colour their own range over the capture around them (the
//! innermost wins), and `@none` / `@embedded` leave theirs uncoloured. Ranges
//! are then split at line ends.
//!
//! The sliced parses keep tree-sitter's parser between steps (a parse halted
//! by the progress callback resumes on the next call with the same input),
//! then run the highlight query over the tree in byte chunks
//! (`QueryCursor::set_byte_range`), so a step stops at a chunk's end and the
//! next step goes on from there. Every capture that colours a byte of a
//! chunk intersects the chunk, so each chunk resolves its bytes as one query
//! over the whole tree would; a segment cut by a chunk's end is joined with
//! its rest at the next chunk's start. The chunks' tokens are those of one
//! query.
//!
//! Every call and every step registers with [`health`] right before it
//! enters tree-sitter, for the budget it was given, and unregisters when it
//! returns, with any result: between steps nothing is registered.
//!
//! Edit mode's incremental parse (`incremental`) keeps a tree between edits
//! and runs the query again only on the lines an edit changed.

mod incremental;

pub(crate) use incremental::EditParse;
#[cfg(test)]
pub(crate) use incremental::input_edit;

use std::cell::Cell;
use std::ops::{ControlFlow, Range};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tree_sitter::{
    Node, ParseOptions, Parser, QueryCursor, QueryCursorOptions, StreamingIterator, Tree,
};

use crate::cut::{self, Bytes};
use crate::health::Elapsed;
use crate::health::{self, Health};
use crate::lang::{Capture, Compiled, Lang, LangHandle, Precedence, Rules};
use crate::limits::{SLOW_WINDOW, SLOW_WINDOW_WALL, WINDOW, WINDOW_MAX};
use crate::spans::{Lines, SpansBuilder, TokenKind, WindowSpans};
use crate::window::{JsonPad, json_last_comma, json_pad};

/// A query chunk: 8 KiB, about 1 ms of query in ordinary code and 2–4 ms in
/// minified code (large-file research「按字节范围的高亮查询」). A sliced
/// step's first chunk is not timed, so a step overruns its budget by at
/// most one chunk.
const QUERY_CHUNK: usize = 8 << 10;

/// Text read by byte ranges, not necessarily stored in one piece (chunks,
/// pages).
pub trait Source: Send + Sync {
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Contiguous bytes from `at`; None when they are not in memory (a parse
    /// takes that as the end of the text: callers only parse what is read).
    fn chunk(&self, at: u64) -> Option<&[u8]>;
}

impl Source for Vec<u8> {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }

    fn chunk(&self, at: u64) -> Option<&[u8]> {
        usize::try_from(at).ok().and_then(|at| self.get(at..))
    }
}

/// One call's run budget (a duration from the call's start, not a
/// deadline), cancel flag and yield probe (`BackgroundGuard::yield_requested`).
/// Every call registers with [`health`] for `budget` and unregisters when it
/// returns, with any result.
pub struct Budget<'a> {
    pub budget: Duration,
    pub cancel: &'a AtomicBool,
    pub yield_probe: Option<&'a dyn Fn() -> bool>,
}

/// How a call ended. `TimedOut`: the budget ran out; `Yielded`: the yield
/// probe asked to stop. A sliced parse keeps its state after both (and after
/// `Cancelled`): the next `step` goes on from where it stopped.
/// `Unavailable`: the grammar version is disabled by [`health`], or the
/// language's query does not compile; the caller shows plain text.
#[derive(Debug)]
pub enum Outcome<T> {
    Done(T),
    TimedOut,
    Cancelled,
    Yielded,
    Unavailable,
}

impl<T> Outcome<T> {
    pub fn done(self) -> Option<T> {
        match self {
            Outcome::Done(value) => Some(value),
            _ => None,
        }
    }
}

/// Highlights the visible part of a window parsed alone: the window's
/// bytes from `src` with `pad` around them (a JSON window's brackets), the
/// tokens of `visible` (inside the window) only, [`WindowSpans::exact`]
/// false. The first screen's call: one call with the first-screen deadline
/// ([`FIRST_WINDOW_DEADLINE`](crate::limits::FIRST_WINDOW_DEADLINE)), not
/// resumable; after `TimedOut` the screen shows plain text and the window is
/// left to the tier's later work. Compiling the language's query on first
/// use is not counted in the budget.
pub fn window(
    lang: &Lang,
    src: &dyn Source,
    window: Range<u64>,
    pad: &JsonPad,
    visible: Range<u64>,
    budget: Budget<'_>,
) -> Outcome<WindowSpans> {
    window_in(health::global(), lang, src, window, pad, visible, budget)
}

pub(crate) fn window_in(
    health: &Health,
    lang: &Lang,
    src: &dyn Source,
    window: Range<u64>,
    pad: &JsonPad,
    visible: Range<u64>,
    budget: Budget<'_>,
) -> Outcome<WindowSpans> {
    let Some(compiled) = lang.compiled() else {
        return Outcome::Unavailable;
    };
    let mut parser = Parser::new();
    if parser.set_language(lang.language()).is_err() {
        return Outcome::Unavailable;
    }
    let end = window.end.min(src.len());
    let start = window.start.min(end);
    let visible = visible.start.clamp(start, end)..visible.end.clamp(start, end);
    let input = Input {
        src,
        start,
        end,
        before: &pad.before,
        after: &pad.after,
    };
    let watch = Watch::new(&budget);
    let Some(_guard) = health.enter(lang.name(), lang.grammar_version(), budget.budget) else {
        return Outcome::Unavailable;
    };
    let tree = match parse_step(&mut parser, input, &watch) {
        Ok(tree) => tree,
        Err(halt) => return halt.outcome(),
    };
    let offset = pad.before.len() + (visible.start - start) as usize;
    let len = (visible.end - visible.start) as usize;
    let mut query = Querying::new(tree, input, offset, len, offset + len, visible.start);
    match query.run(lang.precedence(), compiled, input, &watch, false) {
        Ok(()) => Outcome::Done(query.builder.finish(false)),
        Err(halt) => halt.outcome(),
    }
}

/// Tier A's whole-file parse, in slices: each [`FullParse::step`] runs for
/// at most its budget ([`SLICE`](crate::limits::SLICE)) plus at most one
/// query chunk, and keeps the parser's state between steps. When the parse
/// is done the query runs over the tree in chunks, and the last step returns
/// the tokens of the whole text ([`WindowSpans::exact`]) and drops the tree.
/// A changed text needs a new `FullParse` (tree-sitter must not resume on
/// other input).
pub struct FullParse {
    lang: LangHandle,
    health: Health,
    src: Arc<dyn Source>,
    state: State,
}

enum State {
    Parsing(Parser),
    Querying(Querying),
    /// Done, or the grammar or its query is unusable.
    Over,
}

impl FullParse {
    /// Compiles the language's query on first use (blocking; not counted in
    /// any step's budget). Background threads only.
    pub fn new(lang: &Lang, src: Arc<dyn Source>) -> FullParse {
        FullParse::new_in(health::global().handle(), lang, src)
    }

    pub(crate) fn new_in(health: Health, lang: &Lang, src: Arc<dyn Source>) -> FullParse {
        let lang = lang.handle();
        let state = new_parser(&lang).map_or(State::Over, State::Parsing);
        FullParse {
            lang,
            health,
            src,
            state,
        }
    }

    /// One slice. `Done` once; a step after `Done` or `Unavailable` returns
    /// `Unavailable`.
    pub fn step(&mut self, budget: Budget<'_>) -> Outcome<WindowSpans> {
        let Some(compiled) = self.lang.compiled.clone() else {
            return Outcome::Unavailable;
        };
        if matches!(self.state, State::Over) {
            return Outcome::Unavailable;
        }
        let len = self.src.len();
        let input = Input {
            src: &*self.src,
            start: 0,
            end: len,
            before: &[],
            after: &[],
        };
        let watch = Watch::new(&budget);
        let Some(_guard) =
            self.health
                .enter(self.lang.name, self.lang.grammar_version, budget.budget)
        else {
            return Outcome::Unavailable;
        };
        if let State::Parsing(parser) = &mut self.state {
            match parse_step(parser, input, &watch) {
                Ok(tree) => {
                    let len = len as usize;
                    self.state = State::Querying(Querying::new(tree, input, 0, len, len, 0));
                }
                Err(halt) => {
                    if halt == Halt::Unavailable {
                        self.state = State::Over;
                    }
                    return halt.outcome();
                }
            }
        }
        let State::Querying(query) = &mut self.state else {
            return Outcome::Unavailable;
        };
        match query.run(self.lang.precedence, &compiled, input, &watch, true) {
            Ok(()) => match std::mem::replace(&mut self.state, State::Over) {
                State::Querying(query) => Outcome::Done(query.builder.finish(true)),
                _ => Outcome::Unavailable,
            },
            Err(halt) => halt.outcome(),
        }
    }
}

/// One window of tiers B and C's chain: from `start`, about 64 KiB
/// ([`WINDOW`]) parsed alone and cut by its own tree (code_intel's window
/// rule, `crate::cut`); widened once to [`WINDOW_MAX`] when no complete node
/// ends before its end, then cut after the last token, then at the first
/// identifier boundary. A JSON window is cut after its last comma instead and
/// parsed with the brackets around it ([`JsonPad`]), like the first screen's.
/// `Done` gives the tokens from `start` to the cut ([`WindowSpans::exact`])
/// and the next window's start. Sliced like [`FullParse`]: after `TimedOut`
/// or `Yielded` the same object goes on from where it stopped, so a batch
/// task can stop between steps and keep it.
pub struct ChainWindow {
    lang: LangHandle,
    health: Health,
    src: Arc<dyn Source>,
    start: u64,
    state: Chain,
    spent: Spent,
}

enum Chain {
    Parsing {
        parser: Parser,
        /// Widened to [`WINDOW_MAX`].
        wide: bool,
        extent: Extent,
    },
    Querying {
        query: Querying,
        extent: Extent,
        next: u64,
    },
    Over,
}

/// What of a window is read and parsed.
struct Extent {
    /// The window read: `[start, end)`.
    end: u64,
    /// Parsed: its first `parsed` bytes, with `pad` around them.
    parsed: usize,
    pad: JsonPad,
    /// The cut, when it does not come from the tree (a JSON window, the
    /// file's end).
    cut: Option<usize>,
}

impl Extent {
    fn input<'s>(&'s self, src: &'s dyn Source, start: u64) -> Input<'s> {
        Input {
            src,
            start,
            end: start + self.parsed as u64,
            before: &self.pad.before,
            after: &self.pad.after,
        }
    }
}

impl ChainWindow {
    /// Compiles the language's query on first use, like [`FullParse::new`].
    pub fn new(lang: &Lang, src: Arc<dyn Source>, start: u64) -> ChainWindow {
        ChainWindow::new_in(health::global().handle(), lang, src, start)
    }

    pub(crate) fn new_in(
        health: Health,
        lang: &Lang,
        src: Arc<dyn Source>,
        start: u64,
    ) -> ChainWindow {
        let lang = lang.handle();
        let start = start.min(src.len());
        let state = match new_parser(&lang) {
            Some(parser) => Chain::Parsing {
                parser,
                wide: false,
                extent: window_extent(&*src, start, WINDOW, lang.rules),
            },
            None => Chain::Over,
        };
        ChainWindow {
            lang,
            health,
            src,
            start,
            state,
            spent: Spent::default(),
        }
    }

    /// What this window's parse took so far, for [`SlowWindows`]: the
    /// thread's CPU time inside tree-sitter's parse, summed over the steps
    /// (not the query, the cut or the waits between steps).
    pub fn spent(&self) -> Spent {
        self.spent
    }

    /// One slice. `Done` once; a step after `Done` or `Unavailable` returns
    /// `Unavailable`.
    pub fn step(&mut self, budget: Budget<'_>) -> Outcome<(WindowSpans, u64)> {
        let Some(compiled) = self.lang.compiled.clone() else {
            return Outcome::Unavailable;
        };
        if matches!(self.state, Chain::Over) {
            return Outcome::Unavailable;
        }
        let src = &*self.src;
        let start = self.start;
        let watch = Watch::new(&budget);
        let Some(_guard) =
            self.health
                .enter(self.lang.name, self.lang.grammar_version, budget.budget)
        else {
            return Outcome::Unavailable;
        };
        let rules = self.lang.rules;
        while let Chain::Parsing {
            parser,
            wide,
            extent,
        } = &mut self.state
        {
            let input = extent.input(src, start);
            let stopwatch = self.health.stopwatch();
            let parse = parse_step(parser, input, &watch);
            self.spent.add(stopwatch.elapsed());
            let tree = match parse {
                Ok(tree) => tree,
                Err(halt) => {
                    if halt == Halt::Unavailable {
                        self.state = Chain::Over;
                    }
                    return halt.outcome();
                }
            };
            let parsed = extent.parsed;
            self.spent.parsed += parsed as u64;
            let cut = if let Some(cut) = extent.cut {
                Some(cut)
            } else if !*wide {
                cut::tree_cut(&tree, &input, parsed)
            } else {
                cut::tree_cut(&tree, &input, parsed).or_else(|| cut::leaf_cut(&tree, parsed))
            };
            let cut = match cut {
                Some(cut) => cut,
                None if !*wide => {
                    // Widened once, parsed from the start again.
                    parser.reset();
                    *extent = window_extent(src, start, WINDOW_MAX, rules);
                    *wide = true;
                    continue;
                }
                None => (ident_end(src, extent.end) - start) as usize,
            };
            let offset = extent.pad.before.len();
            let query = Querying::new(tree, input, offset, cut, offset + cut.min(parsed), start);
            let Chain::Parsing { extent, .. } = std::mem::replace(&mut self.state, Chain::Over)
            else {
                unreachable!("the state is Parsing");
            };
            self.state = Chain::Querying {
                query,
                extent,
                next: start + cut as u64,
            };
        }
        let Chain::Querying { query, extent, .. } = &mut self.state else {
            return Outcome::Unavailable;
        };
        let input = extent.input(src, start);
        match query.run(self.lang.precedence, &compiled, input, &watch, true) {
            Ok(()) => match std::mem::replace(&mut self.state, Chain::Over) {
                Chain::Querying { query, next, .. } => {
                    Outcome::Done((query.builder.finish(true), next))
                }
                _ => Outcome::Unavailable,
            },
            Err(halt) => halt.outcome(),
        }
    }
}

/// What a window's parse took ([`ChainWindow::spent`]): tree-sitter's parse
/// only, as ADR 0034's 128 ms is a parse throughput (under 0.5 MiB/s);
/// the query and the cut are not counted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Spent {
    /// The bytes parsed: the window's, plus the first attempt's when it was
    /// widened.
    pub parsed: u64,
    /// The thread's CPU time in the parse. It does not grow while a batch
    /// thread at utility QoS waits for a CPU (a concurrent build), so a
    /// starved window is not taken for a slow one.
    pub cpu: Duration,
    /// Wall time of the steps whose thread's CPU clock could not be read.
    pub wall: Duration,
}

impl Spent {
    fn add(&mut self, elapsed: Elapsed) {
        match elapsed {
            Elapsed::Cpu(d) => self.cpu += d,
            Elapsed::Wall(d) => self.wall += d,
        }
    }
}

/// ADR 0034's degradation: a window whose parse takes longer than
/// [`SLOW_WINDOW`] of CPU time per 64 KiB parsed (wall time where the CPU
/// clock cannot be read: [`SLOW_WINDOW_WALL`]), twice in a row, turns the
/// file's highlighting off for this opening. Fed by the caller with each
/// finished window of the chain.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SlowWindows {
    in_a_row: u8,
}

impl SlowWindows {
    /// A finished window's [`ChainWindow::spent`]. True once two windows in
    /// a row were slow: plain text from now on.
    pub fn record(&mut self, spent: Spent) -> bool {
        // The share of the limit used: CPU time against `SLOW_WINDOW`, wall
        // time against `SLOW_WINDOW_WALL`, for each 64 KiB parsed (at least
        // one: a short last window has the whole limit).
        let units = (spent.parsed as f64 / WINDOW as f64).max(1.0);
        let used = spent.cpu.as_secs_f64() / SLOW_WINDOW.as_secs_f64()
            + spent.wall.as_secs_f64() / SLOW_WINDOW_WALL.as_secs_f64();
        if self.in_a_row < 2 {
            self.in_a_row = if used > units { self.in_a_row + 1 } else { 0 };
        }
        self.plain()
    }

    pub fn plain(&self) -> bool {
        self.in_a_row >= 2
    }
}

/// A new parser for the language; None when the language cannot be set or
/// its query does not compile.
fn new_parser(lang: &LangHandle) -> Option<Parser> {
    lang.compiled.as_ref()?;
    let mut parser = Parser::new();
    parser.set_language(&lang.language).ok()?;
    Some(parser)
}

/// The window from `start` of at most `max` bytes: where its read ends and
/// how much of it to parse. At the file's end all of it, cut there. JSON: up
/// to its last comma, cut after it, with the pad the bytes need. Otherwise
/// up to a line start in its second half (`cut::parse_len`), cut by the
/// tree.
fn window_extent(src: &dyn Source, start: u64, max: u64, rules: Rules) -> Extent {
    let end = (start + max).min(src.len());
    let read = (end - start) as usize;
    let input = Input::of(src, start, end);
    if rules == Rules::Json {
        let bytes: Vec<u8> = NodeText {
            input,
            at: 0,
            end: read,
        }
        .flatten()
        .copied()
        .collect();
        let (parsed, cut) = if end == src.len() {
            (bytes.len(), Some(bytes.len()))
        } else {
            match json_last_comma(&bytes) {
                Some(comma) => (comma, Some(comma + 1)),
                // No comma: cut by the tree like code, without a pad.
                None => (cut::parse_len(&input, read, input.line_starts()), None),
            }
        };
        let pad = if cut.is_some() {
            json_pad(&bytes[..parsed], start > 0)
        } else {
            JsonPad::default()
        };
        return Extent {
            end,
            parsed,
            pad,
            cut,
        };
    }
    let (parsed, cut) = if end == src.len() {
        (read, Some(read))
    } else {
        (cut::parse_len(&input, read, input.line_starts()), None)
    };
    Extent {
        end,
        parsed,
        pad: JsonPad::default(),
        cut,
    }
}

/// The first identifier boundary at or after `at` (code_intel's last
/// resort for a window with no token): the file's end, or the end of what
/// is in memory, when that comes first.
fn ident_end(src: &dyn Source, at: u64) -> u64 {
    let mut at = at;
    while at < src.len() {
        let Some(chunk) = src.chunk(at).filter(|c| !c.is_empty()) else {
            return at;
        };
        match chunk.iter().position(|&b| !cut::ident(b)) {
            Some(i) => return at + i as u64,
            None => at += chunk.len() as u64,
        }
    }
    src.len()
}

/// Why a call stopped early.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stop {
    TimedOut,
    Cancelled,
    Yielded,
}

/// A step that did not finish.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Halt {
    Stopped(Stop),
    /// tree-sitter returned no tree without being stopped: the grammar did
    /// not load.
    Unavailable,
}

impl Halt {
    fn outcome<T>(self) -> Outcome<T> {
        match self {
            Halt::Stopped(Stop::TimedOut) => Outcome::TimedOut,
            Halt::Stopped(Stop::Cancelled) => Outcome::Cancelled,
            Halt::Stopped(Stop::Yielded) => Outcome::Yielded,
            Halt::Unavailable => Outcome::Unavailable,
        }
    }
}

/// The budget's checks, run from tree-sitter's progress callbacks and
/// between query chunks.
struct Watch<'b, 'a> {
    budget: &'b Budget<'a>,
    started: Instant,
    /// False while the time budget is not checked (the first query chunk
    /// of a sliced step, so that every step makes progress).
    timed: Cell<bool>,
    stopped: Cell<Option<Stop>>,
}

impl<'b, 'a> Watch<'b, 'a> {
    fn new(budget: &'b Budget<'a>) -> Watch<'b, 'a> {
        Watch {
            budget,
            started: Instant::now(),
            timed: Cell::new(true),
            stopped: Cell::new(None),
        }
    }

    fn check(&self) -> ControlFlow<()> {
        let stop = if self.budget.cancel.load(Ordering::Relaxed) {
            Stop::Cancelled
        } else if self.budget.yield_probe.is_some_and(|probe| probe()) {
            Stop::Yielded
        } else if self.timed.get() && self.started.elapsed() > self.budget.budget {
            Stop::TimedOut
        } else {
            return ControlFlow::Continue(());
        };
        self.stopped.set(Some(stop));
        ControlFlow::Break(())
    }

    fn halt(&self) -> Halt {
        self.stopped.get().map_or(Halt::Unavailable, Halt::Stopped)
    }
}

/// What a parse reads: `[start, end)` of a source with padding before and
/// after it. Positions are offsets in this input.
#[derive(Clone, Copy)]
struct Input<'s> {
    src: &'s dyn Source,
    start: u64,
    end: u64,
    before: &'s [u8],
    after: &'s [u8],
}

impl<'s> Input<'s> {
    fn of(src: &'s dyn Source, start: u64, end: u64) -> Input<'s> {
        Input {
            src,
            start,
            end,
            before: &[],
            after: &[],
        }
    }

    fn middle(&self) -> usize {
        (self.end - self.start) as usize
    }

    fn len(&self) -> usize {
        self.before.len() + self.middle() + self.after.len()
    }

    /// Contiguous bytes from `at`; empty at the end (or where the source
    /// has nothing in memory).
    fn chunk(&self, at: usize) -> &'s [u8] {
        if at < self.before.len() {
            return &self.before[at..];
        }
        let at = at - self.before.len();
        let middle = self.middle();
        if at < middle {
            return match self.src.chunk(self.start + at as u64) {
                Some(chunk) => &chunk[..chunk.len().min(middle - at)],
                None => &[],
            };
        }
        self.after.get(at - middle..).unwrap_or(&[])
    }

    /// The first `\n` at or after `from`, or the input's end.
    fn newline(&self, from: usize) -> usize {
        let mut at = from;
        loop {
            let chunk = self.chunk(at);
            if chunk.is_empty() {
                return at.min(self.len());
            }
            match memchr::memchr(b'\n', chunk) {
                Some(i) => return at + i,
                None => at += chunk.len(),
            }
        }
    }

    /// The line starts after the first byte, ascending.
    fn line_starts(&self) -> impl Iterator<Item = usize> + '_ {
        let mut at = 0;
        std::iter::from_fn(move || {
            let newline = self.newline(at);
            (newline < self.len()).then(|| {
                at = newline + 1;
                at
            })
        })
    }
}

impl Bytes for Input<'_> {
    fn byte(&self, at: usize) -> Option<u8> {
        self.chunk(at).first().copied()
    }
}

/// The bytes of a node, for the query's predicates.
struct NodeText<'s> {
    input: Input<'s>,
    at: usize,
    end: usize,
}

impl<'s> Iterator for NodeText<'s> {
    type Item = &'s [u8];

    fn next(&mut self) -> Option<&'s [u8]> {
        if self.at >= self.end {
            return None;
        }
        let chunk = self.input.chunk(self.at);
        if chunk.is_empty() {
            return None;
        }
        let take = chunk.len().min(self.end - self.at);
        self.at += take;
        Some(&chunk[..take])
    }
}

/// The input from `offset` on, `len` bytes: the text a [`SpansBuilder`]
/// splits into lines.
struct View<'s> {
    input: Input<'s>,
    offset: usize,
    len: usize,
}

impl Lines for View<'_> {
    fn len(&self) -> usize {
        self.len
    }

    fn line_end(&self, from: usize) -> usize {
        self.input
            .newline(self.offset + from)
            .saturating_sub(self.offset)
            .min(self.len)
    }
}

/// Parses `input`, or resumes the parse the progress callback halted last
/// time (same parser, same input).
fn parse_step(parser: &mut Parser, input: Input<'_>, watch: &Watch<'_, '_>) -> Result<Tree, Halt> {
    parse_from(parser, input, None, watch)
}

/// [`parse_step`] reusing `old`, a tree of the text before an edit that
/// was edited to match this input (`Tree::edit`).
fn parse_from(
    parser: &mut Parser,
    input: Input<'_>,
    old: Option<&Tree>,
    watch: &Watch<'_, '_>,
) -> Result<Tree, Halt> {
    let mut progress = |_: &tree_sitter::ParseState| watch.check();
    let len = input.len();
    let tree = parser.parse_with_options(
        &mut |at, _| if at < len { input.chunk(at) } else { &[] },
        old,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    match tree {
        Some(tree) => {
            // Finished as the callback asked to stop: the tree stands.
            watch.stopped.set(None);
            Ok(tree)
        }
        None => Err(watch.halt()),
    }
}

/// The query over a tree, resumable at chunk boundaries.
struct Querying {
    tree: Tree,
    /// In input positions: the next chunk's start and where the query ends.
    at: usize,
    end: usize,
    /// The input position of the tokens' start, and the tokens' length.
    offset: usize,
    len: usize,
    builder: SpansBuilder,
    /// The last chunk's last segment went on past its end.
    carry: bool,
}

impl Querying {
    fn new(
        tree: Tree,
        input: Input<'_>,
        offset: usize,
        len: usize,
        end: usize,
        start: u64,
    ) -> Querying {
        let view = View { input, offset, len };
        Querying {
            tree,
            at: offset,
            end: end.max(offset),
            offset,
            len,
            builder: SpansBuilder::new(&view, start),
            carry: false,
        }
    }

    /// Runs the query chunk by chunk until its end, or until the watch
    /// stops it (the chunk in flight is run again next time). With
    /// `first_free` the first chunk of the call is not timed, so a sliced
    /// parse always makes progress; cancel and yield still stop it.
    fn run(
        &mut self,
        precedence: Precedence,
        compiled: &Compiled,
        input: Input<'_>,
        watch: &Watch<'_, '_>,
        first_free: bool,
    ) -> Result<(), Halt> {
        let view = View {
            input,
            offset: self.offset,
            len: self.len,
        };
        let mut first = true;
        while self.at < self.end {
            let (from, to) = (self.at, (self.at + QUERY_CHUNK).min(self.end));
            watch.timed.set(!(first && first_free));
            let hits = captures(precedence, compiled, &self.tree, input, from..to, watch);
            watch.timed.set(true);
            let Some(hits) = hits else {
                return Err(watch.halt());
            };
            let (offset, builder, carry) = (self.offset, &mut self.builder, &mut self.carry);
            let continues = std::mem::take(carry);
            resolve(hits, |a, b, kind| {
                let (clipped_a, clipped_b) = (a.max(from), b.min(to));
                if clipped_a >= clipped_b {
                    return;
                }
                if continues && clipped_a == from {
                    builder.push_rest(&view, clipped_a - offset, clipped_b - offset, kind);
                } else {
                    builder.push(&view, clipped_a - offset, clipped_b - offset, kind);
                }
                // Cut by this chunk's end: the next chunk's first segment is
                // its rest.
                *carry = b > to;
            });
            self.at = to;
            first = false;
            if self.at < self.end && watch.check().is_break() {
                return Err(watch.halt());
            }
        }
        Ok(())
    }
}

/// One accepted capture: a node's range and what colours it.
#[derive(Clone, Copy)]
struct Hit {
    start: usize,
    end: usize,
    /// Among captures of the same range, the highest wins.
    priority: usize,
    capture: Capture,
}

/// The captures of the query's matches that intersect `range`. None when
/// the watch stopped it.
fn captures(
    precedence: Precedence,
    compiled: &Compiled,
    tree: &Tree,
    input: Input<'_>,
    range: Range<usize>,
    watch: &Watch<'_, '_>,
) -> Option<Vec<Hit>> {
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range);
    let mut progress = |_: &tree_sitter::QueryCursorState| watch.check();
    let text = |node: Node<'_>| NodeText {
        input,
        at: node.start_byte(),
        end: node.end_byte(),
    };
    let mut captures = cursor.captures_with_options(
        &compiled.query,
        tree.root_node(),
        text,
        QueryCursorOptions::new().progress_callback(&mut progress),
    );
    let mut hits = Vec::new();
    while let Some((found, index)) = captures.next() {
        let capture = found.captures()[*index];
        let what = compiled.captures[capture.index as usize];
        if what == Capture::Ignore {
            continue;
        }
        let range = capture.node.byte_range();
        if range.is_empty() {
            continue;
        }
        let priority = match precedence {
            Precedence::First => usize::MAX - found.pattern_index,
            Precedence::Last => found.pattern_index,
        };
        hits.push(Hit {
            start: range.start,
            end: range.end,
            priority,
            capture: what,
        });
    }
    drop(captures);
    if watch.stopped.get().is_some() {
        return None;
    }
    Some(hits)
}

/// Turns the captures into ascending, non-overlapping segments: of the
/// captures with the same range the highest priority wins, and an inner
/// range wins over the range around it.
fn resolve(mut hits: Vec<Hit>, mut emit: impl FnMut(usize, usize, TokenKind)) {
    // Outer ranges before the ranges they contain; equal ranges by priority,
    // so the last of a run of equal ranges is the winner.
    hits.sort_unstable_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(b.end.cmp(&a.end))
            .then(a.priority.cmp(&b.priority))
    });
    hits.dedup_by(|later, earlier| {
        let same = later.start == earlier.start && later.end == earlier.end;
        if same {
            *earlier = *later;
        }
        same
    });
    // The open captures, outermost first; `at` is where the next segment
    // starts.
    let mut open: Vec<(usize, Capture)> = Vec::new();
    let mut at = 0;
    let mut segment = |from: usize, to: usize, capture: Capture| {
        if let Capture::Kind(kind) = capture
            && from < to
        {
            emit(from, to, kind);
        }
    };
    for hit in hits {
        while let Some(&(end, capture)) = open.last() {
            if end > hit.start {
                break;
            }
            segment(at, end, capture);
            at = at.max(end);
            open.pop();
        }
        if let Some(&(end, capture)) = open.last() {
            segment(at, hit.start, capture);
            // A capture that would cross the end of the one around it (never
            // for tree nodes) is cut there.
            open.push((hit.end.min(end), hit.capture));
        } else {
            open.push((hit.end, hit.capture));
        }
        at = at.max(hit.start);
    }
    while let Some((end, capture)) = open.pop() {
        segment(at, end, capture);
        at = at.max(end);
    }
}

#[cfg(test)]
pub(crate) fn resolve_for_tests(
    hits: &[(usize, usize, usize, Option<TokenKind>)],
) -> Vec<(usize, usize, TokenKind)> {
    let hits = hits
        .iter()
        .map(|&(start, end, priority, kind)| Hit {
            start,
            end,
            priority,
            capture: kind.map_or(Capture::Plain, Capture::Kind),
        })
        .collect();
    let mut out = Vec::new();
    resolve(hits, |from, to, kind| out.push((from, to, kind)));
    out
}

/// The reference the sliced parses are checked against: the whole text
/// parsed in one call and one query over the whole tree, without ranges or
/// chunks (step 1's `whole_file`). No budget, no health registration.
#[cfg(test)]
pub(crate) fn one_shot(lang: &Lang, text: &[u8]) -> WindowSpans {
    let mut parser = Parser::new();
    parser.set_language(lang.language()).unwrap();
    let tree = parser.parse(text, None).expect("a tree");
    query_tree(lang, &tree, text)
}

/// One query over the whole of `tree`, a tree of `text`, without ranges or
/// chunks.
#[cfg(test)]
pub(crate) fn query_tree(lang: &Lang, tree: &Tree, text: &[u8]) -> WindowSpans {
    let compiled = lang.compiled().expect("the query compiles");
    let src: Vec<u8> = text.to_vec();
    let input = Input::of(&src, 0, src.len() as u64);
    let cancel = AtomicBool::new(false);
    let budget = Budget {
        budget: Duration::MAX,
        cancel: &cancel,
        yield_probe: None,
    };
    let watch = Watch::new(&budget);
    let hits = captures(
        lang.precedence(),
        compiled,
        tree,
        input,
        0..text.len(),
        &watch,
    )
    .unwrap();
    let mut builder = SpansBuilder::new(&text, 0);
    resolve(hits, |from, to, kind| builder.push(&text, from, to, kind));
    builder.finish(true)
}
