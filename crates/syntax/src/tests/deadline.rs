//! `deadline_*`: budgets, cancel and yield; a sliced parse resumed after
//! `TimedOut`, `Yielded` or `Cancelled` gives the tokens of one parse; each
//! step registers with `health` for its own budget and nothing is
//! registered between steps.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::highlight::{Budget, ChainWindow, FullParse, Outcome, Source, one_shot, window_in};
use crate::testing;
use crate::tests::health::ManualClocks;
use crate::window::JsonPad;
use crate::{Lang, WindowSpans};

use super::{ample, lang};

fn source(text: &[u8]) -> Arc<dyn Source> {
    Arc::new(text.to_vec())
}

type Line = (u64, Vec<crate::Token>);

fn lines(spans: &WindowSpans) -> Vec<Line> {
    spans.lines().map(|(s, t)| (s, t.to_vec())).collect()
}

fn budget<'a>(
    slice: Duration,
    cancel: &'a AtomicBool,
    probe: Option<&'a dyn Fn() -> bool>,
) -> Budget<'a> {
    Budget {
        budget: slice,
        cancel,
        yield_probe: probe,
    }
}

/// Steps a `FullParse` with `slice` until `Done`: the tokens and the
/// outcomes before it. The probe, when given, is on for every other step:
/// a yield request lasts a while, then the work goes on.
fn step_full(
    full: &mut FullParse,
    slice: Duration,
    probe: Option<&dyn Fn() -> bool>,
) -> (WindowSpans, Vec<&'static str>) {
    let cancel = AtomicBool::new(false);
    let mut before = Vec::new();
    for i in 0..50_000 {
        let probe = probe.filter(|_| i % 2 == 0);
        match full.step(budget(slice, &cancel, probe)) {
            Outcome::Done(spans) => return (spans, before),
            Outcome::TimedOut => before.push("timed out"),
            Outcome::Yielded => before.push("yielded"),
            other => panic!("{other:?}"),
        }
    }
    panic!("no progress");
}

fn step_chain(
    window: &mut ChainWindow,
    slice: Duration,
    probe: Option<&dyn Fn() -> bool>,
) -> ((WindowSpans, u64), usize) {
    let cancel = AtomicBool::new(false);
    let mut stops = 0;
    for i in 0..50_000 {
        let probe = probe.filter(|_| i % 2 == 0);
        match window.step(budget(slice, &cancel, probe)) {
            Outcome::Done(done) => return (done, stops),
            Outcome::TimedOut | Outcome::Yielded => stops += 1,
            other => panic!("{other:?}"),
        }
    }
    panic!("no progress");
}

#[test]
fn deadline_window_times_out() {
    let text = testing::ts(300 << 10, 0);
    let lang = lang("ts");
    let src = text.to_vec();
    let cancel = AtomicBool::new(false);
    let health = ManualClocks::new().health();
    let end = text.len() as u64;
    let outcome = window_in(
        &health,
        lang,
        &src,
        0..end,
        &JsonPad::default(),
        0..end,
        budget(Duration::ZERO, &cancel, None),
    );
    assert!(matches!(outcome, Outcome::TimedOut), "{outcome:?}");
    assert_eq!(health.running(), 0);
}

#[test]
fn deadline_window_cancelled_and_yielded() {
    let text = testing::ts(300 << 10, 0);
    let lang = lang("ts");
    let end = text.len() as u64;
    let health = ManualClocks::new().health();
    let run = |cancel: &AtomicBool, probe: Option<&dyn Fn() -> bool>| {
        window_in(
            &health,
            lang,
            &text,
            0..end,
            &JsonPad::default(),
            0..end,
            budget(Duration::from_secs(60), cancel, probe),
        )
    };
    assert!(matches!(
        run(&AtomicBool::new(true), None),
        Outcome::Cancelled
    ));
    let always = || true;
    assert!(matches!(
        run(&AtomicBool::new(false), Some(&always)),
        Outcome::Yielded
    ));
}

/// Sliced into many steps (a few hundred microseconds, and zero: every step
/// still makes progress), the tokens are those of one parse.
#[test]
fn deadline_full_parse_resumes_to_the_same_tokens() {
    for (name, text, slice) in [
        ("ts", testing::ts(200 << 10, 0), Duration::from_micros(300)),
        (
            "rust",
            testing::rust(200 << 10, 1),
            Duration::from_micros(300),
        ),
        (
            "json",
            testing::json(200 << 10, 2),
            Duration::from_micros(300),
        ),
        ("ts", testing::ts(24 << 10, 3), Duration::ZERO),
    ] {
        let lang = lang(name);
        let mut full = FullParse::new(lang, source(&text));
        let (spans, before) = step_full(&mut full, slice, None);
        assert!(before.len() > 2, "{name}: {} steps", before.len() + 1);
        assert!(before.iter().all(|o| *o == "timed out"));
        assert!(spans.exact());
        assert_eq!(spans.range(), 0..text.len() as u64);
        assert_eq!(lines(&spans), lines(&one_shot(lang, &text)), "{name}");
        // Done once.
        let cancel = AtomicBool::new(false);
        assert!(matches!(full.step(ample(&cancel)), Outcome::Unavailable));
    }
}

/// The yield probe stops a step anywhere (parse or query); the next step on
/// the same object goes on, and the result is the same.
#[test]
fn deadline_full_parse_yielded_resumes() {
    let text = testing::ts(200 << 10, 0);
    let lang = lang("ts");
    let calls = Cell::new(0u32);
    let probe = || {
        calls.set(calls.get() + 1);
        calls.get().is_multiple_of(40)
    };
    let mut full = FullParse::new(lang, source(&text));
    let (spans, before) = step_full(&mut full, Duration::from_millis(1), Some(&probe));
    assert!(before.contains(&"yielded"));
    assert!(before.contains(&"timed out"));
    assert_eq!(lines(&spans), lines(&one_shot(lang, &text)));
}

/// `Cancelled` stops the step; the object keeps its state.
#[test]
fn deadline_full_parse_cancelled() {
    let text = testing::rust(200 << 10, 0);
    let lang = lang("rust");
    let mut full = FullParse::new(lang, source(&text));
    let cancel = AtomicBool::new(false);
    let calls = Cell::new(0u32);
    let probe = || {
        calls.set(calls.get() + 1);
        if calls.get() == 30 {
            cancel.store(true, Ordering::Relaxed);
        }
        false
    };
    let outcome = full.step(budget(Duration::from_secs(60), &cancel, Some(&probe)));
    assert!(matches!(outcome, Outcome::Cancelled), "{outcome:?}");
    cancel.store(false, Ordering::Relaxed);
    let (spans, _) = step_full(&mut full, Duration::from_secs(60), None);
    assert_eq!(lines(&spans), lines(&one_shot(lang, &text)));
}

/// A chain window sliced and in one step: the same tokens and the same
/// next start.
#[test]
fn deadline_chain_window_resumes_to_the_same_window() {
    for (name, text) in [
        ("ts", testing::ts(400 << 10, 0)),
        ("json", testing::json(400 << 10, 0)),
        ("js", testing::minified_js(400 << 10, 0)),
    ] {
        let lang = lang(name);
        let src = source(&text);
        let start = 100_000;
        let ((whole, next), _) = step_chain(
            &mut ChainWindow::new(lang, src.clone(), start),
            Duration::from_secs(60),
            None,
        );
        let mut sliced = ChainWindow::new(lang, src.clone(), start);
        let ((spans, sliced_next), stops) =
            step_chain(&mut sliced, Duration::from_micros(200), None);
        assert!(stops > 2, "{name}: {stops}");
        assert_eq!(sliced_next, next);
        assert_eq!(spans.range(), whole.range());
        assert_eq!(lines(&spans), lines(&whole), "{name}");
        assert!(sliced.spent() > Duration::ZERO);
        let calls = Cell::new(0u32);
        let probe = || {
            calls.set(calls.get() + 1);
            calls.get().is_multiple_of(25)
        };
        let mut yielded = ChainWindow::new(lang, src, start);
        let ((spans, yielded_next), _) =
            step_chain(&mut yielded, Duration::from_millis(1), Some(&probe));
        assert_eq!((lines(&spans), yielded_next), (lines(&whole), next));
    }
}

/// Each step registers for the budget it was given (the remainder when it
/// resumes), and only while it runs: between steps nothing is in the table,
/// however much CPU or wall time passes, so a sliced parse is never taken
/// for a stuck one.
#[test]
fn deadline_health_registered_per_step() {
    let clocks = ManualClocks::new();
    let health = clocks.health();
    let thread = clocks.new_thread();
    clocks.run_on(&thread);
    let text = testing::ts(200 << 10, 0);
    let lang = lang("ts");
    let seen: RefCell<Vec<Vec<Duration>>> = RefCell::new(Vec::new());
    let probe = || {
        seen.borrow_mut().push(health.running_budgets());
        false
    };
    let cancel = AtomicBool::new(false);
    let mut full = FullParse::new_in(health.handle(), lang, source(&text));
    let mut remaining = Duration::from_millis(3);
    let mut steps = 0;
    let mut observed = 0;
    loop {
        seen.borrow_mut().clear();
        let outcome = full.step(budget(remaining, &cancel, Some(&probe)));
        steps += 1;
        // Registered once, for this step's budget, while it ran (the probe
        // runs from tree-sitter's progress callbacks and between chunks).
        let seen = seen.borrow();
        observed += usize::from(!seen.is_empty());
        assert!(seen.iter().all(|b| *b == [remaining]), "{seen:?}");
        assert_eq!(health.running(), 0, "unregistered on return");
        // Between steps: far past any budget and grace, on this thread and
        // on the wall clock.
        thread.add_cpu(Duration::from_secs(30));
        clocks.advance_wall(Duration::from_secs(60));
        assert!(health.allows("ts", lang.grammar_version()));
        match outcome {
            Outcome::Done(spans) => {
                assert_eq!(lines(&spans), lines(&one_shot(lang, &text)));
                break;
            }
            Outcome::TimedOut => {}
            other => panic!("{other:?}"),
        }
        remaining = remaining
            .saturating_sub(Duration::from_micros(500))
            .max(Duration::from_micros(400));
    }
    assert!(steps > 2);
    assert!(observed > 2, "{observed} of {steps} steps");
    // The same for a chain window.
    let mut window = ChainWindow::new_in(health.handle(), lang, source(&text), 50_000);
    let outcome = window.step(budget(Duration::from_micros(700), &cancel, Some(&probe)));
    assert!(matches!(outcome, Outcome::TimedOut | Outcome::Done(_)));
    assert_eq!(health.running(), 0);
}

/// A stuck call of the same grammar version on another thread: the next
/// step registers, finds it and returns `Unavailable` without entering
/// tree-sitter; other languages go on.
#[test]
fn deadline_disabled_grammar_is_unavailable() {
    let clocks = ManualClocks::new();
    let health = clocks.health();
    let ts = lang("ts");
    let text = testing::ts(200 << 10, 0);
    let cancel = AtomicBool::new(false);
    let mut full = FullParse::new_in(health.handle(), ts, source(&text));
    let mut window = ChainWindow::new_in(health.handle(), ts, source(&text), 0);
    // One slice of each before the stuck call.
    assert!(matches!(
        full.step(budget(Duration::from_micros(200), &cancel, None)),
        Outcome::TimedOut
    ));
    let stuck = clocks.new_thread();
    clocks.run_on(&stuck);
    let guard = health
        .enter("ts", ts.grammar_version(), Duration::from_millis(8))
        .unwrap();
    stuck.add_cpu(Duration::from_secs(2));
    clocks.run_on(&clocks.new_thread());
    let entered = Cell::new(false);
    let probe = || {
        entered.set(true);
        false
    };
    assert!(matches!(
        full.step(budget(Duration::from_secs(60), &cancel, Some(&probe))),
        Outcome::Unavailable
    ));
    assert!(matches!(
        window.step(budget(Duration::from_secs(60), &cancel, Some(&probe))),
        Outcome::Unavailable
    ));
    assert!(matches!(
        window_in(
            &health,
            ts,
            &text,
            0..1000,
            &JsonPad::default(),
            0..1000,
            budget(Duration::from_secs(60), &cancel, Some(&probe))
        ),
        Outcome::Unavailable
    ));
    assert!(!entered.get(), "never entered tree-sitter");
    assert!(!health.allows("ts", ts.grammar_version()));
    // Another grammar is not affected.
    let json = lang("json");
    let text = testing::json(10 << 10, 0);
    let mut other = FullParse::new_in(health.handle(), json, source(&text));
    assert!(matches!(other.step(ample(&cancel)), Outcome::Done(_)));
    drop(guard);
}

/// A step that parses and queries in one go is one registration, not one
/// per phase.
#[test]
fn deadline_one_registration_per_step() {
    let clocks = ManualClocks::new();
    let health = clocks.health();
    let lang: &Lang = lang("json");
    let text = testing::json(100 << 10, 0);
    let max = Cell::new(0);
    let probe = || {
        max.set(max.get().max(health.running()));
        false
    };
    let cancel = AtomicBool::new(false);
    let mut full = FullParse::new_in(health.handle(), lang, source(&text));
    assert!(matches!(
        full.step(budget(Duration::from_secs(60), &cancel, Some(&probe))),
        Outcome::Done(_)
    ));
    assert_eq!(max.get(), 1);
    assert_eq!(health.running(), 0);
}
