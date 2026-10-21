//! Parsing and highlighting (docs/design/file_reader.md「解析与高亮」).
//! Blocking; executor threads only, never the UI thread.
//!
//! Step 1 of the design's 「验收」 has the whole-file highlight in one call,
//! [`whole_file`]. The first screen's window, the sliced `FullParse` and
//! the window chain build on the same capture resolution in later steps.
//!
//! How captures become tokens: a node captured by several patterns takes
//! the first or the last of them, as the language's query header says; nested
//! captures colour their own range over the capture around them (the
//! innermost wins), and `@none` / `@embedded` leave theirs uncoloured. Ranges
//! are then split at line ends.

use std::cell::Cell;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tree_sitter::{ParseOptions, Parser, QueryCursor, QueryCursorOptions, StreamingIterator, Tree};

use crate::health::{self, Health};
use crate::lang::{Capture, Compiled, Lang, Precedence};
use crate::spans::{SpansBuilder, TokenKind, WindowSpans};

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
/// probe asked to stop. `Unavailable`: the grammar version is disabled by
/// [`health`], or the language's query does not compile; the caller shows
/// plain text.
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

/// Parses all of `text` and highlights all of it, in one call: the tokens of
/// every line, [`WindowSpans::exact`]. Not resumable: after `TimedOut`,
/// `Yielded` or `Cancelled` nothing is kept. Compiling the language's query
/// on first use ([`Lang::prepare`]) is not counted in the budget. Blocking;
/// background threads only.
pub fn whole_file(lang: &Lang, text: &[u8], budget: Budget<'_>) -> Outcome<WindowSpans> {
    whole_file_in(health::global(), lang, text, budget)
}

pub(crate) fn whole_file_in(
    health: &Health,
    lang: &Lang,
    text: &[u8],
    budget: Budget<'_>,
) -> Outcome<WindowSpans> {
    let Some(compiled) = lang.compiled() else {
        return Outcome::Unavailable;
    };
    let watch = Watch::new(&budget);
    let Some(_guard) = health.enter(lang.name(), lang.grammar_version(), budget.budget) else {
        return Outcome::Unavailable;
    };
    let Some(tree) = parse(lang, text, &watch) else {
        return watch.outcome();
    };
    let mut builder = SpansBuilder::new(text, 0);
    if !highlight_tree(lang, compiled, &tree, text, &watch, &mut builder) {
        return watch.outcome();
    }
    Outcome::Done(builder.finish(true))
}

/// Why a call stopped early.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stop {
    TimedOut,
    Cancelled,
    Yielded,
}

/// The budget's checks, run from tree-sitter's progress callbacks.
struct Watch<'b, 'a> {
    budget: &'b Budget<'a>,
    started: Instant,
    stopped: Cell<Option<Stop>>,
}

impl<'b, 'a> Watch<'b, 'a> {
    fn new(budget: &'b Budget<'a>) -> Watch<'b, 'a> {
        Watch {
            budget,
            started: Instant::now(),
            stopped: Cell::new(None),
        }
    }

    fn check(&self) -> ControlFlow<()> {
        let stop = if self.budget.cancel.load(Ordering::Relaxed) {
            Stop::Cancelled
        } else if self.budget.yield_probe.is_some_and(|probe| probe()) {
            Stop::Yielded
        } else if self.started.elapsed() > self.budget.budget {
            Stop::TimedOut
        } else {
            return ControlFlow::Continue(());
        };
        self.stopped.set(Some(stop));
        ControlFlow::Break(())
    }

    fn outcome<T>(&self) -> Outcome<T> {
        match self.stopped.get() {
            Some(Stop::TimedOut) => Outcome::TimedOut,
            Some(Stop::Cancelled) => Outcome::Cancelled,
            Some(Stop::Yielded) => Outcome::Yielded,
            // tree-sitter returned no tree without being stopped: the
            // grammar did not load.
            None => Outcome::Unavailable,
        }
    }
}

fn parse(lang: &Lang, text: &[u8], watch: &Watch<'_, '_>) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(lang.language()).ok()?;
    let mut progress = |_: &tree_sitter::ParseState| watch.check();
    let len = text.len();
    let tree = parser.parse_with_options(
        &mut |at, _| if at < len { &text[at..] } else { &[] },
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    if watch.stopped.get().is_some() {
        return None;
    }
    tree
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

/// Runs the highlight query over `tree` and pushes the resolved segments
/// into `builder`. False when the watch stopped it.
fn highlight_tree(
    lang: &Lang,
    compiled: &Compiled,
    tree: &Tree,
    text: &[u8],
    watch: &Watch<'_, '_>,
    builder: &mut SpansBuilder<'_>,
) -> bool {
    let mut cursor = QueryCursor::new();
    let mut progress = |_: &tree_sitter::QueryCursorState| watch.check();
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
        let priority = match lang.precedence() {
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
        return false;
    }
    resolve(hits, |from, to, kind| builder.push(from, to, kind));
    true
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
